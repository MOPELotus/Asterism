use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::Question;
use asterism_provider_api::{ProviderContext, ProviderError, ProviderErrorKind, ProviderResult};
use zeroize::{Zeroize, Zeroizing};

use crate::{CidarenAnswerEvidence, CidarenAnswerEvidenceBinding, CidarenAnswerEvidenceTransport};

const MAX_CANDIDATES: usize = 1_024;
const MAX_CANDIDATE_BYTES: usize = 32 * 1_024;

#[derive(Default)]
struct ZeroizingStringSet(BTreeSet<String>);

impl ZeroizingStringSet {
    fn insert(&mut self, value: &str) -> bool {
        if self.0.contains(value) {
            false
        } else {
            self.0.insert(value.to_owned());
            true
        }
    }
}

impl Drop for ZeroizingStringSet {
    fn drop(&mut self) {
        for mut value in std::mem::take(&mut self.0) {
            value.zeroize();
        }
    }
}

#[derive(Default)]
struct ZeroizingAliases(Vec<(String, String)>);

impl ZeroizingAliases {
    fn push(&mut self, alias: &str, target: &str) {
        self.0.push((alias.to_owned(), target.to_owned()));
    }

    fn finish(mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for ZeroizingAliases {
    fn drop(&mut self) {
        for (alias, target) in &mut self.0 {
            alias.zeroize();
            target.zeroize();
        }
    }
}

/// Loads the minimum fresh word evidence needed by one current Cidaren
/// Question, including the donor's authenticated `Course/SearchWord`
/// prototype fallback.
///
/// Every `StudyWordInfo` lookup is derived from the current task-bound word
/// inventory. Prototype results may select an inventory entry but cannot
/// introduce an arbitrary Course/list route.
///
/// # Errors
///
/// Returns a typed error for a foreign Question, unknown topic mode, missing
/// required prompt evidence, protocol drift or transport failure.
pub async fn load_answer_evidence(
    transport: &dyn CidarenAnswerEvidenceTransport,
    context: &ProviderContext,
    binding: &CidarenAnswerEvidenceBinding,
    question: &Question,
) -> ProviderResult<CidarenAnswerEvidence> {
    validate_question(question)?;
    let mode = question
        .metadata_sanitized
        .get("topic_mode")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| protocol_drift("Cidaren Question has no topic mode"))?;
    let (candidates, prompt_evidence_required) = candidate_words(question, mode)?;
    let inventory = transport.fetch_word_inventory(context, binding).await?;
    let mut candidate_keys = ZeroizingStringSet::default();
    let mut lookup_keys = BTreeSet::new();
    let mut lookups = Vec::new();
    let mut aliases = ZeroizingAliases::default();

    for candidate in candidates.iter() {
        let normalized_candidate = Zeroizing::new(candidate.to_lowercase());
        if !candidate_keys.insert(normalized_candidate.as_str()) {
            continue;
        }
        let mut used_prototype = false;
        let lookup = if let Some(lookup) = inventory.lookup(candidate) {
            Some(lookup)
        } else if valid_search_word(candidate) {
            let prototype = transport
                .resolve_word_prototype(context, candidate)
                .await?
                .map(Zeroizing::new);
            prototype.as_ref().and_then(|prototype| {
                let lookup = inventory.lookup(prototype.as_str());
                used_prototype = lookup.is_some();
                lookup
            })
        } else {
            None
        };

        let Some(lookup) = lookup else {
            if prompt_evidence_required {
                return Err(remote_changed(
                    "Cidaren prompt word is absent from the fresh Task inventory",
                ));
            }
            continue;
        };
        let target = Zeroizing::new(lookup.cloned_word().to_lowercase());
        if used_prototype && normalized_candidate.as_str() != target.as_str() {
            aliases.push(normalized_candidate.as_str(), target.as_str());
        }
        if lookup_keys.insert(lookup.dedup_key()) {
            lookups.push(lookup);
        }
    }

    if matches!(mode, 51..=54) {
        let (prefix, answer_length) = completion_query(question)?;
        for lookup in
            inventory.completion_evidence_lookups(prefix.as_str(), answer_length, MAX_CANDIDATES)?
        {
            if lookup_keys.insert(lookup.dedup_key()) {
                lookups.push(lookup);
            }
        }
    }

    let mut word_infos = Vec::with_capacity(lookups.len());
    for lookup in &lookups {
        word_infos.push(transport.fetch_word_evidence(context, lookup).await?);
    }
    inventory.into_answer_evidence_with_aliases(word_infos, aliases.finish())
}

fn candidate_words(
    question: &Question,
    mode: i64,
) -> ProviderResult<(Zeroizing<Vec<String>>, bool)> {
    let candidates = Zeroizing::new(match mode {
        11 | 15 | 16 | 21 | 22 => vec![prompt_word(question)?.to_owned()],
        13 | 31 | 51..=54 => Vec::new(),
        17 | 18 | 32 => option_words(question, false)?,
        41..=44 => option_words(question, true)?,
        _ => {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Cidaren evidence loader does not recognize this topic mode",
            ));
        }
    });
    if candidates.len() > MAX_CANDIDATES {
        return Err(invalid_response(
            "Cidaren Question exceeds the answer-evidence candidate limit",
        ));
    }
    Ok((candidates, matches!(mode, 11 | 15 | 16 | 21 | 22)))
}

fn option_words(question: &Question, sentence_mode: bool) -> ProviderResult<Vec<String>> {
    if sentence_mode {
        let mut top_level = BTreeMap::new();
        for (fallback_index, option) in question.options.iter().enumerate() {
            let index = option
                .metadata_sanitized
                .get("top_level_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(fallback_index);
            let content = option
                .metadata_sanitized
                .get("top_level_content")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    option
                        .content
                        .as_deref()
                        .and_then(|value| value.rsplit(" — ").next())
                })
                .filter(|value| valid_candidate(value))
                .ok_or_else(|| {
                    protocol_drift("Cidaren sentence option contains an invalid parent candidate")
                })?;
            match top_level.get(&index) {
                Some(existing) if *existing != content => {
                    return Err(protocol_drift(
                        "Cidaren sentence options disagree on their parent candidate",
                    ));
                }
                Some(_) => {}
                None => {
                    top_level.insert(index, content);
                }
            }
        }
        return Ok(top_level.into_values().map(ToOwned::to_owned).collect());
    }
    let candidates = question
        .options
        .iter()
        .filter_map(|option| option.content.as_deref())
        .map(|value| {
            valid_candidate(value)
                .then_some(value)
                .ok_or_else(|| protocol_drift("Cidaren Question contains an invalid candidate"))
        })
        .collect::<ProviderResult<Vec<_>>>()?;
    Ok(candidates.into_iter().map(ToOwned::to_owned).collect())
}

fn prompt_word(question: &Question) -> ProviderResult<&str> {
    let prompt = question
        .metadata_sanitized
        .get("prompt_content")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_candidate(value))
        .ok_or_else(|| protocol_drift("Cidaren Question has no bounded prompt content"))?;
    let word = braced_word(prompt).unwrap_or(prompt);
    valid_candidate(word)
        .then_some(word)
        .ok_or_else(|| protocol_drift("Cidaren Question prompt word is invalid"))
}

fn braced_word(value: &str) -> Option<&str> {
    let start = value.find('{')? + 1;
    let end = value[start..].find('}')? + start;
    (start < end).then(|| &value[start..end])
}

fn completion_query(question: &Question) -> ProviderResult<(Zeroizing<String>, usize)> {
    let prefix = Zeroizing::new(
        question
            .metadata_sanitized
            .get("word_tip")
            .and_then(serde_json::Value::as_str)
            .filter(|value| valid_candidate(value))
            .map(str::to_lowercase)
            .ok_or_else(|| {
                protocol_drift("Cidaren completion Question has no bounded word prefix")
            })?,
    );
    let answer_length = question
        .metadata_sanitized
        .get("word_lengths")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0 && *value <= 1_024)
        .ok_or_else(|| protocol_drift("Cidaren completion Question has no bounded word length"))?;
    Ok((prefix, answer_length))
}

fn validate_question(question: &Question) -> ProviderResult<()> {
    question
        .validate()
        .map_err(|_| invalid_response("Cidaren evidence loader received an invalid Question"))?;
    if question
        .metadata_sanitized
        .get("schema")
        .and_then(serde_json::Value::as_str)
        != Some("cidaren.attempt-question.v1")
    {
        return Err(protocol_drift(
            "Cidaren evidence loader received a foreign Question",
        ));
    }
    Ok(())
}

fn valid_candidate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CANDIDATE_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_search_word(value: &str) -> bool {
    value.len() <= 1_024
        && value
            .chars()
            .all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | ' '))
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{
        AssessmentClass, NormalizedAnswer, ProviderAccountId, ProviderId, QuestionId, QuestionKind,
        QuestionOption, RemoteState, SecretId, SourceType, TaskId,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use async_trait::async_trait;
    use serde_json::{Map, json};

    use super::*;
    use crate::{
        CidarenStudyTaskDocument, CidarenWordEvidence, parse_attempt_question,
        parse_study_task_info_response, parse_word_info_response, resolve_answer_candidate,
    };

    #[derive(Debug)]
    struct FixtureTransport {
        prototype_calls: AtomicUsize,
        evidence_calls: AtomicUsize,
    }

    #[async_trait]
    impl CidarenAnswerEvidenceTransport for FixtureTransport {
        async fn bind_answer_evidence(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _detail: &RemoteTaskDetail,
        ) -> ProviderResult<CidarenAnswerEvidenceBinding> {
            Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "unused fixture method",
            ))
        }

        async fn fetch_word_inventory(
            &self,
            _context: &ProviderContext,
            binding: &CidarenAnswerEvidenceBinding,
        ) -> ProviderResult<crate::CidarenWordInventory> {
            parse_study_task_info_response(
                include_str!("../../../fixtures/providers/cidaren/answers/study-task-info.json")
                    .as_bytes(),
                binding,
                None,
            )
        }

        async fn fetch_word_evidence(
            &self,
            _context: &ProviderContext,
            lookup: &crate::CidarenWordLookup,
        ) -> ProviderResult<CidarenWordEvidence> {
            self.evidence_calls.fetch_add(1, Ordering::Relaxed);
            parse_word_info_response(
                include_str!(
                    "../../../fixtures/providers/cidaren/answers/study-word-info-envelope.json"
                )
                .as_bytes(),
                lookup,
                None,
            )
        }

        async fn resolve_word_prototype(
            &self,
            _context: &ProviderContext,
            word: &str,
        ) -> ProviderResult<Option<String>> {
            self.prototype_calls.fetch_add(1, Ordering::Relaxed);
            Ok((word == "packed").then(|| "alpha".to_owned()))
        }
    }

    #[tokio::test]
    async fn loader_uses_prototype_alias_and_resolver_consumes_it() {
        let transport = Arc::new(FixtureTransport {
            prototype_calls: AtomicUsize::new(0),
            evidence_calls: AtomicUsize::new(0),
        });
        let binding = answer_binding();
        let question = question(11, QuestionKind::SingleChoice, "{packed}");
        let evidence =
            load_answer_evidence(transport.as_ref(), &provider_context(), &binding, &question)
                .await
                .unwrap();
        assert_eq!(transport.prototype_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.evidence_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            resolve_answer_candidate(&question, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Selections(vec!["n:0".to_owned()])
        );
    }

    #[tokio::test]
    async fn loader_fetches_only_completion_words_that_need_example_fallback() {
        let transport = Arc::new(FixtureTransport {
            prototype_calls: AtomicUsize::new(0),
            evidence_calls: AtomicUsize::new(0),
        });
        let binding = answer_binding();
        let question = Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("question:completion".to_owned()),
            kind: QuestionKind::ShortAnswer,
            stem: "Complete".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "cidaren.attempt-question.v1",
                "topic_mode": 51,
                "prompt_content": "Complete",
                "prompt_remark": "这是合成例句。",
                "word_tip": "alp",
                "word_lengths": [8],
            }),
            position: 1,
        };
        let evidence =
            load_answer_evidence(transport.as_ref(), &provider_context(), &binding, &question)
                .await
                .unwrap();
        assert_eq!(transport.prototype_calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.evidence_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            resolve_answer_candidate(&question, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Texts(vec!["alpha".to_owned()])
        );
    }

    #[tokio::test]
    async fn nested_sentence_loads_top_level_parent_evidence_once() {
        let transport = Arc::new(FixtureTransport {
            prototype_calls: AtomicUsize::new(0),
            evidence_calls: AtomicUsize::new(0),
        });
        let parsed = parse_attempt_question(
            &json!({
                "topic_code": "nested-topic",
                "topic_mode": 41,
                "stem": {"content": "Complete {}", "remark": "这是合成例句。"},
                "options": [{
                    "answer_tag": "1#",
                    "content": "alpha",
                    "sub_options": [
                        {"answer_tag": 0, "content": "alpha"},
                        {"answer_tag": 1, "content": "beta"}
                    ]
                }]
            }),
            "class-task:2002",
            1,
        )
        .unwrap();
        let question = parsed.to_question(TaskId::new()).unwrap();
        let evidence = load_answer_evidence(
            transport.as_ref(),
            &provider_context(),
            &answer_binding(),
            &question,
        )
        .await
        .unwrap();
        assert_eq!(transport.prototype_calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.evidence_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            resolve_answer_candidate(&question, &evidence)
                .unwrap()
                .answer,
            NormalizedAnswer::Selections(vec!["s:1#0".to_owned()])
        );
    }

    fn answer_binding() -> CidarenAnswerEvidenceBinding {
        let normalized = json!({
            "schema": "cidaren.class-task.v1",
            "release_id": "2002",
            "task_id": -1,
            "course_id": "course-a",
            "task_type": "test",
            "progress": 35,
        });
        let detail = RemoteTaskDetail {
            task: RemoteTask {
                remote_id: "class-task:2002".to_owned(),
                course_remote_id: Some("course:course-a".to_owned()),
                title: "Synthetic List 02".to_owned(),
                source_type: SourceType::Exam,
                assessment_class: AssessmentClass::Routine,
                remote_state: RemoteState::InProgress,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: Vec::new(),
                fingerprint: "synthetic".to_owned(),
                normalized: normalized.clone(),
                raw_sanitized: Map::new().into(),
            },
            normalized_detail: json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": "2002",
                "task": normalized,
            }),
        };
        let units = CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
        .unwrap();
        CidarenAnswerEvidenceBinding::from_fresh_detail("class-task:2002", &detail, &units).unwrap()
    }

    fn question(mode: i64, kind: QuestionKind, prompt: &str) -> Question {
        Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("question:synthetic".to_owned()),
            kind,
            stem: prompt.to_owned(),
            options: vec![
                QuestionOption {
                    id: "n:0".to_owned(),
                    content: Some("noun 合成释义".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                },
                QuestionOption {
                    id: "n:1".to_owned(),
                    content: Some("other".to_owned()),
                    attachments: Vec::new(),
                    metadata_sanitized: json!({}),
                },
            ],
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "cidaren.attempt-question.v1",
                "topic_mode": mode,
                "prompt_content": prompt,
                "prompt_remark": null,
            }),
            position: 1,
        }
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-answer-loader-test".to_owned(),
        }
    }
}
