use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, BatchExecution, BatchExecutionId, ExecutionState, ProviderAccountId,
    TaskCapability, Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{
    BatchExecutionRepository, BatchExecutionScheduleOutcome, BatchExecutionScheduleRequest,
    Database, StorageError, outbox::enqueue_in_transaction,
};

#[derive(Clone, Debug)]
pub struct SqliteBatchExecutionRepository {
    database: Database,
}

impl SqliteBatchExecutionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "parent scheduling keeps binding, idempotency, job, audit and outbox writes visibly atomic"
)]
impl BatchExecutionRepository for SqliteBatchExecutionRepository {
    async fn find_idempotent_batch_execution(
        &self,
        idempotency_scope: &str,
        idempotency_key: &str,
    ) -> Result<Option<BatchExecution>, StorageError> {
        validate_idempotency(idempotency_scope, idempotency_key)?;
        sqlx::query(BATCH_EXECUTION_SELECT_BY_IDEMPOTENCY)
            .bind(idempotency_scope)
            .bind(idempotency_key)
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_batch_execution)
            .transpose()
    }

    async fn schedule_batch_execution(
        &self,
        request: BatchExecutionScheduleRequest<'_>,
    ) -> Result<BatchExecutionScheduleOutcome, StorageError> {
        validate_schedule_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(row) = sqlx::query(BATCH_EXECUTION_SELECT_BY_IDEMPOTENCY)
            .bind(request.idempotency_scope)
            .bind(request.idempotency_key)
            .fetch_optional(&mut *transaction)
            .await?
        {
            let existing = decode_batch_execution(&row)?;
            transaction.commit().await?;
            return if existing == *request.batch_execution {
                Ok(BatchExecutionScheduleOutcome::Existing(existing))
            } else {
                Ok(BatchExecutionScheduleOutcome::IdempotencyConflict)
            };
        }

        let binding = sqlx::query(
            "SELECT account.owner_user_id, account.id AS provider_account_id, \
                    course.id AS course_id \
             FROM provider_accounts AS account \
             INNER JOIN courses AS course ON course.provider_account_id = account.id \
             WHERE account.id = ? AND course.id = ?",
        )
        .bind(request.batch_execution.provider_account_id.to_string())
        .bind(request.batch_execution.course_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(binding) = binding else {
            transaction.rollback().await?;
            return Ok(BatchExecutionScheduleOutcome::BindingConflict);
        };
        let owner_id = UserId::from_str(binding.try_get("owner_user_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let actor_matches = match request.actor {
            AuditActor::User(actor) => {
                actor == owner_id && request.batch_execution.requested_by == Some(actor)
            }
            AuditActor::ServiceToken(_) => request
                .batch_execution
                .requested_by
                .is_none_or(|requester| requester == owner_id),
        };
        if !actor_matches {
            transaction.rollback().await?;
            return Ok(BatchExecutionScheduleOutcome::BindingConflict);
        }

        let batch = request.batch_execution;
        sqlx::query(
            "INSERT INTO batch_executions \
             (id, provider_account_id, course_id, requested_capabilities_json, \
              expected_child_count, requested_by, request_source, state, scheduled_at, \
              started_at, finished_at, idempotency_scope, idempotency_key, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(batch.id.to_string())
        .bind(batch.provider_account_id.to_string())
        .bind(batch.course_id.to_string())
        .bind(serde_json::to_string(&batch.requested_capabilities)?)
        .bind(i64::from(batch.expected_child_count))
        .bind(batch.requested_by.map(|value| value.to_string()))
        .bind(enum_name(batch.request_source)?)
        .bind(enum_name(batch.state)?)
        .bind(batch.scheduled_at.map(encode_timestamp))
        .bind(batch.started_at.map(encode_timestamp))
        .bind(batch.finished_at.map(encode_timestamp))
        .bind(request.idempotency_scope)
        .bind(request.idempotency_key)
        .bind(encode_timestamp(batch.created_at))
        .execute(&mut *transaction)
        .await?;
        let job_kind = ScheduledJobKind::BatchExecution {
            batch_execution_id: batch.id,
        };
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, \
              created_at, updated_at) \
             VALUES (?, 'batch_execution', ?, ?, 'pending', 0, ?, ?, ?)",
        )
        .bind(asterism_domain::ScheduleId::new().to_string())
        .bind(serde_json::to_string(&job_kind)?)
        .bind(encode_timestamp(batch.scheduled_at.ok_or_else(|| {
            StorageError::InvalidData("batch execution schedule time is missing".to_owned())
        })?))
        .bind(format!("batch-execution:{}", batch.id))
        .bind(encode_timestamp(batch.created_at))
        .bind(encode_timestamp(batch.created_at))
        .execute(&mut *transaction)
        .await?;
        insert_batch_execution_audit(&mut transaction, &request).await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                request.correlation_id,
                DomainEvent::BatchExecutionStateChanged {
                    batch_execution_id: batch.id,
                    state: batch.state,
                },
                batch.created_at,
            ),
        )
        .await?;
        transaction.commit().await?;
        Ok(BatchExecutionScheduleOutcome::Created(batch.clone()))
    }

    async fn find_batch_execution(
        &self,
        batch_execution_id: BatchExecutionId,
    ) -> Result<Option<BatchExecution>, StorageError> {
        sqlx::query(BATCH_EXECUTION_SELECT_BY_ID)
            .bind(batch_execution_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_batch_execution)
            .transpose()
    }
}

const BATCH_EXECUTION_SELECT_BY_ID: &str = "SELECT id, provider_account_id, course_id, requested_capabilities_json, \
            expected_child_count, requested_by, request_source, state, scheduled_at, \
            started_at, finished_at, created_at \
     FROM batch_executions WHERE id = ?";
const BATCH_EXECUTION_SELECT_BY_IDEMPOTENCY: &str = "SELECT id, provider_account_id, course_id, requested_capabilities_json, \
            expected_child_count, requested_by, request_source, state, scheduled_at, \
            started_at, finished_at, created_at \
     FROM batch_executions WHERE idempotency_scope = ? AND idempotency_key = ?";

fn validate_schedule_request(
    request: &BatchExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    request
        .batch_execution
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    validate_idempotency(request.idempotency_scope, request.idempotency_key)?;
    if request.batch_execution.state != ExecutionState::Scheduled
        || !valid_token(request.correlation_id, 256)
    {
        return Err(StorageError::InvalidData(
            "batch execution schedule request is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_idempotency(scope: &str, key: &str) -> Result<(), StorageError> {
    if valid_token(scope, 256) && valid_token(key, 256) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "batch execution idempotency identity is invalid".to_owned(),
        ))
    }
}

fn valid_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn decode_batch_execution(row: &sqlx::sqlite::SqliteRow) -> Result<BatchExecution, StorageError> {
    let execution = BatchExecution {
        id: BatchExecutionId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        course_id: asterism_domain::CourseId::from_str(row.try_get("course_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        requested_capabilities: serde_json::from_str::<Vec<TaskCapability>>(
            row.try_get("requested_capabilities_json")?,
        )?,
        expected_child_count: u32::try_from(row.try_get::<i64, _>("expected_child_count")?)
            .map_err(|_| {
                StorageError::InvalidData("batch execution child count is invalid".to_owned())
            })?,
        requested_by: row
            .try_get::<Option<&str>, _>("requested_by")?
            .map(UserId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        request_source: decode_enum(row.try_get("request_source")?)?,
        state: decode_enum(row.try_get("state")?)?,
        scheduled_at: decode_optional_timestamp(row.try_get("scheduled_at")?)?,
        started_at: decode_optional_timestamp(row.try_get("started_at")?)?,
        finished_at: decode_optional_timestamp(row.try_get("finished_at")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    };
    execution
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(execution)
}

async fn insert_batch_execution_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &BatchExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match request.actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let batch = request.batch_execution;
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'batch_execution_requested', 'batch_execution', ?, ?, \
                 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(batch.created_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(batch.id.to_string())
    .bind(request.correlation_id)
    .bind(
        serde_json::json!({
            "provider_account_id": batch.provider_account_id,
            "course_id": batch.course_id,
            "requested_capabilities": batch.requested_capabilities,
            "expected_child_count": batch.expected_child_count,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn enum_name(value: impl serde::Serialize) -> Result<String, StorageError> {
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
    use super::*;
    use asterism_domain::{
        BatchExecutionId, CourseId, ProviderAccountId, ProviderId, RequestSource, UserId,
    };

    #[tokio::test]
    async fn scheduling_is_course_bound_atomic_and_idempotent_without_touching_tasks() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner = UserId::new();
        let account_id = ProviderAccountId::new();
        let course_id = CourseId::new();
        insert_fixture(&database, owner, account_id, course_id, now).await;
        let repository = SqliteBatchExecutionRepository::new(database.clone());
        let batch = BatchExecution {
            id: BatchExecutionId::new(),
            provider_account_id: account_id,
            course_id,
            requested_capabilities: vec![
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ],
            expected_child_count: 2,
            requested_by: Some(owner),
            request_source: RequestSource::WebUi,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        };
        let request = || BatchExecutionScheduleRequest {
            batch_execution: &batch,
            idempotency_scope: "user:test-owner",
            idempotency_key: "course-batch-1",
            actor: AuditActor::User(owner),
            correlation_id: "course-batch-request",
        };
        assert_eq!(
            repository
                .schedule_batch_execution(request())
                .await
                .unwrap(),
            BatchExecutionScheduleOutcome::Created(batch.clone())
        );
        assert_eq!(
            repository
                .schedule_batch_execution(request())
                .await
                .unwrap(),
            BatchExecutionScheduleOutcome::Existing(batch.clone())
        );
        assert_eq!(
            repository.find_batch_execution(batch.id).await.unwrap(),
            Some(batch.clone())
        );
        assert_eq!(
            repository
                .find_idempotent_batch_execution("user:test-owner", "course-batch-1")
                .await
                .unwrap(),
            Some(batch)
        );
        let job: (String, String) = sqlx::query_as(
            "SELECT job_kind, payload_json FROM scheduled_jobs \
             WHERE idempotency_key LIKE 'batch-execution:%'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(job.0, "batch_execution");
        assert!(job.1.contains("batch_execution_id"));
        let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(task_count, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_records WHERE action = 'batch_execution_requested'",
            )
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM event_outbox WHERE event_type = 'batch_execution_state_changed'",
            )
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );
    }

    async fn insert_fixture(
        database: &Database,
        owner: UserId,
        account_id: ProviderAccountId,
        course_id: CourseId,
        now: Timestamp,
    ) {
        let timestamp = encode_timestamp(now);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(format!("user-{owner}"))
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'Test', '{\"state\":\"idle\"}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(ProviderId::new("test").unwrap().as_str())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO courses \
             (id, provider_account_id, remote_id, title, metadata_json, last_seen_at) \
             VALUES (?, ?, 'course-remote', 'Course', '{}', ?)",
        )
        .bind(course_id.to_string())
        .bind(account_id.to_string())
        .bind(timestamp)
        .execute(database.pool())
        .await
        .unwrap();
    }
}
