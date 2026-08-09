//! Asterism's persistence- and transport-independent domain model.

pub mod account;
pub mod auth;
pub mod auth_bootstrap;
pub mod auth_bootstrap_event;
pub mod automation;
pub mod credits;
pub mod execution;
pub mod id;
pub mod question;
pub mod submission;
pub mod task;
pub mod user;

pub use account::{Course, ProviderAccount, ProviderId};
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
pub use credits::{
    CreditAccount, CreditAmount, CreditError, CreditReservation, CreditReservationState,
    CreditTransaction, CreditTransactionType, PriceQuote,
};
pub use execution::{
    AttemptResult, Execution, ExecutionAttempt, ExecutionLease, ExecutionLogEvent,
    ExecutionProgress, ExecutionStage, ExecutionState, LogLevel, ProviderErrorClass, RequestSource,
};
pub use id::*;
pub use question::{
    AnswerCandidate, AnswerConfidence, AnswerConfidenceError, AnswerPair, AnswerSource,
    NormalizedAnswer, Question, QuestionAttachment, QuestionAttachmentKind, QuestionKind,
    QuestionOption, QuestionValidationError,
};
pub use submission::{
    SelectedAnswer, SubmissionDraft, SubmissionDraftItem, SubmissionDraftValidationError,
    SubmissionPayloadEncoding, SubmissionPayloadFieldPreview, SubmissionPayloadPreview,
    SubmissionQuestionVerification, SubmissionQuestionVerificationStatus, SubmissionReceipt,
    SubmissionResult, SubmissionResultStatus, SubmissionResultValidationError, SubmissionScore,
    SubmissionVerificationSnapshot, SubmissionVerificationStatus,
};
pub use task::{
    AssessmentClass, OrchestrationState, RemoteState, SourceType, Task, TaskCapability,
    TaskDiffKind, TaskSnapshot, classify_task_changes,
};
pub use user::{Permission, QqIdentity, Role, User, UserStatus};

/// UTC timestamp used by all persisted and externally visible domain objects.
pub type Timestamp = chrono::DateTime<chrono::Utc>;
