use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
use asterism_provider_api::{ProviderError, ProviderResult, RemoteCourse, RemoteTask};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::course_inventory::{
    UaiCourseContext, course_resource_id_from_remote, invalid_response, protocol_drift,
    required_remote_component, required_text,
};
use crate::{
    UaiCourseProgressDocument, UaiMicroProgressSnapshot, UaiProgressDocument,
    parse_course_progress, parse_group_progress, parse_micro_progress,
    question::supports_question_read, resource_execution::supports_empty_completion_execution,
};

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
    let tree = parse_task_tree(document)?;
    task_tree_unit_ids(&tree.units)?;

    let mut tasks = BTreeMap::new();
    let mut node_count = 0_usize;
    let binding = CourseTreeBinding {
        course,
        resource_id: &resource_id,
        course_publish_version: tree.course_publish_version,
    };
    for unit in &tree.units {
        visit_node(
            binding,
            unit,
            &Hierarchy::default(),
            1,
            &mut node_count,
            &mut tasks,
        )?;
    }
    Ok(tasks.into_values().collect())
}

pub(crate) fn parse_task_tree_unit_ids(document: &str) -> ProviderResult<BTreeSet<String>> {
    let tree = parse_task_tree(document)?;
    task_tree_unit_ids(&tree.units)
}

pub(crate) fn enrich_task_inventory(
    mut tasks: Vec<RemoteTask>,
    tree_units: &BTreeSet<String>,
    course_progress: Option<&UaiCourseProgressDocument>,
    progress_by_unit: &BTreeMap<String, UaiProgressDocument>,
) -> ProviderResult<Vec<RemoteTask>> {
    let course_progress = course_progress
        .map(|document| parse_course_progress(document.as_str()))
        .transpose()?;
    if progress_by_unit.is_empty() && course_progress.is_none() {
        return Ok(tasks);
    }
    let task_units = tasks
        .iter()
        .map(task_unit_id)
        .collect::<ProviderResult<BTreeSet<_>>>()?;
    if !task_units.is_subset(tree_units) {
        return Err(protocol_drift(
            "UAI normalized Task Units do not belong to the Task tree",
        ));
    }
    if !progress_by_unit.is_empty()
        && progress_by_unit.keys().collect::<BTreeSet<_>>()
            != tree_units.iter().collect::<BTreeSet<_>>()
    {
        return Err(protocol_drift(
            "UAI Task strategy documents do not match the Task-tree Units",
        ));
    }
    if course_progress.as_ref().is_some_and(|snapshot| {
        snapshot.units().keys().collect::<BTreeSet<_>>()
            != tree_units.iter().collect::<BTreeSet<_>>()
    }) {
        return Err(protocol_drift(
            "UAI Course progress Units do not match the Task-tree Units",
        ));
    }
    let micro_progress_by_unit = progress_by_unit
        .iter()
        .map(|(unit_id, document)| {
            parse_micro_progress(document.as_str()).map(|progress| (unit_id.clone(), progress))
        })
        .collect::<ProviderResult<BTreeMap<_, _>>>()?;
    for task in &mut tasks {
        let unit_id = task_unit_id(task)?;
        if let Some(course_snapshot) = &course_progress {
            let strategy = course_snapshot
                .units()
                .get(&unit_id)
                .ok_or_else(|| protocol_drift("UAI Course progress has no matching Task Unit"))?;
            bind_task_publish_version(task, course_snapshot.publish_version(), "Course progress")?;
            let strategy = serde_json::json!({
                "required": strategy.required(),
                "min_score_percent": strategy.minimum_score_percent(),
                "statistic_mode_out": strategy.statistic_mode_out(),
                "opens_at": strategy.opens_at(),
                "closes_at": strategy.closes_at(),
            });
            task.normalized["course_unit_strategy"] = strategy.clone();
            task.raw_sanitized["course_unit_strategy"] = strategy;
        }
        if !progress_by_unit.is_empty() {
            let group_id = task
                .normalized
                .get("group_id")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_drift("UAI normalized Task has no Group identity"))?;
            let progress = progress_by_unit
                .get(&unit_id)
                .ok_or_else(|| protocol_drift("UAI Task strategy has no matching Unit document"))?;
            let snapshot = parse_group_progress(progress.as_str(), &unit_id, group_id)?;
            if let Some(publish_version) = snapshot.publish_version() {
                bind_task_publish_version(task, publish_version, "Unit progress")?;
            }
            task.remote_state = if snapshot.is_completed() {
                RemoteState::Completed
            } else {
                RemoteState::Unknown
            };
            task.opens_at = snapshot.opens_at();
            task.closes_at = snapshot.closes_at();
            let strategy = serde_json::json!({
                "required": snapshot.required(),
                "min_score_percent": snapshot.min_score_percent(),
                "statistic_mode_out": snapshot.statistic_mode_out(),
                "tab_type": snapshot.tab_type(),
                "opens_at": snapshot.opens_at(),
                "closes_at": snapshot.closes_at(),
            });
            task.normalized["strategy"] = strategy.clone();
            task.raw_sanitized["strategy"] = strategy;
            if let Some(micros) = micro_progress_by_unit.get(&unit_id) {
                attach_micro_progress(task, micros)?;
            }
        }
        task.fingerprint = fingerprint(&task.normalized)?;
    }
    Ok(tasks)
}

fn attach_micro_progress(
    task: &mut RemoteTask,
    micros: &BTreeMap<String, UaiMicroProgressSnapshot>,
) -> ProviderResult<()> {
    let Some(micro_id) = task
        .normalized
        .get("micro")
        .and_then(Value::as_object)
        .and_then(|micro| micro.get("id"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let mut matches = micros
        .iter()
        .filter(|(path, _)| path.rsplit('/').next() == Some(micro_id));
    let Some((path, snapshot)) = matches.next() else {
        return Ok(());
    };
    if matches.next().is_some() {
        return Err(protocol_drift(
            "UAI progress contains ambiguous Micro paths for one Task hierarchy",
        ));
    }
    let progress = serde_json::json!({
        "path": path,
        "pass": snapshot.pass(),
        "pass2": snapshot.pass2(),
        "perm": snapshot.perm(),
        "completed": snapshot.is_completed(),
        "required": snapshot.required(),
        "min_score_percent": snapshot.min_score_percent(),
        "statistic_mode_out": snapshot.statistic_mode_out(),
        "opens_at": snapshot.opens_at(),
        "closes_at": snapshot.closes_at(),
    });
    task.normalized["micro_progress"] = progress.clone();
    task.raw_sanitized["micro_progress"] = progress;
    Ok(())
}

fn bind_task_publish_version(
    task: &mut RemoteTask,
    publish_version: u64,
    source: &'static str,
) -> ProviderResult<()> {
    match task.normalized.get("course_publish_version") {
        None | Some(Value::Null) => {}
        Some(value) if optional_publish_version(Some(value))? == Some(publish_version) => {}
        Some(_) => {
            return Err(protocol_drift(format!(
                "UAI {source} publish version does not match the current Task snapshot"
            )));
        }
    }
    task.normalized["course_publish_version"] = serde_json::json!(publish_version);
    Ok(())
}

struct ParsedTaskTree {
    course_publish_version: Option<u64>,
    units: Vec<Value>,
}

fn parse_task_tree(document: &str) -> ProviderResult<ParsedTaskTree> {
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
    if outer.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(protocol_drift("UAI Task-tree read did not succeed"));
    }
    let course_publish_version = optional_publish_version(outer.get("publish_version"))?;
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
        .ok_or_else(|| protocol_drift("UAI nested Course tree has no units array"))?
        .clone();
    Ok(ParsedTaskTree {
        course_publish_version,
        units,
    })
}

fn task_tree_unit_ids(units: &[Value]) -> ProviderResult<BTreeSet<String>> {
    let mut identities = BTreeSet::new();
    for unit in units {
        let unit = unit
            .as_object()
            .ok_or_else(|| protocol_drift("UAI Task tree contains a non-object Unit"))?;
        if unit.get("role").and_then(Value::as_str) != Some("unit") {
            return Err(protocol_drift(
                "UAI Task tree contains a non-Unit top-level node",
            ));
        }
        let identity = required_remote_component(unit.get("id"), "Task-tree Unit ID")?;
        if !identities.insert(identity) {
            return Err(protocol_drift(
                "UAI Task tree contains a duplicate Unit identity",
            ));
        }
    }
    Ok(identities)
}

fn task_unit_id(task: &RemoteTask) -> ProviderResult<String> {
    task.normalized
        .get("unit")
        .and_then(Value::as_object)
        .and_then(|unit| unit.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| protocol_drift("UAI normalized Task has no Unit identity"))
}

#[derive(Clone, Copy)]
struct CourseTreeBinding<'a> {
    course: &'a RemoteCourse,
    resource_id: &'a str,
    course_publish_version: Option<u64>,
}

fn visit_node(
    binding: CourseTreeBinding<'_>,
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
            let task = build_task(binding, object, hierarchy, &id, title)?;
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
            visit_node(binding, child, &next, depth + 1, node_count, tasks)?;
        }
    }
    Ok(())
}

fn build_task(
    binding: CourseTreeBinding<'_>,
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
    let mut capabilities = vec![
        TaskCapability::ProgressRead,
        TaskCapability::DurationRead,
        TaskCapability::BrowserBridge,
    ];
    if supports_empty_completion_execution(&task_types, question_count) {
        capabilities.extend([
            TaskCapability::ResourceExecution,
            TaskCapability::ExecutionVerify,
        ]);
    }
    if supports_question_read(&task_types, question_count) {
        capabilities.extend([
            TaskCapability::QuestionInventory,
            TaskCapability::QuestionParse,
            TaskCapability::AnswerResolve,
            TaskCapability::SubmissionBuild,
        ]);
    }
    if supports_submission_execution(&task_types, question_count) {
        capabilities.extend([
            TaskCapability::SubmissionExecute,
            TaskCapability::SubmissionVerify,
        ]);
    }
    let remote_id = format!("group:{}:{}:{group_id}", binding.resource_id, unit.id);
    let normalized = serde_json::json!({
        "schema": "uai.group-task.v1",
        "course_resource_id": binding.resource_id,
        "unit": {"id": unit.id, "title": unit.title},
        "section": hierarchy.section.as_ref().map(|value| serde_json::json!({"id": value.id, "title": value.title})),
        "micro": hierarchy.micro.as_ref().map(|value| serde_json::json!({"id": value.id, "title": value.title})),
        "group_id": group_id,
        "course_publish_version": binding.course_publish_version,
        "task_types": task_types,
        "question_count": question_count,
    });
    Ok(RemoteTask {
        remote_id,
        course_remote_id: Some(binding.course.remote_id.clone()),
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
        let item = canonical_task_type(item.trim());
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
        result.push(item);
        if result.len() > MAX_TASK_TYPES {
            return Err(invalid_response(
                "UAI Group Task exceeds the base-type limit",
            ));
        }
    }
    Ok(result)
}

fn canonical_task_type(value: &str) -> String {
    let normalized = value.to_ascii_lowercase();
    if normalized == "multifileupload" {
        "multiFileUpload".to_owned()
    } else {
        normalized
    }
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

fn optional_publish_version(value: Option<&Value>) -> ProviderResult<Option<u64>> {
    const MAX_SIGNED_64_BIT_VALUE: u64 = 9_223_372_036_854_775_807;

    let Some(value) = value else {
        return Ok(None);
    };
    let version = match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
    .filter(|value| (1..=MAX_SIGNED_64_BIT_VALUE).contains(value))
    .ok_or_else(|| protocol_drift("UAI Task tree has an invalid Course publish version"))?;
    Ok(Some(version))
}

fn supports_submission_execution(task_types: &[String], question_count: Option<u32>) -> bool {
    supports_question_read(task_types, question_count)
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
    const COURSE_PROGRESS: &str =
        include_str!("../../../fixtures/providers/uai/progress/course-mixed.json");
    const UNIT_PROGRESS: &str =
        include_str!("../../../fixtures/providers/uai/progress/unit-mixed.json");

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
            vec![
                TaskCapability::ProgressRead,
                TaskCapability::DurationRead,
                TaskCapability::BrowserBridge,
                TaskCapability::ResourceExecution,
                TaskCapability::ExecutionVerify,
            ]
        );
        assert_eq!(tasks[0].normalized["unit"]["id"], "unit-1");
        assert_eq!(tasks[0].normalized["section"]["id"], "section-1");
        assert_eq!(tasks[0].normalized["micro"]["id"], "micro-1");
        assert_eq!(tasks[0].normalized["course_publish_version"], 123_290);
        assert_eq!(tasks[0].normalized["task_types"][0], "rich-text-read");
        assert_eq!(tasks[0].normalized["question_count"], 1);
        assert_eq!(tasks[1].normalized["question_count"], 2);
        assert!(tasks[0].fingerprint.starts_with("v1:"));
        let encoded = serde_json::to_string(&tasks).unwrap();
        assert!(!encoded.contains("must-be-dropped"));
        assert!(!encoded.contains(context.course_instance_id()));
    }

    #[test]
    fn parser_accepts_the_donor_observed_string_publish_version_shape() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tree = TREE.replace(
            r#""publish_version": 123290"#,
            r#""publish_version": "123290""#,
        );
        let tasks = parse_task_inventory(&course, &context, &tree).unwrap();
        assert_eq!(tasks[0].normalized["course_publish_version"], 123_290);
    }

    #[test]
    fn parser_preserves_the_donor_upload_type_for_native_gates() {
        assert_eq!(canonical_task_type("MULTIFILEUPLOAD"), "multiFileUpload");
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let single = TREE.replace("rich-text-read", "multiFileUpload");
        let tasks = parse_task_inventory(&course, &context, &single).unwrap();
        assert_eq!(
            tasks[0].normalized["task_types"],
            serde_json::json!(["multiFileUpload"])
        );

        let compound = TREE.replace(
            r#"\"base\":\"rich-text-read\",\"question_num\":1"#,
            r#"\"base\":\"multichoice,multiFileUpload\",\"question_num\":2"#,
        );
        let tasks = parse_task_inventory(&course, &context, &compound).unwrap();
        assert_eq!(
            tasks[0].normalized["task_types"],
            serde_json::json!(["multichoice", "multiFileUpload"])
        );
    }

    #[test]
    fn course_progress_independently_fills_and_binds_publish_version() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tree_without_version = TREE.replace("\n  \"publish_version\": 123290,", "");
        let tasks = parse_task_inventory(&course, &context, &tree_without_version).unwrap();
        assert!(tasks[0].normalized["course_publish_version"].is_null());
        let tree_units = parse_task_tree_unit_ids(&tree_without_version).unwrap();
        let course_progress = UaiCourseProgressDocument::try_new(COURSE_PROGRESS).unwrap();
        let progress = BTreeMap::from([(
            "unit-1".to_owned(),
            UaiProgressDocument::try_new(UNIT_PROGRESS).unwrap(),
        )]);
        let tasks =
            enrich_task_inventory(tasks, &tree_units, Some(&course_progress), &progress).unwrap();
        assert_eq!(tasks[0].normalized["course_publish_version"], 123_290);
        assert_eq!(
            tasks[0].normalized["course_unit_strategy"]["required"],
            true
        );
        assert_eq!(
            tasks[0].normalized["course_unit_strategy"]["opens_at"],
            "2026-08-01T00:00:00Z"
        );
        assert_eq!(
            tasks[0].normalized["micro_progress"]["path"],
            "section-1/micro-1"
        );
        assert_eq!(tasks[0].normalized["micro_progress"]["completed"], false);
    }

    #[test]
    fn course_progress_unit_or_version_drift_fails_closed() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tree_units = parse_task_tree_unit_ids(TREE).unwrap();
        let progress = BTreeMap::from([(
            "unit-1".to_owned(),
            UaiProgressDocument::try_new(UNIT_PROGRESS).unwrap(),
        )]);

        let version = UaiCourseProgressDocument::try_new(
            COURSE_PROGRESS.replace(r#""123290""#, r#""123291""#),
        )
        .unwrap();
        assert!(
            enrich_task_inventory(
                parse_task_inventory(&course, &context, TREE).unwrap(),
                &tree_units,
                Some(&version),
                &progress,
            )
            .is_err()
        );

        let units =
            UaiCourseProgressDocument::try_new(COURSE_PROGRESS.replace("unit-1", "unit-other"))
                .unwrap();
        assert!(
            enrich_task_inventory(
                parse_task_inventory(&course, &context, TREE).unwrap(),
                &tree_units,
                Some(&units),
                &progress,
            )
            .is_err()
        );

        let unit_version = BTreeMap::from([(
            "unit-1".to_owned(),
            UaiProgressDocument::try_new(UNIT_PROGRESS.replace(
                r#""publish_version": "123290""#,
                r#""publish_version": "123291""#,
            ))
            .unwrap(),
        )]);
        let current = UaiCourseProgressDocument::try_new(COURSE_PROGRESS).unwrap();
        assert!(
            enrich_task_inventory(
                parse_task_inventory(&course, &context, TREE).unwrap(),
                &tree_units,
                Some(&current),
                &unit_version,
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_duplicate_or_unbound_trees_fail_closed() {
        let courses = parse_course_inventory(COURSES).unwrap();
        let context = parse_course_context(&courses[0], DETAIL).unwrap();
        assert!(
            parse_task_inventory(&courses[0], &context, r#"{"course":"{\"units\":[]}"}"#).is_err()
        );
        assert!(
            parse_task_inventory(
                &courses[1],
                &context,
                r#"{"code":0,"course":"{\"units\":[]}"}"#,
            )
            .is_err()
        );
        assert!(parse_task_inventory(&courses[0], &context, &TREE.replace("123290", "0")).is_err());
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
        assert_question_capabilities(&tasks[0], true);

        let fillblank = TREE.replace("rich-text-read", "material-banked-cloze");
        let fillblank_tasks = parse_task_inventory(&course, &context, &fillblank).unwrap();
        for capability in [
            TaskCapability::QuestionInventory,
            TaskCapability::QuestionParse,
            TaskCapability::AnswerResolve,
            TaskCapability::SubmissionBuild,
            TaskCapability::SubmissionExecute,
            TaskCapability::SubmissionVerify,
        ] {
            assert!(fillblank_tasks[0].capabilities.contains(&capability));
        }
        let writing = TREE.replace("rich-text-read", "writing");
        let writing_tasks = parse_task_inventory(&course, &context, &writing).unwrap();
        assert_question_capabilities(&writing_tasks[0], true);
        let video_popup = TREE.replace("rich-text-read", "video-popup");
        let video_popup_tasks = parse_task_inventory(&course, &context, &video_popup).unwrap();
        assert_question_capabilities(&video_popup_tasks[0], true);
        assert_question_capabilities(&tasks[1], false);

        let multiple = TREE.replace(
            r#"\"base\":\"rich-text-read\",\"question_num\":1"#,
            r#"\"base\":\"multichoice,short_answer\",\"question_num\":2"#,
        );
        let tasks = parse_task_inventory(&course, &context, &multiple).unwrap();
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::SubmissionExecute)
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::SubmissionVerify)
        );

        let mismatched = multiple.replace("question_num\\\":2", "question_num\\\":3");
        let tasks = parse_task_inventory(&course, &context, &mismatched).unwrap();
        assert!(
            !tasks[0]
                .capabilities
                .contains(&TaskCapability::SubmissionExecute)
        );

        let repeated = TREE.replace(
            r#"\"base\":\"rich-text-read\",\"question_num\":1"#,
            r#"\"base\":\"multichoice,short_answer,multichoice\",\"question_num\":3"#,
        );
        let tasks = parse_task_inventory(&course, &context, &repeated).unwrap();
        assert_eq!(
            tasks[0].normalized["task_types"].as_array().unwrap().len(),
            3
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::SubmissionExecute)
        );
    }

    fn assert_question_capabilities(task: &RemoteTask, expected: bool) {
        for capability in [
            TaskCapability::QuestionInventory,
            TaskCapability::QuestionParse,
            TaskCapability::AnswerResolve,
            TaskCapability::SubmissionBuild,
            TaskCapability::SubmissionExecute,
            TaskCapability::SubmissionVerify,
        ] {
            assert_eq!(task.capabilities.contains(&capability), expected);
        }
    }

    #[test]
    fn only_audited_empty_completion_groups_advertise_verified_resource_execution() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let context = parse_course_context(&course, DETAIL).unwrap();
        let tasks = parse_task_inventory(&course, &context, TREE).unwrap();
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::ResourceExecution)
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::ExecutionVerify)
        );
        assert!(
            tasks[1]
                .capabilities
                .contains(&TaskCapability::ResourceExecution)
        );
        assert!(
            tasks[1]
                .capabilities
                .contains(&TaskCapability::ExecutionVerify)
        );

        let unsupported = TREE
            .replace("rich-text-read", "discussion")
            .replace("vocabulary,input", "single-choice");
        let tasks = parse_task_inventory(&course, &context, &unsupported).unwrap();
        assert!(
            !tasks[0]
                .capabilities
                .contains(&TaskCapability::ResourceExecution)
        );
        assert!(
            !tasks[1]
                .capabilities
                .contains(&TaskCapability::ResourceExecution)
        );

        let exit_ticket = TREE
            .replace("rich-text-read", "exit-ticket")
            .replace("vocabulary,input", "single-choice");
        let tasks = parse_task_inventory(&course, &context, &exit_ticket).unwrap();
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::ResourceExecution)
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::ExecutionVerify)
        );

        let oral = TREE
            .replace("rich-text-read", "oral-sentence")
            .replace("vocabulary,input", "single-choice");
        let tasks = parse_task_inventory(&course, &context, &oral).unwrap();
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::ResourceExecution)
        );
        assert!(
            tasks[0]
                .capabilities
                .contains(&TaskCapability::ExecutionVerify)
        );
    }
}
