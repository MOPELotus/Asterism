use std::{fmt, sync::Arc};

use asterism_domain::RemoteState;
use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, RemoteProgress, TaskDetailCapability, TaskProgressCapability,
};
use async_trait::async_trait;
use chrono::Utc;

use crate::metadata::development_metadata;

const MAX_REMOTE_TASK_ID_BYTES: usize = 640;

/// Dispatches progress reads without flattening Chaoxing's distinct module
/// protocols. Chapter, Work and Exam reuse exact fresh Task rediscovery;
/// executable resources keep their targeted card-level recovery path.
pub struct ChaoxingTaskProgress {
    metadata: ProviderMetadata,
    detail: Arc<dyn TaskDetailCapability>,
    resource: Arc<dyn TaskProgressCapability>,
}

impl ChaoxingTaskProgress {
    /// Builds the module-aware read-only progress capability.
    ///
    /// # Errors
    ///
    /// Returns a sanitized internal error if compile-time metadata is invalid.
    pub fn try_new(
        detail: Arc<dyn TaskDetailCapability>,
        resource: Arc<dyn TaskProgressCapability>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            detail,
            resource,
        })
    }
}

impl fmt::Debug for ChaoxingTaskProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChaoxingTaskProgress")
            .field("metadata", &self.metadata)
            .field("detail", &"configured")
            .field("resource", &"configured")
            .finish()
    }
}

impl ProviderIdentity for ChaoxingTaskProgress {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskProgressCapability for ChaoxingTaskProgress {
    async fn read_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress> {
        validate_context(context, &self.metadata)?;
        let module = task_module(remote_task_id)?;
        if module == "resource" {
            return self.resource.read_progress(context, remote_task_id).await;
        }
        if !matches!(module, "chapter" | "work" | "exam" | "sign") {
            return Err(protocol_drift(
                "Chaoxing progress received an unsupported task identity",
            ));
        }
        let detail = self.detail.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id {
            return Err(protocol_drift(
                "Chaoxing detail returned a mismatched progress identity",
            ));
        }
        Ok(progress_from_state(detail.task.remote_state))
    }
}

fn task_module(remote_task_id: &str) -> ProviderResult<&str> {
    if remote_task_id.is_empty()
        || remote_task_id.len() > MAX_REMOTE_TASK_ID_BYTES
        || remote_task_id.chars().any(char::is_control)
    {
        return Err(protocol_drift("Chaoxing progress task identity is invalid"));
    }
    remote_task_id
        .split_once(':')
        .map(|(module, _)| module)
        .filter(|module| !module.is_empty())
        .ok_or_else(|| protocol_drift("Chaoxing progress task identity is invalid"))
}

fn progress_from_state(remote_state: RemoteState) -> RemoteProgress {
    RemoteProgress {
        remote_state,
        percent: match remote_state {
            RemoteState::Completed => Some(100),
            RemoteState::Pending => Some(0),
            RemoteState::Unknown
            | RemoteState::NotOpen
            | RemoteState::InProgress
            | RemoteState::Expired
            | RemoteState::Removed => None,
        },
        duration_seconds: None,
        updated_at: Utc::now(),
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Chaoxing progress received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Chaoxing progress requires an authenticated session",
        ));
    }
    Ok(())
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use asterism_domain::{AssessmentClass, ProviderAccountId, ProviderId, SecretId, SourceType};
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};

    use super::*;

    #[derive(Debug)]
    struct FixtureCapabilities {
        metadata: ProviderMetadata,
        detail_calls: AtomicUsize,
        resource_calls: AtomicUsize,
    }

    impl FixtureCapabilities {
        fn new() -> Self {
            Self {
                metadata: development_metadata().unwrap(),
                detail_calls: AtomicUsize::new(0),
                resource_calls: AtomicUsize::new(0),
            }
        }
    }

    impl ProviderIdentity for FixtureCapabilities {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskDetailCapability for FixtureCapabilities {
        async fn task_detail(
            &self,
            _context: &ProviderContext,
            remote_task_id: &str,
        ) -> ProviderResult<RemoteTaskDetail> {
            self.detail_calls.fetch_add(1, Ordering::Relaxed);
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: Some("course:100:200".to_owned()),
                    title: "fresh task".to_owned(),
                    source_type: if remote_task_id.starts_with("exam:") {
                        SourceType::Exam
                    } else {
                        SourceType::Work
                    },
                    assessment_class: AssessmentClass::Unknown,
                    remote_state: RemoteState::Completed,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: vec![asterism_domain::TaskCapability::ProgressRead],
                    fingerprint: "v1:fresh-progress".to_owned(),
                    normalized: serde_json::json!({"state": "completed"}),
                    raw_sanitized: serde_json::json!({"state": "completed"}),
                },
                normalized_detail: serde_json::json!({"state": "completed"}),
            })
        }
    }

    #[async_trait]
    impl TaskProgressCapability for FixtureCapabilities {
        async fn read_progress(
            &self,
            _context: &ProviderContext,
            _remote_task_id: &str,
        ) -> ProviderResult<RemoteProgress> {
            self.resource_calls.fetch_add(1, Ordering::Relaxed);
            Ok(progress_from_state(RemoteState::Pending))
        }
    }

    #[tokio::test]
    async fn module_progress_uses_detail_while_resource_keeps_targeted_read() {
        let capabilities = Arc::new(FixtureCapabilities::new());
        let progress =
            ChaoxingTaskProgress::try_new(capabilities.clone(), capabilities.clone()).unwrap();

        let work = progress
            .read_progress(&context(), "work:100:200:work-1")
            .await
            .unwrap();
        assert_eq!(work.remote_state, RemoteState::Completed);
        assert_eq!(work.percent, Some(100));
        let resource = progress
            .read_progress(&context(), "resource:100:200:4001:job-1")
            .await
            .unwrap();
        assert_eq!(resource.remote_state, RemoteState::Pending);
        assert_eq!(resource.percent, Some(0));
        assert_eq!(capabilities.detail_calls.load(Ordering::Relaxed), 1);
        assert_eq!(capabilities.resource_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn progress_rejects_foreign_identity_before_capability_calls() {
        let capabilities = Arc::new(FixtureCapabilities::new());
        let progress =
            ChaoxingTaskProgress::try_new(capabilities.clone(), capabilities.clone()).unwrap();

        let error = progress
            .read_progress(&context(), "foreign:100:200:task")
            .await
            .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::ProtocolDrift);
        assert_eq!(capabilities.detail_calls.load(Ordering::Relaxed), 0);
        assert_eq!(capabilities.resource_calls.load(Ordering::Relaxed), 0);
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("chaoxing").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "chaoxing-progress-test".to_owned(),
        }
    }
}
