CREATE TABLE task_action_receipts (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    action TEXT NOT NULL CHECK (action IN ('approve', 'cancel', 'delay', 'ignore')),
    idempotency_key TEXT NOT NULL,
    delayed_until TEXT,
    result_task_state TEXT NOT NULL,
    affected_execution_id TEXT REFERENCES executions(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    UNIQUE (owner_user_id, idempotency_key),
    CHECK ((action = 'delay') = (delayed_until IS NOT NULL))
) STRICT;

CREATE INDEX idx_task_action_receipts_task_time
    ON task_action_receipts (task_id, created_at DESC, id);
