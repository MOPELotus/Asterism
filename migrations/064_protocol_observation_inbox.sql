CREATE TABLE protocol_observations (
    id TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL,
    surface TEXT NOT NULL CHECK (surface IN (
        'authentication', 'course_inventory', 'task_inventory', 'task_detail',
        'task_progress', 'question_inventory', 'question_parse', 'answer_resolve',
        'submission_build', 'submission_execute', 'submission_verify', 'task_execution',
        'browser_bridge', 'other'
    )),
    kind TEXT NOT NULL CHECK (kind IN (
        'unknown_question_kind', 'unknown_result_shape', 'unknown_task_type',
        'field_drift', 'endpoint_version_drift', 'other'
    )),
    shape_digest BLOB NOT NULL CHECK (length(shape_digest) = 32),
    shape_sanitized_json TEXT NOT NULL,
    shape_bytes INTEGER NOT NULL CHECK (shape_bytes BETWEEN 1 AND 65536),
    occurrence_count INTEGER NOT NULL CHECK (occurrence_count > 0),
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    last_execution_id TEXT REFERENCES executions(id) ON DELETE SET NULL,
    UNIQUE (provider_id, surface, kind, shape_digest),
    CHECK (first_seen_at <= last_seen_at)
) STRICT;

CREATE INDEX idx_protocol_observations_inbox
    ON protocol_observations (last_seen_at DESC, provider_id, kind, id);

CREATE TABLE protocol_observation_occurrences (
    occurrence_digest BLOB PRIMARY KEY NOT NULL CHECK (length(occurrence_digest) = 32),
    observation_id TEXT NOT NULL REFERENCES protocol_observations(id) ON DELETE CASCADE,
    execution_id TEXT REFERENCES executions(id) ON DELETE SET NULL,
    observed_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_protocol_observation_occurrences_observation
    ON protocol_observation_occurrences (observation_id, observed_at, occurrence_digest);
