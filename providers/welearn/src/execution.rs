use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    ExecutionEventSink, ExecutionOutcome, ExecutionRequest, ProviderContext, ProviderError,
    ProviderErrorKind, ProviderIdentity, ProviderMetadata, ProviderResult, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::metadata::development_metadata;

/// Dispatches `WELearn`'s independent `ResourceExecution` and `DurationReport`
/// capabilities through the single shared `TaskExecution` registry slot. When
/// Core requests both advertised task actions, duration is reported first and
/// the completion/progress/score preset is applied only after its fresh
/// preservation verification succeeds.
pub struct WellearnTaskExecution {
    metadata: ProviderMetadata,
    resource: Arc<dyn TaskExecutionCapability>,
    duration: Arc<dyn TaskExecutionCapability>,
}

impl WellearnTaskExecution {
    /// Builds one exact-capability dispatcher.
    ///
    /// # Errors
    ///
    /// Returns an internal error when either implementation belongs to a
    /// different Provider contract.
    pub fn try_new(
        resource: Arc<dyn TaskExecutionCapability>,
        duration: Arc<dyn TaskExecutionCapability>,
    ) -> ProviderResult<Self> {
        let metadata = development_metadata()?;
        if resource.metadata() != &metadata || duration.metadata() != &metadata {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn execution implementations have mismatched metadata",
            ));
        }
        Ok(Self {
            metadata,
            resource,
            duration,
        })
    }
}

impl fmt::Debug for WellearnTaskExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WellearnTaskExecution")
            .field("metadata", &self.metadata)
            .field("resource", &"configured")
            .field("duration", &"configured")
            .finish()
    }
}

impl ProviderIdentity for WellearnTaskExecution {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
}

#[async_trait]
impl TaskExecutionCapability for WellearnTaskExecution {
    fn allows_execution_from_remote_state(
        &self,
        requested_capabilities: &[TaskCapability],
        remote_state: RemoteState,
    ) -> bool {
        if remote_state != RemoteState::NotOpen {
            return false;
        }
        matches!(
            requested_capabilities,
            [TaskCapability::ResourceExecution | TaskCapability::DurationReport]
                | [
                    TaskCapability::ResourceExecution,
                    TaskCapability::DurationReport
                ]
                | [
                    TaskCapability::DurationReport,
                    TaskCapability::ResourceExecution
                ]
        )
    }

    fn execution_plan(
        &self,
        requested_capabilities: &[TaskCapability],
    ) -> ProviderResult<Vec<TaskCapability>> {
        match requested_capabilities {
            [capability @ (TaskCapability::ResourceExecution | TaskCapability::DurationReport)] => {
                Ok(vec![*capability])
            }
            [
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ]
            | [
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ] => Ok(vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ]),
            _ => Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn execution received an unsupported capability combination",
            )),
        }
    }

    fn requires_execution_verification(&self, requested_capabilities: &[TaskCapability]) -> bool {
        requested_capabilities == [TaskCapability::ResourceExecution]
    }

    async fn execute(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        match request.requested_capabilities.as_slice() {
            [TaskCapability::ResourceExecution] => {
                self.resource.execute(context, request, events).await
            }
            [TaskCapability::DurationReport] => {
                self.duration.execute(context, request, events).await
            }
            [
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ]
            | [
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ] => {
                let mut duration_request = request.clone();
                duration_request.requested_capabilities = vec![TaskCapability::DurationReport];
                let duration = self
                    .duration
                    .execute(context, &duration_request, events)
                    .await?;
                if !duration.verified {
                    return Err(ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "WELearn combined execution received an unverified duration result",
                    ));
                }

                let mut resource_request = request.clone();
                resource_request.requested_capabilities = vec![TaskCapability::ResourceExecution];
                let resource = self
                    .resource
                    .execute(context, &resource_request, events)
                    .await?;
                if !resource.verified {
                    return Err(ProviderError::new(
                        ProviderErrorKind::InvalidResponse,
                        "WELearn combined execution received an unverified resource result",
                    ));
                }

                Ok(ExecutionOutcome {
                    remote_state: resource.remote_state,
                    verified: true,
                    result_sanitized: serde_json::json!({
                        "schema": "welearn.combined-execution.v1",
                        "order": ["duration_report", "resource_execution"],
                        "duration": duration.result_sanitized,
                        "resource": resource.result_sanitized,
                    }),
                })
            }
            _ => Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn execution received an unsupported capability combination",
            )),
        }
    }

    async fn verify_execution(
        &self,
        context: &ProviderContext,
        request: &ExecutionRequest,
    ) -> ProviderResult<ExecutionOutcome> {
        match request.requested_capabilities.as_slice() {
            [TaskCapability::ResourceExecution] => {
                self.resource.verify_execution(context, request).await
            }
            _ => Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn has no non-mutating verifier for this capability combination",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use asterism_domain::{ProviderAccountId, ProviderId, RemoteState, SecretId, TaskId};
    use asterism_provider_api::{ProviderProgress, ProviderRuntimeSettingsSchema};

    use super::*;

    #[derive(Debug)]
    struct FixtureCapability {
        metadata: ProviderMetadata,
        expected: TaskCapability,
        calls: Arc<Mutex<Vec<TaskCapability>>>,
    }

    impl ProviderIdentity for FixtureCapability {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskExecutionCapability for FixtureCapability {
        async fn execute(
            &self,
            _context: &ProviderContext,
            request: &ExecutionRequest,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ExecutionOutcome> {
            if request.requested_capabilities != [self.expected] {
                return Err(ProviderError::new(
                    ProviderErrorKind::Internal,
                    "fixture received the wrong capability",
                ));
            }
            self.calls.lock().unwrap().push(self.expected);
            Ok(ExecutionOutcome {
                remote_state: if self.expected == TaskCapability::ResourceExecution {
                    RemoteState::Completed
                } else {
                    RemoteState::Pending
                },
                verified: true,
                result_sanitized: serde_json::json!({"capability": self.expected}),
            })
        }
    }

    #[derive(Debug)]
    struct FixtureEvents;

    #[async_trait]
    impl ExecutionEventSink for FixtureEvents {
        async fn report(&self, _update: ProviderProgress) -> ProviderResult<()> {
            Ok(())
        }

        async fn log(
            &self,
            _event: asterism_provider_api::ProviderExecutionLog,
        ) -> ProviderResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn combined_execution_reports_duration_before_applying_completion() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let metadata = development_metadata().unwrap();
        let resource = Arc::new(FixtureCapability {
            metadata: metadata.clone(),
            expected: TaskCapability::ResourceExecution,
            calls: calls.clone(),
        });
        let duration = Arc::new(FixtureCapability {
            metadata,
            expected: TaskCapability::DurationReport,
            calls: calls.clone(),
        });
        let execution = WellearnTaskExecution::try_new(resource, duration).unwrap();
        assert!(execution.requires_execution_verification(&[TaskCapability::ResourceExecution]));
        assert!(!execution.requires_execution_verification(&[TaskCapability::DurationReport]));
        assert!(!execution.requires_execution_verification(&[
            TaskCapability::ResourceExecution,
            TaskCapability::DurationReport,
        ]));

        let outcome = execution
            .execute(&context(), &request(), &FixtureEvents)
            .await
            .unwrap();

        assert_eq!(outcome.remote_state, RemoteState::Completed);
        assert!(outcome.verified);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ]
        );
        assert_eq!(outcome.result_sanitized["order"][0], "duration_report");
    }

    #[test]
    fn current_donor_allows_only_exact_not_open_welearn_action_sets() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let metadata = development_metadata().unwrap();
        let execution = WellearnTaskExecution::try_new(
            Arc::new(FixtureCapability {
                metadata: metadata.clone(),
                expected: TaskCapability::ResourceExecution,
                calls: calls.clone(),
            }),
            Arc::new(FixtureCapability {
                metadata,
                expected: TaskCapability::DurationReport,
                calls,
            }),
        )
        .unwrap();
        for requested in [
            vec![TaskCapability::ResourceExecution],
            vec![TaskCapability::DurationReport],
            vec![
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ],
        ] {
            assert!(execution.allows_execution_from_remote_state(&requested, RemoteState::NotOpen));
            assert!(
                !execution.allows_execution_from_remote_state(&requested, RemoteState::Expired)
            );
        }
        assert!(!execution.allows_execution_from_remote_state(
            &[TaskCapability::SubmissionExecute],
            RemoteState::NotOpen
        ));
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-combined-execution".to_owned(),
        }
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ],
            runtime_settings: ProviderRuntimeSettingsSchema::empty()
                .resolve(None, None, None)
                .unwrap(),
        }
    }
}
