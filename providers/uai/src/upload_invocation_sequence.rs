use std::fmt;

use asterism_domain::{ProviderId, SubmissionReceipt, Timestamp};
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
    ExecutionMutationSequenceAdvanceCondition, ExecutionMutationSequencePhase,
    ExecutionMutationSequencePlan, ExecutionMutationSequenceRecoverySnapshot,
    ExecutionMutationSink, ExecutionMutationStageOutput, ProviderError, ProviderErrorKind,
    ProviderExecutionPlanArtifact, ProviderResult,
};
use asterism_secrets::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    UAI_UPLOAD_OBJECT_OPERATION_TYPE, UaiUploadFinalSubmissionKind,
    UaiUploadFinalSubmissionOutcome, UaiUploadObjectState, metadata::PROVIDER_ID,
};

pub(crate) const UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE: &str =
    "uai.artifact-upload.invocation-plan.v1";
pub(crate) const UAI_UPLOAD_INVOCATION_SEQUENCE_TYPE: &str =
    "uai.artifact-upload.invocation-sequence.v1";
pub(crate) const UAI_UPLOAD_INVOCATION_FINAL_OPERATION_TYPE: &str =
    "uai.artifact-upload.final-submit";
pub(crate) const UAI_UPLOAD_INVOCATION_FINAL_RESULT_TYPE: &str =
    "uai.artifact-upload.final-result.v1";

const MAX_FINAL_RESULT_BYTES: usize = 16 * 1_024;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct UaiUploadInvocationSequence {
    artifact: ProviderExecutionPlanArtifact,
    plan: ExecutionMutationSequencePlan,
}

impl UaiUploadInvocationSequence {
    pub(crate) fn try_new(artifact: ProviderExecutionPlanArtifact) -> ProviderResult<Self> {
        if artifact.provider_id().as_str() != PROVIDER_ID
            || artifact.artifact_type() != UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE
        {
            return Err(foreign_sequence());
        }
        let object = ExecutionMutationSequencePhase::try_new(
            UAI_UPLOAD_OBJECT_OPERATION_TYPE,
            1,
            1,
            true,
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
            None,
        )?;
        let final_submit = ExecutionMutationSequencePhase::try_new(
            UAI_UPLOAD_INVOCATION_FINAL_OPERATION_TYPE,
            1,
            2,
            false,
            ExecutionMutationSequenceAdvanceCondition::AcceptedOrMaximumReached,
            None,
        )?;
        let plan = ExecutionMutationSequencePlan::try_new(
            artifact.artifact_digest(),
            UAI_UPLOAD_INVOCATION_SEQUENCE_TYPE,
            vec![object, final_submit],
        )?;
        Ok(Self { artifact, plan })
    }

    pub(crate) async fn prepare(
        &self,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        sink.prepare_sequence_plan(&self.plan).await
    }

    pub(crate) async fn issue_object(
        &self,
        request_digest: [u8; 32],
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        sink.issue(&ExecutionMutationIssue::new(
            1,
            UAI_UPLOAD_OBJECT_OPERATION_TYPE,
            request_digest,
        )?)
        .await
    }

    pub(crate) async fn issue_final(
        &self,
        attempt: u32,
        request_digest: [u8; 32],
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<u32> {
        let ordinal = attempt
            .checked_add(1)
            .filter(|value| (2..=3).contains(value))
            .ok_or_else(foreign_sequence)?;
        sink.issue(&ExecutionMutationIssue::new(
            ordinal,
            UAI_UPLOAD_INVOCATION_FINAL_OPERATION_TYPE,
            request_digest,
        )?)
        .await?;
        Ok(ordinal)
    }

    pub(crate) async fn record_final(
        &self,
        ordinal: u32,
        kind: UaiUploadFinalSubmissionKind,
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
        outcome: &UaiUploadFinalSubmissionOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<bool> {
        let attempt = ordinal.checked_sub(1).ok_or_else(foreign_sequence)?;
        if !(2..=3).contains(&ordinal)
            || sequence_binding_digest == [0; 32]
            || request_digest == [0; 32]
            || !outcome.matches(attempt, kind, sequence_binding_digest, request_digest)
        {
            return Err(foreign_sequence());
        }
        if outcome.accepted() {
            let receipt = outcome.receipt().ok_or_else(foreign_sequence)?;
            receipt.validate().map_err(|_| foreign_sequence())?;
            let state = UaiUploadInvocationFinalResult::try_new(
                ordinal,
                outcome.kind(),
                outcome.request_digest(),
                outcome.response_digest(),
                receipt,
            )?;
            let encoded = state.encode()?;
            let output = ExecutionMutationStageOutput::try_new(
                ProviderId::new(PROVIDER_ID).map_err(|_| invalid_sequence())?,
                ordinal,
                UAI_UPLOAD_INVOCATION_FINAL_RESULT_TYPE,
                encoded.digest,
                encoded.value,
            )?;
            sink.record_receipt_with_stage_output(
                ExecutionMutationReceipt::new(ordinal, outcome.response_digest(), true)?,
                output,
            )
            .await?;
            Ok(true)
        } else {
            sink.record_receipt(ExecutionMutationReceipt::new_retryable_rejection(
                ordinal,
                outcome.response_digest(),
                outcome.retry_after_seconds().ok_or_else(foreign_sequence)?,
            )?)
            .await?;
            Ok(false)
        }
    }

    pub(crate) fn inspect_completed(
        &self,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<(UaiUploadObjectState, UaiUploadInvocationFinalResult)> {
        if snapshot.artifact() != &self.artifact
            || snapshot.plan() != &self.plan
            || !snapshot.observations().is_empty()
            || !(2..=3).contains(&snapshot.records().len())
        {
            return Err(foreign_sequence());
        }
        let object = UaiUploadObjectState::decode_recovery_record(&snapshot.records()[0])?;
        let final_record = snapshot.records().last().ok_or_else(foreign_sequence)?;
        let final_result = UaiUploadInvocationFinalResult::decode_record(final_record)?;
        if final_result.ordinal != final_record.issue().ordinal() {
            return Err(foreign_sequence());
        }
        Ok((object, final_result))
    }
}

impl fmt::Debug for UaiUploadInvocationSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadInvocationSequence")
            .field("artifact", &self.artifact)
            .field("plan", &self.plan)
            .finish()
    }
}

pub(crate) struct UaiUploadInvocationFinalResult {
    ordinal: u32,
    kind: UaiUploadFinalSubmissionKind,
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    accepted_at: Timestamp,
    submission_version: Zeroizing<String>,
}

impl UaiUploadInvocationFinalResult {
    fn try_new(
        ordinal: u32,
        kind: UaiUploadFinalSubmissionKind,
        request_digest: [u8; 32],
        response_digest: [u8; 32],
        receipt: &SubmissionReceipt,
    ) -> ProviderResult<Self> {
        let version = receipt
            .provider_trace_id
            .as_deref()
            .filter(|version| {
                receipt.remote_status == "accepted"
                    && crate::submission_execute::valid_submission_version(version)
            })
            .ok_or_else(foreign_sequence)?;
        if !(2..=3).contains(&ordinal) || request_digest == [0; 32] || response_digest == [0; 32] {
            return Err(foreign_sequence());
        }
        Ok(Self {
            ordinal,
            kind,
            request_digest,
            response_digest,
            accepted_at: receipt.received_at,
            submission_version: Zeroizing::new(version.to_owned()),
        })
    }

    pub(crate) const fn kind(&self) -> UaiUploadFinalSubmissionKind {
        self.kind
    }

    pub(crate) const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub(crate) fn receipt(&self) -> ProviderResult<SubmissionReceipt> {
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some("UAI accepted the private artifact invocation".to_owned()),
            provider_trace_id: Some(self.submission_version.to_string()),
            received_at: self.accepted_at,
        };
        receipt.validate().map_err(|_| foreign_sequence())?;
        Ok(receipt)
    }

    fn encode(&self) -> ProviderResult<EncodedFinalResult> {
        let wire = FinalResultWireRef {
            schema: UAI_UPLOAD_INVOCATION_FINAL_RESULT_TYPE,
            ordinal: self.ordinal,
            kind: match self.kind {
                UaiUploadFinalSubmissionKind::Single => "single",
                UaiUploadFinalSubmissionKind::Compound => "compound",
            },
            request_digest: self.request_digest,
            response_digest: self.response_digest,
            accepted_at_seconds: self.accepted_at.timestamp(),
            accepted_at_nanos: self.accepted_at.timestamp_subsec_nanos(),
            submission_version: self.submission_version.as_str(),
        };
        let mut bytes = Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| invalid_sequence())?);
        if bytes.is_empty() || bytes.len() > MAX_FINAL_RESULT_BYTES {
            return Err(invalid_sequence());
        }
        let digest = Sha256::digest(bytes.as_slice()).into();
        Ok(EncodedFinalResult {
            value: SecretValue::new(std::mem::take(&mut *bytes)),
            digest,
        })
    }

    fn decode_record(record: &ExecutionMutationRecoveryRecord) -> ProviderResult<Self> {
        let receipt = record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_sequence)?;
        let output = record.stage_output().ok_or_else(foreign_sequence)?;
        if record.issue().operation_type() != UAI_UPLOAD_INVOCATION_FINAL_OPERATION_TYPE
            || record.issue().request_digest() == [0; 32]
            || output.provider_id().as_str() != PROVIDER_ID
            || output.ordinal() != record.issue().ordinal()
            || output.output_type() != UAI_UPLOAD_INVOCATION_FINAL_RESULT_TYPE
            || output.output_digest()
                != <[u8; 32]>::from(Sha256::digest(output.value().expose_secret()))
        {
            return Err(foreign_sequence());
        }
        let mut wire: FinalResultWire = serde_json::from_slice(output.value().expose_secret())
            .map_err(|_| foreign_sequence())?;
        let accepted_at =
            Timestamp::from_timestamp(wire.accepted_at_seconds, wire.accepted_at_nanos)
                .ok_or_else(foreign_sequence)?;
        let kind = match wire.kind.as_str() {
            "single" => UaiUploadFinalSubmissionKind::Single,
            "compound" => UaiUploadFinalSubmissionKind::Compound,
            _ => return Err(foreign_sequence()),
        };
        if wire.schema != UAI_UPLOAD_INVOCATION_FINAL_RESULT_TYPE
            || wire.ordinal != record.issue().ordinal()
            || wire.request_digest != record.issue().request_digest()
            || wire.response_digest != receipt.response_digest()
        {
            return Err(foreign_sequence());
        }
        let submission_version = Zeroizing::new(std::mem::take(&mut wire.submission_version));
        let result = Self {
            ordinal: wire.ordinal,
            kind,
            request_digest: wire.request_digest,
            response_digest: wire.response_digest,
            accepted_at,
            submission_version,
        };
        result.receipt()?;
        Ok(result)
    }
}

impl fmt::Debug for UaiUploadInvocationFinalResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadInvocationFinalResult")
            .field("ordinal", &self.ordinal)
            .field("kind", &self.kind)
            .field("request_digest", &"[HASHED]")
            .field("response_digest", &"[HASHED]")
            .field("accepted_at", &self.accepted_at)
            .field("submission_version", &"[REDACTED]")
            .finish()
    }
}

struct EncodedFinalResult {
    value: SecretValue,
    digest: [u8; 32],
}

#[derive(Serialize)]
struct FinalResultWireRef<'a> {
    schema: &'static str,
    ordinal: u32,
    kind: &'static str,
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    accepted_at_seconds: i64,
    accepted_at_nanos: u32,
    submission_version: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct FinalResultWire {
    schema: String,
    ordinal: u32,
    kind: String,
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    accepted_at_seconds: i64,
    accepted_at_nanos: u32,
    submission_version: String,
}

fn invalid_sequence() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI artifact invocation sequence is invalid",
    )
}

fn foreign_sequence() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI artifact invocation sequence evidence is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn integrated_sequence_binds_final_result_to_the_exact_issued_request() {
        let artifact = ProviderExecutionPlanArtifact::try_new(
            ProviderId::new(PROVIDER_ID).unwrap(),
            UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE,
            serde_json::json!({
                "schema": UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE,
                "submission_kind": "single",
            }),
        )
        .unwrap();
        let sequence = UaiUploadInvocationSequence::try_new(artifact).unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        sequence.issue_object([1; 32], &sink).await.unwrap();
        sink.record_receipt(ExecutionMutationReceipt::new(1, [2; 32], true).unwrap())
            .await
            .unwrap();
        assert_eq!(sequence.issue_final(1, [3; 32], &sink).await.unwrap(), 2);

        let binding = [4; 32];
        let foreign = accepted_outcome(binding, [5; 32]);
        assert!(
            sequence
                .record_final(
                    2,
                    UaiUploadFinalSubmissionKind::Single,
                    binding,
                    [3; 32],
                    &foreign,
                    &sink,
                )
                .await
                .is_err()
        );
        assert_eq!(sink.snapshot(), (2, 1, 0));

        let accepted = accepted_outcome(binding, [3; 32]);
        assert!(
            sequence
                .record_final(
                    2,
                    UaiUploadFinalSubmissionKind::Single,
                    binding,
                    [3; 32],
                    &accepted,
                    &sink,
                )
                .await
                .unwrap()
        );
        assert_eq!(sink.snapshot(), (2, 2, 1));
    }

    fn accepted_outcome(
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
    ) -> UaiUploadFinalSubmissionOutcome {
        UaiUploadFinalSubmissionOutcome::Accepted {
            ordinal: 1,
            kind: UaiUploadFinalSubmissionKind::Single,
            sequence_binding_digest,
            request_digest,
            response_digest: [6; 32],
            receipt: SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some("accepted".to_owned()),
                provider_trace_id: Some("submission-version-1".to_owned()),
                received_at: Utc::now(),
            },
        }
    }

    #[derive(Default)]
    struct FixtureSequenceSink {
        state: Mutex<FixtureSequenceState>,
    }

    #[derive(Default)]
    struct FixtureSequenceState {
        prepared: bool,
        issues: Vec<ExecutionMutationIssue>,
        receipts: Vec<ExecutionMutationReceipt>,
        outputs: Vec<ExecutionMutationStageOutput>,
    }

    impl FixtureSequenceSink {
        fn snapshot(&self) -> (usize, usize, usize) {
            let state = self.state.lock().unwrap();
            (
                state.issues.len(),
                state.receipts.len(),
                state.outputs.len(),
            )
        }
    }

    #[async_trait]
    impl ExecutionMutationSink for FixtureSequenceSink {
        async fn prepare_sequence_plan(
            &self,
            plan: &ExecutionMutationSequencePlan,
        ) -> ProviderResult<()> {
            if plan.sequence_type() != UAI_UPLOAD_INVOCATION_SEQUENCE_TYPE {
                return Err(foreign_sequence());
            }
            self.state.lock().unwrap().prepared = true;
            Ok(())
        }

        async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            if !state.prepared
                || state.issues.len() != state.receipts.len()
                || usize::try_from(issue.ordinal()).ok() != Some(state.issues.len() + 1)
            {
                return Err(foreign_sequence());
            }
            state.issues.push(issue.clone());
            Ok(())
        }

        async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            if state.issues.len() != state.receipts.len() + 1
                || state.issues.last().map(ExecutionMutationIssue::ordinal)
                    != Some(receipt.ordinal())
            {
                return Err(foreign_sequence());
            }
            state.receipts.push(receipt);
            Ok(())
        }

        async fn record_receipt_with_stage_output(
            &self,
            receipt: ExecutionMutationReceipt,
            output: ExecutionMutationStageOutput,
        ) -> ProviderResult<()> {
            if !receipt.accepted()
                || output.ordinal() != receipt.ordinal()
                || output.output_type() != UAI_UPLOAD_INVOCATION_FINAL_RESULT_TYPE
            {
                return Err(foreign_sequence());
            }
            self.record_receipt(receipt).await?;
            self.state.lock().unwrap().outputs.push(output);
            Ok(())
        }
    }
}
