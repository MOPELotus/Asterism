use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use serde::{Deserialize, Deserializer, de::Visitor};
use zeroize::Zeroize;

const MAX_USER_INFO_BYTES: usize = 64 * 1_024;
const MAX_IDENTITY_BYTES: usize = 512;

/// Bounded identity facts required by UAI's read-only study-record routes.
pub(crate) struct UaiUserIdentity {
    app_user_id: String,
    sso_id: String,
}

impl UaiUserIdentity {
    pub(crate) fn app_user_id(&self) -> &str {
        &self.app_user_id
    }

    pub(crate) fn sso_id(&self) -> &str {
        &self.sso_id
    }
}

impl fmt::Debug for UaiUserIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiUserIdentity([REDACTED])")
    }
}

impl Drop for UaiUserIdentity {
    fn drop(&mut self) {
        self.app_user_id.zeroize();
        self.sso_id.zeroize();
    }
}

pub(crate) fn parse_user_identity(document: &[u8]) -> ProviderResult<UaiUserIdentity> {
    if document.is_empty() || document.len() > MAX_USER_INFO_BYTES {
        return Err(invalid_user_info_response());
    }
    let envelope: UserInfoEnvelope =
        serde_json::from_slice(document).map_err(|_| invalid_user_info_response())?;
    if envelope.success == Some(false) {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI user-info endpoint rejected the current session",
        ));
    }
    let mut user = envelope
        .value
        .and_then(|value| value.user_info)
        .ok_or_else(invalid_user_info_response)?;
    Ok(UaiUserIdentity {
        app_user_id: std::mem::take(&mut user.app_user_id.0),
        sso_id: std::mem::take(&mut user.sso_id.0),
    })
}

#[derive(Deserialize)]
struct UserInfoEnvelope {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    value: Option<UserInfoValue>,
}

#[derive(Deserialize)]
struct UserInfoValue {
    #[serde(default, rename = "userInfo")]
    user_info: Option<UserInfo>,
}

#[derive(Deserialize)]
struct UserInfo {
    #[serde(rename = "appUserId")]
    app_user_id: BoundedIdentity,
    #[serde(rename = "ssoId")]
    sso_id: BoundedIdentity,
}

struct BoundedIdentity(String);

impl Drop for BoundedIdentity {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<'de> Deserialize<'de> for BoundedIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct IdentityVisitor;

        impl Visitor<'_> for IdentityVisitor {
            type Value = BoundedIdentity;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded non-empty identity string or positive integer")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if valid_identity(value) {
                    Ok(BoundedIdentity(value.to_owned()))
                } else {
                    Err(E::custom("invalid bounded identity"))
                }
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value == 0 {
                    return Err(E::custom("identity integer must be positive"));
                }
                Ok(BoundedIdentity(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value <= 0 {
                    return Err(E::custom("identity integer must be positive"));
                }
                Ok(BoundedIdentity(value.to_string()))
            }
        }

        deserializer.deserialize_any(IdentityVisitor)
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTITY_BYTES
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn invalid_user_info_response() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI user-info endpoint returned an invalid response",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_INFO: &[u8] =
        include_bytes!("../../../fixtures/providers/uai/auth/user-info-valid.json");

    #[test]
    fn parser_returns_only_bounded_redacted_identity_facts() {
        let identity = parse_user_identity(USER_INFO).unwrap();
        assert_eq!(identity.app_user_id(), "42");
        assert_eq!(identity.sso_id(), "synthetic-sso-id");
        assert!(!format!("{identity:?}").contains("synthetic"));

        let text = parse_user_identity(
            br#"{"success":true,"value":{"userInfo":{"appUserId":"app-42","ssoId":"sso.42"}}}"#,
        )
        .unwrap();
        assert_eq!(text.app_user_id(), "app-42");
    }

    #[test]
    fn parser_rejects_missing_rejected_or_unsafe_identity_facts() {
        assert!(
            parse_user_identity(
                br#"{"success":true,"value":{"userInfo":{"appUserId":"synthetic"}}}"#
            )
            .is_err()
        );
        assert_eq!(
            parse_user_identity(
                br#"{"success":false,"value":{"userInfo":{"appUserId":42,"ssoId":"id"}}}"#
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::Authentication
        );
        assert!(
            parse_user_identity(
                br#"{"success":true,"value":{"userInfo":{"appUserId":"../unsafe","ssoId":"id"}}}"#
            )
            .is_err()
        );
        assert!(
            parse_user_identity(
                br#"{"success":true,"value":{"userInfo":{"appUserId":0,"ssoId":"id"}}}"#
            )
            .is_err()
        );
    }
}
