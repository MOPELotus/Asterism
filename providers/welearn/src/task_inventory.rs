use std::{collections::BTreeMap, fmt};

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
use asterism_provider_api::{ProviderError, ProviderResult, RemoteCourse, RemoteTask};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::course_inventory::{
    course_id_from_remote, invalid_response, optional_scalar_text, protocol_drift,
    required_remote_component, required_text,
};

const MAX_UNIT_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_LEAVES_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_TOTAL_LEAVES_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_UNITS: usize = 512;
const MAX_TASKS: usize = 8_192;
const MAX_TITLE_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
struct WellearnUnit {
    index: u32,
    title: String,
    code: Option<String>,
    visible: bool,
}

/// One bounded, sanitized `scoLeaves` response bound to its zero-based Unit
/// index. The response body is redacted from diagnostics and zeroized on drop.
pub struct WellearnScoLeavesDocument {
    unit_index: u32,
    document: String,
}

impl WellearnScoLeavesDocument {
    /// Binds a response to the Unit index used in the `scoLeaves` request.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized response.
    pub fn try_new(unit_index: u32, document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_LEAVES_DOCUMENT_BYTES {
            document.zeroize();
            return Err(invalid_response(
                "WELearn SCO-leaves response is empty or exceeds the size limit",
            ));
        }
        Ok(Self {
            unit_index,
            document,
        })
    }
}

impl fmt::Debug for WellearnScoLeavesDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnScoLeavesDocument")
            .field("unit_index", &self.unit_index)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for WellearnScoLeavesDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Parses sanitized `courseunits` and `scoLeaves` responses into Resource Tasks.
///
/// Completion, visibility and donor-labelled learning time remain separate
/// observations. No runtime capability is attached at this fixture-only stage.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for unbounded,
/// malformed, misbound or duplicate Unit/SCO data.
pub fn parse_task_inventory(
    course: &RemoteCourse,
    units_document: &str,
    leaves_documents: &[WellearnScoLeavesDocument],
) -> ProviderResult<Vec<RemoteTask>> {
    let course_id = course_id_from_remote(course)?;
    let units = parse_units(units_document)?;
    if leaves_documents.len() != units.len() {
        return Err(protocol_drift(
            "WELearn Task inventory does not contain one SCO response per Unit",
        ));
    }
    let total_bytes = leaves_documents
        .iter()
        .try_fold(0_usize, |total, document| {
            total.checked_add(document.document.len())
        })
        .ok_or_else(|| invalid_response("WELearn SCO responses exceed the total size limit"))?;
    if total_bytes > MAX_TOTAL_LEAVES_BYTES {
        return Err(invalid_response(
            "WELearn SCO responses exceed the total size limit",
        ));
    }

    let mut documents = BTreeMap::new();
    for document in leaves_documents {
        if documents.insert(document.unit_index, document).is_some() {
            return Err(protocol_drift(
                "WELearn Task inventory contains duplicate Unit responses",
            ));
        }
    }

    let mut tasks = BTreeMap::new();
    for (unit_index, document) in documents {
        let unit =
            units
                .get(usize::try_from(unit_index).map_err(|_| {
                    protocol_drift("WELearn SCO response has an invalid Unit index")
                })?)
                .filter(|unit| unit.index == unit_index)
                .ok_or_else(|| {
                    protocol_drift("WELearn SCO response references an unknown Unit index")
                })?;
        for task in parse_leaves(course, &course_id, unit, &document.document)? {
            let remote_id = task.remote_id.clone();
            if tasks.insert(remote_id, task).is_some() {
                return Err(protocol_drift(
                    "WELearn Task inventory contains a duplicate Course/SCO identity",
                ));
            }
            if tasks.len() > MAX_TASKS {
                return Err(invalid_response(
                    "WELearn Task inventory exceeds the item limit",
                ));
            }
        }
    }
    Ok(tasks.into_values().collect())
}

fn parse_units(document: &str) -> ProviderResult<Vec<WellearnUnit>> {
    if document.is_empty() || document.len() > MAX_UNIT_DOCUMENT_BYTES {
        return Err(invalid_response(
            "WELearn Unit inventory is empty or exceeds the size limit",
        ));
    }
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("WELearn Unit inventory is not valid JSON"))?;
    let rows = root
        .get("info")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("WELearn Unit inventory has no info array"))?;
    if rows.len() > MAX_UNITS {
        return Err(invalid_response(
            "WELearn Unit inventory exceeds the item limit",
        ));
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let object = row.as_object().ok_or_else(|| {
                protocol_drift("WELearn Unit inventory contains a non-object row")
            })?;
            Ok(WellearnUnit {
                index: u32::try_from(index)
                    .map_err(|_| invalid_response("WELearn Unit index exceeds the limit"))?,
                title: required_text(object.get("unitname"), MAX_TITLE_BYTES, "Unit title")?,
                code: optional_bounded_text(object.get("name"), MAX_TITLE_BYTES, "Unit code")?,
                visible: required_bool_like(object.get("visible"), "Unit visibility")?,
            })
        })
        .collect()
}

pub(crate) fn unit_count(document: &str) -> ProviderResult<usize> {
    parse_units(document).map(|units| units.len())
}

fn parse_leaves(
    course: &RemoteCourse,
    course_id: &str,
    unit: &WellearnUnit,
    document: &str,
) -> ProviderResult<Vec<RemoteTask>> {
    let root: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("WELearn SCO-leaves response is not valid JSON"))?;
    let rows = root
        .get("info")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("WELearn SCO-leaves response has no info array"))?;
    let mut tasks = Vec::with_capacity(rows.len());
    for (sco_index, row) in rows.iter().enumerate() {
        let object = row.as_object().ok_or_else(|| {
            protocol_drift("WELearn SCO-leaves response contains a non-object row")
        })?;
        let sco_id = required_remote_component(object.get("id"), "SCO ID")?;
        let title = optional_bounded_text(object.get("location"), MAX_TITLE_BYTES, "SCO title")?
            .unwrap_or_else(|| sco_id.clone());
        let leaf_visibility = optional_bool_like(object.get("isvisible"), "SCO visibility")?;
        let leaf_visible = leaf_visibility.unwrap_or(unit.visible);
        let visible = unit.visible && leaf_visible;
        let completion_raw = optional_scalar_text(object.get("iscomplete"), "SCO completion")?;
        let completion_observation = classify_completion(completion_raw.as_deref());
        let duration_raw = optional_scalar_text(object.get("learntime"), "SCO learning time")?;
        let remote_state = if visible {
            completion_observation
        } else {
            RemoteState::NotOpen
        };
        let remote_id = format!("sco:{course_id}:{sco_id}");
        let normalized = serde_json::json!({
            "schema": "welearn.sco.v2",
            "course_id": course_id,
            "unit_index": unit.index,
            "sco_index": sco_index,
            "unit_title": unit.title,
            "unit_code": unit.code,
            "sco_id": sco_id,
            "unit_visible": unit.visible,
            "sco_visible": leaf_visibility,
            "visible": visible,
            "completion_observation": completion_observation,
            "duration_raw": duration_raw,
        });
        let capabilities = vec![
            TaskCapability::ProgressRead,
            TaskCapability::ResourceExecution,
            TaskCapability::ExecutionVerify,
            TaskCapability::DurationRead,
            TaskCapability::DurationReport,
        ];
        tasks.push(RemoteTask {
            remote_id,
            course_remote_id: Some(course.remote_id.clone()),
            title,
            source_type: SourceType::Resource,
            assessment_class: AssessmentClass::Unknown,
            remote_state,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities,
            fingerprint: fingerprint(&normalized)?,
            normalized,
            raw_sanitized: serde_json::json!({
                "schema": "welearn.sco.raw.v2",
                "completion_raw": completion_raw,
                "visibility_raw": object.get("isvisible").map(sanitized_scalar).transpose()?,
                "duration_raw": duration_raw,
            }),
        });
    }
    Ok(tasks)
}

fn classify_completion(value: Option<&str>) -> RemoteState {
    let Some(value) = value else {
        return RemoteState::Unknown;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "已完成" | "completed" | "complete" | "true" | "1" => RemoteState::Completed,
        "学习中" | "进行中" | "in_progress" => RemoteState::InProgress,
        "未完成" | "未学习" | "not_attempted" | "incomplete" | "false" | "0" => {
            RemoteState::Pending
        }
        _ => RemoteState::Unknown,
    }
}

fn required_bool_like(value: Option<&Value>, label: &'static str) -> ProviderResult<bool> {
    optional_bool_like(value, label)?
        .ok_or_else(|| protocol_drift(format!("WELearn inventory has no {label}")))
}

fn optional_bool_like(value: Option<&Value>, label: &'static str) -> ProviderResult<Option<bool>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) if value.as_u64() == Some(1) => Some(true),
        Value::Number(value) if value.as_u64() == Some(0) => Some(false),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
    .ok_or_else(|| protocol_drift(format!("WELearn inventory has an invalid {label}")))?;
    Ok(Some(parsed))
}

fn optional_bounded_text(
    value: Option<&Value>,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| protocol_drift(format!("WELearn inventory has an invalid {label}")))?;
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(protocol_drift(format!(
            "WELearn inventory contains an invalid {label}"
        )));
    }
    Ok(Some(value))
}

fn sanitized_scalar(value: &Value) -> ProviderResult<Value> {
    match value {
        Value::String(value) if value.len() <= 128 && !value.chars().any(char::is_control) => {
            Ok(Value::String(value.clone()))
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => Ok(value.clone()),
        _ => Err(protocol_drift(
            "WELearn inventory contains an invalid scalar observation",
        )),
    }
}

fn fingerprint(normalized: &Value) -> Result<String, ProviderError> {
    let bytes = serde_json::to_vec(normalized)
        .map_err(|_| invalid_response("WELearn normalized Task cannot be encoded"))?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_course_inventory;

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");
    const EXPECTED: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/list-mixed.expected.json");

    #[test]
    fn parser_keeps_completion_visibility_and_duration_separate() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let documents = [
            WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap(),
            WellearnScoLeavesDocument::try_new(1, UNIT_ONE).unwrap(),
        ];
        let tasks = parse_task_inventory(course, UNITS, &documents).unwrap();
        let actual = serde_json::to_value(&tasks).unwrap();
        let expected: Value = serde_json::from_str(EXPECTED).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(tasks[0].remote_state, RemoteState::Completed);
        assert_eq!(tasks[1].remote_state, RemoteState::Unknown);
        assert_eq!(tasks[2].remote_state, RemoteState::NotOpen);
        assert_eq!(
            tasks[0].capabilities,
            [
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
                TaskCapability::DurationRead,
                TaskCapability::DurationReport,
            ]
        );
        assert_eq!(
            tasks[1].capabilities,
            [
                TaskCapability::ProgressRead,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
                TaskCapability::DurationRead,
                TaskCapability::DurationReport,
            ]
        );
        assert_eq!(tasks[2].capabilities, tasks[0].capabilities);
        assert!(tasks.iter().all(|task| task.fingerprint.starts_with("v1:")));
        assert_eq!(tasks[1].normalized["sco_visible"], Value::Null);
        assert_eq!(tasks[2].normalized["unit_visible"], false);
        assert_eq!(tasks[2].normalized["sco_visible"], true);
        assert_eq!(
            tasks[2].normalized["completion_observation"],
            serde_json::json!("completed")
        );
    }

    #[test]
    fn learning_time_does_not_imply_completion() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let document = WellearnScoLeavesDocument::try_new(
            0,
            r#"{"info":[{"id":"duration-only","location":"Practice","learntime":600}]}"#,
        )
        .unwrap();
        let empty_locked = WellearnScoLeavesDocument::try_new(1, r#"{"info":[]}"#).unwrap();
        let tasks = parse_task_inventory(course, UNITS, &[document, empty_locked]).unwrap();
        assert_eq!(tasks[0].remote_state, RemoteState::Unknown);
        assert_eq!(tasks[0].normalized["duration_raw"], "600");
    }

    #[test]
    fn duplicate_or_misbound_sco_documents_fail_closed() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let duplicate_documents = [
            WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap(),
            WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap(),
        ];
        assert!(parse_task_inventory(course, UNITS, &duplicate_documents).is_err());

        let unknown = WellearnScoLeavesDocument::try_new(99, UNIT_ZERO).unwrap();
        assert!(parse_task_inventory(course, UNITS, &[unknown]).is_err());
    }

    #[test]
    fn duplicate_sco_identity_across_units_fails_closed() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let duplicate = WellearnScoLeavesDocument::try_new(1, UNIT_ZERO).unwrap();
        let original = WellearnScoLeavesDocument::try_new(0, UNIT_ZERO).unwrap();
        assert!(parse_task_inventory(course, UNITS, &[original, duplicate]).is_err());
    }
}
