//! `Cidaren` Provider implementation.
//!
//! The current Development checkpoint exposes manual, Capture-assisted and
//! structured external OAuth authentication, account-isolated `BrowserBridge`,
//! class/ordinary-study inventory and fresh task reads. It also implements the
//! current `jv=99` crypto boundary, task-bound answer evidence and a one-shot
//! assessment state machine. Public durable attempt integration and live
//! compatibility remain in progress.
//!
//! Browser result bytes remain opaque outside the Provider. The public runtime
//! surface issues typed commands, accepts only Core-persisted results and
//! consumes validated credentials together with their terminal exchange.
//!
//! ```
//! use asterism_provider_api::BrowserBridgeCapability;
//! use asterism_provider_cidaren::{
//!     CidarenBrowserBridge, CidarenBrowserCapturedValue, CidarenBrowserHelperAction,
//!     CidarenBrowserHelperProjection, CidarenCaptureExchangeCompleted, CidarenCaptureMode,
//!     EncodedCidarenBrowserResultArtifact, project_browser_helper_command,
//! };
//!
//! let _ = CidarenBrowserBridge::capture_snapshot_exchange;
//! let _ = CidarenBrowserBridge::complete_persisted_capture_snapshot_exchange;
//! let _ = <CidarenBrowserBridge as BrowserBridgeCapability>::complete_browser_bridge_credential_result;
//! let _ = CidarenCaptureExchangeCompleted::into_credential_commit_parts;
//! let _ = project_browser_helper_command;
//! let _: Option<CidarenBrowserHelperAction> = None;
//! let _: Option<CidarenBrowserHelperProjection> = None;
//! let _: Option<CidarenBrowserCapturedValue> = None;
//! let _: Option<EncodedCidarenBrowserResultArtifact> = None;
//! let _: CidarenCaptureMode = CidarenCaptureMode::Composite;
//! ```
//!
//! Raw result parsing is deliberately not an external escape hatch.
//!
//! ```compile_fail
//! use asterism_provider_cidaren::{
//!     CidarenBrowserEventEnvelope, CidarenBrowserResultDocument,
//!     CidarenCaptureSnapshot, parse_browser_event,
//! };
//! ```

mod answer;
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
mod browser_protocol;
mod capture_recipe;
mod class_tasks;
mod crypto;
mod duration_read;
mod inventory;
mod metadata;
mod native_http;
mod oauth_authorization;
mod oauth_exchange;
mod pre_question_artifact;
mod protocol_observation;
mod provider;
mod question_artifact;
mod question_inventory;
mod question_parser;
mod response_decode;
mod runtime_settings;
mod score_read;
mod stored_session;
mod study_tasks;
mod submission_build;
mod submission_execute;
mod submission_verify;
mod task_read;

pub use answer::CidarenAnswerResolve;
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
    CidarenAssessmentReceiptKind, CidarenAssessmentRejectionKind, CidarenAssessmentResponse,
    CidarenDecodedAssessmentPayload, parse_assessment_response, parse_word_selection_response,
};
pub use assessment_transport::{CidarenAssessmentTransport, CidarenAssessmentTransportOutcome};
pub use attempt_flow::{
    CIDAREN_REQUIRED_CHILDREN_PENDING_PROVIDER_CODE, CidarenAttemptFlow, CidarenAttemptFlowStatus,
    CidarenAttemptOperation, CidarenDefiniteRejection, CidarenDurableStepOutcome,
    CidarenIssuedCommand, CidarenIssuedOutcome, CidarenPreQuestionContinuation,
    CidarenQuestionMaterialization,
};
pub use authentication::{
    CidarenAuthentication, CidarenAuthenticationTransport, CidarenSessionResolver,
    CidarenTokenSession, classify_token_validation_response,
};
pub use browser_bridge::{
    CidarenBrowserBridge, CidarenCaptureExchangeCompleted, CidarenCaptureExchangeIssued,
};
pub use browser_protocol::{
    CIDAREN_CAPTURE_COMMAND_TYPE, CIDAREN_CAPTURE_RESULT_TYPE, CidarenBrowserCaptureSource,
    CidarenBrowserCapturedValue, CidarenBrowserCommand, CidarenBrowserCommandEnvelope,
    CidarenBrowserHelperAction, CidarenBrowserHelperProjection, CidarenCaptureMode,
    EncodedCidarenBrowserCommandArtifact, EncodedCidarenBrowserResultArtifact,
    project_browser_helper_command,
};
pub use capture_recipe::{cidaren_capture_recipe_v2, cidaren_token_capture_recipe_v1};
pub use class_tasks::{parse_course_inventory, parse_task_inventory};
pub use crypto::CidarenCryptoContext;
pub use duration_read::CidarenDurationRead;
pub use inventory::{
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, CidarenCourseInventory,
    CidarenStudyTaskDocument, CidarenStudyTaskTransport, CidarenTaskInventory,
};
pub use metadata::development_metadata;
pub use native_http::NativeCidarenTransport;
pub use pre_question_artifact::{
    CIDAREN_PRE_QUESTION_ARTIFACT_TYPE, CIDAREN_READING_CARD_PHASE,
    CIDAREN_READY_TO_SELECT_WORDS_PHASE, CIDAREN_READY_TO_START_PHASE, CidarenPreQuestionArtifact,
    EncodedCidarenPreQuestionArtifact,
};
pub use provider::{
    build_development_provider, build_development_provider_native,
    build_development_provider_with_stored_session,
};
pub use question_artifact::{
    CIDAREN_QUESTION_ARTIFACT_PHASE, CIDAREN_QUESTION_ARTIFACT_TYPE,
    CIDAREN_READY_TO_ADVANCE_PHASE, CIDAREN_READY_TO_VERIFY_PHASE, CidarenQuestionArtifact,
    EncodedCidarenQuestionArtifact,
};
pub use question_inventory::CidarenQuestionInventory;
pub use question_parser::{
    CidarenAttemptProgress, CidarenAttemptResponseIdentity, CidarenCurrentQuestionState,
    ParsedCidarenAttemptQuestion, ParsedCidarenAttemptStep, ParsedCidarenReadingCard,
    parse_attempt_question, parse_attempt_step,
};
pub use response_decode::{decode_legacy_response_data, decode_response_data};
pub use runtime_settings::CidarenRuntimeSettings;
pub use score_read::CidarenTaskScoreTransport;
pub use stored_session::StoredCidarenSessionResolver;
pub use study_tasks::{parse_study_course, parse_study_task_inventory};
pub use submission_build::CidarenSubmissionBuild;
pub use submission_execute::CidarenSubmissionExecute;
pub use submission_verify::CidarenSubmissionVerify;
pub use task_read::{CidarenTaskDetail, CidarenTaskProgress};
