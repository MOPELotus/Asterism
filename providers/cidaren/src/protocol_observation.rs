use asterism_domain::{ProtocolObservationKind, ProtocolSurface};
use asterism_provider_api::{ProviderError, ProviderErrorKind};
use serde_json::Value;

pub(crate) fn protocol_drift_with_observation(
    message: &'static str,
    surface: ProtocolSurface,
    kind: ProtocolObservationKind,
    shape_sanitized: Value,
) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
        .try_with_protocol_observation(surface, kind, shape_sanitized)
        .unwrap_or_else(|_| ProviderError::new(ProviderErrorKind::ProtocolDrift, message))
}
