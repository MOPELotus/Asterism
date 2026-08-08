CREATE TABLE auth_bootstrap_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    provider_account_id TEXT REFERENCES provider_accounts(id) ON DELETE CASCADE,
    purpose_json TEXT NOT NULL,
    required_recipe_version INTEGER NOT NULL CHECK (required_recipe_version >= 1),
    state_json TEXT NOT NULL,
    pairing_token_hash BLOB CHECK (
        pairing_token_hash IS NULL OR length(pairing_token_hash) = 32
    ),
    access_token_hash BLOB CHECK (
        access_token_hash IS NULL OR length(access_token_hash) = 32
    ),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    expires_at TEXT NOT NULL,
    claimed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX idx_auth_bootstrap_pairing_token
    ON auth_bootstrap_sessions (pairing_token_hash)
    WHERE pairing_token_hash IS NOT NULL;

CREATE UNIQUE INDEX idx_auth_bootstrap_access_token
    ON auth_bootstrap_sessions (access_token_hash)
    WHERE access_token_hash IS NOT NULL;

CREATE INDEX idx_auth_bootstrap_owner_updated
    ON auth_bootstrap_sessions (owner_user_id, updated_at DESC, id DESC);

CREATE INDEX idx_auth_bootstrap_account_updated
    ON auth_bootstrap_sessions (provider_account_id, updated_at DESC, id DESC)
    WHERE provider_account_id IS NOT NULL;
