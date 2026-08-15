use std::{sync::Arc, time::Duration as StdDuration};

use asterism_domain::Timestamp;
use asterism_provider_api::ProviderRegistry;
use asterism_storage::{
    CompletionWorkflowRepository, ExecutionAtomicMutationRepository,
    ExecutionCapabilityStepRepository, ExecutionLeaseRepository, ExecutionRepository,
    ExecutionVerificationRecoveryRepository, ProviderAccountRuntimeRepository,
    QuestionSessionArtifactRepositoryFactory, SchedulerRepository, StorageError,
    TaskRuntimeRepository,
};
use futures_util::{StreamExt as _, stream};

use crate::{
    ExecutionRunnerConfig, ScheduledExecutionOutcome, ScheduledExecutionRunError,
    ScheduledExecutionRunner,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionSchedulerConfig {
    pub worker_id: String,
    pub claim_limit: u32,
    pub claim_ttl: StdDuration,
    pub runner: ExecutionRunnerConfig,
}

impl ExecutionSchedulerConfig {
    fn validate(self) -> Result<Self, ExecutionSchedulerWorkerError> {
        if self.worker_id.is_empty()
            || self.worker_id.len() > 128
            || self.worker_id.trim() != self.worker_id
            || self.worker_id.chars().any(char::is_control)
            || self.claim_limit == 0
            || self.claim_limit > self.runner.global_concurrency_limit
            || self.claim_ttl.is_zero()
        {
            return Err(ExecutionSchedulerWorkerError::InvalidConfig);
        }
        chrono::Duration::from_std(self.claim_ttl)
            .map_err(|_| ExecutionSchedulerWorkerError::ClaimTimeOverflow)?;
        Ok(self)
    }
}

#[derive(Debug)]
pub struct ExecutionSchedulerWorker<E, L, S, A, T> {
    scheduler: S,
    runner: ScheduledExecutionRunner<E, L, S, A, T>,
    config: ExecutionSchedulerConfig,
}

impl<E, L, S, A, T> ExecutionSchedulerWorker<E, L, S, A, T>
where
    S: Clone,
{
    /// Builds one worker which only claims Execution, Retry and Recovery jobs.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionSchedulerWorkerError`] for unsafe claim or runner
    /// timing and retry configuration.
    pub fn new(
        registry: Arc<ProviderRegistry>,
        executions: E,
        leases: L,
        scheduler: S,
        accounts: A,
        tasks: T,
        config: ExecutionSchedulerConfig,
    ) -> Result<Self, ExecutionSchedulerWorkerError> {
        let config = config.validate()?;
        let runner = ScheduledExecutionRunner::new(
            registry,
            executions,
            leases,
            scheduler.clone(),
            accounts,
            tasks,
            config.runner,
        )?;
        Ok(Self {
            scheduler,
            runner,
            config,
        })
    }

    #[must_use]
    pub fn with_question_session_artifacts(
        mut self,
        artifacts: Arc<dyn QuestionSessionArtifactRepositoryFactory>,
    ) -> Self {
        self.runner = self.runner.with_question_session_artifacts(artifacts);
        self
    }
}

impl<E, L, S, A, T> ExecutionSchedulerWorker<E, L, S, A, T>
where
    E: ExecutionRepository
        + ExecutionAtomicMutationRepository
        + ExecutionCapabilityStepRepository
        + asterism_storage::ExecutionSubmissionRepository
        + ExecutionVerificationRecoveryRepository
        + CompletionWorkflowRepository,
    L: ExecutionLeaseRepository,
    S: Clone + SchedulerRepository,
    A: ProviderAccountRuntimeRepository,
    T: TaskRuntimeRepository,
{
    /// Claims and runs one bounded batch of due Execution, Retry and Recovery jobs.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionSchedulerWorkerError`] when claim persistence,
    /// ownership, execution dispatch, or finalization fails.
    pub async fn tick_once(
        &self,
        now: Timestamp,
    ) -> Result<ExecutionSchedulerTickReport, ExecutionSchedulerWorkerError> {
        let lease_expires_at = now
            .checked_add_signed(
                chrono::Duration::from_std(self.config.claim_ttl)
                    .map_err(|_| ExecutionSchedulerWorkerError::ClaimTimeOverflow)?,
            )
            .ok_or(ExecutionSchedulerWorkerError::ClaimTimeOverflow)?;
        let jobs = self
            .scheduler
            .claim_due_execution_jobs(
                &self.config.worker_id,
                now,
                lease_expires_at,
                self.config.claim_limit,
            )
            .await?;
        let mut report = ExecutionSchedulerTickReport {
            claimed: jobs.len(),
            ..ExecutionSchedulerTickReport::default()
        };
        let runner = &self.runner;
        let outcomes = stream::iter(jobs)
            .map(|job| async move { runner.run_claimed(&job, now).await })
            .buffer_unordered(self.config.claim_limit as usize)
            .collect::<Vec<_>>()
            .await;
        for outcome in outcomes {
            match outcome? {
                ScheduledExecutionOutcome::Succeeded(_) => report.succeeded += 1,
                ScheduledExecutionOutcome::RecoveryScheduled(_) => report.recovery_scheduled += 1,
                ScheduledExecutionOutcome::RetryScheduled { .. } => report.retry_scheduled += 1,
                ScheduledExecutionOutcome::HumanRequired { .. } => report.human_required += 1,
                ScheduledExecutionOutcome::Failed { .. } => report.failed += 1,
                ScheduledExecutionOutcome::Deferred { .. } => report.deferred += 1,
                ScheduledExecutionOutcome::LeaseBusyDeadLetter => report.dead_lettered += 1,
                ScheduledExecutionOutcome::AlreadyTerminal(_) => report.already_terminal += 1,
            }
        }
        Ok(report)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutionSchedulerTickReport {
    pub claimed: usize,
    pub succeeded: usize,
    pub recovery_scheduled: usize,
    pub retry_scheduled: usize,
    pub human_required: usize,
    pub failed: usize,
    pub deferred: usize,
    pub dead_lettered: usize,
    pub already_terminal: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionSchedulerWorkerError {
    #[error("execution scheduler worker configuration is invalid")]
    InvalidConfig,
    #[error("execution scheduler claim expiry is outside the supported clock range")]
    ClaimTimeOverflow,
    #[error(transparent)]
    Runner(#[from] ScheduledExecutionRunError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AuditActor, Execution, ExecutionId, ExecutionState, OrchestrationState, ProviderAccountId,
        RequestSource, TaskId, UserId,
    };
    use asterism_scheduler::RetryPolicy;
    use asterism_storage::{
        Database, ExecutionRepository, ExecutionScheduleRequest, SqliteExecutionLeaseRepository,
        SqliteExecutionRepository, SqliteProviderAccountRepository, SqliteSchedulerRepository,
        SqliteTaskQueryRepository,
    };
    use chrono::{SecondsFormat, Utc};

    use super::*;
    use crate::FormalAssessmentPolicy;

    #[tokio::test]
    async fn tick_claims_and_closes_an_already_terminal_execution_job() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let (owner_id, task_id) = insert_task(&database, now).await;
        let executions = SqliteExecutionRepository::new(database.clone());
        let execution = Execution {
            id: ExecutionId::new(),
            task_id,
            requested_capabilities: vec![asterism_domain::TaskCapability::ResourceExecution],
            submission_draft_id: None,
            requested_by: Some(owner_id),
            request_source: RequestSource::Scheduler,
            quote_id: None,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        };
        executions
            .schedule_execution(ExecutionScheduleRequest {
                execution: &execution,
                capability_plan: &execution.requested_capabilities,
                capability_call_starts: &[1],
                provider_plan_artifact: None,
                billing: None,
                runtime_settings: None,
                expected_task_state: OrchestrationState::Ready,
                idempotency_scope: "test:execution-worker",
                idempotency_key: "terminal-execution",
                actor: AuditActor::User(owner_id),
                correlation_id: "execution-worker-test",
            })
            .await
            .unwrap();
        sqlx::query("UPDATE executions SET state = 'succeeded', finished_at = ? WHERE id = ?")
            .bind(now.to_rfc3339_opts(SecondsFormat::Nanos, true))
            .bind(execution.id.to_string())
            .execute(database.pool())
            .await
            .unwrap();

        let worker = ExecutionSchedulerWorker::new(
            Arc::new(ProviderRegistry::default()),
            executions,
            SqliteExecutionLeaseRepository::new(database.clone()),
            SqliteSchedulerRepository::new(database.clone()),
            SqliteProviderAccountRepository::new(database.clone()),
            SqliteTaskQueryRepository::new(database.clone()),
            config(),
        )
        .unwrap();
        assert_eq!(
            worker.tick_once(now).await.unwrap(),
            ExecutionSchedulerTickReport {
                claimed: 1,
                already_terminal: 1,
                ..ExecutionSchedulerTickReport::default()
            }
        );
        let state: String = sqlx::query_scalar("SELECT state FROM scheduled_jobs")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(state, "completed");
    }

    #[test]
    fn worker_rejects_claims_above_the_global_execution_limit() {
        let mut config = config();
        config.claim_limit = 2;
        assert!(matches!(
            ExecutionSchedulerWorker::new(
                Arc::new(ProviderRegistry::default()),
                (),
                (),
                (),
                (),
                (),
                config,
            ),
            Err(ExecutionSchedulerWorkerError::InvalidConfig)
        ));
    }

    #[test]
    fn worker_accepts_bounded_parallel_claims() {
        let mut config = config();
        config.claim_limit = 4;
        config.runner.global_concurrency_limit = 4;
        assert!(
            ExecutionSchedulerWorker::new(
                Arc::new(ProviderRegistry::default()),
                (),
                (),
                (),
                (),
                (),
                config,
            )
            .is_ok()
        );
    }

    fn config() -> ExecutionSchedulerConfig {
        ExecutionSchedulerConfig {
            worker_id: "execution-worker".to_owned(),
            claim_limit: 1,
            claim_ttl: StdDuration::from_mins(1),
            runner: ExecutionRunnerConfig {
                execution_lease_ttl: StdDuration::from_mins(1),
                heartbeat_interval: StdDuration::from_secs(10),
                global_concurrency_limit: 1,
                retry_policy: RetryPolicy {
                    max_attempts: 3,
                    initial_delay_seconds: 10,
                    multiplier: 2,
                    max_delay_seconds: 60,
                },
                formal_assessment_policy: FormalAssessmentPolicy::default(),
            },
        }
    }

    async fn insert_task(database: &Database, now: Timestamp) -> (UserId, TaskId) {
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let timestamp = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'provider-alpha', 'primary', '{\"state\":\"idle\"}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'remote-task', 'fingerprint', 'resource', 'routine', 'Task', \
                     'pending', 'ready', ?, ?, '[\"resource_execution\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        (owner_id, task_id)
    }
}
