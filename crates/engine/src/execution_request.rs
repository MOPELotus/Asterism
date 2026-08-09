use asterism_domain::{
    AuditActor, Execution, ExecutionId, ExecutionState, OrchestrationState, RemoteState,
    RequestSource, Task, TaskCapability, TaskId, Timestamp, UserId,
};
use asterism_storage::{
    ExecutionRepository, ExecutionScheduleOutcome, ExecutionScheduleRequest, StorageError,
    TaskQueryRepository,
};

use crate::{
    AssessmentGuardError, ExecutionTransitionError, FormalAssessmentPolicy, TaskAction,
    authorize_task_action, transition_execution,
};

#[derive(Clone, Debug)]
pub struct ExecuteTaskCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub request_source: RequestSource,
    pub actor: AuditActor,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub requested_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequestResult {
    pub execution: Execution,
    pub created: bool,
}

#[derive(Clone, Debug)]
pub struct ExecutionRequestService<Q, E> {
    tasks: Q,
    executions: E,
    formal_policy: FormalAssessmentPolicy,
}

impl<Q, E> ExecutionRequestService<Q, E> {
    pub const fn new(tasks: Q, executions: E, formal_policy: FormalAssessmentPolicy) -> Self {
        Self {
            tasks,
            executions,
            formal_policy,
        }
    }
}

impl<Q, E> ExecutionRequestService<Q, E>
where
    Q: TaskQueryRepository,
    E: ExecutionRepository,
{
    /// Authorizes and schedules one owner-scoped Task execution. Scoped
    /// idempotency is checked before mutable Task state so a network replay can
    /// return the original Execution after the first request moved the Task.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionRequestError`] for ownership, capability, remote or
    /// orchestration conflicts, formal-assessment policy, idempotency reuse, or
    /// persistence failures.
    pub async fn execute(
        &self,
        command: ExecuteTaskCommand,
    ) -> Result<ExecutionRequestResult, ExecutionRequestError> {
        let scope = format!("user:{}", command.owner_id);
        if let Some(existing) = self
            .executions
            .find_idempotent_execution(&scope, &command.idempotency_key)
            .await?
        {
            return if existing.task_id == command.task_id
                && existing.requested_by == Some(command.owner_id)
            {
                Ok(ExecutionRequestResult {
                    execution: existing,
                    created: false,
                })
            } else {
                Err(ExecutionRequestError::IdempotencyConflict)
            };
        }

        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ExecutionRequestError::TaskNotFound)?;
        validate_task(&task, self.formal_policy)?;
        let mut execution = Execution {
            id: ExecutionId::new(),
            task_id: task.id,
            requested_by: Some(command.owner_id),
            request_source: command.request_source,
            quote_id: None,
            state: ExecutionState::Requested,
            scheduled_at: None,
            started_at: None,
            finished_at: None,
            created_at: command.requested_at,
        };
        transition_execution(
            &mut execution,
            ExecutionState::Scheduled,
            command.requested_at,
        )?;
        execution.scheduled_at = Some(command.requested_at);
        match self
            .executions
            .schedule_execution(ExecutionScheduleRequest {
                execution: &execution,
                expected_task_state: task.orchestration_state,
                idempotency_scope: &scope,
                idempotency_key: &command.idempotency_key,
                actor: command.actor,
                correlation_id: &command.correlation_id,
            })
            .await?
        {
            ExecutionScheduleOutcome::Created(execution) => Ok(ExecutionRequestResult {
                execution,
                created: true,
            }),
            ExecutionScheduleOutcome::Existing(execution) => Ok(ExecutionRequestResult {
                execution,
                created: false,
            }),
            ExecutionScheduleOutcome::IdempotencyConflict => {
                Err(ExecutionRequestError::IdempotencyConflict)
            }
            ExecutionScheduleOutcome::TaskStateConflict => {
                Err(ExecutionRequestError::TaskStateConflict)
            }
        }
    }
}

fn validate_task(
    task: &Task,
    formal_policy: FormalAssessmentPolicy,
) -> Result<(), ExecutionRequestError> {
    if !matches!(
        task.orchestration_state,
        OrchestrationState::Ready
            | OrchestrationState::WaitingApproval
            | OrchestrationState::Failed
    ) {
        return Err(ExecutionRequestError::TaskStateConflict);
    }
    if !matches!(
        task.remote_state,
        RemoteState::Pending | RemoteState::InProgress
    ) {
        return Err(ExecutionRequestError::RemoteStateNotExecutable);
    }
    if !task
        .capabilities
        .iter()
        .copied()
        .any(is_execution_capability)
    {
        return Err(ExecutionRequestError::UnsupportedTask);
    }
    authorize_task_action(task, TaskAction::Execute, formal_policy)?;
    if task
        .capabilities
        .contains(&TaskCapability::SubmissionExecute)
    {
        authorize_task_action(task, TaskAction::Submit, formal_policy)?;
    }
    Ok(())
}

const fn is_execution_capability(capability: TaskCapability) -> bool {
    matches!(
        capability,
        TaskCapability::ResourceExecution
            | TaskCapability::SubmissionExecute
            | TaskCapability::DurationReport
            | TaskCapability::Discussion
            | TaskCapability::Practice
    )
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionRequestError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task orchestration state changed or is not executable")]
    TaskStateConflict,
    #[error("task remote state is not executable")]
    RemoteStateNotExecutable,
    #[error("task advertises no executable capability")]
    UnsupportedTask,
    #[error("the idempotency key is already bound to another execution request")]
    IdempotencyConflict,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Transition(#[from] ExecutionTransitionError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}
