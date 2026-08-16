CREATE TABLE batch_execution_runtime_settings (
    batch_execution_id TEXT PRIMARY KEY NOT NULL,
    batch_execution_attempt_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    resolved_settings_json TEXT NOT NULL,
    sources_json TEXT NOT NULL,
    provider_revision INTEGER CHECK (
        provider_revision IS NULL OR provider_revision >= 1
    ),
    provider_account_revision INTEGER CHECK (
        provider_account_revision IS NULL OR provider_account_revision >= 1
    ),
    completion_policy_json TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    UNIQUE (batch_execution_id, batch_execution_attempt_id),
    FOREIGN KEY (batch_execution_id, batch_execution_attempt_id)
        REFERENCES batch_execution_attempts(batch_execution_id, id)
        ON DELETE CASCADE
) STRICT;
