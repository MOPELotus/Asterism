use serde::{Deserialize, Serialize};

use crate::{
    CourseEnrollmentAttemptId, CourseEnrollmentDraftId, ProviderAccountId, ProviderId, SecretId,
    Timestamp, UserId,
};

const MAX_REMOTE_ENROLLMENT_ID_BYTES: usize = 512;

/// Immutable, credential-free binding for one user-authorized course join.
///
/// The invite code and Provider request bytes never enter this aggregate. Core
/// stores those exact bytes behind `artifact_secret_id` and binds them through
/// `request_digest` before any non-idempotent remote call is issued.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseEnrollmentDraft {
    pub id: CourseEnrollmentDraftId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub provider_id: ProviderId,
    pub remote_course_id: String,
    pub remote_class_id: String,
    pub preview_digest: [u8; 32],
    pub request_digest: [u8; 32],
    pub artifact_secret_id: SecretId,
    pub created_at: Timestamp,
}

impl CourseEnrollmentDraft {
    /// Validates the frozen owner/account/Provider/target and hash bindings.
    ///
    /// # Errors
    ///
    /// Rejects malformed remote identities or empty digests. The draft is
    /// immutable after construction; changing an invite preview creates a new
    /// draft rather than mutating this one.
    pub fn validate(&self) -> Result<(), CourseEnrollmentValidationError> {
        if !valid_remote_id(&self.remote_course_id)
            || !valid_remote_id(&self.remote_class_id)
            || self.preview_digest == [0; 32]
            || self.request_digest == [0; 32]
        {
            return Err(CourseEnrollmentValidationError::InvalidDraft);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseEnrollmentAttemptState {
    Prepared,
    MutationIssued,
    ReceiptRecorded,
    VerificationPending,
    Succeeded,
    Rejected,
    Cancelled,
    FailedBeforeIssue,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseEnrollmentMutationReceipt {
    pub response_digest: [u8; 32],
    pub accepted: bool,
    pub observed_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseEnrollmentVerification {
    pub observation_digest: [u8; 32],
    pub membership_present: bool,
    pub observed_at: Timestamp,
}

/// Durable no-replay lifecycle for one enrollment mutation.
///
/// Once `issue_mutation` succeeds, no state transition returns to `Prepared`.
/// A lost or ambiguous response therefore moves only into read-only inventory
/// verification; it never authorizes another join request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CourseEnrollmentAttempt {
    pub id: CourseEnrollmentAttemptId,
    pub draft_id: CourseEnrollmentDraftId,
    pub state: CourseEnrollmentAttemptState,
    pub issued_operation_type: Option<String>,
    pub issued_request_digest: Option<[u8; 32]>,
    pub receipt: Option<CourseEnrollmentMutationReceipt>,
    pub verification: Option<CourseEnrollmentVerification>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl CourseEnrollmentAttempt {
    pub const fn new(
        id: CourseEnrollmentAttemptId,
        draft_id: CourseEnrollmentDraftId,
        now: Timestamp,
    ) -> Self {
        Self {
            id,
            draft_id,
            state: CourseEnrollmentAttemptState::Prepared,
            issued_operation_type: None,
            issued_request_digest: None,
            receipt: None,
            verification: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Irreversibly records the exact request before the remote mutation.
    ///
    /// # Errors
    ///
    /// Fails unless this is the first issue from a prepared Attempt and the
    /// digest equals the immutable draft request digest.
    pub fn issue_mutation(
        &mut self,
        draft: &CourseEnrollmentDraft,
        operation_type: impl Into<String>,
        request_digest: [u8; 32],
        now: Timestamp,
    ) -> Result<(), CourseEnrollmentValidationError> {
        let operation_type = operation_type.into();
        if self.draft_id != draft.id
            || self.state != CourseEnrollmentAttemptState::Prepared
            || !valid_operation_type(&draft.provider_id, &operation_type)
            || request_digest == [0; 32]
            || request_digest != draft.request_digest
            || now < self.created_at
        {
            return Err(CourseEnrollmentValidationError::InvalidTransition);
        }
        self.issued_operation_type = Some(operation_type);
        self.issued_request_digest = Some(request_digest);
        self.state = CourseEnrollmentAttemptState::MutationIssued;
        self.updated_at = now;
        Ok(())
    }

    /// Records only a definite parsed remote response.
    ///
    /// Ambiguous transport outcomes must skip this transition and enter
    /// `begin_verification` directly.
    ///
    /// # Errors
    ///
    /// Fails unless the mutation was issued exactly once, the response digest
    /// is non-empty, and observation time does not regress.
    pub fn record_receipt(
        &mut self,
        receipt: CourseEnrollmentMutationReceipt,
    ) -> Result<(), CourseEnrollmentValidationError> {
        if self.state != CourseEnrollmentAttemptState::MutationIssued
            || receipt.response_digest == [0; 32]
            || receipt.observed_at < self.updated_at
        {
            return Err(CourseEnrollmentValidationError::InvalidTransition);
        }
        self.receipt = Some(receipt);
        self.state = if receipt.accepted {
            CourseEnrollmentAttemptState::ReceiptRecorded
        } else {
            CourseEnrollmentAttemptState::Rejected
        };
        self.updated_at = receipt.observed_at;
        Ok(())
    }

    /// Enters read-only membership verification after an accepted or ambiguous
    /// issue. A definite rejection cannot be reinterpreted as ambiguity.
    ///
    /// # Errors
    ///
    /// Fails before issue, after definite rejection, after a terminal state,
    /// or when observation time regresses.
    pub fn begin_verification(
        &mut self,
        now: Timestamp,
    ) -> Result<(), CourseEnrollmentValidationError> {
        if !matches!(
            self.state,
            CourseEnrollmentAttemptState::MutationIssued
                | CourseEnrollmentAttemptState::ReceiptRecorded
                | CourseEnrollmentAttemptState::VerificationPending
        ) || now < self.updated_at
        {
            return Err(CourseEnrollmentValidationError::InvalidTransition);
        }
        self.state = CourseEnrollmentAttemptState::VerificationPending;
        self.updated_at = now;
        Ok(())
    }

    /// Records a fresh Course inventory observation.
    ///
    /// Absence remains pending because it cannot prove that an issued
    /// non-idempotent mutation is safe to replay. Only exact membership
    /// presence completes the Attempt.
    ///
    /// # Errors
    ///
    /// Fails outside read-only verification, for an empty observation digest,
    /// or when observation time regresses.
    pub fn record_verification(
        &mut self,
        verification: CourseEnrollmentVerification,
    ) -> Result<(), CourseEnrollmentValidationError> {
        if self.state != CourseEnrollmentAttemptState::VerificationPending
            || verification.observation_digest == [0; 32]
            || verification.observed_at < self.updated_at
        {
            return Err(CourseEnrollmentValidationError::InvalidTransition);
        }
        self.verification = Some(verification);
        if verification.membership_present {
            self.state = CourseEnrollmentAttemptState::Succeeded;
        }
        self.updated_at = verification.observed_at;
        Ok(())
    }

    /// Cancels an Attempt only while no remote mutation has been issued.
    ///
    /// # Errors
    ///
    /// Fails after issue or when time regresses.
    pub fn cancel_before_issue(
        &mut self,
        now: Timestamp,
    ) -> Result<(), CourseEnrollmentValidationError> {
        self.finish_before_issue(CourseEnrollmentAttemptState::Cancelled, now)
    }

    /// Marks a local preparation failure only while no mutation was issued.
    ///
    /// # Errors
    ///
    /// Fails after issue or when time regresses.
    pub fn fail_before_issue(
        &mut self,
        now: Timestamp,
    ) -> Result<(), CourseEnrollmentValidationError> {
        self.finish_before_issue(CourseEnrollmentAttemptState::FailedBeforeIssue, now)
    }

    fn finish_before_issue(
        &mut self,
        state: CourseEnrollmentAttemptState,
        now: Timestamp,
    ) -> Result<(), CourseEnrollmentValidationError> {
        if self.state != CourseEnrollmentAttemptState::Prepared || now < self.updated_at {
            return Err(CourseEnrollmentValidationError::InvalidTransition);
        }
        self.state = state;
        self.updated_at = now;
        Ok(())
    }
}

fn valid_remote_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_ENROLLMENT_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_operation_type(provider_id: &ProviderId, value: &str) -> bool {
    value
        .strip_prefix(provider_id.as_str())
        .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CourseEnrollmentValidationError {
    #[error("course enrollment draft is invalid")]
    InvalidDraft,
    #[error("course enrollment Attempt transition is invalid")]
    InvalidTransition,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn draft(now: Timestamp) -> CourseEnrollmentDraft {
        CourseEnrollmentDraft {
            id: CourseEnrollmentDraftId::new(),
            owner_user_id: UserId::new(),
            provider_account_id: ProviderAccountId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            remote_course_id: "course-7".to_owned(),
            remote_class_id: "class-9".to_owned(),
            preview_digest: [3; 32],
            request_digest: [5; 32],
            artifact_secret_id: SecretId::new(),
            created_at: now,
        }
    }

    #[test]
    fn accepted_enrollment_requires_independent_membership_verification() {
        let now = Utc::now();
        let draft = draft(now);
        assert_eq!(draft.validate(), Ok(()));
        let mut attempt =
            CourseEnrollmentAttempt::new(CourseEnrollmentAttemptId::new(), draft.id, now);
        attempt
            .issue_mutation(
                &draft,
                "chaoxing.course-enrollment.join",
                draft.request_digest,
                now + Duration::seconds(1),
            )
            .unwrap();
        attempt
            .record_receipt(CourseEnrollmentMutationReceipt {
                response_digest: [7; 32],
                accepted: true,
                observed_at: now + Duration::seconds(2),
            })
            .unwrap();
        assert_eq!(attempt.state, CourseEnrollmentAttemptState::ReceiptRecorded);
        attempt
            .begin_verification(now + Duration::seconds(3))
            .unwrap();
        attempt
            .record_verification(CourseEnrollmentVerification {
                observation_digest: [9; 32],
                membership_present: true,
                observed_at: now + Duration::seconds(4),
            })
            .unwrap();
        assert_eq!(attempt.state, CourseEnrollmentAttemptState::Succeeded);
    }

    #[test]
    fn ambiguous_issue_never_returns_to_a_replayable_state() {
        let now = Utc::now();
        let draft = draft(now);
        let mut attempt =
            CourseEnrollmentAttempt::new(CourseEnrollmentAttemptId::new(), draft.id, now);
        attempt
            .issue_mutation(&draft, "chaoxing.course-enrollment.join", [5; 32], now)
            .unwrap();
        attempt.begin_verification(now).unwrap();
        attempt
            .record_verification(CourseEnrollmentVerification {
                observation_digest: [8; 32],
                membership_present: false,
                observed_at: now,
            })
            .unwrap();
        assert_eq!(
            attempt.state,
            CourseEnrollmentAttemptState::VerificationPending
        );
        assert!(
            attempt
                .issue_mutation(&draft, "chaoxing.course-enrollment.join", [5; 32], now)
                .is_err()
        );
        assert!(attempt.cancel_before_issue(now).is_err());
    }

    #[test]
    fn definite_rejection_cannot_enter_ambiguous_recovery() {
        let now = Utc::now();
        let draft = draft(now);
        let mut attempt =
            CourseEnrollmentAttempt::new(CourseEnrollmentAttemptId::new(), draft.id, now);
        attempt
            .issue_mutation(&draft, "chaoxing.course-enrollment.join", [5; 32], now)
            .unwrap();
        attempt
            .record_receipt(CourseEnrollmentMutationReceipt {
                response_digest: [4; 32],
                accepted: false,
                observed_at: now,
            })
            .unwrap();
        assert_eq!(attempt.state, CourseEnrollmentAttemptState::Rejected);
        assert!(attempt.begin_verification(now).is_err());
    }
}
