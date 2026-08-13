use std::{collections::BTreeSet, fmt, fmt::Write};

use asterism_domain::{Question, QuestionId, QuestionKind, QuestionOption, TaskId};
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, ProviderRouteContext, RemoteQuestionRef,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

const MAX_TOPIC_CODE_BYTES: usize = 4_096;
const MAX_STEM_BYTES: usize = 64 * 1_024;
const MAX_OPTION_BYTES: usize = 32 * 1_024;
const MAX_ANSWER_TAG_BYTES: usize = 256;
const MAX_OPTIONS: usize = 256;
const MAX_RELATIONS: usize = 256;
const MAX_POSITION: u32 = 100_000;
const MAX_REMOTE_TOPIC_TOTAL: u32 = 100_000;

/// Sanitized progress counters returned with the donor's current attempt
/// payload. These counters are remote observations and deliberately remain
/// distinct from the local durable state-machine position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CidarenAttemptProgress {
    completed: u32,
    total: u32,
}

impl CidarenAttemptProgress {
    pub const fn completed(self) -> u32 {
        self.completed
    }

    pub const fn total(self) -> u32 {
        self.total
    }
}

/// One decoded current Cidaren question bound to the remote attempt token.
///
/// The one-time `topic_code` is deliberately absent from the normalized
/// Question and Debug output. It enters only the ephemeral, redacted and
/// zeroizing route context used by the future durable attempt boundary.
pub struct ParsedCidarenAttemptQuestion {
    topic_code: Zeroizing<String>,
    remote_id: String,
    kind: QuestionKind,
    stem: String,
    options: Vec<QuestionOption>,
    metadata_sanitized: Value,
    position: u32,
    remote_progress: Option<CidarenAttemptProgress>,
}

/// One decoded donor attempt step. Reading cards are executable advance stages
/// rather than fake Questions; ordinary assessment steps retain the strict
/// Question model.
pub enum ParsedCidarenAttemptStep {
    Question(ParsedCidarenAttemptQuestion),
    ReadingCard(ParsedCidarenReadingCard),
}

impl fmt::Debug for ParsedCidarenAttemptStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Question(question) => formatter.debug_tuple("Question").field(question).finish(),
            Self::ReadingCard(card) => formatter.debug_tuple("ReadingCard").field(card).finish(),
        }
    }
}

/// Ephemeral mode-0 reading-card stage. Its rotating topic code is available
/// only through redacted route context for the durable advance mutation.
pub struct ParsedCidarenReadingCard {
    topic_code: Zeroizing<String>,
    remote_id: String,
    stem_sanitized: String,
    position: u32,
    remote_progress: Option<CidarenAttemptProgress>,
}

impl ParsedCidarenReadingCard {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used by the Provider-private lifecycle awaiting durable QuestionSession integration"
        )
    )]
    pub(crate) fn topic_code(&self) -> &str {
        self.topic_code.as_str()
    }

    /// Produces a scan-local route binding for `SubmitAnswerAndSave` without
    /// persisting the one-time topic code.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` if the route context exceeds Core bounds.
    pub fn route_context(&self) -> ProviderResult<ProviderRouteContext> {
        ProviderRouteContext::try_from_pairs([
            ("cidaren.topic_code".to_owned(), self.topic_code.to_string()),
            ("cidaren.reading_card_id".to_owned(), self.remote_id.clone()),
        ])
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn stem_sanitized(&self) -> &str {
        &self.stem_sanitized
    }

    pub const fn position(&self) -> u32 {
        self.position
    }

    pub const fn remote_progress(&self) -> Option<CidarenAttemptProgress> {
        self.remote_progress
    }
}

impl fmt::Debug for ParsedCidarenReadingCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedCidarenReadingCard")
            .field("topic_code", &"[REDACTED]")
            .field("remote_id", &self.remote_id)
            .field("position", &self.position)
            .field("remote_progress", &self.remote_progress)
            .finish_non_exhaustive()
    }
}

impl Drop for ParsedCidarenReadingCard {
    fn drop(&mut self) {
        self.remote_id.zeroize();
        self.stem_sanitized.zeroize();
    }
}

/// Parses a decoded `StartAnswer`/advance payload into either a real Question
/// or the donor's mode-0 reading-card advance stage.
///
/// # Errors
///
/// Returns typed errors for malformed, unbounded or unaudited attempt modes.
pub fn parse_attempt_step(
    payload: &Value,
    remote_task_id: &str,
    position: u32,
) -> ProviderResult<ParsedCidarenAttemptStep> {
    let mode = payload
        .as_object()
        .and_then(|object| object.get("topic_mode"))
        .and_then(Value::as_i64)
        .ok_or_else(|| protocol_drift("Cidaren attempt payload has no topic mode"))?;
    if mode == 0 {
        parse_reading_card(payload, remote_task_id, position)
            .map(ParsedCidarenAttemptStep::ReadingCard)
    } else {
        parse_attempt_question(payload, remote_task_id, position)
            .map(ParsedCidarenAttemptStep::Question)
    }
}

impl ParsedCidarenAttemptQuestion {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used by the Provider-private lifecycle awaiting durable QuestionSession integration"
        )
    )]
    pub(crate) fn topic_code(&self) -> &str {
        self.topic_code.as_str()
    }

    /// Produces a bounded scan-local reference with the one-time attempt token
    /// isolated in non-serialized route context.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` when the resulting reference violates Core's
    /// bounded question-reference contract.
    pub fn question_ref(&self) -> ProviderResult<RemoteQuestionRef> {
        let reference = RemoteQuestionRef {
            remote_id: self.remote_id.clone(),
            position: self.position,
            kind_hint: self.kind,
            metadata_sanitized: self.metadata_sanitized.clone(),
            route_context: ProviderRouteContext::try_from_pairs([
                ("cidaren.topic_code".to_owned(), self.topic_code.to_string()),
                ("cidaren.question_id".to_owned(), self.remote_id.clone()),
            ])?,
        };
        reference
            .validate()
            .map_err(|_| invalid_response("Cidaren Question reference is invalid"))?;
        Ok(reference)
    }

    /// Converts the sanitized shape into a persisted Core Question. The
    /// account-bound `topic_code` is never copied into the result.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` if the normalized Question is not bounded.
    pub fn to_question(&self, task_id: TaskId) -> ProviderResult<Question> {
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
            .map_err(|_| invalid_response("Cidaren normalized Question is invalid"))?;
        Ok(question)
    }

    pub const fn remote_progress(&self) -> Option<CidarenAttemptProgress> {
        self.remote_progress
    }
}

impl fmt::Debug for ParsedCidarenAttemptQuestion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedCidarenAttemptQuestion")
            .field("topic_code", &"[REDACTED]")
            .field("remote_id", &self.remote_id)
            .field("kind", &self.kind)
            .field("position", &self.position)
            .field("remote_progress", &self.remote_progress)
            .finish_non_exhaustive()
    }
}

impl Drop for ParsedCidarenAttemptQuestion {
    fn drop(&mut self) {
        self.remote_id.zeroize();
        self.stem.zeroize();
        zeroize_json(&mut self.metadata_sanitized);
        for option in &mut self.options {
            option.id.zeroize();
            if let Some(content) = &mut option.content {
                content.zeroize();
            }
            zeroize_json(&mut option.metadata_sanitized);
        }
    }
}

/// Parses one already decoded `StartAnswer`/advance payload without resolving
/// an answer or performing any mutation.
///
/// Donor-observed topic modes are mapped as follows: 11/13/15-18/21-22 and
/// 41-44 are single-choice; 31 is matching; 32 and 51-54 are text answers.
/// Mode 0 is a reading card rather than a Question and remains a distinct task
/// execution stage. Unknown modes fail closed.
///
/// # Errors
///
/// Returns typed `UnsupportedTask`, `ProtocolDrift` or `InvalidResponse` errors for
/// unknown modes, malformed semantics, duplicate tags or exceeded bounds.
pub fn parse_attempt_question(
    payload: &Value,
    remote_task_id: &str,
    position: u32,
) -> ProviderResult<ParsedCidarenAttemptQuestion> {
    if position == 0 || position > MAX_POSITION || !valid_remote_task_id(remote_task_id) {
        return Err(protocol_drift(
            "Cidaren attempt Question binding is invalid",
        ));
    }
    let object = payload
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren attempt payload is not an object"))?;
    let remote_progress = parse_remote_progress(object)?;
    let mut topic_code =
        required_text(object.get("topic_code"), MAX_TOPIC_CODE_BYTES, "topic code")?;
    let mode = object
        .get("topic_mode")
        .and_then(Value::as_i64)
        .ok_or_else(|| protocol_drift("Cidaren attempt payload has no topic mode"))?;
    let kind = question_kind(mode).inspect_err(|_| topic_code.zeroize())?;
    let stem_object = object
        .get("stem")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Cidaren attempt payload has no stem object"))?;
    let stem = parse_stem(stem_object).inspect_err(|_| topic_code.zeroize())?;
    let options =
        parse_options(object.get("options"), kind).inspect_err(|_| topic_code.zeroize())?;
    let metadata_sanitized =
        sanitized_metadata(object, remote_task_id, mode, kind, &stem, &options)
            .inspect_err(|_| topic_code.zeroize())?;
    let remote_id = question_remote_id(mode, &stem, &options, &metadata_sanitized)
        .inspect_err(|_| topic_code.zeroize())?;
    let parsed = ParsedCidarenAttemptQuestion {
        topic_code: Zeroizing::new(topic_code),
        remote_id,
        kind,
        stem,
        options,
        metadata_sanitized,
        position,
        remote_progress,
    };
    // Validate through both public contracts before handing the ephemeral
    // attempt material to a caller.
    parsed.question_ref()?;
    parsed.to_question(TaskId::new())?;
    Ok(parsed)
}

fn parse_reading_card(
    payload: &Value,
    remote_task_id: &str,
    position: u32,
) -> ProviderResult<ParsedCidarenReadingCard> {
    if position == 0 || position > MAX_POSITION || !valid_remote_task_id(remote_task_id) {
        return Err(protocol_drift("Cidaren reading-card binding is invalid"));
    }
    let object = payload
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren reading-card payload is not an object"))?;
    let remote_progress = parse_remote_progress(object)?;
    if object.get("topic_mode").and_then(Value::as_i64) != Some(0) {
        return Err(protocol_drift(
            "Cidaren reading-card parser received another topic mode",
        ));
    }
    match object.get("options") {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) if values.is_empty() => {}
        _ => {
            return Err(protocol_drift(
                "Cidaren reading-card payload unexpectedly contains answer options",
            ));
        }
    }
    let topic_code = Zeroizing::new(required_text(
        object.get("topic_code"),
        MAX_TOPIC_CODE_BYTES,
        "topic code",
    )?);
    let stem_object = object
        .get("stem")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Cidaren reading-card payload has no stem object"))?;
    let stem_sanitized = parse_stem(stem_object)?;
    let material = serde_json::to_vec(&json!({
        "remote_task_id": remote_task_id,
        "topic_mode": 0,
        "stem": stem_sanitized,
    }))
    .map_err(|_| invalid_response("Cidaren reading-card identity cannot be encoded"))?;
    let remote_id = format!("reading-card:{:x}", Sha256::digest(material));
    let card = ParsedCidarenReadingCard {
        topic_code,
        remote_id,
        stem_sanitized,
        position,
        remote_progress,
    };
    card.route_context()?;
    Ok(card)
}

fn question_kind(topic_mode: i64) -> ProviderResult<QuestionKind> {
    match topic_mode {
        0 => Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Cidaren topic mode 0 is a reading-card execution stage",
        )),
        11 | 13 | 15..=18 | 21 | 22 | 41..=44 => Ok(QuestionKind::SingleChoice),
        31 => Ok(QuestionKind::Matching),
        32 | 51..=54 => Ok(QuestionKind::ShortAnswer),
        _ => Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Cidaren attempt uses an unaudited topic mode",
        )),
    }
}

fn parse_remote_progress(
    object: &Map<String, Value>,
) -> ProviderResult<Option<CidarenAttemptProgress>> {
    let completed = optional_remote_counter(object.get("topic_done_num"), "completed")?;
    let total = optional_remote_counter(object.get("topic_total"), "total")?;
    match total {
        None | Some(0) => match completed {
            None | Some(0) => Ok(None),
            Some(completed) => Err(protocol_drift(format!(
                "Cidaren attempt reports {completed} completed topics without a total"
            ))),
        },
        Some(total) => {
            let completed = completed.unwrap_or(0);
            if completed > total {
                return Err(protocol_drift(
                    "Cidaren attempt completed-topic count exceeds its total",
                ));
            }
            Ok(Some(CidarenAttemptProgress { completed, total }))
        }
    }
}

fn optional_remote_counter(
    value: Option<&Value>,
    label: &'static str,
) -> ProviderResult<Option<u32>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value <= MAX_REMOTE_TOPIC_TOTAL)
            .map(Some)
            .ok_or_else(|| {
                protocol_drift(format!("Cidaren attempt {label}-topic count is invalid"))
            }),
        _ => Err(protocol_drift(format!(
            "Cidaren attempt {label}-topic count is invalid"
        ))),
    }
}

fn parse_stem(stem: &Map<String, Value>) -> ProviderResult<String> {
    let content = optional_text(stem.get("content"), MAX_STEM_BYTES, "stem content")?;
    let remark = match stem.get("remark") {
        None | Some(Value::Null | Value::Array(_)) => None,
        Some(Value::String(value)) if value.is_empty() => None,
        Some(Value::String(value)) => Some(
            valid_optional_text(value, MAX_STEM_BYTES)
                .then(|| normalize_text(value))
                .ok_or_else(|| protocol_drift("Cidaren Question stem remark is invalid"))?,
        ),
        Some(_) => {
            return Err(protocol_drift(
                "Cidaren Question stem remark has an unknown shape",
            ));
        }
    };
    let stem = match (content, remark) {
        (Some(content), Some(remark)) if content != remark => format!("{content} — {remark}"),
        (Some(content), _) => content,
        (None, Some(remark)) => remark,
        (None, None) => {
            return Err(protocol_drift(
                "Cidaren Question stem contains no bounded text",
            ));
        }
    };
    if stem.len() > MAX_STEM_BYTES || stem.chars().any(char::is_control) {
        return Err(invalid_response("Cidaren Question stem exceeds the limit"));
    }
    Ok(stem)
}

fn parse_options(value: Option<&Value>, kind: QuestionKind) -> ProviderResult<Vec<QuestionOption>> {
    let entries = match value {
        None | Some(Value::Null) if kind == QuestionKind::ShortAnswer => return Ok(Vec::new()),
        Some(Value::Array(entries)) => entries,
        _ => {
            return Err(protocol_drift(
                "Cidaren Question options have an unknown shape",
            ));
        }
    };
    if entries.len() > MAX_OPTIONS || (entries.is_empty() && kind != QuestionKind::ShortAnswer) {
        return Err(invalid_response("Cidaren Question option count is invalid"));
    }
    let mut options = Vec::new();
    let mut ids = BTreeSet::new();
    for (top_level_index, entry) in entries.iter().enumerate() {
        let entry = entry
            .as_object()
            .ok_or_else(|| protocol_drift("Cidaren Question contains a non-object option"))?;
        let parent_content =
            required_text(entry.get("content"), MAX_OPTION_BYTES, "option content")?;
        let parent_tag = answer_tag(entry.get("answer_tag"))?;
        let sub_options = match entry.get("sub_options") {
            None | Some(Value::Null) => None,
            Some(Value::Array(values)) if values.is_empty() => None,
            Some(Value::Array(values)) if values.len() <= MAX_OPTIONS => Some(values),
            _ => {
                return Err(protocol_drift(
                    "Cidaren Question sub-options have an unknown shape",
                ));
            }
        };
        if let Some(sub_options) = sub_options {
            let parent_wire = parent_tag.strip_prefix("s:").ok_or_else(|| {
                protocol_drift("Cidaren nested option has a non-string parent tag")
            })?;
            for sub_option in sub_options {
                let sub_option = sub_option.as_object().ok_or_else(|| {
                    protocol_drift("Cidaren Question contains a non-object sub-option")
                })?;
                let content = required_text(
                    sub_option.get("content"),
                    MAX_OPTION_BYTES,
                    "sub-option content",
                )?;
                let child_tag = raw_answer_tag(sub_option.get("answer_tag"))?;
                let combined = format!("s:{parent_wire}{child_tag}");
                push_option(
                    &mut options,
                    &mut ids,
                    combined,
                    format!("{parent_content} — {content}"),
                    OptionSemantics {
                        nested: true,
                        top_level_index,
                        top_level_content: &parent_content,
                        wire_content: &content,
                        parent_answer_id: &parent_tag,
                    },
                )?;
            }
        } else {
            let answer_id = parent_tag.clone();
            push_option(
                &mut options,
                &mut ids,
                answer_id,
                parent_content.clone(),
                OptionSemantics {
                    nested: false,
                    top_level_index,
                    top_level_content: &parent_content,
                    wire_content: &parent_content,
                    parent_answer_id: &parent_tag,
                },
            )?;
        }
    }
    if options.len() > MAX_OPTIONS || (options.is_empty() && kind != QuestionKind::ShortAnswer) {
        return Err(invalid_response(
            "Cidaren flattened Question options exceed the limit",
        ));
    }
    Ok(options)
}

fn push_option(
    options: &mut Vec<QuestionOption>,
    ids: &mut BTreeSet<String>,
    id: String,
    content: String,
    semantics: OptionSemantics<'_>,
) -> ProviderResult<()> {
    if options.len() >= MAX_OPTIONS || id.len() > MAX_ANSWER_TAG_BYTES || !ids.insert(id.clone()) {
        return Err(protocol_drift(
            "Cidaren Question contains a duplicate or oversized answer tag",
        ));
    }
    options.push(QuestionOption {
        id,
        content: Some(content),
        attachments: Vec::new(),
        metadata_sanitized: json!({
            "nested": semantics.nested,
            "top_level_index": semantics.top_level_index,
            "top_level_content": semantics.top_level_content,
            "wire_content": semantics.wire_content,
            "parent_answer_id": semantics.parent_answer_id,
        }),
    });
    Ok(())
}

#[derive(Clone, Copy)]
struct OptionSemantics<'a> {
    nested: bool,
    top_level_index: usize,
    top_level_content: &'a str,
    wire_content: &'a str,
    parent_answer_id: &'a str,
}

fn answer_tag(value: Option<&Value>) -> ProviderResult<String> {
    raw_answer_tag(value).map(|tag| match value {
        Some(Value::Number(_)) => format!("n:{tag}"),
        Some(Value::String(_)) => format!("s:{tag}"),
        _ => unreachable!("raw_answer_tag accepts only number or string"),
    })
}

fn raw_answer_tag(value: Option<&Value>) -> ProviderResult<String> {
    let tag = match value {
        Some(Value::Number(value)) => value
            .as_i64()
            .filter(|value| (0..=1_000_000).contains(value))
            .map(|value| value.to_string()),
        Some(Value::String(value)) if valid_optional_text(value, MAX_ANSWER_TAG_BYTES) => {
            Some(value.to_owned())
        }
        _ => None,
    }
    .ok_or_else(|| protocol_drift("Cidaren Question answer tag is invalid"))?;
    Ok(tag)
}

fn sanitized_metadata(
    object: &Map<String, Value>,
    remote_task_id: &str,
    topic_mode: i64,
    kind: QuestionKind,
    stem: &str,
    options: &[QuestionOption],
) -> ProviderResult<Value> {
    let stem_object = object
        .get("stem")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Cidaren Question has no stem metadata"))?;
    let prompt_content =
        optional_text(stem_object.get("content"), MAX_STEM_BYTES, "prompt content")?;
    let prompt_remark = match stem_object.get("remark") {
        Some(Value::String(value)) if value.is_empty() => None,
        Some(Value::String(value)) if valid_optional_text(value, MAX_STEM_BYTES) => {
            Some(normalize_text(value))
        }
        None | Some(Value::Null | Value::Array(_)) => None,
        _ => {
            return Err(protocol_drift("Cidaren Question prompt remark is invalid"));
        }
    };
    let relations = if kind == QuestionKind::Matching {
        parse_relations(
            object
                .get("stem")
                .and_then(Value::as_object)
                .and_then(|stem| stem.get("remark")),
        )?
    } else {
        Vec::new()
    };
    let word_lengths = match object.get("w_lens") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) if values.len() <= MAX_RELATIONS => values
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .filter(|value| *value > 0 && *value <= 1_024)
                    .ok_or_else(|| protocol_drift("Cidaren Question word length is invalid"))
            })
            .collect::<ProviderResult<Vec<_>>>()?,
        _ => {
            return Err(protocol_drift(
                "Cidaren Question word lengths have an unknown shape",
            ));
        }
    };
    let word_tip = optional_text(object.get("w_tip"), MAX_OPTION_BYTES, "word tip")?;
    Ok(json!({
        "schema": "cidaren.attempt-question.v1",
        "remote_task_id": remote_task_id,
        "topic_mode": topic_mode,
        "kind": kind,
        "relations": relations,
        "prompt_content": prompt_content,
        "prompt_remark": prompt_remark,
        "word_lengths": word_lengths,
        "word_tip": word_tip,
        "stem_sha256": format!("{:x}", Sha256::digest(stem.as_bytes())),
        "option_count": options.len(),
    }))
}

fn parse_relations(value: Option<&Value>) -> ProviderResult<Vec<String>> {
    let entries = value
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Cidaren matching Question has no relation list"))?;
    if entries.is_empty() || entries.len() > MAX_RELATIONS {
        return Err(invalid_response(
            "Cidaren matching relation count is invalid",
        ));
    }
    let mut unique = BTreeSet::new();
    let mut relations = Vec::with_capacity(entries.len());
    for entry in entries {
        let relation = entry
            .as_object()
            .and_then(|entry| entry.get("relation"))
            .map(|value| required_text(Some(value), MAX_OPTION_BYTES, "matching relation"))
            .transpose()?
            .ok_or_else(|| protocol_drift("Cidaren matching relation is missing"))?;
        if !unique.insert(relation.clone()) {
            return Err(protocol_drift(
                "Cidaren matching Question contains duplicate relations",
            ));
        }
        relations.push(relation);
    }
    Ok(relations)
}

fn question_remote_id(
    topic_mode: i64,
    stem: &str,
    options: &[QuestionOption],
    metadata: &Value,
) -> ProviderResult<String> {
    let material = serde_json::to_vec(&json!({
        "topic_mode": topic_mode,
        "stem": stem,
        "options": options,
        "metadata": metadata,
    }))
    .map_err(|_| invalid_response("Cidaren Question fingerprint cannot be encoded"))?;
    let digest = Sha256::digest(material);
    let mut remote_id = String::from("question:");
    write!(&mut remote_id, "{digest:x}")
        .map_err(|_| invalid_response("Cidaren Question fingerprint cannot be encoded"))?;
    Ok(remote_id)
}

fn optional_text(
    value: Option<&Value>,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) if valid_optional_text(value, maximum) => {
            Ok(Some(normalize_text(value)))
        }
        _ => Err(protocol_drift(format!(
            "Cidaren Question {label} is invalid"
        ))),
    }
}

fn required_text(
    value: Option<&Value>,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<String> {
    optional_text(value, maximum, label)?
        .ok_or_else(|| protocol_drift(format!("Cidaren Question has no bounded {label}")))
}

fn valid_optional_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value
            .chars()
            .any(|character| character.is_control() && character != '\n')
}

fn normalize_text(value: &str) -> String {
    value
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn valid_remote_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 768
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && (value.starts_with("class-task:") || value.starts_with("study-task:"))
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(values) => values.values_mut().for_each(zeroize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE: &str =
        include_str!("../../../fixtures/providers/cidaren/questions/start-answer-single.json");

    #[test]
    fn single_choice_is_sanitized_and_topic_code_stays_ephemeral() {
        let payload: Value = serde_json::from_str(SINGLE).unwrap();
        let parsed = parse_attempt_question(&payload, "class-task:2002", 1).unwrap();
        let progress = parsed.remote_progress().unwrap();
        assert_eq!(progress.completed(), 1);
        assert_eq!(progress.total(), 127);
        let reference = parsed.question_ref().unwrap();
        let mut progress_only_payload = payload.clone();
        progress_only_payload["topic_done_num"] = json!(99);
        let progress_only = parse_attempt_question(&progress_only_payload, "class-task:2002", 1)
            .unwrap()
            .question_ref()
            .unwrap();
        assert_eq!(progress_only.remote_id, reference.remote_id);
        assert_eq!(
            progress_only.metadata_sanitized,
            reference.metadata_sanitized
        );
        assert_eq!(reference.kind_hint, QuestionKind::SingleChoice);
        assert_eq!(
            reference.route_context.get("cidaren.topic_code"),
            Some("synthetic-topic-code")
        );
        assert!(
            !serde_json::to_string(&reference)
                .unwrap()
                .contains("synthetic-topic-code")
        );
        assert!(!format!("{parsed:?}").contains("synthetic-topic-code"));

        let question = parsed.to_question(TaskId::new()).unwrap();
        assert_eq!(question.options[0].id, "n:0");
        assert_eq!(question.options[1].id, "n:1");
        let serialized = serde_json::to_string(&question).unwrap();
        assert!(!serialized.contains("synthetic-topic-code"));
    }

    #[test]
    fn nested_matching_and_text_modes_preserve_only_semantics() {
        let matching = json!({
            "topic_code": "synthetic-matching-topic",
            "topic_mode": 31,
            "stem": {
                "content": "Match related words",
                "remark": [{"relation": "alpha"}, {"relation": "beta"}]
            },
            "options": [
                {"answer_tag": 0, "content": "alpha", "sub_options": []},
                {"answer_tag": 1, "content": "beta", "sub_options": []}
            ]
        });
        let parsed = parse_attempt_question(&matching, "study-task:course-a:list-a", 2).unwrap();
        let question = parsed.to_question(TaskId::new()).unwrap();
        assert_eq!(question.kind, QuestionKind::Matching);
        assert_eq!(
            question.metadata_sanitized["relations"],
            json!(["alpha", "beta"])
        );

        let nested = json!({
            "topic_code": "synthetic-nested-topic",
            "topic_mode": 41,
            "stem": {"content": "Complete {}", "remark": "合成例句"},
            "options": [{
                "answer_tag": "1#",
                "content": "parent",
                "sub_options": [
                    {"answer_tag": 0, "content": "first"},
                    {"answer_tag": 1, "content": "second"}
                ]
            }]
        });
        let question = parse_attempt_question(&nested, "class-task:2002", 3)
            .unwrap()
            .to_question(TaskId::new())
            .unwrap();
        assert_eq!(question.options[0].id, "s:1#0");
        assert_eq!(question.options[1].id, "s:1#1");
        assert_eq!(
            question.options[0].metadata_sanitized,
            json!({
                "nested": true,
                "top_level_index": 0,
                "top_level_content": "parent",
                "wire_content": "first",
                "parent_answer_id": "s:1#",
            })
        );

        let text = json!({
            "topic_code": "synthetic-text-topic",
            "topic_mode": 51,
            "stem": {"content": "Complete the word", "remark": ""},
            "options": [],
            "w_lens": [8],
            "w_tip": "syn"
        });
        let question = parse_attempt_question(&text, "class-task:2002", 4)
            .unwrap()
            .to_question(TaskId::new())
            .unwrap();
        assert_eq!(question.kind, QuestionKind::ShortAnswer);
        assert!(question.options.is_empty());
        assert_eq!(question.metadata_sanitized["word_lengths"], json!([8]));
    }

    #[test]
    fn reading_cards_are_distinct_steps_and_unknown_modes_fail_closed() {
        let mut payload: Value = serde_json::from_str(SINGLE).unwrap();
        payload["topic_mode"] = json!(0);
        payload["options"] = json!([]);
        let ParsedCidarenAttemptStep::ReadingCard(card) =
            parse_attempt_step(&payload, "class-task:2002", 1).unwrap()
        else {
            panic!("expected reading card");
        };
        assert!(card.remote_id().starts_with("reading-card:"));
        assert_eq!(card.position(), 1);
        assert_eq!(card.remote_progress().unwrap().completed(), 1);
        assert_eq!(card.remote_progress().unwrap().total(), 127);
        assert!(!card.stem_sanitized().is_empty());
        assert_eq!(
            card.route_context().unwrap().get("cidaren.topic_code"),
            Some("synthetic-topic-code")
        );
        assert!(!format!("{card:?}").contains("synthetic-topic-code"));

        payload["topic_mode"] = json!(999);
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::UnsupportedTask
        );
        let mut payload: Value = serde_json::from_str(SINGLE).unwrap();
        payload["topic_mode"] = json!(17);
        payload["options"][1]["answer_tag"] = json!(0);
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
        assert!(parse_attempt_question(&payload, "foreign-task", 1).is_err());
        assert!(parse_attempt_question(&payload, "class-task:2002", 0).is_err());
    }

    #[test]
    fn remote_attempt_progress_is_optional_bounded_and_consistent() {
        let mut payload: Value = serde_json::from_str(SINGLE).unwrap();
        payload.as_object_mut().unwrap().remove("topic_done_num");
        payload.as_object_mut().unwrap().remove("topic_total");
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap()
                .remote_progress(),
            None
        );

        payload["topic_done_num"] = json!(0);
        payload["topic_total"] = json!(12);
        let progress = parse_attempt_question(&payload, "class-task:2002", 1)
            .unwrap()
            .remote_progress()
            .unwrap();
        assert_eq!(progress.completed(), 0);
        assert_eq!(progress.total(), 12);

        payload["topic_done_num"] = json!(13);
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        payload["topic_done_num"] = json!(1);
        payload["topic_total"] = json!(100_001);
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        payload["topic_total"] = json!(12.5);
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );

        payload["topic_done_num"] = json!(1);
        payload.as_object_mut().unwrap().remove("topic_total");
        assert_eq!(
            parse_attempt_question(&payload, "class-task:2002", 1)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }
}
