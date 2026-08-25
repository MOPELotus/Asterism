use std::{collections::BTreeSet, fmt, str::FromStr};

use asterism_auth::Argon2idPasswordService;
use asterism_config::AiConfig;
use asterism_domain::{
    Permission, ProtocolObservationKind, ProviderId, Role, Timestamp, User, UserId, UserStatus,
};
use asterism_secrets::SecretString;
use asterism_storage::{
    AuditFilter, AuditQueryRepository, ProtocolObservationRepository, SqliteAdminRepository,
    SqliteProtocolObservationRepository, UserAdminCreate, UserAdminCreateOutcome,
    UserAdminRepository, UserAdminUpdate, UserAdminUpdateOutcome,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    ApiError, ApiState,
    auth::{AuditAuthority, AuthContext, ProviderSettingsAuthority},
};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;

pub(super) async fn get_ai_config(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    Ok(crate::auth::no_store(
        Json(state.ai_config().await).into_response(),
    ))
}

pub(super) async fn put_ai_config(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<AiConfig>, JsonRejection>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    let Json(config) = payload.map_err(|_| {
        ApiError::bad_request("invalid_ai_config", "AI configuration body is invalid")
    })?;
    config
        .validate()
        .map_err(|error| ApiError::bad_request("invalid_ai_config", error.to_string()))?;
    let encoded = serde_json::to_string(&config).map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO deployment_ai_config (singleton, config_json, updated_at) VALUES (1, ?, ?) \
         ON CONFLICT(singleton) DO UPDATE SET config_json = excluded.config_json, updated_at = excluded.updated_at",
    )
    .bind(encoded)
    .bind(Utc::now())
    .execute(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let metadata = serde_json::json!({
        "remote_store": config.remote_store,
        "gpt_router_base_url_configured": !config.gpt_router.base_url.is_empty(),
        "profiles": ["economy", "gpt_only"],
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'system', ?, 'ai_config_updated', 'deployment', 'ai', ?, 'succeeded', ?)",
    )
    .bind(asterism_domain::AuditRecordId::new().to_string())
    .bind(Utc::now())
    .bind(Option::<String>::None)
    .bind(asterism_domain::AuditRecordId::new().to_string())
    .bind(serde_json::to_string(&metadata).map_err(ApiError::internal)?)
    .execute(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    state.replace_ai_config(config.clone()).await;
    Ok(crate::auth::no_store(Json(config).into_response()))
}

pub(super) async fn list_ai_usage(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    let (limit, offset) = parse_page(query)?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_usage_records")
        .fetch_one(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
    let rows = sqlx::query(
        "SELECT id, owner_user_id, task_id, provider_endpoint, model, profile, route, \
                input_chars, output_chars, remote_input_tokens, remote_output_tokens, remote_cache_read_tokens, remote_cache_write_tokens, \
                outcome, created_at, estimated_cost, settlement_status \
         FROM ai_usage_records ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
    )
    .bind(i64::from(limit))
    .bind(i64::try_from(offset).map_err(ApiError::internal)?)
    .fetch_all(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let items = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.try_get::<String, _>("id").unwrap_or_default(),
                "owner_user_id": row.try_get::<String, _>("owner_user_id").unwrap_or_default(),
                "task_id": row.try_get::<Option<String>, _>("task_id").unwrap_or(None),
                "endpoint": row.try_get::<String, _>("provider_endpoint").unwrap_or_default(),
                "model": row.try_get::<String, _>("model").unwrap_or_default(),
                "profile": row.try_get::<String, _>("profile").unwrap_or_default(),
                "route": row.try_get::<String, _>("route").unwrap_or_default(),
                "input_chars": row.try_get::<i64, _>("input_chars").unwrap_or_default(),
                "output_chars": row.try_get::<i64, _>("output_chars").unwrap_or_default(),
                "remote_input_tokens": row.try_get::<Option<i64>, _>("remote_input_tokens").unwrap_or(None),
                "remote_output_tokens": row.try_get::<Option<i64>, _>("remote_output_tokens").unwrap_or(None),
                "remote_cache_read_tokens": row.try_get::<Option<i64>, _>("remote_cache_read_tokens").unwrap_or(None),
                "remote_cache_write_tokens": row.try_get::<Option<i64>, _>("remote_cache_write_tokens").unwrap_or(None),
                "outcome": row.try_get::<String, _>("outcome").unwrap_or_default(),
                "estimated_cost": row.try_get::<i64, _>("estimated_cost").unwrap_or_default(),
                "settlement_status": row.try_get::<String, _>("settlement_status").unwrap_or_else(|_| "pending".to_owned()),
                "created_at": row.try_get::<String, _>("created_at").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    Ok(crate::auth::no_store(
        Json(serde_json::json!({"items": items, "total": total, "limit": limit, "offset": offset}))
            .into_response(),
    ))
}

pub(super) async fn list_answer_bank_usage(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    let (limit, offset) = parse_page(query)?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM answer_bank_usage_records")
        .fetch_one(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
    let rows = sqlx::query(
        "SELECT id, owner_user_id, task_id, execution_id, source, hit_count, charged_amount, settlement_status, created_at \
         FROM answer_bank_usage_records ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
    )
    .bind(i64::from(limit))
    .bind(i64::try_from(offset).map_err(ApiError::internal)?)
    .fetch_all(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let items = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.try_get::<String, _>("id").unwrap_or_default(),
                "owner_user_id": row.try_get::<String, _>("owner_user_id").unwrap_or_default(),
                "task_id": row.try_get::<Option<String>, _>("task_id").unwrap_or(None),
                "execution_id": row.try_get::<Option<String>, _>("execution_id").unwrap_or(None),
                "source": row.try_get::<String, _>("source").unwrap_or_default(),
                "hit_count": row.try_get::<i64, _>("hit_count").unwrap_or_default(),
                "charged_amount": row.try_get::<i64, _>("charged_amount").unwrap_or_default(),
                "settlement_status": row.try_get::<String, _>("settlement_status").unwrap_or_default(),
                "created_at": row.try_get::<String, _>("created_at").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    Ok(crate::auth::no_store(
        Json(serde_json::json!({"items": items, "total": total, "limit": limit, "offset": offset}))
            .into_response(),
    ))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricingCatalogRequest {
    revision: String,
    catalog: serde_json::Value,
    effective_from: Option<Timestamp>,
    expires_at: Option<Timestamp>,
}

pub(super) async fn get_pricing_catalog(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    auth.require_pricing_manage()?;
    let row = sqlx::query(
        "SELECT id, revision, catalog_json, effective_from, expires_at, created_by, created_at \
         FROM pricing_catalog_revisions ORDER BY effective_from DESC, created_at DESC LIMIT 1",
    )
    .fetch_optional(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let Some(row) = row else {
        return Ok(crate::auth::no_store(
            Json(serde_json::Value::Null).into_response(),
        ));
    };
    let catalog: serde_json::Value = serde_json::from_str(
        &row.try_get::<String, _>("catalog_json")
            .map_err(ApiError::internal)?,
    )
    .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(serde_json::json!({
            "id": row.try_get::<String, _>("id").map_err(ApiError::internal)?,
            "revision": row.try_get::<String, _>("revision").map_err(ApiError::internal)?,
            "catalog": catalog,
            "effective_from": row.try_get::<String, _>("effective_from").map_err(ApiError::internal)?,
            "expires_at": row.try_get::<Option<String>, _>("expires_at").map_err(ApiError::internal)?,
            "created_by": row.try_get::<String, _>("created_by").map_err(ApiError::internal)?,
            "created_at": row.try_get::<String, _>("created_at").map_err(ApiError::internal)?,
        }))
        .into_response(),
    ))
}

pub(super) async fn put_pricing_catalog(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    payload: Result<Json<PricingCatalogRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let created_by = auth.require_pricing_manage()?;
    let Json(request) = payload.map_err(|_| {
        ApiError::bad_request("invalid_pricing_catalog", "pricing catalog body is invalid")
    })?;
    if request.revision.is_empty()
        || request.revision.len() > 128
        || request.revision.chars().any(char::is_control)
        || !request.catalog.is_object()
    {
        return Err(ApiError::bad_request(
            "invalid_pricing_catalog",
            "revision must be 1-128 bytes and catalog must be a JSON object",
        ));
    }
    let catalog_json = serde_json::to_string(&request.catalog).map_err(ApiError::internal)?;
    if catalog_json.len() > 64 * 1024 {
        return Err(ApiError::bad_request(
            "invalid_pricing_catalog",
            "pricing catalog must not exceed 65536 bytes",
        ));
    }
    let now = Utc::now();
    let effective_from = request.effective_from.unwrap_or_else(|| now.into());
    let id = asterism_domain::AuditRecordId::new().to_string();
    sqlx::query(
        "INSERT INTO pricing_catalog_revisions \
         (id, revision, catalog_json, effective_from, expires_at, created_by, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&request.revision)
    .bind(&catalog_json)
    .bind(effective_from)
    .bind(request.expires_at)
    .bind(created_by.to_string())
    .bind(now)
    .execute(state.database.pool())
    .await
    .map_err(|error| {
        if let sqlx::Error::Database(database_error) = &error {
            if database_error.message().contains("UNIQUE") {
                return ApiError::conflict(
                    "pricing_revision_exists",
                    "pricing revision already exists",
                );
            }
        }
        ApiError::internal(error)
    })?;
    Ok(crate::auth::no_store(
        Json(
            serde_json::json!({"id": id, "revision": request.revision, "catalog": request.catalog}),
        )
        .into_response(),
    ))
}

fn require_system_authority(authority: ProviderSettingsAuthority) -> Result<(), ApiError> {
    match authority {
        ProviderSettingsAuthority::System => Ok(()),
        ProviderSettingsAuthority::Owner(_) => Err(ApiError::forbidden()),
    }
}

pub(super) async fn list_users(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    auth.require_user_manage()?;
    let (limit, offset) = parse_page(query)?;
    let page = SqliteAdminRepository::new(state.database)
        .list_user_profiles(limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(UserProfilePageResponse {
            items: page.items,
            total: page.total,
            limit,
            offset,
        })
        .into_response(),
    ))
}

pub(super) async fn get_user(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
) -> Result<Response, ApiError> {
    auth.require_user_manage()?;
    let user_id = parse_user_id(&user_id)?;
    let profile = SqliteAdminRepository::new(state.database)
        .find_user_profile(user_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("user_not_found"))?;
    Ok(crate::auth::no_store(Json(profile).into_response()))
}

pub(super) async fn create_user(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    payload: Result<Json<CreateUserRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    auth.require_user_manage()?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request(
            "invalid_create_user_request",
            "user creation body is invalid",
        )
    })?;
    let username = crate::auth::validate_username(&request.username)?.to_owned();
    crate::auth::validate_password(&request.password, true)?;
    if request.roles.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_user_roles",
            "at least one user role is required",
        ));
    }
    let password_hash = Argon2idPasswordService::default()
        .hash(&request.password)
        .map_err(ApiError::internal)?;
    let now = Utc::now();
    let user = User {
        id: UserId::new(),
        username,
        password_hash,
        status: request.status.unwrap_or(UserStatus::Active),
        roles: request.roles.into_iter().collect(),
        permissions: request.permissions.into_iter().collect(),
        created_at: now,
        updated_at: now,
    };
    let correlation_id = required_request_id(&headers)?;
    match SqliteAdminRepository::new(state.database)
        .create_user(UserAdminCreate {
            user: &user,
            actor: auth.audit_actor(),
            correlation_id,
        })
        .await
        .map_err(ApiError::internal)?
    {
        UserAdminCreateOutcome::Created(profile) => Ok(crate::auth::no_store(
            (StatusCode::CREATED, Json(profile)).into_response(),
        )),
        UserAdminCreateOutcome::UsernameConflict => Err(ApiError::conflict(
            "username_conflict",
            "the username is already in use",
        )),
    }
}

pub(super) async fn update_user(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<UpdateUserRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    auth.require_user_manage()?;
    let user_id = parse_user_id(&user_id)?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request("invalid_update_user_request", "user update body is invalid")
    })?;
    if request.roles.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_user_roles",
            "at least one user role is required",
        ));
    }
    let now = Utc::now();
    if request.expected_updated_at >= now {
        return Err(ApiError::bad_request(
            "invalid_user_revision",
            "expected_updated_at must identify an earlier persisted revision",
        ));
    }
    let roles: Vec<_> = request.roles.into_iter().collect();
    let permissions: Vec<_> = request.permissions.into_iter().collect();
    let correlation_id = required_request_id(&headers)?;
    match SqliteAdminRepository::new(state.database)
        .update_user(UserAdminUpdate {
            user_id,
            expected_updated_at: request.expected_updated_at,
            status: request.status,
            roles: &roles,
            permissions: &permissions,
            actor: auth.audit_actor(),
            correlation_id,
            updated_at: now,
        })
        .await
        .map_err(ApiError::internal)?
    {
        UserAdminUpdateOutcome::Updated(profile) => {
            Ok(crate::auth::no_store(Json(profile).into_response()))
        }
        UserAdminUpdateOutcome::UserNotFound => Err(ApiError::not_found("user_not_found")),
        UserAdminUpdateOutcome::RevisionConflict => Err(ApiError::conflict(
            "user_revision_conflict",
            "the user changed after expected_updated_at",
        )),
        UserAdminUpdateOutcome::LastActiveMaster => Err(ApiError::conflict(
            "last_active_master",
            "the final active Master cannot be suspended, disabled, or demoted",
        )),
    }
}

pub(super) async fn list_audit(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<AuditListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let authority = auth.require_audit_read()?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request("invalid_audit_query", "audit query parameters are invalid")
    })?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    validate_page(limit, offset)?;
    let filter = AuditFilter {
        action: validate_filter(query.action, "action")?,
        resource_type: validate_filter(query.resource_type, "resource_type")?,
        resource_id: validate_filter(query.resource_id, "resource_id")?,
        outcome: validate_filter(query.outcome, "outcome")?,
    };
    let owner_scope = match authority {
        AuditAuthority::Any => None,
        AuditAuthority::Owner(user_id) => Some(user_id),
    };
    let page = SqliteAdminRepository::new(state.database)
        .list_audit_records(owner_scope, &filter, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(AuditPageResponse {
            items: page.items,
            total: page.total,
            limit,
            offset,
        })
        .into_response(),
    ))
}

pub(super) async fn list_protocol_observations(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<ProtocolObservationListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    auth.require_user_manage()?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_protocol_observation_query",
            "protocol observation query parameters are invalid",
        )
    })?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    validate_page(limit, offset)?;
    let provider_id = query
        .provider_id
        .map(ProviderId::new)
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid_provider_id", "provider ID is invalid"))?;
    let kind: Option<ProtocolObservationKind> = query
        .kind
        .map(|kind| serde_json::from_value(serde_json::Value::String(kind)))
        .transpose()
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_protocol_observation_kind",
                "protocol observation kind is invalid",
            )
        })?;
    let page = SqliteProtocolObservationRepository::new(state.database)
        .list_protocol_observations(provider_id.as_ref(), kind, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(ProtocolObservationPageResponse {
            items: page.items,
            total: page.total,
            limit,
            offset,
        })
        .into_response(),
    ))
}

fn parse_page(query: Result<Query<PageQuery>, QueryRejection>) -> Result<(u32, u64), ApiError> {
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request("invalid_admin_query", "admin query parameters are invalid")
    })?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    validate_page(limit, offset)?;
    Ok((limit, offset))
}

fn validate_page(limit: u32, offset: u64) -> Result<(), ApiError> {
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > MAX_OFFSET {
        Err(ApiError::bad_request(
            "invalid_admin_pagination",
            "limit must be 1-200 and offset must not exceed 1000000",
        ))
    } else {
        Ok(())
    }
}

fn validate_filter(value: Option<String>, name: &str) -> Result<Option<String>, ApiError> {
    value
        .map(|value| {
            if value.is_empty()
                || value.len() > 128
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                Err(ApiError::bad_request(
                    "invalid_audit_filter",
                    format!("audit {name} filter is invalid"),
                ))
            } else {
                Ok(value)
            }
        })
        .transpose()
}

fn parse_user_id(value: &str) -> Result<UserId, ApiError> {
    UserId::from_str(value)
        .map_err(|_| ApiError::bad_request("invalid_user_id", "user ID is invalid"))
}

fn required_request_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| ApiError::bad_request("invalid_request_id", "x-request-id is invalid"))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProtocolObservationListQuery {
    provider_id: Option<String>,
    kind: Option<String>,
    limit: Option<u32>,
    offset: Option<u64>,
}

pub(super) struct CreateUserRequest {
    username: String,
    password: SecretString,
    status: Option<UserStatus>,
    roles: BTreeSet<Role>,
    permissions: BTreeSet<Permission>,
}

impl fmt::Debug for CreateUserRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateUserRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("status", &self.status)
            .field("roles", &self.roles)
            .field("permissions", &self.permissions)
            .finish()
    }
}

impl<'de> Deserialize<'de> for CreateUserRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            username: String,
            password: String,
            status: Option<UserStatus>,
            roles: BTreeSet<Role>,
            #[serde(default)]
            permissions: BTreeSet<Permission>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            username: wire.username,
            password: SecretString::new(wire.password),
            status: wire.status,
            roles: wire.roles,
            permissions: wire.permissions,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateUserRequest {
    expected_updated_at: Timestamp,
    status: UserStatus,
    roles: BTreeSet<Role>,
    #[serde(default)]
    permissions: BTreeSet<Permission>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuditListQuery {
    action: Option<String>,
    resource_type: Option<String>,
    resource_id: Option<String>,
    outcome: Option<String>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct UserProfilePageResponse {
    items: Vec<asterism_domain::UserProfile>,
    total: u64,
    limit: u32,
    offset: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct AuditPageResponse {
    items: Vec<asterism_domain::AuditRecord>,
    total: u64,
    limit: u32,
    offset: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProtocolObservationPageResponse {
    items: Vec<asterism_domain::ProtocolObservation>,
    total: u64,
    limit: u32,
    offset: u64,
}
