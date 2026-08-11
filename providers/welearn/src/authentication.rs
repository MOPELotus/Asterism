use std::{collections::BTreeSet, fmt, fmt::Write as _, sync::Arc};

use asterism_domain::{AuthMethod, HumanRequiredReason, SessionKind, WaitingUserState};
use asterism_provider_api::{
    AuthChallenge, AuthenticationCapability, CredentialReplacement, CredentialValidation,
    ProviderAuthContext, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, SessionStatus,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, CredentialField, SecretPurpose, SecretString,
    SecretValue,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Url, header::HeaderValue};
use serde::Deserialize;
use zeroize::Zeroize;

use crate::metadata::development_metadata;

const MAX_LOGIN_RESPONSE_BYTES: usize = 64 * 1_024;
pub(crate) const MAX_PASSWORD_BYTES: usize = 4 * 1_024;
const MAX_LOGIN_REDIRECT_BYTES: usize = 8 * 1_024;
pub(crate) const MAX_USERNAME_BYTES: usize = 512;
const MAX_COOKIE_BYTES: usize = 64 * 1_024;
const MAX_COOKIE_FIELDS: usize = 128;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const SSO_ORIGIN: &str = "https://sso.sflep.com";
const SSO_CALLBACK_PREFIX: &str = "/idsvr/";

/// A bounded `WELearn` Cookie header. Plaintext is redacted and zeroized.
pub struct WellearnCookieSession(SecretString);

impl WellearnCookieSession {
    /// Validates an imported or newly exchanged Cookie header.
    ///
    /// # Errors
    ///
    /// Returns an authentication error for empty, duplicate, malformed or
    /// oversized Cookie fields.
    pub fn try_new(cookie: impl Into<String>) -> ProviderResult<Self> {
        let mut cookie = cookie.into();
        if cookie.is_empty()
            || cookie.len() > MAX_COOKIE_BYTES
            || cookie.chars().any(char::is_control)
        {
            cookie.zeroize();
            return Err(invalid_credential_shape());
        }
        let valid_header = HeaderValue::from_str(&cookie).is_ok();
        let mut names = BTreeSet::new();
        let mut field_count = 0_usize;
        let valid_fields = cookie.split(';').all(|field| {
            field_count = field_count.saturating_add(1);
            let Some((name, value)) = field.trim().split_once('=') else {
                return false;
            };
            valid_cookie_name(name)
                && !value.is_empty()
                && !value.chars().any(char::is_control)
                && names.insert(name.to_owned())
        });
        if !valid_header || !valid_fields || field_count == 0 || field_count > MAX_COOKIE_FIELDS {
            cookie.zeroize();
            return Err(invalid_credential_shape());
        }
        Ok(Self(SecretString::new(cookie)))
    }

    /// Exposes the Cookie only to the bounded `WELearn` transport.
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for WellearnCookieSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WellearnCookieSession([REDACTED])")
    }
}

/// Provider-internal boundary for password exchange and authenticated session
/// validation. Native HTTP and test transports implement the same contract.
#[async_trait]
pub trait WellearnAuthenticationTransport: Send + Sync {
    async fn exchange_password(
        &self,
        username: &SecretString,
        password: &SecretString,
    ) -> ProviderResult<WellearnCookieSession>;

    async fn validate_cookie(&self, session: &WellearnCookieSession) -> ProviderResult<()>;
}

/// Resolves an account-bound stored Cookie without exposing persistence to the
/// Provider capability.
#[async_trait]
pub trait WellearnSessionResolver: Send + Sync {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnCookieSession>;

    async fn renew_session(
        &self,
        _context: &ProviderContext,
    ) -> ProviderResult<WellearnCookieSession> {
        Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn stored session cannot be renewed automatically",
        ))
    }
}

/// `WELearn` Password, `ImportedCookie` and Capture-assisted Cookie
/// authentication orchestration.
pub struct WellearnAuthentication {
    metadata: ProviderMetadata,
    transport: Arc<dyn WellearnAuthenticationTransport>,
    sessions: Arc<dyn WellearnSessionResolver>,
}

impl WellearnAuthentication {
    /// Creates the capability around injected authentication boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        transport: Arc<dyn WellearnAuthenticationTransport>,
        sessions: Arc<dyn WellearnSessionResolver>,
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
                "WELearn authentication received a mismatched Provider context",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnAuthentication")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnAuthentication {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AuthenticationCapability for WellearnAuthentication {
    async fn begin_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<AuthChallenge> {
        self.validate_context(&context.provider_id)?;
        let session_id = context.auth_session_id.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn authentication requires a Core AuthSession",
            )
        })?;
        let waiting_for = match method {
            AuthMethod::Password => WaitingUserState::CredentialInput,
            AuthMethod::ImportedCookie | AuthMethod::AssistedSession => {
                WaitingUserState::SessionImport
            }
            _ => return Err(unsupported_auth_method()),
        };
        Ok(AuthChallenge {
            session_id,
            method,
            waiting_for,
            user_action: None,
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
                "WELearn credential belongs to another Provider",
            ));
        }
        match credential.auth_method {
            AuthMethod::Password => self.validate_password(credential).await,
            AuthMethod::ImportedCookie => self.validate_imported_cookie(credential).await,
            AuthMethod::AssistedSession => self.validate_captured_cookie(credential).await,
            _ => Err(unsupported_auth_method()),
        }
    }

    async fn validate_session(&self, context: &ProviderContext) -> ProviderResult<SessionStatus> {
        self.validate_context(&context.provider_id)?;
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "WELearn session validation requires stored credentials",
            ));
        }
        let (session, renewed) = match self.sessions.resolve_session(context).await {
            Ok(session) => (session, false),
            Err(error) if error.kind == ProviderErrorKind::Authentication => {
                (self.sessions.renew_session(context).await?, true)
            }
            Err(error) => return Err(error),
        };
        match self.transport.validate_cookie(&session).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.transport.validate_cookie(&session).await?;
            }
            result => result?,
        }
        Ok(valid_session(SessionKind::Cookie))
    }
}

impl WellearnAuthentication {
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
        self.transport.validate_cookie(&session).await?;
        Ok(CredentialValidation {
            status: valid_session(SessionKind::Composite),
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
                        purpose: SecretPurpose::ProviderCookie,
                        value: SecretValue::new(session.expose_secret().as_bytes().to_vec()),
                    },
                ],
            }),
        })
    }

    async fn validate_imported_cookie(
        &self,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if credential.session_kind != SessionKind::Cookie || credential.fields.len() != 1 {
            return Err(invalid_credential_shape());
        }
        let cookie = credential_text(credential, SecretPurpose::ProviderCookie)?;
        let session = WellearnCookieSession::try_new(cookie.to_owned())?;
        self.transport.validate_cookie(&session).await?;
        Ok(CredentialValidation::accepted(valid_session(
            SessionKind::Cookie,
        )))
    }

    async fn validate_captured_cookie(
        &self,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if !matches!(
            credential.acquired_via,
            CredentialAcquisition::CaptureTool | CredentialAcquisition::BrowserExtension
        ) {
            return Err(invalid_credential_shape());
        }
        self.validate_imported_cookie(credential).await
    }
}

fn credential_text(credential: &CredentialBundle, purpose: SecretPurpose) -> ProviderResult<&str> {
    let mut matches = credential
        .fields
        .iter()
        .filter(|field| field.purpose == purpose);
    let field = matches.next().ok_or_else(invalid_credential_shape)?;
    if matches.next().is_some() {
        return Err(invalid_credential_shape());
    }
    std::str::from_utf8(field.value.expose_secret()).map_err(|_| invalid_credential_shape())
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

fn valid_session(kind: SessionKind) -> SessionStatus {
    SessionStatus {
        valid: true,
        kind,
        expires_at: None,
        account_hint: None,
    }
}

pub(crate) fn valid_cookie_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COOKIE_NAME_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn unsupported_auth_method() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn received an authentication method it does not advertise",
    )
}

fn invalid_credential_shape() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "WELearn credential fields do not match the selected authentication method",
    )
}

/// Timestamp-bound password fields expected by the current SSO login form.
pub struct WellearnPasswordCipher {
    encoded: SecretString,
    timestamp_milliseconds: u64,
}

impl WellearnPasswordCipher {
    /// Exposes the encoded form value only to the bounded login transport.
    pub fn encoded(&self) -> &str {
        self.encoded.expose_secret()
    }

    pub const fn timestamp_milliseconds(&self) -> u64 {
        self.timestamp_milliseconds
    }
}

impl fmt::Debug for WellearnPasswordCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnPasswordCipher")
            .field("encoded", &"[REDACTED]")
            .field("timestamp_milliseconds", &self.timestamp_milliseconds)
            .finish()
    }
}

/// A validated SSO callback route. Query material is redacted and zeroized.
pub struct WellearnLoginRedirect(SecretString);

impl WellearnLoginRedirect {
    /// Exposes the validated callback only to the bounded redirect transport.
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for WellearnLoginRedirect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WellearnLoginRedirect([REDACTED])")
    }
}

/// Reproduces the timestamp-bound encoding performed by the current SSO form.
///
/// This function accepts an explicit timestamp so callers can bind form `pwd`
/// and `ts` to the same instant and tests can remain deterministic.
///
/// # Errors
///
/// Returns an authentication error for an empty, control-bearing or oversized
/// password, and an internal error if the bounded encoding cannot be built.
pub fn encode_password_at(
    password: &str,
    epoch_milliseconds: u64,
) -> ProviderResult<WellearnPasswordCipher> {
    if password.is_empty()
        || password.len() > MAX_PASSWORD_BYTES
        || password.chars().any(char::is_control)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn password credential has an invalid shape",
        ));
    }
    let bytes = password.as_bytes();
    let mut checksum = ((epoch_milliseconds >> 16) & 0xff) as u8;
    for byte in bytes {
        checksum ^= byte;
    }
    let adjusted = (epoch_milliseconds / 100)
        .checked_mul(100)
        .and_then(|value| value.checked_add(u64::from(checksum % 100)))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn password timestamp cannot be encoded",
            )
        })?;
    let mut payload = format!("{adjusted}*");
    for byte in bytes {
        write!(&mut payload, "{byte:02x}").map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn password encoding failed",
            )
        })?;
    }
    let encoded = STANDARD.encode(payload.as_bytes());
    payload.zeroize();
    Ok(WellearnPasswordCipher {
        encoded: SecretString::new(encoded),
        timestamp_milliseconds: adjusted,
    })
}

/// Classifies one bounded `idsvr/account/login` JSON response.
///
/// Success returns only a strict, redacted `sso.sflep.com/idsvr/` callback.
/// Password rejection, SMS verification and image captcha remain distinct.
///
/// # Errors
///
/// Returns a typed authentication, human-required or invalid-response error.
pub fn classify_password_login_response(document: &[u8]) -> ProviderResult<WellearnLoginRedirect> {
    if document.is_empty() || document.len() > MAX_LOGIN_RESPONSE_BYTES {
        return Err(invalid_login_response());
    }
    let mut envelope: LoginEnvelope =
        serde_json::from_slice(document).map_err(|_| invalid_login_response())?;
    match envelope.code {
        Some(0) => {
            let redirect = std::mem::take(&mut envelope.data);
            validate_login_redirect(redirect)
        }
        Some(_) => Err(classify_login_rejection(&envelope)),
        None => Err(invalid_login_response()),
    }
}

#[derive(Deserialize)]
struct LoginEnvelope {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    data: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    message: String,
    #[serde(default, rename = "extraCheck")]
    extra_check: LoginExtraCheck,
}

impl Drop for LoginEnvelope {
    fn drop(&mut self) {
        self.data.zeroize();
        self.msg.zeroize();
        self.message.zeroize();
    }
}

#[derive(Default, Deserialize)]
struct LoginExtraCheck {
    #[serde(default, rename = "vcToken")]
    verification_token: String,
}

impl Drop for LoginExtraCheck {
    fn drop(&mut self) {
        self.verification_token.zeroize();
    }
}

fn validate_login_redirect(mut value: String) -> ProviderResult<WellearnLoginRedirect> {
    if value.is_empty()
        || value.len() > MAX_LOGIN_REDIRECT_BYTES
        || value.chars().any(char::is_control)
    {
        value.zeroize();
        return Err(invalid_login_response());
    }
    if value.starts_with('/') && !value.starts_with("//") {
        value.insert_str(0, "https://sso.sflep.com/idsvr");
    }
    let Ok(url) = Url::parse(&value) else {
        value.zeroize();
        return Err(invalid_login_response());
    };
    let valid = url.scheme() == "https"
        && url.host_str() == Some("sso.sflep.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with(SSO_CALLBACK_PREFIX)
        && url.query().is_some()
        && url.fragment().is_none()
        && value.starts_with(SSO_ORIGIN);
    if !valid {
        value.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn login returned an untrusted SSO callback route",
        ));
    }
    Ok(WellearnLoginRedirect(SecretString::new(value)))
}

fn classify_login_rejection(envelope: &LoginEnvelope) -> ProviderError {
    if !envelope.extra_check.verification_token.is_empty()
        || contains_any_case_insensitive(
            [&envelope.msg, &envelope.message],
            &["短信", "手机", "二次验证", "sms", "verification code"],
        )
    {
        ProviderError::human_required(
            "WELearn password login requires SMS or secondary verification",
            HumanRequiredReason::SmsVerification,
        )
    } else if contains_any_case_insensitive(
        [&envelope.msg, &envelope.message],
        &["验证码", "图片", "captcha"],
    ) {
        ProviderError::human_required(
            "WELearn password login requires an image captcha",
            HumanRequiredReason::ImageCaptcha,
        )
    } else {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn rejected the username or password",
        )
    }
}

fn contains_any_case_insensitive<const N: usize>(values: [&str; N], needles: &[&str]) -> bool {
    values.into_iter().any(|value| {
        let lowercase = value.to_ascii_lowercase();
        needles
            .iter()
            .any(|needle| lowercase.contains(&needle.to_ascii_lowercase()))
    })
}

fn invalid_login_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "WELearn login endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{AuthSessionId, ProviderAccountId, ProviderId, SecretId};
    use chrono::Utc;

    use super::*;

    const LOGIN_SUCCESS: &[u8] =
        include_bytes!("../../../fixtures/providers/welearn/auth/password-success.json");
    const LOGIN_REJECTED: &[u8] =
        include_bytes!("../../../fixtures/providers/welearn/auth/password-rejected.json");
    const LOGIN_CAPTCHA: &[u8] =
        include_bytes!("../../../fixtures/providers/welearn/auth/password-captcha.json");
    const LOGIN_SMS: &[u8] =
        include_bytes!("../../../fixtures/providers/welearn/auth/password-sms.json");

    #[derive(Debug)]
    struct FixtureTransport {
        exchanges: AtomicUsize,
        validations: AtomicUsize,
    }

    #[async_trait]
    impl WellearnAuthenticationTransport for FixtureTransport {
        async fn exchange_password(
            &self,
            username: &SecretString,
            password: &SecretString,
        ) -> ProviderResult<WellearnCookieSession> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            assert_eq!(username.expose_secret(), "test-user");
            assert_eq!(password.expose_secret(), "test-password");
            WellearnCookieSession::try_new("WELearn=SAFE_LOGIN; Session=SAFE_SESSION")
        }

        async fn validate_cookie(&self, session: &WellearnCookieSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            assert!(session.expose_secret().contains('='));
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FixtureSessions;

    #[async_trait]
    impl WellearnSessionResolver for FixtureSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnCookieSession> {
            WellearnCookieSession::try_new("WELearn=SAFE_STORED")
        }
    }

    #[derive(Debug, Default)]
    struct FixtureRenewingSessions {
        renewals: AtomicUsize,
    }

    #[async_trait]
    impl WellearnSessionResolver for FixtureRenewingSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnCookieSession> {
            WellearnCookieSession::try_new("session=OLD")
        }

        async fn renew_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnCookieSession> {
            self.renewals.fetch_add(1, Ordering::SeqCst);
            WellearnCookieSession::try_new("session=NEW")
        }
    }

    #[derive(Debug, Default)]
    struct RejectOldTransport {
        validations: AtomicUsize,
    }

    #[async_trait]
    impl WellearnAuthenticationTransport for RejectOldTransport {
        async fn exchange_password(
            &self,
            _username: &SecretString,
            _password: &SecretString,
        ) -> ProviderResult<WellearnCookieSession> {
            unreachable!("session validation does not exchange a password directly")
        }

        async fn validate_cookie(&self, session: &WellearnCookieSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            if session.expose_secret() == "session=OLD" {
                Err(ProviderError::new(
                    ProviderErrorKind::Authentication,
                    "fixture old session expired",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn password_encoding_matches_the_audited_form_algorithm() {
        let cipher = encode_password_at("test-password", 1_760_000_000_123).unwrap();
        assert_eq!(
            cipher.encoded(),
            "MTc2MDAwMDAwMDEwOCo3NDY1NzM3NDJkNzA2MTczNzM3NzZmNzI2NA=="
        );
        assert_eq!(cipher.timestamp_milliseconds(), 1_760_000_000_108);
        assert!(!format!("{cipher:?}").contains("MTc2"));
    }

    #[test]
    fn login_outcomes_are_typed_without_leaking_response_details() {
        let success = classify_password_login_response(LOGIN_SUCCESS).unwrap();
        assert!(
            success
                .expose_secret()
                .starts_with("https://sso.sflep.com/idsvr/connect/authorize/callback?")
        );
        assert!(!format!("{success:?}").contains("SAFE_CODE"));

        let rejected = classify_password_login_response(LOGIN_REJECTED).unwrap_err();
        assert_eq!(rejected.kind, ProviderErrorKind::Authentication);
        assert!(!rejected.message.contains("synthetic account detail"));

        let captcha = classify_password_login_response(LOGIN_CAPTCHA).unwrap_err();
        assert_eq!(captcha.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(
            captcha.human_required_reason,
            Some(HumanRequiredReason::ImageCaptcha)
        );

        let sms = classify_password_login_response(LOGIN_SMS).unwrap_err();
        assert_eq!(sms.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(
            sms.human_required_reason,
            Some(HumanRequiredReason::SmsVerification)
        );
        assert!(!format!("{sms:?}").contains("SAFE_VC_TOKEN"));
    }

    #[test]
    fn login_success_requires_a_strict_sso_callback() {
        for invalid in [
            br#"{"code":0,"data":"https://evil.example/idsvr/callback?code=x"}"#.as_slice(),
            br#"{"code":0,"data":"https://user@sso.sflep.com/idsvr/callback?code=x"}"#,
            br#"{"code":0,"data":"//evil.example/callback?code=x"}"#,
            br#"{"code":0,"data":"/connect/callback"}"#,
            br#"{"data":"/connect/callback?code=x"}"#,
        ] {
            assert!(classify_password_login_response(invalid).is_err());
        }
    }

    #[test]
    fn password_shape_is_bounded() {
        assert!(encode_password_at("", 1).is_err());
        assert!(encode_password_at("bad\npassword", 1).is_err());
        assert!(encode_password_at(&"x".repeat(MAX_PASSWORD_BYTES + 1), 1).is_err());
    }

    #[test]
    fn cookie_session_is_bounded_and_redacted() {
        let session = WellearnCookieSession::try_new("WELearn=SAFE; Session=VALUE").unwrap();
        assert_eq!(session.expose_secret(), "WELearn=SAFE; Session=VALUE");
        assert!(!format!("{session:?}").contains("SAFE"));
        assert!(WellearnCookieSession::try_new("").is_err());
        assert!(WellearnCookieSession::try_new("WELearn=one; WELearn=two").is_err());
        assert!(WellearnCookieSession::try_new("Secure").is_err());
    }

    #[tokio::test]
    async fn capability_validates_native_imported_and_capture_cookie_paths() {
        let transport = Arc::new(FixtureTransport {
            exchanges: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        });
        let authentication =
            WellearnAuthentication::try_new(transport.clone(), Arc::new(FixtureSessions)).unwrap();

        let validation = authentication
            .validate_credential(&auth_context(), &password_bundle())
            .await
            .unwrap();
        assert_eq!(validation.status.kind, SessionKind::Composite);
        let replacement = validation.replacement.unwrap();
        assert_eq!(replacement.session_kind, SessionKind::Composite);
        assert_eq!(replacement.fields.len(), 3);
        assert_eq!(replacement.fields[2].purpose, SecretPurpose::ProviderCookie);
        assert_eq!(
            replacement.fields[2].value.expose_secret(),
            b"WELearn=SAFE_LOGIN; Session=SAFE_SESSION"
        );

        let imported = authentication
            .validate_credential(&auth_context(), &cookie_bundle())
            .await
            .unwrap();
        assert_eq!(imported.status.kind, SessionKind::Cookie);
        assert!(imported.replacement.is_none());
        let captured = authentication
            .validate_credential(&auth_context(), &captured_cookie_bundle())
            .await
            .unwrap();
        assert_eq!(captured.status.kind, SessionKind::Cookie);
        let mut mislabeled = captured_cookie_bundle();
        mislabeled.acquired_via = CredentialAcquisition::ManualImport;
        assert!(
            authentication
                .validate_credential(&auth_context(), &mislabeled)
                .await
                .is_err()
        );
        assert_eq!(transport.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn challenge_and_stored_session_paths_are_explicit() {
        let transport = Arc::new(FixtureTransport {
            exchanges: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        });
        let authentication =
            WellearnAuthentication::try_new(transport.clone(), Arc::new(FixtureSessions)).unwrap();
        let challenge = authentication
            .begin_authentication(&auth_context(), AuthMethod::Password)
            .await
            .unwrap();
        assert_eq!(challenge.waiting_for, WaitingUserState::CredentialInput);
        let imported = authentication
            .begin_authentication(&auth_context(), AuthMethod::ImportedCookie)
            .await
            .unwrap();
        assert_eq!(imported.waiting_for, WaitingUserState::SessionImport);
        let captured = authentication
            .begin_authentication(&auth_context(), AuthMethod::AssistedSession)
            .await
            .unwrap();
        assert_eq!(captured.waiting_for, WaitingUserState::SessionImport);
        assert!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::QrCode)
                .await
                .is_err()
        );

        let status = authentication
            .validate_session(&provider_context())
            .await
            .unwrap();
        assert_eq!(status.kind, SessionKind::Cookie);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stored_session_validation_renews_once_after_authentication_failure() {
        let transport = Arc::new(RejectOldTransport::default());
        let sessions = Arc::new(FixtureRenewingSessions::default());
        let authentication =
            WellearnAuthentication::try_new(transport.clone(), sessions.clone()).unwrap();
        let status = authentication
            .validate_session(&provider_context())
            .await
            .unwrap();
        assert_eq!(status.kind, SessionKind::Cookie);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 2);
        assert_eq!(sessions.renewals.load(Ordering::SeqCst), 1);
    }

    fn auth_context() -> ProviderAuthContext {
        ProviderAuthContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            auth_session_id: Some(AuthSessionId::new()),
            correlation_id: "auth-correlation".to_owned(),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "session-correlation".to_owned(),
        }
    }

    fn password_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("welearn").unwrap(),
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

    fn cookie_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("welearn").unwrap(),
            tenant: None,
            auth_method: AuthMethod::ImportedCookie,
            acquired_via: CredentialAcquisition::ManualImport,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::Cookie,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(b"WELearn=SAFE_IMPORTED".to_vec()),
            }],
            user_id_hint: None,
        }
    }

    fn captured_cookie_bundle() -> CredentialBundle {
        let mut credential = cookie_bundle();
        credential.auth_method = AuthMethod::AssistedSession;
        credential.acquired_via = CredentialAcquisition::CaptureTool;
        credential
    }
}
