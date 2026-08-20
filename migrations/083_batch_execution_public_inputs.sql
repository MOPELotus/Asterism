CREATE TABLE batch_execution_public_inputs (
    batch_execution_id TEXT PRIMARY KEY NOT NULL
        REFERENCES batch_executions(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    input_type TEXT NOT NULL CHECK (length(input_type) BETWEEN 1 AND 96),
    input_digest BLOB NOT NULL CHECK (length(input_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    bound_at TEXT NOT NULL
) STRICT;
