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
                    "fallback_reasoning_effort": "low",
                    "timeout_seconds": 8,
                    "retry_attempts": 0,
                },
                "untimed": {
                    "primary": "gpt_router",
                    "model": "gpt-5.6-terra",
                    "fallback": "domestic_backup",
                    "fallback_model": "deepseek-chat",
                    "reasoning_effort": "medium",
                    "fallback_reasoning_effort": "medium",
                    "timeout_seconds": 60,
                    "retry_attempts": 0,
                },
            },
            "gpt_only": {
                "timed": {
                    "primary": "gpt_site",
                    "model": "gpt-5.6-luna",
                    "reasoning_effort": "low",
                    "timeout_seconds": 10,
                    "retry_attempts": 2,
                },
                "untimed": {
                    "primary": "gpt_site",
                    "model": "gpt-5.6-sol",
                    "reasoning_effort": "xhigh",
                    "timeout_seconds": 180,
                    "retry_attempts": 2,
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
            raise ValueError(f"不支持的本地配置版本：{value.get('version')}")
        for key in ("ui", "notifications", "models", "providers"):
            if not isinstance(value.get(key), dict):
                raise ValueError(f"config.{key} 必须是 JSON 对象")
        self._validate_nested(value)
        return self._merge_defaults(value)

    @staticmethod
    def _validate_nested(value: dict[str, Any]) -> None:
        models = value.get("models", {})
        endpoints = models.get("endpoints", {})
        combinations = models.get("combinations", {})
        if not isinstance(endpoints, dict):
            raise ValueError("models.endpoints 必须是 JSON 对象")
        if not isinstance(combinations, dict):
            raise ValueError("models.combinations 必须是 JSON 对象")
        for name, endpoint in endpoints.items():
            if not isinstance(endpoint, dict):
                raise ValueError(f"AI 站点 {name} 必须是 JSON 对象")
        for name, combination in combinations.items():
            if not isinstance(combination, dict):
                raise ValueError(f"答案组合 {name} 必须是 JSON 对象")
            for route in ("timed", "untimed", "challenge"):
                if route in combination and not isinstance(combination[route], dict):
                    raise ValueError(f"答案组合 {name}.{route} 必须是 JSON 对象")
            conditions = combination.get("conditions")
            if conditions is not None and (
                not isinstance(conditions, list)
                or any(not isinstance(item, dict) for item in conditions)
            ):
                raise ValueError(f"答案组合 {name}.conditions 必须是 JSON 对象数组")
        if "default" in models and not isinstance(models["default"], str):
            raise ValueError("models.default 必须是字符串")
        providers = value.get("providers", {})
        for provider, settings in providers.items():
            if not isinstance(settings, dict):
                raise ValueError(f"平台配置 {provider} 必须是 JSON 对象")

    @staticmethod
    def _merge_defaults(value: dict[str, Any]) -> dict[str, Any]:
        # These two collections are user-managed.  Once a config file contains
        # them, its contents are authoritative so deleting a built-in endpoint
        # or combination is not undone by the default merge on the next load.
        defaults = deepcopy(DEFAULT_CONFIG)
        models = value.get("models")
        if isinstance(models, dict):
            for collection in ("combinations", "endpoints"):
                if isinstance(models.get(collection), dict):
                    defaults["models"][collection] = {}

        def merge(default: Any, current: Any) -> Any:
            if isinstance(default, dict) and isinstance(current, dict):
                return {key: merge(default.get(key), current[key]) for key in current} | {
                    key: deepcopy(child) for key, child in default.items() if key not in current
                }
            return deepcopy(current)

        return merge(defaults, value)

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
                raise ValueError(f"config.{key} 必须是 JSON 对象")
        self._validate_nested(candidate)
        atomic_write_json(self.path, candidate)
