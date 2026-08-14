use serde::{Deserialize, Serialize};

use crate::{
    ProviderAccountId, ProviderId, QuestionReadAttemptId, QuestionSessionId, QuestionSnapshotId,
    TaskId, Timestamp, UserId,
};

const MAX_OPERATION_TYPE_BYTES: usize = 96;
const MAX_PROVIDER_VERSION_BYTES: usize = 128;

/// A pre-Question remote start must finish or become recoverable promptly.
pub const MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS: i64 = 30 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionReadAttemptState {
    Prepared,
    Issued,
    Ambiguous,
    Materialized,
    Rejected,
    Cancelled,
    Expired,
}

/// Durable ledger for a non-idempotent Provider operation required before a
/// real Question snapshot exists (for example `StartAnswer` or Exam start).
///
/// The exact Provider request stays outside Domain; its stable type and digest
/// are frozen before dispatch. A successful or recovered operation can bind
/// exactly one real `QuestionSnapshot` and `QuestionSession` afterwards.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuestionReadAttempt {
    pub id: QuestionReadAttemptId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub state: QuestionReadAttemptState,
    pub question_snapshot_id: Option<QuestionSnapshotId>,
    pub question_session_id: Option<QuestionSessionId>,
    pub response_digest: Option<[u8; 32]>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
    pub issued_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub revision: u32,
}

impl QuestionReadAttempt {
    /// Creates a pre-dispatch attempt with an immutable Provider request
    /// identity.
    ///
    /// # Errors
    ///
    /// Rejects malformed Provider metadata, operation type, request digest or
    /// TTL.
    #[allow(clippy::too_many_arguments)]
    pub fn prepared(
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        task_id: TaskId,
        provider_id: ProviderId,
        provider_version: String,
        operation_type: String,
        request_digest: [u8; 32],
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
            operation_type,
            request_digest,
            state: QuestionReadAttemptState::Prepared,
            question_snapshot_id: None,
            question_session_id: None,
            response_digest: None,
            created_at,
            updated_at: created_at,
            expires_at,
            issued_at: None,
            completed_at: None,
            revision: 1,
        };
        attempt.validate()?;
        Ok(attempt)
    }

    /// Records dispatch before the non-idempotent request can leave Core.
    ///
    /// # Errors
    ///
    /// Rejects expired, already issued, terminal or timestamp-regressing
    /// attempts.
    pub fn issue(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if self.state != QuestionReadAttemptState::Prepared {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        if self.is_expired_at(at) {
            return Err(QuestionReadAttemptError::AttemptExpired);
        }
        self.advance(QuestionReadAttemptState::Issued, at)?;
        self.issued_at = Some(at);
        Ok(())
    }

    /// Conservatively records an issued request whose remote outcome is not
    /// known. The same attempt cannot be issued again.
    ///
    /// # Errors
    ///
    /// Rejects non-issued or timestamp-regressing attempts.
    pub fn mark_ambiguous(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.finish_issued(QuestionReadAttemptState::Ambiguous, None, at)
    }

    /// Records an unambiguous Provider rejection without attaching fabricated
    /// Question entities.
    ///
    /// # Errors
    ///
    /// Rejects non-issued attempts, zero response digests or timestamp
    /// regression.
    pub fn reject(
        &mut self,
        response_digest: [u8; 32],
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        if response_digest == [0; 32] {
            return Err(QuestionReadAttemptError::InvalidResponseDigest);
        }
        self.finish_issued(
            QuestionReadAttemptState::Rejected,
            Some(response_digest),
            at,
        )
    }

    /// Binds an accepted or freshly recovered start to the real immutable
    /// Question entities created from its response.
    ///
    /// # Errors
    ///
    /// Rejects attempts that were never issued, terminal attempts, zero
    /// response digests or timestamp regression.
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
            QuestionReadAttemptState::Issued | QuestionReadAttemptState::Ambiguous
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

    /// Cancels a request that has not been issued.
    ///
    /// # Errors
    ///
    /// Rejects issued, terminal or timestamp-regressing attempts.
    pub fn cancel(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.finish_prepared(QuestionReadAttemptState::Cancelled, at)
    }

    /// Expires a request that was never issued once its deadline is reached.
    /// Issued and ambiguous attempts remain durable for recovery.
    ///
    /// # Errors
    ///
    /// Rejects early expiry, issued/terminal attempts or timestamp regression.
    pub fn expire(&mut self, at: Timestamp) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if !self.is_expired_at(at) {
            return Err(QuestionReadAttemptError::AttemptNotExpired);
        }
        self.finish_prepared(QuestionReadAttemptState::Expired, at)
    }

    pub fn is_expired_at(&self, at: Timestamp) -> bool {
        at >= self.expires_at
    }

    /// Validates a persisted attempt independently of transition code.
    ///
    /// # Errors
    ///
    /// Rejects malformed metadata, timestamps, revisions or state fields.
    pub fn validate(&self) -> Result<(), QuestionReadAttemptError> {
        validate_label(&self.operation_type)?;
        if self.provider_version.is_empty()
            || self.provider_version.len() > MAX_PROVIDER_VERSION_BYTES
            || self
                .provider_version
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(QuestionReadAttemptError::InvalidProviderVersion);
        }
        if self.request_digest == [0; 32] {
            return Err(QuestionReadAttemptError::InvalidRequestDigest);
        }
        let ttl = self.expires_at.signed_duration_since(self.created_at);
        if ttl.num_seconds() <= 0 || ttl.num_seconds() > MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS {
            return Err(QuestionReadAttemptError::InvalidExpiry);
        }
        if self.updated_at < self.created_at
            || self.issued_at.is_some_and(|at| {
                at < self.created_at || at >= self.expires_at || at > self.updated_at
            })
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
        let valid_shape = match (self.state, self.issued_at, self.completed_at, self.revision) {
            (QuestionReadAttemptState::Prepared, None, None, 1) => {
                unbound && self.updated_at == self.created_at
            }
            (QuestionReadAttemptState::Issued, Some(issued_at), None, 2) => {
                unbound && self.updated_at == issued_at
            }
            (QuestionReadAttemptState::Ambiguous, Some(_), Some(completed_at), 3) => {
                unbound && self.updated_at == completed_at
            }
            (QuestionReadAttemptState::Rejected, Some(_), Some(completed_at), 3) => {
                !materialized
                    && self.question_snapshot_id.is_none()
                    && self.question_session_id.is_none()
                    && self.response_digest.is_some()
                    && self.updated_at == completed_at
            }
            (QuestionReadAttemptState::Materialized, Some(_), Some(completed_at), 3 | 4) => {
                materialized && self.updated_at == completed_at
            }
            (
                QuestionReadAttemptState::Cancelled | QuestionReadAttemptState::Expired,
                None,
                Some(completed_at),
                2,
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

    fn finish_issued(
        &mut self,
        next: QuestionReadAttemptState,
        response_digest: Option<[u8; 32]>,
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if self.state != QuestionReadAttemptState::Issued {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        self.advance(next, at)?;
        self.response_digest = response_digest;
        self.completed_at = Some(at);
        Ok(())
    }

    fn finish_prepared(
        &mut self,
        next: QuestionReadAttemptState,
        at: Timestamp,
    ) -> Result<(), QuestionReadAttemptError> {
        self.require_timestamp(at)?;
        if self.state != QuestionReadAttemptState::Prepared {
            return Err(QuestionReadAttemptError::InvalidTransition);
        }
        self.advance(next, at)?;
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

fn validate_label(value: &str) -> Result<(), QuestionReadAttemptError> {
    if value.is_empty()
        || value.len() > MAX_OPERATION_TYPE_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        Err(QuestionReadAttemptError::InvalidOperationType)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuestionReadAttemptError {
    #[error("Question read attempt operation type is invalid or unbounded")]
    InvalidOperationType,
    #[error("Question read attempt Provider version is invalid or unbounded")]
    InvalidProviderVersion,
    #[error("Question read attempt request digest must be non-zero")]
    InvalidRequestDigest,
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
    fn accepted_start_materializes_real_question_entities_once() {
        let now = Utc::now();
        let mut attempt = attempt(now);
        attempt.issue(now + Duration::seconds(1)).unwrap();
        attempt
            .materialize(
                QuestionSnapshotId::new(),
                QuestionSessionId::new(),
                [2; 32],
                now + Duration::seconds(2),
            )
            .unwrap();
        assert_eq!(attempt.state, QuestionReadAttemptState::Materialized);
        assert_eq!(attempt.revision, 3);
        attempt.validate().unwrap();
        assert_eq!(
            attempt.mark_ambiguous(now + Duration::seconds(3)),
            Err(QuestionReadAttemptError::InvalidTransition)
        );
    }

    #[test]
    fn ambiguous_start_is_not_reissued_but_can_materialize_after_fresh_recovery() {
        let now = Utc::now();
        let mut attempt = attempt(now);
        attempt.issue(now + Duration::seconds(1)).unwrap();
        attempt.mark_ambiguous(now + Duration::seconds(2)).unwrap();
        assert_eq!(
            attempt.issue(now + Duration::seconds(3)),
            Err(QuestionReadAttemptError::InvalidTransition)
        );
        attempt
            .materialize(
                QuestionSnapshotId::new(),
                QuestionSessionId::new(),
                [3; 32],
                now + Duration::seconds(4),
            )
            .unwrap();
        assert_eq!(attempt.revision, 4);
        attempt.validate().unwrap();
    }

    #[test]
    fn only_unissued_attempts_expire_or_cancel() {
        let now = Utc::now();
        let mut early = attempt(now);
        assert_eq!(
            early.expire(now + Duration::seconds(10)),
            Err(QuestionReadAttemptError::AttemptNotExpired)
        );
        early.expire(now + Duration::minutes(5)).unwrap();
        early.validate().unwrap();

        let mut issued = attempt(now);
        issued.issue(now + Duration::seconds(1)).unwrap();
        assert_eq!(
            issued.expire(now + Duration::minutes(5)),
            Err(QuestionReadAttemptError::InvalidTransition)
        );
    }

    fn attempt(now: Timestamp) -> QuestionReadAttempt {
        QuestionReadAttempt::prepared(
            UserId::new(),
            ProviderAccountId::new(),
            TaskId::new(),
            ProviderId::new("cidaren").unwrap(),
            "jv99-v1".to_owned(),
            "cidaren.start-answer.v1".to_owned(),
            [1; 32],
            now,
            now + Duration::minutes(5),
        )
        .unwrap()
    }
}
