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
