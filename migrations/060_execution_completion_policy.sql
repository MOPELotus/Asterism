ALTER TABLE execution_runtime_settings RENAME TO execution_runtime_settings_legacy;

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
    completion_policy_json TEXT NOT NULL,
    captured_at TEXT NOT NULL
) STRICT;

INSERT INTO execution_runtime_settings (
    execution_id,
    provider_id,
    schema_version,
    resolved_settings_json,
    sources_json,
    provider_revision,
    provider_account_revision,
    task_revision,
    completion_policy_json,
    captured_at
)
SELECT
    execution_id,
    provider_id,
    schema_version,
    resolved_settings_json,
    sources_json,
    provider_revision,
    provider_account_revision,
    task_revision,
    json_object(
        'strict_completion_enabled', json('true'),
        'score_improvement_enabled', json('false'),
        'strict_attempt_limit', 3,
        'score_improvement_attempt_limit', 1,
        'score_target_millis', 1000,
        'strict_expires_at',
            strftime('%Y-%m-%dT%H:%M:%S', captured_at, '+7 days') || substr(captured_at, 20),
        'score_improvement_expires_at',
            strftime('%Y-%m-%dT%H:%M:%S', captured_at, '+1 day') || substr(captured_at, 20),
        'formal_retry_requires_confirmation', json('true'),
        'captured_at', captured_at
    ),
    captured_at
FROM execution_runtime_settings_legacy;

DROP TABLE execution_runtime_settings_legacy;
