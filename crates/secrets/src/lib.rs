//! The only Core boundary through which plaintext credentials may pass.

use std::fmt;

use asterism_domain::{ProviderAccountId, SecretId, SessionKind, Timestamp, UserId};
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

/// A 256-bit key supplied by daemon configuration rather than persistence.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretKey([u8; 32]);

impl SecretKey {
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    /// Exposes key material only to the authenticated-encryption adapter.
    pub fn expose_secret(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretKey([REDACTED])")
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

impl SecretPurpose {
    pub fn is_provider_credential(self) -> bool {
        matches!(
            self,
            Self::ProviderPassword
                | Self::ProviderCookie
                | Self::ProviderAccessToken
                | Self::ProviderRefreshToken
                | Self::ProviderCompositeSession
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialAcquisition {
    NativeProviderLogin,
    CaptureTool,
    BrowserExtension,
    AndroidHelper,
    ManualImport,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCredential {
    pub provider_account_id: ProviderAccountId,
    pub secret: SecretRef,
    pub session_kind: SessionKind,
    pub acquired_via: CredentialAcquisition,
    pub captured_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}

#[derive(Debug)]
pub struct NewProviderCredential {
    pub provider_account_id: ProviderAccountId,
    pub purpose: SecretPurpose,
    pub session_kind: SessionKind,
    pub acquired_via: CredentialAcquisition,
    pub expires_at: Option<Timestamp>,
    pub value: SecretValue,
}

impl ProviderCredential {
    /// Validates non-secret credential metadata independently from persistence.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderCredentialError`] for non-Provider secret purposes or
    /// lifecycle timestamps that move backwards.
    pub fn validate(&self) -> Result<(), ProviderCredentialError> {
        if !self.secret.purpose.is_provider_credential() {
            return Err(ProviderCredentialError::InvalidPurpose);
        }
        if self.updated_at < self.captured_at
            || self
                .expires_at
                .is_some_and(|expires_at| expires_at <= self.captured_at)
        {
            return Err(ProviderCredentialError::InvalidTimestamps);
        }
        Ok(())
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= at)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProviderCredentialError {
    #[error("credential must use a Provider secret purpose")]
    InvalidPurpose,
    #[error("credential lifecycle timestamps are invalid")]
    InvalidTimestamps,
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

#[async_trait]
pub trait ProviderCredentialStore: Send + Sync {
    async fn create_provider_credential(
        &self,
        owner_user_id: UserId,
        credential: NewProviderCredential,
        access: &SecretAccess,
    ) -> Result<ProviderCredential, SecretStoreError>;

    async fn list_provider_credentials(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        access: &SecretAccess,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError>;

    async fn rotate_provider_credential(
        &self,
        owner_user_id: UserId,
        credential: &ProviderCredential,
        replacement: SecretValue,
        expires_at: Option<Timestamp>,
        access: &SecretAccess,
    ) -> Result<ProviderCredential, SecretStoreError>;

    async fn delete_provider_credential(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        secret_id: SecretId,
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
    #[error("secret value is empty or exceeds the supported size")]
    InvalidValue,
    #[error("Provider credentials require the account-scoped credential store")]
    CredentialManaged,
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
        let key = SecretKey::new([7; 32]);
        assert_eq!(format!("{bytes:?}"), "SecretValue([REDACTED])");
        assert_eq!(format!("{text:?}"), "SecretString([REDACTED])");
        assert_eq!(format!("{key:?}"), "SecretKey([REDACTED])");
    }

    #[test]
    fn provider_credential_rejects_non_provider_purpose_and_invalid_expiry() {
        let now = chrono::Utc::now();
        let mut credential = ProviderCredential {
            provider_account_id: ProviderAccountId::new(),
            secret: SecretRef {
                id: SecretId::new(),
                owner_user_id: UserId::new(),
                purpose: SecretPurpose::ProviderCookie,
                version: 1,
                key_id: "key-a".to_owned(),
                created_at: now,
                updated_at: now,
            },
            session_kind: SessionKind::Cookie,
            acquired_via: CredentialAcquisition::ManualImport,
            captured_at: now,
            expires_at: Some(now + chrono::Duration::hours(1)),
            updated_at: now,
        };
        assert_eq!(credential.validate(), Ok(()));
        assert!(!credential.is_expired_at(now));
        credential.secret.purpose = SecretPurpose::ServiceToken;
        assert_eq!(
            credential.validate(),
            Err(ProviderCredentialError::InvalidPurpose)
        );
        credential.secret.purpose = SecretPurpose::ProviderCookie;
        credential.expires_at = Some(now);
        assert_eq!(
            credential.validate(),
            Err(ProviderCredentialError::InvalidTimestamps)
        );
    }
}
