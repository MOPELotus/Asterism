ALTER TABLE question_snapshots
ADD COLUMN group_count INTEGER NOT NULL DEFAULT 0
    CHECK (group_count BETWEEN 0 AND 1024);

CREATE TABLE question_snapshot_groups (
    snapshot_id TEXT NOT NULL REFERENCES question_snapshots(id) ON DELETE CASCADE,
    group_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 1024),
    remote_group_id TEXT,
    group_json TEXT NOT NULL CHECK (length(group_json) BETWEEN 2 AND 16777216),
    PRIMARY KEY (snapshot_id, ordinal),
    UNIQUE (snapshot_id, group_id),
    UNIQUE (snapshot_id, remote_group_id)
) STRICT;

CREATE INDEX idx_question_snapshot_groups_identity
    ON question_snapshot_groups (group_id);
