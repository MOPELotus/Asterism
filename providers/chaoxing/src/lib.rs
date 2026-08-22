//! Chaoxing Provider implementation.
//!
//! The implementation contains deterministic authentication, Course, Work and
//! Exam contracts plus unverified native transports. It deliberately does not
//! claim live compatibility or a verified Provider.

mod answer;
mod authentication;
mod browser_bridge;
mod chapter_inventory;
mod course_enrollment;
mod course_inventory;
mod course_invite;
mod exam_attempt;
mod exam_submission;
mod history_harvest;
mod inventory;
mod metadata;
mod native_http;
mod provider;
mod qr_authentication;
mod question_parser;
mod question_read;
mod resource_execution;
mod resource_inventory;
mod runtime_settings;
mod sign_analysis;
mod sign_event;
mod sign_in;
mod sign_in_prepare;
mod sign_in_receipt;
mod sign_qr;
mod stored_session;
mod submission_build;
mod submission_execute;
#[cfg(test)]
mod submission_integration_tests;
mod submission_support;
mod submission_verify;
mod task_detail;
mod task_inventory;
mod task_progress;
mod work_submission;

pub use answer::{
    ChaoxingAnswerResolutionTransport, ChaoxingAnswerResolve, ChaoxingChapterWorkResultRequest,
};
pub use authentication::{
    ChaoxingAuthentication, ChaoxingAuthenticationTransport, NativeChaoxingAuthenticationTransport,
};
pub use browser_bridge::ChaoxingBrowserBridge;
pub use chapter_inventory::parse_chapter_inventory;
pub use course_enrollment::ChaoxingCourseEnrollment;
pub use course_inventory::{
    ChaoxingCourseInventory, ChaoxingCourseInventoryTransport, parse_course_inventory,
};
pub use course_invite::{
    ChaoxingCourseEnrollmentCommand, ChaoxingCourseEnrollmentRecoveryOutcome,
    ChaoxingCourseEnrollmentTransport, ChaoxingCourseInviteApiDocument,
    ChaoxingCourseInviteApiPreparation, ChaoxingCourseInvitePreview,
    ChaoxingCourseInvitePreviewDocument, ChaoxingCourseInvitePreviewPreparation,
    ChaoxingCourseInvitePreviewRedirect, ChaoxingCourseJoinPreparation, ChaoxingCourseJoinReceipt,
    ChaoxingCourseJoinReceiptDocument, ChaoxingCourseJoinReceiptKind,
    ChaoxingCourseMembershipObservation, ChaoxingIssuedCourseEnrollment, ChaoxingIssuedCourseJoin,
    parse_course_invite_api_redirect, parse_course_invite_preview, parse_course_join_receipt,
};
pub use exam_attempt::{
    ChaoxingExamQuestionArtifact, ChaoxingExamQuestionRequest, ChaoxingExamStartCommand,
    ChaoxingExamStartOutcome,
};
pub use exam_submission::{
    ChaoxingExamSubmissionCommand, ChaoxingExamSubmissionResponse, parse_exam_submission_response,
};
pub use history_harvest::{
    ChaoxingAnswerHistoryHarvest, ChaoxingAnswerHistoryTransport, ChaoxingChapterWorkHistoryPage,
    ChaoxingChapterWorkHistoryRecord, ChaoxingChapterWorkHistoryResultRequest,
};
pub use inventory::{
    ChaoxingCourseScope, classify_work_detail, parse_exam_inventory, parse_work_inventory,
};
pub use native_http::{
    ChaoxingCookieSession, ChaoxingSessionResolver, NativeChaoxingCourseJoinRequest,
    NativeChaoxingInventoryTransport,
};
pub use provider::{build_development_provider, build_development_provider_with_renewal};
pub use qr_authentication::{
    ChaoxingQrAuthenticationTransport, ChaoxingQrChallenge, ChaoxingQrPollOutcome,
    NativeChaoxingQrAuthenticationTransport,
};
pub use question_parser::{
    ParsedChaoxingQuestion, parse_chapter_work_question_page, parse_exam_preview_question_page,
    parse_exam_question_page, parse_work_preview_question_page,
};
pub use question_read::{ChaoxingQuestionRead, ChaoxingQuestionTransport};
pub use resource_execution::ChaoxingResourceExecution;
pub use resource_inventory::{ChaoxingChapterWorkTarget, parse_chapter_resource_inventory};
pub use sign_analysis::{
    ChaoxingPreSignAnalysisContinuation, ChaoxingPreSignAnalysisDocument,
    ChaoxingPreSignAnalysisPreparation, parse_pre_sign_analysis_continuation,
};
pub use sign_event::{
    ChaoxingSignEvent, ChaoxingSignEventBootstrap, ChaoxingSignEventBootstrapDocument,
    ChaoxingSignEventDocument, ChaoxingSignEventRead, ChaoxingSignEventReadTransport,
    parse_sign_event, parse_sign_event_bootstrap,
};
pub use sign_in::{
    ChaoxingSignActivity, ChaoxingSignActivityListDocument, ChaoxingSignActivityRead,
    ChaoxingSignActivityReadTransport, ChaoxingSignDetail, ChaoxingSignDetailDocument,
    ChaoxingSignDetailRequest, ChaoxingSignVariant, parse_sign_activity_list, parse_sign_detail,
};
pub use sign_in_prepare::{ChaoxingNormalSignPreparation, ChaoxingNormalSignProtocolFamily};
pub use sign_in_receipt::{
    ChaoxingPreSignDocument, ChaoxingPreSignEvidence, ChaoxingPreSignEvidenceKind,
    ChaoxingSignReceipt, ChaoxingSignReceiptDocument, ChaoxingSignReceiptKind,
    parse_pre_sign_evidence, parse_sign_receipt,
};
pub use sign_qr::{
    ChaoxingQrSignMaterial, ChaoxingQrSignMaterialSource, ChaoxingQrSignSubmissionPreparation,
};
pub use stored_session::StoredChaoxingSessionResolver;
pub use submission_build::ChaoxingSubmissionBuild;
pub use submission_execute::{ChaoxingSubmissionExecute, ChaoxingSubmissionTransport};
pub use submission_support::{
    ChaoxingChapterWorkAnswerJudgement, ChaoxingChapterWorkQuestionEvidence,
    ChaoxingChapterWorkResultEvidence, ChaoxingChapterWorkRetakeEntry,
    ChaoxingChapterWorkVerificationDocument, ChaoxingExamVerificationDocument,
    ChaoxingSubmissionPlan, ChaoxingWorkVerificationDocument, ChaoxingWorkVerificationRoute,
    parse_chapter_work_answer_candidates, parse_chapter_work_result_evidence,
    parse_chapter_work_verification_snapshot, parse_exam_verification_snapshot,
    parse_submission_receipt, parse_verification_snapshot,
};
pub use submission_verify::{ChaoxingSubmissionVerificationTransport, ChaoxingSubmissionVerify};
pub use task_detail::ChaoxingTaskDetail;
pub use task_inventory::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingCourseRoute,
    ChaoxingExamDetailFacts, ChaoxingExamDetailRequest, ChaoxingInventoryDocument,
    ChaoxingInventoryTransport, ChaoxingTaskInventory, ChaoxingWorkDetailRequest,
    ChaoxingWorkDetailState,
};
pub use task_progress::ChaoxingTaskProgress;
pub use work_submission::{ChaoxingWorkSubmissionCommand, ChaoxingWorkSubmissionResponse};
