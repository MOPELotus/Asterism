use std::str::FromStr;

use asterism_domain::{Execution, ProviderAccountId, ProviderId, Task, TaskId};
use asterism_engine::{
    ExecuteTaskCommand, ExecutionRequestError, ExecutionRequestService, FormalAssessmentPolicy,
    ProviderTaskDetailError, ProviderTaskDetailService, ReadTaskDetailCommand,
};
use asterism_provider_api::{ProviderErrorKind, RemoteTaskDetail};
use asterism_storage::{
    SqliteExecutionRepository, SqliteProviderAccountRepository,
    SqliteProviderRuntimeSettingsRepository, SqliteTaskQueryRepository, TaskQueryRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;
const IDEMPOTENCY_KEY: &str = "idempotency-key";

pub(super) async fn list_tasks(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<TaskListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_task_query",
            "task query parameters have an invalid format",
        )
    })?;
    let provider_account_id = query
        .provider_account_id
        .as_deref()
        .map(parse_provider_account_id)
        .transpose()?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > MAX_OFFSET {
        return Err(ApiError::bad_request(
            "invalid_task_pagination",
            "task limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    let page = SqliteTaskQueryRepository::new(state.database)
        .list_owned_tasks(owner_id, provider_account_id, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(TaskPageResponse {
            total: page.total,
            limit,
            offset,
            items: page.items,
        })
        .into_response(),
    ))
}

pub(super) async fn get_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let task = SqliteTaskQueryRepository::new(state.database)
        .find_owned_task(owner_id, task_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("task_not_found"))?;
    Ok(crate::auth::no_store(Json(task).into_response()))
}

pub(super) async fn get_task_detail(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let result = ProviderTaskDetailService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database),
    )
    .read(ReadTaskDetailCommand {
        owner_id,
        task_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_task_detail_error)?;
    Ok(crate::auth::no_store(
        Json(TaskDetailResponse {
            task_id: result.task_id,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            detail: result.detail,
        })
        .into_response(),
    ))
}

pub(super) async fn execute_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (owner_id, request_source) = auth.require_task_execute()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let idempotency_key = required_header(&headers, IDEMPOTENCY_KEY, 256)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let service = ExecutionRequestService::new(
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteExecutionRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteProviderRuntimeSettingsRepository::new(state.database),
        state.providers,
        FormalAssessmentPolicy::default(),
    );
    let result = service
        .execute(ExecuteTaskCommand {
            owner_id,
            task_id,
            request_source,
            actor: auth.audit_actor(),
            idempotency_key: idempotency_key.to_owned(),
            correlation_id: correlation_id.to_owned(),
            requested_at: Utc::now(),
        })
        .await
        .map_err(map_execution_request_error)?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(ExecuteTaskResponse {
                execution: result.execution,
                created: result.created,
            }),
        )
            .into_response(),
    ))
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    max_bytes: usize,
) -> Result<&'a str, ApiError> {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "missing_required_header",
                format!("the {name} header is required"),
            )
        })?;
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "invalid_required_header",
            format!("the {name} header is invalid"),
        ));
    }
    Ok(value)
}

fn map_execution_request_error(error: ExecutionRequestError) -> ApiError {
    match error {
        ExecutionRequestError::TaskNotFound => ApiError::not_found("task_not_found"),
        ExecutionRequestError::TaskStateConflict => ApiError::conflict(
            "task_state_conflict",
            "the task orchestration state is not executable or changed concurrently",
        ),
        ExecutionRequestError::RemoteStateNotExecutable => ApiError::conflict(
            "task_remote_state_not_executable",
            "the current remote task state is not executable",
        ),
        ExecutionRequestError::UnsupportedTask => ApiError::conflict(
            "task_execution_unsupported",
            "the task advertises no executable Provider capability",
        ),
        ExecutionRequestError::IdempotencyConflict => ApiError::conflict(
            "idempotency_conflict",
            "the idempotency key is already bound to another execution request",
        ),
        ExecutionRequestError::ProviderRuntimeUnavailable => ApiError::conflict(
            "provider_runtime_settings_unavailable",
            "the registered Provider runtime settings are unavailable or incompatible",
        ),
        ExecutionRequestError::RuntimeSettingsConflict => ApiError::conflict(
            "runtime_settings_revision_conflict",
            "Provider runtime settings changed while the execution was being scheduled",
        ),
        ExecutionRequestError::Assessment(_) => ApiError::conflict(
            "formal_assessment_blocked",
            "formal assessment execution is disabled by Core policy",
        ),
        ExecutionRequestError::Transition(_) | ExecutionRequestError::Storage(_) => {
            ApiError::internal(error)
        }
    }
}

fn map_task_detail_error(error: ProviderTaskDetailError) -> ApiError {
    match error {
        ProviderTaskDetailError::TaskNotFound => ApiError::not_found("task_not_found"),
        ProviderTaskDetailError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before reading task detail",
        ),
        ProviderTaskDetailError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        ProviderTaskDetailError::CapabilityUnavailable(_) => ApiError::conflict(
            "provider_task_detail_unavailable",
            "the Provider exposes no Task Detail capability",
        ),
        ProviderTaskDetailError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        ProviderTaskDetailError::ProviderResponseInvalid => {
            tracing::warn!(%error, "Provider returned invalid Task detail");
            ApiError::bad_gateway(
                "provider_task_detail_invalid",
                "the Provider returned inconsistent task detail",
            )
        }
        ProviderTaskDetailError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider Task detail is temporarily unavailable");
                ApiError::service_unavailable(
                    "provider_unavailable",
                    "the Provider is temporarily unavailable",
                )
            }
            ProviderErrorKind::Authentication
            | ProviderErrorKind::Authorization
            | ProviderErrorKind::HumanRequired => ApiError::conflict(
                "provider_action_required",
                "the Provider requires authentication or user action",
            ),
            ProviderErrorKind::RemoteChanged => ApiError::conflict(
                "task_remote_changed",
                "the remote task no longer matches the stored task",
            ),
            ProviderErrorKind::UnsupportedTask => ApiError::conflict(
                "provider_task_detail_unavailable",
                "the Provider cannot read detail for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid Task detail");
                ApiError::bad_gateway(
                    "provider_task_detail_invalid",
                    "the Provider returned inconsistent task detail",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderTaskDetailError::Storage(error) => ApiError::internal(error),
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskListQuery {
    provider_account_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct TaskPageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<Task>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecuteTaskResponse {
    execution: Execution,
    created: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct TaskDetailResponse {
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    detail: RemoteTaskDetail,
}

fn parse_provider_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "provider account ID is invalid",
        )
    })
}
