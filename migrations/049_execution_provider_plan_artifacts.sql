CREATE TABLE execution_provider_plan_artifacts (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    artifact_type TEXT NOT NULL CHECK (length(artifact_type) BETWEEN 1 AND 96),
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    payload_json TEXT NOT NULL CHECK (length(payload_json) BETWEEN 2 AND 65536),
    captured_at TEXT NOT NULL
) STRICT;
