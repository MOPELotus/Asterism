CREATE TABLE chaoxing_challenge_escalations (
    source_execution_id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'processing', 'retry', 'completed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TEXT,
    claimed_until TEXT,
    target_execution_id TEXT,
    last_error_code TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(source_execution_id) REFERENCES executions(id) ON DELETE CASCADE,
    FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY(owner_user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY(question_snapshot_id) REFERENCES question_snapshots(id) ON DELETE CASCADE,
    FOREIGN KEY(target_execution_id) REFERENCES executions(id) ON DELETE SET NULL
) STRICT;

CREATE INDEX idx_chaoxing_challenge_escalations_claim
    ON chaoxing_challenge_escalations(state, next_attempt_at, claimed_until, updated_at);
