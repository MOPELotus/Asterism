use std::{fmt, sync::Arc};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, RemoteCourse,
};
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER},
};
use zeroize::Zeroize;

use crate::{
    UaiCourseInventoryTransport, UaiInventoryDocument, UaiJwtSession, UaiProgressDocument,
    UaiProgressTransport, UaiSessionResolver, UaiTaskInventoryDocuments, UaiTaskInventoryTransport,
    annotator::generate_annotator_token,
    course_inventory::{
        course_resource_id_from_remote, parse_course_context_for_resource_id,
        required_remote_component,
    },
    parse_course_context,
};

const COURSE_LIST_URL: &str = "https://uai.unipus.cn/api/cmgt/course/getCourseListByStudent";
const UAI_ORIGIN: &str = "https://uai.unipus.cn";
const UCONTENT_ORIGIN: &str = "https://ucontent.unipus.cn";
const MAX_INVENTORY_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_PROGRESS_RESPONSE_BYTES: usize = 1_024 * 1_024;

/// Native, non-redirecting UAI Course/Task inventory and progress transport.
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
        UaiInventoryDocument::try_new(
            self.send_get_with_session(
                session,
                static_url(COURSE_LIST_URL)?,
                ResponseRoute::CourseList,
            )
            .await?,
        )
    }

    async fn send_get_with_session(
        &self,
        session: &UaiJwtSession,
        url: Url,
        route: ResponseRoute,
    ) -> ProviderResult<String> {
        let authorization = sensitive_authorization(session)?;
        let response = self
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, authorization)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_json_response(response, route).await
    }

    async fn fetch_tasks_with_session(
        &self,
        session: &UaiJwtSession,
        course: &RemoteCourse,
    ) -> ProviderResult<UaiTaskInventoryDocuments> {
        let resource_id = course_resource_id_from_remote(course)?;
        let detail = UaiInventoryDocument::try_new(
            self.send_get_with_session(
                session,
                course_resource_detail_url(&resource_id)?,
                ResponseRoute::CourseDetail,
            )
            .await?,
        )?;
        let route = parse_course_context(course, detail.as_str())?;
        let tree = UaiInventoryDocument::try_new(
            self.send_get_with_session(
                session,
                course_tree_url(route.course_instance_id())?,
                ResponseRoute::TaskTree,
            )
            .await?,
        )?;
        Ok(UaiTaskInventoryDocuments::new(detail, tree))
    }

    async fn fetch_progress_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiProgressDocument> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "progress Course-resource ID",
        )?;
        let unit_id = required_remote_component(
            Some(&serde_json::Value::String(unit_id.to_owned())),
            "progress Unit ID",
        )?;
        let detail = UaiInventoryDocument::try_new(
            self.send_get_with_session(
                session,
                course_resource_detail_url(&course_resource_id)?,
                ResponseRoute::CourseDetail,
            )
            .await?,
        )?;
        let route = parse_course_context_for_resource_id(course_resource_id, detail.as_str())?;
        let annotator = generate_annotator_token(session.expose_open_id())?;
        let mut annotator_header =
            HeaderValue::from_str(annotator.expose_secret()).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "UAI annotator token cannot be encoded as a request header",
                )
            })?;
        annotator_header.set_sensitive(true);
        let response = self
            .client
            .get(course_progress_url(
                route.course_instance_id(),
                &unit_id,
                session.expose_open_id(),
            )?)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, sensitive_authorization(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        UaiProgressDocument::try_new(read_json_response(response, ResponseRoute::Progress).await?)
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

#[async_trait]
impl UaiTaskInventoryTransport for NativeUaiInventoryTransport {
    async fn fetch_tasks(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<UaiTaskInventoryDocuments> {
        let session = self.sessions.resolve_session(context).await?;
        self.fetch_tasks_with_session(&session, course).await
    }
}

#[async_trait]
impl UaiProgressTransport for NativeUaiInventoryTransport {
    async fn fetch_progress(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiProgressDocument> {
        let session = self.sessions.resolve_session(context).await?;
        self.fetch_progress_with_session(&session, course_resource_id, unit_id)
            .await
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

#[derive(Clone, Copy)]
enum ResponseRoute {
    CourseList,
    CourseDetail,
    TaskTree,
    Progress,
}

impl ResponseRoute {
    const fn label(self) -> &'static str {
        match self {
            Self::CourseList => "Course inventory",
            Self::CourseDetail => "Course-resource detail",
            Self::TaskTree => "Task tree",
            Self::Progress => "Task progress",
        }
    }

    const fn maximum_bytes(self) -> usize {
        match self {
            Self::Progress => MAX_PROGRESS_RESPONSE_BYTES,
            Self::CourseList | Self::CourseDetail | Self::TaskTree => MAX_INVENTORY_RESPONSE_BYTES,
        }
    }
}

async fn read_json_response(
    mut response: Response,
    route: ResponseRoute,
) -> ProviderResult<String> {
    validate_status(response.status(), response.headers(), route)?;
    validate_json_content_type(response.headers(), route)?;
    if response
        .content_length()
        .is_some_and(|length| length > route.maximum_bytes() as u64)
    {
        return Err(oversized_response(route));
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > route.maximum_bytes() {
            bytes.zeroize();
            return Err(oversized_response(route));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!("UAI {} endpoint returned an empty response", route.label()),
        ));
    }
    let document = match String::from_utf8(bytes) {
        Ok(document) => document,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!("UAI {} endpoint returned invalid UTF-8", route.label()),
            ));
        }
    };
    Ok(document)
}

fn validate_status(
    status: StatusCode,
    headers: &HeaderMap,
    route: ResponseRoute,
) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            format!("UAI rate limited the {} request", route.label()),
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
            format!("UAI rejected the {} session", route.label()),
        ));
    }
    if status == StatusCode::NOT_FOUND || status.is_redirection() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!(
                "UAI {} route changed or redirected unexpectedly",
                route.label()
            ),
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            format!("UAI {} endpoint is temporarily unavailable", route.label()),
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            format!(
                "UAI {} endpoint returned an unexpected status",
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
                    "UAI {} endpoint returned no valid Content-Type",
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
                "UAI {} endpoint returned an unexpected content type",
                route.label()
            ),
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
    ProviderError::new(kind, "UAI inventory HTTP request failed")
}

fn oversized_response(route: ResponseRoute) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        format!("UAI {} response exceeds the size limit", route.label()),
    )
}

fn course_resource_detail_url(resource_id: &str) -> ProviderResult<Url> {
    route_url(
        UAI_ORIGIN,
        &[
            "api",
            "cmgt",
            "course",
            "getCourseResourceInfoById",
            resource_id,
        ],
    )
}

fn course_tree_url(course_instance_id: &str) -> ProviderResult<Url> {
    route_url(
        UCONTENT_ORIGIN,
        &["course", "api", "course", course_instance_id, "default"],
    )
}

fn course_progress_url(
    course_instance_id: &str,
    unit_id: &str,
    open_id: &str,
) -> ProviderResult<Url> {
    route_url(
        UCONTENT_ORIGIN,
        &[
            "course",
            "api",
            "v2",
            "course_progress",
            course_instance_id,
            unit_id,
            open_id,
            "default",
        ],
    )
}

fn route_url(origin: &'static str, segments: &[&str]) -> ProviderResult<Url> {
    let mut url = static_url(origin)?;
    url.path_segments_mut()
        .map_err(|()| static_route_error())?
        .clear()
        .extend(segments);
    Ok(url)
}

fn static_url(value: &'static str) -> ProviderResult<Url> {
    Url::parse(value).map_err(|_| static_route_error())
}

fn static_route_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI compile-time inventory route is invalid",
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
        let limited = validate_status(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            ResponseRoute::CourseList,
        )
        .unwrap_err();
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(7));
        assert_eq!(
            validate_status(
                StatusCode::UNAUTHORIZED,
                &HeaderMap::new(),
                ResponseRoute::CourseDetail,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            validate_status(
                StatusCode::FOUND,
                &HeaderMap::new(),
                ResponseRoute::TaskTree,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let mut content = HeaderMap::new();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        validate_json_content_type(&content, ResponseRoute::CourseList).unwrap();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        assert!(validate_json_content_type(&content, ResponseRoute::TaskTree).is_err());
    }

    #[test]
    fn authorization_header_is_sensitive_and_unprefixed() {
        let session = UaiJwtSession::try_new("open-id", "HEADER.PAYLOAD.SIGNATURE").unwrap();
        let header = sensitive_authorization(&session).unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header.to_str().unwrap(), "HEADER.PAYLOAD.SIGNATURE");
    }

    #[test]
    fn task_routes_are_exact_and_path_encode_fresh_identities() {
        assert_eq!(
            course_resource_detail_url("resource-42").unwrap().as_str(),
            "https://uai.unipus.cn/api/cmgt/course/getCourseResourceInfoById/resource-42"
        );
        assert_eq!(
            course_tree_url("course-v2:synthetic+rw/guard")
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/course/api/course/course-v2:synthetic+rw%2Fguard/default"
        );
        assert_eq!(
            course_progress_url("course-v2:synthetic+rw", "unit-1", "open-id")
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/course/api/v2/course_progress/course-v2:synthetic+rw/unit-1/open-id/default"
        );
        assert_eq!(ResponseRoute::Progress.maximum_bytes(), 1_024 * 1_024);
    }
}
