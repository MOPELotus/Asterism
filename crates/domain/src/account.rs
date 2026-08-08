use serde::{Deserialize, Serialize};

use crate::{CourseId, ProviderAccountId, SecretId, Timestamp, UserId};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Creates a stable identifier suitable for routes and persistence.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderIdError`] when the value is empty, longer than 64
    /// bytes, or contains characters outside lowercase ASCII, digits, and `-`.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderIdError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(ProviderIdError)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("provider id must contain 1-64 lowercase ASCII letters, digits, or hyphens")]
pub struct ProviderIdError;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderAccount {
    pub id: ProviderAccountId,
    pub owner_id: UserId,
    pub provider_id: ProviderId,
    pub display_name: String,
    pub tenant: Option<String>,
    pub auth_state: crate::AuthState,
    pub network_profile_id: Option<String>,
    pub credential_refs: Vec<SecretId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Course {
    pub id: CourseId,
    pub provider_account_id: ProviderAccountId,
    pub remote_id: String,
    pub title: String,
    pub term: Option<String>,
    pub teacher: Option<String>,
    pub remote_status: Option<String>,
    pub metadata: serde_json::Value,
    pub last_seen_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_is_stable_for_routes_and_storage() {
        assert!(ProviderId::new("unipus-ai").is_ok());
        assert!(ProviderId::new("UCampus").is_err());
        assert!(ProviderId::new("with space").is_err());
    }
}
