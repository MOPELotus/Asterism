from __future__ import annotations

import hashlib
import json
import os
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

import httpx

from .answers import AnswerRepository, question_identity
from .config import LocalConfigStore
from .database import QuestionBank


@dataclass(frozen=True)
class ModelEndpoint:
    name: str
    base_url: str
    protocol: str = "responses"
    api_key_env: str = ""
    api_key: str = ""

    @classmethod
    def from_value(cls, name: str, value: Mapping[str, Any]) -> ModelEndpoint:
        base_url = str(value.get("base_url") or "").strip().rstrip("/")
        protocol = str(value.get("protocol") or "responses").strip().lower()
        if protocol not in {"responses", "chat_completions"}:
            raise ValueError("AI endpoint protocol must be responses or chat_completions")
        return cls(
            name=name,
            base_url=base_url,
            protocol=protocol,
            api_key_env=str(value.get("api_key_env") or ""),
            api_key=str(value.get("api_key") or ""),
        )

    def resolved_key(self) -> str:
        return self.api_key or (os.environ.get(self.api_key_env, "") if self.api_key_env else "")


@dataclass(frozen=True)
class ModelChoice:
    endpoint: ModelEndpoint
    model: str
    reasoning_effort: str = "medium"
    fallback: ModelChoice | None = None


class AIAnswerService:
    """OpenAI-compatible Responses client with deployment-local answer caching."""

    def __init__(self, config: LocalConfigStore, bank: QuestionBank) -> None:
        self.config = config
        self.bank = bank
        self.answers = AnswerRepository(bank)

    def choose(self, combination: str | None = None, route: str = "untimed") -> ModelChoice:
        value = self.config.ensure()
        models = value.get("models", {})
        combination_name = combination or str(models.get("default") or "economy")
        combinations = models.get("combinations", {})
        selected = combinations.get(combination_name)
        if not isinstance(selected, Mapping):
            raise ValueError(f"unknown model combination: {combination_name}")
        route_value = selected.get(route)
        if not isinstance(route_value, Mapping):
            raise ValueError(f"model combination {combination_name} has no route {route}")
        endpoints = models.get("endpoints", {})
        primary = self._choice_from_route(route_value, endpoints, "primary")
        fallback_name = str(route_value.get("fallback") or "").strip()
        if fallback_name:
            try:
                fallback = self._choice_from_route(
                    {**route_value, "primary": fallback_name}, endpoints, "primary"
                )
                primary = ModelChoice(
                    primary.endpoint, primary.model, primary.reasoning_effort, fallback
                )
            except ValueError:
                # A disaster endpoint is optional; an empty/malformed fallback
                # must not prevent the configured primary from running.
                pass
        return primary

    @staticmethod
    def _choice_from_route(
        route_value: Mapping[str, Any], endpoints: Mapping[str, Any], key: str
    ) -> ModelChoice:
        endpoint_name = str(route_value.get(key) or "")
        endpoint_value = endpoints.get(endpoint_name)
        if not isinstance(endpoint_value, Mapping):
            raise ValueError(f"model endpoint is not configured: {endpoint_name}")
        endpoint = ModelEndpoint.from_value(endpoint_name, endpoint_value)
        model = str(route_value.get("model") or endpoint_value.get("model") or "")
        if not model:
            raise ValueError(f"model is not configured for endpoint: {endpoint_name}")
        return ModelChoice(endpoint, model, str(route_value.get("reasoning_effort") or "medium"))

    def answer(
        self,
        provider: str,
        question: dict[str, Any],
        *,
        combination: str | None = None,
        route: str = "untimed",
        force_refresh: bool = False,
        timeout: float = 45,
    ) -> dict[str, Any]:
        combination_name = combination or str(
            self.config.ensure().get("models", {}).get("default") or "economy"
        )
        choice = self.choose(combination_name, route)
        identity_hash, _ = question_identity(provider, question)
        if not force_refresh:
            exact = self.answers.resolve_exact(provider, identity_hash)
            if exact.status == "exact":
                return {
                    "answer": {"answer": exact.answer, "confidence": 1.0},
                    "source": "local_cache",
                    "cached": True,
                    "usage": {},
                }
        cache_key = self.cache_key(provider, identity_hash, combination_name, route, choice)
        if not force_refresh:
            cached = self.bank.get_ai_cache(cache_key)
            if cached is not None:
                return {"answer": cached["response"], "cached": True, "usage": cached["usage"]}
        if not choice.endpoint.base_url:
            raise RuntimeError(f"AI endpoint {choice.endpoint.name} has no base_url configured")
        key = choice.endpoint.resolved_key()
        if not key:
            raise RuntimeError(f"AI endpoint {choice.endpoint.name} has no API key")
        try:
            parsed, usage = self._request(question, choice, key, timeout)
        except (httpx.HTTPError, RuntimeError):
            if choice.fallback is None:
                raise
            fallback = choice.fallback
            fallback_key = fallback.endpoint.resolved_key()
            if not fallback.endpoint.base_url or not fallback_key:
                raise
            parsed, usage = self._request(question, fallback, fallback_key, timeout)
        self.bank.put_ai_cache(cache_key, choice.endpoint.name + ":" + choice.model, parsed, usage)
        question_id = self.bank.question_id(provider, identity_hash)
        if question_id is not None:
            self.answers.record_candidate(
                question_id, parsed.get("answer"), "ai", "unverified", source_ref=choice.model
            )
        return {"answer": parsed, "cached": False, "usage": usage}

    def _request(
        self, question: dict[str, Any], choice: ModelChoice, key: str, timeout: float
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        request = self.build_request(question, choice)
        headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
        with httpx.Client(timeout=timeout) as client:
            response = client.post(self._url(choice.endpoint), headers=headers, json=request)
            response.raise_for_status()
            body = response.json()
        parsed = self.parse_response(body, choice.endpoint.protocol)
        usage = body.get("usage") if isinstance(body.get("usage"), dict) else {}
        return parsed, usage

    @staticmethod
    def cache_key(
        provider: str,
        identity_hash: str,
        combination: str,
        route: str,
        choice: ModelChoice,
    ) -> str:
        raw = "\0".join(
            (
                provider,
                identity_hash,
                combination,
                route,
                choice.endpoint.name,
                choice.model,
                choice.reasoning_effort,
            )
        )
        return hashlib.sha256(raw.encode("utf-8")).hexdigest()

    @staticmethod
    def _url(endpoint: ModelEndpoint) -> str:
        suffix = "/v1/responses" if endpoint.protocol == "responses" else "/v1/chat/completions"
        return (
            endpoint.base_url if endpoint.base_url.endswith(suffix) else endpoint.base_url + suffix
        )

    @classmethod
    def build_request(cls, question: dict[str, Any], choice: ModelChoice) -> dict[str, Any]:
        instruction = (
            "You answer an educational question. Return only one JSON object with keys "
            "answer and confidence. answer must be the exact content needed by the question, "
            "not an explanation. Preserve blank order, matching pairs, ordering and rich-media "
            "references. If uncertain, set confidence to 0 and answer to null."
        )
        content = [
            {"type": "input_text", "text": json.dumps(question, ensure_ascii=False, sort_keys=True)}
        ]
        for item in cls._media_blocks(question):
            content.append(item)
        if choice.endpoint.protocol == "responses":
            return {
                "model": choice.model,
                "instructions": instruction,
                "input": [{"role": "user", "content": content}],
                "reasoning": {"effort": choice.reasoning_effort},
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "answer_candidate",
                        "strict": True,
                        "schema": {
                            "type": "object",
                            "properties": {
                                "answer": {},
                                "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                            },
                            "required": ["answer", "confidence"],
                            "additionalProperties": False,
                        },
                    }
                },
                "store": False,
            }
        return {
            "model": choice.model,
            "messages": [
                {"role": "system", "content": instruction},
                {"role": "user", "content": cls._chat_content(question)},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "answer_candidate",
                    "strict": True,
                    "schema": {
                        "type": "object",
                        "properties": {"answer": {}, "confidence": {"type": "number"}},
                        "required": ["answer", "confidence"],
                        "additionalProperties": False,
                    },
                },
            },
            "temperature": 0,
        }

    @staticmethod
    def _chat_content(question: dict[str, Any]) -> str:
        return json.dumps(question, ensure_ascii=False, sort_keys=True)

    @staticmethod
    def _media_blocks(value: Any) -> list[dict[str, Any]]:
        found: list[dict[str, Any]] = []

        def visit(item: Any, media_hint: str | None = None) -> None:
            if isinstance(item, Mapping):
                local_hint = media_hint
                item_type = str(item.get("type") or item.get("mime_type") or "").casefold()
                if "image" in item_type:
                    local_hint = "image"
                elif "file" in item_type or "attachment" in item_type:
                    local_hint = "file"
                for key, child in item.items():
                    name = str(key).casefold()
                    if (
                        isinstance(child, str)
                        and name in {"image", "image_url", "image_src"}
                        and child.startswith(("http://", "https://", "data:"))
                    ) or (
                        isinstance(child, str)
                        and name == "url"
                        and local_hint == "image"
                        and child.startswith(("http://", "https://", "data:"))
                    ):
                        found.append({"type": "input_image", "image_url": child, "detail": "auto"})
                    elif (
                        isinstance(child, str)
                        and name in {"file", "file_url", "file_data"}
                        and child
                    ):
                        found.append({"type": "input_file", "file_data": child})
                    else:
                        visit(child, local_hint)
            elif isinstance(item, list):
                for child in item:
                    visit(child, media_hint)

        visit(value)
        return found[:32]

    @staticmethod
    def parse_response(body: Mapping[str, Any], protocol: str) -> dict[str, Any]:
        text = body.get("output_text") if protocol == "responses" else None
        if not isinstance(text, str):
            if protocol == "responses":
                for item in body.get("output", []) if isinstance(body.get("output"), list) else []:
                    for content in (
                        item.get("content", [])
                        if isinstance(item, Mapping) and isinstance(item.get("content"), list)
                        else []
                    ):
                        if isinstance(content, Mapping) and content.get("type") in {
                            "output_text",
                            "text",
                        }:
                            text = content.get("text")
                            break
            else:
                choices = body.get("choices", [])
                text = choices[0].get("message", {}).get("content") if choices else None
        if not isinstance(text, str) or not text.strip():
            raise RuntimeError("AI response did not contain text")
        try:
            value = json.loads(text)
        except json.JSONDecodeError as error:
            raise RuntimeError("AI response was not valid JSON") from error
        if not isinstance(value, dict) or "answer" not in value:
            raise RuntimeError("AI response JSON must contain answer")
        return value
