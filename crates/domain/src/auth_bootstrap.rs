use serde::{Deserialize, Serialize};

use crate::{AuthBootstrapSessionId, ProviderAccountId, ProviderId, Timestamp, UserId};

pub const MAX_AUTH_BOOTSTRAP_TTL_SECONDS: i64 = 15 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthBootstrapPurpose {
    AddAccount,
    Reauthenticate,
    RepairSession,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthBootstrapState {
    AwaitingClaim,
    Claimed,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthBootstrapSession {
    pub id: AuthBootstrapSessionId,
    pub owner_user_id: UserId,
    pub provider_id: ProviderId,
    pub provider_account_id: Option<ProviderAccountId>,
    pub purpose: AuthBootstrapPurpose,
    pub required_recipe_version: u32,
    pub state: AuthBootstrapState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub claimed_at: Option<Timestamp>,
    pub revision: u32,
}

impl AuthBootstrapSession {
    /// Creates one short-lived, unclaimed Capture pairing session.
    ///
    /// # Errors
    ///
    /// Rejects an invalid purpose/account binding, recipe version, or TTL.
    pub fn awaiting_claim(
        owner_user_id: UserId,
        provider_id: ProviderId,
        provider_account_id: Option<ProviderAccountId>,
        purpose: AuthBootstrapPurpose,
        required_recipe_version: u32,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<Self, AuthBootstrapSessionError> {
        let session = Self {
            id: AuthBootstrapSessionId::new(),
            owner_user_id,
            provider_id,
            provider_account_id,
            purpose,
            required_recipe_version,
            state: AuthBootstrapState::AwaitingClaim,
            created_at,
            updated_at: created_at,
            expires_at,
            claimed_at: None,
            revision: 1,
        };
        session.validate()?;
        Ok(session)
    }

    /// Claims the session exactly once before its expiry.
    ///
    /// # Errors
    ///
    /// Rejects expired, already claimed, terminal, or timestamp-regressing
    /// sessions.
    pub fn claim(&mut self, at: Timestamp) -> Result<(), AuthBootstrapSessionError> {
        self.require_live_transition(at)?;
        if self.state != AuthBootstrapState::AwaitingClaim {
            return Err(AuthBootstrapSessionError::InvalidTransition);
        }
        self.advance(AuthBootstrapState::Claimed, at)?;
        self.claimed_at = Some(at);
        Ok(())
    }

    /// Completes a claimed pairing after its credential flow succeeds and
    /// binds the account created or updated by Core.
    ///
    /// # Errors
    ///
    /// Rejects non-claimed, expired, or timestamp-regressing sessions.
    pub fn complete(
        &mut self,
        provider_account_id: ProviderAccountId,
        at: Timestamp,
    ) -> Result<(), AuthBootstrapSessionError> {
        self.require_live_transition(at)?;
        if self.state != AuthBootstrapState::Claimed {
            return Err(AuthBootstrapSessionError::InvalidTransition);
        }
        match self.purpose {
            AuthBootstrapPurpose::AddAccount if self.provider_account_id.is_none() => {}
            AuthBootstrapPurpose::Reauthenticate | AuthBootstrapPurpose::RepairSession
                if self.provider_account_id == Some(provider_account_id) => {}
            _ => return Err(AuthBootstrapSessionError::InvalidAccountBinding),
        }
        self.advance(AuthBootstrapState::Completed, at)?;
        self.provider_account_id = Some(provider_account_id);
        Ok(())
    }

    /// Marks a claimed pairing as failed without making it reusable.
    ///
    /// # Errors
    ///
    /// Rejects non-claimed, expired, or timestamp-regressing sessions.
    pub fn fail(&mut self, at: Timestamp) -> Result<(), AuthBootstrapSessionError> {
        self.finish(AuthBootstrapState::Failed, at)
    }

    /// Cancels an unclaimed or claimed live session.
    ///
    /// # Errors
    ///
    /// Rejects terminal, expired, or timestamp-regressing sessions.
    pub fn cancel(&mut self, at: Timestamp) -> Result<(), AuthBootstrapSessionError> {
        self.require_live_transition(at)?;
        if !matches!(
            self.state,
            AuthBootstrapState::AwaitingClaim | AuthBootstrapState::Claimed
        ) {
            return Err(AuthBootstrapSessionError::InvalidTransition);
        }
        self.advance(AuthBootstrapState::Cancelled, at)
    }

    /// Expires a live session once its deadline has passed.
    ///
    /// # Errors
    ///
    /// Rejects early expiry, terminal states, or timestamp regression.
    pub fn expire(&mut self, at: Timestamp) -> Result<(), AuthBootstrapSessionError> {
        self.require_timestamp(at)?;
        if at < self.expires_at {
            return Err(AuthBootstrapSessionError::SessionNotExpired);
        }
        if !matches!(
            self.state,
            AuthBootstrapState::AwaitingClaim | AuthBootstrapState::Claimed
        ) {
            return Err(AuthBootstrapSessionError::InvalidTransition);
        }
        self.advance(AuthBootstrapState::Expired, at)
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }

    /// Validates the persisted session shape independently of transition code.
    ///
    /// # Errors
    ///
    /// Rejects invalid bindings, TTLs, timestamps, claim metadata, and revision
    /// shapes.
    pub fn validate(&self) -> Result<(), AuthBootstrapSessionError> {
        validate_account_binding(self.purpose, self.state, self.provider_account_id)?;
        if self.required_recipe_version == 0 {
            return Err(AuthBootstrapSessionError::InvalidRecipeVersion);
        }
        let ttl = self.expires_at.signed_duration_since(self.created_at);
        if ttl.num_seconds() <= 0 || ttl.num_seconds() > MAX_AUTH_BOOTSTRAP_TTL_SECONDS {
            return Err(AuthBootstrapSessionError::InvalidExpiry);
        }
        if self.updated_at < self.created_at {
            return Err(AuthBootstrapSessionError::TimestampRegression);
        }
        if self.claimed_at.is_some_and(|claimed_at| {
            claimed_at < self.created_at
                || claimed_at >= self.expires_at
                || claimed_at > self.updated_at
        }) {
            return Err(AuthBootstrapSessionError::InvalidRecord);
        }
        let valid_shape = match (self.state, self.claimed_at) {
            (AuthBootstrapState::AwaitingClaim, None) => {
                self.revision == 1 && self.updated_at == self.created_at
            }
            (AuthBootstrapState::Claimed, Some(claimed_at)) => {
                self.revision == 2 && self.updated_at == claimed_at
            }
            (AuthBootstrapState::Completed | AuthBootstrapState::Failed, Some(_)) => {
                self.revision == 3 && self.updated_at < self.expires_at
            }
            (AuthBootstrapState::Cancelled | AuthBootstrapState::Expired, None) => {
                self.revision == 2
            }
            (AuthBootstrapState::Cancelled | AuthBootstrapState::Expired, Some(_)) => {
                self.revision == 3
            }
            _ => false,
        };
        if valid_shape {
            Ok(())
        } else {
            Err(AuthBootstrapSessionError::InvalidRecord)
        }
    }

    fn finish(
        &mut self,
        next: AuthBootstrapState,
        at: Timestamp,
    ) -> Result<(), AuthBootstrapSessionError> {
        self.require_live_transition(at)?;
        if self.state != AuthBootstrapState::Claimed {
            return Err(AuthBootstrapSessionError::InvalidTransition);
        }
        self.advance(next, at)
    }

    fn require_live_transition(&self, at: Timestamp) -> Result<(), AuthBootstrapSessionError> {
        self.require_timestamp(at)?;
        if self.is_expired_at(at) {
            Err(AuthBootstrapSessionError::SessionExpired)
        } else {
            Ok(())
        }
    }

    fn require_timestamp(&self, at: Timestamp) -> Result<(), AuthBootstrapSessionError> {
        if at < self.updated_at {
            Err(AuthBootstrapSessionError::TimestampRegression)
        } else {
            Ok(())
        }
    }

    fn advance(
        &mut self,
        next: AuthBootstrapState,
        at: Timestamp,
    ) -> Result<(), AuthBootstrapSessionError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(AuthBootstrapSessionError::RevisionExhausted)?;
        self.state = next;
        self.updated_at = at;
        Ok(())
    }
}

fn validate_account_binding(
    purpose: AuthBootstrapPurpose,
    state: AuthBootstrapState,
    provider_account_id: Option<ProviderAccountId>,
) -> Result<(), AuthBootstrapSessionError> {
    let valid = match purpose {
        AuthBootstrapPurpose::AddAccount => {
            if state == AuthBootstrapState::Completed {
                provider_account_id.is_some()
            } else {
                provider_account_id.is_none()
            }
        }
        AuthBootstrapPurpose::Reauthenticate | AuthBootstrapPurpose::RepairSession => {
            provider_account_id.is_some()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(AuthBootstrapSessionError::InvalidAccountBinding)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthBootstrapSessionError {
    #[error("authentication bootstrap purpose does not match its account binding")]
    InvalidAccountBinding,
    #[error("authentication bootstrap recipe version must be positive")]
    InvalidRecipeVersion,
    #[error("authentication bootstrap expiry must be within the short TTL limit")]
    InvalidExpiry,
    #[error("authentication bootstrap timestamp moved backwards")]
    TimestampRegression,
    #[error("authentication bootstrap session has expired")]
    SessionExpired,
    #[error("authentication bootstrap session has not reached expiry")]
    SessionNotExpired,
    #[error("authentication bootstrap state transition is invalid")]
    InvalidTransition,
    #[error("authentication bootstrap revision is exhausted")]
    RevisionExhausted,
    #[error("authentication bootstrap persisted record is invalid")]
    InvalidRecord,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn pairing_claims_once_and_completes_without_becoming_reusable() {
        let now = Utc::now();
        let mut session = session(
            now,
            AuthBootstrapPurpose::Reauthenticate,
            Some(ProviderAccountId::new()),
        );

        session.claim(now + Duration::seconds(1)).unwrap();
        assert_eq!(session.state, AuthBootstrapState::Claimed);
        assert_eq!(session.revision, 2);
        assert_eq!(
            session.claim(now + Duration::seconds(2)),
            Err(AuthBootstrapSessionError::InvalidTransition)
        );
        session
            .complete(
                session.provider_account_id.unwrap(),
                now + Duration::seconds(3),
            )
            .unwrap();
        assert_eq!(session.state, AuthBootstrapState::Completed);
        assert_eq!(session.revision, 3);
        assert_eq!(
            session.cancel(now + Duration::seconds(4)),
            Err(AuthBootstrapSessionError::InvalidTransition)
        );
        session.validate().unwrap();
    }

    #[test]
    fn purpose_binding_recipe_version_and_short_ttl_are_enforced() {
        let now = Utc::now();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        assert_eq!(
            AuthBootstrapSession::awaiting_claim(
                UserId::new(),
                provider_id.clone(),
                Some(ProviderAccountId::new()),
                AuthBootstrapPurpose::AddAccount,
                1,
                now,
                now + Duration::minutes(10),
            ),
            Err(AuthBootstrapSessionError::InvalidAccountBinding)
        );
        assert_eq!(
            AuthBootstrapSession::awaiting_claim(
                UserId::new(),
                provider_id.clone(),
                None,
                AuthBootstrapPurpose::AddAccount,
                0,
                now,
                now + Duration::minutes(10),
            ),
            Err(AuthBootstrapSessionError::InvalidRecipeVersion)
        );
        assert_eq!(
            AuthBootstrapSession::awaiting_claim(
                UserId::new(),
                provider_id,
                None,
                AuthBootstrapPurpose::AddAccount,
                1,
                now,
                now + Duration::seconds(MAX_AUTH_BOOTSTRAP_TTL_SECONDS + 1),
            ),
            Err(AuthBootstrapSessionError::InvalidExpiry)
        );
    }

    #[test]
    fn expired_pairing_cannot_be_claimed_and_expires_only_after_deadline() {
        let now = Utc::now();
        let mut session = session(now, AuthBootstrapPurpose::AddAccount, None);
        assert_eq!(
            session.expire(now + Duration::minutes(9)),
            Err(AuthBootstrapSessionError::SessionNotExpired)
        );
        assert_eq!(
            session.claim(now + Duration::minutes(10)),
            Err(AuthBootstrapSessionError::SessionExpired)
        );
        session.expire(now + Duration::minutes(10)).unwrap();
        assert_eq!(session.state, AuthBootstrapState::Expired);
        assert_eq!(session.revision, 2);
        session.validate().unwrap();
    }

    #[test]
    fn add_account_binds_only_when_core_completes_the_claimed_session() {
        let now = Utc::now();
        let mut session = session(now, AuthBootstrapPurpose::AddAccount, None);
        let account_id = ProviderAccountId::new();
        session.claim(now + Duration::seconds(1)).unwrap();
        assert_eq!(session.provider_account_id, None);

        session
            .complete(account_id, now + Duration::seconds(2))
            .unwrap();
        assert_eq!(session.state, AuthBootstrapState::Completed);
        assert_eq!(session.provider_account_id, Some(account_id));
        assert_eq!(session.revision, 3);
        session.validate().unwrap();
    }

    fn session(
        now: Timestamp,
        purpose: AuthBootstrapPurpose,
        provider_account_id: Option<ProviderAccountId>,
    ) -> AuthBootstrapSession {
        AuthBootstrapSession::awaiting_claim(
            UserId::new(),
            ProviderId::new("provider-alpha").unwrap(),
            provider_account_id,
            purpose,
            3,
            now,
            now + Duration::minutes(10),
        )
        .unwrap()
    }
}
