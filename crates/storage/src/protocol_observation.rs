use std::str::FromStr;

use asterism_domain::{
    ExecutionId, ProtocolObservation, ProtocolObservationId, ProtocolObservationKind,
    ProtocolSurface, ProviderId, Timestamp,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, ProtocolObservationPage, ProtocolObservationRecordOutcome,
    ProtocolObservationRecordRequest, ProtocolObservationRepository, StorageError,
};

const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u64 = 1_000_000;

#[derive(Clone, Debug)]
pub struct SqliteProtocolObservationRepository {
    database: Database,
}

impl SqliteProtocolObservationRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProtocolObservationRepository for SqliteProtocolObservationRepository {
    async fn record_protocol_observation(
        &self,
        request: ProtocolObservationRecordRequest<'_>,
    ) -> Result<ProtocolObservationRecordOutcome, StorageError> {
        if request.occurrence_digest == [0; 32] {
            return Err(invalid_observation());
        }
        let shape_digest = ProtocolObservation::shape_digest(request.shape_sanitized)
            .map_err(|_| invalid_observation())?;
        let shape_json = serde_json::to_string(request.shape_sanitized)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        validate_execution_binding(&mut transaction, &request).await?;
        if let Some(existing) =
            find_by_occurrence(&mut transaction, request.occurrence_digest).await?
        {
            transaction.commit().await?;
            return Ok(ProtocolObservationRecordOutcome::Duplicate(existing));
        }
        let existing = find_aggregate(
            &mut transaction,
            &request.provider_id,
            request.surface,
            request.kind,
            shape_digest,
        )
        .await?;
        let (observation, created) = if let Some(mut observation) = existing {
            observation.occurrence_count = observation
                .occurrence_count
                .checked_add(1)
                .ok_or_else(invalid_observation)?;
            observation.first_seen_at = observation.first_seen_at.min(request.observed_at);
            if request.observed_at >= observation.last_seen_at {
                observation.last_seen_at = request.observed_at;
                observation.last_execution_id = request.execution_id;
            }
            observation.validate().map_err(|_| invalid_observation())?;
            sqlx::query(
                "UPDATE protocol_observations \
                 SET occurrence_count = ?, first_seen_at = ?, last_seen_at = ?, \
                     last_execution_id = ? WHERE id = ?",
            )
            .bind(i64::try_from(observation.occurrence_count).map_err(|_| invalid_observation())?)
            .bind(encode_timestamp(observation.first_seen_at))
            .bind(encode_timestamp(observation.last_seen_at))
            .bind(observation.last_execution_id.map(|id| id.to_string()))
            .bind(observation.id.to_string())
            .execute(&mut *transaction)
            .await?;
            (observation, false)
        } else {
            let observation = ProtocolObservation {
                id: ProtocolObservationId::new(),
                provider_id: request.provider_id.clone(),
                surface: request.surface,
                kind: request.kind,
                shape_digest,
                shape_sanitized: request.shape_sanitized.clone(),
                occurrence_count: 1,
                first_seen_at: request.observed_at,
                last_seen_at: request.observed_at,
                last_execution_id: request.execution_id,
            };
            observation.validate().map_err(|_| invalid_observation())?;
            sqlx::query(
                "INSERT INTO protocol_observations \
                 (id, provider_id, surface, kind, shape_digest, shape_sanitized_json, shape_bytes, \
                  occurrence_count, first_seen_at, last_seen_at, last_execution_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
            )
            .bind(observation.id.to_string())
            .bind(observation.provider_id.as_str())
            .bind(encode_surface(observation.surface))
            .bind(encode_kind(observation.kind))
            .bind(observation.shape_digest.as_slice())
            .bind(&shape_json)
            .bind(i64::try_from(shape_json.len()).map_err(|_| invalid_observation())?)
            .bind(encode_timestamp(observation.first_seen_at))
            .bind(encode_timestamp(observation.last_seen_at))
            .bind(observation.last_execution_id.map(|id| id.to_string()))
            .execute(&mut *transaction)
            .await?;
            (observation, true)
        };
        sqlx::query(
            "INSERT INTO protocol_observation_occurrences \
             (occurrence_digest, observation_id, execution_id, observed_at) VALUES (?, ?, ?, ?)",
        )
        .bind(request.occurrence_digest.as_slice())
        .bind(observation.id.to_string())
        .bind(request.execution_id.map(|id| id.to_string()))
        .bind(encode_timestamp(request.observed_at))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        if created {
            Ok(ProtocolObservationRecordOutcome::Created(observation))
        } else {
            Ok(ProtocolObservationRecordOutcome::Updated(observation))
        }
    }

    async fn list_protocol_observations(
        &self,
        provider_id: Option<&ProviderId>,
        kind: Option<ProtocolObservationKind>,
        limit: u32,
        offset: u64,
    ) -> Result<ProtocolObservationPage, StorageError> {
        if limit == 0 || limit > MAX_PAGE_SIZE || offset > MAX_OFFSET {
            return Err(StorageError::InvalidData(
                "protocol observation pagination is invalid".to_owned(),
            ));
        }
        let provider_id = provider_id.map(ProviderId::as_str);
        let kind = kind.map(encode_kind);
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM protocol_observations \
             WHERE (? IS NULL OR provider_id = ?) AND (? IS NULL OR kind = ?)",
        )
        .bind(provider_id)
        .bind(provider_id)
        .bind(kind)
        .bind(kind)
        .fetch_one(self.database.pool())
        .await?;
        let rows = sqlx::query(
            "SELECT * FROM protocol_observations \
             WHERE (? IS NULL OR provider_id = ?) AND (? IS NULL OR kind = ?) \
             ORDER BY last_seen_at DESC, provider_id, kind, id LIMIT ? OFFSET ?",
        )
        .bind(provider_id)
        .bind(provider_id)
        .bind(kind)
        .bind(kind)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        Ok(ProtocolObservationPage {
            items: rows
                .iter()
                .map(decode_observation)
                .collect::<Result<_, _>>()?,
            total: u64::try_from(total).map_err(|_| invalid_observation())?,
        })
    }
}

async fn validate_execution_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    request: &ProtocolObservationRecordRequest<'_>,
) -> Result<(), StorageError> {
    let Some(execution_id) = request.execution_id else {
        return Ok(());
    };
    let provider_id: Option<String> = sqlx::query_scalar(
        "SELECT account.provider_id FROM executions AS execution \
         INNER JOIN tasks AS task ON task.id = execution.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE execution.id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    if provider_id.as_deref() == Some(request.provider_id.as_str()) {
        Ok(())
    } else {
        Err(invalid_observation())
    }
}

async fn find_by_occurrence(
    transaction: &mut Transaction<'_, Sqlite>,
    occurrence_digest: [u8; 32],
) -> Result<Option<ProtocolObservation>, StorageError> {
    let row = sqlx::query(
        "SELECT observation.* FROM protocol_observation_occurrences AS occurrence \
         INNER JOIN protocol_observations AS observation \
                 ON observation.id = occurrence.observation_id \
         WHERE occurrence.occurrence_digest = ?",
    )
    .bind(occurrence_digest.as_slice())
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref().map(decode_observation).transpose()
}

async fn find_aggregate(
    transaction: &mut Transaction<'_, Sqlite>,
    provider_id: &ProviderId,
    surface: ProtocolSurface,
    kind: ProtocolObservationKind,
    shape_digest: [u8; 32],
) -> Result<Option<ProtocolObservation>, StorageError> {
    let row = sqlx::query(
        "SELECT * FROM protocol_observations \
         WHERE provider_id = ? AND surface = ? AND kind = ? AND shape_digest = ?",
    )
    .bind(provider_id.as_str())
    .bind(encode_surface(surface))
    .bind(encode_kind(kind))
    .bind(shape_digest.as_slice())
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref().map(decode_observation).transpose()
}

fn decode_observation(row: &SqliteRow) -> Result<ProtocolObservation, StorageError> {
    let shape_json: &str = row.try_get("shape_sanitized_json")?;
    let shape_bytes = usize::try_from(row.try_get::<i64, _>("shape_bytes")?)
        .map_err(|_| invalid_observation())?;
    if shape_json.len() != shape_bytes {
        return Err(invalid_observation());
    }
    let digest: Vec<u8> = row.try_get("shape_digest")?;
    let observation = ProtocolObservation {
        id: ProtocolObservationId::from_str(row.try_get("id")?)
            .map_err(|_| invalid_observation())?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_observation())?,
        surface: decode_surface(row.try_get("surface")?)?,
        kind: decode_kind(row.try_get("kind")?)?,
        shape_digest: digest.try_into().map_err(|_| invalid_observation())?,
        shape_sanitized: serde_json::from_str(shape_json)?,
        occurrence_count: u64::try_from(row.try_get::<i64, _>("occurrence_count")?)
            .map_err(|_| invalid_observation())?,
        first_seen_at: decode_timestamp(row.try_get("first_seen_at")?)?,
        last_seen_at: decode_timestamp(row.try_get("last_seen_at")?)?,
        last_execution_id: row
            .try_get::<Option<&str>, _>("last_execution_id")?
            .map(ExecutionId::from_str)
            .transpose()
            .map_err(|_| invalid_observation())?,
    };
    observation.validate().map_err(|_| invalid_observation())?;
    Ok(observation)
}

const fn encode_surface(surface: ProtocolSurface) -> &'static str {
    match surface {
        ProtocolSurface::Authentication => "authentication",
        ProtocolSurface::CourseInventory => "course_inventory",
        ProtocolSurface::TaskInventory => "task_inventory",
        ProtocolSurface::TaskDetail => "task_detail",
        ProtocolSurface::TaskProgress => "task_progress",
        ProtocolSurface::QuestionInventory => "question_inventory",
        ProtocolSurface::QuestionParse => "question_parse",
        ProtocolSurface::AnswerResolve => "answer_resolve",
        ProtocolSurface::SubmissionBuild => "submission_build",
        ProtocolSurface::SubmissionExecute => "submission_execute",
        ProtocolSurface::SubmissionVerify => "submission_verify",
        ProtocolSurface::TaskExecution => "task_execution",
        ProtocolSurface::BrowserBridge => "browser_bridge",
        ProtocolSurface::Other => "other",
    }
}

fn decode_surface(value: &str) -> Result<ProtocolSurface, StorageError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(StorageError::Serialization)
}

const fn encode_kind(kind: ProtocolObservationKind) -> &'static str {
    match kind {
        ProtocolObservationKind::UnknownQuestionKind => "unknown_question_kind",
        ProtocolObservationKind::UnknownResultShape => "unknown_result_shape",
        ProtocolObservationKind::UnknownTaskType => "unknown_task_type",
        ProtocolObservationKind::FieldDrift => "field_drift",
        ProtocolObservationKind::EndpointVersionDrift => "endpoint_version_drift",
        ProtocolObservationKind::Other => "other",
    }
}

fn decode_kind(value: &str) -> Result<ProtocolObservationKind, StorageError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(StorageError::Serialization)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| invalid_observation())
}

fn invalid_observation() -> StorageError {
    StorageError::InvalidData("protocol observation is invalid".to_owned())
}

#[cfg(test)]
mod tests {
    use asterism_domain::{AuthState, ProviderAccountId, TaskId, UserId};
    use chrono::Duration;

    use super::*;

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the transaction regression keeps occurrence replay, aggregation, Provider binding and secret rejection together"
    )]
    async fn protocol_observations_are_execution_bound_aggregated_and_replay_safe() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner = UserId::new();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'owner', 'hash', 'active', '[\"master\"]', '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        let account_id = ProviderAccountId::new();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'provider-alpha', 'Provider', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        let task_id = TaskId::new();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'task', 'fingerprint', 'work', 'routine', 'Task', 'pending', \
                     'failed', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        let execution_id = ExecutionId::new();
        sqlx::query(
            "INSERT INTO executions (id, task_id, request_source, state, created_at) \
             VALUES (?, ?, 'system', 'failed', ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();

        let repository = SqliteProtocolObservationRepository::new(database.clone());
        let shape = serde_json::json!({"type_code": 991, "fields": ["stem", "options"]});
        let request = |occurrence_digest, observed_at| ProtocolObservationRecordRequest {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            surface: ProtocolSurface::QuestionParse,
            kind: ProtocolObservationKind::UnknownQuestionKind,
            shape_sanitized: &shape,
            occurrence_digest,
            execution_id: Some(execution_id),
            observed_at,
        };
        let created = repository
            .record_protocol_observation(request([1; 32], now))
            .await
            .unwrap();
        let ProtocolObservationRecordOutcome::Created(created) = created else {
            panic!("expected created observation")
        };
        assert_eq!(created.occurrence_count, 1);
        let duplicate = repository
            .record_protocol_observation(request([1; 32], now))
            .await
            .unwrap();
        assert!(matches!(
            duplicate,
            ProtocolObservationRecordOutcome::Duplicate(ref observation)
                if observation.occurrence_count == 1
        ));
        let later = now + Duration::seconds(1);
        let updated = repository
            .record_protocol_observation(request([2; 32], later))
            .await
            .unwrap();
        assert!(matches!(
            updated,
            ProtocolObservationRecordOutcome::Updated(ref observation)
                if observation.id == created.id
                    && observation.occurrence_count == 2
                    && observation.last_seen_at == later
        ));
        let page = repository
            .list_protocol_observations(
                Some(&ProviderId::new("provider-alpha").unwrap()),
                Some(ProtocolObservationKind::UnknownQuestionKind),
                50,
                0,
            )
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].occurrence_count, 2);

        let foreign = ProtocolObservationRecordRequest {
            provider_id: ProviderId::new("provider-beta").unwrap(),
            ..request([3; 32], later)
        };
        assert!(
            repository
                .record_protocol_observation(foreign)
                .await
                .is_err()
        );
        let unsafe_shape = serde_json::json!({"access_token": "secret"});
        assert!(
            repository
                .record_protocol_observation(ProtocolObservationRecordRequest {
                    provider_id: ProviderId::new("provider-alpha").unwrap(),
                    surface: ProtocolSurface::Authentication,
                    kind: ProtocolObservationKind::FieldDrift,
                    shape_sanitized: &unsafe_shape,
                    occurrence_digest: [4; 32],
                    execution_id: None,
                    observed_at: later,
                })
                .await
                .is_err()
        );
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM protocol_observations), \
                    (SELECT COUNT(*) FROM protocol_observation_occurrences)",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(counts, (1, 2));
    }
}
