use std::sync::Arc;

use asterism_networking::ResolvedNetworkProfile;
use asterism_provider_api::{ProviderEntry, ProviderResult};

use crate::{
    ChaoxingAuthentication, ChaoxingCourseInventory, ChaoxingSessionResolver,
    ChaoxingTaskInventory, NativeChaoxingAuthenticationTransport, NativeChaoxingInventoryTransport,
    metadata::development_metadata,
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
    let inventory_transport = Arc::new(NativeChaoxingInventoryTransport::try_new(
        network,
        sessions.clone(),
    )?);
    let authentication = Arc::new(ChaoxingAuthentication::try_new(
        authentication_transport,
        sessions,
    )?);
    let course_inventory = Arc::new(ChaoxingCourseInventory::try_new(
        inventory_transport.clone(),
    )?);
    let task_inventory = Arc::new(ChaoxingTaskInventory::try_new(inventory_transport)?);
    Ok(ProviderEntry {
        metadata: development_metadata()?,
        authentication: Some(authentication),
        course_inventory: Some(course_inventory),
        task_inventory: Some(task_inventory),
        task_detail: None,
        task_progress: None,
        task_execution: None,
        browser_bridge: None,
    })
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;
    use asterism_provider_api::{ProviderCapability, ProviderContext, ProviderRegistry};
    use async_trait::async_trait;

    use super::*;
    use crate::ChaoxingCookieSession;

    #[derive(Debug)]
    struct FixtureSessions;

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
        assert!(entry.authentication.is_some());
        assert!(entry.course_inventory.is_some());
        assert!(entry.task_inventory.is_some());
        assert!(
            entry
                .metadata
                .capabilities
                .contains(&ProviderCapability::Authentication)
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
