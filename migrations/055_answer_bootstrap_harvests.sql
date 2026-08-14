CREATE TABLE answer_bootstrap_harvests (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    schedule_id TEXT NOT NULL UNIQUE REFERENCES scheduled_jobs(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'running', 'paused', 'completed', 'failed', 'cancelled')
    ),
    scanned_task_count INTEGER NOT NULL DEFAULT 0 CHECK (scanned_task_count >= 0),
    total_task_count INTEGER CHECK (total_task_count IS NULL OR total_task_count >= 0),
    watermark_sanitized_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE (provider_account_id, generation),
    CHECK (total_task_count IS NULL OR scanned_task_count <= total_task_count),
    CHECK (
        (state = 'pending' AND started_at IS NULL AND completed_at IS NULL
             AND scanned_task_count = 0)
        OR (state IN ('running', 'paused') AND started_at IS NOT NULL
             AND completed_at IS NULL)
        OR (state IN ('completed', 'failed', 'cancelled') AND started_at IS NOT NULL
             AND completed_at IS NOT NULL)
    ),
    FOREIGN KEY (provider_account_id, provider_id)
        REFERENCES provider_accounts(id, provider_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_answer_bootstrap_harvests_owner_state
    ON answer_bootstrap_harvests (owner_user_id, state, created_at, id);

CREATE TRIGGER trg_answer_bootstrap_harvest_delete_job
AFTER DELETE ON answer_bootstrap_harvests
BEGIN
    DELETE FROM scheduled_jobs WHERE id = OLD.schedule_id;
END;
