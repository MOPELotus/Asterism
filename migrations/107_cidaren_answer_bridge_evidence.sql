CREATE TABLE cidaren_answer_bridge_selections (
    execution_id TEXT NOT NULL,
    remote_question_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    question_snapshot_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    answer_candidate_id TEXT NOT NULL,
    selected_at TEXT NOT NULL,
    correctness TEXT CHECK (correctness IN ('correct', 'wrong', 'mixed', 'unknown')),
    observed_at TEXT,
    PRIMARY KEY (execution_id, remote_question_id),
    FOREIGN KEY (execution_id) REFERENCES executions(id) ON DELETE CASCADE,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (question_snapshot_id) REFERENCES question_snapshots(id) ON DELETE CASCADE
);

CREATE INDEX idx_cidaren_answer_bridge_selection_candidate
    ON cidaren_answer_bridge_selections(question_snapshot_id, answer_candidate_id);
