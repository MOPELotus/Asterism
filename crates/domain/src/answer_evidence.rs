use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AnswerBootstrapHarvestId, AnswerCandidateId, AnswerSource, CourseId, ExecutionAttemptId,
    NormalizedAnswer, PrivateAnswerEvidenceId, ProviderAccountId, ProviderId, Question,
    QuestionContentFingerprint, QuestionId, QuestionKind, QuestionSnapshotId, ScheduleId, TaskId,
    Timestamp, UserId,
};

const MAX_PROVENANCE_BYTES: usize = 256 * 1_024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerBootstrapHarvestState {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerBootstrapHarvest {
    pub id: AnswerBootstrapHarvestId,
    pub owner_user_id: UserId,
    pub provider_id: ProviderId,
    pub provider_account_id: ProviderAccountId,
    pub generation: u32,
    pub schedule_id: ScheduleId,
    pub state: AnswerBootstrapHarvestState,
    pub scanned_task_count: u32,
    pub total_task_count: Option<u32>,
    pub watermark_sanitized: Value,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

impl AnswerBootstrapHarvest {
    /// # Errors
    ///
    /// Rejects zero generations, impossible progress/timestamps and unsafe
    /// watermarks before a harvest can be persisted or resumed.
    pub fn validate(&self) -> Result<(), AnswerBootstrapHarvestValidationError> {
        if self.generation == 0
            || self
                .total_task_count
                .is_some_and(|total| self.scanned_task_count > total)
            || !valid_provenance(&self.watermark_sanitized)
            || self.updated_at < self.created_at
            || self
                .started_at
                .is_some_and(|started| started < self.created_at || started > self.updated_at)
            || self
                .completed_at
                .is_some_and(|completed| completed < self.created_at || completed > self.updated_at)
        {
            return Err(AnswerBootstrapHarvestValidationError);
        }
        let timestamps_match = match self.state {
            AnswerBootstrapHarvestState::Pending => {
                self.started_at.is_none()
                    && self.completed_at.is_none()
                    && self.scanned_task_count == 0
            }
            AnswerBootstrapHarvestState::Running | AnswerBootstrapHarvestState::Paused => {
                self.started_at.is_some() && self.completed_at.is_none()
            }
            AnswerBootstrapHarvestState::Completed
            | AnswerBootstrapHarvestState::Failed
            | AnswerBootstrapHarvestState::Cancelled => {
                self.started_at.is_some() && self.completed_at.is_some()
            }
        };
        if timestamps_match {
            Ok(())
        } else {
            Err(AnswerBootstrapHarvestValidationError)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("answer bootstrap harvest state, progress or timestamps are invalid")]
pub struct AnswerBootstrapHarvestValidationError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerEvidenceClass {
    Official,
    VerifiedHistorical,
    Negative,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmatchedEvidenceReason {
    IncompleteQuestion,
    MissingSharedContext,
    AmbiguousSemanticIdentity,
    UnsupportedHierarchy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum CorpusProjectionEligibility {
    Exact,
    Unmatched(UnmatchedEvidenceReason),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PrivateAnswerEvidence {
    pub id: PrivateAnswerEvidenceId,
    pub owner_user_id: UserId,
    pub provider_id: ProviderId,
    pub provider_account_id: ProviderAccountId,
    pub course_id: Option<CourseId>,
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub question_id: QuestionId,
    pub execution_attempt_id: Option<ExecutionAttemptId>,
    /// Digest of an authenticated Provider-native historical Attempt identity.
    /// This preserves bootstrap provenance without inventing a local Execution.
    pub provider_attempt_digest: Option<[u8; 32]>,
    pub source_candidate_id: Option<AnswerCandidateId>,
    pub question: Question,
    pub question_content_fingerprint: QuestionContentFingerprint,
    pub answer: NormalizedAnswer,
    pub answer_source: AnswerSource,
    pub evidence_class: AnswerEvidenceClass,
    pub result_digest: Option<[u8; 32]>,
    pub provenance_sanitized: Value,
    pub projection: CorpusProjectionEligibility,
    pub observed_at: Timestamp,
    pub verified_at: Timestamp,
}

impl PrivateAnswerEvidence {
    /// # Errors
    ///
    /// Rejects cross-bound, unverified, secret-shaped, oversized or falsely
    /// projectable evidence.
    pub fn validate(&self) -> Result<(), PrivateAnswerEvidenceValidationError> {
        if self.question.validate().is_err()
            || self.question.id != self.question_id
            || self.question.task_id != self.task_id
            || self.question.content_fingerprint().as_ref()
                != Ok(&self.question_content_fingerprint)
            || self.answer.validate().is_err()
            || matches!(self.answer, NormalizedAnswer::Unknown)
            || self.verified_at < self.observed_at
            || !valid_provenance(&self.provenance_sanitized)
        {
            return Err(PrivateAnswerEvidenceValidationError::InvalidEvidence);
        }
        match self.evidence_class {
            AnswerEvidenceClass::Official => {
                if self.answer_source != AnswerSource::ProviderNative {
                    return Err(PrivateAnswerEvidenceValidationError::InvalidEvidence);
                }
            }
            AnswerEvidenceClass::VerifiedHistorical | AnswerEvidenceClass::Negative => {
                if self.source_candidate_id.is_none()
                    || (self.execution_attempt_id.is_none()
                        && self.provider_attempt_digest.is_none())
                    || self.result_digest.is_none_or(|digest| digest == [0; 32])
                {
                    return Err(PrivateAnswerEvidenceValidationError::InvalidEvidence);
                }
            }
        }
        if self.result_digest == Some([0; 32]) {
            return Err(PrivateAnswerEvidenceValidationError::InvalidEvidence);
        }
        if self.provider_attempt_digest == Some([0; 32]) {
            return Err(PrivateAnswerEvidenceValidationError::InvalidEvidence);
        }
        if matches!(self.projection, CorpusProjectionEligibility::Exact)
            && self.global_projection().is_err()
        {
            return Err(PrivateAnswerEvidenceValidationError::UnsafeProjection);
        }
        Ok(())
    }

    /// Builds the identity-free exact-v1 Corpus payload.
    ///
    /// # Errors
    ///
    /// Rejects Question shapes whose context or answer cannot yet be safely
    /// represented without private or snapshot-local identity.
    pub fn global_projection(
        &self,
    ) -> Result<
        (GlobalCorpusQuestionAsset, GlobalSemanticAnswer),
        PrivateAnswerEvidenceValidationError,
    > {
        let question = GlobalCorpusQuestionAsset::try_from_question(&self.question)?;
        let answer = GlobalSemanticAnswer::try_from_answer(&self.question, &self.answer)?;
        Ok((question, answer))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GlobalCorpusQuestionAsset {
    pub kind: QuestionKind,
    pub stem: String,
    pub options: Vec<GlobalCorpusQuestionOption>,
}

impl GlobalCorpusQuestionAsset {
    fn try_from_question(
        question: &Question,
    ) -> Result<Self, PrivateAnswerEvidenceValidationError> {
        if !matches!(
            question.kind,
            QuestionKind::SingleChoice
                | QuestionKind::MultipleChoice
                | QuestionKind::TrueFalse
                | QuestionKind::FillBlank
                | QuestionKind::ShortAnswer
        ) || !question.attachments.is_empty()
        {
            return Err(PrivateAnswerEvidenceValidationError::UnsafeProjection);
        }
        let options = question
            .options
            .iter()
            .map(|option| {
                if !option.attachments.is_empty() {
                    return Err(PrivateAnswerEvidenceValidationError::UnsafeProjection);
                }
                let content = option
                    .content
                    .clone()
                    .ok_or(PrivateAnswerEvidenceValidationError::UnsafeProjection)?;
                Ok(GlobalCorpusQuestionOption { content })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            kind: question.kind,
            stem: question.stem.clone(),
            options,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GlobalCorpusQuestionOption {
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum GlobalSemanticAnswer {
    Selections(Vec<String>),
    Boolean(bool),
    Texts(Vec<String>),
}

impl GlobalSemanticAnswer {
    fn try_from_answer(
        question: &Question,
        answer: &NormalizedAnswer,
    ) -> Result<Self, PrivateAnswerEvidenceValidationError> {
        match answer {
            NormalizedAnswer::Selections(option_ids)
                if matches!(
                    question.kind,
                    QuestionKind::SingleChoice | QuestionKind::MultipleChoice
                ) =>
            {
                let selected_ids = option_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                if selected_ids.len() != option_ids.len() {
                    return Err(PrivateAnswerEvidenceValidationError::UnsafeProjection);
                }
                let mut selected = Vec::with_capacity(option_ids.len());
                let mut unique = BTreeSet::new();
                for option in &question.options {
                    if !selected_ids.contains(option.id.as_str()) {
                        continue;
                    }
                    let content = option
                        .content
                        .clone()
                        .ok_or(PrivateAnswerEvidenceValidationError::UnsafeProjection)?;
                    if !unique.insert(content.clone()) {
                        return Err(PrivateAnswerEvidenceValidationError::UnsafeProjection);
                    }
                    selected.push(content);
                }
                if selected.len() != option_ids.len() {
                    return Err(PrivateAnswerEvidenceValidationError::UnsafeProjection);
                }
                Ok(Self::Selections(selected))
            }
            NormalizedAnswer::Boolean(value) if question.kind == QuestionKind::TrueFalse => {
                Ok(Self::Boolean(*value))
            }
            NormalizedAnswer::Texts(values)
                if matches!(
                    question.kind,
                    QuestionKind::FillBlank | QuestionKind::ShortAnswer
                ) =>
            {
                Ok(Self::Texts(values.clone()))
            }
            _ => Err(PrivateAnswerEvidenceValidationError::UnsafeProjection),
        }
    }
}

fn valid_provenance(value: &Value) -> bool {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return false;
    };
    !bytes.is_empty() && bytes.len() <= MAX_PROVENANCE_BYTES && !contains_sensitive_key(value)
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
pub enum PrivateAnswerEvidenceValidationError {
    #[error("private answer evidence is incomplete, unverified or cross-bound")]
    InvalidEvidence,
    #[error("answer evidence cannot be safely projected with the current semantic model")]
    UnsafeProjection,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::QuestionOption;

    use super::*;

    #[test]
    fn initial_bootstrap_harvest_is_pending_and_bounded() {
        let now = Utc::now();
        let mut harvest = AnswerBootstrapHarvest {
            id: AnswerBootstrapHarvestId::new(),
            owner_user_id: UserId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_account_id: ProviderAccountId::new(),
            generation: 1,
            schedule_id: ScheduleId::new(),
            state: AnswerBootstrapHarvestState::Pending,
            scanned_task_count: 0,
            total_task_count: None,
            watermark_sanitized: serde_json::json!({}),
            created_at: now,
            started_at: None,
            updated_at: now,
            completed_at: None,
        };
        harvest.validate().unwrap();

        harvest.scanned_task_count = 1;
        assert_eq!(
            harvest.validate(),
            Err(AnswerBootstrapHarvestValidationError)
        );
    }

    fn evidence(projection: CorpusProjectionEligibility) -> PrivateAnswerEvidence {
        let task_id = TaskId::new();
        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some("question-private-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Which value is correct?".to_owned(),
            options: vec![
                QuestionOption {
                    id: "A".to_owned(),
                    content: Some("Alpha".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
                QuestionOption {
                    id: "B".to_owned(),
                    content: Some("Beta".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            position: 1,
        };
        let fingerprint = question.content_fingerprint().unwrap();
        let now = Utc::now();
        PrivateAnswerEvidence {
            id: PrivateAnswerEvidenceId::new(),
            owner_user_id: UserId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_account_id: ProviderAccountId::new(),
            course_id: None,
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            question_id: question.id,
            execution_attempt_id: Some(ExecutionAttemptId::new()),
            provider_attempt_digest: None,
            source_candidate_id: Some(AnswerCandidateId::new()),
            question,
            question_content_fingerprint: fingerprint,
            answer: NormalizedAnswer::Selections(vec!["B".to_owned()]),
            answer_source: AnswerSource::Manual,
            evidence_class: AnswerEvidenceClass::VerifiedHistorical,
            result_digest: Some([7; 32]),
            provenance_sanitized: serde_json::json!({"surface": "result_page"}),
            projection,
            observed_at: now,
            verified_at: now,
        }
    }

    #[test]
    fn exact_projection_removes_snapshot_local_option_identity() {
        let evidence = evidence(CorpusProjectionEligibility::Exact);
        evidence.validate().unwrap();
        let (question, answer) = evidence.global_projection().unwrap();
        assert_eq!(question.options[1].content, "Beta");
        assert_eq!(
            answer,
            GlobalSemanticAnswer::Selections(vec!["Beta".to_owned()])
        );
    }

    #[test]
    fn choice_projection_canonicalizes_selection_order() {
        let mut evidence = evidence(CorpusProjectionEligibility::Exact);
        evidence.question.kind = QuestionKind::MultipleChoice;
        evidence.question_content_fingerprint = evidence.question.content_fingerprint().unwrap();
        evidence.answer = NormalizedAnswer::Selections(vec!["B".to_owned(), "A".to_owned()]);
        evidence.validate().unwrap();
        let (_, answer) = evidence.global_projection().unwrap();
        assert_eq!(
            answer,
            GlobalSemanticAnswer::Selections(vec!["Alpha".to_owned(), "Beta".to_owned()])
        );
    }

    #[test]
    fn unsupported_hierarchy_must_remain_explicitly_unmatched() {
        let mut unmatched = evidence(CorpusProjectionEligibility::Unmatched(
            UnmatchedEvidenceReason::UnsupportedHierarchy,
        ));
        unmatched.question.kind = QuestionKind::Composite;
        unmatched.question_content_fingerprint = unmatched.question.content_fingerprint().unwrap();
        assert!(unmatched.validate().is_ok());

        unmatched.projection = CorpusProjectionEligibility::Exact;
        assert_eq!(
            unmatched.validate(),
            Err(PrivateAnswerEvidenceValidationError::UnsafeProjection)
        );
    }

    #[test]
    fn private_provenance_rejects_secret_shaped_keys() {
        let mut evidence = evidence(CorpusProjectionEligibility::Exact);
        evidence.provenance_sanitized = serde_json::json!({"access_token": "hidden"});
        assert_eq!(
            evidence.validate(),
            Err(PrivateAnswerEvidenceValidationError::InvalidEvidence)
        );
    }

    #[test]
    fn bootstrap_history_uses_provider_attempt_without_local_execution() {
        let mut evidence = evidence(CorpusProjectionEligibility::Exact);
        evidence.execution_attempt_id = None;
        evidence.provider_attempt_digest = Some([8; 32]);
        assert!(evidence.validate().is_ok());

        evidence.provider_attempt_digest = None;
        assert_eq!(
            evidence.validate(),
            Err(PrivateAnswerEvidenceValidationError::InvalidEvidence)
        );
    }
}
