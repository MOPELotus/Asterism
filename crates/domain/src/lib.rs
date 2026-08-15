//! Asterism's persistence- and transport-independent domain model.

pub mod account;
pub mod answer_evidence;
pub mod answer_resolution;
pub mod audit;
pub mod auth;
pub mod auth_bootstrap;
pub mod auth_bootstrap_event;
pub mod automation;
pub mod browser_bridge;
pub mod browser_bridge_exchange;
pub mod completion;
pub mod credits;
pub mod execution;
pub mod external_oauth;
pub mod id;
pub mod question;
pub mod question_read_attempt;
pub mod question_session;
pub mod submission;
pub mod task;
pub mod user;

pub use account::{
    AccountHealth, AccountHealthState, Course, CourseAggregateProgress,
    CourseAggregateProgressError, CourseDurationProgress, CourseRequiredProgress,
    CourseScoreProgress, ProviderAccount, ProviderId,
};
pub use answer_evidence::{
    AnswerBootstrapHarvest, AnswerBootstrapHarvestState, AnswerBootstrapHarvestValidationError,
    AnswerEvidenceClass, CorpusProjectionEligibility, GlobalCorpusQuestionAsset,
    GlobalCorpusQuestionOption, GlobalSemanticAnswer, PrivateAnswerEvidence,
    PrivateAnswerEvidenceValidationError, UnmatchedEvidenceReason,
};
pub use answer_resolution::{
    AnswerResolutionDecision, AnswerResolutionPlan, AnswerResolutionStatus,
    AnswerResolutionValidationError,
};
pub use audit::AuditRecord;
pub use auth::{
    AuditActor, AuthMethod, AuthSession, AuthSessionError, AuthState, HumanRequiredReason,
    ServiceScope, ServiceToken, SessionKind, WaitingUserState, WebSession,
};
pub use auth_bootstrap::{
    AuthBootstrapPurpose, AuthBootstrapSession, AuthBootstrapSessionError, AuthBootstrapState,
    MAX_AUTH_BOOTSTRAP_TTL_SECONDS,
};
pub use auth_bootstrap_event::{
    AuthBootstrapClientEvent, AuthBootstrapClientEventError, AuthBootstrapClientEventKind,
};
pub use automation::{
    AutomationPlan, AutomationPlanStatus, BillingPolicy, CoverageSpec, ExecutionPolicy,
    InheritanceMode, PlanScope, SchedulePolicy,
};
pub use browser_bridge::{
    BrowserBridgeRuntimeBinding, BrowserBridgeRuntimeBindingError, BrowserBridgeSession,
    BrowserBridgeSessionCreate, BrowserBridgeSessionError, BrowserBridgeSessionState,
    MAX_BROWSER_BRIDGE_SESSION_TTL_SECONDS,
};
pub use browser_bridge_exchange::{
    BrowserBridgeExchange, BrowserBridgeExchangeError, BrowserBridgeExchangeState,
    BrowserBridgeResultArtifactMetadata, BrowserBridgeRuntimeStateMetadata,
};
pub use completion::{
    CompletionDiagnosis, CompletionOutcome, CompletionPolicySnapshot,
    CompletionPolicyValidationError, CompletionWorkflowBinding, RetakeScorePolicy,
    ScoreImprovementState, ScoreImprovementWorkflow, ScoreImprovementWorkflowError,
    StrictCompletionState, StrictCompletionWorkflow, StrictCompletionWorkflowError,
    VerifiedCompletionBaseline,
};
pub use credits::{
    CreditAccount, CreditAmount, CreditError, CreditReservation, CreditReservationState,
    CreditTransaction, CreditTransactionType, PriceQuote,
};
pub use execution::{
    AttemptResult, Execution, ExecutionAttempt, ExecutionLease, ExecutionLogEvent,
    ExecutionProgress, ExecutionStage, ExecutionState, LogLevel, ProviderErrorClass, RequestSource,
};
pub use external_oauth::{
    ExternalOauthPending, ExternalOauthPendingCreate, ExternalOauthPendingError,
    ExternalOauthState, MAX_EXTERNAL_OAUTH_TTL_SECONDS,
};
pub use id::*;
pub use question::{
    AnswerCandidate, AnswerConfidence, AnswerConfidenceError, AnswerPair, AnswerSource,
    NormalizedAnswer, Question, QuestionAttachment, QuestionAttachmentKind,
    QuestionContentFingerprint, QuestionKind, QuestionOption, QuestionValidationError,
};
pub use question_read_attempt::{
    MAX_QUESTION_READ_ATTEMPT_TTL_SECONDS, QuestionReadAttempt, QuestionReadAttemptError,
    QuestionReadAttemptState,
};
pub use question_session::{
    MAX_QUESTION_SESSION_TTL_SECONDS, QuestionSession, QuestionSessionError, QuestionSessionState,
};
pub use submission::{
    SelectedAnswer, SubmissionAnswerCoverage, SubmissionAttemptReceipt, SubmissionDraft,
    SubmissionDraftItem, SubmissionDraftValidationError, SubmissionPayloadEncoding,
    SubmissionPayloadFieldPreview, SubmissionPayloadPreview, SubmissionQuestionVerification,
    SubmissionQuestionVerificationStatus, SubmissionReceipt, SubmissionResult,
    SubmissionResultStatus, SubmissionResultValidationError, SubmissionScore,
    SubmissionVerificationSnapshot, SubmissionVerificationStatus,
};
pub use task::{
    AssessmentClass, OrchestrationState, RemoteState, SourceType, Task, TaskCapability,
    TaskDiffKind, TaskLifecycleAction, TaskSnapshot, classify_task_changes,
};
pub use user::{Permission, QqIdentity, Role, User, UserProfile, UserStatus};

/// UTC timestamp used by all persisted and externally visible domain objects.
pub type Timestamp = chrono::DateTime<chrono::Utc>;
