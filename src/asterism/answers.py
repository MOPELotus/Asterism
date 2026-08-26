from __future__ import annotations

import hashlib
import json
import re
import unicodedata
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from .database import QuestionBank

IDENTIFIER_KEYS = {
    "id",
    "remote_id",
    "question_id",
    "questionid",
    "option_id",
    "optionid",
    "index",
    "position",
    "sequence",
    "order",
    "letter",
    "key",
}
EPHEMERAL_KEYS = {
    "answer",
    "answer_evidence",
    "correct_answer",
    "learner_response",
    "submitted",
    "submitted_answer",
    "submitted_value",
    "score",
    "status",
    "session",
    "credentials",
    "password",
    "cookie",
    "token",
}
OPTION_KEYS = {"options", "choices", "choice", "option_list"}
SIGNED_URL_KEYS = {
    "token",
    "sign",
    "signature",
    "auth",
    "expires",
    "timestamp",
    "ts",
}
OPTION_PREFIX = re.compile(r"^\s*(?:[A-Za-z]|\d+)\s*[.、:：)）]\s*")


def normalize_text(value: str) -> str:
    return " ".join(unicodedata.normalize("NFKC", value).replace("\u00a0", " ").split())


def stable_url(value: str) -> str:
    parsed = urlsplit(value.strip())
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        return normalize_text(value)
    return urlunsplit(("https", parsed.netloc.casefold(), parsed.path, "", ""))


def _semantic_value(value: Any, *, option: bool = False) -> Any:
    if value is None or isinstance(value, (bool, int, float)):
        return value
    if isinstance(value, str):
        text = normalize_text(value)
        if option:
            text = OPTION_PREFIX.sub("", text)
        if text.startswith(("http://", "https://")):
            return stable_url(text)
        return text
    if isinstance(value, list):
        return [_semantic_value(item, option=option) for item in value]
    if isinstance(value, dict):
        result: dict[str, Any] = {}
        for raw_key, item in sorted(value.items(), key=lambda pair: str(pair[0]).casefold()):
            key = str(raw_key)
            lowered = key.casefold().replace("-", "_")
            if (
                lowered in IDENTIFIER_KEYS
                or lowered in SIGNED_URL_KEYS
                or lowered in EPHEMERAL_KEYS
            ):
                continue
            result[key] = _semantic_value(item, option=option)
        return result
    return normalize_text(str(value))


def option_semantics(options: Any) -> list[Any]:
    if isinstance(options, dict):
        values: Iterable[Any] = options.values()
    elif isinstance(options, list):
        values = options
    else:
        values = ()
    return [_semantic_value(value, option=True) for value in values]


def canonical_question(question: dict[str, Any]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for raw_key, value in question.items():
        key = str(raw_key)
        lowered = key.casefold().replace("-", "_")
        if lowered in IDENTIFIER_KEYS or lowered in EPHEMERAL_KEYS:
            continue
        if lowered in OPTION_KEYS:
            result[key] = sorted(
                option_semantics(value),
                key=lambda item: json.dumps(item, ensure_ascii=False, sort_keys=True),
            )
        else:
            result[key] = _semantic_value(value)
    prompt = result.get("prompt") or result.get("question") or result.get("stem")
    if not normalize_text(str(prompt or "")):
        raise ValueError("question prompt must not be empty")
    return result


def question_identity(provider: str, question: dict[str, Any]) -> tuple[str, dict[str, Any]]:
    canonical = canonical_question(question)
    encoded = json.dumps(canonical, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(f"{provider}\0{encoded}".encode()).hexdigest(), canonical


def _option_bindings(options: Any) -> tuple[dict[str, Any], dict[str, str]]:
    by_key: dict[str, Any] = {}
    key_by_semantic: dict[str, str] = {}
    items: list[tuple[str, Any]] = []
    if isinstance(options, dict):
        items = [(str(key), value) for key, value in options.items()]
    elif isinstance(options, list):
        for index, value in enumerate(options):
            key = chr(ord("A") + index) if index < 26 else str(index + 1)
            if isinstance(value, dict):
                explicit = next(
                    (
                        value.get(name)
                        for name in ("key", "letter", "code", "value", "id")
                        if value.get(name) not in (None, "")
                    ),
                    None,
                )
                if explicit is not None:
                    key = str(explicit)
            elif isinstance(value, str):
                match = re.match(r"^\s*([A-Za-z]|\d+)\s*[.、:：)）]", value)
                if match:
                    key = match.group(1).upper()
            items.append((key, value))
    for key, value in items:
        semantic = _semantic_value(value, option=True)
        encoded = json.dumps(semantic, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        by_key[key.casefold()] = semantic
        if encoded not in key_by_semantic:
            key_by_semantic[encoded] = key
        else:
            key_by_semantic[encoded] = ""
    return by_key, key_by_semantic


def canonical_answer(answer: Any, options: Any) -> Any:
    by_key, key_by_semantic = _option_bindings(options)
    semantic_by_encoded: dict[str, Any] = {}
    for _option_key, semantic in by_key.items():
        encoded = json.dumps(semantic, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        if key_by_semantic.get(encoded):
            semantic_by_encoded[encoded] = semantic

    def convert(value: Any) -> Any:
        if isinstance(value, str):
            text = normalize_text(value)
            direct = by_key.get(text.casefold())
            if direct is not None:
                return {"option": direct}
            semantic = semantic_by_encoded.get(
                json.dumps(text, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
            )
            if semantic is not None:
                return {"option": semantic}
            if len(text) > 1 and text.isalpha() and all(char.casefold() in by_key for char in text):
                return {"options": [by_key[char.casefold()] for char in text]}
            return {"text": text}
        if isinstance(value, list):
            converted = [convert(item) for item in value]
            if all(isinstance(item, dict) and set(item) == {"option"} for item in converted):
                return {"options": [item["option"] for item in converted]}
            return converted
        if isinstance(value, dict):
            return {str(key): convert(item) for key, item in sorted(value.items())}
        return value

    return convert(answer)


def rebind_answer(canonical: Any, options: Any) -> Any:
    _, key_by_semantic = _option_bindings(options)

    def bind(value: Any) -> Any:
        if isinstance(value, dict) and set(value) == {"option"}:
            encoded = json.dumps(
                value["option"], ensure_ascii=False, sort_keys=True, separators=(",", ":")
            )
            key = key_by_semantic.get(encoded)
            if not key:
                raise ValueError("option content did not bind uniquely")
            return key
        if isinstance(value, dict) and set(value) == {"options"}:
            return [bind({"option": item}) for item in value["options"]]
        if isinstance(value, dict) and set(value) == {"text"}:
            return value["text"]
        if isinstance(value, dict):
            return {key: bind(item) for key, item in value.items()}
        if isinstance(value, list):
            return [bind(item) for item in value]
        return value

    return bind(canonical)


@dataclass(frozen=True)
class ResolvedAnswer:
    status: str
    answer: Any | None = None
    candidate_id: int | None = None
    correct_count: int = 0
    incorrect_count: int = 0


class AnswerRepository:
    def __init__(self, database: QuestionBank) -> None:
        self.database = database

    def ingest_question(self, provider: str, question: dict[str, Any]) -> tuple[int, str]:
        identity_hash, canonical = question_identity(provider, question)
        question_id = self.database.upsert_question(
            provider, identity_hash, str(question.get("kind") or "provider_native"), canonical
        )
        evidence = question.get("answer_evidence")
        if isinstance(evidence, dict):
            source = str(evidence.get("source") or f"{provider}_native")
            answer = evidence.get("value", evidence.get("answer"))
            verified = evidence.get("verified")
            if answer is not None:
                outcome = "correct" if verified is not False else "unverified"
                if provider in {"chaoxing", "uai"} and verified is None:
                    outcome = "correct"
                self.record_candidate(
                    question_id,
                    canonical_answer(answer, question.get("options")),
                    source,
                    outcome,
                    confidence=0.25 if provider == "cidaren" else 1.0,
                )
            submitted = evidence.get("submitted_value")
            if submitted is not None and evidence.get("submitted_correct") is False:
                self.record_candidate(
                    question_id,
                    canonical_answer(submitted, question.get("options")),
                    f"{source}_submitted",
                    "incorrect",
                )
        return question_id, identity_hash

    def record_candidate(
        self,
        question_id: int,
        answer: Any,
        source_kind: str,
        outcome: str = "unverified",
        *,
        source_ref: str = "",
        confidence: float | None = None,
        task_ref: str = "",
        details: dict[str, Any] | None = None,
    ) -> int:
        if outcome not in {"correct", "incorrect", "unverified"}:
            raise ValueError(f"unsupported outcome: {outcome}")
        encoded = json.dumps(answer, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        answer_hash = hashlib.sha256(encoded.encode()).hexdigest()
        with self.database.connect() as connection:
            connection.execute(
                """INSERT INTO answer_candidates(
                       question_id, answer_hash, answer_json, source_kind, source_ref, confidence
                   ) VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT(question_id, answer_hash, source_kind, source_ref) DO UPDATE SET
                       confidence=COALESCE(excluded.confidence, answer_candidates.confidence)""",
                (question_id, answer_hash, encoded, source_kind, source_ref, confidence),
            )
            row = connection.execute(
                """SELECT id FROM answer_candidates
                   WHERE question_id=? AND answer_hash=? AND source_kind=? AND source_ref=?""",
                (question_id, answer_hash, source_kind, source_ref),
            ).fetchone()
            if row is None:
                raise RuntimeError("answer candidate insert returned no row")
            candidate_id = int(row[0])
            connection.execute(
                """INSERT INTO answer_observations(candidate_id, outcome, task_ref, details_json)
                   VALUES (?, ?, ?, ?)""",
                (
                    candidate_id,
                    outcome,
                    task_ref,
                    json.dumps(details or {}, ensure_ascii=False, sort_keys=True),
                ),
            )
            return candidate_id

    def resolve_exact(self, provider: str, identity_hash: str) -> ResolvedAnswer:
        question_id = self.database.question_id(provider, identity_hash)
        if question_id is None:
            return ResolvedAnswer("missing")
        with self.database.connect() as connection:
            rows = connection.execute(
                """SELECT candidate.id, candidate.answer_json,
                          SUM(CASE WHEN observation.outcome='correct'
                              THEN 1 ELSE 0 END) AS correct_count,
                          SUM(CASE WHEN observation.outcome='incorrect'
                              THEN 1 ELSE 0 END) AS incorrect_count
                   FROM answer_candidates candidate
                   LEFT JOIN answer_observations observation
                     ON observation.candidate_id=candidate.id
                   WHERE candidate.question_id=?
                   GROUP BY candidate.id, candidate.answer_json""",
                (question_id,),
            ).fetchall()
        reusable = [row for row in rows if int(row[2] or 0) > 0 and int(row[3] or 0) == 0]
        has_correct = any(int(row[2] or 0) > 0 for row in rows)
        has_incorrect = any(int(row[3] or 0) > 0 for row in rows)
        conflicted = has_correct and has_incorrect
        unique_answers = {str(row[1]) for row in reusable}
        if conflicted or len(unique_answers) > 1:
            return ResolvedAnswer("conflict")
        if not reusable:
            return ResolvedAnswer("unverified" if rows else "missing")
        row = reusable[0]
        return ResolvedAnswer(
            "exact",
            json.loads(str(row[1])),
            int(row[0]),
            int(row[2] or 0),
            int(row[3] or 0),
        )
