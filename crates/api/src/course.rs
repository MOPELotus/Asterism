use std::str::FromStr;

use asterism_domain::{Course, CourseAggregateProgress, CourseId};
use asterism_storage::{CourseProgressRepository, SqliteCourseProgressRepository};
use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{ApiError, ApiState, auth::AuthContext};

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

#[derive(Clone, Debug, PartialEq, Serialize)]
struct CourseProgressResponse {
    course: Course,
    progress: CourseAggregateProgress,
}
