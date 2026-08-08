//! The only Core boundary through which plaintext credentials may pass.

use std::fmt;

use asterism_domain::{SecretId, Timestamp, UserId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Owned secret bytes which zero their allocation on drop and never reveal
/// their value through `Debug`.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }

    /// Explicitly exposes the plaintext to a component already authorized to
    /// use this secret. Callers must not log or persist the returned slice.
    pub fn expose_secret(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

/// UTF-8 secret used for passwords and other text credentials.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Explicitly exposes plaintext for a narrowly scoped cryptographic or
    /// provider operation. Callers must not log the returned value.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SecretRef {
    pub id: SecretId,
    pub owner_user_id: UserId,
    pub purpose: SecretPurpose,
    pub version: u32,
    pub key_id: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretPurpose {
    ProviderPassword,
    ProviderCookie,
    ProviderAccessToken,
    ProviderRefreshToken,
    ProviderCompositeSession,
    WebSessionToken,
    ServiceToken,
    IntegrationCredential,
    BrowserJobCredential,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretAccess {
    pub actor: SecretActor,
    pub correlation_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretActor {
    User(UserId),
    CoreService(&'static str),
    ProviderRuntime(String),
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn put(
        &self,
        owner_user_id: UserId,
        purpose: SecretPurpose,
        value: SecretValue,
        access: &SecretAccess,
    ) -> Result<SecretRef, SecretStoreError>;

    async fn get(
        &self,
        secret: &SecretRef,
        access: &SecretAccess,
    ) -> Result<SecretValue, SecretStoreError>;

    async fn rotate(
        &self,
        secret: &SecretRef,
        replacement: SecretValue,
        access: &SecretAccess,
    ) -> Result<SecretRef, SecretStoreError>;

    async fn delete(
        &self,
        secret: &SecretRef,
        access: &SecretAccess,
    ) -> Result<(), SecretStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    #[error("secret does not exist")]
    NotFound,
    #[error("secret access is not authorized")]
    Unauthorized,
    #[error("secret version has changed")]
    VersionConflict,
    #[error("secret encryption key is unavailable")]
    KeyUnavailable,
    #[error("secret storage operation failed")]
    Storage,
    #[error("secret decryption or authentication failed")]
    AuthenticationFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_plaintext() {
        let bytes = SecretValue::new(b"provider-token".to_vec());
        let text = SecretString::new("password");
        assert_eq!(format!("{bytes:?}"), "SecretValue([REDACTED])");
        assert_eq!(format!("{text:?}"), "SecretString([REDACTED])");
    }
}
