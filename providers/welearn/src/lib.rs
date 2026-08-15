//! `WELearn` Provider implementation.
//!
//! The current Development checkpoint provides native Authentication,
//! Course/Task inventory, fresh CMI progress, independent duration reporting,
//! and donor-audited completion/progress/score `ResourceExecution` behind
//! explicit daemon opt-in. Its parser/native-boundary coverage makes no claim
//! of live compatibility.

mod atomic_duration_completion;
mod atomic_mutation_digest;
mod atomic_recovery;
mod authentication;
mod batch_plan;
mod cmi;
mod course_context;
mod course_inventory;
mod duration_read;
mod duration_report;
mod execution;
mod execution_selection;
mod inventory_capabilities;
mod metadata;
mod native_authentication;
mod native_http;
mod protocol_observation;
mod provider;
mod resource_execution;
mod runtime_settings;
mod stored_session;
mod task_detail;
mod task_inventory;

pub use atomic_duration_completion::{
    WellearnAtomicDurationCompletion, WellearnAtomicDurationCompletionDocuments,
    WellearnAtomicDurationCompletionPlan, WellearnAtomicDurationCompletionReceipts,
    WellearnAtomicDurationCompletionTransport, WellearnAtomicDurationCompletionVerification,
    WellearnAtomicMutationKind, verify_atomic_duration_completion,
};
pub use atomic_recovery::{
    WELLEARN_ATOMIC_PRE_FINAL_OBSERVATION_TYPE, WellearnAtomicPreFinalObservation,
    verify_atomic_duration_completion_recovery,
};
pub use authentication::{
    WellearnAuthentication, WellearnAuthenticationTransport, WellearnCookieSession,
    WellearnLoginRedirect, WellearnPasswordCipher, WellearnSessionResolver,
    classify_password_login_response, encode_password_at,
};
pub use batch_plan::{
    WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE, WELLEARN_BATCH_PLAN_SNAPSHOT_TYPE,
    WellearnAtomicBatchPlanningAuthority, WellearnAtomicChildPlan, WellearnAtomicCompletionProfile,
    WellearnAutoDurationBudget, WellearnBatchDispatch, WellearnBatchEntry,
    WellearnBatchExecutionShape, WellearnBatchFlow, WellearnBatchPlan, WellearnBatchTargetStrategy,
    WellearnBatchUnitSelection, WellearnPreparedAtomicChildPlan, build_batch_plan,
    build_selected_batch_plan, materialize_atomic_child_plan,
    prepare_atomic_child_plan_from_fresh_inventory, validate_batch_plan_integrity,
    validate_fresh_batch_entry,
};
pub use cmi::{
    WellearnCmiDocument, WellearnCmiSnapshot, WellearnCmiTransport, WellearnTaskProgress,
    parse_cmi_snapshot,
};
pub use course_context::{WellearnCourseContext, parse_course_context};
pub use course_inventory::parse_course_inventory;
pub use duration_read::WellearnDurationRead;
pub use duration_report::{
    WellearnDurationReport, WellearnDurationReportDocuments, WellearnDurationReportPlan,
    WellearnDurationReportTransport,
};
pub use execution::WellearnTaskExecution;
pub use inventory_capabilities::{
    WellearnCourseInventory, WellearnCourseInventoryTransport, WellearnInventoryDocument,
    WellearnTaskInventory, WellearnTaskInventoryDocuments, WellearnTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_authentication::NativeWellearnAuthenticationTransport;
pub use native_http::NativeWellearnInventoryTransport;
pub use provider::{
    build_development_provider, build_development_provider_native,
    build_development_provider_with_native_inventory, build_development_provider_with_renewal,
    build_development_provider_with_stored_session,
};
pub use resource_execution::{
    WellearnResourceExecution, WellearnResourceExecutionDocuments, WellearnResourceExecutionPlan,
    WellearnResourceExecutionTransport,
};
pub use runtime_settings::{
    WellearnDurationProtocolMode, WellearnResourceCompletionCmiFormat,
    WellearnResourceCompletionSequence, WellearnResourceCompletionTimeMode,
    WellearnResourceCompletionWriteMode, WellearnResourceMutationProfile,
};
pub use stored_session::StoredWellearnSessionResolver;
pub use task_detail::WellearnTaskDetail;
pub use task_inventory::{WellearnScoLeavesDocument, parse_task_inventory};
pub use task_inventory::{WellearnUnitObservation, parse_unit_inventory};
