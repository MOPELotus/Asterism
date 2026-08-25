CREATE TABLE ai_usage_records_v2 (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id TEXT,
    provider_endpoint TEXT NOT NULL,
    model TEXT NOT NULL,
    profile TEXT NOT NULL,
    route TEXT NOT NULL,
    input_chars INTEGER NOT NULL,
    output_chars INTEGER NOT NULL,
    remote_input_tokens INTEGER,
    remote_output_tokens INTEGER,
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'failed', 'cached')),
    created_at TEXT NOT NULL,
    estimated_cost INTEGER NOT NULL DEFAULT 0 CHECK (estimated_cost >= 0),
    settlement_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (settlement_status IN ('not_billable', 'pending', 'settled', 'waived'))
) STRICT;

INSERT INTO ai_usage_records_v2 (
    id,
    owner_user_id,
    task_id,
    provider_endpoint,
    model,
    profile,
    route,
    input_chars,
    output_chars,
    remote_input_tokens,
    remote_output_tokens,
    outcome,
    created_at,
    estimated_cost,
    settlement_status
)
SELECT
    id,
    owner_user_id,
    task_id,
    provider_endpoint,
    model,
    profile,
    route,
    input_chars,
    output_chars,
    remote_input_tokens,
    remote_output_tokens,
    outcome,
    created_at,
    estimated_cost,
    settlement_status
FROM ai_usage_records;

DROP TABLE ai_usage_records;
ALTER TABLE ai_usage_records_v2 RENAME TO ai_usage_records;

CREATE INDEX idx_ai_usage_owner_created
    ON ai_usage_records(owner_user_id, created_at DESC);
