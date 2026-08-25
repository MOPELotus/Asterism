use std::sync::Arc;

use asterism_domain::{
    AuditActor, CreditAmount, CreditReservation, CreditReservationId, CreditReservationState,
    Execution, ExecutionId, ExecutionInvocationDraftId, ExecutionState, OrchestrationState,
    PriceQuote, PriceQuoteId, RemoteState, RequestSource, ScoreImprovementState,
    StrictCompletionState, SubmissionDraft, SubmissionDraftId, Task, TaskCapability, TaskId,
    Timestamp, UserId,
};
use asterism_provider_api::{
    ExecutionInvocationPreparationRequest, ExecutionPlanningRequest,
    MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES, ProviderCapability, ProviderContext,
    ProviderRegistry, ProviderRuntimeSettingsSchema,
};
use asterism_secrets::{SecretAccess, SecretActor, SecretValue};
use asterism_storage::{
    CompletionWorkflowRepository, ExecutionBillingReservation,
    ExecutionInvocationDraftCreateOutcome, ExecutionInvocationDraftCreateRequest,
    ExecutionInvocationDraftRecord, ExecutionInvocationDraftRepositoryFactory, ExecutionRepository,
    ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot, ExecutionScheduleOutcome,
    ExecutionScheduleRequest, ExecutionScoreImprovementRetakeRequest,
    ExecutionStrictCompletionRetryRequest, ProviderAccountRuntimeRepository,
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
    pub invocation_draft_id: Option<ExecutionInvocationDraftId>,
    pub strict_completion_retry: Option<ExecutionStrictCompletionRetryRequest>,
    pub score_improvement_retake: Option<ExecutionScoreImprovementRetakeRequest>,
    pub billing: Option<ExecutionBillingInput>,
    pub request_source: RequestSource,
    pub actor: AuditActor,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub requested_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionBillingInput {
    pub amount: CreditAmount,
    pub pricing_revision: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequestResult {
    pub execution: Execution,
    pub created: bool,
}

#[derive(Debug)]
pub struct PrepareExecutionInvocationCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub requested_capabilities: Vec<TaskCapability>,
    pub submission_draft_id: Option<SubmissionDraftId>,
    pub input_type: String,
    pub raw_input: SecretValue,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionInvocationDraftResult {
    pub record: ExecutionInvocationDraftRecord,
    pub created: bool,
}

#[derive(Clone)]
pub struct ExecutionRequestService<Q, E, A, S, D> {
    tasks: Q,
    executions: E,
    accounts: A,
    settings: S,
    submission_drafts: D,
    providers: Arc<ProviderRegistry>,
    invocation_drafts: Option<Arc<dyn ExecutionInvocationDraftRepositoryFactory>>,
    formal_policy: FormalAssessmentPolicy,
}

impl<Q, E, A, S, D> std::fmt::Debug for ExecutionRequestService<Q, E, A, S, D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionRequestService")
            .field("tasks", &"configured")
            .field("executions", &"configured")
            .field("accounts", &"configured")
            .field("settings", &"configured")
            .field("submission_drafts", &"configured")
            .field("providers", &self.providers)
            .field("invocation_drafts", &self.invocation_drafts.is_some())
            .field("formal_policy", &self.formal_policy)
            .finish()
    }
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
            invocation_drafts: None,
            formal_policy,
        }
    }

    #[must_use]
    pub fn with_execution_invocation_drafts(
        mut self,
        drafts: Arc<dyn ExecutionInvocationDraftRepositoryFactory>,
    ) -> Self {
        self.invocation_drafts = Some(drafts);
        self
    }
}

impl<Q, E, A, S, D> ExecutionRequestService<Q, E, A, S, D>
where
    Q: TaskQueryRepository,
    E: ExecutionRepository + CompletionWorkflowRepository,
    A: ProviderAccountRuntimeRepository,
    S: ProviderRuntimeSettingsRepository,
    D: SubmissionDraftRepository,
{
    /// Freshly validates and encrypts one Provider-private invocation before
    /// any Execution or remote mutation exists.
    ///
    /// # Errors
    ///
    /// Fails closed on owner/account/Task/Draft drift, unsupported Provider
    /// preparation, oversized input, or idempotency conflict.
    #[allow(
        clippy::too_many_lines,
        reason = "preparation keeps authorization, Provider validation, encryption and idempotent persistence in one fail-closed flow"
    )]
    pub async fn prepare_invocation(
        &self,
        mut command: PrepareExecutionInvocationCommand,
    ) -> Result<ExecutionInvocationDraftResult, ExecutionRequestError> {
        command.requested_capabilities =
            normalize_requested_capabilities(command.requested_capabilities)?;
        if command.raw_input.expose_secret().is_empty()
            || command.raw_input.expose_secret().len() > MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES
            || !valid_invocation_input_type(&command.input_type)
        {
            return Err(ExecutionRequestError::InvalidInvocationInput);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ExecutionRequestError::TaskNotFound)?;
        validate_task(
            &task,
            &command.requested_capabilities,
            true,
            false,
            false,
            self.formal_policy,
        )?;
        let (runtime_settings, _, context) = self
            .resolve_runtime_settings(
                command.owner_id,
                &task,
                &command.correlation_id,
                command.created_at,
            )
            .await?;
        if !command
            .input_type
            .strip_prefix(runtime_settings.provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
        {
            return Err(ExecutionRequestError::InvalidInvocationInput);
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
        let provider = self
            .providers
            .get(&runtime_settings.provider_id)
            .ok_or(ExecutionRequestError::ProviderRuntimeUnavailable)?;
        let capability = provider
            .task_execution
            .as_ref()
            .ok_or(ExecutionRequestError::ExecutionVerificationUnavailable)?;
        let resolved_settings = provider
            .runtime_settings
            .hydrate_frozen_core_defaults(&runtime_settings.resolved)
            .map_err(|_| ExecutionRequestError::ProviderRuntimeUnavailable)?;
        let prepared = capability
            .prepare_private_invocation(
                &context,
                &ExecutionInvocationPreparationRequest {
                    task_id: task.id,
                    remote_task_id: &task.remote_id,
                    course_id: task.course_id,
                    requested_capabilities: &command.requested_capabilities,
                    submission_draft: submission_draft.as_ref(),
                    input_type: &command.input_type,
                    raw_input: &command.raw_input,
                    runtime_settings: &resolved_settings,
                },
            )
            .await
            .map_err(|_| ExecutionRequestError::InvocationPreparationFailed)?;
        let (provider_plan_artifact, private_input) = prepared.into_parts();
        if provider_plan_artifact.provider_id() != &runtime_settings.provider_id
            || private_input.provider_id() != &runtime_settings.provider_id
        {
            return Err(ExecutionRequestError::InvocationPreparationFailed);
        }
        let factory = self
            .invocation_drafts
            .as_ref()
            .ok_or(ExecutionRequestError::InvocationDraftUnavailable)?;
        let access = SecretAccess {
            actor: SecretActor::CoreService("execution-invocation-preparation"),
            correlation_id: command.correlation_id.clone(),
            reason: "persist immutable Provider-private execution invocation".to_owned(),
        };
        let scope = format!("user:{}", command.owner_id);
        let outcome = factory
            .for_provider(runtime_settings.provider_id.clone())
            .create_execution_invocation_draft(ExecutionInvocationDraftCreateRequest {
                draft_id: ExecutionInvocationDraftId::new(),
                owner_user_id: command.owner_id,
                provider_account_id: task.provider_account_id,
                course_id: task.course_id,
                task_id: task.id,
                provider_version: &provider.metadata.implementation_version,
                requested_capabilities: &command.requested_capabilities,
                submission_draft_id: command.submission_draft_id,
                provider_plan_artifact: &provider_plan_artifact,
                private_input: &private_input,
                created_at: command.created_at,
                idempotency_scope: &scope,
                idempotency_key: &command.idempotency_key,
                correlation_id: &command.correlation_id,
                access: &access,
            })
            .await
            .map_err(|error| match error {
                asterism_secrets::SecretStoreError::VersionConflict => {
                    ExecutionRequestError::IdempotencyConflict
                }
                other => ExecutionRequestError::SecretStore(other),
            })?;
        Ok(match outcome {
            ExecutionInvocationDraftCreateOutcome::Created(record) => {
                ExecutionInvocationDraftResult {
                    record,
                    created: true,
                }
            }
            ExecutionInvocationDraftCreateOutcome::AlreadyExists(record) => {
                ExecutionInvocationDraftResult {
                    record,
                    created: false,
                }
            }
        })
    }

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
            let persisted_confirmation = self
                .executions
                .find_execution_strict_completion_retry_confirmation(existing.id)
                .await?;
            let confirmation_matches =
                match (persisted_confirmation, command.strict_completion_retry) {
                    (None, None) => true,
                    (Some(persisted), Some(requested)) => {
                        persisted.workflow_id == requested.workflow_id
                            && persisted.workflow_revision == requested.expected_revision
                            && persisted.confirmed_by == command.owner_id
                    }
                    _ => false,
                };
            let invocation_matches = self
                .executions
                .find_execution_invocation_draft_id(existing.id)
                .await?
                == command.invocation_draft_id;
            let persisted_retake = self
                .executions
                .find_execution_score_improvement_retake_confirmation(existing.id)
                .await?;
            let retake_matches = match (persisted_retake, command.score_improvement_retake) {
                (None, None) => true,
                (Some(persisted), Some(requested)) => {
                    persisted.workflow_id == requested.workflow_id
                        && persisted.workflow_revision == requested.expected_revision
                        && persisted.confirmed_by == command.owner_id
                }
                _ => false,
            };
            return if confirmation_matches
                && retake_matches
                && invocation_matches
                && existing.task_id == command.task_id
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
        validate_task(
            &task,
            &command.requested_capabilities,
            command.invocation_draft_id.is_some(),
            command.strict_completion_retry.is_some(),
            command.score_improvement_retake.is_some(),
            self.formal_policy,
        )?;
        self.validate_strict_completion_retry(
            command.owner_id,
            &task,
            &command.requested_capabilities,
            command.strict_completion_retry,
        )
        .await?;
        self.validate_score_improvement_retake(
            command.owner_id,
            &task,
            command.score_improvement_retake,
        )
        .await?;
        let (runtime_settings, runtime_settings_schema, provider_context) = self
            .resolve_runtime_settings(
                command.owner_id,
                &task,
                &command.correlation_id,
                command.requested_at,
            )
            .await?;
        let execution_id = ExecutionId::new();
        let invocation_draft = self
            .resolve_invocation_draft(
                command.owner_id,
                &task,
                &command.requested_capabilities,
                command.submission_draft_id,
                command.invocation_draft_id,
                &runtime_settings,
            )
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
            &provider_context,
            execution_id,
            invocation_draft
                .as_ref()
                .map(|record| &record.provider_plan_artifact),
        )
        .await?;
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
            id: execution_id,
            task_id: task.id,
            requested_capabilities: command.requested_capabilities,
            submission_draft_id: submission_draft.as_ref().map(|draft| draft.id),
            requested_by: Some(command.owner_id),
            request_source: command.request_source,
            quote_id: command.billing.as_ref().map(|_| PriceQuoteId::new()),
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
        let quote = command.billing.as_ref().map(|billing| PriceQuote {
            id: execution.quote_id.expect("billing quote id is set above"),
            task_id: task.id,
            amount: billing.amount,
            pricing_revision: billing.pricing_revision.clone(),
            reason: billing.reason.clone(),
            created_at: command.requested_at,
        });
        let reservation = quote.as_ref().map(|quote| CreditReservation {
            id: CreditReservationId::new(),
            user_id: command.owner_id,
            quote_id: quote.id,
            execution_id,
            amount: quote.amount,
            state: CreditReservationState::Reserved,
            created_at: command.requested_at,
            updated_at: command.requested_at,
        });
        let billing = quote
            .as_ref()
            .zip(reservation.as_ref())
            .map(|(quote, reservation)| ExecutionBillingReservation { quote, reservation });
        match self
            .executions
            .schedule_execution(ExecutionScheduleRequest {
                execution: &execution,
                capability_plan: &capability_plan,
                capability_call_starts: &capability_call_starts,
                provider_plan_artifact: provider_plan_artifact.as_ref(),
                invocation_draft_id: command.invocation_draft_id,
                billing,
                runtime_settings: Some(ExecutionRuntimeSettingsResolution {
                    snapshot: &runtime_settings,
                    schema: &runtime_settings_schema,
                }),
                strict_completion_retry: command.strict_completion_retry,
                score_improvement_retake: command.score_improvement_retake,
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
            ExecutionScheduleOutcome::InvocationDraftConflict => {
                Err(ExecutionRequestError::InvocationDraftConflict)
            }
            ExecutionScheduleOutcome::TaskStateConflict => {
                Err(ExecutionRequestError::TaskStateConflict)
            }
            ExecutionScheduleOutcome::RuntimeSettingsConflict => {
                Err(ExecutionRequestError::RuntimeSettingsConflict)
            }
            ExecutionScheduleOutcome::StrictCompletionRetryConflict => {
                Err(ExecutionRequestError::StrictCompletionRetryConflict)
            }
            ExecutionScheduleOutcome::ScoreImprovementRetakeConflict => {
                Err(ExecutionRequestError::ScoreImprovementRetakeConflict)
            }
        }
    }

    async fn resolve_invocation_draft(
        &self,
        owner_id: UserId,
        task: &Task,
        requested_capabilities: &[TaskCapability],
        submission_draft_id: Option<SubmissionDraftId>,
        invocation_draft_id: Option<ExecutionInvocationDraftId>,
        runtime_settings: &ExecutionRuntimeSettingsSnapshot,
    ) -> Result<Option<ExecutionInvocationDraftRecord>, ExecutionRequestError> {
        let Some(draft_id) = invocation_draft_id else {
            return Ok(None);
        };
        let factory = self
            .invocation_drafts
            .as_ref()
            .ok_or(ExecutionRequestError::InvocationDraftUnavailable)?;
        let repository = factory.for_provider(runtime_settings.provider_id.clone());
        let record = repository
            .find_owned_execution_invocation_draft(owner_id, draft_id)
            .await?
            .ok_or(ExecutionRequestError::InvocationDraftNotFound)?;
        let provider = self
            .providers
            .get(&runtime_settings.provider_id)
            .ok_or(ExecutionRequestError::ProviderRuntimeUnavailable)?;
        if record.claimed_execution_id.is_some()
            || record.claimed_at.is_some()
            || record.draft.owner_user_id != owner_id
            || record.draft.provider_account_id != task.provider_account_id
            || record.draft.course_id != task.course_id
            || record.draft.task_id != task.id
            || record.draft.provider_id != runtime_settings.provider_id
            || record.draft.provider_version != provider.metadata.implementation_version
            || record.draft.requested_capabilities != requested_capabilities
            || record.draft.submission_draft_id != submission_draft_id
            || record.provider_plan_artifact.provider_id() != &runtime_settings.provider_id
            || record.provider_plan_artifact.artifact_digest() != record.draft.plan_artifact_digest
        {
            return Err(ExecutionRequestError::InvocationDraftConflict);
        }
        Ok(Some(record))
    }

    async fn resolve_submission_draft(
        &self,
        owner_id: UserId,
        task: &Task,
        requested_capabilities: &[TaskCapability],
        submission_draft_id: Option<SubmissionDraftId>,
        runtime_settings: &ExecutionRuntimeSettingsSnapshot,
    ) -> Result<Option<SubmissionDraft>, ExecutionRequestError> {
        let submission = requested_capabilities.contains(&TaskCapability::SubmissionExecute);
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

    async fn validate_strict_completion_retry(
        &self,
        owner_id: UserId,
        task: &Task,
        requested_capabilities: &[TaskCapability],
        confirmation: Option<ExecutionStrictCompletionRetryRequest>,
    ) -> Result<(), ExecutionRequestError> {
        let workflow = self
            .executions
            .find_owned_strict_completion_workflow(owner_id, task.id)
            .await?;
        let retry_required = workflow.as_ref().is_some_and(|record| {
            record.workflow.state == StrictCompletionState::Active
                && record.workflow.attempts_started > 0
        }) && (task.assessment_class
            == asterism_domain::AssessmentClass::Formal
            || requested_capabilities.contains(&TaskCapability::SubmissionExecute));
        let valid = match (retry_required, workflow, confirmation) {
            (false, _, None) => true,
            (true, Some(record), Some(confirmation)) => {
                record.workflow.id == confirmation.workflow_id
                    && record.revision == confirmation.expected_revision
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(ExecutionRequestError::StrictCompletionRetryConflict)
        }
    }

    async fn validate_score_improvement_retake(
        &self,
        owner_id: UserId,
        task: &Task,
        confirmation: Option<ExecutionScoreImprovementRetakeRequest>,
    ) -> Result<(), ExecutionRequestError> {
        let Some(confirmation) = confirmation else {
            return Ok(());
        };
        let record = self
            .executions
            .find_owned_score_improvement_workflow(owner_id, task.id)
            .await?
            .ok_or(ExecutionRequestError::ScoreImprovementRetakeConflict)?;
        if task.orchestration_state == OrchestrationState::Succeeded
            && matches!(
                task.remote_state,
                RemoteState::Pending | RemoteState::InProgress
            )
            && record.workflow.state == ScoreImprovementState::Ready
            && record.workflow.id == confirmation.workflow_id
            && record.revision == confirmation.expected_revision
        {
            Ok(())
        } else {
            Err(ExecutionRequestError::ScoreImprovementRetakeConflict)
        }
    }

    async fn resolve_runtime_settings(
        &self,
        owner_id: UserId,
        task: &Task,
        correlation_id: &str,
        captured_at: Timestamp,
    ) -> Result<
        (
            ExecutionRuntimeSettingsSnapshot,
            ProviderRuntimeSettingsSchema,
            ProviderContext,
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
        let completion_policy = schema
            .completion_policy_snapshot(&resolved, captured_at)
            .map_err(|_| ExecutionRequestError::ProviderRuntimeUnavailable)?;
        let provider_id = account.provider_id.clone();
        Ok((
            ExecutionRuntimeSettingsSnapshot {
                provider_id: provider_id.clone(),
                resolved,
                sources,
                completion_policy,
                provider_revision: provider_settings.as_ref().map(|record| record.revision),
                provider_account_revision: account_settings.as_ref().map(|record| record.revision),
                task_revision: task_settings.as_ref().map(|record| record.revision),
                captured_at,
            },
            schema,
            ProviderContext {
                provider_id,
                account_id: account.id,
                credential_refs: account.credential_refs,
                correlation_id: correlation_id.to_owned(),
            },
        ))
    }
}

fn validate_task(
    task: &Task,
    requested_capabilities: &[TaskCapability],
    private_invocation_selected: bool,
    strict_completion_retry_confirmed: bool,
    score_improvement_retake_confirmed: bool,
    formal_policy: FormalAssessmentPolicy,
) -> Result<(), ExecutionRequestError> {
    if !(matches!(
        task.orchestration_state,
        OrchestrationState::Discovered | OrchestrationState::Ready | OrchestrationState::Failed
    ) || strict_completion_retry_confirmed
        && task.orchestration_state == OrchestrationState::HumanRequired
        || score_improvement_retake_confirmed
            && task.orchestration_state == OrchestrationState::Succeeded)
    {
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
    if private_invocation_selected && requested_capabilities == [TaskCapability::SubmissionExecute]
    {
        return Err(ExecutionRequestError::ExecutionVerificationUnavailable);
    }
    if requested_capabilities.contains(&TaskCapability::SubmissionExecute)
        && requested_capabilities != [TaskCapability::SubmissionExecute]
        && !private_invocation_selected
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
            | TaskCapability::ArtifactUpload
            | TaskCapability::OralSubmission
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

#[allow(
    clippy::too_many_arguments,
    reason = "execution contract resolution binds all immutable Provider and scheduling inputs explicitly"
)]
async fn resolve_execution_contract(
    task: &Task,
    requested_capabilities: &[TaskCapability],
    providers: &ProviderRegistry,
    provider_id: &asterism_domain::ProviderId,
    runtime_settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
    context: &ProviderContext,
    execution_id: ExecutionId,
    invocation_plan_artifact: Option<&asterism_provider_api::ProviderExecutionPlanArtifact>,
) -> Result<ResolvedExecutionContract, ExecutionRequestError> {
    if requested_capabilities == [TaskCapability::SubmissionExecute]
        && invocation_plan_artifact.is_none()
    {
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
    if context.provider_id != *provider_id || context.account_id != task.provider_account_id {
        return Err(ExecutionRequestError::ProviderRuntimeUnavailable);
    }
    let planning_request = ExecutionPlanningRequest {
        execution_id,
        task_id: task.id,
        remote_task_id: &task.remote_id,
        course_id: task.course_id,
        requested_capabilities,
        runtime_settings,
    };
    let provider_plan = if let Some(artifact) = invocation_plan_artifact {
        asterism_provider_api::ProviderExecutionPlan::try_new(
            provider_id.clone(),
            capability
                .execution_call_plan(requested_capabilities, runtime_settings)
                .map_err(|_| ExecutionRequestError::ExecutionVerificationUnavailable)?,
            Some(artifact.clone()),
        )
        .map_err(|_| ExecutionRequestError::ExecutionVerificationUnavailable)?
    } else {
        capability
            .prepare_execution_plan(context, &planning_request)
            .await
            .map_err(|_| ExecutionRequestError::ExecutionVerificationUnavailable)?
    };
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

fn valid_invocation_input_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
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
    #[error("the encrypted execution invocation draft repository is unavailable")]
    InvocationDraftUnavailable,
    #[error("the execution invocation draft was not found for this owner")]
    InvocationDraftNotFound,
    #[error("the execution invocation draft is foreign, stale, or already claimed")]
    InvocationDraftConflict,
    #[error("the private execution invocation input is invalid or oversized")]
    InvalidInvocationInput,
    #[error("the Provider could not prepare the private execution invocation")]
    InvocationPreparationFailed,
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
    #[error("the Strict Completion retry confirmation is missing, stale, or invalid")]
    StrictCompletionRetryConflict,
    #[error("the Score Improvement retake confirmation is missing, stale, or invalid")]
    ScoreImprovementRetakeConflict,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Transition(#[from] ExecutionTransitionError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    SecretStore(#[from] asterism_secrets::SecretStoreError),
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
    #[allow(clippy::too_many_lines)]
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
                false,
                false,
                false,
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
                false,
                false,
                false,
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
                false,
                false,
                false,
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
                TaskCapability::ArtifactUpload,
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
                false,
                false,
                false,
                FormalAssessmentPolicy::default(),
            ),
            Err(ExecutionRequestError::ExecutionVerificationUnavailable)
        ));
        assert!(
            validate_task(
                &submission,
                &[
                    TaskCapability::SubmissionExecute,
                    TaskCapability::ArtifactUpload,
                ],
                true,
                false,
                false,
                FormalAssessmentPolicy::default(),
            )
            .is_ok()
        );
        assert!(matches!(
            validate_task(
                &submission,
                &[TaskCapability::SubmissionExecute],
                true,
                false,
                false,
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

    #[test]
    fn newly_discovered_task_can_be_scheduled_directly() {
        let mut discovered = task(
            RemoteState::Pending,
            vec![
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        );
        discovered.orchestration_state = OrchestrationState::Discovered;

        assert!(
            validate_task(
                &discovered,
                &[TaskCapability::ResourceExecution],
                false,
                false,
                false,
                FormalAssessmentPolicy::default(),
            )
            .is_ok()
        );
    }
}
