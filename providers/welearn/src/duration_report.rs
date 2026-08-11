use std::{fmt, sync::Arc};

use asterism_domain::{LogLevel, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::{
    WellearnCmiDocument, WellearnCmiSnapshot,
    cmi::{parse_cmi_snapshot, parse_sco_identity},
    metadata::development_metadata,
    runtime_settings::WellearnRuntimeSettings,
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
    transport: Arc<dyn WellearnDurationReportTransport>,
}

impl WellearnDurationReport {
    /// Creates the duration capability around one lifecycle transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn WellearnDurationReportTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for WellearnDurationReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnDurationReport")
            .field("metadata", &self.metadata)
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
        let documents = self
            .transport
            .report_duration(
                context,
                &course_id,
                &sco_id,
                settings.duration_report_seconds,
                settings.duration_heartbeat_interval_seconds,
                events,
            )
            .await?;
        let before = parse_cmi_snapshot(documents.before.as_str())?;
        let after = parse_cmi_snapshot(documents.after.as_str())?;
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
                    "duration_report_seconds": settings.duration_report_seconds,
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
                "duration_report_seconds": settings.duration_report_seconds,
                "completion_preserved": true,
                "progress_preserved": true,
                "score_preserved": true,
                "duration_observation_changed": true,
            }),
        })
    }
}

fn verify_preserved_state(
    before: &WellearnCmiSnapshot,
    after: &WellearnCmiSnapshot,
) -> ProviderResult<()> {
    if before.completion_raw().unwrap_or("not_attempted")
        != after.completion_raw().unwrap_or("not_attempted")
        || before.progress_raw().unwrap_or("0") != after.progress_raw().unwrap_or("0")
        || before.score_scaled_raw().unwrap_or("") != after.score_scaled_raw().unwrap_or("")
        || before.success_status_raw().unwrap_or("unknown")
            != after.success_status_raw().unwrap_or("unknown")
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn completion, progress or score changed during duration reporting",
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

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, TaskId};
    use asterism_provider_api::{ProviderRuntimeSettingsPatch, ProviderSettingValue};

    use super::*;
    use crate::runtime_settings::{
        DURATION_HEARTBEAT_INTERVAL_KEY, DURATION_REPORT_SECONDS_KEY, runtime_settings_schema,
    };

    const BEFORE: &str =
        include_str!("../../../fixtures/providers/welearn/cmi/duration-before.json");
    const AFTER: &str = include_str!("../../../fixtures/providers/welearn/cmi/duration-after.json");

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: AtomicUsize,
        settings: Mutex<Option<(u64, u64)>>,
        drift_completion: bool,
        unchanged_duration: bool,
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
        let capability = WellearnDurationReport::try_new(transport.clone()).unwrap();
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
            let capability = WellearnDurationReport::try_new(Arc::new(transport)).unwrap();
            let error = capability
                .execute(&context(), &request(), &FixtureEvents::default())
                .await
                .unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        }
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
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::DurationReport],
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }
}
