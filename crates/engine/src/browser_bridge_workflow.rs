use std::{sync::Arc, time::Duration};

use asterism_domain::{
    AuthState, BrowserBridgeSessionState, ProviderAccountId, ProviderId, TaskCapability, TaskId,
    UserId,
};
use asterism_provider_api::{
    BrowserBridgeResultDisposition, BrowserBridgeWorkflowPlanArtifact, BrowserBridgeWorkflowResult,
    BrowserBridgeWorkflowResultRequest, BrowserBridgeWorkflowRuntimeState, ProviderContext,
    ProviderError, ProviderRegistry,
};
use asterism_scheduler::{RetryPolicy, RetryPolicyError};
use asterism_secrets::{SecretAccess, SecretStoreError};
use asterism_storage::{
    BrowserBridgeCommandArtifactRepository, BrowserBridgeResultAttemptFinishRequest,
    BrowserBridgeSessionRepository, BrowserBridgeWorkflowCommitOutcome,
    BrowserBridgeWorkflowCommitRequest, PendingBrowserBridgeResult,
    ProviderAccountRuntimeRepository, StorageError, TaskQueryRepository,
};

use crate::BrowserBridgeRuntimeRecoverySnapshot;

/// Core-owned validation of one recovered intermediate or execution-terminal
/// `BrowserBridge` result. This layer performs no persistence.
#[derive(Clone, Debug)]
pub struct BrowserBridgeWorkflowValidationService<Q, A> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
}

impl<Q, A> BrowserBridgeWorkflowValidationService<Q, A> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A) -> Self {
        Self {
            registry,
            tasks,
            accounts,
        }
    }
}

impl<Q, A> BrowserBridgeWorkflowValidationService<Q, A>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
{
    /// Rebinds all durable identities and invokes the Provider only with exact
    /// creation-time settings and encrypted artifacts recovered by Core.
    ///
    /// # Errors
    ///
    /// Rejects incomplete recovery, changed bindings/version/settings,
    /// undeclared result types, artifact drift or Provider validation failure.
    #[allow(
        clippy::too_many_lines,
        reason = "session, Task, account, Provider, settings and every recovered artifact remain explicit in one fail-closed boundary"
    )]
    pub async fn validate(
        &self,
        command: ValidateBrowserBridgeWorkflowCommand,
    ) -> Result<ValidatedBrowserBridgeWorkflow, BrowserBridgeWorkflowValidationError> {
        let ValidateBrowserBridgeWorkflowCommand {
            owner_user_id,
            recovery,
            access,
        } = command;
        if !access.authorizes(owner_user_id)
            || recovery.session.owner_user_id != owner_user_id
            || recovery.session.state != BrowserBridgeSessionState::Claimed
            || recovery.spec.validate().is_err()
            || recovery.session.spec_version != recovery.spec.version
            || recovery.session.spec_digest
                != recovery
                    .spec
                    .digest()
                    .map_err(|_| BrowserBridgeWorkflowValidationError::InvalidRecovery)?
            || recovery.binding.session_id != recovery.session.id
            || !recovery
                .spec
                .allowed_origins
                .contains(&recovery.binding.observed_origin)
        {
            return Err(BrowserBridgeWorkflowValidationError::InvalidRecovery);
        }
        let task = self
            .tasks
            .find_owned_task(owner_user_id, recovery.session.task_id)
            .await?
            .ok_or(BrowserBridgeWorkflowValidationError::TaskNotFound)?;
        if task.provider_account_id != recovery.session.provider_account_id
            || !task.capabilities.contains(&TaskCapability::BrowserBridge)
        {
            return Err(BrowserBridgeWorkflowValidationError::TaskBindingChanged);
        }
        let account = self
            .accounts
            .find_runtime_provider_account(recovery.session.provider_account_id)
            .await?
            .filter(|account| account.owner_id == owner_user_id)
            .ok_or(BrowserBridgeWorkflowValidationError::TaskNotFound)?;
        if account.provider_id != recovery.session.provider_id
            || account.auth_state != AuthState::Authenticated
        {
            return Err(BrowserBridgeWorkflowValidationError::TaskBindingChanged);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            BrowserBridgeWorkflowValidationError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        if entry.metadata.implementation_version != recovery.session.provider_version {
            return Err(BrowserBridgeWorkflowValidationError::ProviderVersionChanged);
        }
        let capability = entry.browser_bridge.as_ref().ok_or_else(|| {
            BrowserBridgeWorkflowValidationError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let recovered = recovery
            .latest
            .ok_or(BrowserBridgeWorkflowValidationError::ResultMissing)?;
        let result = recovered
            .result
            .ok_or(BrowserBridgeWorkflowValidationError::ResultMissing)?;
        if recovered.command.exchange != result.exchange
            || recovered.command.exchange.session_id != recovery.session.id
        {
            return Err(BrowserBridgeWorkflowValidationError::InvalidRecovery);
        }
        let disposition = capability
            .browser_bridge_result_disposition(&result.metadata.result_type)
            .ok_or(BrowserBridgeWorkflowValidationError::UndeclaredResultType)?;
        if !matches!(
            disposition,
            BrowserBridgeResultDisposition::Intermediate
                | BrowserBridgeResultDisposition::ExecutionTerminal
        ) {
            return Err(BrowserBridgeWorkflowValidationError::UndeclaredResultType);
        }
        let mut context = recovered
            .command
            .workflow_context
            .ok_or(BrowserBridgeWorkflowValidationError::WorkflowContextMissing)?;
        context.runtime_settings = entry
            .runtime_settings
            .hydrate_frozen_core_defaults(&context.runtime_settings)
            .map_err(|_| BrowserBridgeWorkflowValidationError::RuntimeSettingsChanged)?;
        let workflow_plan = context
            .workflow_plan
            .map(|plan| {
                let expected = plan.artifact_digest;
                let artifact =
                    BrowserBridgeWorkflowPlanArtifact::try_new(plan.artifact_type, plan.artifact)?;
                if artifact.artifact_digest() != expected {
                    return Err(ProviderError::new(
                        asterism_provider_api::ProviderErrorKind::ProtocolDrift,
                        "BrowserBridge workflow plan digest changed after recovery",
                    ));
                }
                Ok(artifact)
            })
            .transpose()?;
        let runtime_state =
            recovered
                .command
                .runtime_state
                .map(|state| BrowserBridgeWorkflowRuntimeState {
                    metadata: state.metadata,
                    artifact: state.state_artifact,
                });
        let request = BrowserBridgeWorkflowResultRequest {
            remote_task_id: task.remote_id,
            issued_exchange: recovered.command.exchange,
            command_artifact: recovered.command.command_artifact,
            workflow_plan,
            runtime_state,
            result_metadata: result.metadata,
            result_artifact: result.result_artifact,
            runtime_binding: recovery.binding,
        };
        request.validate()?;
        let transition = capability
            .complete_browser_bridge_workflow_result(
                &ProviderContext {
                    provider_id: account.provider_id,
                    account_id: account.id,
                    credential_refs: account.credential_refs,
                    correlation_id: access.correlation_id.clone(),
                },
                &context.runtime_settings,
                request,
            )
            .await?;
        Ok(ValidatedBrowserBridgeWorkflow {
            owner_user_id,
            provider_account_id: recovery.session.provider_account_id,
            task_id: recovery.session.task_id,
            transition,
            access,
        })
    }
}

#[derive(Debug)]
pub struct ValidateBrowserBridgeWorkflowCommand {
    pub owner_user_id: UserId,
    pub recovery: BrowserBridgeRuntimeRecoverySnapshot,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct ValidatedBrowserBridgeWorkflow {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub transition: BrowserBridgeWorkflowResult,
    pub access: SecretAccess,
}

/// Core-owned durable processor for one Provider's intermediate and execution
/// terminal `BrowserBridge` result inboxes.
#[derive(Clone, Debug)]
pub struct BrowserBridgeWorkflowProcessor<S, C, Q, A> {
    provider_id: ProviderId,
    registry: Arc<ProviderRegistry>,
    sessions: S,
    commands: C,
    tasks: Q,
    accounts: A,
    config: BrowserBridgeWorkflowProcessorConfig,
}

impl<S, C, Q, A> BrowserBridgeWorkflowProcessor<S, C, Q, A> {
    /// # Errors
    ///
    /// Rejects an unsafe worker identity, lease bound or retry policy.
    #[allow(
        clippy::too_many_arguments,
        reason = "Core repositories and their fixed Provider scope remain explicit at composition"
    )]
    pub fn new(
        provider_id: ProviderId,
        registry: Arc<ProviderRegistry>,
        sessions: S,
        commands: C,
        tasks: Q,
        accounts: A,
        config: BrowserBridgeWorkflowProcessorConfig,
    ) -> Result<Self, BrowserBridgeWorkflowProcessorError> {
        config.validate()?;
        Ok(Self {
            provider_id,
            registry,
            sessions,
            commands,
            tasks,
            accounts,
            config,
        })
    }
}

impl<S, C, Q, A> BrowserBridgeWorkflowProcessor<S, C, Q, A>
where
    S: BrowserBridgeSessionRepository + Clone,
    C: BrowserBridgeCommandArtifactRepository + Clone,
    Q: TaskQueryRepository + Clone,
    A: ProviderAccountRuntimeRepository + Clone,
{
    /// Processes at most one exact Provider workflow result. Failures become
    /// durable retry/dead-letter state and never replay a browser command.
    ///
    /// # Errors
    ///
    /// Returns an error only when inbox selection or Provider declarations are
    /// invalid before a result attempt begins.
    pub async fn tick(
        &self,
        now: asterism_domain::Timestamp,
    ) -> Result<BrowserBridgeWorkflowTickReport, BrowserBridgeWorkflowProcessorError> {
        let entry = self.registry.get(&self.provider_id).ok_or_else(|| {
            BrowserBridgeWorkflowProcessorError::ProviderNotRegistered(self.provider_id.clone())
        })?;
        let capability = entry.browser_bridge.as_ref().ok_or_else(|| {
            BrowserBridgeWorkflowProcessorError::CapabilityUnavailable(self.provider_id.clone())
        })?;
        let mut result_types = capability
            .browser_bridge_intermediate_result_types()
            .iter()
            .copied()
            .map(|result_type| (result_type, BrowserBridgeResultDisposition::Intermediate))
            .chain(
                capability
                    .browser_bridge_execution_result_types()
                    .iter()
                    .copied()
                    .map(|result_type| {
                        (
                            result_type,
                            BrowserBridgeResultDisposition::ExecutionTerminal,
                        )
                    }),
            )
            .collect::<Vec<_>>();
        result_types.sort_unstable_by_key(|(result_type, _)| *result_type);
        if result_types.is_empty()
            || result_types.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || result_types.iter().any(|(result_type, expected)| {
                capability.browser_bridge_result_disposition(result_type) != Some(*expected)
            })
        {
            return Err(
                BrowserBridgeWorkflowProcessorError::InvalidResultTypeDeclaration(
                    self.provider_id.clone(),
                ),
            );
        }
        let result_type_refs = result_types
            .iter()
            .map(|(result_type, _)| *result_type)
            .collect::<Vec<_>>();
        let lease_expires_at = now
            .checked_add_signed(
                chrono::Duration::from_std(self.config.claim_ttl)
                    .map_err(|_| BrowserBridgeWorkflowProcessorError::InvalidConfig)?,
            )
            .ok_or(BrowserBridgeWorkflowProcessorError::InvalidConfig)?;
        let pending = self
            .sessions
            .claim_pending_browser_bridge_results(
                now,
                &self.provider_id,
                &result_type_refs,
                1,
                &self.config.worker_id,
                lease_expires_at,
            )
            .await?;
        let mut report = BrowserBridgeWorkflowTickReport {
            selected: u32::try_from(pending.len()).unwrap_or(u32::MAX),
            ..BrowserBridgeWorkflowTickReport::default()
        };
        for candidate in pending {
            self.process_candidate(candidate, now, &mut report).await;
        }
        Ok(report)
    }

    async fn process_candidate(
        &self,
        candidate: PendingBrowserBridgeResult,
        now: asterism_domain::Timestamp,
        report: &mut BrowserBridgeWorkflowTickReport,
    ) {
        let access = SecretAccess {
            actor: asterism_secrets::SecretActor::CoreService("browser-bridge-workflow"),
            correlation_id: format!("bridge-workflow:{}", candidate.session_id),
            reason: "recover and commit Provider-validated BrowserBridge workflow result"
                .to_owned(),
        };
        let recovery = crate::BrowserBridgeRuntimeRecoveryService::new(
            self.sessions.clone(),
            self.commands.clone(),
        )
        .recover(crate::BrowserBridgeRuntimeRecoveryRequest {
            owner_user_id: candidate.owner_user_id,
            session_id: candidate.session_id,
            access: access.clone(),
        })
        .await;
        let Ok(recovery) = recovery else {
            self.record_failure(&candidate, now, "recovery", false, report)
                .await;
            return;
        };
        let validated = BrowserBridgeWorkflowValidationService::new(
            self.registry.clone(),
            self.tasks.clone(),
            self.accounts.clone(),
        )
        .validate(ValidateBrowserBridgeWorkflowCommand {
            owner_user_id: candidate.owner_user_id,
            recovery,
            access,
        })
        .await;
        let Ok(validated) = validated else {
            self.record_failure(&candidate, now, "provider_validation", false, report)
                .await;
            return;
        };
        let outcome = self
            .commands
            .commit_browser_bridge_workflow_result(BrowserBridgeWorkflowCommitRequest {
                owner_user_id: validated.owner_user_id,
                provider_account_id: validated.provider_account_id,
                task_id: validated.task_id,
                transition: validated.transition,
                worker_id: &self.config.worker_id,
                committed_at: std::cmp::max(now, chrono::Utc::now()),
                access: &validated.access,
            })
            .await;
        match outcome {
            Ok(BrowserBridgeWorkflowCommitOutcome::IntermediateCommitted { .. }) => {
                report.intermediate_committed += 1;
            }
            Ok(BrowserBridgeWorkflowCommitOutcome::ExecutionTerminalCommitted { .. }) => {
                report.terminal_committed += 1;
            }
            Ok(
                BrowserBridgeWorkflowCommitOutcome::BindingConflict
                | BrowserBridgeWorkflowCommitOutcome::SequenceConflict
                | BrowserBridgeWorkflowCommitOutcome::ClaimConflict,
            ) => {
                report.conflicted += 1;
                self.record_failure(&candidate, now, "commit_conflict", true, report)
                    .await;
            }
            Err(_) => {
                self.record_failure(&candidate, now, "commit_storage", false, report)
                    .await;
            }
        }
    }

    async fn record_failure(
        &self,
        candidate: &PendingBrowserBridgeResult,
        failed_at: asterism_domain::Timestamp,
        error_kind: &'static str,
        force_dead_letter: bool,
        report: &mut BrowserBridgeWorkflowTickReport,
    ) {
        let delay = if force_dead_letter {
            None
        } else {
            let Ok(delay) = self.config.retry_policy.delay_after(candidate.attempt_no) else {
                report.failed += 1;
                return;
            };
            delay
        };
        let retry_at = delay.and_then(|delay| {
            chrono::Duration::from_std(delay)
                .ok()
                .and_then(|delay| failed_at.checked_add_signed(delay))
        });
        if delay.is_some() && retry_at.is_none() {
            report.failed += 1;
            return;
        }
        match self
            .sessions
            .finish_browser_bridge_result_attempt(BrowserBridgeResultAttemptFinishRequest {
                session_id: candidate.session_id,
                sequence: candidate.sequence,
                worker_id: &self.config.worker_id,
                failed_at,
                retry_at,
                error_kind,
            })
            .await
        {
            Ok(true) if retry_at.is_some() => report.retry_scheduled += 1,
            Ok(true) => report.dead_lettered += 1,
            Ok(false) | Err(_) => report.failed += 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeWorkflowProcessorConfig {
    pub worker_id: String,
    pub claim_ttl: Duration,
    pub retry_policy: RetryPolicy,
}

impl BrowserBridgeWorkflowProcessorConfig {
    fn validate(&self) -> Result<(), BrowserBridgeWorkflowProcessorError> {
        if self.worker_id.is_empty()
            || self.worker_id.len() > 128
            || self.worker_id.trim() != self.worker_id
            || self.worker_id.chars().any(char::is_control)
            || self.claim_ttl.is_zero()
            || self.claim_ttl > Duration::from_hours(1)
            || self.retry_policy.max_attempts > 32
        {
            return Err(BrowserBridgeWorkflowProcessorError::InvalidConfig);
        }
        self.retry_policy.validate()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrowserBridgeWorkflowTickReport {
    pub selected: u32,
    pub intermediate_committed: u32,
    pub terminal_committed: u32,
    pub conflicted: u32,
    pub retry_scheduled: u32,
    pub dead_lettered: u32,
    pub failed: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeWorkflowProcessorError {
    #[error("BrowserBridge workflow processor configuration is invalid")]
    InvalidConfig,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no BrowserBridge capability")]
    CapabilityUnavailable(ProviderId),
    #[error("provider `{0}` has an invalid BrowserBridge workflow-result declaration")]
    InvalidResultTypeDeclaration(ProviderId),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    RetryPolicy(#[from] RetryPolicyError),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeWorkflowValidationError {
    #[error("BrowserBridge recovery evidence is invalid or cross-bound")]
    InvalidRecovery,
    #[error("BrowserBridge Task was not found")]
    TaskNotFound,
    #[error("BrowserBridge Task/account binding changed")]
    TaskBindingChanged,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no BrowserBridge capability")]
    CapabilityUnavailable(ProviderId),
    #[error("BrowserBridge Provider implementation version changed")]
    ProviderVersionChanged,
    #[error("BrowserBridge workflow result is missing")]
    ResultMissing,
    #[error("BrowserBridge result type is not a declared workflow disposition")]
    UndeclaredResultType,
    #[error("BrowserBridge workflow recovery context is missing")]
    WorkflowContextMissing,
    #[error("BrowserBridge frozen runtime settings no longer match the Provider schema")]
    RuntimeSettingsChanged,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::atomic::{AtomicU32, Ordering},
    };

    use asterism_domain::{
        AssessmentClass, BrowserBridgeExchange, BrowserBridgeResultArtifactMetadata,
        BrowserBridgeRuntimeBinding, BrowserBridgeSession, BrowserBridgeSessionCreate,
        OrchestrationState, ProviderAccount, RemoteState, SourceType, Task,
    };
    use asterism_provider_api::{
        BrowserBridgeCapability, BrowserBridgeWorkflowNextCommand, BrowserSessionSpec,
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        ProviderRuntimeSettingsSchema, ResolvedProviderRuntimeSettings, VerificationLevel,
    };
    use asterism_secrets::{SecretActor, SecretValue};
    use asterism_storage::{
        ResolvedBrowserBridgeCommand, ResolvedBrowserBridgeResult,
        ResolvedBrowserBridgeWorkflowContext, ResolvedBrowserBridgeWorkflowPlan, TaskPage,
    };
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::BrowserBridgeRecoveredExchange;

    #[test]
    fn processor_configuration_is_bounded() {
        let valid = BrowserBridgeWorkflowProcessorConfig {
            worker_id: "browser-workflow-test".to_owned(),
            claim_ttl: std::time::Duration::from_secs(30),
            retry_policy: RetryPolicy {
                max_attempts: 5,
                initial_delay_seconds: 1,
                multiplier: 2,
                max_delay_seconds: 30,
            },
        };
        assert!(valid.validate().is_ok());
        assert!(
            BrowserBridgeWorkflowProcessorConfig {
                worker_id: " bad-worker".to_owned(),
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            BrowserBridgeWorkflowProcessorConfig {
                retry_policy: RetryPolicy {
                    max_attempts: 33,
                    ..valid.retry_policy
                },
                ..valid
            }
            .validate()
            .is_err()
        );
    }

    #[derive(Debug)]
    struct FakeBrowser {
        metadata: ProviderMetadata,
        calls: AtomicU32,
    }

    impl ProviderIdentity for FakeBrowser {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl BrowserBridgeCapability for FakeBrowser {
        async fn browser_session_spec(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<BrowserSessionSpec> {
            unreachable!("workflow validation does not create another session")
        }

        fn browser_bridge_intermediate_result_types(&self) -> &'static [&'static str] {
            &["provider-alpha.workflow.event"]
        }

        fn browser_bridge_result_disposition(
            &self,
            result_type: &str,
        ) -> Option<BrowserBridgeResultDisposition> {
            (result_type == "provider-alpha.workflow.event")
                .then_some(BrowserBridgeResultDisposition::Intermediate)
        }

        async fn complete_browser_bridge_workflow_result(
            &self,
            _context: &ProviderContext,
            settings: &ResolvedProviderRuntimeSettings,
            request: BrowserBridgeWorkflowResultRequest,
        ) -> ProviderResult<BrowserBridgeWorkflowResult> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            request.validate()?;
            assert_eq!(settings.schema_version, 1);
            let plan = request.workflow_plan.as_ref().expect("plan is recovered");
            assert_eq!(plan.artifact_type(), "provider-alpha.plan.v1");
            let mut completed = request.issued_exchange.clone();
            completed
                .complete(
                    request.result_metadata.result_type.clone(),
                    request.result_metadata.result_digest,
                    request.result_metadata.received_at,
                )
                .unwrap();
            let next_command = SecretValue::new(b"next-command".to_vec());
            let next = BrowserBridgeExchange::issue(
                completed.session_id,
                completed.sequence + 1,
                completed.command_type.clone(),
                Sha256::digest(next_command.expose_secret()).into(),
                request.result_metadata.received_at,
            )
            .unwrap();
            BrowserBridgeWorkflowResult::try_intermediate(
                completed,
                BrowserBridgeWorkflowNextCommand {
                    exchange: next,
                    command_artifact: next_command,
                    runtime_state: None,
                },
                &request.issued_exchange,
                &request.result_metadata,
            )
        }
    }

    #[derive(Clone, Debug)]
    struct FakeTasks {
        owner: UserId,
        task: Task,
    }

    #[async_trait]
    impl TaskQueryRepository for FakeTasks {
        async fn list_owned_tasks(
            &self,
            owner_id: UserId,
            _provider_account_id: Option<ProviderAccountId>,
            _limit: u32,
            _offset: u64,
        ) -> Result<TaskPage, StorageError> {
            let items = (owner_id == self.owner)
                .then(|| self.task.clone())
                .into_iter()
                .collect::<Vec<_>>();
            Ok(TaskPage {
                total: items.len() as u64,
                items,
            })
        }

        async fn find_owned_task(
            &self,
            owner_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<Task>, StorageError> {
            Ok((owner_id == self.owner && task_id == self.task.id).then(|| self.task.clone()))
        }
    }

    #[derive(Clone, Debug)]
    struct FakeAccounts(ProviderAccount);

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FakeAccounts {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((account_id == self.0.id).then(|| self.0.clone()))
        }
    }

    #[tokio::test]
    async fn exact_recovered_context_reaches_provider_and_missing_context_does_not() {
        let (service, command, provider) = fixture();
        let validated = service.validate(command).await.unwrap();
        assert!(matches!(
            validated.transition,
            BrowserBridgeWorkflowResult::Intermediate { .. }
        ));
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);

        let (service, mut command, provider) = fixture();
        command
            .recovery
            .latest
            .as_mut()
            .unwrap()
            .command
            .workflow_context = None;
        assert!(matches!(
            service.validate(command).await,
            Err(BrowserBridgeWorkflowValidationError::WorkflowContextMissing)
        ));
        assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps every session, Task, account, result and encrypted context binding visible"
    )]
    fn fixture() -> (
        BrowserBridgeWorkflowValidationService<FakeTasks, FakeAccounts>,
        ValidateBrowserBridgeWorkflowCommand,
        Arc<FakeBrowser>,
    ) {
        let now = Utc::now();
        let owner = UserId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account_id = ProviderAccountId::new();
        let task = Task {
            id: TaskId::new(),
            provider_account_id: account_id,
            course_id: None,
            remote_id: "class-task:workflow-1".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Unknown,
            title: "Workflow".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: vec![TaskCapability::BrowserBridge],
        };
        let account = ProviderAccount {
            id: account_id,
            owner_id: owner,
            provider_id: provider_id.clone(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: vec![asterism_domain::SecretId::new()],
            created_at: now,
            updated_at: now,
        };
        let metadata = ProviderMetadata {
            id: provider_id.clone(),
            display_name: "Provider Alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::BrowserBridge]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let provider = Arc::new(FakeBrowser {
            metadata: metadata.clone(),
            calls: AtomicU32::new(0),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.runtime_settings = ProviderRuntimeSettingsSchema::default();
        entry.browser_bridge = Some(provider.clone());
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();

        let spec = BrowserSessionSpec {
            version: 1,
            start_url: "https://provider.example/workflow".to_owned(),
            isolation_key: "provider-workflow".to_owned(),
            allowed_origins: vec!["https://provider.example".to_owned()],
            read_sources: Vec::new(),
            headless: false,
        };
        let mut session = BrowserBridgeSession::awaiting_claim(BrowserBridgeSessionCreate {
            owner_user_id: owner,
            provider_account_id: account_id,
            task_id: task.id,
            provider_id,
            provider_version: "0.1.0".to_owned(),
            spec_version: spec.version,
            spec_digest: spec.digest().unwrap(),
            created_at: now,
            expires_at: now + Duration::hours(1),
        })
        .unwrap();
        session.claim(now + Duration::seconds(1)).unwrap();
        let binding = BrowserBridgeRuntimeBinding {
            session_id: session.id,
            observed_origin: "https://provider.example".to_owned(),
            frame_id: "top-frame:1".to_owned(),
            bound_at: now + Duration::seconds(1),
        };
        let command_artifact = SecretValue::new(b"current-command".to_vec());
        let exchange = BrowserBridgeExchange::issue(
            session.id,
            1,
            "provider-alpha.workflow.command".to_owned(),
            Sha256::digest(command_artifact.expose_secret()).into(),
            now + Duration::seconds(2),
        )
        .unwrap();
        let result_artifact = SecretValue::new(b"workflow-result".to_vec());
        let result_metadata = BrowserBridgeResultArtifactMetadata {
            session_id: session.id,
            sequence: 1,
            result_type: "provider-alpha.workflow.event".to_owned(),
            result_digest: Sha256::digest(result_artifact.expose_secret()).into(),
            received_at: now + Duration::seconds(3),
        };
        let plan = b"provider-plan";
        let recovery = BrowserBridgeRuntimeRecoverySnapshot {
            session,
            spec,
            binding,
            latest: Some(BrowserBridgeRecoveredExchange {
                command: ResolvedBrowserBridgeCommand {
                    exchange: exchange.clone(),
                    command_artifact,
                    runtime_state: None,
                    workflow_context: Some(ResolvedBrowserBridgeWorkflowContext {
                        runtime_settings: ResolvedProviderRuntimeSettings {
                            schema_version: 1,
                            values: BTreeMap::default(),
                        },
                        workflow_plan: Some(ResolvedBrowserBridgeWorkflowPlan {
                            artifact_type: "provider-alpha.plan.v1".to_owned(),
                            artifact_digest: Sha256::digest(plan).into(),
                            artifact: SecretValue::new(plan.to_vec()),
                        }),
                    }),
                },
                result: Some(ResolvedBrowserBridgeResult {
                    exchange,
                    metadata: result_metadata,
                    result_artifact,
                }),
            }),
        };
        let service = BrowserBridgeWorkflowValidationService::new(
            Arc::new(registry),
            FakeTasks {
                owner,
                task: task.clone(),
            },
            FakeAccounts(account),
        );
        let command = ValidateBrowserBridgeWorkflowCommand {
            owner_user_id: owner,
            recovery,
            access: SecretAccess {
                actor: SecretActor::CoreService("browser-bridge-workflow-test"),
                correlation_id: "workflow-validation".to_owned(),
                reason: "validate recovered workflow result".to_owned(),
            },
        };
        (service, command, provider)
    }
}
