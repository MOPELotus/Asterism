use std::collections::BTreeSet;

use asterism_domain::{AuthMethod, ProviderId, SessionKind};
use asterism_provider_api::{
    ProviderCapability, ProviderError, ProviderErrorKind, ProviderMetadata, ProviderResult,
    VerificationLevel,
};

pub(crate) const PROVIDER_ID: &str = "cidaren";

/// Returns compile-time metadata for the Development-only Provider.
///
/// # Errors
///
/// Returns an internal error if the canonical Provider ID is invalid.
pub fn development_metadata() -> ProviderResult<ProviderMetadata> {
    Ok(ProviderMetadata {
        id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Cidaren compile-time Provider ID is invalid",
            )
        })?,
        display_name: "Cidaren".to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        verification: VerificationLevel::Development,
        scan_min_interval_seconds: None,
        capture_recipe_version: Some(2),
        capabilities: BTreeSet::from([
            ProviderCapability::Authentication,
            ProviderCapability::BrowserBridge,
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
            ProviderCapability::TaskDetail,
            ProviderCapability::TaskProgressRead,
            ProviderCapability::DurationRead,
            ProviderCapability::SubmissionBuild,
        ]),
        auth_methods: BTreeSet::from([
            AuthMethod::ImportedToken,
            AuthMethod::AssistedSession,
            AuthMethod::ExternalBrowserOauth,
        ]),
        session_kinds: BTreeSet::from([SessionKind::ProviderSpecific, SessionKind::Composite]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_metadata_advertises_manual_capture_and_external_oauth() {
        let metadata = development_metadata().unwrap();
        assert_eq!(metadata.id.as_str(), PROVIDER_ID);
        assert_eq!(metadata.verification, VerificationLevel::Development);
        assert_eq!(
            metadata.capabilities,
            BTreeSet::from([
                ProviderCapability::Authentication,
                ProviderCapability::BrowserBridge,
                ProviderCapability::CourseInventory,
                ProviderCapability::TaskInventory,
                ProviderCapability::TaskDetail,
                ProviderCapability::TaskProgressRead,
                ProviderCapability::DurationRead,
                ProviderCapability::SubmissionBuild,
            ])
        );
        assert_eq!(
            metadata.auth_methods,
            BTreeSet::from([
                AuthMethod::ImportedToken,
                AuthMethod::AssistedSession,
                AuthMethod::ExternalBrowserOauth,
            ])
        );
        assert_eq!(
            metadata.session_kinds,
            BTreeSet::from([SessionKind::ProviderSpecific, SessionKind::Composite,])
        );
        assert_eq!(metadata.capture_recipe_version, Some(2));
    }
}
