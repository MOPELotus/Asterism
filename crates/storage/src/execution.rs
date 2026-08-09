use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, Execution, ExecutionId, ExecutionState, OrchestrationState,
    ScheduleId, Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::Row;

use crate::outbox::enqueue_in_transaction;
use crate::{
    Database, ExecutionRepository, ExecutionScheduleOutcome, ExecutionScheduleRequest, StorageError,
};

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

fn same_request(existing: &Execution, requested: &Execution) -> bool {
    existing.task_id == requested.task_id
        && existing.requested_by == requested.requested_by
        && existing.request_source == requested.request_source
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
        task_id: asterism_domain::TaskId::from_str(row.try_get("task_id")?)
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
