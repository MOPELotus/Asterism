use std::fmt;

use asterism_domain::{CourseId, ExecutionId, TaskCapability, TaskId};
use asterism_provider_api::{
    ExecutionParentBatchSnapshot, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderExecutionChildPlan, ProviderResult, ResolvedProviderRuntimeSettings,
};

use crate::{
    WellearnPreparedAtomicChildPlan, WellearnPublicBatchMaterializationBinding,
    runtime_settings::WellearnRuntimeSettings,
};

const ATOMIC_CAPABILITIES: [TaskCapability; 2] = [
    TaskCapability::DurationReport,
    TaskCapability::ResourceExecution,
];

/// Exact Core materialization of one `WELearn` atomic batch child.
///
/// The encrypted parent pair and generic child plan remain Provider planning
/// authority. The Core `ExecutionRequest` additionally binds the local
/// Execution, Task, Course, frozen runtime settings and exact grouped artifact
/// created by the parent transaction. This value performs no I/O and grants no
/// mutation authority by itself.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnResolvedAtomicChildExecution {
    execution_id: ExecutionId,
    task_id: TaskId,
    course_id: CourseId,
    position: u32,
    runtime_settings: ResolvedProviderRuntimeSettings,
    prepared: WellearnPreparedAtomicChildPlan,
}

impl WellearnResolvedAtomicChildExecution {
    /// Restores the complete ordered child plan from the encrypted parent pair,
    /// selects the exact durable batch-child position and binds the materialized
    /// Core Execution request.
    ///
    /// This is the direct adapter for Core's child Execution draft: the caller
    /// supplies the resolved parent snapshot, the persisted one-based mapping
    /// position and the ordinary request reconstructed from Execution, Task,
    /// frozen settings, capability steps and Provider artifact rows.
    ///
    /// # Errors
    ///
    /// Rejects an invalid parent, missing/foreign position or every request
    /// detachment documented by [`Self::try_new`].
    pub fn try_from_parent_position(
        parent: &ExecutionParentBatchSnapshot,
        position: u32,
        request: &ExecutionRequest,
    ) -> ProviderResult<Self> {
        let plan = crate::restore_batch_execution_plan(parent)
            .map_err(|_| invalid_resolved_atomic_child())?;
        let child = plan
            .children()
            .get(
                usize::try_from(
                    position
                        .checked_sub(1)
                        .ok_or_else(invalid_resolved_atomic_child)?,
                )
                .map_err(|_| invalid_resolved_atomic_child())?,
            )
            .filter(|child| child.position() == position)
            .ok_or_else(invalid_resolved_atomic_child)?;
        Self::try_new(parent, child, request)
    }

    /// Applies the public-batch materialization binding before restoring the
    /// same exact durable child position and request.
    ///
    /// # Errors
    ///
    /// Rejects local Provider/account/Course, parent digest, Unit/SCO
    /// coordinate, remote Task, grouped capability, artifact or settings-schema
    /// drift before native execution or read-only recovery can perform I/O.
    pub fn try_from_materialization_binding(
        binding: &WellearnPublicBatchMaterializationBinding,
        context: &ProviderContext,
        parent: &ExecutionParentBatchSnapshot,
        position: u32,
        request: &ExecutionRequest,
    ) -> ProviderResult<Self> {
        binding.validate_child_dispatch(context, parent, position, request)?;
        Self::try_from_parent_position(parent, position, request)
    }

    /// Jointly restores the Provider parent/child facts and binds them to the
    /// exact Core materialized Execution request.
    ///
    /// # Errors
    ///
    /// Rejects parent/child drift, a missing Course binding, split or reordered
    /// capability authority, invalid frozen settings, remote Task drift or a
    /// missing/substituted Provider artifact.
    pub fn try_new(
        parent: &ExecutionParentBatchSnapshot,
        child: &ProviderExecutionChildPlan,
        request: &ExecutionRequest,
    ) -> ProviderResult<Self> {
        if !request.has_valid_capability_step()
            || request.requested_capabilities.as_slice() != ATOMIC_CAPABILITIES
            || request.capability_plan.as_slice() != ATOMIC_CAPABILITIES
            || request.capability_step_position != 1
            || request.remote_task_id != child.remote_task_id()
            || request.provider_plan_artifact.as_ref() != child.execution_plan().artifact()
        {
            return Err(invalid_resolved_atomic_child());
        }
        let course_id = request
            .course_id
            .ok_or_else(invalid_resolved_atomic_child)?;
        WellearnRuntimeSettings::resolve(&request.runtime_settings)
            .map_err(|_| invalid_resolved_atomic_child())?;
        let prepared =
            WellearnPreparedAtomicChildPlan::restore_from_execution_parent_batch_snapshot(
                parent, child,
            )
            .map_err(|_| invalid_resolved_atomic_child())?;
        let resolved = Self {
            execution_id: request.execution_id,
            task_id: request.task_id,
            course_id,
            position: child.position(),
            runtime_settings: request.runtime_settings.clone(),
            prepared,
        };
        resolved.validate()?;
        Ok(resolved)
    }

    pub const fn execution_id(&self) -> ExecutionId {
        self.execution_id
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn course_id(&self) -> CourseId {
        self.course_id
    }

    pub const fn position(&self) -> u32 {
        self.position
    }

    pub const fn runtime_settings(&self) -> &ResolvedProviderRuntimeSettings {
        &self.runtime_settings
    }

    pub const fn prepared(&self) -> &WellearnPreparedAtomicChildPlan {
        &self.prepared
    }

    /// Revalidates the immutable resolved value immediately before native
    /// execution or read-only recovery.
    ///
    /// # Errors
    ///
    /// Rejects settings or Provider child drift and a detached one-based
    /// position.
    pub fn validate(&self) -> ProviderResult<()> {
        WellearnRuntimeSettings::resolve(&self.runtime_settings)
            .map_err(|_| invalid_resolved_atomic_child())?;
        self.prepared
            .validate()
            .map_err(|_| invalid_resolved_atomic_child())?;
        if usize::try_from(
            self.position
                .checked_sub(1)
                .ok_or_else(invalid_resolved_atomic_child)?,
        ) != Ok(self.prepared.entry_index())
        {
            return Err(invalid_resolved_atomic_child());
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnResolvedAtomicChildExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnResolvedAtomicChildExecution")
            .field("execution_id", &self.execution_id)
            .field("task_id", &self.task_id)
            .field("course_id", &self.course_id)
            .field("position", &self.position)
            .field("runtime_settings", &"[REDACTED]")
            .field("prepared", &"[REDACTED]")
            .finish()
    }
}

fn invalid_resolved_atomic_child() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn resolved atomic child is detached from its parent or Core Execution",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::ProviderId;

    use super::*;
    use crate::{
        WellearnAtomicBatchPlanningAuthority, WellearnAutoDurationBudget, WellearnBatchFlow,
        WellearnBatchUnitSelection, WellearnScoLeavesDocument, build_batch_plan,
        parse_course_inventory, parse_task_inventory, prepare_atomic_execution_batch_plan,
        runtime_settings::runtime_settings_schema,
    };

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");

    #[test]
    fn resolved_child_binds_parent_plan_and_core_execution_identity() {
        let prepared = prepared();
        let child = &prepared.execution_batch_plan().children()[1];
        let request = execution_request(child);

        let resolved = WellearnResolvedAtomicChildExecution::try_new(
            prepared.parent_snapshot(),
            child,
            &request,
        )
        .unwrap();

        assert_eq!(resolved.execution_id(), request.execution_id);
        assert_eq!(resolved.task_id(), request.task_id);
        assert_eq!(resolved.course_id(), request.course_id.unwrap());
        assert_eq!(resolved.position(), 2);
        assert_eq!(resolved.prepared().entry_index(), 1);
        assert_eq!(
            resolved.prepared().child_plan().remote_task_id(),
            child.remote_task_id()
        );
        assert_eq!(resolved.runtime_settings(), &request.runtime_settings);
        resolved.validate().unwrap();
        let debug = format!("{resolved:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("sco:1001"));
        assert!(!debug.contains("target_seconds"));

        let direct = WellearnResolvedAtomicChildExecution::try_from_parent_position(
            prepared.parent_snapshot(),
            child.position(),
            &request,
        )
        .unwrap();
        assert_eq!(direct, resolved);
    }

    #[test]
    fn resolved_child_rejects_execution_request_detachment() {
        let prepared = prepared();
        let child = &prepared.execution_batch_plan().children()[0];

        let mut wrong_remote = execution_request(child);
        wrong_remote.remote_task_id = "sco:1001:other".to_owned();
        assert!(
            WellearnResolvedAtomicChildExecution::try_new(
                prepared.parent_snapshot(),
                child,
                &wrong_remote,
            )
            .is_err()
        );

        let mut split = execution_request(child);
        split.requested_capabilities = vec![TaskCapability::DurationReport];
        assert!(
            WellearnResolvedAtomicChildExecution::try_new(
                prepared.parent_snapshot(),
                child,
                &split,
            )
            .is_err()
        );

        let mut missing_course = execution_request(child);
        missing_course.course_id = None;
        assert!(
            WellearnResolvedAtomicChildExecution::try_new(
                prepared.parent_snapshot(),
                child,
                &missing_course,
            )
            .is_err()
        );

        let mut missing_artifact = execution_request(child);
        missing_artifact.provider_plan_artifact = None;
        assert!(
            WellearnResolvedAtomicChildExecution::try_new(
                prepared.parent_snapshot(),
                child,
                &missing_artifact,
            )
            .is_err()
        );

        let mut invalid_settings = execution_request(child);
        invalid_settings.runtime_settings.schema_version = u32::MAX;
        assert!(
            WellearnResolvedAtomicChildExecution::try_new(
                prepared.parent_snapshot(),
                child,
                &invalid_settings,
            )
            .is_err()
        );
    }

    fn prepared() -> crate::WellearnPreparedAtomicBatchPlan {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let tasks = parse_task_inventory(
            course,
            UNITS,
            &[
                WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap(),
                WellearnScoLeavesDocument::try_new(1, UNIT_ONE).unwrap(),
            ],
        )
        .unwrap();
        let batch = build_batch_plan(&tasks, WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            None,
            Some(WellearnAutoDurationBudget::try_new(1, 0, 0).unwrap()),
        )
        .unwrap();
        prepare_atomic_execution_batch_plan(&authority, &batch, None).unwrap()
    }

    fn execution_request(child: &ProviderExecutionChildPlan) -> ExecutionRequest {
        ExecutionRequest {
            execution_id: ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: child.remote_task_id().to_owned(),
            course_id: Some(CourseId::new()),
            requested_capabilities: ATOMIC_CAPABILITIES.to_vec(),
            capability_plan: ATOMIC_CAPABILITIES.to_vec(),
            capability_step_position: 1,
            runtime_settings: runtime_settings_schema().resolve(None, None, None).unwrap(),
            provider_plan_artifact: child.execution_plan().artifact().cloned(),
        }
    }

    #[test]
    fn foreign_parent_or_child_still_fails_before_core_identity_is_accepted() {
        let prepared = prepared();
        let child = &prepared.execution_batch_plan().children()[0];
        let request = execution_request(child);
        let foreign = ExecutionParentBatchSnapshot::try_new(
            ProviderId::new("other").unwrap(),
            "other.parent.v1",
            asterism_secrets::SecretValue::new(vec![1]),
            "other.batch.v1",
            asterism_secrets::SecretValue::new(vec![1]),
        )
        .unwrap();
        assert!(WellearnResolvedAtomicChildExecution::try_new(&foreign, child, &request).is_err());
    }

    #[test]
    fn parent_position_adapter_rejects_missing_or_cross_ordinal_execution_drafts() {
        let prepared = prepared();
        let first = &prepared.execution_batch_plan().children()[0];
        let second = &prepared.execution_batch_plan().children()[1];
        let first_request = execution_request(first);

        assert!(
            WellearnResolvedAtomicChildExecution::try_from_parent_position(
                prepared.parent_snapshot(),
                0,
                &first_request,
            )
            .is_err()
        );
        assert!(
            WellearnResolvedAtomicChildExecution::try_from_parent_position(
                prepared.parent_snapshot(),
                4,
                &first_request,
            )
            .is_err()
        );
        assert!(
            WellearnResolvedAtomicChildExecution::try_from_parent_position(
                prepared.parent_snapshot(),
                second.position(),
                &first_request,
            )
            .is_err()
        );

        let mut mixed_artifact = execution_request(second);
        mixed_artifact.provider_plan_artifact = first.execution_plan().artifact().cloned();
        assert!(
            WellearnResolvedAtomicChildExecution::try_from_parent_position(
                prepared.parent_snapshot(),
                second.position(),
                &mixed_artifact,
            )
            .is_err()
        );
    }
}
