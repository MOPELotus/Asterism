CREATE TABLE course_enrollment_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    draft_id TEXT NOT NULL UNIQUE
        REFERENCES course_enrollment_drafts(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (state IN (
        'prepared', 'mutation_issued', 'receipt_recorded', 'verification_pending',
        'succeeded', 'rejected', 'cancelled', 'failed_before_issue'
    )),
    issued_operation_type TEXT CHECK (
        issued_operation_type IS NULL OR length(issued_operation_type) BETWEEN 1 AND 96
    ),
    issued_request_digest BLOB CHECK (
        issued_request_digest IS NULL OR length(issued_request_digest) = 32
    ),
    response_digest BLOB CHECK (response_digest IS NULL OR length(response_digest) = 32),
    response_accepted INTEGER CHECK (response_accepted IN (0, 1)),
    response_observed_at TEXT,
    verification_digest BLOB CHECK (
        verification_digest IS NULL OR length(verification_digest) = 32
    ),
    membership_present INTEGER CHECK (membership_present IN (0, 1)),
    verification_observed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    CHECK (
        (state = 'prepared' AND issued_operation_type IS NULL AND issued_request_digest IS NULL)
        OR (state IN ('cancelled', 'failed_before_issue')
            AND issued_operation_type IS NULL AND issued_request_digest IS NULL)
        OR (state NOT IN ('prepared', 'cancelled', 'failed_before_issue')
            AND issued_operation_type IS NOT NULL AND issued_request_digest IS NOT NULL)
    ),
    CHECK (
        (response_digest IS NULL AND response_accepted IS NULL AND response_observed_at IS NULL)
        OR (response_digest IS NOT NULL AND response_accepted IS NOT NULL
            AND response_observed_at IS NOT NULL)
    ),
    CHECK (
        (verification_digest IS NULL AND membership_present IS NULL
            AND verification_observed_at IS NULL)
        OR (verification_digest IS NOT NULL AND membership_present IS NOT NULL
            AND verification_observed_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX idx_course_enrollment_attempts_recovery
    ON course_enrollment_attempts (state, updated_at, id)
    WHERE state IN ('mutation_issued', 'receipt_recorded', 'verification_pending');
