ALTER TABLE global_answer_corpus_projections RENAME TO global_answer_corpus_projections_legacy;

CREATE TABLE global_answer_corpus_projections (
    private_evidence_id TEXT PRIMARY KEY NOT NULL
        REFERENCES private_answer_evidence(id) ON DELETE CASCADE,
    corpus_entry_id TEXT NOT NULL
        REFERENCES global_answer_corpus_entries(id) ON DELETE RESTRICT,
    projected_at TEXT NOT NULL
) STRICT;

INSERT INTO global_answer_corpus_projections
    (private_evidence_id, corpus_entry_id, projected_at)
SELECT private_evidence_id, corpus_entry_id, projected_at
FROM global_answer_corpus_projections_legacy;

DROP TABLE global_answer_corpus_projections_legacy;

CREATE INDEX idx_global_answer_corpus_projections_entry
    ON global_answer_corpus_projections (corpus_entry_id, private_evidence_id);
