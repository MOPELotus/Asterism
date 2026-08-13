use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::CidarenCryptoContext;

const CIDAREN_ORIGIN: &str = "https://app.vocabgo.com";
const MAX_BROWSER_DOCUMENT_BYTES: usize = 256 * 1_024;
const MAX_SESSION_NONCE_BYTES: usize = 512;
const MAX_FRAME_ID_BYTES: usize = 256;
const MAX_REMOTE_TASK_ID_BYTES: usize = 768;
const MAX_CAPTURE_VALUE_BYTES: usize = 64 * 1_024;

/// Stable Core `BrowserBridge` exchange type for one Cidaren Capture command.
pub const CIDAREN_CAPTURE_COMMAND_TYPE: &str = "cidaren.capture.snapshot";
/// Stable Core `BrowserBridge` exchange type for one Cidaren Capture result.
pub const CIDAREN_CAPTURE_RESULT_TYPE: &str = "cidaren.capture.snapshot.result";

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

/// Source of a captured `UserToken`. The source is descriptive only; the
/// Core Capture recipe remains the authority for which source is allowed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CidarenCaptureTokenSource {
    RequestHeader,
    LocalStorage,
    SessionStorage,
}

/// Source of the optional Composite login-info object.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CidarenCaptureStorageSource {
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
        self.validate()?;
        let encoded = serde_json::to_vec(self)
            .map_err(|_| invalid_response("Cidaren Capture command cannot be hashed"))?;
        Ok(Sha256::digest(encoded).into())
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

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CidarenBrowserEvent {
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

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CidarenBrowserEventEnvelope {
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
    pub const fn exchange_type() -> &'static str {
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
        self,
        command: &CidarenBrowserCommandEnvelope,
        observed_origin: &str,
    ) -> ProviderResult<CidarenCaptureSnapshot> {
        self.validate_for_command(command, observed_origin)?;
        let event = self.event.clone();
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
pub struct CidarenCaptureSnapshot {
    user_token: Option<String>,
    user_token_source: Option<CidarenCaptureTokenSource>,
    login_info: Option<String>,
    login_info_source: Option<CidarenCaptureStorageSource>,
    user_session: Option<String>,
}

impl CidarenCaptureSnapshot {
    pub fn user_token(&self) -> Option<&str> {
        self.user_token.as_deref()
    }

    pub const fn user_token_source(&self) -> Option<CidarenCaptureTokenSource> {
        self.user_token_source
    }

    pub fn login_info(&self) -> Option<&str> {
        self.login_info.as_deref()
    }

    pub const fn login_info_source(&self) -> Option<CidarenCaptureStorageSource> {
        self.login_info_source
    }

    pub fn user_session(&self) -> Option<&str> {
        self.user_session.as_deref()
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
pub fn parse_browser_event(
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
pub fn browser_event_exchange_digest(document: &str) -> ProviderResult<[u8; 32]> {
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
    value.len() <= MAX_CAPTURE_VALUE_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && serde_json::from_str::<serde_json::Value>(value)
            .ok()
            .is_some_and(|value| value.is_object())
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

fn valid_remote_task_id(value: &str) -> bool {
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
        assert_ne!(command.exchange_digest().unwrap(), [0; 32]);
        assert!(format!("{command:?}").contains("REDACTED"));
        assert!(
            serde_json::to_string(&command)
                .unwrap()
                .contains("capture_snapshot")
        );
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
}
