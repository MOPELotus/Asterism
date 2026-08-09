//! `SQLite` adapter for Asterism's repository boundary.

mod auth_bootstrap;
mod auth_session;
mod credit;
mod database;
mod execution;
mod lease;
mod outbox;
mod provider_account;
mod recovery;
mod repository;
mod scan;
mod scheduler;
mod secret;
mod session;
mod task;
mod user;

pub use auth_bootstrap::SqliteAuthBootstrapSessionRepository;
pub use auth_session::SqliteAuthSessionRepository;
pub use credit::{CreditGrant, SqliteCreditRepository};
pub use database::{Database, StorageError};
pub use execution::SqliteExecutionRepository;
pub use lease::{LeaseAcquireOutcome, SqliteExecutionLeaseRepository};
pub use outbox::{FailureDisposition, OutboxHealth, OutboxRecord, SqliteOutboxRepository};
pub use provider_account::SqliteProviderAccountRepository;
pub use recovery::RecoveryReport;
pub use repository::{
    AuthBootstrapClientEventRecord, AuthBootstrapCredentialCommit,
    AuthBootstrapCredentialCommitOutcome, AuthBootstrapCredentialCommitRequest,
    AuthBootstrapCredentialRepository, AuthBootstrapSessionRepository, AuthSessionRepository,
    AuthenticatedCredentialRepository, CreditRepository, ExecutionAttemptFinishRequest,
    ExecutionAttemptStartRequest, ExecutionDetail, ExecutionLeaseRepository, ExecutionLogPage,
    ExecutionPage, ExecutionProgressUpdate, ExecutionQueryRepository,
    ExecutionRecoveryFinishRequest, ExecutionRepository, ExecutionScheduleOutcome,
    ExecutionScheduleRequest, OutboxRepository, ProviderAccountRepository,
    ProviderAccountRuntimeRepository, ScanScheduleRepository, SchedulerRepository,
    SessionRepository, TaskPage, TaskQueryRepository, TaskRepository, TaskRuntimeRepository,
    UserRepository,
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
pub use user::{InitialMaster, SqliteUserRepository};
