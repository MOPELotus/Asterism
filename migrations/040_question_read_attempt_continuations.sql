CREATE TABLE question_read_attempts_v2 (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('active', 'ambiguous', 'completed', 'materialized', 'rejected', 'cancelled', 'expired')
    ),
    question_snapshot_id TEXT UNIQUE,
    question_session_id TEXT UNIQUE REFERENCES question_sessions(id) ON DELETE RESTRICT,
    response_digest BLOB CHECK (response_digest IS NULL OR length(response_digest) = 32),
    revision INTEGER NOT NULL CHECK (revision > 0),
    expires_at TEXT NOT NULL,
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
        (state = 'active' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NULL AND completed_at IS NULL)
        OR (state = 'ambiguous' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NULL AND completed_at IS NOT NULL
            AND updated_at = completed_at)
        OR (state = 'rejected' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NOT NULL AND completed_at IS NOT NULL
            AND updated_at = completed_at)
        OR (state = 'completed' AND question_snapshot_id IS NULL AND question_session_id IS NULL
            AND response_digest IS NOT NULL AND completed_at IS NOT NULL
            AND updated_at = completed_at)
        OR (state = 'materialized' AND question_snapshot_id IS NOT NULL
            AND question_session_id IS NOT NULL AND response_digest IS NOT NULL
            AND completed_at IS NOT NULL AND updated_at = completed_at)
        OR (state IN ('cancelled', 'expired') AND question_snapshot_id IS NULL
            AND question_session_id IS NULL AND response_digest IS NULL
            AND completed_at IS NOT NULL AND updated_at = completed_at
            AND (state != 'expired' OR completed_at >= expires_at))
    )
) STRICT;

-- Version 39 did not persist the exact Provider command. Unissued legacy rows
-- are cancelled and possibly-issued rows are conservatively made ambiguous;
-- neither can be dispatched after this migration.
INSERT INTO question_read_attempts_v2 (
    id, owner_user_id, provider_account_id, task_id, provider_id, provider_version,
    state, question_snapshot_id, question_session_id, response_digest, revision,
    expires_at, completed_at, created_at, updated_at
)
SELECT id, owner_user_id, provider_account_id, task_id, provider_id, provider_version,
       CASE state
           WHEN 'prepared' THEN 'cancelled'
           WHEN 'issued' THEN 'ambiguous'
           ELSE state
       END,
       question_snapshot_id, question_session_id, response_digest,
       CASE WHEN state IN ('prepared', 'issued') THEN revision + 1 ELSE revision END,
       expires_at,
       CASE WHEN state IN ('prepared', 'issued') THEN updated_at ELSE completed_at END,
       created_at, updated_at
FROM question_read_attempts;

CREATE TABLE question_read_attempt_operations (
    attempt_id TEXT NOT NULL REFERENCES question_read_attempts_v2(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    continuation_revision INTEGER NOT NULL CHECK (continuation_revision > 0),
    operation_type TEXT NOT NULL,
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    state TEXT NOT NULL CHECK (state IN ('issued', 'accepted', 'rejected', 'ambiguous')),
    result_digest BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    issued_at TEXT NOT NULL,
    completed_at TEXT,
    PRIMARY KEY (attempt_id, sequence),
    UNIQUE (attempt_id, continuation_revision),
    CHECK (
        (state = 'issued' AND result_digest IS NULL AND completed_at IS NULL)
        OR (state = 'ambiguous' AND result_digest IS NULL AND completed_at IS NOT NULL)
        OR (state IN ('accepted', 'rejected') AND result_digest IS NOT NULL
            AND completed_at IS NOT NULL)
    ),
    CHECK (completed_at IS NULL OR completed_at >= issued_at)
) STRICT;

INSERT INTO question_read_attempt_operations (
    attempt_id, sequence, continuation_revision, operation_type, request_digest,
    state, result_digest, issued_at, completed_at
)
SELECT id, 1, 1, operation_type, request_digest,
       CASE state
           WHEN 'materialized' THEN 'accepted'
           WHEN 'issued' THEN 'ambiguous'
           ELSE state
       END,
       CASE WHEN state IN ('materialized', 'rejected') THEN response_digest ELSE NULL END,
       issued_at,
       CASE WHEN state = 'issued' THEN issued_at ELSE completed_at END
FROM question_read_attempts
WHERE state IN ('issued', 'ambiguous', 'materialized', 'rejected');

DROP TABLE question_read_attempts;
ALTER TABLE question_read_attempts_v2 RENAME TO question_read_attempts;

CREATE INDEX idx_question_read_attempts_owner_task_time_v2
    ON question_read_attempts (owner_user_id, task_id, created_at DESC, id DESC);

CREATE INDEX idx_question_read_attempts_recovery_v2
    ON question_read_attempts (state, updated_at)
    WHERE state = 'ambiguous';

CREATE TABLE question_read_attempt_continuations (
    attempt_id TEXT PRIMARY KEY NOT NULL REFERENCES question_read_attempts(id) ON DELETE CASCADE,
    secret_blob_id TEXT UNIQUE NOT NULL REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    continuation_type TEXT NOT NULL,
    continuation_digest BLOB NOT NULL CHECK (length(continuation_digest) = 32),
    phase TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (updated_at >= created_at)
) STRICT;

CREATE INDEX idx_question_read_attempt_operations_state
    ON question_read_attempt_operations (state, issued_at)
    WHERE state IN ('issued', 'ambiguous');
