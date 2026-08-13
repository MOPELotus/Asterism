use std::{fmt, sync::Arc};

use asterism_domain::{LogLevel, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, TaskDetailCapability, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::{
    WellearnCmiDocument, WellearnCmiSnapshot,
    cmi::{parse_cmi_snapshot, parse_sco_identity},
    execution_selection::uniform_u64,
    metadata::development_metadata,
    runtime_settings::{WellearnDurationTarget, WellearnRuntimeSettings},
    task_detail::validate_fresh_execution_detail,
};

/// Complete before/after evidence returned by one bounded `WELearn` duration
/// lifecycle. Response bodies remain redacted and zeroized by their wrappers.
#[derive(Debug)]
pub struct WellearnDurationReportDocuments {
    before: WellearnCmiDocument,
    after: WellearnCmiDocument,
    started: bool,
    heartbeat_count: u32,
}

impl WellearnDurationReportDocuments {
    /// Binds the fresh baseline and post-finalize CMI documents to one report.
    pub const fn new(
        before: WellearnCmiDocument,
        after: WellearnCmiDocument,
        started: bool,
        heartbeat_count: u32,
    ) -> Self {
        Self {
            before,
            after,
            started,
            heartbeat_count,
        }
    }
}

/// High-level transport boundary for an atomic read → optional start → keep →
/// finalize → fresh-read lifecycle. Implementations must not replay mutations
/// after an authentication failure.
#[async_trait]
pub trait WellearnDurationReportTransport: Send + Sync {
    async fn report_duration(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        duration_seconds: u64,
        heartbeat_interval_seconds: u64,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<WellearnDurationReportDocuments>;
}

/// `WELearn` duration reporting kept separate from completion/progress writes.
pub struct WellearnDurationReport {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn WellearnDurationReportTransport>,
}

impl WellearnDurationReport {
    /// Creates the duration capability around one lifecycle transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn WellearnDurationReportTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
        })
    }
}

impl fmt::Debug for WellearnDurationReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnDurationReport")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnDurationReport {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskExecutionCapability for WellearnDurationReport {
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        if request.requested_capabilities != [TaskCapability::DurationReport] {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn duration execution accepts only DurationReport",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let duration_seconds = select_duration(settings.duration_report, request);
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
                TaskCapability::DurationRead,
                TaskCapability::DurationReport,
            ],
        )?;
        let documents = self
            .transport
            .report_duration(
                context,
                &course_id,
                &sco_id,
                duration_seconds,
                settings.duration_heartbeat_interval_seconds,
                events,
            )
            .await?;
        let before = parse_cmi_snapshot(documents.before.as_str())?;
        let after = parse_cmi_snapshot(documents.after.as_str())?;
        require_duration_snapshot(&before, "baseline")?;
        require_duration_snapshot(&after, "verification")?;
        verify_preserved_state(&before, &after)?;
        if !duration_observation_changed(&before, &after) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "WELearn duration did not change after the reported session",
            ));
        }
        events
            .report(ProviderProgress {
                percent: Some(100),
                stage: "duration_verified".to_owned(),
                status_text: Some("远端学习时长已重新读取并复核".to_owned()),
                completed_items: Some(1),
                total_items: Some(1),
            })
            .await?;
        events
            .log(ProviderExecutionLog {
                level: LogLevel::Info,
                stage: "duration_verified".to_owned(),
                message: "学习会话已完成，完成度与进度保持不变".to_owned(),
                provider_trace_id: None,
                metadata_sanitized: Some(serde_json::json!({
                    "started": documents.started,
                    "heartbeat_count": documents.heartbeat_count,
                    "duration_report_seconds": duration_seconds,
                    "duration_report_mode": duration_mode(settings.duration_report),
                    "duration_observation_changed": true,
                })),
            })
            .await?;
        Ok(ExecutionOutcome {
            remote_state: after.remote_state(),
            verified: true,
            result_sanitized: serde_json::json!({
                "schema": "welearn.duration-report.v1",
                "started": documents.started,
                "heartbeat_count": documents.heartbeat_count,
                "duration_report_seconds": duration_seconds,
                "duration_report_mode": duration_mode(settings.duration_report),
                "completion_preserved": true,
                "progress_preserved": true,
                "score_preserved": true,
                "duration_observation_changed": true,
            }),
        })
    }
}

fn select_duration(configured: WellearnDurationTarget, request: &ExecutionRequest) -> u64 {
    match configured {
        WellearnDurationTarget::Fixed(seconds) => seconds,
        WellearnDurationTarget::RandomRange { minimum, maximum } => uniform_u64(
            b"asterism.welearn.duration-target.uniform.v2",
            request,
            minimum,
            maximum,
        ),
    }
}

const fn duration_mode(configured: WellearnDurationTarget) -> &'static str {
    match configured {
        WellearnDurationTarget::Fixed(_) => "fixed",
        WellearnDurationTarget::RandomRange { .. } => "random_range",
    }
}

fn verify_preserved_state(
    before: &WellearnCmiSnapshot,
    after: &WellearnCmiSnapshot,
) -> ProviderResult<()> {
    if before.completion_raw() != after.completion_raw()
        || before.progress_raw() != after.progress_raw()
        || before.score_scaled_raw() != after.score_scaled_raw()
        || before.success_status_raw() != after.success_status_raw()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn completion, progress or score changed during duration reporting",
        ));
    }
    Ok(())
}

fn require_duration_snapshot(
    snapshot: &WellearnCmiSnapshot,
    stage: &'static str,
) -> ProviderResult<()> {
    if !snapshot.cmi_present()
        || snapshot.completion_raw().is_none()
        || snapshot.progress_raw().is_none()
        || snapshot.score_scaled_raw().is_none()
        || snapshot.success_status_raw().is_none()
        || snapshot.session_time_raw().is_none()
        || snapshot.total_time_raw().is_none()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("WELearn duration {stage} CMI is incomplete"),
        ));
    }
    Ok(())
}

fn duration_observation_changed(before: &WellearnCmiSnapshot, after: &WellearnCmiSnapshot) -> bool {
    before.session_time_raw() != after.session_time_raw()
        || before.total_time_raw() != after.total_time_raw()
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn duration received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn duration requires an authenticated session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType, TaskId,
    };
    use asterism_provider_api::{
        ProviderRuntimeSettingsPatch, ProviderSettingValue, RemoteTask, RemoteTaskDetail,
    };

    use super::*;
    use crate::runtime_settings::{
        DURATION_HEARTBEAT_INTERVAL_KEY, DURATION_REPORT_MAX_SECONDS_KEY,
        DURATION_REPORT_MIN_SECONDS_KEY, DURATION_REPORT_MODE_KEY, DURATION_REPORT_SECONDS_KEY,
        runtime_settings_schema,
    };

    const BEFORE: &str =
        include_str!("../../../fixtures/providers/welearn/cmi/duration-before.json");
    const AFTER: &str = include_str!("../../../fixtures/providers/welearn/cmi/duration-after.json");

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        calls: AtomicUsize,
        disappeared: bool,
    }

    impl FixtureDetail {
        fn present() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                calls: AtomicUsize::new(0),
                disappeared: false,
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
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.disappeared {
                return Err(ProviderError::new(
                    ProviderErrorKind::RemoteChanged,
                    "fixture SCO disappeared",
                ));
            }
            let normalized = serde_json::json!({
                "schema": "welearn.sco.v1",
                "course_id": "1001",
                "unit_index": 0,
                "unit_title": "Unit",
                "unit_code": null,
                "sco_id": "301",
                "visible": true,
                "completion": "in_progress",
                "duration_raw": "45",
            });
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:1001".to_owned()),
                    title: "Practice".to_owned(),
                    source_type: SourceType::Resource,
                    assessment_class: AssessmentClass::Unknown,
                    remote_state: RemoteState::InProgress,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: vec![
                        TaskCapability::ProgressRead,
                        TaskCapability::ResourceExecution,
                        TaskCapability::ExecutionVerify,
                        TaskCapability::DurationRead,
                        TaskCapability::DurationReport,
                    ],
                    fingerprint: "v1:fixture".to_owned(),
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

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: AtomicUsize,
        settings: Mutex<Option<(u64, u64)>>,
        drift_completion: bool,
        unchanged_duration: bool,
        incomplete_verification: bool,
    }

    #[async_trait]
    impl WellearnDurationReportTransport for FixtureTransport {
        async fn report_duration(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            duration_seconds: u64,
            heartbeat_interval_seconds: u64,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<WellearnDurationReportDocuments> {
            assert_eq!(course_id, "1001");
            assert_eq!(sco_id, "301");
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.settings.lock().unwrap() = Some((duration_seconds, heartbeat_interval_seconds));
            let after = if self.drift_completion {
                AFTER.replace("incomplete", "completed")
            } else if self.unchanged_duration {
                BEFORE.to_owned()
            } else if self.incomplete_verification {
                r#"{"ret":0,"comment":"{}"}"#.to_owned()
            } else {
                AFTER.to_owned()
            };
            Ok(WellearnDurationReportDocuments::new(
                WellearnCmiDocument::try_new(BEFORE).unwrap(),
                WellearnCmiDocument::try_new(after).unwrap(),
                false,
                3,
            ))
        }
    }

    #[derive(Debug, Default)]
    struct FixtureEvents {
        progress: AtomicUsize,
        logs: AtomicUsize,
    }

    #[async_trait]
    impl ExecutionEventSink for FixtureEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            self.progress.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
            self.logs.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[tokio::test]
    async fn duration_report_uses_frozen_settings_and_verifies_fresh_cmi() {
        let transport = Arc::new(FixtureTransport::default());
        let details = Arc::new(FixtureDetail::present());
        let capability =
            WellearnDurationReport::try_new(details.clone(), transport.clone()).unwrap();
        let events = FixtureEvents::default();
        let outcome = capability
            .execute(&context(), &request(), &events)
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(
            outcome.remote_state,
            asterism_domain::RemoteState::InProgress
        );
        assert_eq!(outcome.result_sanitized["completion_preserved"], true);
        assert_eq!(*transport.settings.lock().unwrap(), Some((120, 30)));
        assert_eq!(events.progress.load(Ordering::Relaxed), 1);
        assert_eq!(events.logs.load(Ordering::Relaxed), 1);
        assert_eq!(details.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn duration_report_rejects_state_drift_or_missing_time_change() {
        for transport in [
            FixtureTransport {
                drift_completion: true,
                ..FixtureTransport::default()
            },
            FixtureTransport {
                unchanged_duration: true,
                ..FixtureTransport::default()
            },
        ] {
            let capability = WellearnDurationReport::try_new(
                Arc::new(FixtureDetail::present()),
                Arc::new(transport),
            )
            .unwrap();
            let error = capability
                .execute(&context(), &request(), &FixtureEvents::default())
                .await
                .unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        }
    }

    #[tokio::test]
    async fn duration_report_rejects_missing_verification_cmi() {
        let capability = WellearnDurationReport::try_new(
            Arc::new(FixtureDetail::present()),
            Arc::new(FixtureTransport {
                incomplete_verification: true,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();
        let error = capability
            .execute(&context(), &request(), &FixtureEvents::default())
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    #[tokio::test]
    async fn random_duration_is_bounded_and_stable_for_the_frozen_execution() {
        let transport = Arc::new(FixtureTransport::default());
        let capability =
            WellearnDurationReport::try_new(Arc::new(FixtureDetail::present()), transport.clone())
                .unwrap();
        let request = random_request();

        let first = capability
            .execute(&context(), &request, &FixtureEvents::default())
            .await
            .unwrap();
        let second = capability
            .execute(&context(), &request, &FixtureEvents::default())
            .await
            .unwrap();

        let selected = first.result_sanitized["duration_report_seconds"]
            .as_u64()
            .unwrap();
        assert!((180..=240).contains(&selected));
        assert_eq!(
            first.result_sanitized["duration_report_seconds"],
            second.result_sanitized["duration_report_seconds"]
        );
        assert_eq!(
            first.result_sanitized["duration_report_mode"],
            "random_range"
        );
    }

    #[test]
    fn later_executions_can_resample_the_bounded_duration() {
        let mut request = random_request();
        let configured = WellearnRuntimeSettings::resolve(&request.runtime_settings)
            .unwrap()
            .duration_report;
        let mut selected = std::collections::BTreeSet::new();
        for index in 1_u64..=128 {
            request.execution_id = format!("00000000-0000-0000-0000-{index:012x}")
                .parse()
                .unwrap();
            let first = select_duration(configured, &request);
            let recovered = select_duration(configured, &request);
            assert_eq!(first, recovered);
            assert!((180..=240).contains(&first));
            selected.insert(first);
        }
        assert!(
            selected.len() > 1,
            "new Executions must be able to resample"
        );
    }

    #[tokio::test]
    async fn duration_report_requires_fresh_sco_rediscovery_before_transport() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = WellearnDurationReport::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
                calls: AtomicUsize::new(0),
                disappeared: true,
            }),
            transport.clone(),
        )
        .unwrap();

        let error = capability
            .execute(&context(), &request(), &FixtureEvents::default())
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-duration-test".to_owned(),
        }
    }

    fn request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    DURATION_REPORT_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(120),
                ),
                (
                    DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(30),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::DurationReport],
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }

    fn random_request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    DURATION_REPORT_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice("random_range".to_owned()),
                ),
                (
                    DURATION_REPORT_MIN_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(180),
                ),
                (
                    DURATION_REPORT_MAX_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(240),
                ),
                (
                    DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(30),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::DurationReport],
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }
}
