CREATE TABLE batch_execution_child_executions (
    batch_execution_id TEXT NOT NULL,
    child_position INTEGER NOT NULL,
    execution_id TEXT NOT NULL UNIQUE
        REFERENCES executions(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (batch_execution_id, child_position),
    FOREIGN KEY (batch_execution_id, child_position)
        REFERENCES batch_execution_child_plans(batch_execution_id, position)
        ON DELETE RESTRICT
) STRICT;
