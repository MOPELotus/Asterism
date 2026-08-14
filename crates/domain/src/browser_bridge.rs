use serde::{Deserialize, Serialize};

use crate::{BrowserBridgeSessionId, ProviderAccountId, ProviderId, TaskId, Timestamp, UserId};

const MAX_BROWSER_BRIDGE_ORIGIN_BYTES: usize = 256;
const MAX_BROWSER_BRIDGE_FRAME_ID_BYTES: usize = 256;

pub const MAX_BROWSER_BRIDGE_SESSION_TTL_SECONDS: i64 = 12 * 60 * 60;
const MAX_PROVIDER_VERSION_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserBridgeSessionState {
    AwaitingClaim,
    Claimed,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

/// Immutable browser document identity observed by one claimed helper.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrowserBridgeRuntimeBinding {
    pub session_id: BrowserBridgeSessionId,
    pub observed_origin: String,
    pub frame_id: String,
    pub bound_at: Timestamp,
}

impl BrowserBridgeRuntimeBinding {
    /// Validates bounded credential-free runtime identity syntax. Storage also
    /// requires the origin to belong to the frozen session specification.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTPS origins and unsafe or unbounded frame identifiers.
    pub fn validate(&self) -> Result<(), BrowserBridgeRuntimeBindingError> {
        let authority = self
            .observed_origin
            .strip_prefix("https://")
            .ok_or(BrowserBridgeRuntimeBindingError::Invalid)?;
        if self.observed_origin.len() > MAX_BROWSER_BRIDGE_ORIGIN_BYTES
            || authority.is_empty()
            || !self.observed_origin.is_ascii()
            || authority
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'?' | b'#' | b'@'))
            || self.frame_id.is_empty()
            || self.frame_id.len() > MAX_BROWSER_BRIDGE_FRAME_ID_BYTES
            || !self.frame_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            Err(BrowserBridgeRuntimeBindingError::Invalid)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BrowserBridgeRuntimeBindingError {
    #[error("BrowserBridge runtime origin or frame binding is invalid")]
    Invalid,
}

/// Durable, credential-free binding for one `BrowserBridge` helper session.
/// Provider commands, browser credentials, DOM data and remote response
/// payloads are deliberately outside this lifecycle record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeSession {
    pub id: BrowserBridgeSessionId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub spec_version: u32,
    pub spec_digest: [u8; 32],
    pub state: BrowserBridgeSessionState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub claimed_at: Option<Timestamp>,
    pub revision: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeSessionCreate {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub spec_version: u32,
    pub spec_digest: [u8; 32],
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

impl BrowserBridgeSession {
    /// Creates one short-lived `BrowserBridge` helper pairing.
    ///
    /// # Errors
    ///
    /// Rejects malformed Provider/spec identity or a TTL outside the bound.
    pub fn awaiting_claim(
        create: BrowserBridgeSessionCreate,
    ) -> Result<Self, BrowserBridgeSessionError> {
        let session = Self {
            id: BrowserBridgeSessionId::new(),
            owner_user_id: create.owner_user_id,
            provider_account_id: create.provider_account_id,
            task_id: create.task_id,
            provider_id: create.provider_id,
            provider_version: create.provider_version,
            spec_version: create.spec_version,
            spec_digest: create.spec_digest,
            state: BrowserBridgeSessionState::AwaitingClaim,
            created_at: create.created_at,
            updated_at: create.created_at,
            expires_at: create.expires_at,
            claimed_at: None,
            revision: 1,
        };
        session.validate()?;
        Ok(session)
    }

    /// Claims the helper session exactly once before expiry.
    ///
    /// # Errors
    ///
    /// Rejects expired, already claimed, terminal, or regressing transitions.
    pub fn claim(&mut self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        self.require_live_transition(at)?;
        if self.state != BrowserBridgeSessionState::AwaitingClaim {
            return Err(BrowserBridgeSessionError::InvalidTransition);
        }
        self.advance(BrowserBridgeSessionState::Claimed, at)?;
        self.claimed_at = Some(at);
        Ok(())
    }

    /// Records terminal success after a future typed result contract verifies.
    ///
    /// # Errors
    ///
    /// Rejects calls outside one live claimed session.
    pub fn complete(&mut self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        self.finish(BrowserBridgeSessionState::Completed, at)
    }

    /// Records a terminal helper failure without reopening the pairing token.
    ///
    /// # Errors
    ///
    /// Rejects calls outside one live claimed session.
    pub fn fail(&mut self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        self.finish(BrowserBridgeSessionState::Failed, at)
    }

    /// Cancels an unclaimed or claimed live helper session.
    ///
    /// # Errors
    ///
    /// Rejects terminal, expired, or timestamp-regressing transitions.
    pub fn cancel(&mut self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        self.require_live_transition(at)?;
        if !matches!(
            self.state,
            BrowserBridgeSessionState::AwaitingClaim | BrowserBridgeSessionState::Claimed
        ) {
            return Err(BrowserBridgeSessionError::InvalidTransition);
        }
        self.advance(BrowserBridgeSessionState::Cancelled, at)
    }

    /// Expires an unclaimed or claimed helper session at its fixed deadline.
    ///
    /// # Errors
    ///
    /// Rejects early, terminal, or timestamp-regressing transitions.
    pub fn expire(&mut self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        self.require_timestamp(at)?;
        if at < self.expires_at {
            return Err(BrowserBridgeSessionError::NotExpired);
        }
        if !matches!(
            self.state,
            BrowserBridgeSessionState::AwaitingClaim | BrowserBridgeSessionState::Claimed
        ) {
            return Err(BrowserBridgeSessionError::InvalidTransition);
        }
        self.advance(BrowserBridgeSessionState::Expired, at)
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }

    /// Validates the complete persisted record independently of transition code.
    ///
    /// # Errors
    ///
    /// Rejects malformed identity, timestamps, revisions, or state shapes.
    pub fn validate(&self) -> Result<(), BrowserBridgeSessionError> {
        let ttl = self.expires_at.signed_duration_since(self.created_at);
        if ttl.num_seconds() <= 0 || ttl.num_seconds() > MAX_BROWSER_BRIDGE_SESSION_TTL_SECONDS {
            return Err(BrowserBridgeSessionError::InvalidExpiry);
        }
        if self.provider_version.is_empty()
            || self.provider_version.len() > MAX_PROVIDER_VERSION_BYTES
            || self.provider_version.trim() != self.provider_version
            || self.provider_version.chars().any(char::is_control)
        {
            return Err(BrowserBridgeSessionError::InvalidProviderVersion);
        }
        if self.spec_version == 0 || self.spec_digest == [0; 32] {
            return Err(BrowserBridgeSessionError::InvalidSpecBinding);
        }
        if self.updated_at < self.created_at {
            return Err(BrowserBridgeSessionError::TimestampRegression);
        }
        if self.claimed_at.is_some_and(|claimed_at| {
            claimed_at < self.created_at
                || claimed_at >= self.expires_at
                || claimed_at > self.updated_at
        }) {
            return Err(BrowserBridgeSessionError::InvalidRecord);
        }
        let valid_shape = match (self.state, self.claimed_at, self.revision) {
            (BrowserBridgeSessionState::AwaitingClaim, None, 1) => {
                self.updated_at == self.created_at
            }
            (BrowserBridgeSessionState::Claimed, Some(at), 2) => self.updated_at == at,
            (
                BrowserBridgeSessionState::Completed | BrowserBridgeSessionState::Failed,
                Some(_),
                3,
            ) => self.updated_at < self.expires_at,
            (BrowserBridgeSessionState::Cancelled, None, 2)
            | (BrowserBridgeSessionState::Cancelled, Some(_), 3) => {
                self.updated_at < self.expires_at
            }
            (BrowserBridgeSessionState::Expired, None, 2)
            | (BrowserBridgeSessionState::Expired, Some(_), 3) => {
                self.updated_at >= self.expires_at
            }
            _ => false,
        };
        valid_shape
            .then_some(())
            .ok_or(BrowserBridgeSessionError::InvalidRecord)
    }

    fn finish(
        &mut self,
        state: BrowserBridgeSessionState,
        at: Timestamp,
    ) -> Result<(), BrowserBridgeSessionError> {
        self.require_live_transition(at)?;
        if self.state != BrowserBridgeSessionState::Claimed {
            return Err(BrowserBridgeSessionError::InvalidTransition);
        }
        self.advance(state, at)
    }

    fn require_live_transition(&self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        self.require_timestamp(at)?;
        if self.is_expired_at(at) {
            Err(BrowserBridgeSessionError::Expired)
        } else {
            Ok(())
        }
    }

    fn require_timestamp(&self, at: Timestamp) -> Result<(), BrowserBridgeSessionError> {
        if at < self.updated_at {
            Err(BrowserBridgeSessionError::TimestampRegression)
        } else {
            Ok(())
        }
    }

    fn advance(
        &mut self,
        state: BrowserBridgeSessionState,
        at: Timestamp,
    ) -> Result<(), BrowserBridgeSessionError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(BrowserBridgeSessionError::RevisionExhausted)?;
        self.state = state;
        self.updated_at = at;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BrowserBridgeSessionError {
    #[error("BrowserBridge session expiry is outside the short TTL limit")]
    InvalidExpiry,
    #[error("BrowserBridge Provider version is invalid")]
    InvalidProviderVersion,
    #[error("BrowserBridge session specification binding is invalid")]
    InvalidSpecBinding,
    #[error("BrowserBridge session timestamp moved backwards")]
    TimestampRegression,
    #[error("BrowserBridge session has expired")]
    Expired,
    #[error("BrowserBridge session has not expired")]
    NotExpired,
    #[error("BrowserBridge session transition is invalid")]
    InvalidTransition,
    #[error("BrowserBridge session revision is exhausted")]
    RevisionExhausted,
    #[error("BrowserBridge persisted record is invalid")]
    InvalidRecord,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn runtime_binding_requires_exact_https_origin_and_safe_frame() {
        let now = Utc::now();
        let binding = BrowserBridgeRuntimeBinding {
            session_id: BrowserBridgeSessionId::new(),
            observed_origin: "https://provider.example".to_owned(),
            frame_id: "top-frame:1".to_owned(),
            bound_at: now,
        };
        assert_eq!(binding.validate(), Ok(()));

        let mut routed_origin = binding.clone();
        routed_origin.observed_origin.push_str("/task");
        assert_eq!(
            routed_origin.validate(),
            Err(BrowserBridgeRuntimeBindingError::Invalid)
        );

        let mut unsafe_frame = binding;
        unsafe_frame.frame_id = "top frame".to_owned();
        assert_eq!(
            unsafe_frame.validate(),
            Err(BrowserBridgeRuntimeBindingError::Invalid)
        );
    }

    #[test]
    fn helper_claims_once_and_terminal_states_never_reopen() {
        let now = Utc::now();
        let mut session = session(now);
        session.claim(now + Duration::seconds(1)).unwrap();
        assert_eq!(session.state, BrowserBridgeSessionState::Claimed);
        assert_eq!(
            session.claim(now + Duration::seconds(2)),
            Err(BrowserBridgeSessionError::InvalidTransition)
        );
        session.complete(now + Duration::seconds(3)).unwrap();
        assert_eq!(session.state, BrowserBridgeSessionState::Completed);
        assert_eq!(
            session.cancel(now + Duration::seconds(4)),
            Err(BrowserBridgeSessionError::InvalidTransition)
        );
        session.validate().unwrap();
    }

    #[test]
    fn spec_identity_and_short_ttl_are_required() {
        let now = Utc::now();
        let mut invalid_digest = create(now);
        invalid_digest.spec_digest = [0; 32];
        assert_eq!(
            BrowserBridgeSession::awaiting_claim(invalid_digest),
            Err(BrowserBridgeSessionError::InvalidSpecBinding)
        );

        let mut invalid_ttl = create(now);
        invalid_ttl.expires_at =
            now + Duration::seconds(MAX_BROWSER_BRIDGE_SESSION_TTL_SECONDS + 1);
        assert_eq!(
            BrowserBridgeSession::awaiting_claim(invalid_ttl),
            Err(BrowserBridgeSessionError::InvalidExpiry)
        );
    }

    #[test]
    fn expiry_preserves_whether_the_helper_was_claimed() {
        let now = Utc::now();
        let mut unclaimed = session(now);
        unclaimed.expire(now + Duration::hours(1)).unwrap();
        assert_eq!(unclaimed.revision, 2);

        let mut claimed = session(now);
        claimed.claim(now + Duration::seconds(1)).unwrap();
        claimed.expire(now + Duration::hours(1)).unwrap();
        assert_eq!(claimed.revision, 3);
        claimed.validate().unwrap();
    }

    #[test]
    fn terminal_timestamp_shape_fails_closed() {
        let now = Utc::now();
        let mut cancelled = session(now);
        cancelled.cancel(now + Duration::seconds(1)).unwrap();
        cancelled.updated_at = cancelled.expires_at;
        assert_eq!(
            cancelled.validate(),
            Err(BrowserBridgeSessionError::InvalidRecord)
        );

        let mut expired = session(now);
        expired.expire(now + Duration::hours(1)).unwrap();
        expired.updated_at = expired.expires_at - Duration::seconds(1);
        assert_eq!(
            expired.validate(),
            Err(BrowserBridgeSessionError::InvalidRecord)
        );
    }

    fn session(now: Timestamp) -> BrowserBridgeSession {
        BrowserBridgeSession::awaiting_claim(create(now)).unwrap()
    }

    fn create(now: Timestamp) -> BrowserBridgeSessionCreate {
        BrowserBridgeSessionCreate {
            owner_user_id: UserId::new(),
            provider_account_id: ProviderAccountId::new(),
            task_id: TaskId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_version: "0.1.0".to_owned(),
            spec_version: 1,
            spec_digest: [1; 32],
            created_at: now,
            expires_at: now + Duration::hours(1),
        }
    }
}
