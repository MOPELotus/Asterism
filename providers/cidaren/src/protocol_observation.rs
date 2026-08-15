use asterism_domain::{ProtocolObservationKind, ProtocolSurface};
use asterism_provider_api::{ProviderError, ProviderErrorKind};
use serde_json::Value;

pub(crate) fn protocol_drift_with_observation(
    message: &'static str,
    surface: ProtocolSurface,
    kind: ProtocolObservationKind,
    shape_sanitized: Value,
) -> ProviderError {
    error_with_protocol_observation(
        ProviderError::new(ProviderErrorKind::ProtocolDrift, message),
        surface,
        kind,
        shape_sanitized,
    )
}

pub(crate) fn error_with_protocol_observation(
    error: ProviderError,
    surface: ProtocolSurface,
    kind: ProtocolObservationKind,
    shape_sanitized: Value,
) -> ProviderError {
    let fallback = error.clone();
    error
        .try_with_protocol_observation(surface, kind, shape_sanitized)
        .unwrap_or(fallback)
}

pub(crate) const fn json_value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}
