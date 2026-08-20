CREATE TABLE execution_invocation_drafts (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    course_id TEXT REFERENCES courses(id) ON DELETE RESTRICT,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL CHECK (length(provider_version) BETWEEN 1 AND 128),
    requested_capabilities_json TEXT NOT NULL,
    submission_draft_id TEXT REFERENCES submission_drafts(id) ON DELETE RESTRICT,
    private_input_type TEXT NOT NULL CHECK (length(private_input_type) BETWEEN 1 AND 128),
    private_input_digest BLOB NOT NULL CHECK (length(private_input_digest) = 32),
    private_input_secret_blob_id TEXT NOT NULL UNIQUE
        REFERENCES secret_blobs(id) ON DELETE RESTRICT,
    plan_artifact_type TEXT NOT NULL CHECK (length(plan_artifact_type) BETWEEN 1 AND 96),
    plan_artifact_digest BLOB NOT NULL CHECK (length(plan_artifact_digest) = 32),
    plan_artifact_payload_json TEXT NOT NULL
        CHECK (length(plan_artifact_payload_json) BETWEEN 2 AND 65536),
    idempotency_scope TEXT NOT NULL CHECK (length(idempotency_scope) BETWEEN 1 AND 256),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 256),
    created_at TEXT NOT NULL,
    claimed_execution_id TEXT UNIQUE REFERENCES executions(id) ON DELETE RESTRICT,
    claimed_at TEXT,
    CHECK (
        (claimed_execution_id IS NULL AND claimed_at IS NULL)
        OR (claimed_execution_id IS NOT NULL AND claimed_at IS NOT NULL)
    ),
    UNIQUE (idempotency_scope, idempotency_key)
) STRICT;

CREATE INDEX idx_execution_invocation_drafts_owner_task
    ON execution_invocation_drafts (owner_user_id, task_id, created_at DESC, id DESC);

CREATE INDEX idx_execution_invocation_drafts_unclaimed
    ON execution_invocation_drafts (task_id, created_at DESC)
    WHERE claimed_execution_id IS NULL;
