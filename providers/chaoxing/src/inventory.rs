use std::collections::HashSet;

use asterism_domain::{AssessmentClass, RemoteState, SourceType, TaskCapability};
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChaoxingExamFacts {
    score_milli_points: Option<u32>,
    retake_available: bool,
}

impl ChaoxingExamFacts {
    pub(crate) fn score(self) -> Option<f64> {
        self.score_milli_points
            .map(|score| f64::from(score) / 1_000.0)
    }

    pub(crate) fn score_milli_points(self) -> Option<u64> {
        self.score_milli_points.map(u64::from)
    }
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

pub(crate) fn parse_exam_inventory_entries(
    html: &str,
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<ChaoxingParsedInventoryTask>> {
    parse_inventory(html, scope, SourceType::Exam)
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
        let exam_facts = parse_exam_list_facts(row, source_type)?;
        let mut normalized = json!({
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
        insert_exam_list_facts(&mut normalized, exam_facts);
        let capabilities = if source_type == SourceType::Exam
            && remote_state == RemoteState::Pending
            && entry.to_ascii_lowercase().contains("gotest(")
        {
            vec![
                TaskCapability::ProgressRead,
                TaskCapability::QuestionInventory,
                TaskCapability::QuestionParse,
                TaskCapability::SubmissionBuild,
                TaskCapability::SubmissionExecute,
                TaskCapability::SubmissionVerify,
            ]
        } else {
            vec![TaskCapability::ProgressRead]
        };
        let raw_sanitized =
            sanitized_inventory_facts(&status_text, time_text.as_deref(), entry_kind, exam_facts);
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
            capabilities,
            fingerprint: fingerprint(&normalized)?,
            normalized,
            raw_sanitized,
        };
        tasks.push(ChaoxingParsedInventoryTask {
            task,
            entry: SecretString::new(entry),
        });
    }
    Ok(tasks)
}

fn parse_exam_list_facts(
    row: ElementRef<'_>,
    source_type: SourceType,
) -> ProviderResult<Option<ChaoxingExamFacts>> {
    if source_type != SourceType::Exam {
        return Ok(None);
    }
    Ok(Some(ChaoxingExamFacts {
        score_milli_points: extract_exam_score_milli(row)?,
        retake_available: exam_retake_available(row),
    }))
}

fn insert_exam_list_facts(value: &mut Value, facts: Option<ChaoxingExamFacts>) {
    let Some(facts) = facts else {
        return;
    };
    let object = value
        .as_object_mut()
        .expect("Chaoxing inventory facts are an object");
    object.insert("score".to_owned(), json!(facts.score()));
    object.insert("retake_available".to_owned(), json!(facts.retake_available));
}

fn sanitized_inventory_facts(
    status_text: &str,
    time_text: Option<&str>,
    entry_kind: &str,
    exam_facts: Option<ChaoxingExamFacts>,
) -> Value {
    let mut value = json!({
        "status_text": status_text,
        "time_text": time_text,
        "entry_kind": entry_kind,
    });
    insert_exam_list_facts(&mut value, exam_facts);
    value
}

/// Parses score and structural retake facts from one bounded Exam result or
/// detail document. Script/style text is excluded from evidence.
pub(crate) fn parse_exam_detail_facts(html: &str) -> ProviderResult<ChaoxingExamFacts> {
    if html.len() > MAX_INVENTORY_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing Exam detail document exceeds the configured size limit",
        ));
    }
    let mut visible = strip_inert_markup(html);
    let result = (|| {
        let document = Html::parse_document(&visible);
        let root = document.root_element();
        let facts = ChaoxingExamFacts {
            score_milli_points: extract_exam_score_milli(root)?,
            retake_available: exam_retake_available(root),
        };
        let text = normalize_evidence(&root.text().collect::<Vec<_>>().join(" "));
        if facts.score_milli_points.is_none()
            && !facts.retake_available
            && !text.contains("我的答案")
        {
            return Err(protocol_drift(
                "Chaoxing Exam detail response contains no result facts",
            ));
        }
        Ok(facts)
    })();
    visible.zeroize();
    result
}

pub(crate) fn apply_exam_detail_facts(
    task: &mut RemoteTask,
    facts: ChaoxingExamFacts,
) -> ProviderResult<()> {
    if task.source_type != SourceType::Exam {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Exam detail facts were applied to another module",
        ));
    }
    let normalized = task.normalized.as_object_mut().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Exam task has invalid normalized metadata",
        )
    })?;
    let list_score = normalized_score(normalized.get("score"))?;
    let detail_score = facts.score();
    if list_score.is_some() && detail_score.is_some() && list_score != detail_score {
        return Err(protocol_drift(
            "Chaoxing Exam list and detail scores disagree",
        ));
    }
    let list_retake = normalized
        .get("retake_available")
        .and_then(Value::as_bool)
        .ok_or_else(|| protocol_drift("Chaoxing Exam task has no retake fact"))?;
    let score = detail_score.or(list_score);
    let retake_available = list_retake || facts.retake_available;
    normalized.insert("score".to_owned(), json!(score));
    normalized.insert("retake_available".to_owned(), json!(retake_available));
    normalized.insert("detail_score".to_owned(), json!(detail_score));
    normalized.insert(
        "detail_retake_available".to_owned(),
        json!(facts.retake_available),
    );
    let raw = task.raw_sanitized.as_object_mut().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing Exam task has invalid sanitized metadata",
        )
    })?;
    raw.insert("score".to_owned(), json!(score));
    raw.insert("retake_available".to_owned(), json!(retake_available));
    raw.insert("detail_score".to_owned(), json!(detail_score));
    raw.insert(
        "detail_retake_available".to_owned(),
        json!(facts.retake_available),
    );
    task.fingerprint = fingerprint(&task.normalized)?;
    Ok(())
}

fn normalized_score(value: Option<&Value>) -> ProviderResult<Option<f64>> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|score| score.is_finite() && (0.0..=100.0).contains(score))
            .map(Some)
            .ok_or_else(|| protocol_drift("Chaoxing Exam task has an invalid score fact")),
        None => Err(protocol_drift("Chaoxing Exam task has no score fact")),
    }
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
    task.capabilities = if remote_state == RemoteState::Pending {
        vec![
            TaskCapability::ProgressRead,
            TaskCapability::QuestionInventory,
            TaskCapability::QuestionParse,
            TaskCapability::SubmissionBuild,
            TaskCapability::SubmissionExecute,
            TaskCapability::SubmissionVerify,
        ]
    } else {
        vec![TaskCapability::ProgressRead]
    };
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

fn extract_exam_score_milli(row: ElementRef<'_>) -> ProviderResult<Option<u32>> {
    let score_selector = selector(".exam-score, .score, [data-score]");
    let mut candidates = Vec::new();
    if let Some(value) = row.value().attr("data-score") {
        candidates.push(parse_explicit_score(value)?);
    }
    for node in row.select(&score_selector) {
        if let Some(value) = node.value().attr("data-score") {
            candidates.push(parse_explicit_score(value)?);
        }
        let candidate_count = candidates.len();
        let text = normalize_evidence(&node.text().collect::<Vec<_>>().join(" "));
        collect_score_candidates(&text, true, &mut candidates)?;
        if candidates.len() == candidate_count
            && text.bytes().any(|byte| byte.is_ascii_digit())
            && text.contains('分')
        {
            return Err(protocol_drift(
                "Chaoxing Exam row contains an invalid score fact",
            ));
        }
    }
    if candidates.is_empty() {
        let row_text = normalize_evidence(&row.text().collect::<Vec<_>>().join(" "));
        collect_score_candidates(&row_text, false, &mut candidates)?;
    }
    let Some(score) = candidates.first().copied() else {
        return Ok(None);
    };
    if candidates.iter().any(|candidate| *candidate != score) {
        return Err(protocol_drift(
            "Chaoxing Exam row contains conflicting score facts",
        ));
    }
    Ok(Some(score))
}

fn collect_score_candidates(
    text: &str,
    explicit_score_node: bool,
    candidates: &mut Vec<u32>,
) -> ProviderResult<()> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !bytes[cursor].is_ascii_digit()
            || cursor.checked_sub(1).is_some_and(|previous| {
                bytes[previous].is_ascii_alphanumeric() || bytes[previous] == b'.'
            })
        {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b'.' {
            cursor += 1;
            let fraction_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if fraction_start == cursor {
                continue;
            }
        }
        let number_end = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let suffix = &text[cursor..];
        if !suffix.starts_with('分') || suffix.starts_with("分钟") {
            continue;
        }
        let context_start = text[..start]
            .char_indices()
            .rev()
            .nth(8)
            .map_or(0, |(index, _)| index);
        let context = &text[context_start..start];
        if explicit_score_node || context.contains("成绩") || context.contains("得分") {
            candidates.push(parse_score_milli(&text[start..number_end])?);
        }
        cursor += '分'.len_utf8();
    }
    Ok(())
}

fn parse_explicit_score(value: &str) -> ProviderResult<u32> {
    let value = value.trim();
    let value = value.strip_suffix('分').map_or(value, str::trim_end);
    parse_score_milli(value)
}

fn parse_score_milli(value: &str) -> ProviderResult<u32> {
    let (whole, fraction) = value.split_once('.').map_or((value, ""), |parts| parts);
    if whole.is_empty()
        || whole.len() > 3
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 3
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift(
            "Chaoxing Exam row contains an invalid score fact",
        ));
    }
    let whole = whole
        .parse::<u32>()
        .map_err(|_| protocol_drift("Chaoxing Exam row contains an invalid score fact"))?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u32>()
            .map_err(|_| protocol_drift("Chaoxing Exam row contains an invalid score fact"))?
            * 10_u32.pow(u32::try_from(3 - fraction.len()).expect("bounded score precision"))
    };
    whole
        .checked_mul(1_000)
        .and_then(|whole| whole.checked_add(fraction))
        .filter(|score| *score <= 100_000)
        .ok_or_else(|| protocol_drift("Chaoxing Exam row score is out of range"))
}

fn exam_retake_available(row: ElementRef<'_>) -> bool {
    row.value()
        .attr("onclick")
        .is_some_and(|value| contains_javascript_call(value, "retest"))
        || row.select(&selector("[onclick]")).any(|node| {
            node.value()
                .attr("onclick")
                .is_some_and(|value| contains_javascript_call(value, "retest"))
        })
}

pub(crate) fn contains_javascript_call(value: &str, function: &str) -> bool {
    let value = value.as_bytes();
    value
        .windows(function.len())
        .enumerate()
        .filter(|(_, candidate)| candidate.eq_ignore_ascii_case(function.as_bytes()))
        .any(|(start, _)| {
            let valid_start = start == 0
                || !value[start - 1].is_ascii_alphanumeric()
                    && !matches!(value[start - 1], b'_' | b'$');
            let mut end = start + function.len();
            while end < value.len() && value[end].is_ascii_whitespace() {
                end += 1;
            }
            valid_start && value.get(end) == Some(&b'(')
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
    const EXAM_DETAIL: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/detail-result.html");
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
        assert_eq!(selected_exam_facts(&tasks), expected(EXAM_EXPECTED));
        assert!(tasks.iter().all(|task| {
            task.source_type == SourceType::Exam
                && task.assessment_class == AssessmentClass::Unknown
                && task.fingerprint.starts_with("v1:")
                && task.fingerprint.len() == 67
        }));
        let pending = tasks
            .iter()
            .find(|task| task.remote_id.ends_with(":exam-1"))
            .unwrap();
        assert_eq!(
            pending.capabilities,
            [
                TaskCapability::ProgressRead,
                TaskCapability::QuestionInventory,
                TaskCapability::QuestionParse,
                TaskCapability::SubmissionBuild,
                TaskCapability::SubmissionExecute,
                TaskCapability::SubmissionVerify,
            ]
        );
        assert!(
            tasks
                .iter()
                .filter(|task| !task.remote_id.ends_with(":exam-1"))
                .all(|task| task.capabilities == [TaskCapability::ProgressRead])
        );
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
    fn only_a_fresh_pending_work_detail_advertises_question_read() {
        let mut task = parse_work_inventory(WORK_MIXED, &scope())
            .unwrap()
            .into_iter()
            .find(|task| task.remote_id.ends_with(":work-1"))
            .unwrap();

        apply_work_detail_state(&mut task, RemoteState::Pending).unwrap();
        assert_eq!(
            task.capabilities,
            [
                TaskCapability::ProgressRead,
                TaskCapability::QuestionInventory,
                TaskCapability::QuestionParse,
                TaskCapability::SubmissionBuild,
                TaskCapability::SubmissionExecute,
                TaskCapability::SubmissionVerify,
            ]
        );

        apply_work_detail_state(&mut task, RemoteState::Expired).unwrap();
        assert_eq!(task.capabilities, [TaskCapability::ProgressRead]);
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

    #[test]
    fn exam_score_and_retake_facts_are_bounded_without_changing_capabilities() {
        let tasks = parse_exam_inventory(EXAM_MIXED, &scope()).unwrap();
        let completed = tasks
            .iter()
            .find(|task| task.remote_id.ends_with(":exam-2"))
            .unwrap();
        assert_eq!(completed.normalized["score"], 82.5);
        assert_eq!(completed.normalized["retake_available"], true);
        assert_eq!(completed.raw_sanitized["score"], 82.5);
        assert_eq!(completed.raw_sanitized["retake_available"], true);
        assert_eq!(completed.remote_state, RemoteState::Completed);
        assert_eq!(completed.capabilities, [TaskCapability::ProgressRead]);

        let remaining_minutes = tasks
            .iter()
            .find(|task| task.remote_id.ends_with(":exam-5"))
            .unwrap();
        assert_eq!(remaining_minutes.normalized["score"], Value::Null);

        for score in ["100.001", "101", "1.2345", "1e2"] {
            let html = format!(
                r#"<div class="exam-list-item" data-exam-id="exam-x" data="/exam/preview?examId=exam-x"><span class="exam-name">Synthetic</span><span class="exam-status">已完成</span><span class="exam-score">成绩：{score} 分</span></div>"#
            );
            assert!(parse_exam_inventory(&html, &scope()).is_err(), "{score}");
        }
    }

    #[test]
    fn exam_conflicting_scores_and_lookalike_retake_handlers_fail_closed() {
        let conflicting = r#"<div class="exam-list-item" data-exam-id="exam-x" data="/exam/preview?examId=exam-x"><span class="exam-name">Synthetic</span><span class="exam-status">已完成</span><span class="exam-score">成绩：80 分</span><span data-score="81"></span></div>"#;
        assert!(parse_exam_inventory(conflicting, &scope()).is_err());

        let lookalike = r#"<div class="exam-list-item" data-exam-id="exam-x" data="/exam/preview?examId=exam-x"><span class="exam-name">Synthetic</span><span class="exam-status">已完成</span><button onclick="prepareTest()">查看</button><button onclick="notReTest()">不可重考</button></div>"#;
        let task = parse_exam_inventory(lookalike, &scope()).unwrap().remove(0);
        assert_eq!(task.normalized["retake_available"], false);
        assert_eq!(task.remote_state, RemoteState::Completed);
        assert_eq!(task.capabilities, [TaskCapability::ProgressRead]);
    }

    #[test]
    fn exam_detail_facts_merge_without_changing_state_or_capabilities() {
        let facts = parse_exam_detail_facts(EXAM_DETAIL).unwrap();
        assert_eq!(facts.score(), Some(82.5));
        assert_eq!(facts.score_milli_points(), Some(82_500));
        assert!(facts.retake_available);

        let mut task = parse_exam_inventory(EXAM_MIXED, &scope())
            .unwrap()
            .into_iter()
            .find(|task| task.remote_id.ends_with(":exam-2"))
            .unwrap();
        let state = task.remote_state;
        let capabilities = task.capabilities.clone();
        apply_exam_detail_facts(&mut task, facts).unwrap();

        assert_eq!(task.normalized["score"], 82.5);
        assert_eq!(task.normalized["retake_available"], true);
        assert_eq!(task.normalized["detail_score"], 82.5);
        assert_eq!(task.normalized["detail_retake_available"], true);
        assert_eq!(task.remote_state, state);
        assert_eq!(task.capabilities, capabilities);
    }

    #[test]
    fn exam_detail_unknown_or_conflicting_results_fail_closed() {
        assert!(parse_exam_detail_facts("<main>页面待验证</main>").is_err());
        assert!(
            parse_exam_detail_facts(
                "<script>reTest(); const score = '99 分';</script><main>页面待验证</main>",
            )
            .is_err()
        );

        let mut task = parse_exam_inventory(EXAM_MIXED, &scope())
            .unwrap()
            .into_iter()
            .find(|task| task.remote_id.ends_with(":exam-2"))
            .unwrap();
        let conflicting = parse_exam_detail_facts(&EXAM_DETAIL.replace("82.5", "90")).unwrap();
        assert!(apply_exam_detail_facts(&mut task, conflicting).is_err());
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

    fn selected_exam_facts(tasks: &[RemoteTask]) -> Value {
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
                        "score": task.normalized["score"],
                        "retake_available": task.normalized["retake_available"],
                    })
                })
                .collect(),
        )
    }

    fn expected(value: &str) -> Value {
        serde_json::from_str(value).unwrap()
    }
}
