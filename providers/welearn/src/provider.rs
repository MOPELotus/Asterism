use std::sync::Arc;

use asterism_networking::ResolvedNetworkProfile;
use asterism_provider_api::{ProviderEntry, ProviderResult, ProviderRuntimeSettingsSchema};
use asterism_secrets::{ProviderCredentialRenewer, ProviderCredentialResolver};

use crate::{
    WellearnAuthentication, WellearnAuthenticationTransport, WellearnCourseInventory,
    WellearnCourseInventoryTransport, WellearnSessionResolver, WellearnTaskInventory,
    WellearnTaskInventoryTransport, metadata::development_metadata,
    native_authentication::NativeWellearnAuthenticationTransport,
    native_http::NativeWellearnInventoryTransport, stored_session::StoredWellearnSessionResolver,
};

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
) -> ProviderResult<ProviderEntry> {
    let authentication = Arc::new(WellearnAuthentication::try_new(
        authentication_transport,
        sessions,
    )?);
    let course_inventory = Arc::new(WellearnCourseInventory::try_new(course_transport)?);
    let task_inventory = Arc::new(WellearnTaskInventory::try_new(task_transport)?);
    Ok(ProviderEntry {
        metadata: development_metadata()?,
        runtime_settings: ProviderRuntimeSettingsSchema::default(),
        authentication: Some(authentication),
        course_inventory: Some(course_inventory),
        task_inventory: Some(task_inventory),
        task_detail: None,
        task_progress: None,
        question_inventory: None,
        question_parse: None,
        answer_resolve: None,
        submission_build: None,
        submission_execute: None,
        submission_verify: None,
        task_execution: None,
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
    build_development_provider(
        authentication_transport,
        sessions,
        inventory.clone(),
        inventory,
    )
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
        ProviderContext, ProviderError, ProviderErrorKind, ProviderRegistry, RemoteCourse,
    };
    use asterism_secrets::{
        ProviderCredential, ProviderCredentialRenewal, ProviderCredentialRenewer,
        ProviderCredentialResolution, ProviderCredentialResolver, ResolvedProviderCredential,
        SecretStoreError, SecretString,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::{WellearnCookieSession, WellearnInventoryDocument, WellearnTaskInventoryDocuments};

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

    #[test]
    fn development_factory_is_registry_consistent_without_extra_slots() {
        let boundaries = Arc::new(UnusedBoundaries);
        let entry = build_development_provider(
            boundaries.clone(),
            boundaries.clone(),
            boundaries.clone(),
            boundaries,
        )
        .unwrap();
        assert!(entry.authentication.is_some());
        assert!(entry.course_inventory.is_some());
        assert!(entry.task_inventory.is_some());
        assert!(entry.task_progress.is_none());
        assert!(entry.task_execution.is_none());
        assert!(entry.browser_bridge.is_none());
        assert!(entry.runtime_settings.definitions.is_empty());

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
