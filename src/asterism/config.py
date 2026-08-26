from __future__ import annotations

from copy import deepcopy
from pathlib import Path
from typing import Any

from .atomic import atomic_write_json, read_json_object

CONFIG_VERSION = 1
DEFAULT_CONFIG: dict[str, Any] = {
    "version": CONFIG_VERSION,
    "ui": {"theme": "system", "language": "zh-CN"},
    "notifications": {"enabled": False, "command": ""},
    "models": {"combinations": {}, "default": None, "gpt_only": None},
    "providers": {},
}


class LocalConfigStore:
    def __init__(self, path: Path) -> None:
        self.path = path

    def load(self) -> dict[str, Any]:
        if not self.path.exists():
            return deepcopy(DEFAULT_CONFIG)
        value = read_json_object(self.path)
        if int(value.get("version", 0)) != CONFIG_VERSION:
            raise ValueError(f"unsupported local config version: {value.get('version')}")
        for key in ("ui", "notifications", "models", "providers"):
            if not isinstance(value.get(key), dict):
                raise ValueError(f"config.{key} must be an object")
        return value

    def ensure(self) -> dict[str, Any]:
        value = self.load()
        if not self.path.exists():
            self.save(value)
        return value

    def save(self, value: dict[str, Any]) -> None:
        candidate = deepcopy(value)
        candidate["version"] = CONFIG_VERSION
        for key in ("ui", "notifications", "models", "providers"):
            if not isinstance(candidate.get(key), dict):
                raise ValueError(f"config.{key} must be an object")
        atomic_write_json(self.path, candidate)
