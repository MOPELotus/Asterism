use std::{
    collections::BTreeSet,
    fmt,
    io::{self, Write},
    time::Duration,
};

use anyhow::{Context, bail};
use asterism_domain::{ServiceScope, ServiceToken};
use asterism_secrets::SecretString;
use reqwest::{
    Client, Method, RequestBuilder, Response, StatusCode, Url,
    header::{self, HeaderMap},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroize;

const SESSION_COOKIE_NAME: &str = "asterism_session";
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct ApiClient {
    base_url: Url,
    http: Client,
}

impl ApiClient {
    pub fn new(base_url: &str) -> anyhow::Result<Self> {
        let base_url = validate_base_url(base_url)?;
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build Asterism HTTP client")?;
        Ok(Self { base_url, http })
    }

    pub async fn get_public(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        let request = self.request(Method::GET, path)?;
        send_json(request, StatusCode::OK).await
    }

    pub async fn get_authorized(
        &self,
        path: &str,
        token: &SecretString,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self
            .request(Method::GET, path)?
            .bearer_auth(token.expose_secret());
        send_json(request, StatusCode::OK).await
    }

    pub async fn get_authorized_with_query(
        &self,
        path: &str,
        token: &SecretString,
        query: &impl Serialize,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self
            .request(Method::GET, path)?
            .bearer_auth(token.expose_secret())
            .query(query);
        send_json(request, StatusCode::OK).await
    }

    pub async fn post_authorized(
        &self,
        path: &str,
        token: &SecretString,
        body: &impl Serialize,
        expected: StatusCode,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self
            .request(Method::POST, path)?
            .bearer_auth(token.expose_secret())
            .json(body);
        send_json(request, expected).await
    }

    pub async fn post_authorized_empty(
        &self,
        path: &str,
        token: &SecretString,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self
            .request(Method::POST, path)?
            .bearer_auth(token.expose_secret());
        send_json(request, StatusCode::OK).await
    }

    pub async fn put_authorized(
        &self,
        path: &str,
        token: &SecretString,
        body: &impl Serialize,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self
            .request(Method::PUT, path)?
            .bearer_auth(token.expose_secret())
            .json(body);
        send_json(request, StatusCode::OK).await
    }

    pub async fn delete_authorized(&self, path: &str, token: &SecretString) -> anyhow::Result<()> {
        let request = self
            .request(Method::DELETE, path)?
            .bearer_auth(token.expose_secret());
        send_empty(request, StatusCode::NO_CONTENT).await
    }

    pub async fn delete_authorized_json(
        &self,
        path: &str,
        token: &SecretString,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self
            .request(Method::DELETE, path)?
            .bearer_auth(token.expose_secret());
        send_json(request, StatusCode::OK).await
    }

    pub async fn establish_session(
        &self,
        path: &str,
        username: &str,
        password: &SecretString,
    ) -> anyhow::Result<SessionCookie> {
        let response = self
            .request(Method::POST, path)?
            .json(&CredentialsRequest {
                username,
                password: password.expose_secret(),
            })
            .send()
            .await
            .context("failed to request an Asterism Web Session")?;
        if response.status() != StatusCode::OK {
            return Err(api_error(response).await);
        }
        let cookie = extract_session_cookie(response.headers())?;
        let _: serde_json::Value = deserialize_response(response).await?;
        Ok(cookie)
    }

    pub async fn create_service_token_with_session(
        &self,
        session: &SessionCookie,
        request: &CreateServiceTokenRequest,
    ) -> anyhow::Result<IssuedServiceToken> {
        let request = self
            .request(Method::POST, "/api/v1/service-tokens")?
            .header(header::COOKIE, session.expose_secret())
            .json(request);
        send_json(request, StatusCode::OK).await
    }

    pub async fn create_service_token_with_bearer(
        &self,
        token: &SecretString,
        request: &CreateServiceTokenRequest,
    ) -> anyhow::Result<IssuedServiceToken> {
        let request = self
            .request(Method::POST, "/api/v1/service-tokens")?
            .bearer_auth(token.expose_secret())
            .json(request);
        send_json(request, StatusCode::OK).await
    }

    pub async fn revoke_service_token(
        &self,
        token: &SecretString,
        token_id: &str,
    ) -> anyhow::Result<()> {
        let path = format!("/api/v1/service-tokens/{token_id}");
        self.delete_authorized(&path, token).await
    }

    pub async fn logout(&self, session: &SessionCookie) -> anyhow::Result<()> {
        let request = self
            .request(Method::POST, "/api/v1/auth/logout")?
            .header(header::COOKIE, session.expose_secret());
        send_empty(request, StatusCode::NO_CONTENT).await
    }

    fn request(&self, method: Method, path: &str) -> anyhow::Result<RequestBuilder> {
        let url = self
            .base_url
            .join(path)
            .with_context(|| format!("invalid Asterism API path {path}"))?;
        Ok(self.http.request(method, url))
    }
}

#[derive(Debug, Serialize)]
pub struct CreateServiceTokenRequest {
    pub name: String,
    pub scopes: BTreeSet<ServiceScope>,
    pub expires_in_seconds: Option<u64>,
}

#[derive(Deserialize, Serialize)]
pub struct IssuedServiceToken {
    token: String,
    metadata: ServiceToken,
}

impl fmt::Debug for IssuedServiceToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedServiceToken")
            .field("token", &"[REDACTED]")
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl Drop for IssuedServiceToken {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

#[derive(Debug)]
pub struct SessionCookie(SecretString);

impl SessionCookie {
    fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

#[derive(Serialize)]
struct CredentialsRequest<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

pub fn write_json(value: &impl Serialize) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer_pretty(&mut output, value).context("failed to write JSON output")?;
    writeln!(output).context("failed to finish JSON output")
}

async fn send_json<T>(request: RequestBuilder, expected: StatusCode) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let response = request
        .send()
        .await
        .context("failed to request the Asterism API")?;
    if response.status() != expected {
        return Err(api_error(response).await);
    }
    deserialize_response(response).await
}

async fn send_empty(request: RequestBuilder, expected: StatusCode) -> anyhow::Result<()> {
    let response = request
        .send()
        .await
        .context("failed to request the Asterism API")?;
    if response.status() != expected {
        return Err(api_error(response).await);
    }
    Ok(())
}

async fn deserialize_response<T>(response: Response) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let bytes = read_bounded_body(response, MAX_RESPONSE_BODY_BYTES).await?;
    serde_json::from_slice(&bytes).context("Asterism API returned invalid JSON")
}

async fn api_error(response: Response) -> anyhow::Error {
    let status = response.status();
    let bytes = match read_bounded_body(response, MAX_ERROR_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return anyhow::anyhow!("Asterism API returned {status}; {error}");
        }
    };
    if let Ok(body) = serde_json::from_slice::<ErrorResponse>(&bytes) {
        anyhow::anyhow!(
            "Asterism API returned {status} ({}): {}",
            body.error.code,
            body.error.message
        )
    } else {
        anyhow::anyhow!("Asterism API returned {status} with an invalid error response")
    }
}

async fn read_bounded_body(mut response: Response, maximum: usize) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(maximum).expect("response limit fits u64"))
    {
        bail!("response body exceeds the configured safety limit");
    }
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default()
        .min(maximum);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read response body")?
    {
        if chunk.len() > maximum.saturating_sub(body.len()) {
            bail!("response body exceeds the configured safety limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn extract_session_cookie(headers: &HeaderMap) -> anyhow::Result<SessionCookie> {
    for value in headers.get_all(header::SET_COOKIE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        let Some(pair) = value.split(';').next() else {
            continue;
        };
        let Some((name, token)) = pair.split_once('=') else {
            continue;
        };
        if name == SESSION_COOKIE_NAME
            && token.starts_with("ast_ws_")
            && token.len() <= 512
            && !token.chars().any(char::is_control)
        {
            return Ok(SessionCookie(SecretString::new(pair)));
        }
    }
    bail!("Asterism API did not return a valid Web Session cookie")
}

fn validate_base_url(value: &str) -> anyhow::Result<Url> {
    let mut url = Url::parse(value).context("ASTERISM_URL is not a valid URL")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        bail!("ASTERISM_URL must use http or https");
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
    let local_http = host.eq_ignore_ascii_case("localhost")
        || normalized_host
            .unwrap_or(host)
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() == "http" && !local_http {
        bail!("cleartext ASTERISM_URL is allowed only for loopback hosts");
    }
    url.set_path("/");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    #[test]
    fn base_url_rejects_credentials_and_remote_cleartext() {
        assert!(validate_base_url("http://user:password@127.0.0.1:8068").is_err());
        assert!(validate_base_url("http://example.test:8068").is_err());
        assert!(validate_base_url("https://example.test").is_ok());
        assert!(validate_base_url("http://[::1]:8068").is_ok());
    }

    #[test]
    fn session_cookie_extraction_keeps_only_the_cookie_pair() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_static(
                "asterism_session=ast_ws_example; Path=/; HttpOnly; SameSite=Strict",
            ),
        );
        let cookie = extract_session_cookie(&headers).unwrap();
        assert_eq!(cookie.expose_secret(), "asterism_session=ast_ws_example");
        assert_eq!(
            format!("{cookie:?}"),
            "SessionCookie(SecretString([REDACTED]))"
        );
    }

    #[test]
    fn issued_token_debug_output_is_redacted() {
        let response: IssuedServiceToken = serde_json::from_value(serde_json::json!({
            "token": "ast_st_example",
            "metadata": {
                "id": asterism_domain::ServiceTokenId::new(),
                "owner_user_id": null,
                "name": "test",
                "scopes": ["system_read"],
                "created_at": "2026-08-09T00:00:00Z",
                "expires_at": null,
                "revoked_at": null,
                "last_used_at": null
            }
        }))
        .unwrap();
        let debug = format!("{response:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("ast_st_example"));
    }
}
