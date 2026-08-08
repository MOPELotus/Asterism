//! `SQLite` adapter for Asterism's repository boundary.

mod credit;
mod database;
mod lease;
mod outbox;
mod provider_account;
mod recovery;
mod repository;
mod scheduler;
mod session;
mod user;

pub use credit::{CreditGrant, SqliteCreditRepository};
pub use database::{Database, StorageError};
pub use lease::{LeaseAcquireOutcome, SqliteExecutionLeaseRepository};
pub use outbox::{FailureDisposition, OutboxHealth, OutboxRecord, SqliteOutboxRepository};
pub use provider_account::SqliteProviderAccountRepository;
pub use recovery::RecoveryReport;
pub use repository::{
    CreditRepository, ExecutionLeaseRepository, OutboxRepository, ProviderAccountRepository,
    SchedulerRepository, SessionRepository, TaskRepository, UserRepository,
};
pub use scheduler::{JobFailureDisposition, SqliteSchedulerRepository};
pub use session::SqliteSessionRepository;
pub use user::{InitialMaster, SqliteUserRepository};
