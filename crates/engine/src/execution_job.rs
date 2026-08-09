use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration as StdDuration,
};

use asterism_domain::{
    AttemptResult, AuthState, Execution, ExecutionAttempt, ExecutionId, ExecutionLease,
    ExecutionProgress, ExecutionStage, ExecutionState, OrchestrationState, ProviderErrorClass,
    RemoteState, Task, TaskCapability, Timestamp,
};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionRequest as ProviderExecutionRequest, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderExecutionLog, ProviderProgress, ProviderRegistry,
    TaskExecutionCapability, TaskProgressCapability,
};
use asterism_scheduler::{
    RetryPolicy, RetryPolicyError, ScheduledJob, ScheduledJobKind, ScheduledJobState,
};
use asterism_storage::{
    ExecutionAttemptFinishRequest, ExecutionAttemptStartRequest, ExecutionLeaseRepository,
    ExecutionLogAppendRequest, ExecutionProgressUpdate, ExecutionRecoveryFinishRequest,
    ExecutionRepository, LeaseAcquireOutcome, ProviderAccountRuntimeRepository,
    SchedulerRepository, StorageError, TaskRuntimeRepository,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{FormalAssessmentPolicy, TaskAction, authorize_task_action};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionRunnerConfig {
    pub execution_lease_ttl: StdDuration,
    pub heartbeat_interval: StdDuration,
    pub retry_policy: RetryPolicy,
    pub formal_assessment_policy: FormalAssessmentPolicy,
}

impl ExecutionRunnerConfig {
    /// Validates bounded lease, heartbeat and retry settings.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduledExecutionRunError`] when a duration is zero,
    /// unrepresentable, or cannot renew before the lease expires.
    pub fn validate(self) -> Result<Self, ScheduledExecutionRunError> {
        self.retry_policy.validate()?;
        if self.execution_lease_ttl.is_zero()
            || self.heartbeat_interval.is_zero()
            || self.heartbeat_interval >= self.execution_lease_ttl
            || i64::try_from(self.execution_lease_ttl.as_secs()).is_err()
            || i64::try_from(self.heartbeat_interval.as_secs()).is_err()
            || i64::try_from(self.retry_policy.max_delay_seconds).is_err()
        {
            return Err(ScheduledExecutionRunError::InvalidConfiguration);
        }
        Ok(self)
    }
}

#[derive(Debug)]
pub struct ScheduledExecutionRunner<E, L, S, A, T> {
    registry: Arc<ProviderRegistry>,
    executions: E,
    leases: L,
    scheduler: S,
    accounts: A,
    tasks: T,
    config: ExecutionRunnerConfig,
}

impl<E, L, S, A, T> ScheduledExecutionRunner<E, L, S, A, T> {
    /// Builds one shared execution runner.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduledExecutionRunError`] when worker timing or retry
    /// configuration is unsafe.
    pub fn new(
        registry: Arc<ProviderRegistry>,
        executions: E,
        leases: L,
        scheduler: S,
        accounts: A,
        tasks: T,
        config: ExecutionRunnerConfig,
    ) -> Result<Self, ScheduledExecutionRunError> {
        Ok(Self {
            registry,
            executions,
            leases,
            scheduler,
            accounts,
            tasks,
            config: config.validate()?,
        })
    }
}

impl<E, L, S, A, T> ScheduledExecutionRunner<E, L, S, A, T>
where
    E: ExecutionRepository,
    L: ExecutionLeaseRepository,
    S: SchedulerRepository,
    A: ProviderAccountRuntimeRepository,
    T: TaskRuntimeRepository,
{
    /// Runs one already claimed Execution or Retry job through the registered
    /// Provider capability and the transactional execution repository.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduledExecutionRunError`] when the job is not a live
    /// execution claim, persistence is unavailable, or lease ownership is
    /// lost while remote completion is uncertain.
    pub async fn run_claimed(
        &self,
        job: &ScheduledJob,
        now: Timestamp,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let claim = claimed_execution(job, now)?;
        let execution = self
            .executions
            .find_execution(claim.execution_id)
            .await?
            .ok_or(ScheduledExecutionRunError::ExecutionMissing(
                claim.execution_id,
            ))?;
        if execution.state.is_terminal() {
            self.scheduler
                .complete(job.id, claim.worker_id, now)
                .await?;
            return Ok(ScheduledExecutionOutcome::AlreadyTerminal(execution));
        }
        let task = self
            .tasks
            .find_runtime_task(execution.task_id)
            .await?
            .ok_or(ScheduledExecutionRunError::TaskMissing(execution.task_id))?;
        validate_execution_binding(&execution, &task, claim.recovery)?;

        let lease = ExecutionLease {
            task_id: task.id,
            execution_id: execution.id,
            worker_id: claim.worker_id.to_owned(),
            expires_at: add_duration(now, self.config.execution_lease_ttl)?,
        };
        match self.leases.try_acquire(&lease, now).await? {
            LeaseAcquireOutcome::Acquired(_) => {}
            LeaseAcquireOutcome::Conflict(_) => {
                let delay = self
                    .config
                    .retry_policy
                    .delay_after(job.attempts.saturating_add(1))?;
                let retry_at = delay.map(|delay| add_duration(now, delay)).transpose()?;
                self.scheduler
                    .fail(
                        job.id,
                        claim.worker_id,
                        "execution_lease_busy",
                        retry_at,
                        now,
                    )
                    .await?;
                return Ok(match retry_at {
                    Some(retry_at) => ScheduledExecutionOutcome::Deferred { retry_at },
                    None => ScheduledExecutionOutcome::LeaseBusyDeadLetter,
                });
            }
        }
        if self
            .scheduler
            .renew_claim(job.id, claim.worker_id, now, lease.expires_at)
            .await
            .is_err()
        {
            let _ = self
                .leases
                .release(task.id, execution.id, claim.worker_id)
                .await;
            return Err(ScheduledExecutionRunError::ClaimLost);
        }

        let correlation_id = format!("scheduled-execution:{}", job.id);
        if claim.recovery {
            return self
                .recover_execution(job, &task, now, &correlation_id)
                .await;
        }
        let attempt = self
            .executions
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job.id,
                worker_id: claim.worker_id,
                at: now,
                correlation_id: &correlation_id,
            })
            .await?;
        if claim
            .next_attempt_no
            .is_some_and(|expected| expected != attempt.attempt_no)
        {
            return self
                .finish_failure(
                    job,
                    &attempt,
                    ProviderErrorClass::Internal,
                    FailureDisposition::Failed,
                    now,
                    &correlation_id,
                )
                .await;
        }
        self.execute_attempt(job, &task, &attempt, now, &correlation_id)
            .await
    }

    async fn recover_execution(
        &self,
        job: &ScheduledJob,
        task: &Task,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let prepared = match self.prepare_recovery_call(task, correlation_id).await? {
            Ok(prepared) => prepared,
            Err(error_class) => {
                return self
                    .finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(error_class),
                        None,
                        now,
                        correlation_id,
                    )
                    .await;
            }
        };
        let provider = prepared
            .capability
            .read_progress(&prepared.context, &task.remote_id);
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => {
                    self.renew_claims(job, claimed_execution_id(job)?).await?;
                }
            }
        };
        let finished_at = Utc::now().max(now);
        match result {
            Ok(progress) => {
                self.finish_from_remote_progress(
                    job,
                    progress.remote_state,
                    finished_at,
                    correlation_id,
                )
                .await
            }
            Err(error) => {
                self.finish_from_recovery_error(job, &error, finished_at, correlation_id)
                    .await
            }
        }
    }

    async fn prepare_recovery_call(
        &self,
        task: &Task,
        correlation_id: &str,
    ) -> Result<Result<PreparedRecoveryCall, ProviderErrorClass>, ScheduledExecutionRunError> {
        if !task.capabilities.contains(&TaskCapability::ProgressRead) {
            return Ok(Err(ProviderErrorClass::UnsupportedTask));
        }
        let Some(account) = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
        else {
            return Ok(Err(ProviderErrorClass::Internal));
        };
        if account.auth_state != AuthState::Authenticated {
            return Ok(Err(ProviderErrorClass::Authentication));
        }
        let Some(capability) = self
            .registry
            .get(&account.provider_id)
            .and_then(|entry| entry.task_progress.clone())
        else {
            return Ok(Err(ProviderErrorClass::UnsupportedTask));
        };
        Ok(Ok(PreparedRecoveryCall {
            capability,
            context: ProviderContext {
                provider_id: account.provider_id,
                account_id: account.id,
                credential_refs: account.credential_refs,
                correlation_id: correlation_id.to_owned(),
            },
        }))
    }

    async fn finish_from_remote_progress(
        &self,
        job: &ScheduledJob,
        remote_state: RemoteState,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        match remote_state {
            RemoteState::Completed => {
                self.finish_recovery(
                    job,
                    ExecutionState::Succeeded,
                    None,
                    None,
                    at,
                    correlation_id,
                )
                .await
            }
            RemoteState::Pending => {
                if let Some(retry_at) = self.recovery_retry_at(job, at)? {
                    self.finish_recovery(
                        job,
                        ExecutionState::RetryWaiting,
                        Some(ProviderErrorClass::InvalidRemoteState),
                        Some(retry_at),
                        at,
                        correlation_id,
                    )
                    .await
                } else {
                    self.finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(ProviderErrorClass::InvalidRemoteState),
                        None,
                        at,
                        correlation_id,
                    )
                    .await
                }
            }
            RemoteState::InProgress => {
                if let Some(retry_at) = self.recovery_retry_at(job, at)? {
                    self.defer_recovery(job, retry_at, at).await
                } else {
                    self.finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(ProviderErrorClass::InvalidRemoteState),
                        None,
                        at,
                        correlation_id,
                    )
                    .await
                }
            }
            RemoteState::Unknown
            | RemoteState::NotOpen
            | RemoteState::Expired
            | RemoteState::Removed => {
                self.finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::InvalidRemoteState),
                    None,
                    at,
                    correlation_id,
                )
                .await
            }
        }
    }

    async fn finish_from_recovery_error(
        &self,
        job: &ScheduledJob,
        error: &ProviderError,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let error_class = provider_error_class(error);
        if matches!(
            error.kind,
            ProviderErrorKind::RateLimited
                | ProviderErrorKind::Network
                | ProviderErrorKind::ProviderUnavailable
        ) && let Some(retry_at) = self.recovery_retry_at_with_provider(job, error, at)?
        {
            return self.defer_recovery(job, retry_at, at).await;
        }
        self.finish_recovery(
            job,
            ExecutionState::HumanRequired,
            Some(error_class),
            None,
            at,
            correlation_id,
        )
        .await
    }

    fn recovery_retry_at(
        &self,
        job: &ScheduledJob,
        at: Timestamp,
    ) -> Result<Option<Timestamp>, ScheduledExecutionRunError> {
        self.config
            .retry_policy
            .delay_after(job.attempts.saturating_add(1))?
            .map(|delay| add_duration(at, delay))
            .transpose()
    }

    fn recovery_retry_at_with_provider(
        &self,
        job: &ScheduledJob,
        error: &ProviderError,
        at: Timestamp,
    ) -> Result<Option<Timestamp>, ScheduledExecutionRunError> {
        let Some(policy_delay) = self
            .config
            .retry_policy
            .delay_after(job.attempts.saturating_add(1))?
        else {
            return Ok(None);
        };
        let provider_delay = error
            .retry_after_seconds
            .unwrap_or_default()
            .min(self.config.retry_policy.max_delay_seconds);
        add_duration(at, policy_delay.max(StdDuration::from_secs(provider_delay))).map(Some)
    }

    async fn defer_recovery(
        &self,
        job: &ScheduledJob,
        retry_at: Timestamp,
        at: Timestamp,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let worker_id = claimed_worker(job)?;
        self.scheduler
            .fail(
                job.id,
                worker_id,
                "execution_recovery_verification_deferred",
                Some(retry_at),
                at,
            )
            .await?;
        let execution_id = claimed_execution_id(job)?;
        let task_id = execution_id_to_task(&self.executions, execution_id).await?;
        let _ = self
            .leases
            .release(task_id, execution_id, worker_id)
            .await?;
        Ok(ScheduledExecutionOutcome::Deferred { retry_at })
    }

    async fn finish_recovery(
        &self,
        job: &ScheduledJob,
        final_state: ExecutionState,
        error_class: Option<ProviderErrorClass>,
        retry_at: Option<Timestamp>,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let (percent, stage, status_text) = match final_state {
            ExecutionState::Succeeded => (
                Some(100),
                ExecutionStage::Completed,
                "remote completion verified during recovery",
            ),
            ExecutionState::RetryWaiting => (
                None,
                ExecutionStage::Finalizing,
                "remote state confirmed pending; execution retry scheduled",
            ),
            ExecutionState::HumanRequired => (
                None,
                ExecutionStage::Finalizing,
                "remote outcome remains uncertain; human action required",
            ),
            _ => return Err(ScheduledExecutionRunError::StateConflict),
        };
        let execution_id = claimed_execution_id(job)?;
        let progress = ExecutionProgress {
            execution_id,
            percent,
            stage,
            status_text: Some(status_text.to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        };
        let execution = self
            .executions
            .finish_recovery(ExecutionRecoveryFinishRequest {
                execution_id,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                final_state,
                error_class,
                provider_trace_id: None,
                retry_at,
                progress: &progress,
                at,
                correlation_id,
            })
            .await?;
        Ok(match final_state {
            ExecutionState::Succeeded => ScheduledExecutionOutcome::Succeeded(execution),
            ExecutionState::RetryWaiting => ScheduledExecutionOutcome::RetryScheduled {
                execution,
                error_class: error_class.expect("recovery retry has an error class"),
                retry_at: retry_at.expect("recovery retry has a timestamp"),
            },
            ExecutionState::HumanRequired => ScheduledExecutionOutcome::HumanRequired {
                execution,
                error_class: error_class.expect("human recovery has an error class"),
            },
            _ => unreachable!("recovery finish states were validated"),
        })
    }

    async fn execute_attempt(
        &self,
        job: &ScheduledJob,
        task: &Task,
        attempt: &ExecutionAttempt,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if task.remote_state == RemoteState::Completed {
            return self.finish_success(job, attempt, now, correlation_id).await;
        }
        if !matches!(
            task.remote_state,
            RemoteState::Pending | RemoteState::InProgress
        ) {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    FailureDisposition::Failed,
                    now,
                    correlation_id,
                )
                .await;
        }
        let prepared = match self
            .prepare_provider_call(attempt.execution_id, task, correlation_id)
            .await?
        {
            Ok(prepared) => prepared,
            Err(failure) => {
                return self
                    .finish_failure(
                        job,
                        attempt,
                        failure.error_class,
                        failure.disposition,
                        now,
                        correlation_id,
                    )
                    .await;
            }
        };
        self.call_provider(job, attempt, &prepared, correlation_id)
            .await
    }

    async fn prepare_provider_call(
        &self,
        execution_id: ExecutionId,
        task: &Task,
        correlation_id: &str,
    ) -> Result<Result<PreparedProviderCall, PreparedFailure>, ScheduledExecutionRunError> {
        if authorize_execution(task, self.config.formal_assessment_policy).is_err() {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Authorization,
                disposition: FailureDisposition::HumanRequired,
            }));
        }
        let capabilities = execution_capabilities(task);
        if capabilities.is_empty() {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::UnsupportedTask,
                disposition: FailureDisposition::Failed,
            }));
        }
        let Some(account) = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
        else {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Internal,
                disposition: FailureDisposition::Failed,
            }));
        };
        if account.auth_state != AuthState::Authenticated {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Authentication,
                disposition: FailureDisposition::HumanRequired,
            }));
        }
        let Some(entry) = self.registry.get(&account.provider_id) else {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::UnsupportedTask,
                disposition: FailureDisposition::Failed,
            }));
        };
        let Some(capability) = entry.task_execution.clone() else {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::UnsupportedTask,
                disposition: FailureDisposition::Failed,
            }));
        };
        let Some(runtime_settings) = self
            .executions
            .find_execution_runtime_settings(execution_id)
            .await?
        else {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Internal,
                disposition: FailureDisposition::Failed,
            }));
        };
        if runtime_settings.provider_id != account.provider_id
            || entry
                .runtime_settings
                .validate_resolved(&runtime_settings.resolved)
                .is_err()
        {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Internal,
                disposition: FailureDisposition::Failed,
            }));
        }
        Ok(Ok(PreparedProviderCall {
            capability,
            context: ProviderContext {
                provider_id: account.provider_id,
                account_id: account.id,
                credential_refs: account.credential_refs,
                correlation_id: correlation_id.to_owned(),
            },
            request: ProviderExecutionRequest {
                task_id: task.id,
                remote_task_id: task.remote_id.clone(),
                course_id: task.course_id,
                requested_capabilities: capabilities,
                runtime_settings: runtime_settings.resolved,
            },
        }))
    }

    async fn call_provider(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedProviderCall,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let now = attempt.started_at;
        let claim_lost = Arc::new(AtomicBool::new(false));
        let sink = PersistedExecutionEventSink {
            executions: &self.executions,
            execution_id: attempt.execution_id,
            attempt_id: attempt.id,
            worker_id: claimed_worker(job)?,
            correlation_id,
            claim_lost: Arc::clone(&claim_lost),
        };
        let provider = prepared
            .capability
            .execute(&prepared.context, &prepared.request, &sink);
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => {
                    self.renew_claims(job, attempt.execution_id).await?;
                }
            }
        };
        if claim_lost.load(Ordering::Acquire) {
            return Err(ScheduledExecutionRunError::ClaimLost);
        }
        match result {
            Ok(outcome) if outcome.verified && outcome.remote_state == RemoteState::Completed => {
                self.finish_success(job, attempt, Utc::now().max(now), correlation_id)
                    .await
            }
            Ok(_) => {
                self.finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    FailureDisposition::Failed,
                    Utc::now().max(now),
                    correlation_id,
                )
                .await
            }
            Err(error) => {
                let (error_class, disposition) = classify_provider_error(
                    &error,
                    attempt.attempt_no,
                    Utc::now().max(now),
                    self.config.retry_policy,
                )?;
                self.finish_failure(
                    job,
                    attempt,
                    error_class,
                    disposition,
                    Utc::now().max(now),
                    correlation_id,
                )
                .await
            }
        }
    }

    async fn renew_claims(
        &self,
        job: &ScheduledJob,
        execution_id: ExecutionId,
    ) -> Result<(), ScheduledExecutionRunError> {
        let now = Utc::now();
        let expires_at = add_duration(now, self.config.execution_lease_ttl)?;
        let worker_id = claimed_worker(job)?;
        self.scheduler
            .renew_claim(job.id, worker_id, now, expires_at)
            .await
            .map_err(|_| ScheduledExecutionRunError::ClaimLost)?;
        self.leases
            .renew(
                execution_id_to_task(&self.executions, execution_id).await?,
                execution_id,
                worker_id,
                now,
                expires_at,
            )
            .await
            .map_err(|_| ScheduledExecutionRunError::ClaimLost)?;
        Ok(())
    }

    async fn finish_success(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let progress = ExecutionProgress {
            execution_id: attempt.execution_id,
            percent: Some(100),
            stage: ExecutionStage::Completed,
            status_text: Some("provider completion verified".to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        };
        let execution = self
            .executions
            .finish_attempt(ExecutionAttemptFinishRequest {
                execution_id: attempt.execution_id,
                attempt_id: attempt.id,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                final_state: ExecutionState::Succeeded,
                result: AttemptResult::Succeeded,
                error_class: None,
                provider_trace_id: None,
                retry_at: None,
                progress: &progress,
                at,
                correlation_id,
            })
            .await?;
        Ok(ScheduledExecutionOutcome::Succeeded(execution))
    }

    async fn finish_failure(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        error_class: ProviderErrorClass,
        disposition: FailureDisposition,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let (state, retry_at, status) = match disposition {
            FailureDisposition::RetryAt(retry_at) => (
                ExecutionState::RetryWaiting,
                Some(retry_at),
                "execution retry scheduled",
            ),
            FailureDisposition::HumanRequired => (
                ExecutionState::HumanRequired,
                None,
                "execution requires human action",
            ),
            FailureDisposition::Failed => (ExecutionState::Failed, None, "execution failed"),
        };
        let progress = ExecutionProgress {
            execution_id: attempt.execution_id,
            percent: None,
            stage: ExecutionStage::Finalizing,
            status_text: Some(status.to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        };
        let execution = self
            .executions
            .finish_attempt(ExecutionAttemptFinishRequest {
                execution_id: attempt.execution_id,
                attempt_id: attempt.id,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                final_state: state,
                result: AttemptResult::Failed,
                error_class: Some(error_class),
                provider_trace_id: None,
                retry_at,
                progress: &progress,
                at,
                correlation_id,
            })
            .await?;
        Ok(match disposition {
            FailureDisposition::RetryAt(retry_at) => ScheduledExecutionOutcome::RetryScheduled {
                execution,
                error_class,
                retry_at,
            },
            FailureDisposition::HumanRequired => ScheduledExecutionOutcome::HumanRequired {
                execution,
                error_class,
            },
            FailureDisposition::Failed => ScheduledExecutionOutcome::Failed {
                execution,
                error_class,
            },
        })
    }
}

async fn execution_id_to_task<E: ExecutionRepository>(
    executions: &E,
    execution_id: ExecutionId,
) -> Result<asterism_domain::TaskId, ScheduledExecutionRunError> {
    executions
        .find_execution(execution_id)
        .await?
        .map(|execution| execution.task_id)
        .ok_or(ScheduledExecutionRunError::ExecutionMissing(execution_id))
}

struct PreparedProviderCall {
    capability: Arc<dyn TaskExecutionCapability>,
    context: ProviderContext,
    request: ProviderExecutionRequest,
}

struct PreparedRecoveryCall {
    capability: Arc<dyn TaskProgressCapability>,
    context: ProviderContext,
}

struct PreparedFailure {
    error_class: ProviderErrorClass,
    disposition: FailureDisposition,
}

struct PersistedExecutionEventSink<'a, E> {
    executions: &'a E,
    execution_id: ExecutionId,
    attempt_id: asterism_domain::ExecutionAttemptId,
    worker_id: &'a str,
    correlation_id: &'a str,
    claim_lost: Arc<AtomicBool>,
}

#[async_trait]
impl<E: ExecutionRepository> ExecutionEventSink for PersistedExecutionEventSink<'_, E> {
    async fn report(&self, update: ProviderProgress) -> Result<(), ProviderError> {
        let progress = ExecutionProgress {
            execution_id: self.execution_id,
            percent: update.percent,
            stage: map_provider_stage(&update.stage),
            status_text: update.status_text,
            current_item: None,
            completed_items: update.completed_items,
            total_items: update.total_items,
            updated_at: Utc::now(),
        };
        self.executions
            .update_progress(ExecutionProgressUpdate {
                progress: &progress,
                worker_id: self.worker_id,
                correlation_id: self.correlation_id,
            })
            .await
            .map(|_| ())
            .map_err(|error| {
                if matches!(error, StorageError::ExecutionClaimLost) {
                    self.claim_lost.store(true, Ordering::Release);
                }
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Core could not persist Provider progress",
                )
            })
    }

    async fn log(&self, event: ProviderExecutionLog) -> Result<(), ProviderError> {
        event.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Provider execution log was rejected by Core",
            )
        })?;
        let stage = map_provider_stage(&event.stage);
        self.executions
            .append_log(ExecutionLogAppendRequest {
                execution_id: self.execution_id,
                attempt_id: self.attempt_id,
                worker_id: self.worker_id,
                at: Utc::now(),
                level: event.level,
                stage,
                message: &event.message,
                provider_trace_id: event.provider_trace_id.as_deref(),
                metadata_sanitized: event.metadata_sanitized.as_ref(),
                correlation_id: self.correlation_id,
            })
            .await
            .map_err(|error| {
                if matches!(error, StorageError::ExecutionClaimLost) {
                    self.claim_lost.store(true, Ordering::Release);
                }
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "Core could not persist Provider execution log",
                )
            })
    }
}

fn map_provider_stage(stage: &str) -> ExecutionStage {
    let stage = stage.to_ascii_lowercase();
    if stage.contains("complete") {
        ExecutionStage::Completed
    } else if stage.contains("final") {
        ExecutionStage::Finalizing
    } else if stage.contains("verify") {
        ExecutionStage::Verifying
    } else if stage.contains("submit") {
        ExecutionStage::Submitting
    } else if stage.contains("duration") || stage.contains("report") {
        ExecutionStage::ReportingDuration
    } else if stage.contains("detail") || stage.contains("fetch") {
        ExecutionStage::FetchingDetail
    } else if stage.contains("auth") {
        ExecutionStage::Authenticating
    } else if stage.contains("scan") || stage.contains("refresh") {
        ExecutionStage::RefreshingRemoteState
    } else {
        ExecutionStage::Executing
    }
}

fn execution_capabilities(task: &Task) -> Vec<TaskCapability> {
    task.capabilities
        .iter()
        .copied()
        .filter(|capability| {
            !matches!(
                capability,
                TaskCapability::ProgressRead | TaskCapability::BrowserBridge
            )
        })
        .collect()
}

fn authorize_execution(
    task: &Task,
    policy: FormalAssessmentPolicy,
) -> Result<(), crate::AssessmentGuardError> {
    authorize_task_action(task, TaskAction::Execute, policy)?;
    if task
        .capabilities
        .contains(&TaskCapability::SubmissionExecute)
    {
        authorize_task_action(task, TaskAction::Submit, policy)?;
    }
    Ok(())
}

fn validate_execution_binding(
    execution: &Execution,
    task: &Task,
    recovery_job: bool,
) -> Result<(), ScheduledExecutionRunError> {
    let synchronized = if recovery_job {
        (execution.state, task.orchestration_state)
            == (ExecutionState::Recovering, OrchestrationState::Recovering)
    } else {
        matches!(
            (execution.state, task.orchestration_state),
            (ExecutionState::Scheduled, OrchestrationState::Scheduled)
                | (
                    ExecutionState::RetryWaiting,
                    OrchestrationState::RetryWaiting
                )
                | (
                    ExecutionState::HumanRequired,
                    OrchestrationState::HumanRequired
                )
        )
    };
    if execution.task_id == task.id && synchronized {
        Ok(())
    } else {
        Err(ScheduledExecutionRunError::StateConflict)
    }
}

struct ClaimedExecution<'a> {
    execution_id: ExecutionId,
    next_attempt_no: Option<u32>,
    worker_id: &'a str,
    recovery: bool,
}

fn claimed_execution(
    job: &ScheduledJob,
    now: Timestamp,
) -> Result<ClaimedExecution<'_>, ScheduledExecutionRunError> {
    let (execution_id, next_attempt_no, recovery) = match job.kind {
        ScheduledJobKind::Execution { execution_id } => (execution_id, None, false),
        ScheduledJobKind::Retry {
            execution_id,
            next_attempt_no,
        } => (execution_id, Some(next_attempt_no), false),
        ScheduledJobKind::Recovery { execution_id } => (execution_id, None, true),
        _ => return Err(ScheduledExecutionRunError::UnsupportedJobKind),
    };
    match &job.state {
        ScheduledJobState::Claimed {
            worker_id,
            lease_expires_at,
        } if *lease_expires_at > now => Ok(ClaimedExecution {
            execution_id,
            next_attempt_no,
            worker_id,
            recovery,
        }),
        ScheduledJobState::Claimed { .. } => Err(ScheduledExecutionRunError::ClaimExpired),
        _ => Err(ScheduledExecutionRunError::JobNotClaimed),
    }
}

fn claimed_worker(job: &ScheduledJob) -> Result<&str, ScheduledExecutionRunError> {
    match &job.state {
        ScheduledJobState::Claimed { worker_id, .. } => Ok(worker_id),
        _ => Err(ScheduledExecutionRunError::JobNotClaimed),
    }
}

fn claimed_execution_id(job: &ScheduledJob) -> Result<ExecutionId, ScheduledExecutionRunError> {
    match job.kind {
        ScheduledJobKind::Execution { execution_id }
        | ScheduledJobKind::Retry { execution_id, .. }
        | ScheduledJobKind::Recovery { execution_id } => Ok(execution_id),
        _ => Err(ScheduledExecutionRunError::UnsupportedJobKind),
    }
}

fn classify_provider_error(
    error: &ProviderError,
    attempt_no: u32,
    now: Timestamp,
    retry_policy: RetryPolicy,
) -> Result<(ProviderErrorClass, FailureDisposition), ScheduledExecutionRunError> {
    let error_class = provider_error_class(error);
    let disposition = match error.kind {
        ProviderErrorKind::Authentication | ProviderErrorKind::HumanRequired => {
            FailureDisposition::HumanRequired
        }
        ProviderErrorKind::RateLimited
        | ProviderErrorKind::Network
        | ProviderErrorKind::ProviderUnavailable => match retry_policy.delay_after(attempt_no)? {
            Some(policy_delay) => {
                let provider_delay = error
                    .retry_after_seconds
                    .unwrap_or_default()
                    .min(retry_policy.max_delay_seconds);
                FailureDisposition::RetryAt(add_duration(
                    now,
                    policy_delay.max(StdDuration::from_secs(provider_delay)),
                )?)
            }
            None => FailureDisposition::Failed,
        },
        _ => FailureDisposition::Failed,
    };
    Ok((error_class, disposition))
}

fn provider_error_class(error: &ProviderError) -> ProviderErrorClass {
    match error.kind {
        ProviderErrorKind::Authentication => ProviderErrorClass::Authentication,
        ProviderErrorKind::Authorization => ProviderErrorClass::Authorization,
        ProviderErrorKind::RateLimited => ProviderErrorClass::RateLimited,
        ProviderErrorKind::Network => ProviderErrorClass::Network,
        ProviderErrorKind::ProviderUnavailable => ProviderErrorClass::ProviderUnavailable,
        ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
            ProviderErrorClass::ProtocolDrift
        }
        ProviderErrorKind::RemoteChanged => ProviderErrorClass::InvalidRemoteState,
        ProviderErrorKind::UnsupportedTask => ProviderErrorClass::UnsupportedTask,
        ProviderErrorKind::HumanRequired => ProviderErrorClass::HumanRequired,
        ProviderErrorKind::Internal => ProviderErrorClass::Internal,
    }
}

fn add_duration(
    at: Timestamp,
    duration: StdDuration,
) -> Result<Timestamp, ScheduledExecutionRunError> {
    let duration = chrono::Duration::from_std(duration)
        .map_err(|_| ScheduledExecutionRunError::TimeOverflow)?;
    at.checked_add_signed(duration)
        .ok_or(ScheduledExecutionRunError::TimeOverflow)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureDisposition {
    RetryAt(Timestamp),
    HumanRequired,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduledExecutionOutcome {
    Succeeded(Execution),
    RetryScheduled {
        execution: Execution,
        error_class: ProviderErrorClass,
        retry_at: Timestamp,
    },
    HumanRequired {
        execution: Execution,
        error_class: ProviderErrorClass,
    },
    Failed {
        execution: Execution,
        error_class: ProviderErrorClass,
    },
    Deferred {
        retry_at: Timestamp,
    },
    LeaseBusyDeadLetter,
    AlreadyTerminal(Execution),
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduledExecutionRunError {
    #[error("execution worker configuration is invalid")]
    InvalidConfiguration,
    #[error("scheduler job is not an execution, retry or recovery job")]
    UnsupportedJobKind,
    #[error("scheduler job is not claimed")]
    JobNotClaimed,
    #[error("scheduler claim has expired")]
    ClaimExpired,
    #[error("execution or scheduler lease ownership was lost")]
    ClaimLost,
    #[error("execution `{0}` is missing")]
    ExecutionMissing(ExecutionId),
    #[error("task `{0}` is missing")]
    TaskMissing(asterism_domain::TaskId),
    #[error("execution and task orchestration state conflict")]
    StateConflict,
    #[error("execution worker timestamp overflow")]
    TimeOverflow,
    #[error(transparent)]
    RetryPolicy(#[from] RetryPolicyError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use asterism_domain::{
        AssessmentClass, AuditActor, ProviderAccountId, ProviderId, ProviderRuntimeSettingsId,
        RequestSource, TaskId, UserId,
    };
    use asterism_provider_api::{
        ExecutionOutcome, ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata,
        ProviderResult, ProviderRuntimeSettingsSchema, ProviderSettingDefinition,
        ProviderSettingKind, ProviderSettingScope, ProviderSettingValue, RemoteProgress,
        TaskProgressCapability, VerificationLevel,
    };
    use asterism_storage::{
        Database, ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot,
        ExecutionScheduleRequest, SqliteExecutionLeaseRepository, SqliteExecutionRepository,
        SqliteProviderAccountRepository, SqliteSchedulerRepository, SqliteTaskQueryRepository,
    };
    use sqlx::Row;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum ProviderBehavior {
        Success,
        NetworkFailure,
        RecoveryPending,
    }

    #[derive(Debug)]
    struct FakeExecution {
        metadata: ProviderMetadata,
        behavior: ProviderBehavior,
        calls: Mutex<u32>,
        progress_calls: Mutex<u32>,
    }

    impl ProviderIdentity for FakeExecution {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskExecutionCapability for FakeExecution {
        async fn execute(
            &self,
            _context: &ProviderContext,
            request: &ProviderExecutionRequest,
            events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ExecutionOutcome> {
            *self.calls.lock().unwrap() += 1;
            assert_eq!(
                request.requested_capabilities,
                [TaskCapability::ResourceExecution]
            );
            assert_eq!(
                request
                    .runtime_settings
                    .integer("execution.max_concurrency"),
                Some(3)
            );
            match self.behavior {
                ProviderBehavior::Success => {
                    events
                        .report(ProviderProgress {
                            percent: Some(50),
                            stage: "resource_execute".to_owned(),
                            status_text: Some("safe progress".to_owned()),
                            completed_items: Some(1),
                            total_items: Some(2),
                        })
                        .await?;
                    events
                        .log(ProviderExecutionLog {
                            level: asterism_domain::LogLevel::Info,
                            stage: "resource_verify".to_owned(),
                            message: "safe provider diagnostic".to_owned(),
                            provider_trace_id: Some("trace-safe".to_owned()),
                            metadata_sanitized: Some(serde_json::json!({"verified": true})),
                        })
                        .await?;
                    Ok(ExecutionOutcome {
                        remote_state: RemoteState::Completed,
                        verified: true,
                        result_sanitized: serde_json::json!({"verified": true}),
                    })
                }
                ProviderBehavior::NetworkFailure => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "temporary network failure",
                )),
                ProviderBehavior::RecoveryPending => Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "recovery-only fixture cannot execute",
                )),
            }
        }
    }

    #[async_trait]
    impl TaskProgressCapability for FakeExecution {
        async fn read_progress(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<RemoteProgress> {
            *self.progress_calls.lock().unwrap() += 1;
            match self.behavior {
                ProviderBehavior::Success => Ok(RemoteProgress {
                    remote_state: RemoteState::Completed,
                    percent: Some(100),
                    duration_seconds: None,
                    updated_at: Utc::now(),
                }),
                ProviderBehavior::RecoveryPending => Ok(RemoteProgress {
                    remote_state: RemoteState::Pending,
                    percent: Some(0),
                    duration_seconds: None,
                    updated_at: Utc::now(),
                }),
                ProviderBehavior::NetworkFailure => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "temporary progress read failure",
                )),
            }
        }
    }

    #[tokio::test]
    async fn successful_provider_execution_is_verified_and_committed() {
        let fixture = Fixture::new(AssessmentClass::Routine, ProviderBehavior::Success).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::Succeeded(ref execution)
                if execution.id == fixture.execution_id
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        let state = fixture.persisted_state().await;
        assert_eq!(state, ("succeeded".to_owned(), "succeeded".to_owned(), 100));
        let provider_log: (String, String, String, String) = sqlx::query_as(
            "SELECT level, stage, message, metadata_sanitized_json FROM execution_logs \
             WHERE message = 'safe provider diagnostic'",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            provider_log,
            (
                "info".to_owned(),
                "verifying".to_owned(),
                "safe provider diagnostic".to_owned(),
                r#"{"verified":true}"#.to_owned(),
            )
        );
        let live_log: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox WHERE event_type = 'execution_logged' \
             AND payload_json LIKE '%safe provider diagnostic%'",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(live_log, 1);
    }

    #[tokio::test]
    async fn retryable_provider_failure_schedules_numbered_retry() {
        let fixture =
            Fixture::new(AssessmentClass::Routine, ProviderBehavior::NetworkFailure).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::RetryScheduled {
                error_class: ProviderErrorClass::Network,
                ..
            }
        ));
        let jobs: Vec<(String, String)> =
            sqlx::query("SELECT job_kind, state FROM scheduled_jobs ORDER BY created_at, id")
                .fetch_all(fixture.database.pool())
                .await
                .unwrap()
                .iter()
                .map(|row| (row.get("job_kind"), row.get("state")))
                .collect();
        assert_eq!(
            jobs,
            [
                ("execution".to_owned(), "completed".to_owned()),
                ("retry".to_owned(), "pending".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn formal_assessment_stops_before_provider_mutation() {
        let fixture = Fixture::new(AssessmentClass::Formal, ProviderBehavior::Success).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::HumanRequired {
                error_class: ProviderErrorClass::Authorization,
                ..
            }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        let state = fixture.persisted_state().await;
        assert_eq!(
            (state.0, state.1),
            ("human_required".to_owned(), "human_required".to_owned())
        );
    }

    #[tokio::test]
    async fn recovery_verifies_remote_completion_without_reexecuting() {
        let fixture = Fixture::recovering(ProviderBehavior::Success).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
        let state = fixture.persisted_state().await;
        assert_eq!(state, ("succeeded".to_owned(), "succeeded".to_owned(), 100));
    }

    #[tokio::test]
    async fn worker_uses_the_frozen_snapshot_after_provider_defaults_change() {
        let fixture = Fixture::new(AssessmentClass::Routine, ProviderBehavior::Success).await;
        let changed = asterism_provider_api::ProviderRuntimeSettingsPatch {
            schema_version: 1,
            values: std::collections::BTreeMap::from([(
                "execution.max_concurrency".to_owned(),
                ProviderSettingValue::Integer(8),
            )]),
        };
        let now = fixture.now.to_rfc3339();
        sqlx::query(
            "INSERT INTO provider_runtime_settings \
             (id, scope, provider_id, schema_version, revision, settings_json, created_at, updated_at) \
             VALUES (?, 'provider', 'provider-alpha', 1, 1, ?, ?, ?)",
        )
        .bind(ProviderRuntimeSettingsId::new().to_string())
        .bind(serde_json::to_string(&changed).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(fixture.database.pool())
        .await
        .unwrap();

        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn missing_frozen_settings_fail_before_provider_mutation() {
        let fixture = Fixture::new(AssessmentClass::Routine, ProviderBehavior::Success).await;
        sqlx::query("DELETE FROM execution_runtime_settings WHERE execution_id = ?")
            .bind(fixture.execution_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();

        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::Failed {
                error_class: ProviderErrorClass::Internal,
                ..
            }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn recovery_retries_only_after_remote_pending_is_confirmed() {
        let fixture = Fixture::recovering(ProviderBehavior::RecoveryPending).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::RetryScheduled {
                error_class: ProviderErrorClass::InvalidRemoteState,
                ..
            }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        let jobs: Vec<(String, String)> =
            sqlx::query("SELECT job_kind, state FROM scheduled_jobs ORDER BY created_at, id")
                .fetch_all(fixture.database.pool())
                .await
                .unwrap()
                .iter()
                .map(|row| (row.get("job_kind"), row.get("state")))
                .collect();
        assert!(jobs.contains(&("execution".to_owned(), "cancelled".to_owned())));
        assert!(jobs.contains(&("recovery".to_owned(), "completed".to_owned())));
        assert!(jobs.contains(&("retry".to_owned(), "pending".to_owned())));
    }

    #[tokio::test]
    async fn transient_recovery_read_retries_verification_not_execution() {
        let fixture = Fixture::recovering(ProviderBehavior::NetworkFailure).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::Deferred { .. }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        let state: (String, String, i64) = sqlx::query_as(
            "SELECT e.state, t.orchestration_state, j.attempts FROM executions e \
             JOIN tasks t ON t.id = e.task_id \
             JOIN scheduled_jobs j ON j.job_kind = 'recovery' WHERE e.id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(state, ("recovering".to_owned(), "recovering".to_owned(), 1));
    }

    #[tokio::test]
    async fn recovery_without_advertised_progress_requires_human_review() {
        let fixture = Fixture::recovering(ProviderBehavior::Success).await;
        sqlx::query("UPDATE tasks SET capabilities_json = '[\"resource_execution\"]'")
            .execute(fixture.database.pool())
            .await
            .unwrap();
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::HumanRequired {
                error_class: ProviderErrorClass::UnsupportedTask,
                ..
            }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 0);
    }

    type TestRunner = ScheduledExecutionRunner<
        SqliteExecutionRepository,
        SqliteExecutionLeaseRepository,
        SqliteSchedulerRepository,
        SqliteProviderAccountRepository,
        SqliteTaskQueryRepository,
    >;

    struct Fixture {
        database: Database,
        runner: TestRunner,
        provider: Arc<FakeExecution>,
        job: ScheduledJob,
        execution_id: ExecutionId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new(assessment: AssessmentClass, behavior: ProviderBehavior) -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let now = Utc::now();
            let (owner, account_id, task_id) = seed_task(&database, assessment, now).await;
            let execution_id = schedule_and_claim(&database, owner, task_id, now).await;
            let scheduler = SqliteSchedulerRepository::new(database.clone());
            let job = scheduler
                .claim_due(
                    "execution-worker",
                    now,
                    now + chrono::Duration::minutes(5),
                    1,
                )
                .await
                .unwrap()
                .pop()
                .unwrap();
            let provider = Arc::new(FakeExecution {
                metadata: provider_metadata(),
                behavior,
                calls: Mutex::new(0),
                progress_calls: Mutex::new(0),
            });
            let mut registry = ProviderRegistry::default();
            registry
                .register(ProviderEntry {
                    metadata: provider.metadata.clone(),
                    runtime_settings: runtime_settings_schema(),
                    authentication: None,
                    course_inventory: None,
                    task_inventory: None,
                    task_detail: None,
                    task_progress: Some(provider.clone()),
                    task_execution: Some(provider.clone()),
                    browser_bridge: None,
                })
                .unwrap();
            let runner = ScheduledExecutionRunner::new(
                Arc::new(registry),
                SqliteExecutionRepository::new(database.clone()),
                SqliteExecutionLeaseRepository::new(database.clone()),
                scheduler,
                SqliteProviderAccountRepository::new(database.clone()),
                SqliteTaskQueryRepository::new(database.clone()),
                runner_config(),
            )
            .unwrap();
            let _ = account_id;
            Self {
                database,
                runner,
                provider,
                job,
                execution_id,
                now,
            }
        }

        async fn recovering(behavior: ProviderBehavior) -> Self {
            let mut fixture = Self::new(AssessmentClass::Routine, behavior).await;
            let task_id = execution_id_to_task(
                &SqliteExecutionRepository::new(fixture.database.clone()),
                fixture.execution_id,
            )
            .await
            .unwrap();
            let lease = ExecutionLease {
                task_id,
                execution_id: fixture.execution_id,
                worker_id: "execution-worker".to_owned(),
                expires_at: fixture.now + chrono::Duration::minutes(1),
            };
            SqliteExecutionLeaseRepository::new(fixture.database.clone())
                .try_acquire(&lease, fixture.now)
                .await
                .unwrap();
            SqliteExecutionRepository::new(fixture.database.clone())
                .start_attempt(ExecutionAttemptStartRequest {
                    execution_id: fixture.execution_id,
                    scheduler_job_id: fixture.job.id,
                    worker_id: "execution-worker",
                    at: fixture.now,
                    correlation_id: "stale-execution-test",
                })
                .await
                .unwrap();
            let expired = (fixture.now - chrono::Duration::seconds(1)).to_rfc3339();
            sqlx::query("UPDATE execution_leases SET expires_at = ? WHERE execution_id = ?")
                .bind(&expired)
                .bind(fixture.execution_id.to_string())
                .execute(fixture.database.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE scheduled_jobs SET lease_expires_at = ? WHERE id = ?")
                .bind(&expired)
                .bind(fixture.job.id.to_string())
                .execute(fixture.database.pool())
                .await
                .unwrap();
            fixture
                .database
                .recover_stale_work(fixture.now)
                .await
                .unwrap();
            fixture.job = SqliteSchedulerRepository::new(fixture.database.clone())
                .claim_due_execution_jobs(
                    "recovery-worker",
                    fixture.now,
                    fixture.now + chrono::Duration::minutes(5),
                    1,
                )
                .await
                .unwrap()
                .pop()
                .unwrap();
            fixture
        }

        async fn persisted_state(&self) -> (String, String, i64) {
            sqlx::query_as(
                "SELECT e.state, t.orchestration_state, p.percent FROM executions e \
                 JOIN tasks t ON t.id = e.task_id \
                 JOIN execution_progress p ON p.execution_id = e.id WHERE e.id = ?",
            )
            .bind(self.execution_id.to_string())
            .fetch_one(self.database.pool())
            .await
            .unwrap()
        }
    }

    async fn seed_task(
        database: &Database,
        assessment: AssessmentClass,
        now: Timestamp,
    ) -> (UserId, ProviderAccountId, TaskId) {
        let owner = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let now_text = now.to_rfc3339();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, 'hash', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(format!("user-{owner}"))
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'provider-alpha', 'Provider', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'remote-task', 'fingerprint', ?, ?, 'Task', 'pending', 'ready', ?, ?, ?)",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind("resource")
        .bind(match assessment {
            AssessmentClass::Routine => "routine",
            AssessmentClass::Unknown => "unknown",
            AssessmentClass::Formal => "formal",
        })
        .bind(&now_text)
        .bind(&now_text)
        .bind(
            serde_json::to_string(&[
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
            ])
            .unwrap(),
        )
        .execute(database.pool())
        .await
        .unwrap();
        (owner, account_id, task_id)
    }

    async fn schedule_and_claim(
        database: &Database,
        owner: UserId,
        task_id: TaskId,
        now: Timestamp,
    ) -> ExecutionId {
        let execution = Execution {
            id: ExecutionId::new(),
            task_id,
            requested_by: Some(owner),
            request_source: RequestSource::System,
            quote_id: None,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        };
        let schema = runtime_settings_schema();
        let (resolved, sources) = schema.resolve_with_sources(None, None, None).unwrap();
        let runtime_settings = ExecutionRuntimeSettingsSnapshot {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            resolved,
            sources,
            provider_revision: None,
            provider_account_revision: None,
            task_revision: None,
            captured_at: now,
        };
        SqliteExecutionRepository::new(database.clone())
            .schedule_execution(ExecutionScheduleRequest {
                execution: &execution,
                billing: None,
                runtime_settings: Some(ExecutionRuntimeSettingsResolution {
                    snapshot: &runtime_settings,
                    schema: &schema,
                }),
                expected_task_state: OrchestrationState::Ready,
                idempotency_scope: "test",
                idempotency_key: "execution",
                actor: AuditActor::User(owner),
                correlation_id: "execution-test",
            })
            .await
            .unwrap();
        execution.id
    }

    fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
        ProviderRuntimeSettingsSchema {
            version: 1,
            definitions: vec![ProviderSettingDefinition {
                key: "execution.max_concurrency".to_owned(),
                display_name: "Execution concurrency".to_owned(),
                description: "Maximum concurrent Provider work.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 8,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(3),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            }],
        }
    }

    fn provider_metadata() -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "Provider Alpha".to_owned(),
            implementation_version: "test".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([
                ProviderCapability::TaskProgressRead,
                ProviderCapability::ResourceExecution,
            ]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        }
    }

    const fn runner_config() -> ExecutionRunnerConfig {
        ExecutionRunnerConfig {
            execution_lease_ttl: StdDuration::from_mins(1),
            heartbeat_interval: StdDuration::from_secs(10),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay_seconds: 10,
                multiplier: 2,
                max_delay_seconds: 60,
            },
            formal_assessment_policy: FormalAssessmentPolicy {
                allow_execution: false,
                allow_submission: false,
            },
        }
    }
}
