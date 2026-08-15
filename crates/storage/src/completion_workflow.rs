use asterism_domain::{
    ScoreImprovementState, ScoreImprovementWorkflow, ScoreImprovementWorkflowId,
    StrictCompletionState, StrictCompletionWorkflow, StrictCompletionWorkflowId, TaskId, UserId,
};
use async_trait::async_trait;
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::auth_session::encode_timestamp;
use crate::{
    CompletionWorkflowCreateOutcome, CompletionWorkflowRepository, Database,
    ScoreImprovementBeginRequest, ScoreImprovementObserveRequest, ScoreImprovementWorkflowRecord,
    StorageError, StrictCompletionBeginRequest, StrictCompletionObserveRequest,
    StrictCompletionWorkflowRecord,
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
        let workflow = ScoreImprovementWorkflow::new(
            fixture.binding(),
            fixture.policy(),
            baseline,
            RetakeScorePolicy::LastAttempt,
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
