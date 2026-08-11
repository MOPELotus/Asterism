use std::{fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTask, TaskInventoryCapability,
};
use async_trait::async_trait;
use zeroize::Zeroize;

use crate::{
    class_tasks::{parse_course_inventory, parse_task_inventory},
    metadata::development_metadata,
};

const MAX_PAGE_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_PAGE_NUMBER: u32 = 1_000;

/// One bounded class-task page response bound to its one-based requested page.
pub struct CidarenClassTaskPageDocument {
    page_count: u32,
    document: String,
}

impl CidarenClassTaskPageDocument {
    /// Binds a complete response body to the requested page number.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for an invalid page number or empty/oversized
    /// response body.
    pub fn try_new(page_count: u32, document: impl Into<String>) -> ProviderResult<Self> {
        let mut document = document.into();
        if page_count == 0
            || page_count > MAX_PAGE_NUMBER
            || document.is_empty()
            || document.len() > MAX_PAGE_DOCUMENT_BYTES
        {
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Cidaren class-task page is invalid or exceeds the size limit",
            ));
        }
        Ok(Self {
            page_count,
            document,
        })
    }

    pub(crate) const fn page_count(&self) -> u32 {
        self.page_count
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.document
    }
}

impl fmt::Debug for CidarenClassTaskPageDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenClassTaskPageDocument")
            .field("page_count", &self.page_count)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenClassTaskPageDocument {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}

/// Transport boundary for one complete authenticated class-task pagination.
#[async_trait]
pub trait CidarenClassTaskTransport: Send + Sync {
    async fn fetch_class_task_pages(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<Vec<CidarenClassTaskPageDocument>>;
}

/// Development-level Course inventory derived from complete class-task pages.
pub struct CidarenCourseInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn CidarenClassTaskTransport>,
}

impl CidarenCourseInventory {
    /// Creates the capability around an injected authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn CidarenClassTaskTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for CidarenCourseInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenCourseInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenCourseInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl CourseInventoryCapability for CidarenCourseInventory {
    async fn list_courses(&self, context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>> {
        validate_context(context, &self.metadata)?;
        let pages = self.transport.fetch_class_task_pages(context).await?;
        parse_course_inventory(&pages)
    }
}

/// Development-level learning/test Task inventory.
pub struct CidarenTaskInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn CidarenClassTaskTransport>,
}

impl CidarenTaskInventory {
    /// Creates the capability around an injected authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn CidarenClassTaskTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for CidarenTaskInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenTaskInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenTaskInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskInventoryCapability for CidarenTaskInventory {
    async fn list_tasks(
        &self,
        context: &ProviderContext,
        course: Option<&RemoteCourse>,
    ) -> ProviderResult<Vec<RemoteTask>> {
        validate_context(context, &self.metadata)?;
        let pages = self.transport.fetch_class_task_pages(context).await?;
        parse_task_inventory(course, &pages)
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren inventory received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren inventory requires an authenticated session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    #[derive(Debug)]
    struct FixtureTransport;

    #[async_trait]
    impl CidarenClassTaskTransport for FixtureTransport {
        async fn fetch_class_task_pages(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<CidarenClassTaskPageDocument>> {
            fixture_pages()
        }
    }

    #[tokio::test]
    async fn capabilities_read_complete_pages_and_filter_course() {
        let transport = Arc::new(FixtureTransport);
        let courses = CidarenCourseInventory::try_new(transport.clone())
            .unwrap()
            .list_courses(&provider_context())
            .await
            .unwrap();
        assert_eq!(courses.len(), 2);

        let tasks = CidarenTaskInventory::try_new(transport)
            .unwrap()
            .list_tasks(&provider_context(), Some(&courses[0]))
            .await
            .unwrap();
        assert!(!tasks.is_empty());
        assert!(
            tasks
                .iter()
                .all(|task| task.course_remote_id.as_ref() == Some(&courses[0].remote_id))
        );
    }

    #[tokio::test]
    async fn context_is_rejected_before_transport() {
        let capability = CidarenCourseInventory::try_new(Arc::new(FixtureTransport)).unwrap();
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

    #[test]
    fn page_documents_are_bounded_and_redacted() {
        let page = fixture_pages().unwrap().remove(0);
        assert!(!format!("{page:?}").contains("Synthetic Course"));
        assert!(CidarenClassTaskPageDocument::try_new(0, "{}").is_err());
        assert!(CidarenClassTaskPageDocument::try_new(1, "").is_err());
        assert!(
            CidarenClassTaskPageDocument::try_new(1, "x".repeat(MAX_PAGE_DOCUMENT_BYTES + 1))
                .is_err()
        );
    }

    fn fixture_pages() -> ProviderResult<Vec<CidarenClassTaskPageDocument>> {
        Ok(vec![
            CidarenClassTaskPageDocument::try_new(
                1,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-1.json"),
            )?,
            CidarenClassTaskPageDocument::try_new(
                2,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-2.json"),
            )?,
        ])
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-inventory-test".to_owned(),
        }
    }
}
