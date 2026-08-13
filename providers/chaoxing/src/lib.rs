//! Chaoxing Provider implementation.
//!
//! The implementation contains deterministic authentication, Course, Work and
//! Exam contracts plus unverified native transports. It deliberately does not
//! claim live compatibility or a verified Provider.

mod authentication;
mod chapter_inventory;
mod course_inventory;
mod exam_attempt;
mod inventory;
mod metadata;
mod native_http;
mod provider;
mod question_parser;
mod question_read;
mod resource_execution;
mod resource_inventory;
mod runtime_settings;
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

pub use authentication::{
    ChaoxingAuthentication, ChaoxingAuthenticationTransport, NativeChaoxingAuthenticationTransport,
};
pub use chapter_inventory::parse_chapter_inventory;
pub use course_inventory::{
    ChaoxingCourseInventory, ChaoxingCourseInventoryTransport, parse_course_inventory,
};
pub(crate) use exam_attempt::ChaoxingExamQuestionRequest;
pub use inventory::{
    ChaoxingCourseScope, classify_work_detail, parse_exam_inventory, parse_work_inventory,
};
pub use native_http::{
    ChaoxingCookieSession, ChaoxingSessionResolver, NativeChaoxingInventoryTransport,
};
pub use provider::{build_development_provider, build_development_provider_with_renewal};
pub use question_parser::{
    ParsedChaoxingQuestion, parse_chapter_work_question_page, parse_exam_preview_question_page,
    parse_exam_question_page, parse_work_preview_question_page,
};
pub use question_read::{ChaoxingQuestionRead, ChaoxingQuestionTransport};
pub use resource_execution::ChaoxingResourceExecution;
pub use resource_inventory::{ChaoxingChapterWorkTarget, parse_chapter_resource_inventory};
pub use stored_session::StoredChaoxingSessionResolver;
pub use submission_build::ChaoxingSubmissionBuild;
pub use submission_execute::{ChaoxingSubmissionExecute, ChaoxingSubmissionTransport};
pub use submission_support::{
    ChaoxingSubmissionPlan, ChaoxingWorkVerificationDocument, ChaoxingWorkVerificationRoute,
    parse_submission_receipt, parse_verification_snapshot,
};
pub use submission_verify::{ChaoxingSubmissionVerificationTransport, ChaoxingSubmissionVerify};
pub use task_detail::ChaoxingTaskDetail;
pub use task_inventory::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingCourseRoute,
    ChaoxingInventoryDocument, ChaoxingInventoryTransport, ChaoxingTaskInventory,
    ChaoxingWorkDetailRequest, ChaoxingWorkDetailState,
};
pub use task_progress::ChaoxingTaskProgress;
