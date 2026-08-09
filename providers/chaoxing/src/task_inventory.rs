use std::{collections::HashMap, fmt, sync::Arc};

use asterism_domain::RemoteState;
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, RemoteCourse, RemoteTask, TaskInventoryCapability,
};
use async_trait::async_trait;
use reqwest::Url;
use zeroize::Zeroize;

use crate::{
    ChaoxingCourseScope,
    inventory::{apply_work_detail_state, parse_work_inventory_entries},
    metadata::development_metadata,
    parse_chapter_inventory, parse_chapter_resource_inventory, parse_exam_inventory,
};

const COURSE_ID_ROUTE_KEY: &str = "chaoxing.course_id";
const CLASS_ID_ROUTE_KEY: &str = "chaoxing.class_id";
const CPI_ROUTE_KEY: &str = "chaoxing.cpi";
const MAX_INVENTORY_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_WORK_DETAIL_REQUESTS: usize = 256;
const MAX_WORK_DETAIL_ROUTE_BYTES: usize = 8 * 1_024;
pub(crate) const MAX_RESOURCE_CHAPTER_REQUESTS: usize = 64;
pub(crate) const CHAPTER_RESOURCE_CARD_COUNT: u8 = 7;
pub(crate) const MAX_RESOURCE_BATCH_DOCUMENT_BYTES: usize = 32 * 1_024 * 1_024;
const WORK_DETAIL_ORIGIN: &str = "https://mooc1.chaoxing.com";

/// Borrowed Chaoxing routing facts carried only inside one Provider scan.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ChaoxingCourseRoute<'a> {
    remote_course_id: &'a str,
    course_id: &'a str,
    class_id: &'a str,
    cpi: &'a str,
}

impl<'a> ChaoxingCourseRoute<'a> {
    pub(crate) fn from_remote_course(course: &'a RemoteCourse) -> ProviderResult<Self> {
        let route = Self {
            remote_course_id: &course.remote_id,
            course_id: required_route_fact(course, COURSE_ID_ROUTE_KEY)?,
            class_id: required_route_fact(course, CLASS_ID_ROUTE_KEY)?,
            cpi: required_route_fact(course, CPI_ROUTE_KEY)?,
        };
        ChaoxingCourseScope::new(route.remote_course_id, route.course_id, route.class_id)?;
        validate_cpi(route.cpi)?;
        Ok(route)
    }

    pub const fn remote_course_id(self) -> &'a str {
        self.remote_course_id
    }

    pub const fn course_id(self) -> &'a str {
        self.course_id
    }

    pub const fn class_id(self) -> &'a str {
        self.class_id
    }

    /// Exposes the scan-local route value only to an authorized transport.
    pub const fn cpi(self) -> &'a str {
        self.cpi
    }

    pub(crate) fn parser_scope(self) -> ProviderResult<ChaoxingCourseScope> {
        ChaoxingCourseScope::new(self.remote_course_id, self.course_id, self.class_id)
    }
}

impl fmt::Debug for ChaoxingCourseRoute<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingCourseRoute")
            .field("values", &"[REDACTED]")
            .finish()
    }
}

/// One bounded inventory response whose body is redacted and zeroized on drop.
pub struct ChaoxingInventoryDocument(String);

impl ChaoxingInventoryDocument {
    /// Wraps a completed Work or Exam inventory response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for empty or oversized documents.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_INVENTORY_DOCUMENT_BYTES {
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing inventory document is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) const fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for ChaoxingInventoryDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChaoxingInventoryDocument([REDACTED])")
    }
}

impl Drop for ChaoxingInventoryDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// One bounded chapter-resource request derived from a pending Chapter task.
#[derive(Clone, Eq, PartialEq)]
pub struct ChaoxingChapterResourceRequest {
    course: String,
    class: String,
    knowledge: String,
}

impl ChaoxingChapterResourceRequest {
    pub(crate) fn try_from_chapter(task: &RemoteTask) -> ProviderResult<Option<Self>> {
        let request = Self::try_from_available_chapter(task)?;
        if task.remote_state != RemoteState::Pending {
            return Ok(None);
        }
        Ok(request)
    }

    pub(crate) fn try_from_available_chapter(task: &RemoteTask) -> ProviderResult<Option<Self>> {
        if task.source_type != asterism_domain::SourceType::Chapter
            || task
                .normalized
                .get("schema")
                .and_then(serde_json::Value::as_str)
                != Some("chaoxing.chapter.v1")
        {
            return Err(protocol_drift(
                "Chaoxing chapter task cannot produce a resource request",
            ));
        }
        let job_count = task
            .normalized
            .get("job_count")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| protocol_drift("Chaoxing chapter task has no valid job count"))?;
        if task.remote_state == RemoteState::NotOpen || job_count == 0 {
            return Ok(None);
        }
        let knowledge_id = task
            .normalized
            .get("knowledge_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| protocol_drift("Chaoxing chapter task has no knowledge identity"))?;
        validate_knowledge_id(knowledge_id)?;
        let course_id = required_normalized_string(task, "course_id")?;
        let class_id = required_normalized_string(task, "class_id")?;
        Ok(Some(Self {
            course: course_id.to_owned(),
            class: class_id.to_owned(),
            knowledge: knowledge_id.to_owned(),
        }))
    }

    pub fn knowledge_id(&self) -> &str {
        &self.knowledge
    }

    pub(crate) fn belongs_to(&self, route: ChaoxingCourseRoute<'_>) -> bool {
        self.course == route.course_id() && self.class == route.class_id()
    }
}

impl fmt::Debug for ChaoxingChapterResourceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterResourceRequest")
            .field("course_id", &"[REDACTED]")
            .field("class_id", &"[REDACTED]")
            .field("knowledge_id", &"[REDACTED]")
            .finish()
    }
}

/// One card document bound to the originating chapter request and card index.
pub struct ChaoxingChapterResourceDocument {
    knowledge_id: String,
    card_index: u8,
    document: ChaoxingInventoryDocument,
}

impl ChaoxingChapterResourceDocument {
    /// Binds one bounded response to an authorized resource request.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error when the card index is outside the
    /// donor-observed range or the document is empty or oversized.
    pub fn for_request(
        request: &ChaoxingChapterResourceRequest,
        card_index: u8,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        Self::from_document(
            request,
            card_index,
            ChaoxingInventoryDocument::try_new(document)?,
        )
    }

    pub(crate) fn from_document(
        request: &ChaoxingChapterResourceRequest,
        card_index: u8,
        document: ChaoxingInventoryDocument,
    ) -> ProviderResult<Self> {
        if card_index >= CHAPTER_RESOURCE_CARD_COUNT {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing chapter resource card index exceeds the size limit",
            ));
        }
        Ok(Self {
            knowledge_id: request.knowledge.clone(),
            card_index,
            document,
        })
    }

    pub(crate) fn knowledge_id(&self) -> &str {
        &self.knowledge_id
    }

    pub(crate) const fn card_index(&self) -> u8 {
        self.card_index
    }

    pub(crate) fn as_str(&self) -> &str {
        self.document.as_str()
    }
}

impl fmt::Debug for ChaoxingChapterResourceDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingChapterResourceDocument")
            .field("knowledge_id", &"[REDACTED]")
            .field("card_index", &self.card_index)
            .field("document", &self.document)
            .finish()
    }
}

/// One borrowed, scan-local Work route which has been bound to the parsed task
/// and course identities. Query material is exposed only to the authorized
/// transport and is always redacted from diagnostics.
#[derive(Clone, Copy)]
pub struct ChaoxingWorkDetailRequest<'a> {
    remote_task_id: &'a str,
    work_id: &'a str,
    route: &'a str,
}

impl<'a> ChaoxingWorkDetailRequest<'a> {
    fn try_new(
        course: ChaoxingCourseRoute<'_>,
        remote_task_id: &'a str,
        route: &'a str,
    ) -> ProviderResult<Self> {
        if route.is_empty() || route.len() > MAX_WORK_DETAIL_ROUTE_BYTES {
            return Err(protocol_drift(
                "Chaoxing Work detail route is empty or exceeds the size limit",
            ));
        }
        let work_id = remote_task_id
            .rsplit(':')
            .next()
            .ok_or_else(|| protocol_drift("Chaoxing Work task has an invalid remote identity"))?;
        let url = Url::parse(route).or_else(|_| {
            Url::parse(WORK_DETAIL_ORIGIN)
                .expect("static Chaoxing Work origin must be valid")
                .join(route)
        });
        let url = url.map_err(|_| protocol_drift("Chaoxing Work detail route is invalid"))?;
        if !valid_work_detail_url(&url)
            || unique_query(&url, "courseId").as_deref() != Some(course.course_id())
            || work_query_id(&url).as_deref() != Some(work_id)
        {
            return Err(protocol_drift(
                "Chaoxing Work detail route is not bound to the current task",
            ));
        }
        Ok(Self {
            remote_task_id,
            work_id,
            route,
        })
    }

    pub const fn remote_task_id(self) -> &'a str {
        self.remote_task_id
    }

    pub const fn work_id(self) -> &'a str {
        self.work_id
    }

    /// Exposes the short-lived route only to an inventory transport.
    pub const fn route(self) -> &'a str {
        self.route
    }

    pub(crate) fn url(self) -> ProviderResult<Url> {
        Url::parse(self.route)
            .or_else(|_| {
                Url::parse(WORK_DETAIL_ORIGIN)
                    .expect("static Chaoxing Work origin must be valid")
                    .join(self.route)
            })
            .map_err(|_| protocol_drift("Chaoxing Work detail route is invalid"))
    }

    pub(crate) fn allows_redirect(self, url: &Url) -> bool {
        valid_work_detail_url(url)
            && work_query_id(url).is_none_or(|work_id| work_id == self.work_id)
    }
}

impl fmt::Debug for ChaoxingWorkDetailRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingWorkDetailRequest")
            .field("remote_task_id", &self.remote_task_id)
            .field("work_id", &self.work_id)
            .field("route", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaoxingWorkDetailState {
    remote_task_id: String,
    remote_state: RemoteState,
}

impl ChaoxingWorkDetailState {
    pub fn for_request(request: ChaoxingWorkDetailRequest<'_>, remote_state: RemoteState) -> Self {
        Self {
            remote_task_id: request.remote_task_id().to_owned(),
            remote_state,
        }
    }
}

/// Runtime adapter which obtains current Chaoxing inventory pages. A concrete
/// adapter must resolve credentials through Core's secrets boundary, discover
/// a fresh Work `enc`, and preserve the bounded resource-card matrix; none of
/// those concerns belongs to the HTML parsers.
#[async_trait]
pub trait ChaoxingInventoryTransport: Send + Sync {
    async fn fetch_chapter_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument>;

    async fn fetch_work_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument>;

    async fn fetch_chapter_resource_inventories(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingChapterResourceRequest],
    ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>>;

    async fn fetch_exam_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument>;

    async fn fetch_work_detail_states(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        requests: &[ChaoxingWorkDetailRequest<'_>],
    ) -> ProviderResult<Vec<ChaoxingWorkDetailState>>;
}

/// Development-level Chaoxing task inventory capability. Daemon registration
/// remains a separate composition-root decision until live verification.
pub struct ChaoxingTaskInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn ChaoxingInventoryTransport>,
}

impl ChaoxingTaskInventory {
    /// Creates a development-level task inventory around one runtime transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the compile-time Provider ID is invalid.
    pub fn try_new(transport: Arc<dyn ChaoxingInventoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for ChaoxingTaskInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingTaskInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingTaskInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskInventoryCapability for ChaoxingTaskInventory {
    async fn list_tasks(
        &self,
        context: &ProviderContext,
        course: Option<&RemoteCourse>,
    ) -> ProviderResult<Vec<RemoteTask>> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing task inventory received a mismatched Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing task inventory requires an authenticated session",
            ));
        }
        let course = course.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing task inventory requires a course-scoped call",
            )
        })?;
        let route = ChaoxingCourseRoute::from_remote_course(course)?;
        let scope = route.parser_scope()?;

        let chapter = self
            .transport
            .fetch_chapter_inventory(context, route)
            .await?;
        let mut tasks = parse_chapter_inventory(chapter.as_str(), &scope)?;
        let resource_requests = chapter_resource_requests(&tasks)?;
        let resource_documents = if resource_requests.is_empty() {
            Vec::new()
        } else {
            self.transport
                .fetch_chapter_resource_inventories(context, route, &resource_requests)
                .await?
        };
        tasks.extend(parse_resource_documents(
            resource_documents,
            &resource_requests,
            &scope,
        )?);
        let work = self.transport.fetch_work_inventory(context, route).await?;
        let exam = self.transport.fetch_exam_inventory(context, route).await?;
        let mut work_tasks = parse_work_inventory_entries(work.as_str(), &scope)?;
        let requests = work_tasks
            .iter()
            .filter(|parsed| parsed.task().remote_state == RemoteState::Pending)
            .map(|parsed| {
                ChaoxingWorkDetailRequest::try_new(route, &parsed.task().remote_id, parsed.entry())
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        if requests.len() > MAX_WORK_DETAIL_REQUESTS {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing pending Work detail count exceeds the size limit",
            ));
        }
        let detail_states = if requests.is_empty() {
            Vec::new()
        } else {
            self.transport
                .fetch_work_detail_states(context, route, &requests)
                .await?
        };
        let mut states = HashMap::with_capacity(detail_states.len());
        for detail in detail_states {
            if states
                .insert(detail.remote_task_id, detail.remote_state)
                .is_some()
            {
                return Err(protocol_drift(
                    "Chaoxing Work detail transport returned duplicate task state",
                ));
            }
        }
        for parsed in &mut work_tasks {
            if parsed.task().remote_state != RemoteState::Pending {
                continue;
            }
            let state = states.remove(&parsed.task().remote_id).ok_or_else(|| {
                protocol_drift("Chaoxing Work detail transport omitted a requested task")
            })?;
            apply_work_detail_state(parsed.task_mut(), state)?;
        }
        if !states.is_empty() {
            return Err(protocol_drift(
                "Chaoxing Work detail transport returned an unexpected task",
            ));
        }
        tasks.extend(
            work_tasks
                .into_iter()
                .map(crate::inventory::ChaoxingParsedInventoryTask::into_task),
        );
        tasks.extend(parse_exam_inventory(exam.as_str(), &scope)?);
        Ok(tasks)
    }
}

fn chapter_resource_requests(
    chapters: &[RemoteTask],
) -> ProviderResult<Vec<ChaoxingChapterResourceRequest>> {
    let requests = chapters
        .iter()
        .filter_map(|task| ChaoxingChapterResourceRequest::try_from_chapter(task).transpose())
        .collect::<ProviderResult<Vec<_>>>()?;
    if requests.len() > MAX_RESOURCE_CHAPTER_REQUESTS {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing pending resource chapter count exceeds the size limit",
        ));
    }
    Ok(requests)
}

fn parse_resource_documents(
    documents: Vec<ChaoxingChapterResourceDocument>,
    requests: &[ChaoxingChapterResourceRequest],
    scope: &ChaoxingCourseScope,
) -> ProviderResult<Vec<RemoteTask>> {
    let total_bytes = documents
        .iter()
        .try_fold(0_usize, |total, document| {
            total.checked_add(document.document.len())
        })
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Chaoxing resource card batch exceeds the aggregate size limit",
            )
        })?;
    if total_bytes > MAX_RESOURCE_BATCH_DOCUMENT_BYTES {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing resource card batch exceeds the aggregate size limit",
        ));
    }
    let mut indexed_documents = HashMap::new();
    for document in documents {
        let key = (document.knowledge_id.clone(), document.card_index);
        if indexed_documents.insert(key, document).is_some() {
            return Err(protocol_drift(
                "Chaoxing resource transport returned a duplicate card",
            ));
        }
    }
    let mut documents = indexed_documents;
    let expected_count = requests
        .len()
        .saturating_mul(usize::from(CHAPTER_RESOURCE_CARD_COUNT));
    if documents.len() != expected_count {
        return Err(protocol_drift(
            "Chaoxing resource transport returned an incomplete or duplicate card set",
        ));
    }
    let mut tasks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for request in requests {
        for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
            let document = documents
                .remove(&(request.knowledge.clone(), card_index))
                .ok_or_else(|| {
                    protocol_drift("Chaoxing resource transport omitted a requested card")
                })?;
            for task in parse_chapter_resource_inventory(
                document.document.as_str(),
                scope,
                request.knowledge_id(),
                card_index,
            )? {
                if !seen.insert(task.remote_id.clone()) {
                    return Err(protocol_drift(
                        "Chaoxing resource cards contain a duplicate task identity",
                    ));
                }
                tasks.push(task);
            }
        }
    }
    if !documents.is_empty() {
        return Err(protocol_drift(
            "Chaoxing resource transport returned an unexpected card",
        ));
    }
    Ok(tasks)
}

fn valid_work_detail_url(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    url.as_str().len() <= MAX_WORK_DETAIL_ROUTE_BYTES
        && url.query_pairs().count() <= 64
        && url.scheme() == "https"
        && url.host_str() == Some("mooc1.chaoxing.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && ["/work/task", "/work/dowork", "/work/view", "/work/prompt"]
            .into_iter()
            .any(|suffix| path.ends_with(suffix))
}

fn work_query_id(url: &Url) -> Option<std::borrow::Cow<'_, str>> {
    ["workId", "oldWorkId", "jobid"]
        .into_iter()
        .find_map(|key| unique_query(url, key))
}

fn unique_query<'a>(url: &'a Url, key: &str) -> Option<std::borrow::Cow<'a, str>> {
    let mut values = url
        .query_pairs()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn required_route_fact<'a>(course: &'a RemoteCourse, key: &str) -> ProviderResult<&'a str> {
    course.route_context.get(key).ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing course is missing a required ephemeral route fact",
        )
    })
}

fn validate_cpi(value: &str) -> ProviderResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ProviderError::new(
            ProviderErrorKind::InvalidResponse,
            "Chaoxing course contains an invalid cpi route fact",
        ));
    }
    Ok(())
}

fn validate_knowledge_id(value: &str) -> ProviderResult<()> {
    if !(1..=20).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(protocol_drift(
            "Chaoxing chapter resource has an invalid knowledge identity",
        ));
    }
    Ok(())
}

fn required_normalized_string<'a>(task: &'a RemoteTask, key: &str) -> ProviderResult<&'a str> {
    task.normalized
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| protocol_drift("Chaoxing chapter task has incomplete route binding"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, SourceType};
    use asterism_provider_api::{ProviderCapability, ProviderRouteContext, VerificationLevel};

    use super::*;

    const EXAM_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/list-mixed.html");
    const CHAPTER_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
    const RESOURCE_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");
    const WORK_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.html");

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    enum ResourceFixtureBehavior {
        #[default]
        Complete,
        OmitLast,
        DuplicateFirst,
    }

    #[derive(Debug, Default)]
    struct FixtureTransport {
        chapter_calls: AtomicUsize,
        work_calls: AtomicUsize,
        exam_calls: AtomicUsize,
        resource_calls: AtomicUsize,
        work_detail_calls: AtomicUsize,
        fail_exam: bool,
        resource_behavior: ResourceFixtureBehavior,
        omit_work_detail: bool,
    }

    #[async_trait]
    impl ChaoxingInventoryTransport for FixtureTransport {
        async fn fetch_chapter_inventory(
            &self,
            _context: &ProviderContext,
            route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            assert_eq!(route.course_id(), "100");
            assert_eq!(route.class_id(), "200");
            assert_eq!(route.cpi(), "300");
            self.chapter_calls.fetch_add(1, Ordering::Relaxed);
            ChaoxingInventoryDocument::try_new(CHAPTER_MIXED)
        }

        async fn fetch_work_inventory(
            &self,
            _context: &ProviderContext,
            route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            assert_eq!(route.course_id(), "100");
            assert_eq!(route.class_id(), "200");
            assert_eq!(route.cpi(), "300");
            self.work_calls.fetch_add(1, Ordering::Relaxed);
            ChaoxingInventoryDocument::try_new(WORK_MIXED)
        }

        async fn fetch_chapter_resource_inventories(
            &self,
            _context: &ProviderContext,
            route: ChaoxingCourseRoute<'_>,
            requests: &[ChaoxingChapterResourceRequest],
        ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
            assert_eq!(route.course_id(), "100");
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].knowledge_id(), "4001");
            assert!(!format!("{:?}", requests[0]).contains("4001"));
            self.resource_calls.fetch_add(1, Ordering::Relaxed);
            let mut documents = Vec::new();
            for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
                if self.resource_behavior == ResourceFixtureBehavior::OmitLast
                    && card_index == CHAPTER_RESOURCE_CARD_COUNT - 1
                {
                    continue;
                }
                let document = if card_index == 0 {
                    RESOURCE_MIXED
                } else {
                    "<html><body>empty card slot</body></html>"
                };
                documents.push(ChaoxingChapterResourceDocument::for_request(
                    &requests[0],
                    card_index,
                    document,
                )?);
            }
            if self.resource_behavior == ResourceFixtureBehavior::DuplicateFirst {
                documents.push(ChaoxingChapterResourceDocument::for_request(
                    &requests[0],
                    0,
                    RESOURCE_MIXED,
                )?);
            }
            Ok(documents)
        }

        async fn fetch_exam_inventory(
            &self,
            _context: &ProviderContext,
            route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            assert_eq!(route.remote_course_id(), "course:100:200");
            self.exam_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_exam {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "sanitized fixture transport failure",
                ));
            }
            ChaoxingInventoryDocument::try_new(EXAM_MIXED)
        }

        async fn fetch_work_detail_states(
            &self,
            _context: &ProviderContext,
            route: ChaoxingCourseRoute<'_>,
            requests: &[ChaoxingWorkDetailRequest<'_>],
        ) -> ProviderResult<Vec<ChaoxingWorkDetailState>> {
            assert_eq!(route.course_id(), "100");
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].work_id(), "work-1");
            assert!(!format!("{:?}", requests[0]).contains("SAFE_ENC"));
            self.work_detail_calls.fetch_add(1, Ordering::Relaxed);
            if self.omit_work_detail {
                return Ok(Vec::new());
            }
            Ok(vec![ChaoxingWorkDetailState::for_request(
                requests[0],
                RemoteState::Expired,
            )])
        }
    }

    #[tokio::test]
    async fn capability_combines_independent_chapter_work_and_exam_inventories() {
        let transport = Arc::new(FixtureTransport::default());
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();
        let tasks = inventory
            .list_tasks(&context(), Some(&course()))
            .await
            .unwrap();

        assert_eq!(tasks.len(), 16);
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.source_type == SourceType::Chapter)
                .count(),
            4
        );
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.source_type == SourceType::Resource)
                .count(),
            4
        );
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.source_type == SourceType::Work)
                .count(),
            3
        );
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.source_type == SourceType::Exam)
                .count(),
            5
        );
        assert!(
            tasks
                .iter()
                .all(|task| task.course_remote_id.as_deref() == Some("course:100:200"))
        );
        assert_eq!(transport.chapter_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.exam_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.resource_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_detail_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            tasks
                .iter()
                .find(|task| task.remote_id.ends_with(":work-1"))
                .map(|task| task.remote_state),
            Some(RemoteState::Expired)
        );
        assert_eq!(
            inventory.metadata().verification,
            VerificationLevel::Development
        );
        assert_eq!(
            inventory.metadata().capabilities,
            std::collections::BTreeSet::from([
                ProviderCapability::Authentication,
                ProviderCapability::CourseInventory,
                ProviderCapability::TaskInventory,
                ProviderCapability::ResourceExecution,
            ])
        );
        assert!(inventory.metadata().capture_recipe_version.is_none());
    }

    #[tokio::test]
    async fn capability_rejects_missing_scope_before_transport() {
        let transport = Arc::new(FixtureTransport::default());
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();

        assert!(inventory.list_tasks(&context(), None).await.is_err());
        let mut missing_cpi = course();
        missing_cpi.route_context = ProviderRouteContext::try_from_pairs([
            (COURSE_ID_ROUTE_KEY.to_owned(), "100".to_owned()),
            (CLASS_ID_ROUTE_KEY.to_owned(), "200".to_owned()),
        ])
        .unwrap();
        assert!(
            inventory
                .list_tasks(&context(), Some(&missing_cpi))
                .await
                .is_err()
        );
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.exam_calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.chapter_calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.resource_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn capability_never_returns_partial_inventory() {
        let transport = Arc::new(FixtureTransport {
            fail_exam: true,
            ..FixtureTransport::default()
        });
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();
        let error = inventory
            .list_tasks(&context(), Some(&course()))
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::Network);
        assert_eq!(transport.chapter_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.exam_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.resource_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_detail_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn capability_rejects_an_incomplete_resource_card_scan() {
        let transport = Arc::new(FixtureTransport {
            resource_behavior: ResourceFixtureBehavior::OmitLast,
            ..FixtureTransport::default()
        });
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();
        let error = inventory
            .list_tasks(&context(), Some(&course()))
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        assert_eq!(transport.resource_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 0);
        assert_eq!(transport.exam_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn capability_rejects_a_duplicate_resource_card() {
        let transport = Arc::new(FixtureTransport {
            resource_behavior: ResourceFixtureBehavior::DuplicateFirst,
            ..FixtureTransport::default()
        });
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();
        let error = inventory
            .list_tasks(&context(), Some(&course()))
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        assert_eq!(transport.resource_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn capability_rejects_an_incomplete_work_detail_scan() {
        let transport = Arc::new(FixtureTransport {
            omit_work_detail: true,
            ..FixtureTransport::default()
        });
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();
        let error = inventory
            .list_tasks(&context(), Some(&course()))
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        assert_eq!(transport.work_detail_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn route_and_document_debug_output_are_redacted() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        assert!(!format!("{route:?}").contains("300"));
        let document = ChaoxingInventoryDocument::try_new("private page").unwrap();
        assert!(!format!("{document:?}").contains("private page"));
        let chapter = parse_chapter_inventory(CHAPTER_MIXED, &route.parser_scope().unwrap())
            .unwrap()
            .remove(0);
        let request = ChaoxingChapterResourceRequest::try_from_chapter(&chapter)
            .unwrap()
            .unwrap();
        let document =
            ChaoxingChapterResourceDocument::for_request(&request, 0, "private card").unwrap();
        let debug = format!("{document:?}");
        assert!(!debug.contains("4001"));
        assert!(!debug.contains("private card"));
    }

    #[test]
    fn work_detail_routes_remain_bound_to_course_task_and_origin() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        let valid = ChaoxingWorkDetailRequest::try_new(
            route,
            "work:100:200:work-1",
            "/mooc-ans/mooc2/work/task?courseId=100&workId=work-1&enc=SAFE_ENC",
        )
        .unwrap();
        assert!(
            valid.allows_redirect(
                &Url::parse("https://mooc1.chaoxing.com/mooc-ans/mooc2/work/view?workId=work-1")
                    .unwrap()
            )
        );
        assert!(
            !valid.allows_redirect(
                &Url::parse("https://mooc1.chaoxing.com/mooc-ans/mooc2/work/view?workId=other")
                    .unwrap()
            )
        );

        for candidate in [
            "https://evil.invalid/mooc-ans/mooc2/work/task?courseId=100&workId=work-1",
            "https://user@mooc1.chaoxing.com/mooc-ans/mooc2/work/task?courseId=100&workId=work-1",
            "https://mooc1.chaoxing.com/mooc-ans/mooc2/work/task?courseId=other&workId=work-1",
            "https://mooc1.chaoxing.com/mooc-ans/mooc2/work/task?courseId=100&workId=other",
        ] {
            assert!(
                ChaoxingWorkDetailRequest::try_new(route, "work:100:200:work-1", candidate,)
                    .is_err()
            );
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-test".to_owned(),
        }
    }

    fn course() -> RemoteCourse {
        RemoteCourse {
            remote_id: "course:100:200".to_owned(),
            title: "course".to_owned(),
            term: None,
            teacher: None,
            remote_status: None,
            metadata_sanitized: serde_json::json!({"safe": true}),
            route_context: ProviderRouteContext::try_from_pairs([
                (COURSE_ID_ROUTE_KEY.to_owned(), "100".to_owned()),
                (CLASS_ID_ROUTE_KEY.to_owned(), "200".to_owned()),
                (CPI_ROUTE_KEY.to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        }
    }
}
