CREATE TABLE browser_bridge_command_dispatches (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    dispatched_at TEXT NOT NULL,
    PRIMARY KEY (session_id, sequence),
    FOREIGN KEY (session_id, sequence)
        REFERENCES browser_bridge_exchanges(session_id, sequence)
        ON DELETE CASCADE
);
