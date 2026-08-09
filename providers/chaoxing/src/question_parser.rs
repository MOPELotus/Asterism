use std::collections::BTreeSet;

use asterism_domain::{
    Question, QuestionAttachment, QuestionAttachmentKind, QuestionId, QuestionKind, QuestionOption,
    TaskId,
};
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRouteContext, RemoteQuestionRef,
};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};

const MAX_QUESTION_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUESTIONS_PER_DOCUMENT: usize = 5_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuestionPageKind {
    ChapterWorkMobile,
    ExamMobile,
    WorkPreview,
    ExamPreview,
}

impl QuestionPageKind {
    const fn metadata_name(self) -> &'static str {
        match self {
            Self::ChapterWorkMobile => "chapter_work_mobile",
            Self::ExamMobile => "exam_mobile",
            Self::WorkPreview => "work_preview",
            Self::ExamPreview => "exam_preview",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedChaoxingQuestion {
    remote_id: String,
    position: u32,
    kind: QuestionKind,
    stem: String,
    options: Vec<QuestionOption>,
    attachments: Vec<QuestionAttachment>,
    metadata_sanitized: Value,
    page_kind: QuestionPageKind,
}

impl ParsedChaoxingQuestion {
    /// Builds the bounded attempt-local reference consumed by Core before
    /// requesting this normalized Question.
    ///
    /// # Errors
    ///
    /// Returns a sanitized Provider error if the reference or ephemeral page
    /// classification violates the Provider API contract.
    pub fn reference(&self) -> ProviderResult<RemoteQuestionRef> {
        let reference = RemoteQuestionRef {
            remote_id: self.remote_id.clone(),
            position: self.position,
            kind_hint: self.kind,
            metadata_sanitized: self.metadata_sanitized.clone(),
            route_context: ProviderRouteContext::try_from_pairs([(
                "page_kind".to_owned(),
                self.page_kind.metadata_name().to_owned(),
            )])?,
        };
        reference
            .validate()
            .map_err(|_| invalid_response("Chaoxing Question reference is invalid"))?;
        Ok(reference)
    }

    /// Binds this parsed record to one Core-owned Task identity.
    ///
    /// # Errors
    ///
    /// Returns a sanitized Provider error if the normalized Domain Question
    /// violates identity, content, collection or metadata bounds.
    pub fn to_question(&self, task_id: TaskId) -> ProviderResult<Question> {
        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some(self.remote_id.clone()),
            kind: self.kind,
            stem: self.stem.clone(),
            options: self.options.clone(),
            attachments: self.attachments.clone(),
            metadata_sanitized: self.metadata_sanitized.clone(),
            position: self.position,
        };
        question
            .validate()
            .map_err(|_| invalid_response("Chaoxing normalized Question is invalid"))?;
        Ok(question)
    }
}

/// Parses one complete donor-observed mobile Chapter Work Question document
/// without retaining hidden answers or submission fields.
///
/// # Errors
///
/// Returns a sanitized Provider error for login/error documents, unsupported
/// structure, duplicate identities, malformed content or boundedness failures.
pub fn parse_chapter_work_question_page(html: &str) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
    parse_question_page(html, QuestionPageKind::ChapterWorkMobile)
}

/// Parses one complete donor-observed mobile Exam preview document without
/// retaining hidden answers or submission fields.
///
/// # Errors
///
/// Returns a sanitized Provider error for login/error documents, unsupported
/// structure, duplicate identities, malformed content or boundedness failures.
pub fn parse_exam_question_page(html: &str) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
    parse_question_page(html, QuestionPageKind::ExamMobile)
}

/// Parses one complete current OCS-observed independent Work preview document.
///
/// # Errors
///
/// Returns a sanitized Provider error for login/error documents, unsupported
/// structure, duplicate identities, malformed content or boundedness failures.
pub fn parse_work_preview_question_page(html: &str) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
    parse_question_page(html, QuestionPageKind::WorkPreview)
}

/// Parses one complete current OCS-observed independent Exam preview document.
///
/// # Errors
///
/// Returns a sanitized Provider error for login/error documents, unsupported
/// structure, duplicate identities, malformed content or boundedness failures.
pub fn parse_exam_preview_question_page(html: &str) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
    parse_question_page(html, QuestionPageKind::ExamPreview)
}

fn parse_question_page(
    html: &str,
    page_kind: QuestionPageKind,
) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
    if html.is_empty() || html.len() > MAX_QUESTION_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Chaoxing Question document is empty or exceeds the size limit",
        ));
    }
    let document = Html::parse_document(html);
    reject_known_error_page(&document)?;
    let node_selector = selector(match page_kind {
        QuestionPageKind::ChapterWorkMobile => "div.Py-mian1",
        QuestionPageKind::ExamMobile => "div.questionWrap.singleQuesId.ans-cc-exam",
        QuestionPageKind::WorkPreview | QuestionPageKind::ExamPreview => ".questionLi",
    });
    let nodes = document.select(&node_selector).collect::<Vec<_>>();
    if nodes.is_empty() || nodes.len() > MAX_QUESTIONS_PER_DOCUMENT {
        return Err(protocol_drift(
            "Chaoxing Question document has no bounded supported Question set",
        ));
    }
    let mut remote_ids = BTreeSet::new();
    let mut questions = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.into_iter().enumerate() {
        let position = u32::try_from(index + 1)
            .map_err(|_| invalid_response("Chaoxing Question position exceeds the limit"))?;
        let question = parse_question_node(node, position, page_kind)?;
        if !remote_ids.insert(question.remote_id.clone()) {
            return Err(invalid_response(
                "Chaoxing Question document contains duplicate attempt-local identity",
            ));
        }
        question.reference()?;
        questions.push(question);
    }
    Ok(questions)
}

fn parse_question_node(
    node: ElementRef<'_>,
    position: u32,
    page_kind: QuestionPageKind,
) -> ProviderResult<ParsedChaoxingQuestion> {
    let type_input = node
        .select(&selector(match page_kind {
            QuestionPageKind::ChapterWorkMobile | QuestionPageKind::WorkPreview => {
                "input[id^='answertype']"
            }
            QuestionPageKind::ExamMobile | QuestionPageKind::ExamPreview => "input[name^='type']",
        }))
        .next()
        .ok_or_else(|| protocol_drift("Chaoxing Question has no type input"))?;
    let remote_id = match page_kind {
        QuestionPageKind::ChapterWorkMobile | QuestionPageKind::WorkPreview => type_input
            .value()
            .attr("id")
            .and_then(|value| value.strip_prefix("answertype")),
        QuestionPageKind::ExamMobile | QuestionPageKind::ExamPreview => node
            .select(&selector("input[name='questionId']"))
            .next()
            .and_then(|input| input.value().attr("value")),
    }
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .ok_or_else(|| protocol_drift("Chaoxing Question has no attempt-local identity"))?
    .to_owned();
    let type_code = type_input
        .value()
        .attr("value")
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| protocol_drift("Chaoxing Question has an invalid type"))?;
    let kind = question_kind(type_code);
    let title = node
        .select(&selector(match page_kind {
            QuestionPageKind::ChapterWorkMobile => "div.Py-m1-title",
            QuestionPageKind::ExamMobile => "div.tit",
            QuestionPageKind::WorkPreview | QuestionPageKind::ExamPreview => {
                "h3, .mark_name, .question-title, .stem"
            }
        }))
        .next()
        .ok_or_else(|| protocol_drift("Chaoxing Question has no stem"))?;
    let stem = normalized_stem(title, position, page_kind)?;
    let attachments = parse_attachments(title);
    let options = if matches!(
        kind,
        QuestionKind::SingleChoice
            | QuestionKind::MultipleChoice
            | QuestionKind::Matching
            | QuestionKind::Ordering
    ) {
        parse_options(node, page_kind)?
    } else {
        Vec::new()
    };
    if matches!(
        kind,
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice
    ) && options.len() < 2
    {
        return Err(protocol_drift(
            "Chaoxing choice Question has fewer than two options",
        ));
    }
    let metadata_sanitized = json!({
        "page_kind": page_kind.metadata_name(),
        "provider_type_code": type_code,
    });
    let parsed = ParsedChaoxingQuestion {
        remote_id,
        position,
        kind,
        stem,
        options,
        attachments,
        metadata_sanitized,
        page_kind,
    };
    parsed.to_question(TaskId::new())?;
    Ok(parsed)
}

fn parse_options(
    node: ElementRef<'_>,
    page_kind: QuestionPageKind,
) -> ProviderResult<Vec<QuestionOption>> {
    let option_selector = selector(match page_kind {
        QuestionPageKind::ChapterWorkMobile => "li.more-choose-item",
        QuestionPageKind::ExamMobile => "div.answerList.radioList",
        QuestionPageKind::WorkPreview | QuestionPageKind::ExamPreview => {
            ".answerBg .answer_p, .textDIV, .eidtDiv"
        }
    });
    let mut identifiers = BTreeSet::new();
    let mut options = Vec::new();
    for option in node.select(&option_selector) {
        let (id, content_node) = match page_kind {
            QuestionPageKind::ChapterWorkMobile => {
                let id = option
                    .select(&selector("em.choose-opt"))
                    .next()
                    .and_then(|key| key.value().attr("id-param"));
                let content = option.select(&selector("div.choose-desc")).next();
                (id, content)
            }
            QuestionPageKind::ExamMobile => {
                let id = option.value().attr("name");
                let content = option.select(&selector("cc")).next().or(Some(option));
                (id, content)
            }
            QuestionPageKind::WorkPreview | QuestionPageKind::ExamPreview => {
                let id = ["data-option-id", "data-option", "data-value"]
                    .into_iter()
                    .find_map(|attribute| option.value().attr(attribute))
                    .or_else(|| {
                        option
                            .select(&selector("input[value]"))
                            .next()
                            .and_then(|input| input.value().attr("value"))
                    });
                (id, Some(option))
            }
        };
        let id = id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| protocol_drift("Chaoxing Question option has no identity"))?
            .to_owned();
        if !identifiers.insert(id.clone()) {
            return Err(invalid_response(
                "Chaoxing Question contains duplicate option identity",
            ));
        }
        let content_node = content_node
            .ok_or_else(|| protocol_drift("Chaoxing Question option has no content"))?;
        let content = normalized_text(content_node.text());
        let attachments = parse_attachments(content_node);
        options.push(QuestionOption {
            id,
            content: (!content.is_empty()).then_some(content),
            attachments,
            metadata_sanitized: json!({}),
        });
    }
    Ok(options)
}

fn normalized_stem(
    title: ElementRef<'_>,
    position: u32,
    page_kind: QuestionPageKind,
) -> ProviderResult<String> {
    let mut fragments = title
        .text()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    match page_kind {
        QuestionPageKind::ChapterWorkMobile
        | QuestionPageKind::WorkPreview
        | QuestionPageKind::ExamPreview
            if fragments.len() > 2 =>
        {
            fragments.drain(..2);
        }
        QuestionPageKind::ExamMobile => {
            if let Some(heading) = title
                .select(&selector("h3"))
                .next()
                .map(|heading| normalized_text(heading.text()))
            {
                fragments.retain(|fragment| normalized_text([*fragment]) != heading);
            }
            if fragments
                .first()
                .is_some_and(|fragment| is_position_fragment(fragment, position))
            {
                fragments.remove(0);
            }
        }
        QuestionPageKind::ChapterWorkMobile
        | QuestionPageKind::WorkPreview
        | QuestionPageKind::ExamPreview => {}
    }
    let stem = normalized_text(fragments);
    if stem.is_empty() && parse_attachments(title).is_empty() {
        Err(protocol_drift("Chaoxing Question stem is empty"))
    } else {
        Ok(stem)
    }
}

fn normalized_text<'a>(fragments: impl IntoIterator<Item = &'a str>) -> String {
    fragments
        .into_iter()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_position_fragment(value: &str, position: u32) -> bool {
    let value = value.trim();
    [
        format!("{position}."),
        format!("{position}、"),
        format!("{position}．"),
    ]
    .iter()
    .any(|prefix| value == prefix || value.starts_with(prefix))
}

fn parse_attachments(scope: ElementRef<'_>) -> Vec<QuestionAttachment> {
    let mut attachments = Vec::new();
    for image in scope.select(&selector("img")) {
        let label = image
            .value()
            .attr("alt")
            .map(|value| normalized_text([value]))
            .filter(|value| !value.is_empty());
        let class = image.value().attr("class").unwrap_or_default();
        attachments.push(QuestionAttachment {
            kind: if class.split_ascii_whitespace().any(|value| value == "latex") {
                QuestionAttachmentKind::Formula
            } else {
                QuestionAttachmentKind::Image
            },
            remote_id: None,
            label,
            metadata_sanitized: json!({}),
        });
    }
    for (element, kind) in [
        ("audio", QuestionAttachmentKind::Audio),
        ("video", QuestionAttachmentKind::Video),
    ] {
        for media in scope.select(&selector(element)) {
            let label = media
                .value()
                .attr("title")
                .or_else(|| media.value().attr("aria-label"))
                .map(|value| normalized_text([value]))
                .filter(|value| !value.is_empty());
            attachments.push(QuestionAttachment {
                kind,
                remote_id: None,
                label,
                metadata_sanitized: json!({}),
            });
        }
    }
    attachments
}

const fn question_kind(type_code: u16) -> QuestionKind {
    match type_code {
        0 => QuestionKind::SingleChoice,
        1 => QuestionKind::MultipleChoice,
        2 => QuestionKind::FillBlank,
        3 => QuestionKind::TrueFalse,
        4..=9 | 18 | 19 => QuestionKind::ShortAnswer,
        10 | 14 | 15 | 20 => QuestionKind::Composite,
        11 => QuestionKind::Matching,
        13 => QuestionKind::Ordering,
        _ => QuestionKind::Unknown,
    }
}

fn reject_known_error_page(document: &Html) -> ProviderResult<()> {
    if document
        .select(&selector("form[action*='fanyalogin'], input[name='uname']"))
        .next()
        .is_some()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Question read requires a renewed authenticated session",
        ));
    }
    if document
        .select(&selector("p.blankTips, li.msg"))
        .next()
        .is_some()
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Chaoxing Question page is not readable in its current state",
        ));
    }
    Ok(())
}

fn selector(value: &'static str) -> Selector {
    Selector::parse(value).expect("static Chaoxing Question selector must be valid")
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

    const CHAPTER_WORK_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-mobile-mixed.html");
    const EXAM_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/exam-mobile-mixed.html");
    const WORK_PREVIEW_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-preview-mixed.html");
    const EXAM_PREVIEW_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/exam-preview-mixed.html");

    #[test]
    fn work_mobile_questions_are_typed_bounded_and_answer_free() {
        let parsed = parse_chapter_work_question_page(CHAPTER_WORK_MIXED).unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(
            parsed
                .iter()
                .map(|question| question.kind)
                .collect::<Vec<_>>(),
            [
                QuestionKind::SingleChoice,
                QuestionKind::MultipleChoice,
                QuestionKind::FillBlank,
                QuestionKind::TrueFalse,
            ]
        );
        assert_eq!(parsed[0].remote_id, "work-q-1");
        assert_eq!(parsed[0].options[1].content.as_deref(), Some("Mars"));
        assert_eq!(parsed[2].metadata_sanitized["provider_type_code"], 2);
        assert_eq!(
            parsed[3].attachments[0].kind,
            QuestionAttachmentKind::Formula
        );
        for question in parsed {
            let serialized =
                serde_json::to_string(&question.to_question(TaskId::new()).unwrap()).unwrap();
            assert!(!serialized.contains("hidden-answer"));
            assert!(!serialized.contains("answerInput"));
        }
    }

    #[test]
    fn exam_mobile_questions_preserve_attempt_order_and_extended_types() {
        let parsed = parse_exam_question_page(EXAM_MIXED).unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].stem, "Choose the first letter.");
        assert_eq!(parsed[0].options.len(), 2);
        assert_eq!(parsed[1].kind, QuestionKind::TrueFalse);
        assert_eq!(parsed[2].kind, QuestionKind::Matching);
        assert_eq!(parsed[3].kind, QuestionKind::Composite);
        assert_eq!(parsed[3].attachments[0].kind, QuestionAttachmentKind::Audio);
        for (index, question) in parsed.iter().enumerate() {
            let reference = question.reference().unwrap();
            assert_eq!(reference.position, u32::try_from(index + 1).unwrap());
            assert_eq!(reference.remote_id, format!("exam-q-{}", index + 1));
        }
    }

    #[test]
    fn current_independent_work_and_exam_previews_use_distinct_page_kinds() {
        let work = parse_work_preview_question_page(WORK_PREVIEW_MIXED).unwrap();
        assert_eq!(work.len(), 2);
        assert_eq!(work[0].remote_id, "work-preview-q-1");
        assert_eq!(work[0].kind, QuestionKind::SingleChoice);
        assert_eq!(work[0].stem, "Choose the bounded Work option.");
        assert_eq!(work[0].metadata_sanitized["page_kind"], "work_preview");
        assert_eq!(work[1].kind, QuestionKind::FillBlank);

        let exam = parse_exam_preview_question_page(EXAM_PREVIEW_MIXED).unwrap();
        assert_eq!(exam.len(), 2);
        assert_eq!(exam[0].remote_id, "exam-preview-q-1");
        assert_eq!(exam[0].kind, QuestionKind::MultipleChoice);
        assert_eq!(exam[0].options.len(), 3);
        assert_eq!(exam[1].kind, QuestionKind::Ordering);
        assert_eq!(exam[1].metadata_sanitized["page_kind"], "exam_preview");

        let serialized =
            serde_json::to_string(&work[1].to_question(TaskId::new()).unwrap()).unwrap();
        assert!(!serialized.contains("hidden current response"));
    }

    #[test]
    fn duplicate_attempt_question_ids_fail_closed() {
        let duplicated = EXAM_MIXED.replace("exam-q-2", "exam-q-1");
        let error = parse_exam_question_page(&duplicated).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[test]
    fn login_and_unknown_documents_are_not_treated_as_empty_question_sets() {
        let authentication = parse_chapter_work_question_page(
            "<form action='/fanyalogin'><input name='uname'></form>",
        )
        .unwrap_err();
        assert_eq!(authentication.kind, ProviderErrorKind::Authentication);
        let drift = parse_exam_question_page("<html><body>changed</body></html>").unwrap_err();
        assert_eq!(drift.kind, ProviderErrorKind::ProtocolDrift);
    }
}
