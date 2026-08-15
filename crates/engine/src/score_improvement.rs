use asterism_domain::{
    CompletionOutcome, CompletionPolicySnapshot, CompletionWorkflowBinding, RetakeScorePolicy,
    ScoreImprovementRetakeAuthority, ScoreImprovementWorkflow, StrictCompletionState,
    SubmissionScore, TaskId, Timestamp, UserId, VerifiedCompletionBaseline,
};
use asterism_provider_api::AnswerHistoryRetakeFacts;
use asterism_storage::{
    AnswerHistoryIngestionRepository, AnswerHistoryTaskFact, CompletionWorkflowCreateOutcome,
    CompletionWorkflowRepository, ScoreImprovementWorkflowRecord, StorageError,
    StrictCompletionWorkflowRecord,
};

#[derive(Clone, Copy, Debug)]
pub struct OptInScoreImprovementCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreImprovementOptInResult {
    pub record: ScoreImprovementWorkflowRecord,
    pub created: bool,
}

#[derive(Clone, Debug)]
pub struct ScoreImprovementOptInService<W, H> {
    workflows: W,
    history: H,
}

impl<W, H> ScoreImprovementOptInService<W, H> {
    pub const fn new(workflows: W, history: H) -> Self {
        Self { workflows, history }
    }
}

impl<W, H> ScoreImprovementOptInService<W, H>
where
    W: CompletionWorkflowRepository,
    H: AnswerHistoryIngestionRepository,
{
    /// Creates one explicit Score Improvement workflow from a verified
    /// completion baseline and the latest exact read-only Provider result.
    ///
    /// This only records opt-in state. It never creates a remote Attempt or
    /// invokes a Provider mutation.
    ///
    /// # Errors
    ///
    /// Returns [`ScoreImprovementOptInError`] when completion is unverified,
    /// history facts are stale or cross-bound, retake is unavailable, policy
    /// forbids the workflow, or persistence fails.
    pub async fn opt_in(
        &self,
        command: OptInScoreImprovementCommand,
    ) -> Result<ScoreImprovementOptInResult, ScoreImprovementOptInError> {
        if let Some(record) = self
            .workflows
            .find_owned_score_improvement_workflow(command.owner_id, command.task_id)
            .await?
        {
            return Ok(ScoreImprovementOptInResult {
                record,
                created: false,
            });
        }

        let strict = self
            .workflows
            .find_owned_strict_completion_workflow(command.owner_id, command.task_id)
            .await?
            .ok_or(ScoreImprovementOptInError::CompletionBaselineNotVerified)?;
        let history = self
            .history
            .find_latest_owned_answer_history_task_fact(command.owner_id, command.task_id)
            .await?
            .ok_or(ScoreImprovementOptInError::HistoryFactsUnavailable)?;
        let workflow = build_workflow(&strict, &history, command)?;

        match self
            .workflows
            .create_score_improvement_workflow(&workflow)
            .await?
        {
            CompletionWorkflowCreateOutcome::Created(record) => Ok(ScoreImprovementOptInResult {
                record,
                created: true,
            }),
            CompletionWorkflowCreateOutcome::Existing(record) => Ok(ScoreImprovementOptInResult {
                record,
                created: false,
            }),
            CompletionWorkflowCreateOutcome::Conflict => self
                .workflows
                .find_owned_score_improvement_workflow(command.owner_id, command.task_id)
                .await?
                .map(|record| ScoreImprovementOptInResult {
                    record,
                    created: false,
                })
                .ok_or(ScoreImprovementOptInError::WorkflowConflict),
        }
    }
}

fn build_workflow(
    strict: &StrictCompletionWorkflowRecord,
    history: &AnswerHistoryTaskFact,
    command: OptInScoreImprovementCommand,
) -> Result<ScoreImprovementWorkflow, ScoreImprovementOptInError> {
    let outcome = verified_outcome(strict)?;
    if history.owner_user_id != command.owner_id
        || history.task_id != command.task_id
        || history.provider_account_id != strict.workflow.binding.provider_account_id
        || history.observed_at < strict.workflow.updated_at
        || history.observed_at > command.at
    {
        return Err(ScoreImprovementOptInError::HistoryFactsStaleOrMismatched);
    }
    let policy = bounded_policy(
        strict.workflow.policy.clone(),
        history.score,
        history.retake.as_ref(),
        command.at,
    )?;
    let authority = history
        .retake
        .as_ref()
        .map(|retake| ScoreImprovementRetakeAuthority {
            answer_history_import_id: history.import_id,
            result_digest: history.result_digest,
            allowed: retake.allowed,
            remaining_attempts: retake.remaining_attempts,
            closes_at: retake.closes_at,
            observed_at: history.observed_at,
        });
    ScoreImprovementWorkflow::new_with_authority(
        CompletionWorkflowBinding {
            owner_user_id: command.owner_id,
            provider_account_id: history.provider_account_id,
            task_id: command.task_id,
        },
        policy,
        VerifiedCompletionBaseline {
            outcome,
            score: history.score,
            verified_at: strict.workflow.updated_at,
        },
        history
            .retake
            .as_ref()
            .map_or(RetakeScorePolicy::Unknown, |retake| retake.score_policy),
        authority,
        true,
        command.at,
    )
    .map_err(|_| ScoreImprovementOptInError::InvalidWorkflow)
}

fn verified_outcome(
    strict: &StrictCompletionWorkflowRecord,
) -> Result<CompletionOutcome, ScoreImprovementOptInError> {
    if strict.workflow.state != StrictCompletionState::Completed {
        return Err(ScoreImprovementOptInError::CompletionBaselineNotVerified);
    }
    strict
        .workflow
        .verified_outcome
        .ok_or(ScoreImprovementOptInError::CompletionBaselineNotVerified)
}

fn bounded_policy(
    mut policy: CompletionPolicySnapshot,
    score: Option<SubmissionScore>,
    retake: Option<&AnswerHistoryRetakeFacts>,
    at: Timestamp,
) -> Result<CompletionPolicySnapshot, ScoreImprovementOptInError> {
    if !policy.score_improvement_enabled {
        return Err(ScoreImprovementOptInError::PolicyDisabled);
    }
    if policy
        .score_improvement_expires_at
        .is_some_and(|expires_at| at >= expires_at)
    {
        return Err(ScoreImprovementOptInError::PolicyExpired);
    }
    let target_reached =
        score.is_some_and(|score| score_millis(score) >= u128::from(policy.score_target_millis));
    if !target_reached && !retake.is_some_and(|retake| retake_is_available(retake, at)) {
        return Err(ScoreImprovementOptInError::RetakeUnavailable);
    }
    if let Some(retake) = retake {
        if let Some(remaining) = retake.remaining_attempts.filter(|remaining| *remaining > 0) {
            policy.score_improvement_attempt_limit =
                policy.score_improvement_attempt_limit.min(remaining);
        }
        if let Some(closes_at) = retake.closes_at {
            policy.score_improvement_expires_at = Some(
                policy
                    .score_improvement_expires_at
                    .map_or(closes_at, |expires_at| expires_at.min(closes_at)),
            );
        }
    }
    Ok(policy)
}

fn retake_is_available(retake: &AnswerHistoryRetakeFacts, at: Timestamp) -> bool {
    retake.allowed
        && retake.remaining_attempts != Some(0)
        && retake.closes_at.is_none_or(|closes_at| at < closes_at)
}

fn score_millis(score: SubmissionScore) -> u128 {
    u128::from(score.earned_milli_points) * 1_000 / u128::from(score.possible_milli_points)
}

#[derive(Debug, thiserror::Error)]
pub enum ScoreImprovementOptInError {
    #[error("the Task has no verified Completed or Passed baseline")]
    CompletionBaselineNotVerified,
    #[error("the Task has no exact answer-history result facts")]
    HistoryFactsUnavailable,
    #[error("answer-history result facts are stale or cross-bound")]
    HistoryFactsStaleOrMismatched,
    #[error("the frozen policy disables Score Improvement")]
    PolicyDisabled,
    #[error("the frozen Score Improvement policy has expired")]
    PolicyExpired,
    #[error("the latest Provider result does not authorize an available retake")]
    RetakeUnavailable,
    #[error("the Score Improvement workflow is invalid")]
    InvalidWorkflow,
    #[error("a conflicting Score Improvement workflow already exists")]
    WorkflowConflict,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        CompletionOutcome, CompletionPolicySnapshot, ProviderAccountId, ProviderId,
        RetakeScorePolicy, ScoreImprovementState, StrictCompletionWorkflow,
    };
    use asterism_provider_api::AnswerHistoryRetakeFacts;
    use asterism_storage::{
        AnswerHistoryIngestOutcome, AnswerHistoryIngestRequest, AnswerHistoryTaskFact,
        ScoreImprovementBeginRequest, ScoreImprovementObserveRequest, StrictCompletionBeginRequest,
        StrictCompletionExecutionObservationRecord, StrictCompletionExecutionObservationRequest,
        StrictCompletionObserveRequest, StrictCompletionWorkflowRecord,
    };
    use async_trait::async_trait;
    use chrono::{Duration, Utc};

    use super::*;

    struct FakeWorkflows {
        strict: StrictCompletionWorkflowRecord,
        score: Mutex<Option<ScoreImprovementWorkflowRecord>>,
    }

    #[async_trait]
    impl CompletionWorkflowRepository for FakeWorkflows {
        async fn create_strict_completion_workflow(
            &self,
            _workflow: &StrictCompletionWorkflow,
        ) -> Result<CompletionWorkflowCreateOutcome<StrictCompletionWorkflowRecord>, StorageError>
        {
            unreachable!()
        }

        async fn find_owned_strict_completion_workflow(
            &self,
            owner_user_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<StrictCompletionWorkflowRecord>, StorageError> {
            Ok((self.strict.workflow.binding.owner_user_id == owner_user_id
                && self.strict.workflow.binding.task_id == task_id)
                .then(|| self.strict.clone()))
        }

        async fn begin_strict_completion_attempt(
            &self,
            _request: StrictCompletionBeginRequest,
        ) -> Result<StrictCompletionWorkflowRecord, StorageError> {
            unreachable!()
        }

        async fn observe_strict_completion(
            &self,
            _request: StrictCompletionObserveRequest,
        ) -> Result<StrictCompletionWorkflowRecord, StorageError> {
            unreachable!()
        }

        async fn record_strict_completion_execution_observation(
            &self,
            _request: StrictCompletionExecutionObservationRequest<'_>,
        ) -> Result<StrictCompletionExecutionObservationRecord, StorageError> {
            unreachable!()
        }

        async fn create_score_improvement_workflow(
            &self,
            workflow: &ScoreImprovementWorkflow,
        ) -> Result<CompletionWorkflowCreateOutcome<ScoreImprovementWorkflowRecord>, StorageError>
        {
            let mut score = self.score.lock().unwrap();
            if let Some(existing) = score.as_ref() {
                return Ok(CompletionWorkflowCreateOutcome::Existing(existing.clone()));
            }
            let record = ScoreImprovementWorkflowRecord {
                workflow: workflow.clone(),
                revision: 1,
            };
            *score = Some(record.clone());
            Ok(CompletionWorkflowCreateOutcome::Created(record))
        }

        async fn find_owned_score_improvement_workflow(
            &self,
            owner_user_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<ScoreImprovementWorkflowRecord>, StorageError> {
            Ok(self.score.lock().unwrap().as_ref().and_then(|record| {
                (record.workflow.binding.owner_user_id == owner_user_id
                    && record.workflow.binding.task_id == task_id)
                    .then(|| record.clone())
            }))
        }

        async fn begin_score_improvement_attempt(
            &self,
            _request: ScoreImprovementBeginRequest,
        ) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
            unreachable!()
        }

        async fn observe_score_improvement(
            &self,
            _request: ScoreImprovementObserveRequest,
        ) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
            unreachable!()
        }
    }

    struct FakeHistory(AnswerHistoryTaskFact);

    #[async_trait]
    impl AnswerHistoryIngestionRepository for FakeHistory {
        async fn ingest_answer_history_task(
            &self,
            _request: AnswerHistoryIngestRequest<'_>,
        ) -> Result<AnswerHistoryIngestOutcome, StorageError> {
            unreachable!()
        }

        async fn find_latest_owned_answer_history_task_fact(
            &self,
            owner_user_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<AnswerHistoryTaskFact>, StorageError> {
            Ok(
                (self.0.owner_user_id == owner_user_id && self.0.task_id == task_id)
                    .then(|| self.0.clone()),
            )
        }
    }

    #[tokio::test]
    async fn opt_in_freezes_exact_result_and_tightens_remote_bounds() {
        let now = Utc::now();
        let owner_id = UserId::new();
        let task_id = TaskId::new();
        let account_id = ProviderAccountId::new();
        let mut strict = StrictCompletionWorkflow::new(
            CompletionWorkflowBinding {
                owner_user_id: owner_id,
                provider_account_id: account_id,
                task_id,
            },
            CompletionPolicySnapshot {
                score_improvement_attempt_limit: 3,
                score_target_millis: 900,
                captured_at: now,
                score_improvement_expires_at: Some(now + Duration::days(1)),
                ..CompletionPolicySnapshot::default()
            },
            None,
            now,
        )
        .unwrap();
        strict
            .observe_verified_completion(CompletionOutcome::Passed, now + Duration::seconds(1))
            .unwrap();
        let import_id = asterism_domain::AnswerHistoryImportId::new();
        let closes_at = now + Duration::hours(1);
        let history = AnswerHistoryTaskFact {
            import_id,
            owner_user_id: owner_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_account_id: account_id,
            task_id,
            provider_attempt_digest: [3; 32],
            result_digest: [4; 32],
            score: Some(SubmissionScore {
                earned_milli_points: 80,
                possible_milli_points: 100,
            }),
            retake: Some(AnswerHistoryRetakeFacts {
                allowed: true,
                remaining_attempts: Some(1),
                closes_at: Some(closes_at),
                score_policy: RetakeScorePolicy::HighestScore,
                metadata_sanitized: serde_json::json!({"entry": "redo"}),
            }),
            provenance_sanitized: serde_json::json!({"surface": "result"}),
            observed_at: now + Duration::seconds(2),
            imported_at: now + Duration::seconds(2),
        };
        let service = ScoreImprovementOptInService::new(
            FakeWorkflows {
                strict: StrictCompletionWorkflowRecord {
                    workflow: strict,
                    revision: 2,
                },
                score: Mutex::new(None),
            },
            FakeHistory(history),
        );
        let command = OptInScoreImprovementCommand {
            owner_id,
            task_id,
            at: now + Duration::seconds(3),
        };

        let created = service.opt_in(command).await.unwrap();
        assert!(created.created);
        assert_eq!(created.record.workflow.state, ScoreImprovementState::Ready);
        assert_eq!(
            created
                .record
                .workflow
                .policy
                .score_improvement_attempt_limit,
            1
        );
        assert_eq!(
            created.record.workflow.policy.score_improvement_expires_at,
            Some(closes_at)
        );
        let authority = created.record.workflow.retake_authority.unwrap();
        assert_eq!(authority.answer_history_import_id, import_id);
        assert_eq!(authority.result_digest, [4; 32]);

        let replay = service.opt_in(command).await.unwrap();
        assert!(!replay.created);
        assert_eq!(replay.record, created.record);
    }

    #[tokio::test]
    async fn opt_in_rejects_history_older_than_verified_completion() {
        let now = Utc::now();
        let owner_id = UserId::new();
        let task_id = TaskId::new();
        let account_id = ProviderAccountId::new();
        let mut strict = StrictCompletionWorkflow::new(
            CompletionWorkflowBinding {
                owner_user_id: owner_id,
                provider_account_id: account_id,
                task_id,
            },
            CompletionPolicySnapshot {
                captured_at: now,
                ..CompletionPolicySnapshot::default()
            },
            None,
            now,
        )
        .unwrap();
        strict
            .observe_verified_completion(CompletionOutcome::Completed, now + Duration::seconds(2))
            .unwrap();
        let history = AnswerHistoryTaskFact {
            import_id: asterism_domain::AnswerHistoryImportId::new(),
            owner_user_id: owner_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_account_id: account_id,
            task_id,
            provider_attempt_digest: [1; 32],
            result_digest: [2; 32],
            score: None,
            retake: None,
            provenance_sanitized: serde_json::json!({}),
            observed_at: now + Duration::seconds(1),
            imported_at: now + Duration::seconds(1),
        };
        let service = ScoreImprovementOptInService::new(
            FakeWorkflows {
                strict: StrictCompletionWorkflowRecord {
                    workflow: strict,
                    revision: 2,
                },
                score: Mutex::new(None),
            },
            FakeHistory(history),
        );

        assert!(matches!(
            service
                .opt_in(OptInScoreImprovementCommand {
                    owner_id,
                    task_id,
                    at: now + Duration::seconds(3),
                })
                .await,
            Err(ScoreImprovementOptInError::HistoryFactsStaleOrMismatched)
        ));
    }
}
