use std::collections::BTreeMap;

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
use asterism_provider_api::{ProviderError, ProviderResult, RemoteCourse, RemoteTask};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::course_inventory::{
    UaiCourseContext, course_resource_id_from_remote, invalid_response, protocol_drift,
    required_remote_component, required_text,
};
use crate::question::supports_question_read;

const MAX_TREE_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NESTED_COURSE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_NODES: usize = 8_192;
const MAX_DEPTH: usize = 32;
const MAX_TASK_TYPES: usize = 32;
const MAX_TASK_TYPE_BYTES: usize = 64;

#[derive(Clone, Debug, Default)]
struct Hierarchy {
    unit: Option<NodeLabel>,
    section: Option<NodeLabel>,
    micro: Option<NodeLabel>,
}

#[derive(Clone, Debug)]
struct NodeLabel {
    id: String,
    title: String,
}

/// Parses one bound `course/api/course/{instance}/default` response.
///
/// The outer `course` string is decoded as a second JSON document. Stable
/// Group identities become Tasks while Unit, Section and Node ancestors remain
/// normalized hierarchy facts.
///
/// # Errors
///
/// Returns a typed invalid-response or protocol-drift error for oversized,
/// malformed, unbound, deeply nested or duplicate task trees.
pub fn parse_task_inventory(
    course: &RemoteCourse,
    context: &UaiCourseContext,
    document: &str,
) -> ProviderResult<Vec<RemoteTask>> {
    let resource_id = course_resource_id_from_remote(course)?;
    if context.course_resource_id() != resource_id {
        return Err(protocol_drift(
            "UAI Task tree context does not match its Course identity",
        ));
    }
    if document.is_empty() || document.len() > MAX_TREE_DOCUMENT_BYTES {
        return Err(invalid_response(
            "UAI Task tree is empty or exceeds the size limit",
        ));
    }
    let outer: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI Task tree is not valid JSON"))?;
    let outer = outer
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Task tree is not an object"))?;
    if outer
        .get("code")
        .is_some_and(|value| value.as_i64() != Some(0))
    {
        return Err(protocol_drift("UAI Task-tree read did not succeed"));
    }
    let nested = outer
        .get("course")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI Task tree has no nested course document"))?;
    if nested.is_empty() || nested.len() > MAX_NESTED_COURSE_BYTES {
        return Err(invalid_response(
            "UAI nested Course tree is empty or exceeds the size limit",
        ));
    }
    let nested: Value = serde_json::from_str(nested)
        .map_err(|_| invalid_response("UAI nested Course tree is not valid JSON"))?;
    let root = nested
        .as_object()
        .ok_or_else(|| protocol_drift("UAI nested Course tree is not an object"))?;
    let units = root
        .get("units")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI nested Course tree has no units array"))?;

    let mut tasks = BTreeMap::new();
    let mut node_count = 0_usize;
    for unit in units {
        visit_node(
            course,
            &resource_id,
            unit,
            &Hierarchy::default(),
            1,
            &mut node_count,
            &mut tasks,
        )?;
    }
    Ok(tasks.into_values().collect())
}

fn visit_node(
    course: &RemoteCourse,
    resource_id: &str,
    value: &Value,
    hierarchy: &Hierarchy,
    depth: usize,
    node_count: &mut usize,
    tasks: &mut BTreeMap<String, RemoteTask>,
) -> ProviderResult<()> {
    if depth > MAX_DEPTH {
        return Err(invalid_response("UAI Task tree exceeds the depth limit"));
    }
    *node_count = node_count
        .checked_add(1)
        .ok_or_else(|| invalid_response("UAI Task tree node count overflowed"))?;
    if *node_count > MAX_NODES {
        return Err(invalid_response("UAI Task tree exceeds the node limit"));
    }
    let object = value
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Task tree contains a non-object node"))?;
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .ok_or_else(|| protocol_drift("UAI Task tree node has no role"))?;
    if !matches!(
        role.as_str(),
        "unit" | "section" | "node" | "link" | "group"
    ) {
        return Err(protocol_drift(
            "UAI Task tree contains an unknown node role",
        ));
    }
    let id = required_remote_component(object.get("id"), "Task-tree node ID")?;
    let title = node_title(object)?;
    let label = NodeLabel {
        id: id.clone(),
        title: title.clone(),
    };
    let mut next = hierarchy.clone();
    match role.as_str() {
        "unit" => {
            next.unit = Some(label);
            next.section = None;
            next.micro = None;
        }
        "section" => {
            next.section = Some(label);
            next.micro = None;
        }
        "node" => next.micro = Some(label),
        "group" => {
            let task = build_task(course, resource_id, object, hierarchy, &id, title)?;
            if tasks.insert(task.remote_id.clone(), task).is_some() {
                return Err(protocol_drift(
                    "UAI Task tree contains a duplicate Group identity",
                ));
            }
        }
        "link" => {}
        _ => unreachable!("validated UAI node role"),
    }
    if let Some(children) = object.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| protocol_drift("UAI Task-tree children field is not an array"))?;
        for child in children {
            visit_node(
                course,
                resource_id,
                child,
                &next,
                depth + 1,
                node_count,
                tasks,
            )?;
        }
    }
    Ok(())
}

fn build_task(
    course: &RemoteCourse,
    resource_id: &str,
    object: &Map<String, Value>,
    hierarchy: &Hierarchy,
    group_id: &str,
    title: String,
) -> ProviderResult<RemoteTask> {
    let unit = hierarchy
        .unit
        .as_ref()
        .ok_or_else(|| protocol_drift("UAI Group Task is not nested under a Unit"))?;
    let task_types = task_types(object.get("base"))?;
    let question_count = question_count(object.get("question_num"))?;
    let mut capabilities = vec![TaskCapability::ProgressRead, TaskCapability::DurationRead];
    if supports_question_read(&task_types, question_count) {
        capabilities.extend([
            TaskCapability::QuestionInventory,
            TaskCapability::QuestionParse,
            TaskCapability::AnswerResolve,
        ]);
    }
    let remote_id = format!("group:{resource_id}:{}:{group_id}", unit.id);
    let normalized = serde_json::json!({
        "schema": "uai.group-task.v1",
        "course_resource_id": resource_id,
        "unit": {"id": unit.id, "title": unit.title},
        "section": hierarchy.section.as_ref().map(|value| serde_json::json!({"id": value.id, "title": value.title})),
        "micro": hierarchy.micro.as_ref().map(|value| serde_json::json!({"id": value.id, "title": value.title})),
        "group_id": group_id,
        "task_types": task_types,
        "question_count": question_count,
    });
    Ok(RemoteTask {
        remote_id,
        course_remote_id: Some(course.remote_id.clone()),
        title,
        source_type: SourceType::Resource,
        assessment_class: AssessmentClass::Routine,
        remote_state: RemoteState::Unknown,
        opens_at: None,
        due_at: None,
        closes_at: None,
        capabilities,
        fingerprint: fingerprint(&normalized)?,
        normalized,
        raw_sanitized: serde_json::json!({
            "schema": "uai.group-task.raw.v1",
            "role": "group",
            "task_types": task_types,
            "question_count": question_count,
        }),
    })
}

fn node_title(object: &Map<String, Value>) -> ProviderResult<String> {
    object
        .get("name")
        .filter(|value| value.as_str().is_some_and(|value| !value.trim().is_empty()))
        .or_else(|| object.get("caption"))
        .map_or_else(
            || Err(protocol_drift("UAI Task-tree node has no title")),
            |value| required_text(Some(value), "Task-tree node title"),
        )
}

fn task_types(value: Option<&Value>) -> ProviderResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let value = value
        .as_str()
        .ok_or_else(|| protocol_drift("UAI Group Task base field is not text"))?;
    let mut result = Vec::new();
    for item in value.split(',') {
        let item = item.trim().to_ascii_lowercase();
        if item.is_empty()
            || item.len() > MAX_TASK_TYPE_BYTES
            || !item
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(protocol_drift(
                "UAI Group Task contains an invalid base type",
            ));
        }
        if !result.contains(&item) {
            result.push(item);
        }
        if result.len() > MAX_TASK_TYPES {
            return Err(invalid_response(
                "UAI Group Task exceeds the base-type limit",
            ));
        }
    }
    Ok(result)
}

fn question_count(value: Option<&Value>) -> ProviderResult<Option<u32>> {
    value
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value <= 100_000)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| protocol_drift("UAI Group Task contains an invalid question count"))
        })
        .transpose()
}

fn fingerprint(normalized: &Value) -> Result<String, ProviderError> {
    let bytes = serde_json::to_vec(normalized)
        .map_err(|_| invalid_response("UAI normalized Task cannot be encoded"))?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_course_context, parse_course_inventory};

    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str = include_str!("../../../fixtures/providers/uai/tasks/tree-mixed.json");

    #[test]
    fn parser_keeps_unit_section_micro_and_group_identity_separate() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tasks = parse_task_inventory(&course, &context, TREE).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].remote_id, "group:2001:unit-1:group-1");
        assert_eq!(tasks[0].title, "Read the passage");
        assert_eq!(tasks[0].remote_state, RemoteState::Unknown);
        assert_eq!(
            tasks[0].capabilities,
            vec![TaskCapability::ProgressRead, TaskCapability::DurationRead]
        );
        assert_eq!(tasks[0].normalized["unit"]["id"], "unit-1");
        assert_eq!(tasks[0].normalized["section"]["id"], "section-1");
        assert_eq!(tasks[0].normalized["micro"]["id"], "micro-1");
        assert_eq!(tasks[0].normalized["task_types"][0], "rich-text-read");
        assert_eq!(tasks[0].normalized["question_count"], 1);
        assert_eq!(tasks[1].normalized["question_count"], 2);
        assert!(tasks[0].fingerprint.starts_with("v1:"));
        let encoded = serde_json::to_string(&tasks).unwrap();
        assert!(!encoded.contains("must-be-dropped"));
        assert!(!encoded.contains(context.course_instance_id()));
    }

    #[test]
    fn malformed_duplicate_or_unbound_trees_fail_closed() {
        let courses = parse_course_inventory(COURSES).unwrap();
        let context = parse_course_context(&courses[0], DETAIL).unwrap();
        assert!(
            parse_task_inventory(
                &courses[1],
                &context,
                r#"{"code":0,"course":"{\"units\":[]}"}"#,
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(
                &courses[0],
                &context,
                r#"{"code":0,"course":"{\"units\":[{\"id\":\"unit-1\",\"role\":\"unit\",\"name\":\"Unit\",\"children\":[{\"id\":\"same\",\"role\":\"group\",\"name\":\"A\"},{\"id\":\"same\",\"role\":\"group\",\"name\":\"B\"}]}]}"}"#,
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(
                &courses[0],
                &context,
                r#"{"code":0,"course":"{\"units\":[{\"id\":\"unit-1\",\"role\":\"new-role\",\"name\":\"Unit\"}]}"}"#,
            )
            .is_err()
        );
        assert!(
            parse_task_inventory(
                &courses[0],
                &context,
                r#"{"code":0,"course":"{\"units\":[{\"id\":\"unit-1\",\"role\":\"unit\",\"name\":\"Unit\",\"children\":[{\"id\":\"group-1\",\"role\":\"group\",\"name\":\"Task\",\"question_num\":-1}]}]}"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn only_supported_question_groups_advertise_question_and_answer_slots() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tree = TREE.replace("rich-text-read", "single-choice");
        let tasks = parse_task_inventory(&course, &context, &tree).unwrap();
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::QuestionInventory)
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::QuestionParse)
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::AnswerResolve)
        );
        assert!(
            !tasks[1]
                .capabilities
                .contains(&TaskCapability::QuestionInventory)
        );
        assert!(
            !tasks[1]
                .capabilities
                .contains(&TaskCapability::QuestionParse)
        );
        assert!(
            !tasks[1]
                .capabilities
                .contains(&TaskCapability::AnswerResolve)
        );
    }
}
