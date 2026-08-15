use asterism_domain::{ExecutionId, ProtocolObservation, ProviderId, Timestamp};
use asterism_provider_api::{ProviderError, ProviderErrorKind};
use asterism_storage::{
    ProtocolObservationRecordRequest, ProtocolObservationRepository, StorageError,
};
use sha2::{Digest, Sha256};

const MAX_OCCURRENCE_SCOPE_BYTES: usize = 512;

pub(crate) async fn record_provider_protocol_observation(
    repository: Option<&dyn ProtocolObservationRepository>,
    provider_id: &ProviderId,
    execution_id: Option<ExecutionId>,
    occurrence_scope: &str,
    error: &ProviderError,
    observed_at: Timestamp,
) -> Result<(), ProviderProtocolObservationRecordError> {
    let Some(observation) = error.protocol_observation.as_ref() else {
        return Ok(());
    };
    let observation_kind_allowed = matches!(
        error.kind,
        ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse
    ) || error.kind == ProviderErrorKind::HumanRequired
        && error.human_required_reason.is_some();
    if !observation_kind_allowed
        || occurrence_scope.is_empty()
        || occurrence_scope.len() > MAX_OCCURRENCE_SCOPE_BYTES
        || occurrence_scope.trim() != occurrence_scope
        || occurrence_scope.chars().any(char::is_control)
    {
        return Err(ProviderProtocolObservationRecordError::Invalid);
    }
    let shape_digest = ProtocolObservation::shape_digest(&observation.shape_sanitized)
        .map_err(|_| ProviderProtocolObservationRecordError::Invalid)?;
    let occurrence = serde_json::json!({
        "provider_id": provider_id.as_str(),
        "execution_id": execution_id,
        "occurrence_scope": occurrence_scope,
        "error_kind": error.kind,
        "surface": observation.surface,
        "observation_kind": observation.kind,
        "shape_digest": shape_digest,
    });
    let occurrence_digest = Sha256::digest(
        serde_json::to_vec(&occurrence)
            .map_err(|_| ProviderProtocolObservationRecordError::Invalid)?,
    )
    .into();
    let Some(repository) = repository else {
        return Ok(());
    };
    repository
        .record_protocol_observation(ProtocolObservationRecordRequest {
            provider_id: provider_id.clone(),
            surface: observation.surface,
            kind: observation.kind,
            shape_sanitized: &observation.shape_sanitized,
            occurrence_digest,
            execution_id,
            observed_at,
        })
        .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderProtocolObservationRecordError {
    #[error("Provider supplied an invalid protocol observation")]
    Invalid,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        HumanRequiredReason, ProtocolObservationKind, ProtocolSurface, ProviderId,
    };
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn typed_human_required_may_retain_protocol_shape_without_a_repository() {
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let error = ProviderError::human_required(
            "mutation outcome needs review",
            HumanRequiredReason::ManualIntervention,
        )
        .try_with_protocol_observation(
            ProtocolSurface::SubmissionExecute,
            ProtocolObservationKind::UnknownResultShape,
            serde_json::json!({"document": "mutation_receipt", "ret_kind": "string"}),
        )
        .unwrap();
        assert!(
            record_provider_protocol_observation(
                None,
                &provider_id,
                None,
                "execution:test:mutation",
                &error,
                Utc::now(),
            )
            .await
            .is_ok()
        );

        let mut missing_reason = error;
        missing_reason.human_required_reason = None;
        assert!(matches!(
            record_provider_protocol_observation(
                None,
                &provider_id,
                None,
                "execution:test:mutation",
                &missing_reason,
                Utc::now(),
            )
            .await,
            Err(ProviderProtocolObservationRecordError::Invalid)
        ));
    }
}
