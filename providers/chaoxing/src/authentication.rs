use std::{collections::BTreeMap, fmt, sync::Arc};

use aes::Aes128;
use asterism_domain::{AuthMethod, HumanRequiredReason, SessionKind, WaitingUserState};
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    AuthChallenge, AuthenticationCapability, CaptureCredentialOutput, CaptureReadiness,
    CaptureRecipe, CaptureValueSource, CredentialReplacement, CredentialValidation,
    ProviderAuthContext, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderInteractiveAuthBegin, ProviderInteractiveAuthPollOutcome, ProviderMetadata,
    ProviderResult, ResolvedProviderInteractiveAuthContinuation, SessionStatus,
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
    ChaoxingCookieSession, ChaoxingQrAuthenticationTransport, ChaoxingQrChallenge,
    ChaoxingQrPollOutcome, ChaoxingSessionResolver,
    metadata::development_metadata,
    native_http::{classify_reqwest_error, fetch_course_list_html, validate_response_status},
};

const LOGIN_URL: &str = "https://passport2.chaoxing.com/fanyalogin";
const LOGIN_KEY: &[u8; 16] = b"u2oh6Vu^HWe4_AES";
const MAX_LOGIN_RESPONSE_BYTES: usize = 64 * 1_024;
pub(crate) const MAX_USERNAME_BYTES: usize = 512;
pub(crate) const MAX_PASSWORD_BYTES: usize = 4 * 1_024;
const MAX_RESPONSE_COOKIES: usize = 128;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const MAX_COOKIE_VALUE_BYTES: usize = 8 * 1_024;
const CAPTURE_START_URL: &str = "https://i.chaoxing.com";
const COURSE_LIST_ORIGIN: &str = "https://mooc2-ans.chaoxing.com";
const COURSE_LIST_PATH: &str = "/mooc2-ans/visit/courselistdata";

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
    qr_transport: Option<Arc<dyn ChaoxingQrAuthenticationTransport>>,
    sessions: Arc<dyn ChaoxingSessionResolver>,
}

impl ChaoxingAuthentication {
    fn capture_recipe() -> CaptureRecipe {
        CaptureRecipe {
            version: 1,
            start_url: CAPTURE_START_URL.to_owned(),
            navigation_origins: vec![
                "https://i.chaoxing.com".to_owned(),
                "https://passport2.chaoxing.com".to_owned(),
                COURSE_LIST_ORIGIN.to_owned(),
                "https://mooc1.chaoxing.com".to_owned(),
                "https://mooc1-api.chaoxing.com".to_owned(),
            ],
            read_origins: vec![COURSE_LIST_ORIGIN.to_owned()],
            poll_interval_millis: 500,
            auth_method: AuthMethod::AssistedSession,
            session_kind: SessionKind::Cookie,
            readiness: CaptureReadiness::ResponseObserved {
                origin: COURSE_LIST_ORIGIN.to_owned(),
                method: "POST".to_owned(),
                path_and_query: COURSE_LIST_PATH.to_owned(),
                status: 200,
                mime_type: "text/html".to_owned(),
            },
            outputs: vec![CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCookie,
                required: true,
                sources: vec![CaptureValueSource::CookieHeader {
                    origin: COURSE_LIST_ORIGIN.to_owned(),
                }],
            }],
        }
    }

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
        let mut metadata = development_metadata()?;
        metadata.auth_methods.remove(&AuthMethod::QrCode);
        Ok(Self {
            metadata,
            transport,
            qr_transport: None,
            sessions,
        })
    }

    /// Creates the capability with the bounded native QR transport enabled.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new_with_qr(
        transport: Arc<dyn ChaoxingAuthenticationTransport>,
        qr_transport: Arc<dyn ChaoxingQrAuthenticationTransport>,
        sessions: Arc<dyn ChaoxingSessionResolver>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
            qr_transport: Some(qr_transport),
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
            .field(
                "qr_transport",
                &self.qr_transport.as_ref().map(|_| "configured"),
            )
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
    fn capture_recipe(&self) -> Option<CaptureRecipe> {
        Some(Self::capture_recipe())
    }

    fn supports_durable_interactive_authentication(&self) -> bool {
        self.qr_transport.is_some()
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
                "Chaoxing authentication requires a Core AuthSession",
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
            user_action: (method == AuthMethod::AssistedSession).then(|| {
                "在隔离浏览器中完成超星登录及验证码；进入课程页后 Capture 会提交当前 Cookie"
                    .to_owned()
            }),
            expires_at: None,
            external_oauth: None,
        })
    }

    async fn begin_interactive_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<ProviderInteractiveAuthBegin> {
        self.validate_context(&context.provider_id)?;
        if method != AuthMethod::QrCode {
            return Err(unsupported_auth_method());
        }
        let session_id = context.auth_session_id.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing QR authentication requires a Core AuthSession",
            )
        })?;
        let transport = self.qr_transport.as_ref().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing QR authentication transport is not configured",
            )
        })?;
        let challenge = transport.begin_qr(context).await?;
        let user_action = challenge.challenge_url().expose_secret().to_owned();
        validate_qr_user_action(&user_action)?;
        let expires_at = challenge.expires_at();
        let continuation = challenge.to_provider_continuation()?;
        Ok(ProviderInteractiveAuthBegin {
            challenge: AuthChallenge {
                session_id,
                method,
                waiting_for: WaitingUserState::QrScan,
                user_action: Some(user_action),
                expires_at: Some(expires_at),
                external_oauth: None,
            },
            continuation,
        })
    }

    async fn poll_interactive_authentication(
        &self,
        context: &ProviderAuthContext,
        continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
    ) -> ProviderResult<ProviderInteractiveAuthPollOutcome> {
        self.validate_context(&context.provider_id)?;
        let transport = self.qr_transport.as_ref().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing QR authentication transport is not configured",
            )
        })?;
        let mut challenge = ChaoxingQrChallenge::decode_bound(context, &continuation)?;
        let outcome = transport.poll_qr(context, &mut challenge).await?;
        let result = match outcome {
            ChaoxingQrPollOutcome::AwaitingScan { result_digest } => {
                let user_action = challenge.challenge_url().expose_secret().to_owned();
                validate_qr_user_action(&user_action)?;
                ProviderInteractiveAuthPollOutcome::Waiting {
                    waiting_for: WaitingUserState::QrScan,
                    user_action: Some(user_action),
                    continuation: challenge.to_provider_continuation()?,
                    result_digest,
                }
            }
            ChaoxingQrPollOutcome::AwaitingConfirmation { result_digest }
            | ChaoxingQrPollOutcome::IdentityValidation { result_digest } => {
                ProviderInteractiveAuthPollOutcome::Waiting {
                    waiting_for: WaitingUserState::QrConfirm,
                    user_action: None,
                    continuation: challenge.to_provider_continuation()?,
                    result_digest,
                }
            }
            ChaoxingQrPollOutcome::Authenticated { result_digest, .. } => {
                ProviderInteractiveAuthPollOutcome::Authenticated {
                    continuation: challenge.to_provider_continuation()?,
                    result_digest,
                }
            }
            ChaoxingQrPollOutcome::Rejected { result_digest } => {
                ProviderInteractiveAuthPollOutcome::Rejected { result_digest }
            }
            ChaoxingQrPollOutcome::Expired { result_digest } => {
                ProviderInteractiveAuthPollOutcome::Expired { result_digest }
            }
        };
        result.validate()?;
        Ok(result)
    }

    async fn finalize_interactive_authentication(
        &self,
        context: &ProviderAuthContext,
        continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
    ) -> ProviderResult<CredentialBundle> {
        self.validate_context(&context.provider_id)?;
        let challenge = ChaoxingQrChallenge::decode_bound(context, &continuation)?;
        let session = challenge.authenticated_session()?;
        let bundle = CredentialBundle {
            provider_id: self.metadata.id.clone(),
            tenant: None,
            auth_method: AuthMethod::QrCode,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at: challenge.authenticated_at()?,
            expires_at: None,
            session_kind: SessionKind::Cookie,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(session.expose_secret().as_bytes().to_vec()),
            }],
            user_id_hint: None,
        };
        bundle.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing QR terminal credential is invalid",
            )
        })?;
        Ok(bundle)
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
            AuthMethod::AssistedSession => self.validate_captured_cookie(credential).await,
            AuthMethod::QrCode
                if credential.acquired_via == CredentialAcquisition::NativeProviderLogin =>
            {
                self.validate_imported_cookie(credential).await
            }
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

pub(crate) fn encrypt_login_field(value: &str) -> ProviderResult<SecretString> {
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

fn validate_qr_user_action(value: &str) -> ProviderResult<()> {
    let url = Url::parse(value).map_err(|_| invalid_qr_action())?;
    if value.len() > 4_096
        || value.trim() != value
        || url.scheme() != "https"
        || url.host_str() != Some("passport2.chaoxing.com")
        || url.path() != "/toauthlogin"
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_qr_action());
    }
    let mut fields = BTreeMap::new();
    for (name, value) in url.query_pairs() {
        if fields
            .insert(name.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(invalid_qr_action());
        }
    }
    if fields.len() != 5
        || fields.get("uuid").is_none_or(String::is_empty)
        || fields.get("enc").is_none_or(String::is_empty)
        || fields.get("xxtrefer").is_none_or(|value| !value.is_empty())
        || fields.get("clientid").is_none_or(|value| !value.is_empty())
        || fields
            .get("mobiletip")
            .is_none_or(|value| !value.is_empty())
    {
        return Err(invalid_qr_action());
    }
    Ok(())
}

fn invalid_qr_action() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "Chaoxing QR user action is outside the allowlisted origin or shape",
    )
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
    struct FixtureQrTransport {
        polls: AtomicUsize,
        mode: FixtureQrMode,
    }

    #[derive(Clone, Copy, Debug)]
    enum FixtureQrMode {
        RotateThenAuthenticate,
        Reject,
        Expire,
    }

    #[async_trait]
    impl ChaoxingQrAuthenticationTransport for FixtureQrTransport {
        async fn begin_qr(
            &self,
            context: &ProviderAuthContext,
        ) -> ProviderResult<ChaoxingQrChallenge> {
            Ok(ChaoxingQrChallenge::fixture(context))
        }

        async fn poll_qr(
            &self,
            context: &ProviderAuthContext,
            challenge: &mut ChaoxingQrChallenge,
        ) -> ProviderResult<ChaoxingQrPollOutcome> {
            let poll = self.polls.fetch_add(1, Ordering::SeqCst);
            Ok(match self.mode {
                FixtureQrMode::RotateThenAuthenticate if poll == 0 => {
                    challenge.fixture_awaiting_confirmation(context)
                }
                FixtureQrMode::RotateThenAuthenticate if poll == 1 => {
                    challenge.fixture_identity_validation(context)
                }
                FixtureQrMode::RotateThenAuthenticate => challenge.fixture_authenticated(context),
                FixtureQrMode::Reject => challenge.fixture_rejected(context),
                FixtureQrMode::Expire => challenge.fixture_expired(context),
            })
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

    #[test]
    fn assisted_login_capture_waits_for_the_authenticated_course_response() {
        let authentication = ChaoxingAuthentication::try_new(
            Arc::new(FixtureTransport {
                exchanges: AtomicUsize::new(0),
                validations: AtomicUsize::new(0),
            }),
            Arc::new(FixtureSessions),
        )
        .unwrap();
        let recipe = authentication.capture_recipe().unwrap();
        recipe.validate().unwrap();
        assert_eq!(recipe.version, 1);
        assert_eq!(recipe.auth_method, AuthMethod::AssistedSession);
        assert_eq!(recipe.session_kind, SessionKind::Cookie);
        assert_eq!(recipe.read_origins, [COURSE_LIST_ORIGIN]);
        assert_eq!(
            recipe.readiness,
            CaptureReadiness::ResponseObserved {
                origin: COURSE_LIST_ORIGIN.to_owned(),
                method: "POST".to_owned(),
                path_and_query: COURSE_LIST_PATH.to_owned(),
                status: 200,
                mime_type: "text/html".to_owned(),
            }
        );
        assert_eq!(
            recipe.outputs[0].sources,
            [CaptureValueSource::CookieHeader {
                origin: COURSE_LIST_ORIGIN.to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn assisted_capture_revalidates_the_cookie_before_accepting_it() {
        let transport = Arc::new(FixtureTransport {
            exchanges: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        });
        let authentication =
            ChaoxingAuthentication::try_new(transport.clone(), Arc::new(FixtureSessions)).unwrap();
        let validation = authentication
            .validate_credential(&auth_context(), &captured_cookie_bundle())
            .await
            .unwrap();
        assert_eq!(validation.status.kind, SessionKind::Cookie);
        assert_eq!(transport.exchanges.load(Ordering::SeqCst), 0);
        assert_eq!(transport.validations.load(Ordering::SeqCst), 1);
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
        let assisted = authentication
            .begin_authentication(&auth_context(), AuthMethod::AssistedSession)
            .await
            .unwrap();
        assert_eq!(assisted.waiting_for, WaitingUserState::SessionImport);
        assert!(assisted.user_action.is_some());
        assert!(
            authentication
                .begin_authentication(&auth_context(), AuthMethod::QrCode)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn qr_continuations_rotate_then_finalize_without_repolling() {
        let credential_transport = Arc::new(FixtureTransport {
            exchanges: AtomicUsize::new(0),
            validations: AtomicUsize::new(0),
        });
        let qr_transport = Arc::new(FixtureQrTransport {
            polls: AtomicUsize::new(0),
            mode: FixtureQrMode::RotateThenAuthenticate,
        });
        let authentication = ChaoxingAuthentication::try_new_with_qr(
            credential_transport.clone(),
            qr_transport.clone(),
            Arc::new(FixtureSessions),
        )
        .unwrap();
        let context = auth_context();
        let begin = authentication
            .begin_interactive_authentication(&context, AuthMethod::QrCode)
            .await
            .unwrap();
        assert_eq!(begin.challenge.method, AuthMethod::QrCode);
        assert_eq!(begin.challenge.waiting_for, WaitingUserState::QrScan);
        validate_qr_user_action(begin.challenge.user_action.as_deref().unwrap()).unwrap();
        let digest = begin.continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = begin.continuation.into_parts();

        let waiting = authentication
            .poll_interactive_authentication(
                &context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 1,
                    poll_sequence: 1,
                    value: &value,
                },
            )
            .await
            .unwrap();
        let ProviderInteractiveAuthPollOutcome::Waiting {
            waiting_for,
            user_action,
            continuation,
            result_digest,
        } = waiting
        else {
            panic!("first QR poll did not preserve the waiting continuation");
        };
        assert_eq!(waiting_for, WaitingUserState::QrConfirm);
        assert!(user_action.is_none());
        assert_ne!(result_digest, [0; 32]);
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();

        let identity_validation = authentication
            .poll_interactive_authentication(
                &context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 2,
                    poll_sequence: 2,
                    value: &value,
                },
            )
            .await
            .unwrap();
        let ProviderInteractiveAuthPollOutcome::Waiting {
            waiting_for,
            user_action,
            continuation,
            result_digest,
        } = identity_validation
        else {
            panic!("successful QR status did not persist identity validation state");
        };
        assert_eq!(waiting_for, WaitingUserState::QrConfirm);
        assert!(user_action.is_none());
        assert_ne!(result_digest, [0; 32]);
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();

        let authenticated = authentication
            .poll_interactive_authentication(
                &context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 3,
                    poll_sequence: 3,
                    value: &value,
                },
            )
            .await
            .unwrap();
        let ProviderInteractiveAuthPollOutcome::Authenticated {
            continuation,
            result_digest,
        } = authenticated
        else {
            panic!("identity validation did not produce a terminal continuation");
        };
        assert_ne!(result_digest, [0; 32]);
        let digest = continuation.continuation_digest();
        let (continuation_type, phase, value, _, _) = continuation.into_parts();
        let finalized = authentication
            .finalize_interactive_authentication(
                &context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 4,
                    poll_sequence: 3,
                    value: &value,
                },
            )
            .await
            .unwrap();
        let repeated = authentication
            .finalize_interactive_authentication(
                &context,
                ResolvedProviderInteractiveAuthContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest: digest,
                    phase: &phase,
                    revision: 4,
                    poll_sequence: 3,
                    value: &value,
                },
            )
            .await
            .unwrap();
        assert_eq!(finalized.auth_method, AuthMethod::QrCode);
        assert_eq!(finalized.session_kind, SessionKind::Cookie);
        assert_eq!(finalized.captured_at, repeated.captured_at);
        assert_eq!(
            finalized.fields[0].value.expose_secret(),
            repeated.fields[0].value.expose_secret()
        );
        assert!(
            std::str::from_utf8(finalized.fields[0].value.expose_secret())
                .unwrap()
                .contains("_uid=SAFE_QR_UID")
        );
        assert_eq!(qr_transport.polls.load(Ordering::SeqCst), 3);

        let validated = authentication
            .validate_credential(&context, &finalized)
            .await
            .unwrap();
        assert!(validated.status.valid);
        assert_eq!(credential_transport.validations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn qr_rejection_and_expiry_are_persistable_terminal_outcomes() {
        for mode in [FixtureQrMode::Reject, FixtureQrMode::Expire] {
            let authentication = ChaoxingAuthentication::try_new_with_qr(
                Arc::new(FixtureTransport {
                    exchanges: AtomicUsize::new(0),
                    validations: AtomicUsize::new(0),
                }),
                Arc::new(FixtureQrTransport {
                    polls: AtomicUsize::new(0),
                    mode,
                }),
                Arc::new(FixtureSessions),
            )
            .unwrap();
            let context = auth_context();
            let begin = authentication
                .begin_interactive_authentication(&context, AuthMethod::QrCode)
                .await
                .unwrap();
            let digest = begin.continuation.continuation_digest();
            let (continuation_type, phase, value, _, _) = begin.continuation.into_parts();
            let outcome = authentication
                .poll_interactive_authentication(
                    &context,
                    ResolvedProviderInteractiveAuthContinuation {
                        continuation_type: &continuation_type,
                        continuation_digest: digest,
                        phase: &phase,
                        revision: 1,
                        poll_sequence: 1,
                        value: &value,
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                (mode, outcome),
                (
                    FixtureQrMode::Reject,
                    ProviderInteractiveAuthPollOutcome::Rejected { .. }
                ) | (
                    FixtureQrMode::Expire,
                    ProviderInteractiveAuthPollOutcome::Expired { .. }
                )
            ));
        }
    }

    #[test]
    fn qr_user_action_rejects_foreign_or_ambiguous_urls() {
        for action in [
            "http://passport2.chaoxing.com/toauthlogin?uuid=a&enc=b&xxtrefer=&clientid=&mobiletip=",
            "https://foreign.example/toauthlogin?uuid=a&enc=b&xxtrefer=&clientid=&mobiletip=",
            "https://passport2.chaoxing.com/toauthlogin?uuid=a&uuid=b&enc=c&xxtrefer=&clientid=&mobiletip=",
            "https://passport2.chaoxing.com/toauthlogin?uuid=a&enc=b",
        ] {
            assert!(validate_qr_user_action(action).is_err());
        }
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

    fn captured_cookie_bundle() -> CredentialBundle {
        let mut credential = cookie_bundle();
        credential.auth_method = AuthMethod::AssistedSession;
        credential.acquired_via = CredentialAcquisition::CaptureTool;
        credential
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
