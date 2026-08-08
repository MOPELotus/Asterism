use std::sync::Arc;

use asterism_domain::{
    AuditActor, AuthMethod, AuthSession, AuthSessionError, AuthSessionId, AuthState,
    HumanRequiredReason, ProviderAccountId, ProviderId, Timestamp, UserId,
};
use asterism_provider_api::{
    AuthChallenge, ProviderAuthContext, ProviderError, ProviderErrorKind, ProviderRegistry,
};
use asterism_storage::{AuthSessionRepository, ProviderAccountRepository, StorageError};
use chrono::Utc;

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
        transition_once(
            &self.sessions,
            &mut session,
            AuthState::WaitingUser(challenge.waiting_for),
            actor,
            &correlation_id,
        )
        .await?;
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
    Domain(#[from] AuthSessionError),
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
}

async fn transition_provider_failure<S: AuthSessionRepository>(
    sessions: &S,
    session: &mut AuthSession,
    error: &ProviderError,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<(), AuthSessionServiceError> {
    let next = match error.kind {
        ProviderErrorKind::RateLimited
        | ProviderErrorKind::Network
        | ProviderErrorKind::ProviderUnavailable => AuthState::ProviderUnavailable,
        ProviderErrorKind::HumanRequired => {
            AuthState::HumanRequired(HumanRequiredReason::ManualIntervention)
        }
        ProviderErrorKind::RemoteChanged => AuthState::ClientUpdateRequired,
        ProviderErrorKind::Authentication
        | ProviderErrorKind::Authorization
        | ProviderErrorKind::ProtocolDrift
        | ProviderErrorKind::UnsupportedTask
        | ProviderErrorKind::InvalidResponse
        | ProviderErrorKind::Internal => AuthState::AuthFailed,
    };
    transition_once(sessions, session, next, actor, correlation_id).await
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
    use std::collections::BTreeSet;

    use asterism_domain::{ProviderAccount, Role, SessionKind, WaitingUserState};
    use asterism_provider_api::{
        AuthenticationCapability, ProviderCapability, ProviderContext, ProviderEntry,
        ProviderIdentity, ProviderMetadata, ProviderResult, SessionStatus, VerificationLevel,
    };
    use asterism_secrets::CredentialBundle;
    use asterism_storage::{
        Database, SqliteAuthSessionRepository, SqliteProviderAccountRepository,
    };
    use async_trait::async_trait;
    use chrono::Duration;

    use super::*;

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

    struct Fixture {
        owner: UserId,
        account: ProviderAccountId,
        sessions: SqliteAuthSessionRepository,
        service: AuthSessionService<SqliteProviderAccountRepository, SqliteAuthSessionRepository>,
    }

    async fn fixture(echo_session_id: bool) -> Fixture {
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
            capabilities: BTreeSet::from([ProviderCapability::Authentication]),
            auth_methods: BTreeSet::from([AuthMethod::QrCode]),
            session_kinds: BTreeSet::from([SessionKind::Cookie]),
        };
        let authentication = Arc::new(TestAuthentication {
            metadata: metadata.clone(),
            echo_session_id,
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication),
                ..ProviderEntry::metadata_only(metadata)
            })
            .unwrap();
        let sessions = SqliteAuthSessionRepository::new(database);
        let service = AuthSessionService::new(Arc::new(registry), accounts, sessions.clone());
        Fixture {
            owner,
            account,
            sessions,
            service,
        }
    }

    #[derive(Debug)]
    struct TestAuthentication {
        metadata: ProviderMetadata,
        echo_session_id: bool,
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
            Ok(AuthChallenge {
                session_id: if self.echo_session_id {
                    context.auth_session_id.expect("Core session")
                } else {
                    AuthSessionId::new()
                },
                method,
                waiting_for: WaitingUserState::QrScan,
                user_action: Some("https://example.invalid/auth".to_owned()),
                expires_at: None,
            })
        }

        async fn validate_credential(
            &self,
            _context: &ProviderAuthContext,
            _credential: &CredentialBundle,
        ) -> ProviderResult<SessionStatus> {
            Ok(SessionStatus {
                valid: true,
                kind: SessionKind::Cookie,
                expires_at: None,
                account_hint: None,
            })
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
