use std::{fmt, sync::Arc};

use asterism_domain::{AuthMethod, HumanRequiredReason, SessionKind, Timestamp, WaitingUserState};
use asterism_provider_api::{
    AuthChallenge, AuthenticationCapability, CaptureCredentialOutput, CaptureJsonField,
    CaptureReadiness, CaptureRecipe, CaptureScalarSource, CaptureValueSource,
    CredentialReplacement, CredentialValidation, ProviderAuthContext, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderResult,
    SessionStatus,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, CredentialField, SecretPurpose, SecretString,
    SecretValue,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::metadata::development_metadata;

const MAX_LOGIN_RESPONSE_BYTES: usize = 64 * 1_024;
pub(crate) const MAX_USERNAME_BYTES: usize = 512;
pub(crate) const MAX_PASSWORD_BYTES: usize = 4 * 1_024;
const MAX_JWT_BYTES: usize = 64 * 1_024;
const MAX_OPEN_ID_BYTES: usize = 512;
const MAX_SESSION_DOCUMENT_BYTES: usize = 96 * 1_024;
pub(crate) const MAX_BROWSER_COOKIE_BYTES: usize = 64 * 1_024;
const CAPTURE_START_URL: &str = "https://ucontent.unipus.cn/";
const UCONTENT_ORIGIN: &str = "https://ucontent.unipus.cn";
const IPUB_ORIGIN: &str = "https://ipub.unipus.cn";

fn capture_recipe_v3() -> CaptureRecipe {
    CaptureRecipe {
        version: 3,
        start_url: CAPTURE_START_URL.to_owned(),
        navigation_origins: vec![UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()],
        read_origins: vec![UCONTENT_ORIGIN.to_owned()],
        poll_interval_millis: 500,
        auth_method: AuthMethod::AssistedSession,
        session_kind: SessionKind::Jwt,
        readiness: CaptureReadiness::OutputsComplete,
        outputs: vec![
            CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCompositeSession,
                required: true,
                sources: vec![CaptureValueSource::JsonObject {
                    fields: vec![
                        CaptureJsonField {
                            name: "openid".to_owned(),
                            sources: vec![CaptureScalarSource::RequestHeader {
                                origin: UCONTENT_ORIGIN.to_owned(),
                                name: "u-openid".to_owned(),
                            }],
                        },
                        CaptureJsonField {
                            name: "jwt".to_owned(),
                            sources: vec![CaptureScalarSource::RequestHeader {
                                origin: UCONTENT_ORIGIN.to_owned(),
                                name: "Authorization".to_owned(),
                            }],
                        },
                    ],
                }],
            },
            CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCookie,
                required: true,
                sources: vec![CaptureValueSource::CookieHeader {
                    origin: UCONTENT_ORIGIN.to_owned(),
                }],
            },
        ],
    }
}

pub(crate) const fn is_imported_session_acquisition(acquisition: CredentialAcquisition) -> bool {
    matches!(
        acquisition,
        CredentialAcquisition::ManualImport
            | CredentialAcquisition::CaptureTool
            | CredentialAcquisition::BrowserExtension
    )
}

/// A bounded atomic `openid`/JWT session. Both values are redacted and
/// zeroized through their `SecretString` owners.
pub struct UaiJwtSession {
    open_id: SecretString,
    authorization: SecretString,
    expires_at: Option<Timestamp>,
    browser_cookie: Option<SecretString>,
}

impl UaiJwtSession {
    /// Validates one login or imported session pair.
    ///
    /// # Errors
    ///
    /// Returns an Authentication error for an unsafe open ID or malformed JWT.
    pub fn try_new(open_id: impl Into<String>, jwt: impl Into<String>) -> ProviderResult<Self> {
        let mut open_id = open_id.into();
        let mut jwt = jwt.into();
        if !valid_open_id(&open_id) || !valid_jwt(&jwt) {
            open_id.zeroize();
            jwt.zeroize();
            return Err(invalid_credential_shape());
        }
        let expires_at = jwt_expiry(&jwt);
        Ok(Self {
            open_id: SecretString::new(open_id),
            authorization: SecretString::new(jwt),
            expires_at,
            browser_cookie: None,
        })
    }

    pub(crate) fn attach_browser_cookie(
        mut self,
        cookie: Option<SecretString>,
    ) -> ProviderResult<Self> {
        if cookie
            .as_ref()
            .is_some_and(|cookie| !valid_browser_cookie(cookie.expose_secret()))
        {
            return Err(invalid_credential_shape());
        }
        self.browser_cookie = cookie;
        Ok(self)
    }

    /// Parses one encrypted-at-rest Provider composite-session document.
    ///
    /// # Errors
    ///
    /// Returns Authentication for malformed, oversized or extra-field input.
    pub fn try_from_composite(document: &[u8]) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_SESSION_DOCUMENT_BYTES {
            return Err(invalid_credential_shape());
        }
        let mut envelope: StoredSessionEnvelope =
            serde_json::from_slice(document).map_err(|_| invalid_credential_shape())?;
        Self::try_new(
            std::mem::take(&mut envelope.open_id),
            std::mem::take(&mut envelope.jwt),
        )
    }

    /// Exposes the JWT only to the bounded native transport.
    pub fn expose_authorization(&self) -> &str {
        self.authorization.expose_secret()
    }

    /// Exposes the open ID only to bounded account-scoped routes.
    pub fn expose_open_id(&self) -> &str {
        self.open_id.expose_secret()
    }

    pub(crate) fn expose_browser_cookie(&self) -> Option<&str> {
        self.browser_cookie
            .as_ref()
            .map(asterism_secrets::SecretString::expose_secret)
    }

    /// Returns the standard JWT expiry claim when it can be decoded. The
    /// claim is only a conservative lifecycle hint; native user-info remains
    /// the authority for session validity.
    #[must_use]
    pub fn expires_at(&self) -> Option<Timestamp> {
        self.expires_at
    }

    pub(crate) fn to_secret_value(&self) -> ProviderResult<SecretValue> {
        let envelope = BorrowedSessionEnvelope {
            open_id: self.open_id.expose_secret(),
            jwt: self.authorization.expose_secret(),
        };
        serde_json::to_vec(&envelope)
            .map(SecretValue::new)
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "UAI composite session cannot be encoded",
                )
            })
    }
}

impl fmt::Debug for UaiJwtSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiJwtSession([REDACTED])")
    }
}

#[derive(Serialize)]
struct BorrowedSessionEnvelope<'a> {
    #[serde(rename = "openid")]
    open_id: &'a str,
    jwt: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSessionEnvelope {
    #[serde(rename = "openid")]
    open_id: String,
    jwt: String,
}

impl Drop for StoredSessionEnvelope {
    fn drop(&mut self) {
        self.open_id.zeroize();
        self.jwt.zeroize();
    }
}

/// Provider-internal Password exchange and authenticated JWT validation.
#[async_trait]
pub trait UaiAuthenticationTransport: Send + Sync {
    async fn exchange_password(
        &self,
        username: &SecretString,
        password: &SecretString,
    ) -> ProviderResult<UaiJwtSession>;

    async fn validate_jwt(&self, session: &UaiJwtSession) -> ProviderResult<()>;
}

/// Resolves an account-bound stored UAI composite session.
#[async_trait]
pub trait UaiSessionResolver: Send + Sync {
    async fn resolve_session(&self, context: &ProviderContext) -> ProviderResult<UaiJwtSession>;

    async fn renew_session(&self, _context: &ProviderContext) -> ProviderResult<UaiJwtSession> {
        Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI stored session cannot be renewed automatically",
        ))
    }
}

/// UAI Password and `ImportedToken` authentication orchestration.
pub struct UaiAuthentication {
    metadata: ProviderMetadata,
    transport: Arc<dyn UaiAuthenticationTransport>,
    sessions: Arc<dyn UaiSessionResolver>,
}

impl UaiAuthentication {
    /// Creates the capability around injected authentication boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        transport: Arc<dyn UaiAuthenticationTransport>,
        sessions: Arc<dyn UaiSessionResolver>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
            sessions,
        })
    }

    fn validate_context(&self, provider_id: &asterism_domain::ProviderId) -> ProviderResult<()> {
        if provider_id != &self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI authentication received a mismatched Provider context",
            ));
        }
        Ok(())
    }

    async fn validate_password(
        &self,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if credential.session_kind != SessionKind::ProviderSpecific
            || credential.acquired_via != CredentialAcquisition::NativeProviderLogin
            || credential.fields.len() != 2
        {
            return Err(invalid_credential_shape());
        }
        let username = credential_text(credential, SecretPurpose::ProviderUsername)?;
        let password = credential_text(credential, SecretPurpose::ProviderPassword)?;
        validate_login_field(username, MAX_USERNAME_BYTES, true)?;
        validate_login_field(password, MAX_PASSWORD_BYTES, false)?;
        let username_secret = SecretString::new(username.to_owned());
        let password_secret = SecretString::new(password.to_owned());
        let session = self
            .transport
            .exchange_password(&username_secret, &password_secret)
            .await?;
        self.transport.validate_jwt(&session).await?;
        Ok(CredentialValidation {
            status: valid_session(SessionKind::Composite, session.expires_at()),
            replacement: Some(CredentialReplacement {
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
            }),
        })
    }

    async fn validate_imported_token(
        &self,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if credential.session_kind != SessionKind::Jwt
            || !is_imported_session_acquisition(credential.acquired_via)
            || !(credential.fields.len() == 1 || credential.fields.len() == 2)
        {
            return Err(invalid_credential_shape());
        }
        let document = credential_bytes(credential, SecretPurpose::ProviderCompositeSession)?;
        let session = UaiJwtSession::try_from_composite(document)?;
        if credential.fields.len() == 2 {
            let cookie = credential_text(credential, SecretPurpose::ProviderCookie)?;
            if !valid_browser_cookie(cookie) {
                return Err(invalid_credential_shape());
            }
        }
        self.transport.validate_jwt(&session).await?;
        Ok(CredentialValidation::accepted(valid_session(
            SessionKind::Jwt,
            session.expires_at(),
        )))
    }
}

impl fmt::Debug for UaiAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiAuthentication")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiAuthentication {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AuthenticationCapability for UaiAuthentication {
    fn capture_recipe(&self) -> Option<CaptureRecipe> {
        Some(capture_recipe_v3())
    }

    async fn begin_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<AuthChallenge> {
        self.validate_context(&context.provider_id)?;
        let session_id = context.auth_session_id.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI authentication requires a Core AuthSession",
            )
        })?;
        let waiting_for = match method {
            AuthMethod::Password => WaitingUserState::CredentialInput,
            AuthMethod::ImportedToken | AuthMethod::AssistedSession => {
                WaitingUserState::SessionImport
            }
            _ => return Err(unsupported_auth_method()),
        };
        Ok(AuthChallenge {
            session_id,
            method,
            waiting_for,
            user_action: (method == AuthMethod::AssistedSession).then(|| {
                "在打开的 UAI 页面中完成登录并进入任意课程，Capture 将从同一个 ucontent 请求快照读取会话"
                    .to_owned()
            }),
            expires_at: None,
        })
    }

    async fn validate_credential(
        &self,
        context: &ProviderAuthContext,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        self.validate_context(&context.provider_id)?;
        if credential.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "UAI credential belongs to another Provider",
            ));
        }
        match credential.auth_method {
            AuthMethod::Password => self.validate_password(credential).await,
            AuthMethod::ImportedToken | AuthMethod::AssistedSession => {
                self.validate_imported_token(credential).await
            }
            _ => Err(unsupported_auth_method()),
        }
    }

    async fn validate_session(&self, context: &ProviderContext) -> ProviderResult<SessionStatus> {
        self.validate_context(&context.provider_id)?;
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "UAI session validation requires stored credentials",
            ));
        }
        let (session, renewed) = match self.sessions.resolve_session(context).await {
            Ok(session) => (session, false),
            Err(error) if error.kind == ProviderErrorKind::Authentication => {
                (self.sessions.renew_session(context).await?, true)
            }
            Err(error) => return Err(error),
        };
        let session = match self.transport.validate_jwt(&session).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.transport.validate_jwt(&session).await?;
                session
            }
            result => {
                result?;
                session
            }
        };
        Ok(valid_session(SessionKind::Jwt, session.expires_at()))
    }
}

/// Classifies one bounded UAI SSO login response.
///
/// # Errors
///
/// Returns Authentication for ordinary rejection, `HumanRequired` for the
/// donor-observed slider branch, or `InvalidResponse` for malformed success.
pub fn classify_password_login_response(document: &[u8]) -> ProviderResult<UaiJwtSession> {
    if document.is_empty() || document.len() > MAX_LOGIN_RESPONSE_BYTES {
        return Err(invalid_login_response());
    }
    let mut envelope: LoginEnvelope =
        serde_json::from_slice(document).map_err(|_| invalid_login_response())?;
    match envelope.code.as_deref() {
        Some("0") => {
            let mut result = envelope.result.take().ok_or_else(invalid_login_response)?;
            UaiJwtSession::try_new(
                std::mem::take(&mut result.open_id),
                std::mem::take(&mut result.jwt),
            )
            .map_err(|_| invalid_login_response())
        }
        Some("1506") => Err(ProviderError::human_required(
            "UAI password login requires slider verification",
            HumanRequiredReason::ImageCaptcha,
        )),
        Some(_) => Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI rejected the username or password",
        )),
        None => Err(invalid_login_response()),
    }
}

#[derive(Deserialize)]
struct LoginEnvelope {
    #[serde(default)]
    code: Option<String>,
    #[serde(default, rename = "rs")]
    result: Option<LoginResult>,
    #[serde(default)]
    msg: String,
}

impl Drop for LoginEnvelope {
    fn drop(&mut self) {
        self.code.zeroize();
        self.msg.zeroize();
    }
}

#[derive(Deserialize)]
struct LoginResult {
    #[serde(default, rename = "openid")]
    open_id: String,
    #[serde(default)]
    jwt: String,
}

impl Drop for LoginResult {
    fn drop(&mut self) {
        self.open_id.zeroize();
        self.jwt.zeroize();
    }
}

fn credential_text(credential: &CredentialBundle, purpose: SecretPurpose) -> ProviderResult<&str> {
    let bytes = credential_bytes(credential, purpose)?;
    std::str::from_utf8(bytes).map_err(|_| invalid_credential_shape())
}

fn credential_bytes(
    credential: &CredentialBundle,
    purpose: SecretPurpose,
) -> ProviderResult<&[u8]> {
    let mut matches = credential
        .fields
        .iter()
        .filter(|field| field.purpose == purpose);
    let field = matches.next().ok_or_else(invalid_credential_shape)?;
    if matches.next().is_some() {
        return Err(invalid_credential_shape());
    }
    Ok(field.value.expose_secret())
}

pub(crate) fn validate_login_field(value: &str, maximum: usize, trim: bool) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(char::is_control)
        || (trim && value.trim() != value)
    {
        return Err(invalid_credential_shape());
    }
    Ok(())
}

fn valid_open_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPEN_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_jwt(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_JWT_BYTES || value.chars().any(char::is_control) {
        return false;
    }
    let mut segments = value.split('.');
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    matches!(
        (segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(header), Some(payload), Some(signature), None)
            if valid_segment(header) && valid_segment(payload) && valid_segment(signature)
    )
}

pub(crate) fn valid_browser_cookie(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BROWSER_COOKIE_BYTES
        && value.is_ascii()
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn jwt_expiry(value: &str) -> Option<Timestamp> {
    let payload = value.split('.').nth(1)?;
    let mut decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims = serde_json::from_slice::<JwtExpiryClaims>(&decoded).ok();
    decoded.zeroize();
    claims?
        .exp
        .and_then(|seconds| Timestamp::from_timestamp(seconds, 0))
}

#[derive(Deserialize)]
struct JwtExpiryClaims {
    #[serde(default)]
    exp: Option<i64>,
}

fn valid_session(kind: SessionKind, expires_at: Option<Timestamp>) -> SessionStatus {
    SessionStatus {
        valid: true,
        kind,
        expires_at,
        account_hint: None,
    }
}

fn unsupported_auth_method() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI received an authentication method it does not advertise",
    )
}

fn invalid_credential_shape() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "UAI credential fields do not match the selected authentication method",
    )
}

fn invalid_login_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI login endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{AuthSessionId, ProviderAccountId, ProviderId, SecretId};
    use chrono::Utc;

    use super::*;

    const LOGIN_SUCCESS: &[u8] =
        include_bytes!("../../../fixtures/providers/uai/auth/password-success.json");
    const LOGIN_REJECTED: &[u8] =
        include_bytes!("../../../fixtures/providers/uai/auth/password-rejected.json");
    const LOGIN_CAPTCHA: &[u8] =
        include_bytes!("../../../fixtures/providers/uai/auth/password-captcha.json");

    #[derive(Debug, Default)]
    struct FixtureBoundaries {
        exchanges: AtomicUsize,
        validations: AtomicUsize,
    }

    #[async_trait]
    impl UaiAuthenticationTransport for FixtureBoundaries {
        async fn exchange_password(
            &self,
            username: &SecretString,
            password: &SecretString,
        ) -> ProviderResult<UaiJwtSession> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            assert_eq!(username.expose_secret(), "test-user");
            assert_eq!(password.expose_secret(), "test-password");
            UaiJwtSession::try_new(
                "synthetic-open-id",
                "SAFE_HEADER.SAFE_PAYLOAD.SAFE_SIGNATURE",
            )
        }

        async fn validate_jwt(&self, session: &UaiJwtSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            assert!(session.expose_authorization().contains('.'));
            assert!(session.expose_open_id().ends_with("-open-id"));
            Ok(())
        }
    }

    #[async_trait]
    impl UaiSessionResolver for FixtureBoundaries {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<UaiJwtSession> {
            UaiJwtSession::try_new(
                "stored-open-id",
                "STORED_HEADER.STORED_PAYLOAD.STORED_SIGNATURE",
            )
        }
    }

    #[test]
    fn login_outcomes_are_typed_and_redacted() {
        let session = classify_password_login_response(LOGIN_SUCCESS).unwrap();
        assert_eq!(session.expose_open_id(), "synthetic-open-id");
        assert!(!format!("{session:?}").contains("SAFE_HEADER"));

        let rejected = classify_password_login_response(LOGIN_REJECTED).unwrap_err();
        assert_eq!(rejected.kind, ProviderErrorKind::Authentication);
        assert!(!rejected.message.contains("synthetic detail"));

        let captcha = classify_password_login_response(LOGIN_CAPTCHA).unwrap_err();
        assert_eq!(captcha.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(
            captcha.human_required_reason,
            Some(HumanRequiredReason::ImageCaptcha)
        );
    }

    #[test]
    fn composite_session_is_strict_bounded_and_redacted() {
        let session = UaiJwtSession::try_from_composite(
            br#"{"openid":"synthetic-open-id","jwt":"SAFE_HEADER.SAFE_PAYLOAD.SAFE_SIGNATURE"}"#,
        )
        .unwrap();
        assert_eq!(session.expose_open_id(), "synthetic-open-id");
        assert!(!format!("{session:?}").contains("synthetic"));
        assert!(UaiJwtSession::try_from_composite(br#"{"jwt":"a.b.c"}"#).is_err());
        assert!(
            UaiJwtSession::try_from_composite(br#"{"openid":"id","jwt":"a.b.c","extra":"value"}"#)
                .is_err()
        );
        assert!(UaiJwtSession::try_new("bad/open", "a.b.c").is_err());
        assert!(UaiJwtSession::try_new("open", "not-a-jwt").is_err());
    }

    #[test]
    fn standard_jwt_expiry_is_a_bounded_session_hint_only() {
        let expiry_seconds = 4_102_444_800;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{expiry_seconds}}}"#));
        let session = UaiJwtSession::try_new(
            "synthetic-open-id",
            format!("{header}.{payload}.synthetic-signature"),
        )
        .unwrap();
        let expires_at = Timestamp::from_timestamp(expiry_seconds, 0).unwrap();
        assert_eq!(session.expires_at(), Some(expires_at));
        assert_eq!(
            valid_session(SessionKind::Jwt, session.expires_at()).expires_at,
            Some(expires_at)
        );

        let opaque = UaiJwtSession::try_new(
            "synthetic-open-id",
            "SAFE_HEADER.SAFE_PAYLOAD.SAFE_SIGNATURE",
        )
        .unwrap();
        assert_eq!(opaque.expires_at(), None);
    }

    #[test]
    fn capture_recipe_builds_atomic_openid_jwt_and_cookie_outputs() {
        let recipe = capture_recipe_v3();
        recipe.validate().unwrap();
        assert_eq!(recipe.start_url, CAPTURE_START_URL);
        assert_eq!(recipe.auth_method, AuthMethod::AssistedSession);
        assert_eq!(recipe.session_kind, SessionKind::Jwt);
        assert_eq!(
            recipe.navigation_origins,
            [UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()]
        );
        assert_eq!(recipe.read_origins, [UCONTENT_ORIGIN.to_owned()]);
        assert_eq!(recipe.readiness, CaptureReadiness::OutputsComplete);
        assert_eq!(recipe.outputs.len(), 2);
        assert_eq!(
            recipe.outputs[0].purpose,
            SecretPurpose::ProviderCompositeSession
        );
        assert!(recipe.outputs[0].required);
        assert_eq!(
            recipe.outputs[0].sources,
            [CaptureValueSource::JsonObject {
                fields: vec![
                    CaptureJsonField {
                        name: "openid".to_owned(),
                        sources: vec![CaptureScalarSource::RequestHeader {
                            origin: UCONTENT_ORIGIN.to_owned(),
                            name: "u-openid".to_owned(),
                        }],
                    },
                    CaptureJsonField {
                        name: "jwt".to_owned(),
                        sources: vec![CaptureScalarSource::RequestHeader {
                            origin: UCONTENT_ORIGIN.to_owned(),
                            name: "Authorization".to_owned(),
                        }],
                    },
                ],
            }]
        );
        assert_eq!(recipe.outputs[1].purpose, SecretPurpose::ProviderCookie);
        assert!(recipe.outputs[1].required);
        assert_eq!(
            recipe.outputs[1].sources,
            [CaptureValueSource::CookieHeader {
                origin: UCONTENT_ORIGIN.to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn capability_validates_password_import_and_stored_session() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let authentication =
            UaiAuthentication::try_new(boundaries.clone(), boundaries.clone()).unwrap();

        let password = authentication
            .validate_credential(&auth_context(), &password_bundle())
            .await
            .unwrap();
        assert_eq!(password.status.kind, SessionKind::Composite);
        let replacement = password.replacement.unwrap();
        assert_eq!(replacement.fields.len(), 3);
        assert_eq!(
            replacement.fields[2].purpose,
            SecretPurpose::ProviderCompositeSession
        );
        let replaced =
            UaiJwtSession::try_from_composite(replacement.fields[2].value.expose_secret()).unwrap();
        assert_eq!(replaced.expose_open_id(), "synthetic-open-id");

        for acquisition in [
            CredentialAcquisition::ManualImport,
            CredentialAcquisition::CaptureTool,
            CredentialAcquisition::BrowserExtension,
        ] {
            let imported = authentication
                .validate_credential(&auth_context(), &imported_bundle(acquisition))
                .await
                .unwrap();
            assert_eq!(imported.status.kind, SessionKind::Jwt);
            assert!(imported.replacement.is_none());
        }

        let captured = authentication
            .validate_credential(&auth_context(), &captured_bundle())
            .await
            .unwrap();
        assert_eq!(captured.status.kind, SessionKind::Jwt);
        assert!(captured.replacement.is_none());

        let stored = authentication
            .validate_session(&provider_context())
            .await
            .unwrap();
        assert_eq!(stored.kind, SessionKind::Jwt);
        assert_eq!(boundaries.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(boundaries.validations.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn password_candidate_and_replacement_satisfy_core_metadata_gate() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let authentication = UaiAuthentication::try_new(boundaries.clone(), boundaries).unwrap();
        let candidate = password_bundle();

        assert!(
            authentication
                .metadata
                .auth_methods
                .contains(&candidate.auth_method)
        );
        assert!(
            authentication
                .metadata
                .session_kinds
                .contains(&candidate.session_kind)
        );
        let validation = authentication
            .validate_credential(&auth_context(), &candidate)
            .await
            .unwrap();
        let replacement = validation.replacement.unwrap();
        assert_eq!(validation.status.kind, replacement.session_kind);
        assert!(
            authentication
                .metadata
                .session_kinds
                .contains(&replacement.session_kind)
        );
    }

    #[tokio::test]
    async fn challenges_and_credential_shapes_are_explicit() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let authentication = UaiAuthentication::try_new(boundaries.clone(), boundaries).unwrap();
        assert_eq!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::Password)
                .await
                .unwrap()
                .waiting_for,
            WaitingUserState::CredentialInput
        );
        assert_eq!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::ImportedToken)
                .await
                .unwrap()
                .waiting_for,
            WaitingUserState::SessionImport
        );
        assert_eq!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::AssistedSession)
                .await
                .unwrap()
                .waiting_for,
            WaitingUserState::SessionImport
        );
        assert!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::AssistedSession)
                .await
                .unwrap()
                .user_action
                .is_some()
        );
        assert!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::ImportedCookie)
                .await
                .is_err()
        );
        let mut malformed = password_bundle();
        malformed.fields.pop();
        assert!(
            authentication
                .validate_credential(&auth_context(), &malformed)
                .await
                .is_err()
        );
    }

    fn auth_context() -> ProviderAuthContext {
        ProviderAuthContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            auth_session_id: Some(AuthSessionId::new()),
            correlation_id: "uai-auth-test".to_owned(),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-session-test".to_owned(),
        }
    }

    fn password_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("uai").unwrap(),
            tenant: None,
            auth_method: AuthMethod::Password,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::ProviderSpecific,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderUsername,
                    value: SecretValue::new(b"test-user".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderPassword,
                    value: SecretValue::new(b"test-password".to_vec()),
                },
            ],
            user_id_hint: None,
        }
    }

    fn imported_bundle(acquired_via: CredentialAcquisition) -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("uai").unwrap(),
            tenant: None,
            auth_method: AuthMethod::ImportedToken,
            acquired_via,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::Jwt,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCompositeSession,
                value: SecretValue::new(
                    br#"{"openid":"synthetic-open-id","jwt":"SAFE_HEADER.SAFE_PAYLOAD.SAFE_SIGNATURE"}"#
                        .to_vec(),
                ),
            }],
            user_id_hint: None,
        }
    }

    fn captured_bundle() -> CredentialBundle {
        let mut bundle = imported_bundle(CredentialAcquisition::CaptureTool);
        bundle.auth_method = AuthMethod::AssistedSession;
        bundle.fields.push(CredentialField {
            purpose: SecretPurpose::ProviderCookie,
            value: SecretValue::new(b"session=synthetic; csrf=synthetic".to_vec()),
        });
        bundle
    }
}
