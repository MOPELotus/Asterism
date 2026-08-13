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
    UaiAuthenticationTransport, UaiJwtSession, UaiSessionResolver,
    authentication::{
        MAX_PASSWORD_BYTES, MAX_USERNAME_BYTES, is_imported_session_acquisition,
        valid_browser_cookie, validate_login_field,
    },
    metadata::PROVIDER_ID,
};

const RENEWAL_LOCK_COUNT: usize = 32;
const MAX_RENEWED_SESSION_CACHE_ENTRIES: usize = 128;
const RENEWED_SESSION_CACHE_TTL: Duration = Duration::from_mins(10);

/// UAI session adapter backed by Core's provider-scoped credential resolver.
/// Plaintext exists only for one bounded operation.
pub struct StoredUaiSessionResolver {
    credentials: Arc<dyn ProviderCredentialResolver>,
    renewal: Option<UaiRenewal>,
    renewal_locks: [Mutex<()>; RENEWAL_LOCK_COUNT],
    renewed_sessions: Mutex<HashMap<RenewedSessionKey, CachedRenewedSession>>,
}

struct UaiRenewal {
    credentials: Arc<dyn ProviderCredentialRenewer>,
    authentication: Arc<dyn UaiAuthenticationTransport>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RenewedSessionKey {
    provider_account_id: ProviderAccountId,
    credential_refs: Vec<asterism_domain::SecretId>,
    correlation_id: String,
}

struct CachedRenewedSession {
    session: UaiJwtSession,
    cached_at: Instant,
}

impl StoredUaiSessionResolver {
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
        authentication: Arc<dyn UaiAuthenticationTransport>,
    ) -> Self {
        Self {
            credentials,
            renewal: Some(UaiRenewal {
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
    ) -> ProviderResult<Option<UaiJwtSession>> {
        let now = Instant::now();
        let wall_clock = Utc::now();
        let mut sessions = self.renewed_sessions.lock().await;
        sessions.retain(|_, cached| {
            now.duration_since(cached.cached_at) < RENEWED_SESSION_CACHE_TTL
                && cached
                    .session
                    .expires_at()
                    .is_none_or(|expires_at| expires_at > wall_clock)
        });
        sessions
            .get(&renewed_session_key(context))
            .map(|cached| clone_session(&cached.session))
            .transpose()
    }

    async fn cache_renewed_session(
        &self,
        context: &ProviderContext,
        session: &UaiJwtSession,
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
                session: clone_session(session)?,
                cached_at: Instant::now(),
            },
        );
        Ok(())
    }

    async fn resolve_optional_browser_cookie(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Option<SecretString>> {
        let resolved = self
            .credentials
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: context.account_id,
                credential_refs: context.credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCookie],
                correlation_id: context.correlation_id.clone(),
            })
            .await;
        let mut resolved = match resolved {
            Ok(resolved) => resolved,
            Err(SecretStoreError::AccountMismatch) => return Ok(None),
            Err(error) => return Err(map_resolution_error(&error)),
        };
        if resolved.len() != 1 {
            return Err(invalid_stored_session());
        }
        let resolved = resolved.pop().expect("one browser Cookie was required");
        let metadata = &resolved.credential;
        let cookie = std::str::from_utf8(resolved.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        if metadata.provider_account_id != context.account_id
            || metadata.secret.purpose != SecretPurpose::ProviderCookie
            || !context.credential_refs.contains(&metadata.secret.id)
            || metadata.session_kind != SessionKind::Jwt
            || !is_imported_session_acquisition(metadata.acquired_via)
            || metadata.is_expired_at(Utc::now())
            || !valid_browser_cookie(cookie)
        {
            return Err(invalid_stored_session());
        }
        Ok(Some(SecretString::new(cookie.to_owned())))
    }
}

impl fmt::Debug for StoredUaiSessionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredUaiSessionResolver")
            .field("credentials", &"configured")
            .field("renewal", &self.renewal.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl UaiSessionResolver for StoredUaiSessionResolver {
    async fn resolve_session(&self, context: &ProviderContext) -> ProviderResult<UaiJwtSession> {
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
                purposes: vec![SecretPurpose::ProviderCompositeSession],
                correlation_id: context.correlation_id.clone(),
            })
            .await
            .map_err(|error| map_resolution_error(&error))?;
        if credentials.len() != 1 {
            return Err(invalid_stored_session());
        }
        let resolved = credentials.pop().expect("one credential was required");
        let metadata = &resolved.credential;
        let valid_origin = matches!(
            (metadata.session_kind, metadata.acquired_via),
            (
                SessionKind::Composite,
                CredentialAcquisition::NativeProviderLogin
            )
        ) || (metadata.session_kind == SessionKind::Jwt
            && is_imported_session_acquisition(metadata.acquired_via));
        if metadata.provider_account_id != context.account_id
            || metadata.secret.purpose != SecretPurpose::ProviderCompositeSession
            || !context.credential_refs.contains(&metadata.secret.id)
            || !valid_origin
            || metadata.is_expired_at(Utc::now())
        {
            return Err(invalid_stored_session());
        }
        let session = UaiJwtSession::try_from_composite(resolved.value.expose_secret())
            .map_err(|_| invalid_stored_session())?;
        session.attach_browser_cookie(self.resolve_optional_browser_cookie(context).await?)
    }

    async fn renew_session(&self, context: &ProviderContext) -> ProviderResult<UaiJwtSession> {
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
                    SecretPurpose::ProviderCompositeSession,
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
        renewal.authentication.validate_jwt(&session).await?;
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

fn clone_session(session: &UaiJwtSession) -> ProviderResult<UaiJwtSession> {
    UaiJwtSession::try_new(
        session.expose_open_id().to_owned(),
        session.expose_authorization().to_owned(),
    )
    .and_then(|cloned| {
        cloned.attach_school_header(
            session
                .expose_school_header()
                .map(|school| SecretString::new(school.to_owned())),
        )
    })
    .and_then(|cloned| {
        cloned.attach_browser_cookie(
            session
                .expose_browser_cookie()
                .map(|cookie| SecretString::new(cookie.to_owned())),
        )
    })
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
        SecretPurpose::ProviderCompositeSession,
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
    session: &UaiJwtSession,
) -> ProviderResult<CredentialBundle> {
    Ok(CredentialBundle {
        provider_id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI compile-time Provider ID is invalid",
            )
        })?,
        tenant: None,
        auth_method: AuthMethod::Password,
        acquired_via: CredentialAcquisition::NativeProviderLogin,
        captured_at: Utc::now(),
        expires_at: session.expires_at(),
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
                purpose: SecretPurpose::ProviderCompositeSession,
                value: session.to_secret_value()?,
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
    ProviderError::new(kind, "UAI stored session could not be resolved")
}

fn invalid_stored_session() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "UAI account has no usable stored openid/JWT session",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, Timestamp, UserId};
    use asterism_secrets::{
        ProviderCredential, ProviderCredentialRenewal, ProviderCredentialRenewer,
        ResolvedProviderCredential, SecretRef, SecretValue,
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum FixtureBehavior {
        Imported,
        CaptureTool,
        CaptureCookie,
        BrowserExtension,
        Native,
        Duplicate,
        ForeignAccount,
        UnrequestedSecret,
        WrongPurpose,
        WrongOrigin,
        Expired,
        Malformed,
        StorageFailure,
    }

    #[derive(Debug)]
    struct FixtureCredentialResolver {
        behavior: FixtureBehavior,
        request: Mutex<Option<ProviderCredentialResolution>>,
        resolutions: AtomicUsize,
    }

    impl FixtureCredentialResolver {
        fn session_document(&self) -> &'static [u8] {
            if matches!(self.behavior, FixtureBehavior::CaptureCookie) {
                br#"{"openid":"stored-open-id","jwt":"STORED_HEADER.STORED_PAYLOAD.STORED_SIGNATURE","school":"school-42"}"#
            } else {
                br#"{"openid":"stored-open-id","jwt":"STORED_HEADER.STORED_PAYLOAD.STORED_SIGNATURE"}"#
            }
        }

        fn resolve_cookie(
            &self,
            request: &ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            if !matches!(self.behavior, FixtureBehavior::CaptureCookie) {
                return Err(SecretStoreError::AccountMismatch);
            }
            Ok(vec![resolved_credential(
                request.provider_account_id,
                request.credential_refs[1],
                SecretPurpose::ProviderCookie,
                SessionKind::Jwt,
                CredentialAcquisition::CaptureTool,
                None,
                b"session=synthetic; csrf=synthetic",
            )])
        }
    }

    #[derive(Debug)]
    struct RenewalCredentialResolver {
        native: bool,
        expired: bool,
        resolutions: AtomicUsize,
    }

    #[async_trait]
    impl ProviderCredentialResolver for RenewalCredentialResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.credential_refs.len(), 3);
            assert_eq!(
                request.purposes,
                [
                    SecretPurpose::ProviderUsername,
                    SecretPurpose::ProviderPassword,
                    SecretPurpose::ProviderCompositeSession,
                ]
            );
            let (kind, acquired_via) = if self.native {
                (
                    SessionKind::Composite,
                    CredentialAcquisition::NativeProviderLogin,
                )
            } else {
                (SessionKind::Jwt, CredentialAcquisition::ManualImport)
            };
            let expires_at = self.expired.then(Timestamp::default);
            Ok(vec![
                resolved_credential(
                    request.provider_account_id,
                    request.credential_refs[0],
                    SecretPurpose::ProviderUsername,
                    kind,
                    acquired_via,
                    expires_at,
                    b"test-user",
                ),
                resolved_credential(
                    request.provider_account_id,
                    request.credential_refs[1],
                    SecretPurpose::ProviderPassword,
                    kind,
                    acquired_via,
                    expires_at,
                    b"test-password",
                ),
                resolved_credential(
                    request.provider_account_id,
                    request.credential_refs[2],
                    SecretPurpose::ProviderCompositeSession,
                    kind,
                    acquired_via,
                    expires_at,
                    br#"{"openid":"old-open-id","jwt":"OLD.HEADER.SIGNATURE"}"#,
                ),
            ])
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
                field.purpose == SecretPurpose::ProviderCompositeSession
                    && UaiJwtSession::try_from_composite(field.value.expose_secret())
                        .is_ok_and(|session| session.expose_open_id() == "new-open-id")
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
    impl UaiAuthenticationTransport for FixtureAuthentication {
        async fn exchange_password(
            &self,
            username: &SecretString,
            password: &SecretString,
        ) -> ProviderResult<UaiJwtSession> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            assert_eq!(username.expose_secret(), "test-user");
            assert_eq!(password.expose_secret(), "test-password");
            UaiJwtSession::try_new("new-open-id", "NEW.HEADER.SIGNATURE")
        }

        async fn validate_jwt(&self, session: &UaiJwtSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            assert_eq!(session.expose_open_id(), "new-open-id");
            Ok(())
        }
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureCredentialResolver {
        async fn resolve_provider_credentials(
            &self,
            request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            if request.purposes == [SecretPurpose::ProviderCookie] {
                return self.resolve_cookie(&request);
            }
            *self.request.lock().unwrap() = Some(request.clone());
            let secret_id = request.credential_refs[0];
            let valid = |session_kind, acquired_via, value: &[u8]| {
                resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCompositeSession,
                    session_kind,
                    acquired_via,
                    None,
                    value,
                )
            };
            let document = self.session_document();
            match self.behavior {
                FixtureBehavior::Imported => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    document,
                )]),
                FixtureBehavior::CaptureTool | FixtureBehavior::CaptureCookie => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::CaptureTool,
                    document,
                )]),
                FixtureBehavior::BrowserExtension => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::BrowserExtension,
                    document,
                )]),
                FixtureBehavior::Native => Ok(vec![valid(
                    SessionKind::Composite,
                    CredentialAcquisition::NativeProviderLogin,
                    document,
                )]),
                FixtureBehavior::Duplicate => Ok(vec![
                    valid(
                        SessionKind::Jwt,
                        CredentialAcquisition::ManualImport,
                        document,
                    ),
                    valid(
                        SessionKind::Jwt,
                        CredentialAcquisition::ManualImport,
                        document,
                    ),
                ]),
                FixtureBehavior::ForeignAccount => Ok(vec![resolved_credential(
                    ProviderAccountId::new(),
                    secret_id,
                    SecretPurpose::ProviderCompositeSession,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    None,
                    document,
                )]),
                FixtureBehavior::UnrequestedSecret => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    SecretId::new(),
                    SecretPurpose::ProviderCompositeSession,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    None,
                    document,
                )]),
                FixtureBehavior::WrongPurpose => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderAccessToken,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    None,
                    document,
                )]),
                FixtureBehavior::WrongOrigin => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::NativeProviderLogin,
                    document,
                )]),
                FixtureBehavior::Expired => Ok(vec![resolved_credential(
                    request.provider_account_id,
                    secret_id,
                    SecretPurpose::ProviderCompositeSession,
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    Some(Timestamp::default()),
                    document,
                )]),
                FixtureBehavior::Malformed => Ok(vec![valid(
                    SessionKind::Jwt,
                    CredentialAcquisition::ManualImport,
                    b"not-json",
                )]),
                FixtureBehavior::StorageFailure => Err(SecretStoreError::Storage),
            }
        }
    }

    #[tokio::test]
    async fn resolves_only_context_bound_imported_or_native_session() {
        for behavior in [
            FixtureBehavior::Imported,
            FixtureBehavior::CaptureTool,
            FixtureBehavior::BrowserExtension,
            FixtureBehavior::Native,
        ] {
            let credentials = Arc::new(FixtureCredentialResolver {
                behavior,
                request: Mutex::new(None),
                resolutions: AtomicUsize::new(0),
            });
            let resolver = StoredUaiSessionResolver::new(credentials.clone());
            let context = provider_context();
            let session = resolver.resolve_session(&context).await.unwrap();
            assert_eq!(session.expose_open_id(), "stored-open-id");
            assert!(!format!("{resolver:?}").contains("stored-open-id"));
            let request = credentials.request.lock().unwrap().clone().unwrap();
            assert_eq!(request.provider_account_id, context.account_id);
            assert_eq!(request.credential_refs, context.credential_refs);
            assert_eq!(request.purposes, [SecretPurpose::ProviderCompositeSession]);
            assert_eq!(request.correlation_id, context.correlation_id);
        }
    }

    #[tokio::test]
    async fn captured_cookie_is_optional_account_bound_and_zeroizing() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::CaptureCookie,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let resolver = StoredUaiSessionResolver::new(credentials);
        let mut context = provider_context();
        context.credential_refs.push(SecretId::new());
        let session = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(
            session.expose_browser_cookie(),
            Some("session=synthetic; csrf=synthetic")
        );
        assert_eq!(session.expose_school_header(), Some("school-42"));
        assert!(!format!("{session:?}").contains("session=synthetic"));
        assert!(!format!("{session:?}").contains("school-42"));
    }

    #[tokio::test]
    async fn rejects_unbound_expired_or_malformed_credentials() {
        for behavior in [
            FixtureBehavior::Duplicate,
            FixtureBehavior::ForeignAccount,
            FixtureBehavior::UnrequestedSecret,
            FixtureBehavior::WrongPurpose,
            FixtureBehavior::WrongOrigin,
            FixtureBehavior::Expired,
            FixtureBehavior::Malformed,
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
    async fn rejects_wrong_context_before_storage_and_sanitizes_storage_errors() {
        let credentials = Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Imported,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        });
        let resolver = StoredUaiSessionResolver::new(credentials.clone());
        let mut wrong = provider_context();
        wrong.provider_id = ProviderId::new("welearn").unwrap();
        assert_eq!(
            resolver.resolve_session(&wrong).await.unwrap_err().kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 0);

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
    async fn renewal_reauthenticates_validates_and_commits_atomically_once() {
        let credentials = Arc::new(RenewalCredentialResolver {
            native: true,
            expired: false,
            resolutions: AtomicUsize::new(0),
        });
        let renewer = Arc::new(FixtureRenewer::default());
        let authentication = Arc::new(FixtureAuthentication::default());
        let resolver = StoredUaiSessionResolver::with_renewal(
            credentials.clone(),
            renewer.clone(),
            authentication.clone(),
        );
        let context = renewal_context();
        let first = resolver.renew_session(&context).await.unwrap();
        let cached = resolver.renew_session(&context).await.unwrap();
        let from_cache = resolver.resolve_session(&context).await.unwrap();
        assert_eq!(first.expose_open_id(), "new-open-id");
        assert_eq!(cached.expose_open_id(), "new-open-id");
        assert_eq!(from_cache.expose_open_id(), "new-open-id");
        assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 1);
        assert_eq!(renewer.renewals.load(Ordering::SeqCst), 1);
        assert_eq!(authentication.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(authentication.validations.load(Ordering::SeqCst), 1);
        assert!(!format!("{resolver:?}").contains("new-open-id"));
    }

    #[tokio::test]
    async fn expired_native_composite_can_only_be_used_for_atomic_renewal() {
        let credentials = Arc::new(RenewalCredentialResolver {
            native: true,
            expired: true,
            resolutions: AtomicUsize::new(0),
        });
        let renewer = Arc::new(FixtureRenewer::default());
        let authentication = Arc::new(FixtureAuthentication::default());
        let resolver = StoredUaiSessionResolver::with_renewal(
            credentials,
            renewer.clone(),
            authentication.clone(),
        );
        let session = resolver.renew_session(&renewal_context()).await.unwrap();
        assert_eq!(session.expose_open_id(), "new-open-id");
        assert_eq!(renewer.renewals.load(Ordering::SeqCst), 1);
        assert_eq!(authentication.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(authentication.validations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expired_jwt_never_survives_the_short_renewal_cache() {
        let resolver = StoredUaiSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior: FixtureBehavior::Imported,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        }));
        let expiry_seconds = 1_577_836_800;
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{expiry_seconds}}}"#));
        let session =
            UaiJwtSession::try_new("expired-open-id", format!("header.{payload}.signature"))
                .unwrap();
        let context = renewal_context();
        resolver
            .cache_renewed_session(&context, &session)
            .await
            .unwrap();
        assert!(
            resolver
                .cached_renewed_session(&context)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn renewal_rejects_manual_import_metadata_before_login() {
        let credentials = Arc::new(RenewalCredentialResolver {
            native: false,
            expired: false,
            resolutions: AtomicUsize::new(0),
        });
        let renewer = Arc::new(FixtureRenewer::default());
        let authentication = Arc::new(FixtureAuthentication::default());
        let resolver = StoredUaiSessionResolver::with_renewal(
            credentials,
            renewer.clone(),
            authentication.clone(),
        );
        assert_eq!(
            resolver
                .renew_session(&renewal_context())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(renewer.renewals.load(Ordering::SeqCst), 0);
        assert_eq!(authentication.exchanges.load(Ordering::SeqCst), 0);
    }

    fn fixture_resolver(behavior: FixtureBehavior) -> StoredUaiSessionResolver {
        StoredUaiSessionResolver::new(Arc::new(FixtureCredentialResolver {
            behavior,
            request: Mutex::new(None),
            resolutions: AtomicUsize::new(0),
        }))
    }

    fn resolved_credential(
        provider_account_id: ProviderAccountId,
        secret_id: SecretId,
        purpose: SecretPurpose,
        session_kind: SessionKind,
        acquired_via: CredentialAcquisition,
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
                acquired_via,
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
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-stored-session-test".to_owned(),
        }
    }

    fn renewal_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new(), SecretId::new(), SecretId::new()],
            correlation_id: "uai-renewal-test".to_owned(),
        }
    }
}
