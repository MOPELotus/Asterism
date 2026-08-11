use serde::{Deserialize, Serialize};

use crate::{CourseId, ProviderAccountId, TaskId, TaskSnapshotId, Timestamp};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Chapter,
    Work,
    Exam,
    Resource,
    Practice,
    Discussion,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentClass {
    Routine,
    Unknown,
    Formal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteState {
    Unknown,
    NotOpen,
    Pending,
    InProgress,
    Completed,
    Expired,
    Removed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationState {
    Discovered,
    Ready,
    WaitingApproval,
    Scheduled,
    CreditBlocked,
    HumanRequired,
    Running,
    Recovering,
    RetryWaiting,
    Succeeded,
    Failed,
    Cancelled,
    Ignored,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCapability {
    ProgressRead,
    ResourceExecution,
    /// Marks a potentially non-idempotent `TaskExecution` mutation whose
    /// outcome must be confirmed through an independent progress read. Core
    /// invokes the mutation at most once per Execution and uses verify-only
    /// recovery after every ambiguous outcome.
    ExecutionVerify,
    QuestionInventory,
    QuestionParse,
    AnswerResolve,
    SubmissionBuild,
    SubmissionExecute,
    SubmissionVerify,
    DurationRead,
    DurationReport,
    Discussion,
    Practice,
    BrowserBridge,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Task {
    pub id: TaskId,
    pub provider_account_id: ProviderAccountId,
    pub course_id: Option<CourseId>,
    pub remote_id: String,
    pub source_type: SourceType,
    pub assessment_class: AssessmentClass,
    pub title: String,
    pub remote_state: RemoteState,
    pub orchestration_state: OrchestrationState,
    pub opens_at: Option<Timestamp>,
    pub due_at: Option<Timestamp>,
    pub closes_at: Option<Timestamp>,
    pub discovered_at: Timestamp,
    pub updated_at: Timestamp,
    pub latest_snapshot_id: Option<TaskSnapshotId>,
    pub capabilities: Vec<TaskCapability>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TaskSnapshot {
    pub id: TaskSnapshotId,
    pub task_id: TaskId,
    pub captured_at: Timestamp,
    pub provider_version: String,
    pub normalized: serde_json::Value,
    pub remote_raw_sanitized: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDiffKind {
    Created,
    MetadataChanged,
    ContentChanged,
    DeadlineChanged,
    Opened,
    Closed,
    Reopened,
    CompletedExternally,
    Removed,
}

/// Classifies meaningful changes between two normalized observations of the
/// same local task. Local orchestration state is intentionally ignored.
pub fn classify_task_changes(
    previous: &Task,
    current: &Task,
    previous_normalized: &serde_json::Value,
    current_normalized: &serde_json::Value,
) -> Vec<TaskDiffKind> {
    let mut changes = Vec::new();
    if previous.remote_id != current.remote_id
        || previous.course_id != current.course_id
        || previous.title != current.title
        || previous.assessment_class != current.assessment_class
        || previous.capabilities != current.capabilities
    {
        changes.push(TaskDiffKind::MetadataChanged);
    }
    if previous_normalized != current_normalized {
        changes.push(TaskDiffKind::ContentChanged);
    }
    if previous.opens_at != current.opens_at
        || previous.due_at != current.due_at
        || previous.closes_at != current.closes_at
    {
        changes.push(TaskDiffKind::DeadlineChanged);
    }
    if previous.remote_state != current.remote_state {
        let state_change =
            classify_remote_state_change(previous.remote_state, current.remote_state);
        if let Some(change) = state_change {
            changes.push(change);
        } else if !changes.contains(&TaskDiffKind::MetadataChanged) {
            changes.push(TaskDiffKind::MetadataChanged);
        }
    }
    changes
}

const fn classify_remote_state_change(
    previous: RemoteState,
    current: RemoteState,
) -> Option<TaskDiffKind> {
    if matches!(current, RemoteState::Removed) {
        Some(TaskDiffKind::Removed)
    } else if matches!(current, RemoteState::Completed)
        && !matches!(previous, RemoteState::Completed)
    {
        Some(TaskDiffKind::CompletedExternally)
    } else if matches!(
        previous,
        RemoteState::Expired | RemoteState::Removed | RemoteState::Completed
    ) && matches!(
        current,
        RemoteState::NotOpen
            | RemoteState::Pending
            | RemoteState::InProgress
            | RemoteState::Unknown
    ) {
        Some(TaskDiffKind::Reopened)
    } else if matches!(previous, RemoteState::NotOpen)
        && matches!(current, RemoteState::Pending | RemoteState::InProgress)
    {
        Some(TaskDiffKind::Opened)
    } else if matches!(current, RemoteState::Expired) {
        Some(TaskDiffKind::Closed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exam_source_does_not_imply_formal_assessment() {
        let source = SourceType::Exam;
        let classification = AssessmentClass::Routine;
        assert_eq!(source, SourceType::Exam);
        assert_eq!(classification, AssessmentClass::Routine);
    }

    #[test]
    fn normalized_remote_changes_keep_independent_diff_dimensions() {
        let now = chrono::Utc::now();
        let mut previous = Task {
            id: TaskId::new(),
            provider_account_id: ProviderAccountId::new(),
            course_id: None,
            remote_id: "remote-a".to_owned(),
            source_type: SourceType::Exam,
            assessment_class: AssessmentClass::Routine,
            title: "weekly".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Succeeded,
            opens_at: None,
            due_at: Some(now),
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: vec![TaskCapability::ProgressRead],
        };
        let mut current = previous.clone();
        current.remote_id = "remote-b".to_owned();
        current.remote_state = RemoteState::Completed;
        current.due_at = Some(now + chrono::Duration::hours(1));
        current.orchestration_state = OrchestrationState::Ready;
        let changes = classify_task_changes(
            &previous,
            &current,
            &serde_json::json!({"version": 1}),
            &serde_json::json!({"version": 2}),
        );
        assert_eq!(
            changes,
            [
                TaskDiffKind::MetadataChanged,
                TaskDiffKind::ContentChanged,
                TaskDiffKind::DeadlineChanged,
                TaskDiffKind::CompletedExternally,
            ]
        );

        previous.remote_state = RemoteState::Completed;
        current.remote_state = RemoteState::Pending;
        assert!(
            classify_task_changes(
                &previous,
                &current,
                &serde_json::Value::Null,
                &serde_json::Value::Null,
            )
            .contains(&TaskDiffKind::Reopened)
        );
    }
}
