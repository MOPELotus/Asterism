use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use asterism_auth::TokenDigest;
use asterism_domain::{
    AccountHealth, AnswerBootstrapHarvest, AnswerCandidate, AnswerCandidateId, AttemptResult,
    AuditActor, AuditRecord, AuthBootstrapClientEvent, AuthBootstrapSession,
    AuthBootstrapSessionId, AuthSession, AuthSessionId, BrowserBridgeExchange,
    BrowserBridgeResultArtifactMetadata, BrowserBridgeRuntimeBinding,
    BrowserBridgeRuntimeStateMetadata, BrowserBridgeSession, BrowserBridgeSessionId,
    CompletionPolicySnapshot, Course, CourseAggregateProgress, CourseId, CreditAccount,
    CreditReservation, CreditReservationId, CreditTransaction, CreditTransactionId, Execution,
    ExecutionAttempt, ExecutionAttemptId, ExecutionId, ExecutionLease, ExecutionLogEvent,
    ExecutionProgress, ExecutionStage, ExecutionState, ExternalOauthPending,
    GlobalAnswerCorpusEntryId, GlobalCorpusQuestionAsset, GlobalSemanticAnswer, LogLevel,
    OrchestrationState, PriceQuote, PrivateAnswerEvidence, PrivateAnswerEvidenceId,
    ProtocolObservation, ProtocolObservationKind, ProtocolSurface, ProviderAccount,
    ProviderAccountId, ProviderErrorClass, ProviderId, ProviderRuntimeSettingsId, Question,
    QuestionContentFingerprint, QuestionReadAttempt, QuestionReadAttemptId, QuestionSession,
    QuestionSnapshotId, ScheduleId, ServiceToken, ServiceTokenId, SubmissionAttemptReceipt,
    SubmissionDraft, SubmissionDraftId, SubmissionResult, SubmissionResultId, SubmissionScore,
    Task, TaskActionReceiptId, TaskCapability, TaskId, TaskLifecycleAction, Timestamp, User,
    UserId, UserProfile, UserStatus, WebSession, WebSessionId,
};
use asterism_provider_api::{
    AnswerHistoryRetakeFacts, BrowserBridgeWorkflowResult, BrowserSessionSpec,
    ExecutionMutationPlan, ExecutionMutationSequencePlan, ProviderExecutionPlanArtifact,
    ProviderRuntimeSettingSource, ProviderRuntimeSettingsPatch, ProviderRuntimeSettingsSchema,
    ProviderSettingScope, ResolvedProviderRuntimeSettings,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnswerEvidenceProjectionState {
    Projected,
    Unmatched(asterism_domain::UnmatchedEvidenceReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerEvidenceRecord {
    pub private_evidence_id: PrivateAnswerEvidenceId,
    pub corpus_entry_id: Option<GlobalAnswerCorpusEntryId>,
    pub projection_state: AnswerEvidenceProjectionState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnswerEvidenceRecordOutcome {
    Inserted(AnswerEvidenceRecord),
    Duplicate(AnswerEvidenceRecord),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalAnswerCorpusEvidence {
    pub corpus_entry_id: GlobalAnswerCorpusEntryId,
    pub question_content_fingerprint: QuestionContentFingerprint,
    pub question: GlobalCorpusQuestionAsset,
    pub answer: GlobalSemanticAnswer,
    pub official_evidence_count: u64,
    pub verified_historical_evidence_count: u64,
    pub negative_evidence_count: u64,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub last_verified_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnswerEvidenceClassCounts {
    pub official: u64,
    pub verified_historical: u64,
    pub negative: u64,
}

/// Durable two-layer Answer Evidence boundary. Private provenance is accepted
/// here, while reads expose only the identity-free global projection.
#[async_trait]
pub trait AnswerEvidenceRepository: Send + Sync {
    async fn record_answer_evidence(
        &self,
        evidence: &PrivateAnswerEvidence,
    ) -> Result<AnswerEvidenceRecordOutcome, StorageError>;

    async fn list_global_answer_corpus_evidence(
        &self,
        question_content_fingerprint: &QuestionContentFingerprint,
    ) -> Result<Vec<GlobalAnswerCorpusEvidence>, StorageError>;

    async fn count_owned_execution_attempt_evidence(
        &self,
        owner_id: UserId,
        execution_attempt_id: ExecutionAttemptId,
    ) -> Result<Option<AnswerEvidenceClassCounts>, StorageError>;
}

#[async_trait]
pub trait AnswerBootstrapHarvestRepository: Send + Sync {
    async fn find_owned_answer_bootstrap_harvest(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        generation: u32,
    ) -> Result<Option<AnswerBootstrapHarvest>, StorageError>;

    async fn claim_due_answer_bootstrap_harvests(
        &self,
        worker_id: &str,
        eligible_provider_ids: &BTreeSet<ProviderId>,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<ClaimedAnswerBootstrapHarvest>, StorageError>;

    async fn checkpoint_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestCheckpoint<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError>;

    async fn yield_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestYield<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError>;

    async fn complete_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestCompletion<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError>;

    async fn fail_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestFailure<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrictCompletionWorkflowRecord {
    pub workflow: asterism_domain::StrictCompletionWorkflow,
    pub revision: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreImprovementWorkflowRecord {
    pub workflow: asterism_domain::ScoreImprovementWorkflow,
    pub revision: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionWorkflowCreateOutcome<T> {
    Created(T),
    Existing(T),
    Conflict,
}

#[derive(Clone, Debug)]
pub struct StrictCompletionBeginRequest {
    pub owner_user_id: UserId,
    pub workflow_id: asterism_domain::StrictCompletionWorkflowId,
    pub expected_revision: u32,
    pub formal_assessment: bool,
    pub retry_confirmed: bool,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct StrictCompletionObserveRequest {
    pub owner_user_id: UserId,
    pub workflow_id: asterism_domain::StrictCompletionWorkflowId,
    pub expected_revision: u32,
    pub outcome: Option<asterism_domain::CompletionOutcome>,
    pub diagnosis: Option<asterism_domain::CompletionDiagnosis>,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct StrictCompletionExecutionObservationRequest<'a> {
    pub execution_id: ExecutionId,
    pub execution_attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub outcome: Option<asterism_domain::CompletionOutcome>,
    pub diagnosis: Option<asterism_domain::CompletionDiagnosis>,
    pub at: Timestamp,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrictCompletionExecutionObservationRecord {
    pub execution_id: ExecutionId,
    pub execution_attempt_id: ExecutionAttemptId,
    pub workflow: StrictCompletionWorkflowRecord,
    pub workflow_attempt_no: Option<u32>,
    pub outcome: Option<asterism_domain::CompletionOutcome>,
    pub diagnosis: Option<asterism_domain::CompletionDiagnosis>,
    pub observed_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ScoreImprovementBeginRequest {
    pub owner_user_id: UserId,
    pub workflow_id: asterism_domain::ScoreImprovementWorkflowId,
    pub expected_revision: u32,
    pub explicitly_confirmed: bool,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ScoreImprovementObserveRequest {
    pub owner_user_id: UserId,
    pub workflow_id: asterism_domain::ScoreImprovementWorkflowId,
    pub expected_revision: u32,
    pub score: Option<asterism_domain::SubmissionScore>,
    pub retake_still_allowed: bool,
    pub diagnosis: Option<asterism_domain::CompletionDiagnosis>,
    pub at: Timestamp,
}

#[async_trait]
pub trait CompletionWorkflowRepository: Send + Sync {
    async fn create_strict_completion_workflow(
        &self,
        workflow: &asterism_domain::StrictCompletionWorkflow,
    ) -> Result<CompletionWorkflowCreateOutcome<StrictCompletionWorkflowRecord>, StorageError>;

    async fn find_owned_strict_completion_workflow(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<StrictCompletionWorkflowRecord>, StorageError>;

    async fn begin_strict_completion_attempt(
        &self,
        request: StrictCompletionBeginRequest,
    ) -> Result<StrictCompletionWorkflowRecord, StorageError>;

    async fn observe_strict_completion(
        &self,
        request: StrictCompletionObserveRequest,
    ) -> Result<StrictCompletionWorkflowRecord, StorageError>;

    async fn record_strict_completion_execution_observation(
        &self,
        request: StrictCompletionExecutionObservationRequest<'_>,
    ) -> Result<StrictCompletionExecutionObservationRecord, StorageError>;

    async fn create_score_improvement_workflow(
        &self,
        workflow: &asterism_domain::ScoreImprovementWorkflow,
    ) -> Result<CompletionWorkflowCreateOutcome<ScoreImprovementWorkflowRecord>, StorageError>;

    async fn find_owned_score_improvement_workflow(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<ScoreImprovementWorkflowRecord>, StorageError>;

    async fn begin_score_improvement_attempt(
        &self,
        request: ScoreImprovementBeginRequest,
    ) -> Result<ScoreImprovementWorkflowRecord, StorageError>;

    async fn observe_score_improvement(
        &self,
        request: ScoreImprovementObserveRequest,
    ) -> Result<ScoreImprovementWorkflowRecord, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedAnswerBootstrapHarvest {
    pub harvest: AnswerBootstrapHarvest,
    pub worker_id: String,
    pub lease_expires_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct AnswerBootstrapHarvestCheckpoint<'a> {
    pub harvest_id: asterism_domain::AnswerBootstrapHarvestId,
    pub schedule_id: ScheduleId,
    pub worker_id: &'a str,
    pub scanned_task_count: u32,
    pub total_task_count: Option<u32>,
    pub watermark_sanitized: &'a serde_json::Value,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct AnswerBootstrapHarvestCompletion<'a> {
    pub harvest_id: asterism_domain::AnswerBootstrapHarvestId,
    pub schedule_id: ScheduleId,
    pub worker_id: &'a str,
    pub scanned_task_count: u32,
    pub total_task_count: u32,
    pub watermark_sanitized: &'a serde_json::Value,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct AnswerBootstrapHarvestYield<'a> {
    pub harvest_id: asterism_domain::AnswerBootstrapHarvestId,
    pub schedule_id: ScheduleId,
    pub worker_id: &'a str,
    pub scanned_task_count: u32,
    pub total_task_count: Option<u32>,
    pub watermark_sanitized: &'a serde_json::Value,
    pub run_at: Timestamp,
    pub at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct AnswerBootstrapHarvestFailure<'a> {
    pub harvest_id: asterism_domain::AnswerBootstrapHarvestId,
    pub schedule_id: ScheduleId,
    pub worker_id: &'a str,
    pub error_sanitized: &'a str,
    pub retry_at: Option<Timestamp>,
    pub at: Timestamp,
}

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

#[derive(Clone, Debug, PartialEq)]
pub struct CourseAggregateProgressRecord {
    pub course: Course,
    pub progress: CourseAggregateProgress,
}

#[async_trait]
pub trait CourseProgressRepository: Send + Sync {
    async fn find_owned_course_aggregate_progress(
        &self,
        owner_id: UserId,
        course_id: CourseId,
    ) -> Result<Option<CourseAggregateProgressRecord>, StorageError>;
}

#[async_trait]
pub trait AccountHealthRepository: Send + Sync {
    async fn find_owned_account_health(
        &self,
        owner_id: UserId,
        account_id: ProviderAccountId,
    ) -> Result<Option<AccountHealth>, StorageError>;
}

#[derive(Clone, Debug)]
pub struct ProtocolObservationRecordRequest<'a> {
    pub provider_id: ProviderId,
    pub surface: ProtocolSurface,
    pub kind: ProtocolObservationKind,
    pub shape_sanitized: &'a serde_json::Value,
    pub occurrence_digest: [u8; 32],
    pub execution_id: Option<ExecutionId>,
    pub observed_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolObservationRecordOutcome {
    Created(ProtocolObservation),
    Updated(ProtocolObservation),
    Duplicate(ProtocolObservation),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProtocolObservationPage {
    pub items: Vec<ProtocolObservation>,
    pub total: u64,
}

#[async_trait]
pub trait ProtocolObservationRepository: Send + Sync {
    async fn record_protocol_observation(
        &self,
        request: ProtocolObservationRecordRequest<'_>,
    ) -> Result<ProtocolObservationRecordOutcome, StorageError>;

    async fn list_protocol_observations(
        &self,
        provider_id: Option<&ProviderId>,
        kind: Option<ProtocolObservationKind>,
        limit: u32,
        offset: u64,
    ) -> Result<ProtocolObservationPage, StorageError>;
}

/// Internal Task lookup for an already authorized Scheduler execution.
#[async_trait]
pub trait TaskRuntimeRepository: Send + Sync {
    async fn find_runtime_task(&self, task_id: TaskId) -> Result<Option<Task>, StorageError>;

    async fn find_runtime_task_by_remote_identity(
        &self,
        _provider_account_id: ProviderAccountId,
        _remote_task_id: &str,
    ) -> Result<Option<Task>, StorageError> {
        Err(StorageError::InvalidData(
            "runtime repository does not support remote Task identity lookup".to_owned(),
        ))
    }
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
    pub latest_transition: Option<QuestionSessionTransition>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionSessionTransition {
    pub previous_session_id: asterism_domain::QuestionSessionId,
    pub operation_sequence: u64,
    pub execution_id: ExecutionId,
    pub next_session_id: asterism_domain::QuestionSessionId,
    pub next_question_snapshot_id: QuestionSnapshotId,
    pub transitioned_at: Timestamp,
}

#[derive(Debug)]
pub struct QuestionSessionNextMaterializeRequest<'a> {
    pub operation: &'a QuestionSessionOperation,
    pub snapshot: &'a QuestionSnapshot,
    pub artifact_type: &'a str,
    pub artifact_phase: &'a str,
    pub artifact: SecretValue,
    pub artifact_ttl_seconds: u64,
    pub result_digest: [u8; 32],
    pub materialized_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestionSessionNextMaterializeOutcome {
    Materialized {
        operation: QuestionSessionOperation,
        transition: QuestionSessionTransition,
        continuation: QuestionSessionContinuation,
    },
    Duplicate {
        operation: QuestionSessionOperation,
        transition: QuestionSessionTransition,
    },
    Conflict,
    Unavailable,
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

    /// Accepts one definite next-Question response, consumes the old claimed
    /// session and atomically materializes the next immutable snapshot/session.
    async fn materialize_next_question_session(
        &self,
        request: QuestionSessionNextMaterializeRequest<'_>,
    ) -> Result<QuestionSessionNextMaterializeOutcome, SecretStoreError>;

    /// Finishes an issued operation as rejected/ambiguous, or accepts a
    /// definite terminal submission without inventing another continuation.
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

#[derive(Clone, Debug)]
pub struct AnswerHistoryIngestRequest<'a> {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub provider_attempt_digest: [u8; 32],
    pub result_digest: [u8; 32],
    pub snapshot: &'a QuestionSnapshot,
    pub candidates: &'a [AnswerCandidateRecord],
    pub evidence: &'a [PrivateAnswerEvidence],
    pub score: Option<SubmissionScore>,
    pub retake: Option<&'a AnswerHistoryRetakeFacts>,
    pub provenance_sanitized: &'a serde_json::Value,
    pub observed_at: Timestamp,
    pub imported_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerHistoryImportRecord {
    pub import_id: asterism_domain::AnswerHistoryImportId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub candidate_count: u32,
    pub evidence_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerHistoryTaskFact {
    pub import_id: asterism_domain::AnswerHistoryImportId,
    pub owner_user_id: UserId,
    pub provider_id: ProviderId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub provider_attempt_digest: [u8; 32],
    pub result_digest: [u8; 32],
    pub score: Option<SubmissionScore>,
    pub retake: Option<AnswerHistoryRetakeFacts>,
    pub provenance_sanitized: serde_json::Value,
    pub observed_at: Timestamp,
    pub imported_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnswerHistoryIngestOutcome {
    Inserted(AnswerHistoryImportRecord),
    Duplicate(AnswerHistoryImportRecord),
}

#[async_trait]
pub trait AnswerHistoryIngestionRepository: Send + Sync {
    async fn ingest_answer_history_task(
        &self,
        request: AnswerHistoryIngestRequest<'_>,
    ) -> Result<AnswerHistoryIngestOutcome, StorageError>;

    async fn find_latest_owned_answer_history_task_fact(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<AnswerHistoryTaskFact>, StorageError>;
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

    async fn find_previous_owned_submission_score(
        &self,
        owner_id: UserId,
        task_id: TaskId,
        submission_result_id: SubmissionResultId,
    ) -> Result<Option<asterism_domain::SubmissionScore>, StorageError>;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractiveAuthContinuation {
    pub auth_session_id: AuthSessionId,
    pub provider_id: ProviderId,
    pub continuation_type: String,
    pub continuation_digest: [u8; 32],
    pub phase: String,
    pub revision: u32,
    pub poll_count: u32,
    pub maximum_polls: u32,
    pub terminal_result_digest: Option<[u8; 32]>,
    pub expires_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractiveAuthPollClaim {
    pub continuation: InteractiveAuthContinuation,
    pub poll_sequence: u32,
    pub claim_digest: [u8; 32],
    pub claim_expires_at: Timestamp,
}

#[derive(Debug)]
pub struct ResolvedInteractiveAuthCandidate {
    pub session: AuthSession,
    pub continuation: InteractiveAuthContinuation,
    pub value: SecretValue,
}

#[derive(Debug)]
pub struct InteractiveAuthContinuationAttachRequest<'a> {
    pub session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub provider_id: &'a ProviderId,
    pub continuation_type: &'a str,
    pub continuation_digest: [u8; 32],
    pub phase: &'a str,
    pub value: SecretValue,
    pub maximum_polls: u32,
    pub expires_at: Timestamp,
    pub attached_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct InteractiveAuthPollClaimRequest<'a> {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub auth_session_id: AuthSessionId,
    pub claimed_at: Timestamp,
    pub claim_expires_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct InteractiveAuthPollRotateRequest<'a> {
    pub claim: &'a InteractiveAuthPollClaim,
    pub waiting_session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub continuation_type: &'a str,
    pub continuation_digest: [u8; 32],
    pub phase: &'a str,
    pub replacement: SecretValue,
    pub result_digest: [u8; 32],
    pub completed_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct InteractiveAuthPollAuthenticateRequest<'a> {
    pub claim: &'a InteractiveAuthPollClaim,
    pub validating_session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub continuation_type: &'a str,
    pub continuation_digest: [u8; 32],
    pub phase: &'a str,
    pub replacement: SecretValue,
    pub result_digest: [u8; 32],
    pub completed_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractiveAuthTerminalState {
    Rejected,
    Expired,
    Failed,
}

#[derive(Debug)]
pub struct InteractiveAuthPollTerminalRequest<'a> {
    pub claim: &'a InteractiveAuthPollClaim,
    pub terminal_session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub terminal_state: InteractiveAuthTerminalState,
    pub result_digest: [u8; 32],
    pub completed_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct InteractiveAuthCandidateFailureRequest<'a> {
    pub continuation: &'a InteractiveAuthContinuation,
    pub terminal_session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub failed_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct InteractiveAuthAbortRequest<'a> {
    pub terminal_session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub aborted_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub enum InteractiveAuthPollClaimOutcome {
    Claimed {
        claim: Box<InteractiveAuthPollClaim>,
        value: SecretValue,
    },
    Busy,
    Exhausted,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InteractiveAuthContinuationMutationOutcome {
    Rotated(InteractiveAuthContinuation),
    AuthenticatedCandidate(InteractiveAuthContinuation),
    Terminal(AuthSession),
    Conflict,
    Unavailable,
}

/// Provider-scoped encrypted continuation and serialized poll lease for one
/// owner/account/AuthSession-bound interactive authentication flow.
#[async_trait]
pub trait InteractiveAuthContinuationRepository: Send + Sync {
    async fn attach_interactive_auth_continuation(
        &self,
        request: InteractiveAuthContinuationAttachRequest<'_>,
    ) -> Result<InteractiveAuthContinuation, SecretStoreError>;

    async fn claim_interactive_auth_poll(
        &self,
        request: InteractiveAuthPollClaimRequest<'_>,
    ) -> Result<InteractiveAuthPollClaimOutcome, SecretStoreError>;

    async fn release_interactive_auth_poll(
        &self,
        claim: &InteractiveAuthPollClaim,
        released_at: Timestamp,
        access: &SecretAccess,
    ) -> Result<bool, SecretStoreError>;

    async fn resolve_interactive_auth_candidate(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedInteractiveAuthCandidate>, SecretStoreError>;

    async fn rotate_interactive_auth_continuation(
        &self,
        request: InteractiveAuthPollRotateRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError>;

    async fn persist_interactive_auth_candidate(
        &self,
        request: InteractiveAuthPollAuthenticateRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError>;

    async fn finish_interactive_auth_terminal(
        &self,
        request: InteractiveAuthPollTerminalRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError>;

    async fn finish_interactive_auth_candidate_failure(
        &self,
        request: InteractiveAuthCandidateFailureRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError>;

    async fn abort_interactive_auth_continuation(
        &self,
        request: InteractiveAuthAbortRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError>;
}

pub trait InteractiveAuthContinuationRepositoryFactory: Send + Sync {
    fn for_provider(
        &self,
        provider_id: ProviderId,
    ) -> Arc<dyn InteractiveAuthContinuationRepository>;
}

#[derive(Debug)]
pub struct InteractiveAuthCredentialCommitRequest<'a> {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub authenticated_session: &'a AuthSession,
    pub expected_session_revision: u32,
    pub continuation: &'a InteractiveAuthContinuation,
    pub terminal_result_digest: [u8; 32],
    pub bundle: CredentialBundle,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractiveAuthCredentialCommit {
    pub session: AuthSession,
    pub credentials: Vec<ProviderCredential>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InteractiveAuthCredentialCommitOutcome {
    Committed(InteractiveAuthCredentialCommit),
    BindingConflict,
}

/// Atomically consumes an authenticated continuation, commits its validated
/// credential bundle and advances the exact `AuthSession` to `Authenticated`.
#[async_trait]
pub trait InteractiveAuthCredentialRepository: Send + Sync {
    async fn commit_interactive_auth_credentials(
        &self,
        request: InteractiveAuthCredentialCommitRequest<'_>,
    ) -> Result<InteractiveAuthCredentialCommitOutcome, SecretStoreError>;
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

    async fn bind_browser_bridge_runtime(
        &self,
        binding: &BrowserBridgeRuntimeBinding,
        access_token_digest: &TokenDigest,
        correlation_id: &str,
    ) -> Result<BrowserBridgeRuntimeBindingRecord, StorageError>;

    async fn find_browser_bridge_runtime_binding(
        &self,
        owner_user_id: UserId,
        session_id: BrowserBridgeSessionId,
    ) -> Result<Option<BrowserBridgeRuntimeBinding>, StorageError>;

    async fn find_latest_browser_bridge_exchange(
        &self,
        owner_user_id: UserId,
        session_id: BrowserBridgeSessionId,
    ) -> Result<Option<BrowserBridgeExchange>, StorageError>;

    /// Lists bounded durable result inboxes whose claimed session and issued
    /// exchange still await Provider/Core processing.
    async fn list_pending_browser_bridge_results(
        &self,
        now: Timestamp,
        provider_id: &ProviderId,
        result_types: &[&str],
        limit: u32,
    ) -> Result<Vec<PendingBrowserBridgeResult>, StorageError>;

    /// Claims a bounded Provider/result-type inbox batch under one expiring
    /// worker lease and increments each durable attempt exactly once.
    async fn claim_pending_browser_bridge_results(
        &self,
        now: Timestamp,
        provider_id: &ProviderId,
        result_types: &[&str],
        limit: u32,
        worker_id: &str,
        lease_expires_at: Timestamp,
    ) -> Result<Vec<PendingBrowserBridgeResult>, StorageError>;

    /// Releases a claimed result into a future retry or terminal dead letter.
    async fn finish_browser_bridge_result_attempt(
        &self,
        request: BrowserBridgeResultAttemptFinishRequest<'_>,
    ) -> Result<bool, StorageError>;

    async fn update_browser_bridge_session_for_owner(
        &self,
        session: &BrowserBridgeSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError>;

    async fn complete_browser_bridge_exchange(
        &self,
        exchange: &BrowserBridgeExchange,
        access_token_digest: &TokenDigest,
        correlation_id: &str,
    ) -> Result<BrowserBridgeExchangeRecord, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingBrowserBridgeResult {
    pub owner_user_id: UserId,
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub provider_id: ProviderId,
    pub result_type: String,
    pub attempt_no: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeResultAttemptFinishRequest<'a> {
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub worker_id: &'a str,
    pub failed_at: Timestamp,
    pub retry_at: Option<Timestamp>,
    pub error_kind: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserBridgeRuntimeBindingRecord {
    Bound(BrowserBridgeRuntimeBinding),
    Duplicate(BrowserBridgeRuntimeBinding),
    AccessRejected,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserBridgeExchangeRecord {
    Inserted(BrowserBridgeExchange),
    Duplicate(BrowserBridgeExchange),
    AccessRejected,
    SequenceConflict,
}

#[derive(Debug)]
pub struct BrowserBridgeCommandIssueRequest<'a> {
    pub exchange: &'a BrowserBridgeExchange,
    pub command_artifact: SecretValue,
    pub runtime_state: Option<BrowserBridgeRuntimeStateIssue>,
    pub workflow_context: Option<BrowserBridgeWorkflowContextIssue>,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct BrowserBridgeRuntimeStateIssue {
    pub metadata: BrowserBridgeRuntimeStateMetadata,
    pub state_artifact: SecretValue,
}

#[derive(Debug)]
pub struct BrowserBridgeWorkflowContextIssue {
    pub runtime_settings: ResolvedProviderRuntimeSettings,
    pub workflow_plan: Option<BrowserBridgeWorkflowPlanIssue>,
}

#[derive(Debug)]
pub struct BrowserBridgeWorkflowPlanIssue {
    pub artifact_type: String,
    pub artifact: SecretValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeCommandResolveRequest<'a> {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct ResolvedBrowserBridgeCommand {
    pub exchange: BrowserBridgeExchange,
    pub command_artifact: SecretValue,
    pub runtime_state: Option<ResolvedBrowserBridgeRuntimeState>,
    pub workflow_context: Option<ResolvedBrowserBridgeWorkflowContext>,
}

#[derive(Debug)]
pub struct ResolvedBrowserBridgeRuntimeState {
    pub metadata: BrowserBridgeRuntimeStateMetadata,
    pub state_artifact: SecretValue,
}

#[derive(Debug)]
pub struct ResolvedBrowserBridgeWorkflowContext {
    pub runtime_settings: ResolvedProviderRuntimeSettings,
    pub workflow_plan: Option<ResolvedBrowserBridgeWorkflowPlan>,
}

#[derive(Debug)]
pub struct ResolvedBrowserBridgeWorkflowPlan {
    pub artifact_type: String,
    pub artifact_digest: [u8; 32],
    pub artifact: SecretValue,
}

#[derive(Debug)]
pub struct DispatchedBrowserBridgeCommand {
    pub exchange: BrowserBridgeExchange,
    pub command_artifact: SecretValue,
}

#[derive(Debug)]
pub struct BrowserBridgeCommandDispatchRequest<'a> {
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub access_token_digest: &'a TokenDigest,
    pub dispatched_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub enum BrowserBridgeCommandDispatchRecord {
    Dispatched(DispatchedBrowserBridgeCommand),
    AccessRejected,
    NotFound,
    AlreadyDispatched,
    SequenceConflict,
}

/// Provider-scoped encrypted persistence for exact `BrowserBridge` commands.
/// Issuance stores the immutable exchange and its command bytes atomically;
/// resolution re-checks the complete owner/account/Task/Provider binding.
#[async_trait]
pub trait BrowserBridgeCommandArtifactRepository: Send + Sync {
    async fn issue_browser_bridge_command(
        &self,
        request: BrowserBridgeCommandIssueRequest<'_>,
    ) -> Result<BrowserBridgeExchangeRecord, SecretStoreError>;

    async fn resolve_browser_bridge_command(
        &self,
        request: BrowserBridgeCommandResolveRequest<'_>,
    ) -> Result<Option<ResolvedBrowserBridgeCommand>, SecretStoreError>;

    async fn dispatch_browser_bridge_command(
        &self,
        request: BrowserBridgeCommandDispatchRequest<'_>,
    ) -> Result<BrowserBridgeCommandDispatchRecord, SecretStoreError>;

    async fn receive_browser_bridge_result(
        &self,
        request: BrowserBridgeResultReceiveRequest<'_>,
    ) -> Result<BrowserBridgeResultArtifactRecord, SecretStoreError>;

    async fn resolve_browser_bridge_result(
        &self,
        request: BrowserBridgeResultResolveRequest<'_>,
    ) -> Result<Option<ResolvedBrowserBridgeResult>, SecretStoreError>;

    /// Atomically consumes one claimed result and either stores the exact next
    /// command or terminates the helper session after verified readback.
    async fn commit_browser_bridge_workflow_result(
        &self,
        request: BrowserBridgeWorkflowCommitRequest<'_>,
    ) -> Result<BrowserBridgeWorkflowCommitOutcome, SecretStoreError>;
}

#[derive(Debug)]
pub struct BrowserBridgeWorkflowCommitRequest<'a> {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub transition: BrowserBridgeWorkflowResult,
    pub worker_id: &'a str,
    pub committed_at: Timestamp,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub enum BrowserBridgeWorkflowCommitOutcome {
    IntermediateCommitted {
        completed_exchange: BrowserBridgeExchange,
        next_exchange: BrowserBridgeExchange,
    },
    ExecutionTerminalCommitted {
        session: BrowserBridgeSession,
        completed_exchange: BrowserBridgeExchange,
    },
    BindingConflict,
    SequenceConflict,
    ClaimConflict,
}

#[derive(Debug)]
pub struct BrowserBridgeResultReceiveRequest<'a> {
    pub metadata: &'a BrowserBridgeResultArtifactMetadata,
    pub result_artifact: SecretValue,
    pub access_token_digest: &'a TokenDigest,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeResultResolveRequest<'a> {
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub session_id: BrowserBridgeSessionId,
    pub sequence: u64,
    pub access: &'a SecretAccess,
}

#[derive(Debug)]
pub struct ResolvedBrowserBridgeResult {
    pub exchange: BrowserBridgeExchange,
    pub metadata: BrowserBridgeResultArtifactMetadata,
    pub result_artifact: SecretValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserBridgeResultArtifactRecord {
    Inserted(BrowserBridgeResultArtifactMetadata),
    Duplicate(BrowserBridgeResultArtifactMetadata),
    AccessRejected,
    SequenceConflict,
}

#[derive(Debug)]
pub struct BrowserBridgeCredentialCommitRequest<'a> {
    pub exchange: &'a BrowserBridgeExchange,
    pub owner_user_id: UserId,
    pub provider_account_id: ProviderAccountId,
    pub task_id: TaskId,
    pub validated_bundle: CredentialBundle,
    pub access: &'a SecretAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserBridgeCredentialCommit {
    pub session: BrowserBridgeSession,
    pub exchange: BrowserBridgeExchange,
    pub credentials: Vec<ProviderCredential>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserBridgeCredentialCommitOutcome {
    Committed(Box<BrowserBridgeCredentialCommit>),
    AccessRejected,
    BindingConflict,
    SequenceConflict,
}

/// Atomic terminal-result and credential replacement boundary for a claimed
/// `BrowserBridge` command whose Provider result has already been validated.
#[async_trait]
pub trait BrowserBridgeCredentialRepository: Send + Sync {
    async fn commit_browser_bridge_credentials(
        &self,
        request: BrowserBridgeCredentialCommitRequest<'_>,
    ) -> Result<BrowserBridgeCredentialCommitOutcome, SecretStoreError>;
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
    pub completion_policy: CompletionPolicySnapshot,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionStrictCompletionRetryRequest {
    pub workflow_id: asterism_domain::StrictCompletionWorkflowId,
    pub expected_revision: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionStrictCompletionRetryConfirmation {
    pub execution_id: ExecutionId,
    pub workflow_id: asterism_domain::StrictCompletionWorkflowId,
    pub workflow_revision: u32,
    pub confirmed_by: UserId,
    pub confirmed_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ExecutionScheduleRequest<'a> {
    pub execution: &'a Execution,
    /// Exact Provider-approved phase order. This is persisted atomically with
    /// the Execution and must be a permutation of `requested_capabilities`.
    pub capability_plan: &'a [TaskCapability],
    /// One-based global plan positions which start a Provider call. The first
    /// value is always one and the sequence is strictly increasing.
    pub capability_call_starts: &'a [u8],
    pub provider_plan_artifact: Option<&'a ProviderExecutionPlanArtifact>,
    pub billing: Option<ExecutionBillingReservation<'a>>,
    pub runtime_settings: Option<ExecutionRuntimeSettingsResolution<'a>>,
    pub strict_completion_retry: Option<ExecutionStrictCompletionRetryRequest>,
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
    pub call_position: u8,
    pub call_member_position: u8,
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

#[derive(Clone, Debug)]
pub struct ExecutionCapabilityCallMutation<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub call_position: u8,
    pub capabilities: &'a [TaskCapability],
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub correlation_id: &'a str,
    pub at: Timestamp,
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

    async fn issue_execution_capability_call(
        &self,
        request: ExecutionCapabilityCallMutation<'_>,
    ) -> Result<ExecutionCapabilityStepIssueOutcome, StorageError>;

    async fn succeed_execution_capability_call(
        &self,
        request: ExecutionCapabilityCallMutation<'_>,
    ) -> Result<(), StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAtomicMutation {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub ordinal: u32,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: String,
    pub operation_type: String,
    pub request_digest: [u8; 32],
    pub response_digest: Option<[u8; 32]>,
    pub accepted: Option<bool>,
    pub verification_digest: Option<[u8; 32]>,
    pub verified: Option<bool>,
    pub issued_at: Timestamp,
    pub received_at: Option<Timestamp>,
    pub verified_at: Option<Timestamp>,
}

#[derive(Clone, Debug)]
pub struct ExecutionAtomicMutationIssueRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub ordinal: u32,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub operation_type: &'a str,
    pub request_digest: [u8; 32],
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionAtomicMutationIssueOutcome {
    Issued(ExecutionAtomicMutation),
    AlreadyIssued(ExecutionAtomicMutation),
}

#[derive(Clone, Debug)]
pub struct ExecutionAtomicMutationReceiptRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub ordinal: u32,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub response_digest: [u8; 32],
    pub accepted: bool,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionAtomicMutationReceiptOutcome {
    Recorded(ExecutionAtomicMutation),
    AlreadyRecorded(ExecutionAtomicMutation),
}

#[derive(Clone, Debug)]
pub struct ExecutionAtomicMutationVerificationRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub ordinal: u32,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub observation_digest: [u8; 32],
    pub verified: bool,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionAtomicMutationVerificationOutcome {
    Recorded(ExecutionAtomicMutation),
    AlreadyRecorded(ExecutionAtomicMutation),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAtomicMutationPlanRecord {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: String,
    pub plan: ExecutionMutationPlan,
    pub prepared_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ExecutionAtomicMutationPlanPrepareRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub plan: &'a ExecutionMutationPlan,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionAtomicMutationPlanPrepareOutcome {
    Prepared(ExecutionAtomicMutationPlanRecord),
    AlreadyPrepared(ExecutionAtomicMutationPlanRecord),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAtomicMutationSequencePlanRecord {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: String,
    pub plan: ExecutionMutationSequencePlan,
    pub prepared_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ExecutionAtomicMutationSequencePlanPrepareRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub plan: &'a ExecutionMutationSequencePlan,
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionAtomicMutationSequencePlanPrepareOutcome {
    Prepared(ExecutionAtomicMutationSequencePlanRecord),
    AlreadyPrepared(ExecutionAtomicMutationSequencePlanRecord),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionAtomicMutationSequenceObservationRecord {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub phase_position: u8,
    pub observation_type: String,
    pub observation_digest: [u8; 32],
    pub observed_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ExecutionAtomicMutationSequenceObservationRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
    pub phase_position: u8,
    pub observation_type: &'a str,
    pub observation_digest: [u8; 32],
    pub correlation_id: &'a str,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionAtomicMutationSequenceObservationOutcome {
    Recorded(ExecutionAtomicMutationSequenceObservationRecord),
    AlreadyRecorded(ExecutionAtomicMutationSequenceObservationRecord),
}

/// Durable, ordered issue/receipt ledger for the individual remote mutations
/// inside one Provider-owned atomic operation. An issued row without a receipt
/// is intentionally not replayable and prevents issuing the next ordinal.
#[async_trait]
pub trait ExecutionAtomicMutationRepository: Send + Sync {
    async fn find_execution_atomic_mutation_plan(
        &self,
        execution_id: ExecutionId,
        attempt_id: ExecutionAttemptId,
    ) -> Result<Option<ExecutionAtomicMutationPlanRecord>, StorageError>;

    async fn prepare_execution_atomic_mutation_plan(
        &self,
        request: ExecutionAtomicMutationPlanPrepareRequest<'_>,
    ) -> Result<ExecutionAtomicMutationPlanPrepareOutcome, StorageError>;

    async fn find_execution_atomic_mutation_sequence_plan(
        &self,
        execution_id: ExecutionId,
        attempt_id: ExecutionAttemptId,
    ) -> Result<Option<ExecutionAtomicMutationSequencePlanRecord>, StorageError>;

    async fn prepare_execution_atomic_mutation_sequence_plan(
        &self,
        request: ExecutionAtomicMutationSequencePlanPrepareRequest<'_>,
    ) -> Result<ExecutionAtomicMutationSequencePlanPrepareOutcome, StorageError>;

    async fn find_execution_atomic_mutation_sequence_observations(
        &self,
        execution_id: ExecutionId,
        attempt_id: ExecutionAttemptId,
    ) -> Result<Vec<ExecutionAtomicMutationSequenceObservationRecord>, StorageError>;

    async fn record_execution_atomic_mutation_sequence_observation(
        &self,
        request: ExecutionAtomicMutationSequenceObservationRequest<'_>,
    ) -> Result<ExecutionAtomicMutationSequenceObservationOutcome, StorageError>;

    async fn find_execution_atomic_mutations(
        &self,
        execution_id: ExecutionId,
        attempt_id: ExecutionAttemptId,
    ) -> Result<Vec<ExecutionAtomicMutation>, StorageError>;

    async fn issue_execution_atomic_mutation(
        &self,
        request: ExecutionAtomicMutationIssueRequest<'_>,
    ) -> Result<ExecutionAtomicMutationIssueOutcome, StorageError>;

    async fn record_execution_atomic_mutation_receipt(
        &self,
        request: ExecutionAtomicMutationReceiptRequest<'_>,
    ) -> Result<ExecutionAtomicMutationReceiptOutcome, StorageError>;

    async fn record_execution_atomic_mutation_verification(
        &self,
        request: ExecutionAtomicMutationVerificationRequest<'_>,
    ) -> Result<ExecutionAtomicMutationVerificationOutcome, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionScheduleOutcome {
    Created(Execution),
    Existing(Execution),
    IdempotencyConflict,
    SubmissionDraftConflict,
    TaskStateConflict,
    RuntimeSettingsConflict,
    StrictCompletionRetryConflict,
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

/// Finishes one successful submission Execution after the accepted remote
/// response materialized another immutable Question. The Execution is
/// terminal, but the Task returns to `ready` for an explicitly reviewed next
/// Draft instead of being marked complete.
#[derive(Clone, Debug)]
pub struct ExecutionQuestionStepFinishRequest<'a> {
    pub execution_id: ExecutionId,
    pub attempt_id: ExecutionAttemptId,
    pub transition: &'a QuestionSessionTransition,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: &'a str,
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

    async fn find_execution_strict_completion_retry_confirmation(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionStrictCompletionRetryConfirmation>, StorageError>;

    async fn find_execution_provider_plan_artifact(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ProviderExecutionPlanArtifact>, StorageError>;

    async fn find_active_execution_attempt_id(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<ExecutionAttemptId>, StorageError>;

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

    async fn finish_question_step(
        &self,
        request: ExecutionQuestionStepFinishRequest<'_>,
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
