use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, CourseId, ExecutionInvocationDraft, ExecutionInvocationDraftId,
    ProviderAccountId, ProviderId, SecretId, SubmissionDraftId, TaskCapability, TaskId, Timestamp,
    UserId,
};
use asterism_provider_api::{
    MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES, ProviderExecutionPlanArtifact,
    ProviderExecutionPrivateInput,
};
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, ExecutionInvocationDraftCreateOutcome, ExecutionInvocationDraftCreateRequest,
    ExecutionInvocationDraftRecord, ExecutionInvocationDraftRepository,
    ExecutionInvocationDraftResolveRequest, ResolvedExecutionInvocationDraft, SecretKeyring,
    StorageError,
    execution::assert_worker_claims,
    secret::{decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob},
};

#[derive(Clone, Debug)]
pub struct SqliteExecutionInvocationDraftRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteExecutionInvocationDraftRepository {
    pub const fn new(
        database: Database,
        keyring: Arc<SecretKeyring>,
        provider_id: ProviderId,
    ) -> Self {
        Self {
            database,
            keyring,
            provider_id,
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Provider-private draft transactions keep their authority and cryptographic checks visible"
)]
#[async_trait]
impl ExecutionInvocationDraftRepository for SqliteExecutionInvocationDraftRepository {
    #[allow(
        clippy::too_many_lines,
        reason = "draft authority, encryption, idempotency and audit are one transaction"
    )]
    async fn create_execution_invocation_draft(
        &self,
        request: ExecutionInvocationDraftCreateRequest<'_>,
    ) -> Result<ExecutionInvocationDraftCreateOutcome, SecretStoreError> {
        validate_create_request(&request, &self.provider_id)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        authorize_task(
            &mut transaction,
            request.owner_user_id,
            request.provider_account_id,
            request.course_id,
            request.task_id,
            &self.provider_id,
            request.submission_draft_id,
            request.access,
        )
        .await?;

        let secret_id = SecretId::new();
        let candidate = record_from_request(&request, secret_id)?;
        if let Some(existing) = fetch_record(&mut transaction, request.draft_id).await? {
            transaction.rollback().await.map_err(storage_error)?;
            return if records_match(&existing, &candidate) {
                Ok(ExecutionInvocationDraftCreateOutcome::AlreadyExists(
                    existing,
                ))
            } else {
                Err(SecretStoreError::VersionConflict)
            };
        }
        if let Some(existing) = fetch_record_by_idempotency(
            &mut transaction,
            request.idempotency_scope,
            request.idempotency_key,
        )
        .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return if invocation_semantics_match(&existing, &candidate) {
                Ok(ExecutionInvocationDraftCreateOutcome::AlreadyExists(
                    existing,
                ))
            } else {
                Err(SecretStoreError::VersionConflict)
            };
        }

        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: secret_id,
            owner_user_id: request.owner_user_id,
            purpose: SecretPurpose::ProviderExecutionState,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.created_at,
            updated_at: request.created_at,
        };
        let (nonce, encrypted) =
            encrypt(key, &secret, request.private_input.value().expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted).await?;
        sqlx::query(
            "INSERT INTO execution_invocation_drafts \
             (id, owner_user_id, provider_account_id, course_id, task_id, provider_id, \
              provider_version, requested_capabilities_json, submission_draft_id, \
              private_input_type, private_input_digest, private_input_secret_blob_id, \
              plan_artifact_type, plan_artifact_digest, plan_artifact_payload_json, \
              idempotency_scope, idempotency_key, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(candidate.draft.id.to_string())
        .bind(candidate.draft.owner_user_id.to_string())
        .bind(candidate.draft.provider_account_id.to_string())
        .bind(candidate.draft.course_id.map(|id| id.to_string()))
        .bind(candidate.draft.task_id.to_string())
        .bind(candidate.draft.provider_id.as_str())
        .bind(&candidate.draft.provider_version)
        .bind(encode_capabilities(
            &candidate.draft.requested_capabilities,
        )?)
        .bind(candidate.draft.submission_draft_id.map(|id| id.to_string()))
        .bind(&candidate.draft.private_input_type)
        .bind(candidate.draft.private_input_digest.to_vec())
        .bind(secret.id.to_string())
        .bind(candidate.provider_plan_artifact.artifact_type())
        .bind(candidate.provider_plan_artifact.artifact_digest().to_vec())
        .bind(
            serde_json::to_string(candidate.provider_plan_artifact.payload_sanitized())
                .map_err(|_| SecretStoreError::Storage)?,
        )
        .bind(request.idempotency_scope)
        .bind(request.idempotency_key)
        .bind(encode_timestamp(candidate.draft.created_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "execution_invocation_private_input_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_draft_audit(
            &mut transaction,
            &candidate,
            request.correlation_id,
            "execution_invocation_draft_created",
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(ExecutionInvocationDraftCreateOutcome::Created(candidate))
    }

    async fn find_owned_execution_invocation_draft(
        &self,
        owner_user_id: UserId,
        draft_id: ExecutionInvocationDraftId,
    ) -> Result<Option<ExecutionInvocationDraftRecord>, StorageError> {
        let row = sqlx::query(
            "SELECT * FROM execution_invocation_drafts \
             WHERE id = ? AND owner_user_id = ? AND provider_id = ?",
        )
        .bind(draft_id.to_string())
        .bind(owner_user_id.to_string())
        .bind(self.provider_id.as_str())
        .fetch_optional(self.database.pool())
        .await?;
        row.map(|row| {
            decode_record(&row).map_err(|_| {
                StorageError::InvalidData("execution invocation draft is inconsistent".to_owned())
            })
        })
        .transpose()
    }

    async fn resolve_execution_invocation_draft(
        &self,
        request: ExecutionInvocationDraftResolveRequest<'_>,
    ) -> Result<Option<ResolvedExecutionInvocationDraft>, SecretStoreError> {
        validate_resolve_request(&request)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        assert_worker_claims(
            &mut transaction,
            request.execution_id,
            request.scheduler_job_id,
            request.worker_id,
            request.at,
            true,
        )
        .await
        .map_err(|_| SecretStoreError::VersionConflict)?;
        let row = sqlx::query(
            "SELECT draft.* FROM execution_invocation_drafts AS draft \
             INNER JOIN executions AS execution ON execution.id = draft.claimed_execution_id \
             INNER JOIN execution_attempts AS attempt ON attempt.execution_id = execution.id \
             INNER JOIN tasks AS task ON task.id = execution.task_id \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE execution.id = ? AND attempt.id = ? AND attempt.finished_at IS NULL \
               AND execution.requested_by = draft.owner_user_id \
               AND execution.task_id = draft.task_id \
               AND task.provider_account_id = draft.provider_account_id \
               AND account.owner_user_id = draft.owner_user_id \
               AND account.provider_id = draft.provider_id",
        )
        .bind(request.execution_id.to_string())
        .bind(request.attempt_id.to_string())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let Some(row) = row else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        let record = decode_record(&row)?;
        authorize(
            record.draft.owner_user_id,
            &self.provider_id,
            request.access,
        )?;
        if record.draft.provider_id != self.provider_id
            || record.claimed_execution_id != Some(request.execution_id)
            || record.claimed_at.is_none()
            || record.claimed_at.is_some_and(|at| at > request.at)
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let stored = fetch_secret(&mut transaction, record.draft.private_input_secret_id).await?;
        if stored.owner_user_id != record.draft.owner_user_id
            || stored.purpose != SecretPurpose::ProviderExecutionState
            || stored.version != 1
            || stored.created_at != record.draft.created_at
            || stored.updated_at != record.draft.created_at
            || stored.encrypted_data.len() > MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES + 64
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let secret = SecretRef {
            id: record.draft.private_input_secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: stored.version,
            key_id: stored.key_id.clone(),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
        };
        let plaintext = decrypt(
            self.keyring.get(&stored.key_id)?,
            &secret,
            &stored.nonce,
            &stored.encrypted_data,
        )?;
        let private_input = ProviderExecutionPrivateInput::try_new(
            record.draft.provider_id.clone(),
            record.draft.private_input_type.clone(),
            SecretValue::new(plaintext),
        )
        .map_err(|_| SecretStoreError::AuthenticationFailed)?;
        if private_input.input_digest() != record.draft.private_input_digest {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "execution_invocation_private_input_accessed",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_draft_audit(
            &mut transaction,
            &record,
            request.correlation_id,
            "execution_invocation_draft_resolved",
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedExecutionInvocationDraft {
            record,
            private_input,
        }))
    }
}

fn validate_create_request(
    request: &ExecutionInvocationDraftCreateRequest<'_>,
    provider_id: &ProviderId,
) -> Result<(), SecretStoreError> {
    if request.private_input.provider_id() != provider_id
        || request.provider_plan_artifact.provider_id() != provider_id
        || request.private_input.value().expose_secret().is_empty()
        || request.private_input.value().expose_secret().len()
            > MAX_PROVIDER_EXECUTION_PRIVATE_INPUT_BYTES
        || !valid_token(request.correlation_id)
        || !valid_token(request.idempotency_scope)
        || !valid_token(request.idempotency_key)
        || request.access.correlation_id != request.correlation_id
    {
        Err(SecretStoreError::InvalidValue)
    } else {
        Ok(())
    }
}

fn validate_resolve_request(
    request: &ExecutionInvocationDraftResolveRequest<'_>,
) -> Result<(), SecretStoreError> {
    if valid_token(request.worker_id)
        && valid_token(request.correlation_id)
        && request.access.correlation_id == request.correlation_id
    {
        Ok(())
    } else {
        Err(SecretStoreError::InvalidValue)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "authorization deliberately verifies every immutable draft binding at the storage boundary"
)]
async fn authorize_task(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    course_id: Option<CourseId>,
    task_id: TaskId,
    provider_id: &ProviderId,
    submission_draft_id: Option<SubmissionDraftId>,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    authorize(owner_user_id, provider_id, access)?;
    let row = sqlx::query(
        "SELECT task.provider_account_id, task.course_id, account.owner_user_id, \
                account.provider_id \
         FROM tasks AS task \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE task.id = ?",
    )
    .bind(task_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(SecretStoreError::NotFound)?;
    if row
        .try_get::<String, _>("provider_account_id")
        .map_err(storage_error)?
        != provider_account_id.to_string()
        || row
            .try_get::<Option<String>, _>("course_id")
            .map_err(storage_error)?
            != course_id.map(|id| id.to_string())
        || row
            .try_get::<String, _>("owner_user_id")
            .map_err(storage_error)?
            != owner_user_id.to_string()
        || row
            .try_get::<String, _>("provider_id")
            .map_err(storage_error)?
            != provider_id.as_str()
    {
        return Err(SecretStoreError::Unauthorized);
    }
    if let Some(submission_draft_id) = submission_draft_id {
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM submission_drafts \
             WHERE id = ? AND task_id = ? AND provider_id = ?)",
        )
        .bind(submission_draft_id.to_string())
        .bind(task_id.to_string())
        .bind(provider_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(storage_error)?;
        if !valid {
            return Err(SecretStoreError::VersionConflict);
        }
    }
    Ok(())
}

fn authorize(
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
        _ => Err(SecretStoreError::Unauthorized),
    }
}

fn record_from_request(
    request: &ExecutionInvocationDraftCreateRequest<'_>,
    secret_id: SecretId,
) -> Result<ExecutionInvocationDraftRecord, SecretStoreError> {
    let draft = ExecutionInvocationDraft {
        id: request.draft_id,
        owner_user_id: request.owner_user_id,
        provider_account_id: request.provider_account_id,
        course_id: request.course_id,
        task_id: request.task_id,
        provider_id: request.private_input.provider_id().clone(),
        provider_version: request.provider_version.to_owned(),
        requested_capabilities: request.requested_capabilities.to_vec(),
        submission_draft_id: request.submission_draft_id,
        private_input_type: request.private_input.input_type().to_owned(),
        private_input_digest: request.private_input.input_digest(),
        private_input_secret_id: secret_id,
        plan_artifact_digest: request.provider_plan_artifact.artifact_digest(),
        created_at: request.created_at,
    };
    draft
        .validate()
        .map_err(|_| SecretStoreError::InvalidValue)?;
    Ok(ExecutionInvocationDraftRecord {
        draft,
        provider_plan_artifact: request.provider_plan_artifact.clone(),
        claimed_execution_id: None,
        claimed_at: None,
    })
}

fn records_match(
    left: &ExecutionInvocationDraftRecord,
    right: &ExecutionInvocationDraftRecord,
) -> bool {
    left.draft.id == right.draft.id
        && left.draft.created_at == right.draft.created_at
        && left.claimed_execution_id.is_none()
        && left.claimed_at.is_none()
        && invocation_semantics_match(left, right)
}

fn invocation_semantics_match(
    left: &ExecutionInvocationDraftRecord,
    right: &ExecutionInvocationDraftRecord,
) -> bool {
    left.draft.owner_user_id == right.draft.owner_user_id
        && left.draft.provider_account_id == right.draft.provider_account_id
        && left.draft.course_id == right.draft.course_id
        && left.draft.task_id == right.draft.task_id
        && left.draft.provider_id == right.draft.provider_id
        && left.draft.provider_version == right.draft.provider_version
        && left.draft.requested_capabilities == right.draft.requested_capabilities
        && left.draft.submission_draft_id == right.draft.submission_draft_id
        && left.draft.private_input_type == right.draft.private_input_type
        && left.draft.private_input_digest == right.draft.private_input_digest
        && left.draft.plan_artifact_digest == right.draft.plan_artifact_digest
        && left.provider_plan_artifact == right.provider_plan_artifact
}

async fn fetch_record(
    transaction: &mut Transaction<'_, Sqlite>,
    draft_id: ExecutionInvocationDraftId,
) -> Result<Option<ExecutionInvocationDraftRecord>, SecretStoreError> {
    let row = sqlx::query("SELECT * FROM execution_invocation_drafts WHERE id = ?")
        .bind(draft_id.to_string())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?;
    row.map(|row| decode_record(&row)).transpose()
}

async fn fetch_record_by_idempotency(
    transaction: &mut Transaction<'_, Sqlite>,
    scope: &str,
    key: &str,
) -> Result<Option<ExecutionInvocationDraftRecord>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT * FROM execution_invocation_drafts \
         WHERE idempotency_scope = ? AND idempotency_key = ?",
    )
    .bind(scope)
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.map(|row| decode_record(&row)).transpose()
}

fn decode_record(row: &SqliteRow) -> Result<ExecutionInvocationDraftRecord, SecretStoreError> {
    let provider_id = ProviderId::new(
        row.try_get::<String, _>("provider_id")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    let payload: serde_json::Value = serde_json::from_str(
        &row.try_get::<String, _>("plan_artifact_payload_json")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    let provider_plan_artifact = ProviderExecutionPlanArtifact::try_new(
        provider_id.clone(),
        row.try_get::<String, _>("plan_artifact_type")
            .map_err(storage_error)?,
        payload,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    let stored_plan_digest =
        decode_digest(row.try_get("plan_artifact_digest").map_err(storage_error)?)?;
    if provider_plan_artifact.artifact_digest() != stored_plan_digest {
        return Err(SecretStoreError::AuthenticationFailed);
    }
    let draft = ExecutionInvocationDraft {
        id: parse_id(row.try_get("id").map_err(storage_error)?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        provider_account_id: parse_id(row.try_get("provider_account_id").map_err(storage_error)?)?,
        course_id: row
            .try_get::<Option<&str>, _>("course_id")
            .map_err(storage_error)?
            .map(parse_id)
            .transpose()?,
        task_id: parse_id(row.try_get("task_id").map_err(storage_error)?)?,
        provider_id,
        provider_version: row.try_get("provider_version").map_err(storage_error)?,
        requested_capabilities: decode_capabilities(
            row.try_get("requested_capabilities_json")
                .map_err(storage_error)?,
        )?,
        submission_draft_id: row
            .try_get::<Option<&str>, _>("submission_draft_id")
            .map_err(storage_error)?
            .map(parse_id)
            .transpose()?,
        private_input_type: row.try_get("private_input_type").map_err(storage_error)?,
        private_input_digest: decode_digest(
            row.try_get("private_input_digest").map_err(storage_error)?,
        )?,
        private_input_secret_id: parse_id(
            row.try_get("private_input_secret_blob_id")
                .map_err(storage_error)?,
        )?,
        plan_artifact_digest: stored_plan_digest,
        created_at: decode_timestamp(row.try_get("created_at").map_err(storage_error)?)?,
    };
    draft.validate().map_err(|_| SecretStoreError::Storage)?;
    let claimed_execution_id = row
        .try_get::<Option<&str>, _>("claimed_execution_id")
        .map_err(storage_error)?
        .map(parse_id)
        .transpose()?;
    let claimed_at = row
        .try_get::<Option<&str>, _>("claimed_at")
        .map_err(storage_error)?
        .map(decode_timestamp)
        .transpose()?;
    if claimed_execution_id.is_some() != claimed_at.is_some()
        || claimed_at.is_some_and(|at| at < draft.created_at)
    {
        return Err(SecretStoreError::Storage);
    }
    Ok(ExecutionInvocationDraftRecord {
        draft,
        provider_plan_artifact,
        claimed_execution_id,
        claimed_at,
    })
}

async fn insert_draft_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    record: &ExecutionInvocationDraftRecord,
    correlation_id: &str,
    action: &str,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'service', 'core', ?, 'execution_invocation_draft', ?, ?, \
                 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(
        record.claimed_at.unwrap_or(record.draft.created_at),
    ))
    .bind(action)
    .bind(record.draft.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "provider_id": record.draft.provider_id,
            "provider_account_id": record.draft.provider_account_id,
            "task_id": record.draft.task_id,
            "private_input_type": record.draft.private_input_type,
            "private_input_digest": "[HASHED]",
            "plan_artifact_digest": "[HASHED]",
            "claimed_execution_id": record.claimed_execution_id,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn encode_capabilities(value: &[TaskCapability]) -> Result<String, SecretStoreError> {
    serde_json::to_string(value).map_err(|_| SecretStoreError::Storage)
}

fn decode_capabilities(value: &str) -> Result<Vec<TaskCapability>, SecretStoreError> {
    serde_json::from_str(value).map_err(|_| SecretStoreError::Storage)
}

fn parse_id<T: FromStr>(value: &str) -> Result<T, SecretStoreError> {
    T::from_str(value).map_err(|_| SecretStoreError::Storage)
}

fn decode_digest(value: Vec<u8>) -> Result<[u8; 32], SecretStoreError> {
    value.try_into().map_err(|_| SecretStoreError::Storage)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, SecretStoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| SecretStoreError::Storage)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn storage_error(_error: sqlx::Error) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_domain::{
        AuditActor, AuthState, Execution, ExecutionId, ExecutionInvocationDraftId, ExecutionState,
        OrchestrationState, RequestSource, ScheduleId,
    };
    use asterism_provider_api::{ProviderRuntimeSettingsSchema, ResolvedProviderRuntimeSettings};
    use asterism_secrets::{SecretKey, SecretValue};
    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::{
        ExecutionAttemptStartRequest, ExecutionRepository, ExecutionRuntimeSettingsResolution,
        ExecutionRuntimeSettingsSnapshot, ExecutionScheduleOutcome, ExecutionScheduleRequest,
        SqliteExecutionRepository,
    };

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario proves encryption, idempotency, atomic claim and live-Attempt resolution"
    )]
    async fn private_input_is_encrypted_claimed_once_and_attempt_bound() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 20, 2, 3, 4).unwrap();
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let provider_id = ProviderId::new("uai").unwrap();
        let encoded_now = encode_timestamp(now);
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner_id.to_string())
        .bind(format!("user-{owner_id}"))
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'UAI', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner_id.to_string())
        .bind(provider_id.as_str())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'uai-discussion', 'fingerprint', 'discussion', 'routine', \
                     'UAI discussion', 'pending', 'ready', ?, ?, '[\"discussion\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        let keyring = Arc::new(
            SecretKeyring::new(
                "invocation-key",
                BTreeMap::from([("invocation-key".to_owned(), SecretKey::new([41; 32]))]),
            )
            .unwrap(),
        );
        let repository = SqliteExecutionInvocationDraftRepository::new(
            database.clone(),
            keyring,
            provider_id.clone(),
        );
        let access = SecretAccess {
            actor: SecretActor::CoreService("invocation-test"),
            correlation_id: "invocation-create".to_owned(),
            reason: "test encrypted invocation".to_owned(),
        };
        let plaintext = b"PRIVATE_DISCUSSION_REPLY";
        let private_input = ProviderExecutionPrivateInput::try_new(
            provider_id.clone(),
            "uai.discussion.reply-state.v1",
            SecretValue::new(plaintext.to_vec()),
        )
        .unwrap();
        let plan = ProviderExecutionPlanArtifact::try_new(
            provider_id.clone(),
            "uai.discussion.reply-plan.v1",
            serde_json::json!({"topic_digest": vec![7_u8; 32]}),
        )
        .unwrap();
        let requested_capabilities = [TaskCapability::Discussion];
        let draft_id = ExecutionInvocationDraftId::new();
        let create = |candidate_id| ExecutionInvocationDraftCreateRequest {
            draft_id: candidate_id,
            owner_user_id: owner_id,
            provider_account_id: account_id,
            course_id: None,
            task_id,
            provider_version: "0.1.0",
            requested_capabilities: &requested_capabilities,
            submission_draft_id: None,
            provider_plan_artifact: &plan,
            private_input: &private_input,
            created_at: now,
            idempotency_scope: "user:test",
            idempotency_key: "discussion-one",
            correlation_id: &access.correlation_id,
            access: &access,
        };
        assert!(matches!(
            repository
                .create_execution_invocation_draft(create(draft_id))
                .await
                .unwrap(),
            ExecutionInvocationDraftCreateOutcome::Created(_)
        ));
        assert!(matches!(
            repository
                .create_execution_invocation_draft(create(draft_id))
                .await
                .unwrap(),
            ExecutionInvocationDraftCreateOutcome::AlreadyExists(_)
        ));
        let retry = repository
            .create_execution_invocation_draft(create(ExecutionInvocationDraftId::new()))
            .await
            .unwrap();
        let ExecutionInvocationDraftCreateOutcome::AlreadyExists(retry) = retry else {
            panic!("idempotent draft preparation created a second record");
        };
        assert_eq!(retry.draft.id, draft_id);
        let encrypted: Vec<u8> =
            sqlx::query_scalar("SELECT encrypted_data FROM secret_blobs WHERE id = ?")
                .bind(retry.draft.private_input_secret_id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(
            !encrypted
                .windows(plaintext.len())
                .any(|value| value == plaintext)
        );

        let schema = ProviderRuntimeSettingsSchema {
            version: 1,
            definitions: Vec::new(),
        };
        let resolved = ResolvedProviderRuntimeSettings {
            schema_version: 1,
            values: BTreeMap::new(),
        };
        let snapshot = ExecutionRuntimeSettingsSnapshot {
            provider_id: provider_id.clone(),
            completion_policy: schema.completion_policy_snapshot(&resolved, now).unwrap(),
            resolved,
            sources: BTreeMap::new(),
            provider_revision: None,
            provider_account_revision: None,
            task_revision: None,
            captured_at: now,
        };
        let execution = Execution {
            id: ExecutionId::new(),
            task_id,
            requested_capabilities: vec![TaskCapability::Discussion],
            submission_draft_id: None,
            requested_by: Some(owner_id),
            request_source: RequestSource::WebUi,
            quote_id: None,
            state: ExecutionState::Scheduled,
            scheduled_at: Some(now),
            started_at: None,
            finished_at: None,
            created_at: now,
        };
        let executions = SqliteExecutionRepository::new(database.clone());
        assert_eq!(
            executions
                .schedule_execution(ExecutionScheduleRequest {
                    execution: &execution,
                    capability_plan: &execution.requested_capabilities,
                    capability_call_starts: &[1],
                    provider_plan_artifact: Some(&plan),
                    invocation_draft_id: Some(draft_id),
                    billing: None,
                    runtime_settings: Some(ExecutionRuntimeSettingsResolution {
                        snapshot: &snapshot,
                        schema: &schema,
                    }),
                    strict_completion_retry: None,
                    expected_task_state: OrchestrationState::Ready,
                    idempotency_scope: "user:test",
                    idempotency_key: "execution-one",
                    actor: AuditActor::User(owner_id),
                    correlation_id: "execution-schedule",
                })
                .await
                .unwrap(),
            ExecutionScheduleOutcome::Created(execution.clone())
        );
        assert_eq!(
            executions
                .find_execution_invocation_draft_id(execution.id)
                .await
                .unwrap(),
            Some(draft_id)
        );
        let job_id = ScheduleId::from_str(
            &sqlx::query_scalar::<_, String>(
                "SELECT id FROM scheduled_jobs WHERE idempotency_key = ?",
            )
            .bind(format!("execution:{}", execution.id))
            .fetch_one(database.pool())
            .await
            .unwrap(),
        )
        .unwrap();
        let worker_id = "invocation-worker";
        let expires_at = now + Duration::minutes(5);
        sqlx::query(
            "UPDATE scheduled_jobs SET state = 'claimed', worker_id = ?, lease_expires_at = ? \
             WHERE id = ?",
        )
        .bind(worker_id)
        .bind(encode_timestamp(expires_at))
        .bind(job_id.to_string())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_leases (task_id, execution_id, worker_id, expires_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(task_id.to_string())
        .bind(execution.id.to_string())
        .bind(worker_id)
        .bind(encode_timestamp(expires_at))
        .execute(database.pool())
        .await
        .unwrap();
        let attempt = executions
            .start_attempt(ExecutionAttemptStartRequest {
                execution_id: execution.id,
                scheduler_job_id: job_id,
                worker_id,
                at: now,
                correlation_id: "attempt-start",
            })
            .await
            .unwrap();
        let resolve_access = SecretAccess {
            actor: SecretActor::CoreService("invocation-worker"),
            correlation_id: "invocation-resolve".to_owned(),
            reason: "resolve claimed private input".to_owned(),
        };
        let resolved = repository
            .resolve_execution_invocation_draft(ExecutionInvocationDraftResolveRequest {
                execution_id: execution.id,
                attempt_id: attempt.id,
                scheduler_job_id: job_id,
                worker_id,
                correlation_id: &resolve_access.correlation_id,
                at: now,
                access: &resolve_access,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.private_input.value().expose_secret(), plaintext);
        assert_eq!(resolved.record.claimed_execution_id, Some(execution.id));
    }
}
