CREATE TABLE scan_schedules (
    id TEXT PRIMARY KEY NOT NULL,
    provider_account_id TEXT NOT NULL REFERENCES provider_accounts(id) ON DELETE CASCADE,
    interval_seconds INTEGER NOT NULL CHECK (interval_seconds > 0),
    next_run_at TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (provider_account_id)
) STRICT;

CREATE TABLE scheduled_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    job_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    run_at TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'claimed', 'completed', 'cancelled', 'dead_letter')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    idempotency_key TEXT NOT NULL UNIQUE,
    worker_id TEXT,
    lease_expires_at TEXT,
    last_error_sanitized TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK ((state = 'claimed') = (worker_id IS NOT NULL AND lease_expires_at IS NOT NULL))
) STRICT;

CREATE INDEX idx_scheduled_jobs_due
    ON scheduled_jobs (run_at, id)
    WHERE state = 'pending';

CREATE INDEX idx_scheduled_jobs_expired_claims
    ON scheduled_jobs (lease_expires_at)
    WHERE state = 'claimed';
