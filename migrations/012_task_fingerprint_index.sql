CREATE UNIQUE INDEX idx_tasks_account_source_fingerprint
    ON tasks (provider_account_id, source_type, remote_fingerprint);

CREATE INDEX idx_tasks_account_updated
    ON tasks (provider_account_id, updated_at DESC, id DESC);
