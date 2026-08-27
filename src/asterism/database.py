from __future__ import annotations

import json
import sqlite3
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1

SCHEMA = """
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS questions (
    id INTEGER PRIMARY KEY,
    provider TEXT NOT NULL,
    identity_hash TEXT NOT NULL,
    native_kind TEXT NOT NULL,
    content_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(provider, identity_hash)
);

CREATE TABLE IF NOT EXISTS answer_candidates (
    id INTEGER PRIMARY KEY,
    question_id INTEGER NOT NULL REFERENCES questions(id) ON DELETE CASCADE,
    answer_hash TEXT NOT NULL,
    answer_json TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL DEFAULT '',
    confidence REAL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(question_id, answer_hash, source_kind, source_ref)
);

CREATE TABLE IF NOT EXISTS answer_observations (
    id INTEGER PRIMARY KEY,
    candidate_id INTEGER NOT NULL REFERENCES answer_candidates(id) ON DELETE CASCADE,
    outcome TEXT NOT NULL CHECK(outcome IN ('correct', 'incorrect', 'unverified')),
    task_ref TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    observed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS ai_cache (
    cache_key TEXT PRIMARY KEY,
    model_profile TEXT NOT NULL,
    response_json TEXT NOT NULL,
    usage_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_used_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS formal_drafts (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    task_ref TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('draft', 'submitted', 'discarded')),
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    submitted_at TEXT
);

CREATE INDEX IF NOT EXISTS answer_observations_candidate_idx
ON answer_observations(candidate_id, observed_at);
CREATE INDEX IF NOT EXISTS formal_drafts_profile_idx
ON formal_drafts(provider, profile_id, status, updated_at);
"""


class QuestionBank:
    def __init__(self, path: Path) -> None:
        self.path = path

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        connection = sqlite3.connect(self.path)
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute("PRAGMA busy_timeout = 5000")
        try:
            yield connection
            connection.commit()
        except BaseException:
            connection.rollback()
            raise
        finally:
            connection.close()

    def initialize(self) -> None:
        with self.connect() as connection:
            connection.executescript(SCHEMA)
            versions = [row[0] for row in connection.execute("SELECT version FROM schema_version")]
            if versions and max(versions) > SCHEMA_VERSION:
                raise RuntimeError("question bank was created by a newer Asterism version")
            connection.execute(
                "INSERT OR IGNORE INTO schema_version(version) VALUES (?)", (SCHEMA_VERSION,)
            )

    def upsert_question(
        self, provider: str, identity_hash: str, native_kind: str, content: dict[str, Any]
    ) -> int:
        encoded = json.dumps(content, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        with self.connect() as connection:
            connection.execute(
                """INSERT INTO questions(provider, identity_hash, native_kind, content_json)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT(provider, identity_hash) DO UPDATE SET
                     native_kind=excluded.native_kind,
                     content_json=excluded.content_json,
                     updated_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now')""",
                (provider, identity_hash, native_kind, encoded),
            )
            row = connection.execute(
                "SELECT id FROM questions WHERE provider=? AND identity_hash=?",
                (provider, identity_hash),
            ).fetchone()
            if row is None:
                raise RuntimeError("question upsert returned no row")
            return int(row[0])

    def table_names(self) -> set[str]:
        with self.connect() as connection:
            return {
                str(row[0])
                for row in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
                )
            }

    def question_id(self, provider: str, identity_hash: str) -> int | None:
        with self.connect() as connection:
            row = connection.execute(
                "SELECT id FROM questions WHERE provider=? AND identity_hash=?",
                (provider, identity_hash),
            ).fetchone()
            return int(row[0]) if row else None

    def question_count(self) -> int:
        with self.connect() as connection:
            row = connection.execute("SELECT COUNT(*) FROM questions").fetchone()
            return int(row[0]) if row else 0

    def get_ai_cache(self, cache_key: str) -> dict[str, Any] | None:
        with self.connect() as connection:
            row = connection.execute(
                "SELECT model_profile, response_json, usage_json FROM ai_cache WHERE cache_key=?",
                (cache_key,),
            ).fetchone()
            if row is None:
                return None
            connection.execute(
                "UPDATE ai_cache SET last_used_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now') "
                "WHERE cache_key=?",
                (cache_key,),
            )
            return {
                "model_profile": str(row[0]),
                "response": json.loads(str(row[1])),
                "usage": json.loads(str(row[2])),
            }

    def put_ai_cache(
        self,
        cache_key: str,
        model_profile: str,
        response: dict[str, Any],
        usage: dict[str, Any] | None = None,
    ) -> None:
        with self.connect() as connection:
            connection.execute(
                """INSERT INTO ai_cache(cache_key, model_profile, response_json, usage_json)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT(cache_key) DO UPDATE SET
                     model_profile=excluded.model_profile,
                     response_json=excluded.response_json,
                     usage_json=excluded.usage_json,
                     last_used_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now')""",
                (
                    cache_key,
                    model_profile,
                    json.dumps(response, ensure_ascii=False, sort_keys=True),
                    json.dumps(usage or {}, ensure_ascii=False, sort_keys=True),
                ),
            )

    def list_questions(
        self, provider: str | None = None, *, limit: int = 500
    ) -> list[dict[str, Any]]:
        if limit < 1 or limit > 10000:
            raise ValueError("question list limit must be between 1 and 10000")
        with self.connect() as connection:
            if provider:
                rows = connection.execute(
                    """SELECT id, provider, identity_hash, native_kind, content_json,
                              created_at, updated_at
                       FROM questions WHERE provider=? ORDER BY updated_at DESC LIMIT ?""",
                    (provider, limit),
                ).fetchall()
            else:
                rows = connection.execute(
                    """SELECT id, provider, identity_hash, native_kind, content_json,
                              created_at, updated_at
                       FROM questions ORDER BY updated_at DESC LIMIT ?""",
                    (limit,),
                ).fetchall()
            return [
                {
                    "id": int(row[0]),
                    "provider": str(row[1]),
                    "identity_hash": str(row[2]),
                    "native_kind": str(row[3]),
                    "content": json.loads(str(row[4])),
                    "created_at": str(row[5]),
                    "updated_at": str(row[6]),
                }
                for row in rows
            ]

    def list_answer_evidence(self, question_id: int) -> list[dict[str, Any]]:
        with self.connect() as connection:
            rows = connection.execute(
                """SELECT candidate.id, candidate.answer_json, candidate.source_kind,
                          candidate.source_ref, candidate.confidence, candidate.created_at,
                          observation.id, observation.outcome, observation.task_ref,
                          observation.details_json, observation.observed_at
                   FROM answer_candidates candidate
                   LEFT JOIN answer_observations observation
                     ON observation.candidate_id=candidate.id
                   WHERE candidate.question_id=?
                   ORDER BY candidate.created_at DESC, observation.observed_at DESC""",
                (question_id,),
            ).fetchall()
        candidates: dict[int, dict[str, Any]] = {}
        for row in rows:
            candidate_id = int(row[0])
            candidate = candidates.setdefault(
                candidate_id,
                {
                    "id": candidate_id,
                    "answer": json.loads(str(row[1])),
                    "source_kind": str(row[2]),
                    "source_ref": str(row[3]),
                    "confidence": float(row[4]) if row[4] is not None else None,
                    "created_at": str(row[5]),
                    "observations": [],
                },
            )
            if row[6] is not None:
                candidate["observations"].append(
                    {
                        "id": int(row[6]),
                        "outcome": str(row[7]),
                        "task_ref": str(row[8] or ""),
                        "details": json.loads(str(row[9] or "{}")),
                        "observed_at": str(row[10]),
                    }
                )
        return list(candidates.values())
