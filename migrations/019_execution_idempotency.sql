ALTER TABLE executions ADD COLUMN idempotency_scope TEXT;
ALTER TABLE executions ADD COLUMN idempotency_key TEXT;

CREATE UNIQUE INDEX idx_executions_idempotency
    ON executions (idempotency_scope, idempotency_key)
    WHERE idempotency_scope IS NOT NULL AND idempotency_key IS NOT NULL;
