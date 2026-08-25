ALTER TABLE qq_formal_notification_deliveries RENAME TO qq_formal_notification_deliveries_v100;

CREATE TABLE qq_formal_notification_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    qq INTEGER NOT NULL,
    notification_kind TEXT NOT NULL CHECK (notification_kind IN ('confirmation_due', 'deadline_missed')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'claimed', 'delivered', 'retry')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimed_at TEXT,
    next_attempt_at TEXT,
    delivered_at TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(task_id, user_id, notification_kind),
    FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

INSERT INTO qq_formal_notification_deliveries
    (id, task_id, user_id, qq, notification_kind, state, attempts, claimed_at,
     next_attempt_at, delivered_at, last_error, created_at, updated_at)
SELECT id, task_id, user_id, qq, 'confirmation_due', state, attempts, claimed_at,
       next_attempt_at, delivered_at, last_error, created_at, updated_at
FROM qq_formal_notification_deliveries_v100;

DROP TABLE qq_formal_notification_deliveries_v100;

CREATE INDEX idx_qq_formal_notification_claim
    ON qq_formal_notification_deliveries(state, next_attempt_at, updated_at);
