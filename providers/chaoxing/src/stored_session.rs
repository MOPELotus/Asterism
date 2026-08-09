use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use asterism_domain::{AuthMethod, ProviderAccountId, ProviderId, SessionKind};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, CredentialField, ProviderCredentialRenewal,
    ProviderCredentialRenewer, ProviderCredentialResolution, ProviderCredentialResolver,
    ResolvedProviderCredential, SecretPurpose, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Mutex;

use crate::{
    ChaoxingAuthenticationTransport, ChaoxingCookieSession, ChaoxingSessionResolver,
    authentication::{
        MAX_PASSWORD_BYTES, MAX_USERNAME_BYTES, encrypt_login_field, validate_login_field,
    },
    metadata::PROVIDER_ID,
};

const RENEWAL_LOCK_COUNT: usize = 32;
const MAX_RENEWED_SESSION_CACHE_ENTRIES: usize = 128;
const RENEWED_SESSION_CACHE_TTL: Duration = Duration::from_mins(10);

/// Chaoxing session adapter backed by Core's provider-scoped credential
/// resolver. It receives plaintext only for the duration of one operation and
/// never queries persistence directly.
pub struct StoredChaoxingSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
    renewal: Option<ChaoxingRenewal>,
    renewal_locks: [Mutex<()>; RENEWAL_LOCK_COUNT],
    renewed_sessions: Mutex<HashMap<RenewedSessionKey, CachedRenewedSession>>,
}

struct ChaoxingRenewal {
    credentials: Arc<dyn ProviderCredentialRenewer>,
    authentication: Arc<dyn ChaoxingAuthenticationTransport>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RenewedSessionKey {
    provider_account_id: ProviderAccountId,
    credential_refs: Vec<asterism_domain::SecretId>,
    correlation_id: String,
}

struct CachedRenewedSession {
    session: ChaoxingCookieSession,
    cached_at: Instant,
}

impl StoredChaoxingSessionResolver {
    pub fn new(credentials: Arc<dyn ProviderCredentialResolver>) -> Self {
        Self {
            credentials,
            renewal: None,
            renewal_locks: std::array::from_fn(|_| Mutex::new(())),
            renewed_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_renewal(
        credentials: Arc<dyn ProviderCredentialResolver>,
        renewer: Arc<dyn ProviderCredentialRenewer>,
        authentication: Arc<dyn ChaoxingAuthenticationTransport>,
    ) -> Self {
        Self {
            credentials,
            renewal: Some(ChaoxingRenewal {
                credentials: renewer,
                authentication,
            }),
            renewal_locks: std::array::from_fn(|_| Mutex::new(())),
            renewed_sessions: Mutex::new(HashMap::new()),
        }
    }

    fn renewal_lock(&self, account_id: ProviderAccountId) -> &Mutex<()> {
        let mut hasher = DefaultHasher::new();
        account_id.hash(&mut hasher);
        let index = usize::try_from(hasher.finish() % RENEWAL_LOCK_COUNT as u64)
            .expect("renewal lock index is bounded");
        &self.renewal_locks[index]
    }

    async fn cached_renewed_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Option<ChaoxingCookieSession>> {
        let now = Instant::now();
        let mut sessions = self.renewed_sessions.lock().await;
        sessions
            .retain(|_, cached| now.duration_since(cached.cached_at) < RENEWED_SESSION_CACHE_TTL);
        sessions
            .get(&renewed_session_key(context))
            .map(|cached| ChaoxingCookieSession::try_new(cached.session.expose_secret().to_owned()))
            .transpose()
    }

    async fn cache_renewed_session(
        &self,
        context: &ProviderContext,
        session: &ChaoxingCookieSession,
    ) -> ProviderResult<()> {
        let key = renewed_session_key(context);
        let mut sessions = self.renewed_sessions.lock().await;
        if sessions.len() == MAX_RENEWED_SESSION_CACHE_ENTRIES && !sessions.contains_key(&key) {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, cached)| cached.cached_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            key,
            CachedRenewedSession {
                session: ChaoxingCookieSession::try_new(session.expose_secret().to_owned())?,
                cached_at: Instant::now(),
            },
        );
        Ok(())
    }
}

impl fmt::Debug for StoredChaoxingSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredChaoxingSessionResolver")
            .field("credentials", &"configured")
            .field("renewal", &self.renewal.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ChaoxingSessionResolver for StoredChaoxingSessionResolver {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingCookieSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        if let Some(session) = self.cached_renewed_session(context).await? {
            return Ok(session);
        }
        let credentials = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCookie],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        let mut cookies = credentials.into_iter().filter(|resolved| {
            resolved.credential.provider_account_id == context.account_id
                && resolved.credential.secret.purpose == SecretPurpose::ProviderCookie
                && matches!(
                    resolved.credential.session_kind,
                    SessionKind::Cookie | SessionKind::Composite
                )
        });
        let cookie = cookies.next().ok_or_else(invalid_stored_session)?;
        if cookies.next().is_some() || cookie.credential.is_expired_at(Utc::now()) {
            return Err(invalid_stored_session());
        }
        let cookie_text = std::str::from_utf8(cookie.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        ChaoxingCookieSession::try_new(cookie_text.to_owned())
    }

    async fn renew_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingCookieSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        let renewal = self.renewal.as_ref().ok_or_else(invalid_stored_session)?;
        let _guard = self.renewal_lock(context.account_id).lock().await;
        if let Some(session) = self.cached_renewed_session(context).await? {
            return Ok(session);
        }
        let resolved = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![
                    SecretPurpose::ProviderUsername,
                    SecretPurpose::ProviderPassword,
                    SecretPurpose::ProviderCookie,
                ],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        validate_renewal_credentials(&resolved, context.account_id)?;
        let username = resolved_text(&resolved, SecretPurpose::ProviderUsername)?;
        let password = resolved_text(&resolved, SecretPurpose::ProviderPassword)?;
        validate_login_field(username, MAX_USERNAME_BYTES, true)?;
        validate_login_field(password, MAX_PASSWORD_BYTES, false)?;
        let encrypted_username = encrypt_login_field(username)?;
        let encrypted_password = encrypt_login_field(password)?;
        let session = renewal
            .authentication
            .exchange_password(&encrypted_username, &encrypted_password)
            .await?;
        renewal.authentication.validate_cookie(&session).await?;
        let expected_credentials = resolved
            .iter()
            .map(|resolved| resolved.credential.clone())
            .collect();
        let bundle = renewed_bundle(username, password, &session)?;
        renewal
            .credentials
            .renew_provider_credentials(ProviderCredentialRenewal {
                provider_account_id: context.account_id,
                expected_credentials,
                bundle,
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        self.cache_renewed_session(context, &session).await?;
        Ok(session)
    }
}

fn renewed_session_key(context: &ProviderContext) -> RenewedSessionKey {
    RenewedSessionKey {
        provider_account_id: context.account_id,
        credential_refs: context.credential_refs.clone(),
        correlation_id: context.correlation_id.clone(),
    }
}

fn validate_renewal_credentials(
    credentials: &[ResolvedProviderCredential],
    provider_account_id: ProviderAccountId,
) -> ProviderResult<()> {
    if credentials.len() == 3
        && credentials.iter().all(|resolved| {
            resolved.credential.provider_account_id == provider_account_id
                && resolved.credential.session_kind == SessionKind::Composite
        })
    {
        Ok(())
    } else {
        Err(invalid_stored_session())
    }
}

fn resolved_text(
    credentials: &[ResolvedProviderCredential],
    purpose: SecretPurpose,
) -> ProviderResult<&str> {
    let mut matches = credentials
        .iter()
        .filter(|resolved| resolved.credential.secret.purpose == purpose);
    let credential = matches.next().ok_or_else(invalid_stored_session)?;
    if matches.next().is_some() {
        return Err(invalid_stored_session());
    }
    std::str::from_utf8(credential.value.expose_secret()).map_err(|_| invalid_stored_session())
}

fn renewed_bundle(
    username: &str,
    password: &str,
    session: &ChaoxingCookieSession,
) -> ProviderResult<CredentialBundle> {
    Ok(CredentialBundle {
        provider_id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing compile-time Provider ID is invalid",
            )
        })?,
        tenant: None,
        auth_method: AuthMethod::Password,
        acquired_via: CredentialAcquisition::NativeProviderLogin,
        captured_at: Utc::now(),
        expires_at: None,
        session_kind: SessionKind::Composite,
        fields: vec![
            CredentialField {
                purpose: SecretPurpose::ProviderUsername,
                value: SecretValue::new(username.as_bytes().to_vec()),
            },
            CredentialField {
                purpose: SecretPurpose::ProviderPassword,
                value: SecretValue::new(password.as_bytes().to_vec()),
            },
            CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(session.expose_secret().as_bytes().to_vec()),
            },
        ],
        user_id_hint: None,
    })
}

fn map_resolution_error(error: &SecretStoreError) -> ProviderError {
    let kind = match error {
        SecretStoreError::NotFound
        | SecretStoreError::Unauthorized
        | SecretStoreError::VersionConflict
        | SecretStoreError::InvalidValue
        | SecretStoreError::CredentialManaged
        | SecretStoreError::AccountMismatch => ProviderErrorKind::Authentication,
        SecretStoreError::KeyUnavailable
        | SecretStoreError::Storage
        | SecretStoreError::AuthenticationFailed => ProviderErrorKind::Internal,
    };
    ProviderError::new(kind, "Chaoxing stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Chaoxing account has no usable stored Cookie session",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{
        ProviderAccountId, ProviderId, SecretId, SessionKind, Timestamp, UserId,
    };
    use asterism_secrets::{
        CredentialAcquisition, ProviderCredential, ResolvedProviderCredential, SecretRef,
        SecretString, SecretValue,
    };

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum FixtureBehavior {
        Cookie,
        Composite,
        MissingCookie,
        StorageFailure,
    }

    #[derive(Debug)]
    struct FixtureCredentialResolver {
        behavior: FixtureBehavior,
        request: Mutex<Option<ProviderCredentialResolution>>,
        resolutions: AtomicUsize,
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureCredentialResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            let provider_account_id = request.provider_account_id;
            *self.request.lock().unwrap() = Some(request);
            match self.behavior {
                FixtureBehavior::Cookie => Ok(vec![resolved_credential(
                    provider_account_id,
                    SecretPurpose::ProviderCookie,
                    b"_uid=SAFE_UID; uf=SAFE_UF",
                )]),
                FixtureBehavior::Composite => Ok(vec![
                    resolved_credential(
                        provider_account_id,
                        SecretPurpose::ProviderUsername,
                        b"student-a",
                    ),
                    resolved_credential(
                        provider_account_id,
                        SecretPurpose::ProviderPassword,
                        b"password-a",
                    ),
                    resolved_credential(
                        provider_account_id,
                        SecretPurpose::ProviderCookie,
                        b"_uid=OLD_UID; uf=OLD_UF",
                    ),
                ]),
                FixtureBehavior::MissingCookie => Ok(vec![resolved_credential(
                    provider_account_id,
                    SecretPurpose::ProviderPassword,
                    b"private-password",
                )]),
                FixtureBehavior::StorageFailure => Err(SecretStoreError::Storage),
            }
        }
    }

    #[derive(Debug, Default)]
    struct FixtureRenewer {
        renewals: AtomicUsize,
    }

    #[async_trait]
    impl ProviderCredentialRenewer for FixtureRenewer {
        async fn renew_provider_credentials(
            &self,
            request: ProviderCredentialRenewal,
        ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
            self.renewals.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.expected_credentials.len(), 3);
            assert!(request.bundle.fields.iter().any(|field| {
                field.purpose == SecretPurpose::ProviderCookie
                    && field.value.expose_secret() == b"_uid=NEW_UID; uf=NEW_UF"
            }));
            Ok(request.expected_credentials)
        }
    }

    #[derive(Debug, Default)]
    struct FixtureAuthentication {
        exchanges: AtomicUsize,
        validations: AtomicUsize,
    }

    #[async_trait]
    impl ChaoxingAuthenticationTransport for FixtureAuthentication {
        async fn exchange_password(
            &self,
            encrypted_username: &SecretString,
            encrypted_password: &SecretString,
        ) -> ProviderResult<ChaoxingCookieSession> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            assert!(!encrypted_username.expose_secret().contains("student-a"));
            assert!(!encrypted_password.expose_secret().contains("password-a"));
            ChaoxingCookieSession::try_new("_uid=NEW_UID; uf=NEW_UF")
        }

        async fn validate_cookie(&self, _session: &ChaoxingCookieSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn resolves_only_the_context_bound_cookie() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Cookie,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let resolver = StoredChaoxingSessionResolver::new(credentials.clone());
        let context = provider_context();
        let session = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(session.expose_secret(), "_uid=SAFE_UID; uf=SAFE_UF");
        assert!(!format!("{resolver:?}").contains("SAFE_UID"));
        let request = credentials.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.provider_account_id, context.account_id);
        assert_eq!(request.credential_refs, context.credential_refs);
        assert_eq!(request.purposes, [SecretPurpose::ProviderCookie]);
        assert_eq!(request.correlation_id, context.correlation_id);
    }

    #[tokio::test]
    async fn rejects_missing_cookie_wrong_provider_and_storage_failure() {
        let missing = StoredChaoxingSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::MissingCookie,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        }));
        assert_eq!(
            missing
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );

        let storage = StoredChaoxingSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::StorageFailure,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        }));
        assert_eq!(
            storage
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );

        let mut wrong_provider = provider_context();
        wrong_provider.provider_id = ProviderId::new("welearn").unwrap();
        assert_eq!(
            missing
                .resolve_session(&wrong_provider)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
    }

    #[tokio::test]
    async fn renewal_reauthenticates_once_and_commits_with_full_metadata() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Composite,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let renewer = Arc::new(FixtureRenewer::default());
        let authentication = Arc::new(FixtureAuthentication::default());
        let resolver = StoredChaoxingSessionResolver::with_renewal(
            credentials.clone(),
            renewer.clone(),
            authentication.clone(),
        );
        let context = provider_context();
        let session = resolver.renew_session(&context).await.unwrap();
        assert_eq!(session.expose_secret(), "_uid=NEW_UID; uf=NEW_UF");
        assert_eq!(authentication.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(authentication.validations.load(Ordering::SeqCst), 1);
        assert_eq!(renewer.renewals.load(Ordering::SeqCst), 1);
        let cached = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(cached.expose_secret(), "_uid=NEW_UID; uf=NEW_UF");
        assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 1);
    }

    fn resolved_credential(
        provider_account_id: ProviderAccountId,
        purpose: SecretPurpose,
        value: &[u8],
    ) -> ResolvedProviderCredential {
        let now = Timestamp::default();
        ResolvedProviderCredential {
            credential: ProviderCredential {
                provider_account_id,
                secret: SecretRef {
                    id: SecretId::new(),
                    owner_user_id: UserId::new(),
                    purpose,
                    version: 1,
                    key_id: "key-a".to_owned(),
                    created_at: now,
                    updated_at: now,
                },
                session_kind: SessionKind::Composite,
                acquired_via: CredentialAcquisition::NativeProviderLogin,
                captured_at: now,
                expires_at: None,
                updated_at: now,
            },
            value: SecretValue::new(value.to_vec()),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-stored-session-test".to_owned(),
        }
    }
}
