from __future__ import annotations

import importlib.util
import pathlib
import types
import unittest


WORKER_PATH = pathlib.Path(__file__).resolve().parents[1] / "worker.py"
SPEC = importlib.util.spec_from_file_location("asterism_cidaren_worker", WORKER_PATH)
assert SPEC and SPEC.loader
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


class Events:
    def __init__(self):
        self.values = []

    def emit(self, event_type, **payload):
        self.values.append((event_type, payload))


def session_modules(*, runner=None):
    request_header = types.SimpleNamespace(set_token=lambda _token: None)
    decoder = types.SimpleNamespace(clear_crypto_document=lambda: None)
    return [None, request_header, None, None, decoder, None, runner]


class InventoryTests(unittest.TestCase):
    def test_inventory_reads_donor_task_list_and_records_shapes(self):
        modules = session_modules()

        def get_all_unit(public):
            public.all_unit = {"task_list": [{
                "list_id": "unit-1", "task_id": 11, "task_name": "Unit 1", "progress": 100,
            }]}

        def get_class_task(public, _page):
            public.class_task.append({"records": [{
                "task_id": 22, "release_id": 33, "task_name": "Class test",
                "task_type": 2, "progress": 40,
            }]})
            public.task_total_count = 1

        modules[2] = types.SimpleNamespace(get_all_unit=get_all_unit)
        modules[3] = types.SimpleNamespace(get_class_task=get_class_task)
        result = WORKER.tasks(
            modules,
            {"session": {"token": "secret"}, "course": {"course_id": "course-1"}},
            Events(),
            WORKER.Redactor(["secret"]),
        )

        self.assertEqual(
            [row["remote_id"] for row in result["tasks"]],
            ["study-task:course-1:unit-1", "class-task:33"],
        )
        self.assertTrue(all(row["global_remote_id"] for row in result["tasks"]))
        self.assertEqual([row["source_type"] for row in result["tasks"]], ["practice", "exam"])
        self.assertEqual([row["assessment_class"] for row in result["tasks"]], ["routine", "formal"])
        self.assertEqual(result["tasks"][0]["state"], "completed")
        self.assertEqual(result["tasks"][1]["progress_percent"], 40)
        self.assertNotIn("questions", result["tasks"][0]["capabilities"])
        self.assertIn("questions", result["tasks"][1]["capabilities"])
        self.assertIn("run", result["tasks"][1]["capabilities"])
        self.assertEqual(result["tasks"][1]["native"]["task"]["course_id"], "course-1")

    def test_course_title_finds_nested_current_course(self):
        document = {"user_info": {"course_id": "JJ_2"},
                    "current_course": {"course_id": "JJ_2", "course_name": "Book 2"}}

        self.assertEqual(WORKER.course_title(document, "JJ_2"), "Book 2")

    def test_completed_class_task_does_not_offer_unreadable_questions(self):
        self.assertEqual(WORKER.class_task_capabilities(100, False), ["run"])
        self.assertEqual(WORKER.class_task_capabilities(0, True), ["run"])
        self.assertEqual(WORKER.class_task_capabilities(40, False), ["questions", "run"])


class ExecutionTests(unittest.TestCase):
    def test_run_uses_loopback_answer_bridge_for_unsupplied_question(self):
        seen = []

        class Runner:
            def __init__(self, root, *, progress, log):
                self.public = types.SimpleNamespace(course_id=None, right_count=0, wrong_count=0)

            def set_answer_override(self, callback):
                self.override = callback

            def run_class_task(self, task):
                exam = {"topic_code": "topic-bridge", "topic_mode": 13,
                        "stem": {"content": "Choose"},
                        "options": [{"answer_tag": 0, "content": "Alpha"}, {"answer_tag": 1, "content": "Beta"}]}
                self.public.exam = exam
                seen.append(self.override(self.public, 13))
                return {"complete": True}

        runner_module = types.SimpleNamespace(HeadlessTaskRunner=Runner)
        runner_module.submit = lambda public, option: None
        original = WORKER._bridge_post
        try:
            WORKER._bridge_post = lambda bridge, document, timeout: ({
                "answer_available": True, "value": "B"
            } if document.get("kind") == "resolve_answer" else {"ok": True})
            result = WORKER.execute_task(
                session_modules(runner=runner_module),
                {"session": {"token": "secret"},
                 "settings": {"answer_bridge_url": "http://127.0.0.1:19001/answer",
                               "answer_bridge_ticket": "bridge-secret", "execution_id": "e1",
                               "task_id": "t1", "remote_task_id": "r1"},
                 "task": {"native": {"task_family": "class", "course_id": "course-1",
                                       "task": {"task_id": 22, "task_type": 2}}}},
                pathlib.Path("C:/repo/api/login.py"), Events(), WORKER.Redactor(["secret", "bridge-secret"]),
            )
        finally:
            WORKER._bridge_post = original
        self.assertTrue(result["verified"])
        self.assertEqual(seen, [1])

    def test_answer_bridge_requires_loopback_and_bindings(self):
        with self.assertRaises(WORKER.WorkerFailure):
            WORKER._answer_bridge_settings({"answer_bridge_url": "https://example.test/a", "answer_bridge_ticket": "x",
                                             "execution_id": "e", "task_id": "t"})
        with self.assertRaises(WORKER.WorkerFailure):
            WORKER._answer_bridge_settings({"answer_bridge_url": "http://127.0.0.1/a", "answer_bridge_ticket": "x"})

    def test_run_delegates_class_task_to_headless_donor_runner(self):
        seen = {}

        class Runner:
            def __init__(self, root, *, progress, log):
                seen["root"] = pathlib.Path(root)
                seen["progress"] = progress
                seen["log"] = log
                self.public = types.SimpleNamespace(course_id=None)

            def run_class_task(self, task):
                seen["task"] = task
                seen["progress"](2, 5, "答题中")
                return {"complete": True, "score": 100}

        modules = session_modules(runner=types.SimpleNamespace(HeadlessTaskRunner=Runner))
        events = Events()
        result = WORKER.execute_task(
            modules,
            {
                "session": {"token": "secret"},
                "task": {"native": {"task_family": "class", "course_id": "course-1",
                                      "task": {"task_id": 22, "task_type": 2}}},
            },
            pathlib.Path("C:/repo/api/login.py"),
            events,
            WORKER.Redactor(["secret"]),
        )

        self.assertTrue(result["verified"])
        self.assertEqual(result["remote_state"], "completed")
        self.assertEqual(seen["task"]["task_id"], 22)
        self.assertIn(("progress", {"current": 2, "total": 5, "message": "答题中"}), events.values)

    def test_run_forwards_normalized_answers_to_donor_override_boundary(self):
        seen = {}

        class Runner:
            def __init__(self, root, *, progress, log):
                self.public = types.SimpleNamespace(course_id=None)

            def set_answer_override(self, callback):
                seen["override"] = callback

            def run_class_task(self, task):
                exam = {"topic_code": "topic-1", "options": [{"answer_tag": "x", "content": "Alpha"}]}
                fake_public = types.SimpleNamespace(exam=exam)
                seen["value"] = seen["override"](fake_public, 1)
                return {"complete": True}

        modules = session_modules(runner=types.SimpleNamespace(HeadlessTaskRunner=Runner))
        result = WORKER.execute_task(
            modules,
            {
                "session": {"token": "secret"},
                "answers": [{"remote_id": "topic-1", "value": "Alpha"}],
                "task": {"native": {"task_family": "class", "course_id": "course-1",
                                      "task": {"task_id": 22, "task_type": 2}}},
            },
            pathlib.Path("C:/repo/api/login.py"),
            Events(),
            WORKER.Redactor(["secret"]),
        )

        self.assertTrue(result["verified"])
        self.assertEqual(seen["value"], "x")

    def test_run_freezes_timed_answer_policy_without_implementing_model_protocol(self):
        class Runner:
            def __init__(self, root, *, progress, log):
                self.public = types.SimpleNamespace(course_id=None, spend_min_time=1, spend_max_time=2)

            def run_class_task(self, task):
                return {"complete": True}

        modules = session_modules(runner=types.SimpleNamespace(HeadlessTaskRunner=Runner))
        result = WORKER.execute_task(
            modules,
            {
                "session": {"token": "secret"},
                "settings": {"answer_route": "timed", "instant_timeout_seconds": 5, "instant_fallback_grace_seconds": 1},
                "task": {"native": {"task_family": "class", "course_id": "course-1",
                                      "task": {"task_id": 22, "task_type": 2}}},
            },
            pathlib.Path("C:/repo/api/login.py"), Events(), WORKER.Redactor(["secret"]),
        )
        self.assertEqual(result["answer_policy"], {
            "route": "timed", "instant_timeout_seconds": 5, "instant_fallback_grace_seconds": 1,
            "model_budget_seconds": 3, "fallback_decision_seconds": 5,
            "submission_reserve_seconds": 1,
        })

    def test_timed_bridge_retries_with_escalation_after_instant_timeout(self):
        calls = []

        class Runner:
            def __init__(self, root, *, progress, log):
                self.public = types.SimpleNamespace(course_id=None, right_count=0, wrong_count=0)

            def set_answer_override(self, callback):
                self.override = callback

            def run_class_task(self, task):
                self.public.exam = {"topic_code": "timed", "topic_mode": 13,
                                    "options": [{"answer_tag": 0, "content": "A"},
                                                 {"answer_tag": 1, "content": "B"}]}
                self.override(self.public, 13)
                return {"complete": True}

        runner_module = types.SimpleNamespace(HeadlessTaskRunner=Runner)
        original = WORKER._bridge_post
        def bridge(_bridge, request, _timeout):
            calls.append(request["route"])
            if len(calls) == 1:
                raise TimeoutError("instant")
            return {"answer_available": True, "donor_value": "B"}
        try:
            WORKER._bridge_post = bridge
            result = WORKER.execute_task(
                session_modules(runner=runner_module),
                {"session": {"token": "secret"},
                 "settings": {"answer_route": "timed", "instant_timeout_seconds": 1,
                               "instant_fallback_grace_seconds": 2,
                               "answer_bridge_url": "http://127.0.0.1:19001/answer",
                               "answer_bridge_ticket": "bridge-secret", "execution_id": "e1",
                               "task_id": "t1", "remote_task_id": "r1"},
                 "task": {"native": {"task_family": "class", "course_id": "course-1",
                                       "task": {"task_id": 22, "task_type": 2}}}},
                pathlib.Path("C:/repo/api/login.py"), Events(), WORKER.Redactor(["secret", "bridge-secret"]),
            )
        finally:
            WORKER._bridge_post = original
        self.assertTrue(result["verified"])
        self.assertEqual(calls, ["timed", "escalation"])


if __name__ == "__main__":
    unittest.main()
