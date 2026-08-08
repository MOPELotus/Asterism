CREATE TABLE tasks (
    id TEXT PRIMARY KEY NOT NULL,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    course_id TEXT REFERENCES courses(id) ON DELETE SET NULL,
    remote_id TEXT NOT NULL,
    remote_fingerprint TEXT NOT NULL,
    source_type TEXT NOT NULL,
    assessment_class TEXT NOT NULL,
    title TEXT NOT NULL,
    remote_state TEXT NOT NULL,
    orchestration_state TEXT NOT NULL,
    opens_at TEXT,
    due_at TEXT,
    closes_at TEXT,
    discovered_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    latest_snapshot_id TEXT,
    capabilities_json TEXT NOT NULL,
    UNIQUE (provider_account_id, source_type, remote_id)
) STRICT;

CREATE INDEX idx_tasks_course_state ON tasks (course_id, remote_state);
CREATE INDEX idx_tasks_orchestration_state ON tasks (orchestration_state, due_at);

CREATE TABLE task_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    captured_at TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    normalized_json TEXT NOT NULL,
    remote_raw_sanitized_json TEXT NOT NULL
) STRICT;

CREATE INDEX idx_task_snapshots_task_time
    ON task_snapshots (task_id, captured_at DESC);

CREATE TABLE task_diffs (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    from_snapshot_id TEXT REFERENCES task_snapshots(id) ON DELETE SET NULL,
    to_snapshot_id TEXT NOT NULL REFERENCES task_snapshots(id) ON DELETE CASCADE,
    changes_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE automation_plans (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    coverage_json TEXT NOT NULL,
    inheritance_mode TEXT NOT NULL,
    execution_policy TEXT NOT NULL,
    billing_policy_json TEXT NOT NULL,
    schedule_policy_json TEXT NOT NULL,
    notification_policy_json TEXT NOT NULL,
    effective_from TEXT NOT NULL,
    expires_at TEXT,
    priority INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_automation_plans_owner_status
    ON automation_plans (owner_user_id, status, priority DESC);
