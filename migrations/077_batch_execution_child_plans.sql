CREATE TABLE batch_execution_child_plans (
    batch_execution_id TEXT NOT NULL,
    batch_execution_attempt_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position BETWEEN 1 AND 8192),
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
    remote_task_id_digest BLOB NOT NULL CHECK (length(remote_task_id_digest) = 32),
    provider_id TEXT NOT NULL,
    calls_json TEXT NOT NULL CHECK (length(calls_json) BETWEEN 5 AND 2048),
    artifact_type TEXT NOT NULL CHECK (length(artifact_type) BETWEEN 1 AND 96),
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    artifact_payload_json TEXT NOT NULL CHECK (
        length(artifact_payload_json) BETWEEN 2 AND 65536
    ),
    sequence_type TEXT NOT NULL CHECK (length(sequence_type) BETWEEN 1 AND 96),
    sequence_digest BLOB NOT NULL CHECK (length(sequence_digest) = 32),
    materialized_at TEXT NOT NULL,
    PRIMARY KEY (batch_execution_id, position),
    UNIQUE (batch_execution_id, task_id),
    UNIQUE (batch_execution_id, artifact_digest),
    UNIQUE (batch_execution_id, sequence_digest),
    FOREIGN KEY (batch_execution_id, batch_execution_attempt_id)
        REFERENCES batch_execution_parent_snapshots(
            batch_execution_id,
            batch_execution_attempt_id
        ) ON DELETE RESTRICT
) STRICT;

CREATE TABLE batch_execution_child_plan_phases (
    batch_execution_id TEXT NOT NULL,
    child_position INTEGER NOT NULL,
    phase_position INTEGER NOT NULL CHECK (phase_position BETWEEN 1 AND 32),
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
    PRIMARY KEY (batch_execution_id, child_position, phase_position),
    UNIQUE (batch_execution_id, child_position, operation_type),
    FOREIGN KEY (batch_execution_id, child_position)
        REFERENCES batch_execution_child_plans(batch_execution_id, position)
        ON DELETE CASCADE
) STRICT;
