use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, TaskDetailCapability, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::{
    WellearnCmiDocument,
    cmi::{parse_cmi_snapshot, parse_sco_identity},
    execution_selection::{clamped_gaussian_u8, uniform_u8},
    metadata::development_metadata,
    runtime_settings::{WellearnResourceScore, WellearnRuntimeSettings},
    task_detail::validate_fresh_execution_detail,
};

/// Bounded documents returned by one SCO preset-completion attempt.
///
/// `after` is absent only when the fresh baseline already proved completion
/// and the transport issued no mutation.
#[derive(Debug)]
pub struct WellearnResourceExecutionDocuments {
    before: WellearnCmiDocument,
    after: Option<WellearnCmiDocument>,
    mutation_submitted: bool,
    started: bool,
}

impl WellearnResourceExecutionDocuments {
    pub const fn already_completed(before: WellearnCmiDocument) -> Self {
        Self {
            before,
            after: None,
            mutation_submitted: false,
            started: false,
        }
    }

    pub const fn submitted(
        before: WellearnCmiDocument,
        after: WellearnCmiDocument,
        started: bool,
    ) -> Self {
        Self {
            before,
            after: Some(after),
            mutation_submitted: true,
            started,
        }
    }
}

/// Native boundary for the donor-audited SCO completion preset.
///
/// Implementations may renew only before the first mutation and must never
/// replay `start`, `setscoinfo` or `save` after an ambiguous result.
#[async_trait]
pub trait WellearnResourceExecutionTransport: Send + Sync {
    async fn complete_resource(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        score_percent: u8,
    ) -> ProviderResult<WellearnResourceExecutionDocuments>;

    /// Reads one fresh CMI document without starting, setting or saving the
    /// SCO. This is the only transport path used by crash recovery.
    async fn verify_resource(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<WellearnCmiDocument>;
}

/// Executes the audited `WELearn` SCO completion preset after a fresh detail
/// rebind and verifies its exact completion/progress/score tuple with a fresh
/// CMI read. Core persists the frozen execution request and re-evaluates this
/// Provider-specific predicate without replaying an ambiguous mutation.
pub struct WellearnResourceExecution {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn WellearnResourceExecutionTransport>,
}

impl WellearnResourceExecution {
    /// Builds the non-idempotent SCO execution boundary.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn WellearnResourceExecutionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
        })
    }
}

impl fmt::Debug for WellearnResourceExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnResourceExecution")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnResourceExecution {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskExecutionCapability for WellearnResourceExecution {
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(unsupported(
                "WELearn resource execution accepts only ResourceExecution",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let score_percent = select_score(settings.resource_score, request);
        let detail = self
            .details
            .task_detail(context, &request.remote_task_id)
            .await?;
        validate_fresh_execution_detail(
            &detail,
            &request.remote_task_id,
            &course_id,
            &sco_id,
            &[
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        )?;

        events
            .log(ProviderExecutionLog {
                level: asterism_domain::LogLevel::Info,
                stage: "resource_prepare".to_owned(),
                message: "SCO 已通过新鲜详情校验，准备按冻结设置执行完成度预设".to_owned(),
                provider_trace_id: None,
                metadata_sanitized: Some(serde_json::json!({
                    "preset": "completed_progress_score",
                    "score_percent": score_percent,
                    "score_mode": score_mode(settings.resource_score),
                    "verification": "provider_fresh_cmi",
                })),
            })
            .await?;

        let documents = self
            .transport
            .complete_resource(context, &course_id, &sco_id, score_percent)
            .await?;
        let before = parse_cmi_snapshot(documents.before.as_str())?;
        let already_completed = !documents.mutation_submitted;
        if already_completed {
            verify_completed_preset(&before, score_percent)?;
        }
        let immediate_verified = if let Some(after) = documents.after.as_ref() {
            let after = parse_cmi_snapshot(after.as_str())?;
            verify_completed_preset(&after, score_percent)?;
            true
        } else {
            true
        };

        events
            .report(ProviderProgress {
                percent: Some(50),
                stage: if already_completed {
                    "resource_already_completed"
                } else {
                    "resource_submitted"
                }
                .to_owned(),
                status_text: Some(
                    if already_completed {
                        "提交前的新鲜 CMI 已显示完成，Provider 已完成严格回读"
                    } else {
                        "完成度、进度与分数预设已受理，并通过新鲜 CMI 严格回读"
                    }
                    .to_owned(),
                ),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;

        Ok(ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({
                "schema": "welearn.resource-completion.v1",
                "preset": "completed_progress_score",
                "score_percent": score_percent,
                "score_mode": score_mode(settings.resource_score),
                "mutation_submitted": documents.mutation_submitted,
                "started": documents.started,
                "already_completed": already_completed,
                "immediate_cmi_verified": immediate_verified,
                "verification": "provider_fresh_cmi",
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
                "WELearn resource verification accepts only ResourceExecution",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let score_percent = select_score(settings.resource_score, request);
        let detail = self
            .details
            .task_detail(context, &request.remote_task_id)
            .await?;
        validate_fresh_execution_detail(
            &detail,
            &request.remote_task_id,
            &course_id,
            &sco_id,
            &[
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ],
        )?;
        let document = self
            .transport
            .verify_resource(context, &course_id, &sco_id)
            .await?;
        let snapshot = parse_cmi_snapshot(document.as_str())?;
        verify_completed_preset(&snapshot, score_percent)?;
        Ok(ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({
                "schema": "welearn.resource-completion-verification.v1",
                "preset": "completed_progress_score",
                "score_percent": score_percent,
                "score_mode": score_mode(settings.resource_score),
                "goal_matched": true,
                "verification": "fresh_cmi_no_mutation",
            }),
        })
    }
}

fn select_score(configured: WellearnResourceScore, request: &ExecutionRequest) -> u8 {
    match configured {
        WellearnResourceScore::Fixed(score) => score,
        WellearnResourceScore::UniformRandomRange { minimum, maximum } => uniform_u8(
            b"asterism.welearn.resource-score.uniform.v2",
            request,
            minimum,
            maximum,
        ),
        WellearnResourceScore::GaussianRandomRange { minimum, maximum } => clamped_gaussian_u8(
            b"asterism.welearn.resource-score.gaussian.v2",
            request,
            minimum,
            maximum,
        ),
    }
}

const fn score_mode(configured: WellearnResourceScore) -> &'static str {
    match configured {
        WellearnResourceScore::Fixed(_) => "fixed",
        WellearnResourceScore::UniformRandomRange { .. } => "random_range",
        WellearnResourceScore::GaussianRandomRange { .. } => "gaussian_random_range",
    }
}

fn verify_completed_preset(
    snapshot: &crate::WellearnCmiSnapshot,
    score_percent: u8,
) -> ProviderResult<()> {
    let expected_score = score_percent.to_string();
    if !snapshot.cmi_present()
        || snapshot.remote_state() != RemoteState::Completed
        || snapshot.percent() != Some(100)
        || snapshot.score_scaled_raw() != Some(expected_score.as_str())
    {
        return Err(remote_changed(
            "WELearn completion preset was not visible in fresh CMI",
        ));
    }
    Ok(())
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn resource execution received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn resource execution requires an authenticated session",
        ));
    }
    Ok(())
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::runtime_settings::{
        RESOURCE_SCORE_MAX_PERCENT_KEY, RESOURCE_SCORE_MIN_PERCENT_KEY, RESOURCE_SCORE_MODE_KEY,
        RESOURCE_SCORE_PERCENT_KEY, runtime_settings_schema,
    };
    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, SecretId, SourceType, TaskId,
    };
    use asterism_provider_api::{
        ProviderRuntimeSettingsPatch, ProviderSettingValue, RemoteTask, RemoteTaskDetail,
    };

    const BEFORE: &str = r#"{"ret":0,"comment":"{\"cmi\":{\"completion_status\":\"incomplete\",\"progress_measure\":\"0.25\",\"session_time\":\"15\",\"total_time\":\"45\",\"score\":{\"scaled\":\"20\"},\"success_status\":\"unknown\"}}"}"#;
    const AFTER: &str =
        include_str!("../../../fixtures/providers/welearn/cmi/resource-completed.json");

    #[derive(Debug)]
    struct FixtureDetail {
        visible: bool,
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let normalized = serde_json::json!({
                "schema": "welearn.sco.v1",
                "course_id": "1001",
                "unit_index": 0,
                "unit_title": "Unit",
                "unit_code": null,
                "sco_id": "301",
                "visible": self.visible,
                "completion": "pending",
                "duration_raw": null,
            });
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:1001".to_owned()),
                    title: "Practice".to_owned(),
                    source_type: SourceType::Resource,
                    assessment_class: AssessmentClass::Unknown,
                    remote_state: if self.visible {
                        RemoteState::Pending
                    } else {
                        RemoteState::NotOpen
                    },
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: vec![
                        TaskCapability::ProgressRead,
                        TaskCapability::ResourceExecution,
                        TaskCapability::ExecutionVerify,
                        TaskCapability::DurationReport,
                    ],
                    fingerprint: "v1:synthetic".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: serde_json::json!({"schema": "welearn.sco.raw.v1"}),
                },
                normalized_detail: serde_json::json!({
                    "schema": "welearn.sco-task-detail.v1",
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

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<(String, String, u8)>>,
        verifications: Mutex<Vec<(String, String)>>,
        already_completed: bool,
        drift_score: bool,
    }

    #[async_trait]
    impl WellearnResourceExecutionTransport for FixtureTransport {
        async fn complete_resource(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            score_percent: u8,
        ) -> ProviderResult<WellearnResourceExecutionDocuments> {
            self.calls.lock().unwrap().push((
                course_id.to_owned(),
                sco_id.to_owned(),
                score_percent,
            ));
            if self.already_completed {
                return Ok(WellearnResourceExecutionDocuments::already_completed(
                    WellearnCmiDocument::try_new(AFTER).unwrap(),
                ));
            }
            let reflected_score = if self.drift_score {
                if score_percent == 100 {
                    99
                } else {
                    score_percent + 1
                }
            } else {
                score_percent
            };
            let after = AFTER.replace(
                "\\\"scaled\\\":\\\"82\\\"",
                &format!("\\\"scaled\\\":\\\"{reflected_score}\\\""),
            );
            Ok(WellearnResourceExecutionDocuments::submitted(
                WellearnCmiDocument::try_new(BEFORE).unwrap(),
                WellearnCmiDocument::try_new(after).unwrap(),
                true,
            ))
        }

        async fn verify_resource(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
        ) -> ProviderResult<WellearnCmiDocument> {
            self.verifications
                .lock()
                .unwrap()
                .push((course_id.to_owned(), sco_id.to_owned()));
            let score = if self.drift_score { "81" } else { "82" };
            WellearnCmiDocument::try_new(AFTER.replace(
                "\\\"scaled\\\":\\\"82\\\"",
                &format!("\\\"scaled\\\":\\\"{score}\\\""),
            ))
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
    async fn resource_execution_binds_fresh_detail_and_verifies_exact_cmi_goal() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["score_percent"], 82);
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[("1001".to_owned(), "301".to_owned(), 82)]
        );
    }

    #[tokio::test]
    async fn completed_preflight_skips_mutation_and_returns_verified_state() {
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            Arc::new(FixtureTransport {
                already_completed: true,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.result_sanitized["already_completed"], true);
        assert_eq!(outcome.result_sanitized["mutation_submitted"], false);
        assert!(outcome.verified);
    }

    #[tokio::test]
    async fn random_score_mode_is_bounded_and_stable_for_one_frozen_attempt() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let request = random_request();

        let first = execution
            .execute(&context(), &request, &FixtureEvents)
            .await
            .unwrap();
        let second = execution
            .execute(&context(), &request, &FixtureEvents)
            .await
            .unwrap();

        let score = first.result_sanitized["score_percent"].as_u64().unwrap();
        assert!((73..=79).contains(&score));
        assert_eq!(
            first.result_sanitized["score_percent"],
            second.result_sanitized["score_percent"]
        );
        assert_eq!(first.result_sanitized["score_mode"], "random_range");
        assert_eq!(
            transport.calls.lock().unwrap()[0].2,
            u8::try_from(score).unwrap()
        );
    }

    #[test]
    fn donor_random_modes_are_execution_bound_and_recovery_stable() {
        for (mode, expected) in [
            ("random_range", "random_range"),
            ("gaussian_random_range", "gaussian_random_range"),
        ] {
            let mut request = random_request_with_mode(mode, 60, 90);
            let configured = WellearnRuntimeSettings::resolve(&request.runtime_settings)
                .unwrap()
                .resource_score;
            let mut selected = BTreeSet::new();
            for index in 1_u64..=128 {
                request.execution_id = format!("00000000-0000-0000-0000-{index:012x}")
                    .parse()
                    .unwrap();
                let first = select_score(configured, &request);
                let recovered = select_score(configured, &request);
                assert_eq!(first, recovered);
                assert!((60..=90).contains(&first));
                selected.insert(first);
                assert_eq!(score_mode(configured), expected);
            }
            assert!(
                selected.len() > 1,
                "new Executions must be able to resample"
            );
        }
    }

    #[tokio::test]
    async fn goal_bound_verification_fresh_reads_without_replaying_mutation() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();

        let outcome = execution
            .verify_execution(&context(), &request())
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert_eq!(outcome.result_sanitized["score_percent"], 82);
        assert!(transport.calls.lock().unwrap().is_empty());
        assert_eq!(
            transport.verifications.lock().unwrap().as_slice(),
            &[("1001".to_owned(), "301".to_owned())]
        );
    }

    #[tokio::test]
    async fn score_drift_fails_closed_and_hidden_fresh_task_remains_executable() {
        let drift = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            Arc::new(FixtureTransport {
                drift_score: true,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();
        let error = drift
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);

        let hidden_transport = Arc::new(FixtureTransport::default());
        let hidden = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: false }),
            hidden_transport.clone(),
        )
        .unwrap();
        let outcome = hidden
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();
        assert!(outcome.verified);
        assert_eq!(
            hidden_transport.calls.lock().unwrap().as_slice(),
            &[("1001".to_owned(), "301".to_owned(), 82)]
        );
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-resource-execution".to_owned(),
        }
    }

    fn request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([(
                RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                ProviderSettingValue::Integer(82),
            )]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }

    fn random_request() -> ExecutionRequest {
        random_request_with_mode("random_range", 73, 79)
    }

    fn random_request_with_mode(mode: &str, minimum: i64, maximum: i64) -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice(mode.to_owned()),
                ),
                (
                    RESOURCE_SCORE_MIN_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(minimum),
                ),
                (
                    RESOURCE_SCORE_MAX_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(maximum),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }
}
