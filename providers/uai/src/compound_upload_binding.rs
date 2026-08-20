use std::fmt;

use asterism_domain::{SubmissionDraft, SubmissionDraftId, Timestamp};
use asterism_provider_api::{
    ExecutionMutationRecoveryRecord, ProviderError, ProviderErrorKind, ProviderResult,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    UAI_UPLOAD_FINAL_OPERATION_TYPE, UAI_UPLOAD_OBJECT_OPERATION_TYPE, UaiCompoundUploadSubmission,
    UaiCompoundUploadVerification, UaiSubmissionPlan, UaiUploadAttemptScope,
    UaiUploadAttemptStateDigests, UaiUploadFinalPlanState, UaiUploadFinalResultState,
    UaiUploadFinalSubmissionKind, UaiUploadFinalSubmissionSequence, UaiUploadGrantState,
    UaiUploadInputState, UaiUploadObjectState, build_upload_multipart,
};

pub const UAI_COMPOUND_UPLOAD_DRAFT_ATTEMPT_BINDING_TYPE: &str =
    "uai.compound-upload.draft-attempt-binding.v1";

/// Credential-free immutable projection of one accepted and readback-verified
/// atomic ordinary-choice plus upload Draft/Attempt.
///
/// It binds the complete ordinary Draft digest and the private two-module plan
/// digest without serializing the selected answer, artifact bytes, CMS token,
/// object key/hash, dynamic Course/account route, accepted version or readback
/// document. It grants no authority to split or replay either mutation.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiCompoundUploadDraftAttemptBinding {
    schema: String,
    owner_user_id: asterism_domain::UserId,
    provider_account_id: asterism_domain::ProviderAccountId,
    course_id: asterism_domain::CourseId,
    task_id: asterism_domain::TaskId,
    execution_id: asterism_domain::ExecutionId,
    execution_attempt_id: asterism_domain::ExecutionAttemptId,
    ordinary_draft_id: SubmissionDraftId,
    object_ordinal: u32,
    final_ordinal: u32,
    accepted_at: Timestamp,
    ordinary_draft_digest: [u8; 32],
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    artifact_binding_digest: [u8; 32],
    atomic_plan_digest: [u8; 32],
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
    object_request_digest: [u8; 32],
    object_response_digest: [u8; 32],
    final_request_digest: [u8; 32],
    final_response_digest: [u8; 32],
    sequence_artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
    receipt_version_digest: [u8; 32],
    readback_digest: [u8; 32],
    input_state_digest: [u8; 32],
    grant_state_digest: [u8; 32],
    object_state_digest: [u8; 32],
    final_plan_state_digest: [u8; 32],
    final_result_state_digest: [u8; 32],
    stage_chain_digest: [u8; 32],
    draft_binding_digest: [u8; 32],
    attempt_binding_digest: [u8; 32],
}

impl UaiCompoundUploadDraftAttemptBinding {
    /// Projects one complete compound Draft/upload chain only after parsing the
    /// exact atomic answer-plus-key readback.
    ///
    /// # Errors
    ///
    /// Rejects single state, same-ID Draft answer substitution, stage splicing,
    /// a Qiniu receipt promoted to verification, or any final atomic request,
    /// receipt, readback or encrypted-state drift.
    #[allow(
        clippy::too_many_arguments,
        reason = "construction receives every independently persisted compound-upload authority"
    )]
    pub fn try_new(
        scope: &UaiUploadAttemptScope,
        ordinary_draft: &SubmissionDraft,
        sequence: &UaiUploadFinalSubmissionSequence,
        input: &UaiUploadInputState,
        grant: &UaiUploadGrantState,
        object: &UaiUploadObjectState,
        final_plan: &UaiUploadFinalPlanState,
        result: &UaiUploadFinalResultState,
        object_record: &ExecutionMutationRecoveryRecord,
        final_record: &ExecutionMutationRecoveryRecord,
        verification: &UaiCompoundUploadVerification,
        state_digests: UaiUploadAttemptStateDigests,
    ) -> ProviderResult<Self> {
        let binding = Self::rebuild(
            scope,
            ordinary_draft,
            sequence,
            input,
            grant,
            object,
            final_plan,
            result,
            object_record,
            final_record,
            state_digests,
        )?;
        let submission = final_plan.as_compound().ok_or_else(foreign_binding)?;
        if verification.ordinary_draft_id() != ordinary_draft.id
            || verification.remote_task_id() != submission.remote_task_id()
            || verification.artifact_digest() != submission.artifact_digest()
            || verification.submission_version() != result.submission_version()
            || verification.result_digest() != binding.readback_digest
            || !verification.requires_fresh_progress_read()
        {
            return Err(foreign_binding());
        }
        Ok(binding)
    }

    /// Rebuilds the complete projection before fresh Task/progress/readback
    /// recovery I/O.
    ///
    /// # Errors
    ///
    /// Rejects any Core scope, complete Draft, predecessor, atomic plan,
    /// request/receipt/readback or encrypted-state substitution.
    #[allow(
        clippy::too_many_arguments,
        reason = "validation receives every independently persisted compound-upload authority"
    )]
    pub fn validate_before_recovery(
        &self,
        scope: &UaiUploadAttemptScope,
        ordinary_draft: &SubmissionDraft,
        sequence: &UaiUploadFinalSubmissionSequence,
        input: &UaiUploadInputState,
        grant: &UaiUploadGrantState,
        object: &UaiUploadObjectState,
        final_plan: &UaiUploadFinalPlanState,
        result: &UaiUploadFinalResultState,
        object_record: &ExecutionMutationRecoveryRecord,
        final_record: &ExecutionMutationRecoveryRecord,
        state_digests: UaiUploadAttemptStateDigests,
    ) -> ProviderResult<()> {
        let expected = Self::rebuild(
            scope,
            ordinary_draft,
            sequence,
            input,
            grant,
            object,
            final_plan,
            result,
            object_record,
            final_record,
            state_digests,
        )?;
        if self != &expected {
            return Err(foreign_binding());
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the compact projection deliberately checks every atomic compound stage"
    )]
    fn rebuild(
        scope: &UaiUploadAttemptScope,
        ordinary_draft: &SubmissionDraft,
        sequence: &UaiUploadFinalSubmissionSequence,
        input: &UaiUploadInputState,
        grant_state: &UaiUploadGrantState,
        object_state: &UaiUploadObjectState,
        final_plan: &UaiUploadFinalPlanState,
        result: &UaiUploadFinalResultState,
        object_record: &ExecutionMutationRecoveryRecord,
        final_record: &ExecutionMutationRecoveryRecord,
        state_digests: UaiUploadAttemptStateDigests,
    ) -> ProviderResult<Self> {
        let submission = final_plan.as_compound().ok_or_else(foreign_binding)?;
        if !input.is_compound()
            || sequence.kind() != UaiUploadFinalSubmissionKind::Compound
            || result.kind() != UaiUploadFinalSubmissionKind::Compound
            || ordinary_draft.task_id != scope.task_id()
        {
            return Err(foreign_binding());
        }
        input.validate_compound_draft(ordinary_draft)?;
        let ordinary_draft_digest = input
            .ordinary_draft_digest()
            .filter(|digest| *digest != [0; 32])
            .ok_or_else(foreign_binding)?;
        validate_predecessor_chain(input, grant_state, object_state, submission)?;
        validate_atomic_ordinary_plan(ordinary_draft, submission)?;

        let expected_sequence = UaiUploadFinalSubmissionSequence::for_compound(submission)?;
        let object = object_state.uploaded();
        let grant = grant_state.grant();
        let object_receipt = object_record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_binding)?;
        let final_receipt = final_record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_binding)?;
        let readback = final_record
            .verification()
            .filter(|verification| verification.verified())
            .ok_or_else(foreign_binding)?;
        let sequence_artifact_digest = sequence.artifact().artifact_digest();
        let sequence_plan_digest = sequence.plan().plan_digest();
        let atomic_plan_digest = atomic_plan_digest(submission);
        let recovered_object = UaiUploadObjectState::decode_recovery_record(object_record)?;
        let recovered_result =
            UaiUploadFinalResultState::decode_recovery_record(sequence, final_record)?;
        let stored_object_digest = object_record
            .stage_output()
            .map(asterism_provider_api::ExecutionMutationStageOutput::output_digest)
            .ok_or_else(foreign_binding)?;
        let stored_result_digest = final_record
            .stage_output()
            .map(asterism_provider_api::ExecutionMutationStageOutput::output_digest)
            .ok_or_else(foreign_binding)?;
        if input.ordinary_draft_id() != Some(ordinary_draft.id)
            || submission.ordinary_draft_id() != ordinary_draft.id
            || object_record.issue().operation_type() != UAI_UPLOAD_OBJECT_OPERATION_TYPE
            || object_record.issue().ordinal() != 1
            || object_record.issue().request_digest() != object.object_request_digest()
            || object_receipt.ordinal() != 1
            || object_receipt.response_digest() != object.object_response_digest()
            || object_record.verification().is_some()
            || final_record.issue().operation_type() != UAI_UPLOAD_FINAL_OPERATION_TYPE
            || final_record.issue().ordinal() != result.ordinal()
            || final_record.issue().request_digest() != final_plan.request_digest()
            || final_receipt.ordinal() != result.ordinal()
            || final_receipt.response_digest() != result.response_digest()
            || readback.ordinal() != result.ordinal()
            || result.request_digest() != final_plan.request_digest()
            || result.plan_digest() != sequence_plan_digest
            || result.artifact_digest() != sequence_artifact_digest
            || expected_sequence.artifact().artifact_digest() != sequence_artifact_digest
            || expected_sequence.plan().plan_digest() != sequence_plan_digest
            || !recovered_object.same_recovery_authority(object_state)
            || !recovered_result.same_recovery_authority(result)
            || stored_object_digest != state_digests.object()
            || stored_result_digest != state_digests.final_result()
            || state_digests_have_zero(state_digests)
            || [
                atomic_plan_digest,
                grant.grant_request_digest(),
                grant.grant_response_digest(),
                object.object_request_digest(),
                object.object_response_digest(),
                final_plan.request_digest(),
                result.response_digest(),
                sequence_artifact_digest,
                sequence_plan_digest,
                readback.observation_digest(),
            ]
            .contains(&[0; 32])
        {
            return Err(foreign_binding());
        }

        let course_binding_digest = hash_fields(
            b"asterism:uai:compound-upload-course-binding:v1\0",
            &[
                scope.course_id().to_string().as_bytes(),
                object.course_resource_id().as_bytes(),
            ],
        );
        let task_binding_digest = hash_fields(
            b"asterism:uai:compound-upload-task-binding:v1\0",
            &[
                scope.task_id().to_string().as_bytes(),
                object.remote_task_id().as_bytes(),
                object.task_fingerprint().as_bytes(),
            ],
        );
        let artifact_binding_digest = artifact_binding_digest(
            input.artifact().digest().as_str(),
            object.intent_fingerprint(),
            object.upload_position(),
        );
        let receipt_version_digest = hash_fields(
            b"asterism:uai:compound-upload-receipt-version:v1\0",
            &[result.submission_version().as_bytes()],
        );
        let stage_chain_digest = stage_chain_digest(
            state_digests,
            grant.grant_request_digest(),
            grant.grant_response_digest(),
            object.object_request_digest(),
            object.object_response_digest(),
            final_plan.request_digest(),
            result.response_digest(),
        );
        let draft_binding_digest = draft_binding_digest(
            ordinary_draft.id,
            ordinary_draft_digest,
            course_binding_digest,
            task_binding_digest,
            artifact_binding_digest,
            atomic_plan_digest,
            sequence_artifact_digest,
            sequence_plan_digest,
            final_plan.request_digest(),
            result.response_digest(),
            receipt_version_digest,
            readback.observation_digest(),
        );
        let attempt_binding_digest = attempt_binding_digest(
            scope,
            draft_binding_digest,
            stage_chain_digest,
            result.accepted_at(),
        );
        Ok(Self {
            schema: UAI_COMPOUND_UPLOAD_DRAFT_ATTEMPT_BINDING_TYPE.to_owned(),
            owner_user_id: scope.owner_user_id(),
            provider_account_id: scope.provider_account_id(),
            course_id: scope.course_id(),
            task_id: scope.task_id(),
            execution_id: scope.execution_id(),
            execution_attempt_id: scope.execution_attempt_id(),
            ordinary_draft_id: ordinary_draft.id,
            object_ordinal: 1,
            final_ordinal: result.ordinal(),
            accepted_at: result.accepted_at(),
            ordinary_draft_digest,
            course_binding_digest,
            task_binding_digest,
            artifact_binding_digest,
            atomic_plan_digest,
            grant_request_digest: grant.grant_request_digest(),
            grant_response_digest: grant.grant_response_digest(),
            object_request_digest: object.object_request_digest(),
            object_response_digest: object.object_response_digest(),
            final_request_digest: final_plan.request_digest(),
            final_response_digest: result.response_digest(),
            sequence_artifact_digest,
            sequence_plan_digest,
            receipt_version_digest,
            readback_digest: readback.observation_digest(),
            input_state_digest: state_digests.input(),
            grant_state_digest: state_digests.grant(),
            object_state_digest: state_digests.object(),
            final_plan_state_digest: state_digests.final_plan(),
            final_result_state_digest: state_digests.final_result(),
            stage_chain_digest,
            draft_binding_digest,
            attempt_binding_digest,
        })
    }

    pub fn binding_type(&self) -> &str {
        &self.schema
    }

    pub const fn ordinary_draft_id(&self) -> SubmissionDraftId {
        self.ordinary_draft_id
    }

    pub const fn final_ordinal(&self) -> u32 {
        self.final_ordinal
    }

    pub const fn readback_digest(&self) -> [u8; 32] {
        self.readback_digest
    }

    pub const fn draft_binding_digest(&self) -> [u8; 32] {
        self.draft_binding_digest
    }

    pub const fn attempt_binding_digest(&self) -> [u8; 32] {
        self.attempt_binding_digest
    }
}

impl fmt::Debug for UaiCompoundUploadDraftAttemptBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundUploadDraftAttemptBinding")
            .field("schema", &self.schema)
            .field("owner_user_id", &self.owner_user_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("course_id", &self.course_id)
            .field("task_id", &self.task_id)
            .field("execution_id", &self.execution_id)
            .field("execution_attempt_id", &self.execution_attempt_id)
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("object_ordinal", &self.object_ordinal)
            .field("final_ordinal", &self.final_ordinal)
            .field("accepted_at", &self.accepted_at)
            .field("evidence", &"[HASHED]")
            .field("state_handles", &"[HASHED]")
            .field("stage_chain_digest", &"[HASHED]")
            .field("draft_binding_digest", &"[HASHED]")
            .field("attempt_binding_digest", &"[HASHED]")
            .finish_non_exhaustive()
    }
}

fn validate_predecessor_chain(
    input: &UaiUploadInputState,
    grant_state: &UaiUploadGrantState,
    object_state: &UaiUploadObjectState,
    submission: &UaiCompoundUploadSubmission,
) -> ProviderResult<()> {
    let grant = grant_state.grant();
    let object = object_state.uploaded();
    let artifact_digest = input.artifact().digest();
    if input.remote_task_id() != grant.remote_task_id()
        || input.remote_task_id() != object.remote_task_id()
        || artifact_digest != grant.artifact_digest()
        || artifact_digest != object.artifact_digest()
        || grant.remote_task_id() != object.remote_task_id()
        || grant.task_fingerprint() != object.task_fingerprint()
        || grant.course_resource_id() != object.course_resource_id()
        || grant.unit_id() != object.unit_id()
        || grant.group_id() != object.group_id()
        || grant.upload_position() != 2
        || object.upload_position() != 2
        || grant.file_key() != object.file_key()
        || grant.intent_fingerprint() != object.intent_fingerprint()
        || submission.remote_task_id() != object.remote_task_id()
        || submission.task_fingerprint() != object.task_fingerprint()
        || submission.course_resource_id() != object.course_resource_id()
        || submission.unit_id() != object.unit_id()
        || submission.group_id() != object.group_id()
        || submission.expose_file_key() != object.file_key()
        || submission.artifact_digest() != object.artifact_digest()
        || submission.upload_intent_fingerprint() != object.intent_fingerprint()
    {
        return Err(foreign_binding());
    }
    let multipart = build_upload_multipart(grant, input.artifact())?;
    if multipart.request_digest() != object.object_request_digest() {
        return Err(foreign_binding());
    }
    Ok(())
}

fn validate_atomic_ordinary_plan(
    ordinary_draft: &SubmissionDraft,
    submission: &UaiCompoundUploadSubmission,
) -> ProviderResult<()> {
    let expected = UaiSubmissionPlan::from_single_draft_current(
        ordinary_draft,
        "multichoice",
        submission.course_publish_version(),
    )?;
    if ordinary_plan_digest(&expected) != ordinary_plan_digest(submission.ordinary_plan()) {
        return Err(foreign_binding());
    }
    Ok(())
}

fn atomic_plan_digest(submission: &UaiCompoundUploadSubmission) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-atomic-plan:v1\0");
    digest.update(submission.ordinary_draft_id().to_string().as_bytes());
    digest.update(b"\0");
    digest.update(submission.final_sequence_binding_digest());
    digest.update(ordinary_plan_digest(submission.ordinary_plan()));
    digest.finalize().into()
}

fn ordinary_plan_digest(plan: &UaiSubmissionPlan) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-ordinary-plan:v1\0");
    digest.update(plan.protocol_versions().course().to_be_bytes());
    digest.update(plan.protocol_versions().answer().to_be_bytes());
    digest.update(
        u32::try_from(plan.questions().len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for question in plan.questions() {
        update_field(&mut digest, question.remote_question_id());
        update_field(&mut digest, question.task_type());
        digest.update(
            u32::try_from(question.answer_children().len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for child in question.answer_children() {
            digest.update(u32::try_from(child.len()).unwrap_or(u32::MAX).to_be_bytes());
            for value in child {
                update_field(&mut digest, value);
            }
        }
        digest.update(
            u32::try_from(question.judges().len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for judge in question.judges() {
            update_field(&mut digest, judge.question_type());
            update_field(&mut digest, judge.reply_type());
        }
    }
    digest.finalize().into()
}

fn update_field(digest: &mut Sha256, value: &str) {
    digest.update(u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    digest.update(value.as_bytes());
}

fn state_digests_have_zero(digests: UaiUploadAttemptStateDigests) -> bool {
    digests.input() == [0; 32]
        || digests.grant() == [0; 32]
        || digests.object() == [0; 32]
        || digests.final_plan() == [0; 32]
        || digests.final_result() == [0; 32]
}

fn artifact_binding_digest(
    artifact_digest: &str,
    intent_fingerprint: &str,
    upload_position: u32,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-artifact-binding:v1\0");
    digest.update(artifact_digest.as_bytes());
    digest.update(b"\0");
    digest.update(intent_fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(upload_position.to_be_bytes());
    digest.finalize().into()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the chain digest binds all five state handles and transport transitions"
)]
fn stage_chain_digest(
    state_digests: UaiUploadAttemptStateDigests,
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
    object_request_digest: [u8; 32],
    object_response_digest: [u8; 32],
    final_request_digest: [u8; 32],
    final_response_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-stage-chain:v1\0");
    for value in [
        state_digests.input(),
        state_digests.grant(),
        state_digests.object(),
        state_digests.final_plan(),
        state_digests.final_result(),
        grant_request_digest,
        grant_response_digest,
        object_request_digest,
        object_response_digest,
        final_request_digest,
        final_response_digest,
    ] {
        digest.update(value);
    }
    digest.finalize().into()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the Draft digest binds every independent atomic mutation authority"
)]
fn draft_binding_digest(
    ordinary_draft_id: SubmissionDraftId,
    ordinary_draft_digest: [u8; 32],
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    artifact_binding_digest: [u8; 32],
    atomic_plan_digest: [u8; 32],
    sequence_artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
    final_request_digest: [u8; 32],
    final_response_digest: [u8; 32],
    receipt_version_digest: [u8; 32],
    readback_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-draft-binding:v1\0");
    digest.update(ordinary_draft_id.to_string().as_bytes());
    for value in [
        ordinary_draft_digest,
        course_binding_digest,
        task_binding_digest,
        artifact_binding_digest,
        atomic_plan_digest,
        sequence_artifact_digest,
        sequence_plan_digest,
        final_request_digest,
        final_response_digest,
        receipt_version_digest,
        readback_digest,
    ] {
        digest.update(value);
    }
    digest.finalize().into()
}

fn attempt_binding_digest(
    scope: &UaiUploadAttemptScope,
    draft_binding_digest: [u8; 32],
    stage_chain_digest: [u8; 32],
    accepted_at: Timestamp,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-upload-attempt-binding:v1\0");
    for value in [
        scope.owner_user_id().to_string(),
        scope.provider_account_id().to_string(),
        scope.course_id().to_string(),
        scope.task_id().to_string(),
        scope.execution_id().to_string(),
        scope.execution_attempt_id().to_string(),
    ] {
        digest.update(value.as_bytes());
        digest.update(b"\0");
    }
    digest.update(draft_binding_digest);
    digest.update(stage_chain_digest);
    digest.update(accepted_at.timestamp().to_be_bytes());
    digest.update(accepted_at.timestamp_subsec_nanos().to_be_bytes());
    digest.finalize().into()
}

fn hash_fields(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    for field in fields {
        digest.update(field);
        digest.update(b"\0");
    }
    digest.finalize().into()
}

fn foreign_binding() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI compound upload Draft/Attempt binding is stale or foreign",
    )
}
