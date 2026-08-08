//! Versioned HTTP transport for Asterism core services.

use std::sync::Arc;

use asterism_provider_api::{ProviderMetadata, ProviderRegistry};
use asterism_storage::Database;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone, Debug)]
pub struct ApiState {
    pub database: Database,
    pub providers: Arc<ProviderRegistry>,
}

pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/system/health", get(health))
        .route("/api/v1/providers", get(list_providers))
        .route("/api/v1/openapi.json", get(openapi))
        .with_state(state)
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(X_REQUEST_ID.clone()))
        .layer(SetRequestIdLayer::new(X_REQUEST_ID, MakeRequestUuid))
}

async fn health(State(state): State<ApiState>) -> Result<Json<HealthResponse>, ApiError> {
    state
        .database
        .health_check()
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(HealthResponse {
        service: "asterismd".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        status: "ok".to_owned(),
        database: "ok".to_owned(),
    }))
}

async fn list_providers(State(state): State<ApiState>) -> Json<ListResponse<ProviderMetadata>> {
    let items: Vec<_> = state.providers.metadata().cloned().collect();
    Json(ListResponse {
        total: items.len(),
        items,
    })
}

async fn openapi() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Asterism internal API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Internal Phase 0 API; compatibility is stabilized after provider batch two."
        },
        "paths": {
            "/health": {"get": {"operationId": "health", "responses": {"200": {"description": "Service is healthy"}}}},
            "/api/v1/system/health": {"get": {"operationId": "systemHealth", "responses": {"200": {"description": "Core and database are healthy"}}}},
            "/api/v1/providers": {"get": {"operationId": "listProviders", "responses": {"200": {"description": "Registered provider metadata"}}}}
        }
    }))
}

async fn not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "not_found",
        message: "the requested API resource does not exist".to_owned(),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthResponse {
    pub service: String,
    pub version: String,
    pub status: String,
    pub database: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListResponse<T> {
    pub total: usize,
    pub items: Vec<T>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "API request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: "an internal service error occurred".to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: ErrorBody {
                    code: self.code.to_owned(),
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    use super::*;

    async fn test_router() -> Router {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        build_router(ApiState {
            database,
            providers: Arc::new(ProviderRegistry::default()),
        })
    }

    #[tokio::test]
    async fn health_is_versioned_and_has_a_request_id() {
        let response = test_router()
            .await
            .oneshot(
                Request::get("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-request-id"));

        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.status, "ok");
    }

    #[tokio::test]
    async fn unknown_routes_use_the_stable_error_shape() {
        let response = test_router()
            .await
            .oneshot(Request::get("/api/v1/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "not_found");
    }
}
