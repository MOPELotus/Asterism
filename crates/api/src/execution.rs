use std::{future::pending, str::FromStr, time::Duration};

use asterism_domain::{
    Execution, ExecutionAttempt, ExecutionId, ExecutionLogEvent, ExecutionProgress,
    QuestionSnapshotId, TaskId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_storage::{ExecutionQueryRepository, SqliteExecutionRepository};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderName, HeaderValue},
    response::{IntoResponse, Response, Sse, sse::Event, sse::KeepAlive},
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_stream::wrappers::BroadcastStream;

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_LOG_PAGE_SIZE: u32 = 50;
const MAX_LOG_PAGE_SIZE: u32 = 200;
const MAX_LOG_OFFSET: u64 = 1_000_000;
const DEFAULT_EXECUTION_PAGE_SIZE: u32 = 50;
const MAX_EXECUTION_PAGE_SIZE: u32 = 200;
const MAX_EXECUTION_OFFSET: u64 = 1_000_000;
const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");

pub(super) async fn list_executions(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<ExecutionListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_execution_query",
            "execution query parameters have an invalid format",
        )
    })?;
    let task_id = query
        .task_id
        .as_deref()
        .map(TaskId::from_str)
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let limit = query.limit.unwrap_or(DEFAULT_EXECUTION_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > MAX_EXECUTION_PAGE_SIZE || offset > MAX_EXECUTION_OFFSET {
        return Err(ApiError::bad_request(
            "invalid_execution_pagination",
            "execution limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    let page = SqliteExecutionRepository::new(state.database)
        .list_owned_executions(owner_id, task_id, limit, offset)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(ExecutionPageResponse {
            total: page.total,
            limit,
            offset,
            items: page.items,
        })
        .into_response(),
    ))
}

pub(super) async fn get_execution(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(execution_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let execution_id = ExecutionId::from_str(&execution_id)
        .map_err(|_| ApiError::bad_request("invalid_execution_id", "execution ID is invalid"))?;
    let detail = SqliteExecutionRepository::new(state.database.clone())
        .find_owned_execution_detail(owner_id, execution_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("execution_not_found"))?;
    let next_question_snapshot_id =
        find_next_question_snapshot_id(&state.database, execution_id).await?;
    let ai_selection = sqlx::query_as::<_, (String, String)>(
        "SELECT profile, route FROM execution_ai_selections WHERE execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(ExecutionDetailResponse {
            execution: detail.execution,
            progress: detail.progress,
            attempts: detail.attempts,
            next_question_snapshot_id,
            ai_selection,
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

pub(super) async fn stream_execution(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(execution_id): Path<String>,
) -> Result<Response, ApiError> {
    // Subscribe before loading the snapshot so an event committed between the
    // ownership check and response construction remains visible to the client.
    let receiver = state.events.subscribe();
    let shutdown = state.stream_shutdown.clone();
    let owner_id = auth.require_task_read()?;
    let execution_id = ExecutionId::from_str(&execution_id)
        .map_err(|_| ApiError::bad_request("invalid_execution_id", "execution ID is invalid"))?;
    let detail = SqliteExecutionRepository::new(state.database.clone())
        .find_owned_execution_detail(owner_id, execution_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("execution_not_found"))?;
    let next_question_snapshot_id =
        find_next_question_snapshot_id(&state.database, execution_id).await?;
    let ai_selection = sqlx::query_as::<_, (String, String)>(
        "SELECT profile, route FROM execution_ai_selections WHERE execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(state.database.pool())
    .await
    .map_err(ApiError::internal)?;
    let snapshot = ExecutionDetailResponse {
        execution: detail.execution,
        progress: detail.progress,
        attempts: detail.attempts,
        next_question_snapshot_id,
        ai_selection,
    };
    let snapshot = Event::default()
        .event("snapshot")
        .data(serde_json::to_string(&snapshot).map_err(ApiError::internal)?);

    let live =
        tokio_stream::StreamExt::filter_map(BroadcastStream::new(receiver), move |received| {
            match received {
                Ok(envelope) => match execution_stream_event(&envelope, execution_id) {
                    Ok(Some(event)) => Some(Ok(event)),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                },
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                    Some(Ok(Event::default().event("resync").data(
                        serde_json::json!({"reason": "lagged", "skipped": skipped}).to_string(),
                    )))
                }
            }
        });
    let stream = tokio_stream::StreamExt::chain(
        tokio_stream::once(Ok::<_, serde_json::Error>(snapshot)),
        live,
    );
    let stream = futures_util::StreamExt::take_until(stream, wait_for_stream_shutdown(shutdown));
    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response();
    response
        .headers_mut()
        .insert(X_ACCEL_BUFFERING, HeaderValue::from_static("no"));
    Ok(crate::auth::no_store(response))
}

fn execution_stream_event(
    envelope: &EventEnvelope,
    execution_id: ExecutionId,
) -> Result<Option<Event>, serde_json::Error> {
    let name = match &envelope.event {
        DomainEvent::ExecutionStateChanged {
            execution_id: event_execution_id,
            ..
        } if *event_execution_id == execution_id => "execution_state",
        DomainEvent::ExecutionProgressed(progress) if progress.execution_id == execution_id => {
            "execution_progress"
        }
        DomainEvent::ExecutionLogged(log) if log.execution_id == execution_id => "execution_log",
        DomainEvent::ExecutionRecoveryRequired {
            execution_id: event_execution_id,
            ..
        } if *event_execution_id == execution_id => "execution_recovery_required",
        DomainEvent::HumanRequired {
            execution_id: Some(event_execution_id),
            ..
        } if *event_execution_id == execution_id => "human_required",
        _ => return Ok(None),
    };
    Ok(Some(
        Event::default()
            .event(name)
            .id(envelope.id.to_string())
            .data(serde_json::to_string(envelope)?),
    ))
}

async fn wait_for_stream_shutdown(mut shutdown: Option<watch::Receiver<bool>>) {
    let Some(shutdown) = shutdown.as_mut() else {
        pending::<()>().await;
        return;
    };
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            return;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecutionDetailResponse {
    execution: Execution,
    progress: Option<ExecutionProgress>,
    attempts: Vec<ExecutionAttempt>,
    next_question_snapshot_id: Option<QuestionSnapshotId>,
    ai_selection: Option<(String, String)>,
}

async fn find_next_question_snapshot_id(
    database: &asterism_storage::Database,
    execution_id: ExecutionId,
) -> Result<Option<QuestionSnapshotId>, ApiError> {
    let value = sqlx::query_scalar::<_, String>(
        "SELECT next_question_snapshot_id FROM question_session_transitions \
         WHERE execution_id = ? ORDER BY transitioned_at DESC LIMIT 1",
    )
    .bind(execution_id.to_string())
    .fetch_optional(database.pool())
    .await
    .map_err(ApiError::internal)?;
    value
        .map(|value| {
            QuestionSnapshotId::from_str(&value)
                .map_err(|_| ApiError::internal("stored next Question snapshot ID is invalid"))
        })
        .transpose()
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecutionListQuery {
    task_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecutionPageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<Execution>,
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
