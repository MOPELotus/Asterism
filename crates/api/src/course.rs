use std::str::FromStr;

use asterism_domain::{
    AutomationPlanStatus, Course, CourseAggregateProgress, CourseId, ProviderAccountId, Timestamp,
};
use asterism_storage::{
    CourseAutomationPlanRepository, CourseAutomationPlanWriteOutcome,
    CourseAutomationPlanWriteRequest, CourseProgressRepository,
    SqliteCourseAutomationPlanRepository, SqliteCourseProgressRepository,
};
use axum::{
    Extension, Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, sqlite::SqliteRow};

use crate::{ApiError, ApiState, auth::AuthContext};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;

pub(super) async fn list_courses(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    query: Result<Query<CourseListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let query = query.map(|Query(query)| query).map_err(|_| {
        ApiError::bad_request(
            "invalid_course_query",
            "course query parameters are invalid",
        )
    })?;
    let account_id = query
        .provider_account_id
        .as_deref()
        .map(ProviderAccountId::from_str)
        .transpose()
        .map_err(|_| {
            ApiError::bad_request(
                "invalid_provider_account_id",
                "Provider account ID is invalid",
            )
        })?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let offset = query.offset.unwrap_or_default();
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > MAX_OFFSET {
        return Err(ApiError::bad_request(
            "invalid_course_pagination",
            "course limit must be 1-200 and offset must not exceed 1000000",
        ));
    }
    let owner = owner_id.to_string();
    let (total, rows) = if let Some(account_id) = account_id {
        let account = account_id.to_string();
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM courses AS course INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id WHERE account.owner_user_id = ? AND account.id = ?",
        )
        .bind(&owner)
        .bind(&account)
        .fetch_one(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
        let rows = sqlx::query(
            "SELECT course.* FROM courses AS course INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id WHERE account.owner_user_id = ? AND account.id = ? ORDER BY course.last_seen_at DESC, course.id DESC LIMIT ? OFFSET ?",
        )
        .bind(&owner)
        .bind(&account)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).map_err(ApiError::internal)?)
        .fetch_all(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
        (total, rows)
    } else {
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM courses AS course INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id WHERE account.owner_user_id = ?",
        )
        .bind(&owner)
        .fetch_one(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
        let rows = sqlx::query(
            "SELECT course.* FROM courses AS course INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id WHERE account.owner_user_id = ? ORDER BY course.last_seen_at DESC, course.id DESC LIMIT ? OFFSET ?",
        )
        .bind(&owner)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).map_err(ApiError::internal)?)
        .fetch_all(state.database.pool())
        .await
        .map_err(ApiError::internal)?;
        (total, rows)
    };
    let items = rows
        .iter()
        .map(decode_course)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(crate::auth::no_store(
        Json(CoursePageResponse {
            total: u64::try_from(total).map_err(ApiError::internal)?,
            limit,
            offset,
            items,
        })
        .into_response(),
    ))
}

pub(super) async fn get_course(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(course_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let course_id = CourseId::from_str(&course_id)
        .map_err(|_| ApiError::bad_request("invalid_course_id", "course ID is invalid"))?;
    let row = sqlx::query(
        "SELECT course.* FROM courses AS course INNER JOIN provider_accounts AS account ON account.id = course.provider_account_id WHERE account.owner_user_id = ? AND course.id = ?",
    )
    .bind(owner_id.to_string())
    .bind(course_id.to_string())
    .fetch_optional(state.database.pool())
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("course_not_found"))?;
    Ok(crate::auth::no_store(
        Json(decode_course(&row)?).into_response(),
    ))
}

pub(super) async fn get_course_progress(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(course_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let course_id = CourseId::from_str(&course_id)
        .map_err(|_| ApiError::bad_request("invalid_course_id", "course ID is invalid"))?;
    let record = SqliteCourseProgressRepository::new(state.database)
        .find_owned_course_aggregate_progress(owner_id, course_id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("course_not_found"))?;
    Ok(crate::auth::no_store(
        Json(CourseProgressResponse {
            course: record.course,
            progress: record.progress,
        })
        .into_response(),
    ))
}

pub(super) async fn get_course_automation(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(course_id): Path<String>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let course_id = CourseId::from_str(&course_id)
        .map_err(|_| ApiError::bad_request("invalid_course_id", "course ID is invalid"))?;
    let plan = SqliteCourseAutomationPlanRepository::new(state.database.clone())
        .find_owned_course_automation_plan(owner_id, course_id)
        .await
        .map_err(ApiError::internal)?;
    Ok(crate::auth::no_store(
        Json(CourseAutomationResponse::from_plan(plan)).into_response(),
    ))
}

pub(super) async fn configure_course_automation(
    State(state): State<ApiState>,
    Extension(auth): Extension<AuthContext>,
    Path(course_id): Path<String>,
    payload: Result<Json<ConfigureCourseAutomationRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let owner_id = auth.require_task_read()?;
    let course_id = CourseId::from_str(&course_id)
        .map_err(|_| ApiError::bad_request("invalid_course_id", "course ID is invalid"))?;
    let request = payload.map(|Json(value)| value).map_err(|_| {
        ApiError::bad_request(
            "invalid_course_automation",
            "course automation request is invalid",
        )
    })?;
    let result = SqliteCourseAutomationPlanRepository::new(state.database)
        .write_course_automation_plan(CourseAutomationPlanWriteRequest {
            owner_user_id: owner_id,
            course_id,
            enabled: request.enabled,
            updated_at: chrono::Utc::now(),
        })
        .await
        .map_err(ApiError::internal)?;
    let CourseAutomationPlanWriteOutcome::Stored(plan) = result else {
        return Err(ApiError::not_found("course_not_found"));
    };
    Ok(crate::auth::no_store(
        Json(CourseAutomationResponse::from_plan(Some(plan))).into_response(),
    ))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfigureCourseAutomationRequest {
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct CourseAutomationResponse {
    enabled: bool,
    status: Option<AutomationPlanStatus>,
    updated_at: Option<Timestamp>,
}

impl CourseAutomationResponse {
    fn from_plan(plan: Option<asterism_domain::AutomationPlan>) -> Self {
        Self {
            enabled: plan
                .as_ref()
                .is_some_and(|plan| plan.status == AutomationPlanStatus::Active),
            status: plan.as_ref().map(|plan| plan.status),
            updated_at: plan.map(|plan| plan.updated_at),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct CourseProgressResponse {
    course: Course,
    progress: CourseAggregateProgress,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct CourseListQuery {
    provider_account_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct CoursePageResponse {
    total: u64,
    limit: u32,
    offset: u64,
    items: Vec<Course>,
}

fn decode_course(row: &SqliteRow) -> Result<Course, ApiError> {
    let last_seen_at = chrono::DateTime::parse_from_rfc3339(
        row.try_get("last_seen_at").map_err(ApiError::internal)?,
    )
    .map_err(ApiError::internal)?
    .with_timezone(&chrono::Utc);
    Ok(Course {
        id: row
            .try_get::<&str, _>("id")
            .map_err(ApiError::internal)?
            .parse()
            .map_err(ApiError::internal)?,
        provider_account_id: row
            .try_get::<&str, _>("provider_account_id")
            .map_err(ApiError::internal)?
            .parse()
            .map_err(ApiError::internal)?,
        remote_id: row.try_get("remote_id").map_err(ApiError::internal)?,
        title: row.try_get("title").map_err(ApiError::internal)?,
        term: row.try_get("term").map_err(ApiError::internal)?,
        teacher: row.try_get("teacher").map_err(ApiError::internal)?,
        remote_status: row.try_get("remote_status").map_err(ApiError::internal)?,
        metadata: serde_json::from_str(row.try_get("metadata_json").map_err(ApiError::internal)?)
            .map_err(ApiError::internal)?,
        last_seen_at,
    })
}
