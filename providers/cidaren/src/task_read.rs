use std::{fmt, sync::Arc};

use asterism_provider_api::{
    ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity, ProviderMetadata,
    ProviderResult, RemoteProgress, RemoteTask, RemoteTaskDetail, TaskDetailCapability,
    TaskProgressCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use crate::{
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, class_tasks::parse_task_inventory,
    metadata::development_metadata,
};

const MAX_RELEASE_ID_BYTES: usize = 32;

/// Fresh identity-bound Task detail over complete class-task pagination.
pub struct CidarenTaskDetail {
    metadata: ProviderMetadata,
    transport: Arc<dyn CidarenClassTaskTransport>,
}

impl CidarenTaskDetail {
    /// Creates the capability around an authenticated complete-scan transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn CidarenClassTaskTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for CidarenTaskDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenTaskDetail")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenTaskDetail {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskDetailCapability for CidarenTaskDetail {
    async fn task_detail(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteTaskDetail> {
        validate_context(context, &self.metadata)?;
        let release_id = parse_release_identity(remote_task_id)?;
        let pages = self.transport.fetch_class_task_pages(context).await?;
        let task = find_fresh_task(remote_task_id, release_id, &pages)?;
        Ok(RemoteTaskDetail {
            normalized_detail: serde_json::json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": release_id,
                "task": task.normalized,
            }),
            task,
        })
    }
}

/// Fresh read-only class-task progress.
pub struct CidarenTaskProgress {
    metadata: ProviderMetadata,
    transport: Arc<dyn CidarenClassTaskTransport>,
}

impl CidarenTaskProgress {
    /// Creates the capability around an authenticated complete-scan transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(transport: Arc<dyn CidarenClassTaskTransport>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            transport,
        })
    }
}

impl fmt::Debug for CidarenTaskProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenTaskProgress")
            .field("metadata", &self.metadata)
            .field("transport", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenTaskProgress {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskProgressCapability for CidarenTaskProgress {
    async fn read_progress(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteProgress> {
        validate_context(context, &self.metadata)?;
        let release_id = parse_release_identity(remote_task_id)?;
        let pages = self.transport.fetch_class_task_pages(context).await?;
        let task = find_fresh_task(remote_task_id, release_id, &pages)?;
        let percent = task
            .normalized
            .get("progress")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= 100)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::ProtocolDrift,
                    "Cidaren fresh Task has no valid progress observation",
                )
            })?;
        Ok(RemoteProgress {
            remote_state: task.remote_state,
            percent: Some(percent),
            duration_seconds: None,
            updated_at: Utc::now(),
        })
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren Task read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren Task read requires an authenticated session",
        ));
    }
    Ok(())
}

fn parse_release_identity(remote_task_id: &str) -> ProviderResult<&str> {
    let release_id = remote_task_id
        .strip_prefix("class-task:")
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_RELEASE_ID_BYTES
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && value != &"0"
                && !value.starts_with('0')
        })
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::ProtocolDrift,
                "Cidaren Task identity is invalid",
            )
        })?;
    Ok(release_id)
}

fn find_fresh_task(
    remote_task_id: &str,
    release_id: &str,
    pages: &[CidarenClassTaskPageDocument],
) -> ProviderResult<RemoteTask> {
    let mut matches = parse_task_inventory(None, pages)?
        .into_iter()
        .filter(|task| task.remote_id == remote_task_id);
    let task = matches.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Cidaren class task no longer exists in the fresh inventory",
        )
    })?;
    if matches.next().is_some()
        || task.normalized.get("release_id").and_then(Value::as_str) != Some(release_id)
    {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Cidaren fresh Task identity is duplicated or drifted",
        ));
    }
    Ok(task)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, RemoteState, SecretId};

    use super::*;

    #[derive(Debug)]
    struct FixtureTransport;

    #[async_trait]
    impl CidarenClassTaskTransport for FixtureTransport {
        async fn fetch_class_task_pages(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<Vec<CidarenClassTaskPageDocument>> {
            Ok(fixture_pages())
        }
    }

    #[tokio::test]
    async fn task_detail_rebinds_release_identity_to_fresh_row() {
        let capability = CidarenTaskDetail::try_new(Arc::new(FixtureTransport)).unwrap();
        let detail = capability
            .task_detail(&provider_context(), "class-task:2002")
            .await
            .unwrap();
        assert_eq!(detail.task.remote_id, "class-task:2002");
        assert_eq!(detail.task.normalized["task_id"], -1);
        assert_eq!(detail.normalized_detail["release_id"], "2002");
        assert_eq!(
            detail.normalized_detail["schema"],
            "cidaren.class-task.detail.v1"
        );
    }

    #[tokio::test]
    async fn progress_is_fresh_and_never_converts_raw_time() {
        let capability = CidarenTaskProgress::try_new(Arc::new(FixtureTransport)).unwrap();
        let progress = capability
            .read_progress(&provider_context(), "class-task:2002")
            .await
            .unwrap();
        assert_eq!(progress.remote_state, RemoteState::InProgress);
        assert_eq!(progress.percent, Some(35));
        assert!(progress.duration_seconds.is_none());

        let completed = capability
            .read_progress(&provider_context(), "class-task:2003")
            .await
            .unwrap();
        assert_eq!(completed.remote_state, RemoteState::Completed);
        assert_eq!(completed.percent, Some(100));
    }

    #[tokio::test]
    async fn malformed_or_missing_identity_fails_closed() {
        let capability = CidarenTaskDetail::try_new(Arc::new(FixtureTransport)).unwrap();
        for remote_id in [
            "task:2002",
            "class-task:0",
            "class-task:02002",
            "class-task:x",
        ] {
            assert_eq!(
                capability
                    .task_detail(&provider_context(), remote_id)
                    .await
                    .unwrap_err()
                    .kind,
                ProviderErrorKind::ProtocolDrift
            );
        }
        assert_eq!(
            capability
                .task_detail(&provider_context(), "class-task:9999")
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::RemoteChanged
        );
    }

    fn fixture_pages() -> Vec<CidarenClassTaskPageDocument> {
        vec![
            CidarenClassTaskPageDocument::try_new(
                1,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-1.json"),
            )
            .unwrap(),
            CidarenClassTaskPageDocument::try_new(
                2,
                include_str!("../../../fixtures/providers/cidaren/tasks/class-task-page-2.json"),
            )
            .unwrap(),
        ]
    }

    fn provider_context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-task-read-test".to_owned(),
        }
    }
}
