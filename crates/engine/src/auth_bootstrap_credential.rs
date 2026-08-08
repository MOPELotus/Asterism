use std::sync::Arc;

use asterism_auth::{OpaqueTokenService, TokenError};
use asterism_domain::{
    AuthBootstrapPurpose, AuthBootstrapSession, AuthBootstrapSessionId, AuthState, ProviderAccount,
    ProviderAccountId, Timestamp,
};
use asterism_provider_api::{ProviderRegistry, SessionStatus};
use asterism_secrets::{
    CredentialBundle, ProviderCredential, SecretAccess, SecretActor, SecretStoreError, SecretString,
};
use asterism_storage::{
    AuthBootstrapCredentialCommitOutcome, AuthBootstrapCredentialCommitRequest,
    AuthBootstrapCredentialRepository, AuthBootstrapSessionRepository, ProviderAccountRepository,
    StorageError,
};
use chrono::Utc;

use crate::credential::{CredentialProvisionError, validate_candidate_for_account};

#[derive(Clone, Debug)]
pub struct AuthBootstrapCredentialService<A, S, C> {
    registry: Arc<ProviderRegistry>,
    accounts: A,
    sessions: S,
    credentials: C,
    access_tokens: OpaqueTokenService,
}

impl<A, S, C> AuthBootstrapCredentialService<A, S, C> {
    /// Builds the server-side Capture credential submission service.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] only if the fixed access-token prefix violates
    /// the opaque-token contract.
    pub fn new(
        registry: Arc<ProviderRegistry>,
        accounts: A,
        sessions: S,
        credentials: C,
    ) -> Result<Self, TokenError> {
        Ok(Self {
            registry,
            accounts,
            sessions,
            credentials,
            access_tokens: OpaqueTokenService::new("ast_boot")?,
        })
    }
}

impl<A, S, C> AuthBootstrapCredentialService<A, S, C>
where
    A: ProviderAccountRepository,
    S: AuthBootstrapSessionRepository,
    C: AuthBootstrapCredentialRepository,
{
    /// Validates one Capture candidate with the registered Provider and then
    /// enters the atomic account, `SecretStore`, and bootstrap commit boundary.
    ///
    /// # Errors
    ///
    /// Rejects invalid access, account metadata or binding drift, Provider
    /// validation failures, and atomic persistence failures.
    pub async fn submit(
        &self,
        request: AuthBootstrapCredentialRequest,
    ) -> Result<AuthBootstrapCredentialAccepted, AuthBootstrapCredentialServiceError> {
        let access_digest = self.access_tokens.digest(&request.access_token);
        let session = self
            .sessions
            .authenticate_auth_bootstrap_access(
                request.session_id,
                &access_digest,
                request.submitted_at,
            )
            .await?
            .ok_or(AuthBootstrapCredentialServiceError::AccessRejected)?;
        let access = SecretAccess {
            actor: SecretActor::CoreService("auth-bootstrap"),
            correlation_id: request.correlation_id,
            reason: "complete Capture credential submission".to_owned(),
        };
        let mut account = self
            .resolve_account(&session, request.display_name, &request.bundle)
            .await?;
        let (bundle, status) = validate_candidate_for_account(
            self.registry.as_ref(),
            &account,
            request.bundle,
            None,
            &access,
        )
        .await?;
        let completed_at = Utc::now();
        if session.purpose == AuthBootstrapPurpose::AddAccount {
            account.created_at = completed_at;
            account.updated_at = completed_at;
        }
        match self
            .credentials
            .commit_auth_bootstrap_credentials(AuthBootstrapCredentialCommitRequest {
                session_id: session.id,
                access_token_digest: &access_digest,
                validated_account: account,
                bundle,
                completed_at,
                access: &access,
            })
            .await?
        {
            AuthBootstrapCredentialCommitOutcome::Committed(committed) => {
                Ok(AuthBootstrapCredentialAccepted {
                    session: committed.session,
                    account: committed.account,
                    credentials: committed.credentials,
                    status,
                })
            }
            AuthBootstrapCredentialCommitOutcome::AccessRejected => {
                Err(AuthBootstrapCredentialServiceError::AccessRejected)
            }
            AuthBootstrapCredentialCommitOutcome::BindingConflict => {
                Err(AuthBootstrapCredentialServiceError::AccountBindingConflict)
            }
        }
    }

    async fn resolve_account(
        &self,
        session: &AuthBootstrapSession,
        display_name: Option<String>,
        bundle: &CredentialBundle,
    ) -> Result<ProviderAccount, AuthBootstrapCredentialServiceError> {
        match session.purpose {
            AuthBootstrapPurpose::AddAccount => {
                let display_name = display_name
                    .filter(|value| valid_display_name(value))
                    .ok_or(AuthBootstrapCredentialServiceError::InvalidAccountMetadata)?;
                Ok(ProviderAccount {
                    id: ProviderAccountId::new(),
                    owner_id: session.owner_user_id,
                    provider_id: session.provider_id.clone(),
                    display_name,
                    tenant: bundle.tenant.clone(),
                    auth_state: AuthState::Idle,
                    network_profile_id: None,
                    credential_refs: Vec::new(),
                    created_at: session.updated_at,
                    updated_at: session.updated_at,
                })
            }
            AuthBootstrapPurpose::Reauthenticate | AuthBootstrapPurpose::RepairSession => {
                if display_name.is_some() {
                    return Err(AuthBootstrapCredentialServiceError::InvalidAccountMetadata);
                }
                let account_id = session
                    .provider_account_id
                    .ok_or(AuthBootstrapCredentialServiceError::AccountBindingConflict)?;
                self.accounts
                    .find_provider_account(session.owner_user_id, account_id)
                    .await?
                    .ok_or(AuthBootstrapCredentialServiceError::AccountBindingConflict)
            }
        }
    }
}

fn valid_display_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug)]
pub struct AuthBootstrapCredentialRequest {
    pub session_id: AuthBootstrapSessionId,
    pub access_token: SecretString,
    pub display_name: Option<String>,
    pub bundle: CredentialBundle,
    pub submitted_at: Timestamp,
    pub correlation_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthBootstrapCredentialAccepted {
    pub session: AuthBootstrapSession,
    pub account: ProviderAccount,
    pub credentials: Vec<ProviderCredential>,
    pub status: SessionStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthBootstrapCredentialServiceError {
    #[error("authentication bootstrap access token is invalid or expired")]
    AccessRejected,
    #[error("authentication bootstrap account metadata is invalid for this purpose")]
    InvalidAccountMetadata,
    #[error("authentication bootstrap account binding changed during credential validation")]
    AccountBindingConflict,
    #[error(transparent)]
    Credential(#[from] CredentialProvisionError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use asterism_domain::{
        AuditActor, AuthBootstrapState, AuthMethod, ProviderId, Role, SessionKind,
    };
    use asterism_provider_api::{
        AuthChallenge, AuthenticationCapability, CredentialValidation, ProviderAuthContext,
        ProviderCapability, ProviderContext, ProviderEntry, ProviderIdentity, ProviderMetadata,
        ProviderResult, VerificationLevel,
    };
    use asterism_secrets::{
        CredentialAcquisition, CredentialField, SecretKey, SecretPurpose, SecretValue,
    };
    use asterism_storage::{
        Database, SecretKeyring, SqliteAuthBootstrapSessionRepository,
        SqliteProviderAccountRepository, SqliteSecretStore,
    };
    use async_trait::async_trait;
    use chrono::Duration;

    use super::*;
    use crate::{AuthBootstrapClaimRequest, AuthBootstrapCreateRequest, AuthBootstrapService};

    #[tokio::test]
    async fn valid_provider_status_creates_account_and_completes_bootstrap() {
        let fixture = fixture(true).await;
        let (session_id, access_token) =
            claimed_add_session(&fixture, "bootstrap-engine-valid").await;
        let access_plaintext = access_token.expose_secret().to_owned();
        let accepted = fixture
            .credentials
            .submit(AuthBootstrapCredentialRequest {
                session_id,
                access_token,
                display_name: Some("primary".to_owned()),
                bundle: credential_bundle(b"captured-cookie"),
                submitted_at: Utc::now(),
                correlation_id: "bootstrap-engine-submit".to_owned(),
            })
            .await
            .unwrap();
        assert!(accepted.status.valid);
        assert_eq!(accepted.session.state, AuthBootstrapState::Completed);
        assert_eq!(
            accepted.session.provider_account_id,
            Some(accepted.account.id)
        );
        assert_eq!(accepted.account.auth_state, AuthState::Authenticated);
        assert_eq!(accepted.credentials.len(), 1);
        let replay = fixture
            .credentials
            .submit(AuthBootstrapCredentialRequest {
                session_id,
                access_token: SecretString::new(access_plaintext),
                display_name: Some("primary".to_owned()),
                bundle: credential_bundle(b"captured-cookie"),
                submitted_at: Utc::now(),
                correlation_id: "bootstrap-engine-replay".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            replay,
            AuthBootstrapCredentialServiceError::AccessRejected
        ));
        assert_eq!(table_count(&fixture.database, "provider_accounts").await, 1);
        assert_eq!(table_count(&fixture.database, "secret_blobs").await, 1);
    }

    #[tokio::test]
    async fn provider_rejection_leaves_claimed_session_retryable_without_secret_writes() {
        let fixture = fixture(false).await;
        let (session_id, access_token) =
            claimed_add_session(&fixture, "bootstrap-engine-rejected").await;
        let access_plaintext = access_token.expose_secret().to_owned();
        let error = fixture
            .credentials
            .submit(AuthBootstrapCredentialRequest {
                session_id,
                access_token,
                display_name: Some("primary".to_owned()),
                bundle: credential_bundle(b"rejected-cookie"),
                submitted_at: Utc::now(),
                correlation_id: "bootstrap-engine-rejected-submit".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthBootstrapCredentialServiceError::Credential(
                CredentialProvisionError::CredentialRejected
            )
        ));
        let session = fixture
            .bootstrap
            .authenticate_access(crate::AuthBootstrapAccessRequest {
                session_id,
                access_token: SecretString::new(access_plaintext),
                authenticated_at: Utc::now(),
            })
            .await
            .unwrap();
        assert_eq!(session.state, AuthBootstrapState::Claimed);
        assert_eq!(table_count(&fixture.database, "provider_accounts").await, 0);
        assert_eq!(table_count(&fixture.database, "secret_blobs").await, 0);
    }

    struct Fixture {
        database: Database,
        owner: asterism_domain::UserId,
        bootstrap: AuthBootstrapService<SqliteAuthBootstrapSessionRepository>,
        credentials: AuthBootstrapCredentialService<
            SqliteProviderAccountRepository,
            SqliteAuthBootstrapSessionRepository,
            SqliteSecretStore,
        >,
    }

    async fn fixture(valid: bool) -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = asterism_domain::UserId::new();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'bootstrap-credential-owner', '$argon2id$test', 'active', \
              ?, '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let metadata = provider_metadata();
        let authentication = Arc::new(TestAuthentication {
            metadata: metadata.clone(),
            valid,
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication),
                ..ProviderEntry::metadata_only(metadata)
            })
            .unwrap();
        let registry = Arc::new(registry);
        let sessions = SqliteAuthBootstrapSessionRepository::new(database.clone());
        let accounts = SqliteProviderAccountRepository::new(database.clone());
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(
                SecretKeyring::new(
                    "key-a",
                    BTreeMap::from([("key-a".to_owned(), SecretKey::new([41; 32]))]),
                )
                .unwrap(),
            ),
        );
        Fixture {
            database,
            owner,
            bootstrap: AuthBootstrapService::new(sessions.clone()).unwrap(),
            credentials: AuthBootstrapCredentialService::new(registry, accounts, sessions, store)
                .unwrap(),
        }
    }

    async fn claimed_add_session(
        fixture: &Fixture,
        correlation_prefix: &str,
    ) -> (AuthBootstrapSessionId, SecretString) {
        let now = Utc::now();
        let created = fixture
            .bootstrap
            .create(AuthBootstrapCreateRequest {
                owner_user_id: fixture.owner,
                provider_id: ProviderId::new("provider-alpha").unwrap(),
                provider_account_id: None,
                purpose: AuthBootstrapPurpose::AddAccount,
                required_recipe_version: 3,
                created_at: now,
                expires_at: now + Duration::minutes(10),
                actor: AuditActor::User(fixture.owner),
                correlation_id: format!("{correlation_prefix}-create"),
            })
            .await
            .unwrap();
        let claimed = fixture
            .bootstrap
            .claim(AuthBootstrapClaimRequest {
                session_id: created.session.id,
                pairing_token: created.pairing_token,
                claimed_at: Utc::now(),
                correlation_id: format!("{correlation_prefix}-claim"),
            })
            .await
            .unwrap();
        (claimed.session.id, claimed.access_token)
    }

    fn provider_metadata() -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "Provider Alpha".to_owned(),
            implementation_version: "1.0.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: Some(3),
            capabilities: BTreeSet::from([ProviderCapability::Authentication]),
            auth_methods: BTreeSet::from([AuthMethod::ImportedCookie]),
            session_kinds: BTreeSet::from([SessionKind::Cookie]),
        }
    }

    fn credential_bundle(value: &[u8]) -> CredentialBundle {
        let captured_at = Utc::now();
        CredentialBundle {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            tenant: None,
            auth_method: AuthMethod::ImportedCookie,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at,
            expires_at: None,
            session_kind: SessionKind::Cookie,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(value.to_vec()),
            }],
            user_id_hint: None,
        }
    }

    async fn table_count(database: &Database, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(database.pool())
            .await
            .unwrap()
    }

    #[derive(Debug)]
    struct TestAuthentication {
        metadata: ProviderMetadata,
        valid: bool,
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
            _context: &ProviderAuthContext,
            _method: AuthMethod,
        ) -> ProviderResult<AuthChallenge> {
            unreachable!("Capture submission does not begin a Core AuthSession")
        }

        async fn validate_credential(
            &self,
            context: &ProviderAuthContext,
            credential: &CredentialBundle,
        ) -> ProviderResult<CredentialValidation> {
            assert_eq!(context.provider_id, credential.provider_id);
            assert!(context.auth_session_id.is_none());
            Ok(CredentialValidation::accepted(SessionStatus {
                valid: self.valid,
                kind: credential.session_kind,
                expires_at: None,
                account_hint: Some("remote-account".to_owned()),
            }))
        }

        async fn validate_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<SessionStatus> {
            unreachable!("Capture submission validates the candidate directly")
        }
    }
}
