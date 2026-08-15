use std::{fmt, sync::Arc};

use asterism_domain::{
    SubmissionDraft, SubmissionDraftId, SubmissionReceipt, SubmissionScore, Timestamp,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, SubmissionBuildCapability,
    TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Url;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UaiSubmissionBuild, UaiSubmissionPlan, UaiUploadedArtifact,
    encrypted::ZeroizingJsonValue,
    metadata::development_metadata,
    submission_execute::{UaiSubmissionQuestionPlan, valid_submission_version},
    submission_verify::{
        UaiSubmissionPolicyEvidence, bound_verification_state, parse_remote_question,
        verified_submission_policy, verified_submission_score,
    },
    upload::validate_upload_readback_question,
};

const MAX_COMPOUND_SUBMISSION_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_COMPOUND_VERIFICATION_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NESTED_QUESTION_DATA_BYTES: usize = 4 * 1_024 * 1_024;
const UAI_COMPOUND_UPLOAD_SUBMISSION_ROUTE: &str =
    "https://ucontent.unipus.cn/course/api/v3/newExploration/submit";
const UAI_COMPOUND_UPLOAD_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// Provider-private transport for the donor's atomic
/// `multichoice,multiFileUpload` Group mutation. Shared Core still owns the
/// future compound Draft/Artifact/Attempt contract.
#[async_trait]
pub trait UaiCompoundUploadTransport: Send + Sync {
    async fn submit_compound_upload(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundUploadSubmission,
    ) -> ProviderResult<SubmissionReceipt>;

    async fn verify_compound_upload(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundUploadSubmission,
        receipt: &SubmissionReceipt,
    ) -> ProviderResult<UaiCompoundUploadVerification>;
}

/// Fresh Task gate joining one immutable ordinary sub-draft to one uploaded
/// artifact without pretending that the current shared Draft owns both slots.
#[derive(Clone)]
pub struct UaiCompoundUploadPreparation {
    details: Arc<dyn TaskDetailCapability>,
}

impl UaiCompoundUploadPreparation {
    pub fn new(details: Arc<dyn TaskDetailCapability>) -> Self {
        Self { details }
    }

    /// Rebuilds the ordinary preview, then re-discovers the complete mixed
    /// Group and freezes both slots into one Provider-private atomic plan.
    ///
    /// # Errors
    ///
    /// Rejects stale/foreign drafts, changed upload artifacts, reversed or
    /// additional module shapes and missing current Course publish versions.
    pub async fn prepare_submission(
        &self,
        context: &ProviderContext,
        draft: &SubmissionDraft,
        uploaded: &UaiUploadedArtifact,
    ) -> ProviderResult<UaiCompoundUploadSubmission> {
        let metadata = development_metadata()?;
        if draft.provider_id != metadata.id
            || draft.provider_version != metadata.implementation_version
            || draft.validate().is_err()
            || draft.items.len() != 1
        {
            return Err(invalid_input(
                "UAI compound upload received an invalid ordinary sub-draft",
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
        let preview = UaiSubmissionBuild::try_new()?
            .build_submission_preview(context, uploaded.remote_task_id(), &questions, &selected)
            .await?;
        if preview != draft.payload_preview {
            return Err(invalid_input(
                "UAI compound upload ordinary sub-draft preview is stale",
            ));
        }
        let detail = self
            .details
            .task_detail(context, uploaded.remote_task_id())
            .await?;
        build_compound_upload_submission(&detail, draft, uploaded)
    }
}

impl fmt::Debug for UaiCompoundUploadPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundUploadPreparation")
            .field("details", &"configured")
            .finish()
    }
}

/// Immutable Provider-private plan for one atomic choice-plus-upload Group.
/// The object key and ordinary selected answer are zeroized on drop and never
/// enter the shared Draft preview.
pub struct UaiCompoundUploadSubmission {
    ordinary_draft_id: SubmissionDraftId,
    remote_task_id: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    task_fingerprint: String,
    file_key: String,
    artifact_digest: String,
    upload_intent_fingerprint: String,
    course_publish_version: u64,
    ordinary_plan: UaiSubmissionPlan,
    fingerprint: String,
}

impl UaiCompoundUploadSubmission {
    pub const fn ordinary_draft_id(&self) -> SubmissionDraftId {
        self.ordinary_draft_id
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub const fn course_publish_version(&self) -> u64 {
        self.course_publish_version
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn final_sequence_binding_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        let ordinary_draft_id = self.ordinary_draft_id.to_string();
        for field in [
            b"asterism:uai:compound-upload-final-sequence-binding:v1".as_slice(),
            ordinary_draft_id.as_bytes(),
            self.remote_task_id.as_bytes(),
            self.course_resource_id.as_bytes(),
            self.unit_id.as_bytes(),
            self.group_id.as_bytes(),
            self.task_fingerprint.as_bytes(),
            self.file_key.as_bytes(),
            self.artifact_digest.as_bytes(),
            self.upload_intent_fingerprint.as_bytes(),
            self.fingerprint.as_bytes(),
        ] {
            digest.update(field);
            digest.update(b"\0");
        }
        digest.update(self.course_publish_version.to_be_bytes());
        digest.finalize().into()
    }

    pub(crate) fn expose_file_key(&self) -> &str {
        &self.file_key
    }

    fn ordinary_question(&self) -> ProviderResult<&UaiSubmissionQuestionPlan> {
        self.ordinary_plan
            .questions()
            .first()
            .filter(|_| self.ordinary_plan.questions().len() == 1)
            .ok_or_else(|| protocol_drift("UAI compound upload lost its ordinary Question plan"))
    }
}

impl fmt::Debug for UaiCompoundUploadSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundUploadSubmission")
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("course_resource_id", &self.course_resource_id)
            .field("unit_id", &self.unit_id)
            .field("group_id", &self.group_id)
            .field("task_fingerprint", &self.task_fingerprint)
            .field("file_key", &"[ROUTE]")
            .field("artifact_digest", &self.artifact_digest)
            .field("upload_intent_fingerprint", &self.upload_intent_fingerprint)
            .field("course_publish_version", &self.course_publish_version)
            .field("ordinary_plan", &"[REDACTED]")
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl Drop for UaiCompoundUploadSubmission {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.course_resource_id.zeroize();
        self.unit_id.zeroize();
        self.group_id.zeroize();
        self.task_fingerprint.zeroize();
        self.file_key.zeroize();
        self.artifact_digest.zeroize();
        self.upload_intent_fingerprint.zeroize();
        self.fingerprint.zeroize();
    }
}

/// Complete zeroizing request for one atomic choice-plus-upload mutation.
pub struct UaiCompoundUploadSubmissionRequest {
    url: Zeroizing<String>,
    content_type: &'static str,
    body: Zeroizing<String>,
    sequence_binding_digest: [u8; 32],
    request_digest: [u8; 32],
}

impl UaiCompoundUploadSubmissionRequest {
    /// Exact pre-dispatch identity over method, route, content type and body.
    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn sequence_binding_digest(&self) -> [u8; 32] {
        self.sequence_binding_digest
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

impl fmt::Debug for UaiCompoundUploadSubmissionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundUploadSubmissionRequest")
            .field("url", &"[ROUTE]")
            .field("content_type", &self.content_type)
            .field("body", &"[REDACTED]")
            .field("sequence_binding_digest", &"[HASHED]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

impl Drop for UaiCompoundUploadSubmissionRequest {
    fn drop(&mut self) {
        self.sequence_binding_digest.zeroize();
        self.request_digest.zeroize();
    }
}

/// Receipt-versioned confirmation that both the ordinary selected answer and
/// uploaded object key persisted in their exact original order. Fresh Group
/// progress remains the independent completion authority.
pub struct UaiCompoundUploadVerification {
    ordinary_draft_id: SubmissionDraftId,
    remote_task_id: String,
    artifact_digest: String,
    submission_version: String,
    score: Option<SubmissionScore>,
    policy: Option<UaiSubmissionPolicyEvidence>,
    verified_at: Timestamp,
}

impl UaiCompoundUploadVerification {
    pub const fn ordinary_draft_id(&self) -> SubmissionDraftId {
        self.ordinary_draft_id
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    pub fn submission_version(&self) -> &str {
        &self.submission_version
    }

    pub const fn score(&self) -> Option<SubmissionScore> {
        self.score
    }

    pub const fn policy(&self) -> Option<&UaiSubmissionPolicyEvidence> {
        self.policy.as_ref()
    }

    pub const fn verified_at(&self) -> Timestamp {
        self.verified_at
    }

    pub const fn requires_fresh_progress_read(&self) -> bool {
        true
    }
}

impl fmt::Debug for UaiCompoundUploadVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundUploadVerification")
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("artifact_digest", &self.artifact_digest)
            .field("submission_version", &self.submission_version)
            .field("score", &self.score)
            .field("policy", &self.policy)
            .field("verified_at", &self.verified_at)
            .finish()
    }
}

impl Drop for UaiCompoundUploadVerification {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.artifact_digest.zeroize();
        self.submission_version.zeroize();
    }
}

fn build_compound_upload_submission(
    detail: &asterism_provider_api::RemoteTaskDetail,
    draft: &SubmissionDraft,
    uploaded: &UaiUploadedArtifact,
) -> ProviderResult<UaiCompoundUploadSubmission> {
    if detail.task.remote_id != uploaded.remote_task_id()
        || detail.task.fingerprint != uploaded.task_fingerprint()
        || uploaded.upload_position() != 2
    {
        return Err(remote_changed(
            "UAI compound upload Task or upload position changed after object storage",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI compound upload has no normalized Task"))?;
    let task_types = task
        .get("task_types")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI compound upload has no Task type array"))?;
    if task.get("schema").and_then(Value::as_str) != Some("uai.group-task.v1")
        || task.get("course_resource_id").and_then(Value::as_str)
            != Some(uploaded.course_resource_id())
        || task
            .get("unit")
            .and_then(Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(Value::as_str)
            != Some(uploaded.unit_id())
        || task.get("group_id").and_then(Value::as_str) != Some(uploaded.group_id())
        || task.get("question_count").and_then(Value::as_u64) != Some(2)
        || task_types.as_slice()
            != [
                Value::String("multichoice".to_owned()),
                Value::String("multiFileUpload".to_owned()),
            ]
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI compound upload requires exact ordered multichoice and multiFileUpload modules",
        ));
    }
    let expected_remote_task_id = format!(
        "group:{}:{}:{}",
        uploaded.course_resource_id(),
        uploaded.unit_id(),
        uploaded.group_id()
    );
    if uploaded.remote_task_id() != expected_remote_task_id {
        return Err(remote_changed(
            "UAI compound upload hierarchy is foreign to its Task",
        ));
    }
    let course_publish_version = task
        .get("course_publish_version")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && i64::try_from(*value).is_ok())
        .ok_or_else(|| {
            protocol_drift("UAI compound upload has no current Course publish version")
        })?;
    let ordinary_plan =
        UaiSubmissionPlan::from_single_draft_current(draft, "multichoice", course_publish_version)?;
    if ordinary_plan.questions().len() != 1
        || ordinary_plan.questions()[0].task_type() != "multichoice"
    {
        return Err(invalid_input(
            "UAI compound upload ordinary sub-draft is not one multichoice module",
        ));
    }

    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-submission:v1\0");
    digest.update(uploaded.remote_task_id().as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.task_fingerprint().as_bytes());
    digest.update(b"\0");
    digest.update(draft.id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(draft.question_snapshot_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.intent_fingerprint().as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.artifact_digest().as_bytes());
    digest.update(b"\0");
    digest.update(uploaded.file_key().as_bytes());
    digest.update(b"\0");
    digest.update(course_publish_version.to_be_bytes());
    Ok(UaiCompoundUploadSubmission {
        ordinary_draft_id: draft.id,
        remote_task_id: uploaded.remote_task_id().to_owned(),
        course_resource_id: uploaded.course_resource_id().to_owned(),
        unit_id: uploaded.unit_id().to_owned(),
        group_id: uploaded.group_id().to_owned(),
        task_fingerprint: uploaded.task_fingerprint().to_owned(),
        file_key: uploaded.file_key().to_owned(),
        artifact_digest: uploaded.artifact_digest().to_owned(),
        upload_intent_fingerprint: uploaded.intent_fingerprint().to_owned(),
        course_publish_version,
        ordinary_plan,
        fingerprint: format!("uai-compound-upload-v1:{:x}", digest.finalize()),
    })
}

/// Builds the exact two-module body only inside the native mutation boundary.
///
/// # Errors
///
/// Rejects unsafe route identities, lost module bindings or an oversized body.
fn build_compound_upload_submission_body(
    submission: &UaiCompoundUploadSubmission,
    course_instance_id: &str,
    open_id: &str,
) -> ProviderResult<Zeroizing<String>> {
    if !is_remote_component(course_instance_id)
        || open_id.is_empty()
        || open_id.len() > 8 * 1_024
        || open_id.chars().any(char::is_control)
    {
        return Err(invalid_input(
            "UAI compound upload route identity is invalid",
        ));
    }
    let ordinary = submission.ordinary_question()?;
    let protocol_versions = submission.ordinary_plan.protocol_versions();
    if protocol_versions.course() != submission.course_publish_version
        || protocol_versions.answer() != 3
    {
        return Err(protocol_drift(
            "UAI compound upload protocol version binding changed",
        ));
    }
    let mut question_data = ZeroizingJsonValue::new(Value::Array(Vec::with_capacity(2)));
    let mut is_completed = Vec::new();
    let mut judges = ZeroizingJsonValue::new(Value::Array(Vec::new()));
    push_ordinary_question(
        &mut question_data,
        &mut is_completed,
        &mut judges,
        ordinary,
        submission.course_publish_version,
    )?;
    push_upload_question(
        &mut question_data,
        &mut is_completed,
        &mut judges,
        submission.expose_file_key(),
        submission.course_publish_version,
    )?;
    let judges = Zeroizing::new(
        serde_json::to_string(judges.as_value())
            .map_err(|_| invalid_response("UAI compound upload judges cannot be serialized"))?,
    );
    let body = ZeroizingJsonValue::new(serde_json::json!({
        "quesDatas": question_data.as_value(),
        "groupId": submission.group_id,
        "isCompleted": is_completed,
        "thirdPartyJudges": judges.as_str(),
        "submitType": 1,
        "hideLoading": false,
        "associationGroupId": "",
        "courseId": course_instance_id,
        "openId": open_id,
        "version": "default",
    }));
    let encoded = Zeroizing::new(
        serde_json::to_string(body.as_value())
            .map_err(|_| invalid_response("UAI compound upload cannot be serialized"))?,
    );
    if encoded.is_empty() || encoded.len() > MAX_COMPOUND_SUBMISSION_BYTES {
        return Err(invalid_response(
            "UAI compound upload body exceeds the size limit",
        ));
    }
    Ok(encoded)
}

/// Materializes the exact atomic upload request only after fresh route and
/// account rebinding.
///
/// # Errors
///
/// Rejects an invalid fixed route or any body/identity failure from the
/// compound submission builder.
pub fn build_compound_upload_submission_request(
    submission: &UaiCompoundUploadSubmission,
    course_instance_id: &str,
    open_id: &str,
) -> ProviderResult<UaiCompoundUploadSubmissionRequest> {
    let body = build_compound_upload_submission_body(submission, course_instance_id, open_id)?;
    let url = Url::parse(UAI_COMPOUND_UPLOAD_SUBMISSION_ROUTE)
        .map_err(|_| invalid_response("UAI compound upload submission route is invalid"))?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-request:v1\0");
    digest.update(b"POST\0");
    digest.update(url.as_str().as_bytes());
    digest.update(b"\0content-type\0");
    digest.update(UAI_COMPOUND_UPLOAD_CONTENT_TYPE.as_bytes());
    digest.update(b"\0body\0");
    digest.update(body.as_bytes());
    Ok(UaiCompoundUploadSubmissionRequest {
        url: Zeroizing::new(url.into()),
        content_type: UAI_COMPOUND_UPLOAD_CONTENT_TYPE,
        body,
        sequence_binding_digest: submission.final_sequence_binding_digest(),
        request_digest: digest.finalize().into(),
    })
}

fn push_ordinary_question(
    question_data: &mut ZeroizingJsonValue,
    is_completed: &mut Vec<bool>,
    judges: &mut ZeroizingJsonValue,
    question: &UaiSubmissionQuestionPlan,
    course_publish_version: u64,
) -> ProviderResult<()> {
    if question.judges().len() != question.answer_children().len() {
        return Err(protocol_drift(
            "UAI compound upload ordinary Question judge cardinality changed",
        ));
    }
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
    let answer = Zeroizing::new(
        serde_json::to_string(&serde_json::json!({
            "value": [],
            "children": children,
            "progress": {},
            "record": {"url": ""},
        }))
        .map_err(|_| invalid_response("UAI compound ordinary answer cannot be serialized"))?,
    );
    question_data
        .as_value_mut()
        .as_array_mut()
        .ok_or_else(|| protocol_drift("UAI compound Question buffer is invalid"))?
        .push(serde_json::json!({
            "instanceId": question.remote_question_id(),
            "answer": answer.as_str(),
            "context": "{\"state\":\"submitted\"}",
            "contextVersion": 1,
            "answerVersion": 1,
        }));
    for (values, judge) in question.answer_children().iter().zip(question.judges()) {
        is_completed.push(true);
        judges
            .as_value_mut()
            .as_array_mut()
            .ok_or_else(|| protocol_drift("UAI compound judge buffer is invalid"))?
            .push(serde_json::json!({
                "value": values.join(","),
                "question_type": judge.question_type(),
                "reply_type": judge.reply_type(),
                "versions": {
                    "course": course_publish_version,
                    "group": 1,
                    "template": 1,
                    "answer": 3,
                    "content": 0,
                },
                "payloads": [],
            }));
    }
    Ok(())
}

fn push_upload_question(
    question_data: &mut ZeroizingJsonValue,
    is_completed: &mut Vec<bool>,
    judges: &mut ZeroizingJsonValue,
    file_key: &str,
    course_publish_version: u64,
) -> ProviderResult<()> {
    let answer = Zeroizing::new(
        serde_json::to_string(&serde_json::json!({
            "value": [],
            "children": [{
                "value": [file_key],
                "isDone": true,
                "isRight": true,
                "replyCategory": "objective",
            }],
            "progress": {},
            "record": {"url": ""},
        }))
        .map_err(|_| invalid_response("UAI compound upload answer cannot be serialized"))?,
    );
    question_data
        .as_value_mut()
        .as_array_mut()
        .ok_or_else(|| protocol_drift("UAI compound Question buffer is invalid"))?
        .push(serde_json::json!({
            "instanceId": "0",
            "answer": answer.as_str(),
            "context": "{\"state\":\"submitted\"}",
            "contextVersion": 1,
            "answerVersion": 1,
        }));
    is_completed.push(true);
    judges
        .as_value_mut()
        .as_array_mut()
        .ok_or_else(|| protocol_drift("UAI compound judge buffer is invalid"))?
        .push(serde_json::json!({
            "value": file_key,
            "question_type": "multiFileUpload",
            "reply_type": "multiFileUpload",
            "versions": {
                "course": course_publish_version,
                "group": 1,
                "template": 1,
                "answer": 3,
                "content": 0,
            },
            "payloads": [],
        }));
    Ok(())
}

/// Verifies both ordered modules through the one receipt-authorized
/// user-module readback.
///
/// # Errors
///
/// Rejects missing receipts, changed module order/cardinality, a changed
/// ordinary answer or uploaded key, and any route/version drift.
pub fn parse_compound_upload_verification(
    document: &str,
    submission: &UaiCompoundUploadSubmission,
    receipt: &SubmissionReceipt,
) -> ProviderResult<UaiCompoundUploadVerification> {
    receipt
        .validate()
        .map_err(|_| invalid_response("UAI compound upload verification receipt is invalid"))?;
    let version = receipt
        .provider_trace_id
        .as_deref()
        .filter(|value| receipt.remote_status == "accepted" && valid_submission_version(value))
        .ok_or_else(|| {
            invalid_response("UAI compound upload verification requires an accepted receipt")
        })?;
    if document.is_empty() || document.len() > MAX_COMPOUND_VERIFICATION_BYTES {
        return Err(invalid_response(
            "UAI compound upload verification response is empty or oversized",
        ));
    }
    let response = ZeroizingJsonValue::new(serde_json::from_str(document).map_err(|_| {
        invalid_response("UAI compound upload verification response is not valid JSON")
    })?);
    let state = bound_verification_state(response.as_value(), submission.group_id(), version)?;
    let score = verified_submission_score(state)?;
    let question_data = state
        .get("quesData")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_QUESTION_DATA_BYTES)
        .ok_or_else(|| protocol_drift("UAI compound upload readback has no Question data"))?;
    let questions = ZeroizingJsonValue::new(
        serde_json::from_str(question_data)
            .map_err(|_| invalid_response("UAI compound upload Question data is invalid"))?,
    );
    let entries = questions
        .as_value()
        .as_array()
        .filter(|entries| entries.len() == 2)
        .ok_or_else(|| remote_changed("UAI compound upload readback is not exactly two modules"))?;
    parse_remote_question(&entries[0], submission.ordinary_question()?)?;
    validate_upload_readback_question(&entries[1], submission.expose_file_key())?;
    let policy = verified_submission_policy(
        state,
        submission.group_id(),
        version,
        Sha256::digest(document.as_bytes()).into(),
    )?;
    Ok(UaiCompoundUploadVerification {
        ordinary_draft_id: submission.ordinary_draft_id,
        remote_task_id: submission.remote_task_id.clone(),
        artifact_digest: submission.artifact_digest.clone(),
        submission_version: version.to_owned(),
        score,
        policy,
        verified_at: Utc::now(),
    })
}

fn is_remote_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_input(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, AssessmentClass, NormalizedAnswer, ProviderId, Question,
        QuestionId, QuestionKind, QuestionOption, QuestionSnapshotId, RemoteState, SelectedAnswer,
        SourceType, SubmissionAnswerCoverage, SubmissionDraftId, SubmissionPayloadEncoding,
        SubmissionPayloadFieldPreview, SubmissionPayloadPreview, TaskId,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;

    #[test]
    fn exact_mixed_shape_builds_one_atomic_ordered_body() {
        let uploaded = UaiUploadedArtifact::fixture(2);
        let draft = ordinary_draft();
        let submission = build_compound_upload_submission(
            &detail(&["multichoice", "multiFileUpload"]),
            &draft,
            &uploaded,
        )
        .unwrap();
        assert_eq!(submission.ordinary_draft_id(), draft.id);
        assert!(
            submission
                .fingerprint()
                .starts_with("uai-compound-upload-v1:")
        );
        assert!(!format!("{submission:?}").contains(uploaded.file_key()));
        let sequence = crate::UaiUploadFinalSubmissionSequence::for_compound(&submission).unwrap();
        assert_eq!(
            sequence.kind(),
            crate::UaiUploadFinalSubmissionKind::Compound
        );
        assert_eq!(
            sequence.plan().phases()[0].advance_condition(),
            asterism_provider_api::ExecutionMutationSequenceAdvanceCondition::AcceptedOrMaximumReached
        );
        let sequence_artifact =
            serde_json::to_string(sequence.artifact().payload_sanitized()).unwrap();
        assert!(sequence_artifact.contains(&draft.id.to_string()));
        assert!(!sequence_artifact.contains(uploaded.file_key()));

        let body =
            build_compound_upload_submission_body(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["quesDatas"].as_array().unwrap().len(), 2);
        assert_eq!(body["quesDatas"][0]["instanceId"], "1001");
        assert_eq!(body["quesDatas"][1]["instanceId"], "0");
        assert_eq!(body["isCompleted"], json!([true, true]));
        let upload_answer: Value =
            serde_json::from_str(body["quesDatas"][1]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(
            upload_answer["children"][0]["value"],
            json!([uploaded.file_key()])
        );
        let judges: Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges[0]["question_type"], "basic");
        assert_eq!(judges[1]["question_type"], "multiFileUpload");
        assert_eq!(judges[1]["versions"]["course"], 123_290);
    }

    #[test]
    fn atomic_upload_request_digest_binds_route_account_and_complete_body() {
        let uploaded = UaiUploadedArtifact::fixture(2);
        let draft = ordinary_draft();
        let submission = build_compound_upload_submission(
            &detail(&["multichoice", "multiFileUpload"]),
            &draft,
            &uploaded,
        )
        .unwrap();
        let request =
            build_compound_upload_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let duplicate =
            build_compound_upload_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        assert_ne!(request.request_digest(), [0; 32]);
        assert_eq!(request.request_digest(), duplicate.request_digest());
        assert_ne!(
            request.request_digest(),
            build_compound_upload_submission_request(&submission, "course-instance-2", "openid-1",)
                .unwrap()
                .request_digest()
        );
        assert_ne!(
            request.request_digest(),
            build_compound_upload_submission_request(&submission, "course-instance-1", "openid-2",)
                .unwrap()
                .request_digest()
        );
        let debug = format!("{request:?}");
        assert!(debug.contains("[ROUTE]") && debug.contains("[REDACTED]"));
        assert!(!debug.contains(uploaded.file_key()) && !debug.contains("openid-1"));
    }

    #[test]
    fn atomic_upload_request_digest_changes_with_the_selected_answer() {
        let uploaded = UaiUploadedArtifact::fixture(2);
        let first = build_compound_upload_submission(
            &detail(&["multichoice", "multiFileUpload"]),
            &ordinary_draft(),
            &uploaded,
        )
        .unwrap();
        let mut changed_draft = ordinary_draft();
        changed_draft.items[0].selected.answer = NormalizedAnswer::Selections(vec!["B".to_owned()]);
        changed_draft.validate().unwrap();
        let changed = build_compound_upload_submission(
            &detail(&["multichoice", "multiFileUpload"]),
            &changed_draft,
            &uploaded,
        )
        .unwrap();
        let first =
            build_compound_upload_submission_request(&first, "course-instance-1", "openid-1")
                .unwrap();
        let changed =
            build_compound_upload_submission_request(&changed, "course-instance-1", "openid-1")
                .unwrap();
        assert_ne!(first.request_digest(), changed.request_digest());
    }

    #[test]
    fn reversed_extra_or_wrong_upload_position_fails_before_body() {
        let draft = ordinary_draft();
        assert!(
            build_compound_upload_submission(
                &detail(&["multiFileUpload", "multichoice"]),
                &draft,
                &UaiUploadedArtifact::fixture(2),
            )
            .is_err()
        );
        assert!(
            build_compound_upload_submission(
                &detail(&["multichoice", "multiFileUpload", "short_answer"]),
                &draft,
                &UaiUploadedArtifact::fixture(2),
            )
            .is_err()
        );
        assert!(
            build_compound_upload_submission(
                &detail(&["multichoice", "multiFileUpload"]),
                &draft,
                &UaiUploadedArtifact::fixture(1),
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_readback_must_preserve_both_modules_and_values() {
        let uploaded = UaiUploadedArtifact::fixture(2);
        let draft = ordinary_draft();
        let submission = build_compound_upload_submission(
            &detail(&["multichoice", "multiFileUpload"]),
            &draft,
            &uploaded,
        )
        .unwrap();
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some("synthetic accepted compound upload".to_owned()),
            provider_trace_id: Some("compound-v1".to_owned()),
            received_at: Utc::now(),
        };
        let ordinary_answer = json!({
            "value": [],
            "children": [{"value": ["A"], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let upload_answer = json!({
            "value": [],
            "children": [{"value": [uploaded.file_key()], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let questions = json!([
            {"instanceId":"1001","answer":ordinary_answer,"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"0","answer":upload_answer,"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        let document = verification_document(&questions);
        let verified =
            parse_compound_upload_verification(&document, &submission, &receipt).unwrap();
        assert_eq!(verified.ordinary_draft_id(), draft.id);
        assert_eq!(verified.artifact_digest(), uploaded.artifact_digest());
        assert_eq!(verified.score(), None);
        let policy = verified.policy().unwrap();
        assert_eq!(policy.group_id(), "group-upload");
        assert_eq!(policy.submission_version(), "compound-v1");
        assert_eq!(policy.strategy_id(), 3001);
        assert_eq!(
            policy.result_digest(),
            <[u8; 32]>::from(Sha256::digest(document.as_bytes()))
        );
        assert!(verified.requires_fresh_progress_read());

        assert!(
            parse_compound_upload_verification(
                &document.replace("course/42/nothing.mp3", "other/key.mp3"),
                &submission,
                &receipt,
            )
            .is_err()
        );
        let reversed = json!([
            {"instanceId":"0","answer":json!({"children":[{"value":[uploaded.file_key()],"isDone":true}]}).to_string(),"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"1001","answer":json!({"children":[{"value":["A"],"isDone":true}]}).to_string(),"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        assert!(
            parse_compound_upload_verification(
                &verification_document(&reversed),
                &submission,
                &receipt,
            )
            .is_err()
        );
    }

    fn ordinary_draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some("1001".to_owned()),
            kind: QuestionKind::MultipleChoice,
            stem: "Choose all valid answers".to_owned(),
            options: ["A", "B"]
                .into_iter()
                .map(|id| QuestionOption {
                    id: id.to_owned(),
                    content: Some(format!("Option {id}")),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                })
                .collect(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "multichoice",
                "remote_task_id": "group:2001:unit-1:group-upload",
                "judge_types": [{"question_type":"basic","reply_type":"multichoice"}],
                "composite_children": null,
                "media_attachment_ids": [],
                "embedded_transcript": null,
                "matching_lefts": null
            }),
            position: 1,
        };
        let fields = [
            "quesDatas[].instanceId",
            "quesDatas[].answer",
            "quesDatas[].context",
            "quesDatas[].contextVersion",
            "quesDatas[].answerVersion",
        ]
        .into_iter()
        .map(|field_name| SubmissionPayloadFieldPreview {
            question_id: question.id,
            field_name: field_name.to_owned(),
        })
        .collect();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: env!("CARGO_PKG_VERSION").to_owned(),
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![asterism_domain::SubmissionDraftItem {
                selected: SelectedAnswer {
                    candidate_id: AnswerCandidateId::new(),
                    question_id: question.id,
                    answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                    source: AnswerSource::Manual,
                    confidence: None,
                },
                question,
            }],
            payload_preview: SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::Json,
                format: "uai.new-exploration.json.v1".to_owned(),
                fields,
            },
            created_at: Utc::now(),
        };
        draft.validate().unwrap();
        draft
    }

    fn detail(task_types: &[&str]) -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id":"unit-1","title":"Unit 1"},
            "section": {"id":"section-1","title":"Section 1"},
            "micro": {"id":"micro-1","title":"Speaking"},
            "group_id": "group-upload",
            "course_publish_version": 123_290,
            "task_types": task_types,
            "question_count": task_types.len(),
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "group:2001:unit-1:group-upload".to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Compound upload".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "v1:compound-upload".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: json!({"schema":"uai.group-task.raw.v1"}),
            },
            normalized_detail: json!({
                "schema":"uai.group-task-detail.v1",
                "task":normalized
            }),
        }
    }

    fn verification_document(questions: &str) -> String {
        json!({
            "success": true,
            "code": 0,
            "data": {
                "course": "course-instance-1",
                "module": "group-upload-compound-v1",
                "state": {
                    "version": "compound-v1",
                    "quesData": questions,
                    "__EXTEND_DATA__": {"__SUBMIT_INFO__": {
                        "course_id":"course-instance-1",
                        "group_id":"group-upload",
                        "version":"compound-v1",
                        "strategyId":3001,
                        "strategy":{
                            "endTime":1_790_812_800,
                            "record_every_submit":false,
                            "record_max_submit":true,
                            "required":true,
                            "startTime":1_785_542_400,
                            "task_mini_score_pct":60
                        },
                        "state":{
                            "expired":false,
                            "lastSubmit":1_786_752_000,
                            "not_start":false
                        }
                    }}
                }
            }
        })
        .to_string()
    }
}
