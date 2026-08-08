CREATE INDEX idx_web_sessions_active
    ON web_sessions (token_hash, expires_at)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_service_tokens_active
    ON service_tokens (token_hash, expires_at)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_service_tokens_owner
    ON service_tokens (owner_user_id, created_at);
