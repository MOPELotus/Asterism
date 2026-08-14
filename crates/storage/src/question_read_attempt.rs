use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, ProviderId, QuestionReadAttempt, QuestionReadAttemptId,
    QuestionReadAttemptState, TaskId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{Database, QuestionReadAttemptRepository, StorageError};

const ATTEMPT_SELECT: &str = "SELECT id, owner_user_id, provider_account_id, task_id, \
    provider_id, provider_version, operation_type, request_digest, state, question_snapshot_id, \
    question_session_id, response_digest, revision, expires_at, issued_at, completed_at, \
    created_at, updated_at FROM question_read_attempts";

#[derive(Clone, Debug)]
pub struct SqliteQuestionReadAttemptRepository {
    database: Database,
}

impl SqliteQuestionReadAttemptRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl QuestionReadAttemptRepository for SqliteQuestionReadAttemptRepository {
    async fn create_question_read_attempt(
        &self,
        attempt: &QuestionReadAttempt,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError> {
        validate_new_attempt(attempt)?;
        validate_correlation_id(correlation_id)?;
        if !label_belongs_to_provider(&attempt.provider_id, &attempt.operation_type) {
            return Err(StorageError::InvalidData(
                "Question read operation is outside its Provider scope".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        ensure_task_binding(&mut transaction, attempt).await?;
        sqlx::query(
            "INSERT INTO question_read_attempts \
             (id, owner_user_id, provider_account_id, task_id, provider_id, provider_version, \
              operation_type, request_digest, state, revision, expires_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'prepared', 1, ?, ?, ?)",
        )
        .bind(attempt.id.to_string())
        .bind(attempt.owner_user_id.to_string())
        .bind(attempt.provider_account_id.to_string())
        .bind(attempt.task_id.to_string())
        .bind(attempt.provider_id.as_str())
        .bind(&attempt.provider_version)
        .bind(&attempt.operation_type)
        .bind(attempt.request_digest.as_slice())
        .bind(encode_timestamp(attempt.expires_at))
        .bind(encode_timestamp(attempt.created_at))
        .bind(encode_timestamp(attempt.updated_at))
        .execute(&mut *transaction)
        .await?;
        insert_audit(
            &mut transaction,
            actor_type(actor),
            "question_read_attempt_created",
            correlation_id,
            attempt,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_owned_question_read_attempt(
        &self,
        owner_user_id: UserId,
        attempt_id: QuestionReadAttemptId,
    ) -> Result<Option<QuestionReadAttempt>, StorageError> {
        let query = format!("{ATTEMPT_SELECT} WHERE id = ? AND owner_user_id = ?");
        sqlx::query(&query)
            .bind(attempt_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_attempt)
            .transpose()
    }

    async fn find_latest_owned_question_read_attempt(
        &self,
        owner_user_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<QuestionReadAttempt>, StorageError> {
        let query = format!(
            "{ATTEMPT_SELECT} WHERE owner_user_id = ? AND task_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        );
        sqlx::query(&query)
            .bind(owner_user_id.to_string())
            .bind(task_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_attempt)
            .transpose()
    }

    async fn update_question_read_attempt(
        &self,
        attempt: &QuestionReadAttempt,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError> {
        validate_correlation_id(correlation_id)?;
        attempt
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if attempt.revision
            != expected_revision.checked_add(1).ok_or_else(|| {
                StorageError::InvalidData("Question read attempt revision is exhausted".to_owned())
            })?
        {
            return Err(StorageError::InvalidData(
                "Question read attempt revision is invalid".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(current) = fetch_attempt(&mut transaction, attempt.id).await? else {
            transaction.rollback().await?;
            return Ok(false);
        };
        if current.owner_user_id != attempt.owner_user_id
            || current.revision != expected_revision
            || !task_binding_is_valid(&mut transaction, &current).await?
        {
            transaction.rollback().await?;
            return Ok(false);
        }
        if attempt.state == QuestionReadAttemptState::Materialized
            && !materialization_binding_is_valid(&mut transaction, attempt).await?
        {
            transaction.rollback().await?;
            return Ok(false);
        }

        let mut expected = current;
        replay_transition(&mut expected, attempt)?;
        if &expected != attempt {
            return Err(StorageError::InvalidData(
                "Question read attempt changed immutable fields".to_owned(),
            ));
        }
        let result = sqlx::query(
            "UPDATE question_read_attempts SET state = ?, question_snapshot_id = ?, \
             question_session_id = ?, response_digest = ?, revision = ?, issued_at = ?, \
             completed_at = ?, updated_at = ? WHERE id = ? AND revision = ?",
        )
        .bind(state_name(attempt.state))
        .bind(attempt.question_snapshot_id.map(|id| id.to_string()))
        .bind(attempt.question_session_id.map(|id| id.to_string()))
        .bind(attempt.response_digest.map(|value| value.to_vec()))
        .bind(i64::from(attempt.revision))
        .bind(attempt.issued_at.map(encode_timestamp))
        .bind(attempt.completed_at.map(encode_timestamp))
        .bind(encode_timestamp(attempt.updated_at))
        .bind(attempt.id.to_string())
        .bind(i64::from(expected_revision))
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(false);
        }
        insert_audit(
            &mut transaction,
            actor_type(actor),
            action_name(attempt.state),
            correlation_id,
            attempt,
        )
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
}

fn replay_transition(
    current: &mut QuestionReadAttempt,
    target: &QuestionReadAttempt,
) -> Result<(), StorageError> {
    let result = match target.state {
        QuestionReadAttemptState::Issued => current.issue(target.updated_at),
        QuestionReadAttemptState::Ambiguous => current.mark_ambiguous(target.updated_at),
        QuestionReadAttemptState::Materialized => current.materialize(
            target.question_snapshot_id.ok_or_else(invalid_target)?,
            target.question_session_id.ok_or_else(invalid_target)?,
            target.response_digest.ok_or_else(invalid_target)?,
            target.updated_at,
        ),
        QuestionReadAttemptState::Rejected => current.reject(
            target.response_digest.ok_or_else(invalid_target)?,
            target.updated_at,
        ),
        QuestionReadAttemptState::Cancelled => current.cancel(target.updated_at),
        QuestionReadAttemptState::Expired => current.expire(target.updated_at),
        QuestionReadAttemptState::Prepared => {
            Err(asterism_domain::QuestionReadAttemptError::InvalidTransition)
        }
    };
    result.map_err(|error| StorageError::InvalidData(error.to_string()))
}

fn invalid_target() -> StorageError {
    StorageError::InvalidData("Question read attempt target binding is incomplete".to_owned())
}

async fn fetch_attempt(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
) -> Result<Option<QuestionReadAttempt>, StorageError> {
    let query = format!("{ATTEMPT_SELECT} WHERE id = ?");
    sqlx::query(&query)
        .bind(attempt_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_attempt)
        .transpose()
}

fn decode_attempt(row: &SqliteRow) -> Result<QuestionReadAttempt, StorageError> {
    let request_digest = decode_digest(row.try_get("request_digest")?)?;
    let response_digest = row
        .try_get::<Option<Vec<u8>>, _>("response_digest")?
        .map(decode_digest)
        .transpose()?;
    let attempt = QuestionReadAttempt {
        id: parse_id(row.try_get("id")?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id")?)?,
        provider_account_id: parse_id(row.try_get("provider_account_id")?)?,
        task_id: parse_id(row.try_get("task_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_version: row.try_get("provider_version")?,
        operation_type: row.try_get("operation_type")?,
        request_digest,
        state: decode_state(row.try_get("state")?)?,
        question_snapshot_id: row
            .try_get::<Option<&str>, _>("question_snapshot_id")?
            .map(parse_id)
            .transpose()?,
        question_session_id: row
            .try_get::<Option<&str>, _>("question_session_id")?
            .map(parse_id)
            .transpose()?,
        response_digest,
        revision: u32::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| StorageError::InvalidData("invalid attempt revision".to_owned()))?,
        expires_at: decode_timestamp(row.try_get("expires_at")?)?,
        issued_at: decode_optional_timestamp(row.try_get("issued_at")?)?,
        completed_at: decode_optional_timestamp(row.try_get("completed_at")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    attempt
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(attempt)
}

fn validate_new_attempt(attempt: &QuestionReadAttempt) -> Result<(), StorageError> {
    attempt
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if attempt.state != QuestionReadAttemptState::Prepared || attempt.revision != 1 {
        return Err(StorageError::InvalidData(
            "new Question read attempt must be prepared".to_owned(),
        ));
    }
    Ok(())
}

async fn ensure_task_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt: &QuestionReadAttempt,
) -> Result<(), StorageError> {
    if task_binding_is_valid(transaction, attempt).await? {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "Question read attempt owner/account/Task binding is invalid".to_owned(),
        ))
    }
}

async fn task_binding_is_valid(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt: &QuestionReadAttempt,
) -> Result<bool, StorageError> {
    let valid: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users \
         INNER JOIN provider_accounts AS account ON account.owner_user_id = users.id \
         INNER JOIN tasks AS task ON task.provider_account_id = account.id \
         WHERE users.id = ? AND users.status = 'active' AND account.id = ? \
           AND account.provider_id = ? AND task.id = ?)",
    )
    .bind(attempt.owner_user_id.to_string())
    .bind(attempt.provider_account_id.to_string())
    .bind(attempt.provider_id.as_str())
    .bind(attempt.task_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    Ok(valid == 1)
}

async fn materialization_binding_is_valid(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt: &QuestionReadAttempt,
) -> Result<bool, StorageError> {
    let Some(snapshot_id) = attempt.question_snapshot_id else {
        return Ok(false);
    };
    let Some(session_id) = attempt.question_session_id else {
        return Ok(false);
    };
    let valid: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM question_sessions AS session \
         INNER JOIN question_snapshots AS snapshot ON snapshot.id = session.question_snapshot_id \
         INNER JOIN question_session_continuations AS continuation \
            ON continuation.session_id = session.id \
         INNER JOIN secret_blobs AS secret ON secret.id = continuation.secret_blob_id \
         WHERE session.id = ? AND session.owner_user_id = ? AND session.provider_account_id = ? \
           AND session.task_id = ? AND session.provider_id = ? AND session.provider_version = ? \
           AND session.question_snapshot_id = ? AND session.state = 'active' \
           AND session.execution_id IS NULL AND snapshot.task_id = session.task_id \
           AND snapshot.provider_id = session.provider_id \
           AND snapshot.provider_version = session.provider_version \
           AND continuation.execution_id IS NULL AND continuation.revision = 1 \
           AND continuation.continuation_type = session.artifact_type \
           AND continuation.continuation_digest = session.artifact_digest \
           AND secret.owner_user_id = session.owner_user_id \
           AND secret.purpose = 'browser_job_credential' AND secret.version = 1)",
    )
    .bind(session_id.to_string())
    .bind(attempt.owner_user_id.to_string())
    .bind(attempt.provider_account_id.to_string())
    .bind(attempt.task_id.to_string())
    .bind(attempt.provider_id.as_str())
    .bind(&attempt.provider_version)
    .bind(snapshot_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    Ok(valid == 1)
}

async fn insert_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: (&str, Option<String>),
    action: &str,
    correlation_id: &str,
    attempt: &QuestionReadAttempt,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'question_read_attempt', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(attempt.updated_at))
    .bind(actor.0)
    .bind(actor.1)
    .bind(action)
    .bind(attempt.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "state": state_name(attempt.state),
            "revision": attempt.revision,
            "provider_id": attempt.provider_id,
            "provider_version": attempt.provider_version,
            "task_id": attempt.task_id,
            "operation_type": attempt.operation_type,
            "request_digest": "[HASHED]",
            "response_digest": attempt.response_digest.map(|_| "[HASHED]"),
            "question_snapshot_id": attempt.question_snapshot_id,
            "question_session_id": attempt.question_session_id,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn state_name(state: QuestionReadAttemptState) -> &'static str {
    match state {
        QuestionReadAttemptState::Prepared => "prepared",
        QuestionReadAttemptState::Issued => "issued",
        QuestionReadAttemptState::Ambiguous => "ambiguous",
        QuestionReadAttemptState::Materialized => "materialized",
        QuestionReadAttemptState::Rejected => "rejected",
        QuestionReadAttemptState::Cancelled => "cancelled",
        QuestionReadAttemptState::Expired => "expired",
    }
}

fn decode_state(value: &str) -> Result<QuestionReadAttemptState, StorageError> {
    match value {
        "prepared" => Ok(QuestionReadAttemptState::Prepared),
        "issued" => Ok(QuestionReadAttemptState::Issued),
        "ambiguous" => Ok(QuestionReadAttemptState::Ambiguous),
        "materialized" => Ok(QuestionReadAttemptState::Materialized),
        "rejected" => Ok(QuestionReadAttemptState::Rejected),
        "cancelled" => Ok(QuestionReadAttemptState::Cancelled),
        "expired" => Ok(QuestionReadAttemptState::Expired),
        _ => Err(StorageError::InvalidData(
            "invalid Question read attempt state".to_owned(),
        )),
    }
}

fn action_name(state: QuestionReadAttemptState) -> &'static str {
    match state {
        QuestionReadAttemptState::Issued => "question_read_attempt_issued",
        QuestionReadAttemptState::Ambiguous => "question_read_attempt_ambiguous",
        QuestionReadAttemptState::Materialized => "question_read_attempt_materialized",
        QuestionReadAttemptState::Rejected => "question_read_attempt_rejected",
        QuestionReadAttemptState::Cancelled => "question_read_attempt_cancelled",
        QuestionReadAttemptState::Expired => "question_read_attempt_expired",
        QuestionReadAttemptState::Prepared => unreachable!(),
    }
}

fn label_belongs_to_provider(provider_id: &ProviderId, value: &str) -> bool {
    value
        .strip_prefix(provider_id.as_str())
        .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

fn actor_type(actor: AuditActor) -> (&'static str, Option<String>) {
    match actor {
        AuditActor::User(id) => ("user", Some(id.to_string())),
        AuditActor::ServiceToken(id) => ("service_token", Some(id.to_string())),
    }
}

fn validate_correlation_id(value: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(StorageError::InvalidData(
            "Question read attempt correlation ID is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn decode_digest(value: Vec<u8>) -> Result<[u8; 32], StorageError> {
    value
        .try_into()
        .map_err(|_| StorageError::InvalidData("invalid attempt digest".to_owned()))
}

fn parse_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error: T::Err| StorageError::InvalidData(error.to_string()))
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
    use asterism_domain::{ProviderAccountId, QuestionSessionId, QuestionSnapshotId};
    use chrono::{Duration, Utc};

    use super::*;
    use crate::{QuestionSessionRepository, SqliteQuestionSessionRepository};

    #[tokio::test]
    async fn issued_attempt_materializes_only_with_bound_snapshot_session_and_artifact() {
        let fixture = Fixture::new().await;
        let mut attempt = fixture.attempt();
        fixture.create(&attempt).await;
        let revision = attempt.revision;
        attempt.issue(fixture.now + Duration::seconds(1)).unwrap();
        assert!(fixture.update(&attempt, revision, "read-issued").await);

        let (snapshot_id, session_id) = fixture.question_material().await;
        let revision = attempt.revision;
        attempt
            .materialize(
                snapshot_id,
                session_id,
                [9; 32],
                fixture.now + Duration::seconds(2),
            )
            .unwrap();
        assert!(
            fixture
                .update(&attempt, revision, "read-materialized")
                .await
        );
        assert_eq!(
            fixture
                .repository
                .find_owned_question_read_attempt(fixture.owner, attempt.id)
                .await
                .unwrap(),
            Some(attempt)
        );
    }

    #[tokio::test]
    async fn ambiguous_attempt_recovers_without_becoming_reissuable() {
        let fixture = Fixture::new().await;
        let mut attempt = fixture.attempt();
        fixture.create(&attempt).await;
        let revision = attempt.revision;
        attempt.issue(fixture.now + Duration::seconds(1)).unwrap();
        assert!(fixture.update(&attempt, revision, "read-issued").await);
        let revision = attempt.revision;
        attempt
            .mark_ambiguous(fixture.now + Duration::seconds(2))
            .unwrap();
        assert!(fixture.update(&attempt, revision, "read-ambiguous").await);

        let (snapshot_id, session_id) = fixture.question_material().await;
        let revision = attempt.revision;
        attempt
            .materialize(
                snapshot_id,
                session_id,
                [8; 32],
                fixture.now + Duration::seconds(3),
            )
            .unwrap();
        assert!(fixture.update(&attempt, revision, "read-recovered").await);
        assert_eq!(attempt.revision, 4);
    }

    #[tokio::test]
    async fn foreign_or_incomplete_materialization_fails_closed() {
        let fixture = Fixture::new().await;
        let mut attempt = fixture.attempt();
        fixture.create(&attempt).await;
        let revision = attempt.revision;
        attempt.issue(fixture.now + Duration::seconds(1)).unwrap();
        assert!(fixture.update(&attempt, revision, "read-issued").await);
        let revision = attempt.revision;
        attempt
            .materialize(
                QuestionSnapshotId::new(),
                QuestionSessionId::new(),
                [7; 32],
                fixture.now + Duration::seconds(2),
            )
            .unwrap();
        assert!(!fixture.update(&attempt, revision, "read-foreign").await);
    }

    struct Fixture {
        database: Database,
        repository: SqliteQuestionReadAttemptRepository,
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        provider: ProviderId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let task = TaskId::new();
            let provider = ProviderId::new("cidaren").unwrap();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'read-attempt-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'Read Attempt', '\"authenticated\"', ?, ?)",
            )
            .bind(account.to_string())
            .bind(owner.to_string())
            .bind(provider.as_str())
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
                 VALUES (?, ?, 'read-1', 'v1:read', 'exercise', 'unknown', 'Read', \
                         'pending', 'discovered', ?, ?, '[\"question_parse\"]')",
            )
            .bind(task.to_string())
            .bind(account.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            let repository = SqliteQuestionReadAttemptRepository::new(database.clone());
            Self {
                database,
                repository,
                owner,
                account,
                task,
                provider,
                now,
            }
        }

        fn attempt(&self) -> QuestionReadAttempt {
            QuestionReadAttempt::prepared(
                self.owner,
                self.account,
                self.task,
                self.provider.clone(),
                "jv99-v1".to_owned(),
                "cidaren.start-answer.v1".to_owned(),
                [1; 32],
                self.now,
                self.now + Duration::minutes(5),
            )
            .unwrap()
        }

        async fn create(&self, attempt: &QuestionReadAttempt) {
            self.repository
                .create_question_read_attempt(attempt, AuditActor::User(self.owner), "read-create")
                .await
                .unwrap();
        }

        async fn update(
            &self,
            attempt: &QuestionReadAttempt,
            revision: u32,
            correlation_id: &str,
        ) -> bool {
            self.repository
                .update_question_read_attempt(
                    attempt,
                    revision,
                    AuditActor::User(self.owner),
                    correlation_id,
                )
                .await
                .unwrap()
        }

        async fn question_material(&self) -> (QuestionSnapshotId, QuestionSessionId) {
            let snapshot_id = QuestionSnapshotId::new();
            let timestamp = encode_timestamp(self.now);
            sqlx::query(
                "INSERT INTO question_snapshots \
                 (id, task_id, provider_id, provider_version, captured_at, question_count, total_bytes) \
                 VALUES (?, ?, ?, 'jv99-v1', ?, 0, 0)",
            )
            .bind(snapshot_id.to_string())
            .bind(self.task.to_string())
            .bind(self.provider.as_str())
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            let session = asterism_domain::QuestionSession::active(
                self.owner,
                self.account,
                self.task,
                self.provider.clone(),
                "jv99-v1".to_owned(),
                snapshot_id,
                "cidaren.question-attempt.v1".to_owned(),
                [7; 32],
                self.now,
                self.now + Duration::minutes(5),
            )
            .unwrap();
            SqliteQuestionSessionRepository::new(self.database.clone())
                .create_question_session(
                    &session,
                    AuditActor::User(self.owner),
                    "read-session-create",
                )
                .await
                .unwrap();
            let secret_id = asterism_domain::SecretId::new();
            sqlx::query(
                "INSERT INTO secret_blobs \
                 (id, owner_user_id, purpose, key_id, nonce, encrypted_data, version, \
                  created_at, updated_at) VALUES (?, ?, 'browser_job_credential', 'test-key', \
                  ?, ?, 1, ?, ?)",
            )
            .bind(secret_id.to_string())
            .bind(self.owner.to_string())
            .bind(vec![0_u8; 24])
            .bind(vec![0_u8; 16])
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO question_session_continuations \
                 (session_id, secret_blob_id, continuation_type, continuation_digest, phase, \
                  revision, created_at, updated_at) VALUES (?, ?, ?, ?, 'questions_ready', 1, ?, ?)",
            )
            .bind(session.id.to_string())
            .bind(secret_id.to_string())
            .bind(&session.artifact_type)
            .bind(session.artifact_digest.as_slice())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            (snapshot_id, session.id)
        }
    }
}
