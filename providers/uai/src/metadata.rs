use std::collections::BTreeSet;

use asterism_domain::{AuthMethod, ProviderId, SessionKind};
use asterism_provider_api::{
    ProviderCapability, ProviderError, ProviderErrorKind, ProviderMetadata, ProviderResult,
    VerificationLevel,
};

pub(crate) const PROVIDER_ID: &str = "uai";

/// Returns metadata for the current read-only Development checkpoint.
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
        capabilities: BTreeSet::from([
            ProviderCapability::Authentication,
            ProviderCapability::CourseInventory,
            ProviderCapability::TaskInventory,
            ProviderCapability::TaskDetail,
            ProviderCapability::TaskProgressRead,
            ProviderCapability::DurationRead,
            ProviderCapability::QuestionInventory,
            ProviderCapability::QuestionParse,
            ProviderCapability::AnswerResolve,
            ProviderCapability::SubmissionBuild,
        ]),
        auth_methods: BTreeSet::from([AuthMethod::Password, AuthMethod::ImportedToken]),
        session_kinds: BTreeSet::from([SessionKind::Jwt, SessionKind::Composite]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_advertises_all_read_only_development_capabilities() {
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
                ProviderCapability::DurationRead,
                ProviderCapability::QuestionInventory,
                ProviderCapability::QuestionParse,
                ProviderCapability::AnswerResolve,
                ProviderCapability::SubmissionBuild,
            ])
        );
        assert_eq!(
            metadata.auth_methods,
            BTreeSet::from([AuthMethod::Password, AuthMethod::ImportedToken])
        );
        assert_eq!(
            metadata.session_kinds,
            BTreeSet::from([SessionKind::Jwt, SessionKind::Composite])
        );
    }
}
