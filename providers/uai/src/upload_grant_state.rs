use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::UaiUploadGrant;

pub const UAI_UPLOAD_GRANT_STATE_TYPE: &str = "uai.upload.grant.v1";

const MAX_UPLOAD_GRANT_STATE_BYTES: usize = 32 * 1_024;

/// Bounded zeroizing bytes for one exact CMS upload grant.
pub struct EncodedUaiUploadGrantState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiUploadGrantState {
    /// Encodes the token/key grant only after it is bound to exact request and
    /// response digests.
    ///
    /// # Errors
    ///
    /// Rejects legacy/in-memory grants without complete transport lineage.
    pub fn try_new(grant: &UaiUploadGrant) -> ProviderResult<Self> {
        if grant.grant_request_digest() == [0; 32] || grant.grant_response_digest() == [0; 32] {
            return Err(invalid_grant_state());
        }
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&UploadGrantStateWireRef {
                schema: UAI_UPLOAD_GRANT_STATE_TYPE,
                token: grant.expose_token(),
                file_key: grant.file_key(),
                intent_fingerprint: grant.intent_fingerprint(),
                artifact_digest: grant.artifact_digest(),
                remote_task_id: grant.remote_task_id(),
                task_fingerprint: grant.task_fingerprint(),
                course_resource_id: grant.course_resource_id(),
                unit_id: grant.unit_id(),
                group_id: grant.group_id(),
                upload_position: grant.upload_position(),
                grant_request_digest: grant.grant_request_digest(),
                grant_response_digest: grant.grant_response_digest(),
            })
            .map_err(|_| invalid_grant_state())?,
        );
        if encoded.is_empty() || encoded.len() > MAX_UPLOAD_GRANT_STATE_BYTES {
            return Err(invalid_grant_state());
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(Self { value, digest })
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiUploadGrantState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiUploadGrantState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Exact recoverable CMS grant. It authorizes only the matching Qiniu request.
pub struct UaiUploadGrantState {
    grant: UaiUploadGrant,
}

impl UaiUploadGrantState {
    /// Decodes one exact grant against the independently persisted request and
    /// response digests.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, digest-mismatched or foreign grant state
    /// before exposing the short-lived token or object key.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_request_digest: [u8; 32],
        expected_response_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_UPLOAD_GRANT_STATE_BYTES {
            return Err(invalid_grant_state());
        }
        if <[u8; 32]>::from(Sha256::digest(bytes)) != expected_digest {
            return Err(foreign_grant_state());
        }
        let mut wire: UploadGrantStateWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_grant_state())?;
        if wire.schema != UAI_UPLOAD_GRANT_STATE_TYPE
            || wire.grant_request_digest != expected_request_digest
            || wire.grant_response_digest != expected_response_digest
            || [wire.grant_request_digest, wire.grant_response_digest].contains(&[0; 32])
        {
            return Err(foreign_grant_state());
        }
        let grant = UaiUploadGrant::restore_grant_state(
            Zeroizing::new(std::mem::take(&mut wire.token)),
            std::mem::take(&mut wire.file_key),
            std::mem::take(&mut wire.intent_fingerprint),
            std::mem::take(&mut wire.artifact_digest),
            std::mem::take(&mut wire.remote_task_id),
            std::mem::take(&mut wire.task_fingerprint),
            std::mem::take(&mut wire.course_resource_id),
            std::mem::take(&mut wire.unit_id),
            std::mem::take(&mut wire.group_id),
            wire.upload_position,
            wire.grant_request_digest,
            wire.grant_response_digest,
        )?;
        Ok(Self { grant })
    }

    pub const fn grant(&self) -> &UaiUploadGrant {
        &self.grant
    }

    pub fn into_grant(self) -> UaiUploadGrant {
        self.grant
    }
}

impl fmt::Debug for UaiUploadGrantState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadGrantState")
            .field("request_digest", &"[HASHED]")
            .field("response_digest", &"[HASHED]")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize)]
struct UploadGrantStateWireRef<'a> {
    schema: &'static str,
    token: &'a str,
    file_key: &'a str,
    intent_fingerprint: &'a str,
    artifact_digest: &'a str,
    remote_task_id: &'a str,
    task_fingerprint: &'a str,
    course_resource_id: &'a str,
    unit_id: &'a str,
    group_id: &'a str,
    upload_position: u32,
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct UploadGrantStateWire {
    schema: String,
    token: String,
    file_key: String,
    intent_fingerprint: String,
    artifact_digest: String,
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    upload_position: u32,
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
}

fn invalid_grant_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI upload grant state is invalid",
    )
}

fn foreign_grant_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI upload grant state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_state_round_trips_only_against_exact_request_and_response() {
        let grant = UaiUploadGrant::restore_grant_state(
            Zeroizing::new("synthetic-upload-token".to_owned()),
            "course/42/nothing.mp3".to_owned(),
            "uai-upload-v1:synthetic-intent".to_owned(),
            "sha256:synthetic-artifact".to_owned(),
            "group:2001:unit-1:group-upload".to_owned(),
            "v1:synthetic-task".to_owned(),
            "2001".to_owned(),
            "unit-1".to_owned(),
            "group-upload".to_owned(),
            1,
            [3; 32],
            [4; 32],
        )
        .unwrap();
        let encoded = EncodedUaiUploadGrantState::try_new(&grant).unwrap();
        let debug = format!("{encoded:?}");
        assert!(!debug.contains("synthetic-upload-token"));
        assert!(!debug.contains("course/42/nothing.mp3"));
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let restored = UaiUploadGrantState::decode_bound(&value, digest, [3; 32], [4; 32]).unwrap();
        assert_eq!(restored.grant().expose_token(), "synthetic-upload-token");
        assert_eq!(restored.grant().file_key(), "course/42/nothing.mp3");
        assert!(!format!("{restored:?}").contains("synthetic-upload-token"));

        assert!(UaiUploadGrantState::decode_bound(&value, [7; 32], [3; 32], [4; 32]).is_err());
        assert!(UaiUploadGrantState::decode_bound(&value, digest, [8; 32], [4; 32]).is_err());
        assert!(UaiUploadGrantState::decode_bound(&value, digest, [3; 32], [9; 32]).is_err());
    }
}
