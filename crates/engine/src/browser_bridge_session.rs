use asterism_auth::{OpaqueTokenService, TokenError};
use asterism_domain::{
    AuditActor, BrowserBridgeExchange, BrowserBridgeExchangeState,
    BrowserBridgeResultArtifactMetadata, BrowserBridgeRuntimeBinding,
    BrowserBridgeRuntimeBindingError, BrowserBridgeRuntimeStateMetadata, BrowserBridgeSession,
    BrowserBridgeSessionCreate, BrowserBridgeSessionError, BrowserBridgeSessionId,
    ProviderAccountId, ProviderId, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{BrowserSessionSpec, BrowserSessionSpecError};
use asterism_secrets::{
    CredentialBundle, SecretAccess, SecretStoreError, SecretString, SecretValue,
};
use asterism_storage::{
    BrowserBridgeCommandArtifactRepository, BrowserBridgeCommandDispatchRecord,
    BrowserBridgeCommandDispatchRequest as StorageBrowserBridgeCommandDispatchRequest,
    BrowserBridgeCommandIssueRequest as StorageBrowserBridgeCommandIssueRequest,
    BrowserBridgeCommandResolveRequest as StorageBrowserBridgeCommandResolveRequest,
    BrowserBridgeCredentialCommitOutcome,
    BrowserBridgeCredentialCommitRequest as StorageBrowserBridgeCredentialCommitRequest,
    BrowserBridgeCredentialRepository, BrowserBridgeExchangeRecord,
    BrowserBridgeResultArtifactRecord,
    BrowserBridgeResultReceiveRequest as StorageBrowserBridgeResultReceiveRequest,
    BrowserBridgeResultResolveRequest as StorageBrowserBridgeResultResolveRequest,
    BrowserBridgeRuntimeBindingRecord,
    BrowserBridgeRuntimeStateIssue as StorageBrowserBridgeRuntimeStateIssue,
    BrowserBridgeSessionRepository, ResolvedBrowserBridgeCommand, ResolvedBrowserBridgeResult,
    StorageError,
};

#[derive(Debug)]
pub struct BrowserBridgeHelperSessionService<R> {
    repository: R,
    pairing_tokens: OpaqueTokenService,
    access_tokens: OpaqueTokenService,
}

impl<R> BrowserBridgeHelperSessionService<R> {
    /// Builds the fixed token families used by a `BrowserBridge` helper.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] only when an internal static token prefix is
    /// inconsistent with the opaque-token contract.
    pub fn new(repository: R) -> Result<Self, TokenError> {
        Ok(Self {
            repository,
            pairing_tokens: OpaqueTokenService::new("ast_bridge_pair")?,
            access_tokens: OpaqueTokenService::new("ast_bridge")?,
        })
    }
}

impl<R> BrowserBridgeHelperSessionService<R>
where
    R: BrowserBridgeSessionRepository,
{
    /// Persists one immutable owner/account/Task/Provider/spec binding and
    /// returns its plaintext pairing token exactly once.
    ///
    /// # Errors
    ///
    /// Rejects an invalid session/specification or a failed atomic write.
    pub async fn create(
        &self,
        request: BrowserBridgeSessionCreateRequest,
    ) -> Result<BrowserBridgeSessionCreated, BrowserBridgeHelperSessionError> {
        let spec_digest = request.spec.digest()?;
        let session = BrowserBridgeSession::awaiting_claim(BrowserBridgeSessionCreate {
            owner_user_id: request.owner_user_id,
            provider_account_id: request.provider_account_id,
            task_id: request.task_id,
            provider_id: request.provider_id,
            provider_version: request.provider_version,
            spec_version: request.spec.version,
            spec_digest,
            created_at: request.created_at,
            expires_at: request.expires_at,
        })?;
        let (pairing_token, pairing_digest) = self.pairing_tokens.generate();
        self.repository
            .create_browser_bridge_session(
                &session,
                &request.spec,
                &pairing_digest,
                request.actor,
                &request.correlation_id,
            )
            .await?;
        Ok(BrowserBridgeSessionCreated {
            session,
            spec: request.spec,
            pairing_token,
        })
    }

    /// Reads one owner-scoped session without exposing either token digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is missing or persistence rejects its
    /// frozen binding.
    pub async fn read_owner(
        &self,
        owner_user_id: UserId,
        session_id: BrowserBridgeSessionId,
    ) -> Result<BrowserBridgeSessionSnapshot, BrowserBridgeHelperSessionError> {
        let (session, spec) = self
            .repository
            .find_browser_bridge_session(owner_user_id, session_id)
            .await?
            .ok_or(BrowserBridgeHelperSessionError::SessionNotFound(session_id))?;
        Ok(BrowserBridgeSessionSnapshot { session, spec })
    }

    /// Atomically consumes one pairing token and returns an independent,
    /// session-bound helper access token exactly once.
    ///
    /// # Errors
    ///
    /// Wrong, expired, replayed, cross-session, or terminal pairing attempts
    /// share one rejection result.
    pub async fn claim(
        &self,
        request: BrowserBridgeSessionClaimRequest,
    ) -> Result<BrowserBridgeSessionClaimed, BrowserBridgeHelperSessionError> {
        let pairing_digest = self.pairing_tokens.digest(&request.pairing_token);
        let (access_token, access_digest) = self.access_tokens.generate();
        let (session, spec) = self
            .repository
            .claim_browser_bridge_session(
                request.session_id,
                &pairing_digest,
                &access_digest,
                request.claimed_at,
                &request.correlation_id,
            )
            .await?
            .ok_or(BrowserBridgeHelperSessionError::PairingRejected)?;
        Ok(BrowserBridgeSessionClaimed {
            session,
            spec,
            access_token,
        })
    }

    /// Authenticates a helper token only for its exact claimed session.
    ///
    /// # Errors
    ///
    /// Wrong, expired, cancelled, terminal, or cross-session access tokens
    /// share one rejection result.
    pub async fn authenticate_access(
        &self,
        request: BrowserBridgeSessionAccessRequest,
    ) -> Result<BrowserBridgeSessionSnapshot, BrowserBridgeHelperSessionError> {
        let access_digest = self.access_tokens.digest(&request.access_token);
        let (session, spec) = self
            .repository
            .authenticate_browser_bridge_access(
                request.session_id,
                &access_digest,
                request.authenticated_at,
            )
            .await?
            .ok_or(BrowserBridgeHelperSessionError::AccessRejected)?;
        Ok(BrowserBridgeSessionSnapshot { session, spec })
    }

    /// Authenticates one helper and durably freezes the first observed browser
    /// origin/frame identity. Identical retries recover the original record;
    /// a different second writer never replaces it.
    ///
    /// # Errors
    ///
    /// Rejects invalid runtime syntax or a failed atomic repository write.
    pub async fn bind_runtime(
        &self,
        request: BrowserBridgeRuntimeBindRequest,
    ) -> Result<BrowserBridgeRuntimeBindingRecord, BrowserBridgeHelperSessionError> {
        request.binding.validate()?;
        let access_digest = self.access_tokens.digest(&request.access_token);
        Ok(self
            .repository
            .bind_browser_bridge_runtime(&request.binding, &access_digest, &request.correlation_id)
            .await?)
    }

    /// Cancels one owner-scoped live helper session and invalidates all tokens.
    /// An overdue session is durably expired instead.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, terminal, invalid, or concurrently changed
    /// sessions.
    pub async fn cancel(
        &self,
        request: BrowserBridgeSessionCancelRequest,
    ) -> Result<BrowserBridgeSessionSnapshot, BrowserBridgeHelperSessionError> {
        let (mut session, spec) = self
            .repository
            .find_browser_bridge_session(request.owner_user_id, request.session_id)
            .await?
            .ok_or(BrowserBridgeHelperSessionError::SessionNotFound(
                request.session_id,
            ))?;
        let expected_revision = session.revision;
        if session.is_expired_at(request.cancelled_at) {
            session.expire(request.cancelled_at)?;
        } else {
            session.cancel(request.cancelled_at)?;
        }
        if !self
            .repository
            .update_browser_bridge_session_for_owner(
                &session,
                expected_revision,
                request.actor,
                &request.correlation_id,
            )
            .await?
        {
            return Err(BrowserBridgeHelperSessionError::RevisionConflict(
                session.id,
            ));
        }
        Ok(BrowserBridgeSessionSnapshot { session, spec })
    }

    /// Completes one issued command with a typed provider result digest. A
    /// duplicate identical result is returned as `Duplicate`; a conflicting
    /// result is never replayed.
    ///
    /// # Errors
    ///
    /// Returns an error when the access token is rejected, metadata is invalid,
    /// or persistence detects a binding/sequence conflict.
    pub async fn complete_exchange(
        &self,
        request: BrowserBridgeExchangeRequest,
    ) -> Result<BrowserBridgeExchangeRecord, BrowserBridgeHelperSessionError> {
        let access_digest = self.access_tokens.digest(&request.access_token);
        Ok(self
            .repository
            .complete_browser_bridge_exchange(
                &request.exchange,
                &access_digest,
                &request.correlation_id,
            )
            .await?)
    }
}

/// Core-owned service for atomic encrypted `BrowserBridge` command issuance and
/// exact-command recovery. The repository is Provider-scoped at composition.
#[derive(Clone, Debug)]
pub struct BrowserBridgeCommandService<R> {
    repository: R,
}

impl<R> BrowserBridgeCommandService<R> {
    pub const fn new(repository: R) -> Self {
        Self { repository }
    }
}

impl<R> BrowserBridgeCommandService<R>
where
    R: BrowserBridgeCommandArtifactRepository,
{
    /// Persists the immutable exchange and exact encrypted command in one
    /// transaction before any helper dispatch can occur.
    ///
    /// # Errors
    ///
    /// Rejects invalid command bytes, access, binding, or sequence conflicts.
    pub async fn issue(
        &self,
        request: BrowserBridgeCommandIssueRequest,
    ) -> Result<BrowserBridgeExchangeRecord, BrowserBridgeCommandServiceError> {
        Ok(self
            .repository
            .issue_browser_bridge_command(StorageBrowserBridgeCommandIssueRequest {
                exchange: &request.exchange,
                command_artifact: request.command_artifact,
                runtime_state: request.runtime_state.map(|runtime_state| {
                    StorageBrowserBridgeRuntimeStateIssue {
                        metadata: runtime_state.metadata,
                        state_artifact: runtime_state.state_artifact,
                    }
                }),
                access: &request.access,
            })
            .await?)
    }

    /// Recovers one exact encrypted command only after re-binding its complete
    /// owner/account/Task/Provider identity.
    ///
    /// # Errors
    ///
    /// Rejects unauthorized, incomplete, tampered, or un-decryptable records.
    pub async fn resolve(
        &self,
        request: BrowserBridgeCommandResolveRequest,
    ) -> Result<Option<ResolvedBrowserBridgeCommand>, BrowserBridgeCommandServiceError> {
        Ok(self
            .repository
            .resolve_browser_bridge_command(StorageBrowserBridgeCommandResolveRequest {
                owner_user_id: request.owner_user_id,
                provider_account_id: request.provider_account_id,
                task_id: request.task_id,
                session_id: request.session_id,
                sequence: request.sequence,
                access: &request.access,
            })
            .await?)
    }
}

/// Reconstructs one owner-scoped `BrowserBridge` runtime after restart without
/// asking the helper to echo Provider command or cursor state.
#[derive(Clone, Debug)]
pub struct BrowserBridgeRuntimeRecoveryService<S, C> {
    sessions: S,
    commands: C,
}

impl<S, C> BrowserBridgeRuntimeRecoveryService<S, C> {
    pub const fn new(sessions: S, commands: C) -> Self {
        Self { sessions, commands }
    }
}

impl<S, C> BrowserBridgeRuntimeRecoveryService<S, C>
where
    S: BrowserBridgeSessionRepository,
    C: BrowserBridgeCommandArtifactRepository,
{
    /// Loads the frozen session policy and runtime binding, then resolves the
    /// latest exact command, optional Provider state and optional raw result.
    ///
    /// # Errors
    ///
    /// Fails closed on missing runtime identity, partial encrypted artifacts,
    /// cross-binding metadata or a terminal exchange without its raw result.
    pub async fn recover(
        &self,
        request: BrowserBridgeRuntimeRecoveryRequest,
    ) -> Result<BrowserBridgeRuntimeRecoverySnapshot, BrowserBridgeRuntimeRecoveryError> {
        let (session, spec) = self
            .sessions
            .find_browser_bridge_session(request.owner_user_id, request.session_id)
            .await?
            .ok_or(BrowserBridgeRuntimeRecoveryError::SessionNotFound(
                request.session_id,
            ))?;
        let binding = self
            .sessions
            .find_browser_bridge_runtime_binding(request.owner_user_id, request.session_id)
            .await?
            .ok_or(BrowserBridgeRuntimeRecoveryError::RuntimeBindingMissing(
                request.session_id,
            ))?;
        let latest_exchange = self
            .sessions
            .find_latest_browser_bridge_exchange(request.owner_user_id, request.session_id)
            .await?;
        let latest = if let Some(exchange) = latest_exchange {
            let command = self
                .commands
                .resolve_browser_bridge_command(StorageBrowserBridgeCommandResolveRequest {
                    owner_user_id: session.owner_user_id,
                    provider_account_id: session.provider_account_id,
                    task_id: session.task_id,
                    session_id: session.id,
                    sequence: exchange.sequence,
                    access: &request.access,
                })
                .await?
                .ok_or(BrowserBridgeRuntimeRecoveryError::CommandArtifactMissing)?;
            if command.exchange != exchange {
                return Err(BrowserBridgeRuntimeRecoveryError::ExchangeMismatch);
            }
            let result = self
                .commands
                .resolve_browser_bridge_result(StorageBrowserBridgeResultResolveRequest {
                    owner_user_id: session.owner_user_id,
                    provider_account_id: session.provider_account_id,
                    task_id: session.task_id,
                    session_id: session.id,
                    sequence: exchange.sequence,
                    access: &request.access,
                })
                .await?;
            if result
                .as_ref()
                .is_some_and(|result| result.exchange != exchange)
            {
                return Err(BrowserBridgeRuntimeRecoveryError::ExchangeMismatch);
            }
            if exchange.state != BrowserBridgeExchangeState::Issued && result.is_none() {
                return Err(BrowserBridgeRuntimeRecoveryError::ResultArtifactMissing);
            }
            Some(BrowserBridgeRecoveredExchange { command, result })
        } else {
            None
        };
        let recovered_exchange = latest
            .as_ref()
            .map(|latest| latest.command.exchange.clone());
        if self
            .sessions
            .find_latest_browser_bridge_exchange(request.owner_user_id, request.session_id)
            .await?
            != recovered_exchange
        {
            return Err(BrowserBridgeRuntimeRecoveryError::ExchangeMismatch);
        }
        Ok(BrowserBridgeRuntimeRecoverySnapshot {
            session,
            spec,
            binding,
            latest,
        })
    }
}

/// Core-owned one-shot helper dispatch boundary. The access token is checked
/// in the same Storage transaction that permanently records first dispatch,
/// so an ambiguous HTTP retry cannot replay the browser command.
#[derive(Debug)]
pub struct BrowserBridgeCommandDispatchService<R> {
    repository: R,
    access_tokens: OpaqueTokenService,
}

impl<R> BrowserBridgeCommandDispatchService<R> {
    /// # Errors
    ///
    /// Returns [`TokenError`] only if the fixed helper-token family is invalid.
    pub fn new(repository: R) -> Result<Self, TokenError> {
        Ok(Self {
            repository,
            access_tokens: OpaqueTokenService::new("ast_bridge")?,
        })
    }
}

impl<R> BrowserBridgeCommandDispatchService<R>
where
    R: BrowserBridgeCommandArtifactRepository,
{
    /// Resolves and marks one exact command dispatched atomically. A second
    /// call returns `AlreadyDispatched` and never exposes the command bytes.
    ///
    /// # Errors
    ///
    /// Rejects invalid helper access, corrupt encrypted command state or an
    /// unavailable encryption key without retrying dispatch.
    pub async fn dispatch(
        &self,
        request: BrowserBridgeCommandDispatchRequest,
    ) -> Result<BrowserBridgeCommandDispatchRecord, BrowserBridgeCommandServiceError> {
        let access_token_digest = self.access_tokens.digest(&request.access_token);
        Ok(self
            .repository
            .dispatch_browser_bridge_command(StorageBrowserBridgeCommandDispatchRequest {
                session_id: request.session_id,
                sequence: request.sequence,
                access_token_digest: &access_token_digest,
                dispatched_at: request.dispatched_at,
                access: &request.access,
            })
            .await?)
    }
}

/// Core-owned encrypted inbox for raw helper results. Receipt is durable before
/// Provider parsing; recovery can replay parsing without asking the helper to
/// echo browser state or credentials.
#[derive(Debug)]
pub struct BrowserBridgeResultArtifactService<R> {
    repository: R,
    access_tokens: OpaqueTokenService,
}

impl<R> BrowserBridgeResultArtifactService<R> {
    /// # Errors
    ///
    /// Returns [`TokenError`] only if the fixed helper-token family is invalid.
    pub fn new(repository: R) -> Result<Self, TokenError> {
        Ok(Self {
            repository,
            access_tokens: OpaqueTokenService::new("ast_bridge")?,
        })
    }
}

impl<R> BrowserBridgeResultArtifactService<R>
where
    R: BrowserBridgeCommandArtifactRepository,
{
    /// Encrypts one raw result under the exact session/sequence and returns an
    /// idempotent receipt classification without marking it Provider-accepted.
    ///
    /// # Errors
    ///
    /// Rejects invalid helper access, result bytes, bindings or sequence drift.
    pub async fn receive(
        &self,
        request: BrowserBridgeResultReceiveRequest,
    ) -> Result<BrowserBridgeResultArtifactRecord, BrowserBridgeCommandServiceError> {
        let access_token_digest = self.access_tokens.digest(&request.access_token);
        Ok(self
            .repository
            .receive_browser_bridge_result(StorageBrowserBridgeResultReceiveRequest {
                metadata: &request.metadata,
                result_artifact: request.result_artifact,
                access_token_digest: &access_token_digest,
                access: &request.access,
            })
            .await?)
    }

    /// Resolves one encrypted raw result only after complete owner/account/Task
    /// and Provider rebinding.
    ///
    /// # Errors
    ///
    /// Rejects unauthorized, incomplete, tampered or un-decryptable records.
    pub async fn resolve(
        &self,
        request: BrowserBridgeResultResolveRequest,
    ) -> Result<Option<ResolvedBrowserBridgeResult>, BrowserBridgeCommandServiceError> {
        Ok(self
            .repository
            .resolve_browser_bridge_result(StorageBrowserBridgeResultResolveRequest {
                owner_user_id: request.owner_user_id,
                provider_account_id: request.provider_account_id,
                task_id: request.task_id,
                session_id: request.session_id,
                sequence: request.sequence,
                access: &request.access,
            })
            .await?)
    }
}

/// Core-owned terminal boundary for a Provider-validated browser result that
/// replaces account credentials. Result, credentials and session completion
/// are committed atomically by Storage.
#[derive(Debug)]
pub struct BrowserBridgeCredentialCommitService<R> {
    repository: R,
    access_tokens: OpaqueTokenService,
}

impl<R> BrowserBridgeCredentialCommitService<R> {
    /// # Errors
    ///
    /// Returns [`TokenError`] only if the fixed helper-token family is invalid.
    pub fn new(repository: R) -> Result<Self, TokenError> {
        Ok(Self {
            repository,
            access_tokens: OpaqueTokenService::new("ast_bridge")?,
        })
    }
}

impl<R> BrowserBridgeCredentialCommitService<R>
where
    R: BrowserBridgeCredentialRepository,
{
    /// Digests the session-bound helper token and commits one already validated
    /// Provider result plus its replacement credentials exactly once.
    ///
    /// # Errors
    ///
    /// Returns a typed secret-storage error without retrying the terminal
    /// Provider mutation or accepting helper-supplied command authority.
    pub async fn commit(
        &self,
        request: BrowserBridgeCredentialCommitRequest,
    ) -> Result<BrowserBridgeCredentialCommitOutcome, BrowserBridgeCredentialCommitServiceError>
    {
        let access_token_digest = self.access_tokens.digest(&request.access_token);
        Ok(self
            .repository
            .commit_browser_bridge_credentials(StorageBrowserBridgeCredentialCommitRequest {
                exchange: &request.exchange,
                access_token_digest: &access_token_digest,
                validated_bundle: request.validated_bundle,
                access: &request.access,
            })
            .await?)
    }
}

#[derive(Clone, Debug)]
pub struct BrowserBridgeSessionCreateRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub spec: BrowserSessionSpec,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub actor: AuditActor,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct BrowserBridgeSessionCreated {
    pub session: BrowserBridgeSession,
    pub spec: BrowserSessionSpec,
    pub pairing_token: SecretString,
}

#[derive(Debug)]
pub struct BrowserBridgeSessionClaimRequest {
    pub session_id: BrowserBridgeSessionId,
    pub pairing_token: SecretString,
    pub claimed_at: Timestamp,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct BrowserBridgeSessionClaimed {
    pub session: BrowserBridgeSession,
    pub spec: BrowserSessionSpec,
    pub access_token: SecretString,
}

#[derive(Debug)]
pub struct BrowserBridgeSessionAccessRequest {
    pub session_id: BrowserBridgeSessionId,
    pub access_token: SecretString,
    pub authenticated_at: Timestamp,
}

#[derive(Debug)]
pub struct BrowserBridgeRuntimeBindRequest {
    pub binding: BrowserBridgeRuntimeBinding,
    pub access_token: SecretString,
    pub correlation_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeSessionSnapshot {
    pub session: BrowserBridgeSession,
    pub spec: BrowserSessionSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeSessionCancelRequest {
    pub owner_user_id: UserId,
    pub session_id: BrowserBridgeSessionId,
    pub cancelled_at: Timestamp,
    pub actor: AuditActor,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct BrowserBridgeCommandIssueRequest {
    pub exchange: BrowserBridgeExchange,
    pub command_artifact: SecretValue,
    pub runtime_state: Option<BrowserBridgeRuntimeStateIssue>,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct BrowserBridgeRuntimeStateIssue {
    pub metadata: BrowserBridgeRuntimeStateMetadata,
    pub state_artifact: SecretValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeCommandResolveRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub access: SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeRuntimeRecoveryRequest {
    pub owner_user_id: UserId,
    pub session_id: BrowserBridgeSessionId,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct BrowserBridgeRuntimeRecoverySnapshot {
    pub session: BrowserBridgeSession,
    pub spec: BrowserSessionSpec,
    pub binding: BrowserBridgeRuntimeBinding,
    pub latest: Option<BrowserBridgeRecoveredExchange>,
}

#[derive(Debug)]
pub struct BrowserBridgeRecoveredExchange {
    pub command: ResolvedBrowserBridgeCommand,
    pub result: Option<ResolvedBrowserBridgeResult>,
}

#[derive(Debug)]
pub struct BrowserBridgeCommandDispatchRequest {
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub access_token: SecretString,
    pub dispatched_at: Timestamp,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct BrowserBridgeCredentialCommitRequest {
    pub exchange: BrowserBridgeExchange,
    pub access_token: SecretString,
    pub validated_bundle: CredentialBundle,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct BrowserBridgeResultReceiveRequest {
    pub metadata: BrowserBridgeResultArtifactMetadata,
    pub result_artifact: SecretValue,
    pub access_token: SecretString,
    pub access: SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeResultResolveRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub access: SecretAccess,
}

#[derive(Debug)]
pub struct BrowserBridgeExchangeRequest {
    pub exchange: BrowserBridgeExchange,
    pub access_token: SecretString,
    pub correlation_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeHelperSessionError {
    #[error("BrowserBridge session `{0}` does not exist for this owner")]
    SessionNotFound(BrowserBridgeSessionId),
    #[error("BrowserBridge pairing token is invalid or expired")]
    PairingRejected,
    #[error("BrowserBridge access token is invalid or expired")]
    AccessRejected,
    #[error("BrowserBridge session `{0}` changed concurrently")]
    RevisionConflict(BrowserBridgeSessionId),
    #[error(transparent)]
    Domain(#[from] BrowserBridgeSessionError),
    #[error(transparent)]
    RuntimeBinding(#[from] BrowserBridgeRuntimeBindingError),
    #[error(transparent)]
    Spec(#[from] BrowserSessionSpecError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeCommandServiceError {
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeRuntimeRecoveryError {
    #[error("BrowserBridge session `{0}` does not exist for this owner")]
    SessionNotFound(BrowserBridgeSessionId),
    #[error("BrowserBridge session `{0}` has no durable runtime binding")]
    RuntimeBindingMissing(BrowserBridgeSessionId),
    #[error("latest BrowserBridge command artifact is missing")]
    CommandArtifactMissing,
    #[error("terminal BrowserBridge exchange has no durable raw result")]
    ResultArtifactMissing,
    #[error("BrowserBridge runtime recovery artifacts disagree on exchange identity")]
    ExchangeMismatch,
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserBridgeCredentialCommitServiceError {
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}
