use std::{collections::BTreeSet, fmt};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, RemoteCourse, RemoteTask,
    ResolvedProviderRuntimeSettings,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    course_inventory::{course_resource_id_from_remote, required_remote_component, required_text},
    runtime_settings::{BROWSER_PLAY_VIDEO_KEY, BROWSER_RESIDENCE_SECONDS_KEY},
};

const UAI_COURSE_RESIDENCE_BATCH_VERSION: u32 = 1;
const UAI_COURSE_RESIDENCE_CHILD_PLAN_VERSION: u16 = 1;
const MAX_CHILD_PLAN_BYTES: usize = 1_048_576;
const MIN_RESIDENCE_SECONDS: u64 = 60;
const MAX_RESIDENCE_SECONDS: u64 = 28_800;
const MAX_BATCH_MICROS: usize = 2_048;
const MAX_BATCH_TASKS: usize = 8_192;
const MAX_TABS_PER_MICRO: u32 = 64;
const MAX_TASKS_PER_TAB: u32 = 128;
const MAX_TASK_TYPES: usize = 32;
const MAX_TASK_TYPE_BYTES: usize = 64;
const MAX_TITLE_BYTES: usize = 512;

/// Immutable Provider-local plan for the donor's Course-level rendered run.
///
/// The plan freezes the fresh source-tree Micro order before applying the
/// donor's `menuList.slice(startIdx)` selection. It does not start a browser,
/// create Core executions or claim remote duration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiCourseResidenceBatchPlan {
    version: u32,
    course_remote_id: String,
    course_publish_version: Option<u64>,
    total_residence_seconds: u64,
    play_video: bool,
    micros: Vec<UaiCourseResidenceMicro>,
    start: UaiCourseResidenceRestartTarget,
    membership_digest: [u8; 32],
    plan_digest: [u8; 32],
}

/// Credential-free Provider-private value that freezes one recoverable Course
/// batch start.
///
/// Core may persist these bounded JSON bytes without interpreting them. The
/// caller supplies the opaque digest of the resolved settings/profile owner;
/// UAI does not guess Core's profile naming or revision scheme.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiCourseResidenceChildPlan {
    version: u16,
    provider_plan_version: u32,
    course_remote_id: String,
    course_publish_version: Option<u64>,
    runtime_profile_digest: [u8; 32],
    owner_remote_task_id: String,
    ordered_tasks: Vec<UaiCourseResidenceChildTask>,
    start_ordinal: u32,
    start_micro_identity_digest: [u8; 32],
    batch_membership_digest: [u8; 32],
    batch_plan_digest: [u8; 32],
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct UaiCourseResidenceChildTask {
    remote_task_id: String,
    fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UaiCourseResidenceChildPlanWire {
    version: u16,
    provider_plan_version: u32,
    course_remote_id: String,
    course_publish_version: Option<u64>,
    runtime_profile_digest: [u8; 32],
    owner_remote_task_id: String,
    ordered_tasks: Vec<UaiCourseResidenceChildTask>,
    start_ordinal: u32,
    start_micro_identity_digest: [u8; 32],
    batch_membership_digest: [u8; 32],
    batch_plan_digest: [u8; 32],
}

impl<'de> Deserialize<'de> for UaiCourseResidenceChildPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = UaiCourseResidenceChildPlanWire::deserialize(deserializer)?;
        let plan = Self {
            version: wire.version,
            provider_plan_version: wire.provider_plan_version,
            course_remote_id: wire.course_remote_id,
            course_publish_version: wire.course_publish_version,
            runtime_profile_digest: wire.runtime_profile_digest,
            owner_remote_task_id: wire.owner_remote_task_id,
            ordered_tasks: wire.ordered_tasks,
            start_ordinal: wire.start_ordinal,
            start_micro_identity_digest: wire.start_micro_identity_digest,
            batch_membership_digest: wire.batch_membership_digest,
            batch_plan_digest: wire.batch_plan_digest,
        };
        plan.validate().map_err(serde::de::Error::custom)?;
        Ok(plan)
    }
}

impl fmt::Debug for UaiCourseResidenceChildPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCourseResidenceChildPlan")
            .field("version", &self.version)
            .field("provider_plan_version", &self.provider_plan_version)
            .field("course_remote_id", &"[REDACTED]")
            .field("runtime_profile_digest", &"[REDACTED]")
            .field("owner_remote_task_id", &"[REDACTED]")
            .field("ordered_task_count", &self.ordered_tasks.len())
            .field("ordered_tasks", &"[REDACTED]")
            .field("start_ordinal", &self.start_ordinal)
            .field("batch_digests", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl UaiCourseResidenceChildPlan {
    pub const fn version(&self) -> u16 {
        self.version
    }

    pub const fn provider_plan_version(&self) -> u32 {
        self.provider_plan_version
    }

    pub fn course_remote_id(&self) -> &str {
        &self.course_remote_id
    }

    pub fn owner_remote_task_id(&self) -> &str {
        &self.owner_remote_task_id
    }

    pub const fn start_ordinal(&self) -> u32 {
        self.start_ordinal
    }

    pub const fn runtime_profile_digest(&self) -> [u8; 32] {
        self.runtime_profile_digest
    }

    /// Encodes the fully revalidated value under a fixed Provider bound.
    ///
    /// # Errors
    ///
    /// Returns an internal error for invariant, encoding or size drift.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| invalid_child_plan())?;
        if encoded.is_empty() || encoded.len() > MAX_CHILD_PLAN_BYTES {
            return Err(invalid_child_plan());
        }
        Ok(encoded)
    }

    /// Decodes strict JSON and revalidates every self-contained field.
    ///
    /// # Errors
    ///
    /// Returns an internal error for an oversized, unknown or invalid value.
    pub fn decode(encoded: &[u8]) -> ProviderResult<Self> {
        if encoded.is_empty() || encoded.len() > MAX_CHILD_PLAN_BYTES {
            return Err(invalid_child_plan());
        }
        serde_json::from_slice(encoded).map_err(|_| invalid_child_plan())
    }

    /// Rebuilds the exact batch/cursor start from fresh inventory and settings.
    ///
    /// The returned batch is suitable for the subsequent fresh Browser plan,
    /// sequence-one `ScanMenu` command and `UaiBrowserResidenceCursor::begin`.
    ///
    /// # Errors
    ///
    /// Returns `RemoteChanged` when Course identity, ordered Task identities or
    /// fingerprints, settings/profile identity, start authority or batch
    /// digests no longer reproduce this frozen value.
    pub fn rebuild_batch_for_cursor(
        &self,
        course: &RemoteCourse,
        tasks: &[RemoteTask],
        settings: &ResolvedProviderRuntimeSettings,
        runtime_profile_digest: [u8; 32],
    ) -> ProviderResult<UaiCourseResidenceBatchPlan> {
        self.validate()?;
        let rebuilt = build_course_residence_child_plan(
            course,
            tasks,
            settings,
            runtime_profile_digest,
            &self.owner_remote_task_id,
            self.start_ordinal,
        )?;
        if &rebuilt != self {
            return Err(stale_child_plan());
        }
        build_course_residence_batch_plan(course, tasks, settings, self.start_ordinal)
    }

    /// Decodes and freshly rebinds one durable child value in one operation.
    ///
    /// # Errors
    ///
    /// Returns an internal error for invalid bytes or `RemoteChanged` when the
    /// fresh inventory/settings no longer reproduce the value.
    pub fn decode_bound(
        encoded: &[u8],
        course: &RemoteCourse,
        tasks: &[RemoteTask],
        settings: &ResolvedProviderRuntimeSettings,
        runtime_profile_digest: [u8; 32],
    ) -> ProviderResult<(Self, UaiCourseResidenceBatchPlan)> {
        let plan = Self::decode(encoded)?;
        let batch =
            plan.rebuild_batch_for_cursor(course, tasks, settings, runtime_profile_digest)?;
        Ok((plan, batch))
    }

    fn validate(&self) -> ProviderResult<()> {
        if self.version != UAI_COURSE_RESIDENCE_CHILD_PLAN_VERSION
            || self.provider_plan_version != UAI_COURSE_RESIDENCE_BATCH_VERSION
            || !valid_text(&self.course_remote_id, MAX_TITLE_BYTES)
            || !valid_text(&self.owner_remote_task_id, MAX_TITLE_BYTES)
            || self.runtime_profile_digest == [0; 32]
            || self.start_micro_identity_digest == [0; 32]
            || self.batch_membership_digest == [0; 32]
            || self.batch_plan_digest == [0; 32]
            || self.ordered_tasks.is_empty()
            || self.ordered_tasks.len() > MAX_BATCH_TASKS
            || self.start_ordinal as usize >= MAX_BATCH_MICROS
        {
            return Err(invalid_child_plan());
        }
        let mut task_ids = BTreeSet::new();
        for task in &self.ordered_tasks {
            if !valid_text(&task.remote_task_id, MAX_TITLE_BYTES)
                || !valid_task_fingerprint(&task.fingerprint)
                || !task_ids.insert(task.remote_task_id.as_str())
            {
                return Err(invalid_child_plan());
            }
        }
        if !task_ids.contains(self.owner_remote_task_id.as_str()) {
            return Err(invalid_child_plan());
        }
        Ok(())
    }
}

impl UaiCourseResidenceBatchPlan {
    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn course_remote_id(&self) -> &str {
        &self.course_remote_id
    }

    pub const fn course_publish_version(&self) -> Option<u64> {
        self.course_publish_version
    }

    pub const fn total_residence_seconds(&self) -> u64 {
        self.total_residence_seconds
    }

    pub const fn play_video(&self) -> bool {
        self.play_video
    }

    pub fn micros(&self) -> &[UaiCourseResidenceMicro] {
        &self.micros
    }

    pub fn selected_micros(&self) -> &[UaiCourseResidenceMicro] {
        &self.micros[self.start.ordinal as usize..]
    }

    pub const fn start(&self) -> &UaiCourseResidenceRestartTarget {
        &self.start
    }

    pub const fn membership_digest(&self) -> [u8; 32] {
        self.membership_digest
    }

    pub const fn plan_digest(&self) -> [u8; 32] {
        self.plan_digest
    }

    /// Returns the exact donor budget share before runtime Tab/Task division.
    ///
    /// The donor retains a fractional `total / remaining Micros` value and
    /// rounds only at the final residence leaf, so no floor remainder is
    /// discarded at plan time.
    ///
    /// # Errors
    ///
    /// Returns an internal error if immutable plan cardinality drifted.
    pub fn budget_share(&self) -> ProviderResult<UaiCourseResidenceBudgetShare> {
        self.validate()?;
        Ok(UaiCourseResidenceBudgetShare {
            numerator_seconds: self.total_residence_seconds,
            denominator_micros: u64::try_from(self.selected_micros().len())
                .map_err(|_| invalid_plan())?,
        })
    }

    /// Resolves an identity-bound restart target from the immutable full
    /// Course membership. A bare caller-supplied ordinal is never sufficient.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the ordinal is outside the fresh plan.
    pub fn restart_target(&self, ordinal: u32) -> ProviderResult<UaiCourseResidenceRestartTarget> {
        self.validate()?;
        self.micros
            .get(ordinal as usize)
            .map(|micro| UaiCourseResidenceRestartTarget {
                ordinal,
                micro_identity_digest: micro.identity_digest,
            })
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "UAI Course residence restart ordinal is outside fresh membership",
                )
            })
    }

    /// Recreates the donor's restart behavior while retaining the same full
    /// Course membership and total budget.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a target from another Course or stale
    /// membership snapshot.
    pub fn restart_at(&self, target: &UaiCourseResidenceRestartTarget) -> ProviderResult<Self> {
        self.validate()?;
        let micro = self.micros.get(target.ordinal as usize).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course residence restart ordinal is outside fresh membership",
            )
        })?;
        if micro.identity_digest != target.micro_identity_digest {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI Course residence restart target is stale or foreign",
            ));
        }
        let mut restarted = self.clone();
        restarted.start = target.clone();
        restarted.plan_digest = plan_digest(&restarted);
        restarted.validate()?;
        Ok(restarted)
    }

    /// Rebuilds only fresh Course membership and requires it to equal this
    /// immutable plan. Progress/completion changes do not alter membership;
    /// hierarchy, labels, ordered Groups and Course publish version do.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the fresh Course tree no longer represents
    /// the same ordered browser workload.
    pub fn validate_fresh_membership(
        &self,
        course: &RemoteCourse,
        tasks: &[RemoteTask],
    ) -> ProviderResult<()> {
        self.validate()?;
        let membership = build_membership(course, tasks)?;
        if membership.course_remote_id != self.course_remote_id
            || membership.course_publish_version != self.course_publish_version
            || membership.digest != self.membership_digest
            || membership.micros != self.micros
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI Course residence membership changed before execution",
            ));
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an internal error if any immutable membership, restart or
    /// budget invariant no longer matches the plan digests.
    pub fn validate(&self) -> ProviderResult<()> {
        if self.version != UAI_COURSE_RESIDENCE_BATCH_VERSION
            || !(MIN_RESIDENCE_SECONDS..=MAX_RESIDENCE_SECONDS)
                .contains(&self.total_residence_seconds)
            || self.micros.is_empty()
            || self.micros.len() > MAX_BATCH_MICROS
            || self.start.ordinal as usize >= self.micros.len()
        {
            return Err(invalid_plan());
        }
        let mut task_ids = BTreeSet::new();
        for (ordinal, micro) in self.micros.iter().enumerate() {
            if micro.ordinal as usize != ordinal
                || micro.tasks.is_empty()
                || micro.identity_digest != micro_identity_digest(&self.course_remote_id, micro)
                || micro
                    .tasks
                    .iter()
                    .any(|task| !task_ids.insert(task.remote_task_id.as_str()))
            {
                return Err(invalid_plan());
            }
        }
        if task_ids.len() > MAX_BATCH_TASKS
            || self.micros[self.start.ordinal as usize].identity_digest
                != self.start.micro_identity_digest
            || membership_digest(
                &self.course_remote_id,
                self.course_publish_version,
                &self.micros,
            ) != self.membership_digest
            || plan_digest(self) != self.plan_digest
        {
            return Err(invalid_plan());
        }
        Ok(())
    }
}

/// One fresh source-tree Micro and its ordered normalized Group membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiCourseResidenceMicro {
    ordinal: u32,
    unit_id: String,
    unit_title: String,
    section_id: Option<String>,
    section_title: Option<String>,
    micro_id: String,
    micro_title: String,
    tasks: Vec<UaiCourseResidenceTask>,
    identity_digest: [u8; 32],
}

impl UaiCourseResidenceMicro {
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn unit_title(&self) -> &str {
        &self.unit_title
    }

    pub fn section_id(&self) -> Option<&str> {
        self.section_id.as_deref()
    }

    pub fn section_title(&self) -> Option<&str> {
        self.section_title.as_deref()
    }

    pub fn micro_id(&self) -> &str {
        &self.micro_id
    }

    pub fn micro_title(&self) -> &str {
        &self.micro_title
    }

    pub fn tasks(&self) -> &[UaiCourseResidenceTask] {
        &self.tasks
    }

    pub const fn identity_digest(&self) -> [u8; 32] {
        self.identity_digest
    }
}

/// Stable browser-relevant Task shape nested under one Course Micro.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiCourseResidenceTask {
    remote_task_id: String,
    title: String,
    task_types: Vec<String>,
    question_count: u32,
}

impl UaiCourseResidenceTask {
    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn task_types(&self) -> &[String] {
        &self.task_types
    }

    pub const fn question_count(&self) -> u32 {
        self.question_count
    }
}

/// One restart choice that binds both source ordinal and Micro identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiCourseResidenceRestartTarget {
    ordinal: u32,
    micro_identity_digest: [u8; 32],
}

impl UaiCourseResidenceRestartTarget {
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn micro_identity_digest(&self) -> [u8; 32] {
        self.micro_identity_digest
    }
}

/// Exact positive rational Micro share used before runtime DOM cardinalities
/// become available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UaiCourseResidenceBudgetShare {
    numerator_seconds: u64,
    denominator_micros: u64,
}

impl UaiCourseResidenceBudgetShare {
    pub const fn numerator_seconds(self) -> u64 {
        self.numerator_seconds
    }

    pub const fn denominator_micros(self) -> u64 {
        self.denominator_micros
    }

    /// Reproduces positive JavaScript `Math.round` at the final donor leaf.
    /// A zero Tab count uses direct Tasks; a zero Task count represents one
    /// residence leaf for the current Micro or Tab.
    ///
    /// # Errors
    ///
    /// Returns a typed error when runtime DOM cardinality exceeds the frozen
    /// Provider bounds.
    pub fn rounded_leaf_seconds(self, tab_count: u32, task_count: u32) -> ProviderResult<u64> {
        if tab_count > MAX_TABS_PER_MICRO || task_count > MAX_TASKS_PER_TAB {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course residence runtime cardinality exceeds the frozen bounds",
            ));
        }
        let leaves = if tab_count == 0 {
            u64::from(task_count.max(1))
        } else {
            u64::from(tab_count)
                .checked_mul(u64::from(task_count.max(1)))
                .ok_or_else(invalid_plan)?
        };
        let denominator = self
            .denominator_micros
            .checked_mul(leaves)
            .filter(|value| *value > 0)
            .ok_or_else(invalid_plan)?;
        self.numerator_seconds
            .checked_add(denominator / 2)
            .map(|value| value / denominator)
            .ok_or_else(invalid_plan)
    }
}

/// Builds one immutable Course-level donor plan from a fresh ordered Task
/// inventory and Core's already-resolved runtime settings snapshot.
///
/// # Errors
///
/// Returns a typed error for empty/foreign/reordered membership, malformed
/// hierarchy, an out-of-range start ordinal or invalid frozen settings.
pub fn build_course_residence_batch_plan(
    course: &RemoteCourse,
    tasks: &[RemoteTask],
    settings: &ResolvedProviderRuntimeSettings,
    start_ordinal: u32,
) -> ProviderResult<UaiCourseResidenceBatchPlan> {
    let total_residence_seconds = settings
        .duration_seconds(BROWSER_RESIDENCE_SECONDS_KEY)
        .filter(|seconds| (MIN_RESIDENCE_SECONDS..=MAX_RESIDENCE_SECONDS).contains(seconds))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course residence batch has no valid frozen duration budget",
            )
        })?;
    let play_video = settings.boolean(BROWSER_PLAY_VIDEO_KEY).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI Course residence batch has no valid frozen video setting",
        )
    })?;
    let membership = build_membership(course, tasks)?;
    let start_micro_identity_digest = membership
        .micros
        .get(start_ordinal as usize)
        .map(UaiCourseResidenceMicro::identity_digest)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course residence start ordinal is outside fresh membership",
            )
        })?;
    let mut plan = UaiCourseResidenceBatchPlan {
        version: UAI_COURSE_RESIDENCE_BATCH_VERSION,
        course_remote_id: membership.course_remote_id,
        course_publish_version: membership.course_publish_version,
        total_residence_seconds,
        play_video,
        micros: membership.micros,
        start: UaiCourseResidenceRestartTarget {
            ordinal: start_ordinal,
            micro_identity_digest: start_micro_identity_digest,
        },
        membership_digest: membership.digest,
        plan_digest: [0; 32],
    };
    plan.plan_digest = plan_digest(&plan);
    plan.validate()?;
    Ok(plan)
}

/// Freezes one credential-free durable child value around a validated Course
/// batch and an explicit Core-owned runtime settings/profile identity.
///
/// The owner Task must be one of the exact ordered Groups under the selected
/// start Micro so a fresh Browser plan can reproduce the cursor's first menu
/// target without guessing another child.
///
/// # Errors
///
/// Returns a typed error for malformed fingerprints, a zero profile digest or
/// an owner Task that is foreign to the selected start Micro.
pub fn build_course_residence_child_plan(
    course: &RemoteCourse,
    tasks: &[RemoteTask],
    settings: &ResolvedProviderRuntimeSettings,
    runtime_profile_digest: [u8; 32],
    owner_remote_task_id: &str,
    start_ordinal: u32,
) -> ProviderResult<UaiCourseResidenceChildPlan> {
    if runtime_profile_digest == [0; 32] {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI Course residence child plan requires an explicit runtime profile identity",
        ));
    }
    let batch = build_course_residence_batch_plan(course, tasks, settings, start_ordinal)?;
    let start_micro = batch
        .micros()
        .get(start_ordinal as usize)
        .ok_or_else(invalid_child_plan)?;
    if !start_micro
        .tasks()
        .iter()
        .any(|task| task.remote_task_id() == owner_remote_task_id)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI Course residence child owner is foreign to the selected start Micro",
        ));
    }
    let ordered_tasks = tasks
        .iter()
        .map(|task| {
            if !valid_task_fingerprint(&task.fingerprint) {
                return Err(protocol_drift(
                    "UAI Course residence Task fingerprint is malformed",
                ));
            }
            Ok(UaiCourseResidenceChildTask {
                remote_task_id: task.remote_id.clone(),
                fingerprint: task.fingerprint.clone(),
            })
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    let plan = UaiCourseResidenceChildPlan {
        version: UAI_COURSE_RESIDENCE_CHILD_PLAN_VERSION,
        provider_plan_version: batch.version(),
        course_remote_id: batch.course_remote_id().to_owned(),
        course_publish_version: batch.course_publish_version(),
        runtime_profile_digest,
        owner_remote_task_id: owner_remote_task_id.to_owned(),
        ordered_tasks,
        start_ordinal,
        start_micro_identity_digest: start_micro.identity_digest(),
        batch_membership_digest: batch.membership_digest(),
        batch_plan_digest: batch.plan_digest(),
    };
    plan.validate()?;
    plan.encode()?;
    Ok(plan)
}

struct CourseMembership {
    course_remote_id: String,
    course_publish_version: Option<u64>,
    micros: Vec<UaiCourseResidenceMicro>,
    digest: [u8; 32],
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct MicroKey {
    unit: String,
    section: Option<String>,
    micro: String,
}

struct TaskMembership {
    key: MicroKey,
    unit_title: String,
    section_title: Option<String>,
    micro_title: String,
    task: UaiCourseResidenceTask,
    publish_version: Option<u64>,
}

fn build_membership(
    course: &RemoteCourse,
    tasks: &[RemoteTask],
) -> ProviderResult<CourseMembership> {
    let resource_id = course_resource_id_from_remote(course)?;
    if course
        .metadata_sanitized
        .get("schema")
        .and_then(Value::as_str)
        != Some("uai.course-resource.v1")
        || tasks.is_empty()
        || tasks.len() > MAX_BATCH_TASKS
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI Course residence requires one complete fresh Course inventory",
        ));
    }
    let mut micros = Vec::<UaiCourseResidenceMicro>::new();
    let mut seen_micro_keys = BTreeSet::new();
    let mut seen_task_ids = BTreeSet::new();
    let mut course_publish_version = None;
    let mut publish_version_initialized = false;

    for task in tasks {
        let membership = parse_task_membership(course, &resource_id, task)?;
        if !seen_task_ids.insert(task.remote_id.as_str()) {
            return Err(protocol_drift(
                "UAI Course residence contains a duplicate Task identity",
            ));
        }
        if !publish_version_initialized {
            course_publish_version = membership.publish_version;
            publish_version_initialized = true;
        } else if course_publish_version != membership.publish_version {
            return Err(protocol_drift(
                "UAI Course residence Tasks disagree on Course publish version",
            ));
        }

        if let Some(current) = micros.last_mut().filter(|micro| {
            micro.unit_id == membership.key.unit
                && micro.section_id == membership.key.section
                && micro.micro_id == membership.key.micro
        }) {
            if current.unit_title != membership.unit_title
                || current.section_title != membership.section_title
                || current.micro_title != membership.micro_title
            {
                return Err(protocol_drift(
                    "UAI Course residence Micro labels changed within one hierarchy",
                ));
            }
            current.tasks.push(membership.task);
            continue;
        }

        if !seen_micro_keys.insert(membership.key.clone()) {
            return Err(protocol_drift(
                "UAI Course residence Micro membership is non-contiguous or reordered",
            ));
        }
        if micros.len() >= MAX_BATCH_MICROS {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course residence exceeds the Micro limit",
            ));
        }
        let ordinal = u32::try_from(micros.len()).map_err(|_| invalid_plan())?;
        micros.push(UaiCourseResidenceMicro {
            ordinal,
            unit_id: membership.key.unit,
            unit_title: membership.unit_title,
            section_id: membership.key.section,
            section_title: membership.section_title,
            micro_id: membership.key.micro,
            micro_title: membership.micro_title,
            tasks: vec![membership.task],
            identity_digest: [0; 32],
        });
    }
    for micro in &mut micros {
        micro.identity_digest = micro_identity_digest(&course.remote_id, micro);
    }
    let digest = membership_digest(&course.remote_id, course_publish_version, &micros);
    Ok(CourseMembership {
        course_remote_id: course.remote_id.clone(),
        course_publish_version,
        micros,
        digest,
    })
}

fn parse_task_membership(
    course: &RemoteCourse,
    resource_id: &str,
    task: &RemoteTask,
) -> ProviderResult<TaskMembership> {
    let normalized = task
        .normalized
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Course residence Task has no normalized hierarchy"))?;
    if normalized.get("schema").and_then(Value::as_str) != Some("uai.group-task.v1")
        || normalized.get("course_resource_id").and_then(Value::as_str) != Some(resource_id)
        || task.course_remote_id.as_deref() != Some(course.remote_id.as_str())
        || !task.capabilities.contains(&TaskCapability::BrowserBridge)
        || !valid_text(&task.title, MAX_TITLE_BYTES)
    {
        return Err(protocol_drift(
            "UAI Course residence Task is foreign or not BrowserBridge-capable",
        ));
    }
    let (unit_id, unit_title) = required_label(normalized.get("unit"), "Task Unit")?;
    let section = optional_label(normalized.get("section"), "Task Section")?;
    let micro = optional_label(normalized.get("micro"), "Task Micro")?;
    let group_id = required_remote_component(normalized.get("group_id"), "Task Group ID")?;
    let expected_remote_id = format!("group:{resource_id}:{unit_id}:{group_id}");
    if task.remote_id != expected_remote_id {
        return Err(protocol_drift(
            "UAI Course residence Task remote identity changed",
        ));
    }
    let task_types = normalized
        .get("task_types")
        .and_then(Value::as_array)
        .filter(|values| values.len() <= MAX_TASK_TYPES)
        .ok_or_else(|| protocol_drift("UAI Course residence Task types are invalid"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| valid_text(value, MAX_TASK_TYPE_BYTES))
                .map(str::to_owned)
                .ok_or_else(|| protocol_drift("UAI Course residence Task type is invalid"))
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    let question_count = normalized
        .get("question_count")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| protocol_drift("UAI Course residence Question count is invalid"))?;
    let publish_version =
        match normalized.get("course_publish_version") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                protocol_drift("UAI Course residence publish version is invalid")
            })?),
        };
    let (micro_id, micro_title) =
        micro.unwrap_or_else(|| (format!("group.{group_id}"), task.title.clone()));
    Ok(TaskMembership {
        key: MicroKey {
            unit: unit_id,
            section: section.as_ref().map(|(id, _)| id.clone()),
            micro: micro_id,
        },
        unit_title,
        section_title: section.map(|(_, title)| title),
        micro_title,
        task: UaiCourseResidenceTask {
            remote_task_id: task.remote_id.clone(),
            title: task.title.clone(),
            task_types,
            question_count,
        },
        publish_version,
    })
}

fn required_label(value: Option<&Value>, label: &'static str) -> ProviderResult<(String, String)> {
    let value = value
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift(format!("UAI Course residence has no {label}")))?;
    Ok((
        required_remote_component(value.get("id"), label)?,
        required_text(value.get("title"), label)?,
    ))
}

fn optional_label(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<Option<(String, String)>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => required_label(Some(value), label).map(Some),
    }
}

fn membership_digest(
    course_remote_id: &str,
    publish_version: Option<u64>,
    micros: &[UaiCourseResidenceMicro],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    update_field(&mut digest, b"uai.course-residence.membership.v1");
    update_field(&mut digest, course_remote_id.as_bytes());
    update_optional_u64(&mut digest, publish_version);
    digest.update((micros.len() as u64).to_be_bytes());
    for micro in micros {
        digest.update(micro.identity_digest);
    }
    digest.finalize().into()
}

fn micro_identity_digest(course_remote_id: &str, micro: &UaiCourseResidenceMicro) -> [u8; 32] {
    let mut digest = Sha256::new();
    update_field(&mut digest, b"uai.course-residence.micro.v1");
    update_field(&mut digest, course_remote_id.as_bytes());
    digest.update(micro.ordinal.to_be_bytes());
    update_field(&mut digest, micro.unit_id.as_bytes());
    update_field(&mut digest, micro.unit_title.as_bytes());
    update_optional_field(&mut digest, micro.section_id.as_deref());
    update_optional_field(&mut digest, micro.section_title.as_deref());
    update_field(&mut digest, micro.micro_id.as_bytes());
    update_field(&mut digest, micro.micro_title.as_bytes());
    digest.update((micro.tasks.len() as u64).to_be_bytes());
    for task in &micro.tasks {
        update_field(&mut digest, task.remote_task_id.as_bytes());
        update_field(&mut digest, task.title.as_bytes());
        digest.update((task.task_types.len() as u64).to_be_bytes());
        for task_type in &task.task_types {
            update_field(&mut digest, task_type.as_bytes());
        }
        digest.update(task.question_count.to_be_bytes());
    }
    digest.finalize().into()
}

fn plan_digest(plan: &UaiCourseResidenceBatchPlan) -> [u8; 32] {
    let mut digest = Sha256::new();
    update_field(&mut digest, b"uai.course-residence.plan.v1");
    digest.update(plan.membership_digest);
    digest.update(plan.total_residence_seconds.to_be_bytes());
    digest.update([u8::from(plan.play_video)]);
    digest.update(plan.start.ordinal.to_be_bytes());
    digest.update(plan.start.micro_identity_digest);
    digest.finalize().into()
}

fn update_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn update_optional_field(digest: &mut Sha256, value: Option<&str>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        update_field(digest, value.as_bytes());
    }
}

fn update_optional_u64(digest: &mut Sha256, value: Option<u64>) {
    digest.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        digest.update(value.to_be_bytes());
    }
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_task_fingerprint(value: &str) -> bool {
    value.len() == 67
        && value.starts_with("v1:")
        && value[3..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI Course residence batch plan is internally inconsistent",
    )
}

fn invalid_child_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI Course residence child plan is invalid or internally inconsistent",
    )
}

fn stale_child_plan() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "UAI Course residence child plan no longer matches fresh inventory or settings",
    )
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_provider_api::{ProviderRuntimeSettingsPatch, ProviderSettingValue};
    use serde::Deserialize;

    use super::*;
    use crate::{parse_course_context, parse_course_inventory, parse_task_inventory};

    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str =
        include_str!("../../../fixtures/providers/uai/tasks/tree-browser-order.json");
    const EXPECTED: &str =
        include_str!("../../../fixtures/providers/uai/browser/course-residence-batch.json");

    #[derive(Deserialize)]
    struct ExpectedPlan {
        version: u32,
        course_remote_id: String,
        course_publish_version: u64,
        total_residence_seconds: u64,
        play_video: bool,
        start_ordinal: u32,
        all_micro_ids: Vec<String>,
        selected_micro_ids: Vec<String>,
        selected_denominator: u64,
        rounded_tabs: u32,
        rounded_tasks: u32,
        rounded_leaf_seconds: u64,
        restart_ordinal: u32,
        restarted_denominator: u64,
    }

    #[test]
    fn course_batch_freezes_source_order_start_membership_and_rational_budget() {
        let (course, tasks) = inventory();
        let expected: ExpectedPlan = serde_json::from_str(EXPECTED).unwrap();
        let plan = build_course_residence_batch_plan(
            &course,
            &tasks,
            &browser_settings(expected.total_residence_seconds, expected.play_video),
            expected.start_ordinal,
        )
        .unwrap();

        assert_eq!(plan.version(), expected.version);
        assert_eq!(plan.course_remote_id(), expected.course_remote_id);
        assert_eq!(
            plan.course_publish_version(),
            Some(expected.course_publish_version)
        );
        assert_eq!(
            plan.micros()
                .iter()
                .map(UaiCourseResidenceMicro::micro_id)
                .collect::<Vec<_>>(),
            expected.all_micro_ids
        );
        assert_eq!(
            plan.selected_micros()
                .iter()
                .map(UaiCourseResidenceMicro::micro_id)
                .collect::<Vec<_>>(),
            expected.selected_micro_ids
        );
        let share = plan.budget_share().unwrap();
        assert_eq!(share.denominator_micros(), expected.selected_denominator);
        assert_eq!(share.numerator_seconds(), expected.total_residence_seconds);
        assert_eq!(
            share
                .rounded_leaf_seconds(expected.rounded_tabs, expected.rounded_tasks)
                .unwrap(),
            expected.rounded_leaf_seconds
        );
        assert_ne!(plan.membership_digest(), [0; 32]);
        assert_ne!(plan.plan_digest(), plan.membership_digest());
        plan.validate_fresh_membership(&course, &tasks).unwrap();
    }

    #[test]
    fn restart_requires_plan_owned_micro_identity_and_recomputes_budget() {
        let (course, tasks) = inventory();
        let expected: ExpectedPlan = serde_json::from_str(EXPECTED).unwrap();
        let plan = build_course_residence_batch_plan(
            &course,
            &tasks,
            &browser_settings(expected.total_residence_seconds, expected.play_video),
            expected.start_ordinal,
        )
        .unwrap();
        let restart = plan.restart_target(expected.restart_ordinal).unwrap();
        let restarted = plan.restart_at(&restart).unwrap();
        assert_eq!(restarted.start().ordinal(), expected.restart_ordinal);
        assert_eq!(
            restarted.budget_share().unwrap().denominator_micros(),
            expected.restarted_denominator
        );
        assert_eq!(restarted.membership_digest(), plan.membership_digest());
        assert_ne!(restarted.plan_digest(), plan.plan_digest());

        let mut foreign = restart;
        foreign.micro_identity_digest = [9; 32];
        assert_eq!(
            plan.restart_at(&foreign).unwrap_err().kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn fresh_membership_ignores_progress_but_rejects_order_and_shape_drift() {
        let (course, tasks) = inventory();
        let settings = browser_settings(1_200, true);
        let plan = build_course_residence_batch_plan(&course, &tasks, &settings, 1).unwrap();
        let mut progress_changed = tasks.clone();
        progress_changed[0].remote_state = asterism_domain::RemoteState::Completed;
        progress_changed[0].normalized["micro_progress"] = serde_json::json!({"completed": true});
        plan.validate_fresh_membership(&course, &progress_changed)
            .unwrap();

        let mut reordered = tasks.clone();
        reordered.swap(0, 1);
        assert!(plan.validate_fresh_membership(&course, &reordered).is_err());
        let mut renamed = tasks;
        renamed[0].title = "Changed Task".to_owned();
        assert!(plan.validate_fresh_membership(&course, &renamed).is_err());
    }

    #[test]
    fn batch_rejects_invalid_start_noncontiguous_micro_and_dom_overflow() {
        let (course, tasks) = inventory();
        let settings = browser_settings(1_200, false);
        assert!(build_course_residence_batch_plan(&course, &tasks, &settings, 3).is_err());

        let mut noncontiguous = tasks.clone();
        noncontiguous.swap(2, 3);
        assert!(build_course_residence_batch_plan(&course, &noncontiguous, &settings, 0).is_err());
        let share = build_course_residence_batch_plan(&course, &tasks, &settings, 0)
            .unwrap()
            .budget_share()
            .unwrap();
        assert!(
            share
                .rounded_leaf_seconds(MAX_TABS_PER_MICRO + 1, 1)
                .is_err()
        );
        assert!(
            share
                .rounded_leaf_seconds(1, MAX_TASKS_PER_TAB + 1)
                .is_err()
        );
    }

    #[test]
    fn child_plan_round_trips_and_rebuilds_exact_cursor_start() {
        let (course, tasks) = inventory();
        let settings = browser_settings(1_200, true);
        let owner = tasks[1].remote_id.as_str();
        let profile_digest = [7; 32];
        let child =
            build_course_residence_child_plan(&course, &tasks, &settings, profile_digest, owner, 1)
                .unwrap();
        let encoded = child.encode().unwrap();
        let (restored, batch) = UaiCourseResidenceChildPlan::decode_bound(
            &encoded,
            &course,
            &tasks,
            &settings,
            profile_digest,
        )
        .unwrap();

        assert_eq!(restored, child);
        assert_eq!(restored.version(), 1);
        assert_eq!(restored.provider_plan_version(), batch.version());
        assert_eq!(restored.course_remote_id(), course.remote_id);
        assert_eq!(restored.owner_remote_task_id(), owner);
        assert_eq!(restored.start_ordinal(), 1);
        assert_eq!(restored.runtime_profile_digest(), profile_digest);
        assert_eq!(batch.start().ordinal(), restored.start_ordinal());
        assert!(
            batch.selected_micros()[0]
                .tasks()
                .iter()
                .any(|task| task.remote_task_id() == restored.owner_remote_task_id())
        );

        let debug = format!("{restored:?}");
        assert!(!debug.contains(&course.remote_id));
        assert!(!debug.contains(owner));
        assert!(!debug.contains(&tasks[0].fingerprint));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn child_plan_fails_closed_on_foreign_reordered_or_stale_inputs() {
        let (course, tasks) = inventory();
        let settings = browser_settings(1_200, false);
        let owner = tasks[1].remote_id.as_str();
        let child =
            build_course_residence_child_plan(&course, &tasks, &settings, [8; 32], owner, 1)
                .unwrap();
        let encoded = child.encode().unwrap();

        let mut fingerprint_changed = tasks.clone();
        fingerprint_changed[0].fingerprint = format!("v1:{}", "a".repeat(64));
        assert_eq!(
            UaiCourseResidenceChildPlan::decode_bound(
                &encoded,
                &course,
                &fingerprint_changed,
                &settings,
                [8; 32],
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );

        let mut reordered = tasks.clone();
        reordered.swap(1, 2);
        assert!(
            UaiCourseResidenceChildPlan::decode_bound(
                &encoded, &course, &reordered, &settings, [8; 32],
            )
            .is_err()
        );
        let mut foreign_course = course.clone();
        foreign_course.remote_id = "foreign-course".to_owned();
        assert!(
            UaiCourseResidenceChildPlan::decode_bound(
                &encoded,
                &foreign_course,
                &tasks,
                &settings,
                [8; 32],
            )
            .is_err()
        );
        assert_eq!(
            UaiCourseResidenceChildPlan::decode_bound(
                &encoded, &course, &tasks, &settings, [9; 32],
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(
            UaiCourseResidenceChildPlan::decode_bound(
                &encoded,
                &course,
                &tasks,
                &browser_settings(1_260, false),
                [8; 32],
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn child_plan_rejects_schema_drift_and_foreign_start_owner() {
        let (course, tasks) = inventory();
        let settings = browser_settings(1_200, true);
        assert!(
            build_course_residence_child_plan(
                &course,
                &tasks,
                &settings,
                [5; 32],
                &tasks[0].remote_id,
                1,
            )
            .is_err()
        );
        assert!(
            build_course_residence_child_plan(
                &course,
                &tasks,
                &settings,
                [0; 32],
                &tasks[1].remote_id,
                1,
            )
            .is_err()
        );

        let child = build_course_residence_child_plan(
            &course,
            &tasks,
            &settings,
            [5; 32],
            &tasks[1].remote_id,
            1,
        )
        .unwrap();
        let mut value: Value = serde_json::from_slice(&child.encode().unwrap()).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(UaiCourseResidenceChildPlan::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("unknown");
        value["provider_plan_version"] = serde_json::json!(99);
        assert!(UaiCourseResidenceChildPlan::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(
            UaiCourseResidenceChildPlan::decode(&vec![b' '; MAX_CHILD_PLAN_BYTES + 1]).is_err()
        );
    }

    fn inventory() -> (RemoteCourse, Vec<RemoteTask>) {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tasks = parse_task_inventory(&course, &context, TREE).unwrap();
        (course, tasks)
    }

    fn browser_settings(seconds: u64, play_video: bool) -> ResolvedProviderRuntimeSettings {
        let schema = crate::runtime_settings::runtime_settings_schema();
        let patch = ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    BROWSER_RESIDENCE_SECONDS_KEY.to_owned(),
                    ProviderSettingValue::DurationSeconds(seconds),
                ),
                (
                    BROWSER_PLAY_VIDEO_KEY.to_owned(),
                    ProviderSettingValue::Boolean(play_video),
                ),
            ]),
        };
        schema.resolve(None, None, Some(&patch)).unwrap()
    }
}
