CREATE TABLE browser_bridge_runtime_bindings (
    session_id TEXT PRIMARY KEY REFERENCES browser_bridge_sessions(id) ON DELETE CASCADE,
    observed_origin TEXT NOT NULL,
    frame_id TEXT NOT NULL,
    bound_at TEXT NOT NULL
) STRICT;
