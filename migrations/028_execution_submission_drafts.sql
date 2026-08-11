ALTER TABLE executions
    ADD COLUMN submission_draft_id TEXT
        REFERENCES submission_drafts(id) ON DELETE RESTRICT;

CREATE UNIQUE INDEX idx_executions_submission_draft
    ON executions (submission_draft_id)
    WHERE submission_draft_id IS NOT NULL;
