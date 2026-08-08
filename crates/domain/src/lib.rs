//! Asterism's persistence- and transport-independent domain model.

pub mod account;
pub mod auth;
pub mod auth_bootstrap;
pub mod automation;
pub mod credits;
pub mod execution;
pub mod id;
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
pub use automation::{
    AutomationPlan, AutomationPlanStatus, BillingPolicy, CoverageSpec, ExecutionPolicy,
    InheritanceMode, PlanScope, SchedulePolicy,
};
pub use credits::{
    CreditAccount, CreditAmount, CreditError, CreditReservation, CreditReservationState,
    CreditTransaction, CreditTransactionType, PriceQuote,
};
pub use execution::{
    Execution, ExecutionAttempt, ExecutionLease, ExecutionLogEvent, ExecutionProgress,
    ExecutionStage, ExecutionState, LogLevel, RequestSource,
};
pub use id::*;
pub use task::{
    AssessmentClass, OrchestrationState, RemoteState, SourceType, Task, TaskCapability,
    TaskDiffKind, TaskSnapshot, classify_task_changes,
};
pub use user::{Permission, QqIdentity, Role, User, UserStatus};

/// UTC timestamp used by all persisted and externally visible domain objects.
pub type Timestamp = chrono::DateTime<chrono::Utc>;
