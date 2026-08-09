use std::fmt;

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER},
};
use serde::{Deserialize, Deserializer, Serialize, de::Visitor};
use zeroize::Zeroize;

use crate::{UaiAuthenticationTransport, UaiJwtSession, classify_password_login_response};

const LOGIN_URL: &str = "https://sso.unipus.cn/sso/0.1/sso/login";
const LOGIN_SERVICE: &str = "https://uai.unipus.cn/home";
const USER_INFO_URL: &str = "https://uai.unipus.cn/api/account/user/info";
const MAX_AUTH_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_IDENTITY_BYTES: usize = 512;

/// Native Password exchange and JWT validation over the shared network policy.
pub struct NativeUaiAuthenticationTransport {
    client: Client,
}

impl NativeUaiAuthenticationTransport {
    /// Builds a transport from the shared, non-redirecting HTTPS client.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error if the resolved client cannot be
    /// initialized.
    pub fn try_new(network: &ResolvedNetworkProfile) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI authentication HTTP client initialization failed",
            )
        })?;
        Ok(Self { client })
    }
}

impl fmt::Debug for NativeUaiAuthenticationTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeUaiAuthenticationTransport")
            .field("client", &"configured")
            .finish()
    }
}

#[async_trait]
impl UaiAuthenticationTransport for NativeUaiAuthenticationTransport {
    async fn exchange_password(
        &self,
        username: &SecretString,
        password: &SecretString,
    ) -> ProviderResult<UaiJwtSession> {
        let response = self
            .client
            .post(LOGIN_URL)
            .header(ACCEPT, "application/json")
            .json(&PasswordLoginRequest {
                username: username.expose_secret(),
                password: password.expose_secret(),
                remember: true,
                agreement: true,
                service: LOGIN_SERVICE,
            })
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let mut document = read_json_response(response, "login").await?;
        let result = classify_password_login_response(&document);
        document.zeroize();
        result
    }

    async fn validate_jwt(&self, session: &UaiJwtSession) -> ProviderResult<()> {
        let mut authorization =
            HeaderValue::from_str(session.expose_authorization()).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Authentication,
                    "UAI session contains an invalid Authorization value",
                )
            })?;
        authorization.set_sensitive(true);
        let response = self
            .client
            .get(USER_INFO_URL)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, authorization)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let mut document = read_json_response(response, "user-info").await?;
        let result = validate_user_info_response(&document);
        document.zeroize();
        result
    }
}

#[derive(Serialize)]
struct PasswordLoginRequest<'a> {
    username: &'a str,
    password: &'a str,
    remember: bool,
    agreement: bool,
    service: &'static str,
}

async fn read_json_response(
    mut response: Response,
    route: &'static str,
) -> ProviderResult<Vec<u8>> {
    validate_status(response.status(), response.headers(), route)?;
    validate_json_content_type(response.headers(), route)?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AUTH_RESPONSE_BYTES as u64)
    {
        return Err(oversized_response(route));
    }

    let mut document = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if document.len().saturating_add(chunk.len()) > MAX_AUTH_RESPONSE_BYTES {
            document.zeroize();
            return Err(oversized_response(route));
        }
        document.extend_from_slice(&chunk);
    }
    if document.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("UAI {route} endpoint returned an empty response"),
        ));
    }
    if std::str::from_utf8(&document).is_err() {
        document.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("UAI {route} endpoint returned invalid UTF-8"),
        ));
    }
    Ok(document)
}

fn validate_status(
    status: StatusCode,
    headers: &HeaderMap,
    route: &'static str,
) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            format!("UAI rate limited the {route} request"),
        );
        error.retry_after_seconds = headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds <= 3_600);
        return Err(error);
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            format!("UAI rejected the {route} authentication"),
        ));
    }
    if status == StatusCode::NOT_FOUND || status.is_redirection() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("UAI {route} route changed or redirected unexpectedly"),
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            format!("UAI {route} endpoint is temporarily unavailable"),
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("UAI {route} endpoint returned an unexpected status"),
        ));
    }
    Ok(())
}

fn validate_json_content_type(headers: &HeaderMap, route: &'static str) -> ProviderResult<()> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!("UAI {route} endpoint returned no valid Content-Type"),
            )
        })?;
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("application/json")
        && !media_type.to_ascii_lowercase().ends_with("+json")
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("UAI {route} endpoint returned an unexpected content type"),
        ));
    }
    Ok(())
}

fn validate_user_info_response(document: &[u8]) -> ProviderResult<()> {
    if document.is_empty() || document.len() > MAX_AUTH_RESPONSE_BYTES {
        return Err(invalid_user_info_response());
    }
    let envelope: UserInfoEnvelope =
        serde_json::from_slice(document).map_err(|_| invalid_user_info_response())?;
    if envelope.success == Some(false) {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI user-info endpoint rejected the current session",
        ));
    }
    let user = envelope
        .value
        .and_then(|value| value.user_info)
        .ok_or_else(invalid_user_info_response)?;
    if !user.app_user_id.0 || !user.sso_id.0 {
        return Err(invalid_user_info_response());
    }
    Ok(())
}

#[derive(Deserialize)]
struct UserInfoEnvelope {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    value: Option<UserInfoValue>,
}

#[derive(Deserialize)]
struct UserInfoValue {
    #[serde(default, rename = "userInfo")]
    user_info: Option<UserInfo>,
}

#[derive(Deserialize)]
struct UserInfo {
    #[serde(rename = "appUserId")]
    app_user_id: IdentityMarker,
    #[serde(rename = "ssoId")]
    sso_id: IdentityMarker,
}

struct IdentityMarker(bool);

impl<'de> Deserialize<'de> for IdentityMarker {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MarkerVisitor;

        impl Visitor<'_> for MarkerVisitor {
            type Value = IdentityMarker;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded non-empty identity string or positive integer")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(IdentityMarker(
                    !value.is_empty()
                        && value.len() <= MAX_IDENTITY_BYTES
                        && !value.chars().any(char::is_control),
                ))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(IdentityMarker(value > 0))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(IdentityMarker(value > 0))
            }
        }

        deserializer.deserialize_any(MarkerVisitor)
    }
}

fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::InvalidResponse
    };
    ProviderError::new(kind, "UAI authentication HTTP request failed")
}

fn oversized_response(route: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        format!("UAI {route} response exceeds the size limit"),
    )
}

fn invalid_user_info_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI user-info endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;

    use super::*;

    const USER_INFO: &[u8] =
        include_bytes!("../../../fixtures/providers/uai/auth/user-info-valid.json");

    #[test]
    fn native_transport_uses_the_shared_client() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let transport = NativeUaiAuthenticationTransport::try_new(&network).unwrap();
        assert!(format!("{transport:?}").contains("configured"));
    }

    #[test]
    fn password_request_matches_the_audited_json_contract() {
        let document = serde_json::to_value(PasswordLoginRequest {
            username: "synthetic-user",
            password: "synthetic-password",
            remember: true,
            agreement: true,
            service: LOGIN_SERVICE,
        })
        .unwrap();
        assert_eq!(
            document,
            serde_json::json!({
                "username": "synthetic-user",
                "password": "synthetic-password",
                "remember": true,
                "agreement": true,
                "service": "https://uai.unipus.cn/home"
            })
        );
    }

    #[test]
    fn user_info_requires_both_bounded_identity_markers() {
        validate_user_info_response(USER_INFO).unwrap();
        validate_user_info_response(
            br#"{"success":true,"value":{"userInfo":{"appUserId":42,"ssoId":"synthetic-sso"}}}"#,
        )
        .unwrap();
        assert!(
            validate_user_info_response(
                br#"{"success":true,"value":{"userInfo":{"appUserId":"synthetic"}}}"#
            )
            .is_err()
        );
        assert!(
            validate_user_info_response(
                br#"{"success":false,"value":{"userInfo":{"appUserId":42,"ssoId":"id"}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn response_head_classification_is_typed() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        let limited =
            validate_status(StatusCode::TOO_MANY_REQUESTS, &headers, "login").unwrap_err();
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(12));
        assert_eq!(
            validate_status(StatusCode::UNAUTHORIZED, &HeaderMap::new(), "user-info")
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            validate_status(StatusCode::FOUND, &HeaderMap::new(), "login")
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        assert_eq!(
            validate_status(StatusCode::BAD_GATEWAY, &HeaderMap::new(), "login")
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProviderUnavailable
        );
    }

    #[test]
    fn only_json_media_types_are_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json; charset=utf-8"),
        );
        validate_json_content_type(&headers, "login").unwrap();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        assert!(validate_json_content_type(&headers, "login").is_err());
        assert!(validate_json_content_type(&HeaderMap::new(), "login").is_err());
    }
}
