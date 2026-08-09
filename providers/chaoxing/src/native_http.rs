use std::{borrow::Cow, fmt, sync::Arc};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderValue, LOCATION, REFERER, RETRY_AFTER},
};
use scraper::{Html, Selector};
use zeroize::Zeroize;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest,
    ChaoxingCourseInventoryTransport, ChaoxingCourseRoute, ChaoxingInventoryDocument,
    ChaoxingInventoryTransport, ChaoxingWorkDetailRequest, ChaoxingWorkDetailState,
    classify_work_detail,
    resource_execution::ChaoxingImmediateResourceTransport,
    resource_inventory::{ChaoxingImmediateResourceKind, ChaoxingImmediateResourceTarget},
    task_inventory::{
        CHAPTER_RESOURCE_CARD_COUNT, MAX_RESOURCE_BATCH_DOCUMENT_BYTES,
        MAX_RESOURCE_CHAPTER_REQUESTS,
    },
};

const COURSE_PAGE_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/mycourse/stu";
const COURSE_LIST_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/courselistdata";
const COURSE_INTERACTION_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/interaction";
const CHAPTER_LIST_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/mycourse/studentcourse";
const CHAPTER_RESOURCE_BASE: &str = "https://mooc1.chaoxing.com/mooc-ans/knowledge/cards";
const CHAPTER_RESOURCE_VERSION: &str = "2025-0424-1038-3";
const DOCUMENT_COMPLETE_BASE: &str = "https://mooc1.chaoxing.com/ananas/job/document";
const READ_COMPLETE_BASE: &str = "https://mooc1.chaoxing.com/ananas/job/readv2";
const COURSE_LIST_REFERER: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/interaction?moocDomain=https://mooc1-1.chaoxing.com/mooc-ans";
const EXAM_LIST_BASE: &str = "https://mooc1.chaoxing.com/exam-ans/mooc2/exam/exam-list";
const WORK_LIST_ORIGIN: &str = "https://mooc1.chaoxing.com";
const WORK_LIST_PATH: &str = "/mooc2/work/list";
const MAX_COOKIE_BYTES: usize = 64 * 1_024;
const MAX_HTML_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_COURSE_FOLDERS: usize = 256;
const MAX_COURSE_FOLDER_ID_BYTES: usize = 64;
const MAX_WORK_DETAIL_REDIRECTS: usize = 3;

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

    pub(crate) fn expose_secret(&self) -> &str {
        self.0.expose_secret()
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

    async fn renew_session(
        &self,
        _context: &ProviderContext,
    ) -> ProviderResult<ChaoxingCookieSession> {
        Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing stored session cannot be renewed automatically",
        ))
    }
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

    async fn post_course_list(
        &self,
        session: &ChaoxingCookieSession,
        folder_id: &str,
    ) -> ProviderResult<SensitiveHtml> {
        fetch_course_list_html(&self.client, session, folder_id).await
    }

    async fn session_for_operation(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<(ChaoxingCookieSession, bool)> {
        match self.sessions.resolve_session(context).await {
            Ok(session) => Ok((session, false)),
            Err(error) if error.kind == ProviderErrorKind::Authentication => {
                Ok((self.sessions.renew_session(context).await?, true))
            }
            Err(error) => Err(error),
        }
    }

    async fn fetch_work_inventory_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let course_page = self.get_html(session, course_page_url(route)?).await?;
        let work_url = discover_work_list_url(course_page.as_str(), route)?;
        self.get_html(session, work_url)
            .await?
            .into_inventory_document()
    }

    async fn fetch_chapter_inventory_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        self.get_html(session, chapter_list_url(route)?)
            .await?
            .into_inventory_document()
    }

    async fn fetch_exam_inventory_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        self.get_html(session, exam_list_url(route)?)
            .await?
            .into_inventory_document()
    }

    async fn fetch_chapter_resource_inventories_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingChapterResourceRequest],
    ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
        if requests.len() > MAX_RESOURCE_CHAPTER_REQUESTS
            || requests.iter().any(|request| !request.belongs_to(route))
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing chapter resource request batch is unbounded or route-mismatched",
            ));
        }
        let mut documents = Vec::with_capacity(
            requests
                .len()
                .saturating_mul(usize::from(CHAPTER_RESOURCE_CARD_COUNT)),
        );
        let mut total_bytes = 0_usize;
        for request in requests {
            for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
                let document = self
                    .get_html(session, chapter_resource_url(route, request, card_index)?)
                    .await?;
                total_bytes = total_bytes
                    .checked_add(document.as_str().len())
                    .filter(|total| *total <= MAX_RESOURCE_BATCH_DOCUMENT_BYTES)
                    .ok_or_else(|| {
                        ProviderError::new(
                            ProviderErrorKind::InvalidResponse,
                            "Chaoxing resource card batch exceeds the aggregate size limit",
                        )
                    })?;
                documents.push(ChaoxingChapterResourceDocument::from_document(
                    request,
                    card_index,
                    document.into_inventory_document()?,
                )?);
            }
        }
        Ok(documents)
    }

    async fn fetch_work_detail_states_once(
        &self,
        session: &ChaoxingCookieSession,
        requests: &[ChaoxingWorkDetailRequest<'_>],
    ) -> ProviderResult<Vec<ChaoxingWorkDetailState>> {
        let mut states = Vec::with_capacity(requests.len());
        for request in requests {
            states.push(self.fetch_work_detail_state(session, *request).await?);
        }
        Ok(states)
    }

    async fn fetch_work_detail_state(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingWorkDetailState> {
        let mut url = request.url()?;
        for redirect_count in 0..=MAX_WORK_DETAIL_REDIRECTS {
            let response = self
                .client
                .get(url.clone())
                .header(COOKIE, session.header_value()?)
                .header(ACCEPT, "text/html,application/xhtml+xml")
                .send()
                .await
                .map_err(|error| classify_reqwest_error(&error))?;
            if response.status().is_redirection() {
                if redirect_count == MAX_WORK_DETAIL_REDIRECTS {
                    return Err(ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "Chaoxing Work detail exceeded the redirect limit",
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ProviderError::new(
                            ProviderErrorKind::ProtocolDrift,
                            "Chaoxing Work detail redirect has no valid location",
                        )
                    })?;
                if looks_like_login_location(location) {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Authentication,
                        "Chaoxing Work detail redirected to login",
                    ));
                }
                let next = url.join(location).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "Chaoxing Work detail redirect is invalid",
                    )
                })?;
                if looks_like_login_location(next.as_str()) {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Authentication,
                        "Chaoxing Work detail redirected to login",
                    ));
                }
                if !request.allows_redirect(&next) {
                    return Err(ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "Chaoxing Work detail redirected outside its route boundary",
                    ));
                }
                url = next;
                continue;
            }
            let document = classify_response(response).await?;
            let remote_state = classify_work_detail(url.as_str(), document.as_str())?;
            return Ok(ChaoxingWorkDetailState::for_request(request, remote_state));
        }
        unreachable!("bounded Work detail redirect loop always returns")
    }

    async fn complete_immediate_resource_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingImmediateResourceTarget,
    ) -> ProviderResult<()> {
        let response = self
            .client
            .get(immediate_resource_url(route, knowledge_id, target)?)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json,text/html,*/*")
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        let _response = read_response_body(response).await?;
        Ok(())
    }

    async fn fetch_course_inventories_once(
        &self,
        session: &ChaoxingCookieSession,
    ) -> ProviderResult<Vec<ChaoxingInventoryDocument>> {
        let root = self.post_course_list(session, "0").await?;
        let interaction = self
            .get_html(session, static_url(COURSE_INTERACTION_BASE)?)
            .await?;
        let folder_ids = parse_course_folder_ids(interaction.as_str())?;
        let mut documents = Vec::with_capacity(folder_ids.len() + 1);
        documents.push(root.into_inventory_document()?);
        for folder_id in folder_ids {
            documents.push(
                self.post_course_list(session, &folder_id)
                    .await?
                    .into_inventory_document()?,
            );
        }
        Ok(documents)
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
    async fn fetch_chapter_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_chapter_inventory_once(&session, route).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_chapter_inventory_once(&session, route).await
            }
            result => result,
        }
    }

    async fn fetch_work_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_work_inventory_once(&session, route).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_work_inventory_once(&session, route).await
            }
            result => result,
        }
    }

    async fn fetch_chapter_resource_inventories(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingChapterResourceRequest],
    ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_chapter_resource_inventories_once(&session, route, requests)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_chapter_resource_inventories_once(&session, route, requests)
                    .await
            }
            result => result,
        }
    }

    async fn fetch_exam_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_exam_inventory_once(&session, route).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_exam_inventory_once(&session, route).await
            }
            result => result,
        }
    }

    async fn fetch_work_detail_states(
        &self,
        context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingWorkDetailRequest<'_>],
    ) -> ProviderResult<Vec<ChaoxingWorkDetailState>> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_work_detail_states_once(&session, requests).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_work_detail_states_once(&session, requests).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl ChaoxingImmediateResourceTransport for NativeChaoxingInventoryTransport {
    async fn complete_immediate_resource(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingImmediateResourceTarget,
    ) -> ProviderResult<()> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .complete_immediate_resource_once(&session, route, knowledge_id, target)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.complete_immediate_resource_once(&session, route, knowledge_id, target)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl ChaoxingCourseInventoryTransport for NativeChaoxingInventoryTransport {
    async fn fetch_course_inventories(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Vec<ChaoxingInventoryDocument>> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_course_inventories_once(&session).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_course_inventories_once(&session).await
            }
            result => result,
        }
    }
}

fn should_renew_after(error: &ProviderError, already_renewed: bool) -> bool {
    !already_renewed && error.kind == ProviderErrorKind::Authentication
}

pub(crate) struct SensitiveHtml(String);

impl SensitiveHtml {
    pub(crate) fn as_str(&self) -> &str {
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

fn chapter_list_url(route: ChaoxingCourseRoute<'_>) -> ProviderResult<Url> {
    build_url(
        CHAPTER_LIST_BASE,
        &[
            ("courseid", route.course_id()),
            ("clazzid", route.class_id()),
            ("cpi", route.cpi()),
            ("ut", "s"),
        ],
    )
}

fn chapter_resource_url(
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    card_index: u8,
) -> ProviderResult<Url> {
    let card_index = card_index.to_string();
    build_url(
        CHAPTER_RESOURCE_BASE,
        &[
            ("clazzid", route.class_id()),
            ("courseid", route.course_id()),
            ("knowledgeid", request.knowledge_id()),
            ("ut", "s"),
            ("cpi", route.cpi()),
            ("v", CHAPTER_RESOURCE_VERSION),
            ("mooc2", "1"),
            ("num", &card_index),
        ],
    )
}

fn immediate_resource_url(
    route: ChaoxingCourseRoute<'_>,
    knowledge_id: &str,
    target: &ChaoxingImmediateResourceTarget,
) -> ProviderResult<Url> {
    let token = target.token().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing immediate resource no longer has an execution token",
        )
    })?;
    let mut query = vec![
        ("jobid", target.job_id()),
        ("knowledgeid", knowledge_id),
        ("courseid", route.course_id()),
        ("clazzid", route.class_id()),
        ("jtoken", token.expose_secret()),
    ];
    let timestamp;
    let base = match target.kind() {
        ChaoxingImmediateResourceKind::Document => {
            timestamp = chrono::Utc::now().timestamp_millis().to_string();
            query.push(("_dc", &timestamp));
            DOCUMENT_COMPLETE_BASE
        }
        ChaoxingImmediateResourceKind::Read => READ_COMPLETE_BASE,
    };
    build_url(base, &query)
}

fn build_url(base: &str, query: &[(&str, &str)]) -> ProviderResult<Url> {
    let mut url = static_url(base)?;
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    Ok(url)
}

fn static_url(value: &str) -> ProviderResult<Url> {
    Url::parse(value).map_err(|_| static_route_error())
}

fn course_list_form(folder_id: &str) -> ProviderResult<String> {
    if folder_id.len() > MAX_COURSE_FOLDER_ID_BYTES
        || folder_id.is_empty()
        || !folder_id.bytes().all(|byte| byte.is_ascii_digit())
        || (folder_id.len() > 1 && folder_id.starts_with('0'))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Chaoxing course folder contains an invalid identity",
        ));
    }
    Ok(format!(
        "courseType=1&courseFolderId={folder_id}&query=&superstarClass=0"
    ))
}

pub(crate) async fn fetch_course_list_html(
    client: &Client,
    session: &ChaoxingCookieSession,
    folder_id: &str,
) -> ProviderResult<SensitiveHtml> {
    let response = client
        .post(static_url(COURSE_LIST_BASE)?)
        .header(COOKIE, session.header_value()?)
        .header(ACCEPT, "text/html,application/xhtml+xml")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(REFERER, COURSE_LIST_REFERER)
        .body(course_list_form(folder_id)?)
        .send()
        .await
        .map_err(|error| classify_reqwest_error(&error))?;
    classify_response(response).await
}

fn parse_course_folder_ids(html: &str) -> ProviderResult<Vec<String>> {
    if html.len() > MAX_HTML_BYTES {
        return Err(oversized_response());
    }
    let document = Html::parse_document(html);
    let selector = Selector::parse("ul.file-list > li[fileid]")
        .expect("static Chaoxing selector must be valid");
    let mut folder_ids = Vec::new();
    for folder in document.select(&selector) {
        if folder_ids.len() == MAX_COURSE_FOLDERS {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing course folder count exceeds the size limit",
            ));
        }
        let folder_id = folder.value().attr("fileid").unwrap_or_default();
        course_list_form(folder_id)?;
        if folder_id == "0" || folder_ids.iter().any(|known| known == folder_id) {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Chaoxing course folders contain a duplicate or root identity",
            ));
        }
        folder_ids.push(folder_id.to_owned());
    }
    Ok(folder_ids)
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
    validate_response_status(response)?;
    validate_html_response_head(response)
}

pub(crate) fn validate_response_status(response: &Response) -> ProviderResult<()> {
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
    Ok(())
}

fn validate_html_response_head(response: &Response) -> ProviderResult<()> {
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

pub(crate) fn classify_reqwest_error(error: &reqwest::Error) -> ProviderError {
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
    use std::fmt::Write as _;

    use asterism_provider_api::{ProviderRouteContext, RemoteCourse};

    use super::*;

    const COURSE_PAGE: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/course-page-with-work-iframe.html");
    const COURSE_FOLDERS: &str =
        include_str!("../../../fixtures/providers/chaoxing/courses/interaction-folders.html");
    const CHAPTER_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
    const RESOURCE_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");

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

        let chapter_url = chapter_list_url(route).unwrap();
        assert_eq!(chapter_url.path(), "/mooc2-ans/mycourse/studentcourse");
        assert_eq!(query(&chapter_url, "courseid").as_deref(), Some("100"));
        assert_eq!(query(&chapter_url, "clazzid").as_deref(), Some("200"));
        assert_eq!(query(&chapter_url, "cpi").as_deref(), Some("300"));

        let scope = crate::ChaoxingCourseScope::new("course:100:200", "100", "200").unwrap();
        let chapter = crate::parse_chapter_inventory(CHAPTER_MIXED, &scope)
            .unwrap()
            .remove(0);
        let request = ChaoxingChapterResourceRequest::try_from_chapter(&chapter)
            .unwrap()
            .unwrap();
        let resource_url = chapter_resource_url(route, &request, 6).unwrap();
        assert_eq!(resource_url.host_str(), Some("mooc1.chaoxing.com"));
        assert_eq!(resource_url.path(), "/mooc-ans/knowledge/cards");
        assert_eq!(query(&resource_url, "courseid").as_deref(), Some("100"));
        assert_eq!(query(&resource_url, "clazzid").as_deref(), Some("200"));
        assert_eq!(query(&resource_url, "knowledgeid").as_deref(), Some("4001"));
        assert_eq!(query(&resource_url, "cpi").as_deref(), Some("300"));
        assert_eq!(query(&resource_url, "num").as_deref(), Some("6"));
        assert_eq!(
            query(&resource_url, "v").as_deref(),
            Some(CHAPTER_RESOURCE_VERSION)
        );

        let read = crate::resource_inventory::locate_immediate_resource_target(
            RESOURCE_MIXED,
            &scope,
            "4001",
            0,
            "resource:100:200:4001:job-read",
        )
        .unwrap()
        .unwrap();
        let read_url = immediate_resource_url(route, "4001", &read).unwrap();
        assert_eq!(read_url.path(), "/ananas/job/readv2");
        assert_eq!(query(&read_url, "jobid").as_deref(), Some("job-read"));
        assert_eq!(query(&read_url, "knowledgeid").as_deref(), Some("4001"));
        assert_eq!(
            query(&read_url, "jtoken").as_deref(),
            Some("PRIVATE_READ_TOKEN")
        );
        assert!(query(&read_url, "_dc").is_none());

        let pending_document = RESOURCE_MIXED.replace(
            "\"jobid\":\"job-document\",\"isPassed\":true",
            "\"jobid\":\"job-document\",\"isPassed\":false",
        );
        let document = crate::resource_inventory::locate_immediate_resource_target(
            &pending_document,
            &scope,
            "4001",
            0,
            "resource:100:200:4001:job-document",
        )
        .unwrap()
        .unwrap();
        let document_url = immediate_resource_url(route, "4001", &document).unwrap();
        assert_eq!(document_url.path(), "/ananas/job/document");
        assert!(query(&document_url, "_dc").is_some());

        let work_url = discover_work_list_url(COURSE_PAGE, route).unwrap();
        assert_eq!(work_url.host_str(), Some("mooc1.chaoxing.com"));
        assert_eq!(query(&work_url, "enc").as_deref(), Some("SAFE_ENC"));
    }

    #[test]
    fn course_folder_requests_are_bounded_and_deterministic() {
        assert_eq!(
            parse_course_folder_ids(COURSE_FOLDERS).unwrap(),
            ["700".to_owned(), "701".to_owned()]
        );
        assert_eq!(
            course_list_form("700").unwrap(),
            "courseType=1&courseFolderId=700&query=&superstarClass=0"
        );
        for invalid in ["", "00", "folder", "7&query=private"] {
            assert!(course_list_form(invalid).is_err());
        }

        let duplicate = COURSE_FOLDERS.replace("701", "700");
        assert!(parse_course_folder_ids(&duplicate).is_err());
        let oversized = (1..=MAX_COURSE_FOLDERS + 1).fold(String::new(), |mut html, folder_id| {
            write!(
                html,
                "<ul class='file-list'><li fileid='{folder_id}'></li></ul>"
            )
            .unwrap();
            html
        });
        assert!(parse_course_folder_ids(&oversized).is_err());
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

    #[test]
    fn automatic_renewal_is_limited_to_one_authentication_failure() {
        let authentication = ProviderError::new(
            ProviderErrorKind::Authentication,
            "sanitized authentication failure",
        );
        let network = ProviderError::new(ProviderErrorKind::Network, "sanitized network failure");
        assert!(should_renew_after(&authentication, false));
        assert!(!should_renew_after(&authentication, true));
        assert!(!should_renew_after(&network, false));
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
