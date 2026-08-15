use std::{fmt, sync::Arc};

use asterism_domain::Timestamp;
use asterism_provider_api::{
    AnswerHistoryCursor, AnswerHistoryHarvestCapability, AnswerHistoryPage,
    AnswerHistoryQuestionEvidence, AnswerHistoryRetakeFacts, AnswerHistoryTaskRef,
    AnswerHistoryTaskRequest, ProviderAnswerHistoryTaskEvidence, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderResult, ProviderRouteContext,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{
    ChaoxingChapterWorkAnswerJudgement, ChaoxingChapterWorkRetakeEntry,
    ChaoxingChapterWorkVerificationDocument, metadata::development_metadata,
    parse_chapter_work_question_page, parse_chapter_work_result_evidence,
};

const CURSOR_TYPE: &str = "chaoxing.chapter-work-history.v1";
const ATTEMPT_DIGEST_SCHEMA: &str = "chaoxing.chapter-work-attempt.v1";
const RESULT_EVIDENCE_SCHEMA: &str = "chaoxing.chapter-work-history-result.v1";
const MAX_PAGE_SIZE: usize = 100;
const MAX_COMPONENT_BYTES: usize = 128;

/// One completed Chapter Work result discovered by a read-only Provider-local
/// transport. `work_answer_id` is attempt-local route material and never enters
/// the durable sanitized metadata.
#[derive(Clone)]
pub struct ChaoxingChapterWorkHistoryRecord {
    remote_task_id: String,
    course_remote_id: String,
    work_answer_id: String,
    completed_at: Option<Timestamp>,
}

impl ChaoxingChapterWorkHistoryRecord {
    /// # Errors
    ///
    /// Rejects foreign task/course identities or malformed attempt material.
    pub fn try_new(
        remote_task_id: &str,
        course_remote_id: &str,
        work_answer_id: &str,
        completed_at: Option<Timestamp>,
    ) -> ProviderResult<Self> {
        let binding =
            ChapterHistoryBinding::from_identity(remote_task_id, course_remote_id, work_answer_id)?;
        Ok(Self {
            remote_task_id: binding.remote_task.clone(),
            course_remote_id: binding.course_remote.clone(),
            work_answer_id: binding.work_answer.clone(),
            completed_at,
        })
    }

    fn into_task_reference(mut self) -> ProviderResult<AnswerHistoryTaskRef> {
        let binding = ChapterHistoryBinding::from_identity(
            &self.remote_task_id,
            &self.course_remote_id,
            &self.work_answer_id,
        )?;
        let reference = binding.task_reference(self.completed_at)?;
        self.work_answer_id.zeroize();
        Ok(reference)
    }
}

impl fmt::Debug for ChaoxingChapterWorkHistoryRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterWorkHistoryRecord")
            .field("remote_task_id", &self.remote_task_id)
            .field("course_remote_id", &self.course_remote_id)
            .field("work_answer_id", &"[REDACTED]")
            .field("completed_at", &self.completed_at)
            .finish()
    }
}

impl Drop for ChaoxingChapterWorkHistoryRecord {
    fn drop(&mut self) {
        self.work_answer_id.zeroize();
    }
}

/// One bounded Provider-local page. The page number is an ordinal owned by the
/// transport, not a remote URL or credential.
#[derive(Debug)]
pub struct ChaoxingChapterWorkHistoryPage {
    records: Vec<ChaoxingChapterWorkHistoryRecord>,
    next_page: Option<u32>,
    complete: bool,
}

impl ChaoxingChapterWorkHistoryPage {
    /// # Errors
    ///
    /// Rejects oversized, empty-incomplete or ambiguous pagination.
    pub fn try_new(
        records: Vec<ChaoxingChapterWorkHistoryRecord>,
        next_page: Option<u32>,
        complete: bool,
    ) -> ProviderResult<Self> {
        if records.len() > MAX_PAGE_SIZE
            || (records.is_empty() && !complete)
            || complete == next_page.is_some()
        {
            return Err(invalid_response(
                "Chaoxing Chapter Work history page is invalid or ambiguous",
            ));
        }
        Ok(Self {
            records,
            next_page,
            complete,
        })
    }
}

/// Strict read request rebuilt from Core's returned Task reference.
pub struct ChaoxingChapterWorkHistoryResultRequest {
    binding: ChapterHistoryBinding,
}

impl ChaoxingChapterWorkHistoryResultRequest {
    fn try_from_core(request: &AnswerHistoryTaskRequest) -> ProviderResult<Self> {
        request
            .reference
            .validate()
            .map_err(|_| invalid_response("Chaoxing history Task reference is invalid"))?;
        let route = &request.reference.route_context;
        let work_answer_id = route
            .get("chaoxing.work_answer_id")
            .ok_or_else(|| invalid_response("Chaoxing history attempt route is missing"))?;
        let binding = ChapterHistoryBinding::from_identity(
            &request.reference.remote_task_id,
            request
                .reference
                .course_remote_id
                .as_deref()
                .ok_or_else(|| invalid_response("Chaoxing history Course binding is missing"))?,
            work_answer_id,
        )?;
        for (key, expected) in [
            ("chaoxing.course_id", binding.course.as_str()),
            ("chaoxing.class_id", binding.class.as_str()),
            ("chaoxing.knowledge_id", binding.knowledge.as_str()),
            ("chaoxing.job_id", binding.job.as_str()),
        ] {
            if route.get(key) != Some(expected) {
                return Err(remote_changed(
                    "Chaoxing history result route binding changed",
                ));
            }
        }
        if request.reference.provider_attempt_digest != binding.attempt_digest() {
            return Err(remote_changed(
                "Chaoxing history attempt digest changed before result read",
            ));
        }
        Ok(Self { binding })
    }

    pub const fn remote_task_id(&self) -> &str {
        self.binding.remote_task.as_str()
    }

    pub const fn course_id(&self) -> &str {
        self.binding.course.as_str()
    }

    pub const fn class_id(&self) -> &str {
        self.binding.class.as_str()
    }

    pub const fn knowledge_id(&self) -> &str {
        self.binding.knowledge.as_str()
    }

    pub const fn job_id(&self) -> &str {
        self.binding.job.as_str()
    }

    pub const fn work_answer_id(&self) -> &str {
        self.binding.work_answer.as_str()
    }
}

impl fmt::Debug for ChaoxingChapterWorkHistoryResultRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterWorkHistoryResultRequest")
            .field("remote_task_id", &self.binding.remote_task)
            .field("course_id", &self.binding.course)
            .field("class_id", &self.binding.class)
            .field("knowledge_id", &self.binding.knowledge)
            .field("job_id", &self.binding.job)
            .field("work_answer_id", &"[REDACTED]")
            .finish()
    }
}

/// Abstract read-only boundary for audited completed Chapter Work history.
/// No Native implementation is claimed until a live BrowserBridge/Capture
/// route can list and read `selectWorkQuestionYiPiYue` safely.
#[async_trait]
pub trait ChaoxingAnswerHistoryTransport: Send + Sync {
    async fn list_completed_chapter_work(
        &self,
        context: &ProviderContext,
        page: u32,
    ) -> ProviderResult<ChaoxingChapterWorkHistoryPage>;

    async fn read_completed_chapter_work(
        &self,
        context: &ProviderContext,
        request: &ChaoxingChapterWorkHistoryResultRequest,
    ) -> ProviderResult<ChaoxingChapterWorkVerificationDocument>;
}

/// Typed `AnswerHistoryHarvest` implementation for the audited Chapter Work
/// result subset. It is intentionally absent from the development factory
/// until a real read-only transport exists.
pub struct ChaoxingAnswerHistoryHarvest {
    metadata: ProviderMetadata,
    transport: Arc<dyn ChaoxingAnswerHistoryTransport>,
}

impl ChaoxingAnswerHistoryHarvest {
    /// # Errors
    ///
    /// Returns a metadata error when the Provider build is invalid.
    pub fn try_new(transport: Arc<dyn ChaoxingAnswerHistoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for ChaoxingAnswerHistoryHarvest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingAnswerHistoryHarvest")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingAnswerHistoryHarvest {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl AnswerHistoryHarvestCapability for ChaoxingAnswerHistoryHarvest {
    async fn list_answer_history(
        &self,
        context: &ProviderContext,
        cursor: Option<&AnswerHistoryCursor>,
    ) -> ProviderResult<AnswerHistoryPage> {
        validate_context(context, &self.metadata)?;
        let page = parse_cursor(cursor, &self.metadata)?;
        let local = self
            .transport
            .list_completed_chapter_work(context, page)
            .await?;
        if local.next_page.is_some_and(|next| next <= page) {
            return Err(remote_changed(
                "Chaoxing history pagination did not advance",
            ));
        }
        let tasks = local
            .records
            .into_iter()
            .map(ChaoxingChapterWorkHistoryRecord::into_task_reference)
            .collect::<ProviderResult<Vec<_>>>()?;
        let next_cursor = local.next_page.map(|next_page| AnswerHistoryCursor {
            version: 1,
            cursor_type: CURSOR_TYPE.to_owned(),
            value_sanitized: json!({"page": next_page}),
        });
        AnswerHistoryPage::try_new(&self.metadata.id, tasks, next_cursor, local.complete)
            .map_err(|_| invalid_response("Chaoxing history page violates the shared contract"))
    }

    async fn read_answer_history_task(
        &self,
        context: &ProviderContext,
        request: &AnswerHistoryTaskRequest,
    ) -> ProviderResult<ProviderAnswerHistoryTaskEvidence> {
        validate_context(context, &self.metadata)?;
        let provider_request = ChaoxingChapterWorkHistoryResultRequest::try_from_core(request)?;
        let document = self
            .transport
            .read_completed_chapter_work(context, &provider_request)
            .await?;
        let result_digest = digest_parts(&[RESULT_EVIDENCE_SCHEMA, document.as_str()]);
        let questions = parse_chapter_work_question_page(document.as_str())?
            .iter()
            .map(|question| question.to_question(request.task_id))
            .collect::<ProviderResult<Vec<_>>>()?;
        let local = parse_chapter_work_result_evidence(&document, &questions)?;
        let question_evidence = local
            .questions()
            .iter()
            .map(|evidence| AnswerHistoryQuestionEvidence {
                question_id: evidence.question_id(),
                submitted_answer: Some(evidence.submitted_answer().clone()),
                official_answer: Some(evidence.official_answer().clone()),
                submitted_answer_correct: Some(
                    evidence.judgement() == ChaoxingChapterWorkAnswerJudgement::MatchesOfficial,
                ),
                provenance_sanitized: json!({
                    "schema": RESULT_EVIDENCE_SCHEMA,
                    "result_route": "selectWorkQuestionYiPiYue",
                    "submitted_label": evidence.submitted_answer_label(),
                    "official_label": evidence.official_answer_label(),
                }),
            })
            .collect::<Vec<_>>();
        let retake = Some(AnswerHistoryRetakeFacts {
            allowed: local.retake_entry() == Some(ChaoxingChapterWorkRetakeEntry::RedoTest),
            remaining_attempts: None,
            closes_at: None,
            score_policy: asterism_domain::RetakeScorePolicy::Unknown,
            metadata_sanitized: json!({
                "schema": "chaoxing.chapter-work-retake-facts.v1",
                "entry": local
                    .retake_entry()
                    .map(|_| "redoTest"),
                "mutation_authorized": false,
            }),
        });
        let evidence = ProviderAnswerHistoryTaskEvidence {
            task_id: request.task_id,
            provider_attempt_digest: request.reference.provider_attempt_digest,
            result_digest,
            questions,
            question_evidence,
            score: local.score(),
            retake,
            provenance_sanitized: json!({
                "schema": RESULT_EVIDENCE_SCHEMA,
                "source_family": "chapter_work",
                "result_route": "selectWorkQuestionYiPiYue",
                "read_only": true,
            }),
            observed_at: Utc::now(),
        };
        evidence.validate(request).map_err(|_| {
            invalid_response("Chaoxing history result violates the shared evidence contract")
        })?;
        Ok(evidence)
    }
}

struct ChapterHistoryBinding {
    remote_task: String,
    course_remote: String,
    course: String,
    class: String,
    knowledge: String,
    job: String,
    work_answer: String,
}

impl ChapterHistoryBinding {
    fn from_identity(
        remote_task_id: &str,
        course_remote_id: &str,
        work_answer_id: &str,
    ) -> ProviderResult<Self> {
        let components = remote_task_id.split(':').collect::<Vec<_>>();
        let ["resource", course_id, class_id, knowledge_id, job_id] = components.as_slice() else {
            return Err(unsupported(
                "Chaoxing history harvest supports only Chapter Work results",
            ));
        };
        if [course_id, class_id, knowledge_id, job_id, &work_answer_id]
            .into_iter()
            .any(|value| !valid_component(value))
            || course_remote_id != format!("course:{course_id}:{class_id}")
        {
            return Err(remote_changed(
                "Chaoxing history identity is malformed or cross-bound",
            ));
        }
        Ok(Self {
            remote_task: remote_task_id.to_owned(),
            course_remote: course_remote_id.to_owned(),
            course: (*course_id).to_owned(),
            class: (*class_id).to_owned(),
            knowledge: (*knowledge_id).to_owned(),
            job: (*job_id).to_owned(),
            work_answer: work_answer_id.to_owned(),
        })
    }

    fn attempt_digest(&self) -> [u8; 32] {
        digest_parts(&[
            ATTEMPT_DIGEST_SCHEMA,
            &self.remote_task,
            &self.course_remote,
            &self.work_answer,
        ])
    }

    fn task_reference(
        &self,
        completed_at: Option<Timestamp>,
    ) -> ProviderResult<AnswerHistoryTaskRef> {
        Ok(AnswerHistoryTaskRef {
            remote_task_id: self.remote_task.clone(),
            course_remote_id: Some(self.course_remote.clone()),
            provider_attempt_digest: self.attempt_digest(),
            completed_at,
            metadata_sanitized: json!({
                "schema": "chaoxing.chapter-work-history-reference.v1",
                "source_family": "chapter_work",
                "result_route": "selectWorkQuestionYiPiYue",
            }),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), self.course.clone()),
                ("chaoxing.class_id".to_owned(), self.class.clone()),
                ("chaoxing.knowledge_id".to_owned(), self.knowledge.clone()),
                ("chaoxing.job_id".to_owned(), self.job.clone()),
                (
                    "chaoxing.work_answer_id".to_owned(),
                    self.work_answer.clone(),
                ),
            ])?,
        })
    }
}

impl Drop for ChapterHistoryBinding {
    fn drop(&mut self) {
        self.work_answer.zeroize();
    }
}

fn parse_cursor(
    cursor: Option<&AnswerHistoryCursor>,
    metadata: &ProviderMetadata,
) -> ProviderResult<u32> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor
        .validate(&metadata.id)
        .map_err(|_| invalid_response("Chaoxing history cursor is invalid"))?;
    let value = cursor
        .value_sanitized
        .as_object()
        .filter(|value| value.len() == 1)
        .and_then(|value| value.get("page"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0);
    if cursor.version != 1 || cursor.cursor_type != CURSOR_TYPE || value.is_none() {
        return Err(invalid_response("Chaoxing history cursor changed"));
    }
    Ok(value.expect("validated nonzero cursor"))
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing history harvest received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing history harvest requires an authenticated session",
        ));
    }
    Ok(())
}

fn digest_parts(parts: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(part.as_bytes());
    }
    digest.finalize().into()
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::UnsupportedTask, message)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use asterism_domain::{
        CourseId, ProviderAccountId, ProviderId, SecretId, SubmissionScore, TaskId,
    };

    use super::*;

    const RESULT: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/chapter-history-result.html");

    #[derive(Debug, Default)]
    struct FixtureTransport {
        listed_pages: Mutex<Vec<u32>>,
        read_calls: AtomicUsize,
    }

    #[async_trait]
    impl ChaoxingAnswerHistoryTransport for FixtureTransport {
        async fn list_completed_chapter_work(
            &self,
            _context: &ProviderContext,
            page: u32,
        ) -> ProviderResult<ChaoxingChapterWorkHistoryPage> {
            self.listed_pages.lock().unwrap().push(page);
            match page {
                0 => ChaoxingChapterWorkHistoryPage::try_new(
                    vec![record("job-work", "answer-attempt-history-43")],
                    Some(1),
                    false,
                ),
                1 => ChaoxingChapterWorkHistoryPage::try_new(
                    vec![record("job-work-older", "answer-attempt-history-42")],
                    None,
                    true,
                ),
                _ => Err(remote_changed("fixture history page is unknown")),
            }
        }

        async fn read_completed_chapter_work(
            &self,
            _context: &ProviderContext,
            request: &ChaoxingChapterWorkHistoryResultRequest,
        ) -> ProviderResult<ChaoxingChapterWorkVerificationDocument> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(request.remote_task_id(), "resource:100:200:4001:job-work");
            assert_eq!(request.course_id(), "100");
            assert_eq!(request.class_id(), "200");
            assert_eq!(request.knowledge_id(), "4001");
            assert_eq!(request.job_id(), "job-work");
            assert_eq!(request.work_answer_id(), "answer-attempt-history-43");
            assert!(!format!("{request:?}").contains("answer-attempt-history-43"));
            ChaoxingChapterWorkVerificationDocument::try_new(RESULT.to_owned())
        }
    }

    #[tokio::test]
    async fn harvest_pages_and_reads_bound_chapter_result_evidence() {
        let transport = Arc::new(FixtureTransport::default());
        let harvest = ChaoxingAnswerHistoryHarvest::try_new(transport.clone()).unwrap();
        let first = harvest.list_answer_history(&context(), None).await.unwrap();
        assert_eq!(first.tasks().len(), 1);
        assert!(!first.is_complete());
        assert_eq!(
            first.tasks()[0].provider_attempt_digest,
            digest_parts(&[
                ATTEMPT_DIGEST_SCHEMA,
                "resource:100:200:4001:job-work",
                "course:100:200",
                "answer-attempt-history-43",
            ])
        );
        assert_eq!(
            first.tasks()[0].metadata_sanitized["source_family"],
            "chapter_work"
        );
        assert!(!format!("{:?}", first.tasks()[0]).contains("answer-attempt-history-43"));
        let cursor = first.next_cursor().unwrap().clone();
        let second = harvest
            .list_answer_history(&context(), Some(&cursor))
            .await
            .unwrap();
        assert!(second.is_complete());
        assert!(second.next_cursor().is_none());
        assert_ne!(
            first.tasks()[0].provider_attempt_digest,
            second.tasks()[0].provider_attempt_digest
        );
        assert_eq!(*transport.listed_pages.lock().unwrap(), [0, 1]);

        let task_id = TaskId::new();
        let request = AnswerHistoryTaskRequest {
            task_id,
            course_id: Some(CourseId::new()),
            reference: first.tasks()[0].clone(),
        };
        let evidence = harvest
            .read_answer_history_task(&context(), &request)
            .await
            .unwrap();
        assert_eq!(evidence.task_id, task_id);
        assert_eq!(
            evidence.provider_attempt_digest,
            request.reference.provider_attempt_digest
        );
        assert_eq!(
            evidence.result_digest,
            digest_parts(&[RESULT_EVIDENCE_SCHEMA, RESULT])
        );
        assert_eq!(evidence.questions.len(), 3);
        assert!(
            evidence
                .questions
                .iter()
                .all(|question| question.task_id == task_id)
        );
        assert_eq!(
            evidence
                .question_evidence
                .iter()
                .map(|question| question.submitted_answer_correct)
                .collect::<Vec<_>>(),
            [Some(false), Some(true), Some(false)]
        );
        assert!(
            evidence
                .question_evidence
                .iter()
                .all(|question| question.submitted_answer.is_some()
                    && question.official_answer.is_some())
        );
        assert_eq!(
            evidence.score,
            Some(SubmissionScore {
                earned_milli_points: 60_000,
                possible_milli_points: 100_000,
            })
        );
        assert_eq!(
            evidence.retake.as_ref().map(|facts| facts.allowed),
            Some(true)
        );
        assert_eq!(transport.read_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn cursor_and_attempt_drift_fail_before_result_transport() {
        let transport = Arc::new(FixtureTransport::default());
        let harvest = ChaoxingAnswerHistoryHarvest::try_new(transport.clone()).unwrap();
        let invalid_cursor = AnswerHistoryCursor {
            version: 1,
            cursor_type: CURSOR_TYPE.to_owned(),
            value_sanitized: json!({"page": 0}),
        };
        assert_eq!(
            harvest
                .list_answer_history(&context(), Some(&invalid_cursor))
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidResponse
        );

        let page = harvest.list_answer_history(&context(), None).await.unwrap();
        let mut reference = page.tasks()[0].clone();
        reference.provider_attempt_digest[0] ^= 1;
        let request = AnswerHistoryTaskRequest {
            task_id: TaskId::new(),
            course_id: None,
            reference,
        };
        assert_eq!(
            harvest
                .read_answer_history_task(&context(), &request)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
        assert_eq!(transport.read_calls.load(Ordering::Relaxed), 0);
    }

    fn record(job_id: &str, work_answer_id: &str) -> ChaoxingChapterWorkHistoryRecord {
        ChaoxingChapterWorkHistoryRecord::try_new(
            &format!("resource:100:200:4001:{job_id}"),
            "course:100:200",
            work_answer_id,
            None,
        )
        .unwrap()
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-history-harvest-test".to_owned(),
        }
    }
}
