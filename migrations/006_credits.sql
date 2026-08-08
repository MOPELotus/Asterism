CREATE TABLE credit_accounts (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    available INTEGER NOT NULL DEFAULT 0 CHECK (available >= 0),
    reserved INTEGER NOT NULL DEFAULT 0 CHECK (reserved >= 0),
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE price_quotes (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
    amount INTEGER NOT NULL CHECK (amount >= 0),
    pricing_revision TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE credit_reservations (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    quote_id TEXT NOT NULL REFERENCES price_quotes(id) ON DELETE RESTRICT,
    execution_id TEXT NOT NULL UNIQUE REFERENCES executions(id) ON DELETE RESTRICT,
    amount INTEGER NOT NULL CHECK (amount >= 0),
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
CREATE TABLE credit_transactions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    amount INTEGER NOT NULL,
    transaction_type TEXT NOT NULL,
    task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    execution_id TEXT REFERENCES executions(id) ON DELETE SET NULL,
    operator_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_credit_transactions_ledger
    ON credit_transactions (user_id, created_at, id);

CREATE TABLE pricing_catalog_revisions (
    id TEXT PRIMARY KEY NOT NULL,
    revision TEXT NOT NULL UNIQUE,
    catalog_json TEXT NOT NULL,
    effective_from TEXT NOT NULL,
    expires_at TEXT,
    created_by TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE package_entitlements (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    scope_json TEXT NOT NULL,
    coverage_json TEXT NOT NULL,
    pricing_revision TEXT NOT NULL,
    effective_from TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
