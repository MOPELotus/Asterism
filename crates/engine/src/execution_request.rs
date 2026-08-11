use std::sync::Arc;

use asterism_domain::{
    AuditActor, Execution, ExecutionId, ExecutionState, OrchestrationState, RemoteState,
    RequestSource, SubmissionDraft, SubmissionDraftId, Task, TaskCapability, TaskId, Timestamp,
    UserId,
};
use asterism_provider_api::{ProviderCapability, ProviderRegistry, ProviderRuntimeSettingsSchema};
use asterism_storage::{
    ExecutionRepository, ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot,
    ExecutionScheduleOutcome, ExecutionScheduleRequest, ProviderAccountRuntimeRepository,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget, StorageError,
    SubmissionDraftRepository, TaskQueryRepository,
};

use crate::{
    AssessmentGuardError, ExecutionTransitionError, FormalAssessmentPolicy, TaskAction,
    authorize_task_action, transition_execution,
};

#[derive(Clone, Debug)]
pub struct ExecuteTaskCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub submission_draft_id: Option<SubmissionDraftId>,
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
pub struct ExecutionRequestService<Q, E, A, S, D> {
    tasks: Q,
    executions: E,
    accounts: A,
    settings: S,
    submission_drafts: D,
    providers: Arc<ProviderRegistry>,
    formal_policy: FormalAssessmentPolicy,
}

impl<Q, E, A, S, D> ExecutionRequestService<Q, E, A, S, D> {
    pub fn new(
        tasks: Q,
        executions: E,
        accounts: A,
        settings: S,
        submission_drafts: D,
        providers: Arc<ProviderRegistry>,
        formal_policy: FormalAssessmentPolicy,
    ) -> Self {
        Self {
            tasks,
            executions,
            accounts,
            settings,
            submission_drafts,
            providers,
            formal_policy,
        }
    }
}

impl<Q, E, A, S, D> ExecutionRequestService<Q, E, A, S, D>
where
    Q: TaskQueryRepository,
    E: ExecutionRepository,
    A: ProviderAccountRuntimeRepository,
    S: ProviderRuntimeSettingsRepository,
    D: SubmissionDraftRepository,
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
                && existing.submission_draft_id == command.submission_draft_id
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
        validate_execution_verification_contract(
            &task,
            &self.providers,
            &runtime_settings.provider_id,
        )?;
        let submission_draft = self
            .resolve_submission_draft(
                command.owner_id,
                &task,
                command.submission_draft_id,
                &runtime_settings,
            )
            .await?;
        let mut execution = Execution {
            id: ExecutionId::new(),
            task_id: task.id,
            submission_draft_id: submission_draft.as_ref().map(|draft| draft.id),
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
            ExecutionScheduleOutcome::SubmissionDraftConflict => {
                Err(ExecutionRequestError::SubmissionDraftConflict)
            }
            ExecutionScheduleOutcome::TaskStateConflict => {
                Err(ExecutionRequestError::TaskStateConflict)
            }
            ExecutionScheduleOutcome::RuntimeSettingsConflict => {
                Err(ExecutionRequestError::RuntimeSettingsConflict)
            }
        }
    }

    async fn resolve_submission_draft(
        &self,
        owner_id: UserId,
        task: &Task,
        submission_draft_id: Option<SubmissionDraftId>,
        runtime_settings: &ExecutionRuntimeSettingsSnapshot,
    ) -> Result<Option<SubmissionDraft>, ExecutionRequestError> {
        let submission = task
            .capabilities
            .contains(&TaskCapability::SubmissionExecute);
        if !submission {
            return if submission_draft_id.is_some() {
                Err(ExecutionRequestError::UnexpectedSubmissionDraft)
            } else {
                Ok(None)
            };
        }
        if !task
            .capabilities
            .contains(&TaskCapability::SubmissionVerify)
        {
            return Err(ExecutionRequestError::SubmissionVerificationUnavailable);
        }
        let submission_draft_id =
            submission_draft_id.ok_or(ExecutionRequestError::SubmissionDraftRequired)?;
        let draft = self
            .submission_drafts
            .find_owned_submission_draft(owner_id, submission_draft_id)
            .await?
            .ok_or(ExecutionRequestError::SubmissionDraftNotFound)?;
        if draft.task_id != task.id || draft.provider_id != runtime_settings.provider_id {
            return Err(ExecutionRequestError::SubmissionDraftConflict);
        }
        let provider = self
            .providers
            .get(&runtime_settings.provider_id)
            .ok_or(ExecutionRequestError::ProviderRuntimeUnavailable)?;
        if draft.provider_version != provider.metadata.implementation_version {
            return Err(ExecutionRequestError::SubmissionDraftVersionConflict);
        }
        if provider.submission_execute.is_none() || provider.submission_verify.is_none() {
            return Err(ExecutionRequestError::SubmissionVerificationUnavailable);
        }
        Ok(Some(draft))
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
        OrchestrationState::Ready | OrchestrationState::Failed
    ) {
        return Err(ExecutionRequestError::TaskStateConflict);
    }
    if !remote_state_is_executable(task) {
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
    if task.capabilities.contains(&TaskCapability::ExecutionVerify)
        && (!task.capabilities.contains(&TaskCapability::ProgressRead)
            || execution_actions(task).len() != 1
            || task
                .capabilities
                .contains(&TaskCapability::SubmissionExecute))
    {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    Ok(())
}

fn remote_state_is_executable(task: &Task) -> bool {
    let actions = execution_actions(task);
    matches!(
        task.remote_state,
        RemoteState::Pending | RemoteState::InProgress
    ) || (actions == [TaskCapability::DurationReport]
        && matches!(
            task.remote_state,
            RemoteState::Completed | RemoteState::Unknown
        ))
        || (safe_verification_action(task, &actions)
            && matches!(
                task.remote_state,
                RemoteState::Unknown | RemoteState::Completed
            ))
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

fn execution_actions(task: &Task) -> Vec<TaskCapability> {
    task.capabilities
        .iter()
        .copied()
        .filter(|capability| is_execution_capability(*capability))
        .collect()
}

fn safe_verification_action(task: &Task, actions: &[TaskCapability]) -> bool {
    (actions == [TaskCapability::SubmissionExecute]
        && task
            .capabilities
            .contains(&TaskCapability::SubmissionVerify))
        || (actions.len() == 1
            && actions != [TaskCapability::SubmissionExecute]
            && task.capabilities.contains(&TaskCapability::ExecutionVerify)
            && task.capabilities.contains(&TaskCapability::ProgressRead))
}

fn validate_execution_verification_contract(
    task: &Task,
    providers: &ProviderRegistry,
    provider_id: &asterism_domain::ProviderId,
) -> Result<(), ExecutionRequestError> {
    if !task.capabilities.contains(&TaskCapability::ExecutionVerify) {
        return Ok(());
    }
    let provider = providers
        .get(provider_id)
        .ok_or(ExecutionRequestError::ProviderRuntimeUnavailable)?;
    if !provider
        .metadata
        .advertises(ProviderCapability::ExecutionVerify)
        || provider.task_execution.is_none()
        || provider.task_progress.is_none()
    {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    Ok(())
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
    #[error("a SubmissionDraft is required for this task")]
    SubmissionDraftRequired,
    #[error("a SubmissionDraft cannot be supplied for this task")]
    UnexpectedSubmissionDraft,
    #[error("the SubmissionDraft was not found for this owner")]
    SubmissionDraftNotFound,
    #[error("the SubmissionDraft is foreign, stale, or already bound to another Execution")]
    SubmissionDraftConflict,
    #[error("the SubmissionDraft was built by another Provider implementation version")]
    SubmissionDraftVersionConflict,
    #[error("the task cannot be submitted because independent verification is unavailable")]
    SubmissionVerificationUnavailable,
    #[error(
        "the task mutation cannot run because independent execution verification is unavailable"
    )]
    ExecutionVerificationUnavailable,
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

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, OrchestrationState, ProviderAccountId, SourceType, TaskSnapshotId,
    };
    use chrono::Utc;

    use super::*;

    fn task(remote_state: RemoteState, capabilities: Vec<TaskCapability>) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            provider_account_id: ProviderAccountId::new(),
            course_id: None,
            remote_id: "remote-task".to_owned(),
            source_type: SourceType::Resource,
            assessment_class: AssessmentClass::Routine,
            title: "Task".to_owned(),
            remote_state,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None::<TaskSnapshotId>,
            capabilities,
        }
    }

    #[test]
    fn unknown_or_completed_remote_state_requires_a_safe_verification_action() {
        assert!(remote_state_is_executable(&task(
            RemoteState::Completed,
            vec![TaskCapability::DurationReport],
        )));
        assert!(!remote_state_is_executable(&task(
            RemoteState::Completed,
            vec![TaskCapability::ResourceExecution],
        )));
        assert!(remote_state_is_executable(&task(
            RemoteState::Unknown,
            vec![TaskCapability::DurationReport],
        )));
        assert!(!remote_state_is_executable(&task(
            RemoteState::Completed,
            vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ],
        )));
        for state in [RemoteState::Unknown, RemoteState::Completed] {
            assert!(remote_state_is_executable(&task(
                state,
                vec![
                    TaskCapability::ProgressRead,
                    TaskCapability::ResourceExecution,
                    TaskCapability::ExecutionVerify,
                ],
            )));
            assert!(remote_state_is_executable(&task(
                state,
                vec![
                    TaskCapability::SubmissionExecute,
                    TaskCapability::SubmissionVerify,
                ],
            )));
        }
    }

    #[test]
    fn execution_verification_requires_one_non_submission_action_and_progress() {
        let valid = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(validate_task(&valid, FormalAssessmentPolicy::default()).is_ok());

        let missing_progress = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(matches!(
            validate_task(&missing_progress, FormalAssessmentPolicy::default()),
            Err(ExecutionRequestError::ExecutionVerificationUnavailable)
        ));

        let submission = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ProgressRead,
                TaskCapability::SubmissionExecute,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(matches!(
            validate_task(&submission, FormalAssessmentPolicy::default()),
            Err(ExecutionRequestError::ExecutionVerificationUnavailable)
        ));
    }
}
