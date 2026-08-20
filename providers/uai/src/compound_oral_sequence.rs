use std::fmt;

use asterism_domain::{ProviderId, SubmissionReceipt, Timestamp};
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
    ExecutionMutationSequenceAdvanceCondition, ExecutionMutationSequencePhase,
    ExecutionMutationSequencePlan, ExecutionMutationSink, ExecutionMutationStageOutput,
    ExecutionMutationVerification, ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact,
    ProviderResult,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    UaiCompoundOralPlanState, UaiCompoundOralSubmission, UaiCompoundOralSubmissionRequest,
    UaiCompoundOralVerification, metadata::PROVIDER_ID, parse_compound_oral_verification,
    parse_submission_receipt,
};

pub const UAI_COMPOUND_ORAL_SEQUENCE_TYPE: &str = "uai.compound-oral.submit.v1";
pub const UAI_COMPOUND_ORAL_OPERATION_TYPE: &str = "uai.compound-oral.submit";
pub const UAI_COMPOUND_ORAL_RESULT_STATE_TYPE: &str = "uai.compound-oral.result.v1";
pub const UAI_COMPOUND_ORAL_MAXIMUM_ATTEMPTS: u32 = 1;

const UAI_COMPOUND_ORAL_ORDINAL: u32 = 1;
const MAX_COMPOUND_ORAL_RESULT_STATE_BYTES: usize = 16 * 1_024;

/// Sanitized Core projection for one exact, single-occurrence compound-oral
/// mutation.
///
/// Once ordinal one is issued it is never replayed. A rejected, malformed or
/// ambiguous response therefore leaves no receipt and requires explicit
/// recovery/readback or human intervention rather than another submit.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiCompoundOralSubmissionSequence {
    plan_binding_digest: [u8; 32],
    artifact: ProviderExecutionPlanArtifact,
    plan: ExecutionMutationSequencePlan,
}

impl fmt::Debug for UaiCompoundOralSubmissionSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralSubmissionSequence")
            .field("plan_binding_digest", &"[HASHED]")
            .field("artifact", &self.artifact)
            .field("plan", &self.plan)
            .finish()
    }
}

impl UaiCompoundOralSubmissionSequence {
    /// Projects one complete private compound-oral plan into Core's bounded
    /// single-occurrence sequence without copying answers or oral evidence.
    ///
    /// # Errors
    ///
    /// Rejects an invalid semantic binding or compact Core projection.
    pub fn try_new(submission: &UaiCompoundOralSubmission) -> ProviderResult<Self> {
        let plan_binding_digest = submission.plan_binding_digest()?;
        if plan_binding_digest == [0; 32] {
            return Err(invalid_sequence());
        }
        let artifact = submission.plan_artifact()?;
        let phase = ExecutionMutationSequencePhase::try_new(
            UAI_COMPOUND_ORAL_OPERATION_TYPE,
            1,
            UAI_COMPOUND_ORAL_MAXIMUM_ATTEMPTS,
            true,
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
            None,
        )
        .map_err(|_| invalid_sequence())?;
        let plan = ExecutionMutationSequencePlan::try_new(
            artifact.artifact_digest(),
            UAI_COMPOUND_ORAL_SEQUENCE_TYPE,
            vec![phase],
        )
        .map_err(|_| invalid_sequence())?;
        Ok(Self {
            plan_binding_digest,
            artifact,
            plan,
        })
    }

    pub const fn artifact(&self) -> &ProviderExecutionPlanArtifact {
        &self.artifact
    }

    pub const fn plan(&self) -> &ExecutionMutationSequencePlan {
        &self.plan
    }

    pub fn into_parts(self) -> (ProviderExecutionPlanArtifact, ExecutionMutationSequencePlan) {
        (self.artifact, self.plan)
    }

    /// Freezes the accepted donor version and exact request/response lineage
    /// for one atomic receipt-plus-encrypted-state persistence boundary.
    ///
    /// The state carries no ordinary answer or oral value. Readback recovery
    /// additionally requires the independently encrypted complete plan state.
    ///
    /// # Errors
    ///
    /// Rejects a malformed receipt, a foreign exact request or an outcome from
    /// another valid semantic plan/request.
    pub fn accepted_result_state(
        &self,
        request: &UaiCompoundOralSubmissionRequest,
        outcome: &UaiCompoundOralSubmissionOutcome,
    ) -> ProviderResult<UaiCompoundOralResultState> {
        self.validate_request(request)?;
        if !outcome.matches(self.plan_binding_digest, request.request_digest()) {
            return Err(foreign_result_state());
        }
        let submission_version = outcome
            .receipt
            .provider_trace_id
            .as_deref()
            .filter(|version| {
                outcome.receipt.remote_status == "accepted"
                    && crate::submission_execute::valid_submission_version(version)
            })
            .ok_or_else(foreign_result_state)?;
        Ok(UaiCompoundOralResultState {
            ordinal: UAI_COMPOUND_ORAL_ORDINAL,
            plan_digest: self.plan.plan_digest(),
            artifact_digest: self.artifact.artifact_digest(),
            plan_binding_digest: self.plan_binding_digest,
            request_digest: request.request_digest(),
            response_digest: outcome.response_digest,
            accepted_at: outcome.receipt.received_at,
            submission_version: Zeroizing::new(submission_version.to_owned()),
        })
    }

    /// Freezes the exact single-occurrence sequence before request issuance.
    ///
    /// # Errors
    ///
    /// Returns a storage or sequence conflict from the durable sink.
    pub async fn prepare(
        &self,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        sink.prepare_sequence_plan(&self.plan).await
    }

    /// Persists the exact request identity before dispatch. Repeating this
    /// issuance must fail closed in the durable sink.
    ///
    /// # Errors
    ///
    /// Rejects a request materialized from another semantic plan or a sink
    /// conflict.
    pub async fn issue(
        &self,
        request: &UaiCompoundOralSubmissionRequest,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_request(request)?;
        sink.issue(&ExecutionMutationIssue::new(
            UAI_COMPOUND_ORAL_ORDINAL,
            UAI_COMPOUND_ORAL_OPERATION_TYPE,
            request.request_digest(),
        )?)
        .await
    }

    /// Persists only a strictly parsed accepted response. Rejected, malformed
    /// and ambiguous outcomes have no value of this type and cannot call this
    /// boundary.
    ///
    /// # Errors
    ///
    /// Rejects foreign request/outcome material or a sink-side receipt
    /// conflict.
    pub async fn record_accepted_outcome(
        &self,
        request: &UaiCompoundOralSubmissionRequest,
        outcome: &UaiCompoundOralSubmissionOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_request(request)?;
        if !outcome.matches(self.plan_binding_digest, request.request_digest()) {
            return Err(foreign_sequence_material());
        }
        let receipt = ExecutionMutationReceipt::new(
            UAI_COMPOUND_ORAL_ORDINAL,
            outcome.response_digest,
            true,
        )?;
        let encoded = self.accepted_result_state(request, outcome)?.encode()?;
        let output = ExecutionMutationStageOutput::try_new(
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_result_state())?,
            UAI_COMPOUND_ORAL_ORDINAL,
            UAI_COMPOUND_ORAL_RESULT_STATE_TYPE,
            encoded.digest(),
            encoded.into_secret_value(),
        )?;
        sink.record_receipt_with_stage_output(receipt, output).await
    }

    fn validate_request(&self, request: &UaiCompoundOralSubmissionRequest) -> ProviderResult<()> {
        if request.plan_binding_digest() != self.plan_binding_digest
            || request.request_digest() == [0; 32]
        {
            Err(foreign_sequence_material())
        } else {
            Ok(())
        }
    }
}

/// Encoded accepted-result state intended only for the encrypted Provider
/// execution-state store.
pub struct EncodedUaiCompoundOralResultState {
    value: SecretValue,
    digest: [u8; 32],
}

impl EncodedUaiCompoundOralResultState {
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn into_secret_value(self) -> SecretValue {
        self.value
    }
}

impl fmt::Debug for EncodedUaiCompoundOralResultState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedUaiCompoundOralResultState")
            .field("value", &"[REDACTED]")
            .field("digest", &"[HASHED]")
            .finish()
    }
}

/// Accepted compound-oral response authority rebound to one immutable
/// sequence and one exact Core mutation record.
pub struct UaiCompoundOralResultState {
    ordinal: u32,
    plan_digest: [u8; 32],
    artifact_digest: [u8; 32],
    plan_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    accepted_at: Timestamp,
    submission_version: Zeroizing<String>,
}

impl UaiCompoundOralResultState {
    /// Restores one accepted compound-oral successor only from Core's atomic
    /// receipt-plus-stage-output recovery record.
    ///
    /// # Errors
    ///
    /// Rejects missing encrypted successor bytes and any Provider, type,
    /// ordinal, issue, receipt, sequence or digest substitution.
    pub fn decode_recovery_record(
        sequence: &UaiCompoundOralSubmissionSequence,
        record: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<Self> {
        let output = record.stage_output().ok_or_else(foreign_result_state)?;
        if output.provider_id().as_str() != PROVIDER_ID
            || output.ordinal() != record.issue().ordinal()
            || output.output_type() != UAI_COMPOUND_ORAL_RESULT_STATE_TYPE
        {
            return Err(foreign_result_state());
        }
        Self::decode_bound(output.value(), output.output_digest(), sequence, record)
    }

    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn plan_digest(&self) -> [u8; 32] {
        self.plan_digest
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub const fn plan_binding_digest(&self) -> [u8; 32] {
        self.plan_binding_digest
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn accepted_at(&self) -> Timestamp {
        self.accepted_at
    }

    pub fn submission_version(&self) -> &str {
        &self.submission_version
    }

    /// Verifies the receipt-versioned two-module readback only when the
    /// independently recovered private plan carries the same exact issued
    /// request and semantic sequence identity.
    ///
    /// # Errors
    ///
    /// Rejects foreign plan state, accepted-result lineage or changed readback
    /// before producing ordinal-bound mutation verification.
    pub fn verify_plan_state_readback(
        &self,
        document: &str,
        plan_state: &UaiCompoundOralPlanState,
    ) -> ProviderResult<(UaiCompoundOralVerification, ExecutionMutationVerification)> {
        let sequence = UaiCompoundOralSubmissionSequence::try_new(plan_state.submission())?;
        if self.ordinal != UAI_COMPOUND_ORAL_ORDINAL
            || self.plan_digest != sequence.plan().plan_digest()
            || self.artifact_digest != sequence.artifact().artifact_digest()
            || self.plan_binding_digest != plan_state.submission().plan_binding_digest()?
            || self.request_digest != plan_state.request_digest()
        {
            return Err(foreign_result_state());
        }
        let verification = parse_compound_oral_verification(
            document,
            plan_state.submission(),
            &self.verification_receipt()?,
        )?;
        let mutation =
            ExecutionMutationVerification::new(self.ordinal, verification.result_digest(), true)?;
        Ok((verification, mutation))
    }

    fn verification_receipt(&self) -> ProviderResult<SubmissionReceipt> {
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some(
                "UAI accepted the compound oral submission for later verification".to_owned(),
            ),
            provider_trace_id: Some(self.submission_version.to_string()),
            received_at: self.accepted_at,
        };
        receipt.validate().map_err(|_| foreign_result_state())?;
        Ok(receipt)
    }

    /// Encodes deterministic bounded secret continuation bytes.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error when the state cannot be represented
    /// inside its bounded encrypted continuation.
    pub fn encode(&self) -> ProviderResult<EncodedUaiCompoundOralResultState> {
        let mut encoded = Zeroizing::new(
            serde_json::to_vec(&CompoundOralResultStateWireRef {
                schema: UAI_COMPOUND_ORAL_RESULT_STATE_TYPE,
                ordinal: self.ordinal,
                plan_digest: self.plan_digest,
                artifact_digest: self.artifact_digest,
                plan_binding_digest: self.plan_binding_digest,
                request_digest: self.request_digest,
                response_digest: self.response_digest,
                accepted_at_unix_seconds: self.accepted_at.timestamp(),
                accepted_at_subsec_nanos: self.accepted_at.timestamp_subsec_nanos(),
                submission_version: &self.submission_version,
            })
            .map_err(|_| invalid_result_state())?,
        );
        if encoded.is_empty() || encoded.len() > MAX_COMPOUND_ORAL_RESULT_STATE_BYTES {
            return Err(invalid_result_state());
        }
        let digest = Sha256::digest(encoded.as_slice()).into();
        let value = SecretValue::new(std::mem::take(&mut *encoded));
        Ok(EncodedUaiCompoundOralResultState { value, digest })
    }

    /// Decodes the encrypted state only after rebinding the complete sequence,
    /// issued request and accepted response identities.
    ///
    /// # Errors
    ///
    /// Rejects malformed, digest-mismatched, rejected or foreign state before
    /// exposing the accepted submission version.
    pub fn decode_bound(
        value: &SecretValue,
        expected_digest: [u8; 32],
        sequence: &UaiCompoundOralSubmissionSequence,
        record: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<Self> {
        let bytes = value.expose_secret();
        if bytes.is_empty()
            || bytes.len() > MAX_COMPOUND_ORAL_RESULT_STATE_BYTES
            || <[u8; 32]>::from(Sha256::digest(bytes)) != expected_digest
        {
            return Err(foreign_result_state());
        }
        let wire: CompoundOralResultStateWire =
            serde_json::from_slice(bytes).map_err(|_| foreign_result_state())?;
        let receipt = record.receipt().filter(|receipt| receipt.accepted());
        let Some(accepted_at) =
            Timestamp::from_timestamp(wire.accepted_at_unix_seconds, wire.accepted_at_subsec_nanos)
        else {
            return Err(foreign_result_state());
        };
        if wire.schema != UAI_COMPOUND_ORAL_RESULT_STATE_TYPE
            || wire.ordinal != UAI_COMPOUND_ORAL_ORDINAL
            || wire.ordinal != record.issue().ordinal()
            || record.issue().operation_type() != UAI_COMPOUND_ORAL_OPERATION_TYPE
            || record.issue().request_digest() != wire.request_digest
            || receipt.map(ExecutionMutationReceipt::response_digest) != Some(wire.response_digest)
            || wire.plan_digest != sequence.plan().plan_digest()
            || wire.artifact_digest != sequence.artifact().artifact_digest()
            || wire.plan_binding_digest != sequence.plan_binding_digest
            || [
                wire.plan_digest,
                wire.artifact_digest,
                wire.plan_binding_digest,
                wire.request_digest,
                wire.response_digest,
            ]
            .contains(&[0; 32])
            || !crate::submission_execute::valid_submission_version(&wire.submission_version)
        {
            return Err(foreign_result_state());
        }
        Ok(Self {
            ordinal: wire.ordinal,
            plan_digest: wire.plan_digest,
            artifact_digest: wire.artifact_digest,
            plan_binding_digest: wire.plan_binding_digest,
            request_digest: wire.request_digest,
            response_digest: wire.response_digest,
            accepted_at,
            submission_version: Zeroizing::new(wire.submission_version.clone()),
        })
    }
}

impl fmt::Debug for UaiCompoundOralResultState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralResultState")
            .field("ordinal", &self.ordinal)
            .field("plan_digest", &"[HASHED]")
            .field("artifact_digest", &"[HASHED]")
            .field("plan_binding_digest", &"[HASHED]")
            .field("request_digest", &"[HASHED]")
            .field("response_digest", &"[HASHED]")
            .field("accepted_at", &self.accepted_at)
            .field("submission_version", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiCompoundOralResultState {
    fn drop(&mut self) {
        self.plan_digest.zeroize();
        self.artifact_digest.zeroize();
        self.plan_binding_digest.zeroize();
        self.request_digest.zeroize();
        self.response_digest.zeroize();
    }
}

/// Accepted donor response rebound to one exact compound-oral request.
pub struct UaiCompoundOralSubmissionOutcome {
    plan_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    receipt: SubmissionReceipt,
}

impl UaiCompoundOralSubmissionOutcome {
    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn receipt(&self) -> &SubmissionReceipt {
        &self.receipt
    }

    fn matches(&self, plan_binding_digest: [u8; 32], request_digest: [u8; 32]) -> bool {
        self.plan_binding_digest == plan_binding_digest
            && self.request_digest == request_digest
            && self.response_digest != [0; 32]
            && self.receipt.validate().is_ok()
            && self.receipt.remote_status == "accepted"
    }
}

impl fmt::Debug for UaiCompoundOralSubmissionOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralSubmissionOutcome")
            .field("plan_binding_digest", &"[HASHED]")
            .field("request_digest", &"[HASHED]")
            .field("response_digest", &"[HASHED]")
            .field("receipt", &self.receipt)
            .finish()
    }
}

impl UaiCompoundOralSubmissionRequest {
    /// Classifies only the donor's strict accepted acknowledgement and binds
    /// it to this exact pre-issued request.
    ///
    /// # Errors
    ///
    /// Rejected, malformed, oversized or foreign acknowledgements remain
    /// errors and must not be persisted as receipts.
    pub fn classify_compound_oral_response(
        &self,
        document: &str,
        expected_course_instance_id: &str,
        expected_group_id: &str,
    ) -> ProviderResult<UaiCompoundOralSubmissionOutcome> {
        let receipt =
            parse_submission_receipt(document, expected_course_instance_id, expected_group_id)?;
        Ok(UaiCompoundOralSubmissionOutcome {
            plan_binding_digest: self.plan_binding_digest(),
            request_digest: self.request_digest(),
            response_digest: Sha256::digest(document.as_bytes()).into(),
            receipt,
        })
    }
}

#[derive(Serialize)]
struct CompoundOralResultStateWireRef<'a> {
    schema: &'static str,
    ordinal: u32,
    plan_digest: [u8; 32],
    artifact_digest: [u8; 32],
    plan_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    accepted_at_unix_seconds: i64,
    accepted_at_subsec_nanos: u32,
    submission_version: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct CompoundOralResultStateWire {
    schema: String,
    ordinal: u32,
    plan_digest: [u8; 32],
    artifact_digest: [u8; 32],
    plan_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    accepted_at_unix_seconds: i64,
    accepted_at_subsec_nanos: u32,
    submission_version: String,
}

fn invalid_sequence() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI compound oral single-occurrence sequence projection is invalid",
    )
}

fn invalid_result_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI compound oral accepted-result state is invalid",
    )
}

fn foreign_result_state() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI compound oral accepted-result state is stale or foreign",
    )
}

fn foreign_sequence_material() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI compound oral sequence material is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::SubmissionDraftId;
    use asterism_provider_api::ExecutionMutationSequencePlan;
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::build_compound_oral_submission_request;

    #[tokio::test]
    async fn single_occurrence_sequence_records_only_exact_accepted_response() {
        let submission = UaiCompoundOralSubmission::fixture(
            SubmissionDraftId::new(),
            "right",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission).unwrap();
        let phase = &sequence.plan().phases()[0];
        assert_eq!(phase.minimum_occurrences(), 1);
        assert_eq!(phase.maximum_occurrences(), 1);
        assert!(phase.stop_repeating_after_rejection());
        assert_eq!(
            phase.advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached
        );
        let artifact = serde_json::to_string(sequence.artifact().payload_sanitized()).unwrap();
        assert!(!artifact.contains("right"));
        assert!(!artifact.contains("spoken"));
        assert!(!artifact.contains("slot"));
        assert!(!artifact.contains("6001"));
        assert!(!artifact.contains("openid-1"));

        let request =
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let accepted = json!({
            "code": 0,
            "message": "synthetic accepted",
            "data": {
                "course_id": "course-instance-1",
                "group_id": "group-oral",
                "version": "compound-oral-v1"
            }
        })
        .to_string();
        let outcome = request
            .classify_compound_oral_response(&accepted, "course-instance-1", "group-oral")
            .unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        sequence.issue(&request, &sink).await.unwrap();
        sequence
            .record_accepted_outcome(&request, &outcome, &sink)
            .await
            .unwrap();
        assert_eq!(sink.receipts.lock().unwrap().len(), 1);
        assert_eq!(sink.stage_outputs.lock().unwrap().len(), 1);
        assert!(sequence.issue(&request, &sink).await.is_err());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the recovery regression keeps exact request, encrypted state, readback and foreign-cross-binding checks together"
    )]
    fn accepted_result_state_round_trips_and_verifies_only_the_exact_private_plan() {
        let submission = UaiCompoundOralSubmission::fixture(
            SubmissionDraftId::new(),
            "right",
            json!(["spoken"]),
            Some(json!({"slot": 1})),
        );
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission).unwrap();
        let request =
            build_compound_oral_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let accepted = json!({
            "code": 0,
            "message": "synthetic accepted",
            "data": {
                "course_id": "course-instance-1",
                "group_id": "group-oral",
                "version": "compound-oral-v1"
            }
        })
        .to_string();
        let outcome = request
            .classify_compound_oral_response(&accepted, "course-instance-1", "group-oral")
            .unwrap();
        let state = sequence.accepted_result_state(&request, &outcome).unwrap();
        assert_eq!(state.ordinal(), 1);
        assert_eq!(state.plan_digest(), sequence.plan().plan_digest());
        assert_eq!(
            state.artifact_digest(),
            sequence.artifact().artifact_digest()
        );
        assert_eq!(state.request_digest(), request.request_digest());
        assert_eq!(state.response_digest(), outcome.response_digest());
        assert_eq!(state.submission_version(), "compound-oral-v1");
        assert_eq!(state.accepted_at(), outcome.receipt().received_at);
        assert!(!format!("{state:?}").contains("compound-oral-v1"));

        let encoded = state.encode().unwrap();
        let digest = encoded.digest();
        assert!(!format!("{encoded:?}").contains("compound-oral-v1"));
        let value = encoded.into_secret_value();
        let record = ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(
                1,
                UAI_COMPOUND_ORAL_OPERATION_TYPE,
                request.request_digest(),
            )
            .unwrap(),
            Some(ExecutionMutationReceipt::new(1, outcome.response_digest(), true).unwrap()),
            None,
        )
        .unwrap()
        .try_with_stage_output(
            ExecutionMutationStageOutput::try_new(
                ProviderId::new(PROVIDER_ID).unwrap(),
                1,
                UAI_COMPOUND_ORAL_RESULT_STATE_TYPE,
                digest,
                SecretValue::new(value.expose_secret().to_vec()),
            )
            .unwrap(),
        )
        .unwrap();
        let decoded =
            UaiCompoundOralResultState::decode_bound(&value, digest, &sequence, &record).unwrap();
        assert_eq!(
            UaiCompoundOralResultState::decode_recovery_record(&sequence, &record)
                .unwrap()
                .submission_version(),
            "compound-oral-v1"
        );
        let foreign_output_record = ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(
                1,
                UAI_COMPOUND_ORAL_OPERATION_TYPE,
                request.request_digest(),
            )
            .unwrap(),
            Some(ExecutionMutationReceipt::new(1, outcome.response_digest(), true).unwrap()),
            None,
        )
        .unwrap()
        .try_with_stage_output(
            ExecutionMutationStageOutput::try_new(
                ProviderId::new("foreign").unwrap(),
                1,
                "foreign.compound-oral.result.v1",
                digest,
                SecretValue::new(value.expose_secret().to_vec()),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            UaiCompoundOralResultState::decode_recovery_record(&sequence, &foreign_output_record,)
                .is_err()
        );
        assert_eq!(decoded.submission_version(), "compound-oral-v1");

        let encoded_plan =
            crate::EncodedUaiCompoundOralPlanState::try_new(&submission, &request).unwrap();
        let plan_digest = encoded_plan.digest();
        let plan_value = encoded_plan.into_secret_value();
        let plan_state = crate::UaiCompoundOralPlanState::decode_bound(
            &plan_value,
            plan_digest,
            &sequence,
            request.request_digest(),
        )
        .unwrap();
        let document = compound_oral_verification_document(
            "right",
            &json!(["spoken"]),
            Some(json!({"slot": 1})),
            "compound-oral-v1",
        );
        let (verified, mutation) = decoded
            .verify_plan_state_readback(&document, &plan_state)
            .unwrap();
        assert_eq!(verified.submission_version(), "compound-oral-v1");
        assert_eq!(mutation.ordinal(), 1);
        assert_eq!(mutation.observation_digest(), verified.result_digest());
        assert!(mutation.verified());

        let foreign_request =
            build_compound_oral_submission_request(&submission, "course-instance-2", "openid-1")
                .unwrap();
        let foreign_plan =
            crate::EncodedUaiCompoundOralPlanState::try_new(&submission, &foreign_request).unwrap();
        let foreign_plan_digest = foreign_plan.digest();
        let foreign_plan_value = foreign_plan.into_secret_value();
        let foreign_plan = crate::UaiCompoundOralPlanState::decode_bound(
            &foreign_plan_value,
            foreign_plan_digest,
            &sequence,
            foreign_request.request_digest(),
        )
        .unwrap();
        assert!(
            decoded
                .verify_plan_state_readback(&document, &foreign_plan)
                .is_err()
        );
        let foreign_accepted = json!({
            "code": 0,
            "message": "synthetic accepted",
            "data": {
                "course_id": "course-instance-2",
                "group_id": "group-oral",
                "version": "compound-oral-v2"
            }
        })
        .to_string();
        let foreign_outcome = foreign_request
            .classify_compound_oral_response(&foreign_accepted, "course-instance-2", "group-oral")
            .unwrap();
        assert!(
            sequence
                .accepted_result_state(&request, &foreign_outcome)
                .is_err()
        );
        assert!(
            sequence
                .accepted_result_state(&foreign_request, &outcome)
                .is_err()
        );
        assert!(
            UaiCompoundOralResultState::decode_bound(&value, [7; 32], &sequence, &record).is_err()
        );
        let foreign_record = ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(
                1,
                UAI_COMPOUND_ORAL_OPERATION_TYPE,
                request.request_digest(),
            )
            .unwrap(),
            Some(ExecutionMutationReceipt::new(1, [8; 32], true).unwrap()),
            None,
        )
        .unwrap();
        assert!(
            UaiCompoundOralResultState::decode_bound(&value, digest, &sequence, &foreign_record,)
                .is_err()
        );
    }

    #[tokio::test]
    async fn issued_rejection_or_ambiguity_never_authorizes_replay() {
        for response in [
            r#"{"code":900001,"message":"rejected"}"#,
            r#"{"code":0,"data":{"course_id":"foreign","group_id":"group-oral","version":"v1"}}"#,
            "not-json",
        ] {
            let submission = UaiCompoundOralSubmission::fixture(
                SubmissionDraftId::new(),
                "right",
                json!("spoken"),
                None,
            );
            let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission).unwrap();
            let request = build_compound_oral_submission_request(
                &submission,
                "course-instance-1",
                "openid-1",
            )
            .unwrap();
            let sink = FixtureSequenceSink::default();
            sequence.prepare(&sink).await.unwrap();
            sequence.issue(&request, &sink).await.unwrap();
            assert!(
                request
                    .classify_compound_oral_response(response, "course-instance-1", "group-oral",)
                    .is_err()
            );
            assert!(sink.receipts.lock().unwrap().is_empty());
            assert!(sequence.issue(&request, &sink).await.is_err());
        }
    }

    #[tokio::test]
    async fn same_draft_id_answer_substitution_is_foreign_sequence_material() {
        let draft_id = SubmissionDraftId::new();
        let original = UaiCompoundOralSubmission::fixture(draft_id, "right", json!("spoken"), None);
        let substituted =
            UaiCompoundOralSubmission::fixture(draft_id, "changed", json!("spoken"), None);
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&original).unwrap();
        let foreign =
            build_compound_oral_submission_request(&substituted, "course-instance-1", "openid-1")
                .unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        assert!(sequence.issue(&foreign, &sink).await.is_err());
        assert!(sink.issues.lock().unwrap().is_empty());
    }

    fn compound_oral_verification_document(
        ordinary_answer: &str,
        oral_value: &serde_json::Value,
        oral_extra: Option<serde_json::Value>,
        version: &str,
    ) -> String {
        let ordinary = json!({
            "value": [],
            "children": [{"value": [ordinary_answer], "isDone": true}],
            "progress": {},
            "record": {"url": ""}
        })
        .to_string();
        let mut oral_child = json!({"value": oral_value, "isDone": true});
        if let Some(extra) = oral_extra {
            oral_child["extra"] = extra;
        }
        let oral = json!({
            "value": [],
            "children": [oral_child],
            "progress": {},
            "record": {"url": ""}
        })
        .to_string();
        let questions = json!([
            {
                "instanceId": "5001",
                "answer": ordinary,
                "context": "{\"state\":\"submitted\"}"
            },
            {
                "instanceId": "6001",
                "answer": oral,
                "context": "{\"state\":\"submitted\"}"
            }
        ])
        .to_string();
        json!({
            "success": true,
            "code": 0,
            "data": {
                "course": "course-instance-1",
                "module": format!("group-oral-{version}"),
                "state": {
                    "version": version,
                    "quesData": questions,
                    "__EXTEND_DATA__": {"__SUBMIT_INFO__": {
                        "course_id": "course-instance-1",
                        "group_id": "group-oral",
                        "version": version
                    }}
                }
            }
        })
        .to_string()
    }

    #[derive(Default)]
    struct FixtureSequenceSink {
        plan_digest: Mutex<Option<[u8; 32]>>,
        issues: Mutex<Vec<ExecutionMutationIssue>>,
        receipts: Mutex<Vec<ExecutionMutationReceipt>>,
        stage_outputs: Mutex<Vec<ExecutionMutationStageOutput>>,
    }

    #[async_trait]
    impl ExecutionMutationSink for FixtureSequenceSink {
        async fn prepare_sequence_plan(
            &self,
            plan: &ExecutionMutationSequencePlan,
        ) -> ProviderResult<()> {
            let mut stored = self.plan_digest.lock().unwrap();
            match *stored {
                Some(digest) if digest != plan.plan_digest() => Err(foreign_sequence_material()),
                Some(_) => Ok(()),
                None => {
                    *stored = Some(plan.plan_digest());
                    Ok(())
                }
            }
        }

        async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            let mut issues = self.issues.lock().unwrap();
            if !issues.is_empty() {
                return Err(foreign_sequence_material());
            }
            issues.push(issue.clone());
            Ok(())
        }

        async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            let issues = self.issues.lock().unwrap();
            if issues.len() != 1 || receipt.ordinal() != issues[0].ordinal() {
                return Err(foreign_sequence_material());
            }
            drop(issues);
            let mut receipts = self.receipts.lock().unwrap();
            if !receipts.is_empty() {
                return Err(foreign_sequence_material());
            }
            receipts.push(receipt);
            Ok(())
        }

        async fn record_receipt_with_stage_output(
            &self,
            receipt: ExecutionMutationReceipt,
            output: ExecutionMutationStageOutput,
        ) -> ProviderResult<()> {
            let issues = self.issues.lock().unwrap();
            if issues.len() != 1
                || receipt.ordinal() != issues[0].ordinal()
                || output.ordinal() != receipt.ordinal()
                || output.provider_id().as_str() != PROVIDER_ID
            {
                return Err(foreign_sequence_material());
            }
            drop(issues);
            let mut receipts = self.receipts.lock().unwrap();
            let mut outputs = self.stage_outputs.lock().unwrap();
            if !receipts.is_empty() || !outputs.is_empty() {
                return Err(foreign_sequence_material());
            }
            receipts.push(receipt);
            outputs.push(output);
            Ok(())
        }
    }
}
