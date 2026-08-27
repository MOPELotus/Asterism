from __future__ import annotations

import threading
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any

from .answers import AnswerRepository
from .inventory import InventoryStore
from .profiles import Profile, ProfileStateStore
from .runner import RunnerError
from .service import ProviderService


def _now() -> str:
    return datetime.now(UTC).isoformat()


@dataclass
class ScanStatus:
    provider: str
    profile_id: str
    state: str = "idle"
    phase: str = ""
    course_count: int = 0
    task_count: int = 0
    question_count: int = 0
    completed_tasks: int = 0
    retries: int = 0
    cursor: str = ""
    completed_task_refs: list[str] = field(default_factory=list)
    last_error: str = ""
    updated_at: str = field(default_factory=_now)

    @classmethod
    def from_value(cls, value: dict[str, Any], profile: Profile) -> ScanStatus:
        def nonnegative_int(name: str) -> int:
            candidate = value.get(name, 0)
            if isinstance(candidate, bool):
                return 0
            try:
                parsed = int(candidate or 0)
            except (TypeError, ValueError):
                return 0
            return max(0, parsed)

        raw_refs = value.get("completed_task_refs", [])
        refs = raw_refs if isinstance(raw_refs, list) else []
        return cls(
            provider=profile.provider,
            profile_id=profile.id,
            state=str(value.get("state") or "idle"),
            phase=str(value.get("phase") or ""),
            course_count=nonnegative_int("course_count"),
            task_count=nonnegative_int("task_count"),
            question_count=nonnegative_int("question_count"),
            completed_tasks=nonnegative_int("completed_tasks"),
            retries=nonnegative_int("retries"),
            cursor=str(value.get("cursor") or ""),
            completed_task_refs=[str(item) for item in refs if str(item)],
            last_error=str(value.get("last_error") or ""),
            updated_at=str(value.get("updated_at") or _now()),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "provider": self.provider,
            "profile_id": self.profile_id,
            "state": self.state,
            "phase": self.phase,
            "course_count": self.course_count,
            "task_count": self.task_count,
            "question_count": self.question_count,
            "completed_tasks": self.completed_tasks,
            "retries": self.retries,
            "cursor": self.cursor,
            "completed_task_refs": self.completed_task_refs[-10000:],
            "last_error": self.last_error,
            "updated_at": self.updated_at,
        }


class ReadOnlyScanCoordinator:
    """Conservative, resumable inventory/question scan; never submits answers."""

    def __init__(
        self,
        service: ProviderService,
        states: ProfileStateStore,
        inventory: InventoryStore,
        answers: AnswerRepository,
    ) -> None:
        self.service = service
        self.states = states
        self.inventory = inventory
        self.answers = answers

    def status(self, profile: Profile) -> ScanStatus:
        value = self.states.load(profile, "scan")
        return ScanStatus.from_value(value or {}, profile)

    def scan(
        self,
        profile: Profile,
        *,
        max_retries: int = 3,
        allow_cidaren_attempt: bool = False,
        cancel: threading.Event | None = None,
        on_update: Callable[[ScanStatus], None] | None = None,
    ) -> ScanStatus:
        if profile.provider != "chaoxing":
            raise ValueError("background full scan is available only for chaoxing")
        if max_retries < 0 or max_retries > 8:
            raise ValueError("max_retries must be between 0 and 8")
        cancellation = cancel or threading.Event()
        status = self.status(profile)
        status.state = "running"
        status.phase = "courses"
        status.last_error = ""
        status.course_count = 0
        status.task_count = 0
        status.question_count = 0
        status.completed_tasks = 0
        self._save(status, on_update)
        try:
            courses = self._retry(
                lambda: self.service.courses(profile, cancel=cancellation),
                status,
                max_retries,
                cancellation,
                on_update,
            ).data.get("courses", [])
            if not isinstance(courses, list):
                raise RunnerError("protocol_invalid", "courses result must contain an object list")
            status.course_count = len(courses)
            self._save(status, on_update)
            for course in courses:
                self._check_cancel(cancellation)
                if not isinstance(course, dict):
                    continue
                course_id = str(course.get("remote_id") or "")
                if not course_id:
                    continue
                status.phase = f"tasks:{course_id}"
                self._save(status, on_update)
                tasks = self._retry(
                    lambda course=course: self.service.tasks(profile, course, cancel=cancellation),
                    status,
                    max_retries,
                    cancellation,
                    on_update,
                ).data.get("tasks", [])
                if not isinstance(tasks, list):
                    raise RunnerError(
                        "protocol_invalid", "tasks result must contain an object list"
                    )
                status.task_count += len(tasks)
                self._save(status, on_update)
                for task in tasks:
                    self._check_cancel(cancellation)
                    if not isinstance(task, dict):
                        continue
                    task_id = str(task.get("remote_id") or "")
                    if not task_id:
                        continue
                    # Provider task IDs are not guaranteed to be globally
                    # unique. Scope the resume key by course so a task with
                    # the same remote ID in another course is still scanned.
                    task_ref = self._task_ref(course_id, task_id)
                    legacy_ref = (
                        task_id in status.completed_task_refs
                        and not any(
                            item.endswith(f"::{task_id}")
                            for item in status.completed_task_refs
                        )
                    )
                    if task_ref in status.completed_task_refs or legacy_ref:
                        if legacy_ref:
                            status.completed_task_refs.remove(task_id)
                            status.completed_task_refs.append(task_ref)
                        status.completed_tasks += 1
                        status.cursor = task_ref
                        self._save(status, on_update)
                        continue
                    status.phase = f"questions:{task_ref}"
                    self._save(status, on_update)
                    try:
                        result = self._retry(
                            lambda task=task: self.service.questions(
                                profile,
                                task,
                                allow_read_that_starts_attempt=allow_cidaren_attempt,
                                cancel=cancellation,
                            ),
                            status,
                            max_retries,
                            cancellation,
                            on_update,
                        )
                    except RunnerError as error:
                        if error.code == "explicit_read_authorization_required":
                            status.last_error = f"{task_ref}: read authorization required"
                            status.cursor = task_ref
                            self._save(status, on_update)
                            continue
                        raise
                    questions = result.data.get("questions", [])
                    if isinstance(questions, list):
                        for question in questions:
                            if isinstance(question, dict):
                                self.answers.ingest_question(profile.provider, question)
                        status.question_count += len(questions)
                    status.completed_tasks += 1
                    status.cursor = task_ref
                    if task_ref not in status.completed_task_refs:
                        status.completed_task_refs.append(task_ref)
                    status.last_error = ""
                    self._save(status, on_update)
            status.state = "completed"
            status.phase = "done"
        except RunnerError as error:
            status.state = "cancelled" if error.code == "cancelled" else "failed"
            status.last_error = f"{error.code}: scan operation failed"
            raise
        except Exception as error:
            status.state = "failed"
            status.last_error = f"{type(error).__name__}: scan operation failed"
            raise
        finally:
            status.updated_at = _now()
            self._save(status, on_update)
        return status

    def _retry(
        self,
        operation: Callable[[], Any],
        status: ScanStatus,
        max_retries: int,
        cancel: threading.Event,
        on_update: Callable[[ScanStatus], None] | None,
    ) -> Any:
        last_error: RunnerError | None = None
        for attempt in range(max_retries + 1):
            self._check_cancel(cancel)
            try:
                return operation()
            except RunnerError as error:
                last_error = error
                if error.code == "cancelled":
                    raise
                if attempt >= max_retries:
                    raise
                status.last_error = f"{error.code}: scan operation failed"
                status.retries += 1
                self._save(status, on_update)
                cancel.wait(min(2**attempt, 30))
            except (OSError, RuntimeError, ValueError) as error:
                last_error = RunnerError(
                    "retryable_error", f"{type(error).__name__}: scan operation failed"
                )
                if attempt >= max_retries:
                    raise last_error from error
                status.last_error = str(last_error)
                status.retries += 1
                self._save(status, on_update)
                cancel.wait(min(2**attempt, 30))
        raise last_error or AssertionError("unreachable")

    @staticmethod
    def _task_ref(course_id: str, task_id: str) -> str:
        return f"{course_id}::{task_id}"

    @staticmethod
    def _check_cancel(cancel: threading.Event) -> None:
        if cancel.is_set():
            raise RunnerError("cancelled", "read-only scan was cancelled")

    def _save(self, status: ScanStatus, on_update: Callable[[ScanStatus], None] | None) -> None:
        status.updated_at = _now()
        profile = Profile(
            id=status.profile_id,
            provider=status.provider,
            label="scan",
        )
        self.states.save(profile, "scan", status.to_dict())
        if on_update is not None:
            on_update(status)
