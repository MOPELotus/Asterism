ALTER TABLE question_snapshot_items ADD COLUMN semantic_fingerprint TEXT
    CHECK (semantic_fingerprint IS NULL OR (
        length(semantic_fingerprint) = 76
        AND semantic_fingerprint GLOB 'semantic-v1:[0-9a-f]*'
    ));

CREATE INDEX idx_question_snapshot_items_semantic_fingerprint
    ON question_snapshot_items (semantic_fingerprint, snapshot_id)
    WHERE semantic_fingerprint IS NOT NULL;
