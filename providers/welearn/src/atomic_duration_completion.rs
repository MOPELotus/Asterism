use std::{fmt, sync::Arc};

use asterism_domain::{HumanRequiredReason, RemoteState};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionMutationVerification, ExecutionOutcome, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact, ProviderIdentity,
    ProviderMetadata, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;

use crate::{
    WellearnAtomicChildPlan, WellearnAtomicCompletionProfile, WellearnCmiDocument,
    WellearnDurationProtocolMode, WellearnPreparedAtomicChildPlan,
    WellearnResourceCompletionCmiFormat, WellearnResourceCompletionSequence,
    WellearnResourceCompletionTimeMode, WellearnResourceCompletionWriteMode,
    WellearnResourceExecutionPlan, WellearnResourceMutationProfile,
    atomic_mutation_digest::atomic_completion_observation_digest,
    build_atomic_mutation_sequence_plan, cmi::parse_mutation_cmi_baseline,
    metadata::development_metadata, parse_cmi_snapshot,
    runtime_settings::MAX_DURATION_REPORT_SECONDS,
};

/// Stable Provider operation type for one remote mutation inside the atomic
/// `WELearn` lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnAtomicMutationKind {
    Start,
    CounterKeep,
    ImplicitKeep,
    Set,
    Save,
}

impl WellearnAtomicMutationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "welearn.atomic.start",
            Self::CounterKeep => "welearn.atomic.keep_counter",
            Self::ImplicitKeep => "welearn.atomic.keep_implicit",
            Self::Set => "welearn.atomic.set",
            Self::Save => "welearn.atomic.save",
        }
    }
}

/// Sanitized proof returned after exact fresh CMI verification of an atomic
/// duration-completion result.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct WellearnAtomicDurationCompletionVerification {
    pub(crate) profile: WellearnAtomicCompletionProfile,
    pub(crate) score_percent: u8,
    pub(crate) time_preservation_verified: Option<bool>,
    pub(crate) final_save_ordinal: u32,
    pub(crate) final_save_accepted: bool,
    pub(crate) observation_digest: [u8; 32],
}

impl WellearnAtomicDurationCompletionVerification {
    pub const fn profile(self) -> WellearnAtomicCompletionProfile {
        self.profile
    }

    pub const fn score_percent(self) -> u8 {
        self.score_percent
    }

    pub const fn time_preservation_verified(self) -> Option<bool> {
        self.time_preservation_verified
    }

    pub const fn final_save_ordinal(self) -> u32 {
        self.final_save_ordinal
    }

    pub const fn final_save_accepted(self) -> bool {
        self.final_save_accepted
    }

    pub const fn observation_digest(self) -> [u8; 32] {
        self.observation_digest
    }

    /// Adapts an accepted final-save proof to Core's generic durable value.
    /// An explicit negative final receipt remains diagnostic and therefore
    /// produces no verification record.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the Provider proof cannot satisfy Core's
    /// bounded ordinal/digest contract.
    pub fn to_execution_mutation_verification(
        self,
    ) -> ProviderResult<Option<ExecutionMutationVerification>> {
        if !self.final_save_accepted {
            return Ok(None);
        }
        ExecutionMutationVerification::new(
            self.final_save_ordinal,
            self.observation_digest,
            true,
        )
        .map(Some)
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn atomic completion verification cannot satisfy the Core record contract",
            )
        })
    }
}

impl fmt::Debug for WellearnAtomicDurationCompletionVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnAtomicDurationCompletionVerification")
            .field("profile", &self.profile)
            .field("score_percent", &self.score_percent)
            .field(
                "time_preservation_verified",
                &self.time_preservation_verified,
            )
            .field("final_save_ordinal", &self.final_save_ordinal)
            .field("final_save_accepted", &self.final_save_accepted)
            .field("observation_digest", &"[HASHED]")
            .finish()
    }
}

/// Verifies one completed atomic lifecycle from its independent fresh CMI
/// evidence without entering any mutation or transport path.
///
/// # Errors
///
/// Returns a protocol error for malformed CMI and `RemoteChanged` when the
/// exact completion/progress/score goal or current-donor time preservation is
/// not visible.
pub fn verify_atomic_duration_completion(
    plan: WellearnAtomicDurationCompletionPlan,
    documents: &WellearnAtomicDurationCompletionDocuments,
) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
    documents.validate_for_plan(plan)?;
    let final_snapshot = parse_cmi_snapshot(documents.after_completion().as_str())?;
    verify_atomic_final_snapshot(plan, &final_snapshot)?;
    let score_percent = plan.completion().score_percent;

    let time_preservation_verified = match plan.profile() {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            let after_duration = documents
                .after_duration()
                .ok_or_else(invalid_atomic_documents)?;
            let duration_snapshot = parse_cmi_snapshot(after_duration.as_str())?;
            if !duration_snapshot.cmi_present()
                || duration_snapshot.session_time_raw().is_none()
                || duration_snapshot.total_time_raw().is_none()
                || duration_snapshot.session_time_raw() != final_snapshot.session_time_raw()
                || duration_snapshot.total_time_raw() != final_snapshot.total_time_raw()
            {
                return Err(atomic_goal_changed());
            }
            Some(true)
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => None,
    };
    let final_save_ordinal = documents.final_save_ordinal(plan)?;
    let observation_digest =
        atomic_completion_observation_digest(plan, documents, final_save_ordinal)?;
    Ok(WellearnAtomicDurationCompletionVerification {
        profile: plan.profile(),
        score_percent,
        time_preservation_verified,
        final_save_ordinal,
        final_save_accepted: documents.receipts().save_accepted(),
        observation_digest,
    })
}

pub(crate) fn verify_atomic_final_snapshot(
    plan: WellearnAtomicDurationCompletionPlan,
    snapshot: &crate::WellearnCmiSnapshot,
) -> ProviderResult<()> {
    plan.validate()?;
    let expected_score = plan.completion().score_percent.to_string();
    if !snapshot.cmi_present()
        || snapshot.remote_state() != RemoteState::Completed
        || snapshot.percent() != Some(100)
        || snapshot.score_scaled_raw() != Some(expected_score.as_str())
    {
        return Err(atomic_goal_changed());
    }
    Ok(())
}

pub(crate) fn atomic_goal_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "WELearn atomic duration-completion goal was not visible in fresh CMI",
    )
}

/// Coordinates one already-planned atomic child through fresh Task rebinding,
/// one Provider transport call and exact final verification.
///
/// This boundary does not build parent selection, schedule children or grant
/// Core mutation authority. It remains unregistered until the shared parent
/// batch contract can supply a persisted [`WellearnPreparedAtomicChildPlan`].
pub struct WellearnAtomicDurationCompletion {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn WellearnAtomicDurationCompletionTransport>,
}

impl WellearnAtomicDurationCompletion {
    /// Builds the high-level atomic child coordinator.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the fresh-detail implementation belongs
    /// to another Provider contract.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn WellearnAtomicDurationCompletionTransport>,
    ) -> ProviderResult<Self> {
        let metadata = development_metadata()?;
        if details.metadata() != &metadata {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn atomic execution detail boundary has mismatched metadata",
            ));
        }
        Ok(Self {
            metadata,
            details,
            transport,
        })
    }

    /// Executes one exact Core-prepared atomic child.
    ///
    /// # Errors
    ///
    /// Returns a typed error for Provider/context drift, invalid parent/child
    /// planning evidence, fresh Task drift, transport ambiguity or failed exact
    /// completion/time verification.
    pub async fn execute_prepared(
        &self,
        context: &ProviderContext,
        prepared: &WellearnPreparedAtomicChildPlan,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn atomic execution received a foreign Provider context",
            ));
        }
        prepared.validate()?;
        let child = prepared.child_plan();
        let plan = child.duration_completion_plan()?;
        let detail = self
            .details
            .task_detail(context, child.remote_task_id())
            .await?;
        prepared.validate_fresh_detail(&detail)?;
        let artifact = prepared.provider_plan_artifact()?;
        let sequence_plan = build_atomic_mutation_sequence_plan(child, &artifact)?;
        let sink = events.mutation_sink().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn atomic execution requires a durable Core mutation sink",
            )
        })?;
        sink.prepare_sequence_plan(&sequence_plan).await?;
        let documents = self
            .transport
            .complete_duration_atomically(context, child, events)
            .await?;
        let verification = verify_atomic_duration_completion(plan, &documents)?;
        let final_save_verification_recorded =
            persist_atomic_completion_verification(events, verification).await?;
        let receipts = documents.receipts();
        let heartbeat_accepted = receipts
            .heartbeat_acceptances()
            .iter()
            .filter(|accepted| **accepted)
            .count();
        let heartbeat_rejected = receipts.heartbeat_acceptances().len() - heartbeat_accepted;
        Ok(ExecutionOutcome {
            remote_state: RemoteState::Completed,
            verified: true,
            result_sanitized: serde_json::json!({
                "schema": "welearn.atomic-duration-completion.v1",
                "profile": atomic_profile_name(verification.profile()),
                "target_seconds": plan.target_seconds(),
                "score_percent": verification.score_percent(),
                "time_preservation_verified": verification.time_preservation_verified(),
                "start_accepted": receipts.start_accepted(),
                "heartbeat_count": receipts.heartbeat_acceptances().len(),
                "heartbeat_accepted": heartbeat_accepted,
                "heartbeat_rejected": heartbeat_rejected,
                "set_accepted": receipts.set_accepted(),
                "save_accepted": receipts.save_accepted(),
                "final_save_ordinal": verification.final_save_ordinal(),
                "final_save_verification_recorded": final_save_verification_recorded,
                "verification": "provider_fresh_cmi",
            }),
        })
    }

    /// Restores the complete Provider-private parent, batch and child facts,
    /// then enters the same fresh-rebind and atomic execution path.
    ///
    /// # Errors
    ///
    /// Returns a typed error for any durable-artifact, fresh-detail, sequence,
    /// transport, final-goal or verification-persistence failure.
    pub async fn execute_durable_artifacts(
        &self,
        context: &ProviderContext,
        encoded_parent_authority: &[u8],
        encoded_batch_snapshot: &[u8],
        child_artifact: &ProviderExecutionPlanArtifact,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        let prepared = WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
            encoded_parent_authority,
            encoded_batch_snapshot,
            child_artifact,
        )?;
        self.execute_prepared(context, &prepared, events).await
    }
}

async fn persist_atomic_completion_verification(
    events: &(dyn ExecutionEventSink + Send + Sync),
    verification: WellearnAtomicDurationCompletionVerification,
) -> ProviderResult<bool> {
    let Some(record) = verification
        .to_execution_mutation_verification()
        .map_err(|_| atomic_verification_persistence_error())?
    else {
        return Ok(false);
    };
    let sink = events
        .mutation_sink()
        .ok_or_else(atomic_verification_persistence_error)?;
    sink.record_verification(record)
        .await
        .map_err(|_| atomic_verification_persistence_error())?;
    Ok(true)
}

fn atomic_verification_persistence_error() -> ProviderError {
    ProviderError::human_required(
        "WELearn atomic completion was verified but its durable observation was not recorded",
        HumanRequiredReason::ManualIntervention,
    )
}

impl fmt::Debug for WellearnAtomicDurationCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnAtomicDurationCompletion")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnAtomicDurationCompletion {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

const fn atomic_profile_name(profile: WellearnAtomicCompletionProfile) -> &'static str {
    match profile {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            "fanyuchang_fresh_set_save_100"
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => "auto_zero_time_save_only_0",
    }
}

/// Ordered mutation receipts emitted by one authorized atomic lifecycle.
/// Explicit rejection remains diagnostic; only fresh CMI verification can
/// prove the final goal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnAtomicDurationCompletionReceipts {
    start_accepted: bool,
    heartbeat_acceptances: Vec<bool>,
    set_accepted: Option<bool>,
    save_accepted: bool,
}

impl WellearnAtomicDurationCompletionReceipts {
    pub const fn new(
        start_accepted: bool,
        heartbeat_acceptances: Vec<bool>,
        set_accepted: Option<bool>,
        save_accepted: bool,
    ) -> Self {
        Self {
            start_accepted,
            heartbeat_acceptances,
            set_accepted,
            save_accepted,
        }
    }

    pub const fn start_accepted(&self) -> bool {
        self.start_accepted
    }

    pub fn heartbeat_acceptances(&self) -> &[bool] {
        &self.heartbeat_acceptances
    }

    pub const fn set_accepted(&self) -> Option<bool> {
        self.set_accepted
    }

    pub const fn save_accepted(&self) -> bool {
        self.save_accepted
    }

    pub(crate) fn validate_for_plan(
        &self,
        plan: WellearnAtomicDurationCompletionPlan,
        has_after_duration_evidence: bool,
    ) -> ProviderResult<()> {
        plan.validate()?;
        let valid = match plan.profile() {
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
                has_after_duration_evidence
                    && self.start_accepted
                    && self.set_accepted.is_some()
                    && valid_client_counter_receipts(
                        &self.heartbeat_acceptances,
                        plan.target_seconds(),
                    )
            }
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => {
                let expected =
                    usize::try_from(plan.target_seconds() / plan.heartbeat_interval_seconds())
                        .map_err(|_| invalid_atomic_documents())?;
                !has_after_duration_evidence
                    && self.set_accepted.is_none()
                    && self.heartbeat_acceptances.len() == expected
            }
        };
        if !valid {
            return Err(invalid_atomic_documents());
        }
        Ok(())
    }

    pub(crate) fn final_save_ordinal(
        &self,
        plan: WellearnAtomicDurationCompletionPlan,
    ) -> ProviderResult<u32> {
        let has_after_duration = matches!(
            plan.profile(),
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100
        );
        self.validate_for_plan(plan, has_after_duration)?;
        let heartbeat_count = u32::try_from(self.heartbeat_acceptances.len())
            .map_err(|_| invalid_atomic_documents())?;
        2_u32
            .checked_add(heartbeat_count)
            .and_then(|ordinal| ordinal.checked_add(u32::from(self.set_accepted.is_some())))
            .filter(|ordinal| *ordinal <= 100_000)
            .ok_or_else(invalid_atomic_documents)
    }
}

/// Independent CMI evidence and mutation receipts returned by one complete
/// atomic duration-completion lifecycle.
///
/// This value does not grant execution authority. The intermediate
/// `after_duration` document exists only for current Fanyuchang, whose final
/// completion CMI must carry fresh time fields from the same operation.
#[derive(Debug)]
pub struct WellearnAtomicDurationCompletionDocuments {
    initial: WellearnCmiDocument,
    after_duration: Option<WellearnCmiDocument>,
    after_completion: WellearnCmiDocument,
    receipts: WellearnAtomicDurationCompletionReceipts,
}

impl WellearnAtomicDurationCompletionDocuments {
    /// Builds and validates a complete evidence bundle against the authorized
    /// Provider wire plan.
    ///
    /// # Errors
    ///
    /// Returns a typed protocol error for an unknown initial CMI document, or
    /// an internal error when the document slots or ordered mutation receipts
    /// cannot have been produced by the selected donor lifecycle.
    pub fn try_new(
        plan: WellearnAtomicDurationCompletionPlan,
        initial: WellearnCmiDocument,
        after_duration: Option<WellearnCmiDocument>,
        after_completion: WellearnCmiDocument,
        receipts: WellearnAtomicDurationCompletionReceipts,
    ) -> ProviderResult<Self> {
        let documents = Self {
            initial,
            after_duration,
            after_completion,
            receipts,
        };
        documents.validate_for_plan(plan)?;
        Ok(documents)
    }

    pub const fn initial(&self) -> &WellearnCmiDocument {
        &self.initial
    }

    pub const fn after_duration(&self) -> Option<&WellearnCmiDocument> {
        self.after_duration.as_ref()
    }

    pub const fn after_completion(&self) -> &WellearnCmiDocument {
        &self.after_completion
    }

    pub const fn receipts(&self) -> &WellearnAtomicDurationCompletionReceipts {
        &self.receipts
    }

    /// Returns the exact ordinal issued for the final completion-bearing save.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the frozen lifecycle or ordinal count is
    /// invalid.
    pub fn final_save_ordinal(
        &self,
        plan: WellearnAtomicDurationCompletionPlan,
    ) -> ProviderResult<u32> {
        self.validate_for_plan(plan)?;
        self.receipts.final_save_ordinal(plan)
    }

    /// Revalidates an evidence bundle before parsing its CMI documents.
    ///
    /// # Errors
    ///
    /// Returns an internal error for a plan/document shape mismatch.
    pub fn validate_for_plan(
        &self,
        plan: WellearnAtomicDurationCompletionPlan,
    ) -> ProviderResult<()> {
        plan.validate()?;
        validate_atomic_initial_cmi(&self.initial)?;
        self.receipts
            .validate_for_plan(plan, self.after_duration.is_some())
    }
}

pub(crate) fn validate_atomic_initial_cmi(document: &WellearnCmiDocument) -> ProviderResult<()> {
    parse_mutation_cmi_baseline(document.as_str()).map(|_| ())
}

fn valid_client_counter_receipts(receipts: &[bool], target_seconds: u64) -> bool {
    let Ok(expected) = usize::try_from(target_seconds) else {
        return false;
    };
    if receipts.len() > expected {
        return false;
    }
    match receipts.iter().position(|accepted| !accepted) {
        Some(rejected) => rejected + 1 == receipts.len(),
        None => receipts.len() == expected,
    }
}

fn invalid_atomic_documents() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic duration-completion documents do not match the frozen lifecycle",
    )
}

/// Native boundary for one Core-authorized, one-start atomic
/// duration-completion lifecycle.
///
/// Implementations may renew authentication only before the first mutation or
/// for final read-only verification. They must not replay or resume mutations
/// after the first start request has been sent.
#[async_trait]
pub trait WellearnAtomicDurationCompletionTransport: Send + Sync {
    async fn complete_duration_atomically(
        &self,
        context: &ProviderContext,
        child: &WellearnAtomicChildPlan,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<WellearnAtomicDurationCompletionDocuments>;
}

/// Complete immutable wire plan for one donor-audited, one-start
/// duration-completion operation.
///
/// Core owns persistence, attempt authority and scheduling. This value only
/// binds all `WELearn` protocol facts that the authorized Provider operation
/// must consume together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnAtomicDurationCompletionPlan {
    profile: WellearnAtomicCompletionProfile,
    target_seconds: u64,
    heartbeat_interval_seconds: u64,
    duration_protocol_mode: WellearnDurationProtocolMode,
    completion: WellearnResourceExecutionPlan,
}

impl WellearnAtomicDurationCompletionPlan {
    /// Builds the sole complete wire plan associated with the frozen atomic
    /// completion profile and target.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the target is outside the profile's
    /// evidenced bounds. Both atomic profiles accept zero: modular Auto may
    /// derive a zero-second floor allocation, while current Fanyuchang accepts
    /// a literal `0` input and still performs start plus final completion.
    pub fn try_new(
        profile: WellearnAtomicCompletionProfile,
        target_seconds: u64,
    ) -> ProviderResult<Self> {
        use WellearnAtomicCompletionProfile::{AutoZeroTimeSaveOnly0, FanyuchangFreshSetSave100};

        let plan = match profile {
            FanyuchangFreshSetSave100 => Self {
                profile,
                target_seconds,
                heartbeat_interval_seconds: 1,
                duration_protocol_mode: WellearnDurationProtocolMode::ClientCounter,
                completion: WellearnResourceExecutionPlan {
                    score_percent: 100,
                    sequence: WellearnResourceCompletionSequence::SelectedScore,
                    time_mode: WellearnResourceCompletionTimeMode::PreserveFreshTime,
                    cmi_format: WellearnResourceCompletionCmiFormat::Json,
                    write_mode: WellearnResourceCompletionWriteMode::SetThenSave,
                    mutation_profile: WellearnResourceMutationProfile::CurrentFullSimpleReferer,
                },
            },
            AutoZeroTimeSaveOnly0 => Self {
                profile,
                target_seconds,
                heartbeat_interval_seconds: 60,
                duration_protocol_mode: WellearnDurationProtocolMode::ImplicitServer,
                completion: WellearnResourceExecutionPlan {
                    score_percent: 0,
                    sequence: WellearnResourceCompletionSequence::SelectedScore,
                    time_mode: WellearnResourceCompletionTimeMode::ZeroTime,
                    cmi_format: WellearnResourceCompletionCmiFormat::InteractionInfoSuffix,
                    write_mode: WellearnResourceCompletionWriteMode::SaveOnly,
                    mutation_profile: WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
                },
            },
        };
        plan.validate()?;
        Ok(plan)
    }

    pub const fn profile(self) -> WellearnAtomicCompletionProfile {
        self.profile
    }

    pub const fn target_seconds(self) -> u64 {
        self.target_seconds
    }

    pub const fn heartbeat_interval_seconds(self) -> u64 {
        self.heartbeat_interval_seconds
    }

    pub const fn duration_protocol_mode(self) -> WellearnDurationProtocolMode {
        self.duration_protocol_mode
    }

    pub const fn completion(self) -> WellearnResourceExecutionPlan {
        self.completion
    }

    /// Revalidates a restored plan before any fresh discovery or mutation.
    ///
    /// # Errors
    ///
    /// Returns an internal error if any independently persisted field no
    /// longer matches the selected donor profile or target bounds.
    pub fn validate(self) -> ProviderResult<()> {
        use WellearnAtomicCompletionProfile::{AutoZeroTimeSaveOnly0, FanyuchangFreshSetSave100};
        use WellearnDurationProtocolMode::{ClientCounter, ImplicitServer};
        use WellearnResourceCompletionCmiFormat::{InteractionInfoSuffix, Json};
        use WellearnResourceCompletionSequence::SelectedScore;
        use WellearnResourceCompletionTimeMode::{PreserveFreshTime, ZeroTime};
        use WellearnResourceCompletionWriteMode::{SaveOnly, SetThenSave};
        use WellearnResourceMutationProfile::{CurrentFullSimpleReferer, LegacyMinimalTaskReferer};

        if self.completion.validate().is_err() || !self.completion.requires_atomic_authority() {
            return Err(invalid_atomic_plan());
        }
        let complete_profile = (
            self.profile,
            self.target_seconds,
            self.heartbeat_interval_seconds,
            self.duration_protocol_mode,
            self.completion.score_percent,
            self.completion.sequence,
            self.completion.time_mode,
            self.completion.cmi_format,
            self.completion.write_mode,
            self.completion.mutation_profile,
        );
        let valid = matches!(
            complete_profile,
            (
                FanyuchangFreshSetSave100,
                0..=MAX_DURATION_REPORT_SECONDS,
                1,
                ClientCounter,
                100,
                SelectedScore,
                PreserveFreshTime,
                Json,
                SetThenSave,
                CurrentFullSimpleReferer,
            ) | (
                AutoZeroTimeSaveOnly0,
                0..=MAX_DURATION_REPORT_SECONDS,
                60,
                ImplicitServer,
                0,
                SelectedScore,
                ZeroTime,
                InteractionInfoSuffix,
                SaveOnly,
                LegacyMinimalTaskReferer,
            )
        );
        if !valid {
            return Err(invalid_atomic_plan());
        }
        Ok(())
    }
}

fn invalid_atomic_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic duration-completion plan mixes incompatible donor wire facts",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_kinds_have_stable_bounded_operation_types() {
        let actual = [
            WellearnAtomicMutationKind::Start,
            WellearnAtomicMutationKind::CounterKeep,
            WellearnAtomicMutationKind::ImplicitKeep,
            WellearnAtomicMutationKind::Set,
            WellearnAtomicMutationKind::Save,
        ]
        .map(WellearnAtomicMutationKind::as_str);
        assert_eq!(
            actual,
            [
                "welearn.atomic.start",
                "welearn.atomic.keep_counter",
                "welearn.atomic.keep_implicit",
                "welearn.atomic.set",
                "welearn.atomic.save",
            ]
        );
        assert!(
            actual
                .iter()
                .all(|value| !value.is_empty() && value.len() <= 96)
        );
    }

    #[test]
    fn constructor_freezes_each_complete_donor_profile() {
        let current = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            30,
        )
        .unwrap();
        assert_eq!(current.heartbeat_interval_seconds, 1);
        assert_eq!(
            current.duration_protocol_mode,
            WellearnDurationProtocolMode::ClientCounter
        );
        assert_eq!(current.completion.score_percent, 100);
        assert_eq!(
            current.completion.time_mode,
            WellearnResourceCompletionTimeMode::PreserveFreshTime
        );
        assert_eq!(
            current.completion.write_mode,
            WellearnResourceCompletionWriteMode::SetThenSave
        );
        assert_eq!(
            current.completion.mutation_profile,
            WellearnResourceMutationProfile::CurrentFullSimpleReferer
        );
        current.validate().unwrap();

        let auto = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        assert_eq!(auto.heartbeat_interval_seconds, 60);
        assert_eq!(
            auto.duration_protocol_mode,
            WellearnDurationProtocolMode::ImplicitServer
        );
        assert_eq!(auto.completion.score_percent, 0);
        assert_eq!(
            auto.completion.write_mode,
            WellearnResourceCompletionWriteMode::SaveOnly
        );
        assert_eq!(
            auto.completion.mutation_profile,
            WellearnResourceMutationProfile::LegacyMinimalTaskReferer
        );
        auto.validate().unwrap();
    }

    #[test]
    fn target_bounds_preserve_both_evidenced_zero_shapes() {
        assert!(
            WellearnAtomicDurationCompletionPlan::try_new(
                WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                0,
            )
            .is_ok()
        );
        assert!(
            WellearnAtomicDurationCompletionPlan::try_new(
                WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                MAX_DURATION_REPORT_SECONDS,
            )
            .is_ok()
        );
        assert!(
            WellearnAtomicDurationCompletionPlan::try_new(
                WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
                0,
            )
            .is_ok()
        );
        for profile in [
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
        ] {
            assert!(
                WellearnAtomicDurationCompletionPlan::try_new(
                    profile,
                    MAX_DURATION_REPORT_SECONDS + 1,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn restored_plan_rejects_every_cross_donor_drift() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            10,
        )
        .unwrap();

        let mut drifted = plan;
        drifted.profile = WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.heartbeat_interval_seconds = 60;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.duration_protocol_mode = WellearnDurationProtocolMode::ImplicitServer;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.completion.score_percent = 0;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.completion.cmi_format = WellearnResourceCompletionCmiFormat::InteractionInfoSuffix;
        assert!(drifted.validate().is_err());

        let mut drifted = plan;
        drifted.completion.write_mode = WellearnResourceCompletionWriteMode::SaveOnly;
        assert!(drifted.validate().is_err());
    }

    #[test]
    fn current_documents_preserve_ordered_keep_rejection_and_false_diagnostics() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            3,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            Some(cmi()),
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(
                true,
                vec![true, false],
                Some(false),
                false,
            ),
        )
        .unwrap();

        assert!(documents.after_duration().is_some());
        assert_eq!(documents.receipts().heartbeat_acceptances(), [true, false]);
        assert_eq!(documents.receipts().set_accepted(), Some(false));
        assert!(!documents.receipts().save_accepted());
        documents.validate_for_plan(plan).unwrap();
    }

    #[test]
    fn current_zero_target_still_requires_start_completion_and_fresh_readback() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            0,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            Some(snapshot_cmi(
                "incomplete",
                "0.25",
                "20",
                Some("0"),
                Some("0"),
            )),
            snapshot_cmi("completed", "1", "100", Some("0"), Some("0")),
            WellearnAtomicDurationCompletionReceipts::new(true, Vec::new(), Some(true), true),
        )
        .unwrap();

        assert!(documents.receipts().heartbeat_acceptances().is_empty());
        let verification = verify_atomic_duration_completion(plan, &documents).unwrap();
        assert_eq!(verification.score_percent(), 100);
        assert_eq!(verification.time_preservation_verified(), Some(true));
        assert_eq!(verification.final_save_ordinal(), 3);
        assert!(verification.final_save_accepted());
    }

    #[test]
    fn auto_documents_bind_complete_intervals_and_zero_floor() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            125,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            None,
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(false, vec![false, true], None, false),
        )
        .unwrap();
        assert!(documents.after_duration().is_none());
        assert_eq!(documents.receipts().heartbeat_acceptances().len(), 2);
        assert_eq!(documents.final_save_ordinal(plan).unwrap(), 4);

        let zero = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        WellearnAtomicDurationCompletionDocuments::try_new(
            zero,
            cmi(),
            None,
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(true, Vec::new(), None, true),
        )
        .unwrap();
    }

    #[test]
    fn documents_reject_impossible_current_lifecycles() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            3,
        )
        .unwrap();
        for (after_duration, receipts) in [
            (
                None,
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    false,
                    vec![true, true, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, false, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true],
                    Some(true),
                    true,
                ),
            ),
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true, true],
                    None,
                    true,
                ),
            ),
        ] {
            assert!(
                WellearnAtomicDurationCompletionDocuments::try_new(
                    plan,
                    cmi(),
                    after_duration,
                    cmi(),
                    receipts,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn documents_reject_unknown_initial_cmi_before_goal_verification() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        let malformed = WellearnCmiDocument::try_new("not-json".to_owned()).unwrap();
        let error = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            malformed,
            None,
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(true, Vec::new(), None, true),
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);

        let uninitialized =
            WellearnCmiDocument::try_new("学习数据不正确，请先开始学习".to_owned()).unwrap();
        WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            uninitialized,
            None,
            cmi(),
            WellearnAtomicDurationCompletionReceipts::new(true, Vec::new(), None, true),
        )
        .unwrap();
    }

    #[test]
    fn documents_reject_impossible_auto_lifecycles() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            120,
        )
        .unwrap();
        for (after_duration, receipts) in [
            (
                Some(cmi()),
                WellearnAtomicDurationCompletionReceipts::new(true, vec![true, true], None, true),
            ),
            (
                None,
                WellearnAtomicDurationCompletionReceipts::new(true, vec![true], None, true),
            ),
            (
                None,
                WellearnAtomicDurationCompletionReceipts::new(
                    true,
                    vec![true, true],
                    Some(true),
                    true,
                ),
            ),
        ] {
            assert!(
                WellearnAtomicDurationCompletionDocuments::try_new(
                    plan,
                    cmi(),
                    after_duration,
                    cmi(),
                    receipts,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn verifier_proves_current_goal_and_preserved_fresh_times() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            1,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            Some(snapshot_cmi(
                "incomplete",
                "0.25",
                "20",
                Some("15"),
                Some("45"),
            )),
            snapshot_cmi("completed", "1", "100", Some("15"), Some("45")),
            WellearnAtomicDurationCompletionReceipts::new(true, vec![true], Some(false), false),
        )
        .unwrap();

        let verification = verify_atomic_duration_completion(plan, &documents).unwrap();
        assert_eq!(verification.profile(), plan.profile());
        assert_eq!(verification.score_percent(), 100);
        assert_eq!(verification.time_preservation_verified(), Some(true));
        assert_eq!(verification.final_save_ordinal(), 4);
        assert!(!verification.final_save_accepted());
        assert_ne!(verification.observation_digest(), [0; 32]);
        assert!(
            verification
                .to_execution_mutation_verification()
                .unwrap()
                .is_none()
        );
        let debug = format!("{verification:?}");
        assert!(debug.contains("[HASHED]"));
        assert!(!debug.contains(&format!("{:?}", verification.observation_digest())));

        let accepted_documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            Some(snapshot_cmi(
                "incomplete",
                "0.25",
                "20",
                Some("15"),
                Some("45"),
            )),
            snapshot_cmi("completed", "1", "100", Some("15"), Some("45")),
            WellearnAtomicDurationCompletionReceipts::new(true, vec![true], Some(false), true),
        )
        .unwrap();
        let accepted = verify_atomic_duration_completion(plan, &accepted_documents).unwrap();
        let record = accepted
            .to_execution_mutation_verification()
            .unwrap()
            .unwrap();
        assert_eq!(record.ordinal(), accepted.final_save_ordinal());
        assert_eq!(record.observation_digest(), accepted.observation_digest());
        assert!(record.verified());
    }

    #[test]
    fn verifier_proves_auto_goal_without_inventing_a_time_predicate() {
        let plan = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            0,
        )
        .unwrap();
        let documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            None,
            snapshot_cmi("completed", "1", "0", Some("87"), Some("120")),
            WellearnAtomicDurationCompletionReceipts::new(false, Vec::new(), None, false),
        )
        .unwrap();

        let verification = verify_atomic_duration_completion(plan, &documents).unwrap();
        assert_eq!(verification.score_percent(), 0);
        assert_eq!(verification.time_preservation_verified(), None);
        assert_eq!(verification.final_save_ordinal(), 2);
        assert!(!verification.final_save_accepted());
        assert!(
            verification
                .to_execution_mutation_verification()
                .unwrap()
                .is_none()
        );

        let changed_documents = WellearnAtomicDurationCompletionDocuments::try_new(
            plan,
            cmi(),
            None,
            snapshot_cmi("completed", "1", "0", Some("88"), Some("120")),
            WellearnAtomicDurationCompletionReceipts::new(false, Vec::new(), None, false),
        )
        .unwrap();
        assert_ne!(
            verification.observation_digest(),
            verify_atomic_duration_completion(plan, &changed_documents)
                .unwrap()
                .observation_digest()
        );
    }

    #[test]
    fn verifier_rejects_final_goal_or_current_time_drift() {
        let current = WellearnAtomicDurationCompletionPlan::try_new(
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            1,
        )
        .unwrap();
        for (after_duration, after_completion) in [
            (
                snapshot_cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
                snapshot_cmi("completed", "1", "99", Some("15"), Some("45")),
            ),
            (
                snapshot_cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
                snapshot_cmi("completed", "1", "100", Some("16"), Some("45")),
            ),
            (
                snapshot_cmi("incomplete", "0.25", "20", None, Some("45")),
                snapshot_cmi("completed", "1", "100", None, Some("45")),
            ),
        ] {
            let documents = WellearnAtomicDurationCompletionDocuments::try_new(
                current,
                cmi(),
                Some(after_duration),
                after_completion,
                WellearnAtomicDurationCompletionReceipts::new(true, vec![true], Some(true), true),
            )
            .unwrap();
            assert_eq!(
                verify_atomic_duration_completion(current, &documents)
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::RemoteChanged
            );
        }
    }

    fn cmi() -> WellearnCmiDocument {
        WellearnCmiDocument::try_new(r#"{"ret":0,"comment":"{}"}"#.to_owned()).unwrap()
    }

    fn snapshot_cmi(
        completion: &str,
        progress: &str,
        score: &str,
        session_time: Option<&str>,
        total_time: Option<&str>,
    ) -> WellearnCmiDocument {
        let mut cmi = serde_json::json!({
            "completion_status": completion,
            "progress_measure": progress,
            "score": {"scaled": score},
            "success_status": "unknown",
        });
        if let Some(session_time) = session_time {
            cmi["session_time"] = serde_json::json!(session_time);
        }
        if let Some(total_time) = total_time {
            cmi["total_time"] = serde_json::json!(total_time);
        }
        WellearnCmiDocument::try_new(
            serde_json::json!({
                "ret": 0,
                "comment": serde_json::json!({"cmi": cmi}).to_string(),
            })
            .to_string(),
        )
        .unwrap()
    }
}
