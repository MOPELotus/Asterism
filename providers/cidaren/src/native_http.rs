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
    CidarenAnswerEvidenceBinding, CidarenAnswerEvidenceTransport, CidarenAssessmentBinding,
    CidarenAssessmentResponse, CidarenAssessmentTransport, CidarenAuthenticationTransport,
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, CidarenMutationRequest,
    CidarenSessionResolver, CidarenStartAnswerRequest, CidarenStudyTaskDocument,
    CidarenStudyTaskTransport, CidarenTokenSession, CidarenWireAnswer, CidarenWordEvidence,
    CidarenWordInfoRequest, CidarenWordInventory, CidarenWordInventoryRequest, CidarenWordLookup,
    CidarenWordPrototypeRequest, CidarenWordSelectionPlan,
    answer_evidence_protocol::fresh_course_id, assessment_protocol::CidarenMutationAuthorization,
    authentication::selected_course_id, build_skip_answer_request, build_start_answer_request,
    build_submit_answer_and_save_request, build_submit_chose_word_request,
    build_verify_answer_request, build_word_info_request, build_word_inventory_request,
    build_word_prototype_request, class_tasks::class_task_total,
    classify_token_validation_response, parse_assessment_response, parse_course_page_response,
    parse_study_task_info_response, parse_word_info_response, parse_word_prototype_response,
    parse_word_selection_response,
};

const STUDENT_MAIN_URL: &str = "https://app.vocabgo.com/student/api/Student/Main";
const CLASS_TASK_PAGE_URL: &str = "https://app.vocabgo.com/student/api/Student/ClassTask/PageTask";
const STUDY_TASK_LIST_URL: &str = "https://app.vocabgo.com/student/api/Student/StudyTask/List";
const ASSESSMENT_BASE_URL: &str = "https://app.vocabgo.com/student/api/Student";
const STUDENT_REFERER: &str = "https://app.vocabgo.com/student/";
const STUDENT_ORIGIN: &str = "https://app.vocabgo.com";
const DONOR_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 8.1.2; LIO-AN00 Build/LIO-AN00; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/92.0.4515.131 Safari/537.36 MMWEBID/4462 MicroMessenger/8.0.20.2100(0x28001438) Process/toolsmp WeChat/arm64 Weixin Android Tablet NetType/WIFI Language/zh_CN ABI/arm64";
const ABC_HEADER_VALUE: &str = "60cd4becac3c293c3107d9a8087e7f47";
const AUTHORIZATION_V_READ: &str = "cfcd208495d565ef66e7dff9f98764da";
const AUTHORIZATION_V_SUBMIT: &str = "c4ca4238a0b923820dcc509a6f75849b";
const REQUEST_VERSION: &str = "2.6.1.231204";
const SIGNING_VERSION: &str = "2.6.1.240122";
const SIGNING_SUFFIX: &str = "ajfajfamsnfaflfasakljdlalkflak";
const PAGE_SIZE: u32 = 10;
const MAX_TASKS: usize = 10_000;
const MAX_AUTH_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_PAGE_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_STUDY_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ASSESSMENT_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ANSWER_EVIDENCE_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;

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
        self.fetch_course_study_document(session, &course_id).await
    }

    async fn fetch_course_study_document(
        &self,
        session: &CidarenTokenSession,
        course_id: &str,
    ) -> ProviderResult<CidarenStudyTaskDocument> {
        let timestamp = current_timestamp_millis()?;
        let response = study_task_request(&self.client, session, course_id, timestamp)?
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

#[async_trait]
impl CidarenAssessmentTransport for NativeCidarenTransport {
    async fn start_answer(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let session = self.sessions.resolve_session(context).await?;
        let request = build_start_answer_request(binding, current_timestamp_millis()?);
        let request = native_start_answer_request(&self.client, &session, &request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn verify_answer(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        topic_code: &str,
        answer: &CidarenWireAnswer,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let session = self.sessions.resolve_session(context).await?;
        let request =
            build_verify_answer_request(binding, topic_code, answer, current_timestamp_millis()?)?;
        let request = native_mutation_request(&self.client, &session, &request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn submit_answer_and_save(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        topic_code: &str,
        time_spent_millis: u64,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let session = self.sessions.resolve_session(context).await?;
        let request = build_submit_answer_and_save_request(
            binding,
            topic_code,
            time_spent_millis,
            current_timestamp_millis()?,
        )?;
        let request = native_mutation_request(&self.client, &session, &request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn skip_answer(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        topic_code: &str,
        time_spent_millis: u64,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let session = self.sessions.resolve_session(context).await?;
        let request = build_skip_answer_request(
            binding,
            topic_code,
            time_spent_millis,
            current_timestamp_millis()?,
        )?;
        let request = native_mutation_request(&self.client, &session, &request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn submit_chose_word(
        &self,
        context: &ProviderContext,
        binding: &CidarenAssessmentBinding,
        plan: &CidarenWordSelectionPlan,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let session = self.sessions.resolve_session(context).await?;
        let request =
            build_submit_chose_word_request(binding, plan.word_map(), current_timestamp_millis()?)?;
        let request = native_mutation_request(&self.client, &session, &request)?;
        self.send_word_selection_request(request).await
    }
}

#[async_trait]
impl CidarenAnswerEvidenceTransport for NativeCidarenTransport {
    async fn bind_answer_evidence(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        detail: &asterism_provider_api::RemoteTaskDetail,
    ) -> ProviderResult<CidarenAnswerEvidenceBinding> {
        let course_id = fresh_course_id(remote_task_id, detail)?;
        let session = self.sessions.resolve_session(context).await?;
        let units = self
            .fetch_course_study_document(&session, &course_id)
            .await?;
        CidarenAnswerEvidenceBinding::from_fresh_detail(remote_task_id, detail, &units)
    }

    async fn fetch_word_inventory(
        &self,
        context: &ProviderContext,
        binding: &CidarenAnswerEvidenceBinding,
    ) -> ProviderResult<CidarenWordInventory> {
        let session = self.sessions.resolve_session(context).await?;
        let request = build_word_inventory_request(binding, current_timestamp_millis()?);
        match request {
            CidarenWordInventoryRequest::StudyTaskInfo { .. } => {
                let response = native_word_inventory_request(&self.client, &session, &request)?
                    .send()
                    .await
                    .map_err(|error| {
                        classify_reqwest_error(&error, ResponseRoute::WordInventory)
                    })?;
                let mut document = read_json_response(
                    response,
                    ResponseRoute::WordInventory,
                    MAX_ANSWER_EVIDENCE_RESPONSE_BYTES,
                )
                .await?;
                let parsed = parse_study_task_info_response(
                    document.as_bytes(),
                    binding,
                    session.crypto_context(),
                );
                document.zeroize();
                parsed
            }
            CidarenWordInventoryRequest::CoursePage { .. } => {
                let response = native_word_inventory_request(&self.client, &session, &request)?
                    .send()
                    .await
                    .map_err(|error| classify_reqwest_error(&error, ResponseRoute::CoursePage))?;
                let mut document = read_json_response(
                    response,
                    ResponseRoute::CoursePage,
                    MAX_ANSWER_EVIDENCE_RESPONSE_BYTES,
                )
                .await?;
                let parsed = parse_course_page_response(document.as_bytes(), binding);
                document.zeroize();
                parsed
            }
        }
    }

    async fn fetch_word_evidence(
        &self,
        context: &ProviderContext,
        lookup: &CidarenWordLookup,
    ) -> ProviderResult<CidarenWordEvidence> {
        let session = self.sessions.resolve_session(context).await?;
        let request = build_word_info_request(lookup, current_timestamp_millis()?);
        let request = native_word_info_request(&self.client, &session, &request)?;
        let response = request
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::WordInformation))?;
        let mut document = read_json_response(
            response,
            ResponseRoute::WordInformation,
            MAX_ANSWER_EVIDENCE_RESPONSE_BYTES,
        )
        .await?;
        let parsed =
            parse_word_info_response(document.as_bytes(), lookup, session.crypto_context());
        document.zeroize();
        parsed
    }

    async fn resolve_word_prototype(
        &self,
        context: &ProviderContext,
        word: &str,
    ) -> ProviderResult<Option<String>> {
        let session = self.sessions.resolve_session(context).await?;
        let request = build_word_prototype_request(word, current_timestamp_millis()?)?;
        let request = native_word_prototype_request(&self.client, &session, &request)?;
        let response = request
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::WordPrototype))?;
        let mut document = read_json_response(
            response,
            ResponseRoute::WordPrototype,
            MAX_ANSWER_EVIDENCE_RESPONSE_BYTES,
        )
        .await?;
        let parsed = parse_word_prototype_response(document.as_bytes());
        document.zeroize();
        parsed
    }
}

impl NativeCidarenTransport {
    async fn send_assessment_request(
        &self,
        session: &CidarenTokenSession,
        request: RequestBuilder,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let response = request
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::Assessment))?;
        let mut document = read_json_response(
            response,
            ResponseRoute::Assessment,
            MAX_ASSESSMENT_RESPONSE_BYTES,
        )
        .await?;
        let parsed = parse_assessment_response(document.as_bytes(), session.crypto_context());
        document.zeroize();
        parsed
    }

    async fn send_word_selection_request(
        &self,
        request: RequestBuilder,
    ) -> ProviderResult<CidarenAssessmentResponse> {
        let response = request
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::Assessment))?;
        let mut document = read_json_response(
            response,
            ResponseRoute::Assessment,
            MAX_ASSESSMENT_RESPONSE_BYTES,
        )
        .await?;
        let parsed = parse_word_selection_response(document.as_bytes());
        document.zeroize();
        parsed
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
    authenticated_request_with_authorization(request, session, AUTHORIZATION_V_READ)
}

fn authenticated_request_with_authorization(
    request: RequestBuilder,
    session: &CidarenTokenSession,
    authorization: &'static str,
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
        .header(AUTHORIZATION_V, authorization)
        .header(X_REQUESTED_WITH, "XMLHttpRequest")
        .header(USER_AGENT, DONOR_USER_AGENT)
        .header(ACCEPT_LANGUAGE, "*")
        .header(REFERER, STUDENT_REFERER)
        .header(USER_TOKEN, token))
}

fn native_start_answer_request(
    client: &Client,
    session: &CidarenTokenSession,
    request: &CidarenStartAnswerRequest,
) -> ProviderResult<RequestBuilder> {
    let url = assessment_url(&request.path)?;
    authenticated_request(client.get(url).query(&request.query), session)
}

fn native_mutation_request(
    client: &Client,
    session: &CidarenTokenSession,
    request: &CidarenMutationRequest,
) -> ProviderResult<RequestBuilder> {
    let authorization = match request.authorization() {
        CidarenMutationAuthorization::Read => AUTHORIZATION_V_READ,
        CidarenMutationAuthorization::Submit => AUTHORIZATION_V_SUBMIT,
    };
    authenticated_request_with_authorization(
        client
            .post(assessment_url(request.path())?)
            .header("origin", STUDENT_ORIGIN)
            .header(CONTENT_TYPE, "application/json")
            .body(request.body_bytes().to_vec()),
        session,
        authorization,
    )
}

fn native_word_inventory_request(
    client: &Client,
    session: &CidarenTokenSession,
    request: &CidarenWordInventoryRequest,
) -> ProviderResult<RequestBuilder> {
    match request {
        CidarenWordInventoryRequest::StudyTaskInfo { path, query } if *path == "StudyTask/Info" => {
            authenticated_request(
                client
                    .get(format!("{ASSESSMENT_BASE_URL}/{path}"))
                    .query(query),
                session,
            )
        }
        CidarenWordInventoryRequest::CoursePage { url }
            if url.starts_with("https://resource.vocabgo.com/Resource/CoursePage/")
                && std::path::Path::new(url)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json")) =>
        {
            Ok(client
                .get(url)
                .header(ACCEPT, "application/json, text/plain, */*")
                .header(USER_AGENT, DONOR_USER_AGENT)
                .header(ACCEPT_LANGUAGE, "*")
                .header(REFERER, STUDENT_ORIGIN)
                .header("origin", STUDENT_ORIGIN)
                .header(X_REQUESTED_WITH, "com.tencent.mm"))
        }
        _ => Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren word-inventory request route is not audited",
        )),
    }
}

fn native_word_info_request(
    client: &Client,
    session: &CidarenTokenSession,
    request: &CidarenWordInfoRequest,
) -> ProviderResult<RequestBuilder> {
    if request.path != "Course/StudyWordInfo" {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren word-info request route is not audited",
        ));
    }
    authenticated_request(
        client
            .get(format!("{ASSESSMENT_BASE_URL}/{}", request.path))
            .query(&request.query),
        session,
    )
}

fn native_word_prototype_request(
    client: &Client,
    session: &CidarenTokenSession,
    request: &CidarenWordPrototypeRequest,
) -> ProviderResult<RequestBuilder> {
    if request.path != "Course/SearchWord" {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren word-prototype request route is not audited",
        ));
    }
    authenticated_request(
        client
            .get(format!("{ASSESSMENT_BASE_URL}/{}", request.path))
            .query(&request.query),
        session,
    )
}

fn assessment_url(path: &str) -> ProviderResult<String> {
    let allowed = ["ClassTask", "StudyTask"].into_iter().any(|family| {
        [
            "StartAnswer",
            "VerifyAnswer",
            "SubmitAnswerAndSave",
            "SkipAnswer",
            "SubmitChoseWord",
        ]
        .into_iter()
        .any(|operation| path == format!("{family}/{operation}"))
    });
    if !allowed {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren assessment request path is not audited",
        ));
    }
    Ok(format!("{ASSESSMENT_BASE_URL}/{path}"))
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
    Assessment,
    WordInventory,
    CoursePage,
    WordInformation,
    WordPrototype,
}

impl ResponseRoute {
    const fn label(self) -> &'static str {
        match self {
            Self::AccountValidation => "account-validation",
            Self::ClassTaskPage => "class-task",
            Self::StudyTaskList => "study-task",
            Self::Assessment => "assessment",
            Self::WordInventory => "word-inventory",
            Self::CoursePage => "course-page",
            Self::WordInformation => "word-information",
            Self::WordPrototype => "word-prototype",
        }
    }
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AssessmentClass, RemoteState, SourceType};
    use asterism_networking::NetworkProfile;
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::Value;

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
    fn assessment_requests_preserve_route_specific_authorization_and_body() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let client = build_http_client(&network).unwrap();
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();
        let binding = assessment_binding();

        let start = build_start_answer_request(&binding, 1_730_000_000_000);
        let start = native_start_answer_request(&client, &session, &start)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            start.url().path(),
            "/student/api/Student/ClassTask/StartAnswer"
        );
        assert_eq!(
            start.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );

        let verify = build_verify_answer_request(
            &binding,
            "synthetic-topic",
            &CidarenWireAnswer::from_option_id("n:2").unwrap(),
            1_730_000_000_000,
        )
        .unwrap();
        let verify = native_mutation_request(&client, &session, &verify)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            verify.url().path(),
            "/student/api/Student/ClassTask/VerifyAnswer"
        );
        assert_eq!(
            verify.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_SUBMIT
        );
        assert_eq!(verify.headers().get("origin").unwrap(), STUDENT_ORIGIN);
        assert_eq!(
            verify.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = verify.body().and_then(reqwest::Body::as_bytes).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(body).unwrap()["answer"], 2);

        let chose = build_submit_chose_word_request(
            &binding,
            &serde_json::json!(["alpha", "beta"]),
            1_730_000_000_000,
        )
        .unwrap();
        let chose = native_mutation_request(&client, &session, &chose)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            chose.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );
    }

    #[test]
    fn answer_evidence_requests_bind_routes_and_never_leak_to_resource_host() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let client = build_http_client(&network).unwrap();
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();

        let ordinary = answer_evidence_binding("Synthetic List 02");
        let request = build_word_inventory_request(&ordinary, 1_730_000_000_000);
        let request = native_word_inventory_request(&client, &session, &request)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().path(), "/student/api/Student/StudyTask/Info");
        assert!(request.headers().contains_key(USER_TOKEN));
        assert_eq!(
            request.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );

        let inventory = parse_study_task_info_response(
            include_str!("../../../fixtures/providers/cidaren/answers/study-task-info.json")
                .as_bytes(),
            &ordinary,
            None,
        )
        .unwrap();
        let lookup = inventory.lookup("alpha").unwrap();
        let word_info = build_word_info_request(&lookup, 1_730_000_000_000);
        let word_info = native_word_info_request(&client, &session, &word_info)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            word_info.url().path(),
            "/student/api/Student/Course/StudyWordInfo"
        );
        assert!(word_info.headers().contains_key(USER_TOKEN));

        let prototype = build_word_prototype_request("packed", 1_730_000_000_000).unwrap();
        let prototype = native_word_prototype_request(&client, &session, &prototype)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            prototype.url().path(),
            "/student/api/Student/Course/SearchWord"
        );
        assert!(prototype.headers().contains_key(USER_TOKEN));
        assert_eq!(
            prototype.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );

        let self_built = answer_evidence_binding("Self Built Test");
        let request = build_word_inventory_request(&self_built, 1_730_000_000_000);
        let request = native_word_inventory_request(&client, &session, &request)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().host_str(), Some("resource.vocabgo.com"));
        assert_eq!(request.url().path(), "/Resource/CoursePage/course-a.json");
        assert!(!request.headers().contains_key(USER_TOKEN));
        assert!(!request.headers().contains_key(AUTHORIZATION_V));
        assert!(!request.headers().contains_key(ABC));
        assert_eq!(
            request.headers().get(X_REQUESTED_WITH).unwrap(),
            "com.tencent.mm"
        );
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

    fn assessment_binding() -> CidarenAssessmentBinding {
        let normalized = serde_json::json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": -1,
            "course_id": "course-a",
            "task_type": "test",
            "progress": 35,
        });
        CidarenAssessmentBinding::from_fresh_detail(
            "class-task:2002",
            &RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: "class-task:2002".to_owned(),
                    course_remote_id: Some("course:course-a".to_owned()),
                    title: "Synthetic Task".to_owned(),
                    source_type: SourceType::Exam,
                    assessment_class: AssessmentClass::Routine,
                    remote_state: RemoteState::InProgress,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: Vec::new(),
                    fingerprint: "synthetic-fingerprint".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: Value::Object(serde_json::Map::new()),
                },
                normalized_detail: serde_json::json!({
                    "schema": "cidaren.class-task.detail.v1",
                    "release_id": "2002",
                    "task": normalized,
                }),
            },
        )
        .unwrap()
    }

    fn answer_evidence_binding(title: &str) -> CidarenAnswerEvidenceBinding {
        let normalized = serde_json::json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": -1,
            "course_id": "course-a",
            "task_type": "test",
            "progress": 35,
        });
        let detail = RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "class-task:2002".to_owned(),
                course_remote_id: Some("course:course-a".to_owned()),
                title: title.to_owned(),
                source_type: SourceType::Exam,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::InProgress,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "synthetic-fingerprint".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: Value::Object(serde_json::Map::new()),
            },
            normalized_detail: serde_json::json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": "2002",
                "task": normalized,
            }),
        };
        let units = CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
        .unwrap();
        CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &detail, &units).unwrap()
    }
}
