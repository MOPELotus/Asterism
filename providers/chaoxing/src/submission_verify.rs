use std::{fmt, sync::Arc};

use asterism_domain::{SubmissionDraft, SubmissionReceipt, SubmissionVerificationSnapshot};
use asterism_provider_api::{
    CourseInventoryCapability, ProviderContext, ProviderIdentity, ProviderMetadata, ProviderResult,
    SubmissionBuildCapability, SubmissionVerifyCapability,
};
use async_trait::async_trait;

use crate::{
    ChaoxingCourseRoute, ChaoxingInventoryTransport, ChaoxingSubmissionBuild,
    ChaoxingSubmissionPlan, ChaoxingWorkDetailRequest, ChaoxingWorkVerificationDocument,
    inventory::parse_work_inventory_entries,
    metadata::development_metadata,
    submission_execute::{
        invalid_response, matching_course, protocol_drift, remote_changed, validate_context,
        validate_draft,
    },
    submission_support::{WorkSubmissionIdentity, parse_verification_snapshot},
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
        let document = self.inventory.fetch_work_inventory(context, route).await?;
        let entries = parse_work_inventory_entries(document.as_str(), &route.parser_scope()?)?;
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
        let request = ChaoxingWorkDetailRequest::try_new(route, remote_task_id, entry.entry())?;
        let document = self
            .transport
            .fetch_work_verification(context, request)
            .await?;
        parse_verification_snapshot(&document, &plan, draft)
    }
}
