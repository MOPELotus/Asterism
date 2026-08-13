CREATE TABLE browser_bridge_exchanges (
    session_id TEXT NOT NULL REFERENCES browser_bridge_sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    command_type TEXT NOT NULL,
    command_digest BLOB NOT NULL CHECK (length(command_digest) = 32),
    result_type TEXT,
    result_digest BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    state TEXT NOT NULL CHECK (state IN ('issued', 'completed', 'rejected')),
    issued_at TEXT NOT NULL,
    completed_at TEXT,
    PRIMARY KEY (session_id, sequence)
) STRICT;

CREATE INDEX idx_browser_bridge_exchanges_state
    ON browser_bridge_exchanges (session_id, state, sequence);
