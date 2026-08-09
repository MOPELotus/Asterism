use std::{fmt, sync::Arc};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, RemoteCourse,
};
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{CONTENT_TYPE, COOKIE, HeaderMap, LOCATION, REFERER, RETRY_AFTER},
};
use zeroize::Zeroize;

use crate::{
    WellearnCmiDocument, WellearnCmiTransport, WellearnCourseInventoryTransport,
    WellearnInventoryDocument, WellearnScoLeavesDocument, WellearnSessionResolver,
    WellearnTaskInventoryDocuments, WellearnTaskInventoryTransport,
    course_context::{parse_course_context, parse_course_context_for_id},
    course_inventory::course_id_from_remote,
    task_inventory::unit_count,
};

const COURSE_LIST_URL: &str = "https://welearn.sflep.com/ajax/authCourse.aspx?action=gmc";
const COURSE_INDEX_REFERER: &str = "https://welearn.sflep.com/student/index.aspx";
const COURSE_INFO_ORIGIN: &str = "https://welearn.sflep.com";
const COURSE_INFO_PATH: &str = "/student/course_info.aspx";
const STUDY_STAT_URL: &str = "https://welearn.sflep.com/ajax/StudyStat.aspx";
const SCO_URL: &str = "https://welearn.sflep.com/Ajax/SCO.aspx";
const STUDY_COURSE_REFERER: &str = "https://welearn.sflep.com/student/StudyCourse.aspx";
const MAX_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;

/// Native, non-redirecting `WELearn` Course/Unit/SCO inventory transport.
pub struct NativeWellearnInventoryTransport {
    client: Client,
    sessions: Arc<dyn WellearnSessionResolver>,
}

impl NativeWellearnInventoryTransport {
    /// Builds the transport from the shared network policy and account-scoped
    /// stored-session resolver.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error if the shared HTTP client cannot be
    /// initialized.
    pub fn try_new(
        network: &ResolvedNetworkProfile,
        sessions: Arc<dyn WellearnSessionResolver>,
    ) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn inventory HTTP client initialization failed",
            )
        })?;
        Ok(Self { client, sessions })
    }

    async fn send_get_with_session(
        &self,
        session: &crate::WellearnCookieSession,
        url: Url,
        referer: &str,
        expected_content: ResponseContent,
    ) -> ProviderResult<WellearnInventoryDocument> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, referer)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_inventory_response(response, expected_content).await
    }

    async fn session_for_operation(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<(crate::WellearnCookieSession, bool)> {
        match self.sessions.resolve_session(context).await {
            Ok(session) => Ok((session, false)),
            Err(error) if error.kind == ProviderErrorKind::Authentication => {
                Ok((self.sessions.renew_session(context).await?, true))
            }
            Err(error) => Err(error),
        }
    }

    async fn fetch_tasks_once(
        &self,
        session: &crate::WellearnCookieSession,
        course: &RemoteCourse,
    ) -> ProviderResult<WellearnTaskInventoryDocuments> {
        let course_id = course_id_from_remote(course)?;
        let course_url = course_info_url(&course_id)?;
        let course_page = self
            .send_get_with_session(
                session,
                course_url.clone(),
                COURSE_INDEX_REFERER,
                ResponseContent::Html,
            )
            .await?;
        let route = parse_course_context(course, course_page.as_str())?;
        let unit_response = self
            .client
            .post(static_url(STUDY_STAT_URL)?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, course_url.as_str())
            .form(&[
                ("action", "courseunits"),
                ("cid", route.course_id()),
                ("uid", route.user_id()),
            ])
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let units = read_inventory_response(unit_response, ResponseContent::Json).await?;
        let count = unit_count(units.as_str())?;
        let mut leaves = Vec::with_capacity(count);
        for index in 0..count {
            let unit_index = u32::try_from(index).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "WELearn Unit index exceeds the supported range",
                )
            })?;
            let leaves_url = sco_leaves_url(&route, unit_index)?;
            let document = self
                .send_get_with_session(
                    session,
                    leaves_url,
                    course_url.as_str(),
                    ResponseContent::Json,
                )
                .await?;
            leaves.push(WellearnScoLeavesDocument::try_new(
                unit_index,
                document.as_str(),
            )?);
        }
        Ok(WellearnTaskInventoryDocuments::new(units, leaves))
    }

    async fn fetch_cmi_once(
        &self,
        session: &crate::WellearnCookieSession,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument> {
        let course_page = self
            .send_get_with_session(
                session,
                course_info_url(course_id)?,
                COURSE_INDEX_REFERER,
                ResponseContent::Html,
            )
            .await?;
        let route = parse_course_context_for_id(course_id.to_owned(), course_page.as_str())?;
        let response = self
            .client
            .post(sco_url(route.user_id())?)
            .header(COOKIE, session.expose_secret())
            .header(REFERER, STUDY_COURSE_REFERER)
            .form(&[
                ("action", "getscoinfo_v7"),
                ("uid", route.user_id()),
                ("cid", route.course_id()),
                ("scoid", sco_id),
            ])
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let document = read_inventory_response(response, ResponseContent::Json).await?;
        WellearnCmiDocument::try_new(document.as_str().to_owned())
    }
}

impl fmt::Debug for NativeWellearnInventoryTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeWellearnInventoryTransport")
            .field("client", &"configured")
            .field("sessions", &"configured")
            .finish()
    }
}

#[async_trait]
impl WellearnCourseInventoryTransport for NativeWellearnInventoryTransport {
    async fn fetch_courses(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnInventoryDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match fetch_course_inventory_document(&self.client, &session).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                fetch_course_inventory_document(&self.client, &session).await
            }
            result => result,
        }
    }
}

pub(crate) async fn fetch_course_inventory_document(
    client: &Client,
    session: &crate::WellearnCookieSession,
) -> ProviderResult<WellearnInventoryDocument> {
    let response = client
        .get(static_url(COURSE_LIST_URL)?)
        .header(COOKIE, session.expose_secret())
        .header(REFERER, COURSE_INDEX_REFERER)
        .send()
        .await
        .map_err(|error| classify_reqwest_error(&error))?;
    read_inventory_response(response, ResponseContent::Json).await
}

#[async_trait]
impl WellearnTaskInventoryTransport for NativeWellearnInventoryTransport {
    async fn fetch_tasks(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<WellearnTaskInventoryDocuments> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_tasks_once(&session, course).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_tasks_once(&session, course).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl WellearnCmiTransport for NativeWellearnInventoryTransport {
    async fn fetch_cmi(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_cmi_once(&session, course_id, sco_id).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_cmi_once(&session, course_id, sco_id).await
            }
            result => result,
        }
    }
}

#[derive(Clone, Copy)]
enum ResponseContent {
    Html,
    Json,
}

async fn read_inventory_response(
    mut response: Response,
    expected_content: ResponseContent,
) -> ProviderResult<WellearnInventoryDocument> {
    validate_response_head(&response)?;
    let content_type_result = validate_response_content_type(&response, expected_content);
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
            "WELearn inventory endpoint returned an empty response",
        ));
    }
    let document = match String::from_utf8(bytes) {
        Ok(document) => document,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "WELearn inventory endpoint returned invalid UTF-8",
            ));
        }
    };
    if looks_like_login_document(&document) {
        let mut document = document;
        document.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn returned a login page for the current session",
        ));
    }
    if let Err(error) = content_type_result {
        let mut document = document;
        document.zeroize();
        return Err(error);
    }
    WellearnInventoryDocument::try_new(document)
}

fn validate_response_head(response: &Response) -> ProviderResult<()> {
    validate_status(response.status(), response.headers())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(oversized_response());
    }
    Ok(())
}

fn validate_response_content_type(
    response: &Response,
    expected_content: ResponseContent,
) -> ProviderResult<()> {
    let Some(content_type) = response.headers().get(CONTENT_TYPE) else {
        return Ok(());
    };
    let content_type = content_type.to_str().map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an invalid Content-Type",
        )
    })?;
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    let valid = match expected_content {
        ResponseContent::Html => {
            media_type.eq_ignore_ascii_case("text/html")
                || media_type.eq_ignore_ascii_case("application/xhtml+xml")
        }
        ResponseContent::Json => {
            media_type.eq_ignore_ascii_case("application/json")
                || media_type.eq_ignore_ascii_case("text/json")
                || media_type.eq_ignore_ascii_case("text/plain")
        }
    };
    if !valid {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an unexpected content type",
        ));
    }
    Ok(())
}

fn validate_status(status: StatusCode, headers: &HeaderMap) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "WELearn rate limited the inventory request",
        );
        error.retry_after_seconds = headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds <= 3_600);
        return Err(error);
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn rejected the current session",
        ));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(ProviderError::new(
            ProviderErrorKind::Authorization,
            "WELearn denied access to the inventory route",
        ));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn inventory route was not found",
        ));
    }
    if status.is_redirection() {
        let kind = if headers
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
            "WELearn returned an unhandled inventory redirect",
        ));
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "WELearn inventory endpoint is temporarily unavailable",
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn inventory endpoint returned an unexpected status",
        ));
    }
    Ok(())
}

fn course_info_url(course_id: &str) -> ProviderResult<Url> {
    let mut url = Url::parse(COURSE_INFO_ORIGIN).map_err(|_| static_route_error())?;
    url.set_path(COURSE_INFO_PATH);
    url.query_pairs_mut().append_pair("cid", course_id);
    Ok(url)
}

fn sco_leaves_url(route: &crate::WellearnCourseContext, unit_index: u32) -> ProviderResult<Url> {
    let mut url = static_url(STUDY_STAT_URL)?;
    url.query_pairs_mut()
        .append_pair("action", "scoLeaves")
        .append_pair("cid", route.course_id())
        .append_pair("uid", route.user_id())
        .append_pair("unitidx", &unit_index.to_string())
        .append_pair("classid", route.class_id());
    Ok(url)
}

fn sco_url(user_id: &str) -> ProviderResult<Url> {
    let mut url = static_url(SCO_URL)?;
    url.query_pairs_mut().append_pair("uid", user_id);
    Ok(url)
}

fn static_url(value: &'static str) -> ProviderResult<Url> {
    Url::parse(value).map_err(|_| static_route_error())
}

fn looks_like_login_location(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("sso.sflep.com"))
            || url.path().to_ascii_lowercase().contains("login")
    })
}

fn looks_like_login_document(document: &str) -> bool {
    let lowercase = document.to_ascii_lowercase();
    lowercase.contains("<form")
        && lowercase.contains("login")
        && lowercase.contains("account")
        && lowercase.contains("pwd")
}

pub(crate) fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::InvalidResponse
    };
    ProviderError::new(kind, "WELearn inventory HTTP request failed")
}

fn oversized_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "WELearn inventory response exceeds the size limit",
    )
}

fn static_route_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn compile-time inventory route is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_networking::NetworkProfile;

    use super::*;

    #[derive(Debug)]
    struct UnusedSessions;

    #[derive(Debug, Default)]
    struct RenewingSessions {
        renewals: AtomicUsize,
    }

    #[async_trait]
    impl WellearnSessionResolver for UnusedSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<crate::WellearnCookieSession> {
            Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "fixture session must not be resolved",
            ))
        }
    }

    #[async_trait]
    impl WellearnSessionResolver for RenewingSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<crate::WellearnCookieSession> {
            Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "fixture stored session expired",
            ))
        }

        async fn renew_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<crate::WellearnCookieSession> {
            self.renewals.fetch_add(1, Ordering::SeqCst);
            crate::WellearnCookieSession::try_new("session=RENEWED")
        }
    }

    #[test]
    fn native_transport_uses_the_shared_non_redirecting_client() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let transport =
            NativeWellearnInventoryTransport::try_new(&network, Arc::new(UnusedSessions)).unwrap();
        assert!(format!("{transport:?}").contains("configured"));
    }

    #[tokio::test]
    async fn operation_session_renews_only_an_authentication_failure() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let sessions = Arc::new(RenewingSessions::default());
        let transport =
            NativeWellearnInventoryTransport::try_new(&network, sessions.clone()).unwrap();
        let (session, renewed) = transport
            .session_for_operation(&ProviderContext {
                provider_id: asterism_domain::ProviderId::new("welearn").unwrap(),
                account_id: asterism_domain::ProviderAccountId::new(),
                credential_refs: vec![asterism_domain::SecretId::new()],
                correlation_id: "welearn-renewal-test".to_owned(),
            })
            .await
            .unwrap();
        assert!(renewed);
        assert_eq!(session.expose_secret(), "session=RENEWED");
        assert_eq!(sessions.renewals.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn inventory_routes_are_exact_and_encoded() {
        let course = course_info_url("course value").unwrap();
        assert_eq!(course.scheme(), "https");
        assert_eq!(course.host_str(), Some("welearn.sflep.com"));
        assert_eq!(course.path(), COURSE_INFO_PATH);
        assert_eq!(course.query(), Some("cid=course+value"));

        let courses = crate::parse_course_inventory(include_str!(
            "../../../fixtures/providers/welearn/courses/list-mixed.json"
        ))
        .unwrap();
        let context = parse_course_context(
            &courses[0],
            include_str!("../../../fixtures/providers/welearn/courses/course-context.html"),
        )
        .unwrap();
        let leaves = sco_leaves_url(&context, 3).unwrap();
        assert_eq!(
            leaves.query(),
            Some("action=scoLeaves&cid=1001&uid=7001&unitidx=3&classid=class-8001")
        );
        assert_eq!(
            sco_url("user value").unwrap().as_str(),
            "https://welearn.sflep.com/Ajax/SCO.aspx?uid=user+value"
        );
    }

    #[test]
    fn response_statuses_are_typed_without_body_access() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "12".parse().unwrap());
        let limited = validate_status(StatusCode::TOO_MANY_REQUESTS, &headers).unwrap_err();
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(12));

        headers.insert(
            LOCATION,
            "https://sso.sflep.com/idsvr/login".parse().unwrap(),
        );
        assert_eq!(
            validate_status(StatusCode::FOUND, &headers)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            validate_status(StatusCode::SERVICE_UNAVAILABLE, &HeaderMap::new())
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProviderUnavailable
        );
        assert!(validate_status(StatusCode::OK, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn login_document_detection_requires_structural_markers() {
        assert!(looks_like_login_document(
            "<form action='/login'><input name='account'><input name='pwd'></form>"
        ));
        assert!(!looks_like_login_document(
            "<script>const message='login account pwd';</script>"
        ));
    }

    #[tokio::test]
    async fn json_reader_distinguishes_a_login_page_from_unexpected_html() {
        let login = fixture_response(
            "text/html",
            "<form action='/login'><input name='account'><input name='pwd'></form>",
        )
        .await;
        let error = read_inventory_response(login, ResponseContent::Json)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);

        let unrelated = fixture_response("text/html", "<html><p>maintenance</p></html>").await;
        let error = read_inventory_response(unrelated, ResponseContent::Json)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    async fn fixture_response(content_type: &'static str, body: &'static str) -> Response {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut request = [0_u8; 1_024];
            let mut received = Vec::new();
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut request).unwrap();
                assert!(count > 0, "fixture client closed before request headers");
                received.extend_from_slice(&request[..count]);
                assert!(received.len() <= 16 * 1_024, "fixture request is bounded");
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let response = Client::new()
            .get(format!("http://{address}/fixture"))
            .send()
            .await
            .unwrap();
        server.join().unwrap();
        response
    }
}
