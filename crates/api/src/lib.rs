#![recursion_limit = "256"]
#![allow(clippy::similar_names)]
#![allow(clippy::unnested_or_patterns)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::if_not_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::format_collect)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::needless_lifetimes)]

//! Versioned HTTP transport for Asterism core services.

mod account;
mod admin;
mod ai;
mod auth;
mod auth_bootstrap;
mod batch_execution;
mod browser_bridge;
mod cidaren_answer_bridge;
mod course;
mod course_enrollment;
mod credit;
mod execution;
mod openapi_contract;
mod qq;
mod rate_limit;
mod runtime_settings;
mod task;
mod uai_worker;

pub use ai::ChallengeEscalationTick;

use std::{collections::BTreeMap, sync::Arc};

use asterism_config::AiConfig;
use asterism_domain::ProviderId;
use asterism_events::EventBus;
use asterism_provider_api::{CaptureRecipe, ProviderMetadata, ProviderRegistry};
use asterism_storage::{Database, SqliteSecretStore};
use asterism_uai_worker_client::UaiWorkerClient;
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::put,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::{RwLock, watch};
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
    events: EventBus,
    stream_shutdown: Option<watch::Receiver<bool>>,
    login_rate_limiter: rate_limit::LoginRateLimiter,
    bootstrap_claim_rate_limiter: rate_limit::LoginRateLimiter,
    bootstrap_credential_rate_limiter: rate_limit::LoginRateLimiter,
    browser_bridge_claim_rate_limiter: rate_limit::LoginRateLimiter,
    provider_workers: BTreeMap<String, UaiWorkerClient>,
    ai: Arc<RwLock<AiConfig>>,
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
            events: EventBus::new(512),
            stream_shutdown: None,
            login_rate_limiter: rate_limit::LoginRateLimiter::default(),
            bootstrap_claim_rate_limiter: rate_limit::LoginRateLimiter::default(),
            bootstrap_credential_rate_limiter: rate_limit::LoginRateLimiter::default(),
            browser_bridge_claim_rate_limiter: rate_limit::LoginRateLimiter::default(),
            provider_workers: BTreeMap::new(),
            ai: Arc::new(RwLock::new(AiConfig::default())),
        }
    }

    #[must_use]
    pub fn with_secret_store(mut self, secret_store: SqliteSecretStore) -> Self {
        self.secret_store = Some(secret_store);
        self
    }

    #[must_use]
    pub fn with_event_bus(mut self, events: EventBus) -> Self {
        self.events = events;
        self
    }

    #[must_use]
    pub fn with_stream_shutdown(mut self, shutdown: watch::Receiver<bool>) -> Self {
        self.stream_shutdown = Some(shutdown);
        self
    }

    #[must_use]
    pub fn with_uai_worker(mut self, worker: UaiWorkerClient) -> Self {
        self.provider_workers.insert("uai".to_owned(), worker);
        self
    }

    #[must_use]
    pub fn with_ai_config(mut self, ai: AiConfig) -> Self {
        self.ai = Arc::new(RwLock::new(ai));
        self
    }

    pub(crate) async fn ai_config(&self) -> AiConfig {
        self.ai.read().await.clone()
    }

    pub(crate) async fn replace_ai_config(&self, ai: AiConfig) {
        *self.ai.write().await = ai;
    }

    /// Loads the last administrator-saved AI configuration after migrations.
    pub async fn hydrate_ai_config(&self) -> Result<(), asterism_storage::StorageError> {
        let row = sqlx::query("SELECT config_json FROM deployment_ai_config WHERE singleton = 1")
            .fetch_optional(self.database.pool())
            .await?;
        if let Some(row) = row {
            let encoded: String = row.try_get("config_json")?;
            let config = serde_json::from_str::<AiConfig>(&encoded).map_err(|error| {
                asterism_storage::StorageError::InvalidData(format!(
                    "persisted AI configuration is invalid: {error}"
                ))
            })?;
            config.validate().map_err(|error| {
                asterism_storage::StorageError::InvalidData(format!(
                    "persisted AI configuration is unsafe: {error}"
                ))
            })?;
            self.replace_ai_config(config).await;
        }
        Ok(())
    }

    /// Advances at most one durable Chaoxing challenge escalation.
    ///
    /// The processor is deliberately bounded: it materializes only executions
    /// carrying the exact Worker escalation marker, leases one row, generates
    /// a fresh GPT-only candidate set and schedules one new encrypted Worker
    /// invocation. Returned errors contain only a stable code.
    pub async fn process_chaoxing_challenge_escalation_tick(
        &self,
    ) -> Result<ChallengeEscalationTick, String> {
        ai::process_chaoxing_challenge_escalation_tick(self)
            .await
            .map_err(|error| error.code.to_owned())
    }

    /// Resolves and freezes one routine Chaoxing chapter answer set before
    /// scheduling its upstream-backed execution. A `None` result means the
    /// Provider reported no question payload and the caller may schedule the
    /// remaining resource work without an answer invocation.
    pub async fn schedule_automatic_chaoxing_task(
        &self,
        owner_id: asterism_domain::UserId,
        task_id: asterism_domain::TaskId,
        idempotency_key: &str,
        correlation_id: &str,
    ) -> Result<Option<asterism_domain::ExecutionId>, String> {
        ai::schedule_automatic_chaoxing_task(
            self,
            owner_id,
            task_id,
            idempotency_key,
            correlation_id,
        )
        .await
        .map_err(|error| error.code.to_owned())
    }

    /// Generates a bounded human-style response for one required UAI
    /// discussion and schedules the donor-backed publication invocation.
    pub async fn schedule_automatic_uai_discussion(
        &self,
        owner_id: asterism_domain::UserId,
        task_id: asterism_domain::TaskId,
        idempotency_key: &str,
        correlation_id: &str,
        ai_profile_override: Option<&str>,
    ) -> Result<asterism_domain::ExecutionId, String> {
        ai::schedule_automatic_uai_discussion(
            self,
            owner_id,
            task_id,
            idempotency_key,
            correlation_id,
            ai_profile_override,
        )
        .await
        .map_err(|error| error.code.to_owned())
    }

    /// Adds one configured 0.0.1 upstream-backed Provider worker.
    #[must_use]
    pub fn with_provider_worker(
        mut self,
        provider: impl Into<String>,
        worker: UaiWorkerClient,
    ) -> Self {
        self.provider_workers.insert(provider.into(), worker);
        self
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the versioned route table remains together for transport-surface review"
)]
pub fn build_router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/api/v1/auth/session", get(auth::current_identity))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/auth/password", put(auth::change_or_set_password))
        .route(
            "/api/v1/integrations/qq/identity/assert",
            post(qq::assert_qq_identity),
        )
        .route(
            "/api/v1/integrations/qq/notifications/claim",
            post(qq::claim_qq_formal_notifications),
        )
        .route(
            "/api/v1/integrations/qq/notifications/report",
            post(qq::report_qq_formal_notifications),
        )
        .route("/api/v1/providers", get(list_providers))
        .route("/api/v1/providers/{provider}/worker/health", get(uai_worker::health))
        .route(
            "/api/v1/providers/{provider_id}/capture-recipes",
            get(list_provider_capture_recipes),
        )
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
            "/api/v1/browser-bridge/sessions/{session_id}",
            get(browser_bridge::get_browser_bridge_session)
                .delete(browser_bridge::cancel_browser_bridge_session),
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
            "/api/v1/provider-accounts/{account_id}/health",
            get(account::get_provider_account_health),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/scan",
            post(account::scan_provider_account),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/course-enrollment-drafts",
            post(course_enrollment::prepare_course_enrollment),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/course-enrollment-drafts/{draft_id}/execute",
            post(course_enrollment::execute_course_enrollment),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/course-enrollment-drafts/{draft_id}/attempts/{attempt_id}/recover",
            post(course_enrollment::recover_course_enrollment),
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
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/poll",
            post(account::poll_interactive_auth_session),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/external-oauth",
            get(account::get_external_oauth_pending),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/external-oauth/callback",
            post(account::submit_external_oauth_callback),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/scan-schedule",
            get(account::get_scan_schedule).put(account::configure_scan_schedule),
        )
        .route(
            "/api/v1/provider-accounts/{account_id}/chaoxing-verification",
            get(account::get_chaoxing_verification_status),
        )
        .merge(task_routes())
        .route("/api/v1/courses", get(course::list_courses))
        .route("/api/v1/courses/{course_id}", get(course::get_course))
        .route(
            "/api/v1/courses/{course_id}/automation",
            get(course::get_course_automation).put(course::configure_course_automation),
        )
        .route(
            "/api/v1/courses/{course_id}/progress",
            get(course::get_course_progress),
        )
        .route(
            "/api/v1/providers/welearn/accounts/{account_id}/courses/{course_id}/batch-executions",
            post(batch_execution::create_welearn_batch_execution),
        )
        .merge(runtime_settings_routes())
        .merge(credit_routes())
        .merge(execution_routes())
        .route(
            "/api/v1/service-tokens",
            get(auth::list_service_tokens).post(auth::create_service_token),
        )
        .route("/api/v1/audit", get(admin::list_audit))
        .route(
            "/api/v1/admin/protocol-observations",
            get(admin::list_protocol_observations),
        )
        .route(
            "/api/v1/admin/ai-config",
            get(admin::get_ai_config).put(admin::put_ai_config),
        )
        .route(
            "/api/v1/admin/ai-usage",
            get(admin::list_ai_usage),
        )
        .route(
            "/api/v1/admin/answer-bank-usage",
            get(admin::list_answer_bank_usage),
        )
        .route(
            "/api/v1/admin/pricing-catalog",
            get(admin::get_pricing_catalog).put(admin::put_pricing_catalog),
        )
        .route(
            "/api/v1/admin/users",
            get(admin::list_users).post(admin::create_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}",
            get(admin::get_user).put(admin::update_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}/password",
            put(admin::reset_user_password),
        )
        .route(
            "/api/v1/admin/users/{user_id}/credit-grants",
            post(credit::grant_user_credits),
        )
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
            "/api/v1/auth/qq-login/{ticket}",
            get(qq::consume_qq_web_login),
        )
        .route(
            "/api/v1/internal/cidaren/answer-bridge",
            post(cidaren_answer_bridge::handle),
        )
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
        .route(
            "/api/v1/browser-bridge/sessions/{session_id}/claim",
            post(browser_bridge::claim_browser_bridge_session),
        )
        .route(
            "/api/v1/browser-bridge/sessions/{session_id}/snapshot",
            get(browser_bridge::get_browser_bridge_snapshot),
        )
        .route(
            "/api/v1/browser-bridge/sessions/{session_id}/binding",
            axum::routing::put(browser_bridge::bind_browser_bridge_runtime),
        )
        .route(
            "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}",
            get(browser_bridge::dispatch_browser_bridge_command),
        )
        .route(
            "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}/result",
            post(browser_bridge::receive_browser_bridge_result).layer(DefaultBodyLimit::max(
                browser_bridge::MAX_BROWSER_BRIDGE_ARTIFACT_BYTES,
            )),
        )
        .merge(protected)
        .with_state(state)
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(X_REQUEST_ID.clone()))
        .layer(SetRequestIdLayer::new(X_REQUEST_ID, MakeRequestUuid))
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
}

fn credit_routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/credits/account", get(credit::get_credit_account))
        .route(
            "/api/v1/credits/recharge-contact",
            get(credit::get_recharge_contact),
        )
        .route(
            "/api/v1/credits/transactions",
            get(credit::list_credit_transactions),
        )
        .route(
            "/api/v1/credits/reservations",
            get(credit::list_credit_reservations),
        )
}

fn runtime_settings_routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/admin/provider-accounts/{account_id}/answer-history-scan",
            get(account::get_answer_history_scan_status).post(account::control_answer_history_scan),
        )
        .route(
            "/api/v1/admin/providers/{provider_id}/runtime-settings/schema",
            get(runtime_settings::get_provider_runtime_settings_schema),
        )
        .route(
            "/api/v1/admin/providers/{provider_id}/runtime-settings",
            get(runtime_settings::get_provider_runtime_settings)
                .put(runtime_settings::put_provider_runtime_settings),
        )
        .route(
            "/api/v1/admin/provider-accounts/{account_id}/runtime-settings",
            get(runtime_settings::get_account_runtime_settings)
                .put(runtime_settings::put_account_runtime_settings),
        )
        .route(
            "/api/v1/admin/tasks/{task_id}/runtime-settings",
            get(runtime_settings::get_task_runtime_settings)
                .put(runtime_settings::put_task_runtime_settings),
        )
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
        .route(
            "/api/v1/executions/{execution_id}/stream",
            get(execution::stream_execution),
        )
}

fn task_routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/tasks", get(task::list_tasks))
        .route("/api/v1/tasks/{task_id}", get(task::get_task))
        .route(
            "/api/v1/tasks/{task_id}/completion-workflows",
            get(task::get_task_completion_workflows),
        )
        .route(
            "/api/v1/tasks/{task_id}/completion-workflows/score-improvement",
            post(task::opt_in_score_improvement),
        )
        .route(
            "/api/v1/tasks/{task_id}/attempt-history",
            get(task::list_task_attempt_history),
        )
        .route("/api/v1/tasks/{task_id}/detail", get(task::get_task_detail))
        .route(
            "/api/v1/tasks/{task_id}/browser-session-spec",
            get(task::get_task_browser_session_spec),
        )
        .route(
            "/api/v1/tasks/{task_id}/browser-bridge/sessions",
            post(browser_bridge::create_browser_bridge_session),
        )
        .route(
            "/api/v1/tasks/{task_id}/progress",
            get(task::get_task_progress),
        )
        .route(
            "/api/v1/tasks/{task_id}/duration",
            get(task::get_task_duration),
        )
        .route(
            "/api/v1/tasks/{task_id}/questions",
            get(task::get_task_questions),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}",
            get(task::get_task_question_snapshot),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/provider-answer-candidates",
            post(task::resolve_provider_answer_candidates),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/ai-answer-candidates",
            post(ai::generate_ai_answer_candidates),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates",
            get(task::list_answer_candidates).post(task::create_manual_answer_candidate),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates/import-local-cache",
            post(task::import_local_answer_candidates),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-resolution",
            get(task::resolve_answer_candidates),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts",
            post(task::build_submission_draft),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}",
            get(task::get_submission_draft),
        )
        .route(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}/results/{result_id}",
            get(task::get_submission_result),
        )
        .route(
            "/api/v1/tasks/{task_id}/execution-invocation-drafts",
            post(task::prepare_execution_invocation_draft).layer(DefaultBodyLimit::max(
                task::MAX_EXECUTION_INVOCATION_INPUT_BYTES,
            )),
        )
        .route(
            "/api/v1/tasks/{task_id}/ai-discussion-invocation-drafts",
            post(ai::generate_uai_discussion_draft),
        )
        .route("/api/v1/tasks/{task_id}/execute", post(task::execute_task))
        .route("/api/v1/tasks/{task_id}/approve", post(task::approve_task))
        .route("/api/v1/tasks/{task_id}/cancel", post(task::cancel_task))
        .route("/api/v1/tasks/{task_id}/delay", post(task::delay_task))
        .route("/api/v1/tasks/{task_id}/ignore", post(task::ignore_task))
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
        master_initialized: sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM system_settings WHERE key = 'master_initialized')",
        )
        .fetch_one(state.database.pool())
        .await
        .map_err(ApiError::internal)?,
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

async fn list_provider_capture_recipes(
    State(state): State<ApiState>,
    Extension(auth): Extension<auth::AuthContext>,
    Path(provider_id): Path<String>,
) -> Result<Json<ListResponse<CaptureRecipe>>, ApiError> {
    auth::require_provider_read(&auth)?;
    let provider_id = ProviderId::new(provider_id)
        .map_err(|error| ApiError::bad_request("invalid_provider_id", error.to_string()))?;
    let entry = state
        .providers
        .get(&provider_id)
        .ok_or_else(|| ApiError::not_found("provider_not_found"))?;
    let items = entry
        .authentication
        .as_ref()
        .map_or_else(Vec::new, |authentication| authentication.capture_recipes());
    Ok(Json(ListResponse {
        total: items.len(),
        items,
    }))
}

async fn openapi() -> Json<Value> {
    Json(openapi_document())
}

/// Builds the deterministic `OpenAPI` document consumed by the HTTP route and
/// offline client-generation tooling.
///
/// # Panics
///
/// Panics when the statically assembled document no longer has the expected
/// `paths` or `components.schemas` object shape.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the declarative OpenAPI document is kept together for route/schema integrity"
)]
pub fn openapi_document() -> Value {
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
            "/api/v1/auth/password": {"put": {
                "operationId": "changeOrSetPassword",
                "description": "Changes an initialized password after verifying the current password, or sets the first password for a QQ-created account. Every existing Web session is revoked on success.",
                "security": [{"cookieAuth": []}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["new_password"],
                    "properties": {
                        "current_password": {"type": "string", "format": "password", "minLength": 1, "maxLength": 1024, "writeOnly": true},
                        "new_password": {"type": "string", "format": "password", "minLength": 8, "maxLength": 1024, "writeOnly": true}
                    },
                    "additionalProperties": false
                }}}},
                "responses": {
                    "204": {"description": "Password stored and all Web sessions revoked"},
                    "400": {"description": "Invalid password or current password is required"},
                    "401": {"description": "Authentication or current password verification failed"},
                    "403": {"description": "A Web session is required"}
                }
            }},
            "/api/v1/auth/qq-login/{ticket}": {"get": {
                "operationId": "consumeQqWebLogin",
                "description": "Consumes a short-lived single-use QQ Web login ticket, creates an HttpOnly Web session and redirects to its bound relative WebUI path.",
                "parameters": [{"name": "ticket", "in": "path", "required": true, "schema": {"type": "string"}}],
                "responses": {"303": {"description": "Web session created"}, "400": {"description": "Ticket invalid, expired or already consumed"}}
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
            "/api/v1/integrations/qq/identity/assert": {"post": {
                "operationId": "assertQqIdentity",
                "description": "Trusted QQ gateway assertion. Resolves or creates the QQ-bound user and returns a short-lived user session token.",
                "security": [{"bearerAuth": []}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": false, "required": ["qq"],
                    "properties": {
                        "qq": {"type": "string", "pattern": "^[0-9]{5,20}$"},
                        "create_if_missing": {"type": "boolean", "default": true},
                        "return_to": {"type": "string", "description": "Safe relative WebUI path bound to the one-time login ticket"},
                        "master_assertion": {"type": "boolean", "default": false, "description": "Trusted Yunzai e.isMaster attestation; requires both qq_identity_assert and task_command_proxy scopes"}
                    }
                }}}},
                "responses": {
                    "200": {"description": "QQ-bound user session", "content": {"application/json": {"schema": {
                        "type": "object", "additionalProperties": false,
                        "required": ["user_id", "username", "qq", "created", "access_token", "expires_at", "web_login_path", "web_login_expires_at"],
                        "properties": {
                            "user_id": {"type": "string", "format": "uuid"},
                            "username": {"type": "string"},
                            "qq": {"type": "string"},
                            "created": {"type": "boolean"},
                            "access_token": {"type": "string", "writeOnly": true},
                            "expires_at": {"type": "string", "format": "date-time"},
                            "web_login_path": {"type": "string", "writeOnly": true},
                            "web_login_expires_at": {"type": "string", "format": "date-time"}
                        }
                    }}}},
                    "400": {"description": "Invalid QQ identity"},
                    "401": {"description": "Authentication required"},
                    "403": {"description": "Service token lacks qq_identity_assert or the additional task_command_proxy scope required for a master assertion"},
                    "404": {"description": "QQ identity is absent and creation is disabled"}
                }
            }},
            "/api/v1/integrations/qq/notifications/claim": {"post": {
                "operationId": "claimQqFormalNotifications",
                "description": "Claims bounded pending formal-assessment confirmation notifications for a Yunzai gateway.",
                "security": [{"bearerAuth": []}],
                "responses": {"200": {"description": "Claimed notifications", "content": {"application/json": {"schema": {"type": "object", "additionalProperties": false, "required": ["items"], "properties": {"items": {"type": "array", "items": {"type": "object", "additionalProperties": false, "required": ["id", "kind", "qq", "task_id", "title", "closes_at", "message", "web_login_path"], "properties": {"id": {"type": "string"}, "kind": {"type": "string", "enum": ["confirmation_due", "deadline_missed", "execution_succeeded", "execution_failed"]}, "qq": {"type": "string"}, "task_id": {"type": "string"}, "title": {"type": "string"}, "closes_at": {"type": "string", "format": "date-time"}, "message": {"type": "string"}, "web_login_path": {"type": "string"}}}}}}}}}, "403": {"description": "Service token lacks notification delivery scope"}}
            }},
            "/api/v1/integrations/qq/notifications/report": {"post": {
                "operationId": "reportQqFormalNotifications",
                "description": "Reports QQ delivery success or failure; failed deliveries are deferred for retry.",
                "security": [{"bearerAuth": []}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": false, "required": ["items"],
                    "properties": {"items": {"type": "array", "minItems": 1, "maxItems": 100, "items": {"type": "object", "additionalProperties": false, "required": ["id", "delivered"], "properties": {"id": {"type": "string"}, "delivered": {"type": "boolean"}, "error": {"type": "string", "maxLength": 512}}}}}
                }}}},
                "responses": {"204": {"description": "Delivery report accepted"}, "409": {"description": "Notification claim conflict"}}
            }},
            "/api/v1/providers": {"get": {"operationId": "listProviders", "security": [{"cookieAuth": []}, {"bearerAuth": []}], "responses": {"200": {"description": "Registered provider metadata"}}}},
            "/api/v1/providers/uai/worker/health": {"get": {
                "operationId": "getUaiWorkerHealth",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "responses": {
                    "200": {"description": "Pinned UAI upstream worker is available"},
                    "503": {"description": "UAI upstream worker is not configured or unavailable"}
                }
            }},
            "/api/v1/providers/chaoxing/worker/health": {"get": {
                "operationId": "getChaoxingWorkerHealth", "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "responses": {"200": {"description": "Pinned Chaoxing upstream worker is available"}, "503": {"description": "Worker is not configured or unavailable"}}
            }},
            "/api/v1/providers/welearn/worker/health": {"get": {
                "operationId": "getWelearnWorkerHealth", "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "responses": {"200": {"description": "Pinned WELearn upstream worker is available"}, "503": {"description": "Worker is not configured or unavailable"}}
            }},
            "/api/v1/providers/cidaren/worker/health": {"get": {
                "operationId": "getCidarenWorkerHealth", "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "responses": {"200": {"description": "Pinned Cidaren upstream worker is available"}, "503": {"description": "Worker is not configured or unavailable"}}
            }},
            "/api/v1/providers/{provider_id}/capture-recipes": {"get": {
                "operationId": "listProviderCaptureRecipes",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "provider_id", "in": "path", "required": true, "schema": {"type": "string", "pattern": "^[a-z0-9-]{1,64}$"}}],
                "responses": {"200": {"description": "Ordered Capture recipe alternatives"}, "400": {"description": "Invalid Provider ID"}, "404": {"description": "Provider not found"}}
            }},
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
                    "description": "Lists Provider accounts for the authenticated owner. A master/operator may select a target owner with X-Asterism-Target-Owner; actor and owner remain separate in authorization and audit records.",
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
            "/api/v1/provider-accounts/{account_id}/course-enrollment-drafts": {"post": {
                "operationId": "prepareCourseEnrollmentDraft",
                "description": "Resolves one invitation read-only and freezes the exact Provider join request in encrypted storage. No enrollment mutation is issued.",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": false,
                    "required": ["draft_id", "invitation"],
                    "properties": {
                        "draft_id": {"type": "string", "format": "uuid"},
                        "invitation": {"type": "string", "minLength": 1, "maxLength": 4096, "writeOnly": true}
                    }
                }}}},
                "responses": {
                    "201": {"description": "Immutable encrypted enrollment draft and sanitized preview created or resolved idempotently", "content": {"application/json": {"schema": {
                        "type": "object", "additionalProperties": false,
                        "required": ["draft_id", "provider_account_id", "remote_course_id", "remote_class_id", "preview_sanitized", "created_at"],
                        "properties": {
                            "draft_id": {"type": "string", "format": "uuid"},
                            "provider_account_id": {"type": "string", "format": "uuid"},
                            "remote_course_id": {"type": "string"},
                            "remote_class_id": {"type": "string"},
                            "preview_sanitized": {"type": "object", "additionalProperties": true},
                            "created_at": {"type": "string", "format": "date-time"}
                        }
                    }}}},
                    "400": {"description": "Invalid invitation or draft identity"},
                    "404": {"description": "Provider account not found"},
                    "409": {"description": "Account is unauthenticated or Provider capability is unavailable"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider returned an invalid invitation preview"},
                    "503": {"description": "Provider or encrypted storage is unavailable"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/course-enrollment-drafts/{draft_id}/execute": {"post": {
                "operationId": "executeCourseEnrollmentDraft",
                "description": "Issues the frozen non-idempotent join request at most once. An ambiguous response switches permanently to fresh Course inventory verification.",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "draft_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                ],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": false,
                    "required": ["attempt_id", "confirm_non_idempotent_enrollment"],
                    "properties": {
                        "attempt_id": {"type": "string", "format": "uuid"},
                        "confirm_non_idempotent_enrollment": {"type": "boolean", "enum": [true]}
                    }
                }}}},
                "responses": {
                    "200": {"description": "Durable enrollment Attempt state; verification_pending is safe to recover and never replay", "content": {"application/json": {"schema": {"type": "object", "additionalProperties": false, "required": ["attempt_id", "draft_id", "state", "updated_at"], "properties": {"attempt_id": {"type": "string", "format": "uuid"}, "draft_id": {"type": "string", "format": "uuid"}, "state": {"type": "string", "enum": ["prepared", "mutation_issued", "receipt_recorded", "verification_pending", "succeeded", "rejected", "cancelled", "failed_before_issue"]}, "updated_at": {"type": "string", "format": "date-time"}}}}}},
                    "400": {"description": "Explicit confirmation or UUID is invalid"},
                    "404": {"description": "Account or enrollment draft not found"},
                    "409": {"description": "Attempt binding changed, is non-replayable, or account is not ready"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider enrollment protocol drifted"},
                    "503": {"description": "Provider is temporarily unavailable before issue"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/course-enrollment-drafts/{draft_id}/attempts/{attempt_id}/recover": {"post": {
                "operationId": "recoverCourseEnrollmentAttempt",
                "description": "Performs only fresh Course inventory verification for a previously issued enrollment Attempt; the join mutation is never repeated.",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "draft_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "attempt_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                ],
                "responses": {
                    "200": {"description": "Current durable enrollment Attempt state after read-only verification", "content": {"application/json": {"schema": {"type": "object", "additionalProperties": false, "required": ["attempt_id", "draft_id", "state", "updated_at"], "properties": {"attempt_id": {"type": "string", "format": "uuid"}, "draft_id": {"type": "string", "format": "uuid"}, "state": {"type": "string", "enum": ["prepared", "mutation_issued", "receipt_recorded", "verification_pending", "succeeded", "rejected", "cancelled", "failed_before_issue"]}, "updated_at": {"type": "string", "format": "date-time"}}}}}},
                    "404": {"description": "Account, draft, or Attempt not found"},
                    "409": {"description": "Attempt is pre-issue or cross-bound"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider inventory verification drifted"},
                    "503": {"description": "Provider inventory is temporarily unavailable"}
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
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/poll": {
                "post": {
                    "operationId": "pollProviderAccountInteractiveAuthSession",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [
                        {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                        {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                    ],
                    "responses": {
                        "200": {"description": "Interactive authentication advanced, completed, or reached a definite terminal state"},
                        "404": {"description": "Provider account or authentication session not found"},
                        "409": {"description": "Session changed, expired, is busy, or exhausted its bounded poll budget"},
                        "429": {"description": "Provider rate limit reached"},
                        "502": {"description": "Provider returned inconsistent interactive authentication data"},
                        "503": {"description": "Provider, network, or encrypted credential store unavailable"}
                    }
                }
            },
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/external-oauth": {"get": {
                "operationId": "getProviderAccountExternalOauthPending",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                ],
                "responses": {
                    "200": {"description": "Sanitized owner-scoped one-shot OAuth state"},
                    "404": {"description": "Pending OAuth session not found"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/external-oauth/callback": {"post": {
                "operationId": "submitProviderAccountExternalOauthCallback",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                ],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/SubmitExternalOauthCallback"}}}},
                "responses": {
                    "200": {"description": "OAuth callback consumed once, credentials validated and committed"},
                    "400": {"description": "Invalid callback URL"},
                    "404": {"description": "Provider account or OAuth session not found"},
                    "409": {"description": "OAuth callback is expired, consumed, terminal, or changed concurrently"},
                    "429": {"description": "Provider rate limit reached"},
                    "502": {"description": "Provider rejected the callback or protocol drifted"},
                    "503": {"description": "Provider, network, or encrypted credential store unavailable"}
                }
            }},
            "/api/v1/provider-accounts/{account_id}/scan-schedule": {
                "get": {
                    "operationId": "getProviderAccountScanSchedule",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"200": {"description": "Owner-scoped scan schedule"}, "403": {"description": "Account ownership or Provider management permission required"}, "404": {"description": "Account or scan schedule not found"}}
                },
                "put": {
                    "operationId": "configureProviderAccountScanSchedule",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ConfigureScanSchedule"}}}},
                    "responses": {"200": {"description": "Owner-scoped scan schedule configured with the explicit or Provider-default interval and Provider floor"}, "400": {"description": "Invalid interval or Provider default unavailable"}, "403": {"description": "Account ownership or Provider management permission required"}, "404": {"description": "Provider account not found"}, "409": {"description": "Provider is not registered or stored settings no longer match its schema"}}
                }
            },
            "/api/v1/admin/provider-accounts/{account_id}/answer-history-scan": {
                "get": {
                    "operationId": "getAnswerHistoryScanStatus",
                    "description": "Returns the durable Chaoxing full-account answer-history scan checkpoint and locally cached coverage.",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {"200": {"description": "Current durable scan state"}, "403": {"description": "Provider settings permission required"}, "404": {"description": "Account or scan not found"}, "409": {"description": "Provider does not support full answer-history scanning"}}
                },
                "post": {
                    "operationId": "controlAnswerHistoryScan",
                    "description": "Pauses, resumes, or explicitly retries the durable Chaoxing scan without discarding its checkpoint.",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "additionalProperties": false, "required": ["action"], "properties": {"action": {"type": "string", "enum": ["pause", "resume", "retry"]}}}}}},
                    "responses": {"200": {"description": "Updated durable scan state"}, "403": {"description": "Provider settings permission required"}, "404": {"description": "Account or scan not found"}, "409": {"description": "Requested transition is not safe in the current state"}}
                }
            },
            "/api/v1/tasks": {"get": {
                "operationId": "listTasks",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "provider_account_id", "in": "query", "schema": {"type": "string", "format": "uuid"}},
                    {"name": "course_id", "in": "query", "schema": {"type": "string", "format": "uuid"}},
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
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/ai-answer-candidates": {"post": {
                "operationId": "generateAiAnswerCandidates",
                "description": "Generates locally cached AI answer candidates through the selected deployment profile. Remote storage is disabled and Provider-standard answers remain separate.",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                ],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "profile": {"type": "string", "enum": ["economy", "gpt_only"], "default": "economy"},
                        "route": {"type": "string", "enum": ["timed", "untimed", "escalation"], "default": "untimed"},
                        "question_ids": {"type": "array", "items": {"type": "string", "format": "uuid"}, "maxItems": 100, "default": []},
                        "execution_id": {"type": "string", "format": "uuid", "description": "Optional owner/task-bound Execution whose frozen AI profile and route override this request."},
                        "force_refresh": {"type": "boolean", "default": false, "description": "Generate a new candidate for the same immutable snapshot instead of reusing the newest AI cache entry; intended for bounded challenge retries and explicit escalation."}
                    }
                }}}},
                "responses": {"200": {"description": "AI candidates saved in the local deployment cache"}, "400": {"description": "Question selection or request is invalid"}, "404": {"description": "Snapshot not found"}, "502": {"description": "Model returned invalid output"}, "503": {"description": "Model endpoint, key, or service is unavailable"}}
            }},
            "/api/v1/tasks/{task_id}/ai-discussion-invocation-drafts": {"post": {
                "operationId": "generateUaiDiscussionInvocationDraft",
                "description": "Freshly reads a UAI discussion, generates bounded human plain text, and stores an immutable encrypted Worker invocation draft. It does not execute or submit the task.",
                "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                "parameters": [
                    {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                    {"name": "idempotency-key", "in": "header", "required": true, "schema": {"type": "string", "maxLength": 256}}
                ],
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "object", "additionalProperties": false,
                    "properties": {"profile": {"type": "string", "enum": ["economy", "gpt_only"], "default": "economy"}}
                }}}},
                "responses": {
                    "200": {"description": "Idempotent invocation draft replay", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/UaiDiscussionInvocationDraft"}}}},
                    "201": {"description": "Generated encrypted invocation draft", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/UaiDiscussionInvocationDraft"}}}},
                    "409": {"description": "Task is not a current UAI discussion"}, "502": {"description": "Model returned unsafe text"}, "503": {"description": "Model endpoint, key, or encrypted draft storage is unavailable"}
                }
            }},
            "/api/v1/service-tokens": {
                "get": {
                    "operationId": "listServiceTokens",
                    "description": "Master Web Sessions list all token metadata; delegated ServiceTokenManage identities remain owner-scoped. Plaintext and digests are never returned.",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "parameters": [
                        {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
                        {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
                    ],
                    "responses": {"200": {"description": "Paginated sanitized ServiceToken metadata"}, "400": {"description": "Invalid pagination"}, "401": {"description": "Authentication required"}, "403": {"description": "Service token management permission required"}}
                },
                "post": {
                    "operationId": "createServiceToken",
                    "security": [{"cookieAuth": []}, {"bearerAuth": []}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateServiceToken"}}}},
                    "responses": {"200": {"description": "Scoped token created; plaintext returned once"}, "400": {"description": "Invalid request"}, "401": {"description": "Authentication required"}, "403": {"description": "Insufficient permission"}}
                }
            },
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
                "bootstrapAuth": {"type": "apiKey", "in": "header", "name": "Authorization", "description": "Bootstrap followed by a pairing or session-scoped access token, as required by the route"},
                "browserBridgeAuth": {"type": "apiKey", "in": "header", "name": "Authorization", "description": "BrowserBridge followed by a one-time pairing or session-scoped helper access token, as required by the route"}
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
                "UaiDiscussionInvocationDraft": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["task_id", "invocation_draft_id", "generated_text", "created"],
                    "properties": {
                        "task_id": {"type": "string", "format": "uuid"},
                        "invocation_draft_id": {"type": "string", "format": "uuid"},
                        "generated_text": {"type": "string", "minLength": 1, "maxLength": 16384},
                        "created": {"type": "boolean"}
                    }
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
                        "tenant": {"type": ["string", "null"], "maxLength": 256},
                        "owner_user_id": {"type": ["string", "null"], "format": "uuid", "description": "Optional target owner; assigning another user requires ManageProviders"}
                    },
                    "additionalProperties": false
                },
                "CreateAuthBootstrapSession": {
                    "type": "object",
                    "required": ["provider_id", "purpose"],
                    "properties": {
                        "provider_id": {"type": "string", "pattern": "^[a-z0-9-]{1,64}$"},
                        "provider_account_id": {"type": ["string", "null"], "format": "uuid"},
                        "purpose": {"type": "string", "enum": ["add_account", "reauthenticate", "repair_session"]},
                        "recipe_version": {"type": ["integer", "null"], "minimum": 1}
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
                "SubmitExternalOauthCallback": {
                    "type": "object",
                    "required": ["callback_url"],
                    "properties": {
                        "callback_url": {"type": "string", "format": "uri", "minLength": 1, "maxLength": 8192, "writeOnly": true}
                    },
                    "additionalProperties": false
                },
                "ConfigureScanSchedule": {
                    "type": "object",
                    "required": ["enabled"],
                    "properties": {
                        "desired_interval_seconds": {"type": "integer", "minimum": 1, "description": "Omit to snapshot the current Provider/account runtime-settings default"},
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
    document["servers"] = json!([{
        "url": "/",
        "description": "Same-origin Asterism daemon"
    }]);
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/executions".to_owned(), execution_list_path());
    for (path, value) in [
        (
            "/api/v1/admin/providers/{provider_id}/runtime-settings/schema",
            runtime_settings_schema_path(),
        ),
        (
            "/api/v1/admin/providers/{provider_id}/runtime-settings",
            runtime_settings_path(
                "getProviderRuntimeSettings",
                "putProviderRuntimeSettings",
                "provider_id",
            ),
        ),
        (
            "/api/v1/admin/provider-accounts/{account_id}/runtime-settings",
            runtime_settings_path(
                "getProviderAccountRuntimeSettings",
                "putProviderAccountRuntimeSettings",
                "account_id",
            ),
        ),
        (
            "/api/v1/admin/tasks/{task_id}/runtime-settings",
            runtime_settings_path(
                "getTaskRuntimeSettings",
                "putTaskRuntimeSettings",
                "task_id",
            ),
        ),
    ] {
        document["paths"]
            .as_object_mut()
            .expect("static OpenAPI paths object")
            .insert(path.to_owned(), value);
    }
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/credits/account".to_owned(), credit_account_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/credits/recharge-contact".to_owned(),
            recharge_contact_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/credits/transactions".to_owned(),
            credit_page_path(
                "listOwnCreditTransactions",
                "Owner-scoped immutable credit ledger",
            ),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/credits/reservations".to_owned(),
            credit_page_path(
                "listOwnCreditReservations",
                "Owner-scoped reservations with immutable PriceQuote attribution",
            ),
        );
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
            "/api/v1/executions/{execution_id}/stream".to_owned(),
            execution_stream_path(),
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
            "/api/v1/tasks/{task_id}/detail".to_owned(),
            task_detail_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/browser-session-spec".to_owned(),
            task_browser_session_spec_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/browser-bridge/sessions".to_owned(),
            create_browser_bridge_session_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/browser-bridge/sessions/{session_id}".to_owned(),
            browser_bridge_session_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/browser-bridge/sessions/{session_id}/claim".to_owned(),
            claim_browser_bridge_session_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/browser-bridge/sessions/{session_id}/snapshot".to_owned(),
            browser_bridge_snapshot_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/browser-bridge/sessions/{session_id}/binding".to_owned(),
            browser_bridge_binding_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}".to_owned(),
            browser_bridge_command_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}/result".to_owned(),
            browser_bridge_result_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/progress".to_owned(),
            task_progress_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/duration".to_owned(),
            task_duration_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/questions".to_owned(),
            task_questions_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}".to_owned(),
            task_question_snapshot_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/provider-answer-candidates"
                .to_owned(),
            provider_answer_candidates_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates".to_owned(),
            answer_candidates_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/completion-workflows".to_owned(),
            task_completion_workflows_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/completion-workflows/score-improvement".to_owned(),
            score_improvement_opt_in_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/attempt-history".to_owned(),
            task_attempt_history_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/courses".to_owned(), courses_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/courses/{course_id}".to_owned(), course_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/courses/{course_id}/progress".to_owned(),
            course_progress_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/courses/{course_id}/automation".to_owned(),
            course_automation_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/providers/welearn/accounts/{account_id}/courses/{course_id}/batch-executions"
                .to_owned(),
            welearn_batch_executions_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/provider-accounts/{account_id}/health".to_owned(),
            provider_account_health_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/provider-accounts/{account_id}/chaoxing-verification".to_owned(),
            chaoxing_verification_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/admin/protocol-observations".to_owned(),
            protocol_observations_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/admin/ai-config".to_owned(), admin_ai_config_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/admin/ai-usage".to_owned(), admin_ai_usage_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/admin/answer-bank-usage".to_owned(),
            admin_answer_bank_usage_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/admin/pricing-catalog".to_owned(),
            admin_pricing_catalog_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates/import-local-cache".to_owned(),
            local_answer_cache_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-resolution".to_owned(),
            answer_resolution_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts".to_owned(),
            submission_drafts_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}"
                .to_owned(),
            submission_draft_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}/results/{result_id}".to_owned(),
            submission_result_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/tasks/{task_id}/execution-invocation-drafts".to_owned(),
            execution_invocation_drafts_path(),
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
        .insert("/api/v1/admin/users".to_owned(), admin_users_path());
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/admin/users/{user_id}".to_owned(),
            admin_user_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/admin/users/{user_id}/password".to_owned(),
            admin_user_password_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert(
            "/api/v1/admin/users/{user_id}/credit-grants".to_owned(),
            admin_credit_grant_path(),
        );
    document["paths"]
        .as_object_mut()
        .expect("static OpenAPI paths object")
        .insert("/api/v1/audit".to_owned(), audit_path());
    for (action, operation_id, requires_body) in [
        ("approve", "approveTask", false),
        ("cancel", "cancelTask", false),
        ("delay", "delayTask", true),
        ("ignore", "ignoreTask", false),
    ] {
        document["paths"]
            .as_object_mut()
            .expect("static OpenAPI paths object")
            .insert(
                format!("/api/v1/tasks/{{task_id}}/{action}"),
                task_lifecycle_path(operation_id, requires_body),
            );
    }
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
    document["components"]["schemas"]
        .as_object_mut()
        .expect("static OpenAPI schemas object")
        .insert("NormalizedAnswer".to_owned(), normalized_answer_schema());
    document["components"]["schemas"]
        .as_object_mut()
        .expect("static OpenAPI schemas object")
        .insert(
            "AiConfig".to_owned(),
            json!({
                "type": "object",
                "description": "Deployment-local AI routes and endpoint metadata; API keys are environment-only.",
                "required": ["remote_store", "default_profile", "gpt_router", "deepseek", "kimi", "economy", "gpt_only"],
                "properties": {"default_profile": {"type": "string", "enum": ["economy", "gpt_only"], "default": "economy"}},
                "additionalProperties": true
            }),
        );
    document["components"]["parameters"] = json!({
        "TargetOwnerHeader": {
            "name": "X-Asterism-Target-Owner",
            "in": "header",
            "required": false,
            "description": "Optional target resource owner for an authorized master/operator Web actor. Ordinary users may only address themselves; actor identity remains the authenticated session.",
            "schema": {"type": "string", "format": "uuid"}
        }
    });
    if let Some(paths) = document["paths"].as_object_mut() {
        for path in paths.values_mut() {
            if let Some(operations) = path.as_object_mut() {
                for operation in operations.values_mut() {
                    if operation.get("security").is_some() {
                        let parameters = operation
                            .as_object_mut()
                            .expect("OpenAPI operation is an object")
                            .entry("parameters")
                            .or_insert_with(|| json!([]));
                        parameters
                            .as_array_mut()
                            .expect("OpenAPI operation parameters are an array")
                            .push(json!({"$ref": "#/components/parameters/TargetOwnerHeader"}));
                    }
                }
            }
        }
    }
    openapi_contract::finalize(&mut document);
    document
}

fn credit_account_path() -> Value {
    json!({"get": {
        "operationId": "getOwnCreditAccount",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "responses": {
            "200": {"description": "Owner credit account; missing accounts read as zero balances without a write"},
            "401": {"description": "Authentication required"},
            "403": {"description": "ReadOwnCredits or CreditRead is required"}
        }
    }})
}

fn recharge_contact_path() -> Value {
    json!({"get": {
        "operationId": "getRechargeContact",
        "description": "Reads the optional deployment administrator recharge contact from the active pricing catalog.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "responses": {
            "200": {"description": "Optional sanitized recharge contact", "content": {"application/json": {"schema": {"type": "object", "required": ["contact"], "properties": {"contact": {"type": ["string", "null"], "maxLength": 512}}}}}},
            "401": {"description": "Authentication required"},
            "403": {"description": "ReadOwnCredits or CreditRead is required"}
        }
    }})
}

fn runtime_settings_schema_path() -> Value {
    json!({"get": {
        "operationId": "getProviderRuntimeSettingsSchema",
        "description": "Master-only registered Provider runtime settings schema; not exposed through ordinary Provider metadata.",
        "security": [{"cookieAuth": []}],
        "parameters": [{"name": "provider_id", "in": "path", "required": true, "schema": {"type": "string"}}],
        "responses": {
            "200": {"description": "Versioned bounded Provider runtime settings schema"},
            "401": {"description": "Authentication required"},
            "403": {"description": "ManageSystem is required"},
            "404": {"description": "Provider is not registered"}
        }
    }})
}

fn runtime_settings_path(get_operation: &str, put_operation: &str, path_parameter: &str) -> Value {
    json!({
        "get": {
            "operationId": get_operation,
            "description": "Master-managed settings layers, effective values and per-field sources.",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [{"name": path_parameter, "in": "path", "required": true, "schema": {"type": "string"}}],
            "responses": {
                "200": {"description": "Schema, stored overrides, resolved values and source map"},
                "401": {"description": "Authentication required"},
                "403": {"description": "Master or owner-bound ProviderManage authorization is required"},
                "404": {"description": "Settings target is not available to this identity"},
                "409": {"description": "A stored override uses an incompatible Provider schema"}
            }
        },
        "put": {
            "operationId": put_operation,
            "description": "Validates and atomically replaces one settings override using optimistic revision control.",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [{"name": path_parameter, "in": "path", "required": true, "schema": {"type": "string"}}],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["expected_revision", "schema_version", "values"],
                "properties": {
                    "expected_revision": {"type": "integer", "minimum": 0},
                    "schema_version": {"type": "integer", "minimum": 1},
                    "values": {"type": "object"}
                },
                "additionalProperties": false
            }}}},
            "responses": {
                "200": {"description": "Existing override replaced"},
                "201": {"description": "First override created"},
                "400": {"description": "Patch does not match the registered Provider schema"},
                "401": {"description": "Authentication required"},
                "403": {"description": "Master or owner-bound ProviderManage authorization is required"},
                "404": {"description": "Settings target is not available to this identity"},
                "409": {"description": "Expected revision is stale"}
            }
        }
    })
}

fn credit_page_path(operation_id: &str, description: &str) -> Value {
    json!({"get": {
        "operationId": operation_id,
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
            {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
        ],
        "responses": {
            "200": {"description": description},
            "400": {"description": "Invalid pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "ReadOwnCredits or CreditRead is required"}
        }
    }})
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

fn execution_stream_path() -> Value {
    json!({"get": {
        "operationId": "streamExecution",
        "description": "Snapshot-first live state/progress/log stream. Reconnect or resync through the bounded detail and log history APIs; durable event replay is not provided.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "execution_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Owner-scoped text/event-stream with snapshot and live Execution events"},
            "400": {"description": "Invalid Execution ID"},
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
        "requestBody": {
            "required": true,
            "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["requested_capabilities"],
                "additionalProperties": false,
                "properties": {
                    "requested_capabilities": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 5,
                        "uniqueItems": true,
                        "description": "Exact executable Task capability subset frozen onto this Execution.",
                        "items": {"type": "string", "enum": ["resource_execution", "submission_execute", "duration_report", "discussion", "artifact_upload", "oral_submission", "practice"]}
                    },
                    "submission_draft_id": {"type": "string", "format": "uuid", "description": "Required only for Tasks advertising submission_execute; binds this Execution to one immutable draft."},
                    "invocation_draft_id": {"type": "string", "format": "uuid", "description": "Claims one immutable encrypted Provider invocation draft for this Execution."},
                    "formal_assessment_confirmation": {
                        "type": "boolean",
                        "description": "Explicit owner confirmation for this Formal assessment Execution. Omit or set false to keep the default deny policy."
                    },
                    "formal_assessment_save_only": {
                        "type": "boolean",
                        "description": "Allows a Formal assessment Worker to save a fully prepared answer set without final submission. Mutually exclusive with formal_assessment_confirmation."
                    },
                    "strict_completion_retry_confirmation": {
                        "type": "object",
                        "required": ["workflow_id", "expected_revision"],
                        "additionalProperties": false,
                        "description": "Explicitly confirms one Formal Strict Completion retry against the current owner-scoped workflow revision. Submission retries also require a newly captured QuestionSnapshot and unused Draft.",
                        "properties": {
                            "workflow_id": {"type": "string", "format": "uuid"},
                            "expected_revision": {"type": "integer", "format": "int64", "minimum": 1}
                        }
                    },
                    "score_improvement_retake_confirmation": {
                        "type": "object",
                        "required": ["workflow_id", "expected_revision"],
                        "additionalProperties": false,
                        "description": "Binds an already-created fresh remote retake (rediscovered as pending or in_progress) to this Execution and consumes one bounded Score Improvement attempt atomically. It never reuses a completed attempt.",
                        "properties": {
                            "workflow_id": {"type": "string", "format": "uuid"},
                            "expected_revision": {"type": "integer", "format": "int64", "minimum": 1}
                        }
                    },
                    "billing_amount": {
                        "type": "integer",
                        "format": "int64",
                        "minimum": 1,
                        "description": "Optional one-shot credit amount. Must be supplied together with billing_pricing_revision and billing_reason; the quote and reservation are persisted atomically with this Execution."
                    },
                    "billing_pricing_revision": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 128,
                        "description": "Admin pricing policy revision for the one-shot quote."
                    },
                    "billing_reason": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 512,
                        "description": "Human-readable billing reason persisted with the quote."
                    },
                    "ai_profile": {
                        "type": "string",
                        "enum": ["economy", "gpt_only"],
                        "description": "Optional administrator-only AI combination override frozen onto this Execution."
                    },
                    "ai_route": {
                        "type": "string",
                        "enum": ["timed", "untimed", "escalation"],
                        "description": "Optional administrator-only AI route override frozen onto this Execution."
                    }
                }
            }}}
        },
        "responses": {
            "200": {"description": "Idempotent replay of an existing Execution"},
            "201": {"description": "Execution scheduled atomically"},
            "400": {"description": "Invalid task ID, idempotency key, or request body"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"},
            "404": {"description": "Task not found for this owner"},
            "409": {"description": "Task state, capability, assessment policy, or idempotency conflict"}
        }
    }})
}

fn execution_invocation_drafts_path() -> Value {
    json!({"post": {
        "operationId": "prepareExecutionInvocationDraft",
        "description": "Freshly validates Provider-private input, encrypts it at rest, and creates an immutable owner/account/Course/Task/capability-bound draft before any Execution or remote mutation exists.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "Idempotency-Key", "in": "header", "required": true, "schema": {"type": "string", "minLength": 1, "maxLength": 256}},
            {"name": "x-asterism-invocation-input-type", "in": "header", "required": true, "schema": {"type": "string", "minLength": 1, "maxLength": 128}},
            {"name": "x-asterism-requested-capabilities", "in": "header", "required": true, "description": "Comma-separated exact executable capabilities.", "schema": {"type": "string", "maxLength": 256}},
            {"name": "x-asterism-submission-draft-id", "in": "header", "required": false, "schema": {"type": "string", "format": "uuid"}}
        ],
        "requestBody": {
            "required": true,
            "content": {"application/octet-stream": {"schema": {
                "type": "string",
                "format": "binary",
                "minLength": 1,
                "maxLength": 67_108_864
            }}}
        },
        "responses": {
            "200": {"description": "Idempotent replay of an existing invocation draft", "content": {"application/json": {"schema": execution_invocation_draft_response_schema()}}},
            "201": {"description": "Encrypted immutable invocation draft created", "content": {"application/json": {"schema": execution_invocation_draft_response_schema()}}},
            "400": {"description": "Invalid headers or private input"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"},
            "404": {"description": "Task or SubmissionDraft not found for this owner"},
            "409": {"description": "Task, Provider, capability, Draft, preparation, or idempotency conflict"},
            "503": {"description": "Encrypted secret store unavailable"}
        }
    }})
}

fn execution_invocation_draft_response_schema() -> Value {
    json!({
        "type": "object",
        "required": [
            "draft_id", "provider_id", "provider_version", "task_id",
            "requested_capabilities", "private_input_type", "private_input_digest",
            "plan_artifact_type", "plan_artifact_digest", "created_at", "created"
        ],
        "properties": {
            "draft_id": {"type": "string", "format": "uuid"},
            "provider_id": {"type": "string"},
            "provider_version": {"type": "string"},
            "task_id": {"type": "string", "format": "uuid"},
            "requested_capabilities": {"type": "array", "items": {"type": "string"}},
            "submission_draft_id": {"type": ["string", "null"], "format": "uuid"},
            "private_input_type": {"type": "string"},
            "private_input_digest": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "plan_artifact_type": {"type": "string"},
            "plan_artifact_digest": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "created_at": {"type": "string", "format": "date-time"},
            "created": {"type": "boolean"}
        }
    })
}

fn task_lifecycle_path(operation_id: &str, requires_body: bool) -> Value {
    let mut operation = json!({
        "operationId": operation_id,
        "description": "Applies an owner-scoped, idempotent Core Task lifecycle action. Approval never grants formal-assessment execution or submission permission; cancellation refuses already claimed/running remote work.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "Idempotency-Key", "in": "header", "required": true, "schema": {"type": "string", "minLength": 1, "maxLength": 256}}
        ],
        "responses": {
            "200": {"description": "Idempotent replay of the same Task lifecycle action"},
            "201": {"description": "Task lifecycle action committed atomically with audit and outbox effects"},
            "400": {"description": "Invalid Task ID, idempotency key, or delay timestamp"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Insufficient permission"},
            "404": {"description": "Task not found for this owner"},
            "409": {"description": "Task state, scheduler claim, active remote work, or idempotency conflict"}
        }
    });
    if requires_body {
        operation["requestBody"] = json!({
            "required": true,
            "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["delayed_until"],
                "properties": {"delayed_until": {"type": "string", "format": "date-time"}},
                "additionalProperties": false
            }}}
        });
    }
    json!({"post": operation})
}

fn admin_users_path() -> Value {
    json!({
        "get": {
            "operationId": "listAdminUsers",
            "description": "Lists password-free user profiles for a Web Session with ManageUsers.",
            "security": [{"cookieAuth": []}],
            "parameters": admin_page_parameters(),
            "responses": {
                "200": {"description": "Paginated password-free user profiles"},
                "400": {"description": "Invalid pagination"},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManageUsers Web permission required"}
            }
        },
        "post": {
            "operationId": "createAdminUser",
            "description": "Creates one user with an Argon2id password hash, initial zero credit account, Audit and Outbox event. Plaintext password is never returned.",
            "security": [{"cookieAuth": []}],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["username", "password", "roles"],
                "properties": {
                    "username": {"type": "string", "minLength": 1, "maxLength": 64},
                    "password": {"type": "string", "format": "password", "minLength": 8, "maxLength": 1024, "writeOnly": true},
                    "status": {"$ref": "#/components/schemas/UserStatus"},
                    "roles": {"type": "array", "minItems": 1, "uniqueItems": true, "items": {"$ref": "#/components/schemas/Role"}},
                    "permissions": {"type": "array", "uniqueItems": true, "items": {"$ref": "#/components/schemas/Permission"}}
                },
                "additionalProperties": false
            }}}},
            "responses": {
                "201": {"description": "Password-free user profile created"},
                "400": {"description": "Invalid username, password, roles or permissions"},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManageUsers Web permission required"},
                "409": {"description": "Username already exists"}
            }
        }
    })
}

fn admin_ai_config_path() -> Value {
    json!({
        "get": {
            "operationId": "getAdminAiConfig",
            "description": "Returns the deployment-local AI endpoint and combination configuration. API keys remain environment-only.",
            "security": [{"cookieAuth": []}],
            "responses": {
                "200": {"description": "Current local AI configuration", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/AiConfig"}}}},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManageSystem permission required"}
            }
        },
        "put": {
            "operationId": "putAdminAiConfig",
            "description": "Validates and atomically replaces the deployment-local AI configuration; API keys are never accepted.",
            "security": [{"cookieAuth": []}],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/AiConfig"}}}},
            "responses": {
                "200": {"description": "Updated local AI configuration", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/AiConfig"}}}},
                "400": {"description": "Invalid AI configuration"},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManageSystem permission required"}
            }
        }
    })
}

fn admin_ai_usage_path() -> Value {
    json!({"get": {
        "operationId": "listAdminAiUsage",
        "description": "Lists deployment-local AI usage metadata without exposing prompts, answers, API keys or remote credentials.",
        "security": [{"cookieAuth": []}],
        "parameters": admin_page_parameters(),
        "responses": {
            "200": {"description": "Paginated local AI usage metadata", "content": {"application/json": {"schema": {"type": "object", "required": ["items", "total", "limit", "offset"], "properties": {"items": {"type": "array", "items": {"type": "object"}}, "total": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}}}}}},
            "400": {"description": "Invalid pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "ManageSystem permission required"}
        }
    }})
}

fn admin_answer_bank_usage_path() -> Value {
    json!({"get": {
        "operationId": "listAdminAnswerBankUsage",
        "description": "Lists deployment-local answer-bank/cache hit usage and sanitized settlement metadata.",
        "security": [{"cookieAuth": []}],
        "parameters": admin_page_parameters(),
        "responses": {
            "200": {"description": "Paginated answer-bank usage metadata", "content": {"application/json": {"schema": {"type": "object", "required": ["items", "total", "limit", "offset"], "properties": {"items": {"type": "array", "items": {"type": "object"}}, "total": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}}}}}},
            "400": {"description": "Invalid pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "ManageSystem permission required"}
        }
    }})
}

fn admin_pricing_catalog_path() -> Value {
    json!({
        "get": {
            "operationId": "getAdminPricingCatalog",
            "security": [{"cookieAuth": []}],
            "responses": {
                "200": {"description": "Current deployment-local pricing catalog or null", "content": {"application/json": {"schema": {"type": ["object", "null"]}}}},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManagePricing permission required"}
            }
        },
        "put": {
            "operationId": "putAdminPricingCatalog",
            "security": [{"cookieAuth": []}],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["revision", "catalog"],
                "additionalProperties": false,
                "properties": {
                    "revision": {"type": "string", "minLength": 1, "maxLength": 128},
                    "catalog": {"type": "object"},
                    "effective_from": {"type": "string", "format": "date-time"},
                    "expires_at": {"type": "string", "format": "date-time"}
                }
            }}}},
            "responses": {
                "200": {"description": "Pricing catalog revision stored locally", "content": {"application/json": {"schema": {"type": "object"}}}},
                "400": {"description": "Invalid pricing catalog"},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManagePricing permission required"},
                "409": {"description": "Pricing revision already exists"}
            }
        }
    })
}

fn chaoxing_verification_path() -> Value {
    json!({"get": {
        "operationId": "getChaoxingVerificationStatus",
        "description": "Shows deployment-local automatic Chaoxing verification budget and recent account-scoped observations without exposing captcha material.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [{"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
        "responses": {
            "200": {"description": "Chaoxing verification budget and recent results", "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["provider_account_id", "provider_id", "automatic_attempt_budget", "automatic_time_budget_seconds", "recent_attempts"],
                "properties": {
                    "provider_account_id": {"type": "string", "format": "uuid"},
                    "provider_id": {"type": "string"},
                    "automatic_attempt_budget": {"type": "integer", "minimum": 0},
                    "automatic_time_budget_seconds": {"type": "integer", "minimum": 0},
                    "recent_attempts": {"type": "array", "items": {"type": "object", "required": ["occurred_at", "source", "stage", "result", "next_retry_at", "message"], "properties": {"occurred_at": {"type": "string", "format": "date-time"}, "source": {"type": "string", "enum": ["execution", "scan", "question_read", "assessment"]}, "stage": {"type": "string"}, "result": {"type": "string"}, "next_retry_at": {"type": ["string", "null"], "format": "date-time"}, "message": {"type": "string"}}}}
                }
            }}}},
            "403": {"description": "Provider settings permission required"},
            "404": {"description": "Provider account not found"},
            "409": {"description": "Account is not Chaoxing"}
        }
    }})
}

fn admin_user_path() -> Value {
    json!({
        "get": {
            "operationId": "getAdminUser",
            "security": [{"cookieAuth": []}],
            "parameters": [{"name": "user_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
            "responses": {
                "200": {"description": "One password-free user profile"},
                "400": {"description": "Invalid user ID"},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManageUsers Web permission required"},
                "404": {"description": "User not found"}
            }
        },
        "put": {
            "operationId": "updateAdminUser",
            "description": "Revision-guards status, roles and explicit permissions and refuses removal of the final active Master.",
            "security": [{"cookieAuth": []}],
            "parameters": [{"name": "user_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["expected_updated_at", "status", "roles"],
                "properties": {
                    "expected_updated_at": {"type": "string", "format": "date-time"},
                    "status": {"$ref": "#/components/schemas/UserStatus"},
                    "roles": {"type": "array", "minItems": 1, "uniqueItems": true, "items": {"$ref": "#/components/schemas/Role"}},
                    "permissions": {"type": "array", "uniqueItems": true, "items": {"$ref": "#/components/schemas/Permission"}}
                },
                "additionalProperties": false
            }}}},
            "responses": {
                "200": {"description": "Updated password-free user profile"},
                "400": {"description": "Invalid user ID, revision, roles or permissions"},
                "401": {"description": "Authentication required"},
                "403": {"description": "ManageUsers Web permission required"},
                "404": {"description": "User not found"},
                "409": {"description": "Revision conflict or final active Master safeguard"}
            }
        }
    })
}

fn admin_user_password_path() -> Value {
    json!({"put": {
        "operationId": "resetAdminUserPassword",
        "description": "Replaces a user's password under an optimistic revision guard and revokes every existing Web session for that user.",
        "security": [{"cookieAuth": []}],
        "parameters": [{"name": "user_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {
            "type": "object",
            "required": ["password", "expected_updated_at"],
            "properties": {
                "password": {"type": "string", "format": "password", "minLength": 8, "maxLength": 1024, "writeOnly": true},
                "expected_updated_at": {"type": "string", "format": "date-time"}
            },
            "additionalProperties": false
        }}}},
        "responses": {
            "204": {"description": "Password reset and all Web sessions revoked"},
            "400": {"description": "Invalid user ID, password or revision"},
            "401": {"description": "Authentication required"},
            "403": {"description": "ManageUsers Web permission required"},
            "404": {"description": "User not found"},
            "409": {"description": "User revision conflict"}
        }
    }})
}

fn admin_credit_grant_path() -> Value {
    json!({"post": {
        "operationId": "grantUserCredits",
        "description": "Atomically grants credit, immutable ledger transaction, sanitized Audit, Outbox event and persistent idempotency receipt. Exact retries return the original post-grant snapshot.",
        "security": [{"cookieAuth": []}],
        "parameters": [
            {"name": "user_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "Idempotency-Key", "in": "header", "required": true, "schema": {"type": "string", "minLength": 1, "maxLength": 256}}
        ],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {
            "type": "object",
            "required": ["amount", "reason"],
            "properties": {
                "amount": {"type": "integer", "minimum": 1, "maximum": 9_223_372_036_854_775_807_u64},
                "reason": {"type": "string", "minLength": 1, "maxLength": 256}
            },
            "additionalProperties": false
        }}}},
        "responses": {
            "200": {"description": "Exact idempotent replay of the original grant result"},
            "201": {"description": "Credit grant committed atomically"},
            "400": {"description": "Invalid user ID, amount, reason or idempotency key"},
            "401": {"description": "Authentication required"},
            "403": {"description": "GrantCredits Web permission required"},
            "404": {"description": "User not found"},
            "409": {"description": "Idempotency key is bound to a different grant"}
        }
    }})
}

fn audit_path() -> Value {
    let mut parameters = vec![
        json!({"name": "action", "in": "query", "schema": {"type": "string", "minLength": 1, "maxLength": 128}}),
        json!({"name": "resource_type", "in": "query", "schema": {"type": "string", "minLength": 1, "maxLength": 128}}),
        json!({"name": "resource_id", "in": "query", "schema": {"type": "string", "minLength": 1, "maxLength": 128}}),
        json!({"name": "outcome", "in": "query", "schema": {"type": "string", "minLength": 1, "maxLength": 128}}),
    ];
    parameters.extend(admin_page_parameters());
    json!({"get": {
        "operationId": "listAuditRecords",
        "description": "Lists immutable sanitized Audit records. ViewAnyAudit sees all records; ViewOwnAudit and owner-bound AuditRead service tokens receive a non-leaking owner scope.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": parameters,
        "responses": {
            "200": {"description": "Paginated immutable sanitized Audit records"},
            "400": {"description": "Invalid filter or pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Audit permission or owner binding required"}
        }
    }})
}

fn admin_page_parameters() -> Vec<Value> {
    vec![
        json!({"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}}),
        json!({"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}),
    ]
}

fn task_detail_path() -> Value {
    json!({"get": {
        "operationId": "getTaskDetail",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Fresh bounded and sanitized Provider Task detail"},
            "400": {"description": "Invalid Task or request ID"},
            "404": {"description": "Task not found"},
            "409": {"description": "Provider account, capability, user action, or remote binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned inconsistent detail"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn task_browser_session_spec_path() -> Value {
    json!({"get": {
        "operationId": "getTaskBrowserSessionSpec",
        "description": "Freshly rebinds an owner-scoped Task and returns only the bounded credential-free BrowserBridge session policy. It does not launch a helper, inject credentials, interact with the remote page, or claim task completion.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Fresh bounded BrowserBridge session policy"},
            "400": {"description": "Invalid Task or request ID"},
            "404": {"description": "Task not found"},
            "409": {"description": "Task/Provider capability, account, user action, or remote binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned an unsafe browser policy"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn create_browser_bridge_session_path() -> Value {
    json!({"post": {
        "operationId": "createBrowserBridgeSession",
        "description": "Freshly rebinds the owner/account/Task/Provider capability, freezes the validated BrowserBridge policy, and returns one short-lived pairing token exactly once.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "201": {"description": "Durable BrowserBridge helper session created; pairing token returned once"},
            "400": {"description": "Invalid Task or request ID"},
            "401": {"description": "Authentication required"},
            "403": {"description": "TaskExecute permission required"},
            "404": {"description": "Task not found"},
            "409": {"description": "Task/Provider capability, account, or remote binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned an unsafe browser policy"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn browser_bridge_session_path() -> Value {
    let parameter = json!({"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}});
    json!({
        "get": {
            "operationId": "getBrowserBridgeSession",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [parameter.clone()],
            "responses": {
                "200": {"description": "Owner-scoped BrowserBridge session and frozen policy"},
                "400": {"description": "Invalid session ID"},
                "401": {"description": "Authentication required"},
                "403": {"description": "TaskRead permission required"},
                "404": {"description": "Session not found"}
            }
        },
        "delete": {
            "operationId": "cancelBrowserBridgeSession",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [parameter],
            "responses": {
                "200": {"description": "Live BrowserBridge session cancelled or durably expired and all tokens invalidated"},
                "400": {"description": "Invalid session ID"},
                "401": {"description": "Authentication required"},
                "403": {"description": "TaskExecute permission required"},
                "404": {"description": "Session not found"},
                "409": {"description": "Session is terminal or changed concurrently"}
            }
        }
    })
}

fn claim_browser_bridge_session_path() -> Value {
    json!({"post": {
        "operationId": "claimBrowserBridgeSession",
        "security": [{"browserBridgeAuth": []}],
        "parameters": [
            {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Pairing consumed; session-bound helper access token returned once with the frozen policy"},
            "400": {"description": "Invalid session ID"},
            "401": {"description": "Pairing token is invalid, expired, cancelled, or already used"},
            "429": {"description": "Pairing claim rate limit reached"}
        }
    }})
}

fn browser_bridge_snapshot_path() -> Value {
    json!({"get": {
        "operationId": "pollBrowserBridgeSnapshot",
        "security": [{"browserBridgeAuth": []}],
        "parameters": [
            {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Exact claimed session and frozen credential-free policy"},
            "400": {"description": "Invalid session ID"},
            "401": {"description": "Session-scoped BrowserBridge access token is invalid or expired"}
        }
    }})
}

fn browser_bridge_binding_path() -> Value {
    json!({"put": {
        "operationId": "bindBrowserBridgeRuntime",
        "description": "Authenticates the claimed helper and freezes its first exact browser origin/frame identity. Identical retries return the original binding; a conflicting second writer never replaces it.",
        "security": [{"browserBridgeAuth": []}],
        "parameters": [
            {"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "requestBody": {"required": true, "content": {
            "application/json": {"schema": {"$ref": "#/components/schemas/BrowserBridgeRuntimeBindingRequest"}}
        }},
        "responses": {
            "200": {"description": "First durable runtime binding or identical retry"},
            "400": {"description": "Invalid session, origin, frame or request ID"},
            "401": {"description": "Session-scoped BrowserBridge access token is invalid or expired"},
            "409": {"description": "Runtime origin or frame conflicts with the frozen policy or first writer"}
        }
    }})
}

fn browser_bridge_command_path() -> Value {
    json!({"get": {
        "operationId": "dispatchBrowserBridgeCommand",
        "description": "Atomically marks one encrypted Provider command dispatched and returns its exact opaque bytes once. A retry never replays the command.",
        "security": [{"browserBridgeAuth": []}],
        "parameters": browser_bridge_exchange_parameters(),
        "responses": {
            "200": {
                "description": "First and only dispatch of the exact Provider command",
                "headers": {
                    "X-Asterism-Browser-Command-Type": {"schema": {"type": "string"}},
                    "X-Asterism-Browser-Command-Digest": {"schema": {"type": "string", "pattern": "^[0-9a-f]{64}$"}}
                },
                "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}
            },
            "400": {"description": "Invalid session ID or sequence"},
            "401": {"description": "Session-scoped BrowserBridge access token is invalid or expired"},
            "404": {"description": "The requested command has not been issued"},
            "409": {"description": "The command was already dispatched or conflicts with durable state"},
            "503": {"description": "Encrypted BrowserBridge artifact storage is unavailable"}
        }
    }})
}

fn browser_bridge_result_path() -> Value {
    let mut parameters = browser_bridge_exchange_parameters();
    parameters.push(json!({
        "name": "X-Asterism-Browser-Result-Type",
        "in": "header",
        "required": true,
        "schema": {"type": "string", "minLength": 1, "maxLength": 96, "pattern": "^[a-z0-9._-]+$"}
    }));
    json!({"post": {
        "operationId": "receiveBrowserBridgeResult",
        "description": "Authenticates and encrypts the first exact opaque helper result before Provider parsing. Identical retries return the original receipt; conflicts never replace it.",
        "security": [{"browserBridgeAuth": []}],
        "parameters": parameters,
        "requestBody": {"required": true, "content": {
            "application/octet-stream": {"schema": {"type": "string", "format": "binary", "minLength": 1, "maxLength": 262_144}}
        }},
        "responses": {
            "200": {"description": "Identical result was already durably received"},
            "202": {"description": "Result durably encrypted and accepted for Provider validation"},
            "400": {"description": "Invalid session, sequence, type, content type or artifact"},
            "401": {"description": "Session-scoped BrowserBridge access token is invalid or expired"},
            "409": {"description": "Result conflicts with command dispatch or the first durable receipt"},
            "413": {"description": "Result exceeds the bounded helper transport size"},
            "503": {"description": "Encrypted BrowserBridge artifact storage is unavailable"}
        }
    }})
}

fn browser_bridge_exchange_parameters() -> Vec<Value> {
    vec![
        json!({"name": "session_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}),
        json!({"name": "sequence", "in": "path", "required": true, "schema": {"type": "integer", "format": "uint64", "minimum": 1}}),
    ]
}

fn task_progress_path() -> Value {
    json!({"get": {
        "operationId": "getTaskProgress",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Fresh bounded Provider Task progress"},
            "400": {"description": "Invalid Task or request ID"},
            "404": {"description": "Task not found"},
            "409": {"description": "Task/Provider capability, account, user action, or remote binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned inconsistent progress"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn task_duration_path() -> Value {
    json!({"get": {
        "operationId": "getTaskDuration",
        "description": "Reads normalized learning duration through the independent read-only DurationRead capability; this never reports or mutates duration.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Fresh bounded Provider Task duration in seconds"},
            "400": {"description": "Invalid Task or request ID"},
            "404": {"description": "Task not found"},
            "409": {"description": "Task/Provider capability, account, user action, or remote binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned inconsistent duration"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn task_questions_path() -> Value {
    json!({"get": {
        "operationId": "getTaskQuestions",
        "description": "Runs the Provider Question flow with durable non-idempotent operation tracking, then atomically persists one fresh complete Question snapshot; formal assessments require an explicit per-request read confirmation because opening them may start or consume a remote attempt.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "confirm_formal_read", "in": "query", "required": false, "schema": {"type": "boolean", "default": false}, "description": "Explicitly confirms that reading a formal assessment may start or consume a remote attempt."}
        ],
        "responses": {
            "200": {"description": "Fresh bounded, sanitized and deterministically ordered Provider Questions with immutable snapshot identity"},
            "204": {"description": "Provider definitively reported completion before yielding a Question"},
            "400": {"description": "Invalid Task or request ID"},
            "404": {"description": "Task not found"},
            "409": {"description": "Task/Provider capability, account, durable attempt, user action, or remote binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned inconsistent Questions"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn task_question_snapshot_path() -> Value {
    json!({"get": {
        "operationId": "getTaskQuestionSnapshot",
        "description": "Reads one exact immutable, owner-scoped Question snapshot without contacting the Provider.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Persisted bounded and sanitized Questions with their immutable snapshot identity"},
            "400": {"description": "Invalid Task or Question snapshot ID"},
            "404": {"description": "Task-bound owner-scoped Question snapshot not found"}
        }
    }})
}

fn provider_answer_candidates_path() -> Value {
    json!({"post": {
        "operationId": "resolveProviderAnswerCandidates",
        "description": "Resolves and atomically persists Provider-native AnswerCandidates for one explicit immutable Question snapshot; this does not build or execute a submission.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Complete validated Provider-native candidate batch, possibly empty"},
            "400": {"description": "Invalid Task, Question snapshot, or request ID"},
            "404": {"description": "Task or owner-scoped Question snapshot not found"},
            "409": {"description": "Capability, account, policy, user action, or snapshot binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned inconsistent candidates"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn answer_candidates_path() -> Value {
    json!({
        "get": {
            "operationId": "listAnswerCandidates",
            "description": "Reads persisted multi-source AnswerCandidates for one owner-scoped Task/QuestionSnapshot binding without calling a Provider.",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [
                {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
            ],
            "responses": {
                "200": {"description": "Persisted bounded AnswerCandidates ordered by Question and observation time"},
                "400": {"description": "Invalid Task or Question snapshot ID"},
                "404": {"description": "Task-bound owner-scoped Question snapshot not found"}
            }
        },
        "post": {
            "operationId": "createManualAnswerCandidate",
            "description": "Creates one Core-attributed Manual AnswerCandidate for an explicit owner-scoped Task, QuestionSnapshot and Question without calling a Provider.",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [
                {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
                {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
            ],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {
                "type": "object",
                "required": ["question_id", "answer"],
                "properties": {
                    "question_id": {"type": "string", "format": "uuid"},
                    "answer": {"$ref": "#/components/schemas/NormalizedAnswer"},
                    "confidence_basis_points": {"type": ["integer", "null"], "minimum": 0, "maximum": 10000},
                    "explanation": {"type": ["string", "null"], "minLength": 1, "maxLength": 65536}
                },
                "additionalProperties": false
            }}}},
            "responses": {
                "201": {"description": "One immutable Core-attributed Manual AnswerCandidate"},
                "400": {"description": "Invalid JSON, identity, confidence, or normalized answer"},
                "404": {"description": "Task-bound owner-scoped Question snapshot or Question not found"}
            }
        }
    })
}

fn local_answer_cache_path() -> Value {
    json!({"post": {
        "operationId": "importLocalAnswerCandidates",
        "description": "Imports direct candidates from exact, unambiguous earlier Question matches in the same owner-scoped Task. Core fixes LocalCache attribution; no Provider is called and no answer is selected.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Newly imported immutable LocalCache candidates; an empty collection is an idempotent success"},
            "400": {"description": "Invalid Task or Question snapshot ID"},
            "404": {"description": "Task-bound owner-scoped Question snapshot not found"},
            "500": {"description": "Persisted cache evidence violated its validated binding"}
        }
    }})
}

fn normalized_answer_schema() -> Value {
    json!({"oneOf": [
        {
            "type": "object",
            "required": ["type", "value"],
            "properties": {
                "type": {"type": "string", "enum": ["selections", "texts", "ordering"]},
                "value": {"type": "array", "minItems": 1, "maxItems": 256, "items": {"type": "string", "minLength": 1}}
            },
            "additionalProperties": false
        },
        {
            "type": "object",
            "required": ["type", "value"],
            "properties": {
                "type": {"const": "boolean"},
                "value": {"type": "boolean"}
            },
            "additionalProperties": false
        },
        {
            "type": "object",
            "required": ["type", "value"],
            "properties": {
                "type": {"const": "pairs"},
                "value": {"type": "array", "minItems": 1, "maxItems": 256, "items": {
                    "type": "object",
                    "required": ["left", "right"],
                    "properties": {"left": {"type": "string", "minLength": 1}, "right": {"type": "string", "minLength": 1}},
                    "additionalProperties": false
                }}
            },
            "additionalProperties": false
        },
        {
            "type": "object",
            "required": ["type", "value"],
            "properties": {
                "type": {"const": "composite"},
                "value": {"type": "array", "minItems": 1, "maxItems": 256, "items": {"$ref": "#/components/schemas/NormalizedAnswer"}}
            },
            "additionalProperties": false
        },
        {
            "type": "object",
            "required": ["type"],
            "properties": {
                "type": {"const": "skip"}
            },
            "additionalProperties": false
        },
        {
            "type": "object",
            "required": ["type"],
            "properties": {
                "type": {"const": "unknown"}
            },
            "additionalProperties": false
        }
    ]})
}

fn answer_resolution_path() -> Value {
    json!({"get": {
        "operationId": "resolveAnswerCandidates",
        "description": "Derives a non-persisted source-neutral AnswerResolutionPlan from owner-scoped stored evidence; only unanimous known answers are selected.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Reviewable Selected, Conflict and Missing decisions without a persisted winner"},
            "400": {"description": "Invalid Task or Question snapshot ID"},
            "404": {"description": "Task-bound owner-scoped Question snapshot not found"}
        }
    }})
}

fn task_completion_workflows_path() -> Value {
    json!({"get": {
        "operationId": "getTaskCompletionWorkflows",
        "description": "Reads the owner-scoped Strict Completion and Score Improvement workflow snapshots and revisions for one Task without starting or mutating either workflow.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Current workflow snapshots; each absent workflow is returned as null"},
            "400": {"description": "Invalid Task ID"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Task read permission is required"},
            "404": {"description": "Owner-scoped Task not found"}
        }
    }})
}

fn score_improvement_opt_in_path() -> Value {
    json!({"post": {
        "operationId": "optInScoreImprovement",
        "description": "Records an explicit owner opt-in from a verified completion baseline and the latest exact Provider result facts. This operation never creates or starts a remote retake.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ScoreImprovementOptInRequest"}}}},
        "responses": {
            "200": {"description": "Existing idempotent Score Improvement opt-in", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ScoreImprovementOptInResponse"}}}},
            "201": {"description": "Score Improvement opt-in recorded", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ScoreImprovementOptInResponse"}}}},
            "400": {"description": "Invalid Task ID or explicit opt-in body"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Task execution permission is required"},
            "404": {"description": "Owner-scoped Task not found"},
            "409": {"description": "Completion, history, policy, retake or workflow precondition failed"}
        }
    }})
}

fn task_attempt_history_path() -> Value {
    json!({"get": {
        "operationId": "listTaskAttemptHistory",
        "description": "Lists owner-scoped Execution attempts for one Task with learned evidence counts, immutable Submission references, answer-source counts, verification summaries and normalized score deltas.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
            {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
        ],
        "responses": {
            "200": {"description": "Paginated Task attempt history; full selected answers and per-Question readback remain available through the referenced Draft and Result resources"},
            "400": {"description": "Invalid Task ID or pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Task read permission is required"},
            "404": {"description": "Owner-scoped Task not found"}
        }
    }})
}

fn course_progress_path() -> Value {
    json!({"get": {
        "operationId": "getCourseProgress",
        "description": "Reads an owner-scoped aggregate over persisted Course Tasks and latest verified scored results without a Provider call; unsupported required and duration dimensions remain null.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "course_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Course metadata and aggregate completion, blocker and score facts"},
            "400": {"description": "Invalid Course ID"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Task read permission is required"},
            "404": {"description": "Owner-scoped Course not found"}
        }
    }})
}

fn courses_path() -> Value {
    json!({"get": {
        "operationId": "listCourses",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "provider_account_id", "in": "query", "schema": {"type": "string", "format": "uuid"}},
            {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
            {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
        ],
        "responses": {"200": {"description": "Owner-scoped discovered Course page"}, "400": {"description": "Invalid filter or pagination"}, "401": {"description": "Authentication required"}}
    }})
}

fn course_path() -> Value {
    json!({"get": {
        "operationId": "getCourse",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [{"name": "course_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
        "responses": {"200": {"description": "Owner-scoped discovered Course"}, "400": {"description": "Invalid Course ID"}, "401": {"description": "Authentication required"}, "404": {"description": "Course not found"}}
    }})
}

fn course_automation_path() -> Value {
    json!({
        "get": {
            "operationId": "getCourseAutomation",
            "description": "Reads whether the owner opted this Course into automatic execution after patrol.",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [{"name": "course_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
            "responses": {"200": {"description": "Course automation policy; absent or paused means disabled", "content": {"application/json": {"schema": {"type": "object", "required": ["enabled"], "properties": {"enabled": {"type": "boolean"}, "status": {"type": ["string", "null"], "enum": ["draft", "active", "paused", "expired", "cancelled", null]}, "updated_at": {"type": ["string", "null"], "format": "date-time"}, "ai_profile": {"type": ["string", "null"], "enum": ["economy", "gpt_only", null]}}}}}}, "401": {"description": "Authentication required"}, "403": {"description": "Task read permission is required"}, "404": {"description": "Course not found"}}
        },
        "put": {
            "operationId": "configureCourseAutomation",
            "description": "Explicitly enables or disables automatic execution for this Course. Default is disabled.",
            "security": [{"cookieAuth": []}, {"bearerAuth": []}],
            "parameters": [{"name": "course_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "required": ["enabled"], "properties": {"enabled": {"type": "boolean"}, "ai_profile": {"type": ["string", "null"], "enum": ["economy", "gpt_only", null], "description": "Optional per-course override; null inherits the deployment default."}}, "additionalProperties": false}}}},
            "responses": {"200": {"description": "Course automation policy updated", "content": {"application/json": {"schema": {"type": "object", "required": ["enabled"], "properties": {"enabled": {"type": "boolean"}, "status": {"type": ["string", "null"], "enum": ["draft", "active", "paused", "expired", "cancelled", null]}, "updated_at": {"type": ["string", "null"], "format": "date-time"}, "ai_profile": {"type": ["string", "null"], "enum": ["economy", "gpt_only", null]}}}}}}, "400": {"description": "Invalid Course ID or request"}, "401": {"description": "Authentication required"}, "403": {"description": "Task read permission is required"}, "404": {"description": "Course not found"}}
        }
    })
}

fn welearn_batch_executions_path() -> Value {
    json!({"post": {
        "operationId": "createWellearnBatchExecution",
        "description": "Creates one owner-authorized WELearn Course batch from an explicit donor flow, frozen unit selection and duration policy.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "course_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "Idempotency-Key", "in": "header", "required": true, "schema": {"type": "string", "minLength": 1, "maxLength": 256}}
        ],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {
            "type": "object",
            "required": ["course_remote_id", "flow", "expected_remote_task_id", "duration", "expected_child_count"],
            "properties": {
                "course_remote_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "flow": {"type": "string", "enum": ["fanyuchang_duration", "auto_duration"]},
                "selected_unit_indices": {"type": ["array", "null"], "items": {"type": "integer", "minimum": 0}, "maxItems": 8192},
                "expected_remote_task_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "duration": {"oneOf": [
                    {"type": "object", "required": ["kind", "target_seconds"], "properties": {"kind": {"const": "per_child_seconds"}, "target_seconds": {"type": "array", "minItems": 1, "maxItems": 8192, "items": {"type": "integer", "minimum": 0}}}, "additionalProperties": false},
                    {"type": "object", "required": ["kind", "configured_minutes", "random_range_minutes", "sampled_offset_minutes"], "properties": {"kind": {"const": "auto_aggregate"}, "configured_minutes": {"type": "integer", "minimum": 1, "maximum": 300}, "random_range_minutes": {"type": "integer", "minimum": 0, "maximum": 30}, "sampled_offset_minutes": {"type": "integer", "minimum": -30, "maximum": 30}}, "additionalProperties": false}
                ]},
                "expected_child_count": {"type": "integer", "minimum": 1, "maximum": 8192}
            },
            "additionalProperties": false
        }}}},
        "responses": {
            "200": {"description": "Existing idempotent WELearn batch execution"},
            "201": {"description": "WELearn batch execution created and scheduled"},
            "400": {"description": "Invalid identity, flow, policy, count or request header"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Task execution permission is required"},
            "409": {"description": "Provider, binding or idempotency conflict"},
            "503": {"description": "WELearn Provider or encrypted planning storage unavailable"}
        }
    }})
}

fn provider_account_health_path() -> Value {
    json!({"get": {
        "operationId": "getProviderAccountHealth",
        "description": "Reads owner-scoped account health derived from the persisted authentication state and any newer attempt-bound ProtocolDrift fact.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "account_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Current Account Health with exact authentication and ProtocolDrift provenance"},
            "400": {"description": "Invalid ProviderAccount ID"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Provider account read permission is required"},
            "404": {"description": "Owner-scoped ProviderAccount not found"}
        }
    }})
}

fn protocol_observations_path() -> Value {
    json!({"get": {
        "operationId": "listProtocolObservations",
        "description": "Lists the Master-only instance Protocol Observation Inbox of bounded sanitized and replay-safe drift aggregates.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "provider_id", "in": "query", "schema": {"type": "string"}},
            {"name": "kind", "in": "query", "schema": {"type": "string", "enum": ["unknown_question_kind", "unknown_result_shape", "unknown_task_type", "field_drift", "endpoint_version_drift", "other"]}},
            {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}},
            {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0, "maximum": 1_000_000, "default": 0}}
        ],
        "responses": {
            "200": {"description": "Paginated Protocol Observation aggregates ordered by newest occurrence"},
            "400": {"description": "Invalid Provider, kind or pagination"},
            "401": {"description": "Authentication required"},
            "403": {"description": "Master user-management authority is required"}
        }
    }})
}

fn submission_drafts_path() -> Value {
    json!({"post": {
        "operationId": "buildSubmissionDraft",
        "description": "Builds and atomically persists one non-executable draft from exactly one explicit persisted AnswerCandidate per Question.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "requestBody": {"required": true, "content": {"application/json": {"schema": {
            "type": "object",
            "required": ["answer_candidate_ids"],
            "properties": {
                "answer_candidate_ids": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 5000,
                    "items": {"type": "string", "format": "uuid"}
                }
            },
            "additionalProperties": false
        }}}},
        "responses": {
            "201": {"description": "Complete persisted and reviewable SubmissionDraft"},
            "400": {"description": "Invalid Task, snapshot, Candidate, request ID, or body"},
            "404": {"description": "Task or owner-scoped Question snapshot not found"},
            "409": {"description": "Capability, account, selection, policy, user action, or snapshot binding conflict"},
            "429": {"description": "Provider rate limited"},
            "502": {"description": "Provider returned an inconsistent payload preview"},
            "503": {"description": "Provider temporarily unavailable"}
        }
    }})
}

fn submission_draft_path() -> Value {
    json!({"get": {
        "operationId": "getSubmissionDraft",
        "description": "Reads one persisted owner-scoped SubmissionDraft with exact Task and QuestionSnapshot binding and no Provider call.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "draft_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Persisted bounded SubmissionDraft rebuilt from authoritative Question and Candidate records"},
            "400": {"description": "Invalid Task, Question snapshot, or Submission draft ID"},
            "404": {"description": "Task/Snapshot-bound owner-scoped SubmissionDraft not found"}
        }
    }})
}

fn submission_result_path() -> Value {
    json!({"get": {
        "operationId": "getSubmissionResult",
        "description": "Reads one persisted owner-scoped SubmissionResult with exact Task, QuestionSnapshot and SubmissionDraft bindings and no Provider call.",
        "security": [{"cookieAuth": []}, {"bearerAuth": []}],
        "parameters": [
            {"name": "task_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "snapshot_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "draft_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}},
            {"name": "result_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
        ],
        "responses": {
            "200": {"description": "Persisted bounded SubmissionResult with receipt and independent verification snapshot"},
            "400": {"description": "Invalid Task, Question snapshot, Submission draft, or Submission result ID"},
            "404": {"description": "Fully bound owner-scoped SubmissionResult not found"}
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
    pub master_initialized: bool,
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

    fn invalid_browser_bridge_token() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_browser_bridge_token",
            message: "the BrowserBridge token is invalid or expired".to_owned(),
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
                match self.code {
                    "invalid_bootstrap_token" => {
                        HeaderValue::from_static("Bootstrap realm=\"asterism\"")
                    }
                    "invalid_browser_bridge_token" => {
                        HeaderValue::from_static("BrowserBridge realm=\"asterism\"")
                    }
                    _ => HeaderValue::from_static("Bearer realm=\"asterism\""),
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
        path::{Path, PathBuf},
        str::FromStr,
        time::Duration,
    };

    use asterism_domain::{
        AnswerCandidate, AnswerCandidateId, AnswerSource, AssessmentClass, AuthMethod, AuthState,
        BrowserBridgeExchange, BrowserBridgeRuntimeStateMetadata, BrowserBridgeSessionId,
        CompletionOutcome, CompletionPolicySnapshot, CompletionWorkflowBinding, ExecutionAttemptId,
        ExecutionId, NormalizedAnswer, ProtocolObservationKind, ProtocolSurface, ProviderAccountId,
        ProviderId, Question, QuestionId, QuestionKind, QuestionSnapshotId, RemoteState,
        RetakeScorePolicy, SelectedAnswer, SessionKind, SourceType, StrictCompletionWorkflow,
        SubmissionDraft, SubmissionDraftId, SubmissionDraftItem, SubmissionPayloadEncoding,
        SubmissionPayloadFieldPreview, SubmissionPayloadPreview, SubmissionQuestionVerification,
        SubmissionQuestionVerificationStatus, SubmissionReceipt, SubmissionResult,
        SubmissionResultId, SubmissionResultStatus, SubmissionScore,
        SubmissionVerificationSnapshot, SubmissionVerificationStatus, TaskId, UserId,
    };
    use asterism_provider_api::{
        AnswerHistoryRetakeFacts, AuthChallenge, AuthenticationCapability, BrowserBridgeCapability,
        BrowserSessionSpec, CaptureCredentialOutput, CaptureRecipe, CaptureValueSource,
        CourseInventoryCapability, CredentialValidation, ExecutionEventSink, ExecutionOutcome,
        ExecutionPlanningRequest, ExecutionRequest as ProviderExecutionRequest,
        ProviderAuthContext, ProviderCapability, ProviderContext, ProviderEntry,
        ProviderExecutionPlan, ProviderExecutionPlanArtifact, ProviderIdentity,
        ProviderInteractiveAuthBegin, ProviderInteractiveAuthContinuation,
        ProviderInteractiveAuthPollOutcome, ProviderMetadata, ProviderResult,
        ProviderRuntimeSettingsSchema, ProviderSettingCoreBehavior, ProviderSettingDefinition,
        ProviderSettingKind, ProviderSettingScope, ProviderSettingValue, RemoteCourse, RemoteTask,
        ResolvedProviderInteractiveAuthContinuation, SessionStatus, TaskExecutionCapability,
        TaskInventoryCapability, VerificationLevel,
    };
    use asterism_secrets::{
        CredentialAcquisition, CredentialBundle, CredentialField, SecretAccess, SecretActor,
        SecretKey, SecretValue,
    };
    use asterism_storage::{
        AnswerCandidateRecord, AnswerCandidateRepository, AnswerEvidenceRepository,
        AnswerHistoryIngestRequest, AnswerHistoryIngestionRepository, CompletionWorkflowRepository,
        ExecutionQueryRepository, ProtocolObservationRecordRequest, ProtocolObservationRepository,
        QuestionSnapshot, QuestionSnapshotRepository, SecretKeyring,
        SqliteAnswerEvidenceRepository, SqliteAnswerHistoryIngestionRepository,
        SqliteBrowserBridgeSessionRepository, SqliteCompletionWorkflowRepository,
        SqliteExecutionRepository, SqliteProtocolObservationRepository,
        SqliteQuestionSnapshotRepository, SubmissionDraftRepository, SubmissionResultRepository,
    };
    use asterism_uai_worker_client::UaiWorkerHealth;
    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Request, header},
    };
    use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
    use sha2::Digest;
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

    #[derive(Debug)]
    struct ApiTaskExecution {
        metadata: ProviderMetadata,
    }

    impl ProviderIdentity for ApiTaskExecution {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskExecutionCapability for ApiTaskExecution {
        async fn prepare_execution_plan(
            &self,
            context: &ProviderContext,
            request: &ExecutionPlanningRequest<'_>,
        ) -> ProviderResult<ProviderExecutionPlan> {
            assert_eq!(context.provider_id, self.metadata.id);
            assert_eq!(request.requested_capabilities.len(), 1);
            let artifact = ProviderExecutionPlanArtifact::try_new(
                self.metadata.id.clone(),
                "provider-alpha.api-planning.v1",
                json!({
                    "execution_id": request.execution_id,
                    "task_id": request.task_id,
                    "remote_task_id": request.remote_task_id,
                }),
            )?;
            ProviderExecutionPlan::try_new(
                self.metadata.id.clone(),
                vec![request.requested_capabilities.to_vec()],
                Some(artifact),
            )
        }

        async fn execute(
            &self,
            _context: &ProviderContext,
            _request: &ProviderExecutionRequest,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ExecutionOutcome> {
            Ok(ExecutionOutcome {
                remote_state: RemoteState::Completed,
                verified: false,
                result_sanitized: json!({"fixture": "api-scheduling-only"}),
            })
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
                capabilities: vec![asterism_domain::TaskCapability::BrowserBridge],
                fingerprint: "v1:fingerprint-a".to_owned(),
                normalized: json!({"revision": 1}),
                raw_sanitized: json!({"task": "safe"}),
            }])
        }
    }

    #[async_trait]
    impl BrowserBridgeCapability for ApiScanInventory {
        async fn browser_session_spec(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<BrowserSessionSpec> {
            assert_eq!(remote_task_id, "task-a");
            Ok(BrowserSessionSpec {
                version: 1,
                start_url: "https://provider-alpha.example/task/a".to_owned(),
                isolation_key: "provider-alpha-task-a".to_owned(),
                allowed_origins: vec!["https://provider-alpha.example".to_owned()],
                read_sources: Vec::new(),
                headless: false,
            })
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
                ProviderCapability::BrowserBridge,
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
                runtime_settings: ProviderRuntimeSettingsSchema {
                    version: 1,
                    definitions: vec![ProviderSettingDefinition {
                        key: "discovery.scan_interval".to_owned(),
                        display_name: "Scan interval".to_owned(),
                        description: "Default periodic account inventory interval.".to_owned(),
                        kind: ProviderSettingKind::DurationSeconds {
                            minimum: 300,
                            maximum: 86_400,
                            step: 300,
                        },
                        default: ProviderSettingValue::DurationSeconds(1_800),
                        scopes: BTreeSet::from([
                            ProviderSettingScope::Provider,
                            ProviderSettingScope::ProviderAccount,
                        ]),
                        core_behavior: Some(ProviderSettingCoreBehavior::AccountScanInterval),
                    }],
                },
                authentication: None,
                course_inventory: Some(inventory.clone()),
                course_enrollment: None,
                task_inventory: Some(inventory.clone()),
                task_detail: None,
                task_progress: None,
                duration_read: None,
                question_inventory: None,
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                submission_execute: None,
                submission_verify: None,
                answer_history_harvest: None,
                task_execution: None,
                browser_bridge: Some(inventory),
            })
            .unwrap();
        registry
    }

    fn settings_registry() -> ProviderRegistry {
        let metadata = ProviderMetadata {
            id: asterism_domain::ProviderId::new("provider-alpha").unwrap(),
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::ResourceExecution]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.task_execution = Some(Arc::new(ApiTaskExecution {
            metadata: entry.metadata.clone(),
        }));
        entry.runtime_settings = ProviderRuntimeSettingsSchema {
            version: 2,
            definitions: vec![ProviderSettingDefinition {
                key: "execution.max_concurrency".to_owned(),
                display_name: "Execution concurrency".to_owned(),
                description: "Maximum concurrent execution work.".to_owned(),
                kind: ProviderSettingKind::Integer {
                    minimum: 1,
                    maximum: 8,
                    step: 1,
                },
                default: ProviderSettingValue::Integer(2),
                scopes: BTreeSet::from([
                    ProviderSettingScope::Provider,
                    ProviderSettingScope::ProviderAccount,
                    ProviderSettingScope::Task,
                ]),
                core_behavior: None,
            }],
        };
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        registry
    }

    async fn settings_test_app() -> (Router, Database) {
        let (app, database, _) = settings_test_app_with_events().await;
        (app, database)
    }

    async fn settings_test_app_with_events() -> (Router, Database, EventBus) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let events = EventBus::new(16);
        let state = ApiState::new(database.clone(), Arc::new(settings_registry()), 3600, false)
            .with_event_bus(events.clone());
        (build_router(state), database, events)
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
        fn supports_durable_interactive_authentication(&self) -> bool {
            true
        }

        fn capture_recipes(&self) -> Vec<CaptureRecipe> {
            vec![test_capture_recipe(3), test_capture_recipe(4)]
        }

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
                external_oauth: None,
            })
        }

        async fn begin_interactive_authentication(
            &self,
            context: &ProviderAuthContext,
            method: AuthMethod,
        ) -> ProviderResult<ProviderInteractiveAuthBegin> {
            Ok(ProviderInteractiveAuthBegin {
                challenge: AuthChallenge {
                    session_id: context.auth_session_id.unwrap_or_default(),
                    method,
                    waiting_for: asterism_domain::WaitingUserState::QrScan,
                    user_action: Some("https://provider-alpha.example/qr/opaque".to_owned()),
                    expires_at: None,
                    external_oauth: None,
                },
                continuation: ProviderInteractiveAuthContinuation::try_new(
                    &context.provider_id,
                    "provider-alpha.qr.v1",
                    "provider-alpha.qr-scan",
                    SecretValue::new(b"api-qr-state".to_vec()),
                    300,
                    3,
                )?,
            })
        }

        async fn poll_interactive_authentication(
            &self,
            context: &ProviderAuthContext,
            continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
        ) -> ProviderResult<ProviderInteractiveAuthPollOutcome> {
            assert_eq!(continuation.value.expose_secret(), b"api-qr-state");
            Ok(ProviderInteractiveAuthPollOutcome::Authenticated {
                continuation: ProviderInteractiveAuthContinuation::try_new(
                    &context.provider_id,
                    "provider-alpha.qr-terminal.v1",
                    "provider-alpha.authenticated",
                    SecretValue::new(b"api-qr-cookie".to_vec()),
                    300,
                    3,
                )?,
                result_digest: [8; 32],
            })
        }

        async fn finalize_interactive_authentication(
            &self,
            context: &ProviderAuthContext,
            continuation: ResolvedProviderInteractiveAuthContinuation<'_>,
        ) -> ProviderResult<CredentialBundle> {
            assert_eq!(continuation.value.expose_secret(), b"api-qr-cookie");
            Ok(CredentialBundle {
                provider_id: context.provider_id.clone(),
                tenant: None,
                auth_method: AuthMethod::QrCode,
                acquired_via: CredentialAcquisition::NativeProviderLogin,
                captured_at: Utc::now(),
                expires_at: None,
                session_kind: SessionKind::Cookie,
                fields: vec![CredentialField {
                    purpose: asterism_secrets::SecretPurpose::ProviderCookie,
                    value: SecretValue::new(b"api-qr-cookie".to_vec()),
                }],
                user_id_hint: Some("remote-account".to_owned()),
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
            auth_methods: BTreeSet::from([
                AuthMethod::ImportedCookie,
                AuthMethod::AssistedSession,
                AuthMethod::QrCode,
            ]),
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

    fn test_capture_recipe(version: u32) -> CaptureRecipe {
        CaptureRecipe {
            version,
            start_url: "https://provider-alpha.example/login".to_owned(),
            navigation_origins: vec!["https://provider-alpha.example".to_owned()],
            read_origins: vec!["https://provider-alpha.example".to_owned()],
            poll_interval_millis: 500,
            auth_method: AuthMethod::AssistedSession,
            session_kind: SessionKind::Cookie,
            readiness: asterism_provider_api::CaptureReadiness::OutputsComplete,
            outputs: vec![CaptureCredentialOutput {
                purpose: asterism_secrets::SecretPurpose::ProviderCookie,
                required: true,
                sources: vec![CaptureValueSource::CookieHeader {
                    origin: "https://provider-alpha.example".to_owned(),
                }],
            }],
        }
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
        let (app, database, _) = test_app_with_events(secure_cookies, login_rate_limiter).await;
        (app, database)
    }

    async fn test_app_with_events(
        secure_cookies: bool,
        login_rate_limiter: Option<rate_limit::LoginRateLimiter>,
    ) -> (Router, Database, EventBus) {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let events = EventBus::new(16);
        let mut state = ApiState::new(
            database.clone(),
            Arc::new(ProviderRegistry::default()),
            3600,
            secure_cookies,
        )
        .with_event_bus(events.clone());
        if let Some(login_rate_limiter) = login_rate_limiter {
            state.login_rate_limiter = login_rate_limiter;
        }
        (build_router(state), database, events)
    }

    async fn test_router() -> Router {
        test_app(false, None).await.0
    }

    fn workspace_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    async fn uai_worker_test_router() -> Router {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let worker = UaiWorkerClient::new(
            "python",
            workspace_path("workers/uai/worker.py"),
            workspace_path("workers/uai/tests/fixtures/fake_upstream.py"),
        )
        .with_source_metadata(workspace_path(
            "workers/uai/tests/fixtures/fake_SOURCE.json",
        ));
        build_router(
            ApiState::new(database, Arc::new(ProviderRegistry::default()), 3600, false)
                .with_uai_worker(worker),
        )
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
            r#"{{"display_name":"primary","provider_id":"provider-alpha","auth_method":"assisted_session","session_kind":"cookie","fields":[{{"purpose":"provider_cookie","value":"{value}"}}]}}"#
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
            r#"{{"provider_id":"provider-alpha","auth_method":"assisted_session","session_kind":"cookie","fields":[{{"purpose":"provider_cookie","value":"{value}"}}]}}"#
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
        let app = test_router().await;
        let response = app
            .clone()
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
        assert!(!health.master_initialized);

        bootstrap(&app).await;
        let response = app
            .oneshot(
                Request::get("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert!(health.master_initialized);
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
    async fn uai_worker_health_is_protected_and_reports_the_pinned_source() {
        let app = uai_worker_test_router().await;
        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/providers/uai/worker/health")
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
        let response = app
            .oneshot(
                Request::get("/api/v1/providers/uai/worker/health")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let health: UaiWorkerHealth =
            serde_json::from_slice(&to_bytes(response.into_body(), 16 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(health.status, "ok");
        assert_eq!(health.source.revision, "fixture-revision");
    }

    #[tokio::test]
    async fn uai_worker_health_reports_unconfigured_without_affecting_service_health() {
        let app = test_router().await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/providers/uai/worker/health")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error: ErrorResponse =
            serde_json::from_slice(&to_bytes(response.into_body(), 16 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(error.error.code, "uai_worker_not_configured");

        let service_health = app
            .oneshot(
                Request::get("/api/v1/system/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(service_health.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn master_target_owner_header_controls_cross_user_account_scope() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created_user = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &master_cookie)
                    .body(Body::from(
                        r#"{"username":"delegated-user","password":"delegated-password","roles":["user"],"permissions":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created_user.status(), StatusCode::CREATED);
        let user_id = response_json(created_user).await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let account = app
            .clone()
            .oneshot(
                Request::post("/api/v1/provider-accounts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &master_cookie)
                    .body(Body::from(format!(
                        r#"{{"provider_id":"provider-alpha","display_name":"delegated","owner_user_id":"{user_id}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(account.status(), StatusCode::CREATED);
        let account_id = response_json(account).await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let own_list = app
            .clone()
            .oneshot(
                Request::get("/api/v1/provider-accounts")
                    .header(header::COOKIE, &master_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response_json(own_list).await["total"], 0);
        let delegated_list = app
            .clone()
            .oneshot(
                Request::get("/api/v1/provider-accounts")
                    .header(header::COOKIE, &master_cookie)
                    .header("x-asterism-target-owner", &user_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let delegated_list = response_json(delegated_list).await;
        assert_eq!(delegated_list["total"], 1);

        let task_id = TaskId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'delegated-task', 'delegated-task-fingerprint', 'work', \
                     'routine', 'Delegated task', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(&account_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let ignored = app
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/tasks/{task_id}/ignore"))
                    .header(header::COOKIE, &master_cookie)
                    .header("x-asterism-target-owner", &user_id)
                    .header("idempotency-key", "delegated-ignore")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ignored.status(), StatusCode::CREATED);
        assert_eq!(response_json(ignored).await["task_state"], "ignored");
        let persisted_state: String =
            sqlx::query_scalar("SELECT orchestration_state FROM tasks WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(persisted_state, "ignored");
        let master_id: String =
            sqlx::query_scalar("SELECT id FROM users WHERE username = 'master'")
                .fetch_one(database.pool())
                .await
                .unwrap();
        let audit_actor: String = sqlx::query_scalar(
            "SELECT actor_id FROM audit_records \
             WHERE resource_id = ? AND action = 'task_ignore'",
        )
        .bind(task_id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_actor, master_id);
        assert_ne!(audit_actor, user_id);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the account integration keeps CRUD, audit, owner scope and the derived health surface in one fixture"
    )]
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

        let health = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/provider-accounts/{account_id}/health"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(health.headers()[header::CACHE_CONTROL], "no-store");
        let health = response_json(health).await;
        assert_eq!(health["provider_account_id"], account_id);
        assert_eq!(health["state"], "human_action_required");
        assert_eq!(health["human_required_reason"], "auth_required");
        assert!(health["protocol_drift_execution_id"].is_null());

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
    async fn provider_manager_can_create_an_account_for_an_active_user() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();

        let user = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"username":"managed-user","password":"managed-user-password","roles":["user"],"permissions":["manage_own_accounts"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(user.status(), StatusCode::CREATED);
        let user = response_json(user).await;
        let user_id = user["id"].as_str().unwrap();

        let created = app
            .oneshot(
                Request::post("/api/v1/provider-accounts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie)
                    .body(Body::from(format!(
                        r#"{{"provider_id":"provider-alpha","display_name":"managed","owner_user_id":"{user_id}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let account = response_json(created).await;
        let stored_owner: String =
            sqlx::query_scalar("SELECT owner_user_id FROM provider_accounts WHERE id = ?")
                .bind(account["id"].as_str().unwrap())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(stored_owner, user_id);
    }

    #[tokio::test]
    async fn protocol_observation_inbox_is_master_only_filtered_and_sanitized() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let shape = json!({"missing_fields": ["resultCode"], "version": 3});
        SqliteProtocolObservationRepository::new(database)
            .record_protocol_observation(ProtocolObservationRecordRequest {
                provider_id: ProviderId::new("provider-alpha").unwrap(),
                surface: ProtocolSurface::SubmissionVerify,
                kind: ProtocolObservationKind::FieldDrift,
                shape_sanitized: &shape,
                occurrence_digest: [7; 32],
                execution_id: None,
                observed_at: Utc::now(),
            })
            .await
            .unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::get(
                    "/api/v1/admin/protocol-observations?provider_id=provider-alpha&kind=field_drift&limit=10&offset=0",
                )
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let response = response_json(response).await;
        assert_eq!(response["total"], 1);
        assert_eq!(response["items"][0]["provider_id"], "provider-alpha");
        assert_eq!(response["items"][0]["surface"], "submission_verify");
        assert_eq!(response["items"][0]["kind"], "field_drift");
        assert_eq!(response["items"][0]["shape_sanitized"], shape);
        assert_eq!(response["items"][0]["occurrence_count"], 1);

        let invalid = app
            .oneshot(
                Request::get("/api/v1/admin/protocol-observations?kind=not-real")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(invalid).await["error"]["code"],
            "invalid_protocol_observation_kind"
        );
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
        assert_eq!(claimed["recipe"]["version"], 3);
        assert_eq!(claimed["recipe"]["auth_method"], "assisted_session");
        assert_eq!(claimed["recipe"]["session_kind"], "cookie");
        assert_eq!(
            claimed["recipe"]["outputs"][0]["purpose"],
            "provider_cookie"
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
    async fn capture_recipe_alternatives_are_discoverable_and_frozen_by_version() {
        let (app, _) = credential_test_app(true).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let recipes = app
            .clone()
            .oneshot(
                Request::get("/api/v1/providers/provider-alpha/capture-recipes")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(recipes.status(), StatusCode::OK);
        let recipes = response_json(recipes).await;
        assert_eq!(recipes["total"], 2);
        assert_eq!(recipes["items"][0]["version"], 3);
        assert_eq!(recipes["items"][1]["version"], 4);

        let selected = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth-bootstrap/sessions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"provider_id":"provider-alpha","purpose":"add_account","recipe_version":4}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(selected.status(), StatusCode::CREATED);
        assert_eq!(
            response_json(selected).await["session"]["required_recipe_version"],
            4
        );
    }

    #[tokio::test]
    async fn completion_workflows_are_owner_scoped_read_only_snapshots() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let task_id = TaskId::new();
        let now = Utc::now();
        let now_text = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'completion-status-task', 'completion-status-fingerprint', 'work', \
                     'routine', 'Completion status', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(&account_id)
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        let path = format!("/api/v1/tasks/{task_id}/completion-workflows");
        let absent = app
            .clone()
            .oneshot(
                Request::get(&path)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(absent.status(), StatusCode::OK);
        assert_eq!(absent.headers()[header::CACHE_CONTROL], "no-store");
        let absent = response_json(absent).await;
        assert!(absent["strict_completion"].is_null());
        assert!(absent["score_improvement"].is_null());

        let owner_id: UserId = sqlx::query_scalar::<_, String>(
            "SELECT owner_user_id FROM provider_accounts WHERE id = ?",
        )
        .bind(&account_id)
        .fetch_one(database.pool())
        .await
        .unwrap()
        .parse()
        .unwrap();
        let workflow = StrictCompletionWorkflow::new(
            CompletionWorkflowBinding {
                owner_user_id: owner_id,
                provider_account_id: ProviderAccountId::from_str(&account_id).unwrap(),
                task_id,
            },
            CompletionPolicySnapshot {
                captured_at: now,
                ..CompletionPolicySnapshot::default()
            },
            None,
            now,
        )
        .unwrap();
        SqliteCompletionWorkflowRepository::new(database)
            .create_strict_completion_workflow(&workflow)
            .await
            .unwrap();

        let present = app
            .oneshot(
                Request::get(path)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(present.status(), StatusCode::OK);
        let present = response_json(present).await;
        assert_eq!(present["task_id"], task_id.to_string());
        assert_eq!(present["strict_completion"]["revision"], 1);
        assert_eq!(present["strict_completion"]["workflow"]["state"], "active");
        assert_eq!(
            present["strict_completion"]["workflow"]["binding"]["owner_user_id"],
            owner_id.to_string()
        );
        assert!(present["score_improvement"].is_null());
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the API test assembles the verified completion and exact history authority before exercising the owner opt-in"
    )]
    async fn score_improvement_opt_in_is_explicit_result_bound_and_idempotent() {
        let (app, database) = settings_test_app().await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let account_id = ProviderAccountId::from_str(&account_id).unwrap();
        let owner_id: UserId = sqlx::query_scalar::<_, String>(
            "SELECT owner_user_id FROM provider_accounts WHERE id = ?",
        )
        .bind(account_id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap()
        .parse()
        .unwrap();
        let task_id = TaskId::new();
        let now = Utc::now();
        let created_at = now - ChronoDuration::seconds(4);
        let timestamp = created_at.to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'score-opt-in-task', 'score-opt-in-fingerprint', 'work', \
                     'formal', 'Score opt-in', 'completed', 'succeeded', ?, ?, '[\"resource_execution\",\"execution_verify\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        let mut strict = StrictCompletionWorkflow::new(
            CompletionWorkflowBinding {
                owner_user_id: owner_id,
                provider_account_id: account_id,
                task_id,
            },
            CompletionPolicySnapshot {
                captured_at: created_at,
                score_target_millis: 900,
                score_improvement_attempt_limit: 2,
                score_improvement_expires_at: Some(now + ChronoDuration::hours(2)),
                ..CompletionPolicySnapshot::default()
            },
            None,
            created_at,
        )
        .unwrap();
        strict
            .observe_verified_completion(
                CompletionOutcome::Passed,
                now - ChronoDuration::seconds(3),
            )
            .unwrap();
        SqliteCompletionWorkflowRepository::new(database.clone())
            .create_strict_completion_workflow(&strict)
            .await
            .unwrap();

        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some("score-opt-in-question".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Select one".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            position: 1,
        };
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_version: "score-opt-in-v1".to_owned(),
            captured_at: now - ChronoDuration::seconds(2),
            questions: vec![question],
            groups: Vec::new(),
        };
        let retake = AnswerHistoryRetakeFacts {
            allowed: true,
            remaining_attempts: Some(1),
            closes_at: Some(now + ChronoDuration::hours(1)),
            score_policy: RetakeScorePolicy::HighestScore,
            metadata_sanitized: serde_json::json!({"entry": "redo"}),
        };
        let provenance = serde_json::json!({"surface": "result"});
        SqliteAnswerHistoryIngestionRepository::new(database.clone())
            .ingest_answer_history_task(AnswerHistoryIngestRequest {
                owner_user_id: owner_id,
                provider_account_id: account_id,
                provider_attempt_digest: [11; 32],
                result_digest: [12; 32],
                snapshot: &snapshot,
                candidates: &[],
                evidence: &[],
                score: Some(SubmissionScore {
                    earned_milli_points: 80,
                    possible_milli_points: 100,
                }),
                retake: Some(&retake),
                provenance_sanitized: &provenance,
                observed_at: now - ChronoDuration::seconds(1),
                imported_at: now - ChronoDuration::seconds(1),
            })
            .await
            .unwrap();

        let path = format!("/api/v1/tasks/{task_id}/completion-workflows/score-improvement");
        let denied = app
            .clone()
            .oneshot(
                Request::post(&path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(r#"{"explicitly_opted_in":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::BAD_REQUEST);

        let post = || {
            app.clone().oneshot(
                Request::post(&path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(r#"{"explicitly_opted_in":true}"#))
                    .unwrap(),
            )
        };
        let created = post().await.unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let created = response_json(created).await;
        assert_eq!(created["created"], true);
        assert_eq!(created["workflow"]["state"], "ready");
        assert_eq!(
            created["workflow"]["policy"]["score_improvement_attempt_limit"],
            1
        );
        assert_eq!(
            created["workflow"]["retake_authority"]["result_digest"],
            serde_json::json!(vec![12; 32])
        );
        let workflow_id = created["workflow"]["id"].clone();

        let replay = post().await.unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        let replay = response_json(replay).await;
        assert_eq!(replay["created"], false);
        assert_eq!(replay["workflow"]["id"], workflow_id);
        let workflow_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM score_improvement_workflows")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(workflow_count, 1);

        let execute = |idempotency_key: &'static str| {
            let app = app.clone();
            let cookie = cookie.clone();
            let workflow_id = workflow_id.clone();
            async move {
                app.oneshot(
                    Request::post(format!("/api/v1/tasks/{task_id}/execute"))
                        .header(header::COOKIE, cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("x-request-id", format!("request-{idempotency_key}"))
                        .header("idempotency-key", idempotency_key)
                        .body(Body::from(
                            serde_json::json!({
                                "requested_capabilities": ["resource_execution"],
                                "score_improvement_retake_confirmation": {
                                    "workflow_id": workflow_id,
                                    "expected_revision": 1,
                                }
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };
        let before_remote_retake = execute("score-retake-before-remote").await;
        assert_eq!(before_remote_retake.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(before_remote_retake).await["error"]["code"],
            "score_improvement_retake_conflict"
        );
        sqlx::query("UPDATE tasks SET remote_state = 'pending' WHERE id = ?")
            .bind(task_id.to_string())
            .execute(database.pool())
            .await
            .unwrap();
        let scheduled = execute("score-retake").await;
        let scheduled_status = scheduled.status();
        let scheduled = response_json(scheduled).await;
        assert_eq!(scheduled_status, StatusCode::CREATED, "{scheduled}");
        let execution_id = scheduled["execution"]["id"].as_str().unwrap();
        let replay = execute("score-retake").await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(response_json(replay).await["execution"]["id"], execution_id);
        let state: (String, i64) =
            sqlx::query_as("SELECT state, revision FROM score_improvement_workflows WHERE id = ?")
                .bind(workflow_id.as_str().unwrap())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(state, ("attempt_running".to_owned(), 2));
        let confirmation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM execution_score_improvement_retake_confirmations WHERE execution_id = ?",
        )
        .bind(execution_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(confirmation_count, 1);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test follows Provider, account and Task settings as one hierarchy"
    )]
    async fn master_runtime_settings_are_revisioned_and_resolved_by_scope() {
        let (app, database) = settings_test_app().await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let schema = app
            .clone()
            .oneshot(
                Request::get("/api/v1/admin/providers/provider-alpha/runtime-settings/schema")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(schema.status(), StatusCode::OK);
        let schema = response_json(schema).await;
        assert_eq!(schema["schema"]["version"], 2);
        assert_eq!(
            schema["schema"]["definitions"][0]["key"],
            "execution.max_concurrency"
        );

        let provider_path = "/api/v1/admin/providers/provider-alpha/runtime-settings";
        let invalid = app
            .clone()
            .oneshot(
                Request::put(provider_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"expected_revision":0,"schema_version":2,"values":{"execution.max_concurrency":{"type":"integer","value":9}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let provider = app
            .clone()
            .oneshot(
                Request::put(provider_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "settings-provider-create")
                    .body(Body::from(
                        r#"{"expected_revision":0,"schema_version":2,"values":{"execution.max_concurrency":{"type":"integer","value":4}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(provider.status(), StatusCode::CREATED);
        let provider = response_json(provider).await;
        assert_eq!(provider["overrides"]["provider"]["revision"], 1);
        assert_eq!(
            provider["resolved"]["values"]["execution.max_concurrency"]["value"],
            4
        );
        assert_eq!(provider["sources"]["execution.max_concurrency"], "provider");

        let stale = app
            .clone()
            .oneshot(
                Request::put(provider_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"expected_revision":0,"schema_version":2,"values":{"execution.max_concurrency":{"type":"integer","value":5}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);

        let account_id = create_test_provider_account(&app, &cookie).await;
        let account_path = format!("/api/v1/admin/provider-accounts/{account_id}/runtime-settings");
        let account = app
            .clone()
            .oneshot(
                Request::put(&account_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"expected_revision":0,"schema_version":2,"values":{"execution.max_concurrency":{"type":"integer","value":3}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(account.status(), StatusCode::CREATED);
        let account = response_json(account).await;
        assert_eq!(
            account["resolved"]["values"]["execution.max_concurrency"]["value"],
            3
        );
        assert_eq!(
            account["sources"]["execution.max_concurrency"],
            "provider_account"
        );

        let task_id = asterism_domain::TaskId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'settings-api-task', 'settings-api-fingerprint', 'chapter', \
                     'routine', 'Settings API task', 'pending', 'ready', ?, ?, \
                     '[\"resource_execution\"]')",
        )
        .bind(task_id.to_string())
        .bind(&account_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let task_path = format!("/api/v1/admin/tasks/{task_id}/runtime-settings");
        let task = app
            .clone()
            .oneshot(
                Request::put(&task_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"expected_revision":0,"schema_version":2,"values":{"execution.max_concurrency":{"type":"integer","value":1},"core.strict_completion.attempt_limit":{"type":"integer","value":5}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(task.status(), StatusCode::CREATED);
        let task = response_json(task).await;
        assert_eq!(task["target_scope"], "task");
        assert_eq!(
            task["resolved"]["values"]["execution.max_concurrency"]["value"],
            1
        );
        assert_eq!(
            task["resolved"]["values"]["core.strict_completion.attempt_limit"]["value"],
            5
        );
        assert_eq!(task["sources"]["execution.max_concurrency"], "task");
        assert_eq!(task["overrides"]["provider"]["revision"], 1);
        assert_eq!(task["overrides"]["provider_account"]["revision"], 1);
        assert_eq!(task["overrides"]["task"]["revision"], 1);

        let fetched = app
            .clone()
            .oneshot(
                Request::get(task_path)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);
        assert_eq!(response_json(fetched).await, task);

        let execution =
            post_task_execution(&app, &cookie, task_id, Some("settings-task-execution")).await;
        assert_eq!(execution.status(), StatusCode::CREATED);
        let execution = response_json(execution).await;
        let execution_id = execution["execution"]["id"].as_str().unwrap();
        let frozen: (
            String,
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT resolved_settings_json, sources_json, completion_policy_json, \
                    provider_revision, provider_account_revision, task_revision \
             FROM execution_runtime_settings WHERE execution_id = ?",
        )
        .bind(execution_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(frozen.0.contains("\"value\":1"));
        assert!(frozen.1.contains("\"task\""));
        let completion_policy: asterism_domain::CompletionPolicySnapshot =
            serde_json::from_str(&frozen.2).unwrap();
        assert_eq!(completion_policy.strict_attempt_limit, 5);
        assert!(completion_policy.strict_completion_enabled);
        assert!(completion_policy.score_improvement_enabled);
        completion_policy.validate().unwrap();
        assert_eq!((frozen.3, frozen.4, frozen.5), (Some(1), Some(1), Some(1)));

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records \
             WHERE action = 'provider_runtime_settings_configured'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 3);
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
    async fn interactive_auth_poll_api_commits_without_exposing_continuation() {
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
                .body(Body::from(r#"{"method":"qr_code"}"#))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(started.status(), StatusCode::CREATED);
        assert_eq!(started.headers()[header::CACHE_CONTROL], "no-store");
        let started = response_json(started).await;
        assert_eq!(started["challenge"]["waiting_for"], "qr_scan");
        let session_id = started["session"]["id"].as_str().unwrap();
        let response = app
            .oneshot(
                Request::post(format!(
                    "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/poll"
                ))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        for secret in [b"api-qr-state".as_slice(), b"api-qr-cookie".as_slice()] {
            assert!(
                !body
                    .as_ref()
                    .windows(secret.len())
                    .any(|window| window == secret)
            );
        }
        let response: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["session"]["state"]["state"], "authenticated");
        assert_eq!(response["credential_count"], 1);
        assert_eq!(response["status"]["valid"], true);
        let continuation_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM interactive_auth_continuations")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(continuation_count, 0);
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
    #[allow(
        clippy::too_many_lines,
        reason = "the scan integration keeps authentication, inventory persistence and immediate Course aggregate visibility together"
    )]
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
            .clone()
            .oneshot(
                Request::post(scan_path)
                    .header(header::COOKIE, &cookie)
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

        let course_id: String =
            sqlx::query_scalar("SELECT id FROM courses WHERE provider_account_id = ?")
                .bind(account_id)
                .fetch_one(database.pool())
                .await
                .unwrap();
        let courses = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/courses?provider_account_id={account_id}&limit=10&offset=0"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(courses.status(), StatusCode::OK);
        assert_eq!(courses.headers()[header::CACHE_CONTROL], "no-store");
        let courses = response_json(courses).await;
        assert_eq!(courses["total"], 1);
        assert_eq!(courses["items"][0]["id"], course_id);

        let course = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/courses/{course_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(course.status(), StatusCode::OK);
        assert_eq!(response_json(course).await["id"], course_id);

        let progress = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/courses/{course_id}/progress"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(progress.status(), StatusCode::OK);
        assert_eq!(progress.headers()[header::CACHE_CONTROL], "no-store");
        let progress = response_json(progress).await;
        assert_eq!(progress["course"]["id"], course_id);
        assert_eq!(progress["progress"]["total_task_count"], 1);
        assert_eq!(progress["progress"]["countable_task_count"], 1);
        assert_eq!(progress["progress"]["completed_task_count"], 0);
        assert_eq!(progress["progress"]["remaining_task_count"], 1);
        assert_eq!(progress["progress"]["completion_millis"], 0);
        assert!(progress["progress"]["required"].is_null());
        assert!(progress["progress"]["duration"].is_null());
        assert!(progress["progress"]["score"].is_null());

        let automation_path = format!("/api/v1/courses/{course_id}/automation");
        let automation = app
            .clone()
            .oneshot(
                Request::get(&automation_path)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(automation.status(), StatusCode::OK);
        assert_eq!(response_json(automation).await["enabled"], false);
        let automation = app
            .clone()
            .oneshot(
                Request::put(&automation_path)
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"enabled":true,"ai_profile":"gpt_only"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(automation.status(), StatusCode::OK);
        let automation_json = response_json(automation).await;
        assert_eq!(automation_json["enabled"], true);
        assert_eq!(automation_json["ai_profile"], "gpt_only");
        let automation = app
            .clone()
            .oneshot(
                Request::put(&automation_path)
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"enabled":true,"ai_profile":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(automation.status(), StatusCode::OK);
        assert!(response_json(automation).await["ai_profile"].is_null());

        let foreign = app
            .oneshot(
                Request::get(format!(
                    "/api/v1/courses/{}/progress",
                    asterism_domain::CourseId::new()
                ))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

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
    #[allow(
        clippy::too_many_lines,
        reason = "one helper session must prove token rotation, replay rejection and revocation together"
    )]
    async fn browser_bridge_pairing_rotates_once_and_cancellation_revokes_access() {
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
        let app = build_router(
            ApiState::new(database.clone(), Arc::new(scan_registry()), 3600, false)
                .with_secret_store(secret_store.clone()),
        );
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        sqlx::query("UPDATE provider_accounts SET auth_state_json = ? WHERE id = ?")
            .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
            .bind(&account_id)
            .execute(database.pool())
            .await
            .unwrap();
        let scanned = app
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/provider-accounts/{account_id}/scan"))
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "browser-bridge-scan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(scanned.status(), StatusCode::OK);
        let task_id: String = sqlx::query_scalar("SELECT id FROM tasks WHERE remote_id = 'task-a'")
            .fetch_one(database.pool())
            .await
            .unwrap();

        let created = app
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/tasks/{task_id}/browser-bridge/sessions"))
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "browser-bridge-create")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let created = response_json(created).await;
        let session_id = created["session"]["id"].as_str().unwrap().to_owned();
        let pairing_token = created["pairing_token"].as_str().unwrap().to_owned();
        assert!(pairing_token.starts_with("ast_bridge_pair_"));
        assert_eq!(created["session"]["task_id"], task_id);
        assert_eq!(created["session"]["state"], "awaiting_claim");
        assert_eq!(
            created["spec"]["allowed_origins"][0],
            "https://provider-alpha.example"
        );

        let claim_path = format!("/api/v1/browser-bridge/sessions/{session_id}/claim");
        let claimed = app
            .clone()
            .oneshot(
                Request::post(&claim_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {pairing_token}"),
                    )
                    .header("x-request-id", "browser-bridge-claim")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42_001))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK);
        assert_eq!(claimed.headers()[header::CACHE_CONTROL], "no-store");
        let claimed = response_json(claimed).await;
        let access_token = claimed["access_token"].as_str().unwrap().to_owned();
        assert!(access_token.starts_with("ast_bridge_"));
        assert_eq!(claimed["session"]["state"], "claimed");

        let replayed = app
            .clone()
            .oneshot(
                Request::post(&claim_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {pairing_token}"),
                    )
                    .header("x-request-id", "browser-bridge-replay")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42_002))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);

        let snapshot_path = format!("/api/v1/browser-bridge/sessions/{session_id}/snapshot");
        let snapshot = app
            .clone()
            .oneshot(
                Request::get(&snapshot_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status(), StatusCode::OK);
        assert_eq!(response_json(snapshot).await["session"]["id"], session_id);

        let binding_path = format!("/api/v1/browser-bridge/sessions/{session_id}/binding");
        let binding_body = json!({
            "observed_origin": "https://provider-alpha.example",
            "frame_id": "top-frame:1"
        });
        let bound = app
            .clone()
            .oneshot(
                Request::put(&binding_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "browser-bridge-runtime-bind")
                    .body(Body::from(binding_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bound.status(), StatusCode::OK);
        let bound = response_json(bound).await;
        assert_eq!(bound["session_id"], session_id);
        assert_eq!(bound["duplicate"], false);

        let duplicate_binding = app
            .clone()
            .oneshot(
                Request::put(&binding_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "browser-bridge-runtime-duplicate")
                    .body(Body::from(binding_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(duplicate_binding.status(), StatusCode::OK);
        let duplicate_binding = response_json(duplicate_binding).await;
        assert_eq!(duplicate_binding["duplicate"], true);
        assert_eq!(duplicate_binding["bound_at"], bound["bound_at"]);

        let conflicting_binding = app
            .clone()
            .oneshot(
                Request::put(&binding_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "browser-bridge-runtime-conflict")
                    .body(Body::from(
                        json!({
                            "observed_origin": "https://provider-alpha.example",
                            "frame_id": "foreign-frame"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflicting_binding.status(), StatusCode::CONFLICT);

        let command = br#"{"version":1,"kind":"capture_snapshot"}"#;
        let runtime_state = br#"{"cursor":"provider-private"}"#;
        let issued_at = Utc::now();
        let exchange = BrowserBridgeExchange::issue(
            BrowserBridgeSessionId::from_str(&session_id).unwrap(),
            1,
            "provider-alpha.capture.snapshot".to_owned(),
            sha2::Sha256::digest(command).into(),
            issued_at,
        )
        .unwrap();
        asterism_engine::BrowserBridgeCommandService::new(
            secret_store.browser_bridge_commands(ProviderId::new("provider-alpha").unwrap()),
        )
        .issue(asterism_engine::BrowserBridgeCommandIssueRequest {
            exchange,
            command_artifact: SecretValue::new(command.to_vec()),
            runtime_state: Some(asterism_engine::BrowserBridgeRuntimeStateIssue {
                metadata: BrowserBridgeRuntimeStateMetadata {
                    session_id: BrowserBridgeSessionId::from_str(&session_id).unwrap(),
                    sequence: 1,
                    state_type: "provider-alpha.runtime.cursor.v1".to_owned(),
                    state_digest: sha2::Sha256::digest(runtime_state).into(),
                    stored_at: issued_at,
                },
                state_artifact: SecretValue::new(runtime_state.to_vec()),
            }),
            workflow_context: None,
            access: SecretAccess {
                actor: SecretActor::CoreService("browser-bridge-api-test"),
                correlation_id: "browser-bridge-command-issue".to_owned(),
                reason: "issue API transport fixture".to_owned(),
            },
        })
        .await
        .unwrap();

        let owner_id = UserId::from_str(
            &sqlx::query_scalar::<_, String>("SELECT id FROM users LIMIT 1")
                .fetch_one(database.pool())
                .await
                .unwrap(),
        )
        .unwrap();
        let recovered = asterism_engine::BrowserBridgeRuntimeRecoveryService::new(
            SqliteBrowserBridgeSessionRepository::new(database.clone()),
            secret_store.browser_bridge_commands(ProviderId::new("provider-alpha").unwrap()),
        )
        .recover(asterism_engine::BrowserBridgeRuntimeRecoveryRequest {
            owner_user_id: owner_id,
            session_id: BrowserBridgeSessionId::from_str(&session_id).unwrap(),
            access: SecretAccess {
                actor: SecretActor::CoreService("browser-bridge-recovery-test"),
                correlation_id: "browser-bridge-runtime-recover".to_owned(),
                reason: "recover API transport fixture".to_owned(),
            },
        })
        .await
        .unwrap();
        assert_eq!(recovered.binding.frame_id, "top-frame:1");
        let recovered = recovered.latest.unwrap();
        assert_eq!(recovered.command.command_artifact.expose_secret(), command);
        assert_eq!(
            recovered
                .command
                .runtime_state
                .unwrap()
                .state_artifact
                .expose_secret(),
            runtime_state
        );
        assert!(recovered.result.is_none());

        let command_path = format!("/api/v1/browser-bridge/sessions/{session_id}/commands/1");
        let dispatched = app
            .clone()
            .oneshot(
                Request::get(&command_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header("x-request-id", "browser-bridge-command-dispatch")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dispatched.status(), StatusCode::OK);
        assert_eq!(dispatched.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            dispatched.headers()["x-asterism-browser-command-type"],
            "provider-alpha.capture.snapshot"
        );
        assert_eq!(
            to_bytes(dispatched.into_body(), 256 * 1024).await.unwrap(),
            command.as_slice()
        );
        let replayed_command = app
            .clone()
            .oneshot(
                Request::get(&command_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header("x-request-id", "browser-bridge-command-replay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replayed_command.status(), StatusCode::CONFLICT);

        let result_path = format!("{command_path}/result");
        let raw_result = br#"{"version":1,"kind":"capture_result"}"#;
        let accepted = app
            .clone()
            .oneshot(
                Request::post(&result_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(
                        "x-asterism-browser-result-type",
                        "provider-alpha.capture.snapshot.result",
                    )
                    .header("x-request-id", "browser-bridge-result-receive")
                    .body(Body::from(raw_result.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        let receipt = response_json(accepted).await;
        assert_eq!(receipt["sequence"], 1);
        assert_eq!(receipt["duplicate"], false);
        assert_eq!(receipt["result_digest"].as_str().unwrap().len(), 64);

        let recovered = asterism_engine::BrowserBridgeRuntimeRecoveryService::new(
            SqliteBrowserBridgeSessionRepository::new(database.clone()),
            secret_store.browser_bridge_commands(ProviderId::new("provider-alpha").unwrap()),
        )
        .recover(asterism_engine::BrowserBridgeRuntimeRecoveryRequest {
            owner_user_id: owner_id,
            session_id: BrowserBridgeSessionId::from_str(&session_id).unwrap(),
            access: SecretAccess {
                actor: SecretActor::CoreService("browser-bridge-result-recovery-test"),
                correlation_id: "browser-bridge-result-recover".to_owned(),
                reason: "recover received API result fixture".to_owned(),
            },
        })
        .await
        .unwrap()
        .latest
        .unwrap();
        assert_eq!(
            recovered.result.unwrap().result_artifact.expose_secret(),
            raw_result
        );

        let duplicate = app
            .clone()
            .oneshot(
                Request::post(&result_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(
                        "x-asterism-browser-result-type",
                        "provider-alpha.capture.snapshot.result",
                    )
                    .header("x-request-id", "browser-bridge-result-duplicate")
                    .body(Body::from(raw_result.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(response_json(duplicate).await["duplicate"], true);

        let oversized = app
            .clone()
            .oneshot(
                Request::post(&result_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(
                        "x-asterism-browser-result-type",
                        "provider-alpha.capture.snapshot.result",
                    )
                    .header("x-request-id", "browser-bridge-result-oversized")
                    .body(Body::from(vec![
                        0;
                        browser_bridge::MAX_BROWSER_BRIDGE_ARTIFACT_BYTES
                            + 1
                    ]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let conflicting = app
            .clone()
            .oneshot(
                Request::post(&result_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(
                        "x-asterism-browser-result-type",
                        "provider-alpha.capture.snapshot.result",
                    )
                    .header("x-request-id", "browser-bridge-result-conflict")
                    .body(Body::from(br#"{"version":1,"kind":"foreign"}"#.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflicting.status(), StatusCode::CONFLICT);

        let cancelled = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/browser-bridge/sessions/{session_id}"))
                    .header(header::COOKIE, cookie)
                    .header("x-request-id", "browser-bridge-cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(
            response_json(cancelled).await["session"]["state"],
            "cancelled"
        );

        let revoked = app
            .oneshot(
                Request::get(snapshot_path)
                    .header(
                        header::AUTHORIZATION,
                        format!("BrowserBridge {access_token}"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test keeps one complete runtime-default schedule lifecycle"
    )]
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

        let provider_settings = app
            .clone()
            .oneshot(
                Request::put("/api/v1/admin/providers/provider-alpha/runtime-settings")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "schedule-api-provider-settings")
                    .body(Body::from(
                        r#"{"expected_revision":0,"schema_version":1,"values":{"discovery.scan_interval":{"type":"duration_seconds","value":3600}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(provider_settings.status(), StatusCode::CREATED);

        let provider_default = app
            .clone()
            .oneshot(
                Request::put(&schedule_path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .header("x-request-id", "schedule-api-provider-default")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(provider_default.status(), StatusCode::OK);
        let provider_default = response_json(provider_default).await;
        assert_eq!(provider_default["id"], schedule_id);
        assert_eq!(provider_default["desired_interval_seconds"], 3_600);
        assert_eq!(provider_default["effective_interval_seconds"], 3_600);

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
        assert_eq!(audit_count, 3);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test keeps one complete owner-scoped credit history fixture"
    )]
    async fn credit_api_is_owner_scoped_paginated_and_preserves_quote_history() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let bootstrap_body = response_json(bootstrap).await;
        let owner_id = bootstrap_body["user"]["id"].as_str().unwrap();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let task_id = asterism_domain::TaskId::new();
        let quote_id = asterism_domain::PriceQuoteId::new();
        let execution_id = asterism_domain::ExecutionId::new();
        let reservation_id = asterism_domain::CreditReservationId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'credit-task', 'credit-fingerprint', 'work', 'routine', \
                     'Credit Task', 'completed', 'succeeded', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO price_quotes (id, task_id, amount, pricing_revision, reason, created_at) \
             VALUES (?, ?, 30, 'catalog-v1', 'fixed work price', ?)",
        )
        .bind(quote_id.to_string())
        .bind(task_id.to_string())
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_by, request_source, quote_id, state, created_at, finished_at) \
             VALUES (?, ?, ?, 'web_ui', ?, 'succeeded', ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(owner_id)
        .bind(quote_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE credit_accounts SET available = 70, reserved = 0, updated_at = ? \
             WHERE user_id = ?",
        )
        .bind(&now)
        .bind(owner_id)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO credit_reservations \
             (id, user_id, quote_id, execution_id, amount, state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 30, 'committed', ?, ?)",
        )
        .bind(reservation_id.to_string())
        .bind(owner_id)
        .bind(quote_id.to_string())
        .bind(execution_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        for (amount, kind, transaction_task, transaction_execution, reason) in [
            (100_i64, "master_grant", None, None, "initial grant"),
            (
                -30,
                "task_execution",
                Some(task_id.to_string()),
                Some(execution_id.to_string()),
                "execution succeeded",
            ),
        ] {
            sqlx::query(
                "INSERT INTO credit_transactions \
                 (id, user_id, amount, transaction_type, task_id, execution_id, reason, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(asterism_domain::CreditTransactionId::new().to_string())
            .bind(owner_id)
            .bind(amount)
            .bind(kind)
            .bind(transaction_task)
            .bind(transaction_execution)
            .bind(reason)
            .bind(&now)
            .execute(database.pool())
            .await
            .unwrap();
        }

        let account = app
            .clone()
            .oneshot(
                Request::get("/api/v1/credits/account")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(account.status(), StatusCode::OK);
        assert_eq!(account.headers()[header::CACHE_CONTROL], "no-store");
        let account = response_json(account).await;
        assert_eq!(account["available"], 70);
        assert_eq!(account["reserved"], 0);

        let transactions = app
            .clone()
            .oneshot(
                Request::get("/api/v1/credits/transactions?limit=1&offset=0")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(transactions.status(), StatusCode::OK);
        let transactions = response_json(transactions).await;
        assert_eq!(transactions["total"], 2);
        assert_eq!(transactions["limit"], 1);
        assert_eq!(transactions["items"].as_array().unwrap().len(), 1);

        let reservations = app
            .clone()
            .oneshot(
                Request::get("/api/v1/credits/reservations?limit=10&offset=0")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reservations.status(), StatusCode::OK);
        let reservations = response_json(reservations).await;
        assert_eq!(reservations["total"], 1);
        assert_eq!(
            reservations["items"][0]["reservation"]["state"],
            "committed"
        );
        assert_eq!(
            reservations["items"][0]["quote"]["id"],
            quote_id.to_string()
        );
        assert_eq!(
            reservations["items"][0]["quote"]["pricing_revision"],
            "catalog-v1"
        );

        let invalid = app
            .oneshot(
                Request::get("/api/v1/credits/transactions?limit=0")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
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
        let course_id = asterism_domain::CourseId::new();
        let task_id = asterism_domain::TaskId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO courses \
             (id, provider_account_id, remote_id, title, metadata_json, last_seen_at) \
             VALUES (?, ?, 'remote-course', 'course', '{}', ?)",
        )
        .bind(course_id.to_string())
        .bind(account_id)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, course_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, ?, 'remote-task', 'fingerprint', 'exam', 'routine', 'weekly check', \
                     'pending', 'ready', ?, ?, '[\"progress_read\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id)
        .bind(course_id.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let snapshot_id = asterism_domain::TaskSnapshotId::new();
        sqlx::query(
            "INSERT INTO task_snapshots \
             (id, task_id, captured_at, provider_version, normalized_json, remote_raw_sanitized_json) \
             VALUES (?, ?, ?, 'fixture-v1', '{}', ?)",
        )
        .bind(snapshot_id.to_string())
        .bind(task_id.to_string())
        .bind(&now)
        .bind(r#"{"provider_summary":{"required":true,"finish_progress":75}}"#)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE tasks SET latest_snapshot_id = ? WHERE id = ?")
            .bind(snapshot_id.to_string())
            .bind(task_id.to_string())
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
        assert_eq!(listed["items"][0]["provider_summary"]["required"], true);
        assert_eq!(
            listed["items"][0]["provider_summary"]["finish_progress"],
            75
        );
        assert!(listed["items"][0].get("remote_fingerprint").is_none());

        let course_listed = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks?course_id={course_id}&limit=1&offset=0"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(course_listed.status(), StatusCode::OK);
        let course_listed = to_bytes(course_listed.into_body(), 16 * 1024)
            .await
            .unwrap();
        let course_listed: Value = serde_json::from_slice(&course_listed).unwrap();
        assert_eq!(course_listed["total"], 1);
        assert_eq!(course_listed["items"][0]["id"], task_id.to_string());

        let ambiguous = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks?provider_account_id={account_id}&course_id={course_id}"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ambiguous.status(), StatusCode::BAD_REQUEST);

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
        let fetched = to_bytes(fetched.into_body(), 16 * 1024).await.unwrap();
        let fetched: Value = serde_json::from_slice(&fetched).unwrap();
        assert_eq!(fetched["provider_summary"]["required"], true);

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
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test keeps the persisted Candidate and Draft ownership chain in one database fixture"
    )]
    async fn persisted_candidates_and_drafts_require_owner_task_and_snapshot_binding() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let account_id = create_test_provider_account(&app, &cookie).await;
        let task_id = TaskId::new();
        let now = Utc::now();
        let now_text = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'answer-task', 'fingerprint-answer', 'work', 'routine', \
                     'answer work', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(&account_id)
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        let question_id = QuestionId::new();
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            provider_version: "0.1.0-test".to_owned(),
            captured_at: now,
            questions: vec![Question {
                id: question_id,
                task_id,
                remote_question_id: Some("question-1".to_owned()),
                kind: QuestionKind::TrueFalse,
                stem: "A bounded fixture question".to_owned(),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
                position: 1,
            }],
            groups: Vec::new(),
        };
        let repository = SqliteQuestionSnapshotRepository::new(database.clone());
        let mut prior_question = snapshot.questions[0].clone();
        prior_question.id = QuestionId::new();
        prior_question.remote_question_id = Some("prior-question-1".to_owned());
        prior_question.position = 9;
        let prior_snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: snapshot.provider_id.clone(),
            provider_version: "0.1.0-test".to_owned(),
            captured_at: now - ChronoDuration::seconds(1),
            questions: vec![prior_question],
            groups: Vec::new(),
        };
        repository
            .save_question_snapshot(&prior_snapshot)
            .await
            .unwrap();
        repository
            .save_answer_candidate_batch(&[AnswerCandidateRecord {
                id: AnswerCandidateId::new(),
                question_snapshot_id: prior_snapshot.id,
                candidate: AnswerCandidate {
                    question_id: prior_snapshot.questions[0].id,
                    source: AnswerSource::ProviderNative,
                    answer: NormalizedAnswer::Boolean(true),
                    confidence: None,
                    explanation: None,
                    provenance_sanitized: json!({"resolver": "fixture"}),
                },
                created_at: prior_snapshot.captured_at,
            }])
            .await
            .unwrap();
        repository.save_question_snapshot(&snapshot).await.unwrap();
        let candidate_id = AnswerCandidateId::new();
        repository
            .save_answer_candidate_batch(&[AnswerCandidateRecord {
                id: candidate_id,
                question_snapshot_id: snapshot.id,
                candidate: AnswerCandidate {
                    question_id,
                    source: AnswerSource::Manual,
                    answer: NormalizedAnswer::Boolean(true),
                    confidence: None,
                    explanation: None,
                    provenance_sanitized: json!({"input": "fixture"}),
                },
                created_at: now,
            }])
            .await
            .unwrap();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: snapshot.id,
            provider_id: snapshot.provider_id.clone(),
            provider_version: "0.1.0-test".to_owned(),
            answer_coverage: asterism_domain::SubmissionAnswerCoverage {
                total_question_count: 1,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: vec![SubmissionDraftItem {
                question: snapshot.questions[0].clone(),
                selected: SelectedAnswer {
                    candidate_id,
                    question_id,
                    answer: NormalizedAnswer::Boolean(true),
                    source: AnswerSource::Manual,
                    confidence: None,
                },
            }],
            payload_preview: SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::Form,
                format: "provider-alpha.work.v1".to_owned(),
                fields: vec![SubmissionPayloadFieldPreview {
                    question_id,
                    field_name: "answer[question-1]".to_owned(),
                }],
            },
            created_at: now,
        };
        repository.save_submission_draft(&draft).await.unwrap();
        let execution_id = ExecutionId::new();
        let execution_attempt_id = ExecutionAttemptId::new();
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_capabilities_json, submission_draft_id, request_source, \
              state, started_at, created_at) \
             VALUES (?, ?, '[\"submission_execute\"]', ?, 'web_ui', 'running', ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(draft.id.to_string())
        .bind(&now_text)
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_attempts \
             (id, execution_id, attempt_no, started_at) VALUES (?, ?, 1, ?)",
        )
        .bind(execution_attempt_id.to_string())
        .bind(execution_id.to_string())
        .bind(&now_text)
        .execute(database.pool())
        .await
        .unwrap();
        let result = SubmissionResult {
            id: SubmissionResultId::new(),
            submission_draft_id: draft.id,
            execution_id,
            execution_attempt_id,
            task_id,
            question_snapshot_id: snapshot.id,
            provider_id: snapshot.provider_id.clone(),
            provider_version: "0.1.0-test".to_owned(),
            status: SubmissionResultStatus::Confirmed,
            receipt: Some(SubmissionReceipt {
                remote_status: "accepted".to_owned(),
                message_sanitized: None,
                provider_trace_id: Some("trace-api-test".to_owned()),
                received_at: now,
            }),
            verification: SubmissionVerificationSnapshot {
                status: SubmissionVerificationStatus::Confirmed,
                remote_state: Some(RemoteState::Completed),
                score: Some(SubmissionScore {
                    earned_milli_points: 1_000,
                    possible_milli_points: 1_000,
                }),
                progress_percent: Some(100),
                questions: vec![SubmissionQuestionVerification {
                    question_id,
                    status: SubmissionQuestionVerificationStatus::Confirmed,
                }],
                verified_at: now,
            },
            created_at: now,
        };
        repository.save_submission_result(&result).await.unwrap();

        let owner_id: UserId = sqlx::query_scalar::<_, String>(
            "SELECT owner_user_id FROM provider_accounts WHERE id = ?",
        )
        .bind(account_id)
        .fetch_one(database.pool())
        .await
        .unwrap()
        .parse()
        .unwrap();
        let execution_repository = SqliteExecutionRepository::new(database.clone());
        assert_eq!(
            execution_repository
                .list_owned_executions(owner_id, Some(task_id), 10, 0)
                .await
                .unwrap()
                .total,
            1
        );
        assert!(
            execution_repository
                .find_owned_execution_detail(owner_id, execution_id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            SqliteAnswerEvidenceRepository::new(database.clone())
                .count_owned_execution_attempt_evidence(owner_id, execution_attempt_id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            repository
                .find_previous_owned_submission_score(owner_id, task_id, result.id)
                .await
                .unwrap(),
            None
        );

        let history = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/attempt-history?limit=10&offset=0"
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let history_status = history.status();
        let history_headers = history.headers().clone();
        let history = response_json(history).await;
        assert_eq!(history_status, StatusCode::OK, "{history}");
        assert_eq!(history_headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(history["total"], 1);
        assert_eq!(
            history["items"][0]["execution"]["id"],
            execution_id.to_string()
        );
        assert_eq!(
            history["items"][0]["attempts"][0]["attempt"]["id"],
            execution_attempt_id.to_string()
        );
        assert_eq!(
            history["items"][0]["attempts"][0]["learned_evidence"]["verified_historical"],
            1
        );
        assert_eq!(
            history["items"][0]["submission"]["submission_draft_id"],
            draft.id.to_string()
        );
        assert_eq!(
            history["items"][0]["submission"]["answer_sources"]["manual"],
            1
        );
        assert_eq!(
            history["items"][0]["submission"]["result"]["submission_result_id"],
            result.id.to_string()
        );
        assert_eq!(
            history["items"][0]["submission"]["result"]["question_results"]["confirmed"],
            1
        );
        assert!(history["items"][0]["submission"]["result"]["previous_score"].is_null());
        assert!(history["items"][0]["submission"]["result"]["score_delta_millis"].is_null());

        let foreign_history = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/tasks/{}/attempt-history", TaskId::new()))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(foreign_history.status(), StatusCode::NOT_FOUND);

        let snapshot_response = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot_response.status(), StatusCode::OK);
        assert_eq!(
            snapshot_response.headers()[header::CACHE_CONTROL],
            "no-store"
        );
        let snapshot_body = response_json(snapshot_response).await;
        assert_eq!(snapshot_body["snapshot_id"], snapshot.id.to_string());
        assert_eq!(snapshot_body["questions"][0]["id"], question_id.to_string());

        let mismatched_snapshot = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{}/question-snapshots/{}",
                    TaskId::new(),
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatched_snapshot.status(), StatusCode::NOT_FOUND);

        let manual_response = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/answer-candidates",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "question_id": question_id,
                        "answer": {"type": "boolean", "value": false},
                        "confidence_basis_points": 7500,
                        "explanation": "Reviewed manually"
                    })
                    .to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(manual_response.status(), StatusCode::CREATED);
        assert_eq!(manual_response.headers()[header::CACHE_CONTROL], "no-store");
        let manual_body = response_json(manual_response).await;
        assert_eq!(manual_body["candidate"]["source"], "manual");
        assert_eq!(
            manual_body["candidate"]["provenance_sanitized"],
            json!({"origin": "manual_input"})
        );
        assert_eq!(manual_body["candidate"]["confidence"], 7500);

        let resolution_response = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/answer-resolution",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resolution_response.status(), StatusCode::OK);
        assert_eq!(
            resolution_response.headers()[header::CACHE_CONTROL],
            "no-store"
        );
        let resolution_body = response_json(resolution_response).await;
        assert_eq!(resolution_body["task_id"], task_id.to_string());
        assert_eq!(
            resolution_body["question_snapshot_id"],
            snapshot.id.to_string()
        );
        assert_eq!(resolution_body["decisions"][0]["status"], "conflict");
        assert_eq!(
            resolution_body["decisions"][0]["considered_candidate_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(resolution_body["decisions"][0]["selected_candidate_id"].is_null());

        let cache_response = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/answer-candidates/import-local-cache",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cache_response.status(), StatusCode::OK);
        assert_eq!(cache_response.headers()[header::CACHE_CONTROL], "no-store");
        let cache_body = response_json(cache_response).await;
        assert_eq!(cache_body["task_id"], task_id.to_string());
        assert_eq!(cache_body["question_snapshot_id"], snapshot.id.to_string());
        assert_eq!(cache_body["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(
            cache_body["candidates"][0]["candidate"]["source"],
            "local_cache"
        );
        assert_eq!(
            cache_body["candidates"][0]["candidate"]["provenance_sanitized"]["origin"],
            "deployment_global_verified_cache"
        );

        let cache_retry = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/answer-candidates/import-local-cache",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cache_retry.status(), StatusCode::OK);
        assert!(
            response_json(cache_retry).await["candidates"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let response = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/answer-candidates",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = response_json(response).await;
        assert_eq!(body["question_snapshot_id"], snapshot.id.to_string());
        assert_eq!(body["candidates"][0]["id"], candidate_id.to_string());
        assert_eq!(body["candidates"][0]["candidate"]["source"], "manual");
        assert_eq!(body["candidates"].as_array().unwrap().len(), 3);

        let skip_response = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/answer-candidates",
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "question_id": question_id,
                        "answer": {"type": "skip"},
                        "explanation": "Explicitly skip this Question"
                    })
                    .to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(skip_response.status(), StatusCode::CREATED);
        let skip_body = response_json(skip_response).await;
        assert_eq!(skip_body["candidate"]["answer"], json!({"type": "skip"}));

        let draft_response = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/submission-drafts/{}",
                    snapshot.id, draft.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(draft_response.status(), StatusCode::OK);
        assert_eq!(draft_response.headers()[header::CACHE_CONTROL], "no-store");
        let draft_body = response_json(draft_response).await;
        assert_eq!(draft_body["id"], draft.id.to_string());
        assert_eq!(
            draft_body["items"][0]["selected"]["candidate_id"],
            candidate_id.to_string()
        );

        let result_response = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/submission-drafts/{}/results/{}",
                    snapshot.id, draft.id, result.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result_response.status(), StatusCode::OK);
        assert_eq!(result_response.headers()[header::CACHE_CONTROL], "no-store");
        let result_body = response_json(result_response).await;
        assert_eq!(result_body["id"], result.id.to_string());
        assert_eq!(result_body["status"], "confirmed");
        assert_eq!(result_body["verification"]["status"], "confirmed");

        let mismatched = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{}/question-snapshots/{}/answer-candidates",
                    TaskId::new(),
                    snapshot.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatched.status(), StatusCode::NOT_FOUND);

        let mismatched_result = app
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{task_id}/question-snapshots/{}/submission-drafts/{}/results/{}",
                    snapshot.id,
                    SubmissionDraftId::new(),
                    result.id
                ))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatched_result.status(), StatusCode::NOT_FOUND);

        let mismatched_draft = app
            .oneshot(
                Request::get(format!(
                    "/api/v1/tasks/{}/question-snapshots/{}/submission-drafts/{}",
                    TaskId::new(),
                    snapshot.id,
                    draft.id
                ))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatched_draft.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn task_provider_reads_reject_invalid_task_ids_before_provider_access() {
        let (app, _) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        for path in [
            "/api/v1/tasks/not-a-task/detail",
            "/api/v1/tasks/not-a-task/browser-session-spec",
            "/api/v1/tasks/not-a-task/progress",
            "/api/v1/tasks/not-a-task/duration",
            "/api/v1/tasks/not-a-task/questions",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(
                response_json(response).await["error"]["code"],
                "invalid_task_id"
            );
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration test keeps execution authorization, idempotency and atomic side effects together"
    )]
    async fn task_execution_action_is_atomic_idempotent_and_policy_guarded() {
        let (app, database, events, cookie, routine_task, other_task, formal_task, read_only_task) =
            execution_action_fixture().await;

        let created =
            post_task_execution(&app, &cookie, routine_task, Some("execute-document-1")).await;
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
        let created = response_json(created).await;
        assert_eq!(created["created"], true);
        assert_eq!(created["execution"]["request_source"], "web_ui");
        assert_eq!(
            created["execution"]["requested_capabilities"],
            json!(["resource_execution"])
        );
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

        assert_execution_stream(&app, &cookie, &execution_id, &events).await;

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
            ("execution_runtime_settings", "schema_version = 2"),
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
        let frozen_settings: (String, String, Option<i64>, Option<i64>, Option<i64>) =
            sqlx::query_as(
                "SELECT resolved_settings_json, sources_json, provider_revision, \
                        provider_account_revision, task_revision \
                 FROM execution_runtime_settings WHERE execution_id = ?",
            )
            .bind(&execution_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert!(frozen_settings.0.contains("execution.max_concurrency"));
        assert!(frozen_settings.0.contains('2'));
        assert!(frozen_settings.1.contains("schema_default"));
        assert_eq!(
            (frozen_settings.2, frozen_settings.3, frozen_settings.4),
            (None, None, None)
        );
        let frozen_plan: (String, String) = sqlx::query_as(
            "SELECT artifact_type, payload_json FROM execution_provider_plan_artifacts \
             WHERE execution_id = ?",
        )
        .bind(&execution_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(frozen_plan.0, "provider-alpha.api-planning.v1");
        let frozen_plan: Value = serde_json::from_str(&frozen_plan.1).unwrap();
        assert_eq!(frozen_plan["execution_id"], execution_id);
        assert_eq!(frozen_plan["task_id"], routine_task.to_string());
    }

    #[tokio::test]
    async fn task_execution_can_reserve_an_explicit_one_shot_quote_atomically() {
        let (app, database, _events, cookie, routine_task, _, _, _) =
            execution_action_fixture().await;
        sqlx::query("UPDATE credit_accounts SET available = 25, reserved = 0")
            .execute(database.pool())
            .await
            .unwrap();
        let response = app
            .oneshot(
                Request::post(format!("/api/v1/tasks/{routine_task}/execute"))
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", "quoted-execution-1")
                    .header("x-request-id", "quoted-execution-request")
                    .body(Body::from(
                        r#"{"requested_capabilities":["resource_execution"],"billing_amount":7,"billing_pricing_revision":"test-v1","billing_reason":"test execution"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let payload = response_json(response).await;
        let execution_id = payload["execution"]["id"].as_str().unwrap();
        assert!(!payload["execution"]["quote_id"].is_null());
        let account: (i64, i64) =
            sqlx::query_as("SELECT available, reserved FROM credit_accounts LIMIT 1")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(account, (18, 7));
        let reservation: (String, i64, String) = sqlx::query_as(
            "SELECT execution_id, amount, state FROM credit_reservations WHERE execution_id = ?",
        )
        .bind(execution_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            reservation,
            (execution_id.to_owned(), 7, "reserved".to_owned())
        );
    }

    #[tokio::test]
    async fn task_execution_uses_the_current_pricing_catalog_by_default() {
        let (app, database, _events, cookie, routine_task, _, _, _) =
            execution_action_fixture().await;
        sqlx::query("UPDATE credit_accounts SET available = 25, reserved = 0")
            .execute(database.pool())
            .await
            .unwrap();
        let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO pricing_catalog_revisions \
             (id, revision, catalog_json, effective_from, expires_at, created_by, created_at) \
             VALUES (?, ?, ?, ?, NULL, ?, ?)",
        )
        .bind(asterism_domain::AuditRecordId::new().to_string())
        .bind("default-test-v1")
        .bind(r#"{"default_amount":4,"answer_bank_hit_amount":2,"fixed_markup":1,"percentage_markup_basis_points":500,"reason":"catalog execution"}"#)
        .bind(now)
        .bind(user_id)
        .bind(now)
        .execute(database.pool())
        .await
        .unwrap();
        let (metered, answer_bank_amount) = task::configured_billing_with_answer_bank(
            &database,
            &[asterism_domain::TaskCapability::ResourceExecution],
            3,
        )
        .await
        .unwrap();
        assert_eq!(answer_bank_amount, 6);
        assert_eq!(metered.unwrap().amount.value(), 12);
        let response =
            post_task_execution(&app, &cookie, routine_task, Some("catalog-execution-1")).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let payload = response_json(response).await;
        assert!(!payload["execution"]["quote_id"].is_null());
        let amount: i64 =
            sqlx::query_scalar("SELECT amount FROM credit_reservations WHERE execution_id = ?")
                .bind(payload["execution"]["id"].as_str().unwrap())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(amount, 6);
    }

    #[tokio::test]
    async fn recharge_contact_is_owner_scoped_and_sanitized_from_active_catalog() {
        let (app, database, _events, cookie, _routine_task, _, _, _) =
            execution_action_fixture().await;
        let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO pricing_catalog_revisions \
             (id, revision, catalog_json, effective_from, expires_at, created_by, created_at) \
             VALUES (?, ?, ?, ?, NULL, ?, ?)",
        )
        .bind(asterism_domain::AuditRecordId::new().to_string())
        .bind("contact-test-v1")
        .bind(r#"{"default_amount":1,"recharge_contact":"QQ 群：123456\n请联系管理员"}"#)
        .bind(now)
        .bind(user_id)
        .bind(now)
        .execute(database.pool())
        .await
        .unwrap();
        let response = app
            .oneshot(
                Request::get("/api/v1/credits/recharge-contact")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        assert_eq!(payload["contact"], "QQ 群：123456\n请联系管理员");
    }

    #[tokio::test]
    async fn admin_answer_bank_usage_is_paginated_and_master_scoped() {
        let (app, database, _events, cookie, _routine_task, _, _, _) =
            execution_action_fixture().await;
        let owner_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
            .fetch_one(database.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO answer_bank_usage_records \
             (id, owner_user_id, task_id, source, hit_count, charged_amount, settlement_status, created_at) \
             VALUES (?, ?, NULL, 'local_cache', 2, 0, 'not_billable', ?)",
        )
        .bind(asterism_domain::AnswerCandidateId::new().to_string())
        .bind(owner_id)
        .bind(Utc::now())
        .execute(database.pool())
        .await
        .unwrap();
        let response = app
            .oneshot(
                Request::get("/api/v1/admin/answer-bank-usage?limit=1&offset=0")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["limit"], 1);
        assert_eq!(body["items"][0]["source"], "local_cache");
        assert_eq!(body["items"][0]["hit_count"], 2);
    }

    #[tokio::test]
    async fn formal_execution_requires_and_accepts_explicit_owner_confirmation() {
        let (app, _database, _events, cookie, _, _, formal_task, _) =
            execution_action_fixture().await;
        let response = app
            .oneshot(
                Request::post(format!("/api/v1/tasks/{formal_task}/execute"))
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "formal-first-confirmed")
                    .header("idempotency-key", "formal-first-confirmed")
                    .body(Body::from(
                        r#"{"requested_capabilities":["resource_execution"],"formal_assessment_confirmation":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let response = response_json(response).await;
        assert_eq!(response["created"], true);
        assert_eq!(
            response["execution"]["requested_capabilities"],
            json!(["resource_execution"])
        );
    }

    #[tokio::test]
    async fn formal_save_only_schedules_without_submit_confirmation() {
        let (app, _database, _events, cookie, _, _, formal_task, _) =
            execution_action_fixture().await;
        let response = app
            .oneshot(
                Request::post(format!("/api/v1/tasks/{formal_task}/execute"))
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "formal-save-only")
                    .header("idempotency-key", "formal-save-only")
                    .body(Body::from(
                        r#"{"requested_capabilities":["resource_execution"],"formal_assessment_save_only":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let payload = response_json(response).await;
        assert_eq!(payload["created"], true);
        assert_eq!(
            payload["execution"]["requested_capabilities"],
            json!(["resource_execution"])
        );
    }

    #[tokio::test]
    async fn formal_strict_retry_api_requires_current_workflow_revision() {
        let (app, database, _, cookie, _, _, formal_task, _) = execution_action_fixture().await;
        let (owner_id, account_id): (UserId, ProviderAccountId) = {
            let (owner_id, account_id): (String, String) = sqlx::query_as(
                "SELECT account.owner_user_id, account.id FROM tasks AS task \
                 INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
                 WHERE task.id = ?",
            )
            .bind(formal_task.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap();
            (owner_id.parse().unwrap(), account_id.parse().unwrap())
        };
        let now = Utc::now();
        let mut workflow = StrictCompletionWorkflow::new(
            CompletionWorkflowBinding {
                owner_user_id: owner_id,
                provider_account_id: account_id,
                task_id: formal_task,
            },
            CompletionPolicySnapshot {
                captured_at: now - ChronoDuration::seconds(3),
                ..CompletionPolicySnapshot::default()
            },
            None,
            now - ChronoDuration::seconds(3),
        )
        .unwrap();
        workflow
            .begin_attempt(true, false, now - ChronoDuration::seconds(2))
            .unwrap();
        workflow
            .observe(
                None,
                Some(asterism_domain::CompletionDiagnosis::DurationInsufficient),
                now - ChronoDuration::seconds(1),
            )
            .unwrap();
        SqliteCompletionWorkflowRepository::new(database.clone())
            .create_strict_completion_workflow(&workflow)
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET orchestration_state = 'human_required' WHERE id = ?")
            .bind(formal_task.to_string())
            .execute(database.pool())
            .await
            .unwrap();

        let workflow_id = workflow.id;
        let post_retry = |idempotency_key: &'static str, expected_revision: u32| {
            let app = app.clone();
            let cookie = cookie.clone();
            async move {
                app.oneshot(
                    Request::post(format!("/api/v1/tasks/{formal_task}/execute"))
                        .header(header::COOKIE, cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("x-request-id", format!("request-{idempotency_key}"))
                        .header("idempotency-key", idempotency_key)
                        .body(Body::from(
                            serde_json::json!({
                                "requested_capabilities": ["resource_execution"],
                                "strict_completion_retry_confirmation": {
                                    "workflow_id": workflow_id,
                                    "expected_revision": expected_revision,
                                }
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };
        let stale = post_retry("formal-retry-stale", 2).await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(stale).await["error"]["code"],
            "strict_completion_retry_conflict"
        );

        let confirmed = post_retry("formal-retry-confirmed", 1).await;
        assert_eq!(confirmed.status(), StatusCode::CREATED);
        let confirmed = response_json(confirmed).await;
        let execution_id = confirmed["execution"]["id"].as_str().unwrap();
        let replay = post_retry("formal-retry-confirmed", 1).await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(response_json(replay).await["execution"]["id"], execution_id);
        let persisted: (String, i64, String) = sqlx::query_as(
            "SELECT workflow_id, workflow_revision, confirmed_by \
             FROM execution_strict_completion_retry_confirmations WHERE execution_id = ?",
        )
        .bind(execution_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            persisted,
            (workflow.id.to_string(), 1, owner_id.to_string())
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the API regression keeps approval, delay, cancellation, ignore and replay guarantees in one shared workflow"
    )]
    async fn task_lifecycle_actions_share_one_idempotent_owner_scoped_contract() {
        let (app, database, _, cookie, waiting_task, ignored_task, _, _) =
            execution_action_fixture().await;
        sqlx::query("UPDATE tasks SET orchestration_state = 'waiting_approval' WHERE id = ?")
            .bind(waiting_task.to_string())
            .execute(database.pool())
            .await
            .unwrap();

        let unapproved =
            post_task_execution(&app, &cookie, waiting_task, Some("execute-before-approval")).await;
        assert_eq!(unapproved.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(unapproved).await["error"]["code"],
            "task_state_conflict"
        );

        let approved = post_task_lifecycle(
            &app,
            &cookie,
            waiting_task,
            "approve",
            "approve-task-1",
            None,
        )
        .await;
        assert_eq!(approved.status(), StatusCode::CREATED);
        let approved = response_json(approved).await;
        assert_eq!(approved["action"], "approve");
        assert_eq!(approved["task_state"], "ready");
        assert_eq!(approved["created"], true);

        let replayed = post_task_lifecycle(
            &app,
            &cookie,
            waiting_task,
            "approve",
            "approve-task-1",
            None,
        )
        .await;
        assert_eq!(replayed.status(), StatusCode::OK);
        assert_eq!(response_json(replayed).await["created"], false);

        let execution =
            post_task_execution(&app, &cookie, waiting_task, Some("execute-approved-task")).await;
        assert_eq!(execution.status(), StatusCode::CREATED);
        let execution_id = response_json(execution).await["execution"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let delayed_until = Utc::now() + chrono::Duration::hours(2);
        let delayed = post_task_lifecycle(
            &app,
            &cookie,
            waiting_task,
            "delay",
            "delay-task-1",
            Some(json!({"delayed_until": delayed_until})),
        )
        .await;
        assert_eq!(delayed.status(), StatusCode::CREATED);
        let delayed = response_json(delayed).await;
        assert_eq!(delayed["task_state"], "scheduled");
        assert_eq!(delayed["affected_execution_id"], execution_id);

        let cancelled =
            post_task_lifecycle(&app, &cookie, waiting_task, "cancel", "cancel-task-1", None).await;
        assert_eq!(cancelled.status(), StatusCode::CREATED);
        assert_eq!(response_json(cancelled).await["task_state"], "cancelled");
        let persisted: (String, String, String) = sqlx::query_as(
            "SELECT task.orchestration_state, execution.state, job.state \
             FROM tasks AS task INNER JOIN executions AS execution ON execution.task_id = task.id \
             INNER JOIN scheduled_jobs AS job ON job.idempotency_key = ? WHERE task.id = ?",
        )
        .bind(format!("execution:{execution_id}"))
        .bind(waiting_task.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            persisted,
            ("cancelled".into(), "cancelled".into(), "cancelled".into())
        );

        let ignored =
            post_task_lifecycle(&app, &cookie, ignored_task, "ignore", "ignore-task-1", None).await;
        assert_eq!(ignored.status(), StatusCode::CREATED);
        assert_eq!(response_json(ignored).await["task_state"], "ignored");

        let conflicting_key = post_task_lifecycle(
            &app,
            &cookie,
            ignored_task,
            "cancel",
            "approve-task-1",
            None,
        )
        .await;
        assert_eq!(conflicting_key.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(conflicting_key).await["error"]["code"],
            "idempotency_conflict"
        );
        let receipt_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_action_receipts")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(receipt_count, 4);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end administration test keeps mutation, audit, and owner scope together"
    )]
    async fn user_and_audit_admin_surfaces_are_password_free_revisioned_and_scoped() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "create-member-api")
                    .body(Body::from(
                        r#"{"username":"member","password":"member-password","roles":["user"],"permissions":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created = response_json(created).await;
        assert!(created.get("password_hash").is_none());
        assert!(created.get("password").is_none());
        let user_id = created["id"].as_str().unwrap();
        let expected_updated_at = created["updated_at"].as_str().unwrap();

        let list = app
            .clone()
            .oneshot(
                Request::get("/api/v1/admin/users?limit=10&offset=0")
                    .header(header::COOKIE, &master_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let list = response_json(list).await;
        assert_eq!(list["total"], 2);
        assert!(
            list["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|profile| profile.get("password_hash").is_none())
        );

        let updated = app
            .clone()
            .oneshot(
                Request::put(format!("/api/v1/admin/users/{user_id}"))
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "suspend-member-api")
                    .body(Body::from(
                        json!({
                            "expected_updated_at": expected_updated_at,
                            "status": "active",
                            "roles": ["user"],
                            "permissions": ["read_providers"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(updated.status(), StatusCode::OK);
        assert_eq!(
            response_json(updated).await["permissions"],
            json!(["read_providers"])
        );

        let stale = app
            .clone()
            .oneshot(
                Request::put(format!("/api/v1/admin/users/{user_id}"))
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "stale-member-api")
                    .body(Body::from(
                        json!({
                            "expected_updated_at": expected_updated_at,
                            "status": "suspended",
                            "roles": ["user"],
                            "permissions": []
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);

        let audit = app
            .clone()
            .oneshot(
                Request::get("/api/v1/audit?action=user_updated&limit=10&offset=0")
                    .header(header::COOKIE, &master_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(audit.status(), StatusCode::OK);
        let audit = response_json(audit).await;
        assert_eq!(audit["total"], 1);
        assert_eq!(audit["items"][0]["resource_id"], user_id);
        assert!(audit["items"][0].get("metadata_sanitized_json").is_none());
        assert!(audit["items"][0].get("metadata_sanitized").is_some());

        let login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 2], 41_002))))
                    .body(Body::from(
                        r#"{"username":"member","password":"member-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let member_cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let forbidden = app
            .clone()
            .oneshot(
                Request::get("/api/v1/admin/users")
                    .header(header::COOKIE, member_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let own_audit = app
            .clone()
            .oneshot(
                Request::get("/api/v1/audit?limit=50&offset=0")
                    .header(header::COOKIE, member_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(own_audit.status(), StatusCode::OK);
        let own_audit = response_json(own_audit).await;
        assert!(
            own_audit["items"].as_array().unwrap().iter().all(|record| {
                record["resource_id"] == user_id || record["actor_id"] == user_id
            })
        );
        let stored_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(stored_hash.starts_with("$argon2id$"));
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the credit grant API test keeps authorization, idempotency and atomic effects together"
    )]
    async fn master_credit_grant_is_atomic_idempotent_and_web_only() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let member = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"credit-member","password":"credit-member-password","roles":["user"],"permissions":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(member.status(), StatusCode::CREATED);
        let member = response_json(member).await;
        let user_id = member["id"].as_str().unwrap().to_owned();
        let grant_path = format!("/api/v1/admin/users/{user_id}/credit-grants");

        let first = app
            .clone()
            .oneshot(
                Request::post(&grant_path)
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", "grant-credit-member-100")
                    .header("x-request-id", "grant-credit-member-first")
                    .body(Body::from(r#"{"amount":100,"reason":"manual grant"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        let first = response_json(first).await;
        assert_eq!(first["created"], true);
        assert_eq!(first["account"]["available"], 100);
        assert_eq!(first["transaction"]["amount"], 100);

        let replay = app
            .clone()
            .oneshot(
                Request::post(&grant_path)
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", "grant-credit-member-100")
                    .header("x-request-id", "grant-credit-member-retry")
                    .body(Body::from(r#"{"amount":100,"reason":"manual grant"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        let replay = response_json(replay).await;
        assert_eq!(replay["created"], false);
        assert_eq!(replay["account"], first["account"]);
        assert_eq!(replay["transaction"], first["transaction"]);

        let conflict = app
            .clone()
            .oneshot(
                Request::post(&grant_path)
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", "grant-credit-member-100")
                    .body(Body::from(r#"{"amount":101,"reason":"manual grant"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);

        let member_login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 4], 41_004))))
                    .body(Body::from(
                        r#"{"username":"credit-member","password":"credit-member-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let member_cookie = member_login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let forbidden = app
            .oneshot(
                Request::post(&grant_path)
                    .header(header::COOKIE, member_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", "member-self-grant")
                    .body(Body::from(r#"{"amount":100,"reason":"self grant"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

        let transaction_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM credit_transactions WHERE user_id = ? AND transaction_type = 'master_grant'",
        )
        .bind(&user_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        let receipt_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM credit_grant_receipts")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE action = 'credit_granted'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!((transaction_count, receipt_count, audit_count), (1, 1, 1));
    }

    async fn execution_action_fixture() -> (
        Router,
        Database,
        EventBus,
        String,
        asterism_domain::TaskId,
        asterism_domain::TaskId,
        asterism_domain::TaskId,
        asterism_domain::TaskId,
    ) {
        let (app, database, events) = settings_test_app_with_events().await;
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
            events,
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
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-request-id", format!("request-{task_id}"));
        if let Some(idempotency_key) = idempotency_key {
            request = request.header("idempotency-key", idempotency_key);
        }
        app.clone()
            .oneshot(
                request
                    .body(Body::from(
                        r#"{"requested_capabilities":["resource_execution"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_task_lifecycle(
        app: &Router,
        cookie: &str,
        task_id: asterism_domain::TaskId,
        action: &str,
        idempotency_key: &str,
        body: Option<Value>,
    ) -> Response {
        app.clone()
            .oneshot(
                Request::post(format!("/api/v1/tasks/{task_id}/{action}"))
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", idempotency_key)
                    .header("x-request-id", format!("{action}-{task_id}"))
                    .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn assert_execution_stream(
        app: &Router,
        cookie: &str,
        execution_id: &str,
        events: &EventBus,
    ) {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/executions/{execution_id}/stream"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        let mut stream = response.into_body().into_data_stream();
        let snapshot = tokio::time::timeout(
            Duration::from_secs(1),
            tokio_stream::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let snapshot = std::str::from_utf8(&snapshot).unwrap();
        assert!(snapshot.contains("event: snapshot"));
        assert!(snapshot.contains(execution_id));

        events
            .publish(asterism_events::EventEnvelope::new(
                "foreign-execution",
                asterism_events::DomainEvent::ExecutionStateChanged {
                    execution_id: asterism_domain::ExecutionId::new(),
                    state: asterism_domain::ExecutionState::Running,
                },
            ))
            .unwrap();
        events
            .publish(asterism_events::EventEnvelope::new(
                "matching-execution",
                asterism_events::DomainEvent::ExecutionStateChanged {
                    execution_id: execution_id.parse().unwrap(),
                    state: asterism_domain::ExecutionState::Running,
                },
            ))
            .unwrap();
        let live = tokio::time::timeout(
            Duration::from_secs(1),
            tokio_stream::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let live = std::str::from_utf8(&live).unwrap();
        assert!(live.contains("event: execution_state"));
        assert!(live.contains("matching-execution"));
        assert!(!live.contains("foreign-execution"));

        let execution_id = execution_id.parse().unwrap();
        events
            .publish(asterism_events::EventEnvelope::new(
                "matching-log",
                asterism_events::DomainEvent::ExecutionLogged(asterism_domain::ExecutionLogEvent {
                    execution_id,
                    attempt_id: None,
                    timestamp: Utc::now(),
                    level: asterism_domain::LogLevel::Info,
                    stage: asterism_domain::ExecutionStage::Executing,
                    message: "sanitized live log".to_owned(),
                    provider_trace_id: Some("trace-safe".to_owned()),
                    metadata_sanitized: Some(json!({"safe": true})),
                }),
            ))
            .unwrap();
        let log = tokio::time::timeout(
            Duration::from_secs(1),
            tokio_stream::StreamExt::next(&mut stream),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let log = std::str::from_utf8(&log).unwrap();
        assert!(log.contains("event: execution_log"));
        assert!(log.contains("sanitized live log"));
        assert!(log.contains("matching-log"));
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
    async fn password_change_revokes_old_sessions_and_replaces_credentials() {
        let app = test_router().await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let changed = app
            .clone()
            .oneshot(
                Request::put("/api/v1/auth/password")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"current_password":"correct-horse-battery-staple","new_password":"new-correct-horse-battery-staple"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        if changed.status() != StatusCode::NO_CONTENT {
            let status = changed.status();
            let body = to_bytes(changed.into_body(), 16 * 1024).await.unwrap();
            panic!(
                "password change failed: {status} {}",
                String::from_utf8_lossy(&body)
            );
        }
        assert!(changed.headers().contains_key(header::SET_COOKIE));

        let revoked = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            app.clone()
                .oneshot(login_request("correct-horse-battery-staple"))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.oneshot(login_request("new-correct-horse-battery-staple"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn uninitialized_qq_style_user_can_set_first_password_without_current_secret() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::COOKIE, master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "create-qq-style-user")
                    .body(Body::from(
                        r#"{"username":"12345678","password":"unreachable-seed","roles":["user"],"permissions":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let user_id = response_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        sqlx::query("UPDATE users SET password_initialized = 0 WHERE id = ?")
            .bind(user_id)
            .execute(database.pool())
            .await
            .unwrap();
        let login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 3], 41_003))))
                    .body(Body::from(
                        r#"{"username":"12345678","password":"unreachable-seed"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let initialized = app
            .clone()
            .oneshot(
                Request::put("/api/v1/auth/password")
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"new_password":"chosen-user-password"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initialized.status(), StatusCode::NO_CONTENT);
        let relogin = app
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 4], 41_004))))
                    .body(Body::from(
                        r#"{"username":"12345678","password":"chosen-user-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(relogin.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn administrator_password_reset_revokes_user_sessions() {
        let app = test_router().await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::COOKIE, &master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "create-reset-member")
                    .body(Body::from(
                        r#"{"username":"reset-member","password":"original-password","roles":["user"],"permissions":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created = response_json(created).await;
        let user_id = created["id"].as_str().unwrap();
        let expected_updated_at = created["updated_at"].as_str().unwrap();
        let login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 5], 41_005))))
                    .body(Body::from(
                        r#"{"username":"reset-member","password":"original-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let member_cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let reset = app
            .clone()
            .oneshot(
                Request::put(format!("/api/v1/admin/users/{user_id}/password"))
                    .header(header::COOKIE, master_cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "reset-member-password")
                    .body(Body::from(
                        json!({
                            "password": "administrator-reset-password",
                            "expected_updated_at": expected_updated_at
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        if reset.status() != StatusCode::NO_CONTENT {
            let status = reset.status();
            let body = to_bytes(reset.into_body(), 16 * 1024).await.unwrap();
            panic!(
                "password reset failed: {status} {}",
                String::from_utf8_lossy(&body)
            );
        }
        let revoked = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/session")
                    .header(header::COOKIE, member_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
        let relogin = app
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 6], 41_006))))
                    .body(Body::from(
                        r#"{"username":"reset-member","password":"administrator-reset-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(relogin.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the route integrity test keeps the complete versioned API surface together"
    )]
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
        let body = to_bytes(response.into_body(), 512 * 1024).await.unwrap();
        let document: Value = serde_json::from_slice(&body).unwrap();
        if let Err(failures) = openapi_contract::validate(&document) {
            panic!("OpenAPI client-readiness validation failed: {failures:#?}");
        }
        let normalized_variants = document["components"]["schemas"]["NormalizedAnswer"]["oneOf"]
            .as_array()
            .unwrap();
        assert!(normalized_variants.iter().any(|variant| {
            variant["properties"]["type"]["const"] == "skip"
                && variant["required"] == json!(["type"])
                && variant["properties"].get("value").is_none()
                && variant["additionalProperties"] == false
        }));
        for path in [
            "/api/v1/auth/bootstrap",
            "/api/v1/auth/login",
            "/api/v1/auth/password",
            "/api/v1/auth/qq-login/{ticket}",
            "/api/v1/auth/session",
            "/api/v1/auth/logout",
            "/api/v1/auth-bootstrap/sessions",
            "/api/v1/auth-bootstrap/sessions/{session_id}",
            "/api/v1/auth-bootstrap/sessions/{session_id}/claim",
            "/api/v1/auth-bootstrap/sessions/{session_id}/credential",
            "/api/v1/auth-bootstrap/sessions/{session_id}/events",
            "/api/v1/auth-bootstrap/sessions/{session_id}/stream",
            "/api/v1/browser-bridge/sessions/{session_id}",
            "/api/v1/browser-bridge/sessions/{session_id}/claim",
            "/api/v1/browser-bridge/sessions/{session_id}/snapshot",
            "/api/v1/browser-bridge/sessions/{session_id}/binding",
            "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}",
            "/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}/result",
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
            "/api/v1/provider-accounts/{account_id}/auth-sessions/{session_id}/poll",
            "/api/v1/provider-accounts/{account_id}/scan-schedule",
            "/api/v1/admin/providers/{provider_id}/runtime-settings/schema",
            "/api/v1/admin/providers/{provider_id}/runtime-settings",
            "/api/v1/admin/provider-accounts/{account_id}/runtime-settings",
            "/api/v1/admin/tasks/{task_id}/runtime-settings",
            "/api/v1/tasks",
            "/api/v1/tasks/{task_id}",
            "/api/v1/tasks/{task_id}/detail",
            "/api/v1/tasks/{task_id}/browser-session-spec",
            "/api/v1/tasks/{task_id}/browser-bridge/sessions",
            "/api/v1/tasks/{task_id}/progress",
            "/api/v1/tasks/{task_id}/duration",
            "/api/v1/tasks/{task_id}/questions",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/provider-answer-candidates",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates/import-local-cache",
            "/api/v1/tasks/{task_id}/ai-discussion-invocation-drafts",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-resolution",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}",
            "/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}/results/{result_id}",
            "/api/v1/tasks/{task_id}/execute",
            "/api/v1/credits/account",
            "/api/v1/credits/transactions",
            "/api/v1/credits/reservations",
            "/api/v1/admin/users/{user_id}/credit-grants",
            "/api/v1/admin/users/{user_id}/password",
            "/api/v1/executions",
            "/api/v1/executions/{execution_id}",
            "/api/v1/executions/{execution_id}/logs",
            "/api/v1/executions/{execution_id}/stream",
        ] {
            assert!(document["paths"].get(path).is_some(), "missing {path}");
        }
        assert_eq!(
            document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/detail"]["get"]["operationId"],
            "getTaskDetail"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/browser-session-spec"]["get"]["operationId"],
            "getTaskBrowserSessionSpec"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/browser-bridge/sessions"]["post"]["operationId"],
            "createBrowserBridgeSession"
        );
        assert_eq!(
            document["paths"]["/api/v1/browser-bridge/sessions/{session_id}/claim"]["post"]["security"]
                [0]["browserBridgeAuth"],
            json!([])
        );
        assert_eq!(
            document["paths"]["/api/v1/browser-bridge/sessions/{session_id}/snapshot"]["get"]["operationId"],
            "pollBrowserBridgeSnapshot"
        );
        assert_eq!(
            document["paths"]["/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}"]["get"]
                ["operationId"],
            "dispatchBrowserBridgeCommand"
        );
        assert_eq!(
            document["paths"]["/api/v1/browser-bridge/sessions/{session_id}/commands/{sequence}/result"]
                ["post"]["operationId"],
            "receiveBrowserBridgeResult"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/progress"]["get"]["operationId"],
            "getTaskProgress"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/duration"]["get"]["operationId"],
            "getTaskDuration"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/questions"]["get"]["operationId"],
            "getTaskQuestions"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/provider-answer-candidates"]
                ["post"]["operationId"],
            "resolveProviderAnswerCandidates"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates"]
                ["get"]["operationId"],
            "listAnswerCandidates"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates"]
                ["post"]["operationId"],
            "createManualAnswerCandidate"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-candidates/import-local-cache"]
                ["post"]["operationId"],
            "importLocalAnswerCandidates"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/answer-resolution"]
                ["get"]["operationId"],
            "resolveAnswerCandidates"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts"]
                ["post"]["operationId"],
            "buildSubmissionDraft"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}"]
                ["get"]["operationId"],
            "getSubmissionDraft"
        );
        assert_eq!(
            document["paths"]["/api/v1/tasks/{task_id}/question-snapshots/{snapshot_id}/submission-drafts/{draft_id}/results/{result_id}"]
                ["get"]["operationId"],
            "getSubmissionResult"
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
            document["paths"]["/api/v1/executions/{execution_id}/stream"]["get"]["operationId"],
            "streamExecution"
        );
        assert_eq!(
            document["paths"]["/api/v1/credits/account"]["get"]["operationId"],
            "getOwnCreditAccount"
        );
        assert_eq!(
            document["paths"]["/api/v1/credits/transactions"]["get"]["operationId"],
            "listOwnCreditTransactions"
        );
        assert_eq!(
            document["paths"]["/api/v1/credits/reservations"]["get"]["operationId"],
            "listOwnCreditReservations"
        );
        assert_eq!(
            document["paths"]["/api/v1/admin/users/{user_id}/credit-grants"]["post"]["operationId"],
            "grantUserCredits"
        );
        assert_eq!(
            document["paths"]["/api/v1/admin/tasks/{task_id}/runtime-settings"]["put"]["operationId"],
            "putTaskRuntimeSettings"
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
    async fn qq_web_login_ticket_is_bound_single_use_and_never_exposes_the_bearer() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let service = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, master_cookie)
                    .body(Body::from(
                        r#"{"name":"qq-gateway","scopes":["qq_identity_assert"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(service.status(), StatusCode::OK);
        let service = response_json(service).await;
        let service_token = service["token"].as_str().unwrap();
        let asserted = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/identity/assert")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {service_token}"))
                    .body(Body::from(
                        r#"{"qq":"123456789","return_to":"/tasks/task-safe?confirm=1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(asserted.status(), StatusCode::OK);
        let asserted = response_json(asserted).await;
        let bearer = asserted["access_token"].as_str().unwrap();
        let login_path = asserted["web_login_path"].as_str().unwrap();
        assert!(!login_path.contains(bearer));
        let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM qq_web_login_tickets")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(stored.len(), 32);
        assert!(
            !login_path
                .as_bytes()
                .windows(stored.len())
                .any(|part| part == stored)
        );

        let login = app
            .clone()
            .oneshot(Request::get(login_path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::SEE_OTHER);
        assert_eq!(login.headers()[header::LOCATION], "/settings/password");
        let qq_cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let identity = app
            .clone()
            .oneshot(
                Request::get("/api/v1/auth/session")
                    .header(header::COOKIE, qq_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(identity.status(), StatusCode::OK);

        let replay = app
            .oneshot(Request::get(login_path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn yunzai_master_assertion_is_scope_bound_monotonic_and_audited() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let master_cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let limited = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, master_cookie)
                    .body(Body::from(
                        r#"{"name":"qq-limited","scopes":["qq_identity_assert"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let limited = response_json(limited).await;
        let denied = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/identity/assert")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", limited["token"].as_str().unwrap()),
                    )
                    .body(Body::from(r#"{"qq":"223344556","master_assertion":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let gateway = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, master_cookie)
                    .body(Body::from(
                        r#"{"name":"yunzai-gateway","scopes":["provider_read","task_read","task_execute","qq_identity_assert","task_command_proxy"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let gateway = response_json(gateway).await;
        let gateway_token = gateway["token"].as_str().unwrap();
        let registered = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/identity/assert")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {gateway_token}"))
                    .body(Body::from(r#"{"qq":"223344556","master_assertion":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(registered.status(), StatusCode::OK);
        let registered = response_json(registered).await;
        let user_id = registered["user_id"].as_str().unwrap().to_owned();
        let target_account = app
            .clone()
            .oneshot(
                Request::post("/api/v1/provider-accounts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, master_cookie)
                    .body(Body::from(format!(
                        r#"{{"provider_id":"provider-alpha","display_name":"qq-target","owner_user_id":"{user_id}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(target_account.status(), StatusCode::CREATED);
        let delegated_accounts = app
            .clone()
            .oneshot(
                Request::get("/api/v1/provider-accounts")
                    .header(header::AUTHORIZATION, format!("Bearer {gateway_token}"))
                    .header("x-asterism-target-owner", &user_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delegated_accounts.status(), StatusCode::OK);
        assert_eq!(response_json(delegated_accounts).await["total"], 1);
        let delegated_task = TaskId::new();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        let account_id: String =
            sqlx::query_scalar("SELECT id FROM provider_accounts WHERE owner_user_id = ? LIMIT 1")
                .bind(&user_id)
                .fetch_one(database.pool())
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'qq-delegated-task', 'qq-delegated-fingerprint', 'work', \
                     'routine', 'QQ delegated task', 'pending', 'ready', ?, ?, '[]')",
        )
        .bind(delegated_task.to_string())
        .bind(account_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let delegated_ignore = app
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/tasks/{delegated_task}/ignore"))
                    .header(header::AUTHORIZATION, format!("Bearer {gateway_token}"))
                    .header("x-asterism-target-owner", &user_id)
                    .header("idempotency-key", "qq-delegated-ignore")
                    .header("x-request-id", "qq-delegated-ignore-request")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delegated_ignore.status(), StatusCode::CREATED);
        let delegated_actor_type: String = sqlx::query_scalar(
            "SELECT actor_type FROM audit_records WHERE resource_id = ? AND action = 'task_ignore'",
        )
        .bind(delegated_task.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(delegated_actor_type, "service_token");
        let roles: String = sqlx::query_scalar("SELECT roles_json FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(roles, r#"["user"]"#);

        let asserted = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/identity/assert")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {gateway_token}"))
                    .body(Body::from(r#"{"qq":"223344556","master_assertion":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(asserted.status(), StatusCode::OK);
        let roles: String = sqlx::query_scalar("SELECT roles_json FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(roles, r#"["master"]"#);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? AND actor_type = 'service_token'",
        )
        .bind(&user_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(audit_count >= 1);

        let asserted_again = app
            .oneshot(
                Request::post("/api/v1/integrations/qq/identity/assert")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {gateway_token}"))
                    .body(Body::from(r#"{"qq":"223344556","master_assertion":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(asserted_again.status(), StatusCode::OK);
        let roles: String = sqlx::query_scalar("SELECT roles_json FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(roles, r#"["master"]"#);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end token test keeps delegation and cross-owner isolation together"
    )]
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
                    .header(header::COOKIE, &cookie)
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
        let token_id = created["metadata"]["id"].as_str().unwrap().to_owned();
        let stored_digest: Vec<u8> =
            sqlx::query_scalar("SELECT token_hash FROM service_tokens WHERE name = 'limited'")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(stored_digest.len(), 32);
        assert_ne!(stored_digest, plaintext.as_bytes());

        let other_master = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/users")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"other-master","password":"other-master-password","roles":["master"],"permissions":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_master.status(), StatusCode::CREATED);
        let other_master_login = app
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 3], 41_003))))
                    .body(Body::from(
                        r#"{"username":"other-master","password":"other-master-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_master_login.status(), StatusCode::OK);
        let other_cookie = other_master_login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let foreign = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, other_cookie)
                    .body(Body::from(
                        r#"{"name":"foreign","scopes":["service_token_manage"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::OK);
        let foreign = response_json(foreign).await;
        let foreign_id = foreign["metadata"]["id"].as_str().unwrap().to_owned();

        let system_list = app
            .clone()
            .oneshot(
                Request::get("/api/v1/service-tokens?limit=20&offset=0")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(system_list.status(), StatusCode::OK);
        let system_list = response_json(system_list).await;
        assert_eq!(system_list["total"], 2);
        assert!(
            system_list["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|token| { token.get("token").is_none() && token.get("token_hash").is_none() })
        );

        let owner_list = app
            .clone()
            .oneshot(
                Request::get("/api/v1/service-tokens?limit=20&offset=0")
                    .header(header::AUTHORIZATION, format!("Bearer {plaintext}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(owner_list.status(), StatusCode::OK);
        let owner_list = response_json(owner_list).await;
        assert_eq!(owner_list["total"], 1);
        assert_eq!(owner_list["items"][0]["id"], token_id);

        let foreign_revoke = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/service-tokens/{foreign_id}"))
                    .header(header::AUTHORIZATION, format!("Bearer {plaintext}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(foreign_revoke.status(), StatusCode::NOT_FOUND);
        let foreign_revoked_at: Option<String> =
            sqlx::query_scalar("SELECT revoked_at FROM service_tokens WHERE id = ?")
                .bind(&foreign_id)
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(foreign_revoked_at.is_none());

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
    async fn qq_notification_delivery_claims_and_retries_formal_tasks() {
        let (app, database) = test_app(false, None).await;
        let bootstrap = bootstrap(&app).await;
        let cookie = bootstrap.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE username = 'master'")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO qq_identities (user_id, qq, verified_at, is_primary) VALUES (?, 123456789, ?, 1)",
        )
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) VALUES ('account-qq-notify', ?, 'chaoxing', 'notify', '{}', ?, ?)",
        )
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, title, remote_state, orchestration_state, closes_at, discovered_at, updated_at, capabilities_json) VALUES ('formal-task-1', 'account-qq-notify', 'remote-formal-1', 'fingerprint-1', 'exam', 'formal', '期末作业', 'pending', 'ready', ?, ?, ?, '[]')",
        )
        .bind((now + chrono::Duration::hours(2)).to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let token = app
            .clone()
            .oneshot(
                Request::post("/api/v1/service-tokens")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        r#"{"name":"qq-delivery","scopes":["notification_delivery_report"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token.status(), StatusCode::OK);
        let token = response_json(token).await["token"]
            .as_str()
            .unwrap()
            .to_owned();

        let claim = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/notifications/claim")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(claim.status(), StatusCode::OK);
        let claim = response_json(claim).await;
        assert_eq!(claim["items"].as_array().unwrap().len(), 1);
        assert_eq!(claim["items"][0]["kind"], "confirmation_due");
        assert_eq!(claim["items"][0]["qq"], "123456789");
        assert_eq!(claim["items"][0]["task_id"], "formal-task-1");
        assert!(
            claim["items"][0]["web_login_path"]
                .as_str()
                .unwrap()
                .starts_with("/api/v1/auth/qq-login/")
        );
        let delivery_id = claim["items"][0]["id"].as_str().unwrap().to_owned();

        let report = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/notifications/report")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"items":[{{"id":"{delivery_id}","delivered":false,"error":" send\nfailed "}}]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(report.status(), StatusCode::NO_CONTENT);
        let row: (String, i64, String) = sqlx::query_as(
            "SELECT state, attempts, last_error FROM qq_formal_notification_deliveries WHERE id = ?",
        )
        .bind(&delivery_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(row, ("retry".to_owned(), 1, "sendfailed".to_owned()));

        sqlx::query(
            "UPDATE qq_formal_notification_deliveries SET next_attempt_at = ? WHERE id = ?",
        )
        .bind((now - chrono::Duration::minutes(1)).to_rfc3339())
        .bind(&delivery_id)
        .execute(database.pool())
        .await
        .unwrap();
        let retry_claim = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/notifications/claim")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(retry_claim.status(), StatusCode::OK);
        let retry_claim = response_json(retry_claim).await;
        assert_eq!(retry_claim["items"][0]["id"], delivery_id);
        let attempts: i64 = sqlx::query_scalar(
            "SELECT attempts FROM qq_formal_notification_deliveries WHERE id = ?",
        )
        .bind(&delivery_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(attempts, 2);

        sqlx::query("UPDATE tasks SET closes_at = ? WHERE id = 'formal-task-1'")
            .bind((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339())
            .execute(database.pool())
            .await
            .unwrap();
        let missed = app
            .clone()
            .oneshot(
                Request::post("/api/v1/integrations/qq/notifications/claim")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missed.status(), StatusCode::OK);
        let missed = response_json(missed).await;
        assert_eq!(missed["items"].as_array().unwrap().len(), 1);
        assert_eq!(missed["items"][0]["kind"], "deadline_missed");
        assert!(
            missed["items"][0]["message"]
                .as_str()
                .unwrap()
                .contains("没有自动提交")
        );

        sqlx::query(
            "INSERT INTO tasks (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES ('routine-task-1', 'account-qq-notify', 'remote-routine-1', 'fingerprint-routine-1', 'chapter', 'routine', '章节任务', 'in_progress', 'running', ?, ?, '[]')",
        )
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO executions (id, task_id, requested_by, request_source, state, started_at, created_at) \
             VALUES ('execution-progress-1', 'routine-task-1', ?, 'scheduler', 'running', ?, ?)",
        )
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_progress (execution_id, percent, stage, status_text, completed_items, total_items, updated_at) \
             VALUES ('execution-progress-1', 50, 'video', '正在播放', 1, 2, ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let visible_progress: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM executions AS execution \
             INNER JOIN execution_progress AS progress ON progress.execution_id = execution.id \
             INNER JOIN tasks ON tasks.id = execution.task_id \
             INNER JOIN provider_accounts AS accounts ON accounts.id = tasks.provider_account_id \
             INNER JOIN qq_identities AS identities ON identities.user_id = accounts.owner_user_id AND identities.is_primary = 1 \
             WHERE execution.state = 'running' AND progress.percent BETWEEN 25 AND 99",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(visible_progress, 1);
        let progress = app
            .oneshot(
                Request::post("/api/v1/integrations/qq/notifications/claim")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let progress_status = progress.status();
        let progress = response_json(progress).await;
        assert_eq!(progress_status, StatusCode::OK, "{progress}");
        assert_eq!(progress["items"].as_array().unwrap().len(), 1);
        assert_eq!(progress["items"][0]["kind"], "execution_progress");
        assert!(
            progress["items"][0]["message"]
                .as_str()
                .unwrap()
                .contains("50%")
        );
        let progress_return_to: String = sqlx::query_scalar(
            "SELECT return_to FROM qq_web_login_tickets ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(progress_return_to, "/executions/execution-progress-1");
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
