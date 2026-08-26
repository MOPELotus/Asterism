from __future__ import annotations

import sys
import tempfile
import threading
import unittest
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

from asterism.answers import AnswerRepository, canonical_question, question_identity, rebind_answer
from asterism.config import LocalConfigStore
from asterism.database import QuestionBank
from asterism.drafts import DraftRepository
from asterism.inventory import InventoryStore
from asterism.paths import DataPaths
from asterism.profiles import ProfileStateStore, ProfileStore
from asterism.providers import ProviderRegistry, WorkerSpec
from asterism.runner import RunnerError, RunnerManager
from asterism.scan import ReadOnlyScanCoordinator


class LocalStoreTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.paths = DataPaths.resolve(self.temporary.name)
        self.paths.initialize()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_profile_round_trip_keeps_plaintext_only_in_profile_file(self) -> None:
        store = ProfileStore(self.paths)
        created = store.create("chaoxing", "本地账号")
        updated = replace(created, credentials={"username": "alice", "password": "plain-local"})
        store.save(updated)
        loaded = store.get("chaoxing", created.id)
        self.assertEqual(loaded.credentials["password"], "plain-local")
        self.assertEqual(store.list("chaoxing"), [loaded])
        profile_text = store.path_for("chaoxing", created.id).read_text(encoding="utf-8")
        self.assertIn("plain-local", profile_text)
        self.assertFalse(any(self.paths.logs.rglob("*")))

    def test_session_state_is_separate_from_profile(self) -> None:
        profiles = ProfileStore(self.paths)
        states = ProfileStateStore(self.paths)
        profile = profiles.create("uai", "uai-1")
        states.save(profile, "session", {"jwt": "session-value"})
        self.assertEqual(states.load(profile, "session"), {"jwt": "session-value"})
        profile_text = profiles.path_for("uai", profile.id).read_text(encoding="utf-8")
        self.assertNotIn("session-value", profile_text)

    def test_question_bank_schema_contains_no_service_tables(self) -> None:
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        expected = {
            "schema_version",
            "questions",
            "answer_candidates",
            "answer_observations",
            "ai_cache",
            "formal_drafts",
        }
        self.assertEqual(bank.table_names(), expected)
        question_id = bank.upsert_question(
            "cidaren", "identity", "single_choice", {"prompt": "完整题目", "options": []}
        )
        self.assertGreater(question_id, 0)

    def test_local_config_is_created_without_credentials(self) -> None:
        store = LocalConfigStore(self.paths.config)
        value = store.ensure()
        self.assertEqual(value["ui"]["language"], "zh-CN")
        self.assertFalse(value["notifications"]["enabled"])
        self.assertEqual(store.load(), value)

    def test_formal_draft_file_and_sqlite_index_stay_in_sync(self) -> None:
        profiles = ProfileStore(self.paths)
        profile = profiles.create("chaoxing", "draft-owner")
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        drafts = DraftRepository(self.paths, bank)
        draft = drafts.create(profile, "work:123", {"answers": {"q1": "A"}})
        loaded = drafts.get("chaoxing", profile.id, draft.id)
        self.assertEqual(loaded.payload["answers"]["q1"], "A")
        submitted = drafts.set_status(loaded, "submitted")
        self.assertEqual(submitted.status, "submitted")
        with bank.connect() as connection:
            row = connection.execute(
                "SELECT status, payload_json FROM formal_drafts WHERE id=?", (draft.id,)
            ).fetchone()
        self.assertEqual(row["status"], "submitted")
        self.assertIn('"q1":"A"', row["payload_json"])

    def test_question_identity_ignores_remote_ids_and_signed_media_query(self) -> None:
        first = {
            "remote_id": "q-1",
            "kind": "single_choice",
            "prompt": "  Which  option? ",
            "options": [
                {"id": "a", "text": "Alpha", "image": "https://img.example/a.png?sign=one"},
                {"id": "b", "text": "Beta", "image": "https://img.example/b.png?sign=one"},
            ],
        }
        second = {
            "remote_id": "q-99",
            "kind": "single_choice",
            "prompt": "Which option?",
            "options": [
                {"id": "remote-b", "text": "Beta", "image": "https://IMG.EXAMPLE/b.png?sign=two"},
                {"id": "remote-a", "text": "Alpha", "image": "https://img.example/a.png?sign=two"},
            ],
        }
        self.assertEqual(
            question_identity("chaoxing", first)[0], question_identity("chaoxing", second)[0]
        )
        self.assertNotIn("remote_id", canonical_question(first))

    def test_option_answer_rebinds_by_content_after_random_order(self) -> None:
        original_options = ["Alpha", "Beta", "Gamma"]
        rotated_options = ["Gamma", "Alpha", "Beta"]
        identity = question_identity(
            "chaoxing", {"kind": "single_choice", "prompt": "Pick", "options": original_options}
        )[0]
        self.assertTrue(identity)
        self.assertEqual(rebind_answer({"option": "Alpha"}, rotated_options), "B")

    def test_answer_repository_reuses_only_unconflicted_correct_candidate(self) -> None:
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        repository = AnswerRepository(bank)
        question = {
            "kind": "single_choice",
            "prompt": "Pick",
            "options": ["A. Alpha", "B. Beta"],
            "answer_evidence": {
                "source": "cidaren_answer_lib",
                "value": "Alpha",
                "verified": False,
            },
        }
        question_id, identity = repository.ingest_question("cidaren", question)
        self.assertEqual(repository.resolve_exact("cidaren", identity).status, "unverified")
        repository.record_candidate(question_id, {"text": "Alpha"}, "ai", "correct")
        self.assertEqual(repository.resolve_exact("cidaren", identity).status, "exact")
        repository.record_candidate(question_id, {"text": "Beta"}, "ai", "correct")
        self.assertEqual(repository.resolve_exact("cidaren", identity).status, "conflict")


class RunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.paths = DataPaths.resolve(self.root / "data-root")
        self.paths.initialize()
        source_root = Path(__file__).resolve().parents[1]
        self.registry = ProviderRegistry(source_root, python=sys.executable)
        self.fake_upstream = self.root / "upstream.py"
        self.fake_metadata = self.root / "SOURCE.json"
        self.fake_upstream.write_text("# fixture\n", encoding="utf-8")
        self.fake_metadata.write_text("{}\n", encoding="utf-8")
        self.spec = WorkerSpec(
            "chaoxing",
            source_root / "tests" / "fixtures" / "fake_worker.py",
            self.fake_upstream,
            self.fake_metadata,
        )
        self.manager = RunnerManager(self.registry, self.paths.logs)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_result_stream_and_log_summary_do_not_persist_payload(self) -> None:
        result = self.manager.invoke(self.spec, "health", timeout=5)
        self.assertEqual(result.data["session"]["token"], "fixture-secret-must-not-enter-log")
        text = result.log_path.read_text(encoding="utf-8")
        self.assertNotIn("fixture-secret-must-not-enter-log", text)
        self.assertNotIn("fixture question body", text)
        self.assertIn("items_count", text)

    def test_worker_error_preserves_code(self) -> None:
        with self.assertRaises(RunnerError) as raised:
            self.manager.invoke(self.spec, "fail", timeout=5)
        self.assertEqual(raised.exception.code, "expected")

    def test_timeout_stops_owned_process(self) -> None:
        with self.assertRaises(RunnerError) as raised:
            self.manager.invoke(self.spec, "sleep", timeout=0.1)
        self.assertEqual(raised.exception.code, "timeout")

    def test_pre_cancelled_run_stops_owned_process(self) -> None:
        cancelled = threading.Event()
        cancelled.set()
        with self.assertRaises(RunnerError) as raised:
            self.manager.invoke(self.spec, "sleep", timeout=5, cancel=cancelled)
        self.assertEqual(raised.exception.code, "cancelled")


class ScanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.paths = DataPaths.resolve(self.temporary.name)
        self.paths.initialize()
        self.profiles = ProfileStore(self.paths)
        self.profile = self.profiles.create("chaoxing", "scan")
        self.states = ProfileStateStore(self.paths)
        self.bank = QuestionBank(self.paths.database)
        self.bank.initialize()
        self.calls: list[tuple[str, str]] = []

        def result(data):
            return SimpleNamespace(data=data)

        class FakeService:
            def courses(inner, profile, *, cancel=None):
                self.calls.append(("courses", ""))
                return result({"courses": [{"remote_id": "course-1", "title": "one"}]})

            def tasks(inner, profile, course, *, cancel=None):
                self.calls.append(("tasks", str(course["remote_id"])))
                return result(
                    {
                        "tasks": [
                            {"remote_id": "task-1", "title": "one"},
                            {"remote_id": "task-2", "title": "two"},
                        ]
                    }
                )

            def questions(
                inner,
                profile,
                task,
                *,
                allow_read_that_starts_attempt=False,
                cancel=None,
            ):
                self.calls.append(("questions", str(task["remote_id"])))
                return result(
                    {
                        "questions": [
                            {
                                "kind": "single_choice",
                                "prompt": str(task["remote_id"]),
                                "options": ["A", "B"],
                            }
                        ]
                    }
                )

        inventory = InventoryStore(self.states)
        self.coordinator = ReadOnlyScanCoordinator(
            FakeService(), self.states, inventory, AnswerRepository(self.bank)
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_scan_persists_progress_and_resumes_without_repeating_tasks(self) -> None:
        first = self.coordinator.scan(self.profile)
        self.assertEqual(first.state, "completed")
        self.assertEqual(first.completed_tasks, 2)
        self.assertEqual(first.question_count, 2)
        calls_after_first = len(self.calls)
        second = self.coordinator.scan(self.profile)
        self.assertEqual(second.state, "completed")
        self.assertEqual(second.completed_tasks, 2)
        self.assertEqual(len(self.calls), calls_after_first + 2)  # courses + tasks only
        self.assertEqual(self.calls[-2:], [("courses", ""), ("tasks", "course-1")])


if __name__ == "__main__":
    unittest.main()
