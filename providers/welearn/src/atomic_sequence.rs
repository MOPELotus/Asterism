use asterism_provider_api::{
    ExecutionMutationSequenceAdvanceCondition, ExecutionMutationSequencePhase,
    ExecutionMutationSequencePlan, ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact,
    ProviderResult,
};

use crate::{
    WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE, WellearnAtomicChildPlan,
    WellearnAtomicCompletionProfile, WellearnAtomicMutationKind,
};

/// Stable sequence type for either donor's atomic duration-completion child.
pub const WELLEARN_ATOMIC_DURATION_COMPLETION_SEQUENCE_TYPE: &str =
    "welearn.atomic-duration-completion.v1";

/// Builds the complete receipt-conditional phase machine for one exact child.
///
/// The plan is bound to the independently durable child artifact. Fanyuchang
/// retains its response-dependent keep count and requires the hash-only
/// pre-final observation before entering the set phase. Auto retains its fixed
/// complete-minute keep count and has no time observation.
///
/// # Errors
///
/// Returns an internal error for child/artifact drift, an invalid target or an
/// impossible shared sequence-plan projection.
pub fn build_atomic_mutation_sequence_plan(
    child: &WellearnAtomicChildPlan,
    artifact: &ProviderExecutionPlanArtifact,
) -> ProviderResult<ExecutionMutationSequencePlan> {
    child.validate()?;
    if child.to_provider_execution_plan_artifact()? != *artifact {
        return Err(invalid_atomic_sequence_plan());
    }
    let plan = child.duration_completion_plan()?;
    let phases = match plan.profile() {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            let maximum_keeps =
                u32::try_from(plan.target_seconds()).map_err(|_| invalid_atomic_sequence_plan())?;
            vec![
                sequence_phase(
                    WellearnAtomicMutationKind::Start,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
                    None,
                )?,
                sequence_phase(
                    WellearnAtomicMutationKind::CounterKeep,
                    u32::from(maximum_keeps != 0),
                    maximum_keeps,
                    true,
                    ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached,
                    None,
                )?,
                sequence_phase(
                    WellearnAtomicMutationKind::Set,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                    Some(WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE),
                )?,
                sequence_phase(
                    WellearnAtomicMutationKind::Save,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                    None,
                )?,
            ]
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => {
            let keeps = u32::try_from(plan.target_seconds() / plan.heartbeat_interval_seconds())
                .map_err(|_| invalid_atomic_sequence_plan())?;
            vec![
                sequence_phase(
                    WellearnAtomicMutationKind::Start,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                    None,
                )?,
                sequence_phase(
                    WellearnAtomicMutationKind::ImplicitKeep,
                    keeps,
                    keeps,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                    None,
                )?,
                sequence_phase(
                    WellearnAtomicMutationKind::Save,
                    1,
                    1,
                    false,
                    ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                    None,
                )?,
            ]
        }
    };
    ExecutionMutationSequencePlan::try_new(
        artifact.artifact_digest(),
        WELLEARN_ATOMIC_DURATION_COMPLETION_SEQUENCE_TYPE,
        phases,
    )
    .map_err(|_| invalid_atomic_sequence_plan())
}

fn sequence_phase(
    kind: WellearnAtomicMutationKind,
    minimum_occurrences: u32,
    maximum_occurrences: u32,
    stop_repeating_after_rejection: bool,
    advance_condition: ExecutionMutationSequenceAdvanceCondition,
    required_observation_type: Option<&str>,
) -> ProviderResult<ExecutionMutationSequencePhase> {
    ExecutionMutationSequencePhase::try_new(
        kind.as_str(),
        minimum_occurrences,
        maximum_occurrences,
        stop_repeating_after_rejection,
        advance_condition,
        required_observation_type.map(str::to_owned),
    )
    .map_err(|_| invalid_atomic_sequence_plan())
}

fn invalid_atomic_sequence_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic mutation sequence plan is invalid or artifact-drifted",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanyuchang_sequence_freezes_terminal_rejection_and_pre_final_gate() {
        let child = child("fanyuchang_duration", "fanyuchang_fresh_set_save100", 3);
        let artifact = child.to_provider_execution_plan_artifact().unwrap();
        let plan = build_atomic_mutation_sequence_plan(&child, &artifact).unwrap();
        assert_eq!(
            plan.sequence_type(),
            WELLEARN_ATOMIC_DURATION_COMPLETION_SEQUENCE_TYPE
        );
        assert_eq!(plan.artifact_digest(), artifact.artifact_digest());
        assert_ne!(plan.plan_digest(), [0; 32]);
        assert_eq!(plan.phases().len(), 4);

        let start = &plan.phases()[0];
        assert_eq!(
            start.operation_type(),
            WellearnAtomicMutationKind::Start.as_str()
        );
        assert_eq!(start.minimum_occurrences(), 1);
        assert_eq!(start.maximum_occurrences(), 1);
        assert_eq!(
            start.advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached
        );

        let keep = &plan.phases()[1];
        assert_eq!(
            keep.operation_type(),
            WellearnAtomicMutationKind::CounterKeep.as_str()
        );
        assert_eq!(keep.minimum_occurrences(), 1);
        assert_eq!(keep.maximum_occurrences(), 3);
        assert!(keep.stop_repeating_after_rejection());
        assert_eq!(
            keep.advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached
        );

        let set = &plan.phases()[2];
        assert_eq!(
            set.operation_type(),
            WellearnAtomicMutationKind::Set.as_str()
        );
        assert_eq!(
            set.required_observation_type(),
            Some(WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE)
        );
        assert_eq!(
            plan.phases()[3].operation_type(),
            WellearnAtomicMutationKind::Save.as_str()
        );
        let debug = format!("{plan:?}");
        assert!(debug.contains("phase_count: 4"));
        assert!(!debug.contains(&format!("{:?}", plan.artifact_digest())));
    }

    #[test]
    fn zero_target_fanyuchang_and_auto_keep_counts_remain_distinct() {
        let fanyuchang = child("fanyuchang_duration", "fanyuchang_fresh_set_save100", 0);
        let fanyuchang_artifact = fanyuchang.to_provider_execution_plan_artifact().unwrap();
        let fanyuchang_plan =
            build_atomic_mutation_sequence_plan(&fanyuchang, &fanyuchang_artifact).unwrap();
        assert_eq!(fanyuchang_plan.phases()[1].minimum_occurrences(), 0);
        assert_eq!(fanyuchang_plan.phases()[1].maximum_occurrences(), 0);
        assert_eq!(fanyuchang_plan.phases().len(), 4);

        let auto = child("auto_duration", "auto_zero_time_save_only0", 125);
        let auto_artifact = auto.to_provider_execution_plan_artifact().unwrap();
        let auto_plan = build_atomic_mutation_sequence_plan(&auto, &auto_artifact).unwrap();
        assert_eq!(auto_plan.phases().len(), 3);
        assert_eq!(
            auto_plan.phases()[0].advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::MaximumReached
        );
        assert_eq!(auto_plan.phases()[1].minimum_occurrences(), 2);
        assert_eq!(auto_plan.phases()[1].maximum_occurrences(), 2);
        assert_eq!(auto_plan.phases()[1].required_observation_type(), None);
    }

    #[test]
    fn sequence_plan_rejects_a_foreign_child_artifact() {
        let candidate = child("auto_duration", "auto_zero_time_save_only0", 60);
        let other = child("auto_duration", "auto_zero_time_save_only0", 120);
        let artifact = other.to_provider_execution_plan_artifact().unwrap();
        assert!(build_atomic_mutation_sequence_plan(&candidate, &artifact).is_err());
    }

    fn child(flow: &str, profile: &str, target_seconds: u64) -> WellearnAtomicChildPlan {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "entry_index": 0,
            "course_remote_id": "course:1001",
            "remote_task_id": "sco:1001:301",
            "flow": flow,
            "execution_shape": "atomic_duration_completion",
            "atomic_completion_profile": profile,
            "target_seconds": target_seconds,
        }))
        .unwrap()
    }
}
