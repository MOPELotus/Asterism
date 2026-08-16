use std::{fmt, sync::Arc};

use asterism_domain::{ProviderId, TaskId, Timestamp};
use asterism_provider_api::{
    AmbiguousProviderQuestionReadOperation, PreparedProviderQuestionReadOperation, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderQuestionMaterialization, ProviderQuestionReadContinuation,
    ProviderQuestionReadStepOutcome, ProviderResult, QuestionInventoryCapability,
    RemoteQuestionRef, RemoteTaskDetail, ResolvedProviderQuestionReadContinuation,
    ResolvedProviderRuntimeSettings, TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{
    CIDAREN_PRE_QUESTION_ARTIFACT_TYPE, CIDAREN_QUESTION_ARTIFACT_TYPE,
    CIDAREN_READY_TO_SELECT_WORDS_PHASE, CidarenAnswerEvidenceTransport,
    CidarenAssessmentTransport, CidarenAttemptFlow, CidarenAttemptFlowStatus,
    CidarenDurableStepOutcome, CidarenIssuedCommand, CidarenPreQuestionArtifact,
    CidarenPreQuestionContinuation, CidarenQuestionMaterialization, CidarenRuntimeSettings,
    CidarenWordSelectionPlan, build_word_selection_plan, metadata::development_metadata,
};

const CONTINUATION_TTL_SECONDS: u64 = 30 * 60;

/// Durable entry point for Cidaren's mutation-backed first-Question flow.
pub struct CidarenQuestionInventory {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    evidence: Arc<dyn CidarenAnswerEvidenceTransport>,
    assessments: Arc<dyn CidarenAssessmentTransport>,
}

impl CidarenQuestionInventory {
    /// Creates the capability around fresh Task/evidence reads and the native
    /// one-shot assessment transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        evidence: Arc<dyn CidarenAnswerEvidenceTransport>,
        assessments: Arc<dyn CidarenAssessmentTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            evidence,
            assessments,
        })
    }

    async fn fresh_detail_and_selection(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<(RemoteTaskDetail, Option<CidarenWordSelectionPlan>)> {
        let detail = self.details.task_detail(context, remote_task_id).await?;
        let binding = self
            .evidence
            .bind_answer_evidence(context, remote_task_id, &detail)
            .await?;
        let inventory = self
            .evidence
            .fetch_word_inventory(context, &binding)
            .await?;
        let selection = build_word_selection_plan(&binding, &inventory)?;
        Ok((detail, selection))
    }

    pub(crate) async fn fresh_detail_and_phase_selection(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        phase: &str,
    ) -> ProviderResult<(RemoteTaskDetail, Option<CidarenWordSelectionPlan>)> {
        if phase == CIDAREN_READY_TO_SELECT_WORDS_PHASE {
            self.fresh_detail_and_selection(context, remote_task_id)
                .await
        } else {
            Ok((
                self.details.task_detail(context, remote_task_id).await?,
                None,
            ))
        }
    }

    pub(crate) fn assessment_transport(&self) -> Arc<dyn CidarenAssessmentTransport> {
        self.assessments.clone()
    }
}

impl fmt::Debug for CidarenQuestionInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenQuestionInventory")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("evidence", &"configured")
            .field("assessments", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenQuestionInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl QuestionInventoryCapability for CidarenQuestionInventory {
    async fn list_question_refs(
        &self,
        context: &ProviderContext,
        _remote_task_id: &str,
    ) -> ProviderResult<Vec<RemoteQuestionRef>> {
        validate_context(context, &self.metadata)?;
        Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Cidaren Questions require the durable pre-Question attempt flow",
        ))
    }

    async fn prepare_question_read_attempt(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderQuestionReadContinuation>> {
        validate_context(context, &self.metadata)?;
        CidarenRuntimeSettings::resolve(runtime_settings)?;
        let (detail, selection) = self
            .fresh_detail_and_selection(context, remote_task_id)
            .await?;
        let flow =
            CidarenAttemptFlow::try_new(context, task_id, remote_task_id, &detail, selection)?;
        let continuation = flow.pre_question_continuation()?.ok_or_else(|| {
            internal("Cidaren initial attempt produced no pre-Question continuation")
        })?;
        let (continuation, response_digest, received_at) =
            map_pre_question_continuation(&self.metadata.id, continuation)?;
        if response_digest.is_some() || received_at.is_some() {
            return Err(internal(
                "Cidaren initial continuation unexpectedly has a response binding",
            ));
        }
        Ok(Some(continuation))
    }

    async fn prepare_question_read_operation(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        continuation: ResolvedProviderQuestionReadContinuation<'_>,
        runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Box<dyn PreparedProviderQuestionReadOperation>> {
        validate_context(context, &self.metadata)?;
        if continuation.continuation_type != CIDAREN_PRE_QUESTION_ARTIFACT_TYPE {
            return Err(protocol_drift(
                "Cidaren pre-Question continuation type is invalid",
            ));
        }
        let settings = CidarenRuntimeSettings::resolve(runtime_settings)?;
        let artifact = CidarenPreQuestionArtifact::decode_bound(
            continuation.value,
            continuation.continuation_digest,
            task_id,
            remote_task_id,
        )?;
        if artifact.phase() != continuation.phase {
            return Err(protocol_drift(
                "Cidaren pre-Question continuation phase does not match its payload",
            ));
        }
        let (detail, selection) = self
            .fresh_detail_and_phase_selection(context, remote_task_id, continuation.phase)
            .await?;
        let mut flow = CidarenAttemptFlow::restore_pre_question(
            context,
            task_id,
            remote_task_id,
            &detail,
            artifact,
            selection,
        )?;
        let issued_at = Utc::now();
        let command = match flow.status() {
            CidarenAttemptFlowStatus::ReadyToSelectWords => flow.issue_word_selection(issued_at)?,
            CidarenAttemptFlowStatus::ReadyToStart => flow.issue_start(issued_at)?,
            CidarenAttemptFlowStatus::CurrentReadingCard => {
                flow.issue_advance(&settings, issued_at)?
            }
            _ => {
                return Err(protocol_drift(
                    "Cidaren restored an unsupported pre-Question phase",
                ));
            }
        };
        Ok(Box::new(PreparedCidarenQuestionReadOperation {
            provider_id: self.metadata.id.clone(),
            flow,
            command,
            assessments: self.assessments.clone(),
        }))
    }

    async fn recover_ambiguous_question_read_operation(
        &self,
        context: &ProviderContext,
        _task_id: TaskId,
        _remote_task_id: &str,
        _continuation: ResolvedProviderQuestionReadContinuation<'_>,
        _operation: &AmbiguousProviderQuestionReadOperation,
        _runtime_settings: &ResolvedProviderRuntimeSettings,
    ) -> ProviderResult<Option<ProviderQuestionReadStepOutcome>> {
        validate_context(context, &self.metadata)?;
        Ok(None)
    }
}

struct PreparedCidarenQuestionReadOperation {
    provider_id: ProviderId,
    flow: CidarenAttemptFlow,
    command: CidarenIssuedCommand,
    assessments: Arc<dyn CidarenAssessmentTransport>,
}

impl fmt::Debug for PreparedCidarenQuestionReadOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCidarenQuestionReadOperation")
            .field("provider_id", &self.provider_id)
            .field("flow", &self.flow)
            .field("command", &self.command)
            .field("assessments", &"configured")
            .finish()
    }
}

#[async_trait]
impl PreparedProviderQuestionReadOperation for PreparedCidarenQuestionReadOperation {
    fn operation_type(&self) -> &str {
        self.command.operation_type()
    }

    fn request_digest(&self) -> [u8; 32] {
        self.command.request_digest()
    }

    fn delay_before_execute_seconds(&self) -> u64 {
        self.command.delay_before_execute_seconds()
    }

    async fn execute(
        self: Box<Self>,
        context: &ProviderContext,
    ) -> ProviderResult<ProviderQuestionReadStepOutcome> {
        let Self {
            provider_id,
            mut flow,
            command,
            assessments,
        } = *self;
        let outcome = command.execute(assessments, context).await?;
        flow.accept(outcome)?;
        match flow.accepted_step_outcome()? {
            CidarenDurableStepOutcome::Question(materialization) => {
                Ok(ProviderQuestionReadStepOutcome::Materialize(
                    map_question_materialization(&provider_id, materialization)?,
                ))
            }
            CidarenDurableStepOutcome::PreQuestion(continuation) => {
                let (continuation, response_digest, received_at) =
                    map_pre_question_continuation(&provider_id, continuation)?;
                ProviderQuestionReadStepOutcome::continuing(
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
                response_digest,
            } => ProviderQuestionReadStepOutcome::completed(receipt, response_digest),
        }
    }
}

pub(crate) fn map_pre_question_continuation(
    provider_id: &ProviderId,
    continuation: CidarenPreQuestionContinuation,
) -> ProviderResult<(
    ProviderQuestionReadContinuation,
    Option<[u8; 32]>,
    Option<Timestamp>,
)> {
    let (artifact, phase, response_digest, received_at) = continuation.into_parts();
    if response_digest.is_some() != received_at.is_some() {
        return Err(internal(
            "Cidaren continuation response binding is incomplete",
        ));
    }
    let expected_digest = artifact.digest();
    let continuation = ProviderQuestionReadContinuation::try_new(
        provider_id,
        CIDAREN_PRE_QUESTION_ARTIFACT_TYPE,
        phase,
        artifact.into_secret_value(),
        CONTINUATION_TTL_SECONDS,
    )?;
    if continuation.continuation_digest() != expected_digest {
        return Err(internal(
            "Cidaren continuation digest changed at the Provider boundary",
        ));
    }
    Ok((continuation, response_digest, received_at))
}

pub(crate) fn map_question_materialization(
    provider_id: &ProviderId,
    materialization: CidarenQuestionMaterialization,
) -> ProviderResult<ProviderQuestionMaterialization> {
    let (question, artifact, phase, response_digest, received_at) = materialization.into_parts();
    let expected_digest = artifact.digest();
    let continuation = ProviderQuestionReadContinuation::try_new(
        provider_id,
        CIDAREN_QUESTION_ARTIFACT_TYPE,
        phase,
        artifact.into_secret_value(),
        CONTINUATION_TTL_SECONDS,
    )?;
    if continuation.continuation_digest() != expected_digest {
        return Err(internal(
            "Cidaren Question artifact digest changed at the Provider boundary",
        ));
    }
    ProviderQuestionMaterialization::try_new(
        vec![question],
        continuation,
        response_digest,
        received_at,
    )
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "Cidaren Question flow received another Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren Question flow requires an authenticated session",
        ));
    }
    Ok(())
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use asterism_domain::{
        AnswerCandidateId, AnswerPair, AnswerSource, AssessmentClass, NormalizedAnswer,
        ProviderAccountId, QuestionSnapshotId, RemoteState, SecretId, SelectedAnswer, SourceType,
        SubmissionAnswerCoverage, SubmissionDraft, SubmissionDraftId, SubmissionDraftItem,
        TaskCapability,
    };
    use asterism_provider_api::{
        ExecutionEventSink, ProviderExecutionLog, ProviderProgress, ProviderSubmissionStepOutcome,
        RemoteTask, ResolvedProviderQuestionSessionContinuation, SubmissionBuildCapability,
        SubmissionExecuteCapability,
    };
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        CidarenAnswerEvidenceBinding, CidarenAssessmentResponse, CidarenAssessmentTransportOutcome,
        CidarenAttemptOperation, CidarenMutationRequest, CidarenStartAnswerRequest,
        CidarenStudyTaskDocument, CidarenSubmissionBuild, CidarenSubmissionExecute,
        CidarenWordEvidence, CidarenWordInventory, CidarenWordLookup, parse_assessment_response,
        parse_study_task_info_response, parse_word_selection_response, runtime_settings,
    };

    const REMOTE_TASK_ID: &str = "class-task:2002";

    fn complete_answer_coverage(total_question_count: usize) -> SubmissionAnswerCoverage {
        SubmissionAnswerCoverage {
            total_question_count: u32::try_from(total_question_count).unwrap(),
            minimum_coverage_millis: 1_000,
            unanswered_question_ids: Vec::new(),
        }
    }

    struct FixtureBoundaries {
        metadata: ProviderMetadata,
        detail: RemoteTaskDetail,
        responses: Mutex<VecDeque<CidarenAssessmentResponse>>,
        operations: Mutex<Vec<CidarenAttemptOperation>>,
    }

    #[derive(Debug)]
    struct NoopEvents;

    #[async_trait]
    impl ExecutionEventSink for NoopEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            Ok(())
        }

        async fn log(&self, _event: ProviderExecutionLog) -> ProviderResult<()> {
            Ok(())
        }
    }

    impl FixtureBoundaries {
        fn new(detail: RemoteTaskDetail, responses: Vec<CidarenAssessmentResponse>) -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                detail,
                responses: Mutex::new(responses.into()),
                operations: Mutex::new(Vec::new()),
            }
        }

        fn respond(
            &self,
            operation: CidarenAttemptOperation,
        ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
            self.operations.lock().unwrap().push(operation);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| internal("Cidaren fixture response was exhausted"))?;
            let digest = Sha256::new()
                .chain_update(b"cidaren-question-adapter-fixture:v1\0")
                .chain_update(operation.operation_type().as_bytes())
                .chain_update(self.operations.lock().unwrap().len().to_be_bytes())
                .finalize()
                .into();
            CidarenAssessmentTransportOutcome::try_new(response, digest, Utc::now())
        }
    }

    impl ProviderIdentity for FixtureBoundaries {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureBoundaries {
        async fn task_detail(
            &self,
            context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            validate_context(context, &self.metadata)?;
            if remote_task_id != self.detail.task.remote_id {
                return Err(ProviderError::new(
                    ProviderErrorKind::RemoteChanged,
                    "Cidaren fixture Task binding changed",
                ));
            }
            Ok(self.detail.clone())
        }
    }

    #[async_trait]
    impl CidarenAnswerEvidenceTransport for FixtureBoundaries {
        async fn bind_answer_evidence(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
            detail: &RemoteTaskDetail,
        ) -> ProviderResult<CidarenAnswerEvidenceBinding> {
            CidarenAnswerEvidenceBinding::from_fresh_detail(
                remote_task_id,
                detail,
                &CidarenStudyTaskDocument::try_new(
                    "course-a",
                    include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
                )?,
            )
        }

        async fn fetch_word_inventory(
            &self,
            _context: &ProviderContext,
            binding: &CidarenAnswerEvidenceBinding,
        ) -> ProviderResult<CidarenWordInventory> {
            parse_study_task_info_response(
                include_str!("../../../fixtures/providers/cidaren/answers/study-task-info.json")
                    .as_bytes(),
                binding,
                None,
            )
        }

        async fn fetch_word_evidence(
            &self,
            _context: &ProviderContext,
            _lookup: &CidarenWordLookup,
        ) -> ProviderResult<CidarenWordEvidence> {
            Err(internal("Cidaren fixture word evidence must not be read"))
        }

        async fn resolve_word_prototype(
            &self,
            _context: &ProviderContext,
            _word: &str,
        ) -> ProviderResult<Option<String>> {
            Err(internal("Cidaren fixture prototype must not be read"))
        }
    }

    #[async_trait]
    impl CidarenAssessmentTransport for FixtureBoundaries {
        async fn start_answer(
            &self,
            _context: &ProviderContext,
            _request: &CidarenStartAnswerRequest,
        ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::StartAnswer)
        }

        async fn verify_answer(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::VerifyAnswer)
        }

        async fn submit_answer_and_save(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::SubmitAnswerAndSave)
        }

        async fn skip_answer(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::SkipAnswer)
        }

        async fn submit_chose_word(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::SubmitChoseWord)
        }
    }

    #[tokio::test]
    async fn reading_card_rotates_before_real_question_materialization() {
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("test", -1),
            vec![
                response(&reading_card_payload()),
                response(&question_payload()),
            ],
        ));
        let capability = CidarenQuestionInventory::try_new(
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
        )
        .unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();

        let initial = capability
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(initial.phase(), crate::CIDAREN_READY_TO_START_PHASE);
        let prepared = prepare_operation(&capability, &context, task_id, initial, &settings).await;
        assert_eq!(prepared.operation_type(), "cidaren.start-answer.v1");
        assert_ne!(prepared.request_digest(), [0; 32]);
        let ProviderQuestionReadStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("reading card must remain a continuation");
        };
        assert_eq!(continuation.phase(), crate::CIDAREN_READING_CARD_PHASE);

        let prepared =
            prepare_operation(&capability, &context, task_id, continuation, &settings).await;
        assert_eq!(
            prepared.operation_type(),
            "cidaren.submit-answer-and-save.v1"
        );
        assert!((1..=3).contains(&prepared.delay_before_execute_seconds()));
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("second fixture response must materialize a Question");
        };
        assert_eq!(materialization.questions().len(), 1);
        assert_eq!(materialization.questions()[0].task_id, task_id);
        assert_eq!(
            materialization.artifact().continuation_type(),
            CIDAREN_QUESTION_ARTIFACT_TYPE
        );
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::SubmitAnswerAndSave,
            ]
        );
    }

    #[tokio::test]
    async fn word_selection_required_rotates_to_fresh_reselection_phase() {
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("learning", 92002),
            vec![CidarenAssessmentResponse::Receipt {
                kind: crate::CidarenAssessmentReceiptKind::WordSelectionRequired,
                message_sanitized: None,
            }],
        ));
        let capability = CidarenQuestionInventory::try_new(
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
        )
        .unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = capability
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(initial.phase(), CIDAREN_READY_TO_SELECT_WORDS_PHASE);

        let prepared = prepare_operation(&capability, &context, task_id, initial, &settings).await;
        assert_eq!(prepared.operation_type(), "cidaren.submit-chose-word.v1");
        let ProviderQuestionReadStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("selection-required receipt must rotate a continuation");
        };
        assert_eq!(continuation.phase(), CIDAREN_READY_TO_SELECT_WORDS_PHASE);
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [CidarenAttemptOperation::SubmitChoseWord]
        );
    }

    #[tokio::test]
    async fn existing_word_selection_rotates_to_start_without_reselection() {
        let rejection = parse_word_selection_response(include_bytes!(
            "../../../fixtures/providers/cidaren/questions/submit-chose-word-required-children.json"
        ))
        .unwrap();
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("learning", 92002),
            vec![rejection, response(&question_payload())],
        ));
        let capability = CidarenQuestionInventory::try_new(
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
        )
        .unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = capability
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&capability, &context, task_id, initial, &settings).await;

        let ProviderQuestionReadStepOutcome::Continue {
            continuation,
            response_digest,
            received_at,
        } = prepared.execute(&context).await.unwrap()
        else {
            panic!("known existing-selection rejection must continue to StartAnswer");
        };
        assert_eq!(continuation.phase(), crate::CIDAREN_READY_TO_START_PHASE);
        assert_ne!(response_digest, [0; 32]);
        assert!(received_at <= Utc::now());

        let prepared =
            prepare_operation(&capability, &context, task_id, continuation, &settings).await;
        assert_eq!(prepared.operation_type(), "cidaren.start-answer.v1");
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("fresh StartAnswer must materialize the first Question");
        };
        assert_eq!(materialization.questions().len(), 1);
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::SubmitChoseWord,
                CidarenAttemptOperation::StartAnswer,
            ]
        );
    }

    #[tokio::test]
    async fn completion_before_first_question_remains_a_typed_terminal_outcome() {
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("test", -1),
            vec![CidarenAssessmentResponse::Receipt {
                kind: crate::CidarenAssessmentReceiptKind::Completed,
                message_sanitized: Some("synthetic completed".to_owned()),
            }],
        ));
        let capability =
            CidarenQuestionInventory::try_new(boundaries.clone(), boundaries.clone(), boundaries)
                .unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = capability
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&capability, &context, task_id, initial, &settings).await;

        let ProviderQuestionReadStepOutcome::Completed {
            receipt,
            response_digest,
        } = prepared.execute(&context).await.unwrap()
        else {
            panic!("completed receipt must not be represented as an empty Question");
        };
        assert_eq!(receipt.remote_status, "completed");
        assert_eq!(
            receipt.message_sanitized.as_deref(),
            Some("synthetic completed")
        );
        assert_ne!(response_digest, [0; 32]);
    }

    #[tokio::test]
    async fn materialized_mode_73_skip_executes_as_a_terminal_session_step() {
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("test", -1),
            vec![
                response(&serde_json::from_str(include_str!(
                    "../../../fixtures/providers/cidaren/questions/start-answer-fill-blank-73.json"
                ))
                .unwrap()),
                CidarenAssessmentResponse::Receipt {
                    kind: crate::CidarenAssessmentReceiptKind::Completed,
                    message_sanitized: Some("synthetic completed".to_owned()),
                },
            ],
        ));
        let inventory = Arc::new(
            CidarenQuestionInventory::try_new(
                boundaries.clone(),
                boundaries.clone(),
                boundaries.clone(),
            )
            .unwrap(),
        );
        let execute = CidarenSubmissionExecute::try_new(inventory.clone()).unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = inventory
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&inventory, &context, task_id, initial, &settings).await;
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("mode 73 must materialize before Skip");
        };
        let (questions, continuation, _, _) = materialization.into_parts();
        let [question] = questions.as_slice() else {
            panic!("Cidaren must materialize one current Question");
        };
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Skip,
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = CidarenSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context,
                REMOTE_TASK_ID,
                std::slice::from_ref(question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("cidaren").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: complete_answer_coverage(questions.len()),
            items: vec![SubmissionDraftItem {
                question: question.clone(),
                selected,
            }],
            payload_preview: preview,
            created_at: Utc::now(),
        };
        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 1, &settings)
                .await;
        assert_eq!(prepared.operation_type(), "cidaren.skip-answer.v1");
        let ProviderSubmissionStepOutcome::Submitted {
            receipt,
            response_digest,
            ..
        } = prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("definite Cidaren completion must close without a continuation");
        };
        assert_eq!(receipt.remote_status, "completed");
        assert_ne!(response_digest, [0; 32]);
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::SkipAnswer,
            ]
        );
    }

    #[tokio::test]
    async fn verified_answer_rotates_then_materializes_the_next_question() {
        let mut next_question = question_payload();
        next_question["topic_code"] = json!("next-topic-code");
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("test", -1),
            vec![
                response(&question_payload()),
                response(&json!({"topic_code": "rotated-topic-code"})),
                response(&next_question),
            ],
        ));
        let inventory = Arc::new(
            CidarenQuestionInventory::try_new(
                boundaries.clone(),
                boundaries.clone(),
                boundaries.clone(),
            )
            .unwrap(),
        );
        let execute = CidarenSubmissionExecute::try_new(inventory.clone()).unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = inventory
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&inventory, &context, task_id, initial, &settings).await;
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("StartAnswer must materialize the first Question");
        };
        let (questions, continuation, _, _) = materialization.into_parts();
        let [question] = questions.as_slice() else {
            panic!("Cidaren must materialize one current Question");
        };
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Selections(vec!["n:1".to_owned()]),
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let preview = CidarenSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context,
                REMOTE_TASK_ID,
                std::slice::from_ref(question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("cidaren").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: complete_answer_coverage(questions.len()),
            items: vec![SubmissionDraftItem {
                question: question.clone(),
                selected,
            }],
            payload_preview: preview,
            created_at: Utc::now(),
        };
        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 1, &settings)
                .await;
        assert_eq!(prepared.operation_type(), "cidaren.verify-answer.v1");
        let ProviderSubmissionStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("VerifyAnswer must rotate the same Question session");
        };

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 2, &settings)
                .await;
        assert_eq!(
            prepared.operation_type(),
            "cidaren.submit-answer-and-save.v1"
        );
        let ProviderSubmissionStepOutcome::NextQuestion(next) =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("advance must materialize the next Question");
        };
        assert_eq!(next.questions().len(), 1);
        assert_eq!(next.questions()[0].position, 2);
        assert_ne!(next.questions()[0].id, question.id);
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::VerifyAnswer,
                CidarenAttemptOperation::SubmitAnswerAndSave,
            ]
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps every durable matching relation boundary visible"
    )]
    async fn matching_relations_restore_each_rotated_token_before_terminal_advance() {
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("test", -1),
            vec![
                response(&matching_question_payload()),
                response(&json!({"topic_code": "rotated-matching-one"})),
                response(&json!({"topic_code": "rotated-matching-two"})),
                CidarenAssessmentResponse::Receipt {
                    kind: crate::CidarenAssessmentReceiptKind::Completed,
                    message_sanitized: Some("synthetic completed".to_owned()),
                },
            ],
        ));
        let inventory = Arc::new(
            CidarenQuestionInventory::try_new(
                boundaries.clone(),
                boundaries.clone(),
                boundaries.clone(),
            )
            .unwrap(),
        );
        let execute = CidarenSubmissionExecute::try_new(inventory.clone()).unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = inventory
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&inventory, &context, task_id, initial, &settings).await;
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("StartAnswer must materialize the matching Question");
        };
        let (questions, continuation, _, _) = materialization.into_parts();
        let [question] = questions.as_slice() else {
            panic!("Cidaren must materialize one current Question");
        };
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Pairs(vec![
                AnswerPair {
                    left: "alpha".to_owned(),
                    right: "n:1".to_owned(),
                },
                AnswerPair {
                    left: "beta".to_owned(),
                    right: "n:0".to_owned(),
                },
            ]),
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let preview = CidarenSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context,
                REMOTE_TASK_ID,
                std::slice::from_ref(question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("cidaren").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: complete_answer_coverage(questions.len()),
            items: vec![SubmissionDraftItem {
                question: question.clone(),
                selected,
            }],
            payload_preview: preview,
            created_at: Utc::now(),
        };

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 1, &settings)
                .await;
        let ProviderSubmissionStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("first matching relation must rotate the session");
        };
        assert_eq!(continuation.phase(), crate::CIDAREN_READY_TO_VERIFY_PHASE);
        let first_digest = continuation.continuation_digest();

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 2, &settings)
                .await;
        let ProviderSubmissionStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("second matching relation must rotate to advance");
        };
        assert_eq!(continuation.phase(), crate::CIDAREN_READY_TO_ADVANCE_PHASE);
        assert_ne!(continuation.continuation_digest(), first_digest);

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 3, &settings)
                .await;
        let ProviderSubmissionStepOutcome::Submitted { receipt, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("terminal matching advance must submit without a continuation");
        };
        assert_eq!(receipt.remote_status, "completed");
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::VerifyAnswer,
                CidarenAttemptOperation::VerifyAnswer,
                CidarenAttemptOperation::SubmitAnswerAndSave,
            ]
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps both durable prepare/execute boundaries visible"
    )]
    async fn post_question_reading_card_rotates_artifact_before_next_question() {
        let mut next_question = question_payload();
        next_question["topic_code"] = json!("after-reading-topic-code");
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("test", -1),
            vec![
                response(&serde_json::from_str(include_str!(
                    "../../../fixtures/providers/cidaren/questions/start-answer-fill-blank-73.json"
                ))
                .unwrap()),
                response(&reading_card_payload()),
                response(&next_question),
            ],
        ));
        let inventory = Arc::new(
            CidarenQuestionInventory::try_new(
                boundaries.clone(),
                boundaries.clone(),
                boundaries.clone(),
            )
            .unwrap(),
        );
        let execute = CidarenSubmissionExecute::try_new(inventory.clone()).unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();
        let initial = inventory
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&inventory, &context, task_id, initial, &settings).await;
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("mode 73 must materialize before Skip");
        };
        let (questions, continuation, _, _) = materialization.into_parts();
        let [question] = questions.as_slice() else {
            panic!("Cidaren must materialize one current Question");
        };
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Skip,
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = CidarenSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context,
                REMOTE_TASK_ID,
                std::slice::from_ref(question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("cidaren").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: complete_answer_coverage(questions.len()),
            items: vec![SubmissionDraftItem {
                question: question.clone(),
                selected,
            }],
            payload_preview: preview,
            created_at: Utc::now(),
        };

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 1, &settings)
                .await;
        let ProviderSubmissionStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("reading card must rotate to a pre-Question continuation");
        };
        assert_eq!(
            continuation.continuation_type(),
            CIDAREN_PRE_QUESTION_ARTIFACT_TYPE
        );
        assert_eq!(continuation.phase(), crate::CIDAREN_READING_CARD_PHASE);

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 2, &settings)
                .await;
        assert_eq!(
            prepared.operation_type(),
            "cidaren.submit-answer-and-save.v1"
        );
        let ProviderSubmissionStepOutcome::NextQuestion(next) =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("reading-card advance must materialize the next Question");
        };
        assert_eq!(next.questions()[0].position, 3);
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::SkipAnswer,
                CidarenAttemptOperation::SubmitAnswerAndSave,
            ]
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps every persisted selection/start boundary visible"
    )]
    async fn mid_attempt_word_reselection_preserves_the_next_question_position() {
        let mut first_question: Value = serde_json::from_str(include_str!(
            "../../../fixtures/providers/cidaren/questions/start-answer-fill-blank-73.json"
        ))
        .unwrap();
        first_question["task_type"] = json!(1);
        let mut next_question = question_payload();
        next_question["topic_code"] = json!("after-reselection-topic-code");
        let boundaries = Arc::new(FixtureBoundaries::new(
            detail("learning", 92002),
            vec![
                CidarenAssessmentResponse::Receipt {
                    kind: crate::CidarenAssessmentReceiptKind::Accepted,
                    message_sanitized: None,
                },
                response(&first_question),
                CidarenAssessmentResponse::Receipt {
                    kind: crate::CidarenAssessmentReceiptKind::WordSelectionRequired,
                    message_sanitized: None,
                },
                CidarenAssessmentResponse::Receipt {
                    kind: crate::CidarenAssessmentReceiptKind::Accepted,
                    message_sanitized: None,
                },
                response(&next_question),
            ],
        ));
        let inventory = Arc::new(
            CidarenQuestionInventory::try_new(
                boundaries.clone(),
                boundaries.clone(),
                boundaries.clone(),
            )
            .unwrap(),
        );
        let execute = CidarenSubmissionExecute::try_new(inventory.clone()).unwrap();
        let context = context();
        let task_id = TaskId::new();
        let settings = settings();

        let initial = inventory
            .prepare_question_read_attempt(&context, task_id, REMOTE_TASK_ID, &settings)
            .await
            .unwrap()
            .unwrap();
        let prepared = prepare_operation(&inventory, &context, task_id, initial, &settings).await;
        let ProviderQuestionReadStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("word selection must rotate to StartAnswer");
        };
        let prepared =
            prepare_operation(&inventory, &context, task_id, continuation, &settings).await;
        let ProviderQuestionReadStepOutcome::Materialize(materialization) =
            prepared.execute(&context).await.unwrap()
        else {
            panic!("StartAnswer must materialize the first Question");
        };
        let (questions, continuation, _, _) = materialization.into_parts();
        let [question] = questions.as_slice() else {
            panic!("Cidaren must materialize one current Question");
        };
        assert_eq!(question.position, 1);
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Skip,
            source: AnswerSource::Manual,
            confidence: None,
        };
        let preview = CidarenSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context,
                REMOTE_TASK_ID,
                std::slice::from_ref(question),
                std::slice::from_ref(&selected),
            )
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("cidaren").unwrap(),
            provider_version: development_metadata().unwrap().implementation_version,
            answer_coverage: complete_answer_coverage(questions.len()),
            items: vec![SubmissionDraftItem {
                question: question.clone(),
                selected,
            }],
            payload_preview: preview,
            created_at: Utc::now(),
        };

        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 1, &settings)
                .await;
        let ProviderSubmissionStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("selection requirement must rotate after Skip");
        };
        assert_eq!(continuation.phase(), CIDAREN_READY_TO_SELECT_WORDS_PHASE);
        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 2, &settings)
                .await;
        let ProviderSubmissionStepOutcome::Continue { continuation, .. } =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("accepted reselection must rotate to StartAnswer");
        };
        assert_eq!(continuation.phase(), crate::CIDAREN_READY_TO_START_PHASE);
        let prepared =
            prepare_submission_operation(&execute, &context, &draft, continuation, 3, &settings)
                .await;
        let ProviderSubmissionStepOutcome::NextQuestion(next) =
            prepared.execute(&context, &NoopEvents).await.unwrap()
        else {
            panic!("post-reselection StartAnswer must materialize the next Question");
        };
        assert_eq!(next.questions()[0].position, 2);
        assert_eq!(
            *boundaries.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::SubmitChoseWord,
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::SkipAnswer,
                CidarenAttemptOperation::SubmitChoseWord,
                CidarenAttemptOperation::StartAnswer,
            ]
        );
    }

    async fn prepare_operation(
        capability: &CidarenQuestionInventory,
        context: &ProviderContext,
        task_id: TaskId,
        continuation: ProviderQuestionReadContinuation,
        settings: &ResolvedProviderRuntimeSettings,
    ) -> Box<dyn PreparedProviderQuestionReadOperation> {
        let (continuation_type, continuation_digest, phase, value, _) = continuation.into_parts();
        capability
            .prepare_question_read_operation(
                context,
                task_id,
                REMOTE_TASK_ID,
                ResolvedProviderQuestionReadContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest,
                    phase: &phase,
                    revision: 1,
                    value: &value,
                },
                settings,
            )
            .await
            .unwrap()
    }

    async fn prepare_submission_operation(
        capability: &CidarenSubmissionExecute,
        context: &ProviderContext,
        draft: &SubmissionDraft,
        continuation: ProviderQuestionReadContinuation,
        revision: u32,
        settings: &ResolvedProviderRuntimeSettings,
    ) -> Box<dyn asterism_provider_api::PreparedProviderSubmissionOperation> {
        let (continuation_type, continuation_digest, phase, value, _) = continuation.into_parts();
        capability
            .prepare_submission_operation(
                context,
                REMOTE_TASK_ID,
                draft,
                ResolvedProviderQuestionSessionContinuation {
                    continuation_type: &continuation_type,
                    continuation_digest,
                    phase: &phase,
                    revision,
                    value: &value,
                },
                settings,
            )
            .await
            .unwrap()
            .unwrap()
    }

    fn settings() -> ResolvedProviderRuntimeSettings {
        runtime_settings::runtime_settings_schema()
            .resolve(None, None, None)
            .unwrap()
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-question-adapter-test".to_owned(),
        }
    }

    fn detail(task_type: &str, task_id: i64) -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": task_id,
            "course_id": "course-a",
            "task_type": task_type,
            "progress": 0,
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: REMOTE_TASK_ID.to_owned(),
                course_remote_id: Some("course:course-a".to_owned()),
                title: "Synthetic List 02".to_owned(),
                source_type: if task_type == "test" {
                    SourceType::Exam
                } else {
                    SourceType::Practice
                },
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::InProgress,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: vec![TaskCapability::SubmissionExecute],
                fingerprint: "synthetic-question-adapter".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: Map::new().into(),
            },
            normalized_detail: json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": "2002",
                "task": normalized,
            }),
        }
    }

    fn response(data: &Value) -> CidarenAssessmentResponse {
        parse_assessment_response(
            &serde_json::to_vec(&json!({
                "code": 1,
                "msg": "synthetic success",
                "data": data,
                "jv": "0",
            }))
            .unwrap(),
            None,
        )
        .unwrap()
    }

    fn reading_card_payload() -> Value {
        json!({
            "topic_code": "reading-topic",
            "topic_mode": 0,
            "topic_done_num": 0,
            "topic_total": 2,
            "stem": {"content": "Synthetic reading card", "remark": ""},
            "options": []
        })
    }

    fn question_payload() -> Value {
        serde_json::from_str(include_str!(
            "../../../fixtures/providers/cidaren/questions/start-answer-single.json"
        ))
        .unwrap()
    }

    fn matching_question_payload() -> Value {
        json!({
            "topic_code": "matching-topic",
            "topic_mode": 31,
            "stem": {
                "content": "Synthetic matching",
                "remark": [
                    {"relation": "alpha"},
                    {"relation": "beta"}
                ]
            },
            "options": [
                {"answer_tag": 0, "content": "alpha", "sub_options": []},
                {"answer_tag": 1, "content": "beta", "sub_options": []}
            ]
        })
    }
}
