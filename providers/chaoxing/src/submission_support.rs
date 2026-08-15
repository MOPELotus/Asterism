use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use asterism_domain::{
    AnswerCandidate, AnswerConfidence, AnswerSource, NormalizedAnswer, ProtocolObservationKind,
    ProtocolSurface, Question, QuestionId, QuestionKind, RemoteState, SubmissionDraft,
    SubmissionQuestionVerification, SubmissionQuestionVerificationStatus, SubmissionReceipt,
    SubmissionScore, SubmissionVerificationSnapshot, SubmissionVerificationStatus,
};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use chrono::Utc;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use serde_json::json;
use zeroize::{Zeroize, Zeroizing};

use crate::inventory::{contains_javascript_call, parse_exam_detail_facts};

const MAX_REMOTE_TASK_ID_BYTES: usize = 640;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 128;
const MAX_FORM_FIELDS: usize = 128;
const MAX_FORM_FIELD_BYTES: usize = 64 * 1_024;
const MAX_RESPONSE_BYTES: usize = 4 * 1_024 * 1_024;

const FORWARDED_FORM_FIELDS: &[&str] = &[
    "courseId",
    "classId",
    "api",
    "mooc",
    "mooc2",
    "workAnswerId",
    "totalQuestionNum",
    "fullScore",
    "knowledgeid",
    "oldSchoolId",
    "oldWorkId",
    "jobid",
    "originJobId",
    "workRelationId",
    "enc_work",
    "isphone",
    "userId",
    "workTimesEnc",
    "cpi",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubmissionModule {
    IndependentWork,
    ChapterWork,
    Exam,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkSubmissionIdentity<'a> {
    remote_task: &'a str,
    course: &'a str,
    class: &'a str,
    knowledge: Option<&'a str>,
    module: SubmissionModule,
}

impl<'a> WorkSubmissionIdentity<'a> {
    pub(crate) fn parse(remote_task_id: &'a str) -> ProviderResult<Self> {
        if remote_task_id.is_empty()
            || remote_task_id.len() > MAX_REMOTE_TASK_ID_BYTES
            || remote_task_id.chars().any(char::is_control)
        {
            return Err(invalid_response("Chaoxing remote Work identity is invalid"));
        }
        let components = remote_task_id.split(':').collect::<Vec<_>>();
        let (module, course_id, class_id, knowledge) = match components.as_slice() {
            ["work", course, class, work] => {
                valid_component(Some(course))?;
                valid_component(Some(class))?;
                valid_component(Some(work))?;
                (SubmissionModule::IndependentWork, *course, *class, None)
            }
            ["resource", course, class, knowledge, job] => {
                valid_component(Some(course))?;
                valid_component(Some(class))?;
                valid_component(Some(knowledge))?;
                valid_component(Some(job))?;
                (
                    SubmissionModule::ChapterWork,
                    *course,
                    *class,
                    Some(*knowledge),
                )
            }
            ["exam", course, class, exam] => {
                valid_component(Some(course))?;
                valid_component(Some(class))?;
                valid_component(Some(exam))?;
                (SubmissionModule::Exam, *course, *class, None)
            }
            _ => {
                return Err(unsupported(
                    "Chaoxing submission received an unsupported task family",
                ));
            }
        };
        if remote_task_id.chars().any(char::is_control) {
            return Err(invalid_response(
                "Chaoxing remote submission identity is invalid",
            ));
        }
        Ok(Self {
            remote_task: remote_task_id,
            course: course_id,
            class: class_id,
            knowledge,
            module,
        })
    }

    pub(crate) const fn remote_task_id(self) -> &'a str {
        self.remote_task
    }

    pub(crate) const fn course_id(self) -> &'a str {
        self.course
    }

    pub(crate) const fn class_id(self) -> &'a str {
        self.class
    }

    pub(crate) const fn knowledge_id(self) -> Option<&'a str> {
        self.knowledge
    }

    pub(crate) const fn module(self) -> SubmissionModule {
        self.module
    }
}

/// Ephemeral, redacted answer plan rebuilt from one immutable submission draft.
pub struct ChaoxingSubmissionPlan {
    answers: Vec<PlannedAnswer>,
    total_question_count: usize,
}

struct PlannedAnswer {
    remote_question_id: String,
    type_code: String,
    value: String,
}

impl ChaoxingSubmissionPlan {
    /// Rebuilds one bounded Work answer plan without route or credential facts.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the draft contains unsupported question or
    /// answer shapes, stale Provider metadata, or ambiguous option identities.
    pub fn from_draft(draft: &SubmissionDraft) -> ProviderResult<Self> {
        if draft.items.is_empty() {
            return Err(invalid_response(
                "Chaoxing submission draft contains no Questions",
            ));
        }
        let mut remote_ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        let mut answers = Vec::with_capacity(draft.items.len());
        for item in &draft.items {
            let question = &item.question;
            let remote_question_id = question
                .remote_question_id
                .as_deref()
                .filter(|value| valid_question_id(value))
                .ok_or_else(|| {
                    invalid_response("Chaoxing submission Question identity is invalid")
                })?;
            if question.task_id != draft.task_id
                || item.selected.question_id != question.id
                || question.validate().is_err()
                || item.selected.answer.validate().is_err()
                || !matches!(
                    question
                        .metadata_sanitized
                        .get("page_kind")
                        .and_then(serde_json::Value::as_str),
                    Some("work_preview" | "chapter_work_mobile" | "exam_mobile")
                )
                || !remote_ids.insert(remote_question_id.to_owned())
                || !positions.insert(question.position)
            {
                return Err(invalid_response(
                    "Chaoxing submission draft Question binding is stale or invalid",
                ));
            }
            let type_code = provider_type_code(question.kind, &question.metadata_sanitized)?;
            let value = encode_answer(question, &item.selected.answer)?;
            answers.push(PlannedAnswer {
                remote_question_id: remote_question_id.to_owned(),
                type_code: type_code.to_owned(),
                value,
            });
        }
        let total_question_count = usize::try_from(draft.answer_coverage.total_question_count)
            .map_err(|_| invalid_response("Chaoxing submission coverage count is invalid"))?;
        Ok(Self {
            answers,
            total_question_count,
        })
    }

    pub(crate) fn answers(&self) -> impl ExactSizeIterator<Item = (&str, &str, &str)> {
        self.answers.iter().map(|answer| {
            (
                answer.remote_question_id.as_str(),
                answer.type_code.as_str(),
                answer.value.as_str(),
            )
        })
    }

    pub(crate) const fn len(&self) -> usize {
        self.answers.len()
    }

    pub(crate) const fn total_question_count(&self) -> usize {
        self.total_question_count
    }

    pub(crate) const fn is_partial(&self) -> bool {
        self.answers.len() != self.total_question_count
    }
}

impl fmt::Debug for ChaoxingSubmissionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSubmissionPlan")
            .field("answer_count", &self.answers.len())
            .field("total_question_count", &self.total_question_count)
            .field("answers", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingSubmissionPlan {
    fn drop(&mut self) {
        for answer in &mut self.answers {
            answer.remote_question_id.zeroize();
            answer.type_code.zeroize();
            answer.value.zeroize();
        }
        self.answers.clear();
    }
}

/// One bounded form assembled from a fresh editor and an immutable answer plan.
pub(crate) struct ChaoxingSubmissionForm {
    fields: Vec<(String, String)>,
}

impl ChaoxingSubmissionForm {
    /// Builds a final-submit form while refusing to forward unknown hidden
    /// fields or stale answer values from the remote editor.
    ///
    /// # Errors
    ///
    /// Returns a typed protocol error for missing identity/token fields,
    /// mismatched Questions, duplicate names, or unbounded values.
    pub(crate) fn parse(
        document: &str,
        identity: WorkSubmissionIdentity<'_>,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_response(
                "Chaoxing Work editor is empty or exceeds the size limit",
            ));
        }
        let html = Html::parse_document(document);
        reject_login_or_challenge(&html)?;
        let form_selector =
            selector("form#submitWork, form#form1, form[action*='addStudentWorkNew']");
        let mut forms = html.select(&form_selector);
        let form = forms
            .next()
            .ok_or_else(|| protocol_drift("Chaoxing Work editor has no submission form"))?;
        if forms.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing Work editor contains multiple submission forms",
            ));
        }
        let remote_types = validate_remote_question_partition(form, plan)?;

        let mut values = BTreeMap::new();
        for input in form.select(&selector("input[name]")) {
            let Some(name) = input.value().attr("name") else {
                continue;
            };
            if !FORWARDED_FORM_FIELDS.contains(&name) {
                continue;
            }
            let value = input.value().attr("value").unwrap_or_default();
            if value.len() > MAX_FORM_FIELD_BYTES || value.chars().any(char::is_control) {
                return Err(protocol_drift(
                    "Chaoxing Work submission field is unbounded or malformed",
                ));
            }
            if values.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err(protocol_drift(
                    "Chaoxing Work submission form contains duplicate fields",
                ));
            }
        }
        require_field(&values, "courseId", identity.course_id())?;
        require_field(&values, "classId", identity.class_id())?;
        let total = values
            .get("totalQuestionNum")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value == plan.total_question_count())
            .ok_or_else(|| {
                remote_changed("Chaoxing Work Question count changed before submission")
            })?;
        let _ = total;
        for required in ["workAnswerId", "workRelationId", "enc_work"] {
            values
                .get(required)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    protocol_drift("Chaoxing Work editor lacks required submission material")
                })?;
        }

        let mut fields = values.into_iter().collect::<Vec<_>>();
        fields.push(("pyFlag".to_owned(), String::new()));
        append_submission_answer_fields(&mut fields, remote_types, plan);
        if fields.len() > MAX_FORM_FIELDS {
            return Err(invalid_response(
                "Chaoxing Work submission form exceeds the field limit",
            ));
        }
        Ok(Self { fields })
    }

    pub(crate) fn fields(&self) -> &[(String, String)] {
        &self.fields
    }
}

fn validate_remote_question_partition(
    form: ElementRef<'_>,
    plan: &ChaoxingSubmissionPlan,
) -> ProviderResult<Vec<RemoteQuestionType>> {
    let remote_types = parse_remote_question_types(form)?;
    let planned_types = plan
        .answers()
        .map(|(id, type_code, _)| (id.to_owned(), type_code.to_owned()))
        .collect::<BTreeMap<_, _>>();
    if remote_types.len() != plan.total_question_count()
        || planned_types.iter().any(|(remote_id, type_code)| {
            remote_types
                .iter()
                .find(|remote| remote.remote_id == *remote_id)
                .is_none_or(|remote| remote.type_code != *type_code)
        })
    {
        return Err(remote_changed(
            "Chaoxing Work Questions changed after draft construction",
        ));
    }
    Ok(remote_types)
}

fn append_submission_answer_fields(
    fields: &mut Vec<(String, String)>,
    remote_types: Vec<RemoteQuestionType>,
    plan: &ChaoxingSubmissionPlan,
) {
    fields.push((
        "answerwqbid".to_owned(),
        format!(
            "{},",
            remote_types
                .iter()
                .map(|remote| remote.remote_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    ));
    let planned_answers = plan
        .answers()
        .map(|(remote_id, type_code, value)| (remote_id, (type_code, value)))
        .collect::<BTreeMap<_, _>>();
    for remote in remote_types {
        let value = planned_answers
            .get(remote.remote_id.as_str())
            .map_or("", |(_, value)| *value);
        fields.push((format!("answer{}", remote.remote_id), value.to_owned()));
        fields.push((
            format!("answertype{}", remote.remote_id),
            remote.type_code.clone(),
        ));
    }
}

impl fmt::Debug for ChaoxingSubmissionForm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSubmissionForm")
            .field("field_count", &self.fields.len())
            .field("fields", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingSubmissionForm {
    fn drop(&mut self) {
        for (name, value) in &mut self.fields {
            name.zeroize();
            value.zeroize();
        }
        self.fields.zeroize();
    }
}

/// Final route class observed during a read-only Work verification fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingWorkVerificationRoute {
    Editor,
    Prompt,
    View,
}

/// Redacted owner for one fresh post-submit Work page.
pub struct ChaoxingWorkVerificationDocument {
    route: ChaoxingWorkVerificationRoute,
    document: String,
}

impl ChaoxingWorkVerificationDocument {
    /// Owns one bounded verification page and its allowlisted final route.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized pages.
    pub fn try_new(route: ChaoxingWorkVerificationRoute, document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_response(
                "Chaoxing Work verification page is empty or exceeds the size limit",
            ));
        }
        Ok(Self { route, document })
    }

    pub(crate) const fn route(&self) -> ChaoxingWorkVerificationRoute {
        self.route
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.document
    }
}

impl fmt::Debug for ChaoxingWorkVerificationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingWorkVerificationDocument")
            .field("route", &self.route)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ChaoxingWorkVerificationDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Zeroizing owner for one fresh Chapter Work result page obtained through the
/// audited `selectWorkQuestionYiPiYue` iframe route.
pub struct ChaoxingChapterWorkVerificationDocument {
    document: String,
}

impl ChaoxingChapterWorkVerificationDocument {
    /// Owns one bounded Chapter Work result document.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized pages.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_response(
                "Chaoxing Chapter Work verification page is empty or exceeds the size limit",
            ));
        }
        Ok(Self { document })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.document
    }
}

impl fmt::Debug for ChaoxingChapterWorkVerificationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChaoxingChapterWorkVerificationDocument([REDACTED])")
    }
}

impl Drop for ChaoxingChapterWorkVerificationDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Provider-local comparison between one submitted answer and the official
/// answer shown on the same bound Chapter Work result page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingChapterWorkAnswerJudgement {
    MatchesOfficial,
    DiffersFromOfficial,
}

/// The exact donor retake entry observed on a completed Chapter Work result.
/// This is a read-only fact and does not authorize or execute the mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaoxingChapterWorkRetakeEntry {
    RedoTest,
}

/// Bound answer facts for one supported Question on a completed Chapter Work
/// result. Answer values are deliberately omitted from `Debug` output.
#[derive(Clone, PartialEq)]
pub struct ChaoxingChapterWorkQuestionEvidence {
    question_id: QuestionId,
    submitted_answer: NormalizedAnswer,
    official_answer: NormalizedAnswer,
    judgement: ChaoxingChapterWorkAnswerJudgement,
}

impl ChaoxingChapterWorkQuestionEvidence {
    pub const fn question_id(&self) -> QuestionId {
        self.question_id
    }

    pub const fn submitted_answer(&self) -> &NormalizedAnswer {
        &self.submitted_answer
    }

    pub const fn official_answer(&self) -> &NormalizedAnswer {
        &self.official_answer
    }

    pub const fn judgement(&self) -> ChaoxingChapterWorkAnswerJudgement {
        self.judgement
    }

    pub const fn submitted_answer_label(&self) -> &'static str {
        "我的答案"
    }

    pub const fn official_answer_label(&self) -> &'static str {
        "正确答案"
    }
}

impl fmt::Debug for ChaoxingChapterWorkQuestionEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterWorkQuestionEvidence")
            .field("question_id", &self.question_id)
            .field("submitted_answer", &"[REDACTED]")
            .field("official_answer", &"[REDACTED]")
            .field("judgement", &self.judgement)
            .finish()
    }
}

/// Complete supported answer, score and retake facts from one read-only
/// Chapter Work result page. This Provider-local value carries no owner,
/// account, Course or Attempt identity; Core must supply those bindings when a
/// shared harvest capability is introduced.
#[derive(Clone, PartialEq)]
pub struct ChaoxingChapterWorkResultEvidence {
    score: Option<SubmissionScore>,
    retake_entry: Option<ChaoxingChapterWorkRetakeEntry>,
    questions: Vec<ChaoxingChapterWorkQuestionEvidence>,
}

impl ChaoxingChapterWorkResultEvidence {
    pub const fn score(&self) -> Option<SubmissionScore> {
        self.score
    }

    pub const fn retake_entry(&self) -> Option<ChaoxingChapterWorkRetakeEntry> {
        self.retake_entry
    }

    pub fn questions(&self) -> &[ChaoxingChapterWorkQuestionEvidence] {
        &self.questions
    }
}

impl fmt::Debug for ChaoxingChapterWorkResultEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterWorkResultEvidence")
            .field("score", &self.score)
            .field("retake_entry", &self.retake_entry)
            .field("question_count", &self.questions.len())
            .finish()
    }
}

/// Zeroizing owner for one strictly route-bound fresh Exam result page.
pub struct ChaoxingExamVerificationDocument {
    document: String,
}

impl ChaoxingExamVerificationDocument {
    /// Owns one bounded Exam result document.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized pages.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_response(
                "Chaoxing Exam verification page is empty or exceeds the size limit",
            ));
        }
        Ok(Self { document })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.document
    }
}

impl fmt::Debug for ChaoxingExamVerificationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChaoxingExamVerificationDocument([REDACTED])")
    }
}

impl Drop for ChaoxingExamVerificationDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Parses an acknowledgement without treating it as completion.
///
/// # Errors
///
/// Returns typed errors for malformed JSON, explicit rejection, authentication,
/// challenge, expiry, or other protocol drift.
pub fn parse_submission_receipt(document: &str) -> ProviderResult<SubmissionReceipt> {
    if document.is_empty() || document.len() > MAX_RESPONSE_BYTES {
        return Err(invalid_response(
            "Chaoxing submission response is empty or exceeds the size limit",
        ));
    }
    let response: SubmissionResponse = serde_json::from_str(document)
        .map_err(|_| protocol_drift("Chaoxing submission response is not valid JSON"))?;
    if !response.status {
        return Err(classify_rejection(response.message.as_deref()));
    }
    let receipt = SubmissionReceipt {
        remote_status: "accepted".to_owned(),
        message_sanitized: Some(
            "Chaoxing accepted the Work submission for independent verification".to_owned(),
        ),
        provider_trace_id: None,
        received_at: Utc::now(),
    };
    receipt
        .validate()
        .map_err(|_| invalid_response("Chaoxing submission receipt is invalid"))?;
    Ok(receipt)
}

/// Compares one fresh Work page with the immutable answer plan.
///
/// # Errors
///
/// Returns typed errors for login pages, malformed result DOM, duplicate
/// Questions, unsupported answer grammar, or invalid snapshots.
pub fn parse_verification_snapshot(
    document: &ChaoxingWorkVerificationDocument,
    plan: &ChaoxingSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    if plan.len() != draft.items.len() {
        return Err(invalid_response(
            "Chaoxing verification plan does not match its immutable draft",
        ));
    }
    let html = Html::parse_document(document.as_str());
    reject_login_or_challenge(&html)?;
    match document.route() {
        ChaoxingWorkVerificationRoute::Editor => pending_snapshot(draft, RemoteState::Pending),
        ChaoxingWorkVerificationRoute::Prompt => {
            let text = normalized_text(html.root_element().text());
            if !contains_any(&text, &["提交成功", "等待教师批阅", "待批阅", "已提交"])
            {
                return Err(protocol_drift(
                    "Chaoxing Work prompt has no submitted-state evidence",
                ));
            }
            pending_snapshot(draft, RemoteState::Completed)
        }
        ChaoxingWorkVerificationRoute::View => parse_view_snapshot(&html, plan, draft),
    }
}

/// Compares a fresh `selectWorkQuestionYiPiYue` result page with a Chapter
/// Work Draft. Only the donor-observed single-choice, multiple-choice and
/// true/false visible-answer grammar is eligible for exact confirmation.
///
/// # Errors
///
/// Returns typed errors for login pages, duplicate identities, malformed
/// scores or invalid snapshots. Incomplete, reordered or unsupported result
/// facts produce an Inconclusive snapshot.
pub fn parse_chapter_work_verification_snapshot(
    document: &ChaoxingChapterWorkVerificationDocument,
    plan: &ChaoxingSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    if plan.len() != draft.items.len() {
        return Err(invalid_response(
            "Chaoxing Chapter Work verification plan does not match its immutable Draft",
        ));
    }
    let html = Html::parse_document(document.as_str());
    reject_login_or_challenge(&html)?;
    let score = parse_chapter_result_score(&html)?.map(|earned_milli_points| SubmissionScore {
        earned_milli_points,
        possible_milli_points: 100_000,
    });
    let Some(remote_answers) = parse_chapter_result_answers(&html, "我的答案")? else {
        return chapter_result_inconclusive_snapshot(draft, score);
    };
    if remote_answers.len() != plan.len()
        || draft.items.iter().enumerate().any(|(index, item)| {
            item.question.position != u32::try_from(index + 1).unwrap_or(u32::MAX)
        })
    {
        return chapter_result_inconclusive_snapshot(draft, score);
    }
    let planned = plan.answers().collect::<Vec<_>>();
    if remote_answers
        .iter()
        .zip(&planned)
        .any(|(actual, (remote_id, type_code, _))| {
            actual.remote_id != *remote_id || actual.type_code != *type_code
        })
    {
        return chapter_result_inconclusive_snapshot(draft, score);
    }
    let questions = remote_answers
        .iter()
        .zip(planned)
        .zip(&draft.items)
        .map(
            |((actual, (_, _, expected)), item)| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: if actual.value == expected {
                    SubmissionQuestionVerificationStatus::Confirmed
                } else {
                    SubmissionQuestionVerificationStatus::Rejected
                },
            },
        )
        .collect::<Vec<_>>();
    let status = if questions
        .iter()
        .all(|question| question.status == SubmissionQuestionVerificationStatus::Confirmed)
    {
        SubmissionVerificationStatus::Confirmed
    } else {
        SubmissionVerificationStatus::Rejected
    };
    validate_snapshot(SubmissionVerificationSnapshot {
        status,
        remote_state: Some(RemoteState::Completed),
        score,
        progress_percent: Some(100),
        questions,
        verified_at: Utc::now(),
    })
}

/// Resolves exact Provider-native standard answers from a fresh Chapter Work
/// result page. This parser does not navigate, start a retake or mutate state.
///
/// # Errors
///
/// Returns typed errors when the result omits a complete supported standard
/// answer set or when Question identity, order, type or option bindings drift.
pub fn parse_chapter_work_answer_candidates(
    document: &ChaoxingChapterWorkVerificationDocument,
    questions: &[Question],
) -> ProviderResult<Vec<AnswerCandidate>> {
    if questions.is_empty() {
        return Err(invalid_response(
            "Chaoxing Chapter Work answer resolution requires Questions",
        ));
    }
    let html = Html::parse_document(document.as_str());
    reject_login_or_challenge(&html)?;
    let Some(remote_answers) = parse_chapter_result_answers(&html, "正确答案")? else {
        return Err(unsupported(
            "Chaoxing Chapter Work result has no complete supported standard answers",
        ));
    };
    if remote_answers.len() != questions.len()
        || questions.iter().enumerate().any(|(index, question)| {
            question.position != u32::try_from(index + 1).unwrap_or(u32::MAX)
                || question.validate().is_err()
        })
    {
        return Err(remote_changed(
            "Chaoxing Chapter Work standard-answer Question set changed",
        ));
    }
    remote_answers
        .iter()
        .zip(questions)
        .map(|(actual, question)| {
            let remote_id = question
                .remote_question_id
                .as_deref()
                .filter(|value| valid_question_id(value))
                .ok_or_else(|| {
                    invalid_response(
                        "Chaoxing Chapter Work answer-resolution Question identity is invalid",
                    )
                })?;
            let type_code = provider_type_code(question.kind, &question.metadata_sanitized)?;
            if actual.remote_id != remote_id || actual.type_code != type_code {
                return Err(remote_changed(
                    "Chaoxing Chapter Work standard-answer binding changed",
                ));
            }
            let answer = chapter_standard_answer(question, actual)?;
            let candidate = AnswerCandidate {
                question_id: question.id,
                source: AnswerSource::ProviderNative,
                answer,
                confidence: Some(
                    AnswerConfidence::try_new(AnswerConfidence::MAX_BASIS_POINTS).map_err(
                        |_| {
                            invalid_response(
                                "Chaoxing Chapter Work standard-answer confidence is invalid",
                            )
                        },
                    )?,
                ),
                explanation: None,
                provenance_sanitized: json!({
                    "schema": "chaoxing.chapter-work-result-answer.v1",
                    "remote_question_id": remote_id,
                    "result_route": "selectWorkQuestionYiPiYue",
                }),
            };
            candidate.validate().map_err(|_| {
                invalid_response("Chaoxing Chapter Work standard-answer candidate is invalid")
            })?;
            Ok(candidate)
        })
        .collect()
}

/// Parses complete supported historical answer facts from one already
/// completed Chapter Work result without navigating to or invoking `redoTest`.
/// Submitted and official answers remain distinct even when they match.
///
/// # Errors
///
/// Returns typed errors for login/challenge pages, incomplete answer labels,
/// unsupported result types, invalid option bindings or Question-set drift.
pub fn parse_chapter_work_result_evidence(
    document: &ChaoxingChapterWorkVerificationDocument,
    questions: &[Question],
) -> ProviderResult<ChaoxingChapterWorkResultEvidence> {
    if questions.is_empty() {
        return Err(invalid_response(
            "Chaoxing Chapter Work result evidence requires Questions",
        ));
    }
    let html = Html::parse_document(document.as_str());
    reject_login_or_challenge(&html)?;
    let Some(submitted) = parse_chapter_result_answers(&html, "我的答案")? else {
        return Err(unsupported(
            "Chaoxing Chapter Work result has no complete supported submitted answers",
        ));
    };
    let Some(official) = parse_chapter_result_answers(&html, "正确答案")? else {
        return Err(unsupported(
            "Chaoxing Chapter Work result has no complete supported official answers",
        ));
    };
    if submitted.len() != questions.len()
        || official.len() != questions.len()
        || questions.iter().enumerate().any(|(index, question)| {
            question.position != u32::try_from(index + 1).unwrap_or(u32::MAX)
                || question.validate().is_err()
        })
    {
        return Err(remote_changed(
            "Chaoxing Chapter Work result-evidence Question set changed",
        ));
    }
    let questions = submitted
        .iter()
        .zip(&official)
        .zip(questions)
        .map(|((submitted, official), question)| {
            let remote_id = question
                .remote_question_id
                .as_deref()
                .filter(|value| valid_question_id(value))
                .ok_or_else(|| {
                    invalid_response(
                        "Chaoxing Chapter Work result-evidence Question identity is invalid",
                    )
                })?;
            let type_code = provider_type_code(question.kind, &question.metadata_sanitized)?;
            if submitted.remote_id != remote_id
                || official.remote_id != remote_id
                || submitted.type_code != type_code
                || official.type_code != type_code
            {
                return Err(remote_changed(
                    "Chaoxing Chapter Work result-evidence binding changed",
                ));
            }
            let submitted_answer = chapter_standard_answer(question, submitted)?;
            let official_answer = chapter_standard_answer(question, official)?;
            let judgement = if submitted_answer == official_answer {
                ChaoxingChapterWorkAnswerJudgement::MatchesOfficial
            } else {
                ChaoxingChapterWorkAnswerJudgement::DiffersFromOfficial
            };
            Ok(ChaoxingChapterWorkQuestionEvidence {
                question_id: question.id,
                submitted_answer,
                official_answer,
                judgement,
            })
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    let score = parse_chapter_result_score(&html)?.map(|earned_milli_points| SubmissionScore {
        earned_milli_points,
        possible_milli_points: 100_000,
    });
    let retake_entry = html
        .select(&selector("[onclick]"))
        .any(|node| {
            node.value()
                .attr("onclick")
                .is_some_and(|value| contains_javascript_call(value, "redotest"))
        })
        .then_some(ChaoxingChapterWorkRetakeEntry::RedoTest);
    Ok(ChaoxingChapterWorkResultEvidence {
        score,
        retake_entry,
        questions,
    })
}

/// Compares a fresh Exam result with every immutable Draft Question in exact
/// DOM order. Score and list completion are never answer evidence.
///
/// # Errors
///
/// Returns typed errors for duplicate or malformed result identities. Missing,
/// extra, unsupported or drifted Question evidence yields an Inconclusive
/// snapshot instead of inferring answer consistency.
pub fn parse_exam_verification_snapshot(
    document: &ChaoxingExamVerificationDocument,
    plan: &ChaoxingSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    if plan.len() != draft.items.len() {
        return Err(invalid_response(
            "Chaoxing Exam verification plan does not match its immutable Draft",
        ));
    }
    let html = Html::parse_document(document.as_str());
    reject_login_or_challenge(&html)?;
    let score = parse_exam_detail_facts(document.as_str())?
        .score_milli_points()
        .map(|earned_milli_points| SubmissionScore {
            earned_milli_points,
            possible_milli_points: 100_000,
        });
    let Some(remote_answers) = parse_exam_result_answers(&html)? else {
        return exam_inconclusive_snapshot(draft, score);
    };
    if remote_answers.len() != plan.len()
        || draft.items.iter().enumerate().any(|(index, item)| {
            item.question.position != u32::try_from(index + 1).unwrap_or(u32::MAX)
        })
    {
        return exam_inconclusive_snapshot(draft, score);
    }
    let planned = plan.answers().collect::<Vec<_>>();
    if remote_answers
        .iter()
        .zip(&planned)
        .any(|(actual, (remote_id, type_code, _))| {
            actual.remote_id != *remote_id || actual.type_code != *type_code
        })
    {
        return exam_inconclusive_snapshot(draft, score);
    }
    let questions = remote_answers
        .iter()
        .zip(planned)
        .zip(&draft.items)
        .map(
            |((actual, (_, _, expected)), item)| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: if actual.value == expected {
                    SubmissionQuestionVerificationStatus::Confirmed
                } else {
                    SubmissionQuestionVerificationStatus::Rejected
                },
            },
        )
        .collect::<Vec<_>>();
    let status = if questions
        .iter()
        .all(|question| question.status == SubmissionQuestionVerificationStatus::Confirmed)
    {
        SubmissionVerificationStatus::Confirmed
    } else {
        SubmissionVerificationStatus::Rejected
    };
    validate_snapshot(SubmissionVerificationSnapshot {
        status,
        remote_state: Some(RemoteState::Completed),
        score,
        progress_percent: Some(100),
        questions,
        verified_at: Utc::now(),
    })
}

fn exam_inconclusive_snapshot(
    draft: &SubmissionDraft,
    score: Option<SubmissionScore>,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let mut snapshot = inconclusive_snapshot(draft, RemoteState::Completed)?;
    snapshot.score = score;
    validate_snapshot(snapshot)
}

fn chapter_result_inconclusive_snapshot(
    draft: &SubmissionDraft,
    score: Option<SubmissionScore>,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let mut snapshot = inconclusive_snapshot(draft, RemoteState::Completed)?;
    snapshot.score = score;
    validate_snapshot(snapshot)
}

struct ChapterRemoteAnswer {
    remote_id: String,
    type_code: String,
    value: String,
}

impl Drop for ChapterRemoteAnswer {
    fn drop(&mut self) {
        self.remote_id.zeroize();
        self.type_code.zeroize();
        self.value.zeroize();
    }
}

fn parse_chapter_result_answers(
    html: &Html,
    answer_label: &'static str,
) -> ProviderResult<Option<Vec<ChapterRemoteAnswer>>> {
    let mut answers = Vec::new();
    let mut remote_ids = BTreeSet::new();
    for question in html.select(&selector("div.singleQuesId")) {
        let remote_id = question
            .value()
            .attr("data")
            .map(str::trim)
            .filter(|value| valid_question_id(value))
            .ok_or_else(|| {
                protocol_drift("Chaoxing Chapter Work result Question identity is invalid")
            })?;
        if !remote_ids.insert(remote_id.to_owned()) {
            return Err(protocol_drift(
                "Chaoxing Chapter Work result contains duplicate Question identity",
            ));
        }
        let text = Zeroizing::new(normalized_text(question.text()));
        let Some(type_code @ ("0" | "1" | "3")) = chapter_result_type(text.as_str()) else {
            return Ok(None);
        };
        if !text.contains(answer_label) {
            return Ok(None);
        }
        let value = parse_labeled_visible_answer(text.as_str(), answer_label, type_code)?;
        answers.push(ChapterRemoteAnswer {
            remote_id: remote_id.to_owned(),
            type_code: type_code.to_owned(),
            value,
        });
    }
    if answers.is_empty() {
        Ok(None)
    } else {
        Ok(Some(answers))
    }
}

fn chapter_standard_answer(
    question: &Question,
    actual: &ChapterRemoteAnswer,
) -> ProviderResult<NormalizedAnswer> {
    match actual.type_code.as_str() {
        "0" | "1" => {
            let selections = actual
                .value
                .chars()
                .map(|value| value.to_string())
                .collect::<Vec<_>>();
            if (actual.type_code == "0" && selections.len() != 1)
                || !valid_selections(question, &selections)
            {
                return Err(remote_changed(
                    "Chaoxing Chapter Work standard answer no longer matches its options",
                ));
            }
            Ok(NormalizedAnswer::Selections(selections))
        }
        "3" => match actual.value.as_str() {
            "true" => Ok(NormalizedAnswer::Boolean(true)),
            "false" => Ok(NormalizedAnswer::Boolean(false)),
            _ => Err(protocol_drift(
                "Chaoxing Chapter Work standard true/false answer is invalid",
            )),
        },
        _ => Err(unsupported(
            "Chaoxing Chapter Work standard-answer type is unsupported",
        )),
    }
}

fn chapter_result_type(text: &str) -> Option<&'static str> {
    let mut matches = [
        ("【单选题】", "0"),
        ("【多选题】", "1"),
        ("【填空题】", "2"),
        ("【判断题】", "3"),
        ("【简答题】", "4"),
    ]
    .into_iter()
    .filter_map(|(label, code)| text.contains(label).then_some(code));
    let result = matches.next();
    if matches.next().is_some() {
        None
    } else {
        result
    }
}

fn parse_chapter_result_score(html: &Html) -> ProviderResult<Option<u64>> {
    let text = Zeroizing::new(normalized_text(html.root_element().text()));
    let mut candidates = Vec::new();
    for (start, _) in text.match_indices("最终成绩") {
        let value = text[start + "最终成绩".len()..].trim_start();
        let number_end = value
            .bytes()
            .position(|byte| !byte.is_ascii_digit() && byte != b'.')
            .unwrap_or(value.len());
        let number = &value[..number_end];
        if number.is_empty() || !value[number_end..].trim_start().starts_with('分') {
            return Err(protocol_drift(
                "Chaoxing Chapter Work result contains an invalid final score",
            ));
        }
        candidates.push(parse_chapter_score_milli(number)?);
    }
    let Some(score) = candidates.first().copied() else {
        return Ok(None);
    };
    if candidates.iter().any(|candidate| *candidate != score) {
        return Err(protocol_drift(
            "Chaoxing Chapter Work result contains conflicting final scores",
        ));
    }
    Ok(Some(u64::from(score)))
}

fn parse_chapter_score_milli(value: &str) -> ProviderResult<u32> {
    let (whole, fraction) = value.split_once('.').map_or((value, ""), |parts| parts);
    if whole.is_empty()
        || whole.len() > 3
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 3
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(protocol_drift(
            "Chaoxing Chapter Work result contains an invalid final score",
        ));
    }
    let whole = whole.parse::<u32>().map_err(|_| {
        protocol_drift("Chaoxing Chapter Work result contains an invalid final score")
    })?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u32>().map_err(|_| {
            protocol_drift("Chaoxing Chapter Work result contains an invalid final score")
        })? * 10_u32.pow(u32::try_from(3 - fraction.len()).expect("bounded score precision"))
    };
    whole
        .checked_mul(1_000)
        .and_then(|whole| whole.checked_add(fraction))
        .filter(|score| *score <= 100_000)
        .ok_or_else(|| protocol_drift("Chaoxing Chapter Work result final score is out of range"))
}

struct ExamRemoteAnswer {
    remote_id: String,
    type_code: String,
    value: String,
}

impl Drop for ExamRemoteAnswer {
    fn drop(&mut self) {
        self.remote_id.zeroize();
        self.type_code.zeroize();
        self.value.zeroize();
    }
}

fn parse_exam_result_answers(html: &Html) -> ProviderResult<Option<Vec<ExamRemoteAnswer>>> {
    let mut answers = Vec::new();
    let mut remote_ids = BTreeSet::new();
    for question in html.select(&selector(".questionLi")) {
        let Some(identity) = unique_result_node(
            question.select(&selector("input[name='questionId']")),
            "Chaoxing Exam result contains duplicate Question identity fields",
        )?
        else {
            return Ok(None);
        };
        let remote_id = identity
            .value()
            .attr("value")
            .map(str::trim)
            .filter(|value| valid_question_id(value))
            .ok_or_else(|| protocol_drift("Chaoxing Exam result Question identity is invalid"))?;
        if !remote_ids.insert(remote_id.to_owned()) {
            return Err(protocol_drift(
                "Chaoxing Exam result contains duplicate Question identity",
            ));
        }
        let type_name = format!("type{remote_id}");
        let Some(type_input) = unique_result_node(
            question
                .select(&selector("input[name]"))
                .filter(|input| input.value().attr("name") == Some(type_name.as_str())),
            "Chaoxing Exam result contains duplicate Question type fields",
        )?
        else {
            return Ok(None);
        };
        let Some(type_code @ ("0" | "1" | "3")) = type_input.value().attr("value") else {
            return Ok(None);
        };
        let Some(answer_node) = unique_result_node(
            question
                .select(&selector(".mark_answer, .stem_answer, .my-answer"))
                .filter(|node| normalized_text(node.text()).contains("我的答案")),
            "Chaoxing Exam result contains duplicate visible answers",
        )?
        else {
            return Ok(None);
        };
        let text = Zeroizing::new(normalized_text(answer_node.text()));
        let Some(value) = parse_exam_visible_answer(text.as_str(), type_code)? else {
            return Ok(None);
        };
        answers.push(ExamRemoteAnswer {
            remote_id: remote_id.to_owned(),
            type_code: type_code.to_owned(),
            value,
        });
    }
    if answers.is_empty() {
        Ok(None)
    } else {
        Ok(Some(answers))
    }
}

fn unique_result_node<'a>(
    mut nodes: impl Iterator<Item = ElementRef<'a>>,
    duplicate_message: &'static str,
) -> ProviderResult<Option<ElementRef<'a>>> {
    let node = nodes.next();
    if nodes.next().is_some() {
        return Err(protocol_drift(duplicate_message));
    }
    Ok(node)
}

fn parse_exam_visible_answer(value: &str, type_code: &str) -> ProviderResult<Option<String>> {
    let Some((_, value)) = value.split_once("我的答案") else {
        return Ok(None);
    };
    let value = value.trim_start_matches([':', '：']).trim();
    let value = ["正确答案", "答案解析", "得分"]
        .into_iter()
        .filter_map(|marker| value.find(marker))
        .min()
        .map_or(value, |end| &value[..end])
        .trim();
    if value.is_empty() || value.len() > MAX_FORM_FIELD_BYTES {
        return Ok(None);
    }
    match type_code {
        "0" | "1" => {
            let mut answer = value
                .chars()
                .filter(|character| {
                    !character.is_whitespace() && !matches!(character, ',' | '，' | '、' | ';')
                })
                .collect::<Vec<_>>();
            if answer.is_empty() || answer.iter().any(|value| !value.is_ascii_uppercase()) {
                return Ok(None);
            }
            answer.sort_unstable();
            if answer.windows(2).any(|pair| pair[0] == pair[1])
                || (type_code == "0" && answer.len() != 1)
            {
                return Err(protocol_drift(
                    "Chaoxing Exam result choice answer is duplicated or ambiguous",
                ));
            }
            Ok(Some(answer.into_iter().collect()))
        }
        "3" => match value {
            "正确" | "对" | "true" | "TRUE" | "√" => Ok(Some("true".to_owned())),
            "错误" | "错" | "false" | "FALSE" | "×" => Ok(Some("false".to_owned())),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

fn parse_view_snapshot(
    html: &Html,
    plan: &ChaoxingSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let text = normalized_text(html.root_element().text());
    if !contains_any(
        &text,
        &["作业详情", "我的答案", "待批阅", "已批阅", "已提交"],
    ) {
        return Err(unknown_work_view_shape(html));
    }
    let Some(question_statuses) = parse_selected_work_result(html, plan, draft)? else {
        return inconclusive_snapshot(draft, RemoteState::Completed);
    };
    let status = if question_statuses
        .values()
        .all(|status| *status == SubmissionQuestionVerificationStatus::Confirmed)
    {
        SubmissionVerificationStatus::Confirmed
    } else {
        SubmissionVerificationStatus::Rejected
    };
    let snapshot = SubmissionVerificationSnapshot {
        status,
        remote_state: Some(RemoteState::Completed),
        score: None,
        progress_percent: Some(100),
        questions: draft
            .items
            .iter()
            .map(|item| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: item
                    .question
                    .remote_question_id
                    .as_deref()
                    .and_then(|remote_id| question_statuses.get(remote_id))
                    .copied()
                    .unwrap_or(SubmissionQuestionVerificationStatus::Unverified),
            })
            .collect(),
        verified_at: Utc::now(),
    };
    validate_snapshot(snapshot)
}

fn parse_selected_work_result(
    html: &Html,
    plan: &ChaoxingSubmissionPlan,
    draft: &SubmissionDraft,
) -> ProviderResult<Option<BTreeMap<String, SubmissionQuestionVerificationStatus>>> {
    let planned = plan
        .answers()
        .zip(&draft.items)
        .map(|((remote_id, type_code, expected), item)| {
            (remote_id, (type_code, expected, item.question.position))
        })
        .collect::<BTreeMap<_, _>>();
    let mut remote_ids = BTreeSet::new();
    let mut question_statuses = BTreeMap::new();
    let mut remote_count = 0_usize;
    for (index, node) in html.select(&selector(".questionLi")).enumerate() {
        remote_count = remote_count
            .checked_add(1)
            .ok_or_else(|| protocol_drift("Chaoxing Work result Question count is invalid"))?;
        let Some(type_input) = unique_result_node(
            node.select(&selector("input[id^='answertype']")),
            "Chaoxing Work result contains duplicate Question identity fields",
        )?
        else {
            return Ok(None);
        };
        let remote_id = type_input
            .value()
            .attr("id")
            .and_then(|value| value.strip_prefix("answertype"))
            .filter(|value| valid_question_id(value))
            .ok_or_else(|| protocol_drift("Chaoxing Work result Question identity is invalid"))?;
        let type_code = type_input
            .value()
            .attr("value")
            .filter(|value| valid_remote_type_code(value))
            .ok_or_else(|| protocol_drift("Chaoxing Work result Question type is malformed"))?;
        if !remote_ids.insert(remote_id.to_owned()) {
            return Err(protocol_drift(
                "Chaoxing Work result contains duplicate Question identity",
            ));
        }
        let Some((expected_type, expected, expected_position)) = planned.get(remote_id) else {
            continue;
        };
        if type_code != *expected_type
            || usize::try_from(*expected_position).ok() != index.checked_add(1)
        {
            return Ok(None);
        }
        let Some(answer_node) = unique_result_node(
            node.select(&selector(".mark_answer, .stem_answer, .my-answer"))
                .filter(|candidate| normalized_text(candidate.text()).contains("我的答案")),
            "Chaoxing Work result contains duplicate visible answers",
        )?
        else {
            return Ok(None);
        };
        let actual = parse_visible_answer(&normalized_text(answer_node.text()), type_code)?;
        question_statuses.insert(
            remote_id.to_owned(),
            if actual == *expected {
                SubmissionQuestionVerificationStatus::Confirmed
            } else {
                SubmissionQuestionVerificationStatus::Rejected
            },
        );
    }
    if remote_count != plan.total_question_count() || question_statuses.len() != plan.len() {
        return Ok(None);
    }
    Ok(Some(question_statuses))
}

fn unknown_work_view_shape(html: &Html) -> ProviderError {
    let shape = json!({
        "schema": "chaoxing.work-result-shape.v1",
        "question_count": html.select(&selector(".questionLi")).count(),
        "has_type_input": html
            .select(&selector("input[id^='answertype']"))
            .next()
            .is_some(),
        "has_visible_answer_node": html
            .select(&selector(".mark_answer, .stem_answer, .my-answer"))
            .next()
            .is_some(),
        "has_submission_form": html.select(&selector("form")).next().is_some(),
    });
    ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "Chaoxing Work view has no submitted-state evidence",
    )
    .try_with_protocol_observation(
        ProtocolSurface::SubmissionVerify,
        ProtocolObservationKind::UnknownResultShape,
        shape,
    )
    .unwrap_or_else(|_| protocol_drift("Chaoxing Work view has no submitted-state evidence"))
}

fn pending_snapshot(
    draft: &SubmissionDraft,
    remote_state: RemoteState,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let snapshot = SubmissionVerificationSnapshot {
        status: SubmissionVerificationStatus::Pending,
        remote_state: Some(remote_state),
        score: None,
        progress_percent: None,
        questions: unverified_questions(draft),
        verified_at: Utc::now(),
    };
    validate_snapshot(snapshot)
}

pub(crate) fn inconclusive_snapshot(
    draft: &SubmissionDraft,
    remote_state: RemoteState,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let snapshot = SubmissionVerificationSnapshot {
        status: SubmissionVerificationStatus::Inconclusive,
        remote_state: Some(remote_state),
        score: None,
        progress_percent: None,
        questions: unverified_questions(draft),
        verified_at: Utc::now(),
    };
    validate_snapshot(snapshot)
}

fn unverified_questions(draft: &SubmissionDraft) -> Vec<SubmissionQuestionVerification> {
    draft
        .items
        .iter()
        .map(|item| SubmissionQuestionVerification {
            question_id: item.question.id,
            status: SubmissionQuestionVerificationStatus::Unverified,
        })
        .collect()
}

fn validate_snapshot(
    snapshot: SubmissionVerificationSnapshot,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    snapshot
        .validate()
        .map_err(|_| invalid_response("Chaoxing submission verification snapshot is invalid"))?;
    Ok(snapshot)
}

struct RemoteQuestionType {
    remote_id: String,
    type_code: String,
}

impl Drop for RemoteQuestionType {
    fn drop(&mut self) {
        self.remote_id.zeroize();
        self.type_code.zeroize();
    }
}

fn parse_remote_question_types(form: ElementRef<'_>) -> ProviderResult<Vec<RemoteQuestionType>> {
    let mut types = Vec::new();
    let mut remote_ids = BTreeSet::new();
    for input in form.select(&selector("input[id^='answertype']")) {
        let remote_id = input
            .value()
            .attr("id")
            .and_then(|value| value.strip_prefix("answertype"))
            .filter(|value| valid_question_id(value))
            .ok_or_else(|| protocol_drift("Chaoxing Work editor Question identity is invalid"))?;
        let type_code = input
            .value()
            .attr("value")
            .filter(|value| valid_remote_type_code(value))
            .ok_or_else(|| protocol_drift("Chaoxing Work editor Question type is malformed"))?;
        if !remote_ids.insert(remote_id.to_owned()) {
            return Err(protocol_drift(
                "Chaoxing Work editor contains duplicate Question identity",
            ));
        }
        types.push(RemoteQuestionType {
            remote_id: remote_id.to_owned(),
            type_code: type_code.to_owned(),
        });
    }
    if types.is_empty() {
        return Err(protocol_drift("Chaoxing Work editor has no Questions"));
    }
    Ok(types)
}

fn valid_remote_type_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 3
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn provider_type_code(kind: QuestionKind, metadata: &serde_json::Value) -> ProviderResult<&str> {
    let type_code = metadata
        .get("provider_type_code")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| protocol_drift("Chaoxing Question has no Provider type code"))?;
    match (kind, type_code) {
        (QuestionKind::SingleChoice, 0) => Ok("0"),
        (QuestionKind::MultipleChoice, 1) => Ok("1"),
        (QuestionKind::FillBlank, 2) => Ok("2"),
        (QuestionKind::TrueFalse, 3) => Ok("3"),
        (QuestionKind::ShortAnswer, 4) => Ok("4"),
        _ => Err(unsupported(
            "Chaoxing native submission does not support this Question type code",
        )),
    }
}

fn encode_answer(
    question: &asterism_domain::Question,
    answer: &NormalizedAnswer,
) -> ProviderResult<String> {
    match (question.kind, answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
            if values.len() == 1 && valid_selections(question, values) =>
        {
            Ok(values[0].clone())
        }
        (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values))
            if valid_selections(question, values) =>
        {
            let mut values = values.clone();
            values.sort();
            values.dedup();
            if values.len() != answer_selection_count(answer) {
                return Err(invalid_response(
                    "Chaoxing multiple-choice submission contains duplicate selections",
                ));
            }
            Ok(values.concat())
        }
        (QuestionKind::TrueFalse, NormalizedAnswer::Boolean(value)) => {
            Ok(if *value { "true" } else { "false" }.to_owned())
        }
        (QuestionKind::FillBlank, NormalizedAnswer::Texts(values)) if !values.is_empty() => {
            Ok(values.concat())
        }
        (QuestionKind::ShortAnswer, NormalizedAnswer::Texts(values)) if values.len() == 1 => {
            Ok(values[0].clone())
        }
        _ => Err(unsupported(
            "Chaoxing native submission cannot encode this answer shape",
        )),
    }
}

fn answer_selection_count(answer: &NormalizedAnswer) -> usize {
    match answer {
        NormalizedAnswer::Selections(values) => values.len(),
        _ => 0,
    }
}

fn valid_selections(question: &asterism_domain::Question, values: &[String]) -> bool {
    let options = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    !values.is_empty()
        && values.iter().all(|value| {
            value.len() == 1
                && value.bytes().all(|byte| byte.is_ascii_uppercase())
                && options.contains(value.as_str())
        })
}

fn parse_visible_answer(value: &str, type_code: &str) -> ProviderResult<String> {
    parse_labeled_visible_answer(value, "我的答案", type_code)
}

fn parse_labeled_visible_answer(
    value: &str,
    answer_label: &'static str,
    type_code: &str,
) -> ProviderResult<String> {
    let (_, value) = value
        .split_once(answer_label)
        .ok_or_else(|| protocol_drift("Chaoxing Work result visible-answer label is missing"))?;
    let value = value.trim_start_matches([':', '：']).trim();
    let value = ["我的答案", "正确答案", "答案解析", "得分"]
        .into_iter()
        .filter_map(|marker| value.find(marker))
        .min()
        .map_or(value, |end| &value[..end])
        .trim();
    match type_code {
        "0" | "1" => {
            let mut answer = value
                .chars()
                .filter(char::is_ascii_uppercase)
                .collect::<Vec<_>>();
            answer.sort_unstable();
            answer.dedup();
            if answer.is_empty() {
                return Err(protocol_drift(
                    "Chaoxing Work result choice answer is empty",
                ));
            }
            Ok(answer.into_iter().collect())
        }
        "3" if contains_any(value, &["正确", "对", "true", "TRUE", "√"]) => {
            Ok("true".to_owned())
        }
        "3" if contains_any(value, &["错误", "错", "false", "FALSE", "×"]) => {
            Ok("false".to_owned())
        }
        "3" => Err(protocol_drift(
            "Chaoxing Work result true/false answer is invalid",
        )),
        _ => Err(unsupported(
            "Chaoxing Work result answer type is unsupported",
        )),
    }
}

fn require_field(
    values: &BTreeMap<String, String>,
    field: &'static str,
    expected: &str,
) -> ProviderResult<()> {
    if values.get(field).map(String::as_str) != Some(expected) {
        return Err(remote_changed(
            "Chaoxing Work submission form identity changed before mutation",
        ));
    }
    Ok(())
}

fn reject_login_or_challenge(html: &Html) -> ProviderResult<()> {
    let text = normalized_text(html.root_element().text());
    if contains_any(&text, &["登录超星", "账号登录", "手机号登录"]) {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Work page is an authentication response",
        ));
    }
    if contains_any(&text, &["验证码", "安全验证"]) {
        return Err(ProviderError::human_required(
            "Chaoxing Work operation requires an image captcha",
            asterism_domain::HumanRequiredReason::ImageCaptcha,
        ));
    }
    if contains_any(&text, &["人脸识别", "人脸验证"]) {
        return Err(ProviderError::human_required(
            "Chaoxing Work operation requires browser verification",
            asterism_domain::HumanRequiredReason::BrowserRequired,
        ));
    }
    Ok(())
}

fn classify_rejection(message: Option<&str>) -> ProviderError {
    let message = message.unwrap_or_default();
    if contains_any(message, &["验证码", "安全验证"]) {
        return ProviderError::human_required(
            "Chaoxing rejected submission pending an image captcha",
            asterism_domain::HumanRequiredReason::ImageCaptcha,
        );
    }
    if contains_any(message, &["人脸", "身份验证"]) {
        return ProviderError::human_required(
            "Chaoxing rejected submission pending browser verification",
            asterism_domain::HumanRequiredReason::BrowserRequired,
        );
    }
    if contains_any(message, &["已截止", "已过期", "作业不存在", "已删除"]) {
        return remote_changed("Chaoxing Work is no longer submittable");
    }
    if contains_any(message, &["登录", "未登录", "会话"]) {
        return ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing rejected submission because the session is invalid",
        );
    }
    invalid_response("Chaoxing rejected the Work submission mutation")
}

fn valid_component(value: Option<&str>) -> ProviderResult<&str> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_REMOTE_COMPONENT_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        .ok_or_else(|| invalid_response("Chaoxing remote Work identity is invalid"))
}

fn valid_question_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_QUESTION_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn normalized_text<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_any(value: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| value.contains(candidate))
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static Chaoxing selector must be valid")
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

#[derive(Deserialize)]
struct SubmissionResponse {
    status: bool,
    #[serde(default, alias = "msg")]
    message: Option<String>,
}

impl Drop for SubmissionResponse {
    fn drop(&mut self) {
        self.message.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderId, QuestionSnapshotId, SelectedAnswer,
        SubmissionAnswerCoverage, SubmissionDraftId, SubmissionDraftItem, TaskId,
    };

    use super::*;
    use crate::{
        ChaoxingSubmissionBuild, parse_chapter_work_question_page, parse_exam_question_page,
        parse_work_preview_question_page,
    };
    use asterism_provider_api::{ProviderContext, SubmissionBuildCapability};

    const QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-preview-mixed.html");
    const EDITOR: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/submission-editor.html");
    const PARTIAL_EDITOR: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/submission-editor-partial.html");
    const PROMPT: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/submission-prompt.html");
    const VIEW: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/submission-view.html");
    const PARTIAL_VIEW: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/submission-view-partial.html");
    const CHAPTER_QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-mobile-mixed.html");
    const CHAPTER_EDITOR: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/chapter-submission-editor.html");
    const CHAPTER_RESULT: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/chapter-result.html");
    const CHAPTER_HISTORY_RESULT: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/chapter-history-result.html");
    const EXAM_QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/exam-mobile-mixed.html");
    const EXAM_RESULT: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/detail-result.html");

    #[tokio::test]
    async fn fresh_editor_builds_allowlisted_final_submit_form() {
        let draft = draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let identity = WorkSubmissionIdentity::parse("work:100:200:work-1").unwrap();
        let form = ChaoxingSubmissionForm::parse(EDITOR, identity, &plan).unwrap();
        let fields = form.fields().iter().cloned().collect::<BTreeMap<_, _>>();

        assert_eq!(fields.get("courseId").map(String::as_str), Some("100"));
        assert_eq!(fields.get("classId").map(String::as_str), Some("200"));
        assert_eq!(fields.get("pyFlag").map(String::as_str), Some(""));
        assert_eq!(
            fields.get("answerwqbid").map(String::as_str),
            Some("work-preview-q-1,work-preview-q-2,")
        );
        assert_eq!(
            fields.get("answerwork-preview-q-1").map(String::as_str),
            Some("B")
        );
        assert_eq!(
            fields.get("answerwork-preview-q-2").map(String::as_str),
            Some("AC")
        );
        assert!(!fields.contains_key("ignoredFutureField"));
        assert!(!format!("{form:?}").contains("SAFE_EPHEMERAL_TOKEN"));
    }

    #[tokio::test]
    async fn partial_editor_preserves_full_question_partition_without_stale_answers() {
        let draft = partial_draft().await;
        draft.validate().unwrap();
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        assert!(plan.is_partial());
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.total_question_count(), 4);
        let identity = WorkSubmissionIdentity::parse("work:100:200:work-1").unwrap();
        let form = ChaoxingSubmissionForm::parse(PARTIAL_EDITOR, identity, &plan).unwrap();
        let fields = form.fields().iter().cloned().collect::<BTreeMap<_, _>>();

        assert_eq!(
            fields.get("answerwqbid").map(String::as_str),
            Some("work-preview-q-1,work-preview-q-2,work-preview-q-3,work-preview-q-4,")
        );
        assert_eq!(
            fields.get("answerwork-preview-q-1").map(String::as_str),
            Some("B")
        );
        assert_eq!(
            fields.get("answerwork-preview-q-2").map(String::as_str),
            Some("AC")
        );
        for remote_id in ["work-preview-q-3", "work-preview-q-4"] {
            assert_eq!(
                fields
                    .get(&format!("answer{remote_id}"))
                    .map(String::as_str),
                Some("")
            );
        }
        assert_eq!(
            fields.get("answertypework-preview-q-4").map(String::as_str),
            Some("11")
        );
        assert!(!format!("{form:?}").contains("SAFE_PARTIAL_EPHEMERAL_TOKEN"));

        let missing = PARTIAL_EDITOR.replace(
            r#"<div class="questionLi">
        <input id="answertypework-preview-q-4" value="11">
        <input name="answerwork-preview-q-4" value="stale-answer">
      </div>"#,
            "",
        );
        assert_eq!(
            ChaoxingSubmissionForm::parse(&missing, identity, &plan)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn chapter_editor_builds_all_donor_answer_shapes_without_stale_values() {
        let draft = chapter_draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let identity = WorkSubmissionIdentity::parse("resource:100:200:4001:job-work").unwrap();
        let form = ChaoxingSubmissionForm::parse(CHAPTER_EDITOR, identity, &plan).unwrap();
        let fields = form.fields().iter().cloned().collect::<BTreeMap<_, _>>();
        assert_eq!(fields.get("knowledgeid").map(String::as_str), Some("4001"));
        assert_eq!(
            fields.get("answerwqbid").map(String::as_str),
            Some("work-q-1,work-q-2,work-q-3,work-q-4,")
        );
        for (key, expected) in [
            ("answerwork-q-1", "B"),
            ("answerwork-q-2", "AC"),
            ("answerwork-q-3", "bounded answer"),
            ("answerwork-q-4", "true"),
        ] {
            assert_eq!(fields.get(key).map(String::as_str), Some(expected), "{key}");
        }
        assert!(!fields.contains_key("ignoredFutureField"));
        assert!(!format!("{form:?}").contains("SAFE_CHAPTER_EPHEMERAL_TOKEN"));
    }

    #[tokio::test]
    async fn partial_chapter_editor_keeps_unanswered_question_empty() {
        let draft = partial_chapter_draft().await;
        draft.validate().unwrap();
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let identity = WorkSubmissionIdentity::parse("resource:100:200:4001:job-work").unwrap();
        let form = ChaoxingSubmissionForm::parse(CHAPTER_EDITOR, identity, &plan).unwrap();
        let fields = form.fields().iter().cloned().collect::<BTreeMap<_, _>>();
        assert_eq!(
            fields.get("answerwqbid").map(String::as_str),
            Some("work-q-1,work-q-2,work-q-3,work-q-4,")
        );
        assert_eq!(fields.get("answerwork-q-3").map(String::as_str), Some(""));
        assert_eq!(
            fields.get("answertypework-q-3").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            fields.get("answerwork-q-4").map(String::as_str),
            Some("true")
        );
    }

    #[tokio::test]
    async fn submitted_view_confirms_exact_server_visible_answers_only() {
        let draft = draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let view = ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::View,
            VIEW.to_owned(),
        )
        .unwrap();
        let snapshot = parse_verification_snapshot(&view, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.remote_state, Some(RemoteState::Completed));
        assert!(snapshot.questions.iter().all(|question| {
            question.status == SubmissionQuestionVerificationStatus::Confirmed
        }));

        let changed = VIEW.replace("我的答案：AC", "我的答案：AD");
        let changed =
            ChaoxingWorkVerificationDocument::try_new(ChaoxingWorkVerificationRoute::View, changed)
                .unwrap();
        let snapshot = parse_verification_snapshot(&changed, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Rejected);
    }

    #[tokio::test]
    async fn partial_result_confirms_only_selected_answers_against_full_count() {
        let draft = partial_draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let view = ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::View,
            PARTIAL_VIEW.to_owned(),
        )
        .unwrap();
        let snapshot = parse_verification_snapshot(&view, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.questions.len(), 2);
        assert!(snapshot.questions.iter().all(|question| {
            question.status == SubmissionQuestionVerificationStatus::Confirmed
        }));

        let changed = PARTIAL_VIEW.replace("我的答案：AC", "我的答案：AD");
        let changed =
            ChaoxingWorkVerificationDocument::try_new(ChaoxingWorkVerificationRoute::View, changed)
                .unwrap();
        let snapshot = parse_verification_snapshot(&changed, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Rejected);
        assert_eq!(
            snapshot
                .questions
                .iter()
                .map(|question| question.status)
                .collect::<Vec<_>>(),
            [
                SubmissionQuestionVerificationStatus::Confirmed,
                SubmissionQuestionVerificationStatus::Rejected,
            ]
        );

        let missing = PARTIAL_VIEW.replace(
            r#"<div class="questionLi">
      <input id="answertypework-preview-q-4" value="11">
    </div>"#,
            "",
        );
        let missing =
            ChaoxingWorkVerificationDocument::try_new(ChaoxingWorkVerificationRoute::View, missing)
                .unwrap();
        let snapshot = parse_verification_snapshot(&missing, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);

        let reordered = PARTIAL_VIEW
            .replace("work-preview-q-1", "work-preview-q-swap")
            .replace("work-preview-q-2", "work-preview-q-1")
            .replace("work-preview-q-swap", "work-preview-q-2");
        let reordered = ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::View,
            reordered,
        )
        .unwrap();
        let snapshot = parse_verification_snapshot(&reordered, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);
    }

    #[tokio::test]
    async fn prompt_and_editor_never_claim_verified_completion() {
        let draft = draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let prompt = ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::Prompt,
            PROMPT.to_owned(),
        )
        .unwrap();
        let pending = parse_verification_snapshot(&prompt, &plan, &draft).unwrap();
        assert_eq!(pending.status, SubmissionVerificationStatus::Pending);
        assert_eq!(pending.remote_state, Some(RemoteState::Completed));

        let editor = ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::Editor,
            EDITOR.to_owned(),
        )
        .unwrap();
        let pending = parse_verification_snapshot(&editor, &plan, &draft).unwrap();
        assert_eq!(pending.status, SubmissionVerificationStatus::Pending);
        assert_eq!(pending.remote_state, Some(RemoteState::Pending));
    }

    #[tokio::test]
    async fn unknown_work_view_shape_is_observed_without_page_content() {
        let draft = draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let view = ChaoxingWorkVerificationDocument::try_new(
            ChaoxingWorkVerificationRoute::View,
            "<html><body><form><div class='questionLi'><input id='answertypePRIVATE_ID' value='PRIVATE_TYPE'><div class='mark_answer'>PRIVATE_RESULT_TEXT</div></div></form></body></html>".to_owned(),
        )
        .unwrap();
        let error = parse_verification_snapshot(&view, &plan, &draft).unwrap_err();
        let observation = error.protocol_observation.unwrap();
        assert_eq!(observation.surface, ProtocolSurface::SubmissionVerify);
        assert_eq!(
            observation.kind,
            ProtocolObservationKind::UnknownResultShape
        );
        assert_eq!(
            observation.shape_sanitized,
            json!({
                "schema": "chaoxing.work-result-shape.v1",
                "question_count": 1,
                "has_type_input": true,
                "has_visible_answer_node": true,
                "has_submission_form": true,
            })
        );
        let encoded = serde_json::to_string(&observation).unwrap();
        assert!(!encoded.contains("PRIVATE_ID"));
        assert!(!encoded.contains("PRIVATE_TYPE"));
        assert!(!encoded.contains("PRIVATE_RESULT_TEXT"));
    }

    #[tokio::test]
    async fn chapter_result_projects_exact_answers_and_independent_score() {
        let draft = chapter_result_draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let result =
            ChaoxingChapterWorkVerificationDocument::try_new(CHAPTER_RESULT.to_owned()).unwrap();
        let snapshot = parse_chapter_work_verification_snapshot(&result, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.remote_state, Some(RemoteState::Completed));
        assert_eq!(snapshot.score.unwrap().earned_milli_points, 82_500);
        assert!(snapshot.questions.iter().all(|question| {
            question.status == SubmissionQuestionVerificationStatus::Confirmed
        }));

        let changed = CHAPTER_RESULT.replacen("我的答案：正确", "我的答案：错误", 1);
        let changed = ChaoxingChapterWorkVerificationDocument::try_new(changed).unwrap();
        let snapshot = parse_chapter_work_verification_snapshot(&changed, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Rejected);
        assert_eq!(
            snapshot
                .questions
                .iter()
                .map(|question| question.status)
                .collect::<Vec<_>>(),
            [
                SubmissionQuestionVerificationStatus::Confirmed,
                SubmissionQuestionVerificationStatus::Confirmed,
                SubmissionQuestionVerificationStatus::Rejected,
            ]
        );
        assert_eq!(snapshot.score.unwrap().earned_milli_points, 82_500);

        for (replacement, expected) in [("0", 0), ("99.999", 99_999)] {
            let result = ChaoxingChapterWorkVerificationDocument::try_new(CHAPTER_RESULT.replacen(
                "82.5",
                replacement,
                1,
            ))
            .unwrap();
            let snapshot =
                parse_chapter_work_verification_snapshot(&result, &plan, &draft).unwrap();
            assert_eq!(snapshot.score.unwrap().earned_milli_points, expected);
        }

        let no_score =
            CHAPTER_RESULT.replacen(r#"<p class="final-score">最终成绩 82.5 分</p>"#, "", 1);
        let no_score = ChaoxingChapterWorkVerificationDocument::try_new(no_score).unwrap();
        let snapshot = parse_chapter_work_verification_snapshot(&no_score, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.score, None);
    }

    #[tokio::test]
    async fn chapter_result_drift_is_inconclusive_and_duplicates_fail_closed() {
        let draft = chapter_result_draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let extra = CHAPTER_RESULT.replace(
            "</main>",
            r#"<div class="singleQuesId" data="work-q-extra"><span>【单选题】</span><p>我的答案：A</p></div></main>"#,
        );
        let reordered = CHAPTER_RESULT
            .replace("work-q-1", "work-q-swap")
            .replace("work-q-2", "work-q-1")
            .replace("work-q-swap", "work-q-2");
        for document in [
            CHAPTER_RESULT.replacen("【单选题】", "【填空题】", 1),
            CHAPTER_RESULT.replacen("我的答案：B", "已作答", 1),
            CHAPTER_RESULT.replacen("work-q-1", "work-q-x", 1),
            extra,
            reordered,
        ] {
            let result = ChaoxingChapterWorkVerificationDocument::try_new(document).unwrap();
            let snapshot =
                parse_chapter_work_verification_snapshot(&result, &plan, &draft).unwrap();
            assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);
            assert_eq!(snapshot.score.unwrap().earned_milli_points, 82_500);
            assert!(snapshot.questions.iter().all(|question| {
                question.status == SubmissionQuestionVerificationStatus::Unverified
            }));
        }

        let duplicate = CHAPTER_RESULT.replace("work-q-2", "work-q-1");
        let duplicate = ChaoxingChapterWorkVerificationDocument::try_new(duplicate).unwrap();
        assert!(parse_chapter_work_verification_snapshot(&duplicate, &plan, &draft).is_err());

        for document in [
            CHAPTER_RESULT.replacen("82.5", "82.5000", 1),
            CHAPTER_RESULT.replacen("82.5", "100.001", 1),
            CHAPTER_RESULT.replace(
                r#"<p class="final-score">最终成绩 82.5 分</p>"#,
                r#"<p class="final-score">最终成绩 82.5 分</p><p>最终成绩 83 分</p>"#,
            ),
        ] {
            let result = ChaoxingChapterWorkVerificationDocument::try_new(document).unwrap();
            assert!(parse_chapter_work_verification_snapshot(&result, &plan, &draft).is_err());
        }
    }

    #[tokio::test]
    async fn chapter_result_resolves_only_bound_supported_standard_answers() {
        let draft = chapter_result_draft().await;
        let result =
            ChaoxingChapterWorkVerificationDocument::try_new(CHAPTER_RESULT.to_owned()).unwrap();
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let candidates = parse_chapter_work_answer_candidates(&result, &questions).unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].source, AnswerSource::ProviderNative);
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Selections(vec!["B".to_owned()])
        );
        assert_eq!(
            candidates[1].answer,
            NormalizedAnswer::Selections(vec!["A".to_owned(), "C".to_owned()])
        );
        assert_eq!(candidates[2].answer, NormalizedAnswer::Boolean(true));
        assert!(candidates.iter().all(|candidate| {
            candidate.confidence.map(AnswerConfidence::basis_points)
                == Some(AnswerConfidence::MAX_BASIS_POINTS)
                && candidate.provenance_sanitized["result_route"] == "selectWorkQuestionYiPiYue"
        }));

        let unknown_option = ChaoxingChapterWorkVerificationDocument::try_new(
            CHAPTER_RESULT.replacen("正确答案：B", "正确答案：D", 1),
        )
        .unwrap();
        assert_eq!(
            parse_chapter_work_answer_candidates(&unknown_option, &questions)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );

        let missing = ChaoxingChapterWorkVerificationDocument::try_new(CHAPTER_RESULT.replacen(
            "正确答案：B",
            "答案待公布",
            1,
        ))
        .unwrap();
        assert_eq!(
            parse_chapter_work_answer_candidates(&missing, &questions)
                .unwrap_err()
                .kind,
            ProviderErrorKind::UnsupportedTask
        );

        let mut reordered = questions;
        reordered.swap(0, 1);
        assert_eq!(
            parse_chapter_work_answer_candidates(&result, &reordered)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[tokio::test]
    async fn chapter_history_keeps_submitted_official_score_and_retake_facts_separate() {
        let draft = chapter_result_draft().await;
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let result =
            ChaoxingChapterWorkVerificationDocument::try_new(CHAPTER_HISTORY_RESULT.to_owned())
                .unwrap();
        let evidence = parse_chapter_work_result_evidence(&result, &questions).unwrap();
        assert_eq!(
            evidence.score(),
            Some(SubmissionScore {
                earned_milli_points: 60_000,
                possible_milli_points: 100_000,
            })
        );
        assert_eq!(
            evidence.retake_entry(),
            Some(ChaoxingChapterWorkRetakeEntry::RedoTest)
        );
        assert_eq!(evidence.questions().len(), 3);
        assert_eq!(evidence.questions()[0].question_id(), questions[0].id);
        assert_eq!(
            evidence.questions()[0].submitted_answer(),
            &NormalizedAnswer::Selections(vec!["A".to_owned()])
        );
        assert_eq!(
            evidence.questions()[0].official_answer(),
            &NormalizedAnswer::Selections(vec!["B".to_owned()])
        );
        assert_eq!(
            evidence.questions()[0].judgement(),
            ChaoxingChapterWorkAnswerJudgement::DiffersFromOfficial
        );
        assert_eq!(
            evidence.questions()[1].judgement(),
            ChaoxingChapterWorkAnswerJudgement::MatchesOfficial
        );
        assert_eq!(
            evidence.questions()[2].submitted_answer(),
            &NormalizedAnswer::Boolean(false)
        );
        assert_eq!(
            evidence.questions()[2].official_answer(),
            &NormalizedAnswer::Boolean(true)
        );
        assert_eq!(evidence.questions()[0].submitted_answer_label(), "我的答案");
        assert_eq!(evidence.questions()[0].official_answer_label(), "正确答案");
        let debug = format!("{evidence:?}");
        assert!(!debug.contains("Selections"));
        assert!(!debug.contains("synthetic-work"));

        let lookalike = ChaoxingChapterWorkVerificationDocument::try_new(
            CHAPTER_HISTORY_RESULT.replace("redoTest(", "notRedoTest("),
        )
        .unwrap();
        assert_eq!(
            parse_chapter_work_result_evidence(&lookalike, &questions)
                .unwrap()
                .retake_entry(),
            None
        );
    }

    #[tokio::test]
    async fn chapter_history_requires_both_bound_answer_labels() {
        let draft = chapter_result_draft().await;
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        for document in [
            CHAPTER_HISTORY_RESULT.replacen("我的答案：A", "答案未显示", 1),
            CHAPTER_HISTORY_RESULT.replacen("正确答案：B", "答案待公布", 1),
        ] {
            let document = ChaoxingChapterWorkVerificationDocument::try_new(document).unwrap();
            assert_eq!(
                parse_chapter_work_result_evidence(&document, &questions)
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::UnsupportedTask
            );
        }
    }

    #[tokio::test]
    async fn exam_result_confirms_or_rejects_only_exact_visible_values() {
        let draft = exam_draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let result = ChaoxingExamVerificationDocument::try_new(EXAM_RESULT.to_owned()).unwrap();
        let snapshot = parse_exam_verification_snapshot(&result, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(
            snapshot.score,
            Some(SubmissionScore {
                earned_milli_points: 82_500,
                possible_milli_points: 100_000,
            })
        );
        assert!(snapshot.questions.iter().all(|question| {
            question.status == SubmissionQuestionVerificationStatus::Confirmed
        }));

        let changed = EXAM_RESULT.replacen("我的答案：对", "我的答案：错", 1);
        let changed = ChaoxingExamVerificationDocument::try_new(changed).unwrap();
        let snapshot = parse_exam_verification_snapshot(&changed, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Rejected);
        assert_eq!(
            snapshot
                .questions
                .iter()
                .map(|question| question.status)
                .collect::<Vec<_>>(),
            [
                SubmissionQuestionVerificationStatus::Confirmed,
                SubmissionQuestionVerificationStatus::Rejected,
            ]
        );
        assert_eq!(snapshot.score.unwrap().earned_milli_points, 82_500);

        for (replacement, expected) in [("0", 0), ("99.999", 99_999)] {
            let result = ChaoxingExamVerificationDocument::try_new(EXAM_RESULT.replacen(
                "82.5",
                replacement,
                1,
            ))
            .unwrap();
            let snapshot = parse_exam_verification_snapshot(&result, &plan, &draft).unwrap();
            assert_eq!(snapshot.score.unwrap().earned_milli_points, expected);
        }

        let no_score =
            EXAM_RESULT.replacen(r#"<p class="exam-score">考试成绩：82.5 分</p>"#, "", 1);
        let no_score = ChaoxingExamVerificationDocument::try_new(no_score).unwrap();
        let snapshot = parse_exam_verification_snapshot(&no_score, &plan, &draft).unwrap();
        assert_eq!(snapshot.status, SubmissionVerificationStatus::Confirmed);
        assert_eq!(snapshot.score, None);
    }

    #[tokio::test]
    async fn exam_result_missing_extra_or_drifted_binding_is_inconclusive() {
        let draft = exam_draft().await;
        let plan = ChaoxingSubmissionPlan::from_draft(&draft).unwrap();
        let extra = EXAM_RESULT.replace(
            "</main>",
            r#"<section class="questionLi"><input name="questionId" value="exam-q-3"><input name="typeexam-q-3" value="0"><p class="my-answer">我的答案：A</p></section></main>"#,
        );
        let swapped = EXAM_RESULT
            .replace("exam-q-1", "exam-q-swap")
            .replace("exam-q-2", "exam-q-1")
            .replace("exam-q-swap", "exam-q-2");
        for document in [
            EXAM_RESULT.replacen("<input name=\"typeexam-q-1\" value=\"0\">", "", 1),
            EXAM_RESULT.replacen("typeexam-q-1\" value=\"0", "typeexam-q-1\" value=\"2", 1),
            EXAM_RESULT.replacen("exam-q-1", "exam-q-x", 3),
            EXAM_RESULT.replacen("class=\"my-answer\"", "class=\"answer-only\"", 1),
            extra,
            swapped,
        ] {
            let result = ChaoxingExamVerificationDocument::try_new(document).unwrap();
            let snapshot = parse_exam_verification_snapshot(&result, &plan, &draft).unwrap();
            assert_eq!(snapshot.status, SubmissionVerificationStatus::Inconclusive);
            assert_eq!(snapshot.score.unwrap().earned_milli_points, 82_500);
            assert!(snapshot.questions.iter().all(|question| {
                question.status == SubmissionQuestionVerificationStatus::Unverified
            }));
        }

        let duplicate = EXAM_RESULT.replace("exam-q-2", "exam-q-1");
        let duplicate = ChaoxingExamVerificationDocument::try_new(duplicate).unwrap();
        assert!(parse_exam_verification_snapshot(&duplicate, &plan, &draft).is_err());

        let duplicate_field = EXAM_RESULT.replacen(
            r#"<input name="questionId" value="exam-q-1">"#,
            r#"<input name="questionId" value="exam-q-1"><input name="questionId" value="exam-q-1">"#,
            1,
        );
        let duplicate_field = ChaoxingExamVerificationDocument::try_new(duplicate_field).unwrap();
        assert!(parse_exam_verification_snapshot(&duplicate_field, &plan, &draft).is_err());
    }

    #[test]
    fn acknowledgement_is_receipt_not_completion() {
        let receipt = parse_submission_receipt(r#"{"status":true,"msg":"提交成功"}"#).unwrap();
        assert_eq!(receipt.remote_status, "accepted");
        assert!(receipt.provider_trace_id.is_none());
        assert_eq!(
            parse_submission_receipt(r#"{"status":false,"msg":"需要验证码"}"#)
                .unwrap_err()
                .kind,
            ProviderErrorKind::HumanRequired
        );
    }

    async fn draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let mut questions = parse_work_preview_question_page(QUESTIONS)
            .unwrap()
            .iter()
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        questions[1].kind = QuestionKind::MultipleChoice;
        questions[1].options = vec![
            asterism_domain::QuestionOption {
                id: "A".to_owned(),
                content: Some("First".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            },
            asterism_domain::QuestionOption {
                id: "C".to_owned(),
                content: Some("Third".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            },
        ];
        questions[1].metadata_sanitized["provider_type_code"] = serde_json::json!(1);
        let selected = vec![
            SelectedAnswer {
                candidate_id: AnswerCandidateId::new(),
                question_id: questions[0].id,
                answer: NormalizedAnswer::Selections(vec!["B".to_owned()]),
                source: AnswerSource::Manual,
                confidence: None,
            },
            SelectedAnswer {
                candidate_id: AnswerCandidateId::new(),
                question_id: questions[1].id,
                answer: NormalizedAnswer::Selections(vec!["C".to_owned(), "A".to_owned()]),
                source: AnswerSource::Manual,
                confidence: None,
            },
        ];
        let context = ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: asterism_domain::ProviderAccountId::new(),
            credential_refs: vec![asterism_domain::SecretId::new()],
            correlation_id: "chaoxing-submission-support".to_owned(),
        };
        let preview = ChaoxingSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(&context, "work:100:200:work-1", &questions, &selected)
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            provider_version: crate::metadata::development_metadata()
                .unwrap()
                .implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 2,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: questions
                .into_iter()
                .zip(selected)
                .map(|(question, selected)| SubmissionDraftItem { question, selected })
                .collect(),
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    async fn partial_draft() -> SubmissionDraft {
        let mut draft = draft().await;
        draft.answer_coverage = SubmissionAnswerCoverage {
            total_question_count: 4,
            minimum_coverage_millis: 500,
            unanswered_question_ids: vec![QuestionId::new(), QuestionId::new()],
        };
        draft
    }

    async fn chapter_result_draft() -> SubmissionDraft {
        let mut draft = chapter_draft().await;
        let removed_question_id = draft.items.remove(2).question.id;
        draft
            .payload_preview
            .fields
            .retain(|field| field.question_id != removed_question_id);
        draft.answer_coverage.total_question_count = 3;
        for (index, item) in draft.items.iter_mut().enumerate() {
            item.question.position = u32::try_from(index + 1).unwrap();
        }
        draft
    }

    async fn partial_chapter_draft() -> SubmissionDraft {
        let mut draft = chapter_draft().await;
        let unanswered = draft.items.remove(2).question.id;
        draft
            .payload_preview
            .fields
            .retain(|field| field.question_id != unanswered);
        draft.answer_coverage = SubmissionAnswerCoverage {
            total_question_count: 4,
            minimum_coverage_millis: 750,
            unanswered_question_ids: vec![unanswered],
        };
        draft
    }

    async fn chapter_draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let questions = parse_chapter_work_question_page(CHAPTER_QUESTIONS)
            .unwrap()
            .iter()
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        let selected = vec![
            selected(
                questions[0].id,
                NormalizedAnswer::Selections(vec!["B".to_owned()]),
            ),
            selected(
                questions[1].id,
                NormalizedAnswer::Selections(vec!["A".to_owned(), "C".to_owned()]),
            ),
            selected(
                questions[2].id,
                NormalizedAnswer::Texts(vec!["bounded".to_owned(), " answer".to_owned()]),
            ),
            selected(questions[3].id, NormalizedAnswer::Boolean(true)),
        ];
        let context = ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: asterism_domain::ProviderAccountId::new(),
            credential_refs: vec![asterism_domain::SecretId::new()],
            correlation_id: "chaoxing-chapter-submission-support".to_owned(),
        };
        let preview = ChaoxingSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context,
                "resource:100:200:4001:job-work",
                &questions,
                &selected,
            )
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            provider_version: crate::metadata::development_metadata()
                .unwrap()
                .implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 4,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: questions
                .into_iter()
                .zip(selected)
                .map(|(question, selected)| SubmissionDraftItem { question, selected })
                .collect(),
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    async fn exam_draft() -> SubmissionDraft {
        let task_id = TaskId::new();
        let questions = parse_exam_question_page(EXAM_QUESTIONS)
            .unwrap()
            .into_iter()
            .take(2)
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        let selected = vec![
            selected(
                questions[0].id,
                NormalizedAnswer::Selections(vec!["A".to_owned()]),
            ),
            selected(questions[1].id, NormalizedAnswer::Boolean(true)),
        ];
        let context = ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: asterism_domain::ProviderAccountId::new(),
            credential_refs: vec![asterism_domain::SecretId::new()],
            correlation_id: "chaoxing-exam-verification-support".to_owned(),
        };
        let preview = ChaoxingSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(&context, "exam:100:200:exam-1", &questions, &selected)
            .await
            .unwrap();
        SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id,
            question_snapshot_id: QuestionSnapshotId::new(),
            provider_id: ProviderId::new("chaoxing").unwrap(),
            provider_version: crate::metadata::development_metadata()
                .unwrap()
                .implementation_version,
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: 2,
                minimum_coverage_millis: 1_000,
                unanswered_question_ids: Vec::new(),
            },
            items: questions
                .into_iter()
                .zip(selected)
                .map(|(question, selected)| SubmissionDraftItem { question, selected })
                .collect(),
            payload_preview: preview,
            created_at: Utc::now(),
        }
    }

    fn selected(
        question_id: asterism_domain::QuestionId,
        answer: NormalizedAnswer,
    ) -> SelectedAnswer {
        SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id,
            answer,
            source: AnswerSource::Manual,
            confidence: None,
        }
    }
}
