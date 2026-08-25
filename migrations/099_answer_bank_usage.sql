CREATE TABLE answer_bank_usage_records (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id TEXT,
    source TEXT NOT NULL,
    hit_count INTEGER NOT NULL CHECK (hit_count > 0),
    charged_amount INTEGER NOT NULL CHECK (charged_amount >= 0),
    settlement_status TEXT NOT NULL CHECK (settlement_status IN ('not_billable', 'settled')),
    created_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_answer_bank_usage_owner_created
    ON answer_bank_usage_records(owner_user_id, created_at DESC);
