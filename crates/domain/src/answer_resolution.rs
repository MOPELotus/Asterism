use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{AnswerCandidateId, NormalizedAnswer, QuestionId, QuestionSnapshotId, TaskId};

const MAX_RESOLUTION_DECISIONS: usize = 5_000;
const MAX_CONSIDERED_CANDIDATES: usize = 20_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerResolutionStatus {
    Selected,
    Conflict,
    Missing,
}

/// One transparent, non-persisted resolution decision for a Question. A
/// conflict or missing decision never carries a selected answer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerResolutionDecision {
    pub question_id: QuestionId,
    pub status: AnswerResolutionStatus,
    pub considered_candidate_ids: Vec<AnswerCandidateId>,
    pub selected_candidate_id: Option<AnswerCandidateId>,
    pub selected_answer: Option<NormalizedAnswer>,
}

/// A reviewable selection plan derived only from persisted candidate evidence.
/// It is deliberately distinct from a `SubmissionDraft` and grants no remote
/// execution authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerResolutionPlan {
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub decisions: Vec<AnswerResolutionDecision>,
}

impl AnswerResolutionPlan {
    /// Validates identity uniqueness and the structural invariants for selected,
    /// conflicting, and missing decisions.
    ///
    /// # Errors
    ///
    /// Returns [`AnswerResolutionValidationError`] for duplicate identities,
    /// excessive collections, unknown selected answers, or status/payload
    /// combinations that could conceal an unresolved Question.
    pub fn validate(&self) -> Result<(), AnswerResolutionValidationError> {
        if self.decisions.len() > MAX_RESOLUTION_DECISIONS {
            return Err(AnswerResolutionValidationError::TooManyDecisions);
        }
        let mut question_ids = BTreeSet::new();
        let mut candidate_ids = BTreeSet::new();
        let mut total_candidates = 0usize;
        for decision in &self.decisions {
            if !question_ids.insert(decision.question_id) {
                return Err(AnswerResolutionValidationError::DuplicateIdentity);
            }
            total_candidates = total_candidates
                .checked_add(decision.considered_candidate_ids.len())
                .ok_or(AnswerResolutionValidationError::TooManyCandidates)?;
            if total_candidates > MAX_CONSIDERED_CANDIDATES
                || decision
                    .considered_candidate_ids
                    .iter()
                    .any(|candidate_id| !candidate_ids.insert(*candidate_id))
            {
                return Err(AnswerResolutionValidationError::TooManyCandidates);
            }
            match decision.status {
                AnswerResolutionStatus::Selected => {
                    let Some(selected_candidate_id) = decision.selected_candidate_id else {
                        return Err(AnswerResolutionValidationError::InvalidDecision);
                    };
                    let Some(answer) = &decision.selected_answer else {
                        return Err(AnswerResolutionValidationError::InvalidDecision);
                    };
                    if !decision
                        .considered_candidate_ids
                        .contains(&selected_candidate_id)
                        || matches!(answer, NormalizedAnswer::Unknown)
                        || answer.validate().is_err()
                    {
                        return Err(AnswerResolutionValidationError::InvalidDecision);
                    }
                }
                AnswerResolutionStatus::Conflict => {
                    if decision.considered_candidate_ids.len() < 2
                        || decision.selected_candidate_id.is_some()
                        || decision.selected_answer.is_some()
                    {
                        return Err(AnswerResolutionValidationError::InvalidDecision);
                    }
                }
                AnswerResolutionStatus::Missing => {
                    if !decision.considered_candidate_ids.is_empty()
                        || decision.selected_candidate_id.is_some()
                        || decision.selected_answer.is_some()
                    {
                        return Err(AnswerResolutionValidationError::InvalidDecision);
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnswerResolutionValidationError {
    #[error("answer resolution contains too many Question decisions")]
    TooManyDecisions,
    #[error("answer resolution contains too many or duplicate Candidate identities")]
    TooManyCandidates,
    #[error("answer resolution repeats a Question identity")]
    DuplicateIdentity,
    #[error("answer resolution decision status and payload are inconsistent")]
    InvalidDecision,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_distinguishes_selected_conflict_and_missing_without_hiding_uncertainty() {
        let selected_question = QuestionId::new();
        let selected_candidate = AnswerCandidateId::new();
        let conflict_a = AnswerCandidateId::new();
        let conflict_b = AnswerCandidateId::new();
        let plan = AnswerResolutionPlan {
            task_id: TaskId::new(),
            question_snapshot_id: QuestionSnapshotId::new(),
            decisions: vec![
                AnswerResolutionDecision {
                    question_id: selected_question,
                    status: AnswerResolutionStatus::Selected,
                    considered_candidate_ids: vec![selected_candidate],
                    selected_candidate_id: Some(selected_candidate),
                    selected_answer: Some(NormalizedAnswer::Boolean(true)),
                },
                AnswerResolutionDecision {
                    question_id: QuestionId::new(),
                    status: AnswerResolutionStatus::Conflict,
                    considered_candidate_ids: vec![conflict_a, conflict_b],
                    selected_candidate_id: None,
                    selected_answer: None,
                },
                AnswerResolutionDecision {
                    question_id: QuestionId::new(),
                    status: AnswerResolutionStatus::Missing,
                    considered_candidate_ids: Vec::new(),
                    selected_candidate_id: None,
                    selected_answer: None,
                },
            ],
        };
        assert_eq!(plan.validate(), Ok(()));
    }

    #[test]
    fn unresolved_decisions_cannot_smuggle_a_selection() {
        let candidate_id = AnswerCandidateId::new();
        let mut plan = AnswerResolutionPlan {
            task_id: TaskId::new(),
            question_snapshot_id: QuestionSnapshotId::new(),
            decisions: vec![AnswerResolutionDecision {
                question_id: QuestionId::new(),
                status: AnswerResolutionStatus::Missing,
                considered_candidate_ids: vec![candidate_id],
                selected_candidate_id: Some(candidate_id),
                selected_answer: Some(NormalizedAnswer::Unknown),
            }],
        };
        assert_eq!(
            plan.validate(),
            Err(AnswerResolutionValidationError::InvalidDecision)
        );

        plan.decisions[0].status = AnswerResolutionStatus::Selected;
        assert_eq!(
            plan.validate(),
            Err(AnswerResolutionValidationError::InvalidDecision)
        );
    }
}
