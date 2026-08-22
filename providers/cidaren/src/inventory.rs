use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteCourse, RemoteTask, TaskInventoryCapability,
};
use async_trait::async_trait;
use zeroize::Zeroize;

use crate::{
    class_tasks::{parse_course_inventory, parse_task_inventory},
    metadata::development_metadata,
    study_tasks::{parse_study_course, parse_study_task_inventory},
};

const MAX_PAGE_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_PAGE_NUMBER: u32 = 1_000;
const MAX_STUDY_DOCUMENT_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_SELECTED_COURSE_ID_BYTES: usize = 256;

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

/// One bounded `StudyTask/List` response bound to the selected Course used in
/// its request.
pub struct CidarenStudyTaskDocument {
    selected_course_id: String,
    document: String,
}

impl CidarenStudyTaskDocument {
    /// Binds a complete response body to one safe selected Course identity.
    ///
    /// # Errors
    ///
    /// Returns `InvalidResponse` for an unsafe Course identity or an
    /// empty/oversized response body.
    pub fn try_new(
        selected_course_id: impl Into<String>,
        document: impl Into<String>,
    ) -> ProviderResult<Self> {
        let mut selected_course_id = selected_course_id.into();
        let mut document = document.into();
        if !valid_remote_component(&selected_course_id)
            || document.is_empty()
            || document.len() > MAX_STUDY_DOCUMENT_BYTES
        {
            selected_course_id.zeroize();
            document.zeroize();
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidResponse,
                "Cidaren study-task response is invalid or exceeds the size limit",
            ));
        }
        Ok(Self {
            selected_course_id,
            document,
        })
    }

    pub(crate) fn selected_course_id(&self) -> &str {
        &self.selected_course_id
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.document
    }
}

impl fmt::Debug for CidarenStudyTaskDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenStudyTaskDocument")
            .field("selected_course_id", &self.selected_course_id)
            .field("document", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CidarenStudyTaskDocument {
    fn drop(&mut self) {
        self.selected_course_id.zeroize();
        self.document.zeroize();
    }
}

/// Transport boundary for the currently selected ordinary self-study Course
/// and unit list.
#[async_trait]
pub trait CidarenStudyTaskTransport: Send + Sync {
    async fn fetch_study_task_document(
        &self,
        context: &ProviderContext,
    ) -> ProviderResult<CidarenStudyTaskDocument>;
}

/// Development-level Course inventory merged from class and selected study
/// routes.
pub struct CidarenCourseInventory {
    metadata: ProviderMetadata,
    class_tasks: Arc<dyn CidarenClassTaskTransport>,
    study_tasks: Arc<dyn CidarenStudyTaskTransport>,
}

impl CidarenCourseInventory {
    /// Creates the capability around an injected authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        class_tasks: Arc<dyn CidarenClassTaskTransport>,
        study_tasks: Arc<dyn CidarenStudyTaskTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            class_tasks,
            study_tasks,
        })
    }
}

impl fmt::Debug for CidarenCourseInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenCourseInventory")
            .field("metadata", &self.metadata)
            .field("class_tasks", &"configured")
            .field("study_tasks", &"configured")
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
        let pages = self.class_tasks.fetch_class_task_pages(context).await?;
        let study = self.study_tasks.fetch_study_task_document(context).await?;
        merge_courses(parse_course_inventory(&pages)?, parse_study_course(&study)?)
    }
}

/// Development-level class learning/test and ordinary study Task inventory.
pub struct CidarenTaskInventory {
    metadata: ProviderMetadata,
    class_tasks: Arc<dyn CidarenClassTaskTransport>,
    study_tasks: Arc<dyn CidarenStudyTaskTransport>,
}

impl CidarenTaskInventory {
    /// Creates the capability around an injected authenticated transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        class_tasks: Arc<dyn CidarenClassTaskTransport>,
        study_tasks: Arc<dyn CidarenStudyTaskTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            class_tasks,
            study_tasks,
        })
    }
}

impl fmt::Debug for CidarenTaskInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenTaskInventory")
            .field("metadata", &self.metadata)
            .field("class_tasks", &"configured")
            .field("study_tasks", &"configured")
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
        let pages = self.class_tasks.fetch_class_task_pages(context).await?;
        let study = self.study_tasks.fetch_study_task_document(context).await?;
        let mut tasks = parse_task_inventory(course, &pages)?;
        tasks.extend(parse_study_task_inventory(course, &study)?);
        let mut unique = BTreeMap::new();
        for task in tasks {
            if unique.insert(task.remote_id.clone(), task).is_some() {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "Cidaren inventory contains a duplicate Task identity",
                ));
            }
        }
        Ok(unique.into_values().collect())
    }
}

fn merge_courses(
    class_courses: Vec<RemoteCourse>,
    study_course: RemoteCourse,
) -> ProviderResult<Vec<RemoteCourse>> {
    let mut courses = class_courses
        .into_iter()
        .map(|course| (course.remote_id.clone(), course))
        .collect::<BTreeMap<_, _>>();
    if let Some(existing) = courses.get_mut(&study_course.remote_id) {
        if existing.title != study_course.title {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Cidaren class and study inventories disagree on the Course title",
            ));
        }
        let course_id = existing
            .remote_id
            .strip_prefix("course:")
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "Cidaren merged Course identity is invalid",
                )
            })?
            .to_owned();
        existing.remote_status = study_course.remote_status.clone();
        existing.metadata_sanitized = serde_json::json!({
            "schema": "cidaren.course.v1",
            "course_id": course_id,
            "inventory_sources": ["class-task", "study-task"],
            "class_task": existing.metadata_sanitized,
            "study_task": study_course.metadata_sanitized,
        });
    } else {
        courses.insert(study_course.remote_id.clone(), study_course);
    }
    Ok(courses.into_values().collect())
}

fn valid_remote_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SELECTED_COURSE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
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

    #[async_trait]
    impl CidarenStudyTaskTransport for FixtureTransport {
        async fn fetch_study_task_document(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenStudyTaskDocument> {
            fixture_study_document()
        }
    }

    #[tokio::test]
    async fn capabilities_read_complete_pages_and_filter_course() {
        let transport = Arc::new(FixtureTransport);
        let courses = CidarenCourseInventory::try_new(transport.clone(), transport.clone())
            .unwrap()
            .list_courses(&provider_context())
            .await
            .unwrap();
        assert_eq!(courses.len(), 2);
        let selected = courses
            .iter()
            .find(|course| course.remote_id == "course:course-a")
            .unwrap();
        assert_eq!(selected.remote_status.as_deref(), Some("35%"));

        let tasks = CidarenTaskInventory::try_new(transport.clone(), transport)
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
        let transport = Arc::new(FixtureTransport);
        let capability = CidarenCourseInventory::try_new(transport.clone(), transport).unwrap();
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

        let study = fixture_study_document().unwrap();
        assert!(!format!("{study:?}").contains("Synthetic List"));
        assert!(CidarenStudyTaskDocument::try_new("unsafe/course", "{}").is_err());
        assert!(CidarenStudyTaskDocument::try_new("course-a", "").is_err());
        assert!(
            CidarenStudyTaskDocument::try_new(
                "course-a",
                "x".repeat(MAX_STUDY_DOCUMENT_BYTES + 1),
            )
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

    fn fixture_study_document() -> ProviderResult<CidarenStudyTaskDocument> {
        CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
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
