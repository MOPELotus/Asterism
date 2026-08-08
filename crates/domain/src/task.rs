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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
}
