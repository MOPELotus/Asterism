CREATE TABLE browser_bridge_result_artifacts (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    result_type TEXT NOT NULL,
    result_digest BLOB NOT NULL CHECK (length(result_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    received_at TEXT NOT NULL,
    PRIMARY KEY (session_id, sequence),
    FOREIGN KEY (session_id, sequence)
        REFERENCES browser_bridge_exchanges(session_id, sequence)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_browser_bridge_result_received
    ON browser_bridge_result_artifacts (received_at, session_id, sequence);
