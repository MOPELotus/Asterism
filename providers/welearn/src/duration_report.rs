use std::{fmt, sync::Arc};

use asterism_domain::{
    CourseId, ExecutionId, LogLevel, ProviderAccountId, ProviderId, TaskCapability, TaskId,
};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionMutationSequenceAdvanceCondition, ExecutionMutationSequencePhase,
    ExecutionMutationSequencePlan, ExecutionMutationVerification, ExecutionOutcome,
    ExecutionRequest, ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionLog,
    ProviderExecutionPlanArtifact, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, TaskDetailCapability, TaskExecutionCapability,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    WellearnCmiDocument, WellearnCmiSnapshot,
    cmi::{parse_cmi_snapshot, parse_sco_identity},
    execution_selection::uniform_u64,
    metadata::development_metadata,
    runtime_settings::{
        MAX_DURATION_REPORT_SECONDS, MIN_DURATION_REPORT_SECONDS, WellearnDurationProtocolMode,
        WellearnDurationTarget, WellearnRuntimeSettings,
    },
    task_detail::validate_fresh_execution_detail,
};

/// Namespaced credential-free persistence type for one exact singleton
/// `DurationReport` authority.
pub const WELLEARN_DURATION_REPORT_BINDING_TYPE: &str = "welearn.duration-report-binding.v1";
/// Receipt-conditional sequence type derived from one duration binding and
/// its fresh initialized/uninitialized baseline decision.
pub const WELLEARN_DURATION_REPORT_SEQUENCE_TYPE: &str = "welearn.duration-report-sequence.v1";

const WELLEARN_DURATION_REPORT_BINDING_VERSION: u16 = 1;
const MAX_DURATION_REPORT_BINDING_BYTES: usize = 16 * 1_024;
const DURATION_REPORT_BINDING_DIGEST_DOMAIN: &[u8] =
    b"asterism.welearn.duration-report-binding.v1\0";
const DURATION_REPORT_VERIFICATION_DIGEST_DOMAIN: &[u8] =
    b"asterism.welearn.duration-report-verification.v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WellearnDurationMutationKind {
    Start,
    PreserveKeep,
    CounterKeep,
    ImplicitKeep,
    Finalize,
}

impl WellearnDurationMutationKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "welearn.duration.start",
            Self::PreserveKeep => "welearn.duration.preserve-keep",
            Self::CounterKeep => "welearn.duration.counter-keep",
            Self::ImplicitKeep => "welearn.duration.implicit-keep",
            Self::Finalize => "welearn.duration.finalize",
        }
    }
}

/// Complete before/after evidence returned by one bounded `WELearn` duration
/// lifecycle. Response bodies remain redacted and zeroized by their wrappers.
#[derive(Debug)]
pub struct WellearnDurationReportDocuments {
    before: WellearnCmiDocument,
    after: WellearnCmiDocument,
    started: bool,
    heartbeat_count: u32,
    start_accepted: Option<bool>,
    heartbeat_accepted: u32,
    heartbeat_rejected: u32,
    final_accepted: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WellearnDurationReportReceipts {
    pub(crate) start_accepted: Option<bool>,
    pub(crate) heartbeat_accepted: u32,
    pub(crate) heartbeat_rejected: u32,
    pub(crate) final_accepted: Option<bool>,
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
            start_accepted: if started { Some(true) } else { None },
            heartbeat_accepted: heartbeat_count,
            heartbeat_rejected: 0,
            final_accepted: Some(true),
        }
    }

    pub(crate) const fn with_receipts(
        before: WellearnCmiDocument,
        after: WellearnCmiDocument,
        started: bool,
        heartbeat_count: u32,
        receipts: WellearnDurationReportReceipts,
    ) -> Self {
        Self {
            before,
            after,
            started,
            heartbeat_count,
            start_accepted: receipts.start_accepted,
            heartbeat_accepted: receipts.heartbeat_accepted,
            heartbeat_rejected: receipts.heartbeat_rejected,
            final_accepted: receipts.final_accepted,
        }
    }

    fn validate_for_plan(&self, plan: WellearnDurationReportPlan) -> ProviderResult<()> {
        if self.start_accepted.is_some() != self.started {
            return Err(invalid_duration_documents());
        }
        let recorded_heartbeats = self
            .heartbeat_accepted
            .checked_add(self.heartbeat_rejected)
            .ok_or_else(invalid_duration_documents)?;
        if recorded_heartbeats != self.heartbeat_count {
            return Err(invalid_duration_documents());
        }

        let complete_intervals = plan.duration_seconds / plan.heartbeat_interval_seconds;
        let expected_complete =
            u32::try_from(complete_intervals).map_err(|_| invalid_duration_documents())?;
        let valid_protocol_shape = match plan.protocol_mode {
            WellearnDurationProtocolMode::PreserveFresh => {
                self.heartbeat_count == expected_complete.saturating_add(1)
                    && self.final_accepted.is_some()
            }
            WellearnDurationProtocolMode::ImplicitServer => {
                self.started
                    && self.heartbeat_count == expected_complete
                    && self.final_accepted.is_some()
            }
            WellearnDurationProtocolMode::ClientCounter => {
                let expected = u32::try_from(plan.duration_seconds)
                    .map_err(|_| invalid_duration_documents())?;
                self.started
                    && self.start_accepted == Some(true)
                    && self.final_accepted.is_none()
                    && self.heartbeat_rejected <= 1
                    && if self.heartbeat_rejected == 0 {
                        self.heartbeat_count == expected
                    } else {
                        self.heartbeat_count <= expected
                    }
            }
        };
        if !valid_protocol_shape {
            return Err(invalid_duration_documents());
        }
        Ok(())
    }
}

fn invalid_duration_documents() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn duration transport returned receipts inconsistent with the frozen plan",
    )
}

/// Immutable, bounded donor wire plan selected from one frozen execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnDurationReportPlan {
    pub duration_seconds: u64,
    pub heartbeat_interval_seconds: u64,
    pub protocol_mode: WellearnDurationProtocolMode,
}

impl WellearnDurationReportPlan {
    /// Validates the complete bounded donor cadence before any lifecycle I/O.
    ///
    /// # Errors
    ///
    /// Returns an internal error when duration bounds or the protocol-specific
    /// heartbeat interval do not match audited behavior.
    pub fn validate(self) -> ProviderResult<()> {
        if !(MIN_DURATION_REPORT_SECONDS..=MAX_DURATION_REPORT_SECONDS)
            .contains(&self.duration_seconds)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn duration plan contains an out-of-range target",
            ));
        }
        let evidenced_interval = match self.protocol_mode {
            WellearnDurationProtocolMode::ClientCounter => 1,
            WellearnDurationProtocolMode::PreserveFresh
            | WellearnDurationProtocolMode::ImplicitServer => 60,
        };
        if self.heartbeat_interval_seconds != evidenced_interval {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn duration plan contains a protocol/interval mismatch",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnDurationReportPlanWire {
    duration_seconds: u64,
    heartbeat_interval_seconds: u64,
    protocol_mode: String,
}

impl From<WellearnDurationReportPlan> for WellearnDurationReportPlanWire {
    fn from(plan: WellearnDurationReportPlan) -> Self {
        Self {
            duration_seconds: plan.duration_seconds,
            heartbeat_interval_seconds: plan.heartbeat_interval_seconds,
            protocol_mode: plan.protocol_mode.as_str().to_owned(),
        }
    }
}

impl TryFrom<WellearnDurationReportPlanWire> for WellearnDurationReportPlan {
    type Error = ProviderError;

    fn try_from(wire: WellearnDurationReportPlanWire) -> Result<Self, Self::Error> {
        let protocol_mode = match wire.protocol_mode.as_str() {
            "preserve_fresh" => WellearnDurationProtocolMode::PreserveFresh,
            "client_counter" => WellearnDurationProtocolMode::ClientCounter,
            "implicit_server" => WellearnDurationProtocolMode::ImplicitServer,
            _ => return Err(invalid_duration_report_binding()),
        };
        let plan = Self {
            duration_seconds: wire.duration_seconds,
            heartbeat_interval_seconds: wire.heartbeat_interval_seconds,
            protocol_mode,
        };
        plan.validate()
            .map_err(|_| invalid_duration_report_binding())?;
        Ok(plan)
    }
}

/// Immutable credential-free binding between one singleton Core Execution and
/// the exact donor duration plan selected from its frozen runtime settings.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnDurationReportBinding {
    provider_id: ProviderId,
    provider_account_id: ProviderAccountId,
    execution_id: ExecutionId,
    task_id: TaskId,
    course_id: Option<CourseId>,
    remote_task_id: String,
    runtime_settings_schema_version: u32,
    plan: WellearnDurationReportPlan,
    execution_plan_artifact: ProviderExecutionPlanArtifact,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnDurationReportBindingWire {
    version: u16,
    provider_id: String,
    provider_account_id: ProviderAccountId,
    execution_id: ExecutionId,
    task_id: TaskId,
    course_id: Option<CourseId>,
    remote_task_id: String,
    runtime_settings_schema_version: u32,
    plan: WellearnDurationReportPlanWire,
}

impl WellearnDurationReportBinding {
    /// Freezes local identity and the resolved duration target without
    /// retaining a Cookie, CMI body, route or receipt.
    ///
    /// # Errors
    ///
    /// Rejects Provider/request/settings/plan drift and malformed SCO identity.
    pub fn try_new(
        context: &ProviderContext,
        request: &ExecutionRequest,
        plan: WellearnDurationReportPlan,
    ) -> ProviderResult<Self> {
        if context.provider_id.as_str() != crate::metadata::PROVIDER_ID
            || !request.has_valid_capability_step()
            || request.requested_capabilities != [TaskCapability::DurationReport]
            || plan != derive_duration_report_plan(request)?
        {
            return Err(invalid_duration_report_binding());
        }
        let binding = Self {
            provider_id: context.provider_id.clone(),
            provider_account_id: context.account_id,
            execution_id: request.execution_id,
            task_id: request.task_id,
            course_id: request.course_id,
            remote_task_id: request.remote_task_id.clone(),
            runtime_settings_schema_version: request.runtime_settings.schema_version,
            plan,
            execution_plan_artifact:
                crate::execution::singleton_execution_plan_artifact_for_request(context, request)?,
        };
        binding.validate_shape()?;
        binding.validate_request_artifact(request.provider_plan_artifact.as_ref())?;
        Ok(binding)
    }

    pub const fn plan(&self) -> WellearnDurationReportPlan {
        self.plan
    }

    pub(crate) fn validate_remote_identity(
        &self,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<()> {
        let (bound_course_id, bound_sco_id) = parse_sco_identity(&self.remote_task_id)
            .map_err(|_| invalid_duration_report_binding())?;
        if bound_course_id != course_id || bound_sco_id != sco_id {
            return Err(invalid_duration_report_binding());
        }
        Ok(())
    }

    /// Projects the private binding into the only sanitized artifact identity
    /// accepted by Core sequence recovery. The sequence uses the final
    /// artifact digest, never the inner binding digest directly.
    ///
    /// # Errors
    ///
    /// Rejects an invalid binding or generic artifact projection.
    pub fn to_provider_execution_plan_artifact(
        &self,
    ) -> ProviderResult<ProviderExecutionPlanArtifact> {
        self.validate_shape()?;
        Ok(self.execution_plan_artifact.clone())
    }

    /// Encodes one bounded deny-unknown v1 binding.
    ///
    /// # Errors
    ///
    /// Rejects an invalid binding, serialization failure or size overflow.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate_shape()?;
        let encoded = serde_json::to_vec(&WellearnDurationReportBindingWire {
            version: WELLEARN_DURATION_REPORT_BINDING_VERSION,
            provider_id: self.provider_id.as_str().to_owned(),
            provider_account_id: self.provider_account_id,
            execution_id: self.execution_id,
            task_id: self.task_id,
            course_id: self.course_id,
            remote_task_id: self.remote_task_id.clone(),
            runtime_settings_schema_version: self.runtime_settings_schema_version,
            plan: self.plan.into(),
        })
        .map_err(|_| invalid_duration_report_binding())?;
        if encoded.is_empty() || encoded.len() > MAX_DURATION_REPORT_BINDING_BYTES {
            return Err(invalid_duration_report_binding());
        }
        Ok(encoded)
    }

    /// Decodes only this Provider's exact namespaced binding and recomputes it
    /// from independently loaded immutable execution values.
    ///
    /// # Errors
    ///
    /// Rejects foreign types, malformed/unknown/oversized bytes and all
    /// identity, settings or plan substitution.
    pub fn decode(
        binding_type: &str,
        encoded: &[u8],
        context: &ProviderContext,
        request: &ExecutionRequest,
        plan: WellearnDurationReportPlan,
    ) -> ProviderResult<Self> {
        if binding_type != WELLEARN_DURATION_REPORT_BINDING_TYPE
            || encoded.is_empty()
            || encoded.len() > MAX_DURATION_REPORT_BINDING_BYTES
        {
            return Err(invalid_duration_report_binding());
        }
        let wire: WellearnDurationReportBindingWire =
            serde_json::from_slice(encoded).map_err(|_| invalid_duration_report_binding())?;
        if wire.version != WELLEARN_DURATION_REPORT_BINDING_VERSION {
            return Err(invalid_duration_report_binding());
        }
        let decoded = Self {
            provider_id: ProviderId::new(wire.provider_id)
                .map_err(|_| invalid_duration_report_binding())?,
            provider_account_id: wire.provider_account_id,
            execution_id: wire.execution_id,
            task_id: wire.task_id,
            course_id: wire.course_id,
            remote_task_id: wire.remote_task_id,
            runtime_settings_schema_version: wire.runtime_settings_schema_version,
            plan: WellearnDurationReportPlan::try_from(wire.plan)?,
            execution_plan_artifact:
                crate::execution::singleton_execution_plan_artifact_for_request(context, request)?,
        };
        decoded.validate(context, request, plan)?;
        Ok(decoded)
    }

    /// Recomputes the complete binding from independent values.
    ///
    /// # Errors
    ///
    /// Rejects every field substitution or invalid shape.
    pub fn validate(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        plan: WellearnDurationReportPlan,
    ) -> ProviderResult<()> {
        self.validate_shape()?;
        if *self != Self::try_new(context, request, plan)? {
            return Err(invalid_duration_report_binding());
        }
        Ok(())
    }

    /// Projects the exact donor mutation phases after the fresh baseline has
    /// determined whether the historical preservation profile needs `start`.
    /// The complete sequence is persisted before its first issue.
    ///
    /// # Errors
    ///
    /// Rejects an impossible start decision or phase/count overflow.
    pub fn mutation_sequence_plan(
        &self,
        started: bool,
    ) -> ProviderResult<ExecutionMutationSequencePlan> {
        self.validate_shape()?;
        if !started
            && !matches!(
                self.plan.protocol_mode,
                WellearnDurationProtocolMode::PreserveFresh
            )
        {
            return Err(invalid_duration_report_binding());
        }
        let mut phases = Vec::with_capacity(3);
        if started {
            phases.push(duration_sequence_phase(
                WellearnDurationMutationKind::Start,
                1,
                1,
                false,
                if self.plan.protocol_mode == WellearnDurationProtocolMode::ClientCounter {
                    ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached
                } else {
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached
                },
            )?);
        }
        match self.plan.protocol_mode {
            WellearnDurationProtocolMode::PreserveFresh => {
                let keeps = u32::try_from(
                    self.plan.duration_seconds / self.plan.heartbeat_interval_seconds + 1,
                )
                .map_err(|_| invalid_duration_report_binding())?;
                phases.push(duration_sequence_phase(
                    WellearnDurationMutationKind::PreserveKeep,
                    keeps,
                    keeps,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                )?);
                phases.push(duration_sequence_phase(
                    WellearnDurationMutationKind::Finalize,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                )?);
            }
            WellearnDurationProtocolMode::ClientCounter => {
                let maximum = u32::try_from(self.plan.duration_seconds)
                    .map_err(|_| invalid_duration_report_binding())?;
                phases.push(duration_sequence_phase(
                    WellearnDurationMutationKind::CounterKeep,
                    1,
                    maximum,
                    true,
                    ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached,
                )?);
            }
            WellearnDurationProtocolMode::ImplicitServer => {
                let keeps = u32::try_from(
                    self.plan.duration_seconds / self.plan.heartbeat_interval_seconds,
                )
                .map_err(|_| invalid_duration_report_binding())?;
                phases.push(duration_sequence_phase(
                    WellearnDurationMutationKind::ImplicitKeep,
                    keeps,
                    keeps,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                )?);
                phases.push(duration_sequence_phase(
                    WellearnDurationMutationKind::Finalize,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                )?);
            }
        }
        ExecutionMutationSequencePlan::try_new(
            self.to_provider_execution_plan_artifact()?
                .artifact_digest(),
            WELLEARN_DURATION_REPORT_SEQUENCE_TYPE,
            phases,
        )
        .map_err(|_| invalid_duration_report_binding())
    }

    fn binding_digest(&self) -> ProviderResult<[u8; 32]> {
        let mut digest = Sha256::new();
        digest.update(DURATION_REPORT_BINDING_DIGEST_DOMAIN);
        digest.update(self.encode()?);
        Ok(digest.finalize().into())
    }

    fn validate_shape(&self) -> ProviderResult<()> {
        self.plan
            .validate()
            .map_err(|_| invalid_duration_report_binding())?;
        if self.provider_id.as_str() != crate::metadata::PROVIDER_ID
            || self.runtime_settings_schema_version == 0
            || self.execution_plan_artifact.provider_id() != &self.provider_id
            || self.execution_plan_artifact.artifact_type()
                != crate::execution::WELLEARN_SINGLETON_EXECUTION_PLAN_ARTIFACT_TYPE
            || self.remote_task_id.is_empty()
            || self.remote_task_id.len() > 512
            || self.remote_task_id.trim() != self.remote_task_id
            || self.remote_task_id.chars().any(char::is_control)
            || parse_sco_identity(&self.remote_task_id).is_err()
        {
            return Err(invalid_duration_report_binding());
        }
        Ok(())
    }

    fn validate_request_artifact(
        &self,
        artifact: Option<&ProviderExecutionPlanArtifact>,
    ) -> ProviderResult<()> {
        if artifact.is_some_and(
            |artifact| match self.to_provider_execution_plan_artifact() {
                Ok(expected) => expected != *artifact,
                Err(_) => true,
            },
        ) {
            return Err(invalid_duration_report_binding());
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnDurationReportBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnDurationReportBinding")
            .field("provider_id", &self.provider_id)
            .field("provider_account_id", &"[REDACTED]")
            .field("execution_id", &"[REDACTED]")
            .field("task_id", &"[REDACTED]")
            .field("course_id", &"[REDACTED]")
            .field("remote_task_id", &"[REDACTED]")
            .field(
                "runtime_settings_schema_version",
                &self.runtime_settings_schema_version,
            )
            .field("plan", &self.plan)
            .finish()
    }
}

fn duration_sequence_phase(
    kind: WellearnDurationMutationKind,
    minimum: u32,
    maximum: u32,
    stop_repeating_after_rejection: bool,
    advance_condition: ExecutionMutationSequenceAdvanceCondition,
) -> ProviderResult<ExecutionMutationSequencePhase> {
    ExecutionMutationSequencePhase::try_new(
        kind.as_str(),
        minimum,
        maximum,
        stop_repeating_after_rejection,
        advance_condition,
        None,
    )
    .map_err(|_| invalid_duration_report_binding())
}

fn invalid_duration_report_binding() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn duration report binding is invalid or drifted",
    )
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
        binding: &WellearnDurationReportBinding,
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
    #[allow(
        clippy::too_many_lines,
        reason = "duration verification keeps receipt diagnostics and preservation predicates adjacent"
    )]
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_context(context, &self.metadata)?;
        if !request.has_valid_capability_step() {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn duration received an invalid capability step binding",
            ));
        }
        if request.requested_capabilities != [TaskCapability::DurationReport] {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn duration execution accepts only DurationReport",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let duration_seconds = select_duration(settings.duration_report, request);
        let plan = WellearnDurationReportPlan {
            duration_seconds,
            heartbeat_interval_seconds: settings.duration_heartbeat_interval_seconds,
            protocol_mode: settings.duration_protocol_mode,
        };
        plan.validate()?;
        let binding = WellearnDurationReportBinding::try_new(context, request, plan)?;
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
            .report_duration(context, &course_id, &sco_id, &binding, events)
            .await?;
        documents.validate_for_plan(plan)?;
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
        let final_mutation_verification_recorded =
            persist_duration_report_verification(events, &binding, &documents).await?;
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
                    "start_accepted": documents.start_accepted,
                    "heartbeat_accepted": documents.heartbeat_accepted,
                    "heartbeat_rejected": documents.heartbeat_rejected,
                    "final_accepted": documents.final_accepted,
                    "donor_request_delay_seconds": donor_request_delay_seconds(
                        settings.duration_protocol_mode,
                        documents.started,
                    ),
                    "duration_report_seconds": duration_seconds,
                    "duration_report_mode": duration_mode(settings.duration_report),
                    "duration_protocol_mode": settings.duration_protocol_mode.as_str(),
                    "duration_observation_changed": true,
                    "final_mutation_verification_recorded": final_mutation_verification_recorded,
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
                "start_accepted": documents.start_accepted,
                "heartbeat_accepted": documents.heartbeat_accepted,
                "heartbeat_rejected": documents.heartbeat_rejected,
                "final_accepted": documents.final_accepted,
                "donor_request_delay_seconds": donor_request_delay_seconds(
                    settings.duration_protocol_mode,
                    documents.started,
                ),
                "duration_report_seconds": duration_seconds,
                "duration_report_mode": duration_mode(settings.duration_report),
                "duration_protocol_mode": settings.duration_protocol_mode.as_str(),
                "completion_preserved": true,
                "progress_preserved": true,
                "score_preserved": true,
                "duration_observation_changed": true,
                "final_mutation_verification_recorded": final_mutation_verification_recorded,
            }),
        })
    }
}

async fn persist_duration_report_verification(
    events: &(dyn ExecutionEventSink + Send + Sync),
    binding: &WellearnDurationReportBinding,
    documents: &WellearnDurationReportDocuments,
) -> ProviderResult<bool> {
    let final_accepted = match binding.plan.protocol_mode {
        WellearnDurationProtocolMode::ClientCounter => documents.heartbeat_rejected == 0,
        WellearnDurationProtocolMode::PreserveFresh
        | WellearnDurationProtocolMode::ImplicitServer => documents.final_accepted == Some(true),
    };
    if !final_accepted {
        return Ok(false);
    }
    let ordinal = u32::from(documents.started)
        .checked_add(documents.heartbeat_count)
        .and_then(|value| {
            value.checked_add(u32::from(!matches!(
                binding.plan.protocol_mode,
                WellearnDurationProtocolMode::ClientCounter
            )))
        })
        .ok_or_else(invalid_duration_report_binding)?;
    let mut digest = Sha256::new();
    digest.update(DURATION_REPORT_VERIFICATION_DIGEST_DOMAIN);
    digest.update(binding.binding_digest()?);
    digest.update(ordinal.to_be_bytes());
    digest.update(documents.before.as_str().as_bytes());
    digest.update(documents.after.as_str().as_bytes());
    let verification = ExecutionMutationVerification::new(ordinal, digest.finalize().into(), true)
        .map_err(|_| invalid_duration_report_binding())?;
    let sink = events.mutation_sink().ok_or_else(|| {
        ProviderError::human_required(
            "WELearn duration verification cannot be durably attached to its final mutation",
            asterism_domain::HumanRequiredReason::ManualIntervention,
        )
    })?;
    sink.record_verification(verification).await.map_err(|_| {
        ProviderError::human_required(
            "WELearn duration verification persistence failed after mutation",
            asterism_domain::HumanRequiredReason::ManualIntervention,
        )
    })?;
    Ok(true)
}

const fn donor_request_delay_seconds(mode: WellearnDurationProtocolMode, started: bool) -> u64 {
    if matches!(mode, WellearnDurationProtocolMode::PreserveFresh) {
        crate::runtime_settings::LEGACY_DURATION_REQUEST_INTERVAL_SECONDS
            * if started { 4 } else { 3 }
    } else {
        0
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

pub(crate) fn derive_duration_report_plan(
    request: &ExecutionRequest,
) -> ProviderResult<WellearnDurationReportPlan> {
    let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
    let plan = WellearnDurationReportPlan {
        duration_seconds: select_duration(settings.duration_report, request),
        heartbeat_interval_seconds: settings.duration_heartbeat_interval_seconds,
        protocol_mode: settings.duration_protocol_mode,
    };
    plan.validate()?;
    Ok(plan)
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
        DURATION_HEARTBEAT_INTERVAL_KEY, DURATION_PROTOCOL_MODE_KEY,
        DURATION_REPORT_MAX_SECONDS_KEY, DURATION_REPORT_MIN_SECONDS_KEY, DURATION_REPORT_MODE_KEY,
        DURATION_REPORT_SECONDS_KEY, runtime_settings_schema,
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
                "schema": "welearn.sco.v2",
                "course_id": "1001",
                "unit_index": 0,
                "unit_title": "Unit",
                "unit_code": null,
                "sco_id": "301",
                "visible": true,
                "completion_observation": "in_progress",
                "sco_index": 0,
                "unit_visible": true,
                "sco_visible": true,
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

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    enum FixtureBehavior {
        #[default]
        Normal,
        DriftCompletion,
        UnchangedDuration,
        IncompleteVerification,
        ExplicitRejections,
        MalformedReceipts,
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: AtomicUsize,
        settings: Mutex<Option<(u64, u64, WellearnDurationProtocolMode)>>,
        behavior: FixtureBehavior,
    }

    #[async_trait]
    impl WellearnDurationReportTransport for FixtureTransport {
        async fn report_duration(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            binding: &WellearnDurationReportBinding,
            events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<WellearnDurationReportDocuments> {
            let plan = binding.plan();
            assert_eq!(course_id, "1001");
            assert_eq!(sco_id, "301");
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.settings.lock().unwrap() = Some((
                plan.duration_seconds,
                plan.heartbeat_interval_seconds,
                plan.protocol_mode,
            ));
            let after = match self.behavior {
                FixtureBehavior::DriftCompletion => AFTER.replace("incomplete", "completed"),
                FixtureBehavior::UnchangedDuration => BEFORE.to_owned(),
                FixtureBehavior::IncompleteVerification => r#"{"ret":0,"comment":"{}"}"#.to_owned(),
                FixtureBehavior::Normal
                | FixtureBehavior::ExplicitRejections
                | FixtureBehavior::MalformedReceipts => AFTER.to_owned(),
            };
            let accepted = self.behavior != FixtureBehavior::ExplicitRejections;
            let heartbeat_count =
                u32::try_from(1 + plan.duration_seconds / plan.heartbeat_interval_seconds).unwrap();
            let sink = events.mutation_sink().unwrap();
            sink.prepare_sequence_plan(&binding.mutation_sequence_plan(false)?)
                .await?;
            for ordinal in 1..=heartbeat_count + 1 {
                let kind = if ordinal <= heartbeat_count {
                    WellearnDurationMutationKind::PreserveKeep
                } else {
                    WellearnDurationMutationKind::Finalize
                };
                let issue = asterism_provider_api::ExecutionMutationIssue::new(
                    ordinal,
                    kind.as_str(),
                    [u8::try_from(ordinal).unwrap_or(255); 32],
                )?;
                sink.issue(&issue).await?;
                sink.record_receipt(asterism_provider_api::ExecutionMutationReceipt::new(
                    ordinal,
                    [u8::try_from(ordinal + 1).unwrap_or(254); 32],
                    accepted,
                )?)
                .await?;
            }
            Ok(WellearnDurationReportDocuments::with_receipts(
                WellearnCmiDocument::try_new(BEFORE).unwrap(),
                WellearnCmiDocument::try_new(after).unwrap(),
                false,
                heartbeat_count,
                WellearnDurationReportReceipts {
                    start_accepted: None,
                    heartbeat_accepted: if accepted { heartbeat_count } else { 0 },
                    heartbeat_rejected: if accepted { 0 } else { heartbeat_count },
                    final_accepted: (self.behavior != FixtureBehavior::MalformedReceipts)
                        .then_some(accepted),
                },
            ))
        }
    }

    #[derive(Debug, Default)]
    struct FixtureEvents {
        progress: AtomicUsize,
        logs: AtomicUsize,
        prepared: AtomicUsize,
        issues: AtomicUsize,
        receipts: AtomicUsize,
        verifications: AtomicUsize,
        fail_verification: bool,
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

        fn mutation_sink(
            &self,
        ) -> Option<&(dyn asterism_provider_api::ExecutionMutationSink + Send + Sync)> {
            Some(self)
        }
    }

    #[async_trait]
    impl asterism_provider_api::ExecutionMutationSink for FixtureEvents {
        async fn prepare_sequence_plan(
            &self,
            _plan: &ExecutionMutationSequencePlan,
        ) -> ProviderResult<()> {
            self.prepared.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn issue(
            &self,
            _issue: &asterism_provider_api::ExecutionMutationIssue,
        ) -> ProviderResult<()> {
            self.issues.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn record_receipt(
            &self,
            _receipt: asterism_provider_api::ExecutionMutationReceipt,
        ) -> ProviderResult<()> {
            self.receipts.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn record_verification(
            &self,
            _verification: ExecutionMutationVerification,
        ) -> ProviderResult<()> {
            self.verifications.fetch_add(1, Ordering::Relaxed);
            if self.fail_verification {
                Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "fixture verification persistence failed",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn duration_plan_binds_every_protocol_to_its_audited_cadence() {
        for (protocol_mode, heartbeat_interval_seconds) in [
            (WellearnDurationProtocolMode::PreserveFresh, 60),
            (WellearnDurationProtocolMode::ClientCounter, 1),
            (WellearnDurationProtocolMode::ImplicitServer, 60),
        ] {
            WellearnDurationReportPlan {
                duration_seconds: 600,
                heartbeat_interval_seconds,
                protocol_mode,
            }
            .validate()
            .unwrap();
        }

        let valid = WellearnDurationReportPlan {
            duration_seconds: 600,
            heartbeat_interval_seconds: 60,
            protocol_mode: WellearnDurationProtocolMode::PreserveFresh,
        };
        assert!(
            WellearnDurationReportPlan {
                duration_seconds: 0,
                ..valid
            }
            .validate()
            .is_err()
        );
        assert!(
            WellearnDurationReportPlan {
                duration_seconds: MAX_DURATION_REPORT_SECONDS + 1,
                ..valid
            }
            .validate()
            .is_err()
        );
        assert!(
            WellearnDurationReportPlan {
                heartbeat_interval_seconds: 1,
                ..valid
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn singleton_duration_binding_round_trips_and_rejects_identity_or_codec_drift() {
        let context = context();
        let request = request();
        let plan = derive_duration_report_plan(&request).unwrap();
        let binding = WellearnDurationReportBinding::try_new(&context, &request, plan).unwrap();
        let encoded = binding.encode().unwrap();
        let decoded = WellearnDurationReportBinding::decode(
            WELLEARN_DURATION_REPORT_BINDING_TYPE,
            &encoded,
            &context,
            &request,
            plan,
        )
        .unwrap();
        assert_eq!(decoded, binding);
        assert_eq!(decoded.plan(), plan);
        binding.validate_remote_identity("1001", "301").unwrap();
        assert!(binding.validate_remote_identity("1002", "301").is_err());
        assert!(binding.validate_remote_identity("1001", "302").is_err());

        let artifact = binding.to_provider_execution_plan_artifact().unwrap();
        assert_eq!(artifact.provider_id(), &context.provider_id);
        assert_eq!(
            artifact.artifact_type(),
            crate::WELLEARN_SINGLETON_EXECUTION_PLAN_ARTIFACT_TYPE
        );
        assert_eq!(
            artifact.payload_sanitized()["schema"],
            crate::WELLEARN_SINGLETON_EXECUTION_PLAN_ARTIFACT_TYPE
        );
        assert_eq!(
            artifact.payload_sanitized()["binding_digest"]
                .as_array()
                .map(Vec::len),
            Some(32)
        );
        assert_eq!(
            artifact,
            binding.to_provider_execution_plan_artifact().unwrap()
        );
        let mut other_execution = request.clone();
        other_execution.execution_id = ExecutionId::new();
        let other_binding =
            WellearnDurationReportBinding::try_new(&context, &other_execution, plan).unwrap();
        assert_ne!(
            artifact.artifact_digest(),
            other_binding
                .to_provider_execution_plan_artifact()
                .unwrap()
                .artifact_digest()
        );
        let mut request_with_artifact = request.clone();
        request_with_artifact.provider_plan_artifact = Some(artifact.clone());
        assert_eq!(
            WellearnDurationReportBinding::try_new(&context, &request_with_artifact, plan).unwrap(),
            binding
        );

        let debug = format!("{binding:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&request.execution_id.to_string()));
        assert!(!debug.contains(&request.remote_task_id));
        assert!(
            WellearnDurationReportBinding::decode(
                "welearn.foreign.v1",
                &encoded,
                &context,
                &request,
                plan,
            )
            .is_err()
        );

        let mut unknown: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        unknown["unexpected"] = serde_json::json!(true);
        assert!(
            WellearnDurationReportBinding::decode(
                WELLEARN_DURATION_REPORT_BINDING_TYPE,
                &serde_json::to_vec(&unknown).unwrap(),
                &context,
                &request,
                plan,
            )
            .is_err()
        );

        let mut substituted = request.clone();
        substituted.task_id = TaskId::new();
        assert!(
            WellearnDurationReportBinding::decode(
                WELLEARN_DURATION_REPORT_BINDING_TYPE,
                &encoded,
                &context,
                &substituted,
                plan,
            )
            .is_err()
        );

        let mut artifact_drift = request;
        artifact_drift.provider_plan_artifact = Some(
            ProviderExecutionPlanArtifact::try_new(
                context.provider_id.clone(),
                crate::WELLEARN_SINGLETON_EXECUTION_PLAN_ARTIFACT_TYPE,
                serde_json::json!({
                    "schema": crate::WELLEARN_SINGLETON_EXECUTION_PLAN_ARTIFACT_TYPE,
                    "binding_digest": ([7_u8; 32].to_vec()),
                }),
            )
            .unwrap(),
        );
        assert!(WellearnDurationReportBinding::try_new(&context, &artifact_drift, plan).is_err());
    }

    #[test]
    fn singleton_duration_sequence_preserves_each_donor_lifecycle() {
        let context = context();
        let preserve_request = request_for_protocol("preserve_fresh", 120, 60);
        let preserve_plan = derive_duration_report_plan(&preserve_request).unwrap();
        let preserve =
            WellearnDurationReportBinding::try_new(&context, &preserve_request, preserve_plan)
                .unwrap();
        let initialized = preserve.mutation_sequence_plan(false).unwrap();
        assert_eq!(
            initialized.artifact_digest(),
            preserve
                .to_provider_execution_plan_artifact()
                .unwrap()
                .artifact_digest()
        );
        assert_eq!(
            initialized.sequence_type(),
            WELLEARN_DURATION_REPORT_SEQUENCE_TYPE
        );
        assert_eq!(initialized.phases().len(), 2);
        assert_eq!(
            initialized.phases()[0].operation_type(),
            WellearnDurationMutationKind::PreserveKeep.as_str()
        );
        assert_eq!(initialized.phases()[0].minimum_occurrences(), 3);
        assert_eq!(initialized.phases()[0].maximum_occurrences(), 3);
        assert_eq!(
            initialized.phases()[1].operation_type(),
            WellearnDurationMutationKind::Finalize.as_str()
        );
        let uninitialized = preserve.mutation_sequence_plan(true).unwrap();
        assert_eq!(
            initialized.artifact_digest(),
            uninitialized.artifact_digest()
        );
        assert_ne!(initialized.plan_digest(), uninitialized.plan_digest());
        assert_eq!(uninitialized.phases().len(), 3);
        assert_eq!(
            uninitialized.phases()[0].operation_type(),
            WellearnDurationMutationKind::Start.as_str()
        );

        let client_request = request_for_protocol("client_counter", 3, 1);
        let client_plan = derive_duration_report_plan(&client_request).unwrap();
        let client =
            WellearnDurationReportBinding::try_new(&context, &client_request, client_plan).unwrap();
        assert!(client.mutation_sequence_plan(false).is_err());
        let client_sequence = client.mutation_sequence_plan(true).unwrap();
        assert_eq!(client_sequence.phases().len(), 2);
        assert_eq!(
            client_sequence.phases()[0].advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached
        );
        assert_eq!(client_sequence.phases()[1].minimum_occurrences(), 1);
        assert_eq!(client_sequence.phases()[1].maximum_occurrences(), 3);
        assert!(client_sequence.phases()[1].stop_repeating_after_rejection());
        assert_eq!(
            client_sequence.phases()[1].advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached
        );

        let implicit_request = request_for_protocol("implicit_server", 61, 60);
        let implicit_plan = derive_duration_report_plan(&implicit_request).unwrap();
        let implicit =
            WellearnDurationReportBinding::try_new(&context, &implicit_request, implicit_plan)
                .unwrap();
        let implicit_sequence = implicit.mutation_sequence_plan(true).unwrap();
        assert_eq!(implicit_sequence.phases().len(), 3);
        assert_eq!(implicit_sequence.phases()[1].minimum_occurrences(), 1);
        assert_eq!(implicit_sequence.phases()[1].maximum_occurrences(), 1);
        assert_eq!(
            implicit_sequence.phases()[2].operation_type(),
            WellearnDurationMutationKind::Finalize.as_str()
        );
    }

    #[test]
    fn duration_documents_bind_protocol_specific_receipt_slots() {
        let documents = |started, heartbeat_count, receipts| {
            WellearnDurationReportDocuments::with_receipts(
                WellearnCmiDocument::try_new(BEFORE).unwrap(),
                WellearnCmiDocument::try_new(AFTER).unwrap(),
                started,
                heartbeat_count,
                receipts,
            )
        };
        let preserve = WellearnDurationReportPlan {
            duration_seconds: 120,
            heartbeat_interval_seconds: 60,
            protocol_mode: WellearnDurationProtocolMode::PreserveFresh,
        };
        documents(
            false,
            3,
            WellearnDurationReportReceipts {
                start_accepted: None,
                heartbeat_accepted: 2,
                heartbeat_rejected: 1,
                final_accepted: Some(false),
            },
        )
        .validate_for_plan(preserve)
        .unwrap();

        let client_counter = WellearnDurationReportPlan {
            duration_seconds: 10,
            heartbeat_interval_seconds: 1,
            protocol_mode: WellearnDurationProtocolMode::ClientCounter,
        };
        documents(
            true,
            4,
            WellearnDurationReportReceipts {
                start_accepted: Some(true),
                heartbeat_accepted: 3,
                heartbeat_rejected: 1,
                final_accepted: None,
            },
        )
        .validate_for_plan(client_counter)
        .unwrap();
        assert!(
            documents(
                true,
                10,
                WellearnDurationReportReceipts {
                    start_accepted: Some(false),
                    heartbeat_accepted: 10,
                    heartbeat_rejected: 0,
                    final_accepted: None,
                },
            )
            .validate_for_plan(client_counter)
            .is_err()
        );
        let error = documents(
            true,
            4,
            WellearnDurationReportReceipts {
                start_accepted: Some(true),
                heartbeat_accepted: 2,
                heartbeat_rejected: 2,
                final_accepted: None,
            },
        )
        .validate_for_plan(client_counter)
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Internal);

        let implicit = WellearnDurationReportPlan {
            duration_seconds: 120,
            heartbeat_interval_seconds: 60,
            protocol_mode: WellearnDurationProtocolMode::ImplicitServer,
        };
        documents(
            true,
            2,
            WellearnDurationReportReceipts {
                start_accepted: Some(false),
                heartbeat_accepted: 2,
                heartbeat_rejected: 0,
                final_accepted: Some(false),
            },
        )
        .validate_for_plan(implicit)
        .unwrap();
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
        assert_eq!(
            *transport.settings.lock().unwrap(),
            Some((120, 60, WellearnDurationProtocolMode::PreserveFresh))
        );
        assert_eq!(
            outcome.result_sanitized["duration_protocol_mode"],
            "preserve_fresh"
        );
        assert_eq!(outcome.result_sanitized["heartbeat_accepted"], 3);
        assert_eq!(outcome.result_sanitized["heartbeat_rejected"], 0);
        assert_eq!(outcome.result_sanitized["final_accepted"], true);
        assert_eq!(outcome.result_sanitized["donor_request_delay_seconds"], 6);
        assert_eq!(events.progress.load(Ordering::Relaxed), 1);
        assert_eq!(events.logs.load(Ordering::Relaxed), 1);
        assert_eq!(events.prepared.load(Ordering::Relaxed), 1);
        assert_eq!(events.issues.load(Ordering::Relaxed), 4);
        assert_eq!(events.receipts.load(Ordering::Relaxed), 4);
        assert_eq!(events.verifications.load(Ordering::Relaxed), 1);
        assert_eq!(details.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn explicit_negative_legacy_receipts_still_require_fresh_verification() {
        let capability = WellearnDurationReport::try_new(
            Arc::new(FixtureDetail::present()),
            Arc::new(FixtureTransport {
                behavior: FixtureBehavior::ExplicitRejections,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();
        let outcome = capability
            .execute(&context(), &request(), &FixtureEvents::default())
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["heartbeat_accepted"], 0);
        assert_eq!(outcome.result_sanitized["heartbeat_rejected"], 3);
        assert_eq!(outcome.result_sanitized["final_accepted"], false);
    }

    #[tokio::test]
    async fn post_mutation_verification_persistence_failure_requires_human_review() {
        let capability = WellearnDurationReport::try_new(
            Arc::new(FixtureDetail::present()),
            Arc::new(FixtureTransport::default()),
        )
        .unwrap();
        let events = FixtureEvents {
            fail_verification: true,
            ..FixtureEvents::default()
        };
        let error = capability
            .execute(&context(), &request(), &events)
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(events.issues.load(Ordering::Relaxed), 4);
        assert_eq!(events.receipts.load(Ordering::Relaxed), 4);
        assert_eq!(events.verifications.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn duration_receipts_must_match_the_frozen_protocol_shape() {
        let capability = WellearnDurationReport::try_new(
            Arc::new(FixtureDetail::present()),
            Arc::new(FixtureTransport {
                behavior: FixtureBehavior::MalformedReceipts,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();

        let error = capability
            .execute(&context(), &request(), &FixtureEvents::default())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Internal);
    }

    #[tokio::test]
    async fn duration_report_rejects_state_drift_or_missing_time_change() {
        for transport in [
            FixtureTransport {
                behavior: FixtureBehavior::DriftCompletion,
                ..FixtureTransport::default()
            },
            FixtureTransport {
                behavior: FixtureBehavior::UnchangedDuration,
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
                behavior: FixtureBehavior::IncompleteVerification,
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
        request_for_protocol("preserve_fresh", 120, 60)
    }

    fn request_for_protocol(
        protocol_mode: &str,
        duration_seconds: u64,
        heartbeat_interval_seconds: u64,
    ) -> ExecutionRequest {
        let schema = runtime_settings_schema();
        let task = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: std::collections::BTreeMap::from([
                (
                    DURATION_REPORT_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(duration_seconds),
                ),
                (
                    DURATION_HEARTBEAT_INTERVAL_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(heartbeat_interval_seconds),
                ),
                (
                    DURATION_PROTOCOL_MODE_KEY.to_owned(),
                    ProviderSettingValue::Choice(protocol_mode.to_owned()),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::DurationReport],
            capability_plan: vec![TaskCapability::DurationReport],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
            provider_plan_artifact: None,
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
                    ProviderSettingValue::DurationSeconds(60),
                ),
            ]),
        };
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::DurationReport],
            capability_plan: vec![TaskCapability::DurationReport],
            capability_step_position: 1,
            runtime_settings: schema.resolve(None, None, Some(&task)).unwrap(),
            provider_plan_artifact: None,
        }
    }
}
