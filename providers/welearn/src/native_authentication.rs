use std::{
    collections::BTreeMap,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use asterism_networking::{ResolvedNetworkProfile, build_http_client};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use async_trait::async_trait;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{
        ACCEPT, CONTENT_TYPE, COOKIE, HeaderMap, LOCATION, ORIGIN, REFERER, RETRY_AFTER, SET_COOKIE,
    },
};
use zeroize::Zeroize;

use crate::{
    WellearnAuthenticationTransport, WellearnCookieSession, classify_password_login_response,
    encode_password_at,
    native_http::{classify_reqwest_error, fetch_course_inventory_document},
    parse_course_inventory,
};

const PRELOGIN_URL: &str = "https://welearn.sflep.com/user/prelogin.aspx?loginret=http%3a%2f%2fwelearn.sflep.com%2fuser%2floginredirect.aspx";
const LOGIN_URL: &str = "https://sso.sflep.com/idsvr/account/login";
const LOGIN_ORIGIN: &str = "https://sso.sflep.com";
const LOGIN_REFERER: &str = "https://sso.sflep.com/idsvr/login.html";
const TRANSFER_PATH: &str = "/idsvr/transfer.html";
const COURSE_LIST_URL: &str = "https://welearn.sflep.com/ajax/authCourse.aspx?action=gmc";
const MAX_REDIRECTS: usize = 12;
const MAX_LOGIN_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_RETURN_ROUTE_BYTES: usize = 8 * 1_024;
const MAX_COOKIE_ENTRIES: usize = 128;
const MAX_COOKIE_VALUE_BYTES: usize = 8 * 1_024;
const MAX_COOKIE_PATH_BYTES: usize = 2_048;

/// Native Password/OIDC and Cookie-validation transport for `WELearn`.
pub struct NativeWellearnAuthenticationTransport {
    client: Client,
}

impl NativeWellearnAuthenticationTransport {
    /// Builds the transport from the shared, non-redirecting network policy.
    ///
    /// # Errors
    ///
    /// Returns an internal Provider error if the shared HTTP client cannot be
    /// initialized.
    pub fn try_new(network: &ResolvedNetworkProfile) -> ProviderResult<Self> {
        let client = build_http_client(network).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn authentication HTTP client initialization failed",
            )
        })?;
        Ok(Self { client })
    }

    async fn exchange_password_inner(
        &self,
        username: &SecretString,
        password: &SecretString,
    ) -> ProviderResult<WellearnCookieSession> {
        let mut cookies = ScopedCookieJar::default();
        let transfer_url = self
            .follow_get_chain(static_url(PRELOGIN_URL)?, &mut cookies)
            .await?;
        let return_route = extract_return_route(&transfer_url)?;
        let timestamp = current_epoch_milliseconds()?;
        let cipher = encode_password_at(password.expose_secret(), timestamp)?;
        let timestamp = cipher.timestamp_milliseconds().to_string();
        let login_url = static_url(LOGIN_URL)?;
        let mut request = self
            .client
            .post(login_url.clone())
            .header(ACCEPT, "application/json")
            .header(ORIGIN, LOGIN_ORIGIN)
            .header(REFERER, LOGIN_REFERER)
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&[
                ("account", username.expose_secret()),
                ("pwd", cipher.encoded()),
                ("ts", timestamp.as_str()),
                ("rturl", return_route.expose_secret()),
            ]);
        if let Some(header) = cookies.header_for(&login_url)? {
            request = request.header(COOKIE, header.expose_secret());
        }
        let response = request
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
        cookies.capture(response.headers(), &login_url)?;
        let mut body = read_login_response(response).await?;
        let callback = classify_password_login_response(&body);
        body.zeroize();
        let callback = callback?;
        self.follow_get_chain(
            Url::parse(callback.expose_secret()).map_err(|_| invalid_login_response())?,
            &mut cookies,
        )
        .await?;

        let course_url = static_url(COURSE_LIST_URL)?;
        let header = cookies.header_for(&course_url)?.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Authentication,
                "WELearn login completed without an authenticated Cookie",
            )
        })?;
        WellearnCookieSession::try_new(header.expose_secret().to_owned())
    }

    async fn follow_get_chain(
        &self,
        start: Url,
        cookies: &mut ScopedCookieJar,
    ) -> ProviderResult<Url> {
        validate_auth_url(&start)?;
        let mut current = start;
        for redirect_count in 0..=MAX_REDIRECTS {
            let mut request = self.client.get(current.clone());
            if let Some(header) = cookies.header_for(&current)? {
                request = request.header(COOKIE, header.expose_secret());
            }
            let response = request
                .send()
                .await
                .map_err(|error| classify_reqwest_error(&error))?;
            cookies.capture(response.headers(), &current)?;
            if response.status().is_redirection() {
                if redirect_count == MAX_REDIRECTS {
                    return Err(ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "WELearn authentication exceeded the redirect limit",
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ProviderError::new(
                            ProviderErrorKind::ProtocolDrift,
                            "WELearn authentication redirect has no valid Location",
                        )
                    })?;
                let next = current.join(location).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "WELearn authentication redirect is invalid",
                    )
                })?;
                validate_auth_url(&next)?;
                current = next;
                continue;
            }
            validate_auth_status(response.status(), response.headers())?;
            return Ok(current);
        }
        unreachable!("bounded authentication redirect loop always returns")
    }
}

impl fmt::Debug for NativeWellearnAuthenticationTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeWellearnAuthenticationTransport")
            .field("client", &"configured")
            .finish()
    }
}

#[async_trait]
impl WellearnAuthenticationTransport for NativeWellearnAuthenticationTransport {
    async fn exchange_password(
        &self,
        username: &SecretString,
        password: &SecretString,
    ) -> ProviderResult<WellearnCookieSession> {
        self.exchange_password_inner(username, password).await
    }

    async fn validate_cookie(&self, session: &WellearnCookieSession) -> ProviderResult<()> {
        let document = fetch_course_inventory_document(&self.client, session).await?;
        parse_course_inventory(document.as_str())?;
        Ok(())
    }
}

struct SensitiveReturnRoute(SecretString);

impl SensitiveReturnRoute {
    fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for SensitiveReturnRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveReturnRoute([REDACTED])")
    }
}

fn extract_return_route(url: &Url) -> ProviderResult<SensitiveReturnRoute> {
    let mut values = url
        .query_pairs()
        .filter(|(key, _)| key.eq_ignore_ascii_case("returnUrl"))
        .map(|(_, value)| value.into_owned());
    let mut route = values.next().ok_or_else(invalid_login_response)?;
    if url.scheme() != "https"
        || url.host_str() != Some("sso.sflep.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != TRANSFER_PATH
        || url.fragment().is_some()
        || values.next().is_some()
        || route.is_empty()
        || route.len() > MAX_RETURN_ROUTE_BYTES
        || route.chars().any(char::is_control)
        || !route.starts_with("/connect/authorize/callback?")
        || route.starts_with("//")
    {
        route.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn SSO transfer returned an invalid return route",
        ));
    }
    let absolute = format!("https://sso.sflep.com/idsvr{route}");
    let parsed = Url::parse(&absolute).map_err(|_| invalid_login_response())?;
    validate_auth_url(&parsed)?;
    Ok(SensitiveReturnRoute(SecretString::new(route)))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CookieKey {
    domain: String,
    path: String,
    name: String,
}

#[derive(Default)]
struct ScopedCookieJar(BTreeMap<CookieKey, String>);

impl ScopedCookieJar {
    fn capture(&mut self, headers: &HeaderMap, request_url: &Url) -> ProviderResult<()> {
        let request_host = request_url
            .host_str()
            .ok_or_else(invalid_login_response)?
            .to_ascii_lowercase();
        for header in headers.get_all(SET_COOKIE) {
            let header = header.to_str().map_err(|_| invalid_login_response())?;
            let mut fields = header.split(';');
            let pair = fields.next().unwrap_or_default().trim();
            let (name, value) = pair.split_once('=').ok_or_else(invalid_login_response)?;
            if !crate::authentication::valid_cookie_name(name)
                || value.len() > MAX_COOKIE_VALUE_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(invalid_login_response());
            }
            let mut domain = request_host.clone();
            let mut path = default_cookie_path(request_url.path());
            let mut remove = value.is_empty();
            for attribute in fields {
                let attribute = attribute.trim();
                let (key, value) = attribute.split_once('=').unwrap_or((attribute, ""));
                if key.eq_ignore_ascii_case("domain") {
                    domain = value.trim().trim_start_matches('.').to_ascii_lowercase();
                } else if key.eq_ignore_ascii_case("path") {
                    value.trim().clone_into(&mut path);
                } else if key.eq_ignore_ascii_case("max-age") && value.trim() == "0" {
                    remove = true;
                }
            }
            if !allowed_cookie_domain(&domain)
                || !domain_matches(&request_host, &domain)
                || path.is_empty()
                || path.len() > MAX_COOKIE_PATH_BYTES
                || !path.starts_with('/')
                || path.chars().any(char::is_control)
            {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "WELearn authentication returned an untrusted Cookie scope",
                ));
            }
            let key = CookieKey {
                domain,
                path,
                name: name.to_owned(),
            };
            if remove {
                if let Some(mut previous) = self.0.remove(&key) {
                    previous.zeroize();
                }
                continue;
            }
            if !self.0.contains_key(&key) && self.0.len() == MAX_COOKIE_ENTRIES {
                return Err(invalid_login_response());
            }
            if let Some(mut previous) = self.0.insert(key, value.to_owned()) {
                previous.zeroize();
            }
        }
        Ok(())
    }

    fn header_for(&self, url: &Url) -> ProviderResult<Option<SecretString>> {
        let host = url.host_str().ok_or_else(invalid_login_response)?;
        let mut selected: BTreeMap<&str, (usize, usize, &str)> = BTreeMap::new();
        for (key, value) in &self.0 {
            if !domain_matches(host, &key.domain) || !cookie_path_matches(url.path(), &key.path) {
                continue;
            }
            let specificity = (key.path.len(), key.domain.len(), value.as_str());
            match selected.get(key.name.as_str()) {
                Some(previous) if (previous.0, previous.1) >= (specificity.0, specificity.1) => {}
                _ => {
                    selected.insert(key.name.as_str(), specificity);
                }
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        let mut header = String::new();
        for (name, (_, _, value)) in selected {
            if !header.is_empty() {
                header.push_str("; ");
            }
            header.push_str(name);
            header.push('=');
            header.push_str(value);
        }
        if header.len() > 64 * 1_024 {
            header.zeroize();
            return Err(invalid_login_response());
        }
        Ok(Some(SecretString::new(header)))
    }
}

impl fmt::Debug for ScopedCookieJar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedCookieJar")
            .field("entry_count", &self.0.len())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ScopedCookieJar {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

async fn read_login_response(mut response: Response) -> ProviderResult<Vec<u8>> {
    validate_auth_status(response.status(), response.headers())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_LOGIN_RESPONSE_BYTES as u64)
    {
        return Err(invalid_login_response());
    }
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let content_type = content_type
            .to_str()
            .map_err(|_| invalid_login_response())?;
        let media_type = content_type.split(';').next().unwrap_or_default().trim();
        if !media_type.eq_ignore_ascii_case("application/json")
            && !media_type.eq_ignore_ascii_case("text/json")
        {
            return Err(invalid_login_response());
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_LOGIN_RESPONSE_BYTES {
            bytes.zeroize();
            return Err(invalid_login_response());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(invalid_login_response());
    }
    Ok(bytes)
}

fn validate_auth_status(status: StatusCode, headers: &HeaderMap) -> ProviderResult<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let mut error = ProviderError::new(
            ProviderErrorKind::RateLimited,
            "WELearn rate limited the authentication request",
        );
        error.retry_after_seconds = headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds <= 3_600);
        return Err(error);
    }
    if status.is_server_error() {
        return Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "WELearn authentication endpoint is temporarily unavailable",
        ));
    }
    if !status.is_success() {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "WELearn authentication endpoint returned an unexpected status",
        ));
    }
    Ok(())
}

fn validate_auth_url(url: &Url) -> ProviderResult<()> {
    let allowed_host = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("welearn.sflep.com") || host.eq_ignore_ascii_case("sso.sflep.com")
    });
    if url.scheme() != "https"
        || !allowed_host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "WELearn authentication attempted an untrusted redirect",
        ));
    }
    Ok(())
}

fn allowed_cookie_domain(domain: &str) -> bool {
    matches!(domain, "sflep.com" | "sso.sflep.com" | "welearn.sflep.com")
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host.eq_ignore_ascii_case(domain)
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn cookie_path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || request_path
            .strip_prefix(cookie_path)
            .is_some_and(|suffix| cookie_path.ends_with('/') || suffix.starts_with('/'))
}

fn default_cookie_path(request_path: &str) -> String {
    let Some((directory, _)) = request_path.rsplit_once('/') else {
        return "/".to_owned();
    };
    if directory.is_empty() {
        "/".to_owned()
    } else {
        format!("{directory}/")
    }
}

fn current_epoch_milliseconds() -> ProviderResult<u64> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn login clock is before the Unix epoch",
            )
        })?
        .as_millis();
    u64::try_from(milliseconds).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn login timestamp exceeds the supported range",
        )
    })
}

fn static_url(value: &'static str) -> ProviderResult<Url> {
    Url::parse(value).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn compile-time authentication route is invalid",
        )
    })
}

fn invalid_login_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "WELearn authentication endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;

    use super::*;

    #[test]
    fn native_transport_uses_the_shared_client() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let transport = NativeWellearnAuthenticationTransport::try_new(&network).unwrap();
        assert!(format!("{transport:?}").contains("configured"));
    }

    #[test]
    fn transfer_route_is_unique_bounded_and_redacted() {
        let url = Url::parse(
            "https://sso.sflep.com/idsvr/transfer.html?returnUrl=%2Fconnect%2Fauthorize%2Fcallback%3Fclient_id%3Dwelearn_web%26state%3DSAFE_STATE",
        )
        .unwrap();
        let route = extract_return_route(&url).unwrap();
        assert!(
            route
                .expose_secret()
                .starts_with("/connect/authorize/callback?")
        );
        assert!(!format!("{route:?}").contains("SAFE_STATE"));

        let duplicate = Url::parse(
            "https://sso.sflep.com/idsvr/transfer.html?returnUrl=%2Fconnect%2Fauthorize%2Fcallback%3Fa%3D1&returnUrl=%2Fconnect%2Fauthorize%2Fcallback%3Fa%3D2",
        )
        .unwrap();
        assert!(extract_return_route(&duplicate).is_err());
        for invalid in [
            "https://sso.sflep.com/idsvr/login.html?returnUrl=%2Fconnect%2Fauthorize%2Fcallback%3Fa%3D1",
            "https://welearn.sflep.com/idsvr/transfer.html?returnUrl=%2Fconnect%2Fauthorize%2Fcallback%3Fa%3D1",
            "https://sso.sflep.com/idsvr/transfer.html/extra?returnUrl=%2Fconnect%2Fauthorize%2Fcallback%3Fa%3D1",
        ] {
            assert!(extract_return_route(&Url::parse(invalid).unwrap()).is_err());
        }
    }

    #[test]
    fn cookie_jar_enforces_scope_path_and_redaction() {
        let request = Url::parse("https://sso.sflep.com/idsvr/login").unwrap();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            "sso=SAFE_SSO; Domain=.sflep.com; Path=/; Secure"
                .parse()
                .unwrap(),
        );
        headers.append(
            SET_COOKIE,
            "flow=SAFE_FLOW; Path=/idsvr/; HttpOnly".parse().unwrap(),
        );
        let mut jar = ScopedCookieJar::default();
        jar.capture(&headers, &request).unwrap();
        let sso = jar.header_for(&request).unwrap().unwrap();
        assert!(sso.expose_secret().contains("sso=SAFE_SSO"));
        assert!(sso.expose_secret().contains("flow=SAFE_FLOW"));

        let course = Url::parse(COURSE_LIST_URL).unwrap();
        let course_header = jar.header_for(&course).unwrap().unwrap();
        assert_eq!(course_header.expose_secret(), "sso=SAFE_SSO");
        assert!(!format!("{jar:?}").contains("SAFE"));
    }

    #[test]
    fn cookie_jar_rejects_foreign_domain_and_honors_deletion() {
        let request = Url::parse("https://sso.sflep.com/idsvr/login").unwrap();
        let mut foreign = HeaderMap::new();
        foreign.insert(
            SET_COOKIE,
            "session=value; Domain=evil.example; Path=/"
                .parse()
                .unwrap(),
        );
        assert!(
            ScopedCookieJar::default()
                .capture(&foreign, &request)
                .is_err()
        );

        let mut jar = ScopedCookieJar::default();
        let mut set = HeaderMap::new();
        set.insert(SET_COOKIE, "session=value; Path=/".parse().unwrap());
        jar.capture(&set, &request).unwrap();
        let mut remove = HeaderMap::new();
        remove.insert(
            SET_COOKIE,
            "session=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
                .parse()
                .unwrap(),
        );
        jar.capture(&remove, &request).unwrap();
        assert!(jar.header_for(&request).unwrap().is_none());
    }

    #[test]
    fn redirects_accept_only_the_two_https_origins() {
        assert!(validate_auth_url(&Url::parse(PRELOGIN_URL).unwrap()).is_ok());
        assert!(validate_auth_url(&Url::parse(LOGIN_URL).unwrap()).is_ok());
        assert!(validate_auth_url(&Url::parse("https://evil.example/callback").unwrap()).is_err());
        assert!(
            validate_auth_url(&Url::parse("https://user@sso.sflep.com/callback").unwrap()).is_err()
        );
    }

    #[test]
    fn cookie_paths_match_only_at_path_boundaries() {
        assert!(cookie_path_matches("/idsvr/login", "/idsvr"));
        assert!(cookie_path_matches("/idsvr/login", "/idsvr/"));
        assert!(cookie_path_matches("/idsvr", "/idsvr"));
        assert!(!cookie_path_matches("/idsvr-other", "/idsvr"));
    }
}
