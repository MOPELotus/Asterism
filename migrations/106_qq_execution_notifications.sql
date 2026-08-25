ALTER TABLE qq_formal_notification_deliveries RENAME TO qq_notification_deliveries_v105;

CREATE TABLE qq_formal_notification_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    execution_id TEXT,
    user_id TEXT NOT NULL,
    qq INTEGER NOT NULL,
    notification_kind TEXT NOT NULL CHECK (notification_kind IN (
        'confirmation_due', 'deadline_missed', 'execution_succeeded', 'execution_failed'
    )),
    deduplication_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'claimed', 'delivered', 'retry')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimed_at TEXT,
    next_attempt_at TEXT,
    delivered_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(user_id, notification_kind, deduplication_key),
    FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY(execution_id) REFERENCES executions(id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

INSERT INTO qq_formal_notification_deliveries
    (id, task_id, execution_id, user_id, qq, notification_kind, deduplication_key,
     state, attempts, claimed_at, next_attempt_at, delivered_at, last_error,
     created_at, updated_at)
SELECT id, task_id, NULL, user_id, qq, notification_kind, task_id,
       state, attempts, claimed_at, next_attempt_at, delivered_at, last_error,
       created_at, updated_at
FROM qq_notification_deliveries_v105;

DROP TABLE qq_notification_deliveries_v105;

CREATE INDEX idx_qq_execution_notification_claim
    ON qq_formal_notification_deliveries(state, next_attempt_at, updated_at);

CREATE INDEX idx_qq_execution_notification
    ON qq_formal_notification_deliveries(execution_id, notification_kind);
