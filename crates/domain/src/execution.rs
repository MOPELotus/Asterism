use serde::{Deserialize, Serialize};

use crate::{
    BatchExecutionAttemptId, BatchExecutionId, CourseId, ExecutionAttemptId, ExecutionId,
    PriceQuoteId, ProviderAccountId, SubmissionDraftId, TaskCapability, TaskId, Timestamp, UserId,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestSource {
    Scheduler,
    WebUi,
    Yunzai,
    Cli,
    System,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Requested,
    Scheduled,
    Running,
    Recovering,
    RetryWaiting,
    HumanRequired,
    Succeeded,
    Failed,
    Cancelled,
}

impl ExecutionState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Execution {
    pub id: ExecutionId,
    pub task_id: TaskId,
    /// Immutable, canonical action set selected by the caller. Providers must
    /// execute only these advertised capabilities, never every action on the
    /// latest Task snapshot.
    pub requested_capabilities: Vec<TaskCapability>,
    /// Frozen only for independent submission executions. Resource and other
    /// non-submission executions deliberately leave this unset.
    pub submission_draft_id: Option<SubmissionDraftId>,
    pub requested_by: Option<UserId>,
    pub request_source: RequestSource,
    pub quote_id: Option<PriceQuoteId>,
    pub state: ExecutionState,
    pub scheduled_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionAttempt {
    pub id: ExecutionAttemptId,
    pub execution_id: ExecutionId,
    pub attempt_no: u32,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub result: Option<AttemptResult>,
    pub error_class: Option<ProviderErrorClass>,
    pub provider_trace_id: Option<String>,
}

/// Course-scoped parent for a Provider-planned ordered child batch. It is not
/// attached to a synthetic or anchor Task and therefore never owns a child's
/// Task lease.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchExecution {
    pub id: BatchExecutionId,
    pub provider_account_id: ProviderAccountId,
    pub course_id: CourseId,
    /// Canonical child capability set authorized uniformly across the batch.
    pub requested_capabilities: Vec<TaskCapability>,
    pub expected_child_count: u32,
    pub requested_by: Option<UserId>,
    pub request_source: RequestSource,
    pub state: ExecutionState,
    pub scheduled_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

impl BatchExecution {
    /// Rechecks the parent identity, canonical child authority and lifecycle
    /// timestamps independently from Provider-private selection bytes.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized or non-canonical child capability sets, an
    /// invalid child count, or lifecycle timestamps inconsistent with state.
    pub fn validate(&self) -> Result<(), BatchExecutionValidationError> {
        if self.requested_capabilities.is_empty()
            || self.requested_capabilities.len() > 5
            || !self
                .requested_capabilities
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            || !self
                .requested_capabilities
                .iter()
                .copied()
                .all(is_batch_execution_capability)
            || self.expected_child_count == 0
            || self.expected_child_count > 8_192
            || self
                .scheduled_at
                .is_some_and(|scheduled_at| scheduled_at < self.created_at)
            || self
                .started_at
                .is_some_and(|started_at| started_at < self.scheduled_at.unwrap_or(self.created_at))
            || self
                .finished_at
                .is_some_and(|finished_at| finished_at < self.started_at.unwrap_or(self.created_at))
            || !batch_execution_state_timestamps_match(self)
        {
            return Err(BatchExecutionValidationError::Invalid);
        }
        Ok(())
    }
}

const fn batch_execution_state_timestamps_match(execution: &BatchExecution) -> bool {
    match execution.state {
        ExecutionState::Requested => {
            execution.scheduled_at.is_none()
                && execution.started_at.is_none()
                && execution.finished_at.is_none()
        }
        ExecutionState::Scheduled => {
            execution.scheduled_at.is_some()
                && execution.started_at.is_none()
                && execution.finished_at.is_none()
        }
        ExecutionState::Running
        | ExecutionState::Recovering
        | ExecutionState::RetryWaiting
        | ExecutionState::HumanRequired => {
            execution.scheduled_at.is_some()
                && execution.started_at.is_some()
                && execution.finished_at.is_none()
        }
        ExecutionState::Succeeded => {
            execution.scheduled_at.is_some()
                && execution.started_at.is_some()
                && execution.finished_at.is_some()
        }
        ExecutionState::Failed | ExecutionState::Cancelled => {
            execution.scheduled_at.is_some() && execution.finished_at.is_some()
        }
    }
}

const fn is_batch_execution_capability(capability: TaskCapability) -> bool {
    matches!(
        capability,
        TaskCapability::ResourceExecution
            | TaskCapability::SubmissionExecute
            | TaskCapability::DurationReport
            | TaskCapability::Discussion
            | TaskCapability::Practice
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchExecutionAttempt {
    pub id: BatchExecutionAttemptId,
    pub batch_execution_id: BatchExecutionId,
    pub attempt_no: u32,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub result: Option<AttemptResult>,
    pub error_class: Option<ProviderErrorClass>,
    pub provider_trace_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BatchExecutionValidationError {
    #[error("batch execution identity, authority, count or lifecycle is invalid")]
    Invalid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionLease {
    pub task_id: TaskId,
    pub execution_id: ExecutionId,
    pub worker_id: String,
    pub expires_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptResult {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorClass {
    Authentication,
    Authorization,
    RateLimited,
    Network,
    ProviderUnavailable,
    ProtocolDrift,
    InvalidRemoteState,
    UnsupportedTask,
    HumanRequired,
    Internal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStage {
    Preparing,
    RefreshingRemoteState,
    Authenticating,
    Scanning,
    FetchingDetail,
    Executing,
    Submitting,
    ReportingDuration,
    Verifying,
    Finalizing,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionProgress {
    pub execution_id: ExecutionId,
    /// A best-effort value from 0 through 100. `None` means indeterminate.
    pub percent: Option<u8>,
    pub stage: ExecutionStage,
    pub status_text: Option<String>,
    pub current_item: Option<String>,
    pub completed_items: Option<u32>,
    pub total_items: Option<u32>,
    pub updated_at: Timestamp,
}

impl ExecutionProgress {
    /// Checks percentage and item-count invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionProgressError`] when percentage exceeds 100 or the
    /// completed item count exceeds the total.
    pub fn validate(&self) -> Result<(), ExecutionProgressError> {
        if self.percent.is_some_and(|value| value > 100) {
            return Err(ExecutionProgressError::PercentOutOfRange);
        }
        if matches!((self.completed_items, self.total_items), (Some(done), Some(total)) if done > total)
        {
            return Err(ExecutionProgressError::CompletedExceedsTotal);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionProgressError {
    #[error("execution progress percent must be between 0 and 100")]
    PercentOutOfRange,
    #[error("completed items cannot exceed total items")]
    CompletedExceedsTotal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExecutionLogEvent {
    pub execution_id: ExecutionId,
    pub attempt_id: Option<ExecutionAttemptId>,
    pub timestamp: Timestamp,
    pub level: LogLevel,
    pub stage: ExecutionStage,
    pub message: String,
    pub provider_trace_id: Option<String>,
    pub metadata_sanitized: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn rejects_impossible_progress() {
        let progress = ExecutionProgress {
            execution_id: ExecutionId::new(),
            percent: Some(101),
            stage: ExecutionStage::Executing,
            status_text: None,
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: Utc::now(),
        };
        assert_eq!(
            progress.validate(),
            Err(ExecutionProgressError::PercentOutOfRange)
        );
    }

    #[test]
    fn batch_execution_requires_independent_course_scope_and_canonical_children() {
        let now = Utc::now();
        let mut batch = BatchExecution {
            id: BatchExecutionId::new(),
            provider_account_id: ProviderAccountId::new(),
            course_id: CourseId::new(),
            requested_capabilities: vec![
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ],
            expected_child_count: 2,
            requested_by: Some(UserId::new()),
            request_source: RequestSource::WebUi,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        };
        assert_eq!(batch.validate(), Ok(()));
        batch.requested_capabilities.swap(0, 1);
        assert_eq!(
            batch.validate(),
            Err(BatchExecutionValidationError::Invalid)
        );
        batch.requested_capabilities.swap(0, 1);
        batch.expected_child_count = 8_193;
        assert_eq!(
            batch.validate(),
            Err(BatchExecutionValidationError::Invalid)
        );
    }
}
