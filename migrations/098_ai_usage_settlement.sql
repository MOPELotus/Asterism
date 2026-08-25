ALTER TABLE ai_usage_records ADD COLUMN estimated_cost INTEGER NOT NULL DEFAULT 0 CHECK (estimated_cost >= 0);
ALTER TABLE ai_usage_records ADD COLUMN settlement_status TEXT NOT NULL DEFAULT 'pending'
    CHECK (settlement_status IN ('not_billable', 'pending', 'settled', 'waived'));
