use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTask, TaskInventoryCapability,
};
use async_trait::async_trait;
use zeroize::Zeroize;

use crate::{
    UaiCourseProgressDocument, UaiProgressDocument,
    metadata::development_metadata,
    parse_course_context, parse_course_inventory, parse_task_inventory,
    task_inventory::{enrich_task_inventory, parse_task_tree_unit_ids},
};

const MAX_INVENTORY_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;

/// One bounded Course inventory response whose body is redacted and zeroized.
pub struct UaiInventoryDocument(String);

impl UaiInventoryDocument {
    /// Wraps one complete authenticated Course-list response.
    ///
    /// # Errors
    ///
    /// Returns an invalid-response error for an empty or oversized document.
    pub fn try_new(document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if document.is_empty() || document.len() > MAX_INVENTORY_DOCUMENT_BYTES {
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "UAI Course inventory document is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UaiInventoryDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UaiInventoryDocument([REDACTED])")
    }
}

impl Drop for UaiInventoryDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Transport boundary for one authenticated Course-list read.
#[async_trait]
pub trait UaiCourseInventoryTransport: Send + Sync {
    async fn fetch_courses(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<UaiInventoryDocument>;
}

/// Complete fresh resource-detail and nested tree response set for one Course.
#[derive(Debug)]
pub struct UaiTaskInventoryDocuments {
    detail: UaiInventoryDocument,
    tree: UaiInventoryDocument,
    course_progress: Option<UaiCourseProgressDocument>,
    progress_by_unit: BTreeMap<String, UaiProgressDocument>,
}

impl UaiTaskInventoryDocuments {
    /// Binds the two completed responses into one all-or-nothing transport
    /// result.
    pub const fn new(detail: UaiInventoryDocument, tree: UaiInventoryDocument) -> Self {
        Self {
            detail,
            tree,
            course_progress: None,
            progress_by_unit: BTreeMap::new(),
        }
    }

    /// Adds the independent current Course progress document used to bind the
    /// exact Unit set and Course publish version.
    #[must_use]
    pub fn with_course_progress(mut self, course_progress: UaiCourseProgressDocument) -> Self {
        self.course_progress = Some(course_progress);
        self
    }

    /// Adds one complete fresh progress document for every Unit represented by
    /// the tree. Native inventory uses this to retain donor task strategies.
    #[must_use]
    pub fn with_unit_progress(
        mut self,
        progress_by_unit: BTreeMap<String, UaiProgressDocument>,
    ) -> Self {
        self.progress_by_unit = progress_by_unit;
        self
    }
}

/// Transport boundary for a fresh Course-resource detail and its nested tree.
#[async_trait]
pub trait UaiTaskInventoryTransport: Send + Sync {
    async fn fetch_tasks(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<UaiTaskInventoryDocuments>;
}

/// Development-level UAI `CourseInventory` capability.
pub struct UaiCourseInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn UaiCourseInventoryTransport>,
}

impl UaiCourseInventory {
    /// Creates the capability around one authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn UaiCourseInventoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for UaiCourseInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiCourseInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiCourseInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl CourseInventoryCapability for UaiCourseInventory {
    async fn list_courses(&self, context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>> {
        validate_context(context, &self.metadata)?;
        let document = self.transport.fetch_courses(context).await?;
        parse_course_inventory(document.as_str())
    }
}

/// Development-level UAI `TaskInventory` capability.
pub struct UaiTaskInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn UaiTaskInventoryTransport>,
}

impl UaiTaskInventory {
    /// Creates the capability around one authenticated all-or-nothing
    /// transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn UaiTaskInventoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for UaiTaskInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiTaskInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiTaskInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskInventoryCapability for UaiTaskInventory {
    async fn list_tasks(
        &self,
        context: &ProviderContext,
        course: Option<&RemoteCourse>,
    ) -> ProviderResult<Vec<RemoteTask>> {
        validate_context(context, &self.metadata)?;
        let course = course.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI Task inventory requires an explicit Course scope",
            )
        })?;
        let documents = self.transport.fetch_tasks(context, course).await?;
        let route = parse_course_context(course, documents.detail.as_str())?;
        let tree_units = parse_task_tree_unit_ids(documents.tree.as_str())?;
        let tasks = parse_task_inventory(course, &route, documents.tree.as_str())?;
        enrich_task_inventory(
            tasks,
            &tree_units,
            documents.course_progress.as_ref(),
            &documents.progress_by_unit,
        )
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI Course inventory received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI Course inventory requires an authenticated session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    const COURSES: &str = include_str!("../../../fixtures/providers/uai/courses/list-mixed.json");

    #[derive(Debug)]
    struct FixtureTransport;

    #[async_trait]
    impl UaiCourseInventoryTransport for FixtureTransport {
        async fn fetch_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<UaiInventoryDocument> {
            UaiInventoryDocument::try_new(COURSES)
        }
    }

    #[async_trait]
    impl UaiTaskInventoryTransport for FixtureTransport {
        async fn fetch_tasks(
            &self,
            _context: &ProviderContext,
            _course: &RemoteCourse,
        ) -> ProviderResult<UaiTaskInventoryDocuments> {
            Ok(UaiTaskInventoryDocuments::new(
                UaiInventoryDocument::try_new(include_str!(
                    "../../../fixtures/providers/uai/courses/resource-detail.json"
                ))?,
                UaiInventoryDocument::try_new(include_str!(
                    "../../../fixtures/providers/uai/tasks/tree-mixed.json"
                ))?,
            )
            .with_course_progress(UaiCourseProgressDocument::try_new(include_str!(
                "../../../fixtures/providers/uai/progress/course-mixed.json"
            ))?)
            .with_unit_progress(BTreeMap::from([(
                "unit-1".to_owned(),
                UaiProgressDocument::try_new(include_str!(
                    "../../../fixtures/providers/uai/progress/unit-mixed.json"
                ))?,
            )])))
        }
    }

    #[tokio::test]
    async fn capability_reads_one_complete_bound_inventory() {
        let capability = UaiCourseInventory::try_new(Arc::new(FixtureTransport)).unwrap();
        let courses = capability.list_courses(&provider_context()).await.unwrap();
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].remote_id, "course-resource:2001");
        assert!(format!("{capability:?}").contains("configured"));
    }

    #[tokio::test]
    async fn context_is_rejected_before_transport() {
        let capability = UaiCourseInventory::try_new(Arc::new(FixtureTransport)).unwrap();
        let mut context = provider_context();
        context.provider_id = ProviderId::new("other").unwrap();
        assert_eq!(
            capability.list_courses(&context).await.unwrap_err().kind,
            ProviderErrorKind::Internal
        );

        let mut context = provider_context();
        context.credential_refs.clear();
        assert_eq!(
            capability.list_courses(&context).await.unwrap_err().kind,
            ProviderErrorKind::Authentication
        );
    }

    #[tokio::test]
    async fn task_capability_binds_fresh_detail_before_parsing_tree() {
        let course = parse_course_inventory(COURSES).unwrap().remove(0);
        let capability = UaiTaskInventory::try_new(Arc::new(FixtureTransport)).unwrap();
        let tasks = capability
            .list_tasks(&provider_context(), Some(&course))
            .await
            .unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(
            tasks
                .iter()
                .all(|task| { task.remote_id.starts_with("group:2001:unit-1:group-") })
        );
        assert!(format!("{capability:?}").contains("configured"));
        assert!(tasks[0].opens_at.is_some());
        assert!(tasks[0].closes_at.is_some());
        assert_eq!(tasks[0].normalized["strategy"]["required"], true);
        assert_eq!(tasks[0].normalized["strategy"]["min_score_percent"], 60);
        assert_eq!(
            tasks[0].normalized["strategy"]["opens_at"],
            "2026-08-01T00:00:00Z"
        );
        assert_eq!(tasks[0].normalized["course_publish_version"], 123_290);
        assert_eq!(
            tasks[0].normalized["course_unit_strategy"]["required"],
            true
        );
        assert_eq!(
            tasks[0].normalized["course_unit_strategy"]["min_score_percent"],
            60
        );
        assert_eq!(tasks[1].normalized["strategy"]["required"], false);
        assert!(tasks[1].opens_at.is_none());
        assert!(tasks[1].closes_at.is_none());

        assert_eq!(
            capability
                .list_tasks(&provider_context(), None)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::ProtocolDrift
        );
    }

    #[test]
    fn inventory_documents_are_bounded_and_redacted() {
        let document = UaiInventoryDocument::try_new(COURSES).unwrap();
        assert!(!format!("{document:?}").contains("综合英语"));
        assert!(UaiInventoryDocument::try_new("").is_err());
        assert!(
            UaiInventoryDocument::try_new("x".repeat(MAX_INVENTORY_DOCUMENT_BYTES + 1)).is_err()
        );
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-course-test".to_owned(),
        }
    }
}
