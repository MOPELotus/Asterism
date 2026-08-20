use std::sync::Arc;

use asterism_networking::ResolvedNetworkProfile;
use asterism_provider_api::{ProviderEntry, ProviderResult};
use asterism_secrets::{ProviderCredentialRenewer, ProviderCredentialResolver};

use crate::{
    WellearnAtomicDurationCompletion, WellearnAtomicDurationCompletionRecovery,
    WellearnAtomicDurationCompletionRecoveryTransport, WellearnAtomicDurationCompletionTransport,
    WellearnAuthentication, WellearnAuthenticationTransport, WellearnBatchExecutionPlanner,
    WellearnCmiTransport, WellearnCourseInventory, WellearnCourseInventoryTransport,
    WellearnDurationRead, WellearnDurationReport, WellearnDurationReportTransport,
    WellearnResourceExecution, WellearnResourceExecutionTransport, WellearnSessionResolver,
    WellearnTaskDetail, WellearnTaskExecution, WellearnTaskInventory,
    WellearnTaskInventoryTransport, WellearnTaskProgress, metadata::development_metadata,
    native_authentication::NativeWellearnAuthenticationTransport,
    native_http::NativeWellearnInventoryTransport, runtime_settings::runtime_settings_schema,
    stored_session::StoredWellearnSessionResolver,
};

/// Native, unregistered boundaries for Core-owned atomic batch dispatch and
/// verification-only recovery.
pub struct WellearnAtomicDurationCompletionRuntime {
    execution: Arc<WellearnAtomicDurationCompletion>,
    recovery: Arc<WellearnAtomicDurationCompletionRecovery>,
}

impl WellearnAtomicDurationCompletionRuntime {
    pub const fn execution(&self) -> &Arc<WellearnAtomicDurationCompletion> {
        &self.execution
    }

    pub const fn recovery(&self) -> &Arc<WellearnAtomicDurationCompletionRecovery> {
        &self.recovery
    }
}

impl std::fmt::Debug for WellearnAtomicDurationCompletionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WellearnAtomicDurationCompletionRuntime")
            .field("execution", &"configured")
            .field("recovery", &"configured")
            .finish()
    }
}

/// Composes the complete development entry around injected authentication and
/// inventory boundaries. Calling this function does not register the Provider
/// or claim native/live compatibility.
///
/// # Errors
///
/// Returns a sanitized Provider error if compile-time metadata is invalid.
pub fn build_development_provider(
    authentication_transport: Arc<dyn WellearnAuthenticationTransport>,
    sessions: Arc<dyn WellearnSessionResolver>,
    course_transport: Arc<dyn WellearnCourseInventoryTransport>,
    task_transport: Arc<dyn WellearnTaskInventoryTransport>,
    cmi_transport: Arc<dyn WellearnCmiTransport>,
    resource_transport: Arc<dyn WellearnResourceExecutionTransport>,
    duration_transport: Arc<dyn WellearnDurationReportTransport>,
) -> ProviderResult<ProviderEntry> {
    build_development_provider_inner(
        authentication_transport,
        sessions,
        course_transport,
        task_transport,
        cmi_transport,
        resource_transport,
        duration_transport,
        None,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the development composition keeps each audited WELearn boundary injectable"
)]
fn build_development_provider_inner(
    authentication_transport: Arc<dyn WellearnAuthenticationTransport>,
    sessions: Arc<dyn WellearnSessionResolver>,
    course_transport: Arc<dyn WellearnCourseInventoryTransport>,
    task_transport: Arc<dyn WellearnTaskInventoryTransport>,
    cmi_transport: Arc<dyn WellearnCmiTransport>,
    resource_transport: Arc<dyn WellearnResourceExecutionTransport>,
    duration_transport: Arc<dyn WellearnDurationReportTransport>,
    atomic_transports: Option<(
        Arc<dyn WellearnAtomicDurationCompletionTransport>,
        Arc<dyn WellearnAtomicDurationCompletionRecoveryTransport>,
    )>,
) -> ProviderResult<ProviderEntry> {
    let authentication = Arc::new(WellearnAuthentication::try_new(
        authentication_transport,
        sessions,
    )?);
    let course_inventory = Arc::new(WellearnCourseInventory::try_new(course_transport)?);
    let task_inventory = Arc::new(WellearnTaskInventory::try_new(task_transport)?);
    let task_detail = Arc::new(WellearnTaskDetail::try_new(
        course_inventory.clone(),
        task_inventory.clone(),
    )?);
    let task_progress = Arc::new(WellearnTaskProgress::try_new(cmi_transport.clone())?);
    let duration_read = Arc::new(WellearnDurationRead::try_new(cmi_transport)?);
    let resource_execution = Arc::new(WellearnResourceExecution::try_new(
        task_detail.clone(),
        resource_transport,
    )?);
    let duration_report = Arc::new(WellearnDurationReport::try_new(
        task_detail.clone(),
        duration_transport,
    )?);
    let batch_planner = Arc::new(WellearnBatchExecutionPlanner::try_new(
        course_inventory.clone(),
        task_inventory.clone(),
    )?);
    let task_execution = Arc::new(match atomic_transports {
        Some((atomic_transport, atomic_recovery_transport)) => {
            let atomic_execution = Arc::new(WellearnAtomicDurationCompletion::try_new(
                task_detail.clone(),
                atomic_transport,
            )?);
            let atomic_recovery = Arc::new(WellearnAtomicDurationCompletionRecovery::try_new(
                task_detail.clone(),
                atomic_recovery_transport,
            )?);
            WellearnTaskExecution::try_new_with_batch_runtime(
                resource_execution,
                duration_report,
                batch_planner,
                atomic_execution,
                atomic_recovery,
            )?
        }
        None => WellearnTaskExecution::try_new_with_batch_planner(
            resource_execution,
            duration_report,
            batch_planner,
        )?,
    });
    Ok(ProviderEntry {
        metadata: development_metadata()?,
        runtime_settings: runtime_settings_schema(),
        authentication: Some(authentication),
        course_inventory: Some(course_inventory),
        course_enrollment: None,
        task_inventory: Some(task_inventory),
        task_detail: Some(task_detail),
        task_progress: Some(task_progress),
        duration_read: Some(duration_read),
        question_inventory: None,
        question_parse: None,
        answer_resolve: None,
        submission_build: None,
        submission_execute: None,
        submission_verify: None,
        answer_history_harvest: None,
        task_execution: Some(task_execution),
        browser_bridge: None,
    })
}

/// Composes the Development entry with native Course/Task HTTP and an injected
/// authentication exchange. Calling this function does not register it.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or the shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_with_native_inventory(
    network: &ResolvedNetworkProfile,
    authentication_transport: Arc<dyn WellearnAuthenticationTransport>,
    sessions: Arc<dyn WellearnSessionResolver>,
) -> ProviderResult<ProviderEntry> {
    let inventory = Arc::new(NativeWellearnInventoryTransport::try_new(
        network,
        sessions.clone(),
    )?);
    build_development_provider_inner(
        authentication_transport,
        sessions,
        inventory.clone(),
        inventory.clone(),
        inventory.clone(),
        inventory.clone(),
        inventory.clone(),
        Some((inventory.clone(), inventory)),
    )
}

/// Composes the unregistered native atomic execution and read-only recovery
/// boundaries around one shared HTTP transport and one fresh `TaskDetail` path.
///
/// This function does not add a Provider registry capability. Core must still
/// own parent/child creation, durable attempt record loading and dispatch.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or the shared HTTP client
/// cannot be initialized.
pub fn build_native_atomic_duration_completion_runtime(
    network: &ResolvedNetworkProfile,
    sessions: Arc<dyn WellearnSessionResolver>,
) -> ProviderResult<WellearnAtomicDurationCompletionRuntime> {
    let inventory = Arc::new(NativeWellearnInventoryTransport::try_new(
        network, sessions,
    )?);
    let course_inventory = Arc::new(WellearnCourseInventory::try_new(inventory.clone())?);
    let task_inventory = Arc::new(WellearnTaskInventory::try_new(inventory.clone())?);
    let task_detail = Arc::new(WellearnTaskDetail::try_new(
        course_inventory,
        task_inventory,
    )?);
    let execution = Arc::new(WellearnAtomicDurationCompletion::try_new(
        task_detail.clone(),
        inventory.clone(),
    )?);
    let recovery = Arc::new(WellearnAtomicDurationCompletionRecovery::try_new(
        task_detail,
        inventory,
    )?);
    Ok(WellearnAtomicDurationCompletionRuntime {
        execution,
        recovery,
    })
}

/// Composes the Development entry with native Password/OIDC, Cookie validation
/// and Course/Task HTTP around one injected stored-session resolver. Calling
/// this function does not register the Provider or raise its verification level.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or either shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_native(
    network: &ResolvedNetworkProfile,
    sessions: Arc<dyn WellearnSessionResolver>,
) -> ProviderResult<ProviderEntry> {
    let authentication = Arc::new(NativeWellearnAuthenticationTransport::try_new(network)?);
    build_development_provider_with_native_inventory(network, authentication, sessions)
}

/// Composes the fully native Development entry around Core's scoped stored
/// credential resolver. This does not add automatic session renewal.
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
        Arc::new(StoredWellearnSessionResolver::new(credentials)),
    )
}

/// Composes the fully native Development entry with Cookie-first automatic
/// renewal through Core's scoped compare-and-replace credential boundary.
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
    let authentication = Arc::new(NativeWellearnAuthenticationTransport::try_new(network)?);
    let sessions = Arc::new(StoredWellearnSessionResolver::with_renewal(
        resolver,
        renewer,
        authentication.clone(),
    ));
    build_development_provider_with_native_inventory(network, authentication, sessions)
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;
    use asterism_provider_api::{
        ExecutionEventSink, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
        ProviderRegistry, RemoteCourse,
    };
    use asterism_secrets::{
        ProviderCredential, ProviderCredentialRenewal, ProviderCredentialRenewer,
        ProviderCredentialResolution, ProviderCredentialResolver, ResolvedProviderCredential,
        SecretStoreError, SecretString,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::{
        WellearnCmiDocument, WellearnCookieSession, WellearnDurationReportDocuments,
        WellearnInventoryDocument, WellearnResourceExecutionDocuments,
        WellearnTaskInventoryDocuments,
    };

    #[derive(Debug)]
    struct UnusedBoundaries;

    #[derive(Debug)]
    struct UnusedCredentials;

    #[async_trait]
    impl ProviderCredentialResolver for UnusedCredentials {
        async fn resolve_provider_credentials(
            &self,
            _request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            Err(SecretStoreError::NotFound)
        }
    }

    #[async_trait]
    impl ProviderCredentialRenewer for UnusedCredentials {
        async fn renew_provider_credentials(
            &self,
            _request: ProviderCredentialRenewal,
        ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
            Err(SecretStoreError::NotFound)
        }
    }

    #[async_trait]
    impl WellearnAuthenticationTransport for UnusedBoundaries {
        async fn exchange_password(
            &self,
            _username: &SecretString,
            _password: &SecretString,
        ) -> ProviderResult<WellearnCookieSession> {
            Err(unused())
        }

        async fn validate_cookie(&self, _session: &WellearnCookieSession) -> ProviderResult<()> {
            Err(unused())
        }
    }

    #[async_trait]
    impl WellearnSessionResolver for UnusedBoundaries {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnCookieSession> {
            Err(unused())
        }
    }

    #[async_trait]
    impl WellearnCourseInventoryTransport for UnusedBoundaries {
        async fn fetch_courses(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<WellearnInventoryDocument> {
            Err(unused())
        }
    }

    #[async_trait]
    impl WellearnTaskInventoryTransport for UnusedBoundaries {
        async fn fetch_tasks(
            &self,
            _context: &ProviderContext,
            _course: &RemoteCourse,
        ) -> ProviderResult<WellearnTaskInventoryDocuments> {
            Err(unused())
        }
    }

    #[async_trait]
    impl WellearnCmiTransport for UnusedBoundaries {
        async fn fetch_cmi(
            &self,
            _context: &ProviderContext,
            _course_id: &str,
            _sco_id: &str,
        ) -> ProviderResult<WellearnCmiDocument> {
            Err(unused())
        }
    }

    #[async_trait]
    impl WellearnDurationReportTransport for UnusedBoundaries {
        async fn report_duration(
            &self,
            _context: &ProviderContext,
            _course_id: &str,
            _sco_id: &str,
            _binding: &crate::WellearnDurationReportBinding,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<WellearnDurationReportDocuments> {
            Err(unused())
        }
    }

    #[async_trait]
    impl WellearnResourceExecutionTransport for UnusedBoundaries {
        async fn complete_resource(
            &self,
            _context: &ProviderContext,
            _course_id: &str,
            _sco_id: &str,
            _binding: &crate::WellearnResourceExecutionBinding,
            _events: &(dyn asterism_provider_api::ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<WellearnResourceExecutionDocuments> {
            Err(unused())
        }

        async fn verify_resource(
            &self,
            _context: &ProviderContext,
            _course_id: &str,
            _sco_id: &str,
            _mutation_profile: crate::WellearnResourceMutationProfile,
        ) -> ProviderResult<WellearnCmiDocument> {
            Err(unused())
        }
    }

    #[test]
    fn development_factory_is_registry_consistent_without_extra_slots() {
        let boundaries = Arc::new(UnusedBoundaries);
        let entry = build_development_provider(
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
            boundaries,
        )
        .unwrap();
        assert!(entry.authentication.is_some());
        assert!(entry.course_inventory.is_some());
        assert!(entry.task_inventory.is_some());
        assert!(entry.task_detail.is_some());
        assert!(entry.task_progress.is_some());
        assert!(entry.duration_read.is_some());
        assert!(entry.task_execution.is_some());
        assert!(entry.browser_bridge.is_none());
        assert_eq!(entry.runtime_settings.version, 16);
        assert_eq!(entry.runtime_settings.definitions.len(), 18);

        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        assert!(
            registry
                .get(&asterism_domain::ProviderId::new("welearn").unwrap())
                .is_some()
        );

        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let boundaries = Arc::new(UnusedBoundaries);
        let native = build_development_provider_with_native_inventory(
            &network,
            boundaries.clone(),
            boundaries,
        )
        .unwrap();
        let mut registry = ProviderRegistry::default();
        registry.register(native).unwrap();

        let atomic_runtime =
            build_native_atomic_duration_completion_runtime(&network, Arc::new(UnusedBoundaries))
                .unwrap();
        assert_eq!(atomic_runtime.execution().metadata().id.as_str(), "welearn");
        assert_eq!(atomic_runtime.recovery().metadata().id.as_str(), "welearn");
        assert_eq!(
            format!("{atomic_runtime:?}"),
            "WellearnAtomicDurationCompletionRuntime { execution: \"configured\", recovery: \"configured\" }"
        );

        let native =
            build_development_provider_native(&network, Arc::new(UnusedBoundaries)).unwrap();
        let mut registry = ProviderRegistry::default();
        registry.register(native).unwrap();

        let stored =
            build_development_provider_with_stored_session(&network, Arc::new(UnusedCredentials))
                .unwrap();
        let mut registry = ProviderRegistry::default();
        registry.register(stored).unwrap();

        let credentials = Arc::new(UnusedCredentials);
        let renewing =
            build_development_provider_with_renewal(&network, credentials.clone(), credentials)
                .unwrap();
        let mut registry = ProviderRegistry::default();
        registry.register(renewing).unwrap();
    }

    fn unused() -> ProviderError {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "fixture boundary must not be called",
        )
    }
}
