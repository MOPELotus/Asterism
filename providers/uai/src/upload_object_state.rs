use std::fmt;

use asterism_domain::ProviderId;
use asterism_provider_api::{
    ExecutionMutationReceipt, ExecutionMutationRecoveryRecord, ExecutionMutationSink,
    ExecutionMutationStageOutput, ProviderError, ProviderErrorKind, ProviderResult,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{UAI_UPLOAD_OBJECT_OPERATION_TYPE, UaiUploadedArtifact, metadata::PROVIDER_ID};

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

    fn into_stage_output(self, ordinal: u32) -> ProviderResult<ExecutionMutationStageOutput> {
        ExecutionMutationStageOutput::try_new(
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_object_state())?,
            ordinal,
            UAI_UPLOAD_OBJECT_STATE_TYPE,
            self.digest,
            self.value,
        )
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
    /// Restores one definitely accepted Qiniu successor only from Core's
    /// atomic receipt-plus-stage-output recovery record.
    ///
    /// # Errors
    ///
    /// Rejects missing encrypted successor bytes and any Provider, type,
    /// ordinal, request, response or digest substitution.
    pub fn decode_recovery_record(
        record: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<Self> {
        let receipt = record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_object_state)?;
        let output = record.stage_output().ok_or_else(foreign_object_state)?;
        if record.issue().operation_type() != UAI_UPLOAD_OBJECT_OPERATION_TYPE
            || record.issue().ordinal() != 1
            || record.verification().is_some()
            || output.provider_id().as_str() != PROVIDER_ID
            || output.ordinal() != record.issue().ordinal()
            || output.output_type() != UAI_UPLOAD_OBJECT_STATE_TYPE
        {
            return Err(foreign_object_state());
        }
        Self::decode_bound(
            output.value(),
            output.output_digest(),
            record.issue().request_digest(),
            receipt.response_digest(),
        )
    }

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

    pub(crate) fn same_recovery_authority(&self, other: &Self) -> bool {
        let left = self.uploaded();
        let right = other.uploaded();
        left.remote_task_id() == right.remote_task_id()
            && left.task_fingerprint() == right.task_fingerprint()
            && left.course_resource_id() == right.course_resource_id()
            && left.unit_id() == right.unit_id()
            && left.group_id() == right.group_id()
            && left.upload_position() == right.upload_position()
            && left.file_key() == right.file_key()
            && left.artifact_digest() == right.artifact_digest()
            && left.intent_fingerprint() == right.intent_fingerprint()
            && left.object_request_digest() == right.object_request_digest()
            && left.object_response_digest() == right.object_response_digest()
            && left.object_hash() == right.object_hash()
    }
}

/// Atomically records one definite accepted Qiniu receipt and its encrypted
/// uploaded-object successor. The helper never performs the upload itself.
///
/// # Errors
///
/// Rejects a non-first object ordinal, incomplete uploaded state, or a sink
/// that cannot persist the accepted receipt and successor in one transaction.
pub async fn record_accepted_upload_object(
    ordinal: u32,
    uploaded: &UaiUploadedArtifact,
    sink: &(dyn ExecutionMutationSink + Send + Sync),
) -> ProviderResult<()> {
    if ordinal != 1 {
        return Err(foreign_object_state());
    }
    let receipt = ExecutionMutationReceipt::new(ordinal, uploaded.object_response_digest(), true)?;
    let output = EncodedUaiUploadObjectState::try_new(uploaded)?.into_stage_output(ordinal)?;
    sink.record_receipt_with_stage_output(receipt, output).await
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

        let recovery = ExecutionMutationRecoveryRecord::try_new(
            asterism_provider_api::ExecutionMutationIssue::new(
                1,
                UAI_UPLOAD_OBJECT_OPERATION_TYPE,
                uploaded.object_request_digest(),
            )
            .unwrap(),
            Some(
                ExecutionMutationReceipt::new(1, uploaded.object_response_digest(), true).unwrap(),
            ),
            None,
        )
        .unwrap();
        assert!(UaiUploadObjectState::decode_recovery_record(&recovery).is_err());
        let recovered = recovery
            .clone()
            .try_with_stage_output(
                EncodedUaiUploadObjectState::try_new(&uploaded)
                    .unwrap()
                    .into_stage_output(1)
                    .unwrap(),
            )
            .unwrap();
        assert!(
            UaiUploadObjectState::decode_recovery_record(&recovered)
                .unwrap()
                .same_recovery_authority(&restored)
        );
        let foreign_encoded = EncodedUaiUploadObjectState::try_new(&uploaded).unwrap();
        let foreign = recovery
            .try_with_stage_output(
                ExecutionMutationStageOutput::try_new(
                    ProviderId::new("foreign").unwrap(),
                    1,
                    "foreign.upload.object.v1",
                    foreign_encoded.digest(),
                    foreign_encoded.into_secret_value(),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(UaiUploadObjectState::decode_recovery_record(&foreign).is_err());
    }
}
