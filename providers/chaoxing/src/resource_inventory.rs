use std::collections::HashSet;

use asterism_domain::{AssessmentClass, RemoteState, SourceType};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTask};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::ChaoxingCourseScope;

const MAX_CARD_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ATTACHMENTS_PER_CARD: usize = 256;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 512;
const MAX_CARD_INDEX: u8 = 6;

/// Parses one donor-observed `knowledge/cards` response without executing its
/// script. Routing and submission fields stay outside the normalized tasks.
///
/// # Errors
///
/// Returns a protocol error for malformed `mArg`, unbounded attachment lists,
/// unstable identities, duplicate tasks, or unknown attachments marked as
/// actual jobs.
pub fn parse_chapter_resource_inventory(
    html: &str,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
) -> ProviderResult<Vec<RemoteTask>> {
    if html.is_empty() || html.len() > MAX_CARD_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter card document is empty or exceeds the size limit",
        ));
    }
    validate_component(knowledge_id, "knowledge identity")?;
    if card_index > MAX_CARD_INDEX {
        return Err(protocol_drift(
            "Chaoxing chapter card index exceeds the donor range",
        ));
    }
    if html.contains("章节未开放") {
        return Ok(Vec::new());
    }
    let Some(object) = extract_marg_object(html)? else {
        return Ok(Vec::new());
    };
    let cards_data = serde_json::from_str::<Value>(object)
        .map_err(|_| protocol_drift("Chaoxing chapter card contains malformed mArg JSON"))?;
    let root = cards_data
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing chapter card mArg is not an object"))?;
    let attachments = match root.get("attachments") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(attachments)) if attachments.len() <= MAX_ATTACHMENTS_PER_CARD => {
            attachments
        }
        Some(Value::Array(_)) => {
            return Err(invalid_response(
                "Chaoxing chapter attachment count exceeds the size limit",
            ));
        }
        Some(_) => {
            return Err(protocol_drift(
                "Chaoxing chapter attachments are not an array",
            ));
        }
    };

    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    for attachment in attachments {
        let Some(task) = parse_resource_attachment(attachment, scope, knowledge_id, card_index)?
        else {
            continue;
        };
        if !seen.insert(task.remote_id.clone()) {
            return Err(protocol_drift(
                "Chaoxing chapter card contains a duplicate task identity",
            ));
        }
        tasks.push(task);
    }
    Ok(tasks)
}

fn parse_resource_attachment(
    attachment: &Value,
    scope: &ChaoxingCourseScope,
    knowledge_id: &str,
    card_index: u8,
) -> ProviderResult<Option<RemoteTask>> {
    let card = attachment
        .as_object()
        .ok_or_else(|| protocol_drift("Chaoxing chapter attachment is not an object"))?;
    let property = optional_object(card, "property")?;
    let passed = optional_bool(card, "isPassed")?.unwrap_or(false);
    let job = optional_bool(card, "job")?.unwrap_or(false);
    let card_type = optional_string(card, "type")?
        .unwrap_or_default()
        .to_ascii_lowercase();
    let read_pending =
        card_type == "read" && optional_bool(property, "read")?.is_some_and(|read| !read);
    if !passed && !job && !read_pending {
        return Ok(None);
    }
    let kind = classify_resource_kind(&card_type, property).ok_or_else(|| {
        protocol_drift("Chaoxing chapter contains an unknown job attachment type")
    })?;
    let task_id = resource_identity(card, property)?;
    validate_component(&task_id, "resource identity")?;
    let remote_id = format!(
        "resource:{}:{}:{knowledge_id}:{task_id}",
        scope.course_id(),
        scope.class_id()
    );
    let title = resource_title(kind, &task_id, property)?;
    let remote_state = if passed {
        RemoteState::Completed
    } else {
        RemoteState::Pending
    };
    let module = if kind == "chapter_work" {
        "chapter"
    } else {
        "resource"
    };
    let normalized = json!({
        "schema": "chaoxing.chapter-resource.v1",
        "module": module,
        "resource_kind": kind,
        "course_id": scope.course_id(),
        "class_id": scope.class_id(),
        "knowledge_id": knowledge_id,
        "card_index": card_index,
        "task_id": task_id,
        "title": title,
        "remote_state": remote_state,
    });
    Ok(Some(RemoteTask {
        remote_id,
        course_remote_id: Some(scope.remote_course_id().to_owned()),
        title,
        source_type: if kind == "chapter_work" {
            SourceType::Chapter
        } else {
            SourceType::Resource
        },
        assessment_class: AssessmentClass::Unknown,
        remote_state,
        opens_at: None,
        due_at: None,
        closes_at: None,
        capabilities: Vec::new(),
        fingerprint: fingerprint(&normalized)?,
        normalized,
        raw_sanitized: json!({
            "resource_kind": kind,
            "card_index": card_index,
            "passed": passed,
        }),
    }))
}

fn extract_marg_object(html: &str) -> ProviderResult<Option<&str>> {
    let bytes = html.as_bytes();
    let mut search_from = 0;
    let mut found = None;
    while let Some(offset) = html[search_from..].find("mArg") {
        let token_start = search_from + offset;
        let mut cursor = token_start + "mArg".len();
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'=') {
            search_from = cursor;
            continue;
        }
        cursor += 1;
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'{') {
            return Err(protocol_drift(
                "Chaoxing chapter card mArg does not start with an object",
            ));
        }
        let end = matching_json_object_end(bytes, cursor)?;
        if found.replace(&html[cursor..end]).is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter card contains multiple mArg objects",
            ));
        }
        search_from = end;
    }
    Ok(found)
}

fn matching_json_object_end(bytes: &[u8], start: usize) -> ProviderResult<usize> {
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    protocol_drift("Chaoxing chapter card mArg has unbalanced braces")
                })?;
                if depth == 0 {
                    return Ok(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    Err(protocol_drift(
        "Chaoxing chapter card mArg object is incomplete",
    ))
}

fn skip_ascii_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn classify_resource_kind(card_type: &str, property: &Map<String, Value>) -> Option<&'static str> {
    let property_type = optional_unchecked_string(property, "type").to_ascii_lowercase();
    let resource_type = optional_unchecked_string(property, "resourceType").to_ascii_lowercase();
    let live = card_type.contains("live")
        || property_type.contains("live")
        || resource_type.contains("live")
        || property.contains_key("liveId")
        || property.contains_key("streamName")
        || property.contains_key("vdoid");
    if live {
        Some("live")
    } else {
        match card_type {
            "video" => Some("video"),
            "document" => Some("document"),
            "workid" => Some("chapter_work"),
            "read" => Some("read"),
            _ => None,
        }
    }
}

fn resource_identity(
    card: &Map<String, Value>,
    property: &Map<String, Value>,
) -> ProviderResult<String> {
    for (object, key) in [
        (card, "jobid"),
        (card, "id"),
        (property, "id"),
        (card, "objectId"),
        (card, "mid"),
    ] {
        if let Some(value) = object.get(key) {
            return scalar_string(value)
                .ok_or_else(|| protocol_drift("Chaoxing chapter resource identity is not scalar"));
        }
    }
    Err(protocol_drift(
        "Chaoxing chapter resource has no stable identity",
    ))
}

fn resource_title(
    kind: &str,
    task_id: &str,
    property: &Map<String, Value>,
) -> ProviderResult<String> {
    let supplied = ["name", "title"]
        .into_iter()
        .map(|key| optional_unchecked_string(property, key))
        .find(|value| !value.trim().is_empty());
    let title = supplied.map_or_else(
        || format!("{} {task_id}", resource_label(kind)),
        |value| value.split_whitespace().collect::<Vec<_>>().join(" "),
    );
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.chars().any(char::is_control) {
        return Err(protocol_drift(
            "Chaoxing chapter resource has an invalid title",
        ));
    }
    Ok(title)
}

fn resource_label(kind: &str) -> &'static str {
    match kind {
        "video" => "视频任务",
        "document" => "文档任务",
        "chapter_work" => "章节测验",
        "read" => "阅读任务",
        "live" => "直播任务",
        _ => "资源任务",
    }
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> ProviderResult<&'a Map<String, Value>> {
    static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
    match object.get(key) {
        None | Some(Value::Null) => Ok(&EMPTY),
        Some(Value::Object(value)) => Ok(value),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachment property is not an object",
        )),
    }
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> ProviderResult<Option<bool>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachment boolean field is invalid",
        )),
    }
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> ProviderResult<Option<&'a str>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(protocol_drift(
            "Chaoxing chapter attachment string field is invalid",
        )),
    }
}

fn optional_unchecked_string<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn validate_component(value: &str, label: &'static str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            format!("Chaoxing chapter {label} is invalid"),
        ));
    }
    Ok(())
}

fn fingerprint(normalized: &Value) -> ProviderResult<String> {
    let bytes = serde_json::to_vec(normalized).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "failed to serialize sanitized Chaoxing resource inventory",
        )
    })?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");
    const CARDS_EXPECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.expected.json");

    #[test]
    fn card_parser_keeps_resource_kinds_states_and_chapter_work_separate() {
        let tasks = parse_chapter_resource_inventory(CARDS_MIXED, &scope(), "4001", 0).unwrap();
        let selected = Value::Array(
            tasks
                .iter()
                .map(|task| {
                    json!({
                        "remote_id": task.remote_id,
                        "title": task.title,
                        "source_type": task.source_type,
                        "remote_state": task.remote_state,
                        "resource_kind": task.normalized["resource_kind"],
                    })
                })
                .collect(),
        );
        assert_eq!(
            selected,
            serde_json::from_str::<Value>(CARDS_EXPECTED).unwrap()
        );
        let serialized = serde_json::to_string(&tasks).unwrap();
        for private in [
            "PRIVATE_ENC",
            "PRIVATE_TOKEN",
            "PRIVATE_MID",
            "PRIVATE_OTHER",
            "PRIVATE_LIVE",
        ] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn marg_extractor_handles_braces_inside_json_strings() {
        let tasks = parse_chapter_resource_inventory(
            &CARDS_MIXED.replace("视频导论", "视频 {导论}"),
            &scope(),
            "4001",
            0,
        )
        .unwrap();
        assert_eq!(tasks[0].title, "视频 {导论}");
    }

    #[test]
    fn card_parser_treats_an_empty_card_slot_as_no_tasks() {
        let tasks = parse_chapter_resource_inventory(
            "<html><body>当前卡槽没有资源</body></html>",
            &scope(),
            "4001",
            6,
        )
        .unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn card_parser_rejects_unknown_jobs_duplicates_and_malformed_marg() {
        assert!(
            parse_chapter_resource_inventory(
                &CARDS_MIXED.replace("\"type\":\"video\"", "\"type\":\"unknown\""),
                &scope(),
                "4001",
                0,
            )
            .is_err()
        );
        assert!(
            parse_chapter_resource_inventory(
                &CARDS_MIXED.replace("job-document", "job-video"),
                &scope(),
                "4001",
                0,
            )
            .is_err()
        );
        assert!(
            parse_chapter_resource_inventory(
                "<script>mArg={broken};</script>",
                &scope(),
                "4001",
                0
            )
            .is_err()
        );
    }

    fn scope() -> ChaoxingCourseScope {
        ChaoxingCourseScope::new("course:100:200", "100", "200").unwrap()
    }
}
