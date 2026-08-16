use std::sync::Arc;

use asterism_domain::{
    BatchExecution, BatchExecutionAttempt, BatchExecutionId, ExecutionState, ProviderAccountId,
    ProviderId, ScheduleId, Timestamp,
};
use asterism_provider_api::{
    BatchExecutionPlanningRequest, PreparedProviderBatchExecutionPlan, ProviderContext,
    ProviderError, ProviderExecutionBatchPlan, ProviderRegistry, ProviderRuntimeSettingsSchema,
};
use asterism_secrets::{SecretAccess, SecretActor, SecretStoreError};
use asterism_storage::{
    BatchExecutionAttemptStartRequest, BatchExecutionChildPlanMaterializeOutcome,
    BatchExecutionChildPlanMaterializeRequest, BatchExecutionChildPlanRecord,
    BatchExecutionChildPlanRepository, BatchExecutionParentSnapshotBindOutcome,
    BatchExecutionParentSnapshotBindRequest, BatchExecutionParentSnapshotRepositoryFactory,
    BatchExecutionParentSnapshotResolveRequest, BatchExecutionPlanningInputRepository,
    BatchExecutionPlanningInputResolveRequest, BatchExecutionRepository,
    BatchExecutionRuntimeSettingsBindOutcome, BatchExecutionRuntimeSettingsBindRequest,
    BatchExecutionRuntimeSettingsRepository, BatchExecutionRuntimeSettingsResolveRequest,
    CourseRuntimeRepository, ExecutionRuntimeSettingsSnapshot, ProviderAccountRuntimeRepository,
    ProviderRuntimeSettingsRepository, ProviderRuntimeSettingsTarget, StorageError,
};

#[derive(Clone, Debug)]
pub struct PlanBatchExecutionCommand {
    pub batch_execution_id: BatchExecutionId,
    pub scheduler_job_id: ScheduleId,
    pub worker_id: String,
    pub correlation_id: String,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchExecutionPlanningResult {
    pub batch_execution: BatchExecution,
    pub attempt: BatchExecutionAttempt,
    pub execution_batch_plan: ProviderExecutionBatchPlan,
    pub child_plans: Vec<BatchExecutionChildPlanRecord>,
    pub runtime_settings: ExecutionRuntimeSettingsSnapshot,
    pub planned_fresh: bool,
}

/// Claim-bound Course parent planner. It may call a Provider exactly once to
/// obtain fresh read-only batch evidence, but Core alone resolves encrypted
/// input, binds the private parent pair and later materializes children.
pub struct BatchExecutionPlanningService {
    batches: Arc<dyn BatchExecutionRepository>,
    planning_inputs: Arc<dyn BatchExecutionPlanningInputRepository>,
    child_plans: Arc<dyn BatchExecutionChildPlanRepository>,
    batch_settings: Arc<dyn BatchExecutionRuntimeSettingsRepository>,
    accounts: Arc<dyn ProviderAccountRuntimeRepository>,
    courses: Arc<dyn CourseRuntimeRepository>,
    settings: Arc<dyn ProviderRuntimeSettingsRepository>,
    parent_snapshots: Arc<dyn BatchExecutionParentSnapshotRepositoryFactory>,
    providers: Arc<ProviderRegistry>,
}

impl std::fmt::Debug for BatchExecutionPlanningService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BatchExecutionPlanningService")
            .field("providers", &self.providers.len())
            .finish_non_exhaustive()
    }
}

impl BatchExecutionPlanningService {
    #[allow(
        clippy::too_many_arguments,
        reason = "the planner keeps independently mockable Core repository and Provider registry boundaries"
    )]
    pub fn new(
        batches: Arc<dyn BatchExecutionRepository>,
        planning_inputs: Arc<dyn BatchExecutionPlanningInputRepository>,
        child_plans: Arc<dyn BatchExecutionChildPlanRepository>,
        batch_settings: Arc<dyn BatchExecutionRuntimeSettingsRepository>,
        accounts: Arc<dyn ProviderAccountRuntimeRepository>,
        courses: Arc<dyn CourseRuntimeRepository>,
        settings: Arc<dyn ProviderRuntimeSettingsRepository>,
        parent_snapshots: Arc<dyn BatchExecutionParentSnapshotRepositoryFactory>,
        providers: Arc<ProviderRegistry>,
    ) -> Self {
        Self {
            batches,
            planning_inputs,
            child_plans,
            batch_settings,
            accounts,
            courses,
            settings,
            parent_snapshots,
            providers,
        }
    }

    /// Starts or reuses the exact parent Attempt, resolves encrypted planning
    /// input, invokes fresh Provider planning once and durably binds the private
    /// parent pair. A restart restores the public ordered plan only from that
    /// pair and never performs a second fresh scan.
    ///
    /// # Errors
    ///
    /// Fails for lost claims, account/Course/Provider drift, invalid runtime
    /// settings, encrypted-state failures, Provider errors or a fresh plan that
    /// differs from the Provider's deterministic parent-only reconstruction.
    #[allow(
        clippy::too_many_lines,
        reason = "claim, encrypted input, fresh planning, deterministic restore and parent bind stay adjacent as one auditable orchestration boundary"
    )]
    pub async fn plan(
        &self,
        command: PlanBatchExecutionCommand,
    ) -> Result<BatchExecutionPlanningResult, BatchExecutionPlanningError> {
        validate_command(&command)?;
        let attempt = self
            .batches
            .start_batch_execution_attempt(BatchExecutionAttemptStartRequest {
                batch_execution_id: command.batch_execution_id,
                scheduler_job_id: command.scheduler_job_id,
                worker_id: &command.worker_id,
                at: command.at,
                correlation_id: &command.correlation_id,
            })
            .await?;
        let batch = self
            .batches
            .find_batch_execution(command.batch_execution_id)
            .await?
            .filter(|batch| {
                batch.state == ExecutionState::Running
                    && batch.id == attempt.batch_execution_id
                    && batch.started_at.is_some()
            })
            .ok_or(BatchExecutionPlanningError::ParentBindingConflict)?;
        let account = self
            .accounts
            .find_runtime_provider_account(batch.provider_account_id)
            .await?
            .filter(|account| account.id == batch.provider_account_id)
            .ok_or(BatchExecutionPlanningError::ParentBindingConflict)?;
        let course = self
            .courses
            .find_runtime_course(batch.course_id)
            .await?
            .filter(|course| {
                course.id == batch.course_id
                    && course.provider_account_id == batch.provider_account_id
            })
            .ok_or(BatchExecutionPlanningError::ParentBindingConflict)?;
        let provider = self
            .providers
            .get(&account.provider_id)
            .ok_or(BatchExecutionPlanningError::ProviderUnavailable)?;
        let capability = provider
            .task_execution
            .as_ref()
            .ok_or(BatchExecutionPlanningError::ProviderUnavailable)?;
        let runtime_settings = self
            .freeze_runtime_settings(
                &command,
                &batch,
                &attempt,
                &account.provider_id,
                account.id,
                &provider.runtime_settings,
            )
            .await?;
        let access = SecretAccess {
            actor: SecretActor::CoreService("batch-execution-planner"),
            correlation_id: command.correlation_id.clone(),
            reason: "resolve immutable Course batch planning state".to_owned(),
        };
        let parent_repository = self
            .parent_snapshots
            .for_provider(account.provider_id.clone());
        if let Some(resolved) = parent_repository
            .resolve_batch_execution_parent_snapshot(BatchExecutionParentSnapshotResolveRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: command.scheduler_job_id,
                worker_id: &command.worker_id,
                correlation_id: &command.correlation_id,
                at: command.at,
                access: &access,
            })
            .await?
        {
            let restored = capability.restore_batch_execution_plan(&resolved.snapshot)?;
            let prepared = PreparedProviderBatchExecutionPlan::try_new(
                resolved.snapshot,
                restored,
                batch.expected_child_count,
            )?;
            let (_, execution_batch_plan) = prepared.into_parts();
            let child_plans = self
                .materialize_child_plans(&command, &batch, &attempt, &execution_batch_plan)
                .await?;
            return Ok(BatchExecutionPlanningResult {
                batch_execution: batch,
                attempt,
                execution_batch_plan,
                child_plans,
                runtime_settings,
                planned_fresh: false,
            });
        }

        let resolved_input = self
            .planning_inputs
            .resolve_batch_execution_planning_input(BatchExecutionPlanningInputResolveRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: command.scheduler_job_id,
                worker_id: &command.worker_id,
                correlation_id: &command.correlation_id,
                at: command.at,
                access: &access,
            })
            .await?;
        if resolved_input.metadata.provider_id != account.provider_id
            || resolved_input.input.provider_id() != &account.provider_id
        {
            return Err(BatchExecutionPlanningError::ParentBindingConflict);
        }
        let context = ProviderContext {
            provider_id: account.provider_id.clone(),
            account_id: account.id,
            credential_refs: account.credential_refs,
            correlation_id: command.correlation_id.clone(),
        };
        let prepared = capability
            .prepare_batch_execution_plan(
                &context,
                &BatchExecutionPlanningRequest {
                    batch_execution_id: batch.id,
                    attempt_id: attempt.id,
                    course_id: course.id,
                    remote_course_id: &course.remote_id,
                    requested_capabilities: &batch.requested_capabilities,
                    expected_child_count: batch.expected_child_count,
                    runtime_settings: &runtime_settings.resolved,
                    planning_input: &resolved_input.input,
                },
            )
            .await?;
        let restored = capability.restore_batch_execution_plan(prepared.parent_snapshot())?;
        if &restored != prepared.execution_batch_plan() {
            return Err(BatchExecutionPlanningError::DeterministicPlanMismatch);
        }
        let (parent_snapshot, execution_batch_plan) = prepared.into_parts();
        let outcome = parent_repository
            .bind_batch_execution_parent_snapshot(BatchExecutionParentSnapshotBindRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: command.scheduler_job_id,
                worker_id: &command.worker_id,
                snapshot: parent_snapshot,
                correlation_id: &command.correlation_id,
                at: command.at,
                access: &access,
            })
            .await?;
        match outcome {
            BatchExecutionParentSnapshotBindOutcome::Bound(_)
            | BatchExecutionParentSnapshotBindOutcome::AlreadyBound(_) => {}
        }
        let child_plans = self
            .materialize_child_plans(&command, &batch, &attempt, &execution_batch_plan)
            .await?;
        Ok(BatchExecutionPlanningResult {
            batch_execution: batch,
            attempt,
            execution_batch_plan,
            child_plans,
            runtime_settings,
            planned_fresh: true,
        })
    }

    async fn materialize_child_plans(
        &self,
        command: &PlanBatchExecutionCommand,
        batch: &BatchExecution,
        attempt: &BatchExecutionAttempt,
        execution_batch_plan: &ProviderExecutionBatchPlan,
    ) -> Result<Vec<BatchExecutionChildPlanRecord>, BatchExecutionPlanningError> {
        let outcome = self
            .child_plans
            .materialize_batch_execution_child_plans(BatchExecutionChildPlanMaterializeRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: command.scheduler_job_id,
                worker_id: &command.worker_id,
                execution_batch_plan,
                correlation_id: &command.correlation_id,
                at: command.at,
            })
            .await?;
        Ok(match outcome {
            BatchExecutionChildPlanMaterializeOutcome::Created(records)
            | BatchExecutionChildPlanMaterializeOutcome::Existing(records) => records,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the frozen snapshot binds the exact parent, account, Provider schema and live worker claim"
    )]
    async fn freeze_runtime_settings(
        &self,
        command: &PlanBatchExecutionCommand,
        batch: &BatchExecution,
        attempt: &BatchExecutionAttempt,
        provider_id: &ProviderId,
        provider_account_id: ProviderAccountId,
        schema: &ProviderRuntimeSettingsSchema,
    ) -> Result<ExecutionRuntimeSettingsSnapshot, BatchExecutionPlanningError> {
        let claim = BatchExecutionRuntimeSettingsResolveRequest {
            batch_execution_id: batch.id,
            attempt_id: attempt.id,
            scheduler_job_id: command.scheduler_job_id,
            worker_id: &command.worker_id,
            at: command.at,
        };
        if let Some(snapshot) = self
            .batch_settings
            .find_batch_execution_runtime_settings(claim)
            .await?
        {
            if snapshot.provider_id != *provider_id
                || snapshot.task_revision.is_some()
                || schema.validate_resolved(&snapshot.resolved).is_err()
            {
                return Err(BatchExecutionPlanningError::RuntimeSettingsInvalid);
            }
            return Ok(snapshot);
        }
        let provider_settings = self
            .settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::Provider {
                provider_id: provider_id.clone(),
            })
            .await?;
        let account_settings = self
            .settings
            .find_provider_runtime_settings(&ProviderRuntimeSettingsTarget::ProviderAccount {
                provider_id: provider_id.clone(),
                provider_account_id,
            })
            .await?;
        let (resolved, sources) = schema
            .resolve_with_sources(
                provider_settings.as_ref().map(|record| &record.patch),
                account_settings.as_ref().map(|record| &record.patch),
                None,
            )
            .map_err(|_| BatchExecutionPlanningError::RuntimeSettingsInvalid)?;
        let completion_policy = schema
            .completion_policy_snapshot(&resolved, command.at)
            .map_err(|_| BatchExecutionPlanningError::RuntimeSettingsInvalid)?;
        let candidate = ExecutionRuntimeSettingsSnapshot {
            provider_id: provider_id.clone(),
            resolved,
            sources,
            completion_policy,
            provider_revision: provider_settings.as_ref().map(|record| record.revision),
            provider_account_revision: account_settings.as_ref().map(|record| record.revision),
            task_revision: None,
            captured_at: command.at,
        };
        let outcome = self
            .batch_settings
            .bind_batch_execution_runtime_settings(BatchExecutionRuntimeSettingsBindRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: command.scheduler_job_id,
                worker_id: &command.worker_id,
                correlation_id: &command.correlation_id,
                snapshot: &candidate,
                schema,
                at: command.at,
            })
            .await?;
        Ok(match outcome {
            BatchExecutionRuntimeSettingsBindOutcome::Bound(snapshot)
            | BatchExecutionRuntimeSettingsBindOutcome::Existing(snapshot) => snapshot,
        })
    }
}

fn validate_command(
    command: &PlanBatchExecutionCommand,
) -> Result<(), BatchExecutionPlanningError> {
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    if valid(&command.worker_id) && valid(&command.correlation_id) {
        Ok(())
    } else {
        Err(BatchExecutionPlanningError::InvalidCommand)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BatchExecutionPlanningError {
    #[error("batch execution planning command is invalid")]
    InvalidCommand,
    #[error("batch execution parent, account or Course binding changed")]
    ParentBindingConflict,
    #[error("the bound Provider has no Course batch planning runtime")]
    ProviderUnavailable,
    #[error("Provider runtime settings are invalid")]
    RuntimeSettingsInvalid,
    #[error("fresh Provider batch output differs from parent-only reconstruction")]
    DeterministicPlanMismatch,
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Secret(#[from] SecretStoreError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    use asterism_domain::{
        AuthMethod, AuthState, CourseId, ProviderAccountId, ProviderId, RequestSource, SessionKind,
        TaskCapability, UserId,
    };
    use asterism_provider_api::{
        ExecutionEventSink, ExecutionMutationSequenceAdvanceCondition,
        ExecutionMutationSequencePhase, ExecutionMutationSequencePlan, ExecutionOutcome,
        ExecutionParentBatchSnapshot, ExecutionRequest, ProviderCapability, ProviderEntry,
        ProviderErrorKind, ProviderExecutionChildPlan, ProviderExecutionPlan,
        ProviderExecutionPlanArtifact, ProviderIdentity, ProviderMetadata, ProviderResult,
        ProviderRuntimeSettingsSchema, TaskExecutionCapability, VerificationLevel,
    };
    use asterism_secrets::{SecretKey, SecretValue};
    use asterism_storage::{
        BatchExecutionRepository, BatchExecutionScheduleOutcome, BatchExecutionScheduleRequest,
        Database, SchedulerRepository, SecretKeyring, SqliteBatchExecutionRepository,
        SqliteCourseProgressRepository, SqliteProviderAccountRepository,
        SqliteProviderRuntimeSettingsRepository, SqliteSchedulerRepository, SqliteSecretStore,
    };
    use async_trait::async_trait;
    use chrono::{Duration, Utc};

    #[derive(Debug)]
    struct FakeBatchProvider {
        metadata: ProviderMetadata,
        fresh_calls: Arc<AtomicUsize>,
    }

    impl ProviderIdentity for FakeBatchProvider {
        fn metadata(&self) -> &ProviderMetadata {
            &self.metadata
        }
    }

    #[async_trait]
    impl TaskExecutionCapability for FakeBatchProvider {
        async fn prepare_batch_execution_plan(
            &self,
            _context: &ProviderContext,
            request: &BatchExecutionPlanningRequest<'_>,
        ) -> ProviderResult<PreparedProviderBatchExecutionPlan> {
            if request.remote_course_id != "remote-course"
                || request.expected_child_count != 2
                || request.planning_input.payload().expose_secret() != b"PRIVATE_SELECTION"
            {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "fake batch planning request drifted",
                ));
            }
            self.fresh_calls.fetch_add(1, Ordering::SeqCst);
            let parent = fake_parent()?;
            let plan = fake_plan(&parent)?;
            PreparedProviderBatchExecutionPlan::try_new(parent, plan, 2)
        }

        fn restore_batch_execution_plan(
            &self,
            parent: &ExecutionParentBatchSnapshot,
        ) -> ProviderResult<ProviderExecutionBatchPlan> {
            if parent.provider_id() != &self.metadata.id
                || parent.authority_type() != "test-batch.parent-authority.v1"
                || parent.batch_type() != "test-batch.complete-batch.v1"
                || parent.authority().expose_secret() != b"PARENT_AUTHORITY"
                || parent.batch().expose_secret() != b"COMPLETE_BATCH"
            {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidResponse,
                    "fake parent snapshot drifted",
                ));
            }
            fake_plan(parent)
        }

        async fn execute(
            &self,
            _context: &ProviderContext,
            _request: &ExecutionRequest,
            _events: &(dyn ExecutionEventSink + Send + Sync),
        ) -> ProviderResult<ExecutionOutcome> {
            Err(ProviderError::new(
                ProviderErrorKind::UnsupportedTask,
                "fake batch provider has no single-task executor",
            ))
        }
    }

    fn fake_parent() -> ProviderResult<ExecutionParentBatchSnapshot> {
        ExecutionParentBatchSnapshot::try_new(
            ProviderId::new("test-batch").unwrap(),
            "test-batch.parent-authority.v1",
            SecretValue::new(b"PARENT_AUTHORITY".to_vec()),
            "test-batch.complete-batch.v1",
            SecretValue::new(b"COMPLETE_BATCH".to_vec()),
        )
    }

    fn fake_plan(
        parent: &ExecutionParentBatchSnapshot,
    ) -> ProviderResult<ProviderExecutionBatchPlan> {
        let children = (1_u32..=2)
            .map(|position| {
                let provider_id = ProviderId::new("test-batch").unwrap();
                let artifact = ProviderExecutionPlanArtifact::try_new(
                    provider_id.clone(),
                    "test-batch.child.v1",
                    serde_json::json!({"position": position}),
                )?;
                let sequence = ExecutionMutationSequencePlan::try_new(
                    artifact.artifact_digest(),
                    "test-batch.sequence.v1",
                    vec![ExecutionMutationSequencePhase::try_new(
                        "test-batch.atomic.v1",
                        1,
                        1,
                        false,
                        ExecutionMutationSequenceAdvanceCondition::MaximumReached,
                        None,
                    )?],
                )?;
                let execution_plan = ProviderExecutionPlan::try_new(
                    provider_id,
                    vec![vec![
                        TaskCapability::DurationReport,
                        TaskCapability::ResourceExecution,
                    ]],
                    Some(artifact),
                )?;
                ProviderExecutionChildPlan::try_new(
                    position,
                    format!("remote-task-{position}"),
                    execution_plan,
                    sequence,
                )
            })
            .collect::<ProviderResult<Vec<_>>>()?;
        ProviderExecutionBatchPlan::try_new(parent, children)
    }

    fn metadata() -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("test-batch").unwrap(),
            display_name: "Test Batch".to_owned(),
            implementation_version: "test".to_owned(),
            verification: VerificationLevel::Development,
            scan_min_interval_seconds: None,
            capture_recipe_version: None,
            capabilities: BTreeSet::from([
                ProviderCapability::ResourceExecution,
                ProviderCapability::DurationReport,
            ]),
            auth_methods: BTreeSet::<AuthMethod>::new(),
            session_kinds: BTreeSet::<SessionKind>::new(),
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration fixture proves scheduling, encrypted input, fresh planning, parent binding and restart-only restore together"
    )]
    async fn fresh_planning_binds_parent_and_restart_restores_without_rescan() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner = UserId::new();
        let account_id = ProviderAccountId::new();
        let course_id = CourseId::new();
        insert_fixture(&database, owner, account_id, course_id, now).await;
        let keyring = Arc::new(
            SecretKeyring::new(
                "batch-planner-key",
                BTreeMap::from([("batch-planner-key".to_owned(), SecretKey::new([43; 32]))]),
            )
            .unwrap(),
        );
        let batch_repository = Arc::new(SqliteBatchExecutionRepository::new(
            database.clone(),
            keyring.clone(),
        ));
        let planning_input = asterism_provider_api::ProviderBatchExecutionPlanningInput::try_new(
            ProviderId::new("test-batch").unwrap(),
            "test-batch.request.v1",
            SecretValue::new(b"PRIVATE_SELECTION".to_vec()),
        )
        .unwrap();
        let batch = BatchExecution {
            id: BatchExecutionId::new(),
            provider_account_id: account_id,
            course_id,
            requested_capabilities: vec![
                TaskCapability::ResourceExecution,
                TaskCapability::DurationReport,
            ],
            expected_child_count: 2,
            requested_by: Some(owner),
            request_source: RequestSource::WebUi,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        };
        assert_eq!(
            batch_repository
                .schedule_batch_execution(BatchExecutionScheduleRequest {
                    batch_execution: &batch,
                    planning_input: &planning_input,
                    idempotency_scope: "user:test-owner",
                    idempotency_key: "batch-plan-once",
                    actor: asterism_domain::AuditActor::User(owner),
                    correlation_id: "batch-plan-request",
                })
                .await
                .unwrap(),
            BatchExecutionScheduleOutcome::Created(batch.clone())
        );
        let scheduler = SqliteSchedulerRepository::new(database.clone());
        let claimed = scheduler
            .claim_due_batch_execution_jobs("batch-worker", now, now + Duration::minutes(5), 1)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);

        let fresh_calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(FakeBatchProvider {
            metadata: metadata(),
            fresh_calls: fresh_calls.clone(),
        });
        let mut registry = ProviderRegistry::default();
        registry
            .register(ProviderEntry {
                metadata: provider.metadata.clone(),
                runtime_settings: ProviderRuntimeSettingsSchema::default(),
                authentication: None,
                course_inventory: None,
                task_inventory: None,
                task_detail: None,
                task_progress: None,
                duration_read: None,
                question_inventory: None,
                question_parse: None,
                answer_resolve: None,
                submission_build: None,
                submission_execute: None,
                submission_verify: None,
                answer_history_harvest: None,
                task_execution: Some(provider),
                browser_bridge: None,
            })
            .unwrap();
        let secret_store = Arc::new(SqliteSecretStore::new(database.clone(), keyring));
        let service = BatchExecutionPlanningService::new(
            batch_repository.clone(),
            batch_repository.clone(),
            batch_repository.clone(),
            batch_repository,
            Arc::new(SqliteProviderAccountRepository::new(database.clone())),
            Arc::new(SqliteCourseProgressRepository::new(database.clone())),
            Arc::new(SqliteProviderRuntimeSettingsRepository::new(
                database.clone(),
            )),
            secret_store,
            Arc::new(registry),
        );
        let command = PlanBatchExecutionCommand {
            batch_execution_id: batch.id,
            scheduler_job_id: claimed[0].id,
            worker_id: "batch-worker".to_owned(),
            correlation_id: "batch-plan-worker".to_owned(),
            at: now + Duration::seconds(1),
        };
        let first = service.plan(command.clone()).await.unwrap();
        assert!(first.planned_fresh);
        assert_eq!(first.execution_batch_plan.children().len(), 2);
        assert_eq!(first.child_plans.len(), 2);
        assert_eq!(first.child_plans[0].position, 1);
        assert_eq!(first.child_plans[1].position, 2);
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);
        let restored = service.plan(command.clone()).await.unwrap();
        assert!(!restored.planned_fresh);
        assert_eq!(restored.execution_batch_plan, first.execution_batch_plan);
        assert_eq!(restored.child_plans, first.child_plans);
        assert_eq!(restored.attempt, first.attempt);
        assert_eq!(restored.runtime_settings, first.runtime_settings);
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM batch_execution_child_plan_phases \
                 WHERE batch_execution_id = ?",
            )
            .bind(batch.id.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM executions")
                .fetch_one(database.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM batch_execution_runtime_settings \
                 WHERE batch_execution_id = ?",
            )
            .bind(batch.id.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM batch_execution_parent_snapshots \
                 WHERE batch_execution_id = ?",
            )
            .bind(batch.id.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );
        sqlx::query(
            "UPDATE batch_execution_child_plans SET artifact_digest = zeroblob(32) \
             WHERE batch_execution_id = ? AND position = 2",
        )
        .bind(batch.id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        assert!(matches!(
            service.plan(command).await,
            Err(BatchExecutionPlanningError::Storage(
                StorageError::BatchExecutionStateConflict
            ))
        ));
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);
    }

    async fn insert_fixture(
        database: &Database,
        owner: UserId,
        account_id: ProviderAccountId,
        course_id: CourseId,
        now: Timestamp,
    ) {
        let at = now.to_rfc3339();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'batch-owner', 'hash', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(&at)
        .bind(&at)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'test-batch', 'Test Batch', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(&at)
        .bind(&at)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO courses \
             (id, provider_account_id, remote_id, title, metadata_json, last_seen_at) \
             VALUES (?, ?, 'remote-course', 'Course', '{}', ?)",
        )
        .bind(course_id.to_string())
        .bind(account_id.to_string())
        .bind(&at)
        .execute(database.pool())
        .await
        .unwrap();
        for position in 1_u32..=2 {
            sqlx::query(
                "INSERT INTO tasks \
                 (id, provider_account_id, course_id, remote_id, remote_fingerprint, source_type, \
                  assessment_class, title, remote_state, orchestration_state, discovered_at, \
                  updated_at, capabilities_json) \
                 VALUES (?, ?, ?, ?, ?, 'resource', 'routine', ?, 'pending', 'ready', ?, ?, ?)",
            )
            .bind(asterism_domain::TaskId::new().to_string())
            .bind(account_id.to_string())
            .bind(course_id.to_string())
            .bind(format!("remote-task-{position}"))
            .bind(format!("remote-task-fingerprint-{position}"))
            .bind(format!("Task {position}"))
            .bind(&at)
            .bind(&at)
            .bind(
                serde_json::to_string(&vec![
                    TaskCapability::ResourceExecution,
                    TaskCapability::DurationReport,
                ])
                .unwrap(),
            )
            .execute(database.pool())
            .await
            .unwrap();
        }
    }
}
