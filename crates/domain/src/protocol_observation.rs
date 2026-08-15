use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ExecutionId, ProtocolObservationId, ProviderId, Timestamp};

const MAX_SHAPE_BYTES: usize = 64 * 1_024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolSurface {
    Authentication,
    CourseInventory,
    TaskInventory,
    TaskDetail,
    TaskProgress,
    QuestionInventory,
    QuestionParse,
    AnswerResolve,
    SubmissionBuild,
    SubmissionExecute,
    SubmissionVerify,
    TaskExecution,
    BrowserBridge,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolObservationKind {
    UnknownQuestionKind,
    UnknownResultShape,
    UnknownTaskType,
    FieldDrift,
    EndpointVersionDrift,
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProtocolObservation {
    pub id: ProtocolObservationId,
    pub provider_id: ProviderId,
    pub surface: ProtocolSurface,
    pub kind: ProtocolObservationKind,
    pub shape_digest: [u8; 32],
    pub shape_sanitized: Value,
    pub occurrence_count: u64,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub last_execution_id: Option<ExecutionId>,
}

impl ProtocolObservation {
    /// Computes the digest of one bounded secret-free sanitized shape.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolObservationError::InvalidShape`] when serialization
    /// fails, the shape exceeds its byte bound or a sensitive key is present.
    pub fn shape_digest(shape_sanitized: &Value) -> Result<[u8; 32], ProtocolObservationError> {
        let encoded = serde_json::to_vec(shape_sanitized)
            .map_err(|_| ProtocolObservationError::InvalidShape)?;
        if encoded.is_empty()
            || encoded.len() > MAX_SHAPE_BYTES
            || contains_sensitive_key(shape_sanitized)
        {
            return Err(ProtocolObservationError::InvalidShape);
        }
        Ok(Sha256::digest(encoded).into())
    }

    /// Validates digest, occurrence and aggregate timestamp invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolObservationError`] when the shape is unsafe, its
    /// digest differs, the count is zero or aggregate timestamps regress.
    pub fn validate(&self) -> Result<(), ProtocolObservationError> {
        if self.occurrence_count == 0
            || self.last_seen_at < self.first_seen_at
            || Self::shape_digest(&self.shape_sanitized)? != self.shape_digest
        {
            Err(ProtocolObservationError::InvalidObservation)
        } else {
            Ok(())
        }
    }
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized: String = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                normalized.as_str(),
                "cookie"
                    | "authorization"
                    | "password"
                    | "accesstoken"
                    | "refreshtoken"
                    | "sessionsecret"
                    | "clientsecret"
            ) || contains_sensitive_key(value)
        }),
        Value::Array(items) => items.iter().any(contains_sensitive_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProtocolObservationError {
    #[error("protocol observation shape is unsafe or exceeds its bound")]
    InvalidShape,
    #[error("protocol observation aggregate is inconsistent")]
    InvalidObservation,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn protocol_shape_is_bounded_digest_bound_and_secret_free() {
        let shape = serde_json::json!({"type_code": 991, "fields": ["a", "b"]});
        let now = Utc::now();
        let observation = ProtocolObservation {
            id: ProtocolObservationId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            surface: ProtocolSurface::QuestionParse,
            kind: ProtocolObservationKind::UnknownQuestionKind,
            shape_digest: ProtocolObservation::shape_digest(&shape).unwrap(),
            shape_sanitized: shape,
            occurrence_count: 1,
            first_seen_at: now,
            last_seen_at: now,
            last_execution_id: None,
        };
        assert!(observation.validate().is_ok());
        assert!(
            ProtocolObservation::shape_digest(&serde_json::json!({"access_token": "secret"}))
                .is_err()
        );
    }
}
