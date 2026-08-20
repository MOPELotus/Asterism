use serde::{Deserialize, Serialize};

use crate::{
    ProviderAccountId, ProviderId, QuestionReadAttemptId, QuestionSessionId, QuestionSnapshotId,
    TaskId, Timestamp, UserId,
};

const MAX_PROVIDER_VERSION_BYTES: usize = 128;

/// A pre-Question flow must finish or remain explicitly recoverable promptly.
pub const MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS: i64 = 30 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionReadAttemptState {
    Active,
    Ambiguous,
    Completed,
    Materialized,
    Rejected,
    Cancelled,
    Expired,
}

/// Durable owner/account/Task lifecycle for Provider operations that must run
/// before the first real `QuestionSnapshot` exists.
///
/// Provider commands and mutable state are encrypted outside Domain. Each
/// non-idempotent request is recorded in a separate operation ledger, allowing
/// an accepted request to rotate the pre-Question continuation and advance to
/// another operation without pretending that every response contains a real
/// Question.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuestionReadAttempt {
    pub id: QuestionReadAttemptId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub state: QuestionReadAttemptState,
    pub question_snapshot_id: Option<QuestionSnapshotId>,
    pub question_session_id: Option<QuestionSessionId>,
    pub response_digest: Option<[u8; 32]>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub revision: u32,
}

impl QuestionReadAttempt {
    /// Creates an active attempt before any Provider request can be issued.
    ///
    /// # Errors
    ///
    /// Rejects malformed Provider metadata or an invalid TTL.
    #[allow(clippy::too_many_arguments)]
    pub fn active(
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        task_id: TaskId,
        provider_id: ProviderId,
        provider_version: String,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<Self, QuestionReadAttemptError> {
        let attempt = Self {
            id: QuestionReadAttemptId::new(),
            owner_user_id,
            provider_account_id,
            task_id,
            provider_id,
            provider_version,
            state: QuestionReadAttemptState::Active,
            question_snapshot_id: None,
            question_session_id: None,
            response_digest: None,
            created_at,
            updated_at: created_at,
            expires_at,
            completed_at: None,
            revision: 1,
        };
        attempt.validate()?;
        Ok(attempt)
    }

    /// Records an accepted pre-Question operation that rotated encrypted
    /// Provider state but did not yet produce a real Question.
    ///
    /// # Errors
    ///
    /// Rejects terminal, expired, or timestamp-regressing attempts.
    pub fn advance_active(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_active(at)?;
        self.advance(QuestionReadAttemptState::Active, at)
    }

    /// Conservatively locks a flow whose last issued remote outcome is unknown.
    /// The operation ledger retains the exact request and forbids replay.
    ///
    /// # Errors
    ///
    /// Rejects terminal or timestamp-regressing attempts.
    pub fn mark_ambiguous(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.finish_active(QuestionReadAttemptState::Ambiguous, None, at)
    }

    /// Reopens an ambiguity that fresh Provider readback resolved to a
    /// definite continuing state. The revision advances again so the resumed
    /// continuation cannot be confused with either the issued or locked
    /// boundary.
    ///
    /// # Errors
    ///
    /// Rejects non-ambiguous attempts or timestamp regression.
    pub fn recover_active(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if self.state != QuestionReadAttemptState::Ambiguous {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        self.advance(QuestionReadAttemptState::Active, at)?;
        self.completed_at = None;
        Ok(())
    }

    /// Records an unambiguous Provider rejection without fabricating Question
    /// entities.
    ///
    /// # Errors
    ///
    /// Rejects zero response digests, terminal attempts, or timestamp
    /// regression.
    pub fn reject(
        &mut self,
        response_digest: [u8; 32],
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        if response_digest == [0; 32] {
            return Err(QuestionReadAttemptError::InvalidResponseDigest);
        }
        self.finish_active(
            QuestionReadAttemptState::Rejected,
            Some(response_digest),
            at,
        )
    }

    /// Binds an accepted flow, or an ambiguous flow resolved by fresh
    /// rediscovery, to the first real immutable Question entities.
    ///
    /// # Errors
    ///
    /// Rejects terminal attempts, zero response digests, or timestamp
    /// regression.
    pub fn materialize(
        &mut self,
        question_snapshot_id: QuestionSnapshotId,
        question_session_id: QuestionSessionId,
        response_digest: [u8; 32],
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if !matches!(
            self.state,
            QuestionReadAttemptState::Active | QuestionReadAttemptState::Ambiguous
        ) {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        if response_digest == [0; 32] {
            return Err(QuestionReadAttemptError::InvalidResponseDigest);
        }
        self.advance(QuestionReadAttemptState::Materialized, at)?;
        self.question_snapshot_id = Some(question_snapshot_id);
        self.question_session_id = Some(question_session_id);
        self.response_digest = Some(response_digest);
        self.completed_at = Some(at);
        Ok(())
    }

    /// Records a definite successful terminal response when the Provider says
    /// the remote attempt is already complete before yielding any Question.
    ///
    /// # Errors
    ///
    /// Rejects terminal attempts, zero response digests, or timestamp
    /// regression.
    pub fn complete(
        &mut self,
        response_digest: [u8; 32],
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if !matches!(
            self.state,
            QuestionReadAttemptState::Active | QuestionReadAttemptState::Ambiguous
        ) {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        if response_digest == [0; 32] {
            return Err(QuestionReadAttemptError::InvalidResponseDigest);
        }
        self.advance(QuestionReadAttemptState::Completed, at)?;
        self.response_digest = Some(response_digest);
        self.completed_at = Some(at);
        Ok(())
    }

    /// Cancels an active flow before another request is issued.
    ///
    /// # Errors
    ///
    /// Rejects terminal or timestamp-regressing attempts.
    pub fn cancel(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_active(at)?;
        if self.is_expired_at(at) {
            return Err(QuestionReadAttemptError::AttemptExpired);
        }
        self.advance(QuestionReadAttemptState::Cancelled, at)?;
        self.completed_at = Some(at);
        Ok(())
    }

    /// Expires an active flow once its deadline is reached. Issued operations
    /// must first be accepted, rejected, or marked ambiguous by the ledger.
    ///
    /// # Errors
    ///
    /// Rejects early expiry, terminal attempts, or timestamp regression.
    pub fn expire(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if self.state != QuestionReadAttemptState::Active {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        if !self.is_expired_at(at) {
            return Err(QuestionReadAttemptError::AttemptNotExpired);
        }
        self.advance(QuestionReadAttemptState::Expired, at)?;
        self.completed_at = Some(at);
        Ok(())
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }

    /// Validates a persisted attempt independently of transition code.
    ///
    /// # Errors
    ///
    /// Rejects malformed metadata, timestamps, revisions, or state fields.
    pub fn validate(&self) -> Result<(), QuestionReadAttemptError> {
        if self.provider_version.is_empty()
            || self.provider_version.len() > MAX_PROVIDER_VERSION_BYTES
            || self
                .provider_version
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(QuestionReadAttemptError::InvalidProviderVersion);
        }
        let ttl = self.expires_at.signed_duration_since(self.created_at);
        if ttl.num_seconds() <= 0 || ttl.num_seconds() > MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS {
            return Err(QuestionReadAttemptError::InvalidExpiry);
        }
        if self.revision == 0
            || self.updated_at < self.created_at
            || self
                .completed_at
                .is_some_and(|at| at < self.created_at || at > self.updated_at)
            || self.response_digest == Some([0; 32])
        {
            return Err(QuestionReadAttemptError::TimestampRegression);
        }

        let unbound = self.question_snapshot_id.is_none()
            && self.question_session_id.is_none()
            && self.response_digest.is_none();
        let materialized = self.question_snapshot_id.is_some()
            && self.question_session_id.is_some()
            && self.response_digest.is_some();
        let valid_shape = match (self.state, self.completed_at) {
            (QuestionReadAttemptState::Active, None) => unbound,
            (QuestionReadAttemptState::Ambiguous, Some(completed_at)) => {
                unbound && self.updated_at == completed_at
            }
            (
                QuestionReadAttemptState::Completed | QuestionReadAttemptState::Rejected,
                Some(completed_at),
            ) => {
                self.question_snapshot_id.is_none()
                    && self.question_session_id.is_none()
                    && self.response_digest.is_some()
                    && self.updated_at == completed_at
            }
            (QuestionReadAttemptState::Materialized, Some(completed_at)) => {
                materialized && self.updated_at == completed_at
            }
            (
                QuestionReadAttemptState::Cancelled | QuestionReadAttemptState::Expired,
                Some(completed_at),
            ) => {
                unbound
                    && self.updated_at == completed_at
                    && (self.state != QuestionReadAttemptState::Expired
                        || completed_at >= self.expires_at)
            }
            _ => false,
        };
        if valid_shape {
            Ok(())
        } else {
            Err(QuestionReadAttemptError::InvalidRecord)
        }
    }

    fn require_active(&self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if self.state != QuestionReadAttemptState::Active {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        Ok(())
    }

    fn finish_active(
        &mut self,
        next: QuestionReadAttemptState,
        response_digest: Option<[u8; 32]>,
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        self.require_active(at)?;
        self.advance(next, at)?;
        self.response_digest = response_digest;
        self.completed_at = Some(at);
        Ok(())
    }

    fn require_timestamp(&self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        if at < self.updated_at {
            Err(QuestionReadAttemptError::TimestampRegression)
        } else {
            Ok(())
        }
    }

    fn advance(
        &mut self,
        next: QuestionReadAttemptState,
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(QuestionReadAttemptError::RevisionExhausted)?;
        self.state = next;
        self.updated_at = at;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuestionReadAttemptError {
    #[error("Question read attempt Provider version is invalid or unbounded")]
    InvalidProviderVersion,
    #[error("Question read attempt response digest must be non-zero")]
    InvalidResponseDigest,
    #[error("Question read attempt expiry must be within the short TTL limit")]
    InvalidExpiry,
    #[error("Question read attempt timestamp moved backwards")]
    TimestampRegression,
    #[error("Question read attempt has expired")]
    AttemptExpired,
    #[error("Question read attempt has not reached expiry")]
    AttemptNotExpired,
    #[error("Question read attempt state transition is invalid")]
    InvalidTransition,
    #[error("Question read attempt revision is exhausted")]
    RevisionExhausted,
    #[error("Question read attempt persisted record is invalid")]
    InvalidRecord,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    #[test]
    fn accepted_pre_question_steps_advance_until_real_question_materializes() {
        let now = Utc::now();
        let mut attempt = attempt(now);
        attempt.advance_active(now + Duration::seconds(1)).unwrap();
        attempt.advance_active(now + Duration::seconds(2)).unwrap();
        attempt
            .materialize(
                QuestionSnapshotId::new(),
                QuestionSessionId::new(),
                [2; 32],
                now + Duration::seconds(3),
            )
            .unwrap();
        assert_eq!(attempt.state, QuestionReadAttemptState::Materialized);
        assert_eq!(attempt.revision, 4);
        attempt.validate().unwrap();
    }

    #[test]
    fn ambiguous_operation_is_not_advanced_but_can_materialize_after_recovery() {
        let now = Utc::now();
        let mut attempt = attempt(now);
        attempt.mark_ambiguous(now + Duration::seconds(1)).unwrap();
        assert_eq!(
            attempt.advance_active(now + Duration::seconds(2)),
            Err(QuestionReadAttemptError::InvalidTransition)
        );
        attempt
            .materialize(
                QuestionSnapshotId::new(),
                QuestionSessionId::new(),
                [3; 32],
                now + Duration::seconds(3),
            )
            .unwrap();
        attempt.validate().unwrap();
    }

    #[test]
    fn ambiguous_operation_can_reopen_with_a_fresh_revision() {
        let now = Utc::now();
        let mut attempt = attempt(now);
        attempt.mark_ambiguous(now + Duration::seconds(1)).unwrap();
        attempt.recover_active(now + Duration::seconds(2)).unwrap();
        assert_eq!(attempt.state, QuestionReadAttemptState::Active);
        assert_eq!(attempt.revision, 3);
        assert!(attempt.completed_at.is_none());
        attempt.validate().unwrap();
    }

    #[test]
    fn definite_completion_before_first_question_is_terminal_without_snapshot() {
        let now = Utc::now();
        let mut attempt = attempt(now);
        attempt
            .complete([5; 32], now + Duration::seconds(1))
            .unwrap();
        assert_eq!(attempt.state, QuestionReadAttemptState::Completed);
        assert_eq!(attempt.response_digest, Some([5; 32]));
        assert!(attempt.question_snapshot_id.is_none());
        assert!(attempt.question_session_id.is_none());
        attempt.validate().unwrap();
    }

    #[test]
    fn only_active_attempts_expire_or_cancel() {
        let now = Utc::now();
        let mut early = attempt(now);
        assert_eq!(
            early.expire(now + Duration::seconds(10)),
            Err(QuestionReadAttemptError::AttemptNotExpired)
        );
        early.expire(now + Duration::minutes(5)).unwrap();
        early.validate().unwrap();

        let mut ambiguous = attempt(now);
        ambiguous
            .mark_ambiguous(now + Duration::seconds(1))
            .unwrap();
        assert_eq!(
            ambiguous.cancel(now + Duration::seconds(2)),
            Err(QuestionReadAttemptError::InvalidTransition)
        );
    }

    fn attempt(now: Timestamp) -> QuestionReadAttempt {
        QuestionReadAttempt::active(
            UserId::new(),
            ProviderAccountId::new(),
            TaskId::new(),
            ProviderId::new("cidaren").unwrap(),
            "jv99-v1".to_owned(),
            now,
            now + Duration::minutes(5),
        )
        .unwrap()
    }
}
