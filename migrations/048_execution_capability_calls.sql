ALTER TABLE execution_capability_steps RENAME TO execution_capability_steps_legacy;

CREATE TABLE execution_capability_steps (
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position BETWEEN 1 AND 5),
    call_position INTEGER NOT NULL CHECK (call_position BETWEEN 1 AND 5),
    call_member_position INTEGER NOT NULL CHECK (call_member_position BETWEEN 1 AND 5),
    capability TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'issued', 'succeeded', 'ambiguous')),
    issued_attempt_id TEXT,
    issued_at TEXT,
    succeeded_at TEXT,
    PRIMARY KEY (execution_id, position),
    UNIQUE (execution_id, capability),
    UNIQUE (execution_id, call_position, call_member_position),
    FOREIGN KEY (execution_id, issued_attempt_id)
        REFERENCES execution_attempts(execution_id, id)
        ON DELETE RESTRICT,
    CHECK (
        (state = 'pending' AND issued_attempt_id IS NULL AND issued_at IS NULL AND succeeded_at IS NULL)
        OR (state IN ('issued', 'ambiguous') AND issued_attempt_id IS NOT NULL AND issued_at IS NOT NULL AND succeeded_at IS NULL)
        OR (state = 'succeeded' AND issued_attempt_id IS NOT NULL AND issued_at IS NOT NULL AND succeeded_at IS NOT NULL)
    )
) STRICT;

INSERT INTO execution_capability_steps (
    execution_id, position, call_position, call_member_position, capability,
    state, issued_attempt_id, issued_at, succeeded_at
)
SELECT execution_id, position, position, 1, capability,
       state, issued_attempt_id, issued_at, succeeded_at
FROM execution_capability_steps_legacy;

DROP TABLE execution_capability_steps_legacy;

CREATE INDEX idx_execution_capability_steps_state
    ON execution_capability_steps (execution_id, state, call_position, position);
