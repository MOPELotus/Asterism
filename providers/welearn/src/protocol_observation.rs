use asterism_domain::{ProtocolObservationKind, ProtocolSurface};
use asterism_provider_api::{ProviderError, ProviderErrorKind};
use serde_json::Value;

pub(crate) fn json_value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(number)) if number.as_i64().is_some() => "integer",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

pub(crate) fn array_field_shape(
    document: &'static str,
    root: &Value,
    field: &'static str,
) -> Value {
    serde_json::json!({
        "document": document,
        "root_type": json_value_kind(Some(root)),
        "field": field,
        "field_type": json_value_kind(root.get(field)),
        "expected_type": "array",
    })
}

pub(crate) fn protocol_drift_with_observation(
    message: impl Into<String>,
    surface: ProtocolSurface,
    kind: ProtocolObservationKind,
    shape_sanitized: Value,
) -> ProviderError {
    let message = message.into();
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message.clone())
        .try_with_protocol_observation(surface, kind, shape_sanitized)
        .unwrap_or_else(|_| ProviderError::new(ProviderErrorKind::ProtocolDrift, message))
}
