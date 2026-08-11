use asterism_domain::{EventId, Timestamp};
use asterism_events::EventEnvelope;
use async_trait::async_trait;
use chrono::SecondsFormat;
use sqlx::{Row, Sqlite, Transaction};

use crate::{Database, OutboxRepository, StorageError};

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxRecord {
    pub event: EventEnvelope,
    pub publish_attempts: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureDisposition {
    RetryPending,
    DeadLetter,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutboxHealth {
    pub pending: u64,
    pub dead_letter: u64,
}

#[derive(Clone, Debug)]
pub struct SqliteOutboxRepository {
    database: Database,
}

impl SqliteOutboxRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

impl Database {
    /// Counts pending and dead-letter outbox records for health reporting.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the outbox cannot be queried or a stored
    /// count is invalid.
    pub async fn outbox_health(&self) -> Result<OutboxHealth, StorageError> {
        let row = sqlx::query(
            "SELECT \
                 COALESCE(SUM(CASE WHEN state = 'pending' THEN 1 ELSE 0 END), 0) AS pending, \
                 COALESCE(SUM(CASE WHEN state = 'dead_letter' THEN 1 ELSE 0 END), 0) AS dead_letter \
             FROM event_outbox",
        )
        .fetch_one(self.pool())
        .await?;
        let pending = row.try_get::<i64, _>("pending")?;
        let dead_letter = row.try_get::<i64, _>("dead_letter")?;
        Ok(OutboxHealth {
            pending: u64::try_from(pending)
                .map_err(|_| StorageError::InvalidData("negative outbox count".to_owned()))?,
            dead_letter: u64::try_from(dead_letter)
                .map_err(|_| StorageError::InvalidData("negative outbox count".to_owned()))?,
        })
    }
}

#[async_trait]
impl OutboxRepository for SqliteOutboxRepository {
    async fn enqueue(&self, event: &EventEnvelope) -> Result<(), StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        enqueue_in_transaction(&mut transaction, event).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn claim_batch(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<OutboxRecord>, StorageError> {
        if limit == 0 || lease_expires_at <= now {
            return Err(StorageError::InvalidOutboxClaim);
        }
        let now_text = encode_timestamp(now);
        let lease_text = encode_timestamp(lease_expires_at);
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query(
            "UPDATE event_outbox SET worker_id = ?, lock_expires_at = ? \
             WHERE id IN ( \
                 SELECT id FROM event_outbox \
                 WHERE state = 'pending' \
                   AND (lock_expires_at IS NULL OR lock_expires_at <= ?) \
                 ORDER BY occurred_at, id LIMIT ? \
             )",
        )
        .bind(worker_id)
        .bind(&lease_text)
        .bind(&now_text)
        .bind(limit)
        .execute(&mut *transaction)
        .await?;
        let rows = sqlx::query(
            "SELECT payload_json, publish_attempts FROM event_outbox \
             WHERE state = 'pending' AND worker_id = ? AND lock_expires_at = ? \
             ORDER BY occurred_at, id",
        )
        .bind(worker_id)
        .bind(&lease_text)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;

        rows.into_iter()
            .map(|row| -> Result<OutboxRecord, StorageError> {
                let attempts = row.try_get::<i64, _>("publish_attempts")?;
                Ok(OutboxRecord {
                    event: serde_json::from_str(row.try_get("payload_json")?)?,
                    publish_attempts: u32::try_from(attempts).map_err(|_| {
                        StorageError::InvalidData(
                            "outbox publish attempt count does not fit u32".to_owned(),
                        )
                    })?,
                })
            })
            .collect()
    }

    async fn mark_delivered(
        &self,
        event_id: EventId,
        worker_id: &str,
        delivered_at: Timestamp,
    ) -> Result<(), StorageError> {
        let result = sqlx::query(
            "UPDATE event_outbox \
             SET state = 'delivered', published_at = ?, worker_id = NULL, lock_expires_at = NULL \
             WHERE id = ? AND state = 'pending' AND worker_id = ?",
        )
        .bind(encode_timestamp(delivered_at))
        .bind(event_id.to_string())
        .bind(worker_id)
        .execute(self.database.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(StorageError::OutboxClaimLost)
        }
    }

    async fn mark_failed(
        &self,
        event_id: EventId,
        worker_id: &str,
        error_sanitized: &str,
        max_attempts: u32,
    ) -> Result<FailureDisposition, StorageError> {
        if max_attempts == 0 {
            return Err(StorageError::InvalidOutboxClaim);
        }
        let result = sqlx::query(
            "UPDATE event_outbox SET \
                 publish_attempts = publish_attempts + 1, \
                 state = CASE WHEN publish_attempts + 1 >= ? THEN 'dead_letter' ELSE 'pending' END, \
                 last_error_sanitized = ?, worker_id = NULL, lock_expires_at = NULL \
             WHERE id = ? AND state = 'pending' AND worker_id = ? \
             RETURNING state",
        )
        .bind(max_attempts)
        .bind(error_sanitized)
        .bind(event_id.to_string())
        .bind(worker_id)
        .fetch_optional(self.database.pool())
        .await?;
        match result {
            Some(row) if row.try_get::<&str, _>("state")? == "dead_letter" => {
                Ok(FailureDisposition::DeadLetter)
            }
            Some(_) => Ok(FailureDisposition::RetryPending),
            None => Err(StorageError::OutboxClaimLost),
        }
    }
}

pub(crate) async fn enqueue_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    event: &EventEnvelope,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO event_outbox \
         (id, event_type, payload_json, correlation_id, occurred_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(event.id.to_string())
    .bind(event_type(event))
    .bind(serde_json::to_string(event)?)
    .bind(&event.correlation_id)
    .bind(encode_timestamp(event.occurred_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn event_type(event: &EventEnvelope) -> &'static str {
    use asterism_events::DomainEvent::{
        AuthStateChanged, CreditCommitted, CreditGranted, CreditReleased, CreditReserved,
        ExecutionLogged, ExecutionProgressed, ExecutionRecoveryRequired, ExecutionStateChanged,
        HumanRequired, TaskChanged, TaskLifecycleActionApplied,
    };
    match &event.event {
        TaskChanged { .. } => "task_changed",
        TaskLifecycleActionApplied { .. } => "task_lifecycle_action_applied",
        ExecutionStateChanged { .. } => "execution_state_changed",
        ExecutionProgressed(_) => "execution_progressed",
        ExecutionLogged(_) => "execution_logged",
        AuthStateChanged { .. } => "auth_state_changed",
        HumanRequired { .. } => "human_required",
        CreditReserved { .. } => "credit_reserved",
        CreditGranted { .. } => "credit_granted",
        CreditCommitted { .. } => "credit_committed",
        CreditReleased { .. } => "credit_released",
        ExecutionRecoveryRequired { .. } => "execution_recovery_required",
    }
}

#[cfg(test)]
mod tests {
    use asterism_domain::{TaskDiffKind, TaskId};
    use asterism_events::DomainEvent;
    use chrono::{Duration, Utc};

    use super::*;

    #[tokio::test]
    async fn claim_ownership_and_dead_letter_threshold_are_enforced() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteOutboxRepository::new(database);
        let now = Utc::now();
        let event = EventEnvelope::at(
            "test",
            DomainEvent::TaskChanged {
                task_id: TaskId::new(),
                changes: vec![TaskDiffKind::Created],
            },
            now,
        );
        repository.enqueue(&event).await.unwrap();
        let claimed = repository
            .claim_batch("worker-a", now, now + Duration::minutes(1), 10)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].event, event);
        assert!(matches!(
            repository.mark_delivered(event.id, "worker-b", now).await,
            Err(StorageError::OutboxClaimLost)
        ));
        assert_eq!(
            repository
                .mark_failed(event.id, "worker-a", "temporary sink error", 1)
                .await
                .unwrap(),
            FailureDisposition::DeadLetter
        );
        assert!(
            repository
                .claim_batch(
                    "worker-b",
                    now + Duration::minutes(2),
                    now + Duration::minutes(3),
                    10
                )
                .await
                .unwrap()
                .is_empty()
        );
    }
}
