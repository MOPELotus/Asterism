use serde::{Deserialize, Serialize};

use crate::{
    ProviderAccountId, ScoreImprovementWorkflowId, StrictCompletionWorkflowId, SubmissionScore,
    TaskId, Timestamp, UserId,
};

const MAX_POLICY_ATTEMPTS: u32 = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Completed,
    Passed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionDiagnosis {
    ScoreBelowThreshold,
    DurationInsufficient,
    RequiredChildrenPending,
    PrerequisiteLocked,
    TeacherReviewPending,
    HumanActionRequired,
    UnsupportedCapability,
    ProtocolDrift,
    AttemptLimitReached,
    WindowClosed,
    RemoteUnknown,
}

impl CompletionDiagnosis {
    const fn stops_automatic_completion(self) -> bool {
        matches!(
            self,
            Self::PrerequisiteLocked
                | Self::TeacherReviewPending
                | Self::HumanActionRequired
                | Self::UnsupportedCapability
                | Self::ProtocolDrift
                | Self::AttemptLimitReached
                | Self::WindowClosed
                | Self::RemoteUnknown
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetakeScorePolicy {
    HighestScore,
    LastAttempt,
    Average,
    TeacherRule,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletionPolicySnapshot {
    pub strict_completion_enabled: bool,
    pub score_improvement_enabled: bool,
    pub strict_attempt_limit: u32,
    pub score_improvement_attempt_limit: u32,
    pub score_target_millis: u16,
    pub strict_expires_at: Option<Timestamp>,
    pub score_improvement_expires_at: Option<Timestamp>,
    pub formal_retry_requires_confirmation: bool,
    pub captured_at: Timestamp,
}

impl CompletionPolicySnapshot {
    /// Validates bounded attempts, score target and snapshot-relative deadlines.
    ///
    /// # Errors
    ///
    /// Returns [`CompletionPolicyValidationError`] for zero/excessive limits,
    /// an invalid target or a deadline that is not after capture.
    pub fn validate(&self) -> Result<(), CompletionPolicyValidationError> {
        if self.strict_attempt_limit == 0
            || self.strict_attempt_limit > MAX_POLICY_ATTEMPTS
            || self.score_improvement_attempt_limit == 0
            || self.score_improvement_attempt_limit > MAX_POLICY_ATTEMPTS
            || !(1..=1_000).contains(&self.score_target_millis)
            || self
                .strict_expires_at
                .is_some_and(|expires_at| expires_at <= self.captured_at)
            || self
                .score_improvement_expires_at
                .is_some_and(|expires_at| expires_at <= self.captured_at)
        {
            return Err(CompletionPolicyValidationError);
        }
        Ok(())
    }
}

impl Default for CompletionPolicySnapshot {
    fn default() -> Self {
        let captured_at = chrono::Utc::now();
        Self {
            strict_completion_enabled: true,
            score_improvement_enabled: false,
            strict_attempt_limit: 3,
            score_improvement_attempt_limit: 1,
            score_target_millis: 1_000,
            strict_expires_at: None,
            score_improvement_expires_at: None,
            formal_retry_requires_confirmation: true,
            captured_at,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("completion policy snapshot is invalid or unbounded")]
pub struct CompletionPolicyValidationError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletionWorkflowBinding {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StrictCompletionState {
    Disabled,
    Active,
    AttemptRunning,
    Completed,
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StrictCompletionWorkflow {
    pub id: StrictCompletionWorkflowId,
    pub binding: CompletionWorkflowBinding,
    pub policy: CompletionPolicySnapshot,
    pub state: StrictCompletionState,
    pub attempts_started: u32,
    pub last_diagnosis: Option<CompletionDiagnosis>,
    pub verified_outcome: Option<CompletionOutcome>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

impl StrictCompletionWorkflow {
    /// Creates a disabled, active or already-completed Strict workflow.
    ///
    /// # Errors
    ///
    /// Returns [`StrictCompletionWorkflowError`] for an invalid frozen policy
    /// or a creation timestamp before policy capture.
    pub fn new(
        binding: CompletionWorkflowBinding,
        policy: CompletionPolicySnapshot,
        already_completed: Option<CompletionOutcome>,
        at: Timestamp,
    ) -> Result<Self, StrictCompletionWorkflowError> {
        policy.validate()?;
        if policy.captured_at > at {
            return Err(StrictCompletionWorkflowError::InvalidTimestamp);
        }
        let (state, finished_at) = if already_completed.is_some() {
            (StrictCompletionState::Completed, Some(at))
        } else if policy.strict_completion_enabled {
            (StrictCompletionState::Active, None)
        } else {
            (StrictCompletionState::Disabled, Some(at))
        };
        Ok(Self {
            id: StrictCompletionWorkflowId::new(),
            binding,
            policy,
            state,
            attempts_started: 0,
            last_diagnosis: None,
            verified_outcome: already_completed,
            created_at: at,
            updated_at: at,
            finished_at,
        })
    }

    /// Validates a deserialized workflow before a persistence transition.
    ///
    /// # Errors
    ///
    /// Returns [`StrictCompletionWorkflowError`] when state, attempts,
    /// outcome, diagnosis or timestamps violate the state machine.
    pub fn validate(&self) -> Result<(), StrictCompletionWorkflowError> {
        self.policy.validate()?;
        if self.policy.captured_at > self.created_at
            || self.updated_at < self.created_at
            || self.attempts_started > self.policy.strict_attempt_limit
            || self.finished_at.is_some_and(|finished_at| {
                finished_at < self.created_at || finished_at > self.updated_at
            })
        {
            return Err(StrictCompletionWorkflowError::InvalidTimestamp);
        }
        let valid = match self.state {
            StrictCompletionState::Disabled => {
                !self.policy.strict_completion_enabled
                    && self.attempts_started == 0
                    && self.verified_outcome.is_none()
                    && self.last_diagnosis.is_none()
                    && self.finished_at.is_some()
            }
            StrictCompletionState::Active => {
                self.policy.strict_completion_enabled
                    && self.attempts_started < self.policy.strict_attempt_limit
                    && self.verified_outcome.is_none()
                    && self.finished_at.is_none()
            }
            StrictCompletionState::AttemptRunning => {
                self.policy.strict_completion_enabled
                    && (1..=self.policy.strict_attempt_limit).contains(&self.attempts_started)
                    && self.verified_outcome.is_none()
                    && self.finished_at.is_none()
            }
            StrictCompletionState::Completed => {
                self.verified_outcome.is_some()
                    && self.last_diagnosis.is_none()
                    && self.finished_at.is_some()
            }
            StrictCompletionState::Stopped => {
                self.verified_outcome.is_none()
                    && self.last_diagnosis.is_some()
                    && self.finished_at.is_some()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(StrictCompletionWorkflowError::StateConflict)
        }
    }

    /// Starts one bounded attempt without granting an unconfirmed formal retry.
    ///
    /// # Errors
    ///
    /// Returns [`StrictCompletionWorkflowError`] for a terminal/conflicting
    /// state, expired limits, missing confirmation or regressing time.
    pub fn begin_attempt(
        &mut self,
        formal_assessment: bool,
        retry_confirmed: bool,
        at: Timestamp,
    ) -> Result<u32, StrictCompletionWorkflowError> {
        self.validate_transition_time(at)?;
        if self.state != StrictCompletionState::Active {
            return Err(StrictCompletionWorkflowError::StateConflict);
        }
        if self
            .policy
            .strict_expires_at
            .is_some_and(|expires_at| at >= expires_at)
        {
            self.stop(CompletionDiagnosis::WindowClosed, at);
            return Err(StrictCompletionWorkflowError::LimitReached);
        }
        if self.attempts_started >= self.policy.strict_attempt_limit {
            self.stop(CompletionDiagnosis::AttemptLimitReached, at);
            return Err(StrictCompletionWorkflowError::LimitReached);
        }
        if formal_assessment
            && self.attempts_started > 0
            && self.policy.formal_retry_requires_confirmation
            && !retry_confirmed
        {
            return Err(StrictCompletionWorkflowError::ConfirmationRequired);
        }
        self.attempts_started += 1;
        self.state = StrictCompletionState::AttemptRunning;
        self.updated_at = at;
        Ok(self.attempts_started)
    }

    /// Applies one fresh remote completion observation to the running attempt.
    ///
    /// # Errors
    ///
    /// Returns [`StrictCompletionWorkflowError`] for a conflicting state,
    /// regressing time or a non-completion observation without diagnosis.
    pub fn observe(
        &mut self,
        outcome: Option<CompletionOutcome>,
        diagnosis: Option<CompletionDiagnosis>,
        at: Timestamp,
    ) -> Result<(), StrictCompletionWorkflowError> {
        self.validate_transition_time(at)?;
        if self.state != StrictCompletionState::AttemptRunning {
            return Err(StrictCompletionWorkflowError::StateConflict);
        }
        if let Some(outcome) = outcome {
            return self.complete(outcome, at);
        }
        let diagnosis = diagnosis.ok_or(StrictCompletionWorkflowError::DiagnosisRequired)?;
        self.last_diagnosis = Some(diagnosis);
        self.updated_at = at;
        if diagnosis.stops_automatic_completion()
            || self.attempts_started >= self.policy.strict_attempt_limit
        {
            self.stop(
                if self.attempts_started >= self.policy.strict_attempt_limit {
                    CompletionDiagnosis::AttemptLimitReached
                } else {
                    diagnosis
                },
                at,
            );
        } else {
            self.state = StrictCompletionState::Active;
        }
        Ok(())
    }

    /// Applies a fresh verified Completed/Passed observation even after
    /// automatic completion was disabled or stopped. Completion is monotonic:
    /// this method cannot attach a diagnosis or reopen a completed workflow.
    ///
    /// # Errors
    ///
    /// Returns [`StrictCompletionWorkflowError`] for regressing time or a
    /// conflicting second completion outcome.
    pub fn observe_verified_completion(
        &mut self,
        outcome: CompletionOutcome,
        at: Timestamp,
    ) -> Result<(), StrictCompletionWorkflowError> {
        self.validate_transition_time(at)?;
        if self.state == StrictCompletionState::Completed {
            return if self.verified_outcome == Some(outcome) {
                Ok(())
            } else {
                Err(StrictCompletionWorkflowError::StateConflict)
            };
        }
        self.complete(outcome, at)
    }

    fn complete(
        &mut self,
        outcome: CompletionOutcome,
        at: Timestamp,
    ) -> Result<(), StrictCompletionWorkflowError> {
        self.state = StrictCompletionState::Completed;
        self.verified_outcome = Some(outcome);
        self.last_diagnosis = None;
        self.updated_at = at;
        self.finished_at = Some(at);
        self.validate()
    }

    fn stop(&mut self, diagnosis: CompletionDiagnosis, at: Timestamp) {
        self.state = StrictCompletionState::Stopped;
        self.last_diagnosis = Some(diagnosis);
        self.updated_at = at;
        self.finished_at = Some(at);
    }

    fn validate_transition_time(&self, at: Timestamp) -> Result<(), StrictCompletionWorkflowError> {
        if at < self.updated_at {
            Err(StrictCompletionWorkflowError::InvalidTimestamp)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StrictCompletionWorkflowError {
    #[error("strict completion policy is invalid")]
    InvalidPolicy(#[from] CompletionPolicyValidationError),
    #[error("strict completion transition conflicts with its current state")]
    StateConflict,
    #[error("strict completion transition timestamp regresses")]
    InvalidTimestamp,
    #[error("strict completion attempt requires explicit confirmation")]
    ConfirmationRequired,
    #[error("strict completion attempt or time limit is reached")]
    LimitReached,
    #[error("strict completion observation without completion requires a diagnosis")]
    DiagnosisRequired,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifiedCompletionBaseline {
    pub outcome: CompletionOutcome,
    pub score: Option<SubmissionScore>,
    pub verified_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreImprovementState {
    Disabled,
    Ready,
    AttemptRunning,
    Finished,
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScoreImprovementWorkflow {
    pub id: ScoreImprovementWorkflowId,
    pub binding: CompletionWorkflowBinding,
    pub policy: CompletionPolicySnapshot,
    pub completion_baseline: VerifiedCompletionBaseline,
    pub retake_score_policy: RetakeScorePolicy,
    pub explicitly_opted_in: bool,
    pub state: ScoreImprovementState,
    pub attempts_started: u32,
    pub best_observed_score: Option<SubmissionScore>,
    pub last_diagnosis: Option<CompletionDiagnosis>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

impl ScoreImprovementWorkflow {
    /// Creates a separate opt-in retake workflow over an immutable completion baseline.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreImprovementWorkflowError`] for an invalid policy or a
    /// timestamp preceding policy/baseline verification.
    pub fn new(
        binding: CompletionWorkflowBinding,
        policy: CompletionPolicySnapshot,
        completion_baseline: VerifiedCompletionBaseline,
        retake_score_policy: RetakeScorePolicy,
        explicitly_opted_in: bool,
        at: Timestamp,
    ) -> Result<Self, ScoreImprovementWorkflowError> {
        policy.validate()?;
        if policy.captured_at > at || completion_baseline.verified_at > at {
            return Err(ScoreImprovementWorkflowError::InvalidTimestamp);
        }
        let enabled = policy.score_improvement_enabled && explicitly_opted_in;
        let baseline_reaches_target = completion_baseline
            .score
            .is_some_and(|score| score_millis(score) >= u128::from(policy.score_target_millis));
        let (state, diagnosis, finished_at) = if !enabled {
            (ScoreImprovementState::Disabled, None, Some(at))
        } else if baseline_reaches_target {
            (ScoreImprovementState::Finished, None, Some(at))
        } else if matches!(
            retake_score_policy,
            RetakeScorePolicy::TeacherRule | RetakeScorePolicy::Unknown
        ) {
            (
                ScoreImprovementState::Stopped,
                Some(CompletionDiagnosis::HumanActionRequired),
                Some(at),
            )
        } else {
            (ScoreImprovementState::Ready, None, None)
        };
        Ok(Self {
            id: ScoreImprovementWorkflowId::new(),
            binding,
            policy,
            completion_baseline,
            retake_score_policy,
            explicitly_opted_in,
            state,
            attempts_started: 0,
            best_observed_score: completion_baseline.score,
            last_diagnosis: diagnosis,
            created_at: at,
            updated_at: at,
            finished_at,
        })
    }

    /// Validates a deserialized retake workflow and its immutable baseline.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreImprovementWorkflowError`] when state, attempts, scores,
    /// opt-in policy, diagnosis or timestamps violate the state machine.
    pub fn validate(&self) -> Result<(), ScoreImprovementWorkflowError> {
        self.policy.validate()?;
        if self.policy.captured_at > self.created_at
            || self.completion_baseline.verified_at > self.created_at
            || self.updated_at < self.created_at
            || self.attempts_started > self.policy.score_improvement_attempt_limit
            || self.finished_at.is_some_and(|finished_at| {
                finished_at < self.created_at || finished_at > self.updated_at
            })
            || self
                .completion_baseline
                .score
                .is_some_and(|score| score.validate().is_err())
            || self
                .best_observed_score
                .is_some_and(|score| score.validate().is_err())
            || matches!(
                (self.completion_baseline.score, self.best_observed_score),
                (Some(baseline), Some(best)) if score_is_higher(baseline, best)
            )
        {
            return Err(ScoreImprovementWorkflowError::InvalidScore);
        }
        let enabled = self.policy.score_improvement_enabled && self.explicitly_opted_in;
        let target_reached = self.best_observed_score.is_some_and(|score| {
            score_millis(score) >= u128::from(self.policy.score_target_millis)
        });
        let valid = match self.state {
            ScoreImprovementState::Disabled => {
                !enabled
                    && self.attempts_started == 0
                    && self.last_diagnosis.is_none()
                    && self.finished_at.is_some()
            }
            ScoreImprovementState::Ready => {
                enabled
                    && !target_reached
                    && self.attempts_started < self.policy.score_improvement_attempt_limit
                    && self.finished_at.is_none()
            }
            ScoreImprovementState::AttemptRunning => {
                enabled
                    && (1..=self.policy.score_improvement_attempt_limit)
                        .contains(&self.attempts_started)
                    && self.finished_at.is_none()
            }
            ScoreImprovementState::Finished => target_reached && self.finished_at.is_some(),
            ScoreImprovementState::Stopped => {
                enabled && self.last_diagnosis.is_some() && self.finished_at.is_some()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(ScoreImprovementWorkflowError::StateConflict)
        }
    }

    /// Starts one explicitly confirmed retake inside the frozen bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreImprovementWorkflowError`] for missing confirmation,
    /// a terminal/conflicting state, exhausted bounds or regressing time.
    pub fn begin_retake(
        &mut self,
        explicitly_confirmed: bool,
        at: Timestamp,
    ) -> Result<u32, ScoreImprovementWorkflowError> {
        self.validate_transition_time(at)?;
        if self.state != ScoreImprovementState::Ready {
            return Err(ScoreImprovementWorkflowError::StateConflict);
        }
        if !explicitly_confirmed {
            return Err(ScoreImprovementWorkflowError::ConfirmationRequired);
        }
        if self
            .policy
            .score_improvement_expires_at
            .is_some_and(|expires_at| at >= expires_at)
        {
            self.stop(CompletionDiagnosis::WindowClosed, at);
            return Err(ScoreImprovementWorkflowError::LimitReached);
        }
        if self.attempts_started >= self.policy.score_improvement_attempt_limit {
            self.stop(CompletionDiagnosis::AttemptLimitReached, at);
            return Err(ScoreImprovementWorkflowError::LimitReached);
        }
        self.attempts_started += 1;
        self.state = ScoreImprovementState::AttemptRunning;
        self.updated_at = at;
        Ok(self.attempts_started)
    }

    /// Records a retake score/diagnosis without changing the completion baseline.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreImprovementWorkflowError`] for invalid score/diagnosis,
    /// a conflicting state or regressing time.
    pub fn observe(
        &mut self,
        score: Option<SubmissionScore>,
        retake_still_allowed: bool,
        diagnosis: Option<CompletionDiagnosis>,
        at: Timestamp,
    ) -> Result<(), ScoreImprovementWorkflowError> {
        self.validate_transition_time(at)?;
        if self.state != ScoreImprovementState::AttemptRunning {
            return Err(ScoreImprovementWorkflowError::StateConflict);
        }
        if let Some(score) = score {
            score
                .validate()
                .map_err(|_| ScoreImprovementWorkflowError::InvalidScore)?;
            if self
                .best_observed_score
                .is_none_or(|best| score_is_higher(score, best))
            {
                self.best_observed_score = Some(score);
            }
            self.last_diagnosis = diagnosis;
        } else {
            self.last_diagnosis =
                Some(diagnosis.ok_or(ScoreImprovementWorkflowError::DiagnosisRequired)?);
        }
        self.updated_at = at;
        let target_reached = self
            .best_observed_score
            .is_some_and(|best| score_millis(best) >= u128::from(self.policy.score_target_millis));
        if target_reached {
            self.state = ScoreImprovementState::Finished;
            self.finished_at = Some(at);
        } else if !retake_still_allowed
            || self.attempts_started >= self.policy.score_improvement_attempt_limit
        {
            self.stop(CompletionDiagnosis::AttemptLimitReached, at);
        } else {
            self.state = ScoreImprovementState::Ready;
        }
        Ok(())
    }

    fn stop(&mut self, diagnosis: CompletionDiagnosis, at: Timestamp) {
        self.state = ScoreImprovementState::Stopped;
        self.last_diagnosis = Some(diagnosis);
        self.updated_at = at;
        self.finished_at = Some(at);
    }

    fn validate_transition_time(&self, at: Timestamp) -> Result<(), ScoreImprovementWorkflowError> {
        if at < self.updated_at {
            Err(ScoreImprovementWorkflowError::InvalidTimestamp)
        } else {
            Ok(())
        }
    }
}

fn score_is_higher(candidate: SubmissionScore, current: SubmissionScore) -> bool {
    u128::from(candidate.earned_milli_points) * u128::from(current.possible_milli_points)
        > u128::from(current.earned_milli_points) * u128::from(candidate.possible_milli_points)
}

fn score_millis(score: SubmissionScore) -> u128 {
    u128::from(score.earned_milli_points) * 1_000 / u128::from(score.possible_milli_points)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScoreImprovementWorkflowError {
    #[error("score improvement policy is invalid")]
    InvalidPolicy(#[from] CompletionPolicyValidationError),
    #[error("score improvement transition conflicts with its current state")]
    StateConflict,
    #[error("score improvement transition timestamp regresses")]
    InvalidTimestamp,
    #[error("score improvement retake requires explicit confirmation")]
    ConfirmationRequired,
    #[error("score improvement attempt or time limit is reached")]
    LimitReached,
    #[error("score improvement observation without a score requires a diagnosis")]
    DiagnosisRequired,
    #[error("score improvement observation contains an invalid score")]
    InvalidScore,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn binding() -> CompletionWorkflowBinding {
        CompletionWorkflowBinding {
            owner_user_id: UserId::new(),
            provider_account_id: ProviderAccountId::new(),
            task_id: TaskId::new(),
        }
    }

    fn policy(at: Timestamp) -> CompletionPolicySnapshot {
        CompletionPolicySnapshot {
            strict_completion_enabled: true,
            score_improvement_enabled: true,
            strict_attempt_limit: 2,
            score_improvement_attempt_limit: 2,
            score_target_millis: 900,
            strict_expires_at: Some(at + Duration::hours(1)),
            score_improvement_expires_at: Some(at + Duration::hours(1)),
            formal_retry_requires_confirmation: true,
            captured_at: at,
        }
    }

    #[test]
    fn strict_completion_stops_at_verified_completion_and_never_retries_it() {
        let now = Utc::now();
        let mut workflow =
            StrictCompletionWorkflow::new(binding(), policy(now), None, now).unwrap();
        assert_eq!(workflow.begin_attempt(false, false, now).unwrap(), 1);
        workflow
            .observe(
                Some(CompletionOutcome::Passed),
                None,
                now + Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(workflow.state, StrictCompletionState::Completed);
        assert_eq!(workflow.verified_outcome, Some(CompletionOutcome::Passed));
        assert!(
            workflow
                .begin_attempt(false, false, now + Duration::seconds(2))
                .is_err()
        );
    }

    #[test]
    fn formal_strict_retry_requires_confirmation_and_obeys_attempt_limit() {
        let now = Utc::now();
        let mut workflow =
            StrictCompletionWorkflow::new(binding(), policy(now), None, now).unwrap();
        workflow.begin_attempt(true, false, now).unwrap();
        workflow
            .observe(
                None,
                Some(CompletionDiagnosis::ScoreBelowThreshold),
                now + Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(workflow.state, StrictCompletionState::Active);
        assert_eq!(
            workflow.begin_attempt(true, false, now + Duration::seconds(2)),
            Err(StrictCompletionWorkflowError::ConfirmationRequired)
        );
        workflow
            .begin_attempt(true, true, now + Duration::seconds(2))
            .unwrap();
        workflow
            .observe(
                None,
                Some(CompletionDiagnosis::ScoreBelowThreshold),
                now + Duration::seconds(3),
            )
            .unwrap();
        assert_eq!(workflow.state, StrictCompletionState::Stopped);
        assert_eq!(
            workflow.last_diagnosis,
            Some(CompletionDiagnosis::AttemptLimitReached)
        );
    }

    #[test]
    fn fresh_completion_is_monotonic_after_automatic_work_stops() {
        let now = Utc::now();
        let mut workflow =
            StrictCompletionWorkflow::new(binding(), policy(now), None, now).unwrap();
        workflow.begin_attempt(false, false, now).unwrap();
        workflow
            .observe(
                None,
                Some(CompletionDiagnosis::HumanActionRequired),
                now + Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(workflow.state, StrictCompletionState::Stopped);

        workflow
            .observe_verified_completion(CompletionOutcome::Completed, now + Duration::seconds(2))
            .unwrap();
        assert_eq!(workflow.state, StrictCompletionState::Completed);
        assert_eq!(
            workflow.verified_outcome,
            Some(CompletionOutcome::Completed)
        );
        assert_eq!(workflow.last_diagnosis, None);
        assert_eq!(
            workflow
                .observe_verified_completion(CompletionOutcome::Passed, now + Duration::seconds(3)),
            Err(StrictCompletionWorkflowError::StateConflict)
        );
    }

    #[test]
    fn score_improvement_is_opt_in_and_cannot_reverse_completion_baseline() {
        let now = Utc::now();
        let baseline = VerifiedCompletionBaseline {
            outcome: CompletionOutcome::Completed,
            score: Some(SubmissionScore {
                earned_milli_points: 80,
                possible_milli_points: 100,
            }),
            verified_at: now,
        };
        let disabled = ScoreImprovementWorkflow::new(
            binding(),
            policy(now),
            baseline,
            RetakeScorePolicy::HighestScore,
            false,
            now,
        )
        .unwrap();
        assert_eq!(disabled.state, ScoreImprovementState::Disabled);

        let mut active = ScoreImprovementWorkflow::new(
            binding(),
            policy(now),
            baseline,
            RetakeScorePolicy::LastAttempt,
            true,
            now,
        )
        .unwrap();
        assert_eq!(
            active.begin_retake(false, now),
            Err(ScoreImprovementWorkflowError::ConfirmationRequired)
        );
        active.begin_retake(true, now).unwrap();
        active
            .observe(
                Some(SubmissionScore {
                    earned_milli_points: 60,
                    possible_milli_points: 100,
                }),
                false,
                Some(CompletionDiagnosis::ScoreBelowThreshold),
                now + Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(active.state, ScoreImprovementState::Stopped);
        assert_eq!(active.completion_baseline, baseline);
        assert_eq!(active.best_observed_score, baseline.score);
    }

    #[test]
    fn unknown_retake_policy_never_starts_automatically() {
        let now = Utc::now();
        let baseline = VerifiedCompletionBaseline {
            outcome: CompletionOutcome::Completed,
            score: None,
            verified_at: now,
        };
        let workflow = ScoreImprovementWorkflow::new(
            binding(),
            policy(now),
            baseline,
            RetakeScorePolicy::Unknown,
            true,
            now,
        )
        .unwrap();
        assert_eq!(workflow.state, ScoreImprovementState::Stopped);
        assert_eq!(
            workflow.last_diagnosis,
            Some(CompletionDiagnosis::HumanActionRequired)
        );
    }
}
