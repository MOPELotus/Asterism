use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{QuestionId, TaskId};

const MAX_REMOTE_ID_BYTES: usize = 512;
const MAX_STEM_BYTES: usize = 64 * 1024;
const MAX_OPTION_CONTENT_BYTES: usize = 32 * 1024;
const MAX_EXPLANATION_BYTES: usize = 64 * 1024;
const MAX_OPTIONS: usize = 256;
const MAX_ATTACHMENTS: usize = 64;
const MAX_ANSWER_ITEMS: usize = 256;
const MAX_COMPOSITE_DEPTH: usize = 8;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_POSITION: u32 = 100_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    SingleChoice,
    MultipleChoice,
    TrueFalse,
    FillBlank,
    ShortAnswer,
    Matching,
    Ordering,
    Composite,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAttachmentKind {
    Image,
    Audio,
    Video,
    File,
    Formula,
    Other,
}

/// Sanitized attachment identity and display facts. Fetch URLs, signatures and
/// bearer material are intentionally absent from the persisted domain model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuestionAttachment {
    pub kind: QuestionAttachmentKind,
    pub remote_id: Option<String>,
    pub label: Option<String>,
    pub metadata_sanitized: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuestionOption {
    pub id: String,
    pub content: Option<String>,
    pub attachments: Vec<QuestionAttachment>,
    pub metadata_sanitized: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Question {
    pub id: QuestionId,
    pub task_id: TaskId,
    pub remote_question_id: Option<String>,
    pub kind: QuestionKind,
    pub stem: String,
    pub options: Vec<QuestionOption>,
    pub attachments: Vec<QuestionAttachment>,
    pub metadata_sanitized: Value,
    /// One-based position in the current freshly parsed attempt.
    pub position: u32,
}

impl Question {
    /// Validates the bounded, credential-safe normalized Question contract.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError`] for malformed identity/text,
    /// duplicate options, excessive collections, or unsanitized metadata.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        if self.position == 0
            || self.position > MAX_POSITION
            || self
                .remote_question_id
                .as_deref()
                .is_some_and(|value| !valid_text(value, MAX_REMOTE_ID_BYTES))
            || self.stem.len() > MAX_STEM_BYTES
            || self.stem.chars().any(char::is_control)
            || (self.stem.trim().is_empty() && self.attachments.is_empty())
        {
            return Err(QuestionValidationError::InvalidQuestion);
        }
        if self.options.len() > MAX_OPTIONS || self.attachments.len() > MAX_ATTACHMENTS {
            return Err(QuestionValidationError::TooManyItems);
        }
        let mut option_ids = BTreeSet::new();
        for option in &self.options {
            if !valid_text(&option.id, MAX_REMOTE_ID_BYTES)
                || option
                    .content
                    .as_deref()
                    .is_some_and(|value| !valid_optional_content(value, MAX_OPTION_CONTENT_BYTES))
                || (option.content.is_none() && option.attachments.is_empty())
                || option.attachments.len() > MAX_ATTACHMENTS
                || !option_ids.insert(option.id.as_str())
                || !valid_sanitized_json(&option.metadata_sanitized)
            {
                return Err(QuestionValidationError::InvalidOption);
            }
            validate_attachments(&option.attachments)?;
        }
        validate_attachments(&self.attachments)?;
        if !valid_sanitized_json(&self.metadata_sanitized) {
            return Err(QuestionValidationError::UnsanitizedMetadata);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    Manual,
    LocalCache,
    ProviderNative,
    ExternalBank,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct AnswerConfidence(u16);

impl AnswerConfidence {
    pub const MAX_BASIS_POINTS: u16 = 10_000;

    /// Creates a confidence value in basis points, where 10,000 is 100%.
    ///
    /// # Errors
    ///
    /// Returns [`AnswerConfidenceError`] when the value exceeds 10,000.
    pub const fn try_new(basis_points: u16) -> Result<Self, AnswerConfidenceError> {
        if basis_points <= Self::MAX_BASIS_POINTS {
            Ok(Self(basis_points))
        } else {
            Err(AnswerConfidenceError::OutOfRange)
        }
    }

    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for AnswerConfidence {
    type Error = AnswerConfidenceError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AnswerConfidence> for u16 {
    fn from(value: AnswerConfidence) -> Self {
        value.basis_points()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerPair {
    pub left: String,
    pub right: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum NormalizedAnswer {
    Selections(Vec<String>),
    Texts(Vec<String>),
    Boolean(bool),
    Pairs(Vec<AnswerPair>),
    Ordering(Vec<String>),
    Composite(Vec<Self>),
    Unknown,
}

impl NormalizedAnswer {
    /// Validates bounded normalized answer structure without Provider payloads.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError::InvalidAnswer`] for empty, duplicate,
    /// oversized, or excessively nested answer data.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        self.validate_at_depth(0)
    }

    fn validate_at_depth(&self, depth: usize) -> Result<(), QuestionValidationError> {
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(QuestionValidationError::InvalidAnswer);
        }
        match self {
            Self::Selections(values) | Self::Ordering(values) => {
                validate_unique_answer_values(values)
            }
            Self::Texts(values) => validate_answer_values(values),
            Self::Pairs(values) => {
                if values.is_empty() || values.len() > MAX_ANSWER_ITEMS {
                    return Err(QuestionValidationError::InvalidAnswer);
                }
                let mut left = BTreeSet::new();
                for pair in values {
                    if !valid_answer_text(&pair.left)
                        || !valid_answer_text(&pair.right)
                        || !left.insert(pair.left.as_str())
                    {
                        return Err(QuestionValidationError::InvalidAnswer);
                    }
                }
                Ok(())
            }
            Self::Composite(values) => {
                if values.is_empty() || values.len() > MAX_ANSWER_ITEMS {
                    return Err(QuestionValidationError::InvalidAnswer);
                }
                values
                    .iter()
                    .try_for_each(|value| value.validate_at_depth(depth + 1))
            }
            Self::Boolean(_) | Self::Unknown => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AnswerCandidate {
    pub question_id: QuestionId,
    pub source: AnswerSource,
    pub answer: NormalizedAnswer,
    pub confidence: Option<AnswerConfidence>,
    pub explanation: Option<String>,
    pub provenance_sanitized: Value,
}

impl AnswerCandidate {
    /// Validates normalized answer, bounded explanation and sanitized
    /// provenance independently from Provider parsing and submission.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError`] when any candidate field is unsafe.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        self.answer.validate()?;
        if self
            .explanation
            .as_deref()
            .is_some_and(|value| !valid_optional_content(value, MAX_EXPLANATION_BYTES))
            || !valid_sanitized_json(&self.provenance_sanitized)
        {
            return Err(QuestionValidationError::InvalidAnswer);
        }
        Ok(())
    }
}

fn validate_attachments(attachments: &[QuestionAttachment]) -> Result<(), QuestionValidationError> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(QuestionValidationError::TooManyItems);
    }
    for attachment in attachments {
        if attachment
            .remote_id
            .as_deref()
            .is_some_and(|value| !valid_text(value, MAX_REMOTE_ID_BYTES))
            || attachment
                .label
                .as_deref()
                .is_some_and(|value| !valid_optional_content(value, MAX_OPTION_CONTENT_BYTES))
            || !valid_sanitized_json(&attachment.metadata_sanitized)
        {
            return Err(QuestionValidationError::InvalidAttachment);
        }
    }
    Ok(())
}

fn validate_unique_answer_values(values: &[String]) -> Result<(), QuestionValidationError> {
    validate_answer_values(values)?;
    let mut unique = BTreeSet::new();
    if values.iter().all(|value| unique.insert(value.as_str())) {
        Ok(())
    } else {
        Err(QuestionValidationError::InvalidAnswer)
    }
}

fn validate_answer_values(values: &[String]) -> Result<(), QuestionValidationError> {
    if values.is_empty()
        || values.len() > MAX_ANSWER_ITEMS
        || values.iter().any(|value| !valid_answer_text(value))
    {
        Err(QuestionValidationError::InvalidAnswer)
    } else {
        Ok(())
    }
}

fn valid_answer_text(value: &str) -> bool {
    valid_optional_content(value, MAX_OPTION_CONTENT_BYTES)
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_optional_content(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_sanitized_json(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= MAX_JSON_BYTES)
        && !contains_sensitive_key(value)
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized: String = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                normalized.as_str(),
                "cookie"
                    | "authorization"
                    | "password"
                    | "accesstoken"
                    | "refreshtoken"
                    | "sessionsecret"
                    | "clientsecret"
            ) || contains_sensitive_key(value)
        }),
        Value::Array(items) => items.iter().any(contains_sensitive_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnswerConfidenceError {
    #[error("answer confidence must be between 0 and 10000 basis points")]
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuestionValidationError {
    #[error("question identity, stem, or position is invalid")]
    InvalidQuestion,
    #[error("question contains too many options or attachments")]
    TooManyItems,
    #[error("question option is malformed or duplicated")]
    InvalidOption,
    #[error("question attachment is malformed or unsanitized")]
    InvalidAttachment,
    #[error("question metadata is oversized or not sanitized")]
    UnsanitizedMetadata,
    #[error("normalized answer or candidate data is invalid")]
    InvalidAnswer,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_question() -> Question {
        Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("question-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Which option is correct?".to_owned(),
            options: vec![
                QuestionOption {
                    id: "A".to_owned(),
                    content: Some("First".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
                QuestionOption {
                    id: "B".to_owned(),
                    content: Some("Second".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"provider_kind": "single"}),
            position: 1,
        }
    }

    #[test]
    fn question_contract_accepts_bounded_sanitized_content() {
        assert_eq!(valid_question().validate(), Ok(()));
        assert_eq!(
            AnswerConfidence::try_new(9_500).unwrap().basis_points(),
            9_500
        );
    }

    #[test]
    fn question_contract_rejects_duplicate_options_and_secret_metadata() {
        let mut duplicate = valid_question();
        duplicate.options[1].id = "A".to_owned();
        assert_eq!(
            duplicate.validate(),
            Err(QuestionValidationError::InvalidOption)
        );

        let mut secret = valid_question();
        secret.metadata_sanitized = serde_json::json!({"access_token": "forbidden"});
        assert_eq!(
            secret.validate(),
            Err(QuestionValidationError::UnsanitizedMetadata)
        );
    }

    #[test]
    fn normalized_answers_are_typed_bounded_and_source_independent() {
        let question = valid_question();
        let candidate = AnswerCandidate {
            question_id: question.id,
            source: AnswerSource::ExternalBank,
            answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
            confidence: Some(AnswerConfidence::try_new(8_000).unwrap()),
            explanation: Some("Matched by normalized stem.".to_owned()),
            provenance_sanitized: serde_json::json!({"bank": "local-fixture"}),
        };
        assert_eq!(candidate.validate(), Ok(()));

        let duplicate = NormalizedAnswer::Ordering(vec!["A".to_owned(), "A".to_owned()]);
        assert_eq!(
            duplicate.validate(),
            Err(QuestionValidationError::InvalidAnswer)
        );
        assert_eq!(
            AnswerConfidence::try_new(10_001),
            Err(AnswerConfidenceError::OutOfRange)
        );
        assert!(serde_json::from_str::<AnswerConfidence>("10001").is_err());
    }
}
