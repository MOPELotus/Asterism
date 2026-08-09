use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AnswerCandidateId, AnswerConfidence, AnswerSource, NormalizedAnswer, ProviderId, Question,
    QuestionId, QuestionSnapshotId, SubmissionDraftId, TaskId, Timestamp,
};

const MAX_DRAFT_ITEMS: usize = 5_000;
const MAX_PREVIEW_FIELDS: usize = 20_000;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;
const MAX_PREVIEW_FORMAT_BYTES: usize = 128;
const MAX_PREVIEW_FIELD_NAME_BYTES: usize = 256;

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
    pub items: Vec<SubmissionDraftItem>,
    pub payload_preview: SubmissionPayloadPreview,
    pub created_at: Timestamp,
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
    #[error("submission payload preview is foreign, unsafe, or malformed")]
    InvalidPayloadPreview,
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
}
