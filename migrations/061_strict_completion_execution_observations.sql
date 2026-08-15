CREATE TABLE strict_completion_execution_observations (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    execution_attempt_id TEXT NOT NULL UNIQUE
        REFERENCES execution_attempts(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL
        REFERENCES strict_completion_workflows(id) ON DELETE CASCADE,
    workflow_attempt_no INTEGER CHECK (workflow_attempt_no IS NULL OR workflow_attempt_no > 0),
    completion_outcome TEXT CHECK (completion_outcome IS NULL OR completion_outcome IN ('completed', 'passed')),
    diagnosis TEXT CHECK (
        diagnosis IS NULL OR diagnosis IN (
            'score_below_threshold',
            'duration_insufficient',
            'required_children_pending',
            'prerequisite_locked',
            'teacher_review_pending',
            'human_action_required',
            'unsupported_capability',
            'protocol_drift',
            'attempt_limit_reached',
            'window_closed',
            'remote_unknown'
        )
    ),
    observed_at TEXT NOT NULL,
    CHECK (
        (completion_outcome IS NOT NULL AND diagnosis IS NULL) OR
        (completion_outcome IS NULL AND diagnosis IS NOT NULL)
    )
) STRICT;

CREATE INDEX idx_strict_completion_execution_workflow
    ON strict_completion_execution_observations (workflow_id, observed_at, execution_id);
