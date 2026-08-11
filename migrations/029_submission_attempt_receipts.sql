CREATE TABLE submission_attempt_receipts (
    execution_attempt_id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL,
    submission_draft_id TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    receipt_bytes INTEGER NOT NULL CHECK (receipt_bytes BETWEEN 1 AND 65536),
    received_at TEXT NOT NULL,
    UNIQUE (execution_id, execution_attempt_id),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT,
    FOREIGN KEY (submission_draft_id)
        REFERENCES submission_drafts(id)
        ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_submission_attempt_receipts_execution
    ON submission_attempt_receipts (execution_id, received_at DESC);
