use std::{fmt, time::Duration};

use anyhow::{Context, bail};
use asterism_domain::{
    AuthBootstrapClientEvent, AuthBootstrapClientEventKind, AuthBootstrapSession,
    AuthBootstrapSessionId, AuthBootstrapState,
};
use asterism_secrets::SecretString;
use chrono::Utc;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{AUTHORIZATION, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::{Zeroize, Zeroizing};

const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const MAX_BOOTSTRAP_TOKEN_BYTES: usize = 128;

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
        let snapshot: AuthBootstrapSession =
            deserialize_response(response, &[StatusCode::OK]).await?;
        validate_session_binding(&snapshot, session_id)?;
        claimed.session = snapshot.clone();
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
        let receipt: CaptureEventReceipt =
            deserialize_response(response, &[StatusCode::OK, StatusCode::CREATED]).await?;
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureHealth {
    pub status: String,
    pub version: String,
    pub schema_version: i64,
}

pub struct ClaimedCaptureSession {
    session: AuthBootstrapSession,
    access_token: SecretString,
    next_sequence: u64,
}

impl ClaimedCaptureSession {
    pub fn session(&self) -> &AuthBootstrapSession {
        &self.session
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
            .field("access_token", &self.access_token)
            .field("next_sequence", &self.next_sequence)
            .finish()
    }
}

#[derive(Deserialize)]
struct ClaimResponse {
    session: AuthBootstrapSession,
    access_token: String,
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
        validate_bootstrap_token(&self.access_token)?;
        Ok(ClaimedCaptureSession {
            session: self.session.clone(),
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
    let plaintext = Zeroizing::new(format!("Bootstrap {}", token.expose_secret()));
    let mut value = HeaderValue::from_str(&plaintext)
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
    type ObservedClaim = Arc<Mutex<Option<(String, String)>>>;
    type ObservedSessionRequests = Arc<Mutex<Vec<(String, String, Value)>>>;

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
                            "access_token": "ast_boot_access_test"
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
            "access_token": "ast_boot_access_test"
        }))
        .unwrap();
        let session_id = AuthBootstrapSessionId::from_str(SESSION_ID).unwrap();

        assert!(response.into_session(session_id).is_err());
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
            "access_token": "ast_boot_access_test"
        }))
        .unwrap()
    }

    fn claimed_session_json() -> Value {
        let now = Utc::now();
        json!({
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
        })
    }
}
