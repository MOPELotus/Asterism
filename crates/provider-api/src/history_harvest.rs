use std::{collections::BTreeSet, fmt};

use asterism_domain::{
    CourseId, NormalizedAnswer, Question, QuestionId, QuestionKind, RetakeScorePolicy,
    SubmissionScore, TaskId, Timestamp,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ProviderContext, ProviderIdentity, ProviderResult, ProviderRouteContext};

const MAX_CURSOR_BYTES: usize = 64 * 1_024;
const MAX_TASK_METADATA_BYTES: usize = 64 * 1_024;
const MAX_RESULT_PROVENANCE_BYTES: usize = 256 * 1_024;
const MAX_HISTORY_TASKS_PER_PAGE: usize = 100;
const MAX_HISTORY_QUESTIONS: usize = 5_000;
const MAX_REMOTE_ID_BYTES: usize = 512;
const MAX_CURSOR_TYPE_BYTES: usize = 96;

/// Sanitized resumable Provider pagination state. Credential and route secrets
/// belong in Core's secret boundary, never in this durable cursor.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerHistoryCursor {
    pub version: u32,
    pub cursor_type: String,
    pub value_sanitized: Value,
}

impl AnswerHistoryCursor {
    /// # Errors
    ///
    /// Rejects unversioned, unbounded, secret-shaped or cross-Provider cursor
    /// values before they enter a harvest watermark.
    pub fn validate(
        &self,
        provider_id: &asterism_domain::ProviderId,
    ) -> Result<(), AnswerHistoryContractError> {
        if self.version == 0
            || !valid_provider_label(provider_id.as_str(), &self.cursor_type)
            || !valid_json(&self.value_sanitized, MAX_CURSOR_BYTES)
        {
            return Err(AnswerHistoryContractError::InvalidCursor);
        }
        Ok(())
    }
}

impl fmt::Debug for AnswerHistoryCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnswerHistoryCursor")
            .field("version", &self.version)
            .field("cursor_type", &self.cursor_type)
            .field("value_sanitized", &"[REDACTED]")
            .finish()
    }
}

/// One safely readable historical result/Attempt discovered without creating
/// or changing remote work.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct AnswerHistoryTaskRef {
    pub remote_task_id: String,
    pub course_remote_id: Option<String>,
    pub provider_attempt_digest: [u8; 32],
    pub completed_at: Option<Timestamp>,
    pub metadata_sanitized: Value,
    #[serde(skip)]
    pub route_context: ProviderRouteContext,
}

impl AnswerHistoryTaskRef {
    /// # Errors
    ///
    /// Rejects malformed remote identity, a missing Attempt binding or unsafe
    /// metadata before Core maps the reference to a local Task.
    pub fn validate(&self) -> Result<(), AnswerHistoryContractError> {
        if !valid_remote_id(&self.remote_task_id)
            || self
                .course_remote_id
                .as_deref()
                .is_some_and(|value| !valid_remote_id(value))
            || self.provider_attempt_digest == [0; 32]
            || !valid_json(&self.metadata_sanitized, MAX_TASK_METADATA_BYTES)
        {
            return Err(AnswerHistoryContractError::InvalidTaskReference);
        }
        Ok(())
    }
}

impl fmt::Debug for AnswerHistoryTaskRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnswerHistoryTaskRef")
            .field("remote_task_id", &self.remote_task_id)
            .field("course_remote_id", &self.course_remote_id)
            .field("provider_attempt_digest", &"[SHA-256]")
            .field("completed_at", &self.completed_at)
            .field("metadata_sanitized", &"[REDACTED]")
            .field("route_context", &self.route_context)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnswerHistoryPage {
    tasks: Vec<AnswerHistoryTaskRef>,
    next_cursor: Option<AnswerHistoryCursor>,
    complete: bool,
}

impl AnswerHistoryPage {
    /// # Errors
    ///
    /// Rejects oversized/duplicate pages and ambiguous cursor completion.
    pub fn try_new(
        provider_id: &asterism_domain::ProviderId,
        tasks: Vec<AnswerHistoryTaskRef>,
        next_cursor: Option<AnswerHistoryCursor>,
        complete: bool,
    ) -> Result<Self, AnswerHistoryContractError> {
        if tasks.len() > MAX_HISTORY_TASKS_PER_PAGE
            || (tasks.is_empty() && !complete)
            || complete == next_cursor.is_some()
            || tasks.iter().any(|task| task.validate().is_err())
            || next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.validate(provider_id).is_err())
        {
            return Err(AnswerHistoryContractError::InvalidPage);
        }
        let mut identities = BTreeSet::new();
        if tasks.iter().any(|task| {
            !identities.insert((task.remote_task_id.as_str(), task.provider_attempt_digest))
        }) {
            return Err(AnswerHistoryContractError::InvalidPage);
        }
        Ok(Self {
            tasks,
            next_cursor,
            complete,
        })
    }

    pub fn tasks(&self) -> &[AnswerHistoryTaskRef] {
        &self.tasks
    }

    pub const fn next_cursor(&self) -> Option<&AnswerHistoryCursor> {
        self.next_cursor.as_ref()
    }

    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn into_parts(self) -> (Vec<AnswerHistoryTaskRef>, Option<AnswerHistoryCursor>, bool) {
        (self.tasks, self.next_cursor, self.complete)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnswerHistoryTaskRequest {
    pub task_id: TaskId,
    pub course_id: Option<CourseId>,
    pub reference: AnswerHistoryTaskRef,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerHistoryQuestionEvidence {
    pub question_id: QuestionId,
    pub submitted_answer: Option<NormalizedAnswer>,
    pub official_answer: Option<NormalizedAnswer>,
    pub submitted_answer_correct: Option<bool>,
    pub provenance_sanitized: Value,
}

impl AnswerHistoryQuestionEvidence {
    fn validate(&self, question: &Question) -> Result<(), AnswerHistoryContractError> {
        if self.question_id != question.id
            || (self.submitted_answer.is_none()
                && self.official_answer.is_none()
                && self.submitted_answer_correct.is_none())
            || (self.submitted_answer_correct.is_some() && self.submitted_answer.is_none())
            || self
                .submitted_answer
                .as_ref()
                .is_some_and(|answer| !answer_matches_question(question, answer))
            || self
                .official_answer
                .as_ref()
                .is_some_and(|answer| !answer_matches_question(question, answer))
            || !valid_json(&self.provenance_sanitized, MAX_RESULT_PROVENANCE_BYTES)
        {
            return Err(AnswerHistoryContractError::InvalidQuestionEvidence);
        }
        Ok(())
    }
}

impl fmt::Debug for AnswerHistoryQuestionEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnswerHistoryQuestionEvidence")
            .field("question_id", &self.question_id)
            .field("has_submitted_answer", &self.submitted_answer.is_some())
            .field("has_official_answer", &self.official_answer.is_some())
            .field("submitted_answer_correct", &self.submitted_answer_correct)
            .field("provenance_sanitized", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerHistoryRetakeFacts {
    pub allowed: bool,
    pub remaining_attempts: Option<u32>,
    pub closes_at: Option<Timestamp>,
    #[serde(default)]
    pub score_policy: RetakeScorePolicy,
    pub metadata_sanitized: Value,
}

impl AnswerHistoryRetakeFacts {
    /// Validates bounded, internally consistent Provider retake facts.
    ///
    /// # Errors
    ///
    /// Rejects positive remaining attempts when retake is forbidden and
    /// unbounded or secret-shaped metadata.
    pub fn validate(&self) -> Result<(), AnswerHistoryContractError> {
        if (!self.allowed
            && self
                .remaining_attempts
                .is_some_and(|remaining| remaining > 0))
            || !valid_json(&self.metadata_sanitized, MAX_TASK_METADATA_BYTES)
        {
            return Err(AnswerHistoryContractError::InvalidRetakeFacts);
        }
        Ok(())
    }
}

/// Complete read-only result facts for one Core-mapped historical Task.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct ProviderAnswerHistoryTaskEvidence {
    pub task_id: TaskId,
    pub provider_attempt_digest: [u8; 32],
    pub result_digest: [u8; 32],
    pub questions: Vec<Question>,
    pub question_evidence: Vec<AnswerHistoryQuestionEvidence>,
    pub score: Option<SubmissionScore>,
    pub retake: Option<AnswerHistoryRetakeFacts>,
    pub provenance_sanitized: Value,
    pub observed_at: Timestamp,
}

impl ProviderAnswerHistoryTaskEvidence {
    /// # Errors
    ///
    /// Rejects cross-Task Questions, duplicate identities, unbound facts,
    /// malformed answers, zero digests and unsafe provenance.
    pub fn validate(
        &self,
        request: &AnswerHistoryTaskRequest,
    ) -> Result<(), AnswerHistoryContractError> {
        if self.task_id != request.task_id
            || self.provider_attempt_digest != request.reference.provider_attempt_digest
            || self.provider_attempt_digest == [0; 32]
            || self.result_digest == [0; 32]
            || self.questions.is_empty()
            || self.questions.len() > MAX_HISTORY_QUESTIONS
            || self.question_evidence.len() > MAX_HISTORY_QUESTIONS
            || self.score.is_some_and(|score| score.validate().is_err())
            || self
                .retake
                .as_ref()
                .is_some_and(|facts| facts.validate().is_err())
            || !valid_json(&self.provenance_sanitized, MAX_RESULT_PROVENANCE_BYTES)
        {
            return Err(AnswerHistoryContractError::InvalidTaskEvidence);
        }
        let mut question_ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        if self.questions.iter().any(|question| {
            question.task_id != self.task_id
                || question.validate().is_err()
                || !question_ids.insert(question.id)
                || !positions.insert(question.position)
        }) {
            return Err(AnswerHistoryContractError::InvalidTaskEvidence);
        }
        let mut evidenced = BTreeSet::new();
        for evidence in &self.question_evidence {
            let question = self
                .questions
                .iter()
                .find(|question| question.id == evidence.question_id)
                .ok_or(AnswerHistoryContractError::InvalidQuestionEvidence)?;
            if !evidenced.insert(evidence.question_id) || evidence.validate(question).is_err() {
                return Err(AnswerHistoryContractError::InvalidQuestionEvidence);
            }
        }
        Ok(())
    }
}

impl fmt::Debug for ProviderAnswerHistoryTaskEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAnswerHistoryTaskEvidence")
            .field("task_id", &self.task_id)
            .field("provider_attempt_digest", &"[SHA-256]")
            .field("result_digest", &"[SHA-256]")
            .field("question_count", &self.questions.len())
            .field("evidence_count", &self.question_evidence.len())
            .field("score", &self.score)
            .field("has_retake_facts", &self.retake.is_some())
            .field("provenance_sanitized", &"[REDACTED]")
            .field("observed_at", &self.observed_at)
            .finish()
    }
}

#[async_trait]
pub trait AnswerHistoryHarvestCapability: ProviderIdentity {
    async fn list_answer_history(
        &self,
        context: &ProviderContext,
        cursor: Option<&AnswerHistoryCursor>,
    ) -> ProviderResult<AnswerHistoryPage>;

    async fn read_answer_history_task(
        &self,
        context: &ProviderContext,
        request: &AnswerHistoryTaskRequest,
    ) -> ProviderResult<ProviderAnswerHistoryTaskEvidence>;
}

fn answer_matches_question(question: &Question, answer: &NormalizedAnswer) -> bool {
    if answer.validate().is_err()
        || matches!(answer, NormalizedAnswer::Unknown | NormalizedAnswer::Skip)
    {
        return false;
    }
    match (question.kind, answer) {
        (
            QuestionKind::SingleChoice | QuestionKind::MultipleChoice,
            NormalizedAnswer::Selections(selected),
        ) => selected
            .iter()
            .all(|id| question.options.iter().any(|option| option.id == *id)),
        (QuestionKind::TrueFalse, NormalizedAnswer::Boolean(_))
        | (QuestionKind::FillBlank | QuestionKind::ShortAnswer, NormalizedAnswer::Texts(_))
        | (QuestionKind::Matching, NormalizedAnswer::Pairs(_))
        | (QuestionKind::Ordering, NormalizedAnswer::Ordering(_))
        | (QuestionKind::Composite, NormalizedAnswer::Composite(_)) => true,
        _ => false,
    }
}

fn valid_remote_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_provider_label(provider_id: &str, value: &str) -> bool {
    value.len() <= MAX_CURSOR_TYPE_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value
            .strip_prefix(provider_id)
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

fn valid_json(value: &Value, max_bytes: usize) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| {
        !encoded.is_empty() && encoded.len() <= max_bytes && !contains_sensitive_key(value)
    })
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(values) => values.iter().any(|(key, value)| {
            let normalized = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect::<String>();
            ["cookie", "authorization", "password", "token", "secret"]
                .iter()
                .any(|needle| normalized.contains(needle))
                || contains_sensitive_key(value)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnswerHistoryContractError {
    #[error("answer history cursor is invalid or unsafe")]
    InvalidCursor,
    #[error("answer history task reference is invalid or unsafe")]
    InvalidTaskReference,
    #[error("answer history page is invalid, ambiguous or exceeds its bound")]
    InvalidPage,
    #[error("answer history Question evidence is invalid or cross-bound")]
    InvalidQuestionEvidence,
    #[error("answer history retake facts are inconsistent or unsafe")]
    InvalidRetakeFacts,
    #[error("answer history Task evidence is invalid, unsafe or cross-bound")]
    InvalidTaskEvidence,
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderId, QuestionOption};
    use chrono::Utc;

    use super::*;

    fn task_ref() -> AnswerHistoryTaskRef {
        AnswerHistoryTaskRef {
            remote_task_id: "work-100".to_owned(),
            course_remote_id: Some("course-10".to_owned()),
            provider_attempt_digest: [7; 32],
            completed_at: Some(Utc::now()),
            metadata_sanitized: serde_json::json!({"result_surface": "chapter_work"}),
            route_context: ProviderRouteContext::default(),
        }
    }

    fn task_evidence() -> (AnswerHistoryTaskRequest, ProviderAnswerHistoryTaskEvidence) {
        let task_id = TaskId::new();
        let question_id = QuestionId::new();
        let request = AnswerHistoryTaskRequest {
            task_id,
            course_id: Some(CourseId::new()),
            reference: task_ref(),
        };
        let questions = vec![Question {
            id: question_id,
            task_id,
            remote_question_id: Some("question-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Private historical stem".to_owned(),
            options: vec![QuestionOption {
                id: "A".to_owned(),
                content: Some("Private historical answer".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            }],
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            position: 1,
        }];
        let evidence = ProviderAnswerHistoryTaskEvidence {
            task_id,
            provider_attempt_digest: [7; 32],
            result_digest: [9; 32],
            questions,
            question_evidence: vec![AnswerHistoryQuestionEvidence {
                question_id,
                submitted_answer: Some(NormalizedAnswer::Selections(vec!["A".to_owned()])),
                official_answer: Some(NormalizedAnswer::Selections(vec!["A".to_owned()])),
                submitted_answer_correct: Some(true),
                provenance_sanitized: serde_json::json!({"label": "correct"}),
            }],
            score: Some(SubmissionScore {
                earned_milli_points: 100_000,
                possible_milli_points: 100_000,
            }),
            retake: Some(AnswerHistoryRetakeFacts {
                allowed: true,
                remaining_attempts: None,
                closes_at: None,
                score_policy: RetakeScorePolicy::HighestScore,
                metadata_sanitized: serde_json::json!({"action": "redo_test"}),
            }),
            provenance_sanitized: serde_json::json!({"surface": "result_page"}),
            observed_at: Utc::now(),
        };
        (request, evidence)
    }

    #[test]
    fn cursor_and_page_are_provider_bound_bounded_and_secret_free() {
        let provider = ProviderId::new("chaoxing").unwrap();
        let cursor = AnswerHistoryCursor {
            version: 1,
            cursor_type: "chaoxing.chapter-history.v1".to_owned(),
            value_sanitized: serde_json::json!({"page": 2}),
        };
        cursor.validate(&provider).unwrap();
        assert!(cursor.validate(&ProviderId::new("uai").unwrap()).is_err());

        let page =
            AnswerHistoryPage::try_new(&provider, vec![task_ref()], Some(cursor.clone()), false)
                .unwrap();
        assert!(!page.is_complete());
        assert_eq!(page.tasks().len(), 1);
        assert!(
            AnswerHistoryPage::try_new(&provider, vec![task_ref(), task_ref()], None, true)
                .is_err()
        );

        let mut secret = cursor;
        secret.value_sanitized = serde_json::json!({"access_token": "hidden"});
        assert_eq!(
            secret.validate(&provider),
            Err(AnswerHistoryContractError::InvalidCursor)
        );
    }

    #[test]
    fn task_evidence_is_exactly_bound_and_debug_redacts_answers() {
        let (request, mut evidence) = task_evidence();
        evidence.validate(&request).unwrap();
        let debug = format!("{evidence:?}");
        assert!(!debug.contains("Private historical stem"));
        assert!(!debug.contains("Private historical answer"));

        evidence.task_id = TaskId::new();
        assert_eq!(
            evidence.validate(&request),
            Err(AnswerHistoryContractError::InvalidTaskEvidence)
        );
    }

    #[test]
    fn question_correctness_requires_a_valid_submitted_answer() {
        let (request, mut evidence) = task_evidence();
        evidence.question_evidence[0].submitted_answer = None;
        assert_eq!(
            evidence.validate(&request),
            Err(AnswerHistoryContractError::InvalidQuestionEvidence)
        );

        evidence.question_evidence[0].submitted_answer = Some(NormalizedAnswer::Selections(vec![
            "foreign-option".to_owned(),
        ]));
        assert_eq!(
            evidence.validate(&request),
            Err(AnswerHistoryContractError::InvalidQuestionEvidence)
        );
    }
}
