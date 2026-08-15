use std::fmt;

use asterism_domain::{ProviderId, SubmissionReceipt};
use asterism_provider_api::{
    ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
    ExecutionMutationSequenceAdvanceCondition, ExecutionMutationSequenceObservation,
    ExecutionMutationSequencePhase, ExecutionMutationSequencePlan,
    ExecutionMutationSequenceRecoverySnapshot, ExecutionMutationSink,
    ExecutionMutationVerification, ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact,
    ProviderResult,
};
use sha2::{Digest, Sha256};

use crate::{
    UaiDiscussionCompletionPlan, UaiDiscussionReplyDraft, metadata::PROVIDER_ID,
    parse_discussion_reply_receipt, parse_submission_receipt,
};

pub const UAI_DISCUSSION_PLAN_ARTIFACT_TYPE: &str = "uai.discussion.reply-plan.v1";
pub const UAI_DISCUSSION_SEQUENCE_TYPE: &str = "uai.discussion.reply-complete.v1";
pub const UAI_DISCUSSION_REPLY_OPERATION_TYPE: &str = "uai.discussion.reply-submit";
pub const UAI_DISCUSSION_COMPLETION_OPERATION_TYPE: &str = "uai.discussion.complete-submit";
pub const UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE: &str = "uai.discussion.reply-readback.v1";

const UAI_DISCUSSION_REPLY_ORDINAL: u32 = 1;
const UAI_DISCUSSION_COMPLETION_ORDINAL: u32 = 2;
const UAI_DISCUSSION_COMPLETION_PHASE_POSITION: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UaiDiscussionMutationKind {
    Reply,
    Completion,
}

/// Sanitized Core projection of one exact reply/readback/completion workflow.
///
/// The artifact contains no reply text, account identity or mutable wire body.
/// The Provider-private reply Draft remains necessary to perform the first
/// mutation and to derive the independently verified completion plan.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiDiscussionMutationSequence {
    course_resource_id: String,
    group_id: String,
    topic_id: u64,
    reply_request_digest: [u8; 32],
    artifact: ProviderExecutionPlanArtifact,
    plan: ExecutionMutationSequencePlan,
}

impl UaiDiscussionMutationSequence {
    /// Projects one immutable reply Draft into Core's two-mutation sequence.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the exact Provider intent cannot be
    /// represented by the bounded Core artifact and sequence contracts.
    pub fn try_new(draft: &UaiDiscussionReplyDraft) -> ProviderResult<Self> {
        let reply_request_digest = draft.request_digest();
        if reply_request_digest == [0; 32] {
            return Err(invalid_sequence());
        }
        let artifact = ProviderExecutionPlanArtifact::try_new(
            ProviderId::new(PROVIDER_ID).map_err(|_| invalid_sequence())?,
            UAI_DISCUSSION_PLAN_ARTIFACT_TYPE,
            serde_json::json!({
                "schema": UAI_DISCUSSION_PLAN_ARTIFACT_TYPE,
                "course_resource_id": draft.binding().course_resource_id(),
                "group_id": draft.binding().group_id(),
                "topic_id": draft.topic_id(),
                "reply_request_digest": hex_digest(reply_request_digest),
            }),
        )
        .map_err(|_| invalid_sequence())?;
        let reply = ExecutionMutationSequencePhase::try_new(
            UAI_DISCUSSION_REPLY_OPERATION_TYPE,
            1,
            1,
            true,
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
            None,
        )
        .map_err(|_| invalid_sequence())?;
        let completion = ExecutionMutationSequencePhase::try_new(
            UAI_DISCUSSION_COMPLETION_OPERATION_TYPE,
            1,
            1,
            true,
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached,
            Some(UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE.to_owned()),
        )
        .map_err(|_| invalid_sequence())?;
        let plan = ExecutionMutationSequencePlan::try_new(
            artifact.artifact_digest(),
            UAI_DISCUSSION_SEQUENCE_TYPE,
            vec![reply, completion],
        )
        .map_err(|_| invalid_sequence())?;
        Ok(Self {
            course_resource_id: draft.binding().course_resource_id().to_owned(),
            group_id: draft.binding().group_id().to_owned(),
            topic_id: draft.topic_id(),
            reply_request_digest,
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

    /// Freezes the exact two-phase machine before the reply mutation.
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

    /// Persists the exact reply identity before dispatch.
    ///
    /// # Errors
    ///
    /// Rejects a foreign Draft or a sink-side sequence conflict.
    pub async fn issue_reply(
        &self,
        draft: &UaiDiscussionReplyDraft,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_draft(draft)?;
        sink.issue(&ExecutionMutationIssue::new(
            UAI_DISCUSSION_REPLY_ORDINAL,
            UAI_DISCUSSION_REPLY_OPERATION_TYPE,
            draft.request_digest(),
        )?)
        .await
    }

    /// Persists one definite accepted reply response. Errors and ambiguous
    /// transport outcomes must not call this method.
    ///
    /// # Errors
    ///
    /// Rejects foreign response material or a sink-side receipt conflict.
    pub async fn record_reply_outcome(
        &self,
        draft: &UaiDiscussionReplyDraft,
        outcome: &UaiDiscussionMutationOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_draft(draft)?;
        self.record_outcome(
            UaiDiscussionMutationKind::Reply,
            UAI_DISCUSSION_REPLY_ORDINAL,
            draft.request_digest(),
            outcome,
            sink,
        )
        .await
    }

    /// Persists the independent reply readback as both verification of the
    /// first mutation and the exact gate for phase two.
    ///
    /// Persisting verification before the phase observation makes an
    /// interruption fail closed before completion issuance. Recovery uses the
    /// snapshot-specific observation method instead of rewriting verification.
    ///
    /// # Errors
    ///
    /// Rejects a completion plan derived from another reply intent or any
    /// sink-side verification/observation conflict.
    pub async fn record_reply_readback(
        &self,
        completion: &UaiDiscussionCompletionPlan,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<UaiDiscussionReplyReadbackGate> {
        let gate = self.readback_gate(completion)?;
        sink.record_verification(ExecutionMutationVerification::new(
            UAI_DISCUSSION_REPLY_ORDINAL,
            gate.observation,
            true,
        )?)
        .await?;
        sink.record_sequence_observation(ExecutionMutationSequenceObservation::try_new(
            UAI_DISCUSSION_COMPLETION_PHASE_POSITION,
            UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE,
            gate.observation,
        )?)
        .await?;
        Ok(gate)
    }

    /// Persists the separately bound completion request after validating the
    /// exact typed readback gate returned by the durable observation step.
    ///
    /// # Errors
    ///
    /// Rejects a foreign completion plan, missing/changed readback evidence or
    /// a sink-side issue conflict.
    pub async fn issue_completion(
        &self,
        completion: &UaiDiscussionCompletionPlan,
        gate: &UaiDiscussionReplyReadbackGate,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_readback_gate(completion, gate)?;
        sink.issue(&ExecutionMutationIssue::new(
            UAI_DISCUSSION_COMPLETION_ORDINAL,
            UAI_DISCUSSION_COMPLETION_OPERATION_TYPE,
            completion.request_digest(),
        )?)
        .await
    }

    /// Completes an interrupted readback persistence boundary where Core has
    /// the exact verified ordinal-one digest but not its phase-two observation.
    /// It records no mutation and returns a gate only after the missing
    /// observation is durable.
    ///
    /// # Errors
    ///
    /// Rejects every recovery posture except the exact verification-without-
    /// gate state, or any foreign dynamic completion plan/sink conflict.
    pub async fn finish_recovered_reply_readback_gate(
        &self,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
        completion: &UaiDiscussionCompletionPlan,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<UaiDiscussionReplyReadbackGate> {
        if self.inspect_recovery(snapshot, Some(completion))?
            != UaiDiscussionRecoveryState::ReplyReadbackVerifiedAwaitingGate
        {
            return Err(foreign_sequence_material());
        }
        let gate = self.readback_gate(completion)?;
        sink.record_sequence_observation(ExecutionMutationSequenceObservation::try_new(
            UAI_DISCUSSION_COMPLETION_PHASE_POSITION,
            UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE,
            gate.observation,
        )?)
        .await?;
        Ok(gate)
    }

    /// Restores the typed readback gate only when the exact Core snapshot
    /// already contains it and phase two has not been issued.
    ///
    /// # Errors
    ///
    /// Rejects foreign evidence and every state other than the durable
    /// readback-gate boundary. It never writes or issues a mutation.
    pub fn recover_reply_readback_gate(
        &self,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
        completion: &UaiDiscussionCompletionPlan,
    ) -> ProviderResult<UaiDiscussionReplyReadbackGate> {
        if self.inspect_recovery(snapshot, Some(completion))?
            != UaiDiscussionRecoveryState::ReplyReadbackGateRecorded
        {
            return Err(foreign_sequence_material());
        }
        self.readback_gate(completion)
    }

    /// Persists one definite accepted completion response. Fresh Group
    /// progress remains the separate final completion authority.
    ///
    /// # Errors
    ///
    /// Rejects foreign response material or a sink-side receipt conflict.
    pub async fn record_completion_outcome(
        &self,
        completion: &UaiDiscussionCompletionPlan,
        outcome: &UaiDiscussionMutationOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        self.validate_completion_plan(completion)?;
        self.record_outcome(
            UaiDiscussionMutationKind::Completion,
            UAI_DISCUSSION_COMPLETION_ORDINAL,
            completion.request_digest(),
            outcome,
            sink,
        )
        .await
    }

    /// Validates one Core-loaded snapshot and reports only its read-only
    /// recovery posture. No returned state authorizes a mutation replay.
    ///
    /// A completion plan is required once durable evidence contains the reply
    /// readback or any phase-two issue, because the sanitized scheduling
    /// artifact deliberately cannot reconstruct reply content or dynamic Task
    /// state.
    ///
    /// # Errors
    ///
    /// Rejects foreign artifacts/plans, request-digest drift, rejected or
    /// otherwise unsupported receipts, observation drift and impossible
    /// verification/issue ordering.
    pub fn inspect_recovery(
        &self,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
        completion: Option<&UaiDiscussionCompletionPlan>,
    ) -> ProviderResult<UaiDiscussionRecoveryState> {
        if snapshot.artifact() != &self.artifact || snapshot.plan() != &self.plan {
            return Err(foreign_sequence_material());
        }
        if let Some(completion) = completion {
            self.validate_completion_plan(completion)?;
        }
        let records = snapshot.records();
        let observations = snapshot.observations();
        if records.is_empty() {
            return if observations.is_empty() {
                Ok(UaiDiscussionRecoveryState::NoMutationEvidence)
            } else {
                Err(foreign_sequence_material())
            };
        }
        if records.len() > 2 {
            return Err(foreign_sequence_material());
        }
        let reply = &records[0];
        validate_issue(
            reply,
            UAI_DISCUSSION_REPLY_ORDINAL,
            UAI_DISCUSSION_REPLY_OPERATION_TYPE,
            self.reply_request_digest,
        )?;
        let Some(reply_receipt) = reply.receipt() else {
            return if records.len() == 1
                && reply.verification().is_none()
                && observations.is_empty()
            {
                Ok(UaiDiscussionRecoveryState::ReplyIssuedAmbiguous)
            } else {
                Err(foreign_sequence_material())
            };
        };
        if !reply_receipt.accepted() {
            return Err(foreign_sequence_material());
        }
        let Some(reply_verification) = reply.verification() else {
            return if records.len() == 1 && observations.is_empty() {
                Ok(UaiDiscussionRecoveryState::ReplyAcceptedAwaitingReadback)
            } else {
                Err(foreign_sequence_material())
            };
        };
        let completion = completion.ok_or_else(foreign_sequence_material)?;
        let readback_digest = self.validate_completion_plan(completion)?;
        if !reply_verification.verified()
            || reply_verification.observation_digest() != readback_digest
        {
            return Err(foreign_sequence_material());
        }
        if observations.is_empty() {
            return if records.len() == 1 {
                Ok(UaiDiscussionRecoveryState::ReplyReadbackVerifiedAwaitingGate)
            } else {
                Err(foreign_sequence_material())
            };
        }
        if observations.len() != 1 || !observation_matches(&observations[0], readback_digest) {
            return Err(foreign_sequence_material());
        }
        if records.len() == 1 {
            return Ok(UaiDiscussionRecoveryState::ReplyReadbackGateRecorded);
        }
        let completion_record = &records[1];
        validate_issue(
            completion_record,
            UAI_DISCUSSION_COMPLETION_ORDINAL,
            UAI_DISCUSSION_COMPLETION_OPERATION_TYPE,
            completion.request_digest(),
        )?;
        if completion_record.verification().is_some() {
            return Err(foreign_sequence_material());
        }
        match completion_record.receipt() {
            None => Ok(UaiDiscussionRecoveryState::CompletionIssuedAmbiguous),
            Some(receipt) if receipt.accepted() => {
                Ok(UaiDiscussionRecoveryState::CompletionAcceptedAwaitingProgress)
            }
            Some(_) => Err(foreign_sequence_material()),
        }
    }

    async fn record_outcome(
        &self,
        kind: UaiDiscussionMutationKind,
        ordinal: u32,
        request_digest: [u8; 32],
        outcome: &UaiDiscussionMutationOutcome,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<()> {
        if !outcome.matches(kind, ordinal, self.reply_request_digest, request_digest) {
            return Err(foreign_sequence_material());
        }
        sink.record_receipt(ExecutionMutationReceipt::new(
            ordinal,
            outcome.response_digest(),
            true,
        )?)
        .await
    }

    fn validate_draft(&self, draft: &UaiDiscussionReplyDraft) -> ProviderResult<()> {
        if draft.binding().course_resource_id() != self.course_resource_id
            || draft.binding().group_id() != self.group_id
            || draft.topic_id() != self.topic_id
            || draft.request_digest() != self.reply_request_digest
        {
            Err(foreign_sequence_material())
        } else {
            Ok(())
        }
    }

    fn validate_completion_plan(
        &self,
        completion: &UaiDiscussionCompletionPlan,
    ) -> ProviderResult<[u8; 32]> {
        if completion.course_resource_id() != self.course_resource_id
            || completion.group_id() != self.group_id
            || completion.topic_id() != self.topic_id
            || completion.reply_request_digest() != self.reply_request_digest
            || completion.request_digest() == [0; 32]
            || completion.reply_digest() == [0; 32]
        {
            return Err(foreign_sequence_material());
        }
        let mut digest = Sha256::new();
        digest.update(b"asterism:uai:discussion-reply-readback:v1\0");
        digest.update(self.reply_request_digest);
        digest.update(completion.reply_digest());
        digest.update(completion.request_digest());
        let digest = digest.finalize().into();
        if digest == [0; 32] {
            Err(invalid_sequence())
        } else {
            Ok(digest)
        }
    }

    fn readback_gate(
        &self,
        completion: &UaiDiscussionCompletionPlan,
    ) -> ProviderResult<UaiDiscussionReplyReadbackGate> {
        Ok(UaiDiscussionReplyReadbackGate {
            sequence_binding: self.reply_request_digest,
            completion_request: completion.request_digest(),
            observation: self.validate_completion_plan(completion)?,
        })
    }

    fn validate_readback_gate(
        &self,
        completion: &UaiDiscussionCompletionPlan,
        gate: &UaiDiscussionReplyReadbackGate,
    ) -> ProviderResult<()> {
        let expected = self.readback_gate(completion)?;
        if *gate == expected {
            Ok(())
        } else {
            Err(foreign_sequence_material())
        }
    }
}

impl fmt::Debug for UaiDiscussionMutationSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionMutationSequence")
            .field("course_resource_id", &self.course_resource_id)
            .field("group_id", &self.group_id)
            .field("topic_id", &self.topic_id)
            .field("reply_request_digest", &"[HASHED]")
            .field("artifact", &self.artifact)
            .field("plan", &self.plan)
            .finish()
    }
}

/// Read-only interpretation of one exact Core recovery snapshot.
///
/// These values describe durable evidence only. They never authorize replay;
/// both ambiguous states require read-only diagnosis, and final acceptance
/// still requires fresh exact Group progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UaiDiscussionRecoveryState {
    NoMutationEvidence,
    ReplyIssuedAmbiguous,
    ReplyAcceptedAwaitingReadback,
    ReplyReadbackVerifiedAwaitingGate,
    ReplyReadbackGateRecorded,
    CompletionIssuedAmbiguous,
    CompletionAcceptedAwaitingProgress,
}

/// Exact durable authority connecting one verified reply readback to the
/// separately issued completion request. Only sequence methods can construct
/// it, and it carries hashes rather than reply or account material.
#[derive(Clone, Eq, PartialEq)]
pub struct UaiDiscussionReplyReadbackGate {
    sequence_binding: [u8; 32],
    completion_request: [u8; 32],
    observation: [u8; 32],
}

impl UaiDiscussionReplyReadbackGate {
    pub const fn observation_digest(&self) -> [u8; 32] {
        self.observation
    }
}

impl fmt::Debug for UaiDiscussionReplyReadbackGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionReplyReadbackGate")
            .field("sequence_binding", &"[HASHED]")
            .field("completion_request", &"[HASHED]")
            .field("observation", &"[HASHED]")
            .finish()
    }
}

/// Definite accepted response bound to one exact discussion mutation.
pub struct UaiDiscussionMutationOutcome {
    kind: UaiDiscussionMutationKind,
    ordinal: u32,
    sequence_binding_digest: [u8; 32],
    request_digest: [u8; 32],
    response_digest: [u8; 32],
    receipt: SubmissionReceipt,
}

impl UaiDiscussionMutationOutcome {
    pub const fn kind(&self) -> UaiDiscussionMutationKind {
        self.kind
    }

    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn receipt(&self) -> &SubmissionReceipt {
        &self.receipt
    }

    fn matches(
        &self,
        kind: UaiDiscussionMutationKind,
        ordinal: u32,
        sequence_binding_digest: [u8; 32],
        request_digest: [u8; 32],
    ) -> bool {
        self.kind == kind
            && self.ordinal == ordinal
            && self.sequence_binding_digest == sequence_binding_digest
            && self.request_digest == request_digest
    }

    pub(crate) fn into_legacy_result(self) -> SubmissionReceipt {
        self.receipt
    }
}

impl fmt::Debug for UaiDiscussionMutationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionMutationOutcome")
            .field("kind", &self.kind)
            .field("ordinal", &self.ordinal)
            .field("sequence_binding_digest", &"[HASHED]")
            .field("request_digest", &"[HASHED]")
            .field("response_digest", &"[HASHED]")
            .field("receipt", &self.receipt)
            .finish()
    }
}

impl UaiDiscussionReplyDraft {
    /// Parses one definite reply acknowledgement and binds it to ordinal one.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized or non-accepted responses without
    /// constructing sequence receipt material.
    pub fn classify_reply_mutation_response(
        &self,
        document: &str,
    ) -> ProviderResult<UaiDiscussionMutationOutcome> {
        let receipt = parse_discussion_reply_receipt(document)?;
        Ok(UaiDiscussionMutationOutcome {
            kind: UaiDiscussionMutationKind::Reply,
            ordinal: UAI_DISCUSSION_REPLY_ORDINAL,
            sequence_binding_digest: self.request_digest(),
            request_digest: self.request_digest(),
            response_digest: Sha256::digest(document.as_bytes()).into(),
            receipt,
        })
    }
}

impl UaiDiscussionCompletionPlan {
    /// Parses one definite Group-completion acknowledgement and binds it to
    /// ordinal two. It remains a receipt, not completion proof.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, throttled, non-accepted or identity-
    /// drifted responses without constructing sequence receipt material.
    pub fn classify_completion_mutation_response(
        &self,
        document: &str,
        expected_course_instance_id: &str,
    ) -> ProviderResult<UaiDiscussionMutationOutcome> {
        let receipt =
            parse_submission_receipt(document, expected_course_instance_id, self.group_id())?;
        Ok(UaiDiscussionMutationOutcome {
            kind: UaiDiscussionMutationKind::Completion,
            ordinal: UAI_DISCUSSION_COMPLETION_ORDINAL,
            sequence_binding_digest: self.reply_request_digest(),
            request_digest: self.request_digest(),
            response_digest: Sha256::digest(document.as_bytes()).into(),
            receipt,
        })
    }
}

fn validate_issue(
    record: &ExecutionMutationRecoveryRecord,
    ordinal: u32,
    operation_type: &str,
    request_digest: [u8; 32],
) -> ProviderResult<()> {
    let issue = record.issue();
    if issue.ordinal() != ordinal
        || issue.operation_type() != operation_type
        || issue.request_digest() != request_digest
    {
        Err(foreign_sequence_material())
    } else {
        Ok(())
    }
}

fn observation_matches(
    observation: &ExecutionMutationSequenceObservation,
    digest: [u8; 32],
) -> bool {
    observation.phase_position() == UAI_DISCUSSION_COMPLETION_PHASE_POSITION
        && observation.observation_type() == UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE
        && observation.observation_digest() == digest
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
        "UAI discussion mutation sequence projection is invalid",
    )
}

fn foreign_sequence_material() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "UAI discussion mutation sequence received foreign durable material",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{AssessmentClass, RemoteState, SourceType};
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use async_trait::async_trait;

    use super::*;
    use crate::{UaiDiscussionBinding, parse_discussion_reply_page, prepare_discussion_completion};

    const REPLY_ACCEPTED: &str = r#"{"code":200,"success":true,"value":{"replyId":"reply-9"}}"#;
    const COMPLETION_ACCEPTED: &str = r#"{"code":0,"data":{"course_id":"course-instance-1","group_id":"group-1","version":"discussion-v1"}}"#;

    #[test]
    fn projection_is_redacted_and_requires_the_reply_readback_gate() {
        let (draft, completion) = material("private reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        assert_eq!(
            sequence.artifact().artifact_type(),
            UAI_DISCUSSION_PLAN_ARTIFACT_TYPE
        );
        assert_eq!(sequence.artifact().provider_id().as_str(), PROVIDER_ID);
        assert_eq!(
            sequence.plan().artifact_digest(),
            sequence.artifact().artifact_digest()
        );
        assert_eq!(
            sequence.plan().sequence_type(),
            UAI_DISCUSSION_SEQUENCE_TYPE
        );
        assert_eq!(sequence.plan().phases().len(), 2);
        let reply = &sequence.plan().phases()[0];
        assert_eq!(reply.operation_type(), UAI_DISCUSSION_REPLY_OPERATION_TYPE);
        assert_eq!(
            (reply.minimum_occurrences(), reply.maximum_occurrences()),
            (1, 1)
        );
        assert!(reply.stop_repeating_after_rejection());
        assert_eq!(
            reply.advance_condition(),
            ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached
        );
        assert_eq!(reply.required_observation_type(), None);
        let final_submit = &sequence.plan().phases()[1];
        assert_eq!(
            final_submit.operation_type(),
            UAI_DISCUSSION_COMPLETION_OPERATION_TYPE
        );
        assert_eq!(
            final_submit.required_observation_type(),
            Some(UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE)
        );
        let artifact = serde_json::to_string(sequence.artifact().payload_sanitized()).unwrap();
        assert!(!artifact.contains("private reply"));
        assert!(!artifact.contains("user-9"));
        assert!(!artifact.contains("course-instance-1"));
        assert!(!format!("{sequence:?}").contains("private reply"));
        assert_ne!(completion.reply_request_digest(), [0; 32]);
    }

    #[tokio::test]
    async fn accepted_reply_readback_and_completion_are_independent_records() {
        let (draft, completion) = material("private reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        sequence.issue_reply(&draft, &sink).await.unwrap();
        let reply = draft
            .classify_reply_mutation_response(REPLY_ACCEPTED)
            .unwrap();
        assert_eq!(reply.kind(), UaiDiscussionMutationKind::Reply);
        assert_eq!(reply.ordinal(), 1);
        sequence
            .record_reply_outcome(&draft, &reply, &sink)
            .await
            .unwrap();
        let gate = sequence
            .record_reply_readback(&completion, &sink)
            .await
            .unwrap();
        sequence
            .issue_completion(&completion, &gate, &sink)
            .await
            .unwrap();
        let completed = completion
            .classify_completion_mutation_response(COMPLETION_ACCEPTED, "course-instance-1")
            .unwrap();
        assert_eq!(completed.kind(), UaiDiscussionMutationKind::Completion);
        assert_eq!(completed.ordinal(), 2);
        sequence
            .record_completion_outcome(&completion, &completed, &sink)
            .await
            .unwrap();

        let state = sink.state.lock().unwrap();
        assert_eq!(state.issues.len(), 2);
        assert_eq!(state.receipts.len(), 2);
        assert_eq!(state.verifications.len(), 1);
        assert_eq!(state.observations.len(), 1);
        assert_eq!(state.issues[0].request_digest(), draft.request_digest());
        assert_eq!(
            state.issues[1].request_digest(),
            completion.request_digest()
        );
        assert_ne!(
            state.receipts[0].response_digest(),
            state.receipts[1].response_digest()
        );
        assert_eq!(
            state.verifications[0].observation_digest(),
            state.observations[0].observation_digest()
        );
    }

    #[tokio::test]
    async fn ambiguous_reply_or_completion_never_opens_a_replay() {
        let (draft, completion) = material("private reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let reply_sink = FixtureSequenceSink::default();
        sequence.prepare(&reply_sink).await.unwrap();
        sequence.issue_reply(&draft, &reply_sink).await.unwrap();
        assert!(
            draft
                .classify_reply_mutation_response(r#"{"code":500,"success":false}"#)
                .is_err()
        );
        assert!(sequence.issue_reply(&draft, &reply_sink).await.is_err());
        assert!(
            sequence
                .record_reply_readback(&completion, &reply_sink)
                .await
                .is_err()
        );
        assert_eq!(
            sequence
                .inspect_recovery(&reply_sink.snapshot(&sequence), None)
                .unwrap(),
            UaiDiscussionRecoveryState::ReplyIssuedAmbiguous
        );

        let completion_sink = FixtureSequenceSink::default();
        sequence.prepare(&completion_sink).await.unwrap();
        sequence
            .issue_reply(&draft, &completion_sink)
            .await
            .unwrap();
        let reply = draft
            .classify_reply_mutation_response(REPLY_ACCEPTED)
            .unwrap();
        sequence
            .record_reply_outcome(&draft, &reply, &completion_sink)
            .await
            .unwrap();
        let gate = sequence
            .record_reply_readback(&completion, &completion_sink)
            .await
            .unwrap();
        sequence
            .issue_completion(&completion, &gate, &completion_sink)
            .await
            .unwrap();
        assert!(
            completion
                .classify_completion_mutation_response("not-json", "course-instance-1")
                .is_err()
        );
        assert!(
            sequence
                .issue_completion(&completion, &gate, &completion_sink)
                .await
                .is_err()
        );
        assert_eq!(
            sequence
                .inspect_recovery(&completion_sink.snapshot(&sequence), Some(&completion),)
                .unwrap(),
            UaiDiscussionRecoveryState::CompletionIssuedAmbiguous
        );
    }

    #[tokio::test]
    async fn foreign_drafts_plans_and_outcomes_fail_before_persistence() {
        let (draft, completion) = material("private reply");
        let (foreign_draft, foreign_completion) = material("different reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        assert!(sequence.issue_reply(&foreign_draft, &sink).await.is_err());
        sequence.issue_reply(&draft, &sink).await.unwrap();
        let foreign_reply = foreign_draft
            .classify_reply_mutation_response(REPLY_ACCEPTED)
            .unwrap();
        assert!(
            sequence
                .record_reply_outcome(&draft, &foreign_reply, &sink)
                .await
                .is_err()
        );
        let reply = draft
            .classify_reply_mutation_response(REPLY_ACCEPTED)
            .unwrap();
        sequence
            .record_reply_outcome(&draft, &reply, &sink)
            .await
            .unwrap();
        let gate = sequence
            .record_reply_readback(&completion, &sink)
            .await
            .unwrap();
        assert!(
            sequence
                .issue_completion(&foreign_completion, &gate, &sink)
                .await
                .is_err()
        );
        let completion_outcome = completion
            .classify_completion_mutation_response(COMPLETION_ACCEPTED, "course-instance-1")
            .unwrap();
        assert!(
            sequence
                .record_reply_outcome(&draft, &completion_outcome, &sink)
                .await
                .is_err()
        );
        assert_eq!(sink.state.lock().unwrap().issues.len(), 1);
    }

    #[tokio::test]
    async fn recovery_snapshot_is_exact_and_read_only_for_every_durable_boundary() {
        let (draft, completion) = material("private reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let sink = FixtureSequenceSink::default();
        sequence.prepare(&sink).await.unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), None)
                .unwrap(),
            UaiDiscussionRecoveryState::NoMutationEvidence
        );

        sequence.issue_reply(&draft, &sink).await.unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), None)
                .unwrap(),
            UaiDiscussionRecoveryState::ReplyIssuedAmbiguous
        );
        let reply = draft
            .classify_reply_mutation_response(REPLY_ACCEPTED)
            .unwrap();
        sequence
            .record_reply_outcome(&draft, &reply, &sink)
            .await
            .unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), None)
                .unwrap(),
            UaiDiscussionRecoveryState::ReplyAcceptedAwaitingReadback
        );

        let readback_digest = sequence.validate_completion_plan(&completion).unwrap();
        sink.record_verification(
            ExecutionMutationVerification::new(1, readback_digest, true).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), Some(&completion))
                .unwrap(),
            UaiDiscussionRecoveryState::ReplyReadbackVerifiedAwaitingGate
        );
        let verified_snapshot = sink.snapshot(&sequence);
        let gate = sequence
            .finish_recovered_reply_readback_gate(&verified_snapshot, &completion, &sink)
            .await
            .unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), Some(&completion))
                .unwrap(),
            UaiDiscussionRecoveryState::ReplyReadbackGateRecorded
        );
        let recovered_gate = sequence
            .recover_reply_readback_gate(&sink.snapshot(&sequence), &completion)
            .unwrap();
        assert_eq!(recovered_gate, gate);
        sequence
            .issue_completion(&completion, &recovered_gate, &sink)
            .await
            .unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), Some(&completion))
                .unwrap(),
            UaiDiscussionRecoveryState::CompletionIssuedAmbiguous
        );
        let completed = completion
            .classify_completion_mutation_response(COMPLETION_ACCEPTED, "course-instance-1")
            .unwrap();
        sequence
            .record_completion_outcome(&completion, &completed, &sink)
            .await
            .unwrap();
        assert_eq!(
            sequence
                .inspect_recovery(&sink.snapshot(&sequence), Some(&completion))
                .unwrap(),
            UaiDiscussionRecoveryState::CompletionAcceptedAwaitingProgress
        );
    }

    #[test]
    fn recovery_rejects_a_core_valid_but_provider_impossible_rejection() {
        let (draft, _) = material("private reply");
        let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
        let issue = ExecutionMutationIssue::new(
            1,
            UAI_DISCUSSION_REPLY_OPERATION_TYPE,
            draft.request_digest(),
        )
        .unwrap();
        let record = ExecutionMutationRecoveryRecord::try_new(
            issue,
            Some(ExecutionMutationReceipt::new(1, [9; 32], false).unwrap()),
            None,
        )
        .unwrap();
        let snapshot = ExecutionMutationSequenceRecoverySnapshot::try_new(
            sequence.artifact().clone(),
            sequence.plan().clone(),
            vec![record],
            Vec::new(),
        )
        .unwrap();
        assert!(sequence.inspect_recovery(&snapshot, None).is_err());
    }

    fn material(content: &str) -> (UaiDiscussionReplyDraft, UaiDiscussionCompletionPlan) {
        let draft = UaiDiscussionReplyDraft::try_new(
            UaiDiscussionBinding::try_new(
                "2001",
                "course-instance-1",
                "group-1",
                "class-7",
                "1001",
                "user-9",
            )
            .unwrap(),
            42,
            content,
        )
        .unwrap();
        let document = serde_json::json!({
            "success": true,
            "value": {
                "replyContents": [{
                    "replyId": 9,
                    "createId": "user-9",
                    "content": content,
                }],
            },
        })
        .to_string();
        let page = parse_discussion_reply_page(&document, 42, 20).unwrap();
        let completion = prepare_discussion_completion(
            &discussion_detail(),
            "group:2001:unit-1:group-1",
            &draft,
            &page,
        )
        .unwrap();
        (draft, completion)
    }

    fn discussion_detail() -> RemoteTaskDetail {
        let normalized = serde_json::json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": "unit-1", "title": "Unit 1"},
            "section": {"id": "section-1", "title": "Section 1"},
            "micro": {"id": "micro-1", "title": "Discussion"},
            "group_id": "group-1",
            "task_types": ["discussion"],
            "question_count": 1,
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "group:2001:unit-1:group-1".to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Discuss".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "v1:discussion".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: serde_json::json!({"schema":"uai.group-task.raw.v1"}),
            },
            normalized_detail: serde_json::json!({
                "schema": "uai.group-task-detail.v1",
                "task": normalized,
            }),
        }
    }

    #[derive(Default)]
    struct FixtureSequenceSink {
        state: Mutex<FixtureSequenceState>,
    }

    #[derive(Default)]
    struct FixtureSequenceState {
        prepared: Option<ExecutionMutationSequencePlan>,
        issues: Vec<ExecutionMutationIssue>,
        receipts: Vec<ExecutionMutationReceipt>,
        verifications: Vec<ExecutionMutationVerification>,
        observations: Vec<ExecutionMutationSequenceObservation>,
    }

    impl FixtureSequenceSink {
        fn snapshot(
            &self,
            sequence: &UaiDiscussionMutationSequence,
        ) -> ExecutionMutationSequenceRecoverySnapshot {
            let state = self.state.lock().unwrap();
            let records = state
                .issues
                .iter()
                .map(|issue| {
                    ExecutionMutationRecoveryRecord::try_new(
                        issue.clone(),
                        state
                            .receipts
                            .iter()
                            .find(|receipt| receipt.ordinal() == issue.ordinal())
                            .copied(),
                        state
                            .verifications
                            .iter()
                            .find(|verification| verification.ordinal() == issue.ordinal())
                            .copied(),
                    )
                    .unwrap()
                })
                .collect();
            ExecutionMutationSequenceRecoverySnapshot::try_new(
                sequence.artifact().clone(),
                sequence.plan().clone(),
                records,
                state.observations.clone(),
            )
            .unwrap()
        }
    }

    #[async_trait]
    impl ExecutionMutationSink for FixtureSequenceSink {
        async fn prepare_sequence_plan(
            &self,
            plan: &ExecutionMutationSequencePlan,
        ) -> ProviderResult<()> {
            if plan.sequence_type() != UAI_DISCUSSION_SEQUENCE_TYPE {
                return Err(foreign_sequence_material());
            }
            let mut state = self.state.lock().unwrap();
            match &state.prepared {
                Some(existing) if existing != plan => Err(foreign_sequence_material()),
                Some(_) => Ok(()),
                None => {
                    state.prepared = Some(plan.clone());
                    Ok(())
                }
            }
        }

        async fn issue(&self, issue: &ExecutionMutationIssue) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            let next = state.issues.len() + 1;
            let phase_valid = match next {
                1 => issue.operation_type() == UAI_DISCUSSION_REPLY_OPERATION_TYPE,
                2 => {
                    issue.operation_type() == UAI_DISCUSSION_COMPLETION_OPERATION_TYPE
                        && state
                            .receipts
                            .first()
                            .is_some_and(|receipt| receipt.accepted())
                        && state
                            .verifications
                            .first()
                            .is_some_and(|verification| verification.verified())
                        && state.observations.first().is_some_and(|observation| {
                            observation.observation_type()
                                == UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE
                                && observation.observation_digest()
                                    == state.verifications[0].observation_digest()
                        })
                }
                _ => false,
            };
            if state.prepared.is_none()
                || state.issues.len() != state.receipts.len()
                || usize::try_from(issue.ordinal()).ok() != Some(next)
                || !phase_valid
            {
                return Err(foreign_sequence_material());
            }
            state.issues.push(issue.clone());
            Ok(())
        }

        async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            if !receipt.accepted()
                || state.issues.len() != state.receipts.len() + 1
                || state.issues.last().map(ExecutionMutationIssue::ordinal)
                    != Some(receipt.ordinal())
            {
                return Err(foreign_sequence_material());
            }
            state.receipts.push(receipt);
            Ok(())
        }

        async fn record_verification(
            &self,
            verification: ExecutionMutationVerification,
        ) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            if let Some(existing) = state.verifications.first() {
                return if *existing == verification {
                    Ok(())
                } else {
                    Err(foreign_sequence_material())
                };
            }
            if verification.ordinal() != UAI_DISCUSSION_REPLY_ORDINAL
                || !verification.verified()
                || state
                    .receipts
                    .first()
                    .is_none_or(|receipt| !receipt.accepted())
            {
                return Err(foreign_sequence_material());
            }
            state.verifications.push(verification);
            Ok(())
        }

        async fn record_sequence_observation(
            &self,
            observation: ExecutionMutationSequenceObservation,
        ) -> ProviderResult<()> {
            let mut state = self.state.lock().unwrap();
            if let Some(existing) = state.observations.first() {
                return if *existing == observation {
                    Ok(())
                } else {
                    Err(foreign_sequence_material())
                };
            }
            if state.verifications.first().is_none_or(|verification| {
                observation.phase_position() != UAI_DISCUSSION_COMPLETION_PHASE_POSITION
                    || observation.observation_type()
                        != UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE
                    || observation.observation_digest() != verification.observation_digest()
            }) {
                return Err(foreign_sequence_material());
            }
            state.observations.push(observation);
            Ok(())
        }
    }
}
