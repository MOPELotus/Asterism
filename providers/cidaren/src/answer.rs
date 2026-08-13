use std::{fmt, sync::Arc};

use asterism_domain::{AnswerCandidate, Question};
use asterism_provider_api::{
    AnswerResolveCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;

use crate::{
    CidarenAnswerEvidenceTransport, load_answer_evidence, metadata::development_metadata,
    resolve_answer_candidate,
};

/// Provider-native answer resolver for one fresh Cidaren Task.
///
/// The resolver only reads task-bound inventory/word evidence and constructs
/// bounded answer candidates. It never starts an attempt or mutates remote
/// assessment state.
pub struct CidarenAnswerResolve {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn CidarenAnswerEvidenceTransport>,
}

impl CidarenAnswerResolve {
    /// Builds the resolver around fresh Task detail and answer-evidence routes.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn CidarenAnswerEvidenceTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
        })
    }
}

impl fmt::Debug for CidarenAnswerResolve {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenAnswerResolve")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenAnswerResolve {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AnswerResolveCapability for CidarenAnswerResolve {
    async fn resolve_answers(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Vec<AnswerCandidate>> {
        validate_context(context, &self.metadata)?;
        if questions.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Cidaren answer resolution requires at least one Question",
            ));
        }
        let detail = self.details.task_detail(context, remote_task_id).await?;
        validate_question_task_bindings(questions, remote_task_id)?;
        let binding = self
            .transport
            .bind_answer_evidence(context, remote_task_id, &detail)
            .await?;
        let mut candidates = Vec::with_capacity(questions.len());
        for question in questions {
            let evidence =
                load_answer_evidence(self.transport.as_ref(), context, &binding, question).await?;
            candidates.push(resolve_answer_candidate(question, &evidence)?);
        }
        Ok(candidates)
    }
}

fn validate_question_task_bindings(
    questions: &[Question],
    remote_task_id: &str,
) -> ProviderResult<()> {
    let Some(first_task_id) = questions.first().map(|question| question.task_id) else {
        return Ok(());
    };
    for question in questions {
        if question.task_id != first_task_id
            || question
                .metadata_sanitized
                .get("remote_task_id")
                .and_then(serde_json::Value::as_str)
                != Some(remote_task_id)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Cidaren answer resolution received a Question from another Task",
            ));
        }
    }
    Ok(())
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren answer resolution received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren answer resolution requires an authenticated session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{QuestionId, QuestionKind, QuestionOption, TaskId};
    use serde_json::json;

    use super::*;

    fn question(task_id: TaskId, remote_task_id: &str) -> Question {
        Question {
            id: QuestionId::new(),
            task_id,
            remote_question_id: Some("question:synthetic".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Synthetic prompt".to_owned(),
            options: vec![QuestionOption {
                id: "option:0".to_owned(),
                content: Some("Synthetic option".to_owned()),
                attachments: Vec::new(),
                metadata_sanitized: json!({}),
            }],
            attachments: Vec::new(),
            metadata_sanitized: json!({
                "schema": "cidaren.attempt-question.v1",
                "remote_task_id": remote_task_id,
            }),
            position: 1,
        }
    }

    #[test]
    fn answer_resolution_rejects_mixed_task_questions() {
        let first_task = TaskId::new();
        let second_task = TaskId::new();
        let questions = vec![
            question(first_task, "class-task:2002"),
            question(second_task, "class-task:2002"),
        ];
        assert_eq!(
            validate_question_task_bindings(&questions, "class-task:2002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        let foreign = vec![question(first_task, "class-task:2003")];
        assert_eq!(
            validate_question_task_bindings(&foreign, "class-task:2002")
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }
}
