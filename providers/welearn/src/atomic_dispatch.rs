use asterism_domain::{ProviderId, TaskCapability};
use asterism_provider_api::{
    ExecutionMutationSequencePlan, ExecutionParentBatchSnapshot, ProviderError, ProviderErrorKind,
    ProviderExecutionBatchPlan, ProviderExecutionChildPlan, ProviderExecutionPlan,
    ProviderExecutionPlanArtifact, ProviderResult,
};

use crate::batch_plan::{
    WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE, WELLEARN_BATCH_PLAN_SNAPSHOT_TYPE,
    WellearnAtomicBatchPlanningAuthority, WellearnAtomicChildPlan, WellearnBatchExecutionShape,
    WellearnBatchFlow, WellearnBatchPlan, materialize_atomic_child_plan_for_validated_batch,
    validate_batch_plan_integrity,
};
use crate::{build_atomic_mutation_sequence_plan, metadata::PROVIDER_ID};

/// One exact child projection kept together with its target authority, Core
/// artifact and receipt-conditional mutation sequence.
///
/// This value is planning evidence only. It does not create, restore, persist
/// or authorize a Core Execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnAtomicChildDispatchPlan {
    frozen_fanyuchang_target_seconds: Option<u64>,
    child_plan: WellearnAtomicChildPlan,
    provider_plan_artifact: ProviderExecutionPlanArtifact,
    mutation_sequence_plan: ExecutionMutationSequencePlan,
}

impl WellearnAtomicChildDispatchPlan {
    pub const fn entry_index(&self) -> u32 {
        self.child_plan.entry_index()
    }

    pub const fn frozen_fanyuchang_target_seconds(&self) -> Option<u64> {
        self.frozen_fanyuchang_target_seconds
    }

    pub const fn child_plan(&self) -> &WellearnAtomicChildPlan {
        &self.child_plan
    }

    pub const fn provider_plan_artifact(&self) -> &ProviderExecutionPlanArtifact {
        &self.provider_plan_artifact
    }

    pub const fn mutation_sequence_plan(&self) -> &ExecutionMutationSequencePlan {
        &self.mutation_sequence_plan
    }
}

/// Complete, order-preserving projection of one frozen atomic batch.
///
/// Each child target, Provider-private plan, generic Core artifact and exact
/// mutation sequence are held in one immutable item. Future Core-owned parent
/// dispatch can therefore consume one collection instead of independently
/// assembling parallel vectors that may disagree on child ordinal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnAtomicBatchDispatchPlan {
    batch_plan: WellearnBatchPlan,
    children: Vec<WellearnAtomicChildDispatchPlan>,
}

/// Provider-complete output for one future Core parent planning call.
///
/// The private snapshot and public ordered child projection are constructed
/// together and are bound by the same authority/batch digests. This value still
/// does not persist or create a Core parent or child Execution.
pub struct WellearnPreparedAtomicBatchPlan {
    parent_snapshot: ExecutionParentBatchSnapshot,
    execution_batch_plan: ProviderExecutionBatchPlan,
}

impl WellearnPreparedAtomicBatchPlan {
    pub const fn parent_snapshot(&self) -> &ExecutionParentBatchSnapshot {
        &self.parent_snapshot
    }

    pub const fn execution_batch_plan(&self) -> &ProviderExecutionBatchPlan {
        &self.execution_batch_plan
    }

    pub fn into_parts(self) -> (ExecutionParentBatchSnapshot, ProviderExecutionBatchPlan) {
        (self.parent_snapshot, self.execution_batch_plan)
    }
}

impl std::fmt::Debug for WellearnPreparedAtomicBatchPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WellearnPreparedAtomicBatchPlan")
            .field("provider_id", self.parent_snapshot.provider_id())
            .field("authority_digest", &"[HASHED]")
            .field("batch_digest", &"[HASHED]")
            .field("child_count", &self.execution_batch_plan.children().len())
            .finish()
    }
}

impl WellearnAtomicBatchDispatchPlan {
    pub const fn batch_plan(&self) -> &WellearnBatchPlan {
        &self.batch_plan
    }

    pub fn children(&self) -> &[WellearnAtomicChildDispatchPlan] {
        &self.children
    }

    /// Replays the complete projection without repairing order or authority.
    ///
    /// # Errors
    ///
    /// Returns an internal error for batch drift, missing/extra/reordered
    /// children, target-authority drift, artifact substitution or sequence
    /// substitution.
    pub fn validate(&self) -> ProviderResult<()> {
        validate_batch_plan_integrity(&self.batch_plan)?;
        if self.batch_plan.execution_shape != WellearnBatchExecutionShape::AtomicDurationCompletion
            || self.children.len() != self.batch_plan.entries.len()
        {
            return Err(invalid_atomic_batch_dispatch_plan());
        }

        for (entry_index, child) in self.children.iter().enumerate() {
            let frozen_target = match self.batch_plan.flow {
                WellearnBatchFlow::FanyuchangDuration => Some(
                    child
                        .frozen_fanyuchang_target_seconds
                        .ok_or_else(invalid_atomic_batch_dispatch_plan)?,
                ),
                WellearnBatchFlow::AutoDuration => {
                    if child.frozen_fanyuchang_target_seconds.is_some() {
                        return Err(invalid_atomic_batch_dispatch_plan());
                    }
                    None
                }
                WellearnBatchFlow::FanyuchangCompletion
                | WellearnBatchFlow::YzbrhCompletion
                | WellearnBatchFlow::YzbrhDuration
                | WellearnBatchFlow::AutoCompletion
                | WellearnBatchFlow::AutoLegacyDuration => {
                    return Err(invalid_atomic_batch_dispatch_plan());
                }
            };
            let expected_child = materialize_atomic_child_plan_for_validated_batch(
                &self.batch_plan,
                entry_index,
                frozen_target,
            )?;
            if child.child_plan != expected_child {
                return Err(invalid_atomic_batch_dispatch_plan());
            }
            let expected_artifact = expected_child.to_provider_execution_plan_artifact()?;
            if child.provider_plan_artifact != expected_artifact {
                return Err(invalid_atomic_batch_dispatch_plan());
            }
            let expected_sequence =
                build_atomic_mutation_sequence_plan(&expected_child, &expected_artifact)?;
            if child.mutation_sequence_plan != expected_sequence {
                return Err(invalid_atomic_batch_dispatch_plan());
            }
        }
        Ok(())
    }

    /// Converts the complete `WELearn` projection into Core's generic immutable
    /// parent/child batch plan.
    ///
    /// The supplied private parent snapshot is decoded and rebound to this
    /// exact complete batch before any child is projected. Current Fanyuchang's
    /// explicitly selected expected child must also retain the parent authority
    /// target. Core still owns local Task binding and transactional child
    /// Execution creation.
    ///
    /// # Errors
    ///
    /// Returns a typed error for foreign/private-type drift, parent/batch or
    /// expected-child inconsistency, invalid child positions, artifact/sequence
    /// detachment, or any generic Core batch-plan contract failure.
    pub fn to_provider_execution_batch_plan(
        &self,
        parent: &ExecutionParentBatchSnapshot,
    ) -> ProviderResult<ProviderExecutionBatchPlan> {
        self.validate()?;
        let provider_id =
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_atomic_batch_dispatch_plan())?;
        if parent.provider_id() != &provider_id
            || parent.authority_type() != WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE
            || parent.batch_type() != WELLEARN_BATCH_PLAN_SNAPSHOT_TYPE
        {
            return Err(invalid_atomic_batch_dispatch_plan());
        }

        let authority =
            WellearnAtomicBatchPlanningAuthority::decode(parent.authority().expose_secret())?;
        let batch_plan = WellearnBatchPlan::decode_snapshot(parent.batch().expose_secret())?;
        if batch_plan != self.batch_plan {
            return Err(invalid_atomic_batch_dispatch_plan());
        }
        let reconstructed = authority.to_execution_parent_batch_snapshot(&batch_plan)?;
        if reconstructed.authority_digest() != parent.authority_digest()
            || reconstructed.batch_digest() != parent.batch_digest()
        {
            return Err(invalid_atomic_batch_dispatch_plan());
        }

        if authority.flow() == WellearnBatchFlow::FanyuchangDuration {
            let expected = self
                .children
                .iter()
                .find(|child| {
                    child.child_plan().remote_task_id() == authority.expected_remote_task_id()
                })
                .ok_or_else(invalid_atomic_batch_dispatch_plan)?;
            if expected.frozen_fanyuchang_target_seconds()
                != authority.frozen_fanyuchang_target_seconds()
            {
                return Err(invalid_atomic_batch_dispatch_plan());
            }
        }

        let children = self
            .children
            .iter()
            .enumerate()
            .map(|(index, child)| {
                let position =
                    u32::try_from(index + 1).map_err(|_| invalid_atomic_batch_dispatch_plan())?;
                let execution_plan = ProviderExecutionPlan::try_new(
                    provider_id.clone(),
                    vec![vec![
                        TaskCapability::DurationReport,
                        TaskCapability::ResourceExecution,
                    ]],
                    Some(child.provider_plan_artifact().clone()),
                )?;
                ProviderExecutionChildPlan::try_new(
                    position,
                    child.child_plan().remote_task_id(),
                    execution_plan,
                    child.mutation_sequence_plan().clone(),
                )
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        ProviderExecutionBatchPlan::try_new(parent, children)
    }
}

/// Projects every child of one validated atomic batch in exact frozen order.
///
/// Current Fanyuchang requires one already-frozen bounded target per child;
/// modular Auto rejects external targets and retains each equal-floor target
/// from the complete batch, including zero. The returned value grants no Core
/// creation, persistence, recovery or mutation authority.
///
/// # Errors
///
/// Returns an internal error for a non-atomic or drifted batch, a missing,
/// extra or invalid Fanyuchang target, an external Auto target, or any child,
/// artifact or sequence projection failure.
pub fn materialize_atomic_batch_dispatch_plan(
    batch_plan: WellearnBatchPlan,
    frozen_fanyuchang_target_seconds: Option<Vec<u64>>,
) -> ProviderResult<WellearnAtomicBatchDispatchPlan> {
    validate_batch_plan_integrity(&batch_plan)?;
    if batch_plan.execution_shape != WellearnBatchExecutionShape::AtomicDurationCompletion {
        return Err(invalid_atomic_batch_dispatch_plan());
    }
    let targets = match (batch_plan.flow, frozen_fanyuchang_target_seconds) {
        (WellearnBatchFlow::FanyuchangDuration, Some(targets))
            if targets.len() == batch_plan.entries.len() =>
        {
            targets.into_iter().map(Some).collect::<Vec<_>>()
        }
        (WellearnBatchFlow::AutoDuration, None) => vec![None; batch_plan.entries.len()],
        _ => return Err(invalid_atomic_batch_dispatch_plan()),
    };

    let mut children = Vec::with_capacity(batch_plan.entries.len());
    for (entry_index, frozen_target) in targets.into_iter().enumerate() {
        let child_plan = materialize_atomic_child_plan_for_validated_batch(
            &batch_plan,
            entry_index,
            frozen_target,
        )?;
        let provider_plan_artifact = child_plan.to_provider_execution_plan_artifact()?;
        let mutation_sequence_plan =
            build_atomic_mutation_sequence_plan(&child_plan, &provider_plan_artifact)?;
        children.push(WellearnAtomicChildDispatchPlan {
            frozen_fanyuchang_target_seconds: frozen_target,
            child_plan,
            provider_plan_artifact,
            mutation_sequence_plan,
        });
    }
    let plan = WellearnAtomicBatchDispatchPlan {
        batch_plan,
        children,
    };
    plan.validate()?;
    Ok(plan)
}

/// Prepares the exact private parent snapshot and ordered Core child plan as
/// one Provider result.
///
/// # Errors
///
/// Returns the parent/batch encoding error, atomic dispatch projection error,
/// expected-child target mismatch, or generic Core batch-plan validation error.
pub fn prepare_atomic_execution_batch_plan(
    authority: &WellearnAtomicBatchPlanningAuthority,
    batch_plan: WellearnBatchPlan,
    frozen_fanyuchang_target_seconds: Option<Vec<u64>>,
) -> ProviderResult<WellearnPreparedAtomicBatchPlan> {
    let parent_snapshot = authority.to_execution_parent_batch_snapshot(&batch_plan)?;
    let dispatch_plan =
        materialize_atomic_batch_dispatch_plan(batch_plan, frozen_fanyuchang_target_seconds)?;
    let execution_batch_plan = dispatch_plan.to_provider_execution_batch_plan(&parent_snapshot)?;
    Ok(WellearnPreparedAtomicBatchPlan {
        parent_snapshot,
        execution_batch_plan,
    })
}

fn invalid_atomic_batch_dispatch_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic batch dispatch projection is invalid or cross-child mixed",
    )
}

#[cfg(test)]
mod tests {
    use asterism_provider_api::ExecutionMutationSequenceAdvanceCondition;

    use super::*;
    use crate::{
        WellearnAtomicMutationKind, WellearnScoLeavesDocument, build_batch_plan,
        parse_course_inventory, parse_task_inventory,
    };

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");

    fn tasks() -> Vec<asterism_provider_api::RemoteTask> {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        parse_task_inventory(
            course,
            UNITS,
            &[
                WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap(),
                WellearnScoLeavesDocument::try_new(1, UNIT_ONE).unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn fanyuchang_projection_keeps_every_child_target_artifact_and_sequence_together() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let targets = vec![0, 37, 19_800];
        let plan =
            materialize_atomic_batch_dispatch_plan(batch.clone(), Some(targets.clone())).unwrap();

        assert_eq!(plan.batch_plan(), &batch);
        assert_eq!(plan.children().len(), batch.entries.len());
        for (entry_index, (child, target)) in plan.children().iter().zip(targets).enumerate() {
            assert_eq!(child.entry_index(), u32::try_from(entry_index).unwrap());
            assert_eq!(child.frozen_fanyuchang_target_seconds(), Some(target));
            assert_eq!(child.child_plan().target_seconds(), target);
            assert_eq!(
                child.child_plan().remote_task_id(),
                batch.entries[entry_index].remote_task_id
            );
            assert_eq!(
                child.mutation_sequence_plan().artifact_digest(),
                child.provider_plan_artifact().artifact_digest()
            );
            assert_eq!(child.mutation_sequence_plan().phases().len(), 4);
            assert_eq!(
                child.mutation_sequence_plan().phases()[1].maximum_occurrences(),
                u32::try_from(target).unwrap()
            );
            assert!(child.mutation_sequence_plan().phases().iter().all(|phase| {
                phase.advance_condition()
                    != ExecutionMutationSequenceAdvanceCondition::AcceptedOrMaximumReached
            }));
        }
        plan.validate().unwrap();
    }

    #[test]
    fn auto_projection_uses_only_batch_equal_floor_targets_including_zero() {
        let template = tasks().remove(0);
        let mut many = Vec::with_capacity(61);
        for index in 0..61_usize {
            let mut task = template.clone();
            task.remote_id = format!("sco:1001:dispatch-{index}");
            task.normalized["sco_id"] = serde_json::json!(format!("dispatch-{index}"));
            task.normalized["sco_index"] = serde_json::json!(index);
            many.push(task);
        }
        let batch = build_batch_plan(&many, WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        let plan = materialize_atomic_batch_dispatch_plan(batch, None).unwrap();

        assert_eq!(plan.children().len(), 61);
        for child in plan.children() {
            assert_eq!(child.frozen_fanyuchang_target_seconds(), None);
            assert_eq!(child.child_plan().target_seconds(), 0);
            assert_eq!(child.mutation_sequence_plan().phases().len(), 3);
            assert_eq!(
                child.mutation_sequence_plan().phases()[1].operation_type(),
                WellearnAtomicMutationKind::ImplicitKeep.as_str()
            );
            assert_eq!(
                child.mutation_sequence_plan().phases()[1].maximum_occurrences(),
                0
            );
        }
        plan.validate().unwrap();
    }

    #[test]
    fn projection_rejects_target_shape_and_singleton_flows() {
        let fanyuchang =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        assert!(materialize_atomic_batch_dispatch_plan(fanyuchang.clone(), None).is_err());
        assert!(
            materialize_atomic_batch_dispatch_plan(fanyuchang.clone(), Some(vec![1, 2])).is_err()
        );
        assert!(
            materialize_atomic_batch_dispatch_plan(
                fanyuchang,
                Some(vec![
                    1,
                    2,
                    crate::runtime_settings::MAX_DURATION_REPORT_SECONDS + 1
                ]),
            )
            .is_err()
        );

        let auto = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        assert!(materialize_atomic_batch_dispatch_plan(auto, Some(vec![20, 20, 20])).is_err());
        let singleton = build_batch_plan(&tasks(), WellearnBatchFlow::YzbrhDuration, None).unwrap();
        assert!(materialize_atomic_batch_dispatch_plan(singleton, Some(vec![1, 1, 1])).is_err());
    }

    #[test]
    fn validation_rejects_cross_child_artifact_sequence_and_order_substitution() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let plan = materialize_atomic_batch_dispatch_plan(batch, Some(vec![1, 2, 3])).unwrap();

        let mut reordered = plan.clone();
        reordered.children.swap(0, 1);
        assert!(reordered.validate().is_err());

        let mut artifact_mixed = plan.clone();
        artifact_mixed.children[0].provider_plan_artifact =
            artifact_mixed.children[1].provider_plan_artifact.clone();
        assert!(artifact_mixed.validate().is_err());

        let mut sequence_mixed = plan.clone();
        sequence_mixed.children[0].mutation_sequence_plan =
            sequence_mixed.children[1].mutation_sequence_plan.clone();
        assert!(sequence_mixed.validate().is_err());

        let mut target_mixed = plan;
        target_mixed.children[0].frozen_fanyuchang_target_seconds = Some(2);
        assert!(target_mixed.validate().is_err());
    }

    #[test]
    fn core_batch_plan_binds_private_parent_and_every_ordered_child() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let targets = vec![0, 37, 19_800];
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            batch.course_remote_id.clone(),
            WellearnBatchFlow::FanyuchangDuration,
            batch.selection.clone(),
            batch.entries[0].remote_task_id.clone(),
            Some(targets[0]),
            None,
        )
        .unwrap();
        let parent = authority
            .to_execution_parent_batch_snapshot(&batch)
            .unwrap();
        let dispatch =
            materialize_atomic_batch_dispatch_plan(batch.clone(), Some(targets)).unwrap();

        let core = dispatch.to_provider_execution_batch_plan(&parent).unwrap();
        assert_eq!(core.provider_id().as_str(), PROVIDER_ID);
        assert_eq!(core.authority_digest(), parent.authority_digest());
        assert_eq!(core.batch_digest(), parent.batch_digest());
        assert_eq!(core.children().len(), batch.entries.len());
        for (index, child) in core.children().iter().enumerate() {
            assert_eq!(child.position(), u32::try_from(index + 1).unwrap());
            assert_eq!(child.remote_task_id(), batch.entries[index].remote_task_id);
            assert_eq!(
                child.execution_plan().calls(),
                &[vec![
                    TaskCapability::DurationReport,
                    TaskCapability::ResourceExecution,
                ]]
            );
            assert_eq!(
                child.execution_plan().artifact().unwrap().artifact_digest(),
                child.mutation_sequence_plan().artifact_digest()
            );
        }
    }

    #[test]
    fn core_batch_plan_rejects_private_snapshot_or_expected_target_substitution() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            batch.course_remote_id.clone(),
            WellearnBatchFlow::FanyuchangDuration,
            batch.selection.clone(),
            batch.entries[0].remote_task_id.clone(),
            Some(1),
            None,
        )
        .unwrap();
        let parent = authority
            .to_execution_parent_batch_snapshot(&batch)
            .unwrap();
        let dispatch =
            materialize_atomic_batch_dispatch_plan(batch, Some(vec![0, 37, 19_800])).unwrap();
        assert!(dispatch.to_provider_execution_batch_plan(&parent).is_err());

        let malformed = ExecutionParentBatchSnapshot::try_new(
            ProviderId::new(PROVIDER_ID).unwrap(),
            WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE,
            asterism_secrets::SecretValue::new(b"not-json".to_vec()),
            WELLEARN_BATCH_PLAN_SNAPSHOT_TYPE,
            asterism_secrets::SecretValue::new(b"not-json".to_vec()),
        )
        .unwrap();
        assert!(
            dispatch
                .to_provider_execution_batch_plan(&malformed)
                .is_err()
        );
    }

    #[test]
    fn prepared_core_batch_returns_digest_bound_private_and_child_parts() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            batch.course_remote_id.clone(),
            WellearnBatchFlow::FanyuchangDuration,
            batch.selection.clone(),
            batch.entries[1].remote_task_id.clone(),
            Some(37),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_execution_batch_plan(&authority, batch, Some(vec![0, 37, 19_800]))
                .unwrap();
        assert_eq!(
            prepared.parent_snapshot().authority_digest(),
            prepared.execution_batch_plan().authority_digest()
        );
        assert_eq!(
            prepared.parent_snapshot().batch_digest(),
            prepared.execution_batch_plan().batch_digest()
        );
        assert_eq!(prepared.execution_batch_plan().children().len(), 3);
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("course:1001"));
        assert!(!debug.contains("sco:1001"));
        assert!(!debug.contains("19800"));

        let (parent, children) = prepared.into_parts();
        assert_eq!(parent.authority_digest(), children.authority_digest());
        assert_eq!(parent.batch_digest(), children.batch_digest());
    }

    #[test]
    fn prepared_core_batch_accepts_auto_aggregate_without_external_targets() {
        let batch = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            batch.course_remote_id.clone(),
            WellearnBatchFlow::AutoDuration,
            batch.selection.clone(),
            batch.entries[0].remote_task_id.clone(),
            None,
            Some(crate::WellearnAutoDurationBudget::try_new(1, 0, 0).unwrap()),
        )
        .unwrap();
        let prepared = prepare_atomic_execution_batch_plan(&authority, batch, None).unwrap();
        let decoded_batch =
            WellearnBatchPlan::decode_snapshot(prepared.parent_snapshot().batch().expose_secret())
                .unwrap();
        assert_eq!(prepared.execution_batch_plan().children().len(), 3);
        assert!(
            prepared
                .execution_batch_plan()
                .children()
                .iter()
                .all(|child| {
                    WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                        child.execution_plan().artifact().unwrap(),
                        &decoded_batch,
                        usize::try_from(child.position() - 1).unwrap(),
                        None,
                    )
                    .is_ok()
                })
        );
    }

    #[test]
    fn maximum_prepared_core_batch_preserves_every_ordered_child() {
        const MAXIMUM_CHILDREN: usize = 8_192;

        let template = tasks().remove(0);
        let mut maximum = Vec::with_capacity(MAXIMUM_CHILDREN);
        for index in 0..MAXIMUM_CHILDREN {
            let mut task = template.clone();
            task.remote_id = format!("sco:1001:core-batch-{index:04}");
            task.normalized["sco_id"] = serde_json::json!(format!("core-batch-{index:04}"));
            task.normalized["sco_index"] = serde_json::json!(index);
            maximum.push(task);
        }
        let batch =
            build_batch_plan(&maximum, WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            batch.course_remote_id.clone(),
            WellearnBatchFlow::FanyuchangDuration,
            batch.selection.clone(),
            batch.entries[0].remote_task_id.clone(),
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_execution_batch_plan(&authority, batch, Some(vec![1; MAXIMUM_CHILDREN]))
                .unwrap();

        assert_eq!(
            prepared.execution_batch_plan().children().len(),
            MAXIMUM_CHILDREN
        );
        assert_eq!(
            prepared
                .execution_batch_plan()
                .children()
                .last()
                .unwrap()
                .position(),
            u32::try_from(MAXIMUM_CHILDREN).unwrap()
        );
        assert!(prepared.parent_snapshot().batch().expose_secret().len() <= 8 * 1_024 * 1_024);
    }
}
