use std::{collections::BTreeMap, sync::Arc};

use asterism_domain::ProviderId;

use crate::{
    AuthenticationCapability, BrowserBridgeCapability, CourseInventoryCapability,
    ProviderCapability, ProviderMetadata, TaskDetailCapability, TaskExecutionCapability,
    TaskInventoryCapability, TaskProgressCapability,
};

#[derive(Clone)]
pub struct ProviderEntry {
    pub metadata: ProviderMetadata,
    pub authentication: Option<Arc<dyn AuthenticationCapability>>,
    pub course_inventory: Option<Arc<dyn CourseInventoryCapability>>,
    pub task_inventory: Option<Arc<dyn TaskInventoryCapability>>,
    pub task_detail: Option<Arc<dyn TaskDetailCapability>>,
    pub task_progress: Option<Arc<dyn TaskProgressCapability>>,
    pub task_execution: Option<Arc<dyn TaskExecutionCapability>>,
    pub browser_bridge: Option<Arc<dyn BrowserBridgeCapability>>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderEntry")
            .field("metadata", &self.metadata)
            .field("authentication", &self.authentication.is_some())
            .field("course_inventory", &self.course_inventory.is_some())
            .field("task_inventory", &self.task_inventory.is_some())
            .field("task_detail", &self.task_detail.is_some())
            .field("task_progress", &self.task_progress.is_some())
            .field("task_execution", &self.task_execution.is_some())
            .field("browser_bridge", &self.browser_bridge.is_some())
            .finish()
    }
}

impl ProviderEntry {
    pub fn metadata_only(metadata: ProviderMetadata) -> Self {
        Self {
            metadata,
            authentication: None,
            course_inventory: None,
            task_inventory: None,
            task_detail: None,
            task_progress: None,
            task_execution: None,
            browser_bridge: None,
        }
    }

    fn validate(&self) -> Result<(), RegistryError> {
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
            ProviderCapability::QuestionInventory,
            ProviderCapability::QuestionParse,
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
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Arc};

    use asterism_domain::ProviderId;
    use async_trait::async_trait;

    use super::*;
    use crate::{ProviderContext, ProviderResult, RemoteCourse, RemoteTask, VerificationLevel};

    #[derive(Debug)]
    struct OtherTaskInventory {
        metadata: ProviderMetadata,
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
}
