use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{ProviderId, RemoteState, TaskCapability};
use asterism_provider_api::{
    ExecutionParentBatchSnapshot, ProviderError, ProviderErrorKind, ProviderExecutionChildPlan,
    ProviderExecutionPlanArtifact, ProviderResult, RemoteTask, RemoteTaskDetail,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    WellearnUnitObservation, metadata::PROVIDER_ID, task_detail::validate_fresh_execution_detail,
};

/// Namespaced Core artifact type for one version-one atomic child plan.
pub const WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE: &str = "welearn.atomic-child.v1";

/// Namespaced Provider-private type for one complete frozen batch snapshot.
pub const WELLEARN_BATCH_PLAN_SNAPSHOT_TYPE: &str = "welearn.batch-plan.v1";

/// Namespaced Provider-private type for one frozen atomic parent authority.
pub const WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE: &str =
    "welearn.atomic-batch-planning-authority.v2";

/// Namespaced Provider-private type for one target-authorized atomic batch.
pub const WELLEARN_ATOMIC_BATCH_SNAPSHOT_TYPE: &str = "welearn.atomic-batch-snapshot.v2";

/// Audited donor batch flow. This is a pure membership/target boundary; it
/// does not create or schedule Core executions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WellearnBatchFlow {
    FanyuchangCompletion,
    FanyuchangDuration,
    YzbrhCompletion,
    YzbrhDuration,
    AutoCompletion,
    AutoDuration,
    AutoLegacyDuration,
}

/// Donor dispatch behavior that the shared parent/child execution layer must
/// persist with the selected batch flow. This remains a Provider-private
/// semantic value; it does not schedule child executions itself.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WellearnBatchDispatch {
    PerChildConcurrent,
    Sequential,
    BoundedThreadPool,
}

/// Target allocation contract for a frozen donor batch. The shared durable
/// layer uses this fact to persist either each child target or the aggregate
/// derivation without asking the Provider to resample after recovery.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WellearnBatchTargetStrategy {
    PerChild,
    SharedConfigured,
    AggregateEqualFloor,
}

/// Capability authority shape required by one donor batch child. Atomic
/// duration-completion flows remain a Provider fact until Core can authorize
/// and recover the combined mutation without crossing singleton step authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WellearnBatchExecutionShape {
    ResourceExecution,
    DurationReport,
    AtomicDurationCompletion,
}

/// Exact final completion mutation attached to an atomic duration child.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WellearnAtomicCompletionProfile {
    /// Current Fanyuchang carries fresh time into CMI, then saves score 100 once.
    FanyuchangFreshSetSave100,
    /// Modular Auto omits `setscoinfo` and saves completed/progress 100/score 0.
    AutoZeroTimeSaveOnly0,
}

/// Unit selection shape frozen before donor-specific SCO filtering. Explicit
/// indices preserve the caller's order, which current Fanyuchang accepts for
/// comma-separated multi-Unit selections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WellearnBatchUnitSelection {
    All,
    Explicit(Vec<u32>),
}

const MAX_BATCH_TASKS: usize = 8_192;
const MIN_AUTO_CONFIGURED_DURATION_MINUTES: u16 = 1;
const MAX_AUTO_CONFIGURED_DURATION_MINUTES: u16 = 300;
const MAX_AUTO_DURATION_RANDOM_RANGE_MINUTES: u8 = 30;
const MAX_AUTO_DURATION_MINUTES: u64 = 330;
const MAX_BATCH_ID_COMPONENT_BYTES: usize = 128;
const WELLEARN_BATCH_PLAN_SNAPSHOT_VERSION: u16 = 1;
const MAX_BATCH_PLAN_SNAPSHOT_BYTES: usize = 8 * 1_024 * 1_024;
const WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_VERSION: u16 = 1;
const WELLEARN_COMPLETE_ATOMIC_BATCH_AUTHORITY_VERSION: u16 = 2;
const WELLEARN_COMPLETE_ATOMIC_BATCH_SNAPSHOT_VERSION: u16 = 2;
const MAX_ATOMIC_BATCH_PLANNING_AUTHORITY_BYTES: usize = 4_096;
const WELLEARN_ATOMIC_CHILD_PLAN_VERSION: u16 = 1;
const MAX_ATOMIC_CHILD_PLAN_BYTES: usize = 1_024;

impl WellearnBatchFlow {
    pub const fn dispatch(self) -> WellearnBatchDispatch {
        match self {
            Self::FanyuchangCompletion | Self::FanyuchangDuration | Self::YzbrhDuration => {
                WellearnBatchDispatch::PerChildConcurrent
            }
            Self::YzbrhCompletion | Self::AutoCompletion | Self::AutoLegacyDuration => {
                WellearnBatchDispatch::Sequential
            }
            Self::AutoDuration => WellearnBatchDispatch::BoundedThreadPool,
        }
    }

    pub const fn target_strategy(self) -> WellearnBatchTargetStrategy {
        match self {
            Self::AutoCompletion
            | Self::FanyuchangCompletion
            | Self::FanyuchangDuration
            | Self::YzbrhCompletion
            | Self::YzbrhDuration
            | Self::AutoLegacyDuration => WellearnBatchTargetStrategy::PerChild,
            Self::AutoDuration => WellearnBatchTargetStrategy::AggregateEqualFloor,
        }
    }

    const fn requires_raw_sco_visibility(self) -> bool {
        matches!(
            self,
            Self::YzbrhCompletion
                | Self::AutoCompletion
                | Self::AutoDuration
                | Self::AutoLegacyDuration
        )
    }

    const fn skips_completed(self) -> bool {
        matches!(self, Self::YzbrhCompletion | Self::AutoCompletion)
    }

    const fn requires_pending_completion(self) -> bool {
        matches!(self, Self::YzbrhCompletion | Self::AutoCompletion)
    }

    pub const fn execution_shape(self) -> WellearnBatchExecutionShape {
        match self {
            Self::FanyuchangCompletion | Self::YzbrhCompletion | Self::AutoCompletion => {
                WellearnBatchExecutionShape::ResourceExecution
            }
            Self::YzbrhDuration | Self::AutoLegacyDuration => {
                WellearnBatchExecutionShape::DurationReport
            }
            Self::FanyuchangDuration | Self::AutoDuration => {
                WellearnBatchExecutionShape::AtomicDurationCompletion
            }
        }
    }

    pub const fn atomic_completion_profile(self) -> Option<WellearnAtomicCompletionProfile> {
        match self {
            Self::FanyuchangDuration => {
                Some(WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100)
            }
            Self::AutoDuration => Some(WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0),
            Self::FanyuchangCompletion
            | Self::YzbrhCompletion
            | Self::YzbrhDuration
            | Self::AutoCompletion
            | Self::AutoLegacyDuration => None,
        }
    }

    const fn required_capabilities(self) -> &'static [TaskCapability] {
        match self.execution_shape() {
            WellearnBatchExecutionShape::ResourceExecution => &[TaskCapability::ResourceExecution],
            WellearnBatchExecutionShape::DurationReport => &[TaskCapability::DurationReport],
            WellearnBatchExecutionShape::AtomicDurationCompletion => &[
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ],
        }
    }

    fn validate_unit_selection(self, selection: &WellearnBatchUnitSelection) -> ProviderResult<()> {
        let WellearnBatchUnitSelection::Explicit(indices) = selection else {
            return Ok(());
        };
        if matches!(
            self,
            Self::YzbrhCompletion | Self::YzbrhDuration | Self::AutoLegacyDuration
        ) && indices.len() != 1
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn donor flow accepts one explicit Unit or all Units",
            ));
        }
        if matches!(self, Self::AutoCompletion | Self::AutoDuration)
            && !indices.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn Auto Unit selection must preserve fresh response order",
            ));
        }
        Ok(())
    }
}

/// One frozen child selection. The Core batch layer owns durable child
/// Execution creation; this value only records the Provider-side target facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnBatchEntry {
    pub remote_task_id: String,
    pub unit_index: u32,
    pub sco_index: usize,
    pub unit_visible: bool,
    pub sco_visible: Option<bool>,
    pub visible: bool,
    pub completion: RemoteState,
    pub target_seconds: Option<u64>,
}

/// A bounded, deterministic donor batch plan. `discarded_remainder_seconds`
/// is intentionally retained for Auto's integer division semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnBatchPlan {
    pub course_remote_id: String,
    pub flow: WellearnBatchFlow,
    pub dispatch: WellearnBatchDispatch,
    pub target_strategy: WellearnBatchTargetStrategy,
    pub execution_shape: WellearnBatchExecutionShape,
    pub atomic_completion_profile: Option<WellearnAtomicCompletionProfile>,
    pub selection: WellearnBatchUnitSelection,
    pub selected_units: Vec<WellearnUnitObservation>,
    pub entries: Vec<WellearnBatchEntry>,
    pub aggregate_duration_seconds: Option<u64>,
    pub discarded_remainder_seconds: u64,
}

/// Complete modular `Auto_WeLearn` parent-budget selection.
///
/// The donor samples one signed offset for the whole selected batch and then
/// divides the resulting aggregate across every eligible child. Retaining the
/// inputs and sampled offset prevents a restored parent plan from presenting
/// an unexplained final minute value as valid donor configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnAutoDurationBudget {
    configured: u16,
    random_range: u8,
    sampled_offset: i16,
    actual: u16,
}

impl WellearnAutoDurationBudget {
    /// Validates the donor's 1..=300 minute base, 0..=30 minute random range,
    /// and one already-frozen signed sample from that inclusive range.
    ///
    /// # Errors
    ///
    /// Returns an internal error when any restored setting or sample is
    /// outside the audited UI/worker bounds.
    pub fn try_new(
        configured_minutes: u16,
        random_range_minutes: u8,
        sampled_offset_minutes: i16,
    ) -> ProviderResult<Self> {
        if !(MIN_AUTO_CONFIGURED_DURATION_MINUTES..=MAX_AUTO_CONFIGURED_DURATION_MINUTES)
            .contains(&configured_minutes)
            || random_range_minutes > MAX_AUTO_DURATION_RANDOM_RANGE_MINUTES
            || sampled_offset_minutes.unsigned_abs() > u16::from(random_range_minutes)
        {
            return Err(invalid_auto_duration_budget());
        }
        let actual_minutes = (i32::from(configured_minutes) + i32::from(sampled_offset_minutes))
            .max(i32::from(MIN_AUTO_CONFIGURED_DURATION_MINUTES));
        let actual_minutes =
            u16::try_from(actual_minutes).map_err(|_| invalid_auto_duration_budget())?;
        if u64::from(actual_minutes) > MAX_AUTO_DURATION_MINUTES {
            return Err(invalid_auto_duration_budget());
        }
        Ok(Self {
            configured: configured_minutes,
            random_range: random_range_minutes,
            sampled_offset: sampled_offset_minutes,
            actual: actual_minutes,
        })
    }

    pub const fn configured_minutes(self) -> u16 {
        self.configured
    }

    pub const fn random_range_minutes(self) -> u8 {
        self.random_range
    }

    pub const fn sampled_offset_minutes(self) -> i16 {
        self.sampled_offset
    }

    pub const fn actual_minutes(self) -> u16 {
        self.actual
    }
}

/// Explicit parent authority required before a fresh atomic Course batch can
/// be planned. None of these facts may be inferred from one child Task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnAtomicBatchPlanningAuthority {
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelection,
    expected_remote_task_id: String,
    frozen_fanyuchang_target_seconds: Option<u64>,
    frozen_auto_duration_budget: Option<WellearnAutoDurationBudget>,
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
struct WellearnUnitObservationWire {
    index: u32,
    title: String,
    code: Option<String>,
    visible: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnBatchEntryWire {
    remote_task_id: String,
    unit_index: u32,
    sco_index: u32,
    unit_visible: bool,
    sco_visible: Option<bool>,
    visible: bool,
    completion: RemoteState,
    target_seconds: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnBatchPlanWire {
    version: u16,
    course_remote_id: String,
    flow: WellearnBatchFlow,
    dispatch: WellearnBatchDispatch,
    target_strategy: WellearnBatchTargetStrategy,
    execution_shape: WellearnBatchExecutionShape,
    atomic_completion_profile: Option<WellearnAtomicCompletionProfile>,
    selection: WellearnBatchUnitSelectionWire,
    selected_units: Vec<WellearnUnitObservationWire>,
    entries: Vec<WellearnBatchEntryWire>,
    aggregate_duration_seconds: Option<u64>,
    discarded_remainder_seconds: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnCompleteAtomicBatchSnapshotWire {
    version: u16,
    batch_plan: WellearnBatchPlanWire,
    frozen_fanyuchang_target_seconds: Option<Vec<u64>>,
}

impl TryFrom<&WellearnBatchPlan> for WellearnBatchPlanWire {
    type Error = ProviderError;

    fn try_from(plan: &WellearnBatchPlan) -> Result<Self, Self::Error> {
        validate_batch_plan_integrity(plan)?;
        let entries = plan
            .entries
            .iter()
            .map(|entry| {
                Ok(WellearnBatchEntryWire {
                    remote_task_id: entry.remote_task_id.clone(),
                    unit_index: entry.unit_index,
                    sco_index: u32::try_from(entry.sco_index)
                        .map_err(|_| invalid_serialized_batch_plan())?,
                    unit_visible: entry.unit_visible,
                    sco_visible: entry.sco_visible,
                    visible: entry.visible,
                    completion: entry.completion,
                    target_seconds: entry.target_seconds,
                })
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        Ok(Self {
            version: WELLEARN_BATCH_PLAN_SNAPSHOT_VERSION,
            course_remote_id: plan.course_remote_id.clone(),
            flow: plan.flow,
            dispatch: plan.dispatch,
            target_strategy: plan.target_strategy,
            execution_shape: plan.execution_shape,
            atomic_completion_profile: plan.atomic_completion_profile,
            selection: WellearnBatchUnitSelectionWire::from(&plan.selection),
            selected_units: plan
                .selected_units
                .iter()
                .map(|unit| WellearnUnitObservationWire {
                    index: unit.index,
                    title: unit.title.clone(),
                    code: unit.code.clone(),
                    visible: unit.visible,
                })
                .collect(),
            entries,
            aggregate_duration_seconds: plan.aggregate_duration_seconds,
            discarded_remainder_seconds: plan.discarded_remainder_seconds,
        })
    }
}

impl TryFrom<WellearnBatchPlanWire> for WellearnBatchPlan {
    type Error = ProviderError;

    fn try_from(wire: WellearnBatchPlanWire) -> Result<Self, Self::Error> {
        if wire.version != WELLEARN_BATCH_PLAN_SNAPSHOT_VERSION {
            return Err(invalid_serialized_batch_plan());
        }
        let plan = Self {
            course_remote_id: wire.course_remote_id,
            flow: wire.flow,
            dispatch: wire.dispatch,
            target_strategy: wire.target_strategy,
            execution_shape: wire.execution_shape,
            atomic_completion_profile: wire.atomic_completion_profile,
            selection: wire.selection.into(),
            selected_units: wire
                .selected_units
                .into_iter()
                .map(|unit| WellearnUnitObservation {
                    index: unit.index,
                    title: unit.title,
                    code: unit.code,
                    visible: unit.visible,
                })
                .collect(),
            entries: wire
                .entries
                .into_iter()
                .map(|entry| {
                    Ok(WellearnBatchEntry {
                        remote_task_id: entry.remote_task_id,
                        unit_index: entry.unit_index,
                        sco_index: usize::try_from(entry.sco_index)
                            .map_err(|_| invalid_serialized_batch_plan())?,
                        unit_visible: entry.unit_visible,
                        sco_visible: entry.sco_visible,
                        visible: entry.visible,
                        completion: entry.completion,
                        target_seconds: entry.target_seconds,
                    })
                })
                .collect::<ProviderResult<Vec<_>>>()?,
            aggregate_duration_seconds: wire.aggregate_duration_seconds,
            discarded_remainder_seconds: wire.discarded_remainder_seconds,
        };
        validate_batch_plan_integrity(&plan).map_err(|_| invalid_serialized_batch_plan())?;
        Ok(plan)
    }
}

impl WellearnBatchPlan {
    /// Encodes one complete validated batch without credentials or route facts.
    ///
    /// # Errors
    ///
    /// Returns an internal error when plan integrity, serialization or the
    /// Provider's eight-MiB parent snapshot bound fails.
    pub fn encode_snapshot(&self) -> ProviderResult<Vec<u8>> {
        let wire = WellearnBatchPlanWire::try_from(self)?;
        let encoded = serde_json::to_vec(&wire).map_err(|_| invalid_serialized_batch_plan())?;
        if encoded.is_empty() || encoded.len() > MAX_BATCH_PLAN_SNAPSHOT_BYTES {
            return Err(invalid_serialized_batch_plan());
        }
        Ok(encoded)
    }

    /// Restores and fully revalidates one complete bounded batch snapshot.
    ///
    /// # Errors
    ///
    /// Returns an internal error for empty, oversized, malformed,
    /// version-drifted or semantically inconsistent bytes.
    pub fn decode_snapshot(encoded: &[u8]) -> ProviderResult<Self> {
        if encoded.is_empty() || encoded.len() > MAX_BATCH_PLAN_SNAPSHOT_BYTES {
            return Err(invalid_serialized_batch_plan());
        }
        let wire: WellearnBatchPlanWire =
            serde_json::from_slice(encoded).map_err(|_| invalid_serialized_batch_plan())?;
        Self::try_from(wire).map_err(|_| invalid_serialized_batch_plan())
    }
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
            return Err(invalid_serialized_atomic_planning_authority());
        }
        Ok(budget)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnAtomicBatchPlanningAuthorityWire {
    version: u16,
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelectionWire,
    expected_remote_task_id: String,
    frozen_fanyuchang_target_seconds: Option<u64>,
    frozen_auto_duration_budget: Option<WellearnAutoDurationBudgetWire>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnCompleteAtomicBatchAuthorityWire {
    version: u16,
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelectionWire,
    expected_remote_task_id: String,
    frozen_fanyuchang_target_count: Option<u32>,
    frozen_fanyuchang_targets_digest: Option<[u8; 32]>,
    frozen_auto_duration_budget: Option<WellearnAutoDurationBudgetWire>,
}

impl From<&WellearnAtomicBatchPlanningAuthority> for WellearnAtomicBatchPlanningAuthorityWire {
    fn from(authority: &WellearnAtomicBatchPlanningAuthority) -> Self {
        Self {
            version: WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_VERSION,
            course_remote_id: authority.course_remote_id.clone(),
            flow: authority.flow,
            selection: WellearnBatchUnitSelectionWire::from(&authority.selection),
            expected_remote_task_id: authority.expected_remote_task_id.clone(),
            frozen_fanyuchang_target_seconds: authority.frozen_fanyuchang_target_seconds,
            frozen_auto_duration_budget: authority.frozen_auto_duration_budget.map(Into::into),
        }
    }
}

impl TryFrom<WellearnAtomicBatchPlanningAuthorityWire> for WellearnAtomicBatchPlanningAuthority {
    type Error = ProviderError;

    fn try_from(wire: WellearnAtomicBatchPlanningAuthorityWire) -> Result<Self, Self::Error> {
        if wire.version != WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_VERSION {
            return Err(invalid_serialized_atomic_planning_authority());
        }
        let auto_budget = wire
            .frozen_auto_duration_budget
            .map(WellearnAutoDurationBudget::try_from)
            .transpose()?;
        Self::try_new(
            wire.course_remote_id,
            wire.flow,
            wire.selection.into(),
            wire.expected_remote_task_id,
            wire.frozen_fanyuchang_target_seconds,
            auto_budget,
        )
    }
}

impl WellearnAtomicBatchPlanningAuthority {
    /// Freezes one complete parent selection and target authority.
    ///
    /// # Errors
    ///
    /// Returns an internal error unless the flow is one of the two atomic
    /// donor flows, Course/SCO identity is valid, selection is bounded and
    /// exactly one flow-specific target input is present.
    pub fn try_new(
        course_remote_id: impl Into<String>,
        flow: WellearnBatchFlow,
        selection: WellearnBatchUnitSelection,
        expected_remote_task_id: impl Into<String>,
        frozen_fanyuchang_target_seconds: Option<u64>,
        frozen_auto_duration_budget: Option<WellearnAutoDurationBudget>,
    ) -> ProviderResult<Self> {
        let authority = Self {
            course_remote_id: course_remote_id.into(),
            flow,
            selection,
            expected_remote_task_id: expected_remote_task_id.into(),
            frozen_fanyuchang_target_seconds,
            frozen_auto_duration_budget,
        };
        authority.validate()?;
        Ok(authority)
    }

    pub fn course_remote_id(&self) -> &str {
        self.course_remote_id.as_str()
    }

    pub const fn flow(&self) -> WellearnBatchFlow {
        self.flow
    }

    pub const fn selection(&self) -> &WellearnBatchUnitSelection {
        &self.selection
    }

    pub fn expected_remote_task_id(&self) -> &str {
        self.expected_remote_task_id.as_str()
    }

    pub const fn frozen_fanyuchang_target_seconds(&self) -> Option<u64> {
        self.frozen_fanyuchang_target_seconds
    }

    pub const fn frozen_auto_duration_budget(&self) -> Option<WellearnAutoDurationBudget> {
        self.frozen_auto_duration_budget
    }

    pub fn frozen_auto_duration_minutes(&self) -> Option<u64> {
        self.frozen_auto_duration_budget
            .map(|budget| u64::from(budget.actual_minutes()))
    }

    /// Converts this authority and its complete target-authorized batch into
    /// Core's encrypted parent-attempt snapshot value.
    ///
    /// Current Fanyuchang targets are stored in the independently bounded
    /// complete-batch value while this four-KiB authority stores their ordered
    /// count and domain-separated digest. Auto retains only its aggregate
    /// budget because every child target is already derivable from the batch.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the parent and batch are inconsistent, either
    /// local encoding exceeds its stricter bound, or Core rejects the Provider
    /// namespace/type/bounded-secret contract.
    pub fn to_execution_parent_batch_snapshot(
        &self,
        batch_plan: &WellearnBatchPlan,
        frozen_fanyuchang_target_seconds: Option<&[u64]>,
    ) -> ProviderResult<ExecutionParentBatchSnapshot> {
        validate_complete_atomic_parent_batch_binding(
            self,
            batch_plan,
            frozen_fanyuchang_target_seconds,
        )?;
        let target_count = frozen_fanyuchang_target_seconds
            .map(|targets| u32::try_from(targets.len()))
            .transpose()
            .map_err(|_| invalid_atomic_recovery_artifacts())?;
        let target_digest = frozen_fanyuchang_target_seconds.map(digest_fanyuchang_targets);
        let authority = WellearnCompleteAtomicBatchAuthorityWire {
            version: WELLEARN_COMPLETE_ATOMIC_BATCH_AUTHORITY_VERSION,
            course_remote_id: self.course_remote_id.clone(),
            flow: self.flow,
            selection: WellearnBatchUnitSelectionWire::from(&self.selection),
            expected_remote_task_id: self.expected_remote_task_id.clone(),
            frozen_fanyuchang_target_count: target_count,
            frozen_fanyuchang_targets_digest: target_digest,
            frozen_auto_duration_budget: self.frozen_auto_duration_budget.map(Into::into),
        };
        let authority = serde_json::to_vec(&authority)
            .map_err(|_| invalid_serialized_atomic_planning_authority())?;
        if authority.is_empty() || authority.len() > MAX_ATOMIC_BATCH_PLANNING_AUTHORITY_BYTES {
            return Err(invalid_serialized_atomic_planning_authority());
        }
        let batch = serde_json::to_vec(&WellearnCompleteAtomicBatchSnapshotWire {
            version: WELLEARN_COMPLETE_ATOMIC_BATCH_SNAPSHOT_VERSION,
            batch_plan: WellearnBatchPlanWire::try_from(batch_plan)?,
            frozen_fanyuchang_target_seconds: frozen_fanyuchang_target_seconds.map(<[u64]>::to_vec),
        })
        .map_err(|_| invalid_serialized_batch_plan())?;
        if batch.is_empty() || batch.len() > MAX_BATCH_PLAN_SNAPSHOT_BYTES {
            return Err(invalid_serialized_batch_plan());
        }
        ExecutionParentBatchSnapshot::try_new(
            welearn_provider_id()?,
            WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE,
            SecretValue::new(authority),
            WELLEARN_ATOMIC_BATCH_SNAPSHOT_TYPE,
            SecretValue::new(batch),
        )
    }

    /// Encodes this credential-free parent authority using the bounded v1
    /// `WELearn` schema.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the authority no longer validates or
    /// exceeds the Provider's serialized size bound.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate()?;
        let encoded = serde_json::to_vec(&WellearnAtomicBatchPlanningAuthorityWire::from(self))
            .map_err(|_| invalid_serialized_atomic_planning_authority())?;
        if encoded.len() > MAX_ATOMIC_BATCH_PLANNING_AUTHORITY_BYTES {
            return Err(invalid_serialized_atomic_planning_authority());
        }
        Ok(encoded)
    }

    /// Restores and fully revalidates one bounded v1 parent authority.
    ///
    /// # Errors
    ///
    /// Returns an internal error for malformed, oversized, version-drifted or
    /// semantically inconsistent authority bytes.
    pub fn decode(encoded: &[u8]) -> ProviderResult<Self> {
        if encoded.is_empty() || encoded.len() > MAX_ATOMIC_BATCH_PLANNING_AUTHORITY_BYTES {
            return Err(invalid_serialized_atomic_planning_authority());
        }
        let wire: WellearnAtomicBatchPlanningAuthorityWire = serde_json::from_slice(encoded)
            .map_err(|_| invalid_serialized_atomic_planning_authority())?;
        Self::try_from(wire).map_err(|_| invalid_serialized_atomic_planning_authority())
    }

    /// Revalidates restored parent authority without fresh I/O.
    ///
    /// # Errors
    ///
    /// Returns an internal error for identity, selection, flow or target drift.
    pub fn validate(&self) -> ProviderResult<()> {
        split_batch_identity(
            self.course_remote_id.as_str(),
            self.expected_remote_task_id.as_str(),
        )?;
        if let WellearnBatchUnitSelection::Explicit(indices) = &self.selection
            && (indices.is_empty()
                || indices.len() > 512
                || indices.iter().any(|index| *index >= 512)
                || indices.iter().copied().collect::<BTreeSet<_>>().len() != indices.len())
        {
            return Err(invalid_atomic_planning_authority());
        }
        let valid = match self.flow {
            WellearnBatchFlow::FanyuchangDuration => {
                self.frozen_auto_duration_budget.is_none()
                    && self.frozen_fanyuchang_target_seconds.is_some_and(|target| {
                        crate::WellearnAtomicDurationCompletionPlan::try_new(
                            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                            target,
                        )
                        .is_ok()
                    })
            }
            WellearnBatchFlow::AutoDuration => {
                self.frozen_fanyuchang_target_seconds.is_none()
                    && self.frozen_auto_duration_budget.is_some_and(|budget| {
                        (1..=MAX_AUTO_DURATION_MINUTES)
                            .contains(&u64::from(budget.actual_minutes()))
                    })
            }
            WellearnBatchFlow::FanyuchangCompletion
            | WellearnBatchFlow::YzbrhCompletion
            | WellearnBatchFlow::YzbrhDuration
            | WellearnBatchFlow::AutoCompletion
            | WellearnBatchFlow::AutoLegacyDuration => false,
        };
        if !valid
            || self.flow.execution_shape() != WellearnBatchExecutionShape::AtomicDurationCompletion
        {
            return Err(invalid_atomic_planning_authority());
        }
        Ok(())
    }
}

/// One fresh full-inventory rebuild plus its exact atomic child projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnPreparedAtomicChildPlan {
    batch_plan: WellearnBatchPlan,
    entry_index: usize,
    child_plan: WellearnAtomicChildPlan,
}

impl WellearnPreparedAtomicChildPlan {
    pub const fn batch_plan(&self) -> &WellearnBatchPlan {
        &self.batch_plan
    }

    pub const fn entry_index(&self) -> usize {
        self.entry_index
    }

    pub const fn child_plan(&self) -> &WellearnAtomicChildPlan {
        &self.child_plan
    }

    /// Restores one complete prepared child from Core's independently durable
    /// batch, ordinal, target authority and Provider-private child artifact.
    ///
    /// This boundary performs no fresh I/O and grants no execution authority.
    /// The caller must still run [`Self::validate_fresh_detail`] immediately
    /// before entering the atomic transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error for any batch, ordinal, target, Provider
    /// namespace, artifact type, payload or child-plan drift.
    pub fn restore_from_provider_execution_plan_artifact(
        batch_plan: WellearnBatchPlan,
        expected_entry_index: usize,
        frozen_fanyuchang_target_seconds: Option<u64>,
        artifact: &ProviderExecutionPlanArtifact,
    ) -> ProviderResult<Self> {
        validate_batch_plan_integrity(&batch_plan)?;
        let child_plan = WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
            artifact,
            &batch_plan,
            expected_entry_index,
            frozen_fanyuchang_target_seconds,
        )?;
        let prepared = Self {
            batch_plan,
            entry_index: expected_entry_index,
            child_plan,
        };
        prepared.validate()?;
        Ok(prepared)
    }

    /// Restores one prepared child from the three independently durable
    /// Provider values that Core must atomically bind to the parent attempt.
    ///
    /// This boundary decodes and revalidates the parent authority and complete
    /// batch snapshot, binds their Course, flow, Unit selection, expected child
    /// and Auto aggregate, then rebinds the exact child artifact. It performs
    /// no fresh I/O and grants no execution authority.
    ///
    /// # Errors
    ///
    /// Returns an internal error for malformed or inconsistent parent, batch
    /// or child artifacts, including a missing expected child.
    pub fn restore_from_durable_artifacts(
        encoded_parent_authority: &[u8],
        encoded_batch_snapshot: &[u8],
        child_artifact: &ProviderExecutionPlanArtifact,
    ) -> ProviderResult<Self> {
        let authority = WellearnAtomicBatchPlanningAuthority::decode(encoded_parent_authority)?;
        let batch_plan = WellearnBatchPlan::decode_snapshot(encoded_batch_snapshot)?;
        let expected_entry_index = validate_atomic_parent_batch_binding(&authority, &batch_plan)?;
        Self::restore_from_provider_execution_plan_artifact(
            batch_plan,
            expected_entry_index,
            authority.frozen_fanyuchang_target_seconds,
            child_artifact,
        )
    }

    /// Restores one exact child from Core's v2 encrypted parent pair and
    /// immutable generic child projection.
    ///
    /// The parent pair first reconstructs the complete target-authorized batch.
    /// This boundary then binds the child's one-based position, remote Task,
    /// grouped atomic call, Provider artifact and conditional sequence to the
    /// corresponding entry. It performs no fresh I/O and grants no mutation
    /// authority.
    ///
    /// # Errors
    ///
    /// Rejects any parent schema/target drift, foreign or non-atomic grouped
    /// call, position/identity substitution, artifact mismatch or sequence
    /// mismatch.
    pub fn restore_from_execution_parent_batch_snapshot(
        parent: &ExecutionParentBatchSnapshot,
        child: &ProviderExecutionChildPlan,
    ) -> ProviderResult<Self> {
        let (_, batch_plan, frozen_fanyuchang_targets) =
            decode_execution_parent_batch_snapshot(parent)?;
        let entry_index = usize::try_from(child.position() - 1)
            .map_err(|_| invalid_atomic_recovery_artifacts())?;
        if child.execution_plan().provider_id() != parent.provider_id()
            || child.execution_plan().calls()
                != [vec![
                    TaskCapability::DurationReport,
                    TaskCapability::ResourceExecution,
                ]]
            || batch_plan
                .entries
                .get(entry_index)
                .is_none_or(|entry| entry.remote_task_id != child.remote_task_id())
        {
            return Err(invalid_atomic_recovery_artifacts());
        }
        let frozen_target = match batch_plan.flow {
            WellearnBatchFlow::FanyuchangDuration => Some(
                frozen_fanyuchang_targets
                    .as_deref()
                    .and_then(|targets| targets.get(entry_index))
                    .copied()
                    .ok_or_else(invalid_atomic_recovery_artifacts)?,
            ),
            WellearnBatchFlow::AutoDuration => {
                if frozen_fanyuchang_targets.is_some() {
                    return Err(invalid_atomic_recovery_artifacts());
                }
                None
            }
            WellearnBatchFlow::FanyuchangCompletion
            | WellearnBatchFlow::YzbrhCompletion
            | WellearnBatchFlow::YzbrhDuration
            | WellearnBatchFlow::AutoCompletion
            | WellearnBatchFlow::AutoLegacyDuration => {
                return Err(invalid_atomic_recovery_artifacts());
            }
        };
        let artifact = child
            .execution_plan()
            .artifact()
            .ok_or_else(invalid_atomic_recovery_artifacts)?;
        let prepared = Self::restore_from_provider_execution_plan_artifact(
            batch_plan,
            entry_index,
            frozen_target,
            artifact,
        )?;
        let expected_sequence =
            crate::build_atomic_mutation_sequence_plan(prepared.child_plan(), artifact)?;
        if child.mutation_sequence_plan() != &expected_sequence {
            return Err(invalid_atomic_recovery_artifacts());
        }
        Ok(prepared)
    }

    /// Revalidates the exact child projection against its rebuilt batch.
    ///
    /// # Errors
    ///
    /// Returns an internal error for batch, ordinal, flow, identity, profile
    /// or target drift.
    pub fn validate(&self) -> ProviderResult<()> {
        validate_batch_plan_integrity(&self.batch_plan)?;
        let fanyuchang_target = (self.child_plan.flow == WellearnBatchFlow::FanyuchangDuration)
            .then_some(self.child_plan.target_seconds);
        self.child_plan.validate_for_batch_entry(
            &self.batch_plan,
            self.entry_index,
            fanyuchang_target,
        )
    }

    /// Rebinds the prepared child to one complete fresh Task detail.
    ///
    /// # Errors
    ///
    /// Returns the prepared-plan validation error or the existing exact fresh
    /// batch-entry identity/capability/eligibility error.
    pub fn validate_fresh_detail(&self, fresh_detail: &RemoteTaskDetail) -> ProviderResult<()> {
        self.validate()?;
        validate_fresh_batch_entry(&self.batch_plan, self.entry_index, fresh_detail)
    }

    /// Converts the rebound exact child into Core's generic immutable artifact.
    ///
    /// # Errors
    ///
    /// Returns the child plan's typed validation or artifact conversion error.
    pub fn provider_plan_artifact(&self) -> ProviderResult<ProviderExecutionPlanArtifact> {
        self.validate()?;
        self.child_plan.to_provider_execution_plan_artifact()
    }
}

fn validate_atomic_parent_batch_binding(
    authority: &WellearnAtomicBatchPlanningAuthority,
    batch_plan: &WellearnBatchPlan,
) -> ProviderResult<usize> {
    authority.validate()?;
    validate_batch_plan_integrity(batch_plan)?;
    let aggregate_duration_seconds = authority
        .frozen_auto_duration_minutes()
        .map(|minutes| {
            minutes
                .checked_mul(60)
                .ok_or_else(invalid_atomic_recovery_artifacts)
        })
        .transpose()?;
    if batch_plan.course_remote_id != authority.course_remote_id
        || batch_plan.flow != authority.flow
        || batch_plan.selection != authority.selection
        || batch_plan.aggregate_duration_seconds != aggregate_duration_seconds
    {
        return Err(invalid_atomic_recovery_artifacts());
    }
    batch_plan
        .entries
        .iter()
        .position(|entry| entry.remote_task_id == authority.expected_remote_task_id)
        .ok_or_else(invalid_atomic_recovery_artifacts)
}

fn validate_complete_atomic_parent_batch_binding(
    authority: &WellearnAtomicBatchPlanningAuthority,
    batch_plan: &WellearnBatchPlan,
    frozen_fanyuchang_target_seconds: Option<&[u64]>,
) -> ProviderResult<usize> {
    let expected_entry_index = validate_atomic_parent_batch_binding(authority, batch_plan)?;
    match (authority.flow, frozen_fanyuchang_target_seconds) {
        (WellearnBatchFlow::FanyuchangDuration, Some(targets))
            if targets.len() == batch_plan.entries.len()
                && targets.iter().copied().all(|target| {
                    crate::WellearnAtomicDurationCompletionPlan::try_new(
                        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
                        target,
                    )
                    .is_ok()
                })
                && targets.get(expected_entry_index).copied()
                    == authority.frozen_fanyuchang_target_seconds =>
        {
            Ok(expected_entry_index)
        }
        (WellearnBatchFlow::AutoDuration, None) => Ok(expected_entry_index),
        _ => Err(invalid_atomic_recovery_artifacts()),
    }
}

fn digest_fanyuchang_targets(targets: &[u64]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism.welearn.fanyuchang-batch-targets.v2\0");
    digest.update(
        u32::try_from(targets.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for (index, target) in targets.iter().copied().enumerate() {
        digest.update(u32::try_from(index).unwrap_or(u32::MAX).to_be_bytes());
        digest.update(target.to_be_bytes());
    }
    digest.finalize().into()
}

pub(crate) fn decode_execution_parent_batch_snapshot(
    parent: &ExecutionParentBatchSnapshot,
) -> ProviderResult<(
    WellearnAtomicBatchPlanningAuthority,
    WellearnBatchPlan,
    Option<Vec<u64>>,
)> {
    if parent.provider_id() != &welearn_provider_id()?
        || parent.authority_type() != WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE
        || parent.batch_type() != WELLEARN_ATOMIC_BATCH_SNAPSHOT_TYPE
    {
        return Err(invalid_atomic_recovery_artifacts());
    }
    let authority: WellearnCompleteAtomicBatchAuthorityWire =
        serde_json::from_slice(parent.authority().expose_secret())
            .map_err(|_| invalid_serialized_atomic_planning_authority())?;
    let snapshot: WellearnCompleteAtomicBatchSnapshotWire =
        serde_json::from_slice(parent.batch().expose_secret())
            .map_err(|_| invalid_serialized_batch_plan())?;
    if authority.version != WELLEARN_COMPLETE_ATOMIC_BATCH_AUTHORITY_VERSION
        || snapshot.version != WELLEARN_COMPLETE_ATOMIC_BATCH_SNAPSHOT_VERSION
    {
        return Err(invalid_atomic_recovery_artifacts());
    }
    let batch_plan = WellearnBatchPlan::try_from(snapshot.batch_plan)
        .map_err(|_| invalid_serialized_batch_plan())?;
    let auto_budget = authority
        .frozen_auto_duration_budget
        .map(WellearnAutoDurationBudget::try_from)
        .transpose()?;
    let expected_entry_index = batch_plan
        .entries
        .iter()
        .position(|entry| entry.remote_task_id == authority.expected_remote_task_id)
        .ok_or_else(invalid_atomic_recovery_artifacts)?;
    let expected_fanyuchang_target = match authority.flow {
        WellearnBatchFlow::FanyuchangDuration => {
            let targets = snapshot
                .frozen_fanyuchang_target_seconds
                .as_deref()
                .ok_or_else(invalid_atomic_recovery_artifacts)?;
            let count =
                u32::try_from(targets.len()).map_err(|_| invalid_atomic_recovery_artifacts())?;
            if authority.frozen_fanyuchang_target_count != Some(count)
                || authority.frozen_fanyuchang_targets_digest
                    != Some(digest_fanyuchang_targets(targets))
                || auto_budget.is_some()
            {
                return Err(invalid_atomic_recovery_artifacts());
            }
            targets
                .get(expected_entry_index)
                .copied()
                .ok_or_else(invalid_atomic_recovery_artifacts)?
        }
        WellearnBatchFlow::AutoDuration => {
            if snapshot.frozen_fanyuchang_target_seconds.is_some()
                || authority.frozen_fanyuchang_target_count.is_some()
                || authority.frozen_fanyuchang_targets_digest.is_some()
            {
                return Err(invalid_atomic_recovery_artifacts());
            }
            0
        }
        WellearnBatchFlow::FanyuchangCompletion
        | WellearnBatchFlow::YzbrhCompletion
        | WellearnBatchFlow::YzbrhDuration
        | WellearnBatchFlow::AutoCompletion
        | WellearnBatchFlow::AutoLegacyDuration => {
            return Err(invalid_atomic_recovery_artifacts());
        }
    };
    let restored_authority = WellearnAtomicBatchPlanningAuthority::try_new(
        authority.course_remote_id,
        authority.flow,
        authority.selection.into(),
        authority.expected_remote_task_id,
        (authority.flow == WellearnBatchFlow::FanyuchangDuration)
            .then_some(expected_fanyuchang_target),
        auto_budget,
    )?;
    validate_complete_atomic_parent_batch_binding(
        &restored_authority,
        &batch_plan,
        snapshot.frozen_fanyuchang_target_seconds.as_deref(),
    )?;
    Ok((
        restored_authority,
        batch_plan,
        snapshot.frozen_fanyuchang_target_seconds,
    ))
}

/// Versioned Provider-private payload for one exact atomic batch child.
///
/// Core may persist the bounded serialized value but does not interpret its
/// donor flow or wire profile. The Course/SCO and batch ordinal bindings keep
/// the payload from being substituted across children during recovery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WellearnAtomicChildPlan {
    version: u16,
    entry_index: u32,
    course_remote_id: String,
    remote_task_id: String,
    flow: WellearnBatchFlow,
    execution_shape: WellearnBatchExecutionShape,
    atomic_completion_profile: WellearnAtomicCompletionProfile,
    target_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WellearnAtomicChildPlanWire {
    version: u16,
    entry_index: u32,
    course_remote_id: String,
    remote_task_id: String,
    flow: WellearnBatchFlow,
    execution_shape: WellearnBatchExecutionShape,
    atomic_completion_profile: WellearnAtomicCompletionProfile,
    target_seconds: u64,
}

impl<'de> Deserialize<'de> for WellearnAtomicChildPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WellearnAtomicChildPlanWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<WellearnAtomicChildPlanWire> for WellearnAtomicChildPlan {
    type Error = ProviderError;

    fn try_from(wire: WellearnAtomicChildPlanWire) -> Result<Self, Self::Error> {
        let plan = Self {
            version: wire.version,
            entry_index: wire.entry_index,
            course_remote_id: wire.course_remote_id,
            remote_task_id: wire.remote_task_id,
            flow: wire.flow,
            execution_shape: wire.execution_shape,
            atomic_completion_profile: wire.atomic_completion_profile,
            target_seconds: wire.target_seconds,
        };
        plan.validate()?;
        Ok(plan)
    }
}

impl WellearnAtomicChildPlan {
    pub const fn version(&self) -> u16 {
        self.version
    }

    pub const fn entry_index(&self) -> u32 {
        self.entry_index
    }

    pub fn course_remote_id(&self) -> &str {
        self.course_remote_id.as_str()
    }

    pub fn remote_task_id(&self) -> &str {
        self.remote_task_id.as_str()
    }

    pub const fn flow(&self) -> WellearnBatchFlow {
        self.flow
    }

    pub const fn execution_shape(&self) -> WellearnBatchExecutionShape {
        self.execution_shape
    }

    pub const fn atomic_completion_profile(&self) -> WellearnAtomicCompletionProfile {
        self.atomic_completion_profile
    }

    pub const fn target_seconds(&self) -> u64 {
        self.target_seconds
    }

    /// Expands the frozen child profile and target into the complete native
    /// atomic wire plan.
    ///
    /// # Errors
    ///
    /// Returns an internal error for any restored child-plan drift.
    pub fn duration_completion_plan(
        &self,
    ) -> ProviderResult<crate::WellearnAtomicDurationCompletionPlan> {
        self.validate()?;
        crate::WellearnAtomicDurationCompletionPlan::try_new(
            self.atomic_completion_profile,
            self.target_seconds,
        )
    }

    /// Revalidates every restored Provider-private plan fact.
    ///
    /// # Errors
    ///
    /// Returns an internal error for version, identity, flow/profile or target
    /// drift. No field is repaired or inferred from another field.
    pub fn validate(&self) -> ProviderResult<()> {
        let entry_index =
            usize::try_from(self.entry_index).map_err(|_| invalid_atomic_child_plan())?;
        if self.version != WELLEARN_ATOMIC_CHILD_PLAN_VERSION
            || entry_index >= MAX_BATCH_TASKS
            || self.course_remote_id.is_empty()
            || self.remote_task_id.is_empty()
            || self.course_remote_id.chars().any(char::is_control)
            || self.remote_task_id.chars().any(char::is_control)
            || self.execution_shape != WellearnBatchExecutionShape::AtomicDurationCompletion
        {
            return Err(invalid_atomic_child_plan());
        }
        split_batch_identity(self.course_remote_id.as_str(), self.remote_task_id.as_str())?;
        let profile_matches_flow = matches!(
            (self.flow, self.atomic_completion_profile),
            (
                WellearnBatchFlow::FanyuchangDuration,
                WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100,
            ) | (
                WellearnBatchFlow::AutoDuration,
                WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0,
            )
        );
        if !profile_matches_flow
            || self.flow.execution_shape() != self.execution_shape
            || self.flow.atomic_completion_profile() != Some(self.atomic_completion_profile)
            || crate::WellearnAtomicDurationCompletionPlan::try_new(
                self.atomic_completion_profile,
                self.target_seconds,
            )
            .is_err()
        {
            return Err(invalid_atomic_child_plan());
        }
        Ok(())
    }

    /// Rebinds a restored child payload to its exact immutable batch entry.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the batch has drifted or materializing
    /// its recorded ordinal and target authority does not reproduce this value
    /// byte-for-byte at the typed field boundary.
    pub fn validate_for_batch_entry(
        &self,
        batch: &WellearnBatchPlan,
        expected_entry_index: usize,
        frozen_fanyuchang_target_seconds: Option<u64>,
    ) -> ProviderResult<()> {
        self.validate()?;
        if usize::try_from(self.entry_index).map_err(|_| invalid_atomic_child_plan())?
            != expected_entry_index
        {
            return Err(invalid_atomic_child_plan());
        }
        let expected = materialize_atomic_child_plan(
            batch,
            expected_entry_index,
            frozen_fanyuchang_target_seconds,
        )?;
        if self != &expected {
            return Err(invalid_atomic_child_plan());
        }
        Ok(())
    }

    /// Encodes one revalidated bounded Provider-private child plan.
    ///
    /// # Errors
    ///
    /// Returns an internal error when validation, serialization or the encoded
    /// size bound fails.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| invalid_atomic_child_plan())?;
        if encoded.is_empty() || encoded.len() > MAX_ATOMIC_CHILD_PLAN_BYTES {
            return Err(invalid_atomic_child_plan());
        }
        Ok(encoded)
    }

    /// Decodes and revalidates a bounded Provider-private child plan.
    ///
    /// # Errors
    ///
    /// Returns an internal error for size, schema or invariant drift.
    pub fn decode(encoded: &[u8]) -> ProviderResult<Self> {
        if encoded.is_empty() || encoded.len() > MAX_ATOMIC_CHILD_PLAN_BYTES {
            return Err(invalid_atomic_child_plan());
        }
        serde_json::from_slice(encoded).map_err(|_| invalid_atomic_child_plan())
    }

    /// Restores one payload and rebinds it to the exact immutable batch entry.
    ///
    /// # Errors
    ///
    /// Returns an internal error for encoded drift or when the restored value
    /// does not exactly match the validated batch and frozen target authority.
    pub fn decode_bound(
        encoded: &[u8],
        batch: &WellearnBatchPlan,
        expected_entry_index: usize,
        frozen_fanyuchang_target_seconds: Option<u64>,
    ) -> ProviderResult<Self> {
        let plan = Self::decode(encoded)?;
        plan.validate_for_batch_entry(
            batch,
            expected_entry_index,
            frozen_fanyuchang_target_seconds,
        )?;
        Ok(plan)
    }

    /// Converts one validated credential-free plan into Core's generic
    /// Provider execution artifact.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the local plan is invalid, exceeds its
    /// stricter one-KiB bound or cannot satisfy Core's provider/type/payload
    /// artifact contract.
    pub fn to_provider_execution_plan_artifact(
        &self,
    ) -> ProviderResult<ProviderExecutionPlanArtifact> {
        let encoded = self.encode()?;
        let payload = serde_json::from_slice(&encoded).map_err(|_| invalid_atomic_child_plan())?;
        ProviderExecutionPlanArtifact::try_new(
            welearn_provider_id()?,
            WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE,
            payload,
        )
    }

    /// Restores Core's generic artifact and rebinds it to one exact batch
    /// child and frozen target authority.
    ///
    /// Provider namespace and artifact type are checked before the payload is
    /// decoded. A valid payload for another batch ordinal still fails the
    /// existing full batch/ordinal/target rebind.
    ///
    /// # Errors
    ///
    /// Returns an internal error for a foreign provider/type or any payload,
    /// batch, ordinal, profile, identity or target drift.
    pub fn from_provider_execution_plan_artifact_bound(
        artifact: &ProviderExecutionPlanArtifact,
        batch: &WellearnBatchPlan,
        expected_entry_index: usize,
        frozen_fanyuchang_target_seconds: Option<u64>,
    ) -> ProviderResult<Self> {
        if artifact.provider_id() != &welearn_provider_id()?
            || artifact.artifact_type() != WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE
        {
            return Err(invalid_atomic_child_plan());
        }
        let encoded = serde_json::to_vec(artifact.payload_sanitized())
            .map_err(|_| invalid_atomic_child_plan())?;
        Self::decode_bound(
            &encoded,
            batch,
            expected_entry_index,
            frozen_fanyuchang_target_seconds,
        )
    }
}

fn welearn_provider_id() -> ProviderResult<ProviderId> {
    ProviderId::new(PROVIDER_ID).map_err(|_| invalid_atomic_child_plan())
}

/// Materializes one exact atomic child from a fully validated batch plan.
///
/// Current Fanyuchang has no aggregate entry target, so its deterministic
/// per-Execution target must already be frozen and supplied, including the
/// donor-evidenced literal zero. Modular Auto must use the equal-floor target
/// stored on the exact entry, including zero.
/// Singleton flows never produce this value.
///
/// # Errors
///
/// Returns an internal error for batch drift, a foreign ordinal, a singleton
/// flow, missing/extra target authority or an invalid atomic target.
pub fn materialize_atomic_child_plan(
    batch: &WellearnBatchPlan,
    entry_index: usize,
    frozen_fanyuchang_target_seconds: Option<u64>,
) -> ProviderResult<WellearnAtomicChildPlan> {
    validate_batch_plan_integrity(batch)?;
    materialize_atomic_child_plan_for_validated_batch(
        batch,
        entry_index,
        frozen_fanyuchang_target_seconds,
    )
}

/// Materializes one child after the caller has validated the complete batch.
/// This keeps whole-batch projection linear at the audited 8,192-child bound.
pub(crate) fn materialize_atomic_child_plan_for_validated_batch(
    batch: &WellearnBatchPlan,
    entry_index: usize,
    frozen_fanyuchang_target_seconds: Option<u64>,
) -> ProviderResult<WellearnAtomicChildPlan> {
    let entry = batch
        .entries
        .get(entry_index)
        .ok_or_else(invalid_atomic_child_plan)?;
    let profile = batch
        .atomic_completion_profile
        .ok_or_else(invalid_atomic_child_plan)?;
    if batch.execution_shape != WellearnBatchExecutionShape::AtomicDurationCompletion {
        return Err(invalid_atomic_child_plan());
    }
    let target_seconds = match batch.flow {
        WellearnBatchFlow::FanyuchangDuration => {
            if entry.target_seconds.is_some() {
                return Err(invalid_atomic_child_plan());
            }
            frozen_fanyuchang_target_seconds.ok_or_else(invalid_atomic_child_plan)?
        }
        WellearnBatchFlow::AutoDuration => {
            if frozen_fanyuchang_target_seconds.is_some() {
                return Err(invalid_atomic_child_plan());
            }
            entry.target_seconds.ok_or_else(invalid_atomic_child_plan)?
        }
        WellearnBatchFlow::FanyuchangCompletion
        | WellearnBatchFlow::YzbrhCompletion
        | WellearnBatchFlow::YzbrhDuration
        | WellearnBatchFlow::AutoCompletion
        | WellearnBatchFlow::AutoLegacyDuration => return Err(invalid_atomic_child_plan()),
    };
    let plan = WellearnAtomicChildPlan {
        version: WELLEARN_ATOMIC_CHILD_PLAN_VERSION,
        entry_index: u32::try_from(entry_index).map_err(|_| invalid_atomic_child_plan())?,
        course_remote_id: batch.course_remote_id.clone(),
        remote_task_id: entry.remote_task_id.clone(),
        flow: batch.flow,
        execution_shape: batch.execution_shape,
        atomic_completion_profile: profile,
        target_seconds,
    };
    plan.validate()?;
    Ok(plan)
}

/// Rebuilds one complete atomic batch from a fresh Course Unit/SCO inventory
/// and projects the exact authorized child without guessing parent selection.
///
/// The caller owns read-only fresh discovery. This pure boundary requires its
/// explicit parent authority, rebuilds the full selected batch once, locates
/// the expected Task by stable remote identity and binds the resulting child
/// to the rebuilt batch before returning.
///
/// # Errors
///
/// Returns a typed error for invalid authority or fresh inventory, parent
/// Course drift, a missing/ineligible child, target drift or artifact failure.
pub fn prepare_atomic_child_plan_from_fresh_inventory(
    fresh_tasks: &[RemoteTask],
    fresh_units: &[WellearnUnitObservation],
    authority: &WellearnAtomicBatchPlanningAuthority,
) -> ProviderResult<WellearnPreparedAtomicChildPlan> {
    authority.validate()?;
    let batch_plan = build_selected_batch_plan(
        fresh_tasks,
        fresh_units,
        authority.selection.clone(),
        authority.flow,
        authority.frozen_auto_duration_minutes(),
    )?;
    if batch_plan.course_remote_id != authority.course_remote_id {
        return Err(atomic_planning_remote_changed());
    }
    let entry_index = batch_plan
        .entries
        .iter()
        .position(|entry| entry.remote_task_id == authority.expected_remote_task_id)
        .ok_or_else(atomic_planning_remote_changed)?;
    let child_plan = materialize_atomic_child_plan(
        &batch_plan,
        entry_index,
        authority.frozen_fanyuchang_target_seconds,
    )?;
    child_plan.validate_for_batch_entry(
        &batch_plan,
        entry_index,
        authority.frozen_fanyuchang_target_seconds,
    )?;
    child_plan.to_provider_execution_plan_artifact()?;
    Ok(WellearnPreparedAtomicChildPlan {
        batch_plan,
        entry_index,
        child_plan,
    })
}

fn invalid_atomic_planning_authority() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic batch planning authority is incomplete or inconsistent",
    )
}

fn invalid_atomic_recovery_artifacts() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn durable atomic recovery artifacts are inconsistent",
    )
}

fn invalid_serialized_atomic_planning_authority() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn serialized atomic batch planning authority is invalid",
    )
}

fn invalid_serialized_batch_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn serialized batch plan snapshot is invalid",
    )
}

fn invalid_auto_duration_budget() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn Auto aggregate duration budget is outside donor bounds",
    )
}

fn atomic_planning_remote_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "WELearn fresh atomic batch no longer contains the authorized Course child",
    )
}

fn invalid_atomic_child_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic child plan is invalid or inconsistent with its frozen batch entry",
    )
}

/// Validates that a public or restored batch plan still contains one
/// self-consistent immutable snapshot of the selected donor flow.
///
/// # Errors
///
/// Returns an internal error when flow-derived profiles, Unit selection,
/// child membership/order, visibility facts or aggregate target arithmetic
/// have drifted from each other.
pub fn validate_batch_plan_integrity(plan: &WellearnBatchPlan) -> ProviderResult<()> {
    validate_frozen_flow_profiles(plan)?;
    let selected_ordinals = validate_frozen_unit_selection(plan)?;
    validate_frozen_entries(plan, &selected_ordinals)?;
    validate_frozen_targets(plan)
}

fn validate_frozen_flow_profiles(plan: &WellearnBatchPlan) -> ProviderResult<()> {
    if plan.dispatch != plan.flow.dispatch()
        || plan.target_strategy != plan.flow.target_strategy()
        || plan.execution_shape != plan.flow.execution_shape()
        || plan.atomic_completion_profile != plan.flow.atomic_completion_profile()
        || plan.flow.validate_unit_selection(&plan.selection).is_err()
    {
        return Err(invalid_frozen_batch_plan(
            "flow-derived execution profiles are inconsistent",
        ));
    }
    Ok(())
}

fn validate_frozen_unit_selection(
    plan: &WellearnBatchPlan,
) -> ProviderResult<BTreeMap<u32, usize>> {
    if plan.selected_units.is_empty() || plan.selected_units.len() > 512 {
        return Err(invalid_frozen_batch_plan(
            "selected Unit snapshot is empty or oversized",
        ));
    }
    let mut selected_ordinals = BTreeMap::new();
    for (ordinal, unit) in plan.selected_units.iter().enumerate() {
        if unit.title.is_empty()
            || unit.title.len() > 512
            || unit.title.chars().any(char::is_control)
            || unit.code.as_ref().is_some_and(|code| {
                code.is_empty() || code.len() > 512 || code.chars().any(char::is_control)
            })
            || unit.index >= 512
            || selected_ordinals.insert(unit.index, ordinal).is_some()
        {
            return Err(invalid_frozen_batch_plan(
                "selected Unit snapshot contains an invalid or duplicate observation",
            ));
        }
    }
    match &plan.selection {
        WellearnBatchUnitSelection::All => {
            if !plan
                .selected_units
                .iter()
                .enumerate()
                .all(|(ordinal, unit)| usize::try_from(unit.index).ok() == Some(ordinal))
            {
                return Err(invalid_frozen_batch_plan(
                    "all-Unit selection no longer matches the selected Unit snapshot",
                ));
            }
        }
        WellearnBatchUnitSelection::Explicit(indices) => {
            if indices.as_slice()
                != plan
                    .selected_units
                    .iter()
                    .map(|unit| unit.index)
                    .collect::<Vec<_>>()
                    .as_slice()
            {
                return Err(invalid_frozen_batch_plan(
                    "explicit Unit selection no longer matches the selected Unit snapshot",
                ));
            }
        }
    }
    Ok(selected_ordinals)
}

fn validate_frozen_entries(
    plan: &WellearnBatchPlan,
    selected_ordinals: &BTreeMap<u32, usize>,
) -> ProviderResult<()> {
    if plan.entries.is_empty() || plan.entries.len() > MAX_BATCH_TASKS {
        return Err(invalid_frozen_batch_plan(
            "child snapshot is empty or oversized",
        ));
    }
    let mut seen_remote_ids = BTreeSet::new();
    let mut previous_order = None;
    for entry in &plan.entries {
        split_batch_identity(
            plan.course_remote_id.as_str(),
            entry.remote_task_id.as_str(),
        )?;
        if !seen_remote_ids.insert(entry.remote_task_id.as_str()) {
            return Err(invalid_frozen_batch_plan(
                "child snapshot contains a duplicate SCO identity",
            ));
        }
        let selected_ordinal = selected_ordinals.get(&entry.unit_index).ok_or_else(|| {
            invalid_frozen_batch_plan("child snapshot references an unselected Unit")
        })?;
        let unit = &plan.selected_units[*selected_ordinal];
        if entry.unit_visible != unit.visible
            || entry.visible
                != (entry.unit_visible && entry.sco_visible.unwrap_or(entry.unit_visible))
        {
            return Err(invalid_frozen_batch_plan(
                "child and selected Unit visibility facts are inconsistent",
            ));
        }
        if plan.flow.requires_raw_sco_visibility() && entry.sco_visible == Some(false) {
            return Err(invalid_frozen_batch_plan(
                "child snapshot violates donor SCO visibility membership",
            ));
        }
        if plan.flow.requires_pending_completion() && entry.completion != RemoteState::Pending {
            return Err(invalid_frozen_batch_plan(
                "child snapshot violates donor pending-completion membership",
            ));
        }
        if matches!(
            entry.completion,
            RemoteState::Expired | RemoteState::Removed
        ) {
            return Err(invalid_frozen_batch_plan(
                "child snapshot contains an unavailable SCO",
            ));
        }

        let order = (
            *selected_ordinal,
            entry.sco_index,
            entry.remote_task_id.as_str(),
        );
        if previous_order.is_some_and(|previous| previous >= order) {
            return Err(invalid_frozen_batch_plan(
                "child snapshot no longer preserves frozen dispatch order",
            ));
        }
        previous_order = Some(order);
    }
    Ok(())
}

fn validate_frozen_targets(plan: &WellearnBatchPlan) -> ProviderResult<()> {
    if plan.flow == WellearnBatchFlow::AutoDuration {
        let aggregate = plan.aggregate_duration_seconds.ok_or_else(|| {
            invalid_frozen_batch_plan("Auto duration has no frozen aggregate target")
        })?;
        let count = u64::try_from(plan.entries.len()).map_err(|_| {
            invalid_frozen_batch_plan("Auto duration child count exceeds the bounded limit")
        })?;
        if aggregate == 0
            || aggregate > MAX_AUTO_DURATION_MINUTES * 60
            || aggregate % 60 != 0
            || plan.discarded_remainder_seconds != aggregate % count
            || !plan
                .entries
                .iter()
                .all(|entry| entry.target_seconds == Some(aggregate / count))
        {
            return Err(invalid_frozen_batch_plan(
                "Auto duration aggregate, child targets or remainder are inconsistent",
            ));
        }
    } else if plan.aggregate_duration_seconds.is_some()
        || plan.discarded_remainder_seconds != 0
        || plan
            .entries
            .iter()
            .any(|entry| entry.target_seconds.is_some())
    {
        return Err(invalid_frozen_batch_plan(
            "non-aggregate flow contains aggregate target facts",
        ));
    }
    Ok(())
}

fn invalid_frozen_batch_plan(reason: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        format!("WELearn frozen batch plan integrity failure: {reason}"),
    )
}

/// Rebinds one immutable batch child to a complete fresh Task detail without
/// changing membership, dispatch order or any frozen target.
///
/// Response-order SCO movement is allowed because the stable Course/SCO and
/// selected Unit identities still bind the same child; the persisted plan
/// remains authoritative for dispatch order. Completion flows also allow a
/// fresh Completed observation so their single-Task preflight can verify an
/// externally completed child without replaying a mutation.
///
/// # Errors
///
/// Returns a typed error when the entry ordinal or frozen identity is invalid,
/// the fresh detail no longer binds the same Course/SCO/Unit, required
/// capabilities disappear, or the child is no longer eligible for the frozen
/// donor flow.
pub fn validate_fresh_batch_entry(
    plan: &WellearnBatchPlan,
    entry_index: usize,
    fresh_detail: &RemoteTaskDetail,
) -> ProviderResult<()> {
    validate_batch_plan_integrity(plan)?;
    let entry = plan.entries.get(entry_index).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn batch entry ordinal is outside the frozen plan",
        )
    })?;
    let (course_id, sco_id) = split_batch_identity(
        plan.course_remote_id.as_str(),
        entry.remote_task_id.as_str(),
    )?;
    validate_fresh_execution_detail(
        fresh_detail,
        entry.remote_task_id.as_str(),
        course_id,
        sco_id,
        plan.flow.required_capabilities(),
    )?;

    let fresh = &fresh_detail.task;
    let observation = fresh_batch_observation(fresh)?;
    if observation.course_id != course_id
        || observation.sco_id != sco_id
        || observation.unit_index != entry.unit_index
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn batch child Course, SCO or Unit binding changed",
        ));
    }

    let mut selected_units = plan
        .selected_units
        .iter()
        .filter(|unit| unit.index == entry.unit_index);
    let selected_unit = selected_units.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn batch child references an unselected Unit",
        )
    })?;
    if selected_units.next().is_some() {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn frozen batch contains duplicate selected Units",
        ));
    }
    if observation.unit_title != selected_unit.title
        || observation.unit_code != selected_unit.code.as_deref()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn selected Unit identity changed before batch child execution",
        ));
    }

    if observation.visible
        != (observation.unit_visible && observation.sco_visible.unwrap_or(observation.unit_visible))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn fresh batch child visibility observations are inconsistent",
        ));
    }
    if plan.flow.requires_raw_sco_visibility() && observation.sco_visible == Some(false) {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn batch child is no longer eligible under donor SCO visibility rules",
        ));
    }
    if plan.flow.requires_pending_completion()
        && !matches!(
            observation.completion,
            RemoteState::Pending | RemoteState::Completed
        )
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn batch child is no longer pending or already completed",
        ));
    }
    Ok(())
}

struct FreshBatchObservation<'a> {
    course_id: &'a str,
    sco_id: &'a str,
    unit_index: u32,
    unit_title: &'a str,
    unit_code: Option<&'a str>,
    unit_visible: bool,
    sco_visible: Option<bool>,
    visible: bool,
    completion: RemoteState,
}

fn fresh_batch_observation(task: &RemoteTask) -> ProviderResult<FreshBatchObservation<'_>> {
    let normalized = task.normalized.as_object().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn fresh batch child has an invalid SCO observation",
        )
    })?;
    let unit_code = match normalized.get("unit_code") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) if !value.is_empty() => Some(value.as_str()),
        _ => return Err(incomplete_fresh_batch_observation()),
    };
    let observation = FreshBatchObservation {
        course_id: normalized
            .get("course_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(incomplete_fresh_batch_observation)?,
        sco_id: normalized
            .get("sco_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(incomplete_fresh_batch_observation)?,
        unit_index: normalized_u32(task, "unit_index")
            .ok_or_else(incomplete_fresh_batch_observation)?,
        unit_title: normalized
            .get("unit_title")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(incomplete_fresh_batch_observation)?,
        unit_code,
        unit_visible: normalized_bool(task, "unit_visible")
            .ok_or_else(incomplete_fresh_batch_observation)?,
        sco_visible: normalized
            .get("sco_visible")
            .and_then(serde_json::Value::as_bool),
        visible: normalized_bool(task, "visible").ok_or_else(incomplete_fresh_batch_observation)?,
        completion: normalized_completion(task).ok_or_else(incomplete_fresh_batch_observation)?,
    };
    if normalized.get("schema").and_then(serde_json::Value::as_str) != Some("welearn.sco.v2")
        || normalized_usize(task, "sco_index").is_none()
        || !has_valid_optional_bool(task, "sco_visible")
        || !valid_batch_id_component(observation.course_id)
        || !valid_batch_id_component(observation.sco_id)
        || observation.unit_title.is_empty()
        || observation.unit_title.len() > 512
        || observation.unit_title.chars().any(char::is_control)
    {
        return Err(incomplete_fresh_batch_observation());
    }
    Ok(observation)
}

fn incomplete_fresh_batch_observation() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "WELearn fresh batch child has an incomplete SCO observation",
    )
}

/// Builds the exact audited donor membership rule from a fresh inventory.
/// `auto_duration_minutes` is the already-frozen aggregate sample for the
/// Auto duration flow; this function never samples or re-distributes it.
///
/// # Errors
///
/// Returns a typed error when the input is empty, oversized, duplicated,
/// cross-course, capability-incompatible, unavailable, incomplete, or when
/// the selected donor flow has no eligible entries or invalid duration input.
///
/// # Panics
///
/// The internal `expect` calls are guarded by the normalized SCO v2 completeness
/// checks at the start of the function; malformed observations return an error
/// before membership planning reaches those reads.
#[allow(clippy::too_many_lines)]
pub fn build_batch_plan(
    tasks: &[RemoteTask],
    flow: WellearnBatchFlow,
    auto_duration_minutes: Option<u64>,
) -> ProviderResult<WellearnBatchPlan> {
    let units = derive_unit_observations(tasks)?;
    build_selected_batch_plan(
        tasks,
        &units,
        WellearnBatchUnitSelection::All,
        flow,
        auto_duration_minutes,
    )
}

/// Builds a donor batch plan from a complete fresh SCO inventory and an exact
/// fresh Unit selection. Selected Unit order and empty selected Units remain
/// immutable plan facts even though only eligible SCOs become child entries.
///
/// # Errors
///
/// Returns a typed error when Unit observations or selection are empty,
/// oversized, duplicated or inconsistent with the supplied SCO inventory, in
/// addition to the task and target errors documented by [`build_batch_plan`].
///
/// # Panics
///
/// Internal `expect` calls are guarded by complete normalized SCO checks and
/// validated Auto-duration input before membership planning reaches them.
#[allow(clippy::too_many_lines)]
pub fn build_selected_batch_plan(
    tasks: &[RemoteTask],
    units: &[WellearnUnitObservation],
    selection: WellearnBatchUnitSelection,
    flow: WellearnBatchFlow,
    auto_duration_minutes: Option<u64>,
) -> ProviderResult<WellearnBatchPlan> {
    flow.validate_unit_selection(&selection)?;
    let selected_units = select_units(units, &selection)?;
    let selected_ordinals = selected_units
        .iter()
        .enumerate()
        .map(|(ordinal, unit)| (unit.index, ordinal))
        .collect::<BTreeMap<_, _>>();
    if tasks.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "WELearn batch selection contains no SCO tasks",
        ));
    }
    if tasks.len() > MAX_BATCH_TASKS {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn batch selection exceeds the item limit",
        ));
    }
    if flow != WellearnBatchFlow::AutoDuration && auto_duration_minutes.is_some() {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn aggregate duration is only valid for Auto duration flow",
        ));
    }
    if flow == WellearnBatchFlow::AutoDuration && auto_duration_minutes.is_none() {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn Auto duration requires a frozen aggregate minute sample",
        ));
    }

    let course_remote_id = tasks
        .first()
        .and_then(|task| task.course_remote_id.as_deref())
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch selection has no Course binding",
            )
        })?;
    let course_remote_id = course_remote_id.to_owned();
    let mut seen_remote_ids = BTreeSet::new();
    for task in tasks {
        if !seen_remote_ids.insert(task.remote_id.as_str()) {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch selection contains a duplicate SCO identity",
            ));
        }
        if task.course_remote_id.as_deref() != Some(course_remote_id.as_str()) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "WELearn batch selection spans multiple Courses",
            ));
        }
        if !flow
            .required_capabilities()
            .iter()
            .all(|capability| task.capabilities.contains(capability))
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn batch task does not advertise the selected donor capability",
            ));
        }
        if matches!(
            task.remote_state,
            RemoteState::Expired | RemoteState::Removed
        ) {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn batch selection contains an unavailable SCO",
            ));
        }
        if task
            .normalized
            .get("schema")
            .and_then(serde_json::Value::as_str)
            != Some("welearn.sco.v2")
            || normalized_bool(task, "visible").is_none()
            || normalized_bool(task, "unit_visible").is_none()
            || normalized_u32(task, "unit_index").is_none()
            || normalized_usize(task, "sco_index").is_none()
            || !has_valid_optional_bool(task, "sco_visible")
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch selection requires a complete normalized SCO v2 observation",
            ));
        }
    }

    let known_units = units
        .iter()
        .map(|unit| (unit.index, unit))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = tasks
        .iter()
        .filter(|task| {
            normalized_u32(task, "unit_index")
                .is_some_and(|index| selected_ordinals.contains_key(&index))
        })
        .collect::<Vec<_>>();
    for task in &ordered {
        validate_task_unit_observation(task, &known_units)?;
    }
    for task in tasks {
        let unit_index = normalized_u32(task, "unit_index")
            .expect("validated complete normalized observation above");
        if !known_units.contains_key(&unit_index) {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch task references an unknown Unit observation",
            ));
        }
    }
    ordered.sort_by(|left, right| compare_selected_task_order(left, right, &selected_ordinals));
    let mut entries = Vec::with_capacity(ordered.len());
    for task in ordered {
        let visible = normalized_bool(task, "visible")
            .expect("validated complete normalized observation above");
        let unit_visible = normalized_bool(task, "unit_visible")
            .expect("validated complete normalized observation above");
        let sco_visible = task
            .normalized
            .get("sco_visible")
            .and_then(serde_json::Value::as_bool);
        let expected_visible = unit_visible && sco_visible.unwrap_or(unit_visible);
        if visible != expected_visible {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch visibility observations are inconsistent",
            ));
        }
        let completion = normalized_completion(task).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch selection has no independent completion observation",
            )
        })?;
        let raw_sco_visible = sco_visible != Some(false);
        let keep = (!flow.requires_raw_sco_visibility() || raw_sco_visible)
            && (!flow.skips_completed() || completion != RemoteState::Completed)
            && (!flow.requires_pending_completion() || completion == RemoteState::Pending);
        if !keep {
            continue;
        }
        entries.push(WellearnBatchEntry {
            remote_task_id: task.remote_id.clone(),
            unit_index: normalized_u32(task, "unit_index")
                .expect("validated complete normalized observation above"),
            sco_index: normalized_usize(task, "sco_index")
                .expect("validated complete normalized observation above"),
            unit_visible,
            sco_visible,
            visible,
            completion,
            target_seconds: None,
        });
    }

    let (aggregate_duration_seconds, discarded_remainder_seconds) =
        if flow == WellearnBatchFlow::AutoDuration {
            let minutes = auto_duration_minutes.expect("validated above");
            if minutes == 0 || minutes > MAX_AUTO_DURATION_MINUTES {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "WELearn Auto duration is outside the bounded aggregate limit",
                ));
            }
            let aggregate = minutes.checked_mul(60).ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "WELearn Auto duration exceeds the bounded aggregate limit",
                )
            })?;
            let count = u64::try_from(entries.len()).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "WELearn batch entry count exceeds the bounded limit",
                )
            })?;
            if count == 0 {
                return Err(ProviderError::new(
                    ProviderErrorKind::UnsupportedTask,
                    "WELearn Auto duration has no visible SCO tasks",
                ));
            }
            let child = aggregate / count;
            let remainder = aggregate % count;
            for entry in &mut entries {
                entry.target_seconds = Some(child);
            }
            (Some(aggregate), remainder)
        } else {
            (None, 0)
        };

    if entries.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "WELearn batch selection contains no donor-eligible SCO tasks",
        ));
    }
    let plan = WellearnBatchPlan {
        course_remote_id,
        flow,
        dispatch: flow.dispatch(),
        target_strategy: flow.target_strategy(),
        execution_shape: flow.execution_shape(),
        atomic_completion_profile: flow.atomic_completion_profile(),
        selection,
        selected_units,
        entries,
        aggregate_duration_seconds,
        discarded_remainder_seconds,
    };
    validate_batch_plan_integrity(&plan)?;
    Ok(plan)
}

fn select_units(
    units: &[WellearnUnitObservation],
    selection: &WellearnBatchUnitSelection,
) -> ProviderResult<Vec<WellearnUnitObservation>> {
    if units.is_empty() || units.len() > 512 {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn batch Unit inventory is empty or exceeds the item limit",
        ));
    }
    let mut known = BTreeMap::new();
    for (ordinal, unit) in units.iter().enumerate() {
        if unit.title.is_empty()
            || unit.title.len() > 512
            || unit.title.chars().any(char::is_control)
            || unit.code.as_ref().is_some_and(|code| {
                code.is_empty() || code.len() > 512 || code.chars().any(char::is_control)
            })
            || usize::try_from(unit.index).ok() != Some(ordinal)
            || known.insert(unit.index, unit).is_some()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch Unit inventory contains an invalid or duplicate observation",
            ));
        }
    }
    let indices = match selection {
        WellearnBatchUnitSelection::All => units.iter().map(|unit| unit.index).collect(),
        WellearnBatchUnitSelection::Explicit(indices) => indices.clone(),
    };
    if indices.is_empty() || indices.len() > units.len() {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "WELearn batch Unit selection is empty or exceeds the fresh inventory",
        ));
    }
    let mut seen = BTreeSet::new();
    indices
        .into_iter()
        .map(|index| {
            if !seen.insert(index) {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn batch Unit selection contains a duplicate index",
                ));
            }
            known
                .get(&index)
                .map(|unit| (*unit).clone())
                .ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::RemoteChanged,
                        "WELearn batch Unit selection references a missing fresh Unit",
                    )
                })
        })
        .collect()
}

fn validate_task_unit_observation(
    task: &RemoteTask,
    known_units: &BTreeMap<u32, &WellearnUnitObservation>,
) -> ProviderResult<()> {
    let unit_index = normalized_u32(task, "unit_index")
        .expect("validated complete normalized observation above");
    let unit = known_units.get(&unit_index).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn batch task references an unknown Unit observation",
        )
    })?;
    let title = task
        .normalized
        .get("unit_title")
        .and_then(serde_json::Value::as_str);
    let code = task
        .normalized
        .get("unit_code")
        .and_then(serde_json::Value::as_str);
    if title != Some(unit.title.as_str())
        || code != unit.code.as_deref()
        || normalized_bool(task, "unit_visible") != Some(unit.visible)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "WELearn batch task no longer matches its fresh Unit observation",
        ));
    }
    Ok(())
}

fn derive_unit_observations(tasks: &[RemoteTask]) -> ProviderResult<Vec<WellearnUnitObservation>> {
    let mut units = BTreeMap::new();
    for task in tasks {
        let index = normalized_u32(task, "unit_index").ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch task has no Unit index",
            )
        })?;
        let title = task
            .normalized
            .get("unit_title")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn batch task has no Unit title",
                )
            })?
            .to_owned();
        let code = match task.normalized.get("unit_code") {
            Some(serde_json::Value::Null) | None => None,
            Some(serde_json::Value::String(value)) if !value.is_empty() => Some(value.clone()),
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn batch task has an invalid Unit code",
                ));
            }
        };
        let visible = normalized_bool(task, "unit_visible").ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch task has no Unit visibility",
            )
        })?;
        let observation = WellearnUnitObservation {
            index,
            title,
            code,
            visible,
        };
        if let Some(existing) = units.insert(index, observation.clone())
            && existing != observation
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "WELearn batch tasks disagree about one Unit observation",
            ));
        }
    }
    Ok(units.into_values().collect())
}

fn compare_selected_task_order(
    left: &RemoteTask,
    right: &RemoteTask,
    selected_ordinals: &BTreeMap<u32, usize>,
) -> Ordering {
    let left_unit = normalized_u32(left, "unit_index")
        .and_then(|index| selected_ordinals.get(&index).copied())
        .unwrap_or(usize::MAX);
    let right_unit = normalized_u32(right, "unit_index")
        .and_then(|index| selected_ordinals.get(&index).copied())
        .unwrap_or(usize::MAX);
    left_unit
        .cmp(&right_unit)
        .then_with(|| {
            normalized_usize(left, "sco_index")
                .unwrap_or(usize::MAX)
                .cmp(&normalized_usize(right, "sco_index").unwrap_or(usize::MAX))
        })
        .then_with(|| left.remote_id.cmp(&right.remote_id))
}

fn split_batch_identity<'a>(
    course_remote_id: &'a str,
    remote_task_id: &'a str,
) -> ProviderResult<(&'a str, &'a str)> {
    let course_id = course_remote_id
        .strip_prefix("course:")
        .filter(|value| valid_batch_id_component(value))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn frozen batch Course identity is invalid",
            )
        })?;
    let prefix = format!("sco:{course_id}:");
    let sco_id = remote_task_id
        .strip_prefix(prefix.as_str())
        .filter(|value| valid_batch_id_component(value))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn frozen batch SCO identity is invalid",
            )
        })?;
    Ok((course_id, sco_id))
}

fn valid_batch_id_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BATCH_ID_COMPONENT_BYTES
        && !value.contains(':')
        && !value.chars().any(char::is_control)
}

fn normalized_bool(task: &RemoteTask, key: &str) -> Option<bool> {
    task.normalized
        .get(key)
        .and_then(serde_json::Value::as_bool)
}

fn has_valid_optional_bool(task: &RemoteTask, key: &str) -> bool {
    task.normalized
        .get(key)
        .is_some_and(|value| value.is_null() || value.is_boolean())
}

fn normalized_u32(task: &RemoteTask, key: &str) -> Option<u32> {
    task.normalized
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn normalized_usize(task: &RemoteTask, key: &str) -> Option<usize> {
    task.normalized
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn normalized_completion(task: &RemoteTask) -> Option<RemoteState> {
    match task
        .normalized
        .get("completion_observation")
        .and_then(serde_json::Value::as_str)
    {
        Some("unknown") => Some(RemoteState::Unknown),
        Some("not_open") => Some(RemoteState::NotOpen),
        Some("pending") => Some(RemoteState::Pending),
        Some("in_progress") => Some(RemoteState::InProgress),
        Some("completed") => Some(RemoteState::Completed),
        Some("expired") => Some(RemoteState::Expired),
        Some("removed") => Some(RemoteState::Removed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use asterism_domain::ProviderAccountId;
    use asterism_provider_api::{
        ExecutionEventSink, ExecutionMutationIssue, ExecutionMutationReceipt,
        ExecutionMutationRecoveryRecord, ExecutionMutationSequenceObservation,
        ExecutionMutationSequencePlan, ExecutionMutationSequenceRecoverySnapshot,
        ExecutionMutationSink, ExecutionMutationVerification, ProviderContext,
        ProviderExecutionLog, ProviderIdentity, ProviderMetadata, ProviderProgress,
        TaskDetailCapability,
    };
    use async_trait::async_trait;

    use super::*;

    fn auto_budget(configured_minutes: u16) -> WellearnAutoDurationBudget {
        WellearnAutoDurationBudget::try_new(configured_minutes, 0, 0).unwrap()
    }
    use crate::{
        WellearnAtomicDurationCompletion, WellearnAtomicDurationCompletionDocuments,
        WellearnAtomicDurationCompletionPlan, WellearnAtomicDurationCompletionReceipts,
        WellearnAtomicDurationCompletionRecovery,
        WellearnAtomicDurationCompletionRecoveryTransport,
        WellearnAtomicDurationCompletionTransport, WellearnAtomicMutationKind,
        WellearnAtomicPreFinalObservation, WellearnCmiDocument, WellearnScoLeavesDocument,
        build_atomic_mutation_sequence_plan, development_metadata, parse_course_inventory,
        parse_task_inventory, parse_unit_inventory,
    };

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");

    fn tasks() -> Vec<RemoteTask> {
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

    fn pending_tasks() -> Vec<RemoteTask> {
        let mut tasks = tasks();
        tasks[1].remote_state = RemoteState::Pending;
        tasks[1].normalized["completion_observation"] = serde_json::json!("pending");
        tasks
    }

    fn pending_hidden_unit_tasks() -> Vec<RemoteTask> {
        let mut tasks = pending_tasks();
        tasks[2].normalized["completion_observation"] = serde_json::json!("pending");
        tasks
    }

    fn units() -> Vec<WellearnUnitObservation> {
        parse_unit_inventory(UNITS).unwrap()
    }

    fn detail(mut task: RemoteTask) -> RemoteTaskDetail {
        task.fingerprint = crate::task_inventory::task_fingerprint(&task.normalized).unwrap();
        let normalized = task.normalized.clone();
        RemoteTaskDetail {
            task,
            normalized_detail: serde_json::json!({
                "schema": "welearn.sco-task-detail.v2",
                "task": normalized,
            }),
        }
    }

    #[derive(Debug)]
    struct AtomicFixtureDetail {
        metadata: ProviderMetadata,
        detail: RemoteTaskDetail,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl ProviderIdentity for AtomicFixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for AtomicFixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            self.calls.lock().unwrap().push(remote_task_id.to_owned());
            Ok(self.detail.clone())
        }
    }

    #[derive(Debug, Default)]
    struct AtomicFixtureTransport {
        calls: Mutex<Vec<(String, String, WellearnAtomicDurationCompletionPlan)>>,
    }

    #[async_trait]
    impl WellearnAtomicDurationCompletionTransport for AtomicFixtureTransport {
        async fn complete_duration_atomically(
            &self,
            _context: &ProviderContext,
            child: &WellearnAtomicChildPlan,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<WellearnAtomicDurationCompletionDocuments> {
            child.validate()?;
            let plan = child.duration_completion_plan()?;
            let (course_id, sco_id) = crate::cmi::parse_sco_identity(child.remote_task_id())?;
            self.calls.lock().unwrap().push((course_id, sco_id, plan));
            let (after_duration, after_completion, receipts) = match plan.profile() {
                WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => (
                    Some(completed_atomic_cmi("100")),
                    completed_atomic_cmi("100"),
                    WellearnAtomicDurationCompletionReceipts::new(
                        true,
                        vec![true; usize::try_from(plan.target_seconds()).unwrap()],
                        Some(true),
                        true,
                    ),
                ),
                WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => (
                    None,
                    completed_atomic_cmi("0"),
                    WellearnAtomicDurationCompletionReceipts::new(
                        false,
                        vec![
                            true;
                            usize::try_from(
                                plan.target_seconds() / plan.heartbeat_interval_seconds()
                            )
                            .unwrap()
                        ],
                        None,
                        false,
                    ),
                ),
            };
            WellearnAtomicDurationCompletionDocuments::try_new(
                plan,
                WellearnCmiDocument::try_new(r#"{"ret":0,"comment":"{}"}"#.to_owned()).unwrap(),
                after_duration,
                after_completion,
                receipts,
            )
        }
    }

    #[derive(Debug)]
    struct AtomicRecoveryFixtureTransport {
        score: &'static str,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl WellearnAtomicDurationCompletionRecoveryTransport for AtomicRecoveryFixtureTransport {
        async fn read_atomic_final(
            &self,
            _context: &ProviderContext,
            child: &WellearnAtomicChildPlan,
        ) -> ProviderResult<WellearnCmiDocument> {
            child.validate()?;
            self.calls
                .lock()
                .unwrap()
                .push(child.remote_task_id().to_owned());
            Ok(completed_atomic_cmi(self.score))
        }
    }

    #[derive(Debug, Default)]
    struct AtomicFixtureEvents {
        fail_verification: bool,
        sequence_plans: Mutex<Vec<[u8; 32]>>,
        verifications: Mutex<Vec<ExecutionMutationVerification>>,
    }

    #[async_trait]
    impl ExecutionMutationSink for AtomicFixtureEvents {
        async fn prepare_sequence_plan(
            &self,
            plan: &ExecutionMutationSequencePlan,
        ) -> ProviderResult<()> {
            self.sequence_plans.lock().unwrap().push(plan.plan_digest());
            Ok(())
        }

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
                    "fixture verification persistence failed",
                ));
            }
            self.verifications.lock().unwrap().push(verification);
            Ok(())
        }
    }

    #[async_trait]
    impl ExecutionEventSink for AtomicFixtureEvents {
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

    fn completed_atomic_cmi(score: &str) -> WellearnCmiDocument {
        WellearnCmiDocument::try_new(
            serde_json::json!({
                "ret": 0,
                "comment": serde_json::json!({
                    "cmi": {
                        "completion_status": "completed",
                        "progress_measure": "1",
                        "score": {"scaled": score},
                        "success_status": "unknown",
                        "session_time": "0",
                        "total_time": "0",
                    }
                })
                .to_string(),
            })
            .to_string(),
        )
        .unwrap()
    }

    fn atomic_sequence_records(
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

    fn atomic_recovery_snapshot(
        artifact: ProviderExecutionPlanArtifact,
        plan: ExecutionMutationSequencePlan,
        issues: &[ExecutionMutationIssue],
        receipts: &[ExecutionMutationReceipt],
        observation: Option<ExecutionMutationSequenceObservation>,
        verification: Option<ExecutionMutationVerification>,
    ) -> ExecutionMutationSequenceRecoverySnapshot {
        let final_index = issues.len().checked_sub(1).unwrap();
        let records = issues
            .iter()
            .cloned()
            .zip(receipts.iter().copied())
            .enumerate()
            .map(|(index, (issue, receipt))| {
                ExecutionMutationRecoveryRecord::try_new(
                    issue,
                    Some(receipt),
                    if index == final_index {
                        verification
                    } else {
                        None
                    },
                )
                .unwrap()
            })
            .collect();
        ExecutionMutationSequenceRecoverySnapshot::try_new(
            artifact,
            plan,
            records,
            observation.into_iter().collect(),
        )
        .unwrap()
    }

    fn atomic_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: Vec::new(),
            correlation_id: "welearn-atomic-prepared".to_owned(),
        }
    }

    #[test]
    fn current_fanyuchang_retains_hidden_and_completed_in_inventory_order() {
        let plan =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangCompletion, None).unwrap();
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.remote_task_id.as_str())
                .collect::<Vec<_>>(),
            ["sco:1001:301", "sco:1001:302", "sco:1001:401"]
        );
        assert!(plan.entries[0].unit_visible);
        assert_eq!(plan.entries[0].sco_visible, Some(true));
        assert!(!plan.entries[2].unit_visible);
        assert_eq!(plan.entries[2].sco_visible, Some(true));
        assert_eq!(plan.dispatch, WellearnBatchDispatch::PerChildConcurrent);
        assert_eq!(plan.target_strategy, WellearnBatchTargetStrategy::PerChild);
    }

    #[test]
    fn yzbrh_completion_skips_hidden_and_completed() {
        let plan =
            build_batch_plan(&pending_tasks(), WellearnBatchFlow::YzbrhCompletion, None).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].remote_task_id, "sco:1001:302");
    }

    #[test]
    fn yzbrh_and_auto_filter_raw_sco_visibility_not_unit_visibility() {
        let tasks = pending_hidden_unit_tasks();
        for flow in [
            WellearnBatchFlow::YzbrhCompletion,
            WellearnBatchFlow::AutoCompletion,
        ] {
            let plan = build_batch_plan(&tasks, flow, None).unwrap();
            assert_eq!(
                plan.entries
                    .iter()
                    .map(|entry| entry.remote_task_id.as_str())
                    .collect::<Vec<_>>(),
                ["sco:1001:302", "sco:1001:401"]
            );
            assert!(!plan.entries[1].unit_visible);
            assert_eq!(plan.entries[1].sco_visible, Some(true));
            assert!(!plan.entries[1].visible);
        }
    }

    #[test]
    fn auto_flows_skip_an_explicitly_hidden_sco() {
        let mut tasks = pending_tasks();
        tasks[1].normalized["sco_visible"] = serde_json::json!(false);
        tasks[1].normalized["visible"] = serde_json::json!(false);
        let plan = build_batch_plan(&tasks, WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.remote_task_id.as_str())
                .collect::<Vec<_>>(),
            ["sco:1001:301", "sco:1001:401"]
        );
    }

    #[test]
    fn yzbrh_duration_keeps_hidden_and_completed_rows() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::YzbrhDuration, None).unwrap();
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.remote_task_id.as_str())
                .collect::<Vec<_>>(),
            ["sco:1001:301", "sco:1001:302", "sco:1001:401"]
        );
        assert_eq!(plan.dispatch, WellearnBatchDispatch::PerChildConcurrent);
        assert_eq!(plan.target_strategy, WellearnBatchTargetStrategy::PerChild);
    }

    #[test]
    fn auto_completion_skips_hidden_and_completed() {
        let plan =
            build_batch_plan(&pending_tasks(), WellearnBatchFlow::AutoCompletion, None).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].remote_task_id, "sco:1001:302");
    }

    #[test]
    fn auto_duration_freezes_one_budget_and_discards_remainder() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        assert_eq!(plan.entries.len(), 3);
        assert_eq!(plan.aggregate_duration_seconds, Some(60));
        assert_eq!(plan.discarded_remainder_seconds, 0);
        assert_eq!(plan.dispatch, WellearnBatchDispatch::BoundedThreadPool);
        assert_eq!(
            plan.target_strategy,
            WellearnBatchTargetStrategy::AggregateEqualFloor
        );
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.target_seconds == Some(20))
        );
    }

    #[test]
    fn auto_legacy_duration_is_visible_sequential_and_per_child() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::AutoLegacyDuration, None).unwrap();
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.remote_task_id.as_str())
                .collect::<Vec<_>>(),
            ["sco:1001:301", "sco:1001:302", "sco:1001:401"]
        );
        assert_eq!(plan.dispatch, WellearnBatchDispatch::Sequential);
        assert_eq!(plan.target_strategy, WellearnBatchTargetStrategy::PerChild);
        assert_eq!(plan.aggregate_duration_seconds, None);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.target_seconds.is_none())
        );
    }

    #[test]
    fn auto_duration_preserves_zero_floor_targets_and_remainder() {
        let template = tasks().remove(0);
        let mut many = Vec::with_capacity(61);
        for index in 0..61_usize {
            let mut task = template.clone();
            task.remote_id = format!("sco:1001:bulk-{index}");
            task.normalized["sco_id"] = serde_json::json!(format!("bulk-{index}"));
            task.normalized["sco_index"] = serde_json::json!(index);
            many.push(task);
        }
        let plan = build_batch_plan(&many, WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        assert_eq!(plan.aggregate_duration_seconds, Some(60));
        assert_eq!(plan.discarded_remainder_seconds, 60);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.target_seconds == Some(0))
        );
    }

    #[test]
    fn atomic_child_plan_freezes_fanyuchang_target_and_round_trips() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let plan = materialize_atomic_child_plan(&batch, 1, Some(37)).unwrap();

        assert_eq!(plan.version(), WELLEARN_ATOMIC_CHILD_PLAN_VERSION);
        assert_eq!(plan.entry_index(), 1);
        assert_eq!(plan.course_remote_id(), "course:1001");
        assert_eq!(plan.remote_task_id(), batch.entries[1].remote_task_id);
        assert_eq!(plan.flow(), WellearnBatchFlow::FanyuchangDuration);
        assert_eq!(
            plan.execution_shape(),
            WellearnBatchExecutionShape::AtomicDurationCompletion
        );
        assert_eq!(
            plan.atomic_completion_profile(),
            WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100
        );
        assert_eq!(plan.target_seconds(), 37);

        let encoded = plan.encode().unwrap();
        assert!(encoded.len() <= MAX_ATOMIC_CHILD_PLAN_BYTES);
        assert_eq!(WellearnAtomicChildPlan::decode(&encoded).unwrap(), plan);
        assert_eq!(
            WellearnAtomicChildPlan::decode_bound(&encoded, &batch, 1, Some(37)).unwrap(),
            plan
        );
        assert!(WellearnAtomicChildPlan::decode_bound(&encoded, &batch, 0, Some(37)).is_err());
        assert!(WellearnAtomicChildPlan::decode_bound(&encoded, &batch, 1, Some(38)).is_err());
        assert_eq!(
            serde_json::from_slice::<WellearnAtomicChildPlan>(&encoded).unwrap(),
            plan
        );
    }

    #[test]
    fn atomic_child_plan_converts_to_credential_free_core_artifact_and_rebinds() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let plan = materialize_atomic_child_plan(&batch, 1, Some(37)).unwrap();
        let artifact = plan.to_provider_execution_plan_artifact().unwrap();

        assert_eq!(artifact.provider_id().as_str(), PROVIDER_ID);
        assert_eq!(
            artifact.artifact_type(),
            WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE
        );
        assert_ne!(artifact.artifact_digest(), [0; 32]);
        let keys = artifact
            .payload_sanitized()
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "atomic_completion_profile",
                "course_remote_id",
                "entry_index",
                "execution_shape",
                "flow",
                "remote_task_id",
                "target_seconds",
                "version",
            ])
        );
        let debug = format!("{artifact:?}");
        assert!(debug.contains("[HASHED]") && debug.contains("[REDACTED]"));
        assert_eq!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &artifact,
                &batch,
                1,
                Some(37),
            )
            .unwrap(),
            plan
        );
    }

    #[test]
    fn prepared_atomic_child_restores_only_exact_durable_facts() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let child = materialize_atomic_child_plan(&batch, 1, Some(37)).unwrap();
        let artifact = child.to_provider_execution_plan_artifact().unwrap();
        let prepared =
            WellearnPreparedAtomicChildPlan::restore_from_provider_execution_plan_artifact(
                batch.clone(),
                1,
                Some(37),
                &artifact,
            )
            .unwrap();
        assert_eq!(prepared.batch_plan(), &batch);
        assert_eq!(prepared.entry_index(), 1);
        assert_eq!(prepared.child_plan(), &child);

        for (entry_index, target) in [(0, Some(37)), (1, Some(38)), (1, None)] {
            assert!(
                WellearnPreparedAtomicChildPlan::restore_from_provider_execution_plan_artifact(
                    batch.clone(),
                    entry_index,
                    target,
                    &artifact,
                )
                .is_err()
            );
        }

        let mut drifted_batch = batch;
        drifted_batch.entries[1].unit_index = 99;
        assert!(
            WellearnPreparedAtomicChildPlan::restore_from_provider_execution_plan_artifact(
                drifted_batch,
                1,
                Some(37),
                &artifact,
            )
            .is_err()
        );
    }

    #[test]
    fn prepared_atomic_child_restores_all_durable_artifacts_together() {
        let fanyuchang_authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:301",
            Some(37),
            None,
        )
        .unwrap();
        let fanyuchang = prepare_atomic_child_plan_from_fresh_inventory(
            &tasks(),
            &units(),
            &fanyuchang_authority,
        )
        .unwrap();
        let fanyuchang_artifact = fanyuchang.provider_plan_artifact().unwrap();
        let restored = WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
            &fanyuchang_authority.encode().unwrap(),
            &fanyuchang.batch_plan().encode_snapshot().unwrap(),
            &fanyuchang_artifact,
        )
        .unwrap();
        assert_eq!(restored, fanyuchang);

        let auto_authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:302",
            None,
            Some(WellearnAutoDurationBudget::try_new(1, 0, 0).unwrap()),
        )
        .unwrap();
        let auto =
            prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &auto_authority)
                .unwrap();
        let auto_artifact = auto.provider_plan_artifact().unwrap();
        assert_eq!(
            WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
                &auto_authority.encode().unwrap(),
                &auto.batch_plan().encode_snapshot().unwrap(),
                &auto_artifact,
            )
            .unwrap(),
            auto
        );
    }

    #[test]
    fn durable_atomic_child_restore_rejects_parent_identity_and_selection_drift() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:301",
            Some(37),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &authority).unwrap();
        let artifact = prepared.provider_plan_artifact().unwrap();
        for mismatched_authority in [
            WellearnAtomicBatchPlanningAuthority::try_new(
                "course:2002",
                WellearnBatchFlow::FanyuchangDuration,
                WellearnBatchUnitSelection::Explicit(vec![1, 0]),
                "sco:2002:301",
                Some(37),
                None,
            )
            .unwrap(),
            WellearnAtomicBatchPlanningAuthority::try_new(
                "course:1001",
                WellearnBatchFlow::FanyuchangDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                Some(37),
                None,
            )
            .unwrap(),
        ] {
            assert!(
                WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
                    &mismatched_authority.encode().unwrap(),
                    &prepared.batch_plan().encode_snapshot().unwrap(),
                    &artifact,
                )
                .is_err()
            );
        }

        let missing_child = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:missing",
            Some(37),
            None,
        )
        .unwrap();
        assert!(
            WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
                &missing_child.encode().unwrap(),
                &prepared.batch_plan().encode_snapshot().unwrap(),
                &artifact,
            )
            .is_err()
        );
    }

    #[test]
    fn durable_atomic_child_restore_rejects_auto_aggregate_or_child_drift() {
        let auto_authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:302",
            None,
            Some(WellearnAutoDurationBudget::try_new(1, 0, 0).unwrap()),
        )
        .unwrap();
        let auto =
            prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &auto_authority)
                .unwrap();
        let auto_artifact = auto.provider_plan_artifact().unwrap();
        let other_child = materialize_atomic_child_plan(auto.batch_plan(), 0, None)
            .unwrap()
            .to_provider_execution_plan_artifact()
            .unwrap();
        assert!(
            WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
                &auto_authority.encode().unwrap(),
                &auto.batch_plan().encode_snapshot().unwrap(),
                &other_child,
            )
            .is_err()
        );
        let mismatched_auto_budget = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:302",
            None,
            Some(WellearnAutoDurationBudget::try_new(2, 0, 0).unwrap()),
        )
        .unwrap();
        assert!(
            WellearnPreparedAtomicChildPlan::restore_from_durable_artifacts(
                &mismatched_auto_budget.encode().unwrap(),
                &auto.batch_plan().encode_snapshot().unwrap(),
                &auto_artifact,
            )
            .is_err()
        );
    }

    #[test]
    fn fresh_atomic_planning_rebuilds_exact_ordered_fanyuchang_selection() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:301",
            Some(37),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &authority).unwrap();

        assert_eq!(authority.course_remote_id(), "course:1001");
        assert_eq!(authority.flow(), WellearnBatchFlow::FanyuchangDuration);
        assert_eq!(
            authority.selection(),
            &WellearnBatchUnitSelection::Explicit(vec![1, 0])
        );
        assert_eq!(authority.expected_remote_task_id(), "sco:1001:301");
        assert_eq!(authority.frozen_fanyuchang_target_seconds(), Some(37));
        assert_eq!(authority.frozen_auto_duration_minutes(), None);
        assert_eq!(
            prepared.batch_plan().selection,
            WellearnBatchUnitSelection::Explicit(vec![1, 0])
        );
        assert_eq!(
            prepared
                .batch_plan()
                .selected_units
                .iter()
                .map(|unit| unit.index)
                .collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(prepared.entry_index(), 1);
        assert_eq!(prepared.child_plan().remote_task_id(), "sco:1001:301");
        assert_eq!(prepared.child_plan().target_seconds(), 37);
        let artifact = prepared.provider_plan_artifact().unwrap();
        assert_eq!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &artifact,
                prepared.batch_plan(),
                prepared.entry_index(),
                Some(37),
            )
            .unwrap(),
            prepared.child_plan().clone()
        );
    }

    #[test]
    fn auto_duration_budget_retains_configured_range_and_frozen_sample() {
        let minimum = WellearnAutoDurationBudget::try_new(1, 30, -30).unwrap();
        assert_eq!(minimum.configured_minutes(), 1);
        assert_eq!(minimum.random_range_minutes(), 30);
        assert_eq!(minimum.sampled_offset_minutes(), -30);
        assert_eq!(minimum.actual_minutes(), 1);

        let maximum = WellearnAutoDurationBudget::try_new(300, 30, 30).unwrap();
        assert_eq!(maximum.actual_minutes(), 330);

        for invalid in [
            WellearnAutoDurationBudget::try_new(0, 0, 0),
            WellearnAutoDurationBudget::try_new(301, 0, 0),
            WellearnAutoDurationBudget::try_new(60, 31, 0),
            WellearnAutoDurationBudget::try_new(60, 5, -6),
            WellearnAutoDurationBudget::try_new(60, 5, 6),
        ] {
            assert!(invalid.is_err());
        }
    }

    #[test]
    fn atomic_parent_authority_round_trips_every_donor_target_shape() {
        let fanyuchang = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:301",
            Some(0),
            None,
        )
        .unwrap();
        assert_eq!(
            WellearnAtomicBatchPlanningAuthority::decode(&fanyuchang.encode().unwrap()).unwrap(),
            fanyuchang
        );

        let auto = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            None,
            Some(WellearnAutoDurationBudget::try_new(60, 5, -5).unwrap()),
        )
        .unwrap();
        let encoded = auto.encode().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "course_remote_id",
                "expected_remote_task_id",
                "flow",
                "frozen_auto_duration_budget",
                "frozen_fanyuchang_target_seconds",
                "selection",
                "version",
            ])
        );
        assert_eq!(
            value["frozen_auto_duration_budget"]["configured_minutes"],
            60
        );
        assert_eq!(
            value["frozen_auto_duration_budget"]["random_range_minutes"],
            5
        );
        assert_eq!(
            value["frozen_auto_duration_budget"]["sampled_offset_minutes"],
            -5
        );
        assert_eq!(value["frozen_auto_duration_budget"]["actual_minutes"], 55);
        assert_eq!(
            WellearnAtomicBatchPlanningAuthority::decode(&encoded).unwrap(),
            auto
        );
    }

    #[test]
    fn atomic_parent_and_complete_batch_convert_to_core_encrypted_snapshot() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:301",
            Some(37),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &authority).unwrap();
        let targets = [0, 37, 19_800];

        let snapshot = authority
            .to_execution_parent_batch_snapshot(prepared.batch_plan(), Some(&targets))
            .unwrap();
        assert_eq!(snapshot.provider_id().as_str(), PROVIDER_ID);
        assert_eq!(
            snapshot.authority_type(),
            WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE
        );
        assert_eq!(snapshot.batch_type(), WELLEARN_ATOMIC_BATCH_SNAPSHOT_TYPE);
        let authority_value: serde_json::Value =
            serde_json::from_slice(snapshot.authority().expose_secret()).unwrap();
        assert_eq!(authority_value["version"], 2);
        assert_eq!(authority_value["frozen_fanyuchang_target_count"], 3);
        assert!(authority_value["frozen_fanyuchang_targets_digest"].is_array());
        assert!(
            authority_value
                .get("frozen_fanyuchang_target_seconds")
                .is_none()
        );
        let (restored_authority, restored_batch, restored_targets) =
            decode_execution_parent_batch_snapshot(&snapshot).unwrap();
        assert_eq!(restored_authority, authority);
        assert_eq!(restored_batch, prepared.batch_plan().clone());
        assert_eq!(restored_targets.as_deref(), Some(targets.as_slice()));
        assert_ne!(snapshot.authority_digest(), [0; 32]);
        assert_ne!(snapshot.batch_digest(), [0; 32]);

        let replay = authority
            .to_execution_parent_batch_snapshot(prepared.batch_plan(), Some(&targets))
            .unwrap();
        assert_eq!(snapshot.authority_digest(), replay.authority_digest());
        assert_eq!(snapshot.batch_digest(), replay.batch_digest());

        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("course:1001"));
        assert!(!debug.contains("sco:1001:301"));
        assert!(!debug.contains("37"));
    }

    #[test]
    fn complete_parent_snapshot_rejects_any_fanyuchang_target_substitution() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(0),
            None,
        )
        .unwrap();
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let targets = [0, 37, 19_800];
        let snapshot = authority
            .to_execution_parent_batch_snapshot(&batch, Some(&targets))
            .unwrap();
        let original: serde_json::Value =
            serde_json::from_slice(snapshot.batch().expose_secret()).unwrap();

        for drifted_targets in [
            serde_json::json!([0, 37, 19_799]),
            serde_json::json!([37, 0, 19_800]),
        ] {
            let mut drifted = original.clone();
            drifted["frozen_fanyuchang_target_seconds"] = drifted_targets;
            let drifted = ExecutionParentBatchSnapshot::try_new(
                ProviderId::new(PROVIDER_ID).unwrap(),
                WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE,
                SecretValue::new(snapshot.authority().expose_secret().to_vec()),
                WELLEARN_ATOMIC_BATCH_SNAPSHOT_TYPE,
                SecretValue::new(serde_json::to_vec(&drifted).unwrap()),
            )
            .unwrap();
            assert!(decode_execution_parent_batch_snapshot(&drifted).is_err());
        }

        let original_authority: serde_json::Value =
            serde_json::from_slice(snapshot.authority().expose_secret()).unwrap();
        for (field, value) in [
            ("frozen_fanyuchang_target_count", serde_json::json!(2)),
            (
                "frozen_fanyuchang_targets_digest",
                serde_json::Value::Array(vec![serde_json::json!(0); 32]),
            ),
            ("version", serde_json::json!(3)),
        ] {
            let mut drifted = original_authority.clone();
            drifted[field] = value;
            let drifted = ExecutionParentBatchSnapshot::try_new(
                ProviderId::new(PROVIDER_ID).unwrap(),
                WELLEARN_ATOMIC_BATCH_PLANNING_AUTHORITY_TYPE,
                SecretValue::new(serde_json::to_vec(&drifted).unwrap()),
                WELLEARN_ATOMIC_BATCH_SNAPSHOT_TYPE,
                SecretValue::new(snapshot.batch().expose_secret().to_vec()),
            )
            .unwrap();
            assert!(decode_execution_parent_batch_snapshot(&drifted).is_err());
        }
    }

    #[test]
    fn core_parent_batch_snapshot_rejects_cross_selection_and_aggregate_drift() {
        let fanyuchang = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:301",
            Some(37),
            None,
        )
        .unwrap();
        let all_units =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        assert!(
            fanyuchang
                .to_execution_parent_batch_snapshot(&all_units, Some(&[0, 37, 19_800]))
                .is_err()
        );

        let auto = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:302",
            None,
            Some(auto_budget(2)),
        )
        .unwrap();
        let wrong_aggregate =
            build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        assert!(
            auto.to_execution_parent_batch_snapshot(&wrong_aggregate, None)
                .is_err()
        );
    }

    #[test]
    fn atomic_parent_authority_decode_rejects_schema_and_semantic_drift() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::Explicit(vec![0, 1]),
            "sco:1001:301",
            None,
            Some(WellearnAutoDurationBudget::try_new(60, 5, -5).unwrap()),
        )
        .unwrap();
        let original: serde_json::Value =
            serde_json::from_slice(&authority.encode().unwrap()).unwrap();

        let mut drifted = original.clone();
        drifted["version"] = serde_json::json!(2);
        assert!(
            WellearnAtomicBatchPlanningAuthority::decode(&serde_json::to_vec(&drifted).unwrap())
                .is_err()
        );

        let mut drifted = original.clone();
        drifted["frozen_auto_duration_budget"]["actual_minutes"] = serde_json::json!(56);
        assert!(
            WellearnAtomicBatchPlanningAuthority::decode(&serde_json::to_vec(&drifted).unwrap())
                .is_err()
        );

        let mut drifted = original.clone();
        drifted["flow"] = serde_json::json!("fanyuchang_duration");
        assert!(
            WellearnAtomicBatchPlanningAuthority::decode(&serde_json::to_vec(&drifted).unwrap())
                .is_err()
        );

        let mut drifted = original.clone();
        drifted["selection"] = serde_json::json!({"explicit": [0, 0]});
        assert!(
            WellearnAtomicBatchPlanningAuthority::decode(&serde_json::to_vec(&drifted).unwrap())
                .is_err()
        );

        let mut drifted = original;
        drifted["unknown"] = serde_json::json!(true);
        assert!(
            WellearnAtomicBatchPlanningAuthority::decode(&serde_json::to_vec(&drifted).unwrap())
                .is_err()
        );
        assert!(WellearnAtomicBatchPlanningAuthority::decode(&[]).is_err());
        assert!(
            WellearnAtomicBatchPlanningAuthority::decode(&vec![
                b'x';
                MAX_ATOMIC_BATCH_PLANNING_AUTHORITY_BYTES
                    + 1
            ])
            .is_err()
        );
    }

    #[test]
    fn atomic_parent_authority_maximum_selection_stays_within_local_bound() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit((0..512).collect()),
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let encoded = authority.encode().unwrap();
        assert!(encoded.len() <= MAX_ATOMIC_BATCH_PLANNING_AUTHORITY_BYTES);
        assert_eq!(
            WellearnAtomicBatchPlanningAuthority::decode(&encoded).unwrap(),
            authority
        );
    }

    #[test]
    fn fresh_atomic_planning_preserves_auto_zero_floor_from_full_membership() {
        let template = tasks().remove(0);
        let mut many = Vec::with_capacity(61);
        for index in 0..61_usize {
            let mut task = template.clone();
            task.remote_id = format!("sco:1001:planning-{index}");
            task.normalized["sco_id"] = serde_json::json!(format!("planning-{index}"));
            task.normalized["sco_index"] = serde_json::json!(index);
            many.push(task);
        }
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::Explicit(vec![0]),
            "sco:1001:planning-60",
            None,
            Some(auto_budget(1)),
        )
        .unwrap();
        assert_eq!(
            authority.frozen_auto_duration_budget(),
            Some(auto_budget(1))
        );
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&many, &units(), &authority).unwrap();

        assert_eq!(prepared.batch_plan().entries.len(), 61);
        assert_eq!(prepared.batch_plan().aggregate_duration_seconds, Some(60));
        assert_eq!(prepared.child_plan().target_seconds(), 0);
        assert_eq!(
            prepared
                .provider_plan_artifact()
                .unwrap()
                .payload_sanitized()["target_seconds"],
            0
        );
    }

    #[tokio::test]
    async fn prepared_atomic_executor_fresh_rebinds_calls_once_and_verifies() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            None,
            Some(auto_budget(1)),
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let detail_calls = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(AtomicFixtureTransport::default());
        let executor = WellearnAtomicDurationCompletion::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[0].clone()),
                calls: Arc::clone(&detail_calls),
            }),
            transport.clone(),
        )
        .unwrap();

        let events = AtomicFixtureEvents::default();
        let outcome = executor
            .execute_prepared(&atomic_context(), &prepared, &events)
            .await
            .unwrap();
        let durable_events = AtomicFixtureEvents::default();
        let durable_outcome = executor
            .execute_durable_artifacts(
                &atomic_context(),
                &authority.encode().unwrap(),
                &prepared.batch_plan().encode_snapshot().unwrap(),
                &prepared.provider_plan_artifact().unwrap(),
                &durable_events,
            )
            .await
            .unwrap();
        let core_batch =
            crate::prepare_atomic_execution_batch_plan(&authority, prepared.batch_plan(), None)
                .unwrap();
        let core_child = core_batch
            .execution_batch_plan()
            .children()
            .iter()
            .find(|child| child.remote_task_id() == prepared.child_plan().remote_task_id())
            .unwrap();
        let core_events = AtomicFixtureEvents::default();
        let core_outcome = executor
            .execute_core_child_plan(
                &atomic_context(),
                core_batch.parent_snapshot(),
                core_child,
                &core_events,
            )
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert_eq!(durable_outcome.remote_state, outcome.remote_state);
        assert_eq!(durable_outcome.verified, outcome.verified);
        assert_eq!(durable_outcome.result_sanitized, outcome.result_sanitized);
        assert_eq!(core_outcome.result_sanitized, outcome.result_sanitized);
        assert_eq!(
            outcome.result_sanitized["schema"],
            "welearn.atomic-duration-completion.v1"
        );
        assert_eq!(
            outcome.result_sanitized["profile"],
            "auto_zero_time_save_only_0"
        );
        assert_eq!(outcome.result_sanitized["target_seconds"], 20);
        assert_eq!(outcome.result_sanitized["score_percent"], 0);
        assert_eq!(outcome.result_sanitized["final_save_ordinal"], 2);
        assert_eq!(
            outcome.result_sanitized["final_save_verification_recorded"],
            false
        );
        assert!(events.verifications.lock().unwrap().is_empty());
        assert_eq!(events.sequence_plans.lock().unwrap().len(), 1);
        assert!(durable_events.verifications.lock().unwrap().is_empty());
        assert_eq!(durable_events.sequence_plans.lock().unwrap().len(), 1);
        assert_eq!(core_events.sequence_plans.lock().unwrap().len(), 1);
        assert_eq!(detail_calls.lock().unwrap().len(), 3);
        let calls = transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|call| {
            call == &(
                "1001".to_owned(),
                "301".to_owned(),
                prepared.child_plan().duration_completion_plan().unwrap(),
            )
        }));
    }

    #[tokio::test]
    async fn prepared_atomic_executor_verifies_fanyuchang_fresh_time_profile() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let transport = Arc::new(AtomicFixtureTransport::default());
        let executor = WellearnAtomicDurationCompletion::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[0].clone()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }),
            transport.clone(),
        )
        .unwrap();

        let events = AtomicFixtureEvents::default();
        let outcome = executor
            .execute_prepared(&atomic_context(), &prepared, &events)
            .await
            .unwrap();

        assert!(outcome.verified);
        assert_eq!(
            outcome.result_sanitized["profile"],
            "fanyuchang_fresh_set_save_100"
        );
        assert_eq!(outcome.result_sanitized["score_percent"], 100);
        assert_eq!(outcome.result_sanitized["time_preservation_verified"], true);
        assert_eq!(outcome.result_sanitized["heartbeat_count"], 1);
        assert_eq!(outcome.result_sanitized["final_save_ordinal"], 4);
        assert_eq!(
            outcome.result_sanitized["final_save_verification_recorded"],
            true
        );
        assert_eq!(events.sequence_plans.lock().unwrap().len(), 1);
        let verifications = events.verifications.lock().unwrap();
        assert_eq!(verifications.len(), 1);
        assert_eq!(verifications[0].ordinal(), 4);
        assert!(verifications[0].verified());
        assert_ne!(verifications[0].observation_digest(), [0; 32]);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prepared_atomic_recovery_fresh_rebinds_then_reads_without_mutation() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let sequence_plan = build_atomic_mutation_sequence_plan(
            prepared.child_plan(),
            &prepared.provider_plan_artifact().unwrap(),
        )
        .unwrap();
        let (issues, receipts) = atomic_sequence_records(&[
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ]);
        let observation = WellearnAtomicPreFinalObservation::capture(
            prepared.child_plan(),
            &completed_atomic_cmi("100"),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let detail_calls = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(AtomicRecoveryFixtureTransport {
            score: "100",
            calls: Mutex::new(Vec::new()),
        });
        let recovery = WellearnAtomicDurationCompletionRecovery::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[0].clone()),
                calls: Arc::clone(&detail_calls),
            }),
            transport.clone(),
        )
        .unwrap();

        let verification = recovery
            .verify_prepared(
                &atomic_context(),
                &prepared,
                &sequence_plan,
                &issues,
                &receipts,
                Some(&observation),
            )
            .await
            .unwrap();

        assert_eq!(verification.final_save_ordinal(), 4);
        assert_eq!(verification.time_preservation_verified(), Some(true));
        let durable_verification = recovery
            .verify_durable_artifacts(
                &atomic_context(),
                &authority.encode().unwrap(),
                &prepared.batch_plan().encode_snapshot().unwrap(),
                &prepared.provider_plan_artifact().unwrap(),
                &sequence_plan,
                &issues,
                &receipts,
                Some(&observation),
            )
            .await
            .unwrap();
        assert_eq!(durable_verification, verification);
        assert_eq!(
            detail_calls.lock().unwrap().as_slice(),
            &["sco:1001:301", "sco:1001:301"]
        );
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &["sco:1001:301", "sco:1001:301"]
        );
    }

    #[tokio::test]
    async fn prepared_atomic_recovery_stops_record_or_fresh_drift_before_final_read() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let sequence_plan = build_atomic_mutation_sequence_plan(
            prepared.child_plan(),
            &prepared.provider_plan_artifact().unwrap(),
        )
        .unwrap();
        let (issues, mut receipts) = atomic_sequence_records(&[
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ]);
        let observation = WellearnAtomicPreFinalObservation::capture(
            prepared.child_plan(),
            &completed_atomic_cmi("100"),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let detail_calls = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(AtomicRecoveryFixtureTransport {
            score: "100",
            calls: Mutex::new(Vec::new()),
        });
        let recovery = WellearnAtomicDurationCompletionRecovery::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[1].clone()),
                calls: Arc::clone(&detail_calls),
            }),
            transport.clone(),
        )
        .unwrap();

        receipts[1] = ExecutionMutationReceipt::new(3, [2; 32], false).unwrap();
        assert!(
            recovery
                .verify_prepared(
                    &atomic_context(),
                    &prepared,
                    &sequence_plan,
                    &issues,
                    &receipts,
                    Some(&observation),
                )
                .await
                .is_err()
        );
        assert!(detail_calls.lock().unwrap().is_empty());
        assert!(transport.calls.lock().unwrap().is_empty());

        receipts[1] = ExecutionMutationReceipt::new(2, [2; 32], false).unwrap();
        assert!(
            recovery
                .verify_prepared(
                    &atomic_context(),
                    &prepared,
                    &sequence_plan,
                    &issues,
                    &receipts,
                    Some(&observation),
                )
                .await
                .is_err()
        );
        assert_eq!(detail_calls.lock().unwrap().len(), 1);
        assert!(transport.calls.lock().unwrap().is_empty());

        assert!(
            recovery
                .verify_durable_artifacts(
                    &atomic_context(),
                    b"{}",
                    &prepared.batch_plan().encode_snapshot().unwrap(),
                    &prepared.provider_plan_artifact().unwrap(),
                    &sequence_plan,
                    &issues,
                    &receipts,
                    Some(&observation),
                )
                .await
                .is_err()
        );
        assert_eq!(detail_calls.lock().unwrap().len(), 1);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario compares every recovery result state against the same prepared child"
    )]
    async fn recovery_snapshot_prepared_and_durable_entries_prove_the_same_goal() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let plan = build_atomic_mutation_sequence_plan(
            prepared.child_plan(),
            &prepared.provider_plan_artifact().unwrap(),
        )
        .unwrap();
        let (issues, receipts) = atomic_sequence_records(&[
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ]);
        let observation = WellearnAtomicPreFinalObservation::capture(
            prepared.child_plan(),
            &completed_atomic_cmi("100"),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let snapshot = atomic_recovery_snapshot(
            prepared.provider_plan_artifact().unwrap(),
            plan.clone(),
            &issues,
            &receipts,
            Some(observation.clone()),
            None,
        );
        let detail_calls = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(AtomicRecoveryFixtureTransport {
            score: "100",
            calls: Mutex::new(Vec::new()),
        });
        let recovery = WellearnAtomicDurationCompletionRecovery::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[0].clone()),
                calls: Arc::clone(&detail_calls),
            }),
            transport.clone(),
        )
        .unwrap();

        let prepared_recovery = recovery
            .verify_execution_recovery(&atomic_context(), &prepared, &snapshot)
            .await
            .unwrap();
        let durable_recovery = recovery
            .verify_durable_snapshot(
                &atomic_context(),
                &authority.encode().unwrap(),
                &prepared.batch_plan().encode_snapshot().unwrap(),
                &snapshot,
            )
            .await
            .unwrap();
        assert_eq!(prepared_recovery, durable_recovery);
        let targets = [1, 2, 3];
        let core_batch = crate::prepare_atomic_execution_batch_plan(
            &authority,
            prepared.batch_plan(),
            Some(&targets),
        )
        .unwrap();
        let core_child = core_batch
            .execution_batch_plan()
            .children()
            .iter()
            .find(|child| child.remote_task_id() == prepared.child_plan().remote_task_id())
            .unwrap();
        let core_recovery = recovery
            .verify_core_child_snapshot(
                &atomic_context(),
                core_batch.parent_snapshot(),
                core_child,
                &snapshot,
            )
            .await
            .unwrap();
        assert_eq!(prepared_recovery, core_recovery);
        let (outcome, pending_verification) = prepared_recovery.into_parts();
        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["final_save_ordinal"], 4);
        assert_eq!(
            outcome.result_sanitized["final_save_verification_recorded"],
            false
        );
        let pending_verification = pending_verification.unwrap();
        assert_eq!(pending_verification.ordinal(), 4);
        assert!(pending_verification.verified());
        let direct_proof =
            crate::verify_atomic_duration_completion_recovery_from_sequence_snapshot(
                prepared.child_plan(),
                &snapshot,
                &completed_atomic_cmi("100"),
            )
            .unwrap();
        assert_eq!(
            pending_verification.observation_digest(),
            direct_proof.observation_digest()
        );
        assert_eq!(detail_calls.lock().unwrap().len(), 3);
        assert_eq!(transport.calls.lock().unwrap().len(), 3);

        let persisted_snapshot = atomic_recovery_snapshot(
            prepared.provider_plan_artifact().unwrap(),
            plan.clone(),
            &issues,
            &receipts,
            Some(observation.clone()),
            Some(pending_verification),
        );
        let persisted_recovery = recovery
            .verify_prepared_snapshot(&atomic_context(), &prepared, &persisted_snapshot)
            .await
            .unwrap();
        assert!(persisted_recovery.mutation_verification().is_none());
        assert_eq!(
            persisted_recovery.outcome().result_sanitized["final_save_verification_recorded"],
            true
        );

        let mut rejected_receipts = receipts.clone();
        rejected_receipts[3] = ExecutionMutationReceipt::new(4, [2; 32], false).unwrap();
        let rejected_snapshot = atomic_recovery_snapshot(
            prepared.provider_plan_artifact().unwrap(),
            plan.clone(),
            &issues,
            &rejected_receipts,
            Some(observation),
            None,
        );
        let rejected_recovery = recovery
            .verify_prepared_snapshot(&atomic_context(), &prepared, &rejected_snapshot)
            .await
            .unwrap();
        assert!(rejected_recovery.mutation_verification().is_none());
        assert_eq!(
            rejected_recovery.outcome().result_sanitized["final_save_verification_recorded"],
            false
        );
        assert_eq!(
            rejected_recovery.outcome().result_sanitized["save_accepted"],
            false
        );

        let mut retryable_receipts = receipts;
        retryable_receipts[1] =
            ExecutionMutationReceipt::new_retryable_rejection(2, [2; 32], 60).unwrap();
        let retryable_snapshot = atomic_recovery_snapshot(
            prepared.provider_plan_artifact().unwrap(),
            plan,
            &issues,
            &retryable_receipts,
            snapshot.observations().first().cloned(),
            None,
        );
        assert!(
            recovery
                .verify_prepared_snapshot(&atomic_context(), &prepared, &retryable_snapshot)
                .await
                .is_err()
        );
        assert_eq!(detail_calls.lock().unwrap().len(), 5);
        assert_eq!(transport.calls.lock().unwrap().len(), 5);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture proves both invalid durable-record shapes stop before fresh I/O"
    )]
    async fn recovery_snapshot_rejects_ambiguous_final_issue_before_fresh_io() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let plan = build_atomic_mutation_sequence_plan(
            prepared.child_plan(),
            &prepared.provider_plan_artifact().unwrap(),
        )
        .unwrap();
        let (issues, receipts) = atomic_sequence_records(&[
            (WellearnAtomicMutationKind::Start, true),
            (WellearnAtomicMutationKind::CounterKeep, false),
            (WellearnAtomicMutationKind::Set, true),
            (WellearnAtomicMutationKind::Save, true),
        ]);
        let observation = WellearnAtomicPreFinalObservation::capture(
            prepared.child_plan(),
            &completed_atomic_cmi("100"),
        )
        .unwrap()
        .to_sequence_observation()
        .unwrap();
        let early_records = issues
            .iter()
            .cloned()
            .zip(receipts.iter().copied())
            .enumerate()
            .map(|(index, (issue, receipt))| {
                ExecutionMutationRecoveryRecord::try_new(
                    issue,
                    Some(receipt),
                    (index == 0)
                        .then(|| ExecutionMutationVerification::new(1, [9; 32], true).unwrap()),
                )
                .unwrap()
            })
            .collect();
        let early_verification = ExecutionMutationSequenceRecoverySnapshot::try_new(
            prepared.provider_plan_artifact().unwrap(),
            plan.clone(),
            early_records,
            vec![observation.clone()],
        )
        .unwrap();
        let final_index = issues.len() - 1;
        let records = issues
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
        let snapshot = ExecutionMutationSequenceRecoverySnapshot::try_new(
            prepared.provider_plan_artifact().unwrap(),
            plan,
            records,
            vec![observation],
        )
        .unwrap();
        let detail_calls = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(AtomicRecoveryFixtureTransport {
            score: "100",
            calls: Mutex::new(Vec::new()),
        });
        let recovery = WellearnAtomicDurationCompletionRecovery::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[0].clone()),
                calls: Arc::clone(&detail_calls),
            }),
            transport.clone(),
        )
        .unwrap();

        assert!(
            recovery
                .verify_prepared_snapshot(&atomic_context(), &prepared, &early_verification,)
                .await
                .is_err()
        );
        assert!(
            recovery
                .verify_durable_snapshot(
                    &atomic_context(),
                    &authority.encode().unwrap(),
                    &prepared.batch_plan().encode_snapshot().unwrap(),
                    &early_verification,
                )
                .await
                .is_err()
        );
        assert!(
            recovery
                .verify_prepared_snapshot(&atomic_context(), &prepared, &snapshot)
                .await
                .is_err()
        );
        assert!(detail_calls.lock().unwrap().is_empty());
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn prepared_atomic_executor_requires_durable_final_verification() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            Some(1),
            None,
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let transport = Arc::new(AtomicFixtureTransport::default());
        let executor = WellearnAtomicDurationCompletion::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[0].clone()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }),
            transport.clone(),
        )
        .unwrap();

        let events = AtomicFixtureEvents {
            fail_verification: true,
            ..AtomicFixtureEvents::default()
        };
        let error = executor
            .execute_prepared(&atomic_context(), &prepared, &events)
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(
            error.human_required_reason,
            Some(asterism_domain::HumanRequiredReason::ManualIntervention)
        );
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prepared_atomic_executor_stops_on_fresh_child_drift_before_transport() {
        let fresh_tasks = tasks();
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            None,
            Some(auto_budget(1)),
        )
        .unwrap();
        let prepared =
            prepare_atomic_child_plan_from_fresh_inventory(&fresh_tasks, &units(), &authority)
                .unwrap();
        let transport = Arc::new(AtomicFixtureTransport::default());
        let detail_calls = Arc::new(Mutex::new(Vec::new()));
        let executor = WellearnAtomicDurationCompletion::try_new(
            Arc::new(AtomicFixtureDetail {
                metadata: development_metadata().unwrap(),
                detail: detail(fresh_tasks[1].clone()),
                calls: Arc::clone(&detail_calls),
            }),
            transport.clone(),
        )
        .unwrap();

        let events = AtomicFixtureEvents::default();
        assert!(
            executor
                .execute_prepared(&atomic_context(), &prepared, &events)
                .await
                .is_err()
        );
        assert!(transport.calls.lock().unwrap().is_empty());
        assert!(events.verifications.lock().unwrap().is_empty());
        assert_eq!(detail_calls.lock().unwrap().len(), 1);

        assert!(
            executor
                .execute_durable_artifacts(
                    &atomic_context(),
                    b"{}",
                    &prepared.batch_plan().encode_snapshot().unwrap(),
                    &prepared.provider_plan_artifact().unwrap(),
                    &events,
                )
                .await
                .is_err()
        );
        assert_eq!(detail_calls.lock().unwrap().len(), 1);
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn atomic_planning_authority_rejects_singleton_or_incomplete_inputs() {
        for flow in [
            WellearnBatchFlow::FanyuchangCompletion,
            WellearnBatchFlow::YzbrhCompletion,
            WellearnBatchFlow::YzbrhDuration,
            WellearnBatchFlow::AutoCompletion,
            WellearnBatchFlow::AutoLegacyDuration,
        ] {
            assert!(
                WellearnAtomicBatchPlanningAuthority::try_new(
                    "course:1001",
                    flow,
                    WellearnBatchUnitSelection::All,
                    "sco:1001:301",
                    Some(1),
                    None,
                )
                .is_err()
            );
        }
        assert!(
            WellearnAtomicBatchPlanningAuthority::try_new(
                "course:1001",
                WellearnBatchFlow::FanyuchangDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                None,
                None,
            )
            .is_err()
        );
        assert!(
            WellearnAtomicBatchPlanningAuthority::try_new(
                "course:1001",
                WellearnBatchFlow::AutoDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                Some(1),
                Some(auto_budget(1)),
            )
            .is_err()
        );
        assert!(
            WellearnAtomicBatchPlanningAuthority::try_new(
                "course:1001",
                WellearnBatchFlow::AutoDuration,
                WellearnBatchUnitSelection::Explicit(vec![0, 0]),
                "sco:1001:301",
                None,
                Some(auto_budget(1)),
            )
            .is_err()
        );
    }

    #[test]
    fn fresh_atomic_planning_never_infers_unselected_or_missing_child() {
        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![0]),
            "sco:1001:401",
            Some(10),
            None,
        )
        .unwrap();
        let error = prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &authority)
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);

        let authority = WellearnAtomicBatchPlanningAuthority::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:missing",
            Some(10),
            None,
        )
        .unwrap();
        let error = prepare_atomic_child_plan_from_fresh_inventory(&tasks(), &units(), &authority)
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    #[test]
    fn atomic_child_plan_preserves_auto_zero_floor_without_external_target() {
        let template = tasks().remove(0);
        let mut many = Vec::with_capacity(61);
        for index in 0..61_usize {
            let mut task = template.clone();
            task.remote_id = format!("sco:1001:atomic-{index}");
            task.normalized["sco_id"] = serde_json::json!(format!("atomic-{index}"));
            task.normalized["sco_index"] = serde_json::json!(index);
            many.push(task);
        }
        let batch = build_batch_plan(&many, WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        let plan = materialize_atomic_child_plan(&batch, 60, None).unwrap();

        assert_eq!(plan.entry_index(), 60);
        assert_eq!(plan.flow(), WellearnBatchFlow::AutoDuration);
        assert_eq!(
            plan.atomic_completion_profile(),
            WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0
        );
        assert_eq!(plan.target_seconds(), 0);
        assert!(materialize_atomic_child_plan(&batch, 0, Some(1)).is_err());
        assert_eq!(
            WellearnAtomicChildPlan::decode_bound(&plan.encode().unwrap(), &batch, 60, None)
                .unwrap(),
            plan
        );
        let artifact = plan.to_provider_execution_plan_artifact().unwrap();
        assert_eq!(artifact.payload_sanitized()["target_seconds"], 0);
        assert_eq!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &artifact, &batch, 60, None,
            )
            .unwrap(),
            plan
        );
    }

    #[test]
    fn atomic_child_plan_accepts_fanyuchang_zero_and_rejects_invalid_targets() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        assert!(materialize_atomic_child_plan(&batch, 0, None).is_err());
        let zero = materialize_atomic_child_plan(&batch, 0, Some(0)).unwrap();
        assert_eq!(zero.target_seconds(), 0);
        assert_eq!(
            WellearnAtomicChildPlan::decode_bound(&zero.encode().unwrap(), &batch, 0, Some(0))
                .unwrap(),
            zero
        );
        assert!(
            materialize_atomic_child_plan(
                &batch,
                0,
                Some(crate::runtime_settings::MAX_DURATION_REPORT_SECONDS + 1),
            )
            .is_err()
        );
        assert!(materialize_atomic_child_plan(&batch, batch.entries.len(), Some(1)).is_err());
    }

    #[test]
    fn singleton_batch_flows_never_materialize_atomic_children() {
        for flow in [
            WellearnBatchFlow::FanyuchangCompletion,
            WellearnBatchFlow::YzbrhCompletion,
            WellearnBatchFlow::YzbrhDuration,
            WellearnBatchFlow::AutoCompletion,
            WellearnBatchFlow::AutoLegacyDuration,
        ] {
            let source = if matches!(
                flow,
                WellearnBatchFlow::YzbrhCompletion | WellearnBatchFlow::AutoCompletion
            ) {
                pending_tasks()
            } else {
                tasks()
            };
            let batch = build_batch_plan(&source, flow, None).unwrap();
            assert!(materialize_atomic_child_plan(&batch, 0, Some(1)).is_err());
        }
    }

    #[test]
    fn restored_atomic_child_plan_rejects_schema_and_profile_drift() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let plan = materialize_atomic_child_plan(&batch, 0, Some(10)).unwrap();
        let mut value = serde_json::to_value(&plan).unwrap();
        assert_eq!(value["flow"], "fanyuchang_duration");
        assert_eq!(value["execution_shape"], "atomic_duration_completion");

        value["atomic_completion_profile"] = serde_json::json!("auto_zero_time_save_only0");
        assert!(serde_json::from_value::<WellearnAtomicChildPlan>(value).is_err());

        let mut value = serde_json::to_value(&plan).unwrap();
        value["version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<WellearnAtomicChildPlan>(value).is_err());

        let mut value = serde_json::to_value(&plan).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<WellearnAtomicChildPlan>(value).is_err());
        let other = materialize_atomic_child_plan(&batch, 1, Some(10)).unwrap();
        assert!(
            WellearnAtomicChildPlan::decode_bound(&other.encode().unwrap(), &batch, 0, Some(10))
                .is_err()
        );
        assert!(
            WellearnAtomicChildPlan::decode(&vec![b'x'; MAX_ATOMIC_CHILD_PLAN_BYTES + 1]).is_err()
        );
    }

    #[test]
    fn core_artifact_restore_rejects_namespace_and_full_rebind_drift() {
        let batch = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        let plan = materialize_atomic_child_plan(&batch, 1, None).unwrap();
        let artifact = plan.to_provider_execution_plan_artifact().unwrap();
        assert_eq!(plan.target_seconds(), 20);

        let foreign_provider = ProviderExecutionPlanArtifact::try_new(
            ProviderId::new("uai").unwrap(),
            "uai.atomic-child.v1",
            artifact.payload_sanitized().clone(),
        )
        .unwrap();
        assert!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &foreign_provider,
                &batch,
                1,
                None,
            )
            .is_err()
        );
        let foreign_type = ProviderExecutionPlanArtifact::try_new(
            ProviderId::new(PROVIDER_ID).unwrap(),
            "welearn.other-plan.v1",
            artifact.payload_sanitized().clone(),
        )
        .unwrap();
        assert!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &foreign_type,
                &batch,
                1,
                None,
            )
            .is_err()
        );
        assert!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &artifact, &batch, 0, None,
            )
            .is_err()
        );
        assert!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &artifact,
                &batch,
                1,
                Some(20),
            )
            .is_err()
        );
        let mut drifted = batch;
        drifted.entries[1].target_seconds = Some(19);
        assert!(
            WellearnAtomicChildPlan::from_provider_execution_plan_artifact_bound(
                &artifact, &drifted, 1, None,
            )
            .is_err()
        );
    }

    #[test]
    fn auto_duration_bounds_match_the_current_ui_sample_range() {
        let task = tasks().remove(0);
        let plan = build_batch_plan(
            std::slice::from_ref(&task),
            WellearnBatchFlow::AutoDuration,
            Some(330),
        )
        .unwrap();
        assert_eq!(plan.aggregate_duration_seconds, Some(19_800));
        assert_eq!(plan.entries[0].target_seconds, Some(19_800));
        assert!(
            build_batch_plan(
                std::slice::from_ref(&task),
                WellearnBatchFlow::AutoDuration,
                Some(331),
            )
            .is_err()
        );
    }

    #[test]
    fn explicit_selection_preserves_fanyuchang_unit_order() {
        let plan = build_selected_batch_plan(
            &tasks(),
            &units(),
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            WellearnBatchFlow::FanyuchangCompletion,
            None,
        )
        .unwrap();
        assert_eq!(
            plan.selected_units
                .iter()
                .map(|unit| unit.index)
                .collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| entry.remote_task_id.as_str())
                .collect::<Vec<_>>(),
            ["sco:1001:401", "sco:1001:301", "sco:1001:302"]
        );
        assert_eq!(plan.course_remote_id, "course:1001");
    }

    #[test]
    fn selected_empty_unit_remains_a_frozen_plan_fact() {
        let mut units = units();
        units.push(WellearnUnitObservation {
            index: 2,
            title: "Unit 3 Empty".to_owned(),
            code: Some("U3".to_owned()),
            visible: true,
        });
        let plan = build_selected_batch_plan(
            &tasks(),
            &units,
            WellearnBatchUnitSelection::Explicit(vec![2, 0]),
            WellearnBatchFlow::FanyuchangDuration,
            None,
        )
        .unwrap();
        assert_eq!(
            plan.selected_units
                .iter()
                .map(|unit| unit.index)
                .collect::<Vec<_>>(),
            [2, 0]
        );
        assert!(plan.entries.iter().all(|entry| entry.unit_index == 0));
    }

    #[test]
    fn selection_rejects_duplicates_missing_units_and_unit_drift() {
        let tasks = tasks();
        let units = units();
        assert!(
            build_selected_batch_plan(
                &tasks,
                &units,
                WellearnBatchUnitSelection::Explicit(vec![0, 0]),
                WellearnBatchFlow::FanyuchangCompletion,
                None,
            )
            .is_err()
        );
        assert!(
            build_selected_batch_plan(
                &tasks,
                &units,
                WellearnBatchUnitSelection::Explicit(vec![2]),
                WellearnBatchFlow::FanyuchangCompletion,
                None,
            )
            .is_err()
        );

        let mut drifted = units;
        drifted[0].title = "Changed title".to_owned();
        assert!(
            build_selected_batch_plan(
                &tasks,
                &drifted,
                WellearnBatchUnitSelection::All,
                WellearnBatchFlow::FanyuchangCompletion,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn fresh_batch_rebind_keeps_frozen_order_and_accepts_completed_preflight() {
        let tasks = pending_tasks();
        let plan = build_batch_plan(&tasks, WellearnBatchFlow::AutoCompletion, None).unwrap();
        let mut fresh = tasks[1].clone();
        fresh.normalized["sco_index"] = serde_json::json!(99);
        fresh.remote_state = RemoteState::Completed;
        fresh.normalized["completion_observation"] = serde_json::json!("completed");

        validate_fresh_batch_entry(&plan, 0, &detail(fresh)).unwrap();
        assert_eq!(plan.entries[0].sco_index, 1);
        assert_eq!(plan.entries[0].completion, RemoteState::Pending);
    }

    #[test]
    fn every_built_batch_plan_passes_frozen_integrity_validation() {
        for (flow, duration_minutes) in [
            (WellearnBatchFlow::FanyuchangCompletion, None),
            (WellearnBatchFlow::FanyuchangDuration, None),
            (WellearnBatchFlow::YzbrhDuration, None),
            (WellearnBatchFlow::AutoDuration, Some(1)),
            (WellearnBatchFlow::AutoLegacyDuration, None),
        ] {
            let plan = build_batch_plan(&tasks(), flow, duration_minutes).unwrap();
            validate_batch_plan_integrity(&plan).unwrap();
        }
        for flow in [
            WellearnBatchFlow::YzbrhCompletion,
            WellearnBatchFlow::AutoCompletion,
        ] {
            let plan = build_batch_plan(&pending_tasks(), flow, None).unwrap();
            validate_batch_plan_integrity(&plan).unwrap();
        }
    }

    #[test]
    fn batch_plan_snapshot_round_trips_every_donor_flow() {
        assert_eq!(WELLEARN_BATCH_PLAN_SNAPSHOT_TYPE, "welearn.batch-plan.v1");
        for (flow, duration_minutes, pending_only) in [
            (WellearnBatchFlow::FanyuchangCompletion, None, false),
            (WellearnBatchFlow::FanyuchangDuration, None, false),
            (WellearnBatchFlow::YzbrhCompletion, None, true),
            (WellearnBatchFlow::YzbrhDuration, None, false),
            (WellearnBatchFlow::AutoCompletion, None, true),
            (WellearnBatchFlow::AutoDuration, Some(1), false),
            (WellearnBatchFlow::AutoLegacyDuration, None, false),
        ] {
            let tasks = if pending_only {
                pending_tasks()
            } else {
                tasks()
            };
            let plan = build_batch_plan(&tasks, flow, duration_minutes).unwrap();
            let encoded = plan.encode_snapshot().unwrap();
            assert!(!encoded.is_empty());
            assert!(encoded.len() <= MAX_BATCH_PLAN_SNAPSHOT_BYTES);
            assert_eq!(WellearnBatchPlan::decode_snapshot(&encoded).unwrap(), plan);
        }
    }

    #[test]
    fn batch_plan_snapshot_rejects_schema_size_and_semantic_drift() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();
        let encoded = plan.encode_snapshot().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

        let mut drifted = value.clone();
        drifted["version"] = serde_json::json!(2);
        assert!(
            WellearnBatchPlan::decode_snapshot(&serde_json::to_vec(&drifted).unwrap()).is_err()
        );

        let mut drifted = value.clone();
        drifted["unexpected"] = serde_json::json!(true);
        assert!(
            WellearnBatchPlan::decode_snapshot(&serde_json::to_vec(&drifted).unwrap()).is_err()
        );

        let mut drifted = value.clone();
        drifted["dispatch"] = serde_json::json!("sequential");
        assert!(
            WellearnBatchPlan::decode_snapshot(&serde_json::to_vec(&drifted).unwrap()).is_err()
        );

        let mut drifted = value;
        drifted["entries"][0]["target_seconds"] = serde_json::json!(21);
        assert!(
            WellearnBatchPlan::decode_snapshot(&serde_json::to_vec(&drifted).unwrap()).is_err()
        );

        assert!(WellearnBatchPlan::decode_snapshot(&[]).is_err());
        assert!(
            WellearnBatchPlan::decode_snapshot(&vec![b'x'; MAX_BATCH_PLAN_SNAPSHOT_BYTES + 1])
                .is_err()
        );
    }

    #[test]
    fn maximum_batch_plan_stays_within_snapshot_bound() {
        let template = tasks().remove(0);
        let mut maximum = Vec::with_capacity(MAX_BATCH_TASKS);
        for index in 0..MAX_BATCH_TASKS {
            let mut task = template.clone();
            task.remote_id = format!("sco:1001:bulk-{index:04}");
            task.normalized["sco_id"] = serde_json::json!(format!("bulk-{index:04}"));
            task.normalized["sco_index"] = serde_json::json!(index);
            maximum.push(task);
        }
        let plan = build_batch_plan(&maximum, WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        let encoded = plan.encode_snapshot().unwrap();
        assert_eq!(plan.entries.len(), MAX_BATCH_TASKS);
        assert!(encoded.len() <= MAX_BATCH_PLAN_SNAPSHOT_BYTES);
        assert_eq!(
            WellearnBatchPlan::decode_snapshot(&encoded)
                .unwrap()
                .entries
                .len(),
            MAX_BATCH_TASKS
        );
    }

    #[test]
    fn frozen_integrity_rejects_flow_profile_drift() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();

        let mut drifted = plan.clone();
        drifted.dispatch = WellearnBatchDispatch::Sequential;
        assert_eq!(
            validate_batch_plan_integrity(&drifted).unwrap_err().kind,
            ProviderErrorKind::Internal
        );

        let mut drifted = plan.clone();
        drifted.target_strategy = WellearnBatchTargetStrategy::PerChild;
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan.clone();
        drifted.execution_shape = WellearnBatchExecutionShape::DurationReport;
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan;
        drifted.atomic_completion_profile =
            Some(WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100);
        assert!(validate_batch_plan_integrity(&drifted).is_err());
    }

    #[test]
    fn frozen_integrity_rejects_selection_unit_and_entry_drift() {
        let plan = build_selected_batch_plan(
            &tasks(),
            &units(),
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            WellearnBatchFlow::FanyuchangCompletion,
            None,
        )
        .unwrap();

        let mut drifted = plan.clone();
        drifted.selection = WellearnBatchUnitSelection::Explicit(vec![0, 1]);
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan.clone();
        drifted.selected_units[0].index = drifted.selected_units[1].index;
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan.clone();
        drifted.entries[0].unit_visible = !drifted.entries[0].unit_visible;
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan.clone();
        drifted.entries.swap(0, 1);
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan;
        drifted.entries[1].remote_task_id = drifted.entries[0].remote_task_id.clone();
        assert!(validate_batch_plan_integrity(&drifted).is_err());
    }

    #[test]
    fn frozen_integrity_rejects_auto_aggregate_drift() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, Some(1)).unwrap();

        let mut drifted = plan.clone();
        drifted.aggregate_duration_seconds = Some(61);
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan.clone();
        drifted.entries[0].target_seconds = Some(21);
        assert!(validate_batch_plan_integrity(&drifted).is_err());

        let mut drifted = plan;
        drifted.discarded_remainder_seconds = 1;
        assert!(validate_batch_plan_integrity(&drifted).is_err());
    }

    #[test]
    fn fresh_batch_rebind_rejects_a_drifted_persisted_plan_first() {
        let tasks = pending_tasks();
        let mut plan = build_batch_plan(&tasks, WellearnBatchFlow::AutoCompletion, None).unwrap();
        plan.dispatch = WellearnBatchDispatch::PerChildConcurrent;

        let error = validate_fresh_batch_entry(&plan, 0, &detail(tasks[1].clone())).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Internal);
    }

    #[test]
    fn fresh_batch_rebind_rejects_unit_identity_and_flow_eligibility_drift() {
        let tasks = pending_tasks();
        let plan = build_batch_plan(&tasks, WellearnBatchFlow::AutoCompletion, None).unwrap();

        let mut changed_unit = tasks[1].clone();
        changed_unit.normalized["unit_title"] = serde_json::json!("Replacement Unit");
        let error = validate_fresh_batch_entry(&plan, 0, &detail(changed_unit)).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);

        let mut hidden_leaf = tasks[1].clone();
        hidden_leaf.normalized["sco_visible"] = serde_json::json!(false);
        hidden_leaf.normalized["visible"] = serde_json::json!(false);
        hidden_leaf.remote_state = RemoteState::NotOpen;
        let error = validate_fresh_batch_entry(&plan, 0, &detail(hidden_leaf)).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);

        let mut unknown_completion = tasks[1].clone();
        unknown_completion.remote_state = RemoteState::Unknown;
        unknown_completion.normalized["completion_observation"] = serde_json::json!("unknown");
        let error = validate_fresh_batch_entry(&plan, 0, &detail(unknown_completion)).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    #[test]
    fn fresh_batch_rebind_preserves_donor_specific_hidden_sco_behavior() {
        let tasks = tasks();
        let plan = build_batch_plan(&tasks, WellearnBatchFlow::FanyuchangCompletion, None).unwrap();
        let mut fresh = tasks[0].clone();
        fresh.remote_state = RemoteState::NotOpen;
        fresh.normalized["sco_visible"] = serde_json::json!(false);
        fresh.normalized["visible"] = serde_json::json!(false);

        validate_fresh_batch_entry(&plan, 0, &detail(fresh)).unwrap();
    }

    #[test]
    fn donor_flows_enforce_their_exact_unit_selection_shapes() {
        let tasks = pending_tasks();
        let units = units();
        for flow in [
            WellearnBatchFlow::YzbrhCompletion,
            WellearnBatchFlow::YzbrhDuration,
            WellearnBatchFlow::AutoLegacyDuration,
        ] {
            assert!(
                build_selected_batch_plan(
                    &tasks,
                    &units,
                    WellearnBatchUnitSelection::Explicit(vec![0, 1]),
                    flow,
                    None,
                )
                .is_err()
            );
            assert!(
                build_selected_batch_plan(
                    &tasks,
                    &units,
                    WellearnBatchUnitSelection::Explicit(vec![0]),
                    flow,
                    None,
                )
                .is_ok()
            );
        }

        assert!(
            build_selected_batch_plan(
                &tasks,
                &units,
                WellearnBatchUnitSelection::Explicit(vec![1, 0]),
                WellearnBatchFlow::AutoCompletion,
                None,
            )
            .is_err()
        );
        assert!(
            build_selected_batch_plan(
                &tasks,
                &units,
                WellearnBatchUnitSelection::Explicit(vec![0, 1]),
                WellearnBatchFlow::AutoCompletion,
                None,
            )
            .is_ok()
        );
        assert!(
            build_selected_batch_plan(
                &tasks,
                &units,
                WellearnBatchUnitSelection::Explicit(vec![1, 0]),
                WellearnBatchFlow::AutoDuration,
                Some(1),
            )
            .is_err()
        );
    }

    #[test]
    fn auto_duration_requires_a_frozen_sample() {
        assert!(build_batch_plan(&tasks(), WellearnBatchFlow::AutoDuration, None).is_err());
    }

    #[test]
    fn completion_flows_reject_unknown_or_in_progress_observations() {
        let plan = build_batch_plan(&tasks(), WellearnBatchFlow::AutoCompletion, None);
        assert!(plan.is_err());
    }

    #[test]
    fn non_auto_flow_rejects_aggregate_duration_input() {
        assert!(build_batch_plan(&tasks(), WellearnBatchFlow::AutoCompletion, Some(1)).is_err());
    }

    #[test]
    fn every_donor_flow_exposes_a_stable_dispatch_contract() {
        assert_eq!(
            WellearnBatchFlow::FanyuchangCompletion.dispatch(),
            WellearnBatchDispatch::PerChildConcurrent
        );
        assert_eq!(
            WellearnBatchFlow::FanyuchangDuration.dispatch(),
            WellearnBatchDispatch::PerChildConcurrent
        );
        assert_eq!(
            WellearnBatchFlow::YzbrhCompletion.dispatch(),
            WellearnBatchDispatch::Sequential
        );
        assert_eq!(
            WellearnBatchFlow::YzbrhDuration.dispatch(),
            WellearnBatchDispatch::PerChildConcurrent
        );
        assert_eq!(
            WellearnBatchFlow::AutoCompletion.dispatch(),
            WellearnBatchDispatch::Sequential
        );
        assert_eq!(
            WellearnBatchFlow::AutoDuration.dispatch(),
            WellearnBatchDispatch::BoundedThreadPool
        );
        assert_eq!(
            WellearnBatchFlow::AutoLegacyDuration.dispatch(),
            WellearnBatchDispatch::Sequential
        );
        assert_eq!(
            WellearnBatchFlow::AutoCompletion.target_strategy(),
            WellearnBatchTargetStrategy::PerChild
        );
        assert_eq!(
            WellearnBatchFlow::AutoDuration.target_strategy(),
            WellearnBatchTargetStrategy::AggregateEqualFloor
        );
        assert_eq!(
            WellearnBatchFlow::YzbrhDuration.target_strategy(),
            WellearnBatchTargetStrategy::PerChild
        );
        assert_eq!(
            WellearnBatchFlow::AutoLegacyDuration.target_strategy(),
            WellearnBatchTargetStrategy::PerChild
        );
        assert_eq!(
            WellearnBatchFlow::FanyuchangCompletion.execution_shape(),
            WellearnBatchExecutionShape::ResourceExecution
        );
        assert_eq!(
            WellearnBatchFlow::YzbrhCompletion.execution_shape(),
            WellearnBatchExecutionShape::ResourceExecution
        );
        assert_eq!(
            WellearnBatchFlow::AutoCompletion.execution_shape(),
            WellearnBatchExecutionShape::ResourceExecution
        );
        assert_eq!(
            WellearnBatchFlow::YzbrhDuration.execution_shape(),
            WellearnBatchExecutionShape::DurationReport
        );
        assert_eq!(
            WellearnBatchFlow::AutoLegacyDuration.execution_shape(),
            WellearnBatchExecutionShape::DurationReport
        );
        assert_eq!(
            WellearnBatchFlow::FanyuchangDuration.execution_shape(),
            WellearnBatchExecutionShape::AtomicDurationCompletion
        );
        assert_eq!(
            WellearnBatchFlow::AutoDuration.execution_shape(),
            WellearnBatchExecutionShape::AtomicDurationCompletion
        );
        assert_eq!(
            WellearnBatchFlow::FanyuchangDuration.atomic_completion_profile(),
            Some(WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100)
        );
        assert_eq!(
            WellearnBatchFlow::AutoDuration.atomic_completion_profile(),
            Some(WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0)
        );
        assert_eq!(
            WellearnBatchFlow::YzbrhDuration.atomic_completion_profile(),
            None
        );
    }

    #[test]
    fn atomic_duration_flows_require_both_child_capabilities() {
        let mut task = tasks().remove(0);
        task.capabilities
            .retain(|capability| *capability != TaskCapability::ResourceExecution);
        for flow in [
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchFlow::AutoDuration,
        ] {
            assert!(
                build_batch_plan(
                    std::slice::from_ref(&task),
                    flow,
                    (flow == WellearnBatchFlow::AutoDuration).then_some(1),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn incomplete_normalized_task_fails_closed() {
        let mut task = tasks().remove(0);
        task.normalized["sco_index"] = serde_json::Value::Null;
        assert!(build_batch_plan(&[task], WellearnBatchFlow::AutoCompletion, None).is_err());
    }

    #[test]
    fn malformed_optional_sco_visibility_fails_closed() {
        let mut task = tasks().remove(0);
        task.normalized["sco_visible"] = serde_json::json!("false");
        assert!(build_batch_plan(&[task], WellearnBatchFlow::AutoCompletion, None).is_err());
    }

    #[test]
    fn inconsistent_visibility_observations_fail_closed() {
        let mut task = tasks().remove(0);
        task.normalized["visible"] = serde_json::json!(false);
        assert!(build_batch_plan(&[task], WellearnBatchFlow::FanyuchangCompletion, None).is_err());
    }

    #[test]
    fn duplicate_remote_tasks_fail_closed() {
        let tasks = tasks();
        assert!(
            build_batch_plan(
                &[tasks[0].clone(), tasks[0].clone()],
                WellearnBatchFlow::FanyuchangCompletion,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_batch_fails_before_membership_planning() {
        let task = tasks().remove(0);
        let oversized = vec![task; MAX_BATCH_TASKS + 1];
        assert!(
            build_batch_plan(&oversized, WellearnBatchFlow::FanyuchangCompletion, None).is_err()
        );
    }
}
