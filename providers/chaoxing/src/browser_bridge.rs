use std::{fmt, sync::Arc};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    BrowserBridgeCapability, BrowserSessionSpec, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::metadata::development_metadata;

const COURSE_START_URL: &str = "https://mooc2-ans.chaoxing.com/mooc2-ans/mycourse/stu";
const EXAM_START_URL: &str = "https://mooc1.chaoxing.com/exam-ans/mooc2/exam/exam-list";

/// Opens an isolated, visible Chaoxing browser session for the exact freshly
/// rediscovered Task. The first 0.1.0 path intentionally leaves rendered
/// challenge handling to the user instead of inventing Provider JavaScript.
pub struct ChaoxingBrowserBridge {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
}

impl ChaoxingBrowserBridge {
    /// # Errors
    ///
    /// Returns an internal error when compile-time metadata is invalid.
    pub fn try_new(details: Arc<dyn TaskDetailCapability>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
        })
    }
}

impl fmt::Debug for ChaoxingBrowserBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingBrowserBridge")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingBrowserBridge {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl BrowserBridgeCapability for ChaoxingBrowserBridge {
    async fn browser_session_spec(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<BrowserSessionSpec> {
        validate_context(context, &self.metadata)?;
        validate_task_identity(remote_task_id)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::BrowserBridge)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::RemoteChanged,
                "Chaoxing fresh Task no longer allows a browser session",
            ));
        }
        let start_url = if remote_task_id.starts_with("exam:") {
            EXAM_START_URL
        } else {
            COURSE_START_URL
        };
        let spec = BrowserSessionSpec {
            version: 1,
            start_url: start_url.to_owned(),
            isolation_key: isolation_key(context, remote_task_id),
            allowed_origins: vec![
                "https://i.chaoxing.com".to_owned(),
                "https://passport2.chaoxing.com".to_owned(),
                "https://mooc2-ans.chaoxing.com".to_owned(),
                "https://mooc1.chaoxing.com".to_owned(),
                "https://mooc1-api.chaoxing.com".to_owned(),
                "https://mobilelearn.chaoxing.com".to_owned(),
                "https://captcha.chaoxing.com".to_owned(),
                "https://zhibo.chaoxing.com".to_owned(),
            ],
            read_sources: Vec::new(),
            headless: false,
        };
        spec.validate().map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::Internal,
                "Chaoxing browser session policy is invalid",
            )
        })?;
        Ok(spec)
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing BrowserBridge received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing BrowserBridge requires an authenticated account",
        ));
    }
    Ok(())
}

fn validate_task_identity(remote_task_id: &str) -> ProviderResult<()> {
    if remote_task_id.is_empty()
        || remote_task_id.len() > 640
        || remote_task_id.trim() != remote_task_id
        || remote_task_id.chars().any(char::is_control)
    {
        Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Chaoxing BrowserBridge Task identity is invalid",
        ))
    } else {
        Ok(())
    }
}

fn isolation_key(context: &ProviderContext, remote_task_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:chaoxing:browser-session:v1\0");
    digest.update(context.account_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(remote_task_id.as_bytes());
    format!("chaoxing-task-{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    struct FixtureDetails;

    impl ProviderIdentity for FixtureDetails {
        fn metadata(&self) -> &ProviderMetadata {
            static METADATA: std::sync::OnceLock<ProviderMetadata> = std::sync::OnceLock::new();
            METADATA.get_or_init(|| development_metadata().unwrap())
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetails {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:100:200".to_owned()),
                    title: "Synthetic".to_owned(),
                    source_type: SourceType::Exam,
                    assessment_class: AssessmentClass::Formal,
                    remote_state: RemoteState::Pending,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: vec![TaskCapability::ProgressRead, TaskCapability::BrowserBridge],
                    fingerprint: "v1:synthetic".to_owned(),
                    normalized: json!({"schema": "chaoxing.inventory.v1"}),
                    raw_sanitized: json!({}),
                },
                normalized_detail: json!({"schema": "chaoxing.task-detail.v1"}),
            })
        }
    }

    #[tokio::test]
    async fn visible_browser_session_is_account_and_task_bound() {
        let bridge = ChaoxingBrowserBridge::try_new(Arc::new(FixtureDetails)).unwrap();
        let context = ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-browser-test".to_owned(),
        };
        let spec = bridge
            .browser_session_spec(&context, "exam:100:200:exam-1")
            .await
            .unwrap();
        spec.validate().unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.start_url, EXAM_START_URL);
        assert!(!spec.headless);
        assert!(
            spec.allowed_origins
                .contains(&"https://captcha.chaoxing.com".to_owned())
        );
        assert!(!format!("{bridge:?}").contains("exam-1"));
    }
}
