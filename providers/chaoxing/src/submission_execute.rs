use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_domain::{RemoteState, SubmissionDraft, SubmissionReceipt};
use asterism_provider_api::{
    CourseInventoryCapability, ExecutionEventSink, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderResult,
    ProviderRuntimeSettingsSchema, ResolvedProviderRuntimeSettings, SubmissionBuildCapability,
    SubmissionExecuteCapability,
};
use async_trait::async_trait;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingChapterWorkTarget,
    ChaoxingCourseRoute, ChaoxingInventoryTransport, ChaoxingSubmissionBuild,
    ChaoxingSubmissionPlan, ChaoxingWorkDetailRequest,
    inventory::parse_work_inventory_entries,
    metadata::development_metadata,
    parse_chapter_inventory,
    resource_inventory::locate_chapter_work_target,
    runtime_settings::runtime_settings_schema,
    submission_support::{SubmissionModule, WorkSubmissionIdentity},
    task_inventory::CHAPTER_RESOURCE_CARD_COUNT,
};

/// Mutation boundary for exactly one Chaoxing Work submission attempt.
/// Implementations must not retry a POST after any ambiguous transport result.
#[async_trait]
pub trait ChaoxingSubmissionTransport: Send + Sync {
    async fn submit_work(
        &self,
        context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt>;

    async fn submit_chapter_work(
        &self,
        context: &ProviderContext,
        route: ChaoxingCourseRoute<'_>,
        request: &ChaoxingChapterResourceRequest,
        target: &ChaoxingChapterWorkTarget,
        plan: &ChaoxingSubmissionPlan,
    ) -> ProviderResult<SubmissionReceipt>;
}

/// Independent Chaoxing Work mutation. It returns only an acknowledgement;
/// completion remains exclusively owned by `SubmissionVerify`.
pub struct ChaoxingSubmissionExecute {
    metadata: ProviderMetadata,
    runtime_settings: ProviderRuntimeSettingsSchema,
    courses: Arc<dyn CourseInventoryCapability>,
    inventory: Arc<dyn ChaoxingInventoryTransport>,
    preview: ChaoxingSubmissionBuild,
    transport: Arc<dyn ChaoxingSubmissionTransport>,
}

impl ChaoxingSubmissionExecute {
    /// Builds the capability around fresh Course/Work discovery and one
    /// non-retrying mutation transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        inventory: Arc<dyn ChaoxingInventoryTransport>,
        transport: Arc<dyn ChaoxingSubmissionTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            runtime_settings: runtime_settings_schema(),
            courses,
            inventory,
            preview: ChaoxingSubmissionBuild::try_new()?,
            transport,
        })
    }
}

impl fmt::Debug for ChaoxingSubmissionExecute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSubmissionExecute")
            .field("metadata", &self.metadata)
            .field("runtime_settings", &self.runtime_settings)
            .field("courses", &"configured")
            .field("inventory", &"configured")
            .field("preview", &self.preview)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingSubmissionExecute {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionExecuteCapability for ChaoxingSubmissionExecute {
    async fn execute_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        runtime_settings: &ResolvedProviderRuntimeSettings,
        _events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<SubmissionReceipt> {
        validate_context(context, &self.metadata)?;
        self.runtime_settings
            .validate_resolved(runtime_settings)
            .map_err(|_| internal("Chaoxing submission settings snapshot is invalid"))?;
        let identity = WorkSubmissionIdentity::parse(remote_task_id)?;
        validate_draft(draft, &self.metadata)?;
        let questions = draft
            .items
            .iter()
            .map(|item| item.question.clone())
            .collect::<Vec<_>>();
        let selected = draft
            .items
            .iter()
            .map(|item| item.selected.clone())
            .collect::<Vec<_>>();
        let expected_preview = self
            .preview
            .build_submission_preview(context, remote_task_id, &questions, &selected)
            .await?;
        if expected_preview != draft.payload_preview {
            return Err(invalid_response(
                "Chaoxing submission draft preview is stale or foreign",
            ));
        }
        let plan = ChaoxingSubmissionPlan::from_draft(draft)?;

        let courses = self.courses.list_courses(context).await?;
        let course = matching_course(&courses, identity)?;
        let route = ChaoxingCourseRoute::from_remote_course(course)?;
        let receipt = match identity.module() {
            SubmissionModule::IndependentWork => {
                let document = self.inventory.fetch_work_inventory(context, route).await?;
                let entries =
                    parse_work_inventory_entries(document.as_str(), &route.parser_scope()?)?;
                let entry = matching_work_entry(&entries, identity)?;
                if !matches!(
                    entry.task().remote_state,
                    RemoteState::Pending | RemoteState::InProgress
                ) {
                    return Err(remote_changed(
                        "Chaoxing Work is no longer pending before submission",
                    ));
                }
                let request =
                    ChaoxingWorkDetailRequest::try_new(route, remote_task_id, entry.entry())?;
                self.transport.submit_work(context, request, &plan).await
            }
            SubmissionModule::ChapterWork => {
                let (request, target) =
                    resolve_chapter_work_target(self.inventory.as_ref(), context, route, identity)
                        .await?;
                self.transport
                    .submit_chapter_work(context, route, &request, &target, &plan)
                    .await
            }
        }?;
        receipt
            .validate()
            .map_err(|_| invalid_response("Chaoxing submission receipt is invalid"))?;
        Ok(receipt)
    }
}

pub(crate) fn matching_course<'a>(
    courses: &'a [asterism_provider_api::RemoteCourse],
    identity: WorkSubmissionIdentity<'_>,
) -> ProviderResult<&'a asterism_provider_api::RemoteCourse> {
    let mut matching = courses.iter().filter(|course| {
        ChaoxingCourseRoute::from_remote_course(course).is_ok_and(|route| {
            route.course_id() == identity.course_id() && route.class_id() == identity.class_id()
        })
    });
    let course = matching.next().ok_or_else(|| {
        remote_changed("Chaoxing Work course is no longer present in fresh inventory")
    })?;
    if matching.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing fresh inventory contains duplicate course identity",
        ));
    }
    Ok(course)
}

fn matching_work_entry<'a>(
    entries: &'a [crate::inventory::ChaoxingParsedInventoryTask],
    identity: WorkSubmissionIdentity<'_>,
) -> ProviderResult<&'a crate::inventory::ChaoxingParsedInventoryTask> {
    let mut matching = entries
        .iter()
        .filter(|entry| entry.task().remote_id == identity.remote_task_id());
    let entry = matching
        .next()
        .ok_or_else(|| remote_changed("Chaoxing Work is no longer present in fresh inventory"))?;
    if matching.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing Work inventory contains duplicate task identity",
        ));
    }
    Ok(entry)
}

pub(crate) async fn resolve_chapter_work_target(
    inventory: &dyn ChaoxingInventoryTransport,
    context: &ProviderContext,
    route: ChaoxingCourseRoute<'_>,
    identity: WorkSubmissionIdentity<'_>,
) -> ProviderResult<(ChaoxingChapterResourceRequest, ChaoxingChapterWorkTarget)> {
    let knowledge_id = identity.knowledge_id().ok_or_else(|| {
        invalid_response("Chaoxing Chapter Work submission has no knowledge identity")
    })?;
    let document = inventory.fetch_chapter_inventory(context, route).await?;
    let scope = route.parser_scope()?;
    let chapters = parse_chapter_inventory(document.as_str(), &scope)?;
    let mut matching = chapters.iter().filter(|chapter| {
        chapter
            .normalized
            .get("knowledge_id")
            .and_then(serde_json::Value::as_str)
            == Some(knowledge_id)
    });
    let chapter = matching.next().ok_or_else(|| {
        remote_changed("Chaoxing Chapter Work is no longer present before submission")
    })?;
    if matching.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing Chapter Work inventory contains duplicate submission scope",
        ));
    }
    let request =
        ChaoxingChapterResourceRequest::try_from_available_chapter(chapter)?.ok_or_else(|| {
            remote_changed("Chaoxing Chapter Work is no longer available before submission")
        })?;
    let documents = inventory
        .fetch_chapter_resource_inventories(context, route, std::slice::from_ref(&request))
        .await?;
    let target = locate_submission_target(documents, route, &request, identity.remote_task_id())?;
    Ok((request, target))
}

fn locate_submission_target(
    documents: Vec<ChaoxingChapterResourceDocument>,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<ChaoxingChapterWorkTarget> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing Chapter Work submission received an incomplete card set",
        ));
    }
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing Chapter Work submission received a foreign or duplicate card",
            ));
        }
    }
    let scope = route.parser_scope()?;
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed
            .remove(&card_index)
            .ok_or_else(|| protocol_drift("Chaoxing Chapter Work submission omitted a card"))?;
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
                "Chaoxing Chapter Work submission target appears on multiple cards",
            ));
        }
    }
    found.ok_or_else(|| remote_changed("Chaoxing Chapter Work target changed before submission"))
}

pub(crate) fn validate_draft(
    draft: &SubmissionDraft,
    metadata: &ProviderMetadata,
) -> ProviderResult<()> {
    if draft.provider_id != metadata.id
        || draft.provider_version != metadata.implementation_version
        || draft.validate().is_err()
    {
        return Err(invalid_response(
            "Chaoxing submission received an invalid or stale draft",
        ));
    }
    Ok(())
}

pub(crate) fn validate_context(
    context: &ProviderContext,
    metadata: &ProviderMetadata,
) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(internal(
            "Chaoxing submission received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing submission requires an authenticated session",
        ));
    }
    Ok(())
}

pub(crate) fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

pub(crate) fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

pub(crate) fn remote_changed(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::RemoteChanged, message)
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}
