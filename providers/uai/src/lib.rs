//! `uai` Provider implementation.
//!
//! The current Development checkpoint provides native Password/JWT
//! authentication, Course/Task inventory, fresh Group detail and progress,
//! identity-bound duration, separate encrypted Question/answer reads, and a
//! preview/execute/verify submission chain for bounded ordered typed Groups plus
//! verify-only recovery for pure-study preset completion over the shared
//! network policy. It makes no claim of live compatibility.

mod aggregate_progress;
mod annotator;
mod answer;
mod authentication;
mod browser_bridge;
mod course_inventory;
mod course_policy;
mod discussion;
mod duration;
mod encrypted;
mod inventory_capabilities;
mod metadata;
mod native_authentication;
mod native_http;
mod progress;
mod provider;
mod question;
mod resource_execution;
mod runtime_settings;
mod stored_session;
mod submission_build;
mod submission_execute;
mod submission_verify;
mod task_detail;
mod task_inventory;
mod task_type;
mod upload;
mod user_identity;

pub use aggregate_progress::{
    UaiAggregateProgressDocument, UaiAggregateProgressMetric, UaiAggregateProgressTransport,
    UaiCourseAggregateProgress, UaiUnitAggregateProgress, parse_course_aggregate_progress,
};
pub use answer::{
    UaiAnswerDocument, UaiAnswerDocuments, UaiAnswerResolve, UaiAnswerTransport,
    parse_answer_candidates,
};
pub use authentication::{
    UaiAuthentication, UaiAuthenticationTransport, UaiJwtSession, UaiSessionResolver,
    classify_password_login_response,
};
pub use browser_bridge::{
    UaiBrowserBridge, UaiBrowserCommand, UaiBrowserCommandEnvelope, UaiBrowserEvent,
    UaiBrowserEventEnvelope, UaiBrowserMenuEntry, UaiBrowserMessageSecurity, UaiBrowserPageEntry,
    UaiBrowserPageScope, UaiBrowserResidencePlan, UaiBrowserResidenceResult,
    UaiBrowserSessionBinding, UaiBrowserTarget, UaiBrowserTargetMenuEntry,
    UaiBrowserTargetTaskEntry, UaiMenuDiscoveryStrategy, browser_start_url_from_detail,
    parse_browser_event, parse_browser_residence_result,
};
pub use course_inventory::{UaiCourseContext, parse_course_context, parse_course_inventory};
pub use course_policy::{
    UaiCoursePolicy, UaiCoursePolicyDocument, UaiCoursePolicyTransport, UaiUnitCoursePolicy,
    parse_course_policy,
};
pub use discussion::{
    UaiDiscussionBinding, UaiDiscussionReply, UaiDiscussionReplyDraft, UaiDiscussionReplyPage,
    UaiDiscussionTransport, build_discussion_reply_page_request, build_discussion_reply_request,
    build_discussion_topic_request, parse_discussion_binding, parse_discussion_reply_page,
    parse_discussion_reply_receipt, parse_discussion_topic,
};
pub use duration::{
    UaiDurationDocument, UaiDurationTransport, UaiTaskDuration, parse_task_duration,
};
pub use inventory_capabilities::{
    UaiCourseInventory, UaiCourseInventoryTransport, UaiInventoryDocument, UaiTaskInventory,
    UaiTaskInventoryDocuments, UaiTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_authentication::NativeUaiAuthenticationTransport;
pub use native_http::NativeUaiInventoryTransport;
pub use progress::{
    UaiGroupProgressSnapshot, UaiProgressDocument, UaiProgressTransport, UaiTaskProgress,
    parse_group_progress,
};
pub use provider::{
    UaiDevelopmentTransports, UaiSubmissionTransports, build_development_provider,
    build_development_provider_native, build_development_provider_with_native_inventory,
    build_development_provider_with_renewal, build_development_provider_with_stored_session,
};
pub use question::{
    ParsedUaiQuestion, UaiQuestionDocument, UaiQuestionMediaSource, UaiQuestionRead,
    UaiQuestionTransport, parse_question_content,
};
pub use resource_execution::{
    UaiPresetCompletionResult, UaiPresetCompletionTransport, UaiResourceExecution,
};
pub use stored_session::StoredUaiSessionResolver;
pub use submission_build::UaiSubmissionBuild;
pub use submission_execute::{
    UaiSubmissionExecute, UaiSubmissionJudgePlan, UaiSubmissionPlan, UaiSubmissionProtocolVersions,
    UaiSubmissionQuestionPlan, UaiSubmissionResponseDocument, UaiSubmissionTransport,
    parse_submission_receipt,
};
pub use submission_verify::{
    UaiSubmissionVerify, UaiVerificationDocument, UaiVerificationTransport,
    parse_verification_snapshot,
};
pub use task_detail::UaiTaskDetail;
pub use task_inventory::parse_task_inventory;
pub use upload::{
    UaiMultipartUpload, UaiUploadArtifact, UaiUploadGrant, UaiUploadIntent, UaiUploadPreparation,
    UaiUploadTransport, UaiUploadedArtifact, build_upload_multipart, parse_upload_grant,
    parse_upload_result,
};
