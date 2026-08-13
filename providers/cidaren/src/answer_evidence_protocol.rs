use std::{collections::BTreeSet, fmt};

use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderResult, RemoteTask, RemoteTaskDetail,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    CidarenAnswerEvidence, CidarenAssessmentBinding, CidarenCryptoContext,
    CidarenStudyTaskDocument, CidarenWordEvidence, decode_response_data,
    parse_study_task_inventory, parse_word_evidence,
};

const STUDY_TASK_INFO_PATH: &str = "StudyTask/Info";
const STUDY_WORD_INFO_PATH: &str = "Course/StudyWordInfo";
const SEARCH_WORD_PATH: &str = "Course/SearchWord";
const STUDY_TASK_INFO_VERSION: &str = "2.6.1.240305";
const READ_VERSION: &str = "2.6.1.231204";
const SEARCH_WORD_VERSION: &str = "2.6.2.24031302";
const MAX_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_WORDS: usize = 100_000;
const MAX_WORD_BYTES: usize = 1_024;
const MAX_COMPONENT_BYTES: usize = 256;

/// Fresh, task-bound route used to acquire the word inventory required by the
/// donor's answer strategies.
pub struct CidarenAnswerEvidenceBinding {
    remote_task_id: String,
    route: CidarenWordInventoryRoute,
    progress: u8,
    word_selection_eligible: bool,
}

enum CidarenWordInventoryRoute {
    StudyTaskInfo {
        task_id: i64,
        course_id: String,
        list_id: Option<String>,
        release_id: Option<String>,
    },
    CoursePage {
        course_id: String,
    },
}

impl CidarenAnswerEvidenceBinding {
    /// Rebinds answer evidence to a fresh Task detail and a fresh complete
    /// `StudyTask/List` document for that Task's Course.
    ///
    /// Class tasks matching exactly one ordinary unit use that unit. A class
    /// learning task with no matching unit uses donor-observed self-built
    /// `release_id` semantics; a class test with no matching unit uses the
    /// public Course-page resource. Duplicate title matches fail closed.
    ///
    /// # Errors
    ///
    /// Returns a typed error for foreign, stale, cross-Course or ambiguous
    /// bindings.
    pub fn from_fresh_detail(
        remote_task_id: &str,
        detail: &RemoteTaskDetail,
        course_units: &CidarenStudyTaskDocument,
    ) -> ProviderResult<Self> {
        let course_id = fresh_course_id(remote_task_id, detail)?;
        let task = detail
            .normalized_detail
            .get("task")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("Cidaren answer evidence has no fresh Task object"))?;
        if course_units.selected_course_id() != course_id {
            return Err(remote_changed(
                "Cidaren answer evidence changed the freshly bound Course",
            ));
        }
        let task_id = required_task_id(task.get("task_id"))?;
        let progress = required_progress(task.get("progress"))?;
        let units = parse_study_task_inventory(None, course_units)?;

        let (route, word_selection_eligible) = if remote_task_id.starts_with("study-task:") {
            let unit = units
                .into_iter()
                .find(|unit| unit.remote_id == remote_task_id)
                .ok_or_else(|| {
                    remote_changed("Cidaren study unit disappeared before evidence acquisition")
                })?;
            let unit_task_id = required_task_id(unit.normalized.get("task_id"))?;
            let list_id = required_component(unit.normalized.get("list_id"), "list ID")?;
            if unit_task_id != task_id || unit.title != detail.task.title {
                return Err(remote_changed(
                    "Cidaren study unit changed before evidence acquisition",
                ));
            }
            (
                CidarenWordInventoryRoute::StudyTaskInfo {
                    task_id,
                    course_id,
                    list_id: Some(list_id),
                    release_id: None,
                },
                true,
            )
        } else {
            let task_type = task
                .get("task_type")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_drift("Cidaren class Task has no task type"))?;
            let release_id = required_component(task.get("release_id"), "release ID")?;
            let mut title_matches = units
                .into_iter()
                .filter(|unit| unit.title == detail.task.title);
            let matched = title_matches.next();
            if title_matches.next().is_some() {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "Cidaren class Task title matches multiple study units",
                ));
            }
            class_word_inventory_route(task_type, task_id, course_id, release_id, matched)?
        };
        Ok(Self {
            remote_task_id: remote_task_id.to_owned(),
            route,
            progress,
            word_selection_eligible,
        })
    }

    fn route(&self) -> &CidarenWordInventoryRoute {
        &self.route
    }
}

fn class_word_inventory_route(
    task_type: &str,
    task_id: i64,
    course_id: String,
    release_id: String,
    matched: Option<RemoteTask>,
) -> ProviderResult<(CidarenWordInventoryRoute, bool)> {
    match (task_type, matched) {
        ("learning", Some(unit)) => Ok((
            CidarenWordInventoryRoute::StudyTaskInfo {
                task_id,
                course_id,
                list_id: Some(required_component(
                    unit.normalized.get("list_id"),
                    "list ID",
                )?),
                release_id: None,
            },
            true,
        )),
        ("test", Some(unit)) => Ok((
            CidarenWordInventoryRoute::StudyTaskInfo {
                task_id: required_task_id(unit.normalized.get("task_id"))?,
                course_id,
                list_id: Some(required_component(
                    unit.normalized.get("list_id"),
                    "list ID",
                )?),
                release_id: None,
            },
            false,
        )),
        ("learning", None) => Ok((
            CidarenWordInventoryRoute::StudyTaskInfo {
                task_id,
                course_id,
                list_id: None,
                release_id: Some(release_id),
            },
            true,
        )),
        ("test", None) => Ok((CidarenWordInventoryRoute::CoursePage { course_id }, false)),
        _ => Err(protocol_drift(
            "Cidaren class Task uses an unknown answer-evidence family",
        )),
    }
}

pub(crate) fn fresh_course_id(
    remote_task_id: &str,
    detail: &RemoteTaskDetail,
) -> ProviderResult<String> {
    CidarenAssessmentBinding::from_fresh_detail(remote_task_id, detail)?;
    detail
        .normalized_detail
        .get("task")
        .and_then(Value::as_object)
        .map(|task| required_component(task.get("course_id"), "Course ID"))
        .ok_or_else(|| protocol_drift("Cidaren answer evidence has no fresh Task object"))?
}

impl fmt::Debug for CidarenAnswerEvidenceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenAnswerEvidenceBinding")
            .field("remote_task_id", &self.remote_task_id)
            .field("route", &self.route)
            .field("progress", &self.progress)
            .field("word_selection_eligible", &self.word_selection_eligible)
            .finish()
    }
}

impl fmt::Debug for CidarenWordInventoryRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StudyTaskInfo {
                task_id,
                course_id,
                list_id,
                release_id,
            } => formatter
                .debug_struct("StudyTaskInfo")
                .field("task_id", task_id)
                .field("course_id", course_id)
                .field("list_id", list_id)
                .field("release_id", release_id)
                .finish(),
            Self::CoursePage { course_id } => formatter
                .debug_struct("CoursePage")
                .field("course_id", course_id)
                .finish(),
        }
    }
}

impl Drop for CidarenAnswerEvidenceBinding {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        match &mut self.route {
            CidarenWordInventoryRoute::StudyTaskInfo {
                course_id,
                list_id,
                release_id,
                ..
            } => {
                course_id.zeroize();
                list_id.zeroize();
                release_id.zeroize();
            }
            CidarenWordInventoryRoute::CoursePage { course_id } => course_id.zeroize(),
        }
    }
}

/// Credential-free request facts for one donor-observed word-inventory route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CidarenWordInventoryRequest {
    StudyTaskInfo {
        path: &'static str,
        query: Vec<(String, String)>,
    },
    CoursePage {
        url: String,
    },
}

/// Builds the exact current word-inventory request after fresh Task rebinding.
pub fn build_word_inventory_request(
    binding: &CidarenAnswerEvidenceBinding,
    timestamp_millis: u64,
) -> CidarenWordInventoryRequest {
    match binding.route() {
        CidarenWordInventoryRoute::StudyTaskInfo {
            task_id,
            course_id,
            list_id,
            release_id,
        } => {
            let mut query = vec![
                ("task_id".to_owned(), task_id.to_string()),
                ("course_id".to_owned(), course_id.clone()),
                ("timestamp".to_owned(), timestamp_millis.to_string()),
                ("version".to_owned(), STUDY_TASK_INFO_VERSION.to_owned()),
                ("app_type".to_owned(), "1".to_owned()),
            ];
            if let Some(release_id) = release_id {
                query.push(("release_id".to_owned(), release_id.clone()));
            } else if let Some(list_id) = list_id {
                query.push(("list_id".to_owned(), list_id.clone()));
            }
            CidarenWordInventoryRequest::StudyTaskInfo {
                path: STUDY_TASK_INFO_PATH,
                query,
            }
        }
        CidarenWordInventoryRoute::CoursePage { course_id } => {
            CidarenWordInventoryRequest::CoursePage {
                url: format!("https://resource.vocabgo.com/Resource/CoursePage/{course_id}.json"),
            }
        }
    }
}

/// One bounded word and its exact Course unit location.
pub struct CidarenWordLookup {
    course_id: String,
    list_id: String,
    word: String,
}

impl CidarenWordLookup {
    pub(crate) fn dedup_key(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(self.course_id.as_bytes());
        digest.update([0]);
        digest.update(self.list_id.as_bytes());
        digest.update([0]);
        digest.update(self.word.as_bytes());
        digest.finalize().into()
    }

    pub(crate) fn cloned_word(&self) -> String {
        self.word.clone()
    }
}

impl fmt::Debug for CidarenWordLookup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenWordLookup")
            .field("course_id", &self.course_id)
            .field("list_id", &self.list_id)
            .field("word", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenWordLookup {
    fn drop(&mut self) {
        self.course_id.zeroize();
        self.list_id.zeroize();
        self.word.zeroize();
    }
}

/// Exact credential-free `Course/StudyWordInfo` request facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CidarenWordInfoRequest {
    pub path: &'static str,
    pub query: Vec<(String, String)>,
}

/// Exact credential-free `Course/SearchWord` request facts used by the donor
/// when local lemmatization cannot bind an inflected prompt to the Task word
/// inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CidarenWordPrototypeRequest {
    pub path: &'static str,
    pub query: Vec<(String, String)>,
}

/// Builds a word-info request from one location returned by the freshly bound
/// inventory, preventing arbitrary cross-Course lookups.
pub fn build_word_info_request(
    lookup: &CidarenWordLookup,
    timestamp_millis: u64,
) -> CidarenWordInfoRequest {
    CidarenWordInfoRequest {
        path: STUDY_WORD_INFO_PATH,
        query: vec![
            ("course_id".to_owned(), lookup.course_id.clone()),
            ("list_id".to_owned(), lookup.list_id.clone()),
            ("word".to_owned(), lookup.word.clone()),
            ("timestamp".to_owned(), timestamp_millis.to_string()),
            ("version".to_owned(), READ_VERSION.to_owned()),
            ("app_type".to_owned(), "1".to_owned()),
        ],
    }
}

/// Builds one bounded donor-observed prototype lookup.
///
/// # Errors
///
/// Returns `InvalidResponse` for unsafe or unbounded input.
pub fn build_word_prototype_request(
    word: &str,
    timestamp_millis: u64,
) -> ProviderResult<CidarenWordPrototypeRequest> {
    let word = required_text(Some(&Value::String(word.to_owned())), "prototype word")?;
    Ok(CidarenWordPrototypeRequest {
        path: SEARCH_WORD_PATH,
        query: vec![
            ("word".to_owned(), word),
            ("timestamp".to_owned(), timestamp_millis.to_string()),
            ("version".to_owned(), SEARCH_WORD_VERSION.to_owned()),
            ("app_type".to_owned(), "1".to_owned()),
        ],
    })
}

struct CidarenWordLocation {
    word: String,
    list_id: String,
}

/// Bounded current word inventory with first-occurrence Course-unit routing.
/// The current donor also uses the first Course-page occurrence when a word
/// appears in multiple units.
pub struct CidarenWordInventory {
    course_id: String,
    words: Vec<CidarenWordLocation>,
    exist_little_task: Option<u8>,
}

/// Transient donor-compatible `SubmitChoseWord` plan. The word-map payload is
/// redacted and zeroized, and still needs the durable mutation execution path
/// before it can be sent.
pub struct CidarenWordSelectionPlan {
    remote_task_id: String,
    word_map: Value,
    word_count: usize,
}

impl CidarenWordSelectionPlan {
    /// Exposes the transient map only to the signed Provider mutation builder.
    pub fn word_map(&self) -> &Value {
        &self.word_map
    }

    pub(crate) fn is_bound_to(&self, remote_task_id: &str) -> bool {
        self.remote_task_id == remote_task_id
    }
}

impl fmt::Debug for CidarenWordSelectionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenWordSelectionPlan")
            .field("task_binding", &"configured")
            .field("word_count", &self.word_count)
            .field("word_map", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl Drop for CidarenWordSelectionPlan {
    fn drop(&mut self) {
        self.remote_task_id.zeroize();
        zeroize_json(&mut self.word_map);
    }
}

impl CidarenWordInventory {
    /// Returns a lookup only for a word present in this freshly fetched
    /// inventory.
    pub fn lookup(&self, word: &str) -> Option<CidarenWordLookup> {
        self.words
            .iter()
            .find(|entry| entry.word.eq_ignore_ascii_case(word))
            .map(|entry| CidarenWordLookup {
                course_id: self.course_id.clone(),
                list_id: entry.list_id.clone(),
                word: entry.word.clone(),
            })
    }

    /// Donor-observed small-task selector state, when supplied by
    /// `StudyTask/Info`.
    pub const fn exist_little_task(&self) -> Option<u8> {
        self.exist_little_task
    }

    /// Returns the fresh inventory entries whose words can drive the donor's
    /// completion-example fallback. Direct prefix/length matches need no word
    /// info request and are deliberately omitted.
    pub(crate) fn completion_evidence_lookups(
        &self,
        prefix: &str,
        answer_length: usize,
        maximum: usize,
    ) -> ProviderResult<Vec<CidarenWordLookup>> {
        let mut lookups = Vec::new();
        for entry in &self.words {
            if !entry.word.to_lowercase().starts_with(prefix) {
                continue;
            }
            let word_length = entry.word.chars().count();
            if word_length == answer_length || word_length.saturating_add(1) == answer_length {
                continue;
            }
            if lookups.len() >= maximum {
                return Err(invalid_response(
                    "Cidaren completion evidence exceeds the lookup limit",
                ));
            }
            lookups.push(CidarenWordLookup {
                course_id: self.course_id.clone(),
                list_id: entry.list_id.clone(),
                word: entry.word.clone(),
            });
        }
        Ok(lookups)
    }

    /// Converts the inventory and independently fetched word records into one
    /// zeroizing answer-evidence snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for duplicate, unsafe or oversized evidence.
    pub fn into_answer_evidence(
        mut self,
        word_infos: Vec<CidarenWordEvidence>,
    ) -> ProviderResult<CidarenAnswerEvidence> {
        let words = self
            .words
            .iter_mut()
            .map(|entry| std::mem::take(&mut entry.word))
            .collect();
        CidarenAnswerEvidence::try_new(words, word_infos)
    }

    pub(crate) fn into_answer_evidence_with_aliases(
        mut self,
        word_infos: Vec<CidarenWordEvidence>,
        aliases: Vec<(String, String)>,
    ) -> ProviderResult<CidarenAnswerEvidence> {
        let words = self
            .words
            .iter_mut()
            .map(|entry| std::mem::take(&mut entry.word))
            .collect();
        CidarenAnswerEvidence::try_new_with_aliases(words, word_infos, aliases)
    }
}

/// Reproduces the donor's small-task selection rule and exact word-map shape.
/// Standard units submit a flat word array. Self-built learning tasks submit
/// `{course_id:list_id: [words...]}` groups.
///
/// # Errors
///
/// Returns `ProtocolDrift` if an eligible route lacks the donor's
/// `exist_little_task` observation or contains an impossible word location.
pub fn build_word_selection_plan(
    binding: &CidarenAnswerEvidenceBinding,
    inventory: &CidarenWordInventory,
) -> ProviderResult<Option<CidarenWordSelectionPlan>> {
    if !binding.word_selection_eligible {
        return Ok(None);
    }
    let exist_little_task = inventory.exist_little_task.ok_or_else(|| {
        protocol_drift("Cidaren eligible word-selection route has no small-task state")
    })?;
    let should_select = (binding.progress < 2 && exist_little_task != 1) || exist_little_task == 2;
    if !should_select {
        return Ok(None);
    }
    let word_count = inventory.words.len();
    let self_built = matches!(
        binding.route(),
        CidarenWordInventoryRoute::StudyTaskInfo {
            release_id: Some(_),
            ..
        }
    );
    let word_map = if self_built {
        let mut groups = Map::new();
        for entry in &inventory.words {
            let key = format!("{}:{}", inventory.course_id, entry.list_id);
            let values = groups
                .entry(key)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| protocol_drift("Cidaren word-selection group changed shape"))?;
            values.push(Value::String(entry.word.clone()));
        }
        Value::Object(groups)
    } else {
        Value::Array(
            inventory
                .words
                .iter()
                .map(|entry| Value::String(entry.word.clone()))
                .collect(),
        )
    };
    Ok(Some(CidarenWordSelectionPlan {
        remote_task_id: binding.remote_task_id.clone(),
        word_map,
        word_count,
    }))
}

impl fmt::Debug for CidarenWordInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenWordInventory")
            .field("course_id", &self.course_id)
            .field("word_count", &self.words.len())
            .field("exist_little_task", &self.exist_little_task)
            .field("words", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenWordInventory {
    fn drop(&mut self) {
        self.course_id.zeroize();
        for entry in &mut self.words {
            entry.word.zeroize();
            entry.list_id.zeroize();
        }
    }
}

/// Parses and binds one decoded or encoded `StudyTask/Info` envelope.
///
/// # Errors
///
/// Returns a typed error for the wrong route, malformed envelope, crypto
/// mismatch, unknown encoding or word/list drift.
pub fn parse_study_task_info_response(
    document: &[u8],
    binding: &CidarenAnswerEvidenceBinding,
    crypto: Option<&CidarenCryptoContext>,
) -> ProviderResult<CidarenWordInventory> {
    let CidarenWordInventoryRoute::StudyTaskInfo {
        course_id, list_id, ..
    } = binding.route()
    else {
        return Err(protocol_drift(
            "Cidaren StudyTask/Info response was bound to a Course-page route",
        ));
    };
    let mut payload = decode_success_envelope(document, crypto, "StudyTask/Info")?;
    let object = payload
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren StudyTask/Info data is not an object"))?;
    let exist_little_task = object
        .get("exist_little_task")
        .filter(|value| !value.is_null())
        .map(|value| required_small_u8(Some(value), "small-task state"))
        .transpose()?;
    let words = parse_word_locations(object.get("word_list"), list_id.as_deref())?;
    zeroize_json(&mut payload);
    Ok(CidarenWordInventory {
        course_id: course_id.clone(),
        words,
        exist_little_task,
    })
}

/// Parses the public self-built Course-page resource without accepting a
/// redirect, wrapper or cross-Course route.
///
/// # Errors
///
/// Returns a typed error for the wrong binding, malformed JSON or unbounded
/// word locations.
pub fn parse_course_page_response(
    document: &[u8],
    binding: &CidarenAnswerEvidenceBinding,
) -> ProviderResult<CidarenWordInventory> {
    let CidarenWordInventoryRoute::CoursePage { course_id } = binding.route() else {
        return Err(protocol_drift(
            "Cidaren Course-page response was bound to a StudyTask/Info route",
        ));
    };
    if document.is_empty() || document.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Cidaren Course-page response is empty or exceeds the size limit",
        ));
    }
    let mut payload: Value = serde_json::from_slice(document)
        .map_err(|_| invalid_response("Cidaren Course-page response is not valid JSON"))?;
    let words = parse_word_locations(Some(&payload), None)?;
    zeroize_json(&mut payload);
    Ok(CidarenWordInventory {
        course_id: course_id.clone(),
        words,
        exist_little_task: None,
    })
}

/// Parses one exact word-info response and rebinds its Course/list/word tuple
/// to the inventory-derived lookup.
///
/// # Errors
///
/// Returns a typed error for malformed framing, crypto mismatch or identity
/// drift.
pub fn parse_word_info_response(
    document: &[u8],
    lookup: &CidarenWordLookup,
    crypto: Option<&CidarenCryptoContext>,
) -> ProviderResult<CidarenWordEvidence> {
    let mut payload = decode_success_envelope(document, crypto, "StudyWordInfo")?;
    let object = payload
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren word-info data is not an object"))?;
    if required_component(object.get("course_id"), "Course ID")? != lookup.course_id
        || required_component(object.get("list_id"), "list ID")? != lookup.list_id
        || required_text(object.get("word"), "word")? != lookup.word
    {
        zeroize_json(&mut payload);
        return Err(remote_changed(
            "Cidaren word-info response changed its inventory binding",
        ));
    }
    let parsed = parse_word_evidence(&payload);
    zeroize_json(&mut payload);
    parsed
}

/// Parses the donor's HTML-wrapped word-prototype result. Absence of a span is
/// a bounded negative lookup rather than protocol success with invented data.
///
/// # Errors
///
/// Returns a typed error for malformed framing, non-success code or unsafe
/// prototype text.
pub fn parse_word_prototype_response(document: &[u8]) -> ProviderResult<Option<String>> {
    if document.is_empty() || document.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid_response(
            "Cidaren SearchWord response is empty or exceeds the size limit",
        ));
    }
    let mut root: Value = serde_json::from_slice(document)
        .map_err(|_| invalid_response("Cidaren SearchWord response is not valid JSON"))?;
    let object = root
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren SearchWord response is not an object"))?;
    if object.get("code").and_then(Value::as_i64) != Some(1) {
        zeroize_json(&mut root);
        return Err(invalid_response(
            "Cidaren SearchWord endpoint returned a non-success code",
        ));
    }
    let meaning = object
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("word_mean"))
        .and_then(Value::as_object)
        .and_then(|word_mean| word_mean.get("meaning"))
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 64 * 1_024)
        .ok_or_else(|| protocol_drift("Cidaren SearchWord response has no bounded meaning"))?;
    let decoded_markup = if meaning.contains("<span>") {
        None
    } else {
        Some(decode_literal_unicode_escapes(meaning)?)
    };
    let markup = decoded_markup
        .as_ref()
        .map_or(meaning, |decoded| decoded.as_str());
    let prototype = markup
        .find("<span>")
        .map(|start| start + "<span>".len())
        .and_then(|start| {
            markup[start..]
                .find("</span>")
                .map(|end| &markup[start..start + end])
        })
        .filter(|value| valid_word(value))
        .map(ToOwned::to_owned);
    zeroize_json(&mut root);
    Ok(prototype)
}

fn decode_literal_unicode_escapes(value: &str) -> ProviderResult<Zeroizing<String>> {
    let mut characters = value.chars().peekable();
    let mut decoded = String::with_capacity(value.len());
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let Some(escape) = characters.next() else {
            return Err(invalid_response(
                "Cidaren SearchWord meaning ends with an invalid escape",
            ));
        };
        match escape {
            'u' => decoded.push(decode_hex_escape(&mut characters, 4)?),
            'x' => decoded.push(decode_hex_escape(&mut characters, 2)?),
            '\\' => decoded.push('\\'),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            '\'' => decoded.push('\''),
            '"' => decoded.push('"'),
            other => {
                decoded.push('\\');
                decoded.push(other);
            }
        }
        if decoded.len() > 64 * 1_024 {
            decoded.zeroize();
            return Err(invalid_response(
                "Cidaren SearchWord meaning exceeds the decoded size limit",
            ));
        }
    }
    Ok(Zeroizing::new(decoded))
}

fn decode_hex_escape(
    characters: &mut impl Iterator<Item = char>,
    digits: usize,
) -> ProviderResult<char> {
    let mut value = 0_u32;
    for _ in 0..digits {
        let digit = characters
            .next()
            .and_then(|digit| digit.to_digit(16))
            .ok_or_else(|| invalid_response("Cidaren SearchWord meaning has an invalid escape"))?;
        value = value * 16 + digit;
    }
    char::from_u32(value)
        .ok_or_else(|| invalid_response("Cidaren SearchWord meaning has an invalid code point"))
}

fn decode_success_envelope(
    document: &[u8],
    crypto: Option<&CidarenCryptoContext>,
    label: &'static str,
) -> ProviderResult<Value> {
    if document.is_empty() || document.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid_response(format!(
            "Cidaren {label} response is empty or exceeds the size limit"
        )));
    }
    let mut root: Value = serde_json::from_slice(document)
        .map_err(|_| invalid_response(format!("Cidaren {label} response is not valid JSON")))?;
    let object = root
        .as_object()
        .ok_or_else(|| protocol_drift(format!("Cidaren {label} response is not an object")))?;
    if object.get("code").and_then(Value::as_i64) != Some(1) {
        zeroize_json(&mut root);
        return Err(invalid_response(format!(
            "Cidaren {label} endpoint returned a non-success code"
        )));
    }
    let data = object
        .get("data")
        .filter(|value| !value.is_null())
        .ok_or_else(|| protocol_drift(format!("Cidaren {label} response has no data")))?;
    let jv = match object.get("jv") {
        None | Some(Value::Null) => "0",
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 64 => value,
        _ => {
            zeroize_json(&mut root);
            return Err(protocol_drift(format!(
                "Cidaren {label} response has an invalid jv"
            )));
        }
    };
    let decoded = decode_response_data(data, jv, crypto);
    zeroize_json(&mut root);
    decoded
}

fn parse_word_locations(
    value: Option<&Value>,
    expected_list_id: Option<&str>,
) -> ProviderResult<Vec<CidarenWordLocation>> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Cidaren word inventory has no word array"))?;
    if values.is_empty() || values.len() > MAX_WORDS {
        return Err(invalid_response(
            "Cidaren word inventory count is empty or exceeds the limit",
        ));
    }
    let mut unique = BTreeSet::new();
    let mut words = Vec::with_capacity(values.len());
    for value in values {
        let object = value
            .as_object()
            .ok_or_else(|| protocol_drift("Cidaren word inventory contains a non-object row"))?;
        let word = required_text(object.get("word"), "word")?;
        let list_id = required_component(object.get("list_id"), "list ID")?;
        if expected_list_id.is_some_and(|expected| expected != list_id) {
            return Err(remote_changed(
                "Cidaren StudyTask/Info returned a word from another unit",
            ));
        }
        if unique.insert(word.to_lowercase()) {
            words.push(CidarenWordLocation { word, list_id });
        }
    }
    if words.is_empty() {
        return Err(protocol_drift(
            "Cidaren word inventory contains no unique words",
        ));
    }
    Ok(words)
}

fn required_task_id(value: Option<&Value>) -> ProviderResult<i64> {
    match value {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.parse::<i64>().ok(),
        _ => None,
    }
    .filter(|value| *value == -1 || *value > 0)
    .ok_or_else(|| protocol_drift("Cidaren Task contains an invalid task ID"))
}

fn required_progress(value: Option<&Value>) -> ProviderResult<u8> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value <= 100)
        .ok_or_else(|| protocol_drift("Cidaren Task contains invalid progress"))
}

fn required_component(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    let value = match value {
        Some(Value::String(value)) => value.to_owned(),
        Some(Value::Number(value)) if value.as_u64().is_some() => value.to_string(),
        _ => {
            return Err(protocol_drift(format!(
                "Cidaren answer evidence has no valid {label}"
            )));
        }
    };
    if valid_component(&value) {
        Ok(value)
    } else {
        Err(protocol_drift(format!(
            "Cidaren answer evidence contains an invalid {label}"
        )))
    }
}

fn required_text(value: Option<&Value>, label: &'static str) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_WORD_BYTES
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            protocol_drift(format!(
                "Cidaren answer evidence contains an invalid {label}"
            ))
        })
}

fn required_small_u8(value: Option<&Value>, label: &'static str) -> ProviderResult<u8> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value <= 2)
        .ok_or_else(|| {
            protocol_drift(format!(
                "Cidaren answer evidence contains an invalid {label}"
            ))
        })
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_WORD_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && value
            .chars()
            .all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | ' '))
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

fn remote_changed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AssessmentClass, RemoteState, SourceType};
    use asterism_provider_api::RemoteTask;
    use serde_json::json;

    use super::*;

    const STUDY_INFO: &str =
        include_str!("../../../fixtures/providers/cidaren/answers/study-task-info.json");
    const COURSE_PAGE: &str =
        include_str!("../../../fixtures/providers/cidaren/answers/course-page.json");
    const WORD_INFO: &str =
        include_str!("../../../fixtures/providers/cidaren/answers/study-word-info-envelope.json");

    #[test]
    fn ordinary_class_test_rebinds_to_the_matching_study_unit() {
        let detail = detail("class-task:2002", "test", -1, "Synthetic List 02");
        let units = study_document();
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &detail, &units)
                .unwrap();
        let request = build_word_inventory_request(&binding, 1_730_000_000_000);
        let CidarenWordInventoryRequest::StudyTaskInfo { path, query } = request else {
            panic!("expected StudyTask/Info");
        };
        assert_eq!(path, "StudyTask/Info");
        assert_eq!(query_value(&query, "task_id"), Some("71002"));
        assert_eq!(query_value(&query, "list_id"), Some("course-a_02"));
        assert_eq!(query_value(&query, "version"), Some("2.6.1.240305"));
    }

    #[test]
    fn self_built_learning_and_test_preserve_distinct_donor_routes() {
        let units = study_document();
        let learning = detail("class-task:2002", "learning", 92002, "Self Built Learning");
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &learning, &units)
                .unwrap();
        let CidarenWordInventoryRequest::StudyTaskInfo { query, .. } =
            build_word_inventory_request(&binding, 1)
        else {
            panic!("expected StudyTask/Info");
        };
        assert_eq!(query_value(&query, "task_id"), Some("92002"));
        assert_eq!(query_value(&query, "release_id"), Some("2002"));
        assert!(query_value(&query, "list_id").is_none());

        let test = detail("class-task:2002", "test", -1, "Self Built Test");
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &test, &units)
                .unwrap();
        assert_eq!(
            build_word_inventory_request(&binding, 1),
            CidarenWordInventoryRequest::CoursePage {
                url: "https://resource.vocabgo.com/Resource/CoursePage/course-a.json".to_owned(),
            }
        );
    }

    #[test]
    fn response_parsers_bind_inventory_and_word_info() {
        let units = study_document();
        let detail = detail("class-task:2002", "test", -1, "Synthetic List 02");
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &detail, &units)
                .unwrap();
        let inventory =
            parse_study_task_info_response(STUDY_INFO.as_bytes(), &binding, None).unwrap();
        assert_eq!(inventory.exist_little_task(), Some(2));
        let lookup = inventory.lookup("alpha").unwrap();
        let request = build_word_info_request(&lookup, 1_730_000_000_000);
        assert_eq!(request.path, "Course/StudyWordInfo");
        assert_eq!(query_value(&request.query, "list_id"), Some("course-a_02"));
        let info = parse_word_info_response(WORD_INFO.as_bytes(), &lookup, None).unwrap();
        let evidence = inventory.into_answer_evidence(vec![info]).unwrap();
        assert!(!format!("{evidence:?}").contains("alpha"));
    }

    #[test]
    fn course_page_keeps_first_location_and_never_requires_credentials() {
        let units = study_document();
        let self_built_detail = detail("class-task:2002", "test", -1, "Self Built Test");
        let binding = CidarenAnswerEvidenceBinding::from_fresh_detail(
            "class-task:2002",
            &self_built_detail,
            &units,
        )
        .unwrap();
        let inventory = parse_course_page_response(COURSE_PAGE.as_bytes(), &binding).unwrap();
        let alpha = inventory.lookup("alpha").unwrap();
        assert_eq!(alpha.list_id, "course-a_01");
        assert!(!format!("{inventory:?}").contains("alpha"));
    }

    #[test]
    fn cross_route_and_identity_drift_fail_closed() {
        let units = study_document();
        let self_built_detail = detail("class-task:2002", "test", -1, "Self Built Test");
        let binding = CidarenAnswerEvidenceBinding::from_fresh_detail(
            "class-task:2002",
            &self_built_detail,
            &units,
        )
        .unwrap();
        assert!(parse_study_task_info_response(STUDY_INFO.as_bytes(), &binding, None).is_err());

        let ordinary_detail = detail("class-task:2002", "test", -1, "Synthetic List 02");
        let binding = CidarenAnswerEvidenceBinding::from_fresh_detail(
            "class-task:2002",
            &ordinary_detail,
            &units,
        )
        .unwrap();
        let inventory =
            parse_study_task_info_response(STUDY_INFO.as_bytes(), &binding, None).unwrap();
        let lookup = inventory.lookup("alpha").unwrap();
        let changed = WORD_INFO.replace("course-a_02", "course-a_03");
        assert_eq!(
            parse_word_info_response(changed.as_bytes(), &lookup, None)
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    #[test]
    fn word_prototype_request_and_response_are_bounded() {
        let request = build_word_prototype_request("packed", 1_730_000_000_000).unwrap();
        assert_eq!(request.path, "Course/SearchWord");
        assert_eq!(query_value(&request.query, "word"), Some("packed"));
        assert_eq!(
            query_value(&request.query, "version"),
            Some("2.6.2.24031302")
        );
        let response = serde_json::json!({
            "code": 1,
            "data": {"word_mean": {"meaning": "<p><span>pack</span></p>"}}
        });
        assert_eq!(
            parse_word_prototype_response(&serde_json::to_vec(&response).unwrap()).unwrap(),
            Some("pack".to_owned())
        );
        let escaped = serde_json::json!({
            "code": 1,
            "data": {"word_mean": {"meaning": r"\u003cp\u003e\u003cspan\u003epack\u003c/span\u003e\u003c/p\u003e"}}
        });
        assert_eq!(
            parse_word_prototype_response(&serde_json::to_vec(&escaped).unwrap()).unwrap(),
            Some("pack".to_owned())
        );
        let absent = serde_json::json!({
            "code": 1,
            "data": {"word_mean": {"meaning": "<p>no prototype</p>"}}
        });
        assert_eq!(
            parse_word_prototype_response(&serde_json::to_vec(&absent).unwrap()).unwrap(),
            None
        );
        assert!(build_word_prototype_request(" bad\n", 1).is_err());
    }

    #[test]
    fn word_selection_plan_preserves_flat_and_self_built_shapes() {
        let units = study_document();
        let learning = detail("class-task:2002", "learning", 92002, "Synthetic List 02");
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &learning, &units)
                .unwrap();
        let inventory =
            parse_study_task_info_response(STUDY_INFO.as_bytes(), &binding, None).unwrap();
        let plan = build_word_selection_plan(&binding, &inventory)
            .unwrap()
            .unwrap();
        assert_eq!(plan.word_map(), &json!(["alpha", "beta"]));
        assert!(!format!("{plan:?}").contains("alpha"));

        let learning = detail("class-task:2002", "learning", 92002, "Self Built Learning");
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &learning, &units)
                .unwrap();
        let inventory =
            parse_study_task_info_response(STUDY_INFO.as_bytes(), &binding, None).unwrap();
        let plan = build_word_selection_plan(&binding, &inventory)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.word_map(),
            &json!({"course-a:course-a_02": ["alpha", "beta"]})
        );

        let test = detail("class-task:2002", "test", -1, "Synthetic List 02");
        let binding =
            CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &test, &units)
                .unwrap();
        let inventory =
            parse_study_task_info_response(STUDY_INFO.as_bytes(), &binding, None).unwrap();
        assert!(
            build_word_selection_plan(&binding, &inventory)
                .unwrap()
                .is_none()
        );
    }

    fn study_document() -> CidarenStudyTaskDocument {
        CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
        .unwrap()
    }

    fn detail(remote_id: &str, task_type: &str, task_id: i64, title: &str) -> RemoteTaskDetail {
        let normalized = json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": task_id,
            "course_id": "course-a",
            "task_type": task_type,
            "progress": 0,
        });
        let task = RemoteTask {
            remote_id: remote_id.to_owned(),
            course_remote_id: Some("course:course-a".to_owned()),
            title: title.to_owned(),
            source_type: if task_type == "test" {
                SourceType::Exam
            } else {
                SourceType::Practice
            },
            assessment_class: AssessmentClass::Routine,
            remote_state: RemoteState::InProgress,
            opens_at: None,
            due_at: None,
            closes_at: None,
            capabilities: Vec::new(),
            fingerprint: "synthetic".to_owned(),
            normalized: normalized.clone(),
            raw_sanitized: serde_json::Map::new().into(),
        };
        RemoteTaskDetail {
            task,
            normalized_detail: json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": "2002",
                "task": normalized,
            }),
        }
    }

    fn query_value<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
        query
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value.as_str()))
    }
}
