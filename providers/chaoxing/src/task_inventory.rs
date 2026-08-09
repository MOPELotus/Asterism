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
    parse_exam_inventory,
};

const COURSE_ID_ROUTE_KEY: &str = "chaoxing.course_id";
const CLASS_ID_ROUTE_KEY: &str = "chaoxing.class_id";
const CPI_ROUTE_KEY: &str = "chaoxing.cpi";
const MAX_INVENTORY_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_WORK_DETAIL_REQUESTS: usize = 256;
const MAX_WORK_DETAIL_ROUTE_BYTES: usize = 8 * 1_024;
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

    fn parser_scope(self) -> ProviderResult<ChaoxingCourseScope> {
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
/// adapter must resolve credentials through Core's secrets boundary and discover
/// a fresh Work `enc`; neither concern belongs to the HTML parser.
#[async_trait]
pub trait ChaoxingInventoryTransport: Send + Sync {
    async fn fetch_work_inventory(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
    ) -> ProviderResult<ChaoxingInventoryDocument>;

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
        let mut tasks = work_tasks
            .into_iter()
            .map(crate::inventory::ChaoxingParsedInventoryTask::into_task)
            .collect::<Vec<_>>();
        tasks.extend(parse_exam_inventory(exam.as_str(), &scope)?);
        Ok(tasks)
    }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, SourceType};
    use asterism_provider_api::{ProviderCapability, ProviderRouteContext, VerificationLevel};

    use super::*;

    const EXAM_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/list-mixed.html");
    const WORK_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.html");

    #[derive(Debug, Default)]
    struct FixtureTransport {
        work_calls: AtomicUsize,
        exam_calls: AtomicUsize,
        work_detail_calls: AtomicUsize,
        fail_exam: bool,
        omit_work_detail: bool,
    }

    #[async_trait]
    impl ChaoxingInventoryTransport for FixtureTransport {
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
    async fn capability_combines_independent_work_and_exam_inventories() {
        let transport = Arc::new(FixtureTransport::default());
        let inventory = ChaoxingTaskInventory::try_new(transport.clone()).unwrap();
        let tasks = inventory
            .list_tasks(&context(), Some(&course()))
            .await
            .unwrap();

        assert_eq!(tasks.len(), 7);
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
            4
        );
        assert!(
            tasks
                .iter()
                .all(|task| task.course_remote_id.as_deref() == Some("course:100:200"))
        );
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.exam_calls.load(Ordering::Relaxed), 1);
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
        assert_eq!(transport.work_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.exam_calls.load(Ordering::Relaxed), 1);
        assert_eq!(transport.work_detail_calls.load(Ordering::Relaxed), 0);
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
