use std::{collections::BTreeMap, sync::Arc};

use asterism_domain::ProviderId;

use crate::{
    AnswerHistoryHarvestCapability, AnswerResolveCapability, AuthenticationCapability,
    BrowserBridgeCapability, BrowserBridgeResultDisposition, CourseEnrollmentCapability,
    CourseInventoryCapability, DurationReadCapability, ProviderCapability, ProviderIdentity,
    ProviderMetadata, ProviderRuntimeSettingsSchema, ProviderSettingsError,
    QuestionInventoryCapability, QuestionParseCapability, SubmissionBuildCapability,
    SubmissionExecuteCapability, SubmissionVerifyCapability, TaskDetailCapability,
    TaskExecutionCapability, TaskInventoryCapability, TaskProgressCapability,
};

const MAX_CAPTURE_RECIPE_ALTERNATIVES: usize = 8;

#[derive(Clone)]
pub struct ProviderEntry {
    pub metadata: ProviderMetadata,
    pub runtime_settings: ProviderRuntimeSettingsSchema,
    pub authentication: Option<Arc<dyn AuthenticationCapability>>,
    pub course_inventory: Option<Arc<dyn CourseInventoryCapability>>,
    pub course_enrollment: Option<Arc<dyn CourseEnrollmentCapability>>,
    pub task_inventory: Option<Arc<dyn TaskInventoryCapability>>,
    pub task_detail: Option<Arc<dyn TaskDetailCapability>>,
    pub task_progress: Option<Arc<dyn TaskProgressCapability>>,
    pub duration_read: Option<Arc<dyn DurationReadCapability>>,
    pub question_inventory: Option<Arc<dyn QuestionInventoryCapability>>,
    pub question_parse: Option<Arc<dyn QuestionParseCapability>>,
    pub answer_resolve: Option<Arc<dyn AnswerResolveCapability>>,
    pub submission_build: Option<Arc<dyn SubmissionBuildCapability>>,
    pub submission_execute: Option<Arc<dyn SubmissionExecuteCapability>>,
    pub submission_verify: Option<Arc<dyn SubmissionVerifyCapability>>,
    pub answer_history_harvest: Option<Arc<dyn AnswerHistoryHarvestCapability>>,
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
            .field("course_enrollment", &self.course_enrollment.is_some())
            .field("task_inventory", &self.task_inventory.is_some())
            .field("task_detail", &self.task_detail.is_some())
            .field("task_progress", &self.task_progress.is_some())
            .field("duration_read", &self.duration_read.is_some())
            .field("question_inventory", &self.question_inventory.is_some())
            .field("question_parse", &self.question_parse.is_some())
            .field("answer_resolve", &self.answer_resolve.is_some())
            .field("submission_build", &self.submission_build.is_some())
            .field("submission_execute", &self.submission_execute.is_some())
            .field("submission_verify", &self.submission_verify.is_some())
            .field(
                "answer_history_harvest",
                &self.answer_history_harvest.is_some(),
            )
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
            course_enrollment: None,
            task_inventory: None,
            task_detail: None,
            task_progress: None,
            duration_read: None,
            question_inventory: None,
            question_parse: None,
            answer_resolve: None,
            submission_build: None,
            submission_execute: None,
            submission_verify: None,
            answer_history_harvest: None,
            task_execution: None,
            browser_bridge: None,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the registry keeps the complete capability-to-slot integrity matrix in one auditable validation path"
    )]
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
        let capture_recipes = self
            .authentication
            .as_ref()
            .map_or_else(Vec::new, |authentication| authentication.capture_recipes());
        let capture_recipe_valid = match (
            self.metadata.capture_recipe_version,
            capture_recipes.first(),
        ) {
            (None, None) => true,
            (Some(version), Some(recipe)) => {
                version == recipe.version
                    && capture_recipes.len() <= MAX_CAPTURE_RECIPE_ALTERNATIVES
                    && capture_recipes.iter().all(|recipe| {
                        recipe.validate().is_ok()
                            && self.metadata.auth_methods.contains(&recipe.auth_method)
                            && self.metadata.session_kinds.contains(&recipe.session_kind)
                    })
                    && capture_recipes
                        .iter()
                        .enumerate()
                        .all(|(position, recipe)| {
                            capture_recipes[..position]
                                .iter()
                                .all(|earlier| earlier.version != recipe.version)
                        })
            }
            (None, Some(_)) | (Some(_), None) => false,
        };
        if !capture_recipe_valid {
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
                ProviderCapability::CourseEnrollment,
                self.course_enrollment.is_some(),
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
                ProviderCapability::DurationRead,
                self.duration_read.is_some(),
            ),
            (
                ProviderCapability::QuestionInventory,
                self.question_inventory.is_some(),
            ),
            (
                ProviderCapability::QuestionParse,
                self.question_parse.is_some(),
            ),
            (
                ProviderCapability::AnswerResolve,
                self.answer_resolve.is_some(),
            ),
            (
                ProviderCapability::SubmissionBuild,
                self.submission_build.is_some(),
            ),
            (
                ProviderCapability::SubmissionExecute,
                self.submission_execute.is_some(),
            ),
            (
                ProviderCapability::SubmissionVerify,
                self.submission_verify.is_some(),
            ),
            (
                ProviderCapability::AnswerHistoryHarvest,
                self.answer_history_harvest.is_some(),
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

        if self
            .metadata
            .advertises(ProviderCapability::ExecutionVerify)
            && !advertises_execution
        {
            return Err(RegistryError::ExecutionVerificationMismatch {
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
        if self
            .browser_bridge
            .as_deref()
            .is_some_and(|capability| !valid_browser_bridge_result_types(capability))
        {
            return Err(RegistryError::InvalidBrowserBridgeResultTypes {
                provider_id: self.metadata.id.clone(),
            });
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the registry keeps every capability slot identity in one complete audit matrix"
    )]
    fn validate_identities(&self) -> Result<(), RegistryError> {
        self.validate_answer_history_identity()?;
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
                "course_enrollment",
                self.course_enrollment
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
                "duration_read",
                self.duration_read
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
                "answer_resolve",
                self.answer_resolve
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "submission_build",
                self.submission_build
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "submission_execute",
                self.submission_execute
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "submission_verify",
                self.submission_verify
                    .as_ref()
                    .map(|implementation| implementation.metadata()),
            ),
            (
                "task_execution",
                self.task_execution
                    .as_deref()
                    .map(ProviderIdentity::metadata),
            ),
            (
                "browser_bridge",
                self.browser_bridge
                    .as_deref()
                    .map(ProviderIdentity::metadata),
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

    fn validate_answer_history_identity(&self) -> Result<(), RegistryError> {
        if self
            .answer_history_harvest
            .as_ref()
            .is_some_and(|implementation| implementation.metadata() != &self.metadata)
        {
            return Err(RegistryError::IdentityMismatch {
                provider_id: self.metadata.id.clone(),
                slot: "answer_history_harvest",
            });
        }
        Ok(())
    }
}

fn valid_browser_bridge_result_types(capability: &dyn BrowserBridgeCapability) -> bool {
    let sets = [
        (
            BrowserBridgeResultDisposition::CredentialTerminal,
            capability.browser_bridge_credential_result_types(),
        ),
        (
            BrowserBridgeResultDisposition::Intermediate,
            capability.browser_bridge_intermediate_result_types(),
        ),
        (
            BrowserBridgeResultDisposition::ExecutionTerminal,
            capability.browser_bridge_execution_result_types(),
        ),
    ];
    let mut declared = Vec::new();
    for (disposition, result_types) in sets {
        if result_types.len() > 16 {
            return false;
        }
        for result_type in result_types {
            if result_type.is_empty()
                || result_type.len() > 128
                || !result_type.is_ascii()
                || result_type.trim() != *result_type
                || result_type.chars().any(char::is_control)
                || declared.contains(result_type)
                || capability.browser_bridge_result_disposition(result_type) != Some(disposition)
            {
                return false;
            }
            declared.push(*result_type);
        }
    }
    true
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
    pub fn register(&mut self, mut entry: ProviderEntry) -> Result<(), RegistryError> {
        entry.runtime_settings = entry
            .runtime_settings
            .with_core_completion_policy()
            .map_err(|source| RegistryError::InvalidRuntimeSettingsSchema {
                provider_id: entry.metadata.id.clone(),
                source,
            })?;
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
    #[error(
        "provider `{provider_id}` execution verification requires both task execution and progress read"
    )]
    ExecutionVerificationMismatch { provider_id: ProviderId },
    #[error("provider `{provider_id}` has invalid BrowserBridge result type declarations")]
    InvalidBrowserBridgeResultTypes { provider_id: ProviderId },
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

    use asterism_domain::{
        AnswerCandidate, ProviderId, Question, SelectedAnswer, SubmissionDraft,
        SubmissionPayloadEncoding, SubmissionPayloadPreview, SubmissionReceipt,
        SubmissionVerificationSnapshot, TaskId,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::{
        AnswerHistoryCursor, AnswerHistoryPage, AnswerHistoryTaskRequest, BrowserSessionSpec,
        ProviderAnswerHistoryTaskEvidence, ProviderContext, ProviderResult,
        ProviderSettingCoreBehavior, RemoteCourse, RemoteQuestionRef, RemoteTask,
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

    #[derive(Debug)]
    struct FakeAnswerHistory {
        metadata: ProviderMetadata,
    }

    #[derive(Debug)]
    struct FakeBrowserBridge {
        metadata: ProviderMetadata,
        duplicate_declaration: bool,
    }

    impl crate::ProviderIdentity for FakeBrowserBridge {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl BrowserBridgeCapability for FakeBrowserBridge {
        async fn browser_session_spec(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<BrowserSessionSpec> {
            unreachable!("registry declaration test does not create a browser session")
        }

        fn browser_bridge_result_disposition(
            &self,
            result_type: &str,
        ) -> Option<BrowserBridgeResultDisposition> {
            match result_type {
                "chaoxing.browser.event" => Some(BrowserBridgeResultDisposition::Intermediate),
                "chaoxing.browser.terminal" => {
                    Some(BrowserBridgeResultDisposition::ExecutionTerminal)
                }
                _ => None,
            }
        }

        fn browser_bridge_intermediate_result_types(&self) -> &'static [&'static str] {
            &["chaoxing.browser.event"]
        }

        fn browser_bridge_execution_result_types(&self) -> &'static [&'static str] {
            if self.duplicate_declaration {
                &["chaoxing.browser.event"]
            } else {
                &["chaoxing.browser.terminal"]
            }
        }
    }

    impl crate::ProviderIdentity for FakeQuestionRead {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    impl crate::ProviderIdentity for FakeAnswerHistory {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl AnswerHistoryHarvestCapability for FakeAnswerHistory {
        async fn list_answer_history(
            &self,
            _context: &ProviderContext,
            _cursor: Option<&AnswerHistoryCursor>,
        ) -> ProviderResult<AnswerHistoryPage> {
            unreachable!("registry identity test does not enumerate history")
        }

        async fn read_answer_history_task(
            &self,
            _context: &ProviderContext,
            _request: &AnswerHistoryTaskRequest,
        ) -> ProviderResult<ProviderAnswerHistoryTaskEvidence> {
            unreachable!("registry identity test does not read history")
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

    #[async_trait]
    impl AnswerResolveCapability for FakeQuestionRead {
        async fn resolve_answers(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _questions: &[Question],
        ) -> ProviderResult<Vec<AnswerCandidate>> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl SubmissionBuildCapability for FakeQuestionRead {
        async fn build_submission_preview(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _questions: &[Question],
            _selected_answers: &[SelectedAnswer],
        ) -> ProviderResult<SubmissionPayloadPreview> {
            Ok(SubmissionPayloadPreview {
                encoding: SubmissionPayloadEncoding::ProviderSpecific,
                format: "fixture.v1".to_owned(),
                fields: Vec::new(),
            })
        }
    }

    #[async_trait]
    impl SubmissionExecuteCapability for FakeQuestionRead {
        async fn execute_submission(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _draft: &SubmissionDraft,
            _runtime_settings: &crate::ResolvedProviderRuntimeSettings,
            _events: &(dyn crate::ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<SubmissionReceipt> {
            unreachable!("registry identity test does not invoke submission")
        }
    }

    #[async_trait]
    impl SubmissionVerifyCapability for FakeQuestionRead {
        async fn verify_submission(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _draft: &SubmissionDraft,
            _receipt: Option<&SubmissionReceipt>,
        ) -> ProviderResult<SubmissionVerificationSnapshot> {
            unreachable!("registry identity test does not invoke verification")
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
        assert_eq!(
            registry
                .get(&ProviderId::new("chaoxing").unwrap())
                .unwrap()
                .runtime_settings
                .definitions
                .iter()
                .filter(|definition| {
                    matches!(
                        definition.core_behavior,
                        Some(
                            ProviderSettingCoreBehavior::StrictCompletionEnabled
                                | ProviderSettingCoreBehavior::ScoreImprovementEnabled
                                | ProviderSettingCoreBehavior::StrictCompletionAttemptLimit
                                | ProviderSettingCoreBehavior::ScoreImprovementAttemptLimit
                                | ProviderSettingCoreBehavior::ScoreImprovementTarget
                                | ProviderSettingCoreBehavior::StrictCompletionTimeLimit
                                | ProviderSettingCoreBehavior::ScoreImprovementTimeLimit
                        )
                    )
                })
                .count(),
            7
        );
    }

    #[test]
    fn registry_requires_unique_disposition_matched_browser_result_types() {
        let browser_entry = |duplicate_declaration| {
            let mut metadata = metadata();
            metadata
                .capabilities
                .insert(ProviderCapability::BrowserBridge);
            let browser = Arc::new(FakeBrowserBridge {
                metadata: metadata.clone(),
                duplicate_declaration,
            });
            let mut entry = ProviderEntry::metadata_only(metadata);
            entry.browser_bridge = Some(browser);
            entry
        };
        let mut registry = ProviderRegistry::default();
        registry.register(browser_entry(false)).unwrap();

        let mut invalid_registry = ProviderRegistry::default();
        assert!(matches!(
            invalid_registry.register(browser_entry(true)),
            Err(RegistryError::InvalidBrowserBridgeResultTypes { .. })
        ));
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
    fn answer_history_advertisement_requires_the_exact_provider_slot() {
        let mut advertised = metadata();
        advertised
            .capabilities
            .insert(ProviderCapability::AnswerHistoryHarvest);
        assert!(matches!(
            ProviderRegistry::default().register(ProviderEntry::metadata_only(advertised.clone())),
            Err(RegistryError::CapabilityMismatch {
                capability: ProviderCapability::AnswerHistoryHarvest,
                ..
            })
        ));

        let implementation = Arc::new(FakeAnswerHistory {
            metadata: advertised.clone(),
        });
        let mut entry = ProviderEntry::metadata_only(advertised);
        entry.answer_history_harvest = Some(implementation);
        ProviderRegistry::default().register(entry).unwrap();
    }

    #[test]
    fn execution_verification_requires_an_execution_contract() {
        let mut metadata = metadata();
        metadata
            .capabilities
            .insert(ProviderCapability::ExecutionVerify);
        let mut registry = ProviderRegistry::default();

        assert!(matches!(
            registry.register(ProviderEntry::metadata_only(metadata)),
            Err(RegistryError::ExecutionVerificationMismatch { .. })
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

    #[test]
    fn answer_resolution_is_independent_from_task_execution() {
        let mut metadata = metadata();
        metadata
            .capabilities
            .insert(ProviderCapability::AnswerResolve);
        let implementation = Arc::new(FakeQuestionRead {
            metadata: metadata.clone(),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.answer_resolve = Some(implementation);
        assert!(entry.task_execution.is_none());

        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
    }

    #[test]
    fn submission_build_is_independent_from_task_execution() {
        let mut metadata = metadata();
        metadata
            .capabilities
            .insert(ProviderCapability::SubmissionBuild);
        let implementation = Arc::new(FakeQuestionRead {
            metadata: metadata.clone(),
        });
        let mut entry = ProviderEntry::metadata_only(metadata);
        entry.submission_build = Some(implementation);
        assert!(entry.task_execution.is_none());

        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
    }

    #[test]
    fn submission_execute_and_verify_have_independent_slots() {
        let mut execute_metadata = metadata();
        execute_metadata
            .capabilities
            .insert(ProviderCapability::SubmissionExecute);
        let execute = Arc::new(FakeQuestionRead {
            metadata: execute_metadata.clone(),
        });
        let mut execute_entry = ProviderEntry::metadata_only(execute_metadata);
        execute_entry.submission_execute = Some(execute);
        assert!(execute_entry.task_execution.is_none());
        assert!(execute_entry.submission_verify.is_none());
        ProviderRegistry::default().register(execute_entry).unwrap();

        let mut verify_metadata = metadata();
        verify_metadata
            .capabilities
            .insert(ProviderCapability::SubmissionVerify);
        let verify = Arc::new(FakeQuestionRead {
            metadata: verify_metadata.clone(),
        });
        let mut verify_entry = ProviderEntry::metadata_only(verify_metadata);
        verify_entry.submission_verify = Some(verify);
        assert!(verify_entry.task_execution.is_none());
        assert!(verify_entry.submission_execute.is_none());
        ProviderRegistry::default().register(verify_entry).unwrap();
    }
}
