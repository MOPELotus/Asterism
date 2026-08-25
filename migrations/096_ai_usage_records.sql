CREATE TABLE ai_usage_records (
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
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'failed')),
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX idx_ai_usage_owner_created ON ai_usage_records(owner_user_id, created_at DESC);
