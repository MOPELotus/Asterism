CREATE TABLE qq_web_login_tickets (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    return_to TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    CHECK (length(token_hash) = 32),
    CHECK (substr(return_to, 1, 1) = '/' AND substr(return_to, 1, 2) <> '//'),
    CHECK (expires_at > created_at),
    CHECK (consumed_at IS NULL OR consumed_at >= created_at)
);

CREATE INDEX qq_web_login_tickets_active_idx
    ON qq_web_login_tickets (expires_at)
    WHERE consumed_at IS NULL;
