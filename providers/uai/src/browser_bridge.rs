use std::{fmt, sync::Arc};

use asterism_domain::{
    BrowserBridgeExchange, BrowserBridgeExchangeState, BrowserBridgeResultArtifactMetadata,
    BrowserBridgeRuntimeStateMetadata, BrowserBridgeSessionId, TaskCapability, Timestamp,
};
use asterism_provider_api::{
    BrowserBridgeCapability, BrowserSessionSpec, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderResult, RemoteTaskDetail, TaskDetailCapability,
};
use asterism_secrets::SecretValue;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UaiCourseResidenceBatchPlan,
    browser_cursor::{EncodedUaiBrowserCursorArtifact, UaiBrowserResidenceCursor},
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
const MAX_BROWSER_COMMAND_BYTES: usize = 64 * 1_024;
const MAX_BROWSER_BINDING_BYTES: usize = 256;
const MAX_BROWSER_MENU_LABEL_BYTES: usize = 512;
const BROWSER_MENU_HANDLE_PREFIX: &str = "uai-menu-v1-";
const BROWSER_PAGE_HANDLE_PREFIX: &str = "uai-page-v1-";
const UAI_BROWSER_PLAN_VERSION: u32 = 2;
const IFRAME_SCAN_TIMEOUT_MILLIS: u64 = 30_000;
const IFRAME_SCAN_RETRY_MILLIS: u64 = 1_500;
const MAX_IFRAME_SCAN_RETRIES: u32 = 20;
const IFRAME_SELECTORS: [&str; 4] = [
    "#ipublish-pc-book-easy-iframe",
    "iframe.ipublish-pc-iframe-container",
    "iframe[id*=\"iframe\"]",
    "iframe",
];
const TAB_SELECTORS: [&str; 2] = [
    ".pc-header-tabs-container .ant-col.tab .pc-tab-view-container",
    "#header ul.TabsBox a.topTab",
];
const TASK_SELECTORS: [&str; 1] = [".pc-header-tasks-row .pc-task"];
const POPUP_SELECTORS: [&str; 5] = [
    ".know-box .iKnow",
    ".ant-modal-confirm-btns .ant-btn-primary.system-info-cloud-ok-button",
    ".ant-modal-confirm-btns .ant-btn.ant-btn-primary",
    ".ipublish-modal-footer-ok",
    "button.ant-btn.ant-btn-default.ipublish-modal-footer-ok",
];
const VIDEO_SELECTORS: [&str; 2] = ["video.vjs-tech", "video"];

/// Stable Core `BrowserBridge` exchange type for one typed UAI command.
pub const UAI_BROWSER_COMMAND_TYPE: &str = "uai.browser.command";
/// Stable Core `BrowserBridge` exchange type for one typed UAI protocol event.
pub const UAI_BROWSER_EVENT_TYPE: &str = "uai.browser.event";
/// Stable Core `BrowserBridge` exchange type for one terminal residence result.
pub const UAI_BROWSER_RESIDENCE_RESULT_TYPE: &str = "uai.browser.residence.result";
/// Stable Core runtime-state type for the encrypted accumulated cursor.
pub const UAI_BROWSER_CURSOR_STATE_TYPE: &str = "uai.browser.cursor.v4";

/// Encrypted-at-rest UAI command material required to validate a browser
/// result after process recovery. Only the digest belongs in the ordinary
/// durable exchange ledger.
pub struct EncodedUaiBrowserCommandArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiBrowserCommandArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiBrowserCommandArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiBrowserCommandArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

/// One exact UAI helper command paired with Core's durable issued metadata.
///
/// The encrypted artifact and exchange are created from the same validated
/// command, so Core can persist them before dispatch without independently
/// rebuilding Provider-private browser state.
#[derive(Debug)]
pub struct UaiBrowserExchangeIssued {
    command: UaiBrowserCommandEnvelope,
    command_artifact: EncodedUaiBrowserCommandArtifact,
    exchange: BrowserBridgeExchange,
}

impl UaiBrowserExchangeIssued {
    pub const fn command(&self) -> &UaiBrowserCommandEnvelope {
        &self.command
    }

    pub const fn command_artifact(&self) -> &EncodedUaiBrowserCommandArtifact {
        &self.command_artifact
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        &self.exchange
    }

    /// Transfers the dispatch command, encrypted recovery material and exact
    /// ledger metadata as a single issuance boundary.
    pub fn into_parts(
        self,
    ) -> (
        UaiBrowserCommandEnvelope,
        EncodedUaiBrowserCommandArtifact,
        BrowserBridgeExchange,
    ) {
        (self.command, self.command_artifact, self.exchange)
    }
}

/// One exact accumulated cursor and command issued as a single persistence
/// boundary.
///
/// The two encrypted artifacts remain independent because they have different
/// lifecycles, but neither can be substituted without failing the cursor's
/// exact next-command binding.
#[derive(Debug)]
pub struct UaiBrowserCursorExchangeIssued {
    issued: UaiBrowserExchangeIssued,
    cursor_artifact: EncodedUaiBrowserCursorArtifact,
}

impl UaiBrowserCursorExchangeIssued {
    pub const fn command_issuance(&self) -> &UaiBrowserExchangeIssued {
        &self.issued
    }

    pub const fn command(&self) -> &UaiBrowserCommandEnvelope {
        self.issued.command()
    }

    pub const fn command_artifact(&self) -> &EncodedUaiBrowserCommandArtifact {
        self.issued.command_artifact()
    }

    pub const fn cursor_artifact(&self) -> &EncodedUaiBrowserCursorArtifact {
        &self.cursor_artifact
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        self.issued.exchange()
    }

    /// Transfers the exact dispatch command, both encrypted artifacts and
    /// durable exchange metadata without rebuilding any Provider state.
    pub fn into_parts(
        self,
    ) -> (
        UaiBrowserCommandEnvelope,
        EncodedUaiBrowserCommandArtifact,
        EncodedUaiBrowserCursorArtifact,
        BrowserBridgeExchange,
    ) {
        let (command, command_artifact, exchange) = self.issued.into_parts();
        (command, command_artifact, self.cursor_artifact, exchange)
    }

    /// Consumes one cursor-aware issuance into Core persistence material.
    ///
    /// Both secret artifacts move into the handoff without copying their
    /// plaintext bytes. Cursor metadata is derived only from the exact issued
    /// exchange and encoded cursor owned by this value.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the Provider-produced metadata cannot be
    /// represented by Core's generic runtime-state contract.
    pub fn into_persistence_handoff(self) -> ProviderResult<UaiBrowserCursorPersistenceHandoff> {
        let (command, command_artifact, cursor_artifact, exchange) = self.into_parts();
        let cursor_state_metadata = BrowserBridgeRuntimeStateMetadata {
            session_id: exchange.session_id,
            sequence: exchange.sequence,
            state_type: UAI_BROWSER_CURSOR_STATE_TYPE.to_owned(),
            state_digest: cursor_artifact.digest(),
            stored_at: exchange.issued_at,
        };
        cursor_state_metadata.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI cursor runtime-state metadata is invalid",
            )
        })?;
        Ok(UaiBrowserCursorPersistenceHandoff {
            command,
            exchange,
            command_artifact: command_artifact.into_secret_value(),
            cursor_state_metadata,
            cursor_state_artifact: cursor_artifact.into_secret_value(),
        })
    }
}

/// Consuming Core persistence handoff for one exact command and cursor.
#[derive(Debug)]
pub struct UaiBrowserCursorPersistenceHandoff {
    command: UaiBrowserCommandEnvelope,
    exchange: BrowserBridgeExchange,
    command_artifact: SecretValue,
    cursor_state_metadata: BrowserBridgeRuntimeStateMetadata,
    cursor_state_artifact: SecretValue,
}

impl UaiBrowserCursorPersistenceHandoff {
    pub const fn command(&self) -> &UaiBrowserCommandEnvelope {
        &self.command
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        &self.exchange
    }

    pub const fn cursor_state_metadata(&self) -> &BrowserBridgeRuntimeStateMetadata {
        &self.cursor_state_metadata
    }

    /// Transfers all dispatch and persistence owners without exposing or
    /// copying either secret artifact.
    pub fn into_parts(
        self,
    ) -> (
        UaiBrowserCommandEnvelope,
        BrowserBridgeExchange,
        SecretValue,
        BrowserBridgeRuntimeStateMetadata,
        SecretValue,
    ) {
        (
            self.command,
            self.exchange,
            self.command_artifact,
            self.cursor_state_metadata,
            self.cursor_state_artifact,
        )
    }
}

/// Strict consuming input restored from Core's command plus runtime sidecar.
///
/// Construction validates all generic UAI metadata and artifact digests before
/// a fresh Provider read. Owned secret bytes remain zeroizing on every error.
#[derive(Debug)]
pub struct UaiBrowserCursorPersistenceRecovery {
    exchange: BrowserBridgeExchange,
    command_artifact: SecretValue,
    cursor_state_metadata: BrowserBridgeRuntimeStateMetadata,
    cursor_state_artifact: SecretValue,
}

impl UaiBrowserCursorPersistenceRecovery {
    /// # Errors
    ///
    /// Rejects a non-issued UAI exchange, missing/foreign cursor state or
    /// either artifact whose exact bytes no longer match durable metadata.
    pub fn try_new(
        exchange: BrowserBridgeExchange,
        command_artifact: SecretValue,
        cursor_state_metadata: BrowserBridgeRuntimeStateMetadata,
        cursor_state_artifact: SecretValue,
    ) -> ProviderResult<Self> {
        let command_digest: [u8; 32] = Sha256::digest(command_artifact.expose_secret()).into();
        let cursor_digest: [u8; 32] = Sha256::digest(cursor_state_artifact.expose_secret()).into();
        if exchange.validate().is_err()
            || exchange.state != BrowserBridgeExchangeState::Issued
            || exchange.command_type != UAI_BROWSER_COMMAND_TYPE
            || cursor_state_metadata.validate().is_err()
            || cursor_state_metadata.state_type != UAI_BROWSER_CURSOR_STATE_TYPE
            || cursor_state_metadata.session_id != exchange.session_id
            || cursor_state_metadata.sequence != exchange.sequence
            || cursor_state_metadata.stored_at != exchange.issued_at
            || command_digest != exchange.command_digest
            || cursor_digest != cursor_state_metadata.state_digest
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI persisted cursor command or runtime state is stale or foreign",
            ));
        }
        Ok(Self {
            exchange,
            command_artifact,
            cursor_state_metadata,
            cursor_state_artifact,
        })
    }
}

/// One command and accumulated cursor restored from their independently
/// encrypted artifacts and the same persisted exchange.
#[derive(Debug)]
pub struct UaiBrowserCursorExchangeRecovered {
    command: UaiBrowserCommandEnvelope,
    cursor: UaiBrowserResidenceCursor,
}

impl UaiBrowserCursorExchangeRecovered {
    pub const fn command(&self) -> &UaiBrowserCommandEnvelope {
        &self.command
    }

    pub const fn cursor(&self) -> &UaiBrowserResidenceCursor {
        &self.cursor
    }

    pub fn into_parts(self) -> (UaiBrowserCommandEnvelope, UaiBrowserResidenceCursor) {
        (self.command, self.cursor)
    }
}

/// One validated intermediate helper event and its terminal exchange row.
#[derive(Debug)]
pub struct UaiBrowserEventExchangeCompleted {
    event: UaiBrowserEventEnvelope,
    exchange: BrowserBridgeExchange,
}

impl UaiBrowserEventExchangeCompleted {
    pub const fn event(&self) -> &UaiBrowserEventEnvelope {
        &self.event
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        &self.exchange
    }

    pub fn into_parts(self) -> (UaiBrowserEventEnvelope, BrowserBridgeExchange) {
        (self.event, self.exchange)
    }
}

/// One validated terminal residence observation and its completed exchange.
///
/// The observation still requires an independent fresh `DurationRead`; the
/// completed exchange proves only which issued command produced these bytes.
#[derive(Debug)]
pub struct UaiBrowserResidenceExchangeCompleted {
    result: UaiBrowserResidenceResult,
    exchange: BrowserBridgeExchange,
}

impl UaiBrowserResidenceExchangeCompleted {
    pub const fn result(&self) -> &UaiBrowserResidenceResult {
        &self.result
    }

    pub const fn exchange(&self) -> &BrowserBridgeExchange {
        &self.exchange
    }

    pub fn into_parts(self) -> (UaiBrowserResidenceResult, BrowserBridgeExchange) {
        (self.result, self.exchange)
    }
}

/// Owned bounded intermediate `BrowserBridge` event. Session nonce, opaque
/// handles and page labels remain redacted and zeroized until typed parsing
/// finishes.
pub struct UaiBrowserEventDocument(SecretValue);

impl UaiBrowserEventDocument {
    /// # Errors
    ///
    /// Rejects an empty or oversized helper event before parsing.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        Self::try_from_secret_value(SecretValue::new(document.into_bytes()))
    }

    /// Consumes Core's decrypted raw result without copying it into an
    /// ordinary plaintext `String`.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized or non-UTF-8 helper bytes. The owned secret is
    /// zeroized on every error and when the accepted document is dropped.
    pub fn try_from_secret_value(document: SecretValue) -> ProviderResult<Self> {
        let bytes = document.expose_secret();
        if bytes.is_empty()
            || bytes.len() > MAX_BROWSER_MESSAGE_BYTES
            || std::str::from_utf8(bytes).is_err()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge event is empty, oversized or not UTF-8",
            ));
        }
        Ok(Self(document))
    }

    /// # Errors
    ///
    /// Returns a typed error if the owned document violates exchange bounds.
    pub fn exchange_digest(&self) -> ProviderResult<[u8; 32]> {
        browser_event_exchange_digest(self.as_str()?)
    }

    /// # Errors
    ///
    /// Returns a typed error for malformed event JSON or a foreign command,
    /// session, frame, Task or transport-observed origin.
    pub fn parse_for_command(
        &self,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<UaiBrowserEventEnvelope> {
        parse_browser_event(self.as_str()?, plan, command, observed_origin)
    }

    fn as_str(&self) -> ProviderResult<&str> {
        std::str::from_utf8(self.0.expose_secret()).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge event lost its validated UTF-8 encoding",
            )
        })
    }
}

impl fmt::Debug for UaiBrowserEventDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserEventDocument")
            .field("bytes", &self.0.expose_secret().len())
            .field("contents", &"[REDACTED]")
            .finish()
    }
}

/// Core result-inbox metadata and raw event bytes validated as one owner.
#[derive(Debug)]
pub struct UaiBrowserEventInbox {
    metadata: BrowserBridgeResultArtifactMetadata,
    document: UaiBrowserEventDocument,
}

impl UaiBrowserEventInbox {
    /// Consumes one Core-resolved raw event and binds it to the exact issued
    /// UAI exchange before any fresh Provider read.
    ///
    /// # Errors
    ///
    /// Rejects foreign result type/session/sequence/time, changed raw bytes or
    /// an invalid bounded UTF-8 event document.
    pub fn try_new(
        issued_exchange: &BrowserBridgeExchange,
        metadata: BrowserBridgeResultArtifactMetadata,
        result_artifact: SecretValue,
    ) -> ProviderResult<Self> {
        validate_result_inbox(
            issued_exchange,
            &metadata,
            UAI_BROWSER_EVENT_TYPE,
            &result_artifact,
        )?;
        let document = UaiBrowserEventDocument::try_from_secret_value(result_artifact)?;
        Ok(Self { metadata, document })
    }

    pub const fn metadata(&self) -> &BrowserBridgeResultArtifactMetadata {
        &self.metadata
    }

    pub fn into_parts(self) -> (UaiBrowserEventDocument, BrowserBridgeResultArtifactMetadata) {
        (self.document, self.metadata)
    }
}

/// Owned bounded terminal `BrowserBridge` residence result. It remains an
/// observation requiring independent fresh duration readback.
pub struct UaiBrowserResidenceResultDocument(SecretValue);

impl UaiBrowserResidenceResultDocument {
    /// # Errors
    ///
    /// Rejects an empty or oversized terminal helper result before parsing.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        Self::try_from_secret_value(SecretValue::new(document.into_bytes()))
    }

    /// Consumes Core's decrypted terminal result without creating an ordinary
    /// plaintext `String` copy.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized or non-UTF-8 result bytes. The owned secret is
    /// zeroized on every error and when the accepted document is dropped.
    pub fn try_from_secret_value(document: SecretValue) -> ProviderResult<Self> {
        let bytes = document.expose_secret();
        if bytes.is_empty()
            || bytes.len() > MAX_BROWSER_RESULT_BYTES
            || std::str::from_utf8(bytes).is_err()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge result is empty, oversized or not UTF-8",
            ));
        }
        Ok(Self(document))
    }

    /// # Errors
    ///
    /// Returns a typed error if the owned document violates exchange bounds.
    pub fn exchange_digest(&self) -> ProviderResult<[u8; 32]> {
        browser_residence_exchange_digest(self.as_str()?)
    }

    /// # Errors
    ///
    /// Returns a typed error for malformed result JSON or a foreign command,
    /// session, frame, Task, timing fact or transport-observed origin.
    pub fn parse_for_command(
        &self,
        plan: &UaiBrowserResidencePlan,
        command: &UaiBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<UaiBrowserResidenceResult> {
        parse_browser_residence_result(self.as_str()?, plan, command, observed_origin)
    }

    fn as_str(&self) -> ProviderResult<&str> {
        std::str::from_utf8(self.0.expose_secret()).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge result lost its validated UTF-8 encoding",
            )
        })
    }
}

impl fmt::Debug for UaiBrowserResidenceResultDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserResidenceResultDocument")
            .field("bytes", &self.0.expose_secret().len())
            .field("contents", &"[REDACTED]")
            .finish()
    }
}

/// Core result-inbox metadata and terminal residence bytes validated together.
#[derive(Debug)]
pub struct UaiBrowserResidenceInbox {
    metadata: BrowserBridgeResultArtifactMetadata,
    document: UaiBrowserResidenceResultDocument,
}

impl UaiBrowserResidenceInbox {
    /// Consumes one Core-resolved terminal result and binds it to the exact
    /// issued UAI exchange before any fresh Provider read.
    ///
    /// # Errors
    ///
    /// Rejects foreign result type/session/sequence/time, changed raw bytes or
    /// an invalid bounded UTF-8 residence document.
    pub fn try_new(
        issued_exchange: &BrowserBridgeExchange,
        metadata: BrowserBridgeResultArtifactMetadata,
        result_artifact: SecretValue,
    ) -> ProviderResult<Self> {
        validate_result_inbox(
            issued_exchange,
            &metadata,
            UAI_BROWSER_RESIDENCE_RESULT_TYPE,
            &result_artifact,
        )?;
        let document = UaiBrowserResidenceResultDocument::try_from_secret_value(result_artifact)?;
        Ok(Self { metadata, document })
    }

    pub const fn metadata(&self) -> &BrowserBridgeResultArtifactMetadata {
        &self.metadata
    }

    pub fn into_parts(
        self,
    ) -> (
        UaiBrowserResidenceResultDocument,
        BrowserBridgeResultArtifactMetadata,
    ) {
        (self.document, self.metadata)
    }
}

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
    pub iframe_selectors: Vec<String>,
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
    pub iframe_scan_timeout_millis: u64,
    pub iframe_scan_retry_millis: u64,
    pub max_iframe_scan_retries: u32,
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
    /// Returns the stable durable exchange type used by Core.
    pub const fn exchange_type() -> &'static str {
        UAI_BROWSER_RESIDENCE_RESULT_TYPE
    }

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
            || self
                .processed_tabs
                .max(self.processed_micros)
                .checked_mul(plan.max_tasks_per_tab)
                .is_none_or(|max_tasks| self.processed_tasks > max_tasks)
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

    pub(crate) fn validate_for_binding(
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
pub enum UaiBrowserResidenceControl {
    Pause,
    Resume,
    Restart { start_micro_ordinal: u32 },
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
    ResidenceControl {
        task_handle: String,
        seconds: u64,
        control: UaiBrowserResidenceControl,
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
    /// Returns the stable durable exchange type used by Core.
    pub const fn exchange_type() -> &'static str {
        UAI_BROWSER_COMMAND_TYPE
    }

    /// Hashes the canonical typed command envelope for Core's durable
    /// exchange record. The Provider payload itself remains outside Domain
    /// storage.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the command is foreign to the frozen plan or
    /// serialization unexpectedly fails.
    pub fn exchange_digest(&self, plan: &UaiBrowserResidencePlan) -> ProviderResult<[u8; 32]> {
        Ok(self.encode_artifact(plan)?.digest())
    }

    /// Encodes one validated exact command for Core's encrypted recovery
    /// boundary. Session nonce, frame and opaque handles never enter the
    /// ordinary exchange row.
    ///
    /// # Errors
    ///
    /// Returns a typed error if command validation, serialization or size
    /// bounds fail.
    pub fn encode_artifact(
        &self,
        plan: &UaiBrowserResidencePlan,
    ) -> ProviderResult<EncodedUaiBrowserCommandArtifact> {
        self.validate_for_plan(plan)?;
        let mut encoded = Zeroizing::new(serde_json::to_vec(self).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge command cannot be encoded",
            )
        })?);
        if encoded.is_empty() || encoded.len() > MAX_BROWSER_COMMAND_BYTES {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge command artifact is empty or oversized",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedUaiBrowserCommandArtifact { value, digest })
    }

    /// Resolves an encrypted issued command and repeats every independent
    /// browser-session binding before a re-delivered result can be parsed.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest/schema drift or a foreign plan,
    /// session, origin, frame, Task or command sequence.
    #[allow(
        clippy::too_many_arguments,
        reason = "the durable digest and each browser transport authority are independent recovery bindings"
    )]
    pub fn decode_artifact_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        plan: &UaiBrowserResidencePlan,
        expected_session_nonce: &str,
        expected_origin: &str,
        expected_frame_id: &str,
        expected_sequence: u32,
    ) -> ProviderResult<Self> {
        plan.validate()?;
        validate_browser_binding(
            plan,
            expected_session_nonce,
            expected_frame_id,
            expected_origin,
        )?;
        let command = Self::decode_artifact(value, expected_digest, plan)?;
        if command.session_nonce != expected_session_nonce
            || command.origin != expected_origin
            || command.frame_id != expected_frame_id
            || command.remote_task_id != plan.target_remote_task_id
            || command.sequence != expected_sequence
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge command artifact binding is stale or foreign",
            ));
        }
        Ok(command)
    }

    /// Resolves the exact command owned by one persisted Core exchange.
    ///
    /// Origin and frame remain Provider-artifact facts rather than helper
    /// input. The fresh plan validates their allowlist and shape; the actual
    /// transport origin is independently compared when a result is parsed.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest/schema drift, a foreign fresh plan,
    /// session or Task, or a sequence outside the Provider wire boundary.
    pub fn decode_artifact_for_exchange(
        value: &SecretValue,
        expected_digest: [u8; 32],
        plan: &UaiBrowserResidencePlan,
        expected_session_id: BrowserBridgeSessionId,
        expected_sequence: u64,
    ) -> ProviderResult<Self> {
        let expected_sequence = u32::try_from(expected_sequence).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI recovered BrowserBridge sequence exceeds the Provider boundary",
            )
        })?;
        let command = Self::decode_artifact(value, expected_digest, plan)?;
        if command.session_nonce != expected_session_id.to_string()
            || command.remote_task_id != plan.target_remote_task_id
            || command.sequence != expected_sequence
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge command artifact is foreign to its persisted exchange",
            ));
        }
        Ok(command)
    }

    fn decode_artifact(
        value: &SecretValue,
        expected_digest: [u8; 32],
        plan: &UaiBrowserResidencePlan,
    ) -> ProviderResult<Self> {
        plan.validate()?;
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_BROWSER_COMMAND_BYTES {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge command artifact is empty or oversized",
            ));
        }
        let actual_digest: [u8; 32] = Sha256::digest(bytes).into();
        if actual_digest != expected_digest {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI BrowserBridge command artifact digest changed",
            ));
        }
        let command: Self = serde_json::from_slice(bytes).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI BrowserBridge command artifact schema changed",
            )
        })?;
        command.validate_for_plan(plan)?;
        Ok(command)
    }

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
        Self::residence_target_for_leaf(plan, binding, sequence, target, plan.residence_seconds)
    }

    pub(crate) fn residence_target_for_leaf(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        target: &UaiBrowserTargetTaskEntry,
        leaf_seconds: u64,
    ) -> ProviderResult<Self> {
        let entry = target.entry();
        entry.validate_for_binding(plan, binding)?;
        if entry.scope != UaiBrowserPageScope::Task
            || entry.label != plan.target.task
            || leaf_seconds == 0
            || leaf_seconds > plan.residence_seconds
        {
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
                seconds: leaf_seconds,
                play_video: plan.play_video,
            },
        )
    }

    /// Builds a bounded pause/resume/restart control for an active residence
    /// action. A restart keeps the immutable budget and only changes the
    /// freshly discovered Micro start ordinal.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign target, invalid handle or an
    /// out-of-range restart ordinal.
    pub fn residence_control(
        plan: &UaiBrowserResidencePlan,
        binding: &UaiBrowserSessionBinding,
        sequence: u32,
        target: &UaiBrowserTargetTaskEntry,
        control: UaiBrowserResidenceControl,
    ) -> ProviderResult<Self> {
        let entry = target.entry();
        entry.validate_for_binding(plan, binding)?;
        if entry.scope != UaiBrowserPageScope::Task || entry.label != plan.target.task {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI BrowserBridge residence control is outside the exact fresh Task target",
            ));
        }
        if matches!(
            &control,
            UaiBrowserResidenceControl::Restart { start_micro_ordinal }
                if *start_micro_ordinal >= plan.max_discovered_micros
        ) {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge restart Micro ordinal exceeds the frozen bound",
            ));
        }
        Self::new(
            plan,
            binding,
            sequence,
            UaiBrowserCommand::ResidenceControl {
                task_handle: entry.handle.clone(),
                seconds: plan.residence_seconds,
                control,
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
                        || *seconds == 0
                        || *seconds > plan.residence_seconds
                        || *play_video != plan.play_video
            )
            || matches!(
                &self.command,
                UaiBrowserCommand::ResidenceControl { task_handle, seconds, control }
                    if !is_browser_page_handle(task_handle)
                        || *seconds == 0
                        || *seconds > plan.residence_seconds
                        || matches!(control, UaiBrowserResidenceControl::Restart { start_micro_ordinal } if *start_micro_ordinal >= plan.max_discovered_micros)
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
    ResidenceControlResult {
        task_handle: String,
        control: UaiBrowserResidenceControl,
        accepted: bool,
        observed_active_seconds: u64,
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
    /// Returns the stable durable exchange type used by Core.
    pub const fn exchange_type() -> &'static str {
        UAI_BROWSER_EVENT_TYPE
    }

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
                validate_menu_event(plan, &binding, entries)?;
            }
            (
                UaiBrowserCommand::ClickMenu { handle: expected },
                UaiBrowserEvent::ClickResult { handle, clicked },
            ) if *clicked && handle == expected && is_browser_menu_handle(handle) => {}
            (
                UaiBrowserCommand::ScanPage { scope: expected },
                UaiBrowserEvent::PageList { scope, entries },
            ) if scope == expected => {
                validate_page_event(plan, &binding, *scope, entries)?;
            }
            (
                UaiBrowserCommand::ClickTab { handle: expected }
                | UaiBrowserCommand::ClickTask { handle: expected },
                UaiBrowserEvent::ClickResult { handle, clicked },
            ) if *clicked && handle == expected && is_browser_page_handle(handle) => {}
            (
                UaiBrowserCommand::ResidenceControl {
                    task_handle: expected_task,
                    seconds,
                    control: expected_control,
                },
                UaiBrowserEvent::ResidenceControlResult {
                    task_handle,
                    control,
                    accepted,
                    observed_active_seconds,
                },
            ) => validate_residence_control_event(
                expected_task,
                *seconds,
                expected_control,
                task_handle,
                control,
                *accepted,
                *observed_active_seconds,
            )?,
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

fn validate_menu_event(
    plan: &UaiBrowserResidencePlan,
    binding: &UaiBrowserSessionBinding,
    entries: &[UaiBrowserMenuEntry],
) -> ProviderResult<()> {
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
        entry.validate_for_binding(plan, binding)?;
    }
    Ok(())
}

fn validate_page_event(
    plan: &UaiBrowserResidencePlan,
    binding: &UaiBrowserSessionBinding,
    scope: UaiBrowserPageScope,
    entries: &[UaiBrowserPageEntry],
) -> ProviderResult<()> {
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
        if entry.scope != scope || entry.ordinal as usize != expected_ordinal {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI BrowserBridge page entries are foreign, missing or reordered",
            ));
        }
        entry.validate_for_binding(plan, binding)?;
    }
    Ok(())
}

fn validate_residence_control_event(
    expected_task: &str,
    expected_seconds: u64,
    expected_control: &UaiBrowserResidenceControl,
    task_handle: &str,
    control: &UaiBrowserResidenceControl,
    accepted: bool,
    observed_active_seconds: u64,
) -> ProviderResult<()> {
    if accepted
        && task_handle == expected_task
        && control == expected_control
        && observed_active_seconds <= expected_seconds
    {
        Ok(())
    } else {
        Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI BrowserBridge residence control result is foreign or over budget",
        ))
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

/// Hashes one bounded raw protocol event for Core's durable exchange record.
/// The raw event remains Provider/browser-helper-owned and is never persisted
/// in Domain storage.
///
/// # Errors
///
/// Returns a typed error when the document is empty or oversized.
pub fn browser_event_exchange_digest(document: &str) -> ProviderResult<[u8; 32]> {
    browser_exchange_digest(
        document,
        MAX_BROWSER_MESSAGE_BYTES,
        "UAI BrowserBridge event is empty or oversized",
    )
}

/// Hashes one bounded terminal residence result for Core's durable exchange
/// record without persisting the Provider-private document.
///
/// # Errors
///
/// Returns a typed error when the document is empty or oversized.
pub fn browser_residence_exchange_digest(document: &str) -> ProviderResult<[u8; 32]> {
    browser_exchange_digest(
        document,
        MAX_BROWSER_RESULT_BYTES,
        "UAI BrowserBridge result is empty or oversized",
    )
}

fn browser_exchange_digest(
    document: &str,
    max_bytes: usize,
    error_message: &'static str,
) -> ProviderResult<[u8; 32]> {
    if document.is_empty() || document.len() > max_bytes {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            error_message,
        ));
    }
    Ok(Sha256::digest(document.as_bytes()).into())
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
        let valid = self.version == UAI_BROWSER_PLAN_VERSION
            && self.target_remote_task_id.starts_with("group:")
            && self.allowed_origins == [UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()]
            && self.discovery_strategies
                == [
                    UaiMenuDiscoveryStrategy::LegacySlider,
                    UaiMenuDiscoveryStrategy::AntTree,
                    UaiMenuDiscoveryStrategy::AriaMenu,
                    UaiMenuDiscoveryStrategy::U3Menu,
                ]
            && exact_selector_set(&self.iframe_selectors, &IFRAME_SELECTORS)
            && exact_selector_set(&self.tab_selectors, &TAB_SELECTORS)
            && exact_selector_set(&self.task_selectors, &TASK_SELECTORS)
            && exact_selector_set(&self.popup_selectors, &POPUP_SELECTORS)
            && exact_selector_set(&self.video_selectors, &VIDEO_SELECTORS)
            && (60..=28_800).contains(&self.residence_seconds)
            && self.max_discovered_micros == MAX_DISCOVERED_MICROS
            && self.max_tabs_per_micro == MAX_TABS_PER_MICRO
            && self.max_tasks_per_tab == MAX_TASKS_PER_TAB
            && self.max_popup_clicks_per_stage == MAX_POPUP_CLICKS_PER_STAGE
            && self.dom_poll_millis == MAX_DOM_POLL_MILLIS
            && self.iframe_scan_timeout_millis == IFRAME_SCAN_TIMEOUT_MILLIS
            && self.iframe_scan_retry_millis == IFRAME_SCAN_RETRY_MILLIS
            && self.max_iframe_scan_retries == MAX_IFRAME_SCAN_RETRIES
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

fn exact_selector_set(actual: &[String], expected: &[&str]) -> bool {
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

fn owned_selectors(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
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

    /// Freshly rebinds a Task and pairs one exact typed command with the
    /// durable exchange metadata Core must persist before helper dispatch.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the command is foreign to the fresh plan or
    /// Core session, or its sequence cannot be represented durably.
    #[allow(
        clippy::too_many_arguments,
        reason = "Task, settings, Core session, Provider command and issue time are independent bindings"
    )]
    pub async fn issue_command_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        session_id: BrowserBridgeSessionId,
        command: UaiBrowserCommandEnvelope,
        issued_at: Timestamp,
    ) -> ProviderResult<UaiBrowserExchangeIssued> {
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        issue_command_exchange_inner(&plan, remote_task_id, session_id, command, issued_at, false)
    }

    /// Freshly rebinds one accumulated cursor and issues its exact next
    /// command, returning both encrypted artifacts with one durable exchange.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the Course batch, fresh Browser plan,
    /// cursor, command or Core session does not describe the same next action.
    #[allow(
        clippy::too_many_arguments,
        reason = "the Course batch, fresh Task/settings, Core session and cursor are independent issuance bindings"
    )]
    pub async fn issue_cursor_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        batch: &UaiCourseResidenceBatchPlan,
        session_id: BrowserBridgeSessionId,
        cursor: &UaiBrowserResidenceCursor,
        command: UaiBrowserCommandEnvelope,
        issued_at: Timestamp,
    ) -> ProviderResult<UaiBrowserCursorExchangeIssued> {
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        let cursor_artifact = cursor.encode_artifact(batch, &plan, &command)?;
        let issued = issue_command_exchange_inner(
            &plan,
            remote_task_id,
            session_id,
            command,
            issued_at,
            true,
        )?;
        if cursor.next_command_digest() != issued.command_artifact().digest() {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI accumulated cursor and issued command digests diverged",
            ));
        }
        Ok(UaiBrowserCursorExchangeIssued {
            issued,
            cursor_artifact,
        })
    }

    /// Restores both Provider-private artifacts selected by one persisted
    /// exchange and freshly rebinds their Course, Task, plan and command.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a stale exchange, either changed artifact,
    /// changed Course batch or Browser plan, or a cursor from another command.
    #[allow(
        clippy::too_many_arguments,
        reason = "the persisted exchange, two encrypted artifacts and fresh Course/Task authorities are independent recovery bindings"
    )]
    pub async fn recover_cursor_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        batch: &UaiCourseResidenceBatchPlan,
        issued_exchange: &BrowserBridgeExchange,
        command_artifact: &SecretValue,
        cursor_artifact: &SecretValue,
        expected_cursor_digest: [u8; 32],
    ) -> ProviderResult<UaiBrowserCursorExchangeRecovered> {
        validate_issued_exchange(issued_exchange)?;
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        let command = UaiBrowserCommandEnvelope::decode_artifact_for_exchange(
            command_artifact,
            issued_exchange.command_digest,
            &plan,
            issued_exchange.session_id,
            issued_exchange.sequence,
        )?;
        let cursor = UaiBrowserResidenceCursor::decode_artifact_bound(
            cursor_artifact,
            expected_cursor_digest,
            batch,
            &plan,
            &command,
        )?;
        Ok(UaiBrowserCursorExchangeRecovered { command, cursor })
    }

    /// Consumes Core's resolved command and cursor sidecar, then freshly
    /// rebinds both Provider artifacts to the exact Course batch and Task.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale generic metadata, changed encrypted
    /// bytes, fresh Course/Task drift or a cursor paired with another command.
    pub async fn recover_persisted_cursor_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        batch: &UaiCourseResidenceBatchPlan,
        recovery: UaiBrowserCursorPersistenceRecovery,
    ) -> ProviderResult<UaiBrowserCursorExchangeRecovered> {
        let UaiBrowserCursorPersistenceRecovery {
            exchange,
            command_artifact,
            cursor_state_metadata,
            cursor_state_artifact,
        } = recovery;
        self.recover_cursor_exchange(
            context,
            remote_task_id,
            settings,
            batch,
            &exchange,
            &command_artifact,
            &cursor_state_artifact,
            cursor_state_metadata.state_digest,
        )
        .await
    }

    /// Freshly validates an intermediate event for an in-memory issued
    /// command and completes its exact durable exchange row.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale Task/settings, foreign helper output or
    /// invalid terminal exchange metadata.
    #[allow(
        clippy::too_many_arguments,
        reason = "fresh Provider settings and the independently observed helper result remain separate bindings"
    )]
    pub async fn complete_event_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        issued: &UaiBrowserExchangeIssued,
        document: UaiBrowserEventDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<UaiBrowserEventExchangeCompleted> {
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        complete_event_exchange_inner(
            &plan,
            issued.command(),
            issued.exchange(),
            &document,
            observed_origin,
            completed_at,
        )
    }

    /// Restores the command selected by a persisted exchange and completes an
    /// intermediate event without trusting helper-echoed command material.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a non-issued/foreign exchange, encrypted
    /// artifact drift, stale Task/settings or invalid helper output.
    #[allow(
        clippy::too_many_arguments,
        reason = "the persisted exchange, encrypted artifact and independently observed helper result are recovery authorities"
    )]
    pub async fn complete_recovered_event_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        issued_exchange: &BrowserBridgeExchange,
        command_artifact: &SecretValue,
        document: UaiBrowserEventDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<UaiBrowserEventExchangeCompleted> {
        validate_issued_exchange(issued_exchange)?;
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        let command = UaiBrowserCommandEnvelope::decode_artifact_for_exchange(
            command_artifact,
            issued_exchange.command_digest,
            &plan,
            issued_exchange.session_id,
            issued_exchange.sequence,
        )?;
        complete_event_exchange_inner(
            &plan,
            &command,
            issued_exchange,
            &document,
            observed_origin,
            completed_at,
        )
    }

    /// Freshly validates a terminal residence observation and completes its
    /// exact issued exchange. The result still requires fresh duration readback.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale Task/settings, a non-residence command,
    /// foreign helper output or invalid terminal exchange metadata.
    #[allow(
        clippy::too_many_arguments,
        reason = "fresh Provider settings and the independently observed terminal result remain separate bindings"
    )]
    pub async fn complete_residence_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        issued: &UaiBrowserExchangeIssued,
        document: UaiBrowserResidenceResultDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<UaiBrowserResidenceExchangeCompleted> {
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        complete_residence_exchange_inner(
            &plan,
            issued.command(),
            issued.exchange(),
            &document,
            observed_origin,
            completed_at,
        )
    }

    /// Restores a residence command from Core's encrypted artifact repository
    /// and binds the terminal observation to that command and fresh Task.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a non-issued/foreign exchange, encrypted
    /// artifact drift, stale Task/settings or invalid helper output.
    #[allow(
        clippy::too_many_arguments,
        reason = "the persisted exchange, encrypted artifact and independently observed terminal result are recovery authorities"
    )]
    pub async fn complete_recovered_residence_exchange(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        settings: &asterism_provider_api::ResolvedProviderRuntimeSettings,
        issued_exchange: &BrowserBridgeExchange,
        command_artifact: &SecretValue,
        document: UaiBrowserResidenceResultDocument,
        observed_origin: &str,
        completed_at: Timestamp,
    ) -> ProviderResult<UaiBrowserResidenceExchangeCompleted> {
        validate_issued_exchange(issued_exchange)?;
        let plan = self
            .residence_plan(context, remote_task_id, settings)
            .await?;
        let command = UaiBrowserCommandEnvelope::decode_artifact_for_exchange(
            command_artifact,
            issued_exchange.command_digest,
            &plan,
            issued_exchange.session_id,
            issued_exchange.sequence,
        )?;
        complete_residence_exchange_inner(
            &plan,
            &command,
            issued_exchange,
            &document,
            observed_origin,
            completed_at,
        )
    }
}

fn issue_command_exchange_inner(
    plan: &UaiBrowserResidencePlan,
    remote_task_id: &str,
    session_id: BrowserBridgeSessionId,
    command: UaiBrowserCommandEnvelope,
    issued_at: Timestamp,
    allow_leaf_residence_budget: bool,
) -> ProviderResult<UaiBrowserExchangeIssued> {
    command.validate_for_plan(plan)?;
    if command.session_nonce != session_id.to_string() || command.remote_task_id != remote_task_id {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI BrowserBridge command is foreign to the durable Core session",
        ));
    }
    if !allow_leaf_residence_budget
        && matches!(
            &command.command,
            UaiBrowserCommand::ResidenceTarget { seconds, .. }
                | UaiBrowserCommand::ResidenceControl { seconds, .. }
                if *seconds != plan.residence_seconds
        )
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI leaf residence budget requires an accumulated cursor",
        ));
    }
    let sequence = u64::from(command.sequence);
    let command_artifact = command.encode_artifact(plan)?;
    let exchange = BrowserBridgeExchange::issue(
        session_id,
        sequence,
        UaiBrowserCommandEnvelope::exchange_type().to_owned(),
        command_artifact.digest(),
        issued_at,
    )
    .map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI command cannot be represented by the durable BrowserBridge exchange",
        )
    })?;
    Ok(UaiBrowserExchangeIssued {
        command,
        command_artifact,
        exchange,
    })
}

fn validate_issued_exchange(exchange: &BrowserBridgeExchange) -> ProviderResult<()> {
    if exchange.validate().is_err()
        || exchange.state != BrowserBridgeExchangeState::Issued
        || exchange.command_type != UaiBrowserCommandEnvelope::exchange_type()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI recovered BrowserBridge exchange is stale or foreign",
        ));
    }
    Ok(())
}

fn validate_result_inbox(
    issued_exchange: &BrowserBridgeExchange,
    metadata: &BrowserBridgeResultArtifactMetadata,
    expected_result_type: &str,
    result_artifact: &SecretValue,
) -> ProviderResult<()> {
    let result_digest: [u8; 32] = Sha256::digest(result_artifact.expose_secret()).into();
    if issued_exchange.validate().is_err()
        || issued_exchange.state != BrowserBridgeExchangeState::Issued
        || issued_exchange.command_type != UAI_BROWSER_COMMAND_TYPE
        || metadata.validate().is_err()
        || metadata.session_id != issued_exchange.session_id
        || metadata.sequence != issued_exchange.sequence
        || metadata.result_type != expected_result_type
        || metadata.received_at < issued_exchange.issued_at
        || metadata.result_digest != result_digest
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "UAI BrowserBridge result inbox is stale, changed or foreign",
        ));
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the resolved command and independently observed event retain separate exchange bindings"
)]
fn complete_event_exchange_inner(
    plan: &UaiBrowserResidencePlan,
    command: &UaiBrowserCommandEnvelope,
    issued_exchange: &BrowserBridgeExchange,
    document: &UaiBrowserEventDocument,
    observed_origin: &str,
    completed_at: Timestamp,
) -> ProviderResult<UaiBrowserEventExchangeCompleted> {
    let result_digest = document.exchange_digest()?;
    let event = document.parse_for_command(plan, command, observed_origin)?;
    let mut exchange = issued_exchange.clone();
    exchange
        .complete(
            UaiBrowserEventEnvelope::exchange_type().to_owned(),
            result_digest,
            completed_at,
        )
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI event cannot complete the durable BrowserBridge exchange",
            )
        })?;
    Ok(UaiBrowserEventExchangeCompleted { event, exchange })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the resolved command and independently observed result retain separate exchange bindings"
)]
fn complete_residence_exchange_inner(
    plan: &UaiBrowserResidencePlan,
    command: &UaiBrowserCommandEnvelope,
    issued_exchange: &BrowserBridgeExchange,
    document: &UaiBrowserResidenceResultDocument,
    observed_origin: &str,
    completed_at: Timestamp,
) -> ProviderResult<UaiBrowserResidenceExchangeCompleted> {
    let result_digest = document.exchange_digest()?;
    let result = document.parse_for_command(plan, command, observed_origin)?;
    let mut exchange = issued_exchange.clone();
    exchange
        .complete(
            UaiBrowserResidenceResult::exchange_type().to_owned(),
            result_digest,
            completed_at,
        )
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI residence result cannot complete the durable BrowserBridge exchange",
            )
        })?;
    Ok(UaiBrowserResidenceExchangeCompleted { result, exchange })
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
        version: UAI_BROWSER_PLAN_VERSION,
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
        iframe_selectors: owned_selectors(&IFRAME_SELECTORS),
        tab_selectors: owned_selectors(&TAB_SELECTORS),
        task_selectors: owned_selectors(&TASK_SELECTORS),
        popup_selectors: owned_selectors(&POPUP_SELECTORS),
        video_selectors: owned_selectors(&VIDEO_SELECTORS),
        residence_seconds,
        play_video,
        max_discovered_micros: MAX_DISCOVERED_MICROS,
        max_tabs_per_micro: MAX_TABS_PER_MICRO,
        max_tasks_per_tab: MAX_TASKS_PER_TAB,
        max_popup_clicks_per_stage: MAX_POPUP_CLICKS_PER_STAGE,
        dom_poll_millis: MAX_DOM_POLL_MILLIS,
        iframe_scan_timeout_millis: IFRAME_SCAN_TIMEOUT_MILLIS,
        iframe_scan_retry_millis: IFRAME_SCAN_RETRY_MILLIS,
        max_iframe_scan_retries: MAX_IFRAME_SCAN_RETRIES,
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
        let spec = BrowserSessionSpec {
            version: 2,
            start_url: browser_start_url_from_detail(&detail)?,
            isolation_key: isolation_key(context, remote_task_id),
            allowed_origins: vec![UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()],
            // The audited donor depends on a real rendered page, DOM events,
            // iframe messaging and media state. Do not silently substitute a
            // headless-only execution mode before that boundary is verified.
            headless: false,
        };
        spec.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI BrowserBridge session policy is invalid or unbounded",
            )
        })?;
        Ok(spec)
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
    use crate::{UaiDurationDocument, UaiDurationTransport, UaiTaskDuration};

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        advertised: bool,
    }

    #[derive(Debug)]
    struct FixtureDuration;

    #[async_trait]
    impl UaiDurationTransport for FixtureDuration {
        async fn fetch_duration(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            unit_id: &str,
        ) -> ProviderResult<UaiDurationDocument> {
            assert_eq!(course_resource_id, "2001");
            assert_eq!(unit_id, "unit-z");
            UaiDurationDocument::try_new(
                serde_json::json!({
                    "code": 1,
                    "success": true,
                    "value": {
                        "list": [{
                            "nodeId": "unit-z",
                            "role": "unit",
                            "children": [{
                                "nodeId": "group-z",
                                "role": "link",
                                "finishProgress": 50,
                                "duration": 700,
                                "required": true,
                                "scoreTaskFlag": false
                            }]
                        }]
                    }
                })
                .to_string(),
            )
        }
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
        let ordered_batch_target = remote_task_id == "group:2001:unit-z:group-z";
        let (
            unit_id,
            unit_title,
            section_id,
            section_title,
            micro_id,
            micro_title,
            group_id,
            title,
        ) = if ordered_batch_target {
            (
                "unit-z",
                "Unit Z",
                "section-z",
                "Section Z",
                "micro-z",
                "Micro Z",
                "group-z",
                "Task Z",
            )
        } else {
            (
                "unit-1",
                "Unit 1",
                "section-1",
                "Section 2",
                "micro-1",
                "Reading",
                "group-1",
                "Read the passage",
            )
        };
        let normalized = serde_json::json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": unit_id, "title": unit_title},
            "section": {"id": section_id, "title": section_title},
            "micro": {"id": micro_id, "title": micro_title},
            "group_id": group_id,
            "task_types": ["rich-text-read"],
            "question_count": 1,
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: remote_task_id.to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: title.to_owned(),
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
        assert_eq!(first.version, 2);
        assert_eq!(
            first.start_url,
            "https://ucontent.unipus.cn/_explorationpc_default/pc.html?cid=2001"
        );
        assert!(first.validate().is_ok());
        assert_eq!(first.digest(), same.digest());
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
        assert!(!first.start_url.contains("token"));
        assert!(!first.start_url.contains("openid"));
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
        assert_eq!(plan.version, UAI_BROWSER_PLAN_VERSION);
        assert_eq!(plan.discovery_strategies.len(), 4);
        assert_eq!(plan.iframe_selectors.len(), 4);
        assert_eq!(plan.tab_selectors.len(), 2);
        assert_eq!(plan.iframe_scan_timeout_millis, 30_000);
        assert_eq!(plan.iframe_scan_retry_millis, 1_500);
        assert_eq!(plan.max_iframe_scan_retries, 20);
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
    fn residence_plan_rejects_selector_or_iframe_scan_drift() {
        let plan = residence_plan(false);
        assert!(exact_selector_set(
            &plan.iframe_selectors,
            &IFRAME_SELECTORS
        ));
        assert!(exact_selector_set(&plan.tab_selectors, &TAB_SELECTORS));
        assert!(exact_selector_set(&plan.task_selectors, &TASK_SELECTORS));
        assert!(exact_selector_set(&plan.popup_selectors, &POPUP_SELECTORS));
        assert!(exact_selector_set(&plan.video_selectors, &VIDEO_SELECTORS));

        let mut changed = plan.clone();
        changed.iframe_selectors[3] = "iframe[data-arbitrary]".to_owned();
        assert!(changed.validate().is_err());
        let mut changed = plan.clone();
        changed.tab_selectors.swap(0, 1);
        assert!(changed.validate().is_err());
        let mut changed = plan.clone();
        changed.task_selectors.push(".foreign-task".to_owned());
        assert!(changed.validate().is_err());
        let mut changed = plan.clone();
        changed.popup_selectors.pop();
        assert!(changed.validate().is_err());
        let mut changed = plan.clone();
        changed.video_selectors[0] = "video[data-foreign]".to_owned();
        assert!(changed.validate().is_err());
        let mut changed = plan.clone();
        changed.iframe_scan_timeout_millis += 1;
        assert!(changed.validate().is_err());
        let mut changed = plan.clone();
        changed.iframe_scan_retry_millis -= 1;
        assert!(changed.validate().is_err());
        let mut changed = plan;
        changed.max_iframe_scan_retries += 1;
        assert!(changed.validate().is_err());
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
            "version": UAI_BROWSER_PLAN_VERSION,
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
            "processed_tasks": 8,
            "video_seconds": 0,
            "cancelled": false,
            "last_label": "Unit 1 / Section 2 / Micro 3",
        })
        .to_string();
        let owned = UaiBrowserResidenceResultDocument::try_from_secret_value(SecretValue::new(
            document.clone().into_bytes(),
        ))
        .unwrap();
        let debug = format!("{owned:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("nonce-42") && !debug.contains("Unit 1"));
        let result = owned
            .parse_for_command(&plan, &command, UCONTENT_ORIGIN)
            .unwrap();
        assert_eq!(
            UaiBrowserResidenceResult::exchange_type(),
            UAI_BROWSER_RESIDENCE_RESULT_TYPE
        );
        assert_eq!(
            UaiBrowserCommandEnvelope::exchange_type(),
            UAI_BROWSER_COMMAND_TYPE
        );
        assert_ne!(command.exchange_digest(&plan).unwrap(), [0; 32]);
        assert_eq!(
            owned.exchange_digest().unwrap(),
            browser_residence_exchange_digest(&document).unwrap()
        );
        assert_ne!(owned.exchange_digest().unwrap(), [0; 32]);
        assert!(UaiBrowserResidenceResultDocument::try_new(String::new()).is_err());
        assert!(
            UaiBrowserResidenceResultDocument::try_new("x".repeat(MAX_BROWSER_RESULT_BYTES + 1))
                .is_err()
        );
        assert!(
            UaiBrowserResidenceResultDocument::try_from_secret_value(SecretValue::new(vec![0xff]))
                .is_err()
        );
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
            "version": UAI_BROWSER_PLAN_VERSION,
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
        assert_eq!(
            UaiBrowserEventEnvelope::exchange_type(),
            UAI_BROWSER_EVENT_TYPE
        );
        assert_ne!(
            browser_event_exchange_digest(&menu_document).unwrap(),
            [0; 32]
        );
        let UaiBrowserEvent::MenuList { ref entries } = menu.event else {
            panic!("expected menu list")
        };
        let target = plan.select_target_menu_entry(&binding, entries).unwrap();
        let click = UaiBrowserCommandEnvelope::click_menu(&plan, &binding, 2, &target).unwrap();
        assert!(matches!(click.command, UaiBrowserCommand::ClickMenu { .. }));
        let rejected_event = serde_json::json!({
            "version": UAI_BROWSER_PLAN_VERSION,
            "session_nonce": "nonce-42",
            "origin": UCONTENT_ORIGIN,
            "frame_id": "content-frame",
            "remote_task_id": "group:2001:unit-1:group-1",
            "reply_to_sequence": 2,
            "event": {"kind": "click_result", "handle": target.entry().handle, "clicked": false}
        })
        .to_string();
        assert!(parse_browser_event(&rejected_event, &plan, &click, UCONTENT_ORIGIN).is_err());

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
            "version": UAI_BROWSER_PLAN_VERSION,
            "session_nonce": "nonce-42",
            "origin": IPUB_ORIGIN,
            "frame_id": "ipub-frame",
            "remote_task_id": "group:2001:unit-1:group-1",
            "reply_to_sequence": 7,
            "event": { "kind": "pong" },
        })
        .to_string();
        let owned = UaiBrowserEventDocument::try_from_secret_value(SecretValue::new(
            pong_document.clone().into_bytes(),
        ))
        .unwrap();
        assert!(owned.parse_for_command(&plan, &ping, IPUB_ORIGIN).is_ok());
        let debug = format!("{owned:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("nonce-42"));
        assert_ne!(ping.exchange_digest(&plan).unwrap(), [0; 32]);
        assert_eq!(
            owned.exchange_digest().unwrap(),
            browser_event_exchange_digest(&pong_document).unwrap()
        );
        assert_ne!(owned.exchange_digest().unwrap(), [0; 32]);
        assert!(UaiBrowserEventDocument::try_new(String::new()).is_err());
        assert!(
            UaiBrowserEventDocument::try_new("x".repeat(MAX_BROWSER_MESSAGE_BYTES + 1)).is_err()
        );
        assert!(
            UaiBrowserEventDocument::try_from_secret_value(SecretValue::new(vec![0xff])).is_err()
        );
        assert!(browser_event_exchange_digest("").is_err());
        assert!(
            browser_residence_exchange_digest(&"x".repeat(MAX_BROWSER_RESULT_BYTES + 1)).is_err()
        );
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
    fn encrypted_command_artifact_rebinds_every_recovery_authority() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", UCONTENT_ORIGIN, "frame-1")
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
        let fixture =
            include_str!("../../../fixtures/providers/uai/browser/residence-target-command.json")
                .replace("{{target_task_handle}}", &target.entry().handle);
        let fixture_command: UaiBrowserCommandEnvelope = serde_json::from_str(&fixture).unwrap();
        assert_eq!(fixture_command, command);

        let artifact = command.encode_artifact(&plan).unwrap();
        let digest = artifact.digest();
        assert_eq!(digest, command.exchange_digest(&plan).unwrap());
        let debug = format!("{artifact:?}");
        assert!(!debug.contains("nonce-42"));
        assert!(!debug.contains(&target.entry().handle));
        let value = artifact.into_secret_value();
        let restored = UaiBrowserCommandEnvelope::decode_artifact_bound(
            &value,
            digest,
            &plan,
            "nonce-42",
            UCONTENT_ORIGIN,
            "frame-1",
            9,
        )
        .unwrap();
        assert_eq!(restored, command);
        assert!(
            UaiBrowserCommandEnvelope::decode_artifact_bound(
                &value,
                [9; 32],
                &plan,
                "nonce-42",
                UCONTENT_ORIGIN,
                "frame-1",
                9,
            )
            .is_err()
        );
        for (nonce, origin, frame, sequence) in [
            ("nonce-foreign", UCONTENT_ORIGIN, "frame-1", 9),
            ("nonce-42", IPUB_ORIGIN, "frame-1", 9),
            ("nonce-42", UCONTENT_ORIGIN, "frame-foreign", 9),
            ("nonce-42", UCONTENT_ORIGIN, "frame-1", 10),
        ] {
            assert_eq!(
                UaiBrowserCommandEnvelope::decode_artifact_bound(
                    &value, digest, &plan, nonce, origin, frame, sequence,
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::RemoteChanged
            );
        }
        let mut malformed: serde_json::Value =
            serde_json::from_slice(value.expose_secret()).unwrap();
        malformed["unexpected"] = serde_json::json!(true);
        let malformed = asterism_secrets::SecretValue::new(serde_json::to_vec(&malformed).unwrap());
        assert_eq!(
            UaiBrowserCommandEnvelope::decode_artifact_bound(
                &malformed,
                Sha256::digest(malformed.expose_secret()).into(),
                &plan,
                "nonce-42",
                UCONTENT_ORIGIN,
                "frame-1",
                9,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
        let oversized =
            asterism_secrets::SecretValue::new(vec![b'x'; MAX_BROWSER_COMMAND_BYTES + 1]);
        assert!(
            UaiBrowserCommandEnvelope::decode_artifact_bound(
                &oversized,
                Sha256::digest(oversized.expose_secret()).into(),
                &plan,
                "nonce-42",
                UCONTENT_ORIGIN,
                "frame-1",
                9,
            )
            .is_err()
        );
    }

    fn initial_residence_cursor(
        session_nonce: &str,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let courses = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
        let detail = include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
        let tree = include_str!("../../../fixtures/providers/uai/tasks/tree-browser-order.json");
        let course = crate::parse_course_inventory(courses).unwrap().remove(0);
        let context = crate::parse_course_context(&course, detail).unwrap();
        let tasks = crate::parse_task_inventory(&course, &context, tree).unwrap();
        let batch = crate::build_course_residence_batch_plan(
            &course,
            &tasks,
            &browser_runtime_settings(false),
            0,
        )
        .unwrap();

        let mut plan = residence_plan(false);
        plan.target_remote_task_id = "group:2001:unit-z:group-z".to_owned();
        plan.target = UaiBrowserTarget {
            unit: "Unit Z".to_owned(),
            section: Some("Section Z".to_owned()),
            micro: "Micro Z".to_owned(),
            task: "Task Z".to_owned(),
        };
        plan.validate().unwrap();
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, session_nonce, UCONTENT_ORIGIN, "frame-1")
                .unwrap();
        let command = UaiBrowserCommandEnvelope::scan_menu(&plan, &binding, 1).unwrap();
        let cursor = crate::UaiBrowserResidenceCursor::begin(&batch, &plan, &command).unwrap();

        (batch, plan, command, cursor)
    }

    fn completed_event(
        plan: &UaiBrowserResidencePlan,
        session_id: BrowserBridgeSessionId,
        command: &UaiBrowserCommandEnvelope,
        event: UaiBrowserEvent,
        result_digest: [u8; 32],
    ) -> UaiBrowserEventExchangeCompleted {
        let event = UaiBrowserEventEnvelope {
            version: command.version,
            session_nonce: command.session_nonce.clone(),
            origin: command.origin.clone(),
            frame_id: command.frame_id.clone(),
            remote_task_id: command.remote_task_id.clone(),
            reply_to_sequence: command.sequence,
            event,
        };
        event
            .validate_for_command(plan, command, &command.origin)
            .unwrap();
        let issued_at = chrono::Utc::now();
        let mut exchange = BrowserBridgeExchange::issue(
            session_id,
            u64::from(command.sequence),
            UaiBrowserCommandEnvelope::exchange_type().to_owned(),
            command.exchange_digest(plan).unwrap(),
            issued_at,
        )
        .unwrap();
        exchange
            .complete(
                UaiBrowserEventEnvelope::exchange_type().to_owned(),
                result_digest,
                issued_at + chrono::Duration::seconds(1),
            )
            .unwrap();
        UaiBrowserEventExchangeCompleted { event, exchange }
    }

    fn scanning_tabs_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let session_nonce = session_id.to_string();
        let (batch, plan, scan_command, cursor) = initial_residence_cursor(&session_nonce);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, &session_nonce, UCONTENT_ORIGIN, "frame-1")
                .unwrap();
        let target = UaiBrowserMenuEntry::try_new(
            &plan,
            &binding,
            0,
            "Unit Z".to_owned(),
            "Section Z".to_owned(),
            "Micro Z".to_owned(),
        )
        .unwrap();
        let target_handle = target.handle.clone();
        let menu_completed = completed_event(
            &plan,
            session_id,
            &scan_command,
            UaiBrowserEvent::MenuList {
                entries: vec![target],
            },
            [1; 32],
        );
        let menu = cursor
            .advance_menu_list(&batch, &plan, &scan_command, &menu_completed)
            .unwrap();
        let click_completed = completed_event(
            &plan,
            session_id,
            menu.command(),
            UaiBrowserEvent::ClickResult {
                handle: target_handle,
                clicked: true,
            },
            [2; 32],
        );
        let tabs = menu
            .cursor()
            .advance_menu_click(&batch, &plan, menu.command(), &click_completed)
            .unwrap();
        let (cursor, command) = tabs.into_parts();
        (batch, plan, command, cursor)
    }

    fn scanning_direct_tasks_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let (batch, plan, command, cursor) = scanning_tabs_cursor(session_id);
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Tab,
                entries: Vec::new(),
            },
            [3; 32],
        );
        let tasks = cursor
            .advance_tab_list(&batch, &plan, &command, &completed)
            .unwrap();
        let (cursor, command) = tasks.into_parts();
        (batch, plan, command, cursor)
    }

    fn clicking_first_tab_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let (batch, plan, command, cursor) = scanning_tabs_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let tabs = vec![
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Tab,
                0,
                "Overview".to_owned(),
                true,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Tab,
                1,
                "Practice".to_owned(),
                false,
            )
            .unwrap(),
        ];
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Tab,
                entries: tabs,
            },
            [3; 32],
        );
        let clicked = cursor
            .advance_tab_list(&batch, &plan, &command, &completed)
            .unwrap();
        let (cursor, command) = clicked.into_parts();
        (batch, plan, command, cursor)
    }

    fn scanning_first_tab_tasks_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let (batch, plan, command, cursor) = clicking_first_tab_cursor(session_id);
        let handle = match &command.command {
            UaiBrowserCommand::ClickTab { handle } => handle.clone(),
            other => panic!("expected Tab click, got {other:?}"),
        };
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::ClickResult {
                handle,
                clicked: true,
            },
            [4; 32],
        );
        let tasks = cursor
            .advance_tab_click(&batch, &plan, &command, &completed)
            .unwrap();
        let (cursor, command) = tasks.into_parts();
        (batch, plan, command, cursor)
    }

    fn scanning_second_tab_tasks_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let (batch, plan, command, cursor) = scanning_first_tab_tasks_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let task = UaiBrowserPageEntry::try_new(
            &plan,
            &binding,
            UaiBrowserPageScope::Task,
            0,
            "Introduction".to_owned(),
            false,
        )
        .unwrap();
        let listed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Task,
                entries: vec![task],
            },
            [5; 32],
        );
        let next_tab = cursor
            .advance_task_list(&batch, &plan, &command, &listed)
            .unwrap();
        let handle = match &next_tab.command().command {
            UaiBrowserCommand::ClickTab { handle } => handle.clone(),
            other => panic!("expected Tab click, got {other:?}"),
        };
        let clicked = completed_event(
            &plan,
            session_id,
            next_tab.command(),
            UaiBrowserEvent::ClickResult {
                handle,
                clicked: true,
            },
            [6; 32],
        );
        let tasks = next_tab
            .cursor()
            .advance_tab_click(&batch, &plan, next_tab.command(), &clicked)
            .unwrap();
        let (cursor, command) = tasks.into_parts();
        (batch, plan, command, cursor)
    }

    fn clicking_target_task_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let (batch, plan, command, cursor) = scanning_first_tab_tasks_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let tasks = vec![
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                0,
                "Introduction".to_owned(),
                false,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                1,
                "Task Z".to_owned(),
                true,
            )
            .unwrap(),
        ];
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Task,
                entries: tasks,
            },
            [5; 32],
        );
        let clicked = cursor
            .advance_task_list(&batch, &plan, &command, &completed)
            .unwrap();
        let (cursor, command) = clicked.into_parts();
        (batch, plan, command, cursor)
    }

    fn residing_cursor(
        session_id: BrowserBridgeSessionId,
    ) -> (
        crate::UaiCourseResidenceBatchPlan,
        UaiBrowserResidencePlan,
        UaiBrowserCommandEnvelope,
        crate::UaiBrowserResidenceCursor,
    ) {
        let (batch, plan, command, cursor) = clicking_target_task_cursor(session_id);
        let handle = match &command.command {
            UaiBrowserCommand::ClickTask { handle } => handle.clone(),
            other => panic!("expected Task click, got {other:?}"),
        };
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::ClickResult {
                handle,
                clicked: true,
            },
            [6; 32],
        );
        let residence = cursor
            .advance_task_click(&batch, &plan, &command, &completed)
            .unwrap();
        let (cursor, command) = residence.into_parts();
        (batch, plan, command, cursor)
    }

    #[test]
    fn encrypted_accumulated_cursor_artifact_is_stable_redacted_and_recoverable() {
        let (batch, plan, command, cursor) = initial_residence_cursor("nonce-cursor");
        let artifact = cursor.encode_artifact(&batch, &plan, &command).unwrap();
        let digest = artifact.digest();
        assert_eq!(
            cursor
                .encode_artifact(&batch, &plan, &command)
                .unwrap()
                .digest(),
            digest
        );
        let debug = format!("{artifact:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("Unit Z") && !debug.contains("group-z"));

        let value = artifact.into_secret_value();
        let restored = crate::UaiBrowserResidenceCursor::decode_artifact_bound(
            &value, digest, &batch, &plan, &command,
        )
        .unwrap();
        assert_eq!(restored, cursor);
        assert_eq!(
            restored.current_micro_identity_digest(),
            batch.micros()[0].identity_digest()
        );
    }

    #[test]
    fn encrypted_accumulated_cursor_rejects_batch_plan_command_and_schema_drift() {
        let (batch, plan, command, cursor) = initial_residence_cursor("nonce-cursor");
        let artifact = cursor.encode_artifact(&batch, &plan, &command).unwrap();
        let digest = artifact.digest();
        let value = artifact.into_secret_value();
        assert_eq!(
            crate::UaiBrowserResidenceCursor::decode_artifact_bound(
                &value, [9; 32], &batch, &plan, &command,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
        let restarted = batch.restart_at(&batch.restart_target(1).unwrap()).unwrap();
        assert_eq!(
            crate::UaiBrowserResidenceCursor::decode_artifact_bound(
                &value, digest, &restarted, &plan, &command,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );

        let mut changed_plan = plan.clone();
        changed_plan.residence_seconds += 1;
        changed_plan.validate().unwrap();
        assert_eq!(
            crate::UaiBrowserResidenceCursor::decode_artifact_bound(
                &value,
                digest,
                &batch,
                &changed_plan,
                &command,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );

        let changed_binding = UaiBrowserSessionBinding::try_new(
            &plan,
            "nonce-cursor-changed",
            UCONTENT_ORIGIN,
            "frame-1",
        )
        .unwrap();
        let changed_command =
            UaiBrowserCommandEnvelope::scan_menu(&plan, &changed_binding, 1).unwrap();
        assert_eq!(
            crate::UaiBrowserResidenceCursor::decode_artifact_bound(
                &value,
                digest,
                &batch,
                &plan,
                &changed_command,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );

        let mut malformed: serde_json::Value =
            serde_json::from_slice(value.expose_secret()).unwrap();
        malformed["unexpected"] = serde_json::json!(true);
        let malformed = SecretValue::new(serde_json::to_vec(&malformed).unwrap());
        assert_eq!(
            crate::UaiBrowserResidenceCursor::decode_artifact_bound(
                &malformed,
                Sha256::digest(malformed.expose_secret()).into(),
                &batch,
                &plan,
                &command,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );

        for invalid in [
            SecretValue::new(Vec::new()),
            SecretValue::new(vec![b'x'; 256 * 1_024 + 1]),
        ] {
            assert_eq!(
                crate::UaiBrowserResidenceCursor::decode_artifact_bound(
                    &invalid,
                    Sha256::digest(invalid.expose_secret()).into(),
                    &batch,
                    &plan,
                    &command,
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::InvalidResponse
            );
        }
    }

    #[test]
    fn accumulated_cursor_rejects_batch_and_browser_hierarchy_disagreement() {
        let (batch, mut plan, _command, _cursor) = initial_residence_cursor("nonce-cursor");
        plan.target.task = "Task A".to_owned();
        plan.validate().unwrap();
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-cursor", UCONTENT_ORIGIN, "frame-1")
                .unwrap();
        let command = UaiBrowserCommandEnvelope::scan_menu(&plan, &binding, 1).unwrap();
        assert_eq!(
            crate::UaiBrowserResidenceCursor::begin(&batch, &plan, &command)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn cursor_exchange_issuance_atomically_recovers_both_artifacts() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let (batch, _plan, command, cursor) = initial_residence_cursor(&session_nonce);
        let issued_at = chrono::Utc::now();
        let issued = bridge
            .issue_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &batch,
                session_id,
                &cursor,
                command,
                issued_at,
            )
            .await
            .unwrap();
        assert_eq!(
            issued.cursor_artifact().digest(),
            cursor
                .encode_artifact(&batch, &residence_plan_for_group_z(), issued.command())
                .unwrap()
                .digest()
        );
        assert_eq!(
            issued.command_artifact().digest(),
            cursor.next_command_digest()
        );
        assert_eq!(
            issued.exchange().command_digest,
            cursor.next_command_digest()
        );
        let debug = format!("{issued:?}");
        assert!(debug.contains("[REDACTED]") && !debug.contains("Unit Z"));

        let handoff = issued.into_persistence_handoff().unwrap();
        assert_eq!(handoff.exchange().issued_at, issued_at);
        assert_eq!(
            handoff.cursor_state_metadata(),
            &BrowserBridgeRuntimeStateMetadata {
                session_id,
                sequence: 1,
                state_type: UAI_BROWSER_CURSOR_STATE_TYPE.to_owned(),
                state_digest: cursor
                    .encode_artifact(&batch, &residence_plan_for_group_z(), handoff.command())
                    .unwrap()
                    .digest(),
                stored_at: issued_at,
            }
        );
        let handoff_debug = format!("{handoff:?}");
        assert!(handoff_debug.contains("[REDACTED]") && !handoff_debug.contains("Unit Z"));
        let (dispatched, exchange, command_artifact, cursor_state_metadata, cursor_artifact) =
            handoff.into_parts();
        let recovery = UaiBrowserCursorPersistenceRecovery::try_new(
            exchange,
            command_artifact,
            cursor_state_metadata,
            cursor_artifact,
        )
        .unwrap();
        let recovered = bridge
            .recover_persisted_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &batch,
                recovery,
            )
            .await
            .unwrap();
        assert_eq!(recovered.command(), &dispatched);
        assert_eq!(recovered.cursor(), &cursor);
    }

    #[tokio::test]
    async fn cursor_persistence_recovery_rejects_foreign_state_type_before_fresh_reads() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let session_id = BrowserBridgeSessionId::new();
        let (batch, _plan, command, cursor) = initial_residence_cursor(&session_id.to_string());
        let issued = bridge
            .issue_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &batch,
                session_id,
                &cursor,
                command,
                chrono::Utc::now(),
            )
            .await
            .unwrap()
            .into_persistence_handoff()
            .unwrap();
        let (_, exchange, command_artifact, mut metadata, cursor_artifact) = issued.into_parts();
        metadata.state_type = "uai.browser.cursor.v3".to_owned();
        assert_eq!(
            UaiBrowserCursorPersistenceRecovery::try_new(
                exchange,
                command_artifact,
                metadata,
                cursor_artifact,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[tokio::test]
    async fn cursor_exchange_recovery_rejects_another_valid_issued_command() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let (batch, plan, command, cursor) = initial_residence_cursor(&session_nonce);
        let issued_at = chrono::Utc::now();
        let issued = bridge
            .issue_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &batch,
                session_id,
                &cursor,
                command,
                issued_at,
            )
            .await
            .unwrap();
        let (_, _, cursor_artifact, _) = issued.into_parts();
        let cursor_digest = cursor_artifact.digest();

        let foreign_binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &session_nonce,
            UCONTENT_ORIGIN,
            "foreign-frame",
        )
        .unwrap();
        let foreign_command =
            UaiBrowserCommandEnvelope::scan_menu(&plan, &foreign_binding, 1).unwrap();
        let foreign = bridge
            .issue_command_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                session_id,
                foreign_command,
                issued_at,
            )
            .await
            .unwrap();
        let (_, foreign_command_artifact, foreign_exchange) = foreign.into_parts();
        assert_eq!(
            bridge
                .recover_cursor_exchange(
                    &context,
                    "group:2001:unit-z:group-z",
                    &settings,
                    &batch,
                    &foreign_exchange,
                    &foreign_command_artifact.into_secret_value(),
                    &cursor_artifact.into_secret_value(),
                    cursor_digest,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn completed_menu_scan_advances_cursor_to_one_exact_click() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let (batch, plan, command, cursor) = initial_residence_cursor(&session_nonce);
        let issued_at = chrono::Utc::now();
        let issued = bridge
            .issue_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &batch,
                session_id,
                &cursor,
                command,
                issued_at,
            )
            .await
            .unwrap();
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, &session_nonce, UCONTENT_ORIGIN, "frame-1")
                .unwrap();
        let target = UaiBrowserMenuEntry::try_new(
            &plan,
            &binding,
            0,
            "Unit Z".to_owned(),
            "Section Z".to_owned(),
            "Micro Z".to_owned(),
        )
        .unwrap();
        let target_handle = target.handle.clone();
        let document = UaiBrowserEventDocument::try_new(
            serde_json::json!({
                "version": UAI_BROWSER_PLAN_VERSION,
                "session_nonce": session_nonce,
                "origin": UCONTENT_ORIGIN,
                "frame_id": "frame-1",
                "remote_task_id": "group:2001:unit-z:group-z",
                "reply_to_sequence": 1,
                "event": {"kind": "menu_list", "entries": [target]},
            })
            .to_string(),
        )
        .unwrap();
        let result_digest = document.exchange_digest().unwrap();
        let mut completed = bridge
            .complete_event_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                issued.command_issuance(),
                document,
                UCONTENT_ORIGIN,
                issued_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        let advanced = cursor
            .advance_menu_list(&batch, &plan, issued.command(), &completed)
            .unwrap();
        assert_eq!(
            advanced.cursor().stage(),
            crate::UaiBrowserCursorStage::ClickingMenu
        );
        assert_eq!(advanced.cursor().prior_result_sequence(), Some(1));
        assert_eq!(advanced.cursor().prior_result_digest(), Some(result_digest));
        assert!(matches!(
            advanced.command().command,
            UaiBrowserCommand::ClickMenu { ref handle } if *handle == target_handle
        ));
        assert_eq!(advanced.command().sequence, 2);
        assert_eq!(
            advanced.cursor().next_command_digest(),
            advanced.command().exchange_digest(&plan).unwrap()
        );
        assert_eq!(cursor.stage(), crate::UaiBrowserCursorStage::ScanningMenu);
        assert_eq!(cursor.prior_result_digest(), None);
        advanced
            .cursor()
            .encode_artifact(&batch, &plan, advanced.command())
            .unwrap();

        completed.exchange.command_digest = [7; 32];
        assert_eq!(
            cursor
                .advance_menu_list(&batch, &plan, issued.command(), &completed)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn completed_menu_click_advances_cursor_to_tab_scan() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_tabs_cursor(session_id);
        assert_eq!(cursor.stage(), crate::UaiBrowserCursorStage::ScanningTabs);
        assert_eq!(cursor.prior_result_sequence(), Some(2));
        assert_eq!(cursor.prior_result_digest(), Some([2; 32]));
        assert_eq!(command.sequence, 3);
        assert!(matches!(
            command.command,
            UaiBrowserCommand::ScanPage {
                scope: UaiBrowserPageScope::Tab
            }
        ));
        cursor.encode_artifact(&batch, &plan, &command).unwrap();
    }

    #[test]
    fn completed_tab_scan_freezes_snapshot_and_clicks_first_tab() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_tabs_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let tabs = vec![
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Tab,
                0,
                "Overview".to_owned(),
                true,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Tab,
                1,
                "Practice".to_owned(),
                false,
            )
            .unwrap(),
        ];
        let first_handle = tabs[0].handle.clone();
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Tab,
                entries: tabs.clone(),
            },
            [3; 32],
        );
        let advanced = cursor
            .advance_tab_list(&batch, &plan, &command, &completed)
            .unwrap();

        assert_eq!(
            advanced.cursor().stage(),
            crate::UaiBrowserCursorStage::ClickingTab
        );
        assert_eq!(advanced.cursor().tab_snapshot(), tabs);
        assert_eq!(advanced.cursor().current_tab_ordinal(), Some(0));
        assert_eq!(advanced.cursor().next_tab_ordinal(), 1);
        assert_eq!(advanced.cursor().prior_result_digest(), Some([3; 32]));
        assert_eq!(advanced.command().sequence, 4);
        assert!(matches!(
            advanced.command().command,
            UaiBrowserCommand::ClickTab { ref handle } if *handle == first_handle
        ));
        advanced
            .cursor()
            .encode_artifact(&batch, &plan, advanced.command())
            .unwrap();
    }

    #[test]
    fn empty_tab_scan_advances_directly_to_task_scan() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_tabs_cursor(session_id);
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Tab,
                entries: Vec::new(),
            },
            [3; 32],
        );
        let advanced = cursor
            .advance_tab_list(&batch, &plan, &command, &completed)
            .unwrap();

        assert_eq!(
            advanced.cursor().stage(),
            crate::UaiBrowserCursorStage::ScanningTasks
        );
        assert!(advanced.cursor().tab_snapshot().is_empty());
        assert_eq!(advanced.cursor().current_tab_ordinal(), None);
        assert_eq!(advanced.cursor().next_tab_ordinal(), 0);
        assert!(matches!(
            advanced.command().command,
            UaiBrowserCommand::ScanPage {
                scope: UaiBrowserPageScope::Task
            }
        ));
    }

    #[test]
    fn completed_tab_click_advances_cursor_to_current_task_scan() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = clicking_first_tab_cursor(session_id);
        let handle = match &command.command {
            UaiBrowserCommand::ClickTab { handle } => handle.clone(),
            other => panic!("expected Tab click, got {other:?}"),
        };
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::ClickResult {
                handle,
                clicked: true,
            },
            [4; 32],
        );
        let advanced = cursor
            .advance_tab_click(&batch, &plan, &command, &completed)
            .unwrap();

        assert_eq!(
            advanced.cursor().stage(),
            crate::UaiBrowserCursorStage::ScanningTasks
        );
        assert_eq!(advanced.cursor().tab_snapshot().len(), 2);
        assert_eq!(advanced.cursor().current_tab_ordinal(), Some(0));
        assert_eq!(advanced.cursor().next_tab_ordinal(), 1);
        assert!(advanced.cursor().task_snapshot().is_empty());
        assert_eq!(advanced.cursor().processed_tabs(), 0);
        assert_eq!(advanced.cursor().prior_result_digest(), Some([4; 32]));
        assert_eq!(advanced.command().sequence, 5);
        assert!(matches!(
            advanced.command().command,
            UaiBrowserCommand::ScanPage {
                scope: UaiBrowserPageScope::Task
            }
        ));
        assert_eq!(cursor.stage(), crate::UaiBrowserCursorStage::ClickingTab);
    }

    #[test]
    fn completed_task_scan_freezes_snapshot_and_clicks_unique_target() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_first_tab_tasks_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let tasks = vec![
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                0,
                "Introduction".to_owned(),
                false,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                1,
                "Task Z".to_owned(),
                true,
            )
            .unwrap(),
        ];
        let target_handle = tasks[1].handle.clone();
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Task,
                entries: tasks.clone(),
            },
            [5; 32],
        );
        let advanced = cursor
            .advance_task_list(&batch, &plan, &command, &completed)
            .unwrap();

        assert_eq!(
            advanced.cursor().stage(),
            crate::UaiBrowserCursorStage::ClickingTask
        );
        assert_eq!(advanced.cursor().task_snapshot(), tasks);
        assert_eq!(advanced.cursor().processed_tabs(), 1);
        assert_eq!(advanced.cursor().current_tab_ordinal(), Some(0));
        assert_eq!(advanced.cursor().next_tab_ordinal(), 1);
        assert_eq!(advanced.cursor().prior_result_digest(), Some([5; 32]));
        assert!(matches!(
            advanced.command().command,
            UaiBrowserCommand::ClickTask { ref handle } if *handle == target_handle
        ));
    }

    #[test]
    fn task_scan_without_target_advances_to_next_frozen_tab() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_first_tab_tasks_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let task = UaiBrowserPageEntry::try_new(
            &plan,
            &binding,
            UaiBrowserPageScope::Task,
            0,
            "Introduction".to_owned(),
            false,
        )
        .unwrap();
        let next_tab_handle = cursor.tab_snapshot()[1].handle.clone();
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Task,
                entries: vec![task],
            },
            [5; 32],
        );
        let advanced = cursor
            .advance_task_list(&batch, &plan, &command, &completed)
            .unwrap();

        assert_eq!(
            advanced.cursor().stage(),
            crate::UaiBrowserCursorStage::ClickingTab
        );
        assert!(advanced.cursor().task_snapshot().is_empty());
        assert_eq!(advanced.cursor().processed_tabs(), 1);
        assert_eq!(advanced.cursor().current_tab_ordinal(), Some(1));
        assert_eq!(advanced.cursor().next_tab_ordinal(), 2);
        assert!(matches!(
            advanced.command().command,
            UaiBrowserCommand::ClickTab { ref handle } if *handle == next_tab_handle
        ));
    }

    #[test]
    fn final_task_scan_without_target_fails_closed() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_second_tab_tasks_cursor(session_id);
        let completed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Task,
                entries: Vec::new(),
            },
            [7; 32],
        );
        assert_eq!(
            cursor
                .advance_task_list(&batch, &plan, &command, &completed)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(cursor.current_tab_ordinal(), Some(1));
        assert_eq!(cursor.next_tab_ordinal(), 2);
        assert_eq!(cursor.processed_tabs(), 1);
    }

    #[test]
    fn completed_task_click_uses_runtime_batch_leaf_budget() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = residing_cursor(session_id);
        let expected_seconds = batch
            .budget_share()
            .unwrap()
            .rounded_leaf_seconds(2, 2)
            .unwrap();
        assert_eq!(expected_seconds, 100);
        assert_eq!(cursor.stage(), crate::UaiBrowserCursorStage::Residing);
        assert_eq!(cursor.processed_tabs(), 1);
        assert_eq!(cursor.processed_tasks(), 1);
        assert_eq!(cursor.prior_result_sequence(), Some(6));
        assert_eq!(cursor.prior_result_digest(), Some([6; 32]));
        assert_eq!(cursor.remaining_active_seconds(), 1_200);
        assert!(matches!(
            command.command,
            UaiBrowserCommand::ResidenceTarget {
                seconds,
                play_video: false,
                ..
            } if seconds == expected_seconds
        ));
        assert_eq!(command.sequence, 7);
        assert_ne!(expected_seconds, plan.residence_seconds);
        cursor.encode_artifact(&batch, &plan, &command).unwrap();
    }

    #[test]
    fn direct_task_page_uses_task_only_leaf_denominator() {
        let session_id = BrowserBridgeSessionId::new();
        let (batch, plan, command, cursor) = scanning_direct_tasks_cursor(session_id);
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &command.session_nonce,
            &command.origin,
            &command.frame_id,
        )
        .unwrap();
        let tasks = vec![
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                0,
                "Introduction".to_owned(),
                false,
            )
            .unwrap(),
            UaiBrowserPageEntry::try_new(
                &plan,
                &binding,
                UaiBrowserPageScope::Task,
                1,
                "Task Z".to_owned(),
                true,
            )
            .unwrap(),
        ];
        let listed = completed_event(
            &plan,
            session_id,
            &command,
            UaiBrowserEvent::PageList {
                scope: UaiBrowserPageScope::Task,
                entries: tasks,
            },
            [4; 32],
        );
        let clicked = cursor
            .advance_task_list(&batch, &plan, &command, &listed)
            .unwrap();
        let handle = match &clicked.command().command {
            UaiBrowserCommand::ClickTask { handle } => handle.clone(),
            other => panic!("expected Task click, got {other:?}"),
        };
        let completed = completed_event(
            &plan,
            session_id,
            clicked.command(),
            UaiBrowserEvent::ClickResult {
                handle,
                clicked: true,
            },
            [5; 32],
        );
        let residence = clicked
            .cursor()
            .advance_task_click(&batch, &plan, clicked.command(), &completed)
            .unwrap();
        let expected_seconds = batch
            .budget_share()
            .unwrap()
            .rounded_leaf_seconds(0, 2)
            .unwrap();

        assert_eq!(expected_seconds, 200);
        assert_eq!(residence.cursor().processed_tabs(), 0);
        assert_eq!(residence.cursor().processed_tasks(), 1);
        assert!(matches!(
            residence.command().command,
            UaiBrowserCommand::ResidenceTarget { seconds: 200, .. }
        ));
    }

    #[tokio::test]
    async fn leaf_budget_can_only_be_issued_with_its_accumulated_cursor() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let session_id = BrowserBridgeSessionId::new();
        let (_batch, _plan, command, _cursor) = residing_cursor(session_id);
        let issued_at = chrono::Utc::now();
        assert_eq!(
            bridge
                .issue_command_exchange(
                    &context,
                    "group:2001:unit-z:group-z",
                    &settings,
                    session_id,
                    command,
                    issued_at,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let (fresh_batch, _plan, fresh_command, fresh_cursor) = residing_cursor(session_id);
        let issued = bridge
            .issue_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &fresh_batch,
                session_id,
                &fresh_cursor,
                fresh_command,
                issued_at,
            )
            .await
            .unwrap();
        assert_eq!(issued.exchange().sequence, 7);
        assert_eq!(
            issued.cursor_artifact().digest(),
            fresh_cursor
                .encode_artifact(
                    &fresh_batch,
                    &residence_plan_for_group_z(),
                    issued.command(),
                )
                .unwrap()
                .digest()
        );
    }

    #[tokio::test]
    async fn terminal_residence_checkpoint_accounts_exact_leaf_and_requires_readback() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let (batch, plan, command, cursor) = residing_cursor(session_id);
        let task_handle = match &command.command {
            UaiBrowserCommand::ResidenceTarget { task_handle, .. } => task_handle.clone(),
            other => panic!("expected residence target, got {other:?}"),
        };
        let completed_at = chrono::Utc::now();
        let issued_at = completed_at - chrono::Duration::seconds(100);
        let issued = bridge
            .issue_cursor_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                &batch,
                session_id,
                &cursor,
                command,
                issued_at,
            )
            .await
            .unwrap();
        let document = UaiBrowserResidenceResultDocument::try_new(
            serde_json::json!({
                "version": UAI_BROWSER_PLAN_VERSION,
                "session_nonce": session_nonce,
                "origin": UCONTENT_ORIGIN,
                "frame_id": "frame-1",
                "remote_task_id": "group:2001:unit-z:group-z",
                "reply_to_sequence": 7,
                "target_task_handle": task_handle,
                "planned_residence_seconds": 100,
                "observed_active_seconds": 100,
                "processed_micros": 1,
                "processed_tabs": 1,
                "processed_tasks": 1,
                "video_seconds": 0,
                "cancelled": false,
                "last_label": "Task Z",
            })
            .to_string(),
        )
        .unwrap();
        let result_digest = document.exchange_digest().unwrap();
        let mut completed = bridge
            .complete_residence_exchange(
                &context,
                "group:2001:unit-z:group-z",
                &settings,
                issued.command_issuance(),
                document,
                UCONTENT_ORIGIN,
                completed_at,
            )
            .await
            .unwrap();
        let checkpoint = cursor
            .complete_residence(&batch, &plan, issued.command(), &completed)
            .unwrap();
        assert_eq!(checkpoint.completed_command_sequence(), 7);
        assert_eq!(checkpoint.result_digest(), result_digest);
        assert_eq!(checkpoint.remote_task_id(), "group:2001:unit-z:group-z");
        assert_eq!(checkpoint.completed_at(), completed_at);
        assert_eq!(checkpoint.remaining_active_seconds(), 1_100);
        assert_eq!(checkpoint.observed_video_seconds(), 0);
        assert_eq!(checkpoint.processed_micros(), 1);
        assert_eq!(checkpoint.processed_tabs(), 1);
        assert_eq!(checkpoint.processed_tasks(), 1);
        assert!(checkpoint.requires_fresh_duration_read());

        assert_fresh_duration_readback(
            &context,
            &batch,
            &plan,
            &checkpoint,
            result_digest,
            completed_at,
        )
        .await;

        completed.result.observed_active_seconds = 99;
        assert_eq!(
            cursor
                .complete_residence(&batch, &plan, issued.command(), &completed)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    async fn assert_fresh_duration_readback(
        context: &ProviderContext,
        batch: &UaiCourseResidenceBatchPlan,
        plan: &UaiBrowserResidencePlan,
        checkpoint: &crate::UaiBrowserResidenceCheckpoint,
        result_digest: [u8; 32],
        completed_at: Timestamp,
    ) {
        let readback = UaiTaskDuration::try_new(Arc::new(FixtureDuration))
            .unwrap()
            .read_browser_residence_readback(context, batch, plan, checkpoint)
            .await
            .unwrap();
        assert_eq!(readback.batch_plan_digest(), batch.plan_digest());
        assert_eq!(
            readback.browser_plan_digest(),
            checkpoint.browser_plan_digest()
        );
        assert_eq!(readback.residence_result_digest(), result_digest);
        assert_eq!(readback.remote_task_id(), checkpoint.remote_task_id());
        assert_eq!(readback.residence_completed_at(), completed_at);
        assert!(readback.study_record().observed_at() >= completed_at);
        assert_eq!(readback.study_record().duration_seconds(), 700);
        assert_eq!(
            readback.study_record().finish_progress_percent(),
            Some(50.0)
        );

        let mut foreign_plan = plan.clone();
        foreign_plan.target_remote_task_id = "group:2001:unit-1:group-1".to_owned();
        assert_eq!(
            UaiTaskDuration::try_new(Arc::new(FixtureDuration))
                .unwrap()
                .read_browser_residence_readback(context, batch, &foreign_plan, checkpoint)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn issued_event_exchange_recovers_only_the_persisted_command() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let plan = residence_plan(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &session_nonce,
            UCONTENT_ORIGIN,
            "content-frame",
        )
        .unwrap();
        let command = UaiBrowserCommandEnvelope::ping(&plan, &binding, 7).unwrap();
        let issued_at = chrono::Utc::now();
        let issued = bridge
            .issue_command_exchange(
                &context,
                "group:2001:unit-1:group-1",
                &settings,
                session_id,
                command,
                issued_at,
            )
            .await
            .unwrap();
        assert_eq!(issued.exchange().state, BrowserBridgeExchangeState::Issued);
        assert_eq!(issued.exchange().sequence, 7);
        assert_eq!(
            issued.exchange().command_digest,
            issued.command_artifact().digest()
        );

        let event_fixture = include_str!("../../../fixtures/providers/uai/browser/pong-event.json")
            .replace("{{session_nonce}}", &session_nonce);
        let in_memory = bridge
            .complete_event_exchange(
                &context,
                "group:2001:unit-1:group-1",
                &settings,
                &issued,
                UaiBrowserEventDocument::try_new(event_fixture.clone()).unwrap(),
                UCONTENT_ORIGIN,
                issued_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        assert!(matches!(in_memory.event().event, UaiBrowserEvent::Pong));
        assert_eq!(
            in_memory.exchange().result_type.as_deref(),
            Some(UAI_BROWSER_EVENT_TYPE)
        );

        let (_dispatched, artifact, exchange) = issued.into_parts();
        let artifact = artifact.into_secret_value();
        let received_at = issued_at + chrono::Duration::seconds(2);
        let metadata = BrowserBridgeResultArtifactMetadata {
            session_id,
            sequence: 7,
            result_type: UAI_BROWSER_EVENT_TYPE.to_owned(),
            result_digest: browser_event_exchange_digest(&event_fixture).unwrap(),
            received_at,
        };
        let mut wrong_type = metadata.clone();
        wrong_type.result_type = UAI_BROWSER_RESIDENCE_RESULT_TYPE.to_owned();
        assert_eq!(
            UaiBrowserEventInbox::try_new(
                &exchange,
                wrong_type,
                SecretValue::new(event_fixture.clone().into_bytes()),
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
        let inbox = UaiBrowserEventInbox::try_new(
            &exchange,
            metadata,
            SecretValue::new(event_fixture.into_bytes()),
        )
        .unwrap();
        assert_eq!(inbox.metadata().received_at, received_at);
        let (document, metadata) = inbox.into_parts();
        let recovered = bridge
            .complete_recovered_event_exchange(
                &context,
                "group:2001:unit-1:group-1",
                &settings,
                &exchange,
                &artifact,
                document,
                UCONTENT_ORIGIN,
                metadata.received_at,
            )
            .await
            .unwrap();
        assert!(matches!(recovered.event().event, UaiBrowserEvent::Pong));
        assert_eq!(
            recovered.exchange().state,
            BrowserBridgeExchangeState::Completed
        );
    }

    #[tokio::test]
    async fn recovered_event_cannot_select_a_different_persisted_command() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let plan = residence_plan(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let binding = UaiBrowserSessionBinding::try_new(
            &plan,
            &session_nonce,
            UCONTENT_ORIGIN,
            "content-frame",
        )
        .unwrap();
        let issued_at = chrono::Utc::now();
        let scan = UaiBrowserCommandEnvelope::scan_menu(&plan, &binding, 8).unwrap();
        let foreign = bridge
            .issue_command_exchange(
                &context,
                "group:2001:unit-1:group-1",
                &settings,
                session_id,
                scan,
                issued_at,
            )
            .await
            .unwrap();
        let (_, foreign_artifact, foreign_exchange) = foreign.into_parts();
        let wrong_event = include_str!("../../../fixtures/providers/uai/browser/pong-event.json")
            .replace("{{session_nonce}}", &session_nonce)
            .replace("\"reply_to_sequence\": 7", "\"reply_to_sequence\": 8");
        assert_eq!(
            bridge
                .complete_recovered_event_exchange(
                    &context,
                    "group:2001:unit-1:group-1",
                    &settings,
                    &foreign_exchange,
                    &foreign_artifact.into_secret_value(),
                    UaiBrowserEventDocument::try_new(wrong_event).unwrap(),
                    UCONTENT_ORIGIN,
                    issued_at + chrono::Duration::seconds(1),
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[tokio::test]
    async fn recovered_residence_exchange_binds_terminal_result_and_stays_verify_only() {
        let bridge = browser_bridge();
        let context = provider_context();
        let settings = browser_runtime_settings(false);
        let plan = residence_plan(false);
        let session_id = BrowserBridgeSessionId::new();
        let session_nonce = session_id.to_string();
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, &session_nonce, UCONTENT_ORIGIN, "top-frame")
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
        let issued_at = chrono::Utc::now();
        let issued = bridge
            .issue_command_exchange(
                &context,
                "group:2001:unit-1:group-1",
                &settings,
                session_id,
                command,
                issued_at,
            )
            .await
            .unwrap();
        let (_, artifact, exchange) = issued.into_parts();
        let fixture =
            include_str!("../../../fixtures/providers/uai/browser/residence-target-result.json")
                .replace("{{session_nonce}}", &session_nonce)
                .replace("{{target_task_handle}}", &target.entry().handle);
        let result_digest = browser_residence_exchange_digest(&fixture).unwrap();
        let received_at = issued_at + chrono::Duration::seconds(1);
        let metadata = BrowserBridgeResultArtifactMetadata {
            session_id,
            sequence: 9,
            result_type: UAI_BROWSER_RESIDENCE_RESULT_TYPE.to_owned(),
            result_digest,
            received_at,
        };
        let mut wrong_digest = metadata.clone();
        wrong_digest.result_digest = [9; 32];
        assert_eq!(
            UaiBrowserResidenceInbox::try_new(
                &exchange,
                wrong_digest,
                SecretValue::new(fixture.clone().into_bytes()),
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
        let inbox = UaiBrowserResidenceInbox::try_new(
            &exchange,
            metadata,
            SecretValue::new(fixture.into_bytes()),
        )
        .unwrap();
        assert_eq!(inbox.metadata().result_digest, result_digest);
        let (document, metadata) = inbox.into_parts();
        let completed = bridge
            .complete_recovered_residence_exchange(
                &context,
                "group:2001:unit-1:group-1",
                &settings,
                &exchange,
                &artifact.into_secret_value(),
                document,
                UCONTENT_ORIGIN,
                metadata.received_at,
            )
            .await
            .unwrap();
        assert!(completed.result().requires_fresh_duration_read());
        assert_eq!(
            completed.exchange().state,
            BrowserBridgeExchangeState::Completed
        );
        assert_eq!(
            completed.exchange().result_type.as_deref(),
            Some(UAI_BROWSER_RESIDENCE_RESULT_TYPE)
        );
        assert_eq!(completed.exchange().result_digest, Some(result_digest));
    }

    #[test]
    fn residence_controls_preserve_target_budget_and_bound_restart() {
        let plan = residence_plan(false);
        let binding =
            UaiBrowserSessionBinding::try_new(&plan, "nonce-42", UCONTENT_ORIGIN, "frame-1")
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
        let command = UaiBrowserCommandEnvelope::residence_control(
            &plan,
            &binding,
            8,
            &target,
            UaiBrowserResidenceControl::Restart {
                start_micro_ordinal: 3,
            },
        )
        .unwrap();
        for control in [
            UaiBrowserResidenceControl::Pause,
            UaiBrowserResidenceControl::Resume,
        ] {
            let control_command =
                UaiBrowserCommandEnvelope::residence_control(&plan, &binding, 7, &target, control)
                    .unwrap();
            assert!(matches!(
                control_command.command,
                UaiBrowserCommand::ResidenceControl { seconds: 1_200, .. }
            ));
        }
        let document =
            include_str!("../../../fixtures/providers/uai/browser/residence-control-restart.json")
                .replace("{{target_task_handle}}", &target.entry().handle);
        assert!(parse_browser_event(&document, &plan, &command, UCONTENT_ORIGIN).is_ok());
        assert!(
            parse_browser_event(
                &document.replace("\"accepted\": true", "\"accepted\": false"),
                &plan,
                &command,
                UCONTENT_ORIGIN,
            )
            .is_err()
        );
        assert!(
            UaiBrowserCommandEnvelope::residence_control(
                &plan,
                &binding,
                9,
                &target,
                UaiBrowserResidenceControl::Restart {
                    start_micro_ordinal: plan.max_discovered_micros,
                },
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
            "version": UAI_BROWSER_PLAN_VERSION,
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
        let settings = browser_runtime_settings(play_video);
        residence_plan_from_detail(
            &fixture_remote_detail("group:2001:unit-1:group-1", true),
            &settings,
        )
        .unwrap()
    }

    fn residence_plan_for_group_z() -> UaiBrowserResidencePlan {
        let settings = browser_runtime_settings(false);
        residence_plan_from_detail(
            &fixture_remote_detail("group:2001:unit-z:group-z", true),
            &settings,
        )
        .unwrap()
    }

    fn browser_runtime_settings(
        play_video: bool,
    ) -> asterism_provider_api::ResolvedProviderRuntimeSettings {
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
        schema.resolve(None, None, Some(&patch)).unwrap()
    }

    fn browser_bridge() -> UaiBrowserBridge {
        UaiBrowserBridge::try_new(Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised: true,
        }))
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
