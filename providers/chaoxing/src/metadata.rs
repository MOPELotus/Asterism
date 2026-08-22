use std::collections::BTreeSet;

use asterism_domain::{AuthMethod, ProviderId, SessionKind};
use asterism_provider_api::{
    ProviderCapability, ProviderError, ProviderErrorKind, ProviderMetadata, ProviderResult,
    VerificationLevel,
};

pub(crate) const PROVIDER_ID: &str = "chaoxing";

pub(crate) fn development_metadata() -> ProviderResult<ProviderMetadata> {
    Ok(ProviderMetadata {
        id: ProviderId::new(PROVIDER_ID).map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing compile-time Provider ID is invalid",
            )
        })?,
        display_name: "Chaoxing".to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        verification: VerificationLevel::Development,
        scan_min_interval_seconds: None,
        capture_recipe_version: Some(1),
        capabilities: BTreeSet::from([
            ProviderCapability::Authentication,
            ProviderCapability::CourseInventory,
            ProviderCapability::CourseEnrollment,
            ProviderCapability::TaskInventory,
            ProviderCapability::TaskDetail,
            ProviderCapability::TaskProgressRead,
            ProviderCapability::QuestionInventory,
            ProviderCapability::QuestionParse,
            ProviderCapability::AnswerResolve,
            ProviderCapability::AnswerHistoryHarvest,
            ProviderCapability::SubmissionBuild,
            ProviderCapability::SubmissionExecute,
            ProviderCapability::SubmissionVerify,
            ProviderCapability::ResourceExecution,
            ProviderCapability::ExecutionVerify,
            ProviderCapability::BrowserBridge,
        ]),
        auth_methods: BTreeSet::from([
            AuthMethod::Password,
            AuthMethod::ImportedCookie,
            AuthMethod::QrCode,
            AuthMethod::AssistedSession,
        ]),
        session_kinds: BTreeSet::from([
            SessionKind::Cookie,
            SessionKind::Composite,
            SessionKind::ProviderSpecific,
        ]),
    })
}
