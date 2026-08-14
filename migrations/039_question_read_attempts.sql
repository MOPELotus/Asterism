CREATE TABLE question_read_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    operation_type TEXT NOT NULL,
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    state TEXT NOT NULL CHECK (
        state IN ('prepared', 'issued', 'ambiguous', 'materialized', 'rejected', 'cancelled', 'expired')
    ),
    question_snapshot_id TEXT UNIQUE,
    question_session_id TEXT UNIQUE REFERENCES question_sessions(id) ON DELETE RESTRICT,
    response_digest BLOB CHECK (response_digest IS NULL OR length(response_digest) = 32),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 4),
    expires_at TEXT NOT NULL,
    issued_at TEXT,
    completed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (provider_account_id, owner_user_id, provider_id)
        REFERENCES provider_accounts(id, owner_user_id, provider_id)
        ON DELETE CASCADE,
    FOREIGN KEY (task_id, provider_account_id)
        REFERENCES tasks(id, provider_account_id)
        ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id, task_id, provider_id)
        REFERENCES question_snapshots(id, task_id, provider_id)
        ON DELETE RESTRICT,
    CHECK (created_at < expires_at),
    CHECK (updated_at >= created_at),
    CHECK (
        (state = 'prepared' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NULL AND issued_at IS NULL AND completed_at IS NULL
            AND revision = 1 AND updated_at = created_at)
        OR (state = 'issued' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NULL AND issued_at IS NOT NULL AND completed_at IS NULL
            AND revision = 2 AND updated_at = issued_at)
        OR (state = 'ambiguous' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NULL AND issued_at IS NOT NULL AND completed_at IS NOT NULL
            AND revision = 3 AND updated_at = completed_at)
        OR (state = 'rejected' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NOT NULL AND issued_at IS NOT NULL AND completed_at IS NOT NULL
            AND revision = 3 AND updated_at = completed_at)
        OR (state = 'materialized' AND question_snapshot_id IS NOT NULL
            AND question_session_id IS NOT NULL AND response_digest IS NOT NULL
            AND issued_at IS NOT NULL AND completed_at IS NOT NULL
            AND revision IN (3, 4) AND updated_at = completed_at)
        OR (state IN ('cancelled', 'expired') AND question_snapshot_id IS NULL
            AND question_session_id IS NULL AND response_digest IS NULL AND issued_at IS NULL
            AND completed_at IS NOT NULL AND revision = 2 AND updated_at = completed_at
            AND (state != 'expired' OR completed_at >= expires_at))
    )
) STRICT;

CREATE INDEX idx_question_read_attempts_owner_task_time
    ON question_read_attempts (owner_user_id, task_id, created_at DESC, id DESC);

CREATE INDEX idx_question_read_attempts_recovery
    ON question_read_attempts (state, updated_at)
    WHERE state IN ('issued', 'ambiguous');
