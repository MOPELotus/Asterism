use std::fmt;

use asterism_domain::{BrowserBridgeSessionId, SessionKind};
use asterism_provider_api::{
    CredentialReplacement, ProviderError, ProviderErrorKind, ProviderResult,
};
use asterism_secrets::{CredentialField, SecretPurpose, SecretValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::CidarenCryptoContext;

const CIDAREN_ORIGIN: &str = "https://app.vocabgo.com";
const MAX_BROWSER_COMMAND_BYTES: usize = 4 * 1_024;
const MAX_BROWSER_DOCUMENT_BYTES: usize = 256 * 1_024;
const MAX_SESSION_NONCE_BYTES: usize = 512;
const MAX_FRAME_ID_BYTES: usize = 256;
const MAX_REMOTE_TASK_ID_BYTES: usize = 768;
const MAX_CAPTURE_VALUE_BYTES: usize = 64 * 1_024;

/// Stable Core `BrowserBridge` exchange type for one Cidaren Capture command.
pub const CIDAREN_CAPTURE_COMMAND_TYPE: &str = "cidaren.capture.snapshot";
/// Stable Core `BrowserBridge` exchange type for one Cidaren Capture result.
pub const CIDAREN_CAPTURE_RESULT_TYPE: &str = "cidaren.capture.snapshot.result";

/// Encrypted-at-rest command material required to validate a helper result
/// after process recovery. The digest remains safe for the ordinary exchange
/// ledger; only Core's encrypted artifact boundary may persist the value.
pub struct EncodedCidarenBrowserCommandArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedCidarenBrowserCommandArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedCidarenBrowserCommandArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedCidarenBrowserCommandArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

/// Owned raw Capture result delivered by the shared helper boundary.
///
/// The document can contain the account token and current crypto context, so
/// the high-level `BrowserBridge` adapter consumes this zeroizing owner instead
/// of borrowing an ordinary `String` whose cleanup it cannot enforce.
pub(crate) struct CidarenBrowserResultDocument(Zeroizing<String>);

impl CidarenBrowserResultDocument {
    /// Takes ownership of one bounded raw helper result.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty or oversized document.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        let document = Zeroizing::new(document);
        if document.is_empty() || document.len() > MAX_BROWSER_DOCUMENT_BYTES {
            return Err(invalid_response(
                "Cidaren BrowserBridge result is empty or oversized",
            ));
        }
        Ok(Self(document))
    }

    /// Copies one Core-resolved encrypted result into the Provider's bounded,
    /// zeroizing UTF-8 owner. The source remains owned and cleared by Core;
    /// this copy is consumed by Cidaren parsing and cleared on every exit path.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty, oversized or non-UTF-8 artifact, or
    /// protocol drift when its exact bytes no longer match Core's persisted
    /// digest.
    pub fn try_from_secret_value(
        value: &SecretValue,
        expected_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let mut bytes = Zeroizing::new(value.expose_secret().to_vec());
        if bytes.is_empty() || bytes.len() > MAX_BROWSER_DOCUMENT_BYTES {
            return Err(invalid_response(
                "Cidaren BrowserBridge result artifact is empty or oversized",
            ));
        }
        let actual_digest: [u8; 32] = Sha256::digest(bytes.as_slice()).into();
        if actual_digest != expected_digest {
            return Err(protocol_drift(
                "Cidaren persisted Capture result digest changed",
            ));
        }
        let document = match String::from_utf8(std::mem::take(&mut *bytes)) {
            Ok(document) => document,
            Err(error) => {
                let _invalid = Zeroizing::new(error.into_bytes());
                return Err(invalid_response(
                    "Cidaren BrowserBridge result artifact is not UTF-8",
                ));
            }
        };
        Self::try_new(document)
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Hashes the exact owned result for Core's durable exchange record.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the document no longer satisfies the Provider
    /// transport bound.
    pub(crate) fn exchange_digest(&self) -> ProviderResult<[u8; 32]> {
        browser_event_exchange_digest(self.as_str())
    }
}

impl fmt::Debug for CidarenBrowserResultDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenBrowserResultDocument")
            .field("bytes", &self.0.len())
            .field("contents", &"[REDACTED]")
            .finish()
    }
}

/// Selects one audited Capture recipe without combining its output with any
/// other recipe. Version 1 is the public donor's token-only proxy path;
/// version 2 is the Composite storage/crypto path.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CidarenCaptureMode {
    TokenOnly,
    Composite,
}

impl CidarenCaptureMode {
    const fn recipe_version(self) -> u32 {
        match self {
            Self::TokenOnly => 1,
            Self::Composite => 2,
        }
    }
}

/// One exact audited browser source a Cidaren helper may inspect.
///
/// Unit variants deliberately carry no selector, script, origin or arbitrary
/// key. The dispatch origin is validated independently, while each variant
/// identifies one frozen donor-observed header/storage fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CidarenBrowserCaptureSource {
    RequestHeaderUserToken,
    LocalStorageUserToken,
    SessionStorageUserToken,
    LocalStorageLoginInfo,
    SessionStorageLoginInfo,
}

/// Closed helper action set projected from one authenticated command artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CidarenBrowserHelperAction {
    CaptureSnapshotTokenOnly,
    CaptureSnapshotComposite,
}

/// Secret-free helper projection for one exact Cidaren Capture action.
///
/// This contains no command echo, selector or executable script. Source lists
/// are static and selected only by the artifact's validated recipe revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CidarenBrowserHelperProjection {
    action: CidarenBrowserHelperAction,
}

impl CidarenBrowserHelperProjection {
    pub const fn action(self) -> CidarenBrowserHelperAction {
        self.action
    }

    pub const fn user_token_sources(self) -> &'static [CidarenBrowserCaptureSource] {
        match self.action {
            CidarenBrowserHelperAction::CaptureSnapshotTokenOnly => {
                &[CidarenBrowserCaptureSource::RequestHeaderUserToken]
            }
            CidarenBrowserHelperAction::CaptureSnapshotComposite => &[
                CidarenBrowserCaptureSource::RequestHeaderUserToken,
                CidarenBrowserCaptureSource::LocalStorageUserToken,
                CidarenBrowserCaptureSource::SessionStorageUserToken,
            ],
        }
    }

    pub const fn login_info_sources(self) -> &'static [CidarenBrowserCaptureSource] {
        match self.action {
            CidarenBrowserHelperAction::CaptureSnapshotTokenOnly => &[],
            CidarenBrowserHelperAction::CaptureSnapshotComposite => &[
                CidarenBrowserCaptureSource::LocalStorageLoginInfo,
                CidarenBrowserCaptureSource::SessionStorageLoginInfo,
            ],
        }
    }
}

/// Source of a captured `UserToken`. The source is descriptive only; the
/// Core Capture recipe remains the authority for which source is allowed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CidarenCaptureTokenSource {
    RequestHeader,
    LocalStorage,
    SessionStorage,
}

/// Source of the optional Composite login-info object.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CidarenCaptureStorageSource {
    LocalStorage,
    SessionStorage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CidarenBrowserCommand {
    /// Reads one same-origin browser snapshot containing only the declared
    /// Cidaren storage/header facts. No arbitrary script or selector crosses
    /// the Provider boundary.
    CaptureSnapshot { mode: CidarenCaptureMode },
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CidarenBrowserCommandEnvelope {
    pub version: u32,
    pub session_nonce: String,
    pub origin: String,
    pub frame_id: String,
    pub remote_task_id: String,
    pub sequence: u32,
    pub command: CidarenBrowserCommand,
}

impl CidarenBrowserCommandEnvelope {
    /// Returns the stable durable exchange type used by Core.
    pub const fn exchange_type() -> &'static str {
        CIDAREN_CAPTURE_COMMAND_TYPE
    }

    /// Hashes the canonical typed command envelope for Core's durable exchange
    /// record. The command contains no captured credential material.
    ///
    /// # Errors
    ///
    /// Returns a typed error if serialization unexpectedly fails.
    pub fn exchange_digest(&self) -> ProviderResult<[u8; 32]> {
        Ok(self.encode_artifact()?.digest())
    }

    /// Encodes the exact issued command for Core's encrypted recovery
    /// boundary. The serialized session nonce is owned by a zeroizing secret
    /// value and never enters the ordinary exchange row.
    ///
    /// # Errors
    ///
    /// Returns a typed error if command validation or serialization fails.
    pub fn encode_artifact(&self) -> ProviderResult<EncodedCidarenBrowserCommandArtifact> {
        self.validate()?;
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(self)
                .map_err(|_| invalid_response("Cidaren Capture command cannot be encoded"))?,
        );
        if encoded.len() > MAX_BROWSER_COMMAND_BYTES {
            return Err(invalid_response(
                "Cidaren Capture command exceeds its recovery bound",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedCidarenBrowserCommandArtifact { value, digest })
    }

    /// Resolves one encrypted command artifact and repeats every independent
    /// Core binding before a helper result can be accepted after recovery.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest/schema drift or a foreign
    /// session/Task/sequence/recipe binding.
    pub fn decode_artifact_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_session_id: BrowserBridgeSessionId,
        expected_remote_task_id: &str,
        expected_sequence: u64,
        expected_mode: CidarenCaptureMode,
    ) -> ProviderResult<Self> {
        let command = Self::decode_artifact(value, expected_digest)?;
        let expected_sequence = u32::try_from(expected_sequence).map_err(|_| {
            protocol_drift("Cidaren Capture command artifact sequence is unrepresentable")
        })?;
        if command.session_nonce != expected_session_id.to_string()
            || command.remote_task_id != expected_remote_task_id
            || command.sequence != expected_sequence
            || command.command
                != (CidarenBrowserCommand::CaptureSnapshot {
                    mode: expected_mode,
                })
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Cidaren Capture command artifact binding is stale or foreign",
            ));
        }
        Ok(command)
    }

    fn decode_artifact(value: &SecretValue, expected_digest: [u8; 32]) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_BROWSER_COMMAND_BYTES {
            return Err(invalid_response(
                "Cidaren Capture command artifact is empty or oversized",
            ));
        }
        if Sha256::digest(bytes).as_slice() != expected_digest {
            return Err(protocol_drift(
                "Cidaren Capture command artifact digest changed",
            ));
        }
        let command: Self = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Cidaren Capture command artifact schema changed"))?;
        command.validate()?;
        Ok(command)
    }

    /// Builds a recipe-versioned Capture snapshot command.
    ///
    /// # Errors
    ///
    /// Returns a typed error when any session, frame, Task or sequence
    /// binding is malformed.
    pub fn capture_snapshot(
        session_nonce: String,
        frame_id: String,
        remote_task_id: String,
        sequence: u32,
        mode: CidarenCaptureMode,
    ) -> ProviderResult<Self> {
        let envelope = Self {
            version: mode.recipe_version(),
            session_nonce,
            origin: CIDAREN_ORIGIN.to_owned(),
            frame_id,
            remote_task_id,
            sequence,
            command: CidarenBrowserCommand::CaptureSnapshot { mode },
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// # Errors
    ///
    /// Returns a typed error when the command binding or recipe version is
    /// invalid.
    pub fn validate(&self) -> ProviderResult<()> {
        if self.version == 0
            || self.origin != CIDAREN_ORIGIN
            || !valid_token(&self.session_nonce, MAX_SESSION_NONCE_BYTES)
            || !valid_frame_id(&self.frame_id)
            || !valid_remote_task_id(&self.remote_task_id)
            || self.sequence == 0
        {
            return Err(invalid_response(
                "Cidaren BrowserBridge command binding is invalid",
            ));
        }
        let CidarenBrowserCommand::CaptureSnapshot { mode } = self.command;
        if self.version != mode.recipe_version() {
            return Err(protocol_drift(
                "Cidaren Capture command version does not match its recipe mode",
            ));
        }
        Ok(())
    }
}

/// Authenticates and projects one Core-dispatched opaque command for a helper.
///
/// The actual page origin and frame are trusted transport observations, not
/// fields echoed by helper JavaScript. Successful output is a closed,
/// secret-free action/source description; this function never touches the DOM.
///
/// # Errors
///
/// Returns a typed error for size, digest, strict-schema, recipe revision,
/// session nonce, origin, frame, sequence, mode or Task-identity drift.
pub fn project_browser_helper_command(
    value: &SecretValue,
    expected_digest: [u8; 32],
    expected_session_id: BrowserBridgeSessionId,
    actual_origin: &str,
    actual_frame_id: &str,
    expected_sequence: u64,
) -> ProviderResult<CidarenBrowserHelperProjection> {
    let command = CidarenBrowserCommandEnvelope::decode_artifact(value, expected_digest)?;
    let expected_sequence = u32::try_from(expected_sequence)
        .map_err(|_| protocol_drift("Cidaren helper command sequence is unrepresentable"))?;
    if command.session_nonce != expected_session_id.to_string()
        || actual_origin != CIDAREN_ORIGIN
        || command.origin != actual_origin
        || !valid_frame_id(actual_frame_id)
        || command.frame_id != actual_frame_id
        || command.sequence != expected_sequence
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Cidaren helper dispatch binding is stale or foreign",
        ));
    }
    let action = match command.command {
        CidarenBrowserCommand::CaptureSnapshot {
            mode: CidarenCaptureMode::TokenOnly,
        } => CidarenBrowserHelperAction::CaptureSnapshotTokenOnly,
        CidarenBrowserCommand::CaptureSnapshot {
            mode: CidarenCaptureMode::Composite,
        } => CidarenBrowserHelperAction::CaptureSnapshotComposite,
    };
    Ok(CidarenBrowserHelperProjection { action })
}

impl fmt::Debug for CidarenBrowserCommandEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenBrowserCommandEnvelope")
            .field("version", &self.version)
            .field("session_nonce", &"[REDACTED]")
            .field("origin", &self.origin)
            .field("frame_id", &self.frame_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("sequence", &self.sequence)
            .field("command", &self.command)
            .finish()
    }
}

impl Drop for CidarenBrowserCommandEnvelope {
    fn drop(&mut self) {
        self.session_nonce.zeroize();
    }
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CidarenBrowserEvent {
    CaptureSnapshot {
        user_token: Option<String>,
        user_token_source: Option<CidarenCaptureTokenSource>,
        login_info: Option<String>,
        login_info_source: Option<CidarenCaptureStorageSource>,
        user_session: Option<String>,
    },
}

impl fmt::Debug for CidarenBrowserEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenBrowserEvent([REDACTED])")
    }
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CidarenBrowserEventEnvelope {
    pub version: u32,
    pub session_nonce: String,
    pub origin: String,
    pub frame_id: String,
    pub remote_task_id: String,
    pub reply_to_sequence: u32,
    pub event: CidarenBrowserEvent,
}

impl CidarenBrowserEventEnvelope {
    /// Returns the stable durable exchange type used by Core.
    pub(crate) const fn exchange_type() -> &'static str {
        CIDAREN_CAPTURE_RESULT_TYPE
    }
}

impl CidarenBrowserEventEnvelope {
    /// Validates a browser result against the exact command and observed
    /// origin.
    ///
    /// # Errors
    ///
    /// Returns `RemoteChanged` for a foreign binding and typed protocol or
    /// authentication errors for malformed Capture values.
    pub fn validate_for_command(
        &self,
        command: &CidarenBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<()> {
        command.validate()?;
        if observed_origin != CIDAREN_ORIGIN
            || self.version != command.version
            || self.session_nonce != command.session_nonce
            || self.origin != command.origin
            || self.origin != observed_origin
            || self.frame_id != command.frame_id
            || self.remote_task_id != command.remote_task_id
            || self.reply_to_sequence != command.sequence
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Cidaren BrowserBridge result is not bound to the frozen command",
            ));
        }
        let CidarenBrowserCommand::CaptureSnapshot { mode } = command.command;
        let CidarenBrowserEvent::CaptureSnapshot {
            user_token,
            user_token_source,
            login_info,
            login_info_source,
            user_session,
        } = &self.event;
        validate_capture_values(
            mode,
            user_token.as_deref(),
            *user_token_source,
            login_info.as_deref(),
            *login_info_source,
            user_session.as_deref(),
        )
    }

    /// # Errors
    ///
    /// Returns a typed error when the result is foreign to the command or
    /// contains invalid Capture material.
    pub fn into_capture_snapshot(
        mut self,
        command: &CidarenBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<CidarenCaptureSnapshot> {
        self.validate_for_command(command, observed_origin)?;
        let event = std::mem::replace(
            &mut self.event,
            CidarenBrowserEvent::CaptureSnapshot {
                user_token: None,
                user_token_source: None,
                login_info: None,
                login_info_source: None,
                user_session: None,
            },
        );
        let CidarenBrowserEvent::CaptureSnapshot {
            user_token,
            user_token_source,
            login_info,
            login_info_source,
            user_session,
        } = event;
        Ok(CidarenCaptureSnapshot {
            user_token,
            user_token_source,
            login_info,
            login_info_source,
            user_session,
        })
    }
}

impl fmt::Debug for CidarenBrowserEventEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenBrowserEventEnvelope")
            .field("version", &self.version)
            .field("session_nonce", &"[REDACTED]")
            .field("origin", &self.origin)
            .field("frame_id", &self.frame_id)
            .field("remote_task_id", &self.remote_task_id)
            .field("reply_to_sequence", &self.reply_to_sequence)
            .field("event", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenBrowserEventEnvelope {
    fn drop(&mut self) {
        self.session_nonce.zeroize();
        let CidarenBrowserEvent::CaptureSnapshot {
            user_token,
            login_info,
            user_session,
            ..
        } = &mut self.event;
        user_token.zeroize();
        login_info.zeroize();
        user_session.zeroize();
    }
}

/// Zeroizing Provider-private Capture output. It is intentionally not a
/// `CredentialBundle`: Core must validate the recipe/session binding before
/// committing secrets to the `SecretStore`.
pub(crate) struct CidarenCaptureSnapshot {
    user_token: Option<String>,
    user_token_source: Option<CidarenCaptureTokenSource>,
    login_info: Option<String>,
    login_info_source: Option<CidarenCaptureStorageSource>,
    user_session: Option<String>,
}

impl CidarenCaptureSnapshot {
    #[cfg(test)]
    pub(crate) fn user_token(&self) -> Option<&str> {
        self.user_token.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn login_info(&self) -> Option<&str> {
        self.login_info.as_deref()
    }

    #[cfg(test)]
    pub(crate) const fn login_info_source(&self) -> Option<CidarenCaptureStorageSource> {
        self.login_info_source
    }

    #[cfg(test)]
    pub(crate) fn user_session(&self) -> Option<&str> {
        self.user_session.as_deref()
    }

    /// Converts one already validated snapshot into the exact secret-field
    /// replacement expected by Core's Capture credential commit.
    ///
    /// Token-only Capture yields one `ProviderSpecific` access token. Composite
    /// Capture yields one atomic token plus crypto-context pair. The donor's
    /// optional `CDR_USER_SESSION` observation is deliberately not persisted
    /// because neither donor uses it for authentication or `jv=99` decoding.
    ///
    /// # Errors
    ///
    /// Returns an internal error only if an invalid snapshot was constructed
    /// inside this module without passing the typed parser.
    pub(crate) fn into_credential_replacement(mut self) -> ProviderResult<CredentialReplacement> {
        let token = self.user_token.take().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren Capture snapshot lost its validated UserToken",
            )
        })?;
        if self.user_token_source.is_none()
            || self.login_info.is_some() != self.login_info_source.is_some()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren Capture snapshot lost its validated source binding",
            ));
        }

        let mut fields = vec![CredentialField {
            purpose: SecretPurpose::ProviderAccessToken,
            value: SecretValue::new(token.into_bytes()),
        }];
        let session_kind = if let Some(login_info) = self.login_info.take() {
            fields.push(CredentialField {
                purpose: SecretPurpose::ProviderCompositeSession,
                value: SecretValue::new(login_info.into_bytes()),
            });
            SessionKind::Composite
        } else {
            SessionKind::ProviderSpecific
        };
        Ok(CredentialReplacement {
            session_kind,
            fields,
        })
    }
}

impl fmt::Debug for CidarenCaptureSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenCaptureSnapshot")
            .field("user_token", &"[REDACTED]")
            .field("user_token_source", &self.user_token_source)
            .field("login_info", &"[REDACTED]")
            .field("login_info_source", &self.login_info_source)
            .field("user_session", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenCaptureSnapshot {
    fn drop(&mut self) {
        self.user_token.zeroize();
        self.login_info.zeroize();
        self.user_session.zeroize();
    }
}

/// Parses and validates one bounded browser event document.
///
/// # Errors
///
/// Returns a typed error for oversized, malformed or foreign results.
pub(crate) fn parse_browser_event(
    document: &str,
    command: &CidarenBrowserCommandEnvelope,
    observed_origin: &str,
) -> ProviderResult<CidarenBrowserEventEnvelope> {
    if document.is_empty() || document.len() > MAX_BROWSER_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Cidaren BrowserBridge result is empty or oversized",
        ));
    }
    let event = serde_json::from_str::<CidarenBrowserEventEnvelope>(document)
        .map_err(|_| invalid_response("Cidaren BrowserBridge result is not valid JSON"))?;
    event.validate_for_command(command, observed_origin)?;
    Ok(event)
}

/// Hashes one bounded raw result document for Core's durable exchange record.
/// The document itself remains Provider/Capture-owned and is never persisted
/// in Domain storage.
///
/// # Errors
///
/// Returns a typed error when the document is empty or oversized.
pub(crate) fn browser_event_exchange_digest(document: &str) -> ProviderResult<[u8; 32]> {
    if document.is_empty() || document.len() > MAX_BROWSER_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Cidaren BrowserBridge result is empty or oversized",
        ));
    }
    Ok(Sha256::digest(document.as_bytes()).into())
}

fn validate_capture_values(
    mode: CidarenCaptureMode,
    user_token: Option<&str>,
    user_token_source: Option<CidarenCaptureTokenSource>,
    login_info: Option<&str>,
    login_info_source: Option<CidarenCaptureStorageSource>,
    user_session: Option<&str>,
) -> ProviderResult<()> {
    let Some(user_token) = user_token else {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren Capture result has no UserToken",
        ));
    };
    if !valid_token(user_token, MAX_CAPTURE_VALUE_BYTES) || user_token_source.is_none() {
        return Err(invalid_response("Cidaren Capture UserToken is invalid"));
    }
    match mode {
        CidarenCaptureMode::TokenOnly => {
            if user_token_source != Some(CidarenCaptureTokenSource::RequestHeader) {
                return Err(protocol_drift(
                    "Cidaren token-only Capture must use the request-header UserToken",
                ));
            }
            if login_info.is_some() || login_info_source.is_some() || user_session.is_some() {
                return Err(protocol_drift(
                    "Cidaren token-only Capture must not combine Composite storage",
                ));
            }
        }
        CidarenCaptureMode::Composite => {
            let Some(login_info) = login_info else {
                return Err(ProviderError::new(
                    ProviderErrorKind::Authentication,
                    "Cidaren Composite Capture has no login-info context",
                ));
            };
            if login_info_source.is_none() || !valid_json_object(login_info) {
                return Err(invalid_response(
                    "Cidaren Composite Capture login-info is invalid",
                ));
            }
            CidarenCryptoContext::parse(login_info.as_bytes())?;
            if let Some(user_session) = user_session
                && !valid_json_object(user_session)
            {
                return Err(invalid_response("Cidaren optional UserSession is invalid"));
            }
        }
    }
    Ok(())
}

fn valid_json_object(value: &str) -> bool {
    if value.len() > MAX_CAPTURE_VALUE_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return false;
    }
    let Ok(mut document) = serde_json::from_str::<serde_json::Value>(value) else {
        return false;
    };
    let is_object = document.is_object();
    zeroize_json(&mut document);
    is_object
}

fn zeroize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(value) => value.zeroize(),
        serde_json::Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        serde_json::Value::Object(values) => values.values_mut().for_each(zeroize_json),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn valid_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_frame_id(value: &str) -> bool {
    valid_token(value, MAX_FRAME_ID_BYTES)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

pub(crate) fn valid_remote_task_id(value: &str) -> bool {
    if value.len() > MAX_REMOTE_TASK_ID_BYTES || value.trim() != value {
        return false;
    }
    if let Some(release_id) = value.strip_prefix("class-task:") {
        return !release_id.is_empty()
            && release_id != "0"
            && !release_id.starts_with('0')
            && release_id.len() <= 32
            && release_id.bytes().all(|byte| byte.is_ascii_digit());
    }
    value
        .strip_prefix("study-task:")
        .and_then(|identity| identity.split_once(':'))
        .is_some_and(|(course_id, list_id)| valid_component(course_id) && valid_component(list_id))
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(mode: CidarenCaptureMode) -> CidarenBrowserCommandEnvelope {
        CidarenBrowserCommandEnvelope::capture_snapshot(
            "synthetic-session-nonce".to_owned(),
            "frame-1".to_owned(),
            "class-task:2002".to_owned(),
            1,
            mode,
        )
        .unwrap()
    }

    fn document(mode: CidarenCaptureMode) -> String {
        let event = match mode {
            CidarenCaptureMode::TokenOnly => json!({
                "kind": "capture_snapshot",
                "user_token": "synthetic-user-token",
                "user_token_source": "request_header",
                "login_info": null,
                "login_info_source": null,
                "user_session": null
            }),
            CidarenCaptureMode::Composite => json!({
                "kind": "capture_snapshot",
                "user_token": "synthetic-user-token",
                "user_token_source": "request_header",
                "login_info": r#"{"login_info":{"a":"hc3ludGhldGljLXNoYXJlZC1zZWNyZXQ=","b":"ac3ludGhldGljLXNhbHQ="}}"#,
                "login_info_source": "local_storage",
                "user_session": null
            }),
        };
        serde_json::to_string(&json!({
            "version": mode.recipe_version(),
            "session_nonce": "synthetic-session-nonce",
            "origin": CIDAREN_ORIGIN,
            "frame_id": "frame-1",
            "remote_task_id": "class-task:2002",
            "reply_to_sequence": 1,
            "event": event
        }))
        .unwrap()
    }

    #[test]
    fn capture_command_is_recipe_versioned_and_redacted() {
        let command = command(CidarenCaptureMode::Composite);
        assert_eq!(command.version, 2);
        assert_eq!(
            CidarenBrowserCommandEnvelope::exchange_type(),
            CIDAREN_CAPTURE_COMMAND_TYPE
        );
        let digest = command.exchange_digest().unwrap();
        assert_ne!(digest, [0; 32]);
        assert_eq!(command.exchange_digest().unwrap(), digest);
        assert!(format!("{command:?}").contains("REDACTED"));
        assert!(
            serde_json::to_string(&command)
                .unwrap()
                .contains("capture_snapshot")
        );
    }

    #[test]
    fn encrypted_command_artifact_rebinds_every_recovery_authority() {
        let session_id = BrowserBridgeSessionId::new();
        let command = CidarenBrowserCommandEnvelope::capture_snapshot(
            session_id.to_string(),
            "frame-recovery".to_owned(),
            "class-task:2002".to_owned(),
            7,
            CidarenCaptureMode::Composite,
        )
        .unwrap();
        let artifact = command.encode_artifact().unwrap();
        let digest = artifact.digest();
        assert_eq!(digest, command.exchange_digest().unwrap());
        assert!(!format!("{artifact:?}").contains(&session_id.to_string()));
        let value = artifact.into_secret_value();

        let restored = CidarenBrowserCommandEnvelope::decode_artifact_bound(
            &value,
            digest,
            session_id,
            "class-task:2002",
            7,
            CidarenCaptureMode::Composite,
        )
        .unwrap();
        assert_eq!(restored, command);
        assert!(
            CidarenBrowserCommandEnvelope::decode_artifact_bound(
                &value,
                [9; 32],
                session_id,
                "class-task:2002",
                7,
                CidarenCaptureMode::Composite,
            )
            .is_err()
        );
        for (task, sequence, mode) in [
            ("class-task:2003", 7, CidarenCaptureMode::Composite),
            ("class-task:2002", 8, CidarenCaptureMode::Composite),
            ("class-task:2002", 7, CidarenCaptureMode::TokenOnly),
        ] {
            assert_eq!(
                CidarenBrowserCommandEnvelope::decode_artifact_bound(
                    &value, digest, session_id, task, sequence, mode,
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::RemoteChanged
            );
        }
        assert_eq!(
            CidarenBrowserCommandEnvelope::decode_artifact_bound(
                &value,
                digest,
                BrowserBridgeSessionId::new(),
                "class-task:2002",
                7,
                CidarenCaptureMode::Composite,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn helper_projection_exposes_only_audited_capture_actions_and_sources() {
        let session_id = BrowserBridgeSessionId::new();
        for (mode, action, user_token_sources, login_info_sources) in [
            (
                CidarenCaptureMode::TokenOnly,
                CidarenBrowserHelperAction::CaptureSnapshotTokenOnly,
                &[CidarenBrowserCaptureSource::RequestHeaderUserToken][..],
                &[][..],
            ),
            (
                CidarenCaptureMode::Composite,
                CidarenBrowserHelperAction::CaptureSnapshotComposite,
                &[
                    CidarenBrowserCaptureSource::RequestHeaderUserToken,
                    CidarenBrowserCaptureSource::LocalStorageUserToken,
                    CidarenBrowserCaptureSource::SessionStorageUserToken,
                ][..],
                &[
                    CidarenBrowserCaptureSource::LocalStorageLoginInfo,
                    CidarenBrowserCaptureSource::SessionStorageLoginInfo,
                ][..],
            ),
        ] {
            let command = CidarenBrowserCommandEnvelope::capture_snapshot(
                session_id.to_string(),
                "frame-helper".to_owned(),
                "study-task:course-a:list-a".to_owned(),
                11,
                mode,
            )
            .unwrap();
            let artifact = command.encode_artifact().unwrap();
            let digest = artifact.digest();
            let projection = project_browser_helper_command(
                &artifact.into_secret_value(),
                digest,
                session_id,
                CIDAREN_ORIGIN,
                "frame-helper",
                11,
            )
            .unwrap();
            assert_eq!(projection.action(), action);
            assert_eq!(projection.user_token_sources(), user_token_sources);
            assert_eq!(projection.login_info_sources(), login_info_sources);
            let debug = format!("{projection:?}");
            assert!(!debug.contains(&session_id.to_string()));
            assert!(!debug.contains("study-task") && !debug.contains("script"));
        }
    }

    #[test]
    fn helper_projection_rejects_dispatch_binding_drift() {
        let session_id = BrowserBridgeSessionId::new();
        let command = CidarenBrowserCommandEnvelope::capture_snapshot(
            session_id.to_string(),
            "frame-helper".to_owned(),
            "class-task:2002".to_owned(),
            7,
            CidarenCaptureMode::Composite,
        )
        .unwrap();
        let artifact = command.encode_artifact().unwrap();
        let digest = artifact.digest();
        let value = artifact.into_secret_value();
        assert_eq!(
            project_browser_helper_command(
                &value,
                [9; 32],
                session_id,
                CIDAREN_ORIGIN,
                "frame-helper",
                7,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
        for (dispatch_session, origin, frame, sequence) in [
            (
                BrowserBridgeSessionId::new(),
                CIDAREN_ORIGIN,
                "frame-helper",
                7,
            ),
            (session_id, "https://evil.example", "frame-helper", 7),
            (session_id, CIDAREN_ORIGIN, "frame-foreign", 7),
            (session_id, CIDAREN_ORIGIN, "frame-helper", 8),
        ] {
            assert_eq!(
                project_browser_helper_command(
                    &value,
                    digest,
                    dispatch_session,
                    origin,
                    frame,
                    sequence,
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::RemoteChanged
            );
        }
        assert_eq!(
            project_browser_helper_command(
                &value,
                digest,
                session_id,
                CIDAREN_ORIGIN,
                "frame-helper",
                u64::from(u32::MAX) + 1,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn helper_projection_rejects_schema_task_and_automation_drift() {
        let session_id = BrowserBridgeSessionId::new();
        let base = CidarenBrowserCommandEnvelope::capture_snapshot(
            session_id.to_string(),
            "frame-helper".to_owned(),
            "class-task:2002".to_owned(),
            1,
            CidarenCaptureMode::TokenOnly,
        )
        .unwrap();
        let base: serde_json::Value = serde_json::to_value(base).unwrap();
        let mut unknown_field = base.clone();
        unknown_field
            .as_object_mut()
            .unwrap()
            .insert("selector".to_owned(), json!("#arbitrary"));
        let mut arbitrary_automation = base.clone();
        arbitrary_automation["command"] = json!({
            "kind": "run_script",
            "selector": "#arbitrary",
            "script": "return document.cookie"
        });
        let mut unknown_mode = base.clone();
        unknown_mode["command"]["mode"] = json!("future");
        let mut wrong_revision = base.clone();
        wrong_revision["version"] = json!(99);
        let mut invalid_task = base;
        invalid_task["remote_task_id"] = json!("class-task:02");

        for (document, expected_kind) in [
            (unknown_field, ProviderErrorKind::ProtocolDrift),
            (arbitrary_automation, ProviderErrorKind::ProtocolDrift),
            (unknown_mode, ProviderErrorKind::ProtocolDrift),
            (wrong_revision, ProviderErrorKind::ProtocolDrift),
            (invalid_task, ProviderErrorKind::InvalidResponse),
        ] {
            let artifact = SecretValue::new(serde_json::to_vec(&document).unwrap());
            let digest: [u8; 32] = Sha256::digest(artifact.expose_secret()).into();
            assert_eq!(
                project_browser_helper_command(
                    &artifact,
                    digest,
                    session_id,
                    CIDAREN_ORIGIN,
                    "frame-helper",
                    1,
                )
                .unwrap_err()
                .kind,
                expected_kind
            );
        }

        for artifact in [
            SecretValue::new(Vec::new()),
            SecretValue::new(vec![b'x'; MAX_BROWSER_COMMAND_BYTES + 1]),
        ] {
            let digest: [u8; 32] = Sha256::digest(artifact.expose_secret()).into();
            assert_eq!(
                project_browser_helper_command(
                    &artifact,
                    digest,
                    session_id,
                    CIDAREN_ORIGIN,
                    "frame-helper",
                    1,
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::InvalidResponse
            );
        }
    }

    #[test]
    fn encrypted_command_artifact_has_an_independent_tight_bound() {
        let oversized = SecretValue::new(vec![b'x'; MAX_BROWSER_COMMAND_BYTES + 1]);
        let digest: [u8; 32] = Sha256::digest(oversized.expose_secret()).into();
        assert_eq!(
            CidarenBrowserCommandEnvelope::decode_artifact_bound(
                &oversized,
                digest,
                BrowserBridgeSessionId::new(),
                "class-task:2002",
                1,
                CidarenCaptureMode::TokenOnly,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::InvalidResponse
        );
    }

    #[test]
    fn owned_result_document_is_bounded_and_redacted() {
        let owned_document =
            CidarenBrowserResultDocument::try_new(document(CidarenCaptureMode::Composite)).unwrap();
        assert_ne!(owned_document.exchange_digest().unwrap(), [0; 32]);
        let debug = format!("{owned_document:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("synthetic-user-token"));
        assert!(CidarenBrowserResultDocument::try_new(String::new()).is_err());
        assert!(
            CidarenBrowserResultDocument::try_new("x".repeat(MAX_BROWSER_DOCUMENT_BYTES + 1))
                .is_err()
        );

        let persisted =
            SecretValue::new(document(CidarenCaptureMode::TokenOnly).as_bytes().to_vec());
        let digest = persisted_digest(&persisted);
        let restored =
            CidarenBrowserResultDocument::try_from_secret_value(&persisted, digest).unwrap();
        assert_eq!(restored.exchange_digest().unwrap(), digest);
        assert_eq!(
            CidarenBrowserResultDocument::try_from_secret_value(&persisted, [9; 32])
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let invalid_utf8 = SecretValue::new(vec![0xff]);
        assert_eq!(
            CidarenBrowserResultDocument::try_from_secret_value(
                &invalid_utf8,
                persisted_digest(&invalid_utf8),
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::InvalidResponse
        );
        for invalid in [
            SecretValue::new(Vec::new()),
            SecretValue::new(vec![b'x'; MAX_BROWSER_DOCUMENT_BYTES + 1]),
        ] {
            assert_eq!(
                CidarenBrowserResultDocument::try_from_secret_value(
                    &invalid,
                    persisted_digest(&invalid),
                )
                .unwrap_err()
                .kind,
                ProviderErrorKind::InvalidResponse
            );
        }
    }

    fn persisted_digest(value: &SecretValue) -> [u8; 32] {
        Sha256::digest(value.expose_secret()).into()
    }

    #[test]
    fn token_only_result_cannot_mix_composite_material() {
        let command = command(CidarenCaptureMode::TokenOnly);
        let document = document(CidarenCaptureMode::TokenOnly);
        let result = parse_browser_event(&document, &command, CIDAREN_ORIGIN)
            .unwrap()
            .into_capture_snapshot(&command, CIDAREN_ORIGIN)
            .unwrap();
        assert_eq!(result.user_token(), Some("synthetic-user-token"));
        assert!(result.login_info().is_none());
        assert!(!format!("{result:?}").contains("synthetic-user-token"));

        let mixed = document.replace("\"login_info\":null", "\"login_info\":\"{}\"");
        assert_eq!(
            parse_browser_event(&mixed, &command, CIDAREN_ORIGIN)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        let wrong_source = document.replace(
            "\"user_token_source\":\"request_header\"",
            "\"user_token_source\":\"local_storage\"",
        );
        assert_eq!(
            parse_browser_event(&wrong_source, &command, CIDAREN_ORIGIN)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn composite_result_validates_crypto_context_and_binding() {
        let composite_command = command(CidarenCaptureMode::Composite);
        let event = parse_browser_event(
            &document(CidarenCaptureMode::Composite),
            &composite_command,
            CIDAREN_ORIGIN,
        )
        .unwrap();
        let snapshot = event
            .into_capture_snapshot(&composite_command, CIDAREN_ORIGIN)
            .unwrap();
        assert_eq!(
            snapshot.login_info_source(),
            Some(CidarenCaptureStorageSource::LocalStorage)
        );
        assert!(snapshot.user_session().is_none());
        assert_eq!(
            CidarenBrowserEventEnvelope::exchange_type(),
            CIDAREN_CAPTURE_RESULT_TYPE
        );
        assert_ne!(
            browser_event_exchange_digest(&document(CidarenCaptureMode::Composite)).unwrap(),
            [0; 32]
        );

        let mut foreign_command = command(CidarenCaptureMode::Composite);
        foreign_command.sequence = 2;
        assert_eq!(
            parse_browser_event(
                &document(CidarenCaptureMode::Composite),
                &foreign_command,
                CIDAREN_ORIGIN
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn committed_capture_result_fixtures_match_typed_commands() {
        let token_command = command(CidarenCaptureMode::TokenOnly);
        let token_event = parse_browser_event(
            include_str!(
                "../../../fixtures/providers/cidaren/browser/capture-snapshot-token-only.json"
            ),
            &token_command,
            CIDAREN_ORIGIN,
        )
        .unwrap();
        let token_snapshot = token_event
            .into_capture_snapshot(&token_command, CIDAREN_ORIGIN)
            .unwrap();
        assert_eq!(token_snapshot.user_token(), Some("synthetic-user-token"));
        assert!(token_snapshot.login_info().is_none());

        let composite_command = command(CidarenCaptureMode::Composite);
        let composite_event = parse_browser_event(
            include_str!(
                "../../../fixtures/providers/cidaren/browser/capture-snapshot-composite.json"
            ),
            &composite_command,
            CIDAREN_ORIGIN,
        )
        .unwrap();
        let composite_snapshot = composite_event
            .into_capture_snapshot(&composite_command, CIDAREN_ORIGIN)
            .unwrap();
        assert_eq!(
            composite_snapshot.login_info_source(),
            Some(CidarenCaptureStorageSource::LocalStorage)
        );
    }

    #[test]
    fn committed_capture_command_fixtures_are_canonical_recovery_artifacts() {
        for (fixture, expected) in [
            (
                include_str!(
                    "../../../fixtures/providers/cidaren/browser/capture-command-token-only.json"
                ),
                command(CidarenCaptureMode::TokenOnly),
            ),
            (
                include_str!(
                    "../../../fixtures/providers/cidaren/browser/capture-command-composite.json"
                ),
                command(CidarenCaptureMode::Composite),
            ),
        ] {
            let parsed: CidarenBrowserCommandEnvelope = serde_json::from_str(fixture).unwrap();
            assert_eq!(parsed, expected);
            parsed.validate().unwrap();

            let canonical = serde_json::to_string(&parsed).unwrap();
            assert_eq!(canonical, fixture.trim_end());
            let artifact = parsed.encode_artifact().unwrap();
            let expected_digest: [u8; 32] = Sha256::digest(canonical.as_bytes()).into();
            assert_eq!(artifact.digest(), expected_digest);
            assert_eq!(
                artifact.into_secret_value().expose_secret(),
                canonical.as_bytes()
            );
        }
    }

    #[test]
    fn capture_snapshot_converts_only_evidenced_credential_fields() {
        let token_command = command(CidarenCaptureMode::TokenOnly);
        let token_replacement = parse_browser_event(
            &document(CidarenCaptureMode::TokenOnly),
            &token_command,
            CIDAREN_ORIGIN,
        )
        .unwrap()
        .into_capture_snapshot(&token_command, CIDAREN_ORIGIN)
        .unwrap()
        .into_credential_replacement()
        .unwrap();
        assert_eq!(
            token_replacement.session_kind,
            SessionKind::ProviderSpecific
        );
        assert_eq!(token_replacement.fields.len(), 1);
        assert_eq!(
            token_replacement.fields[0].purpose,
            SecretPurpose::ProviderAccessToken
        );
        assert_eq!(
            token_replacement.fields[0].value.expose_secret(),
            b"synthetic-user-token"
        );

        let composite_command = command(CidarenCaptureMode::Composite);
        let composite_replacement = parse_browser_event(
            &document(CidarenCaptureMode::Composite).replace(
                "\"user_session\":null",
                r#""user_session":"{\"observed\":true}""#,
            ),
            &composite_command,
            CIDAREN_ORIGIN,
        )
        .unwrap()
        .into_capture_snapshot(&composite_command, CIDAREN_ORIGIN)
        .unwrap()
        .into_credential_replacement()
        .unwrap();
        assert_eq!(composite_replacement.session_kind, SessionKind::Composite);
        assert_eq!(composite_replacement.fields.len(), 2);
        assert_eq!(
            composite_replacement.fields[0].purpose,
            SecretPurpose::ProviderAccessToken
        );
        assert_eq!(
            composite_replacement.fields[1].purpose,
            SecretPurpose::ProviderCompositeSession
        );
        assert!(composite_replacement.fields.iter().all(|field| matches!(
            field.purpose,
            SecretPurpose::ProviderAccessToken | SecretPurpose::ProviderCompositeSession
        )));
    }
}
