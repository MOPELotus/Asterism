CREATE TABLE execution_parent_batch_snapshots (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    execution_attempt_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    authority_type TEXT NOT NULL CHECK (length(authority_type) BETWEEN 1 AND 96),
    authority_digest BLOB NOT NULL CHECK (length(authority_digest) = 32),
    authority_secret_blob_id TEXT NOT NULL UNIQUE REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    batch_type TEXT NOT NULL CHECK (length(batch_type) BETWEEN 1 AND 96),
    batch_digest BLOB NOT NULL CHECK (length(batch_digest) = 32),
    batch_secret_blob_id TEXT NOT NULL UNIQUE REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    bound_at TEXT NOT NULL,
    UNIQUE (execution_id, execution_attempt_id),
    FOREIGN KEY (execution_id, execution_attempt_id)
        REFERENCES execution_attempts(execution_id, id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_execution_parent_batch_snapshots_attempt
    ON execution_parent_batch_snapshots (execution_id, execution_attempt_id);
