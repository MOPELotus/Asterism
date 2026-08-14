use std::fmt;

use asterism_domain::BrowserBridgeExchangeState;
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UaiBrowserCommand, UaiBrowserCommandEnvelope, UaiBrowserEvent, UaiBrowserEventEnvelope,
    UaiBrowserEventExchangeCompleted, UaiBrowserPageEntry, UaiBrowserPageScope,
    UaiBrowserResidenceControl, UaiBrowserResidencePlan, UaiBrowserSessionBinding,
    UaiCourseResidenceBatchPlan,
};

const UAI_BROWSER_CURSOR_VERSION: u32 = 4;
const MAX_BROWSER_CURSOR_ARTIFACT_BYTES: usize = 256 * 1_024;

/// Encrypted-at-rest accumulated cursor material for one exact next command.
///
/// Only the digest belongs in ordinary durable state. The serialized cursor
/// contains browser labels and opaque handles and must remain in Core's
/// encrypted artifact repository.
pub struct EncodedUaiBrowserCursorArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiBrowserCursorArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiBrowserCursorArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiBrowserCursorArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

/// One immutable accumulated cursor transition and its exact next command.
#[derive(Debug)]
pub struct UaiBrowserCursorAdvance {
    cursor: UaiBrowserResidenceCursor,
    command: UaiBrowserCommandEnvelope,
}

impl UaiBrowserCursorAdvance {
    pub const fn cursor(&self) -> &UaiBrowserResidenceCursor {
        &self.cursor
    }

    pub const fn command(&self) -> &UaiBrowserCommandEnvelope {
        &self.command
    }

    pub fn into_parts(self) -> (UaiBrowserResidenceCursor, UaiBrowserCommandEnvelope) {
        (self.cursor, self.command)
    }
}

/// Command-before-dispatch stage represented by one encrypted accumulated
/// browser cursor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UaiBrowserCursorStage {
    ScanningMenu,
    ClickingMenu,
    ScanningTabs,
    ClickingTab,
    ScanningTasks,
    ClickingTask,
    Residing,
    ControllingResidence,
}

/// Provider-private accumulated state embedded in the next encrypted command
/// artifact.
///
/// Core persists and indexes this value but never interprets it. Every field
/// required after a crash is either self-contained here or explicitly bound
/// to the prior encrypted raw result by sequence and digest.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserResidenceCursor {
    version: u32,
    course_remote_id: String,
    batch_membership_digest: [u8; 32],
    batch_plan_digest: [u8; 32],
    browser_plan_digest: [u8; 32],
    next_command_digest: [u8; 32],
    current_micro_ordinal: u32,
    current_micro_identity_digest: [u8; 32],
    stage: UaiBrowserCursorStage,
    tab_snapshot: Vec<UaiBrowserPageEntry>,
    next_tab_ordinal: u32,
    current_tab_ordinal: Option<u32>,
    task_snapshot: Vec<UaiBrowserPageEntry>,
    processed_micros: u32,
    processed_tabs: u32,
    processed_tasks: u32,
    remaining_active_seconds: u64,
    observed_video_seconds: u64,
    paused: bool,
    prior_result_sequence: Option<u32>,
    prior_result_digest: Option<[u8; 32]>,
}

impl UaiBrowserResidenceCursor {
    /// Creates the first self-contained cursor for a Course batch.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless the command is sequence-one `ScanMenu`
    /// for the exact first selected Micro and one of its fresh Group Tasks.
    pub fn begin(
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
    ) -> ProviderResult<Self> {
        batch.validate()?;
        let current = batch.selected_micros().first().ok_or_else(invalid_cursor)?;
        let browser_plan_digest = browser_plan_digest(plan)?;
        let next_command_digest = command.exchange_digest(plan)?;
        let cursor = Self {
            version: UAI_BROWSER_CURSOR_VERSION,
            course_remote_id: batch.course_remote_id().to_owned(),
            batch_membership_digest: batch.membership_digest(),
            batch_plan_digest: batch.plan_digest(),
            browser_plan_digest,
            next_command_digest,
            current_micro_ordinal: current.ordinal(),
            current_micro_identity_digest: current.identity_digest(),
            stage: UaiBrowserCursorStage::ScanningMenu,
            tab_snapshot: Vec::new(),
            next_tab_ordinal: 0,
            current_tab_ordinal: None,
            task_snapshot: Vec::new(),
            processed_micros: 0,
            processed_tabs: 0,
            processed_tasks: 0,
            remaining_active_seconds: batch.total_residence_seconds(),
            observed_video_seconds: 0,
            paused: false,
            prior_result_sequence: None,
            prior_result_digest: None,
        };
        cursor.validate_for_command(batch, plan, command)?;
        Ok(cursor)
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn course_remote_id(&self) -> &str {
        &self.course_remote_id
    }

    pub const fn batch_membership_digest(&self) -> [u8; 32] {
        self.batch_membership_digest
    }

    pub const fn batch_plan_digest(&self) -> [u8; 32] {
        self.batch_plan_digest
    }

    pub const fn browser_plan_digest(&self) -> [u8; 32] {
        self.browser_plan_digest
    }

    pub const fn next_command_digest(&self) -> [u8; 32] {
        self.next_command_digest
    }

    pub const fn current_micro_ordinal(&self) -> u32 {
        self.current_micro_ordinal
    }

    pub const fn current_micro_identity_digest(&self) -> [u8; 32] {
        self.current_micro_identity_digest
    }

    pub const fn stage(&self) -> UaiBrowserCursorStage {
        self.stage
    }

    pub fn tab_snapshot(&self) -> &[UaiBrowserPageEntry] {
        &self.tab_snapshot
    }

    pub const fn next_tab_ordinal(&self) -> u32 {
        self.next_tab_ordinal
    }

    pub const fn current_tab_ordinal(&self) -> Option<u32> {
        self.current_tab_ordinal
    }

    pub fn task_snapshot(&self) -> &[UaiBrowserPageEntry] {
        &self.task_snapshot
    }

    pub const fn processed_micros(&self) -> u32 {
        self.processed_micros
    }

    pub const fn processed_tabs(&self) -> u32 {
        self.processed_tabs
    }

    pub const fn processed_tasks(&self) -> u32 {
        self.processed_tasks
    }

    pub const fn remaining_active_seconds(&self) -> u64 {
        self.remaining_active_seconds
    }

    pub const fn observed_video_seconds(&self) -> u64 {
        self.observed_video_seconds
    }

    pub const fn paused(&self) -> bool {
        self.paused
    }

    pub const fn prior_result_sequence(&self) -> Option<u32> {
        self.prior_result_sequence
    }

    pub const fn prior_result_digest(&self) -> Option<[u8; 32]> {
        self.prior_result_digest
    }

    /// Advances the initial menu scan to its unique target-bound click.
    ///
    /// The completed event owner retains the exact raw-result digest and
    /// durable exchange correlation. The old cursor remains unchanged; the
    /// returned cursor owns the only next command authorized by that result.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless this cursor owns the issued `ScanMenu`,
    /// the completed exchange matches it exactly and the menu contains one
    /// unique fresh hierarchy target.
    pub fn advance_menu_list(
        &self,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        completed: &UaiBrowserEventExchangeCompleted,
    ) -> ProviderResult<UaiBrowserCursorAdvance> {
        self.validate_for_command(batch, plan, command)?;
        if self.stage != UaiBrowserCursorStage::ScanningMenu
            || !matches!(command.command, UaiBrowserCommand::ScanMenu)
        {
            return Err(stale_cursor());
        }
        let result_digest = completed_event_digest(self, command, completed)?;
        completed
            .event()
            .validate_for_command(plan, command, &completed.event().origin)?;
        let UaiBrowserEvent::MenuList { entries } = &completed.event().event else {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI accumulated cursor requires a completed menu-list event",
            ));
        };
        let binding = UaiBrowserSessionBinding::try_new(
            plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )?;
        let target = plan.select_target_menu_entry(&binding, entries)?;
        let next_sequence = command.sequence.checked_add(1).ok_or_else(invalid_cursor)?;
        let next_command =
            UaiBrowserCommandEnvelope::click_menu(plan, &binding, next_sequence, &target)?;
        let mut next = self.clone();
        next.stage = UaiBrowserCursorStage::ClickingMenu;
        next.prior_result_sequence = Some(command.sequence);
        next.prior_result_digest = Some(result_digest);
        next.next_command_digest = next_command.exchange_digest(plan)?;
        next.validate_for_command(batch, plan, &next_command)?;
        Ok(UaiBrowserCursorAdvance {
            cursor: next,
            command: next_command,
        })
    }

    /// Advances one accepted target menu click to the bounded Tab scan.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless this cursor owns the exact `ClickMenu`
    /// command and its completed event exchange proves that same opaque handle
    /// was accepted.
    pub fn advance_menu_click(
        &self,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        completed: &UaiBrowserEventExchangeCompleted,
    ) -> ProviderResult<UaiBrowserCursorAdvance> {
        self.validate_for_command(batch, plan, command)?;
        if self.stage != UaiBrowserCursorStage::ClickingMenu
            || !matches!(&command.command, UaiBrowserCommand::ClickMenu { .. })
        {
            return Err(stale_cursor());
        }
        let result_digest = completed_event_digest(self, command, completed)?;
        completed
            .event()
            .validate_for_command(plan, command, &completed.event().origin)?;
        if !matches!(
            &completed.event().event,
            UaiBrowserEvent::ClickResult { .. }
        ) {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI accumulated cursor requires a completed menu-click event",
            ));
        }
        let binding = UaiBrowserSessionBinding::try_new(
            plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )?;
        let next_sequence = command.sequence.checked_add(1).ok_or_else(invalid_cursor)?;
        let next_command = UaiBrowserCommandEnvelope::scan_page(
            plan,
            &binding,
            next_sequence,
            UaiBrowserPageScope::Tab,
        )?;
        let mut next = self.clone();
        next.stage = UaiBrowserCursorStage::ScanningTabs;
        next.prior_result_sequence = Some(command.sequence);
        next.prior_result_digest = Some(result_digest);
        next.next_command_digest = next_command.exchange_digest(plan)?;
        next.validate_for_command(batch, plan, &next_command)?;
        Ok(UaiBrowserCursorAdvance {
            cursor: next,
            command: next_command,
        })
    }

    /// Freezes one bounded Tab snapshot and selects its first traversal action.
    ///
    /// A page without Tabs proceeds directly to Task enumeration. Otherwise
    /// the first ordered snapshot handle is the only authorized click.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless this cursor owns the exact Tab scan and
    /// its completed event contains a validated ordered Tab snapshot.
    pub fn advance_tab_list(
        &self,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        completed: &UaiBrowserEventExchangeCompleted,
    ) -> ProviderResult<UaiBrowserCursorAdvance> {
        self.validate_for_command(batch, plan, command)?;
        if self.stage != UaiBrowserCursorStage::ScanningTabs
            || !matches!(
                &command.command,
                UaiBrowserCommand::ScanPage {
                    scope: UaiBrowserPageScope::Tab
                }
            )
        {
            return Err(stale_cursor());
        }
        let result_digest = completed_event_digest(self, command, completed)?;
        completed
            .event()
            .validate_for_command(plan, command, &completed.event().origin)?;
        let UaiBrowserEvent::PageList {
            scope: UaiBrowserPageScope::Tab,
            entries,
        } = &completed.event().event
        else {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI accumulated cursor requires a completed Tab-list event",
            ));
        };
        let binding = UaiBrowserSessionBinding::try_new(
            plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )?;
        let next_sequence = command.sequence.checked_add(1).ok_or_else(invalid_cursor)?;
        let (stage, current_tab_ordinal, next_tab_ordinal, next_command) =
            if let Some(first) = entries.first() {
                (
                    UaiBrowserCursorStage::ClickingTab,
                    Some(first.ordinal),
                    first.ordinal.checked_add(1).ok_or_else(invalid_cursor)?,
                    UaiBrowserCommandEnvelope::click_tab(plan, &binding, next_sequence, first)?,
                )
            } else {
                (
                    UaiBrowserCursorStage::ScanningTasks,
                    None,
                    0,
                    UaiBrowserCommandEnvelope::scan_page(
                        plan,
                        &binding,
                        next_sequence,
                        UaiBrowserPageScope::Task,
                    )?,
                )
            };
        let mut next = self.clone();
        next.stage = stage;
        next.tab_snapshot.clone_from(entries);
        next.next_tab_ordinal = next_tab_ordinal;
        next.current_tab_ordinal = current_tab_ordinal;
        next.task_snapshot.clear();
        next.prior_result_sequence = Some(command.sequence);
        next.prior_result_digest = Some(result_digest);
        next.next_command_digest = next_command.exchange_digest(plan)?;
        next.validate_for_command(batch, plan, &next_command)?;
        Ok(UaiBrowserCursorAdvance {
            cursor: next,
            command: next_command,
        })
    }

    /// Encodes this validated accumulated cursor for encrypted persistence.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the cursor is foreign to the immutable
    /// Course/Task plans or next command, cannot be serialized or exceeds its
    /// bounded artifact size.
    pub fn encode_artifact(
        &self,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
    ) -> ProviderResult<EncodedUaiBrowserCursorArtifact> {
        self.validate_for_command(batch, plan, command)?;
        let mut encoded = Zeroizing::new(serde_json::to_vec(self).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI accumulated browser cursor cannot be encoded",
            )
        })?);
        if encoded.is_empty() || encoded.len() > MAX_BROWSER_CURSOR_ARTIFACT_BYTES {
            return Err(invalid_cursor_artifact());
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedUaiBrowserCursorArtifact { value, digest })
    }

    /// Restores one exact cursor from Core's encrypted artifact repository.
    ///
    /// The digest authenticates the Provider-private bytes while the fresh
    /// Course batch, Task plan and next command independently rebind every
    /// execution authority needed after process recovery.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest/schema/size drift or any stale batch,
    /// Task, command, sequence, snapshot or result binding.
    pub fn decode_artifact_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_BROWSER_CURSOR_ARTIFACT_BYTES {
            return Err(invalid_cursor_artifact());
        }
        let actual_digest: [u8; 32] = Sha256::digest(bytes).into();
        if actual_digest != expected_digest {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI accumulated browser cursor artifact digest changed",
            ));
        }
        let cursor: Self = serde_json::from_slice(bytes).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI accumulated browser cursor artifact schema changed",
            )
        })?;
        cursor.validate_for_command(batch, plan, command)?;
        Ok(cursor)
    }

    /// Validates one accumulated cursor against both immutable plans and the
    /// exact command it accompanies.
    ///
    /// # Errors
    ///
    /// Returns a typed error for foreign batch/Task identity, stale snapshots,
    /// missing prior-result authority, count/budget overflow or a stage that
    /// does not exactly match the command.
    pub fn validate_for_command(
        &self,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
    ) -> ProviderResult<()> {
        batch.validate()?;
        plan.validate()?;
        command.validate_for_plan(plan)?;
        let fresh_browser_plan_digest = browser_plan_digest(plan)?;
        let fresh_command_digest = command.exchange_digest(plan)?;
        let micro = batch
            .micros()
            .get(self.current_micro_ordinal as usize)
            .ok_or_else(invalid_cursor)?;
        if self.version != UAI_BROWSER_CURSOR_VERSION
            || self.course_remote_id != batch.course_remote_id()
            || self.batch_membership_digest != batch.membership_digest()
            || self.batch_plan_digest != batch.plan_digest()
            || self.browser_plan_digest != fresh_browser_plan_digest
            || self.next_command_digest != fresh_command_digest
            || self.current_micro_ordinal < batch.start().ordinal()
            || self.current_micro_identity_digest != micro.identity_digest()
            || micro.unit_title() != plan.target.unit
            || micro.section_title() != plan.target.section.as_deref()
            || micro.micro_title() != plan.target.micro
            || !micro.tasks().iter().any(|task| {
                task.remote_task_id() == plan.target_remote_task_id
                    && task.title() == plan.target.task
            })
            || command.remote_task_id != plan.target_remote_task_id
            || self.remaining_active_seconds > batch.total_residence_seconds()
            || self.observed_video_seconds > plan.max_video_seconds
        {
            return Err(stale_cursor());
        }
        validate_prior_result(self, command.sequence)?;
        let binding = UaiBrowserSessionBinding::try_new(
            plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )?;
        validate_snapshot(plan, &binding, &self.tab_snapshot, UaiBrowserPageScope::Tab)?;
        validate_snapshot(
            plan,
            &binding,
            &self.task_snapshot,
            UaiBrowserPageScope::Task,
        )?;
        let tab_count = u32::try_from(self.tab_snapshot.len()).map_err(|_| invalid_cursor())?;
        let micro_count = u32::try_from(batch.micros().len()).map_err(|_| invalid_cursor())?;
        if self.next_tab_ordinal > tab_count
            || self
                .current_tab_ordinal
                .is_some_and(|ordinal| ordinal >= tab_count)
            || self.processed_micros > self.current_micro_ordinal - batch.start().ordinal()
            || self.processed_tabs > plan.max_tabs_per_micro.saturating_mul(micro_count)
            || self.processed_tasks > self.processed_tabs.saturating_mul(plan.max_tasks_per_tab)
        {
            return Err(invalid_cursor());
        }
        validate_stage(self, plan, command)
    }
}

impl fmt::Debug for UaiBrowserResidenceCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserResidenceCursor")
            .field("version", &self.version)
            .field("course_remote_id", &self.course_remote_id)
            .field("batch_membership_digest", &self.batch_membership_digest)
            .field("batch_plan_digest", &self.batch_plan_digest)
            .field("browser_plan_digest", &self.browser_plan_digest)
            .field("next_command_digest", &self.next_command_digest)
            .field("current_micro_ordinal", &self.current_micro_ordinal)
            .field(
                "current_micro_identity_digest",
                &self.current_micro_identity_digest,
            )
            .field("stage", &self.stage)
            .field("tab_count", &self.tab_snapshot.len())
            .field("next_tab_ordinal", &self.next_tab_ordinal)
            .field("current_tab_ordinal", &self.current_tab_ordinal)
            .field("task_count", &self.task_snapshot.len())
            .field("processed_micros", &self.processed_micros)
            .field("processed_tabs", &self.processed_tabs)
            .field("processed_tasks", &self.processed_tasks)
            .field("remaining_active_seconds", &self.remaining_active_seconds)
            .field("observed_video_seconds", &self.observed_video_seconds)
            .field("paused", &self.paused)
            .field("prior_result_sequence", &self.prior_result_sequence)
            .field("prior_result_digest", &self.prior_result_digest)
            .field("snapshot_contents", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiBrowserResidenceCursor {
    fn drop(&mut self) {
        self.course_remote_id.zeroize();
        zeroize_snapshot(&mut self.tab_snapshot);
        zeroize_snapshot(&mut self.task_snapshot);
    }
}

fn validate_prior_result(
    cursor: &UaiBrowserResidenceCursor,
    command_sequence: u32,
) -> ProviderResult<()> {
    match (cursor.prior_result_sequence, cursor.prior_result_digest) {
        (None, None)
            if command_sequence == 1 && cursor.stage == UaiBrowserCursorStage::ScanningMenu =>
        {
            Ok(())
        }
        (Some(sequence), Some(digest))
            if digest != [0; 32]
                && sequence.checked_add(1) == Some(command_sequence)
                && command_sequence > 1 =>
        {
            Ok(())
        }
        _ => Err(stale_cursor()),
    }
}

fn completed_event_digest(
    cursor: &UaiBrowserResidenceCursor,
    command: &UaiBrowserCommandEnvelope,
    completed: &UaiBrowserEventExchangeCompleted,
) -> ProviderResult<[u8; 32]> {
    let exchange = completed.exchange();
    let result_digest = exchange.result_digest.ok_or_else(stale_cursor)?;
    if exchange.validate().is_err()
        || exchange.state != BrowserBridgeExchangeState::Completed
        || exchange.session_id.to_string() != command.session_nonce
        || exchange.sequence != u64::from(command.sequence)
        || exchange.command_type != UaiBrowserCommandEnvelope::exchange_type()
        || exchange.command_digest != cursor.next_command_digest
        || exchange.result_type.as_deref() != Some(UaiBrowserEventEnvelope::exchange_type())
        || result_digest == [0; 32]
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI accumulated cursor event exchange is stale or foreign",
        ));
    }
    Ok(result_digest)
}

fn validate_snapshot(
    plan: &UaiBrowserResidencePlan,
    binding: &UaiBrowserSessionBinding,
    entries: &[UaiBrowserPageEntry],
    scope: UaiBrowserPageScope,
) -> ProviderResult<()> {
    for (ordinal, entry) in entries.iter().enumerate() {
        if entry.scope != scope || entry.ordinal as usize != ordinal {
            return Err(invalid_cursor());
        }
        entry.validate_for_binding(plan, binding)?;
    }
    Ok(())
}

fn validate_stage(
    cursor: &UaiBrowserResidenceCursor,
    plan: &UaiBrowserResidencePlan,
    command: &UaiBrowserCommandEnvelope,
) -> ProviderResult<()> {
    let valid = match (cursor.stage, &command.command) {
        (UaiBrowserCursorStage::ScanningMenu, UaiBrowserCommand::ScanMenu)
        | (UaiBrowserCursorStage::ClickingMenu, UaiBrowserCommand::ClickMenu { .. }) => {
            cursor.tab_snapshot.is_empty()
                && cursor.task_snapshot.is_empty()
                && cursor.current_tab_ordinal.is_none()
                && cursor.next_tab_ordinal == 0
        }
        (
            UaiBrowserCursorStage::ScanningTabs,
            UaiBrowserCommand::ScanPage {
                scope: UaiBrowserPageScope::Tab,
            },
        ) => cursor.tab_snapshot.is_empty() && cursor.task_snapshot.is_empty(),
        (UaiBrowserCursorStage::ClickingTab, UaiBrowserCommand::ClickTab { handle }) => cursor
            .current_tab_ordinal
            .and_then(|ordinal| cursor.tab_snapshot.get(ordinal as usize))
            .is_some_and(|entry| {
                entry.handle == *handle && cursor.next_tab_ordinal == entry.ordinal + 1
            }),
        (
            UaiBrowserCursorStage::ScanningTasks,
            UaiBrowserCommand::ScanPage {
                scope: UaiBrowserPageScope::Task,
            },
        ) => cursor.task_snapshot.is_empty(),
        (UaiBrowserCursorStage::ClickingTask, UaiBrowserCommand::ClickTask { handle }) => cursor
            .task_snapshot
            .iter()
            .filter(|entry| entry.label == plan.target.task)
            .exactly_one()
            .is_some_and(|entry| entry.handle == *handle),
        (
            UaiBrowserCursorStage::Residing,
            UaiBrowserCommand::ResidenceTarget {
                task_handle,
                seconds,
                play_video,
            },
        ) => {
            *seconds == plan.residence_seconds
                && *play_video == plan.play_video
                && cursor
                    .task_snapshot
                    .iter()
                    .filter(|entry| entry.label == plan.target.task)
                    .exactly_one()
                    .is_some_and(|entry| entry.handle == *task_handle)
                && !cursor.paused
        }
        (
            UaiBrowserCursorStage::ControllingResidence,
            UaiBrowserCommand::ResidenceControl {
                task_handle,
                seconds,
                control,
            },
        ) => {
            *seconds == plan.residence_seconds
                && cursor
                    .task_snapshot
                    .iter()
                    .filter(|entry| entry.label == plan.target.task)
                    .exactly_one()
                    .is_some_and(|entry| entry.handle == *task_handle)
                && match control {
                    UaiBrowserResidenceControl::Pause => !cursor.paused,
                    UaiBrowserResidenceControl::Resume => cursor.paused,
                    UaiBrowserResidenceControl::Restart {
                        start_micro_ordinal,
                    } => *start_micro_ordinal == cursor.current_micro_ordinal,
                }
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI accumulated browser cursor does not match its next command",
        ))
    }
}

trait ExactlyOne<'a, T: 'a>: Iterator<Item = &'a T> + Sized {
    fn exactly_one(mut self) -> Option<&'a T> {
        let first = self.next()?;
        self.next().is_none().then_some(first)
    }
}

impl<'a, T: 'a, I> ExactlyOne<'a, T> for I where I: Iterator<Item = &'a T> {}

fn zeroize_snapshot(entries: &mut Vec<UaiBrowserPageEntry>) {
    for entry in entries.iter_mut() {
        entry.handle.zeroize();
        entry.label.zeroize();
    }
    entries.clear();
}

fn browser_plan_digest(plan: &UaiBrowserResidencePlan) -> ProviderResult<[u8; 32]> {
    plan.validate()?;
    let encoded = Zeroizing::new(serde_json::to_vec(plan).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge residence plan cannot be encoded",
        )
    })?);
    Ok(Sha256::digest(encoded.as_slice()).into())
}

fn invalid_cursor() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI accumulated browser cursor is invalid or unbounded",
    )
}

fn stale_cursor() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "UAI accumulated browser cursor is stale or foreign",
    )
}

fn invalid_cursor_artifact() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI accumulated browser cursor artifact is empty or oversized",
    )
}
