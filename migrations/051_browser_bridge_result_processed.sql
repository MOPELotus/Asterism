ALTER TABLE browser_bridge_result_artifacts
    ADD COLUMN processed_at TEXT;

CREATE INDEX idx_browser_bridge_result_unprocessed
    ON browser_bridge_result_artifacts (
        processed_at, processing_state, next_attempt_at, claim_expires_at,
        received_at, session_id, sequence
    );
