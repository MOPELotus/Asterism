use std::str::FromStr;

use asterism_domain::{Execution, ExecutionAttempt, ExecutionId, ExecutionProgress};
use asterism_storage::{ExecutionQueryRepository, SqliteExecutionRepository};
use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{ApiError, ApiState, auth::AuthContext};

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecutionDetailResponse {
    execution: Execution,
    progress: Option<ExecutionProgress>,
    attempts: Vec<ExecutionAttempt>,
}
