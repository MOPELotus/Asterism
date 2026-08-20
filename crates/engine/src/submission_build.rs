use std::{collections::BTreeMap, sync::Arc};

use asterism_domain::{
    AnswerCandidateId, AuthState, ProviderId, SelectedAnswer, SubmissionAnswerCoverage,
    SubmissionDraft, SubmissionDraftId, TaskCapability, TaskId, UserId,
};
use asterism_provider_api::{
    ProviderContext, ProviderEntry, ProviderError, ProviderRegistry,
    ResolvedProviderRuntimeSettings,
};
use asterism_storage::{
    AnswerCandidateRepository, ProtocolObservationRepository, ProviderAccountRuntimeRepository,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget, QuestionSnapshotRepository,
    StorageError, SubmissionDraftRepository, TaskQueryRepository,
};
use chrono::Utc;

use crate::{
    AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action,
    protocol_observation::{
        ProviderProtocolObservationRecordError, record_provider_protocol_observation,
    },
};

const MAX_CORRELATION_ID_BYTES: usize = 128;
const MAX_SELECTED_CANDIDATES: usize = 5_000;

#[derive(Clone, Debug)]
pub struct BuildSubmissionDraftCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub question_snapshot_id: asterism_domain::QuestionSnapshotId,
    pub answer_candidate_ids: Vec<AnswerCandidateId>,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubmissionDraftBuildResult {
    pub draft: SubmissionDraft,
}

#[derive(Clone)]
pub struct SubmissionDraftBuildService<Q, A, S, R> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
    submissions: S,
    runtime_settings: R,
    protocol_observations: Option<Arc<dyn ProtocolObservationRepository>>,
}

impl<Q, A, S, R> SubmissionDraftBuildService<Q, A, S, R> {
    pub const fn new(
        registry: Arc<ProviderRegistry>,
        tasks: Q,
        accounts: A,
        submissions: S,
        runtime_settings: R,
    ) -> Self {
        Self {
            registry,
            tasks,
            accounts,
            submissions,
            runtime_settings,
            protocol_observations: None,
        }
    }

    #[must_use]
    pub fn with_protocol_observations(
        mut self,
        observations: Arc<dyn ProtocolObservationRepository>,
    ) -> Self {
        self.protocol_observations = Some(observations);
        self
    }
}

impl<Q, A, S, R> std::fmt::Debug for SubmissionDraftBuildService<Q, A, S, R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubmissionDraftBuildService")
            .field("registry", &self.registry)
            .field("tasks", &"configured")
            .field("accounts", &"configured")
            .field("submissions", &"configured")
            .field("runtime_settings", &"configured")
            .field(
                "protocol_observations",
                &self.protocol_observations.is_some(),
            )
            .finish()
    }
}

impl<Q, A, S, R> SubmissionDraftBuildService<Q, A, S, R>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
    S: QuestionSnapshotRepository + AnswerCandidateRepository + SubmissionDraftRepository,
    R: ProviderRuntimeSettingsRepository,
{
    /// Builds and persists one complete, reviewable draft from explicit
    /// persisted Candidate identities without mutating remote state.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionDraftBuildError`] for ownership, capability,
    /// account, snapshot, selection, Provider preview, or storage failures.
    #[allow(
        clippy::too_many_lines,
        reason = "the orchestration keeps every ownership, capability, snapshot, selection and persistence guard in one visible commit path"
    )]
    pub async fn build(
        &self,
        command: BuildSubmissionDraftCommand,
    ) -> Result<SubmissionDraftBuildResult, SubmissionDraftBuildError> {
        if !valid_correlation_id(&command.correlation_id) {
            return Err(SubmissionDraftBuildError::InvalidCorrelationId);
        }
        if command.answer_candidate_ids.is_empty()
            || command.answer_candidate_ids.len() > MAX_SELECTED_CANDIDATES
        {
            return Err(SubmissionDraftBuildError::SelectionInvalid);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(SubmissionDraftBuildError::TaskNotFound)?;
        if !task.capabilities.contains(&TaskCapability::SubmissionBuild) {
            return Err(SubmissionDraftBuildError::TaskCapabilityUnavailable);
        }
        authorize_task_action(&task, TaskAction::Build, FormalAssessmentPolicy::default())?;
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(SubmissionDraftBuildError::TaskNotFound)?;
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(SubmissionDraftBuildError::AccountNotAuthenticated);
        }
        let snapshot = self
            .submissions
            .find_owned_question_snapshot(command.owner_id, command.question_snapshot_id)
            .await?
            .ok_or(SubmissionDraftBuildError::QuestionSnapshotNotFound)?;
        if snapshot.task_id != task.id || snapshot.provider_id != account.provider_id {
            return Err(SubmissionDraftBuildError::QuestionSnapshotBindingInvalid);
        }

        let candidate_records = self
            .submissions
            .list_owned_answer_candidates(command.owner_id, snapshot.id)
            .await?;
        let candidates = candidate_records
            .into_iter()
            .map(|record| (record.id, record))
            .collect::<BTreeMap<_, _>>();
        let mut selected_by_question = BTreeMap::new();
        for candidate_id in command.answer_candidate_ids {
            let record = candidates
                .get(&candidate_id)
                .ok_or(SubmissionDraftBuildError::SelectionInvalid)?;
            if record.question_snapshot_id != snapshot.id
                || record.candidate.validate().is_err()
                || selected_by_question
                    .insert(record.candidate.question_id, record)
                    .is_some()
            {
                return Err(SubmissionDraftBuildError::SelectionInvalid);
            }
        }
        let mut selected_questions = Vec::with_capacity(selected_by_question.len());
        let mut selected_answers = Vec::with_capacity(selected_by_question.len());
        let mut unanswered_question_ids =
            Vec::with_capacity(snapshot.questions.len() - selected_by_question.len());
        for question in &snapshot.questions {
            if let Some(record) = selected_by_question.get(&question.id) {
                selected_questions.push(question.clone());
                selected_answers.push(SelectedAnswer {
                    candidate_id: record.id,
                    question_id: question.id,
                    answer: record.candidate.answer.clone(),
                    source: record.candidate.source,
                    confidence: record.candidate.confidence,
                });
            } else {
                unanswered_question_ids.push(question.id);
            }
        }

        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            SubmissionDraftBuildError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let builder = entry.submission_build.as_ref().ok_or_else(|| {
            SubmissionDraftBuildError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let runtime_settings = self
            .resolve_runtime_settings(&account, task.id, entry)
            .await?;
        let minimum_coverage_millis = entry
            .runtime_settings
            .minimum_answer_coverage_millis(&runtime_settings)
            .map_err(|_| SubmissionDraftBuildError::RuntimeSettingsInvalid)?;
        if selected_answers.len() * 1_000
            < snapshot.questions.len() * usize::from(minimum_coverage_millis)
        {
            return Err(SubmissionDraftBuildError::SelectionIncomplete);
        }
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs,
            correlation_id: command.correlation_id,
        };
        let payload_preview = match builder
            .build_submission_preview(
                &context,
                &task.remote_id,
                &selected_questions,
                &selected_answers,
            )
            .await
        {
            Ok(preview) => preview,
            Err(error) => {
                let occurrence_scope = format!(
                    "submission-build:{}:{}:{}",
                    task.id, snapshot.id, context.correlation_id
                );
                record_provider_protocol_observation(
                    self.protocol_observations.as_deref(),
                    &account.provider_id,
                    None,
                    &occurrence_scope,
                    &error,
                    Utc::now(),
                )
                .await
                .map_err(|error| match error {
                    ProviderProtocolObservationRecordError::Invalid => {
                        SubmissionDraftBuildError::InvalidProtocolObservation
                    }
                    ProviderProtocolObservationRecordError::Storage(error) => {
                        SubmissionDraftBuildError::Storage(error)
                    }
                })?;
                return Err(error.into());
            }
        };
        let items = selected_questions
            .into_iter()
            .zip(selected_answers)
            .map(|(question, selected)| asterism_domain::SubmissionDraftItem { question, selected })
            .collect();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id: task.id,
            question_snapshot_id: snapshot.id,
            provider_id: account.provider_id,
            provider_version: entry.metadata.implementation_version.clone(),
            answer_coverage: SubmissionAnswerCoverage {
                total_question_count: u32::try_from(snapshot.questions.len())
                    .map_err(|_| SubmissionDraftBuildError::ProviderPreviewInvalid)?,
                minimum_coverage_millis,
                unanswered_question_ids,
            },
            items,
            payload_preview,
            created_at: Utc::now(),
        };
        draft
            .validate()
            .map_err(|_| SubmissionDraftBuildError::ProviderPreviewInvalid)?;
        self.submissions.save_submission_draft(&draft).await?;
        Ok(SubmissionDraftBuildResult { draft })
    }

    async fn resolve_runtime_settings(
        &self,
        account: &asterism_domain::ProviderAccount,
        task_id: TaskId,
        entry: &ProviderEntry,
    ) -> Result<ResolvedProviderRuntimeSettings, SubmissionDraftBuildError> {
        let provider = self
            .runtime_settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Provider {
                provider_id: account.provider_id.clone(),
            })
            .await?;
        let provider_account = self
            .runtime_settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id: account.provider_id.clone(),
                provider_account_id: account.id,
            })
            .await?;
        let task = self
            .runtime_settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Task {
                provider_id: account.provider_id.clone(),
                provider_account_id: account.id,
                task_id,
            })
            .await?;
        entry
            .runtime_settings
            .resolve_with_sources(
                provider.as_ref().map(|record| &record.patch),
                provider_account.as_ref().map(|record| &record.patch),
                task.as_ref().map(|record| &record.patch),
            )
            .map(|(resolved, _)| resolved)
            .map_err(|_| SubmissionDraftBuildError::RuntimeSettingsInvalid)
    }
}

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CORRELATION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, thiserror::Error)]
pub enum SubmissionDraftBuildError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task does not advertise SubmissionBuild")]
    TaskCapabilityUnavailable,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("QuestionSnapshot was not found")]
    QuestionSnapshotNotFound,
    #[error("QuestionSnapshot is not bound to this Task and Provider")]
    QuestionSnapshotBindingInvalid,
    #[error("selected AnswerCandidate identities are foreign, duplicated, or invalid")]
    SelectionInvalid,
    #[error("exactly one AnswerCandidate must be selected for every Question")]
    SelectionIncomplete,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no SubmissionBuild implementation")]
    CapabilityUnavailable(ProviderId),
    #[error("submission build correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider supplied an invalid protocol observation")]
    InvalidProtocolObservation,
    #[error("Provider returned an invalid or unsafe submission preview")]
    ProviderPreviewInvalid,
    #[error("Provider runtime settings are invalid for submission coverage")]
    RuntimeSettingsInvalid,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use asterism_domain::{
        AnswerCandidate, AnswerConfidence, AnswerSource, AssessmentClass, NormalizedAnswer,
        OrchestrationState, ProtocolObservationKind, ProtocolSurface, ProviderAccount,
        ProviderAccountId, Question, QuestionId, QuestionKind, QuestionSnapshotId, RemoteState,
        SecretId, SourceType, SubmissionPayloadEncoding, SubmissionPayloadFieldPreview,
        SubmissionPayloadPreview, Task, Timestamp,
    };
    use asterism_provider_api::{
        ProviderCapability, ProviderEntry, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
        ProviderResult, ProviderRuntimeSettingsSchema, ProviderSettingCoreBehavior,
        ProviderSettingDefinition, ProviderSettingKind, ProviderSettingScope, ProviderSettingValue,
        SubmissionBuildCapability, VerificationLevel,
    };
    use asterism_storage::{
        AnswerCandidateRecord, Database, QuestionSnapshot, SqliteProtocolObservationRepository,
        SubmissionDraftRepository, TaskPage,
    };
    use async_trait::async_trait;

    use super::*;

    #[derive(Clone, Debug)]
    struct FakeRepository {
        owner: UserId,
        task: Task,
        account: ProviderAccount,
        snapshot: QuestionSnapshot,
        candidates: Vec<AnswerCandidateRecord>,
        saved: Arc<Mutex<Vec<SubmissionDraft>>>,
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
    impl ProviderRuntimeSettingsRepository for FakeRepository {
        async fn find_provider_runtime_settings(
            &self,
            _target: &ProviderRuntimeSettingsTarget,
        ) -> Result<Option<asterism_storage::ProviderRuntimeSettingsRecord>, StorageError> {
            Ok(None)
        }

        async fn write_provider_runtime_settings(
            &self,
            _request: asterism_storage::ProviderRuntimeSettingsWriteRequest<'_>,
        ) -> Result<asterism_storage::ProviderRuntimeSettingsWriteOutcome, StorageError> {
            unreachable!("submission build only reads runtime settings")
        }
    }

    #[async_trait]
    impl QuestionSnapshotRepository for FakeRepository {
        async fn save_question_snapshot(
            &self,
            _snapshot: &QuestionSnapshot,
        ) -> Result<(), StorageError> {
            unreachable!("submission build does not write Question snapshots")
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
            _candidates: &[AnswerCandidateRecord],
        ) -> Result<(), StorageError> {
            unreachable!("submission build only reads Candidates")
        }

        async fn list_owned_answer_candidates(
            &self,
            owner_id: UserId,
            snapshot_id: QuestionSnapshotId,
        ) -> Result<Vec<AnswerCandidateRecord>, StorageError> {
            Ok(
                if owner_id == self.owner && snapshot_id == self.snapshot.id {
                    self.candidates.clone()
                } else {
                    Vec::new()
                },
            )
        }
    }

    #[async_trait]
    impl SubmissionDraftRepository for FakeRepository {
        async fn save_submission_draft(&self, draft: &SubmissionDraft) -> Result<(), StorageError> {
            self.saved.lock().unwrap().push(draft.clone());
            Ok(())
        }

        async fn find_owned_submission_draft(
            &self,
            owner_id: UserId,
            draft_id: SubmissionDraftId,
        ) -> Result<Option<SubmissionDraft>, StorageError> {
            Ok(self
                .saved
                .lock()
                .unwrap()
                .iter()
                .find(|draft| owner_id == self.owner && draft.id == draft_id)
                .cloned())
        }
    }

    #[derive(Debug)]
    struct FakeBuilder {
        metadata: ProviderMetadata,
        foreign_preview: bool,
        protocol_drift: bool,
        called: AtomicBool,
    }

    impl ProviderIdentity for FakeBuilder {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl SubmissionBuildCapability for FakeBuilder {
        async fn build_submission_preview(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            questions: &[Question],
            _selected_answers: &[SelectedAnswer],
        ) -> ProviderResult<SubmissionPayloadPreview> {
            self.called.store(true, Ordering::Relaxed);
            if self.protocol_drift {
                return Err(ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "submission question type changed",
                )
                .try_with_protocol_observation(
                    ProtocolSurface::SubmissionBuild,
                    ProtocolObservationKind::UnknownQuestionKind,
                    serde_json::json!({"question_type": 991, "page_kind": "work"}),
                )
                .unwrap());
            }
            Ok(SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::Form,
                format: "fixture.work.v1".to_owned(),
                fields: vec![SubmissionPayloadFieldPreview {
                    question_id: if self.foreign_preview {
                        QuestionId::new()
                    } else {
                        questions[0].id
                    },
                    field_name: "answer[question-1]".to_owned(),
                }],
            })
        }
    }

    #[tokio::test]
    async fn complete_explicit_selection_builds_and_persists_one_draft() {
        let fixture = fixture(FixtureMode::Complete);
        let result = fixture
            .service
            .build(fixture.command(vec![fixture.repository.candidates[0].id]))
            .await
            .unwrap();
        assert_eq!(result.draft.items.len(), 1);
        assert_eq!(
            result.draft.items[0].selected.candidate_id,
            fixture.repository.candidates[0].id
        );
        assert!(fixture.builder.called.load(Ordering::Relaxed));
        assert_eq!(fixture.repository.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_selection_never_calls_provider_or_persists() {
        let fixture = fixture(FixtureMode::Complete);
        assert!(matches!(
            fixture
                .service
                .build(fixture.command(vec![AnswerCandidateId::new()]))
                .await,
            Err(SubmissionDraftBuildError::SelectionInvalid)
        ));
        assert!(!fixture.builder.called.load(Ordering::Relaxed));
        assert!(fixture.repository.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn foreign_provider_preview_is_rejected_without_persistence() {
        let fixture = fixture(FixtureMode::ForeignPreview);
        assert!(matches!(
            fixture
                .service
                .build(fixture.command(vec![fixture.repository.candidates[0].id]))
                .await,
            Err(SubmissionDraftBuildError::ProviderPreviewInvalid)
        ));
        assert!(fixture.builder.called.load(Ordering::Relaxed));
        assert!(fixture.repository.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn undeclared_submission_build_never_calls_provider() {
        let fixture = fixture(FixtureMode::CapabilityUnavailable);
        assert!(matches!(
            fixture
                .service
                .build(fixture.command(vec![fixture.repository.candidates[0].id]))
                .await,
            Err(SubmissionDraftBuildError::TaskCapabilityUnavailable)
        ));
        assert!(!fixture.builder.called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn declared_partial_coverage_freezes_unanswered_snapshot_partition() {
        let fixture = fixture(FixtureMode::PartialPolicy);
        let result = fixture
            .service
            .build(fixture.command(vec![fixture.repository.candidates[0].id]))
            .await
            .unwrap();
        assert_eq!(result.draft.items.len(), 1);
        assert_eq!(result.draft.answer_coverage.total_question_count, 2);
        assert_eq!(result.draft.answer_coverage.minimum_coverage_millis, 500);
        assert_eq!(
            result.draft.answer_coverage.unanswered_question_ids.len(),
            1
        );
        assert_eq!(
            result.draft.answer_coverage.unanswered_question_ids[0],
            fixture.repository.snapshot.questions[1].id
        );
    }

    #[tokio::test]
    async fn provider_drift_is_observed_before_draft_build_fails() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let mut fixture = fixture(FixtureMode::ProtocolDrift);
        fixture.service = fixture.service.with_protocol_observations(Arc::new(
            SqliteProtocolObservationRepository::new(database.clone()),
        ));

        assert!(matches!(
            fixture
                .service
                .build(fixture.command(vec![fixture.repository.candidates[0].id]))
                .await,
            Err(SubmissionDraftBuildError::Provider(error))
                if error.kind == ProviderErrorKind::ProtocolDrift
        ));

        let observation: (String, String, i64, Option<String>) = sqlx::query_as(
            "SELECT surface, kind, occurrence_count, last_execution_id \
             FROM protocol_observations",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            observation,
            (
                "submission_build".to_owned(),
                "unknown_question_kind".to_owned(),
                1,
                None,
            )
        );
        assert!(fixture.repository.saved.lock().unwrap().is_empty());
    }

    struct Fixture {
        service: SubmissionDraftBuildService<
            FakeRepository,
            FakeRepository,
            FakeRepository,
            FakeRepository,
        >,
        repository: FakeRepository,
        builder: Arc<FakeBuilder>,
    }

    impl Fixture {
        fn command(
            &self,
            answer_candidate_ids: Vec<AnswerCandidateId>,
        ) -> BuildSubmissionDraftCommand {
            BuildSubmissionDraftCommand {
                owner_id: self.repository.owner,
                task_id: self.repository.task.id,
                question_snapshot_id: self.repository.snapshot.id,
                answer_candidate_ids,
                correlation_id: "submission-build-test".to_owned(),
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FixtureMode {
        CapabilityUnavailable,
        Complete,
        ForeignPreview,
        PartialPolicy,
        ProtocolDrift,
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps Task, account, snapshot, Candidate and Provider bindings visible together"
    )]
    fn fixture(mode: FixtureMode) -> Fixture {
        let advertises_build = mode != FixtureMode::CapabilityUnavailable;
        let foreign_preview = mode == FixtureMode::ForeignPreview;
        let partial_policy = mode == FixtureMode::PartialPolicy;
        let protocol_drift = mode == FixtureMode::ProtocolDrift;
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
            capabilities: advertises_build
                .then_some(TaskCapability::SubmissionBuild)
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
        let mut questions = vec![question];
        if partial_policy {
            questions.push(Question {
                id: QuestionId::new(),
                task_id,
                remote_question_id: Some("question-2".to_owned()),
                kind: QuestionKind::Unknown,
                stem: "Unsupported Question".to_owned(),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({}),
                position: 2,
            });
        }
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: provider_id.clone(),
            provider_version: "0.1.0".to_owned(),
            captured_at: now,
            questions,
            groups: Vec::new(),
        };
        let candidate = AnswerCandidateRecord {
            id: AnswerCandidateId::new(),
            question_snapshot_id: snapshot.id,
            candidate: AnswerCandidate {
                question_id,
                source: AnswerSource::Manual,
                answer: NormalizedAnswer::Selections(vec!["A".to_owned()]),
                confidence: Some(AnswerConfidence::try_new(9_000).unwrap()),
                explanation: None,
                provenance_sanitized: serde_json::json!({"manual": true}),
            },
            created_at: now,
        };
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: advertises_build
                .then_some(ProviderCapability::SubmissionBuild)
                .into_iter()
                .collect(),
            auth_methods: BTreeSet::default(),
            session_kinds: BTreeSet::default(),
        };
        let builder = Arc::new(FakeBuilder {
            metadata: metadata.clone(),
            foreign_preview,
            protocol_drift,
            called: AtomicBool::new(false),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        if partial_policy {
            entry.runtime_settings = ProviderRuntimeSettingsSchema {
                version: 1,
                definitions: vec![ProviderSettingDefinition {
                    key: "submission.minimum_answer_coverage".to_owned(),
                    display_name: "Minimum answer coverage".to_owned(),
                    description: "Minimum selected fraction of the full snapshot.".to_owned(),
                    kind: ProviderSettingKind::DecimalMillis {
                        minimum: 1,
                        maximum: 1_000,
                        step: 1,
                    },
                    default: ProviderSettingValue::DecimalMillis(500),
                    scopes: BTreeSet::from([
                        ProviderSettingScope::Provider,
                        ProviderSettingScope::ProviderAccount,
                        ProviderSettingScope::Task,
                    ]),
                    core_behavior: Some(ProviderSettingCoreBehavior::MinimumAnswerCoverage),
                }],
            };
        }
        if advertises_build {
            entry.submission_build = Some(builder.clone());
        }
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        let repository = FakeRepository {
            owner,
            task,
            account,
            snapshot,
            candidates: vec![candidate],
            saved: Arc::new(Mutex::new(Vec::new())),
        };
        let service = SubmissionDraftBuildService::new(
            Arc::new(registry),
            repository.clone(),
            repository.clone(),
            repository.clone(),
            repository.clone(),
        );
        Fixture {
            service,
            repository,
            builder,
        }
    }
}
