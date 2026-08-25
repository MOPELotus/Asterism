PRAGMA legacy_alter_table = ON;

ALTER TABLE answer_candidates RENAME TO answer_candidates_legacy;

CREATE TABLE answer_candidates (
    id TEXT PRIMARY KEY NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (
        source IN ('manual', 'local_cache', 'provider_native', 'ai', 'external_bank', 'other')
    ),
    candidate_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (question_snapshot_id, question_id)
        REFERENCES question_snapshot_items(snapshot_id, question_id)
        ON DELETE CASCADE
) STRICT;

INSERT INTO answer_candidates
    (id, question_snapshot_id, question_id, source, candidate_json, created_at)
SELECT id, question_snapshot_id, question_id, source, candidate_json, created_at
FROM answer_candidates_legacy;

DROP TABLE answer_candidates_legacy;

CREATE INDEX idx_answer_candidates_snapshot_question
    ON answer_candidates (question_snapshot_id, question_id, source, created_at, id);

CREATE UNIQUE INDEX idx_answer_candidates_snapshot_identity
    ON answer_candidates (question_snapshot_id, id, question_id);

ALTER TABLE private_answer_evidence RENAME TO private_answer_evidence_legacy;

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
    provider_attempt_digest BLOB CHECK (
        provider_attempt_digest IS NULL OR length(provider_attempt_digest) = 32
    ),
    source_candidate_id TEXT,
    question_json TEXT NOT NULL,
    question_content_fingerprint TEXT NOT NULL CHECK (
        length(question_content_fingerprint) = 67
        AND substr(question_content_fingerprint, 1, 3) = 'v1:'
        AND substr(question_content_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    answer_json TEXT NOT NULL,
    answer_source TEXT NOT NULL CHECK (
        answer_source IN ('manual', 'local_cache', 'provider_native', 'ai', 'external_bank', 'other')
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

INSERT INTO private_answer_evidence (
    id, evidence_digest, owner_user_id, provider_id, provider_account_id, course_id,
    task_id, question_snapshot_id, question_id, execution_attempt_id,
    provider_attempt_digest, source_candidate_id, question_json,
    question_content_fingerprint, answer_json, answer_source, evidence_class,
    result_digest, provenance_sanitized_json, projection_state, unmatched_reason,
    observed_at, verified_at, created_at
)
SELECT
    id, evidence_digest, owner_user_id, provider_id, provider_account_id, course_id,
    task_id, question_snapshot_id, question_id, execution_attempt_id,
    provider_attempt_digest, source_candidate_id, question_json,
    question_content_fingerprint, answer_json, answer_source, evidence_class,
    result_digest, provenance_sanitized_json, projection_state, unmatched_reason,
    observed_at, verified_at, created_at
FROM private_answer_evidence_legacy;

DROP TABLE private_answer_evidence_legacy;

CREATE INDEX idx_private_answer_evidence_owner_question
    ON private_answer_evidence (
        owner_user_id, question_content_fingerprint, verified_at DESC, id
    );

CREATE INDEX idx_private_answer_evidence_task
    ON private_answer_evidence (task_id, question_snapshot_id, question_id);

PRAGMA legacy_alter_table = OFF;
