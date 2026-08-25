ALTER TABLE submission_draft_items RENAME TO submission_draft_items_legacy;

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

INSERT INTO submission_draft_items
    (draft_id, question_snapshot_id, question_id, answer_candidate_id, position)
SELECT draft_id, question_snapshot_id, question_id, answer_candidate_id, position
FROM submission_draft_items_legacy;

DROP TABLE submission_draft_items_legacy;

CREATE INDEX idx_submission_draft_items_candidate
    ON submission_draft_items (answer_candidate_id);
