use std::fmt;

use asterism_domain::{
    CourseId, ExecutionAttemptId, ExecutionId, ProviderAccountId, TaskId, UserId,
};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{UaiDiscussionCompletionState, UaiDiscussionMutationSequence, UaiDiscussionReplyState};

pub const UAI_DISCUSSION_DRAFT_ATTEMPT_BINDING_TYPE: &str =
    "uai.discussion.draft-attempt-binding.v1";

/// Independently supplied Core authority for one discussion Draft/Attempt.
/// It contains stable entity IDs only and grants no Provider mutation method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "the explicit ID suffixes preserve the Core entity contract names"
)]
pub struct UaiDiscussionAttemptScope {
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    task_id: TaskId,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
}

impl UaiDiscussionAttemptScope {
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

/// Credential-free immutable projection binding one Core-owned discussion
/// Draft/Attempt to its encrypted Provider states and compact mutation plan.
///
/// Provider route/account values and reply content never enter this object.
/// Only Core IDs, the public numeric topic and domain-separated digests are
/// serializable. All fields are private so changing any authority requires a
/// new projection rather than mutating an existing one.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UaiDiscussionDraftAttemptBinding {
    schema: String,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: CourseId,
    task_id: TaskId,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
    topic_id: u64,
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    reply_request_digest: [u8; 32],
    current_user_reply_digest: [u8; 32],
    phase_two_plan_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
    reply_state_digest: [u8; 32],
    completion_state_digest: [u8; 32],
    draft_binding_digest: [u8; 32],
    attempt_binding_digest: [u8; 32],
}

impl UaiDiscussionDraftAttemptBinding {
    /// Builds one compact projection from independently supplied Core scope and
    /// the already-bound Provider artifact/private states.
    ///
    /// # Errors
    ///
    /// Rejects zero state or Provider digests and any private-state identity
    /// that is inconsistent with its compact sequence.
    pub fn try_new(
        scope: &UaiDiscussionAttemptScope,
        sequence: &UaiDiscussionMutationSequence,
        reply: &UaiDiscussionReplyState,
        completion: &UaiDiscussionCompletionState,
        reply_state_digest: [u8; 32],
        completion_state_digest: [u8; 32],
    ) -> ProviderResult<Self> {
        let draft = reply.draft();
        let completion = completion.completion();
        let artifact_digest = sequence.artifact().artifact_digest();
        let sequence_plan_digest = sequence.plan().plan_digest();
        let reply_request_digest = draft.request_digest();
        let current_user_reply_digest = completion.reply_digest();
        let phase_two_plan_digest = completion.request_digest();
        if draft.topic_id() == 0
            || completion.topic_id() != draft.topic_id()
            || completion.course_resource_id() != draft.binding().course_resource_id()
            || completion.group_id() != draft.binding().group_id()
            || completion.reply_request_digest() != reply_request_digest
            || [
                artifact_digest,
                sequence_plan_digest,
                reply_request_digest,
                current_user_reply_digest,
                phase_two_plan_digest,
                reply_state_digest,
                completion_state_digest,
            ]
            .contains(&[0; 32])
        {
            return Err(foreign_binding());
        }
        let course_binding_digest =
            course_binding_digest(scope.course_id, completion.course_resource_id());
        let task_binding_digest = task_binding_digest(
            scope.task_id,
            completion.remote_task_id(),
            completion.task_fingerprint(),
        );
        let draft_binding_digest = draft_binding_digest(
            course_binding_digest,
            task_binding_digest,
            draft.topic_id(),
            reply_request_digest,
            current_user_reply_digest,
            artifact_digest,
            sequence_plan_digest,
        );
        let attempt_binding_digest = attempt_binding_digest(
            scope,
            draft_binding_digest,
            phase_two_plan_digest,
            reply_state_digest,
            completion_state_digest,
        );
        Ok(Self {
            schema: UAI_DISCUSSION_DRAFT_ATTEMPT_BINDING_TYPE.to_owned(),
            owner_user_id: scope.owner_user_id,
            provider_account_id: scope.provider_account_id,
            course_id: scope.course_id,
            task_id: scope.task_id,
            execution_id: scope.execution_id,
            execution_attempt_id: scope.execution_attempt_id,
            topic_id: draft.topic_id(),
            course_binding_digest,
            task_binding_digest,
            reply_request_digest,
            current_user_reply_digest,
            phase_two_plan_digest,
            artifact_digest,
            sequence_plan_digest,
            reply_state_digest,
            completion_state_digest,
            draft_binding_digest,
            attempt_binding_digest,
        })
    }

    /// Rebuilds every field from independently loaded Core scope and decoded
    /// Provider states immediately before fresh recovery reads.
    ///
    /// # Errors
    ///
    /// Rejects owner, account, Course, Task, execution/Attempt, topic, reply,
    /// phase-two plan, state-handle, artifact or sequence substitution.
    pub fn validate_before_recovery(
        &self,
        scope: &UaiDiscussionAttemptScope,
        sequence: &UaiDiscussionMutationSequence,
        reply: &UaiDiscussionReplyState,
        completion: &UaiDiscussionCompletionState,
        reply_state_digest: [u8; 32],
        completion_state_digest: [u8; 32],
    ) -> ProviderResult<()> {
        let expected = Self::try_new(
            scope,
            sequence,
            reply,
            completion,
            reply_state_digest,
            completion_state_digest,
        )?;
        if self != &expected {
            return Err(foreign_binding());
        }
        Ok(())
    }

    pub const fn owner_user_id(&self) -> UserId {
        self.owner_user_id
    }

    pub fn binding_type(&self) -> &str {
        &self.schema
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

    pub const fn topic_id(&self) -> u64 {
        self.topic_id
    }

    pub const fn course_binding_digest(&self) -> [u8; 32] {
        self.course_binding_digest
    }

    pub const fn task_binding_digest(&self) -> [u8; 32] {
        self.task_binding_digest
    }

    pub const fn reply_request_digest(&self) -> [u8; 32] {
        self.reply_request_digest
    }

    pub const fn current_user_reply_digest(&self) -> [u8; 32] {
        self.current_user_reply_digest
    }

    pub const fn phase_two_plan_digest(&self) -> [u8; 32] {
        self.phase_two_plan_digest
    }

    pub const fn artifact_digest(&self) -> [u8; 32] {
        self.artifact_digest
    }

    pub const fn sequence_plan_digest(&self) -> [u8; 32] {
        self.sequence_plan_digest
    }

    pub const fn reply_state_digest(&self) -> [u8; 32] {
        self.reply_state_digest
    }

    pub const fn completion_state_digest(&self) -> [u8; 32] {
        self.completion_state_digest
    }

    pub const fn draft_binding_digest(&self) -> [u8; 32] {
        self.draft_binding_digest
    }

    pub const fn attempt_binding_digest(&self) -> [u8; 32] {
        self.attempt_binding_digest
    }
}

impl fmt::Debug for UaiDiscussionDraftAttemptBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionDraftAttemptBinding")
            .field("schema", &self.schema)
            .field("owner_user_id", &self.owner_user_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("course_id", &self.course_id)
            .field("task_id", &self.task_id)
            .field("execution_id", &self.execution_id)
            .field("execution_attempt_id", &self.execution_attempt_id)
            .field("topic_id", &self.topic_id)
            .field("course_binding_digest", &"[HASHED]")
            .field("task_binding_digest", &"[HASHED]")
            .field("reply_request_digest", &"[HASHED]")
            .field("current_user_reply_digest", &"[HASHED]")
            .field("phase_two_plan_digest", &"[HASHED]")
            .field("artifact_digest", &"[HASHED]")
            .field("sequence_plan_digest", &"[HASHED]")
            .field("reply_state_digest", &"[HASHED]")
            .field("completion_state_digest", &"[HASHED]")
            .field("draft_binding_digest", &"[HASHED]")
            .field("attempt_binding_digest", &"[HASHED]")
            .finish()
    }
}

fn course_binding_digest(course_id: CourseId, remote_course_id: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:discussion-course-binding:v1\0");
    digest.update(course_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(remote_course_id.as_bytes());
    digest.finalize().into()
}

fn task_binding_digest(task_id: TaskId, remote_task_id: &str, task_fingerprint: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:discussion-task-binding:v1\0");
    digest.update(task_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(remote_task_id.as_bytes());
    digest.update(b"\0");
    digest.update(task_fingerprint.as_bytes());
    digest.finalize().into()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the draft digest binds every independent immutable authority"
)]
fn draft_binding_digest(
    course_binding_digest: [u8; 32],
    task_binding_digest: [u8; 32],
    topic_id: u64,
    reply_request_digest: [u8; 32],
    current_user_reply_digest: [u8; 32],
    artifact_digest: [u8; 32],
    sequence_plan_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:discussion-draft-binding:v1\0");
    digest.update(course_binding_digest);
    digest.update(task_binding_digest);
    digest.update(topic_id.to_be_bytes());
    digest.update(reply_request_digest);
    digest.update(current_user_reply_digest);
    digest.update(artifact_digest);
    digest.update(sequence_plan_digest);
    digest.finalize().into()
}

fn attempt_binding_digest(
    scope: &UaiDiscussionAttemptScope,
    draft_binding_digest: [u8; 32],
    phase_two_plan_digest: [u8; 32],
    reply_state_digest: [u8; 32],
    completion_state_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:discussion-attempt-binding:v1\0");
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
    digest.update(phase_two_plan_digest);
    digest.update(reply_state_digest);
    digest.update(completion_state_digest);
    digest.finalize().into()
}

fn foreign_binding() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI discussion Draft/Attempt binding is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::{
        EncodedUaiDiscussionCompletionState, EncodedUaiDiscussionReplyState, UaiDiscussionBinding,
        UaiDiscussionCompletionPlan, UaiDiscussionReplyDraft,
        discussion::{discussion_completion_request_digest, discussion_reply_digest},
    };

    #[test]
    fn projection_is_serializable_credential_free_and_rebuilds_every_field() {
        let fixture = Fixture::new("bounded reply", 7_001, "v1:discussion");
        let encoded = serde_json::to_string(&fixture.binding).unwrap();
        assert!(!encoded.contains("bounded reply"));
        assert!(!encoded.contains("user-1"));
        assert!(!encoded.contains("course-instance-1"));
        assert!(!encoded.contains("curricula-1"));
        let decoded: UaiDiscussionDraftAttemptBinding = serde_json::from_str(&encoded).unwrap();
        decoded
            .validate_before_recovery(
                &fixture.scope,
                &fixture.sequence,
                &fixture.reply,
                &fixture.completion,
                fixture.reply_state_digest,
                fixture.completion_state_digest,
            )
            .unwrap();
        assert_eq!(decoded.topic_id(), 7_001);
        assert_ne!(decoded.current_user_reply_digest(), [0; 32]);
        assert_ne!(decoded.phase_two_plan_digest(), [0; 32]);
        assert_ne!(decoded.draft_binding_digest(), [0; 32]);
        assert_ne!(decoded.attempt_binding_digest(), [0; 32]);
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("bounded reply"));
        assert!(!debug.contains("user-1"));
    }

    #[test]
    fn projection_rejects_owner_account_course_task_topic_reply_and_plan_drift() {
        let fixture = Fixture::new("bounded reply", 7_001, "v1:discussion");
        for scope in [
            UaiDiscussionAttemptScope::new(
                UserId::new(),
                fixture.scope.provider_account_id(),
                fixture.scope.course_id(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiDiscussionAttemptScope::new(
                fixture.scope.owner_user_id(),
                ProviderAccountId::new(),
                fixture.scope.course_id(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiDiscussionAttemptScope::new(
                fixture.scope.owner_user_id(),
                fixture.scope.provider_account_id(),
                CourseId::new(),
                fixture.scope.task_id(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
            UaiDiscussionAttemptScope::new(
                fixture.scope.owner_user_id(),
                fixture.scope.provider_account_id(),
                fixture.scope.course_id(),
                TaskId::new(),
                fixture.scope.execution_id(),
                fixture.scope.execution_attempt_id(),
            ),
        ] {
            assert!(
                fixture
                    .binding
                    .validate_before_recovery(
                        &scope,
                        &fixture.sequence,
                        &fixture.reply,
                        &fixture.completion,
                        fixture.reply_state_digest,
                        fixture.completion_state_digest,
                    )
                    .is_err()
            );
        }

        for foreign in [
            Fixture::with_scope(fixture.scope, "bounded reply", 7_002, "v1:discussion"),
            Fixture::with_scope(fixture.scope, "different reply", 7_001, "v1:discussion"),
            Fixture::with_scope(fixture.scope, "bounded reply", 7_001, "v1:changed"),
        ] {
            assert!(
                fixture
                    .binding
                    .validate_before_recovery(
                        &foreign.scope,
                        &foreign.sequence,
                        &foreign.reply,
                        &foreign.completion,
                        foreign.reply_state_digest,
                        foreign.completion_state_digest,
                    )
                    .is_err()
            );
        }
    }

    struct Fixture {
        scope: UaiDiscussionAttemptScope,
        sequence: UaiDiscussionMutationSequence,
        reply: UaiDiscussionReplyState,
        completion: UaiDiscussionCompletionState,
        reply_state_digest: [u8; 32],
        completion_state_digest: [u8; 32],
        binding: UaiDiscussionDraftAttemptBinding,
    }

    impl Fixture {
        fn new(content: &str, topic_id: u64, task_fingerprint: &str) -> Self {
            Self::with_scope(
                UaiDiscussionAttemptScope::new(
                    UserId::new(),
                    ProviderAccountId::new(),
                    CourseId::new(),
                    TaskId::new(),
                    ExecutionId::new(),
                    ExecutionAttemptId::new(),
                ),
                content,
                topic_id,
                task_fingerprint,
            )
        }

        fn with_scope(
            scope: UaiDiscussionAttemptScope,
            content: &str,
            topic_id: u64,
            task_fingerprint: &str,
        ) -> Self {
            let draft = UaiDiscussionReplyDraft::try_new(
                UaiDiscussionBinding::try_new(
                    "2001",
                    "course-instance-1",
                    "group-discussion",
                    "class-1",
                    "curricula-1",
                    "user-1",
                )
                .unwrap(),
                topic_id,
                content,
            )
            .unwrap();
            let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
            let encoded_reply = EncodedUaiDiscussionReplyState::try_new(&draft, &sequence).unwrap();
            let reply_state_digest = encoded_reply.digest();
            let reply_value = encoded_reply.into_secret_value();
            let reply =
                UaiDiscussionReplyState::decode_bound(&reply_value, reply_state_digest, &sequence)
                    .unwrap();
            let completion_plan = completion_plan(reply.draft(), task_fingerprint);
            let encoded_completion =
                EncodedUaiDiscussionCompletionState::try_new(&completion_plan, &reply, &sequence)
                    .unwrap();
            let completion_state_digest = encoded_completion.digest();
            let completion_value = encoded_completion.into_secret_value();
            let completion = UaiDiscussionCompletionState::decode_bound(
                &completion_value,
                completion_state_digest,
                &reply,
                &sequence,
            )
            .unwrap();
            let binding = UaiDiscussionDraftAttemptBinding::try_new(
                &scope,
                &sequence,
                &reply,
                &completion,
                reply_state_digest,
                completion_state_digest,
            )
            .unwrap();
            Self {
                scope,
                sequence,
                reply,
                completion,
                reply_state_digest,
                completion_state_digest,
                binding,
            }
        }
    }

    fn completion_plan(
        draft: &UaiDiscussionReplyDraft,
        task_fingerprint: &str,
    ) -> UaiDiscussionCompletionPlan {
        let remote_task_id = "group:2001:unit-1:group-discussion";
        let reply_digest = discussion_reply_digest(draft);
        let request_digest = discussion_completion_request_digest(
            remote_task_id,
            task_fingerprint,
            "2001",
            "unit-1",
            "group-discussion",
            draft.topic_id(),
            draft.request_digest(),
            reply_digest,
        );
        UaiDiscussionCompletionPlan::restore_state(
            remote_task_id.to_owned(),
            task_fingerprint.to_owned(),
            "2001".to_owned(),
            "unit-1".to_owned(),
            "group-discussion".to_owned(),
            draft.topic_id(),
            draft.request_digest(),
            reply_digest,
            request_digest,
            format!("uai-discussion-complete-v1:{}", hex_digest(request_digest)),
        )
        .unwrap()
    }

    fn hex_digest(digest: [u8; 32]) -> String {
        let mut encoded = String::with_capacity(64);
        for byte in digest {
            write!(&mut encoded, "{byte:02x}").unwrap();
        }
        encoded
    }
}
