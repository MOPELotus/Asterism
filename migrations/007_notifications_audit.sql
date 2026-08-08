CREATE TABLE notification_bindings (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    privacy_mode TEXT NOT NULL,
    configuration_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (channel, target_ref)
) STRICT;

CREATE TABLE notification_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    binding_id TEXT NOT NULL REFERENCES notification_bindings(id) ON DELETE CASCADE,
    deduplication_key TEXT,
    payload_sanitized_json TEXT NOT NULL,
    state TEXT NOT NULL,
    attempted_at TEXT,
    delivered_at TEXT,
    expires_at TEXT,
    error_sanitized TEXT
) STRICT;

CREATE UNIQUE INDEX idx_notification_deduplication
    ON notification_deliveries (binding_id, deduplication_key)
    WHERE deduplication_key IS NOT NULL;

CREATE TABLE audit_records (
    id TEXT PRIMARY KEY NOT NULL,
    occurred_at TEXT NOT NULL,
    actor_type TEXT NOT NULL,
    actor_id TEXT,
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT,
    request_id TEXT,
    correlation_id TEXT,
    outcome TEXT NOT NULL,
    metadata_sanitized_json TEXT NOT NULL
) STRICT;

CREATE INDEX idx_audit_resource
    ON audit_records (resource_type, resource_id, occurred_at DESC);
CREATE INDEX idx_audit_actor
    ON audit_records (actor_type, actor_id, occurred_at DESC);
