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
    UaiAnswerTransport, UaiSubmissionBuild, UaiSubmissionPlan,
    answer::decrypt_answer_entries,
    encrypted::ZeroizingJsonValue,
    metadata::development_metadata,
    submission_execute::{UaiSubmissionQuestionPlan, valid_submission_version},
    submission_verify::{
        UaiSubmissionPolicyEvidence, bound_verification_state, parse_remote_question,
        verified_submission_policy, verified_submission_score,
    },
};

const MAX_COMPOUND_ORAL_BODY_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_COMPOUND_ORAL_READBACK_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NESTED_QUESTION_DATA_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NESTED_CONTEXT_BYTES: usize = 1_024 * 1_024;
const MAX_NESTED_ANSWER_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ORAL_CHILDREN: usize = 128;
const MAX_ORAL_CHILD_VALUE_BYTES: usize = 64 * 1_024;
const MAX_ORAL_CHILD_EXTRA_BYTES: usize = 64 * 1_024;
const UAI_COMPOUND_ORAL_SUBMISSION_ROUTE: &str =
    "https://ucontent.unipus.cn/course/api/v3/newExploration/submit";
const UAI_COMPOUND_ORAL_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// Provider-private native boundary for the donor's atomic
/// `basic-scoop-content,oral-sentence` mutation and receipt-authorized
/// readback. Shared Core still owns the future compound Draft/Attempt model.
#[async_trait]
pub trait UaiCompoundOralTransport: Send + Sync {
    async fn submit_compound_oral(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundOralSubmission,
    ) -> ProviderResult<SubmissionReceipt>;

    async fn verify_compound_oral(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundOralSubmission,
        receipt: &SubmissionReceipt,
    ) -> ProviderResult<UaiCompoundOralVerification>;
}

/// Fresh gate joining one immutable matching Draft to the exact oral module
/// carried by the same Group's encrypted standard-answer read.
#[derive(Clone)]
pub struct UaiCompoundOralPreparation {
    details: Arc<dyn TaskDetailCapability>,
    answers: Arc<dyn UaiAnswerTransport>,
}

impl UaiCompoundOralPreparation {
    pub fn new(
        details: Arc<dyn TaskDetailCapability>,
        answers: Arc<dyn UaiAnswerTransport>,
    ) -> Self {
        Self { details, answers }
    }

    /// Rebuilds the ordinary preview, freshly rediscovers the exact two-module
    /// Group, then freezes the encrypted oral instance and bounded child data.
    ///
    /// # Errors
    ///
    /// Rejects stale/foreign Drafts, reordered or additional modules,
    /// unsupported child shapes and missing current Course versions.
    pub async fn prepare_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
    ) -> ProviderResult<UaiCompoundOralSubmission> {
        validate_ordinary_draft(draft, remote_task_id)?;
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
            .build_submission_preview(context, remote_task_id, &questions, &selected)
            .await?;
        if preview != draft.payload_preview {
            return Err(invalid_input(
                "UAI compound oral ordinary sub-draft preview is stale",
            ));
        }
        let detail = self.details.task_detail(context, remote_task_id).await?;
        let binding = CompoundOralBinding::from_detail(&detail, remote_task_id)?;
        let documents = self
            .answers
            .fetch_answer_documents(context, &binding.course_resource_id, &binding.group_id)
            .await?;
        build_compound_oral_submission(&detail, remote_task_id, draft, documents.answer().as_str())
    }
}

impl fmt::Debug for UaiCompoundOralPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralPreparation")
            .field("details", &"configured")
            .field("answers", &"configured")
            .finish()
    }
}

/// Immutable Provider-private plan for one matching-plus-oral Group.
/// The ordinary selected answer and oral instance remain outside shared Draft
/// serialization until Core gains a compound Draft slot.
pub struct UaiCompoundOralSubmission {
    ordinary_draft_id: SubmissionDraftId,
    remote_task_id: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    task_fingerprint: String,
    course_publish_version: u64,
    ordinary_plan: UaiSubmissionPlan,
    oral_instance_id: String,
    oral_children: Vec<OralChildEvidence>,
    fingerprint: String,
}

impl UaiCompoundOralSubmission {
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

    pub const fn course_publish_version(&self) -> u64 {
        self.course_publish_version
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn oral_instance_id(&self) -> &str {
        &self.oral_instance_id
    }

    fn ordinary_question(&self) -> ProviderResult<&UaiSubmissionQuestionPlan> {
        self.ordinary_plan
            .questions()
            .first()
            .filter(|_| self.ordinary_plan.questions().len() == 1)
            .ok_or_else(|| protocol_drift("UAI compound oral lost its ordinary Question plan"))
    }
}

impl fmt::Debug for UaiCompoundOralSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralSubmission")
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("course_resource_id", &self.course_resource_id)
            .field("unit_id", &self.unit_id)
            .field("group_id", &self.group_id)
            .field("task_fingerprint", &self.task_fingerprint)
            .field("course_publish_version", &self.course_publish_version)
            .field("ordinary_plan", &"[REDACTED]")
            .field("oral_instance_id", &"[REMOTE]")
            .field("oral_child_count", &self.oral_children.len())
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl Drop for UaiCompoundOralSubmission {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.course_resource_id.zeroize();
        self.unit_id.zeroize();
        self.group_id.zeroize();
        self.task_fingerprint.zeroize();
        self.oral_instance_id.zeroize();
        self.fingerprint.zeroize();
    }
}

/// Complete zeroizing request for one atomic matching-plus-oral mutation.
pub struct UaiCompoundOralSubmissionRequest {
    url: Zeroizing<String>,
    content_type: &'static str,
    body: Zeroizing<String>,
    request_digest: [u8; 32],
}

impl UaiCompoundOralSubmissionRequest {
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

impl fmt::Debug for UaiCompoundOralSubmissionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralSubmissionRequest")
            .field("url", &"[ROUTE]")
            .field("content_type", &self.content_type)
            .field("body", &"[REDACTED]")
            .field("request_digest", &"[HASHED]")
            .finish()
    }
}

impl Drop for UaiCompoundOralSubmissionRequest {
    fn drop(&mut self) {
        self.request_digest.zeroize();
    }
}

/// Receipt-versioned proof that the ordinary answer and exact oral slot
/// persisted in their original order. Fresh progress remains separate.
pub struct UaiCompoundOralVerification {
    ordinary_draft_id: SubmissionDraftId,
    remote_task_id: String,
    oral_instance_id: String,
    submission_version: String,
    result_digest: [u8; 32],
    score: Option<SubmissionScore>,
    policy: Option<UaiSubmissionPolicyEvidence>,
    verified_at: Timestamp,
}

impl UaiCompoundOralVerification {
    pub const fn ordinary_draft_id(&self) -> SubmissionDraftId {
        self.ordinary_draft_id
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn oral_instance_id(&self) -> &str {
        &self.oral_instance_id
    }

    pub fn submission_version(&self) -> &str {
        &self.submission_version
    }

    pub const fn result_digest(&self) -> [u8; 32] {
        self.result_digest
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

impl fmt::Debug for UaiCompoundOralVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralVerification")
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("oral_instance_id", &"[REMOTE]")
            .field("submission_version", &self.submission_version)
            .field("result_digest", &"[HASHED]")
            .field("score", &self.score)
            .field("policy", &self.policy)
            .field("verified_at", &self.verified_at)
            .finish()
    }
}

impl Drop for UaiCompoundOralVerification {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.oral_instance_id.zeroize();
        self.submission_version.zeroize();
        self.result_digest.zeroize();
    }
}

struct OralChildEvidence {
    value: ZeroizingJsonValue,
    extra: Option<ZeroizingJsonValue>,
    judge_value: Zeroizing<String>,
}

struct CompoundOralBinding {
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    course_publish_version: u64,
}

impl CompoundOralBinding {
    fn from_detail(
        detail: &asterism_provider_api::RemoteTaskDetail,
        remote_task_id: &str,
    ) -> ProviderResult<Self> {
        if detail.task.remote_id != remote_task_id {
            return Err(remote_changed(
                "UAI compound oral Task identity changed during rediscovery",
            ));
        }
        let task = detail
            .normalized_detail
            .get("task")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("UAI compound oral has no normalized Task"))?;
        let task_types = task
            .get("task_types")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI compound oral has no Task type array"))?;
        if task.get("schema").and_then(Value::as_str) != Some("uai.group-task.v1")
            || task.get("question_count").and_then(Value::as_u64) != Some(2)
            || task_types.as_slice()
                != [
                    Value::String("basic-scoop-content".to_owned()),
                    Value::String("oral-sentence".to_owned()),
                ]
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI compound oral requires exact ordered matching and oral-sentence modules",
            ));
        }
        let course_resource_id = remote_component(
            task.get("course_resource_id"),
            "compound oral Course-resource ID",
        )?;
        let unit_id = task
            .get("unit")
            .and_then(Value::as_object)
            .map(|unit| remote_component(unit.get("id"), "compound oral Unit ID"))
            .transpose()?
            .ok_or_else(|| protocol_drift("UAI compound oral has no Unit identity"))?;
        let group_id = remote_component(task.get("group_id"), "compound oral Group ID")?;
        if remote_task_id != format!("group:{course_resource_id}:{unit_id}:{group_id}") {
            return Err(remote_changed(
                "UAI compound oral hierarchy is foreign to its Task",
            ));
        }
        let course_publish_version = task
            .get("course_publish_version")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0 && i64::try_from(*value).is_ok())
            .ok_or_else(|| {
                protocol_drift("UAI compound oral has no current Course publish version")
            })?;
        Ok(Self {
            course_resource_id,
            unit_id,
            group_id,
            course_publish_version,
        })
    }
}

fn validate_ordinary_draft(draft: &SubmissionDraft, remote_task_id: &str) -> ProviderResult<()> {
    let metadata = development_metadata()?;
    if draft.provider_id != metadata.id
        || draft.provider_version != metadata.implementation_version
        || draft.validate().is_err()
        || draft.items.len() != 1
        || draft.items[0]
            .question
            .metadata_sanitized
            .get("remote_task_id")
            .and_then(Value::as_str)
            != Some(remote_task_id)
    {
        return Err(invalid_input(
            "UAI compound oral received an invalid ordinary sub-draft",
        ));
    }
    Ok(())
}

fn build_compound_oral_submission(
    detail: &asterism_provider_api::RemoteTaskDetail,
    remote_task_id: &str,
    draft: &SubmissionDraft,
    answer_document: &str,
) -> ProviderResult<UaiCompoundOralSubmission> {
    validate_ordinary_draft(draft, remote_task_id)?;
    let binding = CompoundOralBinding::from_detail(detail, remote_task_id)?;
    let ordinary_plan = UaiSubmissionPlan::from_single_draft_current(
        draft,
        "basic-scoop-content",
        binding.course_publish_version,
    )?;
    let ordinary = ordinary_plan
        .questions()
        .first()
        .filter(|_| ordinary_plan.questions().len() == 1)
        .ok_or_else(|| invalid_input("UAI compound oral requires one ordinary Question"))?;
    let (oral_instance_id, oral_children) =
        parse_oral_evidence(answer_document, ordinary.remote_question_id())?;

    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-oral-submission:v1\0");
    digest.update(remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(detail.task.fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(draft.id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(draft.question_snapshot_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(oral_instance_id.as_bytes());
    digest.update(b"\0");
    digest.update(binding.course_publish_version.to_be_bytes());
    for child in &oral_children {
        let value =
            Zeroizing::new(serde_json::to_string(child.value.as_value()).map_err(|_| {
                invalid_response("UAI compound oral child value cannot be fingerprinted")
            })?);
        digest.update(value.as_bytes());
        digest.update(b"\0");
        if let Some(extra) = &child.extra {
            let extra = Zeroizing::new(serde_json::to_string(extra.as_value()).map_err(|_| {
                invalid_response("UAI compound oral child extra cannot be fingerprinted")
            })?);
            digest.update([1]);
            digest.update(extra.as_bytes());
        } else {
            digest.update([0]);
        }
        digest.update(b"\0");
        digest.update(child.judge_value.as_bytes());
        digest.update(b"\0");
    }
    Ok(UaiCompoundOralSubmission {
        ordinary_draft_id: draft.id,
        remote_task_id: remote_task_id.to_owned(),
        course_resource_id: binding.course_resource_id,
        unit_id: binding.unit_id,
        group_id: binding.group_id,
        task_fingerprint: detail.task.fingerprint.clone(),
        course_publish_version: binding.course_publish_version,
        ordinary_plan,
        oral_instance_id,
        oral_children,
        fingerprint: format!("uai-compound-oral-v1:{:x}", digest.finalize()),
    })
}

fn parse_oral_evidence(
    document: &str,
    expected_ordinary_id: &str,
) -> ProviderResult<(String, Vec<OralChildEvidence>)> {
    let decrypted = decrypt_answer_entries(document)?;
    let entries = decrypted
        .as_value()
        .as_array()
        .filter(|entries| entries.len() == 2)
        .ok_or_else(|| {
            protocol_drift("UAI compound oral standard answer is not exactly two modules")
        })?;
    if entries[0].get("id").and_then(remote_identity).as_deref() != Some(expected_ordinary_id) {
        return Err(remote_changed(
            "UAI compound oral ordinary instance changed after Draft creation",
        ));
    }
    let oral = entries[1]
        .as_object()
        .ok_or_else(|| protocol_drift("UAI compound oral evidence is not an object"))?;
    let oral_instance_id = oral
        .get("id")
        .and_then(remote_identity)
        .filter(|value| valid_oral_identity(value))
        .ok_or_else(|| protocol_drift("UAI compound oral has no numeric oral instance"))?;
    let answer = match oral.get("answer") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.is_empty() => None,
        Some(Value::String(value)) if value.len() <= MAX_NESTED_ANSWER_BYTES => Some(value),
        _ => {
            return Err(protocol_drift(
                "UAI compound oral answer evidence is invalid or oversized",
            ));
        }
    };
    let Some(answer) = answer else {
        return Ok((oral_instance_id, vec![scalar_empty_oral_child()]));
    };
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| protocol_drift("UAI compound oral answer evidence is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .filter(|children| (1..=MAX_ORAL_CHILDREN).contains(&children.len()))
        .ok_or_else(|| protocol_drift("UAI compound oral has no bounded answer children"))?;
    let mut evidence = Vec::with_capacity(children.len());
    for child in children {
        let child = child
            .as_object()
            .ok_or_else(|| protocol_drift("UAI compound oral answer child is not an object"))?;
        evidence.push(parse_oral_child_evidence(child)?);
    }
    Ok((oral_instance_id, evidence))
}

fn scalar_empty_oral_child() -> OralChildEvidence {
    OralChildEvidence {
        value: ZeroizingJsonValue::new(Value::String(String::new())),
        extra: None,
        judge_value: Zeroizing::new(String::new()),
    }
}

fn parse_oral_child_evidence(
    child: &serde_json::Map<String, Value>,
) -> ProviderResult<OralChildEvidence> {
    let value = child
        .get("answers")
        .filter(|value| json_truthy(value))
        .or_else(|| child.get("value").filter(|value| json_truthy(value)))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let encoded = Zeroizing::new(
        serde_json::to_string(&value)
            .map_err(|_| protocol_drift("UAI compound oral child value is not serializable"))?,
    );
    if encoded.len() > MAX_ORAL_CHILD_VALUE_BYTES {
        return Err(protocol_drift(
            "UAI compound oral child value exceeds its bound",
        ));
    }
    let judge_value = oral_judge_value(&value)?;
    if judge_value.len() > MAX_ORAL_CHILD_VALUE_BYTES {
        return Err(protocol_drift(
            "UAI compound oral child judge value exceeds its bound",
        ));
    }
    let extra = child
        .get("answersExtra")
        .filter(|value| json_truthy(value))
        .map(|value| {
            let encoded = Zeroizing::new(serde_json::to_string(value).map_err(|_| {
                protocol_drift("UAI compound oral child extra is not serializable")
            })?);
            if encoded.len() > MAX_ORAL_CHILD_EXTRA_BYTES {
                return Err(protocol_drift(
                    "UAI compound oral child extra exceeds its bound",
                ));
            }
            Ok(ZeroizingJsonValue::new(value.clone()))
        })
        .transpose()?;
    Ok(OralChildEvidence {
        value: ZeroizingJsonValue::new(value),
        extra,
        judge_value: Zeroizing::new(judge_value),
    })
}

fn oral_judge_value(value: &Value) -> ProviderResult<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::UnsupportedTask,
                        "UAI compound oral list answers must contain only text",
                    )
                })
            })
            .collect::<ProviderResult<Vec<_>>>()
            .map(|values| values.join(",")),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(if *value { "True" } else { "False" }.to_owned()),
        Value::Null => Ok(String::new()),
        Value::Object(_) => Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI compound oral object answer has no stable donor judge encoding",
        )),
    }
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

/// Builds the exact ordered matching-plus-oral body only inside the
/// native mutation boundary.
///
/// # Errors
///
/// Rejects unsafe route identities, lost module/version bindings or an
/// oversized body.
fn build_compound_oral_submission_body(
    submission: &UaiCompoundOralSubmission,
    course_instance_id: &str,
    open_id: &str,
) -> ProviderResult<Zeroizing<String>> {
    if !is_remote_component(course_instance_id)
        || open_id.is_empty()
        || open_id.len() > 8 * 1_024
        || open_id.chars().any(char::is_control)
    {
        return Err(invalid_input("UAI compound oral route identity is invalid"));
    }
    let ordinary = submission.ordinary_question()?;
    let protocol_versions = submission.ordinary_plan.protocol_versions();
    if protocol_versions.course() != submission.course_publish_version
        || protocol_versions.answer() != 3
    {
        return Err(protocol_drift(
            "UAI compound oral protocol version binding changed",
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
    push_oral_question(
        &mut question_data,
        &mut is_completed,
        &mut judges,
        &submission.oral_instance_id,
        &submission.oral_children,
        submission.course_publish_version,
    )?;
    let judges = Zeroizing::new(
        serde_json::to_string(judges.as_value())
            .map_err(|_| invalid_response("UAI compound oral judges cannot be serialized"))?,
    );
    let body = ZeroizingJsonValue::new(serde_json::json!({
        "quesDatas": question_data.as_value(),
        "groupId": submission.group_id(),
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
            .map_err(|_| invalid_response("UAI compound oral body cannot be serialized"))?,
    );
    if encoded.is_empty() || encoded.len() > MAX_COMPOUND_ORAL_BODY_BYTES {
        return Err(invalid_response(
            "UAI compound oral body exceeds the size limit",
        ));
    }
    Ok(encoded)
}

/// Materializes the exact atomic oral request only after fresh route and
/// account rebinding.
///
/// # Errors
///
/// Rejects an invalid fixed route or any body/identity failure from the
/// compound submission builder.
pub fn build_compound_oral_submission_request(
    submission: &UaiCompoundOralSubmission,
    course_instance_id: &str,
    open_id: &str,
) -> ProviderResult<UaiCompoundOralSubmissionRequest> {
    let body = build_compound_oral_submission_body(submission, course_instance_id, open_id)?;
    let url = Url::parse(UAI_COMPOUND_ORAL_SUBMISSION_ROUTE)
        .map_err(|_| invalid_response("UAI compound oral submission route is invalid"))?;
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-oral-request:v1\0");
    digest.update(b"POST\0");
    digest.update(url.as_str().as_bytes());
    digest.update(b"\0content-type\0");
    digest.update(UAI_COMPOUND_ORAL_CONTENT_TYPE.as_bytes());
    digest.update(b"\0body\0");
    digest.update(body.as_bytes());
    Ok(UaiCompoundOralSubmissionRequest {
        url: Zeroizing::new(url.into()),
        content_type: UAI_COMPOUND_ORAL_CONTENT_TYPE,
        body,
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
            "UAI compound oral ordinary judge cardinality changed",
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
        .map_err(|_| invalid_response("UAI compound oral ordinary answer cannot be serialized"))?,
    );
    question_data
        .as_value_mut()
        .as_array_mut()
        .ok_or_else(|| protocol_drift("UAI compound oral Question buffer is invalid"))?
        .push(serde_json::json!({
            "instanceId": question.remote_question_id(),
            "answer": answer.as_str(),
            "context": "{\"state\":\"submitted\"}",
            "contextVersion": 1,
            "answerVersion": 0,
        }));
    for (values, judge) in question.answer_children().iter().zip(question.judges()) {
        is_completed.push(true);
        judges
            .as_value_mut()
            .as_array_mut()
            .ok_or_else(|| protocol_drift("UAI compound oral judge buffer is invalid"))?
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

fn push_oral_question(
    question_data: &mut ZeroizingJsonValue,
    is_completed: &mut Vec<bool>,
    judges: &mut ZeroizingJsonValue,
    oral_instance_id: &str,
    children: &[OralChildEvidence],
    course_publish_version: u64,
) -> ProviderResult<()> {
    if !valid_oral_identity(oral_instance_id) || !(1..=MAX_ORAL_CHILDREN).contains(&children.len())
    {
        return Err(protocol_drift(
            "UAI compound oral lost its bounded oral shape",
        ));
    }
    let answer_children = children
        .iter()
        .map(|child| {
            let mut answer = serde_json::Map::new();
            answer.insert("value".to_owned(), child.value.as_value().clone());
            answer.insert("isDone".to_owned(), Value::Bool(true));
            answer.insert("isRight".to_owned(), Value::Bool(true));
            answer.insert(
                "replyCategory".to_owned(),
                Value::String("objective".to_owned()),
            );
            if let Some(extra) = &child.extra {
                answer.insert("extra".to_owned(), extra.as_value().clone());
            }
            Value::Object(answer)
        })
        .collect::<Vec<_>>();
    let answer = Zeroizing::new(
        serde_json::to_string(&serde_json::json!({
            "value": [],
            "children": answer_children,
            "progress": {},
            "record": {"url": ""},
        }))
        .map_err(|_| invalid_response("UAI compound oral empty answer cannot be serialized"))?,
    );
    question_data
        .as_value_mut()
        .as_array_mut()
        .ok_or_else(|| protocol_drift("UAI compound oral Question buffer is invalid"))?
        .push(serde_json::json!({
            "instanceId": oral_instance_id,
            "answer": answer.as_str(),
            "context": "{\"state\":\"submitted\"}",
            "contextVersion": 1,
            "answerVersion": 0,
        }));
    for child in children {
        is_completed.push(true);
        judges
            .as_value_mut()
            .as_array_mut()
            .ok_or_else(|| protocol_drift("UAI compound oral judge buffer is invalid"))?
            .push(serde_json::json!({
                "value": child.judge_value.as_str(),
                "question_type": "oral-sentence",
                "reply_type": "oral-sentence",
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

/// Verifies both ordered modules through the one receipt-authorized user-module
/// readback.
///
/// # Errors
///
/// Rejects missing receipts, changed module order/cardinality, changed oral or
/// ordinary answers and any route/version drift.
pub fn parse_compound_oral_verification(
    document: &str,
    submission: &UaiCompoundOralSubmission,
    receipt: &SubmissionReceipt,
) -> ProviderResult<UaiCompoundOralVerification> {
    receipt
        .validate()
        .map_err(|_| invalid_response("UAI compound oral verification receipt is invalid"))?;
    let version = receipt
        .provider_trace_id
        .as_deref()
        .filter(|value| receipt.remote_status == "accepted" && valid_submission_version(value))
        .ok_or_else(|| {
            invalid_response("UAI compound oral verification requires an accepted receipt")
        })?;
    if document.is_empty() || document.len() > MAX_COMPOUND_ORAL_READBACK_BYTES {
        return Err(invalid_response(
            "UAI compound oral verification response is empty or oversized",
        ));
    }
    let response = ZeroizingJsonValue::new(serde_json::from_str(document).map_err(|_| {
        invalid_response("UAI compound oral verification response is not valid JSON")
    })?);
    let state = bound_verification_state(response.as_value(), submission.group_id(), version)?;
    let score = verified_submission_score(state)?;
    let question_data = state
        .get("quesData")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_QUESTION_DATA_BYTES)
        .ok_or_else(|| protocol_drift("UAI compound oral readback has no Question data"))?;
    let questions = ZeroizingJsonValue::new(
        serde_json::from_str(question_data)
            .map_err(|_| invalid_response("UAI compound oral Question data is invalid"))?,
    );
    let entries = questions
        .as_value()
        .as_array()
        .filter(|entries| entries.len() == 2)
        .ok_or_else(|| remote_changed("UAI compound oral readback is not exactly two modules"))?;
    parse_remote_question(&entries[0], submission.ordinary_question()?)?;
    validate_oral_readback(
        &entries[1],
        &submission.oral_instance_id,
        &submission.oral_children,
    )?;
    let result_digest = Sha256::digest(document.as_bytes()).into();
    let policy = verified_submission_policy(state, submission.group_id(), version, result_digest)?;
    Ok(UaiCompoundOralVerification {
        ordinary_draft_id: submission.ordinary_draft_id,
        remote_task_id: submission.remote_task_id.clone(),
        oral_instance_id: submission.oral_instance_id.clone(),
        submission_version: version.to_owned(),
        result_digest,
        score,
        policy,
        verified_at: Utc::now(),
    })
}

fn validate_oral_readback(
    entry: &Value,
    expected_instance_id: &str,
    expected_children: &[OralChildEvidence],
) -> ProviderResult<()> {
    let entry = entry
        .as_object()
        .ok_or_else(|| protocol_drift("UAI compound oral readback entry is not an object"))?;
    if entry.get("instanceId").and_then(remote_identity).as_deref() != Some(expected_instance_id) {
        return Err(remote_changed(
            "UAI compound oral readback instance does not match the immutable plan",
        ));
    }
    let context = entry
        .get("context")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_CONTEXT_BYTES)
        .ok_or_else(|| protocol_drift("UAI compound oral readback has no bounded context"))?;
    let context = ZeroizingJsonValue::new(
        serde_json::from_str(context)
            .map_err(|_| invalid_response("UAI compound oral context is not valid JSON"))?,
    );
    if context.as_value().get("state").and_then(Value::as_str) != Some("submitted") {
        return Err(remote_changed(
            "UAI compound oral is not in submitted state",
        ));
    }
    let answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_ANSWER_BYTES)
        .ok_or_else(|| protocol_drift("UAI compound oral readback has no bounded answer"))?;
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| invalid_response("UAI compound oral answer is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .filter(|children| children.len() == expected_children.len())
        .ok_or_else(|| remote_changed("UAI compound oral child cardinality changed"))?;
    for (child, expected) in children.iter().zip(expected_children) {
        if child.get("isDone").and_then(Value::as_bool) != Some(true) {
            return Err(remote_changed(
                "UAI compound oral child is not marked submitted",
            ));
        }
        if child.get("value") != Some(expected.value.as_value()) {
            return Err(remote_changed(
                "UAI compound oral readback value differs from its submission",
            ));
        }
        match &expected.extra {
            Some(extra) if child.get("extra") == Some(extra.as_value()) => {}
            None if child.get("extra").is_none() => {}
            _ => {
                return Err(remote_changed(
                    "UAI compound oral readback extra differs from its submission",
                ));
            }
        }
    }
    Ok(())
}

fn remote_component(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| is_remote_component(value))
        .map(str::to_owned)
        .ok_or_else(|| protocol_drift(format!("UAI {label} is invalid")))
}

fn remote_identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| value.trim() == *value && !value.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn valid_oral_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

fn is_remote_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_input(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use aes::{
        Aes128,
        cipher::{BlockCipherEncrypt, KeyInit},
    };
    use asterism_domain::{
        AnswerCandidateId, AnswerPair, AnswerSource, AssessmentClass, NormalizedAnswer, ProviderId,
        Question, QuestionId, QuestionKind, QuestionSnapshotId, RemoteState, SelectedAnswer,
        SourceType, SubmissionAnswerCoverage, SubmissionDraftId, SubmissionPayloadEncoding,
        SubmissionPayloadFieldPreview, SubmissionPayloadPreview, TaskId,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;

    #[test]
    fn exact_matching_and_empty_oral_build_one_atomic_body() {
        let draft = ordinary_draft();
        let answer = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": ""}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &draft,
            &answer,
        )
        .unwrap();
        assert_eq!(submission.ordinary_draft_id(), draft.id);
        assert_eq!(submission.oral_instance_id(), "6001");
        assert!(
            submission
                .fingerprint()
                .starts_with("uai-compound-oral-v1:")
        );
        assert!(!format!("{submission:?}").contains("6001"));

        let body =
            build_compound_oral_submission_body(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["quesDatas"].as_array().unwrap().len(), 2);
        assert_eq!(body["quesDatas"][0]["instanceId"], "5001");
        assert_eq!(body["quesDatas"][1]["instanceId"], "6001");
        assert_eq!(body["quesDatas"][0]["answerVersion"], 0);
        assert_eq!(body["quesDatas"][1]["answerVersion"], 0);
        assert_eq!(body["isCompleted"], json!([true, true]));
        let oral_answer: Value =
            serde_json::from_str(body["quesDatas"][1]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(oral_answer["children"][0]["value"], "");
        let judges: Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges[0]["question_type"], "basic");
        assert_eq!(judges[0]["reply_type"], "scoop");
        assert_eq!(judges[1]["question_type"], "oral-sentence");
        assert_eq!(judges[1]["versions"]["course"], 123_290);
    }

    #[test]
    fn atomic_oral_request_digest_binds_route_account_and_complete_body() {
        let answer = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": ""}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &ordinary_draft(),
            &answer,
        )
        .unwrap();
        let request =
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let duplicate =
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        assert_ne!(request.request_digest(), [0; 32]);
        assert_eq!(request.request_digest(), duplicate.request_digest());
        assert_ne!(
            request.request_digest(),
            build_compound_oral_submission_request(&submission, "course-instance-2", "openid-1",)
                .unwrap()
                .request_digest()
        );
        assert_ne!(
            request.request_digest(),
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-2",)
                .unwrap()
                .request_digest()
        );
        let debug = format!("{request:?}");
        assert!(debug.contains("[ROUTE]") && debug.contains("[REDACTED]"));
        assert!(!debug.contains("6001") && !debug.contains("openid-1"));
    }

    #[test]
    fn atomic_oral_request_digest_changes_with_the_matching_answer() {
        let answer = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": ""}
        ]));
        let first = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &ordinary_draft(),
            &answer,
        )
        .unwrap();
        let mut changed_draft = ordinary_draft();
        changed_draft.items[0].selected.answer = NormalizedAnswer::Pairs(vec![AnswerPair {
            left: "left".to_owned(),
            right: "changed".to_owned(),
        }]);
        changed_draft.validate().unwrap();
        let changed = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &changed_draft,
            &answer,
        )
        .unwrap();
        let first = build_compound_oral_submission_request(&first, "course-instance-1", "openid-1")
            .unwrap();
        let changed =
            build_compound_oral_submission_request(&changed, "course-instance-1", "openid-1")
                .unwrap();
        assert_ne!(first.request_digest(), changed.request_digest());
    }

    #[test]
    fn oral_evidence_freezes_empty_nonempty_and_dynamic_child_values() {
        let empty_array = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": json!({"children":[{"answers":[],"answersExtra":{}},{"value":""}]}).to_string()}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &ordinary_draft(),
            &empty_array,
        )
        .unwrap();
        let body =
            build_compound_oral_submission_body(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        let oral_answer: Value =
            serde_json::from_str(body["quesDatas"][1]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(oral_answer["children"].as_array().unwrap().len(), 2);
        assert_eq!(oral_answer["children"][0]["value"], json!([]));
        assert_eq!(oral_answer["children"][1]["value"], json!([]));

        let non_empty = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": json!({"children":[{"value":["spoken"]}]}).to_string()}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &ordinary_draft(),
            &non_empty,
        )
        .unwrap();
        let body =
            build_compound_oral_submission_body(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        let oral_answer: Value =
            serde_json::from_str(body["quesDatas"][1]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(oral_answer["children"][0]["value"], json!(["spoken"]));
        let judges: Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges[1]["value"], "spoken");

        let dynamic_extra = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": json!({"children":[
                {"answers":["spoken"],"value":["ignored"],"answersExtra":{"slot":1}},
                {"value":"second","answersExtra":{"slot":2}}
            ]}).to_string()}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &ordinary_draft(),
            &dynamic_extra,
        )
        .unwrap();
        let body =
            build_compound_oral_submission_body(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        let oral_answer: Value =
            serde_json::from_str(body["quesDatas"][1]["answer"].as_str().unwrap()).unwrap();
        assert_eq!(oral_answer["children"][0]["value"], json!(["spoken"]));
        assert_eq!(oral_answer["children"][0]["extra"], json!({"slot":1}));
        assert_eq!(oral_answer["children"][1]["value"], "second");
        assert_eq!(oral_answer["children"][1]["extra"], json!({"slot":2}));
        let judges: Value =
            serde_json::from_str(body["thirdPartyJudges"].as_str().unwrap()).unwrap();
        assert_eq!(judges[1]["value"], "spoken");
        assert_eq!(judges[2]["value"], "second");

        let unsupported = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": json!({"children":[{"value":{"unstable":"object"}}]}).to_string()}
        ]));
        assert!(
            build_compound_oral_submission(
                &detail(&["basic-scoop-content", "oral-sentence"]),
                "group:2001:unit-1:group-oral",
                &ordinary_draft(),
                &unsupported,
            )
            .is_err()
        );
    }

    #[test]
    fn reordered_changed_or_unversioned_compound_oral_fails_closed() {
        let answer = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": ""}
        ]));
        assert!(
            build_compound_oral_submission(
                &detail(&["oral-sentence", "basic-scoop-content"]),
                "group:2001:unit-1:group-oral",
                &ordinary_draft(),
                &answer,
            )
            .is_err()
        );
        let mut missing_version = detail(&["basic-scoop-content", "oral-sentence"]);
        missing_version.normalized_detail["task"]
            .as_object_mut()
            .unwrap()
            .remove("course_publish_version");
        assert!(
            build_compound_oral_submission(
                &missing_version,
                "group:2001:unit-1:group-oral",
                &ordinary_draft(),
                &answer,
            )
            .is_err()
        );
        let changed_instance = encrypted_answer(&json!([
            {"id": 9999, "answer": ""},
            {"id": 6001, "answer": ""}
        ]));
        assert!(
            build_compound_oral_submission(
                &detail(&["basic-scoop-content", "oral-sentence"]),
                "group:2001:unit-1:group-oral",
                &ordinary_draft(),
                &changed_instance,
            )
            .is_err()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture covers current policy, legacy no-policy and both ordered modules"
    )]
    fn receipt_readback_must_preserve_both_compound_oral_modules() {
        let draft = ordinary_draft();
        let answer = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": ""}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &draft,
            &answer,
        )
        .unwrap();
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some("synthetic accepted compound oral".to_owned()),
            provider_trace_id: Some("compound-oral-v1".to_owned()),
            received_at: Utc::now(),
        };
        let ordinary_answer = json!({
            "value": [],
            "children": [{"value": ["right"], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let oral_answer = json!({
            "value": [],
            "children": [{"value": "", "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let questions = json!([
            {"instanceId":"5001","answer":ordinary_answer,"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"6001","answer":oral_answer,"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        let document = verification_document(&questions);
        let verified = parse_compound_oral_verification(&document, &submission, &receipt).unwrap();
        assert_eq!(verified.ordinary_draft_id(), draft.id);
        assert_eq!(verified.oral_instance_id(), "6001");
        assert_eq!(
            verified.result_digest(),
            <[u8; 32]>::from(Sha256::digest(document.as_bytes()))
        );
        assert_eq!(
            verified.score(),
            Some(SubmissionScore {
                earned_milli_points: 88_500,
                possible_milli_points: 100_000,
            })
        );
        let policy = verified.policy().unwrap();
        assert_eq!(policy.group_id(), "group-oral");
        assert_eq!(policy.submission_version(), "compound-oral-v1");
        assert_eq!(policy.strategy_id(), 3001);
        assert_eq!(
            policy.result_digest(),
            <[u8; 32]>::from(Sha256::digest(document.as_bytes()))
        );
        assert!(verified.requires_fresh_progress_read());

        let mut legacy: Value = serde_json::from_str(&document).unwrap();
        legacy["data"]["state"]["__EXTEND_DATA__"]["__SUBMIT_INFO__"] = json!({
            "course_id": "course-instance-1",
            "group_id": "group-oral",
            "version": "compound-oral-v1",
        });
        legacy["data"]["state"]["__EXTEND_DATA__"]
            .as_object_mut()
            .unwrap()
            .remove("__SUMMARY__");
        let legacy_document = serde_json::to_string(&legacy).unwrap();
        let legacy_verified =
            parse_compound_oral_verification(&legacy_document, &submission, &receipt).unwrap();
        assert_eq!(legacy_verified.policy(), None);
        assert_eq!(
            legacy_verified.result_digest(),
            <[u8; 32]>::from(Sha256::digest(legacy_document.as_bytes()))
        );

        let changed_oral = json!({
            "value": [],
            "children": [{"value": ["spoken"], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let changed_questions = json!([
            {"instanceId":"5001","answer":ordinary_answer,"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"6001","answer":changed_oral,"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        assert!(
            parse_compound_oral_verification(
                &verification_document(&changed_questions),
                &submission,
                &receipt,
            )
            .is_err()
        );
        let reversed = json!([
            {"instanceId":"6001","answer":json!({"children":[{"value":"","isDone":true}]}).to_string(),"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"5001","answer":json!({"children":[{"value":["right"],"isDone":true}]}).to_string(),"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        assert!(
            parse_compound_oral_verification(
                &verification_document(&reversed),
                &submission,
                &receipt,
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_readback_preserves_dynamic_oral_value_and_extra() {
        let draft = ordinary_draft();
        let answer = encrypted_answer(&json!([
            {"id": 5001, "answer": ""},
            {"id": 6001, "answer": json!({"children":[{
                "answers":["spoken"],
                "answersExtra":{"mediaId":"clip-1","duration":1.25}
            }]}).to_string()}
        ]));
        let submission = build_compound_oral_submission(
            &detail(&["basic-scoop-content", "oral-sentence"]),
            "group:2001:unit-1:group-oral",
            &draft,
            &answer,
        )
        .unwrap();
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some("synthetic accepted compound oral".to_owned()),
            provider_trace_id: Some("compound-oral-v1".to_owned()),
            received_at: Utc::now(),
        };
        let ordinary_answer = json!({
            "value": [],
            "children": [{"value": ["right"], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let oral_answer = json!({
            "value": [],
            "children": [{
                "value": ["spoken"],
                "extra": {"mediaId":"clip-1","duration":1.25},
                "isDone": true
            }],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let questions = json!([
            {"instanceId":"5001","answer":ordinary_answer,"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"6001","answer":oral_answer,"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        parse_compound_oral_verification(&verification_document(&questions), &submission, &receipt)
            .unwrap();

        let changed_oral = json!({
            "value": [],
            "children": [{"value": ["spoken"], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let changed = json!([
            {"instanceId":"5001","answer":ordinary_answer,"context":"{\"state\":\"submitted\"}"},
            {"instanceId":"6001","answer":changed_oral,"context":"{\"state\":\"submitted\"}"}
        ])
        .to_string();
        assert!(
            parse_compound_oral_verification(
                &verification_document(&changed),
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
            remote_question_id: Some("5001".to_owned()),
            kind: QuestionKind::Matching,
            stem: "Match the expression".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "basic-scoop-content",
                "remote_task_id": "group:2001:unit-1:group-oral",
                "judge_types": [{"question_type":"basic","reply_type":"scoop"}],
                "matching_lefts": ["left"]
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
                    answer: NormalizedAnswer::Pairs(vec![AnswerPair {
                        left: "left".to_owned(),
                        right: "right".to_owned(),
                    }]),
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
            "group_id": "group-oral",
            "course_publish_version": 123_290,
            "task_types": task_types,
            "question_count": task_types.len(),
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "group:2001:unit-1:group-oral".to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Compound oral".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "v1:compound-oral".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: json!({"schema":"uai.group-task.raw.v1"}),
            },
            normalized_detail: json!({
                "schema":"uai.group-task-detail.v1",
                "task":normalized
            }),
        }
    }

    fn encrypted_answer(plaintext: &Value) -> String {
        let suffix = "12345678";
        let key = format!("1a2b3c4d{suffix}");
        let cipher = Aes128::new_from_slice(key.as_bytes()).unwrap();
        let mut bytes = serde_json::to_vec(plaintext).unwrap();
        let padding = 16 - (bytes.len() % 16);
        bytes.extend(std::iter::repeat_n(u8::try_from(padding).unwrap(), padding));
        for chunk in bytes.chunks_exact_mut(16) {
            let block: &mut [u8; 16] = chunk.try_into().unwrap();
            cipher.encrypt_block(block.into());
        }
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(encoded, "{byte:02x}").unwrap();
        }
        json!({
            "code": 0,
            "data": format!("unipus.{encoded}"),
            "k": suffix
        })
        .to_string()
    }

    fn verification_document(questions: &str) -> String {
        json!({
            "success": true,
            "code": 0,
            "data": {
                "course": "course-instance-1",
                "module": "group-oral-compound-oral-v1",
                "state": {
                    "version": "compound-oral-v1",
                    "quesData": questions,
                    "__EXTEND_DATA__": {"__SUBMIT_INFO__": {
                        "course_id":"course-instance-1",
                        "group_id":"group-oral",
                        "version":"compound-oral-v1",
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
                            "not_start":false,
                            "score_avg":88.5
                        }
                    }, "__SUMMARY__": {"answerList": {
                        "0":{"questionType":1},
                        "1":{"questionType":2}
                    }}}
                }
            }
        })
        .to_string()
    }
}
