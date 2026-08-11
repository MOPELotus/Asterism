use std::{fmt, sync::Arc};

use asterism_networking::ResolvedNetworkProfile;
use asterism_provider_api::{ProviderEntry, ProviderResult, ProviderRuntimeSettingsSchema};
use asterism_secrets::{ProviderCredentialRenewer, ProviderCredentialResolver};

use crate::{
    NativeUaiAuthenticationTransport, NativeUaiInventoryTransport, StoredUaiSessionResolver,
    UaiAnswerResolve, UaiAnswerTransport, UaiAuthentication, UaiAuthenticationTransport,
    UaiCourseInventory, UaiCourseInventoryTransport, UaiDurationTransport, UaiProgressTransport,
    UaiQuestionRead, UaiQuestionTransport, UaiSessionResolver, UaiTaskDetail, UaiTaskDuration,
    UaiTaskInventory, UaiTaskInventoryTransport, UaiTaskProgress, metadata::development_metadata,
};

/// Injected read boundaries used by the complete UAI Development entry.
pub struct UaiDevelopmentTransports {
    course: Arc<dyn UaiCourseInventoryTransport>,
    task: Arc<dyn UaiTaskInventoryTransport>,
    progress: Arc<dyn UaiProgressTransport>,
    duration: Arc<dyn UaiDurationTransport>,
    question: Arc<dyn UaiQuestionTransport>,
    answer: Arc<dyn UaiAnswerTransport>,
}

impl UaiDevelopmentTransports {
    pub fn new(
        course: Arc<dyn UaiCourseInventoryTransport>,
        task: Arc<dyn UaiTaskInventoryTransport>,
        progress: Arc<dyn UaiProgressTransport>,
        duration: Arc<dyn UaiDurationTransport>,
        question: Arc<dyn UaiQuestionTransport>,
        answer: Arc<dyn UaiAnswerTransport>,
    ) -> Self {
        Self {
            course,
            task,
            progress,
            duration,
            question,
            answer,
        }
    }
}

impl fmt::Debug for UaiDevelopmentTransports {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiDevelopmentTransports")
            .field("boundaries", &"configured")
            .finish()
    }
}

/// Composes the complete read-only Development entry around injected
/// authentication and inventory boundaries. Calling this function does not
/// register the Provider or claim native/live compatibility.
///
/// # Errors
///
/// Returns a sanitized Provider error if compile-time metadata is invalid.
pub fn build_development_provider(
    authentication_transport: Arc<dyn UaiAuthenticationTransport>,
    sessions: Arc<dyn UaiSessionResolver>,
    transports: UaiDevelopmentTransports,
) -> ProviderResult<ProviderEntry> {
    let authentication = Arc::new(UaiAuthentication::try_new(
        authentication_transport,
        sessions,
    )?);
    let course_inventory = Arc::new(UaiCourseInventory::try_new(transports.course)?);
    let task_inventory = Arc::new(UaiTaskInventory::try_new(transports.task)?);
    let task_detail = Arc::new(UaiTaskDetail::try_new(
        course_inventory.clone(),
        task_inventory.clone(),
    )?);
    let task_progress = Arc::new(UaiTaskProgress::try_new(transports.progress)?);
    let task_duration = Arc::new(UaiTaskDuration::try_new(transports.duration)?);
    let question_read = Arc::new(UaiQuestionRead::try_new(
        task_detail.clone(),
        transports.question,
    )?);
    let answer_resolve = Arc::new(UaiAnswerResolve::try_new(
        task_detail.clone(),
        transports.answer,
    )?);
    Ok(ProviderEntry {
        metadata: development_metadata()?,
        runtime_settings: ProviderRuntimeSettingsSchema::default(),
        authentication: Some(authentication),
        course_inventory: Some(course_inventory),
        task_inventory: Some(task_inventory),
        task_detail: Some(task_detail),
        task_progress: Some(task_progress),
        duration_read: Some(task_duration),
        question_inventory: Some(question_read.clone()),
        question_parse: Some(question_read),
        answer_resolve: Some(answer_resolve),
        submission_build: None,
        submission_execute: None,
        submission_verify: None,
        task_execution: None,
        browser_bridge: None,
    })
}

/// Composes the Development entry with native Course/Task/detail/progress/
/// duration/Question HTTP and an injected authentication exchange and
/// stored-session boundary.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or the shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_with_native_inventory(
    network: &ResolvedNetworkProfile,
    authentication_transport: Arc<dyn UaiAuthenticationTransport>,
    sessions: Arc<dyn UaiSessionResolver>,
) -> ProviderResult<ProviderEntry> {
    let inventory = Arc::new(NativeUaiInventoryTransport::try_new(
        network,
        sessions.clone(),
    )?);
    build_development_provider(
        authentication_transport,
        sessions,
        UaiDevelopmentTransports::new(
            inventory.clone(),
            inventory.clone(),
            inventory.clone(),
            inventory.clone(),
            inventory.clone(),
            inventory,
        ),
    )
}

/// Composes the fully native Development entry around one injected session
/// resolver. Calling this function does not register the Provider.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or either shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_native(
    network: &ResolvedNetworkProfile,
    sessions: Arc<dyn UaiSessionResolver>,
) -> ProviderResult<ProviderEntry> {
    let authentication = Arc::new(NativeUaiAuthenticationTransport::try_new(network)?);
    build_development_provider_with_native_inventory(network, authentication, sessions)
}

/// Composes the fully native Development entry around Core's scoped stored
/// credential resolver. This variant does not renew expired Password sessions.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or either shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_with_stored_session(
    network: &ResolvedNetworkProfile,
    credentials: Arc<dyn ProviderCredentialResolver>,
) -> ProviderResult<ProviderEntry> {
    build_development_provider_native(
        network,
        Arc::new(StoredUaiSessionResolver::new(credentials)),
    )
}

/// Composes the fully native Development entry with Password/Composite
/// automatic renewal through Core's scoped compare-and-replace boundary.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or either shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_with_renewal(
    network: &ResolvedNetworkProfile,
    resolver: Arc<dyn ProviderCredentialResolver>,
    renewer: Arc<dyn ProviderCredentialRenewer>,
) -> ProviderResult<ProviderEntry> {
    let authentication = Arc::new(NativeUaiAuthenticationTransport::try_new(network)?);
    let sessions = Arc::new(StoredUaiSessionResolver::with_renewal(
        resolver,
        renewer,
        authentication.clone(),
    ));
    build_development_provider_with_native_inventory(network, authentication, sessions)
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;
    use asterism_provider_api::{ProviderCapability, ProviderContext, ProviderRegistry};
    use asterism_secrets::{
        ProviderCredential, ProviderCredentialRenewal, ProviderCredentialResolution,
        ResolvedProviderCredential, SecretStoreError,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::UaiJwtSession;

    #[derive(Debug)]
    struct FixtureSessions;

    #[derive(Debug)]
    struct FixtureCredentials;

    #[async_trait]
    impl UaiSessionResolver for FixtureSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<UaiJwtSession> {
            UaiJwtSession::try_new("fixture-openid", "header.payload.signature")
        }
    }

    #[async_trait]
    impl ProviderCredentialResolver for FixtureCredentials {
        async fn resolve_provider_credentials(
            &self,
            _request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            Err(SecretStoreError::NotFound)
        }
    }

    #[async_trait]
    impl ProviderCredentialRenewer for FixtureCredentials {
        async fn renew_provider_credentials(
            &self,
            _request: ProviderCredentialRenewal,
        ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
            Err(SecretStoreError::NotFound)
        }
    }

    #[test]
    fn development_factory_builds_registry_consistent_entries() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");

        let native = build_development_provider_native(&network, Arc::new(FixtureSessions))
            .expect("native development entry");
        assert_registry_consistent(native);

        let stored =
            build_development_provider_with_stored_session(&network, Arc::new(FixtureCredentials))
                .expect("stored development entry");
        assert_registry_consistent(stored);

        let credentials = Arc::new(FixtureCredentials);
        let renewing =
            build_development_provider_with_renewal(&network, credentials.clone(), credentials)
                .expect("renewing development entry");
        assert_registry_consistent(renewing);
    }

    fn assert_registry_consistent(entry: ProviderEntry) {
        assert!(entry.authentication.is_some());
        assert!(entry.course_inventory.is_some());
        assert!(entry.task_inventory.is_some());
        assert!(entry.task_detail.is_some());
        assert!(entry.task_progress.is_some());
        assert!(entry.duration_read.is_some());
        assert!(entry.question_inventory.is_some());
        assert!(entry.question_parse.is_some());
        assert!(entry.answer_resolve.is_some());
        assert!(entry.task_execution.is_none());
        assert!(entry.browser_bridge.is_none());
        assert!(entry.runtime_settings.definitions.is_empty());
        for capability in [
            ProviderCapability::Authentication,
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
            ProviderCapability::TaskDetail,
            ProviderCapability::TaskProgressRead,
            ProviderCapability::DurationRead,
            ProviderCapability::QuestionInventory,
            ProviderCapability::QuestionParse,
            ProviderCapability::AnswerResolve,
        ] {
            assert!(entry.metadata.capabilities.contains(&capability));
        }

        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        assert!(
            registry
                .get(&asterism_domain::ProviderId::new("uai").unwrap())
                .is_some()
        );
    }
}
