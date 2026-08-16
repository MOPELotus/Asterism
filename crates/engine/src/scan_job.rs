use std::time::Duration as StdDuration;

use asterism_domain::{ProviderAccountId, Timestamp};
use asterism_provider_api::ProviderErrorKind;
use asterism_scheduler::{
    RetryPolicy, RetryPolicyError, ScheduledJob, ScheduledJobKind, ScheduledJobState,
};
use asterism_storage::{
    JobFailureDisposition, ProviderAccountRuntimeRepository, ProviderScanReport,
    SchedulerRepository, StorageError,
};

use crate::{ProviderAccountScanner, ProviderScanError};

#[derive(Debug)]
pub struct ScheduledScanRunner<S, A, N> {
    scheduler: S,
    accounts: A,
    scanner: N,
    retry_policy: RetryPolicy,
}

impl<S, A, N> ScheduledScanRunner<S, A, N> {
    /// Constructs a runner with a bounded retry policy.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduledScanRunError`] when the retry policy is invalid or
    /// its maximum delay cannot be represented by the scheduler clock.
    pub fn new(
        scheduler: S,
        accounts: A,
        scanner: N,
        retry_policy: RetryPolicy,
    ) -> Result<Self, ScheduledScanRunError> {
        retry_policy.validate()?;
        i64::try_from(retry_policy.max_delay_seconds)
            .map_err(|_| ScheduledScanRunError::RetryTimeOverflow)?;
        Ok(Self {
            scheduler,
            accounts,
            scanner,
            retry_policy,
        })
    }
}

impl<S, A, N> ScheduledScanRunner<S, A, N>
where
    S: SchedulerRepository,
    A: ProviderAccountRuntimeRepository,
    N: ProviderAccountScanner,
{
    /// Executes one already-claimed scan job and persists its terminal or retry
    /// state under the same worker claim.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduledScanRunError`] for the wrong job kind/state, an
    /// expired claim, invalid retry time arithmetic, or lost scheduler storage.
    pub async fn run_claimed(
        &self,
        job: &ScheduledJob,
        now: Timestamp,
    ) -> Result<ScheduledScanOutcome, ScheduledScanRunError> {
        let (account_id, worker_id) = claimed_scan(job, now)?;
        let account = match self
            .accounts
            .find_runtime_provider_account(account_id)
            .await
        {
            Ok(Some(account)) => account,
            Ok(None) => {
                return self
                    .record_failure(
                        job,
                        worker_id,
                        ScheduledScanFailure::AccountMissing,
                        false,
                        None,
                        now,
                    )
                    .await;
            }
            Err(_) => {
                return self
                    .record_failure(
                        job,
                        worker_id,
                        ScheduledScanFailure::Storage,
                        true,
                        None,
                        now,
                    )
                    .await;
            }
        };
        let correlation_id = format!("scheduled-scan:{}", job.id);
        match self
            .scanner
            .scan_account(&account, correlation_id, None, now)
            .await
        {
            Ok(report) => {
                self.scheduler.complete(job.id, worker_id, now).await?;
                Ok(ScheduledScanOutcome::Completed(report))
            }
            Err(error) => {
                let (failure, retryable, retry_after_seconds) = classify_scan_error(&error);
                self.record_failure(job, worker_id, failure, retryable, retry_after_seconds, now)
                    .await
            }
        }
    }

    async fn record_failure(
        &self,
        job: &ScheduledJob,
        worker_id: &str,
        failure: ScheduledScanFailure,
        retryable: bool,
        retry_after_seconds: Option<u64>,
        now: Timestamp,
    ) -> Result<ScheduledScanOutcome, ScheduledScanRunError> {
        let failed_attempt_no = job.attempts.saturating_add(1);
        let retry_at = if retryable {
            self.retry_at(failed_attempt_no, retry_after_seconds, now)?
        } else {
            None
        };
        let disposition = self
            .scheduler
            .fail(job.id, worker_id, failure.code(), retry_at, now)
            .await?;
        match disposition {
            JobFailureDisposition::RetryPending => Ok(ScheduledScanOutcome::RetryScheduled {
                failure,
                failed_attempt_no,
                retry_at: retry_at.ok_or(ScheduledScanRunError::RetryDispositionMismatch)?,
            }),
            JobFailureDisposition::DeadLetter => Ok(ScheduledScanOutcome::DeadLetter {
                failure,
                failed_attempt_no,
            }),
        }
    }

    fn retry_at(
        &self,
        failed_attempt_no: u32,
        retry_after_seconds: Option<u64>,
        now: Timestamp,
    ) -> Result<Option<Timestamp>, ScheduledScanRunError> {
        let Some(policy_delay) = self.retry_policy.delay_after(failed_attempt_no)? else {
            return Ok(None);
        };
        let provider_delay = retry_after_seconds
            .unwrap_or_default()
            .min(self.retry_policy.max_delay_seconds);
        let delay = policy_delay.max(StdDuration::from_secs(provider_delay));
        let seconds =
            i64::try_from(delay.as_secs()).map_err(|_| ScheduledScanRunError::RetryTimeOverflow)?;
        let duration = chrono::Duration::try_seconds(seconds)
            .ok_or(ScheduledScanRunError::RetryTimeOverflow)?;
        now.checked_add_signed(duration)
            .map(Some)
            .ok_or(ScheduledScanRunError::RetryTimeOverflow)
    }
}

fn claimed_scan(
    job: &ScheduledJob,
    now: Timestamp,
) -> Result<(ProviderAccountId, &str), ScheduledScanRunError> {
    let ScheduledJobKind::Scan {
        provider_account_id: account_id,
    } = job.kind
    else {
        return Err(ScheduledScanRunError::UnsupportedJobKind);
    };
    match &job.state {
        ScheduledJobState::Claimed {
            worker_id,
            lease_expires_at,
        } if *lease_expires_at > now => Ok((account_id, worker_id)),
        ScheduledJobState::Claimed { .. } => Err(ScheduledScanRunError::ClaimExpired),
        _ => Err(ScheduledScanRunError::JobNotClaimed),
    }
}

fn classify_scan_error(error: &ProviderScanError) -> (ScheduledScanFailure, bool, Option<u64>) {
    match error {
        ProviderScanError::Provider(provider) => match provider.kind {
            ProviderErrorKind::RateLimited => (
                ScheduledScanFailure::ProviderRateLimited,
                true,
                provider.retry_after_seconds,
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                (ScheduledScanFailure::ProviderUnavailable, true, None)
            }
            ProviderErrorKind::Authentication | ProviderErrorKind::Authorization => {
                (ScheduledScanFailure::AuthenticationRequired, false, None)
            }
            ProviderErrorKind::RemoteChanged
            | ProviderErrorKind::UnsupportedTask
            | ProviderErrorKind::HumanRequired => {
                (ScheduledScanFailure::HumanRequired, false, None)
            }
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                (ScheduledScanFailure::InvalidInventory, false, None)
            }
            ProviderErrorKind::Internal => (ScheduledScanFailure::Internal, false, None),
        },
        ProviderScanError::Storage(_) => (ScheduledScanFailure::Storage, true, None),
        ProviderScanError::AccountNotAuthenticated(_) => {
            (ScheduledScanFailure::AuthenticationRequired, false, None)
        }
        ProviderScanError::ProviderNotRegistered(_)
        | ProviderScanError::NoInventoryCapabilities(_) => {
            (ScheduledScanFailure::ProviderNotReady, false, None)
        }
        ProviderScanError::CourseScopeMismatch { .. }
        | ProviderScanError::UnadvertisedTaskCapability { .. }
        | ProviderScanError::InvalidProtocolObservation
        | ProviderScanError::InvalidCorrelationId => {
            (ScheduledScanFailure::InvalidInventory, false, None)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduledScanFailure {
    AccountMissing,
    AuthenticationRequired,
    HumanRequired,
    ProviderNotReady,
    ProviderRateLimited,
    ProviderUnavailable,
    InvalidInventory,
    Storage,
    Internal,
}

impl ScheduledScanFailure {
    const fn code(self) -> &'static str {
        match self {
            Self::AccountMissing => "provider_account_missing",
            Self::AuthenticationRequired => "provider_authentication_required",
            Self::HumanRequired => "provider_human_required",
            Self::ProviderNotReady => "provider_not_ready",
            Self::ProviderRateLimited => "provider_rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::InvalidInventory => "provider_inventory_invalid",
            Self::Storage => "scan_storage_unavailable",
            Self::Internal => "scan_internal_error",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduledScanOutcome {
    Completed(ProviderScanReport),
    RetryScheduled {
        failure: ScheduledScanFailure,
        failed_attempt_no: u32,
        retry_at: Timestamp,
    },
    DeadLetter {
        failure: ScheduledScanFailure,
        failed_attempt_no: u32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduledScanRunError {
    #[error("scheduled scan runner received a non-scan job")]
    UnsupportedJobKind,
    #[error("scheduled scan job is not claimed")]
    JobNotClaimed,
    #[error("scheduled scan job claim has expired")]
    ClaimExpired,
    #[error("scheduled scan retry time is outside the supported clock range")]
    RetryTimeOverflow,
    #[error("scheduler returned retry disposition without a retry timestamp")]
    RetryDispositionMismatch,
    #[error(transparent)]
    RetryPolicy(#[from] RetryPolicyError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use asterism_domain::{AuditActor, AuthState, ProviderAccount, ProviderId, ScheduleId, UserId};
    use asterism_provider_api::{ProviderError, ProviderErrorKind};
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    const RETRY_POLICY: RetryPolicy = RetryPolicy {
        max_attempts: 3,
        initial_delay_seconds: 10,
        multiplier: 2,
        max_delay_seconds: 60,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum SchedulerAction {
        Completed(ScheduleId),
        Failed {
            id: ScheduleId,
            error: String,
            retry_at: Option<Timestamp>,
        },
    }

    #[derive(Clone, Debug, Default)]
    struct RecordingScheduler {
        actions: Arc<Mutex<Vec<SchedulerAction>>>,
    }

    #[async_trait]
    impl SchedulerRepository for RecordingScheduler {
        async fn enqueue(&self, _job: &ScheduledJob) -> Result<(), StorageError> {
            Ok(())
        }

        async fn claim_due(
            &self,
            _worker_id: &str,
            _now: Timestamp,
            _lease_expires_at: Timestamp,
            _limit: u32,
        ) -> Result<Vec<ScheduledJob>, StorageError> {
            Ok(Vec::new())
        }

        async fn claim_due_execution_jobs(
            &self,
            _worker_id: &str,
            _now: Timestamp,
            _lease_expires_at: Timestamp,
            _limit: u32,
        ) -> Result<Vec<ScheduledJob>, StorageError> {
            Ok(Vec::new())
        }

        async fn claim_due_batch_execution_jobs(
            &self,
            _worker_id: &str,
            _now: Timestamp,
            _lease_expires_at: Timestamp,
            _limit: u32,
        ) -> Result<Vec<ScheduledJob>, StorageError> {
            Ok(Vec::new())
        }

        async fn renew_claim(
            &self,
            _job_id: ScheduleId,
            _worker_id: &str,
            _now: Timestamp,
            _new_expires_at: Timestamp,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn complete(
            &self,
            job_id: ScheduleId,
            _worker_id: &str,
            _at: Timestamp,
        ) -> Result<(), StorageError> {
            self.actions
                .lock()
                .unwrap()
                .push(SchedulerAction::Completed(job_id));
            Ok(())
        }

        async fn fail(
            &self,
            job_id: ScheduleId,
            _worker_id: &str,
            error_sanitized: &str,
            retry_at: Option<Timestamp>,
            _at: Timestamp,
        ) -> Result<JobFailureDisposition, StorageError> {
            self.actions.lock().unwrap().push(SchedulerAction::Failed {
                id: job_id,
                error: error_sanitized.to_owned(),
                retry_at,
            });
            Ok(if retry_at.is_some() {
                JobFailureDisposition::RetryPending
            } else {
                JobFailureDisposition::DeadLetter
            })
        }
    }

    #[derive(Clone, Debug)]
    struct FixedAccounts(Option<ProviderAccount>);

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FixedAccounts {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok(self.0.clone().filter(|account| account.id == account_id))
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum ScanBehavior {
        Success,
        RateLimited,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ScanCall {
        account_id: ProviderAccountId,
        correlation_id: String,
        initiated_by: Option<AuditActor>,
    }

    #[derive(Clone, Debug)]
    struct FixedScanner {
        behavior: ScanBehavior,
        calls: Arc<Mutex<Vec<ScanCall>>>,
    }

    #[async_trait]
    impl ProviderAccountScanner for FixedScanner {
        async fn scan_account(
            &self,
            account: &ProviderAccount,
            correlation_id: String,
            initiated_by: Option<AuditActor>,
            _observed_at: Timestamp,
        ) -> Result<ProviderScanReport, ProviderScanError> {
            self.calls.lock().unwrap().push(ScanCall {
                account_id: account.id,
                correlation_id,
                initiated_by,
            });
            match self.behavior {
                ScanBehavior::Success => Ok(ProviderScanReport {
                    courses_seen: 1,
                    tasks_created: 1,
                    tasks_updated: 0,
                    tasks_unchanged: 0,
                    task_changes: Vec::new(),
                }),
                ScanBehavior::RateLimited => {
                    let mut error = ProviderError::new(ProviderErrorKind::RateLimited, "sanitized");
                    error.retry_after_seconds = Some(30);
                    Err(ProviderScanError::Provider(error))
                }
            }
        }
    }

    #[tokio::test]
    async fn claimed_scan_completes_with_system_initiator() {
        let now = Utc::now();
        let account = account(now);
        let job = claimed_job(account.id, now);
        let scheduler = RecordingScheduler::default();
        let scanner = FixedScanner {
            behavior: ScanBehavior::Success,
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let runner = ScheduledScanRunner::new(
            scheduler.clone(),
            FixedAccounts(Some(account.clone())),
            scanner.clone(),
            RETRY_POLICY,
        )
        .unwrap();

        let outcome = runner.run_claimed(&job, now).await.unwrap();
        assert!(matches!(outcome, ScheduledScanOutcome::Completed(_)));
        assert_eq!(
            *scheduler.actions.lock().unwrap(),
            [SchedulerAction::Completed(job.id)]
        );
        let calls = scanner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].account_id, account.id);
        assert_eq!(
            calls[0].correlation_id,
            format!("scheduled-scan:{}", job.id)
        );
        assert_eq!(calls[0].initiated_by, None);
    }

    #[tokio::test]
    async fn rate_limit_uses_larger_bounded_provider_delay() {
        let now = Utc::now();
        let account = account(now);
        let job = claimed_job(account.id, now);
        let scheduler = RecordingScheduler::default();
        let scanner = FixedScanner {
            behavior: ScanBehavior::RateLimited,
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let runner = ScheduledScanRunner::new(
            scheduler.clone(),
            FixedAccounts(Some(account)),
            scanner,
            RETRY_POLICY,
        )
        .unwrap();

        let retry_at = now + chrono::Duration::seconds(30);
        assert_eq!(
            runner.run_claimed(&job, now).await.unwrap(),
            ScheduledScanOutcome::RetryScheduled {
                failure: ScheduledScanFailure::ProviderRateLimited,
                failed_attempt_no: 1,
                retry_at,
            }
        );
        assert_eq!(
            *scheduler.actions.lock().unwrap(),
            [SchedulerAction::Failed {
                id: job.id,
                error: "provider_rate_limited".to_owned(),
                retry_at: Some(retry_at),
            }]
        );
    }

    #[tokio::test]
    async fn missing_account_is_dead_lettered_without_provider_call() {
        let now = Utc::now();
        let job = claimed_job(ProviderAccountId::new(), now);
        let scheduler = RecordingScheduler::default();
        let scanner = FixedScanner {
            behavior: ScanBehavior::Success,
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let runner = ScheduledScanRunner::new(
            scheduler,
            FixedAccounts(None),
            scanner.clone(),
            RETRY_POLICY,
        )
        .unwrap();

        assert_eq!(
            runner.run_claimed(&job, now).await.unwrap(),
            ScheduledScanOutcome::DeadLetter {
                failure: ScheduledScanFailure::AccountMissing,
                failed_attempt_no: 1,
            }
        );
        assert!(scanner.calls.lock().unwrap().is_empty());
    }

    fn account(now: Timestamp) -> ProviderAccount {
        ProviderAccount {
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
        }
    }

    fn claimed_job(account_id: ProviderAccountId, now: Timestamp) -> ScheduledJob {
        ScheduledJob {
            id: ScheduleId::new(),
            kind: ScheduledJobKind::Scan {
                provider_account_id: account_id,
            },
            run_at: now,
            state: ScheduledJobState::Claimed {
                worker_id: "scan-worker".to_owned(),
                lease_expires_at: now + chrono::Duration::minutes(1),
            },
            attempts: 0,
            idempotency_key: format!("scan:{account_id}"),
            created_at: now,
            updated_at: now,
        }
    }
}
