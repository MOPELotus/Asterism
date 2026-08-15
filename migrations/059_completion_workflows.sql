CREATE TABLE strict_completion_workflows (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('disabled', 'active', 'attempt_running', 'completed', 'stopped')),
    workflow_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT,
    UNIQUE (task_id)
) STRICT;

CREATE INDEX idx_strict_completion_owner_state
    ON strict_completion_workflows (owner_user_id, state, updated_at DESC, id);

CREATE TABLE score_improvement_workflows (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('disabled', 'ready', 'attempt_running', 'finished', 'stopped')),
    workflow_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT,
    UNIQUE (task_id)
) STRICT;

CREATE INDEX idx_score_improvement_owner_state
    ON score_improvement_workflows (owner_user_id, state, updated_at DESC, id);
