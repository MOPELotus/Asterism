//! `SQLite` adapter for Asterism's repository boundary.

mod admin;
mod answer_evidence;
mod answer_harvest;
mod answer_history_ingest;
mod auth_bootstrap;
mod auth_session;
mod browser_bridge;
mod browser_bridge_command;
mod credit;
mod database;
mod execution;
mod external_oauth;
mod lease;
mod outbox;
mod provider_account;
mod provider_runtime_settings;
mod question;
mod question_artifact;
mod question_read_attempt;
mod question_read_continuation;
mod question_session;
mod recovery;
mod repository;
mod scan;
mod scheduler;
mod secret;
mod session;
mod task;
mod task_lifecycle;
mod user;

pub use admin::SqliteAdminRepository;
pub use answer_evidence::SqliteAnswerEvidenceRepository;
pub use answer_harvest::SqliteAnswerBootstrapHarvestRepository;
pub use answer_history_ingest::SqliteAnswerHistoryIngestionRepository;
pub use auth_bootstrap::SqliteAuthBootstrapSessionRepository;
pub use auth_session::SqliteAuthSessionRepository;
pub use browser_bridge::SqliteBrowserBridgeSessionRepository;
pub use browser_bridge_command::SqliteBrowserBridgeCommandArtifactRepository;
pub use credit::{CreditGrant, CreditGrantOutcome, CreditGrantResult, SqliteCreditRepository};
pub use database::{Database, StorageError};
pub use execution::SqliteExecutionRepository;
pub use lease::{LeaseAcquireOutcome, SqliteExecutionLeaseRepository};
pub use outbox::{FailureDisposition, OutboxHealth, OutboxRecord, SqliteOutboxRepository};
pub use provider_account::SqliteProviderAccountRepository;
pub use provider_runtime_settings::SqliteProviderRuntimeSettingsRepository;
pub use question::SqliteQuestionSnapshotRepository;
pub use question_artifact::SqliteQuestionSessionArtifactRepository;
pub use question_read_attempt::SqliteQuestionReadAttemptRepository;
pub use question_read_continuation::SqliteQuestionReadContinuationRepository;
pub use question_session::SqliteQuestionSessionRepository;
pub use recovery::RecoveryReport;
pub use repository::{
    AnswerBootstrapHarvestCheckpoint, AnswerBootstrapHarvestCompletion,
    AnswerBootstrapHarvestFailure, AnswerBootstrapHarvestRepository, AnswerBootstrapHarvestYield,
    AnswerCacheRepository, AnswerCandidateRecord, AnswerCandidateRepository,
    AnswerEvidenceProjectionState, AnswerEvidenceRecord, AnswerEvidenceRecordOutcome,
    AnswerEvidenceRepository, AnswerHistoryImportRecord, AnswerHistoryIngestOutcome,
    AnswerHistoryIngestRequest, AnswerHistoryIngestionRepository, AuditFilter, AuditPage,
    AuditQueryRepository, AuthBootstrapClientEventRecord, AuthBootstrapCredentialCommit,
    AuthBootstrapCredentialCommitOutcome, AuthBootstrapCredentialCommitRequest,
    AuthBootstrapCredentialRepository, AuthBootstrapSessionRepository, AuthSessionRepository,
    AuthenticatedCredentialRepository, BrowserBridgeCommandArtifactRepository,
    BrowserBridgeCommandDispatchRecord, BrowserBridgeCommandDispatchRequest,
    BrowserBridgeCommandIssueRequest, BrowserBridgeCommandResolveRequest,
    BrowserBridgeCredentialCommit, BrowserBridgeCredentialCommitOutcome,
    BrowserBridgeCredentialCommitRequest, BrowserBridgeCredentialRepository,
    BrowserBridgeExchangeRecord, BrowserBridgeResultArtifactRecord,
    BrowserBridgeResultAttemptFinishRequest, BrowserBridgeResultReceiveRequest,
    BrowserBridgeResultResolveRequest, BrowserBridgeRuntimeBindingRecord,
    BrowserBridgeRuntimeStateIssue, BrowserBridgeSessionRepository,
    BrowserBridgeWorkflowCommitOutcome, BrowserBridgeWorkflowCommitRequest,
    BrowserBridgeWorkflowContextIssue, BrowserBridgeWorkflowPlanIssue,
    ClaimedAnswerBootstrapHarvest, CreditQueryRepository, CreditRepository,
    CreditReservationDetail, CreditReservationPage, CreditTransactionPage,
    DispatchedBrowserBridgeCommand, ExecutionAtomicMutation, ExecutionAtomicMutationIssueOutcome,
    ExecutionAtomicMutationIssueRequest, ExecutionAtomicMutationReceiptOutcome,
    ExecutionAtomicMutationReceiptRequest, ExecutionAtomicMutationRepository,
    ExecutionAttemptFinishRequest, ExecutionAttemptStartRequest, ExecutionBillingReservation,
    ExecutionCapabilityCallMutation, ExecutionCapabilityStep, ExecutionCapabilityStepIssueOutcome,
    ExecutionCapabilityStepMutation, ExecutionCapabilityStepRepository,
    ExecutionCapabilityStepState, ExecutionDetail, ExecutionLeaseRepository,
    ExecutionLogAppendRequest, ExecutionLogPage, ExecutionPage, ExecutionProgressUpdate,
    ExecutionQueryRepository, ExecutionQuestionStepFinishRequest, ExecutionRecoveryFinishRequest,
    ExecutionRepository, ExecutionRuntimeSettingsResolution, ExecutionRuntimeSettingsSnapshot,
    ExecutionScheduleOutcome, ExecutionScheduleRequest, ExecutionSubmissionRepository,
    ExecutionVerificationRecoveryRepository, ExternalOauthClaim, GlobalAnswerCorpusEvidence,
    OutboxRepository, PendingBrowserBridgeResult, PriorAnswerEvidence, ProviderAccountRepository,
    ProviderAccountRuntimeRepository, ProviderRuntimeSettingsRecord,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget,
    ProviderRuntimeSettingsWriteOutcome, ProviderRuntimeSettingsWriteRequest,
    QuestionReadAttemptRepository, QuestionReadContinuation, QuestionReadContinuationAttachRequest,
    QuestionReadContinuationRepository, QuestionReadContinuationRepositoryFactory,
    QuestionReadMaterializeOutcome, QuestionReadMaterializeRequest, QuestionReadOperation,
    QuestionReadOperationAcceptRequest, QuestionReadOperationFinishOutcome,
    QuestionReadOperationIssueOutcome, QuestionReadOperationIssueRequest,
    QuestionReadOperationState, QuestionSessionArtifactAttachRequest,
    QuestionSessionArtifactRepository, QuestionSessionArtifactRepositoryFactory,
    QuestionSessionClaimOutcome, QuestionSessionContinuation, QuestionSessionMaterializeRequest,
    QuestionSessionNextMaterializeOutcome, QuestionSessionNextMaterializeRequest,
    QuestionSessionOperation, QuestionSessionOperationAcceptRequest,
    QuestionSessionOperationFinishOutcome, QuestionSessionOperationIssueOutcome,
    QuestionSessionOperationIssueRequest, QuestionSessionOperationState, QuestionSessionRepository,
    QuestionSessionTransition, QuestionSnapshot, QuestionSnapshotRepository,
    ResolvedBrowserBridgeCommand, ResolvedBrowserBridgeResult, ResolvedBrowserBridgeRuntimeState,
    ResolvedBrowserBridgeWorkflowContext, ResolvedBrowserBridgeWorkflowPlan,
    ResolvedQuestionReadContinuation, ResolvedQuestionSessionContinuation, ScanScheduleRepository,
    SchedulerRepository, ServiceTokenPage, ServiceTokenQueryRepository, SessionRepository,
    SubmissionDraftRepository, SubmissionReceiptPersistRequest, SubmissionResultPersistRequest,
    SubmissionResultRepository, TaskLifecycleMutation, TaskLifecycleMutationOutcome,
    TaskLifecycleReceipt, TaskLifecycleRepository, TaskPage, TaskQueryRepository, TaskRepository,
    TaskRuntimeRepository, UserAdminCreate, UserAdminCreateOutcome, UserAdminRepository,
    UserAdminUpdate, UserAdminUpdateOutcome, UserProfilePage, UserRepository,
    VerificationRecoveryStartRequest,
};
pub use scan::{
    ProviderScanBatch, ProviderScanReport, ProviderScanRepository, ScannedCourse, ScannedTask,
    SqliteProviderScanRepository, TaskScanChange,
};
pub use scheduler::{JobFailureDisposition, SqliteSchedulerRepository};
pub use secret::{
    SecretKeyring, SecretKeyringError, SqliteProviderCredentialResolver, SqliteSecretStore,
};
pub use session::SqliteSessionRepository;
pub use task::SqliteTaskQueryRepository;
pub use task_lifecycle::SqliteTaskLifecycleRepository;
pub use user::{InitialMaster, SqliteUserRepository};
