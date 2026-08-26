from __future__ import annotations

import json
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from uuid import UUID, uuid4

from .atomic import atomic_write_json, read_json_object
from .constants import PROVIDER_IDS
from .database import QuestionBank
from .paths import DataPaths
from .profiles import Profile


def _now() -> str:
    return datetime.now(UTC).isoformat()


@dataclass(frozen=True)
class FormalDraft:
    id: str
    provider: str
    profile_id: str
    task_ref: str
    payload: dict[str, Any]
    status: str = "draft"
    created_at: str = field(default_factory=_now)
    updated_at: str = field(default_factory=_now)
    submitted_at: str | None = None
    version: int = 1

    @classmethod
    def create(cls, profile: Profile, task_ref: str, payload: dict[str, Any]) -> FormalDraft:
        if not task_ref.strip():
            raise ValueError("task_ref must not be empty")
        return cls(str(uuid4()), profile.provider, profile.id, task_ref.strip(), dict(payload))

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> FormalDraft:
        provider = str(value["provider"])
        status = str(value["status"])
        if provider not in PROVIDER_IDS:
            raise ValueError(f"unsupported provider id: {provider}")
        if status not in {"draft", "submitted", "discarded"}:
            raise ValueError(f"unsupported draft status: {status}")
        payload = value.get("payload")
        if not isinstance(payload, dict):
            raise ValueError("draft payload must be an object")
        return cls(
            id=str(UUID(str(value["id"]))),
            provider=provider,
            profile_id=str(UUID(str(value["profile_id"]))),
            task_ref=str(value["task_ref"]),
            payload=dict(payload),
            status=status,
            created_at=str(value["created_at"]),
            updated_at=str(value["updated_at"]),
            submitted_at=str(value["submitted_at"]) if value.get("submitted_at") else None,
            version=int(value.get("version", 0)),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "id": self.id,
            "provider": self.provider,
            "profile_id": self.profile_id,
            "task_ref": self.task_ref,
            "status": self.status,
            "payload": self.payload,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "submitted_at": self.submitted_at,
        }


class DraftRepository:
    """Keep an editable JSON draft and its queryable SQLite index in sync."""

    def __init__(self, paths: DataPaths, database: QuestionBank) -> None:
        self.paths = paths
        self.database = database

    def path_for(self, draft: FormalDraft) -> Path:
        return self.paths.drafts / draft.provider / draft.profile_id / f"{draft.id}.json"

    def create(self, profile: Profile, task_ref: str, payload: dict[str, Any]) -> FormalDraft:
        draft = FormalDraft.create(profile, task_ref, payload)
        self.save(draft)
        return draft

    def save(self, draft: FormalDraft) -> None:
        value = FormalDraft.from_dict(draft.to_dict())
        if value.version != 1:
            raise ValueError(f"unsupported draft version: {value.version}")
        atomic_write_json(self.path_for(value), value.to_dict())
        encoded = json.dumps(
            value.payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        )
        with self.database.connect() as connection:
            connection.execute(
                """INSERT INTO formal_drafts(
                       id, provider, profile_id, task_ref, status, payload_json,
                       created_at, updated_at, submitted_at
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(id) DO UPDATE SET
                       status=excluded.status,
                       payload_json=excluded.payload_json,
                       updated_at=excluded.updated_at,
                       submitted_at=excluded.submitted_at""",
                (
                    value.id,
                    value.provider,
                    value.profile_id,
                    value.task_ref,
                    value.status,
                    encoded,
                    value.created_at,
                    value.updated_at,
                    value.submitted_at,
                ),
            )

    def get(self, provider: str, profile_id: str, draft_id: str) -> FormalDraft:
        if provider not in PROVIDER_IDS:
            raise ValueError(f"unsupported provider id: {provider}")
        placeholder = FormalDraft(
            id=str(UUID(draft_id)),
            provider=provider,
            profile_id=str(UUID(profile_id)),
            task_ref="placeholder",
            payload={},
        )
        return FormalDraft.from_dict(read_json_object(self.path_for(placeholder)))

    def list(self, provider: str | None = None) -> list[FormalDraft]:
        providers = (provider,) if provider else PROVIDER_IDS
        drafts: list[FormalDraft] = []
        for provider_id in providers:
            if provider_id not in PROVIDER_IDS:
                raise ValueError(f"unsupported provider id: {provider_id}")
            root = self.paths.drafts / provider_id
            for path in root.glob("*/*.json") if root.exists() else ():
                try:
                    drafts.append(FormalDraft.from_dict(read_json_object(path)))
                except (OSError, TypeError, ValueError, KeyError):
                    continue
        return sorted(drafts, key=lambda item: item.updated_at, reverse=True)

    def set_status(self, draft: FormalDraft, status: str) -> FormalDraft:
        if status not in {"draft", "submitted", "discarded"}:
            raise ValueError(f"unsupported draft status: {status}")
        now = _now()
        updated = replace(
            draft,
            status=status,
            updated_at=now,
            submitted_at=now if status == "submitted" else draft.submitted_at,
        )
        self.save(updated)
        return updated
