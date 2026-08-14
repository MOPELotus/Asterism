use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTask};

use crate::WellearnUnitObservation;

/// Audited donor batch flow. This is a pure membership/target boundary; it
/// does not create or schedule Core executions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnBatchFlow {
    FanyuchangCompletion,
    FanyuchangDuration,
    YzbrhCompletion,
    YzbrhDuration,
    AutoCompletion,
    AutoDuration,
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

impl WellearnBatchFlow {
    pub const fn dispatch(self) -> WellearnBatchDispatch {
        match self {
            Self::FanyuchangCompletion | Self::FanyuchangDuration | Self::YzbrhDuration => {
                WellearnBatchDispatch::PerChildConcurrent
            }
            Self::YzbrhCompletion | Self::AutoCompletion => WellearnBatchDispatch::Sequential,
            Self::AutoDuration => WellearnBatchDispatch::BoundedThreadPool,
        }
    }

    pub const fn target_strategy(self) -> WellearnBatchTargetStrategy {
        match self {
            Self::AutoCompletion
            | Self::FanyuchangCompletion
            | Self::FanyuchangDuration
            | Self::YzbrhCompletion
            | Self::YzbrhDuration => WellearnBatchTargetStrategy::PerChild,
            Self::AutoDuration => WellearnBatchTargetStrategy::AggregateEqualFloor,
        }
    }

    const fn keeps_hidden(self) -> bool {
        matches!(
            self,
            Self::FanyuchangCompletion | Self::FanyuchangDuration | Self::YzbrhDuration
        )
    }

    const fn skips_completed(self) -> bool {
        matches!(self, Self::YzbrhCompletion | Self::AutoCompletion)
    }

    const fn requires_pending_completion(self) -> bool {
        matches!(self, Self::YzbrhCompletion | Self::AutoCompletion)
    }

    const fn is_duration(self) -> bool {
        matches!(
            self,
            Self::FanyuchangDuration | Self::YzbrhDuration | Self::AutoDuration
        )
    }

    const fn required_capability(self) -> TaskCapability {
        if self.is_duration() {
            TaskCapability::DurationReport
        } else {
            TaskCapability::ResourceExecution
        }
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
    pub selection: WellearnBatchUnitSelection,
    pub selected_units: Vec<WellearnUnitObservation>,
    pub entries: Vec<WellearnBatchEntry>,
    pub aggregate_duration_seconds: Option<u64>,
    pub discarded_remainder_seconds: u64,
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
        if !task.capabilities.contains(&flow.required_capability()) {
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
        let keep = (flow.keeps_hidden() || visible)
            && (!flow.skips_completed() || completion != RemoteState::Completed)
            && (!flow.requires_pending_completion() || completion == RemoteState::Pending);
        if !keep || (!unit_visible && !flow.keeps_hidden()) {
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
    Ok(WellearnBatchPlan {
        course_remote_id,
        flow,
        dispatch: flow.dispatch(),
        target_strategy: flow.target_strategy(),
        selection,
        selected_units,
        entries,
        aggregate_duration_seconds,
        discarded_remainder_seconds,
    })
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

    fn units() -> Vec<WellearnUnitObservation> {
        parse_unit_inventory(UNITS).unwrap()
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
        assert_eq!(plan.entries.len(), 2);
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
                .all(|entry| entry.target_seconds == Some(30))
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
