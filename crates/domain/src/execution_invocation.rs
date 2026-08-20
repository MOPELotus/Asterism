use serde::{Deserialize, Serialize};

use crate::{
    CourseId, ExecutionInvocationDraftId, ProviderAccountId, ProviderId, SecretId,
    SubmissionDraftId, TaskCapability, TaskId, Timestamp, UserId,
};

/// Immutable, credential-free binding for Provider-private input supplied
/// before an Execution is scheduled.
///
/// The potentially large private bytes never enter this aggregate. Core keeps
/// them encrypted behind `private_input_secret_id` and binds them through
/// `private_input_digest`. A draft may be claimed by exactly one Execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionInvocationDraft {
    pub id: ExecutionInvocationDraftId,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub course_id: Option<CourseId>,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub requested_capabilities: Vec<TaskCapability>,
    pub submission_draft_id: Option<SubmissionDraftId>,
    pub private_input_type: String,
    pub private_input_digest: [u8; 32],
    pub private_input_secret_id: SecretId,
    pub plan_artifact_digest: [u8; 32],
    pub created_at: Timestamp,
}

impl ExecutionInvocationDraft {
    /// Validates the immutable owner, Provider, Task, capability and digest
    /// binding independently from encrypted private bytes.
    ///
    /// # Errors
    ///
    /// Rejects non-canonical authority, foreign artifact namespaces, unsafe
    /// Provider versions, or empty digests.
    pub fn validate(&self) -> Result<(), ExecutionInvocationDraftValidationError> {
        let namespaced_type = self
            .private_input_type
            .strip_prefix(self.provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1);
        if self.requested_capabilities.is_empty()
            || self.requested_capabilities.len() > 5
            || !self
                .requested_capabilities
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            || self.provider_version.is_empty()
            || self.provider_version.len() > 128
            || self.provider_version.trim() != self.provider_version
            || self.provider_version.chars().any(char::is_control)
            || self.private_input_type.len() > 128
            || !namespaced_type
            || !self
                .private_input_type
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            || self.private_input_digest == [0; 32]
            || self.plan_artifact_digest == [0; 32]
        {
            Err(ExecutionInvocationDraftValidationError::Invalid)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionInvocationDraftValidationError {
    #[error("execution invocation draft identity, authority or digest binding is invalid")]
    Invalid,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn draft() -> ExecutionInvocationDraft {
        ExecutionInvocationDraft {
            id: ExecutionInvocationDraftId::new(),
            owner_user_id: UserId::new(),
            provider_account_id: ProviderAccountId::new(),
            course_id: Some(CourseId::new()),
            task_id: TaskId::new(),
            provider_id: ProviderId::new("uai").unwrap(),
            provider_version: "0.1.0".to_owned(),
            requested_capabilities: vec![TaskCapability::Discussion],
            submission_draft_id: None,
            private_input_type: "uai.discussion.reply-state.v1".to_owned(),
            private_input_digest: [1; 32],
            private_input_secret_id: SecretId::new(),
            plan_artifact_digest: [2; 32],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn draft_requires_canonical_provider_bound_authority() {
        assert_eq!(draft().validate(), Ok(()));

        let mut duplicate = draft();
        duplicate.requested_capabilities =
            vec![TaskCapability::Discussion, TaskCapability::Discussion];
        assert_eq!(
            duplicate.validate(),
            Err(ExecutionInvocationDraftValidationError::Invalid)
        );

        let mut foreign = draft();
        foreign.private_input_type = "foreign.discussion.reply-state.v1".to_owned();
        assert_eq!(
            foreign.validate(),
            Err(ExecutionInvocationDraftValidationError::Invalid)
        );
    }
}
