use std::sync::Arc;

use asterism_domain::{AuthState, ProviderId, TaskCapability, TaskId, UserId};
use asterism_provider_api::{ProviderContext, ProviderError, ProviderRegistry, RemoteProgress};
use asterism_storage::{ProviderAccountRuntimeRepository, StorageError, TaskQueryRepository};

const MAX_CORRELATION_ID_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct ReadTaskProgressCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub correlation_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderTaskProgressResult {
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub progress: RemoteProgress,
}

#[derive(Clone, Debug)]
pub struct ProviderTaskProgressService<Q, A> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
}

impl<Q, A> ProviderTaskProgressService<Q, A> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A) -> Self {
        Self {
            registry,
            tasks,
            accounts,
        }
    }
}

impl<Q, A> ProviderTaskProgressService<Q, A>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
{
    /// Reads fresh remote progress for one owner-scoped Task. The persisted
    /// Task must explicitly advertise `ProgressRead`; Core never probes an
    /// undeclared task capability.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderTaskProgressError`] when ownership, account state,
    /// task or Provider capability, Provider I/O, or response validation fails.
    pub async fn read(
        &self,
        command: ReadTaskProgressCommand,
    ) -> Result<ProviderTaskProgressResult, ProviderTaskProgressError> {
        if !valid_correlation_id(&command.correlation_id) {
            return Err(ProviderTaskProgressError::InvalidCorrelationId);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ProviderTaskProgressError::TaskNotFound)?;
        if !task.capabilities.contains(&TaskCapability::ProgressRead) {
            return Err(ProviderTaskProgressError::TaskCapabilityUnavailable);
        }
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(ProviderTaskProgressError::TaskNotFound)?;
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(ProviderTaskProgressError::AccountNotAuthenticated);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            ProviderTaskProgressError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let capability = entry.task_progress.as_ref().ok_or_else(|| {
            ProviderTaskProgressError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let progress = capability
            .read_progress(
                &ProviderContext {
                    provider_id: account.provider_id.clone(),
                    account_id: account.id,
                    credential_refs: account.credential_refs,
                    correlation_id: command.correlation_id,
                },
                &task.remote_id,
            )
            .await?;
        if progress.percent.is_some_and(|percent| percent > 100) {
            return Err(ProviderTaskProgressError::ProviderResponseInvalid);
        }
        Ok(ProviderTaskProgressResult {
            task_id: task.id,
            provider_id: account.provider_id,
            provider_version: entry.metadata.implementation_version.clone(),
            progress,
        })
    }
}

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CORRELATION_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderTaskProgressError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task does not advertise ProgressRead")]
    TaskCapabilityUnavailable,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no Task Progress capability")]
    CapabilityUnavailable(ProviderId),
    #[error("task progress correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider returned invalid Task progress")]
    ProviderResponseInvalid,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Mutex};

    use asterism_domain::{
        AssessmentClass, OrchestrationState, ProviderAccount, ProviderAccountId, RemoteState,
        SourceType, Task, UserId,
    };
    use asterism_provider_api::{
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderMetadata, ProviderResult,
        ProviderRuntimeSettingsSchema, TaskProgressCapability, VerificationLevel,
    };
    use asterism_storage::TaskPage;
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
    struct FakeTaskProgress {
        metadata: ProviderMetadata,
        progress: Mutex<RemoteProgress>,
        calls: Mutex<Vec<(ProviderContext, String)>>,
    }

    impl ProviderIdentity for FakeTaskProgress {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskProgressCapability for FakeTaskProgress {
        async fn read_progress(
            &self,
            context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteProgress> {
            self.calls
                .lock()
                .unwrap()
                .push((context.clone(), remote_task_id.to_owned()));
            Ok(self.progress.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn progress_is_owner_scoped_and_passes_only_opaque_credentials() {
        let fixture = fixture(true, 75);
        let result = fixture
            .service
            .read(ReadTaskProgressCommand {
                owner_id: fixture.owner_id,
                task_id: fixture.task_id,
                correlation_id: "progress-request-1".to_owned(),
            })
            .await
            .unwrap();

        assert_eq!(result.progress.percent, Some(75));
        let calls = fixture.capability.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.credential_refs.len(), 1);
        assert_eq!(calls[0].0.correlation_id, "progress-request-1");
        assert_eq!(calls[0].1, "work:100:200:work-1");
    }

    #[tokio::test]
    async fn undeclared_or_foreign_task_never_reaches_provider() {
        let fixture = fixture(false, 50);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskProgressCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "progress-request-2".to_owned(),
                })
                .await,
            Err(ProviderTaskProgressError::TaskCapabilityUnavailable)
        ));
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskProgressCommand {
                    owner_id: UserId::new(),
                    task_id: fixture.task_id,
                    correlation_id: "progress-request-3".to_owned(),
                })
                .await,
            Err(ProviderTaskProgressError::TaskNotFound)
        ));
        assert!(fixture.capability.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_provider_percentage_fails_at_core_boundary() {
        let fixture = fixture(true, 101);
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskProgressCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "progress-request-4".to_owned(),
                })
                .await,
            Err(ProviderTaskProgressError::ProviderResponseInvalid)
        ));
    }

    struct Fixture {
        service: ProviderTaskProgressService<FakeTaskRepository, FakeAccountRepository>,
        owner_id: UserId,
        task_id: TaskId,
        capability: Arc<FakeTaskProgress>,
    }

    fn fixture(advertises_progress: bool, percent: u8) -> Fixture {
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
            assessment_class: AssessmentClass::Unknown,
            title: "work".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: if advertises_progress {
                vec![TaskCapability::ProgressRead]
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
            capabilities: BTreeSet::from([ProviderCapability::TaskProgressRead]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(FakeTaskProgress {
            metadata,
            progress: Mutex::new(RemoteProgress {
                remote_state: RemoteState::InProgress,
                percent: Some(percent),
                duration_seconds: None,
                updated_at: now,
            }),
            calls: Mutex::new(Vec::new()),
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
                task_progress: Some(capability.clone()),
                question_inventory: None,
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let service = ProviderTaskProgressService::new(
            Arc::new(registry),
            FakeTaskRepository {
                owner_id,
                task: task.clone(),
            },
            FakeAccountRepository(account),
        );
        Fixture {
            service,
            owner_id,
            task_id: task.id,
            capability,
        }
    }
}
