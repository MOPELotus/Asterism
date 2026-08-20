use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_domain::{CourseId, ProviderAccountId, ProviderId, TaskCapability};
use asterism_provider_api::{
    BatchExecutionPlanningRequest, CourseInventoryCapability, ExecutionParentBatchSnapshot,
    PreparedProviderBatchExecutionPlan, ProviderBatchExecutionPlanningInput, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderExecutionBatchPlan, ProviderIdentity,
    ProviderMetadata, ProviderResult,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};

use crate::batch_plan::decode_execution_parent_batch_snapshot;
use crate::{
    WellearnAtomicBatchPlanningAuthority, WellearnAtomicCompletionProfile,
    WellearnAtomicDurationCompletionPlan, WellearnAutoDurationBudget, WellearnBatchFlow,
    WellearnBatchUnitSelection, WellearnCourseInventory, WellearnTaskInventory,
    build_selected_batch_plan, metadata::development_metadata, prepare_atomic_execution_batch_plan,
    restore_atomic_batch_dispatch_plan, runtime_settings::WellearnRuntimeSettings,
};

/// Namespaced Provider-private input used by Core's Course batch planner.
pub const WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_TYPE: &str = "welearn.atomic-batch-request.v1";
/// Namespaced credential-free product input accepted by the `WELearn` public
/// batch adapter before Core encrypts the private planning input.
pub const WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE: &str = "welearn.public-batch-request.v1";
/// Namespaced credential-free persistence type for the exact public-to-private
/// batch materialization binding.
pub const WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE: &str =
    "welearn.public-batch-materialization-binding.v1";

const WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_VERSION: u16 = 1;
const WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_VERSION: u16 = 1;
const WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_VERSION: u16 = 1;
const MAX_PLANNING_TARGETS: usize = 8_192;
const MAX_PLANNING_INPUT_BYTES: usize = 1_024 * 1_024;
const MAX_PUBLIC_BATCH_INPUT_BYTES: usize = 1_024 * 1_024;
const MAX_PUBLIC_BATCH_MATERIALIZATION_BINDING_BYTES: usize = 8 * 1_024 * 1_024;
const WELLEARN_MATERIALIZED_CHILD_CAPABILITIES: [TaskCapability; 2] = [
    TaskCapability::DurationReport,
    TaskCapability::ResourceExecution,
];

/// Already-frozen duration authority supplied by the product/API boundary.
///
/// No variant grants the Provider permission to sample. Fanyuchang receives
/// one explicit target per expected fresh child; modular Auto receives the
/// complete configured/range/sample aggregate from which membership planning
/// deterministically derives equal-floor targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WellearnPublicBatchDurationPolicy {
    FrozenPerChildSeconds(Vec<u64>),
    FrozenAutoAggregate(WellearnAutoDurationBudget),
}

/// Explicit final score policy paired with an audited atomic donor flow.
///
/// The currently executable parent flows accept only Fanyuchang's fixed 100 or
/// modular Auto's fixed 0. Requiring the product to state that fact prevents a
/// public request from silently inheriting a Provider mutation goal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WellearnPublicBatchScorePolicy {
    Fixed(u8),
}

/// Credential-free, bounded public adapter input for an atomic `WELearn` Course
/// batch.
///
/// This value carries only explicit product authorization and already-frozen
/// policy outcomes. It performs no discovery, entropy sampling, persistence,
/// scheduling or mutation. Conversion produces the existing Provider-private
/// [`WellearnBatchExecutionPlanningInput`] that Core can encrypt and bind to a
/// parent Attempt.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnPublicBatchExecutionInput {
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelection,
    expected_remote_task_id: String,
    duration_policy: WellearnPublicBatchDurationPolicy,
    score_policy: WellearnPublicBatchScorePolicy,
}

/// Exact settings revisions frozen by Core for one public batch
/// materialization.
///
/// `None` means the corresponding scope used schema defaults and therefore had
/// no persisted override row. A present revision is always non-zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WellearnBatchRuntimeSettingsRevision {
    schema_version: u32,
    provider_revision: Option<u32>,
    provider_account_revision: Option<u32>,
}

impl WellearnBatchRuntimeSettingsRevision {
    /// # Errors
    ///
    /// Rejects an unversioned schema or a zero persisted settings revision.
    pub fn try_new(
        schema_version: u32,
        provider_revision: Option<u32>,
        provider_account_revision: Option<u32>,
    ) -> ProviderResult<Self> {
        if schema_version == 0
            || provider_revision == Some(0)
            || provider_account_revision == Some(0)
        {
            return Err(invalid_public_batch_materialization_binding());
        }
        Ok(Self {
            schema_version,
            provider_revision,
            provider_account_revision,
        })
    }

    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }

    pub const fn provider_revision(self) -> Option<u32> {
        self.provider_revision
    }

    pub const fn provider_account_revision(self) -> Option<u32> {
        self.provider_account_revision
    }
}

/// Core-owned local identity and settings scope against which Provider
/// materialization must be rebound.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnBatchMaterializationScope {
    provider_id: ProviderId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    runtime_settings_revision: WellearnBatchRuntimeSettingsRevision,
    expected_child_count: u32,
}

impl WellearnBatchMaterializationScope {
    /// # Errors
    ///
    /// Rejects an invalid settings revision or child cardinality. Provider,
    /// account and Course identities remain explicit and are checked against
    /// the prepared Provider values by the binding constructor.
    pub fn try_new(
        provider_id: ProviderId,
        provider_account_id: ProviderAccountId,
        course_id: CourseId,
        runtime_settings_revision: WellearnBatchRuntimeSettingsRevision,
        expected_child_count: u32,
    ) -> ProviderResult<Self> {
        let scope = Self {
            provider_id,
            provider_account_id,
            course_id,
            runtime_settings_revision,
            expected_child_count,
        };
        scope.validate()?;
        Ok(scope)
    }

    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub const fn provider_account_id(&self) -> ProviderAccountId {
        self.provider_account_id
    }

    pub const fn course_id(&self) -> CourseId {
        self.course_id
    }

    pub const fn runtime_settings_revision(&self) -> WellearnBatchRuntimeSettingsRevision {
        self.runtime_settings_revision
    }

    pub const fn expected_child_count(&self) -> u32 {
        self.expected_child_count
    }

    fn validate(&self) -> ProviderResult<()> {
        WellearnBatchRuntimeSettingsRevision::try_new(
            self.runtime_settings_revision.schema_version,
            self.runtime_settings_revision.provider_revision,
            self.runtime_settings_revision.provider_account_revision,
        )?;
        if !(1..=u32::try_from(MAX_PLANNING_TARGETS).unwrap_or(u32::MAX))
            .contains(&self.expected_child_count)
        {
            return Err(invalid_public_batch_materialization_binding());
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnBatchMaterializationScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnBatchMaterializationScope")
            .field("provider_id", &self.provider_id)
            .field("provider_account_id", &"[REDACTED]")
            .field("course_id", &"[REDACTED]")
            .field("runtime_settings_revision", &self.runtime_settings_revision)
            .field("expected_child_count", &self.expected_child_count)
            .finish()
    }
}

/// One exact ordered Unit/SCO child selected by the fresh Provider plan.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnBatchMaterializedChildBinding {
    position: u32,
    unit_index: u32,
    sco_index: u32,
    remote_task_id: String,
}

impl WellearnBatchMaterializedChildBinding {
    pub const fn position(&self) -> u32 {
        self.position
    }

    pub const fn unit_index(&self) -> u32 {
        self.unit_index
    }

    pub const fn sco_index(&self) -> u32 {
        self.sco_index
    }

    pub fn remote_task_id(&self) -> &str {
        self.remote_task_id.as_str()
    }
}

impl fmt::Debug for WellearnBatchMaterializedChildBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnBatchMaterializedChildBinding")
            .field("position", &self.position)
            .field("unit_index", &self.unit_index)
            .field("sco_index", &self.sco_index)
            .field("remote_task_id", &"[REDACTED]")
            .finish()
    }
}

/// Pure, immutable rebind record between product authorization and one exact
/// Provider materialization.
///
/// The value freezes local Provider/account/Course scope, settings revisions,
/// complete selected Unit order (including empty Units), ordered SCO children,
/// the converted private planning-input digest and the exact parent pair. It
/// grants no persistence, scheduling, entropy or mutation authority.
#[derive(Clone, Eq, PartialEq)]
pub struct WellearnPublicBatchMaterializationBinding {
    scope: WellearnBatchMaterializationScope,
    course_remote_id: String,
    selection: WellearnBatchUnitSelection,
    selected_unit_indices: Vec<u32>,
    ordered_children: Vec<WellearnBatchMaterializedChildBinding>,
    private_planning_input_digest: [u8; 32],
    parent_authority_digest: [u8; 32],
    parent_batch_digest: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnBatchRuntimeSettingsRevisionWire {
    schema_version: u32,
    provider_revision: Option<u32>,
    provider_account_revision: Option<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnBatchMaterializationScopeWire {
    provider_id: String,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    runtime_settings_revision: WellearnBatchRuntimeSettingsRevisionWire,
    expected_child_count: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnBatchMaterializedChildBindingWire {
    position: u32,
    unit_index: u32,
    sco_index: u32,
    remote_task_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnPublicBatchMaterializationBindingWire {
    version: u16,
    scope: WellearnBatchMaterializationScopeWire,
    course_remote_id: String,
    selection: WellearnBatchUnitSelectionWire,
    selected_unit_indices: Vec<u32>,
    ordered_children: Vec<WellearnBatchMaterializedChildBindingWire>,
    private_planning_input_digest: [u8; 32],
    parent_authority_digest: [u8; 32],
    parent_batch_digest: [u8; 32],
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WellearnPublicBatchDurationPolicyKind {
    FrozenPerChildSeconds,
    FrozenAutoAggregate,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnPublicBatchDurationPolicyWire {
    kind: WellearnPublicBatchDurationPolicyKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_seconds: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_aggregate: Option<WellearnAutoDurationBudgetWire>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WellearnPublicBatchScorePolicyKind {
    Fixed,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnPublicBatchScorePolicyWire {
    kind: WellearnPublicBatchScorePolicyKind,
    percent: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WellearnPublicBatchExecutionInputWire {
    version: u16,
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selection: WellearnBatchUnitSelectionWire,
    expected_remote_task_id: String,
    duration_policy: WellearnPublicBatchDurationPolicyWire,
    score_policy: WellearnPublicBatchScorePolicyWire,
}

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

impl From<&WellearnPublicBatchDurationPolicy> for WellearnPublicBatchDurationPolicyWire {
    fn from(policy: &WellearnPublicBatchDurationPolicy) -> Self {
        match policy {
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(targets) => Self {
                kind: WellearnPublicBatchDurationPolicyKind::FrozenPerChildSeconds,
                target_seconds: Some(targets.clone()),
                auto_aggregate: None,
            },
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(budget) => Self {
                kind: WellearnPublicBatchDurationPolicyKind::FrozenAutoAggregate,
                target_seconds: None,
                auto_aggregate: Some((*budget).into()),
            },
        }
    }
}

impl TryFrom<WellearnPublicBatchDurationPolicyWire> for WellearnPublicBatchDurationPolicy {
    type Error = ProviderError;

    fn try_from(wire: WellearnPublicBatchDurationPolicyWire) -> Result<Self, Self::Error> {
        match (wire.kind, wire.target_seconds, wire.auto_aggregate) {
            (WellearnPublicBatchDurationPolicyKind::FrozenPerChildSeconds, Some(targets), None) => {
                Ok(Self::FrozenPerChildSeconds(targets))
            }
            (WellearnPublicBatchDurationPolicyKind::FrozenAutoAggregate, None, Some(budget)) => {
                WellearnAutoDurationBudget::try_from(budget)
                    .map(Self::FrozenAutoAggregate)
                    .map_err(|_| invalid_public_batch_input())
            }
            _ => Err(invalid_public_batch_input()),
        }
    }
}

impl WellearnPublicBatchExecutionInput {
    /// Constructs one credential-free public policy input from explicit,
    /// already-frozen facts.
    ///
    /// # Errors
    ///
    /// Rejects unsupported flows, malformed Course/Task/Unit selection,
    /// missing or mixed duration authority, non-donor score goals and every
    /// target or aggregate outside the existing atomic planner bounds.
    pub fn try_new(
        course_remote_id: impl Into<String>,
        flow: WellearnBatchFlow,
        selection: WellearnBatchUnitSelection,
        expected_remote_task_id: impl Into<String>,
        duration_policy: WellearnPublicBatchDurationPolicy,
        score_policy: WellearnPublicBatchScorePolicy,
    ) -> ProviderResult<Self> {
        let input = Self {
            course_remote_id: course_remote_id.into(),
            flow,
            selection,
            expected_remote_task_id: expected_remote_task_id.into(),
            duration_policy,
            score_policy,
        };
        input.to_private_planning_input()?;
        Ok(input)
    }

    /// Encodes the deny-unknown public payload without credentials or entropy.
    /// The caller must persist and owner-bind these exact bytes before asking
    /// Core to create the parent `BatchExecution`.
    ///
    /// # Errors
    ///
    /// Rejects semantic drift, serialization failure or local size overflow.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.to_private_planning_input()?;
        let WellearnPublicBatchScorePolicy::Fixed(percent) = self.score_policy;
        let encoded = serde_json::to_vec(&WellearnPublicBatchExecutionInputWire {
            version: WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_VERSION,
            course_remote_id: self.course_remote_id.clone(),
            flow: self.flow,
            selection: WellearnBatchUnitSelectionWire::from(&self.selection),
            expected_remote_task_id: self.expected_remote_task_id.clone(),
            duration_policy: WellearnPublicBatchDurationPolicyWire::from(&self.duration_policy),
            score_policy: WellearnPublicBatchScorePolicyWire {
                kind: WellearnPublicBatchScorePolicyKind::Fixed,
                percent,
            },
        })
        .map_err(|_| invalid_public_batch_input())?;
        if encoded.is_empty() || encoded.len() > MAX_PUBLIC_BATCH_INPUT_BYTES {
            return Err(invalid_public_batch_input());
        }
        Ok(encoded)
    }

    /// Decodes only the exact `WELearn` public input namespace and deny-unknown
    /// v1 schema, then re-enters every typed policy constructor.
    ///
    /// # Errors
    ///
    /// Rejects a foreign type, unknown field, schema drift, malformed policy,
    /// implicit entropy or every invalid existing planning-input combination.
    pub fn decode(input_type: &str, encoded: &[u8]) -> ProviderResult<Self> {
        if input_type != WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE
            || encoded.is_empty()
            || encoded.len() > MAX_PUBLIC_BATCH_INPUT_BYTES
        {
            return Err(invalid_public_batch_input());
        }
        let wire: WellearnPublicBatchExecutionInputWire =
            serde_json::from_slice(encoded).map_err(|_| invalid_public_batch_input())?;
        if wire.version != WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_VERSION {
            return Err(invalid_public_batch_input());
        }
        let duration_policy = WellearnPublicBatchDurationPolicy::try_from(wire.duration_policy)?;
        let score_policy = match wire.score_policy.kind {
            WellearnPublicBatchScorePolicyKind::Fixed => {
                WellearnPublicBatchScorePolicy::Fixed(wire.score_policy.percent)
            }
        };
        Self::try_new(
            wire.course_remote_id,
            wire.flow,
            wire.selection.into(),
            wire.expected_remote_task_id,
            duration_policy,
            score_policy,
        )
        .map_err(|_| invalid_public_batch_input())
    }

    /// Converts explicit product authorization into the existing
    /// Provider-private planning value. This function performs no I/O,
    /// persistence, entropy sampling or scheduling.
    ///
    /// # Errors
    ///
    /// Returns the same fail-closed policy and shape errors as [`Self::try_new`].
    pub fn to_private_planning_input(&self) -> ProviderResult<WellearnBatchExecutionPlanningInput> {
        let WellearnPublicBatchScorePolicy::Fixed(score_percent) = self.score_policy;
        let (targets, auto_budget) = match (&self.duration_policy, self.flow, score_percent) {
            (
                WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(targets),
                WellearnBatchFlow::FanyuchangDuration,
                100,
            ) => (Some(targets.clone()), None),
            (
                WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(budget),
                WellearnBatchFlow::AutoDuration,
                0,
            ) => (None, Some(*budget)),
            _ => return Err(invalid_public_batch_input()),
        };
        WellearnBatchExecutionPlanningInput::try_new(
            self.course_remote_id.clone(),
            self.flow,
            self.selection.clone(),
            self.expected_remote_task_id.clone(),
            targets,
            auto_budget,
        )
        .map_err(|_| invalid_public_batch_input())
    }

    /// Converts directly into Core's namespaced encrypted planning-input
    /// wrapper without performing any Provider discovery or scheduling.
    ///
    /// # Errors
    ///
    /// Returns the public-policy or existing private-input validation error.
    pub fn to_provider_planning_input(
        &self,
    ) -> ProviderResult<ProviderBatchExecutionPlanningInput> {
        self.to_private_planning_input()?
            .to_provider_planning_input()
    }
}

impl WellearnPublicBatchMaterializationBinding {
    /// Binds one validated public policy and its exact converted private input
    /// to the complete fresh Provider materialization.
    ///
    /// This function is pure: it decodes and revalidates immutable values but
    /// performs no discovery, storage, scheduling, entropy or mutation.
    ///
    /// # Errors
    ///
    /// Rejects Provider/account/Course scope drift, invalid settings
    /// revisions, private-input substitution, parent-pair substitution,
    /// cardinality drift, Unit-order drift or any reordered/mixed SCO child.
    pub fn try_new(
        public_input: &WellearnPublicBatchExecutionInput,
        scope: &WellearnBatchMaterializationScope,
        private_input: &ProviderBatchExecutionPlanningInput,
        prepared: &PreparedProviderBatchExecutionPlan,
    ) -> ProviderResult<Self> {
        scope.validate()?;
        let metadata = development_metadata()?;
        let expected_private = public_input.to_private_planning_input()?;
        let expected_generic = expected_private.to_provider_planning_input()?;
        let actual_private = WellearnBatchExecutionPlanningInput::from_provider_planning_input(
            private_input,
            &metadata,
        )
        .map_err(|_| invalid_public_batch_materialization_binding())?;
        if scope.provider_id != metadata.id
            || private_input.provider_id() != &scope.provider_id
            || private_input.input_type() != WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_TYPE
            || private_input.input_digest() != expected_generic.input_digest()
            || actual_private != expected_private
            || prepared.parent_snapshot().provider_id() != &scope.provider_id
            || prepared.execution_batch_plan().provider_id() != &scope.provider_id
            || prepared.execution_batch_plan().authority_digest()
                != prepared.parent_snapshot().authority_digest()
            || prepared.execution_batch_plan().batch_digest()
                != prepared.parent_snapshot().batch_digest()
        {
            return Err(invalid_public_batch_materialization_binding());
        }

        let (authority, batch, frozen_fanyuchang_targets) =
            decode_execution_parent_batch_snapshot(prepared.parent_snapshot())
                .map_err(|_| invalid_public_batch_materialization_binding())?;
        let restored_dispatch = restore_atomic_batch_dispatch_plan(prepared.parent_snapshot())
            .map_err(|_| invalid_public_batch_materialization_binding())?;
        let restored_core = restored_dispatch
            .to_provider_execution_batch_plan(prepared.parent_snapshot())
            .map_err(|_| invalid_public_batch_materialization_binding())?;
        if restored_core != *prepared.execution_batch_plan()
            || restored_dispatch.batch_plan() != &batch
            || authority.course_remote_id() != public_input.course_remote_id
            || authority.flow() != public_input.flow
            || authority.selection() != &public_input.selection
            || authority.expected_remote_task_id() != public_input.expected_remote_task_id
            || batch.course_remote_id != public_input.course_remote_id
            || batch.flow != public_input.flow
            || batch.selection != public_input.selection
        {
            return Err(invalid_public_batch_materialization_binding());
        }

        let duration_matches = match &public_input.duration_policy {
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(targets) => {
                frozen_fanyuchang_targets.as_deref() == Some(targets.as_slice())
                    && authority.frozen_auto_duration_budget().is_none()
            }
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(budget) => {
                frozen_fanyuchang_targets.is_none()
                    && authority.frozen_auto_duration_budget() == Some(*budget)
            }
        };
        let expected_child_count = u32::try_from(batch.entries.len())
            .map_err(|_| invalid_public_batch_materialization_binding())?;
        if !duration_matches
            || scope.expected_child_count != expected_child_count
            || prepared.execution_batch_plan().children().len() != batch.entries.len()
            || batch
                .entries
                .iter()
                .filter(|entry| entry.remote_task_id == public_input.expected_remote_task_id)
                .count()
                != 1
        {
            return Err(invalid_public_batch_materialization_binding());
        }

        let selected_unit_indices = batch
            .selected_units
            .iter()
            .map(|unit| unit.index)
            .collect::<Vec<_>>();
        let ordered_children = batch
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                Ok(WellearnBatchMaterializedChildBinding {
                    position: u32::try_from(index + 1)
                        .map_err(|_| invalid_public_batch_materialization_binding())?,
                    unit_index: entry.unit_index,
                    sco_index: u32::try_from(entry.sco_index)
                        .map_err(|_| invalid_public_batch_materialization_binding())?,
                    remote_task_id: entry.remote_task_id.clone(),
                })
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        let binding = Self {
            scope: scope.clone(),
            course_remote_id: public_input.course_remote_id.clone(),
            selection: public_input.selection.clone(),
            selected_unit_indices,
            ordered_children,
            private_planning_input_digest: private_input.input_digest(),
            parent_authority_digest: prepared.parent_snapshot().authority_digest(),
            parent_batch_digest: prepared.parent_snapshot().batch_digest(),
        };
        binding.validate_shape()?;
        Ok(binding)
    }

    /// Encodes this credential-free materialization record under one bounded,
    /// deny-unknown v1 persistence schema.
    ///
    /// The bytes contain local and remote identity bindings plus digests, but
    /// never credentials, private planning bytes, parent secrets or mutation
    /// authority. Main remains responsible for encrypting and owner-binding
    /// the record at rest.
    ///
    /// # Errors
    ///
    /// Rejects an invalid in-memory shape, serialization failure or output
    /// larger than the complete-parent eight-MiB local ceiling.
    pub fn encode(&self) -> ProviderResult<Vec<u8>> {
        self.validate_shape()?;
        let encoded = serde_json::to_vec(&WellearnPublicBatchMaterializationBindingWire {
            version: WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_VERSION,
            scope: WellearnBatchMaterializationScopeWire {
                provider_id: self.scope.provider_id.as_str().to_owned(),
                provider_account_id: self.scope.provider_account_id,
                course_id: self.scope.course_id,
                runtime_settings_revision: WellearnBatchRuntimeSettingsRevisionWire {
                    schema_version: self.scope.runtime_settings_revision.schema_version,
                    provider_revision: self.scope.runtime_settings_revision.provider_revision,
                    provider_account_revision: self
                        .scope
                        .runtime_settings_revision
                        .provider_account_revision,
                },
                expected_child_count: self.scope.expected_child_count,
            },
            course_remote_id: self.course_remote_id.clone(),
            selection: WellearnBatchUnitSelectionWire::from(&self.selection),
            selected_unit_indices: self.selected_unit_indices.clone(),
            ordered_children: self
                .ordered_children
                .iter()
                .map(|child| WellearnBatchMaterializedChildBindingWire {
                    position: child.position,
                    unit_index: child.unit_index,
                    sco_index: child.sco_index,
                    remote_task_id: child.remote_task_id.clone(),
                })
                .collect(),
            private_planning_input_digest: self.private_planning_input_digest,
            parent_authority_digest: self.parent_authority_digest,
            parent_batch_digest: self.parent_batch_digest,
        })
        .map_err(|_| invalid_public_batch_materialization_binding())?;
        if encoded.is_empty() || encoded.len() > MAX_PUBLIC_BATCH_MATERIALIZATION_BINDING_BYTES {
            return Err(invalid_public_batch_materialization_binding());
        }
        Ok(encoded)
    }

    /// Decodes the bounded v1 record and fully recomputes its exact binding
    /// against Main's independently loaded authorization and prepared values.
    ///
    /// This intentionally has no unvalidated decode form. Main must provide
    /// the same public input, local scope, converted private planning input and
    /// prepared parent/child result; [`Self::validate`] reconstructs the whole
    /// expected record before the decoded value can be returned.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized bytes, malformed or unknown fields, version
    /// drift, invalid identifiers/revisions and every scope, identity, order,
    /// coordinate, input-digest or parent-digest substitution.
    pub fn decode(
        binding_type: &str,
        encoded: &[u8],
        public_input: &WellearnPublicBatchExecutionInput,
        scope: &WellearnBatchMaterializationScope,
        private_input: &ProviderBatchExecutionPlanningInput,
        prepared: &PreparedProviderBatchExecutionPlan,
    ) -> ProviderResult<Self> {
        let binding = Self::decode_shape(binding_type, encoded)?;
        binding.validate(public_input, scope, private_input, prepared)?;
        Ok(binding)
    }

    /// Decodes the exact persisted binding at child runtime and immediately
    /// rebinds it to the resolved parent/position/request before returning.
    ///
    /// Creation-time code must still use [`Self::decode`] with every original
    /// input. This entry exists because the runner intentionally receives only
    /// the encrypted materialization record plus the claimed child facts.
    ///
    /// # Errors
    ///
    /// Rejects every codec, scope, parent, position, Unit/SCO, remote Task,
    /// grouped capability, settings-schema or artifact drift.
    pub fn decode_for_child_dispatch(
        binding_type: &str,
        encoded: &[u8],
        context: &ProviderContext,
        parent: &ExecutionParentBatchSnapshot,
        position: u32,
        request: &asterism_provider_api::ExecutionRequest,
    ) -> ProviderResult<Self> {
        let binding = Self::decode_shape(binding_type, encoded)?;
        binding.validate_child_dispatch(context, parent, position, request)?;
        Ok(binding)
    }

    fn decode_shape(binding_type: &str, encoded: &[u8]) -> ProviderResult<Self> {
        if binding_type != WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE
            || encoded.is_empty()
            || encoded.len() > MAX_PUBLIC_BATCH_MATERIALIZATION_BINDING_BYTES
        {
            return Err(invalid_public_batch_materialization_binding());
        }
        let wire: WellearnPublicBatchMaterializationBindingWire =
            serde_json::from_slice(encoded)
                .map_err(|_| invalid_public_batch_materialization_binding())?;
        if wire.version != WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_VERSION {
            return Err(invalid_public_batch_materialization_binding());
        }
        let revision = WellearnBatchRuntimeSettingsRevision::try_new(
            wire.scope.runtime_settings_revision.schema_version,
            wire.scope.runtime_settings_revision.provider_revision,
            wire.scope
                .runtime_settings_revision
                .provider_account_revision,
        )?;
        let decoded_scope = WellearnBatchMaterializationScope::try_new(
            ProviderId::new(wire.scope.provider_id)
                .map_err(|_| invalid_public_batch_materialization_binding())?,
            wire.scope.provider_account_id,
            wire.scope.course_id,
            revision,
            wire.scope.expected_child_count,
        )?;
        let binding = Self {
            scope: decoded_scope,
            course_remote_id: wire.course_remote_id,
            selection: wire.selection.into(),
            selected_unit_indices: wire.selected_unit_indices,
            ordered_children: wire
                .ordered_children
                .into_iter()
                .map(|child| WellearnBatchMaterializedChildBinding {
                    position: child.position,
                    unit_index: child.unit_index,
                    sco_index: child.sco_index,
                    remote_task_id: child.remote_task_id,
                })
                .collect(),
            private_planning_input_digest: wire.private_planning_input_digest,
            parent_authority_digest: wire.parent_authority_digest,
            parent_batch_digest: wire.parent_batch_digest,
        };
        binding.validate_shape()?;
        Ok(binding)
    }

    /// Recomputes the complete pure binding and requires exact field equality.
    ///
    /// # Errors
    ///
    /// Returns an internal binding error for any cross-account, cross-Course,
    /// revision, count, Unit/SCO order, private-input or parent substitution.
    pub fn validate(
        &self,
        public_input: &WellearnPublicBatchExecutionInput,
        scope: &WellearnBatchMaterializationScope,
        private_input: &ProviderBatchExecutionPlanningInput,
        prepared: &PreparedProviderBatchExecutionPlan,
    ) -> ProviderResult<()> {
        self.validate_shape()?;
        let expected = Self::try_new(public_input, scope, private_input, prepared)?;
        if *self != expected {
            return Err(invalid_public_batch_materialization_binding());
        }
        Ok(())
    }

    pub const fn scope(&self) -> &WellearnBatchMaterializationScope {
        &self.scope
    }

    pub fn course_remote_id(&self) -> &str {
        self.course_remote_id.as_str()
    }

    pub const fn selection(&self) -> &WellearnBatchUnitSelection {
        &self.selection
    }

    pub fn selected_unit_indices(&self) -> &[u32] {
        &self.selected_unit_indices
    }

    pub fn ordered_children(&self) -> &[WellearnBatchMaterializedChildBinding] {
        &self.ordered_children
    }

    pub const fn private_planning_input_digest(&self) -> [u8; 32] {
        self.private_planning_input_digest
    }

    pub const fn parent_authority_digest(&self) -> [u8; 32] {
        self.parent_authority_digest
    }

    pub const fn parent_batch_digest(&self) -> [u8; 32] {
        self.parent_batch_digest
    }

    /// Rebinds one claimed child dispatch to this exact frozen
    /// materialization before either execution or recovery may perform I/O.
    ///
    /// # Errors
    ///
    /// Rejects a foreign Provider/account/Course context, parent digest,
    /// durable position, Unit/SCO coordinate, remote Task, grouped capability,
    /// runtime schema or child artifact substitution.
    pub fn validate_child_dispatch(
        &self,
        context: &ProviderContext,
        parent: &ExecutionParentBatchSnapshot,
        position: u32,
        request: &asterism_provider_api::ExecutionRequest,
    ) -> ProviderResult<()> {
        self.validate_shape()?;
        if context.provider_id != self.scope.provider_id
            || context.account_id != self.scope.provider_account_id
            || request.course_id != Some(self.scope.course_id)
            || request.runtime_settings.schema_version
                != self.scope.runtime_settings_revision.schema_version
            || parent.provider_id() != &self.scope.provider_id
            || parent.authority_digest() != self.parent_authority_digest
            || parent.batch_digest() != self.parent_batch_digest
            || !request.has_valid_capability_step()
            || request.requested_capabilities.as_slice() != WELLEARN_MATERIALIZED_CHILD_CAPABILITIES
            || request.capability_plan.as_slice() != WELLEARN_MATERIALIZED_CHILD_CAPABILITIES
            || request.capability_step_position != 1
        {
            return Err(invalid_public_batch_materialization_binding());
        }
        let child_index = usize::try_from(
            position
                .checked_sub(1)
                .ok_or_else(invalid_public_batch_materialization_binding)?,
        )
        .map_err(|_| invalid_public_batch_materialization_binding())?;
        let bound_child = self
            .ordered_children
            .get(child_index)
            .filter(|child| child.position == position)
            .ok_or_else(invalid_public_batch_materialization_binding)?;

        let (_, batch, _) = decode_execution_parent_batch_snapshot(parent)
            .map_err(|_| invalid_public_batch_materialization_binding())?;
        let selected_unit_indices = batch
            .selected_units
            .iter()
            .map(|unit| unit.index)
            .collect::<Vec<_>>();
        let parent_entry = batch
            .entries
            .get(child_index)
            .ok_or_else(invalid_public_batch_materialization_binding)?;
        let parent_sco_index = u32::try_from(parent_entry.sco_index)
            .map_err(|_| invalid_public_batch_materialization_binding())?;
        if batch.course_remote_id != self.course_remote_id
            || batch.selection != self.selection
            || selected_unit_indices != self.selected_unit_indices
            || parent_entry.unit_index != bound_child.unit_index
            || parent_sco_index != bound_child.sco_index
            || parent_entry.remote_task_id != bound_child.remote_task_id
            || request.remote_task_id != bound_child.remote_task_id
        {
            return Err(invalid_public_batch_materialization_binding());
        }

        let restored = restore_batch_execution_plan(parent)
            .map_err(|_| invalid_public_batch_materialization_binding())?;
        let provider_child = restored
            .children()
            .get(child_index)
            .filter(|child| child.position() == position)
            .ok_or_else(invalid_public_batch_materialization_binding)?;
        if provider_child.remote_task_id() != bound_child.remote_task_id
            || request.provider_plan_artifact.as_ref() != provider_child.execution_plan().artifact()
        {
            return Err(invalid_public_batch_materialization_binding());
        }
        Ok(())
    }

    fn validate_shape(&self) -> ProviderResult<()> {
        self.scope.validate()?;
        if self.course_remote_id.is_empty()
            || self.course_remote_id.len() > 512
            || self.course_remote_id.trim() != self.course_remote_id
            || self.course_remote_id.chars().any(char::is_control)
            || self.selected_unit_indices.is_empty()
            || self.selected_unit_indices.len() > MAX_PLANNING_TARGETS
            || self
                .selected_unit_indices
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != self.selected_unit_indices.len()
            || self.ordered_children.is_empty()
            || self.ordered_children.len() > MAX_PLANNING_TARGETS
            || u32::try_from(self.ordered_children.len()) != Ok(self.scope.expected_child_count)
            || self.private_planning_input_digest == [0; 32]
            || self.parent_authority_digest == [0; 32]
            || self.parent_batch_digest == [0; 32]
            || self
                .ordered_children
                .iter()
                .enumerate()
                .any(|(index, child)| {
                    u32::try_from(index + 1) != Ok(child.position)
                        || !self.selected_unit_indices.contains(&child.unit_index)
                        || child.remote_task_id.is_empty()
                        || child.remote_task_id.len() > 512
                        || child.remote_task_id.trim() != child.remote_task_id
                        || child.remote_task_id.chars().any(char::is_control)
                })
            || self
                .ordered_children
                .iter()
                .map(|child| child.remote_task_id.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                != self.ordered_children.len()
        {
            return Err(invalid_public_batch_materialization_binding());
        }
        match &self.selection {
            WellearnBatchUnitSelection::All => {}
            WellearnBatchUnitSelection::Explicit(indices)
                if indices == &self.selected_unit_indices => {}
            WellearnBatchUnitSelection::Explicit(_) => {
                return Err(invalid_public_batch_materialization_binding());
            }
        }
        Ok(())
    }
}

impl fmt::Debug for WellearnPublicBatchMaterializationBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnPublicBatchMaterializationBinding")
            .field("scope", &self.scope)
            .field("course_remote_id", &"[REDACTED]")
            .field("selection", &"[REDACTED]")
            .field("selected_unit_count", &self.selected_unit_indices.len())
            .field("ordered_child_count", &self.ordered_children.len())
            .field("private_planning_input_digest", &"[HASHED]")
            .field("parent_authority_digest", &"[HASHED]")
            .field("parent_batch_digest", &"[HASHED]")
            .finish()
    }
}

impl fmt::Debug for WellearnPublicBatchExecutionInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnPublicBatchExecutionInput")
            .field("flow", &self.flow)
            .field("course_remote_id", &"[REDACTED]")
            .field("selection", &"[REDACTED]")
            .field("expected_remote_task_id", &"[REDACTED]")
            .field("duration_policy", &"[REDACTED]")
            .field("score_policy", &self.score_policy)
            .finish()
    }
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

fn invalid_public_batch_input() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "WELearn public batch execution input is invalid",
    )
}

fn invalid_public_batch_materialization_binding() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn public batch materialization binding is invalid or drifted",
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
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:302",
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![0, 37, 19_800]),
            WellearnPublicBatchScorePolicy::Fixed(100),
        )
        .unwrap();
        let input = public.to_provider_planning_input().unwrap();
        let settings = settings();
        let capabilities = [
            TaskCapability::ResourceExecution,
            TaskCapability::DurationReport,
        ];
        let public_input = asterism_provider_api::ProviderBatchExecutionPublicInput::try_new(
            ProviderId::new("welearn").unwrap(),
            WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
            SecretValue::new(public.encode().unwrap()),
        )
        .unwrap();
        let request = request(&public_input, &input, &settings, &capabilities, 3);
        let context = context();

        let prepared = execution
            .prepare_batch_execution_plan(&context, &request)
            .await
            .unwrap();
        let persisted = execution
            .build_batch_execution_materialization_binding(&context, &request, &prepared)
            .unwrap()
            .unwrap();
        assert_eq!(persisted.provider_id(), &context.provider_id);
        assert_eq!(
            persisted.binding_type(),
            WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE
        );
        let revision = WellearnBatchRuntimeSettingsRevision::try_new(
            request.runtime_settings_revision.schema_version(),
            None,
            None,
        )
        .unwrap();
        let scope = WellearnBatchMaterializationScope::try_new(
            context.provider_id.clone(),
            context.account_id,
            request.course_id,
            revision,
            request.expected_child_count,
        )
        .unwrap();
        WellearnPublicBatchMaterializationBinding::decode(
            persisted.binding_type(),
            persisted.payload().expose_secret(),
            &public,
            &scope,
            &input,
            &prepared,
        )
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
        let public_input = fixture_public_input();
        let request = request(&public_input, &input, &settings, &capabilities, 3);
        let prepared = planner.prepare(&context(), &request).await.unwrap();

        assert_eq!(prepared.execution_batch_plan().children().len(), 3);
        let restored = restore_batch_execution_plan(prepared.parent_snapshot()).unwrap();
        assert_eq!(restored, *prepared.execution_batch_plan());
        assert!(format!("{private:?}").contains("[REDACTED]"));
        assert!(!format!("{private:?}").contains("sco:1001"));
    }

    #[test]
    fn public_fanyuchang_input_round_trips_only_frozen_policy_into_private_planning() {
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:302",
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![0, 37, 19_800]),
            WellearnPublicBatchScorePolicy::Fixed(100),
        )
        .unwrap();
        let encoded = public.encode().unwrap();
        let restored = WellearnPublicBatchExecutionInput::decode(
            WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
            &encoded,
        )
        .unwrap();
        assert_eq!(restored, public);
        assert_eq!(
            restored.to_private_planning_input().unwrap(),
            public.to_private_planning_input().unwrap()
        );
        let generic = restored.to_provider_planning_input().unwrap();
        assert_eq!(
            generic.input_type(),
            WELLEARN_BATCH_EXECUTION_PLANNING_INPUT_TYPE
        );
        let private = WellearnBatchExecutionPlanningInput::from_provider_planning_input(
            &generic,
            &development_metadata().unwrap(),
        )
        .unwrap();
        assert_eq!(private, public.to_private_planning_input().unwrap());

        let debug = format!("{public:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("course:1001"));
        assert!(!debug.contains("sco:1001"));
        assert!(!debug.contains("19800"));
    }

    #[test]
    fn public_auto_input_requires_the_complete_already_sampled_aggregate() {
        let budget = WellearnAutoDurationBudget::try_new(300, 30, -17).unwrap();
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(budget),
            WellearnPublicBatchScorePolicy::Fixed(0),
        )
        .unwrap();
        let encoded = public.encode().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            value["duration_policy"]["auto_aggregate"]["configured_minutes"],
            300
        );
        assert_eq!(
            value["duration_policy"]["auto_aggregate"]["random_range_minutes"],
            30
        );
        assert_eq!(
            value["duration_policy"]["auto_aggregate"]["sampled_offset_minutes"],
            -17
        );
        assert_eq!(
            value["duration_policy"]["auto_aggregate"]["actual_minutes"],
            budget.actual_minutes()
        );
        assert_eq!(
            WellearnPublicBatchExecutionInput::decode(
                WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
                &encoded,
            )
            .unwrap(),
            public
        );
    }

    #[test]
    fn public_input_rejects_unknown_fields_unfrozen_entropy_and_cross_flow_policy() {
        assert!(
            WellearnPublicBatchExecutionInput::try_new(
                "course:1001",
                WellearnBatchFlow::FanyuchangDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![1, 2, 3]),
                WellearnPublicBatchScorePolicy::Fixed(99),
            )
            .is_err()
        );
        assert!(
            WellearnPublicBatchExecutionInput::try_new(
                "course:1001",
                WellearnBatchFlow::AutoDuration,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![1, 2, 3]),
                WellearnPublicBatchScorePolicy::Fixed(0),
            )
            .is_err()
        );
        assert!(
            WellearnPublicBatchExecutionInput::try_new(
                "course:1001",
                WellearnBatchFlow::FanyuchangCompletion,
                WellearnBatchUnitSelection::All,
                "sco:1001:301",
                WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![1, 2, 3]),
                WellearnPublicBatchScorePolicy::Fixed(100),
            )
            .is_err()
        );

        let auto = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(
                WellearnAutoDurationBudget::try_new(2, 1, -1).unwrap(),
            ),
            WellearnPublicBatchScorePolicy::Fixed(0),
        )
        .unwrap();
        let encoded = auto.encode().unwrap();
        assert!(WellearnPublicBatchExecutionInput::decode("welearn.foreign.v1", &encoded).is_err());

        let mut unknown_root: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        unknown_root["credential"] = serde_json::json!("must-not-be-accepted");
        assert!(
            WellearnPublicBatchExecutionInput::decode(
                WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
                &serde_json::to_vec(&unknown_root).unwrap(),
            )
            .is_err()
        );
        let mut unknown_policy: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        unknown_policy["duration_policy"]["seed"] = serde_json::json!(42);
        assert!(
            WellearnPublicBatchExecutionInput::decode(
                WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
                &serde_json::to_vec(&unknown_policy).unwrap(),
            )
            .is_err()
        );
        let mut missing_sample: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        missing_sample["duration_policy"]["auto_aggregate"]
            .as_object_mut()
            .unwrap()
            .remove("sampled_offset_minutes");
        assert!(
            WellearnPublicBatchExecutionInput::decode(
                WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
                &serde_json::to_vec(&missing_sample).unwrap(),
            )
            .is_err()
        );
        let mut drifted_actual: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        drifted_actual["duration_policy"]["auto_aggregate"]["actual_minutes"] =
            serde_json::json!(2);
        assert!(
            WellearnPublicBatchExecutionInput::decode(
                WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
                &serde_json::to_vec(&drifted_actual).unwrap(),
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn public_materialization_binding_freezes_scope_revisions_and_ordered_membership() {
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::Explicit(vec![1, 0]),
            "sco:1001:302",
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![0, 37, 19_800]),
            WellearnPublicBatchScorePolicy::Fixed(100),
        )
        .unwrap();
        let (private, scope, prepared, binding) =
            prepare_materialization_binding(&public, Some(7), Some(11)).await;
        let account_id = scope.provider_account_id();
        let course_id = scope.course_id();
        let revision = scope.runtime_settings_revision();

        binding
            .validate(&public, &scope, &private, &prepared)
            .unwrap();
        let encoded = binding.encode().unwrap();
        let decoded = WellearnPublicBatchMaterializationBinding::decode(
            WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE,
            &encoded,
            &public,
            &scope,
            &private,
            &prepared,
        )
        .unwrap();
        assert_eq!(decoded, binding);
        assert!(!String::from_utf8(encoded).unwrap().contains("credential"));
        assert_eq!(binding.scope(), &scope);
        assert_eq!(binding.course_remote_id(), "course:1001");
        assert_eq!(
            binding.selection(),
            &WellearnBatchUnitSelection::Explicit(vec![1, 0])
        );
        assert_eq!(binding.selected_unit_indices(), &[1, 0]);
        assert_eq!(
            binding
                .ordered_children()
                .iter()
                .map(WellearnBatchMaterializedChildBinding::position)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            binding
                .ordered_children()
                .iter()
                .map(WellearnBatchMaterializedChildBinding::unit_index)
                .collect::<Vec<_>>(),
            vec![1, 0, 0]
        );
        assert_eq!(
            binding
                .ordered_children()
                .iter()
                .map(WellearnBatchMaterializedChildBinding::sco_index)
                .collect::<Vec<_>>(),
            vec![0, 0, 1]
        );
        assert_eq!(
            binding
                .ordered_children()
                .iter()
                .map(WellearnBatchMaterializedChildBinding::remote_task_id)
                .collect::<Vec<_>>(),
            vec!["sco:1001:401", "sco:1001:301", "sco:1001:302"]
        );
        assert_eq!(
            binding.private_planning_input_digest(),
            private.input_digest()
        );
        assert_eq!(
            binding.parent_authority_digest(),
            prepared.parent_snapshot().authority_digest()
        );
        assert_eq!(
            binding.parent_batch_digest(),
            prepared.parent_snapshot().batch_digest()
        );
        assert_eq!(revision.schema_version(), settings().schema_version);
        assert_eq!(revision.provider_revision(), Some(7));
        assert_eq!(revision.provider_account_revision(), Some(11));

        let debug = format!("{binding:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("[HASHED]"));
        assert!(!debug.contains("course:1001"));
        assert!(!debug.contains("sco:1001"));
        assert!(!debug.contains(&account_id.to_string()));
        assert!(!debug.contains(&course_id.to_string()));
        let decoded_debug = format!("{decoded:?}");
        assert!(!decoded_debug.contains("course:1001"));
        assert!(!decoded_debug.contains("sco:1001"));
        assert!(!decoded_debug.contains(&account_id.to_string()));
        assert!(!decoded_debug.contains(&course_id.to_string()));
    }

    #[tokio::test]
    async fn materialization_binding_decode_rejects_unknown_tampered_and_oversized_bytes() {
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(
                WellearnAutoDurationBudget::try_new(2, 1, -1).unwrap(),
            ),
            WellearnPublicBatchScorePolicy::Fixed(0),
        )
        .unwrap();
        let (private, scope, prepared, binding) =
            prepare_materialization_binding(&public, Some(3), Some(5)).await;
        let encoded = binding.encode().unwrap();
        let original: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let rejects = |value: &serde_json::Value| {
            WellearnPublicBatchMaterializationBinding::decode(
                WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE,
                &serde_json::to_vec(value).unwrap(),
                &public,
                &scope,
                &private,
                &prepared,
            )
            .is_err()
        };

        let mut unknown_root = original.clone();
        unknown_root["credential_refs"] = serde_json::json!(["must-not-be-accepted"]);
        assert!(rejects(&unknown_root));
        let mut unknown_scope = original.clone();
        unknown_scope["scope"]["owner_id"] = serde_json::json!("must-not-be-accepted");
        assert!(rejects(&unknown_scope));
        let mut unknown_child = original.clone();
        unknown_child["ordered_children"][0]["cookie"] = serde_json::json!("must-not-be-accepted");
        assert!(rejects(&unknown_child));

        let mut version_drift = original.clone();
        version_drift["version"] = serde_json::json!(2);
        assert!(rejects(&version_drift));
        let mut account_drift = original.clone();
        account_drift["scope"]["provider_account_id"] = serde_json::json!(ProviderAccountId::new());
        assert!(rejects(&account_drift));
        let mut course_drift = original.clone();
        course_drift["scope"]["course_id"] = serde_json::json!(CourseId::new());
        assert!(rejects(&course_drift));
        let mut remote_child_drift = original.clone();
        remote_child_drift["ordered_children"][0]["remote_task_id"] =
            serde_json::json!("sco:1001:foreign");
        assert!(rejects(&remote_child_drift));
        let mut coordinate_drift = original.clone();
        coordinate_drift["ordered_children"][0]["sco_index"] = serde_json::json!(99);
        assert!(rejects(&coordinate_drift));
        let mut digest_drift = original;
        let first_digest_byte = digest_drift["parent_batch_digest"][0].as_u64().unwrap();
        digest_drift["parent_batch_digest"][0] = serde_json::json!(first_digest_byte ^ 1);
        assert!(rejects(&digest_drift));

        assert!(
            WellearnPublicBatchMaterializationBinding::decode(
                "welearn.foreign-binding.v1",
                &encoded,
                &public,
                &scope,
                &private,
                &prepared,
            )
            .is_err()
        );

        assert!(
            WellearnPublicBatchMaterializationBinding::decode(
                WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE,
                &[],
                &public,
                &scope,
                &private,
                &prepared,
            )
            .is_err()
        );
        assert!(
            WellearnPublicBatchMaterializationBinding::decode(
                WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE,
                &vec![b'x'; MAX_PUBLIC_BATCH_MATERIALIZATION_BINDING_BYTES + 1],
                &public,
                &scope,
                &private,
                &prepared,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn materialization_binding_rejects_cross_scope_revision_and_child_order_drift() {
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::AutoDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(
                WellearnAutoDurationBudget::try_new(2, 1, -1).unwrap(),
            ),
            WellearnPublicBatchScorePolicy::Fixed(0),
        )
        .unwrap();
        let (private, settings, prepared) = prepare_public_batch(&public, 3).await;
        let revision = WellearnBatchRuntimeSettingsRevision::try_new(
            settings.schema_version,
            Some(3),
            Some(5),
        )
        .unwrap();
        let scope = WellearnBatchMaterializationScope::try_new(
            ProviderId::new("welearn").unwrap(),
            ProviderAccountId::new(),
            CourseId::new(),
            revision,
            3,
        )
        .unwrap();
        let binding = WellearnPublicBatchMaterializationBinding::try_new(
            &public, &scope, &private, &prepared,
        )
        .unwrap();

        let mut cross_account = scope.clone();
        cross_account.provider_account_id = ProviderAccountId::new();
        assert!(
            binding
                .validate(&public, &cross_account, &private, &prepared)
                .is_err()
        );
        let mut cross_course = scope.clone();
        cross_course.course_id = CourseId::new();
        assert!(
            binding
                .validate(&public, &cross_course, &private, &prepared)
                .is_err()
        );
        let mut revision_drift = scope.clone();
        revision_drift
            .runtime_settings_revision
            .provider_account_revision = Some(6);
        assert!(
            binding
                .validate(&public, &revision_drift, &private, &prepared)
                .is_err()
        );
        let mut count_drift = scope.clone();
        count_drift.expected_child_count = 2;
        assert!(
            binding
                .validate(&public, &count_drift, &private, &prepared)
                .is_err()
        );
        let foreign_provider = WellearnBatchMaterializationScope::try_new(
            ProviderId::new("foreign").unwrap(),
            scope.provider_account_id,
            scope.course_id,
            revision,
            3,
        )
        .unwrap();
        assert!(
            WellearnPublicBatchMaterializationBinding::try_new(
                &public,
                &foreign_provider,
                &private,
                &prepared,
            )
            .is_err()
        );

        let mut child_order_drift = binding.clone();
        child_order_drift.ordered_children.swap(0, 1);
        assert!(
            child_order_drift
                .validate(&public, &scope, &private, &prepared)
                .is_err()
        );
        let mut unit_order_drift = binding.clone();
        unit_order_drift.selected_unit_indices.swap(0, 1);
        assert!(
            unit_order_drift
                .validate(&public, &scope, &private, &prepared)
                .is_err()
        );
        let mut digest_drift = binding.clone();
        digest_drift.private_planning_input_digest[0] ^= 1;
        assert!(
            digest_drift
                .validate(&public, &scope, &private, &prepared)
                .is_err()
        );
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
                .prepare(
                    &context(),
                    &request(
                        &fixture_public_input(),
                        &foreign,
                        &settings,
                        &capabilities,
                        3,
                    ),
                )
                .await
                .is_err()
        );
        assert!(
            planner
                .prepare(
                    &context(),
                    &request(&fixture_public_input(), &valid, &settings, &capabilities, 2,),
                )
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
        public_input: &'a asterism_provider_api::ProviderBatchExecutionPublicInput,
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
            runtime_settings_revision:
                asterism_provider_api::ProviderBatchExecutionRuntimeSettingsRevision::try_new(
                    settings.schema_version,
                    None,
                    None,
                )
                .unwrap(),
            public_input,
            planning_input: input,
        }
    }

    fn fixture_public_input() -> asterism_provider_api::ProviderBatchExecutionPublicInput {
        let public = WellearnPublicBatchExecutionInput::try_new(
            "course:1001",
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchUnitSelection::All,
            "sco:1001:301",
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(vec![1, 2, 3]),
            WellearnPublicBatchScorePolicy::Fixed(100),
        )
        .unwrap();
        asterism_provider_api::ProviderBatchExecutionPublicInput::try_new(
            ProviderId::new("welearn").unwrap(),
            WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
            SecretValue::new(public.encode().unwrap()),
        )
        .unwrap()
    }

    fn fixture_batch_planner() -> WellearnBatchExecutionPlanner {
        WellearnBatchExecutionPlanner::try_new(
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
        .unwrap()
    }

    async fn prepare_public_batch(
        public: &WellearnPublicBatchExecutionInput,
        expected_child_count: u32,
    ) -> (
        ProviderBatchExecutionPlanningInput,
        asterism_provider_api::ResolvedProviderRuntimeSettings,
        PreparedProviderBatchExecutionPlan,
    ) {
        let private = public.to_provider_planning_input().unwrap();
        let settings = settings();
        let capabilities = [
            TaskCapability::DurationReport,
            TaskCapability::ResourceExecution,
        ];
        let prepared = fixture_batch_planner()
            .prepare(
                &context(),
                &request(
                    &asterism_provider_api::ProviderBatchExecutionPublicInput::try_new(
                        ProviderId::new("welearn").unwrap(),
                        WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
                        SecretValue::new(public.encode().unwrap()),
                    )
                    .unwrap(),
                    &private,
                    &settings,
                    &capabilities,
                    expected_child_count,
                ),
            )
            .await
            .unwrap();
        (private, settings, prepared)
    }

    async fn prepare_materialization_binding(
        public: &WellearnPublicBatchExecutionInput,
        provider_revision: Option<u32>,
        provider_account_revision: Option<u32>,
    ) -> (
        ProviderBatchExecutionPlanningInput,
        WellearnBatchMaterializationScope,
        PreparedProviderBatchExecutionPlan,
        WellearnPublicBatchMaterializationBinding,
    ) {
        let (private, settings, prepared) = prepare_public_batch(public, 3).await;
        let revision = WellearnBatchRuntimeSettingsRevision::try_new(
            settings.schema_version,
            provider_revision,
            provider_account_revision,
        )
        .unwrap();
        let scope = WellearnBatchMaterializationScope::try_new(
            ProviderId::new("welearn").unwrap(),
            ProviderAccountId::new(),
            CourseId::new(),
            revision,
            3,
        )
        .unwrap();
        let binding =
            WellearnPublicBatchMaterializationBinding::try_new(public, &scope, &private, &prepared)
                .unwrap();
        (private, scope, prepared, binding)
    }
}
