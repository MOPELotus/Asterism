//! Pure orchestration state machines and policy guards.

mod assessment;
mod auth_bootstrap;
mod auth_bootstrap_credential;
mod auth_session;
mod credential;
mod execution_job;
mod execution_request;
mod outbox;
mod scan;
mod scan_job;
mod scan_worker;
mod transition;

pub use assessment::{
    AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action,
};
pub use auth_bootstrap::{
    AuthBootstrapAccessRequest, AuthBootstrapCancelRequest, AuthBootstrapClaimRequest,
    AuthBootstrapClaimed, AuthBootstrapCreateRequest, AuthBootstrapCreated,
    AuthBootstrapEventAccepted, AuthBootstrapEventRequest, AuthBootstrapService,
    AuthBootstrapServiceError,
};
pub use auth_bootstrap_credential::{
    AuthBootstrapCredentialAccepted, AuthBootstrapCredentialRequest,
    AuthBootstrapCredentialService, AuthBootstrapCredentialServiceError,
};
pub use auth_session::{
    AuthSessionBegin, AuthSessionCredentialCommit, AuthSessionCredentialRequest,
    AuthSessionService, AuthSessionServiceError, AuthSessionStartRequest,
};
pub use credential::{CredentialCommit, CredentialProvisionError, ProviderCredentialService};
pub use execution_job::{
    ExecutionRunnerConfig, ScheduledExecutionOutcome, ScheduledExecutionRunError,
    ScheduledExecutionRunner,
};
pub use execution_request::{
    ExecuteTaskCommand, ExecutionRequestError, ExecutionRequestResult, ExecutionRequestService,
};
pub use outbox::{
    DeliveryError, DispatchConfig, DispatchError, DispatchReport, EventSink, OutboxDispatcher,
};
pub use scan::{ProviderAccountScanner, ProviderScanError, ProviderScanService};
pub use scan_job::{
    ScheduledScanFailure, ScheduledScanOutcome, ScheduledScanRunError, ScheduledScanRunner,
};
pub use scan_worker::{ScanSchedulerConfig, ScanSchedulerTickReport, ScanSchedulerWorker};
pub use transition::{
    ExecutionTransitionError, OrchestrationTransitionError, transition_execution,
    validate_orchestration_transition,
};
