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
