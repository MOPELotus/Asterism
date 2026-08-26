from __future__ import annotations

import hashlib
from datetime import UTC, datetime
from typing import Any

from .profiles import Profile, ProfileStateStore


def _now() -> str:
    return datetime.now(UTC).isoformat()


def _object_list(value: Any, name: str) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise ValueError(f"{name} must be a list of objects")
    return [dict(item) for item in value]


class InventoryStore:
    def __init__(self, states: ProfileStateStore) -> None:
        self.states = states

    @staticmethod
    def _scope(prefix: str, remote_id: str) -> str:
        digest = hashlib.sha256(remote_id.encode("utf-8")).hexdigest()[:24]
        return f"{prefix}-{digest}"

    def save_courses(self, profile: Profile, courses: list[dict[str, Any]]) -> None:
        self.states.save(profile, "courses", {"updated_at": _now(), "items": courses})

    def load_courses(self, profile: Profile) -> list[dict[str, Any]]:
        value = self.states.load(profile, "courses")
        return _object_list(value.get("items", []), "courses") if value else []

    def save_tasks(
        self, profile: Profile, course_remote_id: str, tasks: list[dict[str, Any]]
    ) -> None:
        self.states.save(
            profile,
            self._scope("tasks", course_remote_id),
            {"updated_at": _now(), "course_remote_id": course_remote_id, "items": tasks},
        )

    def load_tasks(self, profile: Profile, course_remote_id: str) -> list[dict[str, Any]]:
        value = self.states.load(profile, self._scope("tasks", course_remote_id))
        return _object_list(value.get("items", []), "tasks") if value else []

    def save_scan_checkpoint(self, profile: Profile, value: dict[str, Any]) -> None:
        self.states.save(profile, "question-scan", value)

    def load_scan_checkpoint(self, profile: Profile) -> dict[str, Any] | None:
        return self.states.load(profile, "question-scan")
