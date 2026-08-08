use asterism_domain::{ScheduleId, Timestamp};
use asterism_scheduler::{ScheduledJob, ScheduledJobKind, ScheduledJobState};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{Database, SchedulerRepository, StorageError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobFailureDisposition {
    RetryPending,
    DeadLetter,
}

#[derive(Clone, Debug)]
pub struct SqliteSchedulerRepository {
    database: Database,
}

impl SqliteSchedulerRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl SchedulerRepository for SqliteSchedulerRepository {
    async fn enqueue(&self, job: &ScheduledJob) -> Result<(), StorageError> {
        if job.state != ScheduledJobState::Pending {
            return Err(StorageError::InvalidData(
                "new scheduler job must be pending".to_owned(),
            ));
        }
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
        )
        .bind(job.id.to_string())
        .bind(job_kind_name(&job.kind))
        .bind(serde_json::to_string(&job.kind)?)
        .bind(encode_timestamp(job.run_at))
        .bind(job.attempts)
        .bind(&job.idempotency_key)
        .bind(encode_timestamp(job.created_at))
        .bind(encode_timestamp(job.updated_at))
        .execute(self.database.pool())
        .await?;
        Ok(())
    }

    async fn claim_due(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<ScheduledJob>, StorageError> {
        if worker_id.is_empty() || limit == 0 || lease_expires_at <= now {
            return Err(StorageError::InvalidSchedulerClaim);
        }
        let now_text = encode_timestamp(now);
        let lease_text = encode_timestamp(lease_expires_at);
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query(
            "UPDATE scheduled_jobs \
             SET state = 'claimed', worker_id = ?, lease_expires_at = ?, updated_at = ? \
             WHERE id IN ( \
                 SELECT id FROM scheduled_jobs \
                 WHERE state = 'pending' AND run_at <= ? \
                 ORDER BY run_at, id LIMIT ? \
             )",
        )
        .bind(worker_id)
        .bind(&lease_text)
        .bind(&now_text)
        .bind(&now_text)
        .bind(limit)
        .execute(&mut *transaction)
        .await?;
        let rows = sqlx::query(
            "SELECT id, payload_json, run_at, attempts, idempotency_key, created_at, updated_at \
             FROM scheduled_jobs \
             WHERE state = 'claimed' AND worker_id = ? AND lease_expires_at = ? \
             ORDER BY run_at, id",
        )
        .bind(worker_id)
        .bind(&lease_text)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        rows.into_iter()
            .map(|row| decode_claimed_job(&row, worker_id, lease_expires_at))
            .collect()
    }

    async fn complete(
        &self,
        job_id: ScheduleId,
        worker_id: &str,
        at: Timestamp,
    ) -> Result<(), StorageError> {
        let result = sqlx::query(
            "UPDATE scheduled_jobs SET \
                 state = 'completed', worker_id = NULL, lease_expires_at = NULL, updated_at = ? \
             WHERE id = ? AND state = 'claimed' AND worker_id = ?",
        )
        .bind(encode_timestamp(at))
        .bind(job_id.to_string())
        .bind(worker_id)
        .execute(self.database.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(StorageError::SchedulerClaimLost)
        }
    }

    async fn fail(
        &self,
        job_id: ScheduleId,
        worker_id: &str,
        error_sanitized: &str,
        retry_at: Option<Timestamp>,
        at: Timestamp,
    ) -> Result<JobFailureDisposition, StorageError> {
        let (state, run_at, disposition) = if let Some(retry_at) = retry_at {
            (
                "pending",
                encode_timestamp(retry_at),
                JobFailureDisposition::RetryPending,
            )
        } else {
            (
                "dead_letter",
                encode_timestamp(at),
                JobFailureDisposition::DeadLetter,
            )
        };
        let result = sqlx::query(
            "UPDATE scheduled_jobs SET \
                 state = ?, run_at = ?, attempts = attempts + 1, last_error_sanitized = ?, \
                 worker_id = NULL, lease_expires_at = NULL, updated_at = ? \
             WHERE id = ? AND state = 'claimed' AND worker_id = ?",
        )
        .bind(state)
        .bind(run_at)
        .bind(error_sanitized)
        .bind(encode_timestamp(at))
        .bind(job_id.to_string())
        .bind(worker_id)
        .execute(self.database.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(disposition)
        } else {
            Err(StorageError::SchedulerClaimLost)
        }
    }
}

fn decode_claimed_job(
    row: &sqlx::sqlite::SqliteRow,
    worker_id: &str,
    lease_expires_at: Timestamp,
) -> Result<ScheduledJob, StorageError> {
    Ok(ScheduledJob {
        id: row
            .try_get::<&str, _>("id")?
            .parse()
            .map_err(|error: uuid::Error| StorageError::InvalidData(error.to_string()))?,
        kind: serde_json::from_str(row.try_get("payload_json")?)?,
        run_at: decode_timestamp(row.try_get("run_at")?)?,
        state: ScheduledJobState::Claimed {
            worker_id: worker_id.to_owned(),
            lease_expires_at,
        },
        attempts: u32::try_from(row.try_get::<i64, _>("attempts")?).map_err(|_| {
            StorageError::InvalidData("scheduler attempt count does not fit u32".to_owned())
        })?,
        idempotency_key: row.try_get("idempotency_key")?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    })
}

fn job_kind_name(kind: &ScheduledJobKind) -> &'static str {
    match kind {
        ScheduledJobKind::Scan { .. } => "scan",
        ScheduledJobKind::Execution { .. } => "execution",
        ScheduledJobKind::Retry { .. } => "retry",
        ScheduledJobKind::Notification { .. } => "notification",
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

#[cfg(test)]
mod tests {
    use asterism_domain::{ExecutionId, ScheduleId};
    use chrono::{Duration, Utc};

    use super::*;

    #[tokio::test]
    async fn due_job_has_one_claim_owner_and_can_be_retried() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteSchedulerRepository::new(database);
        let now = Utc::now();
        let job = ScheduledJob {
            id: ScheduleId::new(),
            kind: ScheduledJobKind::Execution {
                execution_id: ExecutionId::new(),
            },
            run_at: now,
            state: ScheduledJobState::Pending,
            attempts: 0,
            idempotency_key: "execution:test".to_owned(),
            created_at: now,
            updated_at: now,
        };
        repository.enqueue(&job).await.unwrap();
        let expiry = now + Duration::minutes(1);
        let (first, second) = tokio::join!(
            repository.claim_due("worker-a", now, expiry, 1),
            repository.claim_due("worker-b", now, expiry, 1)
        );
        let claims = [first.unwrap(), second.unwrap()];
        assert_eq!(claims.iter().map(Vec::len).sum::<usize>(), 1);
        let owner = if claims[0].is_empty() {
            "worker-b"
        } else {
            "worker-a"
        };
        assert!(matches!(
            repository.complete(job.id, "not-owner", now).await,
            Err(StorageError::SchedulerClaimLost)
        ));
        assert_eq!(
            repository
                .fail(
                    job.id,
                    owner,
                    "temporary",
                    Some(now + Duration::minutes(2)),
                    now
                )
                .await
                .unwrap(),
            JobFailureDisposition::RetryPending
        );
        assert!(
            repository
                .claim_due(
                    "worker-c",
                    now + Duration::minutes(1),
                    now + Duration::minutes(2),
                    1
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repository
                .claim_due(
                    "worker-c",
                    now + Duration::minutes(2),
                    now + Duration::minutes(3),
                    1
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
