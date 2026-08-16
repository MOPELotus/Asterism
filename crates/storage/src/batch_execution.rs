use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AttemptResult, AuditActor, AuditRecordId, BatchExecution, BatchExecutionAttempt,
    BatchExecutionAttemptId, BatchExecutionId, CreditReservationState, ExecutionId, ExecutionState,
    ProviderAccountId, ProviderErrorClass, ProviderId, ScheduleId, SecretId, TaskCapability,
    TaskId, Timestamp, UserId,
};
use asterism_events::{DomainEvent, EventEnvelope};
use asterism_provider_api::{
    ExecutionMutationSequenceAdvanceCondition, ProviderBatchExecutionPlanningInput,
    ProviderExecutionBatchPlan, ProviderExecutionPlanArtifact, ProviderRuntimeSettingSource,
    ProviderRuntimeSettingsSchema, ProviderSettingValue, ResolvedProviderRuntimeSettings,
};
use asterism_scheduler::ScheduledJobKind;
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{
    BatchExecutionAttemptStartRequest, BatchExecutionChildActivationBilling,
    BatchExecutionChildActivationOutcome, BatchExecutionChildActivationRecord,
    BatchExecutionChildActivationRepository, BatchExecutionChildActivationRequest,
    BatchExecutionChildExecutionCreateOutcome, BatchExecutionChildExecutionCreateRequest,
    BatchExecutionChildExecutionRecord, BatchExecutionChildExecutionRepository,
    BatchExecutionChildPlanMaterializeOutcome, BatchExecutionChildPlanMaterializeRequest,
    BatchExecutionChildPlanRecord, BatchExecutionChildPlanRepository,
    BatchExecutionPlanningInputRecord, BatchExecutionPlanningInputRepository,
    BatchExecutionPlanningInputResolveRequest, BatchExecutionRepository,
    BatchExecutionRuntimeSettingsBindOutcome, BatchExecutionRuntimeSettingsBindRequest,
    BatchExecutionRuntimeSettingsRepository, BatchExecutionRuntimeSettingsResolveRequest,
    BatchExecutionScheduleOutcome, BatchExecutionScheduleRequest, Database,
    ExecutionRuntimeSettingsSnapshot, ResolvedBatchExecutionPlanningInput, SecretKeyring,
    StorageError,
    execution::{insert_credit_reservation, insert_quote_and_reserve_balance},
    outbox::enqueue_in_transaction,
    secret::{decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob},
};

#[derive(Clone, Debug)]
pub struct SqliteBatchExecutionRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
}

impl SqliteBatchExecutionRepository {
    pub const fn new(database: Database, keyring: Arc<SecretKeyring>) -> Self {
        Self { database, keyring }
    }
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "parent scheduling keeps binding, idempotency, job, audit and outbox writes visibly atomic"
)]
impl BatchExecutionRepository for SqliteBatchExecutionRepository {
    async fn find_idempotent_batch_execution(
        &self,
        idempotency_scope: &str,
        idempotency_key: &str,
    ) -> Result<Option<BatchExecution>, StorageError> {
        validate_idempotency(idempotency_scope, idempotency_key)?;
        sqlx::query(BATCH_EXECUTION_SELECT_BY_IDEMPOTENCY)
            .bind(idempotency_scope)
            .bind(idempotency_key)
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_batch_execution)
            .transpose()
    }

    async fn schedule_batch_execution(
        &self,
        request: BatchExecutionScheduleRequest<'_>,
    ) -> Result<BatchExecutionScheduleOutcome, StorageError> {
        validate_schedule_request(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(row) = sqlx::query(BATCH_EXECUTION_SELECT_BY_IDEMPOTENCY)
            .bind(request.idempotency_scope)
            .bind(request.idempotency_key)
            .fetch_optional(&mut *transaction)
            .await?
        {
            let existing = decode_batch_execution(&row)?;
            let input_matches = stored_planning_input_matches(
                &mut transaction,
                existing.id,
                request.planning_input,
            )
            .await?;
            transaction.commit().await?;
            return if existing == *request.batch_execution && input_matches {
                Ok(BatchExecutionScheduleOutcome::Existing(existing))
            } else {
                Ok(BatchExecutionScheduleOutcome::IdempotencyConflict)
            };
        }

        let binding = sqlx::query(
            "SELECT account.owner_user_id, account.provider_id, \
                    account.id AS provider_account_id, \
                    course.id AS course_id \
             FROM provider_accounts AS account \
             INNER JOIN courses AS course ON course.provider_account_id = account.id \
             WHERE account.id = ? AND course.id = ?",
        )
        .bind(request.batch_execution.provider_account_id.to_string())
        .bind(request.batch_execution.course_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(binding) = binding else {
            transaction.rollback().await?;
            return Ok(BatchExecutionScheduleOutcome::BindingConflict);
        };
        let owner_id = UserId::from_str(binding.try_get("owner_user_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let provider_id = ProviderId::new(binding.try_get::<String, _>("provider_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let actor_matches = match request.actor {
            AuditActor::User(actor) => {
                actor == owner_id && request.batch_execution.requested_by == Some(actor)
            }
            AuditActor::ServiceToken(_) => request
                .batch_execution
                .requested_by
                .is_none_or(|requester| requester == owner_id),
        };
        if !actor_matches || request.planning_input.provider_id() != &provider_id {
            transaction.rollback().await?;
            return Ok(BatchExecutionScheduleOutcome::BindingConflict);
        }

        let batch = request.batch_execution;
        sqlx::query(
            "INSERT INTO batch_executions \
             (id, provider_account_id, course_id, requested_capabilities_json, \
              expected_child_count, requested_by, request_source, state, scheduled_at, \
              started_at, finished_at, idempotency_scope, idempotency_key, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(batch.id.to_string())
        .bind(batch.provider_account_id.to_string())
        .bind(batch.course_id.to_string())
        .bind(serde_json::to_string(&batch.requested_capabilities)?)
        .bind(i64::from(batch.expected_child_count))
        .bind(batch.requested_by.map(|value| value.to_string()))
        .bind(enum_name(batch.request_source)?)
        .bind(enum_name(batch.state)?)
        .bind(batch.scheduled_at.map(encode_timestamp))
        .bind(batch.started_at.map(encode_timestamp))
        .bind(batch.finished_at.map(encode_timestamp))
        .bind(request.idempotency_scope)
        .bind(request.idempotency_key)
        .bind(encode_timestamp(batch.created_at))
        .execute(&mut *transaction)
        .await?;
        insert_batch_planning_input(&mut transaction, &self.keyring, owner_id, &request).await?;
        let job_kind = ScheduledJobKind::BatchExecution {
            batch_execution_id: batch.id,
        };
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, \
              created_at, updated_at) \
             VALUES (?, 'batch_execution', ?, ?, 'pending', 0, ?, ?, ?)",
        )
        .bind(asterism_domain::ScheduleId::new().to_string())
        .bind(serde_json::to_string(&job_kind)?)
        .bind(encode_timestamp(batch.scheduled_at.ok_or_else(|| {
            StorageError::InvalidData("batch execution schedule time is missing".to_owned())
        })?))
        .bind(format!("batch-execution:{}", batch.id))
        .bind(encode_timestamp(batch.created_at))
        .bind(encode_timestamp(batch.created_at))
        .execute(&mut *transaction)
        .await?;
        insert_batch_execution_audit(&mut transaction, &request).await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                request.correlation_id,
                DomainEvent::BatchExecutionStateChanged {
                    batch_execution_id: batch.id,
                    state: batch.state,
                },
                batch.created_at,
            ),
        )
        .await?;
        transaction.commit().await?;
        Ok(BatchExecutionScheduleOutcome::Created(batch.clone()))
    }

    async fn find_batch_execution(
        &self,
        batch_execution_id: BatchExecutionId,
    ) -> Result<Option<BatchExecution>, StorageError> {
        sqlx::query(BATCH_EXECUTION_SELECT_BY_ID)
            .bind(batch_execution_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_batch_execution)
            .transpose()
    }

    async fn start_batch_execution_attempt(
        &self,
        request: BatchExecutionAttemptStartRequest<'_>,
    ) -> Result<BatchExecutionAttempt, StorageError> {
        validate_worker_context(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let claim_expires_at = assert_batch_scheduler_claim(
            &mut transaction,
            request.batch_execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let row = sqlx::query(BATCH_EXECUTION_SELECT_BY_ID)
            .bind(request.batch_execution_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::BatchExecutionStateConflict)?;
        let mut batch = decode_batch_execution(&row)?;
        if batch.state == ExecutionState::Running {
            assert_batch_lease(&mut transaction, batch.id, request.worker_id, request.at).await?;
            let attempt = find_active_attempt(&mut transaction, batch.id)
                .await?
                .ok_or(StorageError::BatchExecutionAttemptNotActive)?;
            transaction.commit().await?;
            return Ok(attempt);
        }
        if batch.state != ExecutionState::Scheduled {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let live_other: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM batch_execution_leases \
             WHERE batch_execution_id = ? AND expires_at > ? AND worker_id <> ?",
        )
        .bind(batch.id.to_string())
        .bind(encode_timestamp(request.at))
        .bind(request.worker_id)
        .fetch_one(&mut *transaction)
        .await?;
        if live_other != 0 {
            return Err(StorageError::BatchExecutionClaimLost);
        }
        sqlx::query(
            "INSERT INTO batch_execution_leases (batch_execution_id, worker_id, expires_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(batch_execution_id) DO UPDATE SET worker_id = excluded.worker_id, \
                expires_at = excluded.expires_at \
             WHERE batch_execution_leases.expires_at <= ? \
                OR batch_execution_leases.worker_id = excluded.worker_id",
        )
        .bind(batch.id.to_string())
        .bind(request.worker_id)
        .bind(encode_timestamp(claim_expires_at))
        .bind(encode_timestamp(request.at))
        .execute(&mut *transaction)
        .await?;
        assert_batch_lease(&mut transaction, batch.id, request.worker_id, request.at).await?;
        let next_attempt_no: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(attempt_no), 0) + 1 FROM batch_execution_attempts \
             WHERE batch_execution_id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        let attempt = BatchExecutionAttempt {
            id: BatchExecutionAttemptId::new(),
            batch_execution_id: batch.id,
            attempt_no: u32::try_from(next_attempt_no).map_err(|_| {
                StorageError::InvalidData("batch execution attempt number is invalid".to_owned())
            })?,
            started_at: request.at,
            finished_at: None,
            result: None,
            error_class: None,
            provider_trace_id: None,
        };
        if attempt.attempt_no > 1_000 {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        sqlx::query(
            "INSERT INTO batch_execution_attempts \
             (id, batch_execution_id, attempt_no, started_at) VALUES (?, ?, ?, ?)",
        )
        .bind(attempt.id.to_string())
        .bind(attempt.batch_execution_id.to_string())
        .bind(i64::from(attempt.attempt_no))
        .bind(encode_timestamp(attempt.started_at))
        .execute(&mut *transaction)
        .await?;
        let changed = sqlx::query(
            "UPDATE batch_executions SET state = 'running', started_at = ? \
             WHERE id = ? AND state = 'scheduled' AND started_at IS NULL",
        )
        .bind(encode_timestamp(request.at))
        .bind(batch.id.to_string())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        batch.state = ExecutionState::Running;
        batch.started_at = Some(request.at);
        insert_batch_worker_audit(
            &mut transaction,
            &batch,
            &attempt,
            request.worker_id,
            request.correlation_id,
        )
        .await?;
        enqueue_in_transaction(
            &mut transaction,
            &EventEnvelope::at(
                request.correlation_id,
                DomainEvent::BatchExecutionStateChanged {
                    batch_execution_id: batch.id,
                    state: batch.state,
                },
                request.at,
            ),
        )
        .await?;
        transaction.commit().await?;
        Ok(attempt)
    }

    async fn find_active_batch_execution_attempt(
        &self,
        batch_execution_id: BatchExecutionId,
    ) -> Result<Option<BatchExecutionAttempt>, StorageError> {
        sqlx::query(
            "SELECT id, batch_execution_id, attempt_no, started_at, finished_at, result, \
                    error_class, provider_trace_id FROM batch_execution_attempts \
             WHERE batch_execution_id = ? AND finished_at IS NULL \
             ORDER BY attempt_no DESC LIMIT 1",
        )
        .bind(batch_execution_id.to_string())
        .fetch_optional(self.database.pool())
        .await?
        .as_ref()
        .map(decode_batch_attempt)
        .transpose()
    }
}

#[async_trait]
impl BatchExecutionPlanningInputRepository for SqliteBatchExecutionRepository {
    async fn resolve_batch_execution_planning_input(
        &self,
        request: BatchExecutionPlanningInputResolveRequest<'_>,
    ) -> Result<ResolvedBatchExecutionPlanningInput, SecretStoreError> {
        validate_planning_input_access_context(&request)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(planning_input_storage_error)?;
        assert_batch_worker_claims(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await
        .map_err(|_| SecretStoreError::VersionConflict)?;
        let stored = fetch_stored_planning_input(&mut transaction, request.batch_execution_id)
            .await?
            .ok_or(SecretStoreError::NotFound)?;
        authorize_planning_input_access(
            stored.owner_user_id,
            &stored.actual_provider_id,
            request.access,
        )?;
        if stored.state != "running"
            || stored.metadata.provider_id != stored.actual_provider_id
            || stored.metadata.bound_at > request.at
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let secret = fetch_secret(&mut transaction, stored.secret_id).await?;
        if secret.owner_user_id != stored.owner_user_id
            || secret.purpose != SecretPurpose::ProviderExecutionState
            || secret.version != 1
            || secret.created_at != stored.metadata.bound_at
            || secret.updated_at != stored.metadata.bound_at
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let secret_ref = SecretRef {
            id: stored.secret_id,
            owner_user_id: secret.owner_user_id,
            purpose: secret.purpose,
            version: secret.version,
            key_id: secret.key_id.clone(),
            created_at: secret.created_at,
            updated_at: secret.updated_at,
        };
        let plaintext = decrypt(
            self.keyring.get(&secret.key_id)?,
            &secret_ref,
            &secret.nonce,
            &secret.encrypted_data,
        )?;
        let input = ProviderBatchExecutionPlanningInput::try_new(
            stored.metadata.provider_id.clone(),
            stored.metadata.input_type.clone(),
            SecretValue::new(plaintext),
        )
        .map_err(|_| SecretStoreError::AuthenticationFailed)?;
        if input.input_digest() != stored.metadata.input_digest {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "batch_execution_planning_input_accessed",
            &secret_ref,
        )
        .await
        .map_err(planning_input_storage_error)?;
        transaction
            .commit()
            .await
            .map_err(planning_input_storage_error)?;
        Ok(ResolvedBatchExecutionPlanningInput {
            metadata: stored.metadata,
            input,
        })
    }
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "parent, snapshot, local Task, ordered artifact and sequence bindings stay in one visible transaction"
)]
impl BatchExecutionChildPlanRepository for SqliteBatchExecutionRepository {
    async fn materialize_batch_execution_child_plans(
        &self,
        request: BatchExecutionChildPlanMaterializeRequest<'_>,
    ) -> Result<BatchExecutionChildPlanMaterializeOutcome, StorageError> {
        validate_worker_context(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_batch_worker_claims(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let row = sqlx::query(BATCH_EXECUTION_SELECT_BY_ID)
            .bind(request.batch_execution_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::BatchExecutionStateConflict)?;
        let batch = decode_batch_execution(&row)?;
        if batch.state != ExecutionState::Running
            || usize::try_from(batch.expected_child_count)
                != Ok(request.execution_batch_plan.children().len())
        {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let parent: (String, String, Vec<u8>, Vec<u8>) = sqlx::query_as(
            "SELECT account.provider_id, snapshot.batch_execution_attempt_id, \
                    snapshot.authority_digest, snapshot.batch_digest \
             FROM batch_executions AS batch \
             INNER JOIN provider_accounts AS account ON account.id = batch.provider_account_id \
             INNER JOIN batch_execution_parent_snapshots AS snapshot \
                     ON snapshot.batch_execution_id = batch.id \
             WHERE batch.id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if parent.0 != request.execution_batch_plan.provider_id().as_str()
            || parent.1 != request.attempt_id.to_string()
            || decode_storage_digest(parent.2)? != request.execution_batch_plan.authority_digest()
            || decode_storage_digest(parent.3)? != request.execution_batch_plan.batch_digest()
        {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let existing_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM batch_execution_child_plans WHERE batch_execution_id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if existing_count != 0 {
            let records = load_child_plan_records(&mut transaction, batch.id).await?;
            if usize::try_from(existing_count).ok() != Some(records.len())
                || records.len() != request.execution_batch_plan.children().len()
                || !stored_child_plans_match(
                    &mut transaction,
                    &batch,
                    request.attempt_id,
                    request.execution_batch_plan,
                )
                .await?
            {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            transaction.commit().await?;
            return Ok(BatchExecutionChildPlanMaterializeOutcome::Existing(records));
        }

        let mut resolved = Vec::with_capacity(request.execution_batch_plan.children().len());
        for child in request.execution_batch_plan.children() {
            let calls = child.execution_plan().calls();
            if canonical_capabilities(calls) != batch.requested_capabilities {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            let artifact = child
                .execution_plan()
                .artifact()
                .ok_or(StorageError::BatchExecutionStateConflict)?;
            let task = sqlx::query(
                "SELECT id, capabilities_json FROM tasks \
                 WHERE provider_account_id = ? AND course_id = ? AND remote_id = ?",
            )
            .bind(batch.provider_account_id.to_string())
            .bind(batch.course_id.to_string())
            .bind(child.remote_task_id())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::BatchExecutionStateConflict)?;
            let task_id = TaskId::from_str(task.try_get("id")?)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let task_capabilities: Vec<TaskCapability> =
                serde_json::from_str(task.try_get("capabilities_json")?)?;
            if batch
                .requested_capabilities
                .iter()
                .any(|capability| !task_capabilities.contains(capability))
            {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            resolved.push((child, artifact, task_id));
        }

        let mut records = Vec::with_capacity(resolved.len());
        for (child, artifact, task_id) in resolved {
            let sequence = child.mutation_sequence_plan();
            sqlx::query(
                "INSERT INTO batch_execution_child_plans \
                 (batch_execution_id, batch_execution_attempt_id, position, task_id, \
                  remote_task_id_digest, provider_id, calls_json, artifact_type, \
                  artifact_digest, artifact_payload_json, sequence_type, sequence_digest, \
                  materialized_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(batch.id.to_string())
            .bind(request.attempt_id.to_string())
            .bind(i64::from(child.position()))
            .bind(task_id.to_string())
            .bind(remote_task_id_digest(child.remote_task_id()).to_vec())
            .bind(request.execution_batch_plan.provider_id().as_str())
            .bind(serde_json::to_string(child.execution_plan().calls())?)
            .bind(artifact.artifact_type())
            .bind(artifact.artifact_digest().to_vec())
            .bind(serde_json::to_string(artifact.payload_sanitized())?)
            .bind(sequence.sequence_type())
            .bind(sequence.plan_digest().to_vec())
            .bind(encode_timestamp(request.at))
            .execute(&mut *transaction)
            .await?;
            for (index, phase) in sequence.phases().iter().enumerate() {
                sqlx::query(
                    "INSERT INTO batch_execution_child_plan_phases \
                     (batch_execution_id, child_position, phase_position, operation_type, \
                      minimum_occurrences, maximum_occurrences, stop_repeating_after_rejection, \
                      advance_condition, required_observation_type) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(batch.id.to_string())
                .bind(i64::from(child.position()))
                .bind(i64::try_from(index + 1).expect("sequence phases are bounded"))
                .bind(phase.operation_type())
                .bind(i64::from(phase.minimum_occurrences()))
                .bind(i64::from(phase.maximum_occurrences()))
                .bind(phase.stop_repeating_after_rejection())
                .bind(advance_condition_name(phase.advance_condition()))
                .bind(phase.required_observation_type())
                .execute(&mut *transaction)
                .await?;
            }
            records.push(BatchExecutionChildPlanRecord {
                batch_execution_id: batch.id,
                attempt_id: request.attempt_id,
                position: child.position(),
                task_id,
                artifact_digest: artifact.artifact_digest(),
                sequence_digest: sequence.plan_digest(),
                materialized_at: request.at,
            });
        }
        insert_batch_child_plans_audit(
            &mut transaction,
            &batch,
            request.attempt_id,
            records.len(),
            request.worker_id,
            request.correlation_id,
            request.at,
        )
        .await?;
        transaction.commit().await?;
        Ok(BatchExecutionChildPlanMaterializeOutcome::Created(records))
    }

    async fn find_batch_execution_child_plans(
        &self,
        batch_execution_id: BatchExecutionId,
    ) -> Result<Vec<BatchExecutionChildPlanRecord>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let records = load_child_plan_records(&mut transaction, batch_execution_id).await?;
        transaction.commit().await?;
        Ok(records)
    }
}

#[async_trait]
impl BatchExecutionRuntimeSettingsRepository for SqliteBatchExecutionRepository {
    async fn find_batch_execution_runtime_settings(
        &self,
        request: BatchExecutionRuntimeSettingsResolveRequest<'_>,
    ) -> Result<Option<ExecutionRuntimeSettingsSnapshot>, StorageError> {
        validate_worker_context(request.worker_id, "batch-runtime-settings-resolve")?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_batch_worker_claims(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let snapshot =
            fetch_batch_runtime_settings(&mut transaction, request.batch_execution_id).await?;
        if snapshot.as_ref().is_some_and(|(attempt_id, _, state)| {
            *attempt_id != request.attempt_id || state != "running"
        }) {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        transaction.commit().await?;
        Ok(snapshot.map(|(_, snapshot, _)| snapshot))
    }

    async fn bind_batch_execution_runtime_settings(
        &self,
        request: BatchExecutionRuntimeSettingsBindRequest<'_>,
    ) -> Result<BatchExecutionRuntimeSettingsBindOutcome, StorageError> {
        validate_worker_context(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_batch_worker_claims(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let binding: (String, String) = sqlx::query_as(
            "SELECT account.provider_id, batch.state FROM batch_executions AS batch \
             INNER JOIN provider_accounts AS account ON account.id = batch.provider_account_id \
             WHERE batch.id = ?",
        )
        .bind(request.batch_execution_id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        let provider_id = ProviderId::new(binding.0)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        validate_batch_runtime_settings_snapshot(
            request.snapshot,
            request.schema,
            &provider_id,
            request.at,
        )?;
        if binding.1 != "running" {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        if let Some((attempt_id, existing, state)) =
            fetch_batch_runtime_settings(&mut transaction, request.batch_execution_id).await?
        {
            transaction.commit().await?;
            return if attempt_id == request.attempt_id
                && state == "running"
                && existing == *request.snapshot
            {
                Ok(BatchExecutionRuntimeSettingsBindOutcome::Existing(existing))
            } else {
                Err(StorageError::BatchExecutionStateConflict)
            };
        }
        sqlx::query(
            "INSERT INTO batch_execution_runtime_settings \
             (batch_execution_id, batch_execution_attempt_id, provider_id, schema_version, \
              resolved_settings_json, sources_json, provider_revision, \
              provider_account_revision, completion_policy_json, captured_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request.batch_execution_id.to_string())
        .bind(request.attempt_id.to_string())
        .bind(request.snapshot.provider_id.as_str())
        .bind(i64::from(request.snapshot.resolved.schema_version))
        .bind(serde_json::to_string(&request.snapshot.resolved.values)?)
        .bind(serde_json::to_string(&request.snapshot.sources)?)
        .bind(request.snapshot.provider_revision.map(i64::from))
        .bind(request.snapshot.provider_account_revision.map(i64::from))
        .bind(serde_json::to_string(&request.snapshot.completion_policy)?)
        .bind(encode_timestamp(request.snapshot.captured_at))
        .execute(&mut *transaction)
        .await?;
        insert_batch_runtime_settings_audit(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.snapshot,
            request.worker_id,
            request.correlation_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(BatchExecutionRuntimeSettingsBindOutcome::Bound(
            request.snapshot.clone(),
        ))
    }
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "immutable child Execution, calls, settings, artifact, mapping, audit and outbox writes stay atomic"
)]
impl BatchExecutionChildExecutionRepository for SqliteBatchExecutionRepository {
    async fn create_batch_execution_child_executions(
        &self,
        request: BatchExecutionChildExecutionCreateRequest<'_>,
    ) -> Result<BatchExecutionChildExecutionCreateOutcome, StorageError> {
        validate_worker_context(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_batch_worker_claims(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let row = sqlx::query(BATCH_EXECUTION_SELECT_BY_ID)
            .bind(request.batch_execution_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
        let batch = decode_batch_execution(&row)?;
        if batch.state != ExecutionState::Running {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let (settings_attempt_id, runtime_settings, settings_state) =
            fetch_batch_runtime_settings(&mut transaction, batch.id)
                .await?
                .ok_or(StorageError::BatchExecutionStateConflict)?;
        if settings_attempt_id != request.attempt_id || settings_state != "running" {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let child_plan_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM batch_execution_child_plans WHERE batch_execution_id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if u32::try_from(child_plan_count).ok() != Some(batch.expected_child_count) {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let existing = load_child_execution_records(&mut transaction, batch.id).await?;
        if !existing.is_empty() {
            if existing.len() != usize::try_from(batch.expected_child_count).unwrap_or(usize::MAX)
                || !stored_child_executions_match(
                    &mut transaction,
                    &batch,
                    &runtime_settings,
                    &existing,
                )
                .await?
            {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            transaction.commit().await?;
            return Ok(BatchExecutionChildExecutionCreateOutcome::Existing(
                existing,
            ));
        }

        let rows = sqlx::query(
            "SELECT plan.position, plan.task_id, plan.provider_id, plan.calls_json, \
                    plan.artifact_type, plan.artifact_digest, plan.artifact_payload_json, \
                    task.orchestration_state, task.remote_state, account.provider_id AS actual_provider_id \
             FROM batch_execution_child_plans AS plan \
             INNER JOIN tasks AS task ON task.id = plan.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE plan.batch_execution_id = ? ORDER BY plan.position",
        )
        .bind(batch.id.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        let mut prepared = Vec::with_capacity(rows.len());
        for row in rows {
            let position = u32::try_from(row.try_get::<i64, _>("position")?).map_err(|_| {
                StorageError::InvalidData("batch child position is invalid".to_owned())
            })?;
            let task_id = TaskId::from_str(row.try_get("task_id")?)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let provider_id = ProviderId::new(row.try_get::<String, _>("provider_id")?)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let actual_provider_id =
                ProviderId::new(row.try_get::<String, _>("actual_provider_id")?)
                    .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let calls: Vec<Vec<TaskCapability>> = serde_json::from_str(row.try_get("calls_json")?)?;
            let artifact = ProviderExecutionPlanArtifact::try_new(
                provider_id.clone(),
                row.try_get::<String, _>("artifact_type")?,
                serde_json::from_str(row.try_get("artifact_payload_json")?)?,
            )
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            let persisted_artifact_digest = decode_storage_digest(row.try_get("artifact_digest")?)?;
            let active_task_settings: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM provider_runtime_settings \
                 WHERE scope = 'task' AND task_id = ?",
            )
            .bind(task_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
            let other_active_executions: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM executions WHERE task_id = ? \
                 AND state IN ('requested', 'scheduled', 'running', 'recovering', \
                               'retry_waiting', 'human_required')",
            )
            .bind(task_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
            if provider_id != runtime_settings.provider_id
                || provider_id != actual_provider_id
                || canonical_capabilities(&calls) != batch.requested_capabilities
                || artifact.artifact_digest() != persisted_artifact_digest
                || active_task_settings != 0
                || other_active_executions != 0
                || !matches!(
                    row.try_get::<String, _>("orchestration_state")?.as_str(),
                    "ready" | "failed"
                )
                || matches!(
                    row.try_get::<String, _>("remote_state")?.as_str(),
                    "expired" | "removed"
                )
            {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            prepared.push((position, task_id, calls, artifact));
        }

        let mut records = Vec::with_capacity(prepared.len());
        for (position, task_id, calls, artifact) in prepared {
            let execution_id = ExecutionId::new();
            let created_at = runtime_settings.captured_at;
            sqlx::query(
                "INSERT INTO executions \
                 (id, task_id, requested_capabilities_json, submission_draft_id, requested_by, \
                  request_source, quote_id, state, scheduled_at, started_at, finished_at, \
                  created_at, idempotency_scope, idempotency_key) \
                 VALUES (?, ?, ?, NULL, ?, ?, NULL, 'requested', NULL, NULL, NULL, ?, ?, ?)",
            )
            .bind(execution_id.to_string())
            .bind(task_id.to_string())
            .bind(serde_json::to_string(&batch.requested_capabilities)?)
            .bind(batch.requested_by.map(|id| id.to_string()))
            .bind(enum_name(batch.request_source)?)
            .bind(encode_timestamp(created_at))
            .bind(format!("batch-execution:{}", batch.id))
            .bind(format!("child:{position}"))
            .execute(&mut *transaction)
            .await?;
            insert_child_capability_steps(&mut transaction, execution_id, &calls).await?;
            insert_child_runtime_settings(&mut transaction, execution_id, &runtime_settings)
                .await?;
            insert_child_provider_artifact(&mut transaction, execution_id, created_at, &artifact)
                .await?;
            sqlx::query(
                "INSERT INTO batch_execution_child_executions \
                 (batch_execution_id, child_position, execution_id, created_at) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(batch.id.to_string())
            .bind(i64::from(position))
            .bind(execution_id.to_string())
            .bind(encode_timestamp(created_at))
            .execute(&mut *transaction)
            .await?;
            enqueue_in_transaction(
                &mut transaction,
                &EventEnvelope::at(
                    request.correlation_id,
                    DomainEvent::ExecutionStateChanged {
                        execution_id,
                        state: ExecutionState::Requested,
                    },
                    created_at,
                ),
            )
            .await?;
            records.push(BatchExecutionChildExecutionRecord {
                batch_execution_id: batch.id,
                position,
                task_id,
                execution_id,
                created_at,
            });
        }
        insert_batch_child_executions_audit(
            &mut transaction,
            &batch,
            request.attempt_id,
            records.len(),
            request.worker_id,
            request.correlation_id,
            request.at,
        )
        .await?;
        transaction.commit().await?;
        Ok(BatchExecutionChildExecutionCreateOutcome::Created(records))
    }

    async fn find_batch_execution_child_executions(
        &self,
        batch_execution_id: BatchExecutionId,
    ) -> Result<Vec<BatchExecutionChildExecutionRecord>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let records = load_child_execution_records(&mut transaction, batch_execution_id).await?;
        transaction.commit().await?;
        Ok(records)
    }
}

#[derive(Debug)]
struct PreparedBatchChildActivation {
    position: u32,
    task_id: TaskId,
    execution_id: ExecutionId,
    prior_task_state: String,
    billing_index: Option<usize>,
}

#[async_trait]
#[allow(
    clippy::too_many_lines,
    reason = "complete child preflight, optional batch billing, Task/Execution transitions, jobs, mappings, audit and outbox writes stay atomic"
)]
impl BatchExecutionChildActivationRepository for SqliteBatchExecutionRepository {
    async fn activate_batch_execution_children(
        &self,
        request: BatchExecutionChildActivationRequest<'_>,
    ) -> Result<BatchExecutionChildActivationOutcome, StorageError> {
        validate_worker_context(request.worker_id, request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_batch_worker_claims(
            &mut transaction,
            request.batch_execution_id,
            request.attempt_id,
            request.parent_scheduler_job_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let row = sqlx::query(BATCH_EXECUTION_SELECT_BY_ID)
            .bind(request.batch_execution_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
        let batch = decode_batch_execution(&row)?;
        if batch.state != ExecutionState::Running {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let child_executions = load_child_execution_records(&mut transaction, batch.id).await?;
        let expected_count = usize::try_from(batch.expected_child_count).unwrap_or(usize::MAX);
        if child_executions.len() != expected_count
            || !valid_activation_billing_positions(request.billings, expected_count)
        {
            return Err(StorageError::BatchExecutionStateConflict);
        }
        let existing = load_child_activation_records(&mut transaction, batch.id).await?;
        if !existing.is_empty() {
            if existing.len() != expected_count
                || !stored_child_activations_match(
                    &mut transaction,
                    &batch,
                    &existing,
                    request.billings,
                )
                .await?
            {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            transaction.commit().await?;
            return Ok(BatchExecutionChildActivationOutcome::Existing(existing));
        }

        let mut prepared = Vec::with_capacity(child_executions.len());
        for child in &child_executions {
            let row = sqlx::query(
                "SELECT execution.state, execution.scheduled_at, execution.started_at, \
                        execution.finished_at, execution.quote_id, execution.requested_by, \
                        execution.created_at, task.orchestration_state, task.remote_state \
                 FROM executions AS execution \
                 INNER JOIN tasks AS task ON task.id = execution.task_id \
                 WHERE execution.id = ? AND execution.task_id = ?",
            )
            .bind(child.execution_id.to_string())
            .bind(child.task_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StorageError::BatchExecutionStateConflict)?;
            let other_active_executions: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM executions WHERE task_id = ? AND id != ? \
                 AND state IN ('requested', 'scheduled', 'running', 'recovering', \
                               'retry_waiting', 'human_required')",
            )
            .bind(child.task_id.to_string())
            .bind(child.execution_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
            let existing_job: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs WHERE idempotency_key = ?")
                    .bind(format!("execution:{}", child.execution_id))
                    .fetch_one(&mut *transaction)
                    .await?;
            let prior_task_state = row.try_get::<String, _>("orchestration_state")?;
            if row.try_get::<String, _>("state")? != "requested"
                || row.try_get::<Option<String>, _>("scheduled_at")?.is_some()
                || row.try_get::<Option<String>, _>("started_at")?.is_some()
                || row.try_get::<Option<String>, _>("finished_at")?.is_some()
                || row.try_get::<Option<String>, _>("quote_id")?.is_some()
                || row.try_get::<Option<String>, _>("requested_by")?
                    != batch.requested_by.map(|id| id.to_string())
                || decode_timestamp(row.try_get("created_at")?)? != child.created_at
                || request.at < child.created_at
                || !matches!(prior_task_state.as_str(), "ready" | "failed")
                || matches!(
                    row.try_get::<String, _>("remote_state")?.as_str(),
                    "expired" | "removed"
                )
                || other_active_executions != 0
                || existing_job != 0
            {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            let billing_index = if request.billings.is_empty() {
                None
            } else {
                let index = usize::try_from(child.position.saturating_sub(1))
                    .map_err(|_| StorageError::BatchExecutionStateConflict)?;
                let billing = request
                    .billings
                    .get(index)
                    .ok_or(StorageError::BatchExecutionStateConflict)?;
                validate_child_activation_billing(&batch, child, billing)?;
                Some(index)
            };
            prepared.push(PreparedBatchChildActivation {
                position: child.position,
                task_id: child.task_id,
                execution_id: child.execution_id,
                prior_task_state,
                billing_index,
            });
        }

        let mut activations = Vec::with_capacity(prepared.len());
        for child in prepared {
            let billing = child
                .billing_index
                .and_then(|index| request.billings.get(index));
            if let Some(billing) = billing {
                insert_quote_and_reserve_balance(&mut transaction, &billing.billing).await?;
            }
            let task_update = sqlx::query(
                "UPDATE tasks SET orchestration_state = 'scheduled', updated_at = ? \
                 WHERE id = ? AND orchestration_state = ?",
            )
            .bind(encode_timestamp(request.at))
            .bind(child.task_id.to_string())
            .bind(&child.prior_task_state)
            .execute(&mut *transaction)
            .await?;
            let execution_update = sqlx::query(
                "UPDATE executions SET state = 'scheduled', scheduled_at = ?, quote_id = ? \
                 WHERE id = ? AND state = 'requested' AND scheduled_at IS NULL \
                   AND quote_id IS NULL",
            )
            .bind(encode_timestamp(request.at))
            .bind(billing.map(|value| value.billing.quote.id.to_string()))
            .bind(child.execution_id.to_string())
            .execute(&mut *transaction)
            .await?;
            if task_update.rows_affected() != 1 || execution_update.rows_affected() != 1 {
                return Err(StorageError::BatchExecutionStateConflict);
            }
            if let Some(billing) = billing {
                insert_credit_reservation(
                    &mut transaction,
                    &billing.billing,
                    request.correlation_id,
                )
                .await?;
            }
            let scheduler_job_id = ScheduleId::new();
            let job_kind = ScheduledJobKind::Execution {
                execution_id: child.execution_id,
            };
            sqlx::query(
                "INSERT INTO scheduled_jobs \
                 (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, \
                  created_at, updated_at) \
                 VALUES (?, 'execution', ?, ?, 'pending', 0, ?, ?, ?)",
            )
            .bind(scheduler_job_id.to_string())
            .bind(serde_json::to_string(&job_kind)?)
            .bind(encode_timestamp(request.at))
            .bind(format!("execution:{}", child.execution_id))
            .bind(encode_timestamp(request.at))
            .bind(encode_timestamp(request.at))
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO batch_execution_child_activations \
                 (batch_execution_id, child_position, execution_id, scheduler_job_id, activated_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(batch.id.to_string())
            .bind(i64::from(child.position))
            .bind(child.execution_id.to_string())
            .bind(scheduler_job_id.to_string())
            .bind(encode_timestamp(request.at))
            .execute(&mut *transaction)
            .await?;
            enqueue_in_transaction(
                &mut transaction,
                &EventEnvelope::at(
                    request.correlation_id,
                    DomainEvent::ExecutionStateChanged {
                        execution_id: child.execution_id,
                        state: ExecutionState::Scheduled,
                    },
                    request.at,
                ),
            )
            .await?;
            activations.push(BatchExecutionChildActivationRecord {
                batch_execution_id: batch.id,
                position: child.position,
                task_id: child.task_id,
                execution_id: child.execution_id,
                scheduler_job_id,
                activated_at: request.at,
            });
        }
        insert_batch_child_activation_audit(
            &mut transaction,
            &batch,
            request.attempt_id,
            activations.len(),
            !request.billings.is_empty(),
            request.worker_id,
            request.correlation_id,
            request.at,
        )
        .await?;
        transaction.commit().await?;
        Ok(BatchExecutionChildActivationOutcome::Activated(activations))
    }

    async fn find_batch_execution_child_activations(
        &self,
        batch_execution_id: BatchExecutionId,
    ) -> Result<Vec<BatchExecutionChildActivationRecord>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let records = load_child_activation_records(&mut transaction, batch_execution_id).await?;
        transaction.commit().await?;
        Ok(records)
    }
}

fn valid_activation_billing_positions(
    billings: &[BatchExecutionChildActivationBilling<'_>],
    expected_count: usize,
) -> bool {
    billings.is_empty()
        || (billings.len() == expected_count
            && billings
                .iter()
                .enumerate()
                .all(|(index, billing)| u32::try_from(index + 1).ok() == Some(billing.position)))
}

fn validate_child_activation_billing(
    batch: &BatchExecution,
    child: &BatchExecutionChildExecutionRecord,
    activation: &BatchExecutionChildActivationBilling<'_>,
) -> Result<(), StorageError> {
    let billing = &activation.billing;
    let quote = billing.quote;
    let reservation = billing.reservation;
    let valid = activation.position == child.position
        && quote.task_id == child.task_id
        && quote.created_at <= child.created_at
        && valid_token(&quote.pricing_revision, 128)
        && valid_token(&quote.reason, 2_048)
        && i64::try_from(quote.amount.value()).is_ok()
        && batch.requested_by == Some(reservation.user_id)
        && reservation.quote_id == quote.id
        && reservation.execution_id == child.execution_id
        && reservation.amount == quote.amount
        && reservation.state == CreditReservationState::Reserved
        && reservation.created_at == child.created_at
        && reservation.updated_at == reservation.created_at;
    if valid {
        Ok(())
    } else {
        Err(StorageError::BatchExecutionStateConflict)
    }
}

async fn load_child_activation_records(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Vec<BatchExecutionChildActivationRecord>, StorageError> {
    sqlx::query(
        "SELECT activation.batch_execution_id, activation.child_position, plan.task_id, \
                activation.execution_id, activation.scheduler_job_id, activation.activated_at \
         FROM batch_execution_child_activations AS activation \
         INNER JOIN batch_execution_child_plans AS plan \
                 ON plan.batch_execution_id = activation.batch_execution_id \
                AND plan.position = activation.child_position \
         WHERE activation.batch_execution_id = ? ORDER BY activation.child_position",
    )
    .bind(batch_execution_id.to_string())
    .fetch_all(&mut **transaction)
    .await?
    .iter()
    .map(decode_child_activation_record)
    .collect()
}

fn decode_child_activation_record(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BatchExecutionChildActivationRecord, StorageError> {
    Ok(BatchExecutionChildActivationRecord {
        batch_execution_id: BatchExecutionId::from_str(row.try_get("batch_execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        position: u32::try_from(row.try_get::<i64, _>("child_position")?).map_err(|_| {
            StorageError::InvalidData("batch child activation position is invalid".to_owned())
        })?,
        task_id: TaskId::from_str(row.try_get("task_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        scheduler_job_id: ScheduleId::from_str(row.try_get("scheduler_job_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        activated_at: decode_timestamp(row.try_get("activated_at")?)?,
    })
}

async fn stored_child_activations_match(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    records: &[BatchExecutionChildActivationRecord],
    billings: &[BatchExecutionChildActivationBilling<'_>],
) -> Result<bool, StorageError> {
    for record in records {
        let row = sqlx::query(
            "SELECT execution.task_id, execution.state, execution.quote_id, \
                    execution.scheduled_at, activation.execution_id AS activation_execution_id, \
                    activation.scheduler_job_id, activation.activated_at, \
                    job.job_kind, job.payload_json, job.idempotency_key, job.created_at AS job_created_at, \
                    quote.id AS persisted_quote_id, quote.task_id AS quote_task_id, \
                    quote.amount AS quote_amount, quote.pricing_revision, quote.reason, \
                    quote.created_at AS quote_created_at, \
                    reservation.id AS reservation_id, reservation.user_id AS reservation_user_id, \
                    reservation.quote_id AS reservation_quote_id, \
                    reservation.execution_id AS reservation_execution_id, \
                    reservation.amount AS reservation_amount, reservation.state AS reservation_state, \
                    reservation.created_at AS reservation_created_at \
             FROM batch_execution_child_activations AS activation \
             INNER JOIN executions AS execution ON execution.id = activation.execution_id \
             INNER JOIN scheduled_jobs AS job ON job.id = activation.scheduler_job_id \
             LEFT JOIN price_quotes AS quote ON quote.id = execution.quote_id \
             LEFT JOIN credit_reservations AS reservation \
                    ON reservation.execution_id = execution.id \
             WHERE activation.batch_execution_id = ? AND activation.child_position = ?",
        )
        .bind(batch.id.to_string())
        .bind(i64::from(record.position))
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let state: ExecutionState = decode_enum(row.try_get("state")?)?;
        let job_kind: ScheduledJobKind = serde_json::from_str(row.try_get("payload_json")?)?;
        let immutable_match = row.try_get::<String, _>("task_id")? == record.task_id.to_string()
            && row.try_get::<String, _>("activation_execution_id")?
                == record.execution_id.to_string()
            && row.try_get::<String, _>("scheduler_job_id")? == record.scheduler_job_id.to_string()
            && decode_timestamp(row.try_get("activated_at")?)? == record.activated_at
            && state != ExecutionState::Requested
            && decode_optional_timestamp(row.try_get("scheduled_at")?)?
                == Some(record.activated_at)
            && row.try_get::<String, _>("job_kind")? == "execution"
            && job_kind
                == (ScheduledJobKind::Execution {
                    execution_id: record.execution_id,
                })
            && row.try_get::<String, _>("idempotency_key")?
                == format!("execution:{}", record.execution_id)
            && decode_timestamp(row.try_get("job_created_at")?)? == record.activated_at;
        if !immutable_match {
            return Ok(false);
        }
        if billings.is_empty() {
            if row.try_get::<Option<String>, _>("quote_id")?.is_some()
                || row
                    .try_get::<Option<String>, _>("persisted_quote_id")?
                    .is_some()
                || row
                    .try_get::<Option<String>, _>("reservation_id")?
                    .is_some()
            {
                return Ok(false);
            }
            continue;
        }
        let index = usize::try_from(record.position.saturating_sub(1))
            .map_err(|_| StorageError::BatchExecutionStateConflict)?;
        let Some(activation) = billings.get(index) else {
            return Ok(false);
        };
        if !persisted_activation_billing_matches(&row, record, activation)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn persisted_activation_billing_matches(
    row: &sqlx::sqlite::SqliteRow,
    record: &BatchExecutionChildActivationRecord,
    activation: &BatchExecutionChildActivationBilling<'_>,
) -> Result<bool, StorageError> {
    let quote = activation.billing.quote;
    let reservation = activation.billing.reservation;
    let state = row
        .try_get::<Option<&str>, _>("reservation_state")?
        .map(decode_enum::<CreditReservationState>)
        .transpose()?;
    Ok(activation.position == record.position
        && row.try_get::<Option<String>, _>("quote_id")? == Some(quote.id.to_string())
        && row.try_get::<Option<String>, _>("persisted_quote_id")? == Some(quote.id.to_string())
        && row.try_get::<Option<String>, _>("quote_task_id")? == Some(quote.task_id.to_string())
        && row.try_get::<Option<i64>, _>("quote_amount")?
            == Some(
                i64::try_from(quote.amount.value())
                    .map_err(|_| StorageError::CreditAmountOutOfRange)?,
            )
        && row.try_get::<Option<String>, _>("pricing_revision")?
            == Some(quote.pricing_revision.clone())
        && row.try_get::<Option<String>, _>("reason")? == Some(quote.reason.clone())
        && row
            .try_get::<Option<&str>, _>("quote_created_at")?
            .map(decode_timestamp)
            .transpose()?
            == Some(quote.created_at)
        && row.try_get::<Option<String>, _>("reservation_id")? == Some(reservation.id.to_string())
        && row.try_get::<Option<String>, _>("reservation_user_id")?
            == Some(reservation.user_id.to_string())
        && row.try_get::<Option<String>, _>("reservation_quote_id")?
            == Some(reservation.quote_id.to_string())
        && row.try_get::<Option<String>, _>("reservation_execution_id")?
            == Some(record.execution_id.to_string())
        && row.try_get::<Option<i64>, _>("reservation_amount")?
            == Some(
                i64::try_from(reservation.amount.value())
                    .map_err(|_| StorageError::CreditAmountOutOfRange)?,
            )
        && state.is_some_and(|state| {
            matches!(
                state,
                CreditReservationState::Reserved
                    | CreditReservationState::Committed
                    | CreditReservationState::Released
            )
        })
        && row
            .try_get::<Option<&str>, _>("reservation_created_at")?
            .map(decode_timestamp)
            .transpose()?
            == Some(reservation.created_at))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the audit binds the parent Attempt, child count, billing mode and live worker context"
)]
async fn insert_batch_child_activation_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    attempt_id: BatchExecutionAttemptId,
    child_count: usize,
    billed: bool,
    worker_id: &str,
    correlation_id: &str,
    at: Timestamp,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'batch_execution_children_activated', \
                 'batch_execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(at))
    .bind(worker_id)
    .bind(batch.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": attempt_id,
            "child_count": child_count,
            "billed": billed,
            "execution_ids": "[REDACTED]",
            "scheduler_job_ids": "[REDACTED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_child_capability_steps(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    calls: &[Vec<TaskCapability>],
) -> Result<(), StorageError> {
    let mut position = 0_u8;
    for (call_index, call) in calls.iter().enumerate() {
        for (member_index, capability) in call.iter().copied().enumerate() {
            position = position.checked_add(1).ok_or_else(|| {
                StorageError::InvalidData("batch child capability plan is invalid".to_owned())
            })?;
            sqlx::query(
                "INSERT INTO execution_capability_steps \
                 (execution_id, position, call_position, call_member_position, capability, state) \
                 VALUES (?, ?, ?, ?, ?, 'pending')",
            )
            .bind(execution_id.to_string())
            .bind(i64::from(position))
            .bind(i64::try_from(call_index + 1).expect("execution calls are bounded"))
            .bind(i64::try_from(member_index + 1).expect("call members are bounded"))
            .bind(enum_name(capability)?)
            .execute(&mut **transaction)
            .await?;
        }
    }
    Ok(())
}

async fn insert_child_runtime_settings(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    snapshot: &ExecutionRuntimeSettingsSnapshot,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO execution_runtime_settings \
         (execution_id, provider_id, schema_version, resolved_settings_json, sources_json, \
          provider_revision, provider_account_revision, task_revision, completion_policy_json, \
          captured_at) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)",
    )
    .bind(execution_id.to_string())
    .bind(snapshot.provider_id.as_str())
    .bind(i64::from(snapshot.resolved.schema_version))
    .bind(serde_json::to_string(&snapshot.resolved.values)?)
    .bind(serde_json::to_string(&snapshot.sources)?)
    .bind(snapshot.provider_revision.map(i64::from))
    .bind(snapshot.provider_account_revision.map(i64::from))
    .bind(serde_json::to_string(&snapshot.completion_policy)?)
    .bind(encode_timestamp(snapshot.captured_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_child_provider_artifact(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
    captured_at: Timestamp,
    artifact: &ProviderExecutionPlanArtifact,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO execution_provider_plan_artifacts \
         (execution_id, provider_id, artifact_type, artifact_digest, payload_json, captured_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(execution_id.to_string())
    .bind(artifact.provider_id().as_str())
    .bind(artifact.artifact_type())
    .bind(artifact.artifact_digest().to_vec())
    .bind(serde_json::to_string(artifact.payload_sanitized())?)
    .bind(encode_timestamp(captured_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn load_child_execution_records(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Vec<BatchExecutionChildExecutionRecord>, StorageError> {
    sqlx::query(
        "SELECT mapping.batch_execution_id, mapping.child_position, plan.task_id, \
                mapping.execution_id, mapping.created_at \
         FROM batch_execution_child_executions AS mapping \
         INNER JOIN batch_execution_child_plans AS plan \
                 ON plan.batch_execution_id = mapping.batch_execution_id \
                AND plan.position = mapping.child_position \
         WHERE mapping.batch_execution_id = ? ORDER BY mapping.child_position",
    )
    .bind(batch_execution_id.to_string())
    .fetch_all(&mut **transaction)
    .await?
    .iter()
    .map(decode_child_execution_record)
    .collect()
}

fn decode_child_execution_record(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BatchExecutionChildExecutionRecord, StorageError> {
    Ok(BatchExecutionChildExecutionRecord {
        batch_execution_id: BatchExecutionId::from_str(row.try_get("batch_execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        position: u32::try_from(row.try_get::<i64, _>("child_position")?).map_err(|_| {
            StorageError::InvalidData("batch child execution position is invalid".to_owned())
        })?,
        task_id: TaskId::from_str(row.try_get("task_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        execution_id: ExecutionId::from_str(row.try_get("execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "restart verifies every immutable child request, plan artifact, activation-aware quote binding, call and frozen setting in one fail-closed boundary"
)]
async fn stored_child_executions_match(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    runtime_settings: &ExecutionRuntimeSettingsSnapshot,
    records: &[BatchExecutionChildExecutionRecord],
) -> Result<bool, StorageError> {
    for record in records {
        let row = sqlx::query(
            "SELECT execution.task_id, execution.requested_capabilities_json, \
                    execution.submission_draft_id, execution.requested_by, \
                    execution.request_source, execution.quote_id, execution.created_at, \
                    execution.idempotency_scope, execution.idempotency_key, \
                    plan.provider_id AS child_provider_id, \
                    plan.artifact_type AS child_artifact_type, \
                    plan.artifact_digest AS child_artifact_digest, \
                    plan.artifact_payload_json AS child_artifact_payload_json, \
                    artifact.provider_id AS artifact_provider_id, \
                    artifact.artifact_type AS artifact_type, \
                    artifact.artifact_digest AS artifact_digest, \
                    artifact.payload_json AS artifact_payload_json, \
                    artifact.captured_at AS artifact_captured_at, \
                    activation.execution_id AS activation_execution_id, \
                    reservation.quote_id AS reservation_quote_id, \
                    settings.provider_id AS settings_provider_id, \
                    settings.schema_version, settings.resolved_settings_json, settings.sources_json, \
                    settings.provider_revision, settings.provider_account_revision, \
                    settings.task_revision, settings.completion_policy_json, settings.captured_at \
             FROM executions AS execution \
             INNER JOIN batch_execution_child_plans AS plan \
                     ON plan.batch_execution_id = ? AND plan.position = ? \
             INNER JOIN execution_runtime_settings AS settings \
                     ON settings.execution_id = execution.id \
             INNER JOIN execution_provider_plan_artifacts AS artifact \
                     ON artifact.execution_id = execution.id \
             LEFT JOIN batch_execution_child_activations AS activation \
                    ON activation.batch_execution_id = plan.batch_execution_id \
                   AND activation.child_position = plan.position \
                   AND activation.execution_id = execution.id \
             LEFT JOIN credit_reservations AS reservation \
                    ON reservation.execution_id = execution.id \
             WHERE execution.id = ?",
        )
        .bind(batch.id.to_string())
        .bind(i64::from(record.position))
        .bind(record.execution_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let child_provider_id = ProviderId::new(row.try_get::<String, _>("child_provider_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let child_artifact = ProviderExecutionPlanArtifact::try_new(
            child_provider_id,
            row.try_get::<String, _>("child_artifact_type")?,
            serde_json::from_str(row.try_get("child_artifact_payload_json")?)?,
        )
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let artifact_provider_id =
            ProviderId::new(row.try_get::<String, _>("artifact_provider_id")?)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let execution_artifact = ProviderExecutionPlanArtifact::try_new(
            artifact_provider_id,
            row.try_get::<String, _>("artifact_type")?,
            serde_json::from_str(row.try_get("artifact_payload_json")?)?,
        )
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let steps = load_execution_calls(transaction, record.execution_id).await?;
        let quote_id = row.try_get::<Option<String>, _>("quote_id")?;
        let activation_execution_id =
            row.try_get::<Option<String>, _>("activation_execution_id")?;
        let reservation_quote_id = row.try_get::<Option<String>, _>("reservation_quote_id")?;
        let quote_binding_valid = match (
            quote_id.as_deref(),
            activation_execution_id.as_deref(),
            reservation_quote_id.as_deref(),
        ) {
            (None, None, None) => true,
            (None, Some(execution_id), None) => execution_id == record.execution_id.to_string(),
            (Some(quote_id), Some(execution_id), Some(reservation_quote_id)) => {
                execution_id == record.execution_id.to_string() && quote_id == reservation_quote_id
            }
            _ => false,
        };
        if row.try_get::<String, _>("task_id")? != record.task_id.to_string()
            || serde_json::from_str::<Vec<TaskCapability>>(
                row.try_get("requested_capabilities_json")?,
            )? != batch.requested_capabilities
            || row
                .try_get::<Option<String>, _>("submission_draft_id")?
                .is_some()
            || row.try_get::<Option<String>, _>("requested_by")?
                != batch.requested_by.map(|id| id.to_string())
            || row.try_get::<String, _>("request_source")? != enum_name(batch.request_source)?
            || !quote_binding_valid
            || decode_timestamp(row.try_get("created_at")?)? != record.created_at
            || row.try_get::<Option<String>, _>("idempotency_scope")?
                != Some(format!("batch-execution:{}", batch.id))
            || row.try_get::<Option<String>, _>("idempotency_key")?
                != Some(format!("child:{}", record.position))
            || child_artifact.artifact_digest()
                != decode_storage_digest(row.try_get("child_artifact_digest")?)?
            || execution_artifact != child_artifact
            || execution_artifact.artifact_digest()
                != decode_storage_digest(row.try_get("artifact_digest")?)?
            || decode_timestamp(row.try_get("artifact_captured_at")?)? != record.created_at
            || steps != load_child_calls(transaction, batch.id, record.position).await?
            || row.try_get::<String, _>("settings_provider_id")?
                != runtime_settings.provider_id.as_str()
            || u32::try_from(row.try_get::<i64, _>("schema_version")?).ok()
                != Some(runtime_settings.resolved.schema_version)
            || row.try_get::<String, _>("resolved_settings_json")?
                != serde_json::to_string(&runtime_settings.resolved.values)?
            || row.try_get::<String, _>("sources_json")?
                != serde_json::to_string(&runtime_settings.sources)?
            || decode_optional_batch_revision(row.try_get("provider_revision")?)?
                != runtime_settings.provider_revision
            || decode_optional_batch_revision(row.try_get("provider_account_revision")?)?
                != runtime_settings.provider_account_revision
            || row.try_get::<Option<i64>, _>("task_revision")?.is_some()
            || row.try_get::<String, _>("completion_policy_json")?
                != serde_json::to_string(&runtime_settings.completion_policy)?
            || decode_timestamp(row.try_get("captured_at")?)? != runtime_settings.captured_at
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn load_execution_calls(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    execution_id: ExecutionId,
) -> Result<Vec<Vec<TaskCapability>>, StorageError> {
    let rows = sqlx::query(
        "SELECT call_position, call_member_position, capability \
         FROM execution_capability_steps WHERE execution_id = ? \
         ORDER BY call_position, call_member_position",
    )
    .bind(execution_id.to_string())
    .fetch_all(&mut **transaction)
    .await?;
    group_call_rows(&rows)
}

async fn load_child_calls(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    position: u32,
) -> Result<Vec<Vec<TaskCapability>>, StorageError> {
    let calls: String = sqlx::query_scalar(
        "SELECT calls_json FROM batch_execution_child_plans \
         WHERE batch_execution_id = ? AND position = ?",
    )
    .bind(batch_execution_id.to_string())
    .bind(i64::from(position))
    .fetch_one(&mut **transaction)
    .await?;
    Ok(serde_json::from_str(&calls)?)
}

fn group_call_rows(
    rows: &[sqlx::sqlite::SqliteRow],
) -> Result<Vec<Vec<TaskCapability>>, StorageError> {
    let mut calls = Vec::<Vec<TaskCapability>>::new();
    for row in rows {
        let call_position =
            usize::try_from(row.try_get::<i64, _>("call_position")?).map_err(|_| {
                StorageError::InvalidData("execution call position is invalid".to_owned())
            })?;
        let member_position = usize::try_from(row.try_get::<i64, _>("call_member_position")?)
            .map_err(|_| {
                StorageError::InvalidData("execution call member is invalid".to_owned())
            })?;
        if call_position != calls.len() && call_position != calls.len() + 1 {
            return Err(StorageError::InvalidData(
                "execution call positions are not contiguous".to_owned(),
            ));
        }
        if call_position == calls.len() + 1 {
            calls.push(Vec::new());
        }
        let call = calls
            .get_mut(call_position.saturating_sub(1))
            .ok_or_else(|| StorageError::InvalidData("execution call is invalid".to_owned()))?;
        if member_position != call.len() + 1 {
            return Err(StorageError::InvalidData(
                "execution call members are not contiguous".to_owned(),
            ));
        }
        call.push(decode_enum(row.try_get("capability")?)?);
    }
    Ok(calls)
}

async fn insert_batch_child_executions_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    attempt_id: BatchExecutionAttemptId,
    child_count: usize,
    worker_id: &str,
    correlation_id: &str,
    at: Timestamp,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'batch_execution_child_executions_created', \
                 'batch_execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(at))
    .bind(worker_id)
    .bind(batch.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": attempt_id,
            "child_count": child_count,
            "execution_ids": "[REDACTED]",
            "scheduler_visible": false,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn fetch_batch_runtime_settings(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<
    Option<(
        BatchExecutionAttemptId,
        ExecutionRuntimeSettingsSnapshot,
        String,
    )>,
    StorageError,
> {
    let row = sqlx::query(
        "SELECT settings.batch_execution_attempt_id, settings.provider_id AS snapshot_provider_id, \
                settings.schema_version, settings.resolved_settings_json, settings.sources_json, \
                settings.provider_revision, settings.provider_account_revision, \
                settings.completion_policy_json, settings.captured_at, \
                account.provider_id AS actual_provider_id, batch.state \
         FROM batch_execution_runtime_settings AS settings \
         INNER JOIN batch_executions AS batch ON batch.id = settings.batch_execution_id \
         INNER JOIN provider_accounts AS account ON account.id = batch.provider_account_id \
         WHERE settings.batch_execution_id = ?",
    )
    .bind(batch_execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref().map(decode_batch_runtime_settings).transpose()
}

fn decode_batch_runtime_settings(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<
    (
        BatchExecutionAttemptId,
        ExecutionRuntimeSettingsSnapshot,
        String,
    ),
    StorageError,
> {
    let provider_id = ProviderId::new(row.try_get::<String, _>("snapshot_provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    let actual_provider_id = ProviderId::new(row.try_get::<String, _>("actual_provider_id")?)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if provider_id != actual_provider_id {
        return Err(StorageError::InvalidData(
            "batch runtime settings Provider binding is invalid".to_owned(),
        ));
    }
    let snapshot = ExecutionRuntimeSettingsSnapshot {
        provider_id,
        resolved: ResolvedProviderRuntimeSettings {
            schema_version: u32::try_from(row.try_get::<i64, _>("schema_version")?).map_err(
                |_| {
                    StorageError::InvalidData("batch settings schema version is invalid".to_owned())
                },
            )?,
            values: serde_json::from_str::<std::collections::BTreeMap<String, ProviderSettingValue>>(
                row.try_get("resolved_settings_json")?,
            )?,
        },
        sources: serde_json::from_str(row.try_get("sources_json")?)?,
        completion_policy: serde_json::from_str(row.try_get("completion_policy_json")?)?,
        provider_revision: decode_optional_batch_revision(row.try_get("provider_revision")?)?,
        provider_account_revision: decode_optional_batch_revision(
            row.try_get("provider_account_revision")?,
        )?,
        task_revision: None,
        captured_at: decode_timestamp(row.try_get("captured_at")?)?,
    };
    if !valid_batch_runtime_settings_shape(&snapshot) {
        return Err(StorageError::InvalidData(
            "persisted batch runtime settings are invalid".to_owned(),
        ));
    }
    Ok((
        BatchExecutionAttemptId::from_str(row.try_get("batch_execution_attempt_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        snapshot,
        row.try_get("state")?,
    ))
}

fn validate_batch_runtime_settings_snapshot(
    snapshot: &ExecutionRuntimeSettingsSnapshot,
    schema: &ProviderRuntimeSettingsSchema,
    provider_id: &ProviderId,
    at: Timestamp,
) -> Result<(), StorageError> {
    if snapshot.provider_id != *provider_id
        || snapshot.captured_at != at
        || snapshot.completion_policy.captured_at != at
        || snapshot.task_revision.is_some()
        || schema.validate_resolved(&snapshot.resolved).is_err()
        || !valid_batch_runtime_settings_shape(snapshot)
    {
        return Err(StorageError::BatchExecutionStateConflict);
    }
    Ok(())
}

fn valid_batch_runtime_settings_shape(snapshot: &ExecutionRuntimeSettingsSnapshot) -> bool {
    let revisions_valid = [
        snapshot.provider_revision,
        snapshot.provider_account_revision,
    ]
    .into_iter()
    .flatten()
    .all(|revision| revision > 0);
    let keys_match = snapshot.sources.len() == snapshot.resolved.values.len()
        && snapshot
            .resolved
            .values
            .keys()
            .all(|key| snapshot.sources.contains_key(key));
    let sources_bound = snapshot.sources.values().all(|source| match source {
        ProviderRuntimeSettingSource::SchemaDefault => true,
        ProviderRuntimeSettingSource::Provider => snapshot.provider_revision.is_some(),
        ProviderRuntimeSettingSource::ProviderAccount => {
            snapshot.provider_account_revision.is_some()
        }
        ProviderRuntimeSettingSource::Task => false,
    });
    snapshot.resolved.schema_version > 0
        && snapshot.task_revision.is_none()
        && snapshot.completion_policy.captured_at == snapshot.captured_at
        && snapshot.completion_policy.validate().is_ok()
        && revisions_valid
        && keys_match
        && sources_bound
        && serde_json::to_vec(&snapshot.resolved.values)
            .is_ok_and(|value| value.len() <= 1024 * 1024)
        && serde_json::to_vec(&snapshot.sources).is_ok_and(|value| value.len() <= 1024 * 1024)
        && serde_json::to_vec(&snapshot.completion_policy)
            .is_ok_and(|value| value.len() <= 1024 * 1024)
}

fn decode_optional_batch_revision(value: Option<i64>) -> Result<Option<u32>, StorageError> {
    value
        .map(|revision| {
            u32::try_from(revision)
                .map_err(|_| StorageError::InvalidData("settings revision is invalid".to_owned()))
        })
        .transpose()
}

async fn insert_batch_runtime_settings_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    attempt_id: BatchExecutionAttemptId,
    snapshot: &ExecutionRuntimeSettingsSnapshot,
    worker_id: &str,
    correlation_id: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'batch_execution_runtime_settings_frozen', \
                 'batch_execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(snapshot.captured_at))
    .bind(worker_id)
    .bind(batch_execution_id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": attempt_id,
            "provider_id": snapshot.provider_id,
            "schema_version": snapshot.resolved.schema_version,
            "provider_revision": snapshot.provider_revision,
            "provider_account_revision": snapshot.provider_account_revision,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn stored_child_plans_match(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    attempt_id: BatchExecutionAttemptId,
    plan: &ProviderExecutionBatchPlan,
) -> Result<bool, StorageError> {
    for child in plan.children() {
        let artifact = child
            .execution_plan()
            .artifact()
            .ok_or(StorageError::BatchExecutionStateConflict)?;
        let row = sqlx::query(
            "SELECT stored.batch_execution_attempt_id, stored.task_id, \
                    stored.remote_task_id_digest, stored.provider_id, stored.calls_json, \
                    stored.artifact_type, stored.artifact_digest, stored.artifact_payload_json, \
                    stored.sequence_type, stored.sequence_digest, task.remote_id \
             FROM batch_execution_child_plans AS stored \
             INNER JOIN tasks AS task ON task.id = stored.task_id \
             WHERE stored.batch_execution_id = ? AND stored.position = ?",
        )
        .bind(batch.id.to_string())
        .bind(i64::from(child.position()))
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let sequence = child.mutation_sequence_plan();
        let remote_id: String = row.try_get("remote_id")?;
        if row.try_get::<String, _>("batch_execution_attempt_id")? != attempt_id.to_string()
            || row.try_get::<String, _>("provider_id")? != plan.provider_id().as_str()
            || row.try_get::<String, _>("calls_json")?
                != serde_json::to_string(child.execution_plan().calls())?
            || row.try_get::<String, _>("artifact_type")? != artifact.artifact_type()
            || decode_storage_digest(row.try_get("artifact_digest")?)? != artifact.artifact_digest()
            || row.try_get::<String, _>("artifact_payload_json")?
                != serde_json::to_string(artifact.payload_sanitized())?
            || row.try_get::<String, _>("sequence_type")? != sequence.sequence_type()
            || decode_storage_digest(row.try_get("sequence_digest")?)? != sequence.plan_digest()
            || remote_id != child.remote_task_id()
            || decode_storage_digest(row.try_get("remote_task_id_digest")?)?
                != remote_task_id_digest(child.remote_task_id())
            || !stored_phases_match(transaction, batch.id, child.position(), sequence).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn stored_phases_match(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    child_position: u32,
    sequence: &asterism_provider_api::ExecutionMutationSequencePlan,
) -> Result<bool, StorageError> {
    let rows = sqlx::query(
        "SELECT phase_position, operation_type, minimum_occurrences, maximum_occurrences, \
                stop_repeating_after_rejection, advance_condition, required_observation_type \
         FROM batch_execution_child_plan_phases \
         WHERE batch_execution_id = ? AND child_position = ? ORDER BY phase_position",
    )
    .bind(batch_execution_id.to_string())
    .bind(i64::from(child_position))
    .fetch_all(&mut **transaction)
    .await?;
    if rows.len() != sequence.phases().len() {
        return Ok(false);
    }
    for (index, (row, phase)) in rows.iter().zip(sequence.phases()).enumerate() {
        if row.try_get::<i64, _>("phase_position")?
            != i64::try_from(index + 1).expect("sequence phases are bounded")
            || row.try_get::<String, _>("operation_type")? != phase.operation_type()
            || row.try_get::<i64, _>("minimum_occurrences")?
                != i64::from(phase.minimum_occurrences())
            || row.try_get::<i64, _>("maximum_occurrences")?
                != i64::from(phase.maximum_occurrences())
            || row.try_get::<bool, _>("stop_repeating_after_rejection")?
                != phase.stop_repeating_after_rejection()
            || row.try_get::<String, _>("advance_condition")?
                != advance_condition_name(phase.advance_condition())
            || row
                .try_get::<Option<String>, _>("required_observation_type")?
                .as_deref()
                != phase.required_observation_type()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn load_child_plan_records(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Vec<BatchExecutionChildPlanRecord>, StorageError> {
    sqlx::query(
        "SELECT batch_execution_id, batch_execution_attempt_id, position, task_id, \
                artifact_digest, sequence_digest, materialized_at \
         FROM batch_execution_child_plans WHERE batch_execution_id = ? ORDER BY position",
    )
    .bind(batch_execution_id.to_string())
    .fetch_all(&mut **transaction)
    .await?
    .iter()
    .map(decode_child_plan_record)
    .collect()
}

fn decode_child_plan_record(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BatchExecutionChildPlanRecord, StorageError> {
    Ok(BatchExecutionChildPlanRecord {
        batch_execution_id: BatchExecutionId::from_str(row.try_get("batch_execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        attempt_id: BatchExecutionAttemptId::from_str(row.try_get("batch_execution_attempt_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        position: u32::try_from(row.try_get::<i64, _>("position")?)
            .map_err(|_| StorageError::InvalidData("batch child position is invalid".to_owned()))?,
        task_id: TaskId::from_str(row.try_get("task_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        artifact_digest: decode_storage_digest(row.try_get("artifact_digest")?)?,
        sequence_digest: decode_storage_digest(row.try_get("sequence_digest")?)?,
        materialized_at: decode_timestamp(row.try_get("materialized_at")?)?,
    })
}

fn canonical_capabilities(calls: &[Vec<TaskCapability>]) -> Vec<TaskCapability> {
    let mut capabilities = calls.iter().flatten().copied().collect::<Vec<_>>();
    capabilities.sort_unstable();
    capabilities.dedup();
    capabilities
}

fn remote_task_id_digest(remote_task_id: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"asterism.batch-execution-child-remote-task.v1\0");
    digest.update(remote_task_id.as_bytes());
    digest.finalize().into()
}

const fn advance_condition_name(
    condition: ExecutionMutationSequenceAdvanceCondition,
) -> &'static str {
    match condition {
        ExecutionMutationSequenceAdvanceCondition::MaximumReached => "maximum_reached",
        ExecutionMutationSequenceAdvanceCondition::AcceptedMaximumReached => {
            "accepted_maximum_reached"
        }
        ExecutionMutationSequenceAdvanceCondition::AcceptedOrMaximumReached => {
            "accepted_or_maximum_reached"
        }
        ExecutionMutationSequenceAdvanceCondition::RejectedOrMaximumReached => {
            "rejected_or_maximum_reached"
        }
    }
}

async fn insert_batch_child_plans_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    attempt_id: BatchExecutionAttemptId,
    child_count: usize,
    worker_id: &str,
    correlation_id: &str,
    at: Timestamp,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'batch_execution_child_plans_materialized', \
                 'batch_execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(at))
    .bind(worker_id)
    .bind(batch.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": attempt_id,
            "course_id": batch.course_id,
            "child_count": child_count,
            "artifact_digests": "[HASHED]",
            "sequence_digests": "[HASHED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_batch_planning_input(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    keyring: &SecretKeyring,
    owner_user_id: UserId,
    request: &BatchExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    let batch = request.batch_execution;
    let (key_id, key) = keyring.active();
    let secret = SecretRef {
        id: SecretId::new(),
        owner_user_id,
        purpose: SecretPurpose::ProviderExecutionState,
        version: 1,
        key_id: key_id.to_owned(),
        created_at: batch.created_at,
        updated_at: batch.created_at,
    };
    let (nonce, encrypted) = encrypt(
        key,
        &secret,
        request.planning_input.payload().expose_secret(),
    )?;
    insert_secret_blob(transaction, &secret, &nonce, &encrypted).await?;
    sqlx::query(
        "INSERT INTO batch_execution_planning_inputs \
         (batch_execution_id, provider_id, input_type, input_digest, secret_blob_id, bound_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(batch.id.to_string())
    .bind(request.planning_input.provider_id().as_str())
    .bind(request.planning_input.input_type())
    .bind(request.planning_input.input_digest().to_vec())
    .bind(secret.id.to_string())
    .bind(encode_timestamp(batch.created_at))
    .execute(&mut **transaction)
    .await?;
    insert_secret_audit(
        transaction,
        &planning_input_access(request),
        "batch_execution_planning_input_stored",
        &secret,
    )
    .await?;
    Ok(())
}

async fn stored_planning_input_matches(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    input: &ProviderBatchExecutionPlanningInput,
) -> Result<bool, StorageError> {
    let row = sqlx::query(
        "SELECT provider_id, input_type, input_digest \
         FROM batch_execution_planning_inputs WHERE batch_execution_id = ?",
    )
    .bind(batch_execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    Ok(
        row.try_get::<String, _>("provider_id")? == input.provider_id().as_str()
            && row.try_get::<String, _>("input_type")? == input.input_type()
            && decode_storage_digest(row.try_get("input_digest")?)? == input.input_digest(),
    )
}

fn planning_input_access(request: &BatchExecutionScheduleRequest<'_>) -> SecretAccess {
    SecretAccess {
        actor: match request.actor {
            AuditActor::User(id) => SecretActor::User(id),
            AuditActor::ServiceToken(id) => SecretActor::ServiceToken(id),
        },
        correlation_id: request.correlation_id.to_owned(),
        reason: "freeze batch execution product authorization before scheduling".to_owned(),
    }
}

struct StoredPlanningInput {
    metadata: BatchExecutionPlanningInputRecord,
    secret_id: SecretId,
    owner_user_id: UserId,
    actual_provider_id: ProviderId,
    state: String,
}

async fn fetch_stored_planning_input(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Option<StoredPlanningInput>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT input.batch_execution_id, input.provider_id AS input_provider_id, \
                input.input_type, input.input_digest, input.secret_blob_id, input.bound_at, \
                account.owner_user_id, account.provider_id AS actual_provider_id, batch.state \
         FROM batch_execution_planning_inputs AS input \
         INNER JOIN batch_executions AS batch ON batch.id = input.batch_execution_id \
         INNER JOIN provider_accounts AS account ON account.id = batch.provider_account_id \
         WHERE input.batch_execution_id = ?",
    )
    .bind(batch_execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(planning_input_storage_error)?;
    row.as_ref().map(decode_stored_planning_input).transpose()
}

fn decode_stored_planning_input(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<StoredPlanningInput, SecretStoreError> {
    let provider_id = ProviderId::new(
        row.try_get::<String, _>("input_provider_id")
            .map_err(planning_input_storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    let actual_provider_id = ProviderId::new(
        row.try_get::<String, _>("actual_provider_id")
            .map_err(planning_input_storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    Ok(StoredPlanningInput {
        metadata: BatchExecutionPlanningInputRecord {
            batch_execution_id: BatchExecutionId::from_str(
                row.try_get("batch_execution_id")
                    .map_err(planning_input_storage_error)?,
            )
            .map_err(|_| SecretStoreError::Storage)?,
            provider_id,
            input_type: row
                .try_get("input_type")
                .map_err(planning_input_storage_error)?,
            input_digest: decode_secret_digest(
                row.try_get("input_digest")
                    .map_err(planning_input_storage_error)?,
            )?,
            bound_at: decode_timestamp(
                row.try_get("bound_at")
                    .map_err(planning_input_storage_error)?,
            )
            .map_err(|_| SecretStoreError::Storage)?,
        },
        secret_id: SecretId::from_str(
            row.try_get("secret_blob_id")
                .map_err(planning_input_storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        owner_user_id: UserId::from_str(
            row.try_get("owner_user_id")
                .map_err(planning_input_storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        actual_provider_id,
        state: row.try_get("state").map_err(planning_input_storage_error)?,
    })
}

fn validate_planning_input_access_context(
    request: &BatchExecutionPlanningInputResolveRequest<'_>,
) -> Result<(), SecretStoreError> {
    if valid_token(request.worker_id, 256)
        && valid_token(request.correlation_id, 256)
        && request.access.correlation_id == request.correlation_id
    {
        Ok(())
    } else {
        Err(SecretStoreError::InvalidValue)
    }
}

fn authorize_planning_input_access(
    owner_user_id: UserId,
    provider_id: &ProviderId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    if !access.authorizes(owner_user_id) {
        return Err(SecretStoreError::Unauthorized);
    }
    match &access.actor {
        SecretActor::CoreService(_) => Ok(()),
        SecretActor::ProviderRuntime(actual) if actual == provider_id.as_str() => Ok(()),
        SecretActor::User(_) | SecretActor::ServiceToken(_) | SecretActor::ProviderRuntime(_) => {
            Err(SecretStoreError::Unauthorized)
        }
    }
}

fn decode_secret_digest(bytes: Vec<u8>) -> Result<[u8; 32], SecretStoreError> {
    bytes.try_into().map_err(|_| SecretStoreError::Storage)
}

fn decode_storage_digest(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    bytes
        .try_into()
        .map_err(|_| StorageError::InvalidData("batch planning input digest is invalid".to_owned()))
}

fn planning_input_storage_error(_error: sqlx::Error) -> SecretStoreError {
    SecretStoreError::Storage
}

const BATCH_EXECUTION_SELECT_BY_ID: &str = "SELECT id, provider_account_id, course_id, requested_capabilities_json, \
            expected_child_count, requested_by, request_source, state, scheduled_at, \
            started_at, finished_at, created_at \
     FROM batch_executions WHERE id = ?";
const BATCH_EXECUTION_SELECT_BY_IDEMPOTENCY: &str = "SELECT id, provider_account_id, course_id, requested_capabilities_json, \
            expected_child_count, requested_by, request_source, state, scheduled_at, \
            started_at, finished_at, created_at \
     FROM batch_executions WHERE idempotency_scope = ? AND idempotency_key = ?";

fn validate_schedule_request(
    request: &BatchExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    request
        .batch_execution
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    validate_idempotency(request.idempotency_scope, request.idempotency_key)?;
    if request.batch_execution.state != ExecutionState::Scheduled
        || !valid_token(request.correlation_id, 256)
    {
        return Err(StorageError::InvalidData(
            "batch execution schedule request is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_idempotency(scope: &str, key: &str) -> Result<(), StorageError> {
    if valid_token(scope, 256) && valid_token(key, 256) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "batch execution idempotency identity is invalid".to_owned(),
        ))
    }
}

fn valid_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn decode_batch_execution(row: &sqlx::sqlite::SqliteRow) -> Result<BatchExecution, StorageError> {
    let execution = BatchExecution {
        id: BatchExecutionId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        course_id: asterism_domain::CourseId::from_str(row.try_get("course_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        requested_capabilities: serde_json::from_str::<Vec<TaskCapability>>(
            row.try_get("requested_capabilities_json")?,
        )?,
        expected_child_count: u32::try_from(row.try_get::<i64, _>("expected_child_count")?)
            .map_err(|_| {
                StorageError::InvalidData("batch execution child count is invalid".to_owned())
            })?,
        requested_by: row
            .try_get::<Option<&str>, _>("requested_by")?
            .map(UserId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        request_source: decode_enum(row.try_get("request_source")?)?,
        state: decode_enum(row.try_get("state")?)?,
        scheduled_at: decode_optional_timestamp(row.try_get("scheduled_at")?)?,
        started_at: decode_optional_timestamp(row.try_get("started_at")?)?,
        finished_at: decode_optional_timestamp(row.try_get("finished_at")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
    };
    execution
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(execution)
}

fn decode_batch_attempt(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<BatchExecutionAttempt, StorageError> {
    Ok(BatchExecutionAttempt {
        id: BatchExecutionAttemptId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        batch_execution_id: BatchExecutionId::from_str(row.try_get("batch_execution_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        attempt_no: u32::try_from(row.try_get::<i64, _>("attempt_no")?).map_err(|_| {
            StorageError::InvalidData("batch execution attempt number is invalid".to_owned())
        })?,
        started_at: decode_timestamp(row.try_get("started_at")?)?,
        finished_at: decode_optional_timestamp(row.try_get("finished_at")?)?,
        result: row
            .try_get::<Option<&str>, _>("result")?
            .map(decode_enum::<AttemptResult>)
            .transpose()?,
        error_class: row
            .try_get::<Option<&str>, _>("error_class")?
            .map(decode_enum::<ProviderErrorClass>)
            .transpose()?,
        provider_trace_id: row.try_get("provider_trace_id")?,
    })
}

async fn find_active_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Option<BatchExecutionAttempt>, StorageError> {
    sqlx::query(
        "SELECT id, batch_execution_id, attempt_no, started_at, finished_at, result, \
                error_class, provider_trace_id FROM batch_execution_attempts \
         WHERE batch_execution_id = ? AND finished_at IS NULL \
         ORDER BY attempt_no DESC LIMIT 1",
    )
    .bind(batch_execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .as_ref()
    .map(decode_batch_attempt)
    .transpose()
}

async fn assert_batch_scheduler_claim(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    scheduler_job_id: asterism_domain::ScheduleId,
    worker_id: &str,
    at: Timestamp,
) -> Result<Timestamp, StorageError> {
    let row = sqlx::query(
        "SELECT payload_json, lease_expires_at FROM scheduled_jobs \
         WHERE id = ? AND job_kind = 'batch_execution' AND state = 'claimed' AND worker_id = ?",
    )
    .bind(scheduler_job_id.to_string())
    .bind(worker_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::BatchExecutionClaimLost)?;
    let lease_expires_at = decode_timestamp(row.try_get("lease_expires_at")?)?;
    let kind: ScheduledJobKind = serde_json::from_str(row.try_get("payload_json")?)?;
    if lease_expires_at <= at || kind != (ScheduledJobKind::BatchExecution { batch_execution_id }) {
        return Err(StorageError::BatchExecutionClaimLost);
    }
    Ok(lease_expires_at)
}

pub(crate) async fn assert_batch_worker_claims(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    attempt_id: BatchExecutionAttemptId,
    scheduler_job_id: asterism_domain::ScheduleId,
    worker_id: &str,
    at: Timestamp,
) -> Result<BatchExecutionAttempt, StorageError> {
    assert_batch_scheduler_claim(
        transaction,
        batch_execution_id,
        scheduler_job_id,
        worker_id,
        at,
    )
    .await?;
    assert_batch_lease(transaction, batch_execution_id, worker_id, at).await?;
    let attempt = find_active_attempt(transaction, batch_execution_id)
        .await?
        .ok_or(StorageError::BatchExecutionAttemptNotActive)?;
    if attempt.id == attempt_id {
        Ok(attempt)
    } else {
        Err(StorageError::BatchExecutionAttemptNotActive)
    }
}

pub(crate) async fn assert_batch_lease(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch_execution_id: BatchExecutionId,
    worker_id: &str,
    at: Timestamp,
) -> Result<(), StorageError> {
    let owned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM batch_execution_leases \
         WHERE batch_execution_id = ? AND worker_id = ? AND expires_at > ?",
    )
    .bind(batch_execution_id.to_string())
    .bind(worker_id)
    .bind(encode_timestamp(at))
    .fetch_one(&mut **transaction)
    .await?;
    if owned == 1 {
        Ok(())
    } else {
        Err(StorageError::BatchExecutionClaimLost)
    }
}

fn validate_worker_context(worker_id: &str, correlation_id: &str) -> Result<(), StorageError> {
    if valid_token(worker_id, 256) && valid_token(correlation_id, 256) {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "batch execution worker identity is invalid".to_owned(),
        ))
    }
}

async fn insert_batch_worker_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    batch: &BatchExecution,
    attempt: &BatchExecutionAttempt,
    worker_id: &str,
    correlation_id: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'batch_execution_attempt_started', 'batch_execution', \
                 ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(attempt.started_at))
    .bind(worker_id)
    .bind(batch.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": attempt.id,
            "attempt_no": attempt.attempt_no,
            "provider_account_id": batch.provider_account_id,
            "course_id": batch.course_id,
            "expected_child_count": batch.expected_child_count,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_batch_execution_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &BatchExecutionScheduleRequest<'_>,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match request.actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let batch = request.batch_execution;
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'batch_execution_requested', 'batch_execution', ?, ?, \
                 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(batch.created_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(batch.id.to_string())
    .bind(request.correlation_id)
    .bind(
        serde_json::json!({
            "provider_account_id": batch.provider_account_id,
            "course_id": batch.course_id,
            "requested_capabilities": batch.requested_capabilities,
            "expected_child_count": batch.expected_child_count,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn enum_name(value: impl serde::Serialize) -> Result<String, StorageError> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(StorageError::InvalidData(
            "enum did not serialize to a string".to_owned(),
        )),
    }
}

fn decode_enum<T>(value: &str) -> Result<T, StorageError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(Into::into)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn decode_optional_timestamp(value: Option<&str>) -> Result<Option<Timestamp>, StorageError> {
    value.map(decode_timestamp).transpose()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use super::*;
    use asterism_domain::{
        BatchExecutionId, CourseId, ProviderAccountId, ProviderId, RequestSource, UserId,
    };
    use asterism_provider_api::{
        ExecutionParentBatchSnapshot, ProviderBatchExecutionPlanningInput,
    };
    use asterism_secrets::{SecretAccess, SecretActor, SecretKey, SecretStoreError, SecretValue};
    use chrono::Duration;

    use crate::{
        BatchExecutionParentSnapshotBindOutcome, BatchExecutionParentSnapshotBindRequest,
        BatchExecutionParentSnapshotRepository, BatchExecutionParentSnapshotResolveRequest,
        SchedulerRepository, SecretKeyring, SqliteSchedulerRepository, SqliteSecretStore,
    };

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the test keeps scheduling, claim, lease, attempt and idempotent restart boundaries together"
    )]
    async fn scheduling_is_course_bound_atomic_and_idempotent_without_touching_tasks() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner = UserId::new();
        let account_id = ProviderAccountId::new();
        let course_id = CourseId::new();
        insert_fixture(&database, owner, account_id, course_id, now).await;
        let keyring = Arc::new(
            SecretKeyring::new(
                "batch-parent-key",
                BTreeMap::from([("batch-parent-key".to_owned(), SecretKey::new([31; 32]))]),
            )
            .unwrap(),
        );
        let repository = SqliteBatchExecutionRepository::new(database.clone(), keyring.clone());
        let planning_input = ProviderBatchExecutionPlanningInput::try_new(
            ProviderId::new("test").unwrap(),
            "test.course-batch-request.v1",
            SecretValue::new(b"PRIVATE_BATCH_SELECTION".to_vec()),
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
        let request = || BatchExecutionScheduleRequest {
            batch_execution: &batch,
            planning_input: &planning_input,
            idempotency_scope: "user:test-owner",
            idempotency_key: "course-batch-1",
            actor: AuditActor::User(owner),
            correlation_id: "course-batch-request",
        };
        assert_eq!(
            repository
                .schedule_batch_execution(request())
                .await
                .unwrap(),
            BatchExecutionScheduleOutcome::Created(batch.clone())
        );
        assert_eq!(
            repository
                .schedule_batch_execution(request())
                .await
                .unwrap(),
            BatchExecutionScheduleOutcome::Existing(batch.clone())
        );
        let changed_planning_input = ProviderBatchExecutionPlanningInput::try_new(
            ProviderId::new("test").unwrap(),
            "test.course-batch-request.v1",
            SecretValue::new(b"CHANGED_PRIVATE_BATCH_SELECTION".to_vec()),
        )
        .unwrap();
        let mut conflict = request();
        conflict.planning_input = &changed_planning_input;
        assert_eq!(
            repository.schedule_batch_execution(conflict).await.unwrap(),
            BatchExecutionScheduleOutcome::IdempotencyConflict
        );
        assert_eq!(
            repository.find_batch_execution(batch.id).await.unwrap(),
            Some(batch.clone())
        );
        assert_eq!(
            repository
                .find_idempotent_batch_execution("user:test-owner", "course-batch-1")
                .await
                .unwrap(),
            Some(batch.clone())
        );
        let job: (String, String) = sqlx::query_as(
            "SELECT job_kind, payload_json FROM scheduled_jobs \
             WHERE idempotency_key LIKE 'batch-execution:%'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(job.0, "batch_execution");
        assert!(job.1.contains("batch_execution_id"));
        let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(task_count, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_records WHERE action = 'batch_execution_requested'",
            )
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );
        let planning_ciphertext: Vec<u8> = sqlx::query_scalar(
            "SELECT secret.encrypted_data \
             FROM batch_execution_planning_inputs AS input \
             INNER JOIN secret_blobs AS secret ON secret.id = input.secret_blob_id \
             WHERE input.batch_execution_id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(
            !planning_ciphertext
                .windows(b"PRIVATE_BATCH_SELECTION".len())
                .any(|window| window == b"PRIVATE_BATCH_SELECTION")
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM event_outbox WHERE event_type = 'batch_execution_state_changed'",
            )
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );

        let scheduler = SqliteSchedulerRepository::new(database.clone());
        let claimed = scheduler
            .claim_due_batch_execution_jobs("batch-worker", now, now + Duration::minutes(5), 1)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let start = || BatchExecutionAttemptStartRequest {
            batch_execution_id: batch.id,
            scheduler_job_id: claimed[0].id,
            worker_id: "batch-worker",
            at: now + Duration::seconds(1),
            correlation_id: "batch-attempt-start",
        };
        let attempt = repository
            .start_batch_execution_attempt(start())
            .await
            .unwrap();
        assert_eq!(attempt.batch_execution_id, batch.id);
        assert_eq!(attempt.attempt_no, 1);
        assert_eq!(
            repository
                .start_batch_execution_attempt(start())
                .await
                .unwrap(),
            attempt
        );
        assert_eq!(
            repository
                .find_active_batch_execution_attempt(batch.id)
                .await
                .unwrap(),
            Some(attempt.clone())
        );
        assert_eq!(
            repository
                .find_batch_execution(batch.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ExecutionState::Running
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM batch_execution_leases \
                 WHERE batch_execution_id = ? AND worker_id = 'batch-worker'",
            )
            .bind(batch.id.to_string())
            .fetch_one(database.pool())
            .await
            .unwrap(),
            1
        );
        let planning_access = SecretAccess {
            actor: SecretActor::CoreService("batch-execution-worker"),
            correlation_id: "batch-planning-resolve".to_owned(),
            reason: "resolve frozen product authorization for parent planning".to_owned(),
        };
        let resolved_input = repository
            .resolve_batch_execution_planning_input(BatchExecutionPlanningInputResolveRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: claimed[0].id,
                worker_id: "batch-worker",
                correlation_id: "batch-planning-resolve",
                at: now + Duration::seconds(2),
                access: &planning_access,
            })
            .await
            .unwrap();
        assert_eq!(
            resolved_input.metadata.input_digest,
            planning_input.input_digest()
        );
        assert_eq!(
            resolved_input.input.payload().expose_secret(),
            b"PRIVATE_BATCH_SELECTION"
        );
        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = X'00' WHERE id = ( \
                SELECT secret_blob_id FROM batch_execution_planning_inputs \
                WHERE batch_execution_id = ? \
             )",
        )
        .bind(batch.id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        assert!(matches!(
            repository
                .resolve_batch_execution_planning_input(BatchExecutionPlanningInputResolveRequest {
                    batch_execution_id: batch.id,
                    attempt_id: attempt.id,
                    scheduler_job_id: claimed[0].id,
                    worker_id: "batch-worker",
                    correlation_id: "batch-planning-resolve",
                    at: now + Duration::seconds(2),
                    access: &planning_access,
                })
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));

        let parent_repository = SqliteSecretStore::new(database.clone(), keyring)
            .batch_execution_parent_snapshots(ProviderId::new("test").unwrap());
        let bound_at = now + Duration::seconds(2);
        let access = SecretAccess {
            actor: SecretActor::CoreService("batch-execution-worker"),
            correlation_id: "batch-parent-bind".to_owned(),
            reason: "freeze complete course batch before child creation".to_owned(),
        };
        let snapshot = || {
            ExecutionParentBatchSnapshot::try_new(
                ProviderId::new("test").unwrap(),
                "test.batch-parent-authority.v1",
                SecretValue::new(b"BATCH_PARENT_AUTHORITY".to_vec()),
                "test.complete-course-batch.v1",
                SecretValue::new(b"COMPLETE_COURSE_BATCH".to_vec()),
            )
            .unwrap()
        };
        let bind_request = |snapshot| BatchExecutionParentSnapshotBindRequest {
            batch_execution_id: batch.id,
            attempt_id: attempt.id,
            scheduler_job_id: claimed[0].id,
            worker_id: "batch-worker",
            snapshot,
            correlation_id: "batch-parent-bind",
            at: bound_at,
            access: &access,
        };
        let first = snapshot();
        let expected = crate::BatchExecutionParentSnapshotRecord {
            batch_execution_id: batch.id,
            attempt_id: attempt.id,
            provider_id: ProviderId::new("test").unwrap(),
            authority_type: "test.batch-parent-authority.v1".to_owned(),
            authority_digest: first.authority_digest(),
            batch_type: "test.complete-course-batch.v1".to_owned(),
            batch_digest: first.batch_digest(),
            bound_at,
        };
        assert_eq!(
            parent_repository
                .bind_batch_execution_parent_snapshot(bind_request(first))
                .await
                .unwrap(),
            BatchExecutionParentSnapshotBindOutcome::Bound(expected.clone())
        );
        let resolved = parent_repository
            .resolve_batch_execution_parent_snapshot(BatchExecutionParentSnapshotResolveRequest {
                batch_execution_id: batch.id,
                attempt_id: attempt.id,
                scheduler_job_id: claimed[0].id,
                worker_id: "batch-worker",
                correlation_id: "batch-parent-bind",
                at: bound_at,
                access: &access,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.metadata, expected);
        assert_eq!(
            resolved.snapshot.authority().expose_secret(),
            b"BATCH_PARENT_AUTHORITY"
        );
        assert_eq!(
            resolved.snapshot.batch().expose_secret(),
            b"COMPLETE_COURSE_BATCH"
        );
        assert_eq!(
            parent_repository
                .bind_batch_execution_parent_snapshot(bind_request(snapshot()))
                .await
                .unwrap(),
            BatchExecutionParentSnapshotBindOutcome::AlreadyBound(expected.clone())
        );

        let encrypted: (Vec<u8>, Vec<u8>) = sqlx::query_as(
            "SELECT authority.encrypted_data, child_batch.encrypted_data \
             FROM batch_execution_parent_snapshots AS snapshot \
             INNER JOIN secret_blobs AS authority \
                ON authority.id = snapshot.authority_secret_blob_id \
             INNER JOIN secret_blobs AS child_batch \
                ON child_batch.id = snapshot.batch_secret_blob_id \
             WHERE snapshot.batch_execution_id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(
            !encrypted
                .0
                .windows(b"BATCH_PARENT_AUTHORITY".len())
                .any(|window| window == b"BATCH_PARENT_AUTHORITY")
        );
        assert!(
            !encrypted
                .1
                .windows(b"COMPLETE_COURSE_BATCH".len())
                .any(|window| window == b"COMPLETE_COURSE_BATCH")
        );
        let audit_metadata: String = sqlx::query_scalar(
            "SELECT metadata_sanitized_json FROM audit_records \
             WHERE action = 'batch_execution_parent_snapshot_bound' AND resource_id = ?",
        )
        .bind(batch.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(!audit_metadata.contains("BATCH_PARENT_AUTHORITY"));
        assert!(!audit_metadata.contains("COMPLETE_COURSE_BATCH"));
        assert!(audit_metadata.contains("[HASHED]"));

        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = X'00' WHERE id = ( \
                SELECT batch_secret_blob_id FROM batch_execution_parent_snapshots \
                WHERE batch_execution_id = ? \
             )",
        )
        .bind(batch.id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        assert!(matches!(
            parent_repository
                .resolve_batch_execution_parent_snapshot(
                    BatchExecutionParentSnapshotResolveRequest {
                        batch_execution_id: batch.id,
                        attempt_id: attempt.id,
                        scheduler_job_id: claimed[0].id,
                        worker_id: "batch-worker",
                        correlation_id: "batch-parent-bind",
                        at: bound_at,
                        access: &access,
                    }
                )
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    async fn insert_fixture(
        database: &Database,
        owner: UserId,
        account_id: ProviderAccountId,
        course_id: CourseId,
        now: Timestamp,
    ) {
        let timestamp = encode_timestamp(now);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(format!("user-{owner}"))
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'Test', '{\"state\":\"idle\"}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(ProviderId::new("test").unwrap().as_str())
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO courses \
             (id, provider_account_id, remote_id, title, metadata_json, last_seen_at) \
             VALUES (?, ?, 'course-remote', 'Course', '{}', ?)",
        )
        .bind(course_id.to_string())
        .bind(account_id.to_string())
        .bind(timestamp)
        .execute(database.pool())
        .await
        .unwrap();
    }
}
