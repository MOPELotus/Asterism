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
    ResolvedProviderCredential, SecretPurpose, SecretStoreError, SecretString, SecretValue,
};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Mutex;

use crate::{
    WellearnAuthenticationTransport, WellearnCookieSession, WellearnSessionResolver,
    authentication::{MAX_PASSWORD_BYTES, MAX_USERNAME_BYTES, validate_login_field},
    metadata::PROVIDER_ID,
};

const RENEWAL_LOCK_COUNT: usize = 32;
const MAX_RENEWED_SESSION_CACHE_ENTRIES: usize = 128;
const RENEWED_SESSION_CACHE_TTL: Duration = Duration::from_mins(10);

/// `WELearn` session adapter backed by Core's provider-scoped credential
/// resolver. Plaintext exists only for the duration of one bounded operation.
pub struct StoredWellearnSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
    renewal: Option<WellearnRenewal>,
    renewal_locks: [Mutex<()>; RENEWAL_LOCK_COUNT],
    renewed_sessions: Mutex<HashMap<RenewedSessionKey, CachedRenewedSession>>,
}

struct WellearnRenewal {
    credentials: Arc<dyn ProviderCredentialRenewer>,
    authentication: Arc<dyn WellearnAuthenticationTransport>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RenewedSessionKey {
    provider_account_id: ProviderAccountId,
    credential_refs: Vec<asterism_domain::SecretId>,
    correlation_id: String,
}

struct CachedRenewedSession {
    session: WellearnCookieSession,
    cached_at: Instant,
}

impl StoredWellearnSessionResolver {
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
        authentication: Arc<dyn WellearnAuthenticationTransport>,
    ) -> Self {
        Self {
            credentials,
            renewal: Some(WellearnRenewal {
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
    ) -> ProviderResult<Option<WellearnCookieSession>> {
        let now = Instant::now();
        let mut sessions = self.renewed_sessions.lock().await;
        sessions
            .retain(|_, cached| now.duration_since(cached.cached_at) < RENEWED_SESSION_CACHE_TTL);
        sessions
            .get(&renewed_session_key(context))
            .map(|cached| WellearnCookieSession::try_new(cached.session.expose_secret().to_owned()))
            .transpose()
    }

    async fn cache_renewed_session(
        &self,
        context: &ProviderContext,
        session: &WellearnCookieSession,
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
                session: WellearnCookieSession::try_new(session.expose_secret().to_owned())?,
                cached_at: Instant::now(),
            },
        );
        Ok(())
    }
}

impl fmt::Debug for StoredWellearnSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredWellearnSessionResolver")
            .field("credentials", &"configured")
            .field("renewal", &self.renewal.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl WellearnSessionResolver for StoredWellearnSessionResolver {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnCookieSession> {
        if context.provider_id.as_str() != PROVIDER_ID || context.credential_refs.is_empty() {
            return Err(invalid_stored_session());
        }
        if let Some(session) = self.cached_renewed_session(context).await? {
            return Ok(session);
        }
        let mut credentials = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCookie],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        if credentials.len() != 1 {
            return Err(invalid_stored_session());
        }
        let resolved = credentials.pop().expect("one credential was required");
        let metadata = &resolved.credential;
        if metadata.provider_account_id != context.account_id
            || metadata.secret.purpose != SecretPurpose::ProviderCookie
            || !context.credential_refs.contains(&metadata.secret.id)
            || !matches!(
                metadata.session_kind,
                asterism_domain::SessionKind::Cookie | asterism_domain::SessionKind::Composite
            )
            || metadata.is_expired_at(Utc::now())
        {
            return Err(invalid_stored_session());
        }
        let cookie = std::str::from_utf8(resolved.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        WellearnCookieSession::try_new(cookie.to_owned())
    }

    async fn renew_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnCookieSession> {
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
        validate_renewal_credentials(&resolved, context)?;
        let username = resolved_text(&resolved, SecretPurpose::ProviderUsername)?;
        let password = resolved_text(&resolved, SecretPurpose::ProviderPassword)?;
        validate_login_field(username, MAX_USERNAME_BYTES, true)?;
        validate_login_field(password, MAX_PASSWORD_BYTES, false)?;
        let username_secret = SecretString::new(username.to_owned());
        let password_secret = SecretString::new(password.to_owned());
        let session = renewal
            .authentication
            .exchange_password(&username_secret, &password_secret)
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
    context: &ProviderContext,
) -> ProviderResult<()> {
    if credentials.len() != 3
        || credentials.iter().any(|resolved| {
            resolved.credential.provider_account_id != context.account_id
                || resolved.credential.session_kind != SessionKind::Composite
                || resolved.credential.acquired_via != CredentialAcquisition::NativeProviderLogin
                || !context
                    .credential_refs
                    .contains(&resolved.credential.secret.id)
                || resolved.credential.is_expired_at(Utc::now())
        })
        || credentials.iter().enumerate().any(|(index, resolved)| {
            credentials[..index]
                .iter()
                .any(|previous| previous.credential.secret.id == resolved.credential.secret.id)
        })
    {
        return Err(invalid_stored_session());
    }
    for purpose in [
        SecretPurpose::ProviderUsername,
        SecretPurpose::ProviderPassword,
        SecretPurpose::ProviderCookie,
    ] {
        if credentials
            .iter()
            .filter(|resolved| resolved.credential.secret.purpose == purpose)
            .count()
            != 1
        {
            return Err(invalid_stored_session());
        }
    }
    Ok(())
}

fn resolved_text(
    credentials: &[ResolvedProviderCredential],
    purpose: SecretPurpose,
) -> ProviderResult<&str> {
    let credential = credentials
        .iter()
        .find(|resolved| resolved.credential.secret.purpose == purpose)
        .ok_or_else(invalid_stored_session)?;
    std::str::from_utf8(credential.value.expose_secret()).map_err(|_| invalid_stored_session())
}

fn renewed_bundle(
    username: &str,
    password: &str,
    session: &WellearnCookieSession,
) -> ProviderResult<CredentialBundle> {
    Ok(CredentialBundle {
        provider_id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn compile-time Provider ID is invalid",
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
    ProviderError::new(kind, "WELearn stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "WELearn account has no usable stored Cookie session",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{
        ProviderAccountId, ProviderId, SecretId, SessionKind, Timestamp, UserId,
    };
    use asterism_secrets::{
        CredentialAcquisition, ProviderCredential, ResolvedProviderCredential, SecretRef,
        SecretValue,
    };

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum FixtureBehavior {
        Cookie,
        Composite,
        MissingCookie,
        DuplicateCookie,
        ForeignAccount,
        UnrequestedSecret,
        ExpiredCookie,
        InvalidUtf8,
        StorageFailure,
    }

    #[derive(Debug)]
    struct FixtureCredentialResolver {
        behavior: FixtureBehavior,
        request: StdMutex<Option<ProviderCredentialResolution>>,
        resolutions: AtomicUsize,
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureCredentialResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            *self.request.lock().unwrap() = Some(request.clone());
            let secret_id = request.credential_refs[0];
            let valid = || {
                resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    b"session=SAFE_SESSION",
                )
            };
            match self.behavior {
                FixtureBehavior::Cookie => Ok(vec![valid()]),
                FixtureBehavior::Composite if request.purposes.len() == 1 => {
                    Ok(vec![resolved_credential(
                        request.provider_account_id,
                        secret_id,
                        SecretPurpose::ProviderCookie,
                        SessionKind::Composite,
                        None,
                        b"session=SAFE_COMPOSITE",
                    )])
                }
                FixtureBehavior::Composite => Ok(vec![
                    resolved_credential(
                        request.provider_account_id,
                        request.credential_refs[0],
                        SecretPurpose::ProviderUsername,
                        SessionKind::Composite,
                        None,
                        b"student-a",
                    ),
                    resolved_credential(
                        request.provider_account_id,
                        request.credential_refs[1],
                        SecretPurpose::ProviderPassword,
                        SessionKind::Composite,
                        None,
                        b"password-a",
                    ),
                    resolved_credential(
                        request.provider_account_id,
                        request.credential_refs[2],
                        SecretPurpose::ProviderCookie,
                        SessionKind::Composite,
                        None,
                        b"session=OLD_SESSION",
                    ),
                ]),
                FixtureBehavior::MissingCookie => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderPassword,
                    SessionKind::Composite,
                    None,
                    b"private-password",
                )]),
                FixtureBehavior::DuplicateCookie => Ok(vec![valid(), valid()]),
                FixtureBehavior::ForeignAccount => Ok(vec![resolved_credential(
                    ProviderAccountId::new(),
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    b"session=FOREIGN",
                )]),
                FixtureBehavior::UnrequestedSecret => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    SecretId::new(),
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    b"session=UNREQUESTED",
                )]),
                FixtureBehavior::ExpiredCookie => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    Some(Timestamp::default()),
                    b"session=EXPIRED",
                )]),
                FixtureBehavior::InvalidUtf8 => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCookie,
                    SessionKind::Cookie,
                    None,
                    &[0xff],
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
            assert_eq!(request.bundle.auth_method, AuthMethod::Password);
            assert_eq!(request.bundle.session_kind, SessionKind::Composite);
            assert!(request.bundle.fields.iter().any(|field| {
                field.purpose == SecretPurpose::ProviderCookie
                    && field.value.expose_secret() == b"session=NEW_SESSION"
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
    impl WellearnAuthenticationTransport for FixtureAuthentication {
        async fn exchange_password(
            &self,
            username: &SecretString,
            password: &SecretString,
        ) -> ProviderResult<WellearnCookieSession> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            assert_eq!(username.expose_secret(), "student-a");
            assert_eq!(password.expose_secret(), "password-a");
            WellearnCookieSession::try_new("session=NEW_SESSION")
        }

        async fn validate_cookie(&self, _session: &WellearnCookieSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn resolves_only_the_context_bound_cookie() {
        for (behavior, expected) in [
            (FixtureBehavior::Cookie, "session=SAFE_SESSION"),
            (FixtureBehavior::Composite, "session=SAFE_COMPOSITE"),
        ] {
            let credentials = Arc::new(FixtureCredentialResolver {
                behavior,
                request: StdMutex::new(None),
                resolutions: AtomicUsize::new(0),
            });
            let resolver = StoredWellearnSessionResolver::new(credentials.clone());
            let context = provider_context();
            let session = resolver.resolve_session(&context).await.unwrap();
            assert_eq!(session.expose_secret(), expected);
            assert!(!format!("{resolver:?}").contains("SAFE"));
            let request = credentials.request.lock().unwrap().clone().unwrap();
            assert_eq!(request.provider_account_id, context.account_id);
            assert_eq!(request.credential_refs, context.credential_refs);
            assert_eq!(request.purposes, [SecretPurpose::ProviderCookie]);
            assert_eq!(request.correlation_id, context.correlation_id);
        }
    }

    #[tokio::test]
    async fn rejects_unbound_expired_duplicate_or_malformed_credentials() {
        for behavior in [
            FixtureBehavior::MissingCookie,
            FixtureBehavior::DuplicateCookie,
            FixtureBehavior::ForeignAccount,
            FixtureBehavior::UnrequestedSecret,
            FixtureBehavior::ExpiredCookie,
            FixtureBehavior::InvalidUtf8,
        ] {
            assert_eq!(
                fixture_resolver(behavior)
                    .resolve_session(&provider_context())
                    .await
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::Authentication
            );
        }
    }

    #[tokio::test]
    async fn rejects_wrong_context_before_storage_and_maps_storage_failures() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Cookie,
            request: StdMutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let resolver = StoredWellearnSessionResolver::new(credentials.clone());
        let mut wrong_provider = provider_context();
        wrong_provider.provider_id = ProviderId::new("chaoxing").unwrap();
        assert_eq!(
            resolver
                .resolve_session(&wrong_provider)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert!(credentials.request.lock().unwrap().is_none());

        assert_eq!(
            fixture_resolver(FixtureBehavior::StorageFailure)
                .resolve_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Internal
        );
    }

    #[tokio::test]
    async fn renewal_reauthenticates_once_and_commits_the_complete_bundle() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Composite,
            request: StdMutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let renewer = Arc::new(FixtureRenewer::default());
        let authentication = Arc::new(FixtureAuthentication::default());
        let resolver = StoredWellearnSessionResolver::with_renewal(
            credentials.clone(),
            renewer.clone(),
            authentication.clone(),
        );
        let context = provider_context();
        let session = resolver.renew_session(&context).await.unwrap();
        assert_eq!(session.expose_secret(), "session=NEW_SESSION");
        assert_eq!(authentication.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(authentication.validations.load(Ordering::SeqCst), 1);
        assert_eq!(renewer.renewals.load(Ordering::SeqCst), 1);
        let cached = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(cached.expose_secret(), "session=NEW_SESSION");
        assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn renewal_rejects_an_incomplete_bundle_before_login() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Cookie,
            request: StdMutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let renewer = Arc::new(FixtureRenewer::default());
        let authentication = Arc::new(FixtureAuthentication::default());
        let resolver = StoredWellearnSessionResolver::with_renewal(
            credentials,
            renewer.clone(),
            authentication.clone(),
        );
        assert_eq!(
            resolver
                .renew_session(&provider_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(authentication.exchanges.load(Ordering::SeqCst), 0);
        assert_eq!(renewer.renewals.load(Ordering::SeqCst), 0);
    }

    fn fixture_resolver(behavior: FixtureBehavior) -> StoredWellearnSessionResolver {
        StoredWellearnSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior,
            request: StdMutex::new(None),
            resolutions: AtomicUsize::new(0),
        }))
    }

    fn resolved_credential(
        provider_account_id: ProviderAccountId,
        secret_id: SecretId,
        purpose: SecretPurpose,
        session_kind: SessionKind,
        expires_at: Option<Timestamp>,
        value: &[u8],
    ) -> ResolvedProviderCredential {
        let now = Timestamp::default();
        ResolvedProviderCredential {
            credential: ProviderCredential {
                provider_account_id,
                secret: SecretRef {
                    id: secret_id,
                    owner_user_id: UserId::new(),
                    purpose,
                    version: 1,
                    key_id: "key-a".to_owned(),
                    created_at: now,
                    updated_at: now,
                },
                session_kind,
                acquired_via: CredentialAcquisition::NativeProviderLogin,
                captured_at: now,
                expires_at,
                updated_at: now,
            },
            value: SecretValue::new(value.to_vec()),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new(), SecretId::new(), SecretId::new()],
            correlation_id: "welearn-stored-session-test".to_owned(),
        }
    }
}
