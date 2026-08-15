CREATE TABLE execution_atomic_mutation_plans (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    scheduler_job_id TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE RESTRICT,
    worker_id TEXT NOT NULL CHECK (length(worker_id) BETWEEN 1 AND 256),
    plan_digest BLOB NOT NULL CHECK (length(plan_digest) = 32),
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    step_count INTEGER NOT NULL CHECK (step_count BETWEEN 1 AND 100000),
    prepared_at TEXT NOT NULL,
    PRIMARY KEY (execution_id, execution_attempt_id),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT
) STRICT;

CREATE TABLE execution_atomic_mutation_plan_steps (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 100000),
    operation_type TEXT NOT NULL CHECK (length(operation_type) BETWEEN 1 AND 96),
    planned_request_digest BLOB CHECK (
        planned_request_digest IS NULL OR length(planned_request_digest) = 32
    ),
    bound_request_digest BLOB CHECK (
        bound_request_digest IS NULL OR length(bound_request_digest) = 32
    ),
    PRIMARY KEY (execution_id, execution_attempt_id, ordinal),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_atomic_mutation_plans(execution_id, execution_attempt_id)
        ON DELETE RESTRICT,
    CHECK (planned_request_digest IS NULL OR bound_request_digest IS NULL)
) STRICT;

CREATE TABLE execution_atomic_mutation_plan_dependencies (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    dependency_ordinal INTEGER NOT NULL CHECK (
        dependency_ordinal BETWEEN 1 AND 99999 AND dependency_ordinal < ordinal
    ),
    PRIMARY KEY (execution_id, execution_attempt_id, ordinal, dependency_ordinal),
    FOREIGN KEY (execution_id, execution_attempt_id, ordinal)
        REFERENCES execution_atomic_mutation_plan_steps(
            execution_id, execution_attempt_id, ordinal
        )
        ON DELETE RESTRICT,
    FOREIGN KEY (execution_id, execution_attempt_id, dependency_ordinal)
        REFERENCES execution_atomic_mutation_plan_steps(
            execution_id, execution_attempt_id, ordinal
        )
        ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_execution_atomic_mutation_plan_dependencies
    ON execution_atomic_mutation_plan_dependencies (
        execution_id, execution_attempt_id, ordinal, dependency_ordinal
    );
