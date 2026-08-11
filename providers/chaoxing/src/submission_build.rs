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

const MAX_REMOTE_TASK_ID_BYTES: usize = 640;
const MAX_REMOTE_QUESTION_ID_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct ChaoxingSubmissionBuild {
    metadata: ProviderMetadata,
}

impl ChaoxingSubmissionBuild {
    /// Builds the credential-free independent Work preview capability.
    ///
    /// # Errors
    ///
    /// Returns a sanitized internal error if compile-time Provider metadata is
    /// invalid.
    pub fn try_new() -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
        })
    }
}

impl ProviderIdentity for ChaoxingSubmissionBuild {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionBuildCapability for ChaoxingSubmissionBuild {
    async fn build_submission_preview(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
        selected_answers: &[SelectedAnswer],
    ) -> ProviderResult<SubmissionPayloadPreview> {
        validate_context(context, &self.metadata)?;
        validate_work_identity(remote_task_id)?;
        if questions.is_empty() || questions.len() != selected_answers.len() {
            return Err(invalid_input(
                "Chaoxing Work preview requires one selection per Question",
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
                    "Chaoxing Work preview received an invalid or duplicate selection",
                ));
            }
        }

        let expected_task_id = questions[0].task_id;
        let mut question_ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        let mut fields = Vec::with_capacity(questions.len() * 2);
        for question in questions {
            let remote_question_id = question
                .remote_question_id
                .as_deref()
                .filter(|value| valid_component(value, MAX_REMOTE_QUESTION_ID_BYTES))
                .ok_or_else(|| {
                    invalid_input("Chaoxing Work Question identity cannot form a safe field name")
                })?;
            let selected = selected_by_question
                .get(&question.id)
                .copied()
                .ok_or_else(|| {
                    invalid_input("Chaoxing Work preview is missing a Question selection")
                })?;
            if question.task_id != expected_task_id
                || question.validate().is_err()
                || question
                    .metadata_sanitized
                    .get("page_kind")
                    .and_then(|value| value.as_str())
                    != Some("work_preview")
                || !question_ids.insert(question.id)
                || !positions.insert(question.position)
                || selected.question_id != question.id
            {
                return Err(invalid_input(
                    "Chaoxing Work preview received inconsistent Question bindings",
                ));
            }
            validate_answer_shape(question, &selected.answer)?;
            fields.extend([
                SubmissionPayloadFieldPreview {
                    question_id: question.id,
                    field_name: format!("answer{remote_question_id}"),
                },
                SubmissionPayloadFieldPreview {
                    question_id: question.id,
                    field_name: format!("answertype{remote_question_id}"),
                },
            ]);
        }
        if selected_by_question.len() != question_ids.len() {
            return Err(invalid_input(
                "Chaoxing Work preview contains a foreign Question selection",
            ));
        }

        Ok(SubmissionPayloadPreview {
            encoding: SubmissionPayloadEncoding::Form,
            format: "chaoxing.work.form.v1".to_owned(),
            fields,
        })
    }
}

fn validate_answer_shape(question: &Question, answer: &NormalizedAnswer) -> ProviderResult<()> {
    match (question.kind, answer) {
        (QuestionKind::SingleChoice, NormalizedAnswer::Selections(values))
            if values.len() == 1 && executable_selections_exist(question, values) =>
        {
            Ok(())
        }
        (QuestionKind::MultipleChoice, NormalizedAnswer::Selections(values))
            if executable_selections_exist(question, values) =>
        {
            Ok(())
        }
        (QuestionKind::TrueFalse, NormalizedAnswer::Boolean(_)) => Ok(()),
        (
            QuestionKind::FillBlank
            | QuestionKind::ShortAnswer
            | QuestionKind::Matching
            | QuestionKind::Ordering
            | QuestionKind::Composite
            | QuestionKind::Unknown,
            _,
        ) => Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Chaoxing Work preview does not yet encode this Question kind",
        )),
        _ => Err(invalid_input(
            "Chaoxing Work answer type does not match its Question kind",
        )),
    }
}

fn selections_exist(question: &Question, values: &[String]) -> bool {
    let option_ids = question
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    !values.is_empty()
        && values
            .iter()
            .all(|value| option_ids.contains(value.as_str()))
}

fn executable_selections_exist(question: &Question, values: &[String]) -> bool {
    let distinct = values.iter().collect::<BTreeSet<_>>();
    distinct.len() == values.len()
        && values
            .iter()
            .all(|value| value.len() == 1 && value.bytes().all(|byte| byte.is_ascii_uppercase()))
        && selections_exist(question, values)
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing submission preview received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing submission preview requires an authenticated session",
        ));
    }
    Ok(())
}

fn validate_work_identity(remote_task_id: &str) -> ProviderResult<()> {
    if remote_task_id.is_empty()
        || remote_task_id.len() > MAX_REMOTE_TASK_ID_BYTES
        || remote_task_id.chars().any(char::is_control)
    {
        return Err(invalid_input("Chaoxing remote Work identity is invalid"));
    }
    let parts = remote_task_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4
        || parts[0] != "work"
        || parts[1..].iter().any(|value| !valid_component(value, 128))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::UnsupportedTask,
            "Chaoxing submission preview supports independent Work tasks only",
        ));
    }
    Ok(())
}

fn valid_component(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
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
    use crate::question_parser::parse_work_preview_question_page;

    const WORK_PREVIEW: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-preview-mixed.html");

    #[tokio::test]
    async fn independent_work_preview_is_typed_bounded_and_value_free() {
        let task_id = TaskId::new();
        let mut questions = parse_work_preview_question_page(WORK_PREVIEW)
            .unwrap()
            .iter()
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        questions[1].kind = QuestionKind::MultipleChoice;
        questions[1].options = vec![
            asterism_domain::QuestionOption {
                id: "A".to_owned(),
                content: Some("First".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            },
            asterism_domain::QuestionOption {
                id: "C".to_owned(),
                content: Some("Third".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
            },
        ];
        questions[1].metadata_sanitized["provider_type_code"] = serde_json::json!(1);
        let selected = vec![
            selection(
                questions[0].id,
                NormalizedAnswer::Selections(vec!["B".to_owned()]),
            ),
            selection(
                questions[1].id,
                NormalizedAnswer::Selections(vec!["A".to_owned(), "C".to_owned()]),
            ),
        ];
        let preview = ChaoxingSubmissionBuild::try_new()
            .unwrap()
            .build_submission_preview(&context(), "work:100:200:work-1", &questions, &selected)
            .await
            .unwrap();

        assert_eq!(preview.encoding, SubmissionPayloadEncoding::Form);
        assert_eq!(preview.format, "chaoxing.work.form.v1");
        assert_eq!(preview.fields.len(), 4);
        assert_eq!(preview.fields[0].field_name, "answerwork-preview-q-1");
        assert_eq!(preview.fields[1].field_name, "answertypework-preview-q-1");
        let encoded = serde_json::to_string(&preview).unwrap();
        assert!(!encoded.contains("First"));
        assert!(!encoded.contains("addStudentWorkNew"));
        assert!(!encoded.contains("pyFlag"));
    }

    #[tokio::test]
    async fn preview_rejects_exam_foreign_and_unsupported_answer_shapes() {
        let task_id = TaskId::new();
        let mut questions = parse_work_preview_question_page(WORK_PREVIEW)
            .unwrap()
            .iter()
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        let selected = vec![
            selection(questions[0].id, NormalizedAnswer::Boolean(true)),
            selection(
                questions[1].id,
                NormalizedAnswer::Texts(vec!["answer".to_owned()]),
            ),
        ];
        let capability = ChaoxingSubmissionBuild::try_new().unwrap();
        let mismatch = capability
            .build_submission_preview(&context(), "work:100:200:work-1", &questions, &selected)
            .await
            .unwrap_err();
        assert_eq!(mismatch.kind, ProviderErrorKind::InvalidResponse);

        let exam = capability
            .build_submission_preview(&context(), "exam:100:200:exam-1", &questions, &selected)
            .await
            .unwrap_err();
        assert_eq!(exam.kind, ProviderErrorKind::UnsupportedTask);

        questions[0].kind = QuestionKind::Ordering;
        let selected = vec![
            selection(
                questions[0].id,
                NormalizedAnswer::Ordering(vec!["A".to_owned(), "B".to_owned()]),
            ),
            selection(
                questions[1].id,
                NormalizedAnswer::Texts(vec!["answer".to_owned()]),
            ),
        ];
        let unsupported = capability
            .build_submission_preview(&context(), "work:100:200:work-1", &questions, &selected)
            .await
            .unwrap_err();
        assert_eq!(unsupported.kind, ProviderErrorKind::UnsupportedTask);
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
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "submission-preview-test".to_owned(),
        }
    }
}
