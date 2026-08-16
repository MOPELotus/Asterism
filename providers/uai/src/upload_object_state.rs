use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::UaiUploadedArtifact;

pub const UAI_UPLOAD_OBJECT_STATE_TYPE: &str = "uai.upload.object.v1";

const MAX_UPLOAD_OBJECT_STATE_BYTES: usize = 64 * 1_024;

/// Bounded zeroizing bytes for one definitely accepted Qiniu object result.
pub struct EncodedUaiUploadObjectState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiUploadObjectState {
    /// Encodes the exact uploaded-artifact successor after a definite Qiniu
    /// response has repeated the granted key.
    ///
    /// # Errors
    ///
    /// Rejects legacy/incomplete artifacts that do not retain both exact
    /// request and response digests.
    pub fn try_new(uploaded: &UaiUploadedArtifact) -> ProviderResult<Self> {
        if uploaded.object_request_digest() == [0; 32]
            || uploaded.object_response_digest() == [0; 32]
        {
            return Err(invalid_object_state());
        }
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&UploadObjectStateWireRef {
                schema: UAI_UPLOAD_OBJECT_STATE_TYPE,
                remote_task_id: uploaded.remote_task_id(),
                task_fingerprint: uploaded.task_fingerprint(),
                course_resource_id: uploaded.course_resource_id(),
                unit_id: uploaded.unit_id(),
                group_id: uploaded.group_id(),
                upload_position: uploaded.upload_position(),
                file_key: uploaded.file_key(),
                artifact_digest: uploaded.artifact_digest(),
                intent_fingerprint: uploaded.intent_fingerprint(),
                object_request_digest: uploaded.object_request_digest(),
                object_response_digest: uploaded.object_response_digest(),
                object_hash: uploaded.object_hash(),
            })
            .map_err(|_| invalid_object_state())?,
        );
        if encoded.is_empty() || encoded.len() > MAX_UPLOAD_OBJECT_STATE_BYTES {
            return Err(invalid_object_state());
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

impl fmt::Debug for EncodedUaiUploadObjectState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiUploadObjectState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Exact accepted object-store successor, still not independent verification.
pub struct UaiUploadObjectState {
    uploaded: UaiUploadedArtifact,
}

impl UaiUploadObjectState {
    /// Decodes one accepted object state against Core's exact issue/receipt
    /// digests. The response remains a mutation receipt, never readback proof.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, digest-mismatched or foreign request/
    /// response lineage before exposing the object key or optional Qiniu hash.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_request_digest: [u8; 32],
        expected_response_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_UPLOAD_OBJECT_STATE_BYTES {
            return Err(invalid_object_state());
        }
        if <[u8; 32]>::from(Sha256::digest(bytes)) != expected_digest {
            return Err(foreign_object_state());
        }
        let mut wire: UploadObjectStateWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_object_state())?;
        if wire.schema != UAI_UPLOAD_OBJECT_STATE_TYPE
            || wire.object_request_digest != expected_request_digest
            || wire.object_response_digest != expected_response_digest
            || [wire.object_request_digest, wire.object_response_digest].contains(&[0; 32])
        {
            return Err(foreign_object_state());
        }
        let object_hash = wire.object_hash.take().map(Zeroizing::new);
        let uploaded = UaiUploadedArtifact::restore_object_state(
            std::mem::take(&mut wire.remote_task_id),
            std::mem::take(&mut wire.task_fingerprint),
            std::mem::take(&mut wire.course_resource_id),
            std::mem::take(&mut wire.unit_id),
            std::mem::take(&mut wire.group_id),
            wire.upload_position,
            std::mem::take(&mut wire.file_key),
            std::mem::take(&mut wire.artifact_digest),
            std::mem::take(&mut wire.intent_fingerprint),
            wire.object_request_digest,
            wire.object_response_digest,
            object_hash,
        )?;
        Ok(Self { uploaded })
    }

    pub const fn uploaded(&self) -> &UaiUploadedArtifact {
        &self.uploaded
    }

    pub fn into_uploaded(self) -> UaiUploadedArtifact {
        self.uploaded
    }
}

impl fmt::Debug for UaiUploadObjectState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadObjectState")
            .field("request_digest", &"[HASHED]")
            .field("response_digest", &"[HASHED]")
            .field(
                "object_hash",
                &self.uploaded.object_hash().map(|_| "[REDACTED]"),
            )
            .field("content", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize)]
struct UploadObjectStateWireRef<'a> {
    schema: &'static str,
    remote_task_id: &'a str,
    task_fingerprint: &'a str,
    course_resource_id: &'a str,
    unit_id: &'a str,
    group_id: &'a str,
    upload_position: u32,
    file_key: &'a str,
    artifact_digest: &'a str,
    intent_fingerprint: &'a str,
    object_request_digest: [u8; 32],
    object_response_digest: [u8; 32],
    object_hash: Option<&'a str>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct UploadObjectStateWire {
    schema: String,
    remote_task_id: String,
    task_fingerprint: String,
    course_resource_id: String,
    unit_id: String,
    group_id: String,
    upload_position: u32,
    file_key: String,
    artifact_digest: String,
    intent_fingerprint: String,
    object_request_digest: [u8; 32],
    object_response_digest: [u8; 32],
    object_hash: Option<String>,
}

fn invalid_object_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI upload object state is invalid",
    )
}

fn foreign_object_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI upload object state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_state_round_trips_only_against_exact_issue_and_receipt() {
        let uploaded = UaiUploadedArtifact::fixture(1);
        let encoded = EncodedUaiUploadObjectState::try_new(&uploaded).unwrap();
        assert!(!format!("{encoded:?}").contains(uploaded.file_key()));
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        let restored = UaiUploadObjectState::decode_bound(
            &value,
            digest,
            uploaded.object_request_digest(),
            uploaded.object_response_digest(),
        )
        .unwrap();
        assert_eq!(restored.uploaded().file_key(), uploaded.file_key());
        assert_eq!(restored.uploaded().object_hash(), uploaded.object_hash());
        assert!(!format!("{restored:?}").contains(uploaded.file_key()));

        assert!(
            UaiUploadObjectState::decode_bound(
                &value,
                [7; 32],
                uploaded.object_request_digest(),
                uploaded.object_response_digest(),
            )
            .is_err()
        );
        assert!(
            UaiUploadObjectState::decode_bound(
                &value,
                digest,
                [8; 32],
                uploaded.object_response_digest(),
            )
            .is_err()
        );
        assert!(
            UaiUploadObjectState::decode_bound(
                &value,
                digest,
                uploaded.object_request_digest(),
                [9; 32],
            )
            .is_err()
        );
    }
}
