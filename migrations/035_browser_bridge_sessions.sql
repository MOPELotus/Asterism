CREATE TABLE browser_bridge_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    provider_version TEXT NOT NULL,
    spec_version INTEGER NOT NULL CHECK (spec_version >= 1),
    spec_digest BLOB NOT NULL CHECK (length(spec_digest) = 32),
    spec_json TEXT NOT NULL,
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

CREATE UNIQUE INDEX idx_browser_bridge_pairing_token
    ON browser_bridge_sessions (pairing_token_hash)
    WHERE pairing_token_hash IS NOT NULL;

CREATE UNIQUE INDEX idx_browser_bridge_access_token
    ON browser_bridge_sessions (access_token_hash)
    WHERE access_token_hash IS NOT NULL;

CREATE INDEX idx_browser_bridge_owner_updated
    ON browser_bridge_sessions (owner_user_id, updated_at DESC, id DESC);

CREATE INDEX idx_browser_bridge_task_updated
    ON browser_bridge_sessions (task_id, updated_at DESC, id DESC);
