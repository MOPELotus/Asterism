//! Validated network policy shared by Core and every Provider transport.

use std::{fmt, net::IpAddr, time::Duration};

use reqwest::{Client, Proxy, redirect::Policy};
use serde::{Deserialize, Serialize};

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
const MAX_TIMEOUT_SECONDS: u64 = 300;
const MAX_USER_AGENT_BYTES: usize = 512;
const MAX_PROXY_URL_BYTES: usize = 2_048;
const MAX_REQUESTS_PER_MINUTE: u32 = 60_000;
const MAX_BURST: u32 = 1_000;
const MAX_RETRY_ATTEMPTS: u8 = 10;
const MAX_RETRY_DELAY_MILLISECONDS: u64 = 300_000;

/// One partial network policy. Resolution applies account values over Provider
/// values, then global values, then conservative built-in defaults.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkProfile {
    pub proxy: Option<ProxySetting>,
    pub timeout_seconds: Option<u64>,
    pub address_family_preference: Option<AddressFamilyPreference>,
    pub tls_profile: Option<TlsProfile>,
    pub user_agent: Option<String>,
    pub rate_limit: Option<RateLimit>,
    pub retry_backoff: Option<RetryBackoff>,
}

/// Proxy behavior for one resolved HTTP client.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "url")]
pub enum ProxySetting {
    System,
    Disabled,
    Url(String),
}

impl fmt::Debug for ProxySetting {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => formatter.write_str("System"),
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Url(_) => formatter.write_str("Url([REDACTED])"),
        }
    }
}

/// Address-family selection supported by the current reqwest adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressFamilyPreference {
    System,
    Ipv4Only,
    Ipv6Only,
}

/// Versioned TLS behavior. New profiles require explicit implementation and
/// verification rather than Provider-local TLS switches.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsProfile {
    RustlsWebPki,
}

/// Provider request admission settings. Enforcement is owned by the runtime,
/// not by the Provider parser.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RateLimit {
    pub requests_per_minute: u32,
    pub burst: u32,
}

/// Retry policy for operations which the caller has independently classified
/// as safe to retry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryBackoff {
    pub max_attempts: u8,
    pub base_delay_milliseconds: u64,
    pub max_delay_milliseconds: u64,
}

/// Fully resolved and validated network policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedNetworkProfile {
    proxy: ProxySetting,
    timeout_seconds: u64,
    address_family_preference: AddressFamilyPreference,
    tls_profile: TlsProfile,
    user_agent: String,
    rate_limit: RateLimit,
    retry_backoff: RetryBackoff,
}

impl ResolvedNetworkProfile {
    /// Resolves one profile using `account > Provider > global > built-in`
    /// precedence and validates the final result.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkError::InvalidProfile`] when any resolved field is
    /// empty, malformed, unsupported, or exceeds its safety limit.
    pub fn resolve(
        global: &NetworkProfile,
        provider: Option<&NetworkProfile>,
        account: Option<&NetworkProfile>,
    ) -> Result<Self, NetworkError> {
        let defaults = Self::built_in();
        let resolved = Self {
            proxy: resolve_field(
                account.and_then(|profile| profile.proxy.clone()),
                provider.and_then(|profile| profile.proxy.clone()),
                global.proxy.clone(),
                defaults.proxy,
            ),
            timeout_seconds: resolve_field(
                account.and_then(|profile| profile.timeout_seconds),
                provider.and_then(|profile| profile.timeout_seconds),
                global.timeout_seconds,
                defaults.timeout_seconds,
            ),
            address_family_preference: resolve_field(
                account.and_then(|profile| profile.address_family_preference),
                provider.and_then(|profile| profile.address_family_preference),
                global.address_family_preference,
                defaults.address_family_preference,
            ),
            tls_profile: resolve_field(
                account.and_then(|profile| profile.tls_profile),
                provider.and_then(|profile| profile.tls_profile),
                global.tls_profile,
                defaults.tls_profile,
            ),
            user_agent: resolve_field(
                account.and_then(|profile| profile.user_agent.clone()),
                provider.and_then(|profile| profile.user_agent.clone()),
                global.user_agent.clone(),
                defaults.user_agent,
            ),
            rate_limit: resolve_field(
                account.and_then(|profile| profile.rate_limit),
                provider.and_then(|profile| profile.rate_limit),
                global.rate_limit,
                defaults.rate_limit,
            ),
            retry_backoff: resolve_field(
                account.and_then(|profile| profile.retry_backoff),
                provider.and_then(|profile| profile.retry_backoff),
                global.retry_backoff,
                defaults.retry_backoff,
            ),
        };
        resolved.validate()?;
        Ok(resolved)
    }

    pub const fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }

    pub const fn address_family_preference(&self) -> AddressFamilyPreference {
        self.address_family_preference
    }

    pub const fn tls_profile(&self) -> TlsProfile {
        self.tls_profile
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub const fn rate_limit(&self) -> RateLimit {
        self.rate_limit
    }

    pub const fn retry_backoff(&self) -> RetryBackoff {
        self.retry_backoff
    }

    fn built_in() -> Self {
        Self {
            proxy: ProxySetting::System,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
            address_family_preference: AddressFamilyPreference::System,
            tls_profile: TlsProfile::RustlsWebPki,
            user_agent: format!("Asterism/{}", env!("CARGO_PKG_VERSION")),
            rate_limit: RateLimit {
                requests_per_minute: 60,
                burst: 4,
            },
            retry_backoff: RetryBackoff {
                max_attempts: 3,
                base_delay_milliseconds: 500,
                max_delay_milliseconds: 5_000,
            },
        }
    }

    fn validate(&self) -> Result<(), NetworkError> {
        if self.timeout_seconds == 0 || self.timeout_seconds > MAX_TIMEOUT_SECONDS {
            return Err(NetworkError::InvalidProfile("invalid request timeout"));
        }
        if self.user_agent.is_empty()
            || self.user_agent.len() > MAX_USER_AGENT_BYTES
            || self.user_agent.chars().any(char::is_control)
        {
            return Err(NetworkError::InvalidProfile("invalid user agent"));
        }
        validate_proxy(&self.proxy)?;
        if self.rate_limit.requests_per_minute == 0
            || self.rate_limit.requests_per_minute > MAX_REQUESTS_PER_MINUTE
            || self.rate_limit.burst == 0
            || self.rate_limit.burst > MAX_BURST
        {
            return Err(NetworkError::InvalidProfile("invalid rate limit"));
        }
        if self.retry_backoff.max_attempts == 0
            || self.retry_backoff.max_attempts > MAX_RETRY_ATTEMPTS
            || self.retry_backoff.base_delay_milliseconds == 0
            || self.retry_backoff.max_delay_milliseconds
                < self.retry_backoff.base_delay_milliseconds
            || self.retry_backoff.max_delay_milliseconds > MAX_RETRY_DELAY_MILLISECONDS
        {
            return Err(NetworkError::InvalidProfile("invalid retry backoff"));
        }
        Ok(())
    }
}

/// Builds an HTTP client from one validated profile. Redirects are disabled so
/// each Provider must classify cross-origin and authentication redirects.
///
/// # Errors
///
/// Returns [`NetworkError::ClientBuild`] when reqwest rejects the resolved
/// proxy, TLS, user-agent, or socket configuration.
pub fn build_http_client(profile: &ResolvedNetworkProfile) -> Result<Client, NetworkError> {
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .https_only(true)
        .timeout(Duration::from_secs(profile.timeout_seconds))
        .connect_timeout(Duration::from_secs(profile.timeout_seconds.min(30)))
        .user_agent(&profile.user_agent);

    builder = match profile.address_family_preference {
        AddressFamilyPreference::System => builder,
        AddressFamilyPreference::Ipv4Only => builder.local_address(IpAddr::from([0, 0, 0, 0])),
        AddressFamilyPreference::Ipv6Only => builder.local_address(IpAddr::from([0_u16; 8])),
    };
    builder = match profile.tls_profile {
        TlsProfile::RustlsWebPki => builder.use_rustls_tls(),
    };
    builder = match &profile.proxy {
        ProxySetting::System => builder,
        ProxySetting::Disabled => builder.no_proxy(),
        ProxySetting::Url(url) => builder.proxy(
            Proxy::all(url)
                .map_err(|_| NetworkError::InvalidProfile("invalid proxy configuration"))?,
        ),
    };
    builder.build().map_err(|_| NetworkError::ClientBuild)
}

fn resolve_field<T>(account: Option<T>, provider: Option<T>, global: Option<T>, default: T) -> T {
    account.or(provider).or(global).unwrap_or(default)
}

fn validate_proxy(proxy: &ProxySetting) -> Result<(), NetworkError> {
    let ProxySetting::Url(value) = proxy else {
        return Ok(());
    };
    if value.is_empty() || value.len() > MAX_PROXY_URL_BYTES {
        return Err(NetworkError::InvalidProfile("invalid proxy URL"));
    }
    let url = reqwest::Url::parse(value)
        .map_err(|_| NetworkError::InvalidProfile("invalid proxy URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(NetworkError::InvalidProfile(
            "proxy URL contains unsupported or sensitive components",
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("network profile is invalid: {0}")]
    InvalidProfile(&'static str),
    #[error("HTTP client initialization failed")]
    ClientBuild,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_overrides_provider_and_global_field_by_field() {
        let global = NetworkProfile {
            timeout_seconds: Some(10),
            user_agent: Some("global-agent".to_owned()),
            ..NetworkProfile::default()
        };
        let provider = NetworkProfile {
            timeout_seconds: Some(20),
            rate_limit: Some(RateLimit {
                requests_per_minute: 30,
                burst: 2,
            }),
            ..NetworkProfile::default()
        };
        let account = NetworkProfile {
            user_agent: Some("account-agent".to_owned()),
            ..NetworkProfile::default()
        };

        let resolved =
            ResolvedNetworkProfile::resolve(&global, Some(&provider), Some(&account)).unwrap();
        assert_eq!(resolved.timeout_seconds(), 20);
        assert_eq!(resolved.user_agent(), "account-agent");
        assert_eq!(resolved.rate_limit().requests_per_minute, 30);
        assert_eq!(
            resolved.address_family_preference(),
            AddressFamilyPreference::System
        );
    }

    #[test]
    fn invalid_or_credential_bearing_proxy_is_rejected_and_redacted() {
        let proxy = ProxySetting::Url("https://user:password@proxy.invalid".to_owned());
        assert!(!format!("{proxy:?}").contains("password"));
        let profile = NetworkProfile {
            proxy: Some(proxy),
            ..NetworkProfile::default()
        };
        assert!(ResolvedNetworkProfile::resolve(&profile, None, None).is_err());
    }

    #[test]
    fn unsafe_limits_are_rejected() {
        let profile = NetworkProfile {
            timeout_seconds: Some(0),
            ..NetworkProfile::default()
        };
        assert!(ResolvedNetworkProfile::resolve(&profile, None, None).is_err());

        let profile = NetworkProfile {
            retry_backoff: Some(RetryBackoff {
                max_attempts: 2,
                base_delay_milliseconds: 2_000,
                max_delay_milliseconds: 1_000,
            }),
            ..NetworkProfile::default()
        };
        assert!(ResolvedNetworkProfile::resolve(&profile, None, None).is_err());
    }

    #[test]
    fn validated_profiles_build_non_redirecting_https_clients() {
        for address_family_preference in [
            AddressFamilyPreference::System,
            AddressFamilyPreference::Ipv4Only,
            AddressFamilyPreference::Ipv6Only,
        ] {
            let profile = NetworkProfile {
                proxy: Some(ProxySetting::Disabled),
                address_family_preference: Some(address_family_preference),
                ..NetworkProfile::default()
            };
            let resolved = ResolvedNetworkProfile::resolve(&profile, None, None).unwrap();
            build_http_client(&resolved).unwrap();
        }
    }
}
