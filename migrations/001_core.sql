PRAGMA foreign_keys = ON;

CREATE TABLE system_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE event_outbox (
    id TEXT PRIMARY KEY NOT NULL,
    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'delivered', 'dead_letter')),
    published_at TEXT,
    publish_attempts INTEGER NOT NULL DEFAULT 0 CHECK (publish_attempts >= 0),
    last_error_sanitized TEXT
) STRICT;

CREATE INDEX idx_event_outbox_pending
    ON event_outbox (occurred_at)
    WHERE state = 'pending';
