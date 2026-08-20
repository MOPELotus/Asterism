use std::{fmt, sync::Arc};

use asterism_domain::{
    NormalizedAnswer, QuestionId, SubmissionDraft, SubmissionReceipt, TaskCapability, Timestamp,
};
use asterism_provider_api::{
    AmbiguousProviderQuestionSessionOperation, ExecutionEventSink,
    PreparedProviderSubmissionOperation, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderQuestionOperationArtifact, ProviderResult,
    ProviderSubmissionStepOutcome, ResolvedProviderQuestionSessionContinuation,
    ResolvedProviderRuntimeSettings, SubmissionBuildCapability, SubmissionExecuteCapability,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::attempt_flow::{terminal_result_artifact, validate_mutation_recovery_artifact};
use crate::question_inventory::{map_pre_question_continuation, map_question_materialization};
use crate::{
    CIDAREN_PRE_QUESTION_ARTIFACT_TYPE, CIDAREN_QUESTION_ARTIFACT_PHASE,
    CIDAREN_QUESTION_ARTIFACT_TYPE, CIDAREN_READING_CARD_PHASE, CIDAREN_READY_TO_ADVANCE_PHASE,
    CIDAREN_READY_TO_SELECT_WORDS_PHASE, CIDAREN_READY_TO_START_PHASE,
    CIDAREN_READY_TO_VERIFY_PHASE, CidarenAttemptFlow, CidarenAttemptFlowStatus,
    CidarenDurableStepOutcome, CidarenIssuedCommand, CidarenPreQuestionArtifact,
    CidarenQuestionArtifact, CidarenQuestionInventory, CidarenRuntimeSettings,
    CidarenSubmissionBuild, metadata::development_metadata,
    submission_build::has_complete_answer_coverage,
};

/// Durable post-materialization assessment executor.
///
/// Cidaren exposes only one current Question. Each call freezes exactly one
/// donor mutation from the encrypted session artifact and immutable Draft;
/// accepted responses either rotate that session, materialize the next
/// Question, or close it with the donor's definite completion receipt.
pub struct CidarenSubmissionExecute {
    metadata: ProviderMetadata,
    preview: CidarenSubmissionBuild,
    questions: Arc<CidarenQuestionInventory>,
}

impl CidarenSubmissionExecute {
    /// Builds execution around the same fresh Task/evidence/assessment
    /// dependencies used by the pre-Question flow.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(questions: Arc<CidarenQuestionInventory>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            preview: CidarenSubmissionBuild::try_new()?,
            questions,
        })
    }

    async fn validate_draft(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
    ) -> ProviderResult<()> {
        if draft.provider_id != self.metadata.id
            || draft.provider_version != self.metadata.implementation_version
            || draft.validate().is_err()
        {
            return Err(invalid_input(
                "Cidaren submission execution received an invalid or stale Draft",
            ));
        }
        let [item] = draft.items.as_slice() else {
            return Err(invalid_input(
                "Cidaren submission execution requires one current Question",
            ));
        };
        if !has_complete_answer_coverage(&draft.answer_coverage, draft.items.len()) {
            return Err(invalid_input(
                "Cidaren submission execution requires complete answer coverage",
            ));
        }
        let expected = self
            .preview
            .build_submission_preview(
                context,
                remote_task_id,
                std::slice::from_ref(&item.question),
                std::slice::from_ref(&item.selected),
            )
            .await?;
        if expected != draft.payload_preview {
            return Err(invalid_input(
                "Cidaren submission execution Draft preview is stale or foreign",
            ));
        }
        Ok(())
    }

    pub(crate) async fn prepare_submission_operation_at(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        runtime_settings: &ResolvedProviderRuntimeSettings,
        issued_at: Timestamp,
    ) -> ProviderResult<PreparedCidarenSubmissionOperation> {
        validate_context(context, &self.metadata)?;
        self.validate_draft(context, remote_task_id, draft).await?;
        let settings = CidarenRuntimeSettings::resolve(runtime_settings)?;
        let item = &draft.items[0];
        validate_continuation_shape(&continuation)?;
        let (detail, selection) = self
            .questions
            .fresh_detail_and_phase_selection(context, remote_task_id, continuation.phase)
            .await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::SubmissionExecute)
        {
            return Err(remote_changed(
                "Cidaren fresh Task no longer authorizes submission execution",
            ));
        }

        let mut flow = match continuation.continuation_type {
            CIDAREN_QUESTION_ARTIFACT_TYPE => {
                if selection.is_some() {
                    return Err(protocol_drift(
                        "Cidaren Question artifact unexpectedly requested word selection",
                    ));
                }
                let artifact = CidarenQuestionArtifact::decode_bound(
                    continuation.value,
                    continuation.continuation_digest,
                    remote_task_id,
                    &item.question,
                )?;
                CidarenAttemptFlow::restore_question(
                    context,
                    remote_task_id,
                    &detail,
                    &artifact,
                    continuation.phase,
                    &item.question,
                    Some(&item.selected),
                )?
            }
            CIDAREN_PRE_QUESTION_ARTIFACT_TYPE => {
                let artifact = CidarenPreQuestionArtifact::decode_bound(
                    continuation.value,
                    continuation.continuation_digest,
                    draft.task_id,
                    remote_task_id,
                )?;
                if artifact.phase() != continuation.phase {
                    return Err(protocol_drift(
                        "Cidaren pre-Question artifact phase does not match its session",
                    ));
                }
                CidarenAttemptFlow::restore_pre_question(
                    context,
                    draft.task_id,
                    remote_task_id,
                    &detail,
                    artifact,
                    selection,
                )?
            }
            _ => {
                return Err(protocol_drift(
                    "Cidaren submission continuation type is stale or foreign",
                ));
            }
        };

        let command = match flow.status() {
            CidarenAttemptFlowStatus::CurrentQuestion => match &item.selected.answer {
                NormalizedAnswer::Skip => flow.issue_skip(&settings, issued_at)?,
                _ => flow.issue_selected_answer(&item.selected, issued_at)?,
            },
            CidarenAttemptFlowStatus::ReadyToVerify => flow.issue_next_verify(issued_at)?,
            CidarenAttemptFlowStatus::ReadyToAdvance
            | CidarenAttemptFlowStatus::CurrentReadingCard => {
                flow.issue_advance(&settings, issued_at)?
            }
            CidarenAttemptFlowStatus::ReadyToSelectWords => flow.issue_word_selection(issued_at)?,
            CidarenAttemptFlowStatus::ReadyToStart => flow.issue_start(issued_at)?,
            _ => {
                return Err(protocol_drift(
                    "Cidaren restored submission phase cannot issue an operation",
                ));
            }
        };
        let recovery_artifact = command.recovery_artifact(&self.metadata.id)?;
        Ok(PreparedCidarenSubmissionOperation {
            provider_id: self.metadata.id.clone(),
            previous_question_id: item.question.id,
            previous_position: item.question.position,
            flow,
            command,
            recovery_artifact,
            assessments: self.questions.assessment_transport(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn recover_ambiguous_submission_operation_at(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        recovery_artifact: Option<&ProviderQuestionOperationArtifact>,
        operation: &AmbiguousProviderQuestionSessionOperation,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderSubmissionStepOutcome>> {
        validate_context(context, &self.metadata)?;
        validate_ambiguous_operation_metadata(operation, continuation.revision)?;
        let prepared = self
            .prepare_submission_operation_at(
                context,
                remote_task_id,
                draft,
                continuation,
                runtime_settings,
                operation.issued_at,
            )
            .await?;
        validate_rebuilt_ambiguous_operation(
            operation,
            prepared.command.operation_type(),
            prepared.command.request_digest(),
        )?;
        validate_mutation_recovery_artifact(
            &self.metadata.id,
            recovery_artifact,
            prepared.command.projection(),
        )?;
        // Freshly rebuilding the immutable Draft/Attempt request is only a
        // recovery check. The prepared mutation is always discarded, so the
        // encrypted projection cannot become replay authority.
        drop(prepared);
        Ok(None)
    }
}

impl fmt::Debug for CidarenSubmissionExecute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenSubmissionExecute")
            .field("metadata", &self.metadata)
            .field("preview", &self.preview)
            .field("questions", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenSubmissionExecute {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionExecuteCapability for CidarenSubmissionExecute {
    async fn execute_submission(
        &self,
        context: &ProviderContext,
        _remote_task_id: &str,
        _draft: &SubmissionDraft,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
        _events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<SubmissionReceipt> {
        validate_context(context, &self.metadata)?;
        Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Cidaren submission requires its durable Question session",
        ))
    }

    async fn prepare_submission_operation(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<Box<dyn PreparedProviderSubmissionOperation>>> {
        Ok(Some(Box::new(
            self.prepare_submission_operation_at(
                context,
                remote_task_id,
                draft,
                continuation,
                runtime_settings,
                Utc::now(),
            )
            .await?,
        )))
    }

    async fn recover_ambiguous_submission_operation(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        operation: &AmbiguousProviderQuestionSessionOperation,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderSubmissionStepOutcome>> {
        self.recover_ambiguous_submission_operation_at(
            context,
            remote_task_id,
            draft,
            continuation,
            None,
            operation,
            runtime_settings,
        )
        .await
    }

    async fn recover_ambiguous_submission_operation_with_artifact(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        recovery_artifact: Option<&ProviderQuestionOperationArtifact>,
        operation: &AmbiguousProviderQuestionSessionOperation,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderSubmissionStepOutcome>> {
        self.recover_ambiguous_submission_operation_at(
            context,
            remote_task_id,
            draft,
            continuation,
            recovery_artifact,
            operation,
            runtime_settings,
        )
        .await
    }
}

fn validate_ambiguous_operation_metadata(
    operation: &AmbiguousProviderQuestionSessionOperation,
    continuation_revision: u32,
) -> ProviderResult<()> {
    if operation.continuation_revision != continuation_revision {
        return Err(remote_changed(
            "Cidaren ambiguous submission continuation revision changed",
        ));
    }
    if operation.operation_type.is_empty() || operation.request_digest == [0; 32] {
        return Err(protocol_drift(
            "Cidaren ambiguous submission identity is missing",
        ));
    }
    if operation.ambiguous_at < operation.issued_at {
        return Err(protocol_drift(
            "Cidaren ambiguous submission timestamps are invalid",
        ));
    }
    Ok(())
}

fn validate_rebuilt_ambiguous_operation(
    operation: &AmbiguousProviderQuestionSessionOperation,
    operation_type: &str,
    request_digest: [u8; 32],
) -> ProviderResult<()> {
    if operation.operation_type != operation_type {
        return Err(remote_changed(
            "Cidaren ambiguous submission type changed after fresh readback",
        ));
    }
    if operation.request_digest != request_digest {
        return Err(remote_changed(
            "Cidaren ambiguous submission request changed after fresh readback",
        ));
    }
    Ok(())
}

fn validate_continuation_shape(
    continuation: &ResolvedProviderQuestionSessionContinuation<'_>,
) -> ProviderResult<()> {
    let valid_phase = match continuation.continuation_type {
        CIDAREN_QUESTION_ARTIFACT_TYPE => matches!(
            continuation.phase,
            CIDAREN_QUESTION_ARTIFACT_PHASE
                | CIDAREN_READY_TO_VERIFY_PHASE
                | CIDAREN_READY_TO_ADVANCE_PHASE
        ),
        CIDAREN_PRE_QUESTION_ARTIFACT_TYPE => matches!(
            continuation.phase,
            CIDAREN_READY_TO_SELECT_WORDS_PHASE
                | CIDAREN_READY_TO_START_PHASE
                | CIDAREN_READING_CARD_PHASE
        ),
        _ => false,
    };
    if continuation.revision == 0 || !valid_phase {
        return Err(protocol_drift(
            "Cidaren submission continuation metadata is stale or foreign",
        ));
    }
    Ok(())
}

pub(crate) struct PreparedCidarenSubmissionOperation {
    provider_id: asterism_domain::ProviderId,
    previous_question_id: QuestionId,
    previous_position: u32,
    flow: CidarenAttemptFlow,
    command: CidarenIssuedCommand,
    recovery_artifact: ProviderQuestionOperationArtifact,
    assessments: Arc<dyn crate::CidarenAssessmentTransport>,
}

impl fmt::Debug for PreparedCidarenSubmissionOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCidarenSubmissionOperation")
            .field("provider_id", &self.provider_id)
            .field("previous_question_id", &self.previous_question_id)
            .field("previous_position", &self.previous_position)
            .field("flow", &self.flow)
            .field("command", &self.command)
            .field("recovery_artifact", &self.recovery_artifact)
            .field("assessments", &"configured")
            .finish()
    }
}

#[async_trait]
impl PreparedProviderSubmissionOperation for PreparedCidarenSubmissionOperation {
    fn operation_type(&self) -> &str {
        self.command.operation_type()
    }

    fn request_digest(&self) -> [u8; 32] {
        self.command.request_digest()
    }

    fn delay_before_execute_seconds(&self) -> u64 {
        self.command.delay_before_execute_seconds()
    }

    fn recovery_artifact(&self) -> Option<ProviderQuestionOperationArtifact> {
        Some(self.recovery_artifact.clone())
    }

    async fn execute(
        self: Box<Self>,
        context: &ProviderContext,
        _events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ProviderSubmissionStepOutcome> {
        let Self {
            provider_id,
            previous_question_id,
            previous_position,
            mut flow,
            command,
            recovery_artifact: _,
            assessments,
        } = *self;
        let outcome = command.execute(assessments, context).await?;
        flow.accept(outcome)?;
        match flow.accepted_step_outcome()? {
            CidarenDurableStepOutcome::Question(materialization) => {
                let next_id = materialization.question().id;
                let next_position = materialization.question().position;
                let mapped = map_question_materialization(&provider_id, materialization)?;
                if next_id == previous_question_id && next_position == previous_position {
                    let (_, continuation, response_digest, received_at) = mapped.into_parts();
                    ProviderSubmissionStepOutcome::continuing(
                        continuation,
                        response_digest,
                        received_at,
                    )
                } else if next_id != previous_question_id && next_position > previous_position {
                    Ok(ProviderSubmissionStepOutcome::NextQuestion(mapped))
                } else {
                    Err(remote_changed(
                        "Cidaren accepted response regressed or conflicted with Question identity",
                    ))
                }
            }
            CidarenDurableStepOutcome::PreQuestion(continuation) => {
                let (continuation, response_digest, received_at) =
                    map_pre_question_continuation(&provider_id, continuation)?;
                ProviderSubmissionStepOutcome::continuing(
                    continuation,
                    response_digest.ok_or_else(|| {
                        internal("Cidaren accepted continuation has no response digest")
                    })?,
                    received_at.ok_or_else(|| {
                        internal("Cidaren accepted continuation has no response timestamp")
                    })?,
                )
            }
            CidarenDurableStepOutcome::Completed {
                receipt,
                receipt_digest,
                response_digest,
            } => {
                if receipt_digest == [0; 32] {
                    return Err(internal(
                        "Cidaren completion receipt lost its binding digest",
                    ));
                }
                let received_at = receipt.received_at;
                ProviderSubmissionStepOutcome::submitted_with_artifact(
                    receipt,
                    response_digest,
                    received_at,
                    terminal_result_artifact(&provider_id, receipt_digest)?,
                )
            }
        }
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "Cidaren submission execution received another Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren submission execution requires an authenticated session",
        ));
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_submission_readback_rejects_revision_request_and_time_drift() {
        let issued_at = Utc::now();
        let operation = AmbiguousProviderQuestionSessionOperation {
            continuation_revision: 3,
            operation_type: "cidaren.submit-answer-and-save.v1".to_owned(),
            request_digest: [4; 32],
            issued_at,
            ambiguous_at: issued_at + chrono::Duration::seconds(1),
        };
        validate_ambiguous_operation_metadata(&operation, 3).unwrap();
        validate_rebuilt_ambiguous_operation(
            &operation,
            "cidaren.submit-answer-and-save.v1",
            [4; 32],
        )
        .unwrap();

        assert_eq!(
            validate_ambiguous_operation_metadata(&operation, 4)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        let mut drifted = operation.clone();
        drifted.request_digest = [0; 32];
        assert_eq!(
            validate_ambiguous_operation_metadata(&drifted, 3)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        let mut drifted = operation.clone();
        drifted.ambiguous_at = issued_at - chrono::Duration::seconds(1);
        assert_eq!(
            validate_ambiguous_operation_metadata(&drifted, 3)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        assert_eq!(
            validate_rebuilt_ambiguous_operation(&operation, "cidaren.verify-answer.v1", [4; 32],)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(
            validate_rebuilt_ambiguous_operation(
                &operation,
                "cidaren.submit-answer-and-save.v1",
                [5; 32],
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::RemoteChanged
        );
    }
}
