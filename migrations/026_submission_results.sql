CREATE UNIQUE INDEX idx_submission_drafts_result_binding
    ON submission_drafts (id, question_snapshot_id, task_id, provider_id);

CREATE UNIQUE INDEX idx_executions_task_binding
    ON executions (id, task_id);

CREATE UNIQUE INDEX idx_execution_attempts_execution_binding
    ON execution_attempts (execution_id, id);

CREATE TABLE submission_results (
    id TEXT PRIMARY KEY NOT NULL,
    submission_draft_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL UNIQUE,
    task_id TEXT NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('confirmed', 'rejected', 'execution_failed', 'inconclusive')
    ),
    receipt_json TEXT,
    receipt_bytes INTEGER CHECK (receipt_bytes BETWEEN 1 AND 65536),
    verification_json TEXT NOT NULL,
    verification_bytes INTEGER NOT NULL CHECK (verification_bytes BETWEEN 1 AND 8388608),
    created_at TEXT NOT NULL,
    CHECK ((receipt_json IS NULL) = (receipt_bytes IS NULL)),
    FOREIGN KEY (submission_draft_id, question_snapshot_id, task_id, provider_id)
        REFERENCES submission_drafts(id, question_snapshot_id, task_id, provider_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (execution_id, task_id)
        REFERENCES executions(id, task_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_submission_results_draft_time
    ON submission_results (submission_draft_id, created_at DESC, id DESC);

CREATE INDEX idx_submission_results_execution
    ON submission_results (execution_id, execution_attempt_id);
