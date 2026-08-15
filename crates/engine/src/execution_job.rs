use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration as StdDuration,
};

use asterism_domain::{
    AttemptResult, AuthState, Execution, ExecutionAttempt, ExecutionId, ExecutionLease,
    ExecutionProgress, ExecutionStage, ExecutionState, HumanRequiredReason, OrchestrationState,
    ProviderAccountId, ProviderErrorClass, ProviderId, QuestionSnapshotId, RemoteState,
    StrictCompletionState, SubmissionAttemptReceipt, SubmissionDraft, SubmissionResult,
    SubmissionResultId, SubmissionResultStatus, SubmissionVerificationSnapshot,
    SubmissionVerificationStatus, Task, TaskCapability, Timestamp,
};
use asterism_provider_api::{
    AmbiguousProviderQuestionSessionOperation, ExecutionEventSink, ExecutionMutationIssue,
    ExecutionMutationReceipt, ExecutionMutationSink, ExecutionRequest as ProviderExecutionRequest,
    ProviderCapability, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderExecutionConcurrency, ProviderExecutionLog, ProviderProgress,
    ProviderQuestionMaterialization, ProviderRegistry, ProviderSubmissionStepOutcome,
    ResolvedProviderQuestionSessionContinuation, ResolvedProviderRuntimeSettings,
    SubmissionExecuteCapability, SubmissionVerifyCapability, TaskExecutionCapability,
    TaskProgressCapability,
};
use asterism_scheduler::{
    RetryPolicy, RetryPolicyError, ScheduledJob, ScheduledJobKind, ScheduledJobState,
};
use asterism_secrets::{SecretAccess, SecretActor, SecretStoreError};
use asterism_storage::{
    CompletionWorkflowRepository, ExecutionAtomicMutationIssueOutcome,
    ExecutionAtomicMutationIssueRequest, ExecutionAtomicMutationReceiptOutcome,
    ExecutionAtomicMutationReceiptRequest, ExecutionAtomicMutationRepository,
    ExecutionAttemptFinishRequest, ExecutionAttemptStartRequest, ExecutionCapabilityCallMutation,
    ExecutionCapabilityStep, ExecutionCapabilityStepIssueOutcome, ExecutionCapabilityStepMutation,
    ExecutionCapabilityStepRepository, ExecutionCapabilityStepState, ExecutionLeaseRepository,
    ExecutionLogAppendRequest, ExecutionProgressUpdate, ExecutionQuestionStepFinishRequest,
    ExecutionRecoveryFinishRequest, ExecutionRepository, ExecutionSubmissionRepository,
    ExecutionVerificationRecoveryRepository, LeaseAcquireOutcome, ProviderAccountRuntimeRepository,
    QuestionSessionArtifactRepository, QuestionSessionArtifactRepositoryFactory,
    QuestionSessionNextMaterializeOutcome, QuestionSessionNextMaterializeRequest,
    QuestionSessionOperation, QuestionSessionOperationAcceptRequest,
    QuestionSessionOperationFinishOutcome, QuestionSessionOperationIssueOutcome,
    QuestionSessionOperationIssueRequest, QuestionSessionOperationState, QuestionSessionTransition,
    QuestionSnapshot, ResolvedQuestionSessionContinuation, SchedulerRepository, StorageError,
    StrictCompletionExecutionObservationRecord, StrictCompletionExecutionObservationRequest,
    SubmissionReceiptPersistRequest, SubmissionResultPersistRequest, TaskRuntimeRepository,
    VerificationRecoveryStartRequest,
};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Notify;

use crate::{FormalAssessmentPolicy, TaskAction, authorize_task_action};

const MAX_DURABLE_SUBMISSION_OPERATIONS: usize = 512;
const MAX_DURABLE_SUBMISSION_DELAY_SECONDS: u64 = 15 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionRunnerConfig {
    pub execution_lease_ttl: StdDuration,
    pub heartbeat_interval: StdDuration,
    pub global_concurrency_limit: u32,
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
            || self.global_concurrency_limit == 0
            || self.global_concurrency_limit > 1_000
            || i64::try_from(self.execution_lease_ttl.as_secs()).is_err()
            || i64::try_from(self.heartbeat_interval.as_secs()).is_err()
            || i64::try_from(self.retry_policy.max_delay_seconds).is_err()
        {
            return Err(ScheduledExecutionRunError::InvalidConfiguration);
        }
        Ok(self)
    }
}

#[derive(Debug, Default)]
struct ExecutionAdmissionState {
    global: u32,
    providers: BTreeMap<ProviderId, u32>,
    accounts: BTreeMap<ProviderAccountId, u32>,
}

#[derive(Debug)]
struct ExecutionAdmissionController {
    global_limit: u32,
    state: Mutex<ExecutionAdmissionState>,
    changed: Notify,
}

impl ExecutionAdmissionController {
    fn new(global_limit: u32) -> Self {
        Self {
            global_limit,
            state: Mutex::new(ExecutionAdmissionState::default()),
            changed: Notify::new(),
        }
    }

    async fn acquire(
        self: &Arc<Self>,
        provider_id: &ProviderId,
        account_id: ProviderAccountId,
        limits: ProviderExecutionConcurrency,
    ) -> ExecutionAdmissionGuard {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let provider_active = state
                    .providers
                    .get(provider_id)
                    .copied()
                    .unwrap_or_default();
                let account_active = state.accounts.get(&account_id).copied().unwrap_or_default();
                if state.global < self.global_limit
                    && provider_active < limits.provider
                    && account_active < limits.account
                {
                    state.global += 1;
                    *state.providers.entry(provider_id.clone()).or_default() += 1;
                    *state.accounts.entry(account_id).or_default() += 1;
                    return ExecutionAdmissionGuard {
                        controller: Arc::clone(self),
                        provider_id: provider_id.clone(),
                        account_id,
                    };
                }
            }
            changed.await;
        }
    }
}

#[derive(Debug)]
struct ExecutionAdmissionGuard {
    controller: Arc<ExecutionAdmissionController>,
    provider_id: ProviderId,
    account_id: ProviderAccountId,
}

impl Drop for ExecutionAdmissionGuard {
    fn drop(&mut self) {
        let mut state = self
            .controller
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.global = state.global.saturating_sub(1);
        decrement_or_remove(&mut state.providers, &self.provider_id);
        decrement_or_remove(&mut state.accounts, &self.account_id);
        drop(state);
        self.controller.changed.notify_waiters();
    }
}

fn decrement_or_remove<K: Ord + Clone>(values: &mut BTreeMap<K, u32>, key: &K) {
    let Some(value) = values.get_mut(key) else {
        return;
    };
    if *value <= 1 {
        values.remove(key);
    } else {
        *value -= 1;
    }
}

pub struct ScheduledExecutionRunner<E, L, S, A, T> {
    registry: Arc<ProviderRegistry>,
    executions: E,
    leases: L,
    scheduler: S,
    accounts: A,
    tasks: T,
    question_sessions: Option<Arc<dyn QuestionSessionArtifactRepositoryFactory>>,
    admission: Arc<ExecutionAdmissionController>,
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
        let config = config.validate()?;
        Ok(Self {
            registry,
            executions,
            leases,
            scheduler,
            accounts,
            tasks,
            question_sessions: None,
            admission: Arc::new(ExecutionAdmissionController::new(
                config.global_concurrency_limit,
            )),
            config,
        })
    }

    #[must_use]
    pub fn with_question_session_artifacts(
        mut self,
        artifacts: Arc<dyn QuestionSessionArtifactRepositoryFactory>,
    ) -> Self {
        self.question_sessions = Some(artifacts);
        self
    }
}

impl<E, L, S, A, T> std::fmt::Debug for ScheduledExecutionRunner<E, L, S, A, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScheduledExecutionRunner")
            .field("registry", &self.registry)
            .field("executions", &"configured")
            .field("leases", &"configured")
            .field("scheduler", &"configured")
            .field("accounts", &"configured")
            .field("tasks", &"configured")
            .field("question_sessions", &self.question_sessions.is_some())
            .field("admission", &self.admission)
            .field("config", &self.config)
            .finish()
    }
}

impl<E, L, S, A, T> ScheduledExecutionRunner<E, L, S, A, T>
where
    E: ExecutionRepository
        + ExecutionAtomicMutationRepository
        + ExecutionCapabilityStepRepository
        + ExecutionSubmissionRepository
        + ExecutionVerificationRecoveryRepository
        + CompletionWorkflowRepository,
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
                .recover_execution(job, &execution, &task, now, &correlation_id)
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
        self.execute_attempt(job, &execution, &task, &attempt, now, &correlation_id)
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "recovery keeps dispatch selection, active-attempt binding and final verified observation in one fail-closed chain"
    )]
    async fn recover_execution(
        &self,
        job: &ScheduledJob,
        execution: &Execution,
        task: &Task,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if execution.requested_capabilities == [TaskCapability::SubmissionExecute] {
            return self
                .recover_submission(job, execution, task, now, correlation_id)
                .await;
        }
        if execution.requested_capabilities.len() > 1 {
            return self
                .recover_composite_execution(job, execution, task, now, correlation_id)
                .await;
        }
        let prepared = match self
            .prepare_provider_call(
                execution.id,
                task,
                &execution.requested_capabilities,
                &execution.requested_capabilities,
                1,
                correlation_id,
            )
            .await?
        {
            Ok(prepared) => prepared,
            Err(failure) => {
                return self
                    .finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(failure.error_class),
                        None,
                        now,
                        correlation_id,
                    )
                    .await;
            }
        };
        if !prepared.verification {
            if duration_report_only(&execution.requested_capabilities) {
                return self
                    .finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(ProviderErrorClass::InvalidRemoteState),
                        None,
                        now,
                        correlation_id,
                    )
                    .await;
            }
            return self
                .recover_execution_by_progress(job, task, now, correlation_id)
                .await;
        }
        let execution_id = claimed_execution_id(job)?;
        let Some(attempt_id) = self
            .executions
            .find_active_execution_attempt_id(execution_id)
            .await?
        else {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::Internal),
                    None,
                    now,
                    correlation_id,
                )
                .await;
        };
        let _admission = self
            .acquire_admission(job, execution_id, &prepared.context, prepared.concurrency)
            .await?;
        let provider = prepared
            .capability
            .verify_execution(&prepared.context, &prepared.request);
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
            Ok(verification) => {
                self.finish_from_execution_verification(
                    job,
                    attempt_id,
                    &prepared,
                    &verification,
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

    #[allow(
        clippy::too_many_lines,
        reason = "recovery keeps the exact issued phase, attempt binding and verify-only decision visible"
    )]
    async fn recover_composite_execution(
        &self,
        job: &ScheduledJob,
        execution: &Execution,
        task: &Task,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let steps = self
            .executions
            .find_execution_capability_steps(execution.id)
            .await?;
        if !capability_steps_match_execution(execution, &steps) {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::Internal),
                    None,
                    now,
                    correlation_id,
                )
                .await;
        }
        let capability_plan = steps.iter().map(|step| step.capability).collect::<Vec<_>>();
        let Some(calls) = capability_calls(&steps) else {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::Internal),
                    None,
                    now,
                    correlation_id,
                )
                .await;
        };
        let Some(call) = calls
            .iter()
            .find(|call| call.state != ExecutionCapabilityStepState::Succeeded)
        else {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::Succeeded,
                    None,
                    None,
                    now,
                    correlation_id,
                )
                .await;
        };
        if call.state == ExecutionCapabilityStepState::Pending {
            return self
                .continue_composite_after_recovery(job, now, correlation_id)
                .await;
        }
        let Some(attempt_id) = call.issued_attempt_id else {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::Internal),
                    None,
                    now,
                    correlation_id,
                )
                .await;
        };
        let prepared = match self
            .prepare_provider_call(
                execution.id,
                task,
                &call.capabilities,
                &capability_plan,
                call.first_step_position,
                correlation_id,
            )
            .await?
        {
            Ok(prepared) if prepared.verification => prepared,
            Ok(_) => {
                return self
                    .finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(ProviderErrorClass::InvalidRemoteState),
                        None,
                        now,
                        correlation_id,
                    )
                    .await;
            }
            Err(failure) => {
                return self
                    .finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(failure.error_class),
                        None,
                        now,
                        correlation_id,
                    )
                    .await;
            }
        };
        let execution_id = claimed_execution_id(job)?;
        let _admission = self
            .acquire_admission(job, execution_id, &prepared.context, prepared.concurrency)
            .await?;
        let provider = prepared
            .capability
            .verify_execution(&prepared.context, &prepared.request);
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => self.renew_claims(job, execution_id).await?,
            }
        };
        let finished_at = Utc::now().max(now);
        match result {
            Ok(verification)
                if execution_goal_verified(
                    &prepared.request.requested_capabilities,
                    &verification,
                ) =>
            {
                if call.capabilities.len() == 1 {
                    self.executions
                        .succeed_execution_capability_step(ExecutionCapabilityStepMutation {
                            execution_id,
                            attempt_id,
                            capability: call.capabilities[0],
                            scheduler_job_id: job.id,
                            worker_id: claimed_worker(job)?,
                            correlation_id,
                            at: finished_at,
                        })
                        .await?;
                } else {
                    self.executions
                        .succeed_execution_capability_call(ExecutionCapabilityCallMutation {
                            execution_id,
                            attempt_id,
                            call_position: call.position,
                            capabilities: &call.capabilities,
                            scheduler_job_id: job.id,
                            worker_id: claimed_worker(job)?,
                            correlation_id,
                            at: finished_at,
                        })
                        .await?;
                }
                if calls
                    .iter()
                    .all(|candidate| candidate.position <= call.position)
                {
                    self.record_provider_completion_observation_for_attempt(
                        job,
                        attempt_id,
                        execution_id,
                        &prepared,
                        &verification,
                        finished_at,
                        correlation_id,
                    )
                    .await?;
                    self.finish_recovery(
                        job,
                        ExecutionState::Succeeded,
                        None,
                        None,
                        finished_at,
                        correlation_id,
                    )
                    .await
                } else {
                    self.continue_composite_after_recovery(job, finished_at, correlation_id)
                        .await
                }
            }
            Ok(verification) => {
                if let Some(retry_at) = self.recovery_retry_at(job, finished_at)? {
                    self.defer_recovery(job, retry_at, finished_at).await
                } else {
                    if verification.verified && verification.validate().is_ok() {
                        self.record_provider_completion_observation_for_attempt(
                            job,
                            attempt_id,
                            execution_id,
                            &prepared,
                            &verification,
                            finished_at,
                            correlation_id,
                        )
                        .await?;
                    }
                    self.finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(ProviderErrorClass::InvalidRemoteState),
                        None,
                        finished_at,
                        correlation_id,
                    )
                    .await
                }
            }
            Err(error) => {
                self.finish_from_recovery_error(job, &error, finished_at, correlation_id)
                    .await
            }
        }
    }

    async fn continue_composite_after_recovery(
        &self,
        job: &ScheduledJob,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let retry_at = self.recovery_retry_at(job, at)?.unwrap_or(at);
        self.finish_recovery(
            job,
            ExecutionState::RetryWaiting,
            Some(ProviderErrorClass::InvalidRemoteState),
            Some(retry_at),
            at,
            correlation_id,
        )
        .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "submission recovery keeps the no-resubmit verification path and frozen bindings explicit"
    )]
    async fn recover_submission(
        &self,
        job: &ScheduledJob,
        execution: &Execution,
        task: &Task,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let execution_id = claimed_execution_id(job)?;
        let prepared = match self
            .prepare_submission_call(
                execution_id,
                task,
                &execution.requested_capabilities,
                correlation_id,
            )
            .await?
        {
            Ok(prepared) => prepared,
            Err(failure) => {
                return self
                    .finish_recovery(
                        job,
                        ExecutionState::HumanRequired,
                        Some(failure.error_class),
                        None,
                        now,
                        correlation_id,
                    )
                    .await;
            }
        };
        if let Some(result) = self
            .executions
            .find_active_submission_result(execution_id)
            .await?
        {
            let completion = self
                .record_submission_completion_observation(
                    job,
                    result.execution_attempt_id,
                    &prepared,
                    &result.verification,
                    now.max(result.created_at),
                    correlation_id,
                )
                .await?;
            return self
                .finish_recovery_from_submission_result(
                    job,
                    &result,
                    completion.workflow.workflow.state,
                    now.max(result.created_at),
                    correlation_id,
                )
                .await;
        }
        let Some(attempt_id) = self
            .executions
            .find_active_submission_attempt_id(execution_id)
            .await?
        else {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::Internal),
                    None,
                    now,
                    correlation_id,
                )
                .await;
        };
        let mut receipt = self
            .executions
            .find_active_submission_receipt(execution_id)
            .await?;
        if receipt.as_ref().is_some_and(|record| {
            record.execution_attempt_id != attempt_id
                || record.validate_for_draft(&prepared.draft).is_err()
        }) {
            return self
                .finish_recovery(
                    job,
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::Internal),
                    None,
                    now,
                    correlation_id,
                )
                .await;
        }
        let artifacts = self
            .question_sessions
            .as_ref()
            .map(|factory| factory.for_provider(prepared.context.provider_id.clone()));
        let access = SecretAccess {
            actor: SecretActor::CoreService("execution-worker"),
            correlation_id: correlation_id.to_owned(),
            reason: "durable QuestionSession submission recovery".to_owned(),
        };
        let mut resolved_session = if let Some(artifacts) = artifacts.as_ref() {
            artifacts
                .resolve_question_session_continuation(execution_id, &access)
                .await?
        } else {
            None
        };
        if let Some(transition) = resolved_session
            .as_ref()
            .and_then(|resolved| resolved.latest_transition.clone())
        {
            return self
                .finish_next_question_step(
                    job,
                    execution_id,
                    attempt_id,
                    &transition,
                    now.max(transition.transitioned_at),
                    correlation_id,
                )
                .await;
        }
        if let (Some(artifacts), Some(resolved)) = (artifacts.as_ref(), resolved_session.as_ref())
            && let Some(mut operation) = resolved.latest_operation.clone()
            && matches!(
                operation.state,
                QuestionSessionOperationState::Issued | QuestionSessionOperationState::Ambiguous
            )
        {
            if operation.state == QuestionSessionOperationState::Issued {
                operation = match artifacts
                    .finish_question_session_operation(
                        &operation,
                        QuestionSessionOperationState::Ambiguous,
                        None,
                        now.max(operation.issued_at),
                        correlation_id,
                    )
                    .await?
                {
                    QuestionSessionOperationFinishOutcome::Finished(operation)
                    | QuestionSessionOperationFinishOutcome::Duplicate(operation)
                        if operation.state == QuestionSessionOperationState::Ambiguous =>
                    {
                        operation
                    }
                    _ => return Err(ScheduledExecutionRunError::StateConflict),
                };
            }
            let recovered = {
                let continuation = provider_session_continuation(resolved);
                let ambiguous = provider_ambiguous_operation(&operation);
                let provider = prepared.execute.recover_ambiguous_submission_operation(
                    &prepared.context,
                    &prepared.remote_task_id,
                    &prepared.draft,
                    continuation,
                    &ambiguous,
                    &prepared.runtime_settings,
                );
                tokio::pin!(provider);
                let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                heartbeat.tick().await;
                loop {
                    tokio::select! {
                        result = &mut provider => break result,
                        _ = heartbeat.tick() => self.renew_claims(job, execution_id).await?,
                    }
                }
            };
            let recovered = match recovered {
                Ok(recovered) => recovered,
                Err(error) => {
                    return self
                        .finish_from_recovery_error(job, &error, now, correlation_id)
                        .await;
                }
            };
            if recovered.is_none() {
                return self
                    .defer_or_require_submission_review(
                        job,
                        ProviderErrorClass::InvalidRemoteState,
                        now,
                        correlation_id,
                    )
                    .await;
            }
            if let Some(outcome) = recovered {
                let recovered_outcome = accept_durable_submission_outcome(
                    artifacts.as_ref(),
                    &operation,
                    outcome,
                    &access,
                    &prepared,
                )
                .await?;
                match recovered_outcome {
                    AcceptedDurableSubmissionOutcome::Submitted(recovered_receipt) => {
                        let record = SubmissionAttemptReceipt {
                            submission_draft_id: prepared.draft.id,
                            execution_id,
                            execution_attempt_id: attempt_id,
                            receipt: recovered_receipt.clone(),
                        };
                        record
                            .validate_for_draft(&prepared.draft)
                            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
                        self.executions
                            .persist_submission_receipt(SubmissionReceiptPersistRequest {
                                record: &record,
                                scheduler_job_id: job.id,
                                worker_id: claimed_worker(job)?,
                                correlation_id,
                                at: Utc::now().max(now),
                            })
                            .await?;
                        receipt = Some(record);
                    }
                    AcceptedDurableSubmissionOutcome::NextQuestion(transition) => {
                        return self
                            .finish_next_question_step(
                                job,
                                execution_id,
                                attempt_id,
                                &transition,
                                Utc::now().max(transition.transitioned_at),
                                correlation_id,
                            )
                            .await;
                    }
                    AcceptedDurableSubmissionOutcome::Continue => {
                        let retry_at = self.recovery_retry_at(job, now)?.unwrap_or(now);
                        return self
                            .finish_recovery(
                                job,
                                ExecutionState::RetryWaiting,
                                Some(ProviderErrorClass::InvalidRemoteState),
                                Some(retry_at),
                                now,
                                correlation_id,
                            )
                            .await;
                    }
                }
                resolved_session = artifacts
                    .resolve_question_session_continuation(execution_id, &access)
                    .await?;
            }
        }
        let _admission = self
            .acquire_admission(job, execution_id, &prepared.context, prepared.concurrency)
            .await?;
        let provider = if let Some(resolved) = resolved_session.as_ref() {
            prepared.verify.verify_submission_with_session(
                &prepared.context,
                &prepared.remote_task_id,
                &prepared.draft,
                receipt.as_ref().map(|record| &record.receipt),
                provider_session_continuation(resolved),
            )
        } else {
            prepared.verify.verify_submission(
                &prepared.context,
                &prepared.remote_task_id,
                &prepared.draft,
                receipt.as_ref().map(|record| &record.receipt),
            )
        };
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let verification = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => self.renew_claims(job, execution_id).await?,
            }
        };
        let finished_at = Utc::now().max(now);
        match verification {
            Ok(verification) => {
                self.finish_recovered_submission_verification(
                    job,
                    attempt_id,
                    &prepared,
                    receipt.as_ref().map(|record| &record.receipt),
                    verification,
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

    #[allow(
        clippy::too_many_arguments,
        reason = "recovery persists one exact attempt, Draft, receipt and verification binding"
    )]
    async fn finish_recovered_submission_verification(
        &self,
        job: &ScheduledJob,
        attempt_id: asterism_domain::ExecutionAttemptId,
        prepared: &PreparedSubmissionCall,
        receipt: Option<&asterism_domain::SubmissionReceipt>,
        verification: SubmissionVerificationSnapshot,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if verification.validate().is_err()
            || (verification.status == SubmissionVerificationStatus::Confirmed
                && verification.remote_state != Some(RemoteState::Completed))
        {
            return self
                .defer_or_require_submission_review(
                    job,
                    ProviderErrorClass::ProtocolDrift,
                    at,
                    correlation_id,
                )
                .await;
        }
        if matches!(
            verification.status,
            SubmissionVerificationStatus::Pending | SubmissionVerificationStatus::Inconclusive
        ) && self.recovery_retry_at(job, at)?.is_some()
        {
            return self
                .defer_or_require_submission_review(
                    job,
                    ProviderErrorClass::InvalidRemoteState,
                    at,
                    correlation_id,
                )
                .await;
        }
        let status = match verification.status {
            SubmissionVerificationStatus::Confirmed => SubmissionResultStatus::Confirmed,
            SubmissionVerificationStatus::Rejected => SubmissionResultStatus::Rejected,
            SubmissionVerificationStatus::Pending | SubmissionVerificationStatus::Inconclusive => {
                SubmissionResultStatus::Inconclusive
            }
        };
        let result = SubmissionResult {
            id: SubmissionResultId::new(),
            submission_draft_id: prepared.draft.id,
            execution_id: claimed_execution_id(job)?,
            execution_attempt_id: attempt_id,
            task_id: prepared.draft.task_id,
            question_snapshot_id: prepared.draft.question_snapshot_id,
            provider_id: prepared.draft.provider_id.clone(),
            provider_version: prepared.provider_version.clone(),
            status,
            receipt: receipt.cloned(),
            verification,
            created_at: at,
        };
        result
            .validate_for_draft(&prepared.draft)
            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
        self.executions
            .persist_submission_result(SubmissionResultPersistRequest {
                result: &result,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                correlation_id,
                at,
            })
            .await?;
        let completion = self
            .record_submission_completion_observation(
                job,
                attempt_id,
                prepared,
                &result.verification,
                at,
                correlation_id,
            )
            .await?;
        self.finish_recovery_from_submission_result(
            job,
            &result,
            completion.workflow.workflow.state,
            at,
            correlation_id,
        )
        .await
    }

    async fn defer_or_require_submission_review(
        &self,
        job: &ScheduledJob,
        error_class: ProviderErrorClass,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if let Some(retry_at) = self.recovery_retry_at(job, at)? {
            self.defer_recovery(job, retry_at, at).await
        } else {
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
    }

    async fn finish_recovery_from_submission_result(
        &self,
        job: &ScheduledJob,
        result: &SubmissionResult,
        completion_state: StrictCompletionState,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let (state, error_class) = if completion_state == StrictCompletionState::Completed {
            (ExecutionState::Succeeded, None)
        } else if completion_state == StrictCompletionState::Active {
            (
                ExecutionState::HumanRequired,
                Some(ProviderErrorClass::InvalidRemoteState),
            )
        } else {
            match result.status {
                SubmissionResultStatus::Confirmed => (ExecutionState::Succeeded, None),
                SubmissionResultStatus::Rejected => (
                    ExecutionState::Failed,
                    Some(ProviderErrorClass::InvalidRemoteState),
                ),
                SubmissionResultStatus::ExecutionFailed | SubmissionResultStatus::Inconclusive => (
                    ExecutionState::HumanRequired,
                    Some(ProviderErrorClass::InvalidRemoteState),
                ),
            }
        };
        self.finish_recovery(job, state, error_class, None, at, correlation_id)
            .await
    }

    async fn recover_execution_by_progress(
        &self,
        job: &ScheduledJob,
        task: &Task,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let execution_id = claimed_execution_id(job)?;
        let prepared = match self
            .prepare_progress_recovery_call(execution_id, task, correlation_id)
            .await?
        {
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
        let _admission = self
            .acquire_admission(job, execution_id, &prepared.context, prepared.concurrency)
            .await?;
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
                _ = heartbeat.tick() => self.renew_claims(job, execution_id).await?,
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

    async fn prepare_progress_recovery_call(
        &self,
        execution_id: ExecutionId,
        task: &Task,
        correlation_id: &str,
    ) -> Result<Result<PreparedProgressRecoveryCall, ProviderErrorClass>, ScheduledExecutionRunError>
    {
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
        let Some(entry) = self.registry.get(&account.provider_id) else {
            return Ok(Err(ProviderErrorClass::UnsupportedTask));
        };
        let Some(capability) = entry.task_progress.clone() else {
            return Ok(Err(ProviderErrorClass::UnsupportedTask));
        };
        let Some(runtime_settings) = self
            .executions
            .find_execution_runtime_settings(execution_id)
            .await?
        else {
            return Ok(Err(ProviderErrorClass::Internal));
        };
        if runtime_settings.provider_id != account.provider_id {
            return Ok(Err(ProviderErrorClass::Internal));
        }
        let Ok(resolved_runtime_settings) = entry
            .runtime_settings
            .hydrate_frozen_core_defaults(&runtime_settings.resolved)
        else {
            return Ok(Err(ProviderErrorClass::Internal));
        };
        let Ok(concurrency) = entry
            .runtime_settings
            .execution_concurrency(&resolved_runtime_settings)
        else {
            return Ok(Err(ProviderErrorClass::Internal));
        };
        Ok(Ok(PreparedProgressRecoveryCall {
            capability,
            context: ProviderContext {
                provider_id: account.provider_id,
                account_id: account.id,
                credential_refs: account.credential_refs,
                correlation_id: correlation_id.to_owned(),
            },
            concurrency,
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
                let execution_id = claimed_execution_id(job)?;
                let execution_attempt_id = self
                    .executions
                    .find_active_execution_attempt_id(execution_id)
                    .await?
                    .ok_or(ScheduledExecutionRunError::StateConflict)?;
                self.record_completion_observation(
                    job,
                    execution_attempt_id,
                    execution_id,
                    crate::CompletionObservation {
                        outcome: Some(asterism_domain::CompletionOutcome::Completed),
                        diagnosis: None,
                    },
                    at,
                    correlation_id,
                )
                .await?;
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

    async fn finish_from_execution_verification(
        &self,
        job: &ScheduledJob,
        execution_attempt_id: asterism_domain::ExecutionAttemptId,
        prepared: &PreparedProviderCall,
        verification: &asterism_provider_api::ExecutionOutcome,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let reliable = verification.verified && verification.validate().is_ok();
        if execution_goal_verified(&prepared.request.requested_capabilities, verification) {
            self.record_provider_completion_observation_for_attempt(
                job,
                execution_attempt_id,
                claimed_execution_id(job)?,
                prepared,
                verification,
                at,
                correlation_id,
            )
            .await?;
            return self
                .finish_recovery(
                    job,
                    ExecutionState::Succeeded,
                    None,
                    None,
                    at,
                    correlation_id,
                )
                .await;
        }
        match verification.remote_state {
            RemoteState::Pending | RemoteState::InProgress | RemoteState::Completed => {
                if let Some(retry_at) = self.recovery_retry_at(job, at)? {
                    self.defer_recovery(job, retry_at, at).await
                } else {
                    if reliable {
                        self.record_provider_completion_observation_for_attempt(
                            job,
                            execution_attempt_id,
                            claimed_execution_id(job)?,
                            prepared,
                            verification,
                            at,
                            correlation_id,
                        )
                        .await?;
                    }
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
                if reliable {
                    self.record_provider_completion_observation_for_attempt(
                        job,
                        execution_attempt_id,
                        claimed_execution_id(job)?,
                        prepared,
                        verification,
                        at,
                        correlation_id,
                    )
                    .await?;
                }
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
            ExecutionState::Failed => (
                None,
                ExecutionStage::Finalizing,
                "remote submission was rejected during recovery",
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
            ExecutionState::Failed => ScheduledExecutionOutcome::Failed {
                execution,
                error_class: error_class.expect("failed recovery has an error class"),
            },
            _ => unreachable!("recovery finish states were validated"),
        })
    }

    async fn execute_attempt(
        &self,
        job: &ScheduledJob,
        execution: &Execution,
        task: &Task,
        attempt: &ExecutionAttempt,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if execution.requested_capabilities == [TaskCapability::SubmissionExecute] {
            return self
                .execute_submission_attempt(job, execution, task, attempt, now, correlation_id)
                .await;
        }
        if execution.requested_capabilities.len() > 1 {
            return self
                .execute_composite_attempt(job, execution, task, attempt, now, correlation_id)
                .await;
        }
        let duration_only = duration_report_only(&execution.requested_capabilities);
        let prepared = match self
            .prepare_provider_call(
                attempt.execution_id,
                task,
                &execution.requested_capabilities,
                &execution.requested_capabilities,
                1,
                correlation_id,
            )
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
        if task.remote_state == RemoteState::Completed && !prepared.verification && !duration_only {
            return self.finish_success(job, attempt, now, correlation_id).await;
        }
        let provider_state_exception = prepared.capability.allows_execution_from_remote_state(
            &execution.requested_capabilities,
            task.remote_state,
        );
        if !(matches!(
            task.remote_state,
            RemoteState::Pending | RemoteState::InProgress
        ) || duration_only
            && matches!(
                task.remote_state,
                RemoteState::Unknown | RemoteState::Completed
            )
            || prepared.verification
                && matches!(
                    task.remote_state,
                    RemoteState::Unknown | RemoteState::Completed
                )
            || provider_state_exception
                && !matches!(
                    task.remote_state,
                    RemoteState::Expired | RemoteState::Removed
                ))
        {
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
        if task.remote_state == RemoteState::Completed && prepared.verification {
            return self
                .verify_task_execution_without_mutation(
                    job,
                    task,
                    attempt,
                    &prepared,
                    correlation_id,
                )
                .await;
        }
        if task.remote_state == RemoteState::Unknown && !duration_only && !prepared.verification {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::UnsupportedTask,
                    FailureDisposition::Failed,
                    now,
                    correlation_id,
                )
                .await;
        }
        self.call_provider(job, task, attempt, &prepared, correlation_id)
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "composite execution keeps each durable issue, remote call, verification and success boundary explicit"
    )]
    async fn execute_composite_attempt(
        &self,
        job: &ScheduledJob,
        execution: &Execution,
        task: &Task,
        attempt: &ExecutionAttempt,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let steps = self
            .executions
            .find_execution_capability_steps(execution.id)
            .await?;
        if !capability_steps_match_execution(execution, &steps) {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::Internal,
                    FailureDisposition::HumanRequired,
                    now,
                    correlation_id,
                )
                .await;
        }
        let capability_plan = steps.iter().map(|step| step.capability).collect::<Vec<_>>();
        let Some(calls) = capability_calls(&steps) else {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::Internal,
                    FailureDisposition::HumanRequired,
                    now,
                    correlation_id,
                )
                .await;
        };
        for call in &calls {
            if call.state == ExecutionCapabilityStepState::Succeeded {
                continue;
            }
            if call.state != ExecutionCapabilityStepState::Pending {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        ProviderErrorClass::InvalidRemoteState,
                        correlation_id,
                    )
                    .await;
            }
            let prepared = match self
                .prepare_provider_call(
                    execution.id,
                    task,
                    &call.capabilities,
                    &capability_plan,
                    call.first_step_position,
                    correlation_id,
                )
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
                            Utc::now().max(now),
                            correlation_id,
                        )
                        .await;
                }
            };
            let ordinary_state = matches!(
                task.remote_state,
                RemoteState::Pending | RemoteState::InProgress
            ) || duration_report_only(
                &prepared.request.requested_capabilities,
            ) && matches!(
                task.remote_state,
                RemoteState::Unknown | RemoteState::Completed
            ) || prepared.verification
                && matches!(
                    task.remote_state,
                    RemoteState::Unknown | RemoteState::Completed
                );
            let provider_state_exception = prepared.capability.allows_execution_from_remote_state(
                &prepared.request.requested_capabilities,
                task.remote_state,
            );
            if !(ordinary_state
                || provider_state_exception
                    && !matches!(
                        task.remote_state,
                        RemoteState::Expired | RemoteState::Removed
                    ))
            {
                return self
                    .finish_failure(
                        job,
                        attempt,
                        ProviderErrorClass::InvalidRemoteState,
                        FailureDisposition::Failed,
                        Utc::now().max(now),
                        correlation_id,
                    )
                    .await;
            }
            let issued_at = Utc::now().max(now);
            let issue = if call.capabilities.len() == 1 {
                self.executions
                    .issue_execution_capability_step(ExecutionCapabilityStepMutation {
                        execution_id: execution.id,
                        attempt_id: attempt.id,
                        capability: call.capabilities[0],
                        scheduler_job_id: job.id,
                        worker_id: claimed_worker(job)?,
                        correlation_id,
                        at: issued_at,
                    })
                    .await?
            } else {
                self.executions
                    .issue_execution_capability_call(ExecutionCapabilityCallMutation {
                        execution_id: execution.id,
                        attempt_id: attempt.id,
                        call_position: call.position,
                        capabilities: &call.capabilities,
                        scheduler_job_id: job.id,
                        worker_id: claimed_worker(job)?,
                        correlation_id,
                        at: issued_at,
                    })
                    .await?
            };
            if issue != ExecutionCapabilityStepIssueOutcome::Issued {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        ProviderErrorClass::InvalidRemoteState,
                        correlation_id,
                    )
                    .await;
            }
            let _admission = self
                .acquire_admission(
                    job,
                    attempt.execution_id,
                    &prepared.context,
                    prepared.concurrency,
                )
                .await?;
            let claim_lost = Arc::new(AtomicBool::new(false));
            let sink = PersistedExecutionEventSink {
                executions: &self.executions,
                execution_id: attempt.execution_id,
                attempt_id: attempt.id,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                correlation_id,
                provider_id: prepared.context.provider_id.clone(),
                mutations_enabled: true,
                claim_lost: Arc::clone(&claim_lost),
            };
            let mutation = prepared
                .capability
                .execute(&prepared.context, &prepared.request, &sink);
            tokio::pin!(mutation);
            let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            heartbeat.tick().await;
            let mutation = loop {
                tokio::select! {
                    result = &mut mutation => break result,
                    _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
                }
            };
            if claim_lost.load(Ordering::Acquire) {
                return Err(ScheduledExecutionRunError::ClaimLost);
            }
            let mutation = match mutation {
                Ok(outcome) => outcome,
                Err(error) => {
                    return self
                        .begin_verification_recovery(
                            job,
                            attempt,
                            provider_error_class(&error),
                            correlation_id,
                        )
                        .await;
                }
            };
            let verified_outcome = if prepared.verification {
                let verification = prepared
                    .capability
                    .verify_execution(&prepared.context, &prepared.request);
                tokio::pin!(verification);
                let verification = loop {
                    tokio::select! {
                        result = &mut verification => break result,
                        _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
                    }
                };
                match verification {
                    Ok(outcome)
                        if execution_goal_verified(
                            &prepared.request.requested_capabilities,
                            &outcome,
                        ) =>
                    {
                        outcome
                    }
                    Ok(_) => {
                        return self
                            .begin_verification_recovery(
                                job,
                                attempt,
                                ProviderErrorClass::InvalidRemoteState,
                                correlation_id,
                            )
                            .await;
                    }
                    Err(error) => {
                        return self
                            .begin_verification_recovery(
                                job,
                                attempt,
                                provider_error_class(&error),
                                correlation_id,
                            )
                            .await;
                    }
                }
            } else {
                if !execution_goal_verified(&prepared.request.requested_capabilities, &mutation) {
                    return self
                        .begin_verification_recovery(
                            job,
                            attempt,
                            ProviderErrorClass::InvalidRemoteState,
                            correlation_id,
                        )
                        .await;
                }
                mutation
            };
            let succeeded_at = Utc::now().max(issued_at);
            if call.capabilities.len() == 1 {
                self.executions
                    .succeed_execution_capability_step(ExecutionCapabilityStepMutation {
                        execution_id: execution.id,
                        attempt_id: attempt.id,
                        capability: call.capabilities[0],
                        scheduler_job_id: job.id,
                        worker_id: claimed_worker(job)?,
                        correlation_id,
                        at: succeeded_at,
                    })
                    .await?;
            } else {
                self.executions
                    .succeed_execution_capability_call(ExecutionCapabilityCallMutation {
                        execution_id: execution.id,
                        attempt_id: attempt.id,
                        call_position: call.position,
                        capabilities: &call.capabilities,
                        scheduler_job_id: job.id,
                        worker_id: claimed_worker(job)?,
                        correlation_id,
                        at: succeeded_at,
                    })
                    .await?;
            }
            if call.position
                == calls
                    .last()
                    .expect("composite calls are non-empty")
                    .position
            {
                self.record_provider_completion_observation(
                    job,
                    attempt,
                    &prepared,
                    &verified_outcome,
                    succeeded_at,
                    correlation_id,
                )
                .await?;
            }
        }
        self.finish_success(
            job,
            attempt,
            Utc::now().max(attempt.started_at),
            correlation_id,
        )
        .await
    }

    async fn execute_submission_attempt(
        &self,
        job: &ScheduledJob,
        execution: &Execution,
        task: &Task,
        attempt: &ExecutionAttempt,
        now: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if !matches!(
            task.remote_state,
            RemoteState::Pending
                | RemoteState::InProgress
                | RemoteState::Completed
                | RemoteState::Unknown
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
            .prepare_submission_call(
                attempt.execution_id,
                task,
                &execution.requested_capabilities,
                correlation_id,
            )
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
        if task.remote_state == RemoteState::Completed {
            return self
                .verify_submission_without_mutation(job, attempt, &prepared, None, correlation_id)
                .await;
        }
        self.call_submission(job, attempt, &prepared, correlation_id)
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "submission preparation fails closed across every account, capability, Draft and settings binding"
    )]
    async fn prepare_submission_call(
        &self,
        execution_id: ExecutionId,
        task: &Task,
        requested_capabilities: &[TaskCapability],
        correlation_id: &str,
    ) -> Result<Result<PreparedSubmissionCall, PreparedFailure>, ScheduledExecutionRunError> {
        if authorize_execution(
            task,
            requested_capabilities,
            self.config.formal_assessment_policy,
        )
        .is_err()
        {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::Authorization,
                FailureDisposition::HumanRequired,
            )));
        }
        if requested_capabilities != [TaskCapability::SubmissionExecute]
            || !task
                .capabilities
                .contains(&TaskCapability::SubmissionVerify)
        {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::UnsupportedTask,
                FailureDisposition::Failed,
            )));
        }
        let Some(account) = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        if account.auth_state != AuthState::Authenticated {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::Authentication,
                FailureDisposition::HumanRequired,
            )));
        }
        let Some(entry) = self.registry.get(&account.provider_id) else {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::UnsupportedTask,
                FailureDisposition::Failed,
            )));
        };
        let (Some(execute), Some(verify)) = (
            entry.submission_execute.clone(),
            entry.submission_verify.clone(),
        ) else {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::UnsupportedTask,
                FailureDisposition::Failed,
            )));
        };
        let Some(runtime_settings) = self
            .executions
            .find_execution_runtime_settings(execution_id)
            .await?
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        let Some(draft) = self
            .executions
            .find_execution_submission_draft(execution_id)
            .await?
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        if runtime_settings.provider_id != account.provider_id
            || draft.task_id != task.id
            || draft.provider_id != account.provider_id
            || draft.provider_version != entry.metadata.implementation_version
            || draft.validate().is_err()
        {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Internal,
                disposition: FailureDisposition::HumanRequired,
            }));
        }
        let Ok(resolved_runtime_settings) = entry
            .runtime_settings
            .hydrate_frozen_core_defaults(&runtime_settings.resolved)
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        let Ok(concurrency) = entry
            .runtime_settings
            .execution_concurrency(&resolved_runtime_settings)
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        Ok(Ok(PreparedSubmissionCall {
            execute,
            verify,
            context: ProviderContext {
                provider_id: account.provider_id,
                account_id: account.id,
                credential_refs: account.credential_refs,
                correlation_id: correlation_id.to_owned(),
            },
            remote_task_id: task.remote_id.clone(),
            draft,
            runtime_settings: resolved_runtime_settings,
            concurrency,
            provider_version: entry.metadata.implementation_version.clone(),
        }))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Provider preparation keeps authorization, persisted plan evidence and account/settings binding visible in one fail-closed chain"
    )]
    async fn prepare_provider_call(
        &self,
        execution_id: ExecutionId,
        task: &Task,
        requested_capabilities: &[TaskCapability],
        capability_plan: &[TaskCapability],
        capability_step_position: u8,
        correlation_id: &str,
    ) -> Result<Result<PreparedProviderCall, PreparedFailure>, ScheduledExecutionRunError> {
        if authorize_execution(
            task,
            requested_capabilities,
            self.config.formal_assessment_policy,
        )
        .is_err()
        {
            return Ok(Err(PreparedFailure {
                error_class: ProviderErrorClass::Authorization,
                disposition: FailureDisposition::HumanRequired,
            }));
        }
        let capabilities = requested_capabilities.to_vec();
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
            return Ok(Err(internal_prepared_failure()));
        };
        if account.auth_state != AuthState::Authenticated {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::Authentication,
                FailureDisposition::HumanRequired,
            )));
        }
        let Some(entry) = self.registry.get(&account.provider_id) else {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::UnsupportedTask,
                FailureDisposition::Failed,
            )));
        };
        let Some(capability) = entry.task_execution.clone() else {
            return Ok(Err(prepared_failure(
                ProviderErrorClass::UnsupportedTask,
                FailureDisposition::Failed,
            )));
        };
        let verification = match execution_verification(task, &capabilities, entry, &capability) {
            Ok(verification) => verification,
            Err(failure) => return Ok(Err(failure)),
        };
        let Some(runtime_settings) = self
            .executions
            .find_execution_runtime_settings(execution_id)
            .await?
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        if runtime_settings.provider_id != account.provider_id {
            return Ok(Err(internal_prepared_failure()));
        }
        let provider_plan_artifact = self
            .executions
            .find_execution_provider_plan_artifact(execution_id)
            .await?;
        if provider_plan_artifact
            .as_ref()
            .is_some_and(|artifact| artifact.provider_id() != &account.provider_id)
        {
            return Ok(Err(internal_prepared_failure()));
        }
        let Ok(resolved_runtime_settings) = entry
            .runtime_settings
            .hydrate_frozen_core_defaults(&runtime_settings.resolved)
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        let Ok(concurrency) = entry
            .runtime_settings
            .execution_concurrency(&resolved_runtime_settings)
        else {
            return Ok(Err(internal_prepared_failure()));
        };
        let request = ProviderExecutionRequest {
            execution_id,
            task_id: task.id,
            remote_task_id: task.remote_id.clone(),
            course_id: task.course_id,
            requested_capabilities: capabilities,
            capability_plan: capability_plan.to_vec(),
            capability_step_position,
            runtime_settings: resolved_runtime_settings,
            provider_plan_artifact,
        };
        if !request.has_valid_capability_step() {
            return Ok(Err(internal_prepared_failure()));
        }
        Ok(Ok(PreparedProviderCall {
            capability,
            verification,
            context: ProviderContext {
                provider_id: account.provider_id,
                account_id: account.id,
                credential_refs: account.credential_refs,
                correlation_id: correlation_id.to_owned(),
            },
            request,
            concurrency,
        }))
    }

    async fn record_provider_completion_observation(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedProviderCall,
        outcome: &asterism_provider_api::ExecutionOutcome,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<StrictCompletionExecutionObservationRecord, ScheduledExecutionRunError> {
        self.record_provider_completion_observation_for_attempt(
            job,
            attempt.id,
            attempt.execution_id,
            prepared,
            outcome,
            at,
            correlation_id,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "completion observation retains exact worker, attempt, Provider request and timestamp bindings"
    )]
    async fn record_provider_completion_observation_for_attempt(
        &self,
        job: &ScheduledJob,
        execution_attempt_id: asterism_domain::ExecutionAttemptId,
        execution_id: ExecutionId,
        prepared: &PreparedProviderCall,
        outcome: &asterism_provider_api::ExecutionOutcome,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<StrictCompletionExecutionObservationRecord, ScheduledExecutionRunError> {
        let diagnosis = prepared
            .capability
            .completion_diagnosis(&prepared.request, outcome);
        let observation = crate::observe_execution_completion(outcome, diagnosis)
            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
        self.record_completion_observation(
            job,
            execution_attempt_id,
            execution_id,
            observation,
            at,
            correlation_id,
        )
        .await
    }

    async fn record_submission_completion_observation(
        &self,
        job: &ScheduledJob,
        execution_attempt_id: asterism_domain::ExecutionAttemptId,
        prepared: &PreparedSubmissionCall,
        verification: &SubmissionVerificationSnapshot,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<StrictCompletionExecutionObservationRecord, ScheduledExecutionRunError> {
        let diagnosis = prepared.verify.completion_diagnosis(verification);
        let observation = crate::observe_submission_completion(verification, diagnosis)
            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
        self.record_completion_observation(
            job,
            execution_attempt_id,
            claimed_execution_id(job)?,
            observation,
            at,
            correlation_id,
        )
        .await
    }

    async fn record_completion_observation(
        &self,
        job: &ScheduledJob,
        execution_attempt_id: asterism_domain::ExecutionAttemptId,
        execution_id: ExecutionId,
        observation: crate::CompletionObservation,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<StrictCompletionExecutionObservationRecord, ScheduledExecutionRunError> {
        let record = self
            .executions
            .record_strict_completion_execution_observation(
                StrictCompletionExecutionObservationRequest {
                    execution_id,
                    execution_attempt_id,
                    scheduler_job_id: job.id,
                    worker_id: claimed_worker(job)?,
                    outcome: observation.outcome,
                    diagnosis: observation.diagnosis,
                    at,
                    correlation_id,
                },
            )
            .await?;
        Ok(record)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the Provider call keeps lease renewal, persisted events and the verify-only mutation boundary visible together"
    )]
    async fn call_provider(
        &self,
        job: &ScheduledJob,
        task: &Task,
        attempt: &ExecutionAttempt,
        prepared: &PreparedProviderCall,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let now = attempt.started_at;
        let _admission = self
            .acquire_admission(
                job,
                attempt.execution_id,
                &prepared.context,
                prepared.concurrency,
            )
            .await?;
        let claim_lost = Arc::new(AtomicBool::new(false));
        let sink = PersistedExecutionEventSink {
            executions: &self.executions,
            execution_id: attempt.execution_id,
            attempt_id: attempt.id,
            scheduler_job_id: job.id,
            worker_id: claimed_worker(job)?,
            correlation_id,
            provider_id: prepared.context.provider_id.clone(),
            mutations_enabled: true,
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
        if prepared.verification {
            return match result {
                Ok(_) => {
                    self.verify_task_execution_claimed(job, task, attempt, prepared, correlation_id)
                        .await
                }
                Err(error) => {
                    self.begin_verification_recovery(
                        job,
                        attempt,
                        provider_error_class(&error),
                        correlation_id,
                    )
                    .await
                }
            };
        }
        match result {
            Ok(outcome)
                if execution_goal_verified(&prepared.request.requested_capabilities, &outcome) =>
            {
                let at = Utc::now().max(now);
                let completion = self
                    .record_provider_completion_observation(
                        job,
                        attempt,
                        prepared,
                        &outcome,
                        at,
                        correlation_id,
                    )
                    .await?;
                self.finish_after_completion_observation(
                    job,
                    task,
                    attempt,
                    completion,
                    at,
                    correlation_id,
                )
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
                let (error_class, disposition) =
                    if duration_report_only(&prepared.request.requested_capabilities) {
                        (
                            provider_error_class(&error),
                            FailureDisposition::HumanRequired,
                        )
                    } else {
                        classify_provider_error(
                            &error,
                            attempt.attempt_no,
                            Utc::now().max(now),
                            self.config.retry_policy,
                        )?
                    };
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

    async fn verify_task_execution_without_mutation(
        &self,
        job: &ScheduledJob,
        task: &Task,
        attempt: &ExecutionAttempt,
        prepared: &PreparedProviderCall,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let _admission = self
            .acquire_admission(
                job,
                attempt.execution_id,
                &prepared.context,
                prepared.concurrency,
            )
            .await?;
        self.verify_task_execution_claimed(job, task, attempt, prepared, correlation_id)
            .await
    }

    async fn verify_task_execution_claimed(
        &self,
        job: &ScheduledJob,
        task: &Task,
        attempt: &ExecutionAttempt,
        prepared: &PreparedProviderCall,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let provider = prepared
            .capability
            .verify_execution(&prepared.context, &prepared.request);
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
            }
        };
        match result {
            Ok(verification)
                if execution_goal_verified(
                    &prepared.request.requested_capabilities,
                    &verification,
                ) =>
            {
                let at = Utc::now().max(attempt.started_at);
                let completion = self
                    .record_provider_completion_observation(
                        job,
                        attempt,
                        prepared,
                        &verification,
                        at,
                        correlation_id,
                    )
                    .await?;
                self.finish_after_completion_observation(
                    job,
                    task,
                    attempt,
                    completion,
                    at,
                    correlation_id,
                )
                .await
            }
            Ok(_) => {
                self.begin_verification_recovery(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    correlation_id,
                )
                .await
            }
            Err(error) => {
                self.begin_verification_recovery(
                    job,
                    attempt,
                    provider_error_class(&error),
                    correlation_id,
                )
                .await
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the entry keeps shared admission and legacy/durable submission dispatch visible"
    )]
    async fn call_submission(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedSubmissionCall,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let _admission = self
            .acquire_admission(
                job,
                attempt.execution_id,
                &prepared.context,
                prepared.concurrency,
            )
            .await?;
        let claim_lost = Arc::new(AtomicBool::new(false));
        let sink = PersistedExecutionEventSink {
            executions: &self.executions,
            execution_id: attempt.execution_id,
            attempt_id: attempt.id,
            scheduler_job_id: job.id,
            worker_id: claimed_worker(job)?,
            correlation_id,
            provider_id: prepared.context.provider_id.clone(),
            mutations_enabled: false,
            claim_lost: Arc::clone(&claim_lost),
        };
        if let Some(factory) = self.question_sessions.as_ref() {
            let artifacts = factory.for_provider(prepared.context.provider_id.clone());
            let access = SecretAccess {
                actor: SecretActor::CoreService("execution-worker"),
                correlation_id: correlation_id.to_owned(),
                reason: "durable QuestionSession submission".to_owned(),
            };
            if let Some(resolved) = artifacts
                .resolve_question_session_continuation(attempt.execution_id, &access)
                .await?
            {
                return self
                    .call_durable_submission_claimed(
                        job,
                        attempt,
                        prepared,
                        artifacts,
                        resolved,
                        &access,
                        &sink,
                        &claim_lost,
                        correlation_id,
                    )
                    .await;
            }
        }
        let provider = prepared.execute.execute_submission(
            &prepared.context,
            &prepared.remote_task_id,
            &prepared.draft,
            &prepared.runtime_settings,
            &sink,
        );
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
            }
        };
        if claim_lost.load(Ordering::Acquire) {
            return Err(ScheduledExecutionRunError::ClaimLost);
        }
        let receipt = match result {
            Ok(receipt) if receipt.validate().is_ok() => receipt,
            Ok(_) => {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        ProviderErrorClass::ProtocolDrift,
                        correlation_id,
                    )
                    .await;
            }
            Err(error) => {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        provider_error_class(&error),
                        correlation_id,
                    )
                    .await;
            }
        };
        let persisted_at = Utc::now().max(attempt.started_at);
        let record = SubmissionAttemptReceipt {
            submission_draft_id: prepared.draft.id,
            execution_id: attempt.execution_id,
            execution_attempt_id: attempt.id,
            receipt: receipt.clone(),
        };
        record
            .validate_for_draft(&prepared.draft)
            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
        self.executions
            .persist_submission_receipt(SubmissionReceiptPersistRequest {
                record: &record,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                correlation_id,
                at: persisted_at,
            })
            .await?;
        self.verify_submission_claimed(job, attempt, prepared, Some(&receipt), correlation_id)
            .await
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the durable loop keeps resolve, issue-before-send, ambiguity, continuation rotation and receipt persistence in one auditable path"
    )]
    async fn call_durable_submission_claimed(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedSubmissionCall,
        artifacts: Arc<dyn QuestionSessionArtifactRepository>,
        mut resolved: ResolvedQuestionSessionContinuation,
        access: &SecretAccess,
        sink: &(dyn ExecutionEventSink + Send + Sync),
        claim_lost: &AtomicBool,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        for _ in 0..MAX_DURABLE_SUBMISSION_OPERATIONS {
            if let Some(transition) = resolved.latest_transition.clone() {
                return self
                    .finish_next_question_step(
                        job,
                        attempt.execution_id,
                        attempt.id,
                        &transition,
                        Utc::now().max(transition.transitioned_at),
                        correlation_id,
                    )
                    .await;
            }
            if let Some(latest) = resolved.latest_operation.clone() {
                if latest.state == QuestionSessionOperationState::Accepted
                    && latest.continuation_revision == resolved.metadata.revision
                {
                    return self
                        .verify_submission_claimed(job, attempt, prepared, None, correlation_id)
                        .await;
                }
                match latest.state {
                    QuestionSessionOperationState::Issued => {
                        let finished = artifacts
                            .finish_question_session_operation(
                                &latest,
                                QuestionSessionOperationState::Ambiguous,
                                None,
                                Utc::now().max(latest.issued_at),
                                correlation_id,
                            )
                            .await?;
                        if !matches!(
                            finished,
                            QuestionSessionOperationFinishOutcome::Finished(_)
                                | QuestionSessionOperationFinishOutcome::Duplicate(_)
                        ) {
                            return Err(ScheduledExecutionRunError::StateConflict);
                        }
                        return self
                            .begin_verification_recovery(
                                job,
                                attempt,
                                ProviderErrorClass::Network,
                                correlation_id,
                            )
                            .await;
                    }
                    QuestionSessionOperationState::Ambiguous => {
                        let recovered = {
                            let continuation = provider_session_continuation(&resolved);
                            let ambiguous = provider_ambiguous_operation(&latest);
                            let provider = prepared.execute.recover_ambiguous_submission_operation(
                                &prepared.context,
                                &prepared.remote_task_id,
                                &prepared.draft,
                                continuation,
                                &ambiguous,
                                &prepared.runtime_settings,
                            );
                            tokio::pin!(provider);
                            let mut heartbeat =
                                tokio::time::interval(self.config.heartbeat_interval);
                            heartbeat
                                .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                            heartbeat.tick().await;
                            loop {
                                tokio::select! {
                                    result = &mut provider => break result,
                                    _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
                                }
                            }
                        };
                        if claim_lost.load(Ordering::Acquire) {
                            return Err(ScheduledExecutionRunError::ClaimLost);
                        }
                        let recovered = match recovered {
                            Ok(recovered) => recovered,
                            Err(error) => {
                                return self
                                    .begin_verification_recovery(
                                        job,
                                        attempt,
                                        provider_error_class(&error),
                                        correlation_id,
                                    )
                                    .await;
                            }
                        };
                        let Some(outcome) = recovered else {
                            return self
                                .begin_verification_recovery(
                                    job,
                                    attempt,
                                    ProviderErrorClass::Network,
                                    correlation_id,
                                )
                                .await;
                        };
                        match accept_durable_submission_outcome(
                            artifacts.as_ref(),
                            &latest,
                            outcome,
                            access,
                            prepared,
                        )
                        .await?
                        {
                            AcceptedDurableSubmissionOutcome::Submitted(receipt) => {
                                return self
                                    .persist_durable_submission_receipt_and_verify(
                                        job,
                                        attempt,
                                        prepared,
                                        receipt,
                                        correlation_id,
                                    )
                                    .await;
                            }
                            AcceptedDurableSubmissionOutcome::NextQuestion(transition) => {
                                return self
                                    .finish_next_question_step(
                                        job,
                                        attempt.execution_id,
                                        attempt.id,
                                        &transition,
                                        Utc::now().max(transition.transitioned_at),
                                        correlation_id,
                                    )
                                    .await;
                            }
                            AcceptedDurableSubmissionOutcome::Continue => {}
                        }
                        resolved = artifacts
                            .resolve_question_session_continuation(attempt.execution_id, access)
                            .await?
                            .ok_or(ScheduledExecutionRunError::StateConflict)?;
                        continue;
                    }
                    QuestionSessionOperationState::Rejected => {
                        return self
                            .finish_failure(
                                job,
                                attempt,
                                ProviderErrorClass::InvalidRemoteState,
                                FailureDisposition::Failed,
                                Utc::now().max(attempt.started_at),
                                correlation_id,
                            )
                            .await;
                    }
                    QuestionSessionOperationState::Accepted => {}
                }
            }

            let continuation = provider_session_continuation(&resolved);
            let prepared_operation = prepared
                .execute
                .prepare_submission_operation(
                    &prepared.context,
                    &prepared.remote_task_id,
                    &prepared.draft,
                    continuation,
                    &prepared.runtime_settings,
                )
                .await;
            let Some(operation) = (match prepared_operation {
                Ok(operation) => operation,
                Err(error) => {
                    return self
                        .begin_verification_recovery(
                            job,
                            attempt,
                            provider_error_class(&error),
                            correlation_id,
                        )
                        .await;
                }
            }) else {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        ProviderErrorClass::ProtocolDrift,
                        correlation_id,
                    )
                    .await;
            };
            let operation_type = operation.operation_type().to_owned();
            let request_digest = operation.request_digest();
            let delay_seconds = operation.delay_before_execute_seconds();
            if !valid_question_session_label(&prepared.context.provider_id, &operation_type)
                || request_digest == [0; 32]
                || delay_seconds > MAX_DURABLE_SUBMISSION_DELAY_SECONDS
            {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        ProviderErrorClass::ProtocolDrift,
                        correlation_id,
                    )
                    .await;
            }
            let issued_at = Utc::now().max(resolved.metadata.updated_at);
            let issued = match artifacts
                .issue_question_session_operation(QuestionSessionOperationIssueRequest {
                    execution_id: attempt.execution_id,
                    execution_attempt_id: attempt.id,
                    expected_continuation_revision: resolved.metadata.revision,
                    operation_type,
                    request_digest,
                    issued_at,
                    correlation_id: correlation_id.to_owned(),
                })
                .await?
            {
                QuestionSessionOperationIssueOutcome::Issued(operation) => operation,
                QuestionSessionOperationIssueOutcome::Duplicate(_)
                | QuestionSessionOperationIssueOutcome::Conflict
                | QuestionSessionOperationIssueOutcome::Unavailable => {
                    return self
                        .begin_verification_recovery(
                            job,
                            attempt,
                            ProviderErrorClass::Network,
                            correlation_id,
                        )
                        .await;
                }
            };
            if delay_seconds > 0 {
                let delay = tokio::time::sleep(StdDuration::from_secs(delay_seconds));
                tokio::pin!(delay);
                let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                heartbeat.tick().await;
                loop {
                    tokio::select! {
                        () = &mut delay => break,
                        _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
                    }
                }
            }
            let provider = operation.execute(&prepared.context, sink);
            tokio::pin!(provider);
            let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            heartbeat.tick().await;
            let outcome = loop {
                tokio::select! {
                    result = &mut provider => break result,
                    _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
                }
            };
            if claim_lost.load(Ordering::Acquire) {
                return Err(ScheduledExecutionRunError::ClaimLost);
            }
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    let finished = artifacts
                        .finish_question_session_operation(
                            &issued,
                            QuestionSessionOperationState::Ambiguous,
                            None,
                            Utc::now().max(issued.issued_at),
                            correlation_id,
                        )
                        .await?;
                    if !matches!(
                        finished,
                        QuestionSessionOperationFinishOutcome::Finished(_)
                            | QuestionSessionOperationFinishOutcome::Duplicate(_)
                    ) {
                        return Err(ScheduledExecutionRunError::StateConflict);
                    }
                    return self
                        .begin_verification_recovery(
                            job,
                            attempt,
                            provider_error_class(&error),
                            correlation_id,
                        )
                        .await;
                }
            };
            match accept_durable_submission_outcome(
                artifacts.as_ref(),
                &issued,
                outcome,
                access,
                prepared,
            )
            .await?
            {
                AcceptedDurableSubmissionOutcome::Submitted(receipt) => {
                    return self
                        .persist_durable_submission_receipt_and_verify(
                            job,
                            attempt,
                            prepared,
                            receipt,
                            correlation_id,
                        )
                        .await;
                }
                AcceptedDurableSubmissionOutcome::NextQuestion(transition) => {
                    return self
                        .finish_next_question_step(
                            job,
                            attempt.execution_id,
                            attempt.id,
                            &transition,
                            Utc::now().max(transition.transitioned_at),
                            correlation_id,
                        )
                        .await;
                }
                AcceptedDurableSubmissionOutcome::Continue => {}
            }
            resolved = artifacts
                .resolve_question_session_continuation(attempt.execution_id, access)
                .await?
                .ok_or(ScheduledExecutionRunError::StateConflict)?;
        }
        self.begin_verification_recovery(
            job,
            attempt,
            ProviderErrorClass::ProtocolDrift,
            correlation_id,
        )
        .await
    }

    async fn persist_durable_submission_receipt_and_verify(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedSubmissionCall,
        receipt: asterism_domain::SubmissionReceipt,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let record = SubmissionAttemptReceipt {
            submission_draft_id: prepared.draft.id,
            execution_id: attempt.execution_id,
            execution_attempt_id: attempt.id,
            receipt: receipt.clone(),
        };
        record
            .validate_for_draft(&prepared.draft)
            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
        self.executions
            .persist_submission_receipt(SubmissionReceiptPersistRequest {
                record: &record,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                correlation_id,
                at: Utc::now().max(attempt.started_at),
            })
            .await?;
        self.verify_submission_claimed(job, attempt, prepared, Some(&receipt), correlation_id)
            .await
    }

    async fn verify_submission_without_mutation(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedSubmissionCall,
        receipt: Option<&asterism_domain::SubmissionReceipt>,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let _admission = self
            .acquire_admission(
                job,
                attempt.execution_id,
                &prepared.context,
                prepared.concurrency,
            )
            .await?;
        self.verify_submission_claimed(job, attempt, prepared, receipt, correlation_id)
            .await
    }

    async fn verify_submission_claimed(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedSubmissionCall,
        receipt: Option<&asterism_domain::SubmissionReceipt>,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let artifacts = self
            .question_sessions
            .as_ref()
            .map(|factory| factory.for_provider(prepared.context.provider_id.clone()));
        let access = SecretAccess {
            actor: SecretActor::CoreService("execution-worker"),
            correlation_id: correlation_id.to_owned(),
            reason: "durable QuestionSession submission verification".to_owned(),
        };
        let resolved = if let Some(artifacts) = artifacts.as_ref() {
            artifacts
                .resolve_question_session_continuation(attempt.execution_id, &access)
                .await?
        } else {
            None
        };
        let provider = if let Some(resolved) = resolved.as_ref() {
            prepared.verify.verify_submission_with_session(
                &prepared.context,
                &prepared.remote_task_id,
                &prepared.draft,
                receipt,
                provider_session_continuation(resolved),
            )
        } else {
            prepared.verify.verify_submission(
                &prepared.context,
                &prepared.remote_task_id,
                &prepared.draft,
                receipt,
            )
        };
        tokio::pin!(provider);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut provider => break result,
                _ = heartbeat.tick() => self.renew_claims(job, attempt.execution_id).await?,
            }
        };
        match result {
            Ok(verification) => {
                self.finish_submission_verification(
                    job,
                    attempt,
                    prepared,
                    receipt,
                    verification,
                    correlation_id,
                )
                .await
            }
            Err(error) => {
                self.begin_verification_recovery(
                    job,
                    attempt,
                    provider_error_class(&error),
                    correlation_id,
                )
                .await
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "submission verification must retain the exact worker, Draft and receipt bindings"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "submission finish keeps result persistence, completion observation and terminal state mapping visible together"
    )]
    async fn finish_submission_verification(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        prepared: &PreparedSubmissionCall,
        receipt: Option<&asterism_domain::SubmissionReceipt>,
        verification: SubmissionVerificationSnapshot,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if verification.validate().is_err()
            || (verification.status == SubmissionVerificationStatus::Confirmed
                && verification.remote_state != Some(RemoteState::Completed))
        {
            return self
                .begin_verification_recovery(
                    job,
                    attempt,
                    ProviderErrorClass::ProtocolDrift,
                    correlation_id,
                )
                .await;
        }
        let status = match verification.status {
            SubmissionVerificationStatus::Confirmed => SubmissionResultStatus::Confirmed,
            SubmissionVerificationStatus::Rejected => SubmissionResultStatus::Rejected,
            SubmissionVerificationStatus::Pending | SubmissionVerificationStatus::Inconclusive => {
                return self
                    .begin_verification_recovery(
                        job,
                        attempt,
                        ProviderErrorClass::InvalidRemoteState,
                        correlation_id,
                    )
                    .await;
            }
        };
        let now = Utc::now().max(attempt.started_at);
        let result = SubmissionResult {
            id: SubmissionResultId::new(),
            submission_draft_id: prepared.draft.id,
            execution_id: attempt.execution_id,
            execution_attempt_id: attempt.id,
            task_id: prepared.draft.task_id,
            question_snapshot_id: prepared.draft.question_snapshot_id,
            provider_id: prepared.draft.provider_id.clone(),
            provider_version: prepared.provider_version.clone(),
            status,
            receipt: receipt.cloned(),
            verification,
            created_at: now,
        };
        result
            .validate_for_draft(&prepared.draft)
            .map_err(|_| ScheduledExecutionRunError::StateConflict)?;
        self.executions
            .persist_submission_result(SubmissionResultPersistRequest {
                result: &result,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                correlation_id,
                at: now,
            })
            .await?;
        let completion = self
            .record_submission_completion_observation(
                job,
                attempt.id,
                prepared,
                &result.verification,
                now,
                correlation_id,
            )
            .await?;
        match completion.workflow.workflow.state {
            StrictCompletionState::Completed => {
                self.finish_success(job, attempt, now, correlation_id).await
            }
            StrictCompletionState::Active => {
                self.finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    FailureDisposition::HumanRequired,
                    now,
                    correlation_id,
                )
                .await
            }
            _ => match status {
                SubmissionResultStatus::Confirmed => {
                    self.finish_success(job, attempt, now, correlation_id).await
                }
                SubmissionResultStatus::Rejected => {
                    self.finish_failure(
                        job,
                        attempt,
                        ProviderErrorClass::InvalidRemoteState,
                        FailureDisposition::Failed,
                        now,
                        correlation_id,
                    )
                    .await
                }
                SubmissionResultStatus::ExecutionFailed | SubmissionResultStatus::Inconclusive => {
                    unreachable!("only terminal verification statuses are persisted here")
                }
            },
        }
    }

    async fn begin_verification_recovery(
        &self,
        job: &ScheduledJob,
        attempt: &ExecutionAttempt,
        error_class: ProviderErrorClass,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let at = Utc::now().max(attempt.started_at);
        let progress = ExecutionProgress {
            execution_id: attempt.execution_id,
            percent: None,
            stage: ExecutionStage::Verifying,
            status_text: Some(
                "remote mutation outcome uncertain; verification-only recovery scheduled"
                    .to_owned(),
            ),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        };
        let execution = self
            .executions
            .begin_verification_recovery(VerificationRecoveryStartRequest {
                execution_id: attempt.execution_id,
                attempt_id: attempt.id,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
                error_class,
                progress: &progress,
                at,
                correlation_id,
            })
            .await?;
        Ok(ScheduledExecutionOutcome::RecoveryScheduled(execution))
    }

    async fn acquire_admission(
        &self,
        job: &ScheduledJob,
        execution_id: ExecutionId,
        context: &ProviderContext,
        limits: ProviderExecutionConcurrency,
    ) -> Result<ExecutionAdmissionGuard, ScheduledExecutionRunError> {
        let admission = self
            .admission
            .acquire(&context.provider_id, context.account_id, limits);
        tokio::pin!(admission);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                guard = &mut admission => return Ok(guard),
                _ = heartbeat.tick() => self.renew_claims(job, execution_id).await?,
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

    async fn finish_after_completion_observation(
        &self,
        job: &ScheduledJob,
        task: &Task,
        attempt: &ExecutionAttempt,
        completion: StrictCompletionExecutionObservationRecord,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        if completion.workflow.workflow.state != StrictCompletionState::Active {
            return self.finish_success(job, attempt, at, correlation_id).await;
        }
        if task.assessment_class == asterism_domain::AssessmentClass::Formal {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    FailureDisposition::HumanRequired,
                    at,
                    correlation_id,
                )
                .await;
        }
        let Some(delay) = self.config.retry_policy.delay_after(attempt.attempt_no)? else {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    FailureDisposition::HumanRequired,
                    at,
                    correlation_id,
                )
                .await;
        };
        let retry_at = add_duration(at, delay)?;
        if completion
            .workflow
            .workflow
            .policy
            .strict_expires_at
            .is_some_and(|expires_at| retry_at >= expires_at)
        {
            return self
                .finish_failure(
                    job,
                    attempt,
                    ProviderErrorClass::InvalidRemoteState,
                    FailureDisposition::HumanRequired,
                    at,
                    correlation_id,
                )
                .await;
        }
        self.finish_failure(
            job,
            attempt,
            ProviderErrorClass::InvalidRemoteState,
            FailureDisposition::RetryAt(retry_at),
            at,
            correlation_id,
        )
        .await
    }

    async fn finish_next_question_step(
        &self,
        job: &ScheduledJob,
        execution_id: ExecutionId,
        attempt_id: asterism_domain::ExecutionAttemptId,
        transition: &QuestionSessionTransition,
        at: Timestamp,
        correlation_id: &str,
    ) -> Result<ScheduledExecutionOutcome, ScheduledExecutionRunError> {
        let progress = ExecutionProgress {
            execution_id,
            percent: None,
            stage: ExecutionStage::Completed,
            status_text: Some("next Question materialized and ready for review".to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        };
        let execution = self
            .executions
            .finish_question_step(ExecutionQuestionStepFinishRequest {
                execution_id,
                attempt_id,
                transition,
                scheduler_job_id: job.id,
                worker_id: claimed_worker(job)?,
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

fn provider_session_continuation(
    resolved: &ResolvedQuestionSessionContinuation,
) -> ResolvedProviderQuestionSessionContinuation<'_> {
    ResolvedProviderQuestionSessionContinuation {
        continuation_type: &resolved.metadata.continuation_type,
        continuation_digest: resolved.metadata.continuation_digest,
        phase: &resolved.metadata.phase,
        revision: resolved.metadata.revision,
        value: &resolved.value,
    }
}

fn provider_ambiguous_operation(
    operation: &QuestionSessionOperation,
) -> AmbiguousProviderQuestionSessionOperation {
    AmbiguousProviderQuestionSessionOperation {
        continuation_revision: operation.continuation_revision,
        operation_type: operation.operation_type.clone(),
        request_digest: operation.request_digest,
        issued_at: operation.issued_at,
        ambiguous_at: operation.completed_at.unwrap_or(operation.issued_at),
    }
}

enum AcceptedDurableSubmissionOutcome {
    Continue,
    NextQuestion(QuestionSessionTransition),
    Submitted(asterism_domain::SubmissionReceipt),
}

async fn accept_durable_submission_outcome(
    artifacts: &dyn QuestionSessionArtifactRepository,
    operation: &QuestionSessionOperation,
    outcome: ProviderSubmissionStepOutcome,
    access: &SecretAccess,
    prepared: &PreparedSubmissionCall,
) -> Result<AcceptedDurableSubmissionOutcome, ScheduledExecutionRunError> {
    let (continuation, response_digest, received_at) = match outcome {
        ProviderSubmissionStepOutcome::Continue {
            continuation,
            response_digest,
            received_at,
        } => (continuation, response_digest, received_at),
        ProviderSubmissionStepOutcome::Submitted {
            receipt,
            response_digest,
            received_at,
        } => {
            if response_digest == [0; 32]
                || received_at < operation.issued_at
                || receipt.validate().is_err()
            {
                return Err(ScheduledExecutionRunError::StateConflict);
            }
            let accepted = artifacts
                .finish_question_session_operation(
                    operation,
                    QuestionSessionOperationState::Accepted,
                    Some(response_digest),
                    received_at,
                    &access.correlation_id,
                )
                .await?;
            return match accepted {
                QuestionSessionOperationFinishOutcome::Finished(accepted)
                | QuestionSessionOperationFinishOutcome::Duplicate(accepted)
                    if accepted.state == QuestionSessionOperationState::Accepted =>
                {
                    Ok(AcceptedDurableSubmissionOutcome::Submitted(receipt))
                }
                _ => Err(ScheduledExecutionRunError::StateConflict),
            };
        }
        ProviderSubmissionStepOutcome::NextQuestion(materialization) => {
            return accept_durable_next_question(
                artifacts,
                operation,
                materialization,
                access,
                prepared,
            )
            .await;
        }
    };
    if response_digest == [0; 32] || received_at < operation.issued_at {
        return Err(ScheduledExecutionRunError::StateConflict);
    }
    let (next_type, expected_digest, next_phase, replacement, _) = continuation.into_parts();
    let accepted = artifacts
        .accept_question_session_operation(QuestionSessionOperationAcceptRequest {
            operation,
            next_continuation_type: &next_type,
            next_phase: &next_phase,
            replacement,
            result_digest: response_digest,
            accepted_at: received_at,
            access,
        })
        .await?;
    match accepted {
        QuestionSessionOperationFinishOutcome::Accepted { continuation, .. }
            if continuation.continuation_digest == expected_digest =>
        {
            Ok(AcceptedDurableSubmissionOutcome::Continue)
        }
        QuestionSessionOperationFinishOutcome::Duplicate(_) => {
            Ok(AcceptedDurableSubmissionOutcome::Continue)
        }
        QuestionSessionOperationFinishOutcome::Accepted { .. }
        | QuestionSessionOperationFinishOutcome::Finished(_)
        | QuestionSessionOperationFinishOutcome::Conflict
        | QuestionSessionOperationFinishOutcome::Unavailable => {
            Err(ScheduledExecutionRunError::StateConflict)
        }
    }
}

async fn accept_durable_next_question(
    artifacts: &dyn QuestionSessionArtifactRepository,
    operation: &QuestionSessionOperation,
    materialization: ProviderQuestionMaterialization,
    access: &SecretAccess,
    prepared: &PreparedSubmissionCall,
) -> Result<AcceptedDurableSubmissionOutcome, ScheduledExecutionRunError> {
    let (questions, artifact, response_digest, received_at) = materialization.into_parts();
    if response_digest == [0; 32]
        || received_at < operation.issued_at
        || questions
            .iter()
            .any(|question| question.task_id != prepared.draft.task_id)
    {
        return Err(ScheduledExecutionRunError::StateConflict);
    }
    let (artifact_type, expected_digest, artifact_phase, artifact, ttl_seconds) =
        artifact.into_parts();
    let snapshot = QuestionSnapshot {
        id: QuestionSnapshotId::new(),
        task_id: prepared.draft.task_id,
        provider_id: prepared.draft.provider_id.clone(),
        provider_version: prepared.draft.provider_version.clone(),
        captured_at: received_at,
        questions,
    };
    let materialized = artifacts
        .materialize_next_question_session(QuestionSessionNextMaterializeRequest {
            operation,
            snapshot: &snapshot,
            artifact_type: &artifact_type,
            artifact_phase: &artifact_phase,
            artifact,
            artifact_ttl_seconds: ttl_seconds,
            result_digest: response_digest,
            materialized_at: received_at,
            access,
        })
        .await?;
    match materialized {
        QuestionSessionNextMaterializeOutcome::Materialized {
            operation: accepted,
            transition,
            continuation,
        } if accepted.state == QuestionSessionOperationState::Accepted
            && accepted.result_digest == Some(response_digest)
            && continuation.session_id == transition.next_session_id
            && continuation.execution_id.is_none()
            && continuation.continuation_type == artifact_type
            && continuation.continuation_digest == expected_digest
            && continuation.phase == artifact_phase
            && transition.next_question_snapshot_id == snapshot.id =>
        {
            Ok(AcceptedDurableSubmissionOutcome::NextQuestion(transition))
        }
        QuestionSessionNextMaterializeOutcome::Duplicate {
            operation: accepted,
            transition,
        } if accepted.state == QuestionSessionOperationState::Accepted
            && accepted.result_digest == Some(response_digest) =>
        {
            Ok(AcceptedDurableSubmissionOutcome::NextQuestion(transition))
        }
        QuestionSessionNextMaterializeOutcome::Materialized { .. }
        | QuestionSessionNextMaterializeOutcome::Duplicate { .. }
        | QuestionSessionNextMaterializeOutcome::Conflict
        | QuestionSessionNextMaterializeOutcome::Unavailable => {
            Err(ScheduledExecutionRunError::StateConflict)
        }
    }
}

fn valid_question_session_label(provider_id: &ProviderId, value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

struct PreparedProviderCall {
    capability: Arc<dyn TaskExecutionCapability>,
    verification: bool,
    context: ProviderContext,
    request: ProviderExecutionRequest,
    concurrency: ProviderExecutionConcurrency,
}

struct PreparedSubmissionCall {
    execute: Arc<dyn SubmissionExecuteCapability>,
    verify: Arc<dyn SubmissionVerifyCapability>,
    context: ProviderContext,
    remote_task_id: String,
    draft: SubmissionDraft,
    runtime_settings: ResolvedProviderRuntimeSettings,
    concurrency: ProviderExecutionConcurrency,
    provider_version: String,
}

struct PreparedProgressRecoveryCall {
    capability: Arc<dyn TaskProgressCapability>,
    context: ProviderContext,
    concurrency: ProviderExecutionConcurrency,
}

struct PreparedFailure {
    error_class: ProviderErrorClass,
    disposition: FailureDisposition,
}

const fn prepared_failure(
    error_class: ProviderErrorClass,
    disposition: FailureDisposition,
) -> PreparedFailure {
    PreparedFailure {
        error_class,
        disposition,
    }
}

const fn internal_prepared_failure() -> PreparedFailure {
    prepared_failure(ProviderErrorClass::Internal, FailureDisposition::Failed)
}

struct PersistedExecutionEventSink<'a, E> {
    executions: &'a E,
    execution_id: ExecutionId,
    attempt_id: asterism_domain::ExecutionAttemptId,
    scheduler_job_id: asterism_domain::ScheduleId,
    worker_id: &'a str,
    correlation_id: &'a str,
    provider_id: ProviderId,
    mutations_enabled: bool,
    claim_lost: Arc<AtomicBool>,
}

#[async_trait]
impl<E> ExecutionEventSink for PersistedExecutionEventSink<'_, E>
where
    E: ExecutionRepository + ExecutionAtomicMutationRepository,
{
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

    fn mutation_sink(&self) -> Option<&(dyn ExecutionMutationSink + Send + Sync)> {
        self.mutations_enabled
            .then_some(self as &(dyn ExecutionMutationSink + Send + Sync))
    }
}

#[async_trait]
impl<E: ExecutionAtomicMutationRepository> ExecutionMutationSink
    for PersistedExecutionEventSink<'_, E>
{
    async fn issue(&self, issue: &ExecutionMutationIssue) -> Result<(), ProviderError> {
        if !issue
            .operation_type()
            .starts_with(&format!("{}.", self.provider_id))
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Provider execution mutation type is outside its namespace",
            ));
        }
        match self
            .executions
            .issue_execution_atomic_mutation(ExecutionAtomicMutationIssueRequest {
                execution_id: self.execution_id,
                attempt_id: self.attempt_id,
                ordinal: issue.ordinal(),
                scheduler_job_id: self.scheduler_job_id,
                worker_id: self.worker_id,
                operation_type: issue.operation_type(),
                request_digest: issue.request_digest(),
                correlation_id: self.correlation_id,
                at: Utc::now(),
            })
            .await
        {
            Ok(ExecutionAtomicMutationIssueOutcome::Issued(_)) => Ok(()),
            Ok(ExecutionAtomicMutationIssueOutcome::AlreadyIssued(_)) => {
                Err(ambiguous_mutation_error())
            }
            Err(error) => Err(self.map_mutation_storage_error(&error, false)),
        }
    }

    async fn record_receipt(&self, receipt: ExecutionMutationReceipt) -> Result<(), ProviderError> {
        match self
            .executions
            .record_execution_atomic_mutation_receipt(ExecutionAtomicMutationReceiptRequest {
                execution_id: self.execution_id,
                attempt_id: self.attempt_id,
                ordinal: receipt.ordinal(),
                scheduler_job_id: self.scheduler_job_id,
                worker_id: self.worker_id,
                response_digest: receipt.response_digest(),
                accepted: receipt.accepted(),
                correlation_id: self.correlation_id,
                at: Utc::now(),
            })
            .await
        {
            Ok(ExecutionAtomicMutationReceiptOutcome::Recorded(_)) => Ok(()),
            Ok(ExecutionAtomicMutationReceiptOutcome::AlreadyRecorded(_)) => {
                Err(ambiguous_mutation_error())
            }
            Err(error) => Err(self.map_mutation_storage_error(&error, true)),
        }
    }
}

impl<E> PersistedExecutionEventSink<'_, E> {
    fn map_mutation_storage_error(
        &self,
        error: &StorageError,
        mutation_may_have_completed: bool,
    ) -> ProviderError {
        if matches!(
            error,
            StorageError::ExecutionClaimLost
                | StorageError::SchedulerClaimLost
                | StorageError::LeaseLost
        ) {
            self.claim_lost.store(true, Ordering::Release);
        }
        if mutation_may_have_completed {
            ambiguous_mutation_error()
        } else {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Core could not persist Provider mutation issuance",
            )
        }
    }
}

fn ambiguous_mutation_error() -> ProviderError {
    ProviderError::human_required(
        "Provider mutation history requires fresh read-only recovery",
        HumanRequiredReason::ManualIntervention,
    )
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

fn execution_verification(
    task: &Task,
    execution_capabilities: &[TaskCapability],
    entry: &asterism_provider_api::ProviderEntry,
    capability: &Arc<dyn TaskExecutionCapability>,
) -> Result<bool, PreparedFailure> {
    if !task.capabilities.contains(&TaskCapability::ExecutionVerify) {
        return Ok(false);
    }
    if !capability.requires_execution_verification(execution_capabilities) {
        return Ok(false);
    }
    if !entry
        .metadata
        .advertises(ProviderCapability::ExecutionVerify)
    {
        return Err(unsupported_execution_verification());
    }
    Ok(true)
}

const fn unsupported_execution_verification() -> PreparedFailure {
    PreparedFailure {
        error_class: ProviderErrorClass::UnsupportedTask,
        disposition: FailureDisposition::Failed,
    }
}

fn duration_report_only(capabilities: &[TaskCapability]) -> bool {
    capabilities == [TaskCapability::DurationReport]
}

fn capability_steps_match_execution(
    execution: &Execution,
    steps: &[ExecutionCapabilityStep],
) -> bool {
    steps.len() == execution.requested_capabilities.len()
        && steps.iter().enumerate().all(|(index, step)| {
            step.execution_id == execution.id && usize::from(step.position) == index + 1
        })
        && steps
            .iter()
            .map(|step| step.capability)
            .collect::<std::collections::BTreeSet<_>>()
            == execution
                .requested_capabilities
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        && capability_calls(steps).is_some()
}

#[derive(Clone, Debug)]
struct ExecutionCapabilityCall {
    position: u8,
    first_step_position: u8,
    capabilities: Vec<TaskCapability>,
    state: ExecutionCapabilityStepState,
    issued_attempt_id: Option<asterism_domain::ExecutionAttemptId>,
}

fn capability_calls(steps: &[ExecutionCapabilityStep]) -> Option<Vec<ExecutionCapabilityCall>> {
    let mut calls = Vec::<ExecutionCapabilityCall>::new();
    for step in steps {
        if usize::from(step.call_position) == calls.len() + 1 {
            if step.call_member_position != 1 {
                return None;
            }
            calls.push(ExecutionCapabilityCall {
                position: step.call_position,
                first_step_position: step.position,
                capabilities: vec![step.capability],
                state: step.state,
                issued_attempt_id: step.issued_attempt_id,
            });
            continue;
        }
        let call = calls.last_mut()?;
        if step.call_position != call.position
            || usize::from(step.call_member_position) != call.capabilities.len() + 1
            || step.state != call.state
            || step.issued_attempt_id != call.issued_attempt_id
        {
            return None;
        }
        call.capabilities.push(step.capability);
    }
    (!calls.is_empty()).then_some(calls)
}

fn execution_goal_verified(
    capabilities: &[TaskCapability],
    outcome: &asterism_provider_api::ExecutionOutcome,
) -> bool {
    outcome.validate().is_ok()
        && outcome.verified
        && if duration_report_only(capabilities) {
            matches!(
                outcome.remote_state,
                RemoteState::Pending
                    | RemoteState::InProgress
                    | RemoteState::Completed
                    | RemoteState::Unknown
            )
        } else {
            outcome.remote_state == RemoteState::Completed
        }
}

fn authorize_execution(
    task: &Task,
    requested_capabilities: &[TaskCapability],
    policy: FormalAssessmentPolicy,
) -> Result<(), crate::AssessmentGuardError> {
    authorize_task_action(task, TaskAction::Execute, policy)?;
    if requested_capabilities.contains(&TaskCapability::SubmissionExecute) {
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
    let requested_capabilities_valid = !execution.requested_capabilities.is_empty()
        && execution.requested_capabilities.len() <= 5
        && execution
            .requested_capabilities
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && execution.requested_capabilities.iter().all(|capability| {
            task.capabilities.contains(capability)
                && matches!(
                    capability,
                    TaskCapability::ResourceExecution
                        | TaskCapability::SubmissionExecute
                        | TaskCapability::DurationReport
                        | TaskCapability::Discussion
                        | TaskCapability::Practice
                )
        })
        && (!execution
            .requested_capabilities
            .contains(&TaskCapability::SubmissionExecute)
            || execution.requested_capabilities == [TaskCapability::SubmissionExecute]);
    let submission_selected =
        execution.requested_capabilities == [TaskCapability::SubmissionExecute];
    let submission_binding_matches = submission_selected == execution.submission_draft_id.is_some();
    if execution.task_id == task.id
        && synchronized
        && requested_capabilities_valid
        && submission_binding_matches
    {
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
    RecoveryScheduled(Execution),
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
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{Arc, Mutex},
    };

    use asterism_domain::{
        AnswerCandidate, AnswerCandidateId, AnswerConfidence, AnswerSource, AssessmentClass,
        AuditActor, NormalizedAnswer, ProviderAccountId, ProviderId, ProviderRuntimeSettingsId,
        Question, QuestionId, QuestionKind, QuestionOption, QuestionSession, QuestionSnapshotId,
        RequestSource, SelectedAnswer, SubmissionDraftId, SubmissionDraftItem,
        SubmissionPayloadEncoding, SubmissionPayloadFieldPreview, SubmissionPayloadPreview, TaskId,
        UserId,
    };
    use asterism_provider_api::{
        ExecutionOutcome, PreparedProviderSubmissionOperation, ProviderCapability, ProviderEntry,
        ProviderIdentity, ProviderMetadata, ProviderQuestionReadContinuation, ProviderResult,
        ProviderRuntimeSettingsSchema, ProviderSettingDefinition, ProviderSettingKind,
        ProviderSettingScope, ProviderSettingValue, ProviderSubmissionStepOutcome, RemoteProgress,
        SubmissionExecuteCapability, SubmissionVerifyCapability, TaskProgressCapability,
        VerificationLevel,
    };
    use asterism_secrets::{SecretKey, SecretValue};
    use asterism_storage::{
        AnswerCandidateRecord, AnswerCandidateRepository, Database,
        ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot,
        ExecutionScheduleRequest, QuestionSessionArtifactAttachRequest, QuestionSessionRepository,
        QuestionSnapshot, QuestionSnapshotRepository, SecretKeyring,
        SqliteExecutionLeaseRepository, SqliteExecutionRepository, SqliteProviderAccountRepository,
        SqliteQuestionSessionRepository, SqliteQuestionSnapshotRepository,
        SqliteSchedulerRepository, SqliteSecretStore, SqliteTaskQueryRepository,
        SubmissionDraftRepository,
    };
    use sqlx::Row;

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ProviderBehavior {
        Success,
        VerifiedPending,
        ExecuteNetworkThenCompleted,
        NetworkFailure,
        RecoveryPending,
        DurationSuccess,
        DurationIncomplete,
        DurationNetworkFailure,
        CompositeSuccess,
        SubmissionConfirmed,
        SubmissionRejected,
        SubmissionPending,
        SubmissionExecuteNetwork,
        DurableSubmissionConfirmed,
        DurableSubmissionAmbiguous,
        DurableSubmissionNextQuestion,
    }

    #[tokio::test]
    async fn admission_controller_enforces_global_provider_and_account_limits() {
        let provider = ProviderId::new("provider-alpha").unwrap();
        let account_a = ProviderAccountId::new();
        let account_b = ProviderAccountId::new();
        let limits = ProviderExecutionConcurrency {
            provider: 2,
            account: 1,
        };
        let controller = Arc::new(ExecutionAdmissionController::new(3));
        let first = controller.acquire(&provider, account_a, limits).await;
        let second = controller.acquire(&provider, account_b, limits).await;
        assert!(
            tokio::time::timeout(
                StdDuration::from_millis(1),
                controller.acquire(&provider, account_a, limits),
            )
            .await
            .is_err()
        );
        let account_c = ProviderAccountId::new();
        assert!(
            tokio::time::timeout(
                StdDuration::from_millis(1),
                controller.acquire(&provider, account_c, limits),
            )
            .await
            .is_err()
        );
        drop(first);
        let third = tokio::time::timeout(
            StdDuration::from_millis(10),
            controller.acquire(&provider, account_a, limits),
        )
        .await
        .unwrap();
        drop(second);
        drop(third);

        let global = Arc::new(ExecutionAdmissionController::new(2));
        let permissive = ProviderExecutionConcurrency {
            provider: 3,
            account: 2,
        };
        let provider_b = ProviderId::new("provider-beta").unwrap();
        let one = global.acquire(&provider, account_a, permissive).await;
        let two = global.acquire(&provider_b, account_b, permissive).await;
        assert!(
            tokio::time::timeout(
                StdDuration::from_millis(1),
                global.acquire(&provider_b, account_c, permissive),
            )
            .await
            .is_err()
        );
        drop(one);
        drop(two);
    }

    #[derive(Debug)]
    struct FakeExecution {
        metadata: ProviderMetadata,
        behavior: ProviderBehavior,
        calls: Mutex<u32>,
        progress_calls: Mutex<u32>,
        submission_verify_calls: Mutex<u32>,
        received_provider_plan_artifacts:
            Mutex<Vec<Option<asterism_provider_api::ProviderExecutionPlanArtifact>>>,
        durable_calls: Arc<Mutex<u32>>,
        database: Database,
    }

    impl ProviderIdentity for FakeExecution {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    fn fake_completion_diagnosis(
        behavior: ProviderBehavior,
        request: &ProviderExecutionRequest,
        outcome: &ExecutionOutcome,
    ) -> Option<asterism_domain::CompletionDiagnosis> {
        (behavior == ProviderBehavior::DurationIncomplete
            && request.requested_capabilities == [TaskCapability::DurationReport]
            && outcome.result_sanitized["completion"] == "duration_insufficient")
            .then_some(asterism_domain::CompletionDiagnosis::DurationInsufficient)
    }

    fn fake_duration_outcome(behavior: ProviderBehavior, goal_verified: bool) -> ExecutionOutcome {
        let mut result_sanitized = if goal_verified {
            serde_json::json!({"duration_goal_matched": true})
        } else {
            serde_json::json!({"duration_changed": true})
        };
        if behavior == ProviderBehavior::DurationIncomplete {
            result_sanitized["completion"] = serde_json::json!("duration_insufficient");
        }
        ExecutionOutcome {
            remote_state: RemoteState::InProgress,
            verified: true,
            result_sanitized,
        }
    }

    #[async_trait]
    impl TaskExecutionCapability for FakeExecution {
        fn requires_execution_verification(
            &self,
            requested_capabilities: &[TaskCapability],
        ) -> bool {
            requested_capabilities == [TaskCapability::ResourceExecution]
                || self.behavior == ProviderBehavior::CompositeSuccess
                    && requested_capabilities.len() == 2
        }

        async fn execute(
            &self,
            _context: &ProviderContext,
            request: &ProviderExecutionRequest,
            events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ExecutionOutcome> {
            *self.calls.lock().unwrap() += 1;
            self.received_provider_plan_artifacts
                .lock()
                .unwrap()
                .push(request.provider_plan_artifact.clone());
            let expected_capabilities = match self.behavior {
                ProviderBehavior::DurationSuccess
                | ProviderBehavior::DurationIncomplete
                | ProviderBehavior::DurationNetworkFailure => vec![TaskCapability::DurationReport],
                ProviderBehavior::CompositeSuccess => request.requested_capabilities.clone(),
                ProviderBehavior::SubmissionConfirmed
                | ProviderBehavior::SubmissionRejected
                | ProviderBehavior::SubmissionPending
                | ProviderBehavior::SubmissionExecuteNetwork
                | ProviderBehavior::DurableSubmissionConfirmed
                | ProviderBehavior::DurableSubmissionAmbiguous
                | ProviderBehavior::DurableSubmissionNextQuestion => {
                    panic!("submission behaviors use the independent mutation slot")
                }
                _ => vec![TaskCapability::ResourceExecution],
            };
            assert_eq!(request.requested_capabilities, expected_capabilities);
            assert_eq!(
                request
                    .runtime_settings
                    .integer("execution.max_concurrency"),
                Some(3)
            );
            match self.behavior {
                ProviderBehavior::Success | ProviderBehavior::VerifiedPending => {
                    if self.behavior == ProviderBehavior::Success {
                        persist_fixture_mutation(events).await?;
                    }
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
                ProviderBehavior::NetworkFailure
                | ProviderBehavior::ExecuteNetworkThenCompleted => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "temporary network failure",
                )),
                ProviderBehavior::RecoveryPending => Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "recovery-only fixture cannot execute",
                )),
                ProviderBehavior::DurationSuccess | ProviderBehavior::DurationIncomplete => {
                    Ok(fake_duration_outcome(self.behavior, false))
                }
                ProviderBehavior::DurationNetworkFailure => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "duration mutation outcome is uncertain",
                )),
                ProviderBehavior::CompositeSuccess
                    if request.requested_capabilities == [TaskCapability::DurationReport] =>
                {
                    Ok(ExecutionOutcome {
                        remote_state: RemoteState::InProgress,
                        verified: true,
                        result_sanitized: serde_json::json!({"duration_changed": true}),
                    })
                }
                ProviderBehavior::CompositeSuccess => Ok(ExecutionOutcome {
                    remote_state: RemoteState::Completed,
                    verified: true,
                    result_sanitized: serde_json::json!({"completion_changed": true}),
                }),
                ProviderBehavior::SubmissionConfirmed
                | ProviderBehavior::SubmissionRejected
                | ProviderBehavior::SubmissionPending
                | ProviderBehavior::SubmissionExecuteNetwork
                | ProviderBehavior::DurableSubmissionConfirmed
                | ProviderBehavior::DurableSubmissionAmbiguous
                | ProviderBehavior::DurableSubmissionNextQuestion => {
                    panic!("submission behaviors use the independent mutation slot")
                }
            }
        }

        async fn verify_execution(
            &self,
            _context: &ProviderContext,
            request: &ProviderExecutionRequest,
        ) -> ProviderResult<ExecutionOutcome> {
            *self.progress_calls.lock().unwrap() += 1;
            self.received_provider_plan_artifacts
                .lock()
                .unwrap()
                .push(request.provider_plan_artifact.clone());
            assert_eq!(
                request
                    .runtime_settings
                    .integer("execution.max_concurrency"),
                Some(3)
            );
            match self.behavior {
                ProviderBehavior::Success | ProviderBehavior::ExecuteNetworkThenCompleted => {
                    Ok(ExecutionOutcome {
                        remote_state: RemoteState::Completed,
                        verified: true,
                        result_sanitized: serde_json::json!({"goal_matched": true}),
                    })
                }
                ProviderBehavior::RecoveryPending => Ok(ExecutionOutcome {
                    remote_state: RemoteState::Pending,
                    verified: false,
                    result_sanitized: serde_json::json!({"goal_matched": false}),
                }),
                ProviderBehavior::VerifiedPending => Ok(ExecutionOutcome {
                    remote_state: RemoteState::InProgress,
                    verified: false,
                    result_sanitized: serde_json::json!({"goal_matched": false}),
                }),
                ProviderBehavior::NetworkFailure => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "temporary goal verification failure",
                )),
                ProviderBehavior::DurationSuccess | ProviderBehavior::DurationIncomplete => {
                    Ok(fake_duration_outcome(self.behavior, true))
                }
                ProviderBehavior::DurationNetworkFailure => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "duration goal verification is uncertain",
                )),
                ProviderBehavior::CompositeSuccess => Ok(ExecutionOutcome {
                    remote_state: RemoteState::Completed,
                    verified: true,
                    result_sanitized: serde_json::json!({"goal_matched": true}),
                }),
                ProviderBehavior::SubmissionConfirmed
                | ProviderBehavior::SubmissionRejected
                | ProviderBehavior::SubmissionPending
                | ProviderBehavior::SubmissionExecuteNetwork
                | ProviderBehavior::DurableSubmissionConfirmed
                | ProviderBehavior::DurableSubmissionAmbiguous
                | ProviderBehavior::DurableSubmissionNextQuestion => {
                    panic!("submission recovery must use SubmissionVerify")
                }
            }
        }

        fn completion_diagnosis(
            &self,
            request: &ProviderExecutionRequest,
            outcome: &ExecutionOutcome,
        ) -> Option<asterism_domain::CompletionDiagnosis> {
            fake_completion_diagnosis(self.behavior, request, outcome)
        }
    }

    async fn persist_fixture_mutation(
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<()> {
        let mutations = events
            .mutation_sink()
            .expect("Core execution fixture supplies durable mutation sink");
        mutations
            .issue(
                &ExecutionMutationIssue::new(1, "provider-alpha.fixture.save", [41; 32]).unwrap(),
            )
            .await?;
        mutations
            .record_receipt(ExecutionMutationReceipt::new(1, [42; 32], true).unwrap())
            .await
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
                ProviderBehavior::Success | ProviderBehavior::ExecuteNetworkThenCompleted => {
                    Ok(RemoteProgress {
                        remote_state: RemoteState::Completed,
                        percent: Some(100),
                        duration_seconds: None,
                        updated_at: Utc::now(),
                    })
                }
                ProviderBehavior::RecoveryPending => Ok(RemoteProgress {
                    remote_state: RemoteState::Pending,
                    percent: Some(0),
                    duration_seconds: None,
                    updated_at: Utc::now(),
                }),
                ProviderBehavior::VerifiedPending => Ok(RemoteProgress {
                    remote_state: RemoteState::InProgress,
                    percent: Some(50),
                    duration_seconds: None,
                    updated_at: Utc::now(),
                }),
                ProviderBehavior::NetworkFailure => Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "temporary progress read failure",
                )),
                ProviderBehavior::DurationSuccess
                | ProviderBehavior::DurationIncomplete
                | ProviderBehavior::DurationNetworkFailure => {
                    panic!("DurationReport recovery must not use TaskProgress")
                }
                ProviderBehavior::CompositeSuccess => {
                    panic!("composite recovery must use phase-bound execution verification")
                }
                ProviderBehavior::SubmissionConfirmed
                | ProviderBehavior::SubmissionRejected
                | ProviderBehavior::SubmissionPending
                | ProviderBehavior::SubmissionExecuteNetwork
                | ProviderBehavior::DurableSubmissionConfirmed
                | ProviderBehavior::DurableSubmissionAmbiguous
                | ProviderBehavior::DurableSubmissionNextQuestion => {
                    panic!("submission recovery must use SubmissionVerify")
                }
            }
        }
    }

    #[async_trait]
    impl SubmissionExecuteCapability for FakeExecution {
        async fn execute_submission(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            draft: &SubmissionDraft,
            _runtime_settings: &ResolvedProviderRuntimeSettings,
            events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<asterism_domain::SubmissionReceipt> {
            assert!(events.mutation_sink().is_none());
            assert!(
                !matches!(
                    self.behavior,
                    ProviderBehavior::DurableSubmissionConfirmed
                        | ProviderBehavior::DurableSubmissionAmbiguous
                        | ProviderBehavior::DurableSubmissionNextQuestion
                ),
                "durable submission must use registered operations"
            );
            *self.calls.lock().unwrap() += 1;
            assert_eq!(draft.provider_id, self.metadata.id);
            if self.behavior == ProviderBehavior::SubmissionExecuteNetwork {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "submission mutation outcome is uncertain",
                ));
            }
            Ok(asterism_domain::SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: None,
                provider_trace_id: Some("submission-trace".to_owned()),
                received_at: Utc::now(),
            })
        }

        async fn prepare_submission_operation(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            draft: &SubmissionDraft,
            continuation: ResolvedProviderQuestionSessionContinuation<'_>,
            _runtime_settings: &ResolvedProviderRuntimeSettings,
        ) -> ProviderResult<Option<Box<dyn PreparedProviderSubmissionOperation>>> {
            if !matches!(
                self.behavior,
                ProviderBehavior::DurableSubmissionConfirmed
                    | ProviderBehavior::DurableSubmissionAmbiguous
                    | ProviderBehavior::DurableSubmissionNextQuestion
            ) {
                return Ok(None);
            }
            assert_eq!(draft.provider_id, self.metadata.id);
            let (phase, expected_value, final_step, request_digest) = match continuation.phase {
                "provider-alpha.questions-ready" => {
                    ("answers-save", b"attempt-v1".as_slice(), false, [31; 32])
                }
                "provider-alpha.answers-saved" => {
                    ("submit", b"attempt-v2".as_slice(), true, [32; 32])
                }
                _ => {
                    return Err(ProviderError::new(
                        ProviderErrorKind::ProtocolDrift,
                        "fixture continuation phase is invalid",
                    ));
                }
            };
            assert_eq!(continuation.value.expose_secret(), expected_value);
            Ok(Some(Box::new(FakeDurableSubmissionOperation {
                database: self.database.clone(),
                provider_id: self.metadata.id.clone(),
                operation_type: format!("provider-alpha.{phase}"),
                request_digest,
                final_step,
                fail_after_issue: self.behavior == ProviderBehavior::DurableSubmissionAmbiguous,
                next_question: self.behavior == ProviderBehavior::DurableSubmissionNextQuestion,
                task_id: draft.task_id,
                calls: self.durable_calls.clone(),
            })))
        }
    }

    #[derive(Debug)]
    struct FakeDurableSubmissionOperation {
        database: Database,
        provider_id: ProviderId,
        operation_type: String,
        request_digest: [u8; 32],
        final_step: bool,
        fail_after_issue: bool,
        next_question: bool,
        task_id: asterism_domain::TaskId,
        calls: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl PreparedProviderSubmissionOperation for FakeDurableSubmissionOperation {
        fn operation_type(&self) -> &str {
            &self.operation_type
        }

        fn request_digest(&self) -> [u8; 32] {
            self.request_digest
        }

        fn delay_before_execute_seconds(&self) -> u64 {
            0
        }

        async fn execute(
            self: Box<Self>,
            _context: &ProviderContext,
            events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ProviderSubmissionStepOutcome> {
            assert!(events.mutation_sink().is_none());
            let issued: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM question_session_operations \
                 WHERE operation_type = ? AND request_digest = ? AND state = 'issued'",
            )
            .bind(&self.operation_type)
            .bind(self.request_digest.as_slice())
            .fetch_one(self.database.pool())
            .await
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::Internal,
                    "fixture could not inspect issued operation",
                )
            })?;
            assert_eq!(issued, 1, "Provider ran before exact operation issue");
            *self.calls.lock().unwrap() += 1;
            if self.fail_after_issue {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "fixture operation outcome is ambiguous",
                ));
            }
            let received_at = Utc::now();
            if self.next_question {
                let artifact = ProviderQuestionReadContinuation::try_new(
                    &self.provider_id,
                    "provider-alpha.question-attempt.v1",
                    "provider-alpha.questions-ready",
                    SecretValue::new(b"attempt-question-two".to_vec()),
                    300,
                )?;
                return Ok(ProviderSubmissionStepOutcome::NextQuestion(
                    asterism_provider_api::ProviderQuestionMaterialization::try_new(
                        vec![Question {
                            id: QuestionId::new(),
                            task_id: self.task_id,
                            remote_question_id: Some("question-two".to_owned()),
                            kind: QuestionKind::ShortAnswer,
                            stem: "Second Question".to_owned(),
                            options: Vec::new(),
                            attachments: Vec::new(),
                            metadata_sanitized: serde_json::json!({}),
                            position: 1,
                        }],
                        artifact,
                        [43; 32],
                        received_at,
                    )?,
                ));
            }
            let continuation = ProviderQuestionReadContinuation::try_new(
                &self.provider_id,
                "provider-alpha.question-attempt.v1",
                if self.final_step {
                    "provider-alpha.submitted"
                } else {
                    "provider-alpha.answers-saved"
                },
                SecretValue::new(if self.final_step {
                    b"attempt-v3".to_vec()
                } else {
                    b"attempt-v2".to_vec()
                }),
                300,
            )?;
            if self.final_step {
                ProviderSubmissionStepOutcome::submitted(
                    asterism_domain::SubmissionReceipt {
                        remote_status: "accepted".to_owned(),
                        message_sanitized: None,
                        provider_trace_id: Some("durable-submission".to_owned()),
                        received_at,
                    },
                    [42; 32],
                    received_at,
                )
            } else {
                ProviderSubmissionStepOutcome::continuing(continuation, [41; 32], received_at)
            }
        }
    }

    #[async_trait]
    impl SubmissionVerifyCapability for FakeExecution {
        async fn verify_submission(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            draft: &SubmissionDraft,
            _receipt: Option<&asterism_domain::SubmissionReceipt>,
        ) -> ProviderResult<SubmissionVerificationSnapshot> {
            *self.submission_verify_calls.lock().unwrap() += 1;
            let pending = self.behavior == ProviderBehavior::SubmissionPending;
            let rejected = self.behavior == ProviderBehavior::SubmissionRejected;
            Ok(SubmissionVerificationSnapshot {
                status: if pending {
                    SubmissionVerificationStatus::Pending
                } else if rejected {
                    SubmissionVerificationStatus::Rejected
                } else {
                    SubmissionVerificationStatus::Confirmed
                },
                remote_state: Some(if pending || rejected {
                    RemoteState::InProgress
                } else {
                    RemoteState::Completed
                }),
                score: rejected.then_some(asterism_domain::SubmissionScore {
                    earned_milli_points: 0,
                    possible_milli_points: 1_000,
                }),
                progress_percent: Some(if pending || rejected { 50 } else { 100 }),
                questions: draft
                    .items
                    .iter()
                    .map(|item| asterism_domain::SubmissionQuestionVerification {
                        question_id: item.question.id,
                        status: if pending {
                            asterism_domain::SubmissionQuestionVerificationStatus::Unverified
                        } else if rejected {
                            asterism_domain::SubmissionQuestionVerificationStatus::Rejected
                        } else {
                            asterism_domain::SubmissionQuestionVerificationStatus::Confirmed
                        },
                    })
                    .collect(),
                verified_at: Utc::now(),
            })
        }

        async fn verify_submission_with_session(
            &self,
            context: &ProviderContext,
            remote_task_id: &str,
            draft: &SubmissionDraft,
            receipt: Option<&asterism_domain::SubmissionReceipt>,
            continuation: ResolvedProviderQuestionSessionContinuation<'_>,
        ) -> ProviderResult<SubmissionVerificationSnapshot> {
            assert_eq!(continuation.phase, "provider-alpha.answers-saved");
            assert_eq!(continuation.value.expose_secret(), b"attempt-v2");
            self.verify_submission(context, remote_task_id, draft, receipt)
                .await
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
        let mutation: (i64, String, Vec<u8>, Vec<u8>, bool) = sqlx::query_as(
            "SELECT ordinal, operation_type, request_digest, response_digest, accepted \
             FROM execution_atomic_mutations WHERE execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(mutation.0, 1);
        assert_eq!(mutation.1, "provider-alpha.fixture.save");
        assert_eq!(mutation.2, vec![41; 32]);
        assert_eq!(mutation.3, vec![42; 32]);
        assert!(mutation.4);
        let completion: (String, i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT workflow.state, json_extract(workflow.workflow_json, '$.attempts_started'), \
                    observation.completion_outcome, \
                    observation.diagnosis \
             FROM strict_completion_workflows AS workflow \
             INNER JOIN strict_completion_execution_observations AS observation \
                     ON observation.workflow_id = workflow.id \
             WHERE observation.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            completion,
            (
                "completed".to_owned(),
                0,
                Some("completed".to_owned()),
                None
            )
        );
    }

    #[tokio::test]
    async fn recovery_injects_the_exact_frozen_provider_plan_artifact() {
        let fixture = Fixture::verified_recovering(ProviderBehavior::Success).await;
        let artifact = fixture.attach_provider_plan_artifact().await;

        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
        assert_eq!(
            *fixture
                .provider
                .received_provider_plan_artifacts
                .lock()
                .unwrap(),
            vec![Some(artifact)]
        );
    }

    #[tokio::test]
    async fn non_idempotent_execution_requires_fresh_progress_confirmation() {
        let fixture = Fixture::verified(ProviderBehavior::Success).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unknown_non_idempotent_execution_runs_once_then_verifies() {
        let fixture = Fixture::verified(ProviderBehavior::Success).await;
        sqlx::query("UPDATE tasks SET remote_state = 'unknown'")
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
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn completed_non_idempotent_execution_only_verifies() {
        let fixture = Fixture::verified(ProviderBehavior::Success).await;
        sqlx::query("UPDATE tasks SET remote_state = 'completed'")
            .execute(fixture.database.pool())
            .await
            .unwrap();

        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn ambiguous_non_idempotent_execution_schedules_verify_only_recovery() {
        let fixture = Fixture::verified(ProviderBehavior::NetworkFailure).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::RecoveryScheduled(_)
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 0);
        let jobs: Vec<(String, String)> =
            sqlx::query_as("SELECT job_kind, state FROM scheduled_jobs ORDER BY created_at, id")
                .fetch_all(fixture.database.pool())
                .await
                .unwrap();
        assert!(jobs.contains(&("execution".to_owned(), "completed".to_owned())));
        assert!(jobs.contains(&("recovery".to_owned(), "pending".to_owned())));
        assert!(!jobs.iter().any(|(kind, _)| kind == "retry"));
    }

    #[tokio::test]
    async fn ambiguous_non_idempotent_execution_recovers_without_replaying_mutation() {
        let mut fixture = Fixture::verified(ProviderBehavior::ExecuteNetworkThenCompleted).await;
        let first = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            first,
            ScheduledExecutionOutcome::RecoveryScheduled(_)
        ));

        let recovery_at = Utc::now();
        fixture.job = fixture.claim_recovery(recovery_at).await;
        let recovered = fixture
            .runner
            .run_claimed(&fixture.job, recovery_at)
            .await
            .unwrap();

        assert!(matches!(recovered, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn pending_non_idempotent_verification_never_finishes_from_execute_receipt() {
        let mut fixture = Fixture::verified(ProviderBehavior::VerifiedPending).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::RecoveryScheduled(_)
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);

        let recovery_at = Utc::now();
        fixture.job = fixture.claim_recovery(recovery_at).await;
        let recovered = fixture
            .runner
            .run_claimed(&fixture.job, recovery_at)
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            ScheduledExecutionOutcome::Deferred { .. }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 2);
        let retry_jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs WHERE job_kind = 'retry'")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(retry_jobs, 0);
    }

    #[tokio::test]
    async fn abandoned_non_idempotent_execution_recovers_without_reexecution() {
        let fixture = Fixture::verified_recovering(ProviderBehavior::Success).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn verified_duration_report_succeeds_without_completing_the_task_remotely() {
        let fixture = Fixture::duration(ProviderBehavior::DurationSuccess).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 0);
        let completion: (String, i64, Option<String>) = sqlx::query_as(
            "SELECT workflow.state, json_extract(workflow.workflow_json, '$.attempts_started'), \
                    observation.diagnosis \
             FROM strict_completion_workflows AS workflow \
             INNER JOIN strict_completion_execution_observations AS observation \
                     ON observation.workflow_id = workflow.id \
             WHERE observation.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            completion,
            ("stopped".to_owned(), 1, Some("remote_unknown".to_owned()))
        );
    }

    #[tokio::test]
    async fn strict_completion_retries_incomplete_duration_until_attempt_limit() {
        let mut fixture = Fixture::duration(ProviderBehavior::DurationIncomplete).await;
        let first = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        let first_retry_at = match first {
            ScheduledExecutionOutcome::RetryScheduled {
                error_class: ProviderErrorClass::InvalidRemoteState,
                retry_at,
                ..
            } => retry_at,
            other => panic!("expected strict completion retry, got {other:?}"),
        };

        fixture.job = fixture.claim_retry(first_retry_at).await;
        let second = fixture
            .runner
            .run_claimed(&fixture.job, first_retry_at)
            .await
            .unwrap();
        let second_retry_at = match second {
            ScheduledExecutionOutcome::RetryScheduled {
                error_class: ProviderErrorClass::InvalidRemoteState,
                retry_at,
                ..
            } => retry_at,
            other => panic!("expected second strict completion retry, got {other:?}"),
        };

        fixture.job = fixture.claim_retry(second_retry_at).await;
        let third = fixture
            .runner
            .run_claimed(&fixture.job, second_retry_at)
            .await
            .unwrap();
        assert!(matches!(third, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 3);

        let completion: (String, i64, Option<String>, i64, i64) = sqlx::query_as(
            "SELECT workflow.state, json_extract(workflow.workflow_json, '$.attempts_started'), \
                    json_extract(workflow.workflow_json, '$.last_diagnosis'), \
                    COUNT(DISTINCT observation.execution_attempt_id), COUNT(*) \
             FROM strict_completion_workflows AS workflow \
             INNER JOIN strict_completion_execution_observations AS observation \
                     ON observation.workflow_id = workflow.id \
             WHERE observation.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            completion,
            (
                "stopped".to_owned(),
                3,
                Some("attempt_limit_reached".to_owned()),
                3,
                3,
            )
        );
        let attempts: Vec<(i64, String)> = sqlx::query_as(
            "SELECT attempt_no, result FROM execution_attempts \
             WHERE execution_id = ? ORDER BY attempt_no",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            attempts,
            vec![
                (1, "failed".to_owned()),
                (2, "failed".to_owned()),
                (3, "succeeded".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn formal_strict_completion_requires_confirmation_before_retry() {
        let mut config = runner_config();
        config.formal_assessment_policy.allow_execution = true;
        let fixture = Fixture::new_with_config(
            AssessmentClass::Formal,
            ProviderBehavior::DurationIncomplete,
            config,
        )
        .await
        .configure_duration()
        .await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::HumanRequired {
                error_class: ProviderErrorClass::InvalidRemoteState,
                ..
            }
        ));
        let state: (String, i64, i64) = sqlx::query_as(
            "SELECT workflow.state, json_extract(workflow.workflow_json, '$.attempts_started'), \
                    (SELECT COUNT(*) FROM scheduled_jobs WHERE job_kind = 'retry') \
             FROM strict_completion_workflows AS workflow",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(state, ("active".to_owned(), 1, 0));
    }

    #[tokio::test]
    async fn composite_execution_persists_and_verifies_each_provider_ordered_phase() {
        let fixture = Fixture::composite().await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 2);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
        let steps: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT position, capability, state FROM execution_capability_steps \
             WHERE execution_id = ? ORDER BY position",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            steps,
            vec![
                (1, "duration_report".to_owned(), "succeeded".to_owned()),
                (2, "resource_execution".to_owned(), "succeeded".to_owned()),
            ]
        );
        let completion: (i64, String, Option<String>) = sqlx::query_as(
            "SELECT COUNT(*), workflow.state, observation.completion_outcome \
             FROM strict_completion_execution_observations AS observation \
             INNER JOIN strict_completion_workflows AS workflow \
                     ON workflow.id = observation.workflow_id \
             WHERE observation.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            completion,
            (1, "completed".to_owned(), Some("completed".to_owned()))
        );
    }

    #[tokio::test]
    async fn atomic_composite_call_executes_and_verifies_once() {
        let fixture = Fixture::atomic_composite().await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 1);
        let steps: Vec<(i64, i64, String)> = sqlx::query_as(
            "SELECT call_position, call_member_position, state \
             FROM execution_capability_steps WHERE execution_id = ? ORDER BY position",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            steps,
            vec![
                (1, 1, "succeeded".to_owned()),
                (1, 2, "succeeded".to_owned()),
            ]
        );
    }

    #[test]
    fn verified_duration_goal_does_not_invent_a_completion_state() {
        assert!(execution_goal_verified(
            &[TaskCapability::DurationReport],
            &ExecutionOutcome {
                remote_state: RemoteState::Unknown,
                verified: true,
                result_sanitized: serde_json::json!({"duration_changed": true}),
            },
        ));
    }

    #[tokio::test]
    async fn uncertain_duration_report_requires_human_review_without_retry() {
        let fixture = Fixture::duration(ProviderBehavior::DurationNetworkFailure).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::HumanRequired {
                error_class: ProviderErrorClass::Network,
                ..
            }
        ));
        let retry_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM scheduled_jobs WHERE job_kind IN ('retry', 'recovery')",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(retry_jobs, 0);
    }

    #[tokio::test]
    async fn abandoned_duration_report_never_uses_completion_progress_for_recovery() {
        let fixture = Fixture::duration_recovering(ProviderBehavior::DurationSuccess).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::HumanRequired {
                error_class: ProviderErrorClass::InvalidRemoteState,
                ..
            }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.progress_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn submission_succeeds_only_after_draft_bound_verification() {
        let fixture = Fixture::submission(ProviderBehavior::SubmissionConfirmed).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 1);
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM submission_attempt_receipts), \
                    (SELECT COUNT(*) FROM submission_results)",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(counts, (1, 1));
        let completion: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT workflow.state, observation.completion_outcome, observation.diagnosis \
             FROM strict_completion_workflows AS workflow \
             INNER JOIN strict_completion_execution_observations AS observation \
                     ON observation.workflow_id = workflow.id \
             WHERE observation.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            completion,
            ("completed".to_owned(), Some("completed".to_owned()), None)
        );
    }

    #[tokio::test]
    async fn rejected_submission_requires_fresh_explicit_retry_without_replay() {
        let fixture = Fixture::submission(ProviderBehavior::SubmissionRejected).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            ScheduledExecutionOutcome::HumanRequired {
                error_class: ProviderErrorClass::InvalidRemoteState,
                ..
            }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 1);

        let persisted: (String, String, String, i64, Option<String>, i64) = sqlx::query_as(
            "SELECT execution.state, task.orchestration_state, workflow.state, \
                    json_extract(workflow.workflow_json, '$.attempts_started'), \
                    json_extract(workflow.workflow_json, '$.last_diagnosis'), \
                    (SELECT COUNT(*) FROM scheduled_jobs \
                     WHERE job_kind IN ('retry', 'recovery')) \
             FROM executions AS execution \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN strict_completion_workflows AS workflow ON workflow.task_id = task.id \
             WHERE execution.id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            persisted,
            (
                "human_required".to_owned(),
                "human_required".to_owned(),
                "active".to_owned(),
                1,
                Some("score_below_threshold".to_owned()),
                0,
            )
        );
    }

    #[tokio::test]
    async fn durable_submission_issues_each_operation_before_provider_dispatch() {
        let fixture =
            Fixture::durable_submission(ProviderBehavior::DurableSubmissionConfirmed).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.durable_calls.lock().unwrap(), 2);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 1);
        let operations: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT sequence, operation_type, state FROM question_session_operations \
             ORDER BY sequence",
        )
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            operations,
            vec![
                (
                    1,
                    "provider-alpha.answers-save".to_owned(),
                    "accepted".to_owned()
                ),
                (2, "provider-alpha.submit".to_owned(), "accepted".to_owned()),
            ]
        );
        let continuation: (String, i64) = sqlx::query_as(
            "SELECT phase, revision FROM question_session_continuations \
             WHERE execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(continuation, ("provider-alpha.answers-saved".to_owned(), 2));
        let session: (String, i64, Option<String>) = sqlx::query_as(
            "SELECT state, revision, closed_at FROM question_sessions WHERE execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(session.0, "consumed");
        assert_eq!(session.1, 3);
        assert!(session.2.is_some());
    }

    #[tokio::test]
    async fn durable_next_question_finishes_execution_but_returns_task_to_ready() {
        let fixture =
            Fixture::durable_submission(ProviderBehavior::DurableSubmissionNextQuestion).await;
        let outcome = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(outcome, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 0);
        assert_eq!(*fixture.provider.durable_calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 0);

        let state: (String, String, Option<i64>) = sqlx::query_as(
            "SELECT execution.state, task.orchestration_state, progress.percent \
             FROM executions AS execution \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN execution_progress AS progress ON progress.execution_id = execution.id \
             WHERE execution.id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(state, ("succeeded".to_owned(), "ready".to_owned(), None));

        let sessions: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT session.state, session.execution_id, continuation.phase \
             FROM question_sessions AS session \
             INNER JOIN question_session_continuations AS continuation \
               ON continuation.session_id = session.id \
             ORDER BY session.created_at, session.id",
        )
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(sessions.len(), 2);
        let execution_id = fixture.execution_id.to_string();
        assert_eq!(sessions[0].0, "consumed");
        assert_eq!(sessions[0].1.as_deref(), Some(execution_id.as_str()));
        assert_eq!(sessions[1].0, "active");
        assert_eq!(sessions[1].1, None);
        assert_eq!(sessions[1].2, "provider-alpha.questions-ready");
        let durable_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM question_session_transitions), \
                    (SELECT COUNT(*) FROM submission_attempt_receipts), \
                    (SELECT COUNT(*) FROM submission_results)",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(durable_counts, (1, 0, 0));
    }

    #[tokio::test]
    async fn unresolved_durable_operation_never_falls_through_to_overall_verification() {
        let mut fixture =
            Fixture::durable_submission(ProviderBehavior::DurableSubmissionAmbiguous).await;
        let first = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            first,
            ScheduledExecutionOutcome::RecoveryScheduled(_)
        ));
        assert_eq!(*fixture.provider.durable_calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 0);

        let recovery_at = Utc::now();
        fixture.job = fixture.claim_recovery(recovery_at).await;
        let recovered = fixture
            .runner
            .run_claimed(&fixture.job, recovery_at)
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            ScheduledExecutionOutcome::Deferred { .. }
        ));
        assert_eq!(*fixture.provider.durable_calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 0);
        let persisted: (String, String) = sqlx::query_as(
            "SELECT operation.state, session.state \
             FROM question_session_operations AS operation \
             INNER JOIN question_sessions AS session ON session.id = operation.session_id \
             WHERE session.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(persisted, ("ambiguous".to_owned(), "claimed".to_owned()));
    }

    #[tokio::test]
    async fn unknown_submission_executes_once_under_its_independent_verify_contract() {
        let fixture = Fixture::submission(ProviderBehavior::SubmissionConfirmed).await;
        sqlx::query("UPDATE tasks SET remote_state = 'unknown'")
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
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn receipt_with_pending_verification_recovers_without_resubmitting() {
        let mut fixture = Fixture::submission(ProviderBehavior::SubmissionPending).await;
        let first = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            first,
            ScheduledExecutionOutcome::RecoveryScheduled(_)
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 1);

        let recovery_at = Utc::now();
        fixture.job = fixture.claim_recovery(recovery_at).await;
        let recovered = fixture
            .runner
            .run_claimed(&fixture.job, recovery_at)
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            ScheduledExecutionOutcome::Deferred { .. }
        ));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn ambiguous_submit_error_recovers_by_verification_only() {
        let mut fixture = Fixture::submission(ProviderBehavior::SubmissionExecuteNetwork).await;
        let first = fixture
            .runner
            .run_claimed(&fixture.job, fixture.now)
            .await
            .unwrap();
        assert!(matches!(
            first,
            ScheduledExecutionOutcome::RecoveryScheduled(_)
        ));
        let receipt_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM submission_attempt_receipts")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(receipt_count, 0);

        let recovery_at = Utc::now();
        fixture.job = fixture.claim_recovery(recovery_at).await;
        let recovered = fixture
            .runner
            .run_claimed(&fixture.job, recovery_at)
            .await
            .unwrap();
        assert!(matches!(recovered, ScheduledExecutionOutcome::Succeeded(_)));
        assert_eq!(*fixture.provider.calls.lock().unwrap(), 1);
        assert_eq!(*fixture.provider.submission_verify_calls.lock().unwrap(), 1);
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
        let completion: (String, Option<String>) = sqlx::query_as(
            "SELECT workflow.state, observation.completion_outcome \
             FROM strict_completion_execution_observations AS observation \
             INNER JOIN strict_completion_workflows AS workflow \
                     ON workflow.id = observation.workflow_id \
             WHERE observation.execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            completion,
            ("completed".to_owned(), Some("completed".to_owned()))
        );
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
        let observation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM strict_completion_execution_observations \
             WHERE execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(observation_count, 0);
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
        secret_store: SqliteSecretStore,
        runner: TestRunner,
        provider: Arc<FakeExecution>,
        job: ScheduledJob,
        execution_id: ExecutionId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new(assessment: AssessmentClass, behavior: ProviderBehavior) -> Self {
            Self::new_with_config(assessment, behavior, runner_config()).await
        }

        async fn new_with_config(
            assessment: AssessmentClass,
            behavior: ProviderBehavior,
            config: ExecutionRunnerConfig,
        ) -> Self {
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
                submission_verify_calls: Mutex::new(0),
                received_provider_plan_artifacts: Mutex::new(Vec::new()),
                durable_calls: Arc::new(Mutex::new(0)),
                database: database.clone(),
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
                    duration_read: None,
                    question_inventory: None,
                    question_parse: None,
                    answer_resolve: None,
                    submission_build: None,
                    submission_execute: Some(provider.clone()),
                    submission_verify: Some(provider.clone()),
                    answer_history_harvest: None,
                    task_execution: Some(provider.clone()),
                    browser_bridge: None,
                })
                .unwrap();
            let secret_store = SqliteSecretStore::new(
                database.clone(),
                Arc::new(
                    SecretKeyring::new(
                        "execution-test-key".to_owned(),
                        BTreeMap::from([(
                            "execution-test-key".to_owned(),
                            SecretKey::new([17; 32]),
                        )]),
                    )
                    .unwrap(),
                ),
            );
            let runner = ScheduledExecutionRunner::new(
                Arc::new(registry),
                SqliteExecutionRepository::new(database.clone()),
                SqliteExecutionLeaseRepository::new(database.clone()),
                scheduler,
                SqliteProviderAccountRepository::new(database.clone()),
                SqliteTaskQueryRepository::new(database.clone()),
                config,
            )
            .unwrap()
            .with_question_session_artifacts(Arc::new(secret_store.clone()));
            let _ = account_id;
            Self {
                database,
                secret_store,
                runner,
                provider,
                job,
                execution_id,
                now,
            }
        }

        async fn recovering(behavior: ProviderBehavior) -> Self {
            Self::new(AssessmentClass::Routine, behavior)
                .await
                .enter_recovery()
                .await
        }

        async fn verified(behavior: ProviderBehavior) -> Self {
            let fixture = Self::new(AssessmentClass::Routine, behavior).await;
            sqlx::query(
                "UPDATE tasks SET capabilities_json = \
                 '[\"progress_read\",\"resource_execution\",\"execution_verify\"]'",
            )
            .execute(fixture.database.pool())
            .await
            .unwrap();
            fixture
        }

        async fn verified_recovering(behavior: ProviderBehavior) -> Self {
            Self::verified(behavior).await.enter_recovery().await
        }

        async fn duration(behavior: ProviderBehavior) -> Self {
            Self::new(AssessmentClass::Routine, behavior)
                .await
                .configure_duration()
                .await
        }

        async fn configure_duration(self) -> Self {
            sqlx::query("UPDATE tasks SET capabilities_json = '[\"duration_report\"]'")
                .execute(self.database.pool())
                .await
                .unwrap();
            sqlx::query(
                "UPDATE executions SET requested_capabilities_json = '[\"duration_report\"]' \
                 WHERE id = ?",
            )
            .bind(self.execution_id.to_string())
            .execute(self.database.pool())
            .await
            .unwrap();
            self
        }

        async fn duration_recovering(behavior: ProviderBehavior) -> Self {
            Self::duration(behavior).await.enter_recovery().await
        }

        async fn composite() -> Self {
            let fixture =
                Self::new(AssessmentClass::Routine, ProviderBehavior::CompositeSuccess).await;
            sqlx::query(
                "UPDATE tasks SET capabilities_json = \
                 '[\"resource_execution\",\"execution_verify\",\"duration_report\"]'",
            )
            .execute(fixture.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "UPDATE executions SET requested_capabilities_json = \
                 '[\"resource_execution\",\"duration_report\"]' WHERE id = ?",
            )
            .bind(fixture.execution_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
            sqlx::query("DELETE FROM execution_capability_steps WHERE execution_id = ?")
                .bind(fixture.execution_id.to_string())
                .execute(fixture.database.pool())
                .await
                .unwrap();
            for (position, capability) in
                [(1_i64, "duration_report"), (2_i64, "resource_execution")]
            {
                sqlx::query(
                    "INSERT INTO execution_capability_steps \
                     (execution_id, position, call_position, call_member_position, capability, state) \
                     VALUES (?, ?, ?, 1, ?, 'pending')",
                )
                .bind(fixture.execution_id.to_string())
                .bind(position)
                .bind(position)
                .bind(capability)
                .execute(fixture.database.pool())
                .await
                .unwrap();
            }
            fixture
        }

        async fn atomic_composite() -> Self {
            let fixture = Self::composite().await;
            sqlx::query(
                "UPDATE execution_capability_steps \
                 SET call_position = 1, call_member_position = position WHERE execution_id = ?",
            )
            .bind(fixture.execution_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
            fixture
        }

        async fn submission(behavior: ProviderBehavior) -> Self {
            let fixture = Self::new(AssessmentClass::Routine, behavior).await;
            let task_id = execution_id_to_task(
                &SqliteExecutionRepository::new(fixture.database.clone()),
                fixture.execution_id,
            )
            .await
            .unwrap();
            let draft = persist_submission_draft(&fixture.database, task_id, fixture.now).await;
            sqlx::query(
                "UPDATE tasks SET source_type = 'work', \
                    capabilities_json = '[\"submission_execute\",\"submission_verify\"]' \
                 WHERE id = ?",
            )
            .bind(task_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "UPDATE executions SET requested_capabilities_json = \
                    '[\"submission_execute\"]', submission_draft_id = ? WHERE id = ?",
            )
            .bind(draft.id.to_string())
            .bind(fixture.execution_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
            fixture
        }

        async fn durable_submission(behavior: ProviderBehavior) -> Self {
            let fixture = Self::submission(behavior).await;
            let repository = SqliteExecutionRepository::new(fixture.database.clone());
            let task_id = execution_id_to_task(&repository, fixture.execution_id)
                .await
                .unwrap();
            let draft = repository
                .find_execution_submission_draft(fixture.execution_id)
                .await
                .unwrap()
                .unwrap();
            let (owner_text, account_text): (String, String) = sqlx::query_as(
                "SELECT account.owner_user_id, account.id FROM tasks AS task \
                 INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
                 WHERE task.id = ?",
            )
            .bind(task_id.to_string())
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
            let owner: UserId = owner_text.parse().unwrap();
            let account_id: ProviderAccountId = account_text.parse().unwrap();
            let provider_id = ProviderId::new("provider-alpha").unwrap();
            let initial = b"attempt-v1";
            let initial_digest = ProviderQuestionReadContinuation::try_new(
                &provider_id,
                "provider-alpha.question-attempt.v1",
                "provider-alpha.questions-ready",
                SecretValue::new(initial.to_vec()),
                300,
            )
            .unwrap()
            .continuation_digest();
            let session = QuestionSession::active(
                owner,
                account_id,
                task_id,
                provider_id.clone(),
                "test".to_owned(),
                draft.question_snapshot_id,
                "provider-alpha.question-attempt.v1".to_owned(),
                initial_digest,
                fixture.now,
                fixture.now + chrono::Duration::minutes(5),
            )
            .unwrap();
            let sessions = SqliteQuestionSessionRepository::new(fixture.database.clone());
            sessions
                .create_question_session(
                    &session,
                    AuditActor::User(owner),
                    "durable-submission-session",
                )
                .await
                .unwrap();
            let access = SecretAccess {
                actor: SecretActor::CoreService("execution-worker-test"),
                correlation_id: "durable-submission-artifact".to_owned(),
                reason: "durable submission fixture".to_owned(),
            };
            fixture
                .secret_store
                .question_session_artifacts(provider_id)
                .attach_question_session_artifact(QuestionSessionArtifactAttachRequest {
                    session_id: session.id,
                    phase: "provider-alpha.questions-ready",
                    value: SecretValue::new(initial.to_vec()),
                    attached_at: fixture.now,
                    access: &access,
                })
                .await
                .unwrap();
            assert!(matches!(
                sessions
                    .claim_question_session_for_execution(
                        fixture.execution_id,
                        fixture.now,
                        "durable-submission-claim",
                    )
                    .await
                    .unwrap(),
                asterism_storage::QuestionSessionClaimOutcome::Claimed(_)
            ));
            fixture
        }

        async fn enter_recovery(mut self) -> Self {
            let task_id = execution_id_to_task(
                &SqliteExecutionRepository::new(self.database.clone()),
                self.execution_id,
            )
            .await
            .unwrap();
            let lease = ExecutionLease {
                task_id,
                execution_id: self.execution_id,
                worker_id: "execution-worker".to_owned(),
                expires_at: self.now + chrono::Duration::minutes(1),
            };
            SqliteExecutionLeaseRepository::new(self.database.clone())
                .try_acquire(&lease, self.now)
                .await
                .unwrap();
            SqliteExecutionRepository::new(self.database.clone())
                .start_attempt(ExecutionAttemptStartRequest {
                    execution_id: self.execution_id,
                    scheduler_job_id: self.job.id,
                    worker_id: "execution-worker",
                    at: self.now,
                    correlation_id: "stale-execution-test",
                })
                .await
                .unwrap();
            let expired = (self.now - chrono::Duration::seconds(1)).to_rfc3339();
            sqlx::query("UPDATE execution_leases SET expires_at = ? WHERE execution_id = ?")
                .bind(&expired)
                .bind(self.execution_id.to_string())
                .execute(self.database.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE scheduled_jobs SET lease_expires_at = ? WHERE id = ?")
                .bind(&expired)
                .bind(self.job.id.to_string())
                .execute(self.database.pool())
                .await
                .unwrap();
            self.database.recover_stale_work(self.now).await.unwrap();
            self.job = SqliteSchedulerRepository::new(self.database.clone())
                .claim_due_execution_jobs(
                    "recovery-worker",
                    self.now,
                    self.now + chrono::Duration::minutes(5),
                    1,
                )
                .await
                .unwrap()
                .pop()
                .unwrap();
            self
        }

        async fn claim_recovery(&self, now: Timestamp) -> ScheduledJob {
            SqliteSchedulerRepository::new(self.database.clone())
                .claim_due_execution_jobs(
                    "submission-recovery-worker",
                    now,
                    now + chrono::Duration::minutes(5),
                    1,
                )
                .await
                .unwrap()
                .pop()
                .unwrap()
        }

        async fn claim_retry(&self, now: Timestamp) -> ScheduledJob {
            SqliteSchedulerRepository::new(self.database.clone())
                .claim_due_execution_jobs(
                    "strict-completion-retry-worker",
                    now,
                    now + chrono::Duration::minutes(5),
                    1,
                )
                .await
                .unwrap()
                .pop()
                .unwrap()
        }

        async fn attach_provider_plan_artifact(
            &self,
        ) -> asterism_provider_api::ProviderExecutionPlanArtifact {
            let artifact = asterism_provider_api::ProviderExecutionPlanArtifact::try_new(
                ProviderId::new("provider-alpha").unwrap(),
                "provider-alpha.execution-plan.v1",
                serde_json::json!({"target_seconds": 120}),
            )
            .unwrap();
            sqlx::query(
                "INSERT INTO execution_provider_plan_artifacts \
                 (execution_id, provider_id, artifact_type, artifact_digest, payload_json, captured_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(self.execution_id.to_string())
            .bind(artifact.provider_id().as_str())
            .bind(artifact.artifact_type())
            .bind(artifact.artifact_digest().as_slice())
            .bind(serde_json::to_string(artifact.payload_sanitized()).unwrap())
            .bind(self.now.to_rfc3339())
            .execute(self.database.pool())
            .await
            .unwrap();
            artifact
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

    async fn persist_submission_draft(
        database: &Database,
        task_id: TaskId,
        now: Timestamp,
    ) -> SubmissionDraft {
        let question_id = QuestionId::new();
        let question = Question {
            id: question_id,
            task_id,
            remote_question_id: Some("question-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Question".to_owned(),
            options: vec![QuestionOption {
                id: "A".to_owned(),
                content: Some("Answer".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            }],
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            position: 1,
        };
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_version: "test".to_owned(),
            captured_at: now,
            questions: vec![question.clone()],
        };
        let repository = SqliteQuestionSnapshotRepository::new(database.clone());
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate = AnswerCandidateRecord {
            id: AnswerCandidateId::new(),
            question_snapshot_id: snapshot.id,
            candidate: AnswerCandidate {
                question_id,
                source: AnswerSource::Manual,
                answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                confidence: Some(AnswerConfidence::try_new(10_000).unwrap()),
                explanation: None,
                provenance_sanitized: serde_json::json!({"manual": true}),
            },
            created_at: now,
        };
        repository
            .save_answer_candidate_batch(std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: snapshot.id,
            provider_id: snapshot.provider_id,
            provider_version: "test".to_owned(),
            answer_coverage: asterism_domain::SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem {
                question,
                selected: SelectedAnswer {
                    candidate_id: candidate.id,
                    question_id,
                    answer: candidate.candidate.answer,
                    source: candidate.candidate.source,
                    confidence: candidate.candidate.confidence,
                },
            }],
            payload_preview: SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::Json,
                format: "provider-alpha.submission.v1".to_owned(),
                fields: vec![SubmissionPayloadFieldPreview {
                    question_id,
                    field_name: "answer[question-1]".to_owned(),
                }],
            },
            created_at: now,
        };
        repository.save_submission_draft(&draft).await.unwrap();
        draft
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
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            submission_draft_id: None,
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
        let completion_policy = schema.completion_policy_snapshot(&resolved, now).unwrap();
        let runtime_settings = ExecutionRuntimeSettingsSnapshot {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            resolved,
            sources,
            completion_policy,
            provider_revision: None,
            provider_account_revision: None,
            task_revision: None,
            captured_at: now,
        };
        SqliteExecutionRepository::new(database.clone())
            .schedule_execution(ExecutionScheduleRequest {
                execution: &execution,
                capability_plan: &execution.requested_capabilities,
                capability_call_starts: &[1],
                provider_plan_artifact: None,
                billing: None,
                runtime_settings: Some(ExecutionRuntimeSettingsResolution {
                    snapshot: &runtime_settings,
                    schema: &schema,
                }),
                strict_completion_retry: None,
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
                ProviderCapability::ExecutionVerify,
                ProviderCapability::DurationReport,
                ProviderCapability::SubmissionExecute,
                ProviderCapability::SubmissionVerify,
            ]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        }
    }

    const fn runner_config() -> ExecutionRunnerConfig {
        ExecutionRunnerConfig {
            execution_lease_ttl: StdDuration::from_mins(1),
            heartbeat_interval: StdDuration::from_secs(10),
            global_concurrency_limit: 8,
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
