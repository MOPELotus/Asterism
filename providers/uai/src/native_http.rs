use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use asterism_domain::SubmissionReceipt;
use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, RemoteCourse,
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, RETRY_AFTER},
};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UaiAggregateProgressDocument, UaiAggregateProgressTransport, UaiAnswerDocument,
    UaiAnswerDocuments, UaiAnswerTransport, UaiCourseInventoryTransport, UaiCoursePolicyDocument,
    UaiCoursePolicyTransport, UaiCourseProgressDocument, UaiDiscussionBinding,
    UaiDiscussionReplyDraft, UaiDiscussionReplyPage, UaiDiscussionTransport, UaiDurationDocument,
    UaiDurationTransport, UaiInventoryDocument, UaiJwtSession, UaiPresetCompletionResult,
    UaiPresetCompletionTransport, UaiProgressDocument, UaiProgressTransport, UaiQuestionDocument,
    UaiQuestionTransport, UaiSessionResolver, UaiSubmissionPlan, UaiSubmissionResponseDocument,
    UaiSubmissionTransport, UaiTaskInventoryDocuments, UaiTaskInventoryTransport,
    UaiUploadArtifact, UaiUploadGrant, UaiUploadIntent, UaiUploadTransport, UaiUploadedArtifact,
    UaiVerificationDocument, UaiVerificationTransport,
    annotator::generate_annotator_token,
    build_discussion_reply_page_request, build_discussion_reply_request,
    build_discussion_topic_request, build_upload_multipart,
    course_inventory::{
        course_resource_id_from_remote, parse_course_context_for_resource_id,
        required_remote_component,
    },
    encrypted::ZeroizingJsonValue,
    parse_course_context, parse_course_progress, parse_discussion_binding,
    parse_discussion_reply_page, parse_discussion_reply_receipt, parse_discussion_topic,
    parse_group_progress, parse_submission_receipt, parse_task_inventory, parse_upload_grant,
    parse_upload_result,
    submission_verify::validate_verification_course_binding,
    task_inventory::parse_task_tree_unit_ids,
    user_identity::parse_user_identity,
};

const COURSE_LIST_URL: &str = "https://uai.unipus.cn/api/cmgt/course/getCourseListByStudent";
const USER_INFO_URL: &str = "https://uai.unipus.cn/api/account/user/info";
const UAI_ORIGIN: &str = "https://uai.unipus.cn";
const UCONTENT_ORIGIN: &str = "https://ucontent.unipus.cn";
const UCLOUD_ORIGIN: &str = "https://ucloud.unipus.cn";
const UCONTENT_REFERER: &str = "https://ucontent.unipus.cn/";
const QINIU_UPLOAD_URL: &str = "https://upload-z1.qiniup.com/";
const MAX_INVENTORY_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_PROGRESS_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_USER_INFO_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_DURATION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_QUESTION_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ANSWER_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_SUBMISSION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_SUBMISSION_REQUEST_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_VERIFICATION_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_DISCUSSION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_UPLOAD_RESPONSE_BYTES: usize = 64 * 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmptyCompletionPreflight {
    Preset,
    ExitTicket,
    Oral,
}

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
        let headers = if url.host_str() == Some("ucontent.unipus.cn") {
            ucontent_session_headers(session)?
        } else {
            authorization_headers(session)?
        };
        let response = self
            .client
            .get(url)
            .header(ACCEPT, "application/json")
            .headers(headers)
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
        parse_task_inventory(course, &route, tree.as_str())?;
        let tree_units = parse_task_tree_unit_ids(tree.as_str())?;
        let course_progress = self
            .fetch_course_progress_with_session(session, route.course_instance_id())
            .await?;
        let course_snapshot = parse_course_progress(course_progress.as_str())?;
        if course_snapshot.units().keys().collect::<BTreeSet<_>>()
            != tree_units.iter().collect::<BTreeSet<_>>()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI native Course progress Units do not match the Task tree",
            ));
        }
        let mut progress_by_unit = BTreeMap::new();
        for unit_id in tree_units {
            let progress = self
                .fetch_progress_for_route_with_session(
                    session,
                    route.course_instance_id(),
                    &unit_id,
                )
                .await?;
            progress_by_unit.insert(unit_id, progress);
        }
        Ok(UaiTaskInventoryDocuments::new(detail, tree)
            .with_course_progress(course_progress)
            .with_unit_progress(progress_by_unit))
    }

    async fn fetch_course_progress_with_session(
        &self,
        session: &UaiJwtSession,
        course_instance_id: &str,
    ) -> ProviderResult<UaiCourseProgressDocument> {
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
            .get(course_overview_progress_url(
                course_instance_id,
                session.expose_open_id(),
            )?)
            .header(ACCEPT, "application/json")
            .headers(ucontent_session_headers(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        UaiCourseProgressDocument::try_new(
            read_json_response(response, ResponseRoute::CourseProgress).await?,
        )
    }

    async fn fetch_progress_for_route_with_session(
        &self,
        session: &UaiJwtSession,
        course_instance_id: &str,
        unit_id: &str,
    ) -> ProviderResult<UaiProgressDocument> {
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
                course_instance_id,
                unit_id,
                session.expose_open_id(),
            )?)
            .header(ACCEPT, "application/json")
            .headers(ucontent_session_headers(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        UaiProgressDocument::try_new(read_json_response(response, ResponseRoute::Progress).await?)
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
        self.fetch_progress_for_route_with_session(session, route.course_instance_id(), &unit_id)
            .await
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

    async fn fetch_aggregate_progress_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
    ) -> ProviderResult<UaiAggregateProgressDocument> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "aggregate-progress Course-resource ID",
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
                aggregate_progress_url(&course_resource_id, identity.app_user_id())?,
                ResponseRoute::AggregateProgress,
            )
            .await?;
        UaiAggregateProgressDocument::try_new(course_resource_id, identity.app_user_id(), document)
    }

    async fn fetch_course_policy_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
    ) -> ProviderResult<UaiCoursePolicyDocument> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "Course-policy Course-resource ID",
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
        let body = Zeroizing::new(build_course_policy_request_body(
            route.course_resource_id(),
            route.strategy_id(),
        )?);
        let response = self
            .client
            .post(course_policy_url()?)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .headers(authorization_headers(session)?)
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        UaiCoursePolicyDocument::try_new(
            route.course_resource_id(),
            route.strategy_id(),
            read_json_response(response, ResponseRoute::CoursePolicy).await?,
        )
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
            .headers(ucontent_session_headers(session)?)
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
            .headers(ucontent_session_headers(session)?)
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
            .headers(ucontent_session_headers(session)?)
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
        unit_id: &str,
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
        let unit_id = required_remote_component(
            Some(&serde_json::Value::String(unit_id.to_owned())),
            "submission Unit ID",
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
        let progress = self
            .fetch_progress_for_route_with_session(session, route.course_instance_id(), &unit_id)
            .await?;
        validate_answer_submission_progress_target(
            progress.as_str(),
            &unit_id,
            &group_id,
            Utc::now(),
        )?;
        let body = Zeroizing::new(build_submission_body(
            route.course_instance_id(),
            session.expose_open_id(),
            &group_id,
            plan,
        )?);
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
            .headers(ucontent_session_headers(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error));
        let response = response?;
        let document = UaiSubmissionResponseDocument::try_new(
            read_json_response(response, ResponseRoute::Submission).await?,
        )?;
        parse_submission_receipt(document.as_str(), route.course_instance_id(), &group_id)
    }

    async fn complete_preset_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        self.complete_empty_with_session(
            session,
            course_resource_id,
            unit_id,
            group_id,
            EmptyCompletionPreflight::Preset,
            None,
        )
        .await
    }

    async fn complete_exit_ticket_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        self.complete_empty_with_session(
            session,
            course_resource_id,
            unit_id,
            group_id,
            EmptyCompletionPreflight::ExitTicket,
            None,
        )
        .await
    }

    async fn complete_oral_empty_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
        task_type: &str,
        question_count: u32,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        self.complete_empty_with_session(
            session,
            course_resource_id,
            unit_id,
            group_id,
            EmptyCompletionPreflight::Oral,
            Some((task_type, question_count)),
        )
        .await
    }

    async fn complete_empty_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
        preflight: EmptyCompletionPreflight,
        oral: Option<(&str, u32)>,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "preset completion Course-resource ID",
        )?;
        let group_id = required_remote_component(
            Some(&serde_json::Value::String(group_id.to_owned())),
            "preset completion Group ID",
        )?;
        let unit_id = required_remote_component(
            Some(&serde_json::Value::String(unit_id.to_owned())),
            "preset completion Unit ID",
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
        let progress_response = self
            .client
            .get(course_progress_url(
                route.course_instance_id(),
                &unit_id,
                session.expose_open_id(),
            )?)
            .header(ACCEPT, "application/json")
            .headers(ucontent_session_headers(session)?)
            .header("x-annotator-auth-token", annotator_header.clone())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let progress = UaiProgressDocument::try_new(
            read_json_response(progress_response, ResponseRoute::Progress).await?,
        )?;
        if validate_empty_completion_progress_target(
            progress.as_str(),
            &unit_id,
            &group_id,
            preflight,
        )? {
            return Ok(UaiPresetCompletionResult::AlreadyCompleted);
        }

        let body = Zeroizing::new(match oral {
            Some((task_type, question_count)) => build_oral_empty_submission_body(
                route.course_instance_id(),
                session.expose_open_id(),
                &group_id,
                task_type,
                question_count,
            )?,
            None => build_preset_submission_body(
                route.course_instance_id(),
                session.expose_open_id(),
                &group_id,
            )?,
        });
        let response = self
            .client
            .post(submission_url()?)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .headers(ucontent_session_headers(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error));
        let response = response?;
        let document = UaiSubmissionResponseDocument::try_new(
            read_json_response(response, ResponseRoute::Submission).await?,
        )?;
        parse_submission_receipt(document.as_str(), route.course_instance_id(), &group_id)
            .map(UaiPresetCompletionResult::Submitted)
    }

    async fn resolve_discussion_binding_with_session(
        &self,
        session: &UaiJwtSession,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiDiscussionBinding> {
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(course_resource_id.to_owned())),
            "discussion Course-resource ID",
        )?;
        let group_id = required_remote_component(
            Some(&serde_json::Value::String(group_id.to_owned())),
            "discussion Group ID",
        )?;
        let courses = self.fetch_courses_with_session(session).await?;
        let detail = UaiInventoryDocument::try_new(
            self.send_get_with_session(
                session,
                course_resource_detail_url(&course_resource_id)?,
                ResponseRoute::CourseDetail,
            )
            .await?,
        )?;
        let mut user_info = self
            .send_get_with_session(session, static_url(USER_INFO_URL)?, ResponseRoute::UserInfo)
            .await?;
        let binding = parse_discussion_binding(
            courses.as_str(),
            detail.as_str(),
            user_info.as_bytes(),
            &course_resource_id,
            &group_id,
        );
        user_info.zeroize();
        binding
    }

    async fn send_discussion_post_with_session(
        &self,
        session: &UaiJwtSession,
        url: Url,
        body: &str,
        route: ResponseRoute,
        include_referer: bool,
    ) -> ProviderResult<String> {
        let annotator = generate_annotator_token(session.expose_open_id())?;
        let mut annotator_header =
            HeaderValue::from_str(annotator.expose_secret()).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "UAI annotator token cannot be encoded as a request header",
                )
            })?;
        annotator_header.set_sensitive(true);
        let mut request = self
            .client
            .post(url)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .headers(authorization_headers(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .header("u-app-id", "1501")
            .header("u-platform", "2");
        if include_referer {
            request = request.header("Referer", UCONTENT_REFERER);
        }
        let response = request
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        read_json_response(response, route).await
    }

    async fn find_discussion_topic_with_session(
        &self,
        session: &UaiJwtSession,
        binding: &UaiDiscussionBinding,
    ) -> ProviderResult<Option<u64>> {
        let body = build_discussion_topic_request(binding)?;
        let mut document = self
            .send_discussion_post_with_session(
                session,
                discussion_topic_url()?,
                body.as_str(),
                ResponseRoute::DiscussionTopic,
                false,
            )
            .await?;
        let topic = parse_discussion_topic(&document);
        document.zeroize();
        topic
    }

    async fn read_discussion_replies_with_session(
        &self,
        session: &UaiJwtSession,
        topic_id: u64,
        page_number: u32,
        page_size: u32,
    ) -> ProviderResult<UaiDiscussionReplyPage> {
        let body = build_discussion_reply_page_request(topic_id, page_number, page_size)?;
        let mut document = self
            .send_discussion_post_with_session(
                session,
                discussion_replies_url()?,
                body.as_str(),
                ResponseRoute::DiscussionReplies,
                false,
            )
            .await?;
        let page = parse_discussion_reply_page(&document, page_size);
        document.zeroize();
        page
    }

    async fn submit_discussion_reply_with_session(
        &self,
        session: &UaiJwtSession,
        draft: &UaiDiscussionReplyDraft,
    ) -> ProviderResult<SubmissionReceipt> {
        let body = build_discussion_reply_request(draft)?;
        let mut document = self
            .send_discussion_post_with_session(
                session,
                discussion_reply_mutation_url()?,
                body.as_str(),
                ResponseRoute::DiscussionReplyMutation,
                true,
            )
            .await?;
        let receipt = parse_discussion_reply_receipt(&document);
        document.zeroize();
        receipt
    }

    async fn request_upload_grant_with_session(
        &self,
        session: &UaiJwtSession,
        intent: &UaiUploadIntent,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadGrant> {
        if !intent.matches_artifact(artifact) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload intent is foreign to the selected artifact",
            ));
        }
        let course_resource_id = required_remote_component(
            Some(&serde_json::Value::String(
                intent.course_resource_id().to_owned(),
            )),
            "upload Course-resource ID",
        )?;
        let _group_id = required_remote_component(
            Some(&serde_json::Value::String(intent.group_id().to_owned())),
            "upload Group ID",
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
        let mut user_info = self
            .send_get_with_session(session, static_url(USER_INFO_URL)?, ResponseRoute::UserInfo)
            .await?;
        let identity = parse_user_identity(user_info.as_bytes());
        user_info.zeroize();
        let identity = identity?;
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
            .get(upload_grant_url(
                route.course_instance_id(),
                identity.app_user_id(),
                artifact,
            )?)
            .header(ACCEPT, "application/json")
            .headers(ucontent_session_headers(session)?)
            .header("x-annotator-auth-token", annotator_header)
            .header("Referer", UCONTENT_REFERER)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let mut document = read_json_response(response, ResponseRoute::UploadGrant).await?;
        let grant = parse_upload_grant(&document, intent);
        document.zeroize();
        grant
    }

    async fn upload_artifact_with_session(
        &self,
        grant: &UaiUploadGrant,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadedArtifact> {
        let multipart = build_upload_multipart(grant, artifact)?;
        let response = self
            .client
            .post(static_url(QINIU_UPLOAD_URL)?)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, multipart.content_type())
            .body(multipart.expose_body().to_vec())
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        let mut document = read_json_response(response, ResponseRoute::ObjectUpload).await?;
        let key = parse_upload_result(&document, grant.file_key())
            .and_then(|key| UaiUploadedArtifact::from_grant(grant, key));
        document.zeroize();
        key
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
            .headers(ucontent_session_headers(session)?)
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
impl UaiAggregateProgressTransport for NativeUaiInventoryTransport {
    async fn fetch_aggregate_progress(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
    ) -> ProviderResult<UaiAggregateProgressDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_aggregate_progress_with_session(&session, course_resource_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_aggregate_progress_with_session(&session, course_resource_id)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiCoursePolicyTransport for NativeUaiInventoryTransport {
    async fn fetch_course_policy(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
    ) -> ProviderResult<UaiCoursePolicyDocument> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .fetch_course_policy_with_session(&session, course_resource_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.fetch_course_policy_with_session(&session, course_resource_id)
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
        unit_id: &str,
        group_id: &str,
        plan: &UaiSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .submit_with_session(&session, course_resource_id, unit_id, group_id, plan)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.submit_with_session(&session, course_resource_id, unit_id, group_id, plan)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiPresetCompletionTransport for NativeUaiInventoryTransport {
    async fn complete_preset(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .complete_preset_with_session(&session, course_resource_id, unit_id, group_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.complete_preset_with_session(&session, course_resource_id, unit_id, group_id)
                    .await
            }
            result => result,
        }
    }

    async fn complete_exit_ticket(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .complete_exit_ticket_with_session(&session, course_resource_id, unit_id, group_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.complete_exit_ticket_with_session(
                    &session,
                    course_resource_id,
                    unit_id,
                    group_id,
                )
                .await
            }
            result => result,
        }
    }

    async fn complete_oral_empty(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
        task_type: &str,
        question_count: u32,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .complete_oral_empty_with_session(
                &session,
                course_resource_id,
                unit_id,
                group_id,
                task_type,
                question_count,
            )
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.complete_oral_empty_with_session(
                    &session,
                    course_resource_id,
                    unit_id,
                    group_id,
                    task_type,
                    question_count,
                )
                .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiDiscussionTransport for NativeUaiInventoryTransport {
    async fn resolve_discussion_binding(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiDiscussionBinding> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .resolve_discussion_binding_with_session(&session, course_resource_id, group_id)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.resolve_discussion_binding_with_session(&session, course_resource_id, group_id)
                    .await
            }
            result => result,
        }
    }

    async fn find_discussion_topic(
        &self,
        context: &ProviderContext,
        binding: &UaiDiscussionBinding,
    ) -> ProviderResult<Option<u64>> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .find_discussion_topic_with_session(&session, binding)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.find_discussion_topic_with_session(&session, binding)
                    .await
            }
            result => result,
        }
    }

    async fn read_discussion_replies(
        &self,
        context: &ProviderContext,
        topic_id: u64,
        page_number: u32,
        page_size: u32,
    ) -> ProviderResult<UaiDiscussionReplyPage> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .read_discussion_replies_with_session(&session, topic_id, page_number, page_size)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.read_discussion_replies_with_session(
                    &session,
                    topic_id,
                    page_number,
                    page_size,
                )
                .await
            }
            result => result,
        }
    }

    async fn submit_discussion_reply(
        &self,
        context: &ProviderContext,
        draft: &UaiDiscussionReplyDraft,
    ) -> ProviderResult<SubmissionReceipt> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .submit_discussion_reply_with_session(&session, draft)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.submit_discussion_reply_with_session(&session, draft)
                    .await
            }
            result => result,
        }
    }
}

#[async_trait]
impl UaiUploadTransport for NativeUaiInventoryTransport {
    async fn request_upload_grant(
        &self,
        context: &ProviderContext,
        intent: &UaiUploadIntent,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadGrant> {
        let (session, renewed) = self.session_for_operation(context).await?;
        match self
            .request_upload_grant_with_session(&session, intent, artifact)
            .await
        {
            Err(error) if error.kind == ProviderErrorKind::Authentication && !renewed => {
                let session = self.sessions.renew_session(context).await?;
                self.request_upload_grant_with_session(&session, intent, artifact)
                    .await
            }
            result => result,
        }
    }

    async fn upload_artifact(
        &self,
        context: &ProviderContext,
        grant: &UaiUploadGrant,
        artifact: &UaiUploadArtifact,
    ) -> ProviderResult<UaiUploadedArtifact> {
        let (_session, _renewed) = self.session_for_operation(context).await?;
        self.upload_artifact_with_session(grant, artifact).await
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

fn authorization_headers(session: &UaiJwtSession) -> ProviderResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, sensitive_authorization(session)?);
    Ok(headers)
}

fn ucontent_session_headers(session: &UaiJwtSession) -> ProviderResult<HeaderMap> {
    let mut headers = authorization_headers(session)?;
    let mut open_id = HeaderValue::from_str(session.expose_open_id()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI session contains an invalid open ID header value",
        )
    })?;
    open_id.set_sensitive(true);
    headers.insert("u-openid", open_id.clone());
    headers.insert("x-csrftoken", open_id);
    headers.insert("u-app-id", HeaderValue::from_static("39"));
    headers.insert("u-platform", HeaderValue::from_static("2"));
    headers.insert("appid", HeaderValue::from_static("undefined"));
    headers.insert(
        "origin",
        HeaderValue::from_static("https://ucontent.unipus.cn"),
    );
    headers.insert(
        "referer",
        HeaderValue::from_static("https://ucontent.unipus.cn/_explorationpc_default/pc.html"),
    );
    if let Some(school) = session.expose_school_header() {
        let mut school = HeaderValue::from_str(school).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "UAI browser compatibility school header is invalid",
            )
        })?;
        school.set_sensitive(true);
        headers.insert("u-school", school);
    }
    if let Some(cookie) = session.expose_browser_cookie() {
        let mut cookie = HeaderValue::from_str(cookie).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "UAI browser compatibility Cookie is invalid",
            )
        })?;
        cookie.set_sensitive(true);
        headers.insert(COOKIE, cookie);
    }
    Ok(headers)
}

#[derive(Clone, Copy)]
enum ResponseRoute {
    CourseList,
    CourseDetail,
    TaskTree,
    CourseProgress,
    Progress,
    UserInfo,
    Duration,
    AggregateProgress,
    CoursePolicy,
    QuestionContent,
    StandardAnswer,
    Submission,
    SubmissionVerification,
    DiscussionTopic,
    DiscussionReplies,
    DiscussionReplyMutation,
    UploadGrant,
    ObjectUpload,
}

impl ResponseRoute {
    const fn label(self) -> &'static str {
        match self {
            Self::CourseList => "Course inventory",
            Self::CourseDetail => "Course-resource detail",
            Self::TaskTree => "Task tree",
            Self::CourseProgress => "Course progress",
            Self::Progress => "Task progress",
            Self::UserInfo => "User info",
            Self::Duration => "Task duration",
            Self::AggregateProgress => "Course and Unit aggregate progress",
            Self::CoursePolicy => "required Course policy",
            Self::QuestionContent => "Question content",
            Self::StandardAnswer => "standard answer",
            Self::Submission => "submission",
            Self::SubmissionVerification => "submission verification",
            Self::DiscussionTopic => "discussion topic",
            Self::DiscussionReplies => "discussion replies",
            Self::DiscussionReplyMutation => "discussion reply mutation",
            Self::UploadGrant => "upload grant",
            Self::ObjectUpload => "object upload",
        }
    }

    const fn maximum_bytes(self) -> usize {
        match self {
            Self::CourseProgress | Self::Progress => MAX_PROGRESS_RESPONSE_BYTES,
            Self::UserInfo => MAX_USER_INFO_RESPONSE_BYTES,
            Self::Duration | Self::AggregateProgress | Self::CoursePolicy => {
                MAX_DURATION_RESPONSE_BYTES
            }
            Self::QuestionContent => MAX_QUESTION_RESPONSE_BYTES,
            Self::StandardAnswer => MAX_ANSWER_RESPONSE_BYTES,
            Self::Submission => MAX_SUBMISSION_RESPONSE_BYTES,
            Self::SubmissionVerification => MAX_VERIFICATION_RESPONSE_BYTES,
            Self::DiscussionTopic | Self::DiscussionReplies | Self::DiscussionReplyMutation => {
                MAX_DISCUSSION_RESPONSE_BYTES
            }
            Self::UploadGrant | Self::ObjectUpload => MAX_UPLOAD_RESPONSE_BYTES,
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

fn course_overview_progress_url(course_instance_id: &str, open_id: &str) -> ProviderResult<Url> {
    route_url(
        UCONTENT_ORIGIN,
        &[
            "course",
            "api",
            "v2",
            "course_progress",
            course_instance_id,
            open_id,
            "default",
        ],
    )
}

fn aggregate_progress_url(course_resource_id: &str, app_user_id: &str) -> ProviderResult<Url> {
    let mut url = route_url(
        UAI_ORIGIN,
        &[
            "api",
            "tla",
            "learningDetail",
            "studyRecord",
            "totalAndUnitSituation",
        ],
    )?;
    url.query_pairs_mut()
        .append_pair("id", course_resource_id)
        .append_pair("appUserId", app_user_id);
    Ok(url)
}

fn course_policy_url() -> ProviderResult<Url> {
    route_url(UAI_ORIGIN, &["api", "tla", "courseStudyStrategy", "detail"])
}

fn build_course_policy_request_body(
    course_resource_id: &str,
    strategy_id: u64,
) -> ProviderResult<String> {
    if strategy_id == 0 {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI Course-policy request has an invalid strategy ID",
        ));
    }
    serde_json::to_string(&serde_json::json!({
        "id": strategy_id,
        "courseResourceId": course_resource_id,
    }))
    .map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI Course-policy request serialization failed",
        )
    })
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

fn discussion_topic_url() -> ProviderResult<Url> {
    route_url(UCLOUD_ORIGIN, &["api", "bbs", "utopic", "page"])
}

fn discussion_replies_url() -> ProviderResult<Url> {
    route_url(UCLOUD_ORIGIN, &["api", "bbs", "ureply", "top", "page"])
}

fn discussion_reply_mutation_url() -> ProviderResult<Url> {
    route_url(UCLOUD_ORIGIN, &["api", "bbs", "ureply", "add"])
}

fn upload_grant_url(
    course_instance_id: &str,
    user_id: &str,
    artifact: &UaiUploadArtifact,
) -> ProviderResult<Url> {
    let mut url = route_url(UCONTENT_ORIGIN, &["media", "user_resource", "cms", "token"])?;
    url.query_pairs_mut()
        .append_pair("name", artifact.filename())
        .append_pair("filetype", "audio")
        .append_pair("isconvert", "0")
        .append_pair("courseid", course_instance_id)
        .append_pair("userid", user_id);
    Ok(url)
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
    let (context_version, answer_version) = if plan.questions().len() == 1 {
        (0, 0)
    } else {
        (1, 1)
    };
    let protocol_versions = plan.protocol_versions();
    let mut question_data = ZeroizingJsonValue::new(serde_json::Value::Array(Vec::with_capacity(
        plan.questions().len(),
    )));
    let mut is_completed = Vec::new();
    let mut judges = ZeroizingJsonValue::new(serde_json::Value::Array(Vec::new()));
    for question in plan.questions() {
        let children = question
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
        let inner_answer_value = ZeroizingJsonValue::new(serde_json::json!({
                "value": [],
                "children": children,
                "progress": {},
                "record": {"url": ""},
        }));
        let inner_answer = Zeroizing::new(
            serde_json::to_string(inner_answer_value.as_value())
                .map_err(|_| static_submission_error())?,
        );
        question_data
            .as_value_mut()
            .as_array_mut()
            .ok_or_else(static_submission_error)?
            .push(serde_json::json!({
                "instanceId": question.remote_question_id(),
                "answer": inner_answer.as_str(),
                "context": "{\"state\":\"submitted\"}",
                "contextVersion": context_version,
                "answerVersion": answer_version,
            }));
        if question.judges().len() != question.answer_children().len() {
            return Err(static_submission_error());
        }
        for (values, judge) in question.answer_children().iter().zip(question.judges()) {
            is_completed.push(true);
            judges
                .as_value_mut()
                .as_array_mut()
                .ok_or_else(static_submission_error)?
                .push(serde_json::json!({
                    "value": values.join(","),
                    "question_type": judge.question_type(),
                    "reply_type": judge.reply_type(),
                    "versions": {
                        "course": protocol_versions.course(),
                        "group": 1,
                        "template": 1,
                        "answer": protocol_versions.answer(),
                        "content": 0,
                    },
                    "payloads": [],
                }));
        }
    }
    let judges = Zeroizing::new(
        serde_json::to_string(judges.as_value()).map_err(|_| static_submission_error())?,
    );
    let body = ZeroizingJsonValue::new(serde_json::json!({
        "quesDatas": question_data.as_value(),
        "groupId": group_id,
        "isCompleted": is_completed,
        "thirdPartyJudges": judges.as_str(),
        "submitType": plan.submit_type(),
        "hideLoading": false,
        "associationGroupId": "",
        "courseId": course_instance_id,
        "openId": open_id,
        "version": "default",
    }));
    let mut encoded =
        serde_json::to_string(body.as_value()).map_err(|_| static_submission_error())?;
    if encoded.is_empty() || encoded.len() > MAX_SUBMISSION_REQUEST_BYTES {
        encoded.zeroize();
        return Err(static_submission_error());
    }
    Ok(encoded)
}

fn build_preset_submission_body(
    course_instance_id: &str,
    open_id: &str,
    group_id: &str,
) -> ProviderResult<String> {
    serde_json::to_string(&serde_json::json!({
        "quesDatas": [],
        "groupId": group_id,
        "isCompleted": [],
        "thirdPartyJudges": "[]",
        "submitType": 2,
        "hideLoading": true,
        "associationGroupId": "",
        "courseId": course_instance_id,
        "openId": open_id,
        "version": "default",
    }))
    .map_err(|_| static_submission_error())
}

fn build_oral_empty_submission_body(
    course_instance_id: &str,
    open_id: &str,
    group_id: &str,
    task_type: &str,
    question_count: u32,
) -> ProviderResult<String> {
    if !matches!(
        task_type,
        "oral-sentence" | "video-dub" | "oral-personal-state"
    ) || !(1..=128).contains(&question_count)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI oral empty submission received an unsupported bounded shape",
        ));
    }
    let question = build_oral_placeholder_question(group_id, question_count)?;
    let questions = vec![question; question_count as usize];
    let completed = vec![true; question_count as usize];
    let judge = serde_json::json!({
        "value": "",
        "question_type": task_type.replace('_', "-"),
        "reply_type": task_type.replace('_', "-"),
        "versions": {"course": 123_290, "group": 1, "template": 1, "answer": 3, "content": 0},
        "payloads": [],
    });
    let judges = serde_json::to_string(&vec![judge; question_count as usize])
        .map_err(|_| static_submission_error())?;
    let mut encoded = serde_json::to_string(&serde_json::json!({
        "quesDatas": questions,
        "groupId": group_id,
        "isCompleted": completed,
        "thirdPartyJudges": judges,
        "submitType": 1,
        "hideLoading": false,
        "associationGroupId": "",
        "courseId": course_instance_id,
        "openId": open_id,
        "version": "default",
    }))
    .map_err(|_| static_submission_error())?;
    if encoded.is_empty() || encoded.len() > MAX_SUBMISSION_REQUEST_BYTES {
        encoded.zeroize();
        return Err(static_submission_error());
    }
    Ok(encoded)
}

fn build_oral_placeholder_question(
    group_id: &str,
    question_count: u32,
) -> ProviderResult<serde_json::Value> {
    let now = Utc::now();
    let now_seconds = now.timestamp();
    let completion_deadline = now_seconds
        .checked_add(30 * 24 * 60 * 60)
        .ok_or_else(static_submission_error)?;
    let submit_info = serde_json::json!({
        "group_id": group_id,
        "strategyId": 0,
        "record_grade": {"scorePct": 1.0, "ts": now.timestamp_millis()},
        "state": {
            "expired": false, "lastSubmit": now_seconds, "not_start": false,
            "real_score_pct": "1.0", "score": vec![1; question_count as usize],
            "score_avg": "100", "score_pct": "1.0", "state": 0,
        },
        "strategy": {
            "endTime": completion_deadline, "record_every_submit": false,
            "record_max_submit": false, "required": true, "startTime": now_seconds,
            "task_mini_score_pct": 0,
        },
        "version": now_seconds.to_string(), "pass": true,
    });
    let course_answer = serde_json::json!({
        "isStudy": false, "groupId": group_id, "quesId": "0", "isRight": [true],
        "isDone": [true], "subPct": [1], "counted": [true], "isObjective": [true],
        "firstSubmit": false, "submitInfo": submit_info,
    });
    let course_answer_map = serde_json::json!({"0": course_answer});
    let answer = serde_json::to_string(&serde_json::json!({
        "value": [],
        "children": [{"value": [""], "isDone": true, "isRight": true, "replyCategory": "objective"}],
        "progress": {}, "record": {"url": ""},
    }))
    .map_err(|_| static_submission_error())?;
    let context = serde_json::to_string(&serde_json::json!({"state": "submitted"}))
        .map_err(|_| static_submission_error())?;
    Ok(serde_json::json!({
        "courseAnswer": course_answer_map["0"], "courseAnswerMap": course_answer_map,
        "instanceId": "0", "answer": answer, "context": context,
        "contextVersion": 1, "answerVersion": 0,
    }))
}

fn validate_empty_completion_progress_target(
    document: &str,
    expected_unit_id: &str,
    expected_group_id: &str,
    preflight: EmptyCompletionPreflight,
) -> ProviderResult<bool> {
    validate_empty_completion_progress_target_at(
        document,
        expected_unit_id,
        expected_group_id,
        preflight,
        Utc::now(),
    )
}

fn validate_empty_completion_progress_target_at(
    document: &str,
    expected_unit_id: &str,
    expected_group_id: &str,
    preflight: EmptyCompletionPreflight,
    now: asterism_domain::Timestamp,
) -> ProviderResult<bool> {
    let snapshot = parse_group_progress(document, expected_unit_id, expected_group_id)?;
    let tab_type_matches = match preflight {
        EmptyCompletionPreflight::Preset => matches!(snapshot.tab_type(), Some("text" | "video")),
        EmptyCompletionPreflight::ExitTicket | EmptyCompletionPreflight::Oral => {
            snapshot.tab_type() == Some("task")
        }
    };
    if !tab_type_matches {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            match preflight {
                EmptyCompletionPreflight::Preset => {
                    "UAI preset completion requires a fresh text or video progress leaf"
                }
                EmptyCompletionPreflight::ExitTicket => {
                    "UAI exit-ticket completion requires a fresh task progress leaf"
                }
                EmptyCompletionPreflight::Oral => {
                    "UAI oral empty submission requires a fresh task progress leaf"
                }
            },
        ));
    }
    if !snapshot.mutation_available_at(now) {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI Group is outside its fresh donor availability window",
        ));
    }
    Ok(snapshot.is_completed())
}

fn validate_answer_submission_progress_target(
    document: &str,
    expected_unit_id: &str,
    expected_group_id: &str,
    now: asterism_domain::Timestamp,
) -> ProviderResult<()> {
    let snapshot = parse_group_progress(document, expected_unit_id, expected_group_id)?;
    if snapshot.tab_type() != Some("task") {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI answer submission requires a fresh task progress leaf",
        ));
    }
    if snapshot.is_completed() {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI answer submission refuses to mutate an already-completed Group",
        ));
    }
    if !snapshot.mutation_available_at(now) {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI Group is outside its fresh donor availability window",
        ));
    }
    Ok(())
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

    const PROGRESS: &str = include_str!("../../../fixtures/providers/uai/progress/unit-mixed.json");

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
    fn captured_browser_cookie_and_donor_client_headers_are_ucontent_only() {
        let session = UaiJwtSession::try_new("open-id", "HEADER.PAYLOAD.SIGNATURE")
            .unwrap()
            .attach_school_header(Some(asterism_secrets::SecretString::new(
                "school-42".to_owned(),
            )))
            .unwrap()
            .attach_browser_cookie(Some(asterism_secrets::SecretString::new(
                "session=synthetic; csrf=synthetic".to_owned(),
            )))
            .unwrap();
        let ucontent = ucontent_session_headers(&session).unwrap();
        assert!(ucontent[COOKIE].is_sensitive());
        assert_eq!(ucontent["u-openid"].to_str().unwrap(), "open-id");
        assert_eq!(ucontent["x-csrftoken"].to_str().unwrap(), "open-id");
        assert_eq!(ucontent["u-app-id"], "39");
        assert_eq!(ucontent["u-platform"], "2");
        assert_eq!(ucontent["appid"], "undefined");
        assert!(ucontent["u-school"].is_sensitive());
        assert_eq!(ucontent["u-school"], "school-42");

        let management = authorization_headers(&session).unwrap();
        assert!(management.contains_key(AUTHORIZATION));
        assert!(!management.contains_key(COOKIE));
        assert!(!management.contains_key("u-openid"));
        assert!(!management.contains_key("u-school"));
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
            course_overview_progress_url("course-v2:synthetic+rw", "open-id")
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/course/api/v2/course_progress/course-v2:synthetic+rw/open-id/default"
        );
        assert_eq!(
            course_progress_url("course-v2:synthetic+rw", "unit-1", "open-id")
                .unwrap()
                .as_str(),
            "https://ucontent.unipus.cn/course/api/v2/course_progress/course-v2:synthetic+rw/unit-1/open-id/default"
        );
        assert_eq!(
            aggregate_progress_url("2001", "app-42").unwrap().as_str(),
            "https://uai.unipus.cn/api/tla/learningDetail/studyRecord/totalAndUnitSituation?id=2001&appUserId=app-42"
        );
        assert_eq!(
            course_policy_url().unwrap().as_str(),
            "https://uai.unipus.cn/api/tla/courseStudyStrategy/detail"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &build_course_policy_request_body("2001", 3001).unwrap(),
            )
            .unwrap(),
            serde_json::json!({"id": 3001, "courseResourceId": "2001"})
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
        assert_eq!(ResponseRoute::CourseProgress.maximum_bytes(), 1_024 * 1_024);
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
        assert_eq!(
            ResponseRoute::AggregateProgress.maximum_bytes(),
            1_024 * 1_024
        );
        assert_eq!(ResponseRoute::CoursePolicy.maximum_bytes(), 1_024 * 1_024);
    }

    #[test]
    fn submission_body_matches_the_audited_simple_json_shape() {
        let plan = UaiSubmissionPlan::fixture(
            "1001",
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
        assert_eq!(body["quesDatas"][0]["instanceId"], "1001");
        assert_eq!(body["quesDatas"][0]["contextVersion"], 0);
        assert_eq!(body["quesDatas"][0]["answerVersion"], 0);
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
        assert_eq!(judges[0]["versions"]["course"], 0);
        assert_eq!(judges[0]["versions"]["answer"], 0);
    }

    #[test]
    fn submission_body_uses_the_fresh_current_course_publish_version() {
        let plan = UaiSubmissionPlan::fixture_current(
            "1001",
            "multichoice",
            vec![vec!["A".to_owned()]],
            123_290,
        );
        let body = build_submission_body(
            "course-v2:synthetic+rw",
            "synthetic-open-id",
            "group-1",
            &plan,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        let judges: serde_json::Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();

        assert_eq!(judges[0]["versions"]["course"], 123_290);
        assert_eq!(judges[0]["versions"]["answer"], 3);
    }

    #[test]
    fn video_popup_answer_submission_uses_the_donor_study_submit_type() {
        let plan = UaiSubmissionPlan::fixture("2101", "video-popup", vec![vec!["A".to_owned()]]);
        let body = build_submission_body(
            "course-v2:synthetic+rw",
            "synthetic-open-id",
            "group-video-popup",
            &plan,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(body["submitType"], 2);
        assert_eq!(body["quesDatas"][0]["instanceId"], "2101");
        assert_eq!(body["isCompleted"], serde_json::json!([true]));
    }

    #[test]
    fn submission_body_preserves_multiple_module_and_child_order() {
        let plan = UaiSubmissionPlan::fixture_multiple(vec![
            (
                "1001",
                "multichoice",
                vec![vec!["A".to_owned(), "B".to_owned()]],
                vec![("basic", "multichoice")],
            ),
            (
                "1002",
                "short_answer",
                vec![vec!["first".to_owned()], vec!["second".to_owned()]],
                vec![("basic", "text-area"), ("basic", "text-area")],
            ),
        ]);
        let body = build_submission_body(
            "course-v2:synthetic+rw",
            "synthetic-open-id",
            "group-1",
            &plan,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["quesDatas"].as_array().unwrap().len(), 2);
        assert_eq!(body["quesDatas"][0]["instanceId"], "1001");
        assert_eq!(body["quesDatas"][1]["instanceId"], "1002");
        assert_eq!(body["quesDatas"][0]["contextVersion"], 1);
        assert_eq!(body["quesDatas"][0]["answerVersion"], 1);
        assert_eq!(body["quesDatas"][1]["contextVersion"], 1);
        assert_eq!(body["quesDatas"][1]["answerVersion"], 1);
        let second_answer: serde_json::Value =
            serde_json::from_str(body["quesDatas"][1]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(second_answer["children"][0]["value"][0], "first");
        assert_eq!(second_answer["children"][1]["value"][0], "second");
        assert_eq!(body["isCompleted"], serde_json::json!([true, true, true]));
        let judges: serde_json::Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges.as_array().unwrap().len(), 3);
        assert_eq!(judges[0]["question_type"], "basic");
        assert_eq!(judges[0]["reply_type"], "multichoice");
        assert_eq!(judges[1]["question_type"], "basic");
        assert_eq!(judges[1]["reply_type"], "text-area");
        assert_eq!(judges[2]["value"], "second");
    }

    #[test]
    fn preset_submission_body_matches_current_and_mit_donors() {
        let body =
            build_preset_submission_body("course-v2:synthetic+rw", "synthetic-open-id", "group-1")
                .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(body["quesDatas"], serde_json::json!([]));
        assert_eq!(body["groupId"], "group-1");
        assert_eq!(body["isCompleted"], serde_json::json!([]));
        assert_eq!(body["thirdPartyJudges"], "[]");
        assert_eq!(body["submitType"], 2);
        assert_eq!(body["hideLoading"], true);
        assert_eq!(body["associationGroupId"], "");
        assert_eq!(body["courseId"], "course-v2:synthetic+rw");
        assert_eq!(body["openId"], "synthetic-open-id");
        assert_eq!(body["version"], "default");
        assert_eq!(body.as_object().unwrap().len(), 10);
    }

    #[test]
    fn oral_empty_body_preserves_the_donor_placeholder_answer_shape() {
        let body = build_oral_empty_submission_body(
            "course-v2:synthetic+rw",
            "synthetic-open-id",
            "group-1",
            "oral-sentence",
            2,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["submitType"], 1);
        assert_eq!(body["hideLoading"], false);
        assert_eq!(body["quesDatas"].as_array().unwrap().len(), 2);
        assert_eq!(body["isCompleted"], serde_json::json!([true, true]));
        for question in body["quesDatas"].as_array().unwrap() {
            assert_eq!(question["instanceId"], "0");
            let answer: serde_json::Value =
                serde_json::from_str(question["answer"].as_str().unwrap()).unwrap();
            assert_eq!(answer["children"][0]["value"], serde_json::json!([""]));
        }
        let judges: serde_json::Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges.as_array().unwrap().len(), 2);
        assert_eq!(judges[0]["question_type"], "oral-sentence");
        assert_eq!(judges[0]["reply_type"], "oral-sentence");
        assert!(
            build_oral_empty_submission_body("course", "openid", "group", "discussion", 1,)
                .is_err()
        );
    }

    #[test]
    fn preset_preflight_requires_fresh_text_or_video_leaf() {
        let within_window = chrono::DateTime::from_timestamp(1_786_752_000, 0).unwrap();
        assert!(
            validate_empty_completion_progress_target_at(
                PROGRESS,
                "unit-1",
                "group-1",
                EmptyCompletionPreflight::Preset,
                within_window,
            )
            .unwrap()
        );
        assert!(
            !validate_empty_completion_progress_target_at(
                PROGRESS,
                "unit-1",
                "group-2",
                EmptyCompletionPreflight::Preset,
                within_window,
            )
            .unwrap()
        );
        assert!(
            validate_empty_completion_progress_target_at(
                PROGRESS,
                "other-unit",
                "group-1",
                EmptyCompletionPreflight::Preset,
                within_window,
            )
            .is_err()
        );
        assert!(
            validate_empty_completion_progress_target_at(
                PROGRESS,
                "unit-1",
                "missing",
                EmptyCompletionPreflight::Preset,
                within_window,
            )
            .is_err()
        );
        let task = PROGRESS.replacen("\"tab_type\": \"text\"", "\"tab_type\": \"task\"", 1);
        assert!(
            validate_empty_completion_progress_target_at(
                &task,
                "unit-1",
                "group-1",
                EmptyCompletionPreflight::Preset,
                within_window,
            )
            .is_err()
        );
        assert!(
            validate_empty_completion_progress_target_at(
                &task,
                "unit-1",
                "group-1",
                EmptyCompletionPreflight::ExitTicket,
                within_window,
            )
            .unwrap()
        );
        assert!(
            validate_empty_completion_progress_target_at(
                &task,
                "unit-1",
                "group-1",
                EmptyCompletionPreflight::Oral,
                within_window,
            )
            .unwrap()
        );
        let before_window = chrono::DateTime::from_timestamp(1_784_678_400, 0).unwrap();
        assert!(
            validate_empty_completion_progress_target_at(
                &task,
                "unit-1",
                "group-1",
                EmptyCompletionPreflight::ExitTicket,
                before_window,
            )
            .is_err()
        );
    }

    #[test]
    fn answer_submission_preflight_requires_fresh_available_incomplete_task_leaf() {
        let within_window = chrono::DateTime::from_timestamp(1_786_752_000, 0).unwrap();
        let task = PROGRESS.replacen("\"tab_type\": \"video\"", "\"tab_type\": \"task\"", 1);
        validate_answer_submission_progress_target(&task, "unit-1", "group-2", within_window)
            .unwrap();

        assert!(
            validate_answer_submission_progress_target(
                PROGRESS,
                "unit-1",
                "group-2",
                within_window,
            )
            .is_err()
        );
        let completed_task =
            PROGRESS.replacen("\"tab_type\": \"text\"", "\"tab_type\": \"task\"", 1);
        assert!(
            validate_answer_submission_progress_target(
                &completed_task,
                "unit-1",
                "group-1",
                within_window,
            )
            .is_err()
        );
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
