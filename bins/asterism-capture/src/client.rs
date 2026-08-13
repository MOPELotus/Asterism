use std::{collections::HashSet, fmt, time::Duration};

use anyhow::{Context, bail};
use asterism_domain::{
    AuthBootstrapClientEvent, AuthBootstrapClientEventKind, AuthBootstrapPurpose,
    AuthBootstrapSession, AuthBootstrapSessionId, AuthBootstrapState, AuthMethod,
    ProviderAccountId, SessionKind, Timestamp,
};
use asterism_provider_api::{CaptureRecipe, SessionStatus};
use asterism_secrets::{SecretPurpose, SecretString};
use bytes::Bytes;
use chrono::Utc;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::{Zeroize, Zeroizing};

const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const MAX_BOOTSTRAP_TOKEN_BYTES: usize = 128;
const MAX_CREDENTIAL_FIELD_BYTES: usize = 1024 * 1024;

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

async fn read_bounded_body(mut response: Response) -> anyhow::Result<Vec<u8>> {
    if response.content_length().is_some_and(|length| {
        length > u64::try_from(MAX_RESPONSE_BODY_BYTES).expect("response limit fits u64")
    }) {
        bail!("Asterism server response exceeds the safety limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read the Asterism server response")?
    {
        if chunk.len() > MAX_RESPONSE_BODY_BYTES.saturating_sub(body.len()) {
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
        extract::{Path, State},
        http::HeaderMap,
        routing::{get, post},
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
            "allowed_origins": ["https://app.vocabgo.com"],
            "poll_interval_millis": 800,
            "auth_method": "assisted_session",
            "session_kind": "cookie",
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
