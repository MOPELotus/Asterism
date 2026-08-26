from __future__ import annotations

import importlib.util
import pathlib
import types
import unittest

WORKER_PATH = pathlib.Path(__file__).resolve().parents[1] / "worker.py"
SPEC = importlib.util.spec_from_file_location("asterism_welearn_worker", WORKER_PATH)
assert SPEC and SPEC.loader
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


class Cookies:
    def __init__(self):
        self.values = {}

    def update(self, values):
        self.values.update(values)

    def get_dict(self):
        return dict(self.values)


class Events:
    def __init__(self):
        self.values = []

    def emit(self, event_type, **payload):
        self.values.append((event_type, payload))


class Response:
    def __init__(self, value, *, text="", status_code=200):
        self.value = value
        self.text = text
        self.status_code = status_code

    def json(self):
        return self.value


def module_fixture():
    module = types.SimpleNamespace()
    module.session = types.SimpleNamespace(
        cookies=Cookies(),
        get=lambda *_args, **_kwargs: types.SimpleNamespace(
            json=lambda: {"info": [{"id": "sco-1", "iscomplete": "true"}]}
        ),
    )

    def startstudy(correctness, item):
        module.way1Succeed.append((correctness, item["id"]))

    def startstudy_time(index, statuses, seconds, item):
        statuses[index]["elapsed"] = seconds
        statuses[index]["status"] = "已完成"

    module.startstudy = startstudy
    module.startstudy_time = startstudy_time
    return module


def payload(action, **settings):
    return {
        "session": {"cookies": {"sid": "secret"}},
        "task": {
            "native": {
                "uid": "1",
                "cid": "2",
                "classid": "3",
                "unit_index": 0,
                "item": {"id": "sco-1"},
            }
        },
        "settings": {"action": action, **settings},
    }


class RunTaskTests(unittest.TestCase):
    def test_duration_dispatch_reuses_donor_duration_path(self):
        module = module_fixture()
        original_load = WORKER.load
        WORKER.load = lambda *_args: module
        try:
            result = WORKER.dispatch(
                "duration",
                payload("complete", duration_seconds=12),
                pathlib.Path(__file__).resolve(),
                types.SimpleNamespace(),
                Events(),
                WORKER.Redactor(),
            )
        finally:
            WORKER.load = original_load
        self.assertEqual(result["result"]["action"], "duration")

    def test_course_probe_bootstraps_student_session_after_transient_null(self):
        module = module_fixture()
        responses = iter([Response(None), Response({}), Response({"clist": [{"cid": 1}]})])
        calls = []

        def get(url, **_kwargs):
            calls.append(url)
            if url.endswith("/student/index.aspx"):
                return Response({})
            return next(responses)

        module.session.get = get
        rows = WORKER.course_rows(module, Events(), WORKER.Redactor())

        self.assertEqual(rows, [{"cid": 1}])
        self.assertEqual(sum(url.endswith("/student/index.aspx") for url in calls), 2)

    def test_course_probe_reports_stable_null_without_worker_internal_crash(self):
        module = module_fixture()
        module.session.get = lambda url, **_kwargs: (
            Response({}) if url.endswith("/student/index.aspx") else Response(None)
        )

        with self.assertRaises(WORKER.WorkerFailure) as caught:
            WORKER.course_rows(module, Events(), WORKER.Redactor())

        self.assertEqual(caught.exception.code, "course_probe_unavailable")

    def test_completion_reuses_startstudy(self):
        module = module_fixture()
        result = WORKER.run_task(
            module, payload("complete", correctness=97), Events(), WORKER.Redactor()
        )

        self.assertEqual(module.way1Succeed, [(97, "sco-1")])
        self.assertEqual(result["remote_state"], "completed")

    def test_duration_reuses_startstudy_time(self):
        module = module_fixture()
        result = WORKER.run_task(
            module, payload("duration", duration_seconds=12), Events(), WORKER.Redactor()
        )

        self.assertEqual(result["result"]["action"], "duration")

    def test_completion_is_not_replayed_when_fresh_status_is_unavailable(self):
        module = module_fixture()
        module.session.get = lambda *_args, **_kwargs: types.SimpleNamespace(
            json=lambda: {"info": []}
        )

        result = WORKER.run_task(module, payload("complete"), Events(), WORKER.Redactor())

        self.assertEqual(module.way1Succeed, [(100, "sco-1")])
        self.assertEqual(result["remote_state"], "in_progress")
        self.assertFalse(result["verified"])
        self.assertEqual(result["result"]["verification_error"], "fresh_status_unavailable")

    def test_invalid_duration_fails_before_upstream(self):
        with self.assertRaises(WORKER.WorkerFailure):
            WORKER.run_task(
                module_fixture(),
                payload("duration", duration_seconds=0),
                Events(),
                WORKER.Redactor(),
            )

    def test_zero_progress_course_keeps_upstream_execution_available(self):
        module = module_fixture()
        request = payload("complete")
        request["task"]["native"]["read_only"] = True

        result = WORKER.run_task(module, request, Events(), WORKER.Redactor())

        self.assertEqual(result["remote_state"], "completed")
        self.assertEqual(module.way1Succeed, [(100, "sco-1")])


if __name__ == "__main__":
    unittest.main()
