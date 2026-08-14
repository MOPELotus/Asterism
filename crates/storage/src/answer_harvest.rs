use std::{fmt::Display, str::FromStr};

use asterism_domain::{
    AnswerBootstrapHarvest, AnswerBootstrapHarvestId, AnswerBootstrapHarvestState,
    ProviderAccountId, ProviderId, ScheduleId, Timestamp, UserId,
};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::auth_session::{decode_timestamp, encode_timestamp};
use crate::{
    AnswerBootstrapHarvestCheckpoint, AnswerBootstrapHarvestCompletion,
    AnswerBootstrapHarvestFailure, AnswerBootstrapHarvestRepository, AnswerBootstrapHarvestYield,
    ClaimedAnswerBootstrapHarvest, Database, StorageError,
};

const INITIAL_GENERATION: u32 = 1;
const MAX_HARVEST_CLAIM_BATCH: u32 = 100;
const MAX_HARVEST_ATTEMPTS: i64 = 5;
const MAX_ERROR_BYTES: usize = 2_048;

#[derive(Clone, Debug)]
pub struct SqliteAnswerBootstrapHarvestRepository {
    database: Database,
}

impl SqliteAnswerBootstrapHarvestRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl AnswerBootstrapHarvestRepository for SqliteAnswerBootstrapHarvestRepository {
    async fn find_owned_answer_bootstrap_harvest(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        generation: u32,
    ) -> Result<Option<AnswerBootstrapHarvest>, StorageError> {
        if generation == 0 {
            return Err(invalid_harvest());
        }
        let row = sqlx::query(
            "SELECT harvest.id, harvest.owner_user_id, harvest.provider_id, \
                    harvest.provider_account_id, harvest.generation, harvest.schedule_id, \
                    harvest.state, harvest.scanned_task_count, harvest.total_task_count, \
                    harvest.watermark_sanitized_json, harvest.created_at, harvest.started_at, \
                    harvest.updated_at, harvest.completed_at, job.job_kind, job.payload_json \
             FROM answer_bootstrap_harvests AS harvest \
             INNER JOIN scheduled_jobs AS job ON job.id = harvest.schedule_id \
             WHERE harvest.owner_user_id = ? AND harvest.provider_account_id = ? \
               AND harvest.generation = ?",
        )
        .bind(owner_user_id.to_string())
        .bind(provider_account_id.to_string())
        .bind(i64::from(generation))
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_harvest).transpose()
    }

    async fn claim_due_answer_bootstrap_harvests(
        &self,
        worker_id: &str,
        now: Timestamp,
        lease_expires_at: Timestamp,
        limit: u32,
    ) -> Result<Vec<ClaimedAnswerBootstrapHarvest>, StorageError> {
        validate_claim(worker_id, now, lease_expires_at, limit)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        retire_exhausted_expired_claims(&mut transaction, now).await?;
        let rows = sqlx::query(
            "SELECT harvest.id, harvest.schedule_id, job.state AS job_state \
             FROM answer_bootstrap_harvests AS harvest \
             INNER JOIN scheduled_jobs AS job ON job.id = harvest.schedule_id \
             WHERE job.job_kind = 'answer_bootstrap_harvest' AND job.run_at <= ? \
               AND job.attempts < ? \
               AND harvest.state IN ('pending', 'paused', 'running') \
               AND (job.state = 'pending' OR \
                    (job.state = 'claimed' AND job.lease_expires_at <= ?)) \
             ORDER BY job.run_at, job.id LIMIT ?",
        )
        .bind(encode_timestamp(now))
        .bind(MAX_HARVEST_ATTEMPTS)
        .bind(encode_timestamp(now))
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let mut claimed = Vec::with_capacity(rows.len());
        for row in rows {
            let harvest_id: AnswerBootstrapHarvestId = parse_id(row.try_get("id")?)?;
            let schedule_id: ScheduleId = parse_id(row.try_get("schedule_id")?)?;
            let reclaimed = row.try_get::<&str, _>("job_state")? == "claimed";
            let changed = sqlx::query(
                "UPDATE scheduled_jobs SET state = 'claimed', worker_id = ?, \
                        lease_expires_at = ?, attempts = attempts + ?, \
                        last_error_sanitized = CASE WHEN ? = 1 \
                            THEN 'answer bootstrap harvest lease expired' \
                            ELSE last_error_sanitized END, updated_at = ? \
                 WHERE id = ? AND job_kind = 'answer_bootstrap_harvest' \
                   AND attempts < ? AND (state = 'pending' OR \
                        (state = 'claimed' AND lease_expires_at <= ?))",
            )
            .bind(worker_id)
            .bind(encode_timestamp(lease_expires_at))
            .bind(i64::from(reclaimed))
            .bind(i64::from(reclaimed))
            .bind(encode_timestamp(now))
            .bind(schedule_id.to_string())
            .bind(MAX_HARVEST_ATTEMPTS)
            .bind(encode_timestamp(now))
            .execute(&mut *transaction)
            .await?;
            if changed.rows_affected() != 1 {
                return Err(StorageError::SchedulerClaimLost);
            }
            let changed = sqlx::query(
                "UPDATE answer_bootstrap_harvests \
                 SET state = 'running', started_at = COALESCE(started_at, ?), updated_at = ? \
                 WHERE id = ? AND schedule_id = ? \
                   AND state IN ('pending', 'paused', 'running')",
            )
            .bind(encode_timestamp(now))
            .bind(encode_timestamp(now))
            .bind(harvest_id.to_string())
            .bind(schedule_id.to_string())
            .execute(&mut *transaction)
            .await?;
            if changed.rows_affected() != 1 {
                return Err(invalid_harvest());
            }
            let harvest = fetch_harvest(&mut transaction, harvest_id, schedule_id).await?;
            claimed.push(ClaimedAnswerBootstrapHarvest {
                harvest,
                worker_id: worker_id.to_owned(),
                lease_expires_at,
            });
        }
        transaction.commit().await?;
        Ok(claimed)
    }

    async fn checkpoint_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestCheckpoint<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut harvest = fetch_live_claim(
            &mut transaction,
            request.harvest_id,
            request.schedule_id,
            request.worker_id,
            request.at,
        )
        .await?;
        validate_progress(
            &harvest,
            request.scanned_task_count,
            request.total_task_count,
        )?;
        harvest.scanned_task_count = request.scanned_task_count;
        harvest.total_task_count = request.total_task_count;
        harvest.watermark_sanitized = request.watermark_sanitized.clone();
        harvest.updated_at = request.at;
        harvest.validate().map_err(|_| invalid_harvest())?;
        persist_running_harvest(&mut transaction, &harvest).await?;
        transaction.commit().await?;
        Ok(harvest)
    }

    async fn complete_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestCompletion<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut harvest = fetch_live_claim(
            &mut transaction,
            request.harvest_id,
            request.schedule_id,
            request.worker_id,
            request.at,
        )
        .await?;
        validate_progress(
            &harvest,
            request.scanned_task_count,
            Some(request.total_task_count),
        )?;
        if request.scanned_task_count != request.total_task_count {
            return Err(invalid_harvest());
        }
        harvest.scanned_task_count = request.scanned_task_count;
        harvest.total_task_count = Some(request.total_task_count);
        harvest.watermark_sanitized = request.watermark_sanitized.clone();
        harvest.state = AnswerBootstrapHarvestState::Completed;
        harvest.updated_at = request.at;
        harvest.completed_at = Some(request.at);
        harvest.validate().map_err(|_| invalid_harvest())?;
        persist_terminal_harvest(
            &mut transaction,
            &harvest,
            request.worker_id,
            "completed",
            None,
        )
        .await?;
        transaction.commit().await?;
        Ok(harvest)
    }

    async fn yield_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestYield<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError> {
        if request.run_at <= request.at {
            return Err(invalid_harvest());
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut harvest = fetch_live_claim(
            &mut transaction,
            request.harvest_id,
            request.schedule_id,
            request.worker_id,
            request.at,
        )
        .await?;
        validate_progress(
            &harvest,
            request.scanned_task_count,
            request.total_task_count,
        )?;
        harvest.scanned_task_count = request.scanned_task_count;
        harvest.total_task_count = request.total_task_count;
        harvest.watermark_sanitized = request.watermark_sanitized.clone();
        harvest.state = AnswerBootstrapHarvestState::Paused;
        harvest.updated_at = request.at;
        harvest.validate().map_err(|_| invalid_harvest())?;
        persist_yielded_harvest(
            &mut transaction,
            &harvest,
            request.worker_id,
            request.run_at,
        )
        .await?;
        transaction.commit().await?;
        Ok(harvest)
    }

    async fn fail_answer_bootstrap_harvest(
        &self,
        request: AnswerBootstrapHarvestFailure<'_>,
    ) -> Result<AnswerBootstrapHarvest, StorageError> {
        validate_failure(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut harvest = fetch_live_claim(
            &mut transaction,
            request.harvest_id,
            request.schedule_id,
            request.worker_id,
            request.at,
        )
        .await?;
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM scheduled_jobs WHERE id = ?")
            .bind(request.schedule_id.to_string())
            .fetch_one(&mut *transaction)
            .await?;
        let retry_at = request
            .retry_at
            .filter(|_| attempts.saturating_add(1) < MAX_HARVEST_ATTEMPTS);
        if let Some(retry_at) = retry_at {
            harvest.state = AnswerBootstrapHarvestState::Paused;
            harvest.updated_at = request.at;
            harvest.validate().map_err(|_| invalid_harvest())?;
            persist_paused_harvest(
                &mut transaction,
                &harvest,
                request.worker_id,
                retry_at,
                request.error_sanitized,
            )
            .await?;
        } else {
            harvest.state = AnswerBootstrapHarvestState::Failed;
            harvest.updated_at = request.at;
            harvest.completed_at = Some(request.at);
            harvest.validate().map_err(|_| invalid_harvest())?;
            persist_terminal_harvest(
                &mut transaction,
                &harvest,
                request.worker_id,
                "dead_letter",
                Some(request.error_sanitized),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(harvest)
    }
}

fn validate_claim(
    worker_id: &str,
    now: Timestamp,
    lease_expires_at: Timestamp,
    limit: u32,
) -> Result<(), StorageError> {
    if worker_id.is_empty()
        || worker_id.len() > 128
        || worker_id.trim() != worker_id
        || worker_id.chars().any(char::is_control)
        || lease_expires_at <= now
        || limit == 0
        || limit > MAX_HARVEST_CLAIM_BATCH
    {
        return Err(StorageError::InvalidSchedulerClaim);
    }
    Ok(())
}

fn validate_progress(
    current: &AnswerBootstrapHarvest,
    scanned_task_count: u32,
    total_task_count: Option<u32>,
) -> Result<(), StorageError> {
    if scanned_task_count < current.scanned_task_count
        || total_task_count.is_some_and(|total| scanned_task_count > total)
        || current
            .total_task_count
            .is_some_and(|current_total| total_task_count != Some(current_total))
    {
        return Err(invalid_harvest());
    }
    Ok(())
}

fn validate_failure(request: &AnswerBootstrapHarvestFailure<'_>) -> Result<(), StorageError> {
    if request.error_sanitized.is_empty()
        || request.error_sanitized.len() > MAX_ERROR_BYTES
        || request.error_sanitized.trim() != request.error_sanitized
        || request.error_sanitized.chars().any(char::is_control)
        || request
            .retry_at
            .is_some_and(|retry_at| retry_at <= request.at)
    {
        return Err(invalid_harvest());
    }
    Ok(())
}

async fn retire_exhausted_expired_claims(
    transaction: &mut Transaction<'_, Sqlite>,
    now: Timestamp,
) -> Result<(), StorageError> {
    let schedule_ids = sqlx::query_scalar::<_, String>(
        "SELECT job.id FROM scheduled_jobs AS job \
         INNER JOIN answer_bootstrap_harvests AS harvest ON harvest.schedule_id = job.id \
         WHERE job.job_kind = 'answer_bootstrap_harvest' AND job.state = 'claimed' \
           AND job.lease_expires_at <= ? AND job.attempts + 1 >= ? \
           AND harvest.state = 'running'",
    )
    .bind(encode_timestamp(now))
    .bind(MAX_HARVEST_ATTEMPTS)
    .fetch_all(&mut **transaction)
    .await?;
    for schedule_id in schedule_ids {
        sqlx::query(
            "UPDATE answer_bootstrap_harvests \
             SET state = 'failed', updated_at = ?, completed_at = ? \
             WHERE schedule_id = ? AND state = 'running'",
        )
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .bind(&schedule_id)
        .execute(&mut **transaction)
        .await?;
        sqlx::query(
            "UPDATE scheduled_jobs SET state = 'dead_letter', attempts = attempts + 1, \
                    worker_id = NULL, lease_expires_at = NULL, \
                    last_error_sanitized = 'answer bootstrap harvest lease expired at retry budget', \
                    updated_at = ? \
             WHERE id = ? AND state = 'claimed'",
        )
        .bind(encode_timestamp(now))
        .bind(schedule_id)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn fetch_live_claim(
    transaction: &mut Transaction<'_, Sqlite>,
    harvest_id: AnswerBootstrapHarvestId,
    schedule_id: ScheduleId,
    worker_id: &str,
    at: Timestamp,
) -> Result<AnswerBootstrapHarvest, StorageError> {
    let row = sqlx::query(
        "SELECT state, worker_id, lease_expires_at FROM scheduled_jobs \
         WHERE id = ? AND job_kind = 'answer_bootstrap_harvest'",
    )
    .bind(schedule_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StorageError::SchedulerClaimLost)?;
    let lease_expires_at = row
        .try_get::<Option<&str>, _>("lease_expires_at")?
        .map(decode_timestamp)
        .transpose()?;
    if row.try_get::<&str, _>("state")? != "claimed"
        || row.try_get::<Option<&str>, _>("worker_id")? != Some(worker_id)
        || lease_expires_at.is_none_or(|lease_expires_at| lease_expires_at <= at)
    {
        return Err(StorageError::SchedulerClaimLost);
    }
    let harvest = fetch_harvest(transaction, harvest_id, schedule_id).await?;
    if harvest.state != AnswerBootstrapHarvestState::Running {
        return Err(invalid_harvest());
    }
    Ok(harvest)
}

async fn fetch_harvest(
    transaction: &mut Transaction<'_, Sqlite>,
    harvest_id: AnswerBootstrapHarvestId,
    schedule_id: ScheduleId,
) -> Result<AnswerBootstrapHarvest, StorageError> {
    let row = sqlx::query(
        "SELECT harvest.id, harvest.owner_user_id, harvest.provider_id, \
                harvest.provider_account_id, harvest.generation, harvest.schedule_id, \
                harvest.state, harvest.scanned_task_count, harvest.total_task_count, \
                harvest.watermark_sanitized_json, harvest.created_at, harvest.started_at, \
                harvest.updated_at, harvest.completed_at, job.job_kind, job.payload_json \
         FROM answer_bootstrap_harvests AS harvest \
         INNER JOIN scheduled_jobs AS job ON job.id = harvest.schedule_id \
         WHERE harvest.id = ? AND harvest.schedule_id = ?",
    )
    .bind(harvest_id.to_string())
    .bind(schedule_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(invalid_harvest)?;
    decode_harvest(&row)
}

async fn persist_running_harvest(
    transaction: &mut Transaction<'_, Sqlite>,
    harvest: &AnswerBootstrapHarvest,
) -> Result<(), StorageError> {
    let changed = sqlx::query(
        "UPDATE answer_bootstrap_harvests SET scanned_task_count = ?, \
                total_task_count = ?, watermark_sanitized_json = ?, updated_at = ? \
         WHERE id = ? AND schedule_id = ? AND state = 'running'",
    )
    .bind(i64::from(harvest.scanned_task_count))
    .bind(harvest.total_task_count.map(i64::from))
    .bind(serde_json::to_string(&harvest.watermark_sanitized)?)
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.id.to_string())
    .bind(harvest.schedule_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(invalid_harvest());
    }
    Ok(())
}

async fn persist_paused_harvest(
    transaction: &mut Transaction<'_, Sqlite>,
    harvest: &AnswerBootstrapHarvest,
    worker_id: &str,
    retry_at: Timestamp,
    error_sanitized: &str,
) -> Result<(), StorageError> {
    let changed = sqlx::query(
        "UPDATE answer_bootstrap_harvests SET state = 'paused', updated_at = ? \
         WHERE id = ? AND schedule_id = ? AND state = 'running'",
    )
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.id.to_string())
    .bind(harvest.schedule_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(invalid_harvest());
    }
    let changed = sqlx::query(
        "UPDATE scheduled_jobs SET state = 'pending', run_at = ?, attempts = attempts + 1, \
                worker_id = NULL, lease_expires_at = NULL, last_error_sanitized = ?, \
                updated_at = ? \
         WHERE id = ? AND state = 'claimed' AND worker_id = ?",
    )
    .bind(encode_timestamp(retry_at))
    .bind(error_sanitized)
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.schedule_id.to_string())
    .bind(worker_id)
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(StorageError::SchedulerClaimLost);
    }
    Ok(())
}

async fn persist_yielded_harvest(
    transaction: &mut Transaction<'_, Sqlite>,
    harvest: &AnswerBootstrapHarvest,
    worker_id: &str,
    run_at: Timestamp,
) -> Result<(), StorageError> {
    let changed = sqlx::query(
        "UPDATE answer_bootstrap_harvests SET state = 'paused', scanned_task_count = ?, \
                total_task_count = ?, watermark_sanitized_json = ?, updated_at = ? \
         WHERE id = ? AND schedule_id = ? AND state = 'running'",
    )
    .bind(i64::from(harvest.scanned_task_count))
    .bind(harvest.total_task_count.map(i64::from))
    .bind(serde_json::to_string(&harvest.watermark_sanitized)?)
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.id.to_string())
    .bind(harvest.schedule_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(invalid_harvest());
    }
    let changed = sqlx::query(
        "UPDATE scheduled_jobs SET state = 'pending', run_at = ?, worker_id = NULL, \
                lease_expires_at = NULL, last_error_sanitized = NULL, updated_at = ? \
         WHERE id = ? AND state = 'claimed' AND worker_id = ?",
    )
    .bind(encode_timestamp(run_at))
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.schedule_id.to_string())
    .bind(worker_id)
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(StorageError::SchedulerClaimLost);
    }
    Ok(())
}

async fn persist_terminal_harvest(
    transaction: &mut Transaction<'_, Sqlite>,
    harvest: &AnswerBootstrapHarvest,
    worker_id: &str,
    job_state: &str,
    error_sanitized: Option<&str>,
) -> Result<(), StorageError> {
    let changed = sqlx::query(
        "UPDATE answer_bootstrap_harvests SET state = ?, scanned_task_count = ?, \
                total_task_count = ?, watermark_sanitized_json = ?, updated_at = ?, \
                completed_at = ? \
         WHERE id = ? AND schedule_id = ? AND state = 'running'",
    )
    .bind(encode_state(harvest.state))
    .bind(i64::from(harvest.scanned_task_count))
    .bind(harvest.total_task_count.map(i64::from))
    .bind(serde_json::to_string(&harvest.watermark_sanitized)?)
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.completed_at.map(encode_timestamp))
    .bind(harvest.id.to_string())
    .bind(harvest.schedule_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(invalid_harvest());
    }
    let failed = i64::from(job_state == "dead_letter");
    let changed = sqlx::query(
        "UPDATE scheduled_jobs SET state = ?, attempts = attempts + ?, worker_id = NULL, \
                lease_expires_at = NULL, last_error_sanitized = ?, updated_at = ? \
         WHERE id = ? AND state = 'claimed' AND worker_id = ?",
    )
    .bind(job_state)
    .bind(failed)
    .bind(error_sanitized)
    .bind(encode_timestamp(harvest.updated_at))
    .bind(harvest.schedule_id.to_string())
    .bind(worker_id)
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(StorageError::SchedulerClaimLost);
    }
    Ok(())
}

const fn encode_state(state: AnswerBootstrapHarvestState) -> &'static str {
    match state {
        AnswerBootstrapHarvestState::Pending => "pending",
        AnswerBootstrapHarvestState::Running => "running",
        AnswerBootstrapHarvestState::Paused => "paused",
        AnswerBootstrapHarvestState::Completed => "completed",
        AnswerBootstrapHarvestState::Failed => "failed",
        AnswerBootstrapHarvestState::Cancelled => "cancelled",
    }
}

pub(crate) async fn ensure_initial_answer_bootstrap_harvest(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_id: &ProviderId,
    provider_account_id: ProviderAccountId,
    created_at: Timestamp,
) -> Result<AnswerBootstrapHarvestId, StorageError> {
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT id FROM answer_bootstrap_harvests \
         WHERE provider_account_id = ? AND generation = ?",
    )
    .bind(provider_account_id.to_string())
    .bind(i64::from(INITIAL_GENERATION))
    .fetch_optional(&mut **transaction)
    .await?;
    if let Some(existing) = existing {
        return parse_id(&existing);
    }

    let harvest_id = AnswerBootstrapHarvestId::new();
    let schedule_id = ScheduleId::new();
    let harvest = AnswerBootstrapHarvest {
        id: harvest_id,
        owner_user_id,
        provider_id: provider_id.clone(),
        provider_account_id,
        generation: INITIAL_GENERATION,
        schedule_id,
        state: AnswerBootstrapHarvestState::Pending,
        scanned_task_count: 0,
        total_task_count: None,
        watermark_sanitized: serde_json::json!({}),
        created_at,
        started_at: None,
        updated_at: created_at,
        completed_at: None,
    };
    harvest.validate().map_err(|_| invalid_harvest())?;
    let job_kind = ScheduledJobKind::AnswerBootstrapHarvest {
        harvest_id,
        provider_account_id,
        generation: INITIAL_GENERATION,
    };
    sqlx::query(
        "INSERT INTO scheduled_jobs \
         (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, \
          created_at, updated_at) \
         VALUES (?, 'answer_bootstrap_harvest', ?, ?, 'pending', 0, ?, ?, ?)",
    )
    .bind(schedule_id.to_string())
    .bind(serde_json::to_string(&job_kind)?)
    .bind(encode_timestamp(created_at))
    .bind(format!(
        "answer-bootstrap-harvest:{provider_account_id}:{INITIAL_GENERATION}"
    ))
    .bind(encode_timestamp(created_at))
    .bind(encode_timestamp(created_at))
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "INSERT INTO answer_bootstrap_harvests \
         (id, owner_user_id, provider_id, provider_account_id, generation, schedule_id, \
          state, scanned_task_count, total_task_count, watermark_sanitized_json, \
          created_at, started_at, updated_at, completed_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'pending', 0, NULL, '{}', ?, NULL, ?, NULL)",
    )
    .bind(harvest_id.to_string())
    .bind(owner_user_id.to_string())
    .bind(provider_id.as_str())
    .bind(provider_account_id.to_string())
    .bind(i64::from(INITIAL_GENERATION))
    .bind(schedule_id.to_string())
    .bind(encode_timestamp(created_at))
    .bind(encode_timestamp(created_at))
    .execute(&mut **transaction)
    .await?;
    Ok(harvest_id)
}

fn decode_harvest(row: &SqliteRow) -> Result<AnswerBootstrapHarvest, StorageError> {
    let harvest = AnswerBootstrapHarvest {
        id: parse_id(row.try_get("id")?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_harvest())?,
        provider_account_id: parse_id(row.try_get("provider_account_id")?)?,
        generation: decode_u32(row.try_get("generation")?)?,
        schedule_id: parse_id(row.try_get("schedule_id")?)?,
        state: decode_state(row.try_get("state")?)?,
        scanned_task_count: decode_u32(row.try_get("scanned_task_count")?)?,
        total_task_count: row
            .try_get::<Option<i64>, _>("total_task_count")?
            .map(decode_u32)
            .transpose()?,
        watermark_sanitized: serde_json::from_str(row.try_get("watermark_sanitized_json")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        started_at: row
            .try_get::<Option<&str>, _>("started_at")?
            .map(decode_timestamp)
            .transpose()?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
        completed_at: row
            .try_get::<Option<&str>, _>("completed_at")?
            .map(decode_timestamp)
            .transpose()?,
    };
    harvest.validate().map_err(|_| invalid_harvest())?;
    let job_kind: ScheduledJobKind = serde_json::from_str(row.try_get("payload_json")?)?;
    if row.try_get::<&str, _>("job_kind")? != "answer_bootstrap_harvest"
        || job_kind
            != (ScheduledJobKind::AnswerBootstrapHarvest {
                harvest_id: harvest.id,
                provider_account_id: harvest.provider_account_id,
                generation: harvest.generation,
            })
    {
        return Err(invalid_harvest());
    }
    Ok(harvest)
}

fn decode_state(value: &str) -> Result<AnswerBootstrapHarvestState, StorageError> {
    match value {
        "pending" => Ok(AnswerBootstrapHarvestState::Pending),
        "running" => Ok(AnswerBootstrapHarvestState::Running),
        "paused" => Ok(AnswerBootstrapHarvestState::Paused),
        "completed" => Ok(AnswerBootstrapHarvestState::Completed),
        "failed" => Ok(AnswerBootstrapHarvestState::Failed),
        "cancelled" => Ok(AnswerBootstrapHarvestState::Cancelled),
        _ => Err(invalid_harvest()),
    }
}

fn decode_u32(value: i64) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|_| invalid_harvest())
}

fn parse_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
    T::Err: Display,
{
    value.parse().map_err(|_| invalid_harvest())
}

fn invalid_harvest() -> StorageError {
    StorageError::InvalidData("answer bootstrap harvest is invalid or cross-bound".to_owned())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{ProviderAccountId, ProviderId, UserId};
    use chrono::{Duration, Utc};

    use super::*;
    use crate::{SchedulerRepository, SqliteSchedulerRepository};

    struct Fixture {
        database: Database,
        owner: UserId,
        account: ProviderAccountId,
        provider: ProviderId,
        harvest_id: AnswerBootstrapHarvestId,
        now: Timestamp,
    }

    impl Fixture {
        async fn initialized() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let provider = ProviderId::new("provider-alpha").unwrap();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'harvest-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'harvest', '\"idle\"', ?, ?)",
            )
            .bind(account.to_string())
            .bind(owner.to_string())
            .bind(provider.as_str())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            let mut transaction = database.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
            let harvest_id = ensure_initial_answer_bootstrap_harvest(
                &mut transaction,
                owner,
                &provider,
                account,
                now,
            )
            .await
            .unwrap();
            transaction.commit().await.unwrap();
            Self {
                database,
                owner,
                account,
                provider,
                harvest_id,
                now,
            }
        }

        fn repository(&self) -> SqliteAnswerBootstrapHarvestRepository {
            SqliteAnswerBootstrapHarvestRepository::new(self.database.clone())
        }

        async fn harvest(&self) -> AnswerBootstrapHarvest {
            self.repository()
                .find_owned_answer_bootstrap_harvest(self.owner, self.account, 1)
                .await
                .unwrap()
                .unwrap()
        }

        async fn claim(
            &self,
            worker_id: &str,
            at: Timestamp,
            lease_expires_at: Timestamp,
        ) -> ClaimedAnswerBootstrapHarvest {
            let claimed = self
                .repository()
                .claim_due_answer_bootstrap_harvests(worker_id, at, lease_expires_at, 1)
                .await
                .unwrap();
            assert_eq!(claimed.len(), 1);
            claimed.into_iter().next().unwrap()
        }

        async fn assert_generic_cannot_claim(&self, at: Timestamp, lease_expires_at: Timestamp) {
            let claimed = SqliteSchedulerRepository::new(self.database.clone())
                .claim_due("generic-worker", at, lease_expires_at, 1)
                .await
                .unwrap();
            assert!(claimed.is_empty());
        }
    }

    #[tokio::test]
    async fn initial_harvest_is_idempotent_and_account_cascade_removes_its_job() {
        let fixture = Fixture::initialized().await;
        let mut transaction = fixture
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap();
        let duplicate = ensure_initial_answer_bootstrap_harvest(
            &mut transaction,
            fixture.owner,
            &fixture.provider,
            fixture.account,
            fixture.now,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        assert_eq!(fixture.harvest_id, duplicate);
        assert_eq!(
            count(&fixture.database, "answer_bootstrap_harvests").await,
            1
        );
        assert_eq!(count(&fixture.database, "scheduled_jobs").await, 1);

        sqlx::query("DELETE FROM provider_accounts WHERE id = ?")
            .bind(fixture.account.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(
            count(&fixture.database, "answer_bootstrap_harvests").await,
            0
        );
        assert_eq!(count(&fixture.database, "scheduled_jobs").await, 0);
    }

    #[tokio::test]
    async fn claimed_harvest_checkpoints_retries_and_completes_without_progress_regression() {
        let fixture = Fixture::initialized().await;
        let repository = fixture.repository();
        let claimed_at = fixture.now + Duration::seconds(1);
        let lease = claimed_at + Duration::minutes(1);
        fixture.assert_generic_cannot_claim(claimed_at, lease).await;
        let claimed = fixture.claim("harvest-worker-a", claimed_at, lease).await;
        let schedule_id = claimed.harvest.schedule_id;
        assert_eq!(claimed.harvest.state, AnswerBootstrapHarvestState::Running);

        let watermark = serde_json::json!({"completed_remote_task": "task-1"});
        assert!(
            repository
                .checkpoint_answer_bootstrap_harvest(AnswerBootstrapHarvestCheckpoint {
                    harvest_id: fixture.harvest_id,
                    schedule_id,
                    worker_id: "not-owner",
                    scanned_task_count: 1,
                    total_task_count: Some(2),
                    watermark_sanitized: &watermark,
                    at: claimed_at + Duration::seconds(1),
                })
                .await
                .is_err()
        );
        let checkpoint = repository
            .checkpoint_answer_bootstrap_harvest(AnswerBootstrapHarvestCheckpoint {
                harvest_id: fixture.harvest_id,
                schedule_id,
                worker_id: "harvest-worker-a",
                scanned_task_count: 1,
                total_task_count: Some(2),
                watermark_sanitized: &watermark,
                at: claimed_at + Duration::seconds(2),
            })
            .await
            .unwrap();
        assert_eq!(checkpoint.scanned_task_count, 1);
        assert!(
            repository
                .checkpoint_answer_bootstrap_harvest(AnswerBootstrapHarvestCheckpoint {
                    harvest_id: fixture.harvest_id,
                    schedule_id,
                    worker_id: "harvest-worker-a",
                    scanned_task_count: 0,
                    total_task_count: Some(2),
                    watermark_sanitized: &serde_json::json!({}),
                    at: claimed_at + Duration::milliseconds(2_500),
                })
                .await
                .is_err()
        );
        let retry_at = claimed_at + Duration::minutes(2);
        let paused = repository
            .fail_answer_bootstrap_harvest(AnswerBootstrapHarvestFailure {
                harvest_id: fixture.harvest_id,
                schedule_id,
                worker_id: "harvest-worker-a",
                error_sanitized: "provider temporarily unavailable",
                retry_at: Some(retry_at),
                at: claimed_at + Duration::seconds(3),
            })
            .await
            .unwrap();
        assert_eq!(paused.state, AnswerBootstrapHarvestState::Paused);
        assert!(
            repository
                .claim_due_answer_bootstrap_harvests(
                    "harvest-worker-b",
                    retry_at - Duration::seconds(1),
                    retry_at + Duration::minutes(1),
                    1,
                )
                .await
                .unwrap()
                .is_empty()
        );

        let reclaimed = fixture
            .claim(
                "harvest-worker-b",
                retry_at,
                retry_at + Duration::minutes(1),
            )
            .await;
        assert_eq!(reclaimed.harvest.scanned_task_count, 1);
        let completed = repository
            .complete_answer_bootstrap_harvest(AnswerBootstrapHarvestCompletion {
                harvest_id: fixture.harvest_id,
                schedule_id,
                worker_id: "harvest-worker-b",
                scanned_task_count: 2,
                total_task_count: 2,
                watermark_sanitized: &serde_json::json!({"complete": true}),
                at: retry_at + Duration::seconds(1),
            })
            .await
            .unwrap();
        assert_eq!(completed.state, AnswerBootstrapHarvestState::Completed);
        assert_eq!(completed.scanned_task_count, 2);
        assert_eq!(fixture.harvest().await, completed);
    }

    #[tokio::test]
    async fn successful_page_yield_persists_cursor_without_consuming_retry_budget() {
        let fixture = Fixture::initialized().await;
        let repository = fixture.repository();
        let claimed_at = fixture.now + Duration::seconds(1);
        let claimed = fixture
            .claim(
                "page-worker-a",
                claimed_at,
                claimed_at + Duration::minutes(1),
            )
            .await;
        let next_run = claimed_at + Duration::seconds(5);
        let watermark = serde_json::json!({"version": 1, "cursor": {"page": 2}});
        let yielded = repository
            .yield_answer_bootstrap_harvest(AnswerBootstrapHarvestYield {
                harvest_id: fixture.harvest_id,
                schedule_id: claimed.harvest.schedule_id,
                worker_id: "page-worker-a",
                scanned_task_count: 2,
                total_task_count: None,
                watermark_sanitized: &watermark,
                run_at: next_run,
                at: claimed_at + Duration::seconds(1),
            })
            .await
            .unwrap();
        assert_eq!(yielded.state, AnswerBootstrapHarvestState::Paused);
        assert_eq!(yielded.scanned_task_count, 2);
        assert_eq!(yielded.watermark_sanitized, watermark);
        assert!(
            repository
                .claim_due_answer_bootstrap_harvests(
                    "page-worker-b",
                    next_run - Duration::milliseconds(1),
                    next_run + Duration::minutes(1),
                    1,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM scheduled_jobs")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(attempts, 0);
        let reclaimed = fixture
            .claim("page-worker-b", next_run, next_run + Duration::minutes(1))
            .await;
        assert_eq!(reclaimed.harvest.scanned_task_count, 2);
        assert_eq!(reclaimed.harvest.watermark_sanitized, watermark);
    }

    #[tokio::test]
    async fn expired_claim_at_attempt_budget_is_dead_lettered() {
        let fixture = Fixture::initialized().await;
        let repository = fixture.repository();
        let claimed_at = fixture.now + Duration::seconds(1);
        let lease = claimed_at + Duration::seconds(1);
        let claimed = repository
            .claim_due_answer_bootstrap_harvests("expiring-worker", claimed_at, lease, 1)
            .await
            .unwrap();
        sqlx::query("UPDATE scheduled_jobs SET attempts = 4 WHERE id = ?")
            .bind(claimed[0].harvest.schedule_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        let reclaimed = repository
            .claim_due_answer_bootstrap_harvests(
                "replacement-worker",
                lease,
                lease + Duration::minutes(1),
                1,
            )
            .await
            .unwrap();
        assert!(reclaimed.is_empty());
        assert_eq!(
            fixture.harvest().await.state,
            AnswerBootstrapHarvestState::Failed
        );
        let job_state: String = sqlx::query_scalar("SELECT state FROM scheduled_jobs")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(job_state, "dead_letter");
    }

    async fn count(database: &Database, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(database.pool())
            .await
            .unwrap()
    }
}
