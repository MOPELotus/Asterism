CREATE UNIQUE INDEX idx_question_snapshots_identity_binding
    ON question_snapshots (id, task_id, provider_id);

CREATE UNIQUE INDEX idx_answer_candidates_snapshot_identity
    ON answer_candidates (question_snapshot_id, id, question_id);

CREATE TABLE submission_drafts (
    id TEXT PRIMARY KEY NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    payload_preview_json TEXT NOT NULL,
    preview_bytes INTEGER NOT NULL CHECK (preview_bytes BETWEEN 1 AND 8388608),
    item_count INTEGER NOT NULL CHECK (item_count BETWEEN 1 AND 5000),
    created_at TEXT NOT NULL,
    UNIQUE (id, question_snapshot_id),
    FOREIGN KEY (question_snapshot_id, task_id, provider_id)
        REFERENCES question_snapshots(id, task_id, provider_id)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_submission_drafts_snapshot_time
    ON submission_drafts (question_snapshot_id, created_at DESC, id DESC);

CREATE TABLE submission_draft_items (
    draft_id TEXT NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    answer_candidate_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position BETWEEN 1 AND 100000),
    PRIMARY KEY (draft_id, position),
    UNIQUE (draft_id, question_id),
    UNIQUE (draft_id, answer_candidate_id),
    FOREIGN KEY (draft_id, question_snapshot_id)
        REFERENCES submission_drafts(id, question_snapshot_id)
        ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id, answer_candidate_id, question_id)
        REFERENCES answer_candidates(question_snapshot_id, id, question_id)
        ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_submission_draft_items_candidate
    ON submission_draft_items (answer_candidate_id);
