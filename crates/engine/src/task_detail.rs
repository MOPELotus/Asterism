use std::sync::Arc;

use asterism_domain::{AuthState, ProviderId, Task, TaskId, UserId};
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderMetadata, ProviderRegistry, RemoteTask,
    RemoteTaskDetail,
};
use asterism_storage::{ProviderAccountRuntimeRepository, StorageError, TaskQueryRepository};

use crate::scan::task_provider_capability;

const MAX_CORRELATION_ID_BYTES: usize = 128;
const MAX_REMOTE_ID_BYTES: usize = 512;
const MAX_TITLE_BYTES: usize = 512;
const MAX_DETAIL_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ReadTaskDetailCommand {
    pub owner_id: UserId,
    pub task_id: TaskId,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderTaskDetailResult {
    pub task_id: TaskId,
    pub provider_id: ProviderId,
    pub provider_version: String,
    pub detail: RemoteTaskDetail,
}

#[derive(Clone, Debug)]
pub struct ProviderTaskDetailService<Q, A> {
    registry: Arc<ProviderRegistry>,
    tasks: Q,
    accounts: A,
}

impl<Q, A> ProviderTaskDetailService<Q, A> {
    pub const fn new(registry: Arc<ProviderRegistry>, tasks: Q, accounts: A) -> Self {
        Self {
            registry,
            tasks,
            accounts,
        }
    }
}

impl<Q, A> ProviderTaskDetailService<Q, A>
where
    Q: TaskQueryRepository,
    A: ProviderAccountRuntimeRepository,
{
    /// Reads one freshly rediscovered Provider detail behind the owner-scoped
    /// local Task binding. The Provider payload is bounded and checked again
    /// before it can cross the Core API boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderTaskDetailError`] when the Task/account binding is
    /// unavailable, the account cannot access its Provider, the capability is
    /// absent, or the Provider returns an inconsistent or unsanitized detail.
    pub async fn read(
        &self,
        command: ReadTaskDetailCommand,
    ) -> Result<ProviderTaskDetailResult, ProviderTaskDetailError> {
        if !valid_text(&command.correlation_id, MAX_CORRELATION_ID_BYTES) {
            return Err(ProviderTaskDetailError::InvalidCorrelationId);
        }
        let task = self
            .tasks
            .find_owned_task(command.owner_id, command.task_id)
            .await?
            .ok_or(ProviderTaskDetailError::TaskNotFound)?;
        let account = self
            .accounts
            .find_runtime_provider_account(task.provider_account_id)
            .await?
            .filter(|account| account.owner_id == command.owner_id)
            .ok_or(ProviderTaskDetailError::TaskNotFound)?;
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(ProviderTaskDetailError::AccountNotAuthenticated);
        }
        let entry = self.registry.get(&account.provider_id).ok_or_else(|| {
            ProviderTaskDetailError::ProviderNotRegistered(account.provider_id.clone())
        })?;
        let capability = entry.task_detail.as_ref().ok_or_else(|| {
            ProviderTaskDetailError::CapabilityUnavailable(account.provider_id.clone())
        })?;
        let detail = capability
            .task_detail(
                &ProviderContext {
                    provider_id: account.provider_id.clone(),
                    account_id: account.id,
                    credential_refs: account.credential_refs,
                    correlation_id: command.correlation_id,
                },
                &task.remote_id,
            )
            .await?;
        validate_detail(&entry.metadata, &task, &detail)?;
        Ok(ProviderTaskDetailResult {
            task_id: task.id,
            provider_id: account.provider_id,
            provider_version: entry.metadata.implementation_version.clone(),
            detail,
        })
    }
}

fn validate_detail(
    metadata: &ProviderMetadata,
    persisted: &Task,
    detail: &RemoteTaskDetail,
) -> Result<(), ProviderTaskDetailError> {
    let task = &detail.task;
    if task.remote_id != persisted.remote_id
        || task.source_type != persisted.source_type
        || !valid_remote_task(task)
        || task
            .capabilities
            .iter()
            .copied()
            .any(|capability| !metadata.advertises(task_provider_capability(capability)))
        || serde_json::to_vec(detail).map_or(true, |encoded| encoded.len() > MAX_DETAIL_BYTES)
        || contains_sensitive_key(&detail.normalized_detail)
        || contains_sensitive_key(&task.normalized)
        || contains_sensitive_key(&task.raw_sanitized)
    {
        return Err(ProviderTaskDetailError::ProviderResponseInvalid);
    }
    Ok(())
}

fn valid_remote_task(task: &RemoteTask) -> bool {
    valid_text(&task.remote_id, MAX_REMOTE_ID_BYTES)
        && task
            .course_remote_id
            .as_deref()
            .is_none_or(|value| valid_text(value, MAX_REMOTE_ID_BYTES))
        && valid_text(&task.title, MAX_TITLE_BYTES)
        && valid_fingerprint(&task.fingerprint)
}

fn valid_fingerprint(value: &str) -> bool {
    let Some((version, fingerprint)) = value.split_once(':') else {
        return false;
    };
    version.strip_prefix('v').is_some_and(|version| {
        !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
    }) && valid_text(fingerprint, 256)
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn contains_sensitive_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized: String = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                normalized.as_str(),
                "cookie"
                    | "authorization"
                    | "password"
                    | "accesstoken"
                    | "refreshtoken"
                    | "sessionsecret"
                    | "clientsecret"
            ) || contains_sensitive_key(value)
        }),
        serde_json::Value::Array(items) => items.iter().any(contains_sensitive_key),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderTaskDetailError {
    #[error("task was not found")]
    TaskNotFound,
    #[error("Provider account is not authenticated")]
    AccountNotAuthenticated,
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no Task Detail capability")]
    CapabilityUnavailable(ProviderId),
    #[error("task detail correlation id is invalid")]
    InvalidCorrelationId,
    #[error("Provider returned an inconsistent, oversized, or unsanitized Task detail")]
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
        SourceType, UserId,
    };
    use asterism_provider_api::{
        ProviderCapability, ProviderEntry, ProviderIdentity, ProviderResult,
        ProviderRuntimeSettingsSchema, TaskDetailCapability, VerificationLevel,
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
    struct FakeTaskDetail {
        metadata: ProviderMetadata,
        detail: Mutex<RemoteTaskDetail>,
        contexts: Mutex<Vec<(ProviderContext, String)>>,
    }

    impl ProviderIdentity for FakeTaskDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FakeTaskDetail {
        async fn task_detail(
            &self,
            context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            self.contexts
                .lock()
                .unwrap()
                .push((context.clone(), remote_task_id.to_owned()));
            Ok(self.detail.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn owner_scoped_detail_passes_only_opaque_credential_references() {
        let fixture = make_fixture();
        let result = fixture
            .service
            .read(ReadTaskDetailCommand {
                owner_id: fixture.owner_id,
                task_id: fixture.task_id,
                correlation_id: "detail-request-1".to_owned(),
            })
            .await
            .unwrap();

        assert_eq!(result.task_id, fixture.task_id);
        assert_eq!(result.provider_id.as_str(), "provider-alpha");
        assert_eq!(result.detail.task.remote_id, "work:course:resource:task");
        let contexts = fixture.capability.contexts.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].0.correlation_id, "detail-request-1");
        assert_eq!(contexts[0].0.credential_refs.len(), 1);
        assert_eq!(contexts[0].1, "work:course:resource:task");
    }

    #[tokio::test]
    async fn another_owner_cannot_trigger_a_provider_detail_call() {
        let fixture = make_fixture();
        let result = fixture
            .service
            .read(ReadTaskDetailCommand {
                owner_id: UserId::new(),
                task_id: fixture.task_id,
                correlation_id: "detail-request-2".to_owned(),
            })
            .await;

        assert!(matches!(result, Err(ProviderTaskDetailError::TaskNotFound)));
        assert!(fixture.capability.contexts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn provider_detail_rejects_secret_shaped_fields_and_binding_drift() {
        let fixture = make_fixture();
        fixture.capability.detail.lock().unwrap().normalized_detail =
            serde_json::json!({"access_token": "not-allowed"});
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskDetailCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "detail-request-3".to_owned(),
                })
                .await,
            Err(ProviderTaskDetailError::ProviderResponseInvalid)
        ));

        let fixture = make_fixture();
        fixture.capability.detail.lock().unwrap().task.remote_id =
            "work:other:resource:task".to_owned();
        assert!(matches!(
            fixture
                .service
                .read(ReadTaskDetailCommand {
                    owner_id: fixture.owner_id,
                    task_id: fixture.task_id,
                    correlation_id: "detail-request-4".to_owned(),
                })
                .await,
            Err(ProviderTaskDetailError::ProviderResponseInvalid)
        ));
    }

    struct Fixture {
        service: ProviderTaskDetailService<FakeTaskRepository, FakeAccountRepository>,
        owner_id: UserId,
        task_id: TaskId,
        capability: Arc<FakeTaskDetail>,
    }

    fn make_fixture() -> Fixture {
        let owner_id = UserId::new();
        let provider_id = ProviderId::new("provider-alpha").unwrap();
        let account_id = ProviderAccountId::new();
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            provider_account_id: account_id,
            course_id: None,
            remote_id: "work:course:resource:task".to_owned(),
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
            capabilities: Vec::new(),
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
            capabilities: BTreeSet::from([ProviderCapability::TaskDetail]),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        };
        let capability = Arc::new(FakeTaskDetail {
            metadata,
            detail: Mutex::new(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: task.remote_id.clone(),
                    course_remote_id: Some("course".to_owned()),
                    title: task.title.clone(),
                    source_type: task.source_type,
                    assessment_class: task.assessment_class,
                    remote_state: task.remote_state,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: Vec::new(),
                    fingerprint: "v1:fingerprint".to_owned(),
                    normalized: serde_json::json!({"kind": "work"}),
                    raw_sanitized: serde_json::json!({"state": "pending"}),
                },
                normalized_detail: serde_json::json!({"question_count": 3}),
            }),
            contexts: Mutex::new(Vec::new()),
        });
        let service = service(owner_id, task.clone(), account.clone(), capability.clone());
        Fixture {
            service,
            owner_id,
            task_id: task.id,
            capability,
        }
    }

    fn service(
        owner_id: UserId,
        task: Task,
        account: ProviderAccount,
        capability: Arc<FakeTaskDetail>,
    ) -> ProviderTaskDetailService<FakeTaskRepository, FakeAccountRepository> {
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata: capability.metadata.clone(),
                runtime_settings: ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: None,
                task_inventory: None,
                task_detail: Some(capability),
                task_progress: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        ProviderTaskDetailService::new(
            Arc::new(registry),
            FakeTaskRepository { owner_id, task },
            FakeAccountRepository(account),
        )
    }
}
