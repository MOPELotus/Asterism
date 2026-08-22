use std::str::FromStr;

use asterism_domain::{
    BatchExecution, BatchExecutionId, CourseId, ExecutionState, ProviderAccountId, ProviderId,
    TaskCapability,
};
use asterism_provider_api::ProviderBatchExecutionPublicInput;
use asterism_provider_welearn::{
    WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE, WellearnAutoDurationBudget, WellearnBatchFlow,
    WellearnBatchUnitSelection, WellearnPublicBatchDurationPolicy,
    WellearnPublicBatchExecutionInput, WellearnPublicBatchScorePolicy,
};
use asterism_secrets::SecretValue;
use asterism_storage::{
    BatchExecutionRepository, BatchExecutionScheduleOutcome, BatchExecutionScheduleRequest,
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiState, auth::AuthContext};

const IDEMPOTENCY_KEY: &str = "idempotency-key";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateWellearnBatchExecutionRequest {
    course_remote_id: String,
    flow: WellearnBatchFlow,
    selected_unit_indices: Option<Vec<u32>>,
    expected_remote_task_id: String,
    duration: WellearnBatchDurationRequest,
    expected_child_count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WellearnBatchDurationRequest {
    PerChildSeconds {
        target_seconds: Vec<u64>,
    },
    AutoAggregate {
        configured_minutes: u16,
        random_range_minutes: u8,
        sampled_offset_minutes: i16,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct CreateBatchExecutionResponse {
    batch_execution: BatchExecution,
    created: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "the 0.1.0 WELearn-specific batch route keeps direct functional wiring together"
)]
pub(super) async fn create_welearn_batch_execution(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, course_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<CreateWellearnBatchExecutionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (owner_id, request_source) = auth.require_task_execute()?;
    let account_id = ProviderAccountId::from_str(&account_id).map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_account_id",
            "ProviderAccount ID is invalid",
        )
    })?;
    let course_id = CourseId::from_str(&course_id)
        .map_err(|_| ApiError::bad_request("invalid_course_id", "Course ID is invalid"))?;
    let idempotency_key = required_header(&headers, IDEMPOTENCY_KEY, 256)?;
    let correlation_id = required_header(&headers, "x-request-id", 128)?;
    let request = payload.map(|Json(request)| request).map_err(|_| {
        ApiError::bad_request(
            "invalid_welearn_batch_request",
            "the WELearn batch request must match the documented JSON shape",
        )
    })?;
    if request.expected_child_count == 0 {
        return Err(ApiError::bad_request(
            "invalid_welearn_batch_request",
            "expected_child_count must be positive",
        ));
    }

    let (duration_policy, score_policy) = match (request.flow, request.duration) {
        (
            WellearnBatchFlow::FanyuchangDuration,
            WellearnBatchDurationRequest::PerChildSeconds { target_seconds },
        ) if target_seconds.len() == request.expected_child_count as usize => (
            WellearnPublicBatchDurationPolicy::FrozenPerChildSeconds(target_seconds),
            WellearnPublicBatchScorePolicy::Fixed(100),
        ),
        (
            WellearnBatchFlow::AutoDuration,
            WellearnBatchDurationRequest::AutoAggregate {
                configured_minutes,
                random_range_minutes,
                sampled_offset_minutes,
            },
        ) => (
            WellearnPublicBatchDurationPolicy::FrozenAutoAggregate(
                WellearnAutoDurationBudget::try_new(
                    configured_minutes,
                    random_range_minutes,
                    sampled_offset_minutes,
                )
                .map_err(invalid_batch_policy)?,
            ),
            WellearnPublicBatchScorePolicy::Fixed(0),
        ),
        _ => {
            return Err(ApiError::bad_request(
                "invalid_welearn_batch_request",
                "flow and duration policy are incompatible, or the per-child target count drifted",
            ));
        }
    };
    let provider_id = ProviderId::new("welearn").expect("compile-time Provider ID is valid");
    if state
        .providers
        .get(&provider_id)
        .and_then(|entry| entry.task_execution.as_ref())
        .is_none()
    {
        return Err(ApiError::conflict(
            "welearn_provider_unavailable",
            "the WELearn execution Provider is not enabled",
        ));
    }
    let selection = request.selected_unit_indices.map_or(
        WellearnBatchUnitSelection::All,
        WellearnBatchUnitSelection::Explicit,
    );
    let public = WellearnPublicBatchExecutionInput::try_new(
        request.course_remote_id,
        request.flow,
        selection,
        request.expected_remote_task_id,
        duration_policy,
        score_policy,
    )
    .map_err(invalid_batch_policy)?;
    let planning_input = public
        .to_provider_planning_input()
        .map_err(invalid_batch_policy)?;
    let public_input = ProviderBatchExecutionPublicInput::try_new(
        provider_id,
        WELLEARN_PUBLIC_BATCH_EXECUTION_INPUT_TYPE,
        SecretValue::new(public.encode().map_err(invalid_batch_policy)?),
    )
    .map_err(invalid_batch_policy)?;
    let now = Utc::now();
    let batch_execution = BatchExecution {
        id: BatchExecutionId::new(),
        provider_account_id: account_id,
        course_id,
        requested_capabilities: vec![
            TaskCapability::ResourceExecution,
            TaskCapability::DurationReport,
        ],
        expected_child_count: request.expected_child_count,
        requested_by: Some(owner_id),
        request_source,
        state: ExecutionState::Scheduled,
        scheduled_at: Some(now),
        started_at: None,
        finished_at: None,
        created_at: now,
    };
    batch_execution.validate().map_err(|_| {
        ApiError::bad_request(
            "invalid_welearn_batch_request",
            "the requested parent batch is outside the supported bounds",
        )
    })?;
    let repository = state
        .secret_store
        .as_ref()
        .ok_or_else(|| {
            ApiError::service_unavailable(
                "secret_store_unavailable",
                "encrypted batch planning storage is not configured",
            )
        })?
        .batch_executions();
    let idempotency_scope = format!("user:{owner_id}");
    let outcome = repository
        .schedule_batch_execution(BatchExecutionScheduleRequest {
            batch_execution: &batch_execution,
            public_input: &public_input,
            planning_input: &planning_input,
            idempotency_scope: &idempotency_scope,
            idempotency_key,
            actor: auth.audit_actor(),
            correlation_id,
        })
        .await
        .map_err(ApiError::internal)?;
    let (batch_execution, created, status) = match outcome {
        BatchExecutionScheduleOutcome::Created(batch) => (batch, true, StatusCode::CREATED),
        BatchExecutionScheduleOutcome::Existing(batch) => (batch, false, StatusCode::OK),
        BatchExecutionScheduleOutcome::IdempotencyConflict => {
            return Err(ApiError::conflict(
                "idempotency_conflict",
                "the idempotency key is already bound to another batch request",
            ));
        }
        BatchExecutionScheduleOutcome::BindingConflict => {
            return Err(ApiError::conflict(
                "batch_binding_conflict",
                "the Provider account, Course, owner or Provider input binding is invalid",
            ));
        }
    };
    Ok(crate::auth::no_store(
        (
            status,
            Json(CreateBatchExecutionResponse {
                batch_execution,
                created,
            }),
        )
            .into_response(),
    ))
}

fn invalid_batch_policy(error: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request("invalid_welearn_batch_request", error.to_string())
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    max_bytes: usize,
) -> Result<&'a str, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= max_bytes)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_request_header",
                format!("the {name} header is required and must be bounded UTF-8"),
            )
        })
}
