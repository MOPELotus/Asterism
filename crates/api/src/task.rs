use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AnswerConfidence, Execution, NormalizedAnswer,
    ProviderAccountId, ProviderId, Question, QuestionId, QuestionSnapshotId,
    ScoreImprovementWorkflow, StrictCompletionWorkflow, StrictCompletionWorkflowId,
    SubmissionDraftId, SubmissionResultId, Task, TaskCapability, TaskId, TaskLifecycleAction,
    Timestamp,
};
use asterism_engine::{
    BuildSubmissionDraftCommand, ConservativeAnswerResolverError,
    ConservativeAnswerResolverService, CreateManualAnswerCandidateCommand, ExecuteTaskCommand,
    ExecutionRequestError, ExecutionRequestService, FormalAssessmentPolicy,
    ImportLocalAnswerCandidatesCommand, LocalAnswerCacheError, LocalAnswerCacheService,
    ManualAnswerCandidateError, ManualAnswerCandidateService, ProviderAnswerResolveError,
    ProviderAnswerResolveService, ProviderQuestionReadError, ProviderQuestionReadResult,
    ProviderQuestionReadService, ProviderTaskBrowserSessionError,
    ProviderTaskBrowserSessionService, ProviderTaskDetailError, ProviderTaskDetailService,
    ProviderTaskDurationError, ProviderTaskDurationService, ProviderTaskProgressError,
    ProviderTaskProgressService, ReadTaskBrowserSessionCommand, ReadTaskDetailCommand,
    ReadTaskDurationCommand, ReadTaskProgressCommand, ReadTaskQuestionsCommand,
    ResolveAnswerCandidatesCommand, ResolveProviderAnswersCommand, SubmissionDraftBuildError,
    SubmissionDraftBuildService, TaskLifecycleCommand, TaskLifecycleError, TaskLifecycleService,
};
use asterism_provider_api::{
    BrowserSessionSpec, ProviderErrorKind, RemoteDuration, RemoteProgress, RemoteTaskDetail,
};
use asterism_storage::{
    AnswerCandidateRepository, CompletionWorkflowRepository, ExecutionStrictCompletionRetryRequest,
    QuestionSnapshotRepository, SqliteCompletionWorkflowRepository, SqliteExecutionRepository,
    SqliteProviderAccountRepository, SqliteProviderRuntimeSettingsRepository,
    SqliteQuestionReadAttemptRepository, SqliteQuestionSnapshotRepository,
    SqliteTaskLifecycleRepository, SqliteTaskQueryRepository, SubmissionDraftRepository,
    SubmissionResultRepository, TaskQueryRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
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

pub(super) async fn get_task_completion_workflows(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    SqliteTaskQueryRepository::new(state.database.clone())
        .find_owned_task(owner_id, task_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("task_not_found"))?;
    let workflows = SqliteCompletionWorkflowRepository::new(state.database);
    let strict_completion = workflows
        .find_owned_strict_completion_workflow(owner_id, task_id)
        .await
        .map_err(ApiError::internal)?
        .map(|record| StrictCompletionWorkflowResponse {
            revision: record.revision,
            workflow: record.workflow,
        });
    let score_improvement = workflows
        .find_owned_score_improvement_workflow(owner_id, task_id)
        .await
        .map_err(ApiError::internal)?
        .map(|record| ScoreImprovementWorkflowResponse {
            revision: record.revision,
            workflow: record.workflow,
        });
    Ok(crate::auth::no_store(
        Json(TaskCompletionWorkflowsResponse {
            task_id,
            strict_completion,
            score_improvement,
        })
        .into_response(),
    ))
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

pub(super) async fn get_task_browser_session_spec(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let result = ProviderTaskBrowserSessionService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database),
    )
    .read(ReadTaskBrowserSessionCommand {
        owner_id,
        task_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_task_browser_session_error)?;
    Ok(crate::auth::no_store(
        Json(TaskBrowserSessionSpecResponse {
            task_id: result.task_id,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            spec: result.spec,
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

pub(super) async fn get_task_duration(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let result = ProviderTaskDurationService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database),
    )
    .read(ReadTaskDurationCommand {
        owner_id,
        task_id,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_task_duration_error)?;
    Ok(crate::auth::no_store(
        Json(TaskDurationResponse {
            task_id: result.task_id,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            duration: result.duration,
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
    let mut service = ProviderQuestionReadService::new(
        state.providers.clone(),
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteQuestionSnapshotRepository::new(state.database.clone()),
    );
    if let Some(secret_store) = state.secret_store.clone() {
        service = service.with_durable_flow(
            Arc::new(SqliteProviderRuntimeSettingsRepository::new(
                state.database.clone(),
            )),
            Arc::new(SqliteQuestionReadAttemptRepository::new(
                state.database.clone(),
            )),
            Arc::new(secret_store.clone()),
            Arc::new(secret_store),
        );
    }
    let result = service
        .read(ReadTaskQuestionsCommand {
            owner_id,
            task_id,
            correlation_id: correlation_id.to_owned(),
        })
        .await
        .map_err(map_task_questions_error)?;
    match result {
        ProviderQuestionReadResult::Questions {
            snapshot_id,
            task_id,
            provider_id,
            provider_version,
            captured_at,
            questions,
        } => Ok(crate::auth::no_store(
            Json(TaskQuestionsResponse {
                snapshot_id,
                task_id,
                provider_id,
                provider_version,
                captured_at,
                questions,
            })
            .into_response(),
        )),
        ProviderQuestionReadResult::Completed { .. } => Ok(crate::auth::no_store(
            StatusCode::NO_CONTENT.into_response(),
        )),
    }
}

pub(super) async fn get_task_question_snapshot(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let snapshot = SqliteQuestionSnapshotRepository::new(state.database)
        .find_owned_question_snapshot(owner_id, snapshot_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|snapshot| snapshot.task_id == task_id)
        .ok_or_else(|| ApiError::not_found("question_snapshot_not_found"))?;
    Ok(crate::auth::no_store(
        Json(TaskQuestionsResponse {
            task_id: snapshot.task_id,
            provider_id: snapshot.provider_id,
            provider_version: snapshot.provider_version,
            snapshot_id: snapshot.id,
            captured_at: snapshot.captured_at,
            questions: snapshot.questions,
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
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let answers = SqliteQuestionSnapshotRepository::new(state.database.clone());
    let mut service = ProviderAnswerResolveService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        answers,
    );
    if let Some(secret_store) = state.secret_store {
        service = service.with_question_session_artifacts(Arc::new(secret_store));
    }
    let result = service
        .resolve(ResolveProviderAnswersCommand {
            owner_id,
            task_id,
            question_snapshot_id: snapshot_id,
            correlation_id: correlation_id.to_owned(),
        })
        .await
        .map_err(map_provider_answer_resolve_error)?;
    Ok(crate::auth::no_store(
        Json(AnswerCandidatesResponse {
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

pub(super) async fn list_answer_candidates(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let answers = SqliteQuestionSnapshotRepository::new(state.database);
    let snapshot = answers
        .find_owned_question_snapshot(owner_id, snapshot_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|snapshot| snapshot.task_id == task_id)
        .ok_or_else(|| ApiError::not_found("question_snapshot_not_found"))?;
    let candidates = answers
        .list_owned_answer_candidates(owner_id, snapshot_id)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(AnswerCandidatesResponse {
            task_id,
            question_snapshot_id: snapshot.id,
            provider_id: snapshot.provider_id,
            provider_version: snapshot.provider_version,
            candidates: candidates
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

pub(super) async fn create_manual_answer_candidate(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
    payload: Result<Json<CreateManualAnswerCandidateRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request(
            "invalid_json",
            "the request body must be valid JSON with the expected fields",
        )
    })?;
    let question_id = QuestionId::from_str(&request.question_id)
        .map_err(|_| ApiError::bad_request("invalid_question_id", "Question ID is invalid"))?;
    let confidence = request
        .confidence_basis_points
        .map(AnswerConfidence::try_new)
        .transpose()
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_answer_confidence",
                "answer confidence must be between 0 and 10000 basis points",
            )
        })?;
    let record =
        ManualAnswerCandidateService::new(SqliteQuestionSnapshotRepository::new(state.database))
            .create(CreateManualAnswerCandidateCommand {
                owner_id,
                task_id,
                question_snapshot_id: snapshot_id,
                question_id,
                answer: request.answer,
                confidence,
                explanation: request.explanation,
            })
            .await
            .map_err(map_manual_answer_candidate_error)?;
    Ok(crate::auth::no_store(
        (
            StatusCode::CREATED,
            Json(AnswerCandidateResponse {
                id: record.id,
                candidate: record.candidate,
                created_at: record.created_at,
            }),
        )
            .into_response(),
    ))
}

pub(super) async fn import_local_answer_candidates(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let candidates =
        LocalAnswerCacheService::new(SqliteQuestionSnapshotRepository::new(state.database))
            .import(ImportLocalAnswerCandidatesCommand {
                owner_id,
                task_id,
                question_snapshot_id: snapshot_id,
            })
            .await
            .map_err(map_local_answer_cache_error)?;
    Ok(crate::auth::no_store(
        Json(LocalAnswerCacheImportResponse {
            task_id,
            question_snapshot_id: snapshot_id,
            candidates: candidates
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

pub(super) async fn resolve_answer_candidates(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let plan = ConservativeAnswerResolverService::new(SqliteQuestionSnapshotRepository::new(
        state.database,
    ))
    .resolve(ResolveAnswerCandidatesCommand {
        owner_id,
        task_id,
        question_snapshot_id: snapshot_id,
    })
    .await
    .map_err(map_conservative_answer_resolver_error)?;
    Ok(crate::auth::no_store(Json(plan).into_response()))
}

pub(super) async fn build_submission_draft(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<BuildSubmissionDraftRequest>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let answer_candidate_ids = request
        .answer_candidate_ids
        .iter()
        .map(|candidate_id| {
            AnswerCandidateId::from_str(candidate_id).map_err(|_| {
                ApiError::bad_request(
                    "invalid_answer_candidate_id",
                    "an AnswerCandidate ID is invalid",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let submissions = SqliteQuestionSnapshotRepository::new(state.database.clone());
    let result = SubmissionDraftBuildService::new(
        state.providers,
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        submissions,
        SqliteProviderRuntimeSettingsRepository::new(state.database),
    )
    .build(BuildSubmissionDraftCommand {
        owner_id,
        task_id,
        question_snapshot_id: snapshot_id,
        answer_candidate_ids,
        correlation_id: correlation_id.to_owned(),
    })
    .await
    .map_err(map_submission_draft_build_error)?;
    Ok(crate::auth::no_store(
        (StatusCode::CREATED, Json(result.draft)).into_response(),
    ))
}

pub(super) async fn get_submission_draft(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id, draft_id)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let draft_id = SubmissionDraftId::from_str(&draft_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_submission_draft_id",
            "Submission draft ID is invalid",
        )
    })?;
    let draft = SqliteQuestionSnapshotRepository::new(state.database)
        .find_owned_submission_draft(owner_id, draft_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|draft| draft.task_id == task_id && draft.question_snapshot_id == snapshot_id)
        .ok_or_else(|| ApiError::not_found("submission_draft_not_found"))?;
    Ok(crate::auth::no_store(Json(draft).into_response()))
}

pub(super) async fn get_submission_result(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((task_id, snapshot_id, draft_id, result_id)): Path<(String, String, String, String)>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let (task_id, snapshot_id) = parse_task_question_snapshot_ids(&task_id, &snapshot_id)?;
    let draft_id = SubmissionDraftId::from_str(&draft_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_submission_draft_id",
            "Submission draft ID is invalid",
        )
    })?;
    let result_id = SubmissionResultId::from_str(&result_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_submission_result_id",
            "Submission result ID is invalid",
        )
    })?;
    let result = SqliteQuestionSnapshotRepository::new(state.database)
        .find_owned_submission_result(owner_id, result_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|result| {
            result.task_id == task_id
                && result.question_snapshot_id == snapshot_id
                && result.submission_draft_id == draft_id
        })
        .ok_or_else(|| ApiError::not_found("submission_result_not_found"))?;
    Ok(crate::auth::no_store(Json(result).into_response()))
}

pub(super) async fn execute_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<ExecuteTaskRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (owner_id, request_source) = auth.require_task_execute()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let idempotency_key = required_header(&headers, IDEMPOTENCY_KEY, 256)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request(
            "invalid_execute_task_request",
            "the execution request body must be a valid JSON object",
        )
    })?;
    let submission_draft_id = request
        .submission_draft_id
        .as_deref()
        .map(SubmissionDraftId::from_str)
        .transpose()
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_submission_draft_id",
                "SubmissionDraft ID is invalid",
            )
        })?;
    let strict_completion_retry = request
        .strict_completion_retry_confirmation
        .map(|confirmation| {
            let workflow_id = StrictCompletionWorkflowId::from_str(&confirmation.workflow_id)
                .map_err(|_| {
                    ApiError::bad_request(
                        "invalid_strict_completion_workflow_id",
                        "Strict Completion workflow ID is invalid",
                    )
                })?;
            if confirmation.expected_revision == 0 {
                return Err(ApiError::bad_request(
                    "invalid_strict_completion_workflow_revision",
                    "Strict Completion workflow revision must be positive",
                ));
            }
            Ok(ExecutionStrictCompletionRetryRequest {
                workflow_id,
                expected_revision: confirmation.expected_revision,
            })
        })
        .transpose()?;
    let service = ExecutionRequestService::new(
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteExecutionRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteProviderRuntimeSettingsRepository::new(state.database.clone()),
        SqliteQuestionSnapshotRepository::new(state.database),
        state.providers,
        strict_completion_retry.map_or_else(FormalAssessmentPolicy::default, |_| {
            FormalAssessmentPolicy {
                allow_execution: true,
                allow_submission: true,
            }
        }),
    );
    let result = service
        .execute(ExecuteTaskCommand {
            owner_id,
            task_id,
            requested_capabilities: request.requested_capabilities,
            submission_draft_id,
            strict_completion_retry,
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

pub(super) async fn approve_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    apply_lifecycle_action(
        state,
        auth,
        &task_id,
        &headers,
        TaskLifecycleAction::Approve,
        None,
    )
    .await
}

pub(super) async fn cancel_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    apply_lifecycle_action(
        state,
        auth,
        &task_id,
        &headers,
        TaskLifecycleAction::Cancel,
        None,
    )
    .await
}

pub(super) async fn ignore_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    apply_lifecycle_action(
        state,
        auth,
        &task_id,
        &headers,
        TaskLifecycleAction::Ignore,
        None,
    )
    .await
}

pub(super) async fn delay_task(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<DelayTaskRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request(
            "invalid_delay_task_request",
            "the delay request must contain one valid future delayed_until timestamp",
        )
    })?;
    apply_lifecycle_action(
        state,
        auth,
        &task_id,
        &headers,
        TaskLifecycleAction::Delay,
        Some(request.delayed_until),
    )
    .await
}

async fn apply_lifecycle_action(
    state: ApiState,
    auth: AuthContext,
    task_id: &str,
    headers: &HeaderMap,
    action: TaskLifecycleAction,
    delayed_until: Option<Timestamp>,
) -> Result<Response, ApiError> {
    let (owner_id, request_source) = auth.require_task_execute()?;
    let task_id = TaskId::from_str(task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let idempotency_key = required_header(headers, IDEMPOTENCY_KEY, 256)?;
    let correlation_id = required_header(headers, "x-request-id", 128)?;
    let result = TaskLifecycleService::new(
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteTaskLifecycleRepository::new(state.database),
    )
    .apply(TaskLifecycleCommand {
        owner_id,
        task_id,
        action,
        delayed_until,
        request_source,
        actor: auth.audit_actor(),
        idempotency_key: idempotency_key.to_owned(),
        correlation_id: correlation_id.to_owned(),
        requested_at: Utc::now(),
    })
    .await
    .map_err(map_task_lifecycle_error)?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(TaskLifecycleResponse {
                task_id: result.task_id,
                action: result.action,
                task_state: result.task_state,
                affected_execution_id: result.affected_execution_id,
                delayed_until: result.delayed_until,
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
        ExecutionRequestError::InvalidCapabilitySelection => ApiError::bad_request(
            "invalid_execution_capability_selection",
            "requested_capabilities must be a non-empty unique list of executable Task capabilities",
        ),
        ExecutionRequestError::IdempotencyConflict => ApiError::conflict(
            "idempotency_conflict",
            "the idempotency key is already bound to another execution request",
        ),
        ExecutionRequestError::SubmissionDraftRequired => ApiError::conflict(
            "submission_draft_required",
            "this task requires an explicit immutable SubmissionDraft",
        ),
        ExecutionRequestError::UnexpectedSubmissionDraft => ApiError::conflict(
            "submission_draft_not_applicable",
            "this task does not accept a SubmissionDraft",
        ),
        ExecutionRequestError::SubmissionDraftNotFound => {
            ApiError::not_found("submission_draft_not_found")
        }
        ExecutionRequestError::SubmissionDraftConflict => ApiError::conflict(
            "submission_draft_conflict",
            "the SubmissionDraft is foreign, stale, or already bound to another Execution",
        ),
        ExecutionRequestError::SubmissionDraftVersionConflict => ApiError::conflict(
            "submission_draft_version_conflict",
            "the SubmissionDraft must be rebuilt for the current Provider implementation",
        ),
        ExecutionRequestError::SubmissionVerificationUnavailable => ApiError::conflict(
            "submission_verification_unavailable",
            "submission execution is disabled without independent verification",
        ),
        ExecutionRequestError::ExecutionVerificationUnavailable => ApiError::conflict(
            "execution_verification_unavailable",
            "non-idempotent task execution is disabled without independent progress verification",
        ),
        ExecutionRequestError::ProviderRuntimeUnavailable => ApiError::conflict(
            "provider_runtime_settings_unavailable",
            "the registered Provider runtime settings are unavailable or incompatible",
        ),
        ExecutionRequestError::RuntimeSettingsConflict => ApiError::conflict(
            "runtime_settings_revision_conflict",
            "Provider runtime settings changed while the execution was being scheduled",
        ),
        ExecutionRequestError::StrictCompletionRetryConflict => ApiError::conflict(
            "strict_completion_retry_conflict",
            "the Strict Completion retry confirmation is missing, stale, or invalid",
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

fn map_task_lifecycle_error(error: TaskLifecycleError) -> ApiError {
    match error {
        TaskLifecycleError::TaskNotFound => ApiError::not_found("task_not_found"),
        TaskLifecycleError::TaskStateConflict => ApiError::conflict(
            "task_state_conflict",
            "the task state changed, the action is not allowed, or a pending job is already claimed",
        ),
        TaskLifecycleError::InvalidDelay => ApiError::bad_request(
            "invalid_task_delay",
            "delay requires a future delayed_until timestamp for a pending scheduled task",
        ),
        TaskLifecycleError::IdempotencyConflict => ApiError::conflict(
            "idempotency_conflict",
            "the idempotency key is already bound to another task action",
        ),
        TaskLifecycleError::Storage(_) => ApiError::internal(error),
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

pub(super) fn map_task_browser_session_error(error: ProviderTaskBrowserSessionError) -> ApiError {
    match error {
        ProviderTaskBrowserSessionError::TaskNotFound => ApiError::not_found("task_not_found"),
        ProviderTaskBrowserSessionError::TaskCapabilityUnavailable
        | ProviderTaskBrowserSessionError::CapabilityUnavailable(_) => ApiError::conflict(
            "task_browser_bridge_unavailable",
            "the task does not expose a BrowserBridge session policy",
        ),
        ProviderTaskBrowserSessionError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before preparing BrowserBridge",
        ),
        ProviderTaskBrowserSessionError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        ProviderTaskBrowserSessionError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        ProviderTaskBrowserSessionError::ProviderResponseInvalid => {
            tracing::warn!(%error, "Provider returned invalid BrowserBridge session policy");
            ApiError::bad_gateway(
                "provider_browser_session_invalid",
                "the Provider returned an unsafe BrowserBridge session policy",
            )
        }
        ProviderTaskBrowserSessionError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider BrowserBridge policy is temporarily unavailable");
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
                "task_browser_bridge_unavailable",
                "the Provider cannot prepare BrowserBridge for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid BrowserBridge policy");
                ApiError::bad_gateway(
                    "provider_browser_session_invalid",
                    "the Provider returned an unsafe BrowserBridge session policy",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderTaskBrowserSessionError::Storage(error) => ApiError::internal(error),
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

fn map_task_duration_error(error: ProviderTaskDurationError) -> ApiError {
    match error {
        ProviderTaskDurationError::TaskNotFound => ApiError::not_found("task_not_found"),
        ProviderTaskDurationError::TaskCapabilityUnavailable
        | ProviderTaskDurationError::CapabilityUnavailable(_) => ApiError::conflict(
            "task_duration_unavailable",
            "the task does not expose readable remote duration",
        ),
        ProviderTaskDurationError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before reading task duration",
        ),
        ProviderTaskDurationError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        ProviderTaskDurationError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        ProviderTaskDurationError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider Task duration is temporarily unavailable");
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
                "task_duration_unavailable",
                "the Provider cannot read duration for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid Task duration");
                ApiError::bad_gateway(
                    "provider_task_duration_invalid",
                    "the Provider returned inconsistent task duration",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        ProviderTaskDurationError::Storage(error) => ApiError::internal(error),
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
        ProviderQuestionReadError::DurableStateUnavailable
        | ProviderQuestionReadError::RuntimeSettingsInvalid => ApiError::service_unavailable(
            "question_read_runtime_unavailable",
            "durable Provider Question reading is not configured",
        ),
        ProviderQuestionReadError::AmbiguousAttempt(_) => ApiError::conflict(
            "question_read_ambiguous",
            "the last remote Question operation has an ambiguous outcome and cannot be replayed",
        ),
        ProviderQuestionReadError::ConcurrentAttempt(_) => ApiError::conflict(
            "question_read_in_progress",
            "another Question read operation is already in progress",
        ),
        ProviderQuestionReadError::StateConflict => ApiError::conflict(
            "question_read_state_conflict",
            "the durable Question read state changed concurrently",
        ),
        ProviderQuestionReadError::OperationLimitExceeded => ApiError::bad_gateway(
            "question_read_operation_limit",
            "the Provider Question flow exceeded its bounded operation count",
        ),
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
        ProviderQuestionReadError::Secret(error) => ApiError::internal(error),
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
        ProviderAnswerResolveError::Secret(error) => ApiError::internal(error),
    }
}

fn map_manual_answer_candidate_error(error: ManualAnswerCandidateError) -> ApiError {
    match error {
        ManualAnswerCandidateError::QuestionSnapshotNotFound => {
            ApiError::not_found("question_snapshot_not_found")
        }
        ManualAnswerCandidateError::QuestionNotFound => ApiError::not_found("question_not_found"),
        ManualAnswerCandidateError::InvalidCandidate => ApiError::bad_request(
            "invalid_manual_answer_candidate",
            "the manual answer must be known, typed, bounded, and sanitized",
        ),
        ManualAnswerCandidateError::Storage(error) => ApiError::internal(error),
    }
}

fn map_local_answer_cache_error(error: LocalAnswerCacheError) -> ApiError {
    match error {
        LocalAnswerCacheError::QuestionSnapshotNotFound => {
            ApiError::not_found("question_snapshot_not_found")
        }
        LocalAnswerCacheError::EvidenceInvalid => {
            tracing::warn!(%error, "persisted local answer cache evidence is inconsistent");
            ApiError::internal(error)
        }
        LocalAnswerCacheError::Storage(error) => ApiError::internal(error),
    }
}

fn map_conservative_answer_resolver_error(error: ConservativeAnswerResolverError) -> ApiError {
    match error {
        ConservativeAnswerResolverError::QuestionSnapshotNotFound => {
            ApiError::not_found("question_snapshot_not_found")
        }
        ConservativeAnswerResolverError::EvidenceInvalid
        | ConservativeAnswerResolverError::ResolutionInvalid => {
            tracing::warn!(%error, "persisted answer evidence is inconsistent");
            ApiError::internal(error)
        }
        ConservativeAnswerResolverError::Storage(error) => ApiError::internal(error),
    }
}

fn map_submission_draft_build_error(error: SubmissionDraftBuildError) -> ApiError {
    match error {
        SubmissionDraftBuildError::TaskNotFound => ApiError::not_found("task_not_found"),
        SubmissionDraftBuildError::QuestionSnapshotNotFound => {
            ApiError::not_found("question_snapshot_not_found")
        }
        SubmissionDraftBuildError::TaskCapabilityUnavailable
        | SubmissionDraftBuildError::CapabilityUnavailable(_) => ApiError::conflict(
            "provider_submission_build_unavailable",
            "the task does not expose submission draft construction",
        ),
        SubmissionDraftBuildError::QuestionSnapshotBindingInvalid => ApiError::conflict(
            "question_snapshot_binding_invalid",
            "the Question snapshot is not bound to this task and Provider",
        ),
        SubmissionDraftBuildError::SelectionInvalid
        | SubmissionDraftBuildError::SelectionIncomplete => ApiError::conflict(
            "submission_selection_invalid",
            "the selected Candidates do not satisfy this snapshot's answer coverage policy",
        ),
        SubmissionDraftBuildError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account must be authenticated before building a draft",
        ),
        SubmissionDraftBuildError::ProviderNotRegistered(_) => ApiError::conflict(
            "provider_not_registered",
            "the task Provider is not registered",
        ),
        SubmissionDraftBuildError::InvalidCorrelationId => ApiError::bad_request(
            "invalid_request_id",
            "the request correlation ID is invalid",
        ),
        SubmissionDraftBuildError::ProviderPreviewInvalid => {
            tracing::warn!(%error, "Provider returned invalid SubmissionDraft preview");
            ApiError::bad_gateway(
                "provider_submission_preview_invalid",
                "the Provider returned an inconsistent submission preview",
            )
        }
        SubmissionDraftBuildError::RuntimeSettingsInvalid => {
            tracing::warn!(%error, "submission coverage runtime settings are invalid");
            ApiError::internal(error)
        }
        SubmissionDraftBuildError::Assessment(_) => ApiError::conflict(
            "formal_assessment_blocked",
            "formal assessment draft construction is disabled by Core policy",
        ),
        SubmissionDraftBuildError::Provider(provider_error) => match provider_error.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider_error
                    .retry_after_seconds
                    .unwrap_or(60)
                    .clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                tracing::warn!(error = %provider_error, "Provider submission build is temporarily unavailable");
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
                "provider_submission_build_unavailable",
                "the Provider cannot build a draft for this task",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                tracing::warn!(error = %provider_error, "Provider returned invalid SubmissionDraft preview");
                ApiError::bad_gateway(
                    "provider_submission_preview_invalid",
                    "the Provider returned an inconsistent submission preview",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider_error),
        },
        SubmissionDraftBuildError::Storage(error) => ApiError::internal(error),
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
struct TaskCompletionWorkflowsResponse {
    task_id: TaskId,
    strict_completion: Option<StrictCompletionWorkflowResponse>,
    score_improvement: Option<ScoreImprovementWorkflowResponse>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StrictCompletionWorkflowResponse {
    revision: u32,
    workflow: StrictCompletionWorkflow,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ScoreImprovementWorkflowResponse {
    revision: u32,
    workflow: ScoreImprovementWorkflow,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecuteTaskResponse {
    execution: Execution,
    created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskLifecycleResponse {
    task_id: TaskId,
    action: TaskLifecycleAction,
    task_state: asterism_domain::OrchestrationState,
    affected_execution_id: Option<asterism_domain::ExecutionId>,
    delayed_until: Option<Timestamp>,
    created: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct DelayTaskRequest {
    delayed_until: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecuteTaskRequest {
    requested_capabilities: Vec<TaskCapability>,
    submission_draft_id: Option<String>,
    strict_completion_retry_confirmation: Option<StrictCompletionRetryConfirmationRequest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct StrictCompletionRetryConfirmationRequest {
    workflow_id: String,
    expected_revision: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct TaskDetailResponse {
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    detail: RemoteTaskDetail,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskBrowserSessionSpecResponse {
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    spec: BrowserSessionSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskProgressResponse {
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    progress: RemoteProgress,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskDurationResponse {
    task_id: TaskId,
    provider_id: ProviderId,
    provider_version: String,
    duration: RemoteDuration,
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
struct AnswerCandidatesResponse {
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

#[derive(Clone, Debug, PartialEq, Serialize)]
struct LocalAnswerCacheImportResponse {
    task_id: TaskId,
    question_snapshot_id: QuestionSnapshotId,
    candidates: Vec<AnswerCandidateResponse>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateManualAnswerCandidateRequest {
    question_id: String,
    answer: NormalizedAnswer,
    confidence_basis_points: Option<u16>,
    explanation: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildSubmissionDraftRequest {
    answer_candidate_ids: Vec<String>,
}

fn parse_task_question_snapshot_ids(
    task_id: &str,
    snapshot_id: &str,
) -> Result<(TaskId, QuestionSnapshotId), ApiError> {
    let task_id = TaskId::from_str(task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let snapshot_id = QuestionSnapshotId::from_str(snapshot_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_question_snapshot_id",
            "Question snapshot ID is invalid",
        )
    })?;
    Ok((task_id, snapshot_id))
}

fn parse_provider_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "provider account ID is invalid",
        )
    })
}
