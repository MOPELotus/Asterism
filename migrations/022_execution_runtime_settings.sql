CREATE TABLE execution_runtime_settings (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    resolved_settings_json TEXT NOT NULL,
    sources_json TEXT NOT NULL,
    provider_revision INTEGER CHECK (provider_revision IS NULL OR provider_revision >= 1),
    provider_account_revision INTEGER
        CHECK (provider_account_revision IS NULL OR provider_account_revision >= 1),
    task_revision INTEGER CHECK (task_revision IS NULL OR task_revision >= 1),
    captured_at TEXT NOT NULL
) STRICT;
