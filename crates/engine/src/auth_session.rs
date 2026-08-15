use std::sync::Arc;

use asterism_domain::{
    AuditActor, AuthMethod, AuthSession, AuthSessionError, AuthSessionId, AuthState,
    ExternalOauthPending, ExternalOauthPendingCreate, ExternalOauthState, HumanRequiredReason,
    ProviderAccountId, ProviderId, Timestamp, UserId,
};
use asterism_provider_api::{
    AuthChallenge, ExternalOauthCallbackBinding, ProviderAuthContext, ProviderError,
    ProviderErrorKind, ProviderInteractiveAuthPollOutcome, ProviderRegistry,
    ResolvedProviderInteractiveAuthContinuation, SessionStatus,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, ProviderCredential, SecretAccess, SecretActor,
    SecretStoreError, SecretString,
};
use asterism_storage::{
    AuthSessionRepository, AuthenticatedCredentialRepository, InteractiveAuthAbortRequest,
    InteractiveAuthCandidateFailureRequest, InteractiveAuthContinuationAttachRequest,
    InteractiveAuthContinuationMutationOutcome, InteractiveAuthContinuationRepositoryFactory,
    InteractiveAuthCredentialCommitOutcome, InteractiveAuthCredentialCommitRequest,
    InteractiveAuthCredentialRepository, InteractiveAuthPollAuthenticateRequest,
    InteractiveAuthPollClaim, InteractiveAuthPollClaimOutcome, InteractiveAuthPollClaimRequest,
    InteractiveAuthPollRotateRequest, InteractiveAuthPollTerminalRequest,
    InteractiveAuthTerminalState, ProtocolObservationRepository, ProviderAccountRepository,
    StorageError,
};
use chrono::{Duration, Utc};

use crate::credential::{CredentialProvisionError, validate_candidate};
use crate::protocol_observation::{
    ProviderProtocolObservationRecordError, record_provider_protocol_observation,
};

#[derive(Clone)]
pub struct AuthSessionService<A, S> {
    registry: Arc<ProviderRegistry>,
    accounts: A,
    sessions: S,
    protocol_observations: Option<Arc<dyn ProtocolObservationRepository>>,
    interactive_auth_continuations: Option<Arc<dyn InteractiveAuthContinuationRepositoryFactory>>,
}

impl<A, S> AuthSessionService<A, S> {
    pub const fn new(registry: Arc<ProviderRegistry>, accounts: A, sessions: S) -> Self {
        Self {
            registry,
            accounts,
            sessions,
            protocol_observations: None,
            interactive_auth_continuations: None,
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

    #[must_use]
    pub fn with_interactive_auth_continuations(
        mut self,
        continuations: Arc<dyn InteractiveAuthContinuationRepositoryFactory>,
    ) -> Self {
        self.interactive_auth_continuations = Some(continuations);
        self
    }
}

impl<A, S> std::fmt::Debug for AuthSessionService<A, S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthSessionService")
            .field("registry", &self.registry)
            .field("accounts", &"configured")
            .field("sessions", &"configured")
            .field(
                "protocol_observations",
                &self.protocol_observations.is_some(),
            )
            .field(
                "interactive_auth_continuations",
                &self.interactive_auth_continuations.is_some(),
            )
            .finish()
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
        let durable_interactive = method == AuthMethod::QrCode
            && authentication.supports_durable_interactive_authentication();
        if durable_interactive && self.interactive_auth_continuations.is_none() {
            return Err(AuthSessionServiceError::InteractiveAuthenticationUnavailable);
        }
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
        let provider_begin = if durable_interactive {
            authentication
                .begin_interactive_authentication(&context, method)
                .await
                .and_then(|begin| {
                    begin.validate()?;
                    Ok((begin.challenge, Some(begin.continuation)))
                })
        } else {
            authentication
                .begin_authentication(&context, method)
                .await
                .map(|challenge| (challenge, None))
        };
        let (challenge, interactive_continuation) = match provider_begin {
            Ok(begin) => begin,
            Err(error) => {
                transition_provider_failure(
                    &self.sessions,
                    &mut session,
                    &error,
                    actor,
                    &correlation_id,
                )
                .await?;
                self.record_protocol_observation(
                    &provider_id,
                    session.id,
                    "begin",
                    &correlation_id,
                    &error,
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
        let waiting_at = Utc::now();
        session.transition(AuthState::WaitingUser(challenge.waiting_for), waiting_at)?;
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
        } else if let Some(continuation) = interactive_continuation {
            let continuation_digest = continuation.continuation_digest();
            let ttl_seconds = continuation.ttl_seconds();
            let maximum_polls = continuation.maximum_polls();
            let (continuation_type, phase, value, _, _) = continuation.into_parts();
            let ttl = Duration::seconds(
                i64::try_from(ttl_seconds)
                    .map_err(|_| AuthSessionServiceError::InvalidChallenge(session.id))?,
            );
            let mut continuation_expires_at = waiting_at + ttl;
            continuation_expires_at = continuation_expires_at.min(session.expires_at);
            if let Some(challenge_expires_at) = challenge.expires_at {
                continuation_expires_at = continuation_expires_at.min(challenge_expires_at);
            }
            let access = secret_access_from_audit(
                actor,
                &correlation_id,
                "persist interactive authentication continuation",
            );
            self.interactive_auth_continuations
                .as_ref()
                .ok_or(AuthSessionServiceError::InteractiveAuthenticationUnavailable)?
                .for_provider(provider_id)
                .attach_interactive_auth_continuation(InteractiveAuthContinuationAttachRequest {
                    session: &session,
                    expected_session_revision: expected_revision,
                    provider_id: &context.provider_id,
                    continuation_type: &continuation_type,
                    continuation_digest,
                    phase: &phase,
                    value,
                    maximum_polls,
                    expires_at: continuation_expires_at,
                    attached_at: waiting_at,
                    access: &access,
                })
                .await
                .map_err(AuthSessionServiceError::CredentialStore)?;
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
        if session.method == AuthMethod::QrCode
            && let Some(factory) = &self.interactive_auth_continuations
        {
            let account = self
                .accounts
                .find_provider_account(owner_user_id, session.provider_account_id)
                .await?
                .ok_or(AuthSessionServiceError::AccountNotFound(
                    session.provider_account_id,
                ))?;
            let access = secret_access_from_audit(
                actor,
                correlation_id,
                "cancel interactive authentication continuation",
            );
            let outcome = factory
                .for_provider(account.provider_id)
                .abort_interactive_auth_continuation(InteractiveAuthAbortRequest {
                    terminal_session: &session,
                    expected_session_revision: expected_revision,
                    aborted_at: at,
                    access: &access,
                })
                .await
                .map_err(AuthSessionServiceError::CredentialStore)?;
            if !matches!(
                outcome,
                InteractiveAuthContinuationMutationOutcome::Terminal(_)
            ) {
                return Err(AuthSessionServiceError::RevisionConflict(session_id));
            }
        } else if !self
            .sessions
            .update_auth_session(&session, expected_revision, actor, correlation_id)
            .await?
        {
            return Err(AuthSessionServiceError::RevisionConflict(session_id));
        }
        Ok(session)
    }

    /// Claims and performs one Provider-native interactive authentication poll.
    /// Definite waiting results rotate the encrypted continuation; definite
    /// success first persists a terminal candidate and can therefore recover
    /// credential finalization after a crash without replaying the poll.
    ///
    /// # Errors
    ///
    /// Returns a typed state, Provider, credential or persistence failure.
    #[allow(
        clippy::too_many_lines,
        reason = "the poll coordinator keeps claim, Provider classification and every durable terminal transition in one auditable flow"
    )]
    pub async fn poll_interactive_authentication<C>(
        &self,
        credential_store: &C,
        request: InteractiveAuthPollRequest,
    ) -> Result<InteractiveAuthPollResult, AuthSessionServiceError>
    where
        C: InteractiveAuthCredentialRepository,
    {
        let InteractiveAuthPollRequest {
            owner_user_id,
            provider_account_id,
            session_id,
            access,
        } = request;
        if !access.authorizes(owner_user_id) {
            return Err(AuthSessionServiceError::Credential(
                CredentialProvisionError::Unauthorized,
            ));
        }
        audit_actor_from_access(&access)?;
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
        if !authentication.supports_durable_interactive_authentication()
            || !entry.metadata.auth_methods.contains(&AuthMethod::QrCode)
        {
            return Err(AuthSessionServiceError::InteractiveAuthenticationUnavailable);
        }
        let continuation_factory = self
            .interactive_auth_continuations
            .as_ref()
            .ok_or(AuthSessionServiceError::InteractiveAuthenticationUnavailable)?;
        let repository = continuation_factory.for_provider(account.provider_id.clone());
        let mut session = self
            .sessions
            .find_auth_session(owner_user_id, session_id)
            .await?
            .filter(|session| session.provider_account_id == provider_account_id)
            .ok_or(AuthSessionServiceError::SessionNotFound(session_id))?;
        let context = ProviderAuthContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            auth_session_id: Some(session.id),
            correlation_id: access.correlation_id.clone(),
        };
        if session.state == AuthState::ValidatingCredential {
            return self
                .finalize_interactive_auth_candidate(
                    credential_store,
                    repository.as_ref(),
                    authentication.as_ref(),
                    &context,
                    owner_user_id,
                    provider_account_id,
                    session_id,
                    access,
                )
                .await;
        }
        if !matches!(
            session.state,
            AuthState::WaitingUser(
                asterism_domain::WaitingUserState::QrScan
                    | asterism_domain::WaitingUserState::QrConfirm
            )
        ) {
            return Err(AuthSessionServiceError::InvalidSessionState(session_id));
        }
        let claimed_at = Utc::now();
        if session.is_expired_at(claimed_at) {
            let expected_revision = session.revision;
            session.transition(AuthState::Expired, claimed_at)?;
            repository
                .abort_interactive_auth_continuation(InteractiveAuthAbortRequest {
                    terminal_session: &session,
                    expected_session_revision: expected_revision,
                    aborted_at: claimed_at,
                    access: &access,
                })
                .await
                .map_err(AuthSessionServiceError::CredentialStore)?;
            return Err(AuthSessionServiceError::SessionExpired(session_id));
        }
        let claim_expires_at = (claimed_at + Duration::seconds(60)).min(session.expires_at);
        let (claim, value) = match repository
            .claim_interactive_auth_poll(InteractiveAuthPollClaimRequest {
                owner_user_id,
                provider_account_id,
                auth_session_id: session_id,
                claimed_at,
                claim_expires_at,
                access: &access,
            })
            .await
            .map_err(AuthSessionServiceError::CredentialStore)?
        {
            InteractiveAuthPollClaimOutcome::Claimed { claim, value } => (claim, value),
            InteractiveAuthPollClaimOutcome::Busy => {
                return Err(AuthSessionServiceError::InteractivePollBusy(session_id));
            }
            InteractiveAuthPollClaimOutcome::Exhausted => {
                let expected_revision = session.revision;
                session.transition(AuthState::AuthFailed, claimed_at)?;
                repository
                    .abort_interactive_auth_continuation(InteractiveAuthAbortRequest {
                        terminal_session: &session,
                        expected_session_revision: expected_revision,
                        aborted_at: claimed_at,
                        access: &access,
                    })
                    .await
                    .map_err(AuthSessionServiceError::CredentialStore)?;
                return Err(AuthSessionServiceError::InteractivePollExhausted(
                    session_id,
                ));
            }
            InteractiveAuthPollClaimOutcome::Unavailable => {
                return Err(AuthSessionServiceError::RevisionConflict(session_id));
            }
        };
        let provider_outcome = authentication
            .poll_interactive_authentication(
                &context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &claim.continuation.continuation_type,
                    continuation_digest: claim.continuation.continuation_digest,
                    phase: &claim.continuation.phase,
                    revision: claim.continuation.revision,
                    poll_sequence: claim.poll_sequence,
                    value: &value,
                },
            )
            .await;
        let outcome = match provider_outcome.and_then(|outcome| {
            outcome.validate()?;
            Ok(outcome)
        }) {
            Ok(outcome) => outcome,
            Err(error) => {
                if matches!(
                    error.kind,
                    ProviderErrorKind::RateLimited
                        | ProviderErrorKind::Network
                        | ProviderErrorKind::ProviderUnavailable
                ) {
                    if !repository
                        .release_interactive_auth_poll(&claim, Utc::now(), &access)
                        .await
                        .map_err(AuthSessionServiceError::CredentialStore)?
                    {
                        return Err(AuthSessionServiceError::RevisionConflict(session_id));
                    }
                    self.record_protocol_observation(
                        &context.provider_id,
                        session_id,
                        "interactive-poll",
                        &access.correlation_id,
                        &error,
                    )
                    .await?;
                } else {
                    self.persist_claimed_interactive_failure(
                        repository.as_ref(),
                        &claim,
                        &mut session,
                        &context,
                        &access,
                        &error,
                    )
                    .await?;
                }
                return Err(AuthSessionServiceError::Provider {
                    session_id,
                    source: error,
                });
            }
        };
        let completed_at = Utc::now();
        if session.is_expired_at(completed_at) {
            let result_digest = match &outcome {
                ProviderInteractiveAuthPollOutcome::Waiting { result_digest, .. }
                | ProviderInteractiveAuthPollOutcome::Authenticated { result_digest, .. }
                | ProviderInteractiveAuthPollOutcome::Rejected { result_digest }
                | ProviderInteractiveAuthPollOutcome::Expired { result_digest } => *result_digest,
            };
            let expected_session_revision = session.revision;
            session.transition(AuthState::Expired, completed_at)?;
            let persisted = repository
                .finish_interactive_auth_terminal(InteractiveAuthPollTerminalRequest {
                    claim: &claim,
                    terminal_session: &session,
                    expected_session_revision,
                    terminal_state: InteractiveAuthTerminalState::Expired,
                    result_digest,
                    completed_at,
                    access: &access,
                })
                .await
                .map_err(AuthSessionServiceError::CredentialStore)?;
            if !matches!(
                persisted,
                InteractiveAuthContinuationMutationOutcome::Terminal(_)
            ) {
                return Err(AuthSessionServiceError::RevisionConflict(session_id));
            }
            return Err(AuthSessionServiceError::SessionExpired(session_id));
        }
        match outcome {
            ProviderInteractiveAuthPollOutcome::Waiting {
                waiting_for,
                user_action,
                continuation,
                result_digest,
            } => {
                if continuation.maximum_polls() != claim.continuation.maximum_polls {
                    let error = ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "Provider changed the interactive authentication poll budget",
                    );
                    self.persist_claimed_interactive_failure(
                        repository.as_ref(),
                        &claim,
                        &mut session,
                        &context,
                        &access,
                        &error,
                    )
                    .await?;
                    return Err(AuthSessionServiceError::Provider {
                        session_id,
                        source: error,
                    });
                }
                let continuation_digest = continuation.continuation_digest();
                let (continuation_type, phase, replacement, _, _) = continuation.into_parts();
                let expected_revision = session.revision;
                session.transition(AuthState::WaitingUser(waiting_for), completed_at)?;
                let persisted = repository
                    .rotate_interactive_auth_continuation(InteractiveAuthPollRotateRequest {
                        claim: &claim,
                        waiting_session: &session,
                        expected_session_revision: expected_revision,
                        continuation_type: &continuation_type,
                        continuation_digest,
                        phase: &phase,
                        replacement,
                        result_digest,
                        completed_at,
                        access: &access,
                    })
                    .await
                    .map_err(AuthSessionServiceError::CredentialStore)?;
                if !matches!(
                    persisted,
                    InteractiveAuthContinuationMutationOutcome::Rotated(_)
                ) {
                    return Err(AuthSessionServiceError::RevisionConflict(session_id));
                }
                let challenge = AuthChallenge {
                    session_id,
                    method: AuthMethod::QrCode,
                    waiting_for,
                    user_action,
                    expires_at: Some(claim.continuation.expires_at),
                    external_oauth: None,
                };
                if !valid_challenge(&session, &challenge) {
                    return Err(AuthSessionServiceError::InvalidChallenge(session_id));
                }
                Ok(InteractiveAuthPollResult::Waiting { session, challenge })
            }
            ProviderInteractiveAuthPollOutcome::Authenticated {
                continuation,
                result_digest,
            } => {
                if continuation.maximum_polls() != claim.continuation.maximum_polls {
                    let error = ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "Provider changed the interactive authentication poll budget",
                    );
                    self.persist_claimed_interactive_failure(
                        repository.as_ref(),
                        &claim,
                        &mut session,
                        &context,
                        &access,
                        &error,
                    )
                    .await?;
                    return Err(AuthSessionServiceError::Provider {
                        session_id,
                        source: error,
                    });
                }
                let continuation_digest = continuation.continuation_digest();
                let (continuation_type, phase, replacement, _, _) = continuation.into_parts();
                let expected_revision = session.revision;
                session.transition(AuthState::ValidatingCredential, completed_at)?;
                let persisted = repository
                    .persist_interactive_auth_candidate(InteractiveAuthPollAuthenticateRequest {
                        claim: &claim,
                        validating_session: &session,
                        expected_session_revision: expected_revision,
                        continuation_type: &continuation_type,
                        continuation_digest,
                        phase: &phase,
                        replacement,
                        result_digest,
                        completed_at,
                        access: &access,
                    })
                    .await
                    .map_err(AuthSessionServiceError::CredentialStore)?;
                if !matches!(
                    persisted,
                    InteractiveAuthContinuationMutationOutcome::AuthenticatedCandidate(_)
                ) {
                    return Err(AuthSessionServiceError::RevisionConflict(session_id));
                }
                self.finalize_interactive_auth_candidate(
                    credential_store,
                    repository.as_ref(),
                    authentication.as_ref(),
                    &context,
                    owner_user_id,
                    provider_account_id,
                    session_id,
                    access,
                )
                .await
            }
            ProviderInteractiveAuthPollOutcome::Rejected { result_digest } => {
                let expected_revision = session.revision;
                session.transition(AuthState::AuthFailed, completed_at)?;
                let persisted = repository
                    .finish_interactive_auth_terminal(InteractiveAuthPollTerminalRequest {
                        claim: &claim,
                        terminal_session: &session,
                        expected_session_revision: expected_revision,
                        terminal_state: InteractiveAuthTerminalState::Rejected,
                        result_digest,
                        completed_at,
                        access: &access,
                    })
                    .await
                    .map_err(AuthSessionServiceError::CredentialStore)?;
                if !matches!(
                    persisted,
                    InteractiveAuthContinuationMutationOutcome::Terminal(_)
                ) {
                    return Err(AuthSessionServiceError::RevisionConflict(session_id));
                }
                Ok(InteractiveAuthPollResult::Terminal(session))
            }
            ProviderInteractiveAuthPollOutcome::Expired { result_digest } => {
                let expected_revision = session.revision;
                session.transition(AuthState::Expired, completed_at)?;
                let persisted = repository
                    .finish_interactive_auth_terminal(InteractiveAuthPollTerminalRequest {
                        claim: &claim,
                        terminal_session: &session,
                        expected_session_revision: expected_revision,
                        terminal_state: InteractiveAuthTerminalState::Expired,
                        result_digest,
                        completed_at,
                        access: &access,
                    })
                    .await
                    .map_err(AuthSessionServiceError::CredentialStore)?;
                if !matches!(
                    persisted,
                    InteractiveAuthContinuationMutationOutcome::Terminal(_)
                ) {
                    return Err(AuthSessionServiceError::RevisionConflict(session_id));
                }
                Ok(InteractiveAuthPollResult::Terminal(session))
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "candidate finalization retains every owner, account, session and repository binding explicitly"
    )]
    async fn finalize_interactive_auth_candidate<C>(
        &self,
        credential_store: &C,
        repository: &dyn asterism_storage::InteractiveAuthContinuationRepository,
        authentication: &dyn asterism_provider_api::AuthenticationCapability,
        context: &ProviderAuthContext,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        session_id: AuthSessionId,
        access: SecretAccess,
    ) -> Result<InteractiveAuthPollResult, AuthSessionServiceError>
    where
        C: InteractiveAuthCredentialRepository,
    {
        let resolved = repository
            .resolve_interactive_auth_candidate(
                owner_user_id,
                provider_account_id,
                session_id,
                &access,
            )
            .await
            .map_err(AuthSessionServiceError::CredentialStore)?
            .ok_or(AuthSessionServiceError::InvalidSessionState(session_id))?;
        let bundle = authentication
            .finalize_interactive_authentication(
                context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &resolved.continuation.continuation_type,
                    continuation_digest: resolved.continuation.continuation_digest,
                    phase: &resolved.continuation.phase,
                    revision: resolved.continuation.revision,
                    poll_sequence: resolved.continuation.poll_count,
                    value: &resolved.value,
                },
            )
            .await
            .map_err(CredentialProvisionError::Provider)
            .and_then(|bundle| {
                if bundle.auth_method == AuthMethod::QrCode
                    && bundle.provider_id == context.provider_id
                {
                    Ok(bundle)
                } else {
                    Err(CredentialProvisionError::AccountMismatch)
                }
            });
        let validated = match bundle {
            Ok(bundle) => {
                validate_candidate(
                    self.registry.as_ref(),
                    &self.accounts,
                    owner_user_id,
                    provider_account_id,
                    bundle,
                    Some(session_id),
                    &access,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let (bundle, status) = match validated {
            Ok(validated) => validated,
            Err(error) => {
                let retryable = matches!(
                    &error,
                    CredentialProvisionError::Provider(provider_error)
                        if matches!(
                            provider_error.kind,
                            ProviderErrorKind::RateLimited
                                | ProviderErrorKind::Network
                                | ProviderErrorKind::ProviderUnavailable
                        )
                ) || matches!(
                    &error,
                    CredentialProvisionError::Storage(_) | CredentialProvisionError::SecretStore(_)
                );
                if !retryable {
                    let failed_at = Utc::now();
                    let mut terminal_session = resolved.session.clone();
                    let expected_session_revision = terminal_session.revision;
                    terminal_session.transition(credential_failure_state(&error), failed_at)?;
                    let persisted = repository
                        .finish_interactive_auth_candidate_failure(
                            InteractiveAuthCandidateFailureRequest {
                                continuation: &resolved.continuation,
                                terminal_session: &terminal_session,
                                expected_session_revision,
                                failed_at,
                                access: &access,
                            },
                        )
                        .await
                        .map_err(AuthSessionServiceError::CredentialStore)?;
                    if !matches!(
                        persisted,
                        InteractiveAuthContinuationMutationOutcome::Terminal(_)
                    ) {
                        return Err(AuthSessionServiceError::RevisionConflict(session_id));
                    }
                }
                self.record_credential_protocol_observation(
                    &context.provider_id,
                    session_id,
                    "interactive-credential-validation",
                    &access.correlation_id,
                    &error,
                )
                .await?;
                return Err(AuthSessionServiceError::Credential(error));
            }
        };
        let terminal_result_digest = resolved
            .continuation
            .terminal_result_digest
            .ok_or(AuthSessionServiceError::InvalidSessionState(session_id))?;
        let mut authenticated_session = resolved.session;
        let expected_session_revision = authenticated_session.revision;
        authenticated_session
            .transition(AuthState::Authenticated, resolved.continuation.updated_at)?;
        let committed = credential_store
            .commit_interactive_auth_credentials(InteractiveAuthCredentialCommitRequest {
                owner_user_id,
                provider_account_id,
                authenticated_session: &authenticated_session,
                expected_session_revision,
                continuation: &resolved.continuation,
                terminal_result_digest,
                bundle,
                access: &access,
            })
            .await
            .map_err(AuthSessionServiceError::CredentialStore)?;
        let InteractiveAuthCredentialCommitOutcome::Committed(committed) = committed else {
            return Err(AuthSessionServiceError::RevisionConflict(session_id));
        };
        Ok(InteractiveAuthPollResult::Authenticated(
            AuthSessionCredentialCommit {
                session: committed.session,
                status,
                credentials: committed.credentials,
            },
        ))
    }

    async fn persist_claimed_interactive_failure(
        &self,
        repository: &dyn asterism_storage::InteractiveAuthContinuationRepository,
        claim: &InteractiveAuthPollClaim,
        session: &mut AuthSession,
        context: &ProviderAuthContext,
        access: &SecretAccess,
        error: &ProviderError,
    ) -> Result<(), AuthSessionServiceError> {
        let completed_at = Utc::now();
        let expected_session_revision = session.revision;
        let expired = session.is_expired_at(completed_at);
        session.transition(
            if expired {
                AuthState::Expired
            } else {
                provider_failure_state(error)
            },
            completed_at,
        )?;
        let persisted = repository
            .finish_interactive_auth_terminal(InteractiveAuthPollTerminalRequest {
                claim,
                terminal_session: session,
                expected_session_revision,
                terminal_state: if expired {
                    InteractiveAuthTerminalState::Expired
                } else {
                    InteractiveAuthTerminalState::Failed
                },
                result_digest: provider_error_digest(error),
                completed_at,
                access,
            })
            .await
            .map_err(AuthSessionServiceError::CredentialStore)?;
        if !matches!(
            persisted,
            InteractiveAuthContinuationMutationOutcome::Terminal(_)
        ) {
            return Err(AuthSessionServiceError::RevisionConflict(session.id));
        }
        self.record_protocol_observation(
            &context.provider_id,
            session.id,
            "interactive-poll",
            &access.correlation_id,
            error,
        )
        .await
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
    #[allow(clippy::too_many_lines)]
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
        if bundle.auth_method == AuthMethod::QrCode
            && self
                .registry
                .get(&bundle.provider_id)
                .and_then(|entry| entry.authentication.as_ref())
                .is_some_and(|authentication| {
                    authentication.supports_durable_interactive_authentication()
                })
        {
            return Err(AuthSessionServiceError::InvalidSessionState(session_id));
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
        let provider_id = bundle.provider_id.clone();
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
                self.finish_credential_failure(&mut session, &provider_id, &access, &error)
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
                self.record_protocol_observation(
                    &account.provider_id,
                    session_id,
                    "oauth-exchange",
                    &access.correlation_id,
                    &error,
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
            provider_id: account.provider_id.clone(),
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
                self.record_credential_protocol_observation(
                    &account.provider_id,
                    session_id,
                    "oauth-credential-validation",
                    &access.correlation_id,
                    &error,
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

    async fn record_protocol_observation(
        &self,
        provider_id: &ProviderId,
        session_id: AuthSessionId,
        stage: &str,
        correlation_id: &str,
        error: &ProviderError,
    ) -> Result<(), AuthSessionServiceError> {
        let occurrence_scope = format!("auth-session:{session_id}:{stage}:{correlation_id}");
        record_provider_protocol_observation(
            self.protocol_observations.as_deref(),
            provider_id,
            None,
            &occurrence_scope,
            error,
            Utc::now(),
        )
        .await
        .map_err(|error| match error {
            ProviderProtocolObservationRecordError::Invalid => {
                AuthSessionServiceError::InvalidProtocolObservation
            }
            ProviderProtocolObservationRecordError::Storage(error) => {
                AuthSessionServiceError::Storage(error)
            }
        })
    }

    async fn record_credential_protocol_observation(
        &self,
        provider_id: &ProviderId,
        session_id: AuthSessionId,
        stage: &str,
        correlation_id: &str,
        error: &CredentialProvisionError,
    ) -> Result<(), AuthSessionServiceError> {
        if let CredentialProvisionError::Provider(provider_error) = error {
            self.record_protocol_observation(
                provider_id,
                session_id,
                stage,
                correlation_id,
                provider_error,
            )
            .await?;
        }
        Ok(())
    }

    async fn finish_credential_failure(
        &self,
        session: &mut AuthSession,
        provider_id: &ProviderId,
        access: &SecretAccess,
        error: &CredentialProvisionError,
    ) -> Result<(), AuthSessionServiceError> {
        transition_once(
            &self.sessions,
            session,
            credential_failure_state(error),
            audit_actor_from_access(access)?,
            &access.correlation_id,
        )
        .await?;
        self.record_credential_protocol_observation(
            provider_id,
            session.id,
            "credential-validation",
            &access.correlation_id,
            error,
        )
        .await
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
pub struct InteractiveAuthPollRequest {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub session_id: AuthSessionId,
    pub access: SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InteractiveAuthPollResult {
    Waiting {
        session: AuthSession,
        challenge: AuthChallenge,
    },
    Authenticated(AuthSessionCredentialCommit),
    Terminal(AuthSession),
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
    #[error("encrypted interactive authentication continuation storage is unavailable")]
    InteractiveAuthenticationUnavailable,
    #[error("interactive authentication poll for session `{0}` is already in progress")]
    InteractivePollBusy(AuthSessionId),
    #[error("interactive authentication poll budget for session `{0}` is exhausted")]
    InteractivePollExhausted(AuthSessionId),
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
    #[error("Provider supplied an invalid protocol observation")]
    InvalidProtocolObservation,
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
    let method_state_valid = if challenge.method == AuthMethod::QrCode {
        matches!(
            challenge.waiting_for,
            asterism_domain::WaitingUserState::QrScan
                | asterism_domain::WaitingUserState::QrConfirm
        ) && challenge.external_oauth.is_none()
    } else {
        true
    };
    challenge.session_id == session.id
        && challenge.method == session.method
        && method_state_valid
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

fn provider_error_digest(error: &ProviderError) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"asterism.interactive-auth-provider-error.v1\0");
    hasher.update(format!("{:?}", error.kind).as_bytes());
    hasher.update([0]);
    hasher.update(error.message.as_bytes());
    if let Some(reason) = error.human_required_reason {
        hasher.update([0]);
        hasher.update(format!("{reason:?}").as_bytes());
    }
    hasher.finalize().into()
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

fn secret_access_from_audit(actor: AuditActor, correlation_id: &str, reason: &str) -> SecretAccess {
    SecretAccess {
        actor: match actor {
            AuditActor::User(user_id) => SecretActor::User(user_id),
            AuditActor::ServiceToken(token_id) => SecretActor::ServiceToken(token_id),
        },
        correlation_id: correlation_id.to_owned(),
        reason: reason.to_owned(),
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
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use asterism_domain::{
        ProtocolObservationKind, ProtocolSurface, ProviderAccount, Role, SessionKind,
        WaitingUserState,
    };
    use asterism_provider_api::{
        AuthenticationCapability, CredentialReplacement, CredentialValidation,
        ExternalOauthAuthorization, ExternalOauthCallbackBinding, ProviderCapability,
        ProviderContext, ProviderEntry, ProviderIdentity, ProviderInteractiveAuthBegin,
        ProviderInteractiveAuthContinuation, ProviderInteractiveAuthPollOutcome, ProviderMetadata,
        ProviderResult, ResolvedProviderInteractiveAuthContinuation, SessionStatus,
        VerificationLevel,
    };
    use asterism_secrets::{
        CredentialAcquisition, CredentialBundle, CredentialField, SecretAccess, SecretActor,
        SecretKey, SecretPurpose, SecretStore, SecretValue,
    };
    use asterism_storage::{
        Database, SecretKeyring, SqliteAuthSessionRepository, SqliteProtocolObservationRepository,
        SqliteProviderAccountRepository, SqliteSecretStore,
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
    #[allow(clippy::too_many_lines)]
    async fn durable_interactive_auth_recovers_terminal_candidate_without_repolling() {
        let fixture = fixture(true).await;
        let authentication = Arc::new(DurableTestAuthentication {
            metadata: fixture.authentication.metadata.clone(),
            polls: AtomicUsize::new(0),
            fail_finalize_once: AtomicBool::new(true),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication.clone()),
                ..ProviderEntry::metadata_only(authentication.metadata.clone())
            })
            .unwrap();
        let service = AuthSessionService::new(
            Arc::new(registry),
            fixture.accounts.clone(),
            fixture.sessions.clone(),
        )
        .with_interactive_auth_continuations(Arc::new(fixture.store.clone()));
        let now = Utc::now();
        let started = service
            .begin(AuthSessionStartRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                method: AuthMethod::QrCode,
                created_at: now,
                expires_at: now + Duration::seconds(2),
                actor: AuditActor::User(fixture.owner),
                correlation_id: "durable-qr-begin".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            started.session.state,
            AuthState::WaitingUser(WaitingUserState::QrScan)
        );
        let waiting = service
            .poll_interactive_authentication(
                &fixture.store,
                InteractiveAuthPollRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    access: secret_access(fixture.owner, "durable-qr-poll-one"),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            waiting,
            InteractiveAuthPollResult::Waiting { ref session, ref challenge }
                if session.state == AuthState::WaitingUser(WaitingUserState::QrConfirm)
                    && challenge.waiting_for == WaitingUserState::QrConfirm
        ));
        let interrupted = service
            .poll_interactive_authentication(
                &fixture.store,
                InteractiveAuthPollRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    access: secret_access(fixture.owner, "durable-qr-poll-two"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            interrupted,
            AuthSessionServiceError::Credential(CredentialProvisionError::Provider(ref error))
                if error.kind == ProviderErrorKind::Network
        ));
        let validating = fixture
            .sessions
            .find_auth_session(fixture.owner, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(validating.state, AuthState::ValidatingCredential);
        assert_eq!(authentication.polls.load(Ordering::SeqCst), 2);
        tokio::time::sleep(std::time::Duration::from_millis(2_100)).await;

        let recovered = service
            .poll_interactive_authentication(
                &fixture.store,
                InteractiveAuthPollRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    access: secret_access(fixture.owner, "durable-qr-recover"),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            InteractiveAuthPollResult::Authenticated(ref commit)
                if commit.session.state == AuthState::Authenticated
                    && commit.credentials.len() == 1
        ));
        assert_eq!(authentication.polls.load(Ordering::SeqCst), 2);
        let continuation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM interactive_auth_continuations WHERE auth_session_id = ?",
        )
        .bind(started.session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(continuation_count, 0);
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
    async fn begin_drift_is_observed_after_session_failure_is_persisted() {
        let fixture = fixture(true).await;
        fixture
            .authentication
            .begin_protocol_drift
            .store(true, Ordering::Relaxed);
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
                correlation_id: "auth-begin-drift".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthSessionServiceError::Provider { source, .. }
                if source.kind == ProviderErrorKind::ProtocolDrift
        ));
        let stored = fixture
            .sessions
            .find_latest_account_auth_session(fixture.owner, fixture.account)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, AuthState::AuthFailed);
        assert_observation(&fixture, "authentication", "endpoint_version_drift").await;
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
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM answer_bootstrap_harvests")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap(),
            1
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
    async fn credential_drift_is_observed_after_session_failure_without_secret_commit() {
        let fixture = fixture(true).await;
        let started = begin_session(&fixture, "auth-credential-drift-start").await;
        fixture
            .authentication
            .credential_protocol_drift
            .store(true, Ordering::Relaxed);
        let error = fixture
            .service
            .submit_credentials(
                &fixture.store,
                AuthSessionCredentialRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    bundle: credential_bundle(b"uncommitted-cookie"),
                    access: secret_access(fixture.owner, "auth-credential-drift-submit"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthSessionServiceError::Credential(CredentialProvisionError::Provider(error))
                if error.kind == ProviderErrorKind::ProtocolDrift
        ));
        let stored = fixture
            .sessions
            .find_auth_session(fixture.owner, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, AuthState::AuthFailed);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap(),
            0
        );
        assert_observation(&fixture, "authentication", "field_drift").await;
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

    #[tokio::test]
    async fn oauth_drift_is_observed_after_one_shot_pending_is_finished() {
        let fixture = fixture(true).await;
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
                correlation_id: "oauth-drift-start".to_owned(),
            })
            .await
            .unwrap();
        fixture
            .authentication
            .oauth_protocol_drift
            .store(true, Ordering::Relaxed);
        let error = fixture
            .service
            .submit_external_oauth_callback(
                &fixture.store,
                ExternalOauthCallbackRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    session_id: started.session.id,
                    callback_url: SecretString::new("https://provider.example/callback?code=drift"),
                    access: secret_access(fixture.owner, "oauth-drift-submit"),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthSessionServiceError::Provider { source, .. }
                if source.kind == ProviderErrorKind::ProtocolDrift
        ));
        let pending = fixture
            .sessions
            .find_external_oauth_pending(fixture.owner, fixture.account, started.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, ExternalOauthState::Failed);
        assert_eq!(fixture.oauth_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap(),
            0
        );
        assert_observation(&fixture, "authentication", "unknown_result_shape").await;
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
        authentication: Arc<TestAuthentication>,
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
            begin_protocol_drift: AtomicBool::new(false),
            credential_protocol_drift: AtomicBool::new(false),
            oauth_protocol_drift: AtomicBool::new(false),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication.clone()),
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
            AuthSessionService::new(Arc::new(registry), accounts.clone(), sessions.clone())
                .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
                    database.clone(),
                )));
        Fixture {
            database,
            owner,
            account,
            accounts,
            sessions,
            store,
            service,
            oauth_calls,
            authentication,
        }
    }

    async fn assert_observation(fixture: &Fixture, surface: &str, kind: &str) {
        let observation: (String, String, Option<String>) =
            sqlx::query_as("SELECT surface, kind, last_execution_id FROM protocol_observations")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(observation, (surface.to_owned(), kind.to_owned(), None));
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
        begin_protocol_drift: AtomicBool,
        credential_protocol_drift: AtomicBool,
        oauth_protocol_drift: AtomicBool,
    }

    #[derive(Debug)]
    struct DurableTestAuthentication {
        metadata: ProviderMetadata,
        polls: AtomicUsize,
        fail_finalize_once: AtomicBool,
    }

    impl ProviderIdentity for DurableTestAuthentication {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl AuthenticationCapability for DurableTestAuthentication {
        fn supports_durable_interactive_authentication(&self) -> bool {
            true
        }

        async fn begin_authentication(
            &self,
            _context: &ProviderAuthContext,
            _method: AuthMethod,
        ) -> ProviderResult<AuthChallenge> {
            Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "durable test Provider requires the continuation path",
            ))
        }

        async fn begin_interactive_authentication(
            &self,
            context: &ProviderAuthContext,
            method: AuthMethod,
        ) -> ProviderResult<ProviderInteractiveAuthBegin> {
            let session_id = context.auth_session_id.expect("Core auth session");
            Ok(ProviderInteractiveAuthBegin {
                challenge: AuthChallenge {
                    session_id,
                    method,
                    waiting_for: WaitingUserState::QrScan,
                    user_action: Some("https://provider.example/qr/initial".to_owned()),
                    expires_at: None,
                    external_oauth: None,
                },
                continuation: ProviderInteractiveAuthContinuation::try_new(
                    &context.provider_id,
                    "provider-alpha.qr.v1",
                    "provider-alpha.qr-scan",
                    SecretValue::new(b"durable-initial".to_vec()),
                    300,
                    4,
                )?,
            })
        }

        async fn poll_interactive_authentication(
            &self,
            context: &ProviderAuthContext,
            continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
        ) -> ProviderResult<ProviderInteractiveAuthPollOutcome> {
            let poll = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
            match poll {
                1 => {
                    assert_eq!(continuation.value.expose_secret(), b"durable-initial");
                    Ok(ProviderInteractiveAuthPollOutcome::Waiting {
                        waiting_for: WaitingUserState::QrConfirm,
                        user_action: None,
                        continuation: ProviderInteractiveAuthContinuation::try_new(
                            &context.provider_id,
                            "provider-alpha.qr.v1",
                            "provider-alpha.qr-confirm",
                            SecretValue::new(b"durable-waiting".to_vec()),
                            300,
                            4,
                        )?,
                        result_digest: [1; 32],
                    })
                }
                2 => {
                    assert_eq!(continuation.value.expose_secret(), b"durable-waiting");
                    Ok(ProviderInteractiveAuthPollOutcome::Authenticated {
                        continuation: ProviderInteractiveAuthContinuation::try_new(
                            &context.provider_id,
                            "provider-alpha.qr-terminal.v1",
                            "provider-alpha.authenticated",
                            SecretValue::new(b"_uid=durable-user".to_vec()),
                            300,
                            4,
                        )?,
                        result_digest: [2; 32],
                    })
                }
                _ => panic!("authenticated QR status must not be polled again"),
            }
        }

        async fn finalize_interactive_authentication(
            &self,
            context: &ProviderAuthContext,
            continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
        ) -> ProviderResult<CredentialBundle> {
            if self.fail_finalize_once.swap(false, Ordering::SeqCst) {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "synthetic credential validation interruption",
                ));
            }
            assert_eq!(continuation.value.expose_secret(), b"_uid=durable-user");
            Ok(CredentialBundle {
                provider_id: context.provider_id.clone(),
                tenant: None,
                auth_method: AuthMethod::QrCode,
                acquired_via: CredentialAcquisition::NativeProviderLogin,
                captured_at: Utc::now(),
                expires_at: None,
                session_kind: SessionKind::Cookie,
                fields: vec![CredentialField {
                    purpose: SecretPurpose::ProviderCookie,
                    value: SecretValue::new(b"_uid=durable-user".to_vec()),
                }],
                user_id_hint: Some("durable-user".to_owned()),
            })
        }

        async fn validate_credential(
            &self,
            _context: &ProviderAuthContext,
            credential: &CredentialBundle,
        ) -> ProviderResult<CredentialValidation> {
            Ok(CredentialValidation::accepted(SessionStatus {
                valid: true,
                kind: credential.session_kind,
                expires_at: None,
                account_hint: Some("durable-user".to_owned()),
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
                account_hint: Some("durable-user".to_owned()),
            })
        }
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
            if self.begin_protocol_drift.load(Ordering::Relaxed) {
                return Err(protocol_drift(
                    ProtocolObservationKind::EndpointVersionDrift,
                    serde_json::json!({"document": "auth_challenge", "version": 2}),
                ));
            }
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
            if self.oauth_protocol_drift.load(Ordering::Relaxed) {
                return Err(protocol_drift(
                    ProtocolObservationKind::UnknownResultShape,
                    serde_json::json!({"document": "oauth_exchange", "result": "object"}),
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
            if self.credential_protocol_drift.load(Ordering::Relaxed) {
                return Err(protocol_drift(
                    ProtocolObservationKind::FieldDrift,
                    serde_json::json!({"document": "session_status", "missing": "valid"}),
                ));
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

    fn protocol_drift(kind: ProtocolObservationKind, shape: serde_json::Value) -> ProviderError {
        ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "authentication shape changed",
        )
        .try_with_protocol_observation(ProtocolSurface::Authentication, kind, shape)
        .unwrap()
    }
}
