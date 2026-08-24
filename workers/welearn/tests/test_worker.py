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
        "task": {"native": {"uid": "1", "cid": "2", "classid": "3",
                            "unit_index": 0, "item": {"id": "sco-1"}}},
        "settings": {"action": action, **settings},
    }


class RunTaskTests(unittest.TestCase):
    def test_completion_reuses_startstudy(self):
        module = module_fixture()
        result = WORKER.run_task(module, payload("complete", correctness=97), Events(), WORKER.Redactor())

        self.assertEqual(module.way1Succeed, [(97, "sco-1")])
        self.assertEqual(result["remote_state"], "completed")

    def test_duration_reuses_startstudy_time(self):
        module = module_fixture()
        result = WORKER.run_task(module, payload("duration", duration_seconds=12), Events(), WORKER.Redactor())

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
            WORKER.run_task(module_fixture(), payload("duration", duration_seconds=0), Events(), WORKER.Redactor())

    def test_read_only_course_refuses_execution_before_upstream_mutation(self):
        module = module_fixture()
        request = payload("complete")
        request["task"]["native"]["read_only"] = True

        with self.assertRaises(WORKER.WorkerFailure) as caught:
            WORKER.run_task(module, request, Events(), WORKER.Redactor())

        self.assertEqual(caught.exception.code, "task_read_only")
        self.assertFalse(hasattr(module, "way1Succeed"))


if __name__ == "__main__":
    unittest.main()
