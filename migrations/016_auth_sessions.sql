CREATE TABLE auth_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    method_json TEXT NOT NULL,
    state_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_auth_sessions_owner_updated
    ON auth_sessions (owner_user_id, updated_at DESC, id DESC);

CREATE INDEX idx_auth_sessions_account_updated
    ON auth_sessions (provider_account_id, updated_at DESC, id DESC);
