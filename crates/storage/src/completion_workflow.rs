use std::str::FromStr;

use asterism_domain::{
    CompletionPolicySnapshot, CompletionWorkflowBinding, ExecutionAttemptId, ExecutionId,
    ProviderAccountId, ScoreImprovementState, ScoreImprovementWorkflow, ScoreImprovementWorkflowId,
    StrictCompletionState, StrictCompletionWorkflow, StrictCompletionWorkflowId, TaskId, Timestamp,
    UserId,
};
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::auth_session::{decode_timestamp, encode_timestamp};
use crate::execution::{assert_worker_claims, validate_worker_token};
use crate::{
    CompletionWorkflowCreateOutcome, CompletionWorkflowRepository, Database,
    ScoreImprovementBeginRequest, ScoreImprovementObserveRequest, ScoreImprovementWorkflowRecord,
    SqliteExecutionRepository, StorageError, StrictCompletionBeginRequest,
    StrictCompletionExecutionObservationRecord, StrictCompletionExecutionObservationRequest,
    StrictCompletionObserveRequest, StrictCompletionWorkflowRecord,
};

#[derive(Clone, Debug)]
pub struct SqliteCompletionWorkflowRepository {
    database: Database,
}

impl SqliteCompletionWorkflowRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl CompletionWorkflowRepository for SqliteCompletionWorkflowRepository {
    async fn create_strict_completion_workflow(
        &self,
        workflow: &StrictCompletionWorkflow,
    ) -> Result<CompletionWorkflowCreateOutcome<StrictCompletionWorkflowRecord>, StorageError> {
        workflow.validate().map_err(|_| invalid_workflow())?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        validate_binding(&mut transaction, workflow.binding).await?;
        let rows = sqlx::query(
            "SELECT * FROM strict_completion_workflows WHERE task_id = ? OR id = ? LIMIT 2",
        )
        .bind(workflow.binding.task_id.to_string())
        .bind(workflow.id.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        if !rows.is_empty() {
            let records = rows
                .iter()
                .map(decode_strict_record)
                .collect::<Result<Vec<_>, _>>()?;
            transaction.commit().await?;
            return if records.len() == 1 && records[0].workflow == *workflow {
                Ok(CompletionWorkflowCreateOutcome::Existing(
                    records.into_iter().next().expect("one record"),
                ))
            } else {
                Ok(CompletionWorkflowCreateOutcome::Conflict)
            };
        }
        sqlx::query(
            "INSERT INTO strict_completion_workflows \
             (id, owner_user_id, provider_account_id, task_id, state, workflow_json, revision, \
              created_at, updated_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
        )
        .bind(workflow.id.to_string())
        .bind(workflow.binding.owner_user_id.to_string())
        .bind(workflow.binding.provider_account_id.to_string())
        .bind(workflow.binding.task_id.to_string())
        .bind(encode_strict_state(workflow.state))
        .bind(serde_json::to_string(workflow)?)
        .bind(encode_timestamp(workflow.created_at))
        .bind(encode_timestamp(workflow.updated_at))
        .bind(workflow.finished_at.map(encode_timestamp))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(CompletionWorkflowCreateOutcome::Created(
            StrictCompletionWorkflowRecord {
                workflow: workflow.clone(),
                revision: 1,
            },
        ))
    }

    async fn find_owned_strict_completion_workflow(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<StrictCompletionWorkflowRecord>, StorageError> {
        let row = sqlx::query(
            "SELECT * FROM strict_completion_workflows WHERE owner_user_id = ? AND task_id = ?",
        )
        .bind(owner_user_id.to_string())
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_strict_record).transpose()
    }

    async fn begin_strict_completion_attempt(
        &self,
        request: StrictCompletionBeginRequest,
    ) -> Result<StrictCompletionWorkflowRecord, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut record = fetch_strict_record(
            &mut transaction,
            request.owner_user_id,
            request.workflow_id,
            request.expected_revision,
        )
        .await?;
        record
            .workflow
            .begin_attempt(
                request.formal_assessment,
                request.retry_confirmed,
                request.at,
            )
            .map_err(|_| invalid_transition())?;
        record.revision = persist_strict_record(
            &mut transaction,
            &record.workflow,
            request.expected_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(record)
    }

    async fn observe_strict_completion(
        &self,
        request: StrictCompletionObserveRequest,
    ) -> Result<StrictCompletionWorkflowRecord, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut record = fetch_strict_record(
            &mut transaction,
            request.owner_user_id,
            request.workflow_id,
            request.expected_revision,
        )
        .await?;
        record
            .workflow
            .observe(request.outcome, request.diagnosis, request.at)
            .map_err(|_| invalid_transition())?;
        record.revision = persist_strict_record(
            &mut transaction,
            &record.workflow,
            request.expected_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(record)
    }

    async fn record_strict_completion_execution_observation(
        &self,
        request: StrictCompletionExecutionObservationRequest<'_>,
    ) -> Result<StrictCompletionExecutionObservationRecord, StorageError> {
        validate_worker_token(request.worker_id, request.correlation_id)?;
        validate_execution_observation(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await?;
        if let Some(existing) = find_execution_observation(
            &mut transaction,
            request.execution_id,
            request.execution_attempt_id,
        )
        .await?
        {
            if existing.outcome == request.outcome && existing.diagnosis == request.diagnosis {
                transaction.commit().await?;
                return Ok(existing);
            }
            return Err(invalid_transition());
        }

        let binding = load_execution_observation_binding(&mut transaction, &request).await?;
        let (mut workflow, revision, created) =
            resolve_execution_workflow(&mut transaction, &binding, &request).await?;

        let workflow_attempt_no = if created && request.outcome.is_some() {
            None
        } else if let Some(outcome) = request.outcome {
            workflow
                .observe_verified_completion(outcome, request.at)
                .map_err(|_| invalid_transition())?;
            None
        } else if workflow.state == StrictCompletionState::Disabled {
            None
        } else if workflow.state == StrictCompletionState::Active {
            let attempt_no = workflow
                .begin_attempt(
                    binding.formal_assessment,
                    binding.retry_confirmation.is_some(),
                    binding.attempt_started_at,
                )
                .map_err(|_| invalid_transition())?;
            workflow
                .observe(None, request.diagnosis, request.at)
                .map_err(|_| invalid_transition())?;
            Some(attempt_no)
        } else {
            return Err(invalid_transition());
        };

        let revision = if created {
            insert_strict_record(&mut transaction, &workflow).await?;
            revision
        } else {
            persist_strict_record(&mut transaction, &workflow, revision).await?
        };
        insert_execution_observation(&mut transaction, &request, workflow.id, workflow_attempt_no)
            .await?;
        transaction.commit().await?;
        Ok(StrictCompletionExecutionObservationRecord {
            execution_id: request.execution_id,
            execution_attempt_id: request.execution_attempt_id,
            workflow: StrictCompletionWorkflowRecord { workflow, revision },
            workflow_attempt_no,
            outcome: request.outcome,
            diagnosis: request.diagnosis,
            observed_at: request.at,
        })
    }

    async fn create_score_improvement_workflow(
        &self,
        workflow: &ScoreImprovementWorkflow,
    ) -> Result<CompletionWorkflowCreateOutcome<ScoreImprovementWorkflowRecord>, StorageError> {
        workflow.validate().map_err(|_| invalid_workflow())?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        validate_binding(&mut transaction, workflow.binding).await?;
        let rows = sqlx::query(
            "SELECT * FROM score_improvement_workflows WHERE task_id = ? OR id = ? LIMIT 2",
        )
        .bind(workflow.binding.task_id.to_string())
        .bind(workflow.id.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        if !rows.is_empty() {
            let records = rows
                .iter()
                .map(decode_score_record)
                .collect::<Result<Vec<_>, _>>()?;
            transaction.commit().await?;
            return if records.len() == 1 && records[0].workflow == *workflow {
                Ok(CompletionWorkflowCreateOutcome::Existing(
                    records.into_iter().next().expect("one record"),
                ))
            } else {
                Ok(CompletionWorkflowCreateOutcome::Conflict)
            };
        }
        sqlx::query(
            "INSERT INTO score_improvement_workflows \
             (id, owner_user_id, provider_account_id, task_id, state, workflow_json, revision, \
              created_at, updated_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
        )
        .bind(workflow.id.to_string())
        .bind(workflow.binding.owner_user_id.to_string())
        .bind(workflow.binding.provider_account_id.to_string())
        .bind(workflow.binding.task_id.to_string())
        .bind(encode_score_state(workflow.state))
        .bind(serde_json::to_string(workflow)?)
        .bind(encode_timestamp(workflow.created_at))
        .bind(encode_timestamp(workflow.updated_at))
        .bind(workflow.finished_at.map(encode_timestamp))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(CompletionWorkflowCreateOutcome::Created(
            ScoreImprovementWorkflowRecord {
                workflow: workflow.clone(),
                revision: 1,
            },
        ))
    }

    async fn find_owned_score_improvement_workflow(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<ScoreImprovementWorkflowRecord>, StorageError> {
        let row = sqlx::query(
            "SELECT * FROM score_improvement_workflows WHERE owner_user_id = ? AND task_id = ?",
        )
        .bind(owner_user_id.to_string())
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_score_record).transpose()
    }

    async fn begin_score_improvement_attempt(
        &self,
        request: ScoreImprovementBeginRequest,
    ) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut record = fetch_score_record(
            &mut transaction,
            request.owner_user_id,
            request.workflow_id,
            request.expected_revision,
        )
        .await?;
        record
            .workflow
            .begin_retake(request.explicitly_confirmed, request.at)
            .map_err(|_| invalid_transition())?;
        record.revision = persist_score_record(
            &mut transaction,
            &record.workflow,
            request.expected_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(record)
    }

    async fn observe_score_improvement(
        &self,
        request: ScoreImprovementObserveRequest,
    ) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut record = fetch_score_record(
            &mut transaction,
            request.owner_user_id,
            request.workflow_id,
            request.expected_revision,
        )
        .await?;
        record
            .workflow
            .observe(
                request.score,
                request.retake_still_allowed,
                request.diagnosis,
                request.at,
            )
            .map_err(|_| invalid_transition())?;
        record.revision = persist_score_record(
            &mut transaction,
            &record.workflow,
            request.expected_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(record)
    }
}

#[async_trait]
impl CompletionWorkflowRepository for SqliteExecutionRepository {
    async fn create_strict_completion_workflow(
        &self,
        workflow: &StrictCompletionWorkflow,
    ) -> Result<CompletionWorkflowCreateOutcome<StrictCompletionWorkflowRecord>, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .create_strict_completion_workflow(workflow)
            .await
    }

    async fn find_owned_strict_completion_workflow(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<StrictCompletionWorkflowRecord>, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .find_owned_strict_completion_workflow(owner_user_id, task_id)
            .await
    }

    async fn begin_strict_completion_attempt(
        &self,
        request: StrictCompletionBeginRequest,
    ) -> Result<StrictCompletionWorkflowRecord, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .begin_strict_completion_attempt(request)
            .await
    }

    async fn observe_strict_completion(
        &self,
        request: StrictCompletionObserveRequest,
    ) -> Result<StrictCompletionWorkflowRecord, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .observe_strict_completion(request)
            .await
    }

    async fn record_strict_completion_execution_observation(
        &self,
        request: StrictCompletionExecutionObservationRequest<'_>,
    ) -> Result<StrictCompletionExecutionObservationRecord, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .record_strict_completion_execution_observation(request)
            .await
    }

    async fn create_score_improvement_workflow(
        &self,
        workflow: &ScoreImprovementWorkflow,
    ) -> Result<CompletionWorkflowCreateOutcome<ScoreImprovementWorkflowRecord>, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .create_score_improvement_workflow(workflow)
            .await
    }

    async fn find_owned_score_improvement_workflow(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<ScoreImprovementWorkflowRecord>, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .find_owned_score_improvement_workflow(owner_user_id, task_id)
            .await
    }

    async fn begin_score_improvement_attempt(
        &self,
        request: ScoreImprovementBeginRequest,
    ) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .begin_score_improvement_attempt(request)
            .await
    }

    async fn observe_score_improvement(
        &self,
        request: ScoreImprovementObserveRequest,
    ) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
        SqliteCompletionWorkflowRepository::new(self.database().clone())
            .observe_score_improvement(request)
            .await
    }
}

struct ExecutionObservationBinding {
    binding: CompletionWorkflowBinding,
    policy: CompletionPolicySnapshot,
    formal_assessment: bool,
    attempt_started_at: Timestamp,
    retry_confirmation: Option<(StrictCompletionWorkflowId, u32)>,
}

async fn resolve_execution_workflow(
    transaction: &mut Transaction<'_, Sqlite>,
    binding: &ExecutionObservationBinding,
    request: &StrictCompletionExecutionObservationRequest<'_>,
) -> Result<(StrictCompletionWorkflow, u32, bool), StorageError> {
    let existing = sqlx::query("SELECT * FROM strict_completion_workflows WHERE task_id = ?")
        .bind(binding.binding.task_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_strict_record)
        .transpose()?;
    let (workflow, revision, created) = match existing {
        Some(record) if record.workflow.binding == binding.binding => {
            (record.workflow, record.revision, false)
        }
        Some(_) => return Err(invalid_transition()),
        None => (
            StrictCompletionWorkflow::new(
                binding.binding,
                binding.policy.clone(),
                request.outcome,
                request
                    .outcome
                    .map_or(binding.policy.captured_at, |_| request.at),
            )
            .map_err(|_| invalid_transition())?,
            1,
            true,
        ),
    };
    if binding
        .retry_confirmation
        .is_some_and(|(workflow_id, expected_revision)| {
            created || workflow.id != workflow_id || revision != expected_revision
        })
    {
        return Err(invalid_transition());
    }
    Ok((workflow, revision, created))
}

fn validate_execution_observation(
    request: &StrictCompletionExecutionObservationRequest<'_>,
) -> Result<(), StorageError> {
    if request.outcome.is_some() == request.diagnosis.is_some() {
        Err(invalid_transition())
    } else {
        Ok(())
    }
}

async fn load_execution_observation_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    request: &StrictCompletionExecutionObservationRequest<'_>,
) -> Result<ExecutionObservationBinding, StorageError> {
    let row = sqlx::query(
        "SELECT execution.task_id, task.provider_account_id, account.owner_user_id, \
                task.assessment_class, settings.completion_policy_json, settings.captured_at, \
                attempt.started_at AS attempt_started_at, confirmation.workflow_id AS retry_workflow_id, \
                confirmation.workflow_revision AS retry_workflow_revision \
         FROM executions AS execution \
         INNER JOIN execution_attempts AS attempt ON attempt.execution_id = execution.id \
         INNER JOIN tasks AS task ON task.id = execution.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         INNER JOIN execution_runtime_settings AS settings \
                 ON settings.execution_id = execution.id \
         LEFT JOIN execution_strict_completion_retry_confirmations AS confirmation \
                ON confirmation.execution_id = execution.id \
         WHERE execution.id = ? AND attempt.id = ? AND attempt.finished_at IS NULL",
    )
    .bind(request.execution_id.to_string())
    .bind(request.execution_attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(invalid_transition)?;
    let captured_at = decode_timestamp(row.try_get("captured_at")?)?;
    let attempt_started_at = decode_timestamp(row.try_get("attempt_started_at")?)?;
    let policy_json = row.try_get::<String, _>("completion_policy_json")?;
    let policy: CompletionPolicySnapshot = serde_json::from_str(&policy_json)?;
    if policy.captured_at != captured_at
        || policy.validate().is_err()
        || attempt_started_at < captured_at
        || request.at < attempt_started_at
    {
        return Err(invalid_transition());
    }
    let retry_workflow_id = row.try_get::<Option<&str>, _>("retry_workflow_id")?;
    let retry_workflow_revision = row.try_get::<Option<i64>, _>("retry_workflow_revision")?;
    let retry_confirmation = match (retry_workflow_id, retry_workflow_revision) {
        (None, None) => None,
        (Some(workflow_id), Some(revision)) => Some((
            StrictCompletionWorkflowId::from_str(workflow_id).map_err(|_| invalid_transition())?,
            u32::try_from(revision).map_err(|_| invalid_transition())?,
        )),
        _ => return Err(invalid_transition()),
    };
    Ok(ExecutionObservationBinding {
        binding: CompletionWorkflowBinding {
            owner_user_id: UserId::from_str(row.try_get("owner_user_id")?)
                .map_err(|_| invalid_transition())?,
            provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
                .map_err(|_| invalid_transition())?,
            task_id: TaskId::from_str(row.try_get("task_id")?).map_err(|_| invalid_transition())?,
        },
        policy,
        formal_assessment: row.try_get::<&str, _>("assessment_class")? == "formal",
        attempt_started_at,
        retry_confirmation,
    })
}

async fn find_execution_observation(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
) -> Result<Option<StrictCompletionExecutionObservationRecord>, StorageError> {
    let Some(row) = sqlx::query(
        "SELECT execution_id, execution_attempt_id, workflow_id, workflow_attempt_no, completion_outcome, \
                diagnosis, observed_at \
         FROM strict_completion_execution_observations WHERE execution_attempt_id = ?",
    )
    .bind(execution_attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(None);
    };
    let actual_execution_id =
        ExecutionId::from_str(row.try_get("execution_id")?).map_err(|_| invalid_transition())?;
    let actual_attempt_id = ExecutionAttemptId::from_str(row.try_get("execution_attempt_id")?)
        .map_err(|_| invalid_transition())?;
    if actual_execution_id != execution_id || actual_attempt_id != execution_attempt_id {
        return Err(invalid_transition());
    }
    let workflow_id = StrictCompletionWorkflowId::from_str(row.try_get("workflow_id")?)
        .map_err(|_| invalid_transition())?;
    let workflow_row = sqlx::query("SELECT * FROM strict_completion_workflows WHERE id = ?")
        .bind(workflow_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(invalid_transition)?;
    Ok(Some(StrictCompletionExecutionObservationRecord {
        execution_id,
        execution_attempt_id,
        workflow: decode_strict_record(&workflow_row)?,
        workflow_attempt_no: row
            .try_get::<Option<i64>, _>("workflow_attempt_no")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid_transition())?,
        outcome: decode_optional_enum(row.try_get("completion_outcome")?)?,
        diagnosis: decode_optional_enum(row.try_get("diagnosis")?)?,
        observed_at: decode_timestamp(row.try_get("observed_at")?)?,
    }))
}

async fn insert_strict_record(
    transaction: &mut Transaction<'_, Sqlite>,
    workflow: &StrictCompletionWorkflow,
) -> Result<(), StorageError> {
    workflow.validate().map_err(|_| invalid_transition())?;
    sqlx::query(
        "INSERT INTO strict_completion_workflows \
         (id, owner_user_id, provider_account_id, task_id, state, workflow_json, revision, \
          created_at, updated_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(workflow.id.to_string())
    .bind(workflow.binding.owner_user_id.to_string())
    .bind(workflow.binding.provider_account_id.to_string())
    .bind(workflow.binding.task_id.to_string())
    .bind(encode_strict_state(workflow.state))
    .bind(serde_json::to_string(workflow)?)
    .bind(encode_timestamp(workflow.created_at))
    .bind(encode_timestamp(workflow.updated_at))
    .bind(workflow.finished_at.map(encode_timestamp))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_execution_observation(
    transaction: &mut Transaction<'_, Sqlite>,
    request: &StrictCompletionExecutionObservationRequest<'_>,
    workflow_id: StrictCompletionWorkflowId,
    workflow_attempt_no: Option<u32>,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO strict_completion_execution_observations \
         (execution_id, execution_attempt_id, workflow_id, workflow_attempt_no, \
          completion_outcome, diagnosis, observed_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(request.execution_id.to_string())
    .bind(request.execution_attempt_id.to_string())
    .bind(workflow_id.to_string())
    .bind(workflow_attempt_no.map(i64::from))
    .bind(request.outcome.map(encode_enum).transpose()?)
    .bind(request.diagnosis.map(encode_enum).transpose()?)
    .bind(encode_timestamp(request.at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn encode_enum(value: impl Serialize) -> Result<String, StorageError> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(invalid_transition()),
    }
}

fn decode_optional_enum<T>(value: Option<String>) -> Result<Option<T>, StorageError>
where
    T: DeserializeOwned,
{
    value
        .map(|value| serde_json::from_value(serde_json::Value::String(value)))
        .transpose()
        .map_err(Into::into)
}

async fn validate_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    binding: asterism_domain::CompletionWorkflowBinding,
) -> Result<(), StorageError> {
    let matches: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM tasks AS task \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE task.id = ? AND account.id = ? AND account.owner_user_id = ?",
    )
    .bind(binding.task_id.to_string())
    .bind(binding.provider_account_id.to_string())
    .bind(binding.owner_user_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    if matches.is_none() {
        return Err(invalid_workflow());
    }
    Ok(())
}

async fn fetch_strict_record(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    workflow_id: StrictCompletionWorkflowId,
    expected_revision: u32,
) -> Result<StrictCompletionWorkflowRecord, StorageError> {
    if expected_revision == 0 {
        return Err(invalid_transition());
    }
    let row =
        sqlx::query("SELECT * FROM strict_completion_workflows WHERE id = ? AND owner_user_id = ?")
            .bind(workflow_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(invalid_transition)?;
    let record = decode_strict_record(&row)?;
    if record.revision != expected_revision {
        return Err(invalid_transition());
    }
    Ok(record)
}

async fn fetch_score_record(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    workflow_id: ScoreImprovementWorkflowId,
    expected_revision: u32,
) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
    if expected_revision == 0 {
        return Err(invalid_transition());
    }
    let row =
        sqlx::query("SELECT * FROM score_improvement_workflows WHERE id = ? AND owner_user_id = ?")
            .bind(workflow_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(invalid_transition)?;
    let record = decode_score_record(&row)?;
    if record.revision != expected_revision {
        return Err(invalid_transition());
    }
    Ok(record)
}

async fn persist_strict_record(
    transaction: &mut Transaction<'_, Sqlite>,
    workflow: &StrictCompletionWorkflow,
    expected_revision: u32,
) -> Result<u32, StorageError> {
    workflow.validate().map_err(|_| invalid_transition())?;
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(invalid_transition)?;
    let changed = sqlx::query(
        "UPDATE strict_completion_workflows SET state = ?, workflow_json = ?, revision = ?, \
                updated_at = ?, finished_at = ? WHERE id = ? AND revision = ?",
    )
    .bind(encode_strict_state(workflow.state))
    .bind(serde_json::to_string(workflow)?)
    .bind(i64::from(revision))
    .bind(encode_timestamp(workflow.updated_at))
    .bind(workflow.finished_at.map(encode_timestamp))
    .bind(workflow.id.to_string())
    .bind(i64::from(expected_revision))
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(invalid_transition());
    }
    Ok(revision)
}

async fn persist_score_record(
    transaction: &mut Transaction<'_, Sqlite>,
    workflow: &ScoreImprovementWorkflow,
    expected_revision: u32,
) -> Result<u32, StorageError> {
    workflow.validate().map_err(|_| invalid_transition())?;
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(invalid_transition)?;
    let changed = sqlx::query(
        "UPDATE score_improvement_workflows SET state = ?, workflow_json = ?, revision = ?, \
                updated_at = ?, finished_at = ? WHERE id = ? AND revision = ?",
    )
    .bind(encode_score_state(workflow.state))
    .bind(serde_json::to_string(workflow)?)
    .bind(i64::from(revision))
    .bind(encode_timestamp(workflow.updated_at))
    .bind(workflow.finished_at.map(encode_timestamp))
    .bind(workflow.id.to_string())
    .bind(i64::from(expected_revision))
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(invalid_transition());
    }
    Ok(revision)
}

fn decode_strict_record(row: &SqliteRow) -> Result<StrictCompletionWorkflowRecord, StorageError> {
    let workflow: StrictCompletionWorkflow = serde_json::from_str(row.try_get("workflow_json")?)?;
    workflow.validate().map_err(|_| invalid_workflow())?;
    if workflow.id.to_string() != row.try_get::<String, _>("id")?
        || workflow.binding.owner_user_id.to_string()
            != row.try_get::<String, _>("owner_user_id")?
        || workflow.binding.provider_account_id.to_string()
            != row.try_get::<String, _>("provider_account_id")?
        || workflow.binding.task_id.to_string() != row.try_get::<String, _>("task_id")?
        || encode_strict_state(workflow.state) != row.try_get::<&str, _>("state")?
        || encode_timestamp(workflow.created_at) != row.try_get::<String, _>("created_at")?
        || encode_timestamp(workflow.updated_at) != row.try_get::<String, _>("updated_at")?
        || workflow.finished_at.map(encode_timestamp)
            != row.try_get::<Option<String>, _>("finished_at")?
    {
        return Err(invalid_workflow());
    }
    Ok(StrictCompletionWorkflowRecord {
        workflow,
        revision: decode_revision(row)?,
    })
}

fn decode_score_record(row: &SqliteRow) -> Result<ScoreImprovementWorkflowRecord, StorageError> {
    let workflow: ScoreImprovementWorkflow = serde_json::from_str(row.try_get("workflow_json")?)?;
    workflow.validate().map_err(|_| invalid_workflow())?;
    if workflow.id.to_string() != row.try_get::<String, _>("id")?
        || workflow.binding.owner_user_id.to_string()
            != row.try_get::<String, _>("owner_user_id")?
        || workflow.binding.provider_account_id.to_string()
            != row.try_get::<String, _>("provider_account_id")?
        || workflow.binding.task_id.to_string() != row.try_get::<String, _>("task_id")?
        || encode_score_state(workflow.state) != row.try_get::<&str, _>("state")?
        || encode_timestamp(workflow.created_at) != row.try_get::<String, _>("created_at")?
        || encode_timestamp(workflow.updated_at) != row.try_get::<String, _>("updated_at")?
        || workflow.finished_at.map(encode_timestamp)
            != row.try_get::<Option<String>, _>("finished_at")?
    {
        return Err(invalid_workflow());
    }
    Ok(ScoreImprovementWorkflowRecord {
        workflow,
        revision: decode_revision(row)?,
    })
}

fn decode_revision(row: &SqliteRow) -> Result<u32, StorageError> {
    u32::try_from(row.try_get::<i64, _>("revision")?).map_err(|_| invalid_workflow())
}

const fn encode_strict_state(state: StrictCompletionState) -> &'static str {
    match state {
        StrictCompletionState::Disabled => "disabled",
        StrictCompletionState::Active => "active",
        StrictCompletionState::AttemptRunning => "attempt_running",
        StrictCompletionState::Completed => "completed",
        StrictCompletionState::Stopped => "stopped",
    }
}

const fn encode_score_state(state: ScoreImprovementState) -> &'static str {
    match state {
        ScoreImprovementState::Disabled => "disabled",
        ScoreImprovementState::Ready => "ready",
        ScoreImprovementState::AttemptRunning => "attempt_running",
        ScoreImprovementState::Finished => "finished",
        ScoreImprovementState::Stopped => "stopped",
    }
}

fn invalid_workflow() -> StorageError {
    StorageError::InvalidData("completion workflow is invalid or cross-bound".to_owned())
}

fn invalid_transition() -> StorageError {
    StorageError::InvalidData(
        "completion workflow transition is invalid, stale or unauthorized".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        CompletionDiagnosis, CompletionOutcome, CompletionPolicySnapshot,
        CompletionWorkflowBinding, ProviderAccountId, RetakeScorePolicy, ScoreImprovementState,
        SubmissionScore, VerifiedCompletionBaseline,
    };
    use chrono::{Duration, SecondsFormat, Utc};

    use super::*;

    struct Fixture {
        database: Database,
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        now: asterism_domain::Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let task = TaskId::new();
            let now = Utc::now();
            let timestamp = text(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'completion-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
            )
            .bind(owner.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO provider_accounts \
                 (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
                 VALUES (?, ?, 'provider-alpha', 'completion', '\"authenticated\"', ?, ?)",
            )
            .bind(account.to_string())
            .bind(owner.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO tasks \
                 (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
                  assessment_class, title, remote_state, orchestration_state, discovered_at, \
                  updated_at, capabilities_json) \
                 VALUES (?, ?, 'completion-task', 'v1:completion', 'work', 'routine', \
                         'Completion', 'completed', 'succeeded', ?, ?, '[]')",
            )
            .bind(task.to_string())
            .bind(account.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            Self {
                database,
                owner,
                account,
                task,
                now,
            }
        }

        fn binding(&self) -> CompletionWorkflowBinding {
            CompletionWorkflowBinding {
                owner_user_id: self.owner,
                provider_account_id: self.account,
                task_id: self.task,
            }
        }

        fn policy(&self) -> CompletionPolicySnapshot {
            CompletionPolicySnapshot {
                strict_completion_enabled: true,
                score_improvement_enabled: true,
                strict_attempt_limit: 2,
                score_improvement_attempt_limit: 2,
                score_target_millis: 900,
                strict_expires_at: Some(self.now + Duration::hours(1)),
                score_improvement_expires_at: Some(self.now + Duration::hours(1)),
                formal_retry_requires_confirmation: true,
                captured_at: self.now,
            }
        }

        async fn claimed_execution(
            &self,
            offset_seconds: i64,
        ) -> (
            ExecutionId,
            ExecutionAttemptId,
            asterism_domain::ScheduleId,
            Timestamp,
        ) {
            let execution_id = ExecutionId::new();
            let attempt_id = ExecutionAttemptId::new();
            let job_id = asterism_domain::ScheduleId::new();
            let created_at = self.now + Duration::seconds(offset_seconds);
            let started_at = created_at + Duration::seconds(1);
            let observed_at = started_at + Duration::seconds(1);
            let lease_expires_at = observed_at + Duration::minutes(5);
            let mut policy = self.policy();
            policy.captured_at = created_at;
            policy.strict_expires_at = Some(created_at + Duration::hours(1));
            policy.score_improvement_expires_at = Some(created_at + Duration::hours(1));
            sqlx::query(
                "INSERT INTO executions \
                 (id, task_id, requested_capabilities_json, requested_by, request_source, state, \
                  scheduled_at, started_at, created_at) \
                 VALUES (?, ?, '[\"resource_execution\"]', ?, 'system', 'running', ?, ?, ?)",
            )
            .bind(execution_id.to_string())
            .bind(self.task.to_string())
            .bind(self.owner.to_string())
            .bind(text(created_at))
            .bind(text(started_at))
            .bind(text(created_at))
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO execution_runtime_settings \
                 (execution_id, provider_id, schema_version, resolved_settings_json, sources_json, \
                  completion_policy_json, captured_at) VALUES (?, 'provider-alpha', 1, '{}', '{}', ?, ?)",
            )
            .bind(execution_id.to_string())
            .bind(serde_json::to_string(&policy).unwrap())
            .bind(text(created_at))
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO execution_attempts (id, execution_id, attempt_no, started_at) \
                 VALUES (?, ?, 1, ?)",
            )
            .bind(attempt_id.to_string())
            .bind(execution_id.to_string())
            .bind(text(started_at))
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query("DELETE FROM execution_leases WHERE task_id = ?")
                .bind(self.task.to_string())
                .execute(self.database.pool())
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO execution_leases (task_id, execution_id, worker_id, expires_at) \
                 VALUES (?, ?, 'completion-worker', ?)",
            )
            .bind(self.task.to_string())
            .bind(execution_id.to_string())
            .bind(text(lease_expires_at))
            .execute(self.database.pool())
            .await
            .unwrap();
            let job_kind = asterism_scheduler::ScheduledJobKind::Execution { execution_id };
            sqlx::query(
                "INSERT INTO scheduled_jobs \
                 (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, worker_id, \
                  lease_expires_at, created_at, updated_at) \
                 VALUES (?, 'execution', ?, ?, 'claimed', 1, ?, 'completion-worker', ?, ?, ?)",
            )
            .bind(job_id.to_string())
            .bind(serde_json::to_string(&job_kind).unwrap())
            .bind(text(created_at))
            .bind(format!("completion-observation:{execution_id}"))
            .bind(text(lease_expires_at))
            .bind(text(created_at))
            .bind(text(created_at))
            .execute(self.database.pool())
            .await
            .unwrap();
            (execution_id, attempt_id, job_id, observed_at)
        }
    }

    #[tokio::test]
    async fn execution_observations_are_atomic_idempotent_and_completion_is_monotonic() {
        let fixture = Fixture::new().await;
        let repository = SqliteCompletionWorkflowRepository::new(fixture.database.clone());
        let (execution_id, attempt_id, job_id, observed_at) = fixture.claimed_execution(0).await;
        let request = StrictCompletionExecutionObservationRequest {
            execution_id,
            execution_attempt_id: attempt_id,
            scheduler_job_id: job_id,
            worker_id: "completion-worker",
            outcome: None,
            diagnosis: Some(CompletionDiagnosis::DurationInsufficient),
            at: observed_at,
            correlation_id: "completion-observation-1",
        };
        let first = repository
            .record_strict_completion_execution_observation(request.clone())
            .await
            .unwrap();
        assert_eq!(first.workflow_attempt_no, Some(1));
        assert_eq!(first.workflow.revision, 1);
        assert_eq!(first.workflow.workflow.state, StrictCompletionState::Active);
        assert_eq!(first.workflow.workflow.attempts_started, 1);
        assert_eq!(
            repository
                .record_strict_completion_execution_observation(request)
                .await
                .unwrap(),
            first
        );

        let (execution_id, attempt_id, job_id, observed_at) = fixture.claimed_execution(10).await;
        let second = repository
            .record_strict_completion_execution_observation(
                StrictCompletionExecutionObservationRequest {
                    execution_id,
                    execution_attempt_id: attempt_id,
                    scheduler_job_id: job_id,
                    worker_id: "completion-worker",
                    outcome: None,
                    diagnosis: Some(CompletionDiagnosis::DurationInsufficient),
                    at: observed_at,
                    correlation_id: "completion-observation-2",
                },
            )
            .await
            .unwrap();
        assert_eq!(second.workflow_attempt_no, Some(2));
        assert_eq!(second.workflow.revision, 2);
        assert_eq!(
            second.workflow.workflow.state,
            StrictCompletionState::Stopped
        );
        assert_eq!(
            second.workflow.workflow.last_diagnosis,
            Some(CompletionDiagnosis::AttemptLimitReached)
        );

        let (execution_id, attempt_id, job_id, observed_at) = fixture.claimed_execution(20).await;
        let completed = repository
            .record_strict_completion_execution_observation(
                StrictCompletionExecutionObservationRequest {
                    execution_id,
                    execution_attempt_id: attempt_id,
                    scheduler_job_id: job_id,
                    worker_id: "completion-worker",
                    outcome: Some(CompletionOutcome::Completed),
                    diagnosis: None,
                    at: observed_at,
                    correlation_id: "completion-observation-3",
                },
            )
            .await
            .unwrap();
        assert_eq!(completed.workflow_attempt_no, None);
        assert_eq!(completed.workflow.revision, 3);
        assert_eq!(
            completed.workflow.workflow.state,
            StrictCompletionState::Completed
        );
        assert_eq!(
            completed.workflow.workflow.verified_outcome,
            Some(CompletionOutcome::Completed)
        );
        assert_eq!(completed.workflow.workflow.last_diagnosis, None);
        let observation_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM strict_completion_execution_observations")
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        assert_eq!(observation_count, 3);
    }

    #[tokio::test]
    async fn strict_workflow_is_owner_scoped_revisioned_and_terminal_at_completion() {
        let fixture = Fixture::new().await;
        let repository = SqliteCompletionWorkflowRepository::new(fixture.database.clone());
        let workflow =
            StrictCompletionWorkflow::new(fixture.binding(), fixture.policy(), None, fixture.now)
                .unwrap();
        let created = repository
            .create_strict_completion_workflow(&workflow)
            .await
            .unwrap();
        assert_eq!(
            created,
            CompletionWorkflowCreateOutcome::Created(StrictCompletionWorkflowRecord {
                workflow: workflow.clone(),
                revision: 1,
            })
        );
        assert!(matches!(
            repository
                .create_strict_completion_workflow(&workflow)
                .await
                .unwrap(),
            CompletionWorkflowCreateOutcome::Existing(_)
        ));
        assert!(
            repository
                .find_owned_strict_completion_workflow(UserId::new(), fixture.task)
                .await
                .unwrap()
                .is_none()
        );
        let running = repository
            .begin_strict_completion_attempt(StrictCompletionBeginRequest {
                owner_user_id: fixture.owner,
                workflow_id: workflow.id,
                expected_revision: 1,
                formal_assessment: false,
                retry_confirmed: false,
                at: fixture.now + Duration::seconds(1),
            })
            .await
            .unwrap();
        assert_eq!(running.revision, 2);
        assert_eq!(
            running.workflow.state,
            StrictCompletionState::AttemptRunning
        );
        assert!(
            repository
                .begin_strict_completion_attempt(StrictCompletionBeginRequest {
                    owner_user_id: fixture.owner,
                    workflow_id: workflow.id,
                    expected_revision: 1,
                    formal_assessment: false,
                    retry_confirmed: false,
                    at: fixture.now + Duration::seconds(2),
                })
                .await
                .is_err()
        );
        let completed = repository
            .observe_strict_completion(StrictCompletionObserveRequest {
                owner_user_id: fixture.owner,
                workflow_id: workflow.id,
                expected_revision: 2,
                outcome: Some(CompletionOutcome::Completed),
                diagnosis: None,
                at: fixture.now + Duration::seconds(2),
            })
            .await
            .unwrap();
        assert_eq!(completed.revision, 3);
        assert_eq!(completed.workflow.state, StrictCompletionState::Completed);
        assert!(
            repository
                .begin_strict_completion_attempt(StrictCompletionBeginRequest {
                    owner_user_id: fixture.owner,
                    workflow_id: workflow.id,
                    expected_revision: 3,
                    formal_assessment: false,
                    retry_confirmed: false,
                    at: fixture.now + Duration::seconds(3),
                })
                .await
                .is_err()
        );
        assert_eq!(
            repository
                .find_owned_strict_completion_workflow(fixture.owner, fixture.task)
                .await
                .unwrap()
                .unwrap(),
            completed
        );
    }

    #[tokio::test]
    async fn score_workflow_requires_confirmation_and_preserves_completed_baseline() {
        let fixture = Fixture::new().await;
        let repository = SqliteCompletionWorkflowRepository::new(fixture.database.clone());
        let baseline = VerifiedCompletionBaseline {
            outcome: CompletionOutcome::Passed,
            score: Some(SubmissionScore {
                earned_milli_points: 80,
                possible_milli_points: 100,
            }),
            verified_at: fixture.now,
        };
        let workflow = ScoreImprovementWorkflow::new_with_authority(
            fixture.binding(),
            fixture.policy(),
            baseline,
            RetakeScorePolicy::LastAttempt,
            Some(asterism_domain::ScoreImprovementRetakeAuthority {
                answer_history_import_id: asterism_domain::AnswerHistoryImportId::new(),
                result_digest: [9; 32],
                allowed: true,
                remaining_attempts: Some(1),
                closes_at: Some(fixture.now + Duration::hours(1)),
                observed_at: fixture.now,
            }),
            true,
            fixture.now,
        )
        .unwrap();
        assert!(matches!(
            repository
                .create_score_improvement_workflow(&workflow)
                .await
                .unwrap(),
            CompletionWorkflowCreateOutcome::Created(_)
        ));
        assert!(
            repository
                .begin_score_improvement_attempt(ScoreImprovementBeginRequest {
                    owner_user_id: fixture.owner,
                    workflow_id: workflow.id,
                    expected_revision: 1,
                    explicitly_confirmed: false,
                    at: fixture.now + Duration::seconds(1),
                })
                .await
                .is_err()
        );
        let unchanged = repository
            .find_owned_score_improvement_workflow(fixture.owner, fixture.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.revision, 1);
        assert_eq!(unchanged.workflow.state, ScoreImprovementState::Ready);
        let running = repository
            .begin_score_improvement_attempt(ScoreImprovementBeginRequest {
                owner_user_id: fixture.owner,
                workflow_id: workflow.id,
                expected_revision: 1,
                explicitly_confirmed: true,
                at: fixture.now + Duration::seconds(1),
            })
            .await
            .unwrap();
        let stopped = repository
            .observe_score_improvement(ScoreImprovementObserveRequest {
                owner_user_id: fixture.owner,
                workflow_id: workflow.id,
                expected_revision: running.revision,
                score: Some(SubmissionScore {
                    earned_milli_points: 60,
                    possible_milli_points: 100,
                }),
                retake_still_allowed: false,
                diagnosis: Some(CompletionDiagnosis::ScoreBelowThreshold),
                at: fixture.now + Duration::seconds(2),
            })
            .await
            .unwrap();
        assert_eq!(stopped.revision, 3);
        assert_eq!(stopped.workflow.state, ScoreImprovementState::Stopped);
        assert_eq!(stopped.workflow.completion_baseline, baseline);
        assert_eq!(stopped.workflow.best_observed_score, baseline.score);
    }

    fn text(value: asterism_domain::Timestamp) -> String {
        value.to_rfc3339_opts(SecondsFormat::Nanos, true)
    }
}
