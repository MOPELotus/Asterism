use std::sync::Arc;

use asterism_domain::{
    AuthMethod, BrowserBridgeExchange, BrowserBridgeExchangeState, BrowserBridgeSessionState,
    ProviderAccountId, ProviderId, TaskCapability, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{
    BrowserBridgeCredentialResultRequest, BrowserBridgeResultDisposition, ProviderContext,
    ProviderError, ProviderRegistry, SessionStatus,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, SecretAccess, SecretActor, SecretStoreError,
};
use asterism_storage::{
    BrowserBridgeCommandArtifactRepository, BrowserBridgeCredentialCommitOutcome,
    BrowserBridgeCredentialRepository, BrowserBridgeSessionRepository, ProviderAccountRepository,
    ProviderAccountRuntimeRepository, StorageError, TaskQueryRepository,
};

use crate::{
    BrowserBridgeCredentialCommitRequest, BrowserBridgeCredentialCommitService,
    BrowserBridgeRuntimeRecoveryRequest, BrowserBridgeRuntimeRecoveryService,
    BrowserBridgeRuntimeRecoverySnapshot, CredentialProvisionError, credential::validate_candidate,
};

/// Core-owned validation of one recovered terminal `BrowserBridge` credential
/// result before the existing atomic commit boundary.
#[derive(Clone, Debug)]
pub struct BrowserBridgeCredentialValidationService<Q, A> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
}

impl<Q, A> BrowserBridgeCredentialValidationService<Q, A> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A) -> Self {
        Self {
            registry,
            tasks,
            accounts,
        }
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
            .await?;
        let (replacement, completed_exchange) = accepted.into_parts();
        require_exact_completion(
            &recovered.command.exchange,
            &completed_exchange,
            &result.metadata,
        )?;
        let bundle = CredentialBundle {
            provider_id: account.provider_id,
            tenant: account.tenant,
            auth_method: AuthMethod::AssistedSession,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at: result.metadata.received_at,
            expires_at: None,
            session_kind: replacement.session_kind,
            fields: replacement.fields,
            user_id_hint: None,
        };
        let (bundle, status) = validate_candidate(
            self.registry.as_ref(),
            &self.accounts,
            owner_user_id,
            recovery.session.provider_account_id,
            bundle,
            None,
            &access,
        )
        .await?;
        Ok(ValidatedBrowserBridgeCredential {
            owner_user_id,
            provider_account_id: recovery.session.provider_account_id,
            task_id: recovery.session.task_id,
            completed_exchange,
            bundle,
            status,
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
#[derive(Clone, Debug)]
pub struct BrowserBridgeCredentialProcessor<S, C, Q, A, R> {
    provider_id: ProviderId,
    registry: Arc<ProviderRegistry>,
    sessions: S,
    commands: C,
    tasks: Q,
    accounts: A,
    credentials: R,
}

impl<S, C, Q, A, R> BrowserBridgeCredentialProcessor<S, C, Q, A, R> {
    pub const fn new(
        provider_id: ProviderId,
        registry: Arc<ProviderRegistry>,
        sessions: S,
        commands: C,
        tasks: Q,
        accounts: A,
        credentials: R,
    ) -> Self {
        Self {
            provider_id,
            registry,
            sessions,
            commands,
            tasks,
            accounts,
            credentials,
        }
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
    pub async fn tick(
        &self,
        now: Timestamp,
        limit: u32,
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
        let pending = self
            .sessions
            .list_pending_browser_bridge_results(now, &self.provider_id, result_types, limit)
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
                report.failed += 1;
                continue;
            };
            let validated = BrowserBridgeCredentialValidationService::new(
                self.registry.clone(),
                self.tasks.clone(),
                self.accounts.clone(),
            )
            .validate(ValidateBrowserBridgeCredentialCommand {
                owner_user_id: candidate.owner_user_id,
                recovery,
                access: access.clone(),
            })
            .await;
            let Ok(validated) = validated else {
                report.failed += 1;
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
                ) => report.conflicted += 1,
                Err(_) => report.failed += 1,
            }
        }
        Ok(report)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrowserBridgeCredentialTickReport {
    pub selected: u32,
    pub committed: u32,
    pub conflicted: u32,
    pub failed: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeCredentialProcessorError {
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no BrowserBridge capability")]
    CapabilityUnavailable(ProviderId),
    #[error("provider `{0}` has an invalid BrowserBridge credential-result declaration")]
    InvalidResultTypeDeclaration(ProviderId),
    #[error(transparent)]
    Storage(#[from] StorageError),
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
        OrchestrationState, ProviderAccount, ProviderAccountId, RemoteState, SourceType, Task,
        TaskId, Timestamp,
    };
    use asterism_provider_api::{
        AuthChallenge, AuthenticationCapability, BrowserBridgeCapability,
        BrowserBridgeCredentialResult, CredentialReplacement, CredentialValidation,
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        ProviderRuntimeSettingsSchema, VerificationLevel,
    };
    use asterism_secrets::{CredentialField, SecretActor, SecretPurpose, SecretValue};
    use asterism_storage::{ResolvedBrowserBridgeCommand, ResolvedBrowserBridgeResult, TaskPage};
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use sha2::Digest;

    use super::*;

    #[derive(Debug)]
    struct FakeProvider(ProviderMetadata);

    impl ProviderIdentity for FakeProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.0
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
        let (service, command) = fixture("https://provider.example");
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
        let (service, command) = fixture("https://foreign.example");
        assert!(matches!(
            service.validate(command).await,
            Err(BrowserBridgeCredentialValidationError::InvalidRecovery)
        ));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the integration fixture constructs every durable BrowserBridge, Task, account, artifact and Provider binding explicitly"
    )]
    fn fixture(
        observed_origin: &str,
    ) -> (
        BrowserBridgeCredentialValidationService<FakeTasks, FakeAccounts>,
        ValidateBrowserBridgeCredentialCommand,
    ) {
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
        let provider = Arc::new(FakeProvider(metadata.clone()));
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
            BrowserBridgeCredentialValidationService::new(
                Arc::new(registry),
                FakeTasks {
                    owner,
                    task: task.clone(),
                },
                FakeAccounts(account),
            ),
            ValidateBrowserBridgeCredentialCommand {
                owner_user_id: owner,
                recovery,
                access,
            },
        )
    }
}
