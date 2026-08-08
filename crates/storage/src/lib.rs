//! `SQLite` adapter for Asterism's repository boundary.

mod database;
mod lease;
mod outbox;
mod recovery;
mod repository;

pub use database::{Database, StorageError};
pub use lease::{LeaseAcquireOutcome, SqliteExecutionLeaseRepository};
pub use outbox::{FailureDisposition, OutboxHealth, OutboxRecord, SqliteOutboxRepository};
pub use recovery::RecoveryReport;
pub use repository::{ExecutionLeaseRepository, OutboxRepository, TaskRepository, UserRepository};
