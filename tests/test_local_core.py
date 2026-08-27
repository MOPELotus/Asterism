from __future__ import annotations

import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

from asterism.ai import AIAnswerService
from asterism.answers import (
    AnswerRepository,
    canonical_answer,
    canonical_question,
    question_identity,
    rebind_answer,
)
from asterism.batch import ManualBatchExecutor
from asterism.cidaren_bridge import CidarenAnswerBridge
from asterism.config import LocalConfigStore
from asterism.database import QuestionBank
from asterism.drafts import DraftRepository
from asterism.gui.controller import DesktopController
from asterism.inventory import InventoryStore
from asterism.notifications import NotificationDispatcher
from asterism.paths import DataPaths
from asterism.profiles import ProfileStateStore, ProfileStore
from asterism.providers import ProviderRegistry, WorkerSpec
from asterism.runner import RunnerError, RunnerManager
from asterism.scan import ReadOnlyScanCoordinator
from workers.common.runtime import payload_secrets
from workers.uai.worker import find_browser


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

    def test_worker_secret_collection_recurses_through_nested_credentials(self) -> None:
        values = payload_secrets(
            {"credentials": {"password": "p", "nested": [{"value": "cookie-value"}]}}
        )
        self.assertIn("p", values)
        self.assertIn("cookie-value", values)
        self.assertIn(
            "bridge-ticket",
            payload_secrets({"settings": {"answer_bridge": {"ticket": "bridge-ticket"}}}),
        )
        self.assertNotIn(
            "ordinary-setting",
            payload_secrets({"settings": {"label": "ordinary-setting"}}),
        )

    def test_uai_browser_detection_accepts_a_configured_executable(self) -> None:
        executable = self.paths.root / "browser.exe"
        executable.write_bytes(b"fixture")
        self.assertEqual(find_browser(str(executable)), executable.resolve())

    def test_worker_spec_prefers_portable_executable_when_present(self) -> None:
        executable = self.paths.root / "worker.exe"
        executable.write_bytes(b"fixture")
        spec = WorkerSpec(
            "chaoxing",
            self.paths.root / "worker.py",
            self.paths.root / "upstream",
            self.paths.root / "SOURCE.json",
            executable=executable,
        )
        self.assertEqual(spec.command("ignored-python")[0], str(executable))

    def test_provider_registry_exports_frozen_auxiliary_metadata_paths(self) -> None:
        registry = ProviderRegistry(Path(__file__).resolve().parents[1])
        chaoxing = registry.get("chaoxing")
        uai = registry.get("uai")
        self.assertTrue(chaoxing.environment["ASTERISM_CHAOXING_AUXILIARY_SOURCES"].endswith("AUXILIARY_SOURCES.json"))
        self.assertTrue(uai.environment["ASTERISM_UAI_BROWSER_SOURCE_METADATA"].endswith("BROWSER_SOURCE.json"))

    def test_controller_wraps_cidaren_run_with_a_short_lived_answer_bridge(self) -> None:
        calls: list[dict] = []

        class FakeService:
            def run_task(
                self,
                profile,
                task,
                *,
                answers=None,
                settings=None,
                cancel=None,
                on_event=None,
            ):
                calls.append(dict(settings or {}))
                bridge = (settings or {}).get("answer_bridge")
                assert isinstance(bridge, dict)
                request = urllib.request.Request(
                    bridge["url"],
                    data=b'{"kind":"resolve_answer"}',
                    method="POST",
                    headers={"Authorization": f"Bearer {bridge['ticket']}"},
                )
                with urllib.request.urlopen(request, timeout=2) as response:
                    assert response.status == 200
                return SimpleNamespace(data={"remote_state": "completed"})

        controller = object.__new__(DesktopController)
        controller.service = FakeService()
        controller.config = SimpleNamespace(ensure=lambda: {"providers": {}})
        controller._resolve_cidaren_answer = lambda _document: {"answer_available": False}
        controller._observe_cidaren_answer = lambda _document: {"ok": True}
        profile = ProfileStore(self.paths).create("cidaren", "bridge")
        controller.run_task(profile, {"remote_id": "task-1", "native": {}})
        self.assertIn("answer_bridge", calls[0])
        with self.assertRaises((OSError, urllib.error.URLError)):
            urllib.request.urlopen(
                urllib.request.Request(
                    calls[0]["answer_bridge"]["url"],
                    data=b"{}",
                    method="POST",
                    headers={"Authorization": f"Bearer {calls[0]['answer_bridge']['ticket']}"},
                ),
                timeout=1,
            )

    def test_controller_does_not_auto_answer_formal_cidaren_tasks(self) -> None:
        calls: list[dict] = []

        class FakeService:
            def run_task(
                self,
                profile,
                task,
                *,
                answers=None,
                settings=None,
                cancel=None,
                on_event=None,
            ):
                calls.append(dict(settings or {}))
                return SimpleNamespace(data={"remote_state": "completed"})

        controller = object.__new__(DesktopController)
        controller.service = FakeService()
        controller.config = SimpleNamespace(ensure=lambda: {"providers": {}})
        profile = ProfileStore(self.paths).create("cidaren", "formal")
        controller.run_task(
            profile,
            {
                "remote_id": "exam-1",
                "assessment_class": "formal",
                "native": {"route_kind": "course_exam"},
            },
            answers=[{"remote_id": "q-1", "value": "A"}],
        )
        self.assertNotIn("answer_bridge", calls[0])

    def test_draft_answers_accept_id_to_value_map(self) -> None:
        self.assertEqual(
            DesktopController._normalize_draft_answers({"q-1": "A", "q-2": ["B", "C"]}),
            [
                {"remote_id": "q-1", "value": "A"},
                {"remote_id": "q-2", "value": ["B", "C"]},
            ],
        )

    def test_cidaren_answer_bridge_is_loopback_scoped_and_dispatches_observations(self) -> None:
        resolved: list[dict] = []
        observed: list[dict] = []
        bridge = CidarenAnswerBridge(
            resolve=lambda value: resolved.append(value)
            or {"answer_available": True, "value": "A"},
            observe=lambda value: observed.append(value) or {"ok": True},
        )
        try:
            request = urllib.request.Request(
                bridge.url,
                data=b'{"kind":"resolve_answer"}',
                method="POST",
                headers={"Authorization": f"Bearer {bridge.ticket}"},
            )
            with urllib.request.urlopen(request, timeout=2) as response:
                self.assertEqual(response.status, 200)
                self.assertEqual(response.read(), b'{"answer_available":true,"value":"A"}')
            bad = urllib.request.Request(bridge.url, data=b"{}", method="POST")
            with self.assertRaises(urllib.error.HTTPError) as raised:
                urllib.request.urlopen(bad, timeout=2)
            self.assertEqual(raised.exception.code, 401)
            self.assertEqual(len(resolved), 1)
        finally:
            bridge.close()

    def test_profile_state_delete_does_not_touch_account_file(self) -> None:
        profiles = ProfileStore(self.paths)
        states = ProfileStateStore(self.paths)
        profile = profiles.create("chaoxing", "delete-me")
        states.save(profile, "session", {"cookie": "session-only"})
        states.delete_profile(profile)
        self.assertTrue(profiles.path_for(profile.provider, profile.id).exists())
        self.assertIsNone(states.load(profile, "session"))

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

    def test_existing_config_receives_new_model_defaults_without_losing_custom_values(self) -> None:
        config = LocalConfigStore(self.paths.config)
        config.save(
            {
                "version": 1,
                "ui": {"theme": "dark", "language": "zh-CN"},
                "notifications": {"enabled": False, "command": ""},
                "models": {},
                "providers": {"chaoxing": {"verification_attempt_budget": 5}},
            }
        )
        loaded = config.load()
        self.assertEqual(loaded["ui"]["theme"], "dark")
        self.assertEqual(loaded["providers"]["chaoxing"]["verification_attempt_budget"], 5)
        self.assertEqual(loaded["models"]["default"], "economy")
        self.assertIn("gpt_router", loaded["models"]["endpoints"])
        self.assertEqual(
            loaded["models"]["combinations"]["economy"]["timed"]["fallback_model"],
            "deepseek-chat",
        )

    def test_ai_default_combinations_build_responses_multimodal_request(self) -> None:
        config = LocalConfigStore(self.paths.config)
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        service = AIAnswerService(config, bank)
        choice = service.choose("economy", "untimed")
        self.assertEqual(choice.endpoint.protocol, "responses")
        self.assertEqual(choice.model, "gpt-5.6-terra")
        request = service.build_request(
            {
                "kind": "single_choice",
                "prompt": "看图",
                "options": [{"text": "A", "image": "https://example.test/a.png"}],
            },
            choice,
        )
        self.assertEqual(request["store"], False)
        self.assertEqual(request["text"]["format"]["type"], "json_object")
        self.assertTrue(
            any(item["type"] == "input_image" for item in request["input"][0]["content"])
        )
        file_question = {
            "kind": "provider_native",
            "prompt": "附件",
            "attachments": [{"type": "attachment", "url": "https://example.test/a.pdf"}],
        }
        file_request = service.build_request(file_question, choice)
        self.assertTrue(
            any(item["type"] == "input_file" for item in file_request["input"][0]["content"])
        )
        generic_question = {
            "kind": "single_choice",
            "prompt": "link",
            "reference": "https://example.test/page",
        }
        generic_request = service.build_request(generic_question, choice)
        self.assertFalse(
            any(
                item.get("type") in {"input_image", "input_file"}
                for item in generic_request["input"][0]["content"]
            )
        )

    def test_ai_fallback_uses_its_own_model_when_primary_is_unavailable(self) -> None:
        config = LocalConfigStore(self.paths.config)
        value = config.ensure()
        value["models"]["endpoints"]["gpt_router"].update(
            {"base_url": "https://router.test", "api_key": "router-key"}
        )
        value["models"]["endpoints"]["domestic_backup"].update(
            {"base_url": "https://domestic.test", "api_key": "domestic-key"}
        )
        config.save(value)
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        service = AIAnswerService(config, bank)
        calls = []

        def fake_request(question, choice, key, timeout):
            calls.append((choice.endpoint.name, choice.model, key))
            if choice.endpoint.name == "gpt_router":
                raise RuntimeError("router unavailable")
            return {"answer": "B", "confidence": 0.7}, {"total_tokens": 2}

        service._request = fake_request
        result = service.answer(
            "chaoxing", {"kind": "single_choice", "prompt": "Fallback", "options": ["A", "B"]}
        )
        self.assertEqual(result["answer"]["answer"], "B")
        self.assertEqual(
            calls,
            [
                ("gpt_router", "gpt-5.6-terra", "router-key"),
                ("domestic_backup", "deepseek-chat", "domestic-key"),
            ],
        )

    def test_ai_cache_round_trip(self) -> None:
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        bank.put_ai_cache("cache-1", "gpt:test", {"answer": "A"}, {"total_tokens": 3})
        self.assertEqual(
            bank.get_ai_cache("cache-1"),
            {
                "model_profile": "gpt:test",
                "response": {"answer": "A"},
                "usage": {"total_tokens": 3},
            },
        )

    def test_ai_response_requires_bounded_confidence(self) -> None:
        with self.assertRaises(RuntimeError):
            AIAnswerService.parse_response(
                {"output_text": '{"answer":"A","confidence":2}'}, "responses"
            )

    def test_controller_prepares_rebound_local_answers_for_chaoxing(self) -> None:
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        repository = AnswerRepository(bank)
        question = {
            "kind": "single_choice",
            "prompt": "Prepared",
            "options": ["Alpha", "Beta"],
            "answer_evidence": {"source": "provider_native", "value": "A", "verified": True},
            "remote_id": "q-1",
        }
        repository.ingest_question("chaoxing", question)

        class FakeService:
            def questions(self, profile, task, *, allow_read_that_starts_attempt=False):
                return SimpleNamespace(
                    data={"questions": [{**question, "options": ["Beta", "Alpha"]}]}
                )

        controller = object.__new__(DesktopController)
        controller.service = FakeService()
        controller.bank = bank
        profile = ProfileStore(self.paths).create("chaoxing", "prepared")
        answers = controller.prepare_answers(profile, {"remote_id": "task-1"})
        self.assertEqual(answers, [{"remote_id": "q-1", "value": "B"}])

    def test_ai_service_prefers_exact_local_candidate_before_remote_request(self) -> None:
        config = LocalConfigStore(self.paths.config)
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        repository = AnswerRepository(bank)
        question = {
            "kind": "single_choice",
            "prompt": "Cached",
            "options": ["A", "B"],
        }
        question_id, _identity = repository.ingest_question("chaoxing", question)
        repository.record_candidate(question_id, {"option": "A"}, "provider_native", "correct")
        service = AIAnswerService(config, bank)
        result = service.answer("chaoxing", question)
        self.assertEqual(result["source"], "local_cache")
        self.assertTrue(result["cached"])
        self.assertEqual(result["answer"]["answer"], "A")

    def test_ai_cache_rebinds_canonical_option_content_to_current_order(self) -> None:
        config = LocalConfigStore(self.paths.config)
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        repository = AnswerRepository(bank)
        question = {
            "kind": "single_choice",
            "prompt": "Pick",
            "options": ["Alpha", "Beta"],
        }
        question_id, identity = repository.ingest_question(
            "chaoxing",
            {
                **question,
                "answer_evidence": {"source": "provider_native", "value": "A", "verified": True},
            },
        )
        self.assertGreater(question_id, 0)
        service = AIAnswerService(config, bank)
        result = service.answer(
            "chaoxing",
            {**question, "options": ["Beta", "Alpha"]},
        )
        self.assertEqual(result["source"], "local_cache")
        self.assertEqual(result["answer"]["answer"], "B")
        self.assertEqual(
            identity,
            question_identity("chaoxing", {**question, "options": ["Beta", "Alpha"]})[0],
        )

    def test_notifications_are_disabled_by_default(self) -> None:
        config = LocalConfigStore(self.paths.config)
        result = NotificationDispatcher(config).send(
            "success", provider="chaoxing", operation="run", summary={"status": "success"}
        )
        self.assertFalse(result.sent)

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

    def test_question_identity_keeps_mixed_native_content_but_ignores_answer_state(self) -> None:
        first = {
            "kind": "provider_native",
            "prompt": "挖空 __ 与图片",
            "material": [{"text": "材料", "image": "https://cdn.test/a.png?token=one"}],
            "native": {
                "question": "挖空 __ 与图片",
                "attachments": [{"url": "https://cdn.test/a.png?token=one", "id": "x"}],
                "learner_response": "old-answer",
            },
        }
        second = {
            "kind": "provider_native",
            "prompt": "挖空 __ 与图片",
            "material": [{"text": "材料", "image": "https://cdn.test/a.png?token=two"}],
            "native": {
                "question": "挖空 __ 与图片",
                "attachments": [{"url": "https://CDN.TEST/a.png?token=three", "id": "y"}],
                "learner_response": "new-answer",
            },
        }
        self.assertEqual(
            question_identity("chaoxing", first)[0], question_identity("chaoxing", second)[0]
        )

    def test_question_identity_ignores_cidaren_remote_topic_context(self) -> None:
        first = {
            "kind": "single_choice",
            "prompt": "选择正确答案",
            "options": [{"answer_tag": 0, "content": "甲"}, {"answer_tag": 1, "content": "乙"}],
            "native": {
                "topic_code": "topic-a",
                "course_id": "course-a",
                "task_id": "task-a",
            },
        }
        second = {
            **first,
            "native": {
                "topic_code": "topic-b",
                "course_id": "course-b",
                "task_id": "task-b",
            },
        }
        self.assertEqual(
            question_identity("cidaren", first)[0], question_identity("cidaren", second)[0]
        )

    def test_option_answer_rebinds_by_content_after_random_order(self) -> None:
        original_options = ["Alpha", "Beta", "Gamma"]
        rotated_options = ["Gamma", "Alpha", "Beta"]
        identity = question_identity(
            "chaoxing", {"kind": "single_choice", "prompt": "Pick", "options": original_options}
        )[0]
        self.assertTrue(identity)
        self.assertEqual(rebind_answer({"option": "Alpha"}, rotated_options), "B")

    def test_option_answer_canonicalizes_content_as_well_as_provider_key(self) -> None:
        self.assertEqual(
            canonical_answer("Alpha", ["Alpha", "Beta"]),
            {"option": "Alpha"},
        )

    def test_numeric_native_answer_canonicalizes_cidaren_answer_tag(self) -> None:
        options = [
            {"answer_tag": 0, "content": "Alpha"},
            {"answer_tag": 1, "content": "Beta"},
        ]
        self.assertEqual(canonical_answer(1, options), {"option": {"content": "Beta"}})
        self.assertEqual(
            rebind_answer({"option": {"content": "Beta"}}, list(reversed(options))), "1"
        )

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

    def test_answer_repository_conflicts_when_candidates_have_mixed_outcomes(self) -> None:
        bank = QuestionBank(self.paths.database)
        bank.initialize()
        repository = AnswerRepository(bank)
        question_id, identity = repository.ingest_question(
            "chaoxing", {"kind": "single_choice", "prompt": "Mixed", "options": ["A", "B"]}
        )
        repository.record_candidate(question_id, {"text": "A"}, "native", "correct")
        repository.record_candidate(question_id, {"text": "B"}, "native", "incorrect")
        self.assertEqual(repository.resolve_exact("chaoxing", identity).status, "conflict")


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


class BatchTests(unittest.TestCase):
    def test_batch_passes_answer_lists_and_serializes_uai(self) -> None:
        calls = []

        class FakeService:
            def run_task(self, profile, task, *, answers=None, settings=None, cancel=None):
                calls.append((profile.provider, task["remote_id"], answers))
                return SimpleNamespace(data={"remote_state": "completed"})

        profile = SimpleNamespace(provider="uai")
        tasks = [{"remote_id": "one"}, {"remote_id": "two"}]
        results = ManualBatchExecutor(FakeService()).run(
            profile,
            tasks,
            concurrency=32,
            answer_provider=lambda task: [{"remote_id": task["remote_id"], "value": "A"}],
        )
        self.assertEqual([item.error_code for item in results], [None, None])
        self.assertEqual(
            calls,
            [
                ("uai", "one", [{"remote_id": "one", "value": "A"}]),
                ("uai", "two", [{"remote_id": "two", "value": "A"}]),
            ],
        )

    def test_batch_can_use_controller_runner_for_provider_specific_bridges(self) -> None:
        calls = []

        class FakeService:
            def run_task(self, profile, task, *, answers=None, settings=None, cancel=None):
                raise AssertionError("provider runner should be used")

        def run_task(profile, task, *, answers=None, settings=None, cancel=None):
            calls.append(task["remote_id"])
            return SimpleNamespace(data={"remote_state": "completed"})

        profile = SimpleNamespace(provider="cidaren")
        results = ManualBatchExecutor(FakeService()).run(
            profile,
            [{"remote_id": "one"}, {"remote_id": "two"}],
            run_task=run_task,
        )
        self.assertEqual([item.error_code for item in results], [None, None])
        self.assertEqual(calls, ["one", "two"])

    def test_batch_forwards_events_with_task_identity(self) -> None:
        events = []

        class FakeService:
            def run_task(
                self,
                profile,
                task,
                *,
                answers=None,
                settings=None,
                cancel=None,
                on_event=None,
            ):
                if on_event is not None:
                    on_event({"type": "progress", "current": 1, "total": 1})
                return SimpleNamespace(data={"remote_state": "completed"})

        profile = SimpleNamespace(provider="chaoxing")
        results = ManualBatchExecutor(FakeService()).run(
            profile,
            [{"remote_id": "one"}],
            on_event=events.append,
        )
        self.assertEqual([item.error_code for item in results], [None])
        self.assertEqual(events[0]["task_remote_id"], "one")
        self.assertEqual(events[0]["batch_index"], 0)


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
