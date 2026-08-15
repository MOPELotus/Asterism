use std::fmt;

use asterism_domain::{ProviderId, SubmissionReceipt};
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationSequenceAdvanceCondition,
    ExecutionMutationSequencePhase, ExecutionMutationSequencePlan, ExecutionMutationSink,
    ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact, ProviderResult,
};
use sha2::{Digest, Sha256};

use crate::{
    UaiCompoundUploadSubmission, UaiCompoundUploadSubmissionRequest, UaiUploadSubmission,
    UaiUploadSubmissionRequest, metadata::PROVIDER_ID, parse_submission_receipt,
};

pub const UAI_UPLOAD_FINAL_PLAN_ARTIFACT_TYPE: &str = "uai.upload.final-plan.v1";
pub const UAI_UPLOAD_FINAL_SEQUENCE_TYPE: &str = "uai.upload.final-submit.v1";
pub const UAI_UPLOAD_FINAL_OPERATION_TYPE: &str = "uai.upload.final-submit";
pub const UAI_UPLOAD_FINAL_MAXIMUM_ATTEMPTS: u32 = 2;
pub const UAI_UPLOAD_FINAL_RETRY_SECONDS: u64 = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UaiUploadFinalSubmissionKind {
    Single,
    Compound,
}

impl UaiUploadFinalSubmissionKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Compound => "compound",
        }
    }
}

/// Immutable Core projection and bounded retry sequence for one exact final
/// upload submission.
///
/// The projection deliberately contains no object key or selected answer.
/// Recovery still requires the separately encrypted Provider final-plan state.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiUploadFinalSubmissionSequence {
    kind: UaiUploadFinalSubmissionKind,
    sequence_binding_digest: [u8; 32],
    artifact: ProviderExecutionPlanArtifact,
    plan: ExecutionMutationSequencePlan,
}

impl UaiUploadFinalSubmissionSequence {
    /// Builds the accepted-short-circuit sequence for one exact single upload.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the opaque Provider plan can no longer be
    /// projected into Core's bounded scheduling contracts.
    pub fn for_single(submission: &UaiUploadSubmission) -> ProviderResult<Self> {
        Self::try_new(
            UaiUploadFinalSubmissionKind::Single,
            submission.remote_task_id(),
            submission.course_publish_version(),
            None,
            submission.final_sequence_binding_digest(),
        )
    }

    /// Builds the accepted-short-circuit sequence for one exact compound upload.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the opaque Provider plan can no longer be
    /// projected into Core's bounded scheduling contracts.
    pub fn for_compound(submission: &UaiCompoundUploadSubmission) -> ProviderResult<Self> {
        let ordinary_draft_id = submission.ordinary_draft_id().to_string();
        Self::try_new(
            UaiUploadFinalSubmissionKind::Compound,
            submission.remote_task_id(),
            submission.course_publish_version(),
            Some(&ordinary_draft_id),
            submission.final_sequence_binding_digest(),
        )
    }

    fn try_new(
        kind: UaiUploadFinalSubmissionKind,
        remote_task_id: &str,
        course_publish_version: u64,
        ordinary_draft_id: Option<&str>,
        sequence_binding_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        if sequence_binding_digest == [0; 32] {
            return Err(invalid_sequence());
        }
        let artifact = ProviderExecutionPlanArtifact::try_new(
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_sequence())?,
            UAI_UPLOAD_FINAL_PLAN_ARTIFACT_TYPE,
            serde_json::json!({
                "schema": UAI_UPLOAD_FINAL_PLAN_ARTIFACT_TYPE,
                "submission_kind": kind.as_str(),
                "remote_task_id": remote_task_id,
                "course_publish_version": course_publish_version,
                "ordinary_draft_id": ordinary_draft_id,
                "sequence_binding_digest": hex_digest(sequence_binding_digest),
            }),
        )
        .map_err(|_| invalid_sequence())?;
        let phase = ExecutionMutationSequencePhase::try_new(
            UAI_UPLOAD_FINAL_OPERATION_TYPE,
            1,
            UAI_UPLOAD_FINAL_MAXIMUM_ATTEMPTS,
            false,
            ExecutionMutationSequenceAdvanceCondition::AcceptedOrMaximumReached,
            None,
        )
        .map_err(|_| invalid_sequence())?;
        let plan = ExecutionMutationSequencePlan::try_new(
            artifact.artifact_digest(),
            UAI_UPLOAD_FINAL_SEQUENCE_TYPE,
            vec![phase],
        )
        .map_err(|_| invalid_sequence())?;
        Ok(Self {
            kind,
            sequence_binding_digest,
            artifact,
            plan,
        })
    }

    pub const fn kind(&self) -> UaiUploadFinalSubmissionKind {
        self.kind
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

    /// Freezes the exact final-submit phase machine before request issuance.
    ///
    /// # Errors
    ///
    /// Returns a storage or sequence conflict if the sink cannot atomically
    /// register this exact plan.
    pub async fn prepare(
        &self,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        sink.prepare_sequence_plan(&self.plan).await
    }

    /// Persists one exact single-upload request identity before dispatch.
    ///
    /// # Errors
    ///
    /// Rejects foreign request material or a sink-side sequence conflict.
    pub async fn issue_single(
        &self,
        ordinal: u32,
        request: &UaiUploadSubmissionRequest,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_request(
            UaiUploadFinalSubmissionKind::Single,
            request.sequence_binding_digest(),
        )?;
        issue_request(ordinal, request.request_digest(), sink).await
    }

    /// Persists one exact compound-upload request identity before dispatch.
    ///
    /// # Errors
    ///
    /// Rejects foreign request material or a sink-side sequence conflict.
    pub async fn issue_compound(
        &self,
        ordinal: u32,
        request: &UaiCompoundUploadSubmissionRequest,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_request(
            UaiUploadFinalSubmissionKind::Compound,
            request.sequence_binding_digest(),
        )?;
        issue_request(ordinal, request.request_digest(), sink).await
    }

    /// Persists one definite single-upload outcome after the response parser
    /// has rebound it to the exact issued request.
    ///
    /// # Errors
    ///
    /// Rejects foreign request/outcome material or a sink-side receipt
    /// conflict.
    pub async fn record_single_outcome(
        &self,
        ordinal: u32,
        request: &UaiUploadSubmissionRequest,
        outcome: &UaiUploadFinalSubmissionOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.record_outcome(
            UaiUploadFinalSubmissionKind::Single,
            ordinal,
            request.sequence_binding_digest(),
            request.request_digest(),
            outcome,
            sink,
        )
        .await
    }

    /// Persists one definite compound-upload outcome after the response parser
    /// has rebound it to the exact issued request.
    ///
    /// # Errors
    ///
    /// Rejects foreign request/outcome material or a sink-side receipt
    /// conflict.
    pub async fn record_compound_outcome(
        &self,
        ordinal: u32,
        request: &UaiCompoundUploadSubmissionRequest,
        outcome: &UaiUploadFinalSubmissionOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.record_outcome(
            UaiUploadFinalSubmissionKind::Compound,
            ordinal,
            request.sequence_binding_digest(),
            request.request_digest(),
            outcome,
            sink,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the sequence, issued request and parsed response are independent authorities"
    )]
    async fn record_outcome(
        &self,
        kind: UaiUploadFinalSubmissionKind,
        ordinal: u32,
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
        outcome: &UaiUploadFinalSubmissionOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_request(kind, sequence_binding_digest)?;
        if !outcome.matches(ordinal, kind, sequence_binding_digest, request_digest) {
            return Err(foreign_sequence_material());
        }
        sink.record_receipt(ExecutionMutationReceipt::new(
            ordinal,
            outcome.response_digest(),
            outcome.accepted(),
        )?)
        .await
    }

    fn validate_request(
        &self,
        kind: UaiUploadFinalSubmissionKind,
        sequence_binding_digest: [u8; 32],
    ) -> ProviderResult<()> {
        if self.kind != kind || self.sequence_binding_digest != sequence_binding_digest {
            Err(foreign_sequence_material())
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for UaiUploadFinalSubmissionSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadFinalSubmissionSequence")
            .field("kind", &self.kind)
            .field("sequence_binding_digest", &"[HASHED]")
            .field("artifact", &self.artifact)
            .field("plan", &self.plan)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UaiUploadFinalRetryCode {
    SubmissionFrequency,
    AccountFrequency,
}

impl UaiUploadFinalRetryCode {
    pub const fn provider_code(self) -> &'static str {
        match self {
            Self::SubmissionFrequency => "600001",
            Self::AccountFrequency => "600002",
        }
    }
}

/// Definite parsed result for one exact issued final-upload request.
///
/// Other remote rejection codes and every ambiguous transport/response failure
/// remain errors, so they cannot produce the receipt needed for another ordinal.
pub enum UaiUploadFinalSubmissionOutcome {
    Accepted {
        ordinal: u32,
        kind: UaiUploadFinalSubmissionKind,
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
        response_digest: [u8; 32],
        receipt: SubmissionReceipt,
    },
    RetryableRejected {
        ordinal: u32,
        kind: UaiUploadFinalSubmissionKind,
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
        response_digest: [u8; 32],
        code: UaiUploadFinalRetryCode,
    },
}

impl UaiUploadFinalSubmissionOutcome {
    pub const fn accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        match self {
            Self::Accepted {
                response_digest, ..
            }
            | Self::RetryableRejected {
                response_digest, ..
            } => *response_digest,
        }
    }

    pub const fn retry_code(&self) -> Option<UaiUploadFinalRetryCode> {
        match self {
            Self::Accepted { .. } => None,
            Self::RetryableRejected { code, .. } => Some(*code),
        }
    }

    pub const fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::Accepted { .. } => None,
            Self::RetryableRejected { .. } => Some(UAI_UPLOAD_FINAL_RETRY_SECONDS),
        }
    }

    pub const fn receipt(&self) -> Option<&SubmissionReceipt> {
        match self {
            Self::Accepted { receipt, .. } => Some(receipt),
            Self::RetryableRejected { .. } => None,
        }
    }

    fn matches(
        &self,
        ordinal: u32,
        kind: UaiUploadFinalSubmissionKind,
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
    ) -> bool {
        let (actual_ordinal, actual_kind, actual_binding, actual_request) = match self {
            Self::Accepted {
                ordinal,
                kind,
                sequence_binding_digest,
                request_digest,
                ..
            }
            | Self::RetryableRejected {
                ordinal,
                kind,
                sequence_binding_digest,
                request_digest,
                ..
            } => (*ordinal, *kind, *sequence_binding_digest, *request_digest),
        };
        actual_ordinal == ordinal
            && actual_kind == kind
            && actual_binding == sequence_binding_digest
            && actual_request == request_digest
    }

    pub(crate) fn into_legacy_result(self) -> ProviderResult<SubmissionReceipt> {
        match self {
            Self::Accepted { receipt, .. } => Ok(receipt),
            Self::RetryableRejected { code, .. } => Err(retry_error(code)),
        }
    }
}

impl fmt::Debug for UaiUploadFinalSubmissionOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted {
                ordinal, receipt, ..
            } => formatter
                .debug_struct("UaiUploadFinalSubmissionOutcome::Accepted")
                .field("ordinal", ordinal)
                .field("binding", &"[REDACTED]")
                .field("response_digest", &"[HASHED]")
                .field("receipt", receipt)
                .finish(),
            Self::RetryableRejected { ordinal, code, .. } => formatter
                .debug_struct("UaiUploadFinalSubmissionOutcome::RetryableRejected")
                .field("ordinal", ordinal)
                .field("binding", &"[REDACTED]")
                .field("response_digest", &"[HASHED]")
                .field("code", code)
                .finish(),
        }
    }
}

impl UaiUploadSubmissionRequest {
    /// Classifies only success and the two donor-proven definite retry codes.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, foreign or otherwise non-retryable
    /// responses without constructing a mutation receipt.
    pub fn classify_final_response(
        &self,
        ordinal: u32,
        document: &str,
        expected_course_instance_id: &str,
        expected_group_id: &str,
    ) -> ProviderResult<UaiUploadFinalSubmissionOutcome> {
        classify_response(
            ordinal,
            UaiUploadFinalSubmissionKind::Single,
            self.sequence_binding_digest(),
            self.request_digest(),
            document,
            expected_course_instance_id,
            expected_group_id,
        )
    }
}

impl UaiCompoundUploadSubmissionRequest {
    /// Classifies only success and the two donor-proven definite retry codes.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, foreign or otherwise non-retryable
    /// responses without constructing a mutation receipt.
    pub fn classify_final_response(
        &self,
        ordinal: u32,
        document: &str,
        expected_course_instance_id: &str,
        expected_group_id: &str,
    ) -> ProviderResult<UaiUploadFinalSubmissionOutcome> {
        classify_response(
            ordinal,
            UaiUploadFinalSubmissionKind::Compound,
            self.sequence_binding_digest(),
            self.request_digest(),
            document,
            expected_course_instance_id,
            expected_group_id,
        )
    }
}

fn classify_response(
    ordinal: u32,
    kind: UaiUploadFinalSubmissionKind,
    sequence_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    document: &str,
    expected_course_instance_id: &str,
    expected_group_id: &str,
) -> ProviderResult<UaiUploadFinalSubmissionOutcome> {
    if !(1..=UAI_UPLOAD_FINAL_MAXIMUM_ATTEMPTS).contains(&ordinal) {
        return Err(foreign_sequence_material());
    }
    match parse_submission_receipt(document, expected_course_instance_id, expected_group_id) {
        Ok(receipt) => {
            let response_digest = Sha256::digest(document.as_bytes()).into();
            Ok(UaiUploadFinalSubmissionOutcome::Accepted {
                ordinal,
                kind,
                sequence_binding_digest,
                request_digest,
                response_digest,
                receipt,
            })
        }
        Err(error) => match retry_code(&error) {
            Some(code) => {
                let response_digest = Sha256::digest(document.as_bytes()).into();
                Ok(UaiUploadFinalSubmissionOutcome::RetryableRejected {
                    ordinal,
                    kind,
                    sequence_binding_digest,
                    request_digest,
                    response_digest,
                    code,
                })
            }
            None => Err(error),
        },
    }
}

fn retry_code(error: &ProviderError) -> Option<UaiUploadFinalRetryCode> {
    if error.kind != ProviderErrorKind::RateLimited
        || error.retry_after_seconds != Some(UAI_UPLOAD_FINAL_RETRY_SECONDS)
    {
        return None;
    }
    match error.provider_code.as_deref() {
        Some("600001") => Some(UaiUploadFinalRetryCode::SubmissionFrequency),
        Some("600002") => Some(UaiUploadFinalRetryCode::AccountFrequency),
        _ => None,
    }
}

fn retry_error(code: UaiUploadFinalRetryCode) -> ProviderError {
    let mut error = ProviderError::new(
        ProviderErrorKind::RateLimited,
        "UAI rate limited the final upload submission mutation",
    );
    error.provider_code = Some(code.provider_code().to_owned());
    error.retry_after_seconds = Some(UAI_UPLOAD_FINAL_RETRY_SECONDS);
    error
}

async fn issue_request(
    ordinal: u32,
    request_digest: [u8; 32],
    sink: &(dyn ExecutionMutationSink + Send + Sync),
) -> ProviderResult<()> {
    let issue =
        ExecutionMutationIssue::new(ordinal, UAI_UPLOAD_FINAL_OPERATION_TYPE, request_digest)?;
    sink.issue(&issue).await
}

fn hex_digest(digest: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn invalid_sequence() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI final upload retry sequence projection is invalid",
    )
}

fn foreign_sequence_material() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "UAI final upload retry sequence received foreign plan material",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;

    const ACCEPTED: &str = r#"{"code":0,"data":{"course_id":"course-instance-1","group_id":"group-upload","version":"upload-v1"}}"#;

    #[test]
    fn final_sequence_is_artifact_bound_bounded_and_acceptance_short_circuited() {
        let submission = UaiUploadSubmission::fixture("course/42/nothing.mp3", "fixture-a");
        let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
        assert_eq!(sequence.kind(), UaiUploadFinalSubmissionKind::Single);
        assert_eq!(
            sequence.artifact().artifact_type(),
            UAI_UPLOAD_FINAL_PLAN_ARTIFACT_TYPE
        );
        assert_eq!(sequence.artifact().provider_id().as_str(), PROVIDER_ID);
        assert_eq!(
            sequence.plan().artifact_digest(),
            sequence.artifact().artifact_digest()
        );
        assert_eq!(
            sequence.plan().sequence_type(),
            UAI_UPLOAD_FINAL_SEQUENCE_TYPE
        );
        assert_eq!(sequence.plan().phases().len(), 1);
        let phase = &sequence.plan().phases()[0];
        assert_eq!(phase.operation_type(), UAI_UPLOAD_FINAL_OPERATION_TYPE);
        assert_eq!(phase.minimum_occurrences(), 1);
        assert_eq!(phase.maximum_occurrences(), 2);
        assert!(!phase.stop_repeating_after_rejection());
        assert_eq!(
            phase.advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::AcceptedOrMaximumReached
        );
        let artifact = serde_json::to_string(sequence.artifact().payload_sanitized()).unwrap();
        assert!(!artifact.contains("course/42/nothing.mp3"));
        assert!(!format!("{sequence:?}").contains("course/42/nothing.mp3"));
    }

    #[tokio::test]
    async fn first_accepted_response_records_one_receipt_and_blocks_a_second_issue() {
        let submission = UaiUploadSubmission::fixture("course/42/nothing.mp3", "fixture-a");
        let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
        let request =
            crate::build_upload_submission_request(&submission, "course-instance-1", "openid-1")
                .unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        sequence.issue_single(1, &request, &sink).await.unwrap();
        let outcome = request
            .classify_final_response(1, ACCEPTED, "course-instance-1", "group-upload")
            .unwrap();
        assert!(outcome.accepted());
        assert!(outcome.receipt().is_some());
        sequence
            .record_single_outcome(1, &request, &outcome, &sink)
            .await
            .unwrap();
        assert!(sequence.issue_single(2, &request, &sink).await.is_err());
        assert_eq!(sink.snapshot(), (1, 1, vec![true]));
    }

    #[tokio::test]
    async fn only_exact_rate_rejection_opens_the_second_ordinal() {
        for (code, expected) in [
            (600_001, UaiUploadFinalRetryCode::SubmissionFrequency),
            (600_002, UaiUploadFinalRetryCode::AccountFrequency),
        ] {
            let submission = UaiUploadSubmission::fixture("course/42/nothing.mp3", "fixture-a");
            let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
            let request = crate::build_upload_submission_request(
                &submission,
                "course-instance-1",
                "openid-1",
            )
            .unwrap();
            let sink = FixtureSequenceSink::default();
            sequence.prepare(&sink).await.unwrap();
            sequence.issue_single(1, &request, &sink).await.unwrap();
            let outcome = request
                .classify_final_response(
                    1,
                    &format!(r#"{{"code":{code},"msg":"retry"}}"#),
                    "course-instance-1",
                    "group-upload",
                )
                .unwrap();
            assert!(!outcome.accepted());
            assert_eq!(outcome.retry_code(), Some(expected));
            assert_eq!(outcome.retry_after_seconds(), Some(120));
            sequence
                .record_single_outcome(1, &request, &outcome, &sink)
                .await
                .unwrap();
            sequence.issue_single(2, &request, &sink).await.unwrap();
            assert!(
                sequence
                    .record_single_outcome(2, &request, &outcome, &sink)
                    .await
                    .is_err()
            );
            let accepted = request
                .classify_final_response(2, ACCEPTED, "course-instance-1", "group-upload")
                .unwrap();
            sequence
                .record_single_outcome(2, &request, &accepted, &sink)
                .await
                .unwrap();
            assert_eq!(sink.snapshot(), (2, 2, vec![false, true]));
        }
    }

    #[tokio::test]
    async fn ambiguous_and_other_rejections_record_no_receipt_and_stay_locked() {
        for document in [
            r#"{"code":42,"msg":"rejected"}"#,
            r#"{"code":"600001"}"#,
            "not-json",
        ] {
            let submission = UaiUploadSubmission::fixture("course/42/nothing.mp3", "fixture-a");
            let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
            let request = crate::build_upload_submission_request(
                &submission,
                "course-instance-1",
                "openid-1",
            )
            .unwrap();
            let sink = FixtureSequenceSink::default();
            sequence.prepare(&sink).await.unwrap();
            sequence.issue_single(1, &request, &sink).await.unwrap();
            assert!(
                request
                    .classify_final_response(1, document, "course-instance-1", "group-upload")
                    .is_err()
            );
            assert!(sequence.issue_single(2, &request, &sink).await.is_err());
            assert_eq!(sink.snapshot(), (1, 0, Vec::new()));
        }
    }

    #[tokio::test]
    async fn sequence_rejects_a_request_from_another_valid_submission() {
        let submission = UaiUploadSubmission::fixture("course/42/nothing.mp3", "fixture-a");
        let foreign = UaiUploadSubmission::fixture("course/42/other.mp3", "fixture-b");
        let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
        let request =
            crate::build_upload_submission_request(&foreign, "course-instance-1", "openid-1")
                .unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        assert!(sequence.issue_single(1, &request, &sink).await.is_err());
        assert_eq!(sink.snapshot(), (0, 0, Vec::new()));
    }

    #[derive(Default)]
    struct FixtureSequenceSink {
        state: Mutex<FixtureSequenceState>,
    }

    #[derive(Default)]
    struct FixtureSequenceState {
        prepared: bool,
        issues: Vec<(u32, [u8; 32])>,
        receipts: Vec<(u32, bool)>,
    }

    impl FixtureSequenceSink {
        fn snapshot(&self) -> (usize, usize, Vec<bool>) {
            let state = self.state.lock().unwrap();
            (
                state.issues.len(),
                state.receipts.len(),
                state.receipts.iter().map(|receipt| receipt.1).collect(),
            )
        }
    }

    #[async_trait]
    impl ExecutionMutationSink for FixtureSequenceSink {
        async fn prepare_sequence_plan(
            &self,
            plan: &ExecutionMutationSequencePlan,
        ) -> ProviderResult<()> {
            if plan.sequence_type() != UAI_UPLOAD_FINAL_SEQUENCE_TYPE {
                return Err(invalid_sequence());
            }
            self.state.lock().unwrap().prepared = true;
            Ok(())
        }

        async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            let valid = state.prepared
                && state.issues.len() == state.receipts.len()
                && state.issues.len() < UAI_UPLOAD_FINAL_MAXIMUM_ATTEMPTS as usize
                && !state.receipts.iter().any(|receipt| receipt.1)
                && usize::try_from(issue.ordinal()).ok() == Some(state.issues.len() + 1)
                && issue.operation_type() == UAI_UPLOAD_FINAL_OPERATION_TYPE;
            if !valid {
                return Err(foreign_sequence_material());
            }
            state.issues.push((issue.ordinal(), issue.request_digest()));
            Ok(())
        }

        async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            let valid = state.issues.len() == state.receipts.len() + 1
                && state.issues.last().map(|issue| issue.0) == Some(receipt.ordinal());
            if !valid {
                return Err(foreign_sequence_material());
            }
            state.receipts.push((receipt.ordinal(), receipt.accepted()));
            Ok(())
        }
    }
}
