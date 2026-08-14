CREATE UNIQUE INDEX idx_provider_accounts_question_session_identity
    ON provider_accounts (id, owner_user_id, provider_id);

CREATE UNIQUE INDEX idx_tasks_question_session_identity
    ON tasks (id, provider_account_id);

CREATE TABLE question_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    question_snapshot_id TEXT NOT NULL UNIQUE,
    artifact_type TEXT NOT NULL,
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    state TEXT NOT NULL CHECK (
        state IN ('active', 'claimed', 'consumed', 'cancelled', 'expired')
    ),
    execution_id TEXT UNIQUE REFERENCES executions(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 3),
    expires_at TEXT NOT NULL,
    claimed_at TEXT,
    closed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (owner_user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (provider_account_id, owner_user_id, provider_id)
        REFERENCES provider_accounts(id, owner_user_id, provider_id)
        ON DELETE CASCADE,
    FOREIGN KEY (task_id, provider_account_id)
        REFERENCES tasks(id, provider_account_id)
        ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id, task_id, provider_id)
        REFERENCES question_snapshots(id, task_id, provider_id)
        ON DELETE CASCADE,
    CHECK (created_at < expires_at),
    CHECK (updated_at >= created_at),
    CHECK (
        (state = 'active' AND execution_id IS NULL AND claimed_at IS NULL
            AND closed_at IS NULL AND revision = 1 AND updated_at = created_at)
        OR (state = 'claimed' AND execution_id IS NOT NULL AND claimed_at IS NOT NULL
            AND closed_at IS NULL AND revision = 2 AND updated_at = claimed_at)
        OR (state = 'consumed' AND execution_id IS NOT NULL AND claimed_at IS NOT NULL
            AND closed_at IS NOT NULL AND revision = 3 AND updated_at = closed_at)
        OR (state = 'cancelled' AND closed_at IS NOT NULL AND (
            (execution_id IS NULL AND claimed_at IS NULL AND revision = 2)
            OR (execution_id IS NOT NULL AND claimed_at IS NOT NULL AND revision = 3)
        ) AND updated_at = closed_at)
        OR (state = 'expired' AND execution_id IS NULL AND claimed_at IS NULL
            AND closed_at IS NOT NULL AND revision = 2 AND updated_at = closed_at
            AND closed_at >= expires_at)
    )
) STRICT;

CREATE INDEX idx_question_sessions_owner_time
    ON question_sessions (owner_user_id, created_at DESC, id DESC);

CREATE INDEX idx_question_sessions_task_state
    ON question_sessions (task_id, state, expires_at);
