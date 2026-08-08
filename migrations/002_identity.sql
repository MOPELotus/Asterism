CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL COLLATE NOCASE UNIQUE,
    password_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    roles_json TEXT NOT NULL,
    permissions_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE qq_identities (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    qq INTEGER NOT NULL UNIQUE CHECK (qq > 0),
    verified_at TEXT NOT NULL,
    is_primary INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1)),
    PRIMARY KEY (user_id, qq)
) STRICT;

CREATE UNIQUE INDEX idx_qq_identity_one_primary
    ON qq_identities (user_id)
    WHERE is_primary = 1;

CREATE TABLE web_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash BLOB NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT,
    last_used_at TEXT
) STRICT;
CREATE INDEX idx_web_sessions_user ON web_sessions (user_id, expires_at);

CREATE TABLE service_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    scopes_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    revoked_at TEXT,
    last_used_at TEXT
) STRICT;
