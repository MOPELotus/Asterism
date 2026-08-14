use std::{fmt, sync::Arc};

use asterism_domain::{
    AnswerCandidate, AnswerConfidence, AnswerPair, AnswerSource, NormalizedAnswer, Question,
    QuestionKind, TaskCapability,
};
use asterism_provider_api::{
    AnswerResolveCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteTaskDetail, TaskDetailCapability,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    UaiQuestionDocument,
    encrypted::{ZeroizingJsonValue, decrypt_unipus_payload},
    metadata::development_metadata,
    parse_question_content,
    question::supports_question_read,
    task_type::{question_kind_matches_task_type, supports_audited_question_type},
};

const MAX_ANSWER_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_DECRYPTED_ANSWER_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_QUESTIONS_PER_ANSWER: usize = 5_000;

/// Redacted ownership wrapper for one encrypted UAI standard-answer response.
pub struct UaiAnswerDocument(String);

impl UaiAnswerDocument {
    /// Owns one bounded response body.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error when the response is empty or too large.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_ANSWER_DOCUMENT_BYTES {
            return Err(invalid_response(
                "UAI standard-answer response is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiAnswerDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiAnswerDocument")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiAnswerDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// One all-or-nothing fresh encrypted content/answer pair.
#[derive(Debug)]
pub struct UaiAnswerDocuments {
    content: UaiQuestionDocument,
    answer: UaiAnswerDocument,
}

impl UaiAnswerDocuments {
    /// Binds an already bounded fresh content/answer pair for one Group read.
    pub const fn new(content: UaiQuestionDocument, answer: UaiAnswerDocument) -> Self {
        Self { content, answer }
    }

    pub(crate) const fn answer(&self) -> &UaiAnswerDocument {
        &self.answer
    }
}

/// Native boundary that rebinds a Group and reads content before its standard
/// answer under one session attempt.
#[async_trait]
pub trait UaiAnswerTransport: Send + Sync {
    async fn fetch_answer_documents(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiAnswerDocuments>;
}

/// Provider-native standard-answer resolver kept independent from every
/// submission stage.
pub struct UaiAnswerResolve {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn UaiAnswerTransport>,
}

impl UaiAnswerResolve {
    /// Builds the resolver around fresh Task detail and encrypted read boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn UaiAnswerTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
        })
    }
}

impl fmt::Debug for UaiAnswerResolve {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiAnswerResolve")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiAnswerResolve {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AnswerResolveCapability for UaiAnswerResolve {
    async fn resolve_answers(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Vec<AnswerCandidate>> {
        validate_context(context, &self.metadata)?;
        let identity = GroupIdentity::parse(remote_task_id)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        let shape = TaskAnswerShape::from_detail(&detail, &identity, remote_task_id)?;
        validate_snapshot_questions(questions, &shape, remote_task_id)?;
        let documents = self
            .transport
            .fetch_answer_documents(context, &identity.course_resource, &identity.group)
            .await?;
        let parsed = parse_question_content(
            documents.content.as_str(),
            remote_task_id,
            &shape.task_types,
            Some(shape.question_count),
        )?;
        validate_fresh_content(questions, parsed)?;
        parse_answer_candidates(documents.answer.as_str(), questions)
    }
}

/// Decrypts and binds one standard-answer response to an exact current
/// Question snapshot.
///
/// # Errors
///
/// Returns a typed invalid-response/protocol-drift error for malformed framing,
/// counts, identities, answer shapes or option bindings.
pub fn parse_answer_candidates(
    document: &str,
    questions: &[Question],
) -> ProviderResult<Vec<AnswerCandidate>> {
    validate_question_sequence(questions)?;
    let decrypted = decrypt_answer_entries(document)?;
    let entries = decrypted
        .as_value()
        .as_array()
        .ok_or_else(|| protocol_drift("UAI decrypted standard answer is not an array"))?;
    if entries.len() != questions.len() || entries.len() > MAX_QUESTIONS_PER_ANSWER {
        return Err(protocol_drift(
            "UAI standard-answer count does not match the Question snapshot",
        ));
    }
    entries
        .iter()
        .zip(questions)
        .map(|(entry, question)| parse_answer_entry(entry, question))
        .collect()
}

pub(crate) fn decrypt_answer_entries(document: &str) -> ProviderResult<ZeroizingJsonValue> {
    if document.is_empty() || document.len() > MAX_ANSWER_DOCUMENT_BYTES {
        return Err(invalid_response(
            "UAI standard-answer response is empty or exceeds the size limit",
        ));
    }
    let envelope: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI standard-answer response is not valid JSON"))?;
    let envelope = envelope
        .as_object()
        .ok_or_else(|| protocol_drift("UAI standard-answer response is not an object"))?;
    if envelope.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(protocol_drift("UAI standard-answer read did not succeed"));
    }
    let encrypted = envelope
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI standard-answer response has no ciphertext"))?;
    let key_suffix = envelope
        .get("k")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI standard-answer response has no key suffix"))?;
    let plaintext = Zeroizing::new(decrypt_unipus_payload(
        encrypted,
        key_suffix,
        MAX_DECRYPTED_ANSWER_BYTES,
    )?);
    let decrypted = ZeroizingJsonValue::new(
        serde_json::from_slice(&plaintext)
            .map_err(|_| invalid_response("UAI decrypted standard answer is not valid JSON"))?,
    );
    let entries = decrypted
        .as_value()
        .as_array()
        .ok_or_else(|| protocol_drift("UAI decrypted standard answer is not an array"))?;
    if entries.len() > MAX_QUESTIONS_PER_ANSWER {
        return Err(protocol_drift(
            "UAI standard-answer count exceeds its bound",
        ));
    }
    Ok(decrypted)
}

fn parse_answer_entry(entry: &Value, question: &Question) -> ProviderResult<AnswerCandidate> {
    let entry = entry
        .as_object()
        .ok_or_else(|| protocol_drift("UAI standard-answer entry is not an object"))?;
    let remote_id = entry
        .get("id")
        .and_then(remote_identity)
        .ok_or_else(|| protocol_drift("UAI standard-answer entry has no identity"))?;
    if question.remote_question_id.as_deref() != Some(remote_id.as_str()) {
        return Err(protocol_drift(
            "UAI standard-answer identity does not match the Question snapshot",
        ));
    }
    let answer = match question.kind {
        QuestionKind::SingleChoice => {
            let selections = choice_answers(entry)?;
            if selections.len() != 1 {
                return Err(protocol_drift(
                    "UAI single-choice standard answer is not singular",
                ));
            }
            validate_option_bindings(question, &selections)?;
            NormalizedAnswer::Selections(selections)
        }
        QuestionKind::MultipleChoice => {
            let selections = choice_answers(entry)?;
            validate_option_bindings(question, &selections)?;
            NormalizedAnswer::Selections(selections)
        }
        QuestionKind::ShortAnswer => NormalizedAnswer::Texts(text_answers(entry, true)?),
        QuestionKind::FillBlank => NormalizedAnswer::Texts(text_answers(entry, false)?),
        QuestionKind::Ordering => NormalizedAnswer::Ordering(ordered_answers(entry)?),
        QuestionKind::Matching => NormalizedAnswer::Pairs(matching_answers(entry, question)?),
        QuestionKind::Composite => composite_choice_answers(entry, question)?,
        _ => {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI standard-answer resolver does not support this Question kind",
            ));
        }
    };
    let candidate = AnswerCandidate {
        question_id: question.id,
        source: AnswerSource::ProviderNative,
        answer,
        confidence: Some(
            AnswerConfidence::try_new(AnswerConfidence::MAX_BASIS_POINTS)
                .map_err(|_| internal("UAI standard-answer confidence is invalid"))?,
        ),
        explanation: None,
        provenance_sanitized: json!({
            "schema": "uai.encrypted-standard-answer.v1",
            "remote_question_id": remote_id,
        }),
    };
    candidate
        .validate()
        .map_err(|_| invalid_response("UAI normalized standard-answer candidate is invalid"))?;
    Ok(candidate)
}

fn choice_answers(entry: &serde_json::Map<String, Value>) -> ProviderResult<Vec<String>> {
    let answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_drift("UAI choice standard answer has no answer document"))?;
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| invalid_response("UAI nested choice answer is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI nested choice answer has no children"))?;
    if children.len() != 1 {
        return Err(protocol_drift(
            "UAI supported choice answer does not have exactly one child",
        ));
    }
    let answers = answer_child_values(&children[0])
        .ok_or_else(|| protocol_drift("UAI nested choice answer has no selections"))?;
    let mut result = Vec::with_capacity(answers.len());
    for answer in answers {
        let answer = answer
            .as_str()
            .map(normalize_text)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| protocol_drift("UAI choice standard answer is not bounded text"))?;
        if result.contains(&answer) {
            return Err(protocol_drift(
                "UAI choice standard answer contains duplicate selections",
            ));
        }
        result.push(answer);
    }
    if result.is_empty() {
        return Err(protocol_drift("UAI choice standard answer is empty"));
    }
    Ok(result)
}

fn text_answers(
    entry: &serde_json::Map<String, Value>,
    allow_analysis_fallback: bool,
) -> ProviderResult<Vec<String>> {
    if let Some(answer) = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let answer = ZeroizingJsonValue::new(
            serde_json::from_str(answer)
                .map_err(|_| invalid_response("UAI nested text answer is not valid JSON"))?,
        );
        let children = answer
            .as_value()
            .get("children")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI nested text answer has no children"))?;
        let mut result = Vec::with_capacity(children.len());
        for child in children {
            let values = answer_child_values(child)
                .ok_or_else(|| protocol_drift("UAI nested text answer child has no values"))?;
            if values.len() != 1 {
                return Err(protocol_drift(
                    "UAI nested text answer child is not singular",
                ));
            }
            let text = values[0]
                .as_str()
                .map(normalize_text)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| protocol_drift("UAI text answer is not bounded text"))?;
            result.push(text);
        }
        if result.is_empty() {
            return Err(protocol_drift("UAI text standard answer is empty"));
        }
        return Ok(result);
    }
    if !allow_analysis_fallback {
        return Err(protocol_drift(
            "UAI text standard answer has no answer document",
        ));
    }
    let analysis = entry
        .get("analysis")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            protocol_drift("UAI short-answer standard answer has no answer or analysis")
        })?;
    short_answer_analysis(analysis)
}

fn short_answer_analysis(analysis: &str) -> ProviderResult<Vec<String>> {
    let analysis = analysis.trim();
    if !analysis.starts_with('{') {
        let text = normalize_text(analysis);
        return if text.is_empty() {
            Err(protocol_drift(
                "UAI short-answer analysis is not bounded text",
            ))
        } else {
            Ok(vec![text])
        };
    }
    let analysis = ZeroizingJsonValue::new(
        serde_json::from_str(analysis)
            .map_err(|_| invalid_response("UAI nested short-answer analysis is not valid JSON"))?,
    );
    if let Some(text) = analysis
        .as_value()
        .get("analysis")
        .and_then(Value::as_str)
        .map(normalize_text)
        .filter(|value| !value.is_empty())
    {
        return Ok(vec![text]);
    }
    let children = analysis
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            protocol_drift("UAI nested short-answer analysis has no text or children")
        })?;
    let mut result = Vec::with_capacity(children.len());
    for child in children {
        let text = child
            .get("analysis")
            .and_then(Value::as_str)
            .map(normalize_text)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| protocol_drift("UAI short-answer analysis is not bounded text"))?;
        result.push(text);
    }
    if result.is_empty() {
        return Err(protocol_drift("UAI short-answer standard answer is empty"));
    }
    Ok(result)
}

fn ordered_answers(entry: &serde_json::Map<String, Value>) -> ProviderResult<Vec<String>> {
    let answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_drift("UAI ordering standard answer has no answer document"))?;
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| invalid_response("UAI nested ordering answer is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI nested ordering answer has no children"))?;
    let mut result = Vec::new();
    for child in children {
        let values = answer_child_values(child)
            .ok_or_else(|| protocol_drift("UAI nested ordering answer child has no values"))?;
        for value in values {
            let value = value
                .as_str()
                .map(normalize_text)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| protocol_drift("UAI ordering answer is not bounded text"))?;
            if result.contains(&value) {
                return Err(protocol_drift(
                    "UAI ordering standard answer contains duplicate values",
                ));
            }
            result.push(value);
        }
    }
    if result.is_empty() {
        return Err(protocol_drift("UAI ordering standard answer is empty"));
    }
    Ok(result)
}

fn matching_answers(
    entry: &serde_json::Map<String, Value>,
    question: &Question,
) -> ProviderResult<Vec<AnswerPair>> {
    let lefts = question
        .metadata_sanitized
        .get("matching_lefts")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI matching Question has no bound left values"))?;
    let answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_drift("UAI matching standard answer has no answer document"))?;
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| invalid_response("UAI nested matching answer is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("UAI nested matching answer has no children"))?;
    if children.len() != lefts.len() || children.is_empty() {
        return Err(protocol_drift(
            "UAI matching answer count differs from its Question children",
        ));
    }
    lefts
        .iter()
        .zip(children)
        .map(|(left, child)| {
            let left = left
                .as_str()
                .map(normalize_text)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| protocol_drift("UAI matching left value is invalid"))?;
            let values = answer_child_values(child)
                .filter(|values| values.len() == 1)
                .ok_or_else(|| protocol_drift("UAI matching answer child is not singular"))?;
            let right = values[0]
                .as_str()
                .map(normalize_text)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| protocol_drift("UAI matching right value is invalid"))?;
            Ok(AnswerPair { left, right })
        })
        .collect()
}

fn answer_child_values(child: &Value) -> Option<&Vec<Value>> {
    ["answers", "value"].into_iter().find_map(|field| {
        child
            .get(field)
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
    })
}

fn composite_choice_answers(
    entry: &serde_json::Map<String, Value>,
    question: &Question,
) -> ProviderResult<NormalizedAnswer> {
    let components = question
        .metadata_sanitized
        .get("composite_children")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| protocol_drift("UAI composite choice has no bound components"))?;
    let answer = entry
        .get("answer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_drift("UAI composite choice has no answer document"))?;
    let answer = ZeroizingJsonValue::new(
        serde_json::from_str(answer)
            .map_err(|_| invalid_response("UAI nested composite answer is not valid JSON"))?,
    );
    let children = answer
        .as_value()
        .get("children")
        .and_then(Value::as_array)
        .filter(|values| values.len() == components.len())
        .ok_or_else(|| {
            protocol_drift("UAI composite answer count differs from its Question components")
        })?;
    components
        .iter()
        .zip(children)
        .map(|(component, child)| composite_choice_child_answer(component, child))
        .collect::<ProviderResult<Vec<_>>>()
        .map(NormalizedAnswer::Composite)
}

fn composite_choice_child_answer(
    component: &Value,
    child: &Value,
) -> ProviderResult<NormalizedAnswer> {
    let component = component
        .as_object()
        .ok_or_else(|| protocol_drift("UAI composite choice component is invalid"))?;
    let kind = component
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI composite choice kind is missing"))?;
    let option_ids = component_option_ids(component)?;
    let values = answer_child_values(child)
        .ok_or_else(|| protocol_drift("UAI composite answer child has no values"))?;
    let mut selections = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .map(normalize_text)
            .filter(|value| !value.is_empty() && option_ids.contains(value))
            .ok_or_else(|| {
                protocol_drift("UAI composite answer is absent from its component options")
            })?;
        if selections.contains(&value) {
            return Err(protocol_drift(
                "UAI composite answer contains duplicate selections",
            ));
        }
        selections.push(value);
    }
    if (kind == "single_choice" && selections.len() != 1)
        || (kind != "single_choice" && kind != "multiple_choice")
    {
        return Err(protocol_drift(
            "UAI composite answer does not match its component kind",
        ));
    }
    Ok(NormalizedAnswer::Selections(selections))
}

fn component_option_ids(component: &serde_json::Map<String, Value>) -> ProviderResult<Vec<String>> {
    let options = component
        .get("options")
        .and_then(Value::as_array)
        .filter(|values| values.len() >= 2)
        .ok_or_else(|| protocol_drift("UAI composite choice options are invalid"))?;
    let mut result = Vec::with_capacity(options.len());
    for option in options {
        let id = option
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| valid_metadata_identity(value))
            .map(str::to_owned)
            .filter(|value| !result.contains(value))
            .ok_or_else(|| protocol_drift("UAI composite choice option identity is invalid"))?;
        result.push(id);
    }
    Ok(result)
}

fn valid_metadata_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_QUESTION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn validate_option_bindings(question: &Question, selections: &[String]) -> ProviderResult<()> {
    if question.options.is_empty()
        || selections.iter().any(|selection| {
            !question
                .options
                .iter()
                .any(|option| option.id == *selection)
        })
    {
        return Err(protocol_drift(
            "UAI standard-answer selection is absent from the Question snapshot",
        ));
    }
    Ok(())
}

fn validate_fresh_content(
    questions: &[Question],
    parsed: Vec<crate::ParsedUaiQuestion>,
) -> ProviderResult<()> {
    if parsed.len() != questions.len() {
        return Err(remote_changed(
            "UAI Question content changed before standard-answer resolution",
        ));
    }
    let task_id = questions
        .first()
        .ok_or_else(|| protocol_drift("UAI standard-answer Question snapshot is empty"))?
        .task_id;
    for (parsed, persisted) in parsed.into_iter().zip(questions) {
        let fresh = parsed.to_question(task_id)?;
        let same_fingerprint =
            fresh.content_fingerprint().ok() == persisted.content_fingerprint().ok();
        if fresh.task_id != persisted.task_id
            || fresh.remote_question_id != persisted.remote_question_id
            || fresh.position != persisted.position
            || !same_fingerprint
        {
            return Err(remote_changed(
                "UAI Question content changed before standard-answer resolution",
            ));
        }
    }
    Ok(())
}

fn validate_snapshot_questions(
    questions: &[Question],
    shape: &TaskAnswerShape,
    remote_task_id: &str,
) -> ProviderResult<()> {
    validate_question_sequence(questions)?;
    if usize::try_from(shape.question_count).ok() != Some(questions.len()) {
        return Err(remote_changed(
            "UAI fresh Group question count differs from the Question snapshot",
        ));
    }
    for (index, question) in questions.iter().enumerate() {
        let task_type = shape
            .task_types
            .get(index)
            .or_else(|| shape.task_types.last())
            .ok_or_else(|| protocol_drift("UAI fresh Group has no Question type"))?;
        if !question_kind_matches_task_type(question.kind, task_type) {
            return Err(remote_changed(
                "UAI fresh Group Question type differs from the Question snapshot",
            ));
        }
        if question
            .metadata_sanitized
            .get("remote_task_id")
            .and_then(Value::as_str)
            != Some(remote_task_id)
        {
            return Err(remote_changed(
                "UAI Question snapshot belongs to another remote Task",
            ));
        }
    }
    Ok(())
}

fn validate_question_sequence(questions: &[Question]) -> ProviderResult<()> {
    if questions.is_empty() || questions.len() > MAX_QUESTIONS_PER_ANSWER {
        return Err(protocol_drift(
            "UAI standard-answer Question snapshot has no bounded Question set",
        ));
    }
    let task_id = questions[0].task_id;
    for (index, question) in questions.iter().enumerate() {
        let position = u32::try_from(index + 1)
            .map_err(|_| invalid_response("UAI Question position exceeds the limit"))?;
        if question.task_id != task_id
            || question.position != position
            || question.remote_question_id.is_none()
            || question.validate().is_err()
            || !matches!(
                question.kind,
                QuestionKind::SingleChoice
                    | QuestionKind::MultipleChoice
                    | QuestionKind::ShortAnswer
                    | QuestionKind::FillBlank
                    | QuestionKind::Ordering
                    | QuestionKind::Matching
                    | QuestionKind::Composite
            )
        {
            return Err(protocol_drift(
                "UAI standard-answer Question snapshot is invalid or unsupported",
            ));
        }
    }
    Ok(())
}

struct GroupIdentity {
    course_resource: String,
    unit: String,
    group: String,
}

impl GroupIdentity {
    fn parse(value: &str) -> ProviderResult<Self> {
        if value.is_empty()
            || value.len() > MAX_REMOTE_TASK_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(protocol_drift("UAI Group Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("group") {
            return Err(protocol_drift("UAI Group Task identity is invalid"));
        }
        let course_resource = valid_component(components.next())?;
        let unit = valid_component(components.next())?;
        let group = valid_component(components.next())?;
        if components.next().is_some() {
            return Err(protocol_drift("UAI Group Task identity is invalid"));
        }
        Ok(Self {
            course_resource,
            unit,
            group,
        })
    }
}

struct TaskAnswerShape {
    task_types: Vec<String>,
    question_count: u32,
}

impl TaskAnswerShape {
    fn from_detail(
        detail: &RemoteTaskDetail,
        identity: &GroupIdentity,
        remote_task_id: &str,
    ) -> ProviderResult<Self> {
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::AnswerResolve)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI Group Task does not advertise standard-answer resolution",
            ));
        }
        let task = detail
            .normalized_detail
            .get("task")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("UAI fresh Group detail has no normalized Task"))?;
        if task.get("course_resource_id").and_then(Value::as_str)
            != Some(identity.course_resource.as_str())
            || task
                .get("unit")
                .and_then(Value::as_object)
                .and_then(|unit| unit.get("id"))
                .and_then(Value::as_str)
                != Some(identity.unit.as_str())
            || task.get("group_id").and_then(Value::as_str) != Some(identity.group.as_str())
        {
            return Err(protocol_drift(
                "UAI fresh Group answer detail does not match remote identity",
            ));
        }
        let task_types = task
            .get("task_types")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol_drift("UAI fresh Group detail has no task types"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| supported_answer_type(value))
                    .map(str::to_owned)
                    .ok_or_else(|| protocol_drift("UAI fresh Group has an unsupported task type"))
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        let question_count = task
            .get("question_count")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| protocol_drift("UAI fresh Group has an invalid question count"))?;
        if !supports_question_read(&task_types, Some(question_count)) {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI Group Task does not have a supported standard-answer shape",
            ));
        }
        Ok(Self {
            task_types,
            question_count,
        })
    }
}

fn remote_identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn valid_component(value: Option<&str>) -> ProviderResult<String> {
    let value = value.ok_or_else(|| protocol_drift("UAI Group Task identity is invalid"))?;
    if value.is_empty()
        || value.len() > MAX_REMOTE_COMPONENT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(protocol_drift("UAI Group Task identity is invalid"));
    }
    Ok(value.to_owned())
}

fn supported_answer_type(value: &str) -> bool {
    supports_audited_question_type(value)
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "UAI standard-answer read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI standard-answer read requires an authenticated session",
        ));
    }
    Ok(())
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

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{NormalizedAnswer, ProviderAccountId, ProviderId, SecretId, TaskId};

    use super::*;
    use crate::{parse_course_context, parse_course_inventory, parse_task_inventory};

    const ANSWER: &str =
        include_str!("../../../fixtures/providers/uai/answers/standard-multiple-choice.json");
    const CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-multiple-choice.json");
    const MIXED_ANSWER: &str =
        include_str!("../../../fixtures/providers/uai/answers/standard-mixed-simple.json");
    const MIXED_CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-mixed-simple.json");
    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");
    const DETAIL: &str =
        include_str!("../../../fixtures/providers/uai/courses/resource-detail.json");
    const TREE: &str = include_str!("../../../fixtures/providers/uai/tasks/tree-mixed.json");

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let course = parse_course_inventory(COURSES)?.remove(0);
            let context = parse_course_context(&course, DETAIL)?;
            let tree = TREE.replace("rich-text-read", "multichoice");
            let task = parse_task_inventory(&course, &context, &tree)?
                .into_iter()
                .find(|task| task.remote_id == remote_task_id)
                .ok_or_else(|| protocol_drift("synthetic UAI Group is missing"))?;
            Ok(RemoteTaskDetail {
                normalized_detail: json!({
                    "schema": "uai.group-task-detail.v1",
                    "task": task.normalized.clone(),
                }),
                task,
            })
        }
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl UaiAnswerTransport for FixtureTransport {
        async fn fetch_answer_documents(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            group_id: &str,
        ) -> ProviderResult<UaiAnswerDocuments> {
            self.calls
                .lock()
                .unwrap()
                .push((course_resource_id.to_owned(), group_id.to_owned()));
            Ok(UaiAnswerDocuments::new(
                UaiQuestionDocument::try_new(CONTENT.to_owned())?,
                UaiAnswerDocument::try_new(ANSWER.to_owned())?,
            ))
        }
    }

    #[test]
    fn encrypted_standard_answer_becomes_bound_native_candidates() {
        let questions = fixture_questions();
        let candidates = parse_answer_candidates(ANSWER, &questions).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].question_id, questions[0].id);
        assert_eq!(candidates[0].source, AnswerSource::ProviderNative);
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()])
        );
        assert_eq!(
            candidates[0]
                .confidence
                .expect("native standard answer confidence")
                .basis_points(),
            AnswerConfidence::MAX_BASIS_POINTS
        );
        let encoded = serde_json::to_string(&candidates).unwrap();
        assert!(!encoded.contains("unipus."));
        assert!(!encoded.contains("k1234567"));
        assert!(!encoded.contains("must_be_dropped"));
        assert!(!encoded.contains("sensitive-remote-answer-noise"));
        let document = UaiAnswerDocument::try_new(ANSWER.to_owned()).unwrap();
        assert!(!format!("{document:?}").contains("unipus."));
    }

    #[test]
    fn mixed_standard_answers_bind_each_ordered_question() {
        let task_id = TaskId::new();
        let questions = parse_question_content(
            MIXED_CONTENT,
            "group:2001:unit-1:group-mixed",
            &["multichoice".to_owned(), "short_answer".to_owned()],
            Some(2),
        )
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
        let candidates = parse_answer_candidates(MIXED_ANSWER, &questions).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()])
        );
        assert_eq!(
            candidates[1].answer,
            NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()])
        );
    }

    #[test]
    fn donor_writing_analysis_resolves_as_bound_text() {
        let task_id = TaskId::new();
        let questions = parse_question_content(
            MIXED_CONTENT,
            "group:2001:unit-1:group-writing",
            &["multichoice".to_owned(), "writing".to_owned()],
            Some(2),
        )
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect::<Vec<_>>();
        let candidates = parse_answer_candidates(MIXED_ANSWER, &questions).unwrap();
        assert_eq!(
            candidates[1].answer,
            NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()])
        );
    }

    #[test]
    fn donor_fillblank_standard_answers_preserve_child_order() {
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("2001".to_owned()),
            kind: QuestionKind::FillBlank,
            stem: "Fill every blank".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "material-banked-cloze",
                "remote_task_id": "group:2001:unit-1:group-fillblank",
            }),
            position: 1,
        };
        let document = encrypted_answer_for_test(
            r#"[{"id":"2001","answer":"{\"children\":[{\"answers\":[\"first\"]},{\"value\":[\"second\"]}]}"}]"#,
        );
        let candidates = parse_answer_candidates(&document, &[question]).unwrap();
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()])
        );
    }

    #[test]
    fn donor_video_popup_standard_answers_follow_the_content_derived_shape() {
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("2102".to_owned()),
            kind: QuestionKind::FillBlank,
            stem: "Watch and complete".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "video-popup",
                "remote_task_id": "group:2001:unit-1:group-video-popup",
            }),
            position: 1,
        };
        let document = encrypted_answer_for_test(
            r#"[{"id":"2102","answer":"{\"children\":[{\"answers\":[\"first\"]},{\"value\":[\"second\"]}]}"}]"#,
        );
        let candidates = parse_answer_candidates(&document, &[question]).unwrap();

        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Texts(vec!["first".to_owned(), "second".to_owned()])
        );
    }

    #[test]
    fn donor_matching_standard_answers_bind_each_left() {
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("3001".to_owned()),
            kind: QuestionKind::Matching,
            stem: "Match each expression".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "basic-scoop-content",
                "remote_task_id": "group:2001:unit-1:group-matching",
                "matching_lefts": ["first left", "second left"],
            }),
            position: 1,
        };
        let document = encrypted_answer_for_test(
            r#"[{"id":"3001","answer":"{\"children\":[{\"answers\":[\"first right\"]},{\"value\":[\"second right\"]}]}"}]"#,
        );
        let candidates = parse_answer_candidates(&document, &[question]).unwrap();
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Pairs(vec![
                AnswerPair {
                    left: "first left".to_owned(),
                    right: "first right".to_owned(),
                },
                AnswerPair {
                    left: "second left".to_owned(),
                    right: "second right".to_owned(),
                },
            ])
        );
    }

    #[test]
    fn donor_sequence_standard_answers_preserve_order() {
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("4001".to_owned()),
            kind: QuestionKind::Ordering,
            stem: "Put the clauses in order".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "sequence",
                "remote_task_id": "group:2001:unit-1:group-sequence",
            }),
            position: 1,
        };
        let document = encrypted_answer_for_test(
            r#"[{"id":"4001","answer":"{\"children\":[{\"answers\":[\"second\"]},{\"value\":[\"first\"]}]}"}]"#,
        );
        let candidates = parse_answer_candidates(&document, &[question]).unwrap();
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Ordering(vec!["second".to_owned(), "first".to_owned()])
        );
    }

    #[test]
    fn donor_short_answer_analysis_accepts_plain_and_object_text() {
        for analysis in [
            "a concise synthetic explanation",
            r#"{"analysis":"a concise synthetic explanation"}"#,
        ] {
            let question = Question {
                id: asterism_domain::QuestionId::new(),
                task_id: TaskId::new(),
                remote_question_id: Some("6001".to_owned()),
                kind: QuestionKind::ShortAnswer,
                stem: "Explain briefly".to_owned(),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: json!({
                    "schema": "uai.encrypted-question.v1",
                    "task_type": "short_answer",
                    "remote_task_id": "group:2001:unit-1:group-short-answer",
                }),
                position: 1,
            };
            let plaintext = json!([{
                "id": "6001",
                "answer": "",
                "analysis": analysis,
            }])
            .to_string();
            let document = encrypted_answer_for_test(&plaintext);
            let candidates = parse_answer_candidates(&document, &[question]).unwrap();
            assert_eq!(
                candidates[0].answer,
                NormalizedAnswer::Texts(vec!["a concise synthetic explanation".to_owned()])
            );
        }
    }

    #[test]
    fn donor_choice_answer_falls_back_from_empty_answers_to_value() {
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("7001".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Choose one".to_owned(),
            options: vec![
                asterism_domain::QuestionOption {
                    id: "A".to_owned(),
                    content: Some("Alpha".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                },
                asterism_domain::QuestionOption {
                    id: "B".to_owned(),
                    content: Some("Beta".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "single-choice",
                "remote_task_id": "group:2001:unit-1:group-choice",
            }),
            position: 1,
        };
        let plaintext = json!([{
            "id": "7001",
            "answer": json!({
                "children": [{"answers": [], "value": ["B"]}],
            })
            .to_string(),
        }])
        .to_string();
        let candidates =
            parse_answer_candidates(&encrypted_answer_for_test(&plaintext), &[question]).unwrap();
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Selections(vec!["B".to_owned()])
        );
    }

    #[test]
    fn donor_composite_choice_answer_preserves_each_child() {
        let question = Question {
            id: asterism_domain::QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("8001".to_owned()),
            kind: QuestionKind::Composite,
            stem: "Answer both parts".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "uai.encrypted-question.v1",
                "task_type": "multichoice",
                "remote_task_id": "group:2001:unit-1:group-composite-choice",
                "composite_children": [
                    {
                        "kind": "single_choice",
                        "options": [
                            {"id":"A","content":"First A","attachments":[],"metadata_sanitized":{}},
                            {"id":"B","content":"First B","attachments":[],"metadata_sanitized":{}},
                        ],
                    },
                    {
                        "kind": "multiple_choice",
                        "options": [
                            {"id":"A","content":"Second A","attachments":[],"metadata_sanitized":{}},
                            {"id":"B","content":"Second B","attachments":[],"metadata_sanitized":{}},
                        ],
                    },
                ],
            }),
            position: 1,
        };
        let plaintext = json!([{
            "id": "8001",
            "answer": json!({
                "children": [
                    {"answers": ["B"]},
                    {"answers": ["A", "B"]},
                ],
            })
            .to_string(),
        }])
        .to_string();
        let candidates =
            parse_answer_candidates(&encrypted_answer_for_test(&plaintext), &[question]).unwrap();
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Composite(vec![
                NormalizedAnswer::Selections(vec!["B".to_owned()]),
                NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
            ])
        );
    }

    #[test]
    fn standard_answer_count_identity_shape_and_option_drift_fail_closed() {
        let questions = fixture_questions();
        assert!(parse_answer_candidates(&encrypted_answer_for_test(r"[]"), &questions).is_err());
        assert!(
            parse_answer_candidates(
                &encrypted_answer_for_test(
                    r#"[{"id":"changed","answer":"{\"children\":[{\"answers\":[\"A\"]}]}"}]"#,
                ),
                &questions,
            )
            .is_err()
        );
        assert!(
            parse_answer_candidates(
                &encrypted_answer_for_test(
                    r#"[{"id":"1001","answer":"{\"children\":[{\"answers\":[\"C\"]}]}"}]"#,
                ),
                &questions,
            )
            .is_err()
        );
        assert!(
            parse_answer_candidates(&ANSWER.replace("unipus.", "changed."), &questions).is_err()
        );
    }

    #[tokio::test]
    async fn capability_rechecks_fresh_group_content_and_answer_as_one_pair() {
        let transport = Arc::new(FixtureTransport::default());
        let resolver = UaiAnswerResolve::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
        )
        .unwrap();
        let candidates = resolver
            .resolve_answers(
                &provider_context(),
                "group:2001:unit-1:group-1",
                &fixture_questions(),
            )
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[("2001".to_owned(), "group-1".to_owned())]
        );
    }

    #[tokio::test]
    async fn answer_shape_rejects_fresh_type_count_cardinality_drift() {
        let remote_task_id = "group:2001:unit-1:group-1";
        let mut detail = FixtureDetail {
            metadata: development_metadata().unwrap(),
        }
        .task_detail(&provider_context(), remote_task_id)
        .await
        .unwrap();
        detail.normalized_detail["task"]["question_count"] = json!(3);
        detail.normalized_detail["task"]["task_types"] = json!(["multichoice", "short_answer"]);
        let identity = GroupIdentity::parse(remote_task_id).unwrap();
        let Err(error) = TaskAnswerShape::from_detail(&detail, &identity, remote_task_id) else {
            panic!("invalid UAI answer cardinality must fail closed");
        };
        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
    }

    fn fixture_questions() -> Vec<Question> {
        let task_id = TaskId::new();
        parse_question_content(
            CONTENT,
            "group:2001:unit-1:group-1",
            &["multichoice".to_owned()],
            Some(1),
        )
        .unwrap()
        .iter()
        .map(|question| question.to_question(task_id).unwrap())
        .collect()
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            correlation_id: "answer-resolve".to_owned(),
            credential_refs: vec![SecretId::new()],
        }
    }

    fn encrypted_answer_for_test(plaintext: &str) -> String {
        use aes::{
            Aes128,
            cipher::{Array, BlockCipherEncrypt, KeyInit},
        };

        let key = *b"1a2b3c4dk1234567";
        let cipher = Aes128::new(&Array::from(key));
        let padding = 16 - plaintext.len() % 16;
        let mut bytes = plaintext.as_bytes().to_vec();
        bytes.extend(std::iter::repeat_n(u8::try_from(padding).unwrap(), padding));
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for chunk in bytes.chunks_exact_mut(16) {
            let mut block_bytes = [0_u8; 16];
            block_bytes.copy_from_slice(chunk);
            let mut block = Array::from(block_bytes);
            cipher.encrypt_block(&mut block);
            for byte in block {
                use std::fmt::Write;
                write!(&mut encoded, "{byte:02x}").unwrap();
            }
        }
        bytes.zeroize();
        format!(r#"{{"code":0,"data":"unipus.{encoded}","k":"k1234567"}}"#)
    }
}
