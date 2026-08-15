CREATE TABLE execution_atomic_mutation_sequence_plans (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    scheduler_job_id TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE RESTRICT,
    worker_id TEXT NOT NULL CHECK (length(worker_id) BETWEEN 1 AND 256),
    plan_digest BLOB NOT NULL CHECK (length(plan_digest) = 32),
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    sequence_type TEXT NOT NULL CHECK (length(sequence_type) BETWEEN 1 AND 96),
    phase_count INTEGER NOT NULL CHECK (phase_count BETWEEN 1 AND 32),
    prepared_at TEXT NOT NULL,
    PRIMARY KEY (execution_id, execution_attempt_id),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT
) STRICT;

CREATE TABLE execution_atomic_mutation_sequence_phases (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position BETWEEN 1 AND 32),
    operation_type TEXT NOT NULL CHECK (length(operation_type) BETWEEN 1 AND 96),
    minimum_occurrences INTEGER NOT NULL CHECK (
        minimum_occurrences BETWEEN 0 AND 100000
    ),
    maximum_occurrences INTEGER NOT NULL CHECK (
        maximum_occurrences BETWEEN 0 AND 100000
        AND minimum_occurrences <= maximum_occurrences
    ),
    stop_repeating_after_rejection INTEGER NOT NULL CHECK (
        stop_repeating_after_rejection IN (0, 1)
    ),
    advance_condition TEXT NOT NULL CHECK (
        advance_condition IN (
            'maximum_reached',
            'accepted_maximum_reached',
            'rejected_or_maximum_reached'
        )
    ),
    required_observation_type TEXT CHECK (
        required_observation_type IS NULL
        OR length(required_observation_type) BETWEEN 1 AND 96
    ),
    PRIMARY KEY (execution_id, execution_attempt_id, position),
    UNIQUE (execution_id, execution_attempt_id, operation_type),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_atomic_mutation_sequence_plans(execution_id, execution_attempt_id)
        ON DELETE RESTRICT
) STRICT;

CREATE TABLE execution_atomic_mutation_sequence_observations (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    phase_position INTEGER NOT NULL,
    observation_type TEXT NOT NULL CHECK (length(observation_type) BETWEEN 1 AND 96),
    observation_digest BLOB NOT NULL CHECK (length(observation_digest) = 32),
    observed_at TEXT NOT NULL,
    PRIMARY KEY (execution_id, execution_attempt_id, phase_position),
    FOREIGN KEY (execution_id, execution_attempt_id, phase_position)
        REFERENCES execution_atomic_mutation_sequence_phases(
            execution_id, execution_attempt_id, position
        )
        ON DELETE RESTRICT
) STRICT;
