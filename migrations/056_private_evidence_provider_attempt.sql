ALTER TABLE private_answer_evidence
ADD COLUMN provider_attempt_digest BLOB CHECK (
    provider_attempt_digest IS NULL OR length(provider_attempt_digest) = 32
);
