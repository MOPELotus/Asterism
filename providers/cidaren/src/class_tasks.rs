use std::collections::BTreeMap;

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRouteContext, RemoteCourse,
    RemoteTask,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::inventory::CidarenClassTaskPageDocument;

const PAGE_SIZE: usize = 10;
const MAX_TASKS: usize = 10_000;
const MAX_REMOTE_COMPONENT_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 512;

#[derive(Clone, Debug)]
struct ClassTaskRow {
    release_id: String,
    task_id: i64,
    title: String,
    task_type: TaskType,
    course_id: String,
    course_name: String,
    over_status: OverStatus,
    progress: u8,
    score: Option<f64>,
    source_raw: Option<i64>,
    release_time_ms: Option<u64>,
    start_time_ms: Option<u64>,
    over_time_ms: Option<u64>,
    time_spent_raw: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskType {
    Learning,
    Test,
}

impl TaskType {
    const fn label(self) -> &'static str {
        match self {
            Self::Learning => "learning",
            Self::Test => "test",
        }
    }

    const fn source_type(self) -> SourceType {
        match self {
            Self::Learning => SourceType::Practice,
            Self::Test => SourceType::Exam,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverStatus {
    NotStarted,
    Active,
    Expired,
}

impl OverStatus {
    const fn value(self) -> u8 {
        match self {
            Self::NotStarted => 1,
            Self::Active => 2,
            Self::Expired => 3,
        }
    }
}

/// Derives unique stable Courses from one complete class-task pagination.
///
/// # Errors
///
/// Returns `InvalidResponse` or `ProtocolDrift` for incomplete, malformed,
/// conflicting or unbounded pages.
pub fn parse_course_inventory(
    pages: &[CidarenClassTaskPageDocument],
) -> ProviderResult<Vec<RemoteCourse>> {
    let rows = parse_pages(pages)?;
    let mut courses = BTreeMap::<String, RemoteCourse>::new();
    for row in rows {
        let remote_id = format!("course:{}", row.course_id);
        if let Some(existing) = courses.get(&remote_id) {
            if existing.title != row.course_name {
                return Err(protocol_drift(
                    "Cidaren class-task pages contain conflicting Course titles",
                ));
            }
            continue;
        }
        courses.insert(
            remote_id.clone(),
            RemoteCourse {
                remote_id,
                title: row.course_name,
                term: None,
                teacher: None,
                remote_status: None,
                metadata_sanitized: serde_json::json!({
                    "schema": "cidaren.course.v1",
                    "course_id": row.course_id,
                    "inventory_sources": ["class-task"],
                }),
                route_context: ProviderRouteContext::default(),
            },
        );
    }
    Ok(courses.into_values().collect())
}

/// Parses one complete class-task pagination into stable learning/test Tasks.
///
/// An optional Course scope filters the all-account endpoint after validating
/// its Cidaren identity.
///
/// # Errors
///
/// Returns `InvalidResponse` or `ProtocolDrift` for incomplete, malformed,
/// duplicate, misbound or unbounded data.
pub fn parse_task_inventory(
    course: Option<&RemoteCourse>,
    pages: &[CidarenClassTaskPageDocument],
) -> ProviderResult<Vec<RemoteTask>> {
    let course_id = course.map(course_id_from_remote).transpose()?;
    let rows = parse_pages(pages)?;
    rows.into_iter()
        .filter(|row| course_id.as_deref().is_none_or(|id| row.course_id == id))
        .map(normalize_task)
        .collect()
}

fn parse_pages(pages: &[CidarenClassTaskPageDocument]) -> ProviderResult<Vec<ClassTaskRow>> {
    if pages.is_empty() {
        return Err(invalid_response(
            "Cidaren class-task inventory contains no first page",
        ));
    }
    let mut documents = BTreeMap::new();
    for page in pages {
        if documents.insert(page.page_count(), page).is_some() {
            return Err(protocol_drift(
                "Cidaren class-task inventory contains a duplicate page",
            ));
        }
    }
    if !documents.contains_key(&1) {
        return Err(protocol_drift(
            "Cidaren class-task inventory is missing page one",
        ));
    }

    let mut expected_total = None;
    let mut page_rows = BTreeMap::new();
    for (page_number, page) in documents {
        let (total, rows) = parse_page(page.as_str())?;
        if expected_total
            .replace(total)
            .is_some_and(|known| known != total)
        {
            return Err(protocol_drift(
                "Cidaren class-task pages disagree on the total count",
            ));
        }
        page_rows.insert(page_number, rows);
    }
    let total = expected_total.ok_or_else(|| {
        invalid_response("Cidaren class-task inventory contains no readable page")
    })?;
    let expected_pages = total.div_ceil(PAGE_SIZE).max(1);
    if page_rows.len() != expected_pages {
        return Err(protocol_drift(
            "Cidaren class-task pagination is incomplete or oversized",
        ));
    }

    let mut rows = Vec::with_capacity(total);
    for expected_page in 1..=expected_pages {
        let page_number = u32::try_from(expected_page)
            .map_err(|_| invalid_response("Cidaren class-task page count exceeds the limit"))?;
        let current = page_rows
            .remove(&page_number)
            .ok_or_else(|| protocol_drift("Cidaren class-task pagination contains a page gap"))?;
        let expected_rows = if expected_page < expected_pages {
            PAGE_SIZE
        } else {
            total.saturating_sub(PAGE_SIZE * (expected_pages - 1))
        };
        if current.len() != expected_rows {
            return Err(protocol_drift(
                "Cidaren class-task page length disagrees with its total",
            ));
        }
        rows.extend(current);
    }
    if rows.len() != total {
        return Err(protocol_drift(
            "Cidaren class-task records disagree with the total count",
        ));
    }

    let mut unique = BTreeMap::new();
    for row in rows {
        if unique.insert(row.release_id.clone(), row).is_some() {
            return Err(protocol_drift(
                "Cidaren class-task inventory contains a duplicate release identity",
            ));
        }
    }
    Ok(unique.into_values().collect())
}

fn parse_page(document: &str) -> ProviderResult<(usize, Vec<ClassTaskRow>)> {
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("Cidaren class-task page is not valid JSON"))?;
    let object = root
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren class-task page is not an object"))?;
    if required_i64(object.get("code"), "response code")? != 1 {
        return Err(invalid_response(
            "Cidaren class-task endpoint returned a non-success response",
        ));
    }
    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Cidaren class-task page has no data object"))?;
    let total = required_u64(data.get("total"), "total count").and_then(|value| {
        usize::try_from(value)
            .map_err(|_| invalid_response("Cidaren class-task total exceeds the limit"))
    })?;
    if total > MAX_TASKS {
        return Err(invalid_response(
            "Cidaren class-task total exceeds the item limit",
        ));
    }
    let records = data
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Cidaren class-task page has no records array"))?;
    if records.len() > PAGE_SIZE {
        return Err(invalid_response(
            "Cidaren class-task page exceeds the audited page size",
        ));
    }
    records
        .iter()
        .map(parse_row)
        .collect::<ProviderResult<Vec<_>>>()
        .map(|rows| (total, rows))
}

pub(crate) fn class_task_total(document: &str) -> ProviderResult<usize> {
    parse_page(document).map(|(total, _)| total)
}

fn parse_row(value: &Value) -> ProviderResult<ClassTaskRow> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren class-task page contains a non-object row"))?;
    let release_id = required_positive_id(object.get("release_id"), "release ID")?;
    let task_id = required_i64(object.get("task_id"), "task ID")?;
    if task_id < -1 || task_id == 0 {
        return Err(protocol_drift(
            "Cidaren class-task row contains an invalid task ID",
        ));
    }
    let task_type = match required_i64(object.get("task_type"), "task type")? {
        1 => TaskType::Learning,
        2 => TaskType::Test,
        _ => {
            return Err(protocol_drift(
                "Cidaren class-task row contains an unknown task type",
            ));
        }
    };
    let over_status = match required_i64(object.get("over_status"), "over status")? {
        1 => OverStatus::NotStarted,
        2 => OverStatus::Active,
        3 => OverStatus::Expired,
        _ => {
            return Err(protocol_drift(
                "Cidaren class-task row contains an unknown over status",
            ));
        }
    };
    let progress = required_u64(object.get("progress"), "progress").and_then(|value| {
        u8::try_from(value)
            .ok()
            .filter(|value| *value <= 100)
            .ok_or_else(|| protocol_drift("Cidaren class-task progress is invalid"))
    })?;

    Ok(ClassTaskRow {
        release_id,
        task_id,
        title: required_text(object.get("task_name"), MAX_TITLE_BYTES, "task title")?,
        task_type,
        course_id: required_component(object.get("course_id"), "Course ID")?,
        course_name: required_text(object.get("course_name"), MAX_TITLE_BYTES, "Course title")?,
        over_status,
        progress,
        score: optional_score(object.get("score"))?,
        source_raw: optional_i64(object.get("source"), "source")?,
        release_time_ms: optional_u64(object.get("release_time"), "release time")?,
        start_time_ms: optional_u64(object.get("start_time"), "start time")?,
        over_time_ms: optional_u64(object.get("over_time"), "over time")?,
        time_spent_raw: optional_u64(object.get("time_spent"), "time spent")?,
    })
}

fn normalize_task(row: ClassTaskRow) -> ProviderResult<RemoteTask> {
    let remote_state = classify_state(row.over_status, row.progress);
    let remote_id = format!("class-task:{}", row.release_id);
    let course_remote_id = format!("course:{}", row.course_id);
    let normalized = serde_json::json!({
        "schema": "cidaren.class-task.v1",
        "release_id": row.release_id,
        "task_id": row.task_id,
        "course_id": row.course_id,
        "task_type": row.task_type.label(),
        "remote_state": remote_state,
        "over_status": row.over_status.value(),
        "progress": row.progress,
        "score": row.score,
        "release_time_ms": row.release_time_ms,
        "start_time_ms": row.start_time_ms,
        "over_time_ms": row.over_time_ms,
        "time_spent_raw": row.time_spent_raw,
    });
    let opens_at = row.start_time_ms.map(timestamp_from_millis).transpose()?;
    Ok(RemoteTask {
        remote_id,
        course_remote_id: Some(course_remote_id),
        title: row.title,
        source_type: row.task_type.source_type(),
        assessment_class: AssessmentClass::Routine,
        remote_state,
        opens_at,
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
            "schema": "cidaren.class-task.raw.v1",
            "source_raw": row.source_raw,
            "over_status_raw": row.over_status.value(),
            "progress_raw": row.progress,
            "score_raw": row.score,
            "time_spent_raw": row.time_spent_raw,
        }),
    })
}

const fn classify_state(over_status: OverStatus, progress: u8) -> RemoteState {
    if progress == 100 {
        RemoteState::Completed
    } else {
        match over_status {
            OverStatus::NotStarted => RemoteState::NotOpen,
            OverStatus::Active if progress == 0 => RemoteState::Pending,
            OverStatus::Active => RemoteState::InProgress,
            OverStatus::Expired => RemoteState::Expired,
        }
    }
}

fn course_id_from_remote(course: &RemoteCourse) -> ProviderResult<String> {
    if course
        .metadata_sanitized
        .get("schema")
        .and_then(Value::as_str)
        != Some("cidaren.course.v1")
    {
        return Err(protocol_drift(
            "Cidaren Task inventory received a foreign Course scope",
        ));
    }
    let course_id = course
        .remote_id
        .strip_prefix("course:")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_drift("Cidaren Course identity is invalid"))?;
    if !valid_component(course_id)
        || course
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

fn required_positive_id(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    let value = required_u64(value, label)?;
    if value == 0 {
        return Err(protocol_drift(format!(
            "Cidaren class-task row contains an invalid {label}"
        )));
    }
    Ok(value.to_string())
}

fn required_component(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    let value = match value {
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Number(value)) if value.as_u64().is_some() => value.to_string(),
        _ => {
            return Err(protocol_drift(format!(
                "Cidaren class-task row has no valid {label}"
            )));
        }
    };
    if !valid_component(&value) {
        return Err(protocol_drift(format!(
            "Cidaren class-task row contains an invalid {label}"
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
    let value = value
        .and_then(Value::as_str)
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|value| {
            !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| {
            protocol_drift(format!(
                "Cidaren class-task row contains an invalid {label}"
            ))
        })?;
    Ok(value)
}

fn required_i64(value: Option<&Value>, label: &'static str) -> ProviderResult<i64> {
    match value {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.trim().parse::<i64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        protocol_drift(format!(
            "Cidaren class-task row contains an invalid {label}"
        ))
    })
}

fn optional_i64(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<i64>> {
    value
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
            "Cidaren class-task row contains an invalid {label}"
        ))
    })
}

fn optional_u64(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<u64>> {
    value
        .map(|value| required_u64(Some(value), label))
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
        .ok_or_else(|| protocol_drift("Cidaren class-task score is invalid"))
}

fn timestamp_from_millis(value: u64) -> ProviderResult<DateTime<Utc>> {
    i64::try_from(value)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .ok_or_else(|| protocol_drift("Cidaren class-task start time is invalid"))
}

fn fingerprint(normalized: &Value) -> ProviderResult<String> {
    let bytes = serde_json::to_vec(normalized)
        .map_err(|_| invalid_response("Cidaren normalized Task cannot be encoded"))?;
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

    fn fixture_pages() -> Vec<CidarenClassTaskPageDocument> {
        vec![
            CidarenClassTaskPageDocument::try_new(
                1,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-1.json"),
            )
            .unwrap(),
            CidarenClassTaskPageDocument::try_new(
                2,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-2.json"),
            )
            .unwrap(),
        ]
    }

    #[test]
    fn parser_normalizes_courses_and_distinct_task_types() {
        let pages = fixture_pages();
        let courses = parse_course_inventory(&pages).unwrap();
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].remote_id, "course:course-a");
        assert_eq!(courses[0].title, "Synthetic Course A");

        let tasks = parse_task_inventory(None, &pages).unwrap();
        assert_eq!(tasks.len(), 12);
        let learning = tasks
            .iter()
            .find(|task| task.remote_id == "class-task:2001")
            .unwrap();
        assert_eq!(learning.source_type, SourceType::Practice);
        assert_eq!(learning.assessment_class, AssessmentClass::Routine);
        assert_eq!(learning.remote_state, RemoteState::Pending);
        assert_eq!(
            learning.capabilities,
            [
                TaskCapability::ProgressRead,
                TaskCapability::SubmissionBuild,
                TaskCapability::AnswerResolve,
                TaskCapability::SubmissionVerify,
                TaskCapability::BrowserBridge,
                TaskCapability::DurationRead,
            ]
        );
        assert!(learning.fingerprint.starts_with("v1:"));

        let test = tasks
            .iter()
            .find(|task| task.remote_id == "class-task:2002")
            .unwrap();
        assert_eq!(test.source_type, SourceType::Exam);
        assert_eq!(test.assessment_class, AssessmentClass::Routine);
        assert_eq!(test.remote_state, RemoteState::InProgress);
        assert_eq!(test.normalized["task_id"], -1);
        assert_eq!(test.normalized["time_spent_raw"], 125_000);
        assert!(test.closes_at.is_none());

        let completed = tasks
            .iter()
            .find(|task| task.remote_id == "class-task:2003")
            .unwrap();
        assert_eq!(completed.remote_state, RemoteState::Completed);
        let expired = tasks
            .iter()
            .find(|task| task.remote_id == "class-task:2004")
            .unwrap();
        assert_eq!(expired.remote_state, RemoteState::Expired);

        let encoded = serde_json::to_string(&tasks).unwrap();
        assert!(!encoded.contains("must-be-dropped"));
    }

    #[test]
    fn task_filter_requires_a_bound_cidaren_course() {
        let pages = fixture_pages();
        let course = parse_course_inventory(&pages).unwrap().remove(0);
        let tasks = parse_task_inventory(Some(&course), &pages).unwrap();
        assert!(
            tasks
                .iter()
                .all(|task| task.course_remote_id.as_deref() == Some("course:course-a"))
        );

        let mut foreign = course;
        foreign.metadata_sanitized = serde_json::json!({"schema": "other"});
        assert!(parse_task_inventory(Some(&foreign), &pages).is_err());
    }

    #[test]
    fn incomplete_duplicate_and_conflicting_pages_fail_closed() {
        let pages = fixture_pages();
        assert!(parse_task_inventory(None, &pages[..1]).is_err());

        let duplicate = vec![
            CidarenClassTaskPageDocument::try_new(
                1,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-1.json"),
            )
            .unwrap(),
            CidarenClassTaskPageDocument::try_new(
                1,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-1.json"),
            )
            .unwrap(),
        ];
        assert!(parse_task_inventory(None, &duplicate).is_err());

        let conflict = [CidarenClassTaskPageDocument::try_new(
            1,
            r#"{"code":1,"data":{"records":[
                {"release_id":1,"task_id":1,"task_name":"A","task_type":1,"course_id":"same","course_name":"One","over_status":2,"progress":0},
                {"release_id":2,"task_id":2,"task_name":"B","task_type":1,"course_id":"same","course_name":"Two","over_status":2,"progress":0}
            ],"total":2}}"#,
        )
        .unwrap()];
        assert!(parse_course_inventory(&conflict).is_err());
    }

    #[test]
    fn invalid_identity_status_progress_and_page_shape_fail_closed() {
        let invalid = |record: &str| {
            CidarenClassTaskPageDocument::try_new(
                1,
                format!(r#"{{"code":1,"data":{{"records":[{record}],"total":1}}}}"#),
            )
            .unwrap()
        };
        let base = r#"{"release_id":1,"task_id":1,"task_name":"A","task_type":1,"course_id":"course","course_name":"Course","over_status":2,"progress":0}"#;
        assert!(
            parse_task_inventory(
                None,
                &[invalid(&base.replace("1,\"task_id", "0,\"task_id"))]
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(
                None,
                &[invalid(&base.replace("\"task_type\":1", "\"task_type\":9"))]
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(
                None,
                &[invalid(
                    &base.replace("\"over_status\":2", "\"over_status\":9")
                )]
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(
                None,
                &[invalid(&base.replace("\"progress\":0", "\"progress\":101"))]
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(None, &[invalid(&base.replace("\"course\"", "\"bad/id\""))])
                .is_err()
        );
    }
}
