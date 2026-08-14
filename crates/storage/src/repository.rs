use std::{collections::BTreeMap, sync::Arc};

use asterism_auth::TokenDigest;
use asterism_domain::{
    AnswerCandidate, AnswerCandidateId, AttemptResult, AuditActor, AuditRecord,
    AuthBootstrapClientEvent, AuthBootstrapSession, AuthBootstrapSessionId, AuthSession,
    AuthSessionId, BrowserBridgeExchange, BrowserBridgeSession, BrowserBridgeSessionId,
    CreditAccount, CreditReservation, CreditReservationId, CreditTransaction, CreditTransactionId,
    Execution, ExecutionAttempt, ExecutionAttemptId, ExecutionId, ExecutionLease,
    ExecutionLogEvent, ExecutionProgress, ExecutionStage, ExecutionState, ExternalOauthPending,
    LogLevel, OrchestrationState, PriceQuote, ProviderAccount, ProviderAccountId,
    ProviderErrorClass, ProviderId, ProviderRuntimeSettingsId, Question,
    QuestionContentFingerprint, QuestionReadAttempt, QuestionReadAttemptId, QuestionSession,
    QuestionSnapshotId, ScheduleId, ServiceToken, ServiceTokenId, SubmissionAttemptReceipt,
    SubmissionDraft, SubmissionDraftId, SubmissionResult, SubmissionResultId, Task,
    TaskActionReceiptId, TaskCapability, TaskId, TaskLifecycleAction, Timestamp, User, UserId,
    UserProfile, UserStatus, WebSession, WebSessionId,
};
use asterism_provider_api::{
    BrowserSessionSpec, ProviderRuntimeSettingSource, ProviderRuntimeSettingsPatch,
    ProviderRuntimeSettingsSchema, ProviderSettingScope, ResolvedProviderRuntimeSettings,
};
use asterism_secrets::{
    CredentialBundle, ProviderCredential, SecretAccess, SecretStoreError, SecretValue,
};
use async_trait::async_trait;

use crate::StorageError;

use crate::JobFailureDisposition;
use crate::{
    CreditGrant, CreditGrantOutcome, FailureDisposition, LeaseAcquireOutcome, OutboxRecord,
};

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
pub struct TaskLifecycleReceipt {
    pub id: TaskActionReceiptId,
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub action: TaskLifecycleAction,
    pub idempotency_key: String,
    pub delayed_until: Option<Timestamp>,
    pub result_task_state: OrchestrationState,
    pub affected_execution_id: Option<ExecutionId>,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct TaskLifecycleMutation<'a> {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub action: TaskLifecycleAction,
    pub expected_task_state: OrchestrationState,
    pub target_task_state: OrchestrationState,
    pub delayed_until: Option<Timestamp>,
    pub request_source: asterism_domain::RequestSource,
    pub actor: AuditActor,
    pub idempotency_key: &'a str,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskLifecycleMutationOutcome {
    Applied(TaskLifecycleReceipt),
    Existing(TaskLifecycleReceipt),
    IdempotencyConflict,
    TaskNotFound,
    StateConflict,
}

/// Atomic owner-scoped Task lifecycle control, including any pending
/// Execution, scheduler job, credit reservation, audit and outbox effects.
#[async_trait]
pub trait TaskLifecycleRepository: Send + Sync {
    async fn find_task_lifecycle_receipt(
        &self,
        owner_id: UserId,
        idempotency_key: &str,
    ) -> Result<Option<TaskLifecycleReceipt>, StorageError>;

    async fn apply_task_lifecycle_mutation(
        &self,
        mutation: TaskLifecycleMutation<'_>,
    ) -> Result<TaskLifecycleMutationOutcome, StorageError>;
}

/// One immutable, complete Question parse captured from a single fresh Provider
/// read. Provider route material and answers are intentionally absent.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestionSnapshot {
    pub id: QuestionSnapshotId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub captured_at: Timestamp,
    pub questions: Vec<Question>,
}

/// Transactional Question snapshot persistence and owner-scoped latest read.
#[async_trait]
pub trait QuestionSnapshotRepository: Send + Sync {
    async fn save_question_snapshot(&self, snapshot: &QuestionSnapshot)
    -> Result<(), StorageError>;

    async fn find_owned_question_snapshot(
        &self,
        owner_id: UserId,
        question_snapshot_id: QuestionSnapshotId,
    ) -> Result<Option<QuestionSnapshot>, StorageError>;

    async fn find_latest_owned_question_snapshot(
        &self,
        owner_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<QuestionSnapshot>, StorageError>;
}

/// Durable pre-Question start mutation selected by exact owner and Task.
#[async_trait]
pub trait QuestionReadAttemptRepository: Send + Sync {
    async fn create_question_read_attempt(
        &self,
        attempt: &QuestionReadAttempt,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError>;

    async fn find_owned_question_read_attempt(
        &self,
        owner_user_id: UserId,
        attempt_id: QuestionReadAttemptId,
    ) -> Result<Option<QuestionReadAttempt>, StorageError>;

    async fn find_latest_owned_question_read_attempt(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<QuestionReadAttempt>, StorageError>;

    async fn update_question_read_attempt(
        &self,
        attempt: &QuestionReadAttempt,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionReadContinuation {
    pub attempt_id: QuestionReadAttemptId,
    pub continuation_type: String,
    pub continuation_digest: [u8; 32],
    pub phase: String,
    pub revision: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug)]
pub struct ResolvedQuestionReadContinuation {
    pub metadata: QuestionReadContinuation,
    pub latest_operation: Option<QuestionReadOperation>,
    pub value: SecretValue,
}

#[derive(Debug)]
pub struct QuestionReadContinuationAttachRequest<'a> {
    pub attempt_id: QuestionReadAttemptId,
    pub continuation_type: &'a str,
    pub phase: &'a str,
    pub value: SecretValue,
    pub attached_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestionReadOperationState {
    Issued,
    Accepted,
    Rejected,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionReadOperation {
    pub attempt_id: QuestionReadAttemptId,
    pub sequence: u64,
    pub continuation_revision: u32,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub state: QuestionReadOperationState,
    pub result_digest: Option<[u8; 32]>,
    pub issued_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Clone, Debug)]
pub struct QuestionReadOperationIssueRequest<'a> {
    pub attempt_id: QuestionReadAttemptId,
    pub expected_continuation_revision: u32,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub issued_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionReadOperationIssueOutcome {
    Issued(QuestionReadOperation),
    Duplicate(QuestionReadOperation),
    Conflict,
    Unavailable,
}

#[derive(Debug)]
pub struct QuestionReadOperationAcceptRequest<'a> {
    pub operation: &'a QuestionReadOperation,
    pub next_continuation_type: &'a str,
    pub next_phase: &'a str,
    pub replacement: SecretValue,
    pub result_digest: [u8; 32],
    pub accepted_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct QuestionReadMaterializeRequest<'a> {
    pub operation: &'a QuestionReadOperation,
    pub snapshot: &'a QuestionSnapshot,
    pub session: &'a QuestionSession,
    pub artifact_phase: &'a str,
    pub artifact: SecretValue,
    pub result_digest: [u8; 32],
    pub materialized_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionReadOperationFinishOutcome {
    Accepted {
        operation: QuestionReadOperation,
        continuation: QuestionReadContinuation,
        attempt: QuestionReadAttempt,
    },
    Finished {
        operation: QuestionReadOperation,
        attempt: QuestionReadAttempt,
    },
    Duplicate(QuestionReadOperation),
    Conflict,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionReadMaterializeOutcome {
    Materialized {
        operation: QuestionReadOperation,
        attempt: QuestionReadAttempt,
        session: QuestionSession,
    },
    Duplicate {
        operation: QuestionReadOperation,
        attempt: QuestionReadAttempt,
        session: QuestionSession,
    },
    Conflict,
    Unavailable,
}

/// Provider-scoped encrypted state and ambiguity ledger for operations that
/// occur before the first immutable Question snapshot exists.
#[async_trait]
pub trait QuestionReadContinuationRepository: Send + Sync {
    async fn attach_question_read_continuation(
        &self,
        request: QuestionReadContinuationAttachRequest<'_>,
    ) -> Result<QuestionReadContinuation, SecretStoreError>;

    async fn resolve_question_read_continuation(
        &self,
        attempt_id: QuestionReadAttemptId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedQuestionReadContinuation>, SecretStoreError>;

    async fn issue_question_read_operation(
        &self,
        request: QuestionReadOperationIssueRequest<'_>,
    ) -> Result<QuestionReadOperationIssueOutcome, SecretStoreError>;

    async fn accept_question_read_operation(
        &self,
        request: QuestionReadOperationAcceptRequest<'_>,
    ) -> Result<QuestionReadOperationFinishOutcome, SecretStoreError>;

    async fn materialize_question_read_operation(
        &self,
        request: QuestionReadMaterializeRequest<'_>,
    ) -> Result<QuestionReadMaterializeOutcome, SecretStoreError>;

    /// `Accepted` records a definite completed-before-first-Question
    /// response; continuing responses use `accept_question_read_operation`.
    async fn finish_question_read_operation(
        &self,
        operation: &QuestionReadOperation,
        terminal_state: QuestionReadOperationState,
        result_digest: Option<[u8; 32]>,
        completed_at: Timestamp,
        access: &SecretAccess,
    ) -> Result<QuestionReadOperationFinishOutcome, SecretStoreError>;
}

/// Creates a Provider-scoped continuation boundary from a Provider identity
/// selected through Core-owned account binding.
pub trait QuestionReadContinuationRepositoryFactory: Send + Sync {
    fn for_provider(&self, provider_id: ProviderId) -> Arc<dyn QuestionReadContinuationRepository>;
}

/// Result of atomically claiming the Question session selected by one
/// Execution's immutable `SubmissionDraft`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionSessionClaimOutcome {
    Claimed(QuestionSession),
    Existing(QuestionSession),
    Expired(QuestionSession),
    NotFound,
    BindingConflict,
    StateConflict(QuestionSession),
}

/// Durable owner/account/Task/Snapshot binding for provider attempt material.
/// Provider-specific bytes and secrets are deliberately handled by a separate
/// encrypted artifact boundary.
#[async_trait]
pub trait QuestionSessionRepository: Send + Sync {
    async fn create_question_session(
        &self,
        session: &QuestionSession,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError>;

    async fn find_owned_question_session(
        &self,
        owner_user_id: UserId,
        session_id: asterism_domain::QuestionSessionId,
    ) -> Result<Option<QuestionSession>, StorageError>;

    async fn find_question_session_for_execution(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<QuestionSession>, StorageError>;

    async fn claim_question_session_for_execution(
        &self,
        execution_id: ExecutionId,
        claimed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<QuestionSessionClaimOutcome, StorageError>;

    async fn update_question_session(
        &self,
        session: &QuestionSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionSessionContinuation {
    pub session_id: asterism_domain::QuestionSessionId,
    pub execution_id: Option<ExecutionId>,
    pub continuation_type: String,
    pub continuation_digest: [u8; 32],
    pub phase: String,
    pub revision: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug)]
pub struct ResolvedQuestionSessionContinuation {
    pub metadata: QuestionSessionContinuation,
    pub latest_operation: Option<QuestionSessionOperation>,
    pub value: SecretValue,
}

#[derive(Debug)]
pub struct QuestionSessionArtifactAttachRequest<'a> {
    pub session_id: asterism_domain::QuestionSessionId,
    pub phase: &'a str,
    pub value: SecretValue,
    pub attached_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct QuestionSessionMaterializeRequest<'a> {
    pub snapshot: &'a QuestionSnapshot,
    pub session: &'a QuestionSession,
    pub artifact_phase: &'a str,
    pub artifact: SecretValue,
    pub materialized_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestionSessionOperationState {
    Issued,
    Accepted,
    Rejected,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionSessionOperation {
    pub session_id: asterism_domain::QuestionSessionId,
    pub sequence: u64,
    pub execution_id: ExecutionId,
    pub execution_attempt_id: ExecutionAttemptId,
    pub continuation_revision: u32,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub state: QuestionSessionOperationState,
    pub result_digest: Option<[u8; 32]>,
    pub issued_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionSessionOperationIssueOutcome {
    Issued(QuestionSessionOperation),
    Duplicate(QuestionSessionOperation),
    Conflict,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct QuestionSessionOperationIssueRequest {
    pub execution_id: ExecutionId,
    pub execution_attempt_id: ExecutionAttemptId,
    pub expected_continuation_revision: u32,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub issued_at: Timestamp,
    pub correlation_id: String,
}

#[derive(Debug)]
pub struct QuestionSessionOperationAcceptRequest<'a> {
    pub operation: &'a QuestionSessionOperation,
    pub next_continuation_type: &'a str,
    pub next_phase: &'a str,
    pub replacement: SecretValue,
    pub result_digest: [u8; 32],
    pub accepted_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionSessionOperationFinishOutcome {
    Accepted {
        operation: QuestionSessionOperation,
        continuation: QuestionSessionContinuation,
    },
    Finished(QuestionSessionOperation),
    Duplicate(QuestionSessionOperation),
    Conflict,
    Unavailable,
}

/// Encrypted provider continuation and per-mutation ambiguity ledger. The
/// implementation is permanently Provider-scoped at composition time.
#[async_trait]
pub trait QuestionSessionArtifactRepository: Send + Sync {
    /// Atomically persists one ordinary read-only Question snapshot, its
    /// unclaimed session and encrypted Provider artifact.
    async fn materialize_question_session(
        &self,
        request: QuestionSessionMaterializeRequest<'_>,
    ) -> Result<QuestionSessionContinuation, SecretStoreError>;

    async fn attach_question_session_artifact(
        &self,
        request: QuestionSessionArtifactAttachRequest<'_>,
    ) -> Result<QuestionSessionContinuation, SecretStoreError>;

    async fn resolve_question_session_continuation(
        &self,
        execution_id: ExecutionId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedQuestionSessionContinuation>, SecretStoreError>;

    /// Resolves a still-unclaimed artifact for one owner-scoped immutable
    /// snapshot so Provider-native `AnswerResolve` can use it without exposing
    /// plaintext through Domain or API types.
    async fn resolve_active_question_session_continuation(
        &self,
        owner_user_id: UserId,
        question_snapshot_id: QuestionSnapshotId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedQuestionSessionContinuation>, SecretStoreError>;

    async fn issue_question_session_operation(
        &self,
        request: QuestionSessionOperationIssueRequest,
    ) -> Result<QuestionSessionOperationIssueOutcome, SecretStoreError>;

    async fn accept_question_session_operation(
        &self,
        request: QuestionSessionOperationAcceptRequest<'_>,
    ) -> Result<QuestionSessionOperationFinishOutcome, SecretStoreError>;

    async fn finish_question_session_operation(
        &self,
        operation: &QuestionSessionOperation,
        terminal_state: QuestionSessionOperationState,
        result_digest: Option<[u8; 32]>,
        completed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<QuestionSessionOperationFinishOutcome, SecretStoreError>;
}

/// Creates one permanently Provider-scoped post-materialization artifact
/// boundary selected from Core-owned account binding.
pub trait QuestionSessionArtifactRepositoryFactory: Send + Sync {
    fn for_provider(&self, provider_id: ProviderId) -> Arc<dyn QuestionSessionArtifactRepository>;
}

/// One immutable candidate returned by a specific `AnswerSource` and bound to a
/// Question inside an immutable `QuestionSnapshot`.
#[derive(Clone, Debug, PartialEq)]
pub struct AnswerCandidateRecord {
    pub id: AnswerCandidateId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub candidate: AnswerCandidate,
    pub created_at: Timestamp,
}

/// Direct, non-cache candidate evidence from one unambiguous matching Question
/// in a prior immutable snapshot of the same owned Task.
#[derive(Clone, Debug, PartialEq)]
pub struct PriorAnswerEvidence {
    pub question_content_fingerprint: QuestionContentFingerprint,
    pub source_question: Question,
    pub source_candidate: AnswerCandidateRecord,
}

/// Transactional multi-source candidate persistence. Selection and submission
/// are deliberately outside this repository boundary.
#[async_trait]
pub trait AnswerCandidateRepository: Send + Sync {
    async fn save_answer_candidate_batch(
        &self,
        candidates: &[AnswerCandidateRecord],
    ) -> Result<(), StorageError>;

    async fn list_owned_answer_candidates(
        &self,
        owner_id: UserId,
        question_snapshot_id: QuestionSnapshotId,
    ) -> Result<Vec<AnswerCandidateRecord>, StorageError>;
}

/// Read boundary for conservative `LocalCache` imports. Implementations must
/// exclude the target snapshot, later snapshots, copied `LocalCache` candidates,
/// foreign owners/tasks, and ambiguous fingerprints on either side.
#[async_trait]
pub trait AnswerCacheRepository: Send + Sync {
    async fn list_owned_prior_answer_evidence(
        &self,
        owner_id: UserId,
        task_id: TaskId,
        target_question_snapshot_id: QuestionSnapshotId,
    ) -> Result<Vec<PriorAnswerEvidence>, StorageError>;
}

/// Immutable, owner-scoped submission draft persistence. Implementations must
/// enforce that every selected Candidate and Question belongs to the draft's
/// exact `QuestionSnapshot`.
#[async_trait]
pub trait SubmissionDraftRepository: Send + Sync {
    async fn save_submission_draft(&self, draft: &SubmissionDraft) -> Result<(), StorageError>;

    async fn find_owned_submission_draft(
        &self,
        owner_id: UserId,
        submission_draft_id: SubmissionDraftId,
    ) -> Result<Option<SubmissionDraft>, StorageError>;
}

/// Immutable, owner-scoped results bound to one real Execution attempt and
/// one persisted `SubmissionDraft`.
#[async_trait]
pub trait SubmissionResultRepository: Send + Sync {
    async fn save_submission_result(&self, result: &SubmissionResult) -> Result<(), StorageError>;

    async fn find_owned_submission_result(
        &self,
        owner_id: UserId,
        submission_result_id: SubmissionResultId,
    ) -> Result<Option<SubmissionResult>, StorageError>;

    async fn find_latest_owned_submission_result(
        &self,
        owner_id: UserId,
        submission_draft_id: SubmissionDraftId,
    ) -> Result<Option<SubmissionResult>, StorageError>;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderRuntimeSettingsTarget {
    Provider {
        provider_id: ProviderId,
    },
    ProviderAccount {
        provider_id: ProviderId,
        provider_account_id: ProviderAccountId,
    },
    Task {
        provider_id: ProviderId,
        provider_account_id: ProviderAccountId,
        task_id: TaskId,
    },
}

impl ProviderRuntimeSettingsTarget {
    #[must_use]
    pub const fn scope(&self) -> ProviderSettingScope {
        match self {
            Self::Provider { .. } => ProviderSettingScope::Provider,
            Self::ProviderAccount { .. } => ProviderSettingScope::ProviderAccount,
            Self::Task { .. } => ProviderSettingScope::Task,
        }
    }

    #[must_use]
    pub const fn provider_id(&self) -> &ProviderId {
        match self {
            Self::Provider { provider_id }
            | Self::ProviderAccount { provider_id, .. }
            | Self::Task { provider_id, .. } => provider_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRuntimeSettingsRecord {
    pub id: ProviderRuntimeSettingsId,
    pub target: ProviderRuntimeSettingsTarget,
    pub patch: ProviderRuntimeSettingsPatch,
    pub revision: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ProviderRuntimeSettingsWriteRequest<'a> {
    pub target: ProviderRuntimeSettingsTarget,
    pub expected_revision: u32,
    pub patch: &'a ProviderRuntimeSettingsPatch,
    pub schema: &'a ProviderRuntimeSettingsSchema,
    pub actor: AuditActor,
    pub correlation_id: &'a str,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderRuntimeSettingsWriteOutcome {
    Stored(ProviderRuntimeSettingsRecord),
    TargetNotFound,
    RevisionConflict,
}

/// Master-owned Provider runtime settings with optimistic concurrency.
#[async_trait]
pub trait ProviderRuntimeSettingsRepository: Send + Sync {
    async fn find_provider_runtime_settings(
        &self,
        target: &ProviderRuntimeSettingsTarget,
    ) -> Result<Option<ProviderRuntimeSettingsRecord>, StorageError>;

    async fn write_provider_runtime_settings(
        &self,
        request: ProviderRuntimeSettingsWriteRequest<'_>,
    ) -> Result<ProviderRuntimeSettingsWriteOutcome, StorageError>;
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

    /// Persists a hash-only external OAuth callback binding together with the
    /// authentication session's first waiting-user transition.
    async fn create_external_oauth_pending(
        &self,
        pending: &ExternalOauthPending,
        waiting_session: &AuthSession,
        expected_auth_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError>;

    async fn find_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
    ) -> Result<Option<ExternalOauthPending>, StorageError>;

    async fn find_external_oauth_pending_by_state(
        &self,
        owner_user_id: UserId,
        provider_id: &ProviderId,
        state_digest: [u8; 32],
    ) -> Result<Option<ExternalOauthPending>, StorageError>;

    /// Atomically consumes the pending callback and moves its exact
    /// `AuthSession` from `WaitingUser` to `ExchangingCredential`.
    async fn claim_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
        at: Timestamp,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<Option<ExternalOauthClaim>, StorageError>;

    /// Reconciles a durable callback with its authentication session without
    /// ever replaying the one-shot Provider exchange.
    async fn recover_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
        at: Timestamp,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<Option<ExternalOauthClaim>, StorageError>;

    async fn update_external_oauth_pending(
        &self,
        pending: &ExternalOauthPending,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalOauthClaim {
    pub pending: ExternalOauthPending,
    pub auth_session: AuthSession,
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

/// Short-lived `BrowserBridge` helper sessions with one-time token rotation.
#[async_trait]
pub trait BrowserBridgeSessionRepository: Send + Sync {
    async fn create_browser_bridge_session(
        &self,
        session: &BrowserBridgeSession,
        spec: &BrowserSessionSpec,
        pairing_token_digest: &TokenDigest,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError>;

    async fn find_browser_bridge_session(
        &self,
        owner_user_id: UserId,
        session_id: BrowserBridgeSessionId,
    ) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError>;

    async fn claim_browser_bridge_session(
        &self,
        session_id: BrowserBridgeSessionId,
        pairing_token_digest: &TokenDigest,
        access_token_digest: &TokenDigest,
        claimed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError>;

    async fn authenticate_browser_bridge_access(
        &self,
        session_id: BrowserBridgeSessionId,
        access_token_digest: &TokenDigest,
        authenticated_at: Timestamp,
    ) -> Result<Option<(BrowserBridgeSession, BrowserSessionSpec)>, StorageError>;

    async fn update_browser_bridge_session_for_owner(
        &self,
        session: &BrowserBridgeSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;

    async fn issue_browser_bridge_exchange(
        &self,
        exchange: &BrowserBridgeExchange,
        correlation_id: &str,
    ) -> Result<BrowserBridgeExchangeRecord, StorageError>;

    async fn complete_browser_bridge_exchange(
        &self,
        exchange: &BrowserBridgeExchange,
        access_token_digest: &TokenDigest,
        correlation_id: &str,
    ) -> Result<BrowserBridgeExchangeRecord, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserBridgeExchangeRecord {
    Inserted(BrowserBridgeExchange),
    Duplicate(BrowserBridgeExchange),
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

#[derive(Clone, Debug, PartialEq)]
pub struct UserProfilePage {
    pub items: Vec<UserProfile>,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct UserAdminCreate<'a> {
    pub user: &'a User,
    pub actor: AuditActor,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug)]
pub struct UserAdminUpdate<'a> {
    pub user_id: UserId,
    pub expected_updated_at: Timestamp,
    pub status: UserStatus,
    pub roles: &'a [asterism_domain::Role],
    pub permissions: &'a [asterism_domain::Permission],
    pub actor: AuditActor,
    pub correlation_id: &'a str,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UserAdminCreateOutcome {
    Created(UserProfile),
    UsernameConflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UserAdminUpdateOutcome {
    Updated(UserProfile),
    UserNotFound,
    RevisionConflict,
    LastActiveMaster,
}

#[async_trait]
pub trait UserAdminRepository: Send + Sync {
    async fn list_user_profiles(
        &self,
        limit: u32,
        offset: u64,
    ) -> Result<UserProfilePage, StorageError>;

    async fn find_user_profile(&self, user_id: UserId)
    -> Result<Option<UserProfile>, StorageError>;

    async fn create_user(
        &self,
        request: UserAdminCreate<'_>,
    ) -> Result<UserAdminCreateOutcome, StorageError>;

    async fn update_user(
        &self,
        request: UserAdminUpdate<'_>,
    ) -> Result<UserAdminUpdateOutcome, StorageError>;
}

#[derive(Clone, Debug, Default)]
pub struct AuditFilter {
    pub action: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuditPage {
    pub items: Vec<AuditRecord>,
    pub total: u64,
}

#[async_trait]
pub trait AuditQueryRepository: Send + Sync {
    async fn list_audit_records(
        &self,
        owner_scope: Option<UserId>,
        filter: &AuditFilter,
        limit: u32,
        offset: u64,
    ) -> Result<AuditPage, StorageError>;
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
pub struct ExecutionBillingReservation<'a> {
    pub quote: &'a PriceQuote,
    pub reservation: &'a CreditReservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRuntimeSettingsSnapshot {
    pub provider_id: ProviderId,
    pub resolved: ResolvedProviderRuntimeSettings,
    pub sources: BTreeMap<String, ProviderRuntimeSettingSource>,
    pub provider_revision: Option<u32>,
    pub provider_account_revision: Option<u32>,
    pub task_revision: Option<u32>,
    pub captured_at: Timestamp,
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutionRuntimeSettingsResolution<'a> {
    pub snapshot: &'a ExecutionRuntimeSettingsSnapshot,
    pub schema: &'a ProviderRuntimeSettingsSchema,
}

#[derive(Clone, Debug)]
pub struct ExecutionScheduleRequest<'a> {
    pub execution: &'a Execution,
    /// Exact Provider-approved phase order. This is persisted atomically with
    /// the Execution and must be a permutation of `requested_capabilities`.
    pub capability_plan: &'a [TaskCapability],
    pub billing: Option<ExecutionBillingReservation<'a>>,
    pub runtime_settings: Option<ExecutionRuntimeSettingsResolution<'a>>,
    pub expected_task_state: OrchestrationState,
    pub idempotency_scope: &'a str,
    pub idempotency_key: &'a str,
    pub actor: AuditActor,
    pub correlation_id: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionCapabilityStepState {
    Pending,
    Issued,
    Succeeded,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionCapabilityStep {
    pub execution_id: ExecutionId,
    pub position: u8,
    pub capability: TaskCapability,
    pub state: ExecutionCapabilityStepState,
    pub issued_attempt_id: Option<ExecutionAttemptId>,
    pub issued_at: Option<Timestamp>,
    pub succeeded_at: Option<Timestamp>,
}

#[derive(Clone, Debug)]
pub struct ExecutionCapabilityStepMutation<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub capability: TaskCapability,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionCapabilityStepIssueOutcome {
    Issued,
    AlreadyIssued,
}

/// Durable phase boundary for a composite Provider execution. A phase is
/// marked issued before the remote mutation, preventing crash recovery from
/// replaying a possibly accepted non-idempotent request.
#[async_trait]
pub trait ExecutionCapabilityStepRepository: Send + Sync {
    async fn find_execution_capability_steps(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Vec<ExecutionCapabilityStep>, StorageError>;

    async fn issue_execution_capability_step(
        &self,
        request: ExecutionCapabilityStepMutation<'_>,
    ) -> Result<ExecutionCapabilityStepIssueOutcome, StorageError>;

    async fn succeed_execution_capability_step(
        &self,
        request: ExecutionCapabilityStepMutation<'_>,
    ) -> Result<(), StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionScheduleOutcome {
    Created(Execution),
    Existing(Execution),
    IdempotencyConflict,
    SubmissionDraftConflict,
    TaskStateConflict,
    RuntimeSettingsConflict,
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
pub struct ExecutionLogAppendRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub worker_id: &'a str,
    pub at: Timestamp,
    pub level: LogLevel,
    pub stage: ExecutionStage,
    pub message: &'a str,
    pub provider_trace_id: Option<&'a str>,
    pub metadata_sanitized: Option<&'a serde_json::Value>,
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

#[derive(Clone, Debug)]
pub struct ExecutionRecoveryFinishRequest<'a> {
    pub execution_id: ExecutionId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub final_state: ExecutionState,
    pub error_class: Option<ProviderErrorClass>,
    pub provider_trace_id: Option<&'a str>,
    pub retry_at: Option<Timestamp>,
    pub progress: &'a ExecutionProgress,
    pub at: Timestamp,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug)]
pub struct SubmissionReceiptPersistRequest<'a> {
    pub record: &'a SubmissionAttemptReceipt,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct SubmissionResultPersistRequest<'a> {
    pub result: &'a SubmissionResult,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct VerificationRecoveryStartRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub error_class: ProviderErrorClass,
    pub progress: &'a ExecutionProgress,
    pub at: Timestamp,
    pub correlation_id: &'a str,
}

/// Atomically leaves the mutation attempt and schedules a verify-only Recovery
/// job after a potentially non-idempotent remote outcome becomes ambiguous.
#[async_trait]
pub trait ExecutionVerificationRecoveryRepository: Send + Sync {
    async fn begin_verification_recovery(
        &self,
        request: VerificationRecoveryStartRequest<'_>,
    ) -> Result<Execution, StorageError>;
}

/// Worker-only persistence for a Draft-bound independent submission. A known
/// receipt is durable before verification, while recovery can load the exact
/// Draft and latest active-attempt receipt without granting any resubmit path.
#[async_trait]
pub trait ExecutionSubmissionRepository: Send + Sync {
    async fn find_execution_submission_draft(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<SubmissionDraft>, StorageError>;

    async fn persist_submission_receipt(
        &self,
        request: SubmissionReceiptPersistRequest<'_>,
    ) -> Result<(), StorageError>;

    async fn find_active_submission_receipt(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<SubmissionAttemptReceipt>, StorageError>;

    async fn find_active_submission_attempt_id(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionAttemptId>, StorageError>;

    async fn find_active_submission_result(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<SubmissionResult>, StorageError>;

    async fn persist_submission_result(
        &self,
        request: SubmissionResultPersistRequest<'_>,
    ) -> Result<(), StorageError>;
}

/// Atomic execution request boundary. Creating the optional immutable quote and
/// reservation, moving the task to `scheduled`, creating the execution,
/// enqueuing the scheduler job, and recording audit/outbox entries either all
/// commit or all roll back.
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

    async fn find_execution_runtime_settings(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionRuntimeSettingsSnapshot>, StorageError>;

    async fn start_attempt(
        &self,
        request: ExecutionAttemptStartRequest<'_>,
    ) -> Result<ExecutionAttempt, StorageError>;

    async fn update_progress(
        &self,
        request: ExecutionProgressUpdate<'_>,
    ) -> Result<bool, StorageError>;

    async fn append_log(&self, request: ExecutionLogAppendRequest<'_>) -> Result<(), StorageError>;

    async fn finish_attempt(
        &self,
        request: ExecutionAttemptFinishRequest<'_>,
    ) -> Result<Execution, StorageError>;

    async fn finish_recovery(
        &self,
        request: ExecutionRecoveryFinishRequest<'_>,
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

    async fn grant(&self, grant: &CreditGrant) -> Result<CreditGrantOutcome, StorageError>;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditTransactionPage {
    pub items: Vec<CreditTransaction>,
    pub total: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditReservationDetail {
    pub reservation: CreditReservation,
    pub quote: PriceQuote,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreditReservationPage {
    pub items: Vec<CreditReservationDetail>,
    pub total: u64,
}

#[async_trait]
pub trait CreditQueryRepository: Send + Sync {
    async fn list_owned_credit_transactions(
        &self,
        owner_id: UserId,
        limit: u32,
        offset: u64,
    ) -> Result<CreditTransactionPage, StorageError>;

    async fn list_owned_credit_reservations(
        &self,
        owner_id: UserId,
        limit: u32,
        offset: u64,
    ) -> Result<CreditReservationPage, StorageError>;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceTokenPage {
    pub items: Vec<ServiceToken>,
    pub total: u64,
}

#[async_trait]
pub trait ServiceTokenQueryRepository: Send + Sync {
    async fn list_service_tokens(
        &self,
        owner_scope: Option<UserId>,
        limit: u32,
        offset: u64,
    ) -> Result<ServiceTokenPage, StorageError>;

    async fn find_service_token(
        &self,
        token_id: ServiceTokenId,
    ) -> Result<Option<ServiceToken>, StorageError>;
}
