use asterism_auth::{OpaqueTokenService, TokenError};
use asterism_domain::{
    AuditActor, BrowserBridgeSession, BrowserBridgeSessionCreate, BrowserBridgeSessionError,
    BrowserBridgeSessionId, ProviderAccountId, ProviderId, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{BrowserSessionSpec, BrowserSessionSpecError};
use asterism_secrets::SecretString;
use asterism_storage::{BrowserBridgeSessionRepository, StorageError};

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
    Spec(#[from] BrowserSessionSpecError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}
