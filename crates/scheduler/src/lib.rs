//! Unified scheduling primitives. Providers submit jobs here instead of
//! spawning their own long-running loops.

use std::time::Duration;

use asterism_domain::{
    AnswerBootstrapHarvestId, BatchExecutionId, ExecutionId, NotificationId, ProviderAccountId,
    ScheduleId, Timestamp,
};
use chrono::Duration as ChronoDuration;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScheduledJob {
    pub id: ScheduleId,
    pub kind: ScheduledJobKind,
    pub run_at: Timestamp,
    pub state: ScheduledJobState,
    pub attempts: u32,
    pub idempotency_key: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScanSchedule {
    pub id: ScheduleId,
    pub provider_account_id: ProviderAccountId,
    pub desired_interval_seconds: u64,
    pub interval_seconds: u64,
    pub next_run_at: Timestamp,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ScanSchedule {
    /// Builds a user-managed schedule after applying the Provider's optional
    /// minimum interval. Existing identity and creation time are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`ScanScheduleError`] when either interval is invalid or the
    /// next run cannot be represented.
    pub fn configured(
        provider_account_id: ProviderAccountId,
        desired_interval_seconds: u64,
        provider_min_interval_seconds: Option<u64>,
        enabled: bool,
        at: Timestamp,
        existing: Option<&Self>,
    ) -> Result<Self, ScanScheduleError> {
        validate_interval(desired_interval_seconds)?;
        if let Some(minimum) = provider_min_interval_seconds {
            validate_interval(minimum)?;
        }
        let interval_seconds = provider_min_interval_seconds
            .map_or(desired_interval_seconds, |minimum| {
                minimum.max(desired_interval_seconds)
            });
        let interval =
            i64::try_from(interval_seconds).map_err(|_| ScanScheduleError::IntervalOutOfRange)?;
        let next_run_at = at
            .checked_add_signed(ChronoDuration::seconds(interval))
            .ok_or(ScanScheduleError::NextRunOutOfRange)?;
        let schedule = Self {
            id: existing.map_or_else(ScheduleId::new, |schedule| schedule.id),
            provider_account_id,
            desired_interval_seconds,
            interval_seconds,
            next_run_at,
            enabled,
            created_at: existing.map_or(at, |schedule| schedule.created_at),
            updated_at: at,
        };
        schedule.validate()?;
        Ok(schedule)
    }

    /// Validates persisted schedule fields independently from Provider-specific
    /// minimum interval policy.
    ///
    /// # Errors
    ///
    /// Returns [`ScanScheduleError`] for a zero or unrepresentable interval, or
    /// timestamps that move backwards across the schedule lifecycle.
    pub fn validate(&self) -> Result<(), ScanScheduleError> {
        if self.desired_interval_seconds == 0 || self.interval_seconds == 0 {
            return Err(ScanScheduleError::ZeroInterval);
        }
        if i64::try_from(self.desired_interval_seconds).is_err()
            || i64::try_from(self.interval_seconds).is_err()
        {
            return Err(ScanScheduleError::IntervalOutOfRange);
        }
        if self.interval_seconds < self.desired_interval_seconds {
            return Err(ScanScheduleError::EffectiveIntervalTooShort);
        }
        if self.updated_at < self.created_at {
            return Err(ScanScheduleError::InvalidTimestamps);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScanScheduleError {
    #[error("scan schedule interval must be greater than zero")]
    ZeroInterval,
    #[error("scan schedule interval is outside the supported clock range")]
    IntervalOutOfRange,
    #[error("effective scan interval must not be shorter than the desired interval")]
    EffectiveIntervalTooShort,
    #[error("scan schedule lifecycle timestamps move backwards")]
    InvalidTimestamps,
    #[error("scan schedule next run is outside the supported clock range")]
    NextRunOutOfRange,
}

fn validate_interval(interval_seconds: u64) -> Result<(), ScanScheduleError> {
    if interval_seconds == 0 {
        return Err(ScanScheduleError::ZeroInterval);
    }
    i64::try_from(interval_seconds)
        .map(|_| ())
        .map_err(|_| ScanScheduleError::IntervalOutOfRange)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "payload")]
pub enum ScheduledJobKind {
    AnswerBootstrapHarvest {
        harvest_id: AnswerBootstrapHarvestId,
        provider_account_id: ProviderAccountId,
        generation: u32,
    },
    Scan {
        provider_account_id: ProviderAccountId,
    },
    Execution {
        execution_id: ExecutionId,
    },
    BatchExecution {
        batch_execution_id: BatchExecutionId,
    },
    Retry {
        execution_id: ExecutionId,
        next_attempt_no: u32,
    },
    Recovery {
        execution_id: ExecutionId,
    },
    Notification {
        notification_id: NotificationId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ScheduledJobState {
    Pending,
    Claimed {
        worker_id: String,
        lease_expires_at: Timestamp,
    },
    Completed,
    Cancelled,
    DeadLetter,
}

impl ScheduledJobState {
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::DeadLetter)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryPolicy {
    /// Total number of attempts, including the initial attempt.
    pub max_attempts: u32,
    pub initial_delay_seconds: u64,
    pub multiplier: u32,
    pub max_delay_seconds: u64,
}

impl RetryPolicy {
    /// Validates that retry behavior is bounded and non-zero.
    ///
    /// # Errors
    ///
    /// Returns [`RetryPolicyError`] for an empty attempt budget, zero delay,
    /// multiplier below one, or a cap below the initial delay.
    pub const fn validate(self) -> Result<(), RetryPolicyError> {
        if self.max_attempts == 0 {
            return Err(RetryPolicyError::NoAttempts);
        }
        if self.initial_delay_seconds == 0 {
            return Err(RetryPolicyError::ZeroDelay);
        }
        if self.multiplier == 0 {
            return Err(RetryPolicyError::ZeroMultiplier);
        }
        if self.max_delay_seconds < self.initial_delay_seconds {
            return Err(RetryPolicyError::CapBelowInitialDelay);
        }
        Ok(())
    }

    /// Returns the backoff after `failed_attempt_no`, or `None` once the total
    /// attempt budget has been exhausted.
    ///
    /// # Errors
    ///
    /// Returns [`RetryPolicyError`] when the policy is invalid or attempt zero
    /// is supplied.
    pub fn delay_after(self, failed_attempt_no: u32) -> Result<Option<Duration>, RetryPolicyError> {
        self.validate()?;
        if failed_attempt_no == 0 {
            return Err(RetryPolicyError::AttemptZero);
        }
        if failed_attempt_no >= self.max_attempts {
            return Ok(None);
        }

        let exponent = failed_attempt_no.saturating_sub(1);
        let factor = u64::from(self.multiplier).saturating_pow(exponent);
        let seconds = self
            .initial_delay_seconds
            .saturating_mul(factor)
            .min(self.max_delay_seconds);
        Ok(Some(Duration::from_secs(seconds)))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RetryPolicyError {
    #[error("retry policy must permit at least one attempt")]
    NoAttempts,
    #[error("retry delay must be greater than zero")]
    ZeroDelay,
    #[error("retry multiplier must be at least one")]
    ZeroMultiplier,
    #[error("retry delay cap cannot be below the initial delay")]
    CapBelowInitialDelay,
    #[error("attempt numbering starts at one")]
    AttemptZero,
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: RetryPolicy = RetryPolicy {
        max_attempts: 5,
        initial_delay_seconds: 10,
        multiplier: 3,
        max_delay_seconds: 100,
    };

    #[test]
    fn exponential_backoff_is_capped_and_stops_at_budget() {
        assert_eq!(
            POLICY.delay_after(1).unwrap(),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            POLICY.delay_after(2).unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            POLICY.delay_after(3).unwrap(),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            POLICY.delay_after(4).unwrap(),
            Some(Duration::from_secs(100))
        );
        assert_eq!(POLICY.delay_after(5).unwrap(), None);
    }

    #[test]
    fn invalid_retry_policy_is_rejected() {
        let invalid = RetryPolicy {
            max_attempts: 0,
            ..POLICY
        };
        assert_eq!(invalid.validate(), Err(RetryPolicyError::NoAttempts));
        assert_eq!(POLICY.delay_after(0), Err(RetryPolicyError::AttemptZero));
    }

    #[test]
    fn scan_schedule_rejects_invalid_interval_and_lifecycle() {
        let now = chrono::Utc::now();
        let mut schedule = ScanSchedule {
            id: ScheduleId::new(),
            provider_account_id: ProviderAccountId::new(),
            desired_interval_seconds: 60,
            interval_seconds: 60,
            next_run_at: now,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(schedule.validate(), Ok(()));
        schedule.interval_seconds = 0;
        assert_eq!(schedule.validate(), Err(ScanScheduleError::ZeroInterval));
        schedule.interval_seconds = 30;
        assert_eq!(
            schedule.validate(),
            Err(ScanScheduleError::EffectiveIntervalTooShort)
        );
        schedule.interval_seconds = 60;
        schedule.updated_at = now - chrono::Duration::seconds(1);
        assert_eq!(
            schedule.validate(),
            Err(ScanScheduleError::InvalidTimestamps)
        );
    }

    #[test]
    fn configured_schedule_applies_provider_floor_and_preserves_identity() {
        let now = chrono::Utc::now();
        let account_id = ProviderAccountId::new();
        let schedule =
            ScanSchedule::configured(account_id, 60, Some(300), true, now, None).unwrap();
        assert_eq!(schedule.desired_interval_seconds, 60);
        assert_eq!(schedule.interval_seconds, 300);
        assert_eq!(schedule.next_run_at, now + chrono::Duration::seconds(300));

        let updated_at = now + chrono::Duration::seconds(1);
        let disabled = ScanSchedule::configured(
            account_id,
            600,
            Some(300),
            false,
            updated_at,
            Some(&schedule),
        )
        .unwrap();
        assert_eq!(disabled.id, schedule.id);
        assert_eq!(disabled.created_at, schedule.created_at);
        assert_eq!(disabled.interval_seconds, 600);
        assert!(!disabled.enabled);
    }
}
