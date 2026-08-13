use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use asterism_domain::{ProviderAccountId, Question, TaskId};
use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, QuestionInventoryCapability, QuestionParseCapability,
    RemoteCourse, RemoteQuestionRef,
};
use async_trait::async_trait;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingChapterWorkTarget,
    ChaoxingCourseRoute, ChaoxingInventoryDocument, ChaoxingInventoryTransport,
    ChaoxingWorkDetailRequest,
    inventory::parse_work_inventory_entries,
    metadata::development_metadata,
    parse_chapter_inventory,
    question_parser::{
        ParsedChaoxingQuestion, parse_chapter_work_question_page, parse_work_preview_question_page,
    },
    resource_inventory::locate_chapter_work_target,
    task_inventory::CHAPTER_RESOURCE_CARD_COUNT,
};

const MAX_ACTIVE_QUESTION_ATTEMPTS: usize = 128;
const QUESTION_ATTEMPT_TTL: Duration = Duration::from_mins(5);
const MAX_REMOTE_TASK_ID_BYTES: usize = 640;

/// Native read transport for one independently discovered Work Question page.
/// Implementations must preserve the Work route allowlist and bounded body
/// ownership used by inventory/detail reads.
#[async_trait]
pub trait ChaoxingQuestionTransport: Send + Sync {
    async fn fetch_work_question_document(
        &self,
        context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument>;

    /// Fetches the question page for one freshly rebound Chapter Work target.
    /// The request can create an attempt and must not be ambiguously replayed.
    async fn fetch_chapter_work_question_document(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
    ) -> ProviderResult<ChaoxingInventoryDocument>;
}

/// Development-level independent Work Question reader. Inventory and parsing
/// share one short-lived, process-local attempt cache so a Core all-or-nothing
/// read performs one remote page acquisition without persisting HTML or QIDs.
pub struct ChaoxingQuestionRead {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    inventory: Arc<dyn ChaoxingInventoryTransport>,
    transport: Arc<dyn ChaoxingQuestionTransport>,
    attempts: Mutex<HashMap<QuestionAttemptKey, CachedQuestionAttempt>>,
}

impl ChaoxingQuestionRead {
    /// Builds one independent Work Question read pipeline.
    ///
    /// # Errors
    ///
    /// Returns a sanitized internal error if compile-time Provider metadata is
    /// invalid.
    pub fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        inventory: Arc<dyn ChaoxingInventoryTransport>,
        transport: Arc<dyn ChaoxingQuestionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            courses,
            inventory,
            transport,
            attempts: Mutex::new(HashMap::new()),
        })
    }

    async fn discover_questions(
        &self,
        context: &ProviderContext,
        identity: &QuestionTaskIdentity<'_>,
    ) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
        let courses = self.courses.list_courses(context).await?;
        let course = matching_course(&courses, identity)?;
        let route = ChaoxingCourseRoute::from_remote_course(course)?;
        match identity {
            QuestionTaskIdentity::IndependentWork(identity) => {
                self.discover_independent_work_questions(context, route, identity)
                    .await
            }
            QuestionTaskIdentity::ChapterWork(identity) => {
                self.discover_chapter_work_questions(context, route, identity)
                    .await
            }
        }
    }

    async fn discover_independent_work_questions(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        identity: &ScopedQuestionIdentity<'_>,
    ) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
        let document = self.inventory.fetch_work_inventory(context, route).await?;
        let entries = parse_work_inventory_entries(document.as_str(), &route.parser_scope()?)?;
        let mut matching = entries
            .iter()
            .filter(|entry| entry.task().remote_id == identity.remote_task);
        let entry = matching.next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing Work task is no longer present in fresh inventory",
            )
        })?;
        if matching.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing Work inventory contains duplicate task identity",
            ));
        }
        let request =
            ChaoxingWorkDetailRequest::try_new(route, identity.remote_task, entry.entry())?;
        let document = self
            .transport
            .fetch_work_question_document(context, request)
            .await?;
        parse_work_preview_question_page(document.as_str())
    }

    async fn discover_chapter_work_questions(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        identity: &ChapterWorkIdentity<'_>,
    ) -> ProviderResult<Vec<ParsedChaoxingQuestion>> {
        let document = self
            .inventory
            .fetch_chapter_inventory(context, route)
            .await?;
        let scope = route.parser_scope()?;
        let chapters = parse_chapter_inventory(document.as_str(), &scope)?;
        let mut matching = chapters.iter().filter(|chapter| {
            chapter
                .normalized
                .get("knowledge_id")
                .and_then(serde_json::Value::as_str)
                == Some(identity.knowledge)
        });
        let chapter = matching.next().ok_or_else(chapter_work_changed)?;
        if matching.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter inventory contains duplicate Question scope",
            ));
        }
        let request = ChaoxingChapterResourceRequest::try_from_available_chapter(chapter)?
            .ok_or_else(chapter_work_changed)?;
        if !request.belongs_to(route) || request.knowledge_id() != identity.knowledge {
            return Err(protocol_drift(
                "Chaoxing Chapter Work Question request lost its route binding",
            ));
        }
        let documents = self
            .inventory
            .fetch_chapter_resource_inventories(context, route, std::slice::from_ref(&request))
            .await?;
        let target =
            locate_chapter_work_in_documents(documents, route, &request, identity.remote_task)?;
        let document = self
            .transport
            .fetch_chapter_work_question_document(context, route, &request, &target)
            .await?;
        parse_chapter_work_question_page(document.as_str())
    }

    fn store_attempt(
        &self,
        key: QuestionAttemptKey,
        questions: Vec<ParsedChaoxingQuestion>,
    ) -> ProviderResult<Vec<RemoteQuestionRef>> {
        let references = questions
            .iter()
            .map(ParsedChaoxingQuestion::reference)
            .collect::<ProviderResult<Vec<_>>>()?;
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| internal("Chaoxing Question attempt cache lock is unavailable"))?;
        attempts.retain(|_, attempt| attempt.created_at.elapsed() < QUESTION_ATTEMPT_TTL);
        if attempts.len() >= MAX_ACTIVE_QUESTION_ATTEMPTS && !attempts.contains_key(&key) {
            let mut error = ProviderError::new(
                ProviderErrorKind::RateLimited,
                "Chaoxing Question attempt cache is temporarily full",
            );
            error.retry_after_seconds = Some(1);
            return Err(error);
        }
        attempts.insert(
            key,
            CachedQuestionAttempt {
                created_at: Instant::now(),
                questions,
            },
        );
        Ok(references)
    }
}

impl fmt::Debug for ChaoxingQuestionRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingQuestionRead")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("inventory", &"configured")
            .field("transport", &"configured")
            .field("attempts", &"[REDACTED]")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingQuestionRead {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl QuestionInventoryCapability for ChaoxingQuestionRead {
    async fn list_question_refs(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<Vec<RemoteQuestionRef>> {
        validate_context(context, &self.metadata)?;
        let identity = QuestionTaskIdentity::parse(remote_task_id)?;
        let questions = self.discover_questions(context, &identity).await?;
        self.store_attempt(QuestionAttemptKey::new(context, remote_task_id), questions)
    }
}

#[async_trait]
impl QuestionParseCapability for ChaoxingQuestionRead {
    async fn parse_question(
        &self,
        context: &ProviderContext,
        task_id: TaskId,
        remote_task_id: &str,
        question: &RemoteQuestionRef,
    ) -> ProviderResult<Question> {
        validate_context(context, &self.metadata)?;
        let identity = QuestionTaskIdentity::parse(remote_task_id)?;
        question
            .validate()
            .map_err(|_| invalid_response("Chaoxing Question reference is invalid"))?;
        if question.route_context.get("page_kind") != Some(identity.page_kind()) {
            return Err(invalid_response(
                "Chaoxing Question reference has a mismatched page kind",
            ));
        }
        let key = QuestionAttemptKey::new(context, remote_task_id);
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| internal("Chaoxing Question attempt cache lock is unavailable"))?;
        let attempt = attempts.get(&key).ok_or_else(question_attempt_changed)?;
        if attempt.created_at.elapsed() >= QUESTION_ATTEMPT_TTL {
            attempts.remove(&key);
            return Err(question_attempt_changed());
        }
        let parsed = attempt
            .questions
            .iter()
            .find(|parsed| parsed.matches_reference(question))
            .cloned()
            .ok_or_else(question_attempt_changed)?;
        let is_last = usize::try_from(question.position)
            .is_ok_and(|position| position == attempt.questions.len());
        let normalized = parsed.to_question(task_id)?;
        if is_last {
            attempts.remove(&key);
        }
        Ok(normalized)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct QuestionAttemptKey {
    account: ProviderAccountId,
    correlation: String,
    remote_task: String,
}

impl QuestionAttemptKey {
    fn new(context: &ProviderContext, remote_task_id: &str) -> Self {
        Self {
            account: context.account_id,
            correlation: context.correlation_id.clone(),
            remote_task: remote_task_id.to_owned(),
        }
    }
}

#[derive(Debug)]
struct CachedQuestionAttempt {
    created_at: Instant,
    questions: Vec<ParsedChaoxingQuestion>,
}

#[derive(Clone, Copy, Debug)]
struct ScopedQuestionIdentity<'a> {
    remote_task: &'a str,
    course: &'a str,
    class: &'a str,
}

#[derive(Clone, Copy, Debug)]
struct ChapterWorkIdentity<'a> {
    remote_task: &'a str,
    course: &'a str,
    class: &'a str,
    knowledge: &'a str,
}

#[derive(Clone, Copy, Debug)]
enum QuestionTaskIdentity<'a> {
    IndependentWork(ScopedQuestionIdentity<'a>),
    ChapterWork(ChapterWorkIdentity<'a>),
}

impl<'a> QuestionTaskIdentity<'a> {
    fn parse(remote_task_id: &'a str) -> ProviderResult<Self> {
        if remote_task_id.is_empty()
            || remote_task_id.len() > MAX_REMOTE_TASK_ID_BYTES
            || remote_task_id.chars().any(char::is_control)
        {
            return Err(protocol_drift("Chaoxing remote Work identity is invalid"));
        }
        let parts = remote_task_id.split(':').collect::<Vec<_>>();
        match parts.as_slice() {
            ["work", course, class, work]
                if [course, class, work]
                    .into_iter()
                    .all(|component| valid_component(component)) =>
            {
                Ok(Self::IndependentWork(ScopedQuestionIdentity {
                    remote_task: remote_task_id,
                    course,
                    class,
                }))
            }
            ["resource", course, class, knowledge, job]
                if [course, class, knowledge, job]
                    .into_iter()
                    .all(|component| valid_component(component)) =>
            {
                Ok(Self::ChapterWork(ChapterWorkIdentity {
                    remote_task: remote_task_id,
                    course,
                    class,
                    knowledge,
                }))
            }
            _ => Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing Question read supports Work and Chapter Work tasks",
            )),
        }
    }

    const fn course(self) -> &'a str {
        match self {
            Self::IndependentWork(identity) => identity.course,
            Self::ChapterWork(identity) => identity.course,
        }
    }

    const fn class(self) -> &'a str {
        match self {
            Self::IndependentWork(identity) => identity.class,
            Self::ChapterWork(identity) => identity.class,
        }
    }

    const fn page_kind(self) -> &'static str {
        match self {
            Self::IndependentWork(_) => "work_preview",
            Self::ChapterWork(_) => "chapter_work_mobile",
        }
    }
}

fn matching_course<'a>(
    courses: &'a [RemoteCourse],
    identity: &QuestionTaskIdentity<'_>,
) -> ProviderResult<&'a RemoteCourse> {
    let mut matching = courses.iter().filter(|course| {
        ChaoxingCourseRoute::from_remote_course(course).is_ok_and(|route| {
            route.course_id() == identity.course() && route.class_id() == identity.class()
        })
    });
    let course = matching.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Chaoxing Work course is no longer present in fresh inventory",
        )
    })?;
    if matching.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing fresh inventory contains duplicate course identity",
        ));
    }
    Ok(course)
}

fn locate_chapter_work_in_documents(
    documents: Vec<ChaoxingChapterResourceDocument>,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<ChaoxingChapterWorkTarget> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing Chapter Work Question lookup received an incomplete card set",
        ));
    }
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing Chapter Work Question lookup received a foreign or duplicate card",
            ));
        }
    }
    let scope = route.parser_scope()?;
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed.remove(&card_index).ok_or_else(|| {
            protocol_drift("Chaoxing Chapter Work Question lookup omitted a card")
        })?;
        let Some(target) = locate_chapter_work_target(
            document.as_str(),
            &scope,
            request.knowledge_id(),
            card_index,
            remote_task_id,
        )?
        else {
            continue;
        };
        if found.replace(target).is_some() {
            return Err(protocol_drift(
                "Chaoxing Chapter Work Question target appears on multiple cards",
            ));
        }
    }
    found.ok_or_else(chapter_work_changed)
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "Chaoxing Question read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing Question read requires an authenticated session",
        ));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn question_attempt_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "Chaoxing Question attempt is missing, expired, or no longer matches",
    )
}

fn chapter_work_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "Chaoxing Chapter Work is missing or no longer exposes a fresh Question attempt",
    )
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderId, SecretId};
    use asterism_provider_api::ProviderRouteContext;

    use super::*;

    const WORK_LIST: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.html");
    const WORK_QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-preview-mixed.html");
    const CHAPTER_LIST: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
    const CHAPTER_CARDS: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");
    const CHAPTER_QUESTIONS: &str =
        include_str!("../../../fixtures/providers/chaoxing/questions/work-mobile-mixed.html");

    #[derive(Debug)]
    struct FixtureCourses {
        metadata: ProviderMetadata,
    }

    impl FixtureCourses {
        fn new() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
            }
        }
    }

    impl ProviderIdentity for FixtureCourses {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for FixtureCourses {
        async fn list_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            Ok(vec![course()])
        }
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        work: WorkCounters,
        chapter: ChapterCounters,
    }

    #[derive(Debug, Default)]
    struct WorkCounters {
        inventory: AtomicUsize,
        question: AtomicUsize,
    }

    #[derive(Debug, Default)]
    struct ChapterCounters {
        inventory: AtomicUsize,
        resources: AtomicUsize,
        question: AtomicUsize,
    }

    #[async_trait]
    impl ChaoxingInventoryTransport for FixtureTransport {
        async fn fetch_chapter_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            self.chapter.inventory.fetch_add(1, Ordering::Relaxed);
            ChaoxingInventoryDocument::try_new(CHAPTER_LIST)
        }

        async fn fetch_work_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            self.work.inventory.fetch_add(1, Ordering::Relaxed);
            ChaoxingInventoryDocument::try_new(WORK_LIST)
        }

        async fn fetch_chapter_resource_inventories(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            requests: &[crate::ChaoxingChapterResourceRequest],
        ) -> ProviderResult<Vec<crate::ChaoxingChapterResourceDocument>> {
            self.chapter.resources.fetch_add(1, Ordering::Relaxed);
            let request = requests.first().ok_or_else(unsupported_fixture_call)?;
            if requests.len() != 1 {
                return Err(unsupported_fixture_call());
            }
            (0..CHAPTER_RESOURCE_CARD_COUNT)
                .map(|card_index| {
                    ChaoxingChapterResourceDocument::for_request(
                        request,
                        card_index,
                        if card_index == 0 {
                            CHAPTER_CARDS
                        } else {
                            "<script>mArg={\"defaults\":{},\"attachments\":[]};</script>"
                        },
                    )
                })
                .collect()
        }

        async fn fetch_exam_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            Err(unsupported_fixture_call())
        }

        async fn fetch_work_detail_states(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            _requests: &[ChaoxingWorkDetailRequest<'_>],
        ) -> ProviderResult<Vec<crate::ChaoxingWorkDetailState>> {
            Err(unsupported_fixture_call())
        }
    }

    #[async_trait]
    impl ChaoxingQuestionTransport for FixtureTransport {
        async fn fetch_work_question_document(
            &self,
            _context: &ProviderContext,
            _request: ChaoxingWorkDetailRequest<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            self.work.question.fetch_add(1, Ordering::Relaxed);
            ChaoxingInventoryDocument::try_new(WORK_QUESTIONS)
        }

        async fn fetch_chapter_work_question_document(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            _request: &ChaoxingChapterResourceRequest,
            _target: &ChaoxingChapterWorkTarget,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            self.chapter.question.fetch_add(1, Ordering::Relaxed);
            ChaoxingInventoryDocument::try_new(CHAPTER_QUESTIONS)
        }
    }

    #[tokio::test]
    async fn one_fresh_page_is_shared_by_inventory_and_all_parses() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = ChaoxingQuestionRead::try_new(
            Arc::new(FixtureCourses::new()),
            transport.clone(),
            transport.clone(),
        )
        .unwrap();
        let context = context("question-read-1");
        let remote_task_id = "work:100:200:work-1";
        let references = capability
            .list_question_refs(&context, remote_task_id)
            .await
            .unwrap();
        assert_eq!(references.len(), 2);
        let task_id = TaskId::new();
        for reference in &references {
            let question = capability
                .parse_question(&context, task_id, remote_task_id, reference)
                .await
                .unwrap();
            assert_eq!(question.task_id, task_id);
            assert_eq!(
                question.remote_question_id.as_deref(),
                Some(reference.remote_id.as_str())
            );
        }
        assert_eq!(transport.work.inventory.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work.question.load(Ordering::Relaxed), 1);
        let expired = capability
            .parse_question(&context, task_id, remote_task_id, &references[0])
            .await
            .unwrap_err();
        assert_eq!(expired.kind, ProviderErrorKind::RemoteChanged);
    }

    #[tokio::test]
    async fn cache_is_bound_to_account_correlation_and_task() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = ChaoxingQuestionRead::try_new(
            Arc::new(FixtureCourses::new()),
            transport.clone(),
            transport,
        )
        .unwrap();
        let references = capability
            .list_question_refs(&context("question-read-1"), "work:100:200:work-1")
            .await
            .unwrap();
        let error = capability
            .parse_question(
                &context("question-read-2"),
                TaskId::new(),
                "work:100:200:work-1",
                &references[0],
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RemoteChanged);
    }

    #[tokio::test]
    async fn chapter_work_is_freshly_rebound_and_exam_remains_a_separate_family() {
        let transport = Arc::new(FixtureTransport::default());
        let capability = ChaoxingQuestionRead::try_new(
            Arc::new(FixtureCourses::new()),
            transport.clone(),
            transport.clone(),
        )
        .unwrap();
        let remote_task_id = "resource:100:200:4001:job-work";
        let chapter_context = context("chapter-work-question");
        let references = capability
            .list_question_refs(&chapter_context, remote_task_id)
            .await
            .unwrap();
        assert_eq!(references.len(), 4);
        for reference in &references {
            assert_eq!(
                reference.route_context.get("page_kind"),
                Some("chapter_work_mobile")
            );
            capability
                .parse_question(&chapter_context, TaskId::new(), remote_task_id, reference)
                .await
                .unwrap();
        }
        assert_eq!(transport.chapter.inventory.load(Ordering::Relaxed), 1);
        assert_eq!(transport.chapter.resources.load(Ordering::Relaxed), 1);
        assert_eq!(transport.chapter.question.load(Ordering::Relaxed), 1);

        let error = capability
            .list_question_refs(&context("unsupported"), "exam:100:200:exam-1")
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert_eq!(transport.work.inventory.load(Ordering::Relaxed), 0);
        assert_eq!(transport.work.question.load(Ordering::Relaxed), 0);
    }

    fn course() -> RemoteCourse {
        RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "Fixture course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({}),
            route_context: ProviderRouteContext::try_from_pairs([
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        }
    }

    fn context(correlation_id: &str) -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: correlation_id.to_owned(),
        }
    }

    fn unsupported_fixture_call() -> ProviderError {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "unexpected Fixture transport call",
        )
    }
}
