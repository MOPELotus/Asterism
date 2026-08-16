CREATE UNIQUE INDEX idx_courses_account_binding
    ON courses (id, provider_account_id);

CREATE TABLE batch_executions (
    id TEXT PRIMARY KEY NOT NULL,
    provider_account_id TEXT NOT NULL,
    course_id TEXT NOT NULL,
    requested_capabilities_json TEXT NOT NULL,
    expected_child_count INTEGER NOT NULL CHECK (expected_child_count BETWEEN 1 AND 8192),
    requested_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    request_source TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN (
            'requested', 'scheduled', 'running', 'recovering', 'retry_waiting',
            'human_required', 'succeeded', 'failed', 'cancelled'
        )
    ),
    scheduled_at TEXT,
    started_at TEXT,
    finished_at TEXT,
    idempotency_scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (id, provider_account_id),
    UNIQUE (idempotency_scope, idempotency_key),
    FOREIGN KEY (course_id, provider_account_id)
        REFERENCES courses(id, provider_account_id) ON DELETE RESTRICT,
    FOREIGN KEY (provider_account_id)
        REFERENCES provider_accounts(id) ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_batch_executions_account_time
    ON batch_executions (provider_account_id, created_at DESC);

CREATE INDEX idx_batch_executions_state
    ON batch_executions (state, scheduled_at);

CREATE TABLE batch_execution_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    batch_execution_id TEXT NOT NULL REFERENCES batch_executions(id) ON DELETE CASCADE,
    attempt_no INTEGER NOT NULL CHECK (attempt_no > 0),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    result TEXT,
    error_class TEXT,
    provider_trace_id TEXT,
    UNIQUE (batch_execution_id, attempt_no),
    UNIQUE (batch_execution_id, id)
) STRICT;

CREATE TABLE batch_execution_leases (
    batch_execution_id TEXT PRIMARY KEY NOT NULL
        REFERENCES batch_executions(id) ON DELETE CASCADE,
    worker_id TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT;
