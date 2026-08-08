//! Versioned HTTP transport for Asterism core services.

mod account;
mod auth;
mod rate_limit;

use std::sync::Arc;

use asterism_provider_api::{ProviderMetadata, ProviderRegistry};
use asterism_storage::Database;
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
const MAX_JSON_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct ApiState {
    database: Database,
    providers: Arc<ProviderRegistry>,
    session_ttl_seconds: u64,
    secure_cookies: bool,
    login_rate_limiter: rate_limit::LoginRateLimiter,
}

impl ApiState {
    pub fn new(
        database: Database,
        providers: Arc<ProviderRegistry>,
        session_ttl_seconds: u64,
        secure_cookies: bool,
    ) -> Self {
        Self {
            database,
            providers,
            session_ttl_seconds,
            secure_cookies,
            login_rate_limiter: rate_limit::LoginRateLimiter::default(),
        }
    }
}

pub fn build_router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/api/v1/auth/session", get(auth::current_identity))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/providers", get(list_providers))
        .route(
            "/api/v1/provider-accounts",
            get(account::list_provider_accounts).post(account::create_provider_account),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}",
            get(account::get_provider_account)
                .put(account::update_provider_account)
                .delete(account::delete_provider_account),
        )
        .route("/api/v1/service-tokens", post(auth::create_service_token))
        .route(
            "/api/v1/service-tokens/{token_id}",
            delete(auth::revoke_service_token),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ));
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/system/health", get(health))
        .route("/api/v1/openapi.json", get(openapi))
        .route("/api/v1/auth/bootstrap", post(auth::bootstrap_master))
        .route("/api/v1/auth/login", post(auth::login))
        .merge(protected)
        .with_state(state)
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(X_REQUEST_ID.clone()))
        .layer(SetRequestIdLayer::new(X_REQUEST_ID, MakeRequestUuid))
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
}

async fn health(State(state): State<ApiState>) -> Result<Json<HealthResponse>, ApiError> {
    state
        .database
        .health_check()
        .await
        .map_err(ApiError::internal)?;
    let outbox = state
        .database
        .outbox_health()
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(HealthResponse {
        service: "asterismd".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        status: "ok".to_owned(),
        database: "ok".to_owned(),
        schema_version: state
            .database
            .schema_version()
            .await
            .map_err(ApiError::internal)?,
        registered_providers: state.providers.len(),
        outbox_pending: outbox.pending,
        outbox_dead_letter: outbox.dead_letter,
    }))
}

async fn list_providers(
    State(state): State<ApiState>,
    Extension(auth): Extension<auth::AuthContext>,
) -> Result<Json<ListResponse<ProviderMetadata>>, ApiError> {
    auth::require_provider_read(&auth)?;
    let items: Vec<_> = state.providers.metadata().cloned().collect();
    Ok(Json(ListResponse {
        total: items.len(),
        items,
    }))
}

#[allow(
    clippy::too_many_lines,
    reason = "the declarative OpenAPI document is kept together for route/schema integrity"
)]
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
            "/api/v1/auth/bootstrap": {"post": {
                "operationId": "bootstrapMaster",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Credentials"}}}},
                "responses": {"200": {"description": "Initial Master and session created"}, "400": {"description": "Invalid request"}, "409": {"description": "Master already initialized"}}
            }},
            "/api/v1/auth/login": {"post": {
                "operationId": "login",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Credentials"}}}},
                "responses": {"200": {"description": "Session created"}, "401": {"description": "Invalid credentials"}, "429": {"description": "Rate limited"}}
            }},
            "/api/v1/auth/session": {"get": {
                "operationId": "currentIdentity",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "responses": {"200": {"description": "Authenticated identity"}, "401": {"description": "Authentication required"}}
            }},
            "/api/v1/auth/logout": {"post": {
                "operationId": "logout",
                "security": [{"cookieAuth": []}],
                "responses": {"204": {"description": "Web session revoked"}, "400": {"description": "Not a Web session"}, "401": {"description": "Authentication required"}}
            }},
            "/api/v1/providers": {"get": {"operationId": "listProviders", "security": [{"cookieAuth": []}, {"bearerAuth": []}], "responses": {"200": {"description": "Registered provider metadata"}}}},
            "/api/v1/provider-accounts": {
                "get": {
                    "operationId": "listProviderAccounts",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "responses": {"200": {"description": "Owner-scoped Provider accounts"}, "401": {"description": "Authentication required"}, "403": {"description": "Insufficient permission"}}
                },
                "post": {
                    "operationId": "createProviderAccount",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateProviderAccount"}}}},
                    "responses": {"201": {"description": "Provider account created"}, "400": {"description": "Invalid account"}, "401": {"description": "Authentication required"}, "403": {"description": "Insufficient permission"}}
                }
            },
            "/api/v1/provider-accounts/{account_id}": {
                "get": {
                    "operationId": "getProviderAccount",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"200": {"description": "Owner-scoped Provider account"}, "404": {"description": "Provider account not found"}}
                },
                "put": {
                    "operationId": "updateProviderAccount",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/UpdateProviderAccount"}}}},
                    "responses": {"200": {"description": "Provider account updated"}, "400": {"description": "Invalid account"}, "404": {"description": "Provider account not found"}}
                },
                "delete": {
                    "operationId": "deleteProviderAccount",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"204": {"description": "Provider account deleted"}, "404": {"description": "Provider account not found"}}
                }
            },
            "/api/v1/service-tokens": {"post": {
                "operationId": "createServiceToken",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateServiceToken"}}}},
                "responses": {"200": {"description": "Scoped token created; plaintext returned once"}, "400": {"description": "Invalid request"}, "401": {"description": "Authentication required"}, "403": {"description": "Insufficient permission"}}
            }},
            "/api/v1/service-tokens/{token_id}": {"delete": {
                "operationId": "revokeServiceToken",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "token_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "responses": {"204": {"description": "Service token revoked"}, "400": {"description": "Invalid token ID"}, "401": {"description": "Authentication required"}, "403": {"description": "Insufficient permission"}, "404": {"description": "Token not found"}}
            }}
        },
        "components": {
            "securitySchemes": {
                "cookieAuth": {"type": "apiKey", "in": "cookie", "name": "asterism_session"},
                "bearerAuth": {"type": "http", "scheme": "bearer"}
            },
            "schemas": {
                "Credentials": {
                    "type": "object",
                    "required": ["username", "password"],
                    "properties": {
                        "username": {"type": "string", "minLength": 1, "maxLength": 64},
                        "password": {"type": "string", "format": "password", "minLength": 1, "maxLength": 1024, "writeOnly": true}
                    },
                    "additionalProperties": false
                },
                "CreateServiceToken": {
                    "type": "object",
                    "required": ["name", "scopes"],
                    "properties": {
                        "name": {"type": "string", "minLength": 1, "maxLength": 128},
                        "scopes": {"type": "array", "minItems": 1, "uniqueItems": true, "items": {"$ref": "#/components/schemas/ServiceScope"}},
                        "expires_in_seconds": {"type": "integer", "minimum": 1}
                    },
                    "additionalProperties": false
                },
                "CreateProviderAccount": {
                    "type": "object",
                    "required": ["provider_id", "display_name"],
                    "properties": {
                        "provider_id": {"type": "string", "pattern": "^[a-z0-9-]{1,64}$"},
                        "display_name": {"type": "string", "minLength": 1, "maxLength": 128},
                        "tenant": {"type": ["string", "null"], "maxLength": 256}
                    },
                    "additionalProperties": false
                },
                "UpdateProviderAccount": {
                    "type": "object",
                    "required": ["display_name"],
                    "properties": {
                        "display_name": {"type": "string", "minLength": 1, "maxLength": 128},
                        "tenant": {"type": ["string", "null"], "maxLength": 256}
                    },
                    "additionalProperties": false
                },
                "ServiceScope": {
                    "type": "string",
                    "enum": [
                        "system_read", "provider_read", "provider_manage", "task_read",
                        "task_execute", "credit_read", "credit_manage", "audit_read",
                        "service_token_manage", "qq_identity_assert", "task_command_proxy",
                        "notification_delivery_report", "binding_verify"
                    ]
                }
            }
        }
    }))
}

async fn not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "not_found",
        message: "the requested API resource does not exist".to_owned(),
        retry_after_seconds: None,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthResponse {
    pub service: String,
    pub version: String,
    pub status: String,
    pub database: String,
    pub schema_version: i64,
    pub registered_providers: usize,
    pub outbox_pending: u64,
    pub outbox_dead_letter: u64,
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
    retry_after_seconds: Option<u64>,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "valid authentication is required".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn invalid_credentials() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_credentials",
            message: "username or password is invalid".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: "the authenticated identity lacks permission".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
            retry_after_seconds: None,
        }
    }

    fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: message.into(),
            retry_after_seconds: None,
        }
    }

    fn not_found(code: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message: "the requested resource does not exist".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn rate_limited(retry_after_seconds: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            message: "too many authentication attempts; retry later".to_owned(),
            retry_after_seconds: Some(retry_after_seconds),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "API request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: "an internal service error occurred".to_owned(),
            retry_after_seconds: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorResponse {
                error: ErrorBody {
                    code: self.code.to_owned(),
                    message: self.message,
                },
            }),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"asterism\""),
            );
        }
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
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
    use std::{net::SocketAddr, time::Duration};

    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Request, header},
    };
    use tower::ServiceExt;

    use super::*;

    async fn test_app(
        secure_cookies: bool,
        login_rate_limiter: Option<rate_limit::LoginRateLimiter>,
    ) -> (Router, Database) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let mut state = ApiState::new(
            database.clone(),
            Arc::new(ProviderRegistry::default()),
            3600,
            secure_cookies,
        );
        if let Some(login_rate_limiter) = login_rate_limiter {
            state.login_rate_limiter = login_rate_limiter;
        }
        (build_router(state), database)
    }

    async fn test_router() -> Router {
        test_app(false, None).await.0
    }

    async fn bootstrap(app: &Router) -> Response {
        app.clone()
            .oneshot(
                Request::post("/api/v1/auth/bootstrap")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"master","password":"correct-horse-battery-staple"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn login_request(password: &str) -> Request<Body> {
        Request::post("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41_000))))
            .body(Body::from(format!(
                r#"{{"username":"master","password":"{password}"}}"#
            )))
            .unwrap()
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

    #[tokio::test]
    async fn provider_route_requires_a_valid_bootstrapped_session() {
        let app = test_router().await;
        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap(),
            "Bearer realm=\"asterism\""
        );

        let bootstrap = bootstrap(&app).await;
        assert_eq!(bootstrap.status(), StatusCode::OK);
        assert_eq!(bootstrap.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            bootstrap.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("HttpOnly; SameSite=Strict")
        );
        let cookie = bootstrap
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let authorized = app
            .oneshot(
                Request::get("/api/v1/providers")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn provider_account_api_is_owner_scoped_and_audited() {
        let (app, database) = test_app(false, None).await;
        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/provider-accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/provider-accounts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"provider_id":"provider-alpha","display_name":"primary"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let created_body = to_bytes(created.into_body(), 16 * 1024).await.unwrap();
        let created: Value = serde_json::from_slice(&created_body).unwrap();
        let account_id = created["id"].as_str().unwrap();
        assert_eq!(created["credential_count"], 0);
        assert!(created.get("credential_refs").is_none());

        let updated = app
            .clone()
            .oneshot(
                Request::put(format!("/api/v1/provider-accounts/{account_id}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"display_name":"renamed","tenant":"tenant-a"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(updated.status(), StatusCode::OK);

        let listed = app
            .clone()
            .oneshot(
                Request::get("/api/v1/provider-accounts")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = to_bytes(listed.into_body(), 16 * 1024).await.unwrap();
        let listed: Value = serde_json::from_slice(&listed).unwrap();
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["items"][0]["display_name"], "renamed");

        let deleted = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/provider-accounts/{account_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        let missing = app
            .oneshot(
                Request::get(format!("/api/v1/provider-accounts/{account_id}"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('provider_account_created', 'provider_account_updated', \
                            'provider_account_deleted')",
        )
        .bind(account_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 3);
    }

    #[tokio::test]
    async fn malformed_auth_json_uses_the_stable_error_model() {
        let app = test_router().await;
        let response = app
            .oneshot(
                Request::post("/api/v1/auth/bootstrap")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "invalid_json");
    }

    #[tokio::test]
    async fn login_does_not_reveal_whether_a_username_exists() {
        let app = test_router().await;
        assert_eq!(bootstrap(&app).await.status(), StatusCode::OK);
        let known = app
            .clone()
            .oneshot(login_request("incorrect-password"))
            .await
            .unwrap();
        let unknown = app
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41_001))))
                    .body(Body::from(
                        r#"{"username":"unknown","password":"incorrect-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(known.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        let known_body = to_bytes(known.into_body(), 16 * 1024).await.unwrap();
        let unknown_body = to_bytes(unknown.into_body(), 16 * 1024).await.unwrap();
        assert_eq!(known_body, unknown_body);
    }

    #[tokio::test]
    async fn openapi_describes_every_authentication_route() {
        let response = test_router()
            .await
            .oneshot(
                Request::get("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let document: Value = serde_json::from_slice(&body).unwrap();
        for path in [
            "/api/v1/auth/bootstrap",
            "/api/v1/auth/login",
            "/api/v1/auth/session",
            "/api/v1/auth/logout",
            "/api/v1/service-tokens",
            "/api/v1/service-tokens/{token_id}",
            "/api/v1/provider-accounts",
            "/api/v1/provider-accounts/{account_id}",
        ] {
            assert!(document["paths"].get(path).is_some(), "missing {path}");
        }
        assert_eq!(
            document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
    }

    #[tokio::test]
    async fn login_is_rate_limited_by_principal_and_remote_ip() {
        let limiter = rate_limit::LoginRateLimiter::new(2, 10, Duration::from_mins(1));
        let (app, _) = test_app(false, Some(limiter)).await;
        assert_eq!(bootstrap(&app).await.status(), StatusCode::OK);
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(login_request("incorrect-password"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(login_request("incorrect-password"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response.headers().contains_key(header::RETRY_AFTER));
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "rate_limited");
    }

    #[tokio::test]
    async fn service_tokens_cannot_escalate_their_own_scopes() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie)
                    .body(Body::from(
                        r#"{"name":"limited","scopes":["service_token_manage"],"expires_in_seconds":3600}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(created.into_body(), 16 * 1024).await.unwrap();
        let created: Value = serde_json::from_slice(&body).unwrap();
        let plaintext = created["token"].as_str().unwrap().to_owned();
        let stored_digest: Vec<u8> =
            sqlx::query_scalar("SELECT token_hash FROM service_tokens WHERE name = 'limited'")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(stored_digest.len(), 32);
        assert_ne!(stored_digest, plaintext.as_bytes());

        let escalation = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {plaintext}"))
                    .body(Body::from(
                        r#"{"name":"escalated","scopes":["provider_manage"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(escalation.status(), StatusCode::FORBIDDEN);

        let subset = app
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("bearer {plaintext}"))
                    .body(Body::from(
                        r#"{"name":"rotation","scopes":["service_token_manage"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(subset.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn secure_cookie_configuration_is_applied_to_creation_and_removal() {
        let (app, _) = test_app(true, None).await;
        let bootstrap = bootstrap(&app).await;
        let set_cookie = bootstrap.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(set_cookie.contains("; Secure"));
        let cookie = set_cookie.split(';').next().unwrap();
        let logout = app
            .oneshot(
                Request::post("/api/v1/auth/logout")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        assert!(
            logout.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("; Secure;")
        );
    }
}
