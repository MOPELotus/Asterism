use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::{Client, Response, StatusCode, Url, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct CaptureClient {
    base_url: Url,
    http: Client,
}

impl CaptureClient {
    pub fn new(base_url: &str, allow_insecure_loopback: bool) -> anyhow::Result<Self> {
        let base_url = validate_base_url(base_url, allow_insecure_loopback)?;
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build the Asterism Capture HTTP client")?;
        Ok(Self { base_url, http })
    }

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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureHealth {
    pub status: String,
    pub version: String,
    pub schema_version: i64,
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
    let body = read_bounded_body(response).await?;
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
}
