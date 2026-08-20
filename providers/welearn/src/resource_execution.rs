use std::{fmt, sync::Arc};

use asterism_domain::{
    CourseId, ExecutionId, ProviderAccountId, ProviderId, RemoteState, TaskCapability, TaskId,
};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionMutationSequenceAdvanceCondition, ExecutionMutationSequencePhase,
    ExecutionMutationSequencePlan, ExecutionMutationSequenceRecoverySnapshot,
    ExecutionMutationVerification, ExecutionOutcome, ExecutionRecoveryOutcome, ExecutionRequest,
    ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionLog,
    ProviderExecutionPlanArtifact, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, TaskDetailCapability, TaskExecutionCapability,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    WellearnCmiDocument, WellearnResourceCompletionCmiFormat, WellearnResourceCompletionSequence,
    WellearnResourceCompletionTimeMode,
    cmi::{parse_cmi_snapshot, parse_mutation_cmi_baseline, parse_sco_identity},
    execution_selection::{clamped_gaussian_u8, uniform_u8},
    metadata::development_metadata,
    runtime_settings::{WellearnResourceScore, WellearnRuntimeSettings},
    task_detail::validate_fresh_execution_detail,
};

/// Namespaced credential-free persistence type for one exact singleton
/// `ResourceExecution` mutation authority.
pub const WELLEARN_RESOURCE_EXECUTION_BINDING_TYPE: &str = "welearn.resource-execution-binding.v1";
/// Stable receipt-conditional sequence type derived from that authority.
pub const WELLEARN_RESOURCE_EXECUTION_SEQUENCE_TYPE: &str =
    "welearn.resource-execution-sequence.v1";

const WELLEARN_RESOURCE_EXECUTION_BINDING_VERSION: u16 = 1;
const MAX_RESOURCE_EXECUTION_BINDING_BYTES: usize = 16 * 1_024;
const RESOURCE_EXECUTION_BINDING_DIGEST_DOMAIN: &[u8] =
    b"asterism.welearn.resource-execution-binding.v1\0";
const RESOURCE_EXECUTION_VERIFICATION_DIGEST_DOMAIN: &[u8] =
    b"asterism.welearn.resource-execution-verification.v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WellearnResourceMutationKind {
    Start,
    Set,
    Save,
}

impl WellearnResourceMutationKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "welearn.resource.start",
            Self::Set => "welearn.resource.set",
            Self::Save => "welearn.resource.save",
        }
    }
}

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

    fn validate_for_plan(&self, plan: WellearnResourceExecutionPlan) -> ProviderResult<()> {
        if !self.mutation_submitted {
            if self.after.is_some()
                || self.started
                || self.start_accepted.is_some()
                || self.set_accepted.is_some()
                || !self.save_acceptances.is_empty()
            {
                return Err(invalid_resource_documents());
            }
            return Ok(());
        }

        let expects_set =
            plan.write_mode == crate::WellearnResourceCompletionWriteMode::SetThenSave;
        let expected_saves = match plan.sequence {
            WellearnResourceCompletionSequence::SelectedScore => 1,
            WellearnResourceCompletionSequence::CurrentDonorDualSave100 => 2,
        };
        if self.after.is_none()
            || !self.started
            || self.start_accepted.is_none()
            || self.set_accepted.is_some() != expects_set
            || self.save_acceptances.len() != expected_saves
        {
            return Err(invalid_resource_documents());
        }
        Ok(())
    }
}

fn invalid_resource_documents() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn resource transport returned receipts inconsistent with the frozen plan",
    )
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

    /// Whether this is only the final phase of a donor's one-start atomic
    /// duration-completion operation.
    pub const fn requires_atomic_authority(self) -> bool {
        matches!(
            (
                self.score_percent,
                self.sequence,
                self.time_mode,
                self.write_mode,
                self.mutation_profile,
            ),
            (
                100,
                WellearnResourceCompletionSequence::SelectedScore,
                WellearnResourceCompletionTimeMode::PreserveFreshTime,
                crate::WellearnResourceCompletionWriteMode::SetThenSave,
                crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer,
            ) | (
                0,
                WellearnResourceCompletionSequence::SelectedScore,
                WellearnResourceCompletionTimeMode::ZeroTime,
                crate::WellearnResourceCompletionWriteMode::SaveOnly,
                crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
            )
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnResourceExecutionPlanWire {
    score_percent: u8,
    sequence: String,
    time_mode: String,
    cmi_format: String,
    write_mode: String,
    mutation_profile: String,
}

impl From<WellearnResourceExecutionPlan> for WellearnResourceExecutionPlanWire {
    fn from(plan: WellearnResourceExecutionPlan) -> Self {
        Self {
            score_percent: plan.score_percent,
            sequence: plan.sequence.as_str().to_owned(),
            time_mode: plan.time_mode.as_str().to_owned(),
            cmi_format: plan.cmi_format.as_str().to_owned(),
            write_mode: plan.write_mode.as_str().to_owned(),
            mutation_profile: plan.mutation_profile.as_str().to_owned(),
        }
    }
}

impl TryFrom<WellearnResourceExecutionPlanWire> for WellearnResourceExecutionPlan {
    type Error = ProviderError;

    fn try_from(wire: WellearnResourceExecutionPlanWire) -> Result<Self, Self::Error> {
        use crate::{
            WellearnResourceCompletionCmiFormat::{InteractionInfoSuffix, Json},
            WellearnResourceCompletionSequence::{CurrentDonorDualSave100, SelectedScore},
            WellearnResourceCompletionTimeMode::{PreserveFreshTime, ZeroTime},
            WellearnResourceCompletionWriteMode::{SaveOnly, SetThenSave},
            WellearnResourceMutationProfile::{CurrentFullSimpleReferer, LegacyMinimalTaskReferer},
        };

        let plan = Self {
            score_percent: wire.score_percent,
            sequence: match wire.sequence.as_str() {
                "selected_score" => SelectedScore,
                "current_donor_dual_save_100" => CurrentDonorDualSave100,
                _ => return Err(invalid_resource_execution_binding()),
            },
            time_mode: match wire.time_mode.as_str() {
                "zero_time" => ZeroTime,
                "preserve_fresh_time" => PreserveFreshTime,
                _ => return Err(invalid_resource_execution_binding()),
            },
            cmi_format: match wire.cmi_format.as_str() {
                "json" => Json,
                "interaction_info_suffix" => InteractionInfoSuffix,
                _ => return Err(invalid_resource_execution_binding()),
            },
            write_mode: match wire.write_mode.as_str() {
                "set_then_save" => SetThenSave,
                "save_only" => SaveOnly,
                _ => return Err(invalid_resource_execution_binding()),
            },
            mutation_profile: match wire.mutation_profile.as_str() {
                "current_full_simple_referer" => CurrentFullSimpleReferer,
                "legacy_minimal_task_referer" => LegacyMinimalTaskReferer,
                _ => return Err(invalid_resource_execution_binding()),
            },
        };
        plan.validate()
            .map_err(|_| invalid_resource_execution_binding())?;
        Ok(plan)
    }
}

/// Immutable, credential-free binding between one Core singleton Execution
/// request and the exact donor completion plan derived from its frozen
/// settings.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnResourceExecutionBinding {
    provider_id: ProviderId,
    provider_account_id: ProviderAccountId,
    execution_id: ExecutionId,
    task_id: TaskId,
    course_id: Option<CourseId>,
    remote_task_id: String,
    runtime_settings_schema_version: u32,
    plan: WellearnResourceExecutionPlan,
    execution_plan_artifact: ProviderExecutionPlanArtifact,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnResourceExecutionBindingWire {
    version: u16,
    provider_id: String,
    provider_account_id: ProviderAccountId,
    execution_id: ExecutionId,
    task_id: TaskId,
    course_id: Option<CourseId>,
    remote_task_id: String,
    runtime_settings_schema_version: u32,
    plan: WellearnResourceExecutionPlanWire,
}

impl WellearnResourceExecutionBinding {
    /// Freezes the exact local Execution/Task/account identity and already
    /// resolved donor mutation plan without retaining credentials.
    ///
    /// # Errors
    ///
    /// Rejects Provider/request/capability/settings/plan drift, atomic-only
    /// completion profiles or malformed remote SCO identity.
    pub fn try_new(
        context: &ProviderContext,
        request: &ExecutionRequest,
        plan: WellearnResourceExecutionPlan,
    ) -> ProviderResult<Self> {
        if context.provider_id.as_str() != crate::metadata::PROVIDER_ID
            || !request.has_valid_capability_step()
            || request.requested_capabilities != [TaskCapability::ResourceExecution]
            || plan != derive_resource_execution_plan(request)?
        {
            return Err(invalid_resource_execution_binding());
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

    pub const fn plan(&self) -> WellearnResourceExecutionPlan {
        self.plan
    }

    pub(crate) fn validate_remote_identity(
        &self,
        course_id: &str,
        sco_id: &str,
    ) -> ProviderResult<()> {
        let (bound_course_id, bound_sco_id) = parse_sco_identity(&self.remote_task_id)
            .map_err(|_| invalid_resource_execution_binding())?;
        if bound_course_id != course_id || bound_sco_id != sco_id {
            return Err(invalid_resource_execution_binding());
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

    /// Encodes one bounded, deny-unknown v1 authority. The output contains no
    /// credentials, CMI, route material or mutation receipt.
    ///
    /// # Errors
    ///
    /// Rejects an invalid binding, serialization failure or local size overflow.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate_shape()?;
        let encoded = serde_json::to_vec(&WellearnResourceExecutionBindingWire {
            version: WELLEARN_RESOURCE_EXECUTION_BINDING_VERSION,
            provider_id: self.provider_id.as_str().to_owned(),
            provider_account_id: self.provider_account_id,
            execution_id: self.execution_id,
            task_id: self.task_id,
            course_id: self.course_id,
            remote_task_id: self.remote_task_id.clone(),
            runtime_settings_schema_version: self.runtime_settings_schema_version,
            plan: self.plan.into(),
        })
        .map_err(|_| invalid_resource_execution_binding())?;
        if encoded.is_empty() || encoded.len() > MAX_RESOURCE_EXECUTION_BINDING_BYTES {
            return Err(invalid_resource_execution_binding());
        }
        Ok(encoded)
    }

    /// Decodes only the exact namespaced type and fully recomputes the binding
    /// from an independently loaded context, request and frozen donor plan.
    ///
    /// # Errors
    ///
    /// Rejects foreign type, malformed/unknown/oversized/version-drifted bytes
    /// and every identity, settings or plan substitution.
    pub fn decode(
        binding_type: &str,
        encoded: &[u8],
        context: &ProviderContext,
        request: &ExecutionRequest,
        plan: WellearnResourceExecutionPlan,
    ) -> ProviderResult<Self> {
        if binding_type != WELLEARN_RESOURCE_EXECUTION_BINDING_TYPE
            || encoded.is_empty()
            || encoded.len() > MAX_RESOURCE_EXECUTION_BINDING_BYTES
        {
            return Err(invalid_resource_execution_binding());
        }
        let wire: WellearnResourceExecutionBindingWire =
            serde_json::from_slice(encoded).map_err(|_| invalid_resource_execution_binding())?;
        if wire.version != WELLEARN_RESOURCE_EXECUTION_BINDING_VERSION {
            return Err(invalid_resource_execution_binding());
        }
        let decoded = Self {
            provider_id: ProviderId::new(wire.provider_id)
                .map_err(|_| invalid_resource_execution_binding())?,
            provider_account_id: wire.provider_account_id,
            execution_id: wire.execution_id,
            task_id: wire.task_id,
            course_id: wire.course_id,
            remote_task_id: wire.remote_task_id,
            runtime_settings_schema_version: wire.runtime_settings_schema_version,
            plan: WellearnResourceExecutionPlan::try_from(wire.plan)?,
            execution_plan_artifact:
                crate::execution::singleton_execution_plan_artifact_for_request(context, request)?,
        };
        decoded.validate(context, request, plan)?;
        Ok(decoded)
    }

    /// Recomputes the complete binding from independently frozen values.
    ///
    /// # Errors
    ///
    /// Rejects any field substitution or invalid binding shape.
    pub fn validate(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        plan: WellearnResourceExecutionPlan,
    ) -> ProviderResult<()> {
        self.validate_shape()?;
        if *self != Self::try_new(context, request, plan)? {
            return Err(invalid_resource_execution_binding());
        }
        Ok(())
    }

    /// Builds the exact receipt-conditional sequence to freeze in Core before
    /// the first remote mutation is issued.
    ///
    /// # Errors
    ///
    /// Rejects an invalid binding or generic sequence projection.
    pub fn mutation_sequence_plan(&self) -> ProviderResult<ExecutionMutationSequencePlan> {
        self.validate_shape()?;
        let mut phases = vec![resource_sequence_phase(
            WellearnResourceMutationKind::Start,
            1,
        )?];
        if self.plan.write_mode == crate::WellearnResourceCompletionWriteMode::SetThenSave {
            phases.push(resource_sequence_phase(
                WellearnResourceMutationKind::Set,
                1,
            )?);
        }
        phases.push(resource_sequence_phase(
            WellearnResourceMutationKind::Save,
            match self.plan.sequence {
                WellearnResourceCompletionSequence::SelectedScore => 1,
                WellearnResourceCompletionSequence::CurrentDonorDualSave100 => 2,
            },
        )?);
        ExecutionMutationSequencePlan::try_new(
            self.to_provider_execution_plan_artifact()?
                .artifact_digest(),
            WELLEARN_RESOURCE_EXECUTION_SEQUENCE_TYPE,
            phases,
        )
        .map_err(|_| invalid_resource_execution_binding())
    }

    fn binding_digest(&self) -> ProviderResult<[u8; 32]> {
        let encoded = self.encode()?;
        let mut digest = Sha256::new();
        digest.update(RESOURCE_EXECUTION_BINDING_DIGEST_DOMAIN);
        digest.update(encoded);
        Ok(digest.finalize().into())
    }

    fn validate_shape(&self) -> ProviderResult<()> {
        self.plan
            .validate()
            .map_err(|_| invalid_resource_execution_binding())?;
        if self.provider_id.as_str() != crate::metadata::PROVIDER_ID
            || self.runtime_settings_schema_version == 0
            || self.execution_plan_artifact.provider_id() != &self.provider_id
            || self.execution_plan_artifact.artifact_type()
                != crate::execution::WELLEARN_SINGLETON_EXECUTION_PLAN_ARTIFACT_TYPE
            || self.plan.requires_atomic_authority()
            || self.remote_task_id.is_empty()
            || self.remote_task_id.len() > 512
            || self.remote_task_id.trim() != self.remote_task_id
            || self.remote_task_id.chars().any(char::is_control)
            || parse_sco_identity(&self.remote_task_id).is_err()
        {
            return Err(invalid_resource_execution_binding());
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
            return Err(invalid_resource_execution_binding());
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnResourceExecutionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnResourceExecutionBinding")
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
            .field("execution_plan_artifact", &"[HASHED]")
            .finish()
    }
}

fn resource_sequence_phase(
    kind: WellearnResourceMutationKind,
    occurrences: u32,
) -> ProviderResult<ExecutionMutationSequencePhase> {
    ExecutionMutationSequencePhase::try_new(
        kind.as_str(),
        occurrences,
        occurrences,
        false,
        ExecutionMutationSequenceAdvanceCondition::MaximumReached,
        None,
    )
    .map_err(|_| invalid_resource_execution_binding())
}

fn invalid_resource_execution_binding() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn resource execution binding is invalid or drifted",
    )
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
        binding: &WellearnResourceExecutionBinding,
        events: &(dyn ExecutionEventSink + Send + Sync),
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

    async fn verify_resource_goal(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
    ) -> ProviderResult<(WellearnCmiDocument, ExecutionOutcome)> {
        validate_context(context, &self.metadata)?;
        validate_capability_step(request)?;
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(unsupported(
                "WELearn resource verification accepts only ResourceExecution",
            ));
        }
        let (course_id, sco_id) = parse_sco_identity(&request.remote_task_id)?;
        let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
        let plan = derive_resource_execution_plan(request)?;
        let verified_score_percent = plan.sequence.final_score(plan.score_percent);
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
            .verify_resource(context, &course_id, &sco_id, plan.mutation_profile)
            .await?;
        let snapshot = parse_cmi_snapshot(document.as_str())?;
        verify_completed_preset(&snapshot, verified_score_percent)?;
        let outcome = ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({
                "schema": "welearn.resource-completion-verification.v1",
                "preset": "completed_progress_score",
                "score_percent": verified_score_percent,
                "selected_score_percent": plan.score_percent,
                "verified_score_percent": verified_score_percent,
                "score_mode": score_mode(settings.resource_score),
                "completion_sequence": plan.sequence.as_str(),
                "configured_completion_time_mode": settings.resource_completion_time_mode.as_str(),
                "completion_time_mode": plan.time_mode.as_str(),
                "completion_cmi_format": plan.cmi_format.as_str(),
                "completion_write_mode": plan.write_mode.as_str(),
                "goal_matched": true,
                "verification": "fresh_cmi_no_mutation",
            }),
        };
        Ok((document, outcome))
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
        if plan.requires_atomic_authority() {
            return Err(unsupported(
                "WELearn atomic duration-completion final requires shared atomic authority",
            ));
        }
        let binding = WellearnResourceExecutionBinding::try_new(context, request, plan)?;
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
            .complete_resource(context, &course_id, &sco_id, &binding, events)
            .await?;
        documents.validate_for_plan(plan)?;
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
        let final_save_verification_recorded =
            persist_resource_execution_verification(events, &binding, &documents).await?;

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
                "final_save_verification_recorded": final_save_verification_recorded,
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
        self.verify_resource_goal(context, request)
            .await
            .map(|(_, outcome)| outcome)
    }

    async fn verify_execution_recovery(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        mutation_sequence: Option<&ExecutionMutationSequenceRecoverySnapshot>,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        let plan = derive_resource_execution_plan(request)?;
        let expected_binding = WellearnResourceExecutionBinding::try_new(context, request, plan)?;
        if let Some(snapshot) = mutation_sequence
            && (snapshot.artifact() != &expected_binding.to_provider_execution_plan_artifact()?
                || snapshot.plan() != &expected_binding.mutation_sequence_plan()?)
        {
            return Err(invalid_resource_execution_binding());
        }
        let (document, outcome) = self.verify_resource_goal(context, request).await?;
        let Some(snapshot) = mutation_sequence else {
            return Ok(ExecutionRecoveryOutcome::new(outcome));
        };
        let Some(final_ordinal) = snapshot.final_accepted_mutation_ordinal() else {
            return Ok(ExecutionRecoveryOutcome::new(outcome));
        };
        let persisted = snapshot
            .records()
            .last()
            .and_then(asterism_provider_api::ExecutionMutationRecoveryRecord::verification);
        let verification = persisted.unwrap_or(resource_execution_verification(
            &expected_binding,
            final_ordinal,
            &document,
        )?);
        if verification.ordinal() != final_ordinal || !verification.verified() {
            return Err(invalid_resource_execution_binding());
        }
        Ok(ExecutionRecoveryOutcome::with_mutation_verification(
            outcome,
            verification,
        ))
    }
}

async fn persist_resource_execution_verification(
    events: &(dyn ExecutionEventSink + Send + Sync),
    binding: &WellearnResourceExecutionBinding,
    documents: &WellearnResourceExecutionDocuments,
) -> ProviderResult<bool> {
    if !documents.mutation_submitted || documents.save_acceptances.last() != Some(&true) {
        return Ok(false);
    }
    let after = documents
        .after
        .as_ref()
        .ok_or_else(invalid_resource_execution_binding)?;
    let final_save_ordinal: u32 = match binding.plan.sequence {
        WellearnResourceCompletionSequence::SelectedScore => 3,
        WellearnResourceCompletionSequence::CurrentDonorDualSave100 => 4,
    };
    let verification = resource_execution_verification(binding, final_save_ordinal, after)?;
    let sink = events.mutation_sink().ok_or_else(|| {
        ProviderError::human_required(
            "WELearn resource verification cannot be durably attached to its final mutation",
            asterism_domain::HumanRequiredReason::ManualIntervention,
        )
    })?;
    sink.record_verification(verification).await.map_err(|_| {
        ProviderError::human_required(
            "WELearn resource verification persistence failed after mutation",
            asterism_domain::HumanRequiredReason::ManualIntervention,
        )
    })?;
    Ok(true)
}

fn resource_execution_verification(
    binding: &WellearnResourceExecutionBinding,
    ordinal: u32,
    after: &WellearnCmiDocument,
) -> ProviderResult<ExecutionMutationVerification> {
    let mut digest = Sha256::new();
    digest.update(RESOURCE_EXECUTION_VERIFICATION_DIGEST_DOMAIN);
    digest.update(binding.binding_digest()?);
    digest.update(ordinal.to_be_bytes());
    digest.update(after.as_str().as_bytes());
    ExecutionMutationVerification::new(ordinal, digest.finalize().into(), true)
        .map_err(|_| invalid_resource_execution_binding())
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

pub(crate) fn derive_resource_execution_plan(
    request: &ExecutionRequest,
) -> ProviderResult<WellearnResourceExecutionPlan> {
    let settings = WellearnRuntimeSettings::resolve(&request.runtime_settings)?;
    let plan = WellearnResourceExecutionPlan {
        score_percent: select_score(settings.resource_score, request),
        sequence: settings.resource_completion_sequence,
        time_mode: effective_completion_time_mode(settings.resource_completion_time_mode, request),
        cmi_format: settings.resource_completion_cmi_format,
        write_mode: settings.resource_completion_write_mode,
        mutation_profile: settings.resource_mutation_profile,
    };
    plan.validate()?;
    Ok(plan)
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
        ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
        ExecutionMutationSink, ProviderRuntimeSettingsPatch, ProviderSettingValue, RemoteTask,
        RemoteTaskDetail,
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
    enum FixtureReceiptShape {
        #[default]
        Exact,
        Malformed,
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<CompletionCall>>,
        verifications: Mutex<Vec<(String, String)>>,
        already_completed: bool,
        drift_score: bool,
        explicit_rejections: bool,
        receipt_shape: FixtureReceiptShape,
        baseline: FixtureBaseline,
    }

    #[async_trait]
    impl WellearnResourceExecutionTransport for FixtureTransport {
        async fn complete_resource(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            binding: &WellearnResourceExecutionBinding,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<WellearnResourceExecutionDocuments> {
            let plan = binding.plan();
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
            let malformed_receipts = matches!(self.receipt_shape, FixtureReceiptShape::Malformed);
            Ok(WellearnResourceExecutionDocuments::submitted(
                WellearnCmiDocument::try_new(before).unwrap(),
                WellearnCmiDocument::try_new(after).unwrap(),
                !malformed_receipts,
                accepted,
                if malformed_receipts {
                    None
                } else {
                    set_accepted
                },
                if malformed_receipts {
                    Vec::new()
                } else {
                    vec![accepted; save_count]
                },
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
    impl ExecutionMutationSink for FixtureEvents {
        async fn issue(&self, _issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            Ok(())
        }

        async fn record_receipt(&self, _receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            Ok(())
        }

        async fn record_verification(
            &self,
            _verification: ExecutionMutationVerification,
        ) -> ProviderResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl ExecutionEventSink for FixtureEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            Ok(())
        }

        async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
            Ok(())
        }

        fn mutation_sink(&self) -> Option<&(dyn ExecutionMutationSink + Send + Sync)> {
            Some(self)
        }
    }

    #[derive(Debug, Default)]
    struct RecordingFixtureEvents {
        verifications: Mutex<Vec<ExecutionMutationVerification>>,
        fail_verification: bool,
    }

    #[async_trait]
    impl ExecutionMutationSink for RecordingFixtureEvents {
        async fn issue(&self, _issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            Ok(())
        }

        async fn record_receipt(&self, _receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            Ok(())
        }

        async fn record_verification(
            &self,
            verification: ExecutionMutationVerification,
        ) -> ProviderResult<()> {
            if self.fail_verification {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "fixture verification persistence failure",
                ));
            }
            self.verifications.lock().unwrap().push(verification);
            Ok(())
        }
    }

    #[async_trait]
    impl ExecutionEventSink for RecordingFixtureEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            Ok(())
        }

        async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
            Ok(())
        }

        fn mutation_sink(&self) -> Option<&(dyn ExecutionMutationSink + Send + Sync)> {
            Some(self)
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
        assert!(!current_completion.requires_atomic_authority());
        assert!(current_duration_final.requires_atomic_authority());
        assert!(!legacy_completion.requires_atomic_authority());
        assert!(auto_duration_final.requires_atomic_authority());

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

    #[test]
    fn singleton_resource_binding_round_trips_and_freezes_exact_sequence() {
        let context = context();
        let request = request();
        let plan = legacy_resource_plan();
        let binding = WellearnResourceExecutionBinding::try_new(&context, &request, plan).unwrap();
        let encoded = binding.encode().unwrap();
        let decoded = WellearnResourceExecutionBinding::decode(
            WELLEARN_RESOURCE_EXECUTION_BINDING_TYPE,
            &encoded,
            &context,
            &request,
            plan,
        )
        .unwrap();
        assert_eq!(decoded, binding);
        binding.validate_remote_identity("1001", "301").unwrap();
        assert!(binding.validate_remote_identity("1002", "301").is_err());
        assert!(binding.validate_remote_identity("1001", "302").is_err());
        assert!(!String::from_utf8(encoded).unwrap().contains("credential"));

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
            WellearnResourceExecutionBinding::try_new(&context, &other_execution, plan).unwrap();
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
            WellearnResourceExecutionBinding::try_new(&context, &request_with_artifact, plan)
                .unwrap(),
            binding
        );

        let sequence = binding.mutation_sequence_plan().unwrap();
        assert_eq!(
            sequence.sequence_type(),
            WELLEARN_RESOURCE_EXECUTION_SEQUENCE_TYPE
        );
        assert_eq!(sequence.artifact_digest(), artifact.artifact_digest());
        assert_eq!(sequence.phases().len(), 3);
        assert_eq!(
            sequence.phases()[0].operation_type(),
            WellearnResourceMutationKind::Start.as_str()
        );
        assert_eq!(
            sequence.phases()[1].operation_type(),
            WellearnResourceMutationKind::Set.as_str()
        );
        assert_eq!(
            sequence.phases()[2].operation_type(),
            WellearnResourceMutationKind::Save.as_str()
        );
        assert_eq!(sequence.phases()[2].maximum_occurrences(), 1);

        let debug = format!("{binding:?}");
        for identity in [
            context.account_id.to_string(),
            request.execution_id.to_string(),
            request.task_id.to_string(),
            request.remote_task_id,
        ] {
            assert!(!debug.contains(&identity));
        }
    }

    #[test]
    fn singleton_resource_binding_rejects_codec_and_identity_drift() {
        let context = context();
        let request = request();
        let plan = legacy_resource_plan();
        let binding = WellearnResourceExecutionBinding::try_new(&context, &request, plan).unwrap();
        let encoded = binding.encode().unwrap();
        let original: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let rejects = |value: &serde_json::Value| {
            WellearnResourceExecutionBinding::decode(
                WELLEARN_RESOURCE_EXECUTION_BINDING_TYPE,
                &serde_json::to_vec(value).unwrap(),
                &context,
                &request,
                plan,
            )
            .is_err()
        };

        let mut unknown = original.clone();
        unknown["credential_refs"] = serde_json::json!(["forbidden"]);
        assert!(rejects(&unknown));
        let mut nested_unknown = original.clone();
        nested_unknown["plan"]["cookie"] = serde_json::json!("forbidden");
        assert!(rejects(&nested_unknown));
        let mut version = original.clone();
        version["version"] = serde_json::json!(2);
        assert!(rejects(&version));
        let mut execution = original.clone();
        execution["execution_id"] = serde_json::json!(ExecutionId::new());
        assert!(rejects(&execution));
        let mut score = original;
        score["plan"]["score_percent"] = serde_json::json!(81);
        assert!(rejects(&score));

        let mut artifact_drift = request.clone();
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
        assert!(
            WellearnResourceExecutionBinding::try_new(&context, &artifact_drift, plan).is_err()
        );

        assert!(
            WellearnResourceExecutionBinding::decode(
                "welearn.foreign.v1",
                &encoded,
                &context,
                &request,
                plan,
            )
            .is_err()
        );
        assert!(
            WellearnResourceExecutionBinding::decode(
                WELLEARN_RESOURCE_EXECUTION_BINDING_TYPE,
                &vec![b'x'; MAX_RESOURCE_EXECUTION_BINDING_BYTES + 1],
                &context,
                &request,
                plan,
            )
            .is_err()
        );
    }

    #[test]
    fn singleton_resource_sequence_preserves_dual_save_and_rejects_atomic_final() {
        let context = context();
        let request = current_donor_request();
        let current = current_resource_plan();
        let binding =
            WellearnResourceExecutionBinding::try_new(&context, &request, current).unwrap();
        let sequence = binding.mutation_sequence_plan().unwrap();
        assert_eq!(sequence.phases().len(), 3);
        assert_eq!(sequence.phases()[2].minimum_occurrences(), 2);
        assert_eq!(sequence.phases()[2].maximum_occurrences(), 2);

        let mut atomic = current;
        atomic.sequence = WellearnResourceCompletionSequence::SelectedScore;
        atomic.score_percent = 100;
        atomic.time_mode = WellearnResourceCompletionTimeMode::PreserveFreshTime;
        assert!(atomic.validate().is_ok());
        assert!(WellearnResourceExecutionBinding::try_new(&context, &request, atomic).is_err());
    }

    #[tokio::test]
    async fn resource_execution_binds_fresh_detail_and_verifies_exact_cmi_goal() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let events = RecordingFixtureEvents::default();
        let outcome = execution
            .execute(&context(), &request(), &events)
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["score_percent"], 82);
        assert_eq!(
            outcome.result_sanitized["final_save_verification_recorded"],
            true
        );
        let verifications = events.verifications.lock().unwrap();
        assert_eq!(verifications.len(), 1);
        assert_eq!(verifications[0].ordinal(), 3);
        assert!(verifications[0].verified());
        assert_ne!(verifications[0].observation_digest(), [0; 32]);
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
    async fn singleton_resource_recovery_consumes_the_same_attempt_sequence_without_replay() {
        let context = context();
        let request = request();
        let binding = WellearnResourceExecutionBinding::try_new(
            &context,
            &request,
            derive_resource_execution_plan(&request).unwrap(),
        )
        .unwrap();
        let plan = binding.mutation_sequence_plan().unwrap();
        let snapshot = completed_recovery_snapshot(
            binding.to_provider_execution_plan_artifact().unwrap(),
            plan,
            None,
        );
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();

        let recovered = execution
            .verify_execution_recovery(&context, &request, Some(&snapshot))
            .await
            .unwrap();

        assert!(recovered.outcome().verified);
        let verification = recovered.mutation_verification().unwrap();
        assert_eq!(
            Some(verification.ordinal()),
            snapshot.final_accepted_mutation_ordinal()
        );
        assert!(verification.verified());
        assert!(transport.calls.lock().unwrap().is_empty());
        assert_eq!(transport.verifications.lock().unwrap().len(), 1);
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
        let events = RecordingFixtureEvents::default();
        let outcome = execution
            .execute(&context(), &request(), &events)
            .await
            .unwrap();

        assert_eq!(outcome.result_sanitized["already_completed"], true);
        assert_eq!(outcome.result_sanitized["mutation_submitted"], false);
        assert_eq!(
            outcome.result_sanitized["final_save_verification_recorded"],
            false
        );
        assert!(events.verifications.lock().unwrap().is_empty());
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
        let events = RecordingFixtureEvents::default();
        let outcome = execution
            .execute(&context(), &request(), &events)
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
        assert_eq!(
            outcome.result_sanitized["final_save_verification_recorded"],
            false
        );
        assert!(events.verifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_mutation_verification_persistence_failure_requires_human_review() {
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            Arc::new(FixtureTransport::default()),
        )
        .unwrap();
        let events = RecordingFixtureEvents {
            verifications: Mutex::new(Vec::new()),
            fail_verification: true,
        };

        let error = execution
            .execute(&context(), &request(), &events)
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::HumanRequired);
        assert!(events.verifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn transport_receipts_must_match_the_frozen_mutation_shape() {
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            Arc::new(FixtureTransport {
                receipt_shape: FixtureReceiptShape::Malformed,
                ..FixtureTransport::default()
            }),
        )
        .unwrap();

        let error = execution
            .execute(&context(), &current_donor_request(), &FixtureEvents)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Internal);
    }

    #[tokio::test]
    async fn save_only_atomic_final_requires_shared_authority() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let error = execution
            .execute(&context(), &save_only_request(), &FixtureEvents)
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
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
    async fn atomic_final_remains_available_to_read_only_recovery_verification() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();

        let error = execution
            .verify_execution(&context(), &save_only_request())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
        assert!(transport.calls.lock().unwrap().is_empty());
        assert_eq!(transport.verifications.lock().unwrap().len(), 1);
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
    async fn fresh_time_atomic_final_requires_shared_authority() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let error = execution
            .execute(&context(), &preserved_time_request(), &FixtureEvents)
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn auto_time_atomic_step_context_still_requires_shared_authority() {
        let transport = Arc::new(FixtureTransport::default());
        let execution = WellearnResourceExecution::try_new(
            Arc::new(FixtureDetail { visible: true }),
            transport.clone(),
        )
        .unwrap();
        let error = execution
            .execute(&context(), &composite_resource_request(), &FixtureEvents)
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(transport.calls.lock().unwrap().is_empty());
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

    fn legacy_resource_plan() -> WellearnResourceExecutionPlan {
        WellearnResourceExecutionPlan {
            score_percent: 82,
            sequence: WellearnResourceCompletionSequence::SelectedScore,
            time_mode: WellearnResourceCompletionTimeMode::ZeroTime,
            cmi_format: WellearnResourceCompletionCmiFormat::InteractionInfoSuffix,
            write_mode: crate::WellearnResourceCompletionWriteMode::SetThenSave,
            mutation_profile: crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
        }
    }

    fn current_resource_plan() -> WellearnResourceExecutionPlan {
        WellearnResourceExecutionPlan {
            score_percent: 82,
            sequence: WellearnResourceCompletionSequence::CurrentDonorDualSave100,
            time_mode: WellearnResourceCompletionTimeMode::ZeroTime,
            cmi_format: WellearnResourceCompletionCmiFormat::Json,
            write_mode: crate::WellearnResourceCompletionWriteMode::SetThenSave,
            mutation_profile: crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer,
        }
    }

    fn completed_recovery_snapshot(
        artifact: ProviderExecutionPlanArtifact,
        plan: ExecutionMutationSequencePlan,
        verification: Option<ExecutionMutationVerification>,
    ) -> ExecutionMutationSequenceRecoverySnapshot {
        let final_ordinal = plan
            .phases()
            .iter()
            .map(ExecutionMutationSequencePhase::maximum_occurrences)
            .sum::<u32>();
        let mut ordinal = 1_u32;
        let mut records = Vec::new();
        for phase in plan.phases() {
            for _ in 0..phase.maximum_occurrences() {
                let issue = ExecutionMutationIssue::new(
                    ordinal,
                    phase.operation_type(),
                    [u8::try_from(ordinal).unwrap_or(255); 32],
                )
                .unwrap();
                let receipt = ExecutionMutationReceipt::new(
                    ordinal,
                    [u8::try_from(ordinal + 1).unwrap_or(254); 32],
                    true,
                )
                .unwrap();
                records.push(
                    ExecutionMutationRecoveryRecord::try_new(
                        issue,
                        Some(receipt),
                        (ordinal == final_ordinal).then_some(verification).flatten(),
                    )
                    .unwrap(),
                );
                ordinal += 1;
            }
        }
        ExecutionMutationSequenceRecoverySnapshot::try_new(artifact, plan, records, Vec::new())
            .unwrap()
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
            provider_plan_artifact: None,
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
            provider_plan_artifact: None,
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
            provider_plan_artifact: None,
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
            provider_plan_artifact: None,
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
            provider_plan_artifact: None,
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
            provider_plan_artifact: None,
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
            provider_plan_artifact: None,
        }
    }
}
