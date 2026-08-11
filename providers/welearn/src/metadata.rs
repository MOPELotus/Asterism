use std::collections::BTreeSet;

use asterism_domain::{AuthMethod, ProviderId, SessionKind};
use asterism_provider_api::{
    ProviderCapability, ProviderError, ProviderErrorKind, ProviderMetadata, ProviderResult,
    VerificationLevel,
};

pub(crate) const PROVIDER_ID: &str = "welearn";

/// Returns the compile-time metadata for the development crate.
///
/// Authentication, Course inventory and Task inventory are available behind
/// injected transport boundaries.
///
/// # Errors
///
/// Returns an internal error if the compile-time Provider ID is invalid.
pub fn development_metadata() -> ProviderResult<ProviderMetadata> {
    Ok(ProviderMetadata {
        id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn compile-time Provider ID is invalid",
            )
        })?,
        display_name: "WELearn".to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        verification: VerificationLevel::Development,
        scan_min_interval_seconds: None,
        capture_recipe_version: None,
        capabilities: BTreeSet::from([
            ProviderCapability::Authentication,
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
            ProviderCapability::TaskDetail,
            ProviderCapability::TaskProgressRead,
        ]),
        auth_methods: BTreeSet::from([AuthMethod::Password, AuthMethod::ImportedCookie]),
        session_kinds: BTreeSet::from([
            SessionKind::Cookie,
            SessionKind::Composite,
            SessionKind::ProviderSpecific,
        ]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_metadata_advertises_only_injected_read_and_auth_boundaries() {
        let metadata = development_metadata().unwrap();
        assert_eq!(metadata.id.as_str(), PROVIDER_ID);
        assert_eq!(metadata.verification, VerificationLevel::Development);
        assert_eq!(
            metadata.capabilities,
            BTreeSet::from([
                ProviderCapability::Authentication,
                ProviderCapability::CourseInventory,
                ProviderCapability::TaskInventory,
                ProviderCapability::TaskDetail,
                ProviderCapability::TaskProgressRead,
            ])
        );
        assert_eq!(
            metadata.auth_methods,
            BTreeSet::from([AuthMethod::Password, AuthMethod::ImportedCookie])
        );
    }
}
