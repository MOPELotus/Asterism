CREATE TABLE auth_bootstrap_client_events (
    session_id TEXT NOT NULL REFERENCES auth_bootstrap_sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    kind_json TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (session_id, sequence)
) STRICT;

CREATE INDEX idx_auth_bootstrap_events_received
    ON auth_bootstrap_client_events (session_id, received_at, sequence);
