use std::sync::Arc;

use asterism_domain::{
    AuditActor, AuthState, ProviderAccount, ProviderId, TaskCapability, Timestamp,
};
use asterism_provider_api::{
    ProviderCapability, ProviderContext, ProviderError, ProviderMetadata, ProviderRegistry,
    RemoteCourse, RemoteTask,
};
use asterism_storage::{
    ProviderScanBatch, ProviderScanReport, ProviderScanRepository, ScannedCourse, ScannedTask,
    StorageError,
};
use async_trait::async_trait;

#[async_trait]
pub trait ProviderAccountScanner: Send + Sync {
    async fn scan_account(
        &self,
        account: &ProviderAccount,
        correlation_id: String,
        initiated_by: Option<AuditActor>,
        observed_at: Timestamp,
    ) -> Result<ProviderScanReport, ProviderScanError>;
}

#[derive(Clone, Debug)]
pub struct ProviderScanService<R> {
    registry: Arc<ProviderRegistry>,
    repository: R,
}

impl<R> ProviderScanService<R> {
    pub const fn new(registry: Arc<ProviderRegistry>, repository: R) -> Self {
        Self {
            registry,
            repository,
        }
    }
}

impl<R> ProviderScanService<R>
where
    R: ProviderScanRepository,
{
    /// Reads every available inventory capability before committing one scan
    /// batch. Provider failures therefore cannot leave a partial observation.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderScanError`] when the Provider is absent, exposes no
    /// inventory capability, returns an inconsistent course scope, fails a
    /// capability call, or the repository rejects the completed batch.
    pub async fn scan_account(
        &self,
        account: &ProviderAccount,
        correlation_id: impl Into<String>,
        initiated_by: Option<AuditActor>,
        observed_at: Timestamp,
    ) -> Result<ProviderScanReport, ProviderScanError> {
        let correlation_id = correlation_id.into();
        if !valid_correlation_id(&correlation_id) {
            return Err(ProviderScanError::InvalidCorrelationId);
        }
        if !matches!(account.auth_state, AuthState::Authenticated) {
            return Err(ProviderScanError::AccountNotAuthenticated(account.id));
        }
        let entry = self
            .registry
            .get(&account.provider_id)
            .ok_or_else(|| ProviderScanError::ProviderNotRegistered(account.provider_id.clone()))?;
        if entry.course_inventory.is_none() && entry.task_inventory.is_none() {
            return Err(ProviderScanError::NoInventoryCapabilities(
                account.provider_id.clone(),
            ));
        }
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs.clone(),
            correlation_id: correlation_id.clone(),
        };
        let remote_courses = match &entry.course_inventory {
            Some(inventory) => inventory.list_courses(&context).await?,
            None => Vec::new(),
        };
        let remote_tasks = match &entry.task_inventory {
            Some(inventory) if entry.course_inventory.is_some() => {
                collect_course_tasks(inventory.as_ref(), &context, &remote_courses).await?
            }
            Some(inventory) => inventory.list_tasks(&context, None).await?,
            None => Vec::new(),
        };
        validate_task_capabilities(&entry.metadata, &remote_tasks)?;
        let batch = ProviderScanBatch {
            provider_account_id: account.id,
            provider_id: account.provider_id.clone(),
            provider_version: entry.metadata.implementation_version.clone(),
            observed_at,
            correlation_id,
            initiated_by,
            courses: remote_courses.into_iter().map(scanned_course).collect(),
            tasks: remote_tasks.into_iter().map(scanned_task).collect(),
        };
        Ok(self.repository.ingest_scan(&batch).await?)
    }
}

#[async_trait]
impl<R> ProviderAccountScanner for ProviderScanService<R>
where
    R: ProviderScanRepository,
{
    async fn scan_account(
        &self,
        account: &ProviderAccount,
        correlation_id: String,
        initiated_by: Option<AuditActor>,
        observed_at: Timestamp,
    ) -> Result<ProviderScanReport, ProviderScanError> {
        ProviderScanService::scan_account(self, account, correlation_id, initiated_by, observed_at)
            .await
    }
}

fn validate_task_capabilities(
    metadata: &ProviderMetadata,
    tasks: &[RemoteTask],
) -> Result<(), ProviderScanError> {
    for capability in tasks
        .iter()
        .flat_map(|task| task.capabilities.iter().copied())
    {
        let provider_capability = task_provider_capability(capability);
        if !metadata.advertises(provider_capability) {
            return Err(ProviderScanError::UnadvertisedTaskCapability {
                provider_id: metadata.id.clone(),
                capability,
            });
        }
    }
    Ok(())
}

pub(crate) const fn task_provider_capability(capability: TaskCapability) -> ProviderCapability {
    match capability {
        TaskCapability::ProgressRead => ProviderCapability::TaskProgressRead,
        TaskCapability::ResourceExecution => ProviderCapability::ResourceExecution,
        TaskCapability::QuestionInventory => ProviderCapability::QuestionInventory,
        TaskCapability::QuestionParse => ProviderCapability::QuestionParse,
        TaskCapability::AnswerResolve => ProviderCapability::AnswerResolve,
        TaskCapability::SubmissionBuild => ProviderCapability::SubmissionBuild,
        TaskCapability::SubmissionExecute => ProviderCapability::SubmissionExecute,
        TaskCapability::SubmissionVerify => ProviderCapability::SubmissionVerify,
        TaskCapability::DurationRead => ProviderCapability::DurationRead,
        TaskCapability::DurationReport => ProviderCapability::DurationReport,
        TaskCapability::Discussion => ProviderCapability::Discussion,
        TaskCapability::Practice => ProviderCapability::Practice,
        TaskCapability::BrowserBridge => ProviderCapability::BrowserBridge,
    }
}

async fn collect_course_tasks(
    inventory: &dyn asterism_provider_api::TaskInventoryCapability,
    context: &ProviderContext,
    courses: &[RemoteCourse],
) -> Result<Vec<RemoteTask>, ProviderScanError> {
    let mut tasks = Vec::new();
    for course in courses {
        let mut course_tasks = inventory.list_tasks(context, Some(course)).await?;
        for task in &mut course_tasks {
            match task.course_remote_id.as_deref() {
                Some(remote_id) if remote_id != course.remote_id => {
                    return Err(ProviderScanError::CourseScopeMismatch {
                        expected: course.remote_id.clone(),
                        actual: remote_id.to_owned(),
                    });
                }
                Some(_) => {}
                None => task.course_remote_id = Some(course.remote_id.clone()),
            }
        }
        tasks.extend(course_tasks);
    }
    Ok(tasks)
}

fn scanned_course(course: RemoteCourse) -> ScannedCourse {
    ScannedCourse {
        remote_id: course.remote_id,
        title: course.title,
        term: course.term,
        teacher: course.teacher,
        remote_status: course.remote_status,
        metadata_sanitized: course.metadata_sanitized,
    }
}

fn scanned_task(task: RemoteTask) -> ScannedTask {
    ScannedTask {
        remote_id: task.remote_id,
        course_remote_id: task.course_remote_id,
        fingerprint: task.fingerprint,
        source_type: task.source_type,
        assessment_class: task.assessment_class,
        title: task.title,
        remote_state: task.remote_state,
        opens_at: task.opens_at,
        due_at: task.due_at,
        closes_at: task.closes_at,
        capabilities: task.capabilities,
        normalized: task.normalized,
        remote_raw_sanitized: task.raw_sanitized,
    }
}

fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderScanError {
    #[error("provider `{0}` is not registered")]
    ProviderNotRegistered(ProviderId),
    #[error("provider `{0}` exposes no inventory capabilities")]
    NoInventoryCapabilities(ProviderId),
    #[error("Provider account `{0}` is not authenticated")]
    AccountNotAuthenticated(asterism_domain::ProviderAccountId),
    #[error("scan correlation id is invalid")]
    InvalidCorrelationId,
    #[error("task inventory escaped course scope `{expected}` with `{actual}`")]
    CourseScopeMismatch { expected: String, actual: String },
    #[error("provider `{provider_id}` returned unadvertised task capability `{capability:?}`")]
    UnadvertisedTaskCapability {
        provider_id: ProviderId,
        capability: TaskCapability,
    },
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use asterism_domain::{
        AssessmentClass, AuthState, ProviderAccountId, RemoteState, SourceType, TaskCapability,
        UserId,
    };
    use asterism_provider_api::{
        CourseInventoryCapability, ProviderCapability, ProviderEntry, ProviderErrorKind,
        ProviderIdentity, ProviderMetadata, ProviderResult, TaskInventoryCapability,
        VerificationLevel,
    };
    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;

    #[derive(Clone, Debug, Default)]
    struct RecordingRepository {
        batches: Arc<Mutex<Vec<ProviderScanBatch>>>,
    }

    #[async_trait]
    impl ProviderScanRepository for RecordingRepository {
        async fn ingest_scan(
            &self,
            batch: &ProviderScanBatch,
        ) -> Result<ProviderScanReport, StorageError> {
            self.batches.lock().unwrap().push(batch.clone());
            Ok(ProviderScanReport {
                courses_seen: batch.courses.len(),
                tasks_created: batch.tasks.len(),
                tasks_updated: 0,
                tasks_unchanged: 0,
                task_changes: Vec::new(),
            })
        }
    }

    #[derive(Debug)]
    struct FakeInventory {
        metadata: ProviderMetadata,
        contexts: Mutex<Vec<ProviderContext>>,
        fail_tasks: bool,
        task_capabilities: Vec<TaskCapability>,
    }

    impl ProviderIdentity for FakeInventory {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl CourseInventoryCapability for FakeInventory {
        async fn list_courses(
            &self,
            context: &ProviderContext,
        ) -> ProviderResult<Vec<RemoteCourse>> {
            self.contexts.lock().unwrap().push(context.clone());
            Ok(vec![RemoteCourse {
                remote_id: "course-a".to_owned(),
                title: "course".to_owned(),
                term: None,
                teacher: None,
                remote_status: None,
                metadata_sanitized: serde_json::json!({"revision": 1}),
                route_context: asterism_provider_api::ProviderRouteContext::default(),
            }])
        }
    }

    #[async_trait]
    impl TaskInventoryCapability for FakeInventory {
        async fn list_tasks(
            &self,
            context: &ProviderContext,
            course: Option<&RemoteCourse>,
        ) -> ProviderResult<Vec<RemoteTask>> {
            self.contexts.lock().unwrap().push(context.clone());
            if self.fail_tasks {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "sanitized inventory failure",
                ));
            }
            assert_eq!(
                course.map(|course| course.remote_id.as_str()),
                Some("course-a")
            );
            Ok(vec![RemoteTask {
                remote_id: "task-a".to_owned(),
                course_remote_id: None,
                title: "task".to_owned(),
                source_type: SourceType::Work,
                assessment_class: AssessmentClass::Unknown,
                remote_state: RemoteState::Pending,
                opens_at: None,
                due_at: None,
                closes_at: None,
                capabilities: self.task_capabilities.clone(),
                fingerprint: "v1:fingerprint-a".to_owned(),
                normalized: serde_json::json!({"revision": 1}),
                raw_sanitized: serde_json::json!({"task": "safe"}),
            }])
        }
    }

    #[tokio::test]
    async fn scan_collects_capabilities_before_one_repository_write() {
        let metadata = metadata([
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
        ]);
        let inventory = Arc::new(FakeInventory {
            metadata: metadata.clone(),
            contexts: Mutex::new(Vec::new()),
            fail_tasks: false,
            task_capabilities: Vec::new(),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata,
                runtime_settings: asterism_provider_api::ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: Some(inventory.clone()),
                task_inventory: Some(inventory.clone()),
                task_detail: None,
                task_progress: None,
                question_inventory: None,
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let repository = RecordingRepository::default();
        let service = ProviderScanService::new(Arc::new(registry), repository.clone());
        let report = service
            .scan_account(&account(), "scan-1", None, Utc::now())
            .await
            .unwrap();

        assert_eq!((report.courses_seen, report.tasks_created), (1, 1));
        let batches = repository.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].provider_version, "0.1.0");
        assert_eq!(
            batches[0].tasks[0].course_remote_id.as_deref(),
            Some("course-a")
        );
        assert_eq!(
            batches[0].tasks[0].assessment_class,
            AssessmentClass::Unknown
        );
        let contexts = inventory.contexts.lock().unwrap();
        assert_eq!(contexts.len(), 2);
        assert_eq!(contexts[0].credential_refs.len(), 1);
    }

    #[tokio::test]
    async fn scan_rejects_registered_provider_without_inventory() {
        let metadata = metadata([]);
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry::metadata_only(metadata))
            .unwrap();
        let repository = RecordingRepository::default();
        let service = ProviderScanService::new(Arc::new(registry), repository.clone());

        assert!(matches!(
            service
                .scan_account(&account(), "scan-1", None, Utc::now())
                .await,
            Err(ProviderScanError::NoInventoryCapabilities(_))
        ));
        assert!(repository.batches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn scan_does_not_write_after_provider_failure() {
        let metadata = metadata([
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
        ]);
        let inventory = Arc::new(FakeInventory {
            metadata: metadata.clone(),
            contexts: Mutex::new(Vec::new()),
            fail_tasks: true,
            task_capabilities: Vec::new(),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata,
                runtime_settings: asterism_provider_api::ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: Some(inventory.clone()),
                task_inventory: Some(inventory),
                task_detail: None,
                task_progress: None,
                question_inventory: None,
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let repository = RecordingRepository::default();
        let service = ProviderScanService::new(Arc::new(registry), repository.clone());

        assert!(matches!(
            service
                .scan_account(&account(), "scan-1", None, Utc::now())
                .await,
            Err(ProviderScanError::Provider(error)) if error.is_retryable()
        ));
        assert!(repository.batches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn scan_rejects_task_capabilities_missing_from_provider_metadata() {
        let metadata = metadata([
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
        ]);
        let inventory = Arc::new(FakeInventory {
            metadata: metadata.clone(),
            contexts: Mutex::new(Vec::new()),
            fail_tasks: false,
            task_capabilities: vec![TaskCapability::ProgressRead],
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata,
                runtime_settings: asterism_provider_api::ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: Some(inventory.clone()),
                task_inventory: Some(inventory),
                task_detail: None,
                task_progress: None,
                question_inventory: None,
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                task_execution: None,
                browser_bridge: None,
            })
            .unwrap();
        let repository = RecordingRepository::default();
        let service = ProviderScanService::new(Arc::new(registry), repository.clone());

        assert!(matches!(
            service
                .scan_account(&account(), "scan-1", None, Utc::now())
                .await,
            Err(ProviderScanError::UnadvertisedTaskCapability {
                capability: TaskCapability::ProgressRead,
                ..
            })
        ));
        assert!(repository.batches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn scan_rejects_unauthenticated_account_before_provider_calls() {
        let repository = RecordingRepository::default();
        let service =
            ProviderScanService::new(Arc::new(ProviderRegistry::default()), repository.clone());
        let mut account = account();
        account.auth_state = AuthState::Idle;

        assert!(matches!(
            service
                .scan_account(&account, "scan-1", None, Utc::now())
                .await,
            Err(ProviderScanError::AccountNotAuthenticated(_))
        ));
        assert!(repository.batches.lock().unwrap().is_empty());
    }

    fn metadata<const N: usize>(capabilities: [ProviderCapability; N]) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "provider-alpha".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: Some(300),
            capture_recipe_version: None,
            capabilities: BTreeSet::from(capabilities),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        }
    }

    fn account() -> ProviderAccount {
        let now = Utc::now();
        ProviderAccount {
            id: ProviderAccountId::new(),
            owner_id: UserId::new(),
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: vec![asterism_domain::SecretId::new()],
            created_at: now,
            updated_at: now,
        }
    }
}
