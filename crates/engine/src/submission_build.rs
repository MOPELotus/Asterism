use std::{collections::BTreeMap, sync::Arc};

use asterism_domain::{
    AnswerCandidateId, AuthState, ProviderId, SelectedAnswer, SubmissionDraft, SubmissionDraftId,
    TaskCapability, TaskId, UserId,
};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderRegistry};
use asterism_storage::{
    AnswerCandidateRepository, ProviderAccountRuntimeRepository, QuestionSnapshotRepository,
    StorageError, SubmissionDraftRepository, TaskQueryRepository,
};
use chrono::Utc;

use crate::{AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action};

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

#[derive(Clone, Debug)]
pub struct SubmissionDraftBuildService<Q, A, S> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
    submissions: S,
}

impl<Q, A, S> SubmissionDraftBuildService<Q, A, S> {
    pub const fn new(
        registry: Arc<ProviderRegistry>,
        tasks: Q,
        accounts: A,
        submissions: S,
    ) -> Self {
        Self {
            registry,
            tasks,
            accounts,
            submissions,
        }
    }
}

impl<Q, A, S> SubmissionDraftBuildService<Q, A, S>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
    S: QuestionSnapshotRepository + AnswerCandidateRepository + SubmissionDraftRepository,
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
        if selected_by_question.len() != snapshot.questions.len() {
            return Err(SubmissionDraftBuildError::SelectionIncomplete);
        }

        let mut selected_answers = Vec::with_capacity(snapshot.questions.len());
        for question in &snapshot.questions {
            let record = selected_by_question
                .get(&question.id)
                .ok_or(SubmissionDraftBuildError::SelectionIncomplete)?;
            selected_answers.push(SelectedAnswer {
                candidate_id: record.id,
                question_id: question.id,
                answer: record.candidate.answer.clone(),
                source: record.candidate.source,
                confidence: record.candidate.confidence,
            });
        }

        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            SubmissionDraftBuildError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let builder = entry.submission_build.as_ref().ok_or_else(|| {
            SubmissionDraftBuildError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs,
            correlation_id: command.correlation_id,
        };
        let payload_preview = builder
            .build_submission_preview(
                &context,
                &task.remote_id,
                &snapshot.questions,
                &selected_answers,
            )
            .await?;
        let items = snapshot
            .questions
            .iter()
            .cloned()
            .zip(selected_answers)
            .map(|(question, selected)| asterism_domain::SubmissionDraftItem { question, selected })
            .collect();
        let draft = SubmissionDraft {
            id: SubmissionDraftId::new(),
            task_id: task.id,
            question_snapshot_id: snapshot.id,
            provider_id: account.provider_id,
            provider_version: entry.metadata.implementation_version.clone(),
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
    #[error("Provider returned an invalid or unsafe submission preview")]
    ProviderPreviewInvalid,
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
        OrchestrationState, ProviderAccount, ProviderAccountId, Question, QuestionId, QuestionKind,
        QuestionSnapshotId, RemoteState, SecretId, SourceType, SubmissionPayloadEncoding,
        SubmissionPayloadFieldPreview, SubmissionPayloadPreview, Task, Timestamp,
    };
    use asterism_provider_api::{
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        SubmissionBuildCapability, VerificationLevel,
    };
    use asterism_storage::{
        AnswerCandidateRecord, QuestionSnapshot, SubmissionDraftRepository, TaskPage,
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
        let fixture = fixture(true, false);
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
        let fixture = fixture(true, false);
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
        let fixture = fixture(true, true);
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
        let fixture = fixture(false, false);
        assert!(matches!(
            fixture
                .service
                .build(fixture.command(vec![fixture.repository.candidates[0].id]))
                .await,
            Err(SubmissionDraftBuildError::TaskCapabilityUnavailable)
        ));
        assert!(!fixture.builder.called.load(Ordering::Relaxed));
    }

    struct Fixture {
        service: SubmissionDraftBuildService<FakeRepository, FakeRepository, FakeRepository>,
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

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture keeps Task, account, snapshot, Candidate and Provider bindings visible together"
    )]
    fn fixture(advertises_build: bool, foreign_preview: bool) -> Fixture {
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
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id,
            provider_id: provider_id.clone(),
            provider_version: "0.1.0".to_owned(),
            captured_at: now,
            questions: vec![question],
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
            called: AtomicBool::new(false),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
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
        );
        Fixture {
            service,
            repository,
            builder,
        }
    }
}
