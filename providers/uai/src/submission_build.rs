use std::collections::{BTreeMap, BTreeSet};

use asterism_domain::{
    NormalizedAnswer, Question, QuestionKind, SelectedAnswer, SubmissionPayloadEncoding,
    SubmissionPayloadFieldPreview, SubmissionPayloadPreview,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, SubmissionBuildCapability,
};
use async_trait::async_trait;

use crate::metadata::development_metadata;

const MAX_REMOTE_TASK_ID_BYTES: usize = 512;
const MAX_REMOTE_COMPONENT_BYTES: usize = 128;
const MAX_QUESTIONS_PER_SUBMISSION: usize = 5_000;

/// Credential-free preview for the audited UAI simple `newExploration` JSON
/// shape. It neither reconstructs executable values nor performs remote I/O.
#[derive(Clone, Debug)]
pub struct UaiSubmissionBuild {
    metadata: ProviderMetadata,
}

impl UaiSubmissionBuild {
    /// Builds the independent UAI submission-preview capability.
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

impl ProviderIdentity for UaiSubmissionBuild {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionBuildCapability for UaiSubmissionBuild {
    async fn build_submission_preview(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
        selected_answers: &[SelectedAnswer],
    ) -> ProviderResult<SubmissionPayloadPreview> {
        validate_context(context, &self.metadata)?;
        GroupIdentity::parse(remote_task_id)?;
        if questions.is_empty()
            || questions.len() > MAX_QUESTIONS_PER_SUBMISSION
            || questions.len() != selected_answers.len()
        {
            return Err(invalid_input(
                "UAI submission preview requires one selection per Question",
            ));
        }

        let mut selected_by_question = BTreeMap::new();
        for selected in selected_answers {
            if selected.answer.validate().is_err()
                || matches!(selected.answer, NormalizedAnswer::Unknown)
                || selected_by_question
                    .insert(selected.question_id, selected)
                    .is_some()
            {
                return Err(invalid_input(
                    "UAI submission preview received an invalid or duplicate selection",
                ));
            }
        }

        let task_id = questions[0].task_id;
        let mut question_ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        let mut remote_ids = BTreeSet::new();
        let mut fields = Vec::with_capacity(questions.len() * 5);
        for question in questions {
            let selected = selected_by_question
                .get(&question.id)
                .copied()
                .ok_or_else(|| {
                    invalid_input("UAI submission preview is missing a Question selection")
                })?;
            let remote_id = question
                .remote_question_id
                .as_deref()
                .filter(|value| valid_question_identity(value))
                .ok_or_else(|| {
                    invalid_input("UAI submission preview requires a bounded remote Question ID")
                })?;
            let task_type = question
                .metadata_sanitized
                .get("task_type")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    invalid_input("UAI submission preview requires encrypted Question metadata")
                })?;
            if question.task_id != task_id
                || question.validate().is_err()
                || question
                    .metadata_sanitized
                    .get("schema")
                    .and_then(|value| value.as_str())
                    != Some("uai.encrypted-question.v1")
                || question
                    .metadata_sanitized
                    .get("remote_task_id")
                    .and_then(|value| value.as_str())
                    != Some(remote_task_id)
                || !question_ids.insert(question.id)
                || !positions.insert(question.position)
                || !remote_ids.insert(remote_id)
                || selected.question_id != question.id
            {
                return Err(invalid_input(
                    "UAI submission preview received inconsistent Question bindings",
                ));
            }
            validate_answer_shape(question, task_type, &selected.answer)?;
            fields.extend([
                preview_field(question, "quesDatas[].instanceId"),
                preview_field(question, "quesDatas[].answer"),
                preview_field(question, "quesDatas[].context"),
                preview_field(question, "quesDatas[].contextVersion"),
                preview_field(question, "quesDatas[].answerVersion"),
            ]);
        }
        if selected_by_question.len() != question_ids.len()
            || !positions
                .iter()
                .copied()
                .eq(1..=u32::try_from(questions.len()).map_err(|_| {
                    invalid_input("UAI submission preview Question count exceeds the limit")
                })?)
        {
            return Err(invalid_input(
                "UAI submission preview contains foreign or non-contiguous Questions",
            ));
        }

        Ok(SubmissionPayloadPreview {
            encoding: SubmissionPayloadEncoding::Json,
            format: "uai.new-exploration.json.v1".to_owned(),
            fields,
        })
    }
}

fn preview_field(question: &Question, field_name: &str) -> SubmissionPayloadFieldPreview {
    SubmissionPayloadFieldPreview {
        question_id: question.id,
        field_name: field_name.to_owned(),
    }
}

fn validate_answer_shape(
    question: &Question,
    task_type: &str,
    answer: &NormalizedAnswer,
) -> ProviderResult<()> {
    let expected_kind = match task_type {
        "single-choice" => QuestionKind::SingleChoice,
        "multichoice" => QuestionKind::MultipleChoice,
        "short_answer" => QuestionKind::ShortAnswer,
        _ => {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI submission preview does not support this Question type",
            ));
        }
    };
    if question.kind != expected_kind {
        return Err(invalid_input(
            "UAI submission preview Question kind differs from its task type",
        ));
    }
    match (question.kind, answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
            if values.len() == 1 && selections_exist(question, values) =>
        {
            Ok(())
        }
        (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values))
            if selections_exist(question, values) =>
        {
            Ok(())
        }
        (QuestionKind::ShortAnswer, NormalizedAnswer::Texts(_)) => Ok(()),
        _ => Err(invalid_input(
            "UAI submission preview answer type does not match its Question kind",
        )),
    }
}

fn selections_exist(question: &Question, values: &[String]) -> bool {
    let options = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    !values.is_empty() && values.iter().all(|value| options.contains(value.as_str()))
}

struct GroupIdentity;

impl GroupIdentity {
    fn parse(value: &str) -> ProviderResult<Self> {
        if value.is_empty()
            || value.len() > MAX_REMOTE_TASK_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(invalid_input("UAI Group Task identity is invalid"));
        }
        let mut components = value.split(':');
        if components.next() != Some("group")
            || !valid_component(components.next())
            || !valid_component(components.next())
            || !valid_component(components.next())
            || components.next().is_some()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "UAI submission preview supports stable Group Tasks only",
            ));
        }
        Ok(Self)
    }
}

fn valid_component(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !value.is_empty()
            && value.len() <= MAX_REMOTE_COMPONENT_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    })
}

fn valid_question_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI submission preview received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI submission preview requires an authenticated session",
        ));
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AnswerCandidateId, AnswerSource, ProviderAccountId, ProviderId, SecretId, TaskId,
    };

    use super::*;
    use crate::parse_question_content;

    const CONTENT: &str =
        include_str!("../../../fixtures/providers/uai/questions/content-multiple-choice.json");

    #[tokio::test]
    async fn preview_is_exact_typed_and_contains_no_executable_values() {
        let questions = questions();
        let selected = vec![selection(
            questions[0].id,
            NormalizedAnswer::Selections(vec!["A".to_owned(), "B".to_owned()]),
        )];
        let preview = UaiSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(
                &context(),
                "group:2001:unit-1:group-1",
                &questions,
                &selected,
            )
            .await
            .unwrap();
        assert_eq!(preview.encoding, SubmissionPayloadEncoding::Json);
        assert_eq!(preview.format, "uai.new-exploration.json.v1");
        assert_eq!(preview.fields.len(), 5);
        assert_eq!(preview.fields[0].field_name, "quesDatas[].instanceId");
        assert_eq!(preview.fields[1].field_name, "quesDatas[].answer");
        let encoded = serde_json::to_string(&preview).unwrap();
        assert!(!encoded.contains("\"A\""));
        assert!(!encoded.contains("question-1"));
        assert!(!encoded.contains("newExploration/submit"));
        assert!(!encoded.contains("courseId"));
        assert!(!encoded.contains("openId"));
    }

    #[tokio::test]
    async fn preview_rejects_foreign_non_group_and_mismatched_answer_shapes() {
        let questions = questions();
        let capability = UaiSubmissionBuild::try_new().unwrap();
        let wrong_answer = vec![selection(
            questions[0].id,
            NormalizedAnswer::Texts(vec!["wrong shape".to_owned()]),
        )];
        assert_eq!(
            capability
                .build_submission_preview(
                    &context(),
                    "group:2001:unit-1:group-1",
                    &questions,
                    &wrong_answer,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );
        let selected = vec![selection(
            questions[0].id,
            NormalizedAnswer::Selections(vec!["A".to_owned()]),
        )];
        assert_eq!(
            capability
                .build_submission_preview(&context(), "unit:2001:unit-1", &questions, &selected,)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::UnsupportedTask
        );
        assert_eq!(
            capability
                .build_submission_preview(
                    &context(),
                    "group:2001:unit-1:other-group",
                    &questions,
                    &selected,
                )
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );
        let foreign = vec![selection(
            asterism_domain::QuestionId::new(),
            NormalizedAnswer::Selections(vec!["A".to_owned()]),
        )];
        assert!(
            capability
                .build_submission_preview(
                    &context(),
                    "group:2001:unit-1:group-1",
                    &questions,
                    &foreign,
                )
                .await
                .is_err()
        );
    }

    fn questions() -> Vec<Question> {
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

    fn selection(
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

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            correlation_id: "uai-submission-preview".to_owned(),
            credential_refs: vec![SecretId::new()],
        }
    }
}
