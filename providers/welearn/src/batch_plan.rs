use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, RemoteTask, RemoteTaskDetail,
};
use serde::{Deserialize, Serialize};

use crate::{WellearnUnitObservation, task_detail::validate_fresh_execution_detail};

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnBatchDispatch {
    PerChildConcurrent,
    Sequential,
    BoundedThreadPool,
}

/// Target allocation contract for a frozen donor batch. The shared durable
/// layer uses this fact to persist either each child target or the aggregate
/// derivation without asking the Provider to resample after recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
const MAX_AUTO_DURATION_MINUTES: u64 = 330;
const MAX_BATCH_ID_COMPONENT_BYTES: usize = 128;
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
}

/// Materializes one exact atomic child from a fully validated batch plan.
///
/// Current Fanyuchang has no aggregate entry target, so its deterministic
/// per-Execution target must already be frozen and supplied. Modular Auto must
/// use the equal-floor target stored on the exact entry, including zero.
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
    use super::*;
    use crate::{
        WellearnScoLeavesDocument, parse_course_inventory, parse_task_inventory,
        parse_unit_inventory,
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
    }

    #[test]
    fn atomic_child_plan_rejects_missing_or_invalid_fanyuchang_target() {
        let batch =
            build_batch_plan(&tasks(), WellearnBatchFlow::FanyuchangDuration, None).unwrap();
        assert!(materialize_atomic_child_plan(&batch, 0, None).is_err());
        assert!(materialize_atomic_child_plan(&batch, 0, Some(0)).is_err());
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
