ALTER TABLE question_snapshot_items
ADD COLUMN content_fingerprint TEXT CHECK (
    content_fingerprint IS NULL OR (
        length(content_fingerprint) = 67
        AND substr(content_fingerprint, 1, 3) = 'v1:'
        AND substr(content_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    )
);

CREATE INDEX idx_question_snapshot_items_content_fingerprint
    ON question_snapshot_items (content_fingerprint, snapshot_id);
