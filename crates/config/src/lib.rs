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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
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
    }
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
"#,
        )
        .unwrap();
        let environment = Environment::parse([
            (BIND_ENV.to_owned(), "127.0.0.1:9002".to_owned()),
            (SESSION_TTL_SECONDS_ENV.to_owned(), "202".to_owned()),
            (SECURE_COOKIES_ENV.to_owned(), "true".to_owned()),
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
        };

        let config = Config::load(&ConfigFile::required(&path), &environment, &cli).unwrap();

        assert_eq!(config.server.bind, localhost(9003));
        assert_eq!(config.server.session_ttl_seconds, 202);
        assert!(!config.server.secure_cookies);
        assert_eq!(config.database.url, "sqlite://from-cli.db");
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
