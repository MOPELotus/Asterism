CREATE TABLE execution_score_improvement_retake_confirmations (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL REFERENCES score_improvement_workflows(id) ON DELETE CASCADE,
    workflow_revision INTEGER NOT NULL CHECK (workflow_revision > 0),
    confirmed_by TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    confirmed_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_execution_score_improvement_retake_workflow
    ON execution_score_improvement_retake_confirmations
       (workflow_id, workflow_revision, confirmed_at, execution_id);
