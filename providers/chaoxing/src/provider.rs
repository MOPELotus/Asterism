use std::sync::Arc;

use asterism_networking::ResolvedNetworkProfile;
use asterism_provider_api::{ProviderEntry, ProviderResult};
use asterism_secrets::{ProviderCredentialRenewer, ProviderCredentialResolver};

use crate::{
    ChaoxingAuthentication, ChaoxingCourseInventory, ChaoxingQuestionRead,
    ChaoxingResourceExecution, ChaoxingSessionResolver, ChaoxingSubmissionBuild,
    ChaoxingSubmissionExecute, ChaoxingSubmissionVerify, ChaoxingTaskDetail, ChaoxingTaskInventory,
    ChaoxingTaskProgress, NativeChaoxingAuthenticationTransport, NativeChaoxingInventoryTransport,
    NativeChaoxingQrAuthenticationTransport, StoredChaoxingSessionResolver,
    metadata::development_metadata, runtime_settings::runtime_settings_schema,
};

/// Composes the complete development-level Chaoxing Provider around one Core
/// session boundary and one resolved network policy. Calling this function does
/// not register the Provider or raise its verification level.
///
/// # Errors
///
/// Returns a sanitized Provider error when metadata or either native HTTP
/// client cannot be initialized.
pub fn build_development_provider(
    network: &ResolvedNetworkProfile,
    sessions: Arc<dyn ChaoxingSessionResolver>,
) -> ProviderResult<ProviderEntry> {
    let authentication_transport =
        Arc::new(NativeChaoxingAuthenticationTransport::try_new(network)?);
    compose_development_provider(network, sessions, authentication_transport)
}

/// Composes the complete development Provider with Cookie-first automatic
/// renewal around Core's scoped runtime credential boundaries.
///
/// # Errors
///
/// Returns a sanitized Provider error when metadata or either native HTTP
/// client cannot be initialized.
pub fn build_development_provider_with_renewal(
    network: &ResolvedNetworkProfile,
    resolver: Arc<dyn ProviderCredentialResolver>,
    renewer: Arc<dyn ProviderCredentialRenewer>,
) -> ProviderResult<ProviderEntry> {
    let authentication_transport =
        Arc::new(NativeChaoxingAuthenticationTransport::try_new(network)?);
    let sessions = Arc::new(StoredChaoxingSessionResolver::with_renewal(
        resolver,
        renewer,
        authentication_transport.clone(),
    ));
    compose_development_provider(network, sessions, authentication_transport)
}

fn compose_development_provider(
    network: &ResolvedNetworkProfile,
    sessions: Arc<dyn ChaoxingSessionResolver>,
    authentication_transport: Arc<NativeChaoxingAuthenticationTransport>,
) -> ProviderResult<ProviderEntry> {
    let inventory_transport = Arc::new(NativeChaoxingInventoryTransport::try_new(
        network,
        sessions.clone(),
    )?);
    let qr_transport = Arc::new(NativeChaoxingQrAuthenticationTransport::try_new(network)?);
    let authentication = Arc::new(ChaoxingAuthentication::try_new_with_qr(
        authentication_transport,
        qr_transport,
        sessions,
    )?);
    let course_inventory = Arc::new(ChaoxingCourseInventory::try_new(
        inventory_transport.clone(),
    )?);
    let task_inventory = Arc::new(ChaoxingTaskInventory::try_new(inventory_transport.clone())?);
    let task_detail = Arc::new(ChaoxingTaskDetail::try_new(
        course_inventory.clone(),
        task_inventory.clone(),
    )?);
    let question_read = Arc::new(ChaoxingQuestionRead::try_new(
        course_inventory.clone(),
        inventory_transport.clone(),
        inventory_transport.clone(),
    )?);
    let task_execution = Arc::new(ChaoxingResourceExecution::try_new(
        course_inventory.clone(),
        inventory_transport.clone(),
        inventory_transport.clone(),
        inventory_transport.clone(),
        inventory_transport.clone(),
    )?);
    let task_progress = Arc::new(ChaoxingTaskProgress::try_new(
        task_detail.clone(),
        task_execution.clone(),
    )?);
    let submission_build = Arc::new(ChaoxingSubmissionBuild::try_new()?);
    let submission_execute = Arc::new(ChaoxingSubmissionExecute::try_new(
        course_inventory.clone(),
        inventory_transport.clone(),
        inventory_transport.clone(),
    )?);
    let submission_verify = Arc::new(ChaoxingSubmissionVerify::try_new(
        course_inventory.clone(),
        inventory_transport.clone(),
        inventory_transport,
    )?);
    Ok(ProviderEntry {
        metadata: development_metadata()?,
        runtime_settings: runtime_settings_schema(),
        authentication: Some(authentication),
        course_inventory: Some(course_inventory),
        task_inventory: Some(task_inventory),
        task_detail: Some(task_detail),
        task_progress: Some(task_progress),
        duration_read: None,
        question_inventory: Some(question_read.clone()),
        question_parse: Some(question_read),
        answer_resolve: None,
        submission_build: Some(submission_build),
        submission_execute: Some(submission_execute),
        submission_verify: Some(submission_verify),
        answer_history_harvest: None,
        task_execution: Some(task_execution),
        browser_bridge: None,
    })
}

#[cfg(test)]
mod tests {
    use asterism_domain::AuthMethod;
    use asterism_networking::NetworkProfile;
    use asterism_provider_api::{ProviderCapability, ProviderContext, ProviderRegistry};
    use asterism_secrets::{
        ProviderCredential, ProviderCredentialRenewal, ProviderCredentialResolution,
        ResolvedProviderCredential, SecretStoreError,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::ChaoxingCookieSession;

    #[derive(Debug)]
    struct FixtureSessions;

    #[derive(Debug)]
    struct FixtureCredentials;

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

    #[async_trait]
    impl ChaoxingSessionResolver for FixtureSessions {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<ChaoxingCookieSession> {
            ChaoxingCookieSession::try_new("_uid=SAFE_UID; uf=SAFE_UF")
        }
    }

    #[test]
    fn development_factory_builds_one_registry_consistent_entry() {
        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let entry = build_development_provider(&network, Arc::new(FixtureSessions)).unwrap();
        assert!(
            entry
                .authentication
                .as_ref()
                .unwrap()
                .supports_durable_interactive_authentication()
        );
        assert_registry_consistent(entry);
        let credentials = Arc::new(FixtureCredentials);
        let renewing =
            build_development_provider_with_renewal(&network, credentials.clone(), credentials)
                .unwrap();
        assert_registry_consistent(renewing);
    }

    fn assert_registry_consistent(entry: ProviderEntry) {
        assert!(entry.metadata.auth_methods.contains(&AuthMethod::QrCode));
        assert!(entry.authentication.is_some());
        assert!(entry.course_inventory.is_some());
        assert!(entry.task_inventory.is_some());
        assert!(entry.task_progress.is_some());
        assert!(entry.question_inventory.is_some());
        assert!(entry.question_parse.is_some());
        assert!(entry.submission_build.is_some());
        assert!(entry.task_execution.is_some());
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::Authentication)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::ResourceExecution)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::TaskProgressRead)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::TaskDetail)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::QuestionInventory)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::QuestionParse)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::SubmissionBuild)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::SubmissionExecute)
        );
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::SubmissionVerify)
        );
        assert!(entry.submission_execute.is_some());
        assert!(entry.submission_verify.is_some());
        assert!(entry.answer_history_harvest.is_none());
        assert!(
            !entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::AnswerHistoryHarvest)
        );
        assert!(entry.task_detail.is_some());
        assert_eq!(entry.runtime_settings.version, 5);
        assert_eq!(entry.runtime_settings.definitions.len(), 6);
        assert!(
            entry
                .runtime_settings
                .definitions
                .iter()
                .any(|definition| definition.key == "video.playback_rate")
        );
        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        assert!(
            registry
                .get(&asterism_domain::ProviderId::new("chaoxing").unwrap())
                .is_some()
        );
    }
}
