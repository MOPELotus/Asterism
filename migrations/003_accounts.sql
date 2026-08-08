CREATE TABLE secret_blobs (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL,
    key_id TEXT NOT NULL,
    nonce BLOB NOT NULL,
    encrypted_data BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE network_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    configuration_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE provider_accounts (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    tenant TEXT,
    auth_state_json TEXT NOT NULL,
    network_profile_id TEXT REFERENCES network_profiles(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (id, provider_id)
) STRICT;

CREATE INDEX idx_provider_accounts_owner
    ON provider_accounts (owner_user_id, provider_id);

CREATE TABLE provider_account_credentials (
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    secret_blob_id TEXT NOT NULL REFERENCES secret_blobs(id) ON DELETE CASCADE,
    credential_kind TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (provider_account_id, secret_blob_id)
) STRICT;

CREATE TABLE courses (
    id TEXT PRIMARY KEY NOT NULL,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    remote_id TEXT NOT NULL,
    title TEXT NOT NULL,
    term TEXT,
    teacher TEXT,
    remote_status TEXT,
    metadata_json TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    UNIQUE (provider_account_id, remote_id)
) STRICT;
