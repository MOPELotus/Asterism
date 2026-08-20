use std::{fmt, sync::Arc};

use asterism_provider_api::{
    ExecutionMutationSequenceRecoverySnapshot, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderResult, TaskDetailCapability,
};
use asterism_secrets::SecretValue;
use async_trait::async_trait;

use crate::{
    UaiDiscussionAttemptScope, UaiDiscussionBinding, UaiDiscussionCompletionPlan,
    UaiDiscussionCompletionState, UaiDiscussionDraftAttemptBinding, UaiDiscussionMutationSequence,
    UaiDiscussionRecoveryState, UaiDiscussionReplyPage, UaiDiscussionReplyState,
    prepare_discussion_completion,
};

pub const UAI_DISCUSSION_RECOVERY_PAGE_SIZE: u32 = 20;
pub const UAI_DISCUSSION_RECOVERY_MAX_PAGES: u32 = 128;

/// Read-only subset required by discussion recovery. Implementations cannot
/// receive reply or completion mutation calls through this boundary.
#[async_trait]
pub trait UaiDiscussionRecoveryReadTransport: Send + Sync {
    async fn resolve_discussion_binding(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiDiscussionBinding>;

    async fn find_discussion_topic(
        &self,
        context: &ProviderContext,
        binding: &UaiDiscussionBinding,
    ) -> ProviderResult<Option<u64>>;

    async fn read_discussion_replies(
        &self,
        context: &ProviderContext,
        topic_id: u64,
        page_number: u32,
        page_size: u32,
    ) -> ProviderResult<UaiDiscussionReplyPage>;
}

#[async_trait]
impl<T> UaiDiscussionRecoveryReadTransport for T
where
    T: crate::UaiDiscussionTransport + Send + Sync + ?Sized,
{
    async fn resolve_discussion_binding(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiDiscussionBinding> {
        crate::UaiDiscussionTransport::resolve_discussion_binding(
            self,
            context,
            course_resource_id,
            group_id,
        )
        .await
    }

    async fn find_discussion_topic(
        &self,
        context: &ProviderContext,
        binding: &UaiDiscussionBinding,
    ) -> ProviderResult<Option<u64>> {
        crate::UaiDiscussionTransport::find_discussion_topic(self, context, binding).await
    }

    async fn read_discussion_replies(
        &self,
        context: &ProviderContext,
        topic_id: u64,
        page_number: u32,
        page_size: u32,
    ) -> ProviderResult<UaiDiscussionReplyPage> {
        crate::UaiDiscussionTransport::read_discussion_replies(
            self,
            context,
            topic_id,
            page_number,
            page_size,
        )
        .await
    }
}

/// Fully recovered read-only workflow owner. It contains no sink, HTTP submit
/// callback or method that can issue either remote mutation.
pub struct UaiRecoveredDiscussionWorkflow {
    binding: UaiDiscussionDraftAttemptBinding,
    sequence: UaiDiscussionMutationSequence,
    reply: UaiDiscussionReplyState,
    completion: UaiDiscussionCompletionState,
    recovery_state: UaiDiscussionRecoveryState,
    reply_readback_digest: [u8; 32],
    phase_observation_recorded: bool,
}

impl UaiRecoveredDiscussionWorkflow {
    pub const fn binding(&self) -> &UaiDiscussionDraftAttemptBinding {
        &self.binding
    }

    pub const fn sequence(&self) -> &UaiDiscussionMutationSequence {
        &self.sequence
    }

    pub const fn reply(&self) -> &UaiDiscussionReplyState {
        &self.reply
    }

    pub const fn completion(&self) -> &UaiDiscussionCompletionPlan {
        self.completion.completion()
    }

    pub const fn recovery_state(&self) -> UaiDiscussionRecoveryState {
        self.recovery_state
    }

    pub const fn reply_readback_digest(&self) -> [u8; 32] {
        self.reply_readback_digest
    }

    pub const fn phase_observation_recorded(&self) -> bool {
        self.phase_observation_recorded
    }
}

impl fmt::Debug for UaiRecoveredDiscussionWorkflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiRecoveredDiscussionWorkflow")
            .field("binding", &self.binding)
            .field("sequence", &self.sequence)
            .field("reply", &"[REDACTED]")
            .field("completion", &"[REDACTED]")
            .field("recovery_state", &self.recovery_state)
            .field("reply_readback_digest", &"[HASHED]")
            .field(
                "phase_observation_recorded",
                &self.phase_observation_recorded,
            )
            .finish()
    }
}

/// Provider-owned recovery adapter that consumes only fresh read boundaries
/// and encrypted state. It never dispatches reply or completion mutations.
#[derive(Clone)]
pub struct UaiDiscussionRecovery {
    details: Arc<dyn TaskDetailCapability>,
    reads: Arc<dyn UaiDiscussionRecoveryReadTransport>,
}

impl UaiDiscussionRecovery {
    pub fn new(
        details: Arc<dyn TaskDetailCapability>,
        reads: Arc<dyn UaiDiscussionRecoveryReadTransport>,
    ) -> Self {
        Self { details, reads }
    }

    /// Restores both encrypted Provider states, freshly rebinds route/account,
    /// topic, exact reply evidence and Task detail, then validates the complete
    /// Core sequence snapshot and phase observation.
    ///
    /// # Errors
    ///
    /// Rejects state, route/account, reply, Task, request, observation or
    /// sequence drift. This method performs reads only.
    #[allow(
        clippy::too_many_arguments,
        reason = "the recovery boundary receives every independent persisted authority"
    )]
    pub async fn recover(
        &self,
        context: &ProviderContext,
        scope: &UaiDiscussionAttemptScope,
        binding: &UaiDiscussionDraftAttemptBinding,
        reply_value: &SecretValue,
        reply_state_digest: [u8; 32],
        completion_value: &SecretValue,
        completion_state_digest: [u8; 32],
        sequence: UaiDiscussionMutationSequence,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<UaiRecoveredDiscussionWorkflow> {
        if scope.provider_account_id() != context.account_id {
            return Err(foreign_recovery());
        }
        let reply =
            UaiDiscussionReplyState::decode_bound(reply_value, reply_state_digest, &sequence)?;
        let completion = UaiDiscussionCompletionState::decode_bound(
            completion_value,
            completion_state_digest,
            &reply,
            &sequence,
        )?;
        validate_atomic_stage_outputs(
            snapshot,
            &sequence,
            &reply,
            &completion,
            reply_state_digest,
            completion_state_digest,
        )?;
        binding.validate_before_recovery(
            scope,
            &sequence,
            &reply,
            &completion,
            reply_state_digest,
            completion_state_digest,
        )?;
        let draft = reply.draft();
        let fresh_binding = self
            .reads
            .resolve_discussion_binding(
                context,
                draft.binding().course_resource_id(),
                draft.binding().group_id(),
            )
            .await?;
        if &fresh_binding != draft.binding() {
            return Err(foreign_recovery());
        }
        let fresh_topic = self
            .reads
            .find_discussion_topic(context, &fresh_binding)
            .await?;
        if fresh_topic != Some(draft.topic_id()) {
            return Err(foreign_recovery());
        }
        let verified_page = self
            .find_exact_reply(context, draft.topic_id(), &reply)
            .await?;
        let recovered_completion = completion.completion();
        let detail = self
            .details
            .task_detail(context, recovered_completion.remote_task_id())
            .await?;
        let fresh_completion = prepare_discussion_completion(
            &detail,
            recovered_completion.remote_task_id(),
            draft,
            &verified_page,
        )?;
        if !same_completion_plan(&fresh_completion, recovered_completion) {
            return Err(foreign_recovery());
        }
        let recovery_state = sequence.inspect_recovery(snapshot, Some(recovered_completion))?;
        if !matches!(
            recovery_state,
            UaiDiscussionRecoveryState::ReplyReadbackVerifiedAwaitingGate
                | UaiDiscussionRecoveryState::ReplyReadbackGateRecorded
                | UaiDiscussionRecoveryState::CompletionIssuedAmbiguous
                | UaiDiscussionRecoveryState::CompletionAcceptedAwaitingProgress
        ) {
            return Err(foreign_recovery());
        }
        let reply_readback_digest = snapshot
            .records()
            .first()
            .and_then(asterism_provider_api::ExecutionMutationRecoveryRecord::verification)
            .filter(|verification| verification.verified())
            .map(asterism_provider_api::ExecutionMutationVerification::observation_digest)
            .filter(|digest| *digest != [0; 32])
            .ok_or_else(foreign_recovery)?;
        let phase_observation_recorded = !snapshot.observations().is_empty();
        Ok(UaiRecoveredDiscussionWorkflow {
            binding: binding.clone(),
            sequence,
            reply,
            completion,
            recovery_state,
            reply_readback_digest,
            phase_observation_recorded,
        })
    }

    async fn find_exact_reply(
        &self,
        context: &ProviderContext,
        topic_id: u64,
        reply: &UaiDiscussionReplyState,
    ) -> ProviderResult<UaiDiscussionReplyPage> {
        for page_number in 1..=UAI_DISCUSSION_RECOVERY_MAX_PAGES {
            let page = self
                .reads
                .read_discussion_replies(
                    context,
                    topic_id,
                    page_number,
                    UAI_DISCUSSION_RECOVERY_PAGE_SIZE,
                )
                .await?;
            if page.topic_id() != topic_id {
                return Err(foreign_recovery());
            }
            if page.contains_exact_reply(reply.draft()) {
                return Ok(page);
            }
            if !page.has_more() {
                break;
            }
        }
        Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI discussion recovery could not find the exact reply within its read bound",
        ))
    }
}

fn validate_atomic_stage_outputs(
    snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    sequence: &UaiDiscussionMutationSequence,
    reply: &UaiDiscussionReplyState,
    completion: &UaiDiscussionCompletionState,
    reply_state_digest: [u8; 32],
    completion_state_digest: [u8; 32],
) -> ProviderResult<()> {
    let records = snapshot.records();
    let reply_record = records.first().ok_or_else(foreign_recovery)?;
    if reply_record.receipt().is_some() {
        let stored = UaiDiscussionReplyState::decode_recovery_record(sequence, reply_record)?;
        if reply_record
            .stage_output()
            .map(asterism_provider_api::ExecutionMutationStageOutput::output_digest)
            != Some(reply_state_digest)
            || !stored.same_recovery_authority(reply)
        {
            return Err(foreign_recovery());
        }
    } else if reply_record.stage_output().is_some() {
        return Err(foreign_recovery());
    }
    if let Some(completion_record) = records.get(1) {
        if completion_record.receipt().is_some() {
            let stored = UaiDiscussionCompletionState::decode_recovery_record(
                sequence,
                reply,
                completion_record,
            )?;
            if completion_record
                .stage_output()
                .map(asterism_provider_api::ExecutionMutationStageOutput::output_digest)
                != Some(completion_state_digest)
                || !stored.same_recovery_authority(completion)
            {
                return Err(foreign_recovery());
            }
        } else if completion_record.stage_output().is_some() {
            return Err(foreign_recovery());
        }
    }
    Ok(())
}

impl fmt::Debug for UaiDiscussionRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDiscussionRecovery")
            .field("details", &"configured; read-only")
            .field("reads", &"configured; read-only")
            .finish()
    }
}

fn same_completion_plan(
    left: &UaiDiscussionCompletionPlan,
    right: &UaiDiscussionCompletionPlan,
) -> bool {
    left.remote_task_id() == right.remote_task_id()
        && left.task_fingerprint() == right.task_fingerprint()
        && left.course_resource_id() == right.course_resource_id()
        && left.unit_id() == right.unit_id()
        && left.group_id() == right.group_id()
        && left.topic_id() == right.topic_id()
        && left.reply_request_digest() == right.reply_request_digest()
        && left.reply_digest() == right.reply_digest()
        && left.request_digest() == right.request_digest()
        && left.fingerprint() == right.fingerprint()
}

fn foreign_recovery() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "UAI discussion recovery state is stale or foreign",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AssessmentClass, CourseId, ExecutionAttemptId, ExecutionId, ProviderAccountId, ProviderId,
        RemoteState, SecretId, SourceType, TaskId, UserId,
    };
    use asterism_provider_api::{
        ExecutionMutationIssue, ExecutionMutationReceipt, ExecutionMutationRecoveryRecord,
        ExecutionMutationSequenceObservation, ExecutionMutationVerification, ProviderIdentity,
        ProviderMetadata, RemoteTask, RemoteTaskDetail,
    };
    use async_trait::async_trait;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        EncodedUaiDiscussionCompletionState, EncodedUaiDiscussionReplyState,
        UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE, UaiDiscussionReplyDraft,
        parse_discussion_reply_page,
    };

    #[tokio::test]
    async fn recovery_rebuilds_fresh_reads_and_validates_the_complete_gate() {
        let fixture = Fixture::new();
        let recovery = UaiDiscussionRecovery::new(
            Arc::new(FixtureDetails {
                metadata: crate::development_metadata().unwrap(),
                detail: fixture.detail.clone(),
            }),
            Arc::new(FixtureReads::new(
                fixture.binding.clone(),
                fixture.draft.topic_id(),
                "bounded reply",
            )),
        );
        let recovered = recovery
            .recover(
                &context(fixture.scope.provider_account_id()),
                &fixture.scope,
                &fixture.attempt_binding,
                &fixture.reply_value,
                fixture.reply_state_digest,
                &fixture.completion_value,
                fixture.completion_state_digest,
                fixture.sequence,
                &fixture.snapshot,
            )
            .await
            .unwrap();
        assert_eq!(
            recovered.recovery_state(),
            UaiDiscussionRecoveryState::ReplyReadbackGateRecorded
        );
        assert!(recovered.phase_observation_recorded());
        assert_eq!(recovered.reply_readback_digest(), fixture.readback_digest);
        assert_eq!(
            recovered.completion().request_digest(),
            fixture.completion_request_digest
        );
        assert_eq!(
            recovered.binding().attempt_binding_digest(),
            fixture.attempt_binding.attempt_binding_digest()
        );
        assert!(!format!("{recovered:?}").contains("bounded reply"));
    }

    #[tokio::test]
    async fn recovery_rejects_fresh_account_reply_task_and_observation_drift() {
        for drift in [
            FixtureDrift::Account,
            FixtureDrift::Reply,
            FixtureDrift::Task,
            FixtureDrift::Observation,
        ] {
            let fixture = Fixture::new();
            let binding = match drift {
                FixtureDrift::Account => UaiDiscussionBinding::try_new(
                    "2001",
                    "course-instance-1",
                    "group-discussion",
                    "class-1",
                    "curricula-1",
                    "user-2",
                )
                .unwrap(),
                _ => fixture.binding.clone(),
            };
            let reply_content = if drift == FixtureDrift::Reply {
                "different reply"
            } else {
                "bounded reply"
            };
            let detail = if drift == FixtureDrift::Task {
                task_detail("v1:changed")
            } else {
                fixture.detail.clone()
            };
            let snapshot = if drift == FixtureDrift::Observation {
                recovery_snapshot(
                    &fixture.sequence,
                    &fixture.completion,
                    &fixture.reply_value,
                    fixture.reply_state_digest,
                    [9; 32],
                )
            } else {
                fixture.snapshot.clone()
            };
            let recovery = UaiDiscussionRecovery::new(
                Arc::new(FixtureDetails {
                    metadata: crate::development_metadata().unwrap(),
                    detail,
                }),
                Arc::new(FixtureReads::new(
                    binding,
                    fixture.draft.topic_id(),
                    reply_content,
                )),
            );
            assert!(
                recovery
                    .recover(
                        &context(fixture.scope.provider_account_id()),
                        &fixture.scope,
                        &fixture.attempt_binding,
                        &fixture.reply_value,
                        fixture.reply_state_digest,
                        &fixture.completion_value,
                        fixture.completion_state_digest,
                        fixture.sequence,
                        &snapshot,
                    )
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn foreign_core_binding_fails_before_any_remote_read() {
        let fixture = Fixture::new();
        let reads = Arc::new(FixtureReads::new(
            fixture.binding.clone(),
            fixture.draft.topic_id(),
            "bounded reply",
        ));
        let recovery = UaiDiscussionRecovery::new(
            Arc::new(FixtureDetails {
                metadata: crate::development_metadata().unwrap(),
                detail: fixture.detail.clone(),
            }),
            reads.clone(),
        );
        let foreign_scope = UaiDiscussionAttemptScope::new(
            UserId::new(),
            fixture.scope.provider_account_id(),
            fixture.scope.course_id(),
            fixture.scope.task_id(),
            fixture.scope.execution_id(),
            fixture.scope.execution_attempt_id(),
        );
        assert!(
            recovery
                .recover(
                    &context(fixture.scope.provider_account_id()),
                    &foreign_scope,
                    &fixture.attempt_binding,
                    &fixture.reply_value,
                    fixture.reply_state_digest,
                    &fixture.completion_value,
                    fixture.completion_state_digest,
                    fixture.sequence,
                    &fixture.snapshot,
                )
                .await
                .is_err()
        );
        assert!(reads.calls.lock().unwrap().is_empty());
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum FixtureDrift {
        Account,
        Reply,
        Task,
        Observation,
    }

    struct Fixture {
        scope: UaiDiscussionAttemptScope,
        attempt_binding: UaiDiscussionDraftAttemptBinding,
        binding: UaiDiscussionBinding,
        draft: UaiDiscussionReplyDraft,
        detail: RemoteTaskDetail,
        completion: UaiDiscussionCompletionPlan,
        sequence: UaiDiscussionMutationSequence,
        reply_value: SecretValue,
        reply_state_digest: [u8; 32],
        completion_value: SecretValue,
        completion_state_digest: [u8; 32],
        snapshot: ExecutionMutationSequenceRecoverySnapshot,
        readback_digest: [u8; 32],
        completion_request_digest: [u8; 32],
    }

    impl Fixture {
        fn new() -> Self {
            let binding = UaiDiscussionBinding::try_new(
                "2001",
                "course-instance-1",
                "group-discussion",
                "class-1",
                "curricula-1",
                "user-1",
            )
            .unwrap();
            let draft =
                UaiDiscussionReplyDraft::try_new(binding.clone(), 7_001, "bounded reply").unwrap();
            let detail = task_detail("v1:discussion");
            let page = parse_discussion_reply_page(
                r#"{"success":true,"value":{"replyContents":[{"replyId":9,"createId":"user-1","content":"bounded reply"}]}}"#,
                draft.topic_id(),
                UAI_DISCUSSION_RECOVERY_PAGE_SIZE,
            )
            .unwrap();
            let completion = prepare_discussion_completion(
                &detail,
                "group:2001:unit-1:group-discussion",
                &draft,
                &page,
            )
            .unwrap();
            let completion_request_digest = completion.request_digest();
            let sequence = UaiDiscussionMutationSequence::try_new(&draft).unwrap();
            let encoded_reply = EncodedUaiDiscussionReplyState::try_new(&draft, &sequence).unwrap();
            let reply_state_digest = encoded_reply.digest();
            let reply_value = encoded_reply.into_secret_value();
            let reply_state =
                UaiDiscussionReplyState::decode_bound(&reply_value, reply_state_digest, &sequence)
                    .unwrap();
            let encoded_completion =
                EncodedUaiDiscussionCompletionState::try_new(&completion, &reply_state, &sequence)
                    .unwrap();
            let completion_state_digest = encoded_completion.digest();
            let completion_value = encoded_completion.into_secret_value();
            let completion_state = UaiDiscussionCompletionState::decode_bound(
                &completion_value,
                completion_state_digest,
                &reply_state,
                &sequence,
            )
            .unwrap();
            let scope = UaiDiscussionAttemptScope::new(
                UserId::new(),
                ProviderAccountId::new(),
                CourseId::new(),
                TaskId::new(),
                ExecutionId::new(),
                ExecutionAttemptId::new(),
            );
            let attempt_binding = UaiDiscussionDraftAttemptBinding::try_new(
                &scope,
                &sequence,
                &reply_state,
                &completion_state,
                reply_state_digest,
                completion_state_digest,
            )
            .unwrap();
            let readback_digest = reply_readback_digest(&draft, &completion);
            let snapshot = recovery_snapshot(
                &sequence,
                &completion,
                &reply_value,
                reply_state_digest,
                readback_digest,
            );
            Self {
                scope,
                attempt_binding,
                binding,
                draft,
                detail,
                completion,
                sequence,
                reply_value,
                reply_state_digest,
                completion_value,
                completion_state_digest,
                snapshot,
                readback_digest,
                completion_request_digest,
            }
        }
    }

    #[derive(Clone)]
    struct FixtureDetails {
        metadata: ProviderMetadata,
        detail: RemoteTaskDetail,
    }

    impl ProviderIdentity for FixtureDetails {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetails {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            Ok(self.detail.clone())
        }
    }

    struct FixtureReads {
        binding: UaiDiscussionBinding,
        topic_id: u64,
        reply_content: String,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FixtureReads {
        fn new(binding: UaiDiscussionBinding, topic_id: u64, reply_content: &str) -> Self {
            Self {
                binding,
                topic_id,
                reply_content: reply_content.to_owned(),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl UaiDiscussionRecoveryReadTransport for FixtureReads {
        async fn resolve_discussion_binding(
            &self,
            _context: &ProviderContext,
            _course_resource_id: &str,
            _group_id: &str,
        ) -> ProviderResult<UaiDiscussionBinding> {
            self.calls.lock().unwrap().push("binding");
            Ok(self.binding.clone())
        }

        async fn find_discussion_topic(
            &self,
            _context: &ProviderContext,
            _binding: &UaiDiscussionBinding,
        ) -> ProviderResult<Option<u64>> {
            self.calls.lock().unwrap().push("topic");
            Ok(Some(self.topic_id))
        }

        async fn read_discussion_replies(
            &self,
            _context: &ProviderContext,
            topic_id: u64,
            _page_number: u32,
            page_size: u32,
        ) -> ProviderResult<UaiDiscussionReplyPage> {
            self.calls.lock().unwrap().push("replies");
            parse_discussion_reply_page(
                &serde_json::json!({
                    "success": true,
                    "value": {"replyContents": [{
                        "replyId": 9,
                        "createId": self.binding.current_user_id(),
                        "content": self.reply_content,
                    }]}
                })
                .to_string(),
                topic_id,
                page_size,
            )
        }
    }

    fn recovery_snapshot(
        sequence: &UaiDiscussionMutationSequence,
        completion: &UaiDiscussionCompletionPlan,
        reply_value: &SecretValue,
        reply_state_digest: [u8; 32],
        readback_digest: [u8; 32],
    ) -> ExecutionMutationSequenceRecoverySnapshot {
        let reply = ExecutionMutationRecoveryRecord::try_new(
            ExecutionMutationIssue::new(
                1,
                crate::UAI_DISCUSSION_REPLY_OPERATION_TYPE,
                completion.reply_request_digest(),
            )
            .unwrap(),
            Some(ExecutionMutationReceipt::new(1, [1; 32], true).unwrap()),
            Some(ExecutionMutationVerification::new(1, readback_digest, true).unwrap()),
        )
        .unwrap()
        .try_with_stage_output(
            asterism_provider_api::ExecutionMutationStageOutput::try_new(
                ProviderId::new(crate::metadata::PROVIDER_ID).unwrap(),
                1,
                crate::UAI_DISCUSSION_REPLY_STATE_TYPE,
                reply_state_digest,
                SecretValue::new(reply_value.expose_secret().to_vec()),
            )
            .unwrap(),
        )
        .unwrap();
        ExecutionMutationSequenceRecoverySnapshot::try_new(
            sequence.artifact().clone(),
            sequence.plan().clone(),
            vec![reply],
            vec![
                ExecutionMutationSequenceObservation::try_new(
                    2,
                    UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE,
                    readback_digest,
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    fn reply_readback_digest(
        draft: &UaiDiscussionReplyDraft,
        completion: &UaiDiscussionCompletionPlan,
    ) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"asterism:uai:discussion-reply-readback:v1\0");
        digest.update(draft.request_digest());
        digest.update(completion.reply_digest());
        digest.update(completion.request_digest());
        digest.finalize().into()
    }

    fn task_detail(fingerprint: &str) -> RemoteTaskDetail {
        let normalized = serde_json::json!({
            "schema": "uai.group-task.v1",
            "course_resource_id": "2001",
            "unit": {"id": "unit-1", "title": "Unit 1"},
            "section": {"id": "section-1", "title": "Section 1"},
            "micro": {"id": "micro-1", "title": "Discussion"},
            "group_id": "group-discussion",
            "course_publish_version": 123_290,
            "task_types": ["discussion"],
            "question_count": 1,
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "group:2001:unit-1:group-discussion".to_owned(),
                course_remote_id: Some("course-resource:2001".to_owned()),
                title: "Discussion".to_owned(),
                source_type: SourceType::Resource,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::Unknown,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: fingerprint.to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: serde_json::json!({"schema": "uai.group-task.raw.v1"}),
            },
            normalized_detail: serde_json::json!({
                "schema": "uai.group-task-detail.v1",
                "task": normalized,
            }),
        }
    }

    fn context(account_id: ProviderAccountId) -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id,
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-discussion-recovery".to_owned(),
        }
    }
}
