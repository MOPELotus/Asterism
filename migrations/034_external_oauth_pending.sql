CREATE TABLE external_oauth_pending (
    auth_session_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_sessions(id) ON DELETE CASCADE,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    state_digest BLOB NOT NULL CHECK (length(state_digest) = 32),
    provider_context_digest BLOB NOT NULL CHECK (length(provider_context_digest) = 32),
    state_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (state_digest <> provider_context_digest)
) STRICT;

CREATE UNIQUE INDEX idx_external_oauth_provider_state
    ON external_oauth_pending (provider_id, state_digest);

CREATE INDEX idx_external_oauth_owner_updated
    ON external_oauth_pending (owner_user_id, updated_at DESC, auth_session_id DESC);

CREATE INDEX idx_external_oauth_account_updated
    ON external_oauth_pending (provider_account_id, updated_at DESC, auth_session_id DESC);
