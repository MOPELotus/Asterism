use std::{collections::BTreeSet, sync::Arc};

use asterism_domain::{
    AnswerCandidateId, AnswerSource, AuthState, ProviderId, QuestionSnapshotId, TaskCapability,
    TaskId, UserId,
};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderRegistry, ResolvedProviderQuestionSessionContinuation,
};
use asterism_secrets::{SecretAccess, SecretActor, SecretStoreError};
use asterism_storage::{
    AnswerCandidateRecord, AnswerCandidateRepository, ProviderAccountRuntimeRepository,
    QuestionSessionArtifactRepositoryFactory, QuestionSnapshotRepository,
    ResolvedQuestionSessionContinuation, StorageError, TaskQueryRepository,
};
use chrono::Utc;

use crate::{AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action};

const MAX_CORRELATION_ID_BYTES: usize = 128;
const MAX_PROVIDER_CANDIDATES_PER_RESOLUTION: usize = 20_000;

#[derive(Clone, Debug)]
pub struct ResolveProviderAnswersCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderAnswerResolveResult {
    pub task_id: TaskId,
    pub question_snapshot_id: QuestionSnapshotId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub candidates: Vec<AnswerCandidateRecord>,
}

#[derive(Clone)]
pub struct ProviderAnswerResolveService<Q, A, S> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
    answers: S,
    question_sessions: Option<Arc<dyn QuestionSessionArtifactRepositoryFactory>>,
}

impl<Q, A, S> std::fmt::Debug for ProviderAnswerResolveService<Q, A, S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAnswerResolveService")
            .field("registry", &self.registry)
            .field("tasks", &"configured")
            .field("accounts", &"configured")
            .field("answers", &"configured")
            .field("question_sessions", &self.question_sessions.is_some())
            .finish()
    }
}

impl<Q, A, S> ProviderAnswerResolveService<Q, A, S> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A, answers: S) -> Self {
        Self {
            registry,
            tasks,
            accounts,
            answers,
            question_sessions: None,
        }
    }

    #[must_use]
    pub fn with_question_session_artifacts(
        mut self,
        artifacts: Arc<dyn QuestionSessionArtifactRepositoryFactory>,
    ) -> Self {
        self.question_sessions = Some(artifacts);
        self
    }
}

impl<Q, A, S> ProviderAnswerResolveService<Q, A, S>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
    S: QuestionSnapshotRepository + AnswerCandidateRepository,
{
    /// Resolves Provider-native candidates for one explicit immutable Question
    /// snapshot and persists the complete validated candidate batch.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderAnswerResolveError`] for ownership, capability,
    /// account, snapshot binding, Provider output, or storage failures.
    pub async fn resolve(
        &self,
        command: ResolveProviderAnswersCommand,
    ) -> Result<ProviderAnswerResolveResult, ProviderAnswerResolveError> {
        if !valid_correlation_id(&command.correlation_id) {
            return Err(ProviderAnswerResolveError::InvalidCorrelationId);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ProviderAnswerResolveError::TaskNotFound)?;
        if !task.capabilities.contains(&TaskCapability::AnswerResolve) {
            return Err(ProviderAnswerResolveError::TaskCapabilityUnavailable);
        }
        authorize_task_action(
            &task,
            TaskAction::Resolve,
            FormalAssessmentPolicy::default(),
        )?;
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(ProviderAnswerResolveError::TaskNotFound)?;
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(ProviderAnswerResolveError::AccountNotAuthenticated);
        }
        let snapshot = self
            .answers
            .find_owned_question_snapshot(command.owner_id, command.question_snapshot_id)
            .await?
            .ok_or(ProviderAnswerResolveError::QuestionSnapshotNotFound)?;
        if snapshot.task_id != task.id || snapshot.provider_id != account.provider_id {
            return Err(ProviderAnswerResolveError::QuestionSnapshotBindingInvalid);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            ProviderAnswerResolveError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let resolver = entry.answer_resolve.as_ref().ok_or_else(|| {
            ProviderAnswerResolveError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs,
            correlation_id: command.correlation_id,
        };
        let access = SecretAccess {
            actor: SecretActor::CoreService("answer-resolve"),
            correlation_id: context.correlation_id.clone(),
            reason: "Provider-native AnswerResolve QuestionSession artifact".to_owned(),
        };
        let resolved_session = if let Some(factory) = &self.question_sessions {
            factory
                .for_provider(account.provider_id.clone())
                .resolve_active_question_session_continuation(
                    command.owner_id,
                    snapshot.id,
                    &access,
                )
                .await?
        } else {
            None
        };
        let candidates = resolver
            .resolve_answers_with_session(
                &context,
                &task.remote_id,
                &snapshot.questions,
                resolved_session.as_ref().map(provider_session_continuation),
            )
            .await?;
        validate_candidates(&snapshot.questions, &candidates)?;
        let created_at = Utc::now();
        let records = candidates
            .into_iter()
            .map(|candidate| AnswerCandidateRecord {
                id: AnswerCandidateId::new(),
                question_snapshot_id: snapshot.id,
                candidate,
                created_at,
            })
            .collect::<Vec<_>>();
        if !records.is_empty() {
            self.answers.save_answer_candidate_batch(&records).await?;
        }
        Ok(ProviderAnswerResolveResult {
            task_id: task.id,
            question_snapshot_id: snapshot.id,
            provider_id: account.provider_id,
            provider_version: entry.metadata.implementation_version.clone(),
            candidates: records,
        })
    }
}

fn provider_session_continuation(
    resolved: &ResolvedQuestionSessionContinuation,
) -> ResolvedProviderQuestionSessionContinuation<'_> {
    ResolvedProviderQuestionSessionContinuation {
        continuation_type: &resolved.metadata.continuation_type,
        continuation_digest: resolved.metadata.continuation_digest,
        phase: &resolved.metadata.phase,
        revision: resolved.metadata.revision,
        value: &resolved.value,
    }
}

fn validate_candidates(
    questions: &[asterism_domain::Question],
    candidates: &[asterism_domain::AnswerCandidate],
) -> Result<(), ProviderAnswerResolveError> {
    if candidates.len() > MAX_PROVIDER_CANDIDATES_PER_RESOLUTION {
        return Err(ProviderAnswerResolveError::ProviderResponseInvalid);
    }
    let question_ids = questions
        .iter()
        .map(|question| question.id)
        .collect::<BTreeSet<_>>();
    let mut unique = BTreeSet::new();
    for candidate in candidates {
        let encoded = serde_json::to_vec(candidate)
            .map_err(|_| ProviderAnswerResolveError::ProviderResponseInvalid)?;
        if candidate.source != AnswerSource::ProviderNative
            || !question_ids.contains(&candidate.question_id)
            || candidate.validate().is_err()
            || !unique.insert(encoded)
        {
            return Err(ProviderAnswerResolveError::ProviderResponseInvalid);
        }
    }
    Ok(())
}

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CORRELATION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderAnswerResolveError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task does not advertise AnswerResolve")]
    TaskCapabilityUnavailable,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("QuestionSnapshot was not found")]
    QuestionSnapshotNotFound,
    #[error("QuestionSnapshot is not bound to this Task and Provider")]
    QuestionSnapshotBindingInvalid,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no AnswerResolve implementation")]
    CapabilityUnavailable(ProviderId),
    #[error("answer resolution correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider returned foreign, duplicate, unsanitized, or unbounded candidates")]
    ProviderResponseInvalid,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Secret(#[from] SecretStoreError),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use asterism_domain::{
        AnswerCandidate, AnswerConfidence, AssessmentClass, AuthState, NormalizedAnswer,
        OrchestrationState, ProviderAccount, ProviderAccountId, Question, QuestionId, QuestionKind,
        RemoteState, SecretId, SourceType, Task, Timestamp,
    };
    use asterism_provider_api::{
        AnswerResolveCapability, ProviderCapability, ProviderEntry, ProviderIdentity,
        ProviderMetadata, ProviderResult, ProviderRuntimeSettingsSchema, VerificationLevel,
    };
    use asterism_storage::{QuestionSnapshot, TaskPage};
    use async_trait::async_trait;

    use super::*;

    #[derive(Clone, Debug)]
    struct FakeRepository {
        owner: UserId,
        task: Task,
        account: ProviderAccount,
        snapshot: QuestionSnapshot,
        saved: Arc<Mutex<Vec<AnswerCandidateRecord>>>,
    }

    #[async_trait]
    impl TaskQueryRepository for FakeRepository {
        async fn list_owned_tasks(
            &self,
            owner_id: UserId,
            _provider_account_id: Option<ProviderAccountId>,
            _limit: u32,
            _offset: u64,
        ) -> Result<TaskPage, StorageError> {
            let items = (owner_id == self.owner)
                .then(|| self.task.clone())
                .into_iter()
                .collect();
            Ok(TaskPage {
                total: u64::from(owner_id == self.owner),
                items,
            })
        }

        async fn find_owned_task(
            &self,
            owner_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<Task>, StorageError> {
            Ok((owner_id == self.owner && task_id == self.task.id).then(|| self.task.clone()))
        }
    }

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FakeRepository {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((account_id == self.account.id).then(|| self.account.clone()))
        }
    }

    #[async_trait]
    impl QuestionSnapshotRepository for FakeRepository {
        async fn save_question_snapshot(
            &self,
            _snapshot: &QuestionSnapshot,
        ) -> Result<(), StorageError> {
            unreachable!("answer resolution does not write Question snapshots")
        }

        async fn find_owned_question_snapshot(
            &self,
            owner_id: UserId,
            snapshot_id: QuestionSnapshotId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok((owner_id == self.owner && snapshot_id == self.snapshot.id)
                .then(|| self.snapshot.clone()))
        }

        async fn find_latest_owned_question_snapshot(
            &self,
            owner_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok((owner_id == self.owner && task_id == self.snapshot.task_id)
                .then(|| self.snapshot.clone()))
        }
    }

    #[async_trait]
    impl AnswerCandidateRepository for FakeRepository {
        async fn save_answer_candidate_batch(
            &self,
            candidates: &[AnswerCandidateRecord],
        ) -> Result<(), StorageError> {
            self.saved.lock().unwrap().extend_from_slice(candidates);
            Ok(())
        }

        async fn list_owned_answer_candidates(
            &self,
            owner_id: UserId,
            snapshot_id: QuestionSnapshotId,
        ) -> Result<Vec<AnswerCandidateRecord>, StorageError> {
            Ok(
                if owner_id == self.owner && snapshot_id == self.snapshot.id {
                    self.saved.lock().unwrap().clone()
                } else {
                    Vec::new()
                },
            )
        }
    }

    #[derive(Debug)]
    struct FakeResolver {
        metadata: ProviderMetadata,
        candidates: Mutex<Vec<AnswerCandidate>>,
        called: AtomicBool,
    }

    impl ProviderIdentity for FakeResolver {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl AnswerResolveCapability for FakeResolver {
        async fn resolve_answers(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _questions: &[Question],
        ) -> ProviderResult<Vec<AnswerCandidate>> {
            self.called.store(true, Ordering::Relaxed);
            Ok(self.candidates.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn provider_candidates_are_snapshot_bound_validated_and_persisted() {
        let fixture = fixture(true, AnswerSource::ProviderNative);
        let result = fixture
            .service
            .resolve(fixture.command("answer-resolve-1"))
            .await
            .unwrap();
        assert_eq!(result.question_snapshot_id, fixture.repository.snapshot.id);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(
            result.candidates[0].candidate.question_id,
            fixture.question_id
        );
        assert_eq!(fixture.repository.saved.lock().unwrap().len(), 1);
        assert!(fixture.resolver.called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn foreign_source_is_rejected_without_persistence() {
        let fixture = fixture(true, AnswerSource::ExternalBank);
        assert!(matches!(
            fixture
                .service
                .resolve(fixture.command("answer-resolve-2"))
                .await,
            Err(ProviderAnswerResolveError::ProviderResponseInvalid)
        ));
        assert!(fixture.repository.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn undeclared_capability_never_calls_provider() {
        let fixture = fixture(false, AnswerSource::ProviderNative);
        assert!(matches!(
            fixture
                .service
                .resolve(fixture.command("answer-resolve-3"))
                .await,
            Err(ProviderAnswerResolveError::TaskCapabilityUnavailable)
        ));
        assert!(!fixture.resolver.called.load(Ordering::Relaxed));
    }

    struct Fixture {
        service: ProviderAnswerResolveService<FakeRepository, FakeRepository, FakeRepository>,
        repository: FakeRepository,
        resolver: Arc<FakeResolver>,
        question_id: QuestionId,
    }

    impl Fixture {
        fn command(&self, correlation_id: &str) -> ResolveProviderAnswersCommand {
            ResolveProviderAnswersCommand {
                owner_id: self.repository.owner,
                task_id: self.repository.task.id,
                question_snapshot_id: self.repository.snapshot.id,
                correlation_id: correlation_id.to_owned(),
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps Task, account, immutable snapshot and Provider capability bindings visible together"
    )]
    fn fixture(advertises_answers: bool, source: AnswerSource) -> Fixture {
        let owner = UserId::new();
        let account_id = ProviderAccountId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let task_id = TaskId::new();
        let question_id = QuestionId::new();
        let now: Timestamp = Utc::now();
        let task = Task {
            id: task_id,
            provider_account_id: account_id,
            course_id: None,
            remote_id: "work:100:200:work-1".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Formal,
            title: "Work".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: advertises_answers
                .then_some(TaskCapability::AnswerResolve)
                .into_iter()
                .collect(),
        };
        let account = ProviderAccount {
            id: account_id,
            owner_id: owner,
            provider_id: provider_id.clone(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: vec![SecretId::new()],
            created_at: now,
            updated_at: now,
        };
        let question = Question {
            id: question_id,
            task_id,
            remote_question_id: Some("question-1".to_owned()),
            kind: QuestionKind::SingleChoice,
            stem: "Question".to_owned(),
            options: Vec::new(),
            attachments: Vec::new(),
            metadata_sanitized: serde_json::json!({}),
            position: 1,
        };
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: provider_id.clone(),
            provider_version: "0.1.0".to_owned(),
            captured_at: now,
            questions: vec![question],
        };
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([ProviderCapability::AnswerResolve]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let resolver = Arc::new(FakeResolver {
            metadata: metadata.clone(),
            candidates: Mutex::new(vec![AnswerCandidate {
                question_id,
                source,
                answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                confidence: Some(AnswerConfidence::try_new(9_000).unwrap()),
                explanation: None,
                provenance_sanitized: serde_json::json!({"native": true}),
            }]),
            called: AtomicBool::new(false),
        });
        let mut registry = ProviderRegistry::default();
        let mut entry = ProviderEntry {
            metadata,
            runtime_settings: ProviderRuntimeSettingsSchema::default(),
            authentication: None,
            course_inventory: None,
            task_inventory: None,
            task_detail: None,
            task_progress: None,
            duration_read: None,
            question_inventory: None,
            question_parse: None,
            answer_resolve: Some(resolver.clone()),
            submission_build: None,
            submission_execute: None,
            submission_verify: None,
            task_execution: None,
            browser_bridge: None,
        };
        if !advertises_answers {
            entry.metadata.capabilities.clear();
            entry.answer_resolve = None;
        }
        registry.register(entry).unwrap();
        let repository = FakeRepository {
            owner,
            task,
            account,
            snapshot,
            saved: Arc::new(Mutex::new(Vec::new())),
        };
        let service = ProviderAnswerResolveService::new(
            Arc::new(registry),
            repository.clone(),
            repository.clone(),
            repository.clone(),
        );
        Fixture {
            service,
            repository,
            resolver,
            question_id,
        }
    }
}
