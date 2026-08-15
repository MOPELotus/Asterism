use asterism_domain::{
    HumanRequiredReason, ProtocolObservation, ProtocolObservationError, ProtocolObservationKind,
    ProtocolSurface,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ProviderResult<T> = Result<T, ProviderError>;

/// A sanitized provider error. Secret-bearing response bodies must not be placed
/// in `message` or `provider_code`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub provider_code: Option<String>,
    pub retry_after_seconds: Option<u64>,
    pub human_required_reason: Option<HumanRequiredReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_observation: Option<Box<ProviderProtocolObservation>>,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            provider_code: None,
            retry_after_seconds: None,
            human_required_reason: None,
            protocol_observation: None,
        }
    }

    pub fn human_required(message: impl Into<String>, reason: HumanRequiredReason) -> Self {
        Self {
            kind: ProviderErrorKind::HumanRequired,
            message: message.into(),
            provider_code: None,
            retry_after_seconds: None,
            human_required_reason: Some(reason),
            protocol_observation: None,
        }
    }

    /// Attaches one bounded, secret-free protocol shape to this failure.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolObservationError`] when the shape is unsafe or
    /// exceeds the Core-owned bound.
    pub fn try_with_protocol_observation(
        mut self,
        surface: ProtocolSurface,
        kind: ProtocolObservationKind,
        shape_sanitized: Value,
    ) -> Result<Self, ProtocolObservationError> {
        self.protocol_observation = Some(Box::new(ProviderProtocolObservation::new(
            surface,
            kind,
            shape_sanitized,
        )?));
        Ok(self)
    }

    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ProviderErrorKind::RateLimited
                | ProviderErrorKind::Network
                | ProviderErrorKind::ProviderUnavailable
        )
    }
}

/// A Provider-supplied structural observation. It must describe shape only;
/// raw response bodies, credentials and user answer content are forbidden.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderProtocolObservation {
    pub surface: ProtocolSurface,
    pub kind: ProtocolObservationKind,
    pub shape_sanitized: Value,
}

impl ProviderProtocolObservation {
    /// Builds one validated observation payload.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolObservationError`] when the shape is unsafe or
    /// exceeds the Core-owned bound.
    pub fn new(
        surface: ProtocolSurface,
        kind: ProtocolObservationKind,
        shape_sanitized: Value,
    ) -> Result<Self, ProtocolObservationError> {
        ProtocolObservation::shape_digest(&shape_sanitized)?;
        Ok(Self {
            surface,
            kind,
            shape_sanitized,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Authentication,
    Authorization,
    RateLimited,
    Network,
    ProviderUnavailable,
    ProtocolDrift,
    RemoteChanged,
    UnsupportedTask,
    HumanRequired,
    InvalidResponse,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_observation_is_validated_and_optional_on_legacy_errors() {
        let error = ProviderError::new(ProviderErrorKind::ProtocolDrift, "shape changed")
            .try_with_protocol_observation(
                ProtocolSurface::QuestionParse,
                ProtocolObservationKind::UnknownQuestionKind,
                serde_json::json!({"type_code": 991, "fields": ["id", "type"]}),
            )
            .unwrap();
        assert!(error.protocol_observation.is_some());

        let legacy: ProviderError = serde_json::from_value(serde_json::json!({
            "kind": "protocol_drift",
            "message": "legacy",
            "provider_code": null,
            "retry_after_seconds": null,
            "human_required_reason": null
        }))
        .unwrap();
        assert!(legacy.protocol_observation.is_none());

        assert!(
            ProviderProtocolObservation::new(
                ProtocolSurface::Authentication,
                ProtocolObservationKind::FieldDrift,
                serde_json::json!({"access_token": "must-not-cross-boundary"}),
            )
            .is_err()
        );
    }
}
