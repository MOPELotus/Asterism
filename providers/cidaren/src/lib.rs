//! `Cidaren` Provider implementation.
//!
//! The current Development checkpoint exposes manual and Capture-assisted
//! authentication, account-isolated `BrowserBridge`, class/ordinary-study
//! inventory and fresh task reads. It also implements the current `jv=99`
//! crypto boundary, task-bound answer evidence and a one-shot assessment state
//! machine. Public durable attempt integration and live compatibility remain
//! in progress.

mod answer_evidence_loader;
mod answer_evidence_protocol;
mod answer_evidence_transport;
mod answer_resolve;
mod assessment_protocol;
mod assessment_response;
mod assessment_transport;
mod attempt_flow;
mod authentication;
mod browser_bridge;
mod capture_recipe;
mod class_tasks;
mod crypto;
mod inventory;
mod metadata;
mod native_http;
mod provider;
mod question_parser;
mod response_decode;
mod runtime_settings;
mod stored_session;
mod study_tasks;
mod submission_build;
mod task_read;

pub use answer_evidence_loader::load_answer_evidence;
pub use answer_evidence_protocol::{
    CidarenAnswerEvidenceBinding, CidarenWordInfoRequest, CidarenWordInventory,
    CidarenWordInventoryRequest, CidarenWordLookup, CidarenWordPrototypeRequest,
    CidarenWordSelectionPlan, build_word_info_request, build_word_inventory_request,
    build_word_prototype_request, build_word_selection_plan, parse_course_page_response,
    parse_study_task_info_response, parse_word_info_response, parse_word_prototype_response,
};
pub use answer_evidence_transport::CidarenAnswerEvidenceTransport;
pub use answer_resolve::{
    CidarenAnswerEvidence, CidarenWordEvidence, parse_word_evidence, resolve_answer_candidate,
};
pub use assessment_protocol::{
    CidarenAssessmentBinding, CidarenMutationRequest, CidarenStartAnswerRequest, CidarenWireAnswer,
    build_skip_answer_request, build_start_answer_request, build_submit_answer_and_save_request,
    build_submit_chose_word_request, build_verify_answer_request,
};
pub use assessment_response::{
    CidarenAssessmentReceiptKind, CidarenAssessmentResponse, CidarenDecodedAssessmentPayload,
    parse_assessment_response, parse_word_selection_response,
};
pub use assessment_transport::CidarenAssessmentTransport;
pub use attempt_flow::{
    CidarenAttemptFlow, CidarenAttemptFlowStatus, CidarenAttemptOperation, CidarenIssuedCommand,
    CidarenIssuedOutcome,
};
pub use authentication::{
    CidarenAuthentication, CidarenAuthenticationTransport, CidarenSessionResolver,
    CidarenTokenSession, classify_token_validation_response,
};
pub use browser_bridge::CidarenBrowserBridge;
pub use capture_recipe::cidaren_capture_recipe_v1;
pub use class_tasks::{parse_course_inventory, parse_task_inventory};
pub use crypto::CidarenCryptoContext;
pub use inventory::{
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, CidarenCourseInventory,
    CidarenStudyTaskDocument, CidarenStudyTaskTransport, CidarenTaskInventory,
};
pub use metadata::development_metadata;
pub use native_http::NativeCidarenTransport;
pub use provider::{
    build_development_provider, build_development_provider_native,
    build_development_provider_with_stored_session,
};
pub use question_parser::{
    ParsedCidarenAttemptQuestion, ParsedCidarenAttemptStep, ParsedCidarenReadingCard,
    parse_attempt_question, parse_attempt_step,
};
pub use response_decode::{decode_legacy_response_data, decode_response_data};
pub use runtime_settings::CidarenRuntimeSettings;
pub use stored_session::StoredCidarenSessionResolver;
pub use study_tasks::{parse_study_course, parse_study_task_inventory};
pub use submission_build::CidarenSubmissionBuild;
pub use task_read::{CidarenTaskDetail, CidarenTaskProgress};
