ALTER TABLE execution_atomic_mutations RENAME TO execution_atomic_mutations_legacy;

CREATE TABLE execution_atomic_mutations (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 100000),
    scheduler_job_id TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE RESTRICT,
    worker_id TEXT NOT NULL CHECK (length(worker_id) BETWEEN 1 AND 256),
    operation_type TEXT NOT NULL CHECK (length(operation_type) BETWEEN 1 AND 96),
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    response_digest BLOB CHECK (response_digest IS NULL OR length(response_digest) = 32),
    accepted INTEGER CHECK (accepted IS NULL OR accepted IN (0, 1)),
    verification_digest BLOB CHECK (
        verification_digest IS NULL OR length(verification_digest) = 32
    ),
    verified INTEGER CHECK (verified IS NULL OR verified IN (0, 1)),
    issued_at TEXT NOT NULL,
    received_at TEXT,
    verified_at TEXT,
    PRIMARY KEY (execution_id, execution_attempt_id, ordinal),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT,
    CHECK (
        (response_digest IS NULL AND accepted IS NULL AND received_at IS NULL)
        OR (response_digest IS NOT NULL AND accepted IS NOT NULL AND received_at IS NOT NULL
            AND received_at >= issued_at)
    ),
    CHECK (
        (verification_digest IS NULL AND verified IS NULL AND verified_at IS NULL)
        OR (verification_digest IS NOT NULL AND verified IS NOT NULL AND verified_at IS NOT NULL
            AND response_digest IS NOT NULL AND accepted = 1 AND received_at IS NOT NULL
            AND verified_at >= received_at)
    )
) STRICT;

INSERT INTO execution_atomic_mutations (
    execution_id, execution_attempt_id, ordinal, scheduler_job_id, worker_id,
    operation_type, request_digest, response_digest, accepted, issued_at, received_at
)
SELECT execution_id, execution_attempt_id, ordinal, scheduler_job_id, worker_id,
       operation_type, request_digest, response_digest, accepted, issued_at, received_at
FROM execution_atomic_mutations_legacy;

DROP TABLE execution_atomic_mutations_legacy;

CREATE INDEX idx_execution_atomic_mutations_sequence
    ON execution_atomic_mutations (execution_id, execution_attempt_id, ordinal);
