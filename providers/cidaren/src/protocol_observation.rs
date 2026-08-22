use asterism_domain::{HumanRequiredReason, ProtocolObservationKind, ProtocolSurface};
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

pub(crate) fn security_verification_required(code: i64) -> Option<ProviderError> {
    if code != 11_003 {
        return None;
    }
    let mut error = ProviderError::human_required(
        "Cidaren requires security verification in the official application",
        HumanRequiredReason::ManualIntervention,
    );
    error.provider_code = Some("11003".to_owned());
    Some(error)
}

#[cfg(test)]
mod tests {
    use asterism_domain::HumanRequiredReason;

    use super::*;

    #[test]
    fn security_verification_is_human_required_and_not_retryable() {
        let error = security_verification_required(11_003).unwrap();
        assert_eq!(error.kind, ProviderErrorKind::HumanRequired);
        assert_eq!(error.provider_code.as_deref(), Some("11003"));
        assert_eq!(
            error.human_required_reason,
            Some(HumanRequiredReason::ManualIntervention)
        );
        assert!(!error.is_retryable());
        assert!(security_verification_required(1).is_none());
    }
}
