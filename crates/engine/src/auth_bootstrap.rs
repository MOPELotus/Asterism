use asterism_auth::{OpaqueTokenService, TokenError};
use asterism_domain::{
    AuditActor, AuthBootstrapPurpose, AuthBootstrapSession, AuthBootstrapSessionError,
    AuthBootstrapSessionId, ProviderAccountId, ProviderId, Timestamp, UserId,
};
use asterism_secrets::SecretString;
use asterism_storage::{AuthBootstrapSessionRepository, StorageError};

#[derive(Debug)]
pub struct AuthBootstrapService<R> {
    repository: R,
    pairing_tokens: OpaqueTokenService,
    access_tokens: OpaqueTokenService,
}

impl<R> AuthBootstrapService<R> {
    /// Builds the fixed token families used by Capture pairing.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] only if an internal static prefix violates the
    /// opaque-token contract.
    pub fn new(repository: R) -> Result<Self, TokenError> {
        Ok(Self {
            repository,
            pairing_tokens: OpaqueTokenService::new("ast_pair")?,
            access_tokens: OpaqueTokenService::new("ast_boot")?,
        })
    }
}

impl<R> AuthBootstrapService<R>
where
    R: AuthBootstrapSessionRepository,
{
    /// Creates one pairing session and returns its plaintext token exactly
    /// once while persistence receives only the digest.
    ///
    /// # Errors
    ///
    /// Rejects invalid domain input or a failed atomic repository write.
    pub async fn create(
        &self,
        request: AuthBootstrapCreateRequest,
    ) -> Result<AuthBootstrapCreated, AuthBootstrapServiceError> {
        let session = AuthBootstrapSession::awaiting_claim(
            request.owner_user_id,
            request.provider_id,
            request.provider_account_id,
            request.purpose,
            request.required_recipe_version,
            request.created_at,
            request.expires_at,
        )?;
        let (pairing_token, digest) = self.pairing_tokens.generate();
        self.repository
            .create_auth_bootstrap_session(
                &session,
                &digest,
                request.actor,
                &request.correlation_id,
            )
            .await?;
        Ok(AuthBootstrapCreated {
            session,
            pairing_token,
        })
    }

    /// Consumes a valid pairing token and returns one session-scoped access
    /// token exactly once.
    ///
    /// # Errors
    ///
    /// Wrong, expired, replayed, or cancelled pairing tokens share one
    /// rejection result.
    pub async fn claim(
        &self,
        request: AuthBootstrapClaimRequest,
    ) -> Result<AuthBootstrapClaimed, AuthBootstrapServiceError> {
        let pairing_digest = self.pairing_tokens.digest(&request.pairing_token);
        let (access_token, access_digest) = self.access_tokens.generate();
        let session = self
            .repository
            .claim_auth_bootstrap_session(
                request.session_id,
                &pairing_digest,
                &access_digest,
                request.claimed_at,
                &request.correlation_id,
            )
            .await?
            .ok_or(AuthBootstrapServiceError::PairingRejected)?;
        Ok(AuthBootstrapClaimed {
            session,
            access_token,
        })
    }

    /// Cancels one owner-scoped live pairing and invalidates either token
    /// digest. An overdue session is persisted as expired instead.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, terminal, invalid, or concurrently changed
    /// sessions.
    pub async fn cancel(
        &self,
        request: AuthBootstrapCancelRequest,
    ) -> Result<AuthBootstrapSession, AuthBootstrapServiceError> {
        let mut session = self
            .repository
            .find_auth_bootstrap_session(request.owner_user_id, request.session_id)
            .await?
            .ok_or(AuthBootstrapServiceError::SessionNotFound(
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
            .update_auth_bootstrap_session_for_owner(
                &session,
                expected_revision,
                request.actor,
                &request.correlation_id,
            )
            .await?
        {
            return Err(AuthBootstrapServiceError::RevisionConflict(session.id));
        }
        Ok(session)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthBootstrapCreateRequest {
    pub owner_user_id: UserId,
    pub provider_id: ProviderId,
    pub provider_account_id: Option<ProviderAccountId>,
    pub purpose: AuthBootstrapPurpose,
    pub required_recipe_version: u32,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub actor: AuditActor,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct AuthBootstrapCreated {
    pub session: AuthBootstrapSession,
    pub pairing_token: SecretString,
}

#[derive(Debug)]
pub struct AuthBootstrapClaimRequest {
    pub session_id: AuthBootstrapSessionId,
    pub pairing_token: SecretString,
    pub claimed_at: Timestamp,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct AuthBootstrapClaimed {
    pub session: AuthBootstrapSession,
    pub access_token: SecretString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthBootstrapCancelRequest {
    pub owner_user_id: UserId,
    pub session_id: AuthBootstrapSessionId,
    pub cancelled_at: Timestamp,
    pub actor: AuditActor,
    pub correlation_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthBootstrapServiceError {
    #[error("authentication bootstrap session `{0}` does not exist for this owner")]
    SessionNotFound(AuthBootstrapSessionId),
    #[error("authentication bootstrap pairing token is invalid or expired")]
    PairingRejected,
    #[error("authentication bootstrap session `{0}` changed concurrently")]
    RevisionConflict(AuthBootstrapSessionId),
    #[error(transparent)]
    Domain(#[from] AuthBootstrapSessionError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthBootstrapState, Role};
    use asterism_storage::{Database, SqliteAuthBootstrapSessionRepository};
    use chrono::{Duration, Utc};

    use super::*;

    #[tokio::test]
    async fn create_and_claim_return_each_plaintext_token_only_once() {
        let (database, owner) = database_with_owner().await;
        let repository = SqliteAuthBootstrapSessionRepository::new(database.clone());
        let service = AuthBootstrapService::new(repository.clone()).unwrap();
        let created = service
            .create(create_request(owner, "bootstrap-service-create"))
            .await
            .unwrap();
        let pairing_plaintext = created.pairing_token.expose_secret().to_owned();
        assert!(pairing_plaintext.starts_with("ast_pair_"));
        assert!(!format!("{created:?}").contains(&pairing_plaintext));

        let wrong = service
            .claim(AuthBootstrapClaimRequest {
                session_id: created.session.id,
                pairing_token: SecretString::new("ast_pair_wrong"),
                claimed_at: Utc::now(),
                correlation_id: "bootstrap-service-wrong".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(wrong, AuthBootstrapServiceError::PairingRejected));

        let claimed = service
            .claim(AuthBootstrapClaimRequest {
                session_id: created.session.id,
                pairing_token: created.pairing_token,
                claimed_at: Utc::now(),
                correlation_id: "bootstrap-service-claim".to_owned(),
            })
            .await
            .unwrap();
        let access_plaintext = claimed.access_token.expose_secret().to_owned();
        assert!(access_plaintext.starts_with("ast_boot_"));
        assert!(!format!("{claimed:?}").contains(&access_plaintext));
        assert_eq!(claimed.session.state, AuthBootstrapState::Claimed);
        let replay = service
            .claim(AuthBootstrapClaimRequest {
                session_id: claimed.session.id,
                pairing_token: SecretString::new(pairing_plaintext),
                claimed_at: Utc::now(),
                correlation_id: "bootstrap-service-replay".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(replay, AuthBootstrapServiceError::PairingRejected));
    }

    #[tokio::test]
    async fn owner_cancel_invalidates_an_unclaimed_pairing() {
        let (database, owner) = database_with_owner().await;
        let repository = SqliteAuthBootstrapSessionRepository::new(database);
        let service = AuthBootstrapService::new(repository).unwrap();
        let created = service
            .create(create_request(owner, "bootstrap-cancel-create"))
            .await
            .unwrap();
        let session_id = created.session.id;
        let cancelled = service
            .cancel(AuthBootstrapCancelRequest {
                owner_user_id: owner,
                session_id,
                cancelled_at: Utc::now(),
                actor: AuditActor::User(owner),
                correlation_id: "bootstrap-cancel".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(cancelled.state, AuthBootstrapState::Cancelled);
        assert_eq!(cancelled.revision, 2);
        let rejected = service
            .claim(AuthBootstrapClaimRequest {
                session_id,
                pairing_token: created.pairing_token,
                claimed_at: Utc::now(),
                correlation_id: "bootstrap-cancelled-claim".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            rejected,
            AuthBootstrapServiceError::PairingRejected
        ));
    }

    async fn database_with_owner() -> (Database, UserId) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'bootstrap-service-owner', '$argon2id$test', 'active', \
              ?, '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        (database, owner)
    }

    fn create_request(owner: UserId, correlation_id: &str) -> AuthBootstrapCreateRequest {
        let now = Utc::now();
        AuthBootstrapCreateRequest {
            owner_user_id: owner,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_account_id: None,
            purpose: AuthBootstrapPurpose::AddAccount,
            required_recipe_version: 3,
            created_at: now,
            expires_at: now + Duration::minutes(10),
            actor: AuditActor::User(owner),
            correlation_id: correlation_id.to_owned(),
        }
    }
}
