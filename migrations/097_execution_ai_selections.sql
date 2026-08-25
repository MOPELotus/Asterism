CREATE TABLE execution_ai_selections (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE RESTRICT,
    profile TEXT NOT NULL CHECK (profile IN ('economy', 'gpt_only')),
    route TEXT NOT NULL CHECK (route IN ('timed', 'untimed', 'escalation')),
    created_at TEXT NOT NULL
) STRICT;
