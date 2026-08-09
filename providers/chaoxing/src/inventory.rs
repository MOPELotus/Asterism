use std::collections::HashSet;

use asterism_domain::{AssessmentClass, RemoteState, SourceType};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTask};
use asterism_secrets::SecretString;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 512;
const MAX_EVIDENCE_BYTES: usize = 1_024;
const MAX_INVENTORY_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_INVENTORY_TASKS: usize = 2_048;

pub(crate) struct ChaoxingParsedInventoryTask {
    task: RemoteTask,
    entry: SecretString,
}

impl ChaoxingParsedInventoryTask {
    pub(crate) const fn task(&self) -> &RemoteTask {
        &self.task
    }

    pub(crate) const fn task_mut(&mut self) -> &mut RemoteTask {
        &mut self.task
    }

    pub(crate) fn entry(&self) -> &str {
        self.entry.expose_secret()
    }

    pub(crate) fn into_task(self) -> RemoteTask {
        self.task
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingCourseScope {
    course_remote: String,
    course: String,
    clazz: String,
}

impl ChaoxingCourseScope {
    /// Builds the identity scope carried from Course inventory into task scans.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when any identity component is empty, unbounded,
    /// or contains characters that are unsafe for a stable remote identifier.
    pub fn new(
        course_remote_id: impl Into<String>,
        course_id: impl Into<String>,
        class_id: impl Into<String>,
    ) -> ProviderResult<Self> {
        let scope = Self {
            course_remote: course_remote_id.into(),
            course: course_id.into(),
            clazz: class_id.into(),
        };
        for component in [
            scope.course_remote.as_str(),
            scope.course.as_str(),
            scope.clazz.as_str(),
        ] {
            validate_remote_component(component)?;
        }
        Ok(scope)
    }

    pub(crate) fn remote_course_id(&self) -> &str {
        &self.course_remote
    }

    pub(crate) fn course_id(&self) -> &str {
        &self.course
    }

    pub(crate) fn class_id(&self) -> &str {
        &self.clazz
    }
}

/// Parses independent course homework from one sanitized Work list document.
///
/// # Errors
///
/// Returns a protocol-drift error when a task-like row lacks a stable identity
/// or bounded title. Session-bound route parameters are never retained.
pub fn parse_work_inventory(
    html: &str,
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<RemoteTask>> {
    Ok(parse_work_inventory_entries(html, scope)?
        .into_iter()
        .map(ChaoxingParsedInventoryTask::into_task)
        .collect())
}

pub(crate) fn parse_work_inventory_entries(
    html: &str,
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<ChaoxingParsedInventoryTask>> {
    parse_inventory(html, scope, SourceType::Work)
}

/// Parses independent course exams from one sanitized Exam list document.
///
/// # Errors
///
/// Returns a protocol-drift error when a task-like row lacks a stable identity
/// or bounded title. Script-template status words are ignored because only
/// structural task rows are inspected.
pub fn parse_exam_inventory(
    html: &str,
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<RemoteTask>> {
    Ok(parse_inventory(html, scope, SourceType::Exam)?
        .into_iter()
        .map(ChaoxingParsedInventoryTask::into_task)
        .collect())
}

/// Classifies a followed Work detail response instead of trusting `未交` on the
/// list page.
///
/// # Errors
///
/// Returns a protocol-drift error when neither the final route nor visible
/// server result text identifies an editor, completed result, or closed task.
pub fn classify_work_detail(final_url: &str, visible_text: &str) -> ProviderResult<RemoteState> {
    let route = final_url.to_ascii_lowercase();
    let mut visible = strip_inert_markup(visible_text);
    let text = normalize_evidence(&visible);
    visible.zeroize();
    if contains_any(
        &text,
        &[
            "提交成功",
            "等待教师批阅",
            "待批阅",
            "已批阅",
            "我的答案",
            "正确答案",
        ],
    ) || route.contains("/work/prompt")
    {
        return Ok(RemoteState::Completed);
    }
    if contains_any(&text, &["已过期", "已截止"])
        || (route.contains("/work/view") && contains_any(&text, &["未交", "查看详情"]))
    {
        return Ok(RemoteState::Expired);
    }
    if route.contains("/work/dowork") || contains_any(&text, &["提交 作业", "作业作答", "暂时保存"])
    {
        return Ok(RemoteState::Pending);
    }
    Err(protocol_drift(
        "Chaoxing Work detail response has an unknown route or state",
    ))
}

fn parse_inventory(
    html: &str,
    scope: &ChaoxingCourseScope,
    source_type: SourceType,
) -> ProviderResult<Vec<ChaoxingParsedInventoryTask>> {
    if html.len() > MAX_INVENTORY_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing inventory document exceeds the configured size limit",
        ));
    }
    let document = Html::parse_document(html);
    let row_selector = selector(match source_type {
        SourceType::Work => "li[data], li[data-url], .work-list-item, [data-work-id]",
        SourceType::Exam => "li[data], .exam-list-item, [data-exam-id], [onclick*='goTest']",
        _ => unreachable!("inventory parser accepts only Work and Exam"),
    });
    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    for row in document.select(&row_selector) {
        let Some(entry) = entry_value(row) else {
            continue;
        };
        if !entry_matches_source(&entry, source_type) {
            continue;
        }
        let task_id = extract_task_id(row, &entry, source_type)
            .ok_or_else(|| protocol_drift("Chaoxing inventory row has no stable remote task ID"))?;
        validate_remote_component(&task_id)?;
        let remote_id = format!(
            "{}:{}:{}:{}",
            source_label(source_type),
            scope.course,
            scope.clazz,
            task_id
        );
        if !seen.insert(remote_id.clone()) {
            return Err(protocol_drift(
                "Chaoxing inventory contains a duplicate remote task identity",
            ));
        }
        if tasks.len() == MAX_INVENTORY_TASKS {
            return Err(invalid_response(
                "Chaoxing inventory task count exceeds the size limit",
            ));
        }
        let title = extract_title(row, source_type)?;
        let row_text = normalize_evidence(&row.text().collect::<Vec<_>>().join(" "));
        let status_text = extract_status_text(row).unwrap_or_else(|| row_text.clone());
        let time_text = extract_time_text(row);
        let remote_state = classify_list_state(&status_text);
        let entry_kind = classify_entry_kind(&entry, source_type);
        let normalized = json!({
            "schema": "chaoxing.inventory.v1",
            "module": source_label(source_type),
            "course_id": scope.course,
            "class_id": scope.clazz,
            "task_id": task_id,
            "title": title,
            "remote_state": remote_state,
            "status_text": status_text,
            "time_text": time_text,
            "entry_kind": entry_kind,
        });
        let task = RemoteTask {
            remote_id,
            course_remote_id: Some(scope.course_remote.clone()),
            title,
            source_type,
            assessment_class: AssessmentClass::Unknown,
            remote_state,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities: vec![asterism_domain::TaskCapability::ProgressRead],
            fingerprint: fingerprint(&normalized)?,
            normalized,
            raw_sanitized: json!({
                "status_text": status_text,
                "time_text": time_text,
                "entry_kind": entry_kind,
            }),
        };
        tasks.push(ChaoxingParsedInventoryTask {
            task,
            entry: SecretString::new(entry),
        });
    }
    Ok(tasks)
}

pub(crate) fn apply_work_detail_state(
    task: &mut RemoteTask,
    remote_state: RemoteState,
) -> ProviderResult<()> {
    if task.source_type != SourceType::Work {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Work detail state was applied to another module",
        ));
    }
    let normalized = task.normalized.as_object_mut().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Work task has invalid normalized metadata",
        )
    })?;
    normalized.insert("remote_state".to_owned(), json!(remote_state));
    normalized.insert("detail_remote_state".to_owned(), json!(remote_state));
    let raw = task.raw_sanitized.as_object_mut().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Work task has invalid sanitized metadata",
        )
    })?;
    raw.insert("detail_remote_state".to_owned(), json!(remote_state));
    task.remote_state = remote_state;
    task.fingerprint = fingerprint(&task.normalized)?;
    Ok(())
}

fn entry_value(row: ElementRef<'_>) -> Option<String> {
    for attribute in ["data", "data-url", "onclick", "href"] {
        if let Some(value) = row.value().attr(attribute) {
            return Some(value.to_owned());
        }
    }
    let anchor_selector = selector("a[href]");
    row.select(&anchor_selector)
        .find_map(|anchor| anchor.value().attr("href").map(str::to_owned))
}

fn entry_matches_source(entry: &str, source_type: SourceType) -> bool {
    let entry = entry.to_ascii_lowercase();
    match source_type {
        SourceType::Work => entry.contains("work") && !entry.contains("exam"),
        SourceType::Exam => entry.contains("exam") || entry.contains("gotest("),
        _ => false,
    }
}

fn extract_task_id(row: ElementRef<'_>, entry: &str, source_type: SourceType) -> Option<String> {
    let direct_attribute = match source_type {
        SourceType::Work => "data-work-id",
        SourceType::Exam => "data-exam-id",
        _ => return None,
    };
    if let Some(value) = row.value().attr(direct_attribute) {
        return Some(value.to_owned());
    }
    let query_keys: &[&str] = match source_type {
        SourceType::Work => &["workId", "workid", "oldWorkId", "jobid"],
        SourceType::Exam => &["taskrefId", "examId", "examid"],
        _ => &[],
    };
    for key in query_keys {
        if let Some(value) = query_value(entry, key) {
            return Some(value);
        }
    }
    (source_type == SourceType::Exam)
        .then(|| go_test_exam_id(entry))
        .flatten()
}

fn query_value(value: &str, wanted_key: &str) -> Option<String> {
    let query = value.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|field| {
        let (key, value) = field.split_once('=')?;
        key.eq_ignore_ascii_case(wanted_key)
            .then(|| value.trim_matches(['\'', '"']).to_owned())
    })
}

fn go_test_exam_id(value: &str) -> Option<String> {
    let start = value.to_ascii_lowercase().find("gotest(")? + "gotest(".len();
    let arguments = value.get(start..)?.split(')').next()?;
    arguments
        .split(',')
        .nth(1)
        .map(|value| value.trim().trim_matches(['\'', '"']).to_owned())
        .filter(|value| !value.is_empty())
}

fn extract_title(row: ElementRef<'_>, source_type: SourceType) -> ProviderResult<String> {
    let selector = selector(match source_type {
        SourceType::Work => ".work-name, .task-name, [data-title], p, h3, a",
        SourceType::Exam => ".exam-name, .task-name, [data-title], p, h3, a",
        _ => unreachable!("inventory parser accepts only Work and Exam"),
    });
    let title = row
        .select(&selector)
        .find_map(|node| {
            node.value()
                .attr("data-title")
                .map(str::to_owned)
                .or_else(|| {
                    let text = normalize_evidence(&node.text().collect::<Vec<_>>().join(" "));
                    (!text.is_empty()).then_some(text)
                })
        })
        .unwrap_or_default();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.chars().any(char::is_control) {
        return Err(protocol_drift(
            "Chaoxing inventory row has an invalid or missing title",
        ));
    }
    Ok(title)
}

fn extract_status_text(row: ElementRef<'_>) -> Option<String> {
    let status_selector = selector(".exam-status, .work-status, .task-status, .status");
    row.select(&status_selector)
        .find_map(normalized_node_text)
        .or_else(|| {
            let unclassified_status = selector("span:not(.fr)");
            row.select(&unclassified_status)
                .find_map(normalized_node_text)
        })
}

fn normalized_node_text(node: ElementRef<'_>) -> Option<String> {
    let text = normalize_evidence(&node.text().collect::<Vec<_>>().join(" "));
    (!text.is_empty()).then_some(text)
}

fn extract_time_text(row: ElementRef<'_>) -> Option<String> {
    let time_selector = selector(".exam-deadline, .work-deadline, .task-time, .deadline, .fr");
    row.select(&time_selector).find_map(|node| {
        let text = normalize_evidence(&node.text().collect::<Vec<_>>().join(" "));
        (!text.is_empty()).then_some(text)
    })
}

fn classify_list_state(status: &str) -> RemoteState {
    if contains_any(status, &["已完成", "已提交", "待批阅", "已批阅"]) {
        RemoteState::Completed
    } else if contains_any(status, &["已过期", "已截止"]) {
        RemoteState::Expired
    } else if contains_any(status, &["进行中", "作答中"]) {
        RemoteState::InProgress
    } else if contains_any(status, &["待做", "未交"]) {
        RemoteState::Pending
    } else if contains_any(status, &["尚未开放", "未开放", "未开始"]) {
        RemoteState::NotOpen
    } else {
        RemoteState::Unknown
    }
}

fn classify_entry_kind(entry: &str, source_type: SourceType) -> &'static str {
    let entry = entry.to_ascii_lowercase();
    match source_type {
        SourceType::Work if entry.contains("/work/dowork") => "work_editor",
        SourceType::Work if entry.contains("/work/prompt") => "work_prompt",
        SourceType::Work if entry.contains("/work/view") => "work_view",
        SourceType::Work => "work_entry",
        SourceType::Exam if entry.contains("gotest(") => "exam_go_test",
        SourceType::Exam => "exam_entry",
        _ => "unknown",
    }
}

const fn source_label(source_type: SourceType) -> &'static str {
    match source_type {
        SourceType::Work => "work",
        SourceType::Exam => "exam",
        _ => unreachable!(),
    }
}

fn fingerprint(normalized: &Value) -> ProviderResult<String> {
    let bytes = serde_json::to_vec(normalized).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "failed to serialize sanitized Chaoxing inventory",
        )
    })?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

fn validate_remote_component(value: &str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(protocol_drift(
            "Chaoxing inventory contains an invalid remote identity component",
        ));
    }
    Ok(())
}

fn normalize_evidence(value: &str) -> String {
    let mut normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() > MAX_EVIDENCE_BYTES {
        let mut boundary = MAX_EVIDENCE_BYTES;
        while !normalized.is_char_boundary(boundary) {
            boundary -= 1;
        }
        normalized.truncate(boundary);
    }
    normalized
}

fn strip_inert_markup(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some((start, tag)) = ["script", "style"]
        .into_iter()
        .filter_map(|tag| {
            find_ascii_case_insensitive(value, cursor, &format!("<{tag}")).map(|start| (start, tag))
        })
        .min_by_key(|(start, _)| *start)
    {
        output.push_str(&value[cursor..start]);
        let Some(close) = find_ascii_case_insensitive(value, start, &format!("</{tag}")) else {
            return output;
        };
        let Some(end) = value[close..].find('>').map(|offset| close + offset + 1) else {
            return output;
        };
        cursor = end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn find_ascii_case_insensitive(value: &str, from: usize, needle: &str) -> Option<usize> {
    value.as_bytes()[from..]
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
        .map(|offset| from + offset)
}

fn contains_any(value: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| value.contains(candidate))
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static Chaoxing selector must be valid")
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

    const EXAM_EMPTY: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/list-empty-script-keywords.html");
    const EXAM_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/list-mixed.html");
    const EXAM_EXPECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/list-mixed.expected.json");
    const WORK_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.html");
    const WORK_EXPECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.expected.json");

    #[test]
    fn exam_script_keywords_do_not_create_phantom_tasks() {
        assert!(
            parse_exam_inventory(EXAM_EMPTY, &scope())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exam_inventory_keeps_source_state_and_unknown_status_separate() {
        let tasks = parse_exam_inventory(EXAM_MIXED, &scope()).unwrap();
        assert_eq!(selected_facts(&tasks), expected(EXAM_EXPECTED));
        assert!(tasks.iter().all(|task| {
            task.source_type == SourceType::Exam
                && task.assessment_class == AssessmentClass::Unknown
                && task.capabilities == [asterism_domain::TaskCapability::ProgressRead]
                && task.fingerprint.starts_with("v1:")
                && task.fingerprint.len() == 67
        }));
        let not_open = tasks
            .iter()
            .find(|task| task.remote_id.ends_with(":exam-4"))
            .unwrap();
        assert_eq!(not_open.remote_state, RemoteState::NotOpen);
        assert_eq!(not_open.normalized["status_text"], "未开始");
        assert_eq!(not_open.normalized["time_text"], "开放时间待验证");
        assert!(
            tasks
                .iter()
                .any(|task| task.remote_state == RemoteState::Unknown)
        );
    }

    #[test]
    fn work_inventory_is_independent_from_chapter_tasks() {
        let tasks = parse_work_inventory(WORK_MIXED, &scope()).unwrap();
        assert_eq!(selected_facts(&tasks), expected(WORK_EXPECTED));
        assert!(tasks.iter().all(|task| task.source_type == SourceType::Work
            && task.capabilities == [asterism_domain::TaskCapability::ProgressRead]));
    }

    #[test]
    fn work_detail_overrides_ambiguous_unsubmitted_list_state() {
        let closed = include_str!("../../../fixtures/providers/chaoxing/work/detail-closed.html");
        let editor = include_str!("../../../fixtures/providers/chaoxing/work/detail-editor.html");
        let submitted =
            include_str!("../../../fixtures/providers/chaoxing/work/detail-submitted.html");
        assert_eq!(
            classify_work_detail("https://example.invalid/work/view", closed).unwrap(),
            RemoteState::Expired
        );
        assert_eq!(
            classify_work_detail("https://example.invalid/work/dowork", editor).unwrap(),
            RemoteState::Pending
        );
        assert_eq!(
            classify_work_detail("https://example.invalid/work/prompt", submitted).unwrap(),
            RemoteState::Completed
        );
        assert_eq!(
            classify_work_detail(
                "https://example.invalid/work/view",
                "<script>const template = '提交成功';</script><main>未交 已过期 查看详情</main>",
            )
            .unwrap(),
            RemoteState::Expired
        );
    }

    #[test]
    fn unsafe_identity_or_unknown_detail_fails_closed() {
        assert!(ChaoxingCourseScope::new("course", "123", "bad/value").is_err());
        assert!(classify_work_detail("https://example.invalid/changed", "新页面").is_err());
    }

    #[test]
    fn oversized_inventory_fails_before_html_parsing() {
        let oversized = " ".repeat(MAX_INVENTORY_DOCUMENT_BYTES + 1);
        let error = parse_work_inventory(&oversized, &scope()).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    fn scope() -> ChaoxingCourseScope {
        ChaoxingCourseScope::new("course:100:200", "100", "200").unwrap()
    }

    fn selected_facts(tasks: &[RemoteTask]) -> Value {
        Value::Array(
            tasks
                .iter()
                .map(|task| {
                    json!({
                        "remote_id": task.remote_id,
                        "title": task.title,
                        "source_type": task.source_type,
                        "assessment_class": task.assessment_class,
                        "remote_state": task.remote_state,
                    })
                })
                .collect(),
        )
    }

    fn expected(value: &str) -> Value {
        serde_json::from_str(value).unwrap()
    }
}
