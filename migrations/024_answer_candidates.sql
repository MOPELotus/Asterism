CREATE TABLE answer_candidates (
    id TEXT PRIMARY KEY NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (
        source IN ('manual', 'local_cache', 'provider_native', 'external_bank', 'other')
    ),
    candidate_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (question_snapshot_id, question_id)
        REFERENCES question_snapshot_items(snapshot_id, question_id)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_answer_candidates_snapshot_question
    ON answer_candidates (question_snapshot_id, question_id, source, created_at, id);
