use std::{fmt, sync::Arc};

use asterism_domain::{
    AuthMethod, ProtocolObservationKind, ProtocolSurface, SessionKind, WaitingUserState,
};
use asterism_provider_api::{
    AuthChallenge, AuthenticationCapability, CaptureRecipe, CredentialReplacement,
    CredentialValidation, ExternalOauthCallbackBinding, ProviderAuthContext, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderResult,
    SessionStatus,
};
use asterism_secrets::{CredentialAcquisition, CredentialBundle, SecretPurpose, SecretString};
use async_trait::async_trait;
use http::HeaderValue;
use serde_json::{Value, json};
use zeroize::Zeroize;

use crate::{
    CidarenCryptoContext, cidaren_capture_recipe_v2, cidaren_token_capture_recipe_v1,
    metadata::development_metadata,
    oauth_authorization::CidarenOauthAuthorization,
    protocol_observation::{error_with_protocol_observation, json_value_kind},
};

const MAX_TOKEN_BYTES: usize = 64 * 1_024;
const MAX_VALIDATION_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_SELECTED_COURSE_ID_BYTES: usize = 256;

/// One bounded opaque `UserToken`. Plaintext is redacted and zeroized.
pub struct CidarenTokenSession {
    token: SecretString,
    crypto: Option<CidarenCryptoContext>,
    validation_context: CidarenSessionValidationContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CidarenSessionValidationContext {
    DonorHeaders,
    NativeOauthHeaders,
}

impl CidarenTokenSession {
    /// Validates an imported token without assuming a historical hex format.
    ///
    /// # Errors
    ///
    /// Returns Authentication for empty, oversized, whitespace-padded or
    /// header-unsafe token values.
    pub fn try_new(token: impl Into<String>) -> ProviderResult<Self> {
        let mut token = token.into();
        let valid = !token.is_empty()
            && token.len() <= MAX_TOKEN_BYTES
            && token.trim() == token
            && !token.chars().any(char::is_control)
            && HeaderValue::from_str(&token).is_ok();
        if !valid {
            token.zeroize();
            return Err(invalid_credential_shape());
        }
        Ok(Self {
            token: SecretString::new(token),
            crypto: None,
            validation_context: CidarenSessionValidationContext::DonorHeaders,
        })
    }

    /// Builds one captured composite session after parsing its bounded crypto
    /// context. The token and context remain one inseparable account binding.
    ///
    /// # Errors
    ///
    /// Returns Authentication when either credential component is malformed.
    pub fn try_new_captured(
        token: impl Into<String>,
        crypto_document: &[u8],
    ) -> ProviderResult<Self> {
        let mut session = Self::try_new(token)?;
        session.crypto = Some(CidarenCryptoContext::parse(crypto_document)?);
        Ok(session)
    }

    /// Builds one native V2 OAuth Composite session whose future account
    /// validation must retain the current bootstrap header family.
    pub(crate) fn try_new_native_oauth(
        token: impl Into<String>,
        crypto_document: &[u8],
    ) -> ProviderResult<Self> {
        let mut session = Self::try_new_captured(token, crypto_document)?;
        session.validation_context = CidarenSessionValidationContext::NativeOauthHeaders;
        Ok(session)
    }

    /// Exposes the token only to a bounded authenticated transport.
    pub fn expose_token(&self) -> &str {
        self.token.expose_secret()
    }

    pub fn crypto_context(&self) -> Option<&CidarenCryptoContext> {
        self.crypto.as_ref()
    }

    pub const fn session_kind(&self) -> SessionKind {
        if self.crypto.is_some() {
            SessionKind::Composite
        } else {
            SessionKind::ProviderSpecific
        }
    }

    pub(crate) const fn requires_native_oauth_validation(&self) -> bool {
        matches!(
            self.validation_context,
            CidarenSessionValidationContext::NativeOauthHeaders
        )
    }
}

impl fmt::Debug for CidarenTokenSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenTokenSession([REDACTED])")
    }
}

/// Provider-internal token validation transport.
#[async_trait]
pub trait CidarenAuthenticationTransport: Send + Sync {
    async fn validate_token(&self, session: &CidarenTokenSession) -> ProviderResult<()>;

    /// Revalidates native OAuth material with the exact current bootstrap
    /// request context before Core commits the replacement and after later
    /// account-bound stored-session resolution.
    async fn validate_native_oauth_session(
        &self,
        session: &CidarenTokenSession,
    ) -> ProviderResult<()>;

    /// Consumes a callback already claimed by Core and returns the exact
    /// native Composite replacement without persisting it.
    async fn exchange_external_oauth_callback(
        &self,
        callback_url: SecretString,
        binding: ExternalOauthCallbackBinding,
    ) -> ProviderResult<CredentialReplacement>;
}

/// Resolves one account-bound stored Cidaren token.
#[async_trait]
pub trait CidarenSessionResolver: Send + Sync {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<CidarenTokenSession>;
}

/// Manual token and captured WeChat/browser authentication orchestration.
pub struct CidarenAuthentication {
    metadata: ProviderMetadata,
    transport: Arc<dyn CidarenAuthenticationTransport>,
    sessions: Arc<dyn CidarenSessionResolver>,
}

impl CidarenAuthentication {
    /// Creates the capability around injected validation and stored-session
    /// boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        transport: Arc<dyn CidarenAuthenticationTransport>,
        sessions: Arc<dyn CidarenSessionResolver>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
            sessions,
        })
    }

    fn validate_provider(&self, provider_id: &asterism_domain::ProviderId) -> ProviderResult<()> {
        if provider_id != &self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren authentication received a mismatched Provider context",
            ));
        }
        Ok(())
    }

    async fn validate_imported_token(
        &self,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if credential.session_kind != SessionKind::ProviderSpecific
            || credential.acquired_via != CredentialAcquisition::ManualImport
            || credential.fields.len() != 1
        {
            return Err(invalid_credential_shape());
        }
        let bytes = credential
            .fields
            .iter()
            .find(|field| field.purpose == SecretPurpose::ProviderAccessToken)
            .filter(|_| {
                credential
                    .fields
                    .iter()
                    .filter(|field| field.purpose == SecretPurpose::ProviderAccessToken)
                    .count()
                    == 1
            })
            .map(|field| field.value.expose_secret())
            .ok_or_else(invalid_credential_shape)?;
        let token = std::str::from_utf8(bytes).map_err(|_| invalid_credential_shape())?;
        let session = CidarenTokenSession::try_new(token.to_owned())?;
        self.transport.validate_token(&session).await?;
        Ok(CredentialValidation::accepted(valid_session(
            SessionKind::ProviderSpecific,
        )))
    }

    async fn validate_captured_session(
        &self,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        if credential.auth_method == AuthMethod::AssistedSession
            && credential.session_kind == SessionKind::ProviderSpecific
            && matches!(
                credential.acquired_via,
                CredentialAcquisition::CaptureTool | CredentialAcquisition::BrowserExtension
            )
            && credential.fields.len() == 1
        {
            let token = exact_field(credential, SecretPurpose::ProviderAccessToken)?;
            let token = std::str::from_utf8(token).map_err(|_| invalid_credential_shape())?;
            let session = CidarenTokenSession::try_new(token.to_owned())?;
            self.transport.validate_token(&session).await?;
            return Ok(CredentialValidation::accepted(valid_session(
                SessionKind::ProviderSpecific,
            )));
        }
        let valid_acquisition = match credential.auth_method {
            AuthMethod::AssistedSession => matches!(
                credential.acquired_via,
                CredentialAcquisition::CaptureTool
                    | CredentialAcquisition::BrowserExtension
                    | CredentialAcquisition::NativeProviderLogin
            ),
            AuthMethod::ExternalBrowserOauth => {
                credential.acquired_via == CredentialAcquisition::NativeProviderLogin
            }
            _ => false,
        };
        if credential.session_kind != SessionKind::Composite
            || !valid_acquisition
            || credential.fields.len() != 2
        {
            return Err(invalid_credential_shape());
        }
        let token = exact_field(credential, SecretPurpose::ProviderAccessToken)?;
        let crypto = exact_field(credential, SecretPurpose::ProviderCompositeSession)?;
        let token = std::str::from_utf8(token).map_err(|_| invalid_credential_shape())?;
        let session = if credential.acquired_via == CredentialAcquisition::NativeProviderLogin {
            CidarenTokenSession::try_new_native_oauth(token.to_owned(), crypto)?
        } else {
            CidarenTokenSession::try_new_captured(token.to_owned(), crypto)?
        };
        if session.requires_native_oauth_validation() {
            self.transport
                .validate_native_oauth_session(&session)
                .await?;
        } else {
            self.transport.validate_token(&session).await?;
        }
        Ok(CredentialValidation::accepted(valid_session(
            SessionKind::Composite,
        )))
    }
}

impl fmt::Debug for CidarenAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenAuthentication")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenAuthentication {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AuthenticationCapability for CidarenAuthentication {
    fn capture_recipe(&self) -> Option<CaptureRecipe> {
        Some(cidaren_capture_recipe_v2())
    }

    fn capture_recipes(&self) -> Vec<CaptureRecipe> {
        vec![
            cidaren_capture_recipe_v2(),
            cidaren_token_capture_recipe_v1(),
        ]
    }

    async fn begin_authentication(
        &self,
        context: &ProviderAuthContext,
        method: AuthMethod,
    ) -> ProviderResult<AuthChallenge> {
        self.validate_provider(&context.provider_id)?;
        if !matches!(
            method,
            AuthMethod::ImportedToken
                | AuthMethod::AssistedSession
                | AuthMethod::ExternalBrowserOauth
        ) {
            return Err(unsupported_auth_method());
        }
        let session_id = context.auth_session_id.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren authentication requires a Core AuthSession",
            )
        })?;
        let external_oauth = if method == AuthMethod::ImportedToken {
            None
        } else {
            Some(CidarenOauthAuthorization::generate()?.into_external()?)
        };
        Ok(AuthChallenge {
            session_id,
            method,
            waiting_for: if method == AuthMethod::ImportedToken {
                WaitingUserState::SessionImport
            } else {
                WaitingUserState::BrowserCallback
            },
            user_action: (method != AuthMethod::ImportedToken).then(|| {
                "Open the generated URL in WeChat, authorize, then return the final Cidaren callback URL"
                    .to_owned()
            }),
            expires_at: None,
            external_oauth,
        })
    }

    async fn exchange_external_oauth_callback(
        &self,
        context: &ProviderAuthContext,
        callback_url: SecretString,
        binding: ExternalOauthCallbackBinding,
    ) -> ProviderResult<CredentialReplacement> {
        self.validate_provider(&context.provider_id)?;
        if context.auth_session_id.is_none() || !binding.validate() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Cidaren OAuth callback has no valid Core AuthSession binding",
            ));
        }
        self.transport
            .exchange_external_oauth_callback(callback_url, binding)
            .await
    }

    async fn validate_credential(
        &self,
        context: &ProviderAuthContext,
        credential: &CredentialBundle,
    ) -> ProviderResult<CredentialValidation> {
        self.validate_provider(&context.provider_id)?;
        if credential.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Cidaren credential belongs to another Provider",
            ));
        }
        match credential.auth_method {
            AuthMethod::ImportedToken => self.validate_imported_token(credential).await,
            AuthMethod::AssistedSession | AuthMethod::ExternalBrowserOauth => {
                self.validate_captured_session(credential).await
            }
            _ => Err(unsupported_auth_method()),
        }
    }

    async fn validate_session(&self, context: &ProviderContext) -> ProviderResult<SessionStatus> {
        self.validate_provider(&context.provider_id)?;
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Cidaren session validation requires stored credentials",
            ));
        }
        let session = self.sessions.resolve_session(context).await?;
        if session.requires_native_oauth_validation() {
            self.transport
                .validate_native_oauth_session(&session)
                .await?;
        } else {
            self.transport.validate_token(&session).await?;
        }
        Ok(valid_session(session.session_kind()))
    }
}

/// Classifies one bounded `Student/Main` response without retaining account
/// profile fields.
///
/// # Errors
///
/// Returns Authentication for a well-formed non-success code and
/// `InvalidResponse` for malformed or oversized success data.
pub fn classify_token_validation_response(document: &[u8]) -> ProviderResult<()> {
    if document.is_empty() || document.len() > MAX_VALIDATION_RESPONSE_BYTES {
        return Err(invalid_validation_response());
    }
    let root = ZeroizingValidationJson::new(
        serde_json::from_slice(document).map_err(|_| invalid_validation_response())?,
    );
    let Some(object) = root.as_value().as_object() else {
        return Err(account_validation_observation(
            invalid_validation_response(),
            root.as_value(),
            ProtocolObservationKind::UnknownResultShape,
        ));
    };
    let Some(code) = object.get("code").and_then(Value::as_i64) else {
        return Err(account_validation_observation(
            invalid_validation_response(),
            root.as_value(),
            ProtocolObservationKind::UnknownResultShape,
        ));
    };
    if code != 1 {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren rejected or expired the imported token",
        ));
    }
    let profile = object
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("user_info"))
        .and_then(Value::as_object);
    if profile.is_none_or(serde_json::Map::is_empty) {
        return Err(account_validation_observation(
            invalid_validation_response(),
            root.as_value(),
            ProtocolObservationKind::UnknownResultShape,
        ));
    }
    Ok(())
}

pub(crate) fn selected_course_id(document: &[u8]) -> ProviderResult<String> {
    classify_token_validation_response(document)?;
    let root = ZeroizingValidationJson::new(
        serde_json::from_slice(document).map_err(|_| invalid_validation_response())?,
    );
    let course_id = root
        .as_value()
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("user_info"))
        .and_then(Value::as_object)
        .and_then(|profile| profile.get("course_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_SELECTED_COURSE_ID_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        .ok_or_else(|| {
            account_validation_observation(
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "Cidaren account response has no valid selected Course identity",
                ),
                root.as_value(),
                ProtocolObservationKind::FieldDrift,
            )
        })?;
    Ok(course_id.to_owned())
}

fn account_validation_observation(
    error: ProviderError,
    root: &Value,
    kind: ProtocolObservationKind,
) -> ProviderError {
    let object = root.as_object();
    let data = object.and_then(|object| object.get("data"));
    let user_info = data
        .and_then(Value::as_object)
        .and_then(|data| data.get("user_info"));
    let profile = user_info.and_then(Value::as_object);
    error_with_protocol_observation(
        error,
        ProtocolSurface::Authentication,
        kind,
        json!({
            "schema": "cidaren.account-validation-observation.v1",
            "root_kind": json_value_kind(Some(root)),
            "code_kind": json_value_kind(object.and_then(|object| object.get("code"))),
            "code_value": object
                .and_then(|object| object.get("code"))
                .and_then(Value::as_i64),
            "data_kind": json_value_kind(data),
            "user_info_kind": json_value_kind(user_info),
            "user_info_fields": profile.map(serde_json::Map::len),
            "course_id_kind": json_value_kind(
                profile.and_then(|profile| profile.get("course_id"))
            ),
        }),
    )
}

struct ZeroizingValidationJson(Value);

impl ZeroizingValidationJson {
    const fn new(value: Value) -> Self {
        Self(value)
    }

    const fn as_value(&self) -> &Value {
        &self.0
    }
}

impl Drop for ZeroizingValidationJson {
    fn drop(&mut self) {
        zeroize_json(&mut self.0);
    }
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(values) => values.values_mut().for_each(zeroize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

const fn valid_session(kind: SessionKind) -> SessionStatus {
    SessionStatus {
        valid: true,
        kind,
        expires_at: None,
        account_hint: None,
    }
}

fn unsupported_auth_method() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "Cidaren received an authentication method it does not advertise",
    )
}

fn invalid_credential_shape() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Authentication,
        "Cidaren credential fields do not match an advertised session shape",
    )
}

fn exact_field(credential: &CredentialBundle, purpose: SecretPurpose) -> ProviderResult<&[u8]> {
    let mut fields = credential
        .fields
        .iter()
        .filter(|field| field.purpose == purpose);
    let value = fields
        .next()
        .map(|field| field.value.expose_secret())
        .ok_or_else(invalid_credential_shape)?;
    if fields.next().is_some() {
        return Err(invalid_credential_shape());
    }
    Ok(value)
}

fn invalid_validation_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Cidaren account endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{AuthSessionId, ProviderAccountId, ProviderId, SecretId};
    use asterism_secrets::{CredentialField, SecretValue};
    use chrono::Utc;

    use super::*;

    const TOKEN_SUCCESS: &[u8] =
        include_bytes!("../../../fixtures/providers/cidaren/auth/token-success.json");
    const TOKEN_REJECTED: &[u8] =
        include_bytes!("../../../fixtures/providers/cidaren/auth/token-rejected.json");

    #[derive(Debug, Default)]
    struct FixtureBoundaries {
        validations: AtomicUsize,
        native_oauth_validations: AtomicUsize,
        oauth_exchanges: AtomicUsize,
    }

    #[async_trait]
    impl CidarenAuthenticationTransport for FixtureBoundaries {
        async fn validate_token(&self, session: &CidarenTokenSession) -> ProviderResult<()> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            assert_eq!(session.expose_token(), "synthetic-user-token");
            classify_token_validation_response(TOKEN_SUCCESS)
        }

        async fn exchange_external_oauth_callback(
            &self,
            callback_url: SecretString,
            binding: ExternalOauthCallbackBinding,
        ) -> ProviderResult<CredentialReplacement> {
            self.oauth_exchanges.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                callback_url.expose_secret(),
                "https://app.vocabgo.com/student/?synthetic-callback"
            );
            assert!(binding.validate());
            Ok(CredentialReplacement {
                session_kind: SessionKind::Composite,
                fields: vec![
                    CredentialField {
                        purpose: SecretPurpose::ProviderAccessToken,
                        value: SecretValue::new(b"synthetic-user-token".to_vec()),
                    },
                    CredentialField {
                        purpose: SecretPurpose::ProviderCompositeSession,
                        value: SecretValue::new(b"synthetic-crypto".to_vec()),
                    },
                ],
            })
        }

        async fn validate_native_oauth_session(
            &self,
            session: &CidarenTokenSession,
        ) -> ProviderResult<()> {
            self.native_oauth_validations.fetch_add(1, Ordering::SeqCst);
            assert!(session.requires_native_oauth_validation());
            assert_eq!(session.expose_token(), "synthetic-user-token");
            classify_token_validation_response(TOKEN_SUCCESS)
        }
    }

    #[async_trait]
    impl CidarenSessionResolver for FixtureBoundaries {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenTokenSession> {
            CidarenTokenSession::try_new("synthetic-user-token")
        }
    }

    #[derive(Debug)]
    struct NativeOauthSessions;

    #[async_trait]
    impl CidarenSessionResolver for NativeOauthSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenTokenSession> {
            CidarenTokenSession::try_new_native_oauth(
                "synthetic-user-token",
                &native_crypto_document(),
            )
        }
    }

    #[test]
    fn validation_response_classifies_expiry_and_drops_profile() {
        classify_token_validation_response(TOKEN_SUCCESS).unwrap();
        assert_eq!(selected_course_id(TOKEN_SUCCESS).unwrap(), "course-a");
        let rejected = classify_token_validation_response(TOKEN_REJECTED).unwrap_err();
        assert_eq!(rejected.kind, ProviderErrorKind::Authentication);
        assert!(rejected.protocol_observation.is_none());
        assert!(!rejected.message.contains("synthetic detail"));
        assert!(classify_token_validation_response(br#"{"code":1,"data":{}}"#).is_err());
        assert!(classify_token_validation_response(b"not-json").is_err());
        assert!(
            selected_course_id(br#"{"code":1,"data":{"user_info":{"course_id":"unsafe/course"}}}"#)
                .is_err()
        );
    }

    #[test]
    fn account_shape_drift_excludes_profile_and_course_values() {
        let error = classify_token_validation_response(
            br#"{"code":1,"data":{"user_info":"must-not-cross-profile"}}"#,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::Authentication);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            observation.shape_sanitized,
            json!({
                "schema": "cidaren.account-validation-observation.v1",
                "root_kind": "object",
                "code_kind": "number",
                "code_value": 1,
                "data_kind": "object",
                "user_info_kind": "string",
                "user_info_fields": null,
                "course_id_kind": "missing",
            })
        );

        let error = selected_course_id(
            br#"{"code":1,"data":{"user_info":{"course_id":"unsafe/course","student_name":"must-not-cross-student"}}}"#,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.kind, ProtocolObservationKind::FieldDrift);
        assert_eq!(observation.shape_sanitized["course_id_kind"], "string");
        assert_eq!(observation.shape_sanitized["user_info_fields"], 2);
        let sanitized = serde_json::to_string(&observation.shape_sanitized).unwrap();
        assert!(!sanitized.contains("unsafe/course"));
        assert!(!sanitized.contains("must-not-cross"));
        assert!(!sanitized.contains("student_name"));

        let error = classify_token_validation_response(
            br#"{"code":"must-not-cross-code","data":{"user_info":{}}}"#,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert_eq!(
            error.protocol_observation.unwrap().shape_sanitized["code_kind"],
            "string"
        );
    }

    #[test]
    fn token_is_opaque_bounded_redacted_and_header_safe() {
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();
        assert_eq!(session.expose_token(), "synthetic-user-token");
        assert!(!session.requires_native_oauth_validation());
        let native_crypto = serde_json::to_vec(&serde_json::json!({
            "login_info": {
                "a": "hc3ludGhldGljLXNoYXJlZC1zZWNyZXQ=",
                "b": "ac3ludGhldGljLXNhbHQ="
            }
        }))
        .unwrap();
        let native =
            CidarenTokenSession::try_new_native_oauth("synthetic-user-token", &native_crypto)
                .unwrap();
        assert!(native.requires_native_oauth_validation());
        assert_eq!(native.session_kind(), SessionKind::Composite);
        assert!(!format!("{session:?}").contains("synthetic"));
        assert!(CidarenTokenSession::try_new("").is_err());
        assert!(CidarenTokenSession::try_new(" padded ").is_err());
        assert!(CidarenTokenSession::try_new("bad\nvalue").is_err());
        assert!(CidarenTokenSession::try_new("x".repeat(MAX_TOKEN_BYTES + 1)).is_err());
    }

    #[tokio::test]
    async fn capability_accepts_exact_manual_and_captured_sessions() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let capability =
            CidarenAuthentication::try_new(boundaries.clone(), boundaries.clone()).unwrap();
        let validated = capability
            .validate_credential(&auth_context(), &imported_bundle())
            .await
            .unwrap();
        assert_eq!(validated.status.kind, SessionKind::ProviderSpecific);
        assert!(validated.replacement.is_none());

        let captured = capability
            .validate_credential(&auth_context(), &captured_bundle())
            .await
            .unwrap();
        assert_eq!(captured.status.kind, SessionKind::Composite);
        assert!(captured.replacement.is_none());

        let captured_token = capability
            .validate_credential(&auth_context(), &captured_token_bundle())
            .await
            .unwrap();
        assert_eq!(captured_token.status.kind, SessionKind::ProviderSpecific);
        assert!(captured_token.replacement.is_none());
        for mutate in [
            |bundle: &mut CredentialBundle| {
                bundle.acquired_via = CredentialAcquisition::AndroidHelper;
            },
            |bundle: &mut CredentialBundle| {
                bundle.auth_method = AuthMethod::ExternalBrowserOauth;
            },
        ] {
            let mut invalid = captured_token_bundle();
            mutate(&mut invalid);
            assert!(
                capability
                    .validate_credential(&auth_context(), &invalid)
                    .await
                    .is_err()
            );
        }

        for method in [
            AuthMethod::AssistedSession,
            AuthMethod::ExternalBrowserOauth,
        ] {
            let mut oauth = captured_bundle();
            oauth.auth_method = method;
            oauth.acquired_via = CredentialAcquisition::NativeProviderLogin;
            assert_eq!(
                capability
                    .validate_credential(&auth_context(), &oauth)
                    .await
                    .unwrap()
                    .status
                    .kind,
                SessionKind::Composite
            );
        }
        let mut mislabeled_oauth = captured_bundle();
        mislabeled_oauth.auth_method = AuthMethod::ExternalBrowserOauth;
        assert!(
            capability
                .validate_credential(&auth_context(), &mislabeled_oauth)
                .await
                .is_err()
        );

        let stored = capability
            .validate_session(&provider_context())
            .await
            .unwrap();
        assert_eq!(stored.kind, SessionKind::ProviderSpecific);
        assert_eq!(boundaries.validations.load(Ordering::SeqCst), 4);
        assert_eq!(
            boundaries.native_oauth_validations.load(Ordering::SeqCst),
            2
        );

        let mut malformed = imported_bundle();
        malformed.session_kind = SessionKind::BearerToken;
        assert!(
            capability
                .validate_credential(&auth_context(), &malformed)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stored_native_oauth_session_keeps_current_validation_context() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let capability =
            CidarenAuthentication::try_new(boundaries.clone(), Arc::new(NativeOauthSessions))
                .unwrap();
        assert_eq!(
            capability
                .validate_session(&provider_context())
                .await
                .unwrap()
                .kind,
            SessionKind::Composite
        );
        assert_eq!(boundaries.validations.load(Ordering::SeqCst), 0);
        assert_eq!(
            boundaries.native_oauth_validations.load(Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn external_oauth_challenge_and_exchange_require_core_binding() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let capability =
            CidarenAuthentication::try_new(boundaries.clone(), boundaries.clone()).unwrap();

        let imported = capability
            .begin_authentication(&auth_context(), AuthMethod::ImportedToken)
            .await
            .unwrap();
        assert_eq!(imported.waiting_for, WaitingUserState::SessionImport);
        assert!(imported.external_oauth.is_none());
        for method in [
            AuthMethod::AssistedSession,
            AuthMethod::ExternalBrowserOauth,
        ] {
            let challenge = capability
                .begin_authentication(&auth_context(), method)
                .await
                .unwrap();
            assert_eq!(challenge.waiting_for, WaitingUserState::BrowserCallback);
            assert!(challenge.user_action.is_some());
            let oauth = challenge.external_oauth.unwrap();
            assert!(oauth.validate());
            assert!(oauth.authorization_url.contains("open.weixin.qq.com"));
            assert!(!format!("{oauth:?}").contains("open.weixin.qq.com"));
        }
        let replacement = capability
            .exchange_external_oauth_callback(
                &auth_context(),
                SecretString::new("https://app.vocabgo.com/student/?synthetic-callback"),
                ExternalOauthCallbackBinding::from_digests([1; 32], [2; 32]),
            )
            .await
            .unwrap();
        assert_eq!(replacement.session_kind, SessionKind::Composite);
        assert_eq!(replacement.fields.len(), 2);
        assert_eq!(boundaries.oauth_exchanges.load(Ordering::SeqCst), 1);
        let mut unbound = auth_context();
        unbound.auth_session_id = None;
        assert!(
            capability
                .begin_authentication(&unbound, AuthMethod::ExternalBrowserOauth)
                .await
                .is_err()
        );
        assert!(
            capability
                .exchange_external_oauth_callback(
                    &unbound,
                    SecretString::new("https://app.vocabgo.com/student/?unbound"),
                    ExternalOauthCallbackBinding::from_digests([1; 32], [2; 32]),
                )
                .await
                .is_err()
        );
        assert!(
            capability
                .exchange_external_oauth_callback(
                    &auth_context(),
                    SecretString::new("https://app.vocabgo.com/student/?invalid-binding"),
                    ExternalOauthCallbackBinding::default(),
                )
                .await
                .is_err()
        );
        assert_eq!(boundaries.oauth_exchanges.load(Ordering::SeqCst), 1);
        assert!(
            capability
                .begin_authentication(&auth_context(), AuthMethod::Password)
                .await
                .is_err()
        );
    }

    #[test]
    fn capture_recipes_prefer_composite_and_expose_token_only_fallback() {
        let boundaries = Arc::new(FixtureBoundaries::default());
        let capability = CidarenAuthentication::try_new(boundaries.clone(), boundaries).unwrap();
        let recipes = capability.capture_recipes();
        assert_eq!(recipes.len(), 2);
        assert_eq!(recipes[0], cidaren_capture_recipe_v2());
        assert_eq!(recipes[1], cidaren_token_capture_recipe_v1());
        assert_eq!(recipes[0].session_kind, SessionKind::Composite);
        assert_eq!(recipes[1].session_kind, SessionKind::ProviderSpecific);
        assert!(recipes.iter().all(|recipe| recipe.validate().is_ok()));
    }

    fn auth_context() -> ProviderAuthContext {
        ProviderAuthContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            auth_session_id: Some(AuthSessionId::new()),
            correlation_id: "cidaren-auth-test".to_owned(),
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-session-test".to_owned(),
        }
    }

    fn imported_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("cidaren").unwrap(),
            tenant: None,
            auth_method: AuthMethod::ImportedToken,
            acquired_via: CredentialAcquisition::ManualImport,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::ProviderSpecific,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderAccessToken,
                value: SecretValue::new(b"synthetic-user-token".to_vec()),
            }],
            user_id_hint: None,
        }
    }

    fn captured_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("cidaren").unwrap(),
            tenant: None,
            auth_method: AuthMethod::AssistedSession,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::Composite,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(b"synthetic-user-token".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderCompositeSession,
                    value: SecretValue::new(
                        serde_json::to_vec(&serde_json::json!({
                            "login_info": {
                                "a": "hc3ludGhldGljLXNoYXJlZC1zZWNyZXQ=",
                                "b": "ac3ludGhldGljLXNhbHQ="
                            }
                        }))
                        .unwrap(),
                    ),
                },
            ],
            user_id_hint: None,
        }
    }

    fn captured_token_bundle() -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("cidaren").unwrap(),
            tenant: None,
            auth_method: AuthMethod::AssistedSession,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at: Utc::now(),
            expires_at: None,
            session_kind: SessionKind::ProviderSpecific,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderAccessToken,
                value: SecretValue::new(b"synthetic-user-token".to_vec()),
            }],
            user_id_hint: None,
        }
    }

    fn native_crypto_document() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "login_info": {
                "a": "hc3ludGhldGljLXNoYXJlZC1zZWNyZXQ=",
                "b": "ac3ludGhldGljLXNhbHQ="
            }
        }))
        .unwrap()
    }
}
