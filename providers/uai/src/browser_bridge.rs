use std::{fmt, sync::Arc};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    BrowserBridgeCapability, BrowserSessionSpec, ProviderContext, ProviderError, ProviderErrorKind,
    ProviderIdentity, ProviderMetadata, ProviderResult, TaskDetailCapability,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::metadata::development_metadata;

const UCONTENT_ORIGIN: &str = "https://ucontent.unipus.cn";
const IPUB_ORIGIN: &str = "https://ipub.unipus.cn";

/// Builds the account-and-Task isolated browser boundary required by UAI's
/// page-residence and interaction workflow.
///
/// The shared `BrowserBridge` contract does not yet carry donor-specific DOM
/// actions or a duration budget. Those remain an Engine/Core integration gap;
/// this capability still performs fresh Task rediscovery before granting a
/// browser session and never exposes route or credential material.
pub struct UaiBrowserBridge {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
}

impl UaiBrowserBridge {
    /// Creates the UAI browser-session boundary around the same fresh detail
    /// reader used by native execution paths.
    ///
    /// # Errors
    ///
    /// Returns a sanitized Provider error if compile-time metadata is invalid.
    pub fn try_new(details: Arc<dyn TaskDetailCapability>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
        })
    }
}

impl fmt::Debug for UaiBrowserBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UaiBrowserBridge")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .finish()
    }
}

impl ProviderIdentity for UaiBrowserBridge {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl BrowserBridgeCapability for UaiBrowserBridge {
    async fn browser_session_spec(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<BrowserSessionSpec> {
        validate_context(context, &self.metadata)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::BrowserBridge)
        {
            return Err(ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "UAI fresh Task does not authorize a BrowserBridge session",
            ));
        }

        Ok(BrowserSessionSpec {
            isolation_key: isolation_key(context, remote_task_id),
            allowed_origins: vec![UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()],
            // The audited donor depends on a real rendered page, DOM events,
            // iframe messaging and media state. Do not silently substitute a
            // headless-only execution mode before that boundary is verified.
            headless: false,
        })
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "UAI BrowserBridge received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "UAI BrowserBridge requires an authenticated session",
        ));
    }
    Ok(())
}

fn isolation_key(context: &ProviderContext, remote_task_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"asterism:uai:browser-session:v1\0");
    digest.update(context.account_id.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(remote_task_id.as_bytes());
    format!("uai-task-{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};

    use super::*;

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        advertised: bool,
    }

    impl ProviderIdentity for FixtureDetail {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureDetail {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course-resource:2001".to_owned()),
                    title: "Read the passage".to_owned(),
                    source_type: SourceType::Resource,
                    assessment_class: AssessmentClass::Routine,
                    remote_state: RemoteState::Unknown,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: self
                        .advertised
                        .then_some(TaskCapability::BrowserBridge)
                        .into_iter()
                        .collect(),
                    fingerprint: "v1:fixture".to_owned(),
                    normalized: serde_json::json!({"schema": "uai.group-task.v1"}),
                    raw_sanitized: serde_json::json!({"schema": "uai.group-task.raw.v1"}),
                },
                normalized_detail: serde_json::json!({"schema": "uai.group-task-detail.v1"}),
            })
        }
    }

    #[tokio::test]
    async fn session_spec_is_fresh_task_bound_and_origin_bounded() {
        let detail = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised: true,
        });
        let bridge = UaiBrowserBridge::try_new(detail).unwrap();
        let context = provider_context();
        let first = bridge
            .browser_session_spec(&context, "group:2001:unit-1:group-1")
            .await
            .unwrap();
        let same = bridge
            .browser_session_spec(&context, "group:2001:unit-1:group-1")
            .await
            .unwrap();
        let other = bridge
            .browser_session_spec(&context, "group:2001:unit-1:group-2")
            .await
            .unwrap();

        assert_eq!(first, same);
        assert_ne!(first.isolation_key, other.isolation_key);
        assert_eq!(
            first.allowed_origins,
            [UCONTENT_ORIGIN.to_owned(), IPUB_ORIGIN.to_owned()]
        );
        assert!(!first.headless);
        assert!(
            !first
                .isolation_key
                .contains(&context.account_id.to_string())
        );
        assert!(!first.isolation_key.contains("group-1"));
    }

    #[tokio::test]
    async fn unadvertised_fresh_task_fails_closed() {
        let detail = Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            advertised: false,
        });
        let error = UaiBrowserBridge::try_new(detail)
            .unwrap()
            .browser_session_spec(&provider_context(), "group:2001:unit-1:group-1")
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("uai").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "uai-browser-bridge-test".to_owned(),
        }
    }
}
