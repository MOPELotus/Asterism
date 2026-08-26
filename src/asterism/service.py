from __future__ import annotations

import threading
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from .inventory import InventoryStore
from .profiles import Profile, ProfileStateStore
from .providers import ProviderRegistry
from .runner import RunnerError, RunnerManager, RunResult

AUTHENTICATION_ERRORS = {"authentication_failed", "session_invalid"}


@dataclass(frozen=True)
class ProviderOperationResult:
    operation: str
    data: dict[str, Any]
    run: RunResult


class ProviderService:
    """Local-operator facade over the restored Provider workers."""

    def __init__(
        self,
        registry: ProviderRegistry,
        runner: RunnerManager,
        states: ProfileStateStore,
        inventory: InventoryStore,
    ) -> None:
        self.registry = registry
        self.runner = runner
        self.states = states
        self.inventory = inventory

    def _invoke(
        self,
        profile: Profile,
        operation: str,
        payload: dict[str, Any] | None = None,
        *,
        timeout: float = 120,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
        retry_authentication: bool = True,
    ) -> ProviderOperationResult:
        spec = self.registry.get(profile.provider)
        try:
            run = self.runner.invoke(
                spec,
                operation,
                payload,
                timeout=timeout,
                cancel=cancel,
                on_event=on_event,
                profile=profile,
            )
        except RunnerError as error:
            if (
                retry_authentication
                and operation != "authenticate"
                and error.code in AUTHENTICATION_ERRORS
                and profile.credentials
            ):
                self.authenticate(profile, cancel=cancel, on_event=on_event)
                run = self.runner.invoke(
                    spec,
                    operation,
                    payload,
                    timeout=timeout,
                    cancel=cancel,
                    on_event=on_event,
                    profile=profile,
                )
            else:
                raise
        return ProviderOperationResult(operation, run.data, run)

    def authenticate(
        self,
        profile: Profile,
        *,
        cancel: threading.Event | None = None,
        on_event: Callable[[dict[str, Any]], None] | None = None,
    ) -> ProviderOperationResult:
        return self._invoke(
            profile,
            "authenticate",
            retry_authentication=False,
            cancel=cancel,
            on_event=on_event,
        )

    def oauth_begin(self, profile: Profile) -> ProviderOperationResult:
        if profile.provider != "cidaren":
            raise ValueError("oauth_begin is only available for cidaren")
        result = self._invoke(profile, "oauth_begin", retry_authentication=False)
        binding = {
            key: result.data[key] for key in ("state_digest", "marker_digest") if key in result.data
        }
        if len(binding) != 2:
            raise RunnerError("protocol_invalid", "cidaren OAuth binding is incomplete")
        self.states.save(profile, "oauth-binding", binding)
        return result

    def oauth_exchange(self, profile: Profile, callback_url: str) -> ProviderOperationResult:
        if profile.provider != "cidaren":
            raise ValueError("oauth_exchange is only available for cidaren")
        binding = self.states.load(profile, "oauth-binding")
        if binding is None:
            raise ValueError("cidaren OAuth has not been started for this profile")
        return self._invoke(
            profile,
            "oauth_exchange",
            {"callback_url": callback_url, "binding": binding},
            retry_authentication=False,
        )

    def courses(
        self,
        profile: Profile,
        *,
        cancel: threading.Event | None = None,
    ) -> ProviderOperationResult:
        result = self._invoke(profile, "courses", cancel=cancel)
        courses = result.data.get("courses")
        if not isinstance(courses, list) or not all(isinstance(item, dict) for item in courses):
            raise RunnerError("protocol_invalid", "courses result must contain an object list")
        self.inventory.save_courses(profile, [dict(item) for item in courses])
        return result

    def tasks(
        self,
        profile: Profile,
        course: dict[str, Any],
        *,
        cancel: threading.Event | None = None,
    ) -> ProviderOperationResult:
        result = self._invoke(profile, "tasks", {"course": course}, cancel=cancel)
        tasks = result.data.get("tasks")
        if not isinstance(tasks, list) or not all(isinstance(item, dict) for item in tasks):
            raise RunnerError("protocol_invalid", "tasks result must contain an object list")
        remote_id = str(course.get("remote_id") or "")
        if not remote_id:
            raise ValueError("course.remote_id must not be empty")
        self.inventory.save_tasks(profile, remote_id, [dict(item) for item in tasks])
        return result

    def questions(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        allow_read_that_starts_attempt: bool = False,
        cancel: threading.Event | None = None,
    ) -> ProviderOperationResult:
        payload: dict[str, Any] = {"task": task}
        if allow_read_that_starts_attempt:
            payload["allow_read_that_starts_attempt"] = True
        return self._invoke(profile, "questions", payload, cancel=cancel)

    def inspect(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        cancel: threading.Event | None = None,
    ) -> ProviderOperationResult:
        if profile.provider != "uai":
            raise ValueError("inspect is currently exposed only for uai")
        return self._invoke(profile, "inspect", {"task": task}, cancel=cancel)

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
        payload: dict[str, Any] = {"task": task}
        if answers is not None:
            payload["answers"] = answers
        if settings is not None:
            payload["settings"] = settings
        return self._invoke(profile, "run", payload, cancel=cancel, on_event=on_event, timeout=3600)

    def read_duration(
        self,
        profile: Profile,
        task: dict[str, Any],
        *,
        cancel: threading.Event | None = None,
    ) -> ProviderOperationResult:
        if profile.provider == "uai":
            return self._invoke(profile, "duration", {"task": task}, cancel=cancel)
        if profile.provider == "welearn":
            return self._invoke(profile, "duration", {"task": task}, cancel=cancel)
        raise ValueError(f"duration read is not exposed for {profile.provider}")
