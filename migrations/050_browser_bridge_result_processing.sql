ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN processing_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (processing_state IN ('pending', 'processing', 'retry', 'dead_letter'));

ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0
        CHECK (attempt_count BETWEEN 0 AND 32);

ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN claimed_by TEXT CHECK (claimed_by IS NULL OR length(claimed_by) BETWEEN 1 AND 128);

ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN claim_expires_at TEXT;

ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN next_attempt_at TEXT;

ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN last_error_kind TEXT
        CHECK (last_error_kind IS NULL OR length(last_error_kind) BETWEEN 1 AND 96);

CREATE INDEX idx_browser_bridge_result_processing
    ON browser_bridge_result_artifacts (
        processing_state, next_attempt_at, claim_expires_at, received_at, session_id, sequence
    );
