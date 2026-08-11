CREATE TABLE credit_grant_receipts (
    id TEXT PRIMARY KEY NOT NULL,
    operator_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    amount INTEGER NOT NULL CHECK (amount > 0),
    reason TEXT NOT NULL,
    transaction_id TEXT NOT NULL UNIQUE REFERENCES credit_transactions(id) ON DELETE RESTRICT,
    result_available INTEGER NOT NULL CHECK (result_available >= 0),
    result_reserved INTEGER NOT NULL CHECK (result_reserved >= 0),
    correlation_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (operator_id, idempotency_key)
) STRICT;

CREATE INDEX idx_credit_grant_receipts_target
    ON credit_grant_receipts (user_id, created_at DESC, id);
