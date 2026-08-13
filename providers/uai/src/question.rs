use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use asterism_domain::{Question, QuestionId, QuestionKind, QuestionOption, TaskCapability, TaskId};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, ProviderRouteContext, QuestionInventoryCapability, QuestionParseCapability,
    RemoteQuestionRef, RemoteTaskDetail, TaskDetailCapability,
};
use async_trait::async_trait;
use scraper::Html;
use serde_json::{Map, Value, json};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    encrypted::{ZeroizingJsonValue, decrypt_unipus_payload},
    metadata::development_metadata,
    task_type::{audited_question_kind, supports_audited_question_type},
};

const MAX_QUESTION_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_DECRYPTED_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_INNER_CONTENT_BYTES: usize = 1_024 * 1_024;
const MAX_QUESTIONS_PER_DOCUMENT: usize = 5_000;
const MAX_ACTIVE_QUESTION_ATTEMPTS: usize = 128;
const QUESTION_ATTEMPT_TTL: Duration = Duration::from_mins(5);
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_JUDGE_TYPE_BYTES: usize = 128;
const MAX_QUESTION_CHILDREN: usize = 256;
const MAX_OPTIONS_PER_CHILD: usize = 256;

/// Redacted ownership wrapper for one encrypted UAI content response.
pub struct UaiQuestionDocument(String);

impl UaiQuestionDocument {
    /// Owns one bounded response body.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error when the response is empty or too large.
    pub fn try_new(document: String) -> ProviderResult<Self> {
        if document.is_empty() || document.len() > MAX_QUESTION_DOCUMENT_BYTES {
            return Err(invalid_response(
                "UAI Question content is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiQuestionDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiQuestionDocument")
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for UaiQuestionDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Native read boundary for one identity-bound encrypted Group content document.
#[async_trait]
pub trait UaiQuestionTransport: Send + Sync {
    async fn fetch_question_content(
        &self,
        context: &ProviderContext,
        course_resource_id: &str,
        group_id: &str,
    ) -> ProviderResult<UaiQuestionDocument>;
}

/// Independent, answer-free UAI Question inventory and parser.
pub struct UaiQuestionRead {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn UaiQuestionTransport>,
    attempts: Mutex<HashMap<QuestionAttemptKey, CachedQuestionAttempt>>,
}

impl UaiQuestionRead {
    /// Builds the Question reader around fresh Task detail and content boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn UaiQuestionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
            attempts: Mutex::new(HashMap::new()),
        })
    }

    async fn discover_questions(
        &self,
        context: &ProviderContext,
        identity: &GroupIdentity,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<ParsedUaiQuestion>> {
        let detail = self.details.task_detail(context, remote_task_id).await?;
        let shape = TaskQuestionShape::from_detail(&detail, identity, remote_task_id)?;
        let document = self
            .transport
            .fetch_question_content(context, &identity.course_resource, &identity.group)
            .await?;
        parse_question_content(
            document.as_str(),
            remote_task_id,
            &shape.task_types,
            shape.question_count,
        )
    }

    fn store_attempt(
        &self,
        key: QuestionAttemptKey,
        questions: Vec<ParsedUaiQuestion>,
    ) -> ProviderResult<Vec<RemoteQuestionRef>> {
        let references = questions
            .iter()
            .map(ParsedUaiQuestion::reference)
            .collect::<ProviderResult<Vec<_>>>()?;
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| internal("UAI Question attempt cache lock is unavailable"))?;
        attempts.retain(|_, attempt| attempt.created_at.elapsed() < QUESTION_ATTEMPT_TTL);
        if attempts.len() >= MAX_ACTIVE_QUESTION_ATTEMPTS && !attempts.contains_key(&key) {
            let mut error = ProviderError::new(
                ProviderErrorKind::RateLimited,
                "UAI Question attempt cache is temporarily full",
            );
            error.retry_after_seconds = Some(1);
            return Err(error);
        }
        attempts.insert(
            key,
            CachedQuestionAttempt {
                created_at: Instant::now(),
                questions,
            },
        );
        Ok(references)
    }
}

impl fmt::Debug for UaiQuestionRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiQuestionRead")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .field("attempts", &"[REDACTED]")
            .finish()
    }
}

impl ProviderIdentity for UaiQuestionRead {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl QuestionInventoryCapability for UaiQuestionRead {
    async fn list_question_refs(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<RemoteQuestionRef>> {
        validate_context(context, &self.metadata)?;
        let identity = GroupIdentity::parse(remote_task_id)?;
        let questions = self
            .discover_questions(context, &identity, remote_task_id)
            .await?;
        self.store_attempt(QuestionAttemptKey::new(context, remote_task_id), questions)
    }
}

#[async_trait]
impl QuestionParseCapability for UaiQuestionRead {
    async fn parse_question(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        question: &RemoteQuestionRef,
    ) -> ProviderResult<Question> {
        validate_context(context, &self.metadata)?;
        GroupIdentity::parse(remote_task_id)?;
        question
            .validate()
            .map_err(|_| invalid_response("UAI Question reference is invalid"))?;
        if question.route_context.get("content_kind") != Some("encrypted_v3") {
            return Err(invalid_response(
                "UAI Question reference has a mismatched content kind",
            ));
        }
        let key = QuestionAttemptKey::new(context, remote_task_id);
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| internal("UAI Question attempt cache lock is unavailable"))?;
        let attempt = attempts.get(&key).ok_or_else(question_attempt_changed)?;
        if attempt.created_at.elapsed() >= QUESTION_ATTEMPT_TTL {
            attempts.remove(&key);
            return Err(question_attempt_changed());
        }
        let parsed = attempt
            .questions
            .iter()
            .find(|parsed| parsed.matches_reference(question))
            .cloned()
            .ok_or_else(question_attempt_changed)?;
        let is_last = usize::try_from(question.position)
            .is_ok_and(|position| position == attempt.questions.len());
        let normalized = parsed.to_question(task_id)?;
        if is_last {
            attempts.remove(&key);
        }
        Ok(normalized)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedUaiQuestion {
    remote_id: String,
    position: u32,
    kind: QuestionKind,
    stem: String,
    options: Vec<QuestionOption>,
    metadata_sanitized: Value,
}

impl ParsedUaiQuestion {
    fn matches_reference(&self, reference: &RemoteQuestionRef) -> bool {
        self.remote_id == reference.remote_id
            && self.position == reference.position
            && self.kind == reference.kind_hint
            && self.metadata_sanitized == reference.metadata_sanitized
    }

    fn reference(&self) -> ProviderResult<RemoteQuestionRef> {
        let reference = RemoteQuestionRef {
            remote_id: self.remote_id.clone(),
            position: self.position,
            kind_hint: self.kind,
            metadata_sanitized: self.metadata_sanitized.clone(),
            route_context: ProviderRouteContext::try_from_pairs([(
                "content_kind".to_owned(),
                "encrypted_v3".to_owned(),
            )])?,
        };
        reference
            .validate()
            .map_err(|_| invalid_response("UAI Question reference is invalid"))?;
        Ok(reference)
    }

    pub(crate) fn to_question(&self, task_id: TaskId) -> ProviderResult<Question> {
        let question = Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some(self.remote_id.clone()),
            kind: self.kind,
            stem: self.stem.clone(),
            options: self.options.clone(),
            attachments: Vec::new(),
            metadata_sanitized: self.metadata_sanitized.clone(),
            position: self.position,
        };
        question
            .validate()
            .map_err(|_| invalid_response("UAI normalized Question is invalid"))?;
        Ok(question)
    }
}

/// Decrypts and parses one donor-observed encrypted content response without
/// reading answer or submission fields.
///
/// # Errors
///
/// Returns a typed invalid-response/protocol-drift error for malformed framing,
/// padding, nested JSON, identity, counts or bounded Question semantics.
pub fn parse_question_content(
    document: &str,
    remote_task_id: &str,
    task_types: &[String],
    expected_count: Option<u32>,
) -> ProviderResult<Vec<ParsedUaiQuestion>> {
    if document.is_empty() || document.len() > MAX_QUESTION_DOCUMENT_BYTES {
        return Err(invalid_response(
            "UAI Question content is empty or exceeds the size limit",
        ));
    }
    GroupIdentity::parse(remote_task_id)?;
    if !supports_question_read(task_types, expected_count) {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI Question read does not support this Group Question shape",
        ));
    }
    let envelope: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI Question content response is not valid JSON"))?;
    let envelope = envelope
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Question content response is not an object"))?;
    if envelope.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(protocol_drift("UAI Question content read did not succeed"));
    }
    let encrypted = envelope
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI Question content response has no ciphertext"))?;
    let key_suffix = envelope
        .get("k")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI Question content response has no key suffix"))?;
    let plaintext = Zeroizing::new(decrypt_unipus_payload(
        encrypted,
        key_suffix,
        MAX_DECRYPTED_DOCUMENT_BYTES,
    )?);
    let outer = ZeroizingJsonValue::new(
        serde_json::from_slice(&plaintext)
            .map_err(|_| invalid_response("UAI decrypted Question wrapper is not valid JSON"))?,
    );
    let entries = outer
        .as_value()
        .as_array()
        .ok_or_else(|| protocol_drift("UAI decrypted Question wrapper is not an array"))?;
    if entries.is_empty() || entries.len() > MAX_QUESTIONS_PER_DOCUMENT {
        return Err(protocol_drift(
            "UAI decrypted Question wrapper has no bounded Question set",
        ));
    }
    if expected_count.is_some_and(|count| usize::try_from(count).ok() != Some(entries.len())) {
        return Err(protocol_drift(
            "UAI decrypted Question count does not match fresh Group detail",
        ));
    }

    let mut remote_ids = BTreeSet::new();
    let mut questions = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let position = u32::try_from(index + 1)
            .map_err(|_| invalid_response("UAI Question position exceeds the limit"))?;
        let task_type = task_types
            .get(index)
            .or_else(|| task_types.last())
            .ok_or_else(|| protocol_drift("UAI Group Task has no Question type"))?;
        let question = parse_question_entry(entry, position, task_type, remote_task_id)?;
        if !remote_ids.insert(question.remote_id.clone()) {
            return Err(protocol_drift(
                "UAI decrypted Question wrapper contains duplicate identity",
            ));
        }
        question.reference()?;
        questions.push(question);
    }
    Ok(questions)
}

fn parse_question_entry(
    entry: &Value,
    position: u32,
    task_type: &str,
    remote_task_id: &str,
) -> ProviderResult<ParsedUaiQuestion> {
    let entry = entry
        .as_object()
        .ok_or_else(|| protocol_drift("UAI decrypted Question entry is not an object"))?;
    let nested = entry
        .get("content")
        .ok_or_else(|| protocol_drift("UAI decrypted Question entry has no nested content"))?;
    let content = ZeroizingJsonValue::new(match nested {
        Value::String(content) => {
            if content.is_empty() || content.len() > MAX_INNER_CONTENT_BYTES {
                return Err(invalid_response(
                    "UAI nested Question content is empty or exceeds the size limit",
                ));
            }
            serde_json::from_str(content)
                .map_err(|_| invalid_response("UAI nested Question content is not valid JSON"))?
        }
        Value::Object(_) => {
            if serde_json::to_vec(nested)
                .map_or(true, |encoded| encoded.len() > MAX_INNER_CONTENT_BYTES)
            {
                return Err(invalid_response(
                    "UAI nested Question content is empty or exceeds the size limit",
                ));
            }
            nested.clone()
        }
        _ => {
            return Err(protocol_drift(
                "UAI decrypted Question entry has invalid nested content",
            ));
        }
    });
    let content = content
        .as_value()
        .as_object()
        .ok_or_else(|| protocol_drift("UAI nested Question content is not an object"))?;
    let remote_id = entry
        .get("id")
        .and_then(remote_identity)
        .or_else(|| content.get("id").and_then(remote_identity))
        .ok_or_else(|| protocol_drift("UAI Question has no explicit remote identity"))?;
    valid_question_identity(&remote_id)?;
    let stem = question_stem(content)?;
    let base_kind = audited_question_kind(task_type).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI Question read does not support this audited task type",
        )
    })?;
    let composite_children = choice_composite_children(content, base_kind)?;
    let kind = if composite_children.is_some() {
        QuestionKind::Composite
    } else {
        base_kind
    };
    let options = if kind == QuestionKind::Composite {
        Vec::new()
    } else {
        question_options(
            content,
            matches!(kind, QuestionKind::FillBlank | QuestionKind::Matching),
        )?
    };
    let judge_types = current_judge_types(content)?;
    if matches!(
        kind,
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice
    ) && options.len() < 2
    {
        return Err(protocol_drift(
            "UAI choice Question has no bounded option set",
        ));
    }
    let question = ParsedUaiQuestion {
        remote_id,
        position,
        kind,
        stem,
        options,
        metadata_sanitized: json!({
            "schema": "uai.encrypted-question.v1",
            "task_type": task_type,
            "remote_task_id": remote_task_id,
            "judge_types": judge_types,
            "composite_children": composite_children,
            "matching_lefts": if kind == QuestionKind::Matching {
                Some(matching_lefts(content)?)
            } else {
                None
            },
        }),
    };
    question.to_question(TaskId::new())?;
    Ok(question)
}

fn choice_composite_children(
    content: &Map<String, Value>,
    base_kind: QuestionKind,
) -> ProviderResult<Option<Vec<Value>>> {
    if !matches!(
        base_kind,
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice
    ) {
        return Ok(None);
    }
    let Some(children) = content.get("children").and_then(Value::as_array) else {
        return Ok(None);
    };
    if children.len() <= 1 {
        return Ok(None);
    }
    if children.len() > MAX_QUESTION_CHILDREN {
        return Err(invalid_response(
            "UAI composite choice children exceed the item limit",
        ));
    }
    children
        .iter()
        .map(|child| {
            let child = child
                .as_object()
                .ok_or_else(|| protocol_drift("UAI composite choice child is not an object"))?;
            let kind = child_choice_kind(child, base_kind)?;
            let options = child_options(child)?;
            if options.len() < 2 {
                return Err(protocol_drift(
                    "UAI composite choice child has no bounded option set",
                ));
            }
            Ok(json!({
                "kind": match kind {
                    QuestionKind::SingleChoice => "single_choice",
                    QuestionKind::MultipleChoice => "multiple_choice",
                    _ => unreachable!("child_choice_kind returns only choice kinds"),
                },
                "stem": (["quesText", "text"]
                    .into_iter()
                    .find_map(|key| child.get(key).and_then(Value::as_str))
                    .map(normalize_rich_text)
                    .filter(|value| !value.is_empty())),
                "options": options,
            }))
        })
        .collect::<ProviderResult<Vec<_>>>()
        .map(Some)
}

fn child_choice_kind(
    child: &Map<String, Value>,
    fallback: QuestionKind,
) -> ProviderResult<QuestionKind> {
    match child.get("replyType").and_then(Value::as_str) {
        Some("singlechoice" | "single-choice") => Ok(QuestionKind::SingleChoice),
        Some("multichoice") => Ok(QuestionKind::MultipleChoice),
        None => Ok(fallback),
        _ => Err(protocol_drift(
            "UAI composite choice child has an unsupported reply type",
        )),
    }
}

fn question_stem(content: &Map<String, Value>) -> ProviderResult<String> {
    let mut fragments = Vec::new();
    if let Some(text) = content
        .get("direction")
        .and_then(Value::as_object)
        .and_then(|direction| {
            ["text", "pcText"]
                .into_iter()
                .find_map(|key| direction.get(key).and_then(Value::as_str))
        })
    {
        fragments.push(normalize_rich_text(text));
    }
    for key in ["stem", "question", "title", "text"] {
        if let Some(text) = content.get(key).and_then(Value::as_str) {
            fragments.push(normalize_rich_text(text));
        }
    }
    if let Some(contents) = content.get("contents") {
        let contents = contents
            .as_array()
            .ok_or_else(|| protocol_drift("UAI Question contents field is not an array"))?;
        if contents.len() > 5_000 {
            return Err(invalid_response(
                "UAI Question contents field exceeds the item limit",
            ));
        }
        for item in contents {
            let item = item
                .as_object()
                .ok_or_else(|| protocol_drift("UAI Question contents contains a non-object"))?;
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                fragments.push(normalize_rich_text(text));
            }
        }
    }
    if let Some(children) = content.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| protocol_drift("UAI Question children field is not an array"))?;
        if children.len() > MAX_QUESTION_CHILDREN {
            return Err(invalid_response(
                "UAI Question children field exceeds the item limit",
            ));
        }
        for child in children {
            let child = child
                .as_object()
                .ok_or_else(|| protocol_drift("UAI Question children contains a non-object"))?;
            if let Some(text) = ["quesText", "text"]
                .into_iter()
                .find_map(|key| child.get(key).and_then(Value::as_str))
            {
                fragments.push(normalize_rich_text(text));
            }
        }
    }
    let stem = normalize_text(fragments.iter().map(String::as_str));
    if stem.is_empty() {
        return Err(protocol_drift("UAI Question has no bounded textual stem"));
    }
    Ok(stem)
}

fn matching_lefts(content: &Map<String, Value>) -> ProviderResult<Vec<String>> {
    let children = content
        .get("children")
        .and_then(Value::as_array)
        .filter(|children| !children.is_empty() && children.len() <= MAX_QUESTION_CHILDREN)
        .ok_or_else(|| protocol_drift("UAI matching Question has no bounded children"))?;
    let mut seen = BTreeSet::new();
    let mut lefts = Vec::with_capacity(children.len());
    for child in children {
        let child = child
            .as_object()
            .ok_or_else(|| protocol_drift("UAI matching Question child is not an object"))?;
        let left = ["quesText", "text"]
            .into_iter()
            .find_map(|key| child.get(key).and_then(Value::as_str))
            .map(normalize_rich_text)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| protocol_drift("UAI matching Question child has no left text"))?;
        if !seen.insert(left.clone()) {
            return Err(protocol_drift(
                "UAI matching Question contains duplicate left text",
            ));
        }
        lefts.push(left);
    }
    Ok(lefts)
}

fn question_options(
    content: &Map<String, Value>,
    allow_identical_repeats: bool,
) -> ProviderResult<Vec<QuestionOption>> {
    let Some(children) = content.get("children") else {
        return Ok(Vec::new());
    };
    let children = children
        .as_array()
        .ok_or_else(|| protocol_drift("UAI Question children field is not an array"))?;
    if children.len() > 256 {
        return Err(invalid_response(
            "UAI Question children field exceeds the item limit",
        ));
    }
    let mut identifiers = BTreeSet::new();
    let mut options = Vec::new();
    for child in children {
        let child = child
            .as_object()
            .ok_or_else(|| protocol_drift("UAI Question children contains a non-object"))?;
        for option in child_options(child)? {
            if !identifiers.insert(option.id.clone()) {
                if allow_identical_repeats
                    && options.iter().any(|existing: &QuestionOption| {
                        existing.id == option.id && existing.content == option.content
                    })
                {
                    continue;
                }
                return Err(protocol_drift(
                    "UAI Question contains duplicate option identity",
                ));
            }
            options.push(option);
        }
    }
    Ok(options)
}

fn child_options(child: &Map<String, Value>) -> ProviderResult<Vec<QuestionOption>> {
    let Some(values) = child.get("options") else {
        return Ok(Vec::new());
    };
    let values = values
        .as_array()
        .ok_or_else(|| protocol_drift("UAI Question options field is not an array"))?;
    if values.len() > MAX_OPTIONS_PER_CHILD {
        return Err(invalid_response(
            "UAI Question options field exceeds the item limit",
        ));
    }
    let mut identifiers = BTreeSet::new();
    values
        .iter()
        .map(|value| {
            let value = value
                .as_object()
                .ok_or_else(|| protocol_drift("UAI Question options contains a non-object"))?;
            let id = ["name", "id", "value"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(remote_identity))
                .ok_or_else(|| protocol_drift("UAI Question option has no identity"))?;
            valid_question_identity(&id)?;
            if !identifiers.insert(id.clone()) {
                return Err(protocol_drift(
                    "UAI Question child contains duplicate option identity",
                ));
            }
            let display = ["text", "content", "label", "name"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
                .map(normalize_rich_text)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| id.clone());
            Ok(QuestionOption {
                id,
                content: Some(display),
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
            })
        })
        .collect()
}

fn current_judge_types(content: &Map<String, Value>) -> ProviderResult<Option<Vec<Value>>> {
    let Some(children) = content.get("children") else {
        return Ok(None);
    };
    let children = children
        .as_array()
        .ok_or_else(|| protocol_drift("UAI Question children field is not an array"))?;
    if children.is_empty() || children.len() > MAX_QUESTION_CHILDREN {
        return Ok(None);
    }
    let module_type = optional_judge_type(content.get("type"))?;
    let mut result = Vec::with_capacity(children.len());
    for child in children {
        let child = child
            .as_object()
            .ok_or_else(|| protocol_drift("UAI Question children contains a non-object"))?;
        let Some(question_type) =
            optional_judge_type(child.get("type"))?.or_else(|| module_type.clone())
        else {
            return Ok(None);
        };
        let reply_type =
            optional_judge_type(child.get("replyType"))?.unwrap_or_else(|| "text-area".to_owned());
        result.push(json!({
            "question_type": question_type,
            "reply_type": reply_type,
        }));
    }
    Ok(Some(result))
}

fn optional_judge_type(value: Option<&Value>) -> ProviderResult<Option<String>> {
    value
        .map(|value| {
            bounded_judge_type(value)
                .ok_or_else(|| protocol_drift("UAI Question child judge type is invalid"))
        })
        .transpose()
}

fn bounded_judge_type(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_JUDGE_TYPE_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .map(str::to_owned)
}

fn remote_identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn normalize_text<'a>(fragments: impl IntoIterator<Item = &'a str>) -> String {
    fragments
        .into_iter()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_rich_text(value: &str) -> String {
    let fragment = Html::parse_fragment(value);
    normalize_text(fragment.root_element().text())
}

pub(crate) fn supports_question_read(task_types: &[String], question_count: Option<u32>) -> bool {
    question_count
        .and_then(|count| usize::try_from(count).ok())
        .is_some_and(|count| {
            (1..=MAX_QUESTIONS_PER_DOCUMENT).contains(&count)
                && (task_types.len() == 1 || task_types.len() == count)
        })
        && task_types
            .iter()
            .all(|value| supported_question_type(value))
}

fn supported_question_type(value: &str) -> bool {
    supports_audited_question_type(value)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct QuestionAttemptKey {
    account: asterism_domain::ProviderAccountId,
    correlation: String,
    remote_task: String,
}

impl QuestionAttemptKey {
    fn new(context: &ProviderContext, remote_task_id: &str) -> Self {
        Self {
            account: context.account_id,
            correlation: context.correlation_id.clone(),
            remote_task: remote_task_id.to_owned(),
        }
    }
}

#[derive(Debug)]
struct CachedQuestionAttempt {
    created_at: Instant,
    questions: Vec<ParsedUaiQuestion>,
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
        let course_resource = valid_component(
            components
                .next()
                .ok_or_else(|| protocol_drift("UAI Group Task identity is invalid"))?,
        )?;
        let unit = valid_component(
            components
                .next()
                .ok_or_else(|| protocol_drift("UAI Group Task identity is invalid"))?,
        )?;
        let group = valid_component(
            components
                .next()
                .ok_or_else(|| protocol_drift("UAI Group Task identity is invalid"))?,
        )?;
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

struct TaskQuestionShape {
    task_types: Vec<String>,
    question_count: Option<u32>,
}

impl TaskQuestionShape {
    fn from_detail(
        detail: &RemoteTaskDetail,
        identity: &GroupIdentity,
        remote_task_id: &str,
    ) -> ProviderResult<Self> {
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::QuestionInventory)
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::QuestionParse)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI Group Task does not advertise answer-free Question read",
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
                "UAI fresh Group Question detail does not match remote identity",
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
                    .filter(|value| supported_question_type(value))
                    .map(str::to_owned)
                    .ok_or_else(|| protocol_drift("UAI fresh Group has an unsupported task type"))
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        let question_count = task
            .get("question_count")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| protocol_drift("UAI fresh Group has an invalid question count"))
            })
            .transpose()?;
        if !supports_question_read(&task_types, question_count) {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI Group Task does not have a supported Question shape",
            ));
        }
        Ok(Self {
            task_types,
            question_count,
        })
    }
}

fn valid_component(value: &str) -> ProviderResult<String> {
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

fn valid_question_identity(value: &str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > 512
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(protocol_drift("UAI Question identity is invalid"));
    }
    Ok(())
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "UAI Question read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI Question read requires an authenticated session",
        ));
    }
    Ok(())
}

fn question_attempt_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "UAI Question attempt changed before parsing completed",
    )
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;
    use crate::{parse_course_context, parse_course_inventory, parse_task_inventory};

    const CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-multiple-choice.json");
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
    impl UaiQuestionTransport for FixtureTransport {
        async fn fetch_question_content(
            &self,
            _context: &ProviderContext,
            course_resource_id: &str,
            group_id: &str,
        ) -> ProviderResult<UaiQuestionDocument> {
            self.calls
                .lock()
                .unwrap()
                .push((course_resource_id.to_owned(), group_id.to_owned()));
            UaiQuestionDocument::try_new(CONTENT.to_owned())
        }
    }

    #[test]
    fn encrypted_content_becomes_answer_free_bounded_questions() {
        let questions = parse_question_content(
            CONTENT,
            "group:2001:unit-1:group-questions",
            &["multichoice".to_owned()],
            Some(1),
        )
        .unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].remote_id, "1001");
        assert_eq!(questions[0].position, 1);
        assert_eq!(questions[0].kind, QuestionKind::MultipleChoice);
        assert_eq!(
            questions[0].stem,
            "Choose every fitting option A bounded synthetic prompt"
        );
        assert_eq!(questions[0].options.len(), 2);
        assert_eq!(questions[0].options[0].id, "A");
        assert_eq!(questions[0].options[0].content.as_deref(), Some("Alpha"));
        assert_eq!(
            questions[0].metadata_sanitized["remote_task_id"],
            "group:2001:unit-1:group-questions"
        );
        let encoded = serde_json::to_string(&questions[0].reference().unwrap()).unwrap();
        assert!(!encoded.contains("unipus."));
        assert!(!encoded.contains("k1234567"));
        assert!(!encoded.contains("must_be_dropped"));
        assert!(!encoded.contains("answer"));
    }

    #[test]
    fn encrypted_mixed_content_preserves_exact_remote_question_order() {
        let questions = parse_question_content(
            MIXED_CONTENT,
            "group:2001:unit-1:group-mixed",
            &["multichoice".to_owned(), "short_answer".to_owned()],
            Some(2),
        )
        .unwrap();
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].remote_id, "1001");
        assert_eq!(questions[0].position, 1);
        assert_eq!(questions[0].kind, QuestionKind::MultipleChoice);
        assert_eq!(
            questions[0].metadata_sanitized["judge_types"][0],
            json!({"question_type": "basic", "reply_type": "multichoice"})
        );
        assert_eq!(questions[1].remote_id, "1002");
        assert_eq!(questions[1].position, 2);
        assert_eq!(questions[1].kind, QuestionKind::ShortAnswer);
        assert!(questions[1].options.is_empty());
        assert_eq!(
            questions[1].metadata_sanitized["judge_types"],
            json!([
                {"question_type": "basic", "reply_type": "text-area"},
                {"question_type": "basic", "reply_type": "text-area"},
            ])
        );
    }

    #[test]
    fn donor_fillblank_labels_map_to_typed_questions() {
        for task_type in [
            "material-banked-cloze",
            "basic-scoop-content-dropdown",
            "fillblank-scoop-dropdown",
        ] {
            let parsed = parse_question_entry(
                &json!({
                    "id": "2001",
                    "content": serde_json::to_string(&json!({
                        "type": task_type,
                        "direction": {"text": "Fill every blank"},
                        "children": [
                            {
                                "type": task_type,
                                "replyType": "bankedcloze",
                                "options": [
                                    {"name": "first", "text": "first"},
                                    {"name": "second", "text": "second"},
                                ],
                            },
                            {
                                "type": task_type,
                                "replyType": "bankedcloze",
                                "options": [
                                    {"name": "first", "text": "first"},
                                    {"name": "second", "text": "second"},
                                ],
                            },
                        ],
                    }))
                    .unwrap(),
                }),
                1,
                task_type,
                "group:2001:unit-1:group-fillblank",
            )
            .unwrap();
            assert_eq!(parsed.kind, QuestionKind::FillBlank);
            assert_eq!(parsed.options.len(), 2);
            assert_eq!(parsed.metadata_sanitized["task_type"], task_type);
            assert_eq!(
                parsed.metadata_sanitized["judge_types"][0]["reply_type"],
                "bankedcloze"
            );
        }
    }

    #[test]
    fn donor_scoop_content_preserves_matching_left_order() {
        let parsed = parse_question_entry(
            &json!({
                "id": "3001",
                "content": serde_json::to_string(&json!({
                    "type": "basic-scoop-content",
                    "direction": {"text": "Match each expression"},
                    "children": [
                        {"quesText": "first left", "type": "basic", "replyType": "scoop"},
                        {"quesText": "second left", "type": "basic", "replyType": "scoop"},
                    ],
                }))
                .unwrap(),
            }),
            1,
            "basic-scoop-content",
            "group:2001:unit-1:group-matching",
        )
        .unwrap();

        assert_eq!(parsed.kind, QuestionKind::Matching);
        assert_eq!(
            parsed.metadata_sanitized["matching_lefts"],
            json!(["first left", "second left"])
        );
        assert!(parsed.stem.contains("first left"));
        assert!(parsed.stem.contains("second left"));
    }

    #[test]
    fn donor_multi_child_choice_preserves_component_option_sets() {
        let parsed = parse_question_entry(
            &json!({
                "id": "8001",
                "content": serde_json::to_string(&json!({
                    "type": "multichoice",
                    "direction": {"text": "Answer both parts"},
                    "children": [
                        {
                            "quesText": "Choose one",
                            "type": "basic",
                            "replyType": "singlechoice",
                            "options": [
                                {"name": "A", "text": "First A"},
                                {"name": "B", "text": "First B"},
                            ],
                        },
                        {
                            "quesText": "Choose many",
                            "type": "basic",
                            "replyType": "multichoice",
                            "options": [
                                {"name": "A", "text": "Second A"},
                                {"name": "B", "text": "Second B"},
                            ],
                        },
                    ],
                }))
                .unwrap(),
            }),
            1,
            "multichoice",
            "group:2001:unit-1:group-composite-choice",
        )
        .unwrap();

        assert_eq!(parsed.kind, QuestionKind::Composite);
        assert!(parsed.options.is_empty());
        assert_eq!(
            parsed.metadata_sanitized["composite_children"][0]["kind"],
            "single_choice"
        );
        assert_eq!(
            parsed.metadata_sanitized["composite_children"][1]["kind"],
            "multiple_choice"
        );
        assert_eq!(
            parsed.metadata_sanitized["composite_children"][0]["stem"],
            "Choose one"
        );
        assert_eq!(
            parsed.metadata_sanitized["composite_children"][0]["options"][0]["id"],
            "A"
        );
        assert_eq!(
            parsed.metadata_sanitized["composite_children"][1]["options"][0]["content"],
            "Second A"
        );
    }

    #[test]
    fn current_donor_inline_object_content_is_supported() {
        let nested = json!({
            "type": "basic",
            "direction": {"pcText": "<p>Choose <strong>one</strong> answer</p>"},
            "children": [{
                "type": "basic",
                "replyType": "singlechoice",
                "quesText": "<span>Object-shaped content</span>",
                "options": [
                    {"name": "A", "text": "Alpha"},
                    {"name": "B", "text": "Beta"},
                ],
            }],
        });
        let inline = parse_question_entry(
            &json!({"id": "9001", "content": nested.clone()}),
            1,
            "single-choice",
            "group:2001:unit-1:group-object-content",
        )
        .unwrap();
        let encoded = parse_question_entry(
            &json!({
                "id": "9001",
                "content": serde_json::to_string(&nested).unwrap(),
            }),
            1,
            "single-choice",
            "group:2001:unit-1:group-object-content",
        )
        .unwrap();

        assert_eq!(inline, encoded);
        assert_eq!(inline.kind, QuestionKind::SingleChoice);
        assert_eq!(inline.stem, "Choose one answer Object-shaped content");
        assert_eq!(inline.options.len(), 2);
    }

    #[test]
    fn malformed_framing_padding_counts_and_types_fail_closed() {
        assert!(
            parse_question_content(
                &CONTENT.replacen("\"code\":0,", "", 1),
                "group:2001:unit-1:group-questions",
                &["multichoice".to_owned()],
                Some(1),
            )
            .is_err()
        );
        assert!(
            parse_question_content(
                &CONTENT.replace("unipus.", "changed."),
                "group:2001:unit-1:group-questions",
                &["multichoice".to_owned()],
                Some(1),
            )
            .is_err()
        );
        let mut corrupted = CONTENT.to_owned();
        let ciphertext_end = corrupted.find("\",\"k\"").unwrap();
        corrupted.replace_range(ciphertext_end - 1..ciphertext_end, "0");
        assert!(
            parse_question_content(
                &corrupted,
                "group:2001:unit-1:group-questions",
                &["multichoice".to_owned()],
                Some(1),
            )
            .is_err()
        );
        assert!(
            parse_question_content(
                CONTENT,
                "group:2001:unit-1:group-questions",
                &["multichoice".to_owned()],
                Some(2),
            )
            .is_err()
        );
        assert!(
            parse_question_content(
                CONTENT,
                "group:2001:unit-1:group-questions",
                &["multichoice".to_owned(), "short_answer".to_owned()],
                Some(1),
            )
            .is_err()
        );
        assert!(supports_question_read(&["multichoice".to_owned()], Some(3)));
        assert!(supports_question_read(
            &[
                "multichoice".to_owned(),
                "short_answer".to_owned(),
                "single-choice".to_owned(),
            ],
            Some(3),
        ));
        assert!(!supports_question_read(
            &["multichoice".to_owned(), "short_answer".to_owned()],
            Some(3),
        ));
        assert!(!supports_question_read(
            &["multichoice".to_owned()],
            Some(5_001),
        ));
        assert!(
            parse_question_entry(
                &json!({
                    "content": serde_json::to_string(&json!({
                        "stem": "Synthetic prompt",
                        "children": [{
                            "options": [
                                {"name": "A", "text": "Alpha"},
                                {"name": "B", "text": "Beta"},
                            ]
                        }],
                    }))
                    .unwrap(),
                }),
                1,
                "multichoice",
                "group:2001:unit-1:group-questions",
            )
            .is_err()
        );
        assert!(
            parse_question_content(
                CONTENT,
                "group:2001:unit-1:group-questions",
                &["video-point-read".to_owned()],
                Some(1),
            )
            .is_err()
        );
    }

    #[test]
    fn unsafe_current_judge_type_fails_closed() {
        assert!(
            parse_question_entry(
                &json!({
                    "id": "1001",
                    "content": serde_json::to_string(&json!({
                        "type": "unsafe/type",
                        "stem": "Synthetic prompt",
                        "children": [{
                            "options": [
                                {"name": "A", "text": "Alpha"},
                                {"name": "B", "text": "Beta"},
                            ]
                        }],
                    }))
                    .unwrap(),
                }),
                1,
                "multichoice",
                "group:2001:unit-1:group-questions",
            )
            .is_err()
        );
    }

    #[test]
    fn attempt_key_scopes_ephemeral_content_to_account_and_correlation() {
        let first = QuestionAttemptKey::new(&provider_context("correlation-a"), "group:1:u:g");
        let second = QuestionAttemptKey::new(&provider_context("correlation-b"), "group:1:u:g");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn capability_rechecks_fresh_group_and_consumes_one_ephemeral_attempt() {
        let transport = Arc::new(FixtureTransport::default());
        let reader = UaiQuestionRead::try_new(
            Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
            }),
            transport.clone(),
        )
        .unwrap();
        let context = provider_context("question-attempt");
        let remote_task_id = "group:2001:unit-1:group-1";
        let references = reader
            .list_question_refs(&context, remote_task_id)
            .await
            .unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            &[("2001".to_owned(), "group-1".to_owned())]
        );
        let task_id = TaskId::new();
        let question = reader
            .parse_question(&context, task_id, remote_task_id, &references[0])
            .await
            .unwrap();
        assert_eq!(question.task_id, task_id);
        assert_eq!(question.remote_question_id.as_deref(), Some("1001"));
        assert_eq!(question.kind, QuestionKind::MultipleChoice);
        assert_eq!(
            reader
                .parse_question(&context, task_id, remote_task_id, &references[0])
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    fn provider_context(correlation: &str) -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            correlation_id: correlation.to_owned(),
            credential_refs: vec![SecretId::new()],
        }
    }
}
