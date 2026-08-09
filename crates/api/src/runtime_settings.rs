use std::{collections::BTreeMap, str::FromStr};

use asterism_domain::{
    ProviderAccount, ProviderAccountId, ProviderId, ProviderRuntimeSettingsId, TaskId, Timestamp,
};
use asterism_provider_api::{
    ProviderRuntimeSettingSource, ProviderRuntimeSettingsPatch, ProviderRuntimeSettingsSchema,
    ProviderSettingScope, ProviderSettingValue, ResolvedProviderRuntimeSettings,
};
use asterism_storage::{
    ProviderAccountRuntimeRepository, ProviderRuntimeSettingsRecord,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget,
    ProviderRuntimeSettingsWriteOutcome, ProviderRuntimeSettingsWriteRequest,
    SqliteProviderAccountRepository, SqliteProviderRuntimeSettingsRepository,
    SqliteTaskQueryRepository, TaskQueryRepository, TaskRuntimeRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    ApiError, ApiState,
    auth::{AuthContext, ProviderSettingsAuthority},
};

pub(super) async fn get_provider_runtime_settings_schema(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(provider_id): Path<String>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    let context = provider_context(&state, &provider_id)?;
    Ok(crate::auth::no_store(
        Json(ProviderRuntimeSettingsSchemaResponse {
            provider_id: context.provider_id,
            schema: context.schema,
        })
        .into_response(),
    ))
}

pub(super) async fn get_provider_runtime_settings(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(provider_id): Path<String>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    let context = provider_context(&state, &provider_id)?;
    settings_response(&state, &context).await
}

pub(super) async fn put_provider_runtime_settings(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<PutProviderRuntimeSettingsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    require_system_authority(auth.require_provider_settings_manage()?)?;
    let context = provider_context(&state, &provider_id)?;
    write_settings(state, auth, context, headers, payload).await
}

pub(super) async fn get_account_runtime_settings(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let authority = auth.require_provider_settings_manage()?;
    let context = account_context(&state, authority, &account_id).await?;
    settings_response(&state, &context).await
}

pub(super) async fn put_account_runtime_settings(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<PutProviderRuntimeSettingsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let authority = auth.require_provider_settings_manage()?;
    let context = account_context(&state, authority, &account_id).await?;
    write_settings(state, auth, context, headers, payload).await
}

pub(super) async fn get_task_runtime_settings(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
) -> Result<Response, ApiError> {
    let authority = auth.require_provider_settings_manage()?;
    let context = task_context(&state, authority, &task_id).await?;
    settings_response(&state, &context).await
}

pub(super) async fn put_task_runtime_settings(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<PutProviderRuntimeSettingsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let authority = auth.require_provider_settings_manage()?;
    let context = task_context(&state, authority, &task_id).await?;
    write_settings(state, auth, context, headers, payload).await
}

async fn write_settings(
    state: ApiState,
    auth: AuthContext,
    context: SettingsContext,
    headers: HeaderMap,
    payload: Result<Json<PutProviderRuntimeSettingsRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request(
            "invalid_runtime_settings_json",
            "the runtime settings request must match the Provider schema wire format",
        )
    })?;
    let patch = ProviderRuntimeSettingsPatch {
        schema_version: request.schema_version,
        values: request.values,
    };
    let correlation_id = request_id(&headers)?;
    let repository = SqliteProviderRuntimeSettingsRepository::new(state.database.clone());
    let outcome = repository
        .write_provider_runtime_settings(ProviderRuntimeSettingsWriteRequest {
            target: context.target(),
            expected_revision: request.expected_revision,
            patch: &patch,
            schema: &context.schema,
            actor: auth.audit_actor(),
            correlation_id,
            updated_at: Utc::now(),
        })
        .await
        .map_err(map_settings_storage_error)?;
    match outcome {
        ProviderRuntimeSettingsWriteOutcome::Stored(record) => {
            let created = record.revision == 1;
            let response = build_settings_response(&repository, &context).await?;
            Ok(crate::auth::no_store(
                (
                    if created {
                        StatusCode::CREATED
                    } else {
                        StatusCode::OK
                    },
                    Json(response),
                )
                    .into_response(),
            ))
        }
        ProviderRuntimeSettingsWriteOutcome::TargetNotFound => {
            Err(ApiError::not_found("runtime_settings_target_not_found"))
        }
        ProviderRuntimeSettingsWriteOutcome::RevisionConflict => Err(ApiError::conflict(
            "runtime_settings_revision_conflict",
            "the runtime settings revision changed; reload before saving",
        )),
    }
}

async fn settings_response(
    state: &ApiState,
    context: &SettingsContext,
) -> Result<Response, ApiError> {
    let repository = SqliteProviderRuntimeSettingsRepository::new(state.database.clone());
    Ok(crate::auth::no_store(
        Json(build_settings_response(&repository, context).await?).into_response(),
    ))
}

async fn build_settings_response(
    repository: &SqliteProviderRuntimeSettingsRepository,
    context: &SettingsContext,
) -> Result<ProviderRuntimeSettingsResponse, ApiError> {
    let provider = repository
        .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Provider {
            provider_id: context.provider_id.clone(),
        })
        .await
        .map_err(ApiError::internal)?;
    let account = if let Some(provider_account_id) = context.provider_account_id {
        repository
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id: context.provider_id.clone(),
                provider_account_id,
            })
            .await
            .map_err(ApiError::internal)?
    } else {
        None
    };
    let task = if let (Some(provider_account_id), Some(task_id)) =
        (context.provider_account_id, context.task_id)
    {
        repository
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Task {
                provider_id: context.provider_id.clone(),
                provider_account_id,
                task_id,
            })
            .await
            .map_err(ApiError::internal)?
    } else {
        None
    };
    let resolved = context
        .schema
        .resolve(
            provider.as_ref().map(|record| &record.patch),
            account.as_ref().map(|record| &record.patch),
            task.as_ref().map(|record| &record.patch),
        )
        .map_err(|_| {
            ApiError::conflict(
                "runtime_settings_schema_mismatch",
                "a stored override no longer matches the registered Provider schema",
            )
        })?;
    let sources = resolved_sources(
        &context.schema,
        provider.as_ref(),
        account.as_ref(),
        task.as_ref(),
    );
    Ok(ProviderRuntimeSettingsResponse {
        provider_id: context.provider_id.clone(),
        provider_account_id: context.provider_account_id,
        task_id: context.task_id,
        target_scope: context.target().scope(),
        schema: context.schema.clone(),
        overrides: ProviderRuntimeSettingsLayers {
            provider: provider.as_ref().map(ProviderRuntimeSettingsOverride::from),
            provider_account: account.as_ref().map(ProviderRuntimeSettingsOverride::from),
            task: task.as_ref().map(ProviderRuntimeSettingsOverride::from),
        },
        resolved,
        sources,
    })
}

fn resolved_sources(
    schema: &ProviderRuntimeSettingsSchema,
    provider: Option<&ProviderRuntimeSettingsRecord>,
    account: Option<&ProviderRuntimeSettingsRecord>,
    task: Option<&ProviderRuntimeSettingsRecord>,
) -> BTreeMap<String, ProviderRuntimeSettingSource> {
    let mut sources = schema
        .definitions
        .iter()
        .map(|definition| {
            (
                definition.key.clone(),
                ProviderRuntimeSettingSource::SchemaDefault,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (record, source) in [
        (provider, ProviderRuntimeSettingSource::Provider),
        (account, ProviderRuntimeSettingSource::ProviderAccount),
        (task, ProviderRuntimeSettingSource::Task),
    ] {
        if let Some(record) = record {
            for key in record.patch.values.keys() {
                sources.insert(key.clone(), source);
            }
        }
    }
    sources
}

fn provider_context(state: &ApiState, provider_id: &str) -> Result<SettingsContext, ApiError> {
    let provider_id = parse_provider_id(provider_id)?;
    let provider = state
        .providers
        .get(&provider_id)
        .ok_or_else(|| ApiError::not_found("provider_not_found"))?;
    Ok(SettingsContext {
        provider_id,
        provider_account_id: None,
        task_id: None,
        schema: provider.runtime_settings.clone(),
    })
}

async fn account_context(
    state: &ApiState,
    authority: ProviderSettingsAuthority,
    account_id: &str,
) -> Result<SettingsContext, ApiError> {
    let account_id = parse_account_id(account_id)?;
    let account = authorized_account(state, authority, account_id).await?;
    let provider = state.providers.get(&account.provider_id).ok_or_else(|| {
        ApiError::conflict(
            "provider_not_registered",
            "the account Provider is not registered",
        )
    })?;
    Ok(SettingsContext {
        provider_id: account.provider_id,
        provider_account_id: Some(account.id),
        task_id: None,
        schema: provider.runtime_settings.clone(),
    })
}

async fn task_context(
    state: &ApiState,
    authority: ProviderSettingsAuthority,
    task_id: &str,
) -> Result<SettingsContext, ApiError> {
    let task_id = parse_task_id(task_id)?;
    let tasks = SqliteTaskQueryRepository::new(state.database.clone());
    let task = match authority {
        ProviderSettingsAuthority::System => tasks.find_runtime_task(task_id).await,
        ProviderSettingsAuthority::Owner(owner_id) => {
            tasks.find_owned_task(owner_id, task_id).await
        }
    }
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("task_not_found"))?;
    let account = authorized_account(state, authority, task.provider_account_id).await?;
    let provider = state.providers.get(&account.provider_id).ok_or_else(|| {
        ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        )
    })?;
    Ok(SettingsContext {
        provider_id: account.provider_id,
        provider_account_id: Some(account.id),
        task_id: Some(task.id),
        schema: provider.runtime_settings.clone(),
    })
}

async fn authorized_account(
    state: &ApiState,
    authority: ProviderSettingsAuthority,
    account_id: ProviderAccountId,
) -> Result<ProviderAccount, ApiError> {
    SqliteProviderAccountRepository::new(state.database.clone())
        .find_runtime_provider_account(account_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|account| authority.permits_owner(account.owner_id))
        .ok_or_else(|| ApiError::not_found("provider_account_not_found"))
}

fn require_system_authority(authority: ProviderSettingsAuthority) -> Result<(), ApiError> {
    authority
        .is_system()
        .then_some(())
        .ok_or_else(ApiError::forbidden)
}

fn parse_provider_id(value: &str) -> Result<ProviderId, ApiError> {
    ProviderId::new(value.to_owned())
        .map_err(|_| ApiError::bad_request("invalid_provider_id", "Provider ID is invalid"))
}

fn parse_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "provider account ID is invalid",
        )
    })
}

fn parse_task_id(value: &str) -> Result<TaskId, ApiError> {
    TaskId::from_str(value)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))
}

fn request_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::internal("request ID middleware did not provide an ID"))
}

fn map_settings_storage_error(error: asterism_storage::StorageError) -> ApiError {
    match error {
        asterism_storage::StorageError::InvalidData(message)
            if message.starts_with("provider runtime settings") =>
        {
            ApiError::bad_request("invalid_runtime_settings", message)
        }
        error => ApiError::internal(error),
    }
}

#[derive(Clone, Debug)]
struct SettingsContext {
    provider_id: ProviderId,
    provider_account_id: Option<ProviderAccountId>,
    task_id: Option<TaskId>,
    schema: ProviderRuntimeSettingsSchema,
}

impl SettingsContext {
    fn target(&self) -> ProviderRuntimeSettingsTarget {
        match (self.provider_account_id, self.task_id) {
            (None, None) => ProviderRuntimeSettingsTarget::Provider {
                provider_id: self.provider_id.clone(),
            },
            (Some(provider_account_id), None) => ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id: self.provider_id.clone(),
                provider_account_id,
            },
            (Some(provider_account_id), Some(task_id)) => ProviderRuntimeSettingsTarget::Task {
                provider_id: self.provider_id.clone(),
                provider_account_id,
                task_id,
            },
            (None, Some(_)) => unreachable!("Task settings always carry a Provider account"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PutProviderRuntimeSettingsRequest {
    expected_revision: u32,
    schema_version: u32,
    values: BTreeMap<String, ProviderSettingValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProviderRuntimeSettingsSchemaResponse {
    provider_id: ProviderId,
    schema: ProviderRuntimeSettingsSchema,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProviderRuntimeSettingsResponse {
    provider_id: ProviderId,
    provider_account_id: Option<ProviderAccountId>,
    task_id: Option<TaskId>,
    target_scope: ProviderSettingScope,
    schema: ProviderRuntimeSettingsSchema,
    overrides: ProviderRuntimeSettingsLayers,
    resolved: ResolvedProviderRuntimeSettings,
    sources: BTreeMap<String, ProviderRuntimeSettingSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProviderRuntimeSettingsLayers {
    provider: Option<ProviderRuntimeSettingsOverride>,
    provider_account: Option<ProviderRuntimeSettingsOverride>,
    task: Option<ProviderRuntimeSettingsOverride>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProviderRuntimeSettingsOverride {
    id: ProviderRuntimeSettingsId,
    patch: ProviderRuntimeSettingsPatch,
    revision: u32,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl From<&ProviderRuntimeSettingsRecord> for ProviderRuntimeSettingsOverride {
    fn from(record: &ProviderRuntimeSettingsRecord) -> Self {
        Self {
            id: record.id,
            patch: record.patch.clone(),
            revision: record.revision,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}
