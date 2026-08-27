from __future__ import annotations

import shutil
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from uuid import UUID, uuid4

from .atomic import atomic_write_json, read_json_object
from .constants import PROFILE_VERSION, PROVIDER_IDS
from .paths import DataPaths


def _now() -> str:
    return datetime.now(UTC).isoformat()


def _provider(value: str) -> str:
    if value not in PROVIDER_IDS:
        raise ValueError(f"unsupported provider id: {value}")
    return value


def _profile_id(value: str) -> str:
    return str(UUID(value))


@dataclass(frozen=True)
class Profile:
    id: str
    provider: str
    label: str
    credentials: dict[str, Any] = field(default_factory=dict)
    settings: dict[str, Any] = field(default_factory=dict)
    enabled: bool = True
    created_at: str = field(default_factory=_now)
    updated_at: str = field(default_factory=_now)
    version: int = PROFILE_VERSION

    @classmethod
    def create(cls, provider: str, label: str) -> Profile:
        if not label.strip():
            raise ValueError("profile label must not be empty")
        return cls(id=str(uuid4()), provider=_provider(provider), label=label.strip())

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> Profile:
        version = int(value.get("version", 0))
        if version != PROFILE_VERSION:
            raise ValueError(f"unsupported profile version: {version}")
        credentials = value.get("credentials", {})
        settings = value.get("settings", {})
        if not isinstance(credentials, dict) or not isinstance(settings, dict):
            raise ValueError("credentials and settings must be JSON objects")
        enabled = value.get("enabled", True)
        if not isinstance(enabled, bool):
            raise ValueError("profile.enabled must be a boolean")
        label = str(value["label"]).strip()
        if not label:
            raise ValueError("profile label must not be empty")
        return cls(
            id=_profile_id(str(value["id"])),
            provider=_provider(str(value["provider"])),
            label=label,
            credentials=dict(credentials),
            settings=dict(settings),
            enabled=enabled,
            created_at=str(value["created_at"]),
            updated_at=str(value["updated_at"]),
            version=version,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "id": self.id,
            "provider": self.provider,
            "label": self.label,
            "enabled": self.enabled,
            "credentials": self.credentials,
            "settings": self.settings,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }


class ProfileStore:
    def __init__(self, paths: DataPaths) -> None:
        self.paths = paths

    def _directory(self, provider: str) -> Path:
        return self.paths.accounts / _provider(provider)

    def path_for(self, provider: str, profile_id: str) -> Path:
        return self._directory(provider) / f"{_profile_id(profile_id)}.json"

    def create(self, provider: str, label: str) -> Profile:
        profile = Profile.create(provider, label)
        self.save(profile)
        return profile

    def save(self, profile: Profile) -> None:
        normalized = Profile.from_dict(profile.to_dict())
        atomic_write_json(self.path_for(normalized.provider, normalized.id), normalized.to_dict())

    def get(self, provider: str, profile_id: str) -> Profile:
        return Profile.from_dict(read_json_object(self.path_for(provider, profile_id)))

    def list(self, provider: str | None = None) -> list[Profile]:
        providers = (_provider(provider),) if provider else PROVIDER_IDS
        result: list[Profile] = []
        for provider_id in providers:
            directory = self._directory(provider_id)
            if not directory.exists():
                continue
            for path in sorted(directory.glob("*.json")):
                result.append(Profile.from_dict(read_json_object(path)))
        return sorted(result, key=lambda item: (item.provider, item.label.casefold(), item.id))

    def delete(self, provider: str, profile_id: str) -> None:
        self.path_for(provider, profile_id).unlink(missing_ok=False)


class ProfileStateStore:
    def __init__(self, paths: DataPaths) -> None:
        self.paths = paths

    def _path(self, profile: Profile, name: str) -> Path:
        if not name or any(
            character not in "abcdefghijklmnopqrstuvwxyz0123456789-_" for character in name
        ):
            raise ValueError("state name must use lowercase ASCII letters, digits, '-' or '_'")
        return self.paths.state / profile.provider / profile.id / f"{name}.json"

    def load(self, profile: Profile, name: str) -> dict[str, Any] | None:
        path = self._path(profile, name)
        return read_json_object(path) if path.exists() else None

    def save(self, profile: Profile, name: str, value: dict[str, Any]) -> None:
        atomic_write_json(self._path(profile, name), value)

    def delete_profile(self, profile: Profile) -> None:
        """Remove generated session/state for a profile, never the account file."""
        target = (self.paths.state / profile.provider / profile.id).resolve()
        expected_parent = (self.paths.state / profile.provider).resolve()
        if target.parent != expected_parent:
            raise ValueError("refusing to remove state outside the profile directory")
        if target.exists():
            shutil.rmtree(target)
