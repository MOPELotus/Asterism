from __future__ import annotations

import threading
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import uuid4

from ..ai import AIAnswerService
from ..answers import AnswerRepository, canonical_answer, question_identity, rebind_answer
from ..batch import ManualBatchExecutor
from ..cidaren_bridge import CidarenAnswerBridge
from ..config import LocalConfigStore
from ..database import QuestionBank
from ..drafts import DraftRepository, FormalDraft
from ..inventory import InventoryStore
from ..notifications import NotificationDispatcher, NotificationResult
from ..paths import DataPaths, application_root
from ..profiles import Profile, ProfileStateStore, ProfileStore
from ..providers import ProviderRegistry
from ..runner import RunnerManager
from ..scan import ReadOnlyScanCoordinator, ScanStatus
from ..service import ProviderOperationResult, ProviderService


@dataclass
class DesktopController:
    paths: DataPaths
    profiles: ProfileStore
    states: ProfileStateStore
    config: LocalConfigStore
    bank: QuestionBank
    drafts: DraftRepository
    inventory: InventoryStore
    service: ProviderService
    batch: ManualBatchExecutor
    scanner: ReadOnlyScanCoordinator
    ai: AIAnswerService
    notifications: NotificationDispatcher

    @classmethod
    def create(cls, data_root: str | Path | None = None, source_root: str | Path | None = None):
        paths = DataPaths.resolve(data_root)
        paths.initialize()
        resources = Path(source_root).resolve() if source_root else application_root()
        registry = ProviderRegistry(resources)
        bank = QuestionBank(paths.database)
        bank.initialize()
        states = ProfileStateStore(paths)
        service = ProviderService(
            registry,
            RunnerManager(registry, paths.logs, states),
            states,
            InventoryStore(states),
        )
        inventory = InventoryStore(states)
        scanner = ReadOnlyScanCoordinator(service, states, inventory, AnswerRepository(bank))
        ai = AIAnswerService(LocalConfigStore(paths.config), bank)
        config_store = LocalConfigStore(paths.config)
        return cls(
            paths=paths,
            profiles=ProfileStore(paths),
            states=states,
            config=LocalConfigStore(paths.config),
            bank=bank,
            drafts=DraftRepository(paths, bank),
            inventory=inventory,
            service=service,
            batch=ManualBatchExecutor(service),
            scanner=scanner,
            ai=ai,
            notifications=NotificationDispatcher(config_store),
        )

    def health(
        self,
        provider: str,
        *,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> ProviderOperationResult:
        # A health call does not require a Profile or credentials.
        spec = self.service.registry.get(provider)
        return self.service.runner.invoke(spec, "health", timeout=30, on_event=on_event)

    def save_profile(
        self,
        provider: str,
        label: str,
        credentials: dict[str, Any],
        settings: dict[str, Any] | None = None,
        profile_id: str | None = None,
    ) -> Profile:
        if profile_id:
            profile = self.profiles.get(provider, profile_id)
            from dataclasses import replace

            profile = replace(
                profile,
                label=label.strip(),
                credentials=dict(credentials),
                settings=dict(settings or profile.settings),
            )
            self.profiles.save(profile)
            return profile
        profile = self.profiles.create(provider, label)
        from dataclasses import replace

        profile = replace(profile, credentials=dict(credentials), settings=dict(settings or {}))
        self.profiles.save(profile)
        return profile

    def delete_profile(self, profile: Profile) -> None:
        self.states.delete_profile(profile)
        self.profiles.delete(profile.provider, profile.id)

    def sync_courses(
        self,
        profile: Profile,
        *,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> list[dict[str, Any]]:
        return list(
            self.service.courses(profile, cancel=cancel, on_event=on_event).data.get("courses", [])
        )

    def sync_tasks(
        self,
        profile: Profile,
        course: dict[str, Any],
        *,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> list[dict[str, Any]]:
        return list(
            self.service.tasks(profile, course, cancel=cancel, on_event=on_event).data.get(
                "tasks", []
            )
        )

    def scan_questions(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        allow_read_that_starts_attempt: bool = False,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> list[dict[str, Any]]:
        result = self.service.questions(
            profile,
            task,
            allow_read_that_starts_attempt=allow_read_that_starts_attempt,
            cancel=cancel,
            on_event=on_event,
        )
        questions = result.data.get("questions", [])
        if isinstance(questions, list):
            for question in questions:
                if isinstance(question, dict):
                    self._ingest_question(profile.provider, question)
            self.states.save(
                profile,
                "question-scan",
                {
                    "updated_at": result.run.log_path.stat().st_mtime,
                    "task_remote_id": str(task.get("remote_id") or ""),
                    "question_count": len(questions),
                },
            )
            return [dict(question) for question in questions if isinstance(question, dict)]
        return []

    def _ingest_question(self, provider: str, question: dict[str, Any]) -> None:
        self.answer_repository().ingest_question(provider, question)

    def answer_repository(self) -> AnswerRepository:
        return AnswerRepository(self.bank)

    def run_task(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        answers: dict[str, Any] | None = None,
        settings: dict[str, Any] | None = None,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> ProviderOperationResult:
        if profile.provider != "cidaren":
            return self.service.run_task(
                profile, task, answers=answers, settings=settings, cancel=cancel, on_event=on_event
            )

        bridge = CidarenAnswerBridge(
            resolve=lambda document: self._resolve_cidaren_answer(document),
            observe=lambda document: self._observe_cidaren_answer(document),
        )
        task_ref = str(task.get("remote_id") or uuid4())
        execution_id = str(uuid4())
        merged_settings = dict(profile.settings)
        if settings is not None:
            merged_settings.update(settings)
        merged_settings["answer_bridge"] = bridge.settings(
            execution_id=execution_id, task_id=task_ref, remote_task_id=task_ref
        )
        try:
            return self.service.run_task(
                profile,
                task,
                answers=answers,
                settings=merged_settings,
                cancel=cancel,
                on_event=on_event,
            )
        finally:
            bridge.close()

    def _resolve_cidaren_answer(self, document: dict[str, Any]) -> dict[str, Any]:
        question = self._cidaren_question(document)
        if question is None:
            return {"answer_available": False}
        try:
            response = self.answer_question(
                "cidaren", question, route=str(document.get("route") or "untimed")
            )
            value = response.get("answer", {}).get("answer")
            if value is None:
                return {"answer_available": False}
            return {"answer_available": True, "value": value}
        except (OSError, RuntimeError, ValueError):
            return {"answer_available": False}

    def _observe_cidaren_answer(self, document: dict[str, Any]) -> dict[str, Any]:
        question = self._cidaren_question(document)
        submitted = document.get("submitted")
        if question is None or submitted is None:
            return {"ok": True}
        try:
            repository = self.answer_repository()
            question_id, _identity = repository.ingest_question("cidaren", question)
            outcome_value = document.get("outcome")
            correctness = (
                outcome_value.get("correctness")
                if isinstance(outcome_value, dict)
                else None
            )
            outcome = (
                "correct"
                if correctness == "correct"
                else "incorrect"
                if correctness == "wrong"
                else "unverified"
            )
            source = str(document.get("source") or "cidaren_worker")
            source_kind = "ai" if source == "bridge" else "provider_native"
            repository.record_candidate(
                question_id,
                canonical_answer(submitted, question.get("options")),
                source_kind,
                outcome,
                source_ref=f"cidaren:{source}",
                task_ref=str(document.get("remote_task_id") or ""),
                details={"remote_id": str(document.get("remote_id") or "")},
            )
        except (OSError, TypeError, ValueError, RuntimeError):
            # Observation persistence must never turn a successful donor run
            # into a failed platform operation.
            return {"ok": False}
        return {"ok": True}

    @staticmethod
    def _cidaren_question(document: Mapping[str, Any]) -> dict[str, Any] | None:
        exam = document.get("exam")
        if not isinstance(exam, Mapping):
            return None
        stem = exam.get("stem")
        prompt = ""
        if isinstance(stem, Mapping):
            prompt = str(stem.get("content") or stem.get("text") or "").strip()
        elif stem is not None:
            prompt = str(stem).strip()
        if not prompt:
            prompt = str(
                exam.get("topic_title") or exam.get("question") or exam.get("word") or ""
            ).strip()
        if not prompt:
            return None
        mode = str(exam.get("topic_mode") or exam.get("topic_type") or exam.get("type") or "")
        lowered = mode.casefold()
        kind = {
            "1": "single_choice",
            "single": "single_choice",
            "choice": "single_choice",
            "2": "multiple_choice",
            "multiple": "multiple_choice",
            "3": "true_false",
            "judge": "true_false",
        }.get(
            lowered,
            "matching"
            if "match" in lowered
            else "ordering"
            if "order" in lowered
            else "provider_native",
        )
        question: dict[str, Any] = {
            "remote_id": str(document.get("remote_id") or exam.get("topic_code") or ""),
            "kind": kind,
            "prompt": prompt,
            "options": exam.get("options") or exam.get("option") or [],
            "native": dict(exam),
        }
        if isinstance(stem, Mapping) and stem.get("remark"):
            question["material"] = {"remark": stem.get("remark")}
        return question

    def prepare_answers(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        combination: str | None = None,
        route: str = "untimed",
        allow_read_that_starts_attempt: bool = False,
    ) -> list[dict[str, Any]]:
        """Resolve local/AI answers into the Worker wire shape.

        Provider Workers continue to own encoding and submission; this helper
        only joins the global question bank to current remote question IDs.
        """
        if profile.provider not in {"chaoxing", "cidaren"}:
            return []
        questions = self.service.questions(
            profile,
            task,
            allow_read_that_starts_attempt=allow_read_that_starts_attempt,
        ).data.get("questions", [])
        if not isinstance(questions, list):
            return []
        resolved: list[dict[str, Any]] = []
        repository = self.answer_repository()
        for question in questions:
            if not isinstance(question, dict) or not question.get("remote_id"):
                continue
            try:
                identity, _canonical = question_identity(profile.provider, question)
            except ValueError:
                continue
            if self.bank.question_id(profile.provider, identity) is None:
                repository.ingest_question(profile.provider, question)
            exact = repository.resolve_exact(profile.provider, identity)
            if exact.status == "exact":
                answer = rebind_answer(exact.answer, question.get("options"))
            else:
                try:
                    response = self.answer_question(
                        profile.provider,
                        question,
                        combination=combination,
                        route=route,
                    )
                except (OSError, RuntimeError, ValueError):
                    continue
                value = response.get("answer", {})
                answer = value.get("answer") if isinstance(value, dict) else None
            if answer is not None:
                resolved.append({"remote_id": str(question["remote_id"]), "value": answer})
        return resolved

    def run_batch(
        self,
        profile: Profile,
        tasks: list[dict[str, Any]],
        *,
        concurrency: int = 1,
        settings: dict[str, Any] | None = None,
        answer_provider: Callable[[dict[str, Any]], list[dict[str, Any]] | None] | None = None,
        cancel: threading.Event | None = None,
    ):
        return self.batch.run(
            profile,
            tasks,
            concurrency=concurrency,
            settings=settings,
            answer_provider=answer_provider,
            cancel=cancel,
        )

    def read_duration(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> ProviderOperationResult:
        return self.service.read_duration(profile, task, cancel=cancel, on_event=on_event)

    def inspect_task(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> ProviderOperationResult:
        return self.service.inspect(profile, task, cancel=cancel, on_event=on_event)

    def scan_all(
        self,
        profile: Profile,
        *,
        allow_cidaren_attempt: bool = False,
        max_retries: int = 3,
        cancel: threading.Event | None = None,
        on_update: Callable[[ScanStatus], None] | None = None,
    ) -> ScanStatus:
        return self.scanner.scan(
            profile,
            allow_cidaren_attempt=allow_cidaren_attempt,
            max_retries=max_retries,
            cancel=cancel,
            on_update=on_update,
        )

    def scan_status(self, profile: Profile) -> ScanStatus:
        return self.scanner.status(profile)

    def list_questions(self, provider: str | None = None) -> list[dict[str, Any]]:
        return self.bank.list_questions(provider)

    def answer_question(
        self,
        provider: str,
        question: dict[str, Any],
        *,
        combination: str | None = None,
        route: str = "untimed",
        force_refresh: bool = False,
    ) -> dict[str, Any]:
        return self.ai.answer(
            provider,
            question,
            combination=combination,
            route=route,
            force_refresh=force_refresh,
        )

    def notify(
        self,
        event: str,
        *,
        provider: str,
        operation: str,
        summary: dict[str, Any],
    ) -> NotificationResult:
        return self.notifications.send(
            event, provider=provider, operation=operation, summary=summary
        )

    def save_draft(self, profile: Profile, task_ref: str, payload: dict[str, Any]):
        return self.drafts.create(profile, task_ref, payload)

    def load_draft(self, provider: str, profile_id: str, draft_id: str) -> FormalDraft:
        return self.drafts.get(provider, profile_id, draft_id)

    def update_draft(self, draft: FormalDraft, payload: dict[str, Any]) -> FormalDraft:
        from dataclasses import replace

        updated = replace(draft, payload=dict(payload))
        self.drafts.save(updated)
        return updated

    def submit_draft(self, draft: FormalDraft) -> ProviderOperationResult:
        if draft.status != "draft":
            raise ValueError("only a draft can be submitted")
        profile = self.profiles.get(draft.provider, draft.profile_id)
        task = draft.payload.get("task")
        if not isinstance(task, dict):
            raise ValueError("draft payload.task must be an object")
        answers = draft.payload.get("answers")
        if answers is not None and not isinstance(answers, dict):
            raise ValueError("draft payload.answers must be an object")
        settings = draft.payload.get("settings")
        if settings is not None and not isinstance(settings, dict):
            raise ValueError("draft payload.settings must be an object")
        result = self.service.run_task(profile, task, answers=answers, settings=settings)
        self.drafts.set_status(draft, "submitted")
        return result

    def draft_rows(self) -> list[dict[str, Any]]:
        return [
            {
                "id": draft.id,
                "provider": draft.provider,
                "profile_id": draft.profile_id,
                "task_ref": draft.task_ref,
                "status": draft.status,
                "updated_at": draft.updated_at,
            }
            for draft in self.drafts.list()
        ]
