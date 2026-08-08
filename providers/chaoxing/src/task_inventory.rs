use std::{collections::BTreeSet, fmt, sync::Arc};

use asterism_domain::ProviderId;
use asterism_provider_api::{
    ProviderCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTask, TaskInventoryCapability,
    VerificationLevel,
};
use async_trait::async_trait;
use zeroize::Zeroize;

use crate::{ChaoxingCourseScope, parse_exam_inventory, parse_work_inventory};

const PROVIDER_ID: &str = "chaoxing";
const COURSE_ID_ROUTE_KEY: &str = "chaoxing.course_id";
const CLASS_ID_ROUTE_KEY: &str = "chaoxing.class_id";
const CPI_ROUTE_KEY: &str = "chaoxing.cpi";
const MAX_INVENTORY_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;

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

    fn as_str(&self) -> &str {
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
}

/// Development-level Chaoxing task inventory capability. It is intentionally
/// not registered by this crate until course discovery and a live transport are
/// both present.
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
            metadata: ProviderMetadata {
                id: ProviderId::new(PROVIDER_ID).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::Internal,
                        "Chaoxing compile-time Provider ID is invalid",
                    )
                })?,
                display_name: "Chaoxing".to_owned(),
                implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
                verification: VerificationLevel::Development,
                scan_min_interval_seconds: None,
                capture_recipe_version: None,
                capabilities: BTreeSet::from([ProviderCapability::TaskInventory]),
                auth_methods: BTreeSet::new(),
                session_kinds: BTreeSet::new(),
            },
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
        let mut tasks = parse_work_inventory(work.as_str(), &scope)?;
        tasks.extend(parse_exam_inventory(exam.as_str(), &scope)?);
        Ok(tasks)
    }
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

    use asterism_domain::{ProviderAccountId, SecretId, SourceType};
    use asterism_provider_api::ProviderRouteContext;

    use super::*;

    const EXAM_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/exam/list-mixed.html");
    const WORK_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/work/list-mixed.html");

    #[derive(Debug, Default)]
    struct FixtureTransport {
        work_calls: AtomicUsize,
        exam_calls: AtomicUsize,
        fail_exam: bool,
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
        assert_eq!(
            inventory.metadata().verification,
            VerificationLevel::Development
        );
        assert_eq!(
            inventory.metadata().capabilities,
            BTreeSet::from([ProviderCapability::TaskInventory])
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
    }

    #[test]
    fn route_and_document_debug_output_are_redacted() {
        let course = course();
        let route = ChaoxingCourseRoute::from_remote_course(&course).unwrap();
        assert!(!format!("{route:?}").contains("300"));
        let document = ChaoxingInventoryDocument::try_new("private page").unwrap();
        assert!(!format!("{document:?}").contains("private page"));
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new(PROVIDER_ID).unwrap(),
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
