use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    CourseInventoryCapability, ExecutionOutcome, ExecutionRequest, ProgressSink, ProviderContext,
    ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderProgress,
    ProviderResult, RemoteCourse, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingCourseRoute,
    ChaoxingInventoryTransport,
    metadata::development_metadata,
    parse_chapter_inventory,
    resource_inventory::{
        ChaoxingImmediateResourceKind, ChaoxingImmediateResourceTarget,
        locate_immediate_resource_target,
    },
    task_inventory::CHAPTER_RESOURCE_CARD_COUNT,
};

#[async_trait]
pub(crate) trait ChaoxingImmediateResourceTransport: Send + Sync {
    async fn complete_immediate_resource(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        knowledge_id: &str,
        target: &ChaoxingImmediateResourceTarget,
    ) -> ProviderResult<()>;
}

pub struct ChaoxingResourceExecution {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    inventory: Arc<dyn ChaoxingInventoryTransport>,
    immediate: Arc<dyn ChaoxingImmediateResourceTransport>,
}

impl ChaoxingResourceExecution {
    /// Creates the native immediate-resource execution capability.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the compile-time Provider metadata is
    /// invalid.
    pub(crate) fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        inventory: Arc<dyn ChaoxingInventoryTransport>,
        immediate: Arc<dyn ChaoxingImmediateResourceTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            courses,
            inventory,
            immediate,
        })
    }

    async fn resolve_course(
        &self,
        context: &ProviderContext,
        identity: &ResourceIdentity<'_>,
    ) -> ProviderResult<RemoteCourse> {
        let expected = format!("course:{}:{}", identity.course, identity.class);
        let mut matches = self
            .courses
            .list_courses(context)
            .await?
            .into_iter()
            .filter(|course| course.remote_id == expected);
        let course = matches.next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing resource course is no longer available",
            )
        })?;
        if matches.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing course discovery returned a duplicate execution scope",
            ));
        }
        Ok(course)
    }

    async fn resolve_resource_request(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        identity: &ResourceIdentity<'_>,
    ) -> ProviderResult<ChaoxingChapterResourceRequest> {
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
        let chapter = matching.next().ok_or_else(remote_resource_changed)?;
        if matching.next().is_some() {
            return Err(protocol_drift(
                "Chaoxing chapter inventory returned a duplicate execution scope",
            ));
        }
        let request = ChaoxingChapterResourceRequest::try_from_available_chapter(chapter)?
            .ok_or_else(remote_resource_changed)?;
        if !request.belongs_to(route) || request.knowledge_id() != identity.knowledge {
            return Err(protocol_drift(
                "Chaoxing chapter execution request lost its route binding",
            ));
        }
        Ok(request)
    }

    async fn fetch_target(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        remote_task_id: &str,
    ) -> ProviderResult<ChaoxingImmediateResourceTarget> {
        let documents = self
            .inventory
            .fetch_chapter_resource_inventories(context, route, std::slice::from_ref(request))
            .await?;
        locate_target(documents, route, request, remote_task_id)
    }
}

impl fmt::Debug for ChaoxingResourceExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingResourceExecution")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("inventory", &"configured")
            .field("immediate", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingResourceExecution {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskExecutionCapability for ChaoxingResourceExecution {
    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        progress: &(dyn ProgressSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if context.provider_id != self.metadata.id {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing resource execution received a mismatched Provider context",
            ));
        }
        if context.credential_refs.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Authentication,
                "Chaoxing resource execution requires an authenticated session",
            ));
        }
        if request.requested_capabilities != [TaskCapability::ResourceExecution] {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing immediate execution accepts only ResourceExecution",
            ));
        }
        let identity = ResourceIdentity::parse(&request.remote_task_id)?;
        let course = self.resolve_course(context, &identity).await?;
        let route = ChaoxingCourseRoute::from_remote_course(&course)?;
        let resource_request = self
            .resolve_resource_request(context, route, &identity)
            .await?;
        let target = self
            .fetch_target(context, route, &resource_request, &request.remote_task_id)
            .await?;
        if target.remote_state() == RemoteState::Completed {
            return Ok(completed_outcome(target.kind(), true));
        }

        progress
            .report(ProviderProgress {
                percent: Some(0),
                stage: "resource_execute".to_owned(),
                status_text: Some("正在提交资源完成请求".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        self.immediate
            .complete_immediate_resource(context, route, resource_request.knowledge_id(), &target)
            .await?;
        progress
            .report(ProviderProgress {
                percent: Some(90),
                stage: "resource_verify".to_owned(),
                status_text: Some("正在复核远端完成状态".to_owned()),
                completed_items: Some(0),
                total_items: Some(1),
            })
            .await?;
        let verified = self
            .fetch_target(context, route, &resource_request, &request.remote_task_id)
            .await?;
        if verified.kind() != target.kind() || verified.remote_state() != RemoteState::Completed {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing resource did not become completed after execution",
            ));
        }
        progress
            .report(ProviderProgress {
                percent: Some(100),
                stage: "resource_verified".to_owned(),
                status_text: Some("远端完成状态已复核".to_owned()),
                completed_items: Some(1),
                total_items: Some(1),
            })
            .await?;
        Ok(completed_outcome(target.kind(), false))
    }
}

fn locate_target(
    documents: Vec<ChaoxingChapterResourceDocument>,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<ChaoxingImmediateResourceTarget> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing immediate execution received an incomplete card set",
        ));
    }
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing immediate execution received a foreign or duplicate card",
            ));
        }
    }
    let scope = route.parser_scope()?;
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed
            .remove(&card_index)
            .ok_or_else(|| protocol_drift("Chaoxing immediate execution omitted a card"))?;
        let Some(target) = locate_immediate_resource_target(
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
                "Chaoxing immediate execution found the task on multiple cards",
            ));
        }
    }
    found.ok_or_else(remote_resource_changed)
}

struct ResourceIdentity<'a> {
    course: &'a str,
    class: &'a str,
    knowledge: &'a str,
}

impl<'a> ResourceIdentity<'a> {
    fn parse(remote_task_id: &'a str) -> ProviderResult<Self> {
        let components = remote_task_id.split(':').collect::<Vec<_>>();
        if components.len() != 5
            || components[0] != "resource"
            || !valid_component(components[1])
            || !valid_component(components[2])
            || !(1..=20).contains(&components[3].len())
            || !components[3].bytes().all(|byte| byte.is_ascii_digit())
            || !valid_component(components[4])
        {
            return Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "Chaoxing execution request is not a valid resource task",
            ));
        }
        Ok(Self {
            course: components[1],
            class: components[2],
            knowledge: components[3],
        })
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

const fn resource_kind_name(kind: ChaoxingImmediateResourceKind) -> &'static str {
    match kind {
        ChaoxingImmediateResourceKind::Document => "document",
        ChaoxingImmediateResourceKind::Read => "read",
    }
}

fn completed_outcome(
    kind: ChaoxingImmediateResourceKind,
    already_completed: bool,
) -> ExecutionOutcome {
    ExecutionOutcome {
        remote_state: RemoteState::Completed,
        verified: true,
        result_sanitized: serde_json::json!({
            "schema": "chaoxing.immediate-resource-result.v1",
            "resource_kind": resource_kind_name(kind),
            "already_completed": already_completed,
            "verification": "fresh_card_state",
        }),
    }
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

fn remote_resource_changed() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::RemoteChanged,
        "Chaoxing resource task is no longer available for execution",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use asterism_domain::{ProviderAccountId, ProviderId, SecretId, TaskId};
    use asterism_provider_api::{ProviderRouteContext, RemoteCourse};

    use super::*;
    use crate::{ChaoxingInventoryDocument, ChaoxingWorkDetailRequest, ChaoxingWorkDetailState};

    const CHAPTER_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/chapter/list-mixed.html");
    const RESOURCE_MIXED: &str =
        include_str!("../../../fixtures/providers/chaoxing/resources/cards-mixed.html");

    struct FixtureProvider {
        metadata: ProviderMetadata,
        completed_read: AtomicBool,
        resource_calls: AtomicUsize,
        execute_calls: AtomicUsize,
    }

    impl FixtureProvider {
        fn new(completed_read: bool) -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                completed_read: AtomicBool::new(completed_read),
                resource_calls: AtomicUsize::new(0),
                execute_calls: AtomicUsize::new(0),
            }
        }
    }

    impl fmt::Debug for FixtureProvider {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("FixtureProvider")
        }
    }

    impl ProviderIdentity for FixtureProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for FixtureProvider {
        async fn list_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            Ok(vec![course()])
        }
    }

    #[async_trait]
    impl ChaoxingInventoryTransport for FixtureProvider {
        async fn fetch_chapter_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            ChaoxingInventoryDocument::try_new(CHAPTER_MIXED)
        }

        async fn fetch_work_inventory(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
        ) -> ProviderResult<ChaoxingInventoryDocument> {
            Err(unsupported_fixture_call())
        }

        async fn fetch_chapter_resource_inventories(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            requests: &[ChaoxingChapterResourceRequest],
        ) -> ProviderResult<Vec<ChaoxingChapterResourceDocument>> {
            assert_eq!(requests.len(), 1);
            self.resource_calls.fetch_add(1, Ordering::Relaxed);
            let first = if self.completed_read.load(Ordering::Relaxed) {
                RESOURCE_MIXED.replace(
                    "\"jobid\":\"job-read\",\"isPassed\":false",
                    "\"jobid\":\"job-read\",\"isPassed\":true",
                )
            } else {
                RESOURCE_MIXED.to_owned()
            };
            (0..CHAPTER_RESOURCE_CARD_COUNT)
                .map(|card_index| {
                    ChaoxingChapterResourceDocument::for_request(
                        &requests[0],
                        card_index,
                        if card_index == 0 {
                            first.as_str()
                        } else {
                            "<html><body>empty card slot</body></html>"
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
        ) -> ProviderResult<Vec<ChaoxingWorkDetailState>> {
            Err(unsupported_fixture_call())
        }
    }

    #[async_trait]
    impl ChaoxingImmediateResourceTransport for FixtureProvider {
        async fn complete_immediate_resource(
            &self,
            _context: &ProviderContext,
            _route: ChaoxingCourseRoute<'_>,
            knowledge_id: &str,
            target: &ChaoxingImmediateResourceTarget,
        ) -> ProviderResult<()> {
            assert_eq!(knowledge_id, "4001");
            assert_eq!(target.kind(), ChaoxingImmediateResourceKind::Read);
            assert_eq!(
                target.token().unwrap().expose_secret(),
                "PRIVATE_READ_TOKEN"
            );
            self.execute_calls.fetch_add(1, Ordering::Relaxed);
            self.completed_read.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingProgress(AtomicUsize);

    #[async_trait]
    impl ProgressSink for RecordingProgress {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[tokio::test]
    async fn immediate_execution_refetches_and_verifies_the_remote_card() {
        let fixture = Arc::new(FixtureProvider::new(false));
        let execution =
            ChaoxingResourceExecution::try_new(fixture.clone(), fixture.clone(), fixture.clone())
                .unwrap();
        let progress = RecordingProgress::default();
        let outcome = execution
            .execute(
                &context(),
                &execution_request("resource:100:200:4001:job-read"),
                &progress,
            )
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert!(outcome.verified);
        assert_eq!(outcome.result_sanitized["resource_kind"], "read");
        assert_eq!(fixture.resource_calls.load(Ordering::Relaxed), 2);
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 1);
        assert_eq!(progress.0.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn completed_document_is_idempotent_and_unsupported_video_fails_closed() {
        let fixture = Arc::new(FixtureProvider::new(false));
        let execution =
            ChaoxingResourceExecution::try_new(fixture.clone(), fixture.clone(), fixture.clone())
                .unwrap();
        let progress = RecordingProgress::default();
        let completed = execution
            .execute(
                &context(),
                &execution_request("resource:100:200:4001:job-document"),
                &progress,
            )
            .await
            .unwrap();
        assert_eq!(completed.result_sanitized["already_completed"], true);
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 0);

        let error = execution
            .execute(
                &context(),
                &execution_request("resource:100:200:4001:job-video"),
                &progress,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::UnsupportedTask);
        assert_eq!(fixture.execute_calls.load(Ordering::Relaxed), 0);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-resource-execution-test".to_owned(),
        }
    }

    fn execution_request(remote_task_id: &str) -> ExecutionRequest {
        ExecutionRequest {
            task_id: TaskId::new(),
            remote_task_id: remote_task_id.to_owned(),
            course_id: None,
            requested_capabilities: vec![TaskCapability::ResourceExecution],
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
                ("chaoxing.course_id".to_owned(), "100".to_owned()),
                ("chaoxing.class_id".to_owned(), "200".to_owned()),
                ("chaoxing.cpi".to_owned(), "300".to_owned()),
            ])
            .unwrap(),
        }
    }

    fn unsupported_fixture_call() -> ProviderError {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "unexpected fixture transport call",
        )
    }
}
