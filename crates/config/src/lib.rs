//! Typed Asterism configuration with deterministic source precedence.

use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_FILE: &str = "asterism.toml";
pub const CONFIG_FILE_ENV: &str = "ASTERISM_CONFIG";
pub const BIND_ENV: &str = "ASTERISM_BIND";
pub const DATABASE_URL_ENV: &str = "ASTERISM_DATABASE_URL";
pub const SESSION_TTL_SECONDS_ENV: &str = "ASTERISM_SESSION_TTL_SECONDS";
pub const SECURE_COOKIES_ENV: &str = "ASTERISM_SECURE_COOKIES";
pub const ENABLE_DEVELOPMENT_CHAOXING_ENV: &str = "ASTERISM_ENABLE_DEVELOPMENT_CHAOXING";
pub const ENABLE_DEVELOPMENT_WELEARN_ENV: &str = "ASTERISM_ENABLE_DEVELOPMENT_WELEARN";
pub const ENABLE_DEVELOPMENT_UAI_ENV: &str = "ASTERISM_ENABLE_DEVELOPMENT_UAI";
pub const ENABLE_DEVELOPMENT_CIDAREN_ENV: &str = "ASTERISM_ENABLE_DEVELOPMENT_CIDAREN";
pub const SCHEDULER_ENABLED_ENV: &str = "ASTERISM_SCHEDULER_ENABLED";
pub const SCHEDULER_TICK_INTERVAL_SECONDS_ENV: &str = "ASTERISM_SCHEDULER_TICK_INTERVAL_SECONDS";
pub const SCHEDULER_MATERIALIZE_LIMIT_ENV: &str = "ASTERISM_SCHEDULER_MATERIALIZE_LIMIT";
pub const SCHEDULER_CLAIM_LIMIT_ENV: &str = "ASTERISM_SCHEDULER_CLAIM_LIMIT";
pub const SCHEDULER_EXECUTION_CONCURRENCY_LIMIT_ENV: &str =
    "ASTERISM_SCHEDULER_EXECUTION_CONCURRENCY_LIMIT";
pub const SCHEDULER_CLAIM_TTL_SECONDS_ENV: &str = "ASTERISM_SCHEDULER_CLAIM_TTL_SECONDS";
pub const SCHEDULER_RETRY_MAX_ATTEMPTS_ENV: &str = "ASTERISM_SCHEDULER_RETRY_MAX_ATTEMPTS";
pub const SCHEDULER_RETRY_INITIAL_DELAY_SECONDS_ENV: &str =
    "ASTERISM_SCHEDULER_RETRY_INITIAL_DELAY_SECONDS";
pub const SCHEDULER_RETRY_MULTIPLIER_ENV: &str = "ASTERISM_SCHEDULER_RETRY_MULTIPLIER";
pub const SCHEDULER_RETRY_MAX_DELAY_SECONDS_ENV: &str =
    "ASTERISM_SCHEDULER_RETRY_MAX_DELAY_SECONDS";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub scheduler: SchedulerConfig,
    pub providers: ProviderConfig,
    pub ai: AiConfig,
}

impl Config {
    /// Loads and validates configuration in ascending precedence order:
    /// defaults, TOML file, environment variables, then CLI overrides.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required file cannot be read, TOML is
    /// invalid, an environment value cannot be parsed, or the merged result is
    /// unsafe or unsupported.
    pub fn load(
        file: &ConfigFile,
        environment: &Environment,
        cli: &ConfigOverrides,
    ) -> Result<Self, ConfigError> {
        let mut config = load_file(file)?;
        config.apply(&environment.overrides);
        config.apply(cli);
        config.validate()?;
        Ok(config)
    }

    /// Validates invariants required by the current Phase 0 runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Validation`] for invalid or unsafe values.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.server.bind.ip().is_loopback() {
            return Err(ConfigError::Validation(
                "server.bind must be loopback during Phase 0".to_owned(),
            ));
        }
        if self.server.session_ttl_seconds == 0 {
            return Err(ConfigError::Validation(
                "server.session_ttl_seconds must be greater than zero".to_owned(),
            ));
        }
        if self.server.session_ttl_seconds > i64::MAX.cast_unsigned() {
            return Err(ConfigError::Validation(
                "server.session_ttl_seconds is too large".to_owned(),
            ));
        }
        if self.database.url.trim() != self.database.url
            || self.database.url.is_empty()
            || self.database.url.chars().any(char::is_control)
        {
            return Err(ConfigError::Validation(
                "database.url must be a non-empty single-line value without surrounding whitespace"
                    .to_owned(),
            ));
        }
        if !self.database.url.starts_with("sqlite:") {
            return Err(ConfigError::Validation(
                "database.url must use the sqlite scheme".to_owned(),
            ));
        }
        self.scheduler.validate()?;
        self.ai.validate()?;
        Ok(())
    }

    fn apply(&mut self, overrides: &ConfigOverrides) {
        if let Some(bind) = overrides.server.bind {
            self.server.bind = bind;
        }
        if let Some(session_ttl_seconds) = overrides.server.session_ttl_seconds {
            self.server.session_ttl_seconds = session_ttl_seconds;
        }
        if let Some(secure_cookies) = overrides.server.secure_cookies {
            self.server.secure_cookies = secure_cookies;
        }
        if let Some(url) = &overrides.database.url {
            self.database.url.clone_from(url);
        }
        self.scheduler.apply(&overrides.scheduler);
        self.providers.apply(&overrides.providers);
    }
}

/// Deployment-wide model endpoints and the two built-in answer combinations.
/// API keys are intentionally read from the named environment variables by
/// the eventual client and never serialized into this configuration object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    pub remote_store: bool,
    /// Deployment default used by scheduler-created AI work. Individual
    /// executions may still freeze an explicit profile override.
    #[serde(default = "default_ai_profile")]
    pub default_profile: String,
    #[serde(default = "default_gpt_router_endpoint")]
    pub gpt_router: AiEndpointConfig,
    #[serde(default = "default_deepseek_endpoint")]
    pub deepseek: AiEndpointConfig,
    #[serde(default = "default_kimi_endpoint")]
    pub kimi: AiEndpointConfig,
    #[serde(default = "AiProfileConfig::economy")]
    pub economy: AiProfileConfig,
    #[serde(default = "AiProfileConfig::gpt_only")]
    pub gpt_only: AiProfileConfig,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            remote_store: false,
            default_profile: default_ai_profile(),
            gpt_router: default_gpt_router_endpoint(),
            deepseek: default_deepseek_endpoint(),
            kimi: default_kimi_endpoint(),
            economy: AiProfileConfig::economy(),
            gpt_only: AiProfileConfig::gpt_only(),
        }
    }
}

fn default_ai_profile() -> String {
    "economy".to_owned()
}

fn default_gpt_router_endpoint() -> AiEndpointConfig {
    AiEndpointConfig {
        base_url: String::new(),
        api_key_env: "ASTERISM_GPT_ROUTER_API_KEY".to_owned(),
        protocol: AiProtocol::Responses,
    }
}

fn default_deepseek_endpoint() -> AiEndpointConfig {
    AiEndpointConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        api_key_env: "ASTERISM_DEEPSEEK_API_KEY".to_owned(),
        protocol: AiProtocol::ChatCompletions,
    }
}

fn default_kimi_endpoint() -> AiEndpointConfig {
    AiEndpointConfig {
        base_url: "https://api.moonshot.cn/v1".to_owned(),
        api_key_env: "ASTERISM_KIMI_API_KEY".to_owned(),
        protocol: AiProtocol::ChatCompletions,
    }
}

impl AiConfig {
    /// Validates an AI deployment configuration received from the admin API.
    ///
    /// API keys are never part of this value; only endpoint names, model
    /// routing and environment-variable names are accepted.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !matches!(self.default_profile.as_str(), "economy" | "gpt_only") {
            return Err(ConfigError::Validation(
                "ai.default_profile must be economy or gpt_only".to_owned(),
            ));
        }
        for (name, endpoint) in [
            ("gpt_router", &self.gpt_router),
            ("deepseek", &self.deepseek),
            ("kimi", &self.kimi),
        ] {
            endpoint.validate(name)?;
        }
        self.economy.validate("economy")?;
        self.gpt_only.validate("gpt_only")?;
        if self.remote_store {
            return Err(ConfigError::Validation(
                "ai.remote_store must remain false; Asterism caches answers locally without asking remote model services to retain question content".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiEndpointConfig {
    pub base_url: String,
    pub api_key_env: String,
    pub protocol: AiProtocol,
}

impl Default for AiEndpointConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key_env: String::new(),
            protocol: AiProtocol::Responses,
        }
    }
}

impl AiEndpointConfig {
    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        let endpoint_valid = self.base_url.is_empty()
            || ((self.base_url.starts_with("https://")
                || self.base_url.starts_with("http://127.0.0.1:")
                || self.base_url.starts_with("http://localhost:"))
                && self.base_url.trim() == self.base_url
                && !self.base_url.chars().any(char::is_control));
        let env_valid = !self.api_key_env.is_empty()
            && self.api_key_env.len() <= 128
            && self
                .api_key_env
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if endpoint_valid && env_valid {
            Ok(())
        } else {
            Err(ConfigError::Validation(format!(
                "ai.{name} endpoint or api_key_env is invalid"
            )))
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProtocol {
    #[default]
    Responses,
    ChatCompletions,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiProfileConfig {
    pub trust_verified_cache: bool,
    pub allow_domestic_fallback: bool,
    pub timed: AiModelRoute,
    pub untimed: AiModelRoute,
    pub escalation: AiModelRoute,
    pub objective_fallback: Option<AiModelRoute>,
    pub rich_content_fallback: Option<AiModelRoute>,
}

impl AiProfileConfig {
    fn economy() -> Self {
        Self {
            trust_verified_cache: true,
            allow_domestic_fallback: true,
            timed: AiModelRoute::new("gpt_router", "gpt-5.6-luna", AiReasoningEffort::Low, 8),
            untimed: AiModelRoute::new(
                "gpt_router",
                "gpt-5.6-terra",
                AiReasoningEffort::Medium,
                120,
            ),
            escalation: AiModelRoute::new(
                "gpt_router",
                "gpt-5.6-sol",
                AiReasoningEffort::Xhigh,
                300,
            ),
            objective_fallback: Some(AiModelRoute::new(
                "deepseek",
                "deepseek-v4-flash",
                AiReasoningEffort::Medium,
                90,
            )),
            rich_content_fallback: Some(AiModelRoute::new(
                "kimi",
                "kimi-k3",
                AiReasoningEffort::High,
                180,
            )),
        }
    }

    fn gpt_only() -> Self {
        Self {
            trust_verified_cache: false,
            allow_domestic_fallback: false,
            timed: AiModelRoute::new("gpt_router", "gpt-5.6-luna", AiReasoningEffort::Low, 8),
            untimed: AiModelRoute::new("gpt_router", "gpt-5.6-sol", AiReasoningEffort::Xhigh, 300),
            escalation: AiModelRoute::new(
                "gpt_router",
                "gpt-5.6-sol",
                AiReasoningEffort::Xhigh,
                300,
            ),
            objective_fallback: None,
            rich_content_fallback: None,
        }
    }

    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        self.timed.validate(name)?;
        self.untimed.validate(name)?;
        self.escalation.validate(name)?;
        if let Some(route) = &self.objective_fallback {
            route.validate(name)?;
        }
        if let Some(route) = &self.rich_content_fallback {
            route.validate(name)?;
        }
        if name == "gpt_only"
            && (self.trust_verified_cache
                || self.allow_domestic_fallback
                || self.objective_fallback.is_some()
                || self.rich_content_fallback.is_some())
        {
            return Err(ConfigError::Validation(
                "ai.gpt_only must recheck cache and cannot configure domestic fallbacks".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for AiProfileConfig {
    fn default() -> Self {
        Self::economy()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiModelRoute {
    pub endpoint: String,
    pub model: String,
    pub reasoning_effort: AiReasoningEffort,
    pub timeout_seconds: u64,
}

impl AiModelRoute {
    fn new(
        endpoint: &str,
        model: &str,
        reasoning_effort: AiReasoningEffort,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            reasoning_effort,
            timeout_seconds,
        }
    }

    fn validate(&self, profile: &str) -> Result<(), ConfigError> {
        let valid_text = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value.trim() == value
                && !value.chars().any(char::is_control)
        };
        if valid_text(&self.endpoint)
            && valid_text(&self.model)
            && (1..=600).contains(&self.timeout_seconds)
        {
            Ok(())
        } else {
            Err(ConfigError::Validation(format!(
                "ai.{profile} contains an invalid model route"
            )))
        }
    }
}

impl Default for AiModelRoute {
    fn default() -> Self {
        Self::new(
            "gpt_router",
            "gpt-5.6-terra",
            AiReasoningEffort::Medium,
            120,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub session_ttl_seconds: u64,
    pub secure_cookies: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8068),
            session_ttl_seconds: 43_200,
            secure_cookies: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    pub enabled: bool,
    pub tick_interval_seconds: u64,
    pub materialize_limit: u32,
    pub claim_limit: u32,
    pub execution_concurrency_limit: u32,
    pub claim_ttl_seconds: u64,
    pub retry_max_attempts: u32,
    pub retry_initial_delay_seconds: u64,
    pub retry_multiplier: u32,
    pub retry_max_delay_seconds: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
// These are independent opt-in Provider registrations in the public TOML/env
// schema, not mutually exclusive state flags.
#[allow(clippy::struct_excessive_bools)]
pub struct ProviderConfig {
    /// Explicitly exposes the unverified Chaoxing Provider for local
    /// development and real-account validation. This is false by default.
    pub enable_development_chaoxing: bool,
    /// Explicitly exposes the unverified `WELearn` Provider for local
    /// development and real-account validation. This is false by default.
    pub enable_development_welearn: bool,
    /// Explicitly exposes the unverified UAI Provider for local development
    /// and real-account validation. This is false by default.
    pub enable_development_uai: bool,
    /// Explicitly exposes the unverified Cidaren Provider for local development
    /// and real-account validation. This is false by default.
    pub enable_development_cidaren: bool,
}

impl ProviderConfig {
    fn apply(&mut self, overrides: &ProviderOverrides) {
        if let Some(value) = overrides.enable_development_chaoxing {
            self.enable_development_chaoxing = value;
        }
        if let Some(value) = overrides.enable_development_welearn {
            self.enable_development_welearn = value;
        }
        if let Some(value) = overrides.enable_development_uai {
            self.enable_development_uai = value;
        }
        if let Some(value) = overrides.enable_development_cidaren {
            self.enable_development_cidaren = value;
        }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_interval_seconds: 5,
            materialize_limit: 100,
            claim_limit: 1,
            execution_concurrency_limit: 32,
            claim_ttl_seconds: 300,
            retry_max_attempts: 5,
            retry_initial_delay_seconds: 30,
            retry_multiplier: 2,
            retry_max_delay_seconds: 1_800,
        }
    }
}

impl SchedulerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        let valid = self.tick_interval_seconds > 0
            && self.tick_interval_seconds <= 3_600
            && (1..=1_000).contains(&self.materialize_limit)
            && (1..=1_000).contains(&self.claim_limit)
            && (1..=1_000).contains(&self.execution_concurrency_limit)
            && self.claim_ttl_seconds >= self.tick_interval_seconds
            && self.claim_ttl_seconds <= 86_400
            && (1..=100).contains(&self.retry_max_attempts)
            && self.retry_initial_delay_seconds > 0
            && self.retry_multiplier > 0
            && self.retry_max_delay_seconds >= self.retry_initial_delay_seconds
            && self.retry_max_delay_seconds <= 86_400;
        if valid {
            Ok(())
        } else {
            Err(ConfigError::Validation(
                "scheduler intervals, batches, lease, or retry policy are outside safe bounds"
                    .to_owned(),
            ))
        }
    }

    fn apply(&mut self, overrides: &SchedulerOverrides) {
        if let Some(value) = overrides.enabled {
            self.enabled = value;
        }
        if let Some(value) = overrides.tick_interval_seconds {
            self.tick_interval_seconds = value;
        }
        if let Some(value) = overrides.materialize_limit {
            self.materialize_limit = value;
        }
        if let Some(value) = overrides.claim_limit {
            self.claim_limit = value;
        }
        if let Some(value) = overrides.execution_concurrency_limit {
            self.execution_concurrency_limit = value;
        }
        if let Some(value) = overrides.claim_ttl_seconds {
            self.claim_ttl_seconds = value;
        }
        if let Some(value) = overrides.retry_max_attempts {
            self.retry_max_attempts = value;
        }
        if let Some(value) = overrides.retry_initial_delay_seconds {
            self.retry_initial_delay_seconds = value;
        }
        if let Some(value) = overrides.retry_multiplier {
            self.retry_multiplier = value;
        }
        if let Some(value) = overrides.retry_max_delay_seconds {
            self.retry_max_delay_seconds = value;
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://asterism.db".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigFile {
    Required(PathBuf),
    Optional(PathBuf),
}

impl ConfigFile {
    pub fn required(path: impl Into<PathBuf>) -> Self {
        Self::Required(path.into())
    }

    pub fn optional(path: impl Into<PathBuf>) -> Self {
        Self::Optional(path.into())
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Required(path) | Self::Optional(path) => path,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigOverrides {
    pub server: ServerOverrides,
    pub database: DatabaseOverrides,
    pub scheduler: SchedulerOverrides,
    pub providers: ProviderOverrides,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServerOverrides {
    pub bind: Option<SocketAddr>,
    pub session_ttl_seconds: Option<u64>,
    pub secure_cookies: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatabaseOverrides {
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchedulerOverrides {
    pub enabled: Option<bool>,
    pub tick_interval_seconds: Option<u64>,
    pub materialize_limit: Option<u32>,
    pub claim_limit: Option<u32>,
    pub execution_concurrency_limit: Option<u32>,
    pub claim_ttl_seconds: Option<u64>,
    pub retry_max_attempts: Option<u32>,
    pub retry_initial_delay_seconds: Option<u64>,
    pub retry_multiplier: Option<u32>,
    pub retry_max_delay_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderOverrides {
    pub enable_development_chaoxing: Option<bool>,
    pub enable_development_welearn: Option<bool>,
    pub enable_development_uai: Option<bool>,
    pub enable_development_cidaren: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Environment {
    config_file: Option<PathBuf>,
    overrides: ConfigOverrides,
}

impl Environment {
    /// Reads only supported `ASTERISM_*` variables from the current process.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Environment`] when a supported value is invalid.
    pub fn from_process() -> Result<Self, ConfigError> {
        let mut supported = Vec::new();
        for (name, value) in std::env::vars_os() {
            let Some(name) = name.to_str() else {
                continue;
            };
            if !is_supported_environment_name(name) {
                continue;
            }
            let value = value
                .into_string()
                .map_err(|_| environment_error(name, "value is not valid Unicode"))?;
            supported.push((name.to_owned(), value));
        }
        Self::parse(supported)
    }

    /// Parses supported variables from an injected iterator for deterministic
    /// testing and embedding.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Environment`] when a supported value is invalid.
    pub fn parse(
        variables: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, ConfigError> {
        let mut environment = Self::default();
        for (name, value) in variables {
            match name.as_str() {
                CONFIG_FILE_ENV => {
                    if value.is_empty() {
                        return Err(environment_error(&name, "path cannot be empty"));
                    }
                    environment.config_file = Some(PathBuf::from(value));
                }
                BIND_ENV => {
                    environment.overrides.server.bind = Some(parse_env(&name, &value)?);
                }
                DATABASE_URL_ENV => {
                    environment.overrides.database.url = Some(value);
                }
                SESSION_TTL_SECONDS_ENV => {
                    environment.overrides.server.session_ttl_seconds =
                        Some(parse_env(&name, &value)?);
                }
                SECURE_COOKIES_ENV => {
                    environment.overrides.server.secure_cookies =
                        Some(parse_bool_env(&name, &value)?);
                }
                ENABLE_DEVELOPMENT_CHAOXING_ENV => {
                    environment.overrides.providers.enable_development_chaoxing =
                        Some(parse_bool_env(&name, &value)?);
                }
                ENABLE_DEVELOPMENT_WELEARN_ENV => {
                    environment.overrides.providers.enable_development_welearn =
                        Some(parse_bool_env(&name, &value)?);
                }
                ENABLE_DEVELOPMENT_UAI_ENV => {
                    environment.overrides.providers.enable_development_uai =
                        Some(parse_bool_env(&name, &value)?);
                }
                ENABLE_DEVELOPMENT_CIDAREN_ENV => {
                    environment.overrides.providers.enable_development_cidaren =
                        Some(parse_bool_env(&name, &value)?);
                }
                SCHEDULER_ENABLED_ENV => {
                    environment.overrides.scheduler.enabled = Some(parse_bool_env(&name, &value)?);
                }
                SCHEDULER_TICK_INTERVAL_SECONDS_ENV => {
                    environment.overrides.scheduler.tick_interval_seconds =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_MATERIALIZE_LIMIT_ENV => {
                    environment.overrides.scheduler.materialize_limit =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_CLAIM_LIMIT_ENV => {
                    environment.overrides.scheduler.claim_limit = Some(parse_env(&name, &value)?);
                }
                SCHEDULER_EXECUTION_CONCURRENCY_LIMIT_ENV => {
                    environment.overrides.scheduler.execution_concurrency_limit =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_CLAIM_TTL_SECONDS_ENV => {
                    environment.overrides.scheduler.claim_ttl_seconds =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_RETRY_MAX_ATTEMPTS_ENV => {
                    environment.overrides.scheduler.retry_max_attempts =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_RETRY_INITIAL_DELAY_SECONDS_ENV => {
                    environment.overrides.scheduler.retry_initial_delay_seconds =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_RETRY_MULTIPLIER_ENV => {
                    environment.overrides.scheduler.retry_multiplier =
                        Some(parse_env(&name, &value)?);
                }
                SCHEDULER_RETRY_MAX_DELAY_SECONDS_ENV => {
                    environment.overrides.scheduler.retry_max_delay_seconds =
                        Some(parse_env(&name, &value)?);
                }
                _ => {}
            }
        }
        Ok(environment)
    }

    pub fn config_file(&self) -> Option<&Path> {
        self.config_file.as_deref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read configuration file {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse configuration file {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid environment variable {name}: {reason}")]
    Environment { name: String, reason: String },
    #[error("invalid Asterism configuration: {0}")]
    Validation(String),
}

fn load_file(file: &ConfigFile) -> Result<Config, ConfigError> {
    let path = file.path();
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        }),
        Err(source)
            if matches!(file, ConfigFile::Optional(_))
                && source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(Config::default())
        }
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn parse_env<T>(name: &str, value: &str) -> Result<T, ConfigError>
where
    T: FromStr,
{
    value
        .parse()
        .map_err(|_| environment_error(name, "value has an invalid format"))
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(environment_error(name, "expected a boolean value")),
    }
}

fn environment_error(name: &str, reason: &str) -> ConfigError {
    ConfigError::Environment {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

fn is_supported_environment_name(name: &str) -> bool {
    matches!(
        name,
        CONFIG_FILE_ENV
            | BIND_ENV
            | DATABASE_URL_ENV
            | SESSION_TTL_SECONDS_ENV
            | SECURE_COOKIES_ENV
            | ENABLE_DEVELOPMENT_CHAOXING_ENV
            | ENABLE_DEVELOPMENT_WELEARN_ENV
            | ENABLE_DEVELOPMENT_UAI_ENV
            | ENABLE_DEVELOPMENT_CIDAREN_ENV
            | SCHEDULER_ENABLED_ENV
            | SCHEDULER_TICK_INTERVAL_SECONDS_ENV
            | SCHEDULER_MATERIALIZE_LIMIT_ENV
            | SCHEDULER_CLAIM_LIMIT_ENV
            | SCHEDULER_EXECUTION_CONCURRENCY_LIMIT_ENV
            | SCHEDULER_CLAIM_TTL_SECONDS_ENV
            | SCHEDULER_RETRY_MAX_ATTEMPTS_ENV
            | SCHEDULER_RETRY_INITIAL_DELAY_SECONDS_ENV
            | SCHEDULER_RETRY_MULTIPLIER_ENV
            | SCHEDULER_RETRY_MAX_DELAY_SECONDS_ENV
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn precedence_is_cli_then_environment_then_file_then_defaults() {
        let path = temporary_file("precedence");
        fs::write(
            &path,
            r#"
[server]
bind = "127.0.0.1:9001"
session_ttl_seconds = 101
secure_cookies = false

[database]
url = "sqlite://from-file.db"

[scheduler]
tick_interval_seconds = 11
claim_limit = 2
execution_concurrency_limit = 8

[providers]
enable_development_chaoxing = false
enable_development_welearn = false
enable_development_uai = false
enable_development_cidaren = false
"#,
        )
        .unwrap();
        let environment = Environment::parse([
            (BIND_ENV.to_owned(), "127.0.0.1:9002".to_owned()),
            (SESSION_TTL_SECONDS_ENV.to_owned(), "202".to_owned()),
            (SECURE_COOKIES_ENV.to_owned(), "true".to_owned()),
            (
                ENABLE_DEVELOPMENT_CHAOXING_ENV.to_owned(),
                "true".to_owned(),
            ),
            (ENABLE_DEVELOPMENT_WELEARN_ENV.to_owned(), "true".to_owned()),
            (ENABLE_DEVELOPMENT_UAI_ENV.to_owned(), "true".to_owned()),
            (ENABLE_DEVELOPMENT_CIDAREN_ENV.to_owned(), "true".to_owned()),
            (
                SCHEDULER_TICK_INTERVAL_SECONDS_ENV.to_owned(),
                "12".to_owned(),
            ),
            (
                SCHEDULER_EXECUTION_CONCURRENCY_LIMIT_ENV.to_owned(),
                "16".to_owned(),
            ),
        ])
        .unwrap();
        let cli = ConfigOverrides {
            server: ServerOverrides {
                bind: Some(localhost(9003)),
                session_ttl_seconds: None,
                secure_cookies: Some(false),
            },
            database: DatabaseOverrides {
                url: Some("sqlite://from-cli.db".to_owned()),
            },
            scheduler: SchedulerOverrides {
                claim_limit: Some(3),
                execution_concurrency_limit: Some(24),
                ..SchedulerOverrides::default()
            },
            providers: ProviderOverrides {
                enable_development_chaoxing: Some(false),
                enable_development_welearn: Some(false),
                enable_development_uai: Some(false),
                enable_development_cidaren: Some(false),
            },
        };

        let config = Config::load(&ConfigFile::required(&path), &environment, &cli).unwrap();

        assert_eq!(config.server.bind, localhost(9003));
        assert_eq!(config.server.session_ttl_seconds, 202);
        assert!(!config.server.secure_cookies);
        assert_eq!(config.database.url, "sqlite://from-cli.db");
        assert_eq!(config.scheduler.tick_interval_seconds, 12);
        assert_eq!(config.scheduler.claim_limit, 3);
        assert_eq!(config.scheduler.execution_concurrency_limit, 24);
        assert!(!config.providers.enable_development_chaoxing);
        assert!(!config.providers.enable_development_welearn);
        assert!(!config.providers.enable_development_uai);
        assert!(!config.providers.enable_development_cidaren);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn optional_missing_file_uses_defaults() {
        let path = temporary_file("missing");
        let config = Config::load(
            &ConfigFile::optional(path),
            &Environment::default(),
            &ConfigOverrides::default(),
        )
        .unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn required_missing_file_is_an_error() {
        let path = temporary_file("required-missing");
        let error = Config::load(
            &ConfigFile::required(path),
            &Environment::default(),
            &ConfigOverrides::default(),
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::Read { .. }));
    }

    #[test]
    fn unknown_file_key_is_rejected() {
        let path = temporary_file("unknown");
        fs::write(&path, "[server]\nunknown = true\n").unwrap();
        let error = Config::load(
            &ConfigFile::required(&path),
            &Environment::default(),
            &ConfigOverrides::default(),
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn environment_parsing_does_not_echo_invalid_values() {
        let secret_like_value = "not-a-boolean-sensitive-value";
        let error =
            Environment::parse([(SECURE_COOKIES_ENV.to_owned(), secret_like_value.to_owned())])
                .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(SECURE_COOKIES_ENV));
        assert!(!message.contains(secret_like_value));

        let environment =
            Environment::parse([(ENABLE_DEVELOPMENT_WELEARN_ENV.to_owned(), "true".to_owned())])
                .unwrap();
        assert_eq!(
            environment.overrides.providers.enable_development_welearn,
            Some(true)
        );
        let environment =
            Environment::parse([(ENABLE_DEVELOPMENT_UAI_ENV.to_owned(), "true".to_owned())])
                .unwrap();
        assert_eq!(
            environment.overrides.providers.enable_development_uai,
            Some(true)
        );
        let environment =
            Environment::parse([(ENABLE_DEVELOPMENT_CIDAREN_ENV.to_owned(), "true".to_owned())])
                .unwrap();
        assert_eq!(
            environment.overrides.providers.enable_development_cidaren,
            Some(true)
        );
    }

    #[test]
    fn unsafe_or_unsupported_merged_values_are_rejected() {
        let non_loopback = Config {
            server: ServerConfig {
                bind: "0.0.0.0:8068".parse().unwrap(),
                ..ServerConfig::default()
            },
            ..Config::default()
        };
        assert!(matches!(
            non_loopback.validate(),
            Err(ConfigError::Validation(_))
        ));

        let unsupported_database = Config {
            database: DatabaseConfig {
                url: "postgres://db.example/asterism".to_owned(),
            },
            ..Config::default()
        };
        assert!(matches!(
            unsupported_database.validate(),
            Err(ConfigError::Validation(_))
        ));

        let unsafe_scheduler = Config {
            scheduler: SchedulerConfig {
                claim_ttl_seconds: 1,
                tick_interval_seconds: 5,
                ..SchedulerConfig::default()
            },
            ..Config::default()
        };
        assert!(matches!(
            unsafe_scheduler.validate(),
            Err(ConfigError::Validation(_))
        ));

        let invalid_ai_profile = Config {
            ai: AiConfig {
                default_profile: "unknown".to_owned(),
                ..AiConfig::default()
            },
            ..Config::default()
        };
        assert!(matches!(
            invalid_ai_profile.validate(),
            Err(ConfigError::Validation(_))
        ));
    }

    fn localhost(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn temporary_file(label: &str) -> PathBuf {
        let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "asterism-config-{label}-{}-{id}.toml",
            std::process::id()
        ))
    }
}
