use std::{fmt::Display, str::FromStr};

use asterism_domain::{
    AnswerBootstrapHarvest, AnswerBootstrapHarvestId, AnswerBootstrapHarvestState,
    ProviderAccountId, ProviderId, ScheduleId, Timestamp, UserId,
};
use asterism_scheduler::ScheduledJobKind;
use async_trait::async_trait;
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::auth_session::{decode_timestamp, encode_timestamp};
use crate::{AnswerBootstrapHarvestRepository, Database, StorageError};

const INITIAL_GENERATION: u32 = 1;

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
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn initial_harvest_is_idempotent_and_account_cascade_removes_its_job() {
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
        let first = ensure_initial_answer_bootstrap_harvest(
            &mut transaction,
            owner,
            &provider,
            account,
            now,
        )
        .await
        .unwrap();
        let duplicate = ensure_initial_answer_bootstrap_harvest(
            &mut transaction,
            owner,
            &provider,
            account,
            now,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(count(&database, "answer_bootstrap_harvests").await, 1);
        assert_eq!(count(&database, "scheduled_jobs").await, 1);

        sqlx::query("DELETE FROM provider_accounts WHERE id = ?")
            .bind(account.to_string())
            .execute(database.pool())
            .await
            .unwrap();
        assert_eq!(count(&database, "answer_bootstrap_harvests").await, 0);
        assert_eq!(count(&database, "scheduled_jobs").await, 0);
    }

    async fn count(database: &Database, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(database.pool())
            .await
            .unwrap()
    }
}
