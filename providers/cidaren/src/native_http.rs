use std::{fmt, sync::Arc};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::{
    Client, RequestBuilder, Response, StatusCode,
    header::{
        ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, REFERER,
        RETRY_AFTER, USER_AGENT,
    },
};
use serde::Serialize;
use zeroize::Zeroize;

use crate::{
    CidarenAuthenticationTransport, CidarenClassTaskPageDocument, CidarenClassTaskTransport,
    CidarenSessionResolver, CidarenStudyTaskDocument, CidarenStudyTaskTransport,
    CidarenTokenSession, authentication::selected_course_id, class_tasks::class_task_total,
    classify_token_validation_response,
};

const STUDENT_MAIN_URL: &str = "https://app.vocabgo.com/student/api/Student/Main";
const CLASS_TASK_PAGE_URL: &str = "https://app.vocabgo.com/student/api/Student/ClassTask/PageTask";
const STUDY_TASK_LIST_URL: &str = "https://app.vocabgo.com/student/api/Student/StudyTask/List";
const STUDENT_REFERER: &str = "https://app.vocabgo.com/student/";
const DONOR_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 8.1.2; LIO-AN00 Build/LIO-AN00; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/92.0.4515.131 Safari/537.36 MMWEBID/4462 MicroMessenger/8.0.20.2100(0x28001438) Process/toolsmp WeChat/arm64 Weixin Android Tablet NetType/WIFI Language/zh_CN ABI/arm64";
const ABC_HEADER_VALUE: &str = "60cd4becac3c293c3107d9a8087e7f47";
const AUTHORIZATION_V_READ: &str = "cfcd208495d565ef66e7dff9f98764da";
const REQUEST_VERSION: &str = "2.6.1.231204";
const SIGNING_VERSION: &str = "2.6.1.240122";
const SIGNING_SUFFIX: &str = "ajfajfamsnfaflfasakljdlalkflak";
const PAGE_SIZE: u32 = 10;
const MAX_TASKS: usize = 10_000;
const MAX_AUTH_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_PAGE_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_STUDY_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;

const USER_TOKEN: HeaderName = HeaderName::from_static("usertoken");
const ABC: HeaderName = HeaderName::from_static("abc");
const AUTHORIZATION_V: HeaderName = HeaderName::from_static("authorization-v");
const X_REQUESTED_WITH: HeaderName = HeaderName::from_static("x-requested-with");

/// Native imported-token validation plus class/study Task HTTP transport over
/// the shared non-redirecting network policy.
pub struct NativeCidarenTransport {
    client: Client,
    sessions: Arc<dyn CidarenSessionResolver>,
}

impl NativeCidarenTransport {
    /// Builds the transport from one resolved shared network profile.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error if the shared client cannot be
    /// initialized.
    pub fn try_new(
        network: &ResolvedNetworkProfile,
        sessions: Arc<dyn CidarenSessionResolver>,
    ) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren HTTP client initialization failed",
            )
        })?;
        Ok(Self { client, sessions })
    }

    async fn fetch_account_document(
        &self,
        session: &CidarenTokenSession,
    ) -> ProviderResult<String> {
        let timestamp = current_timestamp_millis()?;
        let request = self.client.get(STUDENT_MAIN_URL).query(&[
            ("timestamp", timestamp.to_string()),
            ("version", REQUEST_VERSION.to_owned()),
            ("app_type", "1".to_owned()),
        ]);
        let response = authenticated_request(request, session)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::AccountValidation))?;
        read_json_response(
            response,
            ResponseRoute::AccountValidation,
            MAX_AUTH_RESPONSE_BYTES,
        )
        .await
    }

    async fn fetch_task_page(
        &self,
        session: &CidarenTokenSession,
        page_count: u32,
    ) -> ProviderResult<CidarenClassTaskPageDocument> {
        let timestamp = current_timestamp_millis()?;
        let body = class_task_request(page_count, timestamp)?;
        let request = self.client.post(CLASS_TASK_PAGE_URL).json(&body);
        let response = authenticated_request(request, session)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::ClassTaskPage))?;
        let document = read_json_response(
            response,
            ResponseRoute::ClassTaskPage,
            MAX_PAGE_RESPONSE_BYTES,
        )
        .await?;
        CidarenClassTaskPageDocument::try_new(page_count, document)
    }

    async fn fetch_study_document(
        &self,
        session: &CidarenTokenSession,
    ) -> ProviderResult<CidarenStudyTaskDocument> {
        let mut account = self.fetch_account_document(session).await?;
        let course_id_result = selected_course_id(account.as_bytes());
        account.zeroize();
        let course_id = course_id_result?;
        let timestamp = current_timestamp_millis()?;
        let response = study_task_request(&self.client, session, &course_id, timestamp)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::StudyTaskList))?;
        let document = read_json_response(
            response,
            ResponseRoute::StudyTaskList,
            MAX_STUDY_RESPONSE_BYTES,
        )
        .await?;
        CidarenStudyTaskDocument::try_new(course_id, document)
    }
}

impl fmt::Debug for NativeCidarenTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCidarenTransport")
            .field("client", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

#[async_trait]
impl CidarenAuthenticationTransport for NativeCidarenTransport {
    async fn validate_token(&self, session: &CidarenTokenSession) -> ProviderResult<()> {
        let mut document = self.fetch_account_document(session).await?;
        let result = classify_token_validation_response(document.as_bytes());
        document.zeroize();
        result
    }
}

#[async_trait]
impl CidarenClassTaskTransport for NativeCidarenTransport {
    async fn fetch_class_task_pages(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Vec<CidarenClassTaskPageDocument>> {
        let session = self.sessions.resolve_session(context).await?;
        let first = self.fetch_task_page(&session, 1).await?;
        let total = class_task_total(first.as_str())?;
        if total > MAX_TASKS {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Cidaren class-task total exceeds the native scan limit",
            ));
        }
        let page_count = total.div_ceil(PAGE_SIZE as usize).max(1);
        let mut pages = Vec::with_capacity(page_count);
        pages.push(first);
        for page in 2..=page_count {
            pages.push(
                self.fetch_task_page(
                    &session,
                    u32::try_from(page).map_err(|_| {
                        ProviderError::new(
                            ProviderErrorKind::InvalidResponse,
                            "Cidaren class-task page count exceeds the limit",
                        )
                    })?,
                )
                .await?,
            );
        }
        Ok(pages)
    }
}

#[async_trait]
impl CidarenStudyTaskTransport for NativeCidarenTransport {
    async fn fetch_study_task_document(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<CidarenStudyTaskDocument> {
        let session = self.sessions.resolve_session(context).await?;
        self.fetch_study_document(&session).await
    }
}

#[derive(Debug, Serialize)]
struct ClassTaskRequest {
    search_type: &'static str,
    page_count: u32,
    page_size: u32,
    timestamp: u64,
    version: &'static str,
    sign: String,
    app_type: u8,
}

fn class_task_request(page_count: u32, timestamp: u64) -> ProviderResult<ClassTaskRequest> {
    if page_count == 0 || page_count > 1_000 {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren class-task request contains an invalid page number",
        ));
    }
    let signature_input = format!(
        "page_count={page_count}&page_size={PAGE_SIZE}&search_type=0&timestamp={timestamp}&version={SIGNING_VERSION}{SIGNING_SUFFIX}"
    );
    Ok(ClassTaskRequest {
        search_type: "0",
        page_count,
        page_size: PAGE_SIZE,
        timestamp,
        version: REQUEST_VERSION,
        sign: format!("{:x}", md5::compute(signature_input.as_bytes())),
        app_type: 1,
    })
}

fn study_task_request(
    client: &Client,
    session: &CidarenTokenSession,
    course_id: &str,
    timestamp: u64,
) -> ProviderResult<RequestBuilder> {
    let request = client.get(STUDY_TASK_LIST_URL).query(&[
        ("course_id", course_id.to_owned()),
        ("timestamp", timestamp.to_string()),
        ("version", REQUEST_VERSION.to_owned()),
        ("app_type", "1".to_owned()),
    ]);
    authenticated_request(request, session)
}

fn authenticated_request(
    request: RequestBuilder,
    session: &CidarenTokenSession,
) -> ProviderResult<RequestBuilder> {
    let mut token = HeaderValue::from_str(session.expose_token()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren stored token cannot be encoded as a request header",
        )
    })?;
    token.set_sensitive(true);
    Ok(request
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(ABC, ABC_HEADER_VALUE)
        .header(AUTHORIZATION_V, AUTHORIZATION_V_READ)
        .header(X_REQUESTED_WITH, "XMLHttpRequest")
        .header(USER_AGENT, DONOR_USER_AGENT)
        .header(ACCEPT_LANGUAGE, "*")
        .header(REFERER, STUDENT_REFERER)
        .header(USER_TOKEN, token))
}

async fn read_json_response(
    mut response: Response,
    route: ResponseRoute,
    maximum: usize,
) -> ProviderResult<String> {
    validate_status(response.status(), response.headers(), route)?;
    validate_json_content_type(response.headers(), route)?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(oversized_response(route));
    }
    let mut document = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error, route))?
    {
        if document.len().saturating_add(chunk.len()) > maximum {
            document.zeroize();
            return Err(oversized_response(route));
        }
        document.extend_from_slice(&chunk);
    }
    if document.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!(
                "Cidaren {} endpoint returned an empty response",
                route.label()
            ),
        ));
    }
    String::from_utf8(document).map_err(|error| {
        let mut bytes = error.into_bytes();
        bytes.zeroize();
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("Cidaren {} endpoint returned invalid UTF-8", route.label()),
        )
    })
}

fn validate_status(
    status: StatusCode,
    headers: &HeaderMap,
    route: ResponseRoute,
) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            format!("Cidaren rate limited the {} request", route.label()),
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
            format!("Cidaren rejected the {} authentication", route.label()),
        ));
    }
    if status == StatusCode::NOT_FOUND || status.is_redirection() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!(
                "Cidaren {} route changed or redirected unexpectedly",
                route.label()
            ),
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            format!(
                "Cidaren {} endpoint is temporarily unavailable",
                route.label()
            ),
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!(
                "Cidaren {} endpoint returned an unexpected status",
                route.label()
            ),
        ));
    }
    Ok(())
}

fn validate_json_content_type(headers: &HeaderMap, route: ResponseRoute) -> ProviderResult<()> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!(
                    "Cidaren {} endpoint returned no valid Content-Type",
                    route.label()
                ),
            )
        })?;
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("application/json")
        && !media_type.to_ascii_lowercase().ends_with("+json")
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!(
                "Cidaren {} endpoint returned an unexpected content type",
                route.label()
            ),
        ));
    }
    Ok(())
}

fn current_timestamp_millis() -> ProviderResult<u64> {
    u64::try_from(Utc::now().timestamp_millis()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren request clock is outside the supported range",
        )
    })
}

fn classify_reqwest_error(error: &reqwest::Error, route: ResponseRoute) -> ProviderError {
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::InvalidResponse
    };
    ProviderError::new(
        kind,
        format!("Cidaren {} HTTP request failed", route.label()),
    )
}

fn oversized_response(route: ResponseRoute) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        format!("Cidaren {} response exceeds the size limit", route.label()),
    )
}

#[derive(Clone, Copy, Debug)]
enum ResponseRoute {
    AccountValidation,
    ClassTaskPage,
    StudyTaskList,
}

impl ResponseRoute {
    const fn label(self) -> &'static str {
        match self {
            Self::AccountValidation => "account-validation",
            Self::ClassTaskPage => "class-task",
            Self::StudyTaskList => "study-task",
        }
    }
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;

    use super::*;

    #[derive(Debug)]
    struct FixtureSessions;

    #[async_trait]
    impl CidarenSessionResolver for FixtureSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenTokenSession> {
            CidarenTokenSession::try_new("synthetic-user-token")
        }
    }

    #[test]
    fn native_transport_uses_shared_client_and_redacts_boundaries() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let transport =
            NativeCidarenTransport::try_new(&network, Arc::new(FixtureSessions)).unwrap();
        let debug = format!("{transport:?}");
        assert!(debug.contains("configured"));
        assert!(!debug.contains("synthetic"));
        assert_eq!(
            STUDENT_MAIN_URL,
            "https://app.vocabgo.com/student/api/Student/Main"
        );
        assert_eq!(
            CLASS_TASK_PAGE_URL,
            "https://app.vocabgo.com/student/api/Student/ClassTask/PageTask"
        );
    }

    #[test]
    fn class_task_request_preserves_body_and_signing_version_split() {
        let request = class_task_request(1, 1_730_000_000_000).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "search_type": "0",
                "page_count": 1,
                "page_size": 10,
                "timestamp": 1_730_000_000_000_u64,
                "version": "2.6.1.231204",
                "sign": "02ba8c77f1e4d3c5e9da4f6719647f49",
                "app_type": 1
            })
        );
        assert!(class_task_request(0, 1).is_err());
    }

    #[test]
    fn study_task_request_is_bound_to_selected_course_and_read_headers() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let client = build_http_client(&network).unwrap();
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();
        let request = study_task_request(&client, &session, "course-a", 1_730_000_000_000)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().path(), "/student/api/Student/StudyTask/List");
        let query = request
            .url()
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            query.get("course_id").map(std::convert::AsRef::as_ref),
            Some("course-a")
        );
        assert_eq!(
            query.get("version").map(std::convert::AsRef::as_ref),
            Some(REQUEST_VERSION)
        );
        assert_eq!(
            query.get("app_type").map(std::convert::AsRef::as_ref),
            Some("1")
        );
        let token = request.headers().get(USER_TOKEN).unwrap();
        assert!(token.is_sensitive());
        assert_eq!(
            request.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );
    }

    #[test]
    fn token_header_is_sensitive_and_donor_headers_are_frozen() {
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();
        let client = Client::new();
        let request = authenticated_request(client.get(STUDENT_MAIN_URL), &session)
            .unwrap()
            .build()
            .unwrap();
        let token = request.headers().get(USER_TOKEN).unwrap();
        assert!(token.is_sensitive());
        assert_eq!(token.to_str().unwrap(), "synthetic-user-token");
        assert_eq!(request.headers().get(ABC).unwrap(), ABC_HEADER_VALUE);
        assert_eq!(
            request.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );
        assert_eq!(request.headers().get(USER_AGENT).unwrap(), DONOR_USER_AGENT);
    }

    #[test]
    fn response_heads_are_typed_and_require_json() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("9"));
        let limited = validate_status(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            ResponseRoute::ClassTaskPage,
        )
        .unwrap_err();
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(9));
        assert_eq!(
            validate_status(
                StatusCode::UNAUTHORIZED,
                &HeaderMap::new(),
                ResponseRoute::AccountValidation,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            validate_status(
                StatusCode::FOUND,
                &HeaderMap::new(),
                ResponseRoute::ClassTaskPage,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let mut content = HeaderMap::new();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        validate_json_content_type(&content, ResponseRoute::ClassTaskPage).unwrap();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        assert!(validate_json_content_type(&content, ResponseRoute::AccountValidation).is_err());
    }

    #[test]
    fn total_parser_drives_bounded_native_pagination() {
        assert_eq!(
            class_task_total(include_str!(
                "../../../fixtures/providers/cidaren/tasks/class-task-page-1.json"
            ))
            .unwrap(),
            12
        );
    }
}
