use std::{fmt, time::Duration};

use anyhow::{Context, bail};
use asterism_domain::{AuthBootstrapSession, AuthBootstrapSessionId, AuthBootstrapState};
use asterism_secrets::SecretString;
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
        deserialize_response(response, StatusCode::OK).await
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
        let claimed: ClaimResponse = deserialize_response(response, StatusCode::OK).await?;
        claimed.into_session(session_id)
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

impl fmt::Debug for ClaimedCaptureSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimedCaptureSession")
            .field("session", &self.session)
            .field("access_token", &self.access_token)
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
        if self.session.id != expected_id || self.session.state != AuthBootstrapState::Claimed {
            bail!("Asterism server returned an invalid claimed session");
        }
        validate_bootstrap_token(&self.access_token)?;
        Ok(ClaimedCaptureSession {
            session: self.session.clone(),
            access_token: SecretString::new(std::mem::take(&mut self.access_token)),
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

async fn deserialize_response<T>(response: Response, expected: StatusCode) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = Zeroizing::new(read_bounded_body(response).await?);
    if status != expected {
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
        bail!("pairing token is empty or malformed");
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
        routing::post,
    };
    use chrono::{Duration as ChronoDuration, Utc};
    use serde_json::json;
    use tokio::{net::TcpListener, sync::Mutex};

    const SESSION_ID: &str = "0198d59e-0194-7ad5-8d2d-e0dbcb332db0";
    type ObservedClaim = Arc<Mutex<Option<(String, String)>>>;

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
}
