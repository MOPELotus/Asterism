use std::collections::BTreeMap;

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRouteContext, RemoteCourse,
    RemoteTask,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::inventory::CidarenStudyTaskDocument;

const MAX_STUDY_TASKS: usize = 10_000;
const MAX_REMOTE_COMPONENT_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 512;

#[derive(Clone, Debug)]
struct StudyTaskRow {
    task_id: i64,
    course_id: String,
    list_id: String,
    title: String,
    progress: u8,
    score: Option<f64>,
    time_spent_raw: Option<u64>,
    free_raw: Option<u64>,
    sort_no_raw: Option<i64>,
}

#[derive(Debug)]
struct StudyTaskSnapshot {
    course_id: String,
    course_name: String,
    course_progress: Option<u8>,
    course_time_spent_raw: Option<u64>,
    course_free_raw: Option<u64>,
    course_status_raw: Option<i64>,
    tasks: Vec<StudyTaskRow>,
}

/// Parses the selected self-study Course from one bounded `StudyTask/List`
/// response.
///
/// # Errors
///
/// Returns `InvalidResponse` or `ProtocolDrift` for malformed, misbound or
/// unbounded data.
pub fn parse_study_course(document: &CidarenStudyTaskDocument) -> ProviderResult<RemoteCourse> {
    let snapshot = parse_snapshot(document)?;
    Ok(RemoteCourse {
        remote_id: format!("course:{}", snapshot.course_id),
        title: snapshot.course_name,
        term: None,
        teacher: None,
        remote_status: None,
        metadata_sanitized: serde_json::json!({
            "schema": "cidaren.course.v1",
            "course_id": snapshot.course_id,
            "inventory_sources": ["study-task"],
            "study_progress": snapshot.course_progress,
            "study_time_spent_raw": snapshot.course_time_spent_raw,
            "study_free_raw": snapshot.course_free_raw,
            "study_course_status_raw": snapshot.course_status_raw,
        }),
        route_context: ProviderRouteContext::default(),
    })
}

/// Parses ordinary self-study units into stable Tasks. Their donor-observed
/// answer mutations are modeled by the same Provider-private attempt lifecycle
/// as class Tasks; registry advertisement follows durable Core integration.
///
/// Stable identity is `study-task:{course_id}:{list_id}` because audited
/// responses may expose `task_id == -1` for every uninitialized unit.
///
/// # Errors
///
/// Returns `InvalidResponse` or `ProtocolDrift` for malformed, duplicate,
/// misbound or unbounded data.
pub fn parse_study_task_inventory(
    course: Option<&RemoteCourse>,
    document: &CidarenStudyTaskDocument,
) -> ProviderResult<Vec<RemoteTask>> {
    let course_id = course.map(course_id_from_remote).transpose()?;
    let snapshot = parse_snapshot(document)?;
    if course_id
        .as_deref()
        .is_some_and(|course_id| course_id != snapshot.course_id)
    {
        return Ok(Vec::new());
    }
    snapshot.tasks.into_iter().map(normalize_task).collect()
}

fn parse_snapshot(document: &CidarenStudyTaskDocument) -> ProviderResult<StudyTaskSnapshot> {
    let root: Value = serde_json::from_str(document.as_str())
        .map_err(|_| invalid_response("Cidaren study-task response is not valid JSON"))?;
    let object = root
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren study-task response is not an object"))?;
    if required_i64(object.get("code"), "response code")? != 1 {
        return Err(invalid_response(
            "Cidaren study-task endpoint returned a non-success response",
        ));
    }
    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Cidaren study-task response has no data object"))?;
    let course_id = required_component(data.get("course_id"), "Course ID")?;
    if course_id != document.selected_course_id() {
        return Err(protocol_drift(
            "Cidaren study-task response changed the selected Course identity",
        ));
    }
    let records = data
        .get("task_list")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Cidaren study-task response has no task list"))?;
    if records.len() > MAX_STUDY_TASKS {
        return Err(invalid_response(
            "Cidaren study-task response exceeds the item limit",
        ));
    }

    let mut tasks = BTreeMap::new();
    for value in records {
        let row = parse_row(value, &course_id)?;
        if tasks.insert(row.list_id.clone(), row).is_some() {
            return Err(protocol_drift(
                "Cidaren study-task response contains a duplicate list identity",
            ));
        }
    }
    Ok(StudyTaskSnapshot {
        course_id,
        course_name: required_text(data.get("course_name"), MAX_TITLE_BYTES, "Course title")?,
        course_progress: optional_progress(data.get("progress"), "Course progress")?,
        course_time_spent_raw: optional_u64(data.get("time_spent"), "Course time spent")?,
        course_free_raw: optional_u64(data.get("free"), "Course free flag")?,
        course_status_raw: optional_i64(data.get("course_status"), "Course status")?,
        tasks: tasks.into_values().collect(),
    })
}

fn parse_row(value: &Value, selected_course_id: &str) -> ProviderResult<StudyTaskRow> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren study-task list contains a non-object row"))?;
    if required_i64(object.get("task_type"), "task type")? != 3 {
        return Err(protocol_drift(
            "Cidaren study-task row contains an unknown task type",
        ));
    }
    let course_id = required_component(object.get("course_id"), "Course ID")?;
    if course_id != selected_course_id {
        return Err(protocol_drift(
            "Cidaren study-task row changed the selected Course identity",
        ));
    }
    let task_id = required_i64(object.get("task_id"), "task ID")?;
    if task_id < -1 || task_id == 0 {
        return Err(protocol_drift(
            "Cidaren study-task row contains an invalid task ID",
        ));
    }
    Ok(StudyTaskRow {
        task_id,
        course_id,
        list_id: required_component(object.get("list_id"), "list ID")?,
        title: required_text(object.get("task_name"), MAX_TITLE_BYTES, "task title")?,
        progress: required_progress(object.get("progress"), "progress")?,
        score: optional_score(object.get("score"))?,
        time_spent_raw: optional_u64(object.get("time_spent"), "time spent")?,
        free_raw: optional_u64(object.get("free"), "free flag")?,
        sort_no_raw: optional_i64(object.get("sort_no"), "sort number")?,
    })
}

fn normalize_task(row: StudyTaskRow) -> ProviderResult<RemoteTask> {
    let remote_state = match row.progress {
        100 => RemoteState::Completed,
        0 => RemoteState::Pending,
        _ => RemoteState::InProgress,
    };
    let normalized = serde_json::json!({
        "schema": "cidaren.study-task.v1",
        "task_id": row.task_id,
        "course_id": row.course_id,
        "list_id": row.list_id,
        "task_type": "study",
        "remote_state": remote_state,
        "progress": row.progress,
        "score": row.score,
        "time_spent_raw": row.time_spent_raw,
        "free_raw": row.free_raw,
        "sort_no_raw": row.sort_no_raw,
    });
    Ok(RemoteTask {
        remote_id: format!("study-task:{}:{}", row.course_id, row.list_id),
        course_remote_id: Some(format!("course:{}", row.course_id)),
        title: row.title,
        source_type: SourceType::Practice,
        assessment_class: AssessmentClass::Routine,
        remote_state,
        opens_at: None,
        due_at: None,
        closes_at: None,
        capabilities: [
            TaskCapability::ProgressRead,
            TaskCapability::SubmissionBuild,
            TaskCapability::AnswerResolve,
            TaskCapability::SubmissionVerify,
            TaskCapability::BrowserBridge,
        ]
        .into_iter()
        .chain(row.time_spent_raw.map(|_| TaskCapability::DurationRead))
        .collect(),
        fingerprint: fingerprint(&normalized)?,
        normalized,
        raw_sanitized: serde_json::json!({
            "schema": "cidaren.study-task.raw.v1",
            "progress_raw": row.progress,
            "score_raw": row.score,
            "time_spent_raw": row.time_spent_raw,
            "free_raw": row.free_raw,
        }),
    })
}

fn course_id_from_remote(course: &RemoteCourse) -> ProviderResult<String> {
    if course
        .metadata_sanitized
        .get("schema")
        .and_then(Value::as_str)
        != Some("cidaren.course.v1")
    {
        return Err(protocol_drift(
            "Cidaren study-task inventory received a foreign Course scope",
        ));
    }
    let course_id = course
        .remote_id
        .strip_prefix("course:")
        .filter(|value| valid_component(value))
        .ok_or_else(|| protocol_drift("Cidaren Course identity is invalid"))?;
    if course
        .metadata_sanitized
        .get("course_id")
        .and_then(Value::as_str)
        != Some(course_id)
    {
        return Err(protocol_drift(
            "Cidaren Course identity does not match its metadata",
        ));
    }
    Ok(course_id.to_owned())
}

fn required_component(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    let value = match value {
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Number(value)) if value.as_u64().is_some() => value.to_string(),
        _ => {
            return Err(protocol_drift(format!(
                "Cidaren study-task response has no valid {label}"
            )));
        }
    };
    if !valid_component(&value) {
        return Err(protocol_drift(format!(
            "Cidaren study-task response contains an invalid {label}"
        )));
    }
    Ok(value)
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn required_text(
    value: Option<&Value>,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|value| {
            !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            protocol_drift(format!(
                "Cidaren study-task response contains an invalid {label}"
            ))
        })
}

fn required_i64(value: Option<&Value>, label: &'static str) -> ProviderResult<i64> {
    match value {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.trim().parse::<i64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        protocol_drift(format!(
            "Cidaren study-task response contains an invalid {label}"
        ))
    })
}

fn optional_i64(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<i64>> {
    value
        .filter(|value| !value.is_null())
        .map(|value| required_i64(Some(value), label))
        .transpose()
}

fn required_u64(value: Option<&Value>, label: &'static str) -> ProviderResult<u64> {
    match value {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        protocol_drift(format!(
            "Cidaren study-task response contains an invalid {label}"
        ))
    })
}

fn optional_u64(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<u64>> {
    value
        .filter(|value| !value.is_null())
        .map(|value| required_u64(Some(value), label))
        .transpose()
}

fn required_progress(value: Option<&Value>, label: &'static str) -> ProviderResult<u8> {
    required_u64(value, label).and_then(|value| {
        u8::try_from(value)
            .ok()
            .filter(|value| *value <= 100)
            .ok_or_else(|| protocol_drift("Cidaren study-task progress is invalid"))
    })
}

fn optional_progress(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<u8>> {
    value
        .filter(|value| !value.is_null())
        .map(|value| required_progress(Some(value), label))
        .transpose()
}

fn optional_score(value: Option<&Value>) -> ProviderResult<Option<f64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_f64()
        .filter(|score| score.is_finite() && (0.0..=100.0).contains(score))
        .map(Some)
        .ok_or_else(|| protocol_drift("Cidaren study-task score is invalid"))
}

fn fingerprint(normalized: &Value) -> ProviderResult<String> {
    let bytes = serde_json::to_vec(normalized)
        .map_err(|_| invalid_response("Cidaren normalized study Task cannot be encoded"))?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_normalizes_selected_course_and_stable_study_units() {
        let document = fixture_document();
        let course = parse_study_course(&document).unwrap();
        assert_eq!(course.remote_id, "course:course-a");
        assert_eq!(course.title, "Synthetic Course A");
        assert_eq!(course.metadata_sanitized["study_progress"], 35);

        let tasks = parse_study_task_inventory(Some(&course), &document).unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].remote_id, "study-task:course-a:course-a_01");
        assert_eq!(tasks[0].remote_state, RemoteState::Pending);
        assert_eq!(tasks[0].normalized["task_id"], -1);
        assert_eq!(tasks[1].remote_state, RemoteState::InProgress);
        assert_eq!(tasks[2].remote_state, RemoteState::Completed);
        assert_eq!(tasks[2].normalized["time_spent_raw"], 600_000);
        assert_eq!(
            tasks[0].capabilities,
            [
                TaskCapability::ProgressRead,
                TaskCapability::SubmissionBuild,
                TaskCapability::AnswerResolve,
                TaskCapability::SubmissionVerify,
                TaskCapability::BrowserBridge,
                TaskCapability::DurationRead,
            ]
        );
        assert!(tasks[0].fingerprint.starts_with("v1:"));
        assert!(
            !serde_json::to_string(&tasks)
                .unwrap()
                .contains("must-be-dropped")
        );
    }

    #[test]
    fn parser_rejects_course_drift_duplicates_and_invalid_progress() {
        let raw = include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json");
        let changed = raw.replace("\"course_id\": \"course-a\"", "\"course_id\": \"course-b\"");
        assert!(
            CidarenStudyTaskDocument::try_new("course-a", changed)
                .and_then(|document| parse_study_course(&document))
                .is_err()
        );

        let duplicate = raw.replace(
            "\"list_id\": \"course-a_02\"",
            "\"list_id\": \"course-a_01\"",
        );
        assert!(
            CidarenStudyTaskDocument::try_new("course-a", duplicate)
                .and_then(|document| parse_study_task_inventory(None, &document))
                .is_err()
        );

        let invalid = raw.replacen("\"progress\": 35", "\"progress\": 101", 2);
        assert!(
            CidarenStudyTaskDocument::try_new("course-a", invalid)
                .and_then(|document| parse_study_task_inventory(None, &document))
                .is_err()
        );
    }

    fn fixture_document() -> CidarenStudyTaskDocument {
        CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
        .unwrap()
    }
}
