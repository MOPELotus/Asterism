use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aes::{
    Aes128,
    cipher::{Array, BlockCipherDecrypt, KeyInit},
};
use asterism_domain::{Question, QuestionId, QuestionKind, QuestionOption, TaskCapability, TaskId};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, ProviderRouteContext, QuestionInventoryCapability, QuestionParseCapability,
    RemoteQuestionRef, RemoteTaskDetail, TaskDetailCapability,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use zeroize::Zeroize;

use crate::metadata::development_metadata;

const ENCRYPTED_PREFIX: &str = "unipus.";
const AES_KEY_PREFIX: &[u8; 8] = b"1a2b3c4d";
const AES_BLOCK_BYTES: usize = 16;
const MAX_QUESTION_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_DECRYPTED_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_INNER_CONTENT_BYTES: usize = 1_024 * 1_024;
const MAX_QUESTIONS_PER_DOCUMENT: usize = 5_000;
const MAX_ACTIVE_QUESTION_ATTEMPTS: usize = 128;
const QUESTION_ATTEMPT_TTL: Duration = Duration::from_mins(5);
const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;

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
            &identity.group,
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

    fn to_question(&self, task_id: TaskId) -> ProviderResult<Question> {
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
    group_id: &str,
    task_types: &[String],
    expected_count: Option<u32>,
) -> ProviderResult<Vec<ParsedUaiQuestion>> {
    if document.is_empty() || document.len() > MAX_QUESTION_DOCUMENT_BYTES {
        return Err(invalid_response(
            "UAI Question content is empty or exceeds the size limit",
        ));
    }
    valid_component(group_id)?;
    if task_types.is_empty()
        || !task_types
            .iter()
            .all(|value| supported_question_type(value))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "UAI Question read does not support this Group task type",
        ));
    }
    let envelope: Value = serde_json::from_str(document)
        .map_err(|_| invalid_response("UAI Question content response is not valid JSON"))?;
    let envelope = envelope
        .as_object()
        .ok_or_else(|| protocol_drift("UAI Question content response is not an object"))?;
    if envelope
        .get("code")
        .is_some_and(|value| value.as_i64() != Some(0))
    {
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
    let mut plaintext = decrypt_content(encrypted, key_suffix)?;
    let outer: Value = serde_json::from_slice(&plaintext)
        .map_err(|_| invalid_response("UAI decrypted Question wrapper is not valid JSON"))?;
    plaintext.zeroize();
    let entries = outer
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
        let question = parse_question_entry(entry, group_id, position, task_type)?;
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
    group_id: &str,
    position: u32,
    task_type: &str,
) -> ProviderResult<ParsedUaiQuestion> {
    let entry = entry
        .as_object()
        .ok_or_else(|| protocol_drift("UAI decrypted Question entry is not an object"))?;
    let content = entry
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_drift("UAI decrypted Question entry has no nested content"))?;
    if content.is_empty() || content.len() > MAX_INNER_CONTENT_BYTES {
        return Err(invalid_response(
            "UAI nested Question content is empty or exceeds the size limit",
        ));
    }
    let content: Value = serde_json::from_str(content)
        .map_err(|_| invalid_response("UAI nested Question content is not valid JSON"))?;
    let content = content
        .as_object()
        .ok_or_else(|| protocol_drift("UAI nested Question content is not an object"))?;
    let remote_id = entry
        .get("id")
        .and_then(remote_identity)
        .or_else(|| content.get("id").and_then(remote_identity))
        .unwrap_or_else(|| format!("{group_id}:question:{position}"));
    valid_question_identity(&remote_id)?;
    let stem = question_stem(content)?;
    let options = question_options(content)?;
    let kind = question_kind(task_type);
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
        }),
    };
    question.to_question(TaskId::new())?;
    Ok(question)
}

fn decrypt_content(encrypted: &str, key_suffix: &str) -> ProviderResult<Vec<u8>> {
    let hexadecimal = encrypted
        .strip_prefix(ENCRYPTED_PREFIX)
        .ok_or_else(|| protocol_drift("UAI Question ciphertext has an unknown prefix"))?;
    if hexadecimal.is_empty()
        || !hexadecimal.len().is_multiple_of(AES_BLOCK_BYTES * 2)
        || hexadecimal.len() / 2 > MAX_DECRYPTED_DOCUMENT_BYTES
    {
        return Err(invalid_response(
            "UAI Question ciphertext has an invalid bounded length",
        ));
    }
    let suffix = key_suffix.as_bytes();
    if suffix.len() != 8 || !suffix.iter().all(u8::is_ascii_graphic) {
        return Err(protocol_drift(
            "UAI Question content key suffix has an invalid shape",
        ));
    }
    let mut key = [0_u8; AES_BLOCK_BYTES];
    key[..AES_KEY_PREFIX.len()].copy_from_slice(AES_KEY_PREFIX);
    key[AES_KEY_PREFIX.len()..].copy_from_slice(suffix);
    let cipher = Aes128::new(&Array::from(key));
    key.zeroize();
    let mut decoded = decode_hex(hexadecimal)?;
    for chunk in decoded.chunks_exact_mut(AES_BLOCK_BYTES) {
        let mut block_bytes = [0_u8; AES_BLOCK_BYTES];
        block_bytes.copy_from_slice(chunk);
        let mut block = Array::from(block_bytes);
        block_bytes.zeroize();
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
        block.zeroize();
    }
    remove_padding(&mut decoded)?;
    if decoded.is_empty() || decoded.len() > MAX_DECRYPTED_DOCUMENT_BYTES {
        decoded.zeroize();
        return Err(invalid_response(
            "UAI decrypted Question wrapper has an invalid bounded length",
        ));
    }
    Ok(decoded)
}

fn decode_hex(value: &str) -> ProviderResult<Vec<u8>> {
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])
            .ok_or_else(|| invalid_response("UAI Question ciphertext is not hexadecimal"))?;
        let low = hex_nibble(pair[1])
            .ok_or_else(|| invalid_response("UAI Question ciphertext is not hexadecimal"))?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn remove_padding(value: &mut Vec<u8>) -> ProviderResult<()> {
    let Some(&last) = value.last() else {
        return Err(invalid_response("UAI decrypted Question wrapper is empty"));
    };
    let padding = usize::from(last);
    if (1..=AES_BLOCK_BYTES).contains(&padding)
        && value.len() >= padding
        && value[value.len() - padding..]
            .iter()
            .all(|byte| usize::from(*byte) == padding)
    {
        value.truncate(value.len() - padding);
        return Ok(());
    }
    let original = value.len();
    while value.last() == Some(&0) {
        value.pop();
    }
    if value.len() == original {
        return Err(invalid_response(
            "UAI decrypted Question wrapper has invalid padding",
        ));
    }
    Ok(())
}

fn question_stem(content: &Map<String, Value>) -> ProviderResult<String> {
    let mut fragments = Vec::new();
    if let Some(text) = content
        .get("direction")
        .and_then(Value::as_object)
        .and_then(|direction| direction.get("text"))
        .and_then(Value::as_str)
    {
        fragments.push(text);
    }
    for key in ["stem", "question", "title", "text"] {
        if let Some(text) = content.get(key).and_then(Value::as_str) {
            fragments.push(text);
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
                fragments.push(text);
            }
        }
    }
    let stem = normalize_text(fragments);
    if stem.is_empty() {
        return Err(protocol_drift("UAI Question has no bounded textual stem"));
    }
    Ok(stem)
}

fn question_options(content: &Map<String, Value>) -> ProviderResult<Vec<QuestionOption>> {
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
        let Some(values) = child.get("options") else {
            continue;
        };
        let values = values
            .as_array()
            .ok_or_else(|| protocol_drift("UAI Question options field is not an array"))?;
        for value in values {
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
                    "UAI Question contains duplicate option identity",
                ));
            }
            let display = ["text", "content", "label", "name"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
                .map(|value| normalize_text([value]))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| id.clone());
            options.push(QuestionOption {
                id,
                content: Some(display),
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
            });
        }
    }
    Ok(options)
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

const fn question_kind(task_type: &str) -> QuestionKind {
    match task_type.as_bytes() {
        b"single-choice" => QuestionKind::SingleChoice,
        b"multichoice" => QuestionKind::MultipleChoice,
        b"short_answer" => QuestionKind::ShortAnswer,
        _ => QuestionKind::Unknown,
    }
}

pub(crate) fn supports_question_read(task_types: &[String], question_count: Option<u32>) -> bool {
    question_count.is_some_and(|value| value > 0)
        && !task_types.is_empty()
        && task_types
            .iter()
            .all(|value| supported_question_type(value))
}

fn supported_question_type(value: &str) -> bool {
    matches!(value, "single-choice" | "multichoice" | "short_answer")
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
            "group-questions",
            &["multichoice".to_owned()],
            Some(1),
        )
        .unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].remote_id, "question-1");
        assert_eq!(questions[0].position, 1);
        assert_eq!(questions[0].kind, QuestionKind::MultipleChoice);
        assert_eq!(
            questions[0].stem,
            "Choose every fitting option A bounded synthetic prompt"
        );
        assert_eq!(questions[0].options.len(), 2);
        assert_eq!(questions[0].options[0].id, "A");
        assert_eq!(questions[0].options[0].content.as_deref(), Some("Alpha"));
        let encoded = serde_json::to_string(&questions[0].reference().unwrap()).unwrap();
        assert!(!encoded.contains("unipus."));
        assert!(!encoded.contains("k1234567"));
        assert!(!encoded.contains("must_be_dropped"));
        assert!(!encoded.contains("answer"));
    }

    #[test]
    fn malformed_framing_padding_counts_and_types_fail_closed() {
        assert!(
            parse_question_content(
                &CONTENT.replace("unipus.", "changed."),
                "group-questions",
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
                "group-questions",
                &["multichoice".to_owned()],
                Some(1),
            )
            .is_err()
        );
        assert!(
            parse_question_content(
                CONTENT,
                "group-questions",
                &["multichoice".to_owned()],
                Some(2),
            )
            .is_err()
        );
        assert!(
            parse_question_content(
                CONTENT,
                "group-questions",
                &["video-point-read".to_owned()],
                Some(1),
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
        assert_eq!(question.remote_question_id.as_deref(), Some("question-1"));
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
