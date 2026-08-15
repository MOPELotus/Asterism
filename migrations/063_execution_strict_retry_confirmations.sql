CREATE TABLE execution_strict_completion_retry_confirmations (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL REFERENCES strict_completion_workflows(id) ON DELETE CASCADE,
    workflow_revision INTEGER NOT NULL CHECK (workflow_revision > 0),
    confirmed_by TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    confirmed_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_execution_strict_retry_workflow
    ON execution_strict_completion_retry_confirmations
       (workflow_id, workflow_revision, confirmed_at, execution_id);
