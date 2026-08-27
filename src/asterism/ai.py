from __future__ import annotations

import hashlib
import json
import os
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

import httpx

from .answers import AnswerRepository, canonical_answer, question_identity, rebind_answer
from .config import LocalConfigStore
from .database import QuestionBank

REASONING_EFFORTS = {"none", "minimal", "low", "medium", "high", "xhigh"}


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
        if route == "escalation":
            # Cidaren uses this transport marker when its instant request
            # timed out; model combinations still configure the timed route.
            route = "timed"
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
                fallback_route = {**route_value, "primary": fallback_name}
                # A domestic endpoint normally uses a different model name.  Do
                # not accidentally send the primary GPT model to the fallback;
                # use an explicit fallback_model or the endpoint's own model.
                if route_value.get("fallback_model"):
                    fallback_route["model"] = route_value["fallback_model"]
                else:
                    fallback_route.pop("model", None)
                fallback = self._choice_from_route(fallback_route, endpoints, "primary")
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
        reasoning_effort = str(route_value.get("reasoning_effort") or "medium").casefold()
        if reasoning_effort not in REASONING_EFFORTS:
            raise ValueError(f"unsupported reasoning effort: {reasoning_effort}")
        return ModelChoice(endpoint, model, reasoning_effort)

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
        if route == "escalation":
            route = "timed"
        combination_name = combination or str(
            self.config.ensure().get("models", {}).get("default") or "economy"
        )
        choice = self.choose(combination_name, route)
        identity_hash, _ = question_identity(provider, question)
        question_id = self.bank.question_id(provider, identity_hash)
        evidence = self._evidence_context(question_id) if question_id is not None else []
        if not force_refresh:
            exact = self.answers.resolve_exact(provider, identity_hash)
            if exact.status == "exact":
                try:
                    rebound = rebind_answer(exact.answer, question.get("options"))
                    rebound = self._validate_answer(question, rebound)
                except (RuntimeError, ValueError):
                    # A stale or ambiguous option set must not block a fresh
                    # AI request. Likewise, retain an old subjective answer
                    # that violates the current safety policy as evidence
                    # rather than auto-submitting it.
                    rebound = None
                if rebound is not None:
                    return {
                        "answer": {"answer": rebound, "confidence": 1.0},
                        "source": "local_cache",
                        "cached": True,
                        "usage": {},
                    }
        evidence_hash = hashlib.sha256(
            json.dumps(evidence, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
            .encode("utf-8")
        ).hexdigest()
        cache_key = self.cache_key(
            provider, identity_hash, combination_name, route, choice, evidence_hash=evidence_hash
        )
        if not force_refresh:
            cached = self.bank.get_ai_cache(cache_key)
            if cached is not None:
                response = dict(cached["response"])
                if "answer" in response:
                    try:
                        response["answer"] = self._validate_answer(
                            question, response["answer"]
                        )
                        response["answer"] = rebind_answer(
                            response["answer"], question.get("options")
                        )
                    except (RuntimeError, ValueError):
                        # The cache key is identity-bound, but a provider may
                        # still change an option's semantic content, or an old
                        # cache may violate the current subjective safety
                        # policy. Treat either case as a miss.
                        response = {}
                if response:
                    return {
                        "answer": response,
                        "source": "ai_cache",
                        "cached": True,
                        "usage": cached["usage"],
                    }
        used_choice = choice
        request_question = dict(question)
        if question_id is not None:
            evidence = self._evidence_context(question_id)
            if evidence:
                request_question["answer_evidence_context"] = evidence
        try:
            if not choice.endpoint.base_url:
                raise RuntimeError(f"AI endpoint {choice.endpoint.name} has no base_url configured")
            key = choice.endpoint.resolved_key()
            if not key:
                raise RuntimeError(f"AI endpoint {choice.endpoint.name} has no API key")
            parsed, usage = self._request(request_question, choice, key, timeout)
        except (httpx.HTTPError, RuntimeError) as error:
            primary_error = RuntimeError(
                f"AI endpoint {choice.endpoint.name} request failed or is unavailable"
            )
            fallback = choice.fallback
            if fallback is None:
                raise primary_error from error
            if not fallback.endpoint.base_url:
                raise primary_error from error
            fallback_key = fallback.endpoint.resolved_key()
            if not fallback_key:
                raise primary_error from error
            try:
                parsed, usage = self._request(request_question, fallback, fallback_key, timeout)
            except (httpx.HTTPError, RuntimeError) as fallback_error:
                raise RuntimeError(
                    f"AI primary and fallback endpoints failed: {fallback_error}"
                ) from fallback_error
            used_choice = fallback
        normalized = dict(parsed)
        normalized["answer"] = self._validate_answer(question, parsed.get("answer"))
        normalized["answer"] = canonical_answer(normalized["answer"], question.get("options"))
        self.bank.put_ai_cache(
            cache_key,
            used_choice.endpoint.name + ":" + used_choice.model,
            normalized,
            usage,
        )
        if question_id is not None:
            self.answers.record_candidate(
                question_id,
                normalized["answer"],
                "ai",
                "unverified",
                source_ref=used_choice.endpoint.name + ":" + used_choice.model,
            )
        return {
            "answer": {
                **normalized,
                "answer": rebind_answer(normalized["answer"], question.get("options")),
            },
            "source": "ai",
            "model": used_choice.endpoint.name + ":" + used_choice.model,
            "cached": False,
            "usage": usage,
        }

    @staticmethod
    def _validate_answer(question: Mapping[str, Any], answer: Any) -> Any:
        """Reject model output that violates the plain-text subjective boundary."""
        if answer is None or (isinstance(answer, str) and not answer.strip()):
            raise RuntimeError("AI answer must be non-empty")
        if isinstance(answer, (list, dict)) and not answer:
            raise RuntimeError("AI answer must be non-empty")
        kind = str(question.get("kind") or question.get("type") or "").casefold()
        if kind not in {"short_answer", "subjective", "discussion", "essay", "long_answer"}:
            return answer

        def validate_text(value: Any) -> str:
            if not isinstance(value, str) or not value.strip():
                raise RuntimeError("AI subjective answer must be non-empty plain text")
            text = value.strip()
            marker_pattern = (
                r"(?im)^\s*(?:system|assistant|user|测试文本|自动化测试)"
                r"\s*[:：]"
            )
            if re.search(marker_pattern, text):
                raise RuntimeError("AI subjective answer contains system or test text")
            if "```" in text or re.search(r"(?m)^\s{0,3}#{1,6}\s", text):
                raise RuntimeError("AI subjective answer contains Markdown formatting")
            if len(text.encode("utf-8")) > 16 * 1024:
                raise RuntimeError("AI subjective answer is oversized")
            return text

        if isinstance(answer, list):
            return [validate_text(item) for item in answer]
        return validate_text(answer)

    def _evidence_context(self, question_id: int) -> list[dict[str, Any]]:
        context: list[dict[str, Any]] = []
        for candidate in self.bank.list_answer_evidence(question_id):
            observations = candidate.get("observations", [])
            counts = {
                outcome: sum(1 for item in observations if item.get("outcome") == outcome)
                for outcome in ("correct", "incorrect", "unverified")
            }
            context.append(
                {
                    "answer": candidate.get("answer"),
                    "source_kind": candidate.get("source_kind"),
                    "confidence": candidate.get("confidence"),
                    "outcomes": counts,
                }
            )
        return context

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
        *,
        evidence_hash: str = "",
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
                evidence_hash,
            )
        )
        return hashlib.sha256(raw.encode("utf-8")).hexdigest()

    @staticmethod
    def _url(endpoint: ModelEndpoint) -> str:
        suffix = "/v1/responses" if endpoint.protocol == "responses" else "/v1/chat/completions"
        if endpoint.base_url.endswith(suffix):
            return endpoint.base_url
        if endpoint.base_url.endswith("/v1"):
            return endpoint.base_url + suffix[len("/v1") :]
        return endpoint.base_url + suffix

    @classmethod
    def build_request(cls, question: dict[str, Any], choice: ModelChoice) -> dict[str, Any]:
        instruction = (
            "You answer an educational question. Return only one JSON object with keys "
            "answer and confidence. answer must be the exact content needed by the question, "
            "not an explanation. Preserve blank order, matching pairs, ordering and rich-media "
            "references. For subjective/discussion answers, use relevant natural plain text only: "
            "no Markdown, labels, system commentary, test phrases, or fabricated citations. "
            "If uncertain, set confidence to 0 and answer to null."
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
                        # The answer payload is intentionally provider-native
                        # (string/list/object for different question kinds).
                        # JSON mode keeps that shape open; parse_response still
                        # requires the fixed outer answer/confidence envelope.
                        "type": "json_object",
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
            "response_format": {"type": "json_object"},
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
                    ):
                        found.append({"type": "input_image", "image_url": child, "detail": "auto"})
                    elif (
                        isinstance(child, str)
                        and name == "url"
                        and local_hint in {"image", "file"}
                        and child.startswith(("http://", "https://", "data:"))
                    ):
                        if local_hint == "file":
                            found.append({"type": "input_file", "file_url": child})
                        else:
                            found.append(
                                {"type": "input_image", "image_url": child, "detail": "auto"}
                            )
                    elif (
                        isinstance(child, str)
                        and name in {"file", "file_url", "attachment", "attachment_url"}
                        and child.startswith(("http://", "https://"))
                    ):
                        found.append({"type": "input_file", "file_url": child})
                    elif isinstance(child, str) and name == "file_data" and child:
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
        confidence = value.get("confidence")
        if (
            isinstance(confidence, bool)
            or not isinstance(confidence, (int, float))
            or not 0 <= confidence <= 1
        ):
            raise RuntimeError("AI response confidence must be a number between 0 and 1")
        return value
