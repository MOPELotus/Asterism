use std::sync::Arc;

use asterism_domain::{AuthState, ProviderAccountId, ProviderId, TaskCapability, TaskId, UserId};
use asterism_provider_api::{BrowserSessionSpec, ProviderContext, ProviderError, ProviderRegistry};
use asterism_storage::{ProviderAccountRuntimeRepository, StorageError, TaskQueryRepository};

const MAX_CORRELATION_ID_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct ReadTaskBrowserSessionCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub correlation_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderTaskBrowserSessionResult {
    pub task_id: TaskId,
    pub provider_account_id: ProviderAccountId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub spec: BrowserSessionSpec,
}

#[derive(Clone, Debug)]
pub struct ProviderTaskBrowserSessionService<Q, A> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
}

impl<Q, A> ProviderTaskBrowserSessionService<Q, A> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A) -> Self {
        Self {
            registry,
            tasks,
            accounts,
        }
    }
}

impl<Q, A> ProviderTaskBrowserSessionService<Q, A>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
{
    /// Reads one fresh, credential-free `BrowserBridge` session policy through
    /// an exact owner/account/Task/Provider binding. This call does not create
    /// a helper session, inject credentials or perform remote interaction.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderTaskBrowserSessionError`] when the binding or
    /// capability is unavailable, the account is unauthenticated, or the
    /// Provider returns an unsafe browser policy.
    pub async fn read(
        &self,
        command: ReadTaskBrowserSessionCommand,
    ) -> Result<ProviderTaskBrowserSessionResult, ProviderTaskBrowserSessionError> {
        if !valid_correlation_id(&command.correlation_id) {
            return Err(ProviderTaskBrowserSessionError::InvalidCorrelationId);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ProviderTaskBrowserSessionError::TaskNotFound)?;
        if !task.capabilities.contains(&TaskCapability::BrowserBridge) {
            return Err(ProviderTaskBrowserSessionError::TaskCapabilityUnavailable);
        }
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(ProviderTaskBrowserSessionError::TaskNotFound)?;
        if account.auth_state != AuthState::Authenticated {
            return Err(ProviderTaskBrowserSessionError::AccountNotAuthenticated);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            ProviderTaskBrowserSessionError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let capability = entry.browser_bridge.as_ref().ok_or_else(|| {
            ProviderTaskBrowserSessionError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let spec = capability
            .browser_session_spec(
                &ProviderContext {
                    provider_id: account.provider_id.clone(),
                    account_id: account.id,
                    credential_refs: account.credential_refs,
                    correlation_id: command.correlation_id,
                },
                &task.remote_id,
            )
            .await?;
        spec.validate()
            .map_err(|_| ProviderTaskBrowserSessionError::ProviderResponseInvalid)?;
        Ok(ProviderTaskBrowserSessionResult {
            task_id: task.id,
            provider_account_id: account.id,
            provider_id: account.provider_id,
            provider_version: entry.metadata.implementation_version.clone(),
            spec,
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
pub enum ProviderTaskBrowserSessionError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("task does not advertise BrowserBridge")]
    TaskCapabilityUnavailable,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no BrowserBridge capability")]
    CapabilityUnavailable(ProviderId),
    #[error("browser session correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider returned an unsafe or inconsistent BrowserBridge session policy")]
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
        SourceType,
    };
    use asterism_provider_api::{
        BrowserBridgeCapability, ProviderCapability, ProviderEntry, ProviderIdentity,
        ProviderMetadata, ProviderResult, ProviderRuntimeSettingsSchema, VerificationLevel,
    };
    use asterism_storage::TaskPage;
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    #[derive(Clone, Debug)]
    struct FakeTasks {
        owner_id: UserId,
        task: asterism_domain::Task,
    }

    #[async_trait]
    impl TaskQueryRepository for FakeTasks {
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
        ) -> Result<Option<asterism_domain::Task>, StorageError> {
            Ok((owner_id == self.owner_id && task_id == self.task.id).then(|| self.task.clone()))
        }
    }

    #[derive(Clone, Debug)]
    struct FakeAccounts(ProviderAccount);

    #[async_trait]
    impl ProviderAccountRuntimeRepository for FakeAccounts {
        async fn find_runtime_provider_account(
            &self,
            account_id: ProviderAccountId,
        ) -> Result<Option<ProviderAccount>, StorageError> {
            Ok((account_id == self.0.id).then(|| self.0.clone()))
        }
    }

    #[derive(Debug)]
    struct FakeBrowser {
        metadata: ProviderMetadata,
        spec: Mutex<BrowserSessionSpec>,
        calls: Mutex<Vec<(ProviderContext, String)>>,
    }

    impl ProviderIdentity for FakeBrowser {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl BrowserBridgeCapability for FakeBrowser {
        async fn browser_session_spec(
            &self,
            context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<BrowserSessionSpec> {
            self.calls
                .lock()
                .unwrap()
                .push((context.clone(), remote_task_id.to_owned()));
            Ok(self.spec.lock().unwrap().clone())
        }
    }

    struct Fixture {
        service: ProviderTaskBrowserSessionService<FakeTasks, FakeAccounts>,
        owner_id: UserId,
        task_id: TaskId,
        capability: Arc<FakeBrowser>,
    }

    #[tokio::test]
    async fn owner_scoped_task_returns_only_a_validated_credential_free_policy() {
        let fixture = fixture();
        let result = fixture
            .service
            .read(ReadTaskBrowserSessionCommand {
                owner_id: fixture.owner_id,
                task_id: fixture.task_id,
                correlation_id: "browser-spec-1".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(result.task_id, fixture.task_id);
        assert_eq!(result.provider_account_id, calls_account_id(&fixture));
        assert_eq!(result.spec.version, 1);
        assert_eq!(result.spec.allowed_origins, ["https://provider.example"]);
        let calls = fixture.capability.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.correlation_id, "browser-spec-1");
        assert_eq!(calls[0].0.credential_refs.len(), 1);
        assert_eq!(calls[0].1, "work:remote");
    }

    fn calls_account_id(fixture: &Fixture) -> ProviderAccountId {
        fixture.capability.calls.lock().unwrap()[0].0.account_id
    }

    #[tokio::test]
    async fn foreign_owner_or_missing_task_capability_never_calls_provider() {
        let foreign_fixture = fixture();
        assert!(matches!(
            foreign_fixture
                .service
                .read(ReadTaskBrowserSessionCommand {
                    owner_id: UserId::new(),
                    task_id: foreign_fixture.task_id,
                    correlation_id: "browser-spec-2".to_owned(),
                })
                .await,
            Err(ProviderTaskBrowserSessionError::TaskNotFound)
        ));
        assert!(foreign_fixture.capability.calls.lock().unwrap().is_empty());

        let mut fixture = fixture();
        fixture.service.tasks.task.capabilities.clear();
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskBrowserSessionCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "browser-spec-3".to_owned(),
                })
                .await,
            Err(ProviderTaskBrowserSessionError::TaskCapabilityUnavailable)
        ));
        assert!(fixture.capability.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unsafe_provider_policy_fails_closed() {
        let fixture = fixture();
        fixture.capability.spec.lock().unwrap().allowed_origins =
            vec!["http://provider.example".to_owned()];
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskBrowserSessionCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "browser-spec-4".to_owned(),
                })
                .await,
            Err(ProviderTaskBrowserSessionError::ProviderResponseInvalid)
        ));
    }

    fn fixture() -> Fixture {
        let owner_id = UserId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account_id = ProviderAccountId::new();
        let now = Utc::now();
        let task = asterism_domain::Task {
            id: TaskId::new(),
            provider_account_id: account_id,
            course_id: None,
            remote_id: "work:remote".to_owned(),
            source_type: SourceType::Work,
            assessment_class: AssessmentClass::Unknown,
            title: "remote work".to_owned(),
            remote_state: RemoteState::Pending,
            orchestration_state: OrchestrationState::Ready,
            opens_at: None,
            due_at: None,
            closes_at: None,
            discovered_at: now,
            updated_at: now,
            latest_snapshot_id: None,
            capabilities: vec![TaskCapability::BrowserBridge],
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
            capabilities: BTreeSet::from([ProviderCapability::BrowserBridge]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(FakeBrowser {
            metadata: metadata.clone(),
            spec: Mutex::new(BrowserSessionSpec {
                version: 1,
                start_url: "https://provider.example/task/a1".to_owned(),
                isolation_key: "provider-task-a1".to_owned(),
                allowed_origins: vec!["https://provider.example".to_owned()],
                read_sources: Vec::new(),
                headless: false,
            }),
            calls: Mutex::new(Vec::new()),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.runtime_settings = ProviderRuntimeSettingsSchema::default();
        entry.browser_bridge = Some(capability.clone());
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        Fixture {
            service: ProviderTaskBrowserSessionService::new(
                Arc::new(registry),
                FakeTasks {
                    owner_id,
                    task: task.clone(),
                },
                FakeAccounts(account),
            ),
            owner_id,
            task_id: task.id,
            capability,
        }
    }
}
