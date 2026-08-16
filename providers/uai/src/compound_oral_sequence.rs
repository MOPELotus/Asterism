use std::fmt;

use asterism_domain::SubmissionReceipt;
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationSequenceAdvanceCondition,
    ExecutionMutationSequencePhase, ExecutionMutationSequencePlan, ExecutionMutationSink,
    ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact, ProviderResult,
};
use sha2::{Digest, Sha256};

use crate::{
    UaiCompoundOralSubmission, UaiCompoundOralSubmissionRequest, parse_submission_receipt,
};

pub const UAI_COMPOUND_ORAL_SEQUENCE_TYPE: &str = "uai.compound-oral.submit.v1";
pub const UAI_COMPOUND_ORAL_OPERATION_TYPE: &str = "uai.compound-oral.submit";
pub const UAI_COMPOUND_ORAL_MAXIMUM_ATTEMPTS: u32 = 1;

const UAI_COMPOUND_ORAL_ORDINAL: u32 = 1;

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
        sink.record_receipt(ExecutionMutationReceipt::new(
            UAI_COMPOUND_ORAL_ORDINAL,
            outcome.response_digest,
            true,
        )?)
        .await
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

fn invalid_sequence() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "UAI compound oral single-occurrence sequence projection is invalid",
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
        assert!(sequence.issue(&request, &sink).await.is_err());
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

    #[derive(Default)]
    struct FixtureSequenceSink {
        plan_digest: Mutex<Option<[u8; 32]>>,
        issues: Mutex<Vec<ExecutionMutationIssue>>,
        receipts: Mutex<Vec<ExecutionMutationReceipt>>,
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
    }
}
