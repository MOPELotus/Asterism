//! Pure orchestration state machines and policy guards.

mod assessment;
mod outbox;
mod transition;

pub use assessment::{
    AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action,
};
pub use outbox::{
    DeliveryError, DispatchConfig, DispatchError, DispatchReport, EventSink, OutboxDispatcher,
};
pub use transition::{
    ExecutionTransitionError, OrchestrationTransitionError, transition_execution,
    validate_orchestration_transition,
};
