use std::{collections::BTreeSet, sync::Arc};

use asterism_domain::{
    AuthState, ProviderId, Question, QuestionKind, QuestionSnapshotId, TaskCapability, TaskId,
    Timestamp, UserId,
};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderRegistry, RemoteQuestionRef};
use asterism_storage::{
    ProviderAccountRuntimeRepository, QuestionSnapshot, QuestionSnapshotRepository, StorageError,
    TaskQueryRepository,
};
use chrono::Utc;

use crate::{AssessmentGuardError, FormalAssessmentPolicy, TaskAction, authorize_task_action};

const MAX_CORRELATION_ID_BYTES: usize = 128;
const MAX_QUESTIONS_PER_READ: usize = 5_000;

#[derive(Clone, Debug)]
pub struct ReadTaskQuestionsCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderQuestionReadResult {
    pub snapshot_id: QuestionSnapshotId,
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub captured_at: Timestamp,
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug)]
pub struct ProviderQuestionReadService<Q, A, S> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
    snapshots: S,
}

impl<Q, A, S> ProviderQuestionReadService<Q, A, S> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A, snapshots: S) -> Self {
        Self {
            registry,
            tasks,
            accounts,
            snapshots,
        }
    }
}

impl<Q, A, S> ProviderQuestionReadService<Q, A, S>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
    S: QuestionSnapshotRepository,
{
    /// Discovers and parses one complete, fresh Question set. Provider output
    /// is returned only after every reference and normalized Question passes
    /// identity, ordering, size and sanitization checks.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderQuestionReadError`] for ownership, task/provider
    /// capability, account state, Provider I/O, or all-or-nothing validation
    /// failures.
    pub async fn read(
        &self,
        command: ReadTaskQuestionsCommand,
    ) -> Result<ProviderQuestionReadResult, ProviderQuestionReadError> {
        if !valid_correlation_id(&command.correlation_id) {
            return Err(ProviderQuestionReadError::InvalidCorrelationId);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ProviderQuestionReadError::TaskNotFound)?;
        if !task
            .capabilities
            .contains(&TaskCapability::QuestionInventory)
            || !task.capabilities.contains(&TaskCapability::QuestionParse)
        {
            return Err(ProviderQuestionReadError::TaskCapabilityUnavailable);
        }
        authorize_task_action(&task, TaskAction::Parse, FormalAssessmentPolicy::default())?;
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(ProviderQuestionReadError::TaskNotFound)?;
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(ProviderQuestionReadError::AccountNotAuthenticated);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            ProviderQuestionReadError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let inventory = entry.question_inventory.as_ref().ok_or_else(|| {
            ProviderQuestionReadError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let parser = entry.question_parse.as_ref().ok_or_else(|| {
            ProviderQuestionReadError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs,
            correlation_id: command.correlation_id,
        };
        let mut references = inventory
            .list_question_refs(&context, &task.remote_id)
            .await?;
        validate_references(&references)?;
        references.sort_by_key(|reference| reference.position);
        let mut questions = Vec::with_capacity(references.len());
        for reference in &references {
            let question = parser
                .parse_question(&context, task.id, &task.remote_id, reference)
                .await?;
            validate_question_binding(&task, reference, &question)?;
            questions.push(question);
        }
        let snapshot = QuestionSnapshot {
            id: QuestionSnapshotId::new(),
            task_id: task.id,
            provider_id: account.provider_id.clone(),
            provider_version: entry.metadata.implementation_version.clone(),
            captured_at: Utc::now(),
            questions: questions.clone(),
        };
        self.snapshots.save_question_snapshot(&snapshot).await?;
        Ok(ProviderQuestionReadResult {
            snapshot_id: snapshot.id,
            task_id: snapshot.task_id,
            provider_id: snapshot.provider_id,
            provider_version: snapshot.provider_version,
            captured_at: snapshot.captured_at,
            questions,
        })
    }
}

fn validate_references(references: &[RemoteQuestionRef]) -> Result<(), ProviderQuestionReadError> {
    if references.len() > MAX_QUESTIONS_PER_READ {
        return Err(ProviderQuestionReadError::ProviderResponseInvalid);
    }
    let mut remote_ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    for reference in references {
        if reference.validate().is_err()
            || !remote_ids.insert(reference.remote_id.as_str())
            || !positions.insert(reference.position)
        {
            return Err(ProviderQuestionReadError::ProviderResponseInvalid);
        }
    }
    Ok(())
}

fn validate_question_binding(
    task: &asterism_domain::Task,
    reference: &RemoteQuestionRef,
    question: &Question,
) -> Result<(), ProviderQuestionReadError> {
    if question.task_id != task.id
        || question.remote_question_id.as_deref() != Some(reference.remote_id.as_str())
        || question.position != reference.position
        || (reference.kind_hint != QuestionKind::Unknown && question.kind != reference.kind_hint)
        || question.validate().is_err()
    {
        Err(ProviderQuestionReadError::ProviderResponseInvalid)
    } else {
        Ok(())
    }
}

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CORRELATION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderQuestionReadError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task does not advertise QuestionInventory and QuestionParse")]
    TaskCapabilityUnavailable,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no complete Question read pipeline")]
    CapabilityUnavailable(ProviderId),
    #[error("question read correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider returned invalid, duplicate, or unsanitized Questions")]
    ProviderResponseInvalid,
    #[error(transparent)]
    Assessment(#[from] AssessmentGuardError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use asterism_domain::{
        AssessmentClass, OrchestrationState, ProviderAccount, ProviderAccountId, RemoteState,
        SourceType, Task,
    };
    use asterism_provider_api::{
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        ProviderRouteContext, ProviderRuntimeSettingsSchema, QuestionInventoryCapability,
        QuestionParseCapability, VerificationLevel,
    };
    use asterism_storage::{QuestionSnapshot, QuestionSnapshotRepository, TaskPage};
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    #[derive(Clone, Debug)]
    struct FakeTaskRepository {
        owner_id: UserId,
        task: Task,
    }

    #[async_trait]
    impl TaskQueryRepository for FakeTaskRepository {
        async fn list_owned_tasks(
            &self,
            owner_id: UserId,
            _provider_account_id: Option<ProviderAccountId>,
            _limit: u32,
            _offset: u64,
        ) -> Result<TaskPage, StorageError> {
            let items = if owner_id == self.owner_id {
                vec![self.task.clone()]
            } else {
                Vec::new()
            };
            Ok(TaskPage {
                total: items.len() as u64,
                items,
            })
        }

        async fn find_owned_task(
            &self,
            owner_id: UserId,
            task_id: TaskId,
        ) -> Result<Option<Task>, StorageError> {
            Ok((owner_id == self.owner_id && task_id == self.task.id).then(|| self.task.clone()))
        }
    }

    #[derive(Clone, Debug)]
    struct FakeAccountRepository(ProviderAccount);

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FakeAccountRepository {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((account_id == self.0.id).then(|| self.0.clone()))
        }
    }

    #[derive(Debug)]
    struct FakeQuestions {
        metadata: ProviderMetadata,
        references: Mutex<Vec<RemoteQuestionRef>>,
        parsed_task_id: Mutex<Option<TaskId>>,
    }

    #[derive(Clone, Debug, Default)]
    struct RecordingSnapshots {
        snapshots: Arc<Mutex<Vec<QuestionSnapshot>>>,
        reject: Arc<AtomicBool>,
    }

    #[async_trait]
    impl QuestionSnapshotRepository for RecordingSnapshots {
        async fn save_question_snapshot(
            &self,
            snapshot: &QuestionSnapshot,
        ) -> Result<(), StorageError> {
            if self.reject.load(Ordering::Relaxed) {
                return Err(StorageError::InvalidData(
                    "fixture rejected Question snapshot".to_owned(),
                ));
            }
            self.snapshots.lock().unwrap().push(snapshot.clone());
            Ok(())
        }

        async fn find_owned_question_snapshot(
            &self,
            _owner_id: UserId,
            question_snapshot_id: QuestionSnapshotId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok(self
                .snapshots
                .lock()
                .unwrap()
                .iter()
                .find(|snapshot| snapshot.id == question_snapshot_id)
                .cloned())
        }

        async fn find_latest_owned_question_snapshot(
            &self,
            _owner_id: UserId,
            _task_id: TaskId,
        ) -> Result<Option<QuestionSnapshot>, StorageError> {
            Ok(self.snapshots.lock().unwrap().last().cloned())
        }
    }

    impl ProviderIdentity for FakeQuestions {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl QuestionInventoryCapability for FakeQuestions {
        async fn list_question_refs(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<Vec<RemoteQuestionRef>> {
            Ok(self.references.lock().unwrap().clone())
        }
    }

    #[async_trait]
    impl QuestionParseCapability for FakeQuestions {
        async fn parse_question(
            &self,
            _context: &ProviderContext,
            task_id: TaskId,
            _remote_task_id: &str,
            reference: &RemoteQuestionRef,
        ) -> ProviderResult<Question> {
            *self.parsed_task_id.lock().unwrap() = Some(task_id);
            Ok(Question {
                id: asterism_domain::QuestionId::new(),
                task_id,
                remote_question_id: Some(reference.remote_id.clone()),
                kind: reference.kind_hint,
                stem: format!("Question {}", reference.position),
                options: Vec::new(),
                attachments: Vec::new(),
                metadata_sanitized: serde_json::json!({"safe": true}),
                position: reference.position,
            })
        }
    }

    #[tokio::test]
    async fn complete_question_set_is_owner_scoped_sorted_and_validated() {
        let fixture = fixture(true);
        let result = fixture
            .service
            .read(ReadTaskQuestionsCommand {
                owner_id: fixture.owner_id,
                task_id: fixture.task_id,
                correlation_id: "question-read-1".to_owned(),
            })
            .await
            .unwrap();

        assert_eq!(result.questions.len(), 2);
        assert_eq!(result.questions[0].position, 1);
        assert_eq!(result.questions[1].position, 2);
        let snapshots = fixture.snapshots.snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, result.snapshot_id);
        assert_eq!(snapshots[0].questions, result.questions);
        assert_eq!(
            *fixture.capability.parsed_task_id.lock().unwrap(),
            Some(fixture.task_id)
        );
    }

    #[tokio::test]
    async fn duplicate_references_fail_before_any_question_is_parsed() {
        let fixture = fixture(true);
        let duplicate = reference("question-1", 2);
        fixture
            .capability
            .references
            .lock()
            .unwrap()
            .push(duplicate);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-2".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::ProviderResponseInvalid)
        ));
        assert!(fixture.capability.parsed_task_id.lock().unwrap().is_none());
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn undeclared_question_capability_never_calls_provider() {
        let fixture = fixture(false);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-3".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::TaskCapabilityUnavailable)
        ));
        assert!(fixture.capability.parsed_task_id.lock().unwrap().is_none());
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn storage_failure_returns_no_question_result() {
        let fixture = fixture(true);
        fixture.snapshots.reject.store(true, Ordering::Relaxed);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskQuestionsCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "question-read-storage-failure".to_owned(),
                })
                .await,
            Err(ProviderQuestionReadError::Storage(_))
        ));
        assert!(fixture.snapshots.snapshots.lock().unwrap().is_empty());
    }

    struct Fixture {
        service: ProviderQuestionReadService<
            FakeTaskRepository,
            FakeAccountRepository,
            RecordingSnapshots,
        >,
        owner_id: UserId,
        task_id: TaskId,
        capability: Arc<FakeQuestions>,
        snapshots: RecordingSnapshots,
    }

    fn fixture(advertises_questions: bool) -> Fixture {
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            provider_account_id: account_id,
            course_id: None,
            remote_id: "work:100:200:work-1".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Formal,
            title: "work".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: if advertises_questions {
                vec![
                    TaskCapability::QuestionInventory,
                    TaskCapability::QuestionParse,
                ]
            } else {
                Vec::new()
            },
        };
        let account = ProviderAccount {
            id: account_id,
            owner_id,
            provider_id: provider_id.clone(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: vec![asterism_domain::SecretId::new()],
            created_at: now,
            updated_at: now,
        };
        let metadata = ProviderMetadata {
            id: provider_id,
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([
                ProviderCapability::QuestionInventory,
                ProviderCapability::QuestionParse,
            ]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(FakeQuestions {
            metadata,
            references: Mutex::new(vec![reference("question-2", 2), reference("question-1", 1)]),
            parsed_task_id: Mutex::new(None),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata: capability.metadata.clone(),
                runtime_settings: ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: None,
                task_inventory: None,
                task_detail: None,
                task_progress: None,
                duration_read: None,
                question_inventory: Some(capability.clone()),
                question_parse: Some(capability.clone()),
                answer_resolve: None,
                submission_build: None,
                submission_execute: None,
                submission_verify: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let snapshots = RecordingSnapshots::default();
        let service = ProviderQuestionReadService::new(
            Arc::new(registry),
            FakeTaskRepository {
                owner_id,
                task: task.clone(),
            },
            FakeAccountRepository(account),
            snapshots.clone(),
        );
        Fixture {
            service,
            owner_id,
            task_id: task.id,
            capability,
            snapshots,
        }
    }

    fn reference(remote_id: &str, position: u32) -> RemoteQuestionRef {
        RemoteQuestionRef {
            remote_id: remote_id.to_owned(),
            position,
            kind_hint: QuestionKind::Unknown,
            metadata_sanitized: serde_json::json!({"safe": true}),
            route_context: ProviderRouteContext::default(),
        }
    }
}
