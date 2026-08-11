use std::{fmt, sync::Arc};

use asterism_domain::SubmissionReceipt;
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
    UaiAnswerDocument, UaiAnswerDocuments, UaiAnswerTransport, UaiCourseInventoryTransport,
    UaiDurationDocument, UaiDurationTransport, UaiInventoryDocument, UaiJwtSession,
    UaiProgressDocument, UaiProgressTransport, UaiQuestionDocument, UaiQuestionTransport,
    UaiSessionResolver, UaiSubmissionPlan, UaiSubmissionResponseDocument, UaiSubmissionTransport,
    UaiTaskInventoryDocuments, UaiTaskInventoryTransport, UaiVerificationDocument,
    UaiVerificationTransport,
    annotator::generate_annotator_token,
    course_inventory::{
        course_resource_id_from_remote, parse_course_context_for_resource_id,
        required_remote_component,
    },
    parse_course_context, parse_submission_receipt,
    submission_verify::validate_verification_course_binding,
    user_identity::parse_user_identity,
};

const COURSE_LIST_URL: &str = "https://uai.unipus.cn/api/cmgt/course/getCourseListByStudent";
const USER_INFO_URL: &str = "https://uai.unipus.cn/api/account/user/info";
const UAI_ORIGIN: &str = "https://uai.unipus.cn";
const UCONTENT_ORIGIN: &str = "https://ucontent.unipus.cn";
const MAX_INVENTORY_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_PROGRESS_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_USER_INFO_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_DURATION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_QUESTION_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ANSWER_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_SUBMISSION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_VERIFICATION_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;

/// Native, non-redirecting UAI read and submission transport.
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

    async fn session_for_operation(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<(UaiJwtSession, bool)> {
        match self.sessions.resolve_session(context).await {
            Ok(session) => Ok((session, false)),
            Err(error) if error.kind == ProviderErrorKind::Authentication => {
                Ok((self.sessions.renew_session(context).await?, true))
            }
            Err(error) => Err(error),
        }
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

    async fn fetch_duration_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiDurationDocument> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "duration Course-resource ID",
        )?;
        let unit_id = required_remote_component(
            Some(&serde_json::Value::String(unit_id.to_owned())),
            "duration Unit ID",
        )?;
        let mut user_info = self
            .send_get_with_session(session, static_url(USER_INFO_URL)?, ResponseRoute::UserInfo)
            .await?;
        let identity = parse_user_identity(user_info.as_bytes());
        user_info.zeroize();
        let identity = identity?;
        let document = self
            .send_get_with_session(
                session,
                unit_task_duration_url(
                    &course_resource_id,
                    &unit_id,
                    identity.app_user_id(),
                    identity.sso_id(),
                )?,
                ResponseRoute::Duration,
            )
            .await?;
        UaiDurationDocument::try_new(document)
    }

    async fn fetch_question_content_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiQuestionDocument> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "Question Course-resource ID",
        )?;
        let group_id = required_remote_component(
            Some(&serde_json::Value::String(group_id.to_owned())),
            "Question Group ID",
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
            .get(question_content_url(route.course_instance_id(), &group_id)?)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, sensitive_authorization(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        UaiQuestionDocument::try_new(
            read_json_response(response, ResponseRoute::QuestionContent).await?,
        )
    }

    async fn fetch_answer_documents_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiAnswerDocuments> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "standard-answer Course-resource ID",
        )?;
        let group_id = required_remote_component(
            Some(&serde_json::Value::String(group_id.to_owned())),
            "standard-answer Group ID",
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
        let content_response = self
            .client
            .get(question_content_url(route.course_instance_id(), &group_id)?)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, sensitive_authorization(session)?)
            .header("x-annotator-auth-token", annotator_header.clone())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let content = UaiQuestionDocument::try_new(
            read_json_response(content_response, ResponseRoute::QuestionContent).await?,
        )?;
        let answer_response = self
            .client
            .get(standard_answer_url(route.course_instance_id(), &group_id)?)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, sensitive_authorization(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let answer = UaiAnswerDocument::try_new(
            read_json_response(answer_response, ResponseRoute::StandardAnswer).await?,
        )?;
        Ok(UaiAnswerDocuments::new(content, answer))
    }

    async fn submit_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        group_id: &str,
        plan: &UaiSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "submission Course-resource ID",
        )?;
        let group_id = required_remote_component(
            Some(&serde_json::Value::String(group_id.to_owned())),
            "submission Group ID",
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
        let mut body = build_submission_body(
            route.course_instance_id(),
            session.expose_open_id(),
            &group_id,
            plan,
        )?;
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
            .post(submission_url()?)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header(AUTHORIZATION, sensitive_authorization(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error));
        body.zeroize();
        let response = response?;
        let document = UaiSubmissionResponseDocument::try_new(
            read_json_response(response, ResponseRoute::Submission).await?,
        )?;
        parse_submission_receipt(document.as_str(), route.course_instance_id(), &group_id)
    }

    async fn fetch_verification_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        group_id: &str,
        submission_version: &str,
    ) -> ProviderResult<UaiVerificationDocument> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "verification Course-resource ID",
        )?;
        let group_id = required_remote_component(
            Some(&serde_json::Value::String(group_id.to_owned())),
            "verification Group ID",
        )?;
        let submission_version = required_remote_component(
            Some(&serde_json::Value::String(submission_version.to_owned())),
            "verification submission version",
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
            .get(verification_url(
                route.course_instance_id(),
                &group_id,
                &submission_version,
            )?)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, sensitive_authorization(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let document = read_json_response(response, ResponseRoute::SubmissionVerification).await?;
        validate_verification_course_binding(&document, route.course_instance_id())?;
        UaiVerificationDocument::try_new(document)
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
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_courses_with_session(&session).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_courses_with_session(&session).await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiTaskInventoryTransport for NativeUaiInventoryTransport {
    async fn fetch_tasks(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<UaiTaskInventoryDocuments> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self.fetch_tasks_with_session(&session, course).await {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_tasks_with_session(&session, course).await
            }
            result => result,
        }
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
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_progress_with_session(&session, course_resource_id, unit_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_progress_with_session(&session, course_resource_id, unit_id)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiDurationTransport for NativeUaiInventoryTransport {
    async fn fetch_duration(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiDurationDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_duration_with_session(&session, course_resource_id, unit_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_duration_with_session(&session, course_resource_id, unit_id)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiQuestionTransport for NativeUaiInventoryTransport {
    async fn fetch_question_content(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiQuestionDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_question_content_with_session(&session, course_resource_id, group_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_question_content_with_session(&session, course_resource_id, group_id)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiAnswerTransport for NativeUaiInventoryTransport {
    async fn fetch_answer_documents(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiAnswerDocuments> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_answer_documents_with_session(&session, course_resource_id, group_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_answer_documents_with_session(&session, course_resource_id, group_id)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiSubmissionTransport for NativeUaiInventoryTransport {
    async fn submit(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
        plan: &UaiSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .submit_with_session(&session, course_resource_id, group_id, plan)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.submit_with_session(&session, course_resource_id, group_id, plan)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiVerificationTransport for NativeUaiInventoryTransport {
    async fn fetch_verification(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
        submission_version: &str,
    ) -> ProviderResult<UaiVerificationDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_verification_with_session(
                &session,
                course_resource_id,
                group_id,
                submission_version,
            )
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_verification_with_session(
                    &session,
                    course_resource_id,
                    group_id,
                    submission_version,
                )
                .await
            }
            result => result,
        }
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
    UserInfo,
    Duration,
    QuestionContent,
    StandardAnswer,
    Submission,
    SubmissionVerification,
}

impl ResponseRoute {
    const fn label(self) -> &'static str {
        match self {
            Self::CourseList => "Course inventory",
            Self::CourseDetail => "Course-resource detail",
            Self::TaskTree => "Task tree",
            Self::Progress => "Task progress",
            Self::UserInfo => "User info",
            Self::Duration => "Task duration",
            Self::QuestionContent => "Question content",
            Self::StandardAnswer => "standard answer",
            Self::Submission => "submission",
            Self::SubmissionVerification => "submission verification",
        }
    }

    const fn maximum_bytes(self) -> usize {
        match self {
            Self::Progress => MAX_PROGRESS_RESPONSE_BYTES,
            Self::UserInfo => MAX_USER_INFO_RESPONSE_BYTES,
            Self::Duration => MAX_DURATION_RESPONSE_BYTES,
            Self::QuestionContent => MAX_QUESTION_RESPONSE_BYTES,
            Self::StandardAnswer => MAX_ANSWER_RESPONSE_BYTES,
            Self::Submission => MAX_SUBMISSION_RESPONSE_BYTES,
            Self::SubmissionVerification => MAX_VERIFICATION_RESPONSE_BYTES,
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

fn question_content_url(course_instance_id: &str, group_id: &str) -> ProviderResult<Url> {
    route_url(
        UCONTENT_ORIGIN,
        &[
            "course",
            "api",
            "v3",
            "content",
            course_instance_id,
            group_id,
            "default",
        ],
    )
}

fn standard_answer_url(course_instance_id: &str, group_id: &str) -> ProviderResult<Url> {
    route_url(
        UCONTENT_ORIGIN,
        &[
            "course",
            "api",
            "v3",
            "answer",
            course_instance_id,
            group_id,
            "default",
        ],
    )
}

fn submission_url() -> ProviderResult<Url> {
    route_url(
        UCONTENT_ORIGIN,
        &["course", "api", "v3", "newExploration", "submit"],
    )
}

fn verification_url(
    course_instance_id: &str,
    group_id: &str,
    submission_version: &str,
) -> ProviderResult<Url> {
    let module = format!("{group_id}-{submission_version}");
    route_url(
        UCONTENT_ORIGIN,
        &["api", "mobile", "user_module", course_instance_id, &module],
    )
}

fn build_submission_body(
    course_instance_id: &str,
    open_id: &str,
    group_id: &str,
    plan: &UaiSubmissionPlan,
) -> ProviderResult<String> {
    let children = plan
        .answer_children()
        .iter()
        .map(|values| {
            serde_json::json!({
                "value": values,
                "isDone": true,
                "isRight": true,
                "replyCategory": "objective",
            })
        })
        .collect::<Vec<_>>();
    let inner_answer = serde_json::to_string(&serde_json::json!({
        "value": [],
        "children": children,
        "progress": {},
        "record": {"url": ""},
    }))
    .map_err(|_| static_submission_error())?;
    let judge_type = plan.task_type().replace('_', "-");
    let judges = plan
        .answer_children()
        .iter()
        .map(|values| {
            serde_json::json!({
                "value": values.join(","),
                "question_type": judge_type,
                "reply_type": "objective",
                "versions": {
                    "course": 0,
                    "group": 1,
                    "template": 1,
                    "answer": 0,
                    "content": 0,
                },
                "payloads": [],
            })
        })
        .collect::<Vec<_>>();
    let judges = serde_json::to_string(&judges).map_err(|_| static_submission_error())?;
    serde_json::to_string(&serde_json::json!({
        "quesDatas": [{
            "instanceId": plan.remote_question_id(),
            "answer": inner_answer,
            "context": "{\"state\":\"submitted\"}",
            "contextVersion": 0,
            "answerVersion": 0,
        }],
        "groupId": group_id,
        "isCompleted": vec![true; plan.answer_children().len()],
        "thirdPartyJudges": judges,
        "submitType": 1,
        "hideLoading": false,
        "associationGroupId": "",
        "courseId": course_instance_id,
        "openId": open_id,
        "version": "default",
    }))
    .map_err(|_| static_submission_error())
}

fn static_submission_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI submission request cannot be encoded",
    )
}

fn unit_task_duration_url(
    course_resource_id: &str,
    unit_id: &str,
    app_user_id: &str,
    sso_id: &str,
) -> ProviderResult<Url> {
    let mut url = route_url(
        UAI_ORIGIN,
        &[
            "api",
            "tla",
            "learningDetail",
            "studyRecord",
            "unitTaskSituation",
        ],
    )?;
    url.query_pairs_mut()
        .append_pair("nodeId", unit_id)
        .append_pair("id", course_resource_id)
        .append_pair("appUserId", app_user_id)
        .append_pair("ssoId", sso_id);
    Ok(url)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};
    use asterism_networking::NetworkProfile;

    use super::*;

    #[derive(Debug)]
    struct FixtureSessions;

    #[derive(Debug, Default)]
    struct RenewingSessions {
        resolutions: AtomicUsize,
        renewals: AtomicUsize,
    }

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

    #[async_trait]
    impl UaiSessionResolver for RenewingSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<UaiJwtSession> {
            self.resolutions.fetch_add(1, Ordering::SeqCst);
            Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "synthetic expired session",
            ))
        }

        async fn renew_session(&self, _context: &ProviderContext) -> ProviderResult<UaiJwtSession> {
            self.renewals.fetch_add(1, Ordering::SeqCst);
            UaiJwtSession::try_new("renewed-open-id", "NEW.HEADER.SIGNATURE")
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

    #[tokio::test]
    async fn operation_session_renews_only_an_authentication_failure() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let sessions = Arc::new(RenewingSessions::default());
        let transport = NativeUaiInventoryTransport::try_new(&network, sessions.clone()).unwrap();
        let (session, renewed) = transport
            .session_for_operation(&provider_context())
            .await
            .unwrap();
        assert!(renewed);
        assert_eq!(session.expose_open_id(), "renewed-open-id");
        assert_eq!(sessions.resolutions.load(Ordering::SeqCst), 1);
        assert_eq!(sessions.renewals.load(Ordering::SeqCst), 1);
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
        assert_eq!(
            question_content_url("course-v2:synthetic+rw/guard", "group/1")
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/course/api/v3/content/course-v2:synthetic+rw%2Fguard/group%2F1/default"
        );
        assert_eq!(
            standard_answer_url("course-v2:synthetic+rw/guard", "group/1")
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/course/api/v3/answer/course-v2:synthetic+rw%2Fguard/group%2F1/default"
        );
        assert_eq!(
            submission_url().unwrap().as_str(),
            "https://ucontent.unipus.cn/course/api/v3/newExploration/submit"
        );
        assert_eq!(
            verification_url("course-v2:synthetic+rw/guard", "group/1", "submit/version",)
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/api/mobile/user_module/course-v2:synthetic+rw%2Fguard/group%2F1-submit%2Fversion"
        );
        assert_eq!(ResponseRoute::Progress.maximum_bytes(), 1_024 * 1_024);
        assert_eq!(
            ResponseRoute::QuestionContent.maximum_bytes(),
            4 * 1_024 * 1_024
        );
        assert_eq!(
            ResponseRoute::StandardAnswer.maximum_bytes(),
            4 * 1_024 * 1_024
        );
        assert_eq!(ResponseRoute::Submission.maximum_bytes(), 1_024 * 1_024);
        assert_eq!(
            ResponseRoute::SubmissionVerification.maximum_bytes(),
            4 * 1_024 * 1_024
        );
        assert_eq!(
            unit_task_duration_url("2001", "unit-1", "app-42", "sso.42")
                .unwrap()
                .as_str(),
            "https://uai.unipus.cn/api/tla/learningDetail/studyRecord/unitTaskSituation?nodeId=unit-1&id=2001&appUserId=app-42&ssoId=sso.42"
        );
        assert_eq!(ResponseRoute::UserInfo.maximum_bytes(), 64 * 1_024);
        assert_eq!(ResponseRoute::Duration.maximum_bytes(), 1_024 * 1_024);
    }

    #[test]
    fn submission_body_matches_the_audited_simple_json_shape() {
        let plan = UaiSubmissionPlan::fixture(
            "question-1",
            "multichoice",
            vec![vec!["A".to_owned(), "B".to_owned()]],
        );
        let body = build_submission_body(
            "course-v2:synthetic+rw",
            "synthetic-open-id",
            "group-1",
            &plan,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["groupId"], "group-1");
        assert_eq!(body["courseId"], "course-v2:synthetic+rw");
        assert_eq!(body["openId"], "synthetic-open-id");
        assert_eq!(body["submitType"], 1);
        assert_eq!(body["quesDatas"][0]["instanceId"], "question-1");
        let answer: serde_json::Value =
            serde_json::from_str(body["quesDatas"][0]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(answer["children"][0]["value"][0], "A");
        assert_eq!(answer["children"][0]["value"][1], "B");
        assert_eq!(body["isCompleted"], serde_json::json!([true]));
        let judges: serde_json::Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges[0]["value"], "A,B");
        assert_eq!(judges[0]["question_type"], "multichoice");
        assert_eq!(judges[0]["reply_type"], "objective");
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-native-renewal-test".to_owned(),
        }
    }
}
