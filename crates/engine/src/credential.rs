use std::sync::Arc;

use asterism_domain::{
    AuthMethod, AuthSessionId, ProviderAccount, ProviderAccountId, ProviderId, SessionKind,
    Timestamp, UserId,
};
use asterism_provider_api::{
    CredentialValidation, ProviderAuthContext, ProviderError, ProviderRegistry, SessionStatus,
};
use asterism_secrets::{
    CredentialBundle, CredentialBundleError, ProviderCredential, ProviderCredentialStore,
    SecretAccess, SecretStoreError,
};
use asterism_storage::{ProviderAccountRepository, StorageError};

#[derive(Clone, Debug)]
pub struct ProviderCredentialService<A, C> {
    registry: Arc<ProviderRegistry>,
    accounts: A,
    credentials: C,
}

impl<A, C> ProviderCredentialService<A, C> {
    pub const fn new(registry: Arc<ProviderRegistry>, accounts: A, credentials: C) -> Self {
        Self {
            registry,
            accounts,
            credentials,
        }
    }
}

impl<A, C> ProviderCredentialService<A, C>
where
    A: ProviderAccountRepository,
    C: ProviderCredentialStore,
{
    /// Validates a candidate against its registered Provider before atomically
    /// replacing the account's encrypted credentials.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialProvisionError`] before persistence when ownership,
    /// account binding, advertised authentication support, or Provider
    /// validation fails. Storage failures leave the previous credential set
    /// and authentication state intact.
    pub async fn validate_and_store(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        bundle: CredentialBundle,
        access: &SecretAccess,
    ) -> Result<CredentialCommit, CredentialProvisionError> {
        let (bundle, status) = validate_candidate(
            self.registry.as_ref(),
            &self.accounts,
            owner_user_id,
            provider_account_id,
            bundle,
            None,
            access,
        )
        .await?;
        let credentials = self
            .credentials
            .replace_provider_credentials(owner_user_id, provider_account_id, bundle, access)
            .await?;
        Ok(CredentialCommit {
            credentials,
            status,
        })
    }
}

pub(crate) async fn validate_candidate<A: ProviderAccountRepository>(
    registry: &ProviderRegistry,
    accounts: &A,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    bundle: CredentialBundle,
    auth_session_id: Option<AuthSessionId>,
    access: &SecretAccess,
) -> Result<(CredentialBundle, SessionStatus), CredentialProvisionError> {
    if !access.authorizes(owner_user_id) {
        return Err(CredentialProvisionError::Unauthorized);
    }
    bundle.validate()?;
    let account = accounts
        .find_provider_account(owner_user_id, provider_account_id)
        .await?
        .ok_or(CredentialProvisionError::AccountNotFound(
            provider_account_id,
        ))?;
    validate_candidate_for_account(registry, &account, bundle, auth_session_id, access).await
}

pub(crate) async fn validate_candidate_for_account(
    registry: &ProviderRegistry,
    account: &ProviderAccount,
    mut bundle: CredentialBundle,
    auth_session_id: Option<AuthSessionId>,
    access: &SecretAccess,
) -> Result<(CredentialBundle, SessionStatus), CredentialProvisionError> {
    if !access.authorizes(account.owner_id) {
        return Err(CredentialProvisionError::Unauthorized);
    }
    bundle.validate()?;
    if account.provider_id != bundle.provider_id || account.tenant != bundle.tenant {
        return Err(CredentialProvisionError::AccountMismatch);
    }
    let entry = registry.get(&bundle.provider_id).ok_or_else(|| {
        CredentialProvisionError::ProviderNotRegistered(bundle.provider_id.clone())
    })?;
    if !entry.metadata.auth_methods.contains(&bundle.auth_method) {
        return Err(CredentialProvisionError::UnsupportedAuthMethod(
            bundle.auth_method,
        ));
    }
    if !entry.metadata.session_kinds.contains(&bundle.session_kind) {
        return Err(CredentialProvisionError::UnsupportedSessionKind(
            bundle.session_kind,
        ));
    }
    let authentication = entry
        .authentication
        .as_ref()
        .ok_or(CredentialProvisionError::AuthenticationUnavailable)?;
    let context = ProviderAuthContext {
        provider_id: account.provider_id.clone(),
        account_id: account.id,
        auth_session_id,
        correlation_id: access.correlation_id.clone(),
    };
    let CredentialValidation {
        status,
        replacement,
    } = authentication
        .validate_credential(&context, &bundle)
        .await?;
    let persisted_kind = replacement
        .as_ref()
        .map_or(bundle.session_kind, |replacement| replacement.session_kind);
    if !entry.metadata.session_kinds.contains(&persisted_kind) {
        return Err(CredentialProvisionError::InvalidProviderStatus);
    }
    validate_status(bundle.captured_at, persisted_kind, &status)?;
    if let Some(replacement) = replacement {
        bundle.session_kind = replacement.session_kind;
        bundle.fields = replacement.fields;
    }
    bundle.expires_at = status.expires_at.or(bundle.expires_at);
    bundle.user_id_hint = status.account_hint.clone().or(bundle.user_id_hint);
    bundle.validate()?;
    Ok((bundle, status))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialCommit {
    pub credentials: Vec<ProviderCredential>,
    pub status: SessionStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialProvisionError {
    #[error("credential operation is not authorized")]
    Unauthorized,
    #[error("credential bundle is invalid: {0}")]
    InvalidBundle(#[from] CredentialBundleError),
    #[error("Provider account `{0}` does not exist for this owner")]
    AccountNotFound(ProviderAccountId),
    #[error("credential bundle does not match its Provider account")]
    AccountMismatch,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider does not expose authentication")]
    AuthenticationUnavailable,
    #[error("provider does not advertise authentication method `{0:?}`")]
    UnsupportedAuthMethod(AuthMethod),
    #[error("provider does not advertise session kind `{0:?}`")]
    UnsupportedSessionKind(SessionKind),
    #[error("provider rejected the candidate credential")]
    CredentialRejected,
    #[error("provider returned an inconsistent credential status")]
    InvalidProviderStatus,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}

fn validate_status(
    captured_at: Timestamp,
    expected_kind: SessionKind,
    status: &SessionStatus,
) -> Result<(), CredentialProvisionError> {
    if !status.valid {
        return Err(CredentialProvisionError::CredentialRejected);
    }
    let hint_valid = status.account_hint.as_deref().is_none_or(|hint| {
        !hint.is_empty()
            && hint.len() <= 256
            && hint.trim() == hint
            && !hint.chars().any(char::is_control)
    });
    if status.kind != expected_kind
        || status
            .expires_at
            .is_some_and(|expires_at| expires_at <= captured_at)
        || !hint_valid
    {
        return Err(CredentialProvisionError::InvalidProviderStatus);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    use asterism_domain::{AuditActor, AuthState, ProviderAccount, Role, Timestamp};
    use asterism_provider_api::{
        AuthChallenge, AuthenticationCapability, CredentialReplacement, CredentialValidation,
        ProviderCapability, ProviderContext, ProviderEntry, ProviderIdentity, ProviderMetadata,
        ProviderResult, VerificationLevel,
    };
    use asterism_secrets::{
        CredentialAcquisition, CredentialField, SecretActor, SecretKey, SecretPurpose, SecretStore,
        SecretValue,
    };
    use asterism_storage::{
        Database, SecretKeyring, SqliteProviderAccountRepository, SqliteSecretStore,
    };
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn valid_provider_status_commits_credentials_and_authenticates_account() {
        let fixture = fixture(true, SessionKind::Cookie).await;
        let captured_at = Utc::now();
        let committed = fixture
            .service
            .validate_and_store(
                fixture.owner_id,
                fixture.account_id,
                bundle(captured_at, b"candidate-cookie"),
                &fixture.access,
            )
            .await
            .unwrap();

        assert_eq!(committed.credentials.len(), 1);
        assert!(committed.status.valid);
        assert_eq!(
            fixture
                .store
                .get(&committed.credentials[0].secret, &fixture.access)
                .await
                .unwrap()
                .expose_secret(),
            b"candidate-cookie"
        );
        let account = fixture
            .accounts
            .find_provider_account(fixture.owner_id, fixture.account_id)
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
    async fn rejected_or_inconsistent_status_never_reaches_persistence() {
        for (valid, kind, expected) in [
            (
                false,
                SessionKind::Cookie,
                CredentialProvisionError::CredentialRejected,
            ),
            (
                true,
                SessionKind::BearerToken,
                CredentialProvisionError::InvalidProviderStatus,
            ),
        ] {
            let fixture = fixture(valid, kind).await;
            let error = fixture
                .service
                .validate_and_store(
                    fixture.owner_id,
                    fixture.account_id,
                    bundle(Utc::now(), b"rejected-cookie"),
                    &fixture.access,
                )
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), expected.to_string());
            let blob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
            assert_eq!(blob_count, 0);
            let account = fixture
                .accounts
                .find_provider_account(fixture.owner_id, fixture.account_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(account.auth_state, AuthState::Idle);
        }
    }

    #[tokio::test]
    async fn provider_replacement_is_revalidated_and_committed_atomically() {
        let fixture = fixture_with_replacement().await;
        let committed = fixture
            .service
            .validate_and_store(
                fixture.owner_id,
                fixture.account_id,
                password_bundle(Utc::now()),
                &fixture.access,
            )
            .await
            .unwrap();

        assert_eq!(committed.status.kind, SessionKind::Composite);
        assert_eq!(committed.credentials.len(), 3);
        let mut persisted = HashMap::new();
        for credential in &committed.credentials {
            assert_eq!(credential.session_kind, SessionKind::Composite);
            persisted.insert(
                credential.secret.purpose,
                fixture
                    .store
                    .get(&credential.secret, &fixture.access)
                    .await
                    .unwrap()
                    .expose_secret()
                    .to_vec(),
            );
        }
        assert_eq!(
            persisted.get(&SecretPurpose::ProviderUsername),
            Some(&b"derived-user".to_vec())
        );
        assert_eq!(
            persisted.get(&SecretPurpose::ProviderPassword),
            Some(&b"derived-password".to_vec())
        );
        assert_eq!(
            persisted.get(&SecretPurpose::ProviderCookie),
            Some(&b"derived-cookie".to_vec())
        );
        assert!(persisted.values().all(|value| value != b"input-password"));
    }

    #[tokio::test]
    async fn invalid_provider_replacement_never_reaches_persistence() {
        let fixture = fixture_with_options(true, SessionKind::Composite, true, true).await;
        let error = fixture
            .service
            .validate_and_store(
                fixture.owner_id,
                fixture.account_id,
                password_bundle(Utc::now()),
                &fixture.access,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CredentialProvisionError::InvalidBundle(_)));
        let blob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret_blobs")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(blob_count, 0);
    }

    struct Fixture {
        database: Database,
        owner_id: UserId,
        account_id: ProviderAccountId,
        accounts: SqliteProviderAccountRepository,
        store: SqliteSecretStore,
        service: ProviderCredentialService<SqliteProviderAccountRepository, SqliteSecretStore>,
        access: SecretAccess,
    }

    async fn fixture(valid: bool, kind: SessionKind) -> Fixture {
        fixture_with_options(valid, kind, false, false).await
    }

    async fn fixture_with_replacement() -> Fixture {
        fixture_with_options(true, SessionKind::Composite, true, false).await
    }

    async fn fixture_with_options(
        valid: bool,
        kind: SessionKind,
        derive_replacement: bool,
        invalid_replacement: bool,
    ) -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = UserId::new();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'credential-owner', '$argon2id$test', 'active', ?, '[]', \
              ?, ?)",
        )
        .bind(owner_id.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account_id = ProviderAccountId::new();
        let account = ProviderAccount {
            id: account_id,
            owner_id,
            provider_id: provider_id.clone(),
            display_name: "Primary".to_owned(),
            tenant: Some("tenant-a".to_owned()),
            auth_state: AuthState::Idle,
            network_profile_id: None,
            credential_refs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        let accounts = SqliteProviderAccountRepository::new(database.clone());
        accounts
            .create_provider_account(&account, AuditActor::User(owner_id))
            .await
            .unwrap();
        let metadata = provider_metadata(provider_id);
        let authentication = Arc::new(TestAuthentication {
            metadata: metadata.clone(),
            status: SessionStatus {
                valid,
                kind,
                expires_at: Some(now + chrono::Duration::hours(1)),
                account_hint: Some("remote-account".to_owned()),
            },
            derive_replacement,
            invalid_replacement,
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication),
                ..ProviderEntry::metadata_only(metadata)
            })
            .unwrap();
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(
                SecretKeyring::new(
                    "key-a",
                    BTreeMap::from([("key-a".to_owned(), SecretKey::new([7; 32]))]),
                )
                .unwrap(),
            ),
        );
        let service =
            ProviderCredentialService::new(Arc::new(registry), accounts.clone(), store.clone());
        let access = SecretAccess {
            actor: SecretActor::User(owner_id),
            correlation_id: "credential-validation-test".to_owned(),
            reason: "validate candidate credential".to_owned(),
        };
        Fixture {
            database,
            owner_id,
            account_id,
            accounts,
            store,
            service,
            access,
        }
    }

    fn bundle(captured_at: Timestamp, value: &[u8]) -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            tenant: Some("tenant-a".to_owned()),
            auth_method: AuthMethod::ImportedCookie,
            acquired_via: CredentialAcquisition::ManualImport,
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

    fn password_bundle(captured_at: Timestamp) -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            tenant: Some("tenant-a".to_owned()),
            auth_method: AuthMethod::Password,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at,
            expires_at: None,
            session_kind: SessionKind::ProviderSpecific,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderUsername,
                    value: SecretValue::new(b"input-user".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderPassword,
                    value: SecretValue::new(b"input-password".to_vec()),
                },
            ],
            user_id_hint: None,
        }
    }

    fn provider_metadata(provider_id: ProviderId) -> ProviderMetadata {
        ProviderMetadata {
            id: provider_id,
            display_name: "Provider Alpha".to_owned(),
            implementation_version: "1.0.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::Authentication]),
            auth_methods: BTreeSet::from([AuthMethod::ImportedCookie, AuthMethod::Password]),
            session_kinds: BTreeSet::from([
                SessionKind::Cookie,
                SessionKind::Composite,
                SessionKind::ProviderSpecific,
            ]),
        }
    }

    #[derive(Debug)]
    struct TestAuthentication {
        metadata: ProviderMetadata,
        status: SessionStatus,
        derive_replacement: bool,
        invalid_replacement: bool,
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
                session_id: context.auth_session_id.unwrap_or_default(),
                method,
                waiting_for: asterism_domain::WaitingUserState::SessionImport,
                user_action: None,
                expires_at: None,
                external_oauth: None,
            })
        }

        async fn validate_credential(
            &self,
            context: &ProviderAuthContext,
            credential: &CredentialBundle,
        ) -> ProviderResult<CredentialValidation> {
            assert_eq!(context.provider_id, credential.provider_id);
            assert!(!credential.fields.is_empty());
            assert!(
                credential
                    .fields
                    .iter()
                    .all(|field| !field.value.expose_secret().is_empty())
            );
            let replacement = self.derive_replacement.then(|| CredentialReplacement {
                session_kind: SessionKind::Composite,
                fields: vec![
                    CredentialField {
                        purpose: SecretPurpose::ProviderUsername,
                        value: SecretValue::new(b"derived-user".to_vec()),
                    },
                    CredentialField {
                        purpose: SecretPurpose::ProviderPassword,
                        value: SecretValue::new(b"derived-password".to_vec()),
                    },
                    CredentialField {
                        purpose: if self.invalid_replacement {
                            SecretPurpose::ProviderUsername
                        } else {
                            SecretPurpose::ProviderCookie
                        },
                        value: SecretValue::new(b"derived-cookie".to_vec()),
                    },
                ],
            });
            Ok(CredentialValidation {
                status: self.status.clone(),
                replacement,
            })
        }

        async fn validate_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<SessionStatus> {
            Ok(self.status.clone())
        }
    }
}
