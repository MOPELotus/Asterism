use std::{fmt, sync::Arc};

use asterism_domain::{
    NormalizedAnswer, QuestionKind, SubmissionDraft, SubmissionReceipt, TaskCapability,
};
use asterism_provider_api::{
    ExecutionEventSink, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, ProviderRuntimeSettingsSchema,
    ResolvedProviderRuntimeSettings, SubmissionBuildCapability, SubmissionExecuteCapability,
    TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use zeroize::Zeroize;

use crate::{UaiSubmissionBuild, metadata::development_metadata};

const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_ANSWER_CHILDREN: usize = 256;
const MAX_ANSWER_VALUES_PER_CHILD: usize = 256;
const MAX_SUBMISSION_RESPONSE_BYTES: usize = 1_024 * 1_024;
const MAX_SUBMISSION_VERSION_BYTES: usize = 128;

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
    let response: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI submission response is not valid JSON"))?;
    let response = response
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

/// Ephemeral executable facts rebuilt from one immutable UAI draft. Debug
/// output is redacted and all owned answer strings are zeroized on drop.
pub struct UaiSubmissionPlan {
    remote_question_id: String,
    task_type: String,
    answer_children: Vec<Vec<String>>,
}

impl UaiSubmissionPlan {
    pub(crate) fn from_draft(draft: &SubmissionDraft, task_type: &str) -> ProviderResult<Self> {
        if draft.items.len() != 1 {
            return Err(unsupported(
                "UAI submission execution currently supports one-Question Groups only",
            ));
        }
        let item = &draft.items[0];
        let remote_question_id = item
            .question
            .remote_question_id
            .as_deref()
            .filter(|value| valid_question_identity(value))
            .ok_or_else(|| {
                invalid_input("UAI submission execution requires a bounded remote Question ID")
            })?
            .to_owned();
        let answer_children = match (item.question.kind, &item.selected.answer) {
            (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
                if values.len() == 1 && selections_exist(&item.question, values) =>
            {
                vec![values.clone()]
            }
            (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values))
                if selections_exist(&item.question, values) =>
            {
                vec![values.clone()]
            }
            (QuestionKind::ShortAnswer, NormalizedAnswer::Texts(values)) => {
                values.iter().map(|value| vec![value.clone()]).collect()
            }
            _ => {
                return Err(invalid_input(
                    "UAI submission execution answer does not match its Question kind",
                ));
            }
        };
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
        let expected_kind = question_kind(task_type).ok_or_else(|| {
            unsupported("UAI submission execution does not support this Group task type")
        })?;
        if item.question.kind != expected_kind
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

    #[cfg(test)]
    pub(crate) fn fixture(
        remote_question_id: &str,
        task_type: &str,
        answer_children: Vec<Vec<String>>,
    ) -> Self {
        Self {
            remote_question_id: remote_question_id.to_owned(),
            task_type: task_type.to_owned(),
            answer_children,
        }
    }
}

impl fmt::Debug for UaiSubmissionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiSubmissionPlan")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiSubmissionPlan {
    fn drop(&mut self) {
        self.remote_question_id.zeroize();
        self.task_type.zeroize();
        for child in &mut self.answer_children {
            child.zeroize();
        }
        self.answer_children.zeroize();
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
        group_id: &str,
        plan: &UaiSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt>;
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
            runtime_settings: ProviderRuntimeSettingsSchema::default(),
            details,
            preview: UaiSubmissionBuild::try_new()?,
            transport,
        })
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

        let detail = self.details.task_detail(context, remote_task_id).await?;
        let task_type = validate_fresh_detail(&detail, &identity, remote_task_id)?;
        let plan = UaiSubmissionPlan::from_draft(draft, task_type)?;
        let receipt = self
            .transport
            .submit(context, &identity.course_resource, &identity.group, &plan)
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
            != Some(1)
    {
        return Err(remote_changed(
            "UAI Group identity or Question count changed before submission",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_response("UAI fresh Group detail has no task types"))?;
    if task_types.len() != 1 {
        return Err(unsupported(
            "UAI submission execution requires one simple Group task type",
        ));
    }
    task_types[0]
        .as_str()
        .filter(|value| question_kind(value).is_some())
        .ok_or_else(|| unsupported("UAI fresh Group task type is not executable"))
}

fn selections_exist(question: &asterism_domain::Question, values: &[String]) -> bool {
    let options = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    !values.is_empty() && values.iter().all(|value| options.contains(value.as_str()))
}

const fn question_kind(task_type: &str) -> Option<QuestionKind> {
    match task_type.as_bytes() {
        b"single-choice" => Some(QuestionKind::SingleChoice),
        b"multichoice" => Some(QuestionKind::MultipleChoice),
        b"short_answer" => Some(QuestionKind::ShortAnswer),
        _ => None,
    }
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

fn valid_question_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_QUESTION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
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
    use std::{collections::BTreeMap, sync::Mutex};

    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderAccountId, ProviderId, QuestionSnapshotId,
        SecretId, SelectedAnswer, SubmissionDraftId, SubmissionDraftItem, TaskId,
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
        calls: Mutex<Vec<RecordedSubmission>>,
    }

    type RecordedSubmission = (String, String, String, String, Vec<Vec<String>>);

    #[async_trait]
    impl UaiSubmissionTransport for FixtureTransport {
        async fn submit(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            group_id: &str,
            plan: &UaiSubmissionPlan,
        ) -> ProviderResult<SubmissionReceipt> {
            self.calls.lock().unwrap().push((
                course_resource_id.to_owned(),
                group_id.to_owned(),
                plan.remote_question_id().to_owned(),
                plan.task_type().to_owned(),
                plan.answer_children().to_vec(),
            ));
            Ok(SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some(
                    "UAI accepted the submission for later verification".to_owned(),
                ),
                provider_trace_id: Some("submit-version-42".to_owned()),
                received_at: Utc::now(),
            })
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
        let capability = UaiSubmissionExecute::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
        )
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
            &[(
                "2001".to_owned(),
                "group-1".to_owned(),
                "question-1".to_owned(),
                "multichoice".to_owned(),
                vec![vec!["A".to_owned(), "B".to_owned()]],
            )]
        );
    }

    #[tokio::test]
    async fn stale_preview_fails_before_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = UaiSubmissionExecute::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
        )
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
    async fn ambiguous_transport_failure_is_returned_after_one_mutation_attempt() {
        let transport = Arc::new(AmbiguousFailureTransport::default());
        let capability = UaiSubmissionExecute::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
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
        assert_eq!(error.kind, ProviderErrorKind::Network);
        assert_eq!(*transport.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unexpected_receipt_semantics_are_rejected_after_the_mutation() {
        let capability = UaiSubmissionExecute::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
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
            items: vec![SubmissionDraftItem { question, selected }],
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    fn runtime_settings() -> ResolvedProviderRuntimeSettings {
        ResolvedProviderRuntimeSettings {
            schema_version: 1,
            values: BTreeMap::new(),
        }
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
