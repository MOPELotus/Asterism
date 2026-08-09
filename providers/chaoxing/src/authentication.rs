use std::{collections::BTreeMap, fmt, sync::Arc};

use aes::Aes128;
use asterism_domain::{AuthMethod, HumanRequiredReason, SessionKind, WaitingUserState};
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
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
use cbc::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use reqwest::{
    Client, Response, Url,
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, SET_COOKIE},
};
use serde::Deserialize;
use zeroize::Zeroize;

use crate::{
    ChaoxingCookieSession, ChaoxingSessionResolver,
    metadata::development_metadata,
    native_http::{classify_reqwest_error, fetch_course_list_html, validate_response_status},
};

const LOGIN_URL: &str = "https://passport2.chaoxing.com/fanyalogin";
const LOGIN_KEY: &[u8; 16] = b"u2oh6Vu^HWe4_AES";
const MAX_LOGIN_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_USERNAME_BYTES: usize = 512;
const MAX_PASSWORD_BYTES: usize = 4 * 1_024;
const MAX_RESPONSE_COOKIES: usize = 128;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const MAX_COOKIE_VALUE_BYTES: usize = 8 * 1_024;

/// Provider-internal boundary for the native password exchange and Cookie
/// validation requests.
#[async_trait]
pub trait ChaoxingAuthenticationTransport: Send + Sync {
    async fn exchange_password(
        &self,
        encrypted_username: &SecretString,
        encrypted_password: &SecretString,
    ) -> ProviderResult<ChaoxingCookieSession>;

    async fn validate_cookie(&self, session: &ChaoxingCookieSession) -> ProviderResult<()>;
}

/// Native reqwest implementation of the Chaoxing authentication transport.
pub struct NativeChaoxingAuthenticationTransport {
    client: Client,
}

impl NativeChaoxingAuthenticationTransport {
    /// Builds the authentication transport from the shared network profile.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error when the shared HTTP client cannot be
    /// initialized.
    pub fn try_new(network: &ResolvedNetworkProfile) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing authentication HTTP client initialization failed",
            )
        })?;
        Ok(Self { client })
    }
}

impl fmt::Debug for NativeChaoxingAuthenticationTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeChaoxingAuthenticationTransport")
            .field("client", &"configured")
            .finish()
    }
}

#[async_trait]
impl ChaoxingAuthenticationTransport for NativeChaoxingAuthenticationTransport {
    async fn exchange_password(
        &self,
        encrypted_username: &SecretString,
        encrypted_password: &SecretString,
    ) -> ProviderResult<ChaoxingCookieSession> {
        let response = self
            .client
            .post(static_login_url()?)
            .header(ACCEPT, "application/json")
            .form(&[
                ("fid", "-1"),
                ("uname", encrypted_username.expose_secret()),
                ("password", encrypted_password.expose_secret()),
                ("refer", "https%3A%2F%2Fi.chaoxing.com"),
                ("t", "true"),
                ("forbidotherlogin", "0"),
                ("validate", ""),
                ("doubleFactorLogin", "0"),
                ("independentId", "0"),
            ])
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        classify_login_response(response).await
    }

    async fn validate_cookie(&self, session: &ChaoxingCookieSession) -> ProviderResult<()> {
        fetch_course_list_html(&self.client, session, "0").await?;
        Ok(())
    }
}

/// Chaoxing Password and `ImportedCookie` authentication capability.
pub struct ChaoxingAuthentication {
    metadata: ProviderMetadata,
    transport: Arc<dyn ChaoxingAuthenticationTransport>,
    sessions: Arc<dyn ChaoxingSessionResolver>,
}

impl ChaoxingAuthentication {
    /// Creates the capability around native authentication and Core secret
    /// resolution boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        transport: Arc<dyn ChaoxingAuthenticationTransport>,
        sessions: Arc<dyn ChaoxingSessionResolver>,
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
                "Chaoxing authentication received a mismatched Provider context",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ChaoxingAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingAuthentication")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingAuthentication {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AuthenticationCapability for ChaoxingAuthentication {
    async fn begin_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<AuthChallenge> {
        self.validate_context(&context.provider_id)?;
        let session_id = context.auth_session_id.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing authentication requires a Core AuthSession",
            )
        })?;
        let waiting_for = match method {
            AuthMethod::Password => WaitingUserState::CredentialInput,
            AuthMethod::ImportedCookie => WaitingUserState::SessionImport,
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
                "Chaoxing credential belongs to another Provider",
            ));
        }
        match credential.auth_method {
            AuthMethod::Password => self.validate_password(credential).await,
            AuthMethod::ImportedCookie => self.validate_imported_cookie(credential).await,
            _ => Err(unsupported_auth_method()),
        }
    }

    async fn validate_session(&self, context: &ProviderContext) -> ProviderResult<SessionStatus> {
        self.validate_context(&context.provider_id)?;
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing session validation requires stored credentials",
            ));
        }
        let session = self.sessions.resolve_session(context).await?;
        self.transport.validate_cookie(&session).await?;
        Ok(valid_session(SessionKind::Cookie))
    }
}

impl ChaoxingAuthentication {
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
        let encrypted_username = encrypt_login_field(username)?;
        let encrypted_password = encrypt_login_field(password)?;
        let session = self
            .transport
            .exchange_password(&encrypted_username, &encrypted_password)
            .await?;
        self.transport.validate_cookie(&session).await?;
        let replacement = CredentialReplacement {
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
        };
        Ok(CredentialValidation {
            status: valid_session(SessionKind::Composite),
            replacement: Some(replacement),
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
        let session = ChaoxingCookieSession::try_new(cookie.to_owned())?;
        self.transport.validate_cookie(&session).await?;
        Ok(CredentialValidation::accepted(valid_session(
            SessionKind::Cookie,
        )))
    }
}

fn credential_text(credential: &CredentialBundle, purpose: SecretPurpose) -> ProviderResult<&str> {
    let mut fields = credential
        .fields
        .iter()
        .filter(|field| field.purpose == purpose);
    let value = fields.next().ok_or_else(invalid_credential_shape)?;
    if fields.next().is_some() {
        return Err(invalid_credential_shape());
    }
    std::str::from_utf8(value.value.expose_secret()).map_err(|_| invalid_credential_shape())
}

fn validate_login_field(value: &str, maximum: usize, trim: bool) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(char::is_control)
        || (trim && value.trim() != value)
    {
        return Err(invalid_credential_shape());
    }
    Ok(())
}

fn encrypt_login_field(value: &str) -> ProviderResult<SecretString> {
    let mut ciphertext = cbc::Encryptor::<Aes128>::new(LOGIN_KEY.into(), LOGIN_KEY.into())
        .encrypt_padded_vec::<Pkcs7>(value.as_bytes());
    if ciphertext.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing login encryption produced an empty value",
        ));
    }
    let encoded = STANDARD.encode(&ciphertext);
    ciphertext.zeroize();
    Ok(SecretString::new(encoded))
}

fn valid_session(kind: SessionKind) -> SessionStatus {
    SessionStatus {
        valid: true,
        kind,
        expires_at: None,
        account_hint: None,
    }
}

async fn classify_login_response(mut response: Response) -> ProviderResult<ChaoxingCookieSession> {
    validate_response_status(&response)?;
    validate_login_response_head(&response)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_LOGIN_RESPONSE_BYTES {
            bytes.zeroize();
            return Err(invalid_login_response());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(invalid_login_response());
    }
    let envelope = serde_json::from_slice::<LoginEnvelope>(&bytes);
    bytes.zeroize();
    let envelope = envelope.map_err(|_| invalid_login_response())?;
    match envelope.status {
        Some(true) => cookie_session_from_headers(response.headers()),
        Some(false) => Err(classify_login_rejection(envelope.message())),
        None => Err(invalid_login_response()),
    }
}

fn validate_login_response_head(response: &Response) -> ProviderResult<()> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_LOGIN_RESPONSE_BYTES as u64)
    {
        return Err(invalid_login_response());
    }
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let content_type = content_type
            .to_str()
            .map_err(|_| invalid_login_response())?;
        let media_type = content_type.split(';').next().unwrap_or_default().trim();
        if !media_type.eq_ignore_ascii_case("application/json")
            && !media_type.eq_ignore_ascii_case("text/json")
        {
            return Err(invalid_login_response());
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct LoginEnvelope {
    status: Option<bool>,
    #[serde(default)]
    msg2: String,
    #[serde(default)]
    msg: String,
}

impl LoginEnvelope {
    fn message(&self) -> &str {
        if self.msg2.is_empty() {
            &self.msg
        } else {
            &self.msg2
        }
    }
}

impl Drop for LoginEnvelope {
    fn drop(&mut self) {
        self.msg2.zeroize();
        self.msg.zeroize();
    }
}

fn classify_login_rejection(message: &str) -> ProviderError {
    let lowercase = message.to_ascii_lowercase();
    if message.contains("短信")
        || message.contains("手机")
        || message.contains("二次验证")
        || lowercase.contains("double factor")
    {
        ProviderError::human_required(
            "Chaoxing password login requires SMS or secondary verification",
            HumanRequiredReason::SmsVerification,
        )
    } else if message.contains("验证码")
        || message.contains("滑块")
        || lowercase.contains("captcha")
    {
        ProviderError::human_required(
            "Chaoxing password login requires an image captcha",
            HumanRequiredReason::ImageCaptcha,
        )
    } else {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing rejected the username or password",
        )
    }
}

fn cookie_session_from_headers(headers: &HeaderMap) -> ProviderResult<ChaoxingCookieSession> {
    let mut cookies = SensitiveCookieJar::default();
    for header in headers.get_all(SET_COOKIE) {
        if cookies.0.len() == MAX_RESPONSE_COOKIES {
            return Err(invalid_login_response());
        }
        let header = header.to_str().map_err(|_| invalid_login_response())?;
        let pair = header.split(';').next().unwrap_or_default().trim();
        let (name, value) = pair.split_once('=').ok_or_else(invalid_login_response)?;
        if !valid_cookie_name(name)
            || value.is_empty()
            || value.len() > MAX_COOKIE_VALUE_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(invalid_login_response());
        }
        if let Some(mut previous) = cookies.0.insert(name.to_owned(), value.to_owned()) {
            previous.zeroize();
        }
    }
    let cookie = cookies.header_value()?;
    ChaoxingCookieSession::try_new(cookie).map_err(|_| invalid_login_response())
}

#[derive(Default)]
struct SensitiveCookieJar(BTreeMap<String, String>);

impl SensitiveCookieJar {
    fn header_value(&self) -> ProviderResult<String> {
        let length = self.0.iter().fold(0_usize, |length, (name, value)| {
            length.saturating_add(name.len() + value.len() + 2)
        });
        if self.0.is_empty() || length > 64 * 1_024 {
            return Err(invalid_login_response());
        }
        Ok(self
            .0
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; "))
    }
}

impl Drop for SensitiveCookieJar {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

fn valid_cookie_name(value: &str) -> bool {
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

fn static_login_url() -> ProviderResult<Url> {
    Url::parse(LOGIN_URL).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing compile-time login route is invalid",
        )
    })
}

fn unsupported_auth_method() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "Chaoxing received an authentication method it does not advertise",
    )
}

fn invalid_credential_shape() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Chaoxing credential fields do not match the selected authentication method",
    )
}

fn invalid_login_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Chaoxing login endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{AuthSessionId, ProviderAccountId, ProviderId, SecretId, Timestamp};
    use http::StatusCode;

    use super::*;

    const LOGIN_SUCCESS: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/password-success.json");
    const LOGIN_REJECTED: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/password-rejected.json");
    const LOGIN_CAPTCHA: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/password-captcha.json");
    const LOGIN_SMS: &[u8] =
        include_bytes!("../../../fixtures/providers/chaoxing/auth/password-sms.json");

    #[derive(Debug)]
    struct FixtureTransport {
        exchanges: AtomicUsize,
        validations: AtomicUsize,
    }

    #[async_trait]
    impl ChaoxingAuthenticationTransport for FixtureTransport {
        async fn exchange_password(
            &self,
            encrypted_username: &SecretString,
            encrypted_password: &SecretString,
        ) -> ProviderResult<ChaoxingCookieSession> {
            self.exchanges.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                encrypted_username.expose_secret(),
                "+Irq4/vrXQYmBDleZs/0Qg=="
            );
            assert_eq!(
                encrypted_password.expose_secret(),
                "xZ9dYlVASzFVINL+G69JXA=="
            );
            ChaoxingCookieSession::try_new("_uid=SAFE_UID; uf=SAFE_UF")
        }

        async fn validate_cookie(&self, _session: &ChaoxingCookieSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FixtureSessions;

    #[async_trait]
    impl ChaoxingSessionResolver for FixtureSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<ChaoxingCookieSession> {
            ChaoxingCookieSession::try_new("_uid=STORED_UID; uf=STORED_UF")
        }
    }

    #[test]
    fn password_cipher_matches_the_current_donor() {
        let username = encrypt_login_field("test-user").unwrap();
        let password = encrypt_login_field("test-password").unwrap();
        assert_eq!(username.expose_secret(), "+Irq4/vrXQYmBDleZs/0Qg==");
        assert_eq!(password.expose_secret(), "xZ9dYlVASzFVINL+G69JXA==");
        assert!(!format!("{username:?}").contains("Irq4"));
    }

    #[tokio::test]
    async fn password_login_derives_a_revalidated_composite_session() {
        let transport = Arc::new(FixtureTransport {
            exchanges: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        });
        let authentication =
            ChaoxingAuthentication::try_new(transport.clone(), Arc::new(FixtureSessions)).unwrap();
        let validation = authentication
            .validate_credential(&auth_context(), &password_bundle())
            .await
            .unwrap();
        assert_eq!(validation.status.kind, SessionKind::Composite);
        let replacement = validation.replacement.unwrap();
        assert_eq!(replacement.session_kind, SessionKind::Composite);
        assert_eq!(replacement.fields.len(), 3);
        assert_eq!(
            replacement.fields[2].value.expose_secret(),
            b"_uid=SAFE_UID; uf=SAFE_UF"
        );
        assert_eq!(transport.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 1);
        assert!(
            authentication
                .metadata()
                .capabilities
                .contains(&asterism_provider_api::ProviderCapability::Authentication)
        );
    }

    #[tokio::test]
    async fn imported_cookie_is_validated_without_a_password_exchange() {
        let transport = Arc::new(FixtureTransport {
            exchanges: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        });
        let authentication =
            ChaoxingAuthentication::try_new(transport.clone(), Arc::new(FixtureSessions)).unwrap();
        let validation = authentication
            .validate_credential(&auth_context(), &cookie_bundle())
            .await
            .unwrap();
        assert_eq!(validation.status.kind, SessionKind::Cookie);
        assert!(validation.replacement.is_none());
        assert_eq!(transport.exchanges.load(Ordering::SeqCst), 0);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 1);

        let session = authentication
            .validate_session(&provider_context())
            .await
            .unwrap();
        assert_eq!(session.kind, SessionKind::Cookie);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn challenges_distinguish_password_from_session_import() {
        let authentication = ChaoxingAuthentication::try_new(
            Arc::new(FixtureTransport {
                exchanges: AtomicUsize::new(0),
                validations: AtomicUsize::new(0),
            }),
            Arc::new(FixtureSessions),
        )
        .unwrap();
        let password = authentication
            .begin_authentication(&auth_context(), AuthMethod::Password)
            .await
            .unwrap();
        assert_eq!(password.waiting_for, WaitingUserState::CredentialInput);
        let imported = authentication
            .begin_authentication(&auth_context(), AuthMethod::ImportedCookie)
            .await
            .unwrap();
        assert_eq!(imported.waiting_for, WaitingUserState::SessionImport);
        assert!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::QrCode)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn login_json_and_set_cookie_are_classified_without_body_leaks() {
        let session = classify_login_response(response(
            StatusCode::OK,
            &[
                ("content-type", "application/json"),
                ("set-cookie", "_uid=SAFE_UID; Path=/; HttpOnly"),
                ("set-cookie", "uf=SAFE_UF; Path=/; Secure"),
            ],
            LOGIN_SUCCESS.to_vec(),
        ))
        .await
        .unwrap();
        assert!(session.expose_secret().contains("_uid=SAFE_UID"));
        assert!(session.expose_secret().contains("uf=SAFE_UF"));
        assert!(!format!("{session:?}").contains("SAFE_UID"));

        let captcha = classify_login_response(response(
            StatusCode::OK,
            &[("content-type", "application/json")],
            LOGIN_CAPTCHA.to_vec(),
        ))
        .await
        .unwrap_err();
        assert_eq!(captcha.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(
            captcha.human_required_reason,
            Some(HumanRequiredReason::ImageCaptcha)
        );

        let sms = classify_login_response(response(
            StatusCode::OK,
            &[("content-type", "application/json")],
            LOGIN_SMS.to_vec(),
        ))
        .await
        .unwrap_err();
        assert_eq!(
            sms.human_required_reason,
            Some(HumanRequiredReason::SmsVerification)
        );

        let rejected = classify_login_response(response(
            StatusCode::OK,
            &[("content-type", "application/json")],
            LOGIN_REJECTED.to_vec(),
        ))
        .await
        .unwrap_err();
        assert_eq!(rejected.kind, ProviderErrorKind::Authentication);
        assert!(!rejected.message.contains("private"));
    }

    fn response(status: StatusCode, headers: &[(&str, &str)], body: Vec<u8>) -> Response {
        let mut response = http::Response::builder().status(status);
        for (name, value) in headers {
            response = response.header(*name, *value);
        }
        response.body(body).unwrap().into()
    }

    fn password_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            tenant: None,
            auth_method: AuthMethod::Password,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: Timestamp::default(),
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
            provider_id: ProviderId::new("chaoxing").unwrap(),
            tenant: None,
            auth_method: AuthMethod::ImportedCookie,
            acquired_via: CredentialAcquisition::ManualImport,
            captured_at: Timestamp::default(),
            expires_at: None,
            session_kind: SessionKind::Cookie,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(b"_uid=IMPORTED_UID; uf=IMPORTED_UF".to_vec()),
            }],
            user_id_hint: None,
        }
    }

    fn auth_context() -> ProviderAuthContext {
        ProviderAuthContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            auth_session_id: Some(AuthSessionId::new()),
            correlation_id: "chaoxing-auth-test".to_owned(),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-session-test".to_owned(),
        }
    }
}
