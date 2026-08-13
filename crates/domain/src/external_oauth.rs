use serde::{Deserialize, Serialize};

use crate::{AuthSessionId, ProviderAccountId, ProviderId, Timestamp, UserId};

pub const MAX_EXTERNAL_OAUTH_TTL_SECONDS: i64 = 15 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalOauthState {
    Pending,
    Completing,
    Succeeded,
    Failed,
    Ambiguous,
    Expired,
    Cancelled,
}

/// Durable hash-only binding for one external OAuth callback. Callback URLs,
/// authorization codes, states and Provider context values are never stored.
#[derive(Clone, Eq, PartialEq)]
pub struct ExternalOauthPending {
    pub auth_session_id: AuthSessionId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub provider_id: ProviderId,
    pub state_digest: [u8; 32],
    pub provider_context_digest: [u8; 32],
    pub state: ExternalOauthState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub consumed_at: Option<Timestamp>,
    pub revision: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalOauthPendingCreate {
    pub auth_session_id: AuthSessionId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub provider_id: ProviderId,
    pub state_digest: [u8; 32],
    pub provider_context_digest: [u8; 32],
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

impl ExternalOauthPending {
    /// Creates a short-lived callback binding containing only digests.
    ///
    /// # Errors
    ///
    /// Rejects invalid digests, lifecycle timestamps, or TTLs.
    pub fn pending(create: ExternalOauthPendingCreate) -> Result<Self, ExternalOauthPendingError> {
        let pending = Self {
            auth_session_id: create.auth_session_id,
            owner_user_id: create.owner_user_id,
            provider_account_id: create.provider_account_id,
            provider_id: create.provider_id,
            state_digest: create.state_digest,
            provider_context_digest: create.provider_context_digest,
            state: ExternalOauthState::Pending,
            created_at: create.created_at,
            updated_at: create.created_at,
            expires_at: create.expires_at,
            consumed_at: None,
            revision: 1,
        };
        pending.validate()?;
        Ok(pending)
    }

    /// Atomically claims the single-use callback before any Provider call.
    ///
    /// # Errors
    ///
    /// Rejects expired, non-pending, or timestamp-regressing callbacks.
    pub fn claim(&mut self, at: Timestamp) -> Result<(), ExternalOauthPendingError> {
        self.require_timestamp(at)?;
        if self.state != ExternalOauthState::Pending {
            return Err(ExternalOauthPendingError::InvalidTransition);
        }
        if at >= self.expires_at {
            return Err(ExternalOauthPendingError::Expired);
        }
        self.advance(ExternalOauthState::Completing, at)?;
        self.consumed_at = Some(at);
        Ok(())
    }

    /// Records the terminal result of the one-shot Provider exchange. Failed
    /// and ambiguous results remain consumed and cannot be reopened.
    ///
    /// # Errors
    ///
    /// Rejects non-completing records, non-terminal outcomes, and timestamp
    /// regression.
    pub fn finish(
        &mut self,
        state: ExternalOauthState,
        at: Timestamp,
    ) -> Result<(), ExternalOauthPendingError> {
        self.require_timestamp(at)?;
        if self.state != ExternalOauthState::Completing
            || !matches!(
                state,
                ExternalOauthState::Succeeded
                    | ExternalOauthState::Failed
                    | ExternalOauthState::Ambiguous
            )
        {
            return Err(ExternalOauthPendingError::InvalidTransition);
        }
        self.advance(state, at)
    }

    /// Expires an unconsumed callback at or after its deadline.
    ///
    /// # Errors
    ///
    /// Rejects early, non-pending, or timestamp-regressing expiry.
    pub fn expire(&mut self, at: Timestamp) -> Result<(), ExternalOauthPendingError> {
        self.require_timestamp(at)?;
        if self.state != ExternalOauthState::Pending {
            return Err(ExternalOauthPendingError::InvalidTransition);
        }
        if at < self.expires_at {
            return Err(ExternalOauthPendingError::NotExpired);
        }
        self.advance(ExternalOauthState::Expired, at)
    }

    /// Cancels an unconsumed live callback.
    ///
    /// # Errors
    ///
    /// Rejects non-pending or timestamp-regressing cancellation.
    pub fn cancel(&mut self, at: Timestamp) -> Result<(), ExternalOauthPendingError> {
        self.require_timestamp(at)?;
        if self.state != ExternalOauthState::Pending {
            return Err(ExternalOauthPendingError::InvalidTransition);
        }
        self.advance(ExternalOauthState::Cancelled, at)
    }

    /// Validates the complete persisted record shape.
    ///
    /// # Errors
    ///
    /// Rejects invalid bindings, timestamps, TTLs, revisions, or lifecycle
    /// metadata.
    pub fn validate(&self) -> Result<(), ExternalOauthPendingError> {
        let ttl = self.expires_at.signed_duration_since(self.created_at);
        if ttl.num_seconds() <= 0 || ttl.num_seconds() > MAX_EXTERNAL_OAUTH_TTL_SECONDS {
            return Err(ExternalOauthPendingError::InvalidExpiry);
        }
        if self.state_digest == [0; 32]
            || self.provider_context_digest == [0; 32]
            || self.state_digest == self.provider_context_digest
        {
            return Err(ExternalOauthPendingError::InvalidDigest);
        }
        if self.updated_at < self.created_at {
            return Err(ExternalOauthPendingError::TimestampRegression);
        }
        if self
            .consumed_at
            .is_some_and(|at| at < self.created_at || at >= self.expires_at || at > self.updated_at)
        {
            return Err(ExternalOauthPendingError::InvalidRecord);
        }
        let valid_shape = match (self.state, self.consumed_at, self.revision) {
            (ExternalOauthState::Pending, None, 1) => self.updated_at == self.created_at,
            (ExternalOauthState::Completing, Some(at), 2) => self.updated_at == at,
            (
                ExternalOauthState::Succeeded
                | ExternalOauthState::Failed
                | ExternalOauthState::Ambiguous,
                Some(_),
                3,
            )
            | (ExternalOauthState::Expired | ExternalOauthState::Cancelled, None, 2) => true,
            _ => false,
        };
        valid_shape
            .then_some(())
            .ok_or(ExternalOauthPendingError::InvalidRecord)
    }

    fn require_timestamp(&self, at: Timestamp) -> Result<(), ExternalOauthPendingError> {
        if at < self.updated_at {
            Err(ExternalOauthPendingError::TimestampRegression)
        } else {
            Ok(())
        }
    }

    fn advance(
        &mut self,
        state: ExternalOauthState,
        at: Timestamp,
    ) -> Result<(), ExternalOauthPendingError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ExternalOauthPendingError::RevisionExhausted)?;
        self.state = state;
        self.updated_at = at;
        Ok(())
    }
}

impl std::fmt::Debug for ExternalOauthPending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalOauthPending")
            .field("auth_session_id", &self.auth_session_id)
            .field("owner_user_id", &self.owner_user_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("provider_id", &self.provider_id)
            .field("state_digest", &"[HASHED]")
            .field("provider_context_digest", &"[HASHED]")
            .field("state", &self.state)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("expires_at", &self.expires_at)
            .field("consumed_at", &self.consumed_at)
            .field("revision", &self.revision)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExternalOauthPendingError {
    #[error("external OAuth pending expiry is outside the short TTL limit")]
    InvalidExpiry,
    #[error("external OAuth callback binding digest is invalid")]
    InvalidDigest,
    #[error("external OAuth pending timestamp moved backwards")]
    TimestampRegression,
    #[error("external OAuth pending callback has expired")]
    Expired,
    #[error("external OAuth pending callback has not expired")]
    NotExpired,
    #[error("external OAuth pending transition is invalid")]
    InvalidTransition,
    #[error("external OAuth pending revision is exhausted")]
    RevisionExhausted,
    #[error("external OAuth pending record is invalid")]
    InvalidRecord,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn pending(now: Timestamp) -> ExternalOauthPending {
        ExternalOauthPending::pending(ExternalOauthPendingCreate {
            auth_session_id: AuthSessionId::new(),
            owner_user_id: UserId::new(),
            provider_account_id: ProviderAccountId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            state_digest: [1; 32],
            provider_context_digest: [2; 32],
            created_at: now,
            expires_at: now + Duration::minutes(5),
        })
        .unwrap()
    }

    #[test]
    fn callback_claims_once_and_ambiguous_result_stays_consumed() {
        let now = Utc::now();
        let mut pending = pending(now);
        pending.claim(now + Duration::seconds(1)).unwrap();
        assert_eq!(pending.state, ExternalOauthState::Completing);
        assert_eq!(
            pending.claim(now + Duration::seconds(2)),
            Err(ExternalOauthPendingError::InvalidTransition)
        );
        pending
            .finish(ExternalOauthState::Ambiguous, now + Duration::seconds(2))
            .unwrap();
        assert_eq!(pending.state, ExternalOauthState::Ambiguous);
        assert!(pending.consumed_at.is_some());
        pending.validate().unwrap();
    }

    #[test]
    fn invalid_digests_ttl_and_early_expiry_fail_closed() {
        let now = Utc::now();
        let invalid = ExternalOauthPending::pending(ExternalOauthPendingCreate {
            auth_session_id: AuthSessionId::new(),
            owner_user_id: UserId::new(),
            provider_account_id: ProviderAccountId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            state_digest: [0; 32],
            provider_context_digest: [2; 32],
            created_at: now,
            expires_at: now + Duration::minutes(5),
        });
        assert_eq!(invalid, Err(ExternalOauthPendingError::InvalidDigest));

        let mut pending = pending(now);
        assert_eq!(
            pending.expire(now + Duration::minutes(4)),
            Err(ExternalOauthPendingError::NotExpired)
        );
        pending.expire(now + Duration::minutes(5)).unwrap();
        pending.validate().unwrap();
    }
}
