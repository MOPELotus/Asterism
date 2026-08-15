use std::sync::Arc;

use asterism_domain::{
    AuditActor, AuthMethod, AuthSession, AuthSessionError, AuthSessionId, AuthState,
    ExternalOauthPending, ExternalOauthPendingCreate, ExternalOauthState, HumanRequiredReason,
    ProviderAccountId, ProviderId, Timestamp, UserId,
};
use asterism_provider_api::{
    AuthChallenge, ExternalOauthCallbackBinding, ProviderAuthContext, ProviderError,
    ProviderErrorKind, ProviderRegistry, SessionStatus,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, ProviderCredential, SecretAccess, SecretStoreError,
    SecretString,
};
use asterism_storage::{
    AuthSessionRepository, AuthenticatedCredentialRepository, ProviderAccountRepository,
    StorageError,
};
use chrono::Utc;

use crate::credential::{CredentialProvisionError, validate_candidate};

#[derive(Clone, Debug)]
pub struct AuthSessionService<A, S> {
    registry: Arc<ProviderRegistry>,
    accounts: A,
    sessions: S,
}

impl<A, S> AuthSessionService<A, S> {
    pub const fn new(registry: Arc<ProviderRegistry>, accounts: A, sessions: S) -> Self {
        Self {
            registry,
            accounts,
            sessions,
        }
    }
}

impl<A, S> AuthSessionService<A, S>
where
    A: ProviderAccountRepository,
    S: AuthSessionRepository,
{
    /// Creates an observable Core session before asking the Provider for its
    /// first user challenge.
    ///
    /// # Errors
    ///
    /// Returns [`AuthSessionServiceError`] when the account, advertised method,
    /// Provider challenge, state transition, or persistence operation is
    /// invalid. Provider failures are classified into an observable session
    /// state before the error is returned.
    #[allow(
        clippy::too_many_lines,
        reason = "authentication start keeps Provider failure classification and durable challenge creation in one auditable state flow"
    )]
    pub async fn begin(
        &self,
        request: AuthSessionStartRequest,
    ) -> Result<AuthSessionBegin, AuthSessionServiceError> {
        let AuthSessionStartRequest {
            owner_user_id,
            provider_account_id,
            method,
            created_at,
            expires_at,
            actor,
            correlation_id,
        } = request;
        let account = self
            .accounts
            .find_provider_account(owner_user_id, provider_account_id)
            .await?
            .ok_or(AuthSessionServiceError::AccountNotFound(
                provider_account_id,
            ))?;
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            AuthSessionServiceError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        if !entry.metadata.auth_methods.contains(&method) {
            return Err(AuthSessionServiceError::UnsupportedAuthMethod(method));
        }
        let authentication = entry
            .authentication
            .as_ref()
            .ok_or(AuthSessionServiceError::AuthenticationUnavailable)?;
        let mut session = AuthSession::starting(
            owner_user_id,
            provider_account_id,
            method,
            created_at,
            expires_at,
        )?;
        self.sessions
            .create_auth_session(&session, actor, &correlation_id)
            .await?;
        let provider_id = account.provider_id.clone();
        let context = ProviderAuthContext {
            provider_id: account.provider_id,
            account_id: account.id,
            auth_session_id: Some(session.id),
            correlation_id: correlation_id.clone(),
        };
        let challenge = match authentication.begin_authentication(&context, method).await {
            Ok(challenge) => challenge,
            Err(error) => {
                transition_provider_failure(
                    &self.sessions,
                    &mut session,
                    &error,
                    actor,
                    &correlation_id,
                )
                .await?;
                return Err(AuthSessionServiceError::Provider {
                    session_id: session.id,
                    source: error,
                });
            }
        };
        if Utc::now() >= session.expires_at {
            transition_once(
                &self.sessions,
                &mut session,
                AuthState::Expired,
                actor,
                &correlation_id,
            )
            .await?;
            return Err(AuthSessionServiceError::SessionExpired(session.id));
        }
        if !valid_challenge(&session, &challenge) {
            transition_once(
                &self.sessions,
                &mut session,
                AuthState::AuthFailed,
                actor,
                &correlation_id,
            )
            .await?;
            return Err(AuthSessionServiceError::InvalidChallenge(session.id));
        }
        let expected_revision = session.revision;
        session.transition(AuthState::WaitingUser(challenge.waiting_for), Utc::now())?;
        if let Some(authorization) = &challenge.external_oauth {
            let binding = authorization.callback_binding;
            let pending = ExternalOauthPending::pending(ExternalOauthPendingCreate {
                auth_session_id: session.id,
                owner_user_id,
                provider_account_id,
                provider_id,
                state_digest: binding.state_digest(),
                provider_context_digest: binding.provider_context_digest(),
                created_at: session.created_at,
                expires_at: session.expires_at,
            })?;
            self.sessions
                .create_external_oauth_pending(
                    &pending,
                    &session,
                    expected_revision,
                    actor,
                    &correlation_id,
                )
                .await?;
        } else if !self
            .sessions
            .update_auth_session(&session, expected_revision, actor, &correlation_id)
            .await?
        {
            return Err(AuthSessionServiceError::RevisionConflict(session.id));
        }
        Ok(AuthSessionBegin { session, challenge })
    }

    /// Cancels the latest still-transitionable owner-scoped attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when the session does not exist, is stale or terminal,
    /// or the optimistic update loses a race.
    pub async fn cancel(
        &self,
        owner_user_id: UserId,
        session_id: AuthSessionId,
        actor: AuditActor,
        correlation_id: &str,
        at: Timestamp,
    ) -> Result<AuthSession, AuthSessionServiceError> {
        let mut session = self
            .sessions
            .find_auth_session(owner_user_id, session_id)
            .await?
            .ok_or(AuthSessionServiceError::SessionNotFound(session_id))?;
        let next = if session.is_expired_at(at) {
            AuthState::Expired
        } else {
            AuthState::Cancelled
        };
        let expected_revision = session.revision;
        session
            .transition(next, at)
            .map_err(|_| AuthSessionServiceError::InvalidSessionState(session_id))?;
        if !self
            .sessions
            .update_auth_session(&session, expected_revision, actor, correlation_id)
            .await?
        {
            return Err(AuthSessionServiceError::RevisionConflict(session_id));
        }
        Ok(session)
    }

    /// Returns the durable external OAuth callback status after reconciling
    /// any crash window. Recovery never calls the Provider or replays a
    /// consumed callback.
    ///
    /// # Errors
    ///
    /// Returns a storage or state error when the owner-scoped callback and
    /// authentication session cannot be reconciled safely.
    pub async fn recover_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        session_id: AuthSessionId,
        actor: AuditActor,
        correlation_id: &str,
        at: Timestamp,
    ) -> Result<Option<ExternalOauthPending>, AuthSessionServiceError> {
        Ok(self
            .sessions
            .recover_external_oauth_pending(
                owner_user_id,
                provider_account_id,
                session_id,
                at,
                actor,
                correlation_id,
            )
            .await?
            .map(|claim| claim.pending))
    }

    /// Submits one in-memory candidate through Provider validation and commits
    /// it only while this remains the account's latest authentication session.
    ///
    /// # Errors
    ///
    /// Provider rejection and availability failures are persisted on the
    /// session. Superseded sessions cannot commit credentials or account state.
    pub async fn submit_credentials<C>(
        &self,
        credential_store: &C,
        request: AuthSessionCredentialRequest,
    ) -> Result<AuthSessionCredentialCommit, AuthSessionServiceError>
    where
        C: AuthenticatedCredentialRepository,
    {
        let AuthSessionCredentialRequest {
            owner_user_id,
            provider_account_id,
            session_id,
            bundle,
            access,
        } = request;
        if !access.authorizes(owner_user_id) {
            return Err(AuthSessionServiceError::Credential(
                CredentialProvisionError::Unauthorized,
            ));
        }
        let actor = audit_actor_from_access(&access)?;
        let mut session = self
            .enter_credential_validation(
                owner_user_id,
                provider_account_id,
                session_id,
                bundle.auth_method,
                actor,
                &access.correlation_id,
            )
            .await?;
        let (bundle, status) = match validate_candidate(
            self.registry.as_ref(),
            &self.accounts,
            owner_user_id,
            provider_account_id,
            bundle,
            Some(session_id),
            &access,
        )
        .await
        {
            Ok(validated) => validated,
            Err(error) => {
                transition_once(
                    &self.sessions,
                    &mut session,
                    credential_failure_state(&error),
                    actor,
                    &access.correlation_id,
                )
                .await?;
                return Err(AuthSessionServiceError::Credential(error));
            }
        };
        let validating_session = session.clone();
        if session.is_expired_at(Utc::now()) {
            transition_once(
                &self.sessions,
                &mut session,
                AuthState::Expired,
                actor,
                &access.correlation_id,
            )
            .await?;
            return Err(AuthSessionServiceError::SessionExpired(session.id));
        }
        let validating_revision = validating_session.revision;
        session.transition(AuthState::Authenticated, Utc::now())?;
        let credentials = match credential_store
            .commit_authenticated_credentials(
                owner_user_id,
                provider_account_id,
                bundle,
                &session,
                validating_revision,
                &access,
            )
            .await
        {
            Ok(credentials) => credentials,
            Err(SecretStoreError::VersionConflict) => {
                return Err(AuthSessionServiceError::RevisionConflict(session.id));
            }
            Err(error) => {
                let mut failed_session = validating_session;
                transition_once(
                    &self.sessions,
                    &mut failed_session,
                    AuthState::ProviderUnavailable,
                    actor,
                    &access.correlation_id,
                )
                .await?;
                return Err(AuthSessionServiceError::CredentialStore(error));
            }
        };
        Ok(AuthSessionCredentialCommit {
            session,
            status,
            credentials,
        })
    }

    /// Claims and consumes one owner/account/AuthSession-bound external OAuth
    /// callback, invokes the Provider exactly once, validates its replacement
    /// credential, and commits it through the existing atomic secret boundary.
    ///
    /// # Errors
    ///
    /// Returns a typed conflict for missing/stale claims, preserves Provider
    /// errors after recording a consumed terminal pending state, and never
    /// reopens or retries a callback after the claim succeeds.
    #[allow(
        clippy::too_many_lines,
        reason = "the one-shot OAuth exchange keeps claim, Provider call, validation, credential commit and terminal receipt ordering visible together"
    )]
    pub async fn submit_external_oauth_callback<C>(
        &self,
        credential_store: &C,
        request: ExternalOauthCallbackRequest,
    ) -> Result<AuthSessionCredentialCommit, AuthSessionServiceError>
    where
        C: AuthenticatedCredentialRepository,
    {
        let ExternalOauthCallbackRequest {
            owner_user_id,
            provider_account_id,
            session_id,
            callback_url,
            access,
        } = request;
        if !access.authorizes(owner_user_id) {
            return Err(AuthSessionServiceError::Credential(
                CredentialProvisionError::Unauthorized,
            ));
        }
        let actor = audit_actor_from_access(&access)?;
        let account = self
            .accounts
            .find_provider_account(owner_user_id, provider_account_id)
            .await?
            .ok_or(AuthSessionServiceError::AccountNotFound(
                provider_account_id,
            ))?;
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            AuthSessionServiceError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let authentication = entry
            .authentication
            .as_ref()
            .ok_or(AuthSessionServiceError::AuthenticationUnavailable)?;
        let Some(mut claim) = self
            .sessions
            .claim_external_oauth_pending(
                owner_user_id,
                provider_account_id,
                session_id,
                Utc::now(),
                actor,
                &access.correlation_id,
            )
            .await?
        else {
            return Err(AuthSessionServiceError::RevisionConflict(session_id));
        };
        if !matches!(
            claim.auth_session.method,
            AuthMethod::ExternalBrowserOauth | AuthMethod::AssistedSession
        ) || claim.pending.provider_id != account.provider_id
        {
            return Err(AuthSessionServiceError::InvalidSessionState(session_id));
        }
        let context = ProviderAuthContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            auth_session_id: Some(session_id),
            correlation_id: access.correlation_id.clone(),
        };
        let binding = ExternalOauthCallbackBinding::from_digests(
            claim.pending.state_digest,
            claim.pending.provider_context_digest,
        );
        let replacement = match authentication
            .exchange_external_oauth_callback(&context, callback_url, binding)
            .await
        {
            Ok(replacement) => replacement,
            Err(error) => {
                finish_oauth_provider_failure(
                    &self.sessions,
                    &mut claim.pending,
                    &mut claim.auth_session,
                    &error,
                    actor,
                    &access.correlation_id,
                )
                .await?;
                return Err(AuthSessionServiceError::Provider {
                    session_id,
                    source: error,
                });
            }
        };
        transition_once(
            &self.sessions,
            &mut claim.auth_session,
            AuthState::ValidatingCredential,
            actor,
            &access.correlation_id,
        )
        .await?;
        let bundle = CredentialBundle {
            provider_id: account.provider_id,
            tenant: account.tenant,
            auth_method: claim.auth_session.method,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: replacement.session_kind,
            fields: replacement.fields,
            user_id_hint: None,
        };
        let (bundle, status) = match validate_candidate(
            self.registry.as_ref(),
            &self.accounts,
            owner_user_id,
            provider_account_id,
            bundle,
            Some(session_id),
            &access,
        )
        .await
        {
            Ok(validated) => validated,
            Err(error) => {
                finish_oauth_credential_failure(
                    &self.sessions,
                    &mut claim.pending,
                    &mut claim.auth_session,
                    &error,
                    actor,
                    &access.correlation_id,
                )
                .await?;
                return Err(AuthSessionServiceError::Credential(error));
            }
        };
        let validating_session = claim.auth_session.clone();
        let validating_revision = validating_session.revision;
        claim
            .auth_session
            .transition(AuthState::Authenticated, Utc::now())?;
        let credentials = match credential_store
            .commit_authenticated_credentials(
                owner_user_id,
                provider_account_id,
                bundle,
                &claim.auth_session,
                validating_revision,
                &access,
            )
            .await
        {
            Ok(credentials) => credentials,
            Err(error) => {
                claim.auth_session = validating_session;
                finish_oauth_pending(
                    &self.sessions,
                    &mut claim.pending,
                    ExternalOauthState::Failed,
                    actor,
                    &access.correlation_id,
                )
                .await?;
                if !matches!(&error, SecretStoreError::VersionConflict) {
                    transition_once(
                        &self.sessions,
                        &mut claim.auth_session,
                        AuthState::ProviderUnavailable,
                        actor,
                        &access.correlation_id,
                    )
                    .await?;
                }
                return Err(match error {
                    SecretStoreError::VersionConflict => {
                        AuthSessionServiceError::RevisionConflict(session_id)
                    }
                    other => AuthSessionServiceError::CredentialStore(other),
                });
            }
        };
        let expected_pending_revision = claim.pending.revision;
        claim
            .pending
            .finish(ExternalOauthState::Succeeded, Utc::now())?;
        if !self
            .sessions
            .update_external_oauth_pending(
                &claim.pending,
                expected_pending_revision,
                actor,
                &access.correlation_id,
            )
            .await?
        {
            return Err(AuthSessionServiceError::RevisionConflict(session_id));
        }
        Ok(AuthSessionCredentialCommit {
            session: claim.auth_session,
            status,
            credentials,
        })
    }

    async fn enter_credential_validation(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        session_id: AuthSessionId,
        method: AuthMethod,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<AuthSession, AuthSessionServiceError> {
        let mut session = self
            .sessions
            .find_auth_session(owner_user_id, session_id)
            .await?
            .filter(|session| session.provider_account_id == provider_account_id)
            .ok_or(AuthSessionServiceError::SessionNotFound(session_id))?;
        if session.method != method || !matches!(session.state, AuthState::WaitingUser(_)) {
            return Err(AuthSessionServiceError::InvalidSessionState(session_id));
        }
        if session.is_expired_at(Utc::now()) {
            transition_once(
                &self.sessions,
                &mut session,
                AuthState::Expired,
                actor,
                correlation_id,
            )
            .await?;
            return Err(AuthSessionServiceError::SessionExpired(session.id));
        }
        let expected_revision = session.revision;
        session.transition(AuthState::ValidatingCredential, Utc::now())?;
        if !self
            .sessions
            .update_auth_session(&session, expected_revision, actor, correlation_id)
            .await?
        {
            return Err(AuthSessionServiceError::RevisionConflict(session_id));
        }
        Ok(session)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthSessionBegin {
    pub session: AuthSession,
    pub challenge: AuthChallenge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthSessionStartRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub method: AuthMethod,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub actor: AuditActor,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct AuthSessionCredentialRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub session_id: AuthSessionId,
    pub bundle: CredentialBundle,
    pub access: SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthSessionCredentialCommit {
    pub session: AuthSession,
    pub status: SessionStatus,
    pub credentials: Vec<ProviderCredential>,
}

#[derive(Debug)]
pub struct ExternalOauthCallbackRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub session_id: AuthSessionId,
    pub callback_url: SecretString,
    pub access: SecretAccess,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthSessionServiceError {
    #[error("Provider account `{0}` does not exist for this owner")]
    AccountNotFound(ProviderAccountId),
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider does not expose authentication")]
    AuthenticationUnavailable,
    #[error("provider does not advertise authentication method `{0:?}`")]
    UnsupportedAuthMethod(AuthMethod),
    #[error("provider returned an invalid challenge for authentication session `{0}`")]
    InvalidChallenge(AuthSessionId),
    #[error("authentication session `{0}` expired while starting")]
    SessionExpired(AuthSessionId),
    #[error("authentication session `{0}` does not exist for this owner")]
    SessionNotFound(AuthSessionId),
    #[error("authentication session `{0}` cannot transition from its current state")]
    InvalidSessionState(AuthSessionId),
    #[error("authentication session `{0}` changed concurrently or is no longer current")]
    RevisionConflict(AuthSessionId),
    #[error("provider authentication failed for session `{session_id}`: {source}")]
    Provider {
        session_id: AuthSessionId,
        #[source]
        source: ProviderError,
    },
    #[error(transparent)]
    Credential(#[from] CredentialProvisionError),
    #[error("authenticated credential commit failed: {0}")]
    CredentialStore(SecretStoreError),
    #[error(transparent)]
    Domain(#[from] AuthSessionError),
    #[error(transparent)]
    ExternalOauthDomain(#[from] asterism_domain::ExternalOauthPendingError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

fn valid_challenge(session: &AuthSession, challenge: &AuthChallenge) -> bool {
    challenge.session_id == session.id
        && challenge.method == session.method
        && challenge
            .expires_at
            .is_none_or(|expires_at| expires_at > Utc::now() && expires_at <= session.expires_at)
        && challenge.user_action.as_deref().is_none_or(|action| {
            !action.is_empty() && action.len() <= 4096 && !action.chars().any(char::is_control)
        })
        && challenge
            .external_oauth
            .as_ref()
            .is_none_or(|authorization| {
                matches!(
                    challenge.method,
                    AuthMethod::ExternalBrowserOauth | AuthMethod::AssistedSession
                ) && challenge.waiting_for == asterism_domain::WaitingUserState::BrowserCallback
                    && authorization.validate()
            })
}

async fn transition_provider_failure<S: AuthSessionRepository>(
    sessions: &S,
    session: &mut AuthSession,
    error: &ProviderError,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), AuthSessionServiceError> {
    transition_once(
        sessions,
        session,
        provider_failure_state(error),
        actor,
        correlation_id,
    )
    .await
}

fn provider_failure_state(error: &ProviderError) -> AuthState {
    match error.kind {
        ProviderErrorKind::RateLimited
        | ProviderErrorKind::Network
        | ProviderErrorKind::ProviderUnavailable => AuthState::ProviderUnavailable,
        ProviderErrorKind::HumanRequired => AuthState::HumanRequired(
            error
                .human_required_reason
                .unwrap_or(HumanRequiredReason::ManualIntervention),
        ),
        ProviderErrorKind::RemoteChanged => AuthState::ClientUpdateRequired,
        ProviderErrorKind::Authentication
        | ProviderErrorKind::Authorization
        | ProviderErrorKind::ProtocolDrift
        | ProviderErrorKind::UnsupportedTask
        | ProviderErrorKind::InvalidResponse
        | ProviderErrorKind::Internal => AuthState::AuthFailed,
    }
}

fn credential_failure_state(error: &CredentialProvisionError) -> AuthState {
    match error {
        CredentialProvisionError::Provider(error) => provider_failure_state(error),
        CredentialProvisionError::ProviderNotRegistered(_)
        | CredentialProvisionError::AuthenticationUnavailable
        | CredentialProvisionError::UnsupportedAuthMethod(_)
        | CredentialProvisionError::UnsupportedSessionKind(_) => AuthState::ClientUpdateRequired,
        CredentialProvisionError::Storage(_) | CredentialProvisionError::SecretStore(_) => {
            AuthState::ProviderUnavailable
        }
        CredentialProvisionError::Unauthorized
        | CredentialProvisionError::InvalidBundle(_)
        | CredentialProvisionError::AccountNotFound(_)
        | CredentialProvisionError::AccountMismatch
        | CredentialProvisionError::CredentialRejected
        | CredentialProvisionError::InvalidProviderStatus
        | CredentialProvisionError::InvalidProtocolObservation => AuthState::AuthFailed,
    }
}

async fn finish_oauth_provider_failure<S: AuthSessionRepository>(
    sessions: &S,
    pending: &mut ExternalOauthPending,
    session: &mut AuthSession,
    error: &ProviderError,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), AuthSessionServiceError> {
    let outcome = if matches!(
        error.kind,
        ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable
    ) {
        ExternalOauthState::Ambiguous
    } else {
        ExternalOauthState::Failed
    };
    finish_oauth_pending(sessions, pending, outcome, actor, correlation_id).await?;
    transition_once(
        sessions,
        session,
        provider_failure_state(error),
        actor,
        correlation_id,
    )
    .await
}

async fn finish_oauth_credential_failure<S: AuthSessionRepository>(
    sessions: &S,
    pending: &mut ExternalOauthPending,
    session: &mut AuthSession,
    error: &CredentialProvisionError,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), AuthSessionServiceError> {
    let outcome = match error {
        CredentialProvisionError::Provider(error)
            if matches!(
                error.kind,
                ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable
            ) =>
        {
            ExternalOauthState::Ambiguous
        }
        _ => ExternalOauthState::Failed,
    };
    finish_oauth_pending(sessions, pending, outcome, actor, correlation_id).await?;
    transition_once(
        sessions,
        session,
        credential_failure_state(error),
        actor,
        correlation_id,
    )
    .await
}

async fn finish_oauth_pending<S: AuthSessionRepository>(
    sessions: &S,
    pending: &mut ExternalOauthPending,
    outcome: ExternalOauthState,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), AuthSessionServiceError> {
    let expected_revision = pending.revision;
    pending.finish(outcome, Utc::now())?;
    if sessions
        .update_external_oauth_pending(pending, expected_revision, actor, correlation_id)
        .await?
    {
        Ok(())
    } else {
        Err(AuthSessionServiceError::RevisionConflict(
            pending.auth_session_id,
        ))
    }
}

fn audit_actor_from_access(access: &SecretAccess) -> Result<AuditActor, AuthSessionServiceError> {
    match access.actor {
        asterism_secrets::SecretActor::User(user_id) => Ok(AuditActor::User(user_id)),
        asterism_secrets::SecretActor::ServiceToken(token_id) => {
            Ok(AuditActor::ServiceToken(token_id))
        }
        asterism_secrets::SecretActor::CoreService(_)
        | asterism_secrets::SecretActor::ProviderRuntime(_) => Err(
            AuthSessionServiceError::Credential(CredentialProvisionError::Unauthorized),
        ),
    }
}

async fn transition_once<S: AuthSessionRepository>(
    sessions: &S,
    session: &mut AuthSession,
    next: AuthState,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), AuthSessionServiceError> {
    let expected_revision = session.revision;
    let at = Utc::now();
    let next = if session.is_expired_at(at) {
        AuthState::Expired
    } else {
        next
    };
    session.transition(next, at)?;
    if sessions
        .update_auth_session(session, expected_revision, actor, correlation_id)
        .await?
    {
        Ok(())
    } else {
        Err(AuthSessionServiceError::RevisionConflict(session.id))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use asterism_domain::{ProviderAccount, Role, SessionKind, WaitingUserState};
    use asterism_provider_api::{
        AuthenticationCapability, CredentialReplacement, CredentialValidation,
        ExternalOauthAuthorization, ExternalOauthCallbackBinding, ProviderCapability,
        ProviderContext, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        SessionStatus, VerificationLevel,
    };
    use asterism_secrets::{
        CredentialAcquisition, CredentialBundle, CredentialField, SecretAccess, SecretActor,
        SecretKey, SecretPurpose, SecretStore, SecretValue,
    };
    use asterism_storage::{
        Database, SecretKeyring, SqliteAuthSessionRepository, SqliteProviderAccountRepository,
        SqliteSecretStore,
    };
    use async_trait::async_trait;
    use chrono::Duration;

    use super::*;

    #[test]
    fn provider_human_required_reason_is_preserved() {
        let captcha =
            ProviderError::human_required("sanitized challenge", HumanRequiredReason::ImageCaptcha);
        assert_eq!(
            provider_failure_state(&captcha),
            AuthState::HumanRequired(HumanRequiredReason::ImageCaptcha)
        );
        assert_eq!(
            provider_failure_state(&ProviderError::new(
                ProviderErrorKind::HumanRequired,
                "sanitized challenge",
            )),
            AuthState::HumanRequired(HumanRequiredReason::ManualIntervention)
        );
    }

    #[tokio::test]
    async fn begin_and_cancel_are_persisted_as_one_observable_state_flow() {
        let fixture = fixture(true).await;
        let now = Utc::now();
        let started = fixture
            .service
            .begin(AuthSessionStartRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                method: AuthMethod::QrCode,
                created_at: now,
                expires_at: now + Duration::minutes(10),
                actor: AuditActor::User(fixture.owner),
                correlation_id: "auth-begin-test".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            started.session.state,
            AuthState::WaitingUser(WaitingUserState::QrScan)
        );
        assert_eq!(started.session.revision, 2);
        let cancelled = fixture
            .service
            .cancel(
                fixture.owner,
                started.session.id,
                AuditActor::User(fixture.owner),
                "auth-cancel-test",
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.state, AuthState::Cancelled);
        assert_eq!(cancelled.revision, 3);
        assert_eq!(
            fixture
                .sessions
                .find_auth_session(fixture.owner, cancelled.id)
                .await
                .unwrap(),
            Some(cancelled)
        );
    }

    #[tokio::test]
    async fn mismatched_provider_challenge_is_recorded_as_auth_failed() {
        let fixture = fixture(false).await;
        let now = Utc::now();
        let error = fixture
            .service
            .begin(AuthSessionStartRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                method: AuthMethod::QrCode,
                created_at: now,
                expires_at: now + Duration::minutes(10),
                actor: AuditActor::User(fixture.owner),
                correlation_id: "auth-invalid-challenge".to_owned(),
            })
            .await
            .unwrap_err();
        let session_id = match error {
            AuthSessionServiceError::InvalidChallenge(session_id) => session_id,
            error => panic!("unexpected error: {error}"),
        };
        let stored = fixture
            .sessions
            .find_auth_session(fixture.owner, session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, AuthState::AuthFailed);
        assert_eq!(stored.revision, 2);
    }

    #[tokio::test]
    async fn validated_credentials_and_authenticated_session_commit_together() {
        let fixture = fixture(true).await;
        let started = begin_session(&fixture, "auth-credential-start").await;
        let committed = fixture
            .service
            .submit_credentials(
                &fixture.store,
                AuthSessionCredentialRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    bundle: credential_bundle(b"session-cookie"),
                    access: secret_access(fixture.owner, "auth-credential-submit"),
                },
            )
            .await
            .unwrap();

        assert_eq!(committed.session.state, AuthState::Authenticated);
        assert_eq!(committed.session.revision, 4);
        assert_eq!(committed.credentials.len(), 1);
        assert_eq!(
            fixture
                .store
                .get(
                    &committed.credentials[0].secret,
                    &secret_access(fixture.owner, "auth-credential-read"),
                )
                .await
                .unwrap()
                .expose_secret(),
            b"session-cookie"
        );
        let stored = fixture
            .sessions
            .find_auth_session(fixture.owner, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, committed.session);
        let account = fixture
            .accounts
            .find_provider_account(fixture.owner, fixture.account)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.auth_state, AuthState::Authenticated);
        assert_eq!(
            account.credential_refs,
            [committed.credentials[0].secret.id]
        );
    }

    #[tokio::test]
    async fn rejected_credentials_fail_the_session_without_storing_plaintext() {
        let fixture = fixture_with_validation(true, false, None, false).await;
        let started = begin_session(&fixture, "auth-rejected-start").await;
        let error = fixture
            .service
            .submit_credentials(
                &fixture.store,
                AuthSessionCredentialRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    bundle: credential_bundle(b"rejected-cookie"),
                    access: secret_access(fixture.owner, "auth-rejected-submit"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthSessionServiceError::Credential(CredentialProvisionError::CredentialRejected)
        ));
        let stored = fixture
            .sessions
            .find_auth_session(fixture.owner, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, AuthState::AuthFailed);
        assert_eq!(stored.revision, 4);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn superseded_network_validation_cannot_commit_credentials() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fixture = fixture_with_validation(
            true,
            true,
            Some((Arc::clone(&entered), Arc::clone(&release))),
            false,
        )
        .await;
        let first = begin_session(&fixture, "auth-race-first").await;
        let service = fixture.service.clone();
        let store = fixture.store.clone();
        let owner = fixture.owner;
        let account = fixture.account;
        let first_session_id = first.session.id;
        let submission = tokio::spawn(async move {
            service
                .submit_credentials(
                    &store,
                    AuthSessionCredentialRequest {
                        owner_user_id: owner,
                        provider_account_id: account,
                        session_id: first_session_id,
                        bundle: credential_bundle(b"stale-cookie"),
                        access: secret_access(owner, "auth-race-submit"),
                    },
                )
                .await
        });
        entered.notified().await;
        let second = begin_session(&fixture, "auth-race-second").await;
        release.notify_one();
        let error = submission.await.unwrap().unwrap_err();

        assert!(matches!(
            error,
            AuthSessionServiceError::RevisionConflict(id) if id == first.session.id
        ));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap(),
            0
        );
        let latest = fixture
            .sessions
            .find_latest_account_auth_session(fixture.owner, fixture.account)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, second.session.id);
        assert_eq!(
            latest.state,
            AuthState::WaitingUser(WaitingUserState::QrScan)
        );
    }

    #[tokio::test]
    async fn external_oauth_callback_is_claimed_once_validated_and_committed() {
        let fixture = fixture_with_validation(true, true, None, false).await;
        let now = Utc::now();
        let started = fixture
            .service
            .begin(AuthSessionStartRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                method: AuthMethod::ExternalBrowserOauth,
                created_at: now,
                expires_at: now + Duration::minutes(5),
                actor: AuditActor::User(fixture.owner),
                correlation_id: "oauth-start".to_owned(),
            })
            .await
            .unwrap();
        assert!(started.challenge.external_oauth.is_some());
        let committed = fixture
            .service
            .submit_external_oauth_callback(
                &fixture.store,
                ExternalOauthCallbackRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    callback_url: SecretString::new(
                        "https://provider.example/callback?code=one-time",
                    ),
                    access: secret_access(fixture.owner, "oauth-submit"),
                },
            )
            .await
            .unwrap();
        assert_eq!(committed.session.state, AuthState::Authenticated);
        assert_eq!(committed.credentials.len(), 2);
        let pending = fixture
            .sessions
            .find_external_oauth_pending(fixture.owner, fixture.account, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, ExternalOauthState::Succeeded);
        assert!(pending.consumed_at.is_some());
        assert_eq!(fixture.oauth_calls.load(Ordering::SeqCst), 1);

        let replay = fixture
            .service
            .submit_external_oauth_callback(
                &fixture.store,
                ExternalOauthCallbackRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    callback_url: SecretString::new(
                        "https://provider.example/callback?code=one-time",
                    ),
                    access: secret_access(fixture.owner, "oauth-replay"),
                },
            )
            .await;
        assert!(
            matches!(replay, Err(AuthSessionServiceError::RevisionConflict(id)) if id == started.session.id)
        );
        assert_eq!(fixture.oauth_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ambiguous_oauth_exchange_is_consumed_and_never_replayed() {
        let fixture = fixture_with_validation(true, true, None, true).await;
        let now = Utc::now();
        let started = fixture
            .service
            .begin(AuthSessionStartRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                method: AuthMethod::ExternalBrowserOauth,
                created_at: now,
                expires_at: now + Duration::minutes(5),
                actor: AuditActor::User(fixture.owner),
                correlation_id: "oauth-ambiguous-start".to_owned(),
            })
            .await
            .unwrap();
        let result = fixture
            .service
            .submit_external_oauth_callback(
                &fixture.store,
                ExternalOauthCallbackRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    callback_url: SecretString::new(
                        "https://provider.example/callback?code=uncertain",
                    ),
                    access: secret_access(fixture.owner, "oauth-ambiguous-submit"),
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(AuthSessionServiceError::Provider { source, .. })
                if source.kind == ProviderErrorKind::Network
        ));
        let pending = fixture
            .sessions
            .find_external_oauth_pending(fixture.owner, fixture.account, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, ExternalOauthState::Ambiguous);
        assert_eq!(fixture.oauth_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap(),
            0
        );
    }

    struct Fixture {
        database: Database,
        owner: UserId,
        account: ProviderAccountId,
        accounts: SqliteProviderAccountRepository,
        sessions: SqliteAuthSessionRepository,
        store: SqliteSecretStore,
        service: AuthSessionService<SqliteProviderAccountRepository, SqliteAuthSessionRepository>,
        oauth_calls: Arc<AtomicUsize>,
    }

    async fn fixture(echo_session_id: bool) -> Fixture {
        fixture_with_validation(echo_session_id, true, None, false).await
    }

    async fn fixture_with_validation(
        echo_session_id: bool,
        credential_valid: bool,
        validation_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
        oauth_network_failure: bool,
    ) -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'auth-flow-owner', '$argon2id$test', 'active', ?, '[]', \
              ?, ?)",
        )
        .bind(owner.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account = ProviderAccountId::new();
        let accounts = SqliteProviderAccountRepository::new(database.clone());
        accounts
            .create_provider_account(
                &ProviderAccount {
                    id: account,
                    owner_id: owner,
                    provider_id: provider_id.clone(),
                    display_name: "Primary".to_owned(),
                    tenant: None,
                    auth_state: AuthState::Idle,
                    network_profile_id: None,
                    credential_refs: Vec::new(),
                    created_at: now,
                    updated_at: now,
                },
                AuditActor::User(owner),
            )
            .await
            .unwrap();
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: "Provider Alpha".to_owned(),
            implementation_version: "1.0.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::Authentication]),
            auth_methods: BTreeSet::from([AuthMethod::QrCode, AuthMethod::ExternalBrowserOauth]),
            session_kinds: BTreeSet::from([SessionKind::Cookie, SessionKind::Composite]),
        };
        let oauth_calls = Arc::new(AtomicUsize::new(0));
        let authentication = Arc::new(TestAuthentication {
            metadata: metadata.clone(),
            echo_session_id,
            credential_valid,
            validation_gate,
            oauth_calls: Arc::clone(&oauth_calls),
            oauth_network_failure,
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication),
                ..ProviderEntry::metadata_only(metadata)
            })
            .unwrap();
        let sessions = SqliteAuthSessionRepository::new(database.clone());
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(
                SecretKeyring::new(
                    "key-a",
                    BTreeMap::from([("key-a".to_owned(), SecretKey::new([9; 32]))]),
                )
                .unwrap(),
            ),
        );
        let service =
            AuthSessionService::new(Arc::new(registry), accounts.clone(), sessions.clone());
        Fixture {
            database,
            owner,
            account,
            accounts,
            sessions,
            store,
            service,
            oauth_calls,
        }
    }

    async fn begin_session(fixture: &Fixture, correlation_id: &str) -> AuthSessionBegin {
        let now = Utc::now();
        fixture
            .service
            .begin(AuthSessionStartRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                method: AuthMethod::QrCode,
                created_at: now,
                expires_at: now + Duration::minutes(10),
                actor: AuditActor::User(fixture.owner),
                correlation_id: correlation_id.to_owned(),
            })
            .await
            .unwrap()
    }

    fn credential_bundle(value: &[u8]) -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            tenant: None,
            auth_method: AuthMethod::QrCode,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::Cookie,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(value.to_vec()),
            }],
            user_id_hint: None,
        }
    }

    fn secret_access(owner: UserId, correlation_id: &str) -> SecretAccess {
        SecretAccess {
            actor: SecretActor::User(owner),
            correlation_id: correlation_id.to_owned(),
            reason: "complete authentication session".to_owned(),
        }
    }

    #[derive(Debug)]
    struct TestAuthentication {
        metadata: ProviderMetadata,
        echo_session_id: bool,
        credential_valid: bool,
        validation_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
        oauth_calls: Arc<AtomicUsize>,
        oauth_network_failure: bool,
    }

    impl ProviderIdentity for TestAuthentication {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl AuthenticationCapability for TestAuthentication {
        async fn begin_authentication(
            &self,
            context: &ProviderAuthContext,
            method: AuthMethod,
        ) -> ProviderResult<AuthChallenge> {
            let external_oauth =
                (method == AuthMethod::ExternalBrowserOauth).then(|| ExternalOauthAuthorization {
                    authorization_url: "https://provider.example/oauth?opaque=redacted".to_owned(),
                    callback_binding: ExternalOauthCallbackBinding::from_digests([1; 32], [2; 32]),
                });
            Ok(AuthChallenge {
                session_id: if self.echo_session_id {
                    context.auth_session_id.expect("Core session")
                } else {
                    AuthSessionId::new()
                },
                method,
                waiting_for: if external_oauth.is_some() {
                    WaitingUserState::BrowserCallback
                } else {
                    WaitingUserState::QrScan
                },
                user_action: Some("https://example.invalid/auth".to_owned()),
                expires_at: None,
                external_oauth,
            })
        }

        async fn exchange_external_oauth_callback(
            &self,
            _context: &ProviderAuthContext,
            callback_url: SecretString,
            binding: ExternalOauthCallbackBinding,
        ) -> ProviderResult<CredentialReplacement> {
            self.oauth_calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                callback_url
                    .expose_secret()
                    .starts_with("https://provider.example/")
            );
            assert!(binding.validate());
            if self.oauth_network_failure {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "synthetic ambiguous exchange",
                ));
            }
            Ok(CredentialReplacement {
                session_kind: SessionKind::Composite,
                fields: vec![
                    CredentialField {
                        purpose: SecretPurpose::ProviderAccessToken,
                        value: SecretValue::new(b"oauth-token".to_vec()),
                    },
                    CredentialField {
                        purpose: SecretPurpose::ProviderCompositeSession,
                        value: SecretValue::new(b"oauth-crypto".to_vec()),
                    },
                ],
            })
        }

        async fn validate_credential(
            &self,
            context: &ProviderAuthContext,
            credential: &CredentialBundle,
        ) -> ProviderResult<CredentialValidation> {
            assert!(context.auth_session_id.is_some());
            if let Some((entered, release)) = &self.validation_gate {
                entered.notify_one();
                release.notified().await;
            }
            Ok(CredentialValidation::accepted(SessionStatus {
                valid: self.credential_valid,
                kind: credential.session_kind,
                expires_at: None,
                account_hint: None,
            }))
        }

        async fn validate_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<SessionStatus> {
            Ok(SessionStatus {
                valid: true,
                kind: SessionKind::Cookie,
                expires_at: None,
                account_hint: None,
            })
        }
    }
}
