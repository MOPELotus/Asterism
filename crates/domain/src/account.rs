use serde::{Deserialize, Serialize};

use crate::{
    AuthState, CourseId, ExecutionId, HumanRequiredReason, ProviderAccountId, SecretId, Timestamp,
    UserId, WaitingUserState,
};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Creates a stable identifier suitable for routes and persistence.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderIdError`] when the value is empty, longer than 64
    /// bytes, or contains characters outside lowercase ASCII, digits, and `-`.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderIdError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(ProviderIdError)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("provider id must contain 1-64 lowercase ASCII letters, digits, or hyphens")]
pub struct ProviderIdError;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderAccount {
    pub id: ProviderAccountId,
    pub owner_id: UserId,
    pub provider_id: ProviderId,
    pub display_name: String,
    pub tenant: Option<String>,
    pub auth_state: crate::AuthState,
    pub network_profile_id: Option<String>,
    pub credential_refs: Vec<SecretId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountHealthState {
    Healthy,
    Checking,
    ExpiringSoon,
    Expired,
    HumanActionRequired,
    Broken,
    ProtocolChanged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountHealth {
    pub provider_account_id: ProviderAccountId,
    pub state: AccountHealthState,
    pub auth_state: AuthState,
    pub human_required_reason: Option<HumanRequiredReason>,
    pub protocol_drift_execution_id: Option<ExecutionId>,
    pub protocol_drift_at: Option<Timestamp>,
    pub observed_at: Timestamp,
}

impl AccountHealth {
    pub fn from_account(
        account: &ProviderAccount,
        protocol_drift: Option<(ExecutionId, Timestamp)>,
    ) -> Self {
        if let Some((execution_id, drift_at)) =
            protocol_drift.filter(|(_, drift_at)| *drift_at >= account.updated_at)
        {
            return Self {
                provider_account_id: account.id,
                state: AccountHealthState::ProtocolChanged,
                auth_state: account.auth_state.clone(),
                human_required_reason: None,
                protocol_drift_execution_id: Some(execution_id),
                protocol_drift_at: Some(drift_at),
                observed_at: drift_at,
            };
        }
        let (state, reason) = match account.auth_state {
            AuthState::Authenticated => (AccountHealthState::Healthy, None),
            AuthState::Starting
            | AuthState::ExchangingCredential
            | AuthState::ValidatingCredential => (AccountHealthState::Checking, None),
            AuthState::Refreshing => (AccountHealthState::ExpiringSoon, None),
            AuthState::Expired => (AccountHealthState::Expired, None),
            AuthState::ClientUpdateRequired => (AccountHealthState::ProtocolChanged, None),
            AuthState::AuthFailed | AuthState::ProviderUnavailable => {
                (AccountHealthState::Broken, None)
            }
            AuthState::HumanRequired(reason) => {
                (AccountHealthState::HumanActionRequired, Some(reason))
            }
            AuthState::WaitingUser(waiting) => (
                AccountHealthState::HumanActionRequired,
                Some(waiting_user_reason(waiting)),
            ),
            AuthState::Idle | AuthState::Cancelled => (
                AccountHealthState::HumanActionRequired,
                Some(HumanRequiredReason::AuthRequired),
            ),
        };
        Self {
            provider_account_id: account.id,
            state,
            auth_state: account.auth_state.clone(),
            human_required_reason: reason,
            protocol_drift_execution_id: None,
            protocol_drift_at: None,
            observed_at: account.updated_at,
        }
    }
}

const fn waiting_user_reason(waiting: WaitingUserState) -> HumanRequiredReason {
    match waiting {
        WaitingUserState::CredentialInput => HumanRequiredReason::AuthRequired,
        WaitingUserState::QrScan | WaitingUserState::QrConfirm => HumanRequiredReason::QrRequired,
        WaitingUserState::BrowserCallback => HumanRequiredReason::BrowserCallbackRequired,
        WaitingUserState::SmsCode => HumanRequiredReason::SmsVerification,
        WaitingUserState::SessionImport => HumanRequiredReason::SessionImportRequired,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Course {
    pub id: CourseId,
    pub provider_account_id: ProviderAccountId,
    pub remote_id: String,
    pub title: String,
    pub term: Option<String>,
    pub teacher: Option<String>,
    pub remote_status: Option<String>,
    pub metadata: serde_json::Value,
    pub last_seen_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseRequiredProgress {
    pub required_task_count: u32,
    pub completed_required_task_count: u32,
    pub completion_millis: Option<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseDurationProgress {
    pub observed_seconds: u64,
    pub required_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseScoreProgress {
    pub scored_task_count: u32,
    pub average_score_millis: u16,
    pub last_verified_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseAggregateProgress {
    pub course_id: CourseId,
    pub provider_account_id: ProviderAccountId,
    pub total_task_count: u32,
    pub countable_task_count: u32,
    pub completed_task_count: u32,
    pub remaining_task_count: u32,
    pub not_open_task_count: u32,
    pub credit_blocked_task_count: u32,
    pub human_required_task_count: u32,
    pub failed_task_count: u32,
    pub completion_millis: Option<u16>,
    pub required: Option<CourseRequiredProgress>,
    pub duration: Option<CourseDurationProgress>,
    pub score: Option<CourseScoreProgress>,
    pub observed_at: Timestamp,
}

impl CourseAggregateProgress {
    /// Validates count partitions and bounded aggregate ratios.
    ///
    /// # Errors
    ///
    /// Returns [`CourseAggregateProgressError`] when a persisted aggregate is
    /// internally inconsistent or claims data absent from its denominator.
    pub fn validate(&self) -> Result<(), CourseAggregateProgressError> {
        if self.countable_task_count > self.total_task_count
            || self.completed_task_count > self.countable_task_count
            || self
                .completed_task_count
                .checked_add(self.remaining_task_count)
                != Some(self.countable_task_count)
            || [
                self.not_open_task_count,
                self.credit_blocked_task_count,
                self.human_required_task_count,
                self.failed_task_count,
            ]
            .into_iter()
            .any(|count| count > self.countable_task_count)
            || self.completion_millis
                != completion_millis(self.completed_task_count, self.countable_task_count)
        {
            return Err(CourseAggregateProgressError);
        }
        if let Some(required) = self.required
            && (required.completed_required_task_count > required.required_task_count
                || required.completion_millis
                    != completion_millis(
                        required.completed_required_task_count,
                        required.required_task_count,
                    ))
        {
            return Err(CourseAggregateProgressError);
        }
        if self.score.is_some_and(|score| {
            score.scored_task_count == 0
                || score.scored_task_count > self.countable_task_count
                || score.average_score_millis > 1_000
                || score.last_verified_at > self.observed_at
        }) {
            return Err(CourseAggregateProgressError);
        }
        Ok(())
    }
}

fn completion_millis(completed: u32, total: u32) -> Option<u16> {
    (total > 0).then(|| {
        u16::try_from(u64::from(completed) * 1_000 / u64::from(total))
            .expect("bounded course completion ratio fits u16")
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("course aggregate progress is internally inconsistent")]
pub struct CourseAggregateProgressError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_is_stable_for_routes_and_storage() {
        assert!(ProviderId::new("unipus-ai").is_ok());
        assert!(ProviderId::new("UCampus").is_err());
        assert!(ProviderId::new("with space").is_err());
    }

    #[test]
    fn course_progress_preserves_unknown_dimensions_and_exact_counts() {
        let now = chrono::Utc::now();
        let progress = CourseAggregateProgress {
            course_id: CourseId::new(),
            provider_account_id: ProviderAccountId::new(),
            total_task_count: 5,
            countable_task_count: 4,
            completed_task_count: 1,
            remaining_task_count: 3,
            not_open_task_count: 1,
            credit_blocked_task_count: 1,
            human_required_task_count: 1,
            failed_task_count: 0,
            completion_millis: Some(250),
            required: None,
            duration: None,
            score: Some(CourseScoreProgress {
                scored_task_count: 1,
                average_score_millis: 800,
                last_verified_at: now,
            }),
            observed_at: now,
        };
        assert!(progress.validate().is_ok());
        let mut invalid = progress;
        invalid.remaining_task_count = 4;
        assert_eq!(invalid.validate(), Err(CourseAggregateProgressError));
    }

    #[test]
    fn account_health_prefers_fresh_protocol_drift_over_authentication_health() {
        let now = chrono::Utc::now();
        let account = ProviderAccount {
            id: ProviderAccountId::new(),
            owner_id: UserId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "Account".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        assert_eq!(
            AccountHealth::from_account(&account, None).state,
            AccountHealthState::Healthy
        );
        let execution_id = ExecutionId::new();
        let drift_at = now + chrono::Duration::seconds(1);
        let health = AccountHealth::from_account(&account, Some((execution_id, drift_at)));
        assert_eq!(health.state, AccountHealthState::ProtocolChanged);
        assert_eq!(health.protocol_drift_execution_id, Some(execution_id));

        let stale = AccountHealth::from_account(
            &account,
            Some((ExecutionId::new(), now - chrono::Duration::seconds(1))),
        );
        assert_eq!(stale.state, AccountHealthState::Healthy);
    }
}
