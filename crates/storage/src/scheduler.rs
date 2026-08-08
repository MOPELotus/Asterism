use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, ProviderAccountId, ScheduleId, Timestamp, UserId,
};
use asterism_scheduler::{ScanSchedule, ScheduledJob, ScheduledJobKind, ScheduledJobState};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction};

use crate::{Database, ScanScheduleRepository, SchedulerRepository, StorageError};

const MAX_MATERIALIZED_SCAN_JOBS: u32 = 1_000;

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

    async fn claim_due_internal(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
        scan_only: bool,
    ) -> Result<Vec<ScheduledJob>, StorageError> {
        if worker_id.is_empty() || limit == 0 || lease_expires_at <= now {
            return Err(StorageError::InvalidSchedulerClaim);
        }
        let now_text = encode_timestamp(now);
        let lease_text = encode_timestamp(lease_expires_at);
        let statement = if scan_only {
            "UPDATE scheduled_jobs \
             SET state = 'claimed', worker_id = ?, lease_expires_at = ?, updated_at = ? \
             WHERE id IN ( \
                 SELECT id FROM scheduled_jobs \
                 WHERE state = 'pending' AND job_kind = 'scan' AND run_at <= ? \
                 ORDER BY run_at, id LIMIT ? \
             ) \
             RETURNING id, payload_json, run_at, attempts, idempotency_key, created_at, updated_at"
        } else {
            "UPDATE scheduled_jobs \
             SET state = 'claimed', worker_id = ?, lease_expires_at = ?, updated_at = ? \
             WHERE id IN ( \
                 SELECT id FROM scheduled_jobs \
                 WHERE state = 'pending' AND run_at <= ? \
                 ORDER BY run_at, id LIMIT ? \
             ) \
             RETURNING id, payload_json, run_at, attempts, idempotency_key, created_at, updated_at"
        };
        let mut transaction = self.database.pool().begin().await?;
        let rows = sqlx::query(statement)
            .bind(worker_id)
            .bind(&lease_text)
            .bind(&now_text)
            .bind(&now_text)
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;
        let mut jobs: Vec<_> = rows
            .into_iter()
            .map(|row| decode_claimed_job(&row, worker_id, lease_expires_at))
            .collect::<Result<_, _>>()?;
        jobs.sort_by_key(|job| (job.run_at, job.id));
        Ok(jobs)
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
        self.claim_due_internal(worker_id, now, lease_expires_at, limit, false)
            .await
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

#[async_trait]
impl ScanScheduleRepository for SqliteSchedulerRepository {
    async fn upsert_scan_schedule(
        &self,
        schedule: &ScanSchedule,
    ) -> Result<ScanSchedule, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let stored = persist_scan_schedule(&mut transaction, schedule).await?;
        transaction.commit().await?;
        Ok(stored)
    }

    async fn upsert_scan_schedule_for_owner(
        &self,
        owner_id: UserId,
        schedule: &ScanSchedule,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<Option<ScanSchedule>, StorageError> {
        validate_correlation_id(correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let provider_id: Option<String> = sqlx::query_scalar(
            "SELECT provider_id FROM provider_accounts WHERE id = ? AND owner_user_id = ?",
        )
        .bind(schedule.provider_account_id.to_string())
        .bind(owner_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(provider_id) = provider_id else {
            transaction.commit().await?;
            return Ok(None);
        };
        let stored = persist_scan_schedule(&mut transaction, schedule).await?;
        insert_scan_schedule_audit(
            &mut transaction,
            actor,
            &stored,
            correlation_id,
            &provider_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(Some(stored))
    }

    async fn find_scan_schedule(
        &self,
        account_id: ProviderAccountId,
    ) -> Result<Option<ScanSchedule>, StorageError> {
        let row = sqlx::query(
            "SELECT id, provider_account_id, desired_interval_seconds, interval_seconds, \
                    next_run_at, enabled, created_at, updated_at \
             FROM scan_schedules WHERE provider_account_id = ?",
        )
        .bind(account_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_scan_schedule).transpose()
    }

    async fn materialize_due_scan_jobs(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<ScheduledJob>, StorageError> {
        if limit == 0 || limit > MAX_MATERIALIZED_SCAN_JOBS {
            return Err(StorageError::InvalidData(
                "scan materialization limit must be 1-1000".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query(
            "SELECT id, provider_account_id, desired_interval_seconds, interval_seconds, \
                    next_run_at, enabled, created_at, updated_at \
             FROM scan_schedules \
             WHERE enabled = 1 AND next_run_at <= ? \
             ORDER BY next_run_at, id LIMIT ?",
        )
        .bind(encode_timestamp(now))
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await?;
        let mut jobs = Vec::with_capacity(rows.len());
        for row in rows {
            let schedule = decode_scan_schedule(&row)?;
            let job = ScheduledJob {
                id: ScheduleId::new(),
                kind: ScheduledJobKind::Scan {
                    provider_account_id: schedule.provider_account_id,
                },
                run_at: schedule.next_run_at,
                state: ScheduledJobState::Pending,
                attempts: 0,
                idempotency_key: format!(
                    "scan-schedule:{}:{}",
                    schedule.id,
                    encode_timestamp(schedule.next_run_at)
                ),
                created_at: now,
                updated_at: now,
            };
            sqlx::query(
                "INSERT INTO scheduled_jobs \
                 (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, \
                  created_at, updated_at) \
                 VALUES (?, 'scan', ?, ?, 'pending', 0, ?, ?, ?)",
            )
            .bind(job.id.to_string())
            .bind(serde_json::to_string(&job.kind)?)
            .bind(encode_timestamp(job.run_at))
            .bind(&job.idempotency_key)
            .bind(encode_timestamp(now))
            .bind(encode_timestamp(now))
            .execute(&mut *transaction)
            .await?;
            let next_run_at = next_run_after(&schedule, now)?;
            sqlx::query("UPDATE scan_schedules SET next_run_at = ?, updated_at = ? WHERE id = ?")
                .bind(encode_timestamp(next_run_at))
                .bind(encode_timestamp(now))
                .bind(schedule.id.to_string())
                .execute(&mut *transaction)
                .await?;
            jobs.push(job);
        }
        transaction.commit().await?;
        Ok(jobs)
    }

    async fn claim_due_scan_jobs(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<ScheduledJob>, StorageError> {
        self.claim_due_internal(worker_id, now, lease_expires_at, limit, true)
            .await
    }
}

async fn persist_scan_schedule(
    transaction: &mut Transaction<'_, Sqlite>,
    schedule: &ScanSchedule,
) -> Result<ScanSchedule, StorageError> {
    schedule
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let desired_interval_seconds = i64::try_from(schedule.desired_interval_seconds)
        .map_err(|_| StorageError::InvalidData("desired scan interval is too large".to_owned()))?;
    let interval_seconds = i64::try_from(schedule.interval_seconds)
        .map_err(|_| StorageError::InvalidData("scan interval is too large".to_owned()))?;
    let row = sqlx::query(
        "INSERT INTO scan_schedules \
         (id, provider_account_id, desired_interval_seconds, interval_seconds, next_run_at, \
          enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(provider_account_id) DO UPDATE SET \
             desired_interval_seconds = excluded.desired_interval_seconds, \
             interval_seconds = excluded.interval_seconds, \
             next_run_at = excluded.next_run_at, enabled = excluded.enabled, \
             updated_at = excluded.updated_at \
         RETURNING id, provider_account_id, desired_interval_seconds, interval_seconds, \
                   next_run_at, enabled, created_at, updated_at",
    )
    .bind(schedule.id.to_string())
    .bind(schedule.provider_account_id.to_string())
    .bind(desired_interval_seconds)
    .bind(interval_seconds)
    .bind(encode_timestamp(schedule.next_run_at))
    .bind(i64::from(schedule.enabled))
    .bind(encode_timestamp(schedule.created_at))
    .bind(encode_timestamp(schedule.updated_at))
    .fetch_one(&mut **transaction)
    .await?;
    decode_scan_schedule(&row)
}

async fn insert_scan_schedule_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: AuditActor,
    schedule: &ScanSchedule,
    correlation_id: &str,
    provider_id: &str,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let metadata = serde_json::json!({
        "provider_account_id": schedule.provider_account_id,
        "provider_id": provider_id,
        "desired_interval_seconds": schedule.desired_interval_seconds,
        "effective_interval_seconds": schedule.interval_seconds,
        "enabled": schedule.enabled,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'scan_schedule_configured', 'scan_schedule', ?, ?, \
                 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(schedule.updated_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(schedule.id.to_string())
    .bind(correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_correlation_id(correlation_id: &str) -> Result<(), StorageError> {
    if correlation_id.is_empty()
        || correlation_id.len() > 128
        || correlation_id.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidData(
            "scan schedule correlation ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn decode_scan_schedule(row: &sqlx::sqlite::SqliteRow) -> Result<ScanSchedule, StorageError> {
    let desired_interval_seconds =
        u64::try_from(row.try_get::<i64, _>("desired_interval_seconds")?).map_err(|_| {
            StorageError::InvalidData("desired scan interval is negative".to_owned())
        })?;
    let interval_seconds = u64::try_from(row.try_get::<i64, _>("interval_seconds")?)
        .map_err(|_| StorageError::InvalidData("scan interval is negative".to_owned()))?;
    let enabled = match row.try_get::<i64, _>("enabled")? {
        0 => false,
        1 => true,
        _ => {
            return Err(StorageError::InvalidData(
                "scan schedule enabled flag is invalid".to_owned(),
            ));
        }
    };
    let schedule = ScanSchedule {
        id: ScheduleId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        desired_interval_seconds,
        interval_seconds,
        next_run_at: decode_timestamp(row.try_get("next_run_at")?)?,
        enabled,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    schedule
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(schedule)
}

fn next_run_after(schedule: &ScanSchedule, now: Timestamp) -> Result<Timestamp, StorageError> {
    let interval = i64::try_from(schedule.interval_seconds)
        .map_err(|_| StorageError::InvalidData("scan interval is too large".to_owned()))?;
    let overdue_seconds = now
        .signed_duration_since(schedule.next_run_at)
        .num_seconds()
        .max(0);
    let periods = overdue_seconds
        .checked_div(interval)
        .and_then(|periods| periods.checked_add(1))
        .ok_or_else(|| StorageError::InvalidData("scan schedule time overflow".to_owned()))?;
    let advance_seconds = interval
        .checked_mul(periods)
        .ok_or_else(|| StorageError::InvalidData("scan schedule time overflow".to_owned()))?;
    let advance = chrono::Duration::try_seconds(advance_seconds)
        .ok_or_else(|| StorageError::InvalidData("scan schedule time overflow".to_owned()))?;
    schedule
        .next_run_at
        .checked_add_signed(advance)
        .ok_or_else(|| StorageError::InvalidData("scan schedule time overflow".to_owned()))
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
    use asterism_domain::{AuthState, ExecutionId, ProviderAccountId, Role, ScheduleId, UserId};
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
        assert!(
            repository
                .claim_due(owner, now, expiry, 1)
                .await
                .unwrap()
                .is_empty()
        );
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

    #[tokio::test]
    async fn due_scan_schedule_materializes_once_and_skips_backlog() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let (_, account_id) = insert_provider_account(&database).await;
        let repository = SqliteSchedulerRepository::new(database.clone());
        let now = Utc::now();
        let schedule = ScanSchedule {
            id: ScheduleId::new(),
            provider_account_id: account_id,
            desired_interval_seconds: 30,
            interval_seconds: 30,
            next_run_at: now - Duration::seconds(95),
            enabled: true,
            created_at: now - Duration::minutes(5),
            updated_at: now - Duration::minutes(5),
        };
        let stored = repository.upsert_scan_schedule(&schedule).await.unwrap();
        assert_eq!(stored, schedule);

        let mut replacement = schedule.clone();
        replacement.id = ScheduleId::new();
        replacement.updated_at = now - Duration::minutes(4);
        let stored = repository.upsert_scan_schedule(&replacement).await.unwrap();
        assert_eq!(stored.id, schedule.id);

        let (first, second) = tokio::join!(
            repository.materialize_due_scan_jobs(now, 10),
            repository.materialize_due_scan_jobs(now, 10)
        );
        let jobs = [first.unwrap(), second.unwrap()];
        assert_eq!(jobs.iter().map(Vec::len).sum::<usize>(), 1);
        let job = jobs.iter().find_map(|jobs| jobs.first()).unwrap();
        assert_eq!(
            job.kind,
            ScheduledJobKind::Scan {
                provider_account_id: account_id
            }
        );
        assert_eq!(job.run_at, schedule.next_run_at);
        let advanced = repository
            .find_scan_schedule(account_id)
            .await
            .unwrap()
            .unwrap();
        assert!(advanced.next_run_at > now);
        assert!(
            repository
                .materialize_due_scan_jobs(now, 10)
                .await
                .unwrap()
                .is_empty()
        );

        let mut disabled = advanced;
        disabled.enabled = false;
        disabled.next_run_at = now - Duration::seconds(1);
        disabled.updated_at = now;
        repository.upsert_scan_schedule(&disabled).await.unwrap();
        assert!(
            repository
                .materialize_due_scan_jobs(now, 10)
                .await
                .unwrap()
                .is_empty()
        );

        let jobs_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(jobs_count, 1);
    }

    #[tokio::test]
    async fn scan_claim_never_takes_another_job_kind() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteSchedulerRepository::new(database);
        let now = Utc::now();
        let scan = ScheduledJob {
            id: ScheduleId::new(),
            kind: ScheduledJobKind::Scan {
                provider_account_id: ProviderAccountId::new(),
            },
            run_at: now,
            state: ScheduledJobState::Pending,
            attempts: 0,
            idempotency_key: "scan:test".to_owned(),
            created_at: now,
            updated_at: now,
        };
        let execution = ScheduledJob {
            id: ScheduleId::new(),
            kind: ScheduledJobKind::Execution {
                execution_id: ExecutionId::new(),
            },
            idempotency_key: "execution:filtered".to_owned(),
            ..scan.clone()
        };
        repository.enqueue(&execution).await.unwrap();
        repository.enqueue(&scan).await.unwrap();

        let claimed = repository
            .claim_due_scan_jobs("scan-worker", now, now + Duration::minutes(1), 10)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, scan.id);
        let remaining = repository
            .claim_due("general-worker", now, now + Duration::minutes(1), 10)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, execution.id);
    }

    #[tokio::test]
    async fn owner_scoped_schedule_write_is_atomic_and_audited() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let (owner_id, account_id) = insert_provider_account(&database).await;
        let repository = SqliteSchedulerRepository::new(database.clone());
        let now = Utc::now();
        let schedule = ScanSchedule {
            id: ScheduleId::new(),
            provider_account_id: account_id,
            desired_interval_seconds: 60,
            interval_seconds: 300,
            next_run_at: now + Duration::seconds(300),
            enabled: true,
            created_at: now,
            updated_at: now,
        };

        assert!(
            repository
                .upsert_scan_schedule_for_owner(
                    UserId::new(),
                    &schedule,
                    AuditActor::User(owner_id),
                    "schedule-test-denied",
                )
                .await
                .unwrap()
                .is_none()
        );
        let stored = repository
            .upsert_scan_schedule_for_owner(
                owner_id,
                &schedule,
                AuditActor::User(owner_id),
                "schedule-test",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, schedule);

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action = 'scan_schedule_configured' AND correlation_id = 'schedule-test'",
        )
        .bind(schedule.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 1);
    }

    async fn insert_provider_account(database: &Database) -> (UserId, ProviderAccountId) {
        let user_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'provider-alpha', 'primary', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(user_id.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        (user_id, account_id)
    }
}
