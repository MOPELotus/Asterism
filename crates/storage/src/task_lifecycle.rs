use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, ExecutionId, ExecutionState, OrchestrationState,
    TaskActionReceiptId, TaskId, TaskLifecycleAction, Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{
    Database, StorageError, TaskLifecycleMutation, TaskLifecycleMutationOutcome,
    TaskLifecycleReceipt, TaskLifecycleRepository, credit::settle_execution_reservation,
    outbox::enqueue_in_transaction,
};

const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
const MAX_CORRELATION_ID_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct SqliteTaskLifecycleRepository {
    database: Database,
}

impl SqliteTaskLifecycleRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "the transaction keeps idempotency, task/execution/job/credit, audit and outbox effects visibly atomic"
)]
impl TaskLifecycleRepository for SqliteTaskLifecycleRepository {
    async fn find_task_lifecycle_receipt(
        &self,
        owner_id: UserId,
        idempotency_key: &str,
    ) -> Result<Option<TaskLifecycleReceipt>, StorageError> {
        validate_token(
            idempotency_key,
            MAX_IDEMPOTENCY_KEY_BYTES,
            "idempotency key",
        )?;
        find_receipt(self.database.pool(), owner_id, idempotency_key).await
    }

    async fn apply_task_lifecycle_mutation(
        &self,
        mutation: TaskLifecycleMutation<'_>,
    ) -> Result<TaskLifecycleMutationOutcome, StorageError> {
        validate_mutation(&mutation)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;

        if let Some(existing) = find_receipt(
            &mut *transaction,
            mutation.owner_id,
            mutation.idempotency_key,
        )
        .await?
        {
            transaction.commit().await?;
            return if receipt_matches(&existing, &mutation) {
                Ok(TaskLifecycleMutationOutcome::Existing(existing))
            } else {
                Ok(TaskLifecycleMutationOutcome::IdempotencyConflict)
            };
        }

        let current_state: Option<String> = sqlx::query_scalar(
            "SELECT task.orchestration_state FROM tasks AS task \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE task.id = ? AND account.owner_user_id = ?",
        )
        .bind(mutation.task_id.to_string())
        .bind(mutation.owner_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(current_state) = current_state else {
            transaction.rollback().await?;
            return Ok(TaskLifecycleMutationOutcome::TaskNotFound);
        };
        let current_state: OrchestrationState = decode_enum(&current_state)?;
        if current_state != mutation.expected_task_state {
            transaction.rollback().await?;
            return Ok(TaskLifecycleMutationOutcome::StateConflict);
        }

        let affected_execution_id = match mutation.action {
            TaskLifecycleAction::Delay => {
                let Some(until) = mutation.delayed_until else {
                    return Err(StorageError::InvalidData(
                        "delay action requires a target timestamp".to_owned(),
                    ));
                };
                if let Some(execution_id) =
                    delay_pending_execution(&mut transaction, &mutation, until).await?
                {
                    Some(execution_id)
                } else {
                    transaction.rollback().await?;
                    return Ok(TaskLifecycleMutationOutcome::StateConflict);
                }
            }
            TaskLifecycleAction::Cancel => {
                match cancel_pending_execution(&mut transaction, &mutation).await? {
                    PendingExecutionCancellation::Cancelled(execution_id) => Some(execution_id),
                    PendingExecutionCancellation::NoExecution => {
                        if !update_task_state(&mut transaction, &mutation).await? {
                            transaction.rollback().await?;
                            return Ok(TaskLifecycleMutationOutcome::StateConflict);
                        }
                        None
                    }
                    PendingExecutionCancellation::Conflict => {
                        transaction.rollback().await?;
                        return Ok(TaskLifecycleMutationOutcome::StateConflict);
                    }
                }
            }
            TaskLifecycleAction::Approve | TaskLifecycleAction::Ignore => {
                if !update_task_state(&mut transaction, &mutation).await? {
                    transaction.rollback().await?;
                    return Ok(TaskLifecycleMutationOutcome::StateConflict);
                }
                None
            }
        };

        let receipt = TaskLifecycleReceipt {
            id: TaskActionReceiptId::new(),
            owner_id: mutation.owner_id,
            task_id: mutation.task_id,
            action: mutation.action,
            idempotency_key: mutation.idempotency_key.to_owned(),
            delayed_until: mutation.delayed_until,
            result_task_state: mutation.target_task_state,
            affected_execution_id,
            created_at: mutation.at,
        };
        insert_receipt(&mut transaction, &receipt).await?;
        insert_audit(&mut transaction, &mutation, affected_execution_id).await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                mutation.correlation_id,
                DomainEvent::TaskLifecycleActionApplied {
                    task_id: mutation.task_id,
                    action: mutation.action,
                    state: mutation.target_task_state,
                    delayed_until: mutation.delayed_until,
                    affected_execution_id,
                },
                mutation.at,
            ),
        )
        .await?;
        if let Some(execution_id) = affected_execution_id
            && mutation.action == TaskLifecycleAction::Cancel
        {
            enqueue_in_transaction(
                &mut transaction,
                &EventEnvelope::at(
                    mutation.correlation_id,
                    DomainEvent::ExecutionStateChanged {
                        execution_id,
                        state: ExecutionState::Cancelled,
                    },
                    mutation.at,
                ),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(TaskLifecycleMutationOutcome::Applied(receipt))
    }
}

async fn delay_pending_execution(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mutation: &TaskLifecycleMutation<'_>,
    delayed_until: Timestamp,
) -> Result<Option<ExecutionId>, StorageError> {
    let Some(execution_id) = single_active_execution(transaction, mutation.task_id).await? else {
        return Ok(None);
    };
    let timestamp = encode_timestamp(delayed_until);
    let execution_changed =
        sqlx::query("UPDATE executions SET scheduled_at = ? WHERE id = ? AND state = 'scheduled'")
            .bind(&timestamp)
            .bind(execution_id.to_string())
            .execute(&mut **transaction)
            .await?
            .rows_affected();
    let job_changed = sqlx::query(
        "UPDATE scheduled_jobs SET run_at = ?, updated_at = ? \
         WHERE idempotency_key = ? AND state = 'pending'",
    )
    .bind(&timestamp)
    .bind(encode_timestamp(mutation.at))
    .bind(format!("execution:{execution_id}"))
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    if execution_changed == 1 && job_changed == 1 {
        Ok(Some(execution_id))
    } else {
        Ok(None)
    }
}

enum PendingExecutionCancellation {
    Cancelled(ExecutionId),
    NoExecution,
    Conflict,
}

async fn cancel_pending_execution(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mutation: &TaskLifecycleMutation<'_>,
) -> Result<PendingExecutionCancellation, StorageError> {
    let active = active_executions(transaction, mutation.task_id).await?;
    let Some((execution_id, execution_state)) = active.first().copied() else {
        return Ok(PendingExecutionCancellation::NoExecution);
    };
    if active.len() != 1
        || !matches!(
            execution_state,
            ExecutionState::Requested
                | ExecutionState::Scheduled
                | ExecutionState::RetryWaiting
                | ExecutionState::HumanRequired
        )
    {
        return Ok(PendingExecutionCancellation::Conflict);
    }
    let lease_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM execution_leases WHERE execution_id = ?")
            .bind(execution_id.to_string())
            .fetch_one(&mut **transaction)
            .await?;
    if lease_count != 0 {
        return Ok(PendingExecutionCancellation::Conflict);
    }
    let jobs = sqlx::query(
        "SELECT id, state, payload_json FROM scheduled_jobs \
         WHERE state IN ('pending', 'claimed')",
    )
    .fetch_all(&mut **transaction)
    .await?;
    let mut pending_job_ids = Vec::new();
    for row in jobs {
        let kind: ScheduledJobKind = serde_json::from_str(row.try_get("payload_json")?)?;
        if scheduled_execution_id(&kind) != Some(execution_id) {
            continue;
        }
        if row.try_get::<&str, _>("state")? == "claimed" {
            return Ok(PendingExecutionCancellation::Conflict);
        }
        pending_job_ids.push(row.try_get::<String, _>("id")?);
    }
    for job_id in pending_job_ids {
        let changed = sqlx::query(
            "UPDATE scheduled_jobs SET state = 'cancelled', updated_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(encode_timestamp(mutation.at))
        .bind(job_id)
        .execute(&mut **transaction)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(PendingExecutionCancellation::Conflict);
        }
    }
    let execution_changed = sqlx::query(
        "UPDATE executions SET state = 'cancelled', finished_at = ? \
         WHERE id = ? AND state = ?",
    )
    .bind(encode_timestamp(mutation.at))
    .bind(execution_id.to_string())
    .bind(enum_name(execution_state)?)
    .execute(&mut **transaction)
    .await?;
    let task_changed = update_task_state(transaction, mutation).await?;
    if execution_changed.rows_affected() != 1 || !task_changed {
        return Ok(PendingExecutionCancellation::Conflict);
    }
    settle_execution_reservation(
        transaction,
        execution_id,
        ExecutionState::Cancelled,
        mutation.at,
        mutation.correlation_id,
    )
    .await?;
    Ok(PendingExecutionCancellation::Cancelled(execution_id))
}

async fn single_active_execution(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: TaskId,
) -> Result<Option<ExecutionId>, StorageError> {
    let active = active_executions(transaction, task_id).await?;
    if active.len() == 1 && active[0].1 == ExecutionState::Scheduled {
        Ok(Some(active[0].0))
    } else {
        Ok(None)
    }
}

async fn active_executions(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: TaskId,
) -> Result<Vec<(ExecutionId, ExecutionState)>, StorageError> {
    let rows = sqlx::query(
        "SELECT id, state FROM executions WHERE task_id = ? \
         AND state NOT IN ('succeeded', 'failed', 'cancelled') ORDER BY created_at DESC, id DESC",
    )
    .bind(task_id.to_string())
    .fetch_all(&mut **transaction)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                ExecutionId::from_str(row.try_get("id")?)
                    .map_err(|error| StorageError::InvalidData(error.to_string()))?,
                decode_enum(row.try_get("state")?)?,
            ))
        })
        .collect()
}

const fn scheduled_execution_id(kind: &ScheduledJobKind) -> Option<ExecutionId> {
    match kind {
        ScheduledJobKind::Execution { execution_id }
        | ScheduledJobKind::Retry { execution_id, .. }
        | ScheduledJobKind::Recovery { execution_id } => Some(*execution_id),
        ScheduledJobKind::BatchExecution { .. }
        | ScheduledJobKind::AnswerBootstrapHarvest { .. }
        | ScheduledJobKind::Scan { .. }
        | ScheduledJobKind::Notification { .. } => None,
    }
}

async fn update_task_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mutation: &TaskLifecycleMutation<'_>,
) -> Result<bool, StorageError> {
    let changed = sqlx::query(
        "UPDATE tasks SET orchestration_state = ?, updated_at = ? \
         WHERE id = ? AND orchestration_state = ?",
    )
    .bind(enum_name(mutation.target_task_state)?)
    .bind(encode_timestamp(mutation.at))
    .bind(mutation.task_id.to_string())
    .bind(enum_name(mutation.expected_task_state)?)
    .execute(&mut **transaction)
    .await?;
    Ok(changed.rows_affected() == 1)
}

async fn find_receipt<'e, E>(
    executor: E,
    owner_id: UserId,
    idempotency_key: &str,
) -> Result<Option<TaskLifecycleReceipt>, StorageError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "SELECT id, owner_user_id, task_id, action, idempotency_key, delayed_until, \
                result_task_state, affected_execution_id, created_at \
         FROM task_action_receipts WHERE owner_user_id = ? AND idempotency_key = ?",
    )
    .bind(owner_id.to_string())
    .bind(idempotency_key)
    .fetch_optional(executor)
    .await?
    .as_ref()
    .map(decode_receipt)
    .transpose()
}

async fn insert_receipt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    receipt: &TaskLifecycleReceipt,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO task_action_receipts \
         (id, owner_user_id, task_id, action, idempotency_key, delayed_until, result_task_state, \
          affected_execution_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(receipt.id.to_string())
    .bind(receipt.owner_id.to_string())
    .bind(receipt.task_id.to_string())
    .bind(action_name(receipt.action))
    .bind(&receipt.idempotency_key)
    .bind(receipt.delayed_until.map(encode_timestamp))
    .bind(enum_name(receipt.result_task_state)?)
    .bind(receipt.affected_execution_id.map(|id| id.to_string()))
    .bind(encode_timestamp(receipt.created_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mutation: &TaskLifecycleMutation<'_>,
    execution_id: Option<ExecutionId>,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match mutation.actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let metadata = serde_json::json!({
        "action": mutation.action,
        "request_source": mutation.request_source,
        "from_state": mutation.expected_task_state,
        "to_state": mutation.target_task_state,
        "delayed_until": mutation.delayed_until,
        "execution_id": execution_id,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'task', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(mutation.at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(format!("task_{}", action_name(mutation.action)))
    .bind(mutation.task_id.to_string())
    .bind(mutation.correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn decode_receipt(row: &sqlx::sqlite::SqliteRow) -> Result<TaskLifecycleReceipt, StorageError> {
    Ok(TaskLifecycleReceipt {
        id: TaskActionReceiptId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        owner_id: UserId::from_str(row.try_get("owner_user_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        task_id: TaskId::from_str(row.try_get("task_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        action: decode_enum(row.try_get("action")?)?,
        idempotency_key: row.try_get("idempotency_key")?,
        delayed_until: row
            .try_get::<Option<&str>, _>("delayed_until")?
            .map(decode_timestamp)
            .transpose()?,
        result_task_state: decode_enum(row.try_get("result_task_state")?)?,
        affected_execution_id: row
            .try_get::<Option<&str>, _>("affected_execution_id")?
            .map(ExecutionId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    })
}

fn receipt_matches(receipt: &TaskLifecycleReceipt, mutation: &TaskLifecycleMutation<'_>) -> bool {
    receipt.task_id == mutation.task_id
        && receipt.action == mutation.action
        && receipt.delayed_until == mutation.delayed_until
}

fn validate_mutation(mutation: &TaskLifecycleMutation<'_>) -> Result<(), StorageError> {
    validate_token(
        mutation.idempotency_key,
        MAX_IDEMPOTENCY_KEY_BYTES,
        "idempotency key",
    )?;
    validate_token(
        mutation.correlation_id,
        MAX_CORRELATION_ID_BYTES,
        "correlation ID",
    )?;
    if (mutation.action == TaskLifecycleAction::Delay) != mutation.delayed_until.is_some() {
        return Err(StorageError::InvalidData(
            "only delay actions may carry a target timestamp".to_owned(),
        ));
    }
    Ok(())
}

fn validate_token(value: &str, max_bytes: usize, field: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(StorageError::InvalidData(format!("{field} is invalid")))
    } else {
        Ok(())
    }
}

const fn action_name(action: TaskLifecycleAction) -> &'static str {
    match action {
        TaskLifecycleAction::Approve => "approve",
        TaskLifecycleAction::Cancel => "cancel",
        TaskLifecycleAction::Delay => "delay",
        TaskLifecycleAction::Ignore => "ignore",
    }
}

fn enum_name<T: serde::Serialize>(value: T) -> Result<String, StorageError> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(StorageError::InvalidData(
            "enum did not serialize as a string".to_owned(),
        )),
    }
}

fn decode_enum<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(StorageError::Serialization)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, RequestSource, ScheduleId};
    use chrono::Duration;

    use super::*;

    #[tokio::test]
    async fn approve_is_owner_scoped_and_idempotent() {
        let fixture = Fixture::new(OrchestrationState::WaitingApproval).await;
        let repository = SqliteTaskLifecycleRepository::new(fixture.database.clone());
        let mutation = fixture.mutation(TaskLifecycleAction::Approve, None, "approve-1");
        let applied = repository
            .apply_task_lifecycle_mutation(mutation.clone())
            .await
            .unwrap();
        assert!(matches!(applied, TaskLifecycleMutationOutcome::Applied(_)));
        let replay = repository
            .apply_task_lifecycle_mutation(mutation)
            .await
            .unwrap();
        assert!(matches!(replay, TaskLifecycleMutationOutcome::Existing(_)));
        assert_eq!(fixture.task_state().await, "ready");
        assert_eq!(fixture.count("task_action_receipts").await, 1);
        assert_eq!(fixture.count("audit_records").await, 1);
        assert_eq!(fixture.count("event_outbox").await, 1);
    }

    #[tokio::test]
    async fn delay_moves_pending_execution_and_job_together() {
        let fixture = Fixture::new(OrchestrationState::Scheduled).await;
        let execution_id = fixture.insert_scheduled_execution().await;
        let delayed_until = fixture.now + Duration::hours(2);
        let outcome = SqliteTaskLifecycleRepository::new(fixture.database.clone())
            .apply_task_lifecycle_mutation(fixture.mutation(
                TaskLifecycleAction::Delay,
                Some(delayed_until),
                "delay-1",
            ))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            TaskLifecycleMutationOutcome::Applied(TaskLifecycleReceipt {
                affected_execution_id: Some(id),
                ..
            }) if id == execution_id
        ));
        let execution_at: String =
            sqlx::query_scalar("SELECT scheduled_at FROM executions WHERE id = ?")
                .bind(execution_id.to_string())
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        let job_at: String =
            sqlx::query_scalar("SELECT run_at FROM scheduled_jobs WHERE idempotency_key = ?")
                .bind(format!("execution:{execution_id}"))
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(execution_at, encode_timestamp(delayed_until));
        assert_eq!(job_at, execution_at);
        assert_eq!(fixture.task_state().await, "scheduled");
    }

    #[tokio::test]
    async fn cancel_stops_pending_execution_job_and_releases_credits_atomically() {
        let fixture = Fixture::new(OrchestrationState::Scheduled).await;
        let execution_id = fixture.insert_scheduled_execution().await;
        fixture.insert_credit_reservation(execution_id).await;
        let outcome = SqliteTaskLifecycleRepository::new(fixture.database.clone())
            .apply_task_lifecycle_mutation(fixture.mutation(
                TaskLifecycleAction::Cancel,
                None,
                "cancel-1",
            ))
            .await
            .unwrap();
        assert!(matches!(outcome, TaskLifecycleMutationOutcome::Applied(_)));
        assert_eq!(fixture.task_state().await, "cancelled");
        assert_eq!(
            fixture.scalar("SELECT state FROM executions").await,
            "cancelled"
        );
        assert_eq!(
            fixture.scalar("SELECT state FROM scheduled_jobs").await,
            "cancelled"
        );
        assert_eq!(
            fixture
                .scalar("SELECT state FROM credit_reservations")
                .await,
            "released"
        );
        let balances: (i64, i64) =
            sqlx::query_as("SELECT available, reserved FROM credit_accounts")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(balances, (10, 0));
    }

    #[tokio::test]
    async fn claimed_execution_cannot_be_cancelled_or_delayed() {
        let fixture = Fixture::new(OrchestrationState::Scheduled).await;
        let execution_id = fixture.insert_scheduled_execution().await;
        sqlx::query(
            "UPDATE scheduled_jobs SET state = 'claimed', worker_id = 'worker', \
             lease_expires_at = ? WHERE idempotency_key = ?",
        )
        .bind(encode_timestamp(fixture.now + Duration::minutes(5)))
        .bind(format!("execution:{execution_id}"))
        .execute(fixture.database.pool())
        .await
        .unwrap();
        let outcome = SqliteTaskLifecycleRepository::new(fixture.database.clone())
            .apply_task_lifecycle_mutation(fixture.mutation(
                TaskLifecycleAction::Cancel,
                None,
                "cancel-claimed",
            ))
            .await
            .unwrap();
        assert_eq!(outcome, TaskLifecycleMutationOutcome::StateConflict);
        assert_eq!(fixture.task_state().await, "scheduled");
        assert_eq!(fixture.count("task_action_receipts").await, 0);
    }

    struct Fixture {
        database: Database,
        owner_id: UserId,
        task_id: TaskId,
        now: Timestamp,
        state: OrchestrationState,
    }

    impl Fixture {
        async fn new(state: OrchestrationState) -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner_id = UserId::new();
            let account_id = ProviderAccountId::new();
            let task_id = TaskId::new();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
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
                 VALUES (?, ?, 'provider', 'Provider', '{\"state\":\"idle\"}', ?, ?)",
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
                 VALUES (?, ?, 'remote', 'fingerprint', 'resource', 'routine', 'Task', 'pending', ?, ?, ?, \
                         '[\"resource_execution\"]')",
            )
            .bind(task_id.to_string())
            .bind(account_id.to_string())
            .bind(enum_name(state).unwrap())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            Self {
                database,
                owner_id,
                task_id,
                now,
                state,
            }
        }

        fn mutation(
            &self,
            action: TaskLifecycleAction,
            delayed_until: Option<Timestamp>,
            idempotency_key: &'static str,
        ) -> TaskLifecycleMutation<'static> {
            let target_task_state = match action {
                TaskLifecycleAction::Approve => OrchestrationState::Ready,
                TaskLifecycleAction::Cancel => OrchestrationState::Cancelled,
                TaskLifecycleAction::Delay => OrchestrationState::Scheduled,
                TaskLifecycleAction::Ignore => OrchestrationState::Ignored,
            };
            TaskLifecycleMutation {
                owner_id: self.owner_id,
                task_id: self.task_id,
                action,
                expected_task_state: self.state,
                target_task_state,
                delayed_until,
                request_source: RequestSource::WebUi,
                actor: AuditActor::User(self.owner_id),
                idempotency_key,
                correlation_id: "test-request",
                at: self.now,
            }
        }

        async fn insert_scheduled_execution(&self) -> ExecutionId {
            let execution_id = ExecutionId::new();
            let timestamp = encode_timestamp(self.now);
            sqlx::query(
                "INSERT INTO executions \
                 (id, task_id, requested_by, request_source, state, scheduled_at, created_at, \
                  idempotency_scope, idempotency_key) \
                 VALUES (?, ?, ?, 'web_ui', 'scheduled', ?, ?, ?, 'execute-1')",
            )
            .bind(execution_id.to_string())
            .bind(self.task_id.to_string())
            .bind(self.owner_id.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .bind(format!("user:{}", self.owner_id))
            .execute(self.database.pool())
            .await
            .unwrap();
            let kind = ScheduledJobKind::Execution { execution_id };
            sqlx::query(
                "INSERT INTO scheduled_jobs \
                 (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, created_at, updated_at) \
                 VALUES (?, 'execution', ?, ?, 'pending', 0, ?, ?, ?)",
            )
            .bind(ScheduleId::new().to_string())
            .bind(serde_json::to_string(&kind).unwrap())
            .bind(&timestamp)
            .bind(format!("execution:{execution_id}"))
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            execution_id
        }

        async fn insert_credit_reservation(&self, execution_id: ExecutionId) {
            let quote_id = asterism_domain::PriceQuoteId::new();
            let reservation_id = asterism_domain::CreditReservationId::new();
            let timestamp = encode_timestamp(self.now);
            sqlx::query(
                "INSERT INTO price_quotes (id, task_id, amount, pricing_revision, reason, created_at) \
                 VALUES (?, ?, 10, 'test', 'test quote', ?)",
            )
            .bind(quote_id.to_string())
            .bind(self.task_id.to_string())
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query("UPDATE executions SET quote_id = ? WHERE id = ?")
                .bind(quote_id.to_string())
                .bind(execution_id.to_string())
                .execute(self.database.pool())
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
                 VALUES (?, 0, 10, ?)",
            )
            .bind(self.owner_id.to_string())
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO credit_reservations \
                 (id, user_id, quote_id, execution_id, amount, state, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 10, 'reserved', ?, ?)",
            )
            .bind(reservation_id.to_string())
            .bind(self.owner_id.to_string())
            .bind(quote_id.to_string())
            .bind(execution_id.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
        }

        async fn task_state(&self) -> String {
            self.scalar("SELECT orchestration_state FROM tasks").await
        }

        async fn scalar(&self, query: &str) -> String {
            sqlx::query_scalar(query)
                .fetch_one(self.database.pool())
                .await
                .unwrap()
        }

        async fn count(&self, table: &str) -> i64 {
            let query = format!("SELECT COUNT(*) FROM {table}");
            sqlx::query_scalar(&query)
                .fetch_one(self.database.pool())
                .await
                .unwrap()
        }
    }
}
