use std::collections::BTreeMap;

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRouteContext, RemoteCourse,
};
use serde_json::Value;

const MAX_COURSE_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_COURSES: usize = 2_048;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 512;

/// Parses one sanitized `WELearn` `authCourse.aspx?action=gmc` response.
///
/// The parser retains only the donor-observed Course identity, title and
/// completion percentage. Unknown response fields are intentionally discarded.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for oversized,
/// malformed or duplicate Course rows.
pub fn parse_course_inventory(document: &str) -> ProviderResult<Vec<RemoteCourse>> {
    if document.is_empty() || document.len() > MAX_COURSE_DOCUMENT_BYTES {
        return Err(invalid_response(
            "WELearn Course inventory is empty or exceeds the size limit",
        ));
    }
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("WELearn Course inventory is not valid JSON"))?;
    let rows = root
        .get("clist")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("WELearn Course inventory has no clist array"))?;
    if rows.len() > MAX_COURSES {
        return Err(invalid_response(
            "WELearn Course inventory exceeds the item limit",
        ));
    }

    let mut courses = BTreeMap::new();
    for row in rows {
        let object = row
            .as_object()
            .ok_or_else(|| protocol_drift("WELearn Course inventory contains a non-object row"))?;
        let course_id = required_remote_component(object.get("cid"), "Course ID")?;
        let title = required_text(object.get("name"), MAX_TITLE_BYTES, "Course title")?;
        let completion_percent = optional_percentage(object.get("per"))?;
        let remote_id = format!("course:{course_id}");
        let route_context =
            ProviderRouteContext::try_from_pairs([("welearn.cid".to_owned(), course_id)])?;
        let course = RemoteCourse {
            remote_id: remote_id.clone(),
            title,
            term: None,
            teacher: None,
            remote_status: completion_percent.map(|value| format!("{value}%")),
            metadata_sanitized: serde_json::json!({
                "schema": "welearn.course.v1",
                "completion_percent": completion_percent,
            }),
            route_context,
        };
        if courses.insert(remote_id, course).is_some() {
            return Err(protocol_drift(
                "WELearn Course inventory contains a duplicate Course identity",
            ));
        }
    }
    Ok(courses.into_values().collect())
}

pub(crate) fn course_id_from_remote(course: &RemoteCourse) -> ProviderResult<String> {
    if course
        .metadata_sanitized
        .get("schema")
        .and_then(Value::as_str)
        != Some("welearn.course.v1")
    {
        return Err(protocol_drift(
            "WELearn inventory received a foreign Course",
        ));
    }
    let course_id = course
        .route_context
        .get("welearn.cid")
        .ok_or_else(|| protocol_drift("WELearn Course has no scan-local Course ID"))?;
    let course_id =
        required_remote_component(Some(&Value::String(course_id.to_owned())), "Course ID")?;
    if course.remote_id != format!("course:{course_id}") {
        return Err(protocol_drift(
            "WELearn Course route identity does not match its remote identity",
        ));
    }
    Ok(course_id)
}

pub(crate) fn required_remote_component(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<String> {
    let value = match value {
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Number(value)) if value.as_u64().is_some() => value.to_string(),
        _ => {
            return Err(protocol_drift(format!(
                "WELearn inventory has no valid {label}"
            )));
        }
    };
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(protocol_drift(format!(
            "WELearn inventory contains an invalid {label}"
        )));
    }
    Ok(value)
}

pub(crate) fn required_text(
    value: Option<&Value>,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<String> {
    let value = value
        .and_then(Value::as_str)
        .map(normalize_text)
        .ok_or_else(|| protocol_drift(format!("WELearn inventory has no valid {label}")))?;
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(protocol_drift(format!(
            "WELearn inventory contains an invalid {label}"
        )));
    }
    Ok(value)
}

pub(crate) fn optional_scalar_text(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<Option<String>> {
    value
        .map(|value| scalar_text(Some(value), label))
        .transpose()
}

fn scalar_text(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    match value {
        Some(Value::String(value)) => Ok(value.trim().to_owned()),
        Some(Value::Number(value)) if value.as_u64().is_some() => Ok(value.to_string()),
        Some(Value::Bool(value)) => Ok(value.to_string()),
        _ => Err(protocol_drift(format!(
            "WELearn inventory has no valid {label}"
        ))),
    }
}

fn optional_percentage(value: Option<&Value>) -> ProviderResult<Option<u8>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Number(number) => number.as_u64().and_then(|value| u8::try_from(value).ok()),
        Value::String(value) => value.trim().parse::<u8>().ok(),
        _ => None,
    }
    .filter(|value| *value <= 100)
    .ok_or_else(|| protocol_drift("WELearn Course completion percentage is invalid"))?;
    Ok(Some(parsed))
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

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const EXPECTED: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.expected.json");

    #[test]
    fn parser_normalizes_ids_and_drops_unknown_fields() {
        let courses = parse_course_inventory(COURSES).unwrap();
        let actual = serde_json::to_value(&courses).unwrap();
        let expected: Value = serde_json::from_str(EXPECTED).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(courses[0].route_context.get("welearn.cid"), Some("1001"));
        let encoded = serde_json::to_string(&courses).unwrap();
        assert!(!encoded.contains("must-be-dropped"));
        assert!(!encoded.contains("welearn.cid"));
    }

    #[test]
    fn malformed_duplicate_or_unbounded_rows_fail_closed() {
        assert!(parse_course_inventory(r#"{"clist":[null]}"#).is_err());
        assert!(
            parse_course_inventory(
                r#"{"clist":[{"cid":"same","name":"A"},{"cid":"same","name":"B"}]}"#,
            )
            .is_err()
        );
        assert!(parse_course_inventory(r#"{"clist":[{"cid":"bad/id","name":"A"}]}"#).is_err());
        assert!(parse_course_inventory(r#"{"clist":[{"cid":true,"name":"A"}]}"#).is_err());
        assert!(parse_course_inventory(r#"{"clist":[{"cid":1,"name":"A","per":101}]}"#).is_err());
    }
}
