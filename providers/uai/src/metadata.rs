use std::collections::BTreeSet;

use asterism_domain::ProviderId;
use asterism_provider_api::{
    ProviderError, ProviderErrorKind, ProviderMetadata, ProviderResult, VerificationLevel,
};

pub(crate) const PROVIDER_ID: &str = "uai";

/// Returns metadata for the parser-only Development checkpoint.
///
/// # Errors
///
/// Returns an internal error if the compile-time Provider ID is invalid.
pub fn development_metadata() -> ProviderResult<ProviderMetadata> {
    Ok(ProviderMetadata {
        id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "UAI compile-time Provider ID is invalid",
            )
        })?,
        display_name: "UAI".to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        verification: VerificationLevel::Development,
        scan_min_interval_seconds: None,
        capture_recipe_version: None,
        capabilities: BTreeSet::new(),
        auth_methods: BTreeSet::new(),
        session_kinds: BTreeSet::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_only_metadata_advertises_no_runtime_boundary() {
        let metadata = development_metadata().unwrap();
        assert_eq!(metadata.id.as_str(), PROVIDER_ID);
        assert_eq!(metadata.verification, VerificationLevel::Development);
        assert!(metadata.capabilities.is_empty());
        assert!(metadata.auth_methods.is_empty());
        assert!(metadata.session_kinds.is_empty());
    }
}
