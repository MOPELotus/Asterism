use std::{collections::BTreeMap, fmt};

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRouteContext, RemoteCourse,
};
use serde_json::{Map, Value};

const MAX_COURSE_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_COURSE_RESOURCES: usize = 2_048;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_ROUTE_VALUE_BYTES: usize = 512;
const MAX_TITLE_BYTES: usize = 240;

/// Fresh route facts resolved from one Course-resource detail response.
pub struct UaiCourseContext {
    course_resource_id: String,
    course_instance_id: String,
}

impl UaiCourseContext {
    pub fn course_resource_id(&self) -> &str {
        &self.course_resource_id
    }

    pub fn course_instance_id(&self) -> &str {
        &self.course_instance_id
    }
}

impl fmt::Debug for UaiCourseContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCourseContext")
            .field("course_resource_id", &self.course_resource_id)
            .field("course_instance_id", &"[ROUTE]")
            .finish()
    }
}

/// Parses one sanitized `getCourseListByStudent` response.
///
/// Each Course-resource row becomes one Asterism Course because that identity
/// selects a distinct tutorial tree and progress record.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for oversized,
/// malformed, inconsistent or duplicate rows.
pub fn parse_course_inventory(document: &str) -> ProviderResult<Vec<RemoteCourse>> {
    let root = parse_object(document, "Course inventory")?;
    require_success(&root, "Course inventory")?;
    let courses = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("courseList"))
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI Course inventory has no value.courseList array"))?;

    let mut resources = BTreeMap::new();
    for course in courses {
        let course = course.as_object().ok_or_else(|| {
            protocol_drift("UAI Course inventory contains a non-object Course row")
        })?;
        let course_title = required_text(course.get("name"), "Course title")?;
        let rows = course
            .get("courseResourceList")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI Course inventory has no Course-resource array"))?;
        for row in rows {
            let row = row.as_object().ok_or_else(|| {
                protocol_drift("UAI Course inventory contains a non-object resource row")
            })?;
            let resource_id = required_remote_component(row.get("id"), "Course-resource ID")?;
            let resource_title = required_text(row.get("name"), "Course-resource title")?;
            let (finished_points, total_points) = point_progress(row)?;
            let title = joined_title(&course_title, &resource_title)?;
            let remote_id = format!("course-resource:{resource_id}");
            let route_context = ProviderRouteContext::try_from_pairs([(
                "uai.course_resource_id".to_owned(),
                resource_id,
            )])?;
            let course = RemoteCourse {
                remote_id: remote_id.clone(),
                title,
                term: None,
                teacher: None,
                remote_status: match (finished_points, total_points) {
                    (Some(finished), Some(total)) => Some(format!("{finished}/{total}")),
                    _ => None,
                },
                metadata_sanitized: serde_json::json!({
                    "schema": "uai.course-resource.v1",
                    "finished_points": finished_points,
                    "total_points": total_points,
                }),
                route_context,
            };
            if resources.insert(remote_id, course).is_some() {
                return Err(protocol_drift(
                    "UAI Course inventory contains a duplicate Course-resource identity",
                ));
            }
            if resources.len() > MAX_COURSE_RESOURCES {
                return Err(invalid_response(
                    "UAI Course inventory exceeds the item limit",
                ));
            }
        }
    }
    Ok(resources.into_values().collect())
}

/// Resolves a fresh Course instance and binds it to one normalized Course.
///
/// # Errors
///
/// Returns protocol drift when the detail belongs to another resource or has
/// no bounded Course-instance route.
pub fn parse_course_context(
    course: &RemoteCourse,
    document: &str,
) -> ProviderResult<UaiCourseContext> {
    let resource_id = course_resource_id_from_remote(course)?;
    parse_course_context_for_resource_id(resource_id, document)
}

pub(crate) fn parse_course_context_for_resource_id(
    resource_id: String,
    document: &str,
) -> ProviderResult<UaiCourseContext> {
    let resource_id =
        required_remote_component(Some(&Value::String(resource_id)), "Course-resource ID")?;
    let root = parse_object(document, "Course-resource detail")?;
    require_success(&root, "Course-resource detail")?;
    let detail = root
        .get("value")
        .and_then(Value::as_object)
        .and_then(|value| value.get("courseResource"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            protocol_drift("UAI Course-resource detail has no value.courseResource object")
        })?;
    let actual =
        required_remote_component(detail.get("courseResourceId"), "detail Course-resource ID")?;
    if actual != resource_id {
        return Err(protocol_drift(
            "UAI Course-resource detail does not match its Course identity",
        ));
    }
    let course_instance_id =
        required_route_value(detail.get("courseInstanceId"), "Course-instance route")?;
    Ok(UaiCourseContext {
        course_resource_id: resource_id,
        course_instance_id,
    })
}

pub(crate) fn course_resource_id_from_remote(course: &RemoteCourse) -> ProviderResult<String> {
    if course
        .metadata_sanitized
        .get("schema")
        .and_then(Value::as_str)
        != Some("uai.course-resource.v1")
    {
        return Err(protocol_drift("UAI parser received a foreign Course"));
    }
    let resource_id = course
        .route_context
        .get("uai.course_resource_id")
        .ok_or_else(|| protocol_drift("UAI Course has no scan-local resource ID"))?;
    let resource_id = required_remote_component(
        Some(&Value::String(resource_id.to_owned())),
        "Course-resource ID",
    )?;
    if course.remote_id != format!("course-resource:{resource_id}") {
        return Err(protocol_drift(
            "UAI Course route identity does not match its remote identity",
        ));
    }
    Ok(resource_id)
}

pub(crate) fn required_remote_component(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<String> {
    let value = scalar_unsigned_or_string(value)
        .ok_or_else(|| protocol_drift(format!("UAI response has no valid {label}")))?;
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(protocol_drift(format!(
            "UAI response contains an invalid {label}"
        )));
    }
    Ok(value)
}

pub(crate) fn required_text(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    let value = value
        .and_then(Value::as_str)
        .map(normalize_text)
        .ok_or_else(|| protocol_drift(format!("UAI response has no valid {label}")))?;
    if value.is_empty() || value.len() > MAX_TITLE_BYTES || value.chars().any(char::is_control) {
        return Err(protocol_drift(format!(
            "UAI response contains an invalid {label}"
        )));
    }
    Ok(value)
}

fn parse_object(document: &str, label: &'static str) -> ProviderResult<Map<String, Value>> {
    if document.is_empty() || document.len() > MAX_COURSE_DOCUMENT_BYTES {
        return Err(invalid_response(format!(
            "UAI {label} is empty or exceeds the size limit"
        )));
    }
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response(format!("UAI {label} is not valid JSON")))?;
    match root {
        Value::Object(root) => Ok(root),
        _ => Err(protocol_drift(format!("UAI {label} is not an object"))),
    }
}

fn require_success(root: &Map<String, Value>, label: &'static str) -> ProviderResult<()> {
    if root.get("code").and_then(Value::as_i64) != Some(1)
        || root.get("success").and_then(Value::as_bool) != Some(true)
    {
        return Err(protocol_drift(format!("UAI {label} did not succeed")));
    }
    Ok(())
}

fn required_route_value(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    let value = value
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| protocol_drift(format!("UAI response has no valid {label}")))?;
    if value.is_empty()
        || value.len() > MAX_ROUTE_VALUE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(protocol_drift(format!(
            "UAI response contains an invalid {label}"
        )));
    }
    Ok(value.to_owned())
}

fn point_progress(row: &Map<String, Value>) -> ProviderResult<(Option<u64>, Option<u64>)> {
    let finished = optional_unsigned(row.get("finishPointNum"), "finished point count")?;
    let total = optional_unsigned(row.get("totalPointNum"), "total point count")?;
    if finished.is_some() != total.is_some() {
        return Err(protocol_drift(
            "UAI Course-resource point progress is incomplete",
        ));
    }
    if finished
        .zip(total)
        .is_some_and(|(finished, total)| finished > total)
    {
        return Err(protocol_drift(
            "UAI Course-resource point progress exceeds its total",
        ));
    }
    Ok((finished, total))
}

fn optional_unsigned(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<u64>> {
    value
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| protocol_drift(format!("UAI response has an invalid {label}")))
        })
        .transpose()
}

fn scalar_unsigned_or_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) => Some(value.trim().to_owned()),
        Some(Value::Number(value)) if value.as_u64().is_some() => Some(value.to_string()),
        _ => None,
    }
}

fn joined_title(course: &str, resource: &str) -> ProviderResult<String> {
    if course == resource {
        return Ok(course.to_owned());
    }
    let title = format!("{course} / {resource}");
    if title.len() > MAX_TITLE_BYTES * 2 + 3 {
        return Err(protocol_drift(
            "UAI combined Course title exceeds the limit",
        ));
    }
    Ok(title)
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

pub(crate) fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");

    #[test]
    fn parser_flattens_resources_and_drops_route_noise() {
        let courses = parse_course_inventory(COURSES).unwrap();
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].remote_id, "course-resource:2001");
        assert_eq!(courses[0].title, "综合英语 / 教程 A");
        assert_eq!(courses[0].remote_status.as_deref(), Some("2/10"));
        assert_eq!(
            courses[0].route_context.get("uai.course_resource_id"),
            Some("2001")
        );
        let encoded = serde_json::to_string(&courses).unwrap();
        assert!(!encoded.contains("class-must-drop"));
        assert!(!encoded.contains("instance-must-drop"));
        assert!(!encoded.contains("uai.course_resource_id"));

        let context = parse_course_context(&courses[0], DETAIL).unwrap();
        assert_eq!(context.course_resource_id(), "2001");
        assert_eq!(
            context.course_instance_id(),
            "course-v2:synthetic+rw+20260809"
        );
        assert!(!format!("{context:?}").contains("course-v2"));
    }

    #[test]
    fn malformed_duplicate_or_misbound_rows_fail_closed() {
        assert!(parse_course_inventory(r#"{"value":{"courseList":[null]}}"#).is_err());
        assert!(
            parse_course_inventory(
                r#"{"code":1,"success":true,"value":{"courseList":[{"name":"A","courseResourceList":[{"id":1,"name":"X"},{"id":1,"name":"Y"}]}]}}"#,
            )
            .is_err()
        );
        assert!(
            parse_course_inventory(
                r#"{"code":1,"success":true,"value":{"courseList":[{"name":"A","courseResourceList":[{"id":1,"name":"X","finishPointNum":2,"totalPointNum":1}]}]}}"#,
            )
            .is_err()
        );
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        assert!(
            parse_course_context(
                &course,
                r#"{"code":1,"success":true,"value":{"courseResource":{"courseResourceId":2002,"courseInstanceId":"other"}}}"#,
            )
            .is_err()
        );
    }
}
