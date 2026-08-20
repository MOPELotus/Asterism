CREATE TABLE course_enrollment_drafts (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL
        REFERENCES users(id) ON DELETE RESTRICT,
    provider_account_id TEXT NOT NULL
        REFERENCES provider_accounts(id) ON DELETE RESTRICT,
    provider_id TEXT NOT NULL CHECK (length(provider_id) BETWEEN 1 AND 64),
    artifact_type TEXT NOT NULL CHECK (length(artifact_type) BETWEEN 1 AND 96),
    remote_course_id TEXT NOT NULL CHECK (length(remote_course_id) BETWEEN 1 AND 512),
    remote_class_id TEXT NOT NULL CHECK (length(remote_class_id) BETWEEN 1 AND 512),
    preview_digest BLOB NOT NULL CHECK (length(preview_digest) = 32),
    preview_sanitized_json TEXT NOT NULL CHECK (json_valid(preview_sanitized_json)),
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    request_secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    UNIQUE (owner_user_id, provider_account_id, request_digest)
) STRICT;

CREATE INDEX idx_course_enrollment_drafts_owner_time
    ON course_enrollment_drafts (owner_user_id, created_at DESC, id);

CREATE INDEX idx_course_enrollment_drafts_account_target
    ON course_enrollment_drafts (
        provider_account_id,
        remote_course_id,
        remote_class_id,
        created_at DESC
    );
