ALTER TABLE submission_drafts
    ADD COLUMN total_question_count INTEGER NOT NULL DEFAULT 1
        CHECK (total_question_count BETWEEN 1 AND 5000);

ALTER TABLE submission_drafts
    ADD COLUMN minimum_coverage_millis INTEGER NOT NULL DEFAULT 1000
        CHECK (minimum_coverage_millis BETWEEN 1 AND 1000);

ALTER TABLE submission_drafts
    ADD COLUMN unanswered_question_ids_json TEXT NOT NULL DEFAULT '[]'
        CHECK (length(unanswered_question_ids_json) BETWEEN 2 AND 200002);

UPDATE submission_drafts SET total_question_count = item_count;
