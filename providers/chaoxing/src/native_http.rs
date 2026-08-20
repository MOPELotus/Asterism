use std::{borrow::Cow, fmt, sync::Arc};

use asterism_domain::{HumanRequiredReason, TaskId};
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationSink, ProviderContext,
    ProviderCourseEnrollmentDraft, ProviderError, ProviderErrorKind, ProviderResult,
};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use chrono::Utc;
use rand_core::{OsRng, RngCore};
use reqwest::{
    Client, Request, Response, StatusCode, Url,
    header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderValue, LOCATION, ORIGIN, REFERER, RETRY_AFTER},
};
use scraper::{Html, Selector};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest,
    ChaoxingCourseEnrollmentTransport, ChaoxingCourseInventoryTransport,
    ChaoxingCourseInviteApiDocument, ChaoxingCourseInviteApiPreparation,
    ChaoxingCourseInvitePreviewDocument, ChaoxingCourseInvitePreviewPreparation,
    ChaoxingCourseInvitePreviewRedirect, ChaoxingCourseJoinPreparation,
    ChaoxingCourseJoinReceiptDocument, ChaoxingCourseRoute, ChaoxingExamDetailFacts,
    ChaoxingExamDetailRequest, ChaoxingExamQuestionArtifact, ChaoxingExamQuestionRequest,
    ChaoxingExamSubmissionCommand, ChaoxingExamSubmissionResponse,
    ChaoxingExamVerificationDocument, ChaoxingInventoryDocument, ChaoxingInventoryTransport,
    ChaoxingIssuedCourseEnrollment, ChaoxingIssuedCourseJoin, ChaoxingQuestionTransport,
    ChaoxingSignActivityListDocument, ChaoxingSignActivityReadTransport,
    ChaoxingSignDetailDocument, ChaoxingSignDetailRequest, ChaoxingSignEventBootstrapDocument,
    ChaoxingSignEventReadTransport, ChaoxingSubmissionPlan, ChaoxingSubmissionTransport,
    ChaoxingSubmissionVerificationTransport, ChaoxingWorkDetailRequest, ChaoxingWorkDetailState,
    ChaoxingWorkVerificationDocument, ChaoxingWorkVerificationRoute, classify_work_detail,
    course_invite::shared_course_join_url,
    exam_attempt::{
        ChaoxingExamStartCommand, ChaoxingExamStartOutcome, parse_exam_attempt, parse_exam_cover,
    },
    parse_exam_submission_response, parse_submission_receipt,
    resource_execution::{
        ChaoxingImmediateResourceTransport, ChaoxingLiveStatus, ChaoxingLiveTransport,
        ChaoxingMediaReportOutcome, ChaoxingMediaReportRequest, ChaoxingVideoStatus,
        ChaoxingVideoTransport, live_post_mutation_error,
    },
    resource_inventory::{
        ChaoxingChapterWorkTarget, ChaoxingImmediateResourceKind, ChaoxingImmediateResourceTarget,
        ChaoxingLiveResourceTarget, ChaoxingMediaKind, ChaoxingMediaResourceTarget,
        ChaoxingVideoRt,
    },
    submission_support::ChaoxingSubmissionForm,
    task_inventory::{
        CHAPTER_RESOURCE_CARD_COUNT, MAX_EXAM_DETAIL_REQUESTS, MAX_RESOURCE_BATCH_DOCUMENT_BYTES,
        MAX_RESOURCE_CHAPTER_REQUESTS,
    },
};

const COURSE_PAGE_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/mycourse/stu";
const COURSE_LIST_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/courselistdata";
const COURSE_INTERACTION_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/interaction";
const CHAPTER_LIST_BASE: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/mycourse/studentcourse";
const CHAPTER_RESOURCE_BASE: &str = "https://mooc1.chaoxing.com/mooc-ans/knowledge/cards";
const CHAPTER_WORK_BASE: &str = "https://mooc1.chaoxing.com/mooc-ans/api/work";
const CHAPTER_RESOURCE_VERSION: &str = "2025-0424-1038-3";
const DOCUMENT_COMPLETE_BASE: &str = "https://mooc1.chaoxing.com/ananas/job/document";
const READ_COMPLETE_BASE: &str = "https://mooc1.chaoxing.com/ananas/job/readv2";
const IMMEDIATE_MUTATION_OPERATION: &str = "chaoxing.resource.immediate-complete";
const IMMEDIATE_REQUEST_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.immediate-resource-request.v1\0";
const IMMEDIATE_RESPONSE_DIGEST_DOMAIN: &[u8] =
    b"asterism.chaoxing.immediate-resource-response.v1\0";
const MEDIA_MUTATION_OPERATION: &str = "chaoxing.media.progress";
const MEDIA_REQUEST_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.media-request.v1\0";
const MEDIA_RESPONSE_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.media-response.v1\0";
const VIDEO_STATUS_ORIGIN: &str = "https://mooc1.chaoxing.com";
const VIDEO_REPORT_ORIGIN: &str = "https://mooc1.chaoxing.com";
const VIDEO_REFERER: &str =
    "https://mooc1.chaoxing.com/ananas/modules/video/index.html?v=2025-0725-1842";
const AUDIO_REFERER: &str =
    "https://mooc1.chaoxing.com/ananas/modules/audio/index_new.html?v=2025-0725-1842";
const LIVE_STATUS_BASE: &str = "https://mooc1.chaoxing.com/ananas/live/liveinfo";
const LIVE_REPORT_BASE: &str = "https://zhibo.chaoxing.com/saveTimePc";
const LIVE_REFERER: &str =
    "https://mooc1.chaoxing.com/ananas/modules/live/index.html?v=2022-1214-1139";
const LIVE_MUTATION_OPERATION: &str = "chaoxing.live.heartbeat";
const LIVE_REQUEST_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.live-request.v1\0";
const LIVE_RESPONSE_DIGEST_DOMAIN: &[u8] = b"asterism.chaoxing.live-response.v1\0";
const COURSE_LIST_REFERER: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/visit/interaction?moocDomain=https://mooc1-1.chaoxing.com/mooc-ans";
const EXAM_LIST_BASE: &str = "https://mooc1.chaoxing.com/exam-ans/mooc2/exam/exam-list";
const EXAM_COVER_BASE: &str = "https://mooc1-api.chaoxing.com/exam-ans/exam/phone/task-exam";
const EXAM_START_BASE: &str = "https://mooc1-api.chaoxing.com/exam-ans/exam/phone/start";
const EXAM_PREVIEW_BASE: &str = "https://mooc1-api.chaoxing.com/exam-ans/exam/phone/preview";
const EXAM_SUBMISSION_BASE: &str =
    "https://mooc1.chaoxing.com/exam-ans/exam/test/reVersionSubmitTestNew";
const EXAM_REQUESTED_WITH: &str = "com.chaoxing.mobile";
const WORK_REQUESTED_WITH: &str = "XMLHttpRequest";
const WORK_LIST_ORIGIN: &str = "https://mooc1.chaoxing.com";
const WORK_LIST_PATH: &str = "/mooc2/work/list";
const WORK_SUBMISSION_BASE: &str = "https://mooc1.chaoxing.com/mooc-ans/work/addStudentWorkNew";
const SIGN_ACTIVITY_LIST_BASE: &str =
    "https://mobilelearn.chaoxing.com/v2/apis/active/student/activelist";
const SIGN_DETAIL_BASE: &str = "https://mobilelearn.chaoxing.com/newsign/signDetail";
const SIGN_EVENT_BOOTSTRAP_BASE: &str = "https://im.chaoxing.com/webim/me";
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

    fn cookie_value(&self, names: &[&str]) -> Option<&str> {
        let mut values = self.0.expose_secret().split(';').filter_map(|field| {
            let (name, value) = field.trim().split_once('=')?;
            names
                .iter()
                .any(|expected| name.trim() == *expected)
                .then(|| value.trim())
                .filter(|value| !value.is_empty())
        });
        let value = values.next()?;
        values.next().is_none().then_some(value)
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

/// Fully built, credential-bearing join request held only between preflight and
/// durable mutation issuance. Debug output never exposes its URL or headers.
pub struct NativeChaoxingCourseJoinRequest {
    request: Request,
    request_digest: [u8; 32],
}

impl fmt::Debug for NativeChaoxingCourseJoinRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeChaoxingCourseJoinRequest")
            .field("request", &"[REDACTED]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
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

    async fn get_course_invite_html(
        &self,
        session: &ChaoxingCookieSession,
        url: Url,
    ) -> ProviderResult<(SensitiveHtml, String)> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let source_url = response.url().to_string();
        let document = classify_response(response).await?;
        Ok((document, source_url))
    }

    async fn get_exam_html(
        &self,
        session: &ChaoxingCookieSession,
        url: Url,
    ) -> ProviderResult<SensitiveHtml> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .header("x-requested-with", EXAM_REQUESTED_WITH)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        classify_response(response).await
    }

    async fn get_sign_json(
        &self,
        session: &ChaoxingCookieSession,
        url: Url,
    ) -> ProviderResult<SensitiveHtml> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json")
            .header("x-requested-with", EXAM_REQUESTED_WITH)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        validate_json_response_head(&response)?;
        read_response_body(response).await
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

    async fn fetch_sign_activity_list_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingSignActivityListDocument> {
        let fid = session.cookie_value(&["fid"]).unwrap_or("1024");
        self.get_sign_json(session, sign_activity_list_url(route, fid)?)
            .await
            .and_then(|document| ChaoxingSignActivityListDocument::try_new(document.into_string()))
    }

    async fn fetch_sign_detail_once(
        &self,
        session: &ChaoxingCookieSession,
        request: &ChaoxingSignDetailRequest,
    ) -> ProviderResult<ChaoxingSignDetailDocument> {
        self.get_sign_json(session, sign_detail_url(request)?)
            .await
            .and_then(|document| ChaoxingSignDetailDocument::try_new(document.into_string()))
    }

    async fn fetch_sign_event_bootstrap_once(
        &self,
        session: &ChaoxingCookieSession,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingSignEventBootstrapDocument> {
        self.get_html(session, static_url(SIGN_EVENT_BOOTSTRAP_BASE)?)
            .await
            .and_then(|document| {
                ChaoxingSignEventBootstrapDocument::for_context(context, document.into_string())
            })
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

    async fn fetch_exam_detail_facts_once(
        &self,
        session: &ChaoxingCookieSession,
        requests: &[ChaoxingExamDetailRequest<'_>],
    ) -> ProviderResult<Vec<ChaoxingExamDetailFacts>> {
        if requests.len() > MAX_EXAM_DETAIL_REQUESTS {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing Exam detail request batch exceeds the size limit",
            ));
        }
        let mut details = Vec::with_capacity(requests.len());
        for request in requests {
            let document = self.get_html(session, request.url()?).await?;
            details.push(ChaoxingExamDetailFacts::for_request(
                *request,
                document.as_str(),
            )?);
        }
        Ok(details)
    }

    async fn fetch_work_detail_state(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingWorkDetailState> {
        let (url, document) = self.fetch_work_page(session, request).await?;
        let remote_state = classify_work_detail(url.as_str(), document.as_str())?;
        Ok(ChaoxingWorkDetailState::for_request(request, remote_state))
    }

    async fn fetch_work_page(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<(Url, SensitiveHtml)> {
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
            return Ok((url, document));
        }
        unreachable!("bounded Work detail redirect loop always returns")
    }

    async fn fetch_work_question_document_once(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let (url, document) = self.fetch_work_page(session, request).await?;
        if !is_readable_work_question_url(&url) {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing Work task is not currently on a readable editor route",
            ));
        }
        document.into_inventory_document()
    }

    async fn fetch_chapter_work_question_document_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        self.fetch_chapter_work_page(session, route, request, target)
            .await?
            .1
            .into_inventory_document()
    }

    async fn prepare_exam_start_once(
        &self,
        session: &ChaoxingCookieSession,
        task_id: TaskId,
        remote_task_id: &str,
        request: &ChaoxingExamQuestionRequest<'_>,
    ) -> ProviderResult<ChaoxingExamStartCommand> {
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Exam cover requires one identity Cookie",
            )
        })?;
        let cover_url = build_url(
            EXAM_COVER_BASE,
            &[
                ("redo", "1"),
                ("taskrefId", request.exam_id()),
                ("courseId", request.route().course_id()),
                ("classId", request.route().class_id()),
                ("userId", user_id),
                ("role", ""),
                ("source", "0"),
                ("enc_task", request.enc_task()),
                ("cpi", request.route().cpi()),
                ("vx", "0"),
                ("examsignal", "1"),
            ],
        )?;
        let cover = self.get_exam_html(session, cover_url).await?;
        let cover_facts = parse_exam_cover(cover.as_str())?;
        if cover_facts.need_code || cover_facts.need_face || cover_facts.need_captcha {
            return Err(ProviderError::human_required(
                "Chaoxing Exam requires browser or human verification before start",
                HumanRequiredReason::BrowserRequired,
            ));
        }
        ChaoxingExamStartCommand::from_cover(task_id, remote_task_id, request, user_id, cover_facts)
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_exam_start_once(
        &self,
        session: &ChaoxingCookieSession,
        command: &ChaoxingExamStartCommand,
    ) -> ProviderResult<ChaoxingExamStartOutcome> {
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Exam start requires one identity Cookie",
            )
        })?;
        if !command.belongs_to_user(user_id) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing Exam identity Cookie changed after start preparation",
            ));
        }
        let start_url = build_url(
            EXAM_START_BASE,
            &[
                ("courseId", command.course_id()),
                ("classId", command.class_id()),
                ("examId", command.exam_id()),
                ("source", "0"),
                ("examAnswerId", command.exam_answer_id()),
                ("cpi", command.cpi()),
                ("keyboardDisplayRequiresUserAction", "1"),
                ("imei", "asterism-native"),
                ("faceDetection", "0"),
                ("facekey", ""),
                ("faceDetectionResult", ""),
                ("captchavalidate", ""),
                ("jt", "0"),
                ("code", ""),
            ],
        )?;
        let response = self
            .client
            .get(start_url.clone())
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .header("x-requested-with", EXAM_REQUESTED_WITH)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| protocol_drift("Chaoxing Exam start redirect has no location"))?;
            let url = start_url
                .join(location)
                .map_err(|_| protocol_drift("Chaoxing Exam start redirect is invalid"))?;
            if !command.valid_start_redirect(&url) {
                return Err(protocol_drift(
                    "Chaoxing Exam start redirect crossed its route boundary",
                ));
            }
            let document = self.get_exam_html(session, url.clone()).await?;
            let material = parse_exam_attempt(&url, document.as_str(), command.exam_answer_id())?;
            let enc_remain_time = material.enc_remain_time.to_string();
            let last_update_time = material.last_update_time.to_string();
            let preview_url = build_url(
                EXAM_PREVIEW_BASE,
                &[
                    ("courseId", command.course_id()),
                    ("classId", command.class_id()),
                    ("source", "0"),
                    ("imei", "asterism-native"),
                    ("start", "0"),
                    ("cpi", command.cpi()),
                    ("examRelationId", command.exam_id()),
                    ("examRelationAnswerId", material.exam_answer_id.as_str()),
                    ("monitorStatus", "0"),
                    ("monitorOp", "-1"),
                    ("remainTimeParam", &enc_remain_time),
                    ("relationAnswerLastUpdateTime", &last_update_time),
                    ("enc", material.enc.as_str()),
                ],
            )?;
            let preview = self.get_exam_html(session, preview_url.clone()).await?;
            if !command.valid_question_url(&preview_url, &material.exam_answer_id) {
                return Err(protocol_drift(
                    "Chaoxing Exam preview route lost attempt binding",
                ));
            }
            let preview_material =
                parse_exam_attempt(&preview_url, preview.as_str(), &material.exam_answer_id)?;
            let mut response_hasher = Sha256::new();
            response_hasher.update(url.as_str().as_bytes());
            response_hasher.update([0]);
            response_hasher.update(document.as_str().as_bytes());
            response_hasher.update([0]);
            response_hasher.update(preview.as_str().as_bytes());
            return Ok(ChaoxingExamStartOutcome {
                document: preview.into_inventory_document()?,
                material: preview_material,
                response_digest: response_hasher.finalize().into(),
                received_at: Utc::now(),
            });
        }
        if status.is_success() {
            return Err(ProviderError::human_required(
                "Chaoxing Exam start requires an exam code or browser verification",
                HumanRequiredReason::BrowserRequired,
            ));
        }
        Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing Exam start returned an unexpected status",
        ))
    }

    async fn fetch_chapter_work_page(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
    ) -> ProviderResult<(Url, SensitiveHtml)> {
        let mut url = chapter_work_url(route, request, target)?;
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
                    return Err(protocol_drift(
                        "Chaoxing Chapter Work exceeded the redirect limit",
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        protocol_drift("Chaoxing Chapter Work redirect has no valid location")
                    })?;
                if looks_like_login_location(location) {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Authentication,
                        "Chaoxing Chapter Work redirected to login",
                    ));
                }
                let next = url
                    .join(location)
                    .map_err(|_| protocol_drift("Chaoxing Chapter Work redirect is invalid"))?;
                if looks_like_login_location(next.as_str()) {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Authentication,
                        "Chaoxing Chapter Work redirected to login",
                    ));
                }
                if !valid_chapter_work_url(&next, route, request, target) {
                    return Err(protocol_drift(
                        "Chaoxing Chapter Work redirected outside its route boundary",
                    ));
                }
                url = next;
                continue;
            }
            return Ok((url, classify_response(response).await?));
        }
        unreachable!("bounded Chapter Work redirect loop always returns")
    }

    async fn prepare_work_submission_once(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingWorkDetailRequest<'_>,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<(Url, ChaoxingSubmissionForm)> {
        let (url, document) = self.fetch_work_page(session, request).await?;
        if !is_readable_work_question_url(&url) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing Work is no longer on a submittable editor route",
            ));
        }
        let identity =
            crate::submission_support::WorkSubmissionIdentity::parse(request.remote_task_id())?;
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Work submission requires one identity Cookie",
            )
        })?;
        let form =
            ChaoxingSubmissionForm::parse(document.as_str(), identity, plan)?.bind_user(user_id)?;
        Ok((url, form))
    }

    async fn post_work_submission_once(
        &self,
        session: &ChaoxingCookieSession,
        referer: &Url,
        form: &ChaoxingSubmissionForm,
    ) -> ProviderResult<asterism_domain::SubmissionReceipt> {
        let request = build_work_submission_request(
            &self.client,
            session,
            static_url(WORK_SUBMISSION_BASE)?,
            referer,
            form.fields(),
        )?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        let body = read_response_body(response).await?;
        parse_submission_receipt(body.as_str())
    }

    async fn fetch_work_verification_once(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingWorkVerificationDocument> {
        let (url, document) = self.fetch_work_page(session, request).await?;
        let path = url.path().to_ascii_lowercase();
        let route = if path.ends_with("/work/dowork") {
            ChaoxingWorkVerificationRoute::Editor
        } else if path.ends_with("/work/prompt") {
            ChaoxingWorkVerificationRoute::Prompt
        } else if path.ends_with("/work/view") {
            ChaoxingWorkVerificationRoute::View
        } else {
            return Err(protocol_drift(
                "Chaoxing Work verification ended on an unsupported route",
            ));
        };
        ChaoxingWorkVerificationDocument::try_new(route, document.into_string())
    }

    async fn fetch_exam_verification_once(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingExamDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingExamVerificationDocument> {
        let document = self.get_html(session, request.url()?).await?;
        ChaoxingExamVerificationDocument::try_new(document.into_string())
    }

    async fn complete_immediate_resource_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingImmediateResourceTarget,
        mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        let url = immediate_resource_url(route, knowledge_id, target)?;
        let request_digest = immediate_mutation_request_digest(target.job_id(), &url)?;
        let request = self
            .client
            .get(url)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json,text/html,*/*")
            .build()
            .map_err(|error| classify_reqwest_error(&error))?;
        let issue = ExecutionMutationIssue::new(1, IMMEDIATE_MUTATION_OPERATION, request_digest)?;
        mutations.issue(&issue).await?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|error| immediate_post_mutation_error(classify_reqwest_error(&error)))?;
        let response_status = response.status();
        let response_result = validate_response_status(&response);
        let body = read_live_mutation_body(response)
            .await
            .map_err(immediate_post_mutation_error)?;
        let response_digest = immediate_mutation_response_digest(response_status, body.as_str())?;
        let accepted = response_result.is_ok();
        let receipt = ExecutionMutationReceipt::new(issue.ordinal(), response_digest, accepted)
            .map_err(immediate_post_mutation_error)?;
        mutations
            .record_receipt(receipt)
            .await
            .map_err(immediate_post_mutation_error)?;
        response_result.map_err(immediate_post_mutation_error)
    }

    async fn video_status_once(
        &self,
        session: &ChaoxingCookieSession,
        target: &ChaoxingMediaResourceTarget,
    ) -> ProviderResult<ChaoxingVideoStatus> {
        let response = self
            .client
            .get(video_status_url(session, target)?)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json,text/plain,*/*")
            .header(REFERER, media_referer(target.kind()))
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        let body = read_response_body(response).await?;
        let mut status: SensitiveVideoStatus = serde_json::from_str(body.as_str())
            .map_err(|_| protocol_drift("Chaoxing Video status JSON is malformed"))?;
        if status.status != "success" {
            return Err(protocol_drift(
                "Chaoxing Video status endpoint did not report success",
            ));
        }
        ChaoxingVideoStatus::try_new(
            std::mem::take(&mut status.dtoken),
            status.duration,
            status.play_time.unwrap_or_default(),
        )
    }

    async fn report_video_progress_once(
        &self,
        session: &ChaoxingCookieSession,
        request: ChaoxingMediaReportRequest<'_>,
        mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<ChaoxingMediaReportOutcome> {
        let candidates = media_report_candidates(request.target.rt(), request.first_ordinal)?;
        for (ordinal, rt) in candidates {
            let url = video_report_url(
                session,
                request.route,
                request.target,
                request.status,
                request.playing_time_seconds,
                rt,
            )?;
            let request_digest = media_mutation_request_digest(
                ordinal,
                request.target.job_id(),
                request.target.kind(),
                &url,
            )?;
            let http_request = self
                .client
                .get(url)
                .header(COOKIE, session.header_value()?)
                .header(ACCEPT, "application/json,text/plain,*/*")
                .header(REFERER, media_referer(request.target.kind()))
                .build()
                .map_err(|error| classify_reqwest_error(&error))?;
            let issue =
                ExecutionMutationIssue::new(ordinal, MEDIA_MUTATION_OPERATION, request_digest)?;
            mutations.issue(&issue).await?;
            let response = self
                .client
                .execute(http_request)
                .await
                .map_err(|error| media_post_mutation_error(classify_reqwest_error(&error)))?;
            let response_status = response.status();
            let response_result = validate_response_status(&response);
            let body = read_live_mutation_body(response)
                .await
                .map_err(media_post_mutation_error)?;
            let response_digest = media_mutation_response_digest(response_status, body.as_str())?;
            if response_status == StatusCode::FORBIDDEN {
                record_media_receipt(mutations, ordinal, response_digest, false).await?;
                if body.as_str().contains("验证码") || body.as_str().contains("validate") {
                    return Err(ProviderError::human_required(
                        "Chaoxing Video progress requires an image captcha",
                        HumanRequiredReason::ImageCaptcha,
                    ));
                }
                continue;
            }
            if let Err(error) = response_result {
                record_media_receipt(mutations, ordinal, response_digest, false).await?;
                return Err(media_post_mutation_error(error));
            }
            let Ok(report) = serde_json::from_str::<VideoReportResponse>(body.as_str()) else {
                record_media_receipt(mutations, ordinal, response_digest, false).await?;
                return Err(media_post_mutation_error(protocol_drift(
                    "Chaoxing Video report JSON is malformed",
                )));
            };
            record_media_receipt(mutations, ordinal, response_digest, true).await?;
            return ChaoxingMediaReportOutcome::try_new(
                report.is_passed,
                request.first_ordinal,
                ordinal,
            );
        }
        Err(ProviderError::new(
            ProviderErrorKind::Authorization,
            "Chaoxing rejected every supported Video report mode",
        ))
    }

    async fn live_status_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingLiveResourceTarget,
    ) -> ProviderResult<ChaoxingLiveStatus> {
        let response = self
            .client
            .get(live_status_url(session, route, knowledge_id, target)?)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json,text/plain,*/*")
            .header(REFERER, LIVE_REFERER)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        let body = read_response_body(response).await?;
        let status: LiveStatusResponse = serde_json::from_str(body.as_str())
            .map_err(|_| protocol_drift("Chaoxing Live status JSON is malformed"))?;
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Live status requires an identity Cookie",
            )
        })?;
        ChaoxingLiveStatus::try_new(status.temp.data.duration, user_id)
    }

    async fn report_live_progress_once(
        &self,
        session: &ChaoxingCookieSession,
        route: ChaoxingCourseRoute<'_>,
        target: &ChaoxingLiveResourceTarget,
        status: &ChaoxingLiveStatus,
        ordinal: u32,
        mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<bool> {
        let url = live_report_url(session, route, target, status)?;
        let request_digest = live_mutation_request_digest(ordinal, target.job_id(), &url)?;
        let issue = ExecutionMutationIssue::new(ordinal, LIVE_MUTATION_OPERATION, request_digest)?;
        mutations.issue(&issue).await.map_err(|error| {
            if ordinal > 1 {
                live_post_mutation_error(error)
            } else {
                error
            }
        })?;
        let response = self
            .client
            .get(url)
            .header(
                COOKIE,
                session.header_value().map_err(live_post_mutation_error)?,
            )
            .header(ACCEPT, "text/plain,*/*")
            .header(REFERER, LIVE_REFERER)
            .send()
            .await
            .map_err(|error| live_post_mutation_error(classify_reqwest_error(&error)))?;
        validate_response_status(&response).map_err(live_post_mutation_error)?;
        let body = read_live_mutation_body(response)
            .await
            .map_err(live_post_mutation_error)?;
        let response_digest =
            live_mutation_response_digest(body.as_str()).map_err(live_post_mutation_error)?;
        let accepted = body.as_str().trim() == "@success";
        let receipt = ExecutionMutationReceipt::new(ordinal, response_digest, accepted)
            .map_err(live_post_mutation_error)?;
        mutations
            .record_receipt(receipt)
            .await
            .map_err(live_post_mutation_error)?;
        Ok(accepted)
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

    async fn fetch_course_invite_direct_once(
        &self,
        session: &ChaoxingCookieSession,
        preparation: &ChaoxingCourseInvitePreviewPreparation,
    ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument> {
        let (document, source_url) = self
            .get_course_invite_html(
                session,
                course_invite_url(preparation.route(), preparation.query())?,
            )
            .await?;
        ChaoxingCourseInvitePreviewDocument::for_preparation_at(
            preparation,
            source_url,
            document.into_string(),
        )
    }

    async fn fetch_course_invite_api_once(
        &self,
        session: &ChaoxingCookieSession,
        preparation: &ChaoxingCourseInviteApiPreparation,
    ) -> ProviderResult<ChaoxingCourseInviteApiDocument> {
        let response = self
            .client
            .post(static_url(preparation.route())?)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
            .header(
                CONTENT_TYPE,
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .header(ORIGIN, "https://i.chaoxing.com")
            .header(REFERER, "https://i.chaoxing.com/")
            .body(preparation.form_body().to_owned())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        validate_json_response_head(&response)?;
        let body = read_response_body(response).await?;
        ChaoxingCourseInviteApiDocument::for_preparation(preparation, body.into_string())
    }

    async fn fetch_course_invite_redirect_once(
        &self,
        session: &ChaoxingCookieSession,
        redirect: &ChaoxingCourseInvitePreviewRedirect,
    ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument> {
        let (document, source_url) = self
            .get_course_invite_html(
                session,
                course_invite_url(redirect.route(), redirect.query())?,
            )
            .await?;
        ChaoxingCourseInvitePreviewDocument::for_redirect_at(
            redirect,
            source_url,
            document.into_string(),
        )
    }

    fn prepare_course_join_request_once(
        &self,
        session: &ChaoxingCookieSession,
        preparation: &ChaoxingCourseJoinPreparation,
    ) -> ProviderResult<NativeChaoxingCourseJoinRequest> {
        let request = self
            .client
            .get(course_invite_url(preparation.route(), preparation.query())?)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(
                REFERER,
                "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview",
            )
            .build()
            .map_err(|error| classify_reqwest_error(&error))?;
        Ok(NativeChaoxingCourseJoinRequest {
            request,
            request_digest: preparation.request_digest_bytes(),
        })
    }

    fn prepare_frozen_course_join_request_once(
        &self,
        session: &ChaoxingCookieSession,
        draft: &ProviderCourseEnrollmentDraft,
    ) -> ProviderResult<NativeChaoxingCourseJoinRequest> {
        let request = self
            .client
            .get(shared_course_join_url(draft)?)
            .header(COOKIE, session.header_value()?)
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(
                REFERER,
                "https://mooc1.chaoxing.com/addcourse/pcqrcodemiddleview",
            )
            .build()
            .map_err(|error| classify_reqwest_error(&error))?;
        Ok(NativeChaoxingCourseJoinRequest {
            request,
            request_digest: draft.request_digest(),
        })
    }
}

fn is_readable_work_question_url(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with("/work/dowork")
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

    async fn fetch_exam_detail_facts(
        &self,
        context: &ProviderContext,
        _route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingExamDetailRequest<'_>],
    ) -> ProviderResult<Vec<ChaoxingExamDetailFacts>> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_exam_detail_facts_once(&session, requests).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_exam_detail_facts_once(&session, requests).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl ChaoxingQuestionTransport for NativeChaoxingInventoryTransport {
    async fn fetch_work_question_document(
        &self,
        context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_work_question_document_once(&session, request)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_work_question_document_once(&session, request)
                    .await
            }
            result => result,
        }
    }

    async fn fetch_chapter_work_question_document(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        // The donor-observed GET can create the attempt. Session renewal may
        // happen before sending, but an ambiguous response must never replay it.
        let (session, _) = self.session_for_operation(context).await?;
        self.fetch_chapter_work_question_document_once(&session, route, request, target)
            .await
    }

    async fn fetch_exam_question_document(
        &self,
        context: &ProviderContext,
        request: ChaoxingExamQuestionRequest<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument> {
        let (session, _) = self.session_for_operation(context).await?;
        let remote_task_id = format!(
            "exam:{}:{}:{}",
            request.route().course_id(),
            request.route().class_id(),
            request.exam_id()
        );
        let command = self
            .prepare_exam_start_once(&session, TaskId::new(), &remote_task_id, &request)
            .await?;
        Ok(self
            .execute_exam_start_once(&session, &command)
            .await?
            .document)
    }

    async fn prepare_exam_start(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        request: &ChaoxingExamQuestionRequest<'_>,
    ) -> ProviderResult<ChaoxingExamStartCommand> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .prepare_exam_start_once(&session, task_id, remote_task_id, request)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.prepare_exam_start_once(&session, task_id, remote_task_id, request)
                    .await
            }
            result => result,
        }
    }

    async fn execute_exam_start(
        &self,
        context: &ProviderContext,
        command: &ChaoxingExamStartCommand,
    ) -> ProviderResult<ChaoxingExamStartOutcome> {
        // Authentication is resolved before the registered one-shot command.
        // A send error is returned directly and Core retains ambiguity.
        let (session, _) = self.session_for_operation(context).await?;
        self.execute_exam_start_once(&session, command).await
    }
}

#[async_trait]
impl ChaoxingSubmissionTransport for NativeChaoxingInventoryTransport {
    async fn submit_work(
        &self,
        context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<asterism_domain::SubmissionReceipt> {
        let (mut session, renewed) = self.session_for_operation(context).await?;
        let prepared = match self
            .prepare_work_submission_once(&session, request, plan)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                session = self.sessions.renew_session(context).await?;
                self.prepare_work_submission_once(&session, request, plan)
                    .await?
            }
            result => result?,
        };
        self.post_work_submission_once(&session, &prepared.0, &prepared.1)
            .await
    }

    async fn submit_chapter_work(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<asterism_domain::SubmissionReceipt> {
        // Both the attempt GET and final POST can mutate remote state. Resolve
        // authentication before either and never replay after a send.
        let (session, _) = self.session_for_operation(context).await?;
        let (referer, document) = self
            .fetch_chapter_work_page(&session, route, request, target)
            .await?;
        let remote_task_id = format!(
            "resource:{}:{}:{}:{}",
            route.course_id(),
            route.class_id(),
            request.knowledge_id(),
            target.job_id()
        );
        let identity = crate::submission_support::WorkSubmissionIdentity::parse(&remote_task_id)?;
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Chapter Work submission requires one identity Cookie",
            )
        })?;
        let form = ChaoxingSubmissionForm::parse(document.as_str(), identity, plan)?
            .bind_user(user_id)?
            .bind_chapter_target(
                request.knowledge_id(),
                route.cpi(),
                target.work_id(),
                target.job_id(),
            )?;
        self.post_work_submission_once(&session, &referer, &form)
            .await
    }

    async fn prepare_exam_submission(
        &self,
        context: &ProviderContext,
        artifact: ChaoxingExamQuestionArtifact,
        draft: &asterism_domain::SubmissionDraft,
    ) -> ProviderResult<ChaoxingExamSubmissionCommand> {
        let (session, _) = self.session_for_operation(context).await?;
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Exam submission requires one identity Cookie",
            )
        })?;
        let mut entropy = [0_u8; 32];
        OsRng.fill_bytes(&mut entropy);
        ChaoxingExamSubmissionCommand::try_new(artifact, draft, user_id, entropy, Utc::now())
    }

    async fn submit_exam(
        &self,
        context: &ProviderContext,
        command: &ChaoxingExamSubmissionCommand,
    ) -> ProviderResult<ChaoxingExamSubmissionResponse> {
        // Resolve authentication before dispatch, then issue the frozen POST
        // exactly once. Any send/body ambiguity is returned to Core unchanged.
        let (session, _) = self.session_for_operation(context).await?;
        let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing Exam submission requires one identity Cookie",
            )
        })?;
        if !command.belongs_to_user(user_id) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing Exam identity Cookie changed after request preparation",
            ));
        }
        let mut url = static_url(EXAM_SUBMISSION_BASE)?;
        {
            let mut query = url.query_pairs_mut();
            for (name, value) in command.query() {
                query.append_pair(name, value);
            }
        }
        let request = build_exam_submission_request(&self.client, &session, url, command.body())?;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        validate_response_status(&response)?;
        let body = read_response_body(response).await?;
        parse_exam_submission_response(body.as_str(), command.is_final(), Utc::now())
    }
}

#[async_trait]
impl ChaoxingSubmissionVerificationTransport for NativeChaoxingInventoryTransport {
    async fn fetch_work_verification(
        &self,
        context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingWorkVerificationDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_work_verification_once(&session, request).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_work_verification_once(&session, request).await
            }
            result => result,
        }
    }

    async fn fetch_exam_verification(
        &self,
        context: &ProviderContext,
        request: ChaoxingExamDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingExamVerificationDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_exam_verification_once(&session, request).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_exam_verification_once(&session, request).await
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
        mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        let session = self.session_for_operation(context).await?.0;
        self.complete_immediate_resource_once(&session, route, knowledge_id, target, mutations)
            .await
    }
}

#[async_trait]
impl ChaoxingVideoTransport for NativeChaoxingInventoryTransport {
    async fn video_status(
        &self,
        context: &ProviderContext,
        target: &ChaoxingMediaResourceTarget,
    ) -> ProviderResult<ChaoxingVideoStatus> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.video_status_once(&session, target).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.video_status_once(&session, target).await
            }
            result => result,
        }
    }

    async fn report_video_progress(
        &self,
        context: &ProviderContext,
        request: ChaoxingMediaReportRequest<'_>,
        mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<ChaoxingMediaReportOutcome> {
        let session = self.session_for_operation(context).await?.0;
        self.report_video_progress_once(&session, request, mutations)
            .await
    }
}

#[async_trait]
impl ChaoxingLiveTransport for NativeChaoxingInventoryTransport {
    async fn live_status(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingLiveResourceTarget,
    ) -> ProviderResult<ChaoxingLiveStatus> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .live_status_once(&session, route, knowledge_id, target)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.live_status_once(&session, route, knowledge_id, target)
                    .await
            }
            result => result,
        }
    }

    async fn report_live_progress(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        target: &ChaoxingLiveResourceTarget,
        status: &ChaoxingLiveStatus,
        ordinal: u32,
        mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<bool> {
        let session = if ordinal == 1 {
            self.session_for_operation(context).await?.0
        } else {
            self.sessions
                .resolve_session(context)
                .await
                .map_err(live_post_mutation_error)?
        };
        let result = self
            .report_live_progress_once(&session, route, target, status, ordinal, mutations)
            .await;
        if ordinal > 1 {
            result.map_err(live_post_mutation_error)
        } else {
            result
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

#[async_trait]
impl ChaoxingCourseEnrollmentTransport for NativeChaoxingInventoryTransport {
    type PreparedJoin = NativeChaoxingCourseJoinRequest;

    async fn fetch_direct_preview(
        &self,
        context: &ProviderContext,
        preparation: &ChaoxingCourseInvitePreviewPreparation,
    ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_course_invite_direct_once(&session, preparation)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_course_invite_direct_once(&session, preparation)
                    .await
            }
            result => result,
        }
    }

    async fn fetch_invite_api(
        &self,
        context: &ProviderContext,
        preparation: &ChaoxingCourseInviteApiPreparation,
    ) -> ProviderResult<ChaoxingCourseInviteApiDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_course_invite_api_once(&session, preparation)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_course_invite_api_once(&session, preparation)
                    .await
            }
            result => result,
        }
    }

    async fn fetch_redirect_preview(
        &self,
        context: &ProviderContext,
        redirect: &ChaoxingCourseInvitePreviewRedirect,
    ) -> ProviderResult<ChaoxingCourseInvitePreviewDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_course_invite_redirect_once(&session, redirect)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_course_invite_redirect_once(&session, redirect)
                    .await
            }
            result => result,
        }
    }

    async fn prepare_join_transport(
        &self,
        context: &ProviderContext,
        preparation: &ChaoxingCourseJoinPreparation,
    ) -> ProviderResult<Self::PreparedJoin> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.prepare_course_join_request_once(&session, preparation) {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.prepare_course_join_request_once(&session, preparation)
            }
            result => result,
        }
    }

    async fn send_issued_join(
        &self,
        prepared: Self::PreparedJoin,
        issued: ChaoxingIssuedCourseJoin<'_>,
    ) -> ProviderResult<ChaoxingCourseJoinReceiptDocument> {
        if prepared.request_digest != issued.preparation().request_digest_bytes() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing Course join transport binding changed before send",
            ));
        }
        let response = self
            .client
            .execute(prepared.request)
            .await
            .map_err(|_| ambiguous_course_join_error())?;
        validate_response_status(&response).map_err(|_| ambiguous_course_join_error())?;
        validate_json_response_head(&response).map_err(|_| ambiguous_course_join_error())?;
        let body = read_live_mutation_body(response)
            .await
            .map_err(|_| ambiguous_course_join_error())?;
        ChaoxingCourseJoinReceiptDocument::for_preparation(issued.preparation(), body.into_string())
            .map_err(|_| ambiguous_course_join_error())
    }

    async fn prepare_frozen_join_transport(
        &self,
        context: &ProviderContext,
        draft: &ProviderCourseEnrollmentDraft,
    ) -> ProviderResult<Self::PreparedJoin> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.prepare_frozen_course_join_request_once(&session, draft) {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.prepare_frozen_course_join_request_once(&session, draft)
            }
            result => result,
        }
    }

    async fn send_issued_frozen_join(
        &self,
        prepared: Self::PreparedJoin,
        issued: ChaoxingIssuedCourseEnrollment<'_>,
    ) -> ProviderResult<ChaoxingCourseJoinReceiptDocument> {
        if prepared.request_digest != issued.draft().request_digest() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing frozen Course join transport binding changed before send",
            ));
        }
        let response = self
            .client
            .execute(prepared.request)
            .await
            .map_err(|_| ambiguous_course_join_error())?;
        validate_response_status(&response).map_err(|_| ambiguous_course_join_error())?;
        validate_json_response_head(&response).map_err(|_| ambiguous_course_join_error())?;
        let body = read_live_mutation_body(response)
            .await
            .map_err(|_| ambiguous_course_join_error())?;
        ChaoxingCourseJoinReceiptDocument::for_shared_draft(issued.draft(), body.into_string())
            .map_err(|_| ambiguous_course_join_error())
    }
}

#[async_trait]
impl ChaoxingSignActivityReadTransport for NativeChaoxingInventoryTransport {
    async fn fetch_sign_activity_list(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingSignActivityListDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_sign_activity_list_once(&session, route).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_sign_activity_list_once(&session, route).await
            }
            result => result,
        }
    }

    async fn fetch_sign_detail(
        &self,
        context: &ProviderContext,
        request: &ChaoxingSignDetailRequest,
    ) -> ProviderResult<ChaoxingSignDetailDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_sign_detail_once(&session, request).await {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_sign_detail_once(&session, request).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl ChaoxingSignEventReadTransport for NativeChaoxingInventoryTransport {
    async fn fetch_sign_event_bootstrap(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<ChaoxingSignEventBootstrapDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_sign_event_bootstrap_once(&session, context)
            .await
        {
            Err(error) if should_renew_after(&error, renewed) => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_sign_event_bootstrap_once(&session, context)
                    .await
            }
            result => result,
        }
    }
}

fn should_renew_after(error: &ProviderError, already_renewed: bool) -> bool {
    !already_renewed && error.kind == ProviderErrorKind::Authentication
}

pub(crate) struct SensitiveHtml(String);

#[derive(Deserialize)]
struct SensitiveVideoStatus {
    status: String,
    dtoken: String,
    duration: u64,
    #[serde(default, rename = "playTime")]
    play_time: Option<u64>,
}

impl Drop for SensitiveVideoStatus {
    fn drop(&mut self) {
        self.status.zeroize();
        self.dtoken.zeroize();
    }
}

#[derive(Deserialize)]
struct VideoReportResponse {
    #[serde(rename = "isPassed")]
    is_passed: bool,
}

#[derive(Deserialize)]
struct LiveStatusResponse {
    temp: LiveStatusTemp,
}

#[derive(Deserialize)]
struct LiveStatusTemp {
    data: LiveStatusData,
}

#[derive(Deserialize)]
struct LiveStatusData {
    duration: u64,
}

impl SensitiveHtml {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn into_inventory_document(mut self) -> ProviderResult<ChaoxingInventoryDocument> {
        ChaoxingInventoryDocument::try_new(std::mem::take(&mut self.0))
    }

    fn into_string(mut self) -> String {
        std::mem::take(&mut self.0)
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

fn sign_activity_list_url(route: ChaoxingCourseRoute<'_>, fid: &str) -> ProviderResult<Url> {
    let timestamp = Utc::now().timestamp_millis().to_string();
    build_url(
        SIGN_ACTIVITY_LIST_BASE,
        &[
            ("fid", fid),
            ("courseId", route.course_id()),
            ("classId", route.class_id()),
            ("showNotStartedActive", "0"),
            ("_", &timestamp),
        ],
    )
}

fn sign_detail_url(request: &ChaoxingSignDetailRequest) -> ProviderResult<Url> {
    build_url(
        SIGN_DETAIL_BASE,
        &[("activePrimaryId", request.activity_id()), ("type", "1")],
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

fn chapter_work_url(
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    target: &ChaoxingChapterWorkTarget,
) -> ProviderResult<Url> {
    if !request.belongs_to(route) {
        return Err(protocol_drift(
            "Chaoxing Chapter Work request is outside the current course route",
        ));
    }
    build_url(
        CHAPTER_WORK_BASE,
        &[
            ("api", "1"),
            ("workId", target.work_id()),
            ("jobid", target.job_id()),
            ("originJobId", target.job_id()),
            ("needRedirect", "true"),
            ("skipHeader", "true"),
            ("knowledgeid", request.knowledge_id()),
            ("ktoken", target.knowledge_token().expose_secret()),
            ("cpi", route.cpi()),
            ("ut", "s"),
            ("clazzId", route.class_id()),
            ("type", ""),
            ("enc", target.enc().expose_secret()),
            ("mooc2", "1"),
            ("courseid", route.course_id()),
        ],
    )
}

fn valid_chapter_work_url(
    url: &Url,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    target: &ChaoxingChapterWorkTarget,
) -> bool {
    let path = url.path().to_ascii_lowercase();
    if url.as_str().len() > 16 * 1_024
        || url.query_pairs().count() > 64
        || url.scheme() != "https"
        || url.host_str() != Some("mooc1.chaoxing.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || !request.belongs_to(route)
        || unique_query(url, "courseid").as_deref() != Some(route.course_id())
        || unique_query_aliases(url, &["clazzId", "classId"]).as_deref() != Some(route.class_id())
        || unique_query(url, "knowledgeid").as_deref() != Some(request.knowledge_id())
        || unique_query(url, "cpi").as_deref() != Some(route.cpi())
        || unique_query(url, "enc").as_deref() != Some(target.enc().expose_secret())
    {
        return false;
    }
    match path.as_str() {
        "/mooc-ans/api/work" => {
            unique_query(url, "workId").as_deref() == Some(target.work_id())
                && unique_query(url, "jobid").as_deref() == Some(target.job_id())
                && unique_query(url, "originJobId").as_deref() == Some(target.job_id())
                && unique_query(url, "ktoken").as_deref()
                    == Some(target.knowledge_token().expose_secret())
        }
        "/mooc-ans/work/dohomeworknew" => {
            unique_query(url, "oldWorkId").as_deref() == Some(target.work_id())
                && unique_query(url, "jobid").as_deref() == Some(target.job_id())
                && unique_query(url, "originJobId").as_deref() == Some(target.job_id())
                && unique_query(url, "workId").is_some_and(|value| valid_route_component(&value))
        }
        _ => false,
    }
}

fn valid_route_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn unique_query_aliases<'a>(url: &'a Url, keys: &[&str]) -> Option<Cow<'a, str>> {
    let mut values = url
        .query_pairs()
        .filter(|(candidate, _)| keys.iter().any(|key| candidate.eq_ignore_ascii_case(key)))
        .map(|(_, value)| value);
    let value = values.next()?;
    values.next().is_none().then_some(value)
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

fn video_status_url(
    session: &ChaoxingCookieSession,
    target: &ChaoxingMediaResourceTarget,
) -> ProviderResult<Url> {
    let mut url = static_url(VIDEO_STATUS_ORIGIN)?;
    url.path_segments_mut()
        .map_err(|()| static_route_error())?
        .extend(["ananas", "status", target.object_id().expose_secret()]);
    let fid = session.cookie_value(&["fid"]).unwrap_or("1024");
    url.query_pairs_mut()
        .append_pair("k", fid)
        .append_pair("flag", "normal");
    Ok(url)
}

fn video_report_url(
    session: &ChaoxingCookieSession,
    route: ChaoxingCourseRoute<'_>,
    target: &ChaoxingMediaResourceTarget,
    status: &ChaoxingVideoStatus,
    playing_time_seconds: u64,
    rt: ChaoxingVideoRt,
) -> ProviderResult<Url> {
    if playing_time_seconds > status.duration_seconds() {
        return Err(protocol_drift(
            "Chaoxing Video report exceeds the current duration",
        ));
    }
    let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Video report requires an identity Cookie",
        )
    })?;
    let duration_millis = status
        .duration_seconds()
        .checked_mul(1_000)
        .ok_or_else(|| protocol_drift("Chaoxing Video duration overflowed"))?;
    let playing_millis = playing_time_seconds
        .checked_mul(1_000)
        .ok_or_else(|| protocol_drift("Chaoxing Video progress overflowed"))?;
    let mut signature_input = format!(
        "[{}][{user_id}][{}][{}][{playing_millis}][d_yHJ!$pdA~5][{duration_millis}][0_{}]",
        route.class_id(),
        target.job_id(),
        target.object_id().expose_secret(),
        status.duration_seconds(),
    );
    let signature = format!("{:x}", md5::compute(signature_input.as_bytes()));
    signature_input.zeroize();
    let mut url = static_url(VIDEO_REPORT_ORIGIN)?;
    url.path_segments_mut()
        .map_err(|()| static_route_error())?
        .extend([
            "mooc-ans",
            "multimedia",
            "log",
            "a",
            route.cpi(),
            status.report_token().expose_secret(),
        ]);
    let duration = status.duration_seconds().to_string();
    let playing_time = playing_time_seconds.to_string();
    let clip_time = format!("0_{duration}");
    let timestamp = Utc::now().timestamp_millis().to_string();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("clazzId", route.class_id())
            .append_pair("playingTime", &playing_time)
            .append_pair("duration", &duration)
            .append_pair("clipTime", &clip_time)
            .append_pair("objectId", target.object_id().expose_secret())
            .append_pair("otherInfo", target.other_info().expose_secret())
            .append_pair("courseId", route.course_id())
            .append_pair("jobid", target.job_id())
            .append_pair("userid", user_id)
            .append_pair("isdrag", "3")
            .append_pair("view", "pc")
            .append_pair("enc", &signature)
            .append_pair("dtype", target.kind().dtype())
            .append_pair("rt", rt.as_str())
            .append_pair("_t", &timestamp);
        if let Some(value) = target.face_capture_enc() {
            query.append_pair("videoFaceCaptureEnc", value.expose_secret());
        }
        if let Some(value) = target.attendance_duration() {
            query.append_pair("attDuration", value.expose_secret());
        }
        if let Some(value) = target.attendance_duration_enc() {
            query.append_pair("attDurationEnc", value.expose_secret());
        }
    }
    Ok(url)
}

const fn media_referer(kind: ChaoxingMediaKind) -> &'static str {
    match kind {
        ChaoxingMediaKind::Video => VIDEO_REFERER,
        ChaoxingMediaKind::Audio => AUDIO_REFERER,
    }
}

fn live_status_url(
    session: &ChaoxingCookieSession,
    route: ChaoxingCourseRoute<'_>,
    knowledge_id: &str,
    target: &ChaoxingLiveResourceTarget,
) -> ProviderResult<Url> {
    let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Live status requires an identity Cookie",
        )
    })?;
    let mut url = static_url(LIVE_STATUS_BASE)?;
    url.query_pairs_mut()
        .append_pair("liveid", target.live_id().expose_secret())
        .append_pair("userid", user_id)
        .append_pair("clazzid", route.class_id())
        .append_pair("knowledgeid", knowledge_id)
        .append_pair("courseid", route.course_id())
        .append_pair(
            "jobid",
            target
                .status_job_id()
                .map_or("", SecretString::expose_secret),
        )
        .append_pair("ut", "s");
    Ok(url)
}

fn live_report_url(
    session: &ChaoxingCookieSession,
    route: ChaoxingCourseRoute<'_>,
    target: &ChaoxingLiveResourceTarget,
    status: &ChaoxingLiveStatus,
) -> ProviderResult<Url> {
    let user_id = session.cookie_value(&["_uid", "UID"]).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Live heartbeat requires an identity Cookie",
        )
    })?;
    if user_id != status.user_id().expose_secret() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Live session identity changed after status discovery",
        ));
    }
    let timestamp = Utc::now().timestamp_millis().to_string();
    let mut url = static_url(LIVE_REPORT_BASE)?;
    url.query_pairs_mut()
        .append_pair("streamName", target.stream_name().expose_secret())
        .append_pair("vdoid", target.video_id().expose_secret())
        .append_pair("userId", user_id)
        .append_pair("isStart", "0")
        .append_pair("t", &timestamp)
        .append_pair("courseId", route.course_id());
    Ok(url)
}

fn immediate_mutation_request_digest(job_id: &str, url: &Url) -> ProviderResult<[u8; 32]> {
    if job_id.is_empty()
        || job_id.len() > 128
        || job_id.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str() != Some("mooc1.chaoxing.com")
        || !matches!(url.path(), "/ananas/job/document" | "/ananas/job/readv2")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.as_str().len() > 16 * 1_024
    {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing immediate resource request identity is invalid",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(IMMEDIATE_REQUEST_DIGEST_DOMAIN);
    hash_immediate_component(&mut hash, b"GET")?;
    hash_immediate_component(&mut hash, job_id.as_bytes())?;
    hash_immediate_component(&mut hash, url.as_str().as_bytes())?;
    Ok(hash.finalize().into())
}

fn immediate_mutation_response_digest(status: StatusCode, body: &str) -> ProviderResult<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(IMMEDIATE_RESPONSE_DIGEST_DOMAIN);
    hash.update(status.as_u16().to_be_bytes());
    hash_immediate_component(&mut hash, body.as_bytes())?;
    Ok(hash.finalize().into())
}

fn media_mutation_request_digest(
    ordinal: u32,
    job_id: &str,
    kind: ChaoxingMediaKind,
    url: &Url,
) -> ProviderResult<[u8; 32]> {
    if !(1..=100_000).contains(&ordinal)
        || job_id.is_empty()
        || job_id.len() > 128
        || job_id.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str() != Some("mooc1.chaoxing.com")
        || !url.path().starts_with("/mooc-ans/multimedia/log/a/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.as_str().len() > 32 * 1_024
    {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing media request identity is invalid",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(MEDIA_REQUEST_DIGEST_DOMAIN);
    hash.update(ordinal.to_be_bytes());
    hash_immediate_component(&mut hash, b"GET")?;
    hash_immediate_component(&mut hash, kind.resource_kind().as_bytes())?;
    hash_immediate_component(&mut hash, job_id.as_bytes())?;
    hash_immediate_component(&mut hash, url.as_str().as_bytes())?;
    hash_immediate_component(&mut hash, media_referer(kind).as_bytes())?;
    Ok(hash.finalize().into())
}

fn media_report_candidates(
    frozen_rt: Option<ChaoxingVideoRt>,
    first_ordinal: u32,
) -> ProviderResult<Vec<(u32, ChaoxingVideoRt)>> {
    let modes = frozen_rt.map_or_else(
        || vec![ChaoxingVideoRt::NineTenths, ChaoxingVideoRt::One],
        |rt| vec![rt],
    );
    modes
        .into_iter()
        .enumerate()
        .map(|(offset, rt)| {
            let offset = u32::try_from(offset).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Chaoxing media mutation ordinal is invalid",
                )
            })?;
            let ordinal = first_ordinal
                .checked_add(offset)
                .filter(|value| (1..=100_000).contains(value))
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "Chaoxing media mutation ordinal overflowed",
                    )
                })?;
            Ok((ordinal, rt))
        })
        .collect()
}

fn media_mutation_response_digest(status: StatusCode, body: &str) -> ProviderResult<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(MEDIA_RESPONSE_DIGEST_DOMAIN);
    hash.update(status.as_u16().to_be_bytes());
    hash_immediate_component(&mut hash, body.as_bytes())?;
    Ok(hash.finalize().into())
}

async fn record_media_receipt(
    mutations: &(dyn ExecutionMutationSink + Send + Sync),
    ordinal: u32,
    response_digest: [u8; 32],
    accepted: bool,
) -> ProviderResult<()> {
    let receipt = ExecutionMutationReceipt::new(ordinal, response_digest, accepted)
        .map_err(media_post_mutation_error)?;
    mutations
        .record_receipt(receipt)
        .await
        .map_err(media_post_mutation_error)
}

fn hash_immediate_component(hash: &mut Sha256, value: &[u8]) -> ProviderResult<()> {
    let length = u64::try_from(value.len()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing immediate resource digest input is invalid",
        )
    })?;
    hash.update(length.to_be_bytes());
    hash.update(value);
    Ok(())
}

fn live_mutation_request_digest(ordinal: u32, job_id: &str, url: &Url) -> ProviderResult<[u8; 32]> {
    if !(1..=100_000).contains(&ordinal)
        || job_id.is_empty()
        || job_id.len() > 128
        || job_id.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str() != Some("zhibo.chaoxing.com")
        || url.path() != "/saveTimePc"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.as_str().len() > 16 * 1_024
    {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Live mutation request identity is invalid",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(LIVE_REQUEST_DIGEST_DOMAIN);
    hash.update(ordinal.to_be_bytes());
    hash_live_component(&mut hash, b"GET")?;
    hash_live_component(&mut hash, job_id.as_bytes())?;
    hash_live_component(&mut hash, url.as_str().as_bytes())?;
    hash_live_component(&mut hash, LIVE_REFERER.as_bytes())?;
    Ok(hash.finalize().into())
}

fn live_mutation_response_digest(body: &str) -> ProviderResult<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(LIVE_RESPONSE_DIGEST_DOMAIN);
    hash_live_component(&mut hash, body.as_bytes())?;
    Ok(hash.finalize().into())
}

fn hash_live_component(hash: &mut Sha256, value: &[u8]) -> ProviderResult<()> {
    let length = u64::try_from(value.len()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Live mutation digest input is invalid",
        )
    })?;
    hash.update(length.to_be_bytes());
    hash.update(value);
    Ok(())
}

fn build_url(base: &str, query: &[(&str, &str)]) -> ProviderResult<Url> {
    let mut url = static_url(base)?;
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    Ok(url)
}

fn course_invite_url(base: &str, query: &str) -> ProviderResult<Url> {
    if query.is_empty() || query.len() > 2_048 || query.chars().any(char::is_control) {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing Course invite query is invalid",
        ));
    }
    let mut url = static_url(base)?;
    url.set_query(Some(query));
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

fn build_exam_submission_request(
    client: &Client,
    session: &ChaoxingCookieSession,
    url: Url,
    body: &[(String, String)],
) -> ProviderResult<Request> {
    client
        .post(url)
        .header(COOKIE, session.header_value()?)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header("x-requested-with", EXAM_REQUESTED_WITH)
        .form(body)
        .build()
        .map_err(|error| classify_reqwest_error(&error))
}

fn build_work_submission_request(
    client: &Client,
    session: &ChaoxingCookieSession,
    url: Url,
    referer: &Url,
    body: &[(String, String)],
) -> ProviderResult<Request> {
    client
        .post(url)
        .header(COOKIE, session.header_value()?)
        .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
        .header(
            CONTENT_TYPE,
            "application/x-www-form-urlencoded; charset=UTF-8",
        )
        .header(ORIGIN, WORK_LIST_ORIGIN)
        .header(REFERER, referer.as_str())
        .header("x-requested-with", WORK_REQUESTED_WITH)
        .form(body)
        .build()
        .map_err(|error| classify_reqwest_error(&error))
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

fn validate_json_response_head(response: &Response) -> ProviderResult<()> {
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let content_type = content_type.to_str().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing sign-in endpoint returned an invalid Content-Type",
            )
        })?;
        let media_type = content_type.split(';').next().unwrap_or_default().trim();
        if !media_type.eq_ignore_ascii_case("application/json")
            && !media_type.eq_ignore_ascii_case("text/json")
            && !media_type.eq_ignore_ascii_case("text/plain")
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing sign-in endpoint returned a non-JSON response",
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

async fn read_response_body(response: Response) -> ProviderResult<SensitiveHtml> {
    read_bounded_response_body(response, true).await
}

async fn read_live_mutation_body(response: Response) -> ProviderResult<SensitiveHtml> {
    read_bounded_response_body(response, false).await
}

async fn read_bounded_response_body(
    mut response: Response,
    detect_login: bool,
) -> ProviderResult<SensitiveHtml> {
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
    if detect_login && looks_like_login_page(&html) {
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

fn ambiguous_course_join_error() -> ProviderError {
    ProviderError::human_required(
        "Chaoxing Course join was issued and requires fresh Course inventory recovery",
        HumanRequiredReason::ManualIntervention,
    )
}

fn immediate_post_mutation_error(error: ProviderError) -> ProviderError {
    if error.kind == ProviderErrorKind::HumanRequired {
        return error;
    }
    let reason = if error.kind == ProviderErrorKind::Authentication {
        HumanRequiredReason::SessionExpired
    } else {
        HumanRequiredReason::ManualIntervention
    };
    ProviderError::human_required(
        "Chaoxing immediate resource completion was issued and requires fresh progress recovery",
        reason,
    )
}

fn media_post_mutation_error(error: ProviderError) -> ProviderError {
    if error.kind == ProviderErrorKind::HumanRequired {
        return error;
    }
    let reason = if error.kind == ProviderErrorKind::Authentication {
        HumanRequiredReason::SessionExpired
    } else {
        HumanRequiredReason::ManualIntervention
    };
    ProviderError::human_required(
        "Chaoxing media progress was issued and requires fresh progress recovery",
        reason,
    )
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

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
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
    const COURSE_INVITE_PREVIEW: &str =
        include_str!("../../../fixtures/providers/chaoxing/course-invite/middle-view.html");

    struct UnavailableSessions;

    #[async_trait]
    impl ChaoxingSessionResolver for UnavailableSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<ChaoxingCookieSession> {
            Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "fixture session resolver must not be called",
            ))
        }
    }

    #[test]
    fn course_join_request_is_fully_built_before_durable_issue() {
        let preview_request =
            ChaoxingCourseInvitePreviewPreparation::try_prepare("12345678".to_owned()).unwrap();
        let document = ChaoxingCourseInvitePreviewDocument::for_preparation(
            &preview_request,
            COURSE_INVITE_PREVIEW.to_owned(),
        )
        .unwrap();
        let preview = crate::parse_course_invite_preview(&document).unwrap();
        let join = ChaoxingCourseJoinPreparation::try_prepare(&preview).unwrap();
        let session = ChaoxingCookieSession::try_new("_uid=777; fid=888").unwrap();
        let transport = NativeChaoxingInventoryTransport {
            client: Client::new(),
            sessions: Arc::new(UnavailableSessions),
        };

        let prepared = transport
            .prepare_course_join_request_once(&session, &join)
            .unwrap();
        assert_eq!(prepared.request.method(), reqwest::Method::GET);
        assert_eq!(
            prepared.request.url().host_str(),
            Some("mooc1.chaoxing.com")
        );
        assert_eq!(
            prepared.request.url().path(),
            "/mooc-ans/teachingClassPhoneManage/phone/participateCls"
        );
        assert_eq!(prepared.request_digest, join.request_digest_bytes());
        assert!(
            prepared
                .request
                .headers()
                .get(COOKIE)
                .expect("Cookie is built before issue")
                .is_sensitive()
        );
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("_uid=777"));
        assert!(!debug.contains("12345678"));
        assert!(!debug.contains("PRIVATE_ADD_CLASS_ENC"));

        let draft = ProviderCourseEnrollmentDraft::try_new(
            asterism_domain::ProviderId::new("chaoxing").unwrap(),
            "chaoxing.course-enrollment.v1",
            "1001",
            "2001",
            serde_json::json!({"course_id": "1001", "class_id": "2001"}),
            join.frozen_request(),
        )
        .unwrap();
        let frozen = transport
            .prepare_frozen_course_join_request_once(&session, &draft)
            .unwrap();
        assert_eq!(frozen.request.method(), reqwest::Method::GET);
        assert_eq!(frozen.request.url(), prepared.request.url());
        assert_eq!(frozen.request_digest, draft.request_digest());
        let debug = format!("{frozen:?}");
        assert!(!debug.contains("12345678"));
        assert!(!debug.contains("PRIVATE_ADD_CLASS_ENC"));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps Video and Audio request identities and their shared mutation digest boundary in one audit"
    )]
    fn video_routes_bind_identity_tokens_and_donor_signature() {
        let session = ChaoxingCookieSession::try_new("_uid=777; fid=888; uf=SAFE_UF").unwrap();
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let scope = route.parser_scope().unwrap();
        let target = crate::resource_inventory::locate_media_resource_target(
            RESOURCE_MIXED,
            &scope,
            "4001",
            0,
            "resource:100:200:4001:job-video",
        )
        .unwrap()
        .unwrap();
        let status_url = video_status_url(&session, &target).unwrap();
        assert_eq!(status_url.path(), "/ananas/status/SAFE_VIDEO_OBJECT");
        assert_eq!(query(&status_url, "k").as_deref(), Some("888"));
        assert_eq!(query(&status_url, "flag").as_deref(), Some("normal"));

        let status = ChaoxingVideoStatus::try_new("SAFE_DTOKEN", 135, 15).unwrap();
        let report = video_report_url(
            &session,
            route,
            &target,
            &status,
            75,
            ChaoxingVideoRt::NineTenths,
        )
        .unwrap();
        assert_eq!(report.path(), "/mooc-ans/multimedia/log/a/300/SAFE_DTOKEN");
        for (key, expected) in [
            ("clazzId", "200"),
            ("playingTime", "75"),
            ("duration", "135"),
            ("clipTime", "0_135"),
            ("objectId", "SAFE_VIDEO_OBJECT"),
            ("otherInfo", "SAFE_VIDEO_REPORT-rt_d"),
            ("courseId", "100"),
            ("jobid", "job-video"),
            ("userid", "777"),
            ("isdrag", "3"),
            ("view", "pc"),
            ("dtype", "Video"),
            ("rt", "0.9"),
            ("enc", "58a0139ba4e5641ae6cce6a09524e6ac"),
            ("videoFaceCaptureEnc", "SAFE_FACE_ENC"),
            ("attDuration", "SAFE_ATTENDANCE"),
            ("attDurationEnc", "SAFE_ATTENDANCE_ENC"),
        ] {
            assert_eq!(query(&report, key).as_deref(), Some(expected), "{key}");
        }
        assert!(query(&report, "_t").is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        }));
        assert!(!report.as_str().contains("SAFE_UF"));
        let first_report_digest =
            media_mutation_request_digest(1, target.job_id(), target.kind(), &report).unwrap();
        assert_eq!(
            first_report_digest,
            media_mutation_request_digest(1, target.job_id(), target.kind(), &report).unwrap()
        );
        assert_ne!(
            first_report_digest,
            media_mutation_request_digest(2, target.job_id(), target.kind(), &report).unwrap()
        );
        assert_ne!(
            first_report_digest,
            media_mutation_response_digest(StatusCode::OK, r#"{"isPassed":false}"#).unwrap()
        );

        let audio = crate::resource_inventory::locate_media_resource_target(
            RESOURCE_MIXED,
            &scope,
            "4001",
            0,
            "resource:100:200:4001:job-audio",
        )
        .unwrap()
        .unwrap();
        assert_eq!(audio.kind(), ChaoxingMediaKind::Audio);
        assert_eq!(media_referer(audio.kind()), AUDIO_REFERER);
        let audio_status = ChaoxingVideoStatus::try_new("SAFE_AUDIO_TOKEN", 65, 5).unwrap();
        let audio_report = video_report_url(
            &session,
            route,
            &audio,
            &audio_status,
            65,
            ChaoxingVideoRt::One,
        )
        .unwrap();
        assert_eq!(
            audio_report.path(),
            "/mooc-ans/multimedia/log/a/300/SAFE_AUDIO_TOKEN"
        );
        assert_ne!(
            first_report_digest,
            media_mutation_request_digest(1, audio.job_id(), audio.kind(), &audio_report).unwrap()
        );
        for (key, expected) in [
            ("objectId", "SAFE_AUDIO_OBJECT"),
            ("otherInfo", "SAFE_AUDIO_REPORT-rt_1"),
            ("jobid", "job-audio"),
            ("dtype", "Audio"),
            ("rt", "1"),
        ] {
            assert_eq!(
                query(&audio_report, key).as_deref(),
                Some(expected),
                "{key}"
            );
        }
    }

    #[test]
    fn media_report_candidates_reserve_one_ordinal_per_frozen_rt_attempt() {
        assert_eq!(
            media_report_candidates(None, 7).unwrap(),
            [(7, ChaoxingVideoRt::NineTenths), (8, ChaoxingVideoRt::One),]
        );
        assert_eq!(
            media_report_candidates(Some(ChaoxingVideoRt::One), 7).unwrap(),
            [(7, ChaoxingVideoRt::One)]
        );
        let error = media_report_candidates(None, 100_000).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[test]
    fn live_routes_and_digests_bind_every_heartbeat_identity() {
        let session = ChaoxingCookieSession::try_new("_uid=777; uf=SAFE_UF").unwrap();
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let scope = route.parser_scope().unwrap();
        let target = crate::resource_inventory::locate_live_resource_target(
            RESOURCE_MIXED,
            &scope,
            "4001",
            0,
            "resource:100:200:4001:job-live",
        )
        .unwrap()
        .unwrap();
        let status_url = live_status_url(&session, route, "4001", &target).unwrap();
        assert_eq!(status_url.path(), "/ananas/live/liveinfo");
        for (key, expected) in [
            ("liveid", "PRIVATE_LIVE"),
            ("userid", "777"),
            ("clazzid", "200"),
            ("knowledgeid", "4001"),
            ("courseid", "100"),
            ("jobid", "PRIVATE_LIVE_JOB"),
            ("ut", "s"),
        ] {
            assert_eq!(query(&status_url, key).as_deref(), Some(expected), "{key}");
        }

        let status = ChaoxingLiveStatus::try_new(125, "777").unwrap();
        let report = live_report_url(&session, route, &target, &status).unwrap();
        assert_eq!(report.path(), "/saveTimePc");
        for (key, expected) in [
            ("streamName", "PRIVATE_STREAM"),
            ("vdoid", "PRIVATE_VDOID"),
            ("userId", "777"),
            ("isStart", "0"),
            ("courseId", "100"),
        ] {
            assert_eq!(query(&report, key).as_deref(), Some(expected), "{key}");
        }
        assert!(query(&report, "t").is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        }));
        let first = live_mutation_request_digest(1, target.job_id(), &report).unwrap();
        assert_eq!(
            first,
            live_mutation_request_digest(1, target.job_id(), &report).unwrap()
        );
        assert_ne!(
            first,
            live_mutation_request_digest(2, target.job_id(), &report).unwrap()
        );
        assert_ne!(
            first,
            live_mutation_request_digest(1, "job-live-other", &report).unwrap()
        );
        assert_ne!(first, live_mutation_response_digest("@success").unwrap());
        assert!(!report.as_str().contains("SAFE_UF"));

        let foreign = ChaoxingLiveStatus::try_new(125, "778").unwrap();
        assert!(live_report_url(&session, route, &target, &foreign).is_err());
    }

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
        let read_digest = immediate_mutation_request_digest(read.job_id(), &read_url).unwrap();
        assert_eq!(
            read_digest,
            immediate_mutation_request_digest(read.job_id(), &read_url).unwrap()
        );
        assert_ne!(
            read_digest,
            immediate_mutation_request_digest("job-read-other", &read_url).unwrap()
        );
        assert_ne!(
            read_digest,
            immediate_mutation_response_digest(StatusCode::OK, "accepted").unwrap()
        );

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
        assert_ne!(
            read_digest,
            immediate_mutation_request_digest(document.job_id(), &document_url).unwrap()
        );
        assert_ne!(
            immediate_mutation_response_digest(StatusCode::OK, "accepted").unwrap(),
            immediate_mutation_response_digest(StatusCode::BAD_REQUEST, "accepted").unwrap()
        );

        let work_url = discover_work_list_url(COURSE_PAGE, route).unwrap();
        assert_eq!(work_url.host_str(), Some("mooc1.chaoxing.com"));
        assert_eq!(query(&work_url, "enc").as_deref(), Some("SAFE_ENC"));
    }

    #[test]
    fn chapter_work_routes_bind_fresh_attempt_and_redirect_aliases() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let scope = route.parser_scope().unwrap();
        let chapter = crate::parse_chapter_inventory(CHAPTER_MIXED, &scope)
            .unwrap()
            .remove(0);
        let request = ChaoxingChapterResourceRequest::try_from_chapter(&chapter)
            .unwrap()
            .unwrap();
        let target = crate::resource_inventory::locate_chapter_work_target(
            RESOURCE_MIXED,
            &scope,
            "4001",
            0,
            "resource:100:200:4001:job-work",
        )
        .unwrap()
        .unwrap();
        let url = chapter_work_url(route, &request, &target).unwrap();
        assert_eq!(url.path(), "/mooc-ans/api/work");
        for (key, expected) in [
            ("workId", "job-work"),
            ("jobid", "job-work"),
            ("ktoken", "PRIVATE_TOKEN"),
            ("enc", "PRIVATE_ENC"),
        ] {
            assert_eq!(query(&url, key).as_deref(), Some(expected), "{key}");
        }
        assert!(valid_chapter_work_url(&url, route, &request, &target));

        let redirect = Url::parse(
            "https://mooc1.chaoxing.com/mooc-ans/work/doHomeWorkNew?courseId=100&classId=200&knowledgeid=4001&cpi=300&enc=PRIVATE_ENC&oldWorkId=job-work&jobid=job-work&originJobId=job-work&workId=server-123",
        )
        .unwrap();
        assert!(valid_chapter_work_url(&redirect, route, &request, &target));
        let foreign = Url::parse(
            "https://mooc1.chaoxing.com/mooc-ans/work/doHomeWorkNew?courseId=100&classId=201&knowledgeid=4001&cpi=300&enc=PRIVATE_ENC&oldWorkId=job-work&jobid=job-work&originJobId=job-work&workId=server-123",
        )
        .unwrap();
        assert!(!valid_chapter_work_url(&foreign, route, &request, &target));
    }

    #[test]
    fn question_reads_accept_only_the_independent_work_editor_route() {
        let editor = Url::parse(
            "https://mooc1.chaoxing.com/mooc-ans/mooc2/work/dowork?courseId=100&classId=200&workId=work-1",
        )
        .unwrap();
        assert!(is_readable_work_question_url(&editor));

        for route in ["view", "prompt", "task"] {
            let url = Url::parse(&format!(
                "https://mooc1.chaoxing.com/mooc-ans/mooc2/work/{route}?courseId=100&classId=200&workId=work-1"
            ))
            .unwrap();
            assert!(!is_readable_work_question_url(&url));
        }
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
    fn sign_reads_bind_exact_course_and_activity_routes() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let list_url = sign_activity_list_url(route, "400").unwrap();
        assert_eq!(
            list_url.origin().ascii_serialization(),
            "https://mobilelearn.chaoxing.com"
        );
        assert_eq!(list_url.path(), "/v2/apis/active/student/activelist");
        assert_eq!(query(&list_url, "fid").as_deref(), Some("400"));
        assert_eq!(query(&list_url, "courseId").as_deref(), Some("100"));
        assert_eq!(query(&list_url, "classId").as_deref(), Some("200"));
        assert_eq!(
            query(&list_url, "showNotStartedActive").as_deref(),
            Some("0")
        );
        assert!(query(&list_url, "_").is_some_and(|value| value.parse::<i64>().is_ok()));

        let scope = route.parser_scope().unwrap();
        let document = ChaoxingSignActivityListDocument::try_new(
            r#"{"result":1,"data":{"activeList":[{"id":7001,"type":2,"otherId":0,"status":1,"name":"attendance"}]}}"#,
        )
        .unwrap();
        let activity = crate::parse_sign_activity_list(&document, &scope)
            .unwrap()
            .remove(0);
        let detail_request = ChaoxingSignDetailRequest::try_new(route, &activity).unwrap();
        let detail_url = sign_detail_url(&detail_request).unwrap();
        assert_eq!(
            detail_url.origin().ascii_serialization(),
            "https://mobilelearn.chaoxing.com"
        );
        assert_eq!(detail_url.path(), "/newsign/signDetail");
        assert_eq!(
            query(&detail_url, "activePrimaryId").as_deref(),
            Some("7001")
        );
        assert_eq!(query(&detail_url, "type").as_deref(), Some("1"));

        let event_bootstrap = static_url(SIGN_EVENT_BOOTSTRAP_BASE).unwrap();
        assert_eq!(
            event_bootstrap.origin().ascii_serialization(),
            "https://im.chaoxing.com"
        );
        assert_eq!(event_bootstrap.path(), "/webim/me");
        assert!(event_bootstrap.query().is_none());
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
    fn exam_submission_request_uses_the_donor_mobile_header() {
        let client = Client::new();
        let session = ChaoxingCookieSession::try_new("_uid=9001; uf=SAFE_UF").unwrap();
        let request = build_exam_submission_request(
            &client,
            &session,
            Url::parse(EXAM_SUBMISSION_BASE).unwrap(),
            &[("tempSave".to_owned(), "true".to_owned())],
        )
        .unwrap();

        assert_eq!(
            request
                .headers()
                .get("x-requested-with")
                .and_then(|value| value.to_str().ok()),
            Some(EXAM_REQUESTED_WITH)
        );
        assert_eq!(EXAM_REQUESTED_WITH, "com.chaoxing.mobile");
    }

    #[test]
    fn work_submission_request_uses_the_donor_web_ajax_header() {
        let client = Client::new();
        let session = ChaoxingCookieSession::try_new("_uid=9001; uf=SAFE_UF").unwrap();
        let referer = Url::parse(
            "https://mooc1.chaoxing.com/mooc-ans/work/doHomeWorkNew?courseId=100&classId=200",
        )
        .unwrap();
        let request = build_work_submission_request(
            &client,
            &session,
            Url::parse(WORK_SUBMISSION_BASE).unwrap(),
            &referer,
            &[("pyFlag".to_owned(), String::new())],
        )
        .unwrap();

        assert_eq!(
            request
                .headers()
                .get("x-requested-with")
                .and_then(|value| value.to_str().ok()),
            Some(WORK_REQUESTED_WITH)
        );
        assert_eq!(WORK_REQUESTED_WITH, "XMLHttpRequest");
        assert_eq!(
            request
                .headers()
                .get(ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some(WORK_LIST_ORIGIN)
        );
        assert_eq!(
            request
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/x-www-form-urlencoded; charset=UTF-8")
        );
        assert_eq!(
            request
                .headers()
                .get(REFERER)
                .and_then(|value| value.to_str().ok()),
            Some(referer.as_str())
        );
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

    #[test]
    fn sign_json_content_type_is_fail_closed() {
        let json = response(
            StatusCode::OK,
            &[("content-type", "application/json; charset=utf-8")],
            b"{}".to_vec(),
        );
        assert!(validate_json_response_head(&json).is_ok());

        let html = response(
            StatusCode::OK,
            &[("content-type", "text/html")],
            b"{}".to_vec(),
        );
        let error = validate_json_response_head(&html).unwrap_err();
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
