use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, TaskDetailCapability, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::{
    WellearnCmiDocument, WellearnResourceCompletionCmiFormat, WellearnResourceCompletionSequence,
    WellearnResourceCompletionTimeMode,
    cmi::{parse_cmi_snapshot, parse_mutation_cmi_baseline, parse_sco_identity},
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
    start_accepted: Option<bool>,
    set_accepted: Option<bool>,
    save_acceptances: Vec<bool>,
}

impl WellearnResourceExecutionDocuments {
    pub const fn already_completed(before: WellearnCmiDocument) -> Self {
        Self {
            before,
            after: None,
            mutation_submitted: false,
            started: false,
            start_accepted: None,
            set_accepted: None,
            save_acceptances: Vec::new(),
        }
    }

    pub const fn submitted(
        before: WellearnCmiDocument,
        after: WellearnCmiDocument,
        started: bool,
        start_accepted: bool,
        set_accepted: Option<bool>,
        save_acceptances: Vec<bool>,
    ) -> Self {
        Self {
            before,
            after: Some(after),
            mutation_submitted: true,
            started,
            start_accepted: Some(start_accepted),
            set_accepted,
            save_acceptances,
        }
    }
}

/// Immutable donor completion wire plan selected from one frozen execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnResourceExecutionPlan {
    pub score_percent: u8,
    pub sequence: WellearnResourceCompletionSequence,
    pub time_mode: WellearnResourceCompletionTimeMode,
    pub cmi_format: WellearnResourceCompletionCmiFormat,
    pub write_mode: crate::WellearnResourceCompletionWriteMode,
    pub mutation_profile: crate::WellearnResourceMutationProfile,
}

impl WellearnResourceExecutionPlan {
    /// Rejects combinations that do not match one complete audited donor wire
    /// profile before any non-idempotent transport call.
    ///
    /// # Errors
    ///
    /// Returns an internal error when independently resolved settings mix
    /// fields from different donor profiles.
    pub fn validate(self) -> ProviderResult<()> {
        use crate::{
            WellearnResourceCompletionCmiFormat::{InteractionInfoSuffix, Json},
            WellearnResourceCompletionSequence::{CurrentDonorDualSave100, SelectedScore},
            WellearnResourceCompletionTimeMode::{PreserveFreshTime, ZeroTime},
            WellearnResourceCompletionWriteMode::{SaveOnly, SetThenSave},
            WellearnResourceMutationProfile::{CurrentFullSimpleReferer, LegacyMinimalTaskReferer},
        };

        let profile = (
            self.score_percent,
            self.sequence,
            self.time_mode,
            self.cmi_format,
            self.write_mode,
            self.mutation_profile,
        );
        let audited = matches!(
            profile,
            (
                0..=100,
                CurrentDonorDualSave100,
                ZeroTime,
                Json,
                SetThenSave,
                CurrentFullSimpleReferer,
            ) | (
                100,
                SelectedScore,
                PreserveFreshTime,
                Json,
                SetThenSave,
                CurrentFullSimpleReferer,
            ) | (
                0..=100,
                SelectedScore,
                ZeroTime,
                InteractionInfoSuffix,
                SetThenSave,
                LegacyMinimalTaskReferer,
            ) | (
                0,
                SelectedScore,
                ZeroTime,
                InteractionInfoSuffix,
                SaveOnly,
                LegacyMinimalTaskReferer,
            )
        );
        if !audited {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn resource execution plan mixes incompatible donor wire facts",
            ));
        }
        Ok(())
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
        plan: WellearnResourceExecutionPlan,
    ) -> ProviderResult<WellearnResourceExecutionDocuments>;

    /// Reads one fresh CMI document without starting, setting or saving the
    /// SCO. This is the only transport path used by crash recovery.
    async fn verify_resource(
        &self,
        context: &ProviderContext,
        course_id: &str,
        sco_id: &str,
        mutation_profile: crate::WellearnResourceMutationProfile,
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
    #[allow(
        clippy::too_many_lines,
        reason = "execution keeps selection, fresh rebind, write-plan and exact final verification visible"
    )]
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        validate_capability_step(request)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(unsupported(
                "WELearn resource execution accepts only ResourceExecution",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let selected_score_percent = select_score(settings.resource_score, request);
        let verified_score_percent = settings
            .resource_completion_sequence
            .final_score(selected_score_percent);
        let completion_time_mode =
            effective_completion_time_mode(settings.resource_completion_time_mode, request);
        let plan = WellearnResourceExecutionPlan {
            score_percent: selected_score_percent,
            sequence: settings.resource_completion_sequence,
            time_mode: completion_time_mode,
            cmi_format: settings.resource_completion_cmi_format,
            write_mode: settings.resource_completion_write_mode,
            mutation_profile: settings.resource_mutation_profile,
        };
        plan.validate()?;
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
                    "selected_score_percent": selected_score_percent,
                    "verified_score_percent": verified_score_percent,
                    "score_mode": score_mode(settings.resource_score),
                    "completion_sequence": settings.resource_completion_sequence.as_str(),
                    "verification": "provider_fresh_cmi",
                })),
            })
            .await?;

        let documents = self
            .transport
            .complete_resource(context, &course_id, &sco_id, plan)
            .await?;
        let before = parse_mutation_cmi_baseline(documents.before.as_str())?;
        let already_completed = !documents.mutation_submitted;
        if already_completed {
            let before = before.as_ref().ok_or_else(|| {
                remote_changed("WELearn completed preflight has no initialized CMI")
            })?;
            verify_completed_preset(before, verified_score_percent)?;
        }
        let time_preservation_verified = if let Some(after) = documents.after.as_ref() {
            let after = parse_cmi_snapshot(after.as_str())?;
            verify_completed_preset(&after, verified_score_percent)?;
            if completion_time_mode == WellearnResourceCompletionTimeMode::PreserveFreshTime {
                let before = before.as_ref().ok_or_else(|| {
                    remote_changed("WELearn completion has no fresh time baseline")
                })?;
                verify_preserved_completion_times(before, &after)?;
                Some(true)
            } else {
                None
            }
        } else {
            None
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
                "score_percent": verified_score_percent,
                "selected_score_percent": selected_score_percent,
                "verified_score_percent": verified_score_percent,
                "score_mode": score_mode(settings.resource_score),
                "completion_sequence": settings.resource_completion_sequence.as_str(),
                "configured_completion_time_mode": settings.resource_completion_time_mode.as_str(),
                "completion_time_mode": completion_time_mode.as_str(),
                "completion_cmi_format": settings.resource_completion_cmi_format.as_str(),
                "completion_write_mode": settings.resource_completion_write_mode.as_str(),
                "mutation_profile": settings.resource_mutation_profile.as_str(),
                "mutation_submitted": documents.mutation_submitted,
                "started": documents.started,
                "start_accepted": documents.start_accepted,
                "set_accepted": documents.set_accepted,
                "save_acceptances": documents.save_acceptances,
                "already_completed": already_completed,
                "immediate_cmi_verified": true,
                "time_preservation_verified": time_preservation_verified,
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
        validate_capability_step(request)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(unsupported(
                "WELearn resource verification accepts only ResourceExecution",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let selected_score_percent = select_score(settings.resource_score, request);
        let verified_score_percent = settings
            .resource_completion_sequence
            .final_score(selected_score_percent);
        let completion_time_mode =
            effective_completion_time_mode(settings.resource_completion_time_mode, request);
        WellearnResourceExecutionPlan {
            score_percent: selected_score_percent,
            sequence: settings.resource_completion_sequence,
            time_mode: completion_time_mode,
            cmi_format: settings.resource_completion_cmi_format,
            write_mode: settings.resource_completion_write_mode,
            mutation_profile: settings.resource_mutation_profile,
        }
        .validate()?;
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
            .verify_resource(
                context,
                &course_id,
                &sco_id,
                settings.resource_mutation_profile,
            )
            .await?;
        let snapshot = parse_cmi_snapshot(document.as_str())?;
        verify_completed_preset(&snapshot, verified_score_percent)?;
        Ok(ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({
                "schema": "welearn.resource-completion-verification.v1",
                "preset": "completed_progress_score",
                "score_percent": verified_score_percent,
                "selected_score_percent": selected_score_percent,
                "verified_score_percent": verified_score_percent,
                "score_mode": score_mode(settings.resource_score),
                "completion_sequence": settings.resource_completion_sequence.as_str(),
                "configured_completion_time_mode": settings.resource_completion_time_mode.as_str(),
                "completion_time_mode": completion_time_mode.as_str(),
                "completion_cmi_format": settings.resource_completion_cmi_format.as_str(),
                "completion_write_mode": settings.resource_completion_write_mode.as_str(),
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

fn effective_completion_time_mode(
    configured: WellearnResourceCompletionTimeMode,
    request: &ExecutionRequest,
) -> WellearnResourceCompletionTimeMode {
    if configured != WellearnResourceCompletionTimeMode::Auto {
        return configured;
    }
    if request.capability_plan
        == [
            TaskCapability::DurationReport,
            TaskCapability::ResourceExecution,
        ]
        && request.capability_step_position == 2
    {
        WellearnResourceCompletionTimeMode::PreserveFreshTime
    } else {
        WellearnResourceCompletionTimeMode::ZeroTime
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

fn verify_preserved_completion_times(
    before: &crate::WellearnCmiSnapshot,
    after: &crate::WellearnCmiSnapshot,
) -> ProviderResult<()> {
    let preserved = before.cmi_present()
        && after.cmi_present()
        && before.session_time_raw().is_some()
        && before.total_time_raw().is_some()
        && before.session_time_raw() == after.session_time_raw()
        && before.total_time_raw() == after.total_time_raw();
    if !preserved {
        return Err(remote_changed(
            "WELearn completion did not preserve the fresh CMI time fields",
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

fn validate_capability_step(request: &ExecutionRequest) -> ProviderResult<()> {
    if !request.has_valid_capability_step() {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn resource execution received an invalid capability step binding",
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
        RESOURCE_COMPLETION_SEQUENCE_KEY, RESOURCE_SCORE_MAX_PERCENT_KEY,
        RESOURCE_SCORE_MIN_PERCENT_KEY, RESOURCE_SCORE_MODE_KEY, RESOURCE_SCORE_PERCENT_KEY,
        runtime_settings_schema,
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

    type CompletionCall = (
        String,
        String,
        u8,
        WellearnResourceCompletionSequence,
        WellearnResourceCompletionTimeMode,
        WellearnResourceCompletionCmiFormat,
        crate::WellearnResourceCompletionWriteMode,
        crate::WellearnResourceMutationProfile,
    );

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
                "schema": "welearn.sco.v2",
                "course_id": "1001",
                "unit_index": 0,
                "unit_title": "Unit",
                "unit_code": null,
                "sco_id": "301",
                "visible": self.visible,
                "completion_observation": "pending",
                "sco_index": 0,
                "unit_visible": true,
                "sco_visible": true,
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
                        TaskCapability::DurationRead,
                        TaskCapability::DurationReport,
                    ],
                    fingerprint: crate::task_inventory::task_fingerprint(&normalized).unwrap(),
                    normalized: normalized.clone(),
                    raw_sanitized: serde_json::json!({"schema": "welearn.sco.raw.v2"}),
                },
                normalized_detail: serde_json::json!({
                    "schema": "welearn.sco-task-detail.v2",
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
    enum FixtureBaseline {
        #[default]
        Initialized,
        Uninitialized,
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<CompletionCall>>,
        verifications: Mutex<Vec<(String, String)>>,
        already_completed: bool,
        drift_score: bool,
        explicit_rejections: bool,
        baseline: FixtureBaseline,
    }

    #[async_trait]
    impl WellearnResourceExecutionTransport for FixtureTransport {
        async fn complete_resource(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            plan: WellearnResourceExecutionPlan,
        ) -> ProviderResult<WellearnResourceExecutionDocuments> {
            self.calls.lock().unwrap().push((
                course_id.to_owned(),
                sco_id.to_owned(),
                plan.score_percent,
                plan.sequence,
                plan.time_mode,
                plan.cmi_format,
                plan.write_mode,
                plan.mutation_profile,
            ));
            if self.already_completed {
                return Ok(WellearnResourceExecutionDocuments::already_completed(
                    WellearnCmiDocument::try_new(AFTER).unwrap(),
                ));
            }
            let final_score = plan.sequence.final_score(plan.score_percent);
            let reflected_score = if self.drift_score {
                if final_score == 100 {
                    99
                } else {
                    final_score + 1
                }
            } else {
                final_score
            };
            let after = AFTER.replace(
                "\\\"scaled\\\":\\\"82\\\"",
                &format!("\\\"scaled\\\":\\\"{reflected_score}\\\""),
            );
            let accepted = !self.explicit_rejections;
            let set_accepted = match plan.write_mode {
                crate::WellearnResourceCompletionWriteMode::SetThenSave => Some(accepted),
                crate::WellearnResourceCompletionWriteMode::SaveOnly => None,
            };
            let save_count = match plan.sequence {
                WellearnResourceCompletionSequence::SelectedScore => 1,
                WellearnResourceCompletionSequence::CurrentDonorDualSave100 => 2,
            };
            let before = match self.baseline {
                FixtureBaseline::Initialized => BEFORE,
                FixtureBaseline::Uninitialized => "学习数据不正确，请先开始学习",
            };
            Ok(WellearnResourceExecutionDocuments::submitted(
                WellearnCmiDocument::try_new(before).unwrap(),
                WellearnCmiDocument::try_new(after).unwrap(),
                true,
                accepted,
                set_accepted,
                vec![accepted; save_count],
            ))
        }

        async fn verify_resource(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            _mutation_profile: crate::WellearnResourceMutationProfile,
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

    #[test]
    fn execution_plan_accepts_only_complete_audited_donor_profiles() {
        use crate::{
            WellearnResourceCompletionCmiFormat::{InteractionInfoSuffix, Json},
            WellearnResourceCompletionSequence::{CurrentDonorDualSave100, SelectedScore},
            WellearnResourceCompletionTimeMode::{PreserveFreshTime, ZeroTime},
            WellearnResourceCompletionWriteMode::{SaveOnly, SetThenSave},
            WellearnResourceMutationProfile::{CurrentFullSimpleReferer, LegacyMinimalTaskReferer},
        };

        let current_completion = WellearnResourceExecutionPlan {
            score_percent: 82,
            sequence: CurrentDonorDualSave100,
            time_mode: ZeroTime,
            cmi_format: Json,
            write_mode: SetThenSave,
            mutation_profile: CurrentFullSimpleReferer,
        };
        let current_duration_final = WellearnResourceExecutionPlan {
            score_percent: 100,
            sequence: SelectedScore,
            time_mode: PreserveFreshTime,
            ..current_completion
        };
        let legacy_completion = WellearnResourceExecutionPlan {
            score_percent: 82,
            sequence: SelectedScore,
            time_mode: ZeroTime,
            cmi_format: InteractionInfoSuffix,
            write_mode: SetThenSave,
            mutation_profile: LegacyMinimalTaskReferer,
        };
        let auto_duration_final = WellearnResourceExecutionPlan {
            score_percent: 0,
            write_mode: SaveOnly,
            ..legacy_completion
        };
        for plan in [
            current_completion,
            current_duration_final,
            legacy_completion,
            auto_duration_final,
        ] {
            plan.validate().unwrap();
        }

        let mixed_save_only = WellearnResourceExecutionPlan {
            write_mode: SaveOnly,
            ..current_completion
        };
        assert_eq!(
            mixed_save_only.validate().unwrap_err().kind,
            ProviderErrorKind::Internal
        );
        let wrong_atomic_score = WellearnResourceExecutionPlan {
            score_percent: 99,
            ..current_duration_final
        };
        assert!(wrong_atomic_score.validate().is_err());
        let legacy_without_suffix = WellearnResourceExecutionPlan {
            cmi_format: Json,
            ..legacy_completion
        };
        assert!(legacy_without_suffix.validate().is_err());
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
            &[(
                "1001".to_owned(),
                "301".to_owned(),
                82,
                WellearnResourceCompletionSequence::SelectedScore,
                WellearnResourceCompletionTimeMode::ZeroTime,
                WellearnResourceCompletionCmiFormat::InteractionInfoSuffix,
                crate::WellearnResourceCompletionWriteMode::SetThenSave,
                crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
            )]
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
    async fn zero_time_completion_accepts_an_explicit_uninitialized_baseline() {
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            Arc::new(FixtureTransport {
                baseline: FixtureBaseline::Uninitialized,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(
            outcome.result_sanitized["completion_time_mode"],
            "zero_time"
        );
        assert_eq!(outcome.result_sanitized["mutation_submitted"], true);
    }

    #[tokio::test]
    async fn explicit_negative_receipts_still_require_and_can_pass_fresh_verification() {
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            Arc::new(FixtureTransport {
                explicit_rejections: true,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["start_accepted"], false);
        assert_eq!(outcome.result_sanitized["set_accepted"], false);
        assert_eq!(
            outcome.result_sanitized["save_acceptances"],
            serde_json::json!([false])
        );
        assert_eq!(
            outcome.result_sanitized["verification"],
            "provider_fresh_cmi"
        );
    }

    #[tokio::test]
    async fn save_only_mode_omits_set_receipt_and_still_verifies_the_goal() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &save_only_request(), &FixtureEvents)
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(
            outcome.result_sanitized["completion_write_mode"],
            "save_only"
        );
        assert!(outcome.result_sanitized["set_accepted"].is_null());
        assert_eq!(
            transport.calls.lock().unwrap()[0].6,
            crate::WellearnResourceCompletionWriteMode::SaveOnly
        );
    }

    #[tokio::test]
    async fn historical_mutation_profile_is_frozen_into_the_transport_plan() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &legacy_mutation_request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(
            outcome.result_sanitized["mutation_profile"],
            "legacy_minimal_task_referer"
        );
        assert_eq!(
            transport.calls.lock().unwrap()[0].7,
            crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer
        );
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

    #[tokio::test]
    async fn current_donor_dual_save_sequence_verifies_its_final_hundred_goal() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &current_donor_request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.result_sanitized["selected_score_percent"], 82);
        assert_eq!(outcome.result_sanitized["verified_score_percent"], 100);
        assert_eq!(outcome.result_sanitized["score_percent"], 100);
        assert_eq!(
            outcome.result_sanitized["completion_sequence"],
            "current_donor_dual_save_100"
        );
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[(
                "1001".to_owned(),
                "301".to_owned(),
                82,
                WellearnResourceCompletionSequence::CurrentDonorDualSave100,
                WellearnResourceCompletionTimeMode::ZeroTime,
                WellearnResourceCompletionCmiFormat::Json,
                crate::WellearnResourceCompletionWriteMode::SetThenSave,
                crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer,
            )]
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
            &[(
                "1001".to_owned(),
                "301".to_owned(),
                82,
                WellearnResourceCompletionSequence::SelectedScore,
                WellearnResourceCompletionTimeMode::ZeroTime,
                WellearnResourceCompletionCmiFormat::InteractionInfoSuffix,
                crate::WellearnResourceCompletionWriteMode::SetThenSave,
                crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
            )]
        );
    }

    #[tokio::test]
    async fn duration_then_completion_preserves_fresh_cmi_times() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &preserved_time_request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(
            outcome.result_sanitized["completion_time_mode"],
            "preserve_fresh_time"
        );
        assert_eq!(outcome.result_sanitized["time_preservation_verified"], true);
        assert_eq!(
            transport.calls.lock().unwrap()[0].4,
            WellearnResourceCompletionTimeMode::PreserveFreshTime
        );
    }

    #[tokio::test]
    async fn auto_time_mode_uses_immutable_duration_then_resource_step_context() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let outcome = execution
            .execute(&context(), &composite_resource_request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(
            outcome.result_sanitized["configured_completion_time_mode"],
            "auto"
        );
        assert_eq!(
            outcome.result_sanitized["completion_time_mode"],
            "preserve_fresh_time"
        );
        assert_eq!(
            transport.calls.lock().unwrap()[0].4,
            WellearnResourceCompletionTimeMode::PreserveFreshTime
        );
    }

    #[tokio::test]
    async fn malformed_step_context_fails_before_fresh_detail_or_transport() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let mut request = composite_resource_request();
        request.capability_step_position = 1;
        let error = execution
            .execute(&context(), &request, &FixtureEvents)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Internal);
        assert!(transport.calls.lock().unwrap().is_empty());
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
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(82),
                ),
                (
                    RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("selected_score".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_CMI_FORMAT_KEY.to_owned(),
                    ProviderSettingValue::Choice("interaction_info_suffix".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_MUTATION_PROFILE_KEY.to_owned(),
                    ProviderSettingValue::Choice("legacy_minimal_task_referer".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }

    fn random_request() -> ExecutionRequest {
        random_request_with_mode("random_range", 73, 79)
    }

    fn current_donor_request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(82),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("current_donor_dual_save_100".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }

    fn preserved_time_request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(100),
                ),
                (
                    RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("selected_score".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_TIME_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice("preserve_fresh_time".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }

    fn save_only_request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(0),
                ),
                (
                    RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("selected_score".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_TIME_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice("zero_time".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_CMI_FORMAT_KEY.to_owned(),
                    ProviderSettingValue::Choice("interaction_info_suffix".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_WRITE_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice("save_only".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_MUTATION_PROFILE_KEY.to_owned(),
                    ProviderSettingValue::Choice("legacy_minimal_task_referer".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }

    fn legacy_mutation_request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(82),
                ),
                (
                    RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("selected_score".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_CMI_FORMAT_KEY.to_owned(),
                    ProviderSettingValue::Choice("interaction_info_suffix".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_MUTATION_PROFILE_KEY.to_owned(),
                    ProviderSettingValue::Choice("legacy_minimal_task_referer".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
            ..request()
        }
    }

    fn composite_resource_request() -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    RESOURCE_SCORE_PERCENT_KEY.to_owned(),
                    ProviderSettingValue::Integer(100),
                ),
                (
                    RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("selected_score".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ],
            capability_step_position: 2,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
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
                (
                    RESOURCE_COMPLETION_SEQUENCE_KEY.to_owned(),
                    ProviderSettingValue::Choice("selected_score".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_COMPLETION_CMI_FORMAT_KEY.to_owned(),
                    ProviderSettingValue::Choice("interaction_info_suffix".to_owned()),
                ),
                (
                    crate::runtime_settings::RESOURCE_MUTATION_PROFILE_KEY.to_owned(),
                    ProviderSettingValue::Choice("legacy_minimal_task_referer".to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            capability_plan: vec![TaskCapability::ResourceExecution],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
        }
    }
}
