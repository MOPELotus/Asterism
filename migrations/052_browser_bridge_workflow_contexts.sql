CREATE TABLE browser_bridge_workflow_contexts (
    session_id TEXT PRIMARY KEY NOT NULL
        REFERENCES browser_bridge_sessions(id) ON DELETE CASCADE,
    runtime_settings_digest BLOB NOT NULL
        CHECK (length(runtime_settings_digest) = 32),
    runtime_settings_json TEXT NOT NULL,
    plan_type TEXT,
    plan_digest BLOB CHECK (plan_digest IS NULL OR length(plan_digest) = 32),
    plan_secret_blob_id TEXT UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    CHECK (
        (plan_type IS NULL AND plan_digest IS NULL AND plan_secret_blob_id IS NULL)
        OR
        (plan_type IS NOT NULL AND plan_digest IS NOT NULL AND plan_secret_blob_id IS NOT NULL)
    )
) STRICT;
