ALTER TABLE provider_account_credentials
    ADD COLUMN session_kind TEXT NOT NULL DEFAULT 'provider_specific';

ALTER TABLE provider_account_credentials
    ADD COLUMN acquired_via TEXT NOT NULL DEFAULT 'manual_import';

ALTER TABLE provider_account_credentials
    ADD COLUMN expires_at TEXT;

ALTER TABLE provider_account_credentials
    ADD COLUMN updated_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00.000000000Z';

UPDATE provider_account_credentials
SET updated_at = created_at;

CREATE UNIQUE INDEX idx_provider_account_credential_kind
    ON provider_account_credentials (provider_account_id, credential_kind);
