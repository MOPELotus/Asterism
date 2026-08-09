use std::{collections::BTreeMap, sync::Arc};

use asterism_domain::ProviderId;

use crate::{
    AuthenticationCapability, BrowserBridgeCapability, CourseInventoryCapability,
    ProviderCapability, ProviderMetadata, ProviderRuntimeSettingsSchema, ProviderSettingsError,
    QuestionInventoryCapability, QuestionParseCapability, TaskDetailCapability,
    TaskExecutionCapability, TaskInventoryCapability, TaskProgressCapability,
};

#[derive(Clone)]
pub struct ProviderEntry {
    pub metadata: ProviderMetadata,
    pub runtime_settings: ProviderRuntimeSettingsSchema,
    pub authentication: Option<Arc<dyn AuthenticationCapability>>,
    pub course_inventory: Option<Arc<dyn CourseInventoryCapability>>,
    pub task_inventory: Option<Arc<dyn TaskInventoryCapability>>,
    pub task_detail: Option<Arc<dyn TaskDetailCapability>>,
    pub task_progress: Option<Arc<dyn TaskProgressCapability>>,
    pub question_inventory: Option<Arc<dyn QuestionInventoryCapability>>,
    pub question_parse: Option<Arc<dyn QuestionParseCapability>>,
    pub task_execution: Option<Arc<dyn TaskExecutionCapability>>,
    pub browser_bridge: Option<Arc<dyn BrowserBridgeCapability>>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderEntry")
            .field("metadata", &self.metadata)
            .field("runtime_settings", &self.runtime_settings)
            .field("authentication", &self.authentication.is_some())
            .field("course_inventory", &self.course_inventory.is_some())
            .field("task_inventory", &self.task_inventory.is_some())
            .field("task_detail", &self.task_detail.is_some())
            .field("task_progress", &self.task_progress.is_some())
            .field("question_inventory", &self.question_inventory.is_some())
            .field("question_parse", &self.question_parse.is_some())
            .field("task_execution", &self.task_execution.is_some())
            .field("browser_bridge", &self.browser_bridge.is_some())
            .finish()
    }
}

impl ProviderEntry {
    pub fn metadata_only(metadata: ProviderMetadata) -> Self {
        Self {
            metadata,
            runtime_settings: ProviderRuntimeSettingsSchema::default(),
            authentication: None,
            course_inventory: None,
            task_inventory: None,
            task_detail: None,
            task_progress: None,
            question_inventory: None,
            question_parse: None,
            task_execution: None,
            browser_bridge: None,
        }
    }

    fn validate(&self) -> Result<(), RegistryError> {
        self.runtime_settings.validate().map_err(|source| {
            RegistryError::InvalidRuntimeSettingsSchema {
                provider_id: self.metadata.id.clone(),
                source,
            }
        })?;
        if self
            .metadata
            .scan_min_interval_seconds
            .is_some_and(|interval| interval == 0 || i64::try_from(interval).is_err())
        {
            return Err(RegistryError::InvalidScanMinimum {
                provider_id: self.metadata.id.clone(),
            });
        }
        if self
            .metadata
            .capture_recipe_version
            .is_some_and(|version| version == 0)
            || (self.metadata.capture_recipe_version.is_some()
                && !self.metadata.advertises(ProviderCapability::Authentication))
        {
            return Err(RegistryError::InvalidCaptureRecipe {
                provider_id: self.metadata.id.clone(),
            });
        }

        let checks = [
            (
                ProviderCapability::Authentication,
                self.authentication.is_some(),
            ),
            (
                ProviderCapability::CourseInventory,
                self.course_inventory.is_some(),
            ),
            (
                ProviderCapability::TaskInventory,
                self.task_inventory.is_some(),
            ),
            (ProviderCapability::TaskDetail, self.task_detail.is_some()),
            (
                ProviderCapability::TaskProgressRead,
                self.task_progress.is_some(),
            ),
            (
                ProviderCapability::QuestionInventory,
                self.question_inventory.is_some(),
            ),
            (
                ProviderCapability::QuestionParse,
                self.question_parse.is_some(),
            ),
        ];
        for (capability, implemented) in checks {
            if self.metadata.advertises(capability) != implemented {
                return Err(RegistryError::CapabilityMismatch {
                    provider_id: self.metadata.id.clone(),
                    capability,
                });
            }
        }

        self.validate_identities()?;

        let execution_capabilities = [
            ProviderCapability::ResourceExecution,
            ProviderCapability::AnswerResolve,
            ProviderCapability::SubmissionBuild,
            ProviderCapability::SubmissionExecute,
            ProviderCapability::SubmissionVerify,
            ProviderCapability::DurationRead,
            ProviderCapability::DurationReport,
            ProviderCapability::Discussion,
            ProviderCapability::Practice,
        ];
        let advertises_execution = execution_capabilities
            .into_iter()
            .any(|capability| self.metadata.advertises(capability));
        if advertises_execution != self.task_execution.is_some() {
            return Err(RegistryError::ExecutionCapabilityMismatch {
                provider_id: self.metadata.id.clone(),
            });
        }

        if self.metadata.advertises(ProviderCapability::BrowserBridge)
            != self.browser_bridge.is_some()
        {
            return Err(RegistryError::CapabilityMismatch {
                provider_id: self.metadata.id.clone(),
                capability: ProviderCapability::BrowserBridge,
            });
        }
        Ok(())
    }

    fn validate_identities(&self) -> Result<(), RegistryError> {
        let identities = [
            (
                "authentication",
                self.authentication
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "course_inventory",
                self.course_inventory
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "task_inventory",
                self.task_inventory
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "task_detail",
                self.task_detail
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "task_progress",
                self.task_progress
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "question_inventory",
                self.question_inventory
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "question_parse",
                self.question_parse
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "task_execution",
                self.task_execution
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "browser_bridge",
                self.browser_bridge
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
        ];
        for (slot, implementation) in identities {
            if implementation.is_some_and(|implementation| implementation != &self.metadata) {
                return Err(RegistryError::IdentityMismatch {
                    provider_id: self.metadata.id.clone(),
                    slot,
                });
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderRegistry {
    entries: BTreeMap<ProviderId, ProviderEntry>,
}

impl ProviderRegistry {
    /// Registers an implementation after checking advertised capability slots.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] when an identifier is duplicated or metadata
    /// does not match the attached capability implementations.
    pub fn register(&mut self, entry: ProviderEntry) -> Result<(), RegistryError> {
        entry.validate()?;
        let provider_id = entry.metadata.id.clone();
        if self.entries.contains_key(&provider_id) {
            return Err(RegistryError::Duplicate(provider_id));
        }
        self.entries.insert(provider_id, entry);
        Ok(())
    }

    pub fn get(&self, provider_id: &ProviderId) -> Option<&ProviderEntry> {
        self.entries.get(provider_id)
    }

    pub fn metadata(&self) -> impl Iterator<Item = &ProviderMetadata> {
        self.entries.values().map(|entry| &entry.metadata)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RegistryError {
    #[error("provider `{0}` is already registered")]
    Duplicate(ProviderId),
    #[error("provider `{provider_id}` metadata disagrees with its `{capability:?}` implementation")]
    CapabilityMismatch {
        provider_id: ProviderId,
        capability: ProviderCapability,
    },
    #[error(
        "provider `{provider_id}` execution capability metadata disagrees with its implementation"
    )]
    ExecutionCapabilityMismatch { provider_id: ProviderId },
    #[error("provider `{provider_id}` has a mismatched implementation in `{slot}`")]
    IdentityMismatch {
        provider_id: ProviderId,
        slot: &'static str,
    },
    #[error("provider `{provider_id}` declares an invalid minimum scan interval")]
    InvalidScanMinimum { provider_id: ProviderId },
    #[error("provider `{provider_id}` declares an invalid Capture recipe contract")]
    InvalidCaptureRecipe { provider_id: ProviderId },
    #[error("provider `{provider_id}` declares an invalid runtime settings schema")]
    InvalidRuntimeSettingsSchema {
        provider_id: ProviderId,
        #[source]
        source: ProviderSettingsError,
    },
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Arc};

    use asterism_domain::{ProviderId, Question, TaskId};
    use async_trait::async_trait;

    use super::*;
    use crate::{
        ProviderContext, ProviderResult, RemoteCourse, RemoteQuestionRef, RemoteTask,
        VerificationLevel,
    };

    #[derive(Debug)]
    struct OtherTaskInventory {
        metadata: ProviderMetadata,
    }

    #[derive(Debug)]
    struct FakeQuestionRead {
        metadata: ProviderMetadata,
    }

    impl crate::ProviderIdentity for FakeQuestionRead {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl QuestionInventoryCapability for FakeQuestionRead {
        async fn list_question_refs(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<Vec<RemoteQuestionRef>> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl QuestionParseCapability for FakeQuestionRead {
        async fn parse_question(
            &self,
            _context: &ProviderContext,
            _task_id: TaskId,
            _remote_task_id: &str,
            _question: &RemoteQuestionRef,
        ) -> ProviderResult<Question> {
            unreachable!("registry identity test does not invoke parsing")
        }
    }

    impl crate::ProviderIdentity for OtherTaskInventory {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskInventoryCapability for OtherTaskInventory {
        async fn list_tasks(
            &self,
            _context: &ProviderContext,
            _course: Option<&RemoteCourse>,
        ) -> ProviderResult<Vec<RemoteTask>> {
            Ok(Vec::new())
        }
    }

    fn metadata() -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("chaoxing").unwrap(),
            display_name: "chaoxing".into(),
            implementation_version: "0.0.0".into(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: Some(300),
            capture_recipe_version: None,
            capabilities: BTreeSet::new(),
            auth_methods: BTreeSet::new(),
            session_kinds: BTreeSet::new(),
        }
    }

    #[test]
    fn registry_rejects_duplicate_provider_ids() {
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry::metadata_only(metadata()))
            .unwrap();
        assert!(matches!(
            registry.register(ProviderEntry::metadata_only(metadata())),
            Err(RegistryError::Duplicate(_))
        ));
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry
                .get(&ProviderId::new("chaoxing").unwrap())
                .unwrap()
                .metadata
                .display_name,
            "chaoxing"
        );
    }

    #[test]
    fn registry_rejects_invalid_minimum_scan_interval() {
        let mut metadata = metadata();
        metadata.scan_min_interval_seconds = Some(0);
        let mut registry = ProviderRegistry::default();

        assert!(matches!(
            registry.register(ProviderEntry::metadata_only(metadata)),
            Err(RegistryError::InvalidScanMinimum { .. })
        ));
    }

    #[test]
    fn registry_rejects_invalid_runtime_settings_schema() {
        let mut entry = ProviderEntry::metadata_only(metadata());
        entry.runtime_settings.version = 0;
        let mut registry = ProviderRegistry::default();

        assert!(matches!(
            registry.register(entry),
            Err(RegistryError::InvalidRuntimeSettingsSchema { .. })
        ));
    }

    #[test]
    fn registry_rejects_zero_or_non_authentication_capture_recipe() {
        let mut registry = ProviderRegistry::default();
        let mut zero = metadata();
        zero.capture_recipe_version = Some(0);
        assert!(matches!(
            registry.register(ProviderEntry::metadata_only(zero)),
            Err(RegistryError::InvalidCaptureRecipe { .. })
        ));

        let mut without_authentication = metadata();
        without_authentication.capture_recipe_version = Some(1);
        assert!(matches!(
            registry.register(ProviderEntry::metadata_only(without_authentication)),
            Err(RegistryError::InvalidCaptureRecipe { .. })
        ));
    }

    #[test]
    fn registry_rejects_advertised_but_missing_capability() {
        let mut metadata = metadata();
        metadata
            .capabilities
            .insert(ProviderCapability::TaskInventory);
        let mut registry = ProviderRegistry::default();
        assert!(matches!(
            registry.register(ProviderEntry::metadata_only(metadata)),
            Err(RegistryError::CapabilityMismatch { .. })
        ));
    }

    #[test]
    fn registry_rejects_capability_from_another_provider() {
        let mut entry = ProviderEntry::metadata_only(metadata());
        entry
            .metadata
            .capabilities
            .insert(ProviderCapability::TaskInventory);
        let mut other_metadata = metadata();
        other_metadata.id = ProviderId::new("provider-beta").unwrap();
        entry.task_inventory = Some(Arc::new(OtherTaskInventory {
            metadata: other_metadata,
        }));
        let mut registry = ProviderRegistry::default();

        assert!(matches!(
            registry.register(entry),
            Err(RegistryError::IdentityMismatch {
                slot: "task_inventory",
                ..
            })
        ));
    }

    #[test]
    fn question_read_capabilities_are_independent_from_task_execution() {
        let mut metadata = metadata();
        metadata.capabilities.extend([
            ProviderCapability::QuestionInventory,
            ProviderCapability::QuestionParse,
        ]);
        let implementation = Arc::new(FakeQuestionRead {
            metadata: metadata.clone(),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.question_inventory = Some(implementation.clone());
        entry.question_parse = Some(implementation);
        assert!(entry.task_execution.is_none());

        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
    }
}
