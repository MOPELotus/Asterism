use serde::{Deserialize, Serialize};

use crate::{
    ExecutionId, ProviderAccountId, ProviderId, QuestionSessionId, QuestionSnapshotId, TaskId,
    Timestamp, UserId,
};

const MAX_ARTIFACT_TYPE_BYTES: usize = 96;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;

/// Maximum lifetime of provider runtime material attached to a Question read.
///
/// Providers may choose a shorter deadline when the remote attempt, crypto
/// context or browser state expires sooner.
pub const MAX_QUESTION_SESSION_TTL_SECONDS: i64 = 24 * 60 * 60;

/// Durable lifecycle of one provider attempt that produced an immutable
/// `QuestionSnapshot`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionSessionState {
    Active,
    Claimed,
    Consumed,
    Cancelled,
    Expired,
}

/// Credential-free identity and lifecycle metadata for provider-owned runtime
/// material required to answer or submit one immutable Question snapshot.
///
/// Provider payloads and secrets are encrypted outside Domain. Their bounded
/// type and digest are retained here so an Execution cannot substitute another
/// attempt, account, Task or payload after a Draft has been reviewed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuestionSession {
    pub id: QuestionSessionId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub question_snapshot_id: QuestionSnapshotId,
    pub artifact_type: String,
    pub artifact_digest: [u8; 32],
    pub state: QuestionSessionState,
    pub execution_id: Option<ExecutionId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub claimed_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub revision: u32,
}

impl QuestionSession {
    /// Creates one short-lived session already bound to its immutable Question
    /// snapshot and provider artifact digest.
    ///
    /// # Errors
    ///
    /// Rejects malformed type/version metadata, a zero digest or an invalid
    /// TTL.
    #[allow(clippy::too_many_arguments)]
    pub fn active(
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        task_id: TaskId,
        provider_id: ProviderId,
        provider_version: String,
        question_snapshot_id: QuestionSnapshotId,
        artifact_type: String,
        artifact_digest: [u8; 32],
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<Self, QuestionSessionError> {
        let session = Self {
            id: QuestionSessionId::new(),
            owner_user_id,
            provider_account_id,
            task_id,
            provider_id,
            provider_version,
            question_snapshot_id,
            artifact_type,
            artifact_digest,
            state: QuestionSessionState::Active,
            execution_id: None,
            created_at,
            updated_at: created_at,
            expires_at,
            claimed_at: None,
            closed_at: None,
            revision: 1,
        };
        session.validate()?;
        Ok(session)
    }

    /// Irreversibly binds this session to one Execution before provider
    /// material can be resolved or any mutation can be issued.
    ///
    /// # Errors
    ///
    /// Rejects expired, already claimed, terminal or timestamp-regressing
    /// sessions.
    pub fn claim(
        &mut self,
        execution_id: ExecutionId,
        at: Timestamp,
    ) -> Result<(), QuestionSessionError> {
        self.require_timestamp(at)?;
        if self.state != QuestionSessionState::Active {
            return Err(QuestionSessionError::InvalidTransition);
        }
        if self.is_expired_at(at) {
            return Err(QuestionSessionError::SessionExpired);
        }
        self.advance(QuestionSessionState::Claimed, at)?;
        self.execution_id = Some(execution_id);
        self.claimed_at = Some(at);
        Ok(())
    }

    /// Marks provider material consumed by its bound Execution. This may be
    /// recorded after the local deadline when the remote mutation was issued
    /// before expiry and persistence or receipt handling completed later.
    ///
    /// # Errors
    ///
    /// Rejects unclaimed, terminal or timestamp-regressing sessions.
    pub fn consume(&mut self, at: Timestamp) -> Result<(), QuestionSessionError> {
        self.finish_claimed(QuestionSessionState::Consumed, at)
    }

    /// Cancels a live session without making its artifact reusable. A claimed
    /// session retains its immutable Execution binding for audit and recovery.
    ///
    /// # Errors
    ///
    /// Rejects terminal or timestamp-regressing sessions.
    pub fn cancel(&mut self, at: Timestamp) -> Result<(), QuestionSessionError> {
        self.require_timestamp(at)?;
        if !matches!(
            self.state,
            QuestionSessionState::Active | QuestionSessionState::Claimed
        ) {
            return Err(QuestionSessionError::InvalidTransition);
        }
        self.advance(QuestionSessionState::Cancelled, at)?;
        self.closed_at = Some(at);
        Ok(())
    }

    /// Expires an unclaimed session after its deadline. Claimed sessions stay
    /// bound for durable recovery and must be consumed or explicitly cancelled.
    ///
    /// # Errors
    ///
    /// Rejects early expiry, claimed/terminal sessions or timestamp regression.
    pub fn expire(&mut self, at: Timestamp) -> Result<(), QuestionSessionError> {
        self.require_timestamp(at)?;
        if self.state != QuestionSessionState::Active {
            return Err(QuestionSessionError::InvalidTransition);
        }
        if !self.is_expired_at(at) {
            return Err(QuestionSessionError::SessionNotExpired);
        }
        self.advance(QuestionSessionState::Expired, at)?;
        self.closed_at = Some(at);
        Ok(())
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }

    /// Validates an independently loaded persisted record.
    ///
    /// # Errors
    ///
    /// Rejects malformed metadata, TTL, timestamps, revision or state fields.
    pub fn validate(&self) -> Result<(), QuestionSessionError> {
        validate_label(&self.artifact_type, MAX_ARTIFACT_TYPE_BYTES)
            .map_err(|()| QuestionSessionError::InvalidArtifactType)?;
        if self.provider_version.is_empty()
            || self.provider_version.len() > MAX_PROVIDER_VERSION_BYTES
            || self
                .provider_version
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(QuestionSessionError::InvalidProviderVersion);
        }
        if self.artifact_digest == [0; 32] {
            return Err(QuestionSessionError::InvalidArtifactDigest);
        }
        let ttl = self.expires_at.signed_duration_since(self.created_at);
        if ttl.num_seconds() <= 0 || ttl.num_seconds() > MAX_QUESTION_SESSION_TTL_SECONDS {
            return Err(QuestionSessionError::InvalidExpiry);
        }
        if self.updated_at < self.created_at
            || self.claimed_at.is_some_and(|at| {
                at < self.created_at || at >= self.expires_at || at > self.updated_at
            })
            || self
                .closed_at
                .is_some_and(|at| at < self.created_at || at > self.updated_at)
        {
            return Err(QuestionSessionError::TimestampRegression);
        }

        let valid_shape = match (
            self.state,
            self.execution_id,
            self.claimed_at,
            self.closed_at,
        ) {
            (QuestionSessionState::Active, None, None, None) => {
                self.revision == 1 && self.updated_at == self.created_at
            }
            (QuestionSessionState::Claimed, Some(_), Some(claimed_at), None) => {
                self.revision == 2 && self.updated_at == claimed_at
            }
            (
                QuestionSessionState::Consumed | QuestionSessionState::Cancelled,
                Some(_),
                Some(_),
                Some(closed_at),
            ) => self.revision == 3 && self.updated_at == closed_at,
            (QuestionSessionState::Cancelled, None, None, Some(closed_at)) => {
                self.revision == 2 && self.updated_at == closed_at
            }
            (QuestionSessionState::Expired, None, None, Some(closed_at)) => {
                self.revision == 2 && self.updated_at == closed_at && closed_at >= self.expires_at
            }
            _ => false,
        };
        if valid_shape {
            Ok(())
        } else {
            Err(QuestionSessionError::InvalidRecord)
        }
    }

    fn finish_claimed(
        &mut self,
        next: QuestionSessionState,
        at: Timestamp,
    ) -> Result<(), QuestionSessionError> {
        self.require_timestamp(at)?;
        if self.state != QuestionSessionState::Claimed {
            return Err(QuestionSessionError::InvalidTransition);
        }
        self.advance(next, at)?;
        self.closed_at = Some(at);
        Ok(())
    }

    fn require_timestamp(&self, at: Timestamp) -> Result<(), QuestionSessionError> {
        if at < self.updated_at {
            Err(QuestionSessionError::TimestampRegression)
        } else {
            Ok(())
        }
    }

    fn advance(
        &mut self,
        next: QuestionSessionState,
        at: Timestamp,
    ) -> Result<(), QuestionSessionError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(QuestionSessionError::RevisionExhausted)?;
        self.state = next;
        self.updated_at = at;
        Ok(())
    }
}

fn validate_label(value: &str, max_bytes: usize) -> Result<(), ()> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        Err(())
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuestionSessionError {
    #[error("Question session artifact type is invalid or unbounded")]
    InvalidArtifactType,
    #[error("Question session provider version is invalid or unbounded")]
    InvalidProviderVersion,
    #[error("Question session artifact digest must be non-zero")]
    InvalidArtifactDigest,
    #[error("Question session expiry must be within the short TTL limit")]
    InvalidExpiry,
    #[error("Question session timestamp moved backwards")]
    TimestampRegression,
    #[error("Question session has expired")]
    SessionExpired,
    #[error("Question session has not reached expiry")]
    SessionNotExpired,
    #[error("Question session state transition is invalid")]
    InvalidTransition,
    #[error("Question session revision is exhausted")]
    RevisionExhausted,
    #[error("Question session persisted record is invalid")]
    InvalidRecord,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn session_claim_is_single_execution_and_terminal_consumption_is_not_reusable() {
        let now = Utc::now();
        let mut session = session(now);
        let execution_id = ExecutionId::new();

        session
            .claim(execution_id, now + Duration::seconds(1))
            .unwrap();
        assert_eq!(session.execution_id, Some(execution_id));
        assert_eq!(session.revision, 2);
        assert_eq!(
            session.claim(ExecutionId::new(), now + Duration::seconds(2)),
            Err(QuestionSessionError::InvalidTransition)
        );

        session.consume(now + Duration::seconds(3)).unwrap();
        assert_eq!(session.state, QuestionSessionState::Consumed);
        assert_eq!(session.revision, 3);
        assert_eq!(
            session.consume(now + Duration::seconds(4)),
            Err(QuestionSessionError::InvalidTransition)
        );
        session.validate().unwrap();
    }

    #[test]
    fn expired_active_session_cannot_be_claimed_but_claimed_session_remains_recoverable() {
        let now = Utc::now();
        let mut expired = session(now);
        assert_eq!(
            expired.claim(ExecutionId::new(), now + Duration::minutes(5)),
            Err(QuestionSessionError::SessionExpired)
        );
        expired.expire(now + Duration::minutes(5)).unwrap();
        expired.validate().unwrap();

        let mut claimed = session(now);
        claimed
            .claim(ExecutionId::new(), now + Duration::seconds(1))
            .unwrap();
        assert_eq!(
            claimed.expire(now + Duration::minutes(5)),
            Err(QuestionSessionError::InvalidTransition)
        );
        claimed.consume(now + Duration::minutes(6)).unwrap();
        claimed.validate().unwrap();
    }

    #[test]
    fn session_rejects_unbounded_metadata_zero_digest_and_oversized_ttl() {
        let now = Utc::now();
        let mut invalid = session(now);
        invalid.artifact_type = "Unsafe Type".to_owned();
        assert_eq!(
            invalid.validate(),
            Err(QuestionSessionError::InvalidArtifactType)
        );

        invalid = session(now);
        invalid.artifact_digest = [0; 32];
        assert_eq!(
            invalid.validate(),
            Err(QuestionSessionError::InvalidArtifactDigest)
        );

        invalid = session(now);
        invalid.expires_at = now + Duration::seconds(MAX_QUESTION_SESSION_TTL_SECONDS + 1);
        assert_eq!(invalid.validate(), Err(QuestionSessionError::InvalidExpiry));
    }

    fn session(now: Timestamp) -> QuestionSession {
        QuestionSession::active(
            UserId::new(),
            ProviderAccountId::new(),
            TaskId::new(),
            ProviderId::new("chaoxing").unwrap(),
            "exam-v1".to_owned(),
            QuestionSnapshotId::new(),
            "chaoxing.exam-attempt.v1".to_owned(),
            [7; 32],
            now,
            now + Duration::minutes(5),
        )
        .unwrap()
    }
}
