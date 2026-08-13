use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, SubmissionReceipt, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, ProviderRuntimeSettingsSchema, TaskDetailCapability, TaskExecutionCapability,
    TaskProgressCapability,
};
use async_trait::async_trait;

use crate::{
    metadata::development_metadata, runtime_settings::runtime_settings_schema,
    submission_execute::valid_submission_version,
};

const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;

/// Native boundary for an audited no-Question completion mutation.
/// Implementations must not retry an ambiguous mutation failure.
#[derive(Clone, Debug, PartialEq)]
pub enum UaiPresetCompletionResult {
    /// Fresh exact progress already proves completion, so no mutation ran.
    AlreadyCompleted,
    /// One mark-seen mutation returned a bounded accepted receipt.
    Submitted(SubmissionReceipt),
}

#[async_trait]
pub trait UaiPresetCompletionTransport: Send + Sync {
    async fn complete_preset(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiPresetCompletionResult>;

    /// Completes one exact `exit-ticket` Group with the donor-observed empty
    /// `submitType=2` body. Implementations must freshly bind the Group and
    /// must not replay an ambiguous POST.
    async fn complete_exit_ticket(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiPresetCompletionResult>;

    /// Submits the donor-configured empty-answer shape for one exact oral
    /// Group. This is intentionally distinct from no-Question completion.
    async fn complete_oral_empty(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        unit_id: &str,
        group_id: &str,
        task_type: &str,
        question_count: u32,
    ) -> ProviderResult<UaiPresetCompletionResult>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmptyCompletionKind {
    Preset,
    ExitTicket,
    Oral {
        task_type: OralTaskType,
        question_count: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OralTaskType {
    Sentence,
    VideoDub,
    PersonalState,
}

impl OralTaskType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Sentence => "oral-sentence",
            Self::VideoDub => "video-dub",
            Self::PersonalState => "oral-personal-state",
        }
    }
}

impl EmptyCompletionKind {
    const fn resource_kind(self) -> &'static str {
        match self {
            Self::Preset => "preset",
            Self::ExitTicket => "exit_ticket",
            Self::Oral { .. } => "oral_empty",
        }
    }

    const fn log_message(self) -> &'static str {
        match self {
            Self::Preset => "纯学习任务已通过新鲜详情校验，准备核验并按需提交完成标记",
            Self::ExitTicket => "出口问卷已通过新鲜详情校验，准备核验并提交 donor 定义的空完成标记",
            Self::Oral { .. } => "口语任务已通过新鲜详情校验，准备核验并提交 donor 定义的空答案",
        }
    }
}

/// Executes donor-audited UAI no-Question completion Groups. Core must
/// independently confirm completion through `TaskProgressRead` and recover
/// without replay.
pub struct UaiResourceExecution {
    metadata: ProviderMetadata,
    runtime_settings: ProviderRuntimeSettingsSchema,
    details: Arc<dyn TaskDetailCapability>,
    progress: Arc<dyn TaskProgressCapability>,
    transport: Arc<dyn UaiPresetCompletionTransport>,
}

impl UaiResourceExecution {
    /// Builds the non-idempotent preset execution boundary.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        progress: Arc<dyn TaskProgressCapability>,
        transport: Arc<dyn UaiPresetCompletionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            runtime_settings: runtime_settings_schema(),
            details,
            progress,
            transport,
        })
    }

    async fn submit_empty_completion(
        &self,
        context: &ProviderContext,
        identity: &GroupIdentity,
        completion_kind: EmptyCompletionKind,
    ) -> ProviderResult<UaiPresetCompletionResult> {
        match completion_kind {
            EmptyCompletionKind::Preset => {
                self.transport
                    .complete_preset(
                        context,
                        &identity.course_resource,
                        &identity.unit,
                        &identity.group,
                    )
                    .await
            }
            EmptyCompletionKind::ExitTicket => {
                self.transport
                    .complete_exit_ticket(
                        context,
                        &identity.course_resource,
                        &identity.unit,
                        &identity.group,
                    )
                    .await
            }
            EmptyCompletionKind::Oral {
                task_type,
                question_count,
            } => {
                self.transport
                    .complete_oral_empty(
                        context,
                        &identity.course_resource,
                        &identity.unit,
                        &identity.group,
                        task_type.as_str(),
                        question_count,
                    )
                    .await
            }
        }
    }
}

impl fmt::Debug for UaiResourceExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiResourceExecution")
            .field("metadata", &self.metadata)
            .field("runtime_settings", &self.runtime_settings)
            .field("details", &"configured")
            .field("progress", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiResourceExecution {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskExecutionCapability for UaiResourceExecution {
    fn requires_execution_verification(&self, requested_capabilities: &[TaskCapability]) -> bool {
        requested_capabilities == [TaskCapability::ResourceExecution]
    }

    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(unsupported(
                "UAI preset execution accepts only ResourceExecution",
            ));
        }
        self.runtime_settings
            .validate_resolved(&request.runtime_settings)
            .map_err(|_| internal("UAI preset execution settings snapshot is invalid"))?;

        let identity = GroupIdentity::parse(&request.remote_task_id)?;
        let detail = self
            .details
            .task_detail(context, &request.remote_task_id)
            .await?;
        let completion_kind =
            validate_fresh_empty_completion_detail(&detail, &identity, &request.remote_task_id)?;

        events
            .log(ProviderExecutionLog {
                level: asterism_domain::LogLevel::Info,
                stage: format!("{}_submit", completion_kind.resource_kind()),
                message: completion_kind.log_message().to_owned(),
                provider_trace_id: None,
                metadata_sanitized: Some(serde_json::json!({
                    "resource_kind": completion_kind.resource_kind(),
                    "verification": "required_fresh_progress",
                })),
            })
            .await?;

        let completion = self
            .submit_empty_completion(context, &identity, completion_kind)
            .await?;
        let already_completed = match completion {
            UaiPresetCompletionResult::AlreadyCompleted => true,
            UaiPresetCompletionResult::Submitted(receipt) => {
                receipt
                    .validate()
                    .map_err(|_| invalid_response("UAI empty completion receipt is invalid"))?;
                if receipt.remote_status != "accepted"
                    || receipt
                        .provider_trace_id
                        .as_deref()
                        .is_none_or(|version| !valid_submission_version(version))
                {
                    return Err(invalid_response(
                        "UAI empty completion requires an accepted version receipt",
                    ));
                }
                false
            }
        };

        events
            .report(ProviderProgress {
                percent: Some(50),
                stage: if already_completed {
                    format!("{}_already_completed", completion_kind.resource_kind())
                } else {
                    format!("{}_submitted", completion_kind.resource_kind())
                },
                status_text: Some(
                    if already_completed {
                        "提交前的新鲜进度已显示完成，等待 Core 独立复核"
                    } else {
                        "完成标记已受理，等待 Core 独立进度复核"
                    }
                    .to_owned(),
                ),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        Ok(ExecutionOutcome {
            remote_state: RemoteState::Unknown,
            verified: false,
            result_sanitized: serde_json::json!({
                "schema": "uai.empty-completion.v1",
                "resource_kind": completion_kind.resource_kind(),
                "mutation_submitted": !already_completed,
                "already_completed": already_completed,
                "verification": "required_fresh_progress",
            }),
        })
    }

    async fn verify_execution(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(unsupported(
                "UAI preset verification accepts only ResourceExecution",
            ));
        }
        self.runtime_settings
            .validate_resolved(&request.runtime_settings)
            .map_err(|_| internal("UAI preset verification settings snapshot is invalid"))?;
        let identity = GroupIdentity::parse(&request.remote_task_id)?;
        let detail = self
            .details
            .task_detail(context, &request.remote_task_id)
            .await?;
        let completion_kind =
            validate_fresh_empty_completion_detail(&detail, &identity, &request.remote_task_id)?;
        let progress = self
            .progress
            .read_progress(context, &request.remote_task_id)
            .await?;
        let goal_matched = progress.remote_state == RemoteState::Completed;
        Ok(ExecutionOutcome {
            remote_state: progress.remote_state,
            verified: goal_matched,
            result_sanitized: serde_json::json!({
                "schema": "uai.empty-completion-verification.v1",
                "resource_kind": completion_kind.resource_kind(),
                "goal_matched": goal_matched,
                "verification": "fresh_exact_group_progress_no_mutation",
            }),
        })
    }
}

pub(crate) fn supports_preset_execution(task_types: &[String]) -> bool {
    !task_types.is_empty()
        && task_types.iter().all(|task_type| {
            matches!(
                task_type.as_str(),
                "rich-text-read" | "text-learn" | "vocabulary" | "input" | "video-point-read"
            )
        })
}

pub(crate) fn supports_empty_completion_execution(
    task_types: &[String],
    question_count: Option<u32>,
) -> bool {
    empty_completion_kind(task_types, question_count).is_some()
}

fn empty_completion_kind(
    task_types: &[String],
    question_count: Option<u32>,
) -> Option<EmptyCompletionKind> {
    if supports_preset_execution(task_types) {
        Some(EmptyCompletionKind::Preset)
    } else if task_types == ["exit-ticket"] {
        Some(EmptyCompletionKind::ExitTicket)
    } else {
        let task_type = match task_types {
            [task_type] if task_type == "oral-sentence" => OralTaskType::Sentence,
            [task_type] if task_type == "video-dub" => OralTaskType::VideoDub,
            [task_type] if task_type == "oral-personal-state" => OralTaskType::PersonalState,
            _ => return None,
        };
        question_count
            .filter(|question_count| (1..=128).contains(question_count))
            .map(|question_count| EmptyCompletionKind::Oral {
                task_type,
                question_count,
            })
    }
}

fn validate_fresh_empty_completion_detail(
    detail: &asterism_provider_api::RemoteTaskDetail,
    identity: &GroupIdentity,
    remote_task_id: &str,
) -> ProviderResult<EmptyCompletionKind> {
    if detail.task.remote_id != remote_task_id
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::ResourceExecution)
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::ExecutionVerify)
        || !detail
            .task
            .capabilities
            .contains(&TaskCapability::ProgressRead)
        || detail
            .task
            .capabilities
            .contains(&TaskCapability::SubmissionExecute)
    {
        return Err(unsupported(
            "UAI fresh Group does not advertise verified empty completion",
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
    {
        return Err(remote_changed(
            "UAI Group identity changed before empty completion",
        ));
    }
    let task_types = task
        .get("task_types")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_response("UAI fresh Group detail has no task types"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_response("UAI fresh Group task type is not text"))
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    let question_count = task
        .get("question_count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    empty_completion_kind(&task_types, question_count).ok_or_else(|| {
        unsupported("UAI fresh Group is not an audited no-Question completion family")
    })
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
                "UAI preset execution supports Group Tasks only",
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

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "UAI preset execution received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI preset execution requires an authenticated session",
        ));
    }
    Ok(())
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
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
        AssessmentClass, CourseId, ProviderAccountId, ProviderId, RemoteState, SecretId,
        SourceType, TaskId,
    };
    use asterism_provider_api::{RemoteProgress, RemoteTask, RemoteTaskDetail};
    use chrono::Utc;

    use super::*;

    #[derive(Debug)]
    struct FixtureDetail {
        task_types: Vec<String>,
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let normalized = serde_json::json!({
                "schema": "uai.group-task.v1",
                "course_resource_id": "2001",
                "unit": {"id": "unit-1", "title": "Unit"},
                "section": null,
                "micro": null,
                "group_id": "group-1",
                "task_types": self.task_types,
                "question_count": 1,
            });
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course-resource:2001".to_owned()),
                    title: "Preset".to_owned(),
                    source_type: SourceType::Resource,
                    assessment_class: AssessmentClass::Routine,
                    remote_state: RemoteState::Unknown,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: vec![
                        TaskCapability::ProgressRead,
                        TaskCapability::DurationRead,
                        TaskCapability::ResourceExecution,
                        TaskCapability::ExecutionVerify,
                    ],
                    fingerprint: "v1:synthetic".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: serde_json::json!({"schema": "uai.group-task.raw.v1"}),
                },
                normalized_detail: serde_json::json!({
                    "schema": "uai.group-task-detail.v1",
                    "task": normalized,
                }),
            })
        }
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            panic!("fixture detail identity is unused")
        }
    }

    #[derive(Debug)]
    struct FixtureProgress {
        remote_state: RemoteState,
        calls: Mutex<Vec<String>>,
    }

    impl ProviderIdentity for FixtureProgress {
        fn metadata(&self) -> &ProviderMetadata {
            panic!("fixture progress identity is unused")
        }
    }

    #[async_trait]
    impl TaskProgressCapability for FixtureProgress {
        async fn read_progress(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteProgress> {
            self.calls.lock().unwrap().push(remote_task_id.to_owned());
            Ok(RemoteProgress {
                remote_state: self.remote_state,
                percent: (self.remote_state == RemoteState::Completed).then_some(100),
                duration_seconds: None,
                updated_at: Utc::now(),
            })
        }
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<(String, String, String, &'static str)>>,
        fail: bool,
        already_completed: bool,
    }

    #[async_trait]
    impl UaiPresetCompletionTransport for FixtureTransport {
        async fn complete_preset(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
            group_id: &str,
        ) -> ProviderResult<UaiPresetCompletionResult> {
            self.calls.lock().unwrap().push((
                course_resource_id.to_owned(),
                unit_id.to_owned(),
                group_id.to_owned(),
                "preset",
            ));
            if self.fail {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "synthetic ambiguous mutation failure",
                ));
            }
            if self.already_completed {
                return Ok(UaiPresetCompletionResult::AlreadyCompleted);
            }
            Ok(UaiPresetCompletionResult::Submitted(SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some("accepted for verification".to_owned()),
                provider_trace_id: Some("submit-version-42".to_owned()),
                received_at: Utc::now(),
            }))
        }

        async fn complete_exit_ticket(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
            group_id: &str,
        ) -> ProviderResult<UaiPresetCompletionResult> {
            self.calls.lock().unwrap().push((
                course_resource_id.to_owned(),
                unit_id.to_owned(),
                group_id.to_owned(),
                "exit_ticket",
            ));
            if self.fail {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "synthetic ambiguous mutation failure",
                ));
            }
            if self.already_completed {
                return Ok(UaiPresetCompletionResult::AlreadyCompleted);
            }
            Ok(UaiPresetCompletionResult::Submitted(SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some("accepted for verification".to_owned()),
                provider_trace_id: Some("submit-version-42".to_owned()),
                received_at: Utc::now(),
            }))
        }

        async fn complete_oral_empty(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
            group_id: &str,
            task_type: &str,
            question_count: u32,
        ) -> ProviderResult<UaiPresetCompletionResult> {
            assert_eq!(task_type, "oral-sentence");
            assert_eq!(question_count, 1);
            self.calls.lock().unwrap().push((
                course_resource_id.to_owned(),
                unit_id.to_owned(),
                group_id.to_owned(),
                "oral_empty",
            ));
            if self.fail {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "synthetic ambiguous mutation failure",
                ));
            }
            Ok(UaiPresetCompletionResult::Submitted(SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some("accepted for verification".to_owned()),
                provider_trace_id: Some("submit-version-42".to_owned()),
                received_at: Utc::now(),
            }))
        }
    }

    #[derive(Debug)]
    struct FixtureEvents;

    #[async_trait]
    impl ExecutionEventSink for FixtureEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            Ok(())
        }

        async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn audited_preset_executes_once_and_requires_core_verification() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = UaiResourceExecution::try_new(
            Arc::new(FixtureDetail {
                task_types: vec!["rich-text-read".to_owned()],
            }),
            fixture_progress(RemoteState::Completed),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Unknown);
        assert!(!outcome.verified);
        assert_eq!(
            outcome.result_sanitized["verification"],
            "required_fresh_progress"
        );
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[(
                "2001".to_owned(),
                "unit-1".to_owned(),
                "group-1".to_owned(),
                "preset",
            )]
        );
    }

    #[tokio::test]
    async fn donor_exit_ticket_executes_the_dedicated_empty_completion_path() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = UaiResourceExecution::try_new(
            Arc::new(FixtureDetail {
                task_types: vec!["exit-ticket".to_owned()],
            }),
            fixture_progress(RemoteState::Completed),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.result_sanitized["resource_kind"], "exit_ticket");
        assert_eq!(outcome.result_sanitized["mutation_submitted"], true);
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[(
                "2001".to_owned(),
                "unit-1".to_owned(),
                "group-1".to_owned(),
                "exit_ticket",
            )]
        );
    }

    #[tokio::test]
    async fn donor_oral_empty_submission_uses_its_dedicated_placeholder_path() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = UaiResourceExecution::try_new(
            Arc::new(FixtureDetail {
                task_types: vec!["oral-sentence".to_owned()],
            }),
            fixture_progress(RemoteState::Completed),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.result_sanitized["resource_kind"], "oral_empty");
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[(
                "2001".to_owned(),
                "unit-1".to_owned(),
                "group-1".to_owned(),
                "oral_empty",
            )]
        );
    }

    #[tokio::test]
    async fn ambiguous_failure_is_returned_after_one_preset_mutation() {
        let transport = Arc::new(FixtureTransport {
            fail: true,
            ..FixtureTransport::default()
        });
        let execution = UaiResourceExecution::try_new(
            Arc::new(FixtureDetail {
                task_types: vec!["text-learn".to_owned(), "vocabulary".to_owned()],
            }),
            fixture_progress(RemoteState::Completed),
            transport.clone(),
        )
        .unwrap();
        let error = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .expect_err("ambiguous mutation must remain visible to Core");

        assert_eq!(error.kind, ProviderErrorKind::Network);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fresh_completed_preset_returns_for_core_readback_without_mutation_claim() {
        let transport = Arc::new(FixtureTransport {
            already_completed: true,
            ..FixtureTransport::default()
        });
        let execution = UaiResourceExecution::try_new(
            Arc::new(FixtureDetail {
                task_types: vec!["video-point-read".to_owned()],
            }),
            fixture_progress(RemoteState::Completed),
            transport,
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.result_sanitized["already_completed"], true);
        assert_eq!(outcome.result_sanitized["mutation_submitted"], false);
        assert!(!outcome.verified);
    }

    #[tokio::test]
    async fn unsupported_group_fails_before_preset_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = UaiResourceExecution::try_new(
            Arc::new(FixtureDetail {
                task_types: vec!["discussion".to_owned()],
            }),
            fixture_progress(RemoteState::Completed),
            transport.clone(),
        )
        .unwrap();
        let error = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .expect_err("discussion must not inherit preset completion");

        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn verify_only_path_rebinds_and_reads_exact_group_progress() {
        for (remote_state, expected_verified) in [
            (RemoteState::Unknown, false),
            (RemoteState::Completed, true),
        ] {
            let progress = fixture_progress(remote_state);
            let transport = Arc::new(FixtureTransport::default());
            let execution = UaiResourceExecution::try_new(
                Arc::new(FixtureDetail {
                    task_types: vec!["rich-text-read".to_owned()],
                }),
                progress.clone(),
                transport.clone(),
            )
            .unwrap();

            assert!(
                execution.requires_execution_verification(&[TaskCapability::ResourceExecution])
            );
            let outcome = execution
                .verify_execution(&context(), &request())
                .await
                .unwrap();
            assert_eq!(outcome.remote_state, remote_state);
            assert_eq!(outcome.verified, expected_verified);
            assert_eq!(
                progress.calls.lock().unwrap().as_slice(),
                &["group:2001:unit-1:group-1".to_owned()]
            );
            assert!(transport.calls.lock().unwrap().is_empty());
        }
    }

    fn fixture_progress(remote_state: RemoteState) -> Arc<FixtureProgress> {
        Arc::new(FixtureProgress {
            remote_state,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-preset-execution".to_owned(),
        }
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            task_id: TaskId::new(),
            remote_task_id: "group:2001:unit-1:group-1".to_owned(),
            course_id: Some(CourseId::new()),
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            runtime_settings: runtime_settings_schema().resolve(None, None, None).unwrap(),
        }
    }
}
