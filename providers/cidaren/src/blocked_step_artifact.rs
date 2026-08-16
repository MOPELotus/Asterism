use std::{fmt, str::FromStr};

use asterism_domain::{TaskId, Timestamp};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretValue;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{CidarenAssessmentRejectionKind, CidarenAttemptOperation, CidarenDefiniteRejection};

pub const CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE: &str = "cidaren.question-blocked-step.v1";

const REQUIRED_CHILDREN_PENDING_KIND: &str = "required_children_pending";
const MAX_ARTIFACT_BYTES: usize = 8 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 768;

/// Encoded Provider-private blocked-step evidence intended for Core's future
/// encrypted durable outcome boundary.
pub struct EncodedCidarenBlockedStepArtifact {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedCidarenBlockedStepArtifact {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedCidarenBlockedStepArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedCidarenBlockedStepArtifact")
            .field("value", &"[REDACTED]")
            .field("digest", &self.digest)
            .finish()
    }
}

/// Strictly bound Cidaren evidence for one definite, non-replayable blocked
/// remote step. Task identity remains zeroizing and absent from Debug output.
pub struct CidarenBlockedStepArtifact {
    task_id: TaskId,
    remote_task_id: Zeroizing<String>,
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    received_at: Timestamp,
}

impl CidarenBlockedStepArtifact {
    pub(crate) fn from_definite_rejection(
        task_id: TaskId,
        remote_task_id: &str,
        rejection: CidarenDefiniteRejection,
    ) -> ProviderResult<Self> {
        Self::try_new(
            task_id,
            remote_task_id,
            rejection.operation(),
            rejection.kind(),
            rejection.request_digest(),
            rejection.response_digest(),
            rejection.received_at(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn try_new(
        task_id: TaskId,
        remote_task_id: &str,
        operation: CidarenAttemptOperation,
        kind: CidarenAssessmentRejectionKind,
        request_digest: [u8; 32],
        response_digest: [u8; 32],
        received_at: Timestamp,
    ) -> ProviderResult<Self> {
        if !valid_remote_task_id(remote_task_id)
            || operation != CidarenAttemptOperation::SubmitChoseWord
            || kind != CidarenAssessmentRejectionKind::RequiredChildrenPending
            || request_digest == [0; 32]
            || response_digest == [0; 32]
        {
            return Err(protocol_drift(
                "Cidaren blocked-step evidence binding is invalid",
            ));
        }
        Ok(Self {
            task_id,
            remote_task_id: Zeroizing::new(remote_task_id.to_owned()),
            request_digest,
            response_digest,
            received_at,
        })
    }

    /// Encodes one canonical bounded artifact for encrypted persistence.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` if serialization fails or exceeds the bound.
    pub fn encode(&self) -> ProviderResult<EncodedCidarenBlockedStepArtifact> {
        let received_at =
            Zeroizing::new(self.received_at.to_rfc3339_opts(SecondsFormat::Nanos, true));
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&ArtifactWireRef {
                schema: CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE,
                task_id: self.task_id,
                remote_task_id: &self.remote_task_id,
                operation_type: CidarenAttemptOperation::SubmitChoseWord.operation_type(),
                rejection_kind: REQUIRED_CHILDREN_PENDING_KIND,
                request_digest: self.request_digest,
                response_digest: self.response_digest,
                received_at: &received_at,
            })
            .map_err(|_| invalid_response("Cidaren blocked-step artifact cannot be encoded"))?,
        );
        if encoded.is_empty() || encoded.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "Cidaren blocked-step artifact exceeds the encoded bound",
            ));
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedCidarenBlockedStepArtifact { value, digest })
    }

    /// Decodes only after checking the encrypted-artifact digest and the exact
    /// Task/request binding supplied by the durable operation ledger.
    ///
    /// # Errors
    ///
    /// Returns a typed error for digest, schema, Task, operation, rejection or
    /// timestamp drift.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        expected_task_id: TaskId,
        expected_remote_task_id: &str,
        expected_request_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid_response(
                "Cidaren blocked-step artifact exceeds the encoded bound",
            ));
        }
        if Sha256::digest(bytes).as_slice() != expected_digest {
            return Err(protocol_drift(
                "Cidaren blocked-step artifact digest does not match",
            ));
        }
        let mut wire: ArtifactWire = serde_json::from_slice(bytes)
            .map_err(|_| protocol_drift("Cidaren blocked-step artifact schema is invalid"))?;
        let received_at = DateTime::parse_from_rfc3339(&wire.received_at)
            .map_err(|_| protocol_drift("Cidaren blocked-step timestamp is invalid"))?
            .with_timezone(&Utc);
        let canonical_received_at = received_at.to_rfc3339_opts(SecondsFormat::Nanos, true);
        if wire.schema != CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE
            || wire.task_id != expected_task_id
            || !valid_remote_task_id(&wire.remote_task_id)
            || wire.remote_task_id != expected_remote_task_id
            || wire.operation_type != CidarenAttemptOperation::SubmitChoseWord.operation_type()
            || wire.rejection_kind != REQUIRED_CHILDREN_PENDING_KIND
            || wire.request_digest == [0; 32]
            || wire.request_digest != expected_request_digest
            || wire.response_digest == [0; 32]
            || wire.received_at != canonical_received_at
        {
            return Err(protocol_drift(
                "Cidaren blocked-step artifact binding is stale or foreign",
            ));
        }
        Ok(Self {
            task_id: wire.task_id,
            remote_task_id: Zeroizing::new(std::mem::take(&mut wire.remote_task_id)),
            request_digest: wire.request_digest,
            response_digest: wire.response_digest,
            received_at,
        })
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub fn remote_task_id(&self) -> &str {
        &self.remote_task_id
    }

    pub const fn operation(&self) -> CidarenAttemptOperation {
        CidarenAttemptOperation::SubmitChoseWord
    }

    pub const fn rejection_kind(&self) -> CidarenAssessmentRejectionKind {
        CidarenAssessmentRejectionKind::RequiredChildrenPending
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn received_at(&self) -> Timestamp {
        self.received_at
    }
}

impl fmt::Debug for CidarenBlockedStepArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenBlockedStepArtifact")
            .field("binding", &"configured")
            .field("operation", &self.operation())
            .field("rejection_kind", &self.rejection_kind())
            .field("request_digest", &self.request_digest)
            .field("response_digest", &self.response_digest)
            .field("received_at", &self.received_at)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct ArtifactWireRef<'a> {
    schema: &'static str,
    task_id: TaskId,
    remote_task_id: &'a str,
    operation_type: &'static str,
    rejection_kind: &'static str,
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    received_at: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct ArtifactWire {
    schema: String,
    #[zeroize(skip)]
    task_id: TaskId,
    remote_task_id: String,
    operation_type: String,
    rejection_kind: String,
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    received_at: String,
}

fn valid_remote_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_TASK_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && (value
            .strip_prefix("class-task:")
            .is_some_and(valid_positive_decimal)
            || value
                .strip_prefix("study-task:")
                .and_then(|identity| identity.split_once(':'))
                .is_some_and(|(course_id, list_id)| {
                    valid_component(course_id) && valid_component(list_id)
                }))
}

fn valid_positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && u64::from_str(value).is_ok_and(|value| value > 0)
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && !value.contains(':')
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;

    #[test]
    fn blocked_step_artifact_roundtrips_every_recovery_binding() {
        let task_id = TaskId::new();
        let request_digest = [7; 32];
        let response_digest = [9; 32];
        let received_at = Utc.with_ymd_and_hms(2026, 8, 16, 1, 2, 3).unwrap();
        let artifact = CidarenBlockedStepArtifact::try_new(
            task_id,
            "class-task:2002",
            CidarenAttemptOperation::SubmitChoseWord,
            CidarenAssessmentRejectionKind::RequiredChildrenPending,
            request_digest,
            response_digest,
            received_at,
        )
        .unwrap();
        let encoded = artifact.encode().unwrap();
        let digest = encoded.digest();
        assert_eq!(artifact.encode().unwrap().digest(), digest);
        assert!(!format!("{artifact:?}").contains("class-task:2002"));
        assert!(!format!("{encoded:?}").contains("class-task:2002"));

        let value = encoded.into_secret_value();
        let decoded = CidarenBlockedStepArtifact::decode_bound(
            &value,
            digest,
            task_id,
            "class-task:2002",
            request_digest,
        )
        .unwrap();
        assert_eq!(decoded.task_id(), task_id);
        assert_eq!(decoded.remote_task_id(), "class-task:2002");
        assert_eq!(
            decoded.operation(),
            CidarenAttemptOperation::SubmitChoseWord
        );
        assert_eq!(
            decoded.rejection_kind(),
            CidarenAssessmentRejectionKind::RequiredChildrenPending
        );
        assert_eq!(decoded.request_digest(), request_digest);
        assert_eq!(decoded.response_digest(), response_digest);
        assert_eq!(decoded.received_at(), received_at);

        assert!(
            CidarenBlockedStepArtifact::decode_bound(
                &value,
                [3; 32],
                task_id,
                "class-task:2002",
                request_digest,
            )
            .is_err()
        );
        assert!(
            CidarenBlockedStepArtifact::decode_bound(
                &value,
                digest,
                TaskId::new(),
                "class-task:2002",
                request_digest,
            )
            .is_err()
        );
        assert!(
            CidarenBlockedStepArtifact::decode_bound(
                &value,
                digest,
                task_id,
                "class-task:9999",
                request_digest,
            )
            .is_err()
        );
        assert!(
            CidarenBlockedStepArtifact::decode_bound(
                &value,
                digest,
                task_id,
                "class-task:2002",
                [8; 32],
            )
            .is_err()
        );
    }

    #[test]
    fn blocked_step_artifact_rejects_unknown_or_malformed_state() {
        let task_id = TaskId::new();
        let received_at = "2026-08-16T01:02:03.000000000Z";
        for value in [
            json!({
                "schema": CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE,
                "task_id": task_id,
                "remote_task_id": "class-task:2002",
                "operation_type": "cidaren.start-answer.v1",
                "rejection_kind": REQUIRED_CHILDREN_PENDING_KIND,
                "request_digest": vec![7_u8; 32],
                "response_digest": vec![9_u8; 32],
                "received_at": received_at,
            }),
            json!({
                "schema": CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE,
                "task_id": task_id,
                "remote_task_id": "class-task:2002",
                "operation_type": "cidaren.submit-chose-word.v1",
                "rejection_kind": "future_reason",
                "request_digest": vec![7_u8; 32],
                "response_digest": vec![9_u8; 32],
                "received_at": received_at,
            }),
            json!({
                "schema": CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE,
                "task_id": task_id,
                "remote_task_id": "class-task:2002",
                "operation_type": "cidaren.submit-chose-word.v1",
                "rejection_kind": REQUIRED_CHILDREN_PENDING_KIND,
                "request_digest": vec![0_u8; 32],
                "response_digest": vec![9_u8; 32],
                "received_at": received_at,
            }),
            json!({
                "schema": CIDAREN_BLOCKED_STEP_ARTIFACT_TYPE,
                "task_id": task_id,
                "remote_task_id": "class-task:2002",
                "operation_type": "cidaren.submit-chose-word.v1",
                "rejection_kind": REQUIRED_CHILDREN_PENDING_KIND,
                "request_digest": vec![7_u8; 32],
                "response_digest": vec![9_u8; 32],
                "received_at": "2026-08-16T01:02:03Z",
                "unexpected": true,
            }),
        ] {
            let value = SecretValue::new(serde_json::to_vec(&value).unwrap());
            let digest = Sha256::digest(value.expose_secret()).into();
            assert!(
                CidarenBlockedStepArtifact::decode_bound(
                    &value,
                    digest,
                    task_id,
                    "class-task:2002",
                    [7; 32],
                )
                .is_err()
            );
        }
    }
}
