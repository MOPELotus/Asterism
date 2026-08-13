use std::fmt;

use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use asterism_secrets::SecretString;
use reqwest::Url;
use scraper::{Html, Selector};
use zeroize::Zeroize;

use crate::ChaoxingCourseRoute;

const MAX_EXAM_ENTRY_BYTES: usize = 8 * 1_024;
const MAX_EXAM_ID_BYTES: usize = 128;
const MAX_ENC_TASK_BYTES: usize = 2_048;
const MAX_ATTEMPT_ENC_BYTES: usize = 4_096;
const MAX_TITLE_BYTES: usize = 512;

/// Fresh Exam entry facts carried from one task inventory into the mutating
/// start chain. The entry and dynamic `enc_task` remain provider-private.
pub struct ChaoxingExamQuestionRequest<'a> {
    exam_id: String,
    enc_task: SecretString,
    route: ChaoxingCourseRoute<'a>,
}

impl<'a> ChaoxingExamQuestionRequest<'a> {
    pub(crate) fn try_new(
        route: ChaoxingCourseRoute<'a>,
        remote_task_id: &'a str,
        entry: &str,
    ) -> ProviderResult<Self> {
        if entry.is_empty()
            || entry.len() > MAX_EXAM_ENTRY_BYTES
            || entry.chars().any(char::is_control)
        {
            return Err(protocol_drift("Chaoxing Exam entry is empty or unbounded"));
        }
        let exam_id = remote_task_id
            .strip_prefix("exam:")
            .and_then(|value| value.strip_prefix(route.course_id()))
            .and_then(|value| value.strip_prefix(':'))
            .and_then(|value| value.strip_prefix(route.class_id()))
            .and_then(|value| value.strip_prefix(':'))
            .filter(|value| valid_component(value))
            .ok_or_else(|| protocol_drift("Chaoxing Exam task identity is not route-bound"))?;
        let enc_task = extract_enc_task(entry).ok_or_else(|| {
            protocol_drift("Chaoxing Exam entry has no bounded enc_task material")
        })?;
        if !entry
            .to_ascii_lowercase()
            .contains(&exam_id.to_ascii_lowercase())
        {
            return Err(protocol_drift(
                "Chaoxing Exam entry does not contain the current exam identity",
            ));
        }
        Ok(Self {
            exam_id: exam_id.to_owned(),
            enc_task: SecretString::new(enc_task),
            route,
        })
    }

    pub(crate) fn exam_id(&self) -> &str {
        &self.exam_id
    }

    pub(crate) fn enc_task(&self) -> &str {
        self.enc_task.expose_secret()
    }

    pub(crate) const fn route(&self) -> ChaoxingCourseRoute<'a> {
        self.route
    }
}

impl fmt::Debug for ChaoxingExamQuestionRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamQuestionRequest")
            .field("exam_id", &self.exam_id)
            .field("enc_task", &"[REDACTED]")
            .field("route", &"[REDACTED]")
            .finish()
    }
}

/// Non-secret metadata from the Exam cover page. Dynamic attempt material is
/// intentionally kept separate and is only produced after the one-shot start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChaoxingExamCover {
    pub(crate) title: String,
    pub(crate) exam_answer_id: String,
    pub(crate) monitor_enc: String,
    pub(crate) need_code: bool,
    pub(crate) need_face: bool,
    pub(crate) need_captcha: bool,
    pub(crate) captcha_id: Option<String>,
}

/// Dynamic attempt state returned by the start redirect/page.
pub(crate) struct ChaoxingExamAttemptMaterial {
    pub(crate) exam_answer_id: String,
    pub(crate) enc: SecretString,
    pub(crate) enc_remain_time: u64,
    pub(crate) remain_time: u64,
    pub(crate) last_update_time: u64,
}

impl fmt::Debug for ChaoxingExamAttemptMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingExamAttemptMaterial")
            .field("exam_answer_id", &self.exam_answer_id)
            .field("enc", &"[REDACTED]")
            .field("enc_remain_time", &self.enc_remain_time)
            .field("remain_time", &self.remain_time)
            .field("last_update_time", &self.last_update_time)
            .finish()
    }
}

impl Drop for ChaoxingExamAttemptMaterial {
    fn drop(&mut self) {
        self.exam_answer_id.zeroize();
    }
}

pub(crate) fn parse_exam_cover(html: &str) -> ProviderResult<ChaoxingExamCover> {
    bounded_html(html)?;
    let document = Html::parse_document(html);
    if let Some(message) = first_text(&document, "h2.color6.fs36.textCenter, p.blankTips, li.msg") {
        let normalized = normalize_text(&message);
        if normalized.contains("尚未开始") || normalized.contains("未开始") {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing Exam has not started",
            ));
        }
        if normalized.contains("验证码") || normalized.contains("人脸") {
            return Err(ProviderError::human_required(
                "Chaoxing Exam requires browser verification before start",
                asterism_domain::HumanRequiredReason::BrowserRequired,
            ));
        }
        return Err(ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing Exam cover returned a non-startable state",
        ));
    }
    let exam_answer_id = input_value(&document, "#testUserRelationId")
        .filter(|value| valid_component(value))
        .ok_or_else(|| protocol_drift("Chaoxing Exam cover has no attempt identity"))?;
    let monitor_enc = input_value(&document, "#monitorEnc").unwrap_or_default();
    if monitor_enc.len() > MAX_ATTEMPT_ENC_BYTES {
        return Err(invalid_response(
            "Chaoxing Exam monitor material is unbounded",
        ));
    }
    let title = first_text(&document, "span.overHidden2")
        .map(|value| normalize_text(&value))
        .filter(|value| !value.is_empty() && value.len() <= MAX_TITLE_BYTES)
        .ok_or_else(|| protocol_drift("Chaoxing Exam cover has no bounded title"))?;
    let need_code = script_flag(&document, "needcode");
    let need_face = input_value(&document, "#faceRecognitionCompare").is_some_and(|v| v != "0");
    let need_captcha = input_value(&document, "#captchaCheck").is_some_and(|v| v != "0");
    let captcha_id = input_value(&document, "#captchaCaptchaId").filter(|v| !v.is_empty());
    Ok(ChaoxingExamCover {
        title,
        exam_answer_id,
        monitor_enc,
        need_code,
        need_face,
        need_captcha,
        captcha_id,
    })
}

pub(crate) fn parse_exam_attempt(
    final_url: &Url,
    html: &str,
    expected_answer_id: &str,
) -> ProviderResult<ChaoxingExamAttemptMaterial> {
    bounded_html(html)?;
    let enc = final_url
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("enc"))
        .map(|(_, value)| value.into_owned())
        .or_else(|| {
            let document = Html::parse_document(html);
            input_value(&document, "form#submitTest input#enc")
        })
        .filter(|value| !value.is_empty() && value.len() <= MAX_ATTEMPT_ENC_BYTES)
        .ok_or_else(|| protocol_drift("Chaoxing Exam start returned no bounded enc"))?;
    let document = Html::parse_document(html);
    let exam_answer_id = input_value(&document, "#testUserRelationId")
        .unwrap_or_else(|| expected_answer_id.to_owned());
    if exam_answer_id != expected_answer_id || !valid_component(&exam_answer_id) {
        return Err(protocol_drift(
            "Chaoxing Exam start changed the bound attempt identity",
        ));
    }
    let enc_remain_time = bounded_u64_input(&document, "#encRemainTime")?;
    let remain_time = bounded_u64_input(&document, "#remainTime")?;
    let last_update_time = bounded_u64_input(&document, "#encLastUpdateTime")?;
    Ok(ChaoxingExamAttemptMaterial {
        exam_answer_id,
        enc: SecretString::new(enc),
        enc_remain_time,
        remain_time,
        last_update_time,
    })
}

pub(crate) fn valid_exam_question_url(
    url: &Url,
    route: ChaoxingCourseRoute<'_>,
    exam_id: &str,
    exam_answer_id: &str,
) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("mooc1-api.chaoxing.com")
        && url.path().contains("/exam-ans/exam/")
        && unique_query(url, "courseId").as_deref() == Some(route.course_id())
        && unique_query(url, "classId").as_deref() == Some(route.class_id())
        && unique_query(url, "tId")
            .or_else(|| unique_query(url, "examRelationId"))
            .is_some_and(|value| value == exam_id)
        && unique_query(url, "id")
            .or_else(|| unique_query(url, "examRelationAnswerId"))
            .is_some_and(|value| value == exam_answer_id)
}

pub(crate) fn valid_exam_start_redirect(
    url: &Url,
    route: ChaoxingCourseRoute<'_>,
    exam_id: &str,
) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("mooc1-api.chaoxing.com")
        && url.path().contains("/exam-ans/exam/")
        && unique_query(url, "courseId").as_deref() == Some(route.course_id())
        && unique_query(url, "classId").as_deref() == Some(route.class_id())
        && unique_query(url, "tId")
            .or_else(|| unique_query(url, "examRelationId"))
            .is_some_and(|value| value == exam_id)
        && url.fragment().is_none()
}

fn extract_enc_task(entry: &str) -> Option<String> {
    if let Ok(url) = Url::parse(entry) {
        return unique_query(&url, "enc_task")
            .map(std::borrow::Cow::into_owned)
            .filter(|value| !value.is_empty() && value.len() <= MAX_ENC_TASK_BYTES);
    }
    let lower = entry.to_ascii_lowercase();
    let start = lower.find("gotest(")? + "gotest(".len();
    let args = entry.get(start..)?.split(')').next()?;
    args.split(',')
        .nth(6)
        .map(|value| value.trim().trim_matches(['\'', '"']).to_owned())
        .filter(|value| !value.is_empty() && value.len() <= MAX_ENC_TASK_BYTES)
}

fn input_value(document: &Html, selector_text: &str) -> Option<String> {
    let selector = Selector::parse(selector_text).ok()?;
    document
        .select(&selector)
        .next()?
        .value()
        .attr("value")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn bounded_u64_input(document: &Html, selector_text: &str) -> ProviderResult<u64> {
    input_value(document, selector_text)
        .ok_or_else(|| protocol_drift("Chaoxing Exam attempt omitted a timing field"))?
        .parse::<u64>()
        .map_err(|_| protocol_drift("Chaoxing Exam attempt has an invalid timing field"))
}

fn first_text(document: &Html, selector_text: &str) -> Option<String> {
    let selector = Selector::parse(selector_text).ok()?;
    document
        .select(&selector)
        .next()
        .map(|node| node.text().collect::<Vec<_>>().join(" "))
}

fn script_flag(document: &Html, name: &str) -> bool {
    document
        .select(&Selector::parse("script").expect("static script selector"))
        .flat_map(|node| node.text())
        .any(|script| {
            let needle = format!("var {name}");
            script.contains(&needle)
                && script
                    .split_once(&needle)
                    .and_then(|(_, tail)| tail.split_once(';'))
                    .is_some_and(|(value, _)| value.contains('1'))
        })
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded_html(value: &str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > 4 * 1_024 * 1_024
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(invalid_response(
            "Chaoxing Exam document is empty, unbounded, or contains controls",
        ));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EXAM_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn unique_query<'a>(url: &'a Url, key: &str) -> Option<std::borrow::Cow<'a, str>> {
    let mut values = url
        .query_pairs()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value);
    let value = values.next()?;
    values.next().is_none().then_some(value)
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
    use asterism_provider_api::ProviderRouteContext;
    use asterism_provider_api::RemoteCourse;

    const COVER: &str = include_str!("../../../fixtures/providers/chaoxing/exam/cover-ready.html");
    const START: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/start-question.html");

    #[test]
    fn cover_and_start_bind_dynamic_attempt_material() {
        let cover = parse_exam_cover(COVER).unwrap();
        assert_eq!(cover.exam_answer_id, "answer-1");
        assert!(cover.need_code);
        let url = Url::parse(
            "https://mooc1-api.chaoxing.com/exam-ans/exam/test/reVersionTestStartNew?courseId=100&classId=200&tId=exam-1&id=answer-1&enc=SAFE_ATTEMPT_ENC",
        )
        .unwrap();
        let attempt = parse_exam_attempt(&url, START, &cover.exam_answer_id).unwrap();
        assert_eq!(attempt.exam_answer_id, "answer-1");
        assert_eq!(attempt.enc_remain_time, 3600);
        assert_eq!(attempt.remain_time, 3600);
    }

    #[test]
    fn request_requires_route_bound_enc_task() {
        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "fixture".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        };
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let request = ChaoxingExamQuestionRequest::try_new(
            route,
            "exam:100:200:exam-1",
            "goTest('100','exam-1',0,'SAFE_TIME','paper-1',false,'SAFE_ENC')",
        )
        .unwrap();
        assert_eq!(request.exam_id(), "exam-1");
        assert_eq!(request.enc_task(), "SAFE_ENC");
        assert!(!format!("{request:?}").contains("SAFE_ENC"));
    }

    #[test]
    fn start_redirect_is_limited_to_the_attempt_host_and_identity() {
        let course = RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "fixture".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        };
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let valid = Url::parse(
            "https://mooc1-api.chaoxing.com/exam-ans/exam/test/reVersionTestStartNew?courseId=100&classId=200&tId=exam-1&id=answer-1",
        )
        .unwrap();
        assert!(valid_exam_start_redirect(&valid, route, "exam-1"));
        let foreign = Url::parse(
            "https://evil.invalid/exam-ans/exam/test/reVersionTestStartNew?courseId=100&classId=200&tId=exam-1",
        )
        .unwrap();
        assert!(!valid_exam_start_redirect(&foreign, route, "exam-1"));
    }
}
