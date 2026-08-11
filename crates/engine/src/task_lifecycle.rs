use asterism_domain::{
    AuditActor, ExecutionId, OrchestrationState, RequestSource, TaskId, TaskLifecycleAction,
    Timestamp, UserId,
};
use asterism_storage::{
    StorageError, TaskLifecycleMutation, TaskLifecycleMutationOutcome, TaskLifecycleReceipt,
    TaskLifecycleRepository, TaskQueryRepository,
};

use crate::validate_orchestration_transition;

#[derive(Clone, Debug)]
pub struct TaskLifecycleCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub action: TaskLifecycleAction,
    pub delayed_until: Option<Timestamp>,
    pub request_source: RequestSource,
    pub actor: AuditActor,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub requested_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLifecycleResult {
    pub task_id: TaskId,
    pub action: TaskLifecycleAction,
    pub task_state: OrchestrationState,
    pub affected_execution_id: Option<ExecutionId>,
    pub delayed_until: Option<Timestamp>,
    pub created: bool,
}

#[derive(Clone, Debug)]
pub struct TaskLifecycleService<Q, L> {
    tasks: Q,
    lifecycle: L,
}

impl<Q, L> TaskLifecycleService<Q, L> {
    pub const fn new(tasks: Q, lifecycle: L) -> Self {
        Self { tasks, lifecycle }
    }
}

impl<Q, L> TaskLifecycleService<Q, L>
where
    Q: TaskQueryRepository,
    L: TaskLifecycleRepository,
{
    /// Applies one owner-scoped orchestration action. Approval changes only
    /// Core scheduling state; it never weakens the independent formal
    /// assessment execution/submission guard.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLifecycleError`] for ownership, action/state, delay,
    /// idempotency or persistence conflicts.
    pub async fn apply(
        &self,
        command: TaskLifecycleCommand,
    ) -> Result<TaskLifecycleResult, TaskLifecycleError> {
        if let Some(receipt) = self
            .lifecycle
            .find_task_lifecycle_receipt(command.owner_id, &command.idempotency_key)
            .await?
        {
            return replay_result(&command, &receipt);
        }

        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(TaskLifecycleError::TaskNotFound)?;
        let target_task_state = validate_action(
            task.orchestration_state,
            command.action,
            command.delayed_until,
            command.requested_at,
        )?;
        let outcome = self
            .lifecycle
            .apply_task_lifecycle_mutation(TaskLifecycleMutation {
                owner_id: command.owner_id,
                task_id: command.task_id,
                action: command.action,
                expected_task_state: task.orchestration_state,
                target_task_state,
                delayed_until: command.delayed_until,
                request_source: command.request_source,
                actor: command.actor,
                idempotency_key: &command.idempotency_key,
                correlation_id: &command.correlation_id,
                at: command.requested_at,
            })
            .await?;
        match outcome {
            TaskLifecycleMutationOutcome::Applied(receipt) => Ok(result(&receipt, true)),
            TaskLifecycleMutationOutcome::Existing(receipt) => replay_result(&command, &receipt),
            TaskLifecycleMutationOutcome::IdempotencyConflict => {
                Err(TaskLifecycleError::IdempotencyConflict)
            }
            TaskLifecycleMutationOutcome::TaskNotFound => Err(TaskLifecycleError::TaskNotFound),
            TaskLifecycleMutationOutcome::StateConflict => {
                Err(TaskLifecycleError::TaskStateConflict)
            }
        }
    }
}

fn validate_action(
    source: OrchestrationState,
    action: TaskLifecycleAction,
    delayed_until: Option<Timestamp>,
    requested_at: Timestamp,
) -> Result<OrchestrationState, TaskLifecycleError> {
    match action {
        TaskLifecycleAction::Approve => {
            if delayed_until.is_some() || source != OrchestrationState::WaitingApproval {
                return Err(TaskLifecycleError::TaskStateConflict);
            }
            validate_transition(source, OrchestrationState::Ready)
        }
        TaskLifecycleAction::Cancel => {
            if delayed_until.is_some()
                || matches!(
                    source,
                    OrchestrationState::Running | OrchestrationState::Recovering
                )
            {
                return Err(TaskLifecycleError::TaskStateConflict);
            }
            validate_transition(source, OrchestrationState::Cancelled)
        }
        TaskLifecycleAction::Ignore => {
            if delayed_until.is_some() {
                return Err(TaskLifecycleError::InvalidDelay);
            }
            validate_transition(source, OrchestrationState::Ignored)
        }
        TaskLifecycleAction::Delay => {
            let delayed_until = delayed_until.ok_or(TaskLifecycleError::InvalidDelay)?;
            if source != OrchestrationState::Scheduled || delayed_until <= requested_at {
                return Err(if source == OrchestrationState::Scheduled {
                    TaskLifecycleError::InvalidDelay
                } else {
                    TaskLifecycleError::TaskStateConflict
                });
            }
            Ok(OrchestrationState::Scheduled)
        }
    }
}

fn validate_transition(
    source: OrchestrationState,
    target: OrchestrationState,
) -> Result<OrchestrationState, TaskLifecycleError> {
    validate_orchestration_transition(source, target)
        .map(|()| target)
        .map_err(|_| TaskLifecycleError::TaskStateConflict)
}

fn replay_result(
    command: &TaskLifecycleCommand,
    receipt: &TaskLifecycleReceipt,
) -> Result<TaskLifecycleResult, TaskLifecycleError> {
    if receipt.task_id != command.task_id
        || receipt.action != command.action
        || receipt.delayed_until != command.delayed_until
    {
        return Err(TaskLifecycleError::IdempotencyConflict);
    }
    Ok(result(receipt, false))
}

fn result(receipt: &TaskLifecycleReceipt, created: bool) -> TaskLifecycleResult {
    TaskLifecycleResult {
        task_id: receipt.task_id,
        action: receipt.action,
        task_state: receipt.result_task_state,
        affected_execution_id: receipt.affected_execution_id,
        delayed_until: receipt.delayed_until,
        created,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TaskLifecycleError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task orchestration state changed or does not permit this action")]
    TaskStateConflict,
    #[error("delay requires a future timestamp for a scheduled task")]
    InvalidDelay,
    #[error("the idempotency key is already bound to another task action")]
    IdempotencyConflict,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn approve_only_releases_waiting_approval_to_ready() {
        let now = Utc::now();
        assert_eq!(
            validate_action(
                OrchestrationState::WaitingApproval,
                TaskLifecycleAction::Approve,
                None,
                now,
            )
            .unwrap(),
            OrchestrationState::Ready
        );
        assert!(matches!(
            validate_action(
                OrchestrationState::Ready,
                TaskLifecycleAction::Approve,
                None,
                now,
            ),
            Err(TaskLifecycleError::TaskStateConflict)
        ));
    }

    #[test]
    fn running_remote_mutation_cannot_be_cancelled_from_control_plane() {
        assert!(matches!(
            validate_action(
                OrchestrationState::Running,
                TaskLifecycleAction::Cancel,
                None,
                Utc::now(),
            ),
            Err(TaskLifecycleError::TaskStateConflict)
        ));
    }

    #[test]
    fn delay_requires_a_future_time_and_keeps_scheduled_state() {
        let now = Utc::now();
        assert_eq!(
            validate_action(
                OrchestrationState::Scheduled,
                TaskLifecycleAction::Delay,
                Some(now + Duration::hours(2)),
                now,
            )
            .unwrap(),
            OrchestrationState::Scheduled
        );
        assert!(matches!(
            validate_action(
                OrchestrationState::Scheduled,
                TaskLifecycleAction::Delay,
                Some(now),
                now,
            ),
            Err(TaskLifecycleError::InvalidDelay)
        ));
    }
}
