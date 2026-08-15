use std::{collections::BTreeMap, str::FromStr};

use asterism_domain::{
    AttemptResult, AuditActor, AuditRecordId, CreditReservationState, Execution, ExecutionAttempt,
    ExecutionAttemptId, ExecutionId, ExecutionLogEvent, ExecutionProgress, ExecutionStage,
    ExecutionState, LogLevel, OrchestrationState, ProviderAccountId, ProviderId, ScheduleId,
    StrictCompletionState, StrictCompletionWorkflow, SubmissionAttemptReceipt, SubmissionDraft,
    TaskCapability, TaskId, Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_provider_api::{
    ProviderExecutionPlanArtifact, ProviderRuntimeSettingSource, ProviderRuntimeSettingsPatch,
    ProviderSettingValue, ResolvedProviderRuntimeSettings,
};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::Row;

use crate::credit::settle_execution_reservation;
use crate::outbox::enqueue_in_transaction;
use crate::question_session::{
    claim_optional_question_session_for_scheduled_execution,
    consume_question_session_for_succeeded_execution,
};
use crate::{
    Database, ExecutionAtomicMutation, ExecutionAtomicMutationIssueOutcome,
    ExecutionAtomicMutationIssueRequest, ExecutionAtomicMutationReceiptOutcome,
    ExecutionAtomicMutationReceiptRequest, ExecutionAtomicMutationRepository,
    ExecutionAttemptFinishRequest, ExecutionAttemptStartRequest, ExecutionBillingReservation,
    ExecutionCapabilityCallMutation, ExecutionCapabilityStep, ExecutionCapabilityStepIssueOutcome,
    ExecutionCapabilityStepMutation, ExecutionCapabilityStepRepository,
    ExecutionCapabilityStepState, ExecutionLogAppendRequest, ExecutionProgressUpdate,
    ExecutionQueryRepository, ExecutionQuestionStepFinishRequest, ExecutionRecoveryFinishRequest,
    ExecutionRepository, ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot,
    ExecutionScheduleOutcome, ExecutionScheduleRequest, ExecutionStrictCompletionRetryConfirmation,
    ExecutionStrictCompletionRetryRequest, ExecutionSubmissionRepository,
    ExecutionVerificationRecoveryRepository, StorageError, SubmissionDraftRepository,
    SubmissionReceiptPersistRequest, SubmissionResultPersistRequest, SubmissionResultRepository,
    VerificationRecoveryStartRequest,
};
use crate::{ExecutionDetail, ExecutionLogPage, ExecutionPage, SqliteQuestionSnapshotRepository};

const MAX_EXECUTION_ATTEMPTS: usize = 1_000;
const MAX_EXECUTION_PAGE_SIZE: u32 = 200;
const MAX_EXECUTION_OFFSET: u64 = 1_000_000;
const MAX_EXECUTION_LOG_PAGE_SIZE: u32 = 200;
const MAX_EXECUTION_LOG_OFFSET: u64 = 1_000_000;
const MAX_PROVIDER_LOGS_PER_ATTEMPT: i64 = 1_000;
const MAX_EXECUTION_LOG_METADATA_BYTES: usize = 8 * 1_024;
const MAX_EXECUTION_SETTINGS_JSON_BYTES: usize = 64 * 1_024;

#[derive(Clone, Debug)]
pub struct SqliteExecutionRepository {
    database: Database,
}

impl SqliteExecutionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    pub(crate) const fn database(&self) -> &Database {
        &self.database
    }
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "the scheduling transaction keeps execution, phase plan, settings, billing, job, audit and outbox writes visibly atomic"
)]
impl ExecutionRepository for SqliteExecutionRepository {
    async fn find_idempotent_execution(
        &self,
        idempotency_scope: &str,
        idempotency_key: &str,
    ) -> Result<Option<Execution>, StorageError> {
        validate_idempotency_tokens(idempotency_scope, idempotency_key)?;
        sqlx::query(
            "SELECT id, task_id, requested_capabilities_json, submission_draft_id, requested_by, request_source, quote_id, \
                    state, scheduled_at, started_at, finished_at, created_at FROM executions \
             WHERE idempotency_scope = ? AND idempotency_key = ?",
        )
        .bind(idempotency_scope)
        .bind(idempotency_key)
        .fetch_optional(self.database.pool())
        .await?
        .map(|row| decode_execution(&row))
        .transpose()
    }

    async fn schedule_execution(
        &self,
        request: ExecutionScheduleRequest<'_>,
    ) -> Result<ExecutionScheduleOutcome, StorageError> {
        validate_schedule_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;

        if let Some(existing) = find_idempotent_execution_in_transaction(
            &mut transaction,
            request.idempotency_scope,
            request.idempotency_key,
        )
        .await?
        {
            let same_confirmation = persisted_strict_retry_matches(
                &mut transaction,
                existing.id,
                request.strict_completion_retry,
            )
            .await?;
            transaction.commit().await?;
            return if same_request(&existing, request.execution) && same_confirmation {
                Ok(ExecutionScheduleOutcome::Existing(existing))
            } else {
                Ok(ExecutionScheduleOutcome::IdempotencyConflict)
            };
        }

        if !submission_draft_is_available(&mut transaction, request.execution).await? {
            transaction.rollback().await?;
            return Ok(ExecutionScheduleOutcome::SubmissionDraftConflict);
        }

        if !strict_completion_retry_is_valid(&mut transaction, &request).await? {
            transaction.rollback().await?;
            return Ok(ExecutionScheduleOutcome::StrictCompletionRetryConflict);
        }

        if let Some(resolution) = request.runtime_settings
            && !runtime_settings_match_current_layers(
                &mut transaction,
                request.execution.task_id,
                request.execution.created_at,
                &resolution,
            )
            .await?
        {
            transaction.rollback().await?;
            return Ok(ExecutionScheduleOutcome::RuntimeSettingsConflict);
        }

        if !transition_task_to_scheduled(&mut transaction, &request).await? {
            transaction.rollback().await?;
            return Ok(ExecutionScheduleOutcome::TaskStateConflict);
        }

        if let Some(billing) = request.billing.as_ref() {
            insert_quote_and_reserve_balance(&mut transaction, billing).await?;
        }

        let execution = request.execution;
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_capabilities_json, submission_draft_id, requested_by, request_source, quote_id, state, \
              scheduled_at, started_at, finished_at, created_at, idempotency_scope, idempotency_key) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(execution.id.to_string())
        .bind(execution.task_id.to_string())
        .bind(serde_json::to_string(&execution.requested_capabilities)?)
        .bind(execution.submission_draft_id.map(|id| id.to_string()))
        .bind(execution.requested_by.map(|id| id.to_string()))
        .bind(enum_name(execution.request_source)?)
        .bind(execution.quote_id.map(|id| id.to_string()))
        .bind(enum_name(execution.state)?)
        .bind(execution.scheduled_at.map(encode_timestamp))
        .bind(execution.started_at.map(encode_timestamp))
        .bind(execution.finished_at.map(encode_timestamp))
        .bind(encode_timestamp(execution.created_at))
        .bind(request.idempotency_scope)
        .bind(request.idempotency_key)
        .execute(&mut *transaction)
        .await?;

        if let Some(confirmation) = request.strict_completion_retry {
            sqlx::query(
                "INSERT INTO execution_strict_completion_retry_confirmations \
                 (execution_id, workflow_id, workflow_revision, confirmed_by, confirmed_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(execution.id.to_string())
            .bind(confirmation.workflow_id.to_string())
            .bind(i64::from(confirmation.expected_revision))
            .bind(
                execution
                    .requested_by
                    .expect("validated retry confirmation owner")
                    .to_string(),
            )
            .bind(encode_timestamp(execution.created_at))
            .execute(&mut *transaction)
            .await?;
        }

        if !claim_optional_question_session_for_scheduled_execution(
            &mut transaction,
            execution.id,
            execution.created_at,
            request.correlation_id,
        )
        .await?
        {
            transaction.rollback().await?;
            return Ok(ExecutionScheduleOutcome::SubmissionDraftConflict);
        }

        let mut call_index = 0_usize;
        let mut call_member_index = 0_usize;
        for (index, capability) in request.capability_plan.iter().copied().enumerate() {
            if request
                .capability_call_starts
                .get(call_index + 1)
                .is_some_and(|start| usize::from(*start) == index + 1)
            {
                call_index += 1;
                call_member_index = 0;
            }
            sqlx::query(
                "INSERT INTO execution_capability_steps \
                 (execution_id, position, call_position, call_member_position, capability, state) \
                 VALUES (?, ?, ?, ?, ?, 'pending')",
            )
            .bind(execution.id.to_string())
            .bind(i64::try_from(index + 1).expect("capability plan is bounded"))
            .bind(i64::try_from(call_index + 1).expect("capability calls are bounded"))
            .bind(i64::try_from(call_member_index + 1).expect("capability call is bounded"))
            .bind(enum_name(capability)?)
            .execute(&mut *transaction)
            .await?;
            call_member_index += 1;
        }

        if let Some(resolution) = request.runtime_settings {
            insert_runtime_settings_snapshot(&mut transaction, execution.id, resolution.snapshot)
                .await?;
        }
        if let Some(artifact) = request.provider_plan_artifact {
            insert_provider_plan_artifact(
                &mut transaction,
                execution.id,
                execution.created_at,
                artifact,
            )
            .await?;
        }

        if let Some(billing) = request.billing.as_ref() {
            insert_credit_reservation(&mut transaction, billing, request.correlation_id).await?;
        }

        let job_id = ScheduleId::new();
        let job_kind = ScheduledJobKind::Execution {
            execution_id: execution.id,
        };
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, created_at, updated_at) \
             VALUES (?, 'execution', ?, ?, 'pending', 0, ?, ?, ?)",
        )
        .bind(job_id.to_string())
        .bind(serde_json::to_string(&job_kind)?)
        .bind(encode_timestamp(execution.scheduled_at.expect("validated scheduled_at")))
        .bind(format!("execution:{}", execution.id))
        .bind(encode_timestamp(execution.created_at))
        .bind(encode_timestamp(execution.created_at))
        .execute(&mut *transaction)
        .await?;

        insert_request_audit(&mut transaction, &request).await?;
        let event = EventEnvelope::at(
            request.correlation_id,
            DomainEvent::ExecutionStateChanged {
                execution_id: execution.id,
                state: execution.state,
            },
            execution.created_at,
        );
        enqueue_in_transaction(&mut transaction, &event).await?;
        transaction.commit().await?;
        Ok(ExecutionScheduleOutcome::Created(execution.clone()))
    }

    async fn find_execution(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<Execution>, StorageError> {
        sqlx::query(
            "SELECT id, task_id, requested_capabilities_json, submission_draft_id, requested_by, request_source, quote_id, \
                    state, scheduled_at, started_at, finished_at, created_at \
             FROM executions WHERE id = ?",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?
        .map(|row| decode_execution(&row))
        .transpose()
    }

    async fn find_execution_runtime_settings(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionRuntimeSettingsSnapshot>, StorageError> {
        sqlx::query(
            "SELECT settings.provider_id AS snapshot_provider_id, settings.schema_version, \
                    settings.resolved_settings_json, settings.sources_json, \
                    settings.provider_revision, settings.provider_account_revision, \
                    settings.task_revision, settings.completion_policy_json, settings.captured_at, \
                    execution.created_at AS execution_created_at, \
                    account.provider_id AS actual_provider_id \
             FROM execution_runtime_settings AS settings \
             INNER JOIN executions AS execution ON execution.id = settings.execution_id \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE settings.execution_id = ?",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?
        .as_ref()
        .map(decode_runtime_settings_snapshot)
        .transpose()
    }

    async fn find_execution_provider_plan_artifact(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ProviderExecutionPlanArtifact>, StorageError> {
        let row = sqlx::query(
            "SELECT plan.provider_id AS plan_provider_id, plan.artifact_type, \
                    plan.artifact_digest, plan.payload_json, plan.captured_at, \
                    execution.created_at AS execution_created_at, \
                    account.provider_id AS actual_provider_id \
             FROM execution_provider_plan_artifacts AS plan \
             INNER JOIN executions AS execution ON execution.id = plan.execution_id \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE plan.execution_id = ?",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_provider_plan_artifact).transpose()
    }

    async fn find_active_execution_attempt_id(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionAttemptId>, StorageError> {
        let attempt_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM execution_attempts \
             WHERE execution_id = ? AND finished_at IS NULL \
             ORDER BY attempt_no DESC LIMIT 1",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        attempt_id
            .map(|value| {
                ExecutionAttemptId::from_str(&value)
                    .map_err(|error| StorageError::InvalidData(error.to_string()))
            })
            .transpose()
    }

    async fn find_execution_strict_completion_retry_confirmation(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionStrictCompletionRetryConfirmation>, StorageError> {
        find_strict_retry_confirmation(self.database.pool(), execution_id).await
    }

    async fn start_attempt(
        &self,
        request: ExecutionAttemptStartRequest<'_>,
    ) -> Result<ExecutionAttempt, StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;

        let execution_row = select_execution(&mut transaction, request.execution_id).await?;
        let execution = decode_execution(&execution_row)?;
        if execution.state == ExecutionState::Running {
            let attempt = find_active_attempt(&mut transaction, request.execution_id).await?;
            transaction.commit().await?;
            return Ok(attempt);
        }
        let (attempt, progress) = create_attempt(&mut transaction, &execution, request.at).await?;
        record_attempt_started(&mut transaction, &attempt, &progress, &request).await?;
        transaction.commit().await?;
        Ok(attempt)
    }

    async fn update_progress(
        &self,
        request: ExecutionProgressUpdate<'_>,
    ) -> Result<bool, StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        validate_progress(request.progress)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_execution_lease(
            &mut transaction,
            request.progress.execution_id,
            request.worker_id,
            request.progress.updated_at,
        )
        .await?;
        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM executions WHERE id = ? AND state = 'running'",
        )
        .bind(request.progress.execution_id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if running != 1 {
            return Err(StorageError::ExecutionStateConflict);
        }
        let changed = upsert_newer_progress(&mut transaction, request.progress).await?;
        if changed {
            enqueue_progress_event(&mut transaction, request.progress, request.correlation_id)
                .await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    async fn append_log(&self, request: ExecutionLogAppendRequest<'_>) -> Result<(), StorageError> {
        validate_log_append_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_execution_lease(
            &mut transaction,
            request.execution_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM execution_attempts AS attempt \
             INNER JOIN executions AS execution ON execution.id = attempt.execution_id \
             WHERE attempt.id = ? AND attempt.execution_id = ? \
               AND attempt.finished_at IS NULL AND attempt.started_at <= ? \
               AND execution.state = 'running'",
        )
        .bind(request.attempt_id.to_string())
        .bind(request.execution_id.to_string())
        .bind(encode_timestamp(request.at))
        .fetch_one(&mut *transaction)
        .await?;
        if active != 1 {
            return Err(StorageError::ExecutionAttemptNotActive);
        }
        let log_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM execution_logs WHERE attempt_id = ?")
                .bind(request.attempt_id.to_string())
                .fetch_one(&mut *transaction)
                .await?;
        if log_count >= MAX_PROVIDER_LOGS_PER_ATTEMPT {
            return Err(StorageError::InvalidData(
                "Provider execution log count exceeds the supported bound".to_owned(),
            ));
        }
        insert_execution_log(
            &mut transaction,
            ExecutionLogInsert {
                execution_id: request.execution_id,
                attempt_id: Some(request.attempt_id),
                at: request.at,
                level: request.level,
                stage: request.stage,
                message: request.message,
                provider_trace_id: request.provider_trace_id,
                metadata_sanitized: request.metadata_sanitized,
            },
            request.correlation_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn finish_attempt(
        &self,
        request: ExecutionAttemptFinishRequest<'_>,
    ) -> Result<Execution, StorageError> {
        validate_finish_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            false,
        )
        .await?;
        let execution_row = select_execution(&mut transaction, request.execution_id).await?;
        let mut execution = decode_execution(&execution_row)?;
        if execution.state != ExecutionState::Running {
            return Err(StorageError::ExecutionStateConflict);
        }
        apply_attempt_finish(
            &mut transaction,
            &mut execution,
            &request,
            ExecutionState::Running,
            OrchestrationState::Running,
            None,
        )
        .await?;
        record_attempt_finished(
            &mut transaction,
            &request,
            "execution_attempt_finished",
            "execution attempt finished",
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }

    async fn finish_question_step(
        &self,
        request: ExecutionQuestionStepFinishRequest<'_>,
    ) -> Result<Execution, StorageError> {
        validate_question_step_finish_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            false,
        )
        .await?;
        if !question_transition_is_ready(&mut transaction, &request).await? {
            return Err(StorageError::ExecutionStateConflict);
        }
        let execution_row = select_execution(&mut transaction, request.execution_id).await?;
        let mut execution = decode_execution(&execution_row)?;
        let (expected_execution_state, expected_task_state) = match execution.state {
            ExecutionState::Running => (ExecutionState::Running, OrchestrationState::Running),
            ExecutionState::Recovering => {
                (ExecutionState::Recovering, OrchestrationState::Recovering)
            }
            _ => return Err(StorageError::ExecutionStateConflict),
        };
        let finish = ExecutionAttemptFinishRequest {
            execution_id: request.execution_id,
            attempt_id: request.attempt_id,
            scheduler_job_id: request.scheduler_job_id,
            worker_id: request.worker_id,
            final_state: ExecutionState::Succeeded,
            result: AttemptResult::Succeeded,
            error_class: None,
            provider_trace_id: None,
            retry_at: None,
            progress: request.progress,
            at: request.at,
            correlation_id: request.correlation_id,
        };
        apply_attempt_finish(
            &mut transaction,
            &mut execution,
            &finish,
            expected_execution_state,
            expected_task_state,
            Some(OrchestrationState::Ready),
        )
        .await?;
        record_attempt_finished(
            &mut transaction,
            &finish,
            "execution_question_step_finished",
            "next Question materialized; execution step finished",
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }

    async fn finish_recovery(
        &self,
        request: ExecutionRecoveryFinishRequest<'_>,
    ) -> Result<Execution, StorageError> {
        validate_recovery_finish_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            false,
        )
        .await?;
        let execution_row = select_execution(&mut transaction, request.execution_id).await?;
        let mut execution = decode_execution(&execution_row)?;
        if execution.state != ExecutionState::Recovering {
            return Err(StorageError::ExecutionStateConflict);
        }
        let attempt = find_active_attempt(&mut transaction, request.execution_id).await?;
        let finish = ExecutionAttemptFinishRequest {
            execution_id: request.execution_id,
            attempt_id: attempt.id,
            scheduler_job_id: request.scheduler_job_id,
            worker_id: request.worker_id,
            final_state: request.final_state,
            result: if request.final_state == ExecutionState::Succeeded {
                AttemptResult::Succeeded
            } else {
                AttemptResult::Failed
            },
            error_class: request.error_class,
            provider_trace_id: request.provider_trace_id,
            retry_at: request.retry_at,
            progress: request.progress,
            at: request.at,
            correlation_id: request.correlation_id,
        };
        apply_attempt_finish(
            &mut transaction,
            &mut execution,
            &finish,
            ExecutionState::Recovering,
            OrchestrationState::Recovering,
            None,
        )
        .await?;
        record_attempt_finished(
            &mut transaction,
            &finish,
            "execution_recovery_finished",
            "execution recovery finished",
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }
}

#[async_trait]
impl ExecutionCapabilityStepRepository for SqliteExecutionRepository {
    async fn find_execution_capability_steps(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Vec<ExecutionCapabilityStep>, StorageError> {
        let rows = sqlx::query(
            "SELECT execution_id, position, call_position, call_member_position, capability, state, issued_attempt_id, \
                    issued_at, succeeded_at FROM execution_capability_steps \
             WHERE execution_id = ? ORDER BY position",
        )
        .bind(execution_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        rows.iter().map(decode_capability_step).collect()
    }

    async fn issue_execution_capability_step(
        &self,
        request: ExecutionCapabilityStepMutation<'_>,
    ) -> Result<ExecutionCapabilityStepIssueOutcome, StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let active = find_active_attempt(&mut transaction, request.execution_id).await?;
        if active.id != request.attempt_id {
            return Err(StorageError::ExecutionAttemptNotActive);
        }
        let step: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT state, issued_attempt_id FROM execution_capability_steps \
             WHERE execution_id = ? AND capability = ?",
        )
        .bind(request.execution_id.to_string())
        .bind(enum_name(request.capability)?)
        .fetch_optional(&mut *transaction)
        .await?;
        match step
            .as_ref()
            .map(|(state, attempt_id)| (state.as_str(), attempt_id.as_deref()))
        {
            Some(("issued", Some(issued_attempt_id)))
                if issued_attempt_id == request.attempt_id.to_string() =>
            {
                transaction.rollback().await?;
                Ok(ExecutionCapabilityStepIssueOutcome::AlreadyIssued)
            }
            Some(("pending", None)) => {
                let changed = sqlx::query(
                    "UPDATE execution_capability_steps SET state = 'issued', \
                            issued_attempt_id = ?, issued_at = ? \
                     WHERE execution_id = ? AND capability = ? AND state = 'pending' \
                       AND NOT EXISTS ( \
                         SELECT 1 FROM execution_capability_steps AS earlier \
                         WHERE earlier.execution_id = execution_capability_steps.execution_id \
                           AND earlier.position < execution_capability_steps.position \
                           AND earlier.state != 'succeeded')",
                )
                .bind(request.attempt_id.to_string())
                .bind(encode_timestamp(request.at))
                .bind(request.execution_id.to_string())
                .bind(enum_name(request.capability)?)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
                if changed != 1 {
                    return Err(StorageError::ExecutionStateConflict);
                }
                transaction.commit().await?;
                Ok(ExecutionCapabilityStepIssueOutcome::Issued)
            }
            Some(("issued" | "succeeded" | "ambiguous", _) | _) | None => {
                Err(StorageError::ExecutionStateConflict)
            }
        }
    }

    async fn succeed_execution_capability_step(
        &self,
        request: ExecutionCapabilityStepMutation<'_>,
    ) -> Result<(), StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let changed = sqlx::query(
            "UPDATE execution_capability_steps SET state = 'succeeded', succeeded_at = ? \
             WHERE execution_id = ? AND capability = ? AND state = 'issued' \
               AND issued_attempt_id = ?",
        )
        .bind(encode_timestamp(request.at))
        .bind(request.execution_id.to_string())
        .bind(enum_name(request.capability)?)
        .bind(request.attempt_id.to_string())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::ExecutionStateConflict);
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn issue_execution_capability_call(
        &self,
        request: ExecutionCapabilityCallMutation<'_>,
    ) -> Result<ExecutionCapabilityStepIssueOutcome, StorageError> {
        validate_capability_call_mutation(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let active = find_active_attempt(&mut transaction, request.execution_id).await?;
        if active.id != request.attempt_id {
            return Err(StorageError::ExecutionAttemptNotActive);
        }
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT state, issued_attempt_id FROM execution_capability_steps \
             WHERE execution_id = ? AND call_position = ? ORDER BY call_member_position",
        )
        .bind(request.execution_id.to_string())
        .bind(i64::from(request.call_position))
        .fetch_all(&mut *transaction)
        .await?;
        let persisted_capabilities = find_capability_call(
            &mut transaction,
            request.execution_id,
            request.call_position,
        )
        .await?;
        if persisted_capabilities != request.capabilities {
            return Err(StorageError::ExecutionStateConflict);
        }
        let attempt_id = request.attempt_id.to_string();
        if !rows.is_empty()
            && rows
                .iter()
                .all(|(state, issued)| state == "issued" && issued.as_deref() == Some(&attempt_id))
        {
            transaction.rollback().await?;
            return Ok(ExecutionCapabilityStepIssueOutcome::AlreadyIssued);
        }
        if rows.len() != request.capabilities.len()
            || rows
                .iter()
                .any(|(state, issued)| state != "pending" || issued.is_some())
        {
            return Err(StorageError::ExecutionStateConflict);
        }
        let changed = sqlx::query(
            "UPDATE execution_capability_steps SET state = 'issued', issued_attempt_id = ?, issued_at = ? \
             WHERE execution_id = ? AND call_position = ? AND state = 'pending' \
               AND NOT EXISTS ( \
                 SELECT 1 FROM execution_capability_steps AS earlier \
                 WHERE earlier.execution_id = execution_capability_steps.execution_id \
                   AND earlier.call_position < execution_capability_steps.call_position \
                   AND earlier.state != 'succeeded')",
        )
        .bind(&attempt_id)
        .bind(encode_timestamp(request.at))
        .bind(request.execution_id.to_string())
        .bind(i64::from(request.call_position))
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != u64::try_from(request.capabilities.len()).expect("call length is bounded") {
            return Err(StorageError::ExecutionStateConflict);
        }
        transaction.commit().await?;
        Ok(ExecutionCapabilityStepIssueOutcome::Issued)
    }

    async fn succeed_execution_capability_call(
        &self,
        request: ExecutionCapabilityCallMutation<'_>,
    ) -> Result<(), StorageError> {
        validate_capability_call_mutation(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let persisted_capabilities = find_capability_call(
            &mut transaction,
            request.execution_id,
            request.call_position,
        )
        .await?;
        if persisted_capabilities != request.capabilities {
            return Err(StorageError::ExecutionStateConflict);
        }
        let changed = sqlx::query(
            "UPDATE execution_capability_steps SET state = 'succeeded', succeeded_at = ? \
             WHERE execution_id = ? AND call_position = ? AND state = 'issued' \
               AND issued_attempt_id = ?",
        )
        .bind(encode_timestamp(request.at))
        .bind(request.execution_id.to_string())
        .bind(i64::from(request.call_position))
        .bind(request.attempt_id.to_string())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != u64::try_from(request.capabilities.len()).expect("call length is bounded") {
            return Err(StorageError::ExecutionStateConflict);
        }
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl ExecutionAtomicMutationRepository for SqliteExecutionRepository {
    async fn find_execution_atomic_mutations(
        &self,
        execution_id: ExecutionId,
        attempt_id: ExecutionAttemptId,
    ) -> Result<Vec<ExecutionAtomicMutation>, StorageError> {
        let rows = sqlx::query(
            "SELECT execution_id, execution_attempt_id, ordinal, scheduler_job_id, worker_id, \
                    operation_type, request_digest, response_digest, accepted, issued_at, received_at \
             FROM execution_atomic_mutations WHERE execution_id = ? AND execution_attempt_id = ? \
             ORDER BY ordinal",
        )
        .bind(execution_id.to_string())
        .bind(attempt_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        rows.iter().map(decode_atomic_mutation).collect()
    }

    async fn issue_execution_atomic_mutation(
        &self,
        request: ExecutionAtomicMutationIssueRequest<'_>,
    ) -> Result<ExecutionAtomicMutationIssueOutcome, StorageError> {
        validate_atomic_mutation_issue(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let active = find_active_attempt(&mut transaction, request.execution_id).await?;
        if active.id != request.attempt_id {
            return Err(StorageError::ExecutionAttemptNotActive);
        }

        let existing = select_atomic_mutation(
            &mut transaction,
            request.execution_id,
            request.attempt_id,
            request.ordinal,
        )
        .await?;
        if let Some(existing) = existing {
            let identical = existing.scheduler_job_id == request.scheduler_job_id
                && existing.worker_id == request.worker_id
                && existing.operation_type == request.operation_type
                && existing.request_digest == request.request_digest;
            transaction.rollback().await?;
            return if identical {
                Ok(ExecutionAtomicMutationIssueOutcome::AlreadyIssued(existing))
            } else {
                Err(StorageError::ExecutionStateConflict)
            };
        }

        let (last_ordinal, incomplete): (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(MAX(ordinal), 0), \
                    COALESCE(SUM(CASE WHEN received_at IS NULL THEN 1 ELSE 0 END), 0) \
             FROM execution_atomic_mutations WHERE execution_id = ? AND execution_attempt_id = ?",
        )
        .bind(request.execution_id.to_string())
        .bind(request.attempt_id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if incomplete != 0 || i64::from(request.ordinal) != last_ordinal + 1 {
            return Err(StorageError::ExecutionStateConflict);
        }

        sqlx::query(
            "INSERT INTO execution_atomic_mutations \
             (execution_id, execution_attempt_id, ordinal, scheduler_job_id, worker_id, \
              operation_type, request_digest, issued_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request.execution_id.to_string())
        .bind(request.attempt_id.to_string())
        .bind(i64::from(request.ordinal))
        .bind(request.scheduler_job_id.to_string())
        .bind(request.worker_id)
        .bind(request.operation_type)
        .bind(request.request_digest.as_slice())
        .bind(encode_timestamp(request.at))
        .execute(&mut *transaction)
        .await?;
        insert_worker_audit(
            &mut transaction,
            request.execution_id,
            request.worker_id,
            "execution_atomic_mutation_issued",
            request.at,
            request.correlation_id,
            serde_json::json!({
                "attempt_id": request.attempt_id,
                "ordinal": request.ordinal,
                "operation_type": request.operation_type,
                "request_digest": "[HASHED]",
            }),
        )
        .await?;
        let mutation = ExecutionAtomicMutation {
            execution_id: request.execution_id,
            attempt_id: request.attempt_id,
            ordinal: request.ordinal,
            scheduler_job_id: request.scheduler_job_id,
            worker_id: request.worker_id.to_owned(),
            operation_type: request.operation_type.to_owned(),
            request_digest: request.request_digest,
            response_digest: None,
            accepted: None,
            issued_at: request.at,
            received_at: None,
        };
        transaction.commit().await?;
        Ok(ExecutionAtomicMutationIssueOutcome::Issued(mutation))
    }

    async fn record_execution_atomic_mutation_receipt(
        &self,
        request: ExecutionAtomicMutationReceiptRequest<'_>,
    ) -> Result<ExecutionAtomicMutationReceiptOutcome, StorageError> {
        validate_atomic_mutation_receipt(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let active = find_active_attempt(&mut transaction, request.execution_id).await?;
        if active.id != request.attempt_id {
            return Err(StorageError::ExecutionAttemptNotActive);
        }
        let Some(mut mutation) = select_atomic_mutation(
            &mut transaction,
            request.execution_id,
            request.attempt_id,
            request.ordinal,
        )
        .await?
        else {
            return Err(StorageError::ExecutionStateConflict);
        };
        if mutation.scheduler_job_id != request.scheduler_job_id
            || mutation.worker_id != request.worker_id
        {
            return Err(StorageError::ExecutionStateConflict);
        }
        if mutation.received_at.is_some() {
            let identical = mutation.response_digest == Some(request.response_digest)
                && mutation.accepted == Some(request.accepted);
            transaction.rollback().await?;
            return if identical {
                Ok(ExecutionAtomicMutationReceiptOutcome::AlreadyRecorded(
                    mutation,
                ))
            } else {
                Err(StorageError::ExecutionStateConflict)
            };
        }
        if request.at < mutation.issued_at {
            return Err(StorageError::InvalidData(
                "execution atomic mutation receipt predates its issue".to_owned(),
            ));
        }
        let changed = sqlx::query(
            "UPDATE execution_atomic_mutations SET response_digest = ?, accepted = ?, received_at = ? \
             WHERE execution_id = ? AND execution_attempt_id = ? AND ordinal = ? \
               AND response_digest IS NULL AND accepted IS NULL AND received_at IS NULL",
        )
        .bind(request.response_digest.as_slice())
        .bind(request.accepted)
        .bind(encode_timestamp(request.at))
        .bind(request.execution_id.to_string())
        .bind(request.attempt_id.to_string())
        .bind(i64::from(request.ordinal))
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::ExecutionStateConflict);
        }
        insert_worker_audit(
            &mut transaction,
            request.execution_id,
            request.worker_id,
            "execution_atomic_mutation_receipt_recorded",
            request.at,
            request.correlation_id,
            serde_json::json!({
                "attempt_id": request.attempt_id,
                "ordinal": request.ordinal,
                "response_digest": "[HASHED]",
                "accepted": request.accepted,
            }),
        )
        .await?;
        mutation.response_digest = Some(request.response_digest);
        mutation.accepted = Some(request.accepted);
        mutation.received_at = Some(request.at);
        transaction.commit().await?;
        Ok(ExecutionAtomicMutationReceiptOutcome::Recorded(mutation))
    }
}

#[async_trait]
impl ExecutionSubmissionRepository for SqliteExecutionRepository {
    async fn find_execution_submission_draft(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<SubmissionDraft>, StorageError> {
        let binding: Option<(String, String)> = sqlx::query_as(
            "SELECT requested_by, submission_draft_id FROM executions \
             WHERE id = ? AND requested_by IS NOT NULL AND submission_draft_id IS NOT NULL",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        let Some((owner_id, draft_id)) = binding else {
            return Ok(None);
        };
        let owner_id = UserId::from_str(&owner_id)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let draft_id = asterism_domain::SubmissionDraftId::from_str(&draft_id)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        SqliteQuestionSnapshotRepository::new(self.database.clone())
            .find_owned_submission_draft(owner_id, draft_id)
            .await
    }

    async fn persist_submission_receipt(
        &self,
        request: SubmissionReceiptPersistRequest<'_>,
    ) -> Result<(), StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        request
            .record
            .receipt
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if request.record.receipt.received_at > request.at {
            return Err(invalid_submission_receipt());
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.record.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let bound: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM executions AS execution \
             INNER JOIN execution_attempts AS attempt ON attempt.execution_id = execution.id \
             WHERE execution.id = ? AND execution.submission_draft_id = ? \
               AND execution.state = 'running' AND attempt.id = ? AND attempt.finished_at IS NULL",
        )
        .bind(request.record.execution_id.to_string())
        .bind(request.record.submission_draft_id.to_string())
        .bind(request.record.execution_attempt_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        if bound.is_none() {
            return Err(invalid_submission_receipt());
        }
        let receipt_json = serde_json::to_string(&request.record.receipt)?;
        if receipt_json.is_empty() || receipt_json.len() > 64 * 1_024 {
            return Err(invalid_submission_receipt());
        }
        let receipt_bytes = receipt_json.len();
        let inserted = sqlx::query(
            "INSERT INTO submission_attempt_receipts \
             (execution_attempt_id, execution_id, submission_draft_id, receipt_json, \
              receipt_bytes, received_at) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(execution_attempt_id) DO NOTHING",
        )
        .bind(request.record.execution_attempt_id.to_string())
        .bind(request.record.execution_id.to_string())
        .bind(request.record.submission_draft_id.to_string())
        .bind(&receipt_json)
        .bind(i64::try_from(receipt_bytes).map_err(|_| invalid_submission_receipt())?)
        .bind(encode_timestamp(request.record.receipt.received_at))
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if inserted == 0 {
            let existing = sqlx::query(
                "SELECT execution_attempt_id, execution_id, submission_draft_id, receipt_json, \
                        receipt_bytes FROM submission_attempt_receipts \
                 WHERE execution_attempt_id = ?",
            )
            .bind(request.record.execution_attempt_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
            if decode_submission_receipt(&existing)? != *request.record {
                return Err(invalid_submission_receipt());
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn find_active_submission_receipt(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<SubmissionAttemptReceipt>, StorageError> {
        let row = sqlx::query(
            "SELECT receipt.execution_attempt_id, receipt.execution_id, \
                    receipt.submission_draft_id, receipt.receipt_json, receipt.receipt_bytes \
             FROM submission_attempt_receipts AS receipt \
             INNER JOIN execution_attempts AS attempt \
                ON attempt.id = receipt.execution_attempt_id \
               AND attempt.execution_id = receipt.execution_id \
             INNER JOIN executions AS execution ON execution.id = receipt.execution_id \
             WHERE receipt.execution_id = ? AND attempt.finished_at IS NULL \
               AND execution.submission_draft_id = receipt.submission_draft_id \
             ORDER BY attempt.attempt_no DESC LIMIT 1",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_submission_receipt).transpose()
    }

    async fn find_active_submission_attempt_id(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionAttemptId>, StorageError> {
        let attempt_id: Option<String> = sqlx::query_scalar(
            "SELECT attempt.id FROM execution_attempts AS attempt \
             INNER JOIN executions AS execution ON execution.id = attempt.execution_id \
             WHERE execution.id = ? AND execution.submission_draft_id IS NOT NULL \
               AND attempt.finished_at IS NULL ORDER BY attempt.attempt_no DESC LIMIT 1",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        attempt_id
            .map(|value| {
                ExecutionAttemptId::from_str(&value)
                    .map_err(|error| StorageError::InvalidData(error.to_string()))
            })
            .transpose()
    }

    async fn find_active_submission_result(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<asterism_domain::SubmissionResult>, StorageError> {
        let binding: Option<(String, String, String)> = sqlx::query_as(
            "SELECT execution.requested_by, execution.submission_draft_id, attempt.id \
             FROM executions AS execution \
             INNER JOIN execution_attempts AS attempt ON attempt.execution_id = execution.id \
             WHERE execution.id = ? AND execution.requested_by IS NOT NULL \
               AND execution.submission_draft_id IS NOT NULL AND attempt.finished_at IS NULL \
             ORDER BY attempt.attempt_no DESC LIMIT 1",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        let Some((owner_id, draft_id, attempt_id)) = binding else {
            return Ok(None);
        };
        let owner_id = UserId::from_str(&owner_id)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let draft_id = asterism_domain::SubmissionDraftId::from_str(&draft_id)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let attempt_id = ExecutionAttemptId::from_str(&attempt_id)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        Ok(SqliteQuestionSnapshotRepository::new(self.database.clone())
            .find_latest_owned_submission_result(owner_id, draft_id)
            .await?
            .filter(|result| {
                result.execution_id == execution_id && result.execution_attempt_id == attempt_id
            }))
    }

    async fn persist_submission_result(
        &self,
        request: SubmissionResultPersistRequest<'_>,
    ) -> Result<(), StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        if request.result.created_at > request.at {
            return Err(StorageError::InvalidData(
                "submission result timestamp is after persistence".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.result.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        transaction.commit().await?;
        if let Some(existing) = self
            .find_active_submission_result(request.result.execution_id)
            .await?
        {
            return if existing == *request.result {
                Ok(())
            } else {
                Err(StorageError::InvalidData(
                    "active submission attempt already has another result".to_owned(),
                ))
            };
        }
        SqliteQuestionSnapshotRepository::new(self.database.clone())
            .save_submission_result(request.result)
            .await
    }
}

#[async_trait]
impl ExecutionVerificationRecoveryRepository for SqliteExecutionRepository {
    #[allow(
        clippy::too_many_lines,
        reason = "the transaction keeps claim, lease, state, recovery job and outbox writes visibly atomic"
    )]
    async fn begin_verification_recovery(
        &self,
        request: VerificationRecoveryStartRequest<'_>,
    ) -> Result<Execution, StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        validate_progress(request.progress)?;
        if request.progress.execution_id != request.execution_id
            || request.progress.updated_at != request.at
            || request.progress.stage != ExecutionStage::Verifying
        {
            return Err(StorageError::InvalidData(
                "verification recovery progress is invalid".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        let row = select_execution(&mut transaction, request.execution_id).await?;
        let mut execution = decode_execution(&row)?;
        if execution.state != ExecutionState::Running {
            return Err(StorageError::ExecutionStateConflict);
        }
        let active_attempt = find_active_attempt(&mut transaction, request.execution_id).await?;
        if active_attempt.id != request.attempt_id {
            return Err(StorageError::ExecutionAttemptNotActive);
        }
        execution.state = ExecutionState::Recovering;
        let execution_changed = sqlx::query(
            "UPDATE executions SET state = 'recovering' WHERE id = ? AND state = 'running'",
        )
        .bind(request.execution_id.to_string())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        let task_changed = sqlx::query(
            "UPDATE tasks SET orchestration_state = 'recovering', updated_at = ? \
             WHERE id = ? AND orchestration_state = 'running'",
        )
        .bind(encode_timestamp(request.at))
        .bind(execution.task_id.to_string())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        upsert_progress(&mut transaction, request.progress).await?;
        let scheduler_changed = sqlx::query(
            "UPDATE scheduled_jobs SET state = 'completed', worker_id = NULL, \
                    lease_expires_at = NULL, updated_at = ? \
             WHERE id = ? AND state = 'claimed' AND worker_id = ?",
        )
        .bind(encode_timestamp(request.at))
        .bind(request.scheduler_job_id.to_string())
        .bind(request.worker_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        let lease_removed =
            sqlx::query("DELETE FROM execution_leases WHERE execution_id = ? AND worker_id = ?")
                .bind(request.execution_id.to_string())
                .bind(request.worker_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
        if execution_changed != 1
            || task_changed != 1
            || scheduler_changed != 1
            || lease_removed != 1
        {
            return Err(StorageError::ExecutionStateConflict);
        }
        let recovery_kind = ScheduledJobKind::Recovery {
            execution_id: request.execution_id,
        };
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, created_at, updated_at) \
             VALUES (?, 'recovery', ?, ?, 'pending', 0, ?, ?, ?)",
        )
        .bind(ScheduleId::new().to_string())
        .bind(serde_json::to_string(&recovery_kind)?)
        .bind(encode_timestamp(request.at))
        .bind(format!("recovery:{}", request.execution_id))
        .bind(encode_timestamp(request.at))
        .bind(encode_timestamp(request.at))
        .execute(&mut *transaction)
        .await?;
        insert_execution_log(
            &mut transaction,
            ExecutionLogInsert {
                execution_id: request.execution_id,
                attempt_id: Some(request.attempt_id),
                at: request.at,
                level: LogLevel::Warn,
                stage: ExecutionStage::Verifying,
                message: "remote mutation outcome requires verification-only recovery",
                provider_trace_id: None,
                metadata_sanitized: None,
            },
            request.correlation_id,
        )
        .await?;
        enqueue_state_event(
            &mut transaction,
            request.execution_id,
            ExecutionState::Recovering,
            request.at,
            request.correlation_id,
        )
        .await?;
        enqueue_progress_event(&mut transaction, request.progress, request.correlation_id).await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                request.correlation_id,
                DomainEvent::ExecutionRecoveryRequired {
                    execution_id: request.execution_id,
                    task_id: execution.task_id,
                },
                request.at,
            ),
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }
}

#[async_trait]
impl ExecutionQueryRepository for SqliteExecutionRepository {
    async fn list_owned_executions(
        &self,
        owner_id: UserId,
        task_id: Option<TaskId>,
        limit: u32,
        offset: u64,
    ) -> Result<ExecutionPage, StorageError> {
        if limit == 0 || limit > MAX_EXECUTION_PAGE_SIZE || offset > MAX_EXECUTION_OFFSET {
            return Err(StorageError::InvalidData(
                "execution pagination is outside the supported range".to_owned(),
            ));
        }
        let task_id = task_id.map(|task_id| task_id.to_string());
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM executions AS execution \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND (? IS NULL OR execution.task_id = ?)",
        )
        .bind(owner_id.to_string())
        .bind(&task_id)
        .bind(&task_id)
        .fetch_one(self.database.pool())
        .await?;
        let rows = sqlx::query(
            "SELECT execution.id, execution.task_id, execution.requested_capabilities_json, execution.submission_draft_id, execution.requested_by, \
                    execution.request_source, execution.quote_id, execution.state, \
                    execution.scheduled_at, execution.started_at, execution.finished_at, \
                    execution.created_at FROM executions AS execution \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND (? IS NULL OR execution.task_id = ?) \
             ORDER BY execution.created_at DESC, execution.id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner_id.to_string())
        .bind(&task_id)
        .bind(&task_id)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated execution offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        let items = rows
            .iter()
            .map(decode_execution)
            .collect::<Result<_, _>>()?;
        Ok(ExecutionPage {
            items,
            total: u64::try_from(total)
                .map_err(|_| StorageError::InvalidData("execution count is invalid".to_owned()))?,
        })
    }

    async fn find_owned_execution_detail(
        &self,
        owner_id: UserId,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionDetail>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let execution = sqlx::query(
            "SELECT execution.id, execution.task_id, execution.requested_capabilities_json, execution.submission_draft_id, execution.requested_by, \
                    execution.request_source, execution.quote_id, execution.state, \
                    execution.scheduled_at, execution.started_at, execution.finished_at, \
                    execution.created_at \
             FROM executions AS execution \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE execution.id = ? AND account.owner_user_id = ?",
        )
        .bind(execution_id.to_string())
        .bind(owner_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?
        .map(|row| decode_execution(&row))
        .transpose()?;
        let Some(execution) = execution else {
            transaction.commit().await?;
            return Ok(None);
        };
        let progress = sqlx::query(
            "SELECT execution_id, percent, stage, status_text, current_item, completed_items, \
                    total_items, updated_at FROM execution_progress WHERE execution_id = ?",
        )
        .bind(execution_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?
        .map(|row| decode_progress(&row))
        .transpose()?;
        let attempt_rows = sqlx::query(
            "SELECT id, execution_id, attempt_no, started_at, finished_at, result, error_class, \
                    provider_trace_id FROM execution_attempts WHERE execution_id = ? \
             ORDER BY attempt_no ASC LIMIT ?",
        )
        .bind(execution_id.to_string())
        .bind(i64::try_from(MAX_EXECUTION_ATTEMPTS + 1).expect("attempt limit fits i64"))
        .fetch_all(&mut *transaction)
        .await?;
        if attempt_rows.len() > MAX_EXECUTION_ATTEMPTS {
            return Err(StorageError::InvalidData(
                "execution attempt history exceeds the supported bound".to_owned(),
            ));
        }
        let attempts = attempt_rows
            .iter()
            .map(decode_attempt)
            .collect::<Result<_, _>>()?;
        transaction.commit().await?;
        Ok(Some(ExecutionDetail {
            execution,
            progress,
            attempts,
        }))
    }

    async fn list_owned_execution_logs(
        &self,
        owner_id: UserId,
        execution_id: ExecutionId,
        limit: u32,
        offset: u64,
    ) -> Result<Option<ExecutionLogPage>, StorageError> {
        if limit == 0 || limit > MAX_EXECUTION_LOG_PAGE_SIZE || offset > MAX_EXECUTION_LOG_OFFSET {
            return Err(StorageError::InvalidData(
                "execution log pagination is outside the supported range".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin().await?;
        let owned: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM executions AS execution \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE execution.id = ? AND account.owner_user_id = ?",
        )
        .bind(execution_id.to_string())
        .bind(owner_id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if owned != 1 {
            transaction.commit().await?;
            return Ok(None);
        }
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM execution_logs WHERE execution_id = ?")
                .bind(execution_id.to_string())
                .fetch_one(&mut *transaction)
                .await?;
        let rows = sqlx::query(
            "SELECT execution_id, attempt_id, timestamp, level, stage, message, \
                    provider_trace_id, metadata_sanitized_json FROM execution_logs \
             WHERE execution_id = ? ORDER BY timestamp ASC, id ASC LIMIT ? OFFSET ?",
        )
        .bind(execution_id.to_string())
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated execution log offset fits i64"))
        .fetch_all(&mut *transaction)
        .await?;
        let items = rows.iter().map(decode_log).collect::<Result<_, _>>()?;
        transaction.commit().await?;
        Ok(Some(ExecutionLogPage {
            items,
            total: u64::try_from(total).map_err(|_| {
                StorageError::InvalidData("execution log count is invalid".to_owned())
            })?,
        }))
    }
}

async fn find_active_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
) -> Result<ExecutionAttempt, StorageError> {
    sqlx::query(
        "SELECT id, execution_id, attempt_no, started_at, finished_at, result, error_class, \
                provider_trace_id FROM execution_attempts \
         WHERE execution_id = ? AND finished_at IS NULL ORDER BY attempt_no DESC LIMIT 1",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .map(|row| decode_attempt(&row))
    .transpose()?
    .ok_or(StorageError::ExecutionAttemptNotActive)
}

async fn create_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution: &Execution,
    at: Timestamp,
) -> Result<(ExecutionAttempt, ExecutionProgress), StorageError> {
    let task_state: String =
        sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
            .bind(execution.task_id.to_string())
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(StorageError::ExecutionStateConflict)?;
    let task_state: OrchestrationState = decode_enum(&task_state)?;
    let synchronized = matches!(
        (execution.state, task_state),
        (ExecutionState::Scheduled, OrchestrationState::Scheduled)
            | (
                ExecutionState::RetryWaiting,
                OrchestrationState::RetryWaiting
            )
            | (
                ExecutionState::HumanRequired,
                OrchestrationState::HumanRequired
            )
    );
    if !synchronized {
        return Err(StorageError::ExecutionStateConflict);
    }

    let attempt_no: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(attempt_no), 0) + 1 FROM execution_attempts WHERE execution_id = ?",
    )
    .bind(execution.id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    let attempt = ExecutionAttempt {
        id: ExecutionAttemptId::new(),
        execution_id: execution.id,
        attempt_no: u32::try_from(attempt_no).map_err(|_| {
            StorageError::InvalidData("execution attempt number does not fit u32".to_owned())
        })?,
        started_at: at,
        finished_at: None,
        result: None,
        error_class: None,
        provider_trace_id: None,
    };
    let execution_update = sqlx::query(
        "UPDATE executions SET state = 'running', started_at = COALESCE(started_at, ?) \
         WHERE id = ? AND state = ?",
    )
    .bind(encode_timestamp(at))
    .bind(execution.id.to_string())
    .bind(enum_name(execution.state)?)
    .execute(&mut **transaction)
    .await?;
    let task_update = sqlx::query(
        "UPDATE tasks SET orchestration_state = 'running', updated_at = ? \
         WHERE id = ? AND orchestration_state = ?",
    )
    .bind(encode_timestamp(at))
    .bind(execution.task_id.to_string())
    .bind(enum_name(task_state)?)
    .execute(&mut **transaction)
    .await?;
    if execution_update.rows_affected() != 1 || task_update.rows_affected() != 1 {
        return Err(StorageError::ExecutionStateConflict);
    }
    sqlx::query(
        "INSERT INTO execution_attempts \
         (id, execution_id, attempt_no, started_at) VALUES (?, ?, ?, ?)",
    )
    .bind(attempt.id.to_string())
    .bind(attempt.execution_id.to_string())
    .bind(attempt.attempt_no)
    .bind(encode_timestamp(attempt.started_at))
    .execute(&mut **transaction)
    .await?;
    let progress = ExecutionProgress {
        execution_id: execution.id,
        percent: Some(0),
        stage: ExecutionStage::Preparing,
        status_text: Some("execution attempt started".to_owned()),
        current_item: None,
        completed_items: None,
        total_items: None,
        updated_at: at,
    };
    upsert_progress(transaction, &progress).await?;
    Ok((attempt, progress))
}

async fn record_attempt_started(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    attempt: &ExecutionAttempt,
    progress: &ExecutionProgress,
    request: &ExecutionAttemptStartRequest<'_>,
) -> Result<(), StorageError> {
    insert_worker_audit(
        transaction,
        request.execution_id,
        request.worker_id,
        "execution_attempt_started",
        request.at,
        request.correlation_id,
        serde_json::json!({ "attempt_no": attempt.attempt_no }),
    )
    .await?;
    insert_execution_log(
        transaction,
        ExecutionLogInsert {
            execution_id: request.execution_id,
            attempt_id: Some(attempt.id),
            at: request.at,
            level: LogLevel::Info,
            stage: ExecutionStage::Preparing,
            message: "execution attempt started",
            provider_trace_id: None,
            metadata_sanitized: None,
        },
        request.correlation_id,
    )
    .await?;
    enqueue_state_event(
        transaction,
        request.execution_id,
        ExecutionState::Running,
        request.at,
        request.correlation_id,
    )
    .await?;
    enqueue_progress_event(transaction, progress, request.correlation_id).await
}

async fn apply_attempt_finish(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution: &mut Execution,
    request: &ExecutionAttemptFinishRequest<'_>,
    expected_execution_state: ExecutionState,
    expected_task_state: OrchestrationState,
    success_task_state: Option<OrchestrationState>,
) -> Result<(), StorageError> {
    if request.final_state == ExecutionState::Succeeded {
        consume_question_session_for_succeeded_execution(
            transaction,
            request.execution_id,
            request.at,
            request.correlation_id,
        )
        .await?;
    }
    let attempt_update = sqlx::query(
        "UPDATE execution_attempts SET finished_at = ?, result = ?, error_class = ?, \
             provider_trace_id = ? \
         WHERE id = ? AND execution_id = ? AND finished_at IS NULL",
    )
    .bind(encode_timestamp(request.at))
    .bind(enum_name(request.result)?)
    .bind(request.error_class.map(enum_name).transpose()?)
    .bind(request.provider_trace_id)
    .bind(request.attempt_id.to_string())
    .bind(request.execution_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if attempt_update.rows_affected() != 1 {
        return Err(StorageError::ExecutionAttemptNotActive);
    }
    update_finished_states(
        transaction,
        execution,
        request,
        expected_execution_state,
        expected_task_state,
        success_task_state,
    )
    .await?;
    settle_execution_reservation(
        transaction,
        request.execution_id,
        request.final_state,
        request.at,
        request.correlation_id,
    )
    .await?;
    upsert_progress(transaction, request.progress).await?;
    complete_scheduler_job(transaction, request).await?;
    if let Some(retry_at) = request.retry_at {
        insert_retry_job(transaction, request.execution_id, retry_at, request.at).await?;
    }
    let released =
        sqlx::query("DELETE FROM execution_leases WHERE execution_id = ? AND worker_id = ?")
            .bind(request.execution_id.to_string())
            .bind(request.worker_id)
            .execute(&mut **transaction)
            .await?;
    if released.rows_affected() != 1 {
        return Err(StorageError::ExecutionClaimLost);
    }
    Ok(())
}

async fn update_finished_states(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution: &mut Execution,
    request: &ExecutionAttemptFinishRequest<'_>,
    expected_execution_state: ExecutionState,
    expected_task_state: OrchestrationState,
    success_task_state: Option<OrchestrationState>,
) -> Result<(), StorageError> {
    let task_state = if request.final_state == ExecutionState::Succeeded {
        success_task_state.unwrap_or(orchestration_for_execution(request.final_state)?)
    } else {
        if success_task_state.is_some() {
            return Err(StorageError::InvalidData(
                "task-state override is only valid for a successful Execution".to_owned(),
            ));
        }
        orchestration_for_execution(request.final_state)?
    };
    let finished_at = request.final_state.is_terminal().then_some(request.at);
    let execution_update = sqlx::query(
        "UPDATE executions SET state = ?, scheduled_at = COALESCE(?, scheduled_at), \
             finished_at = ? WHERE id = ? AND state = ?",
    )
    .bind(enum_name(request.final_state)?)
    .bind(request.retry_at.map(encode_timestamp))
    .bind(finished_at.map(encode_timestamp))
    .bind(request.execution_id.to_string())
    .bind(enum_name(expected_execution_state)?)
    .execute(&mut **transaction)
    .await?;
    let task_update = sqlx::query(
        "UPDATE tasks SET orchestration_state = ?, updated_at = ? \
         WHERE id = ? AND orchestration_state = ?",
    )
    .bind(enum_name(task_state)?)
    .bind(encode_timestamp(request.at))
    .bind(execution.task_id.to_string())
    .bind(enum_name(expected_task_state)?)
    .execute(&mut **transaction)
    .await?;
    if execution_update.rows_affected() != 1 || task_update.rows_affected() != 1 {
        return Err(StorageError::ExecutionStateConflict);
    }
    execution.state = request.final_state;
    execution.finished_at = finished_at;
    if let Some(retry_at) = request.retry_at {
        execution.scheduled_at = Some(retry_at);
    }
    Ok(())
}

async fn complete_scheduler_job(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionAttemptFinishRequest<'_>,
) -> Result<(), StorageError> {
    let completed = sqlx::query(
        "UPDATE scheduled_jobs SET state = 'completed', worker_id = NULL, \
             lease_expires_at = NULL, updated_at = ? \
         WHERE id = ? AND state = 'claimed' AND worker_id = ?",
    )
    .bind(encode_timestamp(request.at))
    .bind(request.scheduler_job_id.to_string())
    .bind(request.worker_id)
    .execute(&mut **transaction)
    .await?;
    if completed.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StorageError::ExecutionClaimLost)
    }
}

async fn record_attempt_finished(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionAttemptFinishRequest<'_>,
    audit_action: &str,
    log_message: &str,
) -> Result<(), StorageError> {
    insert_worker_audit(
        transaction,
        request.execution_id,
        request.worker_id,
        audit_action,
        request.at,
        request.correlation_id,
        serde_json::json!({
            "attempt_id": request.attempt_id,
            "state": request.final_state,
            "error_class": request.error_class,
        }),
    )
    .await?;
    let level = if request.final_state == ExecutionState::Succeeded {
        LogLevel::Info
    } else {
        LogLevel::Warn
    };
    insert_execution_log(
        transaction,
        ExecutionLogInsert {
            execution_id: request.execution_id,
            attempt_id: Some(request.attempt_id),
            at: request.at,
            level,
            stage: request.progress.stage,
            message: log_message,
            provider_trace_id: request.provider_trace_id,
            metadata_sanitized: None,
        },
        request.correlation_id,
    )
    .await?;
    enqueue_state_event(
        transaction,
        request.execution_id,
        request.final_state,
        request.at,
        request.correlation_id,
    )
    .await?;
    enqueue_progress_event(transaction, request.progress, request.correlation_id).await
}

async fn select_execution(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
) -> Result<sqlx::sqlite::SqliteRow, StorageError> {
    sqlx::query(
        "SELECT id, task_id, requested_capabilities_json, submission_draft_id, requested_by, request_source, quote_id, state, \
                scheduled_at, started_at, finished_at, created_at FROM executions WHERE id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::ExecutionStateConflict)
}

pub(crate) async fn assert_worker_claims(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    scheduler_job_id: ScheduleId,
    worker_id: &str,
    at: Timestamp,
    require_live_scheduler_claim: bool,
) -> Result<(), StorageError> {
    assert_execution_lease(transaction, execution_id, worker_id, at).await?;
    let row = sqlx::query(
        "SELECT payload_json, lease_expires_at FROM scheduled_jobs \
         WHERE id = ? AND state = 'claimed' AND worker_id = ?",
    )
    .bind(scheduler_job_id.to_string())
    .bind(worker_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::ExecutionClaimLost)?;
    if require_live_scheduler_claim {
        let expires_at = decode_timestamp(row.try_get("lease_expires_at")?)?;
        if expires_at <= at {
            return Err(StorageError::ExecutionClaimLost);
        }
    }
    let kind: ScheduledJobKind = serde_json::from_str(row.try_get("payload_json")?)?;
    let (ScheduledJobKind::Execution {
        execution_id: claimed_execution,
    }
    | ScheduledJobKind::Retry {
        execution_id: claimed_execution,
        ..
    }
    | ScheduledJobKind::Recovery {
        execution_id: claimed_execution,
    }) = kind
    else {
        return Err(StorageError::ExecutionClaimLost);
    };
    if claimed_execution != execution_id {
        return Err(StorageError::ExecutionClaimLost);
    }
    Ok(())
}

async fn assert_execution_lease(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    worker_id: &str,
    at: Timestamp,
) -> Result<(), StorageError> {
    let owned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM execution_leases \
         WHERE execution_id = ? AND worker_id = ? AND expires_at > ?",
    )
    .bind(execution_id.to_string())
    .bind(worker_id)
    .bind(encode_timestamp(at))
    .fetch_one(&mut **transaction)
    .await?;
    if owned == 1 {
        Ok(())
    } else {
        Err(StorageError::ExecutionClaimLost)
    }
}

pub(crate) fn validate_worker_token(
    worker_id: &str,
    correlation_id: &str,
) -> Result<(), StorageError> {
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    if valid(worker_id) && valid(correlation_id) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "execution worker identifiers are invalid".to_owned(),
        ))
    }
}

fn validate_progress(progress: &ExecutionProgress) -> Result<(), StorageError> {
    progress
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let bounded = |value: &Option<String>, max: usize| {
        value.as_ref().is_none_or(|value| {
            !value.is_empty()
                && value.len() <= max
                && value.trim() == value
                && !value.chars().any(char::is_control)
        })
    };
    if bounded(&progress.status_text, 512) && bounded(&progress.current_item, 256) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "execution progress text is invalid".to_owned(),
        ))
    }
}

fn validate_finish_request(
    request: &ExecutionAttemptFinishRequest<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    validate_progress(request.progress)?;
    let trace_valid = request.provider_trace_id.is_none_or(|value| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    });
    let state_result_valid = match request.final_state {
        ExecutionState::Succeeded => {
            request.result == AttemptResult::Succeeded
                && request.error_class.is_none()
                && request.retry_at.is_none()
                && request.progress.stage == ExecutionStage::Completed
                && request.progress.percent == Some(100)
        }
        ExecutionState::Cancelled => {
            request.result == AttemptResult::Cancelled && request.retry_at.is_none()
        }
        ExecutionState::Failed | ExecutionState::HumanRequired => {
            request.result == AttemptResult::Failed
                && request.error_class.is_some()
                && request.retry_at.is_none()
        }
        ExecutionState::RetryWaiting => {
            request.result == AttemptResult::Failed
                && request.error_class.is_some()
                && request
                    .retry_at
                    .is_some_and(|retry_at| retry_at > request.at)
        }
        _ => false,
    };
    if request.progress.execution_id != request.execution_id
        || request.progress.updated_at != request.at
        || !trace_valid
        || !state_result_valid
    {
        return Err(StorageError::InvalidData(
            "execution attempt finish request is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_question_step_finish_request(
    request: &ExecutionQuestionStepFinishRequest<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    validate_progress(request.progress)?;
    if request.transition.execution_id != request.execution_id
        || request.transition.transitioned_at > request.at
        || request.progress.execution_id != request.execution_id
        || request.progress.updated_at != request.at
        || request.progress.stage != ExecutionStage::Completed
        || request.progress.percent.is_some()
    {
        return Err(StorageError::InvalidData(
            "next-Question execution finish request is invalid".to_owned(),
        ));
    }
    Ok(())
}

async fn question_transition_is_ready(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionQuestionStepFinishRequest<'_>,
) -> Result<bool, StorageError> {
    let ready: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM question_session_transitions AS transition \
         INNER JOIN question_session_operations AS operation \
           ON operation.session_id = transition.previous_session_id \
          AND operation.sequence = transition.operation_sequence \
         INNER JOIN question_sessions AS previous \
           ON previous.id = transition.previous_session_id \
         INNER JOIN question_sessions AS next ON next.id = transition.next_session_id \
         WHERE transition.previous_session_id = ? AND transition.operation_sequence = ? \
           AND transition.execution_id = ? AND transition.next_session_id = ? \
           AND transition.next_question_snapshot_id = ? AND transition.transitioned_at = ? \
           AND operation.execution_attempt_id = ? AND operation.state = 'accepted' \
           AND operation.result_digest IS NOT NULL \
           AND previous.state = 'consumed' AND previous.execution_id = transition.execution_id \
           AND next.state = 'active' AND next.execution_id IS NULL \
           AND next.question_snapshot_id = transition.next_question_snapshot_id)",
    )
    .bind(request.transition.previous_session_id.to_string())
    .bind(
        i64::try_from(request.transition.operation_sequence).map_err(|_| {
            StorageError::InvalidData("Question transition sequence is invalid".to_owned())
        })?,
    )
    .bind(request.execution_id.to_string())
    .bind(request.transition.next_session_id.to_string())
    .bind(request.transition.next_question_snapshot_id.to_string())
    .bind(encode_timestamp(request.transition.transitioned_at))
    .bind(request.attempt_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    Ok(ready == 1)
}

fn validate_recovery_finish_request(
    request: &ExecutionRecoveryFinishRequest<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    validate_progress(request.progress)?;
    let trace_valid = request.provider_trace_id.is_none_or(|value| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    });
    let state_valid = match request.final_state {
        ExecutionState::Succeeded => {
            request.error_class.is_none()
                && request.retry_at.is_none()
                && request.progress.stage == ExecutionStage::Completed
                && request.progress.percent == Some(100)
        }
        ExecutionState::HumanRequired => {
            request.error_class.is_some() && request.retry_at.is_none()
        }
        ExecutionState::Failed => request.error_class.is_some() && request.retry_at.is_none(),
        ExecutionState::RetryWaiting => {
            request.error_class.is_some()
                && request
                    .retry_at
                    .is_some_and(|retry_at| retry_at > request.at)
        }
        _ => false,
    };
    if request.progress.execution_id != request.execution_id
        || request.progress.updated_at != request.at
        || !trace_valid
        || !state_valid
    {
        return Err(StorageError::InvalidData(
            "execution recovery finish request is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn orchestration_for_execution(state: ExecutionState) -> Result<OrchestrationState, StorageError> {
    match state {
        ExecutionState::Succeeded => Ok(OrchestrationState::Succeeded),
        ExecutionState::Failed => Ok(OrchestrationState::Failed),
        ExecutionState::Cancelled => Ok(OrchestrationState::Cancelled),
        ExecutionState::HumanRequired => Ok(OrchestrationState::HumanRequired),
        ExecutionState::RetryWaiting => Ok(OrchestrationState::RetryWaiting),
        _ => Err(StorageError::InvalidData(
            "execution finish state is not supported".to_owned(),
        )),
    }
}

async fn upsert_progress(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    progress: &ExecutionProgress,
) -> Result<(), StorageError> {
    validate_progress(progress)?;
    sqlx::query(
        "INSERT INTO execution_progress \
         (execution_id, percent, stage, status_text, current_item, completed_items, total_items, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(execution_id) DO UPDATE SET percent = excluded.percent, stage = excluded.stage, \
             status_text = excluded.status_text, current_item = excluded.current_item, \
             completed_items = excluded.completed_items, total_items = excluded.total_items, \
             updated_at = excluded.updated_at",
    )
    .bind(progress.execution_id.to_string())
    .bind(progress.percent.map(i64::from))
    .bind(enum_name(progress.stage)?)
    .bind(&progress.status_text)
    .bind(&progress.current_item)
    .bind(progress.completed_items.map(i64::from))
    .bind(progress.total_items.map(i64::from))
    .bind(encode_timestamp(progress.updated_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn upsert_newer_progress(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    progress: &ExecutionProgress,
) -> Result<bool, StorageError> {
    let result = sqlx::query(
        "INSERT INTO execution_progress \
         (execution_id, percent, stage, status_text, current_item, completed_items, total_items, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(execution_id) DO UPDATE SET percent = excluded.percent, stage = excluded.stage, \
             status_text = excluded.status_text, current_item = excluded.current_item, \
             completed_items = excluded.completed_items, total_items = excluded.total_items, \
             updated_at = excluded.updated_at \
         WHERE execution_progress.updated_at < excluded.updated_at",
    )
    .bind(progress.execution_id.to_string())
    .bind(progress.percent.map(i64::from))
    .bind(enum_name(progress.stage)?)
    .bind(&progress.status_text)
    .bind(&progress.current_item)
    .bind(progress.completed_items.map(i64::from))
    .bind(progress.total_items.map(i64::from))
    .bind(encode_timestamp(progress.updated_at))
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn insert_retry_job(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    retry_at: Timestamp,
    at: Timestamp,
) -> Result<(), StorageError> {
    let next_attempt_no: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(attempt_no), 0) + 1 FROM execution_attempts WHERE execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    let next_attempt_no = u32::try_from(next_attempt_no).map_err(|_| {
        StorageError::InvalidData("execution attempt number does not fit u32".to_owned())
    })?;
    let kind = ScheduledJobKind::Retry {
        execution_id,
        next_attempt_no,
    };
    sqlx::query(
        "INSERT INTO scheduled_jobs \
         (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, created_at, updated_at) \
         VALUES (?, 'retry', ?, ?, 'pending', 0, ?, ?, ?)",
    )
    .bind(ScheduleId::new().to_string())
    .bind(serde_json::to_string(&kind)?)
    .bind(encode_timestamp(retry_at))
    .bind(format!("retry:{execution_id}:{next_attempt_no}"))
    .bind(encode_timestamp(at))
    .bind(encode_timestamp(at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_worker_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    worker_id: &str,
    action: &str,
    at: Timestamp,
    correlation_id: &str,
    metadata: serde_json::Value,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, ?, 'execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(at))
    .bind(worker_id)
    .bind(action)
    .bind(execution_id.to_string())
    .bind(correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

struct ExecutionLogInsert<'a> {
    execution_id: ExecutionId,
    attempt_id: Option<ExecutionAttemptId>,
    at: Timestamp,
    level: LogLevel,
    stage: ExecutionStage,
    message: &'a str,
    provider_trace_id: Option<&'a str>,
    metadata_sanitized: Option<&'a serde_json::Value>,
}

async fn insert_execution_log(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: ExecutionLogInsert<'_>,
    correlation_id: &str,
) -> Result<(), StorageError> {
    let event = ExecutionLogEvent {
        execution_id: record.execution_id,
        attempt_id: record.attempt_id,
        timestamp: record.at,
        level: record.level,
        stage: record.stage,
        message: record.message.to_owned(),
        provider_trace_id: record.provider_trace_id.map(str::to_owned),
        metadata_sanitized: record.metadata_sanitized.cloned(),
    };
    sqlx::query(
        "INSERT INTO execution_logs \
         (id, execution_id, attempt_id, timestamp, level, stage, message, provider_trace_id, \
          metadata_sanitized_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(record.execution_id.to_string())
    .bind(record.attempt_id.map(|id| id.to_string()))
    .bind(encode_timestamp(record.at))
    .bind(enum_name(record.level)?)
    .bind(enum_name(record.stage)?)
    .bind(record.message)
    .bind(record.provider_trace_id)
    .bind(
        record
            .metadata_sanitized
            .map(serde_json::to_string)
            .transpose()?,
    )
    .execute(&mut **transaction)
    .await?;
    enqueue_in_transaction(
        transaction,
        &EventEnvelope::at(
            correlation_id,
            DomainEvent::ExecutionLogged(event),
            record.at,
        ),
    )
    .await
}

fn validate_log_append_request(
    request: &ExecutionLogAppendRequest<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    if !valid_log_text(request.message, 2_048)
        || request
            .provider_trace_id
            .is_some_and(|value| !valid_log_text(value, 256))
        || request.metadata_sanitized.is_some_and(|value| {
            serde_json::to_vec(value).map_or(true, |encoded| {
                encoded.len() > MAX_EXECUTION_LOG_METADATA_BYTES
            }) || contains_log_secret_key(value)
        })
    {
        return Err(StorageError::InvalidData(
            "Provider execution log is oversized or not sanitized".to_owned(),
        ));
    }
    Ok(())
}

fn valid_log_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn contains_log_secret_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
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
            ) || contains_log_secret_key(value)
        }),
        serde_json::Value::Array(items) => items.iter().any(contains_log_secret_key),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

async fn enqueue_state_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    state: ExecutionState,
    at: Timestamp,
    correlation_id: &str,
) -> Result<(), StorageError> {
    let event = EventEnvelope::at(
        correlation_id,
        DomainEvent::ExecutionStateChanged {
            execution_id,
            state,
        },
        at,
    );
    enqueue_in_transaction(transaction, &event).await
}

async fn enqueue_progress_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    progress: &ExecutionProgress,
    correlation_id: &str,
) -> Result<(), StorageError> {
    let event = EventEnvelope::at(
        correlation_id,
        DomainEvent::ExecutionProgressed(progress.clone()),
        progress.updated_at,
    );
    enqueue_in_transaction(transaction, &event).await
}

fn validate_schedule_request(request: &ExecutionScheduleRequest<'_>) -> Result<(), StorageError> {
    let execution = request.execution;
    let token_valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    let billing_valid = match (&request.billing, execution.quote_id) {
        (None, None) => true,
        (Some(billing), Some(quote_id)) => {
            let quote = billing.quote;
            let reservation = billing.reservation;
            quote.id == quote_id
                && quote.task_id == execution.task_id
                && quote.created_at <= execution.created_at
                && valid_bounded_text(&quote.pricing_revision, 128)
                && valid_bounded_text(&quote.reason, 2_048)
                && i64::try_from(quote.amount.value()).is_ok()
                && execution.requested_by == Some(reservation.user_id)
                && reservation.quote_id == quote.id
                && reservation.execution_id == execution.id
                && reservation.amount == quote.amount
                && reservation.state == CreditReservationState::Reserved
                && reservation.created_at == execution.created_at
                && reservation.updated_at == reservation.created_at
        }
        _ => false,
    };
    let runtime_settings_valid = request.runtime_settings.is_none_or(|resolution| {
        resolution.schema.validate().is_ok()
            && valid_runtime_settings_snapshot(resolution.snapshot, execution.created_at)
            && resolution.snapshot.resolved.schema_version == resolution.schema.version
    });
    let provider_plan_valid = request.provider_plan_artifact.is_none_or(|artifact| {
        request
            .runtime_settings
            .is_some_and(|resolution| artifact.provider_id() == &resolution.snapshot.provider_id)
    });
    let capability_plan_valid = request.capability_plan.len()
        == execution.requested_capabilities.len()
        && request
            .capability_plan
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            == execution
                .requested_capabilities
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
    let capability_calls_valid = !request.capability_call_starts.is_empty()
        && request.capability_call_starts[0] == 1
        && request.capability_call_starts.len() <= request.capability_plan.len()
        && request
            .capability_call_starts
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && request
            .capability_call_starts
            .iter()
            .all(|position| usize::from(*position) <= request.capability_plan.len());
    if execution.state != ExecutionState::Scheduled
        || !valid_requested_capabilities(&execution.requested_capabilities)
        || !capability_plan_valid
        || !capability_calls_valid
        || execution
            .requested_capabilities
            .contains(&TaskCapability::SubmissionExecute)
            != execution.submission_draft_id.is_some()
        || execution.scheduled_at.is_none()
        || execution.started_at.is_some()
        || execution.finished_at.is_some()
        || execution
            .scheduled_at
            .is_some_and(|at| at < execution.created_at)
        || !token_valid(request.idempotency_scope)
        || !token_valid(request.idempotency_key)
        || !token_valid(request.correlation_id)
        || request.expected_task_state == OrchestrationState::Scheduled
        || !billing_valid
        || !runtime_settings_valid
        || !provider_plan_valid
    {
        return Err(StorageError::InvalidData(
            "execution schedule request is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn valid_runtime_settings_snapshot(
    snapshot: &ExecutionRuntimeSettingsSnapshot,
    execution_created_at: Timestamp,
) -> bool {
    let revisions_valid = [
        snapshot.provider_revision,
        snapshot.provider_account_revision,
        snapshot.task_revision,
    ]
    .into_iter()
    .flatten()
    .all(|revision| revision > 0);
    let keys_match = snapshot.sources.len() == snapshot.resolved.values.len()
        && snapshot
            .resolved
            .values
            .keys()
            .all(|key| snapshot.sources.contains_key(key));
    let sources_bound = snapshot.sources.values().all(|source| match source {
        ProviderRuntimeSettingSource::SchemaDefault => true,
        ProviderRuntimeSettingSource::Provider => snapshot.provider_revision.is_some(),
        ProviderRuntimeSettingSource::ProviderAccount => {
            snapshot.provider_account_revision.is_some()
        }
        ProviderRuntimeSettingSource::Task => snapshot.task_revision.is_some(),
    });
    snapshot.resolved.schema_version > 0
        && snapshot.captured_at == execution_created_at
        && snapshot.completion_policy.captured_at == snapshot.captured_at
        && snapshot.completion_policy.validate().is_ok()
        && revisions_valid
        && keys_match
        && sources_bound
        && serde_json::to_vec(&snapshot.resolved.values)
            .is_ok_and(|value| value.len() <= MAX_EXECUTION_SETTINGS_JSON_BYTES)
        && serde_json::to_vec(&snapshot.sources)
            .is_ok_and(|value| value.len() <= MAX_EXECUTION_SETTINGS_JSON_BYTES)
        && serde_json::to_vec(&snapshot.completion_policy)
            .is_ok_and(|value| value.len() <= MAX_EXECUTION_SETTINGS_JSON_BYTES)
}

fn valid_bounded_text(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn validate_idempotency_tokens(scope: &str, key: &str) -> Result<(), StorageError> {
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    if valid(scope) && valid(key) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "execution idempotency tokens are invalid".to_owned(),
        ))
    }
}

fn same_request(existing: &Execution, requested: &Execution) -> bool {
    existing.task_id == requested.task_id
        && existing.requested_capabilities == requested.requested_capabilities
        && existing.submission_draft_id == requested.submission_draft_id
        && existing.requested_by == requested.requested_by
        && existing.quote_id == requested.quote_id
}

async fn submission_draft_is_available(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution: &Execution,
) -> Result<bool, StorageError> {
    let Some(submission_draft_id) = execution.submission_draft_id else {
        return Ok(true);
    };
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM executions WHERE submission_draft_id = ?")
            .bind(submission_draft_id.to_string())
            .fetch_optional(&mut **transaction)
            .await?;
    if existing.is_some() {
        return Ok(false);
    }
    let binding: Option<(String, String)> = sqlx::query_as(
        "SELECT draft.task_id, draft.provider_id FROM submission_drafts AS draft WHERE draft.id = ?",
    )
    .bind(submission_draft_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let expected_provider: Option<String> = sqlx::query_scalar(
        "SELECT account.provider_id FROM tasks AS task \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE task.id = ?",
    )
    .bind(execution.task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(binding.as_ref().is_some_and(|(task_id, provider_id)| {
        task_id == &execution.task_id.to_string() && expected_provider.as_ref() == Some(provider_id)
    }))
}

async fn strict_completion_retry_is_valid(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionScheduleRequest<'_>,
) -> Result<bool, StorageError> {
    let execution = request.execution;
    let Some((assessment_class, owner_user_id)) = sqlx::query_as::<_, (String, String)>(
        "SELECT task.assessment_class, account.owner_user_id FROM tasks AS task \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE task.id = ?",
    )
    .bind(execution.task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(false);
    };
    let workflow_row: Option<(String, i64)> = sqlx::query_as(
        "SELECT workflow_json, revision FROM strict_completion_workflows WHERE task_id = ?",
    )
    .bind(execution.task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((workflow_json, revision)) = workflow_row else {
        return Ok(request.strict_completion_retry.is_none());
    };
    let workflow: StrictCompletionWorkflow = serde_json::from_str(&workflow_json)?;
    if workflow.validate().is_err()
        || workflow.binding.task_id != execution.task_id
        || workflow.binding.owner_user_id.to_string() != owner_user_id
        || i64::from(revision_u32(revision)?) != revision
    {
        return Err(StorageError::InvalidData(
            "strict completion retry workflow binding is invalid".to_owned(),
        ));
    }
    let retry_required = workflow.state == StrictCompletionState::Active
        && workflow.attempts_started > 0
        && (assessment_class == "formal"
            || execution.requested_capabilities == [TaskCapability::SubmissionExecute]);
    let Some(confirmation) = request.strict_completion_retry else {
        return Ok(!retry_required);
    };
    if !retry_required
        || confirmation.expected_revision != revision_u32(revision)?
        || confirmation.workflow_id != workflow.id
        || execution.requested_by != Some(workflow.binding.owner_user_id)
    {
        return Ok(false);
    }
    if execution.requested_capabilities != [TaskCapability::SubmissionExecute] {
        return Ok(true);
    }
    let Some(draft_id) = execution.submission_draft_id else {
        return Ok(false);
    };
    let freshness: Option<(String, String)> = sqlx::query_as(
        "SELECT snapshot.captured_at, draft.created_at FROM submission_drafts AS draft \
         INNER JOIN question_snapshots AS snapshot ON snapshot.id = draft.question_snapshot_id \
         WHERE draft.id = ? AND draft.task_id = ?",
    )
    .bind(draft_id.to_string())
    .bind(execution.task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((snapshot_captured_at, draft_created_at)) = freshness else {
        return Ok(false);
    };
    Ok(
        decode_timestamp(&snapshot_captured_at)? > workflow.updated_at
            && decode_timestamp(&draft_created_at)? > workflow.updated_at,
    )
}

fn revision_u32(revision: i64) -> Result<u32, StorageError> {
    u32::try_from(revision).map_err(|_| {
        StorageError::InvalidData("strict completion workflow revision is invalid".to_owned())
    })
}

async fn persisted_strict_retry_matches(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    requested: Option<ExecutionStrictCompletionRetryRequest>,
) -> Result<bool, StorageError> {
    let persisted: Option<(String, i64)> = sqlx::query_as(
        "SELECT workflow_id, workflow_revision \
         FROM execution_strict_completion_retry_confirmations WHERE execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    match (persisted, requested) {
        (None, None) => Ok(true),
        (Some((workflow_id, revision)), Some(requested)) => Ok(workflow_id
            == requested.workflow_id.to_string()
            && revision_u32(revision)? == requested.expected_revision),
        _ => Ok(false),
    }
}

async fn find_strict_retry_confirmation(
    pool: &sqlx::SqlitePool,
    execution_id: ExecutionId,
) -> Result<Option<ExecutionStrictCompletionRetryConfirmation>, StorageError> {
    let row: Option<(String, i64, String, String)> = sqlx::query_as(
        "SELECT workflow_id, workflow_revision, confirmed_by, confirmed_at \
         FROM execution_strict_completion_retry_confirmations WHERE execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(pool)
    .await?;
    row.map(|(workflow_id, revision, confirmed_by, confirmed_at)| {
        Ok(ExecutionStrictCompletionRetryConfirmation {
            execution_id,
            workflow_id: workflow_id.parse().map_err(|_| {
                StorageError::InvalidData(
                    "strict completion retry workflow ID is invalid".to_owned(),
                )
            })?,
            workflow_revision: revision_u32(revision)?,
            confirmed_by: confirmed_by.parse().map_err(|_| {
                StorageError::InvalidData("strict completion retry owner is invalid".to_owned())
            })?,
            confirmed_at: decode_timestamp(&confirmed_at)?,
        })
    })
    .transpose()
}

async fn transition_task_to_scheduled(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionScheduleRequest<'_>,
) -> Result<bool, StorageError> {
    let result = sqlx::query(
        "UPDATE tasks SET orchestration_state = 'scheduled', updated_at = ? \
         WHERE id = ? AND orchestration_state = ?",
    )
    .bind(encode_timestamp(request.execution.created_at))
    .bind(request.execution.task_id.to_string())
    .bind(enum_name(request.expected_task_state)?)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn find_idempotent_execution_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    idempotency_scope: &str,
    idempotency_key: &str,
) -> Result<Option<Execution>, StorageError> {
    sqlx::query(
        "SELECT id, task_id, requested_capabilities_json, submission_draft_id, requested_by, request_source, quote_id, state, \
                scheduled_at, started_at, finished_at, created_at FROM executions \
         WHERE idempotency_scope = ? AND idempotency_key = ?",
    )
    .bind(idempotency_scope)
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await?
    .as_ref()
    .map(decode_execution)
    .transpose()
}

async fn runtime_settings_match_current_layers(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: TaskId,
    captured_at: Timestamp,
    resolution: &ExecutionRuntimeSettingsResolution<'_>,
) -> Result<bool, StorageError> {
    let target: Option<(String, String)> = sqlx::query_as(
        "SELECT task.provider_account_id, account.provider_id FROM tasks AS task \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE task.id = ?",
    )
    .bind(task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((provider_account_id, actual_provider_id)) = target else {
        return Err(StorageError::InvalidData(
            "execution runtime settings Task binding is invalid".to_owned(),
        ));
    };
    if actual_provider_id != resolution.snapshot.provider_id.as_str() {
        return Err(StorageError::InvalidData(
            "execution runtime settings Provider binding is invalid".to_owned(),
        ));
    }
    let provider_account_id = ProviderAccountId::from_str(&provider_account_id)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let provider = find_runtime_settings_layer(
        transaction,
        "provider",
        resolution.snapshot.provider_id.as_str(),
    )
    .await?;
    let account = find_runtime_settings_layer(
        transaction,
        "provider_account",
        &provider_account_id.to_string(),
    )
    .await?;
    let task = find_runtime_settings_layer(transaction, "task", &task_id.to_string()).await?;
    let Ok((resolved, sources)) = resolution.schema.resolve_with_sources(
        provider.as_ref().map(|layer| &layer.patch),
        account.as_ref().map(|layer| &layer.patch),
        task.as_ref().map(|layer| &layer.patch),
    ) else {
        return Ok(false);
    };
    let Ok(completion_policy) = resolution
        .schema
        .completion_policy_snapshot(&resolved, captured_at)
    else {
        return Ok(false);
    };
    let current = ExecutionRuntimeSettingsSnapshot {
        provider_id: resolution.snapshot.provider_id.clone(),
        resolved,
        sources,
        completion_policy,
        provider_revision: provider.as_ref().map(|layer| layer.revision),
        provider_account_revision: account.as_ref().map(|layer| layer.revision),
        task_revision: task.as_ref().map(|layer| layer.revision),
        captured_at,
    };
    Ok(current == *resolution.snapshot)
}

#[derive(Clone, Debug)]
struct RuntimeSettingsLayer {
    patch: ProviderRuntimeSettingsPatch,
    revision: u32,
}

async fn find_runtime_settings_layer(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    scope: &str,
    target_id: &str,
) -> Result<Option<RuntimeSettingsLayer>, StorageError> {
    let (column, expected_scope) = match scope {
        "provider" => ("provider_id", "provider"),
        "provider_account" => ("provider_account_id", "provider_account"),
        "task" => ("task_id", "task"),
        _ => {
            return Err(StorageError::InvalidData(
                "execution runtime settings scope is invalid".to_owned(),
            ));
        }
    };
    let statement = format!(
        "SELECT schema_version, revision, settings_json FROM provider_runtime_settings \
         WHERE scope = ? AND {column} = ?"
    );
    let row = sqlx::query(&statement)
        .bind(expected_scope)
        .bind(target_id)
        .fetch_optional(&mut **transaction)
        .await?;
    row.as_ref().map(decode_runtime_settings_layer).transpose()
}

fn decode_runtime_settings_layer(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<RuntimeSettingsLayer, StorageError> {
    let settings_json = row.try_get::<String, _>("settings_json")?;
    if settings_json.len() > MAX_EXECUTION_SETTINGS_JSON_BYTES {
        return Err(StorageError::InvalidData(
            "persisted Provider runtime settings are too large".to_owned(),
        ));
    }
    let patch: ProviderRuntimeSettingsPatch = serde_json::from_str(&settings_json)?;
    let schema_version = u32::try_from(row.try_get::<i64, _>("schema_version")?)
        .map_err(|_| StorageError::InvalidData("invalid settings schema version".to_owned()))?;
    if patch.schema_version != schema_version {
        return Err(StorageError::InvalidData(
            "persisted Provider runtime settings schema binding is invalid".to_owned(),
        ));
    }
    Ok(RuntimeSettingsLayer {
        patch,
        revision: u32::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| StorageError::InvalidData("invalid settings revision".to_owned()))?,
    })
}

async fn insert_runtime_settings_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    snapshot: &ExecutionRuntimeSettingsSnapshot,
) -> Result<(), StorageError> {
    let resolved_settings_json = serde_json::to_string(&snapshot.resolved.values)?;
    let sources_json = serde_json::to_string(&snapshot.sources)?;
    let completion_policy_json = serde_json::to_string(&snapshot.completion_policy)?;
    sqlx::query(
        "INSERT INTO execution_runtime_settings \
         (execution_id, provider_id, schema_version, resolved_settings_json, sources_json, \
          provider_revision, provider_account_revision, task_revision, completion_policy_json, \
          captured_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(execution_id.to_string())
    .bind(snapshot.provider_id.as_str())
    .bind(i64::from(snapshot.resolved.schema_version))
    .bind(resolved_settings_json)
    .bind(sources_json)
    .bind(snapshot.provider_revision.map(i64::from))
    .bind(snapshot.provider_account_revision.map(i64::from))
    .bind(snapshot.task_revision.map(i64::from))
    .bind(completion_policy_json)
    .bind(encode_timestamp(snapshot.captured_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_provider_plan_artifact(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    captured_at: Timestamp,
    artifact: &ProviderExecutionPlanArtifact,
) -> Result<(), StorageError> {
    let payload_json = serde_json::to_string(artifact.payload_sanitized())?;
    sqlx::query(
        "INSERT INTO execution_provider_plan_artifacts \
         (execution_id, provider_id, artifact_type, artifact_digest, payload_json, captured_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(execution_id.to_string())
    .bind(artifact.provider_id().as_str())
    .bind(artifact.artifact_type())
    .bind(artifact.artifact_digest().as_slice())
    .bind(payload_json)
    .bind(encode_timestamp(captured_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn decode_provider_plan_artifact(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ProviderExecutionPlanArtifact, StorageError> {
    let provider_id = ProviderId::new(row.try_get::<String, _>("plan_provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let actual_provider_id = ProviderId::new(row.try_get::<String, _>("actual_provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let captured_at = decode_timestamp(row.try_get("captured_at")?)?;
    let execution_created_at = decode_timestamp(row.try_get("execution_created_at")?)?;
    if provider_id != actual_provider_id || captured_at != execution_created_at {
        return Err(StorageError::InvalidData(
            "execution Provider plan binding is invalid".to_owned(),
        ));
    }
    let artifact_type = row.try_get::<String, _>("artifact_type")?;
    let payload_json = row.try_get::<String, _>("payload_json")?;
    if payload_json.len() > 64 * 1_024 {
        return Err(StorageError::InvalidData(
            "execution Provider plan artifact is too large".to_owned(),
        ));
    }
    let artifact = ProviderExecutionPlanArtifact::try_new(
        provider_id,
        artifact_type,
        serde_json::from_str(&payload_json)?,
    )
    .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let persisted_digest = decode_execution_digest(row.try_get("artifact_digest")?)?;
    if artifact.artifact_digest() != persisted_digest {
        return Err(StorageError::InvalidData(
            "execution Provider plan artifact digest is invalid".to_owned(),
        ));
    }
    Ok(artifact)
}

fn decode_runtime_settings_snapshot(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ExecutionRuntimeSettingsSnapshot, StorageError> {
    let provider_id = ProviderId::new(row.try_get::<String, _>("snapshot_provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let actual_provider_id = ProviderId::new(row.try_get::<String, _>("actual_provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if provider_id != actual_provider_id {
        return Err(StorageError::InvalidData(
            "execution runtime settings Provider binding is invalid".to_owned(),
        ));
    }
    let resolved_settings_json = row.try_get::<String, _>("resolved_settings_json")?;
    let sources_json = row.try_get::<String, _>("sources_json")?;
    let completion_policy_json = row.try_get::<String, _>("completion_policy_json")?;
    if resolved_settings_json.len() > MAX_EXECUTION_SETTINGS_JSON_BYTES
        || sources_json.len() > MAX_EXECUTION_SETTINGS_JSON_BYTES
        || completion_policy_json.len() > MAX_EXECUTION_SETTINGS_JSON_BYTES
    {
        return Err(StorageError::InvalidData(
            "execution runtime settings snapshot is too large".to_owned(),
        ));
    }
    let captured_at = decode_timestamp(row.try_get("captured_at")?)?;
    let execution_created_at = decode_timestamp(row.try_get("execution_created_at")?)?;
    let snapshot = ExecutionRuntimeSettingsSnapshot {
        provider_id,
        resolved: ResolvedProviderRuntimeSettings {
            schema_version: u32::try_from(row.try_get::<i64, _>("schema_version")?).map_err(
                |_| {
                    StorageError::InvalidData(
                        "invalid execution settings schema version".to_owned(),
                    )
                },
            )?,
            values: serde_json::from_str::<BTreeMap<String, ProviderSettingValue>>(
                &resolved_settings_json,
            )?,
        },
        sources: serde_json::from_str(&sources_json)?,
        completion_policy: serde_json::from_str(&completion_policy_json)?,
        provider_revision: decode_optional_revision(row.try_get("provider_revision")?)?,
        provider_account_revision: decode_optional_revision(
            row.try_get("provider_account_revision")?,
        )?,
        task_revision: decode_optional_revision(row.try_get("task_revision")?)?,
        captured_at,
    };
    if !valid_runtime_settings_snapshot(&snapshot, execution_created_at) {
        return Err(StorageError::InvalidData(
            "persisted execution runtime settings snapshot is invalid".to_owned(),
        ));
    }
    Ok(snapshot)
}

fn decode_optional_revision(value: Option<i64>) -> Result<Option<u32>, StorageError> {
    value
        .map(|value| {
            u32::try_from(value)
                .map_err(|_| StorageError::InvalidData("invalid settings revision".to_owned()))
        })
        .transpose()
}

async fn insert_request_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match request.actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let billing = request.billing.as_ref().map(|billing| {
        serde_json::json!({
            "quote_id": billing.quote.id,
            "quoted_amount": billing.quote.amount,
            "pricing_revision": billing.quote.pricing_revision,
        })
    });
    let metadata = serde_json::json!({
        "task_id": request.execution.task_id,
        "request_source": request.execution.request_source,
        "billing": billing,
        "runtime_settings": request.runtime_settings.map(|resolution| serde_json::json!({
            "provider_id": resolution.snapshot.provider_id,
            "schema_version": resolution.snapshot.resolved.schema_version,
            "provider_revision": resolution.snapshot.provider_revision,
            "provider_account_revision": resolution.snapshot.provider_account_revision,
            "task_revision": resolution.snapshot.task_revision,
        })),
        "provider_plan": request.provider_plan_artifact.map(|artifact| serde_json::json!({
            "provider_id": artifact.provider_id(),
            "artifact_type": artifact.artifact_type(),
            "artifact_digest": "[HASHED]",
        })),
        "strict_completion_retry_confirmation": request.strict_completion_retry.map(|confirmation| serde_json::json!({
            "workflow_id": confirmation.workflow_id,
            "workflow_revision": confirmation.expected_revision,
            "confirmed_by": request.execution.requested_by,
        })),
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'execution_requested', 'execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(request.execution.created_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(request.execution.id.to_string())
    .bind(request.correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_quote_and_reserve_balance(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    billing: &ExecutionBillingReservation<'_>,
) -> Result<(), StorageError> {
    let quote = billing.quote;
    let reservation = billing.reservation;
    let amount =
        i64::try_from(quote.amount.value()).map_err(|_| StorageError::CreditAmountOutOfRange)?;
    sqlx::query(
        "INSERT INTO price_quotes (id, task_id, amount, pricing_revision, reason, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(quote.id.to_string())
    .bind(quote.task_id.to_string())
    .bind(amount)
    .bind(&quote.pricing_revision)
    .bind(&quote.reason)
    .bind(encode_timestamp(quote.created_at))
    .execute(&mut **transaction)
    .await?;
    let timestamp = encode_timestamp(reservation.created_at);
    sqlx::query(
        "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
         VALUES (?, 0, 0, ?) ON CONFLICT(user_id) DO NOTHING",
    )
    .bind(reservation.user_id.to_string())
    .bind(&timestamp)
    .execute(&mut **transaction)
    .await?;
    let balance_update = sqlx::query(
        "UPDATE credit_accounts SET available = available - ?, reserved = reserved + ?, \
             updated_at = ? WHERE user_id = ? AND available >= ?",
    )
    .bind(amount)
    .bind(amount)
    .bind(&timestamp)
    .bind(reservation.user_id.to_string())
    .bind(amount)
    .execute(&mut **transaction)
    .await?;
    if balance_update.rows_affected() != 1 {
        return Err(StorageError::InsufficientCredits);
    }
    Ok(())
}

async fn insert_credit_reservation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    billing: &ExecutionBillingReservation<'_>,
    correlation_id: &str,
) -> Result<(), StorageError> {
    let reservation = billing.reservation;
    let amount = i64::try_from(reservation.amount.value())
        .map_err(|_| StorageError::CreditAmountOutOfRange)?;
    sqlx::query(
        "INSERT INTO credit_reservations \
         (id, user_id, quote_id, execution_id, amount, state, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 'reserved', ?, ?)",
    )
    .bind(reservation.id.to_string())
    .bind(reservation.user_id.to_string())
    .bind(reservation.quote_id.to_string())
    .bind(reservation.execution_id.to_string())
    .bind(amount)
    .bind(encode_timestamp(reservation.created_at))
    .bind(encode_timestamp(reservation.updated_at))
    .execute(&mut **transaction)
    .await?;
    enqueue_in_transaction(
        transaction,
        &EventEnvelope::at(
            correlation_id,
            DomainEvent::CreditReserved {
                user_id: reservation.user_id,
                execution_id: reservation.execution_id,
                amount: reservation.amount,
            },
            reservation.created_at,
        ),
    )
    .await
}

fn decode_execution(row: &sqlx::sqlite::SqliteRow) -> Result<Execution, StorageError> {
    let requested_capabilities: Vec<TaskCapability> =
        serde_json::from_str(row.try_get("requested_capabilities_json")?)?;
    if !valid_requested_capabilities(&requested_capabilities) {
        return Err(StorageError::InvalidData(
            "persisted Execution capability selection is invalid".to_owned(),
        ));
    }
    Ok(Execution {
        id: ExecutionId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        task_id: TaskId::from_str(row.try_get("task_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        requested_capabilities,
        submission_draft_id: row
            .try_get::<Option<&str>, _>("submission_draft_id")?
            .map(asterism_domain::SubmissionDraftId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        requested_by: row
            .try_get::<Option<&str>, _>("requested_by")?
            .map(UserId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        request_source: decode_enum(row.try_get("request_source")?)?,
        quote_id: row
            .try_get::<Option<&str>, _>("quote_id")?
            .map(asterism_domain::PriceQuoteId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        state: decode_enum(row.try_get("state")?)?,
        scheduled_at: decode_optional_timestamp(row.try_get("scheduled_at")?)?,
        started_at: decode_optional_timestamp(row.try_get("started_at")?)?,
        finished_at: decode_optional_timestamp(row.try_get("finished_at")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    })
}

fn decode_capability_step(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ExecutionCapabilityStep, StorageError> {
    let state = match row.try_get::<&str, _>("state")? {
        "pending" => ExecutionCapabilityStepState::Pending,
        "issued" => ExecutionCapabilityStepState::Issued,
        "succeeded" => ExecutionCapabilityStepState::Succeeded,
        "ambiguous" => ExecutionCapabilityStepState::Ambiguous,
        _ => {
            return Err(StorageError::InvalidData(
                "persisted execution capability step has an invalid state".to_owned(),
            ));
        }
    };
    Ok(ExecutionCapabilityStep {
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        position: u8::try_from(row.try_get::<i64, _>("position")?).map_err(|_| {
            StorageError::InvalidData("execution capability step position is invalid".to_owned())
        })?,
        call_position: u8::try_from(row.try_get::<i64, _>("call_position")?).map_err(|_| {
            StorageError::InvalidData("execution capability call position is invalid".to_owned())
        })?,
        call_member_position: u8::try_from(row.try_get::<i64, _>("call_member_position")?)
            .map_err(|_| {
                StorageError::InvalidData(
                    "execution capability call member position is invalid".to_owned(),
                )
            })?,
        capability: decode_enum(row.try_get("capability")?)?,
        state,
        issued_attempt_id: row
            .try_get::<Option<&str>, _>("issued_attempt_id")?
            .map(ExecutionAttemptId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        issued_at: decode_optional_timestamp(row.try_get("issued_at")?)?,
        succeeded_at: decode_optional_timestamp(row.try_get("succeeded_at")?)?,
    })
}

async fn find_capability_call(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    call_position: u8,
) -> Result<Vec<TaskCapability>, StorageError> {
    let capabilities: Vec<String> = sqlx::query_scalar(
        "SELECT capability FROM execution_capability_steps \
         WHERE execution_id = ? AND call_position = ? ORDER BY call_member_position",
    )
    .bind(execution_id.to_string())
    .bind(i64::from(call_position))
    .fetch_all(&mut **transaction)
    .await?;
    capabilities
        .iter()
        .map(|capability| decode_enum(capability))
        .collect()
}

fn validate_capability_call_mutation(
    request: &ExecutionCapabilityCallMutation<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    if !(1..=5).contains(&request.call_position)
        || request.capabilities.is_empty()
        || request.capabilities.len() > 5
        || request
            .capabilities
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != request.capabilities.len()
    {
        return Err(StorageError::InvalidData(
            "execution capability call mutation is invalid".to_owned(),
        ));
    }
    Ok(())
}

async fn select_atomic_mutation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    attempt_id: ExecutionAttemptId,
    ordinal: u32,
) -> Result<Option<ExecutionAtomicMutation>, StorageError> {
    sqlx::query(
        "SELECT execution_id, execution_attempt_id, ordinal, scheduler_job_id, worker_id, \
                operation_type, request_digest, response_digest, accepted, issued_at, received_at \
         FROM execution_atomic_mutations WHERE execution_id = ? AND execution_attempt_id = ? \
           AND ordinal = ?",
    )
    .bind(execution_id.to_string())
    .bind(attempt_id.to_string())
    .bind(i64::from(ordinal))
    .fetch_optional(&mut **transaction)
    .await?
    .map(|row| decode_atomic_mutation(&row))
    .transpose()
}

fn decode_atomic_mutation(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ExecutionAtomicMutation, StorageError> {
    let accepted = row
        .try_get::<Option<i64>, _>("accepted")?
        .map(|value| match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StorageError::InvalidData(
                "persisted execution atomic mutation acceptance is invalid".to_owned(),
            )),
        })
        .transpose()?;
    Ok(ExecutionAtomicMutation {
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        attempt_id: ExecutionAttemptId::from_str(row.try_get("execution_attempt_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        ordinal: u32::try_from(row.try_get::<i64, _>("ordinal")?).map_err(|_| {
            StorageError::InvalidData("execution atomic mutation ordinal is invalid".to_owned())
        })?,
        scheduler_job_id: ScheduleId::from_str(row.try_get("scheduler_job_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        worker_id: row.try_get("worker_id")?,
        operation_type: row.try_get("operation_type")?,
        request_digest: decode_execution_digest(row.try_get("request_digest")?)?,
        response_digest: row
            .try_get::<Option<Vec<u8>>, _>("response_digest")?
            .map(decode_execution_digest)
            .transpose()?,
        accepted,
        issued_at: decode_timestamp(row.try_get("issued_at")?)?,
        received_at: decode_optional_timestamp(row.try_get("received_at")?)?,
    })
}

fn decode_execution_digest(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    bytes.try_into().map_err(|_| {
        StorageError::InvalidData("persisted execution mutation digest is invalid".to_owned())
    })
}

fn validate_atomic_mutation_issue(
    request: &ExecutionAtomicMutationIssueRequest<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    if !(1..=100_000).contains(&request.ordinal)
        || !valid_atomic_operation_type(request.operation_type)
        || request.request_digest == [0; 32]
    {
        return Err(StorageError::InvalidData(
            "execution atomic mutation issue is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_atomic_mutation_receipt(
    request: &ExecutionAtomicMutationReceiptRequest<'_>,
) -> Result<(), StorageError> {
    validate_worker_token(request.worker_id, request.correlation_id)?;
    if !(1..=100_000).contains(&request.ordinal) || request.response_digest == [0; 32] {
        return Err(StorageError::InvalidData(
            "execution atomic mutation receipt is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn valid_atomic_operation_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.trim() == value
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_requested_capabilities(capabilities: &[TaskCapability]) -> bool {
    !capabilities.is_empty()
        && capabilities.len() <= 5
        && capabilities.windows(2).all(|pair| pair[0] < pair[1])
        && capabilities.iter().all(|capability| {
            matches!(
                capability,
                TaskCapability::ResourceExecution
                    | TaskCapability::SubmissionExecute
                    | TaskCapability::DurationReport
                    | TaskCapability::Discussion
                    | TaskCapability::Practice
            )
        })
        && (!capabilities.contains(&TaskCapability::SubmissionExecute)
            || capabilities == [TaskCapability::SubmissionExecute])
}

fn decode_submission_receipt(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<SubmissionAttemptReceipt, StorageError> {
    let receipt_json: &str = row.try_get("receipt_json")?;
    let receipt_bytes = usize::try_from(row.try_get::<i64, _>("receipt_bytes")?)
        .map_err(|_| invalid_submission_receipt())?;
    if receipt_json.is_empty() || receipt_json.len() != receipt_bytes || receipt_bytes > 64 * 1_024
    {
        return Err(invalid_submission_receipt());
    }
    let record = SubmissionAttemptReceipt {
        submission_draft_id: asterism_domain::SubmissionDraftId::from_str(
            row.try_get("submission_draft_id")?,
        )
        .map_err(|_| invalid_submission_receipt())?,
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|_| invalid_submission_receipt())?,
        execution_attempt_id: ExecutionAttemptId::from_str(row.try_get("execution_attempt_id")?)
            .map_err(|_| invalid_submission_receipt())?,
        receipt: serde_json::from_str(receipt_json)?,
    };
    record
        .receipt
        .validate()
        .map_err(|_| invalid_submission_receipt())?;
    Ok(record)
}

fn invalid_submission_receipt() -> StorageError {
    StorageError::InvalidData(
        "submission receipt is malformed or not bound to the active attempt".to_owned(),
    )
}

fn decode_attempt(row: &sqlx::sqlite::SqliteRow) -> Result<ExecutionAttempt, StorageError> {
    let attempt_no = row.try_get::<i64, _>("attempt_no")?;
    Ok(ExecutionAttempt {
        id: ExecutionAttemptId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        attempt_no: u32::try_from(attempt_no).map_err(|_| {
            StorageError::InvalidData("execution attempt number does not fit u32".to_owned())
        })?,
        started_at: decode_timestamp(row.try_get("started_at")?)?,
        finished_at: decode_optional_timestamp(row.try_get("finished_at")?)?,
        result: row
            .try_get::<Option<&str>, _>("result")?
            .map(decode_enum)
            .transpose()?,
        error_class: row
            .try_get::<Option<&str>, _>("error_class")?
            .map(decode_enum)
            .transpose()?,
        provider_trace_id: row.try_get("provider_trace_id")?,
    })
}

fn decode_progress(row: &sqlx::sqlite::SqliteRow) -> Result<ExecutionProgress, StorageError> {
    let percent = row
        .try_get::<Option<i64>, _>("percent")?
        .map(u8::try_from)
        .transpose()
        .map_err(|_| StorageError::InvalidData("execution percent does not fit u8".to_owned()))?;
    let completed_items = row
        .try_get::<Option<i64>, _>("completed_items")?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| {
            StorageError::InvalidData("completed execution items do not fit u32".to_owned())
        })?;
    let total_items = row
        .try_get::<Option<i64>, _>("total_items")?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| {
            StorageError::InvalidData("total execution items do not fit u32".to_owned())
        })?;
    let progress = ExecutionProgress {
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        percent,
        stage: decode_enum(row.try_get("stage")?)?,
        status_text: row.try_get("status_text")?,
        current_item: row.try_get("current_item")?,
        completed_items,
        total_items,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    progress
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(progress)
}

fn decode_log(row: &sqlx::sqlite::SqliteRow) -> Result<ExecutionLogEvent, StorageError> {
    Ok(ExecutionLogEvent {
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        attempt_id: row
            .try_get::<Option<&str>, _>("attempt_id")?
            .map(ExecutionAttemptId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        timestamp: decode_timestamp(row.try_get("timestamp")?)?,
        level: decode_enum(row.try_get("level")?)?,
        stage: decode_enum(row.try_get("stage")?)?,
        message: row.try_get("message")?,
        provider_trace_id: row.try_get("provider_trace_id")?,
        metadata_sanitized: row
            .try_get::<Option<&str>, _>("metadata_sanitized_json")?
            .map(serde_json::from_str)
            .transpose()?,
    })
}

fn enum_name(value: impl Serialize) -> Result<String, StorageError> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(StorageError::InvalidData(
            "enum did not serialize to a string".to_owned(),
        )),
    }
}

fn decode_enum<T>(value: &str) -> Result<T, StorageError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(Into::into)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn decode_optional_timestamp(value: Option<&str>) -> Result<Option<Timestamp>, StorageError> {
    value.map(decode_timestamp).transpose()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Arc};

    use asterism_domain::{
        CompletionDiagnosis, CompletionPolicySnapshot, CompletionWorkflowBinding, CreditAmount,
        CreditReservation, CreditReservationId, PriceQuote, PriceQuoteId, ProviderAccountId,
        ProviderId, QuestionSession, QuestionSnapshotId, RequestSource, StrictCompletionWorkflow,
        SubmissionAttemptReceipt, SubmissionDraftId, SubmissionReceipt, TaskId,
    };
    use asterism_provider_api::{
        ProviderRuntimeSettingsSchema, ProviderSettingDefinition, ProviderSettingKind,
        ProviderSettingScope,
    };
    use asterism_secrets::{SecretAccess, SecretActor, SecretKey, SecretValue};
    use sha2::Digest as _;
    use sqlx::Row;

    use super::*;
    use crate::{
        CompletionWorkflowRepository, QuestionSessionArtifactRepository, QuestionSessionRepository,
        SqliteCompletionWorkflowRepository, SqliteQuestionSessionRepository,
    };

    #[tokio::test]
    async fn scheduling_is_atomic_and_idempotent() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        let request = || ExecutionScheduleRequest {
            execution: &execution,
            capability_plan: &execution.requested_capabilities,
            capability_call_starts: &[1],
            provider_plan_artifact: None,
            billing: None,
            runtime_settings: None,
            strict_completion_retry: None,
            expected_task_state: OrchestrationState::Ready,
            idempotency_scope: "user:test-owner",
            idempotency_key: "request-1",
            actor: AuditActor::User(owner),
            correlation_id: "correlation-1",
        };

        assert_eq!(
            repository.schedule_execution(request()).await.unwrap(),
            ExecutionScheduleOutcome::Created(execution.clone())
        );
        assert_eq!(
            repository.schedule_execution(request()).await.unwrap(),
            ExecutionScheduleOutcome::Existing(execution.clone())
        );
        assert_eq!(
            repository.find_execution(execution.id).await.unwrap(),
            Some(execution)
        );

        let state: String =
            sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(state, "scheduled");
        for table in [
            "executions",
            "scheduled_jobs",
            "audit_records",
            "event_outbox",
        ] {
            let count: i64 = sqlx::query(&format!("SELECT COUNT(*) AS count FROM {table}"))
                .fetch_one(database.pool())
                .await
                .unwrap()
                .get("count");
            assert_eq!(count, 1, "unexpected row count in {table}");
        }
    }

    #[tokio::test]
    async fn formal_strict_retry_requires_exact_persisted_confirmation() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let workflow = insert_active_formal_workflow(&database, owner, task_id, now).await;

        let execution = scheduled_execution(owner, task_id, now);
        assert_eq!(
            repository
                .schedule_execution(test_request(&execution, owner, "formal-retry-missing"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::StrictCompletionRetryConflict
        );
        let mut confirmed = test_request(&execution, owner, "formal-retry-confirmed");
        confirmed.strict_completion_retry = Some(ExecutionStrictCompletionRetryRequest {
            workflow_id: workflow.id,
            expected_revision: 1,
        });
        assert_eq!(
            repository
                .schedule_execution(confirmed.clone())
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(execution.clone())
        );
        assert_eq!(
            repository.schedule_execution(confirmed).await.unwrap(),
            ExecutionScheduleOutcome::Existing(execution.clone())
        );
        assert_eq!(
            repository
                .find_execution_strict_completion_retry_confirmation(execution.id)
                .await
                .unwrap(),
            Some(ExecutionStrictCompletionRetryConfirmation {
                execution_id: execution.id,
                workflow_id: workflow.id,
                workflow_revision: 1,
                confirmed_by: owner,
                confirmed_at: now,
            })
        );
        let audit_metadata: String = sqlx::query_scalar(
            "SELECT metadata_sanitized_json FROM audit_records \
             WHERE action = 'execution_requested' AND resource_id = ?",
        )
        .bind(execution.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(audit_metadata.contains(&workflow.id.to_string()));
        assert!(audit_metadata.contains("\"workflow_revision\":1"));
        insert_execution_completion_policy(&database, execution.id, now).await;
        let job_id = claim_execution(&database, &execution, "formal-retry-worker", now).await;
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "formal-retry-worker",
                at: now + chrono::Duration::seconds(1),
                correlation_id: "formal-retry-attempt",
            })
            .await
            .unwrap();
        let observation = repository
            .record_strict_completion_execution_observation(
                crate::StrictCompletionExecutionObservationRequest {
                    execution_id: execution.id,
                    execution_attempt_id: attempt.id,
                    scheduler_job_id: job_id,
                    worker_id: "formal-retry-worker",
                    outcome: None,
                    diagnosis: Some(CompletionDiagnosis::DurationInsufficient),
                    at: now + chrono::Duration::seconds(2),
                    correlation_id: "formal-retry-observation",
                },
            )
            .await
            .unwrap();
        assert_eq!(observation.workflow_attempt_no, Some(2));
        assert_eq!(observation.workflow.revision, 2);
        assert_eq!(observation.workflow.workflow.attempts_started, 2);
    }

    #[tokio::test]
    async fn scored_submission_retry_requires_confirmation_fresh_snapshot_and_draft() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let workflow = insert_active_formal_workflow(&database, owner, task_id, now).await;
        sqlx::query("UPDATE tasks SET assessment_class = 'routine' WHERE id = ?")
            .bind(task_id.to_string())
            .execute(database.pool())
            .await
            .unwrap();
        let old_draft =
            insert_submission_draft(&database, task_id, now - chrono::Duration::seconds(2)).await;
        let mut old_execution = scheduled_execution(owner, task_id, now);
        old_execution.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        old_execution.submission_draft_id = Some(old_draft);
        assert_eq!(
            repository
                .schedule_execution(test_request(
                    &old_execution,
                    owner,
                    "submission-without-confirmation",
                ))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::StrictCompletionRetryConflict
        );
        let mut old_request = test_request(&old_execution, owner, "formal-old-draft");
        old_request.strict_completion_retry = Some(ExecutionStrictCompletionRetryRequest {
            workflow_id: workflow.id,
            expected_revision: 1,
        });
        assert_eq!(
            repository.schedule_execution(old_request).await.unwrap(),
            ExecutionScheduleOutcome::StrictCompletionRetryConflict
        );

        let fresh_draft = insert_submission_draft(&database, task_id, now).await;
        let mut fresh_execution = scheduled_execution(owner, task_id, now);
        fresh_execution.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        fresh_execution.submission_draft_id = Some(fresh_draft);
        let mut fresh_request = test_request(&fresh_execution, owner, "formal-fresh-draft");
        fresh_request.strict_completion_retry = Some(ExecutionStrictCompletionRetryRequest {
            workflow_id: workflow.id,
            expected_revision: 1,
        });
        assert_eq!(
            repository.schedule_execution(fresh_request).await.unwrap(),
            ExecutionScheduleOutcome::Created(fresh_execution)
        );
    }

    #[tokio::test]
    async fn composite_capability_steps_are_ordered_and_attempt_bound() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let mut execution = scheduled_execution(owner, task_id, now);
        execution.requested_capabilities = vec![
            TaskCapability::ResourceExecution,
            TaskCapability::DurationReport,
        ];
        let plan = [
            TaskCapability::DurationReport,
            TaskCapability::ResourceExecution,
        ];
        let mut request = test_request(&execution, owner, "composite-capabilities");
        request.capability_plan = &plan;
        request.capability_call_starts = &[1, 2];
        assert!(matches!(
            repository.schedule_execution(request).await.unwrap(),
            ExecutionScheduleOutcome::Created(_)
        ));
        let steps = repository
            .find_execution_capability_steps(execution.id)
            .await
            .unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].position, 1);
        assert_eq!(steps[0].capability, TaskCapability::DurationReport);
        assert_eq!(steps[1].position, 2);
        assert_eq!(steps[1].capability, TaskCapability::ResourceExecution);

        let job_id = claim_execution(&database, &execution, "phase-worker", now).await;
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "phase-worker",
                at: now + chrono::Duration::seconds(1),
                correlation_id: "phase-attempt",
            })
            .await
            .unwrap();
        let mutation = |capability, seconds| ExecutionCapabilityStepMutation {
            execution_id: execution.id,
            attempt_id: attempt.id,
            capability,
            scheduler_job_id: job_id,
            worker_id: "phase-worker",
            correlation_id: "phase-attempt",
            at: now + chrono::Duration::seconds(seconds),
        };
        assert!(
            repository
                .issue_execution_capability_step(mutation(TaskCapability::ResourceExecution, 2,))
                .await
                .is_err()
        );
        assert_eq!(
            repository
                .issue_execution_capability_step(mutation(TaskCapability::DurationReport, 2))
                .await
                .unwrap(),
            ExecutionCapabilityStepIssueOutcome::Issued
        );
        assert_eq!(
            repository
                .issue_execution_capability_step(mutation(TaskCapability::DurationReport, 2))
                .await
                .unwrap(),
            ExecutionCapabilityStepIssueOutcome::AlreadyIssued
        );
        repository
            .succeed_execution_capability_step(mutation(TaskCapability::DurationReport, 3))
            .await
            .unwrap();
        repository
            .issue_execution_capability_step(mutation(TaskCapability::ResourceExecution, 4))
            .await
            .unwrap();
        repository
            .succeed_execution_capability_step(mutation(TaskCapability::ResourceExecution, 5))
            .await
            .unwrap();
        assert!(
            repository
                .find_execution_capability_steps(execution.id)
                .await
                .unwrap()
                .iter()
                .all(|step| step.state == ExecutionCapabilityStepState::Succeeded)
        );
    }

    #[tokio::test]
    async fn grouped_capability_call_is_issued_and_succeeded_as_one_boundary() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let mut execution = scheduled_execution(owner, task_id, now);
        execution.requested_capabilities = vec![
            TaskCapability::ResourceExecution,
            TaskCapability::DurationReport,
        ];
        let plan = [
            TaskCapability::DurationReport,
            TaskCapability::ResourceExecution,
        ];
        let mut schedule = test_request(&execution, owner, "grouped-capabilities");
        schedule.capability_plan = &plan;
        schedule.capability_call_starts = &[1];
        repository.schedule_execution(schedule).await.unwrap();
        let steps = repository
            .find_execution_capability_steps(execution.id)
            .await
            .unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].call_position, 1);
        assert_eq!(steps[0].call_member_position, 1);
        assert_eq!(steps[1].call_position, 1);
        assert_eq!(steps[1].call_member_position, 2);

        let job_id = claim_execution(&database, &execution, "group-worker", now).await;
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "group-worker",
                at: now + chrono::Duration::seconds(1),
                correlation_id: "group-attempt",
            })
            .await
            .unwrap();
        let mutation = |seconds| ExecutionCapabilityCallMutation {
            execution_id: execution.id,
            attempt_id: attempt.id,
            call_position: 1,
            capabilities: &plan,
            scheduler_job_id: job_id,
            worker_id: "group-worker",
            correlation_id: "group-call",
            at: now + chrono::Duration::seconds(seconds),
        };
        let mut wrong = mutation(2);
        wrong.capabilities = &plan[..1];
        assert!(
            repository
                .issue_execution_capability_call(wrong)
                .await
                .is_err()
        );
        assert_eq!(
            repository
                .issue_execution_capability_call(mutation(2))
                .await
                .unwrap(),
            ExecutionCapabilityStepIssueOutcome::Issued
        );
        assert_eq!(
            repository
                .issue_execution_capability_call(mutation(3))
                .await
                .unwrap(),
            ExecutionCapabilityStepIssueOutcome::AlreadyIssued
        );
        let mut wrong = mutation(4);
        wrong.capabilities = &plan[..1];
        assert!(
            repository
                .succeed_execution_capability_call(wrong)
                .await
                .is_err()
        );
        repository
            .succeed_execution_capability_call(mutation(4))
            .await
            .unwrap();
        assert!(
            repository
                .find_execution_capability_steps(execution.id)
                .await
                .unwrap()
                .iter()
                .all(|step| step.state == ExecutionCapabilityStepState::Succeeded)
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one ledger scenario keeps ordering, idempotency, conflict and audit assertions on the same claimed attempt"
    )]
    async fn atomic_mutations_require_ordered_definite_attempt_bound_receipts() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        assert!(matches!(
            repository
                .schedule_execution(test_request(&execution, owner, "atomic-mutations"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(_)
        ));
        let job_id = claim_execution(&database, &execution, "atomic-worker", now).await;
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "atomic-worker",
                at: now + chrono::Duration::seconds(1),
                correlation_id: "atomic-attempt",
            })
            .await
            .unwrap();
        let issue = |ordinal, digest, seconds| ExecutionAtomicMutationIssueRequest {
            execution_id: execution.id,
            attempt_id: attempt.id,
            ordinal,
            scheduler_job_id: job_id,
            worker_id: "atomic-worker",
            operation_type: "welearn.duration.keep",
            request_digest: [digest; 32],
            correlation_id: "atomic-mutation",
            at: now + chrono::Duration::seconds(seconds),
        };
        assert!(
            repository
                .issue_execution_atomic_mutation(issue(2, 2, 2))
                .await
                .is_err()
        );
        let first = match repository
            .issue_execution_atomic_mutation(issue(1, 1, 2))
            .await
            .unwrap()
        {
            ExecutionAtomicMutationIssueOutcome::Issued(mutation) => mutation,
            ExecutionAtomicMutationIssueOutcome::AlreadyIssued(_) => panic!("first issue repeated"),
        };
        assert_eq!(first.ordinal, 1);
        assert_eq!(first.response_digest, None);
        assert!(
            repository
                .issue_execution_atomic_mutation(issue(2, 2, 3))
                .await
                .is_err()
        );
        assert_eq!(
            repository
                .issue_execution_atomic_mutation(issue(1, 1, 3))
                .await
                .unwrap(),
            ExecutionAtomicMutationIssueOutcome::AlreadyIssued(first.clone())
        );
        assert!(
            repository
                .issue_execution_atomic_mutation(issue(1, 9, 3))
                .await
                .is_err()
        );

        let receipt = |ordinal, digest, accepted, seconds| ExecutionAtomicMutationReceiptRequest {
            execution_id: execution.id,
            attempt_id: attempt.id,
            ordinal,
            scheduler_job_id: job_id,
            worker_id: "atomic-worker",
            response_digest: [digest; 32],
            accepted,
            correlation_id: "atomic-receipt",
            at: now + chrono::Duration::seconds(seconds),
        };
        let received = match repository
            .record_execution_atomic_mutation_receipt(receipt(1, 11, true, 4))
            .await
            .unwrap()
        {
            ExecutionAtomicMutationReceiptOutcome::Recorded(mutation) => mutation,
            ExecutionAtomicMutationReceiptOutcome::AlreadyRecorded(_) => {
                panic!("first receipt repeated")
            }
        };
        assert_eq!(received.response_digest, Some([11; 32]));
        assert_eq!(received.accepted, Some(true));
        assert_eq!(
            repository
                .record_execution_atomic_mutation_receipt(receipt(1, 11, true, 5))
                .await
                .unwrap(),
            ExecutionAtomicMutationReceiptOutcome::AlreadyRecorded(received)
        );
        assert!(
            repository
                .record_execution_atomic_mutation_receipt(receipt(1, 12, true, 5))
                .await
                .is_err()
        );

        repository
            .issue_execution_atomic_mutation(issue(2, 2, 6))
            .await
            .unwrap();
        repository
            .record_execution_atomic_mutation_receipt(receipt(2, 22, false, 7))
            .await
            .unwrap();
        repository
            .issue_execution_atomic_mutation(issue(3, 3, 8))
            .await
            .unwrap();
        let mutations = repository
            .find_execution_atomic_mutations(execution.id, attempt.id)
            .await
            .unwrap();
        assert_eq!(mutations.len(), 3);
        assert_eq!(mutations[1].accepted, Some(false));
        assert_eq!(mutations[2].received_at, None);

        let audit_metadata: Vec<String> = sqlx::query_scalar(
            "SELECT metadata_sanitized_json FROM audit_records \
             WHERE resource_id = ? AND action LIKE 'execution_atomic_mutation_%' ORDER BY occurred_at",
        )
        .bind(execution.id.to_string())
        .fetch_all(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_metadata.len(), 5);
        assert!(audit_metadata.iter().all(|metadata| {
            !metadata.contains(&"01".repeat(32))
                && !metadata.contains(&"0b".repeat(32))
                && metadata.contains("[HASHED]")
        }));
    }

    #[tokio::test]
    async fn submission_draft_is_frozen_unique_and_checked_before_scheduling() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let missing_draft_id = SubmissionDraftId::new();
        let mut missing = scheduled_execution(owner, task_id, now);
        missing.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        missing.submission_draft_id = Some(missing_draft_id);
        assert_eq!(
            repository
                .schedule_execution(test_request(&missing, owner, "missing-draft"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::SubmissionDraftConflict
        );
        let task_state: String =
            sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(task_state, "ready");

        let draft_id = insert_submission_draft(&database, task_id, now).await;
        let mut execution = scheduled_execution(owner, task_id, now);
        execution.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        execution.submission_draft_id = Some(draft_id);
        assert_eq!(
            repository
                .schedule_execution(test_request(&execution, owner, "bound-draft"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(execution.clone())
        );
        assert_eq!(
            repository.find_execution(execution.id).await.unwrap(),
            Some(execution.clone())
        );

        let mut duplicate = scheduled_execution(owner, task_id, now);
        duplicate.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        duplicate.submission_draft_id = Some(draft_id);
        assert_eq!(
            repository
                .schedule_execution(test_request(&duplicate, owner, "duplicate-draft"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::SubmissionDraftConflict
        );
        let execution_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM executions")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(execution_count, 1);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression proves rollback on artifact drift and the subsequent atomic claim in one setup"
    )]
    async fn scheduling_atomically_claims_the_draft_question_session() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let (draft_id, snapshot_id) =
            insert_submission_draft_with_snapshot(&database, task_id, now).await;
        let account_id: ProviderAccountId =
            sqlx::query_scalar::<_, String>("SELECT provider_account_id FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap()
                .parse()
                .unwrap();
        let provider_id = ProviderId::new("test").unwrap();
        let artifact = b"encrypted-at-rest-provider-attempt";
        let artifact_digest = sha2::Sha256::digest(artifact).into();
        let session = QuestionSession::active(
            owner,
            account_id,
            task_id,
            provider_id.clone(),
            "0.1.0".to_owned(),
            snapshot_id,
            "test.question-attempt.v1".to_owned(),
            artifact_digest,
            now,
            now + chrono::Duration::minutes(5),
        )
        .unwrap();
        SqliteQuestionSessionRepository::new(database.clone())
            .create_question_session(&session, AuditActor::User(owner), "schedule-session-create")
            .await
            .unwrap();
        let keyring = Arc::new(
            crate::SecretKeyring::new(
                "test-key".to_owned(),
                [("test-key".to_owned(), SecretKey::new([11; 32]))]
                    .into_iter()
                    .collect(),
            )
            .unwrap(),
        );
        let access = SecretAccess {
            actor: SecretActor::CoreService("execution-scheduler-test"),
            correlation_id: "schedule-artifact-attach".to_owned(),
            reason: "test QuestionSession scheduling claim".to_owned(),
        };
        crate::SqliteQuestionSessionArtifactRepository::new(database.clone(), keyring, provider_id)
            .attach_question_session_artifact(crate::QuestionSessionArtifactAttachRequest {
                session_id: session.id,
                phase: "test.questions-ready",
                value: SecretValue::new(artifact.to_vec()),
                attached_at: now,
                access: &access,
            })
            .await
            .unwrap();

        let mut execution = scheduled_execution(owner, task_id, now);
        execution.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        execution.submission_draft_id = Some(draft_id);
        sqlx::query(
            "UPDATE question_session_continuations SET continuation_digest = ? \
             WHERE session_id = ?",
        )
        .bind([99_u8; 32].as_slice())
        .bind(session.id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        assert_eq!(
            repository
                .schedule_execution(test_request(&execution, owner, "session-bound-draft"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::SubmissionDraftConflict
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM executions")
                .fetch_one(database.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT orchestration_state FROM tasks WHERE id = ?",)
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap(),
            "ready"
        );
        sqlx::query(
            "UPDATE question_session_continuations SET continuation_digest = ? \
             WHERE session_id = ?",
        )
        .bind(artifact_digest.as_slice())
        .bind(session.id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        assert_eq!(
            repository
                .schedule_execution(test_request(&execution, owner, "session-bound-draft"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(execution.clone())
        );
        let claimed: (String, String, i64) = sqlx::query_as(
            "SELECT state, execution_id, revision FROM question_sessions WHERE id = ?",
        )
        .bind(session.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(claimed, ("claimed".to_owned(), execution.id.to_string(), 2));
        let continuation_execution: String = sqlx::query_scalar(
            "SELECT execution_id FROM question_session_continuations WHERE session_id = ?",
        )
        .bind(session.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(continuation_execution, execution.id.to_string());
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression keeps receipt idempotency and recovery transition in one attempt"
    )]
    async fn submission_receipt_is_active_attempt_bound_and_idempotent() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let draft_id = insert_submission_draft(&database, task_id, now).await;
        let mut execution = scheduled_execution(owner, task_id, now);
        execution.requested_capabilities = vec![TaskCapability::SubmissionExecute];
        execution.submission_draft_id = Some(draft_id);
        repository
            .schedule_execution(test_request(&execution, owner, "receipt-bound-draft"))
            .await
            .unwrap();
        let job_id = claim_execution(&database, &execution, "receipt-worker", now).await;
        let started_at = now + chrono::Duration::seconds(1);
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "receipt-worker",
                at: started_at,
                correlation_id: "receipt-attempt",
            })
            .await
            .unwrap();
        let record = SubmissionAttemptReceipt {
            submission_draft_id: draft_id,
            execution_id: execution.id,
            execution_attempt_id: attempt.id,
            receipt: SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: None,
                provider_trace_id: Some("trace-receipt".to_owned()),
                received_at: started_at + chrono::Duration::seconds(1),
            },
        };
        let persist = || SubmissionReceiptPersistRequest {
            record: &record,
            scheduler_job_id: job_id,
            worker_id: "receipt-worker",
            correlation_id: "receipt-persist",
            at: record.receipt.received_at,
        };
        repository
            .persist_submission_receipt(persist())
            .await
            .unwrap();
        repository
            .persist_submission_receipt(persist())
            .await
            .unwrap();
        assert_eq!(
            repository
                .find_active_submission_receipt(execution.id)
                .await
                .unwrap(),
            Some(record.clone())
        );

        let mut conflicting = record;
        conflicting.receipt.remote_status = "different".to_owned();
        assert!(
            repository
                .persist_submission_receipt(SubmissionReceiptPersistRequest {
                    record: &conflicting,
                    scheduler_job_id: job_id,
                    worker_id: "receipt-worker",
                    correlation_id: "receipt-conflict",
                    at: conflicting.receipt.received_at,
                })
                .await
                .is_err()
        );

        let recovery_at = conflicting.receipt.received_at + chrono::Duration::seconds(1);
        let progress = ExecutionProgress {
            execution_id: execution.id,
            percent: None,
            stage: ExecutionStage::Verifying,
            status_text: Some("verification-only recovery".to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: recovery_at,
        };
        let recovering = repository
            .begin_verification_recovery(VerificationRecoveryStartRequest {
                execution_id: execution.id,
                attempt_id: attempt.id,
                scheduler_job_id: job_id,
                worker_id: "receipt-worker",
                error_class: asterism_domain::ProviderErrorClass::Network,
                progress: &progress,
                at: recovery_at,
                correlation_id: "receipt-recovery",
            })
            .await
            .unwrap();
        assert_eq!(recovering.state, ExecutionState::Recovering);
        let states: Vec<(String, String)> =
            sqlx::query_as("SELECT job_kind, state FROM scheduled_jobs ORDER BY created_at, id")
                .fetch_all(database.pool())
                .await
                .unwrap();
        assert!(states.contains(&("execution".to_owned(), "completed".to_owned())));
        assert!(states.contains(&("recovery".to_owned(), "pending".to_owned())));
        let mut expected_receipt = conflicting;
        expected_receipt.receipt.remote_status = "accepted".to_owned();
        assert_eq!(
            repository
                .find_active_submission_receipt(execution.id)
                .await
                .unwrap(),
            Some(expected_receipt)
        );
    }

    #[tokio::test]
    async fn scheduling_freezes_one_provider_bound_runtime_settings_snapshot() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        insert_provider_runtime_settings(&database, "test", 4).await;
        let schema = runtime_settings_schema();
        let mut snapshot = runtime_settings_snapshot(now, "test", 4);
        let expected = snapshot.clone();
        let mut request = test_request(&execution, owner, "settings-snapshot");
        request.runtime_settings = Some(ExecutionRuntimeSettingsResolution {
            snapshot: &snapshot,
            schema: &schema,
        });

        assert!(matches!(
            repository.schedule_execution(request).await.unwrap(),
            ExecutionScheduleOutcome::Created(_)
        ));
        snapshot.resolved.values.insert(
            "execution.max_concurrency".to_owned(),
            ProviderSettingValue::Integer(8),
        );
        assert_eq!(
            repository
                .find_execution_runtime_settings(execution.id)
                .await
                .unwrap(),
            Some(expected)
        );

        let audit_metadata: String = sqlx::query_scalar(
            "SELECT metadata_sanitized_json FROM audit_records \
             WHERE resource_type = 'execution' AND resource_id = ?",
        )
        .bind(execution.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(audit_metadata.contains("\"schema_version\":2"));
        assert!(audit_metadata.contains("\"provider_revision\":7"));
        assert!(!audit_metadata.contains("execution.max_concurrency"));
    }

    #[tokio::test]
    async fn scheduling_freezes_and_rechecks_provider_plan_artifact() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        insert_provider_runtime_settings(&database, "test", 4).await;
        let schema = runtime_settings_schema();
        let snapshot = runtime_settings_snapshot(now, "test", 4);
        let artifact = ProviderExecutionPlanArtifact::try_new(
            ProviderId::new("test").unwrap(),
            "test.atomic-child.v1",
            serde_json::json!({"profile": "atomic", "target_seconds": 120}),
        )
        .unwrap();
        let mut request = test_request(&execution, owner, "provider-plan-artifact");
        request.runtime_settings = Some(ExecutionRuntimeSettingsResolution {
            snapshot: &snapshot,
            schema: &schema,
        });
        request.provider_plan_artifact = Some(&artifact);
        repository.schedule_execution(request).await.unwrap();
        assert_eq!(
            repository
                .find_execution_provider_plan_artifact(execution.id)
                .await
                .unwrap(),
            Some(artifact)
        );
        let audit_metadata: String = sqlx::query_scalar(
            "SELECT metadata_sanitized_json FROM audit_records \
             WHERE resource_type = 'execution' AND resource_id = ?",
        )
        .bind(execution.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(audit_metadata.contains("test.atomic-child.v1"));
        assert!(audit_metadata.contains("[HASHED]"));
        assert!(!audit_metadata.contains("target_seconds"));

        sqlx::query(
            "UPDATE execution_provider_plan_artifacts SET artifact_digest = zeroblob(32) \
             WHERE execution_id = ?",
        )
        .bind(execution.id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        assert!(
            repository
                .find_execution_provider_plan_artifact(execution.id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn scheduling_rejects_cross_provider_runtime_settings_without_partial_writes() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        let schema = runtime_settings_schema();
        let snapshot = runtime_settings_snapshot(now, "other-provider", 4);
        let mut request = test_request(&execution, owner, "settings-wrong-provider");
        request.runtime_settings = Some(ExecutionRuntimeSettingsResolution {
            snapshot: &snapshot,
            schema: &schema,
        });

        assert!(matches!(
            repository.schedule_execution(request).await,
            Err(StorageError::InvalidData(_))
        ));
        let execution_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM executions")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let task_state: String =
            sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(execution_count, 0);
        assert_eq!(task_state, "ready");
    }

    #[tokio::test]
    async fn scheduling_rejects_a_stale_resolution_without_partial_writes() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_provider_runtime_settings(&database, "test", 4).await;
        let schema = runtime_settings_schema();
        let snapshot = runtime_settings_snapshot(now, "test", 4);
        let replacement = ProviderRuntimeSettingsPatch {
            schema_version: 2,
            values: BTreeMap::from([(
                "execution.max_concurrency".to_owned(),
                ProviderSettingValue::Integer(5),
            )]),
        };
        sqlx::query(
            "UPDATE provider_runtime_settings SET revision = 8, settings_json = ? \
             WHERE scope = 'provider' AND provider_id = 'test'",
        )
        .bind(serde_json::to_string(&replacement).unwrap())
        .execute(database.pool())
        .await
        .unwrap();
        let execution = scheduled_execution(owner, task_id, now);
        let mut request = test_request(&execution, owner, "settings-stale");
        request.runtime_settings = Some(ExecutionRuntimeSettingsResolution {
            snapshot: &snapshot,
            schema: &schema,
        });

        assert_eq!(
            repository.schedule_execution(request).await.unwrap(),
            ExecutionScheduleOutcome::RuntimeSettingsConflict
        );
        let execution_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM executions")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let task_state: String =
            sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(execution_count, 0);
        assert_eq!(task_state, "ready");
    }

    #[tokio::test]
    async fn billed_scheduling_reserves_credit_atomically_and_replays_without_mutation() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_credit_account(&database, owner, 100, now).await;
        let mut execution = scheduled_execution(owner, task_id, now);
        let quote = PriceQuote {
            id: PriceQuoteId::new(),
            task_id,
            amount: CreditAmount::new(30),
            pricing_revision: "catalog-2026-08".to_owned(),
            reason: "resource execution".to_owned(),
            created_at: now,
        };
        execution.quote_id = Some(quote.id);
        let reservation = CreditReservation {
            id: CreditReservationId::new(),
            user_id: owner,
            quote_id: quote.id,
            execution_id: execution.id,
            amount: quote.amount,
            state: CreditReservationState::Reserved,
            created_at: now,
            updated_at: now,
        };
        let request = || billed_request(&execution, &quote, &reservation, owner, "billed-request");

        assert_eq!(
            repository.schedule_execution(request()).await.unwrap(),
            ExecutionScheduleOutcome::Created(execution.clone())
        );
        assert_eq!(
            repository.schedule_execution(request()).await.unwrap(),
            ExecutionScheduleOutcome::Existing(execution.clone())
        );

        let balance: (i64, i64) =
            sqlx::query_as("SELECT available, reserved FROM credit_accounts WHERE user_id = ?")
                .bind(owner.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(balance, (70, 30));
        for (table, expected) in [
            ("price_quotes", 1_i64),
            ("credit_reservations", 1),
            ("executions", 1),
            ("scheduled_jobs", 1),
            ("audit_records", 1),
            ("event_outbox", 2),
        ] {
            let count: i64 = sqlx::query(&format!("SELECT COUNT(*) AS count FROM {table}"))
                .fetch_one(database.pool())
                .await
                .unwrap()
                .get("count");
            assert_eq!(count, expected, "unexpected row count in {table}");
        }
        let event_types: Vec<String> =
            sqlx::query_scalar("SELECT event_type FROM event_outbox ORDER BY event_type")
                .fetch_all(database.pool())
                .await
                .unwrap();
        assert_eq!(
            event_types,
            vec!["credit_reserved", "execution_state_changed"]
        );
        let audit_metadata: String =
            sqlx::query_scalar("SELECT metadata_sanitized_json FROM audit_records")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(audit_metadata.contains(&quote.id.to_string()));
        assert!(audit_metadata.contains("catalog-2026-08"));
    }

    #[tokio::test]
    async fn insufficient_credit_rolls_back_the_entire_schedule_request() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_credit_account(&database, owner, 10, now).await;
        let mut execution = scheduled_execution(owner, task_id, now);
        let quote = PriceQuote {
            id: PriceQuoteId::new(),
            task_id,
            amount: CreditAmount::new(30),
            pricing_revision: "catalog-2026-08".to_owned(),
            reason: "resource execution".to_owned(),
            created_at: now,
        };
        execution.quote_id = Some(quote.id);
        let reservation = CreditReservation {
            id: CreditReservationId::new(),
            user_id: owner,
            quote_id: quote.id,
            execution_id: execution.id,
            amount: quote.amount,
            state: CreditReservationState::Reserved,
            created_at: now,
            updated_at: now,
        };

        assert!(matches!(
            repository
                .schedule_execution(billed_request(
                    &execution,
                    &quote,
                    &reservation,
                    owner,
                    "insufficient-request",
                ))
                .await,
            Err(StorageError::InsufficientCredits)
        ));
        let task_state: String =
            sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(task_state, "ready");
        let balance: (i64, i64) =
            sqlx::query_as("SELECT available, reserved FROM credit_accounts WHERE user_id = ?")
                .bind(owner.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(balance, (10, 0));
        for table in [
            "price_quotes",
            "credit_reservations",
            "executions",
            "scheduled_jobs",
            "audit_records",
            "event_outbox",
        ] {
            let count: i64 = sqlx::query(&format!("SELECT COUNT(*) AS count FROM {table}"))
                .fetch_one(database.pool())
                .await
                .unwrap()
                .get("count");
            assert_eq!(count, 0, "unexpected row count in {table}");
        }
    }

    #[tokio::test]
    async fn covered_zero_cost_scheduling_creates_an_auditable_reservation() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let mut execution = scheduled_execution(owner, task_id, now);
        let quote = PriceQuote {
            id: PriceQuoteId::new(),
            task_id,
            amount: CreditAmount::ZERO,
            pricing_revision: "entitlement-2026-08".to_owned(),
            reason: "covered by active package".to_owned(),
            created_at: now,
        };
        execution.quote_id = Some(quote.id);
        let reservation = CreditReservation {
            id: CreditReservationId::new(),
            user_id: owner,
            quote_id: quote.id,
            execution_id: execution.id,
            amount: quote.amount,
            state: CreditReservationState::Reserved,
            created_at: now,
            updated_at: now,
        };

        assert!(matches!(
            repository
                .schedule_execution(billed_request(
                    &execution,
                    &quote,
                    &reservation,
                    owner,
                    "covered-request",
                ))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(_)
        ));
        let balance: (i64, i64) =
            sqlx::query_as("SELECT available, reserved FROM credit_accounts WHERE user_id = ?")
                .bind(owner.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(balance, (0, 0));
        let reservation_state: String =
            sqlx::query_scalar("SELECT state FROM credit_reservations WHERE execution_id = ?")
                .bind(execution.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(reservation_state, "reserved");
    }

    #[tokio::test]
    async fn successful_finish_commits_reserved_credit_in_the_worker_transaction() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_credit_account(&database, owner, 100, now).await;
        let (execution, reservation) =
            schedule_billed_execution(&repository, owner, task_id, now, 30, "settle-success").await;
        let (job_id, attempt) = start_execution(&repository, &database, &execution, now).await;
        let finished_at = now + chrono::Duration::seconds(2);
        let progress = completed_progress(execution.id, finished_at);

        let finished = repository
            .finish_attempt(ExecutionAttemptFinishRequest {
                execution_id: execution.id,
                attempt_id: attempt.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                final_state: ExecutionState::Succeeded,
                result: AttemptResult::Succeeded,
                error_class: None,
                provider_trace_id: None,
                retry_at: None,
                progress: &progress,
                at: finished_at,
                correlation_id: "settle-success",
            })
            .await
            .unwrap();

        assert_eq!(finished.state, ExecutionState::Succeeded);
        assert_eq!(credit_balance(&database, owner).await, (70, 0));
        assert_eq!(
            reservation_state(&database, reservation.id).await,
            "committed"
        );
        let ledger: (i64, String, String) = sqlx::query_as(
            "SELECT amount, transaction_type, execution_id FROM credit_transactions",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            ledger,
            (-30, "task_execution".to_owned(), execution.id.to_string())
        );
        assert_eq!(
            event_count(&database, "credit_committed").await,
            1,
            "credit settlement must share the durable outbox"
        );
    }

    #[tokio::test]
    async fn failed_finish_releases_reserved_credit_in_the_worker_transaction() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_credit_account(&database, owner, 100, now).await;
        let (execution, reservation) =
            schedule_billed_execution(&repository, owner, task_id, now, 30, "settle-failed").await;
        let (job_id, attempt) = start_execution(&repository, &database, &execution, now).await;
        let finished_at = now + chrono::Duration::seconds(2);
        let progress = failed_progress(execution.id, finished_at);

        repository
            .finish_attempt(ExecutionAttemptFinishRequest {
                execution_id: execution.id,
                attempt_id: attempt.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                final_state: ExecutionState::Failed,
                result: AttemptResult::Failed,
                error_class: Some(asterism_domain::ProviderErrorClass::InvalidRemoteState),
                provider_trace_id: None,
                retry_at: None,
                progress: &progress,
                at: finished_at,
                correlation_id: "settle-failed",
            })
            .await
            .unwrap();

        assert_eq!(credit_balance(&database, owner).await, (100, 0));
        assert_eq!(
            reservation_state(&database, reservation.id).await,
            "released"
        );
        let ledger_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM credit_transactions")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(ledger_count, 0);
        assert_eq!(event_count(&database, "credit_released").await, 1);
    }

    #[tokio::test]
    async fn settlement_invariant_failure_rolls_back_the_worker_finish() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_credit_account(&database, owner, 100, now).await;
        let (execution, reservation) =
            schedule_billed_execution(&repository, owner, task_id, now, 30, "settle-rollback")
                .await;
        let (job_id, attempt) = start_execution(&repository, &database, &execution, now).await;
        sqlx::query("UPDATE credit_accounts SET reserved = 0 WHERE user_id = ?")
            .bind(owner.to_string())
            .execute(database.pool())
            .await
            .unwrap();
        let finished_at = now + chrono::Duration::seconds(2);
        let progress = completed_progress(execution.id, finished_at);

        assert!(matches!(
            repository
                .finish_attempt(ExecutionAttemptFinishRequest {
                    execution_id: execution.id,
                    attempt_id: attempt.id,
                    scheduler_job_id: job_id,
                    worker_id: "worker-a",
                    final_state: ExecutionState::Succeeded,
                    result: AttemptResult::Succeeded,
                    error_class: None,
                    provider_trace_id: None,
                    retry_at: None,
                    progress: &progress,
                    at: finished_at,
                    correlation_id: "settle-rollback",
                })
                .await,
            Err(StorageError::CreditInvariant)
        ));
        let execution_state: String =
            sqlx::query_scalar("SELECT state FROM executions WHERE id = ?")
                .bind(execution.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(execution_state, "running");
        let attempt_finished_at: Option<String> =
            sqlx::query_scalar("SELECT finished_at FROM execution_attempts WHERE id = ?")
                .bind(attempt.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(attempt_finished_at.is_none());
        assert_eq!(
            reservation_state(&database, reservation.id).await,
            "reserved"
        );
        let job_state: String = sqlx::query_scalar("SELECT state FROM scheduled_jobs WHERE id = ?")
            .bind(job_id.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(job_state, "claimed");
        assert_eq!(event_count(&database, "credit_committed").await, 0);
    }

    #[tokio::test]
    async fn idempotency_key_cannot_be_reused_for_another_task() {
        let (database, owner, task_id) = fixture().await;
        let other_task = insert_task(&database, ProviderAccountId::new(), owner).await;
        let repository = SqliteExecutionRepository::new(database);
        let now = Utc::now();
        let first = scheduled_execution(owner, task_id, now);
        let second = scheduled_execution(owner, other_task, now);
        assert!(matches!(
            repository
                .schedule_execution(test_request(&first, owner, "same-key"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(_)
        ));
        assert_eq!(
            repository
                .schedule_execution(test_request(&second, owner, "same-key"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::IdempotencyConflict
        );

        let mut different_action = first.clone();
        different_action.id = ExecutionId::new();
        different_action.requested_capabilities = vec![TaskCapability::DurationReport];
        assert_eq!(
            repository
                .schedule_execution(test_request(&different_action, owner, "same-key"))
                .await
                .unwrap(),
            ExecutionScheduleOutcome::IdempotencyConflict
        );
    }

    #[tokio::test]
    async fn worker_attempt_progress_and_finish_share_claim_boundaries() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        repository
            .schedule_execution(test_request(&execution, owner, "worker-flow"))
            .await
            .unwrap();
        let job_id = claim_execution(&database, &execution, "worker-a", now).await;
        let started_at = now + chrono::Duration::seconds(1);
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                at: started_at,
                correlation_id: "execution-worker-flow",
            })
            .await
            .unwrap();
        assert_eq!(attempt.attempt_no, 1);
        assert_eq!(
            repository
                .start_attempt(ExecutionAttemptStartRequest {
                    execution_id: execution.id,
                    scheduler_job_id: job_id,
                    worker_id: "worker-a",
                    at: started_at,
                    correlation_id: "execution-worker-flow",
                })
                .await
                .unwrap(),
            attempt
        );

        let secret_metadata = serde_json::json!({"password": "must-not-be-persisted"});
        assert!(matches!(
            repository
                .append_log(ExecutionLogAppendRequest {
                    execution_id: execution.id,
                    attempt_id: attempt.id,
                    worker_id: "worker-a",
                    at: started_at,
                    level: LogLevel::Info,
                    stage: ExecutionStage::Executing,
                    message: "provider diagnostic",
                    provider_trace_id: None,
                    metadata_sanitized: Some(&secret_metadata),
                    correlation_id: "execution-worker-flow",
                })
                .await,
            Err(StorageError::InvalidData(_))
        ));
        let log_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM execution_logs WHERE execution_id = ?")
                .bind(execution.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(log_count, 1);

        assert_progress_claim_and_idempotency(&repository, execution.id, now).await;
        finish_successfully(&repository, &database, &execution, job_id, &attempt, now).await;
    }

    async fn assert_progress_claim_and_idempotency(
        repository: &SqliteExecutionRepository,
        execution_id: ExecutionId,
        now: Timestamp,
    ) {
        let progress = ExecutionProgress {
            execution_id,
            percent: Some(50),
            stage: ExecutionStage::Executing,
            status_text: Some("provider execution in progress".to_owned()),
            current_item: None,
            completed_items: Some(1),
            total_items: Some(2),
            updated_at: now + chrono::Duration::seconds(2),
        };
        assert!(matches!(
            repository
                .update_progress(ExecutionProgressUpdate {
                    progress: &progress,
                    worker_id: "worker-b",
                    correlation_id: "execution-worker-flow",
                })
                .await,
            Err(StorageError::ExecutionClaimLost)
        ));
        assert!(
            repository
                .update_progress(ExecutionProgressUpdate {
                    progress: &progress,
                    worker_id: "worker-a",
                    correlation_id: "execution-worker-flow",
                })
                .await
                .unwrap()
        );
        assert!(
            !repository
                .update_progress(ExecutionProgressUpdate {
                    progress: &progress,
                    worker_id: "worker-a",
                    correlation_id: "execution-worker-flow",
                })
                .await
                .unwrap()
        );
    }

    async fn finish_successfully(
        repository: &SqliteExecutionRepository,
        database: &Database,
        execution: &Execution,
        job_id: ScheduleId,
        attempt: &ExecutionAttempt,
        now: Timestamp,
    ) {
        let finished_at = now + chrono::Duration::seconds(3);
        let completed = ExecutionProgress {
            execution_id: execution.id,
            percent: Some(100),
            stage: ExecutionStage::Completed,
            status_text: Some("provider completion verified".to_owned()),
            current_item: None,
            completed_items: Some(2),
            total_items: Some(2),
            updated_at: finished_at,
        };
        let finished = repository
            .finish_attempt(ExecutionAttemptFinishRequest {
                execution_id: execution.id,
                attempt_id: attempt.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                final_state: ExecutionState::Succeeded,
                result: AttemptResult::Succeeded,
                error_class: None,
                provider_trace_id: Some("trace-1"),
                retry_at: None,
                progress: &completed,
                at: finished_at,
                correlation_id: "execution-worker-flow",
            })
            .await
            .unwrap();
        assert_eq!(finished.state, ExecutionState::Succeeded);
        assert_eq!(finished.finished_at, Some(finished_at));

        let states: (String, String, i64, String, i64) = sqlx::query_as(
            "SELECT e.state, t.orchestration_state, \
                    (SELECT COUNT(*) FROM execution_leases WHERE execution_id = e.id), \
                    j.state, p.percent \
             FROM executions e JOIN tasks t ON t.id = e.task_id \
             JOIN scheduled_jobs j ON j.id = ? \
             JOIN execution_progress p ON p.execution_id = e.id WHERE e.id = ?",
        )
        .bind(job_id.to_string())
        .bind(execution.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            states,
            (
                "succeeded".to_owned(),
                "succeeded".to_owned(),
                0,
                "completed".to_owned(),
                100,
            )
        );
        let attempt_state: (String, String) =
            sqlx::query_as("SELECT result, provider_trace_id FROM execution_attempts WHERE id = ?")
                .bind(attempt.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(
            attempt_state,
            ("succeeded".to_owned(), "trace-1".to_owned())
        );
        let live_log_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox \
             WHERE event_type = 'execution_logged'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(live_log_events, 2);

        assert_execution_read_models(repository, execution, &finished, &completed).await;
    }

    async fn assert_execution_read_models(
        repository: &SqliteExecutionRepository,
        execution: &Execution,
        finished: &Execution,
        completed: &ExecutionProgress,
    ) {
        let owner = execution.requested_by.unwrap();
        let detail = repository
            .find_owned_execution_detail(owner, execution.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&detail.execution, finished);
        assert_eq!(detail.progress.as_ref(), Some(completed));
        assert_eq!(detail.attempts.len(), 1);
        assert_eq!(
            detail.attempts[0].provider_trace_id.as_deref(),
            Some("trace-1")
        );
        let page = repository
            .list_owned_executions(owner, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items, vec![finished.clone()]);
        let task_page = repository
            .list_owned_executions(owner, Some(execution.task_id), 1, 0)
            .await
            .unwrap();
        assert_eq!(task_page.total, 1);
        assert_eq!(task_page.items, vec![finished.clone()]);
        let foreign_page = repository
            .list_owned_executions(UserId::new(), None, 50, 0)
            .await
            .unwrap();
        assert_eq!(foreign_page.total, 0);
        assert!(foreign_page.items.is_empty());
        assert!(
            repository
                .find_owned_execution_detail(UserId::new(), execution.id)
                .await
                .unwrap()
                .is_none()
        );
        let first_log = repository
            .list_owned_execution_logs(owner, execution.id, 1, 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_log.total, 2);
        assert_eq!(first_log.items.len(), 1);
        assert_eq!(first_log.items[0].stage, ExecutionStage::Preparing);
        let final_log = repository
            .list_owned_execution_logs(owner, execution.id, 1, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(final_log.items[0].stage, ExecutionStage::Completed);
        assert!(
            repository
                .list_owned_execution_logs(UserId::new(), execution.id, 50, 0)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn retry_finish_completes_current_job_and_enqueues_next_attempt() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        insert_credit_account(&database, owner, 100, now).await;
        let (execution, reservation) =
            schedule_billed_execution(&repository, owner, task_id, now, 30, "retry-flow").await;
        let job_id = claim_execution(&database, &execution, "worker-a", now).await;
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                at: now + chrono::Duration::seconds(1),
                correlation_id: "execution-retry-flow",
            })
            .await
            .unwrap();
        let failed_at = now + chrono::Duration::seconds(2);
        let retry_at = failed_at + chrono::Duration::minutes(1);
        let progress = ExecutionProgress {
            execution_id: execution.id,
            percent: None,
            stage: ExecutionStage::Executing,
            status_text: Some("provider temporarily unavailable".to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: failed_at,
        };
        repository
            .finish_attempt(ExecutionAttemptFinishRequest {
                execution_id: execution.id,
                attempt_id: attempt.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                final_state: ExecutionState::RetryWaiting,
                result: AttemptResult::Failed,
                error_class: Some(asterism_domain::ProviderErrorClass::ProviderUnavailable),
                provider_trace_id: None,
                retry_at: Some(retry_at),
                progress: &progress,
                at: failed_at,
                correlation_id: "execution-retry-flow",
            })
            .await
            .unwrap();

        let jobs: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT job_kind, state, payload_json FROM scheduled_jobs ORDER BY created_at, id",
        )
        .fetch_all(database.pool())
        .await
        .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            (&jobs[0].0, &jobs[0].1),
            (&"execution".to_owned(), &"completed".to_owned())
        );
        assert_eq!(
            (&jobs[1].0, &jobs[1].1),
            (&"retry".to_owned(), &"pending".to_owned())
        );
        assert!(jobs[1].2.contains("\"next_attempt_no\":2"));
        assert_eq!(credit_balance(&database, owner).await, (70, 30));
        assert_eq!(
            reservation_state(&database, reservation.id).await,
            "reserved"
        );
        assert_eq!(event_count(&database, "credit_committed").await, 0);
        assert_eq!(event_count(&database, "credit_released").await, 0);
    }

    fn test_request<'a>(
        execution: &'a Execution,
        owner: UserId,
        idempotency_key: &'a str,
    ) -> ExecutionScheduleRequest<'a> {
        ExecutionScheduleRequest {
            execution,
            capability_plan: &execution.requested_capabilities,
            capability_call_starts: &[1],
            provider_plan_artifact: None,
            billing: None,
            runtime_settings: None,
            strict_completion_retry: None,
            expected_task_state: OrchestrationState::Ready,
            idempotency_scope: "user:test-owner",
            idempotency_key,
            actor: AuditActor::User(owner),
            correlation_id: "correlation-1",
        }
    }

    fn billed_request<'a>(
        execution: &'a Execution,
        quote: &'a PriceQuote,
        reservation: &'a CreditReservation,
        owner: UserId,
        idempotency_key: &'a str,
    ) -> ExecutionScheduleRequest<'a> {
        ExecutionScheduleRequest {
            execution,
            capability_plan: &execution.requested_capabilities,
            capability_call_starts: &[1],
            provider_plan_artifact: None,
            billing: Some(ExecutionBillingReservation { quote, reservation }),
            runtime_settings: None,
            strict_completion_retry: None,
            expected_task_state: OrchestrationState::Ready,
            idempotency_scope: "user:test-owner",
            idempotency_key,
            actor: AuditActor::User(owner),
            correlation_id: "correlation-billing",
        }
    }

    fn scheduled_execution(owner: UserId, task_id: TaskId, now: Timestamp) -> Execution {
        Execution {
            id: ExecutionId::new(),
            task_id,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
            submission_draft_id: None,
            requested_by: Some(owner),
            request_source: RequestSource::WebUi,
            quote_id: None,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        }
    }

    fn runtime_settings_snapshot(
        captured_at: Timestamp,
        provider_id: &str,
        concurrency: i64,
    ) -> ExecutionRuntimeSettingsSnapshot {
        ExecutionRuntimeSettingsSnapshot {
            provider_id: ProviderId::new(provider_id).unwrap(),
            resolved: ResolvedProviderRuntimeSettings {
                schema_version: 2,
                values: BTreeMap::from([(
                    "execution.max_concurrency".to_owned(),
                    ProviderSettingValue::Integer(concurrency),
                )]),
            },
            sources: BTreeMap::from([(
                "execution.max_concurrency".to_owned(),
                ProviderRuntimeSettingSource::Provider,
            )]),
            completion_policy: asterism_domain::CompletionPolicySnapshot {
                captured_at,
                ..asterism_domain::CompletionPolicySnapshot::default()
            },
            provider_revision: Some(7),
            provider_account_revision: None,
            task_revision: None,
            captured_at,
        }
    }

    fn runtime_settings_schema() -> ProviderRuntimeSettingsSchema {
        ProviderRuntimeSettingsSchema {
            version: 2,
            definitions: vec![ProviderSettingDefinition {
                key: "execution.max_concurrency".to_owned(),
                display_name: "Execution concurrency".to_owned(),
                description: "Maximum concurrent execution work.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 8,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(2),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            }],
        }
    }

    async fn insert_provider_runtime_settings(
        database: &Database,
        provider_id: &str,
        concurrency: i64,
    ) {
        let patch = ProviderRuntimeSettingsPatch {
            schema_version: 2,
            values: BTreeMap::from([(
                "execution.max_concurrency".to_owned(),
                ProviderSettingValue::Integer(concurrency),
            )]),
        };
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO provider_runtime_settings \
             (id, scope, provider_id, schema_version, revision, settings_json, created_at, updated_at) \
             VALUES (?, 'provider', ?, 2, 7, ?, ?, ?)",
        )
        .bind(asterism_domain::ProviderRuntimeSettingsId::new().to_string())
        .bind(provider_id)
        .bind(serde_json::to_string(&patch).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
    }

    async fn schedule_billed_execution(
        repository: &SqliteExecutionRepository,
        owner: UserId,
        task_id: TaskId,
        now: Timestamp,
        amount: u64,
        idempotency_key: &str,
    ) -> (Execution, CreditReservation) {
        let mut execution = scheduled_execution(owner, task_id, now);
        let quote = PriceQuote {
            id: PriceQuoteId::new(),
            task_id,
            amount: CreditAmount::new(amount),
            pricing_revision: "catalog-2026-08".to_owned(),
            reason: "resource execution".to_owned(),
            created_at: now,
        };
        execution.quote_id = Some(quote.id);
        let reservation = CreditReservation {
            id: CreditReservationId::new(),
            user_id: owner,
            quote_id: quote.id,
            execution_id: execution.id,
            amount: quote.amount,
            state: CreditReservationState::Reserved,
            created_at: now,
            updated_at: now,
        };
        repository
            .schedule_execution(billed_request(
                &execution,
                &quote,
                &reservation,
                owner,
                idempotency_key,
            ))
            .await
            .unwrap();
        (execution, reservation)
    }

    async fn start_execution(
        repository: &SqliteExecutionRepository,
        database: &Database,
        execution: &Execution,
        now: Timestamp,
    ) -> (ScheduleId, ExecutionAttempt) {
        let job_id = claim_execution(database, execution, "worker-a", now).await;
        let attempt = repository
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id: "worker-a",
                at: now + chrono::Duration::seconds(1),
                correlation_id: "settlement-start",
            })
            .await
            .unwrap();
        (job_id, attempt)
    }

    fn completed_progress(execution_id: ExecutionId, at: Timestamp) -> ExecutionProgress {
        ExecutionProgress {
            execution_id,
            percent: Some(100),
            stage: ExecutionStage::Completed,
            status_text: Some("execution completed".to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        }
    }

    fn failed_progress(execution_id: ExecutionId, at: Timestamp) -> ExecutionProgress {
        ExecutionProgress {
            execution_id,
            percent: None,
            stage: ExecutionStage::Verifying,
            status_text: Some("execution failed".to_owned()),
            current_item: None,
            completed_items: None,
            total_items: None,
            updated_at: at,
        }
    }

    async fn credit_balance(database: &Database, owner: UserId) -> (i64, i64) {
        sqlx::query_as("SELECT available, reserved FROM credit_accounts WHERE user_id = ?")
            .bind(owner.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap()
    }

    async fn reservation_state(database: &Database, reservation_id: CreditReservationId) -> String {
        sqlx::query_scalar("SELECT state FROM credit_reservations WHERE id = ?")
            .bind(reservation_id.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap()
    }

    async fn event_count(database: &Database, event_type: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE event_type = ?")
            .bind(event_type)
            .fetch_one(database.pool())
            .await
            .unwrap()
    }

    async fn fixture() -> (Database, UserId, TaskId) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        insert_user(&database, owner).await;
        let task_id = insert_task(&database, ProviderAccountId::new(), owner).await;
        (database, owner, task_id)
    }

    async fn insert_active_formal_workflow(
        database: &Database,
        owner: UserId,
        task_id: TaskId,
        now: Timestamp,
    ) -> StrictCompletionWorkflow {
        sqlx::query("UPDATE tasks SET assessment_class = 'formal' WHERE id = ?")
            .bind(task_id.to_string())
            .execute(database.pool())
            .await
            .unwrap();
        let account_id: ProviderAccountId =
            sqlx::query_scalar::<_, String>("SELECT provider_account_id FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap()
                .parse()
                .unwrap();
        let mut workflow = StrictCompletionWorkflow::new(
            CompletionWorkflowBinding {
                owner_user_id: owner,
                provider_account_id: account_id,
                task_id,
            },
            CompletionPolicySnapshot {
                captured_at: now - chrono::Duration::seconds(3),
                ..CompletionPolicySnapshot::default()
            },
            None,
            now - chrono::Duration::seconds(3),
        )
        .unwrap();
        workflow
            .begin_attempt(true, false, now - chrono::Duration::seconds(2))
            .unwrap();
        workflow
            .observe(
                None,
                Some(CompletionDiagnosis::DurationInsufficient),
                now - chrono::Duration::seconds(1),
            )
            .unwrap();
        SqliteCompletionWorkflowRepository::new(database.clone())
            .create_strict_completion_workflow(&workflow)
            .await
            .unwrap();
        workflow
    }

    async fn insert_submission_draft(
        database: &Database,
        task_id: TaskId,
        now: Timestamp,
    ) -> SubmissionDraftId {
        insert_submission_draft_with_snapshot(database, task_id, now)
            .await
            .0
    }

    async fn insert_execution_completion_policy(
        database: &Database,
        execution_id: ExecutionId,
        captured_at: Timestamp,
    ) {
        let policy = CompletionPolicySnapshot {
            captured_at,
            ..CompletionPolicySnapshot::default()
        };
        sqlx::query(
            "INSERT INTO execution_runtime_settings \
             (execution_id, provider_id, schema_version, resolved_settings_json, sources_json, \
              completion_policy_json, captured_at) VALUES (?, 'test', 1, '{}', '{}', ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(serde_json::to_string(&policy).unwrap())
        .bind(encode_timestamp(captured_at))
        .execute(database.pool())
        .await
        .unwrap();
    }

    async fn insert_submission_draft_with_snapshot(
        database: &Database,
        task_id: TaskId,
        now: Timestamp,
    ) -> (SubmissionDraftId, QuestionSnapshotId) {
        let snapshot_id = QuestionSnapshotId::new();
        sqlx::query(
            "INSERT INTO question_snapshots \
             (id, task_id, provider_id, provider_version, captured_at, question_count, total_bytes) \
             VALUES (?, ?, 'test', '0.1.0', ?, 0, 0)",
        )
        .bind(snapshot_id.to_string())
        .bind(task_id.to_string())
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        let draft_id = SubmissionDraftId::new();
        let preview = r#"{"encoding":"json","format":"test.v1","fields":[]}"#;
        sqlx::query(
            "INSERT INTO submission_drafts \
             (id, question_snapshot_id, task_id, provider_id, provider_version, \
              payload_preview_json, preview_bytes, item_count, created_at) \
             VALUES (?, ?, ?, 'test', '0.1.0', ?, ?, 1, ?)",
        )
        .bind(draft_id.to_string())
        .bind(snapshot_id.to_string())
        .bind(task_id.to_string())
        .bind(preview)
        .bind(i64::try_from(preview.len()).unwrap())
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        (draft_id, snapshot_id)
    }

    async fn insert_credit_account(
        database: &Database,
        owner: UserId,
        available: i64,
        now: Timestamp,
    ) {
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, ?, 0, ?)",
        )
        .bind(owner.to_string())
        .bind(available)
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
    }

    async fn claim_execution(
        database: &Database,
        execution: &Execution,
        worker_id: &str,
        now: Timestamp,
    ) -> ScheduleId {
        let job_id = ScheduleId::from_str(
            &sqlx::query_scalar::<_, String>(
                "SELECT id FROM scheduled_jobs WHERE idempotency_key = ?",
            )
            .bind(format!("execution:{}", execution.id))
            .fetch_one(database.pool())
            .await
            .unwrap(),
        )
        .unwrap();
        let expires_at = encode_timestamp(now + chrono::Duration::minutes(5));
        sqlx::query(
            "UPDATE scheduled_jobs SET state = 'claimed', worker_id = ?, lease_expires_at = ? \
             WHERE id = ?",
        )
        .bind(worker_id)
        .bind(&expires_at)
        .bind(job_id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_leases (task_id, execution_id, worker_id, expires_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(execution.task_id.to_string())
        .bind(execution.id.to_string())
        .bind(worker_id)
        .bind(expires_at)
        .execute(database.pool())
        .await
        .unwrap();
        job_id
    }

    async fn insert_user(database: &Database, owner: UserId) {
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(format!("user-{owner}"))
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
    }

    async fn insert_task(
        database: &Database,
        account_id: ProviderAccountId,
        owner: UserId,
    ) -> TaskId {
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'test', 'Test', '{\"state\":\"idle\"}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let task_id = TaskId::new();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, ?, ?, 'resource', 'routine', 'Task', 'pending', 'ready', ?, ?, \
                     '[\"resource_execution\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(format!("remote-{task_id}"))
        .bind(format!("fingerprint-{task_id}"))
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        task_id
    }
}
