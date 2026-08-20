CREATE TABLE batch_execution_materialization_bindings (
    batch_execution_id TEXT PRIMARY KEY NOT NULL
        REFERENCES batch_executions(id) ON DELETE CASCADE,
    batch_execution_attempt_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    binding_type TEXT NOT NULL CHECK (length(binding_type) BETWEEN 1 AND 96),
    binding_digest BLOB NOT NULL CHECK (length(binding_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    bound_at TEXT NOT NULL,
    UNIQUE (batch_execution_id, batch_execution_attempt_id),
    FOREIGN KEY (batch_execution_id, batch_execution_attempt_id)
        REFERENCES batch_execution_attempts(batch_execution_id, id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_batch_execution_materialization_bindings_attempt
    ON batch_execution_materialization_bindings (
        batch_execution_id,
        batch_execution_attempt_id
    );
