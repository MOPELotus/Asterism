use std::{fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTask, TaskInventoryCapability,
};
use async_trait::async_trait;
use zeroize::Zeroize;

use crate::{
    WellearnScoLeavesDocument, metadata::development_metadata, parse_course_inventory,
    parse_task_inventory,
};

const MAX_INVENTORY_DOCUMENT_BYTES: usize = 4 * 1_024 * 1_024;

/// One bounded inventory response whose body is redacted and zeroized.
pub struct WellearnInventoryDocument(String);

impl WellearnInventoryDocument {
    /// Wraps a completed Course or Unit inventory response.
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
                "WELearn inventory document is empty or exceeds the size limit",
            ));
        }
        Ok(Self(document))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WellearnInventoryDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WellearnInventoryDocument([REDACTED])")
    }
}

impl Drop for WellearnInventoryDocument {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Complete Unit and SCO response set for one Course scan.
#[derive(Debug)]
pub struct WellearnTaskInventoryDocuments {
    units: WellearnInventoryDocument,
    leaves: Vec<WellearnScoLeavesDocument>,
}

impl WellearnTaskInventoryDocuments {
    /// Binds the Unit response and all Unit-indexed SCO responses into one
    /// all-or-nothing transport result.
    pub const fn new(
        units: WellearnInventoryDocument,
        leaves: Vec<WellearnScoLeavesDocument>,
    ) -> Self {
        Self { units, leaves }
    }
}

/// Transport boundary for the authenticated Course-list read.
#[async_trait]
pub trait WellearnCourseInventoryTransport: Send + Sync {
    async fn fetch_courses(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<WellearnInventoryDocument>;
}

/// Transport boundary for the Course page context, Units and every Unit's SCO
/// leaves. It must return no partial scan on failure.
#[async_trait]
pub trait WellearnTaskInventoryTransport: Send + Sync {
    async fn fetch_tasks(
        &self,
        context: &ProviderContext,
        course: &RemoteCourse,
    ) -> ProviderResult<WellearnTaskInventoryDocuments>;
}

/// Development-level `WELearn` `CourseInventory` capability.
pub struct WellearnCourseInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn WellearnCourseInventoryTransport>,
}

impl WellearnCourseInventory {
    /// Creates the capability around an authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn WellearnCourseInventoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for WellearnCourseInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnCourseInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnCourseInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl CourseInventoryCapability for WellearnCourseInventory {
    async fn list_courses(&self, context: &ProviderContext) -> ProviderResult<Vec<RemoteCourse>> {
        validate_context(context, &self.metadata)?;
        let document = self.transport.fetch_courses(context).await?;
        parse_course_inventory(document.as_str())
    }
}

/// Development-level `WELearn` `TaskInventory` capability.
pub struct WellearnTaskInventory {
    metadata: ProviderMetadata,
    transport: Arc<dyn WellearnTaskInventoryTransport>,
}

impl WellearnTaskInventory {
    /// Creates the capability around an authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn WellearnTaskInventoryTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for WellearnTaskInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnTaskInventory")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnTaskInventory {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskInventoryCapability for WellearnTaskInventory {
    async fn list_tasks(
        &self,
        context: &ProviderContext,
        course: Option<&RemoteCourse>,
    ) -> ProviderResult<Vec<RemoteTask>> {
        validate_context(context, &self.metadata)?;
        let course = course.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "WELearn Task inventory requires an explicit Course scope",
            )
        })?;
        let documents = self.transport.fetch_tasks(context, course).await?;
        parse_task_inventory(course, documents.units.as_str(), &documents.leaves)
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "WELearn inventory received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "WELearn inventory requires an authenticated session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId};

    use super::*;

    const COURSES: &str =
        include_str!("../../../fixtures/providers/welearn/courses/list-mixed.json");
    const UNITS: &str = include_str!("../../../fixtures/providers/welearn/units/list-mixed.json");
    const UNIT_ZERO: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-0.json");
    const UNIT_ONE: &str =
        include_str!("../../../fixtures/providers/welearn/tasks/leaves-unit-1.json");

    #[derive(Debug)]
    struct FixtureCourseTransport;

    #[async_trait]
    impl WellearnCourseInventoryTransport for FixtureCourseTransport {
        async fn fetch_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnInventoryDocument> {
            WellearnInventoryDocument::try_new(COURSES)
        }
    }

    #[derive(Debug)]
    struct FixtureTaskTransport {
        calls: AtomicUsize,
        complete: bool,
    }

    #[async_trait]
    impl WellearnTaskInventoryTransport for FixtureTaskTransport {
        async fn fetch_tasks(
            &self,
            _context: &ProviderContext,
            _course: &RemoteCourse,
        ) -> ProviderResult<WellearnTaskInventoryDocuments> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut leaves = vec![WellearnScoLeavesDocument::try_new(0, UNIT_ZERO)?];
            if self.complete {
                leaves.push(WellearnScoLeavesDocument::try_new(1, UNIT_ONE)?);
            }
            Ok(WellearnTaskInventoryDocuments::new(
                WellearnInventoryDocument::try_new(UNITS)?,
                leaves,
            ))
        }
    }

    #[tokio::test]
    async fn capabilities_parse_one_complete_authenticated_scan() {
        let courses_capability =
            WellearnCourseInventory::try_new(Arc::new(FixtureCourseTransport)).unwrap();
        let courses = courses_capability.list_courses(&context()).await.unwrap();
        assert_eq!(courses.len(), 2);

        let transport = Arc::new(FixtureTaskTransport {
            calls: AtomicUsize::new(0),
            complete: true,
        });
        let tasks_capability = WellearnTaskInventory::try_new(transport.clone()).unwrap();
        let tasks = tasks_capability
            .list_tasks(&context(), Some(&courses[0]))
            .await
            .unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            tasks_capability.metadata(),
            &development_metadata().unwrap()
        );
    }

    #[tokio::test]
    async fn incomplete_or_unscoped_task_scans_fail_closed() {
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let transport = Arc::new(FixtureTaskTransport {
            calls: AtomicUsize::new(0),
            complete: false,
        });
        let capability = WellearnTaskInventory::try_new(transport.clone()).unwrap();
        assert!(
            capability
                .list_tasks(&context(), Some(course))
                .await
                .is_err()
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert!(capability.list_tasks(&context(), None).await.is_err());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn context_is_rejected_before_transport() {
        let transport = Arc::new(FixtureTaskTransport {
            calls: AtomicUsize::new(0),
            complete: true,
        });
        let capability = WellearnTaskInventory::try_new(transport.clone()).unwrap();
        let course = &parse_course_inventory(COURSES).unwrap()[0];
        let mut invalid = context();
        invalid.credential_refs.clear();
        assert!(capability.list_tasks(&invalid, Some(course)).await.is_err());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "inventory-correlation".to_owned(),
        }
    }
}
