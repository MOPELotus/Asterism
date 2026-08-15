CREATE TABLE interactive_auth_continuations (
    auth_session_id TEXT PRIMARY KEY NOT NULL
        REFERENCES auth_sessions(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL CHECK (length(provider_id) BETWEEN 1 AND 128),
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE CASCADE,
    continuation_type TEXT NOT NULL CHECK (length(continuation_type) BETWEEN 1 AND 96),
    continuation_digest BLOB NOT NULL CHECK (length(continuation_digest) = 32),
    phase TEXT NOT NULL CHECK (length(phase) BETWEEN 1 AND 96),
    revision INTEGER NOT NULL CHECK (revision > 0),
    poll_count INTEGER NOT NULL CHECK (poll_count >= 0),
    maximum_polls INTEGER NOT NULL CHECK (maximum_polls BETWEEN 1 AND 10000),
    active_poll_sequence INTEGER CHECK (active_poll_sequence > 0),
    active_poll_digest BLOB CHECK (
        active_poll_digest IS NULL OR length(active_poll_digest) = 32
    ),
    active_poll_expires_at TEXT,
    terminal_result_digest BLOB CHECK (
        terminal_result_digest IS NULL OR length(terminal_result_digest) = 32
    ),
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (poll_count <= maximum_polls),
    CHECK (updated_at >= created_at),
    CHECK (expires_at > created_at),
    CHECK (
        (active_poll_sequence IS NULL AND active_poll_digest IS NULL
            AND active_poll_expires_at IS NULL)
        OR (active_poll_sequence IS NOT NULL AND active_poll_digest IS NOT NULL
            AND active_poll_expires_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX idx_interactive_auth_continuations_provider
    ON interactive_auth_continuations (provider_id, updated_at);

CREATE TABLE interactive_auth_poll_operations (
    auth_session_id TEXT NOT NULL
        REFERENCES auth_sessions(id) ON DELETE CASCADE,
    poll_sequence INTEGER NOT NULL CHECK (poll_sequence > 0),
    continuation_revision INTEGER NOT NULL CHECK (continuation_revision > 0),
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    state TEXT NOT NULL CHECK (
        state IN (
            'issued', 'retryable', 'waiting', 'authenticated', 'rejected', 'expired', 'failed'
        )
    ),
    result_digest BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    issued_at TEXT NOT NULL,
    completed_at TEXT,
    PRIMARY KEY (auth_session_id, poll_sequence),
    CHECK (
        (state = 'issued' AND result_digest IS NULL AND completed_at IS NULL)
        OR (state = 'retryable' AND result_digest IS NULL AND completed_at IS NOT NULL)
        OR (state IN ('waiting', 'authenticated', 'rejected', 'expired', 'failed')
            AND result_digest IS NOT NULL AND completed_at IS NOT NULL)
    )
) STRICT;
