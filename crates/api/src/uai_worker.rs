use axum::{
    Extension, Json,
    extract::{Path, State},
};

use crate::{ApiError, ApiState, auth};
use asterism_uai_worker_client::UaiWorkerHealth;

pub(super) async fn health(
    State(state): State<ApiState>,
    Path(provider): Path<String>,
    Extension(auth): Extension<auth::AuthContext>,
) -> Result<Json<UaiWorkerHealth>, ApiError> {
    auth::require_provider_read(&auth)?;
    if !matches!(
        provider.as_str(),
        "chaoxing" | "welearn" | "uai" | "cidaren"
    ) {
        return Err(ApiError::not_found("provider_worker_not_found"));
    }
    let worker = state.provider_workers.get(&provider).ok_or_else(|| {
        let code = if provider == "uai" {
            "uai_worker_not_configured"
        } else {
            "provider_worker_not_configured"
        };
        ApiError::service_unavailable(code, "the upstream Provider worker is not configured")
    })?;
    worker.health().await.map(Json).map_err(|error| {
        tracing::warn!(%error, %provider, "upstream Provider worker health check failed");
        ApiError::service_unavailable(
            if provider == "uai" {
                "uai_worker_unavailable"
            } else {
                "provider_worker_unavailable"
            },
            "the upstream Provider worker is unavailable",
        )
    })
}
