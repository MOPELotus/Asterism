use std::fmt;

use asterism_domain::{
    CourseId, ExecutionAttemptId, ExecutionId, ProviderAccountId, SubmissionDraftId, TaskId,
    Timestamp, UserId,
};
use asterism_provider_api::{
    ExecutionMutationRecoveryRecord, ProviderError, ProviderErrorKind, ProviderResult,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    UAI_COMPOUND_ORAL_OPERATION_TYPE, UaiCompoundOralPlanState, UaiCompoundOralResultState,
    UaiCompoundOralSubmissionSequence,
};

pub const UAI_COMPOUND_ORAL_DRAFT_ATTEMPT_BINDING_TYPE: &str =
    "uai.compound-oral.draft-attempt-binding.v1";

/// Independently loaded Core authority for one compound-oral Draft/Attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "the explicit ID suffixes preserve the Core entity contract names"
)]
pub struct UaiCompoundOralAttemptScope {
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    task_id: TaskId,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
}

impl UaiCompoundOralAttemptScope {
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

/// Credential-free immutable Core projection for one accepted and verified
/// compound-oral Draft/Attempt.
///
/// It stores Core UUIDs, the fixed mutation ordinal and domain-separated
/// digests only. Matching answers, oral instance/value/extra, Course-instance,
/// account openid, submission version and readback document remain outside the
/// serialized projection.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiCompoundOralDraftAttemptBinding {
    schema: String,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    task_id: TaskId,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
    ordinary_draft_id: SubmissionDraftId,
    oral_ordinal: u32,
    accepted_at: Timestamp,
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    oral_evidence_digest: [u8; 32],
    request_digest: [u8; 32],
    outcome_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
    semantic_plan_digest: [u8; 32],
    receipt_version_digest: [u8; 32],
    readback_digest: [u8; 32],
    fresh_task_evidence_digest: [u8; 32],
    plan_state_digest: [u8; 32],
    result_state_digest: [u8; 32],
    draft_binding_digest: [u8; 32],
    attempt_binding_digest: [u8; 32],
}

impl UaiCompoundOralDraftAttemptBinding {
    /// Projects exact decoded Provider states and one complete Core mutation
    /// record into a credential-free immutable binding.
    ///
    /// # Errors
    ///
    /// Rejects missing/false readback, non-accepted receipt, ordinal, request,
    /// outcome, artifact, sequence, result-version, Task or state-handle drift.
    #[allow(
        clippy::too_many_lines,
        reason = "construction deliberately derives and stores every independent evidence field"
    )]
    pub fn try_new(
        scope: &UaiCompoundOralAttemptScope,
        sequence: &UaiCompoundOralSubmissionSequence,
        plan_state: &UaiCompoundOralPlanState,
        result_state: &UaiCompoundOralResultState,
        record: &ExecutionMutationRecoveryRecord,
        plan_state_digest: [u8; 32],
        result_state_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let submission = plan_state.submission();
        let expected_sequence = UaiCompoundOralSubmissionSequence::try_new(submission)?;
        let receipt = record
            .receipt()
            .filter(|receipt| receipt.accepted())
            .ok_or_else(foreign_binding)?;
        let readback = record
            .verification()
            .filter(|verification| verification.verified())
            .ok_or_else(foreign_binding)?;
        let recovered_result =
            UaiCompoundOralResultState::decode_recovery_record(sequence, record)?;
        let stored_result_digest = record
            .stage_output()
            .map(asterism_provider_api::ExecutionMutationStageOutput::output_digest)
            .ok_or_else(foreign_binding)?;
        let semantic_plan_digest = submission.plan_binding_digest()?;
        let artifact_digest = sequence.artifact().artifact_digest();
        let sequence_plan_digest = sequence.plan().plan_digest();
        if result_state.ordinal() == 0
            || result_state.ordinal() != record.issue().ordinal()
            || readback.ordinal() != result_state.ordinal()
            || record.issue().operation_type() != UAI_COMPOUND_ORAL_OPERATION_TYPE
            || record.issue().request_digest() != plan_state.request_digest()
            || receipt.ordinal() != result_state.ordinal()
            || receipt.response_digest() != result_state.response_digest()
            || result_state.request_digest() != plan_state.request_digest()
            || result_state.plan_binding_digest() != semantic_plan_digest
            || result_state.artifact_digest() != artifact_digest
            || result_state.plan_digest() != sequence_plan_digest
            || stored_result_digest != result_state_digest
            || recovered_result.ordinal() != result_state.ordinal()
            || recovered_result.plan_digest() != result_state.plan_digest()
            || recovered_result.artifact_digest() != result_state.artifact_digest()
            || recovered_result.plan_binding_digest() != result_state.plan_binding_digest()
            || recovered_result.request_digest() != result_state.request_digest()
            || recovered_result.response_digest() != result_state.response_digest()
            || recovered_result.accepted_at() != result_state.accepted_at()
            || recovered_result.submission_version() != result_state.submission_version()
            || expected_sequence.artifact().artifact_digest() != artifact_digest
            || expected_sequence.plan().plan_digest() != sequence_plan_digest
            || [
                plan_state.request_digest(),
                result_state.response_digest(),
                artifact_digest,
                sequence_plan_digest,
                semantic_plan_digest,
                readback.observation_digest(),
                plan_state_digest,
                result_state_digest,
            ]
            .contains(&[0; 32])
        {
            return Err(foreign_binding());
        }
        let receipt_version_digest = receipt_version_digest(result_state.submission_version());
        let course_binding_digest =
            course_binding_digest(scope.course_id, submission.course_resource_id());
        let task_binding_digest = task_binding_digest(
            scope.task_id,
            submission.remote_task_id(),
            submission.task_fingerprint(),
        );
        let oral_evidence_digest = oral_evidence_digest(
            result_state.ordinal(),
            submission.oral_instance_id(),
            semantic_plan_digest,
        );
        let fresh_task_evidence_digest = fresh_task_evidence_digest(
            task_binding_digest,
            submission.course_publish_version(),
            artifact_digest,
            semantic_plan_digest,
        );
        let draft_binding_digest = draft_binding_digest(
            submission.ordinary_draft_id(),
            course_binding_digest,
            task_binding_digest,
            oral_evidence_digest,
            plan_state.request_digest(),
            result_state.response_digest(),
            artifact_digest,
            sequence_plan_digest,
            semantic_plan_digest,
            receipt_version_digest,
            readback.observation_digest(),
            fresh_task_evidence_digest,
        );
        let attempt_binding_digest = attempt_binding_digest(
            scope,
            draft_binding_digest,
            plan_state_digest,
            result_state_digest,
            result_state.accepted_at(),
        );
        Ok(Self {
            schema: UAI_COMPOUND_ORAL_DRAFT_ATTEMPT_BINDING_TYPE.to_owned(),
            owner_user_id: scope.owner_user_id,
            provider_account_id: scope.provider_account_id,
            course_id: scope.course_id,
            task_id: scope.task_id,
            execution_id: scope.execution_id,
            execution_attempt_id: scope.execution_attempt_id,
            ordinary_draft_id: submission.ordinary_draft_id(),
            oral_ordinal: result_state.ordinal(),
            accepted_at: result_state.accepted_at(),
            course_binding_digest,
            task_binding_digest,
            oral_evidence_digest,
            request_digest: plan_state.request_digest(),
            outcome_digest: result_state.response_digest(),
            artifact_digest,
            sequence_plan_digest,
            semantic_plan_digest,
            receipt_version_digest,
            readback_digest: readback.observation_digest(),
            fresh_task_evidence_digest,
            plan_state_digest,
            result_state_digest,
            draft_binding_digest,
            attempt_binding_digest,
        })
    }

    /// Rebuilds every serialized field from independently loaded Core scope,
    /// mutation evidence and decoded encrypted states before fresh reads.
    ///
    /// # Errors
    ///
    /// Rejects any cross-owner/account/Course/Task/Attempt or Provider evidence
    /// substitution.
    #[allow(
        clippy::too_many_arguments,
        reason = "validation receives every independently persisted recovery authority"
    )]
    pub fn validate_before_recovery(
        &self,
        scope: &UaiCompoundOralAttemptScope,
        sequence: &UaiCompoundOralSubmissionSequence,
        plan_state: &UaiCompoundOralPlanState,
        result_state: &UaiCompoundOralResultState,
        record: &ExecutionMutationRecoveryRecord,
        plan_state_digest: [u8; 32],
        result_state_digest: [u8; 32],
    ) -> ProviderResult<()> {
        let expected = Self::try_new(
            scope,
            sequence,
            plan_state,
            result_state,
            record,
            plan_state_digest,
            result_state_digest,
        )?;
        if self != &expected {
            return Err(foreign_binding());
        }
        Ok(())
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

    pub const fn ordinary_draft_id(&self) -> SubmissionDraftId {
        self.ordinary_draft_id
    }

    pub const fn oral_ordinal(&self) -> u32 {
        self.oral_ordinal
    }

    pub const fn accepted_at(&self) -> Timestamp {
        self.accepted_at
    }

    pub const fn course_binding_digest(&self) -> [u8; 32] {
        self.course_binding_digest
    }

    pub const fn task_binding_digest(&self) -> [u8; 32] {
        self.task_binding_digest
    }

    pub const fn oral_evidence_digest(&self) -> [u8; 32] {
        self.oral_evidence_digest
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn outcome_digest(&self) -> [u8; 32] {
        self.outcome_digest
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub const fn sequence_plan_digest(&self) -> [u8; 32] {
        self.sequence_plan_digest
    }

    pub const fn semantic_plan_digest(&self) -> [u8; 32] {
        self.semantic_plan_digest
    }

    pub const fn receipt_version_digest(&self) -> [u8; 32] {
        self.receipt_version_digest
    }

    pub const fn readback_digest(&self) -> [u8; 32] {
        self.readback_digest
    }

    pub const fn fresh_task_evidence_digest(&self) -> [u8; 32] {
        self.fresh_task_evidence_digest
    }

    pub const fn plan_state_digest(&self) -> [u8; 32] {
        self.plan_state_digest
    }

    pub const fn result_state_digest(&self) -> [u8; 32] {
        self.result_state_digest
    }

    pub const fn draft_binding_digest(&self) -> [u8; 32] {
        self.draft_binding_digest
    }

    pub const fn attempt_binding_digest(&self) -> [u8; 32] {
        self.attempt_binding_digest
    }
}

impl fmt::Debug for UaiCompoundOralDraftAttemptBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCompoundOralDraftAttemptBinding")
            .field("schema", &self.schema)
            .field("owner_user_id", &self.owner_user_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("course_id", &self.course_id)
            .field("task_id", &self.task_id)
            .field("execution_id", &self.execution_id)
            .field("execution_attempt_id", &self.execution_attempt_id)
            .field("ordinary_draft_id", &self.ordinary_draft_id)
            .field("oral_ordinal", &self.oral_ordinal)
            .field("accepted_at", &self.accepted_at)
            .field("course_binding_digest", &"[HASHED]")
            .field("task_binding_digest", &"[HASHED]")
            .field("oral_evidence_digest", &"[HASHED]")
            .field("request_digest", &"[HASHED]")
            .field("outcome_digest", &"[HASHED]")
            .field("artifact_digest", &"[HASHED]")
            .field("sequence_plan_digest", &"[HASHED]")
            .field("semantic_plan_digest", &"[HASHED]")
            .field("receipt_version_digest", &"[HASHED]")
            .field("readback_digest", &"[HASHED]")
            .field("fresh_task_evidence_digest", &"[HASHED]")
            .field("plan_state_digest", &"[HASHED]")
            .field("result_state_digest", &"[HASHED]")
            .field("draft_binding_digest", &"[HASHED]")
            .field("attempt_binding_digest", &"[HASHED]")
            .finish()
    }
}

fn course_binding_digest(course_id: CourseId, remote_course_id: &str) -> [u8; 32] {
    hash_fields(
        b"asterism:uai:compound-oral-course-binding:v1\0",
        &[
            course_id.to_string().as_bytes(),
            remote_course_id.as_bytes(),
        ],
    )
}

fn task_binding_digest(task_id: TaskId, remote_task_id: &str, task_fingerprint: &str) -> [u8; 32] {
    hash_fields(
        b"asterism:uai:compound-oral-task-binding:v1\0",
        &[
            task_id.to_string().as_bytes(),
            remote_task_id.as_bytes(),
            task_fingerprint.as_bytes(),
        ],
    )
}

fn oral_evidence_digest(
    ordinal: u32,
    oral_instance_id: &str,
    semantic_plan_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-oral-slot-binding:v1\0");
    digest.update(ordinal.to_be_bytes());
    digest.update(oral_instance_id.as_bytes());
    digest.update(b"\0");
    digest.update(semantic_plan_digest);
    digest.finalize().into()
}

fn receipt_version_digest(version: &str) -> [u8; 32] {
    hash_fields(
        b"asterism:uai:compound-oral-receipt-version:v1\0",
        &[version.as_bytes()],
    )
}

fn fresh_task_evidence_digest(
    task_binding_digest: [u8; 32],
    course_publish_version: u64,
    artifact_digest: [u8; 32],
    semantic_plan_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-oral-fresh-task-evidence:v1\0");
    digest.update(task_binding_digest);
    digest.update(course_publish_version.to_be_bytes());
    digest.update(artifact_digest);
    digest.update(semantic_plan_digest);
    digest.finalize().into()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the draft digest binds every independent oral evidence authority"
)]
fn draft_binding_digest(
    ordinary_draft_id: SubmissionDraftId,
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    oral_evidence_digest: [u8; 32],
    request_digest: [u8; 32],
    outcome_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
    semantic_plan_digest: [u8; 32],
    receipt_version_digest: [u8; 32],
    readback_digest: [u8; 32],
    fresh_task_evidence_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-oral-draft-binding:v1\0");
    digest.update(ordinary_draft_id.to_string().as_bytes());
    for value in [
        course_binding_digest,
        task_binding_digest,
        oral_evidence_digest,
        request_digest,
        outcome_digest,
        artifact_digest,
        sequence_plan_digest,
        semantic_plan_digest,
        receipt_version_digest,
        readback_digest,
        fresh_task_evidence_digest,
    ] {
        digest.update(value);
    }
    digest.finalize().into()
}

fn attempt_binding_digest(
    scope: &UaiCompoundOralAttemptScope,
    draft_binding_digest: [u8; 32],
    plan_state_digest: [u8; 32],
    result_state_digest: [u8; 32],
    accepted_at: Timestamp,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:compound-oral-attempt-binding:v1\0");
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
    digest.update(draft_binding_digest);
    digest.update(plan_state_digest);
    digest.update(result_state_digest);
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
        "UAI compound oral Draft/Attempt binding is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        CourseId, ExecutionAttemptId, ExecutionId, ProviderAccountId, SubmissionDraftId, TaskId,
        UserId,
    };
    use asterism_provider_api::{
        ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
        ExecutionMutationVerification,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        EncodedUaiCompoundOralPlanState, UAI_COMPOUND_ORAL_OPERATION_TYPE,
        UaiCompoundOralSubmission, build_compound_oral_submission_request,
    };

    #[test]
    fn projection_serializes_only_core_ids_ordinal_and_digests() {
        let fixture = Fixture::new();
        let encoded = serde_json::to_string(&fixture.binding).unwrap();
        for secret in [
            "right",
            "spoken",
            "openid-1",
            "course-instance-1",
            "group-oral",
            "6001",
            "compound-oral-v1",
        ] {
            assert!(!encoded.contains(secret));
        }
        let decoded: UaiCompoundOralDraftAttemptBinding = serde_json::from_str(&encoded).unwrap();
        decoded
            .validate_before_recovery(
                &fixture.scope,
                &fixture.sequence,
                &fixture.plan_state,
                &fixture.result_state,
                &fixture.record,
                fixture.plan_state_digest,
                fixture.result_state_digest,
            )
            .unwrap();
        assert_eq!(decoded.oral_ordinal(), 1);
        assert_ne!(decoded.request_digest(), [0; 32]);
        assert_ne!(decoded.outcome_digest(), [0; 32]);
        assert_ne!(decoded.receipt_version_digest(), [0; 32]);
        assert_ne!(decoded.readback_digest(), [0; 32]);
        assert_ne!(decoded.fresh_task_evidence_digest(), [0; 32]);
        assert!(!format!("{decoded:?}").contains("compound-oral-v1"));
    }

    #[test]
    fn projection_rejects_core_scope_and_every_evidence_digest_drift() {
        let fixture = Fixture::new();
        for scope in [
            scope_with(&fixture.scope, ScopeDrift::Owner),
            scope_with(&fixture.scope, ScopeDrift::Account),
            scope_with(&fixture.scope, ScopeDrift::Course),
            scope_with(&fixture.scope, ScopeDrift::Task),
            scope_with(&fixture.scope, ScopeDrift::Execution),
            scope_with(&fixture.scope, ScopeDrift::Attempt),
        ] {
            assert!(
                fixture
                    .binding
                    .validate_before_recovery(
                        &scope,
                        &fixture.sequence,
                        &fixture.plan_state,
                        &fixture.result_state,
                        &fixture.record,
                        fixture.plan_state_digest,
                        fixture.result_state_digest,
                    )
                    .is_err()
            );
        }

        let tampered = [
            {
                let mut value = fixture.binding.clone();
                value.oral_ordinal = 2;
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.request_digest = [1; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.outcome_digest = [2; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.artifact_digest = [3; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.sequence_plan_digest = [4; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.receipt_version_digest = [5; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.readback_digest = [6; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.fresh_task_evidence_digest = [7; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.plan_state_digest = [8; 32];
                value
            },
            {
                let mut value = fixture.binding.clone();
                value.result_state_digest = [9; 32];
                value
            },
        ];
        for binding in tampered {
            assert!(
                binding
                    .validate_before_recovery(
                        &fixture.scope,
                        &fixture.sequence,
                        &fixture.plan_state,
                        &fixture.result_state,
                        &fixture.record,
                        fixture.plan_state_digest,
                        fixture.result_state_digest,
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn projection_rejects_request_outcome_receipt_readback_and_oral_cross_binding() {
        let fixture = Fixture::new();
        for foreign in [
            Fixture::with_scope(
                fixture.scope,
                fixture.ordinary_draft_id,
                "right",
                &json!(["spoken"]),
                "openid-2",
                "compound-oral-v1",
                "synthetic accepted",
            ),
            Fixture::with_scope(
                fixture.scope,
                fixture.ordinary_draft_id,
                "right",
                &json!(["spoken"]),
                "openid-1",
                "compound-oral-v1",
                "different accepted outcome",
            ),
            Fixture::with_scope(
                fixture.scope,
                fixture.ordinary_draft_id,
                "right",
                &json!(["spoken"]),
                "openid-1",
                "compound-oral-v2",
                "synthetic accepted",
            ),
            Fixture::with_scope(
                fixture.scope,
                fixture.ordinary_draft_id,
                "changed",
                &json!(["spoken"]),
                "openid-1",
                "compound-oral-v1",
                "synthetic accepted",
            ),
            Fixture::with_scope(
                fixture.scope,
                fixture.ordinary_draft_id,
                "right",
                &json!(["changed"]),
                "openid-1",
                "compound-oral-v1",
                "synthetic accepted",
            ),
        ] {
            assert!(
                fixture
                    .binding
                    .validate_before_recovery(
                        &foreign.scope,
                        &foreign.sequence,
                        &foreign.plan_state,
                        &foreign.result_state,
                        &foreign.record,
                        foreign.plan_state_digest,
                        foreign.result_state_digest,
                    )
                    .is_err()
            );
        }
    }

    #[derive(Clone, Copy)]
    enum ScopeDrift {
        Owner,
        Account,
        Course,
        Task,
        Execution,
        Attempt,
    }

    fn scope_with(
        scope: &UaiCompoundOralAttemptScope,
        drift: ScopeDrift,
    ) -> UaiCompoundOralAttemptScope {
        UaiCompoundOralAttemptScope::new(
            if matches!(drift, ScopeDrift::Owner) {
                UserId::new()
            } else {
                scope.owner_user_id()
            },
            if matches!(drift, ScopeDrift::Account) {
                ProviderAccountId::new()
            } else {
                scope.provider_account_id()
            },
            if matches!(drift, ScopeDrift::Course) {
                CourseId::new()
            } else {
                scope.course_id()
            },
            if matches!(drift, ScopeDrift::Task) {
                TaskId::new()
            } else {
                scope.task_id()
            },
            if matches!(drift, ScopeDrift::Execution) {
                ExecutionId::new()
            } else {
                scope.execution_id()
            },
            if matches!(drift, ScopeDrift::Attempt) {
                ExecutionAttemptId::new()
            } else {
                scope.execution_attempt_id()
            },
        )
    }

    struct Fixture {
        scope: UaiCompoundOralAttemptScope,
        ordinary_draft_id: SubmissionDraftId,
        sequence: UaiCompoundOralSubmissionSequence,
        plan_state: UaiCompoundOralPlanState,
        result_state: UaiCompoundOralResultState,
        record: ExecutionMutationRecoveryRecord,
        plan_state_digest: [u8; 32],
        result_state_digest: [u8; 32],
        binding: UaiCompoundOralDraftAttemptBinding,
    }

    impl Fixture {
        fn new() -> Self {
            let ordinary_draft_id = SubmissionDraftId::new();
            Self::with_scope(
                UaiCompoundOralAttemptScope::new(
                    UserId::new(),
                    ProviderAccountId::new(),
                    CourseId::new(),
                    TaskId::new(),
                    ExecutionId::new(),
                    ExecutionAttemptId::new(),
                ),
                ordinary_draft_id,
                "right",
                &json!(["spoken"]),
                "openid-1",
                "compound-oral-v1",
                "synthetic accepted",
            )
        }

        #[allow(
            clippy::too_many_arguments,
            reason = "the fixture varies each independent cross-binding authority"
        )]
        fn with_scope(
            scope: UaiCompoundOralAttemptScope,
            ordinary_draft_id: SubmissionDraftId,
            ordinary_answer: &str,
            oral_value: &Value,
            account_openid: &str,
            submission_version: &str,
            outcome_message: &str,
        ) -> Self {
            let submission = UaiCompoundOralSubmission::fixture(
                ordinary_draft_id,
                ordinary_answer,
                oral_value.clone(),
                Some(json!({"slot": 1})),
            );
            let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission).unwrap();
            let request = build_compound_oral_submission_request(
                &submission,
                "course-instance-1",
                account_openid,
            )
            .unwrap();
            let encoded_plan =
                EncodedUaiCompoundOralPlanState::try_new(&submission, &request).unwrap();
            let plan_state_digest = encoded_plan.digest();
            let plan_value = encoded_plan.into_secret_value();
            let plan_state = UaiCompoundOralPlanState::decode_bound(
                &plan_value,
                plan_state_digest,
                &sequence,
                request.request_digest(),
            )
            .unwrap();
            let accepted = json!({
                "code": 0,
                "message": outcome_message,
                "data": {
                    "course_id": "course-instance-1",
                    "group_id": "group-oral",
                    "version": submission_version,
                }
            })
            .to_string();
            let outcome = request
                .classify_compound_oral_response(&accepted, "course-instance-1", "group-oral")
                .unwrap();
            let result = sequence.accepted_result_state(&request, &outcome).unwrap();
            let encoded_result = result.encode().unwrap();
            let result_state_digest = encoded_result.digest();
            let result_value = encoded_result.into_secret_value();
            let initial_record = record(request.request_digest(), outcome.response_digest(), None);
            let result_state = UaiCompoundOralResultState::decode_bound(
                &result_value,
                result_state_digest,
                &sequence,
                &initial_record,
            )
            .unwrap();
            let document = verification_document(ordinary_answer, oral_value, submission_version);
            let (_, verification) = result_state
                .verify_plan_state_readback(&document, &plan_state)
                .unwrap();
            let record = record(
                request.request_digest(),
                outcome.response_digest(),
                Some(verification),
            )
            .try_with_stage_output(
                asterism_provider_api::ExecutionMutationStageOutput::try_new(
                    asterism_domain::ProviderId::new(crate::metadata::PROVIDER_ID).unwrap(),
                    1,
                    crate::UAI_COMPOUND_ORAL_RESULT_STATE_TYPE,
                    result_state_digest,
                    asterism_secrets::SecretValue::new(result_value.expose_secret().to_vec()),
                )
                .unwrap(),
            )
            .unwrap();
            let binding = UaiCompoundOralDraftAttemptBinding::try_new(
                &scope,
                &sequence,
                &plan_state,
                &result_state,
                &record,
                plan_state_digest,
                result_state_digest,
            )
            .unwrap();
            Self {
                scope,
                ordinary_draft_id,
                sequence,
                plan_state,
                result_state,
                record,
                plan_state_digest,
                result_state_digest,
                binding,
            }
        }
    }

    fn record(
        request_digest: [u8; 32],
        response_digest: [u8; 32],
        verification: Option<ExecutionMutationVerification>,
    ) -> ExecutionMutationRecoveryRecord {
        ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(1, UAI_COMPOUND_ORAL_OPERATION_TYPE, request_digest)
                .unwrap(),
            Some(ExecutionMutationReceipt::new(1, response_digest, true).unwrap()),
            verification,
        )
        .unwrap()
    }

    fn verification_document(ordinary_answer: &str, oral_value: &Value, version: &str) -> String {
        let ordinary = json!({
            "value": [],
            "children": [{"value": [ordinary_answer], "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let oral = json!({
            "value": [],
            "children": [{"value": oral_value, "extra": {"slot": 1}, "isDone": true}],
            "progress": {},
            "record": {"url": ""},
        })
        .to_string();
        let questions = json!([
            {
                "instanceId": "5001",
                "answer": ordinary,
                "context": "{\"state\":\"submitted\"}",
            },
            {
                "instanceId": "6001",
                "answer": oral,
                "context": "{\"state\":\"submitted\"}",
            },
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
                        "version": version,
                    }},
                },
            },
        })
        .to_string()
    }
}
