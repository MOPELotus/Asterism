use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_domain::{AnswerCandidate, Question, RemoteState, SourceType};
use asterism_provider_api::{
    AnswerResolveCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteTaskDetail, TaskDetailCapability,
};
use async_trait::async_trait;

use crate::{
    ChaoxingChapterWorkVerificationDocument, metadata::development_metadata,
    parse_chapter_work_answer_candidates,
};

const MAX_REMOTE_COMPONENT_BYTES: usize = 128;

/// Credential-free stable binding for one fresh Chapter Work result read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingChapterWorkResultRequest {
    remote_task: String,
    course: String,
    class: String,
    knowledge: String,
    job: String,
}

impl ChaoxingChapterWorkResultRequest {
    /// Binds a result request to the exact freshly rediscovered completed Task.
    ///
    /// # Errors
    ///
    /// Returns a typed error for foreign, incomplete, pending or drifted Task
    /// identity and normalized detail facts.
    pub fn try_new(remote_task_id: &str, detail: &RemoteTaskDetail) -> ProviderResult<Self> {
        let components = remote_task_id.split(':').collect::<Vec<_>>();
        let ["resource", course_id, class_id, knowledge_id, job_id] = components.as_slice() else {
            return Err(unsupported(
                "Chaoxing native answer resolution supports only Chapter Work results",
            ));
        };
        if [course_id, class_id, knowledge_id, job_id]
            .into_iter()
            .any(|value| !valid_component(value))
        {
            return Err(protocol_drift(
                "Chaoxing Chapter Work answer-resolution identity is invalid",
            ));
        }
        if detail.task.remote_id != remote_task_id
            || detail.task.source_type != SourceType::Chapter
            || detail.task.remote_state != RemoteState::Completed
        {
            return Err(remote_changed(
                "Chaoxing Chapter Work is not the same completed fresh Task",
            ));
        }
        let normalized = detail
            .task
            .normalized
            .as_object()
            .ok_or_else(|| protocol_drift("Chaoxing Chapter Work detail is not an object"))?;
        for (field, expected) in [
            ("resource_kind", "chapter_work"),
            ("course_id", *course_id),
            ("class_id", *class_id),
            ("knowledge_id", *knowledge_id),
            ("task_id", *job_id),
        ] {
            if normalized.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                return Err(remote_changed(
                    "Chaoxing Chapter Work normalized answer-resolution binding changed",
                ));
            }
        }
        Ok(Self {
            remote_task: remote_task_id.to_owned(),
            course: (*course_id).to_owned(),
            class: (*class_id).to_owned(),
            knowledge: (*knowledge_id).to_owned(),
            job: (*job_id).to_owned(),
        })
    }

    pub const fn remote_task_id(&self) -> &str {
        self.remote_task.as_str()
    }

    pub const fn course_id(&self) -> &str {
        self.course.as_str()
    }

    pub const fn class_id(&self) -> &str {
        self.class.as_str()
    }

    pub const fn knowledge_id(&self) -> &str {
        self.knowledge.as_str()
    }

    pub const fn job_id(&self) -> &str {
        self.job.as_str()
    }
}

/// Read-only boundary that must reach a fresh, fully bound
/// `selectWorkQuestionYiPiYue` result iframe without invoking `redoTest`.
#[async_trait]
pub trait ChaoxingAnswerResolutionTransport: Send + Sync {
    async fn fetch_chapter_work_result(
        &self,
        context: &ProviderContext,
        request: &ChaoxingChapterWorkResultRequest,
    ) -> ProviderResult<ChaoxingChapterWorkVerificationDocument>;
}

/// Typed Provider-native resolver for standard answers already exposed by one
/// completed Chapter Work result. It is intentionally not registered by the
/// development factory until a bound `BrowserBridge` transport exists.
pub struct ChaoxingAnswerResolve {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
    transport: Arc<dyn ChaoxingAnswerResolutionTransport>,
}

impl ChaoxingAnswerResolve {
    /// Builds an unregistered resolver around fresh Task detail and result-read
    /// boundaries.
    ///
    /// # Errors
    ///
    /// Returns an internal error when compile-time metadata is invalid.
    pub fn try_new(
        details: Arc<dyn TaskDetailCapability>,
        transport: Arc<dyn ChaoxingAnswerResolutionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
            transport,
        })
    }

    async fn resolve_bound_answers(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Vec<AnswerCandidate>> {
        validate_context(context, &self.metadata)?;
        validate_questions(questions)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        let request = ChaoxingChapterWorkResultRequest::try_new(remote_task_id, &detail)?;
        let document = self
            .transport
            .fetch_chapter_work_result(context, &request)
            .await?;
        parse_chapter_work_answer_candidates(&document, questions)
    }
}

impl fmt::Debug for ChaoxingAnswerResolve {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingAnswerResolve")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingAnswerResolve {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AnswerResolveCapability for ChaoxingAnswerResolve {
    async fn resolve_answers(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        questions: &[Question],
    ) -> ProviderResult<Vec<AnswerCandidate>> {
        self.resolve_bound_answers(context, remote_task_id, questions)
            .await
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing answer resolution received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing answer resolution requires an authenticated session",
        ));
    }
    Ok(())
}

fn validate_questions(questions: &[Question]) -> ProviderResult<()> {
    let Some(task_id) = questions.first().map(|question| question.task_id) else {
        return Err(invalid_response(
            "Chaoxing answer resolution requires Questions",
        ));
    };
    let mut remote_ids = BTreeSet::new();
    if questions.iter().enumerate().any(|(index, question)| {
        question.task_id != task_id
            || question.position != u32::try_from(index + 1).unwrap_or(u32::MAX)
            || question.validate().is_err()
            || question
                .metadata_sanitized
                .get("page_kind")
                .and_then(serde_json::Value::as_str)
                != Some("chapter_work_mobile")
            || question
                .remote_question_id
                .as_deref()
                .is_none_or(|remote_id| {
                    !valid_component(remote_id) || !remote_ids.insert(remote_id)
                })
    }) {
        return Err(invalid_response(
            "Chaoxing answer-resolution Question snapshot is stale or foreign",
        ));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asterism_domain::{
        AssessmentClass, NormalizedAnswer, ProviderAccountId, ProviderId, SecretId, TaskCapability,
        TaskId,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;
    use crate::parse_chapter_work_question_page;

    const QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-mobile-mixed.html");
    const RESULT: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/chapter-result.html");
    const REMOTE_TASK_ID: &str = "resource:100:200:4001:job-work";

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        state: RemoteState,
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            let normalized = json!({
                "schema": "chaoxing.chapter-resource.v1",
                "resource_kind": "chapter_work",
                "course_id": "100",
                "class_id": "200",
                "knowledge_id": "4001",
                "task_id": "job-work",
            });
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:100:200".to_owned()),
                    title: "Synthetic Chapter Work".to_owned(),
                    source_type: SourceType::Chapter,
                    assessment_class: AssessmentClass::Unknown,
                    remote_state: self.state,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: vec![TaskCapability::SubmissionVerify],
                    fingerprint: "v1:synthetic-chapter-result".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: json!({"passed": self.state == RemoteState::Completed}),
                },
                normalized_detail: json!({
                    "schema": "chaoxing.task-detail.v1",
                    "task": normalized,
                }),
            })
        }
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        calls: Mutex<Vec<ChaoxingChapterWorkResultRequest>>,
    }

    #[async_trait]
    impl ChaoxingAnswerResolutionTransport for FixtureTransport {
        async fn fetch_chapter_work_result(
            &self,
            _context: &ProviderContext,
            request: &ChaoxingChapterWorkResultRequest,
        ) -> ProviderResult<ChaoxingChapterWorkVerificationDocument> {
            self.calls.lock().unwrap().push(request.clone());
            ChaoxingChapterWorkVerificationDocument::try_new(RESULT.to_owned())
        }
    }

    #[tokio::test]
    async fn completed_chapter_result_resolves_bound_native_candidates() {
        let detail = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            state: RemoteState::Completed,
        });
        let transport = Arc::new(FixtureTransport::default());
        let capability = ChaoxingAnswerResolve::try_new(detail, transport.clone()).unwrap();
        let questions = questions();
        let candidates = capability
            .resolve_answers(&context(), REMOTE_TASK_ID, &questions)
            .await
            .unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].answer,
            NormalizedAnswer::Selections(vec!["B".to_owned()])
        );
        assert_eq!(candidates[2].answer, NormalizedAnswer::Boolean(true));
        let calls = transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].remote_task_id(), REMOTE_TASK_ID);
        assert_eq!(calls[0].course_id(), "100");
        assert_eq!(calls[0].class_id(), "200");
        assert_eq!(calls[0].knowledge_id(), "4001");
        assert_eq!(calls[0].job_id(), "job-work");
    }

    #[tokio::test]
    async fn pending_or_foreign_questions_fail_before_result_transport() {
        let transport = Arc::new(FixtureTransport::default());
        let pending = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            state: RemoteState::Pending,
        });
        let capability = ChaoxingAnswerResolve::try_new(pending, transport.clone()).unwrap();
        assert_eq!(
            capability
                .resolve_answers(&context(), REMOTE_TASK_ID, &questions())
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert!(transport.calls.lock().unwrap().is_empty());

        let completed = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            state: RemoteState::Completed,
        });
        let capability = ChaoxingAnswerResolve::try_new(completed, transport.clone()).unwrap();
        let mut foreign = questions();
        foreign[0].metadata_sanitized["page_kind"] = json!("work_preview");
        assert_eq!(
            capability
                .resolve_answers(&context(), REMOTE_TASK_ID, &foreign)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    fn questions() -> Vec<Question> {
        let task_id = TaskId::new();
        let mut questions = parse_chapter_work_question_page(QUESTIONS)
            .unwrap()
            .iter()
            .map(|question| question.to_question(task_id).unwrap())
            .collect::<Vec<_>>();
        questions.remove(2);
        for (index, question) in questions.iter_mut().enumerate() {
            question.position = u32::try_from(index + 1).unwrap();
        }
        questions
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-answer-resolution-test".to_owned(),
        }
    }
}
