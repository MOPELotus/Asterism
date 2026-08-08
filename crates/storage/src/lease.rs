use std::str::FromStr;

use asterism_domain::{ExecutionId, ExecutionLease, TaskId, Timestamp};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{Database, ExecutionLeaseRepository, StorageError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseAcquireOutcome {
    Acquired(ExecutionLease),
    Conflict(ExecutionLease),
}

#[derive(Clone, Debug)]
pub struct SqliteExecutionLeaseRepository {
    database: Database,
}

impl SqliteExecutionLeaseRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    async fn find(&self, task_id: TaskId) -> Result<Option<ExecutionLease>, StorageError> {
        let row = sqlx::query(
            "SELECT task_id, execution_id, worker_id, expires_at \
             FROM execution_leases WHERE task_id = ?",
        )
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.map(|row| decode_lease(&row)).transpose()
    }
}

#[async_trait]
impl ExecutionLeaseRepository for SqliteExecutionLeaseRepository {
    async fn try_acquire(
        &self,
        lease: &ExecutionLease,
        now: Timestamp,
    ) -> Result<LeaseAcquireOutcome, StorageError> {
        if lease.expires_at <= now {
            return Err(StorageError::InvalidLeaseExpiry);
        }

        let result = sqlx::query(
            "INSERT INTO execution_leases (task_id, execution_id, worker_id, expires_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(task_id) DO UPDATE SET \
                 execution_id = excluded.execution_id, \
                 worker_id = excluded.worker_id, \
                 expires_at = excluded.expires_at \
             WHERE execution_leases.expires_at <= ?",
        )
        .bind(lease.task_id.to_string())
        .bind(lease.execution_id.to_string())
        .bind(&lease.worker_id)
        .bind(encode_timestamp(lease.expires_at))
        .bind(encode_timestamp(now))
        .execute(self.database.pool())
        .await?;

        if result.rows_affected() == 1 {
            Ok(LeaseAcquireOutcome::Acquired(lease.clone()))
        } else {
            let conflict = self.find(lease.task_id).await?.ok_or_else(|| {
                StorageError::InvalidData(
                    "lease conflict was reported without a persisted lease".to_owned(),
                )
            })?;
            Ok(LeaseAcquireOutcome::Conflict(conflict))
        }
    }

    async fn renew(
        &self,
        task_id: TaskId,
        execution_id: ExecutionId,
        worker_id: &str,
        now: Timestamp,
        new_expires_at: Timestamp,
    ) -> Result<ExecutionLease, StorageError> {
        if new_expires_at <= now {
            return Err(StorageError::InvalidLeaseExpiry);
        }
        let result = sqlx::query(
            "UPDATE execution_leases SET expires_at = ? \
             WHERE task_id = ? AND execution_id = ? AND worker_id = ? AND expires_at > ?",
        )
        .bind(encode_timestamp(new_expires_at))
        .bind(task_id.to_string())
        .bind(execution_id.to_string())
        .bind(worker_id)
        .bind(encode_timestamp(now))
        .execute(self.database.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(StorageError::LeaseLost);
        }
        Ok(ExecutionLease {
            task_id,
            execution_id,
            worker_id: worker_id.to_owned(),
            expires_at: new_expires_at,
        })
    }

    async fn release(
        &self,
        task_id: TaskId,
        execution_id: ExecutionId,
        worker_id: &str,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "DELETE FROM execution_leases \
             WHERE task_id = ? AND execution_id = ? AND worker_id = ?",
        )
        .bind(task_id.to_string())
        .bind(execution_id.to_string())
        .bind(worker_id)
        .execute(self.database.pool())
        .await?;
        Ok(result.rows_affected() == 1)
    }
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn decode_lease(row: &sqlx::sqlite::SqliteRow) -> Result<ExecutionLease, StorageError> {
    let task_id = TaskId::from_str(row.try_get::<&str, _>("task_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let execution_id = ExecutionId::from_str(row.try_get::<&str, _>("execution_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let expires_at = decode_timestamp(row.try_get::<&str, _>("expires_at")?)?;
    Ok(ExecutionLease {
        task_id,
        execution_id,
        worker_id: row.try_get("worker_id")?,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ExecutionId, ExecutionLease, TaskId};
    use chrono::{Duration, Utc};

    use super::*;

    async fn repository_with_task_and_executions() -> (
        SqliteExecutionLeaseRepository,
        TaskId,
        ExecutionId,
        ExecutionId,
    ) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let user_id = asterism_domain::UserId::new();
        let account_id = asterism_domain::ProviderAccountId::new();
        let task_id = TaskId::new();
        let first_execution = ExecutionId::new();
        let second_execution = ExecutionId::new();
        let now = encode_timestamp(Utc::now());

        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'test', 'argon2id-placeholder', 'active', '[]', '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'test', 'Test', '{}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(user_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'remote', 'fingerprint', 'work', 'routine', 'Task', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        for execution_id in [first_execution, second_execution] {
            sqlx::query(
                "INSERT INTO executions \
                 (id, task_id, request_source, state, created_at) \
                 VALUES (?, ?, 'system', 'requested', ?)",
            )
            .bind(execution_id.to_string())
            .bind(task_id.to_string())
            .bind(&now)
            .execute(database.pool())
            .await
            .unwrap();
        }

        (
            SqliteExecutionLeaseRepository::new(database),
            task_id,
            first_execution,
            second_execution,
        )
    }

    #[tokio::test]
    async fn only_one_execution_can_acquire_a_live_task_lease() {
        let (repository, task_id, first_execution, second_execution) =
            repository_with_task_and_executions().await;
        let now = Utc::now();
        let first = ExecutionLease {
            task_id,
            execution_id: first_execution,
            worker_id: "worker-a".to_owned(),
            expires_at: now + Duration::minutes(1),
        };
        let second = ExecutionLease {
            task_id,
            execution_id: second_execution,
            worker_id: "worker-b".to_owned(),
            expires_at: now + Duration::minutes(1),
        };

        let (first_result, second_result) = tokio::join!(
            repository.try_acquire(&first, now),
            repository.try_acquire(&second, now)
        );
        let outcomes = [first_result.unwrap(), second_result.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, LeaseAcquireOutcome::Acquired(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, LeaseAcquireOutcome::Conflict(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn expired_lease_can_be_replaced_but_not_renewed() {
        let (repository, task_id, first_execution, second_execution) =
            repository_with_task_and_executions().await;
        let start = Utc::now();
        let first = ExecutionLease {
            task_id,
            execution_id: first_execution,
            worker_id: "worker-a".to_owned(),
            expires_at: start + Duration::seconds(1),
        };
        assert!(matches!(
            repository.try_acquire(&first, start).await.unwrap(),
            LeaseAcquireOutcome::Acquired(_)
        ));

        let after_expiry = start + Duration::seconds(2);
        assert!(matches!(
            repository
                .renew(
                    task_id,
                    first_execution,
                    "worker-a",
                    after_expiry,
                    after_expiry + Duration::minutes(1)
                )
                .await,
            Err(StorageError::LeaseLost)
        ));

        let replacement = ExecutionLease {
            task_id,
            execution_id: second_execution,
            worker_id: "worker-b".to_owned(),
            expires_at: after_expiry + Duration::minutes(1),
        };
        assert!(matches!(
            repository
                .try_acquire(&replacement, after_expiry)
                .await
                .unwrap(),
            LeaseAcquireOutcome::Acquired(_)
        ));
        assert!(
            repository
                .release(task_id, second_execution, "worker-b")
                .await
                .unwrap()
        );
    }
}
