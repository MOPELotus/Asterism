ALTER TABLE execution_atomic_mutations
ADD COLUMN retry_not_before TEXT CHECK (
    retry_not_before IS NULL
    OR (
        accepted = 0
        AND received_at IS NOT NULL
        AND retry_not_before > received_at
    )
);
