use std::str::FromStr;

use asterism_domain::{ProviderAccountId, Task, TaskId};
use asterism_storage::{SqliteTaskQueryRepository, TaskQueryRepository};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;

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

fn parse_provider_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "provider account ID is invalid",
        )
    })
}
