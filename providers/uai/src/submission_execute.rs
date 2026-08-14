use std::{fmt, sync::Arc};

use asterism_domain::{
    NormalizedAnswer, ProviderAccountId, ProviderId, QuestionKind, SubmissionDraft,
    SubmissionReceipt, TaskCapability,
};
use asterism_provider_api::{
    AmbiguousProviderQuestionSessionOperation, ExecutionEventSink,
    PreparedProviderSubmissionOperation, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderResult, ProviderRuntimeSettingsSchema,
    ProviderSubmissionStepOutcome, ResolvedProviderQuestionSessionContinuation,
    ResolvedProviderRuntimeSettings, SubmissionBuildCapability, SubmissionExecuteCapability,
    TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Url;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UAI_QUESTION_SET_ARTIFACT_PHASE, UAI_QUESTION_SET_ARTIFACT_TYPE, UaiQuestionArtifactSet,
    UaiSubmissionBuild,
    encrypted::ZeroizingJsonValue,
    metadata::development_metadata,
    runtime_settings::runtime_settings_schema,
    submission_build::composite_selections_bound,
    task_type::{question_kind_matches_task_type, supports_audited_question_type},
};

const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_ANSWER_CHILDREN: usize = 256;
const MAX_ANSWER_VALUES_PER_CHILD: usize = 256;
const MAX_QUESTIONS_PER_SUBMISSION: usize = 5_000;
const MAX_JUDGE_TYPE_BYTES: usize = 128;
const MAX_SUBMISSION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_SUBMISSION_REQUEST_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_SUBMISSION_VERSION_BYTES: usize = 128;
const MAX_COURSE_INSTANCE_ID_BYTES: usize = 512;
const MAX_OPEN_ID_BYTES: usize = 8 * 1_024;
const UAI_SUBMISSION_ROUTE: &str = "https://ucontent.unipus.cn/course/api/v3/newExploration/submit";
const UAI_SUBMISSION_CONTENT_TYPE: &str = "application/json; charset=utf-8";
pub(crate) const UAI_SUBMISSION_OPERATION_TYPE: &str = "uai.answer-submit.v1";

#[cfg(test)]
type UaiSubmissionFixture<'a> = (&'a str, &'a str, Vec<Vec<String>>, Vec<(&'a str, &'a str)>);

/// Redacted ownership wrapper for one bounded UAI submit acknowledgement.
pub struct UaiSubmissionResponseDocument(String);

impl UaiSubmissionResponseDocument {
    /// Owns one bounded JSON response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized documents.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_SUBMISSION_RESPONSE_BYTES {
            return Err(invalid_response(
                "UAI submission response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiSubmissionResponseDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionResponseDocument")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiSubmissionResponseDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Parses one submit acknowledgement without treating it as completion.
///
/// # Errors
///
/// Returns typed rate-limit, invalid-response, or protocol-drift errors for
/// non-success codes and identity/version drift.
pub fn parse_submission_receipt(
    document: &str,
    expected_course_instance_id: &str,
    expected_group_id: &str,
) -> ProviderResult<SubmissionReceipt> {
    if document.is_empty() || document.len() > MAX_SUBMISSION_RESPONSE_BYTES {
        return Err(invalid_response(
            "UAI submission response is empty or exceeds the size limit",
        ));
    }
    let response = ZeroizingJsonValue::new(
        serde_json::from_str(document)
            .map_err(|_| invalid_response("UAI submission response is not valid JSON"))?,
    );
    let response = response
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI submission response is not an object"))?;
    let code = response
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| protocol_drift("UAI submission response has no numeric code"))?;
    if matches!(code, 600_001 | 600_002) {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "UAI rate limited the submission mutation",
        );
        error.provider_code = Some(code.to_string());
        error.retry_after_seconds = Some(120);
        return Err(error);
    }
    if code != 0 {
        let mut error = invalid_response("UAI rejected the submission mutation");
        error.provider_code = Some(code.to_string());
        return Err(error);
    }
    let data = response
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI accepted submission has no data object"))?;
    if data.get("course_id").and_then(Value::as_str) != Some(expected_course_instance_id)
        || data.get("group_id").and_then(Value::as_str) != Some(expected_group_id)
    {
        return Err(remote_changed(
            "UAI submission acknowledgement identity does not match the mutation",
        ));
    }
    let version = data
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| valid_submission_version(value))
        .ok_or_else(|| {
            protocol_drift("UAI submission acknowledgement has no bounded verification version")
        })?;
    let receipt = SubmissionReceipt {
        remote_status: "accepted".to_owned(),
        message_sanitized: Some("UAI accepted the submission for later verification".to_owned()),
        provider_trace_id: Some(version.to_owned()),
        received_at: Utc::now(),
    };
    receipt
        .validate()
        .map_err(|_| invalid_response("UAI submission receipt is invalid"))?;
    Ok(receipt)
}

/// One ordered native module inside an ephemeral UAI submission plan.
pub struct UaiSubmissionQuestionPlan {
    remote_question_id: String,
    task_type: String,
    answer_children: Vec<Vec<String>>,
    judges: Vec<UaiSubmissionJudgePlan>,
}

/// One bounded native judge descriptor aligned with one answer child.
pub struct UaiSubmissionJudgePlan {
    question_type: String,
    reply_type: String,
}

impl UaiSubmissionQuestionPlan {
    fn from_draft_item(
        item: &asterism_domain::SubmissionDraftItem,
        task_type: &str,
        expected_position: u32,
    ) -> ProviderResult<Self> {
        if item.question.position != expected_position {
            return Err(invalid_input(
                "UAI submission execution requires Questions in exact remote order",
            ));
        }
        let remote_question_id = item
            .question
            .remote_question_id
            .as_deref()
            .filter(|value| valid_submission_question_identity(value))
            .ok_or_else(|| {
                unsupported("UAI submission execution requires a donor-audited numeric instance ID")
            })?
            .to_owned();
        let answer_children = submission_answer_children(item)?;
        if answer_children.is_empty()
            || answer_children.len() > MAX_ANSWER_CHILDREN
            || answer_children
                .iter()
                .any(|values| values.is_empty() || values.len() > MAX_ANSWER_VALUES_PER_CHILD)
        {
            return Err(invalid_input(
                "UAI submission execution has an invalid bounded answer shape",
            ));
        }
        let judges = submission_judges(item, answer_children.len())?;
        if !supports_audited_question_type(task_type) {
            return Err(unsupported(
                "UAI submission execution does not support this Group task type",
            ));
        }
        if !question_kind_matches_task_type(item.question.kind, task_type)
            || item
                .question
                .metadata_sanitized
                .get("schema")
                .and_then(|value| value.as_str())
                != Some("uai.encrypted-question.v1")
            || item
                .question
                .metadata_sanitized
                .get("task_type")
                .and_then(|value| value.as_str())
                != Some(task_type)
        {
            return Err(remote_changed(
                "UAI submission execution Question type changed after draft construction",
            ));
        }
        Ok(Self {
            remote_question_id,
            task_type: task_type.to_owned(),
            answer_children,
            judges,
        })
    }

    #[must_use]
    pub fn remote_question_id(&self) -> &str {
        &self.remote_question_id
    }

    #[must_use]
    pub fn task_type(&self) -> &str {
        &self.task_type
    }

    #[must_use]
    pub fn answer_children(&self) -> &[Vec<String>] {
        &self.answer_children
    }

    #[must_use]
    pub fn judges(&self) -> &[UaiSubmissionJudgePlan] {
        &self.judges
    }
}

fn submission_answer_children(
    item: &asterism_domain::SubmissionDraftItem,
) -> ProviderResult<Vec<Vec<String>>> {
    match (item.question.kind, &item.selected.answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
            if values.len() == 1 && selections_exist(&item.question, values) =>
        {
            Ok(vec![values.clone()])
        }
        (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values))
            if selections_exist(&item.question, values) =>
        {
            Ok(vec![values.clone()])
        }
        (QuestionKind::ShortAnswer | QuestionKind::FillBlank, NormalizedAnswer::Texts(values)) => {
            Ok(values.iter().map(|value| vec![value.clone()]).collect())
        }
        (QuestionKind::Ordering, NormalizedAnswer::Ordering(values)) => {
            ordering_answer_children(item, values)
        }
        (QuestionKind::Matching, NormalizedAnswer::Pairs(values)) => {
            matching_answer_children(item, values)
        }
        (QuestionKind::Composite, NormalizedAnswer::Composite(values))
            if composite_selections_bound(&item.question, values) =>
        {
            values
                .iter()
                .map(|value| match value {
                    NormalizedAnswer::Selections(selections) => Ok(selections.clone()),
                    _ => Err(invalid_input(
                        "UAI composite submission child is not a selection",
                    )),
                })
                .collect()
        }
        _ => Err(invalid_input(
            "UAI submission execution answer does not match its Question kind",
        )),
    }
}

fn ordering_answer_children(
    item: &asterism_domain::SubmissionDraftItem,
    values: &[String],
) -> ProviderResult<Vec<Vec<String>>> {
    let child_count = item
        .question
        .metadata_sanitized
        .get("judge_types")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            invalid_input("UAI ordering submission requires current child judge metadata")
        })?;
    if child_count == 1 {
        Ok(vec![values.to_vec()])
    } else if child_count == values.len() {
        Ok(values.iter().map(|value| vec![value.clone()]).collect())
    } else {
        Err(invalid_input(
            "UAI ordering answer cardinality differs from current child metadata",
        ))
    }
}

fn matching_answer_children(
    item: &asterism_domain::SubmissionDraftItem,
    values: &[asterism_domain::AnswerPair],
) -> ProviderResult<Vec<Vec<String>>> {
    item.question
        .metadata_sanitized
        .get("matching_lefts")
        .and_then(serde_json::Value::as_array)
        .filter(|lefts| lefts.len() == values.len())
        .ok_or_else(|| invalid_input("UAI matching submission has no exact bound left values"))?
        .iter()
        .map(|left| {
            let left = left.as_str().ok_or_else(|| {
                invalid_input("UAI matching submission has an invalid left value")
            })?;
            let mut matches = values.iter().filter(|pair| pair.left == left);
            let pair = matches
                .next()
                .ok_or_else(|| invalid_input("UAI matching submission is missing a bound pair"))?;
            if matches.next().is_some() {
                return Err(invalid_input(
                    "UAI matching submission contains duplicate bound pairs",
                ));
            }
            Ok(vec![pair.right.clone()])
        })
        .collect()
}

impl UaiSubmissionJudgePlan {
    #[must_use]
    pub fn question_type(&self) -> &str {
        &self.question_type
    }

    #[must_use]
    pub fn reply_type(&self) -> &str {
        &self.reply_type
    }
}

impl fmt::Debug for UaiSubmissionJudgePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionJudgePlan")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiSubmissionJudgePlan {
    fn drop(&mut self) {
        self.question_type.zeroize();
        self.reply_type.zeroize();
    }
}

impl fmt::Debug for UaiSubmissionQuestionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionQuestionPlan")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiSubmissionQuestionPlan {
    fn drop(&mut self) {
        self.remote_question_id.zeroize();
        self.task_type.zeroize();
        for child in &mut self.answer_children {
            child.zeroize();
        }
        self.answer_children.zeroize();
    }
}

/// Ephemeral executable facts rebuilt from one immutable UAI draft. Debug
/// output is redacted and all owned identities, types and answers are zeroized
/// on drop.
pub struct UaiSubmissionPlan {
    questions: Vec<UaiSubmissionQuestionPlan>,
    protocol_versions: UaiSubmissionProtocolVersions,
}

/// Complete zeroizing request for one ordinary answer-bearing mutation.
pub struct UaiSubmissionRequest {
    url: Zeroizing<String>,
    content_type: &'static str,
    body: Zeroizing<String>,
    request_digest: [u8; 32],
}

impl UaiSubmissionRequest {
    /// Exact pre-dispatch identity over method, route, content type and body.
    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub(crate) fn expose_url(&self) -> &str {
        self.url.as_str()
    }

    pub(crate) const fn content_type(&self) -> &'static str {
        self.content_type
    }

    pub(crate) fn expose_body(&self) -> &str {
        self.body.as_str()
    }
}

impl fmt::Debug for UaiSubmissionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionRequest")
            .field("url", &"[ROUTE]")
            .field("content_type", &self.content_type)
            .field("body", &"[REDACTED]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

impl Drop for UaiSubmissionRequest {
    fn drop(&mut self) {
        self.request_digest.zeroize();
    }
}

/// Donor-observed `thirdPartyJudges[].versions` protocol material. A fresh
/// Course publish version selects the current donor's `answer=3` shape; an
/// absent publish version retains the independently evidenced MIT `0/0`
/// compatibility shape. Arbitrary mixed values are not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UaiSubmissionProtocolVersions {
    course: u64,
    answer: u64,
}

impl UaiSubmissionProtocolVersions {
    const fn legacy() -> Self {
        Self {
            course: 0,
            answer: 0,
        }
    }

    fn current(course_publish_version: u64) -> ProviderResult<Self> {
        const MAX_SIGNED_64_BIT_VALUE: u64 = 9_223_372_036_854_775_807;

        if (1..=MAX_SIGNED_64_BIT_VALUE).contains(&course_publish_version) {
            Ok(Self {
                course: course_publish_version,
                answer: 3,
            })
        } else {
            Err(remote_changed(
                "UAI fresh Course publish version is invalid",
            ))
        }
    }

    #[must_use]
    pub const fn course(&self) -> u64 {
        self.course
    }

    #[must_use]
    pub const fn answer(&self) -> u64 {
        self.answer
    }
}

impl UaiSubmissionPlan {
    pub(crate) fn from_draft(
        draft: &SubmissionDraft,
        task_types: &[String],
    ) -> ProviderResult<Self> {
        Self::from_draft_with_versions(draft, task_types, UaiSubmissionProtocolVersions::legacy())
    }

    fn from_draft_with_versions(
        draft: &SubmissionDraft,
        task_types: &[String],
        protocol_versions: UaiSubmissionProtocolVersions,
    ) -> ProviderResult<Self> {
        if draft.items.is_empty() || draft.items.len() > MAX_QUESTIONS_PER_SUBMISSION {
            return Err(unsupported(
                "UAI submission execution requires a bounded non-empty Question set",
            ));
        }
        if task_types.is_empty()
            || (task_types.len() != 1 && task_types.len() != draft.items.len())
            || task_types
                .iter()
                .any(|value| !supports_audited_question_type(value))
        {
            return Err(unsupported(
                "UAI submission execution requires one shared or one-per-Question audited type",
            ));
        }
        let questions = draft
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let task_type = task_types.get(index).unwrap_or(&task_types[0]);
                let expected_position = u32::try_from(index + 1).map_err(|_| {
                    invalid_input("UAI submission execution Question position exceeds the limit")
                })?;
                UaiSubmissionQuestionPlan::from_draft_item(item, task_type, expected_position)
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        Ok(Self {
            questions,
            protocol_versions,
        })
    }

    pub(crate) fn from_single_draft_current(
        draft: &SubmissionDraft,
        task_type: &str,
        course_publish_version: u64,
    ) -> ProviderResult<Self> {
        Self::from_draft_with_versions(
            draft,
            &[task_type.to_owned()],
            UaiSubmissionProtocolVersions::current(course_publish_version)?,
        )
    }

    #[must_use]
    pub fn questions(&self) -> &[UaiSubmissionQuestionPlan] {
        &self.questions
    }

    #[must_use]
    pub const fn protocol_versions(&self) -> UaiSubmissionProtocolVersions {
        self.protocol_versions
    }

    /// Donor `video-popup` is answer-bearing but remains a study-mode submit.
    /// Mixed ordinary/video groups use the ordinary submit type, matching the
    /// donor rule that every question type must be a study mode before type 2
    /// is selected.
    #[must_use]
    pub fn submit_type(&self) -> u8 {
        if self
            .questions
            .iter()
            .all(|question| question.task_type == "video-popup")
        {
            2
        } else {
            1
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        remote_question_id: &str,
        task_type: &str,
        answer_children: Vec<Vec<String>>,
    ) -> Self {
        let judges = (0..answer_children.len())
            .map(|_| UaiSubmissionJudgePlan {
                question_type: task_type.replace('_', "-"),
                reply_type: "objective".to_owned(),
            })
            .collect();
        Self {
            protocol_versions: UaiSubmissionProtocolVersions::legacy(),
            questions: vec![UaiSubmissionQuestionPlan {
                remote_question_id: remote_question_id.to_owned(),
                task_type: task_type.to_owned(),
                answer_children,
                judges,
            }],
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture_current(
        remote_question_id: &str,
        task_type: &str,
        answer_children: Vec<Vec<String>>,
        course_publish_version: u64,
    ) -> Self {
        let mut plan = Self::fixture(remote_question_id, task_type, answer_children);
        plan.protocol_versions = UaiSubmissionProtocolVersions::current(course_publish_version)
            .expect("synthetic current Course publish version must be valid");
        plan
    }

    #[cfg(test)]
    pub(crate) fn fixture_multiple(questions: Vec<UaiSubmissionFixture<'_>>) -> Self {
        Self {
            protocol_versions: UaiSubmissionProtocolVersions::legacy(),
            questions: questions
                .into_iter()
                .map(|(remote_question_id, task_type, answer_children, judges)| {
                    UaiSubmissionQuestionPlan {
                        remote_question_id: remote_question_id.to_owned(),
                        task_type: task_type.to_owned(),
                        answer_children,
                        judges: judges
                            .into_iter()
                            .map(|(question_type, reply_type)| UaiSubmissionJudgePlan {
                                question_type: question_type.to_owned(),
                                reply_type: reply_type.to_owned(),
                            })
                            .collect(),
                    }
                })
                .collect(),
        }
    }
}

/// Materializes the exact answer-bearing request only after fresh route and
/// account rebinding.
///
/// # Errors
///
/// Rejects malformed route/account identities, invalid plan cardinality or an
/// oversized body before any remote mutation can occur.
pub fn build_submission_request(
    course_instance_id: &str,
    open_id: &str,
    group_id: &str,
    plan: &UaiSubmissionPlan,
) -> ProviderResult<UaiSubmissionRequest> {
    if !valid_request_text(course_instance_id, MAX_COURSE_INSTANCE_ID_BYTES)
        || !valid_request_text(open_id, MAX_OPEN_ID_BYTES)
        || valid_component(Some(group_id)).is_err()
    {
        return Err(invalid_input("UAI submission request identity is invalid"));
    }
    let body = build_submission_request_body(course_instance_id, open_id, group_id, plan)?;
    bind_submission_request_body(body)
}

/// Binds one already validated UAI submission body to the exact common POST
/// route and content type. This is shared by ordinary answer submission and
/// the independently authorized `ResourceExecution` wire families.
pub(crate) fn bind_submission_request_body(
    body: Zeroizing<String>,
) -> ProviderResult<UaiSubmissionRequest> {
    if body.is_empty() || body.len() > MAX_SUBMISSION_REQUEST_BYTES {
        return Err(invalid_input(
            "UAI submission request body is empty or exceeds the size limit",
        ));
    }
    let url = Url::parse(UAI_SUBMISSION_ROUTE)
        .map_err(|_| invalid_response("UAI submission route is invalid"))?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:submission-request:v1\0");
    digest.update(b"POST\0");
    digest.update(url.as_str().as_bytes());
    digest.update(b"\0content-type\0");
    digest.update(UAI_SUBMISSION_CONTENT_TYPE.as_bytes());
    digest.update(b"\0body\0");
    digest.update(body.as_bytes());
    Ok(UaiSubmissionRequest {
        url: Zeroizing::new(url.into()),
        content_type: UAI_SUBMISSION_CONTENT_TYPE,
        body,
        request_digest: digest.finalize().into(),
    })
}

fn build_submission_request_body(
    course_instance_id: &str,
    open_id: &str,
    group_id: &str,
    plan: &UaiSubmissionPlan,
) -> ProviderResult<Zeroizing<String>> {
    let (context_version, answer_version) = if plan.questions().len() == 1 {
        (0, 0)
    } else {
        (1, 1)
    };
    let protocol_versions = plan.protocol_versions();
    let mut question_data =
        ZeroizingJsonValue::new(Value::Array(Vec::with_capacity(plan.questions().len())));
    let mut is_completed = Vec::new();
    let mut judges = ZeroizingJsonValue::new(Value::Array(Vec::new()));
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
                .map_err(|_| invalid_response("UAI submission answer cannot be serialized"))?,
        );
        question_data
            .as_value_mut()
            .as_array_mut()
            .ok_or_else(|| invalid_response("UAI submission Question buffer is invalid"))?
            .push(serde_json::json!({
                "instanceId": question.remote_question_id(),
                "answer": inner_answer.as_str(),
                "context": "{\"state\":\"submitted\"}",
                "contextVersion": context_version,
                "answerVersion": answer_version,
            }));
        if question.judges().len() != question.answer_children().len() {
            return Err(protocol_drift(
                "UAI submission judge cardinality changed during request materialization",
            ));
        }
        for (values, judge) in question.answer_children().iter().zip(question.judges()) {
            is_completed.push(true);
            judges
                .as_value_mut()
                .as_array_mut()
                .ok_or_else(|| invalid_response("UAI submission judge buffer is invalid"))?
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
        serde_json::to_string(judges.as_value())
            .map_err(|_| invalid_response("UAI submission judges cannot be serialized"))?,
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
    let encoded = Zeroizing::new(
        serde_json::to_string(body.as_value())
            .map_err(|_| invalid_response("UAI submission request cannot be serialized"))?,
    );
    if encoded.is_empty() || encoded.len() > MAX_SUBMISSION_REQUEST_BYTES {
        return Err(invalid_response(
            "UAI submission request body exceeds the size limit",
        ));
    }
    Ok(encoded)
}

fn submission_judges(
    item: &asterism_domain::SubmissionDraftItem,
    expected_children: usize,
) -> ProviderResult<Vec<UaiSubmissionJudgePlan>> {
    let judges = item
        .question
        .metadata_sanitized
        .get("judge_types")
        .and_then(serde_json::Value::as_array)
        .filter(|values| values.len() == expected_children)
        .ok_or_else(|| unsupported("UAI submission requires exact current child judge metadata"))?;
    judges
        .iter()
        .map(|value| {
            let value = value
                .as_object()
                .filter(|value| value.len() == 2)
                .ok_or_else(|| unsupported("UAI submission child judge metadata is malformed"))?;
            let question_type = value
                .get("question_type")
                .and_then(serde_json::Value::as_str)
                .filter(|value| valid_judge_type(value))
                .ok_or_else(|| unsupported("UAI submission child Question type is invalid"))?;
            let reply_type = value
                .get("reply_type")
                .and_then(serde_json::Value::as_str)
                .filter(|value| valid_judge_type(value))
                .ok_or_else(|| unsupported("UAI submission child reply type is invalid"))?;
            Ok(UaiSubmissionJudgePlan {
                question_type: question_type.to_owned(),
                reply_type: reply_type.to_owned(),
            })
        })
        .collect()
}

fn valid_judge_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_JUDGE_TYPE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl fmt::Debug for UaiSubmissionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionPlan")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Mutation boundary for one UAI submission attempt. Implementations must not
/// retry ambiguous network failures; only a definite Authentication rejection
/// may be renewed and retried before returning.
#[async_trait]
pub trait UaiSubmissionTransport: Send + Sync {
    async fn submit(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
        plan: &UaiSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt>;

    /// Freezes the exact native mutation after all read-only route, account and
    /// progress rebinding has completed. The returned operation must not send
    /// the mutation until Core has persisted its request digest.
    async fn prepare_submission(
        &self,
        _context: &ProviderContext,
        _course_resource_id: &str,
        _unit_id: &str,
        _group_id: &str,
        _plan: &UaiSubmissionPlan,
    ) -> ProviderResult<Box<dyn PreparedProviderSubmissionOperation>> {
        Err(unsupported(
            "UAI submission transport does not support durable session execution",
        ))
    }
}

/// Independent UAI remote mutation. It returns only an acknowledgement receipt;
/// completion remains exclusively owned by `SubmissionVerify`.
pub struct UaiSubmissionExecute {
    metadata: ProviderMetadata,
    runtime_settings: ProviderRuntimeSettingsSchema,
    details: Arc<dyn TaskDetailCapability>,
    preview: UaiSubmissionBuild,
    transport: Arc<dyn UaiSubmissionTransport>,
}

impl UaiSubmissionExecute {
    /// Builds the execution capability around fresh Task detail and mutation
    /// boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn UaiSubmissionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            runtime_settings: runtime_settings_schema(),
            details,
            preview: UaiSubmissionBuild::try_new()?,
            transport,
        })
    }

    async fn validate_draft(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<GroupIdentity> {
        validate_context(context, &self.metadata)?;
        self.runtime_settings
            .validate_resolved(runtime_settings)
            .map_err(|_| internal("UAI submission execution settings snapshot is invalid"))?;
        let identity = GroupIdentity::parse(remote_task_id)?;
        if draft.provider_id != self.metadata.id
            || draft.provider_version != self.metadata.implementation_version
            || draft.validate().is_err()
        {
            return Err(invalid_input(
                "UAI submission execution received an invalid or stale draft",
            ));
        }
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = draft
            .items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        let expected_preview = self
            .preview
            .build_submission_preview(context, remote_task_id, &questions, &selected)
            .await?;
        if expected_preview != draft.payload_preview {
            return Err(invalid_input(
                "UAI submission execution draft preview is stale or foreign",
            ));
        }
        Ok(identity)
    }

    async fn fresh_plan(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        identity: &GroupIdentity,
    ) -> ProviderResult<UaiSubmissionPlan> {
        let detail = self.details.task_detail(context, remote_task_id).await?;
        let fresh = validate_fresh_detail(&detail, identity, remote_task_id, draft.items.len())?;
        UaiSubmissionPlan::from_draft_with_versions(
            draft,
            &fresh.task_types,
            fresh.protocol_versions,
        )
    }

    fn validate_session_continuation(
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: &ResolvedProviderQuestionSessionContinuation<'_>,
    ) -> ProviderResult<()> {
        if continuation.continuation_type != UAI_QUESTION_SET_ARTIFACT_TYPE
            || continuation.phase != UAI_QUESTION_SET_ARTIFACT_PHASE
            || continuation.revision == 0
        {
            return Err(protocol_drift(
                "UAI submission continuation metadata is stale or foreign",
            ));
        }
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        UaiQuestionArtifactSet::decode_bound(
            continuation.value,
            continuation.continuation_digest,
            remote_task_id,
            &questions,
        )?;
        Ok(())
    }
}

impl fmt::Debug for UaiSubmissionExecute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionExecute")
            .field("metadata", &self.metadata)
            .field("runtime_settings", &self.runtime_settings)
            .field("details", &"configured")
            .field("preview", &self.preview)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiSubmissionExecute {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionExecuteCapability for UaiSubmissionExecute {
    async fn execute_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        runtime_settings: &ResolvedProviderRuntimeSettings,
        _events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<SubmissionReceipt> {
        let identity = self
            .validate_draft(context, remote_task_id, draft, runtime_settings)
            .await?;
        let plan = self
            .fresh_plan(context, remote_task_id, draft, &identity)
            .await?;
        let receipt = self
            .transport
            .submit(
                context,
                &identity.course_resource,
                &identity.unit,
                &identity.group,
                &plan,
            )
            .await?;
        receipt
            .validate()
            .map_err(|_| invalid_response("UAI submission receipt is invalid"))?;
        if receipt.remote_status != "accepted"
            || receipt
                .provider_trace_id
                .as_deref()
                .is_none_or(|value| !valid_submission_version(value))
        {
            return Err(invalid_response(
                "UAI submission execution requires an accepted version receipt",
            ));
        }
        Ok(receipt)
    }

    async fn prepare_submission_operation(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<Box<dyn PreparedProviderSubmissionOperation>>> {
        let identity = self
            .validate_draft(context, remote_task_id, draft, runtime_settings)
            .await?;
        Self::validate_session_continuation(remote_task_id, draft, &continuation)?;
        let plan = self
            .fresh_plan(context, remote_task_id, draft, &identity)
            .await?;
        let operation = self
            .transport
            .prepare_submission(
                context,
                &identity.course_resource,
                &identity.unit,
                &identity.group,
                &plan,
            )
            .await?;
        if operation.operation_type() != UAI_SUBMISSION_OPERATION_TYPE
            || operation.request_digest() == [0; 32]
            || operation.delay_before_execute_seconds() != 0
        {
            return Err(invalid_response(
                "UAI submission transport prepared an invalid operation",
            ));
        }
        Ok(Some(Box::new(BoundUaiPreparedSubmissionOperation {
            provider_id: context.provider_id.clone(),
            account_id: context.account_id,
            operation,
        })))
    }

    async fn recover_ambiguous_submission_operation(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        operation: &AmbiguousProviderQuestionSessionOperation,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderSubmissionStepOutcome>> {
        let identity = self
            .validate_draft(context, remote_task_id, draft, runtime_settings)
            .await?;
        Self::validate_session_continuation(remote_task_id, draft, &continuation)?;
        if operation.continuation_revision != continuation.revision
            || operation.operation_type != UAI_SUBMISSION_OPERATION_TYPE
            || operation.request_digest == [0; 32]
            || operation.ambiguous_at < operation.issued_at
        {
            return Err(protocol_drift(
                "UAI ambiguous submission operation is stale or foreign",
            ));
        }
        let plan = self
            .fresh_plan(context, remote_task_id, draft, &identity)
            .await?;
        let fresh = self
            .transport
            .prepare_submission(
                context,
                &identity.course_resource,
                &identity.unit,
                &identity.group,
                &plan,
            )
            .await?;
        if fresh.operation_type() != UAI_SUBMISSION_OPERATION_TYPE
            || fresh.request_digest() != operation.request_digest
            || fresh.delay_before_execute_seconds() != 0
        {
            return Err(remote_changed(
                "UAI ambiguous submission no longer matches the fresh request",
            ));
        }
        Ok(None)
    }
}

struct BoundUaiPreparedSubmissionOperation {
    provider_id: ProviderId,
    account_id: ProviderAccountId,
    operation: Box<dyn PreparedProviderSubmissionOperation>,
}

impl fmt::Debug for BoundUaiPreparedSubmissionOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundUaiPreparedSubmissionOperation")
            .field("provider_id", &self.provider_id)
            .field("account_id", &self.account_id)
            .field("operation", &self.operation)
            .finish()
    }
}

#[async_trait]
impl PreparedProviderSubmissionOperation for BoundUaiPreparedSubmissionOperation {
    fn operation_type(&self) -> &str {
        self.operation.operation_type()
    }

    fn request_digest(&self) -> [u8; 32] {
        self.operation.request_digest()
    }

    fn delay_before_execute_seconds(&self) -> u64 {
        self.operation.delay_before_execute_seconds()
    }

    async fn execute(
        self: Box<Self>,
        context: &ProviderContext,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<asterism_provider_api::ProviderSubmissionStepOutcome> {
        if context.provider_id != self.provider_id || context.account_id != self.account_id {
            return Err(internal(
                "UAI prepared submission received a foreign execution context",
            ));
        }
        self.operation.execute(context, events).await
    }
}

struct FreshSubmissionShape {
    task_types: Vec<String>,
    protocol_versions: UaiSubmissionProtocolVersions,
}

fn validate_fresh_detail(
    detail: &asterism_provider_api::RemoteTaskDetail,
    identity: &GroupIdentity,
    remote_task_id: &str,
    expected_question_count: usize,
) -> ProviderResult<FreshSubmissionShape> {
    if detail.task.remote_id != remote_task_id
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionExecute)
    {
        return Err(unsupported(
            "UAI fresh Group does not advertise submission execution",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid_response("UAI fresh Group detail has no normalized Task"))?;
    if task
        .get("course_resource_id")
        .and_then(serde_json::Value::as_str)
        != Some(identity.course_resource.as_str())
        || task
            .get("unit")
            .and_then(serde_json::Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(serde_json::Value::as_str)
            != Some(identity.unit.as_str())
        || task.get("group_id").and_then(serde_json::Value::as_str) != Some(identity.group.as_str())
        || task
            .get("question_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            != Some(expected_question_count)
    {
        return Err(remote_changed(
            "UAI Group identity or Question count changed before submission",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_response("UAI fresh Group detail has no task types"))?;
    if task_types.is_empty()
        || (task_types.len() != 1 && task_types.len() != expected_question_count)
    {
        return Err(unsupported(
            "UAI submission execution requires one shared or one-per-Question task type",
        ));
    }
    let task_types = task_types
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| supports_audited_question_type(value))
                .map(str::to_owned)
                .ok_or_else(|| unsupported("UAI fresh Group task type is not executable"))
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    let protocol_versions = match task.get("course_publish_version") {
        None | Some(Value::Null) => UaiSubmissionProtocolVersions::legacy(),
        Some(Value::Number(value)) => value
            .as_u64()
            .ok_or_else(|| remote_changed("UAI fresh Course publish version is invalid"))
            .and_then(UaiSubmissionProtocolVersions::current)?,
        _ => {
            return Err(remote_changed(
                "UAI fresh Course publish version has an unsupported shape",
            ));
        }
    };
    Ok(FreshSubmissionShape {
        task_types,
        protocol_versions,
    })
}

fn selections_exist(question: &asterism_domain::Question, values: &[String]) -> bool {
    let options = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    !values.is_empty() && values.iter().all(|value| options.contains(value.as_str()))
}

struct GroupIdentity {
    course_resource: String,
    unit: String,
    group: String,
}

impl GroupIdentity {
    fn parse(value: &str) -> ProviderResult<Self> {
        if value.is_empty()
            || value.len() > MAX_REMOTE_TASK_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(invalid_input("UAI Group Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("group") {
            return Err(unsupported(
                "UAI submission execution supports Group Tasks only",
            ));
        }
        let course_resource = valid_component(components.next())?;
        let unit = valid_component(components.next())?;
        let group = valid_component(components.next())?;
        if components.next().is_some() {
            return Err(invalid_input("UAI Group Task identity is invalid"));
        }
        Ok(Self {
            course_resource,
            unit,
            group,
        })
    }
}

fn valid_component(value: Option<&str>) -> ProviderResult<String> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_REMOTE_COMPONENT_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        .map(str::to_owned)
        .ok_or_else(|| invalid_input("UAI Group Task identity is invalid"))
}

fn valid_request_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_question_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_QUESTION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_submission_question_identity(value: &str) -> bool {
    valid_question_identity(value)
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|value| value > 0)
}

pub(crate) fn valid_submission_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBMISSION_VERSION_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "UAI submission execution received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI submission execution requires an authenticated session",
        ));
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AnswerCandidateId, AnswerPair, AnswerSource, ProviderAccountId, ProviderId,
        QuestionSnapshotId, SecretId, SelectedAnswer, SubmissionAnswerCoverage, SubmissionDraftId,
        SubmissionDraftItem, TaskId,
    };
    use asterism_provider_api::{ProviderExecutionLog, ProviderProgress, RemoteTaskDetail};

    use super::*;
    use crate::{
        parse_course_context, parse_course_inventory, parse_question_content, parse_task_inventory,
    };

    const ACCEPTED: &str =
        include_str!("../../../fixtures/providers/uai/submissions/accepted.json");
    const CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-multiple-choice.json");
    const MIXED_CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-mixed-simple.json");
    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str = include_str!("../../../fixtures/providers/uai/tasks/tree-mixed.json");

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        multiple: bool,
    }

    impl FixtureDetail {
        fn single() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                multiple: false,
            }
        }

        fn multiple() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                multiple: true,
            }
        }
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let course = parse_course_inventory(COURSES)?.remove(0);
            let context = parse_course_context(&course, DETAIL)?;
            let tree = if self.multiple {
                TREE.replace(
                    r#"\"base\":\"rich-text-read\",\"question_num\":1"#,
                    r#"\"base\":\"multichoice,short_answer\",\"question_num\":2"#,
                )
            } else {
                TREE.replace("rich-text-read", "multichoice")
            };
            let task = parse_task_inventory(&course, &context, &tree)?
                .into_iter()
                .find(|task| task.remote_id == remote_task_id)
                .ok_or_else(|| protocol_drift("synthetic UAI Group is missing"))?;
            Ok(RemoteTaskDetail {
                normalized_detail: serde_json::json!({
                    "schema": "uai.group-task-detail.v1",
                    "task": task.normalized.clone(),
                }),
                task,
            })
        }
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Arc<Mutex<Vec<RecordedSubmission>>>,
    }

    type RecordedQuestion = (String, String, Vec<Vec<String>>);

    #[derive(Debug, Eq, PartialEq)]
    struct RecordedSubmission {
        course_resource_id: String,
        unit_id: String,
        group_id: String,
        protocol_versions: UaiSubmissionProtocolVersions,
        questions: Vec<RecordedQuestion>,
    }

    #[async_trait]
    impl UaiSubmissionTransport for FixtureTransport {
        async fn submit(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
            group_id: &str,
            plan: &UaiSubmissionPlan,
        ) -> ProviderResult<SubmissionReceipt> {
            self.calls.lock().unwrap().push(recorded_submission(
                course_resource_id,
                unit_id,
                group_id,
                plan,
            ));
            Ok(fixture_receipt())
        }

        async fn prepare_submission(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
            group_id: &str,
            plan: &UaiSubmissionPlan,
        ) -> ProviderResult<Box<dyn PreparedProviderSubmissionOperation>> {
            Ok(Box::new(PreparedFixtureSubmission {
                calls: self.calls.clone(),
                submission: recorded_submission(course_resource_id, unit_id, group_id, plan),
            }))
        }
    }

    struct PreparedFixtureSubmission {
        calls: Arc<Mutex<Vec<RecordedSubmission>>>,
        submission: RecordedSubmission,
    }

    impl fmt::Debug for PreparedFixtureSubmission {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("PreparedFixtureSubmission")
                .field("calls", &"configured")
                .field("submission", &self.submission)
                .finish()
        }
    }

    #[async_trait]
    impl PreparedProviderSubmissionOperation for PreparedFixtureSubmission {
        fn operation_type(&self) -> &str {
            UAI_SUBMISSION_OPERATION_TYPE
        }

        fn request_digest(&self) -> [u8; 32] {
            [41; 32]
        }

        fn delay_before_execute_seconds(&self) -> u64 {
            0
        }

        async fn execute(
            self: Box<Self>,
            _context: &ProviderContext,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<asterism_provider_api::ProviderSubmissionStepOutcome> {
            self.calls.lock().unwrap().push(self.submission);
            let receipt = fixture_receipt();
            let received_at = receipt.received_at;
            asterism_provider_api::ProviderSubmissionStepOutcome::submitted(
                receipt,
                [42; 32],
                received_at,
            )
        }
    }

    fn recorded_submission(
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
        plan: &UaiSubmissionPlan,
    ) -> RecordedSubmission {
        RecordedSubmission {
            course_resource_id: course_resource_id.to_owned(),
            unit_id: unit_id.to_owned(),
            group_id: group_id.to_owned(),
            protocol_versions: plan.protocol_versions(),
            questions: plan
                .questions()
                .iter()
                .map(|question| {
                    (
                        question.remote_question_id().to_owned(),
                        question.task_type().to_owned(),
                        question.answer_children().to_vec(),
                    )
                })
                .collect(),
        }
    }

    fn fixture_receipt() -> SubmissionReceipt {
        SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some(
                "UAI accepted the submission for later verification".to_owned(),
            ),
            provider_trace_id: Some("submit-version-42".to_owned()),
            received_at: Utc::now(),
        }
    }

    #[derive(Debug, Default)]
    struct AmbiguousFailureTransport {
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl UaiSubmissionTransport for AmbiguousFailureTransport {
        async fn submit(
            &self,
            _context: &ProviderContext,
            _course_resource_id: &str,
            _unit_id: &str,
            _group_id: &str,
            _plan: &UaiSubmissionPlan,
        ) -> ProviderResult<SubmissionReceipt> {
            *self.calls.lock().unwrap() += 1;
            Err(ProviderError::new(
                ProviderErrorKind::Network,
                "synthetic ambiguous UAI submission failure",
            ))
        }
    }

    #[derive(Debug)]
    struct UnexpectedReceiptTransport;

    #[async_trait]
    impl UaiSubmissionTransport for UnexpectedReceiptTransport {
        async fn submit(
            &self,
            _context: &ProviderContext,
            _course_resource_id: &str,
            _unit_id: &str,
            _group_id: &str,
            _plan: &UaiSubmissionPlan,
        ) -> ProviderResult<SubmissionReceipt> {
            Ok(SubmissionReceipt {
                remote_status: "queued".to_owned(),
                message_sanitized: None,
                provider_trace_id: Some("submit-version-42".to_owned()),
                received_at: Utc::now(),
            })
        }
    }

    #[derive(Debug)]
    struct NoopEvents;

    #[async_trait]
    impl ExecutionEventSink for NoopEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            Ok(())
        }

        async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
            Ok(())
        }
    }

    #[test]
    fn accepted_response_is_a_receipt_not_completion() {
        let receipt =
            parse_submission_receipt(ACCEPTED, "course-v2:synthetic+rw", "group-1").unwrap();
        assert_eq!(receipt.remote_status, "accepted");
        assert_eq!(
            receipt.provider_trace_id.as_deref(),
            Some("submit-version-42")
        );
        assert!(!format!("{receipt:?}").contains("must_be_dropped"));
        assert_eq!(
            parse_submission_receipt(
                r#"{"code":600001,"msg":"too frequent"}"#,
                "course-v2:synthetic+rw",
                "group-1",
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RateLimited
        );
        assert_eq!(
            parse_submission_receipt(ACCEPTED, "changed", "group-1")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(
            parse_submission_receipt(
                &ACCEPTED.replace("submit-version-42", "unsafe/version"),
                "course-v2:synthetic+rw",
                "group-1",
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[tokio::test]
    async fn execution_rechecks_preview_and_fresh_group_before_one_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let draft = draft().await;
        let receipt = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap();
        assert_eq!(receipt.remote_status, "accepted");
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[RecordedSubmission {
                course_resource_id: "2001".to_owned(),
                unit_id: "unit-1".to_owned(),
                group_id: "group-1".to_owned(),
                protocol_versions: UaiSubmissionProtocolVersions {
                    course: 123_290,
                    answer: 3,
                },
                questions: vec![(
                    "1001".to_owned(),
                    "multichoice".to_owned(),
                    vec![vec!["A".to_owned(), "B".to_owned()]],
                )],
            }]
        );
    }

    #[tokio::test]
    async fn multiple_question_group_executes_one_ordered_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::multiple()), transport.clone())
                .unwrap();
        let receipt = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &multiple_draft().await,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap();
        assert_eq!(receipt.remote_status, "accepted");
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[RecordedSubmission {
                course_resource_id: "2001".to_owned(),
                unit_id: "unit-1".to_owned(),
                group_id: "group-1".to_owned(),
                protocol_versions: UaiSubmissionProtocolVersions {
                    course: 123_290,
                    answer: 3,
                },
                questions: vec![
                    (
                        "1001".to_owned(),
                        "multichoice".to_owned(),
                        vec![vec!["A".to_owned(), "B".to_owned()]],
                    ),
                    (
                        "1002".to_owned(),
                        "short_answer".to_owned(),
                        vec![vec!["first".to_owned()], vec!["second".to_owned()]],
                    ),
                ],
            }]
        );
    }

    #[tokio::test]
    async fn donor_writing_draft_builds_a_bound_text_submission_plan() {
        let mut draft = multiple_draft().await;
        draft.items[1].question.metadata_sanitized["task_type"] = serde_json::json!("writing");
        let plan = UaiSubmissionPlan::from_draft(
            &draft,
            &["multichoice".to_owned(), "writing".to_owned()],
        )
        .unwrap();

        assert_eq!(plan.questions()[1].task_type(), "writing");
        assert_eq!(
            plan.questions()[1].answer_children(),
            &[vec!["first".to_owned()], vec!["second".to_owned()]]
        );
    }

    #[tokio::test]
    async fn donor_video_popup_draft_keeps_answer_children_and_study_submit_type() {
        let mut draft = draft().await;
        draft.items[0].question.metadata_sanitized["task_type"] = serde_json::json!("video-popup");
        let plan = UaiSubmissionPlan::from_draft(&draft, &["video-popup".to_owned()]).unwrap();

        assert_eq!(plan.questions()[0].task_type(), "video-popup");
        assert_eq!(
            plan.questions()[0].answer_children(),
            &[vec!["A".to_owned(), "B".to_owned()]]
        );
        assert_eq!(plan.submit_type(), 2);
    }

    #[tokio::test]
    async fn multiple_question_count_drift_fails_before_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let error = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &multiple_draft().await,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn multiple_question_missing_current_judge_metadata_fails_before_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::multiple()), transport.clone())
                .unwrap();
        let mut draft = multiple_draft().await;
        draft.items[1].question.metadata_sanitized["judge_types"] = serde_json::Value::Null;
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = draft
            .items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        draft.payload_preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                &questions,
                &selected,
            )
            .await
            .unwrap();
        let error = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn single_question_missing_current_judge_metadata_fails_before_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let mut draft = draft().await;
        draft.items[0].question.metadata_sanitized["judge_types"] = serde_json::Value::Null;
        let questions = [draft.items[0].question.clone()];
        let selected = [draft.items[0].selected.clone()];
        draft.payload_preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                &questions,
                &selected,
            )
            .await
            .unwrap();
        let error = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_preview_fails_before_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let mut draft = draft().await;
        draft.payload_preview.format = "uai.changed.v1".to_owned();
        assert!(
            capability
                .execute_submission(
                    &context(),
                    "group:2001:unit-1:group-1",
                    &draft,
                    &runtime_settings(),
                    &NoopEvents,
                )
                .await
                .is_err()
        );
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_numeric_question_identity_fails_before_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let mut draft = draft().await;
        draft.items[0].question.remote_question_id = Some("question-1".to_owned());
        let error = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ambiguous_transport_failure_is_returned_after_one_mutation_attempt() {
        let transport = Arc::new(AmbiguousFailureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let error = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft().await,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Network);
        assert_eq!(*transport.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unexpected_receipt_semantics_are_rejected_after_the_mutation() {
        let capability = UaiSubmissionExecute::try_new(
            Arc::new(FixtureDetail::single()),
            Arc::new(UnexpectedReceiptTransport),
        )
        .unwrap();
        let error = capability
            .execute_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft().await,
                &runtime_settings(),
                &NoopEvents,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[tokio::test]
    async fn media_session_freezes_digest_before_one_submission_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let context = context();
        let (draft, value, digest) = media_draft_and_artifact().await;
        let continuation = ResolvedProviderQuestionSessionContinuation {
            continuation_type: UAI_QUESTION_SET_ARTIFACT_TYPE,
            continuation_digest: digest,
            phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
            revision: 1,
            value: &value,
        };
        let operation = capability
            .prepare_submission_operation(
                &context,
                "group:2001:unit-1:group-1",
                &draft,
                continuation,
                &runtime_settings(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.operation_type(), "uai.answer-submit.v1");
        assert_eq!(operation.request_digest(), [41; 32]);
        assert!(transport.calls.lock().unwrap().is_empty());

        let outcome = operation.execute(&context, &NoopEvents).await.unwrap();
        let asterism_provider_api::ProviderSubmissionStepOutcome::Submitted {
            receipt,
            response_digest,
            ..
        } = outcome
        else {
            panic!("UAI answer submit must be a terminal session mutation");
        };
        assert_eq!(receipt.remote_status, "accepted");
        assert_eq!(response_digest, [42; 32]);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prepared_media_submission_rejects_a_foreign_account_before_send() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let owner_context = context();
        let (draft, value, digest) = media_draft_and_artifact().await;
        let operation = capability
            .prepare_submission_operation(
                &owner_context,
                "group:2001:unit-1:group-1",
                &draft,
                ResolvedProviderQuestionSessionContinuation {
                    continuation_type: UAI_QUESTION_SET_ARTIFACT_TYPE,
                    continuation_digest: digest,
                    phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
                    revision: 1,
                    value: &value,
                },
                &runtime_settings(),
            )
            .await
            .unwrap()
            .unwrap();
        let error = operation
            .execute(&context(), &NoopEvents)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Internal);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ambiguous_media_submission_rebinds_exact_request_without_replay() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let context = context();
        let (draft, value, digest) = media_draft_and_artifact().await;
        let issued_at = Utc::now();
        let operation = AmbiguousProviderQuestionSessionOperation {
            continuation_revision: 1,
            operation_type: UAI_SUBMISSION_OPERATION_TYPE.to_owned(),
            request_digest: [41; 32],
            issued_at,
            ambiguous_at: issued_at,
        };
        let recovered = capability
            .recover_ambiguous_submission_operation(
                &context,
                "group:2001:unit-1:group-1",
                &draft,
                ResolvedProviderQuestionSessionContinuation {
                    continuation_type: UAI_QUESTION_SET_ARTIFACT_TYPE,
                    continuation_digest: digest,
                    phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
                    revision: 1,
                    value: &value,
                },
                &operation,
                &runtime_settings(),
            )
            .await
            .unwrap();
        assert!(recovered.is_none());
        assert!(transport.calls.lock().unwrap().is_empty());

        let changed = AmbiguousProviderQuestionSessionOperation {
            request_digest: [43; 32],
            ..operation
        };
        let error = capability
            .recover_ambiguous_submission_operation(
                &context,
                "group:2001:unit-1:group-1",
                &draft,
                ResolvedProviderQuestionSessionContinuation {
                    continuation_type: UAI_QUESTION_SET_ARTIFACT_TYPE,
                    continuation_digest: digest,
                    phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
                    revision: 1,
                    value: &value,
                },
                &changed,
                &runtime_settings(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn foreign_media_session_fails_before_transport_preparation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            UaiSubmissionExecute::try_new(Arc::new(FixtureDetail::single()), transport.clone())
                .unwrap();
        let (draft, value, digest) = media_draft_and_artifact().await;
        let continuation = ResolvedProviderQuestionSessionContinuation {
            continuation_type: "uai.foreign-artifact.v1",
            continuation_digest: digest,
            phase: UAI_QUESTION_SET_ARTIFACT_PHASE,
            revision: 1,
            value: &value,
        };
        let error = capability
            .prepare_submission_operation(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                continuation,
                &runtime_settings(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    async fn draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let mut question = parse_question_content(
            CONTENT,
            "group:2001:unit-1:group-1",
            &["multichoice".to_owned()],
            Some(1),
        )
        .unwrap()
        .remove(0)
        .to_question(task_id)
        .unwrap();
        question.metadata_sanitized["judge_types"] = serde_json::json!([
            {"question_type": "multichoice", "reply_type": "multichoice"}
        ]);
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    async fn media_draft_and_artifact() -> (SubmissionDraft, asterism_secrets::SecretValue, [u8; 32])
    {
        let remote_task_id = "group:2001:unit-1:group-1";
        let task_id = TaskId::new();
        let parsed = crate::question::parse_question_entry(
            &serde_json::json!({
                "id": "1001",
                "content": {
                    "type": "basic",
                    "direction": {"text": "Choose every matching answer"},
                    "contents": [{
                        "name": "Listening.mp3",
                        "path": "https://media.example.edu/listening.mp3"
                    }],
                    "children": [{
                        "type": "basic",
                        "replyType": "multichoice",
                        "options": [
                            {"name": "A", "text": "Alpha"},
                            {"name": "B", "text": "Beta"}
                        ]
                    }]
                }
            }),
            1,
            "multichoice",
            remote_task_id,
        )
        .unwrap();
        let question = parsed.to_question(task_id).unwrap();
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                remote_task_id,
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let artifact = UaiQuestionArtifactSet::from_parsed_questions(
            std::slice::from_ref(&parsed),
            std::slice::from_ref(&question),
            remote_task_id,
        )
        .unwrap()
        .unwrap()
        .encode()
        .unwrap();
        let digest = artifact.digest();
        let value = artifact.into_secret_value();
        (
            SubmissionDraft {
                id: SubmissionDraftId::new(),
                task_id,
                question_snapshot_id: QuestionSnapshotId::new(),
                provider_id: ProviderId::new("uai").unwrap(),
                provider_version: development_metadata().unwrap().implementation_version,
                answer_coverage: SubmissionAnswerCoverage {
                    total_question_count: 1,
                    minimum_coverage_millis: 1_000,
                    unanswered_question_ids: Vec::new(),
                },
                items: vec![SubmissionDraftItem { question, selected }],
                payload_preview: preview,
                created_at: Utc::now(),
            },
            value,
            digest,
        )
    }

    async fn multiple_draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let questions = parse_question_content(
            MIXED_CONTENT,
            "group:2001:unit-1:group-1",
            &["multichoice".to_owned(), "short_answer".to_owned()],
            Some(2),
        )
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
        let answers = [
            NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
            NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()]),
        ];
        let items = questions
            .into_iter()
            .zip(answers)
            .map(|(question, answer)| SubmissionDraftItem {
                selected: SelectedAnswer {
                    candidate_id: AnswerCandidateId::new(),
                    question_id: question.id,
                    answer,
                    source: AnswerSource::Manual,
                    confidence: None,
                },
                question,
            })
            .collect::<Vec<_>>();
        let questions = items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                &questions,
                &selected,
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id: items[0].question.task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: u32::try_from(items.len()).unwrap(),
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items,
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn fillblank_plan_preserves_each_child_and_current_judge_type() {
        let task_id = TaskId::new();
        let question = asterism_domain::Question {
            id: asterism_domain::QuestionId::new(),
            task_id,
            remote_question_id: Some("2001".to_owned()),
            kind: QuestionKind::FillBlank,
            stem: "Fill every blank".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "material-banked-cloze",
                "remote_task_id": "group:2001:unit-1:group-fillblank",
                "judge_types": [
                    {"question_type": "material-banked-cloze", "reply_type": "bankedcloze"},
                    {"question_type": "material-banked-cloze", "reply_type": "bankedcloze"},
                ],
            }),
            position: 1,
        };
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()]),
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-fillblank",
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        };
        let plan =
            UaiSubmissionPlan::from_draft(&draft, &["material-banked-cloze".to_owned()]).unwrap();

        assert_eq!(
            plan.questions()[0].answer_children(),
            &[vec!["first".to_owned()], vec!["second".to_owned()]]
        );
        assert!(
            plan.questions()[0]
                .judges()
                .iter()
                .all(|judge| judge.question_type() == "material-banked-cloze"
                    && judge.reply_type() == "bankedcloze")
        );
    }

    #[tokio::test]
    async fn sequence_plan_preserves_order_and_child_cardinality() {
        let task_id = TaskId::new();
        let question = asterism_domain::Question {
            id: asterism_domain::QuestionId::new(),
            task_id,
            remote_question_id: Some("4001".to_owned()),
            kind: QuestionKind::Ordering,
            stem: "Put the clauses in order".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "sequence",
                "remote_task_id": "group:2001:unit-1:group-sequence",
                "judge_types": [
                    {"question_type": "sequence", "reply_type": "sequence"},
                    {"question_type": "sequence", "reply_type": "sequence"},
                ],
            }),
            position: 1,
        };
        let draft = typed_draft(
            question,
            NormalizedAnswer::Ordering(vec!["second".to_owned(), "first".to_owned()]),
            "group:2001:unit-1:group-sequence",
        )
        .await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["sequence".to_owned()]).unwrap();

        assert_eq!(
            plan.questions()[0].answer_children(),
            &[vec!["second".to_owned()], vec!["first".to_owned()]]
        );
    }

    #[tokio::test]
    async fn matching_plan_rebinds_pairs_to_question_left_order() {
        let task_id = TaskId::new();
        let question = asterism_domain::Question {
            id: asterism_domain::QuestionId::new(),
            task_id,
            remote_question_id: Some("5001".to_owned()),
            kind: QuestionKind::Matching,
            stem: "Match each expression".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "basic-scoop-content",
                "remote_task_id": "group:2001:unit-1:group-matching",
                "matching_lefts": ["first left", "second left"],
                "judge_types": [
                    {"question_type": "basic", "reply_type": "scoop"},
                    {"question_type": "basic", "reply_type": "scoop"},
                ],
            }),
            position: 1,
        };
        let draft = typed_draft(
            question,
            NormalizedAnswer::Pairs(vec![
                AnswerPair {
                    left: "second left".to_owned(),
                    right: "second right".to_owned(),
                },
                AnswerPair {
                    left: "first left".to_owned(),
                    right: "first right".to_owned(),
                },
            ]),
            "group:2001:unit-1:group-matching",
        )
        .await;
        let plan =
            UaiSubmissionPlan::from_draft(&draft, &["basic-scoop-content".to_owned()]).unwrap();

        assert_eq!(
            plan.questions()[0].answer_children(),
            &[
                vec!["first right".to_owned()],
                vec!["second right".to_owned()]
            ]
        );
    }

    #[tokio::test]
    async fn composite_choice_plan_preserves_each_child_selection() {
        let task_id = TaskId::new();
        let question = asterism_domain::Question {
            id: asterism_domain::QuestionId::new(),
            task_id,
            remote_question_id: Some("8001".to_owned()),
            kind: QuestionKind::Composite,
            stem: "Answer both parts".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "multichoice",
                "remote_task_id": "group:2001:unit-1:group-composite-choice",
                "judge_types": [
                    {"question_type": "basic", "reply_type": "singlechoice"},
                    {"question_type": "basic", "reply_type": "multichoice"},
                ],
                "composite_children": [
                    {
                        "kind": "single_choice",
                        "options": [
                            {"id":"A","content":"First A","attachments":[],"metadata_sanitized":{}},
                            {"id":"B","content":"First B","attachments":[],"metadata_sanitized":{}},
                        ],
                    },
                    {
                        "kind": "multiple_choice",
                        "options": [
                            {"id":"A","content":"Second A","attachments":[],"metadata_sanitized":{}},
                            {"id":"B","content":"Second B","attachments":[],"metadata_sanitized":{}},
                        ],
                    },
                ],
            }),
            position: 1,
        };
        let draft = typed_draft(
            question,
            NormalizedAnswer::Composite(vec![
                NormalizedAnswer::Selections(vec!["B".to_owned()]),
                NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
            ]),
            "group:2001:unit-1:group-composite-choice",
        )
        .await;
        let plan = UaiSubmissionPlan::from_draft(&draft, &["multichoice".to_owned()]).unwrap();

        assert_eq!(
            plan.questions()[0].answer_children(),
            &[vec!["B".to_owned()], vec!["A".to_owned(), "B".to_owned()]]
        );
        assert_eq!(plan.questions()[0].judges().len(), 2);
    }

    async fn typed_draft(
        question: asterism_domain::Question,
        answer: NormalizedAnswer,
        remote_task_id: &str,
    ) -> SubmissionDraft {
        let task_id = question.task_id;
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer,
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                remote_task_id,
                std::slice::from_ref(&question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    fn runtime_settings() -> ResolvedProviderRuntimeSettings {
        runtime_settings_schema().resolve(None, None, None).unwrap()
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            correlation_id: "uai-submission-execute".to_owned(),
            credential_refs: vec![SecretId::new()],
        }
    }
}
