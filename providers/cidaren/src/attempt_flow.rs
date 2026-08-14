use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::Arc,
};

use asterism_domain::{
    NormalizedAnswer, Question, QuestionKind, SelectedAnswer, SubmissionReceipt, TaskId, Timestamp,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderResult, RemoteTaskDetail,
};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::pre_question_artifact::CidarenPreQuestionState;
use crate::{
    CIDAREN_QUESTION_ARTIFACT_PHASE, CIDAREN_READY_TO_ADVANCE_PHASE, CIDAREN_READY_TO_VERIFY_PHASE,
    CidarenAssessmentBinding, CidarenAssessmentReceiptKind, CidarenAssessmentResponse,
    CidarenAssessmentTransport, CidarenAttemptProgress, CidarenMutationRequest,
    CidarenPreQuestionArtifact, CidarenQuestionArtifact, CidarenRuntimeSettings,
    CidarenStartAnswerRequest, CidarenWireAnswer, CidarenWordSelectionPlan,
    EncodedCidarenPreQuestionArtifact, EncodedCidarenQuestionArtifact,
    ParsedCidarenAttemptQuestion, ParsedCidarenAttemptStep, ParsedCidarenReadingCard,
    build_skip_answer_request, build_start_answer_request, build_submit_answer_and_save_request,
    build_submit_chose_word_request, build_verify_answer_request, parse_attempt_step,
};

const MAX_CORRELATION_ID_BYTES: usize = 512;
const MAX_TOPIC_CODE_BYTES: usize = 4_096;
const MAX_VERIFIED_STEPS: u32 = 256;

/// One donor-observed remote mutation in the Cidaren answer lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CidarenAttemptOperation {
    SubmitChoseWord,
    StartAnswer,
    VerifyAnswer,
    SubmitAnswerAndSave,
    SkipAnswer,
}

impl CidarenAttemptOperation {
    pub const fn operation_type(self) -> &'static str {
        match self {
            Self::SubmitChoseWord => "cidaren.submit-chose-word.v1",
            Self::StartAnswer => "cidaren.start-answer.v1",
            Self::VerifyAnswer => "cidaren.verify-answer.v1",
            Self::SubmitAnswerAndSave => "cidaren.submit-answer-and-save.v1",
            Self::SkipAnswer => "cidaren.skip-answer.v1",
        }
    }
}

/// Sanitized current state of the Provider-private attempt machine.
///
/// `Issued` means a caller must durably record the result before accepting it.
/// If transport outcome is unknown, `mark_ambiguous` permanently prevents the
/// same one-time operation from being produced again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CidarenAttemptFlowStatus {
    ReadyToSelectWords,
    ReadyToStart,
    CurrentQuestion,
    CurrentReadingCard,
    ReadyToVerify,
    ReadyToAdvance,
    Issued(CidarenAttemptOperation),
    Receipt(CidarenAssessmentReceiptKind),
    Ambiguous(CidarenAttemptOperation),
    FailedClosed(CidarenAttemptOperation),
}

/// Provider-private, single-step Cidaren attempt lifecycle.
///
/// The pre-Question subset is consumed by the public `QuestionInventory`
/// adapter. Post-materialization Verify/advance/skip edges remain available
/// through this Provider-private surface until Core's `QuestionSession` step
/// contract persists every `Issued` marker. The machine prevents matching
/// batches, topic codes in Drafts and ambiguous mutation replay in both paths.
pub struct CidarenAttemptFlow {
    binding: CidarenAssessmentBinding,
    context_binding: [u8; 32],
    flow_binding: [u8; 32],
    remote_task_id: String,
    task_id: TaskId,
    position: u32,
    phase: Option<CidarenAttemptPhase>,
    last_response_binding: Option<CidarenQuestionResponseBinding>,
}

#[derive(Clone, Copy)]
struct CidarenQuestionResponseBinding {
    response_digest: [u8; 32],
    received_at: Timestamp,
}

enum CidarenAttemptPhase {
    ReadyToSelectWords(CidarenWordSelectionPlan),
    ReadyToStart,
    CurrentQuestion(Box<CidarenCurrentQuestion>),
    CurrentReadingCard(ParsedCidarenReadingCard),
    ReadyToVerify {
        current: Box<CidarenCurrentQuestion>,
        topic_code: Zeroizing<String>,
        remaining: VecDeque<CidarenWireAnswer>,
        verified_steps: u32,
    },
    ReadyToAdvance {
        current: Box<CidarenCurrentQuestion>,
        topic_code: Zeroizing<String>,
        verified_steps: u32,
    },
    Issued {
        operation: CidarenAttemptOperation,
        continuation: CidarenAttemptContinuation,
    },
    Receipt {
        kind: CidarenAssessmentReceiptKind,
        message_sanitized: Option<String>,
        received_at: asterism_domain::Timestamp,
    },
    Ambiguous(CidarenAttemptOperation),
    FailedClosed(CidarenAttemptOperation),
}

struct CidarenCurrentQuestion {
    parsed: ParsedCidarenAttemptQuestion,
    question: Question,
}

enum CidarenAttemptContinuation {
    SelectWords,
    Start,
    Verify {
        current: Box<CidarenCurrentQuestion>,
        remaining: VecDeque<CidarenWireAnswer>,
        verified_steps: u32,
    },
    NextStep {
        position: u32,
    },
}

/// Opaque one-shot command produced only after the flow enters `Issued`.
/// Debug output contains no answer, topic code, word map or account identity.
pub struct CidarenIssuedCommand {
    context_binding: [u8; 32],
    flow_binding: [u8; 32],
    operation: CidarenAttemptOperation,
    action: CidarenIssuedAction,
    delay_before_execute_seconds: u64,
}

enum CidarenIssuedAction {
    SubmitChoseWord(CidarenMutationRequest),
    StartAnswer(CidarenStartAnswerRequest),
    VerifyAnswer(CidarenMutationRequest),
    SubmitAnswerAndSave(CidarenMutationRequest),
    SkipAnswer(CidarenMutationRequest),
}

impl CidarenIssuedAction {
    fn request_digest(&self) -> [u8; 32] {
        match self {
            Self::StartAnswer(request) => request.request_digest(),
            Self::SubmitChoseWord(request)
            | Self::VerifyAnswer(request)
            | Self::SubmitAnswerAndSave(request)
            | Self::SkipAnswer(request) => request.request_digest(),
        }
    }
}

/// Successful transport result bound to exactly one flow and operation.
pub struct CidarenIssuedOutcome {
    flow_binding: [u8; 32],
    operation: CidarenAttemptOperation,
    response: CidarenAssessmentResponse,
    response_digest: [u8; 32],
    received_at: asterism_domain::Timestamp,
}

/// Provider-private material Core needs to atomically create or rotate a
/// `QuestionSession` after one accepted Cidaren operation yielded a real
/// Question. The artifact value remains zeroizing and Debug-redacted.
pub struct CidarenQuestionMaterialization {
    question: Question,
    artifact: EncodedCidarenQuestionArtifact,
    phase: &'static str,
    response_digest: [u8; 32],
    received_at: Timestamp,
}

impl CidarenQuestionMaterialization {
    pub fn question(&self) -> &Question {
        &self.question
    }

    pub fn artifact(&self) -> &EncodedCidarenQuestionArtifact {
        &self.artifact
    }

    pub const fn phase(&self) -> &'static str {
        self.phase
    }

    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    pub const fn received_at(&self) -> Timestamp {
        self.received_at
    }

    pub fn into_parts(
        self,
    ) -> (
        Question,
        EncodedCidarenQuestionArtifact,
        &'static str,
        [u8; 32],
        Timestamp,
    ) {
        (
            self.question,
            self.artifact,
            self.phase,
            self.response_digest,
            self.received_at,
        )
    }
}

impl fmt::Debug for CidarenQuestionMaterialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenQuestionMaterialization")
            .field("question_id", &self.question.id)
            .field("artifact", &self.artifact)
            .field("phase", &self.phase)
            .field("response_digest", &self.response_digest)
            .field("received_at", &self.received_at)
            .finish()
    }
}

/// Encrypted continuation input for accepted pre-Question stages. Initial
/// continuations have no response binding; rotations produced by a remote
/// acknowledgement retain its digest and observation time.
pub struct CidarenPreQuestionContinuation {
    artifact: EncodedCidarenPreQuestionArtifact,
    phase: &'static str,
    response_binding: Option<CidarenQuestionResponseBinding>,
}

impl CidarenPreQuestionContinuation {
    pub fn artifact(&self) -> &EncodedCidarenPreQuestionArtifact {
        &self.artifact
    }

    pub const fn phase(&self) -> &'static str {
        self.phase
    }

    pub const fn response_digest(&self) -> Option<[u8; 32]> {
        match self.response_binding {
            Some(binding) => Some(binding.response_digest),
            None => None,
        }
    }

    pub const fn received_at(&self) -> Option<Timestamp> {
        match self.response_binding {
            Some(binding) => Some(binding.received_at),
            None => None,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        EncodedCidarenPreQuestionArtifact,
        &'static str,
        Option<[u8; 32]>,
        Option<Timestamp>,
    ) {
        (
            self.artifact,
            self.phase,
            self.response_binding.map(|binding| binding.response_digest),
            self.response_binding.map(|binding| binding.received_at),
        )
    }
}

impl fmt::Debug for CidarenPreQuestionContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenPreQuestionContinuation")
            .field("artifact", &self.artifact)
            .field("phase", &self.phase)
            .field("has_response", &self.response_binding.is_some())
            .finish()
    }
}

impl CidarenAttemptFlow {
    /// Creates an attempt from one fresh Core Task detail. An optional word
    /// selection plan must have been derived from the same stable Task ID.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign context, stale Task detail, unsafe
    /// correlation binding or cross-Task word plan.
    pub fn try_new(
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
        word_selection: Option<CidarenWordSelectionPlan>,
    ) -> ProviderResult<Self> {
        validate_context(context)?;
        let binding = CidarenAssessmentBinding::from_fresh_detail(remote_task_id, detail)?;
        if word_selection
            .as_ref()
            .is_some_and(|plan| !plan.is_bound_to(remote_task_id))
        {
            return Err(remote_changed(
                "Cidaren word-selection plan belongs to another Task",
            ));
        }
        let context_binding = context_binding(context);
        let flow_binding = flow_binding(context_binding, task_id, remote_task_id);
        let phase = word_selection.map_or(
            CidarenAttemptPhase::ReadyToStart,
            CidarenAttemptPhase::ReadyToSelectWords,
        );
        Ok(Self {
            binding,
            context_binding,
            flow_binding,
            remote_task_id: remote_task_id.to_owned(),
            task_id,
            position: 1,
            phase: Some(phase),
            last_response_binding: None,
        })
    }

    /// Restores one encrypted pre-Question phase after fresh account/Task
    /// rebinding. A word-selection phase requires a freshly rediscovered plan;
    /// the potentially large prior map is never trusted from storage.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a stale Task, missing/foreign fresh plan, or a
    /// phase that carries an unexpected plan.
    pub fn restore_pre_question(
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
        artifact: CidarenPreQuestionArtifact,
        fresh_word_selection: Option<CidarenWordSelectionPlan>,
    ) -> ProviderResult<Self> {
        validate_context(context)?;
        let binding = CidarenAssessmentBinding::from_fresh_detail(remote_task_id, detail)?;
        let phase = match (artifact.into_state(), fresh_word_selection) {
            (CidarenPreQuestionState::ReadyToSelectWords, Some(plan))
                if plan.is_bound_to(remote_task_id) =>
            {
                CidarenAttemptPhase::ReadyToSelectWords(plan)
            }
            (CidarenPreQuestionState::ReadyToStart, None) => CidarenAttemptPhase::ReadyToStart,
            (CidarenPreQuestionState::ReadingCard(card), None) => {
                CidarenAttemptPhase::CurrentReadingCard(card)
            }
            _ => {
                return Err(remote_changed(
                    "Cidaren pre-Question recovery plan does not match its phase",
                ));
            }
        };
        let context_binding = context_binding(context);
        Ok(Self {
            binding,
            context_binding,
            flow_binding: flow_binding(context_binding, task_id, remote_task_id),
            remote_task_id: remote_task_id.to_owned(),
            task_id,
            position: match &phase {
                CidarenAttemptPhase::CurrentReadingCard(card) => card.position(),
                _ => 1,
            },
            phase: Some(phase),
            last_response_binding: None,
        })
    }

    /// Restores a post-materialization Question phase from its encrypted
    /// artifact and the immutable Draft selection bound to the same snapshot.
    /// Matching relation progress is rebuilt deterministically and the
    /// already accepted prefix is removed according to `verified_steps`.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a stale Task/Question, mismatched phase,
    /// missing Draft selection, or an impossible relation checkpoint.
    pub fn restore_question(
        context: &ProviderContext,
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
        artifact: &CidarenQuestionArtifact,
        phase: &str,
        question: &Question,
        selected: Option<&SelectedAnswer>,
    ) -> ProviderResult<Self> {
        validate_context(context)?;
        let binding = CidarenAssessmentBinding::from_fresh_detail(remote_task_id, detail)?;
        let verified_steps = artifact.verified_steps();
        let parsed = ParsedCidarenAttemptQuestion::from_artifact(
            artifact.topic_code().to_owned(),
            remote_task_id.to_owned(),
            question,
        )?;
        let topic_code = Zeroizing::new(artifact.topic_code().to_owned());
        let current = Box::new(CidarenCurrentQuestion {
            parsed,
            question: question.clone(),
        });
        let restored_phase = match phase {
            CIDAREN_QUESTION_ARTIFACT_PHASE if verified_steps == 0 => {
                CidarenAttemptPhase::CurrentQuestion(current)
            }
            CIDAREN_READY_TO_VERIFY_PHASE => {
                let selected = selected.ok_or_else(|| {
                    invalid_state("Cidaren matching recovery requires its immutable selection")
                })?;
                let mut remaining = wire_answers(question, selected)?;
                let verified_steps = usize::try_from(verified_steps).map_err(|_| {
                    protocol_drift("Cidaren verified relation checkpoint is unrepresentable")
                })?;
                if verified_steps == 0 || verified_steps >= remaining.len() {
                    return Err(protocol_drift(
                        "Cidaren ready-to-verify checkpoint does not match its selection",
                    ));
                }
                for _ in 0..verified_steps {
                    remaining.pop_front();
                }
                CidarenAttemptPhase::ReadyToVerify {
                    current,
                    topic_code,
                    remaining,
                    verified_steps: u32::try_from(verified_steps).map_err(|_| {
                        protocol_drift("Cidaren verified relation checkpoint exceeds its bound")
                    })?,
                }
            }
            CIDAREN_READY_TO_ADVANCE_PHASE => {
                let selected = selected.ok_or_else(|| {
                    invalid_state("Cidaren advance recovery requires its immutable selection")
                })?;
                let answers = wire_answers(question, selected)?;
                if verified_steps == 0
                    || usize::try_from(verified_steps).ok() != Some(answers.len())
                {
                    return Err(protocol_drift(
                        "Cidaren ready-to-advance checkpoint does not match its selection",
                    ));
                }
                CidarenAttemptPhase::ReadyToAdvance {
                    current,
                    topic_code,
                    verified_steps,
                }
            }
            _ => {
                return Err(protocol_drift(
                    "Cidaren Question artifact phase shape is invalid",
                ));
            }
        };
        let context_binding = context_binding(context);
        Ok(Self {
            binding,
            context_binding,
            flow_binding: flow_binding(context_binding, question.task_id, remote_task_id),
            remote_task_id: remote_task_id.to_owned(),
            task_id: question.task_id,
            position: question.position,
            phase: Some(restored_phase),
            last_response_binding: None,
        })
    }

    pub fn status(&self) -> CidarenAttemptFlowStatus {
        match self.phase() {
            CidarenAttemptPhase::ReadyToSelectWords(_) => {
                CidarenAttemptFlowStatus::ReadyToSelectWords
            }
            CidarenAttemptPhase::ReadyToStart => CidarenAttemptFlowStatus::ReadyToStart,
            CidarenAttemptPhase::CurrentQuestion(_) => CidarenAttemptFlowStatus::CurrentQuestion,
            CidarenAttemptPhase::CurrentReadingCard(_) => {
                CidarenAttemptFlowStatus::CurrentReadingCard
            }
            CidarenAttemptPhase::ReadyToVerify { .. } => CidarenAttemptFlowStatus::ReadyToVerify,
            CidarenAttemptPhase::ReadyToAdvance { .. } => CidarenAttemptFlowStatus::ReadyToAdvance,
            CidarenAttemptPhase::Issued { operation, .. } => {
                CidarenAttemptFlowStatus::Issued(*operation)
            }
            CidarenAttemptPhase::Receipt { kind, .. } => CidarenAttemptFlowStatus::Receipt(*kind),
            CidarenAttemptPhase::Ambiguous(operation) => {
                CidarenAttemptFlowStatus::Ambiguous(*operation)
            }
            CidarenAttemptPhase::FailedClosed(operation) => {
                CidarenAttemptFlowStatus::FailedClosed(*operation)
            }
        }
    }

    /// Encodes the current pre-Question phase for Main's encrypted attempt
    /// continuation. This never copies the fresh word map into storage.
    ///
    /// # Errors
    ///
    /// Returns a typed error if a reading-card payload cannot be represented
    /// by the bounded artifact schema.
    pub fn pre_question_continuation(
        &self,
    ) -> ProviderResult<Option<CidarenPreQuestionContinuation>> {
        let artifact = match self.phase() {
            CidarenAttemptPhase::ReadyToSelectWords(_) => {
                CidarenPreQuestionArtifact::ready_to_select_words(
                    self.task_id,
                    &self.remote_task_id,
                )
            }
            CidarenAttemptPhase::ReadyToStart => {
                CidarenPreQuestionArtifact::ready_to_start(self.task_id, &self.remote_task_id)
            }
            CidarenAttemptPhase::CurrentReadingCard(card) => {
                CidarenPreQuestionArtifact::reading_card(self.task_id, &self.remote_task_id, card)?
            }
            CidarenAttemptPhase::Receipt {
                kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
                ..
            } => CidarenPreQuestionArtifact::ready_to_select_words(
                self.task_id,
                &self.remote_task_id,
            ),
            _ => return Ok(None),
        };
        let phase = artifact.phase();
        Ok(Some(CidarenPreQuestionContinuation {
            artifact: artifact.encode()?,
            phase,
            response_binding: self.last_response_binding,
        }))
    }

    pub fn current_question(&self) -> Option<&Question> {
        match self.phase() {
            CidarenAttemptPhase::CurrentQuestion(current)
            | CidarenAttemptPhase::ReadyToVerify { current, .. }
            | CidarenAttemptPhase::ReadyToAdvance { current, .. } => Some(&current.question),
            _ => None,
        }
    }

    /// Builds the exact real Question plus encrypted-at-rest Provider artifact
    /// input produced by the latest accepted remote response. The same method
    /// also emits rotated artifacts after sequential `VerifyAnswer` steps.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the in-memory Question/artifact binding is
    /// invalid or no accepted response is bound to the current phase.
    pub fn current_question_materialization(
        &self,
    ) -> ProviderResult<Option<CidarenQuestionMaterialization>> {
        let (current, rotated_topic_code, verified_steps, phase) = match self.phase() {
            CidarenAttemptPhase::CurrentQuestion(current) => {
                (current.as_ref(), None, 0, CIDAREN_QUESTION_ARTIFACT_PHASE)
            }
            CidarenAttemptPhase::ReadyToVerify {
                current,
                topic_code,
                verified_steps,
                ..
            } => (
                current.as_ref(),
                Some(topic_code.as_str()),
                *verified_steps,
                CIDAREN_READY_TO_VERIFY_PHASE,
            ),
            CidarenAttemptPhase::ReadyToAdvance {
                current,
                topic_code,
                verified_steps,
            } => (
                current.as_ref(),
                Some(topic_code.as_str()),
                *verified_steps,
                CIDAREN_READY_TO_ADVANCE_PHASE,
            ),
            _ => return Ok(None),
        };
        let binding = self.last_response_binding.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren current Question has no accepted response binding",
            )
        })?;
        let mut artifact =
            CidarenQuestionArtifact::from_parsed(&current.parsed, &current.question)?;
        if let Some(topic_code) = rotated_topic_code {
            artifact = artifact.checkpoint_after_verify(topic_code, verified_steps)?;
        }
        Ok(Some(CidarenQuestionMaterialization {
            question: current.question.clone(),
            artifact: artifact.encode()?,
            phase,
            response_digest: binding.response_digest,
            received_at: binding.received_at,
        }))
    }

    pub fn current_reading_card(&self) -> Option<&ParsedCidarenReadingCard> {
        match self.phase() {
            CidarenAttemptPhase::CurrentReadingCard(card) => Some(card),
            _ => None,
        }
    }

    /// Returns the donor-observed completed/total counters for the current
    /// remote step, independently from this machine's local position.
    pub fn current_remote_progress(&self) -> Option<CidarenAttemptProgress> {
        match self.phase() {
            CidarenAttemptPhase::CurrentQuestion(current)
            | CidarenAttemptPhase::ReadyToVerify { current, .. }
            | CidarenAttemptPhase::ReadyToAdvance { current, .. } => {
                current.parsed.remote_progress()
            }
            CidarenAttemptPhase::CurrentReadingCard(card) => card.remote_progress(),
            _ => None,
        }
    }

    /// Rebinds a word-selection-required receipt to a freshly constructed plan.
    /// The exact fresh Task detail is checked again before another mutation can
    /// be issued.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless selection is required or the fresh
    /// detail/plan still binds the same Task.
    pub fn supply_word_selection(
        &mut self,
        detail: &RemoteTaskDetail,
        plan: CidarenWordSelectionPlan,
    ) -> ProviderResult<()> {
        if self.status()
            != CidarenAttemptFlowStatus::Receipt(
                CidarenAssessmentReceiptKind::WordSelectionRequired,
            )
        {
            return Err(invalid_state(
                "Cidaren word selection is not required by the current attempt",
            ));
        }
        if !plan.is_bound_to(&self.remote_task_id) {
            return Err(remote_changed(
                "Cidaren word-selection plan belongs to another Task",
            ));
        }
        self.binding = CidarenAssessmentBinding::from_fresh_detail(&self.remote_task_id, detail)?;
        self.phase = Some(CidarenAttemptPhase::ReadyToSelectWords(plan));
        Ok(())
    }

    /// Issues one task-bound `SubmitChoseWord` command.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless a fresh word-selection plan is ready.
    pub fn issue_word_selection(
        &mut self,
        request_at: Timestamp,
    ) -> ProviderResult<CidarenIssuedCommand> {
        let timestamp_millis = request_timestamp_millis(request_at)?;
        let phase = self.take_phase();
        let CidarenAttemptPhase::ReadyToSelectWords(plan) = phase else {
            self.phase = Some(phase);
            return Err(invalid_state(
                "Cidaren word selection cannot be issued in the current state",
            ));
        };
        let request =
            match build_submit_chose_word_request(&self.binding, plan.word_map(), timestamp_millis)
            {
                Ok(request) => request,
                Err(error) => {
                    self.phase = Some(CidarenAttemptPhase::ReadyToSelectWords(plan));
                    return Err(error);
                }
            };
        self.issue(
            CidarenAttemptOperation::SubmitChoseWord,
            CidarenAttemptContinuation::SelectWords,
            CidarenIssuedAction::SubmitChoseWord(request),
            0,
        )
    }

    /// Issues one non-replayable `StartAnswer` command.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless word selection is complete and start is ready.
    pub fn issue_start(&mut self, request_at: Timestamp) -> ProviderResult<CidarenIssuedCommand> {
        let timestamp_millis = request_timestamp_millis(request_at)?;
        let phase = self.take_phase();
        if !matches!(phase, CidarenAttemptPhase::ReadyToStart) {
            self.phase = Some(phase);
            return Err(invalid_state(
                "Cidaren StartAnswer cannot be issued in the current state",
            ));
        }
        let request = build_start_answer_request(&self.binding, timestamp_millis);
        self.issue(
            CidarenAttemptOperation::StartAnswer,
            CidarenAttemptContinuation::Start,
            CidarenIssuedAction::StartAnswer(request),
            0,
        )
    }

    /// Issues only the first `VerifyAnswer` for the selected normalized answer.
    /// Matching answers remain queued and each later relation requires an
    /// independently accepted rotated topic code before `issue_next_verify`.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a foreign/malformed answer or unless the flow
    /// is positioned on its current Question.
    pub fn issue_selected_answer(
        &mut self,
        selected: &SelectedAnswer,
        request_at: Timestamp,
    ) -> ProviderResult<CidarenIssuedCommand> {
        let timestamp_millis = request_timestamp_millis(request_at)?;
        let (topic_code, mut answers) = match self.phase() {
            CidarenAttemptPhase::CurrentQuestion(current) => (
                Zeroizing::new(current.parsed.topic_code().to_owned()),
                wire_answers(&current.question, selected)?,
            ),
            _ => {
                return Err(invalid_state(
                    "Cidaren VerifyAnswer requires the current Question",
                ));
            }
        };
        let answer = answers
            .pop_front()
            .ok_or_else(|| invalid_state("Cidaren normalized answer has no Verify step"))?;
        let request =
            build_verify_answer_request(&self.binding, &topic_code, &answer, timestamp_millis)?;
        let CidarenAttemptPhase::CurrentQuestion(current) = self.take_phase() else {
            unreachable!("Cidaren current Question was checked before request construction");
        };
        self.issue(
            CidarenAttemptOperation::VerifyAnswer,
            CidarenAttemptContinuation::Verify {
                current,
                remaining: answers,
                verified_steps: 0,
            },
            CidarenIssuedAction::VerifyAnswer(request),
            0,
        )
    }

    /// Issues the next matching relation only after the preceding response has
    /// supplied a fresh topic code.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless another sequential relation is ready.
    pub fn issue_next_verify(
        &mut self,
        request_at: Timestamp,
    ) -> ProviderResult<CidarenIssuedCommand> {
        let timestamp_millis = request_timestamp_millis(request_at)?;
        let phase = self.take_phase();
        let CidarenAttemptPhase::ReadyToVerify {
            current,
            topic_code,
            mut remaining,
            verified_steps,
        } = phase
        else {
            self.phase = Some(phase);
            return Err(invalid_state(
                "Cidaren has no sequential VerifyAnswer step ready",
            ));
        };
        let Some(answer) = remaining.pop_front() else {
            self.phase = Some(CidarenAttemptPhase::ReadyToVerify {
                current,
                topic_code,
                remaining,
                verified_steps,
            });
            return Err(invalid_state(
                "Cidaren normalized answer has no Verify step",
            ));
        };
        let request = match build_verify_answer_request(
            &self.binding,
            &topic_code,
            &answer,
            timestamp_millis,
        ) {
            Ok(request) => request,
            Err(error) => {
                remaining.push_front(answer);
                self.phase = Some(CidarenAttemptPhase::ReadyToVerify {
                    current,
                    topic_code,
                    remaining,
                    verified_steps,
                });
                return Err(error);
            }
        };
        self.issue(
            CidarenAttemptOperation::VerifyAnswer,
            CidarenAttemptContinuation::Verify {
                current,
                remaining,
                verified_steps,
            },
            CidarenIssuedAction::VerifyAnswer(request),
            0,
        )
    }

    /// Issues `SubmitAnswerAndSave` for a verified Question or reading card.
    /// Reported duration is selected stably from the immutable settings
    /// snapshot rather than chosen again after a crash.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless a verified Question or reading card is
    /// ready, or when the bounded position cannot advance.
    pub fn issue_advance(
        &mut self,
        settings: &CidarenRuntimeSettings,
        request_at: Timestamp,
    ) -> ProviderResult<CidarenIssuedCommand> {
        let timestamp_millis = request_timestamp_millis(request_at)?;
        let phase = self.take_phase();
        let delay_entropy = step_entropy(self.flow_binding, self.position, b"advance-delay");
        let (topic_code, delay_before_execute_seconds) = match &phase {
            CidarenAttemptPhase::ReadyToAdvance { topic_code, .. } => (
                topic_code.as_str(),
                settings.verified_advance_delay_seconds(&delay_entropy),
            ),
            CidarenAttemptPhase::CurrentReadingCard(card) => (
                card.topic_code(),
                settings.reading_advance_delay_seconds(&delay_entropy),
            ),
            _ => {
                self.phase = Some(phase);
                return Err(invalid_state("Cidaren SubmitAnswerAndSave is not ready"));
            }
        };
        let next_position = match next_position(self.position) {
            Ok(position) => position,
            Err(error) => {
                self.phase = Some(phase);
                return Err(error);
            }
        };
        let entropy = step_entropy(self.flow_binding, self.position, b"advance");
        let time_spent_millis = settings.reported_answer_time_millis(&entropy);
        let request = match build_submit_answer_and_save_request(
            &self.binding,
            topic_code,
            time_spent_millis,
            timestamp_millis,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.phase = Some(phase);
                return Err(error);
            }
        };
        self.issue(
            CidarenAttemptOperation::SubmitAnswerAndSave,
            CidarenAttemptContinuation::NextStep {
                position: next_position,
            },
            CidarenIssuedAction::SubmitAnswerAndSave(request),
            delay_before_execute_seconds,
        )
    }

    /// Issues donor-observed `SkipAnswer` only for the current unresolved
    /// Question. The current topic code is consumed and cannot be replayed.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless the current stage is an unresolved
    /// Question or the bounded position cannot advance.
    pub fn issue_skip(
        &mut self,
        settings: &CidarenRuntimeSettings,
        request_at: Timestamp,
    ) -> ProviderResult<CidarenIssuedCommand> {
        let timestamp_millis = request_timestamp_millis(request_at)?;
        let CidarenAttemptPhase::CurrentQuestion(current) = self.phase() else {
            return Err(invalid_state(
                "Cidaren SkipAnswer requires the current Question",
            ));
        };
        let next_position = next_position(self.position)?;
        let request = build_skip_answer_request(
            &self.binding,
            current.parsed.topic_code(),
            settings.skip_reported_time_millis(),
            timestamp_millis,
        )?;
        let _current = self.take_phase();
        self.issue(
            CidarenAttemptOperation::SkipAnswer,
            CidarenAttemptContinuation::NextStep {
                position: next_position,
            },
            CidarenIssuedAction::SkipAnswer(request),
            0,
        )
    }

    /// Returns the stable donor-compatible delay which Core should schedule
    /// before operating on the next step.
    pub fn inter_step_delay_seconds(&self, settings: &CidarenRuntimeSettings) -> u64 {
        let entropy = step_entropy(self.flow_binding, self.position, b"delay");
        settings.answer_delay_seconds(&entropy)
    }

    /// Produces a bounded Core receipt only after the donor returned its
    /// terminal completion acknowledgement. This remains acknowledgement
    /// context for fresh verification and never marks the Task successful.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless the flow is terminally completed.
    pub fn completion_receipt(&self) -> ProviderResult<SubmissionReceipt> {
        let CidarenAttemptPhase::Receipt {
            kind: CidarenAssessmentReceiptKind::Completed,
            message_sanitized,
            received_at,
        } = self.phase()
        else {
            return Err(invalid_state(
                "Cidaren attempt has no terminal completion receipt",
            ));
        };
        let receipt = SubmissionReceipt {
            remote_status: "completed".to_owned(),
            message_sanitized: message_sanitized.clone(),
            provider_trace_id: None,
            received_at: *received_at,
        };
        receipt
            .validate()
            .map_err(|_| invalid_response("Cidaren completion receipt is invalid"))?;
        Ok(receipt)
    }

    /// Returns the definite terminal receipt together with the exact raw
    /// response digest accepted by this flow. Non-terminal phases return
    /// `None`; malformed terminal response binding fails closed.
    ///
    /// # Errors
    ///
    /// Returns a typed error if a completed receipt lost or changed its
    /// response binding.
    pub fn terminal_completion(&self) -> ProviderResult<Option<(SubmissionReceipt, [u8; 32])>> {
        if self.status()
            != CidarenAttemptFlowStatus::Receipt(CidarenAssessmentReceiptKind::Completed)
        {
            return Ok(None);
        }
        let receipt = self.completion_receipt()?;
        let binding = self
            .last_response_binding
            .ok_or_else(|| invalid_state("Cidaren terminal receipt has no response binding"))?;
        if binding.received_at != receipt.received_at || binding.response_digest == [0; 32] {
            return Err(protocol_drift(
                "Cidaren terminal receipt response binding is inconsistent",
            ));
        }
        Ok(Some((receipt, binding.response_digest)))
    }

    /// Accepts only a response produced by the exact issued command. Each
    /// successful response advances one state-machine edge.
    ///
    /// # Errors
    ///
    /// Returns a typed error for cross-flow outcomes, unexpected response
    /// semantics, invalid rotated tokens or malformed next steps.
    pub fn accept(&mut self, outcome: CidarenIssuedOutcome) -> ProviderResult<()> {
        if outcome.flow_binding != self.flow_binding {
            return Err(remote_changed(
                "Cidaren attempt outcome belongs to another execution",
            ));
        }
        let phase = self.take_phase();
        let CidarenAttemptPhase::Issued {
            operation,
            continuation,
        } = phase
        else {
            self.phase = Some(phase);
            return Err(invalid_state(
                "Cidaren attempt has no issued operation to accept",
            ));
        };
        if operation != outcome.operation {
            self.phase = Some(CidarenAttemptPhase::Issued {
                operation,
                continuation,
            });
            return Err(remote_changed(
                "Cidaren attempt outcome operation does not match",
            ));
        }
        let response_binding = CidarenQuestionResponseBinding {
            response_digest: outcome.response_digest,
            received_at: outcome.received_at,
        };
        let applied = self.apply_response(continuation, outcome.response, outcome.received_at);
        if applied.is_err() {
            self.phase = Some(CidarenAttemptPhase::FailedClosed(operation));
        } else {
            self.last_response_binding = Some(response_binding);
        }
        applied
    }

    /// Permanently blocks replay after a request may have reached the remote
    /// endpoint but no unambiguous response was obtained.
    ///
    /// # Errors
    ///
    /// Returns a typed error unless an operation is currently issued.
    pub fn mark_ambiguous(&mut self) -> ProviderResult<()> {
        let phase = self.take_phase();
        let CidarenAttemptPhase::Issued { operation, .. } = phase else {
            self.phase = Some(phase);
            return Err(invalid_state(
                "Cidaren attempt has no issued operation to mark ambiguous",
            ));
        };
        self.phase = Some(CidarenAttemptPhase::Ambiguous(operation));
        Ok(())
    }

    fn issue(
        &mut self,
        operation: CidarenAttemptOperation,
        continuation: CidarenAttemptContinuation,
        action: CidarenIssuedAction,
        delay_before_execute_seconds: u64,
    ) -> ProviderResult<CidarenIssuedCommand> {
        if self.phase.is_some() {
            return Err(invalid_state(
                "Cidaren attempt tried to overlap remote mutations",
            ));
        }
        self.phase = Some(CidarenAttemptPhase::Issued {
            operation,
            continuation,
        });
        Ok(CidarenIssuedCommand {
            context_binding: self.context_binding,
            flow_binding: self.flow_binding,
            operation,
            action,
            delay_before_execute_seconds,
        })
    }

    fn apply_response(
        &mut self,
        continuation: CidarenAttemptContinuation,
        response: CidarenAssessmentResponse,
        received_at: asterism_domain::Timestamp,
    ) -> ProviderResult<()> {
        match continuation {
            CidarenAttemptContinuation::SelectWords => match response {
                CidarenAssessmentResponse::Receipt {
                    kind: CidarenAssessmentReceiptKind::Accepted,
                    ..
                } => {
                    self.phase = Some(CidarenAttemptPhase::ReadyToStart);
                    Ok(())
                }
                CidarenAssessmentResponse::Receipt {
                    kind: CidarenAssessmentReceiptKind::Completed,
                    message_sanitized,
                } => {
                    self.phase = Some(CidarenAttemptPhase::Receipt {
                        kind: CidarenAssessmentReceiptKind::Completed,
                        message_sanitized,
                        received_at,
                    });
                    Ok(())
                }
                CidarenAssessmentResponse::Receipt {
                    kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
                    message_sanitized,
                } => {
                    self.phase = Some(CidarenAttemptPhase::Receipt {
                        kind: CidarenAssessmentReceiptKind::WordSelectionRequired,
                        message_sanitized,
                        received_at,
                    });
                    Ok(())
                }
                _ => Err(protocol_drift(
                    "Cidaren SubmitChoseWord returned an unexpected response",
                )),
            },
            CidarenAttemptContinuation::Start => {
                self.apply_next_step_response(response, self.position, received_at)
            }
            CidarenAttemptContinuation::Verify {
                current,
                remaining,
                verified_steps,
            } => {
                let CidarenAssessmentResponse::Payload(payload) = response else {
                    return Err(protocol_drift(
                        "Cidaren VerifyAnswer returned no rotated topic code",
                    ));
                };
                let topic_code = rotated_topic_code(payload.as_value())?;
                let verified_steps = verified_steps
                    .checked_add(1)
                    .filter(|steps| *steps <= MAX_VERIFIED_STEPS)
                    .ok_or_else(|| {
                        invalid_response("Cidaren VerifyAnswer step count exceeds its bound")
                    })?;
                self.phase = Some(if remaining.is_empty() {
                    CidarenAttemptPhase::ReadyToAdvance {
                        current,
                        topic_code,
                        verified_steps,
                    }
                } else {
                    CidarenAttemptPhase::ReadyToVerify {
                        current,
                        topic_code,
                        remaining,
                        verified_steps,
                    }
                });
                Ok(())
            }
            CidarenAttemptContinuation::NextStep { position } => {
                self.apply_next_step_response(response, position, received_at)
            }
        }
    }

    fn apply_next_step_response(
        &mut self,
        response: CidarenAssessmentResponse,
        position: u32,
        received_at: asterism_domain::Timestamp,
    ) -> ProviderResult<()> {
        self.position = position;
        match response {
            CidarenAssessmentResponse::Payload(payload) => {
                match parse_attempt_step(payload.as_value(), &self.remote_task_id, position)? {
                    ParsedCidarenAttemptStep::Question(parsed) => {
                        let question = parsed.to_question(self.task_id)?;
                        self.phase = Some(CidarenAttemptPhase::CurrentQuestion(Box::new(
                            CidarenCurrentQuestion { parsed, question },
                        )));
                    }
                    ParsedCidarenAttemptStep::ReadingCard(card) => {
                        self.phase = Some(CidarenAttemptPhase::CurrentReadingCard(card));
                    }
                }
                Ok(())
            }
            CidarenAssessmentResponse::Receipt {
                kind:
                    kind @ (CidarenAssessmentReceiptKind::Completed
                    | CidarenAssessmentReceiptKind::WordSelectionRequired),
                message_sanitized,
            } => {
                self.phase = Some(CidarenAttemptPhase::Receipt {
                    kind,
                    message_sanitized,
                    received_at,
                });
                Ok(())
            }
            CidarenAssessmentResponse::Receipt {
                kind: CidarenAssessmentReceiptKind::Accepted,
                ..
            } => Err(protocol_drift(
                "Cidaren assessment step returned only a generic acknowledgement",
            )),
        }
    }

    fn phase(&self) -> &CidarenAttemptPhase {
        self.phase
            .as_ref()
            .expect("Cidaren attempt phase is restored before returning")
    }

    fn take_phase(&mut self) -> CidarenAttemptPhase {
        self.phase
            .take()
            .expect("Cidaren attempt phase is restored before returning")
    }
}

impl fmt::Debug for CidarenAttemptFlow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenAttemptFlow")
            .field("task_binding", &"configured")
            .field("position", &self.position)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Drop for CidarenAttemptFlow {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        self.context_binding.zeroize();
        self.flow_binding.zeroize();
        if let Some(binding) = &mut self.last_response_binding {
            binding.response_digest.zeroize();
        }
    }
}

impl CidarenIssuedCommand {
    pub const fn operation(&self) -> CidarenAttemptOperation {
        self.operation
    }

    /// Stable Provider operation label for Core's durable operation ledger.
    pub const fn operation_type(&self) -> &'static str {
        self.operation.operation_type()
    }

    /// Digest of the exact credential-free path/query or path/body/signature
    /// already frozen into this one-shot command.
    pub fn request_digest(&self) -> [u8; 32] {
        self.action.request_digest()
    }

    /// Returns the stable donor-observed residence time which Core must
    /// schedule after durably recording this issued command and before
    /// executing its one-shot mutation.
    pub const fn delay_before_execute_seconds(&self) -> u64 {
        self.delay_before_execute_seconds
    }

    /// Executes exactly one prepared remote operation. The command is consumed
    /// even when transport fails, so the one-time topic code cannot be reused.
    ///
    /// The owning flow remains `Issued` until the caller accepts this result or
    /// marks the operation ambiguous.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a cross-account context or transport failure.
    pub async fn execute(
        self,
        transport: Arc<dyn CidarenAssessmentTransport>,
        context: &ProviderContext,
    ) -> ProviderResult<CidarenIssuedOutcome> {
        if context_binding(context) != self.context_binding {
            return Err(remote_changed(
                "Cidaren issued command received another account context",
            ));
        }
        let transport_outcome = match &self.action {
            CidarenIssuedAction::SubmitChoseWord(request) => {
                transport.submit_chose_word(context, request).await
            }
            CidarenIssuedAction::StartAnswer(request) => {
                transport.start_answer(context, request).await
            }
            CidarenIssuedAction::VerifyAnswer(request) => {
                transport.verify_answer(context, request).await
            }
            CidarenIssuedAction::SubmitAnswerAndSave(request) => {
                transport.submit_answer_and_save(context, request).await
            }
            CidarenIssuedAction::SkipAnswer(request) => {
                transport.skip_answer(context, request).await
            }
        }?;
        let (response, response_digest, received_at) = transport_outcome.into_parts();
        Ok(CidarenIssuedOutcome {
            flow_binding: self.flow_binding,
            operation: self.operation,
            response,
            response_digest,
            received_at,
        })
    }
}

impl fmt::Debug for CidarenIssuedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenIssuedCommand")
            .field("operation", &self.operation)
            .field(
                "delay_before_execute_seconds",
                &self.delay_before_execute_seconds,
            )
            .field("payload", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for CidarenIssuedOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenIssuedOutcome")
            .field("operation", &self.operation)
            .field("response", &self.response)
            .field("response_digest", &self.response_digest)
            .finish_non_exhaustive()
    }
}

impl CidarenIssuedOutcome {
    /// Digest of the exact raw response bytes accepted by the strict parser.
    pub const fn response_digest(&self) -> [u8; 32] {
        self.response_digest
    }

    /// Actual response observation time retained across delayed acceptance.
    pub const fn received_at(&self) -> Timestamp {
        self.received_at
    }
}

fn wire_answers(
    question: &Question,
    selected: &SelectedAnswer,
) -> ProviderResult<VecDeque<CidarenWireAnswer>> {
    question
        .validate()
        .map_err(|_| invalid_response("Cidaren current Question is invalid"))?;
    selected
        .answer
        .validate()
        .map_err(|_| invalid_response("Cidaren selected answer is invalid"))?;
    if selected.question_id != question.id {
        return Err(remote_changed(
            "Cidaren selected answer belongs to another Question",
        ));
    }
    let option_ids = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    match (&question.kind, &selected.answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
            if values.len() == 1
                && (option_ids.contains(values[0].as_str())
                    || valid_donor_third_parent_fallback(question, &values[0])) =>
        {
            Ok(VecDeque::from([CidarenWireAnswer::from_option_id(
                &values[0],
            )?]))
        }
        (QuestionKind::ShortAnswer, NormalizedAnswer::Texts(values)) if values.len() == 1 => {
            Ok(VecDeque::from([CidarenWireAnswer::from_text(&values[0])?]))
        }
        (QuestionKind::Matching, NormalizedAnswer::Pairs(pairs)) => {
            let relations = question
                .metadata_sanitized
                .get("relations")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| protocol_drift("Cidaren matching Question has no relations"))?;
            if relations.len() != pairs.len() || relations.is_empty() {
                return Err(remote_changed("Cidaren matching answer count changed"));
            }
            let by_relation = pairs
                .iter()
                .map(|pair| (pair.left.as_str(), pair.right.as_str()))
                .collect::<BTreeMap<_, _>>();
            if by_relation.len() != pairs.len() {
                return Err(invalid_response(
                    "Cidaren matching answer repeats a relation",
                ));
            }
            relations
                .iter()
                .map(|relation| {
                    let relation = relation
                        .as_str()
                        .ok_or_else(|| protocol_drift("Cidaren matching relation is invalid"))?;
                    let option = by_relation.get(relation).copied().ok_or_else(|| {
                        remote_changed("Cidaren matching answer omitted a relation")
                    })?;
                    if !option_ids.contains(option) {
                        return Err(remote_changed(
                            "Cidaren matching answer references another option",
                        ));
                    }
                    CidarenWireAnswer::from_option_id(option)
                })
                .collect()
        }
        _ => Err(invalid_response(
            "Cidaren selected answer does not match the current Question",
        )),
    }
}

fn valid_donor_third_parent_fallback(question: &Question, answer_id: &str) -> bool {
    matches!(
        question
            .metadata_sanitized
            .get("topic_mode")
            .and_then(serde_json::Value::as_i64),
        Some(41..=44)
    ) && question.options.iter().any(|option| {
        option
            .metadata_sanitized
            .get("top_level_index")
            .and_then(serde_json::Value::as_u64)
            == Some(2)
            && option
                .metadata_sanitized
                .get("parent_answer_id")
                .and_then(serde_json::Value::as_str)
                == Some(answer_id)
    })
}

fn rotated_topic_code(value: &serde_json::Value) -> ProviderResult<Zeroizing<String>> {
    value
        .as_object()
        .and_then(|object| object.get("topic_code"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_TOPIC_CODE_BYTES
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .map(|value| Zeroizing::new(value.to_owned()))
        .ok_or_else(|| protocol_drift("Cidaren VerifyAnswer returned an invalid topic code"))
}

fn validate_context(context: &ProviderContext) -> ProviderResult<()> {
    if context.provider_id.as_str() != "cidaren" {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren attempt received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren attempt requires an authenticated account binding",
        ));
    }
    if context.correlation_id.is_empty()
        || context.correlation_id.len() > MAX_CORRELATION_ID_BYTES
        || context.correlation_id.trim() != context.correlation_id
        || context.correlation_id.chars().any(char::is_control)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren attempt correlation binding is invalid",
        ));
    }
    Ok(())
}

fn context_binding(context: &ProviderContext) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"asterism:cidaren:attempt-context:v1\0")
        .chain_update(context.provider_id.as_str().as_bytes())
        .chain_update(b"\0")
        .chain_update(context.account_id.to_string().as_bytes())
        .finalize()
        .into()
}

fn flow_binding(context_binding: [u8; 32], task_id: TaskId, remote_task_id: &str) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"asterism:cidaren:attempt-flow:v1\0")
        .chain_update(context_binding)
        .chain_update(b"\0")
        .chain_update(task_id.to_string().as_bytes())
        .chain_update(b"\0")
        .chain_update(remote_task_id.as_bytes())
        .finalize()
        .into()
}

fn step_entropy(flow_binding: [u8; 32], position: u32, operation: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"asterism:cidaren:attempt-step:v1\0")
        .chain_update(flow_binding)
        .chain_update(position.to_be_bytes())
        .chain_update(b"\0")
        .chain_update(operation)
        .finalize()
        .into()
}

fn request_timestamp_millis(request_at: Timestamp) -> ProviderResult<u64> {
    u64::try_from(request_at.timestamp_millis()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren issued request timestamp is outside the supported range",
        )
    })
}

fn next_position(position: u32) -> ProviderResult<u32> {
    position
        .checked_add(1)
        .filter(|value| *value <= 100_000)
        .ok_or_else(|| invalid_response("Cidaren attempt exceeds the Question limit"))
}

fn invalid_state(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AnswerCandidateId, AnswerPair, AnswerSource, AssessmentClass, ProviderAccountId,
        ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::RemoteTask;
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        CidarenAnswerEvidenceBinding, CidarenStudyTaskDocument, build_word_selection_plan,
        parse_assessment_response, parse_attempt_question, parse_study_task_info_response,
    };

    struct FixtureTransport {
        responses: Mutex<VecDeque<CidarenAssessmentResponse>>,
        operations: Mutex<Vec<CidarenAttemptOperation>>,
    }

    #[async_trait]
    impl CidarenAssessmentTransport for FixtureTransport {
        async fn start_answer(
            &self,
            _context: &ProviderContext,
            _request: &CidarenStartAnswerRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::StartAnswer)
        }

        async fn verify_answer(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::VerifyAnswer)
        }

        async fn submit_answer_and_save(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::SubmitAnswerAndSave)
        }

        async fn skip_answer(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::SkipAnswer)
        }

        async fn submit_chose_word(
            &self,
            _context: &ProviderContext,
            _request: &CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            self.respond(CidarenAttemptOperation::SubmitChoseWord)
        }
    }

    impl FixtureTransport {
        fn respond(
            &self,
            operation: CidarenAttemptOperation,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            self.operations.lock().unwrap().push(operation);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| invalid_response("fixture response exhausted"))?;
            let digest = Sha256::new()
                .chain_update(b"cidaren-fixture-response:v1\0")
                .chain_update(operation.operation_type().as_bytes())
                .finalize()
                .into();
            crate::CidarenAssessmentTransportOutcome::try_new(response, digest, request_at())
        }
    }

    fn request_at() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn pre_question_start_restores_across_audit_correlation() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([response(&start_payload())])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let task_id = TaskId::new();
        let mut flow =
            CidarenAttemptFlow::try_new(&context, task_id, "class-task:2002", &detail(), None)
                .unwrap();
        let continuation = flow.pre_question_continuation().unwrap().unwrap();
        assert_eq!(continuation.phase(), crate::CIDAREN_READY_TO_START_PHASE);
        assert!(continuation.response_digest().is_none());
        assert!(continuation.received_at().is_none());
        let artifact_digest = continuation.artifact().digest();
        let (artifact, _, _, _) = continuation.into_parts();
        let value = artifact.into_secret_value();

        let recovered_context = recovered_context(&context);
        let outcome = flow
            .issue_start(request_at())
            .unwrap()
            .execute(transport, &recovered_context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert!(flow.current_question_materialization().unwrap().is_some());

        let decoded = CidarenPreQuestionArtifact::decode_bound(
            &value,
            artifact_digest,
            task_id,
            "class-task:2002",
        )
        .unwrap();
        let restored = CidarenAttemptFlow::restore_pre_question(
            &recovered_context,
            task_id,
            "class-task:2002",
            &detail(),
            decoded,
            None,
        )
        .unwrap();
        assert_eq!(restored.status(), CidarenAttemptFlowStatus::ReadyToStart);
    }

    #[test]
    fn word_selection_continuation_requires_fresh_plan_without_persisting_map() {
        let context = context();
        let task_id = TaskId::new();
        let (detail, plan) = word_selection_plan();
        let flow =
            CidarenAttemptFlow::try_new(&context, task_id, "class-task:2002", &detail, Some(plan))
                .unwrap();
        let continuation = flow.pre_question_continuation().unwrap().unwrap();
        assert_eq!(
            continuation.phase(),
            crate::CIDAREN_READY_TO_SELECT_WORDS_PHASE
        );
        let digest = continuation.artifact().digest();
        let (artifact, _, _, _) = continuation.into_parts();
        let value = artifact.into_secret_value();
        assert!(!String::from_utf8_lossy(value.expose_secret()).contains("alpha"));
        assert!(
            CidarenPreQuestionArtifact::decode_bound(&value, [7; 32], task_id, "class-task:2002",)
                .is_err()
        );
        assert!(
            CidarenPreQuestionArtifact::decode_bound(
                &value,
                digest,
                TaskId::new(),
                "class-task:2002",
            )
            .is_err()
        );
        let decoded =
            CidarenPreQuestionArtifact::decode_bound(&value, digest, task_id, "class-task:2002")
                .unwrap();
        assert!(
            CidarenAttemptFlow::restore_pre_question(
                &context,
                task_id,
                "class-task:2002",
                &detail,
                decoded,
                None,
            )
            .is_err()
        );

        let decoded =
            CidarenPreQuestionArtifact::decode_bound(&value, digest, task_id, "class-task:2002")
                .unwrap();
        let (_, fresh_plan) = word_selection_plan();
        let restored = CidarenAttemptFlow::restore_pre_question(
            &context,
            task_id,
            "class-task:2002",
            &detail,
            decoded,
            Some(fresh_plan),
        )
        .unwrap();
        assert_eq!(
            restored.status(),
            CidarenAttemptFlowStatus::ReadyToSelectWords
        );
    }

    #[tokio::test]
    async fn single_answer_advances_one_durable_operation_at_a_time() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([
                response(&start_payload()),
                response(&json!({"topic_code": "rotated-topic"})),
                receipt(CidarenAssessmentReceiptKind::Completed),
            ])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let task_id = TaskId::new();
        let mut flow =
            CidarenAttemptFlow::try_new(&context, task_id, "class-task:2002", &detail(), None)
                .unwrap();

        let command = flow.issue_start(request_at()).unwrap();
        assert_eq!(command.delay_before_execute_seconds(), 0);
        assert_eq!(command.operation_type(), "cidaren.start-answer.v1");
        assert_ne!(command.request_digest(), [0; 32]);
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Issued(CidarenAttemptOperation::StartAnswer)
        );
        let outcome = command.execute(transport.clone(), &context).await.unwrap();
        assert_ne!(outcome.response_digest(), [0; 32]);
        assert_eq!(outcome.received_at(), request_at());
        flow.accept(outcome).unwrap();
        let initial_materialization = flow.current_question_materialization().unwrap().unwrap();
        assert_eq!(
            initial_materialization.phase(),
            CIDAREN_QUESTION_ARTIFACT_PHASE
        );
        assert_ne!(initial_materialization.response_digest(), [0; 32]);
        assert_eq!(initial_materialization.received_at(), request_at());
        let initial_artifact_digest = initial_materialization.artifact().digest();
        assert!(!format!("{initial_materialization:?}").contains("synthetic-topic-code"));
        let remote_progress = flow.current_remote_progress().unwrap();
        assert_eq!(remote_progress.completed(), 1);
        assert_eq!(remote_progress.total(), 127);
        let recovered = recovered_context(&context);
        let (mut flow, question, verified_steps) =
            restore_materialization(&recovered, initial_materialization, None, None);
        assert_eq!(verified_steps, 0);
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::CurrentQuestion);
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer: NormalizedAnswer::Selections(vec!["n:1".to_owned()]),
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let command = flow.issue_selected_answer(&selected, request_at()).unwrap();
        assert_eq!(command.delay_before_execute_seconds(), 0);
        let outcome = command
            .execute(transport.clone(), &recovered)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToAdvance);
        let rotated_materialization = flow.current_question_materialization().unwrap().unwrap();
        assert_eq!(rotated_materialization.question().id, question.id);
        assert_eq!(
            rotated_materialization.phase(),
            CIDAREN_READY_TO_ADVANCE_PHASE
        );
        assert_ne!(
            rotated_materialization.artifact().digest(),
            initial_artifact_digest
        );
        let (mut flow, restored_question, verified_steps) =
            restore_materialization(&context, rotated_materialization, Some(&selected), None);
        assert_eq!(verified_steps, 1);
        assert_eq!(restored_question.id, question.id);
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToAdvance);
        let command = flow.issue_advance(&settings(), request_at()).unwrap();
        assert!((1..=2).contains(&command.delay_before_execute_seconds()));
        let outcome = command.execute(transport.clone(), &context).await.unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Receipt(CidarenAssessmentReceiptKind::Completed)
        );
        let receipt = flow.completion_receipt().unwrap();
        assert_eq!(receipt.remote_status, "completed");
        assert!(receipt.validate().is_ok());
        assert_eq!(
            *transport.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::VerifyAnswer,
                CidarenAttemptOperation::SubmitAnswerAndSave,
            ]
        );
        assert!(!format!("{flow:?}").contains("rotated-topic"));
    }

    #[tokio::test]
    async fn matching_requires_each_rotated_topic_before_the_next_relation() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([
                response(&matching_payload()),
                response(&json!({"topic_code": "rotated-one"})),
                response(&json!({"topic_code": "rotated-two"})),
            ])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail(),
            None,
        )
        .unwrap();
        let outcome = flow
            .issue_start(request_at())
            .unwrap()
            .execute(transport.clone(), &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        let initial_artifact_digest = flow
            .current_question_materialization()
            .unwrap()
            .unwrap()
            .artifact()
            .digest();
        let question = flow.current_question().unwrap().clone();
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
        let outcome = flow
            .issue_selected_answer(&selected, request_at())
            .unwrap()
            .execute(transport.clone(), &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToVerify);
        let first_rotation = flow.current_question_materialization().unwrap().unwrap();
        assert_eq!(first_rotation.question().id, question.id);
        assert_eq!(first_rotation.phase(), CIDAREN_READY_TO_VERIFY_PHASE);
        assert_ne!(first_rotation.artifact().digest(), initial_artifact_digest);
        let first_rotation_digest = first_rotation.artifact().digest();
        let recovered = recovered_context(&context);
        let (mut flow, restored_question, verified_steps) = restore_materialization(
            &recovered,
            first_rotation,
            Some(&selected),
            Some(CIDAREN_READY_TO_ADVANCE_PHASE),
        );
        assert_eq!(verified_steps, 1);
        assert_eq!(restored_question.id, question.id);
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToVerify);
        let outcome = flow
            .issue_next_verify(request_at())
            .unwrap()
            .execute(transport.clone(), &recovered)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToAdvance);
        let second_rotation = flow.current_question_materialization().unwrap().unwrap();
        assert_eq!(second_rotation.question().id, question.id);
        assert_eq!(second_rotation.phase(), CIDAREN_READY_TO_ADVANCE_PHASE);
        assert_ne!(second_rotation.artifact().digest(), first_rotation_digest);
        assert_eq!(
            *transport.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::VerifyAnswer,
                CidarenAttemptOperation::VerifyAnswer,
            ]
        );
    }

    #[tokio::test]
    async fn ambiguous_operation_cannot_be_reissued() {
        let context = context();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail(),
            None,
        )
        .unwrap();
        let command = flow.issue_start(request_at()).unwrap();
        drop(command);
        flow.mark_ambiguous().unwrap();
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Ambiguous(CidarenAttemptOperation::StartAnswer)
        );
        assert!(flow.issue_start(request_at()).is_err());
    }

    #[test]
    fn terminal_receipt_preserves_the_outcome_observation_time() {
        let context = context();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail(),
            None,
        )
        .unwrap();
        let command = flow.issue_start(request_at()).unwrap();
        let received_at = Utc.with_ymd_and_hms(2026, 8, 14, 1, 2, 3).unwrap();
        let outcome = CidarenIssuedOutcome {
            flow_binding: flow.flow_binding,
            operation: command.operation(),
            response: receipt(CidarenAssessmentReceiptKind::Completed),
            response_digest: [9; 32],
            received_at,
        };
        drop(command);

        flow.accept(outcome).unwrap();
        assert_eq!(flow.completion_receipt().unwrap().received_at, received_at);
    }

    #[tokio::test]
    async fn word_selection_requires_ack_before_start() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([receipt(
                CidarenAssessmentReceiptKind::Accepted,
            )])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let (detail, plan) = word_selection_plan();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail,
            Some(plan),
        )
        .unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToSelectWords);
        assert!(flow.issue_start(request_at()).is_err());
        let outcome = flow
            .issue_word_selection(request_at())
            .unwrap()
            .execute(transport.clone(), &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToStart);
        assert_eq!(
            *transport.operations.lock().unwrap(),
            [CidarenAttemptOperation::SubmitChoseWord]
        );
    }

    #[tokio::test]
    async fn terminal_word_selection_receipt_does_not_issue_start() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([receipt(
                CidarenAssessmentReceiptKind::Completed,
            )])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let (detail, plan) = word_selection_plan();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail,
            Some(plan),
        )
        .unwrap();
        let outcome = flow
            .issue_word_selection(request_at())
            .unwrap()
            .execute(transport.clone(), &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();

        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Receipt(CidarenAssessmentReceiptKind::Completed)
        );
        assert!(flow.completion_receipt().is_ok());
        assert!(flow.issue_start(request_at()).is_err());
        assert_eq!(
            *transport.operations.lock().unwrap(),
            [CidarenAttemptOperation::SubmitChoseWord]
        );
    }

    #[tokio::test]
    async fn repeated_word_selection_request_requires_fresh_plan() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([receipt(
                CidarenAssessmentReceiptKind::WordSelectionRequired,
            )])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let (detail, plan) = word_selection_plan();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail,
            Some(plan),
        )
        .unwrap();
        let outcome = flow
            .issue_word_selection(request_at())
            .unwrap()
            .execute(transport, &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Receipt(CidarenAssessmentReceiptKind::WordSelectionRequired)
        );
        assert!(flow.issue_start(request_at()).is_err());

        let (_, fresh_plan) = word_selection_plan();
        flow.supply_word_selection(&detail, fresh_plan).unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::ReadyToSelectWords);
    }

    #[tokio::test]
    async fn reading_card_and_skip_are_distinct_single_step_mutations() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([
                response(&reading_card_payload()),
                response(&start_payload()),
                receipt(CidarenAssessmentReceiptKind::Completed),
            ])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let task_id = TaskId::new();
        let mut flow =
            CidarenAttemptFlow::try_new(&context, task_id, "class-task:2002", &detail(), None)
                .unwrap();
        let outcome = flow
            .issue_start(request_at())
            .unwrap()
            .execute(transport.clone(), &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(flow.status(), CidarenAttemptFlowStatus::CurrentReadingCard);
        let remote_progress = flow.current_remote_progress().unwrap();
        assert_eq!(remote_progress.completed(), 0);
        assert_eq!(remote_progress.total(), 2);
        let continuation = flow.pre_question_continuation().unwrap().unwrap();
        assert_eq!(continuation.phase(), crate::CIDAREN_READING_CARD_PHASE);
        assert!(continuation.response_digest().is_some());
        let digest = continuation.artifact().digest();
        let (artifact, _, _, _) = continuation.into_parts();
        let value = artifact.into_secret_value();
        let recovered_context = recovered_context(&context);
        let artifact =
            CidarenPreQuestionArtifact::decode_bound(&value, digest, task_id, "class-task:2002")
                .unwrap();
        flow = CidarenAttemptFlow::restore_pre_question(
            &recovered_context,
            task_id,
            "class-task:2002",
            &detail(),
            artifact,
            None,
        )
        .unwrap();
        assert_eq!(flow.current_reading_card().unwrap().position(), 1);
        let command = flow.issue_advance(&settings(), request_at()).unwrap();
        assert!((1..=3).contains(&command.delay_before_execute_seconds()));
        let outcome = command
            .execute(transport.clone(), &recovered_context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(flow.current_question().unwrap().position, 2);
        assert_eq!(flow.inter_step_delay_seconds(&settings()), 2);
        let command = flow.issue_skip(&settings(), request_at()).unwrap();
        assert_eq!(command.delay_before_execute_seconds(), 0);
        let outcome = command
            .execute(transport.clone(), &recovered_context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Receipt(CidarenAssessmentReceiptKind::Completed)
        );
        assert!(flow.completion_receipt().is_ok());
        assert_eq!(
            *transport.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::SubmitAnswerAndSave,
                CidarenAttemptOperation::SkipAnswer,
            ]
        );
    }

    #[tokio::test]
    async fn unsupported_mode_73_can_use_the_evidenced_skip_path() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([
                response(&mode_73_payload()),
                receipt(CidarenAssessmentReceiptKind::Completed),
            ])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail(),
            None,
        )
        .unwrap();
        let outcome = flow
            .issue_start(request_at())
            .unwrap()
            .execute(transport.clone(), &context)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(
            flow.current_question().unwrap().kind,
            QuestionKind::FillBlank
        );
        let materialization = flow.current_question_materialization().unwrap().unwrap();
        let recovered = recovered_context(&context);
        let (mut flow, question, verified_steps) =
            restore_materialization(&recovered, materialization, None, None);
        assert_eq!(question.kind, QuestionKind::FillBlank);
        assert_eq!(verified_steps, 0);

        let outcome = flow
            .issue_skip(&settings(), request_at())
            .unwrap()
            .execute(transport.clone(), &recovered)
            .await
            .unwrap();
        flow.accept(outcome).unwrap();
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::Receipt(CidarenAssessmentReceiptKind::Completed)
        );
        assert_eq!(
            *transport.operations.lock().unwrap(),
            [
                CidarenAttemptOperation::StartAnswer,
                CidarenAttemptOperation::SkipAnswer,
            ]
        );
    }

    #[tokio::test]
    async fn unexpected_acknowledgement_fails_closed() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([receipt(
                CidarenAssessmentReceiptKind::Accepted,
            )])),
            operations: Mutex::new(Vec::new()),
        });
        let context = context();
        let mut flow = CidarenAttemptFlow::try_new(
            &context,
            TaskId::new(),
            "class-task:2002",
            &detail(),
            None,
        )
        .unwrap();
        let outcome = flow
            .issue_start(request_at())
            .unwrap()
            .execute(transport, &context)
            .await
            .unwrap();
        assert!(flow.accept(outcome).is_err());
        assert_eq!(
            flow.status(),
            CidarenAttemptFlowStatus::FailedClosed(CidarenAttemptOperation::StartAnswer)
        );
        assert!(flow.issue_start(request_at()).is_err());
    }

    #[test]
    fn nested_third_parent_fallback_is_encoded_as_the_exact_donor_wire_answer() {
        let mut parsed = parse_attempt_question(
            &json!({
                "topic_code": "nested-topic",
                "topic_mode": 41,
                "stem": {"content": "Complete {}", "remark": "unmatched"},
                "options": [
                    {"answer_tag": 0, "content": "first", "sub_options": []},
                    {"answer_tag": 1, "content": "second", "sub_options": []},
                    {
                        "answer_tag": "2#",
                        "content": "third",
                        "sub_options": [{"answer_tag": 0, "content": "child"}]
                    }
                ]
            }),
            "class-task:2002",
            1,
        )
        .unwrap()
        .to_question(TaskId::new())
        .unwrap();
        let selected = SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: parsed.id,
            answer: NormalizedAnswer::Selections(vec!["s:2#".to_owned()]),
            source: AnswerSource::ProviderNative,
            confidence: None,
        };
        let mut answers = wire_answers(&parsed, &selected).unwrap();
        let CidarenWireAnswer::Text(answer) = answers.pop_front().unwrap() else {
            panic!("expected text wire answer");
        };
        assert_eq!(answer.as_str(), "2#");
        assert!(answers.is_empty());

        parsed.metadata_sanitized["topic_mode"] = json!(17);
        assert!(wire_answers(&parsed, &selected).is_err());
    }

    fn restore_materialization(
        context: &ProviderContext,
        materialization: CidarenQuestionMaterialization,
        selected: Option<&SelectedAnswer>,
        rejected_phase: Option<&str>,
    ) -> (CidarenAttemptFlow, Question, u32) {
        let (question, encoded, phase, _, _) = materialization.into_parts();
        let digest = encoded.digest();
        let value = encoded.into_secret_value();
        if let Some(rejected_phase) = rejected_phase {
            let mismatched =
                CidarenQuestionArtifact::decode_bound(&value, digest, "class-task:2002", &question)
                    .unwrap();
            assert!(
                CidarenAttemptFlow::restore_question(
                    context,
                    "class-task:2002",
                    &detail(),
                    &mismatched,
                    rejected_phase,
                    &question,
                    selected,
                )
                .is_err()
            );
        }
        let artifact =
            CidarenQuestionArtifact::decode_bound(&value, digest, "class-task:2002", &question)
                .unwrap();
        let verified_steps = artifact.verified_steps();
        let flow = CidarenAttemptFlow::restore_question(
            context,
            "class-task:2002",
            &detail(),
            &artifact,
            phase,
            &question,
            selected,
        )
        .unwrap();
        (flow, question, verified_steps)
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

    fn receipt(kind: CidarenAssessmentReceiptKind) -> CidarenAssessmentResponse {
        CidarenAssessmentResponse::Receipt {
            kind,
            message_sanitized: None,
        }
    }

    fn start_payload() -> Value {
        serde_json::from_str(include_str!(
            "../../../fixtures/providers/cidaren/questions/start-answer-single.json"
        ))
        .unwrap()
    }

    fn matching_payload() -> Value {
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

    fn mode_73_payload() -> Value {
        serde_json::from_str(include_str!(
            "../../../fixtures/providers/cidaren/questions/start-answer-fill-blank-73.json"
        ))
        .unwrap()
    }

    fn word_selection_plan() -> (RemoteTaskDetail, CidarenWordSelectionPlan) {
        let mut detail = detail();
        detail.task.title = "Synthetic List 02".to_owned();
        detail.task.source_type = SourceType::Practice;
        detail.task.normalized["task_type"] = json!("learning");
        detail.task.normalized["task_id"] = json!(92002);
        detail.normalized_detail["task"]["task_type"] = json!("learning");
        detail.normalized_detail["task"]["task_id"] = json!(92002);
        let units = CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
        .unwrap();
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &detail, &units)
                .unwrap();
        let inventory = parse_study_task_info_response(
            include_str!("../../../fixtures/providers/cidaren/answers/study-task-info.json")
                .as_bytes(),
            &binding,
            None,
        )
        .unwrap();
        let plan = build_word_selection_plan(&binding, &inventory)
            .unwrap()
            .unwrap();
        (detail, plan)
    }

    fn settings() -> CidarenRuntimeSettings {
        CidarenRuntimeSettings {
            answer_delay_min_seconds: 2,
            answer_delay_max_seconds: 2,
            answer_time_min_millis: 2_500,
            answer_time_max_millis: 7_500,
            skip_time_millis: 20_000,
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-attempt-flow-test".to_owned(),
        }
    }

    fn recovered_context(original: &ProviderContext) -> ProviderContext {
        ProviderContext {
            provider_id: original.provider_id.clone(),
            account_id: original.account_id,
            credential_refs: original.credential_refs.clone(),
            correlation_id: "cidaren-recovery-audit".to_owned(),
        }
    }

    fn detail() -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": -1,
            "course_id": "course-a",
            "task_type": "test",
            "progress": 35,
        });
        RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "class-task:2002".to_owned(),
                course_remote_id: Some("course:course-a".to_owned()),
                title: "Synthetic Task".to_owned(),
                source_type: SourceType::Exam,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::InProgress,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "synthetic-fingerprint".to_owned(),
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
}
