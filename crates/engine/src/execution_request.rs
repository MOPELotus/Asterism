use std::sync::Arc;

use asterism_domain::{
    AuditActor, Execution, ExecutionId, ExecutionState, OrchestrationState, RemoteState,
    RequestSource, Task, TaskCapability, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{ProviderRegistry, ProviderRuntimeSettingsSchema};
use asterism_storage::{
    ExecutionRepository, ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot,
    ExecutionScheduleOutcome, ExecutionScheduleRequest, ProviderAccountRuntimeRepository,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget, StorageError,
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
pub struct ExecutionRequestService<Q, E, A, S> {
    tasks: Q,
    executions: E,
    accounts: A,
    settings: S,
    providers: Arc<ProviderRegistry>,
    formal_policy: FormalAssessmentPolicy,
}

impl<Q, E, A, S> ExecutionRequestService<Q, E, A, S> {
    pub fn new(
        tasks: Q,
        executions: E,
        accounts: A,
        settings: S,
        providers: Arc<ProviderRegistry>,
        formal_policy: FormalAssessmentPolicy,
    ) -> Self {
        Self {
            tasks,
            executions,
            accounts,
            settings,
            providers,
            formal_policy,
        }
    }
}

impl<Q, E, A, S> ExecutionRequestService<Q, E, A, S>
where
    Q: TaskQueryRepository,
    E: ExecutionRepository,
    A: ProviderAccountRuntimeRepository,
    S: ProviderRuntimeSettingsRepository,
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
        let (runtime_settings, runtime_settings_schema) = self
            .resolve_runtime_settings(command.owner_id, &task, command.requested_at)
            .await?;
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
                billing: None,
                runtime_settings: Some(ExecutionRuntimeSettingsResolution {
                    snapshot: &runtime_settings,
                    schema: &runtime_settings_schema,
                }),
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
            ExecutionScheduleOutcome::RuntimeSettingsConflict => {
                Err(ExecutionRequestError::RuntimeSettingsConflict)
            }
        }
    }

    async fn resolve_runtime_settings(
        &self,
        owner_id: UserId,
        task: &Task,
        captured_at: Timestamp,
    ) -> Result<
        (
            ExecutionRuntimeSettingsSnapshot,
            ProviderRuntimeSettingsSchema,
        ),
        ExecutionRequestError,
    > {
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == owner_id)
            .ok_or(ExecutionRequestError::TaskNotFound)?;
        let schema = self
            .providers
            .get(&account.provider_id)
            .map(|provider| provider.runtime_settings.clone())
            .ok_or(ExecutionRequestError::ProviderRuntimeUnavailable)?;
        let provider_settings = self
            .settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Provider {
                provider_id: account.provider_id.clone(),
            })
            .await?;
        let account_settings = self
            .settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id: account.provider_id.clone(),
                provider_account_id: account.id,
            })
            .await?;
        let task_settings = self
            .settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Task {
                provider_id: account.provider_id.clone(),
                provider_account_id: account.id,
                task_id: task.id,
            })
            .await?;
        let (resolved, sources) = schema
            .resolve_with_sources(
                provider_settings.as_ref().map(|record| &record.patch),
                account_settings.as_ref().map(|record| &record.patch),
                task_settings.as_ref().map(|record| &record.patch),
            )
            .map_err(|_| ExecutionRequestError::ProviderRuntimeUnavailable)?;
        Ok((
            ExecutionRuntimeSettingsSnapshot {
                provider_id: account.provider_id,
                resolved,
                sources,
                provider_revision: provider_settings.as_ref().map(|record| record.revision),
                provider_account_revision: account_settings.as_ref().map(|record| record.revision),
                task_revision: task_settings.as_ref().map(|record| record.revision),
                captured_at,
            },
            schema,
        ))
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
    #[error("the registered Provider runtime settings are unavailable or incompatible")]
    ProviderRuntimeUnavailable,
    #[error("Provider runtime settings changed while the execution was being scheduled")]
    RuntimeSettingsConflict,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Transition(#[from] ExecutionTransitionError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}
