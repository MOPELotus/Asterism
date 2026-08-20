use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    CourseEnrollmentAttemptId, CourseEnrollmentAttemptState, CourseEnrollmentDraftId,
    ProviderAccountId, Timestamp,
};
use asterism_engine::{
    CourseEnrollmentRunResult, CourseEnrollmentService, CourseEnrollmentServiceError,
    ExecuteCourseEnrollmentCommand, PrepareCourseEnrollmentCommand, RecoverCourseEnrollmentCommand,
};
use asterism_provider_api::ProviderErrorKind;
use asterism_secrets::SecretString;
use asterism_storage::SqliteProviderAccountRepository;
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApiError, ApiState, auth::AuthContext};

const MAX_INVITATION_BYTES: usize = 4_096;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareCourseEnrollmentRequest {
    draft_id: String,
    invitation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecuteCourseEnrollmentRequest {
    attempt_id: String,
    confirm_non_idempotent_enrollment: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct CourseEnrollmentDraftResponse {
    draft_id: CourseEnrollmentDraftId,
    provider_account_id: ProviderAccountId,
    remote_course_id: String,
    remote_class_id: String,
    preview_sanitized: Value,
    created_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct CourseEnrollmentAttemptResponse {
    attempt_id: CourseEnrollmentAttemptId,
    draft_id: CourseEnrollmentDraftId,
    state: CourseEnrollmentAttemptState,
    updated_at: Timestamp,
}

pub(super) async fn prepare_course_enrollment(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(account_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PrepareCourseEnrollmentRequest>,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_account_manage()?;
    let provider_account_id = parse_account_id(&account_id)?;
    let draft_id = parse_draft_id(&request.draft_id)?;
    if request.invitation.is_empty()
        || request.invitation.len() > MAX_INVITATION_BYTES
        || request.invitation.trim() != request.invitation
        || request.invitation.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "invalid_course_invitation",
            "course invitation must be a bounded non-empty value",
        ));
    }
    let correlation_id = required_request_id(&headers)?;
    let service = enrollment_service(&state)?;
    let record = service
        .prepare(PrepareCourseEnrollmentCommand {
            draft_id,
            owner_user_id,
            provider_account_id,
            invitation: SecretString::new(request.invitation),
            correlation_id: correlation_id.to_owned(),
            at: chrono::Utc::now(),
        })
        .await
        .map_err(map_service_error)?;
    Ok(crate::auth::no_store(
        (
            StatusCode::CREATED,
            Json(CourseEnrollmentDraftResponse {
                draft_id: record.draft.id,
                provider_account_id: record.draft.provider_account_id,
                remote_course_id: record.draft.remote_course_id,
                remote_class_id: record.draft.remote_class_id,
                preview_sanitized: record.preview_sanitized,
                created_at: record.draft.created_at,
            }),
        )
            .into_response(),
    ))
}

pub(super) async fn execute_course_enrollment(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, draft_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<ExecuteCourseEnrollmentRequest>,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_account_manage()?;
    if !request.confirm_non_idempotent_enrollment {
        return Err(ApiError::bad_request(
            "course_enrollment_confirmation_required",
            "explicit confirmation is required before joining a course",
        ));
    }
    let provider_account_id = parse_account_id(&account_id)?;
    let draft_id = parse_draft_id(&draft_id)?;
    let attempt_id = parse_attempt_id(&request.attempt_id)?;
    let correlation_id = required_request_id(&headers)?;
    let result = enrollment_service(&state)?
        .execute(ExecuteCourseEnrollmentCommand {
            attempt_id,
            draft_id,
            owner_user_id,
            provider_account_id,
            correlation_id: correlation_id.to_owned(),
            at: chrono::Utc::now(),
        })
        .await
        .map_err(map_service_error)?;
    Ok(crate::auth::no_store(
        Json(attempt_response(&result)).into_response(),
    ))
}

pub(super) async fn recover_course_enrollment(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path((account_id, draft_id, attempt_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let owner_user_id = auth.require_account_manage()?;
    let provider_account_id = parse_account_id(&account_id)?;
    let draft_id = parse_draft_id(&draft_id)?;
    let attempt_id = parse_attempt_id(&attempt_id)?;
    let correlation_id = required_request_id(&headers)?;
    let result = enrollment_service(&state)?
        .recover(RecoverCourseEnrollmentCommand {
            attempt_id,
            draft_id,
            owner_user_id,
            provider_account_id,
            correlation_id: correlation_id.to_owned(),
            at: chrono::Utc::now(),
        })
        .await
        .map_err(map_service_error)?;
    Ok(crate::auth::no_store(
        Json(attempt_response(&result)).into_response(),
    ))
}

fn enrollment_service(
    state: &ApiState,
) -> Result<CourseEnrollmentService<SqliteProviderAccountRepository>, ApiError> {
    let secrets = state.secret_store.clone().ok_or_else(|| {
        ApiError::service_unavailable(
            "course_enrollment_unavailable",
            "encrypted course enrollment storage is unavailable",
        )
    })?;
    Ok(CourseEnrollmentService::new(
        state.providers.clone(),
        SqliteProviderAccountRepository::new(state.database.clone()),
        Arc::new(secrets),
    ))
}

fn attempt_response(result: &CourseEnrollmentRunResult) -> CourseEnrollmentAttemptResponse {
    CourseEnrollmentAttemptResponse {
        attempt_id: result.attempt.id,
        draft_id: result.attempt.draft_id,
        state: result.attempt.state,
        updated_at: result.attempt.updated_at,
    }
}

fn parse_account_id(value: &str) -> Result<ProviderAccountId, ApiError> {
    ProviderAccountId::from_str(value)
        .map_err(|_| ApiError::bad_request("invalid_account_id", "account ID is invalid"))
}

fn parse_draft_id(value: &str) -> Result<CourseEnrollmentDraftId, ApiError> {
    CourseEnrollmentDraftId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_course_enrollment_draft_id",
            "course enrollment draft ID is invalid",
        )
    })
}

fn parse_attempt_id(value: &str) -> Result<CourseEnrollmentAttemptId, ApiError> {
    CourseEnrollmentAttemptId::from_str(value).map_err(|_| {
        ApiError::bad_request(
            "invalid_course_enrollment_attempt_id",
            "course enrollment Attempt ID is invalid",
        )
    })
}

fn required_request_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| ApiError::bad_request("invalid_request_id", "x-request-id is invalid"))
}

fn map_service_error(error: CourseEnrollmentServiceError) -> ApiError {
    match error {
        CourseEnrollmentServiceError::InvalidCorrelationId => {
            ApiError::bad_request("invalid_request_id", "x-request-id is invalid")
        }
        CourseEnrollmentServiceError::AccountNotFound => {
            ApiError::not_found("provider_account_not_found")
        }
        CourseEnrollmentServiceError::AccountNotAuthenticated => ApiError::conflict(
            "provider_account_not_authenticated",
            "the Provider account is not authenticated",
        ),
        CourseEnrollmentServiceError::ProviderNotRegistered(_)
        | CourseEnrollmentServiceError::CapabilityUnavailable(_) => ApiError::conflict(
            "course_enrollment_unavailable",
            "the Provider does not support course enrollment",
        ),
        CourseEnrollmentServiceError::ProviderResponseInvalid => ApiError::bad_gateway(
            "provider_response_invalid",
            "the Provider returned an invalid course enrollment preview",
        ),
        CourseEnrollmentServiceError::DraftNotFound => {
            ApiError::not_found("course_enrollment_draft_not_found")
        }
        CourseEnrollmentServiceError::AttemptNotFound => {
            ApiError::not_found("course_enrollment_attempt_not_found")
        }
        CourseEnrollmentServiceError::AttemptStateConflict => ApiError::conflict(
            "course_enrollment_state_conflict",
            "the enrollment Attempt changed or cannot be replayed",
        ),
        CourseEnrollmentServiceError::Provider(provider) => match provider.kind {
            ProviderErrorKind::RateLimited => ApiError::provider_rate_limited(
                provider.retry_after_seconds.unwrap_or(60).clamp(1, 86_400),
            ),
            ProviderErrorKind::Network | ProviderErrorKind::ProviderUnavailable => {
                ApiError::service_unavailable(
                    "provider_unavailable",
                    "the Provider is temporarily unavailable",
                )
            }
            ProviderErrorKind::Authentication
            | ProviderErrorKind::Authorization
            | ProviderErrorKind::HumanRequired => ApiError::conflict(
                "provider_authentication_required",
                "the Provider requires renewed authentication or human action",
            ),
            ProviderErrorKind::RemoteChanged => ApiError::conflict(
                "course_enrollment_preview_stale",
                "the remote invitation changed and must be previewed again",
            ),
            ProviderErrorKind::UnsupportedTask => ApiError::conflict(
                "course_enrollment_unsupported",
                "the invitation cannot be enrolled by this Provider",
            ),
            ProviderErrorKind::ProtocolDrift | ProviderErrorKind::InvalidResponse => {
                ApiError::bad_gateway(
                    "provider_response_invalid",
                    "the Provider course enrollment protocol changed",
                )
            }
            ProviderErrorKind::Internal => ApiError::internal(provider),
        },
        CourseEnrollmentServiceError::Storage(error) => ApiError::internal(error),
        CourseEnrollmentServiceError::SecretStorage(error) => ApiError::internal(error),
    }
}
