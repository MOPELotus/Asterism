use std::fmt;

use asterism_domain::ProviderId;
use asterism_secrets::{SecretString, SecretValue};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ExecutionMutationRecoveryRecord, ExecutionMutationSink, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderIdentity, ProviderResult,
};

const MAX_ENROLLMENT_REMOTE_ID_BYTES: usize = 512;
const MAX_ENROLLMENT_PREVIEW_BYTES: usize = 64 * 1_024;
const MAX_ENROLLMENT_REQUEST_BYTES: usize = 1024 * 1024;

/// Provider-private immutable enrollment artifact prepared from one invitation.
///
/// The sanitized preview is safe for confirmation UI. The exact request bytes
/// remain secret, are digest-bound here, and must be encrypted by Core before
/// the invitation plaintext leaves memory.
pub struct ProviderCourseEnrollmentDraft {
    provider_id: ProviderId,
    artifact_type: String,
    remote_course_id: String,
    remote_class_id: String,
    preview_digest: [u8; 32],
    preview_sanitized: Value,
    request_digest: [u8; 32],
    request: SecretValue,
}

impl ProviderCourseEnrollmentDraft {
    /// Creates one bounded Provider-scoped enrollment artifact.
    ///
    /// # Errors
    ///
    /// Rejects malformed identities, foreign artifact labels, secret-shaped
    /// preview data, and empty or oversized exact request bytes.
    pub fn try_new(
        provider_id: ProviderId,
        artifact_type: impl Into<String>,
        remote_course_id: impl Into<String>,
        remote_class_id: impl Into<String>,
        preview_sanitized: Value,
        request: SecretValue,
    ) -> ProviderResult<Self> {
        let artifact_type = artifact_type.into();
        let remote_course_id = remote_course_id.into();
        let remote_class_id = remote_class_id.into();
        let preview_bytes =
            serde_json::to_vec(&preview_sanitized).map_err(|_| invalid_enrollment_artifact())?;
        let request_bytes = request.expose_secret();
        if !valid_provider_label(&provider_id, &artifact_type)
            || !valid_remote_id(&remote_course_id)
            || !valid_remote_id(&remote_class_id)
            || !preview_sanitized.is_object()
            || preview_bytes.is_empty()
            || preview_bytes.len() > MAX_ENROLLMENT_PREVIEW_BYTES
            || contains_secret_key(&preview_sanitized)
            || request_bytes.is_empty()
            || request_bytes.len() > MAX_ENROLLMENT_REQUEST_BYTES
        {
            return Err(invalid_enrollment_artifact());
        }
        let preview_digest = enrollment_digest(
            b"asterism.course-enrollment-preview.v1\0",
            &provider_id,
            &artifact_type,
            &preview_bytes,
        );
        let request_digest = enrollment_digest(
            b"asterism.course-enrollment-request.v1\0",
            &provider_id,
            &artifact_type,
            request_bytes,
        );
        Ok(Self {
            provider_id,
            artifact_type,
            remote_course_id,
            remote_class_id,
            preview_digest,
            preview_sanitized,
            request_digest,
            request,
        })
    }

    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn artifact_type(&self) -> &str {
        &self.artifact_type
    }

    pub fn remote_course_id(&self) -> &str {
        &self.remote_course_id
    }

    pub fn remote_class_id(&self) -> &str {
        &self.remote_class_id
    }

    pub const fn preview_digest(&self) -> [u8; 32] {
        self.preview_digest
    }

    pub const fn preview_sanitized(&self) -> &Value {
        &self.preview_sanitized
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn request(&self) -> &SecretValue {
        &self.request
    }
}

impl fmt::Debug for ProviderCourseEnrollmentDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCourseEnrollmentDraft")
            .field("provider_id", &self.provider_id)
            .field("artifact_type", &self.artifact_type)
            .field("remote_course_id", &self.remote_course_id)
            .field("remote_class_id", &self.remote_class_id)
            .field("preview_digest", &"[HASHED]")
            .field("preview_sanitized", &"[REDACTED]")
            .field("request_digest", &"[HASHED]")
            .field("request", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCourseEnrollmentDispatchOutcome {
    Accepted,
    Rejected,
}

/// Fresh, read-only Course inventory evidence for one exact frozen target.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ProviderCourseEnrollmentVerification {
    observation_digest: [u8; 32],
    membership_present: bool,
}

impl ProviderCourseEnrollmentVerification {
    /// # Errors
    ///
    /// Rejects an empty inventory observation digest.
    pub fn try_new(observation_digest: [u8; 32], membership_present: bool) -> ProviderResult<Self> {
        if observation_digest == [0; 32] {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Course enrollment inventory verification is invalid",
            ));
        }
        Ok(Self {
            observation_digest,
            membership_present,
        })
    }

    pub const fn observation_digest(self) -> [u8; 32] {
        self.observation_digest
    }

    pub const fn membership_present(self) -> bool {
        self.membership_present
    }
}

impl fmt::Debug for ProviderCourseEnrollmentVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCourseEnrollmentVerification")
            .field("observation_digest", &"[HASHED]")
            .field("membership_present", &self.membership_present)
            .finish()
    }
}

/// Course-level non-idempotent enrollment capability.
///
/// Preparation is read-only. Execution must call `mutations.issue` before the
/// remote join request and record a receipt only for a definite parsed
/// response. Recovery is always a fresh inventory read and never repeats the
/// invitation request.
#[async_trait]
pub trait CourseEnrollmentCapability: ProviderIdentity {
    async fn prepare_course_enrollment(
        &self,
        context: &ProviderContext,
        invitation: SecretString,
    ) -> ProviderResult<ProviderCourseEnrollmentDraft>;

    async fn execute_course_enrollment(
        &self,
        context: &ProviderContext,
        draft: &ProviderCourseEnrollmentDraft,
        mutations: &dyn ExecutionMutationSink,
    ) -> ProviderResult<ProviderCourseEnrollmentDispatchOutcome>;

    async fn verify_course_enrollment(
        &self,
        context: &ProviderContext,
        draft: &ProviderCourseEnrollmentDraft,
        mutation: &ExecutionMutationRecoveryRecord,
    ) -> ProviderResult<ProviderCourseEnrollmentVerification>;
}

fn enrollment_digest(
    domain: &[u8],
    provider_id: &ProviderId,
    artifact_type: &str,
    bytes: &[u8],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(provider_id.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(artifact_type.as_bytes());
    digest.update(b"\0");
    digest.update(bytes);
    digest.finalize().into()
}

fn valid_provider_label(provider_id: &ProviderId, value: &str) -> bool {
    value
        .strip_prefix(provider_id.as_str())
        .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_remote_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ENROLLMENT_REMOTE_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn contains_secret_key(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            [
                "cookie",
                "token",
                "secret",
                "password",
                "authorization",
                "invite",
                "enc",
            ]
            .iter()
            .any(|needle| key.contains(needle))
                || contains_secret_key(value)
        }),
        Value::Array(values) => values.iter().any(contains_secret_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn invalid_enrollment_artifact() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Provider Course enrollment artifact is invalid",
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn draft_binds_exact_request_and_redacts_debug() {
        let draft = ProviderCourseEnrollmentDraft::try_new(
            ProviderId::new("chaoxing").unwrap(),
            "chaoxing.course-enrollment.v1",
            "course-1",
            "class-2",
            json!({"course_title": "Writing", "teacher": "Li"}),
            SecretValue::new(b"exact-request-with-check-value".to_vec()),
        )
        .unwrap();
        assert_ne!(draft.preview_digest(), [0; 32]);
        assert_ne!(draft.request_digest(), [0; 32]);
        let debug = format!("{draft:?}");
        assert!(!debug.contains("Writing"));
        assert!(!debug.contains("exact-request"));
    }

    #[test]
    fn sanitized_preview_rejects_invitation_and_crypto_fields() {
        for preview in [
            json!({"invite_code": "123"}),
            json!({"checkEnc": "opaque"}),
            json!({"authorization": "Bearer value"}),
        ] {
            assert!(
                ProviderCourseEnrollmentDraft::try_new(
                    ProviderId::new("chaoxing").unwrap(),
                    "chaoxing.course-enrollment.v1",
                    "course-1",
                    "class-2",
                    preview,
                    SecretValue::new(vec![1]),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn absence_is_explicit_but_never_authorizes_replay() {
        let verification = ProviderCourseEnrollmentVerification::try_new([7; 32], false).unwrap();
        assert!(!verification.membership_present());
        assert!(ProviderCourseEnrollmentVerification::try_new([0; 32], true).is_err());
    }
}
