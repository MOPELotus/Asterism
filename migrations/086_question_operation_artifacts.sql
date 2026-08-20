CREATE TABLE question_read_operation_recovery_artifacts (
    attempt_id TEXT NOT NULL,
    operation_sequence INTEGER NOT NULL CHECK (operation_sequence > 0),
    provider_id TEXT NOT NULL,
    artifact_type TEXT NOT NULL,
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    stored_at TEXT NOT NULL,
    PRIMARY KEY (attempt_id, operation_sequence),
    FOREIGN KEY (attempt_id, operation_sequence)
        REFERENCES question_read_attempt_operations(attempt_id, sequence) ON DELETE CASCADE
) STRICT;

CREATE TABLE question_read_operation_results (
    attempt_id TEXT NOT NULL,
    operation_sequence INTEGER NOT NULL CHECK (operation_sequence > 0),
    provider_id TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    receipt_bytes INTEGER NOT NULL CHECK (receipt_bytes > 0 AND receipt_bytes <= 65536),
    artifact_type TEXT,
    artifact_digest BLOB CHECK (artifact_digest IS NULL OR length(artifact_digest) = 32),
    secret_blob_id TEXT UNIQUE REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (attempt_id, operation_sequence),
    FOREIGN KEY (attempt_id, operation_sequence)
        REFERENCES question_read_attempt_operations(attempt_id, sequence) ON DELETE CASCADE,
    CHECK (
        (artifact_type IS NULL AND artifact_digest IS NULL AND secret_blob_id IS NULL)
        OR (artifact_type IS NOT NULL AND artifact_digest IS NOT NULL AND secret_blob_id IS NOT NULL)
    )
) STRICT;

CREATE TABLE question_session_operation_recovery_artifacts (
    session_id TEXT NOT NULL,
    operation_sequence INTEGER NOT NULL CHECK (operation_sequence > 0),
    provider_id TEXT NOT NULL,
    artifact_type TEXT NOT NULL,
    artifact_digest BLOB NOT NULL CHECK (length(artifact_digest) = 32),
    secret_blob_id TEXT NOT NULL UNIQUE REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    stored_at TEXT NOT NULL,
    PRIMARY KEY (session_id, operation_sequence),
    FOREIGN KEY (session_id, operation_sequence)
        REFERENCES question_session_operations(session_id, sequence) ON DELETE CASCADE
) STRICT;

CREATE TABLE question_session_operation_results (
    session_id TEXT NOT NULL,
    operation_sequence INTEGER NOT NULL CHECK (operation_sequence > 0),
    provider_id TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    receipt_bytes INTEGER NOT NULL CHECK (receipt_bytes > 0 AND receipt_bytes <= 65536),
    artifact_type TEXT,
    artifact_digest BLOB CHECK (artifact_digest IS NULL OR length(artifact_digest) = 32),
    secret_blob_id TEXT UNIQUE REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (session_id, operation_sequence),
    FOREIGN KEY (session_id, operation_sequence)
        REFERENCES question_session_operations(session_id, sequence) ON DELETE CASCADE,
    CHECK (
        (artifact_type IS NULL AND artifact_digest IS NULL AND secret_blob_id IS NULL)
        OR (artifact_type IS NOT NULL AND artifact_digest IS NOT NULL AND secret_blob_id IS NOT NULL)
    )
) STRICT;
