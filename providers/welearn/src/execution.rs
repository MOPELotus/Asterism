use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    BatchExecutionPlanningRequest, ExecutionEventSink, ExecutionOutcome,
    ExecutionParentBatchSnapshot, ExecutionRequest, PreparedProviderBatchExecutionPlan,
    ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionBatchPlan,
    ProviderIdentity, ProviderMetadata, ProviderResult, TaskExecutionCapability,
};
use async_trait::async_trait;

use crate::{
    WellearnBatchExecutionPlanner, metadata::development_metadata, restore_batch_execution_plan,
};

/// Dispatches `WELearn`'s independent `ResourceExecution` and `DurationReport`
/// capabilities through the single shared `TaskExecution` registry slot. Core
/// persists a composite plan and calls this boundary once per exact mutation
/// step; the immutable full plan remains context, never mutation authority.
pub struct WellearnTaskExecution {
    metadata: ProviderMetadata,
    resource: Arc<dyn TaskExecutionCapability>,
    duration: Arc<dyn TaskExecutionCapability>,
    batch_planner: Option<Arc<WellearnBatchExecutionPlanner>>,
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
        Self::try_new_inner(resource, duration, None)
    }

    /// Builds the registered dispatcher with fresh Course batch planning.
    ///
    /// # Errors
    ///
    /// Returns an internal error when any implementation belongs to a
    /// different Provider contract.
    pub fn try_new_with_batch_planner(
        resource: Arc<dyn TaskExecutionCapability>,
        duration: Arc<dyn TaskExecutionCapability>,
        batch_planner: Arc<WellearnBatchExecutionPlanner>,
    ) -> ProviderResult<Self> {
        Self::try_new_inner(resource, duration, Some(batch_planner))
    }

    fn try_new_inner(
        resource: Arc<dyn TaskExecutionCapability>,
        duration: Arc<dyn TaskExecutionCapability>,
        batch_planner: Option<Arc<WellearnBatchExecutionPlanner>>,
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
            batch_planner,
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
            .field(
                "batch_planner",
                &self.batch_planner.as_ref().map(|_| "configured"),
            )
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
    async fn prepare_batch_execution_plan(
        &self,
        context: &ProviderContext,
        request: &BatchExecutionPlanningRequest<'_>,
    ) -> ProviderResult<PreparedProviderBatchExecutionPlan> {
        let planner = self.batch_planner.as_ref().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "WELearn batch execution planner is not configured",
            )
        })?;
        planner.prepare(context, request).await
    }

    fn restore_batch_execution_plan(
        &self,
        parent: &ExecutionParentBatchSnapshot,
    ) -> ProviderResult<ProviderExecutionBatchPlan> {
        restore_batch_execution_plan(parent)
    }

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
        if !request.has_valid_capability_step() {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn execution received an invalid capability step binding",
            ));
        }
        match request.requested_capabilities.as_slice() {
            [TaskCapability::ResourceExecution] => {
                self.resource.execute(context, request, events).await
            }
            [TaskCapability::DurationReport] => {
                self.duration.execute(context, request, events).await
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
        if !request.has_valid_capability_step() {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn verification received an invalid capability step binding",
            ));
        }
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
    async fn composite_plan_dispatches_only_each_core_authorized_step() {
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

        assert_eq!(
            execution
                .execution_plan(&[
                    TaskCapability::ResourceExecution,
                    TaskCapability::DurationReport,
                ])
                .unwrap(),
            [
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ]
        );
        let duration = execution
            .execute(
                &context(),
                &request(TaskCapability::DurationReport, 1),
                &FixtureEvents,
            )
            .await
            .unwrap();
        let resource = execution
            .execute(
                &context(),
                &request(TaskCapability::ResourceExecution, 2),
                &FixtureEvents,
            )
            .await
            .unwrap();

        assert_eq!(duration.remote_state, RemoteState::Pending);
        assert_eq!(resource.remote_state, RemoteState::Completed);
        assert!(duration.verified && resource.verified);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ]
        );
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

    #[test]
    fn verified_incomplete_duration_has_no_evidenced_completion_diagnosis() {
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
        let request = request(TaskCapability::DurationReport, 1);

        for remote_state in [RemoteState::Pending, RemoteState::InProgress] {
            let outcome = ExecutionOutcome {
                remote_state,
                verified: true,
                result_sanitized: serde_json::json!({
                    "schema": "welearn.duration-report.v1",
                    "completion_preserved": true,
                    "progress_preserved": true,
                    "score_preserved": true,
                    "duration_observation_changed": true,
                }),
            };
            assert_eq!(execution.completion_diagnosis(&request, &outcome), None);
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            provider_id: ProviderId::new("welearn").unwrap(),
            account_id: ProviderAccountId::new(),
            credential_refs: vec![SecretId::new()],
            correlation_id: "welearn-combined-execution".to_owned(),
        }
    }

    fn request(capability: TaskCapability, position: u8) -> ExecutionRequest {
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: None,
            requested_capabilities: vec![capability],
            capability_plan: vec![
                TaskCapability::DurationReport,
                TaskCapability::ResourceExecution,
            ],
            capability_step_position: position,
            runtime_settings: ProviderRuntimeSettingsSchema::empty()
                .resolve(None, None, None)
                .unwrap(),
            provider_plan_artifact: None,
        }
    }
}
