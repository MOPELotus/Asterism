use std::str::FromStr;

use asterism_domain::{ExecutionId, TaskId, Timestamp};
use asterism_events::{DomainEvent, EventEnvelope};
use chrono::SecondsFormat;
use sqlx::Row;

use crate::{Database, StorageError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryReport {
    pub executions_marked_recovering: u64,
    pub expired_execution_leases_removed: u64,
    pub scheduler_claims_requeued: u64,
}

impl Database {
    /// Moves abandoned work into explicit recovery states without assuming the
    /// remote provider operation failed.
    ///
    /// Credit reservations are deliberately left untouched until provider-side
    /// verification determines whether to commit, retry/release, or require a
    /// human decision.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the recovery transaction cannot complete.
    pub async fn recover_stale_work(&self, now: Timestamp) -> Result<RecoveryReport, StorageError> {
        let now_text = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        let mut transaction = self.pool().begin().await?;

        let stale_executions = sqlx::query(
            "SELECT e.id, e.task_id \
             FROM executions e \
             LEFT JOIN execution_leases l ON l.execution_id = e.id \
             WHERE e.state = 'running' AND (l.execution_id IS NULL OR l.expires_at <= ?)",
        )
        .bind(&now_text)
        .fetch_all(&mut *transaction)
        .await?;

        for row in &stale_executions {
            let execution_id: String = row.try_get("id")?;
            let task_id: String = row.try_get("task_id")?;
            sqlx::query(
                "UPDATE executions SET state = 'recovering' WHERE id = ? AND state = 'running'",
            )
            .bind(&execution_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE tasks SET orchestration_state = 'recovering', updated_at = ? WHERE id = ?",
            )
            .bind(&now_text)
            .bind(&task_id)
            .execute(&mut *transaction)
            .await?;
            let typed_execution_id = ExecutionId::from_str(&execution_id)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let typed_task_id = TaskId::from_str(&task_id)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let envelope = EventEnvelope::at(
                format!("recovery:{execution_id}"),
                DomainEvent::ExecutionRecoveryRequired {
                    execution_id: typed_execution_id,
                    task_id: typed_task_id,
                },
                now,
            );
            sqlx::query(
                "INSERT INTO event_outbox \
                 (id, event_type, payload_json, correlation_id, occurred_at) \
                 VALUES (?, 'execution_recovery_required', ?, ?, ?)",
            )
            .bind(envelope.id.to_string())
            .bind(serde_json::to_string(&envelope)?)
            .bind(&envelope.correlation_id)
            .bind(&now_text)
            .execute(&mut *transaction)
            .await?;
        }

        let expired_execution_leases_removed =
            sqlx::query("DELETE FROM execution_leases WHERE expires_at <= ?")
                .bind(&now_text)
                .execute(&mut *transaction)
                .await?
                .rows_affected();

        let scheduler_claims_requeued = sqlx::query(
            "UPDATE scheduled_jobs \
             SET state = 'pending', worker_id = NULL, lease_expires_at = NULL, updated_at = ? \
             WHERE state = 'claimed' AND lease_expires_at <= ?",
        )
        .bind(&now_text)
        .bind(&now_text)
        .execute(&mut *transaction)
        .await?
        .rows_affected();

        transaction.commit().await?;
        Ok(RecoveryReport {
            executions_marked_recovering: u64::try_from(stale_executions.len()).map_err(|_| {
                StorageError::InvalidData("recovery candidate count overflowed u64".to_owned())
            })?,
            expired_execution_leases_removed,
            scheduler_claims_requeued,
        })
    }
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ExecutionId, ProviderAccountId, TaskId, UserId};
    use chrono::{Duration, Utc};

    use super::*;

    struct ScenarioIds {
        execution_id: ExecutionId,
        reservation_id: asterism_domain::CreditReservationId,
    }

    async fn seed_recovery_scenario(database: &Database, earlier: &str) -> ScenarioIds {
        let user_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let execution_id = ExecutionId::new();
        let quote_id = asterism_domain::PriceQuoteId::new();
        let reservation_id = asterism_domain::CreditReservationId::new();

        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'recovery', 'hash', 'active', '[]', '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(earlier)
        .bind(earlier)
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
        .bind(earlier)
        .bind(earlier)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'remote', 'fingerprint', 'work', 'routine', 'Task', 'pending', 'running', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(earlier)
        .bind(earlier)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO price_quotes (id, task_id, amount, pricing_revision, reason, created_at) \
             VALUES (?, ?, 10, 'test', 'test', ?)",
        )
        .bind(quote_id.to_string())
        .bind(task_id.to_string())
        .bind(earlier)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_by, request_source, quote_id, state, started_at, created_at) \
             VALUES (?, ?, ?, 'system', ?, 'running', ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(user_id.to_string())
        .bind(quote_id.to_string())
        .bind(earlier)
        .bind(earlier)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, 90, 10, ?)",
        )
        .bind(user_id.to_string())
        .bind(earlier)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO credit_reservations \
             (id, user_id, quote_id, execution_id, amount, state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 10, 'reserved', ?, ?)",
        )
        .bind(reservation_id.to_string())
        .bind(user_id.to_string())
        .bind(quote_id.to_string())
        .bind(execution_id.to_string())
        .bind(earlier)
        .bind(earlier)
        .execute(database.pool())
        .await
        .unwrap();

        ScenarioIds {
            execution_id,
            reservation_id,
        }
    }

    #[tokio::test]
    async fn stale_running_execution_enters_recovery_without_touching_credit() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let earlier = (now - Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Nanos, true);
        let scenario = seed_recovery_scenario(&database, &earlier).await;

        let report = database.recover_stale_work(now).await.unwrap();
        assert_eq!(report.executions_marked_recovering, 1);
        let execution_state: String =
            sqlx::query_scalar("SELECT state FROM executions WHERE id = ?")
                .bind(scenario.execution_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        let reservation: (String, i64) =
            sqlx::query_as("SELECT state, amount FROM credit_reservations WHERE id = ?")
                .bind(scenario.reservation_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        let outbox_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox WHERE event_type = 'execution_recovery_required'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(execution_state, "recovering");
        assert_eq!(reservation, ("reserved".to_owned(), 10));
        assert_eq!(outbox_count, 1);

        let second = database.recover_stale_work(now).await.unwrap();
        assert_eq!(second.executions_marked_recovering, 0);
    }
}
