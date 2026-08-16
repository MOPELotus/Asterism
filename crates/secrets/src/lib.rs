//! The only Core boundary through which plaintext credentials may pass.

use std::{collections::HashSet, fmt};

use asterism_domain::{
    AuthMethod, ProviderAccountId, ProviderId, SecretId, ServiceTokenId, SessionKind, Timestamp,
    UserId,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

const MAX_CREDENTIAL_FIELD_BYTES: usize = 1024 * 1024;
const MAX_ACCESS_REASON_BYTES: usize = 256;
const MAX_CORRELATION_ID_BYTES: usize = 128;

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
    ProviderUsername,
    ProviderPassword,
    ProviderCookie,
    ProviderAccessToken,
    ProviderRefreshToken,
    ProviderCompositeSession,
    WebSessionToken,
    ServiceToken,
    IntegrationCredential,
    BrowserJobCredential,
    ProviderExecutionState,
}

impl SecretPurpose {
    pub fn is_provider_credential(self) -> bool {
        matches!(
            self,
            Self::ProviderUsername
                | Self::ProviderPassword
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

/// One provider credential decrypted for a single bounded runtime operation.
/// The plaintext remains redacted in `Debug` output and is zeroized on drop.
#[derive(Debug)]
pub struct ResolvedProviderCredential {
    pub credential: ProviderCredential,
    pub value: SecretValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCredentialResolution {
    pub provider_account_id: ProviderAccountId,
    pub credential_refs: Vec<SecretId>,
    pub purposes: Vec<SecretPurpose>,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct ProviderCredentialRenewal {
    pub provider_account_id: ProviderAccountId,
    pub expected_credentials: Vec<ProviderCredential>,
    pub bundle: CredentialBundle,
    pub correlation_id: String,
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

#[derive(Debug)]
pub struct CredentialField {
    pub purpose: SecretPurpose,
    pub value: SecretValue,
}

pub struct CredentialBundle {
    pub provider_id: ProviderId,
    pub tenant: Option<String>,
    pub auth_method: AuthMethod,
    pub acquired_via: CredentialAcquisition,
    pub captured_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub session_kind: SessionKind,
    pub fields: Vec<CredentialField>,
    pub user_id_hint: Option<String>,
}

impl CredentialBundle {
    /// Validates a normalized in-memory candidate before a Provider sees it.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialBundleError`] when fields are empty, duplicated,
    /// non-Provider, oversized, or carry invalid lifecycle metadata.
    pub fn validate(&self) -> Result<(), CredentialBundleError> {
        if self.fields.is_empty() || self.fields.len() > 16 {
            return Err(CredentialBundleError::InvalidFieldCount);
        }
        let purposes = self
            .fields
            .iter()
            .map(|field| field.purpose)
            .collect::<HashSet<_>>();
        if purposes.len() != self.fields.len()
            || purposes
                .iter()
                .any(|purpose| !purpose.is_provider_credential())
        {
            return Err(CredentialBundleError::InvalidFields);
        }
        if self.fields.iter().any(|field| {
            let length = field.value.expose_secret().len();
            length == 0 || length > MAX_CREDENTIAL_FIELD_BYTES
        }) {
            return Err(CredentialBundleError::InvalidFields);
        }
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= self.captured_at)
        {
            return Err(CredentialBundleError::InvalidTimestamps);
        }
        if !valid_optional_hint(self.tenant.as_deref(), 256)
            || !valid_optional_hint(self.user_id_hint.as_deref(), 256)
        {
            return Err(CredentialBundleError::InvalidMetadata);
        }
        Ok(())
    }
}

impl fmt::Debug for CredentialBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialBundle")
            .field("provider_id", &self.provider_id)
            .field("tenant", &self.tenant.as_ref().map(|_| "[REDACTED]"))
            .field("auth_method", &self.auth_method)
            .field("acquired_via", &self.acquired_via)
            .field("captured_at", &self.captured_at)
            .field("expires_at", &self.expires_at)
            .field("session_kind", &self.session_kind)
            .field(
                "field_purposes",
                &self
                    .fields
                    .iter()
                    .map(|field| field.purpose)
                    .collect::<Vec<_>>(),
            )
            .field(
                "user_id_hint",
                &self.user_id_hint.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CredentialBundleError {
    #[error("credential bundle must contain 1-16 fields")]
    InvalidFieldCount,
    #[error("credential bundle fields must be unique non-empty Provider credentials")]
    InvalidFields,
    #[error("credential bundle timestamps are invalid")]
    InvalidTimestamps,
    #[error("credential bundle metadata is invalid")]
    InvalidMetadata,
}

fn valid_optional_hint(value: Option<&str>, max_bytes: usize) -> bool {
    value.is_none_or(|value| {
        !value.is_empty()
            && value.len() <= max_bytes
            && value.trim() == value
            && !value.chars().any(char::is_control)
    })
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

impl SecretAccess {
    pub fn authorizes(&self, owner_user_id: UserId) -> bool {
        let actor_valid = match &self.actor {
            SecretActor::User(user_id) => *user_id == owner_user_id,
            SecretActor::ServiceToken(_) => true,
            SecretActor::CoreService(service) => valid_actor_label(service),
            SecretActor::ProviderRuntime(provider_id) => valid_actor_label(provider_id),
        };
        actor_valid
            && valid_bounded_text(&self.correlation_id, MAX_CORRELATION_ID_BYTES)
            && valid_bounded_text(&self.reason, MAX_ACCESS_REASON_BYTES)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretActor {
    User(UserId),
    ServiceToken(ServiceTokenId),
    CoreService(&'static str),
    ProviderRuntime(String),
}

fn valid_bounded_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_actor_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
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
    async fn replace_provider_credentials(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        bundle: CredentialBundle,
        access: &SecretAccess,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError>;

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

/// Provider-scoped runtime boundary for resolving only the credentials already
/// bound to one account. Implementations must verify the complete opaque
/// reference set before returning plaintext.
#[async_trait]
pub trait ProviderCredentialResolver: Send + Sync {
    async fn resolve_provider_credentials(
        &self,
        request: ProviderCredentialResolution,
    ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError>;
}

/// Provider-scoped compare-and-replace boundary for a runtime which has
/// established and validated a fresh session. Stale credential metadata must
/// never overwrite a newer credential set or in-place Secret rotation.
#[async_trait]
pub trait ProviderCredentialRenewer: Send + Sync {
    async fn renew_provider_credentials(
        &self,
        request: ProviderCredentialRenewal,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError>;
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
    #[error("credential bundle does not match its Provider account")]
    AccountMismatch,
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

    #[test]
    fn credential_bundle_accepts_unique_provider_fields() {
        let captured_at = chrono::Utc::now();
        let bundle = CredentialBundle {
            provider_id: ProviderId::new("provider-a").expect("provider id"),
            tenant: Some("tenant-a".to_owned()),
            auth_method: AuthMethod::ImportedToken,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at,
            expires_at: Some(captured_at + chrono::Duration::hours(1)),
            session_kind: SessionKind::Composite,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(b"access-value".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderRefreshToken,
                    value: SecretValue::new(b"refresh-value".to_vec()),
                },
            ],
            user_id_hint: Some("account-a".to_owned()),
        };

        assert_eq!(bundle.validate(), Ok(()));
        let debug = format!("{bundle:?}");
        assert!(!debug.contains("access-value"));
        assert!(!debug.contains("refresh-value"));
        assert!(!debug.contains("tenant-a"));
        assert!(!debug.contains("account-a"));
    }

    #[test]
    fn credential_bundle_rejects_duplicate_or_non_provider_fields() {
        let captured_at = chrono::Utc::now();
        let mut bundle = CredentialBundle {
            provider_id: ProviderId::new("provider-a").expect("provider id"),
            tenant: None,
            auth_method: AuthMethod::ImportedCookie,
            acquired_via: CredentialAcquisition::ManualImport,
            captured_at,
            expires_at: None,
            session_kind: SessionKind::Cookie,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderCookie,
                    value: SecretValue::new(b"cookie-a".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderCookie,
                    value: SecretValue::new(b"cookie-b".to_vec()),
                },
            ],
            user_id_hint: None,
        };

        assert_eq!(bundle.validate(), Err(CredentialBundleError::InvalidFields));
        bundle.fields.truncate(1);
        bundle.fields[0].purpose = SecretPurpose::ServiceToken;
        assert_eq!(bundle.validate(), Err(CredentialBundleError::InvalidFields));
    }
}
