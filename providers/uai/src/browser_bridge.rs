use std::{fmt, sync::Arc};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    BrowserBridgeCapability, BrowserSessionSpec, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderResult, RemoteTaskDetail, TaskDetailCapability,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{
    metadata::development_metadata,
    runtime_settings::{BROWSER_PLAY_VIDEO_KEY, BROWSER_RESIDENCE_SECONDS_KEY},
};

const UCONTENT_ORIGIN: &str = "https://ucontent.unipus.cn";
const IPUB_ORIGIN: &str = "https://ipub.unipus.cn";
const EXPLORATION_PC_PATH: &str = "/_explorationpc_default/pc.html";
const MAX_DISCOVERED_MICROS: u32 = 2_048;
const MAX_TABS_PER_MICRO: u32 = 64;
const MAX_TASKS_PER_TAB: u32 = 128;
const MAX_POPUP_CLICKS_PER_STAGE: u32 = 16;
const MAX_DOM_POLL_MILLIS: u64 = 3_000;
const MAX_VIDEO_SECONDS: u64 = 30 * 60;
const MAX_BROWSER_RESULT_BYTES: usize = 64 * 1_024;
const MAX_BROWSER_RESULT_LABEL_BYTES: usize = 512;
const MAX_BROWSER_MESSAGE_BYTES: usize = 1_024 * 1_024;
const MAX_BROWSER_BINDING_BYTES: usize = 256;
const MAX_BROWSER_MENU_LABEL_BYTES: usize = 512;
const BROWSER_MENU_HANDLE_PREFIX: &str = "uai-menu-v1-";
const BROWSER_PAGE_HANDLE_PREFIX: &str = "uai-page-v1-";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UaiMenuDiscoveryStrategy {
    LegacySlider,
    AntTree,
    AriaMenu,
    U3Menu,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UaiBrowserResidencePlan {
    pub version: u32,
    pub target_remote_task_id: String,
    pub start_url: String,
    pub target: UaiBrowserTarget,
    pub allowed_origins: Vec<String>,
    pub discovery_strategies: Vec<UaiMenuDiscoveryStrategy>,
    pub tab_selectors: Vec<String>,
    pub task_selectors: Vec<String>,
    pub popup_selectors: Vec<String>,
    pub video_selectors: Vec<String>,
    pub residence_seconds: u64,
    pub play_video: bool,
    pub max_discovered_micros: u32,
    pub max_tabs_per_micro: u32,
    pub max_tasks_per_tab: u32,
    pub max_popup_clicks_per_stage: u32,
    pub dom_poll_millis: u64,
    pub max_video_seconds: u64,
    pub message_security: UaiBrowserMessageSecurity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserTarget {
    pub unit: String,
    pub section: Option<String>,
    pub micro: String,
    pub task: String,
}

impl UaiBrowserTarget {
    fn from_detail(detail: &RemoteTaskDetail) -> ProviderResult<Self> {
        let task = detail
            .normalized_detail
            .get("task")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "UAI BrowserBridge fresh detail has no normalized Task",
                )
            })?;
        let unit = nested_title(task, "unit")?;
        let section = optional_nested_title(task, "section")?;
        let micro =
            optional_nested_title(task, "micro")?.unwrap_or_else(|| detail.task.title.clone());
        let target = Self {
            unit,
            section,
            micro,
            task: detail.task.title.clone(),
        };
        target.validate()?;
        Ok(target)
    }

    fn validate(&self) -> ProviderResult<()> {
        if is_browser_label(&self.unit, false)
            && self
                .section
                .as_ref()
                .is_none_or(|value| is_browser_label(value, false))
            && is_browser_label(&self.micro, false)
            && is_browser_label(&self.task, false)
        {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge target labels are invalid or unbounded",
            ))
        }
    }

    fn matches_menu_entry(&self, entry: &UaiBrowserMenuEntry) -> bool {
        self.unit == entry.unit
            && self.section.as_deref().unwrap_or_default() == entry.section
            && self.micro == entry.micro
    }
}

/// Constructs the donor-observed rendered Course entry from a freshly
/// rebound Group detail. The URL carries only the public Course-resource
/// route identity; browser credentials remain in the isolated session.
///
/// The exact Group is selected after navigation through the plan's freshly
/// bound Unit/Section/Micro/Task hierarchy. Optional client/theme/tutorial
/// parameters are deliberately omitted because the audited page accepts the
/// Course-resource `cid` as its minimal stable route.
///
/// # Errors
///
/// Returns a typed drift error when the fresh detail does not bind one exact
/// Course-resource identity, or an internal error if the static route cannot
/// be constructed safely.
pub fn browser_start_url_from_detail(detail: &RemoteTaskDetail) -> ProviderResult<String> {
    let task = detail
        .normalized_detail
        .get("task")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI BrowserBridge fresh detail has no normalized Task route",
            )
        })?;
    let course_resource_id = task
        .get("course_resource_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| is_route_component(value))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI BrowserBridge fresh detail has no valid Course-resource route",
            )
        })?;
    let expected_course_remote_id = format!("course-resource:{course_resource_id}");
    if detail.task.course_remote_id.as_deref() != Some(expected_course_remote_id.as_str())
        || !detail
            .task
            .remote_id
            .starts_with(&format!("group:{course_resource_id}:"))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI BrowserBridge Course-resource route is foreign to its fresh Group",
        ));
    }

    let mut url =
        reqwest::Url::parse(&format!("{UCONTENT_ORIGIN}{EXPLORATION_PC_PATH}")).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI BrowserBridge static start route is invalid",
            )
        })?;
    url.query_pairs_mut().append_pair("cid", course_resource_id);
    let start_url = url.to_string();
    validate_start_url(&start_url)?;
    Ok(start_url)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UaiBrowserMessageSecurity {
    SessionNonceFrameAndExactOrigin,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserResidenceResult {
    pub version: u32,
    pub session_nonce: String,
    pub origin: String,
    pub frame_id: String,
    pub remote_task_id: String,
    pub reply_to_sequence: u32,
    pub target_task_handle: String,
    pub planned_residence_seconds: u64,
    pub observed_active_seconds: u64,
    pub processed_micros: u32,
    pub processed_tabs: u32,
    pub processed_tasks: u32,
    pub video_seconds: u64,
    pub cancelled: bool,
    pub last_label: Option<String>,
}

impl fmt::Debug for UaiBrowserResidenceResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserResidenceResult")
            .field("version", &self.version)
            .field("session_nonce", &"redacted")
            .field("origin", &self.origin)
            .field("frame_id", &self.frame_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("reply_to_sequence", &self.reply_to_sequence)
            .field("target_task_handle", &self.target_task_handle)
            .field("planned_residence_seconds", &self.planned_residence_seconds)
            .field("observed_active_seconds", &self.observed_active_seconds)
            .field("processed_micros", &self.processed_micros)
            .field("processed_tabs", &self.processed_tabs)
            .field("processed_tasks", &self.processed_tasks)
            .field("video_seconds", &self.video_seconds)
            .field("cancelled", &self.cancelled)
            .field("last_label", &self.last_label)
            .finish()
    }
}

impl Drop for UaiBrowserResidenceResult {
    fn drop(&mut self) {
        self.session_nonce.zeroize();
    }
}

impl UaiBrowserResidenceResult {
    /// Validates one browser-side receipt against the frozen session and plan.
    /// It remains an observation only; fresh `DurationRead` is the acceptance
    /// authority.
    ///
    /// # Errors
    ///
    /// Returns a typed invalid-response or remote-changed error when any
    /// session, frame, origin, task, count or timing fact is unsafe or foreign.
    pub fn validate_for_command(
        &self,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<()> {
        plan.validate()?;
        command.validate_for_plan(plan)?;
        let UaiBrowserCommand::ResidenceTarget {
            task_handle,
            seconds,
            play_video,
        } = &command.command
        else {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge result requires a target residence command",
            ));
        };
        if self.version != plan.version
            || self.session_nonce != command.session_nonce
            || self.frame_id != command.frame_id
            || self.remote_task_id != plan.target_remote_task_id
            || self.reply_to_sequence != command.sequence
            || self.target_task_handle != *task_handle
            || self.origin != observed_origin
            || self.origin != command.origin
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge result is not bound to the frozen session and Task",
            ));
        }
        if validate_browser_binding(
            plan,
            &command.session_nonce,
            &command.frame_id,
            observed_origin,
        )
        .is_err()
            || self.observed_active_seconds > *seconds
            || self.planned_residence_seconds != *seconds
            || self.processed_micros > 1
            || self.processed_tabs > plan.max_tabs_per_micro
            || self.processed_tasks > self.processed_tabs
            || self.video_seconds > plan.max_video_seconds
            || (!play_video && self.video_seconds != 0)
            || self.last_label.as_ref().is_some_and(|label| {
                label.is_empty()
                    || label.len() > MAX_BROWSER_RESULT_LABEL_BYTES
                    || label.chars().any(char::is_control)
            })
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge result is invalid or exceeds the frozen plan",
            ));
        }
        Ok(())
    }

    pub const fn requires_fresh_duration_read(&self) -> bool {
        true
    }
}

#[derive(Eq, PartialEq)]
pub struct UaiBrowserSessionBinding {
    version: u32,
    session_nonce: String,
    origin: String,
    frame_id: String,
    remote_task_id: String,
}

impl UaiBrowserSessionBinding {
    /// Freezes one validated browser session/frame/origin/Task binding.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the binding is empty, unbounded or outside
    /// the exact plan origin and Task constraints.
    pub fn try_new(
        plan: &UaiBrowserResidencePlan,
        session_nonce: &str,
        origin: &str,
        frame_id: &str,
    ) -> ProviderResult<Self> {
        validate_browser_binding(plan, session_nonce, frame_id, origin)?;
        Ok(Self {
            version: plan.version,
            session_nonce: session_nonce.to_owned(),
            origin: origin.to_owned(),
            frame_id: frame_id.to_owned(),
            remote_task_id: plan.target_remote_task_id.clone(),
        })
    }

    fn validate_for_plan(&self, plan: &UaiBrowserResidencePlan) -> ProviderResult<()> {
        validate_browser_binding(plan, &self.session_nonce, &self.frame_id, &self.origin)?;
        if self.version == plan.version && self.remote_task_id == plan.target_remote_task_id {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge session binding is foreign to the frozen plan",
            ))
        }
    }
}

impl fmt::Debug for UaiBrowserSessionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserSessionBinding")
            .field("version", &self.version)
            .field("session_nonce", &"redacted")
            .field("origin", &self.origin)
            .field("frame_id", &self.frame_id)
            .field("remote_task_id", &self.remote_task_id)
            .finish()
    }
}

impl Drop for UaiBrowserSessionBinding {
    fn drop(&mut self) {
        self.session_nonce.zeroize();
    }
}

/// Parses one bounded browser-side result document and binds it to the frozen
/// session/plan. This does not verify remote duration.
///
/// # Errors
///
/// Returns a typed error for oversized/malformed JSON or any failed binding.
pub fn parse_browser_residence_result(
    document: &str,
    plan: &UaiBrowserResidencePlan,
    command: &UaiBrowserCommandEnvelope,
    observed_origin: &str,
) -> ProviderResult<UaiBrowserResidenceResult> {
    if document.is_empty() || document.len() > MAX_BROWSER_RESULT_BYTES {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge result is empty or oversized",
        ));
    }
    let result = serde_json::from_str::<UaiBrowserResidenceResult>(document).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge result is not valid JSON",
        )
    })?;
    result.validate_for_command(plan, command, observed_origin)?;
    Ok(result)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserMenuEntry {
    pub ordinal: u32,
    pub handle: String,
    pub unit: String,
    pub section: String,
    pub micro: String,
}

impl UaiBrowserMenuEntry {
    /// Creates one opaque, session-bound menu handle from a browser discovery
    /// result. The handle never contains a CSS selector or DOM path.
    ///
    /// # Errors
    ///
    /// Returns a typed invalid-response error when the browser binding,
    /// ordinal or donor-normalized labels are unsafe.
    pub fn try_new(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        ordinal: u32,
        unit: String,
        section: String,
        micro: String,
    ) -> ProviderResult<Self> {
        binding.validate_for_plan(plan)?;
        if ordinal >= plan.max_discovered_micros
            || !is_browser_label(&unit, true)
            || !is_browser_label(&section, true)
            || !is_browser_label(&micro, false)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge menu entry is invalid or unbounded",
            ));
        }
        let handle = browser_menu_handle(binding, ordinal, &unit, &section, &micro);
        Ok(Self {
            ordinal,
            handle,
            unit,
            section,
            micro,
        })
    }

    fn validate_for_binding(
        &self,
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
    ) -> ProviderResult<()> {
        let expected = Self::try_new(
            plan,
            binding,
            self.ordinal,
            self.unit.clone(),
            self.section.clone(),
            self.micro.clone(),
        )?;
        if self.handle == expected.handle {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge menu handle is not bound to its discovery snapshot",
            ))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiBrowserTargetMenuEntry(UaiBrowserMenuEntry);

impl UaiBrowserTargetMenuEntry {
    pub fn entry(&self) -> &UaiBrowserMenuEntry {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UaiBrowserPageScope {
    Tab,
    Task,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserPageEntry {
    pub scope: UaiBrowserPageScope,
    pub ordinal: u32,
    pub handle: String,
    pub label: String,
    pub active: bool,
}

impl UaiBrowserPageEntry {
    /// Creates one bounded, opaque page-item handle for a known Tab or Task.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an invalid binding, ordinal or normalized
    /// label.
    pub fn try_new(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        scope: UaiBrowserPageScope,
        ordinal: u32,
        label: String,
        active: bool,
    ) -> ProviderResult<Self> {
        binding.validate_for_plan(plan)?;
        let limit = match scope {
            UaiBrowserPageScope::Tab => plan.max_tabs_per_micro,
            UaiBrowserPageScope::Task => plan.max_tasks_per_tab,
        };
        if ordinal >= limit || !is_browser_label(&label, false) {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge page entry is invalid or unbounded",
            ));
        }
        let handle = browser_page_handle(binding, scope, ordinal, &label);
        Ok(Self {
            scope,
            ordinal,
            handle,
            label,
            active,
        })
    }

    fn validate_for_binding(
        &self,
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
    ) -> ProviderResult<()> {
        let expected = Self::try_new(
            plan,
            binding,
            self.scope,
            self.ordinal,
            self.label.clone(),
            self.active,
        )?;
        if self.handle == expected.handle {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge page handle is not bound to its snapshot",
            ))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UaiBrowserTargetTaskEntry(UaiBrowserPageEntry);

impl UaiBrowserTargetTaskEntry {
    pub fn entry(&self) -> &UaiBrowserPageEntry {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UaiBrowserCommand {
    ScanMenu,
    ClickMenu {
        handle: String,
    },
    ScanPage {
        scope: UaiBrowserPageScope,
    },
    ClickTab {
        handle: String,
    },
    ClickTask {
        handle: String,
    },
    ResidenceTarget {
        task_handle: String,
        seconds: u64,
        play_video: bool,
    },
    Ping,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserCommandEnvelope {
    pub version: u32,
    pub session_nonce: String,
    pub origin: String,
    pub frame_id: String,
    pub remote_task_id: String,
    pub sequence: u32,
    pub command: UaiBrowserCommand,
}

impl UaiBrowserCommandEnvelope {
    /// Builds the bounded equivalent of the donor's `SCAN` command.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the plan or browser binding is invalid.
    pub fn scan_menu(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
    ) -> ProviderResult<Self> {
        Self::new(plan, binding, sequence, UaiBrowserCommand::ScanMenu)
    }

    /// Builds the bounded equivalent of the donor's `PING` command.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the plan or browser binding is invalid.
    pub fn ping(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
    ) -> ProviderResult<Self> {
        Self::new(plan, binding, sequence, UaiBrowserCommand::Ping)
    }

    /// Builds the bounded equivalent of the donor's `CLICK` command from a
    /// previously validated opaque menu entry.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the entry is foreign to this exact browser
    /// session or any binding is invalid.
    pub fn click_menu(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        target: &UaiBrowserTargetMenuEntry,
    ) -> ProviderResult<Self> {
        let entry = target.entry();
        entry.validate_for_binding(plan, binding)?;
        if !plan.target.matches_menu_entry(entry) {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge refuses to click a menu entry outside the exact Task target",
            ));
        }
        Self::new(
            plan,
            binding,
            sequence,
            UaiBrowserCommand::ClickMenu {
                handle: entry.handle.clone(),
            },
        )
    }

    /// Requests a bounded enumeration of either the audited Tab or Task
    /// selector family.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the plan or browser binding is invalid.
    pub fn scan_page(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        scope: UaiBrowserPageScope,
    ) -> ProviderResult<Self> {
        Self::new(
            plan,
            binding,
            sequence,
            UaiBrowserCommand::ScanPage { scope },
        )
    }

    /// Authorizes a click on one validated Tab entry from the current page
    /// snapshot. Already-active Tabs are retained as no-op-capable actions.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign handle or a non-Tab entry.
    pub fn click_tab(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        entry: &UaiBrowserPageEntry,
    ) -> ProviderResult<Self> {
        entry.validate_for_binding(plan, binding)?;
        if entry.scope != UaiBrowserPageScope::Tab {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge Tab click requires a Tab snapshot entry",
            ));
        }
        Self::new(
            plan,
            binding,
            sequence,
            UaiBrowserCommand::ClickTab {
                handle: entry.handle.clone(),
            },
        )
    }

    /// Authorizes a click on the uniquely selected fresh Task-title match.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign or non-target Task entry.
    pub fn click_task(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        target: &UaiBrowserTargetTaskEntry,
    ) -> ProviderResult<Self> {
        let entry = target.entry();
        entry.validate_for_binding(plan, binding)?;
        if entry.scope != UaiBrowserPageScope::Task || entry.label != plan.target.task {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge Task click is outside the exact fresh Task target",
            ));
        }
        Self::new(
            plan,
            binding,
            sequence,
            UaiBrowserCommand::ClickTask {
                handle: entry.handle.clone(),
            },
        )
    }

    /// Builds the single final target-bound residence action. Popup dismissal
    /// and optional video playback remain constrained by the frozen plan.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign target or invalid browser binding.
    pub fn residence_target(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        target: &UaiBrowserTargetTaskEntry,
    ) -> ProviderResult<Self> {
        let entry = target.entry();
        entry.validate_for_binding(plan, binding)?;
        if entry.scope != UaiBrowserPageScope::Task || entry.label != plan.target.task {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge residence action is outside the exact fresh Task target",
            ));
        }
        Self::new(
            plan,
            binding,
            sequence,
            UaiBrowserCommand::ResidenceTarget {
                task_handle: entry.handle.clone(),
                seconds: plan.residence_seconds,
                play_video: plan.play_video,
            },
        )
    }

    fn new(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        command: UaiBrowserCommand,
    ) -> ProviderResult<Self> {
        binding.validate_for_plan(plan)?;
        if sequence == 0 {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge command sequence must be non-zero",
            ));
        }
        let envelope = Self {
            version: binding.version,
            session_nonce: binding.session_nonce.clone(),
            origin: binding.origin.clone(),
            frame_id: binding.frame_id.clone(),
            remote_task_id: binding.remote_task_id.clone(),
            sequence,
            command,
        };
        envelope.validate_for_plan(plan)?;
        Ok(envelope)
    }

    /// Revalidates a command before the browser adapter dispatches it.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign binding, zero sequence or a value
    /// that could smuggle a selector/path instead of an opaque handle.
    pub fn validate_for_plan(&self, plan: &UaiBrowserResidencePlan) -> ProviderResult<()> {
        validate_browser_binding(plan, &self.session_nonce, &self.frame_id, &self.origin)?;
        if self.version != plan.version
            || self.remote_task_id != plan.target_remote_task_id
            || self.sequence == 0
            || matches!(
                &self.command,
                UaiBrowserCommand::ClickMenu { handle } if !is_browser_menu_handle(handle)
            )
            || matches!(
                &self.command,
                UaiBrowserCommand::ClickTab { handle } | UaiBrowserCommand::ClickTask { handle }
                    if !is_browser_page_handle(handle)
            )
            || matches!(
                &self.command,
                UaiBrowserCommand::ResidenceTarget { task_handle, seconds, play_video }
                    if !is_browser_page_handle(task_handle)
                        || *seconds != plan.residence_seconds
                        || *play_video != plan.play_video
            )
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge command is invalid or not plan-bound",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for UaiBrowserCommandEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserCommandEnvelope")
            .field("version", &self.version)
            .field("session_nonce", &"redacted")
            .field("origin", &self.origin)
            .field("frame_id", &self.frame_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("sequence", &self.sequence)
            .field("command", &self.command)
            .finish()
    }
}

impl Drop for UaiBrowserCommandEnvelope {
    fn drop(&mut self) {
        self.session_nonce.zeroize();
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UaiBrowserEvent {
    MenuList {
        entries: Vec<UaiBrowserMenuEntry>,
    },
    PageList {
        scope: UaiBrowserPageScope,
        entries: Vec<UaiBrowserPageEntry>,
    },
    ClickResult {
        handle: String,
        clicked: bool,
    },
    Pong,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiBrowserEventEnvelope {
    pub version: u32,
    pub session_nonce: String,
    pub origin: String,
    pub frame_id: String,
    pub remote_task_id: String,
    pub reply_to_sequence: u32,
    pub event: UaiBrowserEvent,
}

impl UaiBrowserEventEnvelope {
    /// Validates one received event against its exact command and transport
    /// supplied origin. JSON cannot self-assert its own origin.
    ///
    /// # Errors
    ///
    /// Returns a typed error for foreign session/frame/origin/Task, mismatched
    /// command correlation, malformed menu entries or arbitrary click paths.
    pub fn validate_for_command(
        &self,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<()> {
        command.validate_for_plan(plan)?;
        validate_browser_binding(
            plan,
            &command.session_nonce,
            &command.frame_id,
            observed_origin,
        )?;
        if self.version != command.version
            || self.session_nonce != command.session_nonce
            || self.origin != command.origin
            || self.origin != observed_origin
            || self.frame_id != command.frame_id
            || self.remote_task_id != command.remote_task_id
            || self.reply_to_sequence != command.sequence
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge event is foreign to its command binding",
            ));
        }
        let binding = UaiBrowserSessionBinding::try_new(
            plan,
            &self.session_nonce,
            &self.origin,
            &self.frame_id,
        )?;

        match (&command.command, &self.event) {
            (UaiBrowserCommand::ScanMenu, UaiBrowserEvent::MenuList { entries }) => {
                if entries.len() > plan.max_discovered_micros as usize {
                    return Err(ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "UAI BrowserBridge menu result exceeds the frozen bound",
                    ));
                }
                for (expected_ordinal, entry) in entries.iter().enumerate() {
                    if entry.ordinal as usize != expected_ordinal {
                        return Err(ProviderError::new(
                            ProviderErrorKind::ProtocolDrift,
                            "UAI BrowserBridge menu ordinals are missing or reordered",
                        ));
                    }
                    entry.validate_for_binding(plan, &binding)?;
                }
            }
            (
                UaiBrowserCommand::ClickMenu { handle: expected },
                UaiBrowserEvent::ClickResult { handle, .. },
            ) if handle == expected && is_browser_menu_handle(handle) => {}
            (
                UaiBrowserCommand::ScanPage { scope: expected },
                UaiBrowserEvent::PageList { scope, entries },
            ) if scope == expected => {
                let limit = match scope {
                    UaiBrowserPageScope::Tab => plan.max_tabs_per_micro,
                    UaiBrowserPageScope::Task => plan.max_tasks_per_tab,
                };
                if entries.len() > limit as usize {
                    return Err(ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "UAI BrowserBridge page result exceeds the frozen bound",
                    ));
                }
                for (expected_ordinal, entry) in entries.iter().enumerate() {
                    if entry.scope != *scope || entry.ordinal as usize != expected_ordinal {
                        return Err(ProviderError::new(
                            ProviderErrorKind::ProtocolDrift,
                            "UAI BrowserBridge page entries are foreign, missing or reordered",
                        ));
                    }
                    entry.validate_for_binding(plan, &binding)?;
                }
            }
            (
                UaiBrowserCommand::ClickTab { handle: expected }
                | UaiBrowserCommand::ClickTask { handle: expected },
                UaiBrowserEvent::ClickResult { handle, .. },
            ) if handle == expected && is_browser_page_handle(handle) => {}
            (UaiBrowserCommand::Ping, UaiBrowserEvent::Pong) => {}
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "UAI BrowserBridge event does not match its command",
                ));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for UaiBrowserEventEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserEventEnvelope")
            .field("version", &self.version)
            .field("session_nonce", &"redacted")
            .field("origin", &self.origin)
            .field("frame_id", &self.frame_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("reply_to_sequence", &self.reply_to_sequence)
            .field("event", &self.event)
            .finish()
    }
}

impl Drop for UaiBrowserEventEnvelope {
    fn drop(&mut self) {
        self.session_nonce.zeroize();
    }
}

/// Parses one bounded `BrowserBridge` event and requires its transport-provided
/// origin to match the command and exact plan allowlist.
///
/// # Errors
///
/// Returns a typed error for oversized/malformed JSON or any command/session
/// correlation failure.
pub fn parse_browser_event(
    document: &str,
    plan: &UaiBrowserResidencePlan,
    command: &UaiBrowserCommandEnvelope,
    observed_origin: &str,
) -> ProviderResult<UaiBrowserEventEnvelope> {
    if document.is_empty() || document.len() > MAX_BROWSER_MESSAGE_BYTES {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge event is empty or oversized",
        ));
    }
    let event = serde_json::from_str::<UaiBrowserEventEnvelope>(document).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge event is not valid JSON",
        )
    })?;
    event.validate_for_command(plan, command, observed_origin)?;
    Ok(event)
}

fn validate_browser_binding(
    plan: &UaiBrowserResidencePlan,
    session_nonce: &str,
    frame_id: &str,
    origin: &str,
) -> ProviderResult<()> {
    plan.validate()?;
    let valid = is_browser_binding_value(session_nonce)
        && is_browser_binding_value(frame_id)
        && plan.allowed_origins.iter().any(|allowed| allowed == origin);
    if valid {
        Ok(())
    } else {
        Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge session/frame/origin binding is invalid",
        ))
    }
}

fn is_browser_binding_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BROWSER_BINDING_BYTES
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn is_browser_label(value: &str, allow_empty: bool) -> bool {
    if value.len() > MAX_BROWSER_MENU_LABEL_BYTES
        || value.chars().any(char::is_control)
        || (!allow_empty && value.is_empty())
    {
        return false;
    }
    value.split_whitespace().collect::<Vec<_>>().join(" ") == value
}

fn nested_title(
    task: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> ProviderResult<String> {
    optional_nested_title(task, key)?.ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI BrowserBridge normalized hierarchy is incomplete",
        )
    })
}

fn optional_nested_title(
    task: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> ProviderResult<Option<String>> {
    let Some(value) = task.get(key) else {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI BrowserBridge normalized hierarchy field disappeared",
        ));
    };
    if value.is_null() {
        return Ok(None);
    }
    let title = value
        .as_object()
        .and_then(|object| object.get("title"))
        .and_then(serde_json::Value::as_str)
        .filter(|title| is_browser_label(title, false))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI BrowserBridge normalized hierarchy title is invalid",
            )
        })?;
    Ok(Some(title.to_owned()))
}

fn browser_menu_handle(
    binding: &UaiBrowserSessionBinding,
    ordinal: u32,
    unit: &str,
    section: &str,
    micro: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:browser-menu-handle:v1\0");
    digest.update(binding.remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(binding.session_nonce.as_bytes());
    digest.update(b"\0");
    digest.update(binding.origin.as_bytes());
    digest.update(b"\0");
    digest.update(binding.frame_id.as_bytes());
    digest.update(b"\0");
    digest.update(ordinal.to_be_bytes());
    digest.update(b"\0");
    digest.update(unit.as_bytes());
    digest.update(b"\0");
    digest.update(section.as_bytes());
    digest.update(b"\0");
    digest.update(micro.as_bytes());
    format!("{BROWSER_MENU_HANDLE_PREFIX}{:x}", digest.finalize())
}

fn is_browser_menu_handle(handle: &str) -> bool {
    handle
        .strip_prefix(BROWSER_MENU_HANDLE_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn browser_page_handle(
    binding: &UaiBrowserSessionBinding,
    scope: UaiBrowserPageScope,
    ordinal: u32,
    label: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:browser-page-handle:v1\0");
    digest.update(binding.remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(binding.session_nonce.as_bytes());
    digest.update(b"\0");
    digest.update(binding.origin.as_bytes());
    digest.update(b"\0");
    digest.update(binding.frame_id.as_bytes());
    digest.update(b"\0");
    digest.update(match scope {
        UaiBrowserPageScope::Tab => b"tab".as_slice(),
        UaiBrowserPageScope::Task => b"task".as_slice(),
    });
    digest.update(b"\0");
    digest.update(ordinal.to_be_bytes());
    digest.update(b"\0");
    digest.update(label.as_bytes());
    format!("{BROWSER_PAGE_HANDLE_PREFIX}{:x}", digest.finalize())
}

fn is_browser_page_handle(handle: &str) -> bool {
    handle
        .strip_prefix(BROWSER_PAGE_HANDLE_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn is_route_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BROWSER_BINDING_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_start_url(value: &str) -> ProviderResult<()> {
    let url = reqwest::Url::parse(value).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge start route is not a valid URL",
        )
    })?;
    let query = url.query_pairs().collect::<Vec<_>>();
    let valid = url.scheme() == "https"
        && url.host_str() == Some("ucontent.unipus.cn")
        && url.port().is_none()
        && url.path() == EXPLORATION_PC_PATH
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && query.len() == 1
        && query[0].0 == "cid"
        && is_route_component(query[0].1.as_ref());
    if valid {
        Ok(())
    } else {
        Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge start route is not the bounded donor route",
        ))
    }
}

impl UaiBrowserResidencePlan {
    /// Validates one complete bounded donor-derived plan.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error if any bound, selector family,
    /// origin or message-security invariant is weakened.
    pub fn validate(&self) -> ProviderResult<()> {
        self.target.validate()?;
        validate_start_url(&self.start_url)?;
        let valid = self.version == 1
            && self.target_remote_task_id.starts_with("group:")
            && self.allowed_origins == [UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()]
            && self.discovery_strategies
                == [
                    UaiMenuDiscoveryStrategy::LegacySlider,
                    UaiMenuDiscoveryStrategy::AntTree,
                    UaiMenuDiscoveryStrategy::AriaMenu,
                    UaiMenuDiscoveryStrategy::U3Menu,
                ]
            && !self.tab_selectors.is_empty()
            && !self.task_selectors.is_empty()
            && !self.popup_selectors.is_empty()
            && !self.video_selectors.is_empty()
            && (60..=28_800).contains(&self.residence_seconds)
            && self.max_discovered_micros == MAX_DISCOVERED_MICROS
            && self.max_tabs_per_micro == MAX_TABS_PER_MICRO
            && self.max_tasks_per_tab == MAX_TASKS_PER_TAB
            && self.max_popup_clicks_per_stage == MAX_POPUP_CLICKS_PER_STAGE
            && self.dom_poll_millis == MAX_DOM_POLL_MILLIS
            && self.max_video_seconds == MAX_VIDEO_SECONDS
            && self.message_security == UaiBrowserMessageSecurity::SessionNonceFrameAndExactOrigin;
        if valid {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge residence plan is invalid or unbounded",
            ))
        }
    }

    /// Selects exactly one fresh menu entry matching this plan's normalized
    /// Unit/Section/Micro target and returns the only value authorized for a
    /// menu-click command.
    ///
    /// # Errors
    ///
    /// Returns a typed drift error when entries are malformed, the target has
    /// disappeared, or more than one entry matches the bound hierarchy.
    pub fn select_target_menu_entry(
        &self,
        binding: &UaiBrowserSessionBinding,
        entries: &[UaiBrowserMenuEntry],
    ) -> ProviderResult<UaiBrowserTargetMenuEntry> {
        binding.validate_for_plan(self)?;
        let mut selected = None;
        for (expected_ordinal, entry) in entries.iter().enumerate() {
            if entry.ordinal as usize != expected_ordinal {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "UAI BrowserBridge menu ordinals are missing or reordered",
                ));
            }
            entry.validate_for_binding(self, binding)?;
            if self.target.matches_menu_entry(entry) {
                if selected.is_some() {
                    return Err(ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "UAI BrowserBridge menu contains duplicate target hierarchy",
                    ));
                }
                selected = Some(UaiBrowserTargetMenuEntry(entry.clone()));
            }
        }
        selected.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge target hierarchy disappeared from the fresh menu",
            )
        })
    }

    /// Selects exactly one current-page Task entry matching the fresh Group
    /// title and returns the only value authorized for a Task click.
    ///
    /// # Errors
    ///
    /// Returns a typed drift error when entries are malformed or the exact
    /// Task title is missing or duplicated within the current Tab.
    pub fn select_target_task_entry(
        &self,
        binding: &UaiBrowserSessionBinding,
        entries: &[UaiBrowserPageEntry],
    ) -> ProviderResult<UaiBrowserTargetTaskEntry> {
        binding.validate_for_plan(self)?;
        let mut selected = None;
        for (expected_ordinal, entry) in entries.iter().enumerate() {
            if entry.scope != UaiBrowserPageScope::Task
                || entry.ordinal as usize != expected_ordinal
            {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "UAI BrowserBridge Task entries are foreign, missing or reordered",
                ));
            }
            entry.validate_for_binding(self, binding)?;
            if entry.label == self.target.task {
                if selected.is_some() {
                    return Err(ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "UAI BrowserBridge page contains duplicate target Task titles",
                    ));
                }
                selected = Some(UaiBrowserTargetTaskEntry(entry.clone()));
            }
        }
        selected.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge target Task disappeared from the current Tab",
            )
        })
    }
}

/// Builds the account-and-Task isolated browser boundary required by UAI's
/// page-residence and interaction workflow.
///
/// The shared `BrowserBridge` contract does not yet carry donor-specific DOM
/// actions or a duration budget. Those remain an Engine/Core integration gap;
/// this capability still performs fresh Task rediscovery before granting a
/// browser session and never exposes route or credential material.
pub struct UaiBrowserBridge {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
}

impl UaiBrowserBridge {
    /// Creates the UAI browser-session boundary around the same fresh detail
    /// reader used by native execution paths.
    ///
    /// # Errors
    ///
    /// Returns a sanitized Provider error if compile-time metadata is invalid.
    pub fn try_new(details: Arc<dyn TaskDetailCapability>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
        })
    }

    /// Produces the exact bounded Provider-private interaction plan from the
    /// immutable runtime-settings snapshot. Shared `BrowserBridge` execution
    /// must bind the plan to the session spec's isolation key and nonce.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the settings snapshot lacks the exact
    /// bounded residence/video values or the resulting plan is invalid.
    pub async fn residence_plan(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<UaiBrowserResidencePlan> {
        validate_context(context, &self.metadata)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::BrowserBridge)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI fresh Task does not authorize a BrowserBridge residence plan",
            ));
        }
        residence_plan_from_detail(&detail, settings)
    }
}

fn residence_plan_from_detail(
    detail: &RemoteTaskDetail,
    settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
) -> ProviderResult<UaiBrowserResidencePlan> {
    let residence_seconds = settings
        .duration_seconds(BROWSER_RESIDENCE_SECONDS_KEY)
        .filter(|seconds| (60..=28_800).contains(seconds))
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge has no valid frozen residence budget",
            )
        })?;
    let play_video = settings.boolean(BROWSER_PLAY_VIDEO_KEY).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "UAI BrowserBridge has no valid frozen video setting",
        )
    })?;
    let plan = UaiBrowserResidencePlan {
        version: 1,
        target_remote_task_id: detail.task.remote_id.clone(),
        start_url: browser_start_url_from_detail(detail)?,
        target: UaiBrowserTarget::from_detail(detail)?,
        allowed_origins: vec![UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()],
        discovery_strategies: vec![
            UaiMenuDiscoveryStrategy::LegacySlider,
            UaiMenuDiscoveryStrategy::AntTree,
            UaiMenuDiscoveryStrategy::AriaMenu,
            UaiMenuDiscoveryStrategy::U3Menu,
        ],
        tab_selectors: vec![
            ".pc-header-tabs-container .ant-col.tab .pc-tab-view-container".to_owned(),
            "#header ul.TabsBox a.topTab".to_owned(),
        ],
        task_selectors: vec![".pc-header-tasks-row .pc-task".to_owned()],
        popup_selectors: vec![
            ".know-box .iKnow".to_owned(),
            ".ant-modal-confirm-btns .ant-btn-primary.system-info-cloud-ok-button".to_owned(),
            ".ant-modal-confirm-btns .ant-btn.ant-btn-primary".to_owned(),
            ".ipublish-modal-footer-ok".to_owned(),
            "button.ant-btn.ant-btn-default.ipublish-modal-footer-ok".to_owned(),
        ],
        video_selectors: vec!["video.vjs-tech".to_owned(), "video".to_owned()],
        residence_seconds,
        play_video,
        max_discovered_micros: MAX_DISCOVERED_MICROS,
        max_tabs_per_micro: MAX_TABS_PER_MICRO,
        max_tasks_per_tab: MAX_TASKS_PER_TAB,
        max_popup_clicks_per_stage: MAX_POPUP_CLICKS_PER_STAGE,
        dom_poll_millis: MAX_DOM_POLL_MILLIS,
        max_video_seconds: MAX_VIDEO_SECONDS,
        message_security: UaiBrowserMessageSecurity::SessionNonceFrameAndExactOrigin,
    };
    plan.validate()?;
    Ok(plan)
}

impl fmt::Debug for UaiBrowserBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserBridge")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiBrowserBridge {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl BrowserBridgeCapability for UaiBrowserBridge {
    async fn browser_session_spec(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<BrowserSessionSpec> {
        validate_context(context, &self.metadata)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::BrowserBridge)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI fresh Task does not authorize a BrowserBridge session",
            ));
        }

        Ok(BrowserSessionSpec {
            version: 1,
            isolation_key: isolation_key(context, remote_task_id),
            allowed_origins: vec![UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()],
            // The audited donor depends on a real rendered page, DOM events,
            // iframe messaging and media state. Do not silently substitute a
            // headless-only execution mode before that boundary is verified.
            headless: false,
        })
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI BrowserBridge received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI BrowserBridge requires an authenticated session",
        ));
    }
    Ok(())
}

fn isolation_key(context: &ProviderContext, remote_task_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:browser-session:v1\0");
    digest.update(context.account_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(remote_task_id.as_bytes());
    format!("uai-task-{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        advertised: bool,
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            Ok(fixture_remote_detail(remote_task_id, self.advertised))
        }
    }

    fn fixture_remote_detail(remote_task_id: &str, advertised: bool) -> RemoteTaskDetail {
        let normalized = serde_json::json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": "unit-1", "title": "Unit 1"},
            "section": {"id": "section-1", "title": "Section 2"},
            "micro": {"id": "micro-1", "title": "Reading"},
            "group_id": "group-1",
            "task_types": ["rich-text-read"],
            "question_count": 1,
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: remote_task_id.to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Read the passage".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: advertised
                    .then_some(TaskCapability::BrowserBridge)
                    .into_iter()
                    .collect(),
                fingerprint: "v1:fixture".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: serde_json::json!({"schema": "uai.group-task.raw.v1"}),
            },
            normalized_detail: serde_json::json!({
                "schema": "uai.group-task-detail.v1",
                "task": normalized,
            }),
        }
    }

    #[tokio::test]
    async fn session_spec_is_fresh_task_bound_and_origin_bounded() {
        let detail = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised: true,
        });
        let bridge = UaiBrowserBridge::try_new(detail).unwrap();
        let context = provider_context();
        let first = bridge
            .browser_session_spec(&context, "group:2001:unit-1:group-1")
            .await
            .unwrap();
        let same = bridge
            .browser_session_spec(&context, "group:2001:unit-1:group-1")
            .await
            .unwrap();
        let other = bridge
            .browser_session_spec(&context, "group:2001:unit-1:group-2")
            .await
            .unwrap();

        assert_eq!(first, same);
        assert_ne!(first.isolation_key, other.isolation_key);
        assert_eq!(
            first.allowed_origins,
            [UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()]
        );
        assert!(!first.headless);
        assert!(
            !first
                .isolation_key
                .contains(&context.account_id.to_string())
        );
        assert!(!first.isolation_key.contains("group-1"));
    }

    #[tokio::test]
    async fn unadvertised_fresh_task_fails_closed() {
        let detail = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised: false,
        });
        let error = UaiBrowserBridge::try_new(detail)
            .unwrap()
            .browser_session_spec(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    #[tokio::test]
    async fn residence_plan_freezes_exact_donor_selectors_bounds_and_security_requirements() {
        let detail = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised: true,
        });
        let bridge = UaiBrowserBridge::try_new(detail).unwrap();
        let mut settings = crate::runtime_settings::runtime_settings_schema()
            .resolve(None, None, None)
            .unwrap();
        settings.values = BTreeMap::from([
            (
                BROWSER_RESIDENCE_SECONDS_KEY.to_owned(),
                asterism_provider_api::ProviderSettingValue::DurationSeconds(1_200),
            ),
            (
                BROWSER_PLAY_VIDEO_KEY.to_owned(),
                asterism_provider_api::ProviderSettingValue::Boolean(true),
            ),
            (
                crate::runtime_settings::PROVIDER_EXECUTION_CONCURRENCY_KEY.to_owned(),
                asterism_provider_api::ProviderSettingValue::Integer(1),
            ),
            (
                crate::runtime_settings::ACCOUNT_EXECUTION_CONCURRENCY_KEY.to_owned(),
                asterism_provider_api::ProviderSettingValue::Integer(1),
            ),
            (
                crate::runtime_settings::ACCOUNT_SCAN_INTERVAL_KEY.to_owned(),
                asterism_provider_api::ProviderSettingValue::DurationSeconds(1_800),
            ),
        ]);
        let plan = bridge
            .residence_plan(&provider_context(), "group:2001:unit-1:group-1", &settings)
            .await
            .unwrap();

        assert_eq!(plan.residence_seconds, 1_200);
        assert!(plan.play_video);
        assert_eq!(
            plan.start_url,
            "https://ucontent.unipus.cn/_explorationpc_default/pc.html?cid=2001"
        );
        assert!(!plan.start_url.contains("openid"));
        assert!(!plan.start_url.contains("token"));
        assert_eq!(plan.discovery_strategies.len(), 4);
        assert_eq!(plan.tab_selectors.len(), 2);
        assert_eq!(plan.max_video_seconds, 1_800);
        assert_eq!(plan.target.unit, "Unit 1");
        assert_eq!(plan.target.section.as_deref(), Some("Section 2"));
        assert_eq!(plan.target.micro, "Reading");
        assert_eq!(plan.target.task, "Read the passage");
        assert_eq!(
            plan.message_security,
            UaiBrowserMessageSecurity::SessionNonceFrameAndExactOrigin
        );
        assert!(plan.validate().is_ok());

        let mut unsafe_plan = plan;
        unsafe_plan.allowed_origins.pop();
        assert!(unsafe_plan.validate().is_err());
    }

    #[test]
    fn start_url_is_fresh_course_bound_https_and_secret_free() {
        let detail = fixture_remote_detail("group:2001:unit-1:group-1", true);
        let url = browser_start_url_from_detail(&detail).unwrap();
        assert_eq!(
            url,
            "https://ucontent.unipus.cn/_explorationpc_default/pc.html?cid=2001"
        );

        let mut foreign = detail;
        foreign.task.course_remote_id = Some("course-resource:2002".to_owned());
        assert!(browser_start_url_from_detail(&foreign).is_err());

        let unsafe_url = url.replace("cid=2001", "cid=2001&Authorization=secret");
        assert!(validate_start_url(&unsafe_url).is_err());
        assert!(
            validate_start_url("http://ucontent.unipus.cn/_explorationpc_default/pc.html?cid=2001")
                .is_err()
        );
    }

    #[test]
    fn browser_result_is_session_frame_origin_task_and_plan_bound() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", UCONTENT_ORIGIN, "top-frame")
                .unwrap();
        let task = UaiBrowserPageEntry::try_new(
            &plan,
            &binding,
            UaiBrowserPageScope::Task,
            0,
            "Read the passage".to_owned(),
            true,
        )
        .unwrap();
        let target = plan.select_target_task_entry(&binding, &[task]).unwrap();
        let command =
            UaiBrowserCommandEnvelope::residence_target(&plan, &binding, 9, &target).unwrap();
        let document = serde_json::json!({
            "version": 1,
            "session_nonce": "nonce-42",
            "origin": UCONTENT_ORIGIN,
            "frame_id": "top-frame",
            "remote_task_id": "group:2001:unit-1:group-1",
            "reply_to_sequence": 9,
            "target_task_handle": target.entry().handle.clone(),
            "planned_residence_seconds": 1_200,
            "observed_active_seconds": 1_200,
            "processed_micros": 1,
            "processed_tabs": 4,
            "processed_tasks": 4,
            "video_seconds": 0,
            "cancelled": false,
            "last_label": "Unit 1 / Section 2 / Micro 3",
        })
        .to_string();
        let result =
            parse_browser_residence_result(&document, &plan, &command, UCONTENT_ORIGIN).unwrap();
        assert!(result.requires_fresh_duration_read());
        assert!(parse_browser_residence_result(&document, &plan, &command, IPUB_ORIGIN,).is_err());
        let wrong_origin = document.replace(UCONTENT_ORIGIN, "https://evil.example");
        assert!(
            parse_browser_residence_result(&wrong_origin, &plan, &command, UCONTENT_ORIGIN,)
                .is_err()
        );
        let unexpected_video = document.replace("\"video_seconds\":0", "\"video_seconds\":1");
        assert!(
            parse_browser_residence_result(&unexpected_video, &plan, &command, UCONTENT_ORIGIN,)
                .is_err()
        );
    }

    #[test]
    fn bounded_messages_replace_donor_wildcard_and_arbitrary_selector_protocol() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", UCONTENT_ORIGIN, "content-frame")
                .unwrap();
        let scan = UaiBrowserCommandEnvelope::scan_menu(&plan, &binding, 1).unwrap();
        assert!(format!("{scan:?}").contains("redacted"));
        assert!(!format!("{scan:?}").contains("nonce-42"));

        let entry = UaiBrowserMenuEntry::try_new(
            &plan,
            &binding,
            0,
            "Unit 1".to_owned(),
            "Section 2".to_owned(),
            "Reading".to_owned(),
        )
        .unwrap();
        assert!(entry.handle.starts_with(BROWSER_MENU_HANDLE_PREFIX));
        assert!(!entry.handle.contains('#'));
        assert!(!entry.handle.contains('['));

        let menu_document = serde_json::json!({
            "version": 1,
            "session_nonce": "nonce-42",
            "origin": UCONTENT_ORIGIN,
            "frame_id": "content-frame",
            "remote_task_id": "group:2001:unit-1:group-1",
            "reply_to_sequence": 1,
            "event": {
                "kind": "menu_list",
                "entries": [entry],
            },
        })
        .to_string();
        let menu = parse_browser_event(&menu_document, &plan, &scan, UCONTENT_ORIGIN).unwrap();
        let UaiBrowserEvent::MenuList { ref entries } = menu.event else {
            panic!("expected menu list")
        };
        let target = plan.select_target_menu_entry(&binding, entries).unwrap();
        let click = UaiBrowserCommandEnvelope::click_menu(&plan, &binding, 2, &target).unwrap();
        assert!(matches!(click.command, UaiBrowserCommand::ClickMenu { .. }));

        let mut forged_entry = entries[0].clone();
        forged_entry.handle = "#arbitrary-selector".to_owned();
        assert!(
            plan.select_target_menu_entry(&binding, &[forged_entry])
                .is_err()
        );
    }

    #[test]
    fn browser_events_require_transport_origin_and_exact_command_correlation() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", IPUB_ORIGIN, "ipub-frame")
                .unwrap();
        let ping = UaiBrowserCommandEnvelope::ping(&plan, &binding, 7).unwrap();
        let pong_document = serde_json::json!({
            "version": 1,
            "session_nonce": "nonce-42",
            "origin": IPUB_ORIGIN,
            "frame_id": "ipub-frame",
            "remote_task_id": "group:2001:unit-1:group-1",
            "reply_to_sequence": 7,
            "event": { "kind": "pong" },
        })
        .to_string();
        assert!(parse_browser_event(&pong_document, &plan, &ping, IPUB_ORIGIN).is_ok());
        assert!(parse_browser_event(&pong_document, &plan, &ping, UCONTENT_ORIGIN).is_err());
        assert!(
            parse_browser_event(
                &pong_document.replace(":7", ":8"),
                &plan,
                &ping,
                IPUB_ORIGIN,
            )
            .is_err()
        );
    }

    #[test]
    fn target_menu_selection_is_unique_and_fresh_task_bound() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", UCONTENT_ORIGIN, "frame-1")
                .unwrap();
        let target = UaiBrowserMenuEntry::try_new(
            &plan,
            &binding,
            0,
            "Unit 1".to_owned(),
            "Section 2".to_owned(),
            "Reading".to_owned(),
        )
        .unwrap();
        assert!(
            plan.select_target_menu_entry(&binding, std::slice::from_ref(&target))
                .is_ok()
        );

        let foreign = UaiBrowserMenuEntry::try_new(
            &plan,
            &binding,
            0,
            "Unit 1".to_owned(),
            "Section 2".to_owned(),
            "Listening".to_owned(),
        )
        .unwrap();
        assert!(plan.select_target_menu_entry(&binding, &[foreign]).is_err());

        let duplicate = UaiBrowserMenuEntry::try_new(
            &plan,
            &binding,
            1,
            "Unit 1".to_owned(),
            "Section 2".to_owned(),
            "Reading".to_owned(),
        )
        .unwrap();
        assert!(
            plan.select_target_menu_entry(&binding, &[target, duplicate])
                .is_err()
        );
    }

    #[test]
    fn tab_and_task_actions_use_bounded_snapshot_handles_and_exact_task_title() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", UCONTENT_ORIGIN, "frame-1")
                .unwrap();
        let tabs = [
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Tab,
                0,
                "Learn".to_owned(),
                true,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Tab,
                1,
                "Exercise".to_owned(),
                false,
            )
            .unwrap(),
        ];
        let scan_tabs =
            UaiBrowserCommandEnvelope::scan_page(&plan, &binding, 3, UaiBrowserPageScope::Tab)
                .unwrap();
        let tab_document = serde_json::json!({
            "version": 1,
            "session_nonce": "nonce-42",
            "origin": UCONTENT_ORIGIN,
            "frame_id": "frame-1",
            "remote_task_id": "group:2001:unit-1:group-1",
            "reply_to_sequence": 3,
            "event": {"kind": "page_list", "scope": "tab", "entries": tabs},
        })
        .to_string();
        assert!(parse_browser_event(&tab_document, &plan, &scan_tabs, UCONTENT_ORIGIN).is_ok());
        assert!(UaiBrowserCommandEnvelope::click_tab(&plan, &binding, 4, &tabs[1]).is_ok());

        let tasks = [
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                0,
                "Warm up".to_owned(),
                false,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                1,
                "Read the passage".to_owned(),
                false,
            )
            .unwrap(),
        ];
        let target = plan.select_target_task_entry(&binding, &tasks).unwrap();
        assert!(UaiBrowserCommandEnvelope::click_task(&plan, &binding, 5, &target).is_ok());
        let residence =
            UaiBrowserCommandEnvelope::residence_target(&plan, &binding, 6, &target).unwrap();
        assert!(matches!(
            residence.command,
            UaiBrowserCommand::ResidenceTarget {
                seconds: 1_200,
                play_video: false,
                ..
            }
        ));

        let foreign_only = [tasks[0].clone()];
        assert!(
            plan.select_target_task_entry(&binding, &foreign_only)
                .is_err()
        );
    }

    fn residence_plan(play_video: bool) -> UaiBrowserResidencePlan {
        let schema = crate::runtime_settings::runtime_settings_schema();
        let patch = asterism_provider_api::ProviderRuntimeSettingsPatch {
            schema_version: schema.version,
            values: BTreeMap::from([
                (
                    BROWSER_RESIDENCE_SECONDS_KEY.to_owned(),
                    asterism_provider_api::ProviderSettingValue::DurationSeconds(1_200),
                ),
                (
                    BROWSER_PLAY_VIDEO_KEY.to_owned(),
                    asterism_provider_api::ProviderSettingValue::Boolean(play_video),
                ),
            ]),
        };
        let settings = schema.resolve(None, None, Some(&patch)).unwrap();
        residence_plan_from_detail(
            &fixture_remote_detail("group:2001:unit-1:group-1", true),
            &settings,
        )
        .unwrap()
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-browser-bridge-test".to_owned(),
        }
    }
}
