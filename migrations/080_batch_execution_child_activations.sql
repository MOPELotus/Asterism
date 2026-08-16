CREATE TABLE batch_execution_child_activations (
    batch_execution_id TEXT NOT NULL,
    child_position INTEGER NOT NULL,
    execution_id TEXT NOT NULL UNIQUE
        REFERENCES executions(id) ON DELETE RESTRICT,
    scheduler_job_id TEXT NOT NULL UNIQUE
        REFERENCES scheduled_jobs(id) ON DELETE RESTRICT,
    activated_at TEXT NOT NULL,
    PRIMARY KEY (batch_execution_id, child_position),
    FOREIGN KEY (batch_execution_id, child_position)
        REFERENCES batch_execution_child_executions(batch_execution_id, child_position)
        ON DELETE RESTRICT
) STRICT;
