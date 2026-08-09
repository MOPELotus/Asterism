use std::str::FromStr;

use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, Execution, ProviderAccountId, ProviderId, Question,
    QuestionSnapshotId, Task, TaskId, Timestamp,
};
use asterism_engine::{
    ExecuteTaskCommand, ExecutionRequestError, ExecutionRequestService, FormalAssessmentPolicy,
    ProviderAnswerResolveError, ProviderAnswerResolveService, ProviderQuestionReadError,
    ProviderQuestionReadService, ProviderTaskDetailError, ProviderTaskDetailService,
    ProviderTaskProgressError, ProviderTaskProgressService, ReadTaskDetailCommand,
    ReadTaskProgressCommand, ReadTaskQuestionsCommand, ResolveProviderAnswersCommand,
};
use asterism_provider_api::{ProviderErrorKind, RemoteProgress, RemoteTaskDetail};
use asterism_storage::{
    SqliteExecutionRepository, SqliteProviderAccountRepository,
    SqliteProviderRuntimeSettingsRepository, SqliteQuestionSnapshotRepository,
    SqliteTaskQueryRepository, TaskQueryRepository,
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

pub(super) async fn get_task_progress(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let result = ProviderTaskProgressService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database),
    )
    .read(ReadTaskProgressCommand {
        owner_id,
        task_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_task_progress_error)?;
    Ok(crate::auth::no_store(
        Json(TaskProgressResponse {
            task_id: result.task_id,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            progress: result.progress,
        })
        .into_response(),
    ))
}

pub(super) async fn get_task_questions(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let result = ProviderQuestionReadService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteQuestionSnapshotRepository::new(state.database),
    )
    .read(ReadTaskQuestionsCommand {
        owner_id,
        task_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_task_questions_error)?;
    Ok(crate::auth::no_store(
        Json(TaskQuestionsResponse {
            snapshot_id: result.snapshot_id,
            task_id: result.task_id,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            captured_at: result.captured_at,
            questions: result.questions,
        })
        .into_response(),
    ))
}

pub(super) async fn resolve_provider_answer_candidates(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let snapshot_id = QuestionSnapshotId::from_str(&snapshot_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_question_snapshot_id",
            "Question snapshot ID is invalid",
        )
    })?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let answers = SqliteQuestionSnapshotRepository::new(state.database.clone());
    let result = ProviderAnswerResolveService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database),
        answers,
    )
    .resolve(ResolveProviderAnswersCommand {
        owner_id,
        task_id,
        question_snapshot_id: snapshot_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_provider_answer_resolve_error)?;
    Ok(crate::auth::no_store(
        Json(ProviderAnswerCandidatesResponse {
            task_id: result.task_id,
            question_snapshot_id: result.question_snapshot_id,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            candidates: result
                .candidates
                .into_iter()
                .map(|record| AnswerCandidateResponse {
                    id: record.id,
                    candidate: record.candidate,
                    created_at: record.created_at,
                })
                .collect(),
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

fn map_task_progress_error(error: ProviderTaskProgressError) -> ApiError {
    match error {
        ProviderTaskProgressError::TaskNotFound => ApiError::not_found("task_not_found"),
        ProviderTaskProgressError::TaskCapabilityUnavailable
        | ProviderTaskProgressError::CapabilityUnavailable(_) => ApiError::conflict(
            "task_progress_unavailable",
            "the task does not expose readable remote progress",
        ),
        ProviderTaskProgressError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before reading task progress",
        ),
        ProviderTaskProgressError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        ProviderTaskProgressError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        ProviderTaskProgressError::ProviderResponseInvalid => {
            tracing::warn!(%error, "Provider returned invalid Task progress");
            ApiError::bad_gateway(
                "provider_task_progress_invalid",
                "the Provider returned inconsistent task progress",
            )
        }
        ProviderTaskProgressError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider Task progress is temporarily unavailable");
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
                "task_progress_unavailable",
                "the Provider cannot read progress for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid Task progress");
                ApiError::bad_gateway(
                    "provider_task_progress_invalid",
                    "the Provider returned inconsistent task progress",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderTaskProgressError::Storage(error) => ApiError::internal(error),
    }
}

fn map_task_questions_error(error: ProviderQuestionReadError) -> ApiError {
    match error {
        ProviderQuestionReadError::TaskNotFound => ApiError::not_found("task_not_found"),
        ProviderQuestionReadError::TaskCapabilityUnavailable
        | ProviderQuestionReadError::CapabilityUnavailable(_) => ApiError::conflict(
            "task_questions_unavailable",
            "the task does not expose a complete Question read pipeline",
        ),
        ProviderQuestionReadError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before reading task Questions",
        ),
        ProviderQuestionReadError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        ProviderQuestionReadError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        ProviderQuestionReadError::ProviderResponseInvalid => {
            tracing::warn!(%error, "Provider returned invalid Questions");
            ApiError::bad_gateway(
                "provider_task_questions_invalid",
                "the Provider returned inconsistent task Questions",
            )
        }
        ProviderQuestionReadError::Assessment(_) => ApiError::conflict(
            "formal_assessment_blocked",
            "formal assessment Question reading is disabled by Core policy",
        ),
        ProviderQuestionReadError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider Questions are temporarily unavailable");
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
                "task_questions_unavailable",
                "the Provider cannot read Questions for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid Questions");
                ApiError::bad_gateway(
                    "provider_task_questions_invalid",
                    "the Provider returned inconsistent task Questions",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderQuestionReadError::Storage(error) => ApiError::internal(error),
    }
}

fn map_provider_answer_resolve_error(error: ProviderAnswerResolveError) -> ApiError {
    match error {
        ProviderAnswerResolveError::TaskNotFound => ApiError::not_found("task_not_found"),
        ProviderAnswerResolveError::QuestionSnapshotNotFound => {
            ApiError::not_found("question_snapshot_not_found")
        }
        ProviderAnswerResolveError::TaskCapabilityUnavailable
        | ProviderAnswerResolveError::CapabilityUnavailable(_) => ApiError::conflict(
            "provider_answer_resolve_unavailable",
            "the task does not expose Provider-native answer resolution",
        ),
        ProviderAnswerResolveError::QuestionSnapshotBindingInvalid => ApiError::conflict(
            "question_snapshot_binding_invalid",
            "the Question snapshot is not bound to this task and Provider",
        ),
        ProviderAnswerResolveError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before resolving candidates",
        ),
        ProviderAnswerResolveError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        ProviderAnswerResolveError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        ProviderAnswerResolveError::ProviderResponseInvalid => {
            tracing::warn!(%error, "Provider returned invalid AnswerCandidates");
            ApiError::bad_gateway(
                "provider_answer_candidates_invalid",
                "the Provider returned inconsistent answer candidates",
            )
        }
        ProviderAnswerResolveError::Assessment(_) => ApiError::conflict(
            "formal_assessment_blocked",
            "formal assessment answer resolution is disabled by Core policy",
        ),
        ProviderAnswerResolveError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider answer resolution is temporarily unavailable");
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
                "provider_answer_resolve_unavailable",
                "the Provider cannot resolve answers for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid AnswerCandidates");
                ApiError::bad_gateway(
                    "provider_answer_candidates_invalid",
                    "the Provider returned inconsistent answer candidates",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderAnswerResolveError::Storage(error) => ApiError::internal(error),
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskProgressResponse {
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    progress: RemoteProgress,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct TaskQuestionsResponse {
    snapshot_id: QuestionSnapshotId,
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    captured_at: Timestamp,
    questions: Vec<Question>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProviderAnswerCandidatesResponse {
    task_id: TaskId,
    question_snapshot_id: QuestionSnapshotId,
    provider_id: ProviderId,
    provider_version: String,
    candidates: Vec<AnswerCandidateResponse>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct AnswerCandidateResponse {
    id: AnswerCandidateId,
    candidate: AnswerCandidate,
    created_at: Timestamp,
}

fn parse_provider_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "provider account ID is invalid",
        )
    })
}
