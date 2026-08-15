CREATE TABLE execution_atomic_mutation_sequence_observations_071_backup AS
SELECT
    execution_id,
    execution_attempt_id,
    phase_position,
    observation_type,
    observation_digest,
    observed_at
FROM execution_atomic_mutation_sequence_observations;

DROP TABLE execution_atomic_mutation_sequence_observations;

CREATE TABLE execution_atomic_mutation_sequence_phases_071_new (
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
            'accepted_or_maximum_reached',
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

INSERT INTO execution_atomic_mutation_sequence_phases_071_new (
    execution_id,
    execution_attempt_id,
    position,
    operation_type,
    minimum_occurrences,
    maximum_occurrences,
    stop_repeating_after_rejection,
    advance_condition,
    required_observation_type
)
SELECT
    execution_id,
    execution_attempt_id,
    position,
    operation_type,
    minimum_occurrences,
    maximum_occurrences,
    stop_repeating_after_rejection,
    advance_condition,
    required_observation_type
FROM execution_atomic_mutation_sequence_phases;

DROP TABLE execution_atomic_mutation_sequence_phases;

ALTER TABLE execution_atomic_mutation_sequence_phases_071_new
    RENAME TO execution_atomic_mutation_sequence_phases;

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

INSERT INTO execution_atomic_mutation_sequence_observations (
    execution_id,
    execution_attempt_id,
    phase_position,
    observation_type,
    observation_digest,
    observed_at
)
SELECT
    execution_id,
    execution_attempt_id,
    phase_position,
    observation_type,
    observation_digest,
    observed_at
FROM execution_atomic_mutation_sequence_observations_071_backup;

DROP TABLE execution_atomic_mutation_sequence_observations_071_backup;
