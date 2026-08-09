CREATE UNIQUE INDEX idx_tasks_id_provider_account
    ON tasks (id, provider_account_id);

CREATE TABLE provider_runtime_settings (
    id TEXT PRIMARY KEY NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('provider', 'provider_account', 'task')),
    provider_id TEXT NOT NULL,
    provider_account_id TEXT,
    task_id TEXT,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    settings_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (scope = 'provider' AND provider_account_id IS NULL AND task_id IS NULL)
        OR (scope = 'provider_account' AND provider_account_id IS NOT NULL AND task_id IS NULL)
        OR (scope = 'task' AND provider_account_id IS NOT NULL AND task_id IS NOT NULL)
    ),
    FOREIGN KEY (provider_account_id, provider_id)
        REFERENCES provider_accounts (id, provider_id) ON DELETE CASCADE,
    FOREIGN KEY (task_id, provider_account_id)
        REFERENCES tasks (id, provider_account_id) ON DELETE CASCADE
) STRICT;

CREATE UNIQUE INDEX idx_provider_runtime_settings_provider
    ON provider_runtime_settings (provider_id)
    WHERE scope = 'provider';

CREATE UNIQUE INDEX idx_provider_runtime_settings_account
    ON provider_runtime_settings (provider_account_id)
    WHERE scope = 'provider_account';

CREATE UNIQUE INDEX idx_provider_runtime_settings_task
    ON provider_runtime_settings (task_id)
    WHERE scope = 'task';
