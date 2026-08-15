use std::{fmt, sync::Arc};

use asterism_domain::{ProtocolObservationKind, ProtocolSurface};
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    CredentialReplacement, ExternalOauthCallbackBinding, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderResult,
};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::{
    Client, RequestBuilder, Response, StatusCode,
    header::{
        ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, REFERER,
        RETRY_AFTER, USER_AGENT,
    },
};
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::oauth_authorization::{CidarenOauthCallbackBinding, parse_oauth_callback};
use crate::oauth_exchange::{
    CidarenOauthBootstrap, CidarenOauthClientContext, CidarenOauthCode, CidarenOauthLoginMaterial,
    CidarenOauthLoginRequest, LOGIN_VERSION,
};
use crate::{
    CidarenAnswerEvidenceBinding, CidarenAnswerEvidenceTransport, CidarenAssessmentTransport,
    CidarenAssessmentTransportOutcome, CidarenAuthenticationTransport,
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, CidarenMutationRequest,
    CidarenSessionResolver, CidarenStartAnswerRequest, CidarenStudyTaskDocument,
    CidarenStudyTaskTransport, CidarenTaskScoreTransport, CidarenTokenSession, CidarenWordEvidence,
    CidarenWordInfoRequest, CidarenWordInventory, CidarenWordInventoryRequest, CidarenWordLookup,
    CidarenWordPrototypeRequest,
    answer_evidence_protocol::fresh_course_id,
    assessment_protocol::CidarenMutationAuthorization,
    authentication::selected_course_id,
    build_word_info_request, build_word_inventory_request, build_word_prototype_request,
    class_tasks::class_task_total,
    classify_token_validation_response, parse_assessment_response, parse_course_page_response,
    parse_study_task_info_response, parse_word_info_response, parse_word_prototype_response,
    parse_word_selection_response,
    protocol_observation::error_with_protocol_observation,
    score_read::{
        CidarenTaskScoreRequest, build_task_score_request, normalized_task_score,
        parse_task_score_response,
    },
};

const STUDENT_MAIN_URL: &str = "https://app.vocabgo.com/student/api/Student/Main";
const WECHAT_V2_LOGIN_URL: &str =
    "https://app.vocabgo.com/student/api/Auth/Wechat/V2/LoginByWechatCode";
const CLASS_TASK_PAGE_URL: &str = "https://app.vocabgo.com/student/api/Student/ClassTask/PageTask";
const STUDY_TASK_LIST_URL: &str = "https://app.vocabgo.com/student/api/Student/StudyTask/List";
const ASSESSMENT_BASE_URL: &str = "https://app.vocabgo.com/student/api/Student";
const STUDENT_REFERER: &str = "https://app.vocabgo.com/student/";
const STUDENT_ORIGIN: &str = "https://app.vocabgo.com";
const DONOR_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 8.1.2; LIO-AN00 Build/LIO-AN00; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/92.0.4515.131 Safari/537.36 MMWEBID/4462 MicroMessenger/8.0.20.2100(0x28001438) Process/toolsmp WeChat/arm64 Weixin Android Tablet NetType/WIFI Language/zh_CN ABI/arm64";
const OAUTH_BOOTSTRAP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
const ABC_HEADER_VALUE: &str = "60cd4becac3c293c3107d9a8087e7f47";
const AUTHORIZATION_V_READ: &str = "cfcd208495d565ef66e7dff9f98764da";
const AUTHORIZATION_V_SUBMIT: &str = "c4ca4238a0b923820dcc509a6f75849b";
const REQUEST_VERSION: &str = "2.6.1.231204";
const SIGNING_VERSION: &str = "2.6.1.240122";
const SIGNING_SUFFIX: &str = "ajfajfamsnfaflfasakljdlalkflak";
const PAGE_SIZE: u32 = 10;
const MAX_TASKS: usize = 10_000;
const MAX_AUTH_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_OAUTH_RESPONSE_BYTES: usize = 256 * 1_024;
const MAX_PAGE_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_STUDY_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ASSESSMENT_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ANSWER_EVIDENCE_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_TASK_SCORE_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;

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

    /// Validates one manually returned `WeChat` callback against the pending
    /// login's hash-only binding, executes the current native V2 exchange
    /// exactly once, authenticates
    /// its P-256/HKDF/AES-GCM response and performs a fresh `Student/Main`
    /// readback before returning persistable Composite material.
    ///
    /// Core must atomically claim the owner/AuthSession-bound pending login
    /// before this call and must not replay it after an ambiguous network
    /// failure. `client_context` is short-lived browser request context, not a
    /// stored account credential.
    ///
    /// # Errors
    ///
    /// Returns typed Authentication, Network, `InvalidResponse` or
    /// `ProtocolDrift` errors. It never retries the single-use mutation.
    pub(crate) async fn exchange_wechat_oauth_callback(
        &self,
        callback_url: impl Into<String>,
        callback_binding: &CidarenOauthCallbackBinding,
        client_context: &CidarenOauthClientContext,
    ) -> ProviderResult<CidarenOauthLoginMaterial> {
        let code = parse_oauth_callback(callback_url, callback_binding)?;
        self.exchange_wechat_oauth_code(code, client_context).await
    }

    async fn exchange_wechat_oauth_code(
        &self,
        code: CidarenOauthCode,
        client_context: &CidarenOauthClientContext,
    ) -> ProviderResult<CidarenOauthLoginMaterial> {
        let bootstrap = CidarenOauthBootstrap::generate()?;
        let request = bootstrap.build_request(code, current_timestamp_millis()?)?;
        let response = native_oauth_request(&self.client, &request, client_context)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::OauthLogin))?;
        let document = read_json_response(
            response,
            ResponseRoute::OauthLogin,
            MAX_OAUTH_RESPONSE_BYTES,
        )
        .await?;
        let material = bootstrap.complete(document.into_bytes())?;
        let session = material.token_session()?;
        self.validate_oauth_session(&session, client_context)
            .await?;
        Ok(material)
    }

    async fn validate_oauth_session(
        &self,
        session: &CidarenTokenSession,
        client_context: &CidarenOauthClientContext,
    ) -> ProviderResult<()> {
        let mut account = self
            .fetch_oauth_account_document(session, client_context)
            .await?;
        let validation = classify_token_validation_response(account.as_bytes());
        account.zeroize();
        validation
    }

    async fn fetch_oauth_account_document(
        &self,
        session: &CidarenTokenSession,
        client_context: &CidarenOauthClientContext,
    ) -> ProviderResult<String> {
        let timestamp = current_timestamp_millis()?;
        let request = self.client.get(STUDENT_MAIN_URL).query(&[
            ("timestamp", timestamp.to_string()),
            ("version", LOGIN_VERSION.to_owned()),
            ("app_type", "1".to_owned()),
        ]);
        let response = oauth_authenticated_request(request, session, client_context)?
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

fn native_oauth_request(
    client: &Client,
    request: &CidarenOauthLoginRequest,
    client_context: &CidarenOauthClientContext,
) -> ProviderResult<RequestBuilder> {
    let body = request.body_bytes()?;
    let mut empty_user_token = HeaderValue::from_static("");
    empty_user_token.set_sensitive(true);
    Ok(client
        .post(WECHAT_V2_LOGIN_URL)
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(ABC, client_context.abc_header()?)
        .header(AUTHORIZATION_V, client_context.authorization_header()?)
        .header(USER_TOKEN, empty_user_token)
        .header(X_REQUESTED_WITH, "XMLHttpRequest")
        .header(USER_AGENT, client_context.user_agent_header()?)
        .header(ACCEPT_LANGUAGE, "*")
        .header(REFERER, STUDENT_REFERER)
        .header("origin", STUDENT_ORIGIN)
        .header(CONTENT_TYPE, "application/json;charset=UTF-8")
        .body(body.as_slice().to_vec()))
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

    async fn validate_native_oauth_session(
        &self,
        session: &CidarenTokenSession,
    ) -> ProviderResult<()> {
        let client_context =
            CidarenOauthClientContext::try_for_oauth_bootstrap(OAUTH_BOOTSTRAP_USER_AGENT)?;
        self.validate_oauth_session(session, &client_context).await
    }

    async fn exchange_external_oauth_callback(
        &self,
        callback_url: SecretString,
        binding: ExternalOauthCallbackBinding,
    ) -> ProviderResult<CredentialReplacement> {
        let callback_binding = CidarenOauthCallbackBinding::from_external(binding)?;
        let client_context =
            CidarenOauthClientContext::try_for_oauth_bootstrap(OAUTH_BOOTSTRAP_USER_AGENT)?;
        self.exchange_wechat_oauth_callback(
            callback_url.expose_secret().to_owned(),
            &callback_binding,
            &client_context,
        )
        .await
        .map(CidarenOauthLoginMaterial::into_credential_replacement)
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
        request: &CidarenStartAnswerRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
        let session = self.sessions.resolve_session(context).await?;
        let request = native_start_answer_request(&self.client, &session, request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn verify_answer(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
        let session = self.sessions.resolve_session(context).await?;
        let request = native_mutation_request(&self.client, &session, request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn submit_answer_and_save(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
        let session = self.sessions.resolve_session(context).await?;
        let request = native_mutation_request(&self.client, &session, request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn skip_answer(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
        let session = self.sessions.resolve_session(context).await?;
        let request = native_mutation_request(&self.client, &session, request)?;
        self.send_assessment_request(&session, request).await
    }

    async fn submit_chose_word(
        &self,
        context: &ProviderContext,
        request: &CidarenMutationRequest,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
        let session = self.sessions.resolve_session(context).await?;
        let request = native_mutation_request(&self.client, &session, request)?;
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

#[async_trait]
impl CidarenTaskScoreTransport for NativeCidarenTransport {
    async fn fetch_task_score(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        detail: &asterism_provider_api::RemoteTaskDetail,
    ) -> ProviderResult<Option<asterism_domain::SubmissionScore>> {
        let normalized = normalized_task_score(detail)?;
        let Some(request) =
            build_task_score_request(remote_task_id, detail, current_timestamp_millis()?)?
        else {
            return Ok(normalized);
        };
        let session = self.sessions.resolve_session(context).await?;
        let response = native_task_score_request(&self.client, &session, &request)?
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error, ResponseRoute::TaskScore))?;
        let mut document = read_json_response(
            response,
            ResponseRoute::TaskScore,
            MAX_TASK_SCORE_RESPONSE_BYTES,
        )
        .await?;
        let parsed = parse_task_score_response(document.as_bytes(), session.crypto_context());
        document.zeroize();
        parsed.map(|score| score.or(normalized))
    }
}

impl NativeCidarenTransport {
    async fn send_assessment_request(
        &self,
        session: &CidarenTokenSession,
        request: RequestBuilder,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
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
        let response_digest = Sha256::digest(document.as_bytes()).into();
        let received_at = Utc::now();
        let parsed = parse_assessment_response(document.as_bytes(), session.crypto_context());
        document.zeroize();
        CidarenAssessmentTransportOutcome::try_new(parsed?, response_digest, received_at)
    }

    async fn send_word_selection_request(
        &self,
        request: RequestBuilder,
    ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
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
        let response_digest = Sha256::digest(document.as_bytes()).into();
        let received_at = Utc::now();
        let parsed = parse_word_selection_response(document.as_bytes());
        document.zeroize();
        CidarenAssessmentTransportOutcome::try_new(parsed?, response_digest, received_at)
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
    let token = token_header(session)?;
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

fn oauth_authenticated_request(
    request: RequestBuilder,
    session: &CidarenTokenSession,
    client_context: &CidarenOauthClientContext,
) -> ProviderResult<RequestBuilder> {
    Ok(request
        .header(ACCEPT, "application/json, text/plain, */*")
        .header(ABC, client_context.abc_header()?)
        .header(AUTHORIZATION_V, client_context.authorization_header()?)
        .header(X_REQUESTED_WITH, "XMLHttpRequest")
        .header(USER_AGENT, client_context.user_agent_header()?)
        .header(ACCEPT_LANGUAGE, "*")
        .header(REFERER, STUDENT_REFERER)
        .header(CONTENT_TYPE, "application/json;charset=UTF-8")
        .header(USER_TOKEN, token_header(session)?))
}

fn token_header(session: &CidarenTokenSession) -> ProviderResult<HeaderValue> {
    let mut token = HeaderValue::from_str(session.expose_token()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren stored token cannot be encoded as a request header",
        )
    })?;
    token.set_sensitive(true);
    Ok(token)
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

fn native_task_score_request(
    client: &Client,
    session: &CidarenTokenSession,
    request: &CidarenTaskScoreRequest,
) -> ProviderResult<RequestBuilder> {
    if !matches!(request.path, "ClassTask/Info" | "StudyTask/Info") {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren task-score request route is not audited",
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
        return Err(response_body_observation(
            oversized_response(route),
            route,
            "declared_oversized",
            response.content_length(),
            maximum,
        ));
    }
    let mut document = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                document.zeroize();
                return Err(classify_reqwest_error(&error, route));
            }
        };
        let observed_length = document.len().saturating_add(chunk.len());
        if observed_length > maximum {
            document.zeroize();
            return Err(response_body_observation(
                oversized_response(route),
                route,
                "streamed_oversized",
                u64::try_from(observed_length).ok(),
                maximum,
            ));
        }
        document.extend_from_slice(&chunk);
    }
    if document.is_empty() {
        return Err(response_body_observation(
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!(
                    "Cidaren {} endpoint returned an empty response",
                    route.label()
                ),
            ),
            route,
            "empty",
            Some(0),
            maximum,
        ));
    }
    let mut document = String::from_utf8(document).map_err(|error| {
        let mut bytes = error.into_bytes();
        let observed_length = u64::try_from(bytes.len()).ok();
        bytes.zeroize();
        response_body_observation(
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!("Cidaren {} endpoint returned invalid UTF-8", route.label()),
            ),
            route,
            "invalid_utf8",
            observed_length,
            maximum,
        )
    })?;
    let mut deserializer = serde_json::Deserializer::from_str(&document);
    let valid_json = IgnoredAny::deserialize(&mut deserializer)
        .and_then(|_| deserializer.end())
        .is_ok();
    if !valid_json {
        let observed_length = u64::try_from(document.len()).ok();
        document.zeroize();
        return Err(response_body_observation(
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!("Cidaren {} endpoint returned invalid JSON", route.label()),
            ),
            route,
            "invalid_json",
            observed_length,
            maximum,
        ));
    }
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
        return Err(http_status_observation(
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                format!(
                    "Cidaren {} route changed or redirected unexpectedly",
                    route.label()
                ),
            ),
            route,
            status,
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
        return Err(http_status_observation(
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!(
                    "Cidaren {} endpoint returned an unexpected status",
                    route.label()
                ),
            ),
            route,
            status,
        ));
    }
    Ok(())
}

fn validate_json_content_type(headers: &HeaderMap, route: ResponseRoute) -> ProviderResult<()> {
    let header = headers.get(CONTENT_TYPE);
    let content_type = header.and_then(|value| value.to_str().ok());
    let Some(content_type) = content_type else {
        return Err(http_content_type_observation(
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!(
                    "Cidaren {} endpoint returned no valid Content-Type",
                    route.label()
                ),
            ),
            route,
            header,
            None,
        ));
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("application/json")
        && !media_type.to_ascii_lowercase().ends_with("+json")
    {
        return Err(http_content_type_observation(
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                format!(
                    "Cidaren {} endpoint returned an unexpected content type",
                    route.label()
                ),
            ),
            route,
            header,
            Some(content_type),
        ));
    }
    Ok(())
}

fn http_status_observation(
    error: ProviderError,
    route: ResponseRoute,
    status: StatusCode,
) -> ProviderError {
    error_with_protocol_observation(
        error,
        ProtocolSurface::Other,
        ProtocolObservationKind::UnknownResultShape,
        json!({
            "schema": "cidaren.http-response-head-observation.v1",
            "stage": "status",
            "route": route.label(),
            "status": status.as_u16(),
        }),
    )
}

fn http_content_type_observation(
    error: ProviderError,
    route: ResponseRoute,
    header: Option<&HeaderValue>,
    content_type: Option<&str>,
) -> ProviderError {
    let media_type = content_type.map(|value| value.split(';').next().unwrap_or_default().trim());
    error_with_protocol_observation(
        error,
        ProtocolSurface::Other,
        ProtocolObservationKind::UnknownResultShape,
        json!({
            "schema": "cidaren.http-response-head-observation.v1",
            "stage": "content_type",
            "route": route.label(),
            "header_present": header.is_some(),
            "header_utf8": header.is_some_and(|value| value.to_str().is_ok()),
            "media_type_ascii": media_type.map(str::is_ascii),
            "media_type_length": media_type.map(str::len),
            "parameter_count": content_type.map(|value| value.split(';').count().saturating_sub(1)),
            "json_suffix": media_type.map(|value| {
                value.eq_ignore_ascii_case("application/json")
                    || value.to_ascii_lowercase().ends_with("+json")
            }),
        }),
    )
}

fn response_body_observation(
    error: ProviderError,
    route: ResponseRoute,
    state: &'static str,
    observed_length: Option<u64>,
    maximum: usize,
) -> ProviderError {
    error_with_protocol_observation(
        error,
        ProtocolSurface::Other,
        ProtocolObservationKind::UnknownResultShape,
        json!({
            "schema": "cidaren.http-response-body-observation.v1",
            "route": route.label(),
            "state": state,
            "observed_length": observed_length,
            "maximum": maximum,
        }),
    )
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
    OauthLogin,
    AccountValidation,
    ClassTaskPage,
    StudyTaskList,
    Assessment,
    WordInventory,
    CoursePage,
    WordInformation,
    WordPrototype,
    TaskScore,
}

impl ResponseRoute {
    const fn label(self) -> &'static str {
        match self {
            Self::OauthLogin => "oauth-login",
            Self::AccountValidation => "account-validation",
            Self::ClassTaskPage => "class-task",
            Self::StudyTaskList => "study-task",
            Self::Assessment => "assessment",
            Self::WordInventory => "word-inventory",
            Self::CoursePage => "course-page",
            Self::WordInformation => "word-information",
            Self::WordPrototype => "word-prototype",
            Self::TaskScore => "task-score",
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
    use crate::{
        CidarenAssessmentBinding, CidarenWireAnswer, build_skip_answer_request,
        build_start_answer_request, build_submit_answer_and_save_request,
        build_submit_chose_word_request, build_verify_answer_request,
    };

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
    fn oauth_request_is_single_route_bootstrap_bound_and_has_no_stale_token() {
        let bootstrap = CidarenOauthBootstrap::generate().unwrap();
        let request = bootstrap
            .build_request(
                CidarenOauthCode::try_new("synthetic-oauth-code").unwrap(),
                1_786_444_968_188,
            )
            .unwrap();
        let client_context = CidarenOauthClientContext::try_for_oauth_bootstrap(
            "Mozilla/5.0 synthetic-cidaren-client",
        )
        .unwrap();
        let request = native_oauth_request(&Client::new(), &request, &client_context)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://app.vocabgo.com/student/api/Auth/Wechat/V2/LoginByWechatCode"
        );
        let bootstrap_token = request.headers().get(USER_TOKEN).unwrap();
        assert!(bootstrap_token.is_sensitive());
        assert!(bootstrap_token.as_bytes().is_empty());
        assert!(
            request
                .headers()
                .get(AUTHORIZATION_V)
                .unwrap()
                .is_sensitive()
        );
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION_V)
                .unwrap()
                .to_str()
                .unwrap(),
            "00"
        );
        assert_eq!(
            request.headers().get(ABC).unwrap().to_str().unwrap(),
            format!(
                "{:x}",
                md5::compute(b"Mozilla/5.0 synthetic-cidaren-client")
            )
        );
        assert_eq!(
            request.headers().get(USER_AGENT).unwrap(),
            "Mozilla/5.0 synthetic-cidaren-client"
        );
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["code"], "synthetic-oauth-code");
        assert_eq!(body["version"], "2.7.0.260715_01");
        assert_eq!(body["app_type"], 1);
    }

    #[test]
    fn oauth_fresh_readback_keeps_current_bootstrap_context() {
        let client_context =
            CidarenOauthClientContext::try_for_oauth_bootstrap(OAUTH_BOOTSTRAP_USER_AGENT).unwrap();
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();
        let request = oauth_authenticated_request(
            Client::new().get(STUDENT_MAIN_URL).query(&[
                ("timestamp", "1786444968188"),
                ("version", LOGIN_VERSION),
                ("app_type", "1"),
            ]),
            &session,
            &client_context,
        )
        .unwrap()
        .build()
        .unwrap();
        assert_eq!(request.url().path(), "/student/api/Student/Main");
        assert_eq!(
            request
                .url()
                .query_pairs()
                .collect::<std::collections::BTreeMap<_, _>>()
                .get("version")
                .map(AsRef::as_ref),
            Some("2.7.0.260715_01")
        );
        assert_eq!(request.headers().get(AUTHORIZATION_V).unwrap(), "00");
        assert_eq!(
            request.headers().get(USER_AGENT).unwrap(),
            OAUTH_BOOTSTRAP_USER_AGENT
        );
        assert_eq!(
            request.headers().get(ABC).unwrap().to_str().unwrap(),
            format!("{:x}", md5::compute(OAUTH_BOOTSTRAP_USER_AGENT))
        );
        let token = request.headers().get(USER_TOKEN).unwrap();
        assert!(token.is_sensitive());
        assert_eq!(token, "synthetic-user-token");
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
        assert_class_assessment_requests(&client, &session);
        assert_study_assessment_requests(&client, &session);
        assert!(assessment_url("ClassTask/UnmappedMutation").is_err());
        assert!(assessment_url("ForeignTask/StartAnswer").is_err());
    }

    fn assert_class_assessment_requests(client: &Client, session: &CidarenTokenSession) {
        let binding = assessment_binding();

        let start = build_start_answer_request(&binding, 1_730_000_000_000);
        let start = native_start_answer_request(client, session, &start)
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
        let verify = native_mutation_request(client, session, &verify)
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

        for mutation in [
            build_submit_answer_and_save_request(
                &binding,
                "synthetic-topic",
                25_000,
                1_730_000_000_000,
            )
            .unwrap(),
            build_skip_answer_request(&binding, "synthetic-topic", 20_000, 1_730_000_000_000)
                .unwrap(),
        ] {
            let expected_operation = mutation.path().rsplit('/').next().unwrap().to_owned();
            let request = native_mutation_request(client, session, &mutation)
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(
                request.url().path(),
                format!("/student/api/Student/ClassTask/{expected_operation}")
            );
            assert_eq!(
                request.headers().get(AUTHORIZATION_V).unwrap(),
                AUTHORIZATION_V_SUBMIT
            );
        }
    }

    fn assert_study_assessment_requests(client: &Client, session: &CidarenTokenSession) {
        let study_binding = study_assessment_binding();
        let chose = build_submit_chose_word_request(
            &study_binding,
            &serde_json::json!(["alpha", "beta"]),
            1_730_000_000_000,
        )
        .unwrap();
        let chose = native_mutation_request(client, session, &chose)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            chose.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );
        assert_eq!(
            chose.url().path(),
            "/student/api/Student/StudyTask/SubmitChoseWord"
        );

        let study_start = build_start_answer_request(&study_binding, 1_730_000_000_000);
        let study_start = native_start_answer_request(client, session, &study_start)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            study_start.url().path(),
            "/student/api/Student/StudyTask/StartAnswer"
        );
        let query = study_start
            .url()
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            query.get("task_type").map(std::convert::AsRef::as_ref),
            Some("3")
        );
        assert_eq!(
            query.get("course_id").map(std::convert::AsRef::as_ref),
            Some("course-a")
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
    fn task_score_read_is_authenticated_and_route_allowlisted() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let client = build_http_client(&network).unwrap();
        let session = CidarenTokenSession::try_new("synthetic-user-token").unwrap();
        let request = CidarenTaskScoreRequest {
            path: "ClassTask/Info",
            query: vec![
                ("task_id", "812".to_owned()),
                ("release_id", "2002".to_owned()),
                ("timestamp", "1730000000000".to_owned()),
                ("version", "2.6.1.240122".to_owned()),
                ("app_type", "1".to_owned()),
            ],
        };
        let request = native_task_score_request(&client, &session, &request)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().path(), "/student/api/Student/ClassTask/Info");
        assert!(request.headers().contains_key(USER_TOKEN));
        assert_eq!(
            request.headers().get(AUTHORIZATION_V).unwrap(),
            AUTHORIZATION_V_READ
        );
        assert_eq!(request.url().query_pairs().count(), 5);

        let rejected = CidarenTaskScoreRequest {
            path: "ClassTask/SubmitAnswerAndSave",
            query: Vec::new(),
        };
        assert!(native_task_score_request(&client, &session, &rejected).is_err());
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
        assert!(limited.protocol_observation.is_none());
        let unauthorized = validate_status(
            StatusCode::UNAUTHORIZED,
            &HeaderMap::new(),
            ResponseRoute::AccountValidation,
        )
        .unwrap_err();
        assert_eq!(unauthorized.kind, ProviderErrorKind::Authentication);
        assert!(unauthorized.protocol_observation.is_none());

        let redirect = validate_status(
            StatusCode::FOUND,
            &HeaderMap::new(),
            ResponseRoute::ClassTaskPage,
        )
        .unwrap_err();
        assert_eq!(redirect.kind, ProviderErrorKind::ProtocolDrift);
        let observation = redirect.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::Other);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            observation.shape_sanitized,
            serde_json::json!({
                "schema": "cidaren.http-response-head-observation.v1",
                "stage": "status",
                "route": "class-task",
                "status": 302,
            })
        );

        let mut content = HeaderMap::new();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        validate_json_content_type(&content, ResponseRoute::ClassTaskPage).unwrap();
        content.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        let error =
            validate_json_content_type(&content, ResponseRoute::AccountValidation).unwrap_err();
        let observation = error.protocol_observation.unwrap();
        assert_eq!(
            observation.shape_sanitized,
            serde_json::json!({
                "schema": "cidaren.http-response-head-observation.v1",
                "stage": "content_type",
                "route": "account-validation",
                "header_present": true,
                "header_utf8": true,
                "media_type_ascii": true,
                "media_type_length": 9,
                "parameter_count": 0,
                "json_suffix": false,
            })
        );
        assert!(
            !serde_json::to_string(&observation.shape_sanitized)
                .unwrap()
                .contains("text/html")
        );

        let error =
            validate_json_content_type(&HeaderMap::new(), ResponseRoute::TaskScore).unwrap_err();
        assert_eq!(
            error.protocol_observation.unwrap().shape_sanitized,
            serde_json::json!({
                "schema": "cidaren.http-response-head-observation.v1",
                "stage": "content_type",
                "route": "task-score",
                "header_present": false,
                "header_utf8": false,
                "media_type_ascii": null,
                "media_type_length": null,
                "parameter_count": null,
                "json_suffix": null,
            })
        );
    }

    #[tokio::test]
    async fn response_body_framing_is_bounded_and_shape_only() {
        let declared = fixture_response(vec![1, 2, 3, 4], Some(4));
        let error = read_json_response(declared, ResponseRoute::TaskScore, 3)
            .await
            .unwrap_err();
        assert_eq!(
            error.protocol_observation.unwrap().shape_sanitized,
            serde_json::json!({
                "schema": "cidaren.http-response-body-observation.v1",
                "route": "task-score",
                "state": "declared_oversized",
                "observed_length": 4,
                "maximum": 3,
            })
        );

        let error = response_body_observation(
            oversized_response(ResponseRoute::WordInformation),
            ResponseRoute::WordInformation,
            "streamed_oversized",
            Some(4),
            3,
        );
        assert_eq!(
            error.protocol_observation.unwrap().shape_sanitized,
            serde_json::json!({
                "schema": "cidaren.http-response-body-observation.v1",
                "route": "word-information",
                "state": "streamed_oversized",
                "observed_length": 4,
                "maximum": 3,
            })
        );

        let empty = fixture_response(Vec::new(), None);
        let error = read_json_response(empty, ResponseRoute::AccountValidation, 16)
            .await
            .unwrap_err();
        assert_eq!(
            error.protocol_observation.unwrap().shape_sanitized["state"],
            "empty"
        );

        let invalid_utf8 = fixture_response(vec![0xff, 0xfe], None);
        let error = read_json_response(invalid_utf8, ResponseRoute::Assessment, 16)
            .await
            .unwrap_err();
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.shape_sanitized["state"], "invalid_utf8");
        assert_eq!(observation.shape_sanitized["observed_length"], 2);
        let sanitized = serde_json::to_string(&observation.shape_sanitized).unwrap();
        assert!(!sanitized.contains("255"));
        assert!(!sanitized.contains("254"));

        let invalid_json = fixture_response(b"must-not-cross-json".to_vec(), None);
        let error = read_json_response(invalid_json, ResponseRoute::WordPrototype, 64)
            .await
            .unwrap_err();
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.shape_sanitized["state"], "invalid_json");
        assert_eq!(observation.shape_sanitized["observed_length"], 19);
        let sanitized = serde_json::to_string(&observation.shape_sanitized).unwrap();
        assert!(!sanitized.contains("must-not-cross"));

        let valid = fixture_response(br#"{"ok":true}"#.to_vec(), None);
        assert_eq!(
            read_json_response(valid, ResponseRoute::StudyTaskList, 16)
                .await
                .unwrap(),
            r#"{"ok":true}"#
        );
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

    fn fixture_response(body: Vec<u8>, content_length: Option<usize>) -> Response {
        let mut builder = http::Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json");
        if let Some(content_length) = content_length {
            builder = builder.header("content-length", content_length);
        }
        Response::from(builder.body(reqwest::Body::from(body)).unwrap())
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

    fn study_assessment_binding() -> CidarenAssessmentBinding {
        let normalized = serde_json::json!({
            "schema": "cidaren.study-task.v1",
            "task_id": 71002,
            "course_id": "course-a",
            "list_id": "list-a",
            "task_type": "study",
            "progress": 35,
        });
        CidarenAssessmentBinding::from_fresh_detail(
            "study-task:course-a:list-a",
            &RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: "study-task:course-a:list-a".to_owned(),
                    course_remote_id: Some("course:course-a".to_owned()),
                    title: "Synthetic Study Task".to_owned(),
                    source_type: SourceType::Practice,
                    assessment_class: AssessmentClass::Routine,
                    remote_state: RemoteState::InProgress,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: Vec::new(),
                    fingerprint: "synthetic-study-fingerprint".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: Value::Object(serde_json::Map::new()),
                },
                normalized_detail: serde_json::json!({
                    "schema": "cidaren.study-task.detail.v1",
                    "course_id": "course-a",
                    "list_id": "list-a",
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
