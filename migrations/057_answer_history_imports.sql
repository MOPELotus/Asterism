CREATE TABLE answer_history_imports (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    provider_attempt_digest BLOB NOT NULL CHECK (length(provider_attempt_digest) = 32),
    result_digest BLOB NOT NULL CHECK (length(result_digest) = 32),
    content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),
    question_snapshot_id TEXT NOT NULL,
    candidate_count INTEGER NOT NULL CHECK (candidate_count > 0),
    evidence_count INTEGER NOT NULL CHECK (evidence_count > 0),
    imported_at TEXT NOT NULL,
    UNIQUE (provider_account_id, task_id, provider_attempt_digest, result_digest),
    FOREIGN KEY (provider_account_id, provider_id)
        REFERENCES provider_accounts(id, provider_id) ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id, task_id, provider_id)
        REFERENCES question_snapshots(id, task_id, provider_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_answer_history_imports_owner_task
    ON answer_history_imports (owner_user_id, task_id, imported_at DESC, id);
