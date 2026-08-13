use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use asterism_domain::{
    AnswerCandidate, AnswerConfidence, AnswerPair, AnswerSource, NormalizedAnswer, Question,
    QuestionKind,
};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

const MAX_WORDS: usize = 100_000;
const MAX_WORD_BYTES: usize = 1_024;
const MAX_MEANINGS: usize = 1_024;
const MAX_EXAMPLES: usize = 10_000;
const MAX_TEXT_BYTES: usize = 32 * 1_024;

/// One bounded word-info record normalized from either donor-observed response
/// family (`means` or nested `options`).
pub struct CidarenWordEvidence {
    word: String,
    meanings: Vec<String>,
    examples: Vec<CidarenExampleEvidence>,
}

impl fmt::Debug for CidarenWordEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenWordEvidence")
            .field("word", &"[REDACTED]")
            .field("meaning_count", &self.meanings.len())
            .field("example_count", &self.examples.len())
            .finish()
    }
}

impl Drop for CidarenWordEvidence {
    fn drop(&mut self) {
        self.word.zeroize();
        self.meanings.iter_mut().for_each(Zeroize::zeroize);
        for example in &mut self.examples {
            example.english.zeroize();
            example.chinese.zeroize();
        }
    }
}

struct CidarenExampleEvidence {
    english: String,
    chinese: String,
    use_kind: CidarenEvidenceUse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CidarenEvidenceUse {
    Phrase,
    Example,
}

/// Fresh answer evidence for one Cidaren Question. Debug output is redacted
/// and every word/meaning/example is zeroized when the operation ends.
pub struct CidarenAnswerEvidence {
    word_list: Vec<String>,
    by_word: BTreeMap<String, CidarenWordEvidence>,
    aliases: Vec<(String, String)>,
}

impl CidarenAnswerEvidence {
    /// Binds a current Task word list and independently fetched word-info
    /// records into one duplicate-free answer snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for unsafe, duplicate or oversized evidence.
    pub fn try_new(
        word_list: Vec<String>,
        word_infos: Vec<CidarenWordEvidence>,
    ) -> ProviderResult<Self> {
        Self::try_new_with_aliases(word_list, word_infos, Vec::new())
    }

    pub(crate) fn try_new_with_aliases(
        mut word_list: Vec<String>,
        word_infos: Vec<CidarenWordEvidence>,
        mut aliases: Vec<(String, String)>,
    ) -> ProviderResult<Self> {
        if word_list.len() > MAX_WORDS || word_infos.len() > MAX_WORDS {
            word_list.iter_mut().for_each(Zeroize::zeroize);
            zeroize_aliases(&mut aliases);
            return Err(invalid_response(
                "Cidaren answer evidence exceeds the word limit",
            ));
        }
        let mut normalized_words = BTreeSet::new();
        for word in &word_list {
            if !valid_text(word, MAX_WORD_BYTES) || !normalized_words.insert(word.to_lowercase()) {
                word_list.iter_mut().for_each(Zeroize::zeroize);
                zeroize_aliases(&mut aliases);
                return Err(invalid_response(
                    "Cidaren answer evidence contains an invalid or duplicate word",
                ));
            }
        }
        let mut by_word = BTreeMap::new();
        for info in word_infos {
            let key = info.word.to_lowercase();
            if by_word.insert(key, info).is_some() {
                word_list.iter_mut().for_each(Zeroize::zeroize);
                zeroize_aliases(&mut aliases);
                return Err(invalid_response(
                    "Cidaren answer evidence contains duplicate word info",
                ));
            }
        }
        let mut unique_aliases = BTreeSet::new();
        if aliases.len() > MAX_WORDS
            || aliases.iter().any(|(alias, target)| {
                !valid_text(alias, MAX_WORD_BYTES)
                    || !valid_text(target, MAX_WORD_BYTES)
                    || !by_word.contains_key(target)
                    || !unique_aliases.insert(alias)
            })
        {
            word_list.iter_mut().for_each(Zeroize::zeroize);
            zeroize_aliases(&mut aliases);
            return Err(invalid_response(
                "Cidaren answer evidence contains an invalid prototype alias",
            ));
        }
        Ok(Self {
            word_list,
            by_word,
            aliases,
        })
    }

    fn info(&self, word: &str) -> Option<&CidarenWordEvidence> {
        let key = word.to_lowercase();
        self.by_word.get(&key).or_else(|| {
            self.aliases
                .iter()
                .find_map(|(alias, target)| (alias == &key).then_some(target))
                .and_then(|target| self.by_word.get(target))
        })
    }

    fn contains_task_word(&self, word: &str) -> bool {
        self.word_list.iter().any(|candidate| candidate == word)
    }
}

impl fmt::Debug for CidarenAnswerEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenAnswerEvidence")
            .field("word_count", &self.word_list.len())
            .field("info_count", &self.by_word.len())
            .field("alias_count", &self.aliases.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenAnswerEvidence {
    fn drop(&mut self) {
        self.word_list.iter_mut().for_each(Zeroize::zeroize);
        zeroize_aliases(&mut self.aliases);
    }
}

fn zeroize_aliases(aliases: &mut [(String, String)]) {
    for (alias, target) in aliases {
        alias.zeroize();
        target.zeroize();
    }
}

/// Parses a decoded `Course/StudyWordInfo` payload into bounded answer
/// evidence without retaining audio URLs, source IDs or unrelated fields.
///
/// # Errors
///
/// Returns `ProtocolDrift` or `InvalidResponse` for unknown shapes, malformed
/// text or exceeded collection bounds.
pub fn parse_word_evidence(payload: &Value) -> ProviderResult<CidarenWordEvidence> {
    let object = payload
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren word-info payload is not an object"))?;
    let word = required_text(object.get("word"), MAX_WORD_BYTES, "word")?;
    let mut meanings = Vec::new();
    let mut examples = Vec::new();
    let means_entries = optional_evidence_array(object.get("means"), "mean")?;
    let option_entries = optional_evidence_array(object.get("options"), "option")?;
    if let Some(entries) = means_entries.filter(|entries| !entries.is_empty()) {
        if entries.len() > MAX_MEANINGS {
            return Err(invalid_response(
                "Cidaren word-info meanings exceed the limit",
            ));
        }
        for entry in entries {
            parse_means_entry(entry, &mut meanings, &mut examples)?;
        }
    } else if let Some(entries) = option_entries {
        if entries.len() > MAX_MEANINGS {
            return Err(invalid_response(
                "Cidaren word-info options exceed the limit",
            ));
        }
        for entry in entries {
            parse_options_entry(entry, &mut meanings, &mut examples)?;
        }
    } else if means_entries.is_none() {
        return Err(protocol_drift(
            "Cidaren word-info payload has no audited meaning family",
        ));
    }
    if meanings.len() > MAX_MEANINGS || examples.len() > MAX_EXAMPLES {
        return Err(invalid_response(
            "Cidaren word-info payload exceeds the evidence limit",
        ));
    }
    Ok(CidarenWordEvidence {
        word,
        meanings,
        examples,
    })
}

fn optional_evidence_array<'a>(
    value: Option<&'a Value>,
    family: &'static str,
) -> ProviderResult<Option<&'a [Value]>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(entries)) => Ok(Some(entries)),
        Some(_) => Err(protocol_drift(format!(
            "Cidaren word-info {family} family is not an array"
        ))),
    }
}

/// Resolves one parsed Cidaren Question from a fresh bounded word-evidence
/// snapshot using the audited donor behavior for its topic mode.
///
/// Every donor-observed fallback remains explicit: mode 13 uses the fixed
/// fourth choice, meaning/sentence misses use the third choice, word-to-meaning
/// misses use stable pseudo-random selection, and completion misses use the
/// last fresh Task word. Each fallback is marked with low confidence and
/// sanitized provenance; malformed protocol evidence still fails closed.
///
/// # Errors
///
/// Returns typed Provider errors for foreign Question metadata, missing fresh
/// evidence or a donor-observed mode whose answer cannot be resolved.
pub fn resolve_answer_candidate(
    question: &Question,
    evidence: &CidarenAnswerEvidence,
) -> ProviderResult<AnswerCandidate> {
    question
        .validate()
        .map_err(|_| invalid_response("Cidaren answer resolver received an invalid Question"))?;
    if question
        .metadata_sanitized
        .get("schema")
        .and_then(Value::as_str)
        != Some("cidaren.attempt-question.v1")
    {
        return Err(protocol_drift(
            "Cidaren answer resolver received a foreign Question",
        ));
    }
    let mode = question
        .metadata_sanitized
        .get("topic_mode")
        .and_then(Value::as_i64)
        .ok_or_else(|| protocol_drift("Cidaren Question has no topic mode"))?;
    let (answer, confidence, strategy) = match mode {
        11 | 15 | 16 | 21 | 22 => match resolve_word_to_meaning(question, evidence)? {
            Some(answer) => (answer, 10_000, "word-to-meaning"),
            None => donor_stable_random_choice(question)?,
        },
        13 => (
            donor_fixed_fourth_choice(question)?,
            2_500,
            "donor-fixed-fourth-choice",
        ),
        17 | 18 => match resolve_meaning_to_word(question, evidence)? {
            Some(answer) => (answer, 10_000, "meaning-to-word"),
            None => donor_third_choice(question, "donor-third-choice-fallback")?,
        },
        31 => (resolve_matching(question)?, 10_000, "relation-matching"),
        32 => (
            resolve_translation_text(question, evidence)?,
            9_000,
            "phrase-translation",
        ),
        41..=44 => match resolve_sentence_choice(question, evidence)? {
            Some(answer) => (answer, 9_000, "example-sentence"),
            None => donor_third_choice(question, "donor-third-choice-fallback")?,
        },
        51..=54 => resolve_word_completion(question, evidence)?,
        _ => {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Cidaren answer resolver does not recognize this topic mode",
            ));
        }
    };
    let candidate = AnswerCandidate {
        question_id: question.id,
        source: AnswerSource::ProviderNative,
        answer,
        confidence: Some(
            AnswerConfidence::try_new(confidence)
                .map_err(|_| invalid_response("Cidaren answer confidence is invalid"))?,
        ),
        explanation: None,
        provenance_sanitized: json!({
            "schema": "cidaren.answer-resolution.v1",
            "topic_mode": mode,
            "strategy": strategy,
        }),
    };
    candidate
        .validate()
        .map_err(|_| invalid_response("Cidaren resolved answer is invalid"))?;
    Ok(candidate)
}

fn resolve_word_to_meaning(
    question: &Question,
    evidence: &CidarenAnswerEvidence,
) -> ProviderResult<Option<NormalizedAnswer>> {
    require_kind(question, QuestionKind::SingleChoice)?;
    let prompt = metadata_text(question, "prompt_content")?;
    let word = braced_word(prompt).unwrap_or(prompt);
    let info = evidence
        .info(word)
        .or_else(|| stripped_inflection(word).and_then(|word| evidence.info(word)))
        .ok_or_else(|| remote_changed("Cidaren word evidence is missing for the current prompt"))?;
    let option = question
        .options
        .iter()
        .find(|option| {
            option.content.as_deref().is_some_and(|content| {
                info.meanings
                    .iter()
                    .any(|meaning| semantic_equal(content, meaning) || content.contains(meaning))
            })
        })
        .map(|option| NormalizedAnswer::Selections(vec![option.id.clone()]));
    Ok(option)
}

fn resolve_meaning_to_word(
    question: &Question,
    evidence: &CidarenAnswerEvidence,
) -> ProviderResult<Option<NormalizedAnswer>> {
    require_kind(question, QuestionKind::SingleChoice)?;
    let target = metadata_text(question, "prompt_content")?;
    let option = question
        .options
        .iter()
        .find(|option| {
            option
                .content
                .as_deref()
                .and_then(|word| evidence.info(word))
                .is_some_and(|info| {
                    info.meanings
                        .iter()
                        .any(|meaning| semantic_equal(target, meaning))
                })
        })
        .map(|option| NormalizedAnswer::Selections(vec![option.id.clone()]));
    Ok(option)
}

fn donor_fixed_fourth_choice(question: &Question) -> ProviderResult<NormalizedAnswer> {
    require_kind(question, QuestionKind::SingleChoice)?;
    let option = question
        .options
        .iter()
        .find(|option| option.id == "n:3")
        .ok_or_else(|| protocol_drift("Cidaren donor fallback option is absent"))?;
    Ok(NormalizedAnswer::Selections(vec![option.id.clone()]))
}

fn resolve_matching(question: &Question) -> ProviderResult<NormalizedAnswer> {
    require_kind(question, QuestionKind::Matching)?;
    let relations = question
        .metadata_sanitized
        .get("relations")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Cidaren matching Question has no relations"))?;
    let pairs = relations
        .iter()
        .map(|relation| {
            let relation = relation
                .as_str()
                .ok_or_else(|| protocol_drift("Cidaren matching relation is invalid"))?;
            let option = question
                .options
                .iter()
                .find(|option| option.content.as_deref() == Some(relation))
                .ok_or_else(|| {
                    remote_changed("Cidaren matching relation is absent from options")
                })?;
            Ok(AnswerPair {
                left: relation.to_owned(),
                right: option.id.clone(),
            })
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    Ok(NormalizedAnswer::Pairs(pairs))
}

fn resolve_translation_text(
    question: &Question,
    evidence: &CidarenAnswerEvidence,
) -> ProviderResult<NormalizedAnswer> {
    require_kind(question, QuestionKind::ShortAnswer)?;
    let target = metadata_text(question, "prompt_remark")?;
    for option in &question.options {
        let Some(word) = option.content.as_deref() else {
            continue;
        };
        let Some(info) = evidence.info(word) else {
            continue;
        };
        if let Some(answer) = matching_phrase_answer(info, target) {
            return Ok(NormalizedAnswer::Texts(vec![answer]));
        }
    }
    Err(remote_changed(
        "Cidaren translation evidence does not match the current prompt",
    ))
}

fn resolve_sentence_choice(
    question: &Question,
    evidence: &CidarenAnswerEvidence,
) -> ProviderResult<Option<NormalizedAnswer>> {
    require_kind(question, QuestionKind::SingleChoice)?;
    let target = metadata_text(question, "prompt_remark")?;
    let mut top_level = BTreeMap::new();
    for (fallback_index, option) in question.options.iter().enumerate() {
        let index = option
            .metadata_sanitized
            .get("top_level_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(fallback_index);
        let content = option
            .metadata_sanitized
            .get("top_level_content")
            .and_then(Value::as_str)
            .or_else(|| {
                option
                    .content
                    .as_deref()
                    .and_then(|value| value.rsplit(" — ").next())
            })
            .ok_or_else(|| protocol_drift("Cidaren sentence option has no content"))?;
        match top_level.get(&index) {
            Some(existing) if existing != &content => {
                return Err(protocol_drift(
                    "Cidaren nested options disagree on their parent content",
                ));
            }
            Some(_) => {}
            None => {
                top_level.insert(index, content);
            }
        }
    }
    let mut donor_order = Vec::with_capacity(top_level.len());
    for content in top_level.into_values() {
        if evidence.contains_task_word(content) {
            donor_order.insert(0, content);
        } else {
            donor_order.push(content);
        }
    }
    for candidate in donor_order {
        let Some(info) = evidence.info(candidate) else {
            continue;
        };
        let Some(answer) = matching_evidence(info, target, CidarenEvidenceUse::Example)
            .and_then(|example| braced_word(&example.english))
        else {
            continue;
        };
        return Ok(question
            .options
            .iter()
            .find(|option| {
                option
                    .metadata_sanitized
                    .get("wire_content")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        option
                            .content
                            .as_deref()
                            .map(|value| value.rsplit(" — ").next().unwrap_or(value))
                    })
                    == Some(answer)
            })
            .map(|option| NormalizedAnswer::Selections(vec![option.id.clone()])));
    }
    Ok(None)
}

fn resolve_word_completion(
    question: &Question,
    evidence: &CidarenAnswerEvidence,
) -> ProviderResult<(NormalizedAnswer, u16, &'static str)> {
    require_kind(question, QuestionKind::ShortAnswer)?;
    let tip = metadata_text(question, "word_tip")?.to_lowercase();
    let length = question
        .metadata_sanitized
        .get("word_lengths")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| protocol_drift("Cidaren completion Question has no word length"))?;
    for word in &evidence.word_list {
        if !word.to_lowercase().starts_with(&tip) {
            continue;
        }
        if word.chars().count() == length {
            return Ok((
                NormalizedAnswer::Texts(vec![word.clone()]),
                9_000,
                "word-prefix-length",
            ));
        }
        if word.chars().count().saturating_add(1) == length {
            return Ok((
                NormalizedAnswer::Texts(vec![format!("{word}s")]),
                9_000,
                "word-prefix-length-plus-s",
            ));
        }
        let target = metadata_text(question, "prompt_remark")?;
        if let Some(answer) = evidence
            .info(word)
            .and_then(|info| matching_evidence(info, target, CidarenEvidenceUse::Example))
            .and_then(|example| braced_word(&example.english))
        {
            return Ok((
                NormalizedAnswer::Texts(vec![answer.to_owned()]),
                9_000,
                "word-completion-example",
            ));
        }
    }
    evidence
        .word_list
        .last()
        .cloned()
        .map(|word| {
            (
                NormalizedAnswer::Texts(vec![word]),
                500,
                "donor-last-word-fallback",
            )
        })
        .ok_or_else(|| remote_changed("Cidaren current Task word list is empty"))
}

fn donor_stable_random_choice(
    question: &Question,
) -> ProviderResult<(NormalizedAnswer, u16, &'static str)> {
    require_kind(question, QuestionKind::SingleChoice)?;
    if question.options.is_empty() {
        return Err(protocol_drift(
            "Cidaren donor random fallback has no options",
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"asterism:cidaren:donor-random-choice:v1\0");
    digest.update(question.id.to_string().as_bytes());
    digest.update(b"\0");
    if let Some(remote_id) = &question.remote_question_id {
        digest.update(remote_id.as_bytes());
    }
    let digest = digest.finalize();
    let index = usize::from(u16::from_be_bytes([digest[0], digest[1]])) % question.options.len();
    let confidence = u16::try_from(10_000 / question.options.len()).unwrap_or(1);
    Ok((
        NormalizedAnswer::Selections(vec![question.options[index].id.clone()]),
        confidence,
        "donor-random-choice-stable",
    ))
}

fn donor_third_choice(
    question: &Question,
    strategy: &'static str,
) -> ProviderResult<(NormalizedAnswer, u16, &'static str)> {
    require_kind(question, QuestionKind::SingleChoice)?;
    let answer_id = question
        .options
        .iter()
        .find(|option| {
            option
                .metadata_sanitized
                .get("top_level_index")
                .and_then(Value::as_u64)
                == Some(2)
        })
        .and_then(|option| {
            option
                .metadata_sanitized
                .get("parent_answer_id")
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .or_else(|| question.options.get(2).map(|option| option.id.clone()))
        .ok_or_else(|| protocol_drift("Cidaren donor third-choice fallback is absent"))?;
    Ok((
        NormalizedAnswer::Selections(vec![answer_id]),
        2_500,
        strategy,
    ))
}

fn parse_means_entry(
    value: &Value,
    meanings: &mut Vec<String>,
    examples: &mut Vec<CidarenExampleEvidence>,
) -> ProviderResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol_drift("Cidaren means entry is not an object"))?;
    let parts = object
        .get("mean")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_drift("Cidaren means entry has no mean array"))?;
    let meaning = parts
        .iter()
        .map(|part| required_text(Some(part), MAX_TEXT_BYTES, "meaning part"))
        .collect::<ProviderResult<Vec<_>>>()?
        .join(" ");
    push_unique_meaning(meanings, meaning)?;
    if let Some(usages) = object.get("usages").and_then(Value::as_array) {
        for usage in usages {
            let usage = usage
                .as_object()
                .ok_or_else(|| protocol_drift("Cidaren usage is not an object"))?;
            parse_example_array(
                usage.get("phrases_infos"),
                examples,
                CidarenEvidenceUse::Phrase,
            )?;
            parse_example_array(usage.get("examples"), examples, CidarenEvidenceUse::Example)?;
        }
    }
    Ok(())
}

fn parse_options_entry(
    value: &Value,
    meanings: &mut Vec<String>,
    examples: &mut Vec<CidarenExampleEvidence>,
) -> ProviderResult<()> {
    let content = value
        .as_object()
        .and_then(|value| value.get("content"))
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_drift("Cidaren word option has no content object"))?;
    let meaning = required_text(content.get("mean"), MAX_TEXT_BYTES, "option meaning")?;
    push_unique_meaning(meanings, remove_chinese_parenthetical(&meaning))?;
    parse_example_array(
        content.get("usage_infos"),
        examples,
        CidarenEvidenceUse::Phrase,
    )?;
    parse_example_array(
        content.get("example"),
        examples,
        CidarenEvidenceUse::Example,
    )?;
    Ok(())
}

fn parse_example_array(
    value: Option<&Value>,
    examples: &mut Vec<CidarenExampleEvidence>,
    use_kind: CidarenEvidenceUse,
) -> ProviderResult<()> {
    let Some(entries) = value else {
        return Ok(());
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| protocol_drift("Cidaren word examples are not an array"))?;
    if examples.len().saturating_add(entries.len()) > MAX_EXAMPLES {
        return Err(invalid_response("Cidaren word examples exceed the limit"));
    }
    for entry in entries {
        let entry = entry
            .as_object()
            .ok_or_else(|| protocol_drift("Cidaren word example is not an object"))?;
        examples.push(CidarenExampleEvidence {
            english: required_text(entry.get("sen_content"), MAX_TEXT_BYTES, "example text")?,
            chinese: required_text(
                entry.get("sen_mean_cn"),
                MAX_TEXT_BYTES,
                "example translation",
            )?,
            use_kind,
        });
    }
    Ok(())
}

fn push_unique_meaning(meanings: &mut Vec<String>, meaning: String) -> ProviderResult<()> {
    if meanings.len() >= MAX_MEANINGS || meanings.contains(&meaning) {
        return Err(protocol_drift(
            "Cidaren word info contains duplicate or excessive meanings",
        ));
    }
    meanings.push(meaning);
    Ok(())
}

fn metadata_text<'a>(question: &'a Question, key: &str) -> ProviderResult<&'a str> {
    question
        .metadata_sanitized
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| valid_text(value, MAX_TEXT_BYTES))
        .ok_or_else(|| protocol_drift("Cidaren Question is missing answer-resolution metadata"))
}

fn require_kind(question: &Question, expected: QuestionKind) -> ProviderResult<()> {
    if question.kind == expected {
        Ok(())
    } else {
        Err(protocol_drift(
            "Cidaren Question kind disagrees with its topic mode",
        ))
    }
}

fn matching_evidence<'a>(
    info: &'a CidarenWordEvidence,
    target: &str,
    use_kind: CidarenEvidenceUse,
) -> Option<&'a CidarenExampleEvidence> {
    info.examples
        .iter()
        .find(|example| example.use_kind == use_kind && example.chinese == target)
}

fn matching_phrase_answer(info: &CidarenWordEvidence, target: &str) -> Option<String> {
    let phrase = &matching_evidence(info, target, CidarenEvidenceUse::Phrase)?.english;
    let answer = phrase
        .replace(['{', '}'], "")
        .replace(" ...", "")
        .replace(" …", "")
        .replace(' ', ",");
    valid_text(&answer, MAX_TEXT_BYTES).then_some(answer)
}

fn braced_word(value: &str) -> Option<&str> {
    let start = value.find('{')?.saturating_add(1);
    let end = value[start..].find('}')?.saturating_add(start);
    (start < end).then(|| &value[start..end])
}

fn stripped_inflection(value: &str) -> Option<&str> {
    value
        .strip_suffix("ing")
        .filter(|value| !value.is_empty())
        .or_else(|| value.strip_suffix("ed").filter(|value| !value.is_empty()))
}

fn semantic_equal(left: &str, right: &str) -> bool {
    let mut left = semantic_chars(left);
    let mut right = semantic_chars(right);
    left.sort_unstable();
    right.sort_unstable();
    left == right
}

fn semantic_chars(value: &str) -> Vec<char> {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn remove_chinese_parenthetical(value: &str) -> String {
    let Some(start) = value.find('（') else {
        return value.to_owned();
    };
    let Some(end_offset) = value[start..].find('）') else {
        return value.to_owned();
    };
    let end = start + end_offset + '）'.len_utf8();
    format!("{}{}", &value[..start], &value[end..])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn required_text(
    value: Option<&Value>,
    maximum: usize,
    label: &'static str,
) -> ProviderResult<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| valid_text(value, maximum))
        .map(ToOwned::to_owned)
        .ok_or_else(|| protocol_drift(format!("Cidaren word info contains an invalid {label}")))
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value
            .chars()
            .any(|character| character.is_control() && character != '\n')
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
    use asterism_domain::{QuestionId, QuestionOption, TaskId};
    use serde_json::Map;

    use super::*;

    const WORD_INFO: &str =
        include_str!("../../../fixtures/providers/cidaren/answers/study-word-info-means.json");

    #[test]
    fn word_info_parser_keeps_only_meaning_and_example_evidence() {
        let payload: Value = serde_json::from_str(WORD_INFO).unwrap();
        let info = parse_word_evidence(&payload).unwrap();
        assert_eq!(info.word, "alpha");
        assert_eq!(info.meanings, ["noun 合成释义"]);
        assert_eq!(info.examples.len(), 2);
        assert!(!format!("{info:?}").contains("alpha"));
    }

    #[test]
    fn empty_means_uses_options_or_reaches_the_donor_fallback() {
        let options = json!({
            "word": "alpha",
            "means": [],
            "options": [{
                "content": {
                    "mean": "noun option meaning",
                    "usage_infos": [],
                    "example": []
                }
            }]
        });
        let parsed = parse_word_evidence(&options).unwrap();
        assert_eq!(parsed.meanings, ["noun option meaning"]);

        let empty = parse_word_evidence(&json!({"word": "alpha", "means": []})).unwrap();
        let evidence =
            CidarenAnswerEvidence::try_new(vec!["alpha".to_owned()], vec![empty]).unwrap();
        let question = question(
            15,
            QuestionKind::SingleChoice,
            "{alpha}",
            None,
            vec![("n:0", "first"), ("n:1", "second")],
            &json!({}),
        );
        let candidate = resolve_answer_candidate(&question, &evidence).unwrap();
        assert_eq!(
            candidate.provenance_sanitized["strategy"],
            "donor-random-choice-stable"
        );
    }

    #[test]
    fn resolves_meaning_direction_matching_and_completion_modes() {
        let evidence = evidence();
        let word_to_meaning = question(
            15,
            QuestionKind::SingleChoice,
            "{alpha}",
            None,
            vec![("n:0", "other"), ("n:1", "preferred noun 合成释义")],
            &json!({}),
        );
        assert_eq!(
            resolve_answer_candidate(&word_to_meaning, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Selections(vec!["n:1".to_owned()])
        );

        let meaning_to_word = question(
            17,
            QuestionKind::SingleChoice,
            "noun 合成释义",
            None,
            vec![("n:0", "beta"), ("n:1", "alpha")],
            &json!({}),
        );
        assert_eq!(
            resolve_answer_candidate(&meaning_to_word, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Selections(vec!["n:1".to_owned()])
        );

        let matching = question(
            31,
            QuestionKind::Matching,
            "Match",
            None,
            vec![("n:0", "alpha"), ("n:1", "beta")],
            &json!({"relations": ["alpha", "beta"]}),
        );
        assert_eq!(
            resolve_answer_candidate(&matching, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Pairs(vec![
                AnswerPair {
                    left: "alpha".to_owned(),
                    right: "n:0".to_owned()
                },
                AnswerPair {
                    left: "beta".to_owned(),
                    right: "n:1".to_owned()
                },
            ])
        );

        let completion = question(
            51,
            QuestionKind::ShortAnswer,
            "Complete",
            None,
            Vec::new(),
            &json!({"word_tip": "alp", "word_lengths": [5]}),
        );
        assert_eq!(
            resolve_answer_candidate(&completion, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Texts(vec!["alpha".to_owned()])
        );
    }

    #[test]
    fn resolves_phrase_sentence_and_explicit_donor_fallback() {
        let evidence = evidence();
        let translation = question(
            32,
            QuestionKind::ShortAnswer,
            "Translate",
            Some("合成短语"),
            vec![("n:0", "alpha")],
            &json!({}),
        );
        assert_eq!(
            resolve_answer_candidate(&translation, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Texts(vec!["a,synthetic,alpha,phrase".to_owned()])
        );

        let sentence = question(
            41,
            QuestionKind::SingleChoice,
            "Complete sentence",
            Some("这是合成例句。"),
            vec![("s:1#0", "parent — alpha"), ("s:1#1", "parent — beta")],
            &json!({}),
        );
        assert_eq!(
            resolve_answer_candidate(&sentence, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Selections(vec!["s:1#0".to_owned()])
        );

        let fallback = question(
            13,
            QuestionKind::SingleChoice,
            "Audited heuristic",
            None,
            vec![("n:0", "a"), ("n:1", "b"), ("n:2", "c"), ("n:3", "d")],
            &json!({}),
        );
        let candidate = resolve_answer_candidate(&fallback, &evidence).unwrap();
        assert_eq!(
            candidate.answer,
            NormalizedAnswer::Selections(vec!["n:3".to_owned()])
        );
        assert_eq!(candidate.confidence.unwrap().basis_points(), 2_500);

        let completion_example = question(
            51,
            QuestionKind::ShortAnswer,
            "Complete",
            Some("这是合成例句。"),
            Vec::new(),
            &json!({"word_tip": "alp", "word_lengths": [8]}),
        );
        assert_eq!(
            resolve_answer_candidate(&completion_example, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Texts(vec!["alpha".to_owned()])
        );
    }

    #[test]
    fn donor_failure_fallbacks_are_explicit_bounded_and_stable() {
        let evidence = evidence();
        let word_to_meaning = question(
            15,
            QuestionKind::SingleChoice,
            "{alpha}",
            None,
            vec![
                ("n:0", "other-a"),
                ("n:1", "other-b"),
                ("n:2", "other-c"),
                ("n:3", "other-d"),
            ],
            &json!({}),
        );
        let first = resolve_answer_candidate(&word_to_meaning, &evidence).unwrap();
        let second = resolve_answer_candidate(&word_to_meaning, &evidence).unwrap();
        assert_eq!(first.answer, second.answer);
        assert_eq!(first.confidence.unwrap().basis_points(), 2_500);
        assert_eq!(
            first.provenance_sanitized["strategy"],
            "donor-random-choice-stable"
        );

        for mode in [17, 41] {
            let question = question(
                mode,
                QuestionKind::SingleChoice,
                "unmatched",
                Some("unmatched"),
                vec![("n:0", "a"), ("n:1", "b"), ("n:2", "c")],
                &json!({}),
            );
            let candidate = resolve_answer_candidate(&question, &evidence).unwrap();
            assert_eq!(
                candidate.answer,
                NormalizedAnswer::Selections(vec!["n:2".to_owned()])
            );
            assert_eq!(
                candidate.provenance_sanitized["strategy"],
                "donor-third-choice-fallback"
            );
        }

        let completion = question(
            51,
            QuestionKind::ShortAnswer,
            "Complete",
            Some("unmatched"),
            Vec::new(),
            &json!({"word_tip": "zzz", "word_lengths": [99]}),
        );
        let candidate = resolve_answer_candidate(&completion, &evidence).unwrap();
        assert_eq!(
            candidate.answer,
            NormalizedAnswer::Texts(vec!["beta".to_owned()])
        );
        assert_eq!(
            candidate.provenance_sanitized["strategy"],
            "donor-last-word-fallback"
        );
        assert_eq!(candidate.confidence.unwrap().basis_points(), 500);
    }

    #[test]
    fn sentence_modes_preserve_nested_wire_selection_and_parent_fallback() {
        let evidence = evidence();
        let options = vec![
            semantic_option("n:0", "other-a", 0, "other-a", "other-a", "n:0"),
            semantic_option("n:1", "other-b", 1, "other-b", "other-b", "n:1"),
            semantic_option("s:2#0", "alpha — alpha", 2, "alpha", "alpha", "s:2#"),
            semantic_option("s:2#1", "alpha — beta", 2, "alpha", "beta", "s:2#"),
        ];
        let mut matched = question(
            41,
            QuestionKind::SingleChoice,
            "Complete sentence",
            Some("这是合成例句。"),
            Vec::new(),
            &json!({}),
        );
        matched.options = options.clone();
        assert_eq!(
            resolve_answer_candidate(&matched, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Selections(vec!["s:2#0".to_owned()])
        );

        matched.metadata_sanitized["prompt_remark"] = json!("unmatched");
        let fallback = resolve_answer_candidate(&matched, &evidence).unwrap();
        assert_eq!(
            fallback.answer,
            NormalizedAnswer::Selections(vec!["s:2#".to_owned()])
        );
        assert_eq!(
            fallback.provenance_sanitized["strategy"],
            "donor-third-choice-fallback"
        );
    }

    #[test]
    fn missing_or_malformed_evidence_fails_closed() {
        let payload = json!({"word": "alpha", "unknown": []});
        assert_eq!(
            parse_word_evidence(&payload).unwrap_err().kind,
            ProviderErrorKind::ProtocolDrift
        );
        let evidence = CidarenAnswerEvidence::try_new(Vec::new(), Vec::new()).unwrap();
        let question = question(
            17,
            QuestionKind::SingleChoice,
            "missing",
            None,
            vec![("n:0", "missing")],
            &json!({}),
        );
        assert_eq!(
            resolve_answer_candidate(&question, &evidence)
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    fn evidence() -> CidarenAnswerEvidence {
        let payload: Value = serde_json::from_str(WORD_INFO).unwrap();
        CidarenAnswerEvidence::try_new(
            vec!["alpha".to_owned(), "beta".to_owned()],
            vec![parse_word_evidence(&payload).unwrap()],
        )
        .unwrap()
    }

    fn question(
        mode: i64,
        kind: QuestionKind,
        content: &str,
        remark: Option<&str>,
        options: Vec<(&str, &str)>,
        extra: &Value,
    ) -> Question {
        let mut metadata = Map::from_iter([
            ("schema".to_owned(), json!("cidaren.attempt-question.v1")),
            ("topic_mode".to_owned(), json!(mode)),
            ("prompt_content".to_owned(), json!(content)),
            ("prompt_remark".to_owned(), json!(remark)),
        ]);
        metadata.extend(extra.as_object().unwrap().clone());
        Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some(format!("question:synthetic-{mode}")),
            kind,
            stem: content.to_owned(),
            options: options
                .into_iter()
                .map(|(id, content)| QuestionOption {
                    id: id.to_owned(),
                    content: Some(content.to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                })
                .collect(),
            attachments: Vec::new(),
            metadata_sanitized: Value::Object(metadata),
            position: 1,
        }
    }

    fn semantic_option(
        id: &str,
        content: &str,
        top_level_index: usize,
        top_level_content: &str,
        wire_content: &str,
        parent_answer_id: &str,
    ) -> QuestionOption {
        QuestionOption {
            id: id.to_owned(),
            content: Some(content.to_owned()),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "nested": id != parent_answer_id,
                "top_level_index": top_level_index,
                "top_level_content": top_level_content,
                "wire_content": wire_content,
                "parent_answer_id": parent_answer_id,
            }),
        }
    }
}
