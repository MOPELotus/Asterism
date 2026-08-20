use std::fmt;

use asterism_domain::{
    CourseId, ExecutionAttemptId, ExecutionId, ProviderAccountId, TaskId, Timestamp, UserId,
};
use asterism_provider_api::{
    ExecutionMutationRecoveryRecord, ProviderError, ProviderErrorKind, ProviderResult,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    UAI_UPLOAD_FINAL_OPERATION_TYPE, UaiUploadFinalPlanState, UaiUploadFinalResultState,
    UaiUploadFinalSubmissionKind, UaiUploadFinalSubmissionSequence, UaiUploadGrantState,
    UaiUploadInputState, UaiUploadObjectState, UaiUploadVerification, build_upload_multipart,
};

pub const UAI_UPLOAD_ATTEMPT_BINDING_TYPE: &str = "uai.upload.attempt-binding.v1";
pub const UAI_UPLOAD_OBJECT_OPERATION_TYPE: &str = "uai.upload.object";

/// Independently loaded Core authority for one single-upload Attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "the explicit ID suffixes preserve the Core entity contract names"
)]
pub struct UaiUploadAttemptScope {
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    task_id: TaskId,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
}

impl UaiUploadAttemptScope {
    pub const fn new(
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        course_id: CourseId,
        task_id: TaskId,
        execution_id: ExecutionId,
        execution_attempt_id: ExecutionAttemptId,
    ) -> Self {
        Self {
            owner_user_id,
            provider_account_id,
            course_id,
            task_id,
            execution_id,
            execution_attempt_id,
        }
    }

    pub const fn owner_user_id(&self) -> UserId {
        self.owner_user_id
    }

    pub const fn provider_account_id(&self) -> ProviderAccountId {
        self.provider_account_id
    }

    pub const fn course_id(&self) -> CourseId {
        self.course_id
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn execution_id(&self) -> ExecutionId {
        self.execution_id
    }

    pub const fn execution_attempt_id(&self) -> ExecutionAttemptId {
        self.execution_attempt_id
    }
}

/// Independently persisted encrypted Provider-state handles for one upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UaiUploadAttemptStateDigests {
    input: [u8; 32],
    grant: [u8; 32],
    object: [u8; 32],
    final_plan: [u8; 32],
    final_result: [u8; 32],
}

impl UaiUploadAttemptStateDigests {
    pub const fn new(
        input: [u8; 32],
        grant: [u8; 32],
        object: [u8; 32],
        final_plan: [u8; 32],
        final_result: [u8; 32],
    ) -> Self {
        Self {
            input,
            grant,
            object,
            final_plan,
            final_result,
        }
    }

    pub const fn input(&self) -> [u8; 32] {
        self.input
    }

    pub const fn grant(&self) -> [u8; 32] {
        self.grant
    }

    pub const fn object(&self) -> [u8; 32] {
        self.object
    }

    pub const fn final_plan(&self) -> [u8; 32] {
        self.final_plan
    }

    pub const fn final_result(&self) -> [u8; 32] {
        self.final_result
    }
}

/// Credential-free immutable projection of one accepted and readback-verified
/// single-upload Attempt.
///
/// The exact artifact bytes, filename, token, object key/hash, openid, Course
/// instance, accepted version and readback document remain in encrypted
/// Provider state. Fresh Group progress is deliberately not represented here:
/// it remains the separate completion authority.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiUploadAttemptBinding {
    schema: String,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    task_id: TaskId,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
    object_ordinal: u32,
    final_ordinal: u32,
    accepted_at: Timestamp,
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    artifact_binding_digest: [u8; 32],
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
    attempt_binding_digest: [u8; 32],
}

impl UaiUploadAttemptBinding {
    /// Projects one fully parsed single-upload readback into a compact Core
    /// binding after validating every predecessor state and both mutation
    /// records.
    ///
    /// # Errors
    ///
    /// Rejects compound state, cross-stage splicing, a Qiniu response promoted
    /// to verification, or any issue/receipt/readback/state-handle drift.
    #[allow(
        clippy::too_many_arguments,
        reason = "construction receives every independently persisted upload authority"
    )]
    pub fn try_new(
        scope: &UaiUploadAttemptScope,
        sequence: &UaiUploadFinalSubmissionSequence,
        input: &UaiUploadInputState,
        grant: &UaiUploadGrantState,
        object: &UaiUploadObjectState,
        final_plan: &UaiUploadFinalPlanState,
        result: &UaiUploadFinalResultState,
        object_record: &ExecutionMutationRecoveryRecord,
        final_record: &ExecutionMutationRecoveryRecord,
        verification: &UaiUploadVerification,
        state_digests: UaiUploadAttemptStateDigests,
    ) -> ProviderResult<Self> {
        let binding = Self::rebuild(
            scope,
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
        let submission = final_plan.as_single().ok_or_else(foreign_binding)?;
        if verification.remote_task_id() != submission.remote_task_id()
            || verification.artifact_digest() != submission.artifact_digest()
            || verification.submission_version() != result.submission_version()
            || verification.result_digest() != binding.readback_digest
            || !verification.requires_fresh_progress_read()
        {
            return Err(foreign_binding());
        }
        Ok(binding)
    }

    /// Rebuilds every serialized field before any fresh Task/progress/readback
    /// recovery I/O.
    ///
    /// # Errors
    ///
    /// Rejects owner/account/Course/Task/Attempt, predecessor, state, request,
    /// receipt, accepted-version or readback substitution.
    #[allow(
        clippy::too_many_arguments,
        reason = "validation receives every independently persisted upload authority"
    )]
    pub fn validate_before_recovery(
        &self,
        scope: &UaiUploadAttemptScope,
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
        reason = "the compact projection deliberately checks every independent stage"
    )]
    fn rebuild(
        scope: &UaiUploadAttemptScope,
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
        let submission = final_plan.as_single().ok_or_else(foreign_binding)?;
        if input.is_compound()
            || sequence.kind() != UaiUploadFinalSubmissionKind::Single
            || result.kind() != UaiUploadFinalSubmissionKind::Single
        {
            return Err(foreign_binding());
        }
        validate_predecessor_chain(input, grant_state, object_state, submission)?;
        let expected_sequence = UaiUploadFinalSubmissionSequence::for_single(submission)?;
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
        if object_record.issue().operation_type() != UAI_UPLOAD_OBJECT_OPERATION_TYPE
            || object_record.issue().ordinal() != 1
            || object_record.issue().request_digest() != object.object_request_digest()
            || object_receipt.ordinal() != object_record.issue().ordinal()
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
            || stored_object_digest != state_digests.object
            || stored_result_digest != state_digests.final_result
            || state_digests_have_zero(state_digests)
            || [
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

        let course_binding_digest =
            course_binding_digest(scope.course_id, object.course_resource_id());
        let task_binding_digest = task_binding_digest(
            scope.task_id,
            object.remote_task_id(),
            object.task_fingerprint(),
        );
        let artifact_binding_digest = artifact_binding_digest(
            input.artifact().digest().as_str(),
            object.intent_fingerprint(),
            object.upload_position(),
        );
        let receipt_version_digest = receipt_version_digest(result.submission_version());
        let stage_chain_digest = stage_chain_digest(
            state_digests,
            grant.grant_request_digest(),
            grant.grant_response_digest(),
            object.object_request_digest(),
            object.object_response_digest(),
            final_plan.request_digest(),
            result.response_digest(),
        );
        let attempt_binding_digest = attempt_binding_digest(
            scope,
            course_binding_digest,
            task_binding_digest,
            artifact_binding_digest,
            stage_chain_digest,
            sequence_artifact_digest,
            sequence_plan_digest,
            receipt_version_digest,
            readback.observation_digest(),
            result.accepted_at(),
        );
        Ok(Self {
            schema: UAI_UPLOAD_ATTEMPT_BINDING_TYPE.to_owned(),
            owner_user_id: scope.owner_user_id,
            provider_account_id: scope.provider_account_id,
            course_id: scope.course_id,
            task_id: scope.task_id,
            execution_id: scope.execution_id,
            execution_attempt_id: scope.execution_attempt_id,
            object_ordinal: object_record.issue().ordinal(),
            final_ordinal: result.ordinal(),
            accepted_at: result.accepted_at(),
            course_binding_digest,
            task_binding_digest,
            artifact_binding_digest,
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
            input_state_digest: state_digests.input,
            grant_state_digest: state_digests.grant,
            object_state_digest: state_digests.object,
            final_plan_state_digest: state_digests.final_plan,
            final_result_state_digest: state_digests.final_result,
            stage_chain_digest,
            attempt_binding_digest,
        })
    }

    pub fn binding_type(&self) -> &str {
        &self.schema
    }

    pub const fn owner_user_id(&self) -> UserId {
        self.owner_user_id
    }

    pub const fn provider_account_id(&self) -> ProviderAccountId {
        self.provider_account_id
    }

    pub const fn course_id(&self) -> CourseId {
        self.course_id
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn execution_id(&self) -> ExecutionId {
        self.execution_id
    }

    pub const fn execution_attempt_id(&self) -> ExecutionAttemptId {
        self.execution_attempt_id
    }

    pub const fn object_ordinal(&self) -> u32 {
        self.object_ordinal
    }

    pub const fn final_ordinal(&self) -> u32 {
        self.final_ordinal
    }

    pub const fn accepted_at(&self) -> Timestamp {
        self.accepted_at
    }

    pub const fn readback_digest(&self) -> [u8; 32] {
        self.readback_digest
    }

    pub const fn stage_chain_digest(&self) -> [u8; 32] {
        self.stage_chain_digest
    }

    pub const fn attempt_binding_digest(&self) -> [u8; 32] {
        self.attempt_binding_digest
    }
}

impl fmt::Debug for UaiUploadAttemptBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiUploadAttemptBinding")
            .field("schema", &self.schema)
            .field("owner_user_id", &self.owner_user_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("course_id", &self.course_id)
            .field("task_id", &self.task_id)
            .field("execution_id", &self.execution_id)
            .field("execution_attempt_id", &self.execution_attempt_id)
            .field("object_ordinal", &self.object_ordinal)
            .field("final_ordinal", &self.final_ordinal)
            .field("accepted_at", &self.accepted_at)
            .field("evidence", &"[HASHED]")
            .field("state_handles", &"[HASHED]")
            .field("stage_chain_digest", &"[HASHED]")
            .field("attempt_binding_digest", &"[HASHED]")
            .finish_non_exhaustive()
    }
}

fn validate_predecessor_chain(
    input: &UaiUploadInputState,
    grant_state: &UaiUploadGrantState,
    object_state: &UaiUploadObjectState,
    submission: &crate::UaiUploadSubmission,
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
        || grant.upload_position() != 1
        || object.upload_position() != 1
        || grant.file_key() != object.file_key()
        || grant.intent_fingerprint() != object.intent_fingerprint()
        || submission.remote_task_id() != object.remote_task_id()
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

fn state_digests_have_zero(digests: UaiUploadAttemptStateDigests) -> bool {
    digests.input == [0; 32]
        || digests.grant == [0; 32]
        || digests.object == [0; 32]
        || digests.final_plan == [0; 32]
        || digests.final_result == [0; 32]
}

fn course_binding_digest(course_id: CourseId, remote_course_id: &str) -> [u8; 32] {
    hash_fields(
        b"asterism:uai:upload-course-binding:v1\0",
        &[
            course_id.to_string().as_bytes(),
            remote_course_id.as_bytes(),
        ],
    )
}

fn task_binding_digest(task_id: TaskId, remote_task_id: &str, task_fingerprint: &str) -> [u8; 32] {
    hash_fields(
        b"asterism:uai:upload-task-binding:v1\0",
        &[
            task_id.to_string().as_bytes(),
            remote_task_id.as_bytes(),
            task_fingerprint.as_bytes(),
        ],
    )
}

fn artifact_binding_digest(
    artifact_digest: &str,
    intent_fingerprint: &str,
    upload_position: u32,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-artifact-binding:v1\0");
    digest.update(artifact_digest.as_bytes());
    digest.update(b"\0");
    digest.update(intent_fingerprint.as_bytes());
    digest.update(b"\0");
    digest.update(upload_position.to_be_bytes());
    digest.finalize().into()
}

fn receipt_version_digest(version: &str) -> [u8; 32] {
    hash_fields(
        b"asterism:uai:upload-receipt-version:v1\0",
        &[version.as_bytes()],
    )
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
    digest.update(b"asterism:uai:upload-stage-chain:v1\0");
    for value in [
        state_digests.input,
        state_digests.grant,
        state_digests.object,
        state_digests.final_plan,
        state_digests.final_result,
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
    reason = "the Attempt digest binds every independent aggregate authority"
)]
fn attempt_binding_digest(
    scope: &UaiUploadAttemptScope,
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    artifact_binding_digest: [u8; 32],
    stage_chain_digest: [u8; 32],
    sequence_artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
    receipt_version_digest: [u8; 32],
    readback_digest: [u8; 32],
    accepted_at: Timestamp,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:upload-attempt-binding:v1\0");
    for value in [
        scope.owner_user_id.to_string(),
        scope.provider_account_id.to_string(),
        scope.course_id.to_string(),
        scope.task_id.to_string(),
        scope.execution_id.to_string(),
        scope.execution_attempt_id.to_string(),
    ] {
        digest.update(value.as_bytes());
        digest.update(b"\0");
    }
    for value in [
        course_binding_digest,
        task_binding_digest,
        artifact_binding_digest,
        stage_chain_digest,
        sequence_artifact_digest,
        sequence_plan_digest,
        receipt_version_digest,
        readback_digest,
    ] {
        digest.update(value);
    }
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
        "UAI upload Attempt binding is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        CourseId, ExecutionAttemptId, ExecutionId, ProviderAccountId, TaskId, UserId,
    };
    use asterism_provider_api::{
        ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
        ExecutionMutationVerification,
    };
    use zeroize::Zeroizing;

    use super::*;
    use crate::{
        EncodedUaiUploadFinalPlanState, EncodedUaiUploadGrantState, EncodedUaiUploadInputState,
        EncodedUaiUploadObjectState, UAI_UPLOAD_OBJECT_STATE_TYPE, UaiUploadArtifact,
        UaiUploadGrant, UaiUploadedArtifact, build_upload_submission_request,
    };

    const REMOTE_TASK_ID: &str = "group:2001:unit-1:group-upload";

    #[test]
    fn projection_is_serializable_credential_free_and_rebuilds_every_stage() {
        let fixture = Fixture::new("course/42/private-recording.mp3", "fixture-a");
        let encoded = serde_json::to_string(&fixture.binding).unwrap();
        for secret in [
            "nothing.mp3",
            "private-recording.mp3",
            "secret-upload-token",
            "synthetic-qiniu-etag",
            "openid-1",
            "course-instance-1",
            "upload-v1",
            REMOTE_TASK_ID,
            "v1:upload",
        ] {
            assert!(!encoded.contains(secret));
        }
        let decoded: UaiUploadAttemptBinding = serde_json::from_str(&encoded).unwrap();
        decoded
            .validate_before_recovery(
                &fixture.scope,
                &fixture.sequence,
                &fixture.input,
                &fixture.grant,
                &fixture.object,
                &fixture.final_plan,
                &fixture.result,
                &fixture.object_record,
                &fixture.final_record,
                fixture.state_digests,
            )
            .unwrap();
        assert_eq!(decoded.binding_type(), UAI_UPLOAD_ATTEMPT_BINDING_TYPE);
        assert_eq!(decoded.object_ordinal(), 1);
        assert_eq!(decoded.final_ordinal(), 1);
        assert_ne!(decoded.readback_digest(), [0; 32]);
        assert_ne!(decoded.stage_chain_digest(), [0; 32]);
        assert_ne!(decoded.attempt_binding_digest(), [0; 32]);
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("private-recording.mp3"));
        assert!(!debug.contains("upload-v1"));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the test independently substitutes every Core aggregate identity"
    )]
    fn projection_rejects_scope_state_and_cross_attempt_substitution() {
        let fixture = Fixture::new("course/42/nothing.mp3", "fixture-a");
        for scope in [
            UaiUploadAttemptScope::new(
                UserId::new(),
                fixture.scope.provider_account_id(),
                fixture.scope.course_id(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiUploadAttemptScope::new(
                fixture.scope.owner_user_id(),
                ProviderAccountId::new(),
                fixture.scope.course_id(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiUploadAttemptScope::new(
                fixture.scope.owner_user_id(),
                fixture.scope.provider_account_id(),
                CourseId::new(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiUploadAttemptScope::new(
                fixture.scope.owner_user_id(),
                fixture.scope.provider_account_id(),
                fixture.scope.course_id(),
                TaskId::new(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiUploadAttemptScope::new(
                fixture.scope.owner_user_id(),
                fixture.scope.provider_account_id(),
                fixture.scope.course_id(),
                fixture.scope.task_id(),
                ExecutionId::new(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiUploadAttemptScope::new(
                fixture.scope.owner_user_id(),
                fixture.scope.provider_account_id(),
                fixture.scope.course_id(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                ExecutionAttemptId::new(),
            ),
        ] {
            assert!(
                fixture
                    .binding
                    .validate_before_recovery(
                        &scope,
                        &fixture.sequence,
                        &fixture.input,
                        &fixture.grant,
                        &fixture.object,
                        &fixture.final_plan,
                        &fixture.result,
                        &fixture.object_record,
                        &fixture.final_record,
                        fixture.state_digests,
                    )
                    .is_err()
            );
        }

        let changed_state = UaiUploadAttemptStateDigests::new(
            [9; 32],
            fixture.state_digests.grant(),
            fixture.state_digests.object(),
            fixture.state_digests.final_plan(),
            fixture.state_digests.final_result(),
        );
        assert!(
            fixture
                .binding
                .validate_before_recovery(
                    &fixture.scope,
                    &fixture.sequence,
                    &fixture.input,
                    &fixture.grant,
                    &fixture.object,
                    &fixture.final_plan,
                    &fixture.result,
                    &fixture.object_record,
                    &fixture.final_record,
                    changed_state,
                )
                .is_err()
        );

        let foreign = Fixture::with_scope(
            fixture.scope,
            "course/42/another-recording.mp3",
            "fixture-b",
        );
        assert!(
            fixture
                .binding
                .validate_before_recovery(
                    &foreign.scope,
                    &foreign.sequence,
                    &foreign.input,
                    &foreign.grant,
                    &foreign.object,
                    &foreign.final_plan,
                    &foreign.result,
                    &foreign.object_record,
                    &foreign.final_record,
                    foreign.state_digests,
                )
                .is_err()
        );
    }

    #[test]
    fn qiniu_receipt_is_not_accepted_as_object_verification() {
        let fixture = Fixture::new("course/42/nothing.mp3", "fixture-a");
        let foreign_object_record = recovery_record(
            1,
            UAI_UPLOAD_OBJECT_OPERATION_TYPE,
            fixture.object.uploaded().object_request_digest(),
            fixture.object.uploaded().object_response_digest(),
            Some(ExecutionMutationVerification::new(1, [8; 32], true).unwrap()),
        );
        let encoded_object =
            EncodedUaiUploadObjectState::try_new(fixture.object.uploaded()).unwrap();
        let foreign_object_record = with_stage_output(
            foreign_object_record,
            UAI_UPLOAD_OBJECT_STATE_TYPE,
            encoded_object.digest(),
            encoded_object.into_secret_value(),
        );
        assert!(
            fixture
                .binding
                .validate_before_recovery(
                    &fixture.scope,
                    &fixture.sequence,
                    &fixture.input,
                    &fixture.grant,
                    &fixture.object,
                    &fixture.final_plan,
                    &fixture.result,
                    &foreign_object_record,
                    &fixture.final_record,
                    fixture.state_digests,
                )
                .is_err()
        );
    }

    struct Fixture {
        scope: UaiUploadAttemptScope,
        sequence: UaiUploadFinalSubmissionSequence,
        input: UaiUploadInputState,
        grant: UaiUploadGrantState,
        object: UaiUploadObjectState,
        final_plan: UaiUploadFinalPlanState,
        result: UaiUploadFinalResultState,
        object_record: ExecutionMutationRecoveryRecord,
        final_record: ExecutionMutationRecoveryRecord,
        state_digests: UaiUploadAttemptStateDigests,
        binding: UaiUploadAttemptBinding,
    }

    impl Fixture {
        fn new(file_key: &str, fingerprint_suffix: &str) -> Self {
            Self::with_scope(
                UaiUploadAttemptScope::new(
                    UserId::new(),
                    ProviderAccountId::new(),
                    CourseId::new(),
                    TaskId::new(),
                    ExecutionId::new(),
                    ExecutionAttemptId::new(),
                ),
                file_key,
                fingerprint_suffix,
            )
        }

        #[allow(
            clippy::too_many_lines,
            reason = "the fixture keeps all five independently persisted stages visible"
        )]
        fn with_scope(
            scope: UaiUploadAttemptScope,
            file_key: &str,
            fingerprint_suffix: &str,
        ) -> Self {
            let artifact = UaiUploadArtifact::donor_minimal_mp3();
            let artifact_digest = artifact.digest();
            let encoded_input =
                EncodedUaiUploadInputState::for_single(REMOTE_TASK_ID, &artifact).unwrap();
            let input_digest = encoded_input.digest();
            let input_value = encoded_input.into_secret_value();
            let input = UaiUploadInputState::decode_single_bound(
                &input_value,
                input_digest,
                REMOTE_TASK_ID,
                &artifact_digest,
            )
            .unwrap();

            let grant_value = UaiUploadGrant::restore_grant_state(
                Zeroizing::new("secret-upload-token".to_owned()),
                file_key.to_owned(),
                "uai-upload-v1:synthetic-intent".to_owned(),
                artifact_digest.clone(),
                REMOTE_TASK_ID.to_owned(),
                "v1:upload".to_owned(),
                "2001".to_owned(),
                "unit-1".to_owned(),
                "group-upload".to_owned(),
                1,
                [1; 32],
                [2; 32],
            )
            .unwrap();
            let encoded_grant = EncodedUaiUploadGrantState::try_new(&grant_value).unwrap();
            let grant_digest = encoded_grant.digest();
            let grant_secret = encoded_grant.into_secret_value();
            let grant = UaiUploadGrantState::decode_bound(
                &grant_secret,
                grant_digest,
                grant_value.grant_request_digest(),
                grant_value.grant_response_digest(),
            )
            .unwrap();

            let multipart = build_upload_multipart(grant.grant(), input.artifact()).unwrap();
            let uploaded = UaiUploadedArtifact::restore_object_state(
                REMOTE_TASK_ID.to_owned(),
                "v1:upload".to_owned(),
                "2001".to_owned(),
                "unit-1".to_owned(),
                "group-upload".to_owned(),
                1,
                file_key.to_owned(),
                artifact_digest.clone(),
                "uai-upload-v1:synthetic-intent".to_owned(),
                multipart.request_digest(),
                [4; 32],
                Some(Zeroizing::new("synthetic-qiniu-etag".to_owned())),
            )
            .unwrap();
            let encoded_object = EncodedUaiUploadObjectState::try_new(&uploaded).unwrap();
            let object_digest = encoded_object.digest();
            let object_secret = encoded_object.into_secret_value();
            let object = UaiUploadObjectState::decode_bound(
                &object_secret,
                object_digest,
                uploaded.object_request_digest(),
                uploaded.object_response_digest(),
            )
            .unwrap();
            let object_record = recovery_record(
                1,
                UAI_UPLOAD_OBJECT_OPERATION_TYPE,
                uploaded.object_request_digest(),
                uploaded.object_response_digest(),
                None,
            );
            let object_record = with_stage_output(
                object_record,
                UAI_UPLOAD_OBJECT_STATE_TYPE,
                object_digest,
                asterism_secrets::SecretValue::new(object_secret.expose_secret().to_vec()),
            );

            let submission = crate::UaiUploadSubmission::restore_final_plan(
                REMOTE_TASK_ID.to_owned(),
                "2001".to_owned(),
                "unit-1".to_owned(),
                "group-upload".to_owned(),
                file_key.to_owned(),
                artifact_digest,
                "uai-upload-v1:synthetic-intent".to_owned(),
                123_290,
                format!("uai-upload-submit-v1:{fingerprint_suffix}"),
            )
            .unwrap();
            let sequence = UaiUploadFinalSubmissionSequence::for_single(&submission).unwrap();
            let request =
                build_upload_submission_request(&submission, "course-instance-1", "openid-1")
                    .unwrap();
            let encoded_plan =
                EncodedUaiUploadFinalPlanState::for_single(&submission, &request).unwrap();
            let final_plan_digest = encoded_plan.digest();
            let plan_secret = encoded_plan.into_secret_value();
            let final_plan =
                UaiUploadFinalPlanState::decode_bound(&plan_secret, final_plan_digest, &sequence)
                    .unwrap();
            let outcome = request
                .classify_final_response(
                    1,
                    r#"{"code":0,"data":{"course_id":"course-instance-1","group_id":"group-upload","version":"upload-v1"}}"#,
                    "course-instance-1",
                    "group-upload",
                )
                .unwrap();
            let result = sequence.accepted_result_state(&outcome).unwrap();
            let document = verification_document(file_key, "upload-v1");
            let (verification, mutation_verification) = result
                .verify_single_plan_state(&document, &final_plan)
                .unwrap();
            let initial_final_record = recovery_record(
                1,
                UAI_UPLOAD_FINAL_OPERATION_TYPE,
                request.request_digest(),
                outcome.response_digest(),
                Some(mutation_verification),
            );
            let encoded_result = result.encode().unwrap();
            let final_result_digest = encoded_result.digest();
            let result_secret = encoded_result.into_secret_value();
            let result = UaiUploadFinalResultState::decode_bound(
                &result_secret,
                final_result_digest,
                &sequence,
                &initial_final_record,
            )
            .unwrap();
            let final_record = with_stage_output(
                initial_final_record,
                crate::UAI_UPLOAD_FINAL_RESULT_STATE_TYPE,
                final_result_digest,
                asterism_secrets::SecretValue::new(result_secret.expose_secret().to_vec()),
            );
            let state_digests = UaiUploadAttemptStateDigests::new(
                input_digest,
                grant_digest,
                object_digest,
                final_plan_digest,
                final_result_digest,
            );
            let binding = UaiUploadAttemptBinding::try_new(
                &scope,
                &sequence,
                &input,
                &grant,
                &object,
                &final_plan,
                &result,
                &object_record,
                &final_record,
                &verification,
                state_digests,
            )
            .unwrap();
            Self {
                scope,
                sequence,
                input,
                grant,
                object,
                final_plan,
                result,
                object_record,
                final_record,
                state_digests,
                binding,
            }
        }
    }

    fn recovery_record(
        ordinal: u32,
        operation_type: &str,
        request_digest: [u8; 32],
        response_digest: [u8; 32],
        verification: Option<ExecutionMutationVerification>,
    ) -> ExecutionMutationRecoveryRecord {
        ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(ordinal, operation_type, request_digest).unwrap(),
            Some(ExecutionMutationReceipt::new(ordinal, response_digest, true).unwrap()),
            verification,
        )
        .unwrap()
    }

    fn with_stage_output(
        record: ExecutionMutationRecoveryRecord,
        output_type: &str,
        digest: [u8; 32],
        value: asterism_secrets::SecretValue,
    ) -> ExecutionMutationRecoveryRecord {
        let ordinal = record.issue().ordinal();
        record
            .try_with_stage_output(
                asterism_provider_api::ExecutionMutationStageOutput::try_new(
                    asterism_domain::ProviderId::new(crate::metadata::PROVIDER_ID).unwrap(),
                    ordinal,
                    output_type,
                    digest,
                    value,
                )
                .unwrap(),
            )
            .unwrap()
    }

    fn verification_document(file_key: &str, version: &str) -> String {
        let answer = serde_json::json!({
            "value": [],
            "children": [{"value": [file_key], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let questions = serde_json::json!([{
            "instanceId": "0",
            "answer": answer,
            "context": "{\"state\":\"submitted\"}",
        }])
        .to_string();
        serde_json::json!({
            "success": true,
            "code": 0,
            "data": {
                "course": "course-instance-1",
                "module": format!("group-upload-{version}"),
                "state": {
                    "version": version,
                    "quesData": questions,
                    "__EXTEND_DATA__": {"__SUBMIT_INFO__": {
                        "course_id": "course-instance-1",
                        "group_id": "group-upload",
                        "version": version,
                    }},
                },
            },
        })
        .to_string()
    }

    #[test]
    fn projection_rejects_unknown_serialized_fields() {
        let fixture = Fixture::new("course/42/nothing.mp3", "fixture-a");
        let mut value = serde_json::to_value(&fixture.binding).unwrap();
        value.as_object_mut().unwrap().insert(
            "object_key".to_owned(),
            serde_json::Value::String("course/42/nothing.mp3".to_owned()),
        );
        assert!(serde_json::from_value::<UaiUploadAttemptBinding>(value).is_err());
    }
}
