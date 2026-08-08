use serde::{Deserialize, Serialize};

use crate::{ExecutionAttemptId, ExecutionId, PriceQuoteId, TaskId, Timestamp, UserId};

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
}
