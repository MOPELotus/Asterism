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
    CidarenClassTaskTransport, CidarenStudyTaskTransport, class_tasks::parse_task_inventory,
    metadata::development_metadata, study_tasks::parse_study_task_inventory,
};

const MAX_RELEASE_ID_BYTES: usize = 32;
const MAX_STUDY_COMPONENT_BYTES: usize = 256;

/// Fresh identity-bound Task detail over the matching class/study inventory.
pub struct CidarenTaskDetail {
    metadata: ProviderMetadata,
    class_tasks: Arc<dyn CidarenClassTaskTransport>,
    study_tasks: Arc<dyn CidarenStudyTaskTransport>,
}

impl CidarenTaskDetail {
    /// Creates the capability around an authenticated complete-scan transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        class_tasks: Arc<dyn CidarenClassTaskTransport>,
        study_tasks: Arc<dyn CidarenStudyTaskTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            class_tasks,
            study_tasks,
        })
    }
}

impl fmt::Debug for CidarenTaskDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenTaskDetail")
            .field("metadata", &self.metadata)
            .field("class_tasks", &"configured")
            .field("study_tasks", &"configured")
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
        let identity = parse_task_identity(remote_task_id)?;
        let task = find_fresh_task(
            remote_task_id,
            &identity,
            context,
            self.class_tasks.as_ref(),
            self.study_tasks.as_ref(),
        )
        .await?;
        let normalized_detail = match identity {
            TaskIdentity::Class { release_id } => serde_json::json!({
                "schema": "cidaren.class-task.detail.v1",
                "release_id": release_id,
                "task": task.normalized,
            }),
            TaskIdentity::Study { course_id, list_id } => serde_json::json!({
                "schema": "cidaren.study-task.detail.v1",
                "course_id": course_id,
                "list_id": list_id,
                "task": task.normalized,
            }),
        };
        Ok(RemoteTaskDetail {
            normalized_detail,
            task,
        })
    }
}

/// Fresh read-only class/study Task progress.
pub struct CidarenTaskProgress {
    metadata: ProviderMetadata,
    class_tasks: Arc<dyn CidarenClassTaskTransport>,
    study_tasks: Arc<dyn CidarenStudyTaskTransport>,
}

impl CidarenTaskProgress {
    /// Creates the capability around an authenticated complete-scan transport.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time metadata is invalid.
    pub fn try_new(
        class_tasks: Arc<dyn CidarenClassTaskTransport>,
        study_tasks: Arc<dyn CidarenStudyTaskTransport>,
    ) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            class_tasks,
            study_tasks,
        })
    }
}

impl fmt::Debug for CidarenTaskProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenTaskProgress")
            .field("metadata", &self.metadata)
            .field("class_tasks", &"configured")
            .field("study_tasks", &"configured")
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
        let identity = parse_task_identity(remote_task_id)?;
        let task = find_fresh_task(
            remote_task_id,
            &identity,
            context,
            self.class_tasks.as_ref(),
            self.study_tasks.as_ref(),
        )
        .await?;
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

#[derive(Clone, Copy, Debug)]
enum TaskIdentity<'a> {
    Class {
        release_id: &'a str,
    },
    Study {
        course_id: &'a str,
        list_id: &'a str,
    },
}

fn parse_task_identity(remote_task_id: &str) -> ProviderResult<TaskIdentity<'_>> {
    if let Some(release_id) = remote_task_id.strip_prefix("class-task:")
        && !release_id.is_empty()
        && release_id.len() <= MAX_RELEASE_ID_BYTES
        && release_id.bytes().all(|byte| byte.is_ascii_digit())
        && release_id != "0"
        && !release_id.starts_with('0')
    {
        return Ok(TaskIdentity::Class { release_id });
    }
    if let Some(identity) = remote_task_id.strip_prefix("study-task:")
        && let Some((course_id, list_id)) = identity.split_once(':')
        && valid_study_component(course_id)
        && valid_study_component(list_id)
    {
        return Ok(TaskIdentity::Study { course_id, list_id });
    }
    Err(ProviderError::new(
        ProviderErrorKind::ProtocolDrift,
        "Cidaren Task identity is invalid",
    ))
}

async fn find_fresh_task(
    remote_task_id: &str,
    identity: &TaskIdentity<'_>,
    context: &ProviderContext,
    class_tasks: &dyn CidarenClassTaskTransport,
    study_tasks: &dyn CidarenStudyTaskTransport,
) -> ProviderResult<RemoteTask> {
    let tasks = match identity {
        TaskIdentity::Class { .. } => {
            let pages = class_tasks.fetch_class_task_pages(context).await?;
            parse_task_inventory(None, &pages)?
        }
        TaskIdentity::Study { .. } => {
            let document = study_tasks.fetch_study_task_document(context).await?;
            parse_study_task_inventory(None, &document)?
        }
    };
    let mut matches = tasks
        .into_iter()
        .filter(|task| task.remote_id == remote_task_id);
    let task = matches.next().ok_or_else(|| {
        ProviderError::new(
            ProviderErrorKind::RemoteChanged,
            "Cidaren Task no longer exists in the matching fresh inventory",
        )
    })?;
    let identity_matches = match identity {
        TaskIdentity::Class { release_id } => {
            task.normalized.get("release_id").and_then(Value::as_str) == Some(*release_id)
        }
        TaskIdentity::Study { course_id, list_id } => {
            task.normalized.get("course_id").and_then(Value::as_str) == Some(*course_id)
                && task.normalized.get("list_id").and_then(Value::as_str) == Some(*list_id)
        }
    };
    if matches.next().is_some() || !identity_matches {
        return Err(ProviderError::new(
            ProviderErrorKind::ProtocolDrift,
            "Cidaren fresh Task identity is duplicated or drifted",
        ));
    }
    Ok(task)
}

fn valid_study_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_STUDY_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, RemoteState, SecretId};

    use super::*;
    use crate::{CidarenClassTaskPageDocument, CidarenStudyTaskDocument};

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

    #[async_trait]
    impl CidarenStudyTaskTransport for FixtureTransport {
        async fn fetch_study_task_document(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<CidarenStudyTaskDocument> {
            Ok(fixture_study_document())
        }
    }

    #[tokio::test]
    async fn task_detail_rebinds_release_identity_to_fresh_row() {
        let transport = Arc::new(FixtureTransport);
        let capability = CidarenTaskDetail::try_new(transport.clone(), transport).unwrap();
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
    async fn study_detail_rebinds_course_and_list_identity() {
        let transport = Arc::new(FixtureTransport);
        let capability = CidarenTaskDetail::try_new(transport.clone(), transport).unwrap();
        let detail = capability
            .task_detail(&provider_context(), "study-task:course-a:course-a_02")
            .await
            .unwrap();
        assert_eq!(detail.task.normalized["task_id"], 71002);
        assert_eq!(detail.normalized_detail["course_id"], "course-a");
        assert_eq!(detail.normalized_detail["list_id"], "course-a_02");
        assert_eq!(
            detail.normalized_detail["schema"],
            "cidaren.study-task.detail.v1"
        );
    }

    #[tokio::test]
    async fn progress_is_fresh_and_never_converts_raw_time() {
        let transport = Arc::new(FixtureTransport);
        let capability = CidarenTaskProgress::try_new(transport.clone(), transport).unwrap();
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

        let transport = Arc::new(FixtureTransport);
        let study = CidarenTaskProgress::try_new(transport.clone(), transport)
            .unwrap()
            .read_progress(&provider_context(), "study-task:course-a:course-a_02")
            .await
            .unwrap();
        assert_eq!(study.remote_state, RemoteState::InProgress);
        assert_eq!(study.percent, Some(35));
        assert!(study.duration_seconds.is_none());
    }

    #[tokio::test]
    async fn malformed_or_missing_identity_fails_closed() {
        let transport = Arc::new(FixtureTransport);
        let capability = CidarenTaskDetail::try_new(transport.clone(), transport).unwrap();
        for remote_id in [
            "task:2002",
            "class-task:0",
            "class-task:02002",
            "class-task:x",
            "study-task:course-a",
            "study-task:unsafe/course:list",
            "study-task:course-a:list:extra",
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
        assert_eq!(
            capability
                .task_detail(&provider_context(), "study-task:course-a:course-a_99")
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

    fn fixture_study_document() -> CidarenStudyTaskDocument {
        CidarenStudyTaskDocument::try_new(
            "course-a",
            include_str!("../../../fixtures/providers/cidaren/tasks/study-task-list.json"),
        )
        .unwrap()
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
