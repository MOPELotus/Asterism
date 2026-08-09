use asterism_auth::TokenDigest;
use asterism_domain::{
    AttemptResult, AuditActor, AuthBootstrapClientEvent, AuthBootstrapSession,
    AuthBootstrapSessionId, AuthSession, AuthSessionId, CreditAccount, CreditReservation,
    CreditReservationId, CreditTransactionId, Execution, ExecutionAttempt, ExecutionAttemptId,
    ExecutionId, ExecutionLease, ExecutionLogEvent, ExecutionProgress, ExecutionState,
    OrchestrationState, ProviderAccount, ProviderAccountId, ProviderErrorClass, ScheduleId,
    ServiceToken, ServiceTokenId, Task, TaskId, Timestamp, User, UserId, WebSession, WebSessionId,
};
use asterism_secrets::{CredentialBundle, ProviderCredential, SecretAccess, SecretStoreError};
use async_trait::async_trait;

use crate::StorageError;

use crate::JobFailureDisposition;
use crate::{CreditGrant, FailureDisposition, LeaseAcquireOutcome, OutboxRecord};

/// Persistence contract consumed by task services. It intentionally contains no
/// `SQLite` types.
#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn find_task(&self, id: TaskId) -> Result<Option<Task>, StorageError>;
    async fn save_task(&self, task: &Task) -> Result<(), StorageError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskPage {
    pub items: Vec<Task>,
    pub total: u64,
}

/// Owner-scoped, paginated read model for task surfaces.
#[async_trait]
pub trait TaskQueryRepository: Send + Sync {
    async fn list_owned_tasks(
        &self,
        owner_id: UserId,
        provider_account_id: Option<ProviderAccountId>,
        limit: u32,
        offset: u64,
    ) -> Result<TaskPage, StorageError>;

    async fn find_owned_task(
        &self,
        owner_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<Task>, StorageError>;
}

/// Internal Task lookup for an already authorized Scheduler execution.
#[async_trait]
pub trait TaskRuntimeRepository: Send + Sync {
    async fn find_runtime_task(&self, task_id: TaskId) -> Result<Option<Task>, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionDetail {
    pub execution: Execution,
    pub progress: Option<ExecutionProgress>,
    pub attempts: Vec<ExecutionAttempt>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionPage {
    pub items: Vec<Execution>,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionLogPage {
    pub items: Vec<ExecutionLogEvent>,
    pub total: u64,
}

/// Owner-scoped read model for execution status surfaces.
#[async_trait]
pub trait ExecutionQueryRepository: Send + Sync {
    async fn list_owned_executions(
        &self,
        owner_id: UserId,
        task_id: Option<TaskId>,
        limit: u32,
        offset: u64,
    ) -> Result<ExecutionPage, StorageError>;

    async fn find_owned_execution_detail(
        &self,
        owner_id: UserId,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionDetail>, StorageError>;

    async fn list_owned_execution_logs(
        &self,
        owner_id: UserId,
        execution_id: ExecutionId,
        limit: u32,
        offset: u64,
    ) -> Result<Option<ExecutionLogPage>, StorageError>;
}

/// Owner-scoped persistence contract for Provider account management.
#[async_trait]
pub trait ProviderAccountRepository: Send + Sync {
    async fn list_provider_accounts(
        &self,
        owner_id: UserId,
    ) -> Result<Vec<ProviderAccount>, StorageError>;

    async fn find_provider_account(
        &self,
        owner_id: UserId,
        account_id: ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StorageError>;

    async fn create_provider_account(
        &self,
        account: &ProviderAccount,
        actor: AuditActor,
    ) -> Result<(), StorageError>;

    async fn update_provider_account(
        &self,
        account: &ProviderAccount,
        actor: AuditActor,
    ) -> Result<bool, StorageError>;

    async fn delete_provider_account(
        &self,
        owner_id: UserId,
        account_id: ProviderAccountId,
        at: Timestamp,
        actor: AuditActor,
    ) -> Result<bool, StorageError>;
}

/// Internal lookup used by scheduler/runtime services after a job has already
/// been authorized and bound to one Provider account.
#[async_trait]
pub trait ProviderAccountRuntimeRepository: Send + Sync {
    async fn find_runtime_provider_account(
        &self,
        account_id: ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StorageError>;
}

/// Owner-scoped observable Provider authentication attempts.
#[async_trait]
pub trait AuthSessionRepository: Send + Sync {
    async fn create_auth_session(
        &self,
        session: &AuthSession,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError>;

    async fn find_auth_session(
        &self,
        owner_user_id: UserId,
        session_id: AuthSessionId,
    ) -> Result<Option<AuthSession>, StorageError>;

    async fn find_latest_account_auth_session(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
    ) -> Result<Option<AuthSession>, StorageError>;

    async fn update_auth_session(
        &self,
        session: &AuthSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;
}

/// Short-lived Capture pairing sessions with one-time token rotation at claim.
#[async_trait]
pub trait AuthBootstrapSessionRepository: Send + Sync {
    async fn create_auth_bootstrap_session(
        &self,
        session: &AuthBootstrapSession,
        pairing_token_digest: &TokenDigest,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError>;

    async fn find_auth_bootstrap_session(
        &self,
        owner_user_id: UserId,
        session_id: AuthBootstrapSessionId,
    ) -> Result<Option<AuthBootstrapSession>, StorageError>;

    async fn claim_auth_bootstrap_session(
        &self,
        session_id: AuthBootstrapSessionId,
        pairing_token_digest: &TokenDigest,
        access_token_digest: &TokenDigest,
        claimed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<Option<AuthBootstrapSession>, StorageError>;

    async fn authenticate_auth_bootstrap_access(
        &self,
        session_id: AuthBootstrapSessionId,
        access_token_digest: &TokenDigest,
        authenticated_at: Timestamp,
    ) -> Result<Option<AuthBootstrapSession>, StorageError>;

    async fn record_auth_bootstrap_client_event(
        &self,
        event: &AuthBootstrapClientEvent,
        access_token_digest: &TokenDigest,
        correlation_id: &str,
    ) -> Result<AuthBootstrapClientEventRecord, StorageError>;

    async fn update_auth_bootstrap_session_for_owner(
        &self,
        session: &AuthBootstrapSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthBootstrapClientEventRecord {
    Inserted(AuthBootstrapClientEvent),
    Duplicate(AuthBootstrapClientEvent),
    AccessRejected,
    SequenceConflict,
}

#[derive(Debug)]
pub struct AuthBootstrapCredentialCommitRequest<'a> {
    pub session_id: AuthBootstrapSessionId,
    pub access_token_digest: &'a TokenDigest,
    pub validated_account: ProviderAccount,
    pub bundle: CredentialBundle,
    pub completed_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthBootstrapCredentialCommit {
    pub session: AuthBootstrapSession,
    pub account: ProviderAccount,
    pub credentials: Vec<ProviderCredential>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthBootstrapCredentialCommitOutcome {
    Committed(Box<AuthBootstrapCredentialCommit>),
    AccessRejected,
    BindingConflict,
}

/// Atomic credential commit boundary for a claimed Capture pairing.
#[async_trait]
pub trait AuthBootstrapCredentialRepository: Send + Sync {
    async fn commit_auth_bootstrap_credentials(
        &self,
        request: AuthBootstrapCredentialCommitRequest<'_>,
    ) -> Result<AuthBootstrapCredentialCommitOutcome, SecretStoreError>;
}

/// Atomic commit boundary used after a candidate has passed Provider
/// validation inside one current authentication session.
#[async_trait]
pub trait AuthenticatedCredentialRepository: Send + Sync {
    async fn commit_authenticated_credentials(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        bundle: CredentialBundle,
        authenticated_session: &AuthSession,
        expected_session_revision: u32,
        access: &SecretAccess,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError>;
}

/// Persistence contract consumed by identity and authorization services.
#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn find_user(&self, id: UserId) -> Result<Option<User>, StorageError>;
    async fn find_user_by_username(&self, username: &str) -> Result<Option<User>, StorageError>;
    async fn save_user(&self, user: &User) -> Result<(), StorageError>;
}

#[async_trait]
pub trait ExecutionLeaseRepository: Send + Sync {
    async fn try_acquire(
        &self,
        lease: &ExecutionLease,
        now: Timestamp,
    ) -> Result<LeaseAcquireOutcome, StorageError>;

    async fn renew(
        &self,
        task_id: TaskId,
        execution_id: ExecutionId,
        worker_id: &str,
        now: Timestamp,
        new_expires_at: Timestamp,
    ) -> Result<ExecutionLease, StorageError>;

    async fn release(
        &self,
        task_id: TaskId,
        execution_id: ExecutionId,
        worker_id: &str,
    ) -> Result<bool, StorageError>;
}

#[derive(Clone, Debug)]
pub struct ExecutionScheduleRequest<'a> {
    pub execution: &'a Execution,
    pub expected_task_state: OrchestrationState,
    pub idempotency_scope: &'a str,
    pub idempotency_key: &'a str,
    pub actor: AuditActor,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionScheduleOutcome {
    Created(Execution),
    Existing(Execution),
    IdempotencyConflict,
    TaskStateConflict,
}

#[derive(Clone, Debug)]
pub struct ExecutionAttemptStartRequest<'a> {
    pub execution_id: ExecutionId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub at: Timestamp,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug)]
pub struct ExecutionProgressUpdate<'a> {
    pub progress: &'a ExecutionProgress,
    pub worker_id: &'a str,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug)]
pub struct ExecutionAttemptFinishRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub final_state: ExecutionState,
    pub result: AttemptResult,
    pub error_class: Option<ProviderErrorClass>,
    pub provider_trace_id: Option<&'a str>,
    pub retry_at: Option<Timestamp>,
    pub progress: &'a ExecutionProgress,
    pub at: Timestamp,
    pub correlation_id: &'a str,
}

/// Atomic execution request boundary. Creating the execution, moving the task
/// to `scheduled`, enqueuing the scheduler job, and recording audit/outbox
/// entries either all commit or all roll back.
#[async_trait]
pub trait ExecutionRepository: Send + Sync {
    async fn find_idempotent_execution(
        &self,
        idempotency_scope: &str,
        idempotency_key: &str,
    ) -> Result<Option<Execution>, StorageError>;

    async fn schedule_execution(
        &self,
        request: ExecutionScheduleRequest<'_>,
    ) -> Result<ExecutionScheduleOutcome, StorageError>;

    async fn find_execution(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<Execution>, StorageError>;

    async fn start_attempt(
        &self,
        request: ExecutionAttemptStartRequest<'_>,
    ) -> Result<ExecutionAttempt, StorageError>;

    async fn update_progress(
        &self,
        request: ExecutionProgressUpdate<'_>,
    ) -> Result<bool, StorageError>;

    async fn finish_attempt(
        &self,
        request: ExecutionAttemptFinishRequest<'_>,
    ) -> Result<Execution, StorageError>;
}

#[async_trait]
pub trait OutboxRepository: Send + Sync {
    async fn enqueue(&self, event: &asterism_events::EventEnvelope) -> Result<(), StorageError>;

    async fn claim_batch(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<OutboxRecord>, StorageError>;

    async fn mark_delivered(
        &self,
        event_id: asterism_domain::EventId,
        worker_id: &str,
        delivered_at: Timestamp,
    ) -> Result<(), StorageError>;

    async fn mark_failed(
        &self,
        event_id: asterism_domain::EventId,
        worker_id: &str,
        error_sanitized: &str,
        max_attempts: u32,
    ) -> Result<FailureDisposition, StorageError>;
}

#[async_trait]
pub trait CreditRepository: Send + Sync {
    async fn account(&self, user_id: UserId) -> Result<Option<CreditAccount>, StorageError>;

    async fn grant(&self, grant: &CreditGrant) -> Result<CreditAccount, StorageError>;

    async fn reserve(&self, reservation: &CreditReservation)
    -> Result<CreditAccount, StorageError>;

    async fn commit(
        &self,
        reservation_id: CreditReservationId,
        transaction_id: CreditTransactionId,
        at: Timestamp,
    ) -> Result<CreditAccount, StorageError>;

    async fn release(
        &self,
        reservation_id: CreditReservationId,
        at: Timestamp,
    ) -> Result<CreditAccount, StorageError>;
}

#[async_trait]
pub trait SchedulerRepository: Send + Sync {
    async fn enqueue(&self, job: &asterism_scheduler::ScheduledJob) -> Result<(), StorageError>;

    async fn claim_due(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<asterism_scheduler::ScheduledJob>, StorageError>;

    async fn claim_due_execution_jobs(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<asterism_scheduler::ScheduledJob>, StorageError>;

    async fn renew_claim(
        &self,
        job_id: asterism_domain::ScheduleId,
        worker_id: &str,
        now: Timestamp,
        new_expires_at: Timestamp,
    ) -> Result<(), StorageError>;

    async fn complete(
        &self,
        job_id: asterism_domain::ScheduleId,
        worker_id: &str,
        at: Timestamp,
    ) -> Result<(), StorageError>;

    async fn fail(
        &self,
        job_id: asterism_domain::ScheduleId,
        worker_id: &str,
        error_sanitized: &str,
        retry_at: Option<Timestamp>,
        at: Timestamp,
    ) -> Result<JobFailureDisposition, StorageError>;
}

#[async_trait]
pub trait ScanScheduleRepository: Send + Sync {
    async fn upsert_scan_schedule(
        &self,
        schedule: &asterism_scheduler::ScanSchedule,
    ) -> Result<asterism_scheduler::ScanSchedule, StorageError>;

    async fn upsert_scan_schedule_for_owner(
        &self,
        owner_id: UserId,
        schedule: &asterism_scheduler::ScanSchedule,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<Option<asterism_scheduler::ScanSchedule>, StorageError>;

    async fn find_scan_schedule(
        &self,
        account_id: ProviderAccountId,
    ) -> Result<Option<asterism_scheduler::ScanSchedule>, StorageError>;

    async fn materialize_due_scan_jobs(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<asterism_scheduler::ScheduledJob>, StorageError>;

    async fn claim_due_scan_jobs(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<asterism_scheduler::ScheduledJob>, StorageError>;
}

#[async_trait]
pub trait SessionRepository: Send + Sync {
    async fn create_web_session(
        &self,
        session: &WebSession,
        token_digest: &TokenDigest,
        actor: AuditActor,
    ) -> Result<(), StorageError>;

    async fn authenticate_web_session(
        &self,
        token_digest: &TokenDigest,
        now: Timestamp,
    ) -> Result<Option<(WebSession, User)>, StorageError>;

    async fn revoke_web_session(
        &self,
        session_id: WebSessionId,
        at: Timestamp,
        actor: AuditActor,
    ) -> Result<bool, StorageError>;

    async fn create_service_token(
        &self,
        token: &ServiceToken,
        token_digest: &TokenDigest,
        actor: AuditActor,
    ) -> Result<(), StorageError>;

    async fn authenticate_service_token(
        &self,
        token_digest: &TokenDigest,
        now: Timestamp,
    ) -> Result<Option<ServiceToken>, StorageError>;

    async fn revoke_service_token(
        &self,
        token_id: ServiceTokenId,
        at: Timestamp,
        actor: AuditActor,
    ) -> Result<bool, StorageError>;
}
