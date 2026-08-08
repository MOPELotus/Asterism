//! Pure orchestration state machines and policy guards.

mod assessment;
mod outbox;
mod scan;
mod scan_job;
mod transition;

pub use assessment::{
    AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action,
};
pub use outbox::{
    DeliveryError, DispatchConfig, DispatchError, DispatchReport, EventSink, OutboxDispatcher,
};
pub use scan::{ProviderAccountScanner, ProviderScanError, ProviderScanService};
pub use scan_job::{
    ScheduledScanFailure, ScheduledScanOutcome, ScheduledScanRunError, ScheduledScanRunner,
};
pub use transition::{
    ExecutionTransitionError, OrchestrationTransitionError, transition_execution,
    validate_orchestration_transition,
};
