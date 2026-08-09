use std::collections::HashSet;

use asterism_domain::{AssessmentClass, RemoteState, SourceType};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult, RemoteTask};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ChaoxingCourseScope;

const MAX_CHAPTER_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_CHAPTER_TASKS: usize = 4_096;
const MAX_TITLE_BYTES: usize = 512;
const MAX_STATUS_BYTES: usize = 1_024;
const MAX_JOB_COUNT: u32 = 10_000;

/// Parses Chaoxing's course chapter tree independently from Work and Exam.
///
/// # Errors
///
/// Returns a protocol error when a chapter-like row has no donor-observed
/// `cur{knowledgeId}` identity, bounded title, valid job count, or coherent
/// completion/lock state.
pub fn parse_chapter_inventory(
    html: &str,
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<RemoteTask>> {
    if html.is_empty() || html.len() > MAX_CHAPTER_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing chapter document is empty or exceeds the size limit",
        ));
    }
    let document = Html::parse_document(html);
    let chapter_selector = selector("div.chapter_unit li > div[id^='cur']");
    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    for chapter in document.select(&chapter_selector) {
        if tasks.len() == MAX_CHAPTER_TASKS {
            return Err(invalid_response(
                "Chaoxing chapter count exceeds the size limit",
            ));
        }
        let knowledge_id = chapter
            .value()
            .attr("id")
            .and_then(|value| value.strip_prefix("cur"))
            .filter(|value| {
                (1..=20).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
            })
            .ok_or_else(|| protocol_drift("Chaoxing chapter has an invalid knowledge identity"))?;
        let remote_id = format!(
            "chapter:{}:{}:{knowledge_id}",
            scope.course_id(),
            scope.class_id()
        );
        if !seen.insert(remote_id.clone()) {
            return Err(protocol_drift(
                "Chaoxing chapter inventory contains a duplicate identity",
            ));
        }
        let title = chapter_title(chapter)?;
        let job_count = chapter_job_count(chapter)?;
        let status_text = chapter_status(chapter);
        let completed = status_text.contains("已完成");
        let locked = status_text.contains("解锁");
        if completed && locked {
            return Err(protocol_drift(
                "Chaoxing chapter reports conflicting completion and lock state",
            ));
        }
        let remote_state = if completed {
            RemoteState::Completed
        } else if locked {
            RemoteState::NotOpen
        } else {
            RemoteState::Pending
        };
        let normalized = json!({
            "schema": "chaoxing.chapter.v1",
            "module": "chapter",
            "course_id": scope.course_id(),
            "class_id": scope.class_id(),
            "knowledge_id": knowledge_id,
            "title": title,
            "job_count": job_count,
            "remote_state": remote_state,
            "locked": locked,
        });
        tasks.push(RemoteTask {
            remote_id,
            course_remote_id: Some(scope.remote_course_id().to_owned()),
            title,
            source_type: SourceType::Chapter,
            assessment_class: AssessmentClass::Unknown,
            remote_state,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities: Vec::new(),
            fingerprint: fingerprint(&normalized)?,
            normalized,
            raw_sanitized: json!({
                "job_count": job_count,
                "status_text": status_text,
                "locked": locked,
            }),
        });
    }
    Ok(tasks)
}

fn chapter_title(chapter: ElementRef<'_>) -> ProviderResult<String> {
    let title_selector = selector("a.clicktitle");
    let title = chapter
        .select(&title_selector)
        .next()
        .map(|node| normalize_text(&node.text().collect::<Vec<_>>().join(" ")))
        .unwrap_or_default();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.chars().any(char::is_control) {
        return Err(protocol_drift(
            "Chaoxing chapter has an invalid or missing title",
        ));
    }
    Ok(title)
}

fn chapter_job_count(chapter: ElementRef<'_>) -> ProviderResult<u32> {
    let count_selector = selector("input.knowledgeJobCount[value]");
    let Some(value) = chapter
        .select(&count_selector)
        .next()
        .and_then(|node| node.value().attr("value"))
    else {
        return Ok(1);
    };
    value
        .parse::<u32>()
        .ok()
        .filter(|count| *count <= MAX_JOB_COUNT)
        .ok_or_else(|| protocol_drift("Chaoxing chapter has an invalid job count"))
}

fn chapter_status(chapter: ElementRef<'_>) -> String {
    let status_selector = selector("span.bntHoverTips");
    let mut status = chapter
        .select(&status_selector)
        .next()
        .map(|node| normalize_text(&node.text().collect::<Vec<_>>().join(" ")))
        .unwrap_or_default();
    if status.len() > MAX_STATUS_BYTES {
        let mut boundary = MAX_STATUS_BYTES;
        while !status.is_char_boundary(boundary) {
            boundary -= 1;
        }
        status.truncate(boundary);
    }
    status
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fingerprint(normalized: &Value) -> ProviderResult<String> {
    let bytes = serde_json::to_vec(normalized).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "failed to serialize sanitized Chaoxing chapter inventory",
        )
    })?;
    Ok(format!("v1:{:x}", Sha256::digest(bytes)))
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static Chaoxing chapter selector must be valid")
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

    const CHAPTER_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
    const CHAPTER_EXPECTED: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.expected.json");

    #[test]
    fn chapter_parser_keeps_module_identity_job_count_and_state_separate() {
        let tasks = parse_chapter_inventory(CHAPTER_MIXED, &scope()).unwrap();
        let selected = Value::Array(
            tasks
                .iter()
                .map(|task| {
                    json!({
                        "remote_id": task.remote_id,
                        "title": task.title,
                        "source_type": task.source_type,
                        "remote_state": task.remote_state,
                        "job_count": task.normalized["job_count"],
                    })
                })
                .collect(),
        );
        assert_eq!(
            selected,
            serde_json::from_str::<Value>(CHAPTER_EXPECTED).unwrap()
        );
        assert!(tasks.iter().all(|task| {
            task.course_remote_id.as_deref() == Some("course:100:200")
                && task.fingerprint.starts_with("v1:")
                && task.fingerprint.len() == 67
        }));
    }

    #[test]
    fn chapter_parser_rejects_duplicate_malformed_or_conflicting_rows() {
        assert!(
            parse_chapter_inventory(&CHAPTER_MIXED.replace("cur4002", "cur4001"), &scope())
                .is_err()
        );
        assert!(
            parse_chapter_inventory(&CHAPTER_MIXED.replace("cur4002", "curbad"), &scope()).is_err()
        );
        assert!(
            parse_chapter_inventory(
                &CHAPTER_MIXED.replace("value=\"3\"", "value=\"many\""),
                &scope()
            )
            .is_err()
        );
        assert!(
            parse_chapter_inventory(
                &CHAPTER_MIXED.replace("等待解锁", "等待解锁 已完成"),
                &scope(),
            )
            .is_err()
        );
    }

    fn scope() -> ChaoxingCourseScope {
        ChaoxingCourseScope::new("course:100:200", "100", "200").unwrap()
    }
}
