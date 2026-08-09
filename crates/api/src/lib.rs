//! Versioned HTTP transport for Asterism core services.

mod account;
mod auth;
mod auth_bootstrap;
mod execution;
mod rate_limit;
mod task;

use std::sync::Arc;

use asterism_provider_api::{ProviderMetadata, ProviderRegistry};
use asterism_storage::{Database, SqliteSecretStore};
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
    secret_store: Option<SqliteSecretStore>,
    login_rate_limiter: rate_limit::LoginRateLimiter,
    bootstrap_claim_rate_limiter: rate_limit::LoginRateLimiter,
    bootstrap_credential_rate_limiter: rate_limit::LoginRateLimiter,
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
            secret_store: None,
            login_rate_limiter: rate_limit::LoginRateLimiter::default(),
            bootstrap_claim_rate_limiter: rate_limit::LoginRateLimiter::default(),
            bootstrap_credential_rate_limiter: rate_limit::LoginRateLimiter::default(),
        }
    }

    #[must_use]
    pub fn with_secret_store(mut self, secret_store: SqliteSecretStore) -> Self {
        self.secret_store = Some(secret_store);
        self
    }
}

pub fn build_router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/api/v1/auth/session", get(auth::current_identity))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/providers", get(list_providers))
        .route(
            "/api/v1/auth-bootstrap/sessions",
            post(auth_bootstrap::create_auth_bootstrap_session),
        )
        .route(
            "/api/v1/auth-bootstrap/sessions/{session_id}",
            get(auth_bootstrap::get_auth_bootstrap_session)
                .delete(auth_bootstrap::cancel_auth_bootstrap_session),
        )
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
        .route(
            "/api/v1/provider-accounts/{account_id}/scan",
            post(account::scan_provider_account),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/credentials",
            axum::routing::put(account::put_provider_credentials),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/auth-sessions",
            post(account::begin_auth_session),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/auth-sessions/latest",
            get(account::get_latest_auth_session),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}",
            get(account::get_auth_session).delete(account::cancel_auth_session),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/credentials",
            axum::routing::put(account::put_auth_session_credentials),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/scan-schedule",
            get(account::get_scan_schedule).put(account::configure_scan_schedule),
        )
        .route("/api/v1/tasks", get(task::list_tasks))
        .route("/api/v1/tasks/{task_id}", get(task::get_task))
        .route("/api/v1/tasks/{task_id}/execute", post(task::execute_task))
        .merge(execution_routes())
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
        .route(
            "/api/v1/auth-bootstrap/sessions/{session_id}/claim",
            post(auth_bootstrap::claim_auth_bootstrap_session),
        )
        .route(
            "/api/v1/auth-bootstrap/sessions/{session_id}/stream",
            get(auth_bootstrap::get_auth_bootstrap_stream_snapshot),
        )
        .route(
            "/api/v1/auth-bootstrap/sessions/{session_id}/events",
            post(auth_bootstrap::record_auth_bootstrap_event),
        )
        .route(
            "/api/v1/auth-bootstrap/sessions/{session_id}/credential",
            post(auth_bootstrap::submit_auth_bootstrap_credential),
        )
        .merge(protected)
        .with_state(state)
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(X_REQUEST_ID.clone()))
        .layer(SetRequestIdLayer::new(X_REQUEST_ID, MakeRequestUuid))
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
}

fn execution_routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/executions", get(execution::list_executions))
        .route(
            "/api/v1/executions/{execution_id}",
            get(execution::get_execution),
        )
        .route(
            "/api/v1/executions/{execution_id}/logs",
            get(execution::list_execution_logs),
        )
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
        secret_store_configured: state.secret_store.is_some(),
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
    let mut document = json!({
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
            "/api/v1/auth-bootstrap/sessions": {"post": {
                "operationId": "createAuthBootstrapSession",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateAuthBootstrapSession"}}}},
                "responses": {
                    "201": {"description": "Pairing session created; plaintext pairing token returned once"},
                    "400": {"description": "Invalid purpose or account binding"},
                    "404": {"description": "Provider account not found"},
                    "409": {"description": "Provider has no Capture recipe"}
                }
            }},
            "/api/v1/auth-bootstrap/sessions/{session_id}": {
                "get": {
                    "operationId": "getAuthBootstrapSession",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"200": {"description": "Owner-scoped pairing session"}, "404": {"description": "Pairing session not found"}}
                },
                "delete": {
                    "operationId": "cancelAuthBootstrapSession",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"200": {"description": "Pairing session cancelled or expired"}, "404": {"description": "Pairing session not found"}, "409": {"description": "Pairing session is terminal or changed"}}
                }
            },
            "/api/v1/auth-bootstrap/sessions/{session_id}/claim": {"post": {
                "operationId": "claimAuthBootstrapSession",
                "security": [{"bootstrapAuth": []}],
                "parameters": [{"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "responses": {
                    "200": {"description": "Pairing claimed; scoped access token returned once"},
                    "401": {"description": "Pairing token is invalid, expired, cancelled, or already used"},
                    "429": {"description": "Pairing claim rate limit reached"}
                }
            }},
            "/api/v1/auth-bootstrap/sessions/{session_id}/stream": {"get": {
                "operationId": "pollAuthBootstrapStream",
                "security": [{"bootstrapAuth": []}],
                "parameters": [{"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "responses": {
                    "200": {"description": "Current non-secret session snapshot for the HTTP polling fallback"},
                    "401": {"description": "Session-scoped Bootstrap access token is invalid or expired"}
                }
            }},
            "/api/v1/auth-bootstrap/sessions/{session_id}/events": {"post": {
                "operationId": "recordAuthBootstrapEvent",
                "security": [{"bootstrapAuth": []}],
                "parameters": [{"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/RecordAuthBootstrapEvent"}}}},
                "responses": {
                    "200": {"description": "Exact event retry accepted with the original server timestamp"},
                    "201": {"description": "Client status event recorded"},
                    "400": {"description": "Invalid event payload"},
                    "401": {"description": "Session-scoped Bootstrap access token is invalid or expired"},
                    "409": {"description": "Event sequence changed or is not contiguous"}
                }
            }},
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
            "/api/v1/provider-accounts/{account_id}/scan": {"post": {
                "operationId": "scanProviderAccount",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "responses": {
                    "200": {"description": "Provider inventory collected and committed"},
                    "404": {"description": "Provider account not found"},
                    "409": {"description": "Account or Provider is not ready for scanning"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider returned inconsistent inventory"},
                    "503": {"description": "Provider is temporarily unavailable"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/credentials": {"put": {
                "operationId": "replaceProviderAccountCredentials",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/PutProviderCredentials"}}}},
                "responses": {
                    "200": {"description": "Credentials validated, encrypted, and committed"},
                    "400": {"description": "Invalid credential bundle"},
                    "404": {"description": "Provider account not found"},
                    "409": {"description": "Provider rejected or does not support the credential"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider returned inconsistent authentication data"},
                    "503": {"description": "Provider or encrypted credential store unavailable"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/auth-sessions": {"post": {
                "operationId": "beginProviderAccountAuthSession",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/BeginAuthSession"}}}},
                "responses": {
                    "201": {"description": "Authentication session created with its first challenge"},
                    "404": {"description": "Provider account not found"},
                    "409": {"description": "Provider does not support the method or requires action"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider returned an inconsistent challenge"},
                    "503": {"description": "Provider is temporarily unavailable"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/auth-sessions/latest": {"get": {
                "operationId": "getLatestProviderAccountAuthSession",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "responses": {"200": {"description": "Latest owner-scoped authentication state"}, "404": {"description": "Authentication session not found"}}
            }},
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}": {
                "get": {
                    "operationId": "getProviderAccountAuthSession",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [
                        {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                        {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                    ],
                    "responses": {"200": {"description": "Owner-scoped authentication state"}, "404": {"description": "Authentication session not found"}}
                },
                "delete": {
                    "operationId": "cancelProviderAccountAuthSession",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [
                        {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                        {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                    ],
                    "responses": {"200": {"description": "Authentication session cancelled"}, "404": {"description": "Authentication session not found"}, "409": {"description": "Authentication session is terminal or changed"}}
                }
            },
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/credentials": {
                "put": {
                    "operationId": "submitProviderAccountAuthSessionCredentials",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [
                        {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                        {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                    ],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/PutProviderCredentials"}}}},
                    "responses": {
                        "200": {"description": "Credentials validated and committed with the authenticated session"},
                        "400": {"description": "Invalid credential bundle"},
                        "404": {"description": "Provider account or authentication session not found"},
                        "409": {"description": "Session changed, expired, or Provider rejected the credential"},
                        "429": {"description": "Provider rate limit reached"},
                        "502": {"description": "Provider returned inconsistent authentication data"},
                        "503": {"description": "Provider or encrypted credential store unavailable"}
                    }
                }
            },
            "/api/v1/provider-accounts/{account_id}/scan-schedule": {
                "get": {
                    "operationId": "getProviderAccountScanSchedule",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"200": {"description": "Owner-scoped scan schedule"}, "404": {"description": "Account or scan schedule not found"}}
                },
                "put": {
                    "operationId": "configureProviderAccountScanSchedule",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ConfigureScanSchedule"}}}},
                    "responses": {"200": {"description": "Scan schedule configured with the Provider floor"}, "400": {"description": "Invalid interval"}, "404": {"description": "Provider account not found"}, "409": {"description": "Provider is not registered"}}
                }
            },
            "/api/v1/tasks": {"get": {
                "operationId": "listTasks",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "provider_account_id", "in": "query", "schema": {"type": "string", "format": "uuid"}},
                    {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
                    {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
                ],
                "responses": {"200": {"description": "Owner-scoped paginated tasks"}, "400": {"description": "Invalid query"}, "401": {"description": "Authentication required"}, "403": {"description": "Insufficient permission"}}
            }},
            "/api/v1/tasks/{task_id}": {"get": {
                "operationId": "getTask",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "responses": {"200": {"description": "Owner-scoped task"}, "400": {"description": "Invalid task ID"}, "404": {"description": "Task not found"}}
            }},
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
                "bearerAuth": {"type": "http", "scheme": "bearer"},
                "bootstrapAuth": {"type": "apiKey", "in": "header", "name": "Authorization", "description": "Bootstrap followed by a pairing or session-scoped access token, as required by the route"}
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
                "CreateAuthBootstrapSession": {
                    "type": "object",
                    "required": ["provider_id", "purpose"],
                    "properties": {
                        "provider_id": {"type": "string", "pattern": "^[a-z0-9-]{1,64}$"},
                        "provider_account_id": {"type": ["string", "null"], "format": "uuid"},
                        "purpose": {"type": "string", "enum": ["add_account", "reauthenticate", "repair_session"]}
                    },
                    "additionalProperties": false
                },
                "RecordAuthBootstrapEvent": {
                    "type": "object",
                    "required": ["sequence", "kind"],
                    "properties": {
                        "sequence": {"type": "integer", "minimum": 1, "maximum": 9_223_372_036_854_775_807_u64},
                        "kind": {
                            "type": "object",
                            "required": ["type"],
                            "properties": {
                                "type": {"type": "string", "enum": ["client_ready", "stage_changed", "progress", "credential_detected", "validating", "authenticated", "failed"]},
                                "stage": {"type": "string", "pattern": "^[a-z0-9._-]{1,64}$"},
                                "percent": {"type": "integer", "minimum": 0, "maximum": 100},
                                "code": {"type": "string", "pattern": "^[a-z0-9._-]{1,64}$"}
                            },
                            "additionalProperties": false
                        }
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
                "PutProviderCredentials": {
                    "type": "object",
                    "required": ["auth_method", "acquired_via", "session_kind", "fields"],
                    "properties": {
                        "auth_method": {"type": "string", "enum": ["password", "qr_code", "external_browser_oauth", "assisted_session", "imported_cookie", "imported_token"]},
                        "acquired_via": {"type": "string", "enum": ["native_provider_login", "capture_tool", "browser_extension", "android_helper", "manual_import"]},
                        "session_kind": {"type": "string", "enum": ["cookie", "bearer_token", "jwt", "composite", "provider_specific"]},
                        "expires_at": {"type": ["string", "null"], "format": "date-time"},
                        "fields": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 16,
                            "items": {
                                "type": "object",
                                "required": ["purpose", "value"],
                                "properties": {
                                    "purpose": {"type": "string", "enum": ["provider_username", "provider_password", "provider_cookie", "provider_access_token", "provider_refresh_token", "provider_composite_session"]},
                                    "value": {"type": "string", "minLength": 1, "writeOnly": true}
                                },
                                "additionalProperties": false
                            }
                        }
                    },
                    "additionalProperties": false
                },
                "BeginAuthSession": {
                    "type": "object",
                    "required": ["method"],
                    "properties": {
                        "method": {"type": "string", "enum": ["password", "qr_code", "external_browser_oauth", "assisted_session", "imported_cookie", "imported_token"]}
                    },
                    "additionalProperties": false
                },
                "ConfigureScanSchedule": {
                    "type": "object",
                    "required": ["desired_interval_seconds", "enabled"],
                    "properties": {
                        "desired_interval_seconds": {"type": "integer", "minimum": 1},
                        "enabled": {"type": "boolean"}
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
    });
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/executions".to_owned(), execution_list_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/executions/{execution_id}/logs".to_owned(),
            execution_logs_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/executions/{execution_id}".to_owned(),
            execution_detail_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/execute".to_owned(),
            task_execute_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/auth-bootstrap/sessions/{session_id}/credential".to_owned(),
            auth_bootstrap_credential_path(),
        );
    document["components"]["schemas"]
        .as_object_mut()
        .expect("static OpenAPI schemas object")
        .insert(
            "SubmitAuthBootstrapCredential".to_owned(),
            auth_bootstrap_credential_schema(),
        );
    Json(document)
}

fn execution_detail_path() -> Value {
    json!({"get": {
        "operationId": "getExecution",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "execution_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Owner-scoped Execution with current progress and Attempt history"},
            "400": {"description": "Invalid Execution ID"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"},
            "404": {"description": "Execution not found for this owner"}
        }
    }})
}

fn execution_list_path() -> Value {
    json!({"get": {
        "operationId": "listExecutions",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "query", "schema": {"type": "string", "format": "uuid"}},
            {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
            {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
        ],
        "responses": {
            "200": {"description": "Owner-scoped Execution page, optionally filtered by Task"},
            "400": {"description": "Invalid Task ID or pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"}
        }
    }})
}

fn execution_logs_path() -> Value {
    json!({"get": {
        "operationId": "listExecutionLogs",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "execution_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
            {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
        ],
        "responses": {
            "200": {"description": "Owner-scoped chronological Execution log page"},
            "400": {"description": "Invalid Execution ID or pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"},
            "404": {"description": "Execution not found for this owner"}
        }
    }})
}

fn task_execute_path() -> Value {
    json!({"post": {
        "operationId": "executeTask",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "Idempotency-Key", "in": "header", "required": true, "schema": {"type": "string", "minLength": 1, "maxLength": 256}}
        ],
        "responses": {
            "200": {"description": "Idempotent replay of an existing Execution"},
            "201": {"description": "Execution scheduled atomically"},
            "400": {"description": "Invalid task ID or idempotency key"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"},
            "404": {"description": "Task not found for this owner"},
            "409": {"description": "Task state, capability, assessment policy, or idempotency conflict"}
        }
    }})
}

fn auth_bootstrap_credential_schema() -> Value {
    json!({
        "type": "object",
        "required": ["provider_id", "auth_method", "session_kind", "fields"],
        "properties": {
            "display_name": {"type": ["string", "null"], "minLength": 1, "maxLength": 128},
            "provider_id": {"type": "string", "pattern": "^[a-z0-9-]{1,64}$"},
            "tenant": {"type": ["string", "null"], "minLength": 1, "maxLength": 256},
            "auth_method": {"type": "string", "enum": ["password", "qr_code", "external_browser_oauth", "assisted_session", "imported_cookie", "imported_token"]},
            "session_kind": {"type": "string", "enum": ["cookie", "bearer_token", "jwt", "composite", "provider_specific"]},
            "expires_at": {"type": ["string", "null"], "format": "date-time"},
            "fields": {
                "type": "array",
                "minItems": 1,
                "maxItems": 16,
                "items": {
                    "type": "object",
                    "required": ["purpose", "value"],
                    "properties": {
                        "purpose": {"type": "string", "enum": ["provider_username", "provider_password", "provider_cookie", "provider_access_token", "provider_refresh_token", "provider_composite_session"]},
                        "value": {"type": "string", "minLength": 1, "writeOnly": true}
                    },
                    "additionalProperties": false
                }
            }
        },
        "additionalProperties": false
    })
}

fn auth_bootstrap_credential_path() -> Value {
    json!({"post": {
        "operationId": "submitAuthBootstrapCredential",
        "security": [{"bootstrapAuth": []}],
        "parameters": [{"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/SubmitAuthBootstrapCredential"}}}},
        "responses": {
            "200": {"description": "Provider-validated credentials committed and pairing completed"},
            "400": {"description": "Invalid account metadata or credential bundle"},
            "401": {"description": "Session-scoped Bootstrap access token is invalid or expired"},
            "409": {"description": "Provider rejected the credential or the account binding changed"},
            "429": {"description": "Credential submission rate limit reached"},
            "502": {"description": "Provider returned an inconsistent authentication result"},
            "503": {"description": "Encrypted credential storage is unavailable"}
        }
    }})
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
    pub secret_store_configured: bool,
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

    fn invalid_bootstrap_token() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_bootstrap_token",
            message: "the pairing token is invalid or expired".to_owned(),
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

    fn provider_rate_limited(retry_after_seconds: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "provider_rate_limited",
            message: "the Provider rate limit was reached; retry later".to_owned(),
            retry_after_seconds: Some(retry_after_seconds),
        }
    }

    fn bad_gateway(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code,
            message: message.into(),
            retry_after_seconds: None,
        }
    }

    fn service_unavailable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
            message: message.into(),
            retry_after_seconds: None,
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
                if self.code == "invalid_bootstrap_token" {
                    HeaderValue::from_static("Bootstrap realm=\"asterism\"")
                } else {
                    HeaderValue::from_static("Bearer realm=\"asterism\"")
                },
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
    use std::{
        collections::{BTreeMap, BTreeSet},
        net::SocketAddr,
        time::Duration,
    };

    use asterism_domain::{
        AssessmentClass, AuthMethod, AuthState, RemoteState, SessionKind, SourceType,
    };
    use asterism_provider_api::{
        AuthChallenge, AuthenticationCapability, CourseInventoryCapability, CredentialValidation,
        ProviderAuthContext, ProviderCapability, ProviderContext, ProviderEntry, ProviderIdentity,
        ProviderMetadata, ProviderResult, RemoteCourse, RemoteTask, SessionStatus,
        TaskInventoryCapability, VerificationLevel,
    };
    use asterism_secrets::{CredentialBundle, SecretKey};
    use asterism_storage::SecretKeyring;
    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Request, header},
    };
    use chrono::{SecondsFormat, Utc};
    use tower::ServiceExt;

    use super::*;

    #[derive(Debug)]
    struct ApiScanInventory {
        metadata: ProviderMetadata,
    }

    impl ProviderIdentity for ApiScanInventory {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for ApiScanInventory {
        async fn list_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            Ok(vec![RemoteCourse {
                remote_id: "course-a".to_owned(),
                title: "course".to_owned(),
                term: None,
                teacher: None,
                remote_status: None,
                metadata_sanitized: json!({"revision": 1}),
                route_context: asterism_provider_api::ProviderRouteContext::default(),
            }])
        }
    }

    #[async_trait]
    impl TaskInventoryCapability for ApiScanInventory {
        async fn list_tasks(
            &self,
            _context: &ProviderContext,
            course: Option<&RemoteCourse>,
        ) -> ProviderResult<Vec<RemoteTask>> {
            Ok(vec![RemoteTask {
                remote_id: "task-a".to_owned(),
                course_remote_id: course.map(|course| course.remote_id.clone()),
                title: "task".to_owned(),
                source_type: SourceType::Work,
                assessment_class: AssessmentClass::Unknown,
                remote_state: RemoteState::Pending,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "v1:fingerprint-a".to_owned(),
                normalized: json!({"revision": 1}),
                raw_sanitized: json!({"task": "safe"}),
            }])
        }
    }

    fn scan_registry() -> ProviderRegistry {
        let metadata = ProviderMetadata {
            id: asterism_domain::ProviderId::new("provider-alpha").unwrap(),
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: Some(300),
            capture_recipe_version: None,
            capabilities: BTreeSet::from([
                ProviderCapability::CourseInventory,
                ProviderCapability::TaskInventory,
            ]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let inventory = Arc::new(ApiScanInventory {
            metadata: metadata.clone(),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata,
                authentication: None,
                course_inventory: Some(inventory.clone()),
                task_inventory: Some(inventory),
                task_detail: None,
                task_progress: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        registry
    }

    #[derive(Debug)]
    struct ApiCredentialAuthentication {
        metadata: ProviderMetadata,
        valid: bool,
    }

    impl ProviderIdentity for ApiCredentialAuthentication {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl AuthenticationCapability for ApiCredentialAuthentication {
        async fn begin_authentication(
            &self,
            context: &ProviderAuthContext,
            method: AuthMethod,
        ) -> ProviderResult<AuthChallenge> {
            Ok(AuthChallenge {
                session_id: context.auth_session_id.unwrap_or_default(),
                method,
                waiting_for: asterism_domain::WaitingUserState::SessionImport,
                user_action: None,
                expires_at: None,
            })
        }

        async fn validate_credential(
            &self,
            _context: &ProviderAuthContext,
            credential: &CredentialBundle,
        ) -> ProviderResult<CredentialValidation> {
            assert_eq!(credential.fields.len(), 1);
            assert!(!credential.fields[0].value.expose_secret().is_empty());
            Ok(CredentialValidation::accepted(SessionStatus {
                valid: self.valid,
                kind: SessionKind::Cookie,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                account_hint: Some("remote-account".to_owned()),
            }))
        }

        async fn validate_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<SessionStatus> {
            Ok(SessionStatus {
                valid: self.valid,
                kind: SessionKind::Cookie,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                account_hint: Some("remote-account".to_owned()),
            })
        }
    }

    fn credential_registry(valid: bool) -> ProviderRegistry {
        let metadata = ProviderMetadata {
            id: asterism_domain::ProviderId::new("provider-alpha").unwrap(),
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: Some(3),
            capabilities: BTreeSet::from([ProviderCapability::Authentication]),
            auth_methods: BTreeSet::from([AuthMethod::ImportedCookie]),
            session_kinds: BTreeSet::from([SessionKind::Cookie]),
        };
        let authentication = Arc::new(ApiCredentialAuthentication {
            metadata: metadata.clone(),
            valid,
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                authentication: Some(authentication),
                ..ProviderEntry::metadata_only(metadata)
            })
            .unwrap();
        registry
    }

    async fn credential_test_app(valid: bool) -> (Router, Database) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let secret_store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(
                SecretKeyring::new(
                    "key-a",
                    BTreeMap::from([("key-a".to_owned(), SecretKey::new([9; 32]))]),
                )
                .unwrap(),
            ),
        );
        let state = ApiState::new(
            database.clone(),
            Arc::new(credential_registry(valid)),
            3600,
            false,
        )
        .with_secret_store(secret_store);
        (build_router(state), database)
    }

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

    async fn create_test_provider_account(app: &Router, cookie: &str) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/provider-accounts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie)
                    .body(Body::from(
                        r#"{"provider_id":"provider-alpha","display_name":"primary"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        body["id"].as_str().unwrap().to_owned()
    }

    async fn create_test_auth_bootstrap(app: &Router, cookie: &str) -> Value {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth-bootstrap/sessions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie)
                    .body(Body::from(
                        r#"{"provider_id":"provider-alpha","purpose":"add_account"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        response_json(response).await
    }

    async fn response_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        serde_json::from_slice(&body).unwrap()
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

    fn auth_bootstrap_event_request(session_id: &str, token: &str, body: &str) -> Request<Body> {
        Request::post(format!(
            "/api/v1/auth-bootstrap/sessions/{session_id}/events"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bootstrap {token}"))
        .body(Body::from(body.to_owned()))
        .unwrap()
    }

    fn auth_bootstrap_credential_request(
        session_id: &str,
        token: &str,
        value: &str,
    ) -> Request<Body> {
        Request::post(format!(
            "/api/v1/auth-bootstrap/sessions/{session_id}/credential"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bootstrap {token}"))
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 7], 42_007))))
        .body(Body::from(format!(
            r#"{{"display_name":"primary","provider_id":"provider-alpha","auth_method":"imported_cookie","session_kind":"cookie","fields":[{{"purpose":"provider_cookie","value":"{value}"}}]}}"#
        )))
        .unwrap()
    }

    fn auth_bootstrap_existing_credential_request(
        session_id: &str,
        token: &str,
        value: &str,
    ) -> Request<Body> {
        Request::post(format!(
            "/api/v1/auth-bootstrap/sessions/{session_id}/credential"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bootstrap {token}"))
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 9], 42_009))))
        .body(Body::from(format!(
            r#"{{"provider_id":"provider-alpha","auth_method":"imported_cookie","session_kind":"cookie","fields":[{{"purpose":"provider_cookie","value":"{value}"}}]}}"#
        )))
        .unwrap()
    }

    async fn claim_test_auth_bootstrap(
        app: &Router,
        session_id: &str,
        pairing_token: &str,
        remote: SocketAddr,
    ) -> String {
        let claimed = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/auth-bootstrap/sessions/{session_id}/claim"
                ))
                .header(header::AUTHORIZATION, format!("Bootstrap {pairing_token}"))
                .extension(ConnectInfo(remote))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK);
        response_json(claimed).await["access_token"]
            .as_str()
            .unwrap()
            .to_owned()
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
        assert!(!health.secret_store_configured);
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
    async fn provider_credential_api_validates_before_returning_sanitized_metadata() {
        let (app, database) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let response = app
            .clone()
            .oneshot(
                Request::put(format!(
                    "/api/v1/provider-accounts/{account_id}/credentials"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(
                    r#"{"auth_method":"imported_cookie","acquired_via":"manual_import","session_kind":"cookie","fields":[{"purpose":"provider_cookie","value":"credential-value"}]}"#,
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        assert!(
            !body
                .as_ref()
                .windows(b"credential-value".len())
                .any(|window| window == b"credential-value")
        );
        let response: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["credential_count"], 1);
        assert_eq!(response["status"]["valid"], true);
        let auth_state: String =
            sqlx::query_scalar("SELECT auth_state_json FROM provider_accounts WHERE id = ?")
                .bind(&account_id)
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<AuthState>(&auth_state).unwrap(),
            AuthState::Authenticated
        );
    }

    #[tokio::test]
    async fn auth_bootstrap_api_issues_and_claims_one_time_tokens() {
        let (app, database) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap().to_owned();
        let pairing_token = created["pairing_token"].as_str().unwrap().to_owned();
        assert!(pairing_token.starts_with("ast_pair_"));
        assert_eq!(created["session"]["required_recipe_version"], 3);
        assert_eq!(created["session"]["state"], "awaiting_claim");

        let fetched = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/auth-bootstrap/sessions/{session_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);
        assert!(response_json(fetched).await.get("pairing_token").is_none());
        let claim_path = format!("/api/v1/auth-bootstrap/sessions/{session_id}/claim");
        let wrong = app
            .clone()
            .oneshot(
                Request::post(&claim_path)
                    .header(header::AUTHORIZATION, "Bootstrap ast_pair_wrong")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42_001))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            wrong.headers()[header::WWW_AUTHENTICATE],
            "Bootstrap realm=\"asterism\""
        );
        let claimed = app
            .clone()
            .oneshot(
                Request::post(&claim_path)
                    .header(header::AUTHORIZATION, format!("Bootstrap {pairing_token}"))
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42_001))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK);
        assert_eq!(claimed.headers()[header::CACHE_CONTROL], "no-store");
        let claimed = response_json(claimed).await;
        assert_eq!(claimed["session"]["state"], "claimed");
        assert!(
            claimed["access_token"]
                .as_str()
                .unwrap()
                .starts_with("ast_boot_")
        );
        let replay = app
            .clone()
            .oneshot(
                Request::post(claim_path)
                    .header(header::AUTHORIZATION, format!("Bootstrap {pairing_token}"))
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42_001))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('auth_bootstrap_session_created', 'auth_bootstrap_session_claimed')",
        )
        .bind(session_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 2);
    }

    #[tokio::test]
    async fn auth_bootstrap_owner_cancel_invalidates_the_pairing_token() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let cancellable = create_test_auth_bootstrap(&app, &cookie).await;
        let cancellable_id = cancellable["session"]["id"].as_str().unwrap();
        let cancellable_token = cancellable["pairing_token"].as_str().unwrap();
        let cancelled = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/auth-bootstrap/sessions/{cancellable_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(response_json(cancelled).await["state"], "cancelled");
        let cancelled_claim = app
            .oneshot(
                Request::post(format!(
                    "/api/v1/auth-bootstrap/sessions/{cancellable_id}/claim"
                ))
                .header(
                    header::AUTHORIZATION,
                    format!("Bootstrap {cancellable_token}"),
                )
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42_001))))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled_claim.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_bootstrap_access_token_is_scoped_to_one_live_session() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap();
        let pairing_token = created["pairing_token"].as_str().unwrap();
        let claimed = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/auth-bootstrap/sessions/{session_id}/claim"
                ))
                .header(header::AUTHORIZATION, format!("Bootstrap {pairing_token}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 3], 42_003))))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let claimed = response_json(claimed).await;
        let access_token = claimed["access_token"].as_str().unwrap();
        let stream_path = format!("/api/v1/auth-bootstrap/sessions/{session_id}/stream");
        let snapshot = app
            .clone()
            .oneshot(
                Request::get(&stream_path)
                    .header(header::AUTHORIZATION, format!("Bootstrap {access_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status(), StatusCode::OK);
        assert_eq!(response_json(snapshot).await["state"], "claimed");

        let other = create_test_auth_bootstrap(&app, &cookie).await;
        let other_id = other["session"]["id"].as_str().unwrap();
        let cross_session = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/auth-bootstrap/sessions/{other_id}/stream"))
                    .header(header::AUTHORIZATION, format!("Bootstrap {access_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cross_session.status(), StatusCode::UNAUTHORIZED);
        let cancelled = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/auth-bootstrap/sessions/{session_id}"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        let terminal = app
            .oneshot(
                Request::get(stream_path)
                    .header(header::AUTHORIZATION, format!("Bootstrap {access_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(terminal.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_bootstrap_events_require_scoped_access_and_preserve_exact_retries() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap().to_owned();
        let pairing_token = created["pairing_token"].as_str().unwrap().to_owned();

        let pairing_rejected = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &pairing_token,
                r#"{"sequence":1,"kind":{"type":"client_ready"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(pairing_rejected.status(), StatusCode::UNAUTHORIZED);

        let access_token = claim_test_auth_bootstrap(
            &app,
            &session_id,
            &pairing_token,
            SocketAddr::from(([127, 0, 0, 4], 42_004)),
        )
        .await;

        let client_timestamp = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"client_ready"},"received_at":"2026-08-09T00:00:00Z"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(client_timestamp.status(), StatusCode::BAD_REQUEST);
        let arbitrary_message = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"client_ready","message":"unsafe"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(arbitrary_message.status(), StatusCode::BAD_REQUEST);

        let first = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"client_ready"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        assert_eq!(first.headers()[header::CACHE_CONTROL], "no-store");
        let first = response_json(first).await;
        assert_eq!(first["duplicate"], false);
        assert_eq!(first["event"]["sequence"], 1);
        let first_received_at = first["event"]["received_at"].clone();

        let retry = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"client_ready"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::OK);
        let retry = response_json(retry).await;
        assert_eq!(retry["duplicate"], true);
        assert_eq!(retry["event"]["received_at"], first_received_at);

        let other = create_test_auth_bootstrap(&app, &cookie).await;
        let other_id = other["session"]["id"].as_str().unwrap();
        let cross_session = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                other_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"client_ready"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(cross_session.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_bootstrap_event_sequences_conflict_without_advancing_core_state() {
        let (app, database) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap().to_owned();
        let pairing_token = created["pairing_token"].as_str().unwrap();
        let access_token = claim_test_auth_bootstrap(
            &app,
            &session_id,
            pairing_token,
            SocketAddr::from(([127, 0, 0, 5], 42_005)),
        )
        .await;

        let first = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"client_ready"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);

        let changed = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":1,"kind":{"type":"validating"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(changed.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(changed).await["error"]["code"],
            "auth_bootstrap_event_sequence_conflict"
        );

        let gap = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":3,"kind":{"type":"credential_detected"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(gap.status(), StatusCode::CONFLICT);

        let reported = app
            .clone()
            .oneshot(auth_bootstrap_event_request(
                &session_id,
                &access_token,
                r#"{"sequence":2,"kind":{"type":"authenticated"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(reported.status(), StatusCode::CREATED);
        assert_eq!(
            response_json(reported).await["event"]["kind"]["type"],
            "authenticated"
        );

        let session = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/auth-bootstrap/sessions/{session_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let session = response_json(session).await;
        assert_eq!(session["state"], "claimed");
        assert_eq!(session["revision"], 2);

        let event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM auth_bootstrap_client_events WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(event_count, 2);
    }

    #[tokio::test]
    async fn auth_bootstrap_credential_is_provider_validated_committed_and_one_time() {
        let (app, database) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap().to_owned();
        let pairing_token = created["pairing_token"].as_str().unwrap().to_owned();
        let access_token = claim_test_auth_bootstrap(
            &app,
            &session_id,
            &pairing_token,
            SocketAddr::from(([127, 0, 0, 6], 42_006)),
        )
        .await;

        let wrong_family = app
            .clone()
            .oneshot(auth_bootstrap_credential_request(
                &session_id,
                &pairing_token,
                "wrong-family-cookie",
            ))
            .await
            .unwrap();
        assert_eq!(wrong_family.status(), StatusCode::UNAUTHORIZED);
        let submitted = app
            .clone()
            .oneshot(auth_bootstrap_credential_request(
                &session_id,
                &access_token,
                "captured-cookie",
            ))
            .await
            .unwrap();
        assert_eq!(submitted.status(), StatusCode::OK);
        assert_eq!(submitted.headers()[header::CACHE_CONTROL], "no-store");
        let submitted = response_json(submitted).await;
        assert_eq!(submitted["session"]["state"], "completed");
        assert_eq!(submitted["credential_count"], 1);
        assert_eq!(submitted["status"]["valid"], true);
        assert!(submitted.get("access_token").is_none());
        assert!(!submitted.to_string().contains("captured-cookie"));

        let owner_view = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/auth-bootstrap/sessions/{session_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let owner_view = response_json(owner_view).await;
        assert_eq!(owner_view["state"], "completed");
        assert_eq!(
            owner_view["provider_account_id"],
            submitted["provider_account_id"]
        );
        let replay = app
            .clone()
            .oneshot(auth_bootstrap_credential_request(
                &session_id,
                &access_token,
                "captured-cookie",
            ))
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        let account_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_accounts")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret_blobs")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let acquisition: String =
            sqlx::query_scalar("SELECT acquired_via FROM provider_account_credentials")
                .fetch_one(database.pool())
                .await
                .unwrap();
        let leaked_audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE metadata_sanitized_json LIKE ?",
        )
        .bind("%captured-cookie%")
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(account_count, 1);
        assert_eq!(secret_count, 1);
        assert_eq!(acquisition, "capture_tool");
        assert_eq!(leaked_audit_count, 0);
    }

    #[tokio::test]
    async fn auth_bootstrap_credential_reauthenticates_the_bound_account() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth-bootstrap/sessions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(format!(
                        r#"{{"provider_id":"provider-alpha","provider_account_id":"{account_id}","purpose":"reauthenticate"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created = response_json(created).await;
        let session_id = created["session"]["id"].as_str().unwrap();
        let pairing_token = created["pairing_token"].as_str().unwrap();
        let access_token = claim_test_auth_bootstrap(
            &app,
            session_id,
            pairing_token,
            SocketAddr::from(([127, 0, 0, 10], 42_010)),
        )
        .await;
        let submitted = app
            .clone()
            .oneshot(auth_bootstrap_existing_credential_request(
                session_id,
                &access_token,
                "replacement-cookie",
            ))
            .await
            .unwrap();
        assert_eq!(submitted.status(), StatusCode::OK);
        let submitted = response_json(submitted).await;
        assert_eq!(submitted["provider_account_id"], account_id);
        assert_eq!(submitted["session"]["purpose"], "reauthenticate");
        assert_eq!(submitted["session"]["state"], "completed");

        let account = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/provider-accounts/{account_id}"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response_json(account).await["auth_state"]["state"],
            "authenticated"
        );
    }

    #[tokio::test]
    async fn auth_bootstrap_provider_rejection_keeps_claim_retryable_without_writes() {
        let (app, database) = credential_test_app(false).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap().to_owned();
        let pairing_token = created["pairing_token"].as_str().unwrap();
        let access_token = claim_test_auth_bootstrap(
            &app,
            &session_id,
            pairing_token,
            SocketAddr::from(([127, 0, 0, 8], 42_008)),
        )
        .await;
        let rejected = app
            .clone()
            .oneshot(auth_bootstrap_credential_request(
                &session_id,
                &access_token,
                "rejected-cookie",
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(rejected).await["error"]["code"],
            "provider_credential_rejected"
        );
        let retryable = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/auth-bootstrap/sessions/{session_id}/stream"
                ))
                .header(header::AUTHORIZATION, format!("Bootstrap {access_token}"))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(retryable.status(), StatusCode::OK);
        assert_eq!(response_json(retryable).await["state"], "claimed");
        let account_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_accounts")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret_blobs")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(account_count, 0);
        assert_eq!(secret_count, 0);
    }

    #[tokio::test]
    async fn auth_bootstrap_claim_is_rate_limited_per_session_and_ip() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = create_test_auth_bootstrap(&app, &cookie).await;
        let session_id = created["session"]["id"].as_str().unwrap();
        let claim_path = format!("/api/v1/auth-bootstrap/sessions/{session_id}/claim");
        for attempt in 0..6 {
            let response = app
                .clone()
                .oneshot(
                    Request::post(&claim_path)
                        .header(header::AUTHORIZATION, "Bootstrap ast_pair_wrong")
                        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 2], 42_002))))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if attempt < 5 {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                }
            );
        }
    }

    #[tokio::test]
    async fn auth_session_credential_api_commits_one_sanitized_state_transition() {
        let (app, database) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let started = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/provider-accounts/{account_id}/auth-sessions"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(r#"{"method":"imported_cookie"}"#))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(started.status(), StatusCode::CREATED);
        let started = response_json(started).await;
        let session_id = started["session"]["id"].as_str().unwrap();
        let path = format!(
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/credentials"
        );
        let response = app
            .clone()
            .oneshot(
                Request::put(&path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"auth_method":"imported_cookie","acquired_via":"manual_import","session_kind":"cookie","fields":[{"purpose":"provider_cookie","value":"session-credential-value"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        assert!(
            !body
                .as_ref()
                .windows(b"session-credential-value".len())
                .any(|window| window == b"session-credential-value")
        );
        let response: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["session"]["state"]["state"], "authenticated");
        assert_eq!(response["session"]["revision"], 4);
        assert_eq!(response["credential_count"], 1);
        assert_eq!(response["status"]["valid"], true);

        let repeated = app
            .oneshot(
                Request::put(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie)
                    .body(Body::from(
                        r#"{"auth_method":"imported_cookie","acquired_via":"manual_import","session_kind":"cookie","fields":[{"purpose":"provider_cookie","value":"replacement-value"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(repeated.status(), StatusCode::CONFLICT);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM secret_blobs")
                .fetch_one(database.pool())
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn provider_auth_session_api_exposes_one_owner_scoped_state_flow() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let created = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/provider-accounts/{account_id}/auth-sessions"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(r#"{"method":"imported_cookie"}"#))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let created = response_json(created).await;
        let session_id = created["session"]["id"].as_str().unwrap();
        assert_eq!(created["session"]["state"]["state"], "waiting_user");
        assert_eq!(created["challenge"]["waiting_for"], "session_import");

        let latest = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/provider-accounts/{account_id}/auth-sessions/latest"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(latest.status(), StatusCode::OK);
        assert_eq!(response_json(latest).await["id"], session_id);

        let session_path =
            format!("/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}");
        let cancelled = app
            .clone()
            .oneshot(
                Request::delete(&session_path)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(
            response_json(cancelled).await["state"]["state"],
            "cancelled"
        );
        let fetched = app
            .oneshot(
                Request::get(&session_path)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);
        assert_eq!(response_json(fetched).await["revision"], 3);
    }

    #[tokio::test]
    async fn provider_account_scan_requires_remote_auth_and_commits_inventory() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let app = build_router(ApiState::new(
            database.clone(),
            Arc::new(scan_registry()),
            3600,
            false,
        ));
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
        let created = to_bytes(created.into_body(), 16 * 1024).await.unwrap();
        let created: Value = serde_json::from_slice(&created).unwrap();
        let account_id = created["id"].as_str().unwrap();
        let scan_path = format!("/api/v1/provider-accounts/{account_id}/scan");

        let idle = app
            .clone()
            .oneshot(
                Request::post(&scan_path)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(idle.status(), StatusCode::CONFLICT);

        sqlx::query("UPDATE provider_accounts SET auth_state_json = ? WHERE id = ?")
            .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
            .bind(account_id)
            .execute(database.pool())
            .await
            .unwrap();
        let scanned = app
            .oneshot(
                Request::post(scan_path)
                    .header(header::COOKIE, cookie)
                    .header("x-request-id", "scan-api-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(scanned.status(), StatusCode::OK);
        assert_eq!(scanned.headers()[header::CACHE_CONTROL], "no-store");
        let report: Value =
            serde_json::from_slice(&to_bytes(scanned.into_body(), 16 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(report["courses_seen"], 1);
        assert_eq!(report["tasks_created"], 1);

        for table in ["tasks", "task_snapshots", "task_diffs", "event_outbox"] {
            let query = format!("SELECT COUNT(*) FROM {table}");
            let count: i64 = sqlx::query_scalar(&query)
                .fetch_one(database.pool())
                .await
                .unwrap();
            assert_eq!(count, 1, "unexpected row count for {table}");
        }
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action = 'provider_account_scanned' AND correlation_id = 'scan-api-test'",
        )
        .bind(account_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 1);
    }

    #[tokio::test]
    async fn scan_schedule_api_applies_provider_floor_and_audits_updates() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let app = build_router(ApiState::new(
            database.clone(),
            Arc::new(scan_registry()),
            3600,
            false,
        ));
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let schedule_path = format!("/api/v1/provider-accounts/{account_id}/scan-schedule");

        let idle = app
            .clone()
            .oneshot(
                Request::put(&schedule_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"desired_interval_seconds":60,"enabled":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(idle.status(), StatusCode::CONFLICT);
        sqlx::query("UPDATE provider_accounts SET auth_state_json = ? WHERE id = ?")
            .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
            .bind(&account_id)
            .execute(database.pool())
            .await
            .unwrap();

        let configured = app
            .clone()
            .oneshot(
                Request::put(&schedule_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "schedule-api-create")
                    .body(Body::from(
                        r#"{"desired_interval_seconds":60,"enabled":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(configured.status(), StatusCode::OK);
        assert_eq!(configured.headers()[header::CACHE_CONTROL], "no-store");
        let configured = response_json(configured).await;
        assert_eq!(configured["desired_interval_seconds"], 60);
        assert_eq!(configured["effective_interval_seconds"], 300);
        assert_eq!(configured["provider_min_interval_seconds"], 300);
        let schedule_id = configured["id"].as_str().unwrap().to_owned();

        let disabled = app
            .clone()
            .oneshot(
                Request::put(&schedule_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "schedule-api-disable")
                    .body(Body::from(
                        r#"{"desired_interval_seconds":600,"enabled":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(disabled.status(), StatusCode::OK);
        let disabled = response_json(disabled).await;
        assert_eq!(disabled["id"], schedule_id);
        assert_eq!(disabled["effective_interval_seconds"], 600);
        assert_eq!(disabled["enabled"], false);

        let fetched = app
            .oneshot(
                Request::get(schedule_path)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);
        let fetched = response_json(fetched).await;
        assert_eq!(fetched, disabled);

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action = 'scan_schedule_configured'",
        )
        .bind(schedule_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 2);
    }

    #[tokio::test]
    async fn task_api_is_owner_scoped_paginated_and_keeps_state_dimensions_separate() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account = app
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
        let account = to_bytes(account.into_body(), 16 * 1024).await.unwrap();
        let account: Value = serde_json::from_slice(&account).unwrap();
        let account_id = account["id"].as_str().unwrap();
        let task_id = asterism_domain::TaskId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'remote-task', 'fingerprint', 'exam', 'routine', 'weekly check', \
                     'pending', 'ready', ?, ?, '[\"progress_read\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();

        let listed = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks?provider_account_id={account_id}&limit=1&offset=0"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        assert_eq!(listed.headers()[header::CACHE_CONTROL], "no-store");
        let listed = to_bytes(listed.into_body(), 16 * 1024).await.unwrap();
        let listed: Value = serde_json::from_slice(&listed).unwrap();
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["limit"], 1);
        assert_eq!(listed["items"][0]["source_type"], "exam");
        assert_eq!(listed["items"][0]["assessment_class"], "routine");
        assert!(listed["items"][0].get("remote_fingerprint").is_none());

        let fetched = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/tasks/{task_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);

        let invalid_page = app
            .oneshot(
                Request::get("/api/v1/tasks?limit=0")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_page.status(), StatusCode::BAD_REQUEST);
        let invalid_page = to_bytes(invalid_page.into_body(), 16 * 1024).await.unwrap();
        let error: ErrorResponse = serde_json::from_slice(&invalid_page).unwrap();
        assert_eq!(error.error.code, "invalid_task_pagination");
    }

    #[tokio::test]
    async fn task_execution_action_is_atomic_idempotent_and_policy_guarded() {
        let (app, database, cookie, routine_task, other_task, formal_task, read_only_task) =
            execution_action_fixture().await;

        let created =
            post_task_execution(&app, &cookie, routine_task, Some("execute-document-1")).await;
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let created = response_json(created).await;
        assert_eq!(created["created"], true);
        assert_eq!(created["execution"]["request_source"], "web_ui");
        let execution_id = created["execution"]["id"].as_str().unwrap().to_owned();

        let execution_page = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/executions?task_id={routine_task}&limit=10&offset=0"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(execution_page.status(), StatusCode::OK);
        assert_eq!(execution_page.headers()[header::CACHE_CONTROL], "no-store");
        let execution_page = response_json(execution_page).await;
        assert_eq!(execution_page["total"], 1);
        assert_eq!(execution_page["limit"], 10);
        assert_eq!(execution_page["items"][0]["id"], execution_id);

        let detail = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/executions/{execution_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        assert_eq!(detail.headers()[header::CACHE_CONTROL], "no-store");
        let detail = response_json(detail).await;
        assert_eq!(detail["execution"]["id"], execution_id);
        assert!(detail["progress"].is_null());
        assert_eq!(detail["attempts"], json!([]));

        let logs = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/executions/{execution_id}/logs?limit=10&offset=0"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logs.status(), StatusCode::OK);
        let logs = response_json(logs).await;
        assert_eq!(logs["total"], 0);
        assert_eq!(logs["limit"], 10);
        assert_eq!(logs["items"], json!([]));

        let replayed =
            post_task_execution(&app, &cookie, routine_task, Some("execute-document-1")).await;
        assert_eq!(replayed.status(), StatusCode::OK);
        let replayed = response_json(replayed).await;
        assert_eq!(replayed["created"], false);
        assert_eq!(replayed["execution"]["id"], execution_id);

        let conflict =
            post_task_execution(&app, &cookie, other_task, Some("execute-document-1")).await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let conflict: ErrorResponse =
            serde_json::from_value(response_json(conflict).await).unwrap();
        assert_eq!(conflict.error.code, "idempotency_conflict");

        let blocked =
            post_task_execution(&app, &cookie, formal_task, Some("formal-document-1")).await;
        assert_eq!(blocked.status(), StatusCode::CONFLICT);
        let blocked: ErrorResponse = serde_json::from_value(response_json(blocked).await).unwrap();
        assert_eq!(blocked.error.code, "formal_assessment_blocked");

        let unsupported =
            post_task_execution(&app, &cookie, read_only_task, Some("read-only-1")).await;
        assert_eq!(unsupported.status(), StatusCode::CONFLICT);
        let unsupported: ErrorResponse =
            serde_json::from_value(response_json(unsupported).await).unwrap();
        assert_eq!(unsupported.error.code, "task_execution_unsupported");

        let missing_key = post_task_execution(&app, &cookie, routine_task, None).await;
        assert_eq!(missing_key.status(), StatusCode::BAD_REQUEST);

        for (table, predicate) in [
            ("executions", "1 = 1"),
            ("scheduled_jobs", "job_kind = 'execution'"),
            ("audit_records", "action = 'execution_requested'"),
            ("event_outbox", "event_type = 'execution_state_changed'"),
        ] {
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {predicate}"))
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
            assert_eq!(count, 1, "unexpected row count in {table}");
        }
    }

    async fn execution_action_fixture() -> (
        Router,
        Database,
        String,
        asterism_domain::TaskId,
        asterism_domain::TaskId,
        asterism_domain::TaskId,
        asterism_domain::TaskId,
    ) {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account = app
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
        let account = response_json(account).await;
        let account_id = account["id"].as_str().unwrap();
        let routine_task = asterism_domain::TaskId::new();
        let other_task = asterism_domain::TaskId::new();
        let formal_task = asterism_domain::TaskId::new();
        let read_only_task = asterism_domain::TaskId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        for (task_id, assessment, capabilities) in [
            (routine_task, "routine", "[\"resource_execution\"]"),
            (other_task, "routine", "[\"resource_execution\"]"),
            (formal_task, "formal", "[\"resource_execution\"]"),
            (read_only_task, "routine", "[\"progress_read\"]"),
        ] {
            sqlx::query(
                "INSERT INTO tasks \
                 (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
                  assessment_class, title, remote_state, orchestration_state, discovered_at, \
                  updated_at, capabilities_json) \
                 VALUES (?, ?, ?, ?, 'resource', ?, 'document', 'pending', 'ready', ?, ?, ?)",
            )
            .bind(task_id.to_string())
            .bind(account_id)
            .bind(format!("remote-{task_id}"))
            .bind(format!("fingerprint-{task_id}"))
            .bind(assessment)
            .bind(&now)
            .bind(&now)
            .bind(capabilities)
            .execute(database.pool())
            .await
            .unwrap();
        }
        (
            app,
            database,
            cookie,
            routine_task,
            other_task,
            formal_task,
            read_only_task,
        )
    }

    async fn post_task_execution(
        app: &Router,
        cookie: &str,
        task_id: asterism_domain::TaskId,
        idempotency_key: Option<&str>,
    ) -> Response {
        let mut request = Request::post(format!("/api/v1/tasks/{task_id}/execute"))
            .header(header::COOKIE, cookie)
            .header("x-request-id", format!("request-{task_id}"));
        if let Some(idempotency_key) = idempotency_key {
            request = request.header("idempotency-key", idempotency_key);
        }
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
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
            "/api/v1/auth-bootstrap/sessions",
            "/api/v1/auth-bootstrap/sessions/{session_id}",
            "/api/v1/auth-bootstrap/sessions/{session_id}/claim",
            "/api/v1/auth-bootstrap/sessions/{session_id}/credential",
            "/api/v1/auth-bootstrap/sessions/{session_id}/events",
            "/api/v1/auth-bootstrap/sessions/{session_id}/stream",
            "/api/v1/service-tokens",
            "/api/v1/service-tokens/{token_id}",
            "/api/v1/provider-accounts",
            "/api/v1/provider-accounts/{account_id}",
            "/api/v1/provider-accounts/{account_id}/scan",
            "/api/v1/provider-accounts/{account_id}/credentials",
            "/api/v1/provider-accounts/{account_id}/auth-sessions",
            "/api/v1/provider-accounts/{account_id}/auth-sessions/latest",
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}",
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/credentials",
            "/api/v1/provider-accounts/{account_id}/scan-schedule",
            "/api/v1/tasks",
            "/api/v1/tasks/{task_id}",
            "/api/v1/tasks/{task_id}/execute",
            "/api/v1/executions",
            "/api/v1/executions/{execution_id}",
            "/api/v1/executions/{execution_id}/logs",
        ] {
            assert!(document["paths"].get(path).is_some(), "missing {path}");
        }
        assert_eq!(
            document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/execute"]["post"]["operationId"],
            "executeTask"
        );
        assert_eq!(
            document["paths"]["/api/v1/executions"]["get"]["operationId"],
            "listExecutions"
        );
        assert_eq!(
            document["paths"]["/api/v1/executions/{execution_id}"]["get"]["operationId"],
            "getExecution"
        );
        assert_eq!(
            document["paths"]["/api/v1/executions/{execution_id}/logs"]["get"]["operationId"],
            "listExecutionLogs"
        );
        assert_eq!(
            document["paths"]["/api/v1/auth-bootstrap/sessions/{session_id}/claim"]["post"]["security"]
                [0]["bootstrapAuth"],
            json!([])
        );
        assert_eq!(
            document["paths"]["/api/v1/auth-bootstrap/sessions/{session_id}/events"]["post"]["security"]
                [0]["bootstrapAuth"],
            json!([])
        );
        assert_eq!(
            document["paths"]["/api/v1/auth-bootstrap/sessions/{session_id}/credential"]["post"]["security"]
                [0]["bootstrapAuth"],
            json!([])
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
