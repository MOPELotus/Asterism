use std::{fmt, sync::Arc};

use asterism_domain::{RemoteState, TaskCapability};
use asterism_provider_api::{
    BatchExecutionPlanningRequest, ExecutionEventSink, ExecutionMutationSequenceRecoverySnapshot,
    ExecutionOutcome, ExecutionParentBatchSnapshot, ExecutionRecoveryOutcome, ExecutionRequest,
    PreparedProviderBatchExecutionPlan, ProviderBatchExecutionMaterializationBinding,
    ProviderContext, ProviderError, ProviderErrorKind, ProviderExecutionBatchPlan,
    ProviderIdentity, ProviderMetadata, ProviderResult, TaskExecutionCapability,
};
use asterism_secrets::SecretValue;
use async_trait::async_trait;

use crate::{
    WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE, WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE,
    WellearnAtomicDurationCompletion, WellearnAtomicDurationCompletionRecovery,
    WellearnBatchExecutionPlanner, WellearnBatchMaterializationScope,
    WellearnBatchRuntimeSettingsRevision, WellearnPublicBatchExecutionInput,
    WellearnPublicBatchMaterializationBinding, metadata::development_metadata,
    restore_batch_execution_plan,
};

const ATOMIC_BATCH_CHILD_CAPABILITIES: [TaskCapability; 2] = [
    TaskCapability::DurationReport,
    TaskCapability::ResourceExecution,
];

/// Dispatches `WELearn`'s independent `ResourceExecution` and `DurationReport`
/// capabilities through the single shared `TaskExecution` registry slot. Core
/// persists a composite plan and calls this boundary once per exact mutation
/// step; the immutable full plan remains context, never mutation authority.
pub struct WellearnTaskExecution {
    metadata: ProviderMetadata,
    resource: Arc<dyn TaskExecutionCapability>,
    duration: Arc<dyn TaskExecutionCapability>,
    batch_planner: Option<Arc<WellearnBatchExecutionPlanner>>,
    atomic_execution: Option<Arc<WellearnAtomicDurationCompletion>>,
    atomic_recovery: Option<Arc<WellearnAtomicDurationCompletionRecovery>>,
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
        Self::try_new_inner(resource, duration, None, None)
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
        Self::try_new_inner(resource, duration, Some(batch_planner), None)
    }

    /// Builds the registered dispatcher with Course-batch planning plus the
    /// parent-bound atomic child execution and read-only recovery paths.
    ///
    /// # Errors
    ///
    /// Returns an internal error when any implementation belongs to a
    /// different Provider contract.
    pub fn try_new_with_batch_runtime(
        resource: Arc<dyn TaskExecutionCapability>,
        duration: Arc<dyn TaskExecutionCapability>,
        batch_planner: Arc<WellearnBatchExecutionPlanner>,
        atomic_execution: Arc<WellearnAtomicDurationCompletion>,
        atomic_recovery: Arc<WellearnAtomicDurationCompletionRecovery>,
    ) -> ProviderResult<Self> {
        Self::try_new_inner(
            resource,
            duration,
            Some(batch_planner),
            Some((atomic_execution, atomic_recovery)),
        )
    }

    fn try_new_inner(
        resource: Arc<dyn TaskExecutionCapability>,
        duration: Arc<dyn TaskExecutionCapability>,
        batch_planner: Option<Arc<WellearnBatchExecutionPlanner>>,
        atomic_runtime: Option<(
            Arc<WellearnAtomicDurationCompletion>,
            Arc<WellearnAtomicDurationCompletionRecovery>,
        )>,
    ) -> ProviderResult<Self> {
        let metadata = development_metadata()?;
        if resource.metadata() != &metadata
            || duration.metadata() != &metadata
            || atomic_runtime
                .as_ref()
                .is_some_and(|(execution, recovery)| {
                    execution.metadata() != &metadata || recovery.metadata() != &metadata
                })
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "WELearn execution implementations have mismatched metadata",
            ));
        }
        let (atomic_execution, atomic_recovery) = atomic_runtime.unzip();
        Ok(Self {
            metadata,
            resource,
            duration,
            batch_planner,
            atomic_execution,
            atomic_recovery,
        })
    }

    /// Executes the existing parent-bound child path only after the exact
    /// public materialization binding has been revalidated.
    ///
    /// # Errors
    ///
    /// Rejects grouped-call, scope, parent, position, Unit/SCO, remote Task,
    /// settings or artifact drift before any atomic runtime I/O.
    pub async fn execute_bound_batch_child(
        &self,
        context: &ProviderContext,
        binding: &WellearnPublicBatchMaterializationBinding,
        parent: &ExecutionParentBatchSnapshot,
        position: u32,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if !self.requires_batch_execution_parent(request) {
            return Err(invalid_atomic_batch_child_dispatch());
        }
        binding.validate_child_dispatch(context, parent, position, request)?;
        let execution = self
            .atomic_execution
            .as_ref()
            .ok_or_else(atomic_batch_runtime_unavailable)?;
        execution
            .execute_bound_materialized_core_child(
                context, binding, parent, position, request, events,
            )
            .await
    }

    /// Runs the existing read-only parent-bound recovery path only after the
    /// exact public materialization binding has been revalidated.
    ///
    /// # Errors
    ///
    /// Rejects grouped-call, scope, parent, position, Unit/SCO, remote Task,
    /// settings, artifact or recovery-snapshot drift before any final HTTP read.
    pub async fn verify_bound_batch_child_recovery(
        &self,
        context: &ProviderContext,
        binding: &WellearnPublicBatchMaterializationBinding,
        parent: &ExecutionParentBatchSnapshot,
        position: u32,
        request: &ExecutionRequest,
        mutation_sequence: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        if !self.requires_batch_execution_parent(request) {
            return Err(invalid_atomic_batch_child_dispatch());
        }
        binding.validate_child_dispatch(context, parent, position, request)?;
        let recovery = self
            .atomic_recovery
            .as_ref()
            .ok_or_else(atomic_batch_runtime_unavailable)?;
        recovery
            .verify_bound_materialized_core_child_snapshot(
                context,
                binding,
                parent,
                position,
                request,
                mutation_sequence,
            )
            .await
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
            .field(
                "atomic_execution",
                &self.atomic_execution.as_ref().map(|_| "configured"),
            )
            .field(
                "atomic_recovery",
                &self.atomic_recovery.as_ref().map(|_| "configured"),
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

    fn build_batch_execution_materialization_binding(
        &self,
        context: &ProviderContext,
        request: &BatchExecutionPlanningRequest<'_>,
        prepared: &PreparedProviderBatchExecutionPlan,
    ) -> ProviderResult<Option<ProviderBatchExecutionMaterializationBinding>> {
        if context.provider_id != self.metadata.id
            || request.public_input.provider_id() != &self.metadata.id
            || request.planning_input.provider_id() != &self.metadata.id
        {
            return Err(invalid_atomic_batch_child_dispatch());
        }
        let public = WellearnPublicBatchExecutionInput::decode(
            request.public_input.input_type(),
            request.public_input.payload().expose_secret(),
        )?;
        let revision = WellearnBatchRuntimeSettingsRevision::try_new(
            request.runtime_settings_revision.schema_version(),
            request.runtime_settings_revision.provider_revision(),
            request
                .runtime_settings_revision
                .provider_account_revision(),
        )?;
        let scope = WellearnBatchMaterializationScope::try_new(
            self.metadata.id.clone(),
            context.account_id,
            request.course_id,
            revision,
            request.expected_child_count,
        )?;
        let binding = WellearnPublicBatchMaterializationBinding::try_new(
            &public,
            &scope,
            request.planning_input,
            prepared,
        )?;
        ProviderBatchExecutionMaterializationBinding::try_new(
            self.metadata.id.clone(),
            WELLEARN_PUBLIC_BATCH_MATERIALIZATION_BINDING_TYPE,
            SecretValue::new(binding.encode()?),
        )
        .map(Some)
    }

    fn restore_batch_execution_plan(
        &self,
        parent: &ExecutionParentBatchSnapshot,
    ) -> ProviderResult<ProviderExecutionBatchPlan> {
        restore_batch_execution_plan(parent)
    }

    fn requires_batch_execution_parent(&self, request: &ExecutionRequest) -> bool {
        request.has_valid_capability_step()
            && request.requested_capabilities.as_slice() == ATOMIC_BATCH_CHILD_CAPABILITIES
            && request.capability_plan.as_slice() == ATOMIC_BATCH_CHILD_CAPABILITIES
            && request.capability_step_position == 1
            && request
                .provider_plan_artifact
                .as_ref()
                .is_some_and(|artifact| {
                    artifact.provider_id() == &self.metadata.id
                        && artifact.artifact_type() == WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE
                })
    }

    fn requires_batch_execution_materialization_binding(&self, request: &ExecutionRequest) -> bool {
        self.requires_batch_execution_parent(request)
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

    async fn execute_batch_child(
        &self,
        context: &ProviderContext,
        parent: &ExecutionParentBatchSnapshot,
        materialization_binding: Option<&ProviderBatchExecutionMaterializationBinding>,
        position: u32,
        request: &ExecutionRequest,
        events: &(dyn ExecutionEventSink + Send + Sync),
    ) -> ProviderResult<ExecutionOutcome> {
        if !self.requires_batch_execution_parent(request) {
            return Err(invalid_atomic_batch_child_dispatch());
        }
        let persisted = materialization_binding
            .filter(|binding| binding.provider_id() == &self.metadata.id)
            .ok_or_else(invalid_atomic_batch_child_dispatch)?;
        let binding = WellearnPublicBatchMaterializationBinding::decode_for_child_dispatch(
            persisted.binding_type(),
            persisted.payload().expose_secret(),
            context,
            parent,
            position,
            request,
        )?;
        self.execute_bound_batch_child(context, &binding, parent, position, request, events)
            .await
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

    async fn verify_batch_child_recovery(
        &self,
        context: &ProviderContext,
        parent: &ExecutionParentBatchSnapshot,
        materialization_binding: Option<&ProviderBatchExecutionMaterializationBinding>,
        position: u32,
        request: &ExecutionRequest,
        mutation_sequence: &ExecutionMutationSequenceRecoverySnapshot,
    ) -> ProviderResult<ExecutionRecoveryOutcome> {
        if !self.requires_batch_execution_parent(request) {
            return Err(invalid_atomic_batch_child_dispatch());
        }
        let persisted = materialization_binding
            .filter(|binding| binding.provider_id() == &self.metadata.id)
            .ok_or_else(invalid_atomic_batch_child_dispatch)?;
        let binding = WellearnPublicBatchMaterializationBinding::decode_for_child_dispatch(
            persisted.binding_type(),
            persisted.payload().expose_secret(),
            context,
            parent,
            position,
            request,
        )?;
        self.verify_bound_batch_child_recovery(
            context,
            &binding,
            parent,
            position,
            request,
            mutation_sequence,
        )
        .await
    }
}

fn invalid_atomic_batch_child_dispatch() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic child dispatch is detached from its grouped parent authority",
    )
}

fn atomic_batch_runtime_unavailable() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic child runtime is not configured",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use asterism_domain::{ProviderAccountId, ProviderId, RemoteState, SecretId, TaskId};
    use asterism_provider_api::{
        ProviderExecutionPlanArtifact, ProviderProgress, ProviderRuntimeSettingsSchema,
    };
    use asterism_secrets::SecretValue;

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

    #[tokio::test]
    async fn atomic_child_requires_parent_and_never_falls_back_to_singleton_dispatch() {
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
                calls: calls.clone(),
            }),
        )
        .unwrap();
        let request = atomic_request();
        assert!(execution.requires_batch_execution_parent(&request));

        let mut missing_artifact = request.clone();
        missing_artifact.provider_plan_artifact = None;
        assert!(!execution.requires_batch_execution_parent(&missing_artifact));
        let mut split_step = request.clone();
        split_step.requested_capabilities = vec![TaskCapability::DurationReport];
        assert!(!execution.requires_batch_execution_parent(&split_step));

        let singleton_error = execution
            .execute(&context(), &request, &FixtureEvents)
            .await
            .unwrap_err();
        assert_eq!(singleton_error.kind, ProviderErrorKind::UnsupportedTask);
        assert!(calls.lock().unwrap().is_empty());

        let parent = ExecutionParentBatchSnapshot::try_new(
            ProviderId::new("welearn").unwrap(),
            "welearn.fixture-authority.v1",
            SecretValue::new(vec![1]),
            "welearn.fixture-batch.v1",
            SecretValue::new(vec![2]),
        )
        .unwrap();
        let batch_error = execution
            .execute_batch_child(&context(), &parent, None, 1, &request, &FixtureEvents)
            .await
            .unwrap_err();
        assert_eq!(batch_error.kind, ProviderErrorKind::Internal);
        assert!(calls.lock().unwrap().is_empty());
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

    fn atomic_request() -> ExecutionRequest {
        ExecutionRequest {
            execution_id: asterism_domain::ExecutionId::new(),
            task_id: TaskId::new(),
            remote_task_id: "sco:1001:301".to_owned(),
            course_id: Some(asterism_domain::CourseId::new()),
            requested_capabilities: ATOMIC_BATCH_CHILD_CAPABILITIES.to_vec(),
            capability_plan: ATOMIC_BATCH_CHILD_CAPABILITIES.to_vec(),
            capability_step_position: 1,
            runtime_settings: ProviderRuntimeSettingsSchema::empty()
                .resolve(None, None, None)
                .unwrap(),
            provider_plan_artifact: Some(
                ProviderExecutionPlanArtifact::try_new(
                    ProviderId::new("welearn").unwrap(),
                    WELLEARN_ATOMIC_CHILD_PLAN_ARTIFACT_TYPE,
                    serde_json::json!({"fixture": true}),
                )
                .unwrap(),
            ),
        }
    }
}
