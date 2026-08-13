//! Pure orchestration state machines and policy guards.

mod answer_resolution;
mod answer_resolve;
mod assessment;
mod auth_bootstrap;
mod auth_bootstrap_credential;
mod auth_session;
mod credential;
mod execution_job;
mod execution_request;
mod execution_worker;
mod local_answer_cache;
mod manual_answer;
mod outbox;
mod question_read;
mod scan;
mod scan_job;
mod scan_worker;
mod submission_build;
mod task_browser;
mod task_detail;
mod task_duration;
mod task_lifecycle;
mod task_progress;
mod transition;

pub use answer_resolution::{
    ConservativeAnswerResolverError, ConservativeAnswerResolverService,
    ResolveAnswerCandidatesCommand,
};
pub use answer_resolve::{
    ProviderAnswerResolveError, ProviderAnswerResolveResult, ProviderAnswerResolveService,
    ResolveProviderAnswersCommand,
};
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
    ExternalOauthCallbackRequest,
};
pub use credential::{CredentialCommit, CredentialProvisionError, ProviderCredentialService};
pub use execution_job::{
    ExecutionRunnerConfig, ScheduledExecutionOutcome, ScheduledExecutionRunError,
    ScheduledExecutionRunner,
};
pub use execution_request::{
    ExecuteTaskCommand, ExecutionRequestError, ExecutionRequestResult, ExecutionRequestService,
};
pub use execution_worker::{
    ExecutionSchedulerConfig, ExecutionSchedulerTickReport, ExecutionSchedulerWorker,
    ExecutionSchedulerWorkerError,
};
pub use local_answer_cache::{
    ImportLocalAnswerCandidatesCommand, LocalAnswerCacheError, LocalAnswerCacheService,
};
pub use manual_answer::{
    CreateManualAnswerCandidateCommand, ManualAnswerCandidateError, ManualAnswerCandidateService,
};
pub use outbox::{
    DeliveryError, DispatchConfig, DispatchError, DispatchReport, EventSink, OutboxDispatcher,
};
pub use question_read::{
    ProviderQuestionReadError, ProviderQuestionReadResult, ProviderQuestionReadService,
    ReadTaskQuestionsCommand,
};
pub use scan::{ProviderAccountScanner, ProviderScanError, ProviderScanService};
pub use scan_job::{
    ScheduledScanFailure, ScheduledScanOutcome, ScheduledScanRunError, ScheduledScanRunner,
};
pub use scan_worker::{ScanSchedulerConfig, ScanSchedulerTickReport, ScanSchedulerWorker};
pub use submission_build::{
    BuildSubmissionDraftCommand, SubmissionDraftBuildError, SubmissionDraftBuildResult,
    SubmissionDraftBuildService,
};
pub use task_browser::{
    ProviderTaskBrowserSessionError, ProviderTaskBrowserSessionResult,
    ProviderTaskBrowserSessionService, ReadTaskBrowserSessionCommand,
};
pub use task_detail::{
    ProviderTaskDetailError, ProviderTaskDetailResult, ProviderTaskDetailService,
    ReadTaskDetailCommand,
};
pub use task_duration::{
    ProviderTaskDurationError, ProviderTaskDurationResult, ProviderTaskDurationService,
    ReadTaskDurationCommand,
};
pub use task_lifecycle::{
    TaskLifecycleCommand, TaskLifecycleError, TaskLifecycleResult, TaskLifecycleService,
};
pub use task_progress::{
    ProviderTaskProgressError, ProviderTaskProgressResult, ProviderTaskProgressService,
    ReadTaskProgressCommand,
};
pub use transition::{
    ExecutionTransitionError, OrchestrationTransitionError, transition_execution,
    validate_orchestration_transition,
};
