CREATE TABLE browser_bridge_runtime_state_artifacts (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    state_type TEXT NOT NULL,
    state_digest BLOB NOT NULL CHECK (length(state_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    stored_at TEXT NOT NULL,
    PRIMARY KEY (session_id, sequence),
    FOREIGN KEY (session_id, sequence)
        REFERENCES browser_bridge_exchanges(session_id, sequence)
        ON DELETE CASCADE
) STRICT;
