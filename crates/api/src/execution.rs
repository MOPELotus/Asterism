use std::str::FromStr;

use asterism_domain::{
    Execution, ExecutionAttempt, ExecutionId, ExecutionLogEvent, ExecutionProgress,
};
use asterism_storage::{ExecutionQueryRepository, SqliteExecutionRepository};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_LOG_PAGE_SIZE: u32 = 50;
const MAX_LOG_PAGE_SIZE: u32 = 200;
const MAX_LOG_OFFSET: u64 = 1_000_000;

pub(super) async fn get_execution(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(execution_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let execution_id = ExecutionId::from_str(&execution_id)
        .map_err(|_| ApiError::bad_request("invalid_execution_id", "execution ID is invalid"))?;
    let detail = SqliteExecutionRepository::new(state.database)
        .find_owned_execution_detail(owner_id, execution_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("execution_not_found"))?;
    Ok(crate::auth::no_store(
        Json(ExecutionDetailResponse {
            execution: detail.execution,
            progress: detail.progress,
            attempts: detail.attempts,
        })
        .into_response(),
    ))
}

pub(super) async fn list_execution_logs(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(execution_id): Path<String>,
    query: Result<Query<ExecutionLogQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let execution_id = ExecutionId::from_str(&execution_id)
        .map_err(|_| ApiError::bad_request("invalid_execution_id", "execution ID is invalid"))?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_execution_log_query",
            "execution log query parameters have an invalid format",
        )
    })?;
    let limit = query.limit.unwrap_or(DEFAULT_LOG_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > MAX_LOG_PAGE_SIZE || offset > MAX_LOG_OFFSET {
        return Err(ApiError::bad_request(
            "invalid_execution_log_pagination",
            "execution log limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    let page = SqliteExecutionRepository::new(state.database)
        .list_owned_execution_logs(owner_id, execution_id, limit, offset)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("execution_not_found"))?;
    Ok(crate::auth::no_store(
        Json(ExecutionLogPageResponse {
            total: page.total,
            limit,
            offset,
            items: page.items,
        })
        .into_response(),
    ))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecutionDetailResponse {
    execution: Execution,
    progress: Option<ExecutionProgress>,
    attempts: Vec<ExecutionAttempt>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecutionLogQuery {
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ExecutionLogPageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<ExecutionLogEvent>,
}
