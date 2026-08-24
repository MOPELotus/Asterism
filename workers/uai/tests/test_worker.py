import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


WORKER = pathlib.Path(__file__).parents[1] / "worker.py"
FAKE = pathlib.Path(__file__).parent / "fixtures" / "fake_upstream.py"


class WorkerTests(unittest.TestCase):
    def invoke(self, operation, payload=None):
        with tempfile.TemporaryDirectory() as directory:
            metadata = pathlib.Path(directory) / "SOURCE.json"
            metadata.write_text(
                json.dumps(
                    {
                        "name": "fixture/fake",
                        "repository": "https://example.invalid/fake",
                        "revision": "fixture-revision",
                        "entrypoint": FAKE.name,
                        "entrypoint_sha256": hashlib.sha256(FAKE.read_bytes()).hexdigest(),
                        "license": "Apache-2.0",
                        "adapter_protocol": "asterism.uai.worker.v1",
                    }
                ),
                encoding="utf-8",
            )
            request = {
                "request_id": "request-1",
                "operation": operation,
                "payload": payload or {},
            }
            completed = subprocess.run(
                [
                    sys.executable,
                    str(WORKER),
                    "--upstream",
                    str(FAKE),
                    "--source-metadata",
                    str(metadata),
                ],
                input=json.dumps(request) + "\n",
                text=True,
                capture_output=True,
                check=False,
            )
        events = [json.loads(line) for line in completed.stdout.splitlines()]
        return completed, events

    def test_health_reports_pinned_source_and_operations(self):
        completed, events = self.invoke("health")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = events[-1]
        self.assertEqual(result["type"], "result")
        self.assertEqual(result["data"]["status"], "ok")
        self.assertEqual(result["data"]["source"]["revision"], "fixture-revision")
        self.assertIn("inspect", result["data"]["operations"])

    def test_authentication_returns_session_without_logging_credentials(self):
        completed, events = self.invoke(
            "authenticate",
            {"credentials": {"username": "user-secret", "password": "password-secret"}},
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        serialized = completed.stdout
        logs = [event for event in events if event["type"] == "log"]
        self.assertTrue(logs)
        self.assertNotIn("user-secret", json.dumps(logs))
        self.assertNotIn("password-secret", json.dumps(logs))
        self.assertIn("[REDACTED]", serialized)
        self.assertEqual(events[-1]["data"]["session"]["open_id"], "open-1")

    def test_unexpected_traceback_is_redacted(self):
        completed, events = self.invoke(
            "authenticate",
            {"credentials": {"username": "raise-secret", "password": "password-secret"}},
        )
        self.assertEqual(completed.returncode, 3)
        self.assertEqual(events[-1]["code"], "worker_internal")
        self.assertNotIn("raise-secret", completed.stdout)
        self.assertNotIn("raise-secret", completed.stderr)
        self.assertNotIn("password-secret", completed.stdout)
        self.assertNotIn("password-secret", completed.stderr)

    def test_courses_and_tasks_reuse_donor_session_and_inventory(self):
        session = {
            "authorization": "jwt-secret",
            "cookies": [],
            "open_id": "open-1",
            "user_id": "user-1",
            "sso_id": "sso-1",
        }
        completed, events = self.invoke("courses", {"session": session})
        self.assertEqual(completed.returncode, 0, completed.stderr)
        course = events[-1]["data"]["courses"][0]
        self.assertEqual(course["remote_id"], "9")

        completed, events = self.invoke(
            "tasks", {"session": session, "course": course["native"]}
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        task = events[-1]["data"]["tasks"][0]
        self.assertEqual(task["remote_id"], "task-1")
        self.assertEqual(task["state"], "pending")
        self.assertEqual(task["source_type"], "resource")
        self.assertIn("duration_read", task["capabilities"])
        self.assertEqual(task["native"]["category"], "objective")

    def test_integrity_mismatch_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            metadata = pathlib.Path(directory) / "SOURCE.json"
            metadata.write_text(
                json.dumps(
                    {
                        "name": "fixture/fake",
                        "repository": "https://example.invalid/fake",
                        "revision": "fixture-revision",
                        "entrypoint": FAKE.name,
                        "entrypoint_sha256": "0" * 64,
                        "license": "Apache-2.0",
                        "adapter_protocol": "asterism.uai.worker.v1",
                    }
                ),
                encoding="utf-8",
            )
            completed = subprocess.run(
                [sys.executable, str(WORKER), "--upstream", str(FAKE), "--source-metadata", str(metadata)],
                input=json.dumps({"request_id": "request-1", "operation": "health", "payload": {}}) + "\n",
                text=True,
                capture_output=True,
                check=False,
            )
        event = json.loads(completed.stdout)
        self.assertEqual(completed.returncode, 2)
        self.assertEqual(event["code"], "upstream_integrity_mismatch")

    def test_questions_normalize_known_kind_and_keep_native_answer_evidence(self):
        session = {"authorization": "jwt-secret", "cookies": [], "open_id": "open-1", "user_id": "user-1", "sso_id": "sso-1"}
        task = {"remote_id": "task-1"}
        course = {"resource_id": 9, "class_id": "class-1", "curricula_id": "7"}
        completed, events = self.invoke("questions", {"session": session, "course": course, "task": task})
        self.assertEqual(completed.returncode, 0, completed.stderr)
        question = events[-1]["data"]["questions"][0]
        self.assertEqual(question["kind"], "single_choice")
        self.assertEqual(question["answer_evidence"]["answer"], "answer-native")

    def test_run_uses_donor_process_task_and_fresh_completion(self):
        session = {"authorization": "jwt-secret", "cookies": [], "open_id": "open-1", "user_id": "user-1", "sso_id": "sso-1"}
        task = {"remote_id": "task-1"}
        course = {"resource_id": 9, "class_id": "class-1", "curricula_id": "7"}
        completed, events = self.invoke("run", {"session": session, "course": course, "task": task})
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = events[-1]["data"]
        self.assertTrue(result["verified"])
        self.assertEqual(result["remote_state"], "completed")

    def test_duration_reads_exact_task_seconds(self):
        session = {"authorization": "jwt-secret", "cookies": [], "open_id": "open-1", "user_id": "user-1", "sso_id": "sso-1"}
        task = {
            "remote_id": "task-1",
            "native": {
                "unit_id": "unit-1",
                "course": {"resource_id": 9, "class_id": "class-1", "curricula_id": "7"},
            },
        }
        completed, events = self.invoke("duration", {"session": session, "task": task})
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(events[-1]["data"]["duration_seconds"], 321)
        self.assertEqual(events[-1]["data"]["native_record"]["finishProgress"], 50)


if __name__ == "__main__":
    unittest.main()
