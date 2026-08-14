use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AnswerCandidateId, AnswerConfidence, AnswerSource, ExecutionAttemptId, ExecutionId,
    NormalizedAnswer, ProviderId, Question, QuestionId, QuestionSnapshotId, RemoteState,
    SubmissionDraftId, SubmissionResultId, TaskId, Timestamp,
};

const MAX_DRAFT_ITEMS: usize = 5_000;
const MAX_PREVIEW_FIELDS: usize = 20_000;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;
const MAX_PREVIEW_FORMAT_BYTES: usize = 128;
const MAX_PREVIEW_FIELD_NAME_BYTES: usize = 256;
const MAX_RECEIPT_STATUS_BYTES: usize = 128;
const MAX_RECEIPT_MESSAGE_BYTES: usize = 2_048;
const MAX_PROVIDER_TRACE_ID_BYTES: usize = 256;
const MAX_SCORE_MILLI_POINTS: u64 = 1_000_000_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionPayloadEncoding {
    Form,
    Json,
    Query,
    ProviderSpecific,
}

/// A credential-free description of one Provider payload field. Values are
/// deliberately absent: the selected normalized answer remains the only value
/// carried by a persisted draft.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionPayloadFieldPreview {
    pub question_id: QuestionId,
    pub field_name: String,
}

/// A bounded, non-executable preview of how a Provider would encode selected
/// answers. It contains neither an endpoint nor headers, cookies, tokens, or
/// arbitrary JSON.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionPayloadPreview {
    pub encoding: SubmissionPayloadEncoding,
    pub format: String,
    pub fields: Vec<SubmissionPayloadFieldPreview>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SelectedAnswer {
    pub candidate_id: AnswerCandidateId,
    pub question_id: QuestionId,
    pub answer: NormalizedAnswer,
    pub source: AnswerSource,
    pub confidence: Option<AnswerConfidence>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SubmissionDraftItem {
    pub question: Question,
    pub selected: SelectedAnswer,
}

/// Immutable proof that a selected subset satisfies the resolved answer
/// coverage policy against the complete source Question snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionAnswerCoverage {
    pub total_question_count: u32,
    pub minimum_coverage_millis: u16,
    pub unanswered_question_ids: Vec<QuestionId>,
}

/// An immutable, reviewable submission plan. Remote submission material is
/// rebuilt later by a distinct execution capability instead of being persisted
/// in the draft.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SubmissionDraft {
    pub id: SubmissionDraftId,
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub answer_coverage: SubmissionAnswerCoverage,
    pub items: Vec<SubmissionDraftItem>,
    pub payload_preview: SubmissionPayloadPreview,
    pub created_at: Timestamp,
}

/// Bounded acknowledgement facts returned by the remote mutation. Raw response
/// bodies, headers and request material are intentionally excluded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionReceipt {
    pub remote_status: String,
    pub message_sanitized: Option<String>,
    pub provider_trace_id: Option<String>,
    pub received_at: Timestamp,
}

/// Durable acknowledgement attached to the exact attempt that issued one
/// remote submission mutation. Persisting it does not imply success; it only
/// gives later verification and crash recovery bounded context without
/// allowing the mutation to be replayed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionAttemptReceipt {
    pub submission_draft_id: SubmissionDraftId,
    pub execution_id: ExecutionId,
    pub execution_attempt_id: ExecutionAttemptId,
    pub receipt: SubmissionReceipt,
}

impl SubmissionAttemptReceipt {
    /// Validates the bounded receipt and its immutable Draft binding.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionResultValidationError`] when receipt facts are
    /// malformed or the record is foreign to the supplied draft.
    pub fn validate_for_draft(
        &self,
        draft: &SubmissionDraft,
    ) -> Result<(), SubmissionResultValidationError> {
        self.receipt.validate()?;
        if self.submission_draft_id != draft.id {
            return Err(SubmissionResultValidationError::DraftBindingInvalid);
        }
        Ok(())
    }
}

impl SubmissionReceipt {
    /// Validates bounded, credential-free acknowledgement facts.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionResultValidationError::InvalidReceipt`] for
    /// malformed or unbounded receipt data.
    pub fn validate(&self) -> Result<(), SubmissionResultValidationError> {
        if !valid_text(&self.remote_status, MAX_RECEIPT_STATUS_BYTES)
            || self
                .message_sanitized
                .as_deref()
                .is_some_and(|value| !valid_text(value, MAX_RECEIPT_MESSAGE_BYTES))
            || self
                .provider_trace_id
                .as_deref()
                .is_some_and(|value| !valid_text(value, MAX_PROVIDER_TRACE_ID_BYTES))
        {
            Err(SubmissionResultValidationError::InvalidReceipt)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionVerificationStatus {
    Confirmed,
    Rejected,
    Pending,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionQuestionVerificationStatus {
    Confirmed,
    Rejected,
    Unverified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionQuestionVerification {
    pub question_id: QuestionId,
    pub status: SubmissionQuestionVerificationStatus,
}

/// Score represented in thousandths of one point to avoid floating-point
/// ambiguity across Providers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionScore {
    pub earned_milli_points: u64,
    pub possible_milli_points: u64,
}

impl SubmissionScore {
    /// Validates the fixed-point score range.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionResultValidationError::InvalidVerification`] when
    /// the maximum is zero or earned/possible points exceed their bounds.
    pub const fn validate(self) -> Result<(), SubmissionResultValidationError> {
        if self.possible_milli_points == 0
            || self.possible_milli_points > MAX_SCORE_MILLI_POINTS
            || self.earned_milli_points > self.possible_milli_points
        {
            Err(SubmissionResultValidationError::InvalidVerification)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionVerificationSnapshot {
    pub status: SubmissionVerificationStatus,
    pub remote_state: Option<RemoteState>,
    pub score: Option<SubmissionScore>,
    pub progress_percent: Option<u8>,
    pub questions: Vec<SubmissionQuestionVerification>,
    pub verified_at: Timestamp,
}

impl SubmissionVerificationSnapshot {
    /// Validates bounded remote state, score, progress and per-Question facts.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionResultValidationError::InvalidVerification`] for
    /// duplicate Questions or malformed and unbounded facts.
    pub fn validate(&self) -> Result<(), SubmissionResultValidationError> {
        if self.progress_percent.is_some_and(|progress| progress > 100)
            || self.questions.len() > MAX_DRAFT_ITEMS
            || self.score.is_some_and(|score| score.validate().is_err())
        {
            return Err(SubmissionResultValidationError::InvalidVerification);
        }
        let mut question_ids = BTreeSet::new();
        if self
            .questions
            .iter()
            .any(|question| !question_ids.insert(question.question_id))
        {
            return Err(SubmissionResultValidationError::InvalidVerification);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionResultStatus {
    Confirmed,
    Rejected,
    ExecutionFailed,
    Inconclusive,
}

/// A submission result is complete only when a distinct verification snapshot
/// supports its status. Receipt presence alone never implies success.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionResult {
    pub id: SubmissionResultId,
    pub submission_draft_id: SubmissionDraftId,
    pub execution_id: ExecutionId,
    pub execution_attempt_id: ExecutionAttemptId,
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub status: SubmissionResultStatus,
    pub receipt: Option<SubmissionReceipt>,
    pub verification: SubmissionVerificationSnapshot,
    pub created_at: Timestamp,
}

impl SubmissionResult {
    /// Validates result fields and requires status to agree with verification.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionResultValidationError`] for malformed receipt or
    /// verification data and for a result status unsupported by verification.
    pub fn validate(&self) -> Result<(), SubmissionResultValidationError> {
        if !valid_text(&self.provider_version, MAX_PROVIDER_VERSION_BYTES) {
            return Err(SubmissionResultValidationError::InvalidResult);
        }
        if let Some(receipt) = &self.receipt {
            receipt.validate()?;
        }
        self.verification.validate()?;
        let status_matches_verification = matches!(
            (self.status, self.verification.status),
            (
                SubmissionResultStatus::Confirmed,
                SubmissionVerificationStatus::Confirmed
            ) | (
                SubmissionResultStatus::Rejected,
                SubmissionVerificationStatus::Rejected
            ) | (
                SubmissionResultStatus::ExecutionFailed | SubmissionResultStatus::Inconclusive,
                SubmissionVerificationStatus::Inconclusive | SubmissionVerificationStatus::Pending
            )
        );
        if !status_matches_verification {
            return Err(SubmissionResultValidationError::StatusMismatch);
        }
        Ok(())
    }

    /// Validates this result against one immutable source draft.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionResultValidationError::DraftBindingInvalid`] when
    /// identities or per-Question verification facts are foreign to the draft,
    /// in addition to errors returned by [`Self::validate`].
    pub fn validate_for_draft(
        &self,
        draft: &SubmissionDraft,
    ) -> Result<(), SubmissionResultValidationError> {
        self.validate()?;
        if self.submission_draft_id != draft.id
            || self.task_id != draft.task_id
            || self.question_snapshot_id != draft.question_snapshot_id
            || self.provider_id != draft.provider_id
        {
            return Err(SubmissionResultValidationError::DraftBindingInvalid);
        }
        let draft_questions = draft
            .items
            .iter()
            .map(|item| item.question.id)
            .chain(
                draft
                    .answer_coverage
                    .unanswered_question_ids
                    .iter()
                    .copied(),
            )
            .collect::<BTreeSet<_>>();
        if self
            .verification
            .questions
            .iter()
            .any(|question| !draft_questions.contains(&question.question_id))
        {
            return Err(SubmissionResultValidationError::DraftBindingInvalid);
        }
        Ok(())
    }
}

impl SubmissionDraft {
    /// Validates identity, selected-answer, and credential-free preview
    /// boundaries without accepting executable Provider payloads.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionDraftValidationError`] for empty, duplicate,
    /// foreign, unsafe, or unbounded draft data.
    pub fn validate(&self) -> Result<(), SubmissionDraftValidationError> {
        if self.items.is_empty()
            || self.items.len() > MAX_DRAFT_ITEMS
            || !valid_text(&self.provider_version, MAX_PROVIDER_VERSION_BYTES)
            || !valid_identifier(&self.payload_preview.format, MAX_PREVIEW_FORMAT_BYTES)
            || self.payload_preview.fields.is_empty()
            || self.payload_preview.fields.len() > MAX_PREVIEW_FIELDS
        {
            return Err(SubmissionDraftValidationError::InvalidDraft);
        }

        let mut question_ids = BTreeSet::new();
        let mut candidate_ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        for item in &self.items {
            if item.question.task_id != self.task_id
                || item.question.id != item.selected.question_id
                || item.question.validate().is_err()
                || item.selected.answer.validate().is_err()
                || matches!(item.selected.answer, NormalizedAnswer::Unknown)
                || !question_ids.insert(item.question.id)
                || !candidate_ids.insert(item.selected.candidate_id)
                || !positions.insert(item.question.position)
            {
                return Err(SubmissionDraftValidationError::InvalidSelection);
            }
        }
        self.answer_coverage.validate(&question_ids)?;

        let mut preview_fields = BTreeSet::new();
        for field in &self.payload_preview.fields {
            if !question_ids.contains(&field.question_id)
                || !valid_identifier(&field.field_name, MAX_PREVIEW_FIELD_NAME_BYTES)
                || sensitive_name(&field.field_name)
                || !preview_fields.insert((field.question_id, field.field_name.as_str()))
            {
                return Err(SubmissionDraftValidationError::InvalidPayloadPreview);
            }
        }
        if question_ids.iter().any(|question_id| {
            !self
                .payload_preview
                .fields
                .iter()
                .any(|field| field.question_id == *question_id)
        }) {
            return Err(SubmissionDraftValidationError::InvalidPayloadPreview);
        }
        Ok(())
    }
}

impl SubmissionAnswerCoverage {
    fn validate(
        &self,
        answered_question_ids: &BTreeSet<QuestionId>,
    ) -> Result<(), SubmissionDraftValidationError> {
        let Ok(total_question_count) = usize::try_from(self.total_question_count) else {
            return Err(SubmissionDraftValidationError::InvalidCoverage);
        };
        if total_question_count == 0
            || total_question_count > MAX_DRAFT_ITEMS
            || !(1..=1_000).contains(&self.minimum_coverage_millis)
            || answered_question_ids.is_empty()
            || answered_question_ids.len() > total_question_count
            || self.unanswered_question_ids.len() > total_question_count
        {
            return Err(SubmissionDraftValidationError::InvalidCoverage);
        }
        let mut unanswered = BTreeSet::new();
        if self.unanswered_question_ids.iter().any(|question_id| {
            answered_question_ids.contains(question_id) || !unanswered.insert(*question_id)
        }) || answered_question_ids.len() + unanswered.len() != total_question_count
            || answered_question_ids.len() * 1_000
                < total_question_count * usize::from(self.minimum_coverage_millis)
        {
            return Err(SubmissionDraftValidationError::InvalidCoverage);
        }
        Ok(())
    }
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    valid_text(value, maximum)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-[]".contains(character))
}

fn sensitive_name(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    [
        "cookie",
        "authorization",
        "password",
        "accesstoken",
        "refreshtoken",
        "sessionsecret",
        "clientsecret",
    ]
    .iter()
    .any(|secret| normalized.contains(secret))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SubmissionDraftValidationError {
    #[error("submission draft identity or bounds are invalid")]
    InvalidDraft,
    #[error("submission draft contains an invalid or duplicate answer selection")]
    InvalidSelection,
    #[error("submission draft answer coverage is incomplete or below policy")]
    InvalidCoverage,
    #[error("submission payload preview is foreign, unsafe, or malformed")]
    InvalidPayloadPreview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SubmissionResultValidationError {
    #[error("submission result identity or Provider version is invalid")]
    InvalidResult,
    #[error("submission receipt is malformed or unbounded")]
    InvalidReceipt,
    #[error("submission verification snapshot is malformed or unbounded")]
    InvalidVerification,
    #[error("submission result status is unsupported by its verification snapshot")]
    StatusMismatch,
    #[error("submission result is not bound to its draft Questions and identities")]
    DraftBindingInvalid,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::{QuestionKind, QuestionOption};

    use super::*;

    fn draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let question_id = QuestionId::new();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            provider_version: "0.1.0".to_owned(),
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem {
                question: Question {
                    id: question_id,
                    task_id,
                    remote_question_id: Some("question-1".to_owned()),
                    kind: QuestionKind::SingleChoice,
                    stem: "Which option is correct?".to_owned(),
                    options: vec![QuestionOption {
                        id: "A".to_owned(),
                        content: Some("First".to_owned()),
                        attachments: Vec::new(),
                        metadata_sanitized: json!({}),
                    }],
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                    position: 1,
                },
                selected: SelectedAnswer {
                    candidate_id: AnswerCandidateId::new(),
                    question_id,
                    answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                    source: AnswerSource::Manual,
                    confidence: Some(AnswerConfidence::try_new(10_000).unwrap()),
                },
            }],
            payload_preview: SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::Form,
                format: "chaoxing.work.v1".to_owned(),
                fields: vec![SubmissionPayloadFieldPreview {
                    question_id,
                    field_name: "answer[question-1]".to_owned(),
                }],
            },
            created_at: Utc::now(),
        }
    }

    #[test]
    fn partial_draft_requires_an_exact_snapshot_partition_and_threshold() {
        let mut draft = draft();
        let unanswered = QuestionId::new();
        draft.answer_coverage = SubmissionAnswerCoverage {
            total_question_count: 2,
            minimum_coverage_millis: 500,
            unanswered_question_ids: vec![unanswered],
        };
        draft.validate().unwrap();

        draft.answer_coverage.minimum_coverage_millis = 501;
        assert_eq!(
            draft.validate(),
            Err(SubmissionDraftValidationError::InvalidCoverage)
        );
        draft.answer_coverage.minimum_coverage_millis = 500;
        draft.answer_coverage.unanswered_question_ids = vec![draft.items[0].question.id];
        assert_eq!(
            draft.validate(),
            Err(SubmissionDraftValidationError::InvalidCoverage)
        );
    }

    #[test]
    fn draft_accepts_selected_answers_without_executable_payload_material() {
        assert_eq!(draft().validate(), Ok(()));
    }

    #[test]
    fn draft_rejects_foreign_unknown_or_sensitive_preview_data() {
        let mut foreign = draft();
        foreign.items[0].selected.question_id = QuestionId::new();
        assert_eq!(
            foreign.validate(),
            Err(SubmissionDraftValidationError::InvalidSelection)
        );

        let mut unknown = draft();
        unknown.items[0].selected.answer = NormalizedAnswer::Unknown;
        assert_eq!(
            unknown.validate(),
            Err(SubmissionDraftValidationError::InvalidSelection)
        );

        let mut sensitive = draft();
        sensitive.payload_preview.fields[0].field_name = "access_token".to_owned();
        assert_eq!(
            sensitive.validate(),
            Err(SubmissionDraftValidationError::InvalidPayloadPreview)
        );
    }

    #[test]
    fn result_requires_distinct_matching_verification_not_just_a_receipt() {
        let draft = draft();
        let mut result = SubmissionResult {
            id: SubmissionResultId::new(),
            submission_draft_id: draft.id,
            execution_id: ExecutionId::new(),
            execution_attempt_id: ExecutionAttemptId::new(),
            task_id: draft.task_id,
            question_snapshot_id: draft.question_snapshot_id,
            provider_id: draft.provider_id.clone(),
            provider_version: "0.1.0".to_owned(),
            status: SubmissionResultStatus::Confirmed,
            receipt: Some(SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: Some("request accepted".to_owned()),
                provider_trace_id: Some("trace-1".to_owned()),
                received_at: Utc::now(),
            }),
            verification: SubmissionVerificationSnapshot {
                status: SubmissionVerificationStatus::Confirmed,
                remote_state: Some(RemoteState::Completed),
                score: Some(SubmissionScore {
                    earned_milli_points: 90_000,
                    possible_milli_points: 100_000,
                }),
                progress_percent: Some(100),
                questions: vec![SubmissionQuestionVerification {
                    question_id: draft.items[0].question.id,
                    status: SubmissionQuestionVerificationStatus::Confirmed,
                }],
                verified_at: Utc::now(),
            },
            created_at: Utc::now(),
        };
        assert_eq!(result.validate_for_draft(&draft), Ok(()));

        result.verification.status = SubmissionVerificationStatus::Inconclusive;
        assert_eq!(
            result.validate_for_draft(&draft),
            Err(SubmissionResultValidationError::StatusMismatch)
        );
    }

    #[test]
    fn attempt_receipt_is_only_acknowledgement_context_for_one_draft() {
        let draft = draft();
        let record = SubmissionAttemptReceipt {
            submission_draft_id: draft.id,
            execution_id: ExecutionId::new(),
            execution_attempt_id: ExecutionAttemptId::new(),
            receipt: SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: None,
                provider_trace_id: None,
                received_at: Utc::now(),
            },
        };
        assert_eq!(record.validate_for_draft(&draft), Ok(()));
        let mut foreign = record;
        foreign.submission_draft_id = SubmissionDraftId::new();
        assert_eq!(
            foreign.validate_for_draft(&draft),
            Err(SubmissionResultValidationError::DraftBindingInvalid)
        );
    }

    #[test]
    fn result_rejects_foreign_question_verification_and_invalid_score() {
        let draft = draft();
        let mut verification = SubmissionVerificationSnapshot {
            status: SubmissionVerificationStatus::Inconclusive,
            remote_state: None,
            score: None,
            progress_percent: None,
            questions: vec![SubmissionQuestionVerification {
                question_id: QuestionId::new(),
                status: SubmissionQuestionVerificationStatus::Unverified,
            }],
            verified_at: Utc::now(),
        };
        let result = SubmissionResult {
            id: SubmissionResultId::new(),
            submission_draft_id: draft.id,
            execution_id: ExecutionId::new(),
            execution_attempt_id: ExecutionAttemptId::new(),
            task_id: draft.task_id,
            question_snapshot_id: draft.question_snapshot_id,
            provider_id: draft.provider_id.clone(),
            provider_version: "0.1.0".to_owned(),
            status: SubmissionResultStatus::Inconclusive,
            receipt: None,
            verification: verification.clone(),
            created_at: Utc::now(),
        };
        assert_eq!(
            result.validate_for_draft(&draft),
            Err(SubmissionResultValidationError::DraftBindingInvalid)
        );

        verification.score = Some(SubmissionScore {
            earned_milli_points: 2,
            possible_milli_points: 1,
        });
        assert_eq!(
            verification.validate(),
            Err(SubmissionResultValidationError::InvalidVerification)
        );
    }
}
