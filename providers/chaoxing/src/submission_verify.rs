use std::{collections::BTreeMap, fmt, sync::Arc};

use asterism_domain::{
    RemoteState, SubmissionDraft, SubmissionQuestionVerification,
    SubmissionQuestionVerificationStatus, SubmissionReceipt, SubmissionVerificationSnapshot,
    SubmissionVerificationStatus,
};
use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderIdentity, ProviderMetadata, ProviderResult,
    ResolvedProviderQuestionSessionContinuation, SubmissionBuildCapability,
    SubmissionVerifyCapability,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingCourseRoute,
    ChaoxingExamQuestionArtifact, ChaoxingInventoryTransport, ChaoxingSubmissionBuild,
    ChaoxingSubmissionPlan, ChaoxingWorkDetailRequest, ChaoxingWorkVerificationDocument,
    inventory::{parse_exam_inventory_entries, parse_work_inventory_entries},
    metadata::development_metadata,
    submission_execute::{
        invalid_response, matching_course, protocol_drift, remote_changed, validate_context,
        validate_draft, validate_exam_continuation,
    },
    submission_support::{SubmissionModule, WorkSubmissionIdentity, parse_verification_snapshot},
    task_inventory::CHAPTER_RESOURCE_CARD_COUNT,
};

/// Read-only transport for one freshly rediscovered Work result page.
#[async_trait]
pub trait ChaoxingSubmissionVerificationTransport: Send + Sync {
    async fn fetch_work_verification(
        &self,
        context: &ProviderContext,
        request: ChaoxingWorkDetailRequest<'_>,
    ) -> ProviderResult<ChaoxingWorkVerificationDocument>;
}

/// Independent Chaoxing post-submit verification. It never calls the mutation
/// endpoint and can therefore recover an ambiguous submit without a receipt.
pub struct ChaoxingSubmissionVerify {
    metadata: ProviderMetadata,
    courses: Arc<dyn CourseInventoryCapability>,
    inventory: Arc<dyn ChaoxingInventoryTransport>,
    preview: ChaoxingSubmissionBuild,
    transport: Arc<dyn ChaoxingSubmissionVerificationTransport>,
}

impl ChaoxingSubmissionVerify {
    /// Builds verification around fresh Work discovery and a read-only page
    /// transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(
        courses: Arc<dyn CourseInventoryCapability>,
        inventory: Arc<dyn ChaoxingInventoryTransport>,
        transport: Arc<dyn ChaoxingSubmissionVerificationTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            courses,
            inventory,
            preview: ChaoxingSubmissionBuild::try_new()?,
            transport,
        })
    }
}

impl fmt::Debug for ChaoxingSubmissionVerify {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingSubmissionVerify")
            .field("metadata", &self.metadata)
            .field("courses", &"configured")
            .field("inventory", &"configured")
            .field("preview", &self.preview)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingSubmissionVerify {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl SubmissionVerifyCapability for ChaoxingSubmissionVerify {
    async fn verify_submission(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        validate_context(context, &self.metadata)?;
        let identity = WorkSubmissionIdentity::parse(remote_task_id)?;
        validate_draft(draft, &self.metadata)?;
        if let Some(receipt) = receipt {
            receipt
                .validate()
                .map_err(|_| invalid_response("Chaoxing verification receipt is invalid"))?;
            if receipt.remote_status != "accepted" {
                return Err(invalid_response(
                    "Chaoxing verification receipt is not an accepted acknowledgement",
                ));
            }
        }
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
                "Chaoxing verification draft preview is stale or foreign",
            ));
        }
        let plan = ChaoxingSubmissionPlan::from_draft(draft)?;

        let courses = self.courses.list_courses(context).await?;
        let course = matching_course(&courses, identity)?;
        let route = ChaoxingCourseRoute::from_remote_course(course)?;
        match identity.module() {
            SubmissionModule::IndependentWork => {
                let document = self.inventory.fetch_work_inventory(context, route).await?;
                let entries =
                    parse_work_inventory_entries(document.as_str(), &route.parser_scope()?)?;
                let mut matching = entries
                    .iter()
                    .filter(|entry| entry.task().remote_id == identity.remote_task_id());
                let entry = matching.next().ok_or_else(|| {
                    remote_changed("Chaoxing Work is no longer present during verification")
                })?;
                if matching.next().is_some() {
                    return Err(protocol_drift(
                        "Chaoxing Work verification inventory contains duplicate task identity",
                    ));
                }
                let request =
                    ChaoxingWorkDetailRequest::try_new(route, remote_task_id, entry.entry())?;
                let document = self
                    .transport
                    .fetch_work_verification(context, request)
                    .await?;
                parse_verification_snapshot(&document, &plan, draft)
            }
            SubmissionModule::ChapterWork => {
                let state =
                    resolve_chapter_work_state(self.inventory.as_ref(), context, route, identity)
                        .await?;
                chapter_work_snapshot(draft, state)
            }
            SubmissionModule::Exam => Err(asterism_provider_api::ProviderError::new(
                asterism_provider_api::ProviderErrorKind::UnsupportedTask,
                "Chaoxing Exam verification requires its durable Question session",
            )),
        }
    }

    async fn verify_submission_with_session(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
        draft: &SubmissionDraft,
        receipt: Option<&SubmissionReceipt>,
        continuation: ResolvedProviderQuestionSessionContinuation<'_>,
    ) -> ProviderResult<SubmissionVerificationSnapshot> {
        let identity = WorkSubmissionIdentity::parse(remote_task_id)?;
        if identity.module() != SubmissionModule::Exam {
            return self
                .verify_submission(context, remote_task_id, draft, receipt)
                .await;
        }
        validate_context(context, &self.metadata)?;
        validate_draft(draft, &self.metadata)?;
        validate_exam_continuation(&continuation)?;
        if let Some(receipt) = receipt {
            receipt
                .validate()
                .map_err(|_| invalid_response("Chaoxing Exam verification receipt is invalid"))?;
            if receipt.remote_status != "accepted" {
                return Err(invalid_response(
                    "Chaoxing Exam verification receipt is not accepted",
                ));
            }
        }
        let artifact = ChaoxingExamQuestionArtifact::decode_bound(
            continuation.value,
            continuation.continuation_digest,
            draft,
            remote_task_id,
        )?;
        if artifact.next_answer_index() != artifact.question_count() {
            return Err(remote_changed(
                "Chaoxing Exam verification was requested before every answer save",
            ));
        }
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
                "Chaoxing Exam verification Draft preview is stale or foreign",
            ));
        }
        let state = fresh_exam_state(
            self.courses.as_ref(),
            self.inventory.as_ref(),
            context,
            identity,
        )
        .await?;
        task_state_snapshot(draft, state, "Exam")
    }
}

pub(crate) async fn fresh_exam_state(
    courses: &dyn CourseInventoryCapability,
    inventory: &dyn ChaoxingInventoryTransport,
    context: &ProviderContext,
    identity: WorkSubmissionIdentity<'_>,
) -> ProviderResult<RemoteState> {
    let courses = courses.list_courses(context).await?;
    let course = matching_course(&courses, identity)?;
    let route = ChaoxingCourseRoute::from_remote_course(course)?;
    let document = inventory.fetch_exam_inventory(context, route).await?;
    let entries = parse_exam_inventory_entries(document.as_str(), &route.parser_scope()?)?;
    let mut matching = entries
        .iter()
        .filter(|entry| entry.task().remote_id == identity.remote_task_id());
    let entry = matching
        .next()
        .ok_or_else(|| remote_changed("Chaoxing Exam is no longer present during verification"))?;
    if matching.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing Exam verification inventory contains duplicate task identity",
        ));
    }
    Ok(entry.task().remote_state)
}

async fn resolve_chapter_work_state(
    inventory: &dyn ChaoxingInventoryTransport,
    context: &ProviderContext,
    route: ChaoxingCourseRoute<'_>,
    identity: WorkSubmissionIdentity<'_>,
) -> ProviderResult<RemoteState> {
    let knowledge_id = identity.knowledge_id().ok_or_else(|| {
        invalid_response("Chaoxing Chapter Work verification has no knowledge identity")
    })?;
    let document = inventory.fetch_chapter_inventory(context, route).await?;
    let scope = route.parser_scope()?;
    let chapters = crate::parse_chapter_inventory(document.as_str(), &scope)?;
    let mut matching = chapters.iter().filter(|chapter| {
        chapter
            .normalized
            .get("knowledge_id")
            .and_then(serde_json::Value::as_str)
            == Some(knowledge_id)
    });
    let chapter = matching.next().ok_or_else(|| {
        remote_changed("Chaoxing Chapter Work is no longer present during verification")
    })?;
    if matching.next().is_some() {
        return Err(protocol_drift(
            "Chaoxing Chapter Work verification found duplicate Chapter scope",
        ));
    }
    let request =
        ChaoxingChapterResourceRequest::try_from_available_chapter(chapter)?.ok_or_else(|| {
            remote_changed("Chaoxing Chapter Work is no longer readable during verification")
        })?;
    let documents = inventory
        .fetch_chapter_resource_inventories(context, route, std::slice::from_ref(&request))
        .await?;
    locate_chapter_work_state(documents, route, &request, identity.remote_task_id())
}

fn locate_chapter_work_state(
    documents: Vec<ChaoxingChapterResourceDocument>,
    route: ChaoxingCourseRoute<'_>,
    request: &ChaoxingChapterResourceRequest,
    remote_task_id: &str,
) -> ProviderResult<RemoteState> {
    if documents.len() != usize::from(CHAPTER_RESOURCE_CARD_COUNT) {
        return Err(protocol_drift(
            "Chaoxing Chapter Work verification received an incomplete card set",
        ));
    }
    let mut indexed = BTreeMap::new();
    for document in documents {
        if document.knowledge_id() != request.knowledge_id()
            || indexed.insert(document.card_index(), document).is_some()
        {
            return Err(protocol_drift(
                "Chaoxing Chapter Work verification received a foreign or duplicate card",
            ));
        }
    }
    let scope = route.parser_scope()?;
    let mut found = None;
    for card_index in 0..CHAPTER_RESOURCE_CARD_COUNT {
        let document = indexed
            .remove(&card_index)
            .ok_or_else(|| protocol_drift("Chaoxing Chapter Work verification omitted a card"))?;
        for task in crate::parse_chapter_resource_inventory(
            document.as_str(),
            &scope,
            request.knowledge_id(),
            card_index,
        )? {
            if task.remote_id != remote_task_id {
                continue;
            }
            if task
                .normalized
                .get("resource_kind")
                .and_then(serde_json::Value::as_str)
                != Some("chapter_work")
            {
                return Err(protocol_drift(
                    "Chaoxing Chapter Work verification resolved another resource kind",
                ));
            }
            if found.replace(task.remote_state).is_some() {
                return Err(protocol_drift(
                    "Chaoxing Chapter Work verification target appears on multiple cards",
                ));
            }
        }
    }
    found.ok_or_else(|| {
        remote_changed("Chaoxing Chapter Work target disappeared during verification")
    })
}

fn chapter_work_snapshot(
    draft: &SubmissionDraft,
    remote_state: RemoteState,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    task_state_snapshot(draft, remote_state, "Chapter Work")
}

fn task_state_snapshot(
    draft: &SubmissionDraft,
    remote_state: RemoteState,
    module: &'static str,
) -> ProviderResult<SubmissionVerificationSnapshot> {
    let (status, progress_percent) = match remote_state {
        RemoteState::Completed => (SubmissionVerificationStatus::Confirmed, Some(100)),
        RemoteState::Pending | RemoteState::InProgress => {
            (SubmissionVerificationStatus::Pending, None)
        }
        RemoteState::Unknown
        | RemoteState::NotOpen
        | RemoteState::Expired
        | RemoteState::Removed => (SubmissionVerificationStatus::Inconclusive, None),
    };
    let snapshot = SubmissionVerificationSnapshot {
        status,
        remote_state: Some(remote_state),
        score: None,
        progress_percent,
        questions: draft
            .items
            .iter()
            .map(|item| SubmissionQuestionVerification {
                question_id: item.question.id,
                status: SubmissionQuestionVerificationStatus::Unverified,
            })
            .collect(),
        verified_at: Utc::now(),
    };
    snapshot.validate().map_err(|_| {
        let _ = module;
        invalid_response("Chaoxing task-state verification snapshot is invalid")
    })?;
    Ok(snapshot)
}
