from __future__ import annotations

from copy import deepcopy
from pathlib import Path
from typing import Any

from .atomic import atomic_write_json, read_json_object

CONFIG_VERSION = 1
DEFAULT_CONFIG: dict[str, Any] = {
    "version": CONFIG_VERSION,
    "onboarding_completed": False,
    "ui": {"theme": "system", "language": "zh-CN"},
    "notifications": {"enabled": False, "command": ""},
    "models": {
        "combinations": {
            "economy": {
                "timed": {
                    "primary": "gpt_router",
                    "model": "gpt-5.6-luna",
                    "fallback": "domestic_backup",
                    "fallback_model": "deepseek-chat",
                    "reasoning_effort": "low",
                },
                "untimed": {
                    "primary": "gpt_router",
                    "model": "gpt-5.6-terra",
                    "fallback": "domestic_backup",
                    "fallback_model": "deepseek-chat",
                    "reasoning_effort": "medium",
                },
            },
            "gpt_only": {
                "timed": {
                    "primary": "gpt_site",
                    "model": "gpt-5.6-luna",
                    "reasoning_effort": "low",
                },
                "untimed": {
                    "primary": "gpt_site",
                    "model": "gpt-5.6-sol",
                    "reasoning_effort": "xhigh",
                },
            },
        },
        "endpoints": {
            "gpt_router": {
                "base_url": "",
                "protocol": "responses",
                "api_key_env": "ASTERISM_GPT_ROUTER_API_KEY",
            },
            "gpt_site": {
                "base_url": "",
                "protocol": "responses",
                "api_key_env": "ASTERISM_GPT_SITE_API_KEY",
            },
            "domestic_backup": {
                "base_url": "",
                "protocol": "responses",
                "model": "deepseek-chat",
                "api_key_env": "ASTERISM_DOMESTIC_AI_API_KEY",
            },
        },
        "default": "economy",
        "gpt_only": "gpt_only",
    },
    "providers": {
        "chaoxing": {
            "speed": 2.0,
            "verification_attempt_budget": 3,
            "verification_time_budget_seconds": 90,
            "challenge_retry_attempts": 3,
            "challenge_escalation_route": "sol_xhigh",
            "minimum_answer_coverage": 0.9,
        },
        "welearn": {"correctness": 100, "duration_seconds": 0},
        "uai": {"duration_seconds": 60, "cooldown_count": 5, "cooldown_seconds": 120},
        "cidaren": {
            "instant_timeout_seconds": 8,
            "instant_fallback_grace_seconds": 2,
            "spend_min_time": 1,
            "spend_max_time": 2,
        },
    },
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
        return self._merge_defaults(value)

    @staticmethod
    def _merge_defaults(value: dict[str, Any]) -> dict[str, Any]:
        def merge(default: Any, current: Any) -> Any:
            if isinstance(default, dict) and isinstance(current, dict):
                return {key: merge(default.get(key), current[key]) for key in current} | {
                    key: deepcopy(child) for key, child in default.items() if key not in current
                }
            return deepcopy(current)

        return merge(DEFAULT_CONFIG, value)

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
