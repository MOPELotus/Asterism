use std::{fmt, sync::Arc};

use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationSequenceObservation,
    ExecutionMutationSequencePlan, ExecutionMutationSequenceRecoverySnapshot,
    ExecutionMutationVerification, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderExecutionPlanArtifact, ProviderIdentity, ProviderMetadata, ProviderResult,
    TaskDetailCapability,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    WellearnAtomicChildPlan, WellearnAtomicCompletionProfile, WellearnAtomicDurationCompletionPlan,
    WellearnAtomicDurationCompletionReceipts, WellearnAtomicDurationCompletionVerification,
    WellearnAtomicMutationKind, WellearnCmiDocument, WellearnPreparedAtomicChildPlan,
    WellearnResourceExecutionTransport,
    atomic_duration_completion::{atomic_goal_changed, verify_atomic_final_snapshot},
    build_atomic_mutation_sequence_plan,
    cmi::parse_sco_identity,
    metadata::development_metadata,
    parse_cmi_snapshot,
};

/// Namespaced Provider-private type for one pre-final Fanyuchang observation.
pub const WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE: &str =
    "welearn.atomic-pre-final-observation.v1";

/// The Fanyuchang set phase that requires the pre-final time observation.
pub const WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_PHASE_POSITION: u8 = 3;

const WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_VERSION: u16 = 1;
const MAX_ATOMIC_PRE_FINAL_OBSERVATION_BYTES: usize = 512;
const PRE_FINAL_TIME_DOMAIN: &[u8] = b"asterism.welearn.atomic-pre-final-time.v1\0";
const RECOVERY_OBSERVATION_DOMAIN: &[u8] = b"asterism.welearn.atomic-recovery-observation.v1\0";

/// Read-only boundary for the exact final CMI route of one atomic child.
#[async_trait]
pub trait WellearnAtomicDurationCompletionRecoveryTransport: Send + Sync {
    async fn read_atomic_final(
        &self,
        context: &ProviderContext,
        child: &WellearnAtomicChildPlan,
    ) -> ProviderResult<WellearnCmiDocument>;
}

/// Fresh-rebind and read-only verification coordinator for a durable atomic
/// child attempt. It has no mutation or resume method.
pub struct WellearnAtomicDurationCompletionRecovery {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn WellearnAtomicDurationCompletionRecoveryTransport>,
}

impl WellearnAtomicDurationCompletionRecovery {
    /// Builds the recovery-only coordinator around injected fresh-read
    /// boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn WellearnAtomicDurationCompletionRecoveryTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
        })
    }

    /// Fresh-rebinds one prepared child and proves its final remote goal using
    /// only the exact durable sequence records and one new CMI read.
    ///
    /// Sequence-record drift is rejected before fresh discovery. Fresh Task
    /// drift is rejected before the CMI read. No mutation is issued on any
    /// branch.
    ///
    /// # Errors
    ///
    /// Returns a typed error for context, prepared-child, sequence-record,
    /// fresh-detail, CMI or exact-goal drift.
    #[allow(
        clippy::too_many_arguments,
        reason = "recovery keeps each independently durable Core record explicit"
    )]
    pub async fn verify_prepared(
        &self,
        context: &ProviderContext,
        prepared: &WellearnPreparedAtomicChildPlan,
        sequence_plan: &ExecutionMutationSequencePlan,
        issues: &[ExecutionMutationIssue],
        receipts: &[ExecutionMutationReceipt],
        observation: Option<&ExecutionMutationSequenceObservation>,
    ) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn atomic recovery received a foreign Provider context",
            ));
        }
        prepared.validate()?;
        let child = prepared.child_plan();
        let (restored_receipts, pre_final) =
            restore_recovery_sequence_records(child, sequence_plan, issues, receipts, observation)?;
        let detail = self
            .details
            .task_detail(context, child.remote_task_id())
            .await?;
        prepared.validate_fresh_detail(&detail)?;
        let fresh_final = self.transport.read_atomic_final(context, child).await?;
        verify_atomic_duration_completion_recovery(
            child,
            &restored_receipts,
            pre_final.as_ref(),
            &fresh_final,
        )
    }

    /// Fresh-rebinds and verifies one prepared child from Core's immutable
    /// same-attempt sequence snapshot.
    ///
    /// The snapshot is fully restored before Task discovery. An ambiguous
    /// final issue, a foreign artifact/plan/observation or a misplaced durable
    /// verification therefore stops before any fresh I/O.
    ///
    /// # Errors
    ///
    /// Returns a typed error for context, prepared-child, snapshot,
    /// fresh-detail, CMI or exact-goal drift.
    pub async fn verify_prepared_snapshot(
        &self,
        context: &ProviderContext,
        prepared: &WellearnPreparedAtomicChildPlan,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn atomic recovery received a foreign Provider context",
            ));
        }
        prepared.validate()?;
        let child = prepared.child_plan();
        let (receipts, pre_final, stored_verification) =
            restore_recovery_sequence_snapshot(child, snapshot)?;
        let detail = self
            .details
            .task_detail(context, child.remote_task_id())
            .await?;
        prepared.validate_fresh_detail(&detail)?;
        let fresh_final = self.transport.read_atomic_final(context, child).await?;
        verify_restored_recovery_snapshot(
            child,
            &receipts,
            pre_final.as_ref(),
            stored_verification,
            &fresh_final,
        )
    }

    /// Restores all Provider-private durable artifacts together, then enters
    /// the same record-first, fresh-rebind, read-only verification path.
    ///
    /// # Errors
    ///
    /// Returns a typed error for any parent, batch, child, sequence, receipt,
    /// observation, fresh-detail or final-CMI inconsistency.
    #[allow(
        clippy::too_many_arguments,
        reason = "the boundary keeps each independently stored recovery value explicit"
    )]
    pub async fn verify_durable_artifacts(
        &self,
        context: &ProviderContext,
        encoded_parent_authority: &[u8],
        encoded_batch_snapshot: &[u8],
        child_artifact: &ProviderExecutionPlanArtifact,
        sequence_plan: &ExecutionMutationSequencePlan,
        issues: &[ExecutionMutationIssue],
        receipts: &[ExecutionMutationReceipt],
        observation: Option<&ExecutionMutationSequenceObservation>,
    ) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
        let prepared = WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
            encoded_parent_authority,
            encoded_batch_snapshot,
            child_artifact,
        )?;
        self.verify_prepared(
            context,
            &prepared,
            sequence_plan,
            issues,
            receipts,
            observation,
        )
        .await
    }

    /// Jointly restores parent, batch and the snapshot's exact child artifact,
    /// then enters the same snapshot-first read-only verification path.
    ///
    /// # Errors
    ///
    /// Returns a typed error for any parent, batch, child, snapshot,
    /// fresh-detail or final-CMI inconsistency.
    pub async fn verify_durable_snapshot(
        &self,
        context: &ProviderContext,
        encoded_parent_authority: &[u8],
        encoded_batch_snapshot: &[u8],
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
        let prepared = WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
            encoded_parent_authority,
            encoded_batch_snapshot,
            snapshot.artifact(),
        )?;
        self.verify_prepared_snapshot(context, &prepared, snapshot)
            .await
    }
}

impl fmt::Debug for WellearnAtomicDurationCompletionRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnAtomicDurationCompletionRecovery")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnAtomicDurationCompletionRecovery {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl<T> WellearnAtomicDurationCompletionRecoveryTransport for T
where
    T: WellearnResourceExecutionTransport + Send + Sync,
{
    async fn read_atomic_final(
        &self,
        context: &ProviderContext,
        child: &WellearnAtomicChildPlan,
    ) -> ProviderResult<WellearnCmiDocument> {
        child.validate()?;
        let (course_id, sco_id) = parse_sco_identity(child.remote_task_id())?;
        let mutation_profile = child
            .duration_completion_plan()?
            .completion()
            .mutation_profile;
        self.verify_resource(context, &course_id, &sco_id, mutation_profile)
            .await
    }
}

/// Hash-only evidence of the Fanyuchang time values read after its duration
/// phase and before its completion-bearing set/save mutations.
///
/// Core must bind the encoded value to the same execution attempt before the
/// set is issued. The value carries no raw CMI, route or credential material
/// and never grants mutation or resume authority.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnAtomicPreFinalObservation {
    version: u16,
    binding_digest: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnAtomicPreFinalObservationWire {
    version: u16,
    binding_digest: [u8; 32],
}

impl WellearnAtomicPreFinalObservation {
    /// Captures only the child-bound hash of Fanyuchang's fresh post-duration
    /// time pair.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an invalid/non-Fanyuchang child, malformed CMI
    /// or missing post-duration time evidence.
    pub fn capture(
        child: &WellearnAtomicChildPlan,
        after_duration: &WellearnCmiDocument,
    ) -> ProviderResult<Self> {
        child.validate()?;
        if child.atomic_completion_profile()
            != WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100
        {
            return Err(invalid_pre_final_observation());
        }
        let snapshot = parse_cmi_snapshot(after_duration.as_str())?;
        let (session_time, total_time) = required_time_pair(&snapshot).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn Fanyuchang post-duration CMI has no complete time evidence",
            )
        })?;
        let observation = Self {
            version: WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_VERSION,
            binding_digest: pre_final_time_digest(child, session_time, total_time)?,
        };
        observation.validate()?;
        Ok(observation)
    }

    pub const fn binding_digest(&self) -> [u8; 32] {
        self.binding_digest
    }

    /// Adapts the hash-only value to Core's attempt-bound sequence record.
    ///
    /// # Errors
    ///
    /// Returns an internal error if this observation no longer validates.
    pub fn to_sequence_observation(&self) -> ProviderResult<ExecutionMutationSequenceObservation> {
        self.validate()?;
        ExecutionMutationSequenceObservation::try_new(
            WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_PHASE_POSITION,
            WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE,
            self.binding_digest,
        )
        .map_err(|_| invalid_pre_final_observation())
    }

    /// Restores the Provider value from Core's exact phase/type/digest record.
    ///
    /// # Errors
    ///
    /// Returns an internal error for a foreign phase, type or zero digest.
    pub fn from_sequence_observation(
        observation: &ExecutionMutationSequenceObservation,
    ) -> ProviderResult<Self> {
        if observation.phase_position() != WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_PHASE_POSITION
            || observation.observation_type() != WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE
        {
            return Err(invalid_pre_final_observation());
        }
        let observation = Self {
            version: WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_VERSION,
            binding_digest: observation.observation_digest(),
        };
        observation.validate()?;
        Ok(observation)
    }

    /// Encodes the hash-only observation under the bounded v1 schema.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the observation is invalid, cannot be
    /// serialized or exceeds the local 512-byte bound.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate()?;
        let encoded = serde_json::to_vec(&WellearnAtomicPreFinalObservationWire {
            version: self.version,
            binding_digest: self.binding_digest,
        })
        .map_err(|_| invalid_pre_final_observation())?;
        if encoded.is_empty() || encoded.len() > MAX_ATOMIC_PRE_FINAL_OBSERVATION_BYTES {
            return Err(invalid_pre_final_observation());
        }
        Ok(encoded)
    }

    /// Restores and validates one bounded hash-only observation.
    ///
    /// # Errors
    ///
    /// Returns an internal error for empty, oversized, malformed, unknown,
    /// version-drifted or zero-digest input.
    pub fn decode(encoded: &[u8]) -> ProviderResult<Self> {
        if encoded.is_empty() || encoded.len() > MAX_ATOMIC_PRE_FINAL_OBSERVATION_BYTES {
            return Err(invalid_pre_final_observation());
        }
        let wire: WellearnAtomicPreFinalObservationWire =
            serde_json::from_slice(encoded).map_err(|_| invalid_pre_final_observation())?;
        let observation = Self {
            version: wire.version,
            binding_digest: wire.binding_digest,
        };
        observation.validate()?;
        Ok(observation)
    }

    fn validate(&self) -> ProviderResult<()> {
        if self.version != WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_VERSION
            || self.binding_digest == [0; 32]
        {
            return Err(invalid_pre_final_observation());
        }
        Ok(())
    }

    fn verify_final_snapshot(
        &self,
        child: &WellearnAtomicChildPlan,
        final_snapshot: &crate::WellearnCmiSnapshot,
    ) -> ProviderResult<()> {
        self.validate()?;
        let (session_time, total_time) =
            required_time_pair(final_snapshot).ok_or_else(atomic_goal_changed)?;
        if pre_final_time_digest(child, session_time, total_time)? != self.binding_digest {
            return Err(atomic_goal_changed());
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnAtomicPreFinalObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnAtomicPreFinalObservation")
            .field("version", &self.version)
            .field("binding_digest", &"[HASHED]")
            .finish()
    }
}

/// Performs read-only recovery verification from a fresh final CMI document.
///
/// Fanyuchang requires the attempt-bound pre-final observation so the final
/// time pair is proven equal to the values read before its set/save. Auto has
/// no evidenced time predicate and therefore rejects an invented observation.
/// Receipt shape determines the response-dependent final-save ordinal without
/// replaying any mutation.
///
/// # Errors
///
/// Returns a typed error for child/receipt/evidence drift or when the exact
/// completion, progress, score and required time-preservation goal is absent.
pub fn verify_atomic_duration_completion_recovery(
    child: &WellearnAtomicChildPlan,
    receipts: &WellearnAtomicDurationCompletionReceipts,
    pre_final: Option<&WellearnAtomicPreFinalObservation>,
    fresh_final: &WellearnCmiDocument,
) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
    child.validate()?;
    let plan = child.duration_completion_plan()?;
    receipts.validate_for_plan(plan, pre_final.is_some())?;
    let final_snapshot = parse_cmi_snapshot(fresh_final.as_str())?;
    verify_atomic_final_snapshot(plan, &final_snapshot)?;
    let time_preservation_verified = match plan.profile() {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            pre_final
                .ok_or_else(invalid_pre_final_observation)?
                .verify_final_snapshot(child, &final_snapshot)?;
            Some(true)
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => None,
    };
    let final_save_ordinal = receipts.final_save_ordinal(plan)?;
    let observation_digest =
        recovery_observation_digest(child, final_save_ordinal, pre_final, fresh_final.as_str())?;
    Ok(WellearnAtomicDurationCompletionVerification {
        profile: plan.profile(),
        score_percent: plan.completion().score_percent,
        time_preservation_verified,
        final_save_ordinal,
        final_save_accepted: receipts.save_accepted(),
        observation_digest,
    })
}

/// Rebuilds `WELearn` recovery evidence from Core's exact durable sequence
/// records, then performs the same read-only final-CMI proof.
///
/// The caller remains responsible for loading all values from the same bound
/// attempt. This adapter verifies the complete child-bound plan, contiguous
/// issue/receipt pairs and donor-specific operation shape; it never authorizes
/// or replays a mutation.
///
/// # Errors
///
/// Returns a typed error for plan, ordinal, operation, receipt, observation or
/// final-goal drift.
pub fn verify_atomic_duration_completion_recovery_from_sequence_records(
    child: &WellearnAtomicChildPlan,
    sequence_plan: &ExecutionMutationSequencePlan,
    issues: &[ExecutionMutationIssue],
    receipts: &[ExecutionMutationReceipt],
    observation: Option<&ExecutionMutationSequenceObservation>,
    fresh_final: &WellearnCmiDocument,
) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
    let (receipts, pre_final) =
        restore_recovery_sequence_records(child, sequence_plan, issues, receipts, observation)?;
    verify_atomic_duration_completion_recovery(child, &receipts, pre_final.as_ref(), fresh_final)
}

/// Verifies one atomic child from Core's complete same-attempt recovery
/// snapshot and a new final CMI read.
///
/// This adapter requires the exact child artifact and sequence, a receipt for
/// every issued mutation, the donor-specific observation shape and at most one
/// already-persisted verification on the accepted final save. It never issues
/// or resumes a mutation.
///
/// # Errors
///
/// Returns a typed error for artifact, plan, record, observation, persisted
/// verification or final-goal drift.
pub fn verify_atomic_duration_completion_recovery_from_sequence_snapshot(
    child: &WellearnAtomicChildPlan,
    snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    fresh_final: &WellearnCmiDocument,
) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
    let (receipts, pre_final, stored_verification) =
        restore_recovery_sequence_snapshot(child, snapshot)?;
    verify_restored_recovery_snapshot(
        child,
        &receipts,
        pre_final.as_ref(),
        stored_verification,
        fresh_final,
    )
}

fn restore_recovery_sequence_snapshot(
    child: &WellearnAtomicChildPlan,
    snapshot: &ExecutionMutationSequenceRecoverySnapshot,
) -> ProviderResult<(
    WellearnAtomicDurationCompletionReceipts,
    Option<WellearnAtomicPreFinalObservation>,
    Option<ExecutionMutationVerification>,
)> {
    child.validate()?;
    if snapshot.artifact() != &child.to_provider_execution_plan_artifact()? {
        return Err(invalid_recovery_sequence_records());
    }
    let issues = snapshot
        .records()
        .iter()
        .map(|record| record.issue().clone())
        .collect::<Vec<_>>();
    let receipts = snapshot
        .records()
        .iter()
        .map(|record| {
            record
                .receipt()
                .ok_or_else(invalid_recovery_sequence_records)
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    let observation = match snapshot.observations() {
        [] => None,
        [observation] => Some(observation),
        _ => return Err(invalid_recovery_sequence_records()),
    };
    let (receipts, pre_final) =
        restore_recovery_sequence_records(child, snapshot.plan(), &issues, &receipts, observation)?;
    let final_save_ordinal = receipts.final_save_ordinal(child.duration_completion_plan()?)?;
    let mut stored_verifications = snapshot
        .records()
        .iter()
        .filter_map(asterism_provider_api::ExecutionMutationRecoveryRecord::verification);
    let stored_verification = stored_verifications.next();
    if stored_verifications.next().is_some()
        || stored_verification.is_some_and(|verification| {
            verification.ordinal() != final_save_ordinal || !verification.verified()
        })
    {
        return Err(invalid_recovery_sequence_records());
    }
    Ok((receipts, pre_final, stored_verification))
}

fn verify_restored_recovery_snapshot(
    child: &WellearnAtomicChildPlan,
    receipts: &WellearnAtomicDurationCompletionReceipts,
    pre_final: Option<&WellearnAtomicPreFinalObservation>,
    stored_verification: Option<ExecutionMutationVerification>,
    fresh_final: &WellearnCmiDocument,
) -> ProviderResult<WellearnAtomicDurationCompletionVerification> {
    let verification =
        verify_atomic_duration_completion_recovery(child, receipts, pre_final, fresh_final)?;
    if stored_verification.is_some()
        && verification.to_execution_mutation_verification()? != stored_verification
    {
        return Err(invalid_recovery_sequence_records());
    }
    Ok(verification)
}

fn restore_recovery_sequence_records(
    child: &WellearnAtomicChildPlan,
    sequence_plan: &ExecutionMutationSequencePlan,
    issues: &[ExecutionMutationIssue],
    receipts: &[ExecutionMutationReceipt],
    observation: Option<&ExecutionMutationSequenceObservation>,
) -> ProviderResult<(
    WellearnAtomicDurationCompletionReceipts,
    Option<WellearnAtomicPreFinalObservation>,
)> {
    child.validate()?;
    let artifact = child.to_provider_execution_plan_artifact()?;
    if build_atomic_mutation_sequence_plan(child, &artifact)? != *sequence_plan {
        return Err(invalid_recovery_sequence_records());
    }
    let plan = child.duration_completion_plan()?;
    let receipts = recovery_receipts_from_sequence_records(plan, issues, receipts)?;
    let pre_final = observation
        .map(WellearnAtomicPreFinalObservation::from_sequence_observation)
        .transpose()?;
    receipts.validate_for_plan(plan, pre_final.is_some())?;
    Ok((receipts, pre_final))
}

fn recovery_receipts_from_sequence_records(
    plan: WellearnAtomicDurationCompletionPlan,
    issues: &[ExecutionMutationIssue],
    receipts: &[ExecutionMutationReceipt],
) -> ProviderResult<WellearnAtomicDurationCompletionReceipts> {
    plan.validate()?;
    if issues.len() != receipts.len()
        || receipts
            .iter()
            .any(|receipt| receipt.retry_after_seconds().is_some())
        || issues
            .iter()
            .zip(receipts)
            .enumerate()
            .any(|(index, (issue, receipt))| {
                let ordinal = u32::try_from(index + 1).ok();
                ordinal != Some(issue.ordinal()) || ordinal != Some(receipt.ordinal())
            })
    {
        return Err(invalid_recovery_sequence_records());
    }

    let (heartbeat_count, set_accepted) = match plan.profile() {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            let heartbeat_count = issues
                .len()
                .checked_sub(3)
                .ok_or_else(invalid_recovery_sequence_records)?;
            let maximum = usize::try_from(plan.target_seconds())
                .map_err(|_| invalid_recovery_sequence_records())?;
            if heartbeat_count > maximum
                || !operation_shape_matches(
                    issues,
                    heartbeat_count,
                    WellearnAtomicMutationKind::CounterKeep,
                    true,
                )
            {
                return Err(invalid_recovery_sequence_records());
            }
            (
                heartbeat_count,
                Some(receipts[heartbeat_count + 1].accepted()),
            )
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => {
            let heartbeat_count =
                usize::try_from(plan.target_seconds() / plan.heartbeat_interval_seconds())
                    .map_err(|_| invalid_recovery_sequence_records())?;
            if issues.len() != heartbeat_count + 2
                || !operation_shape_matches(
                    issues,
                    heartbeat_count,
                    WellearnAtomicMutationKind::ImplicitKeep,
                    false,
                )
            {
                return Err(invalid_recovery_sequence_records());
            }
            (heartbeat_count, None)
        }
    };

    let heartbeat_acceptances = receipts
        .iter()
        .skip(1)
        .take(heartbeat_count)
        .map(|receipt| receipt.accepted())
        .collect();
    let restored = WellearnAtomicDurationCompletionReceipts::new(
        receipts[0].accepted(),
        heartbeat_acceptances,
        set_accepted,
        receipts[receipts.len() - 1].accepted(),
    );
    restored.validate_for_plan(
        plan,
        matches!(
            plan.profile(),
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100
        ),
    )?;
    Ok(restored)
}

fn operation_shape_matches(
    issues: &[ExecutionMutationIssue],
    heartbeat_count: usize,
    heartbeat_kind: WellearnAtomicMutationKind,
    has_set: bool,
) -> bool {
    issues
        .first()
        .is_some_and(|issue| issue.operation_type() == WellearnAtomicMutationKind::Start.as_str())
        && issues
            .iter()
            .skip(1)
            .take(heartbeat_count)
            .all(|issue| issue.operation_type() == heartbeat_kind.as_str())
        && (!has_set
            || issues.get(heartbeat_count + 1).is_some_and(|issue| {
                issue.operation_type() == WellearnAtomicMutationKind::Set.as_str()
            }))
        && issues.last().is_some_and(|issue| {
            issue.operation_type() == WellearnAtomicMutationKind::Save.as_str()
        })
}

fn required_time_pair(snapshot: &crate::WellearnCmiSnapshot) -> Option<(&str, &str)> {
    if !snapshot.cmi_present() {
        return None;
    }
    Some((snapshot.session_time_raw()?, snapshot.total_time_raw()?))
}

fn pre_final_time_digest(
    child: &WellearnAtomicChildPlan,
    session_time: &str,
    total_time: &str,
) -> ProviderResult<[u8; 32]> {
    child.validate()?;
    if child.atomic_completion_profile()
        != WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100
    {
        return Err(invalid_pre_final_observation());
    }
    let mut hash = Sha256::new();
    hash.update(PRE_FINAL_TIME_DOMAIN);
    hash_child_binding(&mut hash, child)?;
    hash_component(&mut hash, session_time.as_bytes())?;
    hash_component(&mut hash, total_time.as_bytes())?;
    let digest = hash.finalize().into();
    if digest == [0; 32] {
        return Err(invalid_pre_final_observation());
    }
    Ok(digest)
}

fn recovery_observation_digest(
    child: &WellearnAtomicChildPlan,
    final_save_ordinal: u32,
    pre_final: Option<&WellearnAtomicPreFinalObservation>,
    fresh_final: &str,
) -> ProviderResult<[u8; 32]> {
    if !(1..=100_000).contains(&final_save_ordinal) || fresh_final.is_empty() {
        return Err(invalid_pre_final_observation());
    }
    let mut hash = Sha256::new();
    hash.update(RECOVERY_OBSERVATION_DOMAIN);
    hash.update(final_save_ordinal.to_be_bytes());
    hash_child_binding(&mut hash, child)?;
    match pre_final {
        Some(observation) => {
            observation.validate()?;
            hash.update([1]);
            hash.update(observation.binding_digest);
        }
        None => hash.update([0]),
    }
    hash_component(&mut hash, fresh_final.as_bytes())?;
    let digest = hash.finalize().into();
    if digest == [0; 32] {
        return Err(invalid_pre_final_observation());
    }
    Ok(digest)
}

fn hash_child_binding(hash: &mut Sha256, child: &WellearnAtomicChildPlan) -> ProviderResult<()> {
    child.validate()?;
    hash.update(child.version().to_be_bytes());
    hash.update(child.entry_index().to_be_bytes());
    hash.update(child.target_seconds().to_be_bytes());
    hash_component(hash, child.course_remote_id().as_bytes())?;
    hash_component(hash, child.remote_task_id().as_bytes())?;
    hash_component(
        hash,
        match child.atomic_completion_profile() {
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
                b"fanyuchang_fresh_set_save_100".as_slice()
            }
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => {
                b"auto_zero_time_save_only_0".as_slice()
            }
        },
    )
}

fn hash_component(hash: &mut Sha256, value: &[u8]) -> ProviderResult<()> {
    hash.update(
        u64::try_from(value.len())
            .map_err(|_| invalid_pre_final_observation())?
            .to_be_bytes(),
    );
    hash.update(value);
    Ok(())
}

fn invalid_pre_final_observation() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic pre-final observation is invalid or inconsistent",
    )
}

fn invalid_recovery_sequence_records() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn durable atomic recovery sequence records are invalid or inconsistent",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{ProviderAccountId, ProviderId};
    use asterism_provider_api::ExecutionMutationRecoveryRecord;

    use super::*;

    #[derive(Debug, Default)]
    struct RecoveryResourceFixture {
        profiles: Mutex<Vec<crate::WellearnResourceMutationProfile>>,
    }

    #[async_trait]
    impl WellearnResourceExecutionTransport for RecoveryResourceFixture {
        async fn complete_resource(
            &self,
            _context: &ProviderContext,
            _course_id: &str,
            _sco_id: &str,
            _plan: crate::WellearnResourceExecutionPlan,
        ) -> ProviderResult<crate::WellearnResourceExecutionDocuments> {
            Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "recovery fixture has no mutation path",
            ))
        }

        async fn verify_resource(
            &self,
            _context: &ProviderContext,
            course_id: &str,
            sco_id: &str,
            mutation_profile: crate::WellearnResourceMutationProfile,
        ) -> ProviderResult<WellearnCmiDocument> {
            assert_eq!(course_id, "1001");
            assert!(matches!(sco_id, "301" | "302"));
            self.profiles.lock().unwrap().push(mutation_profile);
            Ok(cmi(
                "completed",
                "1",
                if sco_id == "301" { "100" } else { "0" },
                Some("15"),
                Some("45"),
            ))
        }
    }

    #[test]
    fn fanyuchang_pre_final_observation_round_trips_and_verifies_recovery() {
        let child = fanyuchang_child(3);
        let after_duration = cmi("incomplete", "0.25", "20", Some("15"), Some("45"));
        let observation =
            WellearnAtomicPreFinalObservation::capture(&child, &after_duration).unwrap();
        assert_eq!(
            WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE,
            "welearn.atomic-pre-final-observation.v1"
        );
        assert_ne!(observation.binding_digest(), [0; 32]);
        let encoded = observation.encode().unwrap();
        assert!(encoded.len() <= MAX_ATOMIC_PRE_FINAL_OBSERVATION_BYTES);
        let encoded_value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let encoded_object = encoded_value.as_object().unwrap();
        assert_eq!(encoded_object.len(), 2);
        assert!(encoded_object.contains_key("version"));
        assert!(encoded_object.contains_key("binding_digest"));
        assert_eq!(
            WellearnAtomicPreFinalObservation::decode(&encoded).unwrap(),
            observation
        );
        let sequence_observation = observation.to_sequence_observation().unwrap();
        assert_eq!(sequence_observation.phase_position(), 3);
        assert_eq!(
            sequence_observation.observation_type(),
            WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE
        );
        assert_eq!(
            WellearnAtomicPreFinalObservation::from_sequence_observation(&sequence_observation)
                .unwrap(),
            observation
        );
        let debug = format!("{observation:?}");
        assert!(debug.contains("[HASHED]"));
        assert!(!debug.contains(&format!("{:?}", observation.binding_digest())));

        let receipts = WellearnAtomicDurationCompletionReceipts::new(
            true,
            vec![true, true, true],
            Some(true),
            true,
        );
        let verification = verify_atomic_duration_completion_recovery(
            &child,
            &receipts,
            Some(&observation),
            &cmi("completed", "1", "100", Some("15"), Some("45")),
        )
        .unwrap();
        assert_eq!(verification.final_save_ordinal(), 6);
        assert!(verification.final_save_accepted());
        assert_eq!(verification.time_preservation_verified(), Some(true));
        assert_ne!(verification.observation_digest(), [0; 32]);
    }

    #[test]
    fn fanyuchang_recovery_rejects_time_child_or_schema_drift() {
        let child = fanyuchang_child(3);
        let observation = WellearnAtomicPreFinalObservation::capture(
            &child,
            &cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
        )
        .unwrap();
        let receipts = WellearnAtomicDurationCompletionReceipts::new(
            true,
            vec![true, false],
            Some(false),
            true,
        );
        for (candidate_child, session_time) in
            [(fanyuchang_child(3), "16"), (fanyuchang_child(4), "15")]
        {
            assert_eq!(
                verify_atomic_duration_completion_recovery(
                    &candidate_child,
                    &receipts,
                    Some(&observation),
                    &cmi("completed", "1", "100", Some(session_time), Some("45")),
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::RemoteChanged
            );
        }

        let encoded = observation.encode().unwrap();
        let original: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let mut zero_digest = original.clone();
        zero_digest["binding_digest"] = serde_json::Value::Array(vec![serde_json::json!(0); 32]);
        for drifted in [
            serde_json::json!({"version": 2, "binding_digest": original["binding_digest"]}),
            serde_json::json!({
                "version": 1,
                "binding_digest": original["binding_digest"],
                "unexpected": true,
            }),
            zero_digest,
        ] {
            assert!(
                WellearnAtomicPreFinalObservation::decode(&serde_json::to_vec(&drifted).unwrap())
                    .is_err()
            );
        }
        assert!(WellearnAtomicPreFinalObservation::decode(&[]).is_err());
        assert!(
            WellearnAtomicPreFinalObservation::decode(&vec![
                b'x';
                MAX_ATOMIC_PRE_FINAL_OBSERVATION_BYTES
                    + 1
            ])
            .is_err()
        );
        let wrong_phase = ExecutionMutationSequenceObservation::try_new(
            2,
            WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE,
            observation.binding_digest(),
        )
        .unwrap();
        assert!(
            WellearnAtomicPreFinalObservation::from_sequence_observation(&wrong_phase).is_err()
        );
        let wrong_type = ExecutionMutationSequenceObservation::try_new(
            WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_PHASE_POSITION,
            "welearn.other-observation.v1",
            observation.binding_digest(),
        )
        .unwrap();
        assert!(WellearnAtomicPreFinalObservation::from_sequence_observation(&wrong_type).is_err());
    }

    #[test]
    fn recovery_preserves_conditional_fanyuchang_and_deterministic_auto_receipts() {
        let child = fanyuchang_child(3);
        let observation = WellearnAtomicPreFinalObservation::capture(
            &child,
            &cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
        )
        .unwrap();
        let final_cmi = cmi("completed", "1", "100", Some("15"), Some("45"));
        let terminal_rejection = WellearnAtomicDurationCompletionReceipts::new(
            true,
            vec![true, false],
            Some(false),
            true,
        );
        assert_eq!(
            verify_atomic_duration_completion_recovery(
                &child,
                &terminal_rejection,
                Some(&observation),
                &final_cmi,
            )
            .unwrap()
            .final_save_ordinal(),
            5
        );
        for invalid in [
            WellearnAtomicDurationCompletionReceipts::new(
                true,
                vec![true, false, true],
                Some(true),
                true,
            ),
            WellearnAtomicDurationCompletionReceipts::new(true, vec![true, true], Some(true), true),
        ] {
            assert!(
                verify_atomic_duration_completion_recovery(
                    &child,
                    &invalid,
                    Some(&observation),
                    &final_cmi,
                )
                .is_err()
            );
        }

        let auto = auto_child(120);
        let auto_receipts =
            WellearnAtomicDurationCompletionReceipts::new(false, vec![true, false], None, true);
        let auto_verification = verify_atomic_duration_completion_recovery(
            &auto,
            &auto_receipts,
            None,
            &cmi("completed", "1", "0", Some("87"), Some("120")),
        )
        .unwrap();
        assert_eq!(auto_verification.final_save_ordinal(), 4);
        assert_eq!(auto_verification.time_preservation_verified(), None);
        assert!(
            verify_atomic_duration_completion_recovery(
                &auto,
                &auto_receipts,
                Some(&observation),
                &cmi("completed", "1", "0", Some("87"), Some("120")),
            )
            .is_err()
        );
        assert!(WellearnAtomicPreFinalObservation::capture(&auto, &final_cmi).is_err());
    }

    #[test]
    fn sequence_records_restore_conditional_fanyuchang_recovery_without_replay() {
        let child = fanyuchang_child(3);
        let artifact = child.to_provider_execution_plan_artifact().unwrap();
        let sequence_plan = build_atomic_mutation_sequence_plan(&child, &artifact).unwrap();
        let (issues, receipts) = sequence_records(&[
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ]);
        let observation = WellearnAtomicPreFinalObservation::capture(
            &child,
            &cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let verification = verify_atomic_duration_completion_recovery_from_sequence_records(
            &child,
            &sequence_plan,
            &issues,
            &receipts,
            Some(&observation),
            &cmi("completed", "1", "100", Some("15"), Some("45")),
        )
        .unwrap();
        assert_eq!(verification.final_save_ordinal(), 5);
        assert_eq!(verification.time_preservation_verified(), Some(true));

        let foreign = fanyuchang_child(2);
        let foreign_artifact = foreign.to_provider_execution_plan_artifact().unwrap();
        let foreign_plan =
            build_atomic_mutation_sequence_plan(&foreign, &foreign_artifact).unwrap();
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_records(
                &child,
                &foreign_plan,
                &issues,
                &receipts,
                Some(&observation),
                &cmi("completed", "1", "100", Some("15"), Some("45")),
            )
            .is_err()
        );
    }

    #[test]
    fn sequence_records_reject_operation_or_ordinal_drift_and_preserve_auto_shape() {
        let auto = auto_child(120);
        let artifact = auto.to_provider_execution_plan_artifact().unwrap();
        let sequence_plan = build_atomic_mutation_sequence_plan(&auto, &artifact).unwrap();
        let (issues, receipts) = sequence_records(&[
            (WellearnAtomicMutationKind::Start, false),
            (WellearnAtomicMutationKind::ImplicitKeep, true),
            (WellearnAtomicMutationKind::ImplicitKeep, false),
            (WellearnAtomicMutationKind::Save, true),
        ]);
        let verification = verify_atomic_duration_completion_recovery_from_sequence_records(
            &auto,
            &sequence_plan,
            &issues,
            &receipts,
            None,
            &cmi("completed", "1", "0", Some("87"), Some("120")),
        )
        .unwrap();
        assert_eq!(verification.final_save_ordinal(), 4);

        let mut wrong_operation = issues.clone();
        wrong_operation[1] = ExecutionMutationIssue::new(
            2,
            WellearnAtomicMutationKind::CounterKeep.as_str(),
            [2; 32],
        )
        .unwrap();
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_records(
                &auto,
                &sequence_plan,
                &wrong_operation,
                &receipts,
                None,
                &cmi("completed", "1", "0", Some("87"), Some("120")),
            )
            .is_err()
        );

        let mut wrong_ordinal = receipts.clone();
        wrong_ordinal[1] = ExecutionMutationReceipt::new(3, [2; 32], true).unwrap();
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_records(
                &auto,
                &sequence_plan,
                &issues,
                &wrong_ordinal,
                None,
                &cmi("completed", "1", "0", Some("87"), Some("120")),
            )
            .is_err()
        );

        let mut retryable = receipts;
        retryable[1] = ExecutionMutationReceipt::new_retryable_rejection(2, [2; 32], 60).unwrap();
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_records(
                &auto,
                &sequence_plan,
                &issues,
                &retryable,
                None,
                &cmi("completed", "1", "0", Some("87"), Some("120")),
            )
            .is_err()
        );
    }

    #[test]
    fn sequence_snapshot_restores_exact_fanyuchang_and_auto_evidence() {
        let fanyuchang = fanyuchang_child(3);
        let observation = WellearnAtomicPreFinalObservation::capture(
            &fanyuchang,
            &cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let records = [
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ];
        let snapshot = sequence_snapshot(&fanyuchang, &records, Some(observation.clone()), None);
        let final_cmi = cmi("completed", "1", "100", Some("15"), Some("45"));
        let verification = verify_atomic_duration_completion_recovery_from_sequence_snapshot(
            &fanyuchang,
            &snapshot,
            &final_cmi,
        )
        .unwrap();
        assert_eq!(verification.final_save_ordinal(), 5);

        let persisted = verification
            .to_execution_mutation_verification()
            .unwrap()
            .unwrap();
        let snapshot = sequence_snapshot(&fanyuchang, &records, Some(observation), Some(persisted));
        assert_eq!(
            verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                &fanyuchang,
                &snapshot,
                &final_cmi,
            )
            .unwrap(),
            verification
        );

        let auto = auto_child(120);
        let auto_snapshot = sequence_snapshot(
            &auto,
            &[
                (WellearnAtomicMutationKind::Start, false),
                (WellearnAtomicMutationKind::ImplicitKeep, true),
                (WellearnAtomicMutationKind::ImplicitKeep, false),
                (WellearnAtomicMutationKind::Save, true),
            ],
            None,
            None,
        );
        assert_eq!(
            verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                &auto,
                &auto_snapshot,
                &cmi("completed", "1", "0", Some("87"), Some("120")),
            )
            .unwrap()
            .final_save_ordinal(),
            4
        );
    }

    #[test]
    fn sequence_snapshot_rejects_ambiguity_substitution_and_verification_drift() {
        let child = fanyuchang_child(1);
        let observation = WellearnAtomicPreFinalObservation::capture(
            &child,
            &cmi("incomplete", "0.25", "20", Some("15"), Some("45")),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let records = [
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ];
        let final_cmi = cmi("completed", "1", "100", Some("15"), Some("45"));

        let foreign = fanyuchang_child(2);
        let foreign_snapshot =
            sequence_snapshot(&foreign, &records, Some(observation.clone()), None);
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                &child,
                &foreign_snapshot,
                &final_cmi,
            )
            .is_err()
        );

        let early_verification = ExecutionMutationVerification::new(1, [9; 32], true).unwrap();
        let early_snapshot = sequence_snapshot(
            &child,
            &records,
            Some(observation.clone()),
            Some(early_verification),
        );
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                &child,
                &early_snapshot,
                &final_cmi,
            )
            .is_err()
        );

        let drifted_verification = ExecutionMutationVerification::new(4, [8; 32], true).unwrap();
        let drifted_snapshot = sequence_snapshot(
            &child,
            &records,
            Some(observation.clone()),
            Some(drifted_verification),
        );
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                &child,
                &drifted_snapshot,
                &final_cmi,
            )
            .is_err()
        );

        let artifact = child.to_provider_execution_plan_artifact().unwrap();
        let plan = build_atomic_mutation_sequence_plan(&child, &artifact).unwrap();
        let (issues, receipts) = sequence_records(&records);
        let final_index = receipts.len() - 1;
        let ambiguous_records = issues
            .into_iter()
            .zip(receipts)
            .enumerate()
            .map(|(index, (issue, receipt))| {
                ExecutionMutationRecoveryRecord::try_new(
                    issue,
                    (index != final_index).then_some(receipt),
                    None,
                )
                .unwrap()
            })
            .collect();
        let ambiguous = ExecutionMutationSequenceRecoverySnapshot::try_new(
            artifact,
            plan,
            ambiguous_records,
            vec![observation],
        )
        .unwrap();
        assert!(
            verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                &child, &ambiguous, &final_cmi,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn recovery_read_transport_uses_each_atomic_final_profile() {
        let transport = RecoveryResourceFixture::default();
        let context = ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: Vec::new(),
            correlation_id: "welearn-atomic-recovery-read".to_owned(),
        };

        for child in [fanyuchang_child(1), auto_child(60)] {
            WellearnAtomicDurationCompletionRecoveryTransport::read_atomic_final(
                &transport, &context, &child,
            )
            .await
            .unwrap();
        }

        assert_eq!(
            transport.profiles.lock().unwrap().as_slice(),
            &[
                crate::WellearnResourceMutationProfile::CurrentFullSimpleReferer,
                crate::WellearnResourceMutationProfile::LegacyMinimalTaskReferer,
            ]
        );
    }

    fn fanyuchang_child(target_seconds: u64) -> WellearnAtomicChildPlan {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "entry_index": 0,
            "course_remote_id": "course:1001",
            "remote_task_id": "sco:1001:301",
            "flow": "fanyuchang_duration",
            "execution_shape": "atomic_duration_completion",
            "atomic_completion_profile": "fanyuchang_fresh_set_save100",
            "target_seconds": target_seconds,
        }))
        .unwrap()
    }

    fn auto_child(target_seconds: u64) -> WellearnAtomicChildPlan {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "entry_index": 1,
            "course_remote_id": "course:1001",
            "remote_task_id": "sco:1001:302",
            "flow": "auto_duration",
            "execution_shape": "atomic_duration_completion",
            "atomic_completion_profile": "auto_zero_time_save_only0",
            "target_seconds": target_seconds,
        }))
        .unwrap()
    }

    fn sequence_records(
        records: &[(WellearnAtomicMutationKind, bool)],
    ) -> (Vec<ExecutionMutationIssue>, Vec<ExecutionMutationReceipt>) {
        records
            .iter()
            .enumerate()
            .map(|(index, (kind, accepted))| {
                let ordinal = u32::try_from(index + 1).unwrap();
                (
                    ExecutionMutationIssue::new(ordinal, kind.as_str(), [1; 32]).unwrap(),
                    ExecutionMutationReceipt::new(ordinal, [2; 32], *accepted).unwrap(),
                )
            })
            .unzip()
    }

    fn sequence_snapshot(
        child: &WellearnAtomicChildPlan,
        records: &[(WellearnAtomicMutationKind, bool)],
        observation: Option<ExecutionMutationSequenceObservation>,
        verification: Option<ExecutionMutationVerification>,
    ) -> ExecutionMutationSequenceRecoverySnapshot {
        let artifact = child.to_provider_execution_plan_artifact().unwrap();
        let plan = build_atomic_mutation_sequence_plan(child, &artifact).unwrap();
        let (issues, receipts) = sequence_records(records);
        let recovery_records = issues
            .into_iter()
            .zip(receipts)
            .map(|(issue, receipt)| {
                let record_verification =
                    verification.filter(|candidate| candidate.ordinal() == issue.ordinal());
                ExecutionMutationRecoveryRecord::try_new(issue, Some(receipt), record_verification)
                    .unwrap()
            })
            .collect();
        ExecutionMutationSequenceRecoverySnapshot::try_new(
            artifact,
            plan,
            recovery_records,
            observation.into_iter().collect(),
        )
        .unwrap()
    }

    fn cmi(
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
