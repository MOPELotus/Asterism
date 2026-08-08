use asterism_domain::Timestamp;
use asterism_scheduler::RetryPolicy;
use asterism_storage::{
    ProviderAccountRuntimeRepository, ScanScheduleRepository, SchedulerRepository, StorageError,
};

use crate::{
    ProviderAccountScanner, ScheduledScanOutcome, ScheduledScanRunError, ScheduledScanRunner,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanSchedulerConfig {
    pub worker_id: String,
    pub materialize_limit: u32,
    pub claim_limit: u32,
    pub claim_ttl_seconds: u64,
    pub retry_policy: RetryPolicy,
}

impl ScanSchedulerConfig {
    fn validate(&self) -> Result<(), ScanSchedulerWorkerError> {
        if self.worker_id.is_empty()
            || self.worker_id.len() > 128
            || self.worker_id.trim() != self.worker_id
            || self.worker_id.chars().any(char::is_control)
            || self.materialize_limit == 0
            || self.materialize_limit > 1_000
            || self.claim_limit == 0
            || self.claim_limit > 1_000
            || self.claim_ttl_seconds == 0
            || i64::try_from(self.claim_ttl_seconds).is_err()
        {
            return Err(ScanSchedulerWorkerError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ScanSchedulerWorker<S, A, N> {
    schedules: S,
    runner: ScheduledScanRunner<S, A, N>,
    config: ScanSchedulerConfig,
}

impl<S, A, N> ScanSchedulerWorker<S, A, N>
where
    S: Clone,
{
    /// Builds one unified scan worker from queue, account, and Provider scan
    /// boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`ScanSchedulerWorkerError`] for unsafe batch/lease settings or
    /// an invalid retry policy.
    pub fn new(
        schedules: S,
        accounts: A,
        scanner: N,
        config: ScanSchedulerConfig,
    ) -> Result<Self, ScanSchedulerWorkerError> {
        config.validate()?;
        let runner =
            ScheduledScanRunner::new(schedules.clone(), accounts, scanner, config.retry_policy)?;
        Ok(Self {
            schedules,
            runner,
            config,
        })
    }
}

impl<S, A, N> ScanSchedulerWorker<S, A, N>
where
    S: Clone + ScanScheduleRepository + SchedulerRepository,
    A: ProviderAccountRuntimeRepository,
    N: ProviderAccountScanner,
{
    /// Materializes due periods, claims only scan jobs, and processes the
    /// bounded claimed batch sequentially.
    ///
    /// # Errors
    ///
    /// Returns [`ScanSchedulerWorkerError`] when queue persistence fails, the
    /// claim time overflows, or a claimed job loses its scheduler ownership.
    pub async fn tick_once(
        &self,
        now: Timestamp,
    ) -> Result<ScanSchedulerTickReport, ScanSchedulerWorkerError> {
        let materialized = self
            .schedules
            .materialize_due_scan_jobs(now, self.config.materialize_limit)
            .await?
            .len();
        let ttl_seconds = i64::try_from(self.config.claim_ttl_seconds)
            .map_err(|_| ScanSchedulerWorkerError::InvalidConfig)?;
        let claim_ttl = chrono::Duration::try_seconds(ttl_seconds)
            .ok_or(ScanSchedulerWorkerError::ClaimTimeOverflow)?;
        let lease_expires_at = now
            .checked_add_signed(claim_ttl)
            .ok_or(ScanSchedulerWorkerError::ClaimTimeOverflow)?;
        let jobs = self
            .schedules
            .claim_due_scan_jobs(
                &self.config.worker_id,
                now,
                lease_expires_at,
                self.config.claim_limit,
            )
            .await?;
        let mut report = ScanSchedulerTickReport {
            materialized,
            claimed: jobs.len(),
            completed: 0,
            retry_scheduled: 0,
            dead_lettered: 0,
        };
        for job in jobs {
            match self.runner.run_claimed(&job, now).await? {
                ScheduledScanOutcome::Completed(_) => report.completed += 1,
                ScheduledScanOutcome::RetryScheduled { .. } => report.retry_scheduled += 1,
                ScheduledScanOutcome::DeadLetter { .. } => report.dead_lettered += 1,
            }
        }
        Ok(report)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanSchedulerTickReport {
    pub materialized: usize,
    pub claimed: usize,
    pub completed: usize,
    pub retry_scheduled: usize,
    pub dead_lettered: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanSchedulerWorkerError {
    #[error("scan scheduler worker configuration is invalid")]
    InvalidConfig,
    #[error("scan scheduler claim expiry is outside the supported clock range")]
    ClaimTimeOverflow,
    #[error(transparent)]
    Runner(#[from] ScheduledScanRunError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use asterism_domain::{
        AuthState, ProviderAccount, ProviderAccountId, ProviderId, ScheduleId, UserId,
    };
    use asterism_scheduler::ScanSchedule;
    use asterism_storage::{
        Database, ProviderScanReport, SqliteProviderAccountRepository, SqliteSchedulerRepository,
    };
    use async_trait::async_trait;
    use chrono::{SecondsFormat, Utc};

    use super::*;
    use crate::ProviderScanError;

    #[derive(Clone, Debug, Default)]
    struct SuccessfulScanner {
        calls: Arc<Mutex<Vec<ProviderAccountId>>>,
    }

    #[async_trait]
    impl ProviderAccountScanner for SuccessfulScanner {
        async fn scan_account(
            &self,
            account: &ProviderAccount,
            _correlation_id: String,
            _initiated_by: Option<asterism_domain::AuditActor>,
            _observed_at: Timestamp,
        ) -> Result<ProviderScanReport, ProviderScanError> {
            self.calls.lock().unwrap().push(account.id);
            Ok(ProviderScanReport {
                courses_seen: 0,
                tasks_created: 0,
                tasks_updated: 0,
                tasks_unchanged: 0,
                task_changes: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn tick_materializes_claims_and_completes_one_due_scan() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let account = insert_account(&database, now).await;
        let schedules = SqliteSchedulerRepository::new(database.clone());
        schedules
            .upsert_scan_schedule(&ScanSchedule {
                id: ScheduleId::new(),
                provider_account_id: account.id,
                interval_seconds: 60,
                next_run_at: now,
                enabled: true,
                created_at: now,
                updated_at: now,
            })
            .await
            .unwrap();
        let scanner = SuccessfulScanner::default();
        let worker = ScanSchedulerWorker::new(
            schedules,
            SqliteProviderAccountRepository::new(database.clone()),
            scanner.clone(),
            config(),
        )
        .unwrap();

        assert_eq!(
            worker.tick_once(now).await.unwrap(),
            ScanSchedulerTickReport {
                materialized: 1,
                claimed: 1,
                completed: 1,
                retry_scheduled: 0,
                dead_lettered: 0,
            }
        );
        assert_eq!(*scanner.calls.lock().unwrap(), [account.id]);
        let state: String = sqlx::query_scalar("SELECT state FROM scheduled_jobs")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(state, "completed");
    }

    #[test]
    fn worker_rejects_zero_or_unbounded_batches() {
        let mut invalid = config();
        invalid.claim_limit = 0;
        assert!(matches!(
            ScanSchedulerWorker::new((), (), NoScanner, invalid),
            Err(ScanSchedulerWorkerError::InvalidConfig)
        ));
    }

    #[derive(Clone, Copy, Debug)]
    struct NoScanner;

    #[async_trait]
    impl ProviderAccountScanner for NoScanner {
        async fn scan_account(
            &self,
            _account: &ProviderAccount,
            _correlation_id: String,
            _initiated_by: Option<asterism_domain::AuditActor>,
            _observed_at: Timestamp,
        ) -> Result<ProviderScanReport, ProviderScanError> {
            unreachable!()
        }
    }

    fn config() -> ScanSchedulerConfig {
        ScanSchedulerConfig {
            worker_id: "scan-worker".to_owned(),
            materialize_limit: 10,
            claim_limit: 10,
            claim_ttl_seconds: 60,
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay_seconds: 10,
                multiplier: 2,
                max_delay_seconds: 60,
            },
        }
    }

    async fn insert_account(database: &Database, now: Timestamp) -> ProviderAccount {
        let account = ProviderAccount {
            id: ProviderAccountId::new(),
            owner_id: UserId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        let now_text = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(account.owner_id.to_string())
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account.id.to_string())
        .bind(account.owner_id.to_string())
        .bind(account.provider_id.as_str())
        .bind(&account.display_name)
        .bind(serde_json::to_string(&account.auth_state).unwrap())
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        account
    }
}
