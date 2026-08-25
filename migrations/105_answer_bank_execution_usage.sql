CREATE TABLE answer_bank_usage_records_v2 (
    id TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id TEXT,
    execution_id TEXT UNIQUE REFERENCES executions(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    hit_count INTEGER NOT NULL CHECK (hit_count > 0),
    charged_amount INTEGER NOT NULL CHECK (charged_amount >= 0),
    settlement_status TEXT NOT NULL
        CHECK (settlement_status IN ('not_billable', 'pending', 'settled', 'waived')),
    created_at TEXT NOT NULL
) STRICT;

INSERT INTO answer_bank_usage_records_v2
    (id, owner_user_id, task_id, execution_id, source, hit_count, charged_amount, settlement_status, created_at)
SELECT id, owner_user_id, task_id, NULL, source, hit_count, charged_amount, settlement_status, created_at
FROM answer_bank_usage_records;

DROP TABLE answer_bank_usage_records;
ALTER TABLE answer_bank_usage_records_v2 RENAME TO answer_bank_usage_records;

CREATE INDEX idx_answer_bank_usage_owner_created
    ON answer_bank_usage_records(owner_user_id, created_at DESC);

CREATE TRIGGER answer_bank_usage_reservation_committed
AFTER UPDATE OF state ON credit_reservations
WHEN NEW.state = 'committed'
BEGIN
    UPDATE answer_bank_usage_records
    SET settlement_status = 'settled'
    WHERE execution_id = NEW.execution_id AND settlement_status = 'pending';
END;

CREATE TRIGGER answer_bank_usage_reservation_released
AFTER UPDATE OF state ON credit_reservations
WHEN NEW.state = 'released'
BEGIN
    UPDATE answer_bank_usage_records
    SET settlement_status = 'waived'
    WHERE execution_id = NEW.execution_id AND settlement_status = 'pending';
END;
