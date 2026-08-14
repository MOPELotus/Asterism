CREATE TABLE private_answer_evidence (
    id TEXT PRIMARY KEY NOT NULL,
    evidence_digest BLOB NOT NULL UNIQUE CHECK (length(evidence_digest) = 32),
    owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    course_id TEXT REFERENCES courses(id) ON DELETE SET NULL,
    task_id TEXT NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    execution_attempt_id TEXT REFERENCES execution_attempts(id) ON DELETE SET NULL,
    source_candidate_id TEXT,
    question_json TEXT NOT NULL,
    question_content_fingerprint TEXT NOT NULL CHECK (
        length(question_content_fingerprint) = 67
        AND substr(question_content_fingerprint, 1, 3) = 'v1:'
        AND substr(question_content_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    answer_json TEXT NOT NULL,
    answer_source TEXT NOT NULL CHECK (
        answer_source IN ('manual', 'local_cache', 'provider_native', 'external_bank', 'other')
    ),
    evidence_class TEXT NOT NULL CHECK (
        evidence_class IN ('official', 'verified_historical', 'negative')
    ),
    result_digest BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    provenance_sanitized_json TEXT NOT NULL,
    projection_state TEXT NOT NULL CHECK (projection_state IN ('projected', 'unmatched')),
    unmatched_reason TEXT CHECK (
        unmatched_reason IS NULL OR unmatched_reason IN (
            'incomplete_question', 'missing_shared_context',
            'ambiguous_semantic_identity', 'unsupported_hierarchy'
        )
    ),
    observed_at TEXT NOT NULL,
    verified_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    CHECK (
        (projection_state = 'projected' AND unmatched_reason IS NULL)
        OR (projection_state = 'unmatched' AND unmatched_reason IS NOT NULL)
    ),
    FOREIGN KEY (provider_account_id, provider_id)
        REFERENCES provider_accounts(id, provider_id) ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id, task_id, provider_id)
        REFERENCES question_snapshots(id, task_id, provider_id) ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id, source_candidate_id, question_id)
        REFERENCES answer_candidates(question_snapshot_id, id, question_id) ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_private_answer_evidence_owner_question
    ON private_answer_evidence (
        owner_user_id, question_content_fingerprint, verified_at DESC, id
    );

CREATE INDEX idx_private_answer_evidence_task
    ON private_answer_evidence (task_id, question_snapshot_id, question_id);

CREATE TABLE global_answer_corpus_entries (
    id TEXT PRIMARY KEY NOT NULL,
    question_content_fingerprint TEXT NOT NULL CHECK (
        length(question_content_fingerprint) = 67
        AND substr(question_content_fingerprint, 1, 3) = 'v1:'
        AND substr(question_content_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    question_asset_json TEXT NOT NULL,
    semantic_answer_digest BLOB NOT NULL CHECK (length(semantic_answer_digest) = 32),
    semantic_answer_json TEXT NOT NULL,
    official_evidence_count INTEGER NOT NULL DEFAULT 0 CHECK (official_evidence_count >= 0),
    verified_historical_evidence_count INTEGER NOT NULL DEFAULT 0
        CHECK (verified_historical_evidence_count >= 0),
    negative_evidence_count INTEGER NOT NULL DEFAULT 0 CHECK (negative_evidence_count >= 0),
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    last_verified_at TEXT,
    UNIQUE (question_content_fingerprint, semantic_answer_digest)
) STRICT;

CREATE INDEX idx_global_answer_corpus_question
    ON global_answer_corpus_entries (
        question_content_fingerprint,
        official_evidence_count DESC,
        verified_historical_evidence_count DESC,
        negative_evidence_count ASC,
        id
    );

CREATE TABLE global_answer_corpus_projections (
    private_evidence_id TEXT PRIMARY KEY NOT NULL
        REFERENCES private_answer_evidence(id) ON DELETE CASCADE,
    corpus_entry_id TEXT NOT NULL
        REFERENCES global_answer_corpus_entries(id) ON DELETE RESTRICT,
    projected_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_global_answer_corpus_projections_entry
    ON global_answer_corpus_projections (corpus_entry_id, private_evidence_id);
