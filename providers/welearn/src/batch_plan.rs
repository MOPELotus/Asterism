use std::cmp::Ordering;
use std::collections::BTreeSet;

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTask};

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
    SharedHeartbeat,
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

const MAX_BATCH_TASKS: usize = 8_192;

impl WellearnBatchFlow {
    pub const fn dispatch(self) -> WellearnBatchDispatch {
        match self {
            Self::FanyuchangCompletion | Self::FanyuchangDuration => {
                WellearnBatchDispatch::PerChildConcurrent
            }
            Self::YzbrhCompletion | Self::AutoCompletion => WellearnBatchDispatch::Sequential,
            Self::YzbrhDuration => WellearnBatchDispatch::SharedHeartbeat,
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
    pub visible: bool,
    pub completion: RemoteState,
    pub target_seconds: Option<u64>,
}

/// A bounded, deterministic donor batch plan. `discarded_remainder_seconds`
/// is intentionally retained for Auto's integer division semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WellearnBatchPlan {
    pub flow: WellearnBatchFlow,
    pub dispatch: WellearnBatchDispatch,
    pub target_strategy: WellearnBatchTargetStrategy,
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
    let mut seen_remote_ids = BTreeSet::new();
    for task in tasks {
        if !seen_remote_ids.insert(task.remote_id.as_str()) {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch selection contains a duplicate SCO identity",
            ));
        }
        if task.course_remote_id.as_deref() != Some(course_remote_id) {
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
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn batch selection requires a complete normalized SCO v2 observation",
            ));
        }
    }

    let mut ordered = tasks.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| compare_task_order(left, right));
    let mut entries = Vec::with_capacity(ordered.len());
    for task in ordered {
        let visible = normalized_bool(task, "visible")
            .expect("validated complete normalized observation above");
        let unit_visible = normalized_bool(task, "unit_visible")
            .expect("validated complete normalized observation above");
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
            visible,
            completion,
            target_seconds: None,
        });
    }

    let (aggregate_duration_seconds, discarded_remainder_seconds) =
        if flow == WellearnBatchFlow::AutoDuration {
            let minutes = auto_duration_minutes.expect("validated above");
            if minutes == 0 || minutes > 7_200 {
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
        flow,
        dispatch: flow.dispatch(),
        target_strategy: flow.target_strategy(),
        entries,
        aggregate_duration_seconds,
        discarded_remainder_seconds,
    })
}

fn compare_task_order(left: &RemoteTask, right: &RemoteTask) -> Ordering {
    normalized_u32(left, "unit_index")
        .unwrap_or(u32::MAX)
        .cmp(&normalized_u32(right, "unit_index").unwrap_or(u32::MAX))
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
    use crate::{parse_course_inventory, parse_task_inventory, WellearnScoLeavesDocument};

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
        assert_eq!(plan.dispatch, WellearnBatchDispatch::SharedHeartbeat);
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
        assert!(plan
            .entries
            .iter()
            .all(|entry| entry.target_seconds == Some(30)));
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
        assert!(plan
            .entries
            .iter()
            .all(|entry| entry.target_seconds == Some(0)));
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
            WellearnBatchDispatch::SharedHeartbeat
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
    fn duplicate_remote_tasks_fail_closed() {
        let tasks = tasks();
        assert!(build_batch_plan(
            &[tasks[0].clone(), tasks[0].clone()],
            WellearnBatchFlow::FanyuchangCompletion,
            None
        )
        .is_err());
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
