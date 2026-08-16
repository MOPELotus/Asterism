use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    BatchExecutionPlanningRequest, CourseInventoryCapability, ExecutionParentBatchSnapshot,
    PreparedProviderBatchExecutionPlan, ProviderBatchExecutionPlanningInput, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderExecutionBatchPlan, ProviderIdentity,
    ProviderMetadata, ProviderResult,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};

use crate::{
    WellearnAtomicBatchPlanningAuthority, WellearnAtomicCompletionProfile,
    WellearnAtomicDurationCompletionPlan, WellearnAutoDurationBudget, WellearnBatchFlow,
    WellearnBatchUnitSelection, WellearnCourseInventory, WellearnTaskInventory,
    build_selected_batch_plan, metadata::development_metadata, prepare_atomic_execution_batch_plan,
    restore_atomic_batch_dispatch_plan, runtime_settings::WellearnRuntimeSettings,
};

/// Namespaced Provider-private input used by Core's Course batch planner.
pub const WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_TYPE: &str = "welearn.atomic-batch-request.v1";

const WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_VERSION: u16 = 1;
const MAX_PLANNING_TARGETS: usize = 8_192;
const MAX_PLANNING_INPUT_BYTES: usize = 1_024 * 1_024;

/// Bounded product authorization for one fresh atomic Course batch plan.
///
/// The complete Fanyuchang target vector is already frozen here. Auto instead
/// carries the complete sampled aggregate budget, from which the fresh batch
/// deterministically derives every equal-floor child target.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnBatchExecutionPlanningInput {
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelection,
    expected_remote_task_id: String,
    frozen_fanyuchang_target_seconds: Option<Vec<u64>>,
    frozen_auto_duration_budget: Option<WellearnAutoDurationBudget>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WellearnBatchUnitSelectionWire {
    All,
    Explicit(Vec<u32>),
}

impl From<&WellearnBatchUnitSelection> for WellearnBatchUnitSelectionWire {
    fn from(selection: &WellearnBatchUnitSelection) -> Self {
        match selection {
            WellearnBatchUnitSelection::All => Self::All,
            WellearnBatchUnitSelection::Explicit(indices) => Self::Explicit(indices.clone()),
        }
    }
}

impl From<WellearnBatchUnitSelectionWire> for WellearnBatchUnitSelection {
    fn from(selection: WellearnBatchUnitSelectionWire) -> Self {
        match selection {
            WellearnBatchUnitSelectionWire::All => Self::All,
            WellearnBatchUnitSelectionWire::Explicit(indices) => Self::Explicit(indices),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnAutoDurationBudgetWire {
    #[serde(rename = "configured_minutes")]
    configured: u16,
    #[serde(rename = "random_range_minutes")]
    random_range: u8,
    #[serde(rename = "sampled_offset_minutes")]
    sampled_offset: i16,
    #[serde(rename = "actual_minutes")]
    actual: u16,
}

impl From<WellearnAutoDurationBudget> for WellearnAutoDurationBudgetWire {
    fn from(budget: WellearnAutoDurationBudget) -> Self {
        Self {
            configured: budget.configured_minutes(),
            random_range: budget.random_range_minutes(),
            sampled_offset: budget.sampled_offset_minutes(),
            actual: budget.actual_minutes(),
        }
    }
}

impl TryFrom<WellearnAutoDurationBudgetWire> for WellearnAutoDurationBudget {
    type Error = ProviderError;

    fn try_from(wire: WellearnAutoDurationBudgetWire) -> Result<Self, Self::Error> {
        let budget = Self::try_new(wire.configured, wire.random_range, wire.sampled_offset)?;
        if budget.actual_minutes() != wire.actual {
            return Err(invalid_planning_input());
        }
        Ok(budget)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnBatchExecutionPlanningInputWire {
    version: u16,
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelectionWire,
    expected_remote_task_id: String,
    frozen_fanyuchang_target_seconds: Option<Vec<u64>>,
    frozen_auto_duration_budget: Option<WellearnAutoDurationBudgetWire>,
}

impl WellearnBatchExecutionPlanningInput {
    /// Creates one exact atomic parent authorization before fresh discovery.
    ///
    /// # Errors
    ///
    /// Rejects unsupported flows, invalid Course/SCO identity, malformed Unit
    /// selection, target shape, target bounds or Auto budget drift.
    pub fn try_new(
        course_remote_id: impl Into<String>,
        flow: WellearnBatchFlow,
        selection: WellearnBatchUnitSelection,
        expected_remote_task_id: impl Into<String>,
        frozen_fanyuchang_target_seconds: Option<Vec<u64>>,
        frozen_auto_duration_budget: Option<WellearnAutoDurationBudget>,
    ) -> ProviderResult<Self> {
        let input = Self {
            course_remote_id: course_remote_id.into(),
            flow,
            selection,
            expected_remote_task_id: expected_remote_task_id.into(),
            frozen_fanyuchang_target_seconds,
            frozen_auto_duration_budget,
        };
        input.validate()?;
        Ok(input)
    }

    /// Serializes the private selection into Core's bounded encrypted-input
    /// wrapper. The returned digest is scheduling evidence, not authority.
    ///
    /// # Errors
    ///
    /// Rejects semantic drift, serialization failure, local size overflow or
    /// a Core Provider namespace/bound violation.
    pub fn to_provider_planning_input(
        &self,
    ) -> ProviderResult<ProviderBatchExecutionPlanningInput> {
        self.validate()?;
        let encoded = serde_json::to_vec(&WellearnBatchExecutionPlanningInputWire {
            version: WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_VERSION,
            course_remote_id: self.course_remote_id.clone(),
            flow: self.flow,
            selection: WellearnBatchUnitSelectionWire::from(&self.selection),
            expected_remote_task_id: self.expected_remote_task_id.clone(),
            frozen_fanyuchang_target_seconds: self.frozen_fanyuchang_target_seconds.clone(),
            frozen_auto_duration_budget: self.frozen_auto_duration_budget.map(Into::into),
        })
        .map_err(|_| invalid_planning_input())?;
        if encoded.is_empty() || encoded.len() > MAX_PLANNING_INPUT_BYTES {
            return Err(invalid_planning_input());
        }
        ProviderBatchExecutionPlanningInput::try_new(
            development_metadata()?.id,
            WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_TYPE,
            SecretValue::new(encoded),
        )
    }

    fn from_provider_planning_input(
        input: &ProviderBatchExecutionPlanningInput,
        metadata: &ProviderMetadata,
    ) -> ProviderResult<Self> {
        if input.provider_id() != &metadata.id
            || input.input_type() != WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_TYPE
        {
            return Err(invalid_planning_input());
        }
        let encoded = input.payload().expose_secret();
        if encoded.is_empty() || encoded.len() > MAX_PLANNING_INPUT_BYTES {
            return Err(invalid_planning_input());
        }
        let wire: WellearnBatchExecutionPlanningInputWire =
            serde_json::from_slice(encoded).map_err(|_| invalid_planning_input())?;
        if wire.version != WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_VERSION {
            return Err(invalid_planning_input());
        }
        let auto_budget = wire
            .frozen_auto_duration_budget
            .map(WellearnAutoDurationBudget::try_from)
            .transpose()
            .map_err(|_| invalid_planning_input())?;
        Self::try_new(
            wire.course_remote_id,
            wire.flow,
            wire.selection.into(),
            wire.expected_remote_task_id,
            wire.frozen_fanyuchang_target_seconds,
            auto_budget,
        )
        .map_err(|_| invalid_planning_input())
    }

    fn validate(&self) -> ProviderResult<()> {
        let anchor_target = self
            .frozen_fanyuchang_target_seconds
            .as_deref()
            .and_then(|targets| targets.first())
            .copied();
        WellearnAtomicBatchPlanningAuthority::try_new(
            self.course_remote_id.clone(),
            self.flow,
            self.selection.clone(),
            self.expected_remote_task_id.clone(),
            anchor_target,
            self.frozen_auto_duration_budget,
        )
        .map_err(|_| invalid_planning_input())?;

        match self.flow {
            WellearnBatchFlow::FanyuchangDuration => {
                let targets = self
                    .frozen_fanyuchang_target_seconds
                    .as_deref()
                    .ok_or_else(invalid_planning_input)?;
                if targets.is_empty() || targets.len() > MAX_PLANNING_TARGETS {
                    return Err(invalid_planning_input());
                }
                for target in targets {
                    WellearnAtomicDurationCompletionPlan::try_new(
                        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                        *target,
                    )
                    .map_err(|_| invalid_planning_input())?;
                }
            }
            WellearnBatchFlow::AutoDuration => {
                if self.frozen_fanyuchang_target_seconds.is_some() {
                    return Err(invalid_planning_input());
                }
            }
            WellearnBatchFlow::FanyuchangCompletion
            | WellearnBatchFlow::YzbrhCompletion
            | WellearnBatchFlow::YzbrhDuration
            | WellearnBatchFlow::AutoCompletion
            | WellearnBatchFlow::AutoLegacyDuration => return Err(invalid_planning_input()),
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnBatchExecutionPlanningInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnBatchExecutionPlanningInput")
            .field("flow", &self.flow)
            .field("course_remote_id", &"[REDACTED]")
            .field("selection", &"[REDACTED]")
            .field("expected_remote_task_id", &"[REDACTED]")
            .field("frozen_fanyuchang_target_seconds", &"[REDACTED]")
            .field("frozen_auto_duration_budget", &"[REDACTED]")
            .finish()
    }
}

/// Fresh read-only planner used by the registered `WELearn` `TaskExecution`
/// slot.
pub struct WellearnBatchExecutionPlanner {
    metadata: ProviderMetadata,
    courses: Arc<WellearnCourseInventory>,
    tasks: Arc<WellearnTaskInventory>,
}

impl WellearnBatchExecutionPlanner {
    /// Binds planning to the same complete Course and Task inventory
    /// capabilities exposed by the Provider entry.
    ///
    /// # Errors
    ///
    /// Rejects invalid compile-time metadata or mismatched inventory
    /// capabilities.
    pub fn try_new(
        courses: Arc<WellearnCourseInventory>,
        tasks: Arc<WellearnTaskInventory>,
    ) -> ProviderResult<Self> {
        let metadata = development_metadata()?;
        if courses.metadata() != &metadata || tasks.metadata() != &metadata {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn batch planner has mismatched inventory metadata",
            ));
        }
        Ok(Self {
            metadata,
            courses,
            tasks,
        })
    }

    /// Performs exactly one fresh Course list and one complete Unit/SCO scan,
    /// then returns the encrypted parent pair and all ordered children.
    ///
    /// # Errors
    ///
    /// Rejects context/input/capability drift, invalid resolved settings,
    /// incomplete fresh inventory, membership/target drift or detached child
    /// cardinality.
    pub async fn prepare(
        &self,
        context: &ProviderContext,
        request: &BatchExecutionPlanningRequest<'_>,
    ) -> ProviderResult<PreparedProviderBatchExecutionPlan> {
        if context.provider_id != self.metadata.id
            || request.planning_input.provider_id() != &self.metadata.id
            || !atomic_capability_selection(request.requested_capabilities)
        {
            return Err(invalid_planning_request());
        }
        WellearnRuntimeSettings::resolve(request.runtime_settings)?;
        let input = WellearnBatchExecutionPlanningInput::from_provider_planning_input(
            request.planning_input,
            &self.metadata,
        )?;
        if input.course_remote_id != request.remote_course_id {
            return Err(remote_changed());
        }

        let courses = self.courses.list_courses(context).await?;
        let course = exact_course(&courses, request.remote_course_id)?;
        let (fresh_tasks, fresh_units) = self.tasks.list_tasks_and_units(context, course).await?;
        let batch = build_selected_batch_plan(
            &fresh_tasks,
            &fresh_units,
            input.selection.clone(),
            input.flow,
            input
                .frozen_auto_duration_budget
                .map(|budget| u64::from(budget.actual_minutes())),
        )?;
        if batch.course_remote_id != request.remote_course_id {
            return Err(remote_changed());
        }
        let expected_index = batch
            .entries
            .iter()
            .position(|entry| entry.remote_task_id == input.expected_remote_task_id)
            .ok_or_else(remote_changed)?;
        let expected_target = input
            .frozen_fanyuchang_target_seconds
            .as_deref()
            .and_then(|targets| targets.get(expected_index))
            .copied();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            input.course_remote_id,
            input.flow,
            input.selection,
            input.expected_remote_task_id,
            expected_target,
            input.frozen_auto_duration_budget,
        )?;
        let prepared = prepare_atomic_execution_batch_plan(
            &authority,
            &batch,
            input.frozen_fanyuchang_target_seconds.as_deref(),
        )?;
        let (parent, children) = prepared.into_parts();
        PreparedProviderBatchExecutionPlan::try_new(parent, children, request.expected_child_count)
    }
}

impl fmt::Debug for WellearnBatchExecutionPlanner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnBatchExecutionPlanner")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("tasks", &"configured")
            .finish()
    }
}

/// Restores every child deterministically from only the resolved encrypted
/// parent pair. This path deliberately has no inventory or transport input.
///
/// # Errors
///
/// Rejects a foreign, malformed, digest-drifted or internally inconsistent
/// parent pair or child projection.
pub fn restore_batch_execution_plan(
    parent: &ExecutionParentBatchSnapshot,
) -> ProviderResult<ProviderExecutionBatchPlan> {
    let dispatch = restore_atomic_batch_dispatch_plan(parent)?;
    dispatch.to_provider_execution_batch_plan(parent)
}

fn atomic_capability_selection(capabilities: &[TaskCapability]) -> bool {
    capabilities.len() == 2
        && capabilities.iter().copied().collect::<BTreeSet<_>>()
            == BTreeSet::from([
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ])
}

fn exact_course<'a>(
    courses: &'a [asterism_provider_api::RemoteCourse],
    remote_course_id: &str,
) -> ProviderResult<&'a asterism_provider_api::RemoteCourse> {
    let mut matches = courses
        .iter()
        .filter(|course| course.remote_id == remote_course_id);
    let course = matches.next().ok_or_else(remote_changed)?;
    if matches.next().is_some() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn fresh batch inventory contains a duplicate Course identity",
        ));
    }
    Ok(course)
}

fn invalid_planning_input() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "WELearn batch execution planning input is invalid",
    )
}

fn invalid_planning_request() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::UnsupportedTask,
        "WELearn batch execution planning request is unsupported",
    )
}

fn remote_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "WELearn fresh Course batch no longer matches its authorization",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{
        BatchExecutionAttemptId, BatchExecutionId, CourseId, ProviderAccountId, ProviderId,
        SecretId,
    };
    use asterism_provider_api::{
        ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderRuntimeSettingsSchema,
        RemoteCourse, TaskExecutionCapability,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::{
        WellearnCourseInventoryTransport, WellearnInventoryDocument, WellearnScoLeavesDocument,
        WellearnTaskExecution, WellearnTaskInventoryDocuments, WellearnTaskInventoryTransport,
        runtime_settings::runtime_settings_schema,
    };

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");

    #[derive(Debug)]
    struct FixtureCourseTransport {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WellearnCourseInventoryTransport for FixtureCourseTransport {
        async fn fetch_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnInventoryDocument> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            WellearnInventoryDocument::try_new(COURSES)
        }
    }

    #[derive(Debug)]
    struct FixtureTaskTransport {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WellearnTaskInventoryTransport for FixtureTaskTransport {
        async fn fetch_tasks(
            &self,
            _context: &ProviderContext,
            _course: &RemoteCourse,
        ) -> ProviderResult<WellearnTaskInventoryDocuments> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(WellearnTaskInventoryDocuments::new(
                WellearnInventoryDocument::try_new(UNITS)?,
                vec![
                    WellearnScoLeavesDocument::try_new(0, UNIT_ZERO)?,
                    WellearnScoLeavesDocument::try_new(1, UNIT_ONE)?,
                ],
            ))
        }
    }

    #[derive(Debug)]
    struct UnsupportedExecution {
        metadata: ProviderMetadata,
    }

    impl ProviderIdentity for UnsupportedExecution {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskExecutionCapability for UnsupportedExecution {
        async fn execute(
            &self,
            _context: &ProviderContext,
            _request: &ExecutionRequest,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ExecutionOutcome> {
            Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "fixture does not execute mutations",
            ))
        }
    }

    #[tokio::test]
    async fn fresh_contract_scans_once_and_restore_uses_only_parent_pair() {
        let course_transport = Arc::new(FixtureCourseTransport {
            calls: AtomicUsize::new(0),
        });
        let task_transport = Arc::new(FixtureTaskTransport {
            calls: AtomicUsize::new(0),
        });
        let courses = Arc::new(WellearnCourseInventory::try_new(course_transport.clone()).unwrap());
        let tasks = Arc::new(WellearnTaskInventory::try_new(task_transport.clone()).unwrap());
        let planner = Arc::new(WellearnBatchExecutionPlanner::try_new(courses, tasks).unwrap());
        let placeholder = Arc::new(UnsupportedExecution {
            metadata: development_metadata().unwrap(),
        });
        let execution = WellearnTaskExecution::try_new_with_batch_planner(
            placeholder.clone(),
            placeholder,
            planner,
        )
        .unwrap();
        let private = WellearnBatchExecutionPlanningInput::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:302",
            Some(vec![0, 37, 19_800]),
            None,
        )
        .unwrap();
        let input = private.to_provider_planning_input().unwrap();
        let settings = settings();
        let capabilities = [
            TaskCapability::ResourceExecution,
            TaskCapability::DurationReport,
        ];
        let request = request(&input, &settings, &capabilities, 3);

        let prepared = execution
            .prepare_batch_execution_plan(&context(), &request)
            .await
            .unwrap();
        assert_eq!(course_transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(task_transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.execution_batch_plan().children().len(), 3);
        assert_eq!(
            prepared.execution_batch_plan().children()[1].remote_task_id(),
            "sco:1001:302"
        );

        let restored = execution
            .restore_batch_execution_plan(prepared.parent_snapshot())
            .unwrap();
        assert_eq!(restored, *prepared.execution_batch_plan());
        assert_eq!(course_transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(task_transport.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn auto_input_preserves_sampled_budget_and_derives_all_children() {
        let courses = Arc::new(
            WellearnCourseInventory::try_new(Arc::new(FixtureCourseTransport {
                calls: AtomicUsize::new(0),
            }))
            .unwrap(),
        );
        let tasks = Arc::new(
            WellearnTaskInventory::try_new(Arc::new(FixtureTaskTransport {
                calls: AtomicUsize::new(0),
            }))
            .unwrap(),
        );
        let planner = WellearnBatchExecutionPlanner::try_new(courses, tasks).unwrap();
        let private = WellearnBatchExecutionPlanningInput::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            None,
            Some(WellearnAutoDurationBudget::try_new(2, 1, -1).unwrap()),
        )
        .unwrap();
        let input = private.to_provider_planning_input().unwrap();
        let settings = settings();
        let capabilities = [
            TaskCapability::DurationReport,
            TaskCapability::ResourceExecution,
        ];
        let request = request(&input, &settings, &capabilities, 3);
        let prepared = planner.prepare(&context(), &request).await.unwrap();

        assert_eq!(prepared.execution_batch_plan().children().len(), 3);
        let restored = restore_batch_execution_plan(prepared.parent_snapshot()).unwrap();
        assert_eq!(restored, *prepared.execution_batch_plan());
        assert!(format!("{private:?}").contains("[REDACTED]"));
        assert!(!format!("{private:?}").contains("sco:1001"));
    }

    #[tokio::test]
    async fn invalid_namespace_and_target_or_child_shape_fail_closed() {
        assert!(
            WellearnBatchExecutionPlanningInput::try_new(
                "course:1001",
                WellearnBatchFlow::FanyuchangDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                Some(Vec::new()),
                None,
            )
            .is_err()
        );
        assert!(
            WellearnBatchExecutionPlanningInput::try_new(
                "course:1001",
                WellearnBatchFlow::YzbrhDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                None,
                None,
            )
            .is_err()
        );

        let planner = WellearnBatchExecutionPlanner::try_new(
            Arc::new(
                WellearnCourseInventory::try_new(Arc::new(FixtureCourseTransport {
                    calls: AtomicUsize::new(0),
                }))
                .unwrap(),
            ),
            Arc::new(
                WellearnTaskInventory::try_new(Arc::new(FixtureTaskTransport {
                    calls: AtomicUsize::new(0),
                }))
                .unwrap(),
            ),
        )
        .unwrap();
        let private = WellearnBatchExecutionPlanningInput::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(vec![1, 2, 3]),
            None,
        )
        .unwrap();
        let valid = private.to_provider_planning_input().unwrap();
        let foreign = ProviderBatchExecutionPlanningInput::try_new(
            ProviderId::new("welearn").unwrap(),
            "welearn.foreign-request.v1",
            SecretValue::new(valid.payload().expose_secret().to_vec()),
        )
        .unwrap();
        let settings = settings();
        let capabilities = [
            TaskCapability::DurationReport,
            TaskCapability::ResourceExecution,
        ];
        assert!(
            planner
                .prepare(&context(), &request(&foreign, &settings, &capabilities, 3))
                .await
                .is_err()
        );
        assert!(
            planner
                .prepare(&context(), &request(&valid, &settings, &capabilities, 2))
                .await
                .is_err()
        );
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-batch-planning".to_owned(),
        }
    }

    fn settings() -> asterism_provider_api::ResolvedProviderRuntimeSettings {
        let schema: ProviderRuntimeSettingsSchema = runtime_settings_schema();
        schema.resolve(None, None, None).unwrap()
    }

    fn request<'a>(
        input: &'a ProviderBatchExecutionPlanningInput,
        settings: &'a asterism_provider_api::ResolvedProviderRuntimeSettings,
        capabilities: &'a [TaskCapability],
        expected_child_count: u32,
    ) -> BatchExecutionPlanningRequest<'a> {
        BatchExecutionPlanningRequest {
            batch_execution_id: BatchExecutionId::new(),
            attempt_id: BatchExecutionAttemptId::new(),
            course_id: CourseId::new(),
            remote_course_id: "course:1001",
            requested_capabilities: capabilities,
            expected_child_count,
            runtime_settings: settings,
            planning_input: input,
        }
    }
}
