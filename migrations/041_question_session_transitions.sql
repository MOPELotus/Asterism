CREATE TABLE question_session_transitions (
    previous_session_id TEXT NOT NULL,
    operation_sequence INTEGER NOT NULL CHECK (operation_sequence > 0),
    execution_id TEXT NOT NULL,
    next_session_id TEXT NOT NULL UNIQUE
        REFERENCES question_sessions(id) ON DELETE RESTRICT,
    next_question_snapshot_id TEXT NOT NULL UNIQUE
        REFERENCES question_snapshots(id) ON DELETE RESTRICT,
    transitioned_at TEXT NOT NULL,
    PRIMARY KEY (previous_session_id, operation_sequence),
    FOREIGN KEY (previous_session_id, operation_sequence)
        REFERENCES question_session_operations(session_id, sequence)
        ON DELETE RESTRICT,
    FOREIGN KEY (previous_session_id, execution_id)
        REFERENCES question_session_continuations(session_id, execution_id)
        ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_question_session_transitions_execution
    ON question_session_transitions (execution_id, transitioned_at);
