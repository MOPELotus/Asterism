use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{
    NormalizedAnswer, Question, QuestionKind, SelectedAnswer, SubmissionAnswerCoverage,
    SubmissionPayloadEncoding, SubmissionPayloadFieldPreview, SubmissionPayloadPreview,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, SubmissionBuildCapability,
};
use async_trait::async_trait;
use serde_json::Value;

use crate::metadata::development_metadata;

const MAX_REMOTE_TASK_ID_BYTES: usize = 768;

pub(crate) fn has_complete_answer_coverage(
    coverage: &SubmissionAnswerCoverage,
    answered_question_count: usize,
) -> bool {
    coverage.minimum_coverage_millis == 1_000
        && coverage.unanswered_question_ids.is_empty()
        && usize::try_from(coverage.total_question_count) == Ok(answered_question_count)
}

/// Credential-free Cidaren preview for one current attempt Question.
///
/// Cidaren reveals Questions sequentially and rotates `topic_code` after
/// Verify. A Draft therefore describes exactly one current Question; executable
/// topic codes, signatures, endpoints and headers are rebuilt only inside the
/// durable execution attempt.
#[derive(Clone, Debug)]
pub struct CidarenSubmissionBuild {
    metadata: ProviderMetadata,
}

impl CidarenSubmissionBuild {
    /// Builds the Development submission-preview capability.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new() -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
        })
    }
}

impl ProviderIdentity for CidarenSubmissionBuild {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionBuildCapability for CidarenSubmissionBuild {
    async fn build_submission_preview(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
        selected_answers: &[SelectedAnswer],
    ) -> ProviderResult<SubmissionPayloadPreview> {
        validate_context(context, &self.metadata)?;
        validate_remote_task_id(remote_task_id)?;
        let [question] = questions else {
            return Err(invalid_input(
                "Cidaren Draft requires exactly one current attempt Question",
            ));
        };
        let [selected] = selected_answers else {
            return Err(invalid_input(
                "Cidaren Draft requires exactly one selected answer",
            ));
        };
        validate_binding(question, selected, remote_task_id)?;

        let (format, mut fields) = match (&question.kind, &selected.answer) {
            (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
                if values.len() == 1 && valid_single_choice_id(question, &values[0]) =>
            {
                (
                    "cidaren.answer-lifecycle.json.v1",
                    verify_fields(question, None),
                )
            }
            (QuestionKind::ShortAnswer, NormalizedAnswer::Texts(values)) if values.len() == 1 => (
                "cidaren.answer-lifecycle.json.v1",
                verify_fields(question, None),
            ),
            (QuestionKind::Matching, NormalizedAnswer::Pairs(pairs)) => {
                validate_matching(question, pairs)?;
                (
                    "cidaren.answer-lifecycle.json.v1",
                    pairs
                        .iter()
                        .enumerate()
                        .flat_map(|(index, _)| verify_fields(question, Some(index)))
                        .collect(),
                )
            }
            (_, NormalizedAnswer::Skip) => (
                "cidaren.skip-lifecycle.json.v1",
                vec![
                    preview_field(question, "skip.topic_code"),
                    preview_field(question, "skip.time_spent"),
                ],
            ),
            _ => {
                return Err(invalid_input(
                    "Cidaren selected answer does not match the Question mode",
                ));
            }
        };
        if !matches!(&selected.answer, NormalizedAnswer::Skip) {
            fields.extend([
                preview_field(question, "advance.topic_code"),
                preview_field(question, "advance.time_spent"),
            ]);
        }
        Ok(SubmissionPayloadPreview {
            encoding: SubmissionPayloadEncoding::Json,
            format: format.to_owned(),
            fields,
        })
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren submission preview received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren submission preview requires an authenticated session",
        ));
    }
    Ok(())
}

fn validate_binding(
    question: &Question,
    selected: &SelectedAnswer,
    remote_task_id: &str,
) -> ProviderResult<()> {
    if question.validate().is_err()
        || selected.answer.validate().is_err()
        || matches!(selected.answer, NormalizedAnswer::Unknown)
        || selected.question_id != question.id
        || question.position == 0
        || question
            .metadata_sanitized
            .get("schema")
            .and_then(Value::as_str)
            != Some("cidaren.attempt-question.v1")
        || question
            .metadata_sanitized
            .get("remote_task_id")
            .and_then(Value::as_str)
            != Some(remote_task_id)
    {
        return Err(invalid_input(
            "Cidaren Draft contains inconsistent Question bindings",
        ));
    }
    Ok(())
}

fn validate_matching(
    question: &Question,
    pairs: &[asterism_domain::AnswerPair],
) -> ProviderResult<()> {
    let relations = question
        .metadata_sanitized
        .get("relations")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_input("Cidaren matching Question has no relations"))?;
    if pairs.len() != relations.len() || pairs.is_empty() {
        return Err(invalid_input(
            "Cidaren matching answer count differs from the Question",
        ));
    }
    let options = option_ids(question);
    let mut pair_map = BTreeMap::new();
    for pair in pairs {
        if !options.contains(pair.right.as_str())
            || pair_map
                .insert(pair.left.as_str(), pair.right.as_str())
                .is_some()
        {
            return Err(invalid_input(
                "Cidaren matching answer contains an invalid pair",
            ));
        }
    }
    let expected = relations
        .iter()
        .map(|relation| {
            relation
                .as_str()
                .ok_or_else(|| invalid_input("Cidaren matching relation is invalid"))
        })
        .collect::<ProviderResult<BTreeSet<_>>>()?;
    if pair_map.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(invalid_input(
            "Cidaren matching answer is not bound to every relation",
        ));
    }
    Ok(())
}

fn verify_fields(question: &Question, index: Option<usize>) -> Vec<SubmissionPayloadFieldPreview> {
    let prefix = index.map_or_else(
        || "verify_answer".to_owned(),
        |index| format!("verify_answer[{index}]"),
    );
    vec![
        preview_field(question, &format!("{prefix}.answer")),
        preview_field(question, &format!("{prefix}.topic_code")),
    ]
}

fn preview_field(question: &Question, name: &str) -> SubmissionPayloadFieldPreview {
    SubmissionPayloadFieldPreview {
        question_id: question.id,
        field_name: name.to_owned(),
    }
}

fn option_ids(question: &Question) -> BTreeSet<&str> {
    question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect()
}

fn valid_single_choice_id(question: &Question, answer_id: &str) -> bool {
    option_ids(question).contains(answer_id)
        || (matches!(
            question
                .metadata_sanitized
                .get("topic_mode")
                .and_then(Value::as_i64),
            Some(41..=44)
        ) && question.options.iter().any(|option| {
            option
                .metadata_sanitized
                .get("top_level_index")
                .and_then(Value::as_u64)
                == Some(2)
                && option
                    .metadata_sanitized
                    .get("parent_answer_id")
                    .and_then(Value::as_str)
                    == Some(answer_id)
        }))
}

fn validate_remote_task_id(value: &str) -> ProviderResult<()> {
    if !value.is_empty()
        && value.len() <= MAX_REMOTE_TASK_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && (value.starts_with("class-task:") || value.starts_with("study-task:"))
    {
        Ok(())
    } else {
        Err(invalid_input("Cidaren Draft Task identity is invalid"))
    }
}

fn invalid_input(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerPair, AnswerSource, ProviderAccountId, ProviderId, QuestionId,
        QuestionOption, SecretId, TaskId,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn cidaren_accepts_only_complete_answer_coverage() {
        let mut coverage = SubmissionAnswerCoverage {
            total_question_count: 1,
            minimum_coverage_millis: 1_000,
            unanswered_question_ids: Vec::new(),
        };
        assert!(has_complete_answer_coverage(&coverage, 1));

        coverage.minimum_coverage_millis = 999;
        assert!(!has_complete_answer_coverage(&coverage, 1));
        coverage.minimum_coverage_millis = 1_000;
        coverage.total_question_count = 2;
        assert!(!has_complete_answer_coverage(&coverage, 1));
        coverage.total_question_count = 1;
        coverage
            .unanswered_question_ids
            .push(asterism_domain::QuestionId::new());
        assert!(!has_complete_answer_coverage(&coverage, 1));
    }

    #[tokio::test]
    async fn single_and_text_previews_contain_no_executable_values() {
        let capability = CidarenSubmissionBuild::try_new().unwrap();
        for (question, answer) in [
            (
                question(
                    QuestionKind::SingleChoice,
                    vec![("n:0", "first"), ("n:1", "second")],
                    &json!({}),
                ),
                NormalizedAnswer::Selections(vec!["n:1".to_owned()]),
            ),
            (
                question(QuestionKind::ShortAnswer, Vec::new(), &json!({})),
                NormalizedAnswer::Texts(vec!["synthetic answer".to_owned()]),
            ),
        ] {
            let selected = selected_answer(&question, answer);
            let preview = capability
                .build_submission_preview(
                    &context(),
                    "class-task:2002",
                    std::slice::from_ref(&question),
                    std::slice::from_ref(&selected),
                )
                .await
                .unwrap();
            assert_eq!(preview.format, "cidaren.answer-lifecycle.json.v1");
            let serialized = serde_json::to_string(&preview).unwrap();
            assert!(!serialized.contains("synthetic answer"));
            assert!(!serialized.contains("topic-code-value"));
            assert_eq!(preview.fields.len(), 4);
        }
    }

    #[tokio::test]
    async fn matching_preview_preserves_ordered_verify_steps() {
        let capability = CidarenSubmissionBuild::try_new().unwrap();
        let question = question(
            QuestionKind::Matching,
            vec![("n:0", "alpha"), ("n:1", "beta")],
            &json!({"relations": ["alpha", "beta"]}),
        );
        let selected = selected_answer(
            &question,
            NormalizedAnswer::Pairs(vec![
                AnswerPair {
                    left: "alpha".to_owned(),
                    right: "n:0".to_owned(),
                },
                AnswerPair {
                    left: "beta".to_owned(),
                    right: "n:1".to_owned(),
                },
            ]),
        );
        let preview = capability
            .build_submission_preview(&context(), "class-task:2002", &[question], &[selected])
            .await
            .unwrap();
        assert_eq!(preview.fields.len(), 6);
        assert_eq!(preview.fields[0].field_name, "verify_answer[0].answer");
        assert_eq!(preview.fields[3].field_name, "verify_answer[1].topic_code");
    }

    #[tokio::test]
    async fn skip_preview_is_explicit_and_contains_no_answer_or_advance() {
        let capability = CidarenSubmissionBuild::try_new().unwrap();
        let question = question(
            QuestionKind::FillBlank,
            Vec::new(),
            &json!({"topic_mode": 73}),
        );
        let selected = selected_answer(&question, NormalizedAnswer::Skip);
        let preview = capability
            .build_submission_preview(&context(), "class-task:2002", &[question], &[selected])
            .await
            .unwrap();
        assert_eq!(preview.format, "cidaren.skip-lifecycle.json.v1");
        assert_eq!(preview.fields.len(), 2);
        assert_eq!(preview.fields[0].field_name, "skip.topic_code");
        assert_eq!(preview.fields[1].field_name, "skip.time_spent");
    }

    #[tokio::test]
    async fn nested_third_parent_fallback_remains_an_executable_donor_answer() {
        let capability = CidarenSubmissionBuild::try_new().unwrap();
        let mut question = question(
            QuestionKind::SingleChoice,
            vec![
                ("n:0", "first"),
                ("n:1", "second"),
                ("s:2#0", "third child"),
            ],
            &json!({"topic_mode": 41}),
        );
        question.options[2].metadata_sanitized = json!({
            "nested": true,
            "top_level_index": 2,
            "top_level_content": "third",
            "wire_content": "third child",
            "parent_answer_id": "s:2#",
        });
        let selected = selected_answer(
            &question,
            NormalizedAnswer::Selections(vec!["s:2#".to_owned()]),
        );
        assert!(
            capability
                .build_submission_preview(
                    &context(),
                    "class-task:2002",
                    std::slice::from_ref(&question),
                    std::slice::from_ref(&selected),
                )
                .await
                .is_ok()
        );
        question.metadata_sanitized["topic_mode"] = json!(17);
        assert!(
            capability
                .build_submission_preview(&context(), "class-task:2002", &[question], &[selected],)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn foreign_multiple_or_mismatched_inputs_fail_closed() {
        let capability = CidarenSubmissionBuild::try_new().unwrap();
        let question = question(
            QuestionKind::SingleChoice,
            vec![("n:0", "first")],
            &json!({}),
        );
        let mut selected = selected_answer(
            &question,
            NormalizedAnswer::Selections(vec!["n:0".to_owned()]),
        );
        selected.question_id = QuestionId::new();
        assert!(
            capability
                .build_submission_preview(
                    &context(),
                    "class-task:2002",
                    std::slice::from_ref(&question),
                    &[selected],
                )
                .await
                .is_err()
        );
        let selected = selected_answer(
            &question,
            NormalizedAnswer::Selections(vec!["n:0".to_owned()]),
        );
        assert!(
            capability
                .build_submission_preview(
                    &context(),
                    "class-task:2002",
                    &[question.clone(), question],
                    &[selected.clone(), selected],
                )
                .await
                .is_err()
        );
    }

    fn question(kind: QuestionKind, options: Vec<(&str, &str)>, extra: &Value) -> Question {
        let mut metadata = serde_json::Map::from_iter([
            ("schema".to_owned(), json!("cidaren.attempt-question.v1")),
            ("remote_task_id".to_owned(), json!("class-task:2002")),
            ("topic_mode".to_owned(), json!(17)),
        ]);
        metadata.extend(extra.as_object().unwrap().clone());
        Question {
            id: QuestionId::new(),
            task_id: TaskId::new(),
            remote_question_id: Some("question:synthetic".to_owned()),
            kind,
            stem: "Synthetic Question".to_owned(),
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

    fn selected_answer(question: &Question, answer: NormalizedAnswer) -> SelectedAnswer {
        SelectedAnswer {
            candidate_id: AnswerCandidateId::new(),
            question_id: question.id,
            answer,
            source: AnswerSource::ProviderNative,
            confidence: None,
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-submission-build-test".to_owned(),
        }
    }
}
