from __future__ import annotations

import threading
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
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
        config_store = LocalConfigStore(paths.config)
        bank = QuestionBank(paths.database)
        bank.initialize()
        states = ProfileStateStore(paths)
        service = ProviderService(
            registry,
            RunnerManager(registry, paths.logs, states),
            states,
            InventoryStore(states),
            settings_provider=lambda provider: config_store.ensure()
            .get("providers", {})
            .get(provider, {}),
        )
        inventory = InventoryStore(states)
        scanner = ReadOnlyScanCoordinator(service, states, inventory, AnswerRepository(bank))
        ai = AIAnswerService(LocalConfigStore(paths.config), bank)
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
        enabled: bool | None = None,
    ) -> Profile:
        if profile_id:
            profile = self.profiles.get(provider, profile_id)
            from dataclasses import replace

            profile = replace(
                profile,
                label=label.strip(),
                credentials=dict(credentials),
                settings=dict(settings or profile.settings),
                enabled=profile.enabled if enabled is None else enabled,
            )
            self.profiles.save(profile)
            return profile
        profile = self.profiles.create(provider, label)
        from dataclasses import replace

        profile = replace(
            profile,
            credentials=dict(credentials),
            settings=dict(settings or {}),
            enabled=True if enabled is None else enabled,
        )
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
        answers: Any | None = None,
        settings: dict[str, Any] | None = None,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> ProviderOperationResult:
        merged_settings = self.provider_settings(profile, settings)
        if profile.provider != "cidaren" or self._is_formal_task(task):
            result = self.service.run_task(
                profile,
                task,
                answers=answers,
                settings=merged_settings,
                cancel=cancel,
                on_event=on_event,
            )
            if self._needs_challenge_escalation(profile, task, result, merged_settings):
                # The worker can report that a challenge point is still
                # incomplete after its local state polling. Re-resolve the
                # answer and replay the point a bounded number of times before
                # consuming the expensive Sol xhigh escalation. The explicit
                # marker keeps escalation and retry calls from recursing.
                retry_attempts = merged_settings.get("challenge_retry_attempts", 3)
                try:
                    retry_attempts = max(0, min(3, int(retry_attempts)))
                except (TypeError, ValueError):
                    retry_attempts = 3
                for attempt in range(1, retry_attempts + 1):
                    retry_answers = self.prepare_answers(
                        profile,
                        task,
                        combination=str(merged_settings.get("answer_combination") or "economy"),
                        route="untimed",
                        force_refresh=True,
                        cancel=cancel,
                        on_event=on_event,
                    )
                    if not retry_answers:
                        break
                    retry_settings = dict(merged_settings)
                    retry_settings["_challenge_retry_attempt"] = attempt
                    retry_settings["challenge_retry_attempts"] = 1
                    result = self.service.run_task(
                        profile,
                        task,
                        answers=retry_answers,
                        settings=retry_settings,
                        cancel=cancel,
                        on_event=on_event,
                    )
                    if not self._needs_challenge_escalation(
                        profile, task, result, retry_settings
                    ):
                        return result
                escalation_answers = self.prepare_answers(
                    profile,
                    task,
                    combination="gpt_only",
                    route="untimed",
                    force_refresh=True,
                )
                escalation_settings = dict(merged_settings)
                escalation_settings["_challenge_escalation"] = True
                escalation_settings["challenge_retry_attempts"] = 1
                escalation_settings["challenge_escalation_route"] = "sol_xhigh"
                if not escalation_answers:
                    # Never replay the answer that just failed the challenge;
                    # an unavailable escalation model leaves the task safely
                    # incomplete for explicit user inspection.
                    return result
                return self.service.run_task(
                    profile,
                    task,
                    answers=escalation_answers,
                    settings=escalation_settings,
                    cancel=cancel,
                    on_event=on_event,
                )
            return result

        bridge = CidarenAnswerBridge(
            resolve=lambda document: self._resolve_cidaren_answer(document),
            observe=lambda document: self._observe_cidaren_answer(document),
        )
        task_ref = str(task.get("remote_id") or uuid4())
        execution_id = str(uuid4())
        merged_settings["answer_bridge"] = bridge.settings(
            execution_id=execution_id,
            task_id=task_ref,
            remote_task_id=task_ref,
            combination=str(merged_settings.get("answer_combination") or ""),
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

    @staticmethod
    def _is_formal_task(task: Mapping[str, Any]) -> bool:
        native = task.get("native")
        route = native.get("route_kind") if isinstance(native, Mapping) else None
        return task.get("assessment_class") == "formal" or route in {
            "course_exam",
            "course_homework",
        }

    @staticmethod
    def _needs_challenge_escalation(
        profile: Profile,
        task: Mapping[str, Any],
        result: ProviderOperationResult,
        settings: Mapping[str, Any],
    ) -> bool:
        if profile.provider != "chaoxing" or settings.get("_challenge_escalation"):
            return False
        native = task.get("native")
        if not isinstance(native, Mapping) or native.get("route_kind") != "knowledge_point":
            return False
        data = result.data if isinstance(result, ProviderOperationResult) else {}
        details = data.get("result") if isinstance(data, Mapping) else None
        return (
            isinstance(details, Mapping)
            and details.get("challenge_escalation_requested") is True
        )

    def provider_settings(
        self, profile: Profile, overrides: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        configured = self.config.ensure().get("providers", {}).get(profile.provider, {})
        result = dict(configured) if isinstance(configured, Mapping) else {}
        result.update(profile.settings)
        if overrides is not None:
            result.update(overrides)
        return result

    def _resolve_cidaren_answer(self, document: dict[str, Any]) -> dict[str, Any]:
        question = self._cidaren_question(document)
        if question is None:
            return {"answer_available": False}
        try:
            response = self.answer_question(
                "cidaren",
                question,
                combination=str(document.get("combination") or "") or None,
                route=str(document.get("route") or "untimed"),
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
        force_refresh: bool = False,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
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
            cancel=cancel,
            on_event=on_event,
        ).data.get("questions", [])
        if not isinstance(questions, list):
            return []
        return self._resolve_question_answers(
            profile,
            questions,
            combination=combination,
            route=route,
            force_refresh=force_refresh,
        )

    def _resolve_question_answers(
        self,
        profile: Profile,
        questions: list[Any],
        *,
        combination: str | None,
        route: str,
        force_refresh: bool = False,
    ) -> list[dict[str, Any]]:
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
                        force_refresh=force_refresh,
                    )
                except (OSError, RuntimeError, ValueError):
                    continue
                value = response.get("answer", {})
                answer = value.get("answer") if isinstance(value, dict) else None
            if answer is not None:
                resolved.append({"remote_id": str(question["remote_id"]), "value": answer})
        return resolved

    def prepare_formal_draft(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        combination: str = "economy",
        settings: Mapping[str, Any] | None = None,
        allow_read_that_starts_attempt: bool = False,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> FormalDraft:
        """Read and prefill a formal assessment without submitting it.

        Some Provider read paths may establish an assessment attempt.  The UI
        must obtain explicit operator confirmation before setting
        ``allow_read_that_starts_attempt``.  Final submission remains a
        separate action on the drafts page.
        """
        if not self._is_formal_task(task):
            raise ValueError("formal draft preparation requires a formal task")
        questions = self.scan_questions(
            profile,
            task,
            allow_read_that_starts_attempt=allow_read_that_starts_attempt,
            cancel=cancel,
            on_event=on_event,
        )
        native = task.get("native") if isinstance(task.get("native"), Mapping) else {}
        route = "timed" if native.get("route_kind") == "course_exam" else "untimed"
        answers = self._resolve_question_answers(
            profile,
            list(questions),
            combination=combination,
            route=route,
        )
        answered = {str(row["remote_id"]) for row in answers}
        missing = [
            str(question.get("remote_id"))
            for question in questions
            if isinstance(question, Mapping)
            and question.get("remote_id")
            and str(question.get("remote_id")) not in answered
        ]
        draft_settings = dict(settings or {})
        draft_settings["answer_combination"] = combination
        return self.save_draft(
            profile,
            str(task.get("remote_id") or ""),
            {
                "task": dict(task),
                "questions": [dict(question) for question in questions],
                "answers": answers,
                "unresolved_question_ids": missing,
                "settings": draft_settings,
            },
        )

    def prepare_formal_drafts(
        self,
        profile: Profile,
        tasks: list[dict[str, Any]],
        *,
        combination: str = "economy",
        settings: Mapping[str, Any] | None = None,
        allow_read_that_starts_attempt: bool = False,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> list[FormalDraft]:
        drafts: list[FormalDraft] = []
        total = len(tasks)
        for index, task in enumerate(tasks, 1):
            if cancel is not None and cancel.is_set():
                raise RuntimeError("formal draft preparation was cancelled")
            if on_event is not None:
                on_event(
                    {
                        "type": "progress",
                        "current": index,
                        "total": total,
                        "message": str(task.get("title") or task.get("remote_id") or "formal"),
                    }
                )
            drafts.append(
                self.prepare_formal_draft(
                    profile,
                    task,
                    combination=combination,
                    settings=settings,
                    allow_read_that_starts_attempt=allow_read_that_starts_attempt,
                    cancel=cancel,
                    on_event=on_event,
                )
            )
        return drafts

    def run_batch(
        self,
        profile: Profile,
        tasks: list[dict[str, Any]],
        *,
        concurrency: int = 1,
        settings: dict[str, Any] | None = None,
        answer_provider: Callable[[dict[str, Any]], list[dict[str, Any]] | None] | None = None,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ):
        return self.batch.run(
            profile,
            tasks,
            concurrency=concurrency,
            settings=settings,
            answer_provider=answer_provider,
            cancel=cancel,
            run_task=self.run_task,
            on_event=on_event,
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

    def scan_all_profiles(
        self,
        *,
        allow_cidaren_attempt: bool = False,
        max_retries: int = 3,
        cancel: threading.Event | None = None,
        on_update: Callable[[Profile, ScanStatus], None] | None = None,
    ) -> list[ScanStatus]:
        """Run the manual Chaoxing inventory scan across enabled local profiles.

        Each profile owns its cursor and failures are isolated so one blocked
        account cannot prevent the remaining accounts from being scanned.
        This is deliberately a manual action, not a scheduler.
        """
        results: list[ScanStatus] = []
        for profile in self.profiles.list("chaoxing"):
            if not profile.enabled:
                continue
            if cancel is not None and cancel.is_set():
                break
            try:
                status = self.scanner.scan(
                    profile,
                    allow_cidaren_attempt=allow_cidaren_attempt,
                    max_retries=max_retries,
                    cancel=cancel,
                    on_update=(
                        (lambda value, profile=profile: on_update(profile, value))
                        if on_update is not None
                        else None
                    ),
                )
            except Exception:
                # A malformed donor payload must be isolated to this Profile;
                # the remaining local accounts still need their own scan.
                status = self.scanner.status(profile)
            results.append(status)
        return results

    def list_questions(self, provider: str | None = None) -> list[dict[str, Any]]:
        return self.bank.list_questions(provider)

    def question_evidence(self, question_id: int) -> dict[str, Any]:
        rows = self.bank.list_answer_evidence(question_id)
        return {
            "question_id": question_id,
            "candidates": rows,
            "summary": {
                "candidate_count": len(rows),
                "correct_observations": sum(
                    1 for c in rows for o in c["observations"] if o["outcome"] == "correct"
                ),
                "incorrect_observations": sum(
                    1 for c in rows for o in c["observations"] if o["outcome"] == "incorrect"
                ),
            },
        }

    def save_manual_answer(self, question: Mapping[str, Any], answer: Any) -> int:
        question_id = int(question.get("id") or 0)
        content = question.get("content")
        if question_id < 1 or not isinstance(content, Mapping):
            raise ValueError("question row is incomplete")
        return self.answer_repository().record_candidate(
            question_id,
            canonical_answer(answer, content.get("options")),
            "manual",
            "correct",
            source_ref="local_operator",
        )

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

        updated = replace(draft, payload=dict(payload), updated_at=datetime.now(UTC).isoformat())
        self.drafts.save(updated)
        return updated

    def _formal_draft_invocation(
        self, draft: FormalDraft, *, assessment_mode: str
    ) -> tuple[Profile, dict[str, Any], list[dict[str, Any]] | None, dict[str, Any]]:
        if draft.status != "draft":
            raise ValueError("only a draft can be executed")
        if assessment_mode not in {"save", "submit"}:
            raise ValueError("assessment_mode must be save or submit")
        profile = self.profiles.get(draft.provider, draft.profile_id)
        task = draft.payload.get("task")
        if not isinstance(task, dict):
            raise ValueError("draft payload.task must be an object")
        answers = self._normalize_draft_answers(draft.payload.get("answers"))
        if self._is_formal_task(task) and not answers:
            raise ValueError("formal draft requires at least one reviewed answer")
        questions = draft.payload.get("questions")
        if isinstance(questions, list):
            question_ids = {
                str(question.get("remote_id"))
                for question in questions
                if isinstance(question, Mapping) and str(question.get("remote_id") or "").strip()
            }
            answer_ids = {str(row["remote_id"]) for row in answers or ()}
            unknown = sorted(answer_ids - question_ids)
            if unknown:
                raise ValueError(
                    "draft contains answers for unknown questions: " + ", ".join(unknown[:8])
                )
            missing = sorted(question_ids - answer_ids)
            if missing:
                raise ValueError(
                    "formal draft still has unanswered questions: " + ", ".join(missing[:8])
                )
        settings = draft.payload.get("settings")
        if settings is not None and not isinstance(settings, dict):
            raise ValueError("draft payload.settings must be an object")
        invocation_settings = dict(settings or {})
        invocation_settings["assessment_mode"] = assessment_mode
        return profile, task, answers, invocation_settings

    def save_draft_to_provider(self, draft: FormalDraft) -> ProviderOperationResult:
        if draft.provider != "chaoxing":
            raise ValueError("provider does not expose a verified save-only formal route")
        profile, task, answers, settings = self._formal_draft_invocation(
            draft, assessment_mode="save"
        )
        return self.run_task(profile, task, answers=answers, settings=settings)

    def submit_draft(self, draft: FormalDraft) -> ProviderOperationResult:
        profile, task, answers, settings = self._formal_draft_invocation(
            draft, assessment_mode="submit"
        )
        result = self.run_task(profile, task, answers=answers, settings=settings)
        self.drafts.set_status(draft, "submitted")
        return result

    @staticmethod
    def _normalize_draft_answers(value: Any) -> list[dict[str, Any]] | None:
        """Accept the editor's convenient map form and emit Worker answer rows."""
        if value is None:
            return None
        if isinstance(value, list):
            rows = value
        elif isinstance(value, Mapping):
            nested = value.get("rows", value.get("items", value.get("answers")))
            if isinstance(nested, list):
                rows = nested
            elif "remote_id" in value and "value" in value:
                rows = [value]
            else:
                rows = [{"remote_id": str(key), "value": answer} for key, answer in value.items()]
        else:
            raise ValueError("draft payload.answers must be an object or array")
        normalized: list[dict[str, Any]] = []
        seen_remote_ids: set[str] = set()
        for row in rows:
            if not isinstance(row, Mapping) or not str(row.get("remote_id") or "").strip():
                raise ValueError("each draft answer must contain a non-empty remote_id")
            remote_id = str(row["remote_id"])
            if remote_id in seen_remote_ids:
                raise ValueError(f"draft contains duplicate answer for question {remote_id}")
            seen_remote_ids.add(remote_id)
            answer = row.get("value")
            if answer is None or (isinstance(answer, str) and not answer.strip()):
                raise ValueError(
                    f"draft answer {row['remote_id']} must contain a non-empty value"
                )
            if isinstance(answer, (list, dict)) and not answer:
                raise ValueError(
                    f"draft answer {row['remote_id']} must contain a non-empty value"
                )
            normalized.append({"remote_id": remote_id, "value": answer})
        return normalized

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
