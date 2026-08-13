use std::{fmt, sync::Arc};

use asterism_domain::TaskCapability;
use asterism_provider_api::{
    DurationReadCapability, ProviderContext, ProviderError, ProviderErrorKind, ProviderIdentity,
    ProviderMetadata, ProviderResult, RemoteDuration, RemoteTask, TaskDetailCapability,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use crate::metadata::development_metadata;

const MILLIS_PER_SECOND: u64 = 1_000;
const MAX_DURATION_MILLIS: u64 = 100 * 366 * 24 * 60 * 60 * MILLIS_PER_SECOND;

/// Fresh read-only Cidaren duration derived from the task inventory's
/// millisecond `time_spent` observation.
pub struct CidarenDurationRead {
    metadata: ProviderMetadata,
    details: Arc<dyn TaskDetailCapability>,
}

impl CidarenDurationRead {
    /// Creates the duration capability around the existing fresh Task reader.
    ///
    /// # Errors
    ///
    /// Returns an internal error if compile-time Provider metadata is invalid.
    pub fn try_new(details: Arc<dyn TaskDetailCapability>) -> ProviderResult<Self> {
        Ok(Self {
            metadata: development_metadata()?,
            details,
        })
    }
}

impl fmt::Debug for CidarenDurationRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenDurationRead")
            .field("metadata", &self.metadata)
            .field("details", &"configured")
            .finish()
    }
}

impl ProviderIdentity for CidarenDurationRead {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl DurationReadCapability for CidarenDurationRead {
    async fn read_duration(
        &self,
        context: &ProviderContext,
        remote_task_id: &str,
    ) -> ProviderResult<RemoteDuration> {
        validate_context(context, &self.metadata)?;
        let detail = self.details.task_detail(context, remote_task_id).await?;
        if detail.task.remote_id != remote_task_id
            || !detail
                .task
                .capabilities
                .contains(&TaskCapability::DurationRead)
        {
            return Err(protocol_drift(
                "Cidaren fresh Task does not advertise a duration observation",
            ));
        }
        let task = detail
            .normalized_detail
            .get("task")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol_drift("Cidaren fresh Task detail has no task object"))?;
        validate_detail_family(&detail.normalized_detail, task)?;
        let duration_seconds = duration_seconds_from_task(&detail.task)?.ok_or_else(|| {
            protocol_drift("Cidaren fresh Task has no bounded millisecond duration")
        })?;
        Ok(RemoteDuration {
            duration_seconds,
            updated_at: Utc::now(),
        })
    }
}

pub(crate) fn duration_seconds_from_task(task: &RemoteTask) -> ProviderResult<Option<u64>> {
    match task.normalized.get("time_spent_raw") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .filter(|value| *value <= MAX_DURATION_MILLIS)
            .map(|value| Some(value / MILLIS_PER_SECOND))
            .ok_or_else(|| {
                protocol_drift("Cidaren fresh Task has an invalid millisecond duration")
            }),
        Some(_) => Err(protocol_drift(
            "Cidaren fresh Task has an invalid millisecond duration",
        )),
    }
}

fn validate_detail_family(
    detail: &Value,
    task: &serde_json::Map<String, Value>,
) -> ProviderResult<()> {
    let valid = matches!(
        (
            detail.get("schema").and_then(Value::as_str),
            task.get("schema").and_then(Value::as_str),
        ),
        (
            Some("cidaren.class-task.detail.v1"),
            Some("cidaren.class-task.v1")
        ) | (
            Some("cidaren.study-task.detail.v1"),
            Some("cidaren.study-task.v1")
        )
    );
    if valid {
        Ok(())
    } else {
        Err(protocol_drift(
            "Cidaren duration Task detail has an inconsistent schema",
        ))
    }
}

fn validate_context(context: &ProviderContext, metadata: &ProviderMetadata) -> ProviderResult<()> {
    if context.provider_id != metadata.id {
        return Err(ProviderError::new(
            ProviderErrorKind::Internal,
            "Cidaren duration read received a mismatched Provider context",
        ));
    }
    if context.credential_refs.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren duration read requires an authenticated account binding",
        ));
    }
    Ok(())
}

fn protocol_drift(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AssessmentClass, ProviderAccountId, ProviderId, RemoteState, SecretId, SourceType,
    };
    use asterism_provider_api::{RemoteTask, RemoteTaskDetail};
    use serde_json::{Map, json};

    use super::*;

    #[derive(Debug)]
    struct FixtureDetail {
        metadata: ProviderMetadata,
        duration_millis: Option<u64>,
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
            let normalized = json!({
                "schema": "cidaren.class-task.v1",
                "time_spent_raw": self.duration_millis,
            });
            Ok(RemoteTaskDetail {
                task: RemoteTask {
                    remote_id: remote_task_id.to_owned(),
                    course_remote_id: None,
                    title: "Synthetic".to_owned(),
                    source_type: SourceType::Practice,
                    assessment_class: AssessmentClass::Routine,
                    remote_state: RemoteState::InProgress,
                    opens_at: None,
                    due_at: None,
                    closes_at: None,
                    capabilities: self
                        .advertised
                        .then_some(TaskCapability::DurationRead)
                        .into_iter()
                        .collect(),
                    fingerprint: "synthetic".to_owned(),
                    normalized: normalized.clone(),
                    raw_sanitized: Map::new().into(),
                },
                normalized_detail: json!({
                    "schema": "cidaren.class-task.detail.v1",
                    "release_id": "2002",
                    "task": normalized,
                }),
            })
        }
    }

    #[tokio::test]
    async fn fresh_milliseconds_are_exposed_as_bounded_seconds() {
        let capability = CidarenDurationRead::try_new(Arc::new(FixtureDetail {
            metadata: development_metadata().unwrap(),
            duration_millis: Some(2_181_226),
            advertised: true,
        }))
        .unwrap();
        let duration = capability
            .read_duration(&context(), "class-task:2002")
            .await
            .unwrap();
        assert_eq!(duration.duration_seconds, 2_181);
    }

    #[tokio::test]
    async fn absent_unadvertised_or_unbounded_duration_fails_closed() {
        for (duration_millis, advertised) in [
            (None, true),
            (Some(1_000), false),
            (Some(MAX_DURATION_MILLIS + 1), true),
        ] {
            let capability = CidarenDurationRead::try_new(Arc::new(FixtureDetail {
                metadata: development_metadata().unwrap(),
                duration_millis,
                advertised,
            }))
            .unwrap();
            assert!(
                capability
                    .read_duration(&context(), "class-task:2002")
                    .await
                    .is_err()
            );
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("cidaren").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "cidaren-duration-test".to_owned(),
        }
    }
}
