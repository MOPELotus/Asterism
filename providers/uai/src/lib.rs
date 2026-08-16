//! `uai` Provider implementation.
//!
//! The current Development checkpoint provides native Password/JWT
//! authentication, Course/Task inventory, independent Course/Group progress,
//! identity-bound duration, separate encrypted Question/answer reads, and a
//! preview/execute/verify submission chain for bounded ordered typed Groups plus
//! verify-only recovery for pure-study preset completion over the shared
//! network policy. It makes no claim of live compatibility.

mod aggregate_progress;
mod annotator;
mod answer;
mod authentication;
mod browser_batch;
mod browser_bridge;
mod browser_cursor;
mod compound_oral;
mod compound_upload;
mod course_inventory;
mod course_policy;
mod course_progress;
mod discussion;
mod discussion_sequence;
mod duration;
mod encrypted;
mod inventory_capabilities;
mod metadata;
mod native_authentication;
mod native_http;
mod progress;
mod provider;
mod question;
mod question_artifact;
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
mod upload_final_plan;
mod upload_sequence;
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
pub use browser_batch::{
    UAI_COURSE_RESIDENCE_CHILD_PLAN_ARTIFACT_TYPE, UaiCourseResidenceBatchPlan,
    UaiCourseResidenceBudgetShare, UaiCourseResidenceChildPlan, UaiCourseResidenceMicro,
    UaiCourseResidenceRestartTarget, UaiCourseResidenceTask, build_course_residence_batch_plan,
    build_course_residence_child_plan,
};
pub use browser_bridge::{
    EncodedUaiBrowserCommandArtifact, EncodedUaiBrowserHelperResult, UAI_BROWSER_COMMAND_TYPE,
    UAI_BROWSER_CURSOR_STATE_TYPE, UAI_BROWSER_EVENT_TYPE, UAI_BROWSER_RESIDENCE_RESULT_TYPE,
    UaiBrowserBridge, UaiBrowserCommand, UaiBrowserCommandEnvelope, UaiBrowserCursorExchangeIssued,
    UaiBrowserCursorExchangeRecovered, UaiBrowserCursorPersistenceHandoff,
    UaiBrowserCursorPersistenceRecovery, UaiBrowserCursorResult, UaiBrowserEvent,
    UaiBrowserEventDocument, UaiBrowserEventEnvelope, UaiBrowserEventExchangeCompleted,
    UaiBrowserEventInbox, UaiBrowserExchangeIssued, UaiBrowserExecutionTerminal,
    UaiBrowserHelperAction, UaiBrowserHelperClickKind, UaiBrowserHelperClickRecipe,
    UaiBrowserHelperClickStep, UaiBrowserHelperCommandProjection, UaiBrowserHelperDomProfile,
    UaiBrowserHelperDomRecipe, UaiBrowserHelperMenuFamilyRecipe, UaiBrowserHelperMenuHierarchyRule,
    UaiBrowserHelperMenuLeafRule, UaiBrowserHelperMenuObservation, UaiBrowserHelperMenuRecipe,
    UaiBrowserHelperObservation, UaiBrowserHelperPageActiveRule, UaiBrowserHelperPageObservation,
    UaiBrowserHelperPageRecipe, UaiBrowserHelperResidenceControlObservation,
    UaiBrowserHelperResidenceControlRecipe, UaiBrowserHelperResidenceObservation,
    UaiBrowserHelperResidenceRecipe, UaiBrowserHelperTextField, UaiBrowserIntermediateResult,
    UaiBrowserMenuEntry, UaiBrowserMessageSecurity, UaiBrowserPageEntry, UaiBrowserPageScope,
    UaiBrowserResidenceControl, UaiBrowserResidenceControlExchangeCompleted,
    UaiBrowserResidenceExchangeCompleted, UaiBrowserResidenceInbox, UaiBrowserResidencePlan,
    UaiBrowserResidenceResult, UaiBrowserResidenceResultDocument, UaiBrowserResultInbox,
    UaiBrowserSessionBinding, UaiBrowserTarget, UaiBrowserTargetMenuEntry,
    UaiBrowserTargetTaskEntry, UaiMenuDiscoveryStrategy, browser_event_exchange_digest,
    browser_residence_exchange_digest, browser_start_url_from_detail, parse_browser_event,
    parse_browser_residence_result, project_browser_helper_command,
};
pub use browser_cursor::{
    EncodedUaiBrowserCursorArtifact, UaiBrowserCursorAdvance, UaiBrowserCursorStage,
    UaiBrowserDurationReadback, UaiBrowserResidenceCheckpoint, UaiBrowserResidenceCursor,
};
pub use compound_oral::{
    UaiCompoundOralPreparation, UaiCompoundOralSubmission, UaiCompoundOralSubmissionRequest,
    UaiCompoundOralTransport, UaiCompoundOralVerification, build_compound_oral_submission_request,
    parse_compound_oral_verification,
};
pub use compound_upload::{
    UaiCompoundUploadPreparation, UaiCompoundUploadSubmission, UaiCompoundUploadSubmissionRequest,
    UaiCompoundUploadTransport, UaiCompoundUploadVerification,
    build_compound_upload_submission_request, parse_compound_upload_verification,
};
pub use course_inventory::{UaiCourseContext, parse_course_context, parse_course_inventory};
pub use course_policy::{
    UaiCoursePolicy, UaiCoursePolicyDocument, UaiCoursePolicyTransport, UaiUnitCoursePolicy,
    parse_course_policy,
};
pub use course_progress::{
    UaiCourseProgressDocument, UaiCourseProgressSnapshot, UaiCourseUnitProgressStrategy,
    parse_course_progress,
};
pub use discussion::{
    UaiDiscussionBinding, UaiDiscussionCompletionPlan, UaiDiscussionCompletionResult,
    UaiDiscussionReply, UaiDiscussionReplyDraft, UaiDiscussionReplyPage, UaiDiscussionTransport,
    build_discussion_reply_page_request, build_discussion_reply_request,
    build_discussion_topic_request, parse_discussion_binding, parse_discussion_reply_page,
    parse_discussion_reply_receipt, parse_discussion_topic, prepare_discussion_completion,
};
pub use discussion_sequence::{
    UAI_DISCUSSION_COMPLETION_OPERATION_TYPE, UAI_DISCUSSION_PLAN_ARTIFACT_TYPE,
    UAI_DISCUSSION_REPLY_OPERATION_TYPE, UAI_DISCUSSION_REPLY_READBACK_OBSERVATION_TYPE,
    UAI_DISCUSSION_SEQUENCE_TYPE, UaiDiscussionMutationKind, UaiDiscussionMutationOutcome,
    UaiDiscussionMutationSequence, UaiDiscussionRecoveryState, UaiDiscussionReplyReadbackGate,
};
pub use duration::{
    UaiDurationDocument, UaiDurationTransport, UaiTaskDuration, UaiTaskStudyRecord,
    parse_task_duration, parse_task_study_record,
};
pub use inventory_capabilities::{
    UaiCourseInventory, UaiCourseInventoryTransport, UaiInventoryDocument, UaiTaskInventory,
    UaiTaskInventoryDocuments, UaiTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_authentication::NativeUaiAuthenticationTransport;
pub use native_http::NativeUaiInventoryTransport;
pub use progress::{
    UaiGroupProgressSnapshot, UaiMicroProgressSnapshot, UaiProgressDocument, UaiProgressTransport,
    UaiTaskProgress, parse_group_progress, parse_micro_progress,
};
pub use provider::{
    UaiDevelopmentTransports, UaiSubmissionTransports, build_development_provider,
    build_development_provider_native, build_development_provider_with_native_inventory,
    build_development_provider_with_renewal, build_development_provider_with_stored_session,
};
pub use question::{
    ParsedUaiQuestion, UaiQuestionDocument, UaiQuestionMediaSource, UaiQuestionParseResult,
    UaiQuestionRead, UaiQuestionTransport, parse_question_content,
};
pub use question_artifact::{
    EncodedUaiQuestionArtifact, EncodedUaiQuestionArtifactSet, UAI_QUESTION_ARTIFACT_PHASE,
    UAI_QUESTION_ARTIFACT_TYPE, UAI_QUESTION_SET_ARTIFACT_PHASE,
    UAI_QUESTION_SET_ARTIFACT_TTL_SECONDS, UAI_QUESTION_SET_ARTIFACT_TYPE, UaiMediaFetchBatchPlan,
    UaiMediaFetchBatchResponseSet, UaiMediaFetchCredentialScope, UaiMediaFetchPlan,
    UaiMediaFetchResponse, UaiMediaSubtitleTranscript, UaiQuestionArtifact,
    UaiQuestionArtifactMediaSource, UaiQuestionArtifactSet,
};
pub use resource_execution::{
    UaiDiscussionEmptySubmission, UaiOralEmptySubmission, UaiPresetCompletionResult,
    UaiPresetCompletionTransport, UaiPresetEmptySubmission, UaiResourceExecution,
    UaiSubjectiveEmptySubmission,
};
pub use stored_session::StoredUaiSessionResolver;
pub use submission_build::UaiSubmissionBuild;
pub use submission_execute::{
    UaiSubmissionExecute, UaiSubmissionJudgePlan, UaiSubmissionPlan, UaiSubmissionProtocolVersions,
    UaiSubmissionQuestionPlan, UaiSubmissionRequest, UaiSubmissionResponseDocument,
    UaiSubmissionTransport, build_submission_request, parse_submission_receipt,
};
pub use submission_verify::{
    UaiSubmissionPolicyEvidence, UaiSubmissionVerify, UaiVerificationDocument,
    UaiVerificationEvidenceSnapshot, UaiVerificationTransport,
    parse_verification_evidence_snapshot, parse_verification_snapshot,
};
pub use task_detail::UaiTaskDetail;
pub use task_inventory::parse_task_inventory;
pub use upload::{
    UaiMultipartUpload, UaiUploadArtifact, UaiUploadGrant, UaiUploadGrantRequest, UaiUploadIntent,
    UaiUploadPreparation, UaiUploadSubmission, UaiUploadSubmissionRequest, UaiUploadTransport,
    UaiUploadVerification, UaiUploadedArtifact, build_upload_grant_request, build_upload_multipart,
    build_upload_submission_request, parse_upload_grant, parse_upload_result,
    parse_upload_verification,
};
pub use upload_final_plan::{
    EncodedUaiUploadFinalPlanState, UAI_UPLOAD_FINAL_PLAN_STATE_TYPE, UaiUploadFinalPlanState,
};
pub use upload_sequence::{
    EncodedUaiUploadFinalResultState, UAI_UPLOAD_FINAL_MAXIMUM_ATTEMPTS,
    UAI_UPLOAD_FINAL_OPERATION_TYPE, UAI_UPLOAD_FINAL_PLAN_ARTIFACT_TYPE,
    UAI_UPLOAD_FINAL_RESULT_STATE_TYPE, UAI_UPLOAD_FINAL_RETRY_SECONDS,
    UAI_UPLOAD_FINAL_SEQUENCE_TYPE, UaiUploadFinalResultState, UaiUploadFinalRetryCode,
    UaiUploadFinalSubmissionKind, UaiUploadFinalSubmissionOutcome,
    UaiUploadFinalSubmissionSequence,
};
