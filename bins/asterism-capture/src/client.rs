use std::{collections::HashSet, fmt, time::Duration};

use anyhow::{Context, bail};
use asterism_domain::{
    AuthBootstrapClientEvent, AuthBootstrapClientEventKind, AuthBootstrapPurpose,
    AuthBootstrapSession, AuthBootstrapSessionId, AuthBootstrapState, AuthMethod,
    BrowserBridgeSessionId, BrowserBridgeSessionState, ProviderAccountId, ProviderId, SessionKind,
    TaskId, Timestamp,
};
use asterism_provider_api::{CaptureRecipe, SessionStatus};
use asterism_secrets::{SecretPurpose, SecretString, SecretValue};
use bytes::Bytes;
use chrono::Utc;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const MAX_BOOTSTRAP_TOKEN_BYTES: usize = 128;
const MAX_CREDENTIAL_FIELD_BYTES: usize = 1024 * 1024;
const MAX_BROWSER_BRIDGE_ARTIFACT_BYTES: usize = 256 * 1024;
const MAX_BROWSER_BRIDGE_LABEL_BYTES: usize = 128;
const X_BROWSER_COMMAND_TYPE: &str = "x-asterism-browser-command-type";
const X_BROWSER_COMMAND_DIGEST: &str = "x-asterism-browser-command-digest";
const X_BROWSER_RESULT_TYPE: &str = "x-asterism-browser-result-type";

#[derive(Debug)]
pub struct CaptureClient {
    base_url: Url,
    http: Client,
}

impl CaptureClient {
    /// Builds an outbound-only Capture client for one Asterism server.
    ///
    /// # Errors
    ///
    /// Returns an error when the base URL violates the HTTPS/loopback policy or
    /// the HTTP client cannot be initialized.
    pub fn new(base_url: &str, allow_insecure_loopback: bool) -> anyhow::Result<Self> {
        let base_url = validate_base_url(base_url, allow_insecure_loopback)?;
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build the Asterism Capture HTTP client")?;
        Ok(Self { base_url, http })
    }

    /// Reads the server's non-secret health summary.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, an unexpected response status,
    /// an oversized body, or an invalid response document.
    pub async fn health(&self) -> anyhow::Result<CaptureHealth> {
        let url = self
            .base_url
            .join("api/v1/system/health")
            .context("failed to construct the health endpoint")?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to connect to the Asterism server")?;
        deserialize_response(response, &[StatusCode::OK]).await
    }

    /// Consumes one pairing token and keeps the returned session-scoped access
    /// token only in memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the pairing token is malformed, the request is
    /// rejected, or the server returns a session that does not match the claim.
    pub async fn claim(
        &self,
        session_id: AuthBootstrapSessionId,
        pairing_token: &SecretString,
    ) -> anyhow::Result<ClaimedCaptureSession> {
        let url = self
            .base_url
            .join(&format!(
                "api/v1/auth-bootstrap/sessions/{session_id}/claim"
            ))
            .context("failed to construct the pairing claim endpoint")?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, bootstrap_authorization(pairing_token)?)
            .send()
            .await
            .context("failed to submit the Asterism pairing claim")?;
        let claimed: ClaimResponse = deserialize_response(response, &[StatusCode::OK]).await?;
        claimed.into_session(session_id)
    }

    /// Consumes one `BrowserBridge` pairing token and retains the resulting
    /// session-scoped access token only in zeroizing memory.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid token, rejected claim, unsafe browser
    /// policy or a response bound to another session.
    pub async fn claim_browser_bridge(
        &self,
        session_id: BrowserBridgeSessionId,
        pairing_token: &SecretString,
    ) -> anyhow::Result<ClaimedBrowserBridgeSession> {
        validate_browser_bridge_token(pairing_token.expose_secret(), true)?;
        let url = self
            .base_url
            .join(&format!(
                "api/v1/browser-bridge/sessions/{session_id}/claim"
            ))
            .context("failed to construct the BrowserBridge claim endpoint")?;
        let response = self
            .http
            .post(url)
            .header(
                AUTHORIZATION,
                browser_bridge_authorization(pairing_token, true)?,
            )
            .send()
            .await
            .context("failed to submit the BrowserBridge pairing claim")?;
        let claimed: BrowserBridgeClaimResponse =
            deserialize_response(response, &[StatusCode::OK]).await?;
        claimed.into_session(session_id)
    }

    /// Binds the helper to one exact allowed top-level origin and browser
    /// frame before any opaque Provider command can be dispatched.
    ///
    /// # Errors
    ///
    /// Returns an error for a terminal session, unsafe/foreign binding,
    /// rejected access token or mismatched server receipt.
    pub async fn bind_browser_bridge_runtime(
        &self,
        claimed: &mut ClaimedBrowserBridgeSession,
        observed_origin: &str,
        frame_id: &str,
    ) -> anyhow::Result<BrowserBridgeRuntimeBindingReceipt> {
        claimed.ensure_live()?;
        if !claimed
            .spec
            .allowed_origins
            .iter()
            .any(|allowed| allowed == observed_origin)
            || !valid_browser_bridge_frame_id(frame_id)
        {
            bail!("BrowserBridge runtime binding is outside the frozen browser policy");
        }
        let session_id = claimed.session.id;
        let url = self
            .base_url
            .join(&format!(
                "api/v1/browser-bridge/sessions/{session_id}/binding"
            ))
            .context("failed to construct the BrowserBridge binding endpoint")?;
        let response = self
            .http
            .put(url)
            .header(
                AUTHORIZATION,
                browser_bridge_authorization(&claimed.access_token, false)?,
            )
            .json(&BrowserBridgeRuntimeBindingRequest {
                observed_origin,
                frame_id,
            })
            .send()
            .await
            .context("failed to bind the BrowserBridge runtime")?;
        let access_rejected = response.status() == StatusCode::UNAUTHORIZED;
        let result = deserialize_response(response, &[StatusCode::OK]).await;
        if access_rejected {
            claimed.invalidate_access();
        }
        let receipt: BrowserBridgeRuntimeBindingReceipt = result?;
        if receipt.session_id != session_id
            || receipt.observed_origin != observed_origin
            || receipt.frame_id != frame_id
            || receipt.bound_at
                < claimed
                    .session
                    .claimed_at
                    .unwrap_or(claimed.session.created_at)
        {
            bail!("Asterism server returned a mismatched BrowserBridge binding receipt");
        }
        claimed.binding = Some(receipt.clone());
        Ok(receipt)
    }

    /// Dispatches the next opaque Provider command exactly once and verifies
    /// its type plus SHA-256 identity before exposing zeroizing bytes.
    ///
    /// A transport failure after the GET is deliberately not retried: Core may
    /// already have marked the command dispatched.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing runtime binding, concurrent command,
    /// rejected access, unsafe metadata, oversized body or digest mismatch.
    pub async fn dispatch_browser_bridge_command(
        &self,
        claimed: &mut ClaimedBrowserBridgeSession,
    ) -> anyhow::Result<BrowserBridgeCommand> {
        claimed.ensure_live()?;
        if claimed.binding.is_none() || claimed.active_command.is_some() {
            bail!("BrowserBridge command dispatch requires one idle bound runtime");
        }
        let session_id = claimed.session.id;
        let sequence = claimed.next_sequence;
        let url = self
            .base_url
            .join(&format!(
                "api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}"
            ))
            .context("failed to construct the BrowserBridge command endpoint")?;
        let response = self
            .http
            .get(url)
            .header(
                AUTHORIZATION,
                browser_bridge_authorization(&claimed.access_token, false)?,
            )
            .send()
            .await
            .context("failed to dispatch the BrowserBridge command; it must not be replayed")?;
        if response.status() != StatusCode::OK {
            if response.status() == StatusCode::UNAUTHORIZED {
                claimed.invalidate_access();
            }
            let _: serde_json::Value = deserialize_response(response, &[StatusCode::OK]).await?;
            unreachable!("unexpected BrowserBridge command response passed validation");
        }
        require_octet_stream(&response)?;
        let command_type = response
            .headers()
            .get(X_BROWSER_COMMAND_TYPE)
            .and_then(|value| value.to_str().ok())
            .filter(|value| valid_provider_label(&claimed.session.provider_id, value))
            .context("Asterism server returned an invalid BrowserBridge command type")?
            .to_owned();
        let command_digest = response
            .headers()
            .get(X_BROWSER_COMMAND_DIGEST)
            .and_then(|value| value.to_str().ok())
            .and_then(decode_digest)
            .context("Asterism server returned an invalid BrowserBridge command digest")?;
        let body =
            read_bounded_body_with_limit(response, MAX_BROWSER_BRIDGE_ARTIFACT_BYTES).await?;
        let actual_digest: [u8; 32] = Sha256::digest(&body).into();
        if body.is_empty() || actual_digest != command_digest {
            bail!("BrowserBridge command bytes do not match their durable digest");
        }
        claimed.active_command = Some(ActiveBrowserBridgeCommand {
            sequence,
            command_type: command_type.clone(),
            command_digest,
        });
        Ok(BrowserBridgeCommand {
            session_id,
            sequence,
            command_type,
            command_digest,
            command_artifact: SecretValue::new(body),
        })
    }

    /// Submits one opaque result for the active command. Retrying the exact
    /// same result after an ambiguous response is safe because Core compares
    /// its type and digest before returning a duplicate receipt.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign result type, empty/oversized bytes,
    /// missing active command, rejected access or mismatched receipt.
    pub async fn submit_browser_bridge_result(
        &self,
        claimed: &mut ClaimedBrowserBridgeSession,
        result_type: &str,
        result_artifact: &SecretValue,
    ) -> anyhow::Result<BrowserBridgeResultReceipt> {
        claimed.ensure_live()?;
        let active = claimed
            .active_command
            .as_ref()
            .context("BrowserBridge result requires one active dispatched command")?;
        if !valid_provider_label(&claimed.session.provider_id, &active.command_type)
            || active.command_digest == [0; 32]
            || !valid_provider_label(&claimed.session.provider_id, result_type)
            || result_artifact.expose_secret().is_empty()
            || result_artifact.expose_secret().len() > MAX_BROWSER_BRIDGE_ARTIFACT_BYTES
        {
            bail!("BrowserBridge result metadata or bytes are invalid");
        }
        let session_id = claimed.session.id;
        let sequence = active.sequence;
        let result_digest: [u8; 32] = Sha256::digest(result_artifact.expose_secret()).into();
        let url = self
            .base_url
            .join(&format!(
                "api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}/result"
            ))
            .context("failed to construct the BrowserBridge result endpoint")?;
        let body = Zeroizing::new(result_artifact.expose_secret().to_vec());
        let response = self
            .http
            .post(url)
            .header(
                AUTHORIZATION,
                browser_bridge_authorization(&claimed.access_token, false)?,
            )
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(X_BROWSER_RESULT_TYPE, result_type)
            .body(Bytes::from_owner(ZeroizingRequestBody(body)))
            .send()
            .await
            .context("failed to submit the BrowserBridge result")?;
        let access_rejected = response.status() == StatusCode::UNAUTHORIZED;
        let result = deserialize_response(response, &[StatusCode::OK, StatusCode::ACCEPTED]).await;
        if access_rejected {
            claimed.invalidate_access();
        }
        let receipt: BrowserBridgeResultReceiptWire = result?;
        if receipt.session_id != session_id
            || receipt.sequence != sequence
            || receipt.result_type != result_type
            || decode_digest(&receipt.result_digest) != Some(result_digest)
            || receipt.received_at
                < claimed
                    .session
                    .claimed_at
                    .unwrap_or(claimed.session.created_at)
        {
            bail!("Asterism server returned a mismatched BrowserBridge result receipt");
        }
        claimed.active_command = None;
        claimed.next_sequence = claimed
            .next_sequence
            .checked_add(1)
            .context("BrowserBridge command sequence exhausted")?;
        Ok(BrowserBridgeResultReceipt {
            session_id: receipt.session_id,
            sequence: receipt.sequence,
            result_type: receipt.result_type,
            result_digest,
            received_at: receipt.received_at,
            duplicate: receipt.duplicate,
        })
    }

    /// Polls the server-side snapshot for a claimed Capture session.
    ///
    /// # Errors
    ///
    /// Returns an error when the scoped access token is rejected or the server
    /// returns an invalid or differently bound session.
    pub async fn poll_session(
        &self,
        claimed: &mut ClaimedCaptureSession,
    ) -> anyhow::Result<AuthBootstrapSession> {
        claimed.ensure_live()?;
        let session_id = claimed.session.id;
        let url = self
            .base_url
            .join(&format!(
                "api/v1/auth-bootstrap/sessions/{session_id}/stream"
            ))
            .context("failed to construct the session polling endpoint")?;
        let response = self
            .http
            .get(url)
            .header(
                AUTHORIZATION,
                bootstrap_authorization(&claimed.access_token)?,
            )
            .send()
            .await
            .context("failed to poll the Asterism pairing session")?;
        let access_rejected = response.status() == StatusCode::UNAUTHORIZED;
        let result = deserialize_response(response, &[StatusCode::OK]).await;
        if access_rejected {
            claimed.invalidate_access();
        }
        let snapshot: AuthBootstrapSession = result?;
        validate_session_progress(&claimed.session, &snapshot)?;
        claimed.session = snapshot.clone();
        if claimed.session.state != AuthBootstrapState::Claimed {
            claimed.invalidate_access();
        }
        Ok(snapshot)
    }

    /// Appends one bounded non-secret status event to a claimed session.
    ///
    /// Sequence numbers are assigned monotonically in memory and advance only
    /// after the server accepts the event, so an ambiguous transport failure
    /// can safely retry the same sequence.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid event, rejected access token, transport
    /// failure, or a response that does not exactly match the submitted event.
    pub async fn record_event(
        &self,
        claimed: &mut ClaimedCaptureSession,
        kind: AuthBootstrapClientEventKind,
    ) -> anyhow::Result<CaptureEventReceipt> {
        claimed.ensure_live()?;
        let session_id = claimed.session.id;
        let sequence = claimed.next_sequence;
        AuthBootstrapClientEvent::new(session_id, sequence, kind.clone(), Utc::now())
            .context("Capture status event is invalid")?;
        let url = self
            .base_url
            .join(&format!(
                "api/v1/auth-bootstrap/sessions/{session_id}/events"
            ))
            .context("failed to construct the status event endpoint")?;
        let response = self
            .http
            .post(url)
            .header(
                AUTHORIZATION,
                bootstrap_authorization(&claimed.access_token)?,
            )
            .json(&RecordEventRequest {
                sequence,
                kind: &kind,
            })
            .send()
            .await
            .context("failed to record the Capture status event")?;
        let access_rejected = response.status() == StatusCode::UNAUTHORIZED;
        let result = deserialize_response(response, &[StatusCode::OK, StatusCode::CREATED]).await;
        if access_rejected {
            claimed.invalidate_access();
        }
        let receipt: CaptureEventReceipt = result?;
        receipt
            .event
            .validate()
            .context("Asterism server returned an invalid Capture status event")?;
        if receipt.event.session_id != session_id
            || receipt.event.sequence != sequence
            || receipt.event.kind != kind
        {
            bail!("Asterism server returned a mismatched Capture status event");
        }
        claimed.next_sequence += 1;
        Ok(receipt)
    }

    /// Submits one locally captured credential candidate for server-side
    /// Provider validation and atomic persistence.
    ///
    /// The serialized request body owns a zeroizing allocation until the HTTP
    /// request is dropped. A successful response immediately clears the scoped
    /// access token because Core has completed and invalidated the session.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid local metadata, rejected credentials,
    /// transport failure, or a response that is not bound to the current
    /// session and submitted credential shape.
    pub async fn submit_credential(
        &self,
        claimed: &mut ClaimedCaptureSession,
        submission: CaptureCredentialSubmission,
    ) -> anyhow::Result<CaptureCredentialAccepted> {
        claimed.ensure_live()?;
        submission.validate_for(&claimed.session)?;
        submission.validate_for_recipe(&claimed.recipe)?;
        let session_id = claimed.session.id;
        let expected_count = submission.fields.len();
        let expected_kind = submission.session_kind;
        let body = submission.serialize_for(&claimed.session)?;
        let url = self
            .base_url
            .join(&format!(
                "api/v1/auth-bootstrap/sessions/{session_id}/credential"
            ))
            .context("failed to construct the credential submission endpoint")?;
        let request = self
            .http
            .post(url)
            .header(
                AUTHORIZATION,
                bootstrap_authorization(&claimed.access_token)?,
            )
            .header(CONTENT_TYPE, "application/json")
            .body(Bytes::from_owner(ZeroizingRequestBody(body)));
        drop(submission);
        let response = request
            .send()
            .await
            .context("failed to submit the captured credential")?;
        let access_rejected = response.status() == StatusCode::UNAUTHORIZED;
        let result = deserialize_response(response, &[StatusCode::OK]).await;
        if access_rejected {
            claimed.invalidate_access();
        }
        let accepted: CaptureCredentialAccepted = result?;
        validate_session_progress(&claimed.session, &accepted.session)?;
        if accepted.session.state != AuthBootstrapState::Completed
            || accepted.session.provider_account_id != Some(accepted.provider_account_id)
            || accepted.credential_count != expected_count
            || !accepted.status.valid
            || accepted.status.kind != expected_kind
        {
            bail!("Asterism server returned an invalid credential acceptance response");
        }
        claimed.access_token.zeroize();
        claimed.session = accepted.session.clone();
        Ok(accepted)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureHealth {
    pub status: String,
    pub version: String,
    pub schema_version: i64,
}

pub struct ClaimedCaptureSession {
    session: AuthBootstrapSession,
    recipe: CaptureRecipe,
    access_token: SecretString,
    next_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrowserBridgeSessionSnapshot {
    pub id: BrowserBridgeSessionId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub spec_version: u32,
    pub state: BrowserBridgeSessionState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub claimed_at: Option<Timestamp>,
    pub revision: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrowserBridgeRuntimeBindingReceipt {
    pub session_id: BrowserBridgeSessionId,
    pub observed_origin: String,
    pub frame_id: String,
    pub bound_at: Timestamp,
    pub duplicate: bool,
}

pub struct BrowserBridgeCommand {
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub command_type: String,
    pub command_digest: [u8; 32],
    pub command_artifact: SecretValue,
}

impl fmt::Debug for BrowserBridgeCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeCommand")
            .field("session_id", &self.session_id)
            .field("sequence", &self.sequence)
            .field("command_type", &self.command_type)
            .field("command_digest", &"[HASHED]")
            .field("command_artifact", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct BrowserBridgeResultReceipt {
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub result_type: String,
    pub result_digest: [u8; 32],
    pub received_at: Timestamp,
    pub duplicate: bool,
}

impl fmt::Debug for BrowserBridgeResultReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserBridgeResultReceipt")
            .field("session_id", &self.session_id)
            .field("sequence", &self.sequence)
            .field("result_type", &self.result_type)
            .field("result_digest", &"[HASHED]")
            .field("received_at", &self.received_at)
            .field("duplicate", &self.duplicate)
            .finish()
    }
}

pub struct ClaimedBrowserBridgeSession {
    session: BrowserBridgeSessionSnapshot,
    spec: asterism_provider_api::BrowserSessionSpec,
    access_token: SecretString,
    binding: Option<BrowserBridgeRuntimeBindingReceipt>,
    next_sequence: u64,
    active_command: Option<ActiveBrowserBridgeCommand>,
}

impl ClaimedBrowserBridgeSession {
    pub const fn session(&self) -> &BrowserBridgeSessionSnapshot {
        &self.session
    }

    pub const fn spec(&self) -> &asterism_provider_api::BrowserSessionSpec {
        &self.spec
    }

    pub const fn binding(&self) -> Option<&BrowserBridgeRuntimeBindingReceipt> {
        self.binding.as_ref()
    }

    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn ensure_live(&mut self) -> anyhow::Result<()> {
        if self.session.state != BrowserBridgeSessionState::Claimed
            || self.session.expires_at <= Utc::now()
            || self.access_token.expose_secret().is_empty()
        {
            self.invalidate_access();
            bail!("BrowserBridge pairing session is no longer active");
        }
        Ok(())
    }

    fn invalidate_access(&mut self) {
        self.access_token.zeroize();
    }

    #[cfg(test)]
    fn access_token(&self) -> &SecretString {
        &self.access_token
    }
}

impl fmt::Debug for ClaimedBrowserBridgeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimedBrowserBridgeSession")
            .field("session", &self.session)
            .field("spec", &self.spec)
            .field("access_token", &self.access_token)
            .field("binding", &self.binding)
            .field("next_sequence", &self.next_sequence)
            .field("active_command", &self.active_command)
            .finish()
    }
}

struct ActiveBrowserBridgeCommand {
    sequence: u64,
    command_type: String,
    command_digest: [u8; 32],
}

impl fmt::Debug for ActiveBrowserBridgeCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActiveBrowserBridgeCommand")
            .field("sequence", &self.sequence)
            .field("command_type", &self.command_type)
            .field("command_digest", &"[HASHED]")
            .finish()
    }
}

#[derive(Serialize)]
struct BrowserBridgeRuntimeBindingRequest<'a> {
    observed_origin: &'a str,
    frame_id: &'a str,
}

#[derive(Deserialize)]
struct BrowserBridgeClaimResponse {
    session: BrowserBridgeSessionSnapshot,
    spec: asterism_provider_api::BrowserSessionSpec,
    access_token: String,
}

impl BrowserBridgeClaimResponse {
    fn into_session(
        mut self,
        expected_id: BrowserBridgeSessionId,
    ) -> anyhow::Result<ClaimedBrowserBridgeSession> {
        validate_browser_bridge_session(&self.session, expected_id, &self.spec)?;
        validate_browser_bridge_token(&self.access_token, false)?;
        Ok(ClaimedBrowserBridgeSession {
            session: self.session.clone(),
            spec: self.spec.clone(),
            access_token: SecretString::new(std::mem::take(&mut self.access_token)),
            binding: None,
            next_sequence: 1,
            active_command: None,
        })
    }
}

impl Drop for BrowserBridgeClaimResponse {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}

#[derive(Deserialize)]
struct BrowserBridgeResultReceiptWire {
    session_id: BrowserBridgeSessionId,
    sequence: u64,
    result_type: String,
    result_digest: String,
    received_at: Timestamp,
    duplicate: bool,
}

impl ClaimedCaptureSession {
    pub fn session(&self) -> &AuthBootstrapSession {
        &self.session
    }

    pub const fn recipe(&self) -> &CaptureRecipe {
        &self.recipe
    }

    fn ensure_live(&mut self) -> anyhow::Result<()> {
        if self.session.state != AuthBootstrapState::Claimed
            || self.session.is_expired_at(Utc::now())
            || self.access_token.expose_secret().is_empty()
        {
            self.invalidate_access();
            bail!("Capture pairing session is no longer active");
        }
        Ok(())
    }

    fn invalidate_access(&mut self) {
        self.access_token.zeroize();
    }

    #[cfg(test)]
    fn access_token(&self) -> &SecretString {
        &self.access_token
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureEventReceipt {
    pub event: AuthBootstrapClientEvent,
    pub duplicate: bool,
}

pub struct CaptureCredentialField {
    purpose: SecretPurpose,
    value: SecretString,
}

impl CaptureCredentialField {
    /// Builds one provider credential field held in zeroizing memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the purpose is not a provider credential or the
    /// plaintext is empty or exceeds the one-mebibyte transport boundary.
    pub fn new(purpose: SecretPurpose, value: SecretString) -> anyhow::Result<Self> {
        if !purpose.is_provider_credential()
            || value.expose_secret().is_empty()
            || value.expose_secret().len() > MAX_CREDENTIAL_FIELD_BYTES
        {
            bail!("captured credential field is invalid");
        }
        Ok(Self { purpose, value })
    }

    pub const fn purpose(&self) -> SecretPurpose {
        self.purpose
    }
}

impl fmt::Debug for CaptureCredentialField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureCredentialField")
            .field("purpose", &self.purpose)
            .field("value", &self.value)
            .finish()
    }
}

pub struct CaptureCredentialSubmission {
    display_name: Option<String>,
    tenant: Option<SecretString>,
    auth_method: AuthMethod,
    session_kind: SessionKind,
    expires_at: Option<Timestamp>,
    fields: Vec<CaptureCredentialField>,
}

impl CaptureCredentialSubmission {
    /// Builds one manual or assisted credential candidate without copying its
    /// plaintext fields into ordinary application strings.
    pub fn new(
        display_name: Option<String>,
        tenant: Option<SecretString>,
        auth_method: AuthMethod,
        session_kind: SessionKind,
        expires_at: Option<Timestamp>,
        fields: Vec<CaptureCredentialField>,
    ) -> Self {
        Self {
            display_name,
            tenant,
            auth_method,
            session_kind,
            expires_at,
            fields,
        }
    }

    fn validate_for(&self, session: &AuthBootstrapSession) -> anyhow::Result<()> {
        if session.state != AuthBootstrapState::Claimed {
            bail!("credential submission requires a claimed pairing session");
        }
        let valid_display_name = match session.purpose {
            AuthBootstrapPurpose::AddAccount => self.display_name.as_deref().is_some_and(|name| {
                !name.is_empty()
                    && name.len() <= 128
                    && name.trim() == name
                    && !name.chars().any(char::is_control)
            }),
            AuthBootstrapPurpose::Reauthenticate | AuthBootstrapPurpose::RepairSession => {
                self.display_name.is_none()
            }
        };
        if !valid_display_name {
            bail!("account display name does not match the pairing purpose");
        }
        if self.tenant.as_ref().is_some_and(|tenant| {
            tenant.expose_secret().is_empty()
                || tenant.expose_secret().len() > 256
                || tenant.expose_secret().trim() != tenant.expose_secret()
                || tenant.expose_secret().chars().any(char::is_control)
        }) {
            bail!("credential tenant metadata is invalid");
        }
        if self.fields.is_empty() || self.fields.len() > 16 {
            bail!("credential submission must contain 1-16 fields");
        }
        let purposes = self
            .fields
            .iter()
            .map(CaptureCredentialField::purpose)
            .collect::<HashSet<_>>();
        if purposes.len() != self.fields.len() {
            bail!("credential submission contains duplicate field purposes");
        }
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            bail!("credential expiry must be in the future");
        }
        Ok(())
    }

    fn validate_for_recipe(&self, recipe: &CaptureRecipe) -> anyhow::Result<()> {
        if self.auth_method != recipe.auth_method || self.session_kind != recipe.session_kind {
            bail!("credential submission does not match the claimed Capture recipe");
        }
        let purposes = self
            .fields
            .iter()
            .map(CaptureCredentialField::purpose)
            .collect::<HashSet<_>>();
        if purposes.iter().any(|purpose| {
            !recipe
                .outputs
                .iter()
                .any(|output| output.purpose == *purpose)
        }) || recipe
            .outputs
            .iter()
            .any(|output| output.required && !purposes.contains(&output.purpose))
        {
            bail!("credential fields do not match the claimed Capture recipe");
        }
        Ok(())
    }

    fn serialize_for(&self, session: &AuthBootstrapSession) -> anyhow::Result<Zeroizing<Vec<u8>>> {
        let fields = self
            .fields
            .iter()
            .map(|field| SubmitCredentialField {
                purpose: field.purpose,
                value: field.value.expose_secret(),
            })
            .collect();
        let request = SubmitCredentialRequest {
            display_name: self.display_name.as_deref(),
            provider_id: session.provider_id.as_str(),
            tenant: self.tenant.as_ref().map(SecretString::expose_secret),
            auth_method: self.auth_method,
            session_kind: self.session_kind,
            expires_at: self.expires_at,
            fields,
        };
        serde_json::to_vec(&request)
            .map(Zeroizing::new)
            .context("failed to serialize the captured credential")
    }
}

impl fmt::Debug for CaptureCredentialSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureCredentialSubmission")
            .field(
                "display_name",
                &self.display_name.as_ref().map(|_| "[REDACTED]"),
            )
            .field("tenant", &self.tenant.as_ref().map(|_| "[REDACTED]"))
            .field("auth_method", &self.auth_method)
            .field("session_kind", &self.session_kind)
            .field("expires_at", &self.expires_at)
            .field(
                "field_purposes",
                &self
                    .fields
                    .iter()
                    .map(|field| field.purpose)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureCredentialAccepted {
    pub session: AuthBootstrapSession,
    pub provider_account_id: ProviderAccountId,
    pub credential_count: usize,
    pub status: SessionStatus,
}

#[derive(Serialize)]
struct SubmitCredentialRequest<'a> {
    display_name: Option<&'a str>,
    provider_id: &'a str,
    tenant: Option<&'a str>,
    auth_method: AuthMethod,
    session_kind: SessionKind,
    expires_at: Option<Timestamp>,
    fields: Vec<SubmitCredentialField<'a>>,
}

#[derive(Serialize)]
struct SubmitCredentialField<'a> {
    purpose: SecretPurpose,
    value: &'a str,
}

struct ZeroizingRequestBody(Zeroizing<Vec<u8>>);

impl AsRef<[u8]> for ZeroizingRequestBody {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

#[derive(Serialize)]
struct RecordEventRequest<'a> {
    sequence: u64,
    kind: &'a AuthBootstrapClientEventKind,
}

impl fmt::Debug for ClaimedCaptureSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimedCaptureSession")
            .field("session", &self.session)
            .field("recipe", &self.recipe)
            .field("access_token", &self.access_token)
            .field("next_sequence", &self.next_sequence)
            .finish()
    }
}

#[derive(Deserialize)]
struct ClaimResponse {
    session: AuthBootstrapSession,
    access_token: String,
    recipe: CaptureRecipe,
}

impl ClaimResponse {
    fn into_session(
        mut self,
        expected_id: AuthBootstrapSessionId,
    ) -> anyhow::Result<ClaimedCaptureSession> {
        validate_session_binding(&self.session, expected_id)?;
        if self.session.state != AuthBootstrapState::Claimed {
            bail!("Asterism server returned an invalid claimed session");
        }
        self.recipe
            .validate()
            .context("Asterism server returned an invalid Capture recipe")?;
        if self.recipe.version != self.session.required_recipe_version {
            bail!("Asterism server returned a mismatched Capture recipe version");
        }
        validate_bootstrap_token(&self.access_token)?;
        Ok(ClaimedCaptureSession {
            session: self.session.clone(),
            recipe: self.recipe.clone(),
            access_token: SecretString::new(std::mem::take(&mut self.access_token)),
            next_sequence: 1,
        })
    }
}

impl Drop for ClaimResponse {
    fn drop(&mut self) {
        self.access_token.zeroize();
    }
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

async fn deserialize_response<T>(response: Response, expected: &[StatusCode]) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = Zeroizing::new(read_bounded_body(response).await?);
    if !expected.contains(&status) {
        if let Ok(error) = serde_json::from_slice::<ErrorResponse>(&body) {
            bail!(
                "Asterism server returned {status} ({}): {}",
                error.error.code,
                error.error.message
            );
        }
        bail!("Asterism server returned {status} with an invalid error response");
    }
    serde_json::from_slice(&body).context("Asterism server returned invalid JSON")
}

fn validate_session_binding(
    session: &AuthBootstrapSession,
    expected_id: AuthBootstrapSessionId,
) -> anyhow::Result<()> {
    session
        .validate()
        .context("Asterism server returned an invalid pairing session")?;
    if session.id != expected_id {
        bail!("Asterism server returned a differently bound pairing session");
    }
    Ok(())
}

fn validate_session_progress(
    previous: &AuthBootstrapSession,
    next: &AuthBootstrapSession,
) -> anyhow::Result<()> {
    validate_session_binding(next, previous.id)?;
    let stable_binding = next.owner_user_id == previous.owner_user_id
        && next.provider_id == previous.provider_id
        && next.purpose == previous.purpose
        && next.required_recipe_version == previous.required_recipe_version
        && next.created_at == previous.created_at
        && next.expires_at == previous.expires_at
        && next.claimed_at == previous.claimed_at;
    let account_binding = previous.provider_account_id.is_none()
        || next.provider_account_id == previous.provider_account_id;
    if !stable_binding
        || !account_binding
        || next.revision < previous.revision
        || next.updated_at < previous.updated_at
    {
        bail!("Asterism server returned an inconsistent pairing session update");
    }
    Ok(())
}

async fn read_bounded_body(response: Response) -> anyhow::Result<Vec<u8>> {
    read_bounded_body_with_limit(response, MAX_RESPONSE_BODY_BYTES).await
}

async fn read_bounded_body_with_limit(
    mut response: Response,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(limit).expect("response limit fits u64"))
    {
        bail!("Asterism server response exceeds the safety limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read the Asterism server response")?
    {
        if chunk.len() > limit.saturating_sub(body.len()) {
            bail!("Asterism server response exceeds the safety limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn bootstrap_authorization(token: &SecretString) -> anyhow::Result<HeaderValue> {
    validate_bootstrap_token(token.expose_secret())?;
    let plaintext = Zeroizing::new(format!("Bootstrap {}", token.expose_secret()).into_bytes());
    let shared = Bytes::from_owner(ZeroizingRequestBody(plaintext));
    let mut value = HeaderValue::from_maybe_shared(shared)
        .context("pairing token cannot be represented as an HTTP header")?;
    value.set_sensitive(true);
    Ok(value)
}

fn browser_bridge_authorization(
    token: &SecretString,
    pairing: bool,
) -> anyhow::Result<HeaderValue> {
    validate_browser_bridge_token(token.expose_secret(), pairing)?;
    let plaintext = Zeroizing::new(format!("BrowserBridge {}", token.expose_secret()).into_bytes());
    let shared = Bytes::from_owner(ZeroizingRequestBody(plaintext));
    let mut value = HeaderValue::from_maybe_shared(shared)
        .context("BrowserBridge token cannot be represented as an HTTP header")?;
    value.set_sensitive(true);
    Ok(value)
}

fn validate_browser_bridge_token(token: &str, pairing: bool) -> anyhow::Result<()> {
    let prefix = if pairing {
        "ast_bridge_pair_"
    } else {
        "ast_bridge_"
    };
    if !token.starts_with(prefix)
        || (!pairing && token.starts_with("ast_bridge_pair_"))
        || token.len() <= prefix.len()
        || token.len() > MAX_BOOTSTRAP_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        bail!("BrowserBridge token is empty or malformed");
    }
    Ok(())
}

fn validate_browser_bridge_session(
    session: &BrowserBridgeSessionSnapshot,
    expected_id: BrowserBridgeSessionId,
    spec: &asterism_provider_api::BrowserSessionSpec,
) -> anyhow::Result<()> {
    spec.validate()
        .context("Asterism server returned an invalid BrowserBridge policy")?;
    let provider_id = ProviderId::new(session.provider_id.as_str().to_owned())
        .context("Asterism server returned an invalid BrowserBridge Provider")?;
    let claimed_at = session
        .claimed_at
        .context("Asterism server returned an unclaimed BrowserBridge session")?;
    let ttl = session.expires_at.signed_duration_since(session.created_at);
    if session.id != expected_id
        || session.provider_id != provider_id
        || session.provider_version.is_empty()
        || session.provider_version.len() > 64
        || session.provider_version.trim() != session.provider_version
        || session.provider_version.chars().any(char::is_control)
        || session.spec_version != spec.version
        || session.state != BrowserBridgeSessionState::Claimed
        || session.revision != 2
        || ttl.num_seconds() <= 0
        || ttl.num_seconds() > 12 * 60 * 60
        || session.created_at > claimed_at
        || session.updated_at != claimed_at
        || claimed_at >= session.expires_at
        || session.expires_at <= Utc::now()
    {
        bail!("Asterism server returned a mismatched BrowserBridge session");
    }
    Ok(())
}

fn valid_provider_label(provider_id: &ProviderId, value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BROWSER_BRIDGE_LABEL_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

fn valid_browser_bridge_frame_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn require_octet_stream(response: &Response) -> anyhow::Result<()> {
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type == Some("application/octet-stream") {
        Ok(())
    } else {
        bail!("Asterism server returned a non-binary BrowserBridge command");
    }
}

fn decode_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (position, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        digest[position] = (high << 4) | low;
    }
    Some(digest)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validate_bootstrap_token(token: &str) -> anyhow::Result<()> {
    if token.is_empty()
        || token.len() > MAX_BOOTSTRAP_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        bail!("Bootstrap token is empty or malformed");
    }
    Ok(())
}

fn validate_base_url(value: &str, allow_insecure_loopback: bool) -> anyhow::Result<Url> {
    let mut url = Url::parse(value).context("ASTERISM_URL is not a valid URL")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        bail!("ASTERISM_URL must use https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("ASTERISM_URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("ASTERISM_URL must not contain a query or fragment");
    }
    if url.path() != "/" && !url.path().is_empty() {
        bail!("ASTERISM_URL must not contain a path");
    }
    let host = url.host_str().context("ASTERISM_URL must contain a host")?;
    let normalized_host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'));
    let local = host.eq_ignore_ascii_case("localhost")
        || normalized_host
            .unwrap_or(host)
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() == "http" && !(allow_insecure_loopback && local) {
        bail!("cleartext Capture connections require the explicit loopback-only development flag");
    }
    url.set_path("/");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{str::FromStr, sync::Arc};

    use axum::{
        Json, Router,
        body::{Body, Bytes as AxumBytes},
        extract::{Path, State},
        http::HeaderMap,
        response::Response as AxumResponse,
        routing::{get, post, put},
    };
    use chrono::{Duration as ChronoDuration, Utc};
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::Mutex};

    const SESSION_ID: &str = "0198d59e-0194-7ad5-8d2d-e0dbcb332db0";
    const ACCOUNT_ID: &str = "0198d59e-0194-7ad5-8d2d-e0dbcb332db2";
    type ObservedClaim = Arc<Mutex<Option<(String, String)>>>;
    type ObservedSessionRequests = Arc<Mutex<Vec<(String, String, Value)>>>;
    type ObservedCredential = Arc<Mutex<Option<(String, String, Value)>>>;

    #[test]
    fn capture_url_requires_https_by_default() {
        assert!(validate_base_url("https://example.test", false).is_ok());
        assert!(validate_base_url("http://127.0.0.1:8068", false).is_err());
        assert!(validate_base_url("http://example.test", true).is_err());
    }

    #[test]
    fn insecure_development_mode_remains_loopback_only() {
        assert!(validate_base_url("http://127.0.0.1:8068", true).is_ok());
        assert!(validate_base_url("http://[::1]:8068", true).is_ok());
        assert!(validate_base_url("http://localhost:8068", true).is_ok());
    }

    #[test]
    fn capture_url_rejects_embedded_authority_or_route_data() {
        assert!(validate_base_url("https://user:password@example.test", false).is_err());
        assert!(validate_base_url("https://example.test/base", false).is_err());
        assert!(validate_base_url("https://example.test?token=value", false).is_err());
        assert!(validate_base_url("asterism://auth", false).is_err());
    }

    #[test]
    fn browser_bridge_tokens_and_digests_have_distinct_canonical_shapes() {
        assert!(validate_browser_bridge_token("ast_bridge_pair_test", true).is_ok());
        assert!(validate_browser_bridge_token("ast_bridge_access_test", false).is_ok());
        assert!(validate_browser_bridge_token("ast_bridge_pair_test", false).is_err());
        assert!(decode_digest(&"ab".repeat(32)).is_some());
        assert!(decode_digest(&"AB".repeat(32)).is_none());
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the transport regression keeps claim, binding, one-shot dispatch, digest verification and result receipt in one session"
    )]
    async fn browser_bridge_transport_is_bound_digest_checked_and_one_command_at_a_time() {
        let command = br#"{"version":1,"action":"scan"}"#;
        let command_digest: [u8; 32] = Sha256::digest(command).into();
        let command_digest_hex = encode_test_digest(command_digest);
        let app = Router::new()
            .route(
                "/api/v1/browser-bridge/sessions/{session_id}/claim",
                post(
                    |Path(session_id): Path<String>, headers: HeaderMap| async move {
                        assert_eq!(session_id, SESSION_ID);
                        assert_eq!(
                            authorization_text(&headers),
                            "BrowserBridge ast_bridge_pair_test"
                        );
                        let claimed_at = Utc::now();
                        Json(json!({
                            "session": {
                                "id": SESSION_ID,
                                "task_id": ACCOUNT_ID,
                                "provider_id": "uai",
                                "provider_version": "test",
                                "spec_version": 1,
                                "state": "claimed",
                                "created_at": claimed_at - ChronoDuration::seconds(1),
                                "updated_at": claimed_at,
                                "expires_at": claimed_at + ChronoDuration::minutes(10),
                                "claimed_at": claimed_at,
                                "revision": 2
                            },
                            "spec": {
                                "version": 1,
                                "start_url": "https://ucontent.unipus.cn/task/a",
                                "isolation_key": "uai-task-a",
                                "allowed_origins": ["https://ucontent.unipus.cn"],
                                "headless": false
                            },
                            "access_token": "ast_bridge_access_test"
                        }))
                    },
                ),
            )
            .route(
                "/api/v1/browser-bridge/sessions/{session_id}/binding",
                put(
                    |Path(session_id): Path<String>,
                     headers: HeaderMap,
                     Json(payload): Json<Value>| async move {
                        assert_eq!(session_id, SESSION_ID);
                        assert_eq!(
                            authorization_text(&headers),
                            "BrowserBridge ast_bridge_access_test"
                        );
                        Json(json!({
                            "session_id": SESSION_ID,
                            "observed_origin": payload["observed_origin"],
                            "frame_id": payload["frame_id"],
                            "bound_at": Utc::now(),
                            "duplicate": false
                        }))
                    },
                ),
            )
            .route(
                "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}",
                get({
                    let command_digest_hex = command_digest_hex.clone();
                    move |Path((session_id, sequence)): Path<(String, u64)>, headers: HeaderMap| {
                        let command_digest_hex = command_digest_hex.clone();
                        async move {
                            assert_eq!((session_id.as_str(), sequence), (SESSION_ID, 1));
                            assert_eq!(
                                authorization_text(&headers),
                                "BrowserBridge ast_bridge_access_test"
                            );
                            AxumResponse::builder()
                                .header(CONTENT_TYPE, "application/octet-stream")
                                .header(X_BROWSER_COMMAND_TYPE, "uai.browser.scan.v1")
                                .header(X_BROWSER_COMMAND_DIGEST, command_digest_hex)
                                .body(Body::from(command.as_slice()))
                                .unwrap()
                        }
                    }
                }),
            )
            .route(
                "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}/result",
                post(
                    |Path((session_id, sequence)): Path<(String, u64)>,
                     headers: HeaderMap,
                     body: AxumBytes| async move {
                        assert_eq!((session_id.as_str(), sequence), (SESSION_ID, 1));
                        assert_eq!(
                            authorization_text(&headers),
                            "BrowserBridge ast_bridge_access_test"
                        );
                        assert_eq!(headers[X_BROWSER_RESULT_TYPE], "uai.browser.scan-result.v1");
                        let result_digest: [u8; 32] = Sha256::digest(&body).into();
                        (
                            StatusCode::ACCEPTED,
                            Json(json!({
                                "session_id": SESSION_ID,
                                "sequence": 1,
                                "result_type": "uai.browser.scan-result.v1",
                                "result_digest": encode_test_digest(result_digest),
                                "received_at": Utc::now(),
                                "duplicate": false
                            })),
                        )
                    },
                ),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CaptureClient::new(&format!("http://{address}"), true).unwrap();
        let session_id = BrowserBridgeSessionId::from_str(SESSION_ID).unwrap();
        let pairing_token = SecretString::new("ast_bridge_pair_test");

        let mut claimed = client
            .claim_browser_bridge(session_id, &pairing_token)
            .await
            .unwrap();
        assert_eq!(claimed.session().id, session_id);
        assert_eq!(
            claimed.access_token().expose_secret(),
            "ast_bridge_access_test"
        );
        assert!(!format!("{claimed:?}").contains("ast_bridge_access_test"));
        let binding = client
            .bind_browser_bridge_runtime(&mut claimed, "https://ucontent.unipus.cn", "top-frame:1")
            .await
            .unwrap();
        assert_eq!(binding.frame_id, "top-frame:1");

        let dispatched = client
            .dispatch_browser_bridge_command(&mut claimed)
            .await
            .unwrap();
        assert_eq!(dispatched.command_digest, command_digest);
        assert_eq!(dispatched.command_artifact.expose_secret(), command);
        assert!(!format!("{dispatched:?}").contains("action"));
        assert!(
            client
                .dispatch_browser_bridge_command(&mut claimed)
                .await
                .is_err()
        );

        let raw_result = SecretValue::new(br#"{"version":1,"tasks":2}"#.to_vec());
        let receipt = client
            .submit_browser_bridge_result(&mut claimed, "uai.browser.scan-result.v1", &raw_result)
            .await
            .unwrap();
        let expected_result_digest: [u8; 32] = Sha256::digest(raw_result.expose_secret()).into();
        assert_eq!(receipt.result_digest, expected_result_digest);
        assert_eq!(claimed.next_sequence(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn claim_is_path_bound_and_keeps_access_token_redacted() {
        let observed = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/v1/auth-bootstrap/sessions/{session_id}/claim",
                post(
                    |State(observed): State<ObservedClaim>,
                     Path(session_id): Path<String>,
                     headers: HeaderMap| async move {
                        let authorization = headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        *observed.lock().await = Some((session_id, authorization));
                        let now = Utc::now();
                        Json(json!({
                            "session": {
                                "id": SESSION_ID,
                                "owner_user_id": "0198d59e-0194-7ad5-8d2d-e0dbcb332db1",
                                "provider_id": "cidaren",
                                "provider_account_id": null,
                                "purpose": "add_account",
                                "required_recipe_version": 1,
                                "state": "claimed",
                                "created_at": now,
                                "updated_at": now,
                                "expires_at": now + ChronoDuration::minutes(10),
                                "claimed_at": now,
                                "revision": 2
                            },
                            "access_token": "ast_boot_access_test",
                            "recipe": capture_recipe_json()
                        }))
                    },
                ),
            )
            .with_state(observed.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CaptureClient::new(&format!("http://{address}"), true).unwrap();
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();
        let pairing_token = SecretString::new("ast_pair_test");

        let claimed = client.claim(session_id, &pairing_token).await.unwrap();

        assert_eq!(claimed.session().id, session_id);
        assert_eq!(claimed.session().state, AuthBootstrapState::Claimed);
        assert_eq!(
            claimed.access_token().expose_secret(),
            "ast_boot_access_test"
        );
        assert!(!format!("{claimed:?}").contains("ast_boot_access_test"));
        assert_eq!(
            observed.lock().await.as_ref(),
            Some(&(SESSION_ID.to_owned(), "Bootstrap ast_pair_test".to_owned()))
        );
        server.abort();
    }

    #[tokio::test]
    async fn claimed_session_polls_and_records_contiguous_bound_events() {
        let observed = ObservedSessionRequests::default();
        let app = Router::new()
            .route(
                "/api/v1/auth-bootstrap/sessions/{session_id}/stream",
                get(
                    |State(observed): State<ObservedSessionRequests>,
                     Path(session_id): Path<String>,
                     headers: HeaderMap| async move {
                        observed.lock().await.push((
                            format!("stream:{session_id}"),
                            authorization_text(&headers),
                            Value::Null,
                        ));
                        Json(claimed_session_json())
                    },
                ),
            )
            .route(
                "/api/v1/auth-bootstrap/sessions/{session_id}/events",
                post(
                    |State(observed): State<ObservedSessionRequests>,
                     Path(session_id): Path<String>,
                     headers: HeaderMap,
                     Json(payload): Json<Value>| async move {
                        observed.lock().await.push((
                            format!("event:{session_id}"),
                            authorization_text(&headers),
                            payload.clone(),
                        ));
                        (
                            StatusCode::CREATED,
                            Json(json!({
                                "event": {
                                    "session_id": SESSION_ID,
                                    "sequence": payload["sequence"],
                                    "kind": payload["kind"],
                                    "received_at": Utc::now()
                                },
                                "duplicate": false
                            })),
                        )
                    },
                ),
            )
            .with_state(observed.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CaptureClient::new(&format!("http://{address}"), true).unwrap();
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();
        let mut claimed = claimed_response().into_session(session_id).unwrap();

        let ready = client
            .record_event(&mut claimed, AuthBootstrapClientEventKind::ClientReady)
            .await
            .unwrap();
        let progress = client
            .record_event(
                &mut claimed,
                AuthBootstrapClientEventKind::Progress {
                    stage: "manual.input".to_owned(),
                    percent: 50,
                },
            )
            .await
            .unwrap();
        let snapshot = client.poll_session(&mut claimed).await.unwrap();

        assert_eq!(ready.event.sequence, 1);
        assert_eq!(progress.event.sequence, 2);
        assert_eq!(snapshot.id, session_id);
        let observed = observed.lock().await;
        assert_eq!(observed.len(), 3);
        assert!(
            observed
                .iter()
                .all(|(_, authorization, _)| authorization == "Bootstrap ast_boot_access_test")
        );
        assert_eq!(observed[0].2["sequence"], 1);
        assert_eq!(observed[1].2["sequence"], 2);
        assert_eq!(observed[2].0, format!("stream:{SESSION_ID}"));
        server.abort();
    }

    #[tokio::test]
    async fn credential_submission_is_bound_and_clears_completed_access() {
        let observed = ObservedCredential::default();
        let app = Router::new()
            .route(
                "/api/v1/auth-bootstrap/sessions/{session_id}/credential",
                post(
                    |State(observed): State<ObservedCredential>,
                     Path(session_id): Path<String>,
                     headers: HeaderMap,
                     Json(payload): Json<Value>| async move {
                        *observed.lock().await =
                            Some((session_id, authorization_text(&headers), payload));
                        Json(json!({
                            "session": completed_session_json(),
                            "provider_account_id": ACCOUNT_ID,
                            "credential_count": 1,
                            "status": {
                                "valid": true,
                                "kind": "cookie",
                                "expires_at": null,
                                "account_hint": null
                            }
                        }))
                    },
                ),
            )
            .with_state(observed.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CaptureClient::new(&format!("http://{address}"), true).unwrap();
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();
        let mut claimed = claimed_response().into_session(session_id).unwrap();
        let submission = CaptureCredentialSubmission::new(
            Some("Primary account".to_owned()),
            None,
            AuthMethod::AssistedSession,
            SessionKind::Cookie,
            None,
            vec![
                CaptureCredentialField::new(
                    SecretPurpose::ProviderCookie,
                    SecretString::new("captured-cookie-test"),
                )
                .unwrap(),
            ],
        );
        assert!(!format!("{submission:?}").contains("captured-cookie-test"));

        let accepted = client
            .submit_credential(&mut claimed, submission)
            .await
            .unwrap();

        assert_eq!(accepted.session.state, AuthBootstrapState::Completed);
        assert!(accepted.status.valid);
        assert!(claimed.access_token().expose_secret().is_empty());
        let observed = observed.lock().await;
        let (observed_id, authorization, payload) = observed.as_ref().unwrap();
        assert_eq!(observed_id, SESSION_ID);
        assert_eq!(authorization, "Bootstrap ast_boot_access_test");
        assert_eq!(payload["provider_id"], "cidaren");
        assert_eq!(payload["fields"][0]["purpose"], "provider_cookie");
        assert_eq!(payload["fields"][0]["value"], "captured-cookie-test");
        server.abort();
    }

    #[tokio::test]
    async fn rejected_or_locally_expired_access_is_cleared_immediately() {
        let app = Router::new().route(
            "/api/v1/auth-bootstrap/sessions/{session_id}/stream",
            get(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({
                        "error": {
                            "code": "invalid_bootstrap_token",
                            "message": "the Bootstrap token is invalid or expired"
                        }
                    })),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CaptureClient::new(&format!("http://{address}"), true).unwrap();
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();
        let mut rejected = claimed_response().into_session(session_id).unwrap();

        assert!(client.poll_session(&mut rejected).await.is_err());
        assert!(rejected.access_token().expose_secret().is_empty());

        let now = Utc::now();
        let expired: ClaimResponse = serde_json::from_value(json!({
            "session": {
                "id": SESSION_ID,
                "owner_user_id": "0198d59e-0194-7ad5-8d2d-e0dbcb332db1",
                "provider_id": "cidaren",
                "provider_account_id": null,
                "purpose": "add_account",
                "required_recipe_version": 1,
                "state": "claimed",
                "created_at": now - ChronoDuration::minutes(20),
                "updated_at": now - ChronoDuration::minutes(19),
                "expires_at": now - ChronoDuration::minutes(10),
                "claimed_at": now - ChronoDuration::minutes(19),
                "revision": 2
            },
            "access_token": "ast_boot_access_test",
            "recipe": capture_recipe_json()
        }))
        .unwrap();
        let mut expired = expired.into_session(session_id).unwrap();
        assert!(client.poll_session(&mut expired).await.is_err());
        assert!(expired.access_token().expose_secret().is_empty());
        server.abort();
    }

    #[test]
    fn claim_rejects_a_mismatched_or_non_claimed_response() {
        let now = Utc::now();
        let response: ClaimResponse = serde_json::from_value(json!({
            "session": {
                "id": SESSION_ID,
                "owner_user_id": "0198d59e-0194-7ad5-8d2d-e0dbcb332db1",
                "provider_id": "cidaren",
                "provider_account_id": null,
                "purpose": "add_account",
                "required_recipe_version": 1,
                "state": "completed",
                "created_at": now,
                "updated_at": now,
                "expires_at": now + ChronoDuration::minutes(10),
                "claimed_at": now,
                "revision": 3
            },
            "access_token": "ast_boot_access_test",
            "recipe": capture_recipe_json()
        }))
        .unwrap();
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();

        assert!(response.into_session(session_id).is_err());
    }

    #[test]
    fn bootstrap_authorization_is_marked_sensitive() {
        let token = SecretString::new("ast_pair_test");
        let authorization = bootstrap_authorization(&token).unwrap();
        assert!(authorization.is_sensitive());
        assert!(!format!("{authorization:?}").contains("ast_pair_test"));
    }

    #[test]
    fn credential_submission_rejects_duplicate_fields_and_wrong_account_metadata() {
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();
        let claimed = claimed_response().into_session(session_id).unwrap();
        let duplicate_fields = CaptureCredentialSubmission::new(
            Some("Primary account".to_owned()),
            None,
            AuthMethod::ImportedCookie,
            SessionKind::Cookie,
            None,
            vec![
                CaptureCredentialField::new(
                    SecretPurpose::ProviderCookie,
                    SecretString::new("first-cookie"),
                )
                .unwrap(),
                CaptureCredentialField::new(
                    SecretPurpose::ProviderCookie,
                    SecretString::new("second-cookie"),
                )
                .unwrap(),
            ],
        );
        assert!(duplicate_fields.validate_for(claimed.session()).is_err());

        let missing_display_name = CaptureCredentialSubmission::new(
            None,
            None,
            AuthMethod::ImportedCookie,
            SessionKind::Cookie,
            None,
            vec![
                CaptureCredentialField::new(
                    SecretPurpose::ProviderCookie,
                    SecretString::new("captured-cookie"),
                )
                .unwrap(),
            ],
        );
        assert!(
            missing_display_name
                .validate_for(claimed.session())
                .is_err()
        );
    }

    fn authorization_text(headers: &HeaderMap) -> String {
        headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    fn encode_test_digest(digest: [u8; 32]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(64);
        for byte in digest {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }

    fn claimed_response() -> ClaimResponse {
        serde_json::from_value(json!({
            "session": claimed_session_json(),
            "access_token": "ast_boot_access_test",
            "recipe": capture_recipe_json()
        }))
        .unwrap()
    }

    fn capture_recipe_json() -> Value {
        json!({
            "version": 1,
            "start_url": "https://app.vocabgo.com/student/",
            "navigation_origins": ["https://app.vocabgo.com"],
            "read_origins": ["https://app.vocabgo.com"],
            "poll_interval_millis": 800,
            "auth_method": "assisted_session",
            "session_kind": "cookie",
            "readiness": {"type": "outputs_complete"},
            "outputs": [{
                "purpose": "provider_cookie",
                "required": true,
                "sources": [{
                    "type": "cookie_header",
                    "origin": "https://app.vocabgo.com"
                }]
            }]
        })
    }

    fn claimed_session_json() -> Value {
        json!({
            "id": SESSION_ID,
            "owner_user_id": "0198d59e-0194-7ad5-8d2d-e0dbcb332db1",
            "provider_id": "cidaren",
            "provider_account_id": null,
            "purpose": "add_account",
            "required_recipe_version": 1,
            "state": "claimed",
            "created_at": "2099-08-09T00:00:00Z",
            "updated_at": "2099-08-09T00:00:01Z",
            "expires_at": "2099-08-09T00:10:00Z",
            "claimed_at": "2099-08-09T00:00:01Z",
            "revision": 2
        })
    }

    fn completed_session_json() -> Value {
        let mut session = claimed_session_json();
        session["provider_account_id"] = Value::String(ACCOUNT_ID.to_owned());
        session["state"] = Value::String("completed".to_owned());
        session["updated_at"] = Value::String("2099-08-09T00:00:02Z".to_owned());
        session["revision"] = Value::from(3);
        session
    }
}
