ALTER TABLE event_outbox ADD COLUMN worker_id TEXT;
ALTER TABLE event_outbox ADD COLUMN lock_expires_at TEXT;

CREATE INDEX idx_event_outbox_claims
    ON event_outbox (state, lock_expires_at, occurred_at);
