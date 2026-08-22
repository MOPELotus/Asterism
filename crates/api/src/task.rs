use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AnswerConfidence, AnswerSource, Execution,
    ExecutionAttempt, ExecutionInvocationDraftId, NormalizedAnswer, ProviderAccountId, ProviderId,
    Question, QuestionGroup, QuestionId, QuestionSnapshotId, RemoteState, ScoreImprovementWorkflow,
    StrictCompletionWorkflow, StrictCompletionWorkflowId, SubmissionDraftId,
    SubmissionQuestionVerificationStatus, SubmissionResultId, SubmissionResultStatus,
    SubmissionScore, Task, TaskCapability, TaskId, TaskLifecycleAction, Timestamp,
};
use asterism_engine::{
    BuildSubmissionDraftCommand, ConservativeAnswerResolverError,
    ConservativeAnswerResolverService, CreateManualAnswerCandidateCommand, ExecuteTaskCommand,
    ExecutionRequestError, ExecutionRequestService, FormalAssessmentPolicy,
    ImportLocalAnswerCandidatesCommand, LocalAnswerCacheError, LocalAnswerCacheService,
    ManualAnswerCandidateError, ManualAnswerCandidateService, OptInScoreImprovementCommand,
    PrepareExecutionInvocationCommand, ProviderAnswerResolveError, ProviderAnswerResolveService,
    ProviderQuestionReadError, ProviderQuestionReadResult, ProviderQuestionReadService,
    ProviderTaskBrowserSessionError, ProviderTaskBrowserSessionService, ProviderTaskDetailError,
    ProviderTaskDetailService, ProviderTaskDurationError, ProviderTaskDurationService,
    ProviderTaskProgressError, ProviderTaskProgressService, ReadTaskBrowserSessionCommand,
    ReadTaskDetailCommand, ReadTaskDurationCommand, ReadTaskProgressCommand,
    ReadTaskQuestionsCommand, ResolveAnswerCandidatesCommand, ResolveProviderAnswersCommand,
    ScoreImprovementOptInError, ScoreImprovementOptInService, SubmissionDraftBuildError,
    SubmissionDraftBuildService, TaskLifecycleCommand, TaskLifecycleError, TaskLifecycleService,
};
use asterism_provider_api::{
    BrowserSessionSpec, MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES, ProviderErrorKind,
    RemoteDuration, RemoteProgress, RemoteTaskDetail,
};
use asterism_secrets::SecretValue;
use asterism_storage::{
    AnswerCandidateRepository, AnswerEvidenceClassCounts, AnswerEvidenceRepository,
    CompletionWorkflowRepository, ExecutionQueryRepository, ExecutionStrictCompletionRetryRequest,
    QuestionSnapshotRepository, SqliteAnswerEvidenceRepository,
    SqliteAnswerHistoryIngestionRepository, SqliteCompletionWorkflowRepository,
    SqliteExecutionRepository, SqliteProtocolObservationRepository,
    SqliteProviderAccountRepository, SqliteProviderRuntimeSettingsRepository,
    SqliteQuestionReadAttemptRepository, SqliteQuestionSnapshotRepository,
    SqliteTaskLifecycleRepository, SqliteTaskQueryRepository, SubmissionDraftRepository,
    SubmissionResultRepository, TaskQueryRepository,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection, rejection::QueryRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;
const IDEMPOTENCY_KEY: &str = "idempotency-key";
const INVOCATION_INPUT_TYPE: &str = "x-asterism-invocation-input-type";
const INVOCATION_CAPABILITIES: &str = "x-asterism-requested-capabilities";
const INVOCATION_SUBMISSION_DRAFT: &str = "x-asterism-submission-draft-id";
pub(super) const MAX_EXECUTION_INVOCATION_INPUT_BYTES: usize =
    MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES;

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

pub(super) async fn opt_in_score_improvement(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    payload: Result<Json<ScoreImprovementOptInRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (owner_id, _) = auth.require_task_execute()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let Json(payload) = payload.map_err(|_| {
        ApiError::bad_request(
            "invalid_score_improvement_opt_in",
            "score improvement opt-in body is invalid",
        )
    })?;
    if !payload.explicitly_opted_in {
        return Err(ApiError::bad_request(
            "score_improvement_opt_in_required",
            "explicitly_opted_in must be true",
        ));
    }
    SqliteTaskQueryRepository::new(state.database.clone())
        .find_owned_task(owner_id, task_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("task_not_found"))?;

    let result = ScoreImprovementOptInService::new(
        SqliteCompletionWorkflowRepository::new(state.database.clone()),
        SqliteAnswerHistoryIngestionRepository::new(state.database),
    )
    .opt_in(OptInScoreImprovementCommand {
        owner_id,
        task_id,
        at: Utc::now(),
    })
    .await
    .map_err(map_score_improvement_opt_in_error)?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(ScoreImprovementOptInResponse {
                revision: result.record.revision,
                workflow: result.record.workflow,
                created: result.created,
            }),
        )
            .into_response(),
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "the owner-scoped history assembly keeps Execution, evidence, Draft and Result bindings visible together"
)]
pub(super) async fn list_task_attempt_history(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    query: Result<Query<TaskAttemptHistoryQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_attempt_history_query",
            "attempt history query parameters have an invalid format",
        )
    })?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > MAX_OFFSET {
        return Err(ApiError::bad_request(
            "invalid_attempt_history_pagination",
            "attempt history limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    SqliteTaskQueryRepository::new(state.database.clone())
        .find_owned_task(owner_id, task_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("task_not_found"))?;

    let executions = SqliteExecutionRepository::new(state.database.clone());
    let submissions = SqliteQuestionSnapshotRepository::new(state.database.clone());
    let evidence = SqliteAnswerEvidenceRepository::new(state.database);
    let page = executions
        .list_owned_executions(owner_id, Some(task_id), limit, offset)
        .await
        .map_err(ApiError::internal)?;
    let mut items = Vec::with_capacity(page.items.len());
    for execution in page.items {
        let detail = executions
            .find_owned_execution_detail(owner_id, execution.id)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| inconsistent_attempt_history("owned Execution detail disappeared"))?;
        let mut attempts = Vec::with_capacity(detail.attempts.len());
        for attempt in detail.attempts {
            let learned_evidence = evidence
                .count_owned_execution_attempt_evidence(owner_id, attempt.id)
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| {
                    inconsistent_attempt_history("ExecutionAttempt owner binding is invalid")
                })?;
            attempts.push(TaskAttemptHistoryAttempt {
                attempt,
                learned_evidence: learned_evidence.into(),
            });
        }
        let submission = if let Some(draft_id) = detail.execution.submission_draft_id {
            let draft = submissions
                .find_owned_submission_draft(owner_id, draft_id)
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(|| {
                    inconsistent_attempt_history("Execution Draft binding is invalid")
                })?;
            let result = submissions
                .find_latest_owned_submission_result(owner_id, draft.id)
                .await
                .map_err(ApiError::internal)?;
            let result = if let Some(result) = result {
                let previous_score = submissions
                    .find_previous_owned_submission_score(owner_id, task_id, result.id)
                    .await
                    .map_err(ApiError::internal)?;
                Some(submission_result_history(&result, previous_score))
            } else {
                None
            };
            Some(TaskSubmissionHistory {
                submission_draft_id: draft.id,
                question_snapshot_id: draft.question_snapshot_id,
                item_count: u32::try_from(draft.items.len()).map_err(ApiError::internal)?,
                total_question_count: draft.answer_coverage.total_question_count,
                unanswered_question_count: u32::try_from(
                    draft.answer_coverage.unanswered_question_ids.len(),
                )
                .map_err(ApiError::internal)?,
                answer_sources: answer_source_counts(&draft),
                created_at: draft.created_at,
                result,
            })
        } else {
            None
        };
        items.push(TaskAttemptHistoryEntry {
            execution: detail.execution,
            attempts,
            submission,
        });
    }
    Ok(crate::auth::no_store(
        Json(TaskAttemptHistoryPageResponse {
            total: page.total,
            limit,
            offset,
            items,
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
        SqliteProviderAccountRepository::new(state.database.clone()),
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
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
        SqliteProviderAccountRepository::new(state.database.clone()),
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
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
        SqliteProviderAccountRepository::new(state.database.clone()),
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
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
        SqliteProviderAccountRepository::new(state.database.clone()),
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
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
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database.clone(),
    )));
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
            groups,
        } => Ok(crate::auth::no_store(
            Json(TaskQuestionsResponse {
                snapshot_id,
                task_id,
                provider_id,
                provider_version,
                captured_at,
                questions,
                groups,
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
            groups: snapshot.groups,
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
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database.clone(),
    )));
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
        SqliteProviderRuntimeSettingsRepository::new(state.database.clone()),
    )
    .with_protocol_observations(Arc::new(SqliteProtocolObservationRepository::new(
        state.database,
    )))
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

#[allow(
    clippy::too_many_lines,
    reason = "the HTTP boundary keeps authentication, idempotency, immutable draft selection and response mapping visible"
)]
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
    let formal_assessment_confirmed = request.formal_assessment_confirmation.unwrap_or(false);
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
    let invocation_draft_id = request
        .invocation_draft_id
        .as_deref()
        .map(ExecutionInvocationDraftId::from_str)
        .transpose()
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_execution_invocation_draft_id",
                "Execution invocation draft ID is invalid",
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
    let invocation_store = state.secret_store.clone();
    let mut service = ExecutionRequestService::new(
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteExecutionRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteProviderRuntimeSettingsRepository::new(state.database.clone()),
        SqliteQuestionSnapshotRepository::new(state.database),
        state.providers,
        if formal_assessment_confirmed || strict_completion_retry.is_some() {
            FormalAssessmentPolicy {
                allow_execution: true,
                allow_submission: true,
            }
        } else {
            FormalAssessmentPolicy::default()
        },
    );
    if let Some(store) = invocation_store {
        service = service.with_execution_invocation_drafts(Arc::new(store));
    }
    let result = service
        .execute(ExecuteTaskCommand {
            owner_id,
            task_id,
            requested_capabilities: request.requested_capabilities,
            submission_draft_id,
            invocation_draft_id,
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

pub(super) async fn prepare_execution_invocation_draft(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let (owner_id, _) = auth.require_task_execute()?;
    let task_id = TaskId::from_str(&task_id)
        .map_err(|_| ApiError::bad_request("invalid_task_id", "task ID is invalid"))?;
    require_octet_stream(&headers)?;
    if body.is_empty() || body.len() > MAX_EXECUTION_INVOCATION_INPUT_BYTES {
        return Err(ApiError::bad_request(
            "invalid_execution_invocation_input",
            "execution invocation input is empty or oversized",
        ));
    }
    let idempotency_key = required_header(&headers, IDEMPOTENCY_KEY, 256)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let input_type = required_header(&headers, INVOCATION_INPUT_TYPE, 128)?.to_owned();
    let requested_capabilities =
        parse_invocation_capabilities(required_header(&headers, INVOCATION_CAPABILITIES, 256)?)?;
    let submission_draft_id = headers
        .get(INVOCATION_SUBMISSION_DRAFT)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| SubmissionDraftId::from_str(value).ok())
                .ok_or_else(|| {
                    ApiError::bad_request(
                        "invalid_submission_draft_id",
                        "SubmissionDraft ID is invalid",
                    )
                })
        })
        .transpose()?;
    let secret_store = state.secret_store.clone().ok_or_else(|| {
        ApiError::service_unavailable(
            "secret_store_unavailable",
            "encrypted execution invocation drafts are not configured",
        )
    })?;
    let service = ExecutionRequestService::new(
        SqliteTaskQueryRepository::new(state.database.clone()),
        SqliteExecutionRepository::new(state.database.clone()),
        SqliteProviderAccountRepository::new(state.database.clone()),
        SqliteProviderRuntimeSettingsRepository::new(state.database.clone()),
        SqliteQuestionSnapshotRepository::new(state.database),
        state.providers,
        FormalAssessmentPolicy::default(),
    )
    .with_execution_invocation_drafts(Arc::new(secret_store));
    let result = service
        .prepare_invocation(PrepareExecutionInvocationCommand {
            owner_id,
            task_id,
            requested_capabilities,
            submission_draft_id,
            input_type,
            raw_input: secret_request_body(body),
            idempotency_key: idempotency_key.to_owned(),
            correlation_id: correlation_id.to_owned(),
            created_at: Utc::now(),
        })
        .await
        .map_err(map_execution_request_error)?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let record = result.record;
    Ok(crate::auth::no_store(
        (
            status,
            Json(ExecutionInvocationDraftResponse {
                draft_id: record.draft.id,
                provider_id: record.draft.provider_id,
                provider_version: record.draft.provider_version,
                task_id: record.draft.task_id,
                requested_capabilities: record.draft.requested_capabilities,
                submission_draft_id: record.draft.submission_draft_id,
                private_input_type: record.draft.private_input_type,
                private_input_digest: encode_digest(record.draft.private_input_digest),
                plan_artifact_type: record.provider_plan_artifact.artifact_type().to_owned(),
                plan_artifact_digest: encode_digest(record.draft.plan_artifact_digest),
                created_at: record.draft.created_at,
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
        ExecutionRequestError::InvocationDraftUnavailable => ApiError::conflict(
            "execution_invocation_draft_unavailable",
            "encrypted execution invocation drafts are unavailable",
        ),
        ExecutionRequestError::InvocationDraftNotFound => {
            ApiError::not_found("execution_invocation_draft_not_found")
        }
        ExecutionRequestError::InvocationDraftConflict => ApiError::conflict(
            "execution_invocation_draft_conflict",
            "the execution invocation draft is foreign, stale, or already claimed",
        ),
        ExecutionRequestError::InvalidInvocationInput => ApiError::bad_request(
            "invalid_execution_invocation_input",
            "the Provider invocation input is invalid or oversized",
        ),
        ExecutionRequestError::InvocationPreparationFailed => ApiError::conflict(
            "execution_invocation_preparation_failed",
            "the Provider could not safely prepare this private invocation",
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
        ExecutionRequestError::Transition(_)
        | ExecutionRequestError::Storage(_)
        | ExecutionRequestError::SecretStore(_) => ApiError::internal(error),
    }
}

fn map_score_improvement_opt_in_error(error: ScoreImprovementOptInError) -> ApiError {
    match error {
        ScoreImprovementOptInError::CompletionBaselineNotVerified => ApiError::conflict(
            "completion_baseline_not_verified",
            "score improvement requires a verified Completed or Passed baseline",
        ),
        ScoreImprovementOptInError::HistoryFactsUnavailable => ApiError::conflict(
            "answer_history_facts_unavailable",
            "no exact Provider result facts are available for this task",
        ),
        ScoreImprovementOptInError::HistoryFactsStaleOrMismatched => ApiError::conflict(
            "answer_history_facts_stale",
            "the latest Provider result facts are older than completion or cross-bound",
        ),
        ScoreImprovementOptInError::PolicyDisabled => ApiError::conflict(
            "score_improvement_disabled",
            "the frozen task policy disables score improvement",
        ),
        ScoreImprovementOptInError::PolicyExpired => ApiError::conflict(
            "score_improvement_policy_expired",
            "the frozen score improvement policy has expired",
        ),
        ScoreImprovementOptInError::RetakeUnavailable => ApiError::conflict(
            "retake_unavailable",
            "the latest Provider result does not authorize an available retake",
        ),
        ScoreImprovementOptInError::InvalidWorkflow
        | ScoreImprovementOptInError::WorkflowConflict => ApiError::conflict(
            "score_improvement_workflow_conflict",
            "the score improvement workflow is invalid or changed concurrently",
        ),
        ScoreImprovementOptInError::Storage(_) => ApiError::internal(error),
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
        ProviderTaskDetailError::ProviderResponseInvalid
        | ProviderTaskDetailError::InvalidProtocolObservation => {
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
        ProviderTaskBrowserSessionError::ProviderResponseInvalid
        | ProviderTaskBrowserSessionError::InvalidProtocolObservation => {
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
        ProviderTaskProgressError::ProviderResponseInvalid
        | ProviderTaskProgressError::InvalidProtocolObservation => {
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
        ProviderTaskDurationError::InvalidProtocolObservation => {
            tracing::warn!(%error, "Provider returned an invalid protocol observation");
            ApiError::bad_gateway(
                "provider_task_duration_invalid",
                "the Provider returned inconsistent task duration",
            )
        }
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
        ProviderQuestionReadError::ProviderResponseInvalid
        | ProviderQuestionReadError::InvalidProtocolObservation => {
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
        ProviderAnswerResolveError::ProviderResponseInvalid
        | ProviderAnswerResolveError::InvalidProtocolObservation => {
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
        SubmissionDraftBuildError::InvalidProtocolObservation => {
            tracing::warn!(%error, "Provider returned an invalid protocol observation");
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskAttemptHistoryQuery {
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskAttemptHistoryPageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<TaskAttemptHistoryEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskAttemptHistoryEntry {
    execution: Execution,
    attempts: Vec<TaskAttemptHistoryAttempt>,
    submission: Option<TaskSubmissionHistory>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskAttemptHistoryAttempt {
    attempt: ExecutionAttempt,
    learned_evidence: AnswerEvidenceCountsResponse,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct AnswerEvidenceCountsResponse {
    official: u64,
    verified_historical: u64,
    negative: u64,
}

impl From<AnswerEvidenceClassCounts> for AnswerEvidenceCountsResponse {
    fn from(value: AnswerEvidenceClassCounts) -> Self {
        Self {
            official: value.official,
            verified_historical: value.verified_historical,
            negative: value.negative,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskSubmissionHistory {
    submission_draft_id: SubmissionDraftId,
    question_snapshot_id: QuestionSnapshotId,
    item_count: u32,
    total_question_count: u32,
    unanswered_question_count: u32,
    answer_sources: AnswerSourceCountsResponse,
    created_at: Timestamp,
    result: Option<TaskSubmissionResultHistory>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct AnswerSourceCountsResponse {
    manual: u32,
    local_cache: u32,
    provider_native: u32,
    external_bank: u32,
    other: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TaskSubmissionResultHistory {
    submission_result_id: SubmissionResultId,
    execution_attempt_id: asterism_domain::ExecutionAttemptId,
    status: SubmissionResultStatus,
    score: Option<SubmissionScore>,
    previous_score: Option<SubmissionScore>,
    score_delta_millis: Option<i32>,
    remote_state: Option<RemoteState>,
    progress_percent: Option<u8>,
    question_results: SubmissionQuestionResultCountsResponse,
    verified_at: Timestamp,
    created_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct SubmissionQuestionResultCountsResponse {
    confirmed: u32,
    rejected: u32,
    unverified: u32,
}

fn answer_source_counts(draft: &asterism_domain::SubmissionDraft) -> AnswerSourceCountsResponse {
    let mut counts = AnswerSourceCountsResponse::default();
    for item in &draft.items {
        let count = match item.selected.source {
            AnswerSource::Manual => &mut counts.manual,
            AnswerSource::LocalCache => &mut counts.local_cache,
            AnswerSource::ProviderNative => &mut counts.provider_native,
            AnswerSource::ExternalBank => &mut counts.external_bank,
            AnswerSource::Other => &mut counts.other,
        };
        *count = count.saturating_add(1);
    }
    counts
}

fn submission_result_history(
    result: &asterism_domain::SubmissionResult,
    previous_score: Option<SubmissionScore>,
) -> TaskSubmissionResultHistory {
    let mut question_results = SubmissionQuestionResultCountsResponse::default();
    for question in &result.verification.questions {
        let count = match question.status {
            SubmissionQuestionVerificationStatus::Confirmed => &mut question_results.confirmed,
            SubmissionQuestionVerificationStatus::Rejected => &mut question_results.rejected,
            SubmissionQuestionVerificationStatus::Unverified => &mut question_results.unverified,
        };
        *count = count.saturating_add(1);
    }
    TaskSubmissionResultHistory {
        submission_result_id: result.id,
        execution_attempt_id: result.execution_attempt_id,
        status: result.status,
        score: result.verification.score,
        previous_score,
        score_delta_millis: result
            .verification
            .score
            .zip(previous_score)
            .map(|(score, previous)| score_millis(score) - score_millis(previous)),
        remote_state: result.verification.remote_state,
        progress_percent: result.verification.progress_percent,
        question_results,
        verified_at: result.verification.verified_at,
        created_at: result.created_at,
    }
}

fn score_millis(score: SubmissionScore) -> i32 {
    let millis =
        u128::from(score.earned_milli_points) * 1_000 / u128::from(score.possible_milli_points);
    i32::try_from(millis).expect("validated score ratio fits i32")
}

fn inconsistent_attempt_history(message: &'static str) -> ApiError {
    ApiError::internal(std::io::Error::other(message))
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ScoreImprovementOptInRequest {
    explicitly_opted_in: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ScoreImprovementOptInResponse {
    revision: u32,
    workflow: ScoreImprovementWorkflow,
    created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecuteTaskResponse {
    execution: Execution,
    created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ExecutionInvocationDraftResponse {
    draft_id: ExecutionInvocationDraftId,
    provider_id: ProviderId,
    provider_version: String,
    task_id: TaskId,
    requested_capabilities: Vec<TaskCapability>,
    submission_draft_id: Option<SubmissionDraftId>,
    private_input_type: String,
    private_input_digest: String,
    plan_artifact_type: String,
    plan_artifact_digest: String,
    created_at: Timestamp,
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
    invocation_draft_id: Option<String>,
    formal_assessment_confirmation: Option<bool>,
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
    groups: Vec<QuestionGroup>,
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

fn parse_invocation_capabilities(value: &str) -> Result<Vec<TaskCapability>, ApiError> {
    let capabilities = value
        .split(',')
        .map(str::trim)
        .map(|value| {
            serde_json::from_value::<TaskCapability>(serde_json::Value::String(value.to_owned()))
                .map_err(|_| {
                    ApiError::bad_request(
                        "invalid_execution_capability_selection",
                        "x-asterism-requested-capabilities contains an invalid capability",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if capabilities.is_empty() || capabilities.len() > 5 {
        return Err(ApiError::bad_request(
            "invalid_execution_capability_selection",
            "x-asterism-requested-capabilities must contain 1-5 capabilities",
        ));
    }
    Ok(capabilities)
}

fn require_octet_stream(headers: &HeaderMap) -> Result<(), ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type == Some("application/octet-stream") {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid_execution_invocation_content_type",
            "execution invocation input requires application/octet-stream",
        ))
    }
}

fn secret_request_body(body: Bytes) -> SecretValue {
    match body.try_into_mut() {
        Ok(mut body) => {
            let secret = SecretValue::new(body.as_ref().to_vec());
            body.as_mut().zeroize();
            secret
        }
        Err(body) => SecretValue::new(body.to_vec()),
    }
}

fn encode_digest(digest: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
