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
    pub requested_capabilities: Vec<TaskCapability>,
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
    // Keep authorization, fresh Task validation, frozen settings, pricing and
    // the single atomic scheduling request adjacent so no preflight result is
    // accidentally reused across a mutation boundary.
    #[allow(clippy::too_many_lines)]
    pub async fn execute(
        &self,
        mut command: ExecuteTaskCommand,
    ) -> Result<ExecutionRequestResult, ExecutionRequestError> {
        command.requested_capabilities =
            normalize_requested_capabilities(command.requested_capabilities)?;
        let scope = format!("user:{}", command.owner_id);
        if let Some(existing) = self
            .executions
            .find_idempotent_execution(&scope, &command.idempotency_key)
            .await?
        {
            return if existing.task_id == command.task_id
                && existing.requested_by == Some(command.owner_id)
                && existing.requested_capabilities == command.requested_capabilities
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
        validate_task(&task, &command.requested_capabilities, self.formal_policy)?;
        let (runtime_settings, runtime_settings_schema) = self
            .resolve_runtime_settings(command.owner_id, &task, command.requested_at)
            .await?;
        let ResolvedExecutionContract {
            verification_required,
            capability_plan,
            capability_call_starts,
            provider_plan_artifact,
            provider_state_exception,
        } = resolve_execution_contract(
            &task,
            &command.requested_capabilities,
            &self.providers,
            &runtime_settings.provider_id,
            &runtime_settings.resolved,
        )?;
        if !remote_state_is_executable(
            &task,
            &command.requested_capabilities,
            verification_required,
            provider_state_exception,
        ) {
            return Err(ExecutionRequestError::RemoteStateNotExecutable);
        }
        let submission_draft = self
            .resolve_submission_draft(
                command.owner_id,
                &task,
                &command.requested_capabilities,
                command.submission_draft_id,
                &runtime_settings,
            )
            .await?;
        let mut execution = Execution {
            id: ExecutionId::new(),
            task_id: task.id,
            requested_capabilities: command.requested_capabilities,
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
                capability_plan: &capability_plan,
                capability_call_starts: &capability_call_starts,
                provider_plan_artifact: provider_plan_artifact.as_ref(),
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
        requested_capabilities: &[TaskCapability],
        submission_draft_id: Option<SubmissionDraftId>,
        runtime_settings: &ExecutionRuntimeSettingsSnapshot,
    ) -> Result<Option<SubmissionDraft>, ExecutionRequestError> {
        let submission = requested_capabilities == [TaskCapability::SubmissionExecute];
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
    requested_capabilities: &[TaskCapability],
    formal_policy: FormalAssessmentPolicy,
) -> Result<(), ExecutionRequestError> {
    if !matches!(
        task.orchestration_state,
        OrchestrationState::Ready | OrchestrationState::Failed
    ) {
        return Err(ExecutionRequestError::TaskStateConflict);
    }
    if requested_capabilities.is_empty()
        || requested_capabilities
            .iter()
            .any(|capability| !task.capabilities.contains(capability))
    {
        return Err(ExecutionRequestError::UnsupportedTask);
    }
    authorize_task_action(task, TaskAction::Execute, formal_policy)?;
    if requested_capabilities.contains(&TaskCapability::SubmissionExecute) {
        authorize_task_action(task, TaskAction::Submit, formal_policy)?;
    }
    if requested_capabilities.contains(&TaskCapability::SubmissionExecute)
        && requested_capabilities != [TaskCapability::SubmissionExecute]
    {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    Ok(())
}

fn remote_state_is_executable(
    task: &Task,
    requested_capabilities: &[TaskCapability],
    verification_required: bool,
    provider_state_exception: bool,
) -> bool {
    matches!(
        task.remote_state,
        RemoteState::Pending | RemoteState::InProgress
    ) || (requested_capabilities == [TaskCapability::DurationReport]
        && matches!(
            task.remote_state,
            RemoteState::Completed | RemoteState::Unknown
        ))
        || (safe_verification_action(task, requested_capabilities, verification_required)
            && matches!(
                task.remote_state,
                RemoteState::Unknown | RemoteState::Completed
            ))
        || (provider_state_exception
            && !matches!(
                task.remote_state,
                RemoteState::Expired | RemoteState::Removed
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

fn safe_verification_action(
    task: &Task,
    actions: &[TaskCapability],
    verification_required: bool,
) -> bool {
    (actions == [TaskCapability::SubmissionExecute]
        && task
            .capabilities
            .contains(&TaskCapability::SubmissionVerify))
        || verification_required
}

fn resolve_execution_contract(
    task: &Task,
    requested_capabilities: &[TaskCapability],
    providers: &ProviderRegistry,
    provider_id: &asterism_domain::ProviderId,
    runtime_settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
) -> Result<ResolvedExecutionContract, ExecutionRequestError> {
    if requested_capabilities == [TaskCapability::SubmissionExecute] {
        return Ok(ResolvedExecutionContract {
            verification_required: false,
            capability_plan: requested_capabilities.to_vec(),
            capability_call_starts: vec![1],
            provider_plan_artifact: None,
            provider_state_exception: false,
        });
    }
    let provider = providers
        .get(provider_id)
        .ok_or(ExecutionRequestError::ProviderRuntimeUnavailable)?;
    let Some(capability) = provider.task_execution.as_ref() else {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    };
    let provider_plan = capability
        .execution_plan_snapshot(requested_capabilities, runtime_settings)
        .map_err(|_| ExecutionRequestError::ExecutionVerificationUnavailable)?;
    if provider_plan.provider_id() != provider_id {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    let calls = provider_plan.calls();
    if calls.is_empty()
        || calls.len() > 5
        || calls.iter().any(Vec::is_empty)
        || calls.iter().map(Vec::len).sum::<usize>() > 5
    {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    let mut call_starts = Vec::with_capacity(calls.len());
    let mut next_position = 1_usize;
    for call in calls {
        call_starts.push(
            u8::try_from(next_position)
                .map_err(|_| ExecutionRequestError::ExecutionVerificationUnavailable)?,
        );
        next_position += call.len();
    }
    let plan = calls.iter().flatten().copied().collect::<Vec<_>>();
    if plan.len() != requested_capabilities.len()
        || plan
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            != requested_capabilities
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    let verification = task.capabilities.contains(&TaskCapability::ExecutionVerify)
        && provider
            .metadata
            .advertises(ProviderCapability::ExecutionVerify)
        && capability.requires_execution_verification(requested_capabilities);
    let state_exception =
        capability.allows_execution_from_remote_state(requested_capabilities, task.remote_state);
    Ok(ResolvedExecutionContract {
        verification_required: verification,
        capability_plan: plan,
        capability_call_starts: call_starts,
        provider_plan_artifact: provider_plan.artifact().cloned(),
        provider_state_exception: state_exception,
    })
}

struct ResolvedExecutionContract {
    verification_required: bool,
    capability_plan: Vec<TaskCapability>,
    capability_call_starts: Vec<u8>,
    provider_plan_artifact: Option<asterism_provider_api::ProviderExecutionPlanArtifact>,
    provider_state_exception: bool,
}

fn normalize_requested_capabilities(
    mut capabilities: Vec<TaskCapability>,
) -> Result<Vec<TaskCapability>, ExecutionRequestError> {
    if capabilities.is_empty()
        || capabilities.len() > 5
        || capabilities
            .iter()
            .copied()
            .any(|capability| !is_execution_capability(capability))
    {
        return Err(ExecutionRequestError::InvalidCapabilitySelection);
    }
    capabilities.sort_unstable();
    if capabilities.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ExecutionRequestError::InvalidCapabilitySelection);
    }
    Ok(capabilities)
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
    #[error("requested capabilities must be a non-empty unique executable action set")]
    InvalidCapabilitySelection,
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
        let duration = task(RemoteState::Completed, vec![TaskCapability::DurationReport]);
        assert!(remote_state_is_executable(
            &duration,
            &[TaskCapability::DurationReport],
            false,
            false,
        ));
        let resource = task(
            RemoteState::Completed,
            vec![TaskCapability::ResourceExecution],
        );
        assert!(!remote_state_is_executable(
            &resource,
            &[TaskCapability::ResourceExecution],
            false,
            false,
        ));
        assert!(remote_state_is_executable(
            &resource,
            &[TaskCapability::ResourceExecution],
            true,
            false,
        ));
        let submission = task(
            RemoteState::Unknown,
            vec![
                TaskCapability::SubmissionExecute,
                TaskCapability::SubmissionVerify,
            ],
        );
        assert!(remote_state_is_executable(
            &submission,
            &[TaskCapability::SubmissionExecute],
            false,
            false,
        ));
    }

    #[test]
    fn provider_state_exception_never_reopens_terminally_unavailable_tasks() {
        let not_open = task(
            RemoteState::NotOpen,
            vec![TaskCapability::ResourceExecution],
        );
        assert!(!remote_state_is_executable(
            &not_open,
            &[TaskCapability::ResourceExecution],
            true,
            false,
        ));
        assert!(remote_state_is_executable(
            &not_open,
            &[TaskCapability::ResourceExecution],
            true,
            true,
        ));

        for remote_state in [RemoteState::Expired, RemoteState::Removed] {
            let unavailable = task(remote_state, vec![TaskCapability::ResourceExecution]);
            assert!(!remote_state_is_executable(
                &unavailable,
                &[TaskCapability::ResourceExecution],
                true,
                true,
            ));
        }
    }

    #[test]
    fn task_validation_uses_only_the_explicit_action_selection() {
        let valid = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(
            validate_task(
                &valid,
                &[TaskCapability::ResourceExecution],
                FormalAssessmentPolicy::default(),
            )
            .is_ok()
        );

        let without_progress = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(
            validate_task(
                &without_progress,
                &[TaskCapability::ResourceExecution],
                FormalAssessmentPolicy::default(),
            )
            .is_ok()
        );

        let multiple_actions = task(
            RemoteState::Pending,
            vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(
            validate_task(
                &multiple_actions,
                &[TaskCapability::DurationReport],
                FormalAssessmentPolicy::default(),
            )
            .is_ok()
        );

        let submission = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::SubmissionExecute,
                TaskCapability::ExecutionVerify,
            ],
        );
        assert!(matches!(
            validate_task(
                &submission,
                &[
                    TaskCapability::ResourceExecution,
                    TaskCapability::SubmissionExecute,
                ],
                FormalAssessmentPolicy::default(),
            ),
            Err(ExecutionRequestError::ExecutionVerificationUnavailable)
        ));
    }

    #[test]
    fn capability_selection_is_non_empty_unique_and_canonical() {
        assert_eq!(
            normalize_requested_capabilities(vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ])
            .unwrap(),
            vec![
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ]
        );
        assert!(matches!(
            normalize_requested_capabilities(vec![]),
            Err(ExecutionRequestError::InvalidCapabilitySelection)
        ));
        assert!(matches!(
            normalize_requested_capabilities(vec![
                TaskCapability::ResourceExecution,
                TaskCapability::ResourceExecution,
            ]),
            Err(ExecutionRequestError::InvalidCapabilitySelection)
        ));
        assert!(matches!(
            normalize_requested_capabilities(vec![TaskCapability::ProgressRead]),
            Err(ExecutionRequestError::InvalidCapabilitySelection)
        ));
    }
}
