use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, ExecutionId, ProviderId, QuestionSession, QuestionSessionId,
    QuestionSessionState, QuestionSnapshotId, TaskCapability, TaskId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{Database, QuestionSessionClaimOutcome, QuestionSessionRepository, StorageError};

const SESSION_SELECT: &str = "SELECT id, owner_user_id, provider_account_id, task_id, provider_id, \
    provider_version, question_snapshot_id, artifact_type, artifact_digest, state, execution_id, \
    revision, expires_at, claimed_at, closed_at, created_at, updated_at FROM question_sessions";

#[derive(Clone, Debug)]
pub struct SqliteQuestionSessionRepository {
    database: Database,
}

impl SqliteQuestionSessionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl QuestionSessionRepository for SqliteQuestionSessionRepository {
    async fn create_question_session(
        &self,
        session: &QuestionSession,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError> {
        validate_new_session(session)?;
        validate_correlation_id(correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        ensure_binding(&mut transaction, session).await?;
        sqlx::query(
            "INSERT INTO question_sessions \
             (id, owner_user_id, provider_account_id, task_id, provider_id, provider_version, \
              question_snapshot_id, artifact_type, artifact_digest, state, execution_id, \
              revision, expires_at, claimed_at, closed_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, NULL, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(session.owner_user_id.to_string())
        .bind(session.provider_account_id.to_string())
        .bind(session.task_id.to_string())
        .bind(session.provider_id.as_str())
        .bind(&session.provider_version)
        .bind(session.question_snapshot_id.to_string())
        .bind(&session.artifact_type)
        .bind(session.artifact_digest.as_slice())
        .bind(state_name(session.state))
        .bind(i64::from(session.revision))
        .bind(encode_timestamp(session.expires_at))
        .bind(encode_timestamp(session.created_at))
        .bind(encode_timestamp(session.updated_at))
        .execute(&mut *transaction)
        .await?;
        insert_audit(
            &mut transaction,
            actor_type(actor),
            "question_session_created",
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_owned_question_session(
        &self,
        owner_user_id: UserId,
        session_id: QuestionSessionId,
    ) -> Result<Option<QuestionSession>, StorageError> {
        let query = format!("{SESSION_SELECT} WHERE id = ? AND owner_user_id = ?");
        sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_session)
            .transpose()
    }

    async fn find_question_session_for_execution(
        &self,
        execution_id: ExecutionId,
    ) -> Result<Option<QuestionSession>, StorageError> {
        let query = format!("{SESSION_SELECT} WHERE execution_id = ?");
        sqlx::query(&query)
            .bind(execution_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_session)
            .transpose()
    }

    async fn claim_question_session_for_execution(
        &self,
        execution_id: ExecutionId,
        claimed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<QuestionSessionClaimOutcome, StorageError> {
        validate_correlation_id(correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(binding) = fetch_execution_binding(&mut transaction, execution_id).await? else {
            transaction.rollback().await?;
            return Ok(QuestionSessionClaimOutcome::NotFound);
        };
        if !binding.is_submission_execution() {
            transaction.rollback().await?;
            return Ok(QuestionSessionClaimOutcome::BindingConflict);
        }
        let Some(mut session) = fetch_by_snapshot(&mut transaction, binding.snapshot_id).await?
        else {
            transaction.rollback().await?;
            return Ok(QuestionSessionClaimOutcome::NotFound);
        };
        if !binding.matches(&session) || !binding_is_valid(&mut transaction, &session).await? {
            transaction.rollback().await?;
            return Ok(QuestionSessionClaimOutcome::BindingConflict);
        }

        match session.state {
            QuestionSessionState::Claimed if session.execution_id == Some(execution_id) => {
                if !continuation_matches_claim(
                    &mut transaction,
                    &session,
                    Some(execution_id),
                    false,
                )
                .await?
                {
                    transaction.rollback().await?;
                    return Ok(QuestionSessionClaimOutcome::BindingConflict);
                }
                transaction.rollback().await?;
                return Ok(QuestionSessionClaimOutcome::Existing(session));
            }
            QuestionSessionState::Claimed => {
                transaction.rollback().await?;
                return Ok(QuestionSessionClaimOutcome::BindingConflict);
            }
            QuestionSessionState::Consumed
            | QuestionSessionState::Cancelled
            | QuestionSessionState::Expired => {
                transaction.rollback().await?;
                return Ok(QuestionSessionClaimOutcome::StateConflict(session));
            }
            QuestionSessionState::Active => {}
        }

        if !continuation_matches_claim(&mut transaction, &session, None, true).await? {
            transaction.rollback().await?;
            return Ok(QuestionSessionClaimOutcome::BindingConflict);
        }

        let expected_revision = session.revision;
        if session.is_expired_at(claimed_at) {
            session
                .expire(claimed_at)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?;
            persist_transition(&mut transaction, &session, expected_revision).await?;
            insert_audit(
                &mut transaction,
                ("execution", Some(execution_id.to_string())),
                "question_session_expired",
                correlation_id,
                &session,
            )
            .await?;
            transaction.commit().await?;
            return Ok(QuestionSessionClaimOutcome::Expired(session));
        }

        session
            .claim(execution_id, claimed_at)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        persist_transition(&mut transaction, &session, expected_revision).await?;
        bind_continuation_to_execution(&mut transaction, session.id, execution_id).await?;
        insert_audit(
            &mut transaction,
            ("execution", Some(execution_id.to_string())),
            "question_session_claimed",
            correlation_id,
            &session,
        )
        .await?;
        transaction.commit().await?;
        Ok(QuestionSessionClaimOutcome::Claimed(session))
    }

    async fn update_question_session(
        &self,
        session: &QuestionSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError> {
        validate_correlation_id(correlation_id)?;
        session
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if session.revision
            != expected_revision.checked_add(1).ok_or_else(|| {
                StorageError::InvalidData("Question session revision is exhausted".to_owned())
            })?
        {
            return Err(StorageError::InvalidData(
                "Question session revision is invalid".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(current) = fetch_by_id(&mut transaction, session.id).await? else {
            transaction.rollback().await?;
            return Ok(false);
        };
        if current.revision != expected_revision
            || current.owner_user_id != session.owner_user_id
            || !binding_is_valid(&mut transaction, &current).await?
        {
            transaction.rollback().await?;
            return Ok(false);
        }
        if let Some(execution_id) = current.execution_id {
            let Some(binding) = fetch_execution_binding(&mut transaction, execution_id).await?
            else {
                transaction.rollback().await?;
                return Ok(false);
            };
            if !binding.matches(&current) {
                transaction.rollback().await?;
                return Ok(false);
            }
        }

        let mut expected = current;
        match session.state {
            QuestionSessionState::Consumed => expected.consume(session.updated_at),
            QuestionSessionState::Cancelled => expected.cancel(session.updated_at),
            QuestionSessionState::Expired => expected.expire(session.updated_at),
            QuestionSessionState::Active | QuestionSessionState::Claimed => {
                Err(asterism_domain::QuestionSessionError::InvalidTransition)
            }
        }
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        if &expected != session {
            return Err(StorageError::InvalidData(
                "Question session changed immutable fields".to_owned(),
            ));
        }
        persist_transition(&mut transaction, session, expected_revision).await?;
        insert_audit(
            &mut transaction,
            actor_type(actor),
            match session.state {
                QuestionSessionState::Consumed => "question_session_consumed",
                QuestionSessionState::Cancelled => "question_session_cancelled",
                QuestionSessionState::Expired => "question_session_expired",
                QuestionSessionState::Active | QuestionSessionState::Claimed => unreachable!(),
            },
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
}

#[derive(Clone, Debug)]
struct ExecutionBinding {
    task_id: TaskId,
    snapshot_id: QuestionSnapshotId,
    provider_id: ProviderId,
    requested_capabilities: Vec<TaskCapability>,
    state: String,
}

impl ExecutionBinding {
    fn is_submission_execution(&self) -> bool {
        self.requested_capabilities == [TaskCapability::SubmissionExecute]
            && matches!(
                self.state.as_str(),
                "running" | "recovering" | "retry_waiting"
            )
    }

    fn matches(&self, session: &QuestionSession) -> bool {
        self.task_id == session.task_id
            && self.snapshot_id == session.question_snapshot_id
            && self.provider_id == session.provider_id
    }
}

async fn fetch_execution_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
) -> Result<Option<ExecutionBinding>, StorageError> {
    let row = sqlx::query(
        "SELECT execution.task_id, execution.requested_capabilities_json, execution.state, \
                draft.question_snapshot_id, draft.provider_id \
         FROM executions AS execution \
         INNER JOIN submission_drafts AS draft ON draft.id = execution.submission_draft_id \
         WHERE execution.id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    row.as_ref().map(decode_execution_binding).transpose()
}

fn decode_execution_binding(row: &SqliteRow) -> Result<ExecutionBinding, StorageError> {
    Ok(ExecutionBinding {
        task_id: parse_id(row.try_get("task_id")?)?,
        snapshot_id: parse_id(row.try_get("question_snapshot_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        requested_capabilities: serde_json::from_str(row.try_get("requested_capabilities_json")?)?,
        state: row.try_get("state")?,
    })
}

async fn fetch_by_id(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: QuestionSessionId,
) -> Result<Option<QuestionSession>, StorageError> {
    let query = format!("{SESSION_SELECT} WHERE id = ?");
    sqlx::query(&query)
        .bind(session_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_session)
        .transpose()
}

async fn fetch_by_snapshot(
    transaction: &mut Transaction<'_, Sqlite>,
    snapshot_id: QuestionSnapshotId,
) -> Result<Option<QuestionSession>, StorageError> {
    let query = format!("{SESSION_SELECT} WHERE question_snapshot_id = ?");
    sqlx::query(&query)
        .bind(snapshot_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_session)
        .transpose()
}

async fn persist_transition(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &QuestionSession,
    expected_revision: u32,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "UPDATE question_sessions SET state = ?, execution_id = ?, revision = ?, \
         claimed_at = ?, closed_at = ?, updated_at = ? WHERE id = ? AND revision = ?",
    )
    .bind(state_name(session.state))
    .bind(session.execution_id.map(|id| id.to_string()))
    .bind(i64::from(session.revision))
    .bind(session.claimed_at.map(encode_timestamp))
    .bind(session.closed_at.map(encode_timestamp))
    .bind(encode_timestamp(session.updated_at))
    .bind(session.id.to_string())
    .bind(i64::from(expected_revision))
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() != 1 {
        return Err(StorageError::InvalidData(
            "Question session transition lost its revision race".to_owned(),
        ));
    }
    Ok(())
}

async fn continuation_matches_claim(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &QuestionSession,
    expected_execution_id: Option<ExecutionId>,
    require_initial: bool,
) -> Result<bool, StorageError> {
    let row = sqlx::query(
        "SELECT continuation.execution_id, continuation.continuation_type, \
                continuation.continuation_digest, continuation.revision, \
                secret.owner_user_id, secret.purpose, secret.version \
         FROM question_session_continuations AS continuation \
         INNER JOIN secret_blobs AS secret ON secret.id = continuation.secret_blob_id \
         WHERE continuation.session_id = ?",
    )
    .bind(session.id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let execution_id = row
        .try_get::<Option<&str>, _>("execution_id")?
        .map(parse_id)
        .transpose()?;
    let continuation_digest: [u8; 32] = row
        .try_get::<Vec<u8>, _>("continuation_digest")?
        .try_into()
        .map_err(|_| StorageError::InvalidData("invalid continuation digest".to_owned()))?;
    let continuation_revision = u32::try_from(row.try_get::<i64, _>("revision")?)
        .map_err(|_| StorageError::InvalidData("invalid continuation revision".to_owned()))?;
    let secret_version = u32::try_from(row.try_get::<i64, _>("version")?)
        .map_err(|_| StorageError::InvalidData("invalid continuation secret version".to_owned()))?;
    let secret_owner_user_id: UserId = parse_id(row.try_get("owner_user_id")?)?;
    let common_valid = execution_id == expected_execution_id
        && secret_owner_user_id == session.owner_user_id
        && row.try_get::<&str, _>("purpose")? == "browser_job_credential"
        && continuation_revision == secret_version;
    Ok(common_valid
        && (!require_initial
            || (continuation_revision == 1
                && row.try_get::<&str, _>("continuation_type")? == session.artifact_type
                && continuation_digest == session.artifact_digest)))
}

async fn bind_continuation_to_execution(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: QuestionSessionId,
    execution_id: ExecutionId,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "UPDATE question_session_continuations SET execution_id = ? \
         WHERE session_id = ? AND execution_id IS NULL",
    )
    .bind(execution_id.to_string())
    .bind(session_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "Question continuation lost its Execution claim".to_owned(),
        ))
    }
}

fn decode_session(row: &SqliteRow) -> Result<QuestionSession, StorageError> {
    let artifact_digest: [u8; 32] = row
        .try_get::<Vec<u8>, _>("artifact_digest")?
        .try_into()
        .map_err(|_| StorageError::InvalidData("invalid Question artifact digest".to_owned()))?;
    let session = QuestionSession {
        id: parse_id(row.try_get("id")?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id")?)?,
        provider_account_id: parse_id(row.try_get("provider_account_id")?)?,
        task_id: parse_id(row.try_get("task_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_version: row.try_get("provider_version")?,
        question_snapshot_id: parse_id(row.try_get("question_snapshot_id")?)?,
        artifact_type: row.try_get("artifact_type")?,
        artifact_digest,
        state: decode_state(row.try_get("state")?)?,
        execution_id: row
            .try_get::<Option<&str>, _>("execution_id")?
            .map(parse_id)
            .transpose()?,
        revision: u32::try_from(row.try_get::<i64, _>("revision")?).map_err(|_| {
            StorageError::InvalidData("invalid Question session revision".to_owned())
        })?,
        expires_at: decode_timestamp(row.try_get("expires_at")?)?,
        claimed_at: decode_optional_timestamp(row.try_get("claimed_at")?)?,
        closed_at: decode_optional_timestamp(row.try_get("closed_at")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(session)
}

fn validate_new_session(session: &QuestionSession) -> Result<(), StorageError> {
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if session.state != QuestionSessionState::Active
        || session.execution_id.is_some()
        || session.revision != 1
    {
        return Err(StorageError::InvalidData(
            "new Question session must be active and unclaimed".to_owned(),
        ));
    }
    Ok(())
}

async fn ensure_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &QuestionSession,
) -> Result<(), StorageError> {
    if binding_is_valid(transaction, session).await? {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "Question session owner/account/Task/Snapshot binding is invalid".to_owned(),
        ))
    }
}

async fn binding_is_valid(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &QuestionSession,
) -> Result<bool, StorageError> {
    let valid: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users \
         INNER JOIN provider_accounts AS account ON account.owner_user_id = users.id \
         INNER JOIN tasks AS task ON task.provider_account_id = account.id \
         INNER JOIN question_snapshots AS snapshot ON snapshot.task_id = task.id \
         WHERE users.id = ? AND users.status = 'active' AND account.id = ? \
           AND account.provider_id = ? AND task.id = ? AND snapshot.id = ? \
           AND snapshot.provider_id = ? AND snapshot.provider_version = ?)",
    )
    .bind(session.owner_user_id.to_string())
    .bind(session.provider_account_id.to_string())
    .bind(session.provider_id.as_str())
    .bind(session.task_id.to_string())
    .bind(session.question_snapshot_id.to_string())
    .bind(session.provider_id.as_str())
    .bind(&session.provider_version)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(valid == 1)
}

async fn insert_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: (&str, Option<String>),
    action: &str,
    correlation_id: &str,
    session: &QuestionSession,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'question_session', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(session.updated_at))
    .bind(actor.0)
    .bind(actor.1)
    .bind(action)
    .bind(session.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "state": session.state,
            "revision": session.revision,
            "provider_id": session.provider_id,
            "provider_version": session.provider_version,
            "provider_account_id": session.provider_account_id,
            "task_id": session.task_id,
            "question_snapshot_id": session.question_snapshot_id,
            "execution_id": session.execution_id,
            "artifact_type": session.artifact_type,
            "artifact_digest": "[HASHED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn state_name(state: QuestionSessionState) -> &'static str {
    match state {
        QuestionSessionState::Active => "active",
        QuestionSessionState::Claimed => "claimed",
        QuestionSessionState::Consumed => "consumed",
        QuestionSessionState::Cancelled => "cancelled",
        QuestionSessionState::Expired => "expired",
    }
}

fn decode_state(value: &str) -> Result<QuestionSessionState, StorageError> {
    match value {
        "active" => Ok(QuestionSessionState::Active),
        "claimed" => Ok(QuestionSessionState::Claimed),
        "consumed" => Ok(QuestionSessionState::Consumed),
        "cancelled" => Ok(QuestionSessionState::Cancelled),
        "expired" => Ok(QuestionSessionState::Expired),
        _ => Err(StorageError::InvalidData(
            "invalid Question session state".to_owned(),
        )),
    }
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
            "Question session correlation ID is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
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
    use asterism_domain::ProviderAccountId;
    use chrono::{Duration, Utc};

    use super::*;

    #[tokio::test]
    async fn exact_draft_execution_claims_once_and_recovery_reads_same_session() {
        let fixture = Fixture::new().await;
        let session = fixture.session(fixture.now + Duration::minutes(5));
        fixture.create(&session).await;
        let execution_id = fixture.execution(session.question_snapshot_id).await;

        let claimed = fixture
            .repository
            .claim_question_session_for_execution(
                execution_id,
                fixture.now + Duration::seconds(1),
                "question-claim",
            )
            .await
            .unwrap();
        let QuestionSessionClaimOutcome::Claimed(claimed) = claimed else {
            panic!("expected a new claim");
        };
        assert_eq!(claimed.execution_id, Some(execution_id));
        assert!(matches!(
            fixture
                .repository
                .claim_question_session_for_execution(
                    execution_id,
                    fixture.now + Duration::seconds(2),
                    "question-recover",
                )
                .await
                .unwrap(),
            QuestionSessionClaimOutcome::Existing(existing) if existing == claimed
        ));
        assert_eq!(
            fixture
                .repository
                .find_question_session_for_execution(execution_id)
                .await
                .unwrap(),
            Some(claimed)
        );
    }

    #[tokio::test]
    async fn different_execution_cannot_steal_claim_and_consumed_session_stays_bound() {
        let fixture = Fixture::new().await;
        let session = fixture.session(fixture.now + Duration::minutes(5));
        fixture.create(&session).await;
        let first = fixture.execution(session.question_snapshot_id).await;
        let second = fixture.execution(session.question_snapshot_id).await;
        let QuestionSessionClaimOutcome::Claimed(mut claimed) = fixture
            .repository
            .claim_question_session_for_execution(
                first,
                fixture.now + Duration::seconds(1),
                "question-first",
            )
            .await
            .unwrap()
        else {
            panic!("expected first claim");
        };
        assert_eq!(
            fixture
                .repository
                .claim_question_session_for_execution(
                    second,
                    fixture.now + Duration::seconds(2),
                    "question-steal",
                )
                .await
                .unwrap(),
            QuestionSessionClaimOutcome::BindingConflict
        );

        let revision = claimed.revision;
        claimed.consume(fixture.now + Duration::seconds(3)).unwrap();
        assert!(
            fixture
                .repository
                .update_question_session(
                    &claimed,
                    revision,
                    AuditActor::User(fixture.owner),
                    "question-consume",
                )
                .await
                .unwrap()
        );
        assert_eq!(
            fixture
                .repository
                .find_question_session_for_execution(first)
                .await
                .unwrap()
                .unwrap()
                .state,
            QuestionSessionState::Consumed
        );
    }

    #[tokio::test]
    async fn expired_or_foreign_session_fails_closed() {
        let fixture = Fixture::new().await;
        let session = fixture.session(fixture.now + Duration::seconds(1));
        fixture.create(&session).await;
        let execution_id = fixture.execution(session.question_snapshot_id).await;
        assert!(matches!(
            fixture
                .repository
                .claim_question_session_for_execution(
                    execution_id,
                    fixture.now + Duration::seconds(1),
                    "question-expired",
                )
                .await
                .unwrap(),
            QuestionSessionClaimOutcome::Expired(expired)
                if expired.state == QuestionSessionState::Expired
        ));

        let mut foreign = fixture.session(fixture.now + Duration::minutes(5));
        foreign.owner_user_id = UserId::new();
        assert!(
            fixture
                .repository
                .create_question_session(
                    &foreign,
                    AuditActor::User(fixture.owner),
                    "question-foreign",
                )
                .await
                .is_err()
        );
    }

    struct Fixture {
        database: Database,
        repository: SqliteQuestionSessionRepository,
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        provider: ProviderId,
        snapshot: QuestionSnapshotId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let task = TaskId::new();
            let snapshot = QuestionSnapshotId::new();
            let provider = ProviderId::new("chaoxing").unwrap();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'question-session-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'Question Session', '\"authenticated\"', ?, ?)",
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
                 VALUES (?, ?, 'exam-1', 'v1:exam', 'exam', 'unknown', 'Exam', \
                         'pending', 'discovered', ?, ?, '[\"submission_execute\"]')",
            )
            .bind(task.to_string())
            .bind(account.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO question_snapshots \
                 (id, task_id, provider_id, provider_version, captured_at, question_count, total_bytes) \
                 VALUES (?, ?, ?, 'exam-v1', ?, 0, 0)",
            )
            .bind(snapshot.to_string())
            .bind(task.to_string())
            .bind(provider.as_str())
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            let repository = SqliteQuestionSessionRepository::new(database.clone());
            Self {
                database,
                repository,
                owner,
                account,
                task,
                provider,
                snapshot,
                now,
            }
        }

        fn session(&self, expires_at: Timestamp) -> QuestionSession {
            QuestionSession::active(
                self.owner,
                self.account,
                self.task,
                self.provider.clone(),
                "exam-v1".to_owned(),
                self.snapshot,
                "chaoxing.exam-attempt.v1".to_owned(),
                [7; 32],
                self.now,
                expires_at,
            )
            .unwrap()
        }

        async fn create(&self, session: &QuestionSession) {
            self.repository
                .create_question_session(session, AuditActor::User(self.owner), "question-create")
                .await
                .unwrap();
            let secret_id = asterism_domain::SecretId::new();
            let timestamp = encode_timestamp(session.created_at);
            sqlx::query(
                "INSERT INTO secret_blobs \
                 (id, owner_user_id, purpose, key_id, nonce, encrypted_data, version, \
                  created_at, updated_at) VALUES (?, ?, 'browser_job_credential', 'test-key', \
                  ?, ?, 1, ?, ?)",
            )
            .bind(secret_id.to_string())
            .bind(session.owner_user_id.to_string())
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
        }

        async fn execution(&self, snapshot_id: QuestionSnapshotId) -> ExecutionId {
            let draft_id = asterism_domain::SubmissionDraftId::new();
            let execution_id = ExecutionId::new();
            let timestamp = encode_timestamp(self.now);
            sqlx::query(
                "INSERT INTO submission_drafts \
                 (id, question_snapshot_id, task_id, provider_id, provider_version, \
                  payload_preview_json, preview_bytes, item_count, created_at) \
                 VALUES (?, ?, ?, ?, 'builder-v1', '{}', 2, 1, ?)",
            )
            .bind(draft_id.to_string())
            .bind(snapshot_id.to_string())
            .bind(self.task.to_string())
            .bind(self.provider.as_str())
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO executions \
                 (id, task_id, requested_capabilities_json, submission_draft_id, requested_by, \
                  request_source, state, started_at, created_at) \
                 VALUES (?, ?, '[\"submission_execute\"]', ?, ?, 'web_ui', 'running', ?, ?)",
            )
            .bind(execution_id.to_string())
            .bind(self.task.to_string())
            .bind(draft_id.to_string())
            .bind(self.owner.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            execution_id
        }
    }
}
