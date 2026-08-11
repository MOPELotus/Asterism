use std::{fmt, sync::Arc};

use asterism_domain::{
    SubmissionDraft, SubmissionQuestionVerification, SubmissionQuestionVerificationStatus,
    SubmissionReceipt, SubmissionVerificationSnapshot, SubmissionVerificationStatus,
    TaskCapability,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, SubmissionBuildCapability, SubmissionVerifyCapability, TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use zeroize::Zeroize;

use crate::{UaiSubmissionBuild, UaiSubmissionPlan, metadata::development_metadata};

const MAX_VERIFICATION_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NESTED_QUESTION_DATA_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_NESTED_ANSWER_BYTES: usize = 1_024 * 1_024;
const MAX_NESTED_CONTEXT_BYTES: usize = 64 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_SUBMISSION_VERSION_BYTES: usize = 256;

/// Redacted ownership wrapper for one bounded fresh UAI user-module response.
pub struct UaiVerificationDocument(String);

impl UaiVerificationDocument {
    /// Owns one bounded JSON response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized documents.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_VERIFICATION_DOCUMENT_BYTES {
            return Err(invalid_response(
                "UAI submission verification response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiVerificationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiVerificationDocument")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiVerificationDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Read-only boundary for one exact receipt-versioned user-module snapshot.
#[async_trait]
pub trait UaiVerificationTransport: Send + Sync {
    async fn fetch_verification(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
        submission_version: &str,
    ) -> ProviderResult<UaiVerificationDocument>;
}

/// Distinct UAI post-submit verification capability. It never issues the
/// submission mutation and does not infer Task completion from a receipt.
pub struct UaiSubmissionVerify {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    preview: UaiSubmissionBuild,
    transport: Arc<dyn UaiVerificationTransport>,
}

impl UaiSubmissionVerify {
    /// Builds verification around fresh Task detail and user-module reads.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn UaiVerificationTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            preview: UaiSubmissionBuild::try_new()?,
            transport,
        })
    }
}

impl fmt::Debug for UaiSubmissionVerify {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionVerify")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("preview", &self.preview)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiSubmissionVerify {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionVerifyCapability for UaiSubmissionVerify {
    async fn verify_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        validate_context(context, &self.metadata)?;
        let identity = GroupIdentity::parse(remote_task_id)?;
        validate_draft(draft, &self.metadata)?;
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
            return Err(invalid_response(
                "UAI submission verification draft preview is stale or foreign",
            ));
        }
        let Some(receipt) = receipt else {
            return inconclusive_snapshot(draft);
        };
        receipt
            .validate()
            .map_err(|_| invalid_response("UAI submission verification receipt is invalid"))?;
        let version = receipt
            .provider_trace_id
            .as_deref()
            .filter(|value| valid_submission_version(value))
            .filter(|_| receipt.remote_status == "accepted")
            .ok_or_else(|| {
                invalid_response("UAI submission verification requires an accepted version receipt")
            })?;

        let detail = self.details.task_detail(context, remote_task_id).await?;
        let task_type = validate_fresh_detail(&detail, &identity, remote_task_id)?;
        let plan = UaiSubmissionPlan::from_draft(draft, task_type)?;
        let document = self
            .transport
            .fetch_verification(context, &identity.course_resource, &identity.group, version)
            .await?;
        parse_verification_snapshot(document.as_str(), &identity.group, version, &plan, draft)
    }
}

/// Parses and strictly binds one fresh user-module response to the exact draft
/// answer and receipt version.
///
/// # Errors
///
/// Returns invalid-response, protocol-drift, or remote-changed errors for
/// malformed nesting, duplicate identities, route drift, or changed answers.
pub fn parse_verification_snapshot(
    document: &str,
    expected_group_id: &str,
    expected_version: &str,
    plan: &UaiSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    if document.is_empty() || document.len() > MAX_VERIFICATION_DOCUMENT_BYTES {
        return Err(invalid_response(
            "UAI submission verification response is empty or exceeds the size limit",
        ));
    }
    let response: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI submission verification response is not valid JSON"))?;
    let state = bound_verification_state(&response, expected_group_id, expected_version)?;
    let mut remote_children = parse_remote_question(state, plan, draft.items.len())?;
    if remote_children != plan.answer_children() {
        remote_children.zeroize();
        return Err(remote_changed(
            "UAI user-module answer differs from the immutable submission draft",
        ));
    }
    remote_children.zeroize();

    let snapshot = SubmissionVerificationSnapshot {
        status: SubmissionVerificationStatus::Confirmed,
        remote_state: None,
        score: None,
        progress_percent: None,
        questions: vec![SubmissionQuestionVerification {
            question_id: draft.items[0].question.id,
            status: SubmissionQuestionVerificationStatus::Confirmed,
        }],
        verified_at: Utc::now(),
    };
    snapshot
        .validate()
        .map_err(|_| invalid_response("UAI submission verification snapshot is invalid"))?;
    Ok(snapshot)
}

fn bound_verification_state<'a>(
    response: &'a Value,
    expected_group_id: &str,
    expected_version: &str,
) -> ProviderResult<&'a serde_json::Map<String, Value>> {
    let response = response
        .as_object()
        .ok_or_else(|| protocol_drift("UAI submission verification response is not an object"))?;
    if response.get("success").and_then(Value::as_bool) != Some(true)
        || response.get("code").and_then(Value::as_i64) != Some(0)
    {
        return Err(invalid_response(
            "UAI submission verification read did not succeed",
        ));
    }
    let state = response
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("state"))
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI submission verification has no state object"))?;
    if state.get("version").and_then(Value::as_str) != Some(expected_version) {
        return Err(remote_changed(
            "UAI user-module version does not match the submission receipt",
        ));
    }
    let submit_info = state
        .get("__EXTEND_DATA__")
        .and_then(Value::as_object)
        .and_then(|value| value.get("__SUBMIT_INFO__"))
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI user-module has no submit-info binding"))?;
    if submit_info.get("group_id").and_then(Value::as_str) != Some(expected_group_id)
        || submit_info.get("version").and_then(Value::as_str) != Some(expected_version)
    {
        return Err(remote_changed(
            "UAI user-module submit-info does not match the receipt binding",
        ));
    }
    Ok(state)
}

fn parse_remote_question(
    state: &serde_json::Map<String, Value>,
    plan: &UaiSubmissionPlan,
    expected_count: usize,
) -> ProviderResult<Vec<Vec<String>>> {
    let mut question_data = state
        .get("quesData")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_QUESTION_DATA_BYTES)
        .ok_or_else(|| protocol_drift("UAI user-module has no bounded Question data"))?
        .to_owned();
    let question_data_value: Value = serde_json::from_str(&question_data)
        .map_err(|_| invalid_response("UAI user-module Question data is not valid JSON"))?;
    question_data.zeroize();
    let entries = question_data_value
        .as_array()
        .ok_or_else(|| protocol_drift("UAI user-module Question data is not an array"))?;
    if entries.len() != 1 || expected_count != 1 {
        return Err(remote_changed(
            "UAI user-module Question count does not match the submission draft",
        ));
    }
    let entry = entries[0]
        .as_object()
        .ok_or_else(|| protocol_drift("UAI user-module Question entry is not an object"))?;
    if entry.get("instanceId").and_then(remote_identity).as_deref()
        != Some(plan.remote_question_id())
    {
        return Err(remote_changed(
            "UAI user-module Question identity does not match the submission draft",
        ));
    }
    let context = entry
        .get("context")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_CONTEXT_BYTES)
        .ok_or_else(|| protocol_drift("UAI user-module has no bounded submission context"))?;
    let context: Value = serde_json::from_str(context)
        .map_err(|_| invalid_response("UAI user-module submission context is not valid JSON"))?;
    if context.get("state").and_then(Value::as_str) != Some("submitted") {
        return Err(remote_changed(
            "UAI user-module Question is not in submitted state",
        ));
    }
    let mut answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_NESTED_ANSWER_BYTES)
        .ok_or_else(|| protocol_drift("UAI user-module has no bounded answer data"))?
        .to_owned();
    let answer_value: Value = serde_json::from_str(&answer)
        .map_err(|_| invalid_response("UAI user-module answer data is not valid JSON"))?;
    answer.zeroize();
    let children = answer_value
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI user-module answer has no children"))?;
    let mut remote_children = Vec::with_capacity(children.len());
    for child in children {
        if child.get("isDone").and_then(Value::as_bool) != Some(true) {
            return Err(remote_changed(
                "UAI user-module answer child is not marked submitted",
            ));
        }
        let values = child
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI user-module answer child has no values"))?;
        let mut normalized = Vec::with_capacity(values.len());
        for value in values {
            let value = value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| protocol_drift("UAI user-module answer contains invalid text"))?;
            normalized.push(value.to_owned());
        }
        remote_children.push(normalized);
    }
    Ok(remote_children)
}

pub(crate) fn validate_verification_course_binding(
    document: &str,
    expected_course_instance_id: &str,
) -> ProviderResult<()> {
    let response: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI submission verification response is not valid JSON"))?;
    let course = response
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("course"))
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI submission verification has no Course binding"))?;
    if course != expected_course_instance_id {
        return Err(remote_changed(
            "UAI submission verification Course does not match its fresh route",
        ));
    }
    Ok(())
}

fn inconclusive_snapshot(
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let snapshot = SubmissionVerificationSnapshot {
        status: SubmissionVerificationStatus::Inconclusive,
        remote_state: None,
        score: None,
        progress_percent: None,
        questions: draft
            .items
            .iter()
            .map(|item| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: SubmissionQuestionVerificationStatus::Unverified,
            })
            .collect(),
        verified_at: Utc::now(),
    };
    snapshot
        .validate()
        .map_err(|_| invalid_response("UAI inconclusive verification snapshot is invalid"))?;
    Ok(snapshot)
}

fn validate_draft(draft: &SubmissionDraft, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if draft.provider_id != metadata.id
        || draft.provider_version != metadata.implementation_version
        || draft.validate().is_err()
    {
        return Err(invalid_response(
            "UAI submission verification received an invalid or stale draft",
        ));
    }
    Ok(())
}

fn validate_fresh_detail<'a>(
    detail: &'a asterism_provider_api::RemoteTaskDetail,
    identity: &GroupIdentity,
    remote_task_id: &str,
) -> ProviderResult<&'a str> {
    if detail.task.remote_id != remote_task_id
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionVerify)
    {
        return Err(unsupported(
            "UAI fresh Group does not advertise submission verification",
        ));
    }
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("UAI fresh Group detail has no normalized Task"))?;
    if task.get("course_resource_id").and_then(Value::as_str)
        != Some(identity.course_resource.as_str())
        || task
            .get("unit")
            .and_then(Value::as_object)
            .and_then(|unit| unit.get("id"))
            .and_then(Value::as_str)
            != Some(identity.unit.as_str())
        || task.get("group_id").and_then(Value::as_str) != Some(identity.group.as_str())
        || task.get("question_count").and_then(Value::as_u64) != Some(1)
    {
        return Err(remote_changed(
            "UAI Group identity or Question count changed before verification",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI fresh Group detail has no task types"))?;
    if task_types.len() != 1 {
        return Err(unsupported(
            "UAI submission verification requires one simple Group task type",
        ));
    }
    task_types[0]
        .as_str()
        .filter(|value| matches!(*value, "single-choice" | "multichoice" | "short_answer"))
        .ok_or_else(|| unsupported("UAI fresh Group task type is not verifiable"))
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
            return Err(invalid_response("UAI Group Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("group") {
            return Err(unsupported(
                "UAI submission verification supports Group Tasks only",
            ));
        }
        let course_resource = valid_component(components.next())?;
        let unit = valid_component(components.next())?;
        let group = valid_component(components.next())?;
        if components.next().is_some() {
            return Err(invalid_response("UAI Group Task identity is invalid"));
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
        .ok_or_else(|| invalid_response("UAI Group Task identity is invalid"))
}

fn valid_submission_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBMISSION_VERSION_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn remote_identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI submission verification received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI submission verification requires an authenticated session",
        ));
    }
    Ok(())
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

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderAccountId, ProviderId, QuestionSnapshotId,
        SecretId, SelectedAnswer, SubmissionDraftId, SubmissionDraftItem, TaskId,
    };
    use asterism_provider_api::RemoteTaskDetail;

    use super::*;
    use crate::{
        parse_course_context, parse_course_inventory, parse_question_content, parse_task_inventory,
    };

    const VERIFIED: &str =
        include_str!("../../../fixtures/providers/uai/submissions/verified.json");
    const CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-multiple-choice.json");
    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str = include_str!("../../../fixtures/providers/uai/tasks/tree-mixed.json");

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
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
            let tree = TREE.replace("rich-text-read", "multichoice");
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
        calls: Mutex<Vec<(String, String, String)>>,
    }

    #[async_trait]
    impl UaiVerificationTransport for FixtureTransport {
        async fn fetch_verification(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            group_id: &str,
            submission_version: &str,
        ) -> ProviderResult<UaiVerificationDocument> {
            self.calls.lock().unwrap().push((
                course_resource_id.to_owned(),
                group_id.to_owned(),
                submission_version.to_owned(),
            ));
            UaiVerificationDocument::try_new(VERIFIED.to_owned())
        }
    }

    #[tokio::test]
    async fn exact_user_module_snapshot_confirms_the_submitted_question_only() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, "multichoice").unwrap();
        let snapshot =
            parse_verification_snapshot(VERIFIED, "group-1", "submit-version-42", &plan, &draft)
                .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.remote_state, None);
        assert_eq!(snapshot.score, None);
        assert_eq!(snapshot.questions.len(), 1);
        assert_eq!(
            snapshot.questions[0].status,
            SubmissionQuestionVerificationStatus::Confirmed
        );
        assert!(!format!("{snapshot:?}").contains("must_be_dropped"));
        validate_verification_course_binding(VERIFIED, "course-v2:synthetic+rw").unwrap();
        assert!(validate_verification_course_binding(VERIFIED, "changed").is_err());
    }

    #[tokio::test]
    async fn verifier_reads_the_exact_receipt_version_without_mutating() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = UaiSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
        )
        .unwrap();
        let draft = draft().await;
        let snapshot = capability
            .verify_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft,
                Some(&receipt()),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[(
                "2001".to_owned(),
                "group-1".to_owned(),
                "submit-version-42".to_owned(),
            )]
        );
    }

    #[tokio::test]
    async fn missing_receipt_is_inconclusive_without_guessing_a_route() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = UaiSubmissionVerify::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
        )
        .unwrap();
        let snapshot = capability
            .verify_submission(
                &context(),
                "group:2001:unit-1:group-1",
                &draft().await,
                None,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);
        assert_eq!(
            snapshot.questions[0].status,
            SubmissionQuestionVerificationStatus::Unverified
        );
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_remote_answer_fails_closed() {
        let draft = draft().await;
        let plan = UaiSubmissionPlan::from_draft(&draft, "multichoice").unwrap();
        let mut changed: Value = serde_json::from_str(VERIFIED).unwrap();
        let mut question_data: Value =
            serde_json::from_str(changed["data"]["state"]["quesData"].as_str().unwrap()).unwrap();
        let mut answer: Value =
            serde_json::from_str(question_data[0]["answer"].as_str().unwrap()).unwrap();
        answer["children"][0]["value"][1] = Value::String("C".to_owned());
        question_data[0]["answer"] = Value::String(serde_json::to_string(&answer).unwrap());
        changed["data"]["state"]["quesData"] =
            Value::String(serde_json::to_string(&question_data).unwrap());
        let changed = serde_json::to_string(&changed).unwrap();
        assert_eq!(
            parse_verification_snapshot(&changed, "group-1", "submit-version-42", &plan, &draft,)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    async fn draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let question =
            parse_question_content(CONTENT, "group-1", &["multichoice".to_owned()], Some(1))
                .unwrap()
                .remove(0)
                .to_question(task_id)
                .unwrap();
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: asterism_domain::NormalizedAnswer::Selections(vec![
                "A".to_owned(),
                "B".to_owned(),
            ]),
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
            provider_id: asterism_domain::ProviderId::new("uai").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            items: vec![SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    fn receipt() -> SubmissionReceipt {
        SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some(
                "UAI accepted the submission for later verification".to_owned(),
            ),
            provider_trace_id: Some("submit-version-42".to_owned()),
            received_at: Utc::now(),
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            correlation_id: "uai-submission-verify".to_owned(),
            credential_refs: vec![SecretId::new()],
        }
    }
}
