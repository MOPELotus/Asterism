CREATE TABLE execution_mutation_stage_outputs (
    execution_id TEXT NOT NULL,
    execution_attempt_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 100000),
    provider_id TEXT NOT NULL,
    output_type TEXT NOT NULL CHECK (length(output_type) BETWEEN 1 AND 96),
    output_digest BLOB NOT NULL CHECK (length(output_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    stored_at TEXT NOT NULL,
    PRIMARY KEY (execution_id, execution_attempt_id, ordinal),
    FOREIGN KEY (execution_id, execution_attempt_id, ordinal)
        REFERENCES execution_atomic_mutations(
            execution_id,
            execution_attempt_id,
            ordinal
        ) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_execution_mutation_stage_outputs_attempt
    ON execution_mutation_stage_outputs (execution_id, execution_attempt_id, ordinal);
