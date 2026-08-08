CREATE TABLE executions (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
    requested_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    request_source TEXT NOT NULL,
    quote_id TEXT REFERENCES price_quotes(id) ON DELETE RESTRICT,
    resolved_plan_json TEXT,
    state TEXT NOT NULL,
    scheduled_at TEXT,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_executions_task_time ON executions (task_id, created_at DESC);
CREATE INDEX idx_executions_queue ON executions (state, scheduled_at);

CREATE TABLE execution_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    attempt_no INTEGER NOT NULL CHECK (attempt_no > 0),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    result TEXT,
    error_class TEXT,
    provider_trace_id TEXT,
    UNIQUE (execution_id, attempt_no)
) STRICT;

CREATE TABLE execution_leases (
    task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    execution_id TEXT NOT NULL UNIQUE REFERENCES executions(id) ON DELETE CASCADE,
    worker_id TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT;

CREATE TABLE execution_progress (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    percent INTEGER CHECK (percent BETWEEN 0 AND 100),
    stage TEXT NOT NULL,
    status_text TEXT,
    current_item TEXT,
    completed_items INTEGER CHECK (completed_items IS NULL OR completed_items >= 0),
    total_items INTEGER CHECK (total_items IS NULL OR total_items >= 0),
    updated_at TEXT NOT NULL,
    CHECK (completed_items IS NULL OR total_items IS NULL OR completed_items <= total_items)
) STRICT;

CREATE TABLE execution_logs (
    id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    attempt_id TEXT REFERENCES execution_attempts(id) ON DELETE SET NULL,
    timestamp TEXT NOT NULL,
    level TEXT NOT NULL,
    stage TEXT NOT NULL,
    message TEXT NOT NULL,
    provider_trace_id TEXT,
    metadata_sanitized_json TEXT
) STRICT;

CREATE INDEX idx_execution_logs_stream
    ON execution_logs (execution_id, timestamp);
