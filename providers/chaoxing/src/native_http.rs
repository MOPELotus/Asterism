use std::{borrow::Cow, fmt, sync::Arc};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderValue, LOCATION, RETRY_AFTER},
};
use scraper::{Html, Selector};
use zeroize::Zeroize;

use crate::{ChaoxingCourseRoute, ChaoxingInventoryDocument, ChaoxingInventoryTransport};

const COURSE_PAGE_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/mycourse/stu";
const EXAM_LIST_BASE: &str = "https://mooc1.chaoxing.com/exam-ans/mooc2/exam/exam-list";
const WORK_LIST_ORIGIN: &str = "https://mooc1.chaoxing.com";
const WORK_LIST_PATH: &str = "/mooc2/work/list";
const MAX_COOKIE_BYTES: usize = 64 * 1_024;
const MAX_HTML_BYTES: usize = 4 * 1_024 * 1_024;

/// One short-lived Chaoxing Cookie header resolved through Core's secrets
/// boundary. Its value is redacted and zeroized on drop.
pub struct ChaoxingCookieSession(SecretString);

impl ChaoxingCookieSession {
    /// Validates one imported or natively established Cookie session.
    ///
    /// # Errors
    ///
    /// Returns an authentication error when the value is empty, malformed,
    /// oversized, or lacks the donor-observed `_uid`/`UID` identity marker.
    pub fn try_new(cookie: impl Into<String>) -> ProviderResult<Self> {
        let cookie = cookie.into();
        if cookie.is_empty()
            || cookie.len() > MAX_COOKIE_BYTES
            || cookie.chars().any(char::is_control)
            || !has_identity_cookie(&cookie)
            || HeaderValue::from_str(&cookie).is_err()
        {
            let mut cookie = cookie;
            cookie.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Cookie session is invalid or lacks an identity marker",
            ));
        }
        Ok(Self(SecretString::new(cookie)))
    }

    fn header_value(&self) -> ProviderResult<HeaderValue> {
        let mut value = HeaderValue::from_str(self.0.expose_secret()).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Cookie session cannot be represented as an HTTP header",
            )
        })?;
        value.set_sensitive(true);
        Ok(value)
    }
}

impl fmt::Debug for ChaoxingCookieSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChaoxingCookieSession([REDACTED])")
    }
}

/// Runtime boundary which resolves opaque credential references for exactly one
/// authorized Provider operation. Implementations must not query plaintext from
/// Provider code or persist the returned session.
#[async_trait]
pub trait ChaoxingSessionResolver: Send + Sync {
    async fn resolve_session(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingCookieSession>;
}

/// Native reqwest implementation of the Chaoxing Work/Exam inventory transport.
/// It remains experimental until the same account is compared with a browser
/// session for the donor-reported `uf` fingerprint binding.
pub struct NativeChaoxingInventoryTransport {
    client: Client,
    sessions: Arc<dyn ChaoxingSessionResolver>,
}

impl NativeChaoxingInventoryTransport {
    /// Builds the transport from the centrally resolved network profile.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error when the shared HTTP client cannot be
    /// initialized.
    pub fn try_new(
        network: &ResolvedNetworkProfile,
        sessions: Arc<dyn ChaoxingSessionResolver>,
    ) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing HTTP client initialization failed",
            )
        })?;
        Ok(Self { client, sessions })
    }

    async fn get_html(
        &self,
        session: &ChaoxingCookieSession,
        url: Url,
    ) -> ProviderResult<SensitiveHtml> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        classify_response(response).await
    }
}

impl fmt::Debug for NativeChaoxingInventoryTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeChaoxingInventoryTransport")
            .field("client", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

#[async_trait]
impl ChaoxingInventoryTransport for NativeChaoxingInventoryTransport {
    async fn fetch_work_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let session = self.sessions.resolve_session(context).await?;
        let course_page = self.get_html(&session, course_page_url(route)?).await?;
        let work_url = discover_work_list_url(course_page.as_str(), route)?;
        self.get_html(&session, work_url)
            .await?
            .into_inventory_document()
    }

    async fn fetch_exam_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let session = self.sessions.resolve_session(context).await?;
        self.get_html(&session, exam_list_url(route)?)
            .await?
            .into_inventory_document()
    }
}

struct SensitiveHtml(String);

impl SensitiveHtml {
    fn as_str(&self) -> &str {
        &self.0
    }

    fn into_inventory_document(mut self) -> ProviderResult<ChaoxingInventoryDocument> {
        ChaoxingInventoryDocument::try_new(std::mem::take(&mut self.0))
    }
}

impl fmt::Debug for SensitiveHtml {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveHtml([REDACTED])")
    }
}

impl Drop for SensitiveHtml {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn course_page_url(route: ChaoxingCourseRoute<'_>) -> ProviderResult<Url> {
    build_url(
        COURSE_PAGE_BASE,
        &[
            ("courseid", route.course_id()),
            ("clazzid", route.class_id()),
            ("cpi", route.cpi()),
            ("ut", "s"),
        ],
    )
}

fn exam_list_url(route: ChaoxingCourseRoute<'_>) -> ProviderResult<Url> {
    build_url(
        EXAM_LIST_BASE,
        &[
            ("courseid", route.course_id()),
            ("clazzid", route.class_id()),
            ("cpi", route.cpi()),
            ("ut", "s"),
        ],
    )
}

fn build_url(base: &str, query: &[(&str, &str)]) -> ProviderResult<Url> {
    let mut url = Url::parse(base).map_err(|_| static_route_error())?;
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    Ok(url)
}

fn discover_work_list_url(html: &str, route: ChaoxingCourseRoute<'_>) -> ProviderResult<Url> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("iframe[src], a[href], [data-url]")
        .expect("static Chaoxing selector must be valid");
    for node in document.select(&selector) {
        let Some(candidate) = ["src", "href", "data-url"]
            .into_iter()
            .find_map(|attribute| node.value().attr(attribute))
        else {
            continue;
        };
        let Ok(url) = Url::parse(candidate)
            .or_else(|_| Url::parse(WORK_LIST_ORIGIN).and_then(|origin| origin.join(candidate)))
        else {
            continue;
        };
        if valid_work_list_url(&url, route) {
            return Ok(url);
        }
    }
    Err(ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "Chaoxing course page has no matching Work inventory route",
    ))
}

fn valid_work_list_url(url: &Url, route: ChaoxingCourseRoute<'_>) -> bool {
    if url.scheme() != "https"
        || url.host_str() != Some("mooc1.chaoxing.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != WORK_LIST_PATH
        || url.fragment().is_some()
    {
        return false;
    }
    unique_query(url, "courseId").as_deref() == Some(route.course_id())
        && unique_query(url, "classId").as_deref() == Some(route.class_id())
        && unique_query(url, "enc").is_some_and(|value| !value.is_empty() && value.len() <= 2_048)
}

#[cfg(test)]
fn query<'a>(url: &'a Url, key: &str) -> Option<Cow<'a, str>> {
    url.query_pairs()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

fn unique_query<'a>(url: &'a Url, key: &str) -> Option<Cow<'a, str>> {
    let mut values = url
        .query_pairs()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

async fn classify_response(response: Response) -> ProviderResult<SensitiveHtml> {
    validate_response_head(&response)?;
    read_response_body(response).await
}

fn validate_response_head(response: &Response) -> ProviderResult<()> {
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "Chaoxing rate limited the inventory request",
        );
        error.retry_after_seconds = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds <= 3_600);
        return Err(error);
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing rejected the current session",
        ));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(ProviderError::new(
            ProviderErrorKind::Authorization,
            "Chaoxing denied access to the inventory route",
        ));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Chaoxing inventory route was not found",
        ));
    }
    if status.is_redirection() {
        let kind = if response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(looks_like_login_location)
        {
            ProviderErrorKind::Authentication
        } else {
            ProviderErrorKind::ProtocolDrift
        };
        return Err(ProviderError::new(
            kind,
            "Chaoxing returned an unhandled inventory redirect",
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "Chaoxing inventory endpoint is temporarily unavailable",
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing inventory endpoint returned an unexpected status",
        ));
    }
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let content_type = content_type.to_str().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing inventory endpoint returned an invalid Content-Type",
            )
        })?;
        let media_type = content_type.split(';').next().unwrap_or_default().trim();
        if !media_type.eq_ignore_ascii_case("text/html")
            && !media_type.eq_ignore_ascii_case("application/xhtml+xml")
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing inventory endpoint returned a non-HTML response",
            ));
        }
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTML_BYTES as u64)
    {
        return Err(oversized_response());
    }

    Ok(())
}

async fn read_response_body(mut response: Response) -> ProviderResult<SensitiveHtml> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_HTML_BYTES {
            bytes.zeroize();
            return Err(oversized_response());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing inventory endpoint returned an empty response",
        ));
    }
    let html = match String::from_utf8(bytes) {
        Ok(html) => html,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing inventory endpoint returned invalid UTF-8",
            ));
        }
    };
    if looks_like_login_page(&html) {
        let mut html = html;
        html.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing returned a login page for the current session",
        ));
    }
    Ok(SensitiveHtml(html))
}

fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::InvalidResponse
    };
    ProviderError::new(kind, "Chaoxing inventory HTTP request failed")
}

fn has_identity_cookie(cookie: &str) -> bool {
    cookie.split(';').any(|field| {
        let Some((name, value)) = field.trim().split_once('=') else {
            return false;
        };
        matches!(name.trim(), "_uid" | "UID") && !value.trim().is_empty()
    })
}

fn looks_like_login_location(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("passport2.chaoxing.com")
                || host.eq_ignore_ascii_case("passport.chaoxing.com")
        }) || url.path().to_ascii_lowercase().contains("login")
    })
}

fn looks_like_login_page(html: &str) -> bool {
    let document = Html::parse_document(html);
    let form = Selector::parse("form[action*='fanyalogin'], form[action*='login']")
        .expect("static Chaoxing selector must be valid");
    let username = Selector::parse("input[name='uname'], input[name='username']")
        .expect("static Chaoxing selector must be valid");
    let password = Selector::parse("input[name='password'][type='password']")
        .expect("static Chaoxing selector must be valid");
    document.select(&form).next().is_some()
        && document.select(&username).next().is_some()
        && document.select(&password).next().is_some()
}

fn oversized_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Chaoxing inventory response exceeds the size limit",
    )
}

fn static_route_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "Chaoxing compile-time inventory route is invalid",
    )
}

#[cfg(test)]
mod tests {
    use asterism_provider_api::{ProviderRouteContext, RemoteCourse};

    use super::*;

    const COURSE_PAGE: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/course-page-with-work-iframe.html");

    #[test]
    fn routes_preserve_course_scope_and_discover_fresh_work_enc() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let course_url = course_page_url(route).unwrap();
        assert_eq!(course_url.host_str(), Some("mooc2-ans.chaoxing.com"));
        assert_eq!(query(&course_url, "courseid").as_deref(), Some("100"));
        assert_eq!(query(&course_url, "clazzid").as_deref(), Some("200"));
        assert_eq!(query(&course_url, "cpi").as_deref(), Some("300"));

        let exam_url = exam_list_url(route).unwrap();
        assert_eq!(exam_url.path(), "/exam-ans/mooc2/exam/exam-list");
        assert_eq!(query(&exam_url, "cpi").as_deref(), Some("300"));

        let work_url = discover_work_list_url(COURSE_PAGE, route).unwrap();
        assert_eq!(work_url.host_str(), Some("mooc1.chaoxing.com"));
        assert_eq!(query(&work_url, "enc").as_deref(), Some("SAFE_ENC"));
    }

    #[test]
    fn work_discovery_rejects_foreign_or_mismatched_routes() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        for candidate in [
            "<iframe src='https://evil.invalid/mooc2/work/list?courseId=100&classId=200&enc=x'>",
            "<iframe src='https://user@mooc1.chaoxing.com/mooc2/work/list?courseId=100&classId=200&enc=x'>",
            "<iframe src='https://mooc1.chaoxing.com:444/mooc2/work/list?courseId=100&classId=200&enc=x'>",
            "<iframe src='https://mooc1.chaoxing.com/mooc2/work/list?courseId=other&classId=200&enc=x'>",
            "<iframe src='https://mooc1.chaoxing.com/mooc2/work/list?courseId=100&classId=200'>",
            "<iframe src='https://mooc1.chaoxing.com/mooc2/work/list?courseId=100&courseId=other&classId=200&enc=x'>",
        ] {
            let error = discover_work_list_url(candidate, route).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        }
    }

    #[test]
    fn cookie_sessions_require_identity_and_redact_debug_output() {
        let session = ChaoxingCookieSession::try_new("_uid=SAFE_UID; uf=SAFE_UF").unwrap();
        assert!(!format!("{session:?}").contains("SAFE_UID"));
        assert!(session.header_value().unwrap().is_sensitive());
        assert!(ChaoxingCookieSession::try_new("uf=missing-identity").is_err());
        assert!(ChaoxingCookieSession::try_new("_uid=bad\nvalue").is_err());
    }

    #[test]
    fn login_detection_requires_structural_form_fields() {
        assert!(looks_like_login_page(
            "<form action='/fanyalogin'><input name='uname'><input name='password' type='password'></form>"
        ));
        assert!(!looks_like_login_page(
            "<script>const password = 'template';</script><div>course</div>"
        ));
        assert!(looks_like_login_location(
            "https://passport2.chaoxing.com/login?refer=safe"
        ));
    }

    #[tokio::test]
    async fn response_statuses_are_classified_without_exposing_bodies() {
        for (status, expected) in [
            (StatusCode::UNAUTHORIZED, ProviderErrorKind::Authentication),
            (StatusCode::FORBIDDEN, ProviderErrorKind::Authorization),
            (StatusCode::NOT_FOUND, ProviderErrorKind::ProtocolDrift),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                ProviderErrorKind::ProviderUnavailable,
            ),
        ] {
            let error = classify_response(response(status, &[], b"private".to_vec()))
                .await
                .unwrap_err();
            assert_eq!(error.kind, expected);
        }

        let error = classify_response(response(
            StatusCode::TOO_MANY_REQUESTS,
            &[("retry-after", "60")],
            Vec::new(),
        ))
        .await
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);
        assert_eq!(error.retry_after_seconds, Some(60));
    }

    #[tokio::test]
    async fn response_reader_accepts_html_and_rejects_login_or_oversize() {
        let html = classify_response(response(
            StatusCode::OK,
            &[("content-type", "text/html; charset=utf-8")],
            b"<html><body>course</body></html>".to_vec(),
        ))
        .await
        .unwrap();
        assert!(html.as_str().contains("course"));

        let login = b"<form action='/fanyalogin'><input name='uname'><input name='password' type='password'></form>".to_vec();
        let error = classify_response(response(StatusCode::OK, &[], login))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);

        let error = classify_response(response(
            StatusCode::OK,
            &[("content-type", "text/html")],
            vec![b'x'; MAX_HTML_BYTES + 1],
        ))
        .await
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    fn response(status: StatusCode, headers: &[(&str, &str)], body: Vec<u8>) -> Response {
        let mut response = http::Response::builder().status(status);
        for (name, value) in headers {
            response = response.header(*name, *value);
        }
        response.body(body).unwrap().into()
    }

    fn course() -> RemoteCourse {
        RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({"safe": true}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        }
    }
}
