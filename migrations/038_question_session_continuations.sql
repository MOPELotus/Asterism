CREATE TABLE question_session_continuations (
    session_id TEXT PRIMARY KEY NOT NULL
        REFERENCES question_sessions(id) ON DELETE CASCADE,
    execution_id TEXT UNIQUE REFERENCES executions(id) ON DELETE RESTRICT,
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE CASCADE,
    continuation_type TEXT NOT NULL,
    continuation_digest BLOB NOT NULL CHECK (length(continuation_digest) = 32),
    phase TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (updated_at >= created_at),
    UNIQUE (session_id, execution_id)
) STRICT;

CREATE TABLE question_session_operations (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    continuation_revision INTEGER NOT NULL CHECK (continuation_revision > 0),
    operation_type TEXT NOT NULL,
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    state TEXT NOT NULL CHECK (state IN ('issued', 'accepted', 'rejected', 'ambiguous')),
    result_digest BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    issued_at TEXT NOT NULL,
    completed_at TEXT,
    PRIMARY KEY (session_id, sequence),
    UNIQUE (session_id, continuation_revision),
    FOREIGN KEY (session_id, execution_id)
        REFERENCES question_session_continuations(session_id, execution_id)
        ON DELETE CASCADE,
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT,
    CHECK (
        (state = 'issued' AND result_digest IS NULL AND completed_at IS NULL)
        OR (state IN ('accepted', 'rejected') AND result_digest IS NOT NULL
            AND completed_at IS NOT NULL AND completed_at >= issued_at)
        OR (state = 'ambiguous' AND result_digest IS NULL
            AND completed_at IS NOT NULL AND completed_at >= issued_at)
    )
) STRICT;

CREATE INDEX idx_question_session_operations_execution
    ON question_session_operations (execution_id, state, sequence);
