use std::{sync::Arc, time::Duration};

use asterism_domain::{
    AuthMethod, BrowserBridgeExchange, BrowserBridgeExchangeState, BrowserBridgeSessionState,
    ProviderAccountId, ProviderId, TaskCapability, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{
    BrowserBridgeCredentialResultRequest, BrowserBridgeResultDisposition, ProviderContext,
    ProviderError, ProviderRegistry, SessionStatus,
};
use asterism_scheduler::{RetryPolicy, RetryPolicyError};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, SecretAccess, SecretActor, SecretStoreError,
};
use asterism_storage::{
    BrowserBridgeCommandArtifactRepository, BrowserBridgeCredentialCommitOutcome,
    BrowserBridgeCredentialRepository, BrowserBridgeResultAttemptFinishRequest,
    BrowserBridgeSessionRepository, PendingBrowserBridgeResult, ProtocolObservationRepository,
    ProviderAccountRepository, ProviderAccountRuntimeRepository, StorageError, TaskQueryRepository,
};

use crate::{
    BrowserBridgeCredentialCommitRequest, BrowserBridgeCredentialCommitService,
    BrowserBridgeRuntimeRecoveryRequest, BrowserBridgeRuntimeRecoveryService,
    BrowserBridgeRuntimeRecoverySnapshot, CredentialProvisionError,
    credential::validate_candidate,
    protocol_observation::{
        ProviderProtocolObservationRecordError, record_provider_protocol_observation,
    },
};

/// Core-owned validation of one recovered terminal `BrowserBridge` credential
/// result before the existing atomic commit boundary.
#[derive(Clone)]
pub struct BrowserBridgeCredentialValidationService<Q, A> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
    protocol_observations: Option<Arc<dyn ProtocolObservationRepository>>,
}

impl<Q, A> BrowserBridgeCredentialValidationService<Q, A> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A) -> Self {
        Self {
            registry,
            tasks,
            accounts,
            protocol_observations: None,
        }
    }

    #[must_use]
    pub fn with_protocol_observations(
        mut self,
        observations: Arc<dyn ProtocolObservationRepository>,
    ) -> Self {
        self.protocol_observations = Some(observations);
        self
    }
}

impl<Q, A> std::fmt::Debug for BrowserBridgeCredentialValidationService<Q, A> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserBridgeCredentialValidationService")
            .field("registry", &self.registry)
            .field("tasks", &"configured")
            .field("accounts", &"configured")
            .field(
                "protocol_observations",
                &self.protocol_observations.is_some(),
            )
            .finish()
    }
}

impl<Q, A> BrowserBridgeCredentialValidationService<Q, A>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository + ProviderAccountRepository,
{
    /// Rebinds all owner/account/Task/Provider evidence and validates one
    /// recovered terminal result without persisting it.
    ///
    /// # Errors
    ///
    /// Rejects incomplete recovery, cross-owner state, stale task/account
    /// bindings, unsupported Provider results or invalid credentials.
    #[allow(
        clippy::too_many_lines,
        reason = "owner, recovery, Task, account, Provider, artifact and credential bindings remain explicit in one fail-closed validation flow"
    )]
    pub async fn validate(
        &self,
        command: ValidateBrowserBridgeCredentialCommand,
    ) -> Result<ValidatedBrowserBridgeCredential, BrowserBridgeCredentialValidationError> {
        let ValidateBrowserBridgeCredentialCommand {
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
                    .map_err(|_| BrowserBridgeCredentialValidationError::InvalidRecovery)?
            || recovery.binding.session_id != recovery.session.id
            || !recovery
                .spec
                .allowed_origins
                .contains(&recovery.binding.observed_origin)
        {
            return Err(BrowserBridgeCredentialValidationError::InvalidRecovery);
        }
        let task = self
            .tasks
            .find_owned_task(owner_user_id, recovery.session.task_id)
            .await?
            .ok_or(BrowserBridgeCredentialValidationError::TaskNotFound)?;
        if task.provider_account_id != recovery.session.provider_account_id
            || !task.capabilities.contains(&TaskCapability::BrowserBridge)
        {
            return Err(BrowserBridgeCredentialValidationError::TaskBindingChanged);
        }
        let account = self
            .accounts
            .find_runtime_provider_account(recovery.session.provider_account_id)
            .await?
            .filter(|account| account.owner_id == owner_user_id)
            .ok_or(BrowserBridgeCredentialValidationError::TaskNotFound)?;
        if account.provider_id != recovery.session.provider_id {
            return Err(BrowserBridgeCredentialValidationError::TaskBindingChanged);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            BrowserBridgeCredentialValidationError::ProviderNotRegistered(
                account.provider_id.clone(),
            )
        })?;
        if entry.metadata.implementation_version != recovery.session.provider_version {
            return Err(BrowserBridgeCredentialValidationError::ProviderVersionChanged);
        }
        let capability = entry.browser_bridge.as_ref().ok_or_else(|| {
            BrowserBridgeCredentialValidationError::CapabilityUnavailable(
                account.provider_id.clone(),
            )
        })?;
        let recovered = recovery
            .latest
            .ok_or(BrowserBridgeCredentialValidationError::ResultMissing)?;
        let session_id = recovery.session.id;
        let sequence = recovered.command.exchange.sequence;
        if recovered.command.runtime_state.is_some() {
            return Err(BrowserBridgeCredentialValidationError::UnexpectedRuntimeState);
        }
        let result = recovered
            .result
            .ok_or(BrowserBridgeCredentialValidationError::ResultMissing)?;
        if recovered.command.exchange != result.exchange
            || recovered.command.exchange.session_id != recovery.session.id
        {
            return Err(BrowserBridgeCredentialValidationError::InvalidRecovery);
        }
        let request = BrowserBridgeCredentialResultRequest {
            remote_task_id: &task.remote_id,
            issued_exchange: &recovered.command.exchange,
            command_artifact: &recovered.command.command_artifact,
            result_metadata: &result.metadata,
            result_artifact: &result.result_artifact,
            runtime_binding: &recovery.binding,
        };
        request.validate()?;
        let accepted = capability
            .complete_browser_bridge_credential_result(
                &ProviderContext {
                    provider_id: account.provider_id.clone(),
                    account_id: account.id,
                    credential_refs: account.credential_refs,
                    correlation_id: access.correlation_id.clone(),
                },
                request,
            )
            .await;
        let accepted = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                self.record_protocol_observation(
                    &account.provider_id,
                    session_id,
                    sequence,
                    "complete-result",
                    &access.correlation_id,
                    &error,
                )
                .await?;
                return Err(BrowserBridgeCredentialValidationError::Provider(error));
            }
        };
        let (replacement, completed_exchange) = accepted.into_parts();
        require_exact_completion(
            &recovered.command.exchange,
            &completed_exchange,
            &result.metadata,
        )?;
        let provider_id = account.provider_id;
        let bundle = CredentialBundle {
            provider_id: provider_id.clone(),
            tenant: account.tenant,
            auth_method: AuthMethod::AssistedSession,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at: result.metadata.received_at,
            expires_at: None,
            session_kind: replacement.session_kind,
            fields: replacement.fields,
            user_id_hint: None,
        };
        let validation = validate_candidate(
            self.registry.as_ref(),
            &self.accounts,
            owner_user_id,
            recovery.session.provider_account_id,
            bundle,
            None,
            &access,
        )
        .await;
        let (bundle, status) = match validation {
            Ok(validated) => validated,
            Err(CredentialProvisionError::Provider(error)) => {
                self.record_protocol_observation(
                    &provider_id,
                    session_id,
                    sequence,
                    "validate-credential",
                    &access.correlation_id,
                    &error,
                )
                .await?;
                return Err(BrowserBridgeCredentialValidationError::Credential(
                    CredentialProvisionError::Provider(error),
                ));
            }
            Err(error) => return Err(BrowserBridgeCredentialValidationError::Credential(error)),
        };
        Ok(ValidatedBrowserBridgeCredential {
            owner_user_id,
            provider_account_id: recovery.session.provider_account_id,
            task_id: recovery.session.task_id,
            completed_exchange,
            bundle,
            status,
        })
    }

    async fn record_protocol_observation(
        &self,
        provider_id: &ProviderId,
        session_id: asterism_domain::BrowserBridgeSessionId,
        sequence: u64,
        stage: &str,
        correlation_id: &str,
        error: &ProviderError,
    ) -> Result<(), BrowserBridgeCredentialValidationError> {
        let occurrence_scope =
            format!("browser-bridge-credential:{session_id}:{sequence}:{stage}:{correlation_id}");
        record_provider_protocol_observation(
            self.protocol_observations.as_deref(),
            provider_id,
            None,
            &occurrence_scope,
            error,
            chrono::Utc::now(),
        )
        .await
        .map_err(|error| match error {
            ProviderProtocolObservationRecordError::Invalid => {
                BrowserBridgeCredentialValidationError::InvalidProtocolObservation
            }
            ProviderProtocolObservationRecordError::Storage(error) => {
                BrowserBridgeCredentialValidationError::Storage(error)
            }
        })
    }
}

fn require_exact_completion(
    issued: &BrowserBridgeExchange,
    completed: &BrowserBridgeExchange,
    result: &asterism_domain::BrowserBridgeResultArtifactMetadata,
) -> Result<(), BrowserBridgeCredentialValidationError> {
    if completed.validate().is_err()
        || completed.state != BrowserBridgeExchangeState::Completed
        || completed.session_id != issued.session_id
        || completed.sequence != issued.sequence
        || completed.command_type != issued.command_type
        || completed.command_digest != issued.command_digest
        || completed.issued_at != issued.issued_at
        || completed.result_type.as_deref() != Some(result.result_type.as_str())
        || completed.result_digest != Some(result.result_digest)
        || completed.completed_at != Some(result.received_at)
    {
        Err(BrowserBridgeCredentialValidationError::ProviderCompletionMismatch)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub struct ValidateBrowserBridgeCredentialCommand {
    pub owner_user_id: UserId,
    pub recovery: BrowserBridgeRuntimeRecoverySnapshot,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct ValidatedBrowserBridgeCredential {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub completed_exchange: BrowserBridgeExchange,
    pub bundle: CredentialBundle,
    pub status: SessionStatus,
}

/// Core-owned durable processor for one Provider's credential-terminal
/// `BrowserBridge` result inbox.
#[derive(Clone)]
pub struct BrowserBridgeCredentialProcessor<S, C, Q, A, R> {
    provider_id: ProviderId,
    registry: Arc<ProviderRegistry>,
    sessions: S,
    commands: C,
    tasks: Q,
    accounts: A,
    credentials: R,
    config: BrowserBridgeCredentialProcessorConfig,
    protocol_observations: Option<Arc<dyn ProtocolObservationRepository>>,
}

impl<S, C, Q, A, R> BrowserBridgeCredentialProcessor<S, C, Q, A, R> {
    /// Builds one Provider-scoped processor after validating its bounded claim
    /// and retry policy.
    ///
    /// # Errors
    ///
    /// Rejects invalid worker identity, claim bounds, lease or retry policy.
    #[allow(
        clippy::too_many_arguments,
        reason = "Core-owned repositories and their fixed Provider scope remain explicit at composition"
    )]
    pub fn new(
        provider_id: ProviderId,
        registry: Arc<ProviderRegistry>,
        sessions: S,
        commands: C,
        tasks: Q,
        accounts: A,
        credentials: R,
        config: BrowserBridgeCredentialProcessorConfig,
    ) -> Result<Self, BrowserBridgeCredentialProcessorError> {
        config.validate()?;
        Ok(Self {
            provider_id,
            registry,
            sessions,
            commands,
            tasks,
            accounts,
            credentials,
            config,
            protocol_observations: None,
        })
    }

    #[must_use]
    pub fn with_protocol_observations(
        mut self,
        observations: Arc<dyn ProtocolObservationRepository>,
    ) -> Self {
        self.protocol_observations = Some(observations);
        self
    }
}

impl<S, C, Q, A, R> std::fmt::Debug for BrowserBridgeCredentialProcessor<S, C, Q, A, R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserBridgeCredentialProcessor")
            .field("provider_id", &self.provider_id)
            .field("config", &self.config)
            .field(
                "protocol_observations",
                &self.protocol_observations.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl<S, C, Q, A, R> BrowserBridgeCredentialProcessor<S, C, Q, A, R>
where
    S: BrowserBridgeSessionRepository + Clone,
    C: BrowserBridgeCommandArtifactRepository + Clone,
    Q: TaskQueryRepository + Clone,
    A: ProviderAccountRuntimeRepository + ProviderAccountRepository + Clone,
    R: BrowserBridgeCredentialRepository + Clone,
{
    /// Processes one bounded Provider-scoped batch. Individual failures remain
    /// durable and are counted rather than aborting unrelated results.
    ///
    /// # Errors
    ///
    /// Returns an error only when the bounded inbox cannot be selected or the
    /// Provider's terminal-result declaration is internally inconsistent.
    #[allow(
        clippy::too_many_lines,
        reason = "claim, recovery, validation, commit and durable failure classification remain explicit in one result-attempt flow"
    )]
    pub async fn tick(
        &self,
        now: Timestamp,
    ) -> Result<BrowserBridgeCredentialTickReport, BrowserBridgeCredentialProcessorError> {
        let entry = self.registry.get(&self.provider_id).ok_or_else(|| {
            BrowserBridgeCredentialProcessorError::ProviderNotRegistered(self.provider_id.clone())
        })?;
        let capability = entry.browser_bridge.as_ref().ok_or_else(|| {
            BrowserBridgeCredentialProcessorError::CapabilityUnavailable(self.provider_id.clone())
        })?;
        let result_types = capability.browser_bridge_credential_result_types();
        if result_types.is_empty()
            || result_types.iter().enumerate().any(|(index, result_type)| {
                result_types[..index].contains(result_type)
                    || capability.browser_bridge_result_disposition(result_type)
                        != Some(BrowserBridgeResultDisposition::CredentialTerminal)
            })
        {
            return Err(
                BrowserBridgeCredentialProcessorError::InvalidResultTypeDeclaration(
                    self.provider_id.clone(),
                ),
            );
        }
        let lease_expires_at = now
            .checked_add_signed(
                chrono::Duration::from_std(self.config.claim_ttl)
                    .map_err(|_| BrowserBridgeCredentialProcessorError::InvalidConfig)?,
            )
            .ok_or(BrowserBridgeCredentialProcessorError::InvalidConfig)?;
        let pending = self
            .sessions
            .claim_pending_browser_bridge_results(
                now,
                &self.provider_id,
                result_types,
                1,
                &self.config.worker_id,
                lease_expires_at,
            )
            .await?;
        let mut report = BrowserBridgeCredentialTickReport {
            selected: u32::try_from(pending.len()).unwrap_or(u32::MAX),
            ..BrowserBridgeCredentialTickReport::default()
        };
        for candidate in pending {
            let access = SecretAccess {
                actor: SecretActor::CoreService("browser-bridge-credential"),
                correlation_id: format!("bridge-credential:{}", candidate.session_id),
                reason: "recover and commit Provider-validated BrowserBridge credentials"
                    .to_owned(),
            };
            let recovery = BrowserBridgeRuntimeRecoveryService::new(
                self.sessions.clone(),
                self.commands.clone(),
            )
            .recover(BrowserBridgeRuntimeRecoveryRequest {
                owner_user_id: candidate.owner_user_id,
                session_id: candidate.session_id,
                access: access.clone(),
            })
            .await;
            let Ok(recovery) = recovery else {
                self.record_failure(&candidate, now, "recovery", false, &mut report)
                    .await;
                continue;
            };
            let mut validation = BrowserBridgeCredentialValidationService::new(
                self.registry.clone(),
                self.tasks.clone(),
                self.accounts.clone(),
            );
            if let Some(observations) = &self.protocol_observations {
                validation = validation.with_protocol_observations(observations.clone());
            }
            let validated = validation
                .validate(ValidateBrowserBridgeCredentialCommand {
                    owner_user_id: candidate.owner_user_id,
                    recovery,
                    access: access.clone(),
                })
                .await;
            let Ok(validated) = validated else {
                self.record_failure(&candidate, now, "provider_validation", false, &mut report)
                    .await;
                continue;
            };
            let outcome = BrowserBridgeCredentialCommitService::new(self.credentials.clone())
                .commit(BrowserBridgeCredentialCommitRequest {
                    owner_user_id: validated.owner_user_id,
                    provider_account_id: validated.provider_account_id,
                    task_id: validated.task_id,
                    exchange: validated.completed_exchange,
                    validated_bundle: validated.bundle,
                    access,
                })
                .await;
            match outcome {
                Ok(BrowserBridgeCredentialCommitOutcome::Committed(_)) => report.committed += 1,
                Ok(
                    BrowserBridgeCredentialCommitOutcome::AccessRejected
                    | BrowserBridgeCredentialCommitOutcome::BindingConflict
                    | BrowserBridgeCredentialCommitOutcome::SequenceConflict,
                ) => {
                    report.conflicted += 1;
                    self.record_failure(&candidate, now, "commit_conflict", true, &mut report)
                        .await;
                }
                Err(_) => {
                    self.record_failure(&candidate, now, "commit_storage", false, &mut report)
                        .await;
                }
            }
        }
        Ok(report)
    }

    async fn record_failure(
        &self,
        candidate: &PendingBrowserBridgeResult,
        failed_at: Timestamp,
        error_kind: &'static str,
        force_dead_letter: bool,
        report: &mut BrowserBridgeCredentialTickReport,
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
pub struct BrowserBridgeCredentialProcessorConfig {
    pub worker_id: String,
    pub claim_ttl: Duration,
    pub retry_policy: RetryPolicy,
}

impl BrowserBridgeCredentialProcessorConfig {
    fn validate(&self) -> Result<(), BrowserBridgeCredentialProcessorError> {
        if self.worker_id.is_empty()
            || self.worker_id.len() > 128
            || self.worker_id.chars().any(char::is_control)
            || self.claim_ttl.is_zero()
            || self.claim_ttl > Duration::from_hours(1)
            || self.retry_policy.max_attempts > 32
        {
            return Err(BrowserBridgeCredentialProcessorError::InvalidConfig);
        }
        self.retry_policy.validate()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrowserBridgeCredentialTickReport {
    pub selected: u32,
    pub committed: u32,
    pub conflicted: u32,
    pub retry_scheduled: u32,
    pub dead_lettered: u32,
    pub failed: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeCredentialProcessorError {
    #[error("BrowserBridge credential processor configuration is invalid")]
    InvalidConfig,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no BrowserBridge capability")]
    CapabilityUnavailable(ProviderId),
    #[error("provider `{0}` has an invalid BrowserBridge credential-result declaration")]
    InvalidResultTypeDeclaration(ProviderId),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    RetryPolicy(#[from] RetryPolicyError),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeCredentialValidationError {
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
    #[error("BrowserBridge terminal result is missing")]
    ResultMissing,
    #[error("BrowserBridge credential result has an unexpected runtime sidecar")]
    UnexpectedRuntimeState,
    #[error("Provider completion does not match the recovered result")]
    ProviderCompletionMismatch,
    #[error("Provider supplied an invalid protocol observation")]
    InvalidProtocolObservation,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Credential(#[from] CredentialProvisionError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use asterism_domain::{
        AssessmentClass, AuditActor, AuthState, BrowserBridgeResultArtifactMetadata,
        BrowserBridgeRuntimeBinding, BrowserBridgeSession, BrowserBridgeSessionCreate,
        OrchestrationState, ProtocolObservationKind, ProtocolSurface, ProviderAccount,
        ProviderAccountId, RemoteState, SourceType, Task, TaskId, Timestamp,
    };
    use asterism_provider_api::{
        AuthChallenge, AuthenticationCapability, BrowserBridgeCapability,
        BrowserBridgeCredentialResult, CredentialReplacement, CredentialValidation,
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        ProviderRuntimeSettingsSchema, VerificationLevel,
    };
    use asterism_secrets::{CredentialField, SecretActor, SecretPurpose, SecretValue};
    use asterism_storage::{
        Database, ResolvedBrowserBridgeCommand, ResolvedBrowserBridgeResult,
        SqliteProtocolObservationRepository, TaskPage,
    };
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use sha2::Digest;

    use super::*;

    #[test]
    fn processor_configuration_bounds_claims_and_attempts() {
        let valid = BrowserBridgeCredentialProcessorConfig {
            worker_id: "browser-credential-test".to_owned(),
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
            BrowserBridgeCredentialProcessorConfig {
                worker_id: String::new(),
                ..valid.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            BrowserBridgeCredentialProcessorConfig {
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
    struct FakeProvider {
        metadata: ProviderMetadata,
        credential_result_drift: bool,
        credential_validation_drift: bool,
    }

    impl ProviderIdentity for FakeProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl BrowserBridgeCapability for FakeProvider {
        async fn browser_session_spec(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<asterism_provider_api::BrowserSessionSpec> {
            unreachable!("terminal validation does not create another session")
        }

        async fn complete_browser_bridge_credential_result(
            &self,
            _context: &ProviderContext,
            request: BrowserBridgeCredentialResultRequest<'_>,
        ) -> ProviderResult<BrowserBridgeCredentialResult> {
            request.validate()?;
            if self.credential_result_drift {
                return Err(ProviderError::new(
                    asterism_provider_api::ProviderErrorKind::ProtocolDrift,
                    "BrowserBridge credential result shape changed",
                )
                .try_with_protocol_observation(
                    ProtocolSurface::BrowserBridge,
                    ProtocolObservationKind::UnknownResultShape,
                    serde_json::json!({
                        "document": "browser_bridge_credential_result",
                        "result": "object"
                    }),
                )
                .unwrap());
            }
            let mut exchange = request.issued_exchange.clone();
            exchange
                .complete(
                    request.result_metadata.result_type.clone(),
                    request.result_metadata.result_digest,
                    request.result_metadata.received_at,
                )
                .unwrap();
            BrowserBridgeCredentialResult::try_new(
                CredentialReplacement {
                    session_kind: asterism_domain::SessionKind::Composite,
                    fields: vec![CredentialField {
                        purpose: SecretPurpose::ProviderAccessToken,
                        value: SecretValue::new(b"validated-token".to_vec()),
                    }],
                },
                exchange,
            )
        }
    }

    #[async_trait]
    impl AuthenticationCapability for FakeProvider {
        async fn begin_authentication(
            &self,
            _context: &asterism_provider_api::ProviderAuthContext,
            _method: AuthMethod,
        ) -> ProviderResult<AuthChallenge> {
            unreachable!("terminal validation does not start authentication")
        }

        async fn validate_credential(
            &self,
            _context: &asterism_provider_api::ProviderAuthContext,
            credential: &CredentialBundle,
        ) -> ProviderResult<asterism_provider_api::CredentialValidation> {
            if self.credential_validation_drift {
                return Err(ProviderError::new(
                    asterism_provider_api::ProviderErrorKind::ProtocolDrift,
                    "BrowserBridge derived credential status changed",
                )
                .try_with_protocol_observation(
                    ProtocolSurface::Authentication,
                    ProtocolObservationKind::FieldDrift,
                    serde_json::json!({
                        "document": "session_status",
                        "missing": "valid"
                    }),
                )
                .unwrap());
            }
            Ok(CredentialValidation::accepted(SessionStatus {
                valid: true,
                kind: credential.session_kind,
                expires_at: None,
                account_hint: Some("bound-account".to_owned()),
            }))
        }

        async fn validate_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<SessionStatus> {
            unreachable!("terminal validation validates the candidate directly")
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
            let items = if owner_id == self.owner {
                vec![self.task.clone()]
            } else {
                Vec::new()
            };
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

    #[async_trait]
    impl ProviderAccountRepository for FakeAccounts {
        async fn list_provider_accounts(
            &self,
            owner_id: UserId,
        ) -> Result<Vec<ProviderAccount>, StorageError> {
            Ok(if owner_id == self.0.owner_id {
                vec![self.0.clone()]
            } else {
                Vec::new()
            })
        }

        async fn find_provider_account(
            &self,
            owner_id: UserId,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((owner_id == self.0.owner_id && account_id == self.0.id).then(|| self.0.clone()))
        }

        async fn create_provider_account(
            &self,
            _account: &ProviderAccount,
            _actor: AuditActor,
        ) -> Result<(), StorageError> {
            unreachable!("validation is read-only")
        }

        async fn update_provider_account(
            &self,
            _account: &ProviderAccount,
            _actor: AuditActor,
        ) -> Result<bool, StorageError> {
            unreachable!("validation is read-only")
        }

        async fn delete_provider_account(
            &self,
            _owner_id: UserId,
            _account_id: ProviderAccountId,
            _at: Timestamp,
            _actor: AuditActor,
        ) -> Result<bool, StorageError> {
            unreachable!("validation is read-only")
        }
    }

    #[tokio::test]
    async fn recovered_result_is_freshly_validated_without_persistence() {
        let (_, service, command) = fixture("https://provider.example", false, false).await;
        let validated = service.validate(command).await.unwrap();
        assert_eq!(
            validated.completed_exchange.state,
            BrowserBridgeExchangeState::Completed
        );
        assert_eq!(validated.bundle.fields.len(), 1);
        assert_eq!(
            validated.status.account_hint.as_deref(),
            Some("bound-account")
        );
    }

    #[tokio::test]
    async fn foreign_runtime_origin_fails_before_provider_validation() {
        let (_, service, command) = fixture("https://foreign.example", false, false).await;
        assert!(matches!(
            service.validate(command).await,
            Err(BrowserBridgeCredentialValidationError::InvalidRecovery)
        ));
    }

    #[tokio::test]
    async fn credential_result_drift_is_observed_without_validated_output() {
        let (database, service, command) = fixture("https://provider.example", true, false).await;
        let error = service.validate(command).await.unwrap_err();
        assert!(matches!(
            error,
            BrowserBridgeCredentialValidationError::Provider(error)
                if error.kind == asterism_provider_api::ProviderErrorKind::ProtocolDrift
        ));
        let observation: (String, String, Option<String>) =
            sqlx::query_as("SELECT surface, kind, last_execution_id FROM protocol_observations")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(
            observation,
            (
                "browser_bridge".to_owned(),
                "unknown_result_shape".to_owned(),
                None,
            )
        );
    }

    #[tokio::test]
    async fn derived_credential_drift_is_observed_without_validated_output() {
        let (database, service, command) = fixture("https://provider.example", false, true).await;
        let error = service.validate(command).await.unwrap_err();
        assert!(matches!(
            error,
            BrowserBridgeCredentialValidationError::Credential(
                CredentialProvisionError::Provider(error)
            ) if error.kind == asterism_provider_api::ProviderErrorKind::ProtocolDrift
        ));
        let observation: (String, String, Option<String>) =
            sqlx::query_as("SELECT surface, kind, last_execution_id FROM protocol_observations")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(
            observation,
            ("authentication".to_owned(), "field_drift".to_owned(), None,)
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the integration fixture constructs every durable BrowserBridge, Task, account, artifact and Provider binding explicitly"
    )]
    async fn fixture(
        observed_origin: &str,
        credential_result_drift: bool,
        credential_validation_drift: bool,
    ) -> (
        Database,
        BrowserBridgeCredentialValidationService<FakeTasks, FakeAccounts>,
        ValidateBrowserBridgeCredentialCommand,
    ) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner = UserId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account_id = ProviderAccountId::new();
        let task = Task {
            id: TaskId::new(),
            provider_account_id: account_id,
            course_id: None,
            remote_id: "class-task:1".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Unknown,
            title: "Browser capture".to_owned(),
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
            capabilities: BTreeSet::from([
                ProviderCapability::Authentication,
                ProviderCapability::BrowserBridge,
            ]),
            auth_methods: BTreeSet::from([AuthMethod::AssistedSession]),
            session_kinds: BTreeSet::from([asterism_domain::SessionKind::Composite]),
        };
        let provider = Arc::new(FakeProvider {
            metadata: metadata.clone(),
            credential_result_drift,
            credential_validation_drift,
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.runtime_settings = ProviderRuntimeSettingsSchema::default();
        entry.authentication = Some(provider.clone());
        entry.browser_bridge = Some(provider);
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();

        let spec = asterism_provider_api::BrowserSessionSpec {
            version: 1,
            start_url: "https://provider.example/task".to_owned(),
            isolation_key: "provider-task".to_owned(),
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
            observed_origin: observed_origin.to_owned(),
            frame_id: "top-frame:1".to_owned(),
            bound_at: now + Duration::seconds(1),
        };
        let command_artifact = SecretValue::new(b"bounded-command".to_vec());
        let command_digest = sha2::Sha256::digest(command_artifact.expose_secret()).into();
        let exchange = BrowserBridgeExchange::issue(
            session.id,
            1,
            "provider-alpha.capture".to_owned(),
            command_digest,
            now + Duration::seconds(2),
        )
        .unwrap();
        let result_artifact = SecretValue::new(b"bounded-result".to_vec());
        let result_metadata = BrowserBridgeResultArtifactMetadata {
            session_id: session.id,
            sequence: 1,
            result_type: "provider-alpha.capture.result".to_owned(),
            result_digest: sha2::Sha256::digest(result_artifact.expose_secret()).into(),
            received_at: now + Duration::seconds(3),
        };
        let recovery = BrowserBridgeRuntimeRecoverySnapshot {
            session,
            spec,
            binding,
            latest: Some(crate::BrowserBridgeRecoveredExchange {
                command: ResolvedBrowserBridgeCommand {
                    exchange: exchange.clone(),
                    command_artifact,
                    runtime_state: None,
                    workflow_context: None,
                },
                result: Some(ResolvedBrowserBridgeResult {
                    exchange,
                    metadata: result_metadata,
                    result_artifact,
                }),
            }),
        };
        let access = SecretAccess {
            actor: SecretActor::User(owner),
            correlation_id: "bridge-validation-1".to_owned(),
            reason: "validate BrowserBridge result".to_owned(),
        };
        (
            database.clone(),
            BrowserBridgeCredentialValidationService::new(
                Arc::new(registry),
                FakeTasks {
                    owner,
                    task: task.clone(),
                },
                FakeAccounts(account),
            )
            .with_protocol_observations(Arc::new(
                SqliteProtocolObservationRepository::new(database),
            )),
            ValidateBrowserBridgeCredentialCommand {
                owner_user_id: owner,
                recovery,
                access,
            },
        )
    }
}
