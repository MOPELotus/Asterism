use std::{fmt, sync::Arc};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER},
};
use zeroize::Zeroize;

use crate::{UaiCourseInventoryTransport, UaiInventoryDocument, UaiJwtSession, UaiSessionResolver};

const COURSE_LIST_URL: &str = "https://uai.unipus.cn/api/cmgt/course/getCourseListByStudent";
const MAX_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;

/// Native, non-redirecting UAI Course inventory transport.
pub struct NativeUaiInventoryTransport {
    client: Client,
    sessions: Arc<dyn UaiSessionResolver>,
}

impl NativeUaiInventoryTransport {
    /// Builds the transport from the shared network policy and scoped session
    /// resolver.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error if the HTTP client cannot be built.
    pub fn try_new(
        network: &ResolvedNetworkProfile,
        sessions: Arc<dyn UaiSessionResolver>,
    ) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI inventory HTTP client initialization failed",
            )
        })?;
        Ok(Self { client, sessions })
    }

    async fn fetch_courses_with_session(
        &self,
        session: &UaiJwtSession,
    ) -> ProviderResult<UaiInventoryDocument> {
        let authorization = sensitive_authorization(session)?;
        let response = self
            .client
            .get(COURSE_LIST_URL)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, authorization)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_inventory_response(response).await
    }
}

impl fmt::Debug for NativeUaiInventoryTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeUaiInventoryTransport")
            .field("client", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

#[async_trait]
impl UaiCourseInventoryTransport for NativeUaiInventoryTransport {
    async fn fetch_courses(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<UaiInventoryDocument> {
        let session = self.sessions.resolve_session(context).await?;
        self.fetch_courses_with_session(&session).await
    }
}

fn sensitive_authorization(session: &UaiJwtSession) -> ProviderResult<HeaderValue> {
    let mut value = HeaderValue::from_str(session.expose_authorization()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI session contains an invalid Authorization value",
        )
    })?;
    value.set_sensitive(true);
    Ok(value)
}

async fn read_inventory_response(mut response: Response) -> ProviderResult<UaiInventoryDocument> {
    validate_status(response.status(), response.headers())?;
    validate_json_content_type(response.headers())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(oversized_response());
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bytes.zeroize();
            return Err(oversized_response());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI Course inventory endpoint returned an empty response",
        ));
    }
    let document = match String::from_utf8(bytes) {
        Ok(document) => document,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course inventory endpoint returned invalid UTF-8",
            ));
        }
    };
    UaiInventoryDocument::try_new(document)
}

fn validate_status(status: StatusCode, headers: &HeaderMap) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "UAI rate limited the Course inventory request",
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
            "UAI rejected the Course inventory session",
        ));
    }
    if status == StatusCode::NOT_FOUND || status.is_redirection() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI Course inventory route changed or redirected unexpectedly",
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "UAI Course inventory endpoint is temporarily unavailable",
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI Course inventory endpoint returned an unexpected status",
        ));
    }
    Ok(())
}

fn validate_json_content_type(headers: &HeaderMap) -> ProviderResult<()> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course inventory endpoint returned no valid Content-Type",
            )
        })?;
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("application/json")
        && !media_type.to_ascii_lowercase().ends_with("+json")
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI Course inventory endpoint returned an unexpected content type",
        ));
    }
    Ok(())
}

fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::InvalidResponse
    };
    ProviderError::new(kind, "UAI Course inventory HTTP request failed")
}

fn oversized_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI Course inventory response exceeds the size limit",
    )
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;

    use super::*;

    #[derive(Debug)]
    struct FixtureSessions;

    #[async_trait]
    impl UaiSessionResolver for FixtureSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<UaiJwtSession> {
            UaiJwtSession::try_new(
                "synthetic-open-id",
                "SAFE_HEADER.SAFE_PAYLOAD.SAFE_SIGNATURE",
            )
        }
    }

    #[test]
    fn native_transport_uses_shared_client_and_redacts_boundaries() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let transport =
            NativeUaiInventoryTransport::try_new(&network, Arc::new(FixtureSessions)).unwrap();
        let debug = format!("{transport:?}");
        assert!(debug.contains("configured"));
        assert!(!debug.contains("SAFE"));
        assert_eq!(
            COURSE_LIST_URL,
            "https://uai.unipus.cn/api/cmgt/course/getCourseListByStudent"
        );
    }

    #[test]
    fn response_heads_are_typed_and_require_json() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        let limited = validate_status(StatusCode::TOO_MANY_REQUESTS, &headers).unwrap_err();
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(7));
        assert_eq!(
            validate_status(StatusCode::UNAUTHORIZED, &HeaderMap::new())
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            validate_status(StatusCode::FOUND, &HeaderMap::new())
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let mut content = HeaderMap::new();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        validate_json_content_type(&content).unwrap();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        assert!(validate_json_content_type(&content).is_err());
    }

    #[test]
    fn authorization_header_is_sensitive_and_unprefixed() {
        let session = UaiJwtSession::try_new("open-id", "HEADER.PAYLOAD.SIGNATURE").unwrap();
        let header = sensitive_authorization(&session).unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header.to_str().unwrap(), "HEADER.PAYLOAD.SIGNATURE");
    }
}
