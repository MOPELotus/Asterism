use std::sync::Arc;

use asterism_networking::ResolvedNetworkProfile;
use asterism_provider_api::{ProviderEntry, ProviderResult};
use asterism_secrets::ProviderCredentialResolver;

use crate::{
    CidarenAnswerEvidenceTransport, CidarenAnswerResolve, CidarenAssessmentTransport,
    CidarenAuthentication, CidarenAuthenticationTransport, CidarenBrowserBridge,
    CidarenClassTaskTransport, CidarenCourseInventory, CidarenDurationRead,
    CidarenQuestionInventory, CidarenSessionResolver, CidarenStudyTaskTransport,
    CidarenSubmissionBuild, CidarenSubmissionExecute, CidarenSubmissionVerify, CidarenTaskDetail,
    CidarenTaskInventory, CidarenTaskProgress, CidarenTaskScoreTransport,
    metadata::development_metadata, native_http::NativeCidarenTransport,
    runtime_settings::runtime_settings_schema, stored_session::StoredCidarenSessionResolver,
};

/// Composes the Development entry around injected token/session and complete
/// class-task and ordinary study-task boundaries. Calling this function does
/// not register the Provider or claim live compatibility.
///
/// # Errors
///
/// Returns a sanitized Provider error if compile-time metadata is invalid.
pub fn build_development_provider(
    authentication_transport: Arc<dyn CidarenAuthenticationTransport>,
    answer_evidence_transport: Arc<dyn CidarenAnswerEvidenceTransport>,
    assessment_transport: Arc<dyn CidarenAssessmentTransport>,
    score_transport: Arc<dyn CidarenTaskScoreTransport>,
    sessions: Arc<dyn CidarenSessionResolver>,
    class_tasks: Arc<dyn CidarenClassTaskTransport>,
    study_tasks: Arc<dyn CidarenStudyTaskTransport>,
) -> ProviderResult<ProviderEntry> {
    let task_detail = Arc::new(CidarenTaskDetail::try_new(
        class_tasks.clone(),
        study_tasks.clone(),
    )?);
    let task_progress = Arc::new(CidarenTaskProgress::try_new(
        class_tasks.clone(),
        study_tasks.clone(),
    )?);
    let answer_resolve = Arc::new(CidarenAnswerResolve::try_new(
        task_detail.clone(),
        answer_evidence_transport.clone(),
    )?);
    let question_inventory = Arc::new(CidarenQuestionInventory::try_new(
        task_detail.clone(),
        answer_evidence_transport,
        assessment_transport,
    )?);
    let submission_execute = Arc::new(CidarenSubmissionExecute::try_new(
        question_inventory.clone(),
    )?);
    let submission_verify = Arc::new(CidarenSubmissionVerify::try_new(
        task_detail.clone(),
        score_transport,
    )?);
    Ok(ProviderEntry {
        metadata: development_metadata()?,
        runtime_settings: runtime_settings_schema(),
        authentication: Some(Arc::new(CidarenAuthentication::try_new(
            authentication_transport,
            sessions,
        )?)),
        course_inventory: Some(Arc::new(CidarenCourseInventory::try_new(
            class_tasks.clone(),
            study_tasks.clone(),
        )?)),
        course_enrollment: None,
        task_inventory: Some(Arc::new(CidarenTaskInventory::try_new(
            class_tasks,
            study_tasks,
        )?)),
        task_detail: Some(task_detail.clone()),
        task_progress: Some(task_progress),
        duration_read: Some(Arc::new(CidarenDurationRead::try_new(task_detail.clone())?)),
        question_inventory: Some(question_inventory),
        question_parse: None,
        answer_resolve: Some(answer_resolve),
        submission_build: Some(Arc::new(CidarenSubmissionBuild::try_new()?)),
        submission_execute: Some(submission_execute),
        submission_verify: Some(submission_verify),
        answer_history_harvest: None,
        task_execution: None,
        browser_bridge: Some(Arc::new(CidarenBrowserBridge::try_new(task_detail)?)),
    })
}

/// Composes the Development entry with native account validation, complete
/// class-task pagination and selected-Course study units around one injected
/// session resolver.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or the shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_native(
    network: &ResolvedNetworkProfile,
    sessions: Arc<dyn CidarenSessionResolver>,
) -> ProviderResult<ProviderEntry> {
    let native = Arc::new(NativeCidarenTransport::try_new(network, sessions.clone())?);
    build_development_provider(
        native.clone(),
        native.clone(),
        native.clone(),
        native.clone(),
        sessions,
        native.clone(),
        native,
    )
}

/// Composes the native Development entry around Core's provider-scoped stored
/// credential resolver. Manual tokens cannot renew automatically.
///
/// # Errors
///
/// Returns a sanitized Provider error if metadata or the shared HTTP client
/// cannot be initialized.
pub fn build_development_provider_with_stored_session(
    network: &ResolvedNetworkProfile,
    credentials: Arc<dyn ProviderCredentialResolver>,
) -> ProviderResult<ProviderEntry> {
    build_development_provider_native(
        network,
        Arc::new(StoredCidarenSessionResolver::new(credentials)),
    )
}

#[cfg(test)]
mod tests {
    use asterism_networking::NetworkProfile;
    use asterism_provider_api::{
        CredentialReplacement, ExternalOauthCallbackBinding, ProviderContext, ProviderError,
        ProviderErrorKind, ProviderRegistry,
    };
    use asterism_secrets::SecretString;
    use asterism_secrets::{
        ProviderCredentialResolution, ResolvedProviderCredential, SecretStoreError,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::{CidarenClassTaskPageDocument, CidarenStudyTaskDocument, CidarenTokenSession};

    #[derive(Debug)]
    struct UnusedBoundaries;

    #[async_trait]
    impl ProviderCredentialResolver for UnusedBoundaries {
        async fn resolve_provider_credentials(
            &self,
            _request: ProviderCredentialResolution,
        ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
            Err(SecretStoreError::NotFound)
        }
    }

    #[async_trait]
    impl CidarenAuthenticationTransport for UnusedBoundaries {
        async fn validate_token(&self, _session: &CidarenTokenSession) -> ProviderResult<()> {
            Err(unused())
        }

        async fn validate_native_oauth_session(
            &self,
            _session: &CidarenTokenSession,
        ) -> ProviderResult<()> {
            Err(unused())
        }

        async fn exchange_external_oauth_callback(
            &self,
            _callback_url: SecretString,
            _binding: ExternalOauthCallbackBinding,
        ) -> ProviderResult<CredentialReplacement> {
            Err(unused())
        }
    }

    #[async_trait]
    impl CidarenSessionResolver for UnusedBoundaries {
        async fn resolve_session(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenTokenSession> {
            Err(unused())
        }
    }

    #[async_trait]
    impl CidarenClassTaskTransport for UnusedBoundaries {
        async fn fetch_class_task_pages(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<CidarenClassTaskPageDocument>> {
            Err(unused())
        }
    }

    #[async_trait]
    impl CidarenStudyTaskTransport for UnusedBoundaries {
        async fn fetch_study_task_document(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenStudyTaskDocument> {
            Err(unused())
        }
    }

    #[async_trait]
    impl CidarenAnswerEvidenceTransport for UnusedBoundaries {
        async fn bind_answer_evidence(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _detail: &asterism_provider_api::RemoteTaskDetail,
        ) -> ProviderResult<crate::CidarenAnswerEvidenceBinding> {
            Err(unused())
        }

        async fn fetch_word_inventory(
            &self,
            _context: &ProviderContext,
            _binding: &crate::CidarenAnswerEvidenceBinding,
        ) -> ProviderResult<crate::CidarenWordInventory> {
            Err(unused())
        }

        async fn fetch_word_evidence(
            &self,
            _context: &ProviderContext,
            _lookup: &crate::CidarenWordLookup,
        ) -> ProviderResult<crate::CidarenWordEvidence> {
            Err(unused())
        }

        async fn resolve_word_prototype(
            &self,
            _context: &ProviderContext,
            _word: &str,
        ) -> ProviderResult<Option<String>> {
            Err(unused())
        }
    }

    #[async_trait]
    impl CidarenTaskScoreTransport for UnusedBoundaries {
        async fn fetch_task_score(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
            _detail: &asterism_provider_api::RemoteTaskDetail,
        ) -> ProviderResult<Option<asterism_domain::SubmissionScore>> {
            Err(unused())
        }
    }

    #[async_trait]
    impl CidarenAssessmentTransport for UnusedBoundaries {
        async fn start_answer(
            &self,
            _context: &ProviderContext,
            _request: &crate::CidarenStartAnswerRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            Err(unused())
        }

        async fn verify_answer(
            &self,
            _context: &ProviderContext,
            _request: &crate::CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            Err(unused())
        }

        async fn submit_answer_and_save(
            &self,
            _context: &ProviderContext,
            _request: &crate::CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            Err(unused())
        }

        async fn skip_answer(
            &self,
            _context: &ProviderContext,
            _request: &crate::CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
            Err(unused())
        }

        async fn submit_chose_word(
            &self,
            _context: &ProviderContext,
            _request: &crate::CidarenMutationRequest,
        ) -> ProviderResult<crate::CidarenAssessmentTransportOutcome> {
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
        assert!(entry.question_inventory.is_some());
        assert!(entry.question_parse.is_none());
        assert!(entry.answer_resolve.is_some());
        assert!(entry.submission_build.is_some());
        assert!(entry.submission_execute.is_some());
        assert!(entry.submission_verify.is_some());
        assert!(entry.answer_history_harvest.is_none());
        assert!(entry.task_execution.is_none());
        assert!(entry.browser_bridge.is_some());
        assert_eq!(entry.runtime_settings.version, 2);
        assert_eq!(entry.runtime_settings.definitions.len(), 10);

        let mut registry = ProviderRegistry::default();
        registry.register(entry).unwrap();
        assert!(
            registry
                .get(&asterism_domain::ProviderId::new("cidaren").unwrap())
                .is_some()
        );

        let network = ResolvedNetworkProfile::resolve(&NetworkProfile::default(), None, None)
            .expect("built-in network profile");
        let boundaries = Arc::new(UnusedBoundaries);
        let native = build_development_provider_native(&network, boundaries).unwrap();
        let mut registry = ProviderRegistry::default();
        registry.register(native).unwrap();

        let stored =
            build_development_provider_with_stored_session(&network, Arc::new(UnusedBoundaries))
                .unwrap();
        let mut registry = ProviderRegistry::default();
        registry.register(stored).unwrap();
    }

    fn unused() -> ProviderError {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "fixture boundary must not be called",
        )
    }
}
