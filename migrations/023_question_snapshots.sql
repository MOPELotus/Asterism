CREATE TABLE question_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    question_count INTEGER NOT NULL CHECK (question_count BETWEEN 0 AND 5000),
    total_bytes INTEGER NOT NULL CHECK (total_bytes BETWEEN 0 AND 16777216)
) STRICT;

CREATE INDEX idx_question_snapshots_task_time
    ON question_snapshots (task_id, captured_at DESC, id DESC);

CREATE TABLE question_snapshot_items (
    snapshot_id TEXT NOT NULL REFERENCES question_snapshots(id) ON DELETE CASCADE,
    question_id TEXT NOT NULL,
    remote_question_id TEXT,
    position INTEGER NOT NULL CHECK (position BETWEEN 1 AND 100000),
    question_json TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, position),
    UNIQUE (snapshot_id, question_id),
    UNIQUE (snapshot_id, remote_question_id)
) STRICT;

CREATE INDEX idx_question_snapshot_items_question
    ON question_snapshot_items (question_id);
