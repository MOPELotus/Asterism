use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{QuestionGroupId, QuestionId, TaskId};

const MAX_REMOTE_ID_BYTES: usize = 512;
const MAX_STEM_BYTES: usize = 64 * 1024;
const MAX_OPTION_CONTENT_BYTES: usize = 32 * 1024;
const MAX_EXPLANATION_BYTES: usize = 64 * 1024;
const MAX_OPTIONS: usize = 256;
const MAX_ATTACHMENTS: usize = 64;
const MAX_QUESTION_GROUPS: usize = 1_024;
const MAX_GROUP_CHILDREN: usize = 5_000;
const MAX_ANSWER_ITEMS: usize = 256;
const MAX_COMPOSITE_DEPTH: usize = 8;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_POSITION: u32 = 100_000;
const QUESTION_CONTENT_FINGERPRINT_PREFIX: &str = "v1:";
const QUESTION_SEMANTIC_FINGERPRINT_PREFIX: &str = "semantic-v1:";

/// Stable hash of the exact sanitized Question content used for conservative
/// cache matching. Snapshot-local identities and position are excluded.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct QuestionContentFingerprint(String);

impl QuestionContentFingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QuestionContentFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for QuestionContentFingerprint {
    type Err = QuestionValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digest = value
            .strip_prefix(QUESTION_CONTENT_FINGERPRINT_PREFIX)
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(QuestionValidationError::InvalidFingerprint)?;
        Ok(Self(format!(
            "{QUESTION_CONTENT_FINGERPRINT_PREFIX}{digest}"
        )))
    }
}

/// Conservative semantic identity for one leaf Question plus its complete
/// shared-context ancestry. Snapshot IDs, remote IDs, positions and option
/// labels are excluded; exact content remains required.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct QuestionSemanticFingerprint(String);

impl QuestionSemanticFingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QuestionSemanticFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for QuestionSemanticFingerprint {
    type Err = QuestionValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digest = value
            .strip_prefix(QUESTION_SEMANTIC_FINGERPRINT_PREFIX)
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(QuestionValidationError::InvalidFingerprint)?;
        Ok(Self(format!(
            "{QUESTION_SEMANTIC_FINGERPRINT_PREFIX}{digest}"
        )))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    SingleChoice,
    MultipleChoice,
    TrueFalse,
    FillBlank,
    ShortAnswer,
    Matching,
    Ordering,
    Composite,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAttachmentKind {
    Image,
    Audio,
    Video,
    File,
    Formula,
    Other,
}

/// Sanitized attachment identity and display facts. Fetch URLs, signatures and
/// bearer material are intentionally absent from the persisted domain model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuestionAttachment {
    pub kind: QuestionAttachmentKind,
    pub remote_id: Option<String>,
    pub label: Option<String>,
    pub metadata_sanitized: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuestionOption {
    pub id: String,
    pub content: Option<String>,
    pub attachments: Vec<QuestionAttachment>,
    pub metadata_sanitized: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Question {
    pub id: QuestionId,
    pub task_id: TaskId,
    pub remote_question_id: Option<String>,
    pub kind: QuestionKind,
    pub stem: String,
    pub options: Vec<QuestionOption>,
    pub attachments: Vec<QuestionAttachment>,
    pub metadata_sanitized: Value,
    /// One-based position in the current freshly parsed attempt.
    pub position: u32,
}

/// One ordered child reference inside a shared-context or compound Question
/// group. Leaf Questions retain independent kinds and grading identities.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum QuestionGroupChild {
    Question(QuestionId),
    Group(QuestionGroupId),
}

/// Sanitized shared material for an ordered set of leaf Questions or nested
/// groups. Route data, signed attachment URLs and Provider mutation state are
/// intentionally excluded.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QuestionGroup {
    pub id: QuestionGroupId,
    pub task_id: TaskId,
    pub remote_group_id: Option<String>,
    pub stem: Option<String>,
    pub options: Vec<QuestionOption>,
    pub attachments: Vec<QuestionAttachment>,
    pub metadata_sanitized: Value,
    pub children: Vec<QuestionGroupChild>,
}

impl QuestionGroup {
    /// Validates one bounded, credential-safe shared Question node. Exact
    /// child existence, ownership and acyclicity are checked by
    /// [`validate_question_groups`].
    ///
    /// # Errors
    ///
    /// Rejects malformed shared content, duplicated children, unsafe metadata
    /// or exceeded collection bounds.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        if self
            .remote_group_id
            .as_deref()
            .is_some_and(|value| !valid_text(value, MAX_REMOTE_ID_BYTES))
            || self
                .stem
                .as_deref()
                .is_some_and(|value| !valid_optional_content(value, MAX_STEM_BYTES))
            || self.options.len() > MAX_OPTIONS
            || self.attachments.len() > MAX_ATTACHMENTS
            || self.children.is_empty()
            || self.children.len() > MAX_GROUP_CHILDREN
            || !valid_sanitized_json(&self.metadata_sanitized)
        {
            return Err(QuestionValidationError::InvalidGroup);
        }
        validate_options(&self.options)?;
        validate_attachments(&self.attachments)?;
        let mut children = BTreeSet::new();
        if self.children.iter().any(|child| !children.insert(*child)) {
            return Err(QuestionValidationError::InvalidGroup);
        }
        Ok(())
    }
}

/// Validates one complete snapshot hierarchy. A Question or nested group may
/// have at most one parent, references must remain Task-local, and cycles fail
/// closed. Questions not belonging to a group remain valid ordinary leaves.
///
/// # Errors
///
/// Rejects malformed groups, missing or cross-Task children, multiple parents,
/// duplicate identities and cycles.
pub fn validate_question_groups(
    task_id: TaskId,
    questions: &[Question],
    groups: &[QuestionGroup],
) -> Result<(), QuestionValidationError> {
    if groups.len() > MAX_QUESTION_GROUPS {
        return Err(QuestionValidationError::TooManyItems);
    }
    let question_ids = questions
        .iter()
        .map(|question| question.id)
        .collect::<BTreeSet<_>>();
    if question_ids.len() != questions.len()
        || questions.iter().any(|question| question.task_id != task_id)
    {
        return Err(QuestionValidationError::InvalidGroup);
    }
    let mut group_by_id = BTreeMap::new();
    let mut remote_ids = BTreeSet::new();
    for group in groups {
        if group.task_id != task_id
            || group.validate().is_err()
            || group_by_id.insert(group.id, group).is_some()
            || group
                .remote_group_id
                .as_deref()
                .is_some_and(|remote_id| !remote_ids.insert(remote_id))
        {
            return Err(QuestionValidationError::InvalidGroup);
        }
    }
    let mut parented_questions = BTreeSet::new();
    let mut parented_groups = BTreeSet::new();
    for group in groups {
        for child in &group.children {
            match child {
                QuestionGroupChild::Question(question_id) => {
                    if !question_ids.contains(question_id)
                        || !parented_questions.insert(*question_id)
                    {
                        return Err(QuestionValidationError::InvalidGroup);
                    }
                }
                QuestionGroupChild::Group(group_id) => {
                    if *group_id == group.id
                        || !group_by_id.contains_key(group_id)
                        || !parented_groups.insert(*group_id)
                    {
                        return Err(QuestionValidationError::InvalidGroup);
                    }
                }
            }
        }
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for group_id in group_by_id.keys().copied() {
        visit_question_group(group_id, &group_by_id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_question_group(
    group_id: QuestionGroupId,
    groups: &BTreeMap<QuestionGroupId, &QuestionGroup>,
    visiting: &mut BTreeSet<QuestionGroupId>,
    visited: &mut BTreeSet<QuestionGroupId>,
) -> Result<(), QuestionValidationError> {
    if visited.contains(&group_id) {
        return Ok(());
    }
    if !visiting.insert(group_id) {
        return Err(QuestionValidationError::InvalidGroup);
    }
    let group = groups
        .get(&group_id)
        .ok_or(QuestionValidationError::InvalidGroup)?;
    for child in &group.children {
        if let QuestionGroupChild::Group(child_id) = child {
            visit_question_group(*child_id, groups, visiting, visited)?;
        }
    }
    visiting.remove(&group_id);
    visited.insert(group_id);
    Ok(())
}

/// Borrowed complete Question set used for conservative semantic matching.
/// Construction validates the full hierarchy once before any fingerprint or
/// answer rebinding is attempted.
#[derive(Clone, Copy, Debug)]
pub struct QuestionSetView<'a> {
    task_id: TaskId,
    questions: &'a [Question],
    groups: &'a [QuestionGroup],
}

impl<'a> QuestionSetView<'a> {
    /// # Errors
    ///
    /// Rejects invalid leaf Questions or an invalid structured hierarchy.
    pub fn try_new(
        task_id: TaskId,
        questions: &'a [Question],
        groups: &'a [QuestionGroup],
    ) -> Result<Self, QuestionValidationError> {
        if questions.is_empty()
            || questions
                .iter()
                .any(|question| question.validate().is_err())
        {
            return Err(QuestionValidationError::InvalidQuestion);
        }
        validate_question_groups(task_id, questions, groups)?;
        Ok(Self {
            task_id,
            questions,
            groups,
        })
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn questions(&self) -> &'a [Question] {
        self.questions
    }

    pub const fn groups(&self) -> &'a [QuestionGroup] {
        self.groups
    }

    pub fn question(&self, question_id: QuestionId) -> Option<&'a Question> {
        self.questions
            .iter()
            .find(|question| question.id == question_id)
    }

    /// Returns the leaf's own option set, or the nearest non-empty shared
    /// ancestor option set when the leaf intentionally carries none.
    ///
    /// # Errors
    ///
    /// Rejects an unknown leaf or an inconsistent hierarchy.
    pub fn answer_options(
        &self,
        question_id: QuestionId,
    ) -> Result<&'a [QuestionOption], QuestionValidationError> {
        let question = self
            .question(question_id)
            .ok_or(QuestionValidationError::InvalidQuestion)?;
        if !question.options.is_empty() {
            return Ok(&question.options);
        }
        let mut question_parent = BTreeMap::new();
        let mut group_parent = BTreeMap::new();
        let groups = self
            .groups
            .iter()
            .map(|group| (group.id, group))
            .collect::<BTreeMap<_, _>>();
        for group in self.groups {
            for child in &group.children {
                match child {
                    QuestionGroupChild::Question(child_id) => {
                        question_parent.insert(*child_id, group.id);
                    }
                    QuestionGroupChild::Group(child_id) => {
                        group_parent.insert(*child_id, group.id);
                    }
                }
            }
        }
        let mut current = question_parent.get(&question_id).copied();
        while let Some(group_id) = current {
            let group = groups
                .get(&group_id)
                .ok_or(QuestionValidationError::InvalidGroup)?;
            if !group.options.is_empty() {
                return Ok(&group.options);
            }
            current = group_parent.get(&group_id).copied();
        }
        Ok(&question.options)
    }

    /// Hashes exact semantic leaf content plus all shared ancestors while
    /// ignoring snapshot-local identity, position, option labels and option
    /// ordering. Duplicate semantic options fail closed because they cannot be
    /// uniquely rebound.
    ///
    /// # Errors
    ///
    /// Rejects an unknown leaf, invalid hierarchy, ambiguous semantic options
    /// or an unencodable sanitized representation.
    pub fn semantic_fingerprint(
        &self,
        question_id: QuestionId,
    ) -> Result<QuestionSemanticFingerprint, QuestionValidationError> {
        let question = self
            .question(question_id)
            .ok_or(QuestionValidationError::InvalidQuestion)?;
        let material = semantic_question_material(self, question)?;
        let encoded = serde_json::to_vec(&material)
            .map_err(|_| QuestionValidationError::InvalidFingerprint)?;
        let digest = Sha256::digest(encoded);
        let mut value = String::with_capacity(QUESTION_SEMANTIC_FINGERPRINT_PREFIX.len() + 64);
        value.push_str(QUESTION_SEMANTIC_FINGERPRINT_PREFIX);
        for byte in digest {
            write!(&mut value, "{byte:02x}")
                .map_err(|_| QuestionValidationError::InvalidFingerprint)?;
        }
        Ok(QuestionSemanticFingerprint(value))
    }
}

#[derive(Clone, Debug, Serialize)]
struct SemanticQuestionMaterial {
    ancestors: Vec<SemanticGroupMaterial>,
    kind: QuestionKind,
    provider_native_kind: Option<String>,
    stem: String,
    options: Vec<SemanticOptionMaterial>,
    attachments: Vec<SemanticAttachmentMaterial>,
}

#[derive(Clone, Debug, Serialize)]
struct SemanticGroupMaterial {
    stem: Option<String>,
    options: Vec<SemanticOptionMaterial>,
    attachments: Vec<SemanticAttachmentMaterial>,
}

#[derive(Clone, Debug, Serialize)]
struct SemanticOptionMaterial {
    content: Option<String>,
    attachments: Vec<SemanticAttachmentMaterial>,
}

#[derive(Clone, Debug, Serialize)]
struct SemanticAttachmentMaterial {
    kind: QuestionAttachmentKind,
    label: Option<String>,
    content_identity: Option<String>,
}

fn semantic_question_material(
    view: &QuestionSetView<'_>,
    question: &Question,
) -> Result<SemanticQuestionMaterial, QuestionValidationError> {
    let mut question_parent = BTreeMap::new();
    let mut group_parent = BTreeMap::new();
    let groups = view
        .groups
        .iter()
        .map(|group| (group.id, group))
        .collect::<BTreeMap<_, _>>();
    for group in view.groups {
        for child in &group.children {
            match child {
                QuestionGroupChild::Question(question_id) => {
                    question_parent.insert(*question_id, group.id);
                }
                QuestionGroupChild::Group(group_id) => {
                    group_parent.insert(*group_id, group.id);
                }
            }
        }
    }
    let mut ancestor_ids = Vec::new();
    let mut current = question_parent.get(&question.id).copied();
    while let Some(group_id) = current {
        ancestor_ids.push(group_id);
        current = group_parent.get(&group_id).copied();
    }
    ancestor_ids.reverse();
    let mut ancestors = Vec::with_capacity(ancestor_ids.len());
    for group_id in ancestor_ids {
        let group = groups
            .get(&group_id)
            .ok_or(QuestionValidationError::InvalidGroup)?;
        ancestors.push(SemanticGroupMaterial {
            stem: group.stem.clone(),
            options: semantic_options(&group.options)?,
            attachments: semantic_attachments(&group.attachments),
        });
    }
    Ok(SemanticQuestionMaterial {
        ancestors,
        kind: question.kind,
        provider_native_kind: question
            .metadata_sanitized
            .get("provider_kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stem: question.stem.clone(),
        options: semantic_options(&question.options)?,
        attachments: semantic_attachments(&question.attachments),
    })
}

fn semantic_attachments(attachments: &[QuestionAttachment]) -> Vec<SemanticAttachmentMaterial> {
    attachments
        .iter()
        .map(|attachment| SemanticAttachmentMaterial {
            kind: attachment.kind,
            label: attachment.label.clone(),
            content_identity: attachment
                .metadata_sanitized
                .get("content_sha256")
                .and_then(Value::as_str)
                .map(|digest| format!("sha256:{digest}"))
                .or_else(|| {
                    attachment
                        .remote_id
                        .as_deref()
                        .map(stable_attachment_identity)
                }),
        })
        .collect()
}

fn stable_attachment_identity(value: &str) -> String {
    let without_fragment = value.split_once('#').map_or(value, |(base, _)| base);
    let Some((base, query)) = without_fragment.split_once('?') else {
        return without_fragment.to_owned();
    };
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return without_fragment.to_owned();
    }
    let mut stable = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(key, _)| key);
            let key = key.to_ascii_lowercase();
            !matches!(
                key.as_str(),
                "token"
                    | "access_token"
                    | "auth"
                    | "authorization"
                    | "signature"
                    | "sig"
                    | "expires"
                    | "expiry"
                    | "timestamp"
                    | "ts"
                    | "t"
                    | "nonce"
                    | "random"
            ) && !key.starts_with("x-amz-")
        })
        .collect::<Vec<_>>();
    stable.sort_unstable();
    if stable.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", stable.join("&"))
    }
}

fn semantic_options(
    options: &[QuestionOption],
) -> Result<Vec<SemanticOptionMaterial>, QuestionValidationError> {
    let mut keyed = Vec::with_capacity(options.len());
    for option in options {
        let material = SemanticOptionMaterial {
            content: option.content.clone(),
            attachments: semantic_attachments(&option.attachments),
        };
        let key = serde_json::to_string(&material)
            .map_err(|_| QuestionValidationError::InvalidFingerprint)?;
        keyed.push((key, material));
    }
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    if keyed.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(QuestionValidationError::AmbiguousSemanticOptions);
    }
    Ok(keyed.into_iter().map(|(_, material)| material).collect())
}

/// Rebinds one reusable semantic answer to a fresh Question set. The complete
/// structured fingerprints must match first; choice labels are then mapped by
/// unique option content. Unsupported or ambiguous answer shapes fail closed.
///
/// # Errors
///
/// Rejects invalid sets, semantic drift, ambiguous option content and answer
/// kinds that cannot be conservatively remapped.
pub fn rebind_semantic_answer(
    source: QuestionSetView<'_>,
    source_question_id: QuestionId,
    target: QuestionSetView<'_>,
    target_question_id: QuestionId,
    answer: &NormalizedAnswer,
) -> Result<NormalizedAnswer, SemanticAnswerRebindError> {
    if source
        .semantic_fingerprint(source_question_id)
        .map_err(|_| SemanticAnswerRebindError::InvalidQuestionSet)?
        != target
            .semantic_fingerprint(target_question_id)
            .map_err(|_| SemanticAnswerRebindError::InvalidQuestionSet)?
    {
        return Err(SemanticAnswerRebindError::SemanticMismatch);
    }
    let source_options = source
        .answer_options(source_question_id)
        .map_err(|_| SemanticAnswerRebindError::InvalidQuestionSet)?;
    let target_options = target
        .answer_options(target_question_id)
        .map_err(|_| SemanticAnswerRebindError::InvalidQuestionSet)?;
    let rebound = rebind_normalized_answer(source_options, target_options, answer, 0)?;
    rebound
        .validate()
        .map_err(|_| SemanticAnswerRebindError::UnsupportedAnswer)?;
    Ok(rebound)
}

fn rebind_normalized_answer(
    source_options: &[QuestionOption],
    target_options: &[QuestionOption],
    answer: &NormalizedAnswer,
    depth: usize,
) -> Result<NormalizedAnswer, SemanticAnswerRebindError> {
    if depth > MAX_COMPOSITE_DEPTH {
        return Err(SemanticAnswerRebindError::UnsupportedAnswer);
    }
    match answer {
        NormalizedAnswer::Selections(values) => Ok(NormalizedAnswer::Selections(
            rebind_selection_values(source_options, target_options, values, true)?,
        )),
        NormalizedAnswer::Ordering(values) => Ok(NormalizedAnswer::Ordering(
            rebind_selection_values(source_options, target_options, values, false)?,
        )),
        NormalizedAnswer::Pairs(values) => {
            let mut rebound = Vec::with_capacity(values.len());
            for pair in values {
                let left = rebind_selection_values(
                    source_options,
                    target_options,
                    std::slice::from_ref(&pair.left),
                    true,
                )?;
                let right = rebind_selection_values(
                    source_options,
                    target_options,
                    std::slice::from_ref(&pair.right),
                    true,
                )?;
                rebound.push(AnswerPair {
                    left: left
                        .into_iter()
                        .next()
                        .ok_or(SemanticAnswerRebindError::UnsupportedAnswer)?,
                    right: right
                        .into_iter()
                        .next()
                        .ok_or(SemanticAnswerRebindError::UnsupportedAnswer)?,
                });
            }
            Ok(NormalizedAnswer::Pairs(rebound))
        }
        NormalizedAnswer::Composite(values) => Ok(NormalizedAnswer::Composite(
            values
                .iter()
                .map(|value| {
                    rebind_normalized_answer(source_options, target_options, value, depth + 1)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )),
        NormalizedAnswer::Texts(_) | NormalizedAnswer::Boolean(_) => Ok(answer.clone()),
        NormalizedAnswer::Skip | NormalizedAnswer::Unknown => {
            Err(SemanticAnswerRebindError::UnsupportedAnswer)
        }
    }
}

fn rebind_selection_values(
    source: &[QuestionOption],
    target: &[QuestionOption],
    values: &[String],
    target_order: bool,
) -> Result<Vec<String>, SemanticAnswerRebindError> {
    let mut mapped = Vec::with_capacity(values.len());
    for value in values {
        let source_option = source
            .iter()
            .find(|option| option.id == *value)
            .ok_or(SemanticAnswerRebindError::UnsupportedAnswer)?;
        let source_key = semantic_option_key(source_option)?;
        let matches = target
            .iter()
            .filter_map(|option| {
                semantic_option_key(option)
                    .ok()
                    .filter(|key| *key == source_key)
                    .map(|_| option.id.clone())
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(SemanticAnswerRebindError::AmbiguousOption);
        }
        mapped.push(matches[0].clone());
    }
    if target_order {
        let selected = mapped.into_iter().collect::<BTreeSet<_>>();
        Ok(target
            .iter()
            .filter(|option| selected.contains(&option.id))
            .map(|option| option.id.clone())
            .collect())
    } else {
        Ok(mapped)
    }
}

fn semantic_option_key(option: &QuestionOption) -> Result<String, SemanticAnswerRebindError> {
    serde_json::to_string(&SemanticOptionMaterial {
        content: option.content.clone(),
        attachments: semantic_attachments(&option.attachments),
    })
    .map_err(|_| SemanticAnswerRebindError::InvalidQuestionSet)
}

#[derive(Serialize)]
struct QuestionFingerprintMaterial<'a> {
    kind: QuestionKind,
    stem: &'a str,
    options: &'a [QuestionOption],
    attachments: &'a [QuestionAttachment],
    metadata_sanitized: &'a Value,
}

impl Question {
    /// Validates the bounded, credential-safe normalized Question contract.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError`] for malformed identity/text,
    /// duplicate options, excessive collections, or unsanitized metadata.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        if self.position == 0
            || self.position > MAX_POSITION
            || self
                .remote_question_id
                .as_deref()
                .is_some_and(|value| !valid_text(value, MAX_REMOTE_ID_BYTES))
            || self.stem.len() > MAX_STEM_BYTES
            || self.stem.chars().any(char::is_control)
            || (self.stem.trim().is_empty() && self.attachments.is_empty())
        {
            return Err(QuestionValidationError::InvalidQuestion);
        }
        if self.options.len() > MAX_OPTIONS || self.attachments.len() > MAX_ATTACHMENTS {
            return Err(QuestionValidationError::TooManyItems);
        }
        validate_options(&self.options)?;
        validate_attachments(&self.attachments)?;
        if !valid_sanitized_json(&self.metadata_sanitized) {
            return Err(QuestionValidationError::UnsanitizedMetadata);
        }
        Ok(())
    }

    /// Hashes the complete sanitized semantic shape while excluding IDs and
    /// attempt position. Callers must still scope matches to one Task and reject
    /// duplicate fingerprints before reusing answer evidence.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError`] when the Question is invalid or its
    /// sanitized representation cannot be encoded.
    pub fn content_fingerprint(
        &self,
    ) -> Result<QuestionContentFingerprint, QuestionValidationError> {
        self.validate()?;
        let encoded = serde_json::to_vec(&QuestionFingerprintMaterial {
            kind: self.kind,
            stem: &self.stem,
            options: &self.options,
            attachments: &self.attachments,
            metadata_sanitized: &self.metadata_sanitized,
        })
        .map_err(|_| QuestionValidationError::InvalidFingerprint)?;
        let digest = Sha256::digest(encoded);
        let mut value = String::with_capacity(QUESTION_CONTENT_FINGERPRINT_PREFIX.len() + 64);
        value.push_str(QUESTION_CONTENT_FINGERPRINT_PREFIX);
        for byte in digest {
            write!(&mut value, "{byte:02x}")
                .map_err(|_| QuestionValidationError::InvalidFingerprint)?;
        }
        Ok(QuestionContentFingerprint(value))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    Manual,
    LocalCache,
    ProviderNative,
    Ai,
    ExternalBank,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct AnswerConfidence(u16);

impl AnswerConfidence {
    pub const MAX_BASIS_POINTS: u16 = 10_000;

    /// Creates a confidence value in basis points, where 10,000 is 100%.
    ///
    /// # Errors
    ///
    /// Returns [`AnswerConfidenceError`] when the value exceeds 10,000.
    pub const fn try_new(basis_points: u16) -> Result<Self, AnswerConfidenceError> {
        if basis_points <= Self::MAX_BASIS_POINTS {
            Ok(Self(basis_points))
        } else {
            Err(AnswerConfidenceError::OutOfRange)
        }
    }

    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for AnswerConfidence {
    type Error = AnswerConfidenceError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AnswerConfidence> for u16 {
    fn from(value: AnswerConfidence) -> Self {
        value.basis_points()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnswerPair {
    pub left: String,
    pub right: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum NormalizedAnswer {
    Selections(Vec<String>),
    Texts(Vec<String>),
    Boolean(bool),
    Pairs(Vec<AnswerPair>),
    Ordering(Vec<String>),
    Composite(Vec<Self>),
    /// Explicitly authorizes the Provider's audited skip operation for this
    /// Question. This is a command intent, never an answer value and never a
    /// substitute for `Unknown`.
    Skip,
    Unknown,
}

impl NormalizedAnswer {
    /// Validates bounded normalized answer structure without Provider payloads.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError::InvalidAnswer`] for empty, duplicate,
    /// oversized, or excessively nested answer data.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        self.validate_at_depth(0)
    }

    fn validate_at_depth(&self, depth: usize) -> Result<(), QuestionValidationError> {
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(QuestionValidationError::InvalidAnswer);
        }
        match self {
            Self::Selections(values) | Self::Ordering(values) => {
                validate_unique_answer_values(values)
            }
            Self::Texts(values) => validate_answer_values(values),
            Self::Pairs(values) => {
                if values.is_empty() || values.len() > MAX_ANSWER_ITEMS {
                    return Err(QuestionValidationError::InvalidAnswer);
                }
                let mut left = BTreeSet::new();
                for pair in values {
                    if !valid_answer_text(&pair.left)
                        || !valid_answer_text(&pair.right)
                        || !left.insert(pair.left.as_str())
                    {
                        return Err(QuestionValidationError::InvalidAnswer);
                    }
                }
                Ok(())
            }
            Self::Composite(values) => {
                if values.is_empty() || values.len() > MAX_ANSWER_ITEMS {
                    return Err(QuestionValidationError::InvalidAnswer);
                }
                values
                    .iter()
                    .try_for_each(|value| value.validate_at_depth(depth + 1))
            }
            Self::Boolean(_) | Self::Skip | Self::Unknown => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AnswerCandidate {
    pub question_id: QuestionId,
    pub source: AnswerSource,
    pub answer: NormalizedAnswer,
    pub confidence: Option<AnswerConfidence>,
    pub explanation: Option<String>,
    pub provenance_sanitized: Value,
}

impl AnswerCandidate {
    /// Validates normalized answer, bounded explanation and sanitized
    /// provenance independently from Provider parsing and submission.
    ///
    /// # Errors
    ///
    /// Returns [`QuestionValidationError`] when any candidate field is unsafe.
    pub fn validate(&self) -> Result<(), QuestionValidationError> {
        self.answer.validate()?;
        if self
            .explanation
            .as_deref()
            .is_some_and(|value| !valid_optional_content(value, MAX_EXPLANATION_BYTES))
            || !valid_sanitized_json(&self.provenance_sanitized)
        {
            return Err(QuestionValidationError::InvalidAnswer);
        }
        Ok(())
    }
}

fn validate_attachments(attachments: &[QuestionAttachment]) -> Result<(), QuestionValidationError> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(QuestionValidationError::TooManyItems);
    }
    for attachment in attachments {
        if attachment
            .remote_id
            .as_deref()
            .is_some_and(|value| !valid_text(value, MAX_REMOTE_ID_BYTES))
            || attachment
                .label
                .as_deref()
                .is_some_and(|value| !valid_optional_content(value, MAX_OPTION_CONTENT_BYTES))
            || !valid_sanitized_json(&attachment.metadata_sanitized)
        {
            return Err(QuestionValidationError::InvalidAttachment);
        }
    }
    Ok(())
}

fn validate_options(options: &[QuestionOption]) -> Result<(), QuestionValidationError> {
    let mut option_ids = BTreeSet::new();
    for option in options {
        if !valid_text(&option.id, MAX_REMOTE_ID_BYTES)
            || option
                .content
                .as_deref()
                .is_some_and(|value| !valid_optional_content(value, MAX_OPTION_CONTENT_BYTES))
            || (option.content.is_none() && option.attachments.is_empty())
            || option.attachments.len() > MAX_ATTACHMENTS
            || !option_ids.insert(option.id.as_str())
            || !valid_sanitized_json(&option.metadata_sanitized)
        {
            return Err(QuestionValidationError::InvalidOption);
        }
        validate_attachments(&option.attachments)?;
    }
    Ok(())
}

fn validate_unique_answer_values(values: &[String]) -> Result<(), QuestionValidationError> {
    validate_answer_values(values)?;
    let mut unique = BTreeSet::new();
    if values.iter().all(|value| unique.insert(value.as_str())) {
        Ok(())
    } else {
        Err(QuestionValidationError::InvalidAnswer)
    }
}

fn validate_answer_values(values: &[String]) -> Result<(), QuestionValidationError> {
    if values.is_empty()
        || values.len() > MAX_ANSWER_ITEMS
        || values.iter().any(|value| !valid_answer_text(value))
    {
        Err(QuestionValidationError::InvalidAnswer)
    } else {
        Ok(())
    }
}

fn valid_answer_text(value: &str) -> bool {
    valid_optional_content(value, MAX_OPTION_CONTENT_BYTES)
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_optional_content(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_sanitized_json(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= MAX_JSON_BYTES)
        && !contains_sensitive_key(value)
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized: String = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                normalized.as_str(),
                "cookie"
                    | "authorization"
                    | "password"
                    | "accesstoken"
                    | "refreshtoken"
                    | "sessionsecret"
                    | "clientsecret"
            ) || contains_sensitive_key(value)
        }),
        Value::Array(items) => items.iter().any(contains_sensitive_key),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnswerConfidenceError {
    #[error("answer confidence must be between 0 and 10000 basis points")]
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuestionValidationError {
    #[error("question identity, stem, or position is invalid")]
    InvalidQuestion,
    #[error("question contains too many options or attachments")]
    TooManyItems,
    #[error("question option is malformed or duplicated")]
    InvalidOption,
    #[error("question group is malformed, cross-bound, duplicated, or cyclic")]
    InvalidGroup,
    #[error("question attachment is malformed or unsanitized")]
    InvalidAttachment,
    #[error("question metadata is oversized or not sanitized")]
    UnsanitizedMetadata,
    #[error("normalized answer or candidate data is invalid")]
    InvalidAnswer,
    #[error("question content fingerprint is invalid")]
    InvalidFingerprint,
    #[error("question options do not have unique semantic identities")]
    AmbiguousSemanticOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SemanticAnswerRebindError {
    #[error("question set is invalid for semantic matching")]
    InvalidQuestionSet,
    #[error("source and target question semantics differ")]
    SemanticMismatch,
    #[error("answer shape cannot be safely rebound")]
    UnsupportedAnswer,
    #[error("option content does not map uniquely")]
    AmbiguousOption,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_question() -> Question {
        Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("question-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Which option is correct?".to_owned(),
            options: vec![
                QuestionOption {
                    id: "A".to_owned(),
                    content: Some("First".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
                QuestionOption {
                    id: "B".to_owned(),
                    content: Some("Second".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"provider_kind": "single"}),
            position: 1,
        }
    }

    #[test]
    fn question_contract_accepts_bounded_sanitized_content() {
        assert_eq!(valid_question().validate(), Ok(()));
        assert_eq!(
            AnswerConfidence::try_new(9_500).unwrap().basis_points(),
            9_500
        );
    }

    #[test]
    fn question_contract_rejects_duplicate_options_and_secret_metadata() {
        let mut duplicate = valid_question();
        duplicate.options[1].id = "A".to_owned();
        assert_eq!(
            duplicate.validate(),
            Err(QuestionValidationError::InvalidOption)
        );

        let mut secret = valid_question();
        secret.metadata_sanitized = serde_json::json!({"access_token": "forbidden"});
        assert_eq!(
            secret.validate(),
            Err(QuestionValidationError::UnsanitizedMetadata)
        );
    }

    #[test]
    fn normalized_answers_are_typed_bounded_and_source_independent() {
        let question = valid_question();
        let candidate = AnswerCandidate {
            question_id: question.id,
            source: AnswerSource::ExternalBank,
            answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
            confidence: Some(AnswerConfidence::try_new(8_000).unwrap()),
            explanation: Some("Matched by normalized stem.".to_owned()),
            provenance_sanitized: serde_json::json!({"bank": "local-fixture"}),
        };
        assert_eq!(candidate.validate(), Ok(()));
        assert_eq!(NormalizedAnswer::Skip.validate(), Ok(()));

        let duplicate = NormalizedAnswer::Ordering(vec!["A".to_owned(), "A".to_owned()]);
        assert_eq!(
            duplicate.validate(),
            Err(QuestionValidationError::InvalidAnswer)
        );
        assert_eq!(
            AnswerConfidence::try_new(10_001),
            Err(AnswerConfidenceError::OutOfRange)
        );
        assert!(serde_json::from_str::<AnswerConfidence>("10001").is_err());
    }

    #[test]
    fn content_fingerprint_excludes_attempt_identity_but_keeps_exact_semantics() {
        let original = valid_question();
        let fingerprint = original.content_fingerprint().unwrap();
        assert_eq!(fingerprint.as_str().len(), 67);
        assert_eq!(fingerprint.to_string().parse(), Ok(fingerprint.clone()));

        let mut next_attempt = original.clone();
        next_attempt.id = QuestionId::new();
        next_attempt.task_id = TaskId::new();
        next_attempt.remote_question_id = Some("fresh-attempt-question".to_owned());
        next_attempt.position = 99;
        assert_eq!(next_attempt.content_fingerprint().unwrap(), fingerprint);

        let mut changed_option = original.clone();
        changed_option.options[0].content = Some("Changed".to_owned());
        assert_ne!(changed_option.content_fingerprint().unwrap(), fingerprint);

        let mut changed_option_id = original;
        changed_option_id.options[0].id = "C".to_owned();
        assert_ne!(
            changed_option_id.content_fingerprint().unwrap(),
            fingerprint
        );
        assert!("v1:ABC".parse::<QuestionContentFingerprint>().is_err());
    }

    #[test]
    fn shared_question_groups_are_ordered_nested_and_task_bound() {
        let first = valid_question();
        let mut second = first.clone();
        second.id = QuestionId::new();
        second.remote_question_id = Some("question-2".to_owned());
        second.position = 2;
        let nested_id = QuestionGroupId::new();
        let root_id = QuestionGroupId::new();
        let nested = QuestionGroup {
            id: nested_id,
            task_id: first.task_id,
            remote_group_id: Some("material-child".to_owned()),
            stem: Some("Choose from the shared options.".to_owned()),
            options: first.options.clone(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"kind": "common_options"}),
            children: vec![QuestionGroupChild::Question(second.id)],
        };
        let root = QuestionGroup {
            id: root_id,
            task_id: first.task_id,
            remote_group_id: Some("material-root".to_owned()),
            stem: Some("Read the passage.".to_owned()),
            options: Vec::new(),
            attachments: vec![QuestionAttachment {
                kind: QuestionAttachmentKind::Image,
                remote_id: Some("image-1".to_owned()),
                label: Some("Figure one".to_owned()),
                metadata_sanitized: serde_json::json!({}),
            }],
            metadata_sanitized: serde_json::json!({"kind": "reading"}),
            children: vec![
                QuestionGroupChild::Question(first.id),
                QuestionGroupChild::Group(nested_id),
            ],
        };
        let questions = [first.clone(), second.clone()];
        assert_eq!(
            validate_question_groups(first.task_id, &questions, &[root.clone(), nested.clone()]),
            Ok(())
        );

        let mut duplicate_parent = root.clone();
        duplicate_parent
            .children
            .push(QuestionGroupChild::Question(second.id));
        assert_eq!(
            validate_question_groups(
                first.task_id,
                &questions,
                &[duplicate_parent, nested.clone()],
            ),
            Err(QuestionValidationError::InvalidGroup)
        );

        let mut cycle = nested;
        cycle.children = vec![QuestionGroupChild::Group(root_id)];
        assert_eq!(
            validate_question_groups(first.task_id, &questions, &[root, cycle]),
            Err(QuestionValidationError::InvalidGroup)
        );
    }

    #[test]
    fn semantic_fingerprint_rebinds_only_unique_reordered_options() {
        let source_question = valid_question();
        let source_group = QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: source_question.task_id,
            remote_group_id: Some("source-passage".to_owned()),
            stem: Some("Shared passage".to_owned()),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"family": "reading"}),
            children: vec![QuestionGroupChild::Question(source_question.id)],
        };
        let mut target_question = source_question.clone();
        target_question.id = QuestionId::new();
        target_question.remote_question_id = Some("target-question".to_owned());
        target_question.position = 17;
        target_question.options = vec![
            QuestionOption {
                id: "Y".to_owned(),
                content: Some("Second".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            },
            QuestionOption {
                id: "X".to_owned(),
                content: Some("First".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            },
        ];
        let target_group = QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: target_question.task_id,
            remote_group_id: Some("target-passage".to_owned()),
            stem: source_group.stem.clone(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: source_group.metadata_sanitized.clone(),
            children: vec![QuestionGroupChild::Question(target_question.id)],
        };
        let source_questions = [source_question.clone()];
        let source_groups = [source_group];
        let target_questions = [target_question.clone()];
        let target_groups = [target_group.clone()];
        let source =
            QuestionSetView::try_new(source_question.task_id, &source_questions, &source_groups)
                .unwrap();
        let target =
            QuestionSetView::try_new(target_question.task_id, &target_questions, &target_groups)
                .unwrap();
        assert_eq!(
            source.semantic_fingerprint(source_question.id).unwrap(),
            target.semantic_fingerprint(target_question.id).unwrap()
        );
        assert_eq!(
            rebind_semantic_answer(
                source,
                source_question.id,
                target,
                target_question.id,
                &NormalizedAnswer::Selections(vec!["A".to_owned()]),
            ),
            Ok(NormalizedAnswer::Selections(vec!["X".to_owned()]))
        );

        let mut relabeled_question = target_question.clone();
        relabeled_question.options[0].metadata_sanitized =
            serde_json::json!({"provider_option_id": "remote-99"});
        relabeled_question.options[1].metadata_sanitized =
            serde_json::json!({"provider_option_id": "remote-42"});
        let relabeled_questions = [relabeled_question.clone()];
        let relabeled = QuestionSetView::try_new(
            relabeled_question.task_id,
            &relabeled_questions,
            &target_groups,
        )
        .unwrap();
        assert_eq!(
            source.semantic_fingerprint(source_question.id).unwrap(),
            relabeled
                .semantic_fingerprint(relabeled_question.id)
                .unwrap(),
            "Provider labels and remote option IDs are submission bindings, not cache identity"
        );

        let mut changed_group = target_group;
        changed_group.stem = Some("Different passage".to_owned());
        let changed_groups = [changed_group];
        let changed =
            QuestionSetView::try_new(target_question.task_id, &target_questions, &changed_groups)
                .unwrap();
        assert_eq!(
            rebind_semantic_answer(
                source,
                source_question.id,
                changed,
                target_question.id,
                &NormalizedAnswer::Selections(vec!["A".to_owned()]),
            ),
            Err(SemanticAnswerRebindError::SemanticMismatch)
        );

        let mut ambiguous_question = target_question.clone();
        ambiguous_question.options[1].content = Some("Second".to_owned());
        let ambiguous_questions = [ambiguous_question.clone()];
        let ambiguous_group = QuestionGroup {
            children: vec![QuestionGroupChild::Question(ambiguous_question.id)],
            ..target_groups[0].clone()
        };
        let ambiguous_groups = [ambiguous_group];
        let ambiguous = QuestionSetView::try_new(
            ambiguous_question.task_id,
            &ambiguous_questions,
            &ambiguous_groups,
        )
        .unwrap();
        assert_eq!(
            ambiguous.semantic_fingerprint(ambiguous_question.id),
            Err(QuestionValidationError::AmbiguousSemanticOptions)
        );
    }

    #[test]
    fn attachment_identity_ignores_volatile_url_signatures_but_keeps_resource_keys() {
        assert_eq!(
            stable_attachment_identity(
                "https://cdn.example/q.png?token=first&objectId=42&expires=1#preview"
            ),
            "https://cdn.example/q.png?objectId=42"
        );
        assert_eq!(
            stable_attachment_identity(
                "https://cdn.example/q.png?expires=2&objectId=42&signature=second"
            ),
            "https://cdn.example/q.png?objectId=42"
        );
        assert_ne!(
            stable_attachment_identity("https://cdn.example/q.png?objectId=42&token=a"),
            stable_attachment_identity("https://cdn.example/q.png?objectId=43&token=a")
        );
    }

    #[test]
    fn semantic_rebind_uses_the_nearest_shared_option_set() {
        let mut source_question = valid_question();
        let source_options = std::mem::take(&mut source_question.options);
        let source_group = QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: source_question.task_id,
            remote_group_id: Some("source-common-options".to_owned()),
            stem: Some("Choose from the common options.".to_owned()),
            options: source_options,
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({"family": "common_options"}),
            children: vec![QuestionGroupChild::Question(source_question.id)],
        };
        let mut target_question = source_question.clone();
        target_question.id = QuestionId::new();
        target_question.remote_question_id = Some("target-common-option-child".to_owned());
        target_question.position = 9;
        let target_group = QuestionGroup {
            id: QuestionGroupId::new(),
            task_id: target_question.task_id,
            remote_group_id: Some("target-common-options".to_owned()),
            stem: source_group.stem.clone(),
            options: vec![
                QuestionOption {
                    id: "Y".to_owned(),
                    content: Some("Second".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
                QuestionOption {
                    id: "X".to_owned(),
                    content: Some("First".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: serde_json::json!({}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: source_group.metadata_sanitized.clone(),
            children: vec![QuestionGroupChild::Question(target_question.id)],
        };
        let source_questions = [source_question.clone()];
        let source_groups = [source_group];
        let target_questions = [target_question.clone()];
        let target_groups = [target_group];
        let source =
            QuestionSetView::try_new(source_question.task_id, &source_questions, &source_groups)
                .unwrap();
        let target =
            QuestionSetView::try_new(target_question.task_id, &target_questions, &target_groups)
                .unwrap();

        assert_eq!(
            rebind_semantic_answer(
                source,
                source_question.id,
                target,
                target_question.id,
                &NormalizedAnswer::Selections(vec!["A".to_owned()]),
            ),
            Ok(NormalizedAnswer::Selections(vec!["X".to_owned()]))
        );
    }
}
