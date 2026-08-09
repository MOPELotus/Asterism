use std::{fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse,
};
use async_trait::async_trait;
use zeroize::Zeroize;

use crate::{metadata::development_metadata, parse_course_inventory};

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
