use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, SubmissionDraft, SubmissionReceipt, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionInvocationPreparationRequest,
    ExecutionMutationSequenceRecoverySnapshot, ExecutionMutationSink, ExecutionOutcome,
    ExecutionRecoveryOutcome, ExecutionRequest, PreparedProviderExecutionInvocation,
    ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionPlanArtifact,
    ProviderExecutionPrivateInput, ProviderResult, TaskDetailCapability, TaskProgressCapability,
};
use asterism_secrets::SecretValue;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{
    EncodedUaiCompoundOralPlanState, EncodedUaiUploadGrantState, EncodedUaiUploadInputState,
    UAI_ARTIFACT_UPLOAD_INPUT_TYPE, UAI_COMPOUND_ORAL_INPUT_TYPE, UAI_DISCUSSION_REPLY_INPUT_TYPE,
    UaiAnswerTransport, UaiCompoundOralPlanState, UaiCompoundOralPreparation,
    UaiCompoundOralResultState, UaiCompoundOralSubmission, UaiCompoundOralSubmissionOutcome,
    UaiCompoundOralSubmissionRequest, UaiCompoundOralSubmissionSequence, UaiCompoundOralTransport,
    UaiCompoundUploadPreparation, UaiCompoundUploadSubmission, UaiCompoundUploadSubmissionRequest,
    UaiCompoundUploadTransport, UaiDiscussionCompletionPlan, UaiDiscussionMutationOutcome,
    UaiDiscussionMutationSequence, UaiDiscussionReplyDraft, UaiDiscussionTransport,
    UaiUploadFinalSubmissionKind, UaiUploadFinalSubmissionOutcome, UaiUploadGrant,
    UaiUploadGrantState, UaiUploadInputState, UaiUploadPreparation, UaiUploadSubmission,
    UaiUploadSubmissionRequest, UaiUploadTransport,
    invocation_input::{
        UaiArtifactUploadInvocationInput, UaiDiscussionInvocationInput,
        validate_compound_oral_authorization,
    },
    metadata::PROVIDER_ID,
    runtime_settings::runtime_settings_schema,
    upload_invocation_sequence::{
        UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE, UaiUploadInvocationSequence,
    },
};

pub(crate) const UAI_UPLOAD_PREPARED_INPUT_TYPE: &str = "uai.artifact-upload.prepared-input.v1";
pub(crate) const UAI_COMPOUND_ORAL_PREPARED_INPUT_TYPE: &str =
    "uai.compound-oral.prepared-input.v1";

const UPLOAD_PREPARED_MAGIC: &[u8] = b"uai.artifact-upload.prepared-input.v1\0";
const ORAL_PREPARED_MAGIC: &[u8] = b"uai.compound-oral.prepared-input.v1\0";
const MAX_DRAFT_BYTES: usize = 4 * 1_024 * 1_024;

#[async_trait]
pub(crate) trait UaiPrivateInvocationTransport:
    UaiDiscussionTransport
    + UaiUploadTransport
    + UaiCompoundUploadTransport
    + UaiCompoundOralTransport
    + Send
    + Sync
{
    async fn materialize_single_upload_request(
        &self,
        context: &ProviderContext,
        submission: &UaiUploadSubmission,
    ) -> ProviderResult<UaiUploadSubmissionRequest>;

    async fn upload_artifact_durable(
        &self,
        context: &ProviderContext,
        grant: &UaiUploadGrant,
        artifact: &crate::UaiUploadArtifact,
        sequence: &UaiUploadInvocationSequence,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<crate::UaiUploadedArtifact>;

    async fn submit_single_upload_durable(
        &self,
        context: &ProviderContext,
        attempt: u32,
        submission: &UaiUploadSubmission,
        sequence: &UaiUploadInvocationSequence,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<(UaiUploadSubmissionRequest, UaiUploadFinalSubmissionOutcome)>;

    async fn materialize_compound_upload_request(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundUploadSubmission,
    ) -> ProviderResult<UaiCompoundUploadSubmissionRequest>;

    async fn submit_compound_upload_durable(
        &self,
        context: &ProviderContext,
        attempt: u32,
        submission: &UaiCompoundUploadSubmission,
        sequence: &UaiUploadInvocationSequence,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<(
        UaiCompoundUploadSubmissionRequest,
        UaiUploadFinalSubmissionOutcome,
    )>;

    async fn materialize_compound_oral_request(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundOralSubmission,
    ) -> ProviderResult<UaiCompoundOralSubmissionRequest>;

    async fn submit_compound_oral_durable(
        &self,
        context: &ProviderContext,
        submission: &UaiCompoundOralSubmission,
        expected_request_digest: [u8; 32],
        sequence: &UaiCompoundOralSubmissionSequence,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<(
        UaiCompoundOralSubmissionRequest,
        UaiCompoundOralSubmissionOutcome,
    )>;

    async fn submit_discussion_reply_durable(
        &self,
        context: &ProviderContext,
        draft: &UaiDiscussionReplyDraft,
        sequence: &UaiDiscussionMutationSequence,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<UaiDiscussionMutationOutcome>;

    async fn submit_discussion_completion_durable(
        &self,
        context: &ProviderContext,
        plan: &UaiDiscussionCompletionPlan,
        gate: &crate::UaiDiscussionReplyReadbackGate,
        sequence: &UaiDiscussionMutationSequence,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<UaiDiscussionMutationOutcome>;
}

pub(crate) struct UaiPrivateInvocation {
    details: Arc<dyn TaskDetailCapability>,
    progress: Arc<dyn TaskProgressCapability>,
    answers: Arc<dyn UaiAnswerTransport>,
    transport: Arc<dyn UaiPrivateInvocationTransport>,
}

impl UaiPrivateInvocation {
    pub(crate) fn new(
        details: Arc<dyn TaskDetailCapability>,
        progress: Arc<dyn TaskProgressCapability>,
        answers: Arc<dyn UaiAnswerTransport>,
        transport: Arc<dyn UaiPrivateInvocationTransport>,
    ) -> Self {
        Self {
            details,
            progress,
            answers,
            transport,
        }
    }

    pub(crate) async fn prepare(
        &self,
        context: &ProviderContext,
        request: &ExecutionInvocationPreparationRequest<'_>,
    ) -> ProviderResult<PreparedProviderExecutionInvocation> {
        validate_preparation(context, request)?;
        match request.requested_capabilities {
            [TaskCapability::Discussion] => self.prepare_discussion(context, request).await,
            [TaskCapability::ArtifactUpload]
            | [
                TaskCapability::SubmissionExecute,
                TaskCapability::ArtifactUpload,
            ] => self.prepare_upload(context, request).await,
            [
                TaskCapability::SubmissionExecute,
                TaskCapability::OralSubmission,
            ] => self.prepare_oral(context, request).await,
            _ => Err(unsupported()),
        }
    }

    async fn prepare_discussion(
        &self,
        context: &ProviderContext,
        request: &ExecutionInvocationPreparationRequest<'_>,
    ) -> ProviderResult<PreparedProviderExecutionInvocation> {
        if request.input_type != UAI_DISCUSSION_REPLY_INPUT_TYPE
            || request.submission_draft.is_some()
        {
            return Err(invalid_input());
        }
        let input = UaiDiscussionInvocationInput::decode(request.raw_input)?;
        let (course_resource_id, group_id) = parse_group_identity(request.remote_task_id)?;
        let binding = self
            .transport
            .resolve_discussion_binding(context, course_resource_id, group_id)
            .await?;
        let topic_id = self
            .transport
            .find_discussion_topic(context, &binding)
            .await?
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::RemoteChanged,
                    "UAI discussion invocation has no current topic",
                )
            })?;
        let draft = UaiDiscussionReplyDraft::try_new(binding, topic_id, input.content())?;
        let sequence = UaiDiscussionMutationSequence::try_new(&draft)?;
        let private_input = ProviderExecutionPrivateInput::try_new(
            provider_id()?,
            UAI_DISCUSSION_REPLY_INPUT_TYPE,
            SecretValue::new(request.raw_input.expose_secret().to_vec()),
        )?;
        PreparedProviderExecutionInvocation::try_new(sequence.artifact().clone(), private_input)
    }

    async fn prepare_upload(
        &self,
        context: &ProviderContext,
        request: &ExecutionInvocationPreparationRequest<'_>,
    ) -> ProviderResult<PreparedProviderExecutionInvocation> {
        if request.input_type != UAI_ARTIFACT_UPLOAD_INPUT_TYPE {
            return Err(invalid_input());
        }
        let compound = request.requested_capabilities
            == [
                TaskCapability::SubmissionExecute,
                TaskCapability::ArtifactUpload,
            ];
        let ordinary_draft = match (compound, request.submission_draft) {
            (false, None) => None,
            (true, Some(draft)) => Some(draft),
            _ => return Err(invalid_input()),
        };
        let input = UaiArtifactUploadInvocationInput::decode(request.raw_input)?;
        let preparation = UaiUploadPreparation::new(self.details.clone());
        let intent = preparation
            .prepare_intent(context, request.remote_task_id, input.artifact())
            .await?;
        let grant = self
            .transport
            .request_upload_grant(context, &intent, input.artifact())
            .await?;
        let encoded_input = match ordinary_draft {
            Some(draft) => EncodedUaiUploadInputState::for_compound(
                request.remote_task_id,
                input.artifact(),
                draft,
            )?,
            None => {
                EncodedUaiUploadInputState::for_single(request.remote_task_id, input.artifact())?
            }
        };
        let input_digest = encoded_input.digest();
        let input_value = encoded_input.into_secret_value();
        let encoded_grant = EncodedUaiUploadGrantState::try_new(&grant)?;
        let grant_digest = encoded_grant.digest();
        let grant_value = encoded_grant.into_secret_value();
        let draft_bytes = encode_optional_draft(ordinary_draft)?;
        let prepared_bytes = encode_upload_prepared(
            input_digest,
            &input_value,
            grant_digest,
            grant.grant_request_digest(),
            grant.grant_response_digest(),
            &grant_value,
            &draft_bytes,
        )?;
        let private_input = ProviderExecutionPrivateInput::try_new(
            provider_id()?,
            UAI_UPLOAD_PREPARED_INPUT_TYPE,
            SecretValue::new(prepared_bytes),
        )?;
        let object_key_digest = Sha256::digest(grant.file_key().as_bytes());
        let artifact = ProviderExecutionPlanArtifact::try_new(
            provider_id()?,
            UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE,
            serde_json::json!({
                "schema": UAI_UPLOAD_INVOCATION_PLAN_ARTIFACT_TYPE,
                "submission_kind": if compound { "compound" } else { "single" },
                "remote_task_id": request.remote_task_id,
                "ordinary_draft_id": ordinary_draft.map(|draft| draft.id.to_string()),
                "artifact_digest": input.artifact().digest(),
                "intent_fingerprint": intent.fingerprint(),
                "upload_position": intent.upload_position(),
                "object_key_digest": hex_digest(object_key_digest.into()),
                "grant_request_digest": hex_digest(grant.grant_request_digest()),
                "grant_response_digest": hex_digest(grant.grant_response_digest()),
                "private_input_digest": hex_digest(private_input.input_digest()),
            }),
        )?;
        UaiUploadInvocationSequence::try_new(artifact.clone())?;
        PreparedProviderExecutionInvocation::try_new(artifact, private_input)
    }

    async fn prepare_oral(
        &self,
        context: &ProviderContext,
        request: &ExecutionInvocationPreparationRequest<'_>,
    ) -> ProviderResult<PreparedProviderExecutionInvocation> {
        if request.input_type != UAI_COMPOUND_ORAL_INPUT_TYPE {
            return Err(invalid_input());
        }
        validate_compound_oral_authorization(request.raw_input)?;
        let draft = request.submission_draft.ok_or_else(invalid_input)?;
        let preparation =
            UaiCompoundOralPreparation::new(self.details.clone(), self.answers.clone());
        let submission = preparation
            .prepare_submission(context, request.remote_task_id, draft)
            .await?;
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission)?;
        let exact_request = self
            .transport
            .materialize_compound_oral_request(context, &submission)
            .await?;
        let encoded = EncodedUaiCompoundOralPlanState::try_new(&submission, &exact_request)?;
        let state_digest = encoded.digest();
        let state = encoded.into_secret_value();
        let draft_bytes = encode_draft(draft)?;
        let private_input = ProviderExecutionPrivateInput::try_new(
            provider_id()?,
            UAI_COMPOUND_ORAL_PREPARED_INPUT_TYPE,
            SecretValue::new(encode_oral_prepared(
                state_digest,
                exact_request.request_digest(),
                &state,
                &draft_bytes,
            )?),
        )?;
        PreparedProviderExecutionInvocation::try_new(sequence.artifact().clone(), private_input)
    }

    pub(crate) async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        validate_execution(context, request, private_input)?;
        let sink = events.mutation_sink().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI private invocation requires the durable Core mutation sink",
            )
        })?;
        match request.requested_capabilities.as_slice() {
            [TaskCapability::Discussion] => {
                self.execute_discussion(context, request, private_input, sink)
                    .await
            }
            [TaskCapability::ArtifactUpload]
            | [
                TaskCapability::SubmissionExecute,
                TaskCapability::ArtifactUpload,
            ] => {
                self.execute_upload(context, request, private_input, sink)
                    .await
            }
            [
                TaskCapability::SubmissionExecute,
                TaskCapability::OralSubmission,
            ] => {
                self.execute_oral(context, request, private_input, sink)
                    .await
            }
            _ => Err(unsupported()),
        }
    }

    async fn execute_discussion(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        let input = UaiDiscussionInvocationInput::decode(private_input.value())?;
        let (course_resource_id, group_id) = parse_group_identity(&request.remote_task_id)?;
        let binding = self
            .transport
            .resolve_discussion_binding(context, course_resource_id, group_id)
            .await?;
        let topic_id = self
            .transport
            .find_discussion_topic(context, &binding)
            .await?
            .ok_or_else(invalid_input)?;
        let draft = UaiDiscussionReplyDraft::try_new(binding, topic_id, input.content())?;
        let sequence = UaiDiscussionMutationSequence::try_new(&draft)?;
        validate_artifact(request, sequence.artifact())?;
        sequence.prepare(sink).await?;
        let reply = self
            .transport
            .submit_discussion_reply_durable(context, &draft, &sequence, sink)
            .await?;
        sequence.record_reply_outcome(&draft, &reply, sink).await?;
        let page = self.find_exact_discussion_reply(context, &draft).await?;
        let detail = self
            .details
            .task_detail(context, &request.remote_task_id)
            .await?;
        let completion =
            crate::prepare_discussion_completion(&detail, &request.remote_task_id, &draft, &page)?;
        let gate = sequence.record_reply_readback(&completion, sink).await?;
        let completed = self
            .transport
            .submit_discussion_completion_durable(context, &completion, &gate, &sequence, sink)
            .await?;
        sequence
            .record_completion_outcome(&completion, &draft, &completed, sink)
            .await?;
        self.verify_progress(context, &request.remote_task_id, Some((2, sink)))
            .await
    }

    async fn find_exact_discussion_reply(
        &self,
        context: &ProviderContext,
        draft: &UaiDiscussionReplyDraft,
    ) -> ProviderResult<crate::UaiDiscussionReplyPage> {
        for page_number in 1..=crate::UAI_DISCUSSION_RECOVERY_MAX_PAGES {
            let page = self
                .transport
                .read_discussion_replies(
                    context,
                    draft.topic_id(),
                    page_number,
                    crate::UAI_DISCUSSION_RECOVERY_PAGE_SIZE,
                )
                .await?;
            if page.contains_exact_reply(draft) {
                return Ok(page);
            }
            if !page.has_more() {
                break;
            }
        }
        Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI discussion reply was not found by exact readback",
        ))
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_upload(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if private_input.input_type() != UAI_UPLOAD_PREPARED_INPUT_TYPE {
            return Err(invalid_input());
        }
        let artifact = request
            .provider_plan_artifact
            .as_ref()
            .ok_or_else(invalid_input)?;
        let sequence = UaiUploadInvocationSequence::try_new(artifact.clone())?;
        let payload = artifact.payload_sanitized();
        if payload
            .get("private_input_digest")
            .and_then(serde_json::Value::as_str)
            != Some(hex_digest(private_input.input_digest()).as_str())
            || payload
                .get("remote_task_id")
                .and_then(serde_json::Value::as_str)
                != Some(request.remote_task_id.as_str())
        {
            return Err(invalid_input());
        }
        let decoded = decode_upload_prepared(private_input.value())?;
        let artifact_digest = required_plan_text(payload, "artifact_digest")?;
        let compound = request.requested_capabilities
            == [
                TaskCapability::SubmissionExecute,
                TaskCapability::ArtifactUpload,
            ];
        let draft = decoded.decode_draft(compound, request.task_id)?;
        let input = match draft.as_ref() {
            Some(draft) => UaiUploadInputState::decode_compound_bound(
                &decoded.input,
                decoded.input_digest,
                &request.remote_task_id,
                artifact_digest,
                draft,
            )?,
            None => UaiUploadInputState::decode_single_bound(
                &decoded.input,
                decoded.input_digest,
                &request.remote_task_id,
                artifact_digest,
            )?,
        };
        let preparation = UaiUploadPreparation::new(self.details.clone());
        let intent = preparation
            .prepare_intent(context, &request.remote_task_id, input.artifact())
            .await?;
        if payload
            .get("intent_fingerprint")
            .and_then(serde_json::Value::as_str)
            != Some(intent.fingerprint())
            || payload
                .get("upload_position")
                .and_then(serde_json::Value::as_u64)
                != Some(u64::from(intent.upload_position()))
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload invocation Task changed before object issue",
            ));
        }
        let grant = UaiUploadGrantState::decode_bound(
            &decoded.grant,
            decoded.grant_digest,
            decoded.grant_request_digest,
            decoded.grant_response_digest,
        )?
        .into_grant();
        validate_grant(&grant, &intent, input.artifact())?;
        sequence.prepare(sink).await?;
        let uploaded = self
            .transport
            .upload_artifact_durable(context, &grant, input.artifact(), &sequence, sink)
            .await?;
        crate::record_accepted_upload_object(1, &uploaded, sink).await?;

        let (kind, single, compound_submission) = if let Some(draft) = draft.as_ref() {
            let submission = UaiCompoundUploadPreparation::new(self.details.clone())
                .prepare_submission(context, draft, &uploaded)
                .await?;
            (
                UaiUploadFinalSubmissionKind::Compound,
                None,
                Some(submission),
            )
        } else {
            let submission = preparation.prepare_submission(context, &uploaded).await?;
            (UaiUploadFinalSubmissionKind::Single, Some(submission), None)
        };
        for attempt in 1..=2 {
            let (request_digest, sequence_binding_digest, outcome) =
                match (&single, &compound_submission) {
                    (Some(submission), None) => {
                        let (exact_request, outcome) = self
                            .transport
                            .submit_single_upload_durable(
                                context, attempt, submission, &sequence, sink,
                            )
                            .await?;
                        (
                            exact_request.request_digest(),
                            submission.final_sequence_binding_digest(),
                            outcome,
                        )
                    }
                    (None, Some(submission)) => {
                        let (exact_request, outcome) = self
                            .transport
                            .submit_compound_upload_durable(
                                context, attempt, submission, &sequence, sink,
                            )
                            .await?;
                        (
                            exact_request.request_digest(),
                            submission.final_sequence_binding_digest(),
                            outcome,
                        )
                    }
                    _ => return Err(invalid_input()),
                };
            let ordinal = attempt + 1;
            let accepted = sequence
                .record_final(
                    ordinal,
                    kind,
                    sequence_binding_digest,
                    request_digest,
                    &outcome,
                    sink,
                )
                .await?;
            if !accepted {
                continue;
            }
            let receipt = outcome.receipt().ok_or_else(invalid_input)?;
            let result_digest = match (&single, &compound_submission, kind) {
                (Some(submission), None, UaiUploadFinalSubmissionKind::Single) => self
                    .transport
                    .verify_uploaded_artifact(context, submission, receipt)
                    .await?
                    .result_digest(),
                (None, Some(submission), UaiUploadFinalSubmissionKind::Compound) => self
                    .transport
                    .verify_compound_upload(context, submission, receipt)
                    .await?
                    .result_digest(),
                _ => return Err(invalid_input()),
            };
            sink.record_verification(asterism_provider_api::ExecutionMutationVerification::new(
                ordinal,
                result_digest,
                true,
            )?)
            .await?;
            return self
                .verify_progress(context, &request.remote_task_id, None)
                .await;
        }
        Err(ProviderError::new(
            ProviderErrorKind::ProviderUnavailable,
            "UAI upload final submission exhausted its evidenced retry bound",
        ))
    }

    async fn execute_oral(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        sink: &(dyn ExecutionMutationSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if private_input.input_type() != UAI_COMPOUND_ORAL_PREPARED_INPUT_TYPE {
            return Err(invalid_input());
        }
        let decoded = decode_oral_prepared(private_input.value())?;
        let draft = decode_required_draft(&decoded.draft, request.task_id)?;
        let preparation =
            UaiCompoundOralPreparation::new(self.details.clone(), self.answers.clone());
        let submission = preparation
            .prepare_submission(context, &request.remote_task_id, &draft)
            .await?;
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission)?;
        validate_artifact(request, sequence.artifact())?;
        let stored = UaiCompoundOralPlanState::decode_bound(
            &decoded.state,
            decoded.state_digest,
            &sequence,
            decoded.request_digest,
        )?;
        if stored.submission().plan_binding_digest()? != submission.plan_binding_digest()? {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI compound oral evidence changed before issue",
            ));
        }
        sequence.prepare(sink).await?;
        let (exact_request, outcome) = self
            .transport
            .submit_compound_oral_durable(
                context,
                &submission,
                decoded.request_digest,
                &sequence,
                sink,
            )
            .await?;
        sequence
            .record_accepted_outcome(&exact_request, &outcome, sink)
            .await?;
        let verification = self
            .transport
            .verify_compound_oral(context, &submission, outcome.receipt())
            .await?;
        sink.record_verification(asterism_provider_api::ExecutionMutationVerification::new(
            1,
            verification.result_digest(),
            true,
        )?)
        .await?;
        self.verify_progress(context, &request.remote_task_id, None)
            .await
    }

    pub(crate) async fn verify(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
    ) -> ProviderResult<ExecutionOutcome> {
        validate_execution(context, request, private_input)?;
        self.verify_progress(context, &request.remote_task_id, None)
            .await
    }

    pub(crate) async fn recover(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        snapshot: Option<&ExecutionMutationSequenceRecoverySnapshot>,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        validate_execution(context, request, private_input)?;
        let snapshot = snapshot.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI private invocation recovery requires its exact mutation sequence",
            )
        })?;
        match request.requested_capabilities.as_slice() {
            [TaskCapability::Discussion] => {
                self.recover_discussion(context, request, private_input, snapshot)
                    .await
            }
            [TaskCapability::ArtifactUpload]
            | [
                TaskCapability::SubmissionExecute,
                TaskCapability::ArtifactUpload,
            ] => {
                self.recover_upload(context, request, private_input, snapshot)
                    .await
            }
            [
                TaskCapability::SubmissionExecute,
                TaskCapability::OralSubmission,
            ] => {
                self.recover_oral(context, request, private_input, snapshot)
                    .await
            }
            _ => Err(unsupported()),
        }
    }

    async fn recover_discussion(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        let input = UaiDiscussionInvocationInput::decode(private_input.value())?;
        let (course_resource_id, group_id) = parse_group_identity(&request.remote_task_id)?;
        let binding = self
            .transport
            .resolve_discussion_binding(context, course_resource_id, group_id)
            .await?;
        let topic_id = self
            .transport
            .find_discussion_topic(context, &binding)
            .await?
            .ok_or_else(invalid_input)?;
        let draft = UaiDiscussionReplyDraft::try_new(binding, topic_id, input.content())?;
        let sequence = UaiDiscussionMutationSequence::try_new(&draft)?;
        validate_artifact(request, sequence.artifact())?;
        let completion = if snapshot.records().first().is_some_and(|record| {
            record
                .receipt()
                .is_some_and(asterism_provider_api::ExecutionMutationReceipt::accepted)
        }) {
            let page = self.find_exact_discussion_reply(context, &draft).await?;
            let detail = self
                .details
                .task_detail(context, &request.remote_task_id)
                .await?;
            Some(crate::prepare_discussion_completion(
                &detail,
                &request.remote_task_id,
                &draft,
                &page,
            )?)
        } else {
            None
        };
        let state = sequence.inspect_recovery(snapshot, completion.as_ref())?;
        let (outcome, progress_digest) = self
            .read_progress_verification(context, &request.remote_task_id)
            .await?;
        if matches!(
            state,
            crate::UaiDiscussionRecoveryState::CompletionAcceptedAwaitingProgress
        ) {
            let verified = outcome.verified;
            attach_recovery_verification(snapshot, outcome, progress_digest, verified)
        } else {
            Ok(ExecutionRecoveryOutcome::new(outcome))
        }
    }

    async fn recover_upload(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        if private_input.input_type() != UAI_UPLOAD_PREPARED_INPUT_TYPE {
            return Err(invalid_input());
        }
        let artifact = request
            .provider_plan_artifact
            .as_ref()
            .ok_or_else(invalid_input)?;
        let sequence = UaiUploadInvocationSequence::try_new(artifact.clone())?;
        let (object, result) = sequence.inspect_completed(snapshot)?;
        let decoded = decode_upload_prepared(private_input.value())?;
        let payload = artifact.payload_sanitized();
        let artifact_digest = required_plan_text(payload, "artifact_digest")?;
        let compound = request.requested_capabilities
            == [
                TaskCapability::SubmissionExecute,
                TaskCapability::ArtifactUpload,
            ];
        let draft = decoded.decode_draft(compound, request.task_id)?;
        let input = match draft.as_ref() {
            Some(draft) => UaiUploadInputState::decode_compound_bound(
                &decoded.input,
                decoded.input_digest,
                &request.remote_task_id,
                artifact_digest,
                draft,
            )?,
            None => UaiUploadInputState::decode_single_bound(
                &decoded.input,
                decoded.input_digest,
                &request.remote_task_id,
                artifact_digest,
            )?,
        };
        let preparation = UaiUploadPreparation::new(self.details.clone());
        let intent = preparation
            .prepare_intent(context, &request.remote_task_id, input.artifact())
            .await?;
        let grant = UaiUploadGrantState::decode_bound(
            &decoded.grant,
            decoded.grant_digest,
            decoded.grant_request_digest,
            decoded.grant_response_digest,
        )?
        .into_grant();
        validate_grant(&grant, &intent, input.artifact())?;
        validate_uploaded_object(object.uploaded(), &grant, input.artifact())?;
        let (result_digest, exact_request_digest) = match draft.as_ref() {
            Some(draft) if result.kind() == UaiUploadFinalSubmissionKind::Compound => {
                let submission = UaiCompoundUploadPreparation::new(self.details.clone())
                    .prepare_submission(context, draft, object.uploaded())
                    .await?;
                let exact_request = self
                    .transport
                    .materialize_compound_upload_request(context, &submission)
                    .await?;
                let verification = self
                    .transport
                    .verify_compound_upload(context, &submission, &result.receipt()?)
                    .await?;
                (verification.result_digest(), exact_request.request_digest())
            }
            None if result.kind() == UaiUploadFinalSubmissionKind::Single => {
                let submission = preparation
                    .prepare_submission(context, object.uploaded())
                    .await?;
                let exact_request = self
                    .transport
                    .materialize_single_upload_request(context, &submission)
                    .await?;
                let verification = self
                    .transport
                    .verify_uploaded_artifact(context, &submission, &result.receipt()?)
                    .await?;
                (verification.result_digest(), exact_request.request_digest())
            }
            _ => return Err(invalid_input()),
        };
        if exact_request_digest != result.request_digest() {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "UAI upload final request changed during recovery",
            ));
        }
        let outcome = self
            .verify_progress(context, &request.remote_task_id, None)
            .await?;
        attach_recovery_verification(snapshot, outcome, result_digest, true)
    }

    async fn recover_oral(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        private_input: &ProviderExecutionPrivateInput,
        snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        if private_input.input_type() != UAI_COMPOUND_ORAL_PREPARED_INPUT_TYPE {
            return Err(invalid_input());
        }
        let decoded = decode_oral_prepared(private_input.value())?;
        let draft = decode_required_draft(&decoded.draft, request.task_id)?;
        let preparation =
            UaiCompoundOralPreparation::new(self.details.clone(), self.answers.clone());
        let submission = preparation
            .prepare_submission(context, &request.remote_task_id, &draft)
            .await?;
        let sequence = UaiCompoundOralSubmissionSequence::try_new(&submission)?;
        validate_artifact(request, sequence.artifact())?;
        if snapshot.artifact() != sequence.artifact()
            || snapshot.plan() != sequence.plan()
            || snapshot.records().len() != 1
            || !snapshot.observations().is_empty()
        {
            return Err(invalid_input());
        }
        let plan_state = UaiCompoundOralPlanState::decode_bound(
            &decoded.state,
            decoded.state_digest,
            &sequence,
            decoded.request_digest,
        )?;
        if plan_state.submission().plan_binding_digest()? != submission.plan_binding_digest()? {
            return Err(invalid_input());
        }
        let exact_request = self
            .transport
            .materialize_compound_oral_request(context, &submission)
            .await?;
        plan_state.validate_request(&exact_request)?;
        let result =
            UaiCompoundOralResultState::decode_recovery_record(&sequence, &snapshot.records()[0])?;
        let receipt = SubmissionReceipt {
            remote_status: "accepted".to_owned(),
            message_sanitized: Some("UAI accepted the compound oral invocation".to_owned()),
            provider_trace_id: Some(result.submission_version().to_owned()),
            received_at: result.accepted_at(),
        };
        receipt.validate().map_err(|_| invalid_input())?;
        let verification = self
            .transport
            .verify_compound_oral(context, &submission, &receipt)
            .await?;
        let outcome = self
            .verify_progress(context, &request.remote_task_id, None)
            .await?;
        attach_recovery_verification(snapshot, outcome, verification.result_digest(), true)
    }

    async fn verify_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        mutation: Option<(u32, &(dyn ExecutionMutationSink + Send + Sync))>,
    ) -> ProviderResult<ExecutionOutcome> {
        let (outcome, digest) = self
            .read_progress_verification(context, remote_task_id)
            .await?;
        if let Some((ordinal, sink)) = mutation {
            sink.record_verification(asterism_provider_api::ExecutionMutationVerification::new(
                ordinal,
                digest,
                outcome.verified,
            )?)
            .await?;
        }
        Ok(outcome)
    }

    async fn read_progress_verification(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<(ExecutionOutcome, [u8; 32])> {
        let progress = self.progress.read_progress(context, remote_task_id).await?;
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&progress).map_err(|_| {
            ProviderError::new(ProviderErrorKind::Internal, "UAI progress is not encodable")
        })?)
        .into();
        let verified = progress.remote_state == RemoteState::Completed;
        Ok((
            ExecutionOutcome {
                remote_state: progress.remote_state,
                verified,
                result_sanitized: serde_json::json!({
                    "schema": "uai.private-invocation-verification.v1",
                    "goal_matched": verified,
                    "verification": "fresh_exact_group_progress",
                }),
            },
            digest,
        ))
    }
}

impl fmt::Debug for UaiPrivateInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiPrivateInvocation")
            .field("boundaries", &"configured")
            .finish()
    }
}

fn validate_preparation(
    context: &ProviderContext,
    request: &ExecutionInvocationPreparationRequest<'_>,
) -> ProviderResult<()> {
    if context.provider_id.as_str() != PROVIDER_ID
        || context.credential_refs.is_empty()
        || request.task_id
            != request
                .submission_draft
                .map_or(request.task_id, |draft| draft.task_id)
    {
        return Err(invalid_input());
    }
    runtime_settings_schema()
        .validate_resolved(request.runtime_settings)
        .map_err(|_| invalid_input())
}

fn validate_execution(
    context: &ProviderContext,
    request: &ExecutionRequest,
    private_input: &ProviderExecutionPrivateInput,
) -> ProviderResult<()> {
    if context.provider_id.as_str() != PROVIDER_ID
        || context.credential_refs.is_empty()
        || private_input.provider_id().as_str() != PROVIDER_ID
        || !request.has_valid_capability_step()
        || request
            .provider_plan_artifact
            .as_ref()
            .is_none_or(|artifact| artifact.provider_id().as_str() != PROVIDER_ID)
    {
        return Err(invalid_input());
    }
    runtime_settings_schema()
        .validate_resolved(&request.runtime_settings)
        .map_err(|_| invalid_input())
}

fn validate_artifact(
    request: &ExecutionRequest,
    expected: &ProviderExecutionPlanArtifact,
) -> ProviderResult<()> {
    if request.provider_plan_artifact.as_ref() == Some(expected) {
        Ok(())
    } else {
        Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI private invocation plan changed before execution",
        ))
    }
}

fn parse_group_identity(remote_task_id: &str) -> ProviderResult<(&str, &str)> {
    let mut parts = remote_task_id.split(':');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some("group"), Some(course), Some(_unit), Some(group), None)
            if !course.is_empty() && !group.is_empty() =>
        {
            Ok((course, group))
        }
        _ => Err(invalid_input()),
    }
}

fn encode_optional_draft(draft: Option<&SubmissionDraft>) -> ProviderResult<Zeroizing<Vec<u8>>> {
    match draft {
        Some(draft) => encode_draft(draft),
        None => Ok(Zeroizing::new(Vec::new())),
    }
}

fn encode_draft(draft: &SubmissionDraft) -> ProviderResult<Zeroizing<Vec<u8>>> {
    let bytes = serde_json::to_vec(draft).map_err(|_| invalid_input())?;
    if bytes.is_empty() || bytes.len() > MAX_DRAFT_BYTES {
        return Err(invalid_input());
    }
    Ok(Zeroizing::new(bytes))
}

#[allow(clippy::too_many_arguments)]
fn encode_upload_prepared(
    input_digest: [u8; 32],
    input: &SecretValue,
    grant_digest: [u8; 32],
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
    grant: &SecretValue,
    draft: &[u8],
) -> ProviderResult<Vec<u8>> {
    let mut encoded = Zeroizing::new(Vec::with_capacity(
        input
            .expose_secret()
            .len()
            .saturating_add(grant.expose_secret().len())
            .saturating_add(draft.len())
            .saturating_add(256),
    ));
    encoded.extend_from_slice(UPLOAD_PREPARED_MAGIC);
    encoded.extend_from_slice(&input_digest);
    push_bytes(&mut encoded, input.expose_secret())?;
    encoded.extend_from_slice(&grant_digest);
    encoded.extend_from_slice(&grant_request_digest);
    encoded.extend_from_slice(&grant_response_digest);
    push_bytes(&mut encoded, grant.expose_secret())?;
    push_bytes(&mut encoded, draft)?;
    Ok(std::mem::take(&mut *encoded))
}

fn encode_oral_prepared(
    state_digest: [u8; 32],
    request_digest: [u8; 32],
    state: &SecretValue,
    draft: &[u8],
) -> ProviderResult<Vec<u8>> {
    let mut encoded = Zeroizing::new(Vec::with_capacity(
        state
            .expose_secret()
            .len()
            .saturating_add(draft.len())
            .saturating_add(160),
    ));
    encoded.extend_from_slice(ORAL_PREPARED_MAGIC);
    encoded.extend_from_slice(&state_digest);
    encoded.extend_from_slice(&request_digest);
    push_bytes(&mut encoded, state.expose_secret())?;
    push_bytes(&mut encoded, draft)?;
    Ok(std::mem::take(&mut *encoded))
}

fn push_bytes(target: &mut Vec<u8>, bytes: &[u8]) -> ProviderResult<()> {
    let length = u32::try_from(bytes.len()).map_err(|_| invalid_input())?;
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(bytes);
    Ok(())
}

struct DecodedUploadPrepared {
    input_digest: [u8; 32],
    input: SecretValue,
    grant_digest: [u8; 32],
    grant_request_digest: [u8; 32],
    grant_response_digest: [u8; 32],
    grant: SecretValue,
    draft: Zeroizing<Vec<u8>>,
}

impl DecodedUploadPrepared {
    fn decode_draft(
        &self,
        compound: bool,
        task_id: asterism_domain::TaskId,
    ) -> ProviderResult<Option<SubmissionDraft>> {
        match (compound, self.draft.is_empty()) {
            (false, true) => Ok(None),
            (true, false) => decode_required_draft(&self.draft, task_id).map(Some),
            _ => Err(invalid_input()),
        }
    }
}

struct DecodedOralPrepared {
    state_digest: [u8; 32],
    request_digest: [u8; 32],
    state: SecretValue,
    draft: Zeroizing<Vec<u8>>,
}

fn decode_upload_prepared(value: &SecretValue) -> ProviderResult<DecodedUploadPrepared> {
    let mut reader = PrivateReader::new(value.expose_secret());
    reader.expect_magic(UPLOAD_PREPARED_MAGIC)?;
    let input_digest = reader.read_array_32()?;
    let input = SecretValue::new(reader.read_bytes()?.to_vec());
    let grant_digest = reader.read_array_32()?;
    let grant_request_digest = reader.read_array_32()?;
    let grant_response_digest = reader.read_array_32()?;
    let grant = SecretValue::new(reader.read_bytes()?.to_vec());
    let draft = Zeroizing::new(reader.read_bytes()?.to_vec());
    if !reader.finished()
        || [
            input_digest,
            grant_digest,
            grant_request_digest,
            grant_response_digest,
        ]
        .contains(&[0; 32])
    {
        return Err(invalid_input());
    }
    Ok(DecodedUploadPrepared {
        input_digest,
        input,
        grant_digest,
        grant_request_digest,
        grant_response_digest,
        grant,
        draft,
    })
}

fn decode_oral_prepared(value: &SecretValue) -> ProviderResult<DecodedOralPrepared> {
    let mut reader = PrivateReader::new(value.expose_secret());
    reader.expect_magic(ORAL_PREPARED_MAGIC)?;
    let state_digest = reader.read_array_32()?;
    let request_digest = reader.read_array_32()?;
    let state = SecretValue::new(reader.read_bytes()?.to_vec());
    let draft = Zeroizing::new(reader.read_bytes()?.to_vec());
    if !reader.finished() || [state_digest, request_digest].contains(&[0; 32]) || draft.is_empty() {
        return Err(invalid_input());
    }
    Ok(DecodedOralPrepared {
        state_digest,
        request_digest,
        state,
        draft,
    })
}

fn decode_required_draft(
    bytes: &[u8],
    task_id: asterism_domain::TaskId,
) -> ProviderResult<SubmissionDraft> {
    if bytes.is_empty() || bytes.len() > MAX_DRAFT_BYTES {
        return Err(invalid_input());
    }
    let draft: SubmissionDraft = serde_json::from_slice(bytes).map_err(|_| invalid_input())?;
    if draft.task_id != task_id
        || draft.provider_id.as_str() != PROVIDER_ID
        || draft.provider_version != env!("CARGO_PKG_VERSION")
        || draft.validate().is_err()
    {
        return Err(invalid_input());
    }
    Ok(draft)
}

fn validate_grant(
    grant: &UaiUploadGrant,
    intent: &crate::UaiUploadIntent,
    artifact: &crate::UaiUploadArtifact,
) -> ProviderResult<()> {
    if grant.remote_task_id() != intent.remote_task_id()
        || grant.task_fingerprint() != intent.task_fingerprint()
        || grant.course_resource_id() != intent.course_resource_id()
        || grant.unit_id() != intent.unit_id()
        || grant.group_id() != intent.group_id()
        || grant.upload_position() != intent.upload_position()
        || grant.intent_fingerprint() != intent.fingerprint()
        || grant.artifact_digest() != artifact.digest()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI recovered upload grant is foreign to the fresh intent",
        ));
    }
    Ok(())
}

fn validate_uploaded_object(
    uploaded: &crate::UaiUploadedArtifact,
    grant: &UaiUploadGrant,
    artifact: &crate::UaiUploadArtifact,
) -> ProviderResult<()> {
    let multipart = crate::build_upload_multipart(grant, artifact)?;
    if uploaded.remote_task_id() != grant.remote_task_id()
        || uploaded.task_fingerprint() != grant.task_fingerprint()
        || uploaded.course_resource_id() != grant.course_resource_id()
        || uploaded.unit_id() != grant.unit_id()
        || uploaded.group_id() != grant.group_id()
        || uploaded.upload_position() != grant.upload_position()
        || uploaded.file_key() != grant.file_key()
        || uploaded.artifact_digest() != grant.artifact_digest()
        || uploaded.intent_fingerprint() != grant.intent_fingerprint()
        || uploaded.object_request_digest() != multipart.request_digest()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "UAI recovered object result is foreign to the prepared upload",
        ));
    }
    Ok(())
}

fn attach_recovery_verification(
    snapshot: &ExecutionMutationSequenceRecoverySnapshot,
    outcome: ExecutionOutcome,
    observation_digest: [u8; 32],
    verified: bool,
) -> ProviderResult<ExecutionRecoveryOutcome> {
    let ordinal = snapshot
        .final_accepted_mutation_ordinal()
        .ok_or_else(invalid_input)?;
    let record = snapshot.records().last().ok_or_else(invalid_input)?;
    if let Some(existing) = record.verification() {
        if existing.ordinal() != ordinal
            || existing.observation_digest() != observation_digest
            || existing.verified() != verified
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI recovered mutation verification changed",
            ));
        }
        Ok(ExecutionRecoveryOutcome::new(outcome))
    } else {
        Ok(ExecutionRecoveryOutcome::with_mutation_verification(
            outcome,
            asterism_provider_api::ExecutionMutationVerification::new(
                ordinal,
                observation_digest,
                verified,
            )?,
        ))
    }
}

fn required_plan_text<'a>(payload: &'a serde_json::Value, field: &str) -> ProviderResult<&'a str> {
    payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid_input)
}

struct PrivateReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> PrivateReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn expect_magic(&mut self, magic: &[u8]) -> ProviderResult<()> {
        if self.take(magic.len())? == magic {
            Ok(())
        } else {
            Err(invalid_input())
        }
    }

    fn read_array_32(&mut self) -> ProviderResult<[u8; 32]> {
        self.take(32)?.try_into().map_err(|_| invalid_input())
    }

    fn read_bytes(&mut self) -> ProviderResult<&'a [u8]> {
        let length: [u8; 4] = self.take(4)?.try_into().map_err(|_| invalid_input())?;
        let length = usize::try_from(u32::from_be_bytes(length)).map_err(|_| invalid_input())?;
        self.take(length)
    }

    fn take(&mut self, length: usize) -> ProviderResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(invalid_input)?;
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    const fn finished(&self) -> bool {
        self.position == self.bytes.len()
    }
}

fn provider_id() -> ProviderResult<asterism_domain::ProviderId> {
    asterism_domain::ProviderId::new(PROVIDER_ID)
        .map_err(|_| ProviderError::new(ProviderErrorKind::Internal, "UAI Provider ID is invalid"))
}

fn hex_digest(digest: [u8; 32]) -> String {
    let mut result = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn invalid_input() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "UAI private invocation input or binding is invalid",
    )
}

fn unsupported() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::UnsupportedTask,
        "UAI private invocation capability selection is unsupported",
    )
}
