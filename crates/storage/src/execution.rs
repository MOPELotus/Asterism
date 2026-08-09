use std::str::FromStr;

use asterism_domain::{
    AttemptResult, AuditActor, AuditRecordId, Execution, ExecutionAttempt, ExecutionAttemptId,
    ExecutionId, ExecutionLogEvent, ExecutionProgress, ExecutionStage, ExecutionState, LogLevel,
    OrchestrationState, ScheduleId, TaskId, Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::Row;

use crate::outbox::enqueue_in_transaction;
use crate::{
    Database, ExecutionAttemptFinishRequest, ExecutionAttemptStartRequest, ExecutionProgressUpdate,
    ExecutionQueryRepository, ExecutionRepository, ExecutionScheduleOutcome,
    ExecutionScheduleRequest, StorageError,
};
use crate::{ExecutionDetail, ExecutionLogPage, ExecutionPage};

const MAX_EXECUTION_ATTEMPTS: usize = 1_000;
const MAX_EXECUTION_PAGE_SIZE: u32 = 200;
const MAX_EXECUTION_OFFSET: u64 = 1_000_000;
const MAX_EXECUTION_LOG_PAGE_SIZE: u32 = 200;
const MAX_EXECUTION_LOG_OFFSET: u64 = 1_000_000;

#[derive(Clone, Debug)]
pub struct SqliteExecutionRepository {
    database: Database,
}

impl SqliteExecutionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ExecutionRepository for SqliteExecutionRepository {
    async fn find_idempotent_execution(
        &self,
        idempotency_scope: &str,
        idempotency_key: &str,
    ) -> Result<Option<Execution>, StorageError> {
        validate_idempotency_tokens(idempotency_scope, idempotency_key)?;
        sqlx::query(
            "SELECT id, task_id, requested_by, request_source, quote_id, state, scheduled_at, \
                    started_at, finished_at, created_at FROM executions \
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

        if let Some(row) = sqlx::query(
            "SELECT id, task_id, requested_by, request_source, quote_id, state, scheduled_at, \
                    started_at, finished_at, created_at \
             FROM executions WHERE idempotency_scope = ? AND idempotency_key = ?",
        )
        .bind(request.idempotency_scope)
        .bind(request.idempotency_key)
        .fetch_optional(&mut *transaction)
        .await?
        {
            let existing = decode_execution(&row)?;
            transaction.commit().await?;
            return if same_request(&existing, request.execution) {
                Ok(ExecutionScheduleOutcome::Existing(existing))
            } else {
                Ok(ExecutionScheduleOutcome::IdempotencyConflict)
            };
        }

        let expected_task_state = enum_name(request.expected_task_state)?;
        let task_update = sqlx::query(
            "UPDATE tasks SET orchestration_state = 'scheduled', updated_at = ? \
             WHERE id = ? AND orchestration_state = ?",
        )
        .bind(encode_timestamp(request.execution.created_at))
        .bind(request.execution.task_id.to_string())
        .bind(expected_task_state)
        .execute(&mut *transaction)
        .await?;
        if task_update.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(ExecutionScheduleOutcome::TaskStateConflict);
        }

        let execution = request.execution;
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_by, request_source, quote_id, state, scheduled_at, started_at, \
              finished_at, created_at, idempotency_scope, idempotency_key) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(execution.id.to_string())
        .bind(execution.task_id.to_string())
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
            "SELECT id, task_id, requested_by, request_source, quote_id, state, scheduled_at, \
                    started_at, finished_at, created_at FROM executions WHERE id = ?",
        )
        .bind(execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?
        .map(|row| decode_execution(&row))
        .transpose()
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
        apply_attempt_finish(&mut transaction, &mut execution, &request).await?;
        record_attempt_finished(&mut transaction, &request).await?;
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
            "SELECT execution.id, execution.task_id, execution.requested_by, \
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
            "SELECT execution.id, execution.task_id, execution.requested_by, \
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
        },
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
) -> Result<(), StorageError> {
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
    update_finished_states(transaction, execution, request).await?;
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
) -> Result<(), StorageError> {
    let task_state = orchestration_for_execution(request.final_state)?;
    let finished_at = request.final_state.is_terminal().then_some(request.at);
    let execution_update = sqlx::query(
        "UPDATE executions SET state = ?, scheduled_at = COALESCE(?, scheduled_at), \
             finished_at = ? WHERE id = ? AND state = 'running'",
    )
    .bind(enum_name(request.final_state)?)
    .bind(request.retry_at.map(encode_timestamp))
    .bind(finished_at.map(encode_timestamp))
    .bind(request.execution_id.to_string())
    .execute(&mut **transaction)
    .await?;
    let task_update = sqlx::query(
        "UPDATE tasks SET orchestration_state = ?, updated_at = ? \
         WHERE id = ? AND orchestration_state = 'running'",
    )
    .bind(enum_name(task_state)?)
    .bind(encode_timestamp(request.at))
    .bind(execution.task_id.to_string())
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
) -> Result<(), StorageError> {
    insert_worker_audit(
        transaction,
        request.execution_id,
        request.worker_id,
        "execution_attempt_finished",
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
            message: "execution attempt finished",
            provider_trace_id: request.provider_trace_id,
        },
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
        "SELECT id, task_id, requested_by, request_source, quote_id, state, scheduled_at, \
                started_at, finished_at, created_at FROM executions WHERE id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::ExecutionStateConflict)
}

async fn assert_worker_claims(
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

fn validate_worker_token(worker_id: &str, correlation_id: &str) -> Result<(), StorageError> {
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
}

async fn insert_execution_log(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: ExecutionLogInsert<'_>,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO execution_logs \
         (id, execution_id, attempt_id, timestamp, level, stage, message, provider_trace_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(record.execution_id.to_string())
    .bind(record.attempt_id.map(|id| id.to_string()))
    .bind(encode_timestamp(record.at))
    .bind(enum_name(record.level)?)
    .bind(enum_name(record.stage)?)
    .bind(record.message)
    .bind(record.provider_trace_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
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
    if execution.state != ExecutionState::Scheduled
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
    {
        return Err(StorageError::InvalidData(
            "execution schedule request is invalid".to_owned(),
        ));
    }
    Ok(())
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
        && existing.requested_by == requested.requested_by
        && existing.quote_id == requested.quote_id
}

async fn insert_request_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &ExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match request.actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let metadata = serde_json::json!({
        "task_id": request.execution.task_id,
        "request_source": request.execution.request_source,
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

fn decode_execution(row: &sqlx::sqlite::SqliteRow) -> Result<Execution, StorageError> {
    Ok(Execution {
        id: ExecutionId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        task_id: TaskId::from_str(row.try_get("task_id")?)
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
    use asterism_domain::{ProviderAccountId, RequestSource, TaskId};
    use sqlx::Row;

    use super::*;

    #[tokio::test]
    async fn scheduling_is_atomic_and_idempotent() {
        let (database, owner, task_id) = fixture().await;
        let repository = SqliteExecutionRepository::new(database.clone());
        let now = Utc::now();
        let execution = scheduled_execution(owner, task_id, now);
        let request = || ExecutionScheduleRequest {
            execution: &execution,
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
        let execution = scheduled_execution(owner, task_id, now);
        repository
            .schedule_execution(test_request(&execution, owner, "retry-flow"))
            .await
            .unwrap();
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
    }

    fn test_request<'a>(
        execution: &'a Execution,
        owner: UserId,
        idempotency_key: &'a str,
    ) -> ExecutionScheduleRequest<'a> {
        ExecutionScheduleRequest {
            execution,
            expected_task_state: OrchestrationState::Ready,
            idempotency_scope: "user:test-owner",
            idempotency_key,
            actor: AuditActor::User(owner),
            correlation_id: "correlation-1",
        }
    }

    fn scheduled_execution(owner: UserId, task_id: TaskId, now: Timestamp) -> Execution {
        Execution {
            id: ExecutionId::new(),
            task_id,
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

    async fn fixture() -> (Database, UserId, TaskId) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        insert_user(&database, owner).await;
        let task_id = insert_task(&database, ProviderAccountId::new(), owner).await;
        (database, owner, task_id)
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
