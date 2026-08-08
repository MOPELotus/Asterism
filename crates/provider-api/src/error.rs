use serde::{Deserialize, Serialize};

pub type ProviderResult<T> = Result<T, ProviderError>;

/// A sanitized provider error. Secret-bearing response bodies must not be placed
/// in `message` or `provider_code`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub provider_code: Option<String>,
    pub retry_after_seconds: Option<u64>,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            provider_code: None,
            retry_after_seconds: None,
        }
    }

    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ProviderErrorKind::RateLimited
                | ProviderErrorKind::Network
                | ProviderErrorKind::ProviderUnavailable
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Authentication,
    Authorization,
    RateLimited,
    Network,
    ProviderUnavailable,
    ProtocolDrift,
    RemoteChanged,
    UnsupportedTask,
    HumanRequired,
    InvalidResponse,
    Internal,
}
