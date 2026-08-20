use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, ExecutionAttemptId, ExecutionId, ProviderId, SecretId, Timestamp, UserId,
};
use asterism_provider_api::ExecutionMutationStageOutput;
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, ExecutionMutationReceiptWithStageOutputOutcome,
    ExecutionMutationReceiptWithStageOutputRequest, ExecutionMutationStageOutputRecord,
    ExecutionMutationStageOutputRepository, ExecutionMutationStageOutputResolveRequest,
    SecretKeyring,
    execution::assert_worker_claims,
    secret::{decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob},
};

#[derive(Clone, Debug)]
pub struct SqliteExecutionMutationStageOutputRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteExecutionMutationStageOutputRepository {
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

#[async_trait]
impl ExecutionMutationStageOutputRepository for SqliteExecutionMutationStageOutputRepository {
    #[allow(
        clippy::too_many_lines,
        reason = "the receipt, encrypted successor, audits and idempotency checks form one indivisible transaction"
    )]
    async fn record_execution_mutation_receipt_with_stage_output(
        &self,
        request: ExecutionMutationReceiptWithStageOutputRequest<'_>,
    ) -> Result<ExecutionMutationReceiptWithStageOutputOutcome, SecretStoreError> {
        validate_bind_request(&request, &self.provider_id)?;
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
        let binding = fetch_execution_binding(
            &mut transaction,
            request.execution_id,
            request.attempt_id,
            request.ordinal,
        )
        .await?
        .ok_or(SecretStoreError::VersionConflict)?;
        authorize(
            binding.owner_user_id,
            &binding.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if binding.scheduler_job_id != request.scheduler_job_id
            || binding.worker_id != request.worker_id
            || binding.attempt_finished_at.is_some()
            || binding.issue_ordinal != request.ordinal
            || request.at < binding.issued_at
            || binding
                .received_at
                .is_some_and(|received_at| request.at < received_at)
            || binding.received_at.is_some()
                && (binding.response_digest != Some(request.response_digest)
                    || binding.accepted != Some(true))
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let candidate = ExecutionMutationStageOutputRecord {
            execution_id: request.execution_id,
            attempt_id: request.attempt_id,
            ordinal: request.ordinal,
            provider_id: self.provider_id.clone(),
            output_type: request.stage_output.output_type().to_owned(),
            output_digest: request.stage_output.output_digest(),
            stored_at: request.at,
        };
        if let Some(existing) = fetch_stage_output_record(
            &mut transaction,
            request.execution_id,
            request.attempt_id,
            request.ordinal,
        )
        .await?
        {
            let identical = existing.record.execution_id == candidate.execution_id
                && existing.record.attempt_id == candidate.attempt_id
                && existing.record.ordinal == candidate.ordinal
                && existing.record.provider_id == candidate.provider_id
                && existing.record.output_type == candidate.output_type
                && existing.record.output_digest == candidate.output_digest
                && existing.owner_user_id == binding.owner_user_id
                && existing.actual_provider_id == self.provider_id;
            transaction.rollback().await.map_err(storage_error)?;
            return if identical {
                Ok(
                    ExecutionMutationReceiptWithStageOutputOutcome::AlreadyRecorded(
                        existing.record,
                    ),
                )
            } else {
                Err(SecretStoreError::VersionConflict)
            };
        }
        if binding.received_at.is_some() {
            return Err(SecretStoreError::VersionConflict);
        }

        let changed = sqlx::query(
            "UPDATE execution_atomic_mutations \
             SET response_digest = ?, accepted = 1, received_at = ? \
             WHERE execution_id = ? AND execution_attempt_id = ? AND ordinal = ? \
               AND response_digest IS NULL AND accepted IS NULL \
               AND retry_not_before IS NULL AND received_at IS NULL",
        )
        .bind(request.response_digest.as_slice())
        .bind(encode_timestamp(request.at))
        .bind(request.execution_id.to_string())
        .bind(request.attempt_id.to_string())
        .bind(i64::from(request.ordinal))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id: binding.owner_user_id,
            purpose: SecretPurpose::ProviderExecutionState,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.at,
            updated_at: request.at,
        };
        let (nonce, encrypted) =
            encrypt(key, &secret, request.stage_output.value().expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted).await?;
        sqlx::query(
            "INSERT INTO execution_mutation_stage_outputs \
             (execution_id, execution_attempt_id, ordinal, provider_id, output_type, \
              output_digest, secret_blob_id, stored_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(candidate.execution_id.to_string())
        .bind(candidate.attempt_id.to_string())
        .bind(i64::from(candidate.ordinal))
        .bind(candidate.provider_id.as_str())
        .bind(&candidate.output_type)
        .bind(candidate.output_digest.to_vec())
        .bind(secret.id.to_string())
        .bind(encode_timestamp(candidate.stored_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "execution_mutation_stage_output_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_stage_output_audit(
            &mut transaction,
            &candidate,
            request.worker_id,
            request.correlation_id,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(ExecutionMutationReceiptWithStageOutputOutcome::Recorded(
            candidate,
        ))
    }

    async fn resolve_execution_mutation_stage_outputs(
        &self,
        request: ExecutionMutationStageOutputResolveRequest<'_>,
    ) -> Result<Vec<ExecutionMutationStageOutput>, SecretStoreError> {
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
        let binding =
            fetch_attempt_binding(&mut transaction, request.execution_id, request.attempt_id)
                .await?
                .ok_or(SecretStoreError::VersionConflict)?;
        authorize(
            binding.owner_user_id,
            &binding.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if binding.attempt_finished_at.is_some() {
            return Err(SecretStoreError::VersionConflict);
        }
        let stored =
            fetch_all_stage_outputs(&mut transaction, request.execution_id, request.attempt_id)
                .await?;
        let mut outputs = Vec::with_capacity(stored.len());
        for stored in stored {
            if stored.record.provider_id != self.provider_id
                || stored.actual_provider_id != self.provider_id
                || stored.owner_user_id != binding.owner_user_id
                || stored.record.stored_at > request.at
                || stored.response_digest.is_none()
                || stored.accepted != Some(true)
                || stored.received_at != Some(stored.record.stored_at)
            {
                return Err(SecretStoreError::VersionConflict);
            }
            let secret = fetch_secret(&mut transaction, stored.secret_id).await?;
            let secret_ref = SecretRef {
                id: stored.secret_id,
                owner_user_id: secret.owner_user_id,
                purpose: secret.purpose,
                version: secret.version,
                key_id: secret.key_id.clone(),
                created_at: secret.created_at,
                updated_at: secret.updated_at,
            };
            if secret_ref.owner_user_id != binding.owner_user_id
                || secret_ref.purpose != SecretPurpose::ProviderExecutionState
                || secret_ref.version != 1
                || secret_ref.created_at != stored.record.stored_at
                || secret_ref.updated_at != stored.record.stored_at
            {
                return Err(SecretStoreError::VersionConflict);
            }
            let plaintext = decrypt(
                self.keyring.get(&secret_ref.key_id)?,
                &secret_ref,
                &secret.nonce,
                &secret.encrypted_data,
            )?;
            let output = ExecutionMutationStageOutput::try_new(
                self.provider_id.clone(),
                stored.record.ordinal,
                stored.record.output_type,
                stored.record.output_digest,
                SecretValue::new(plaintext),
            )
            .map_err(|_| SecretStoreError::AuthenticationFailed)?;
            insert_secret_audit(
                &mut transaction,
                request.access,
                "execution_mutation_stage_output_accessed",
                &secret_ref,
            )
            .await
            .map_err(storage_error)?;
            outputs.push(output);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(outputs)
    }
}

struct ExecutionBinding {
    owner_user_id: UserId,
    provider_id: ProviderId,
    scheduler_job_id: asterism_domain::ScheduleId,
    worker_id: String,
    issue_ordinal: u32,
    issued_at: Timestamp,
    response_digest: Option<[u8; 32]>,
    accepted: Option<bool>,
    received_at: Option<Timestamp>,
    attempt_finished_at: Option<Timestamp>,
}

struct AttemptBinding {
    owner_user_id: UserId,
    provider_id: ProviderId,
    attempt_finished_at: Option<Timestamp>,
}

struct StoredStageOutput {
    record: ExecutionMutationStageOutputRecord,
    secret_id: SecretId,
    owner_user_id: UserId,
    actual_provider_id: ProviderId,
    response_digest: Option<[u8; 32]>,
    accepted: Option<bool>,
    received_at: Option<Timestamp>,
}

async fn fetch_execution_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    attempt_id: ExecutionAttemptId,
    ordinal: u32,
) -> Result<Option<ExecutionBinding>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT account.owner_user_id, account.provider_id, mutation.scheduler_job_id, \
                mutation.worker_id, mutation.ordinal, mutation.issued_at, \
                mutation.response_digest, mutation.accepted, mutation.received_at, \
                attempt.finished_at AS attempt_finished_at \
         FROM execution_atomic_mutations AS mutation \
         INNER JOIN execution_attempts AS attempt \
            ON attempt.id = mutation.execution_attempt_id \
           AND attempt.execution_id = mutation.execution_id \
         INNER JOIN executions AS execution ON execution.id = mutation.execution_id \
         INNER JOIN tasks AS task ON task.id = execution.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE mutation.execution_id = ? AND mutation.execution_attempt_id = ? \
           AND mutation.ordinal = ?",
    )
    .bind(execution_id.to_string())
    .bind(attempt_id.to_string())
    .bind(i64::from(ordinal))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.as_ref().map(decode_execution_binding).transpose()
}

async fn fetch_attempt_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    attempt_id: ExecutionAttemptId,
) -> Result<Option<AttemptBinding>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT account.owner_user_id, account.provider_id, \
                attempt.finished_at AS attempt_finished_at \
         FROM execution_attempts AS attempt \
         INNER JOIN executions AS execution ON execution.id = attempt.execution_id \
         INNER JOIN tasks AS task ON task.id = execution.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE attempt.execution_id = ? AND attempt.id = ?",
    )
    .bind(execution_id.to_string())
    .bind(attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.as_ref().map(decode_attempt_binding).transpose()
}

async fn fetch_stage_output_record(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    attempt_id: ExecutionAttemptId,
    ordinal: u32,
) -> Result<Option<StoredStageOutput>, SecretStoreError> {
    let rows =
        fetch_stage_outputs_query(transaction, execution_id, attempt_id, Some(ordinal)).await?;
    Ok(rows.into_iter().next())
}

async fn fetch_all_stage_outputs(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    attempt_id: ExecutionAttemptId,
) -> Result<Vec<StoredStageOutput>, SecretStoreError> {
    fetch_stage_outputs_query(transaction, execution_id, attempt_id, None).await
}

async fn fetch_stage_outputs_query(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    attempt_id: ExecutionAttemptId,
    ordinal: Option<u32>,
) -> Result<Vec<StoredStageOutput>, SecretStoreError> {
    let rows = sqlx::query(
        "SELECT output.execution_id, output.execution_attempt_id, output.ordinal, \
                output.provider_id AS output_provider_id, output.output_type, \
                output.output_digest, output.secret_blob_id, output.stored_at, \
                account.owner_user_id, account.provider_id AS actual_provider_id, \
                mutation.response_digest, mutation.accepted, mutation.received_at \
         FROM execution_mutation_stage_outputs AS output \
         INNER JOIN execution_atomic_mutations AS mutation \
            ON mutation.execution_id = output.execution_id \
           AND mutation.execution_attempt_id = output.execution_attempt_id \
           AND mutation.ordinal = output.ordinal \
         INNER JOIN executions AS execution ON execution.id = output.execution_id \
         INNER JOIN tasks AS task ON task.id = execution.task_id \
         INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
         WHERE output.execution_id = ? AND output.execution_attempt_id = ? \
           AND (? IS NULL OR output.ordinal = ?) ORDER BY output.ordinal",
    )
    .bind(execution_id.to_string())
    .bind(attempt_id.to_string())
    .bind(ordinal.map(i64::from))
    .bind(ordinal.map(i64::from))
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    rows.iter().map(decode_stored_stage_output).collect()
}

fn decode_execution_binding(row: &SqliteRow) -> Result<ExecutionBinding, SecretStoreError> {
    Ok(ExecutionBinding {
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        provider_id: parse_provider(row.try_get("provider_id").map_err(storage_error)?)?,
        scheduler_job_id: parse_id(row.try_get("scheduler_job_id").map_err(storage_error)?)?,
        worker_id: row.try_get("worker_id").map_err(storage_error)?,
        issue_ordinal: u32::try_from(row.try_get::<i64, _>("ordinal").map_err(storage_error)?)
            .map_err(|_| SecretStoreError::Storage)?,
        issued_at: decode_timestamp(row.try_get("issued_at").map_err(storage_error)?)?,
        response_digest: decode_optional_digest(
            row.try_get("response_digest").map_err(storage_error)?,
        )?,
        accepted: row.try_get("accepted").map_err(storage_error)?,
        received_at: row
            .try_get::<Option<&str>, _>("received_at")
            .map_err(storage_error)?
            .map(decode_timestamp)
            .transpose()?,
        attempt_finished_at: row
            .try_get::<Option<&str>, _>("attempt_finished_at")
            .map_err(storage_error)?
            .map(decode_timestamp)
            .transpose()?,
    })
}

fn decode_attempt_binding(row: &SqliteRow) -> Result<AttemptBinding, SecretStoreError> {
    Ok(AttemptBinding {
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        provider_id: parse_provider(row.try_get("provider_id").map_err(storage_error)?)?,
        attempt_finished_at: row
            .try_get::<Option<&str>, _>("attempt_finished_at")
            .map_err(storage_error)?
            .map(decode_timestamp)
            .transpose()?,
    })
}

fn decode_stored_stage_output(row: &SqliteRow) -> Result<StoredStageOutput, SecretStoreError> {
    Ok(StoredStageOutput {
        record: ExecutionMutationStageOutputRecord {
            execution_id: parse_id(row.try_get("execution_id").map_err(storage_error)?)?,
            attempt_id: parse_id(row.try_get("execution_attempt_id").map_err(storage_error)?)?,
            ordinal: u32::try_from(row.try_get::<i64, _>("ordinal").map_err(storage_error)?)
                .map_err(|_| SecretStoreError::Storage)?,
            provider_id: parse_provider(row.try_get("output_provider_id").map_err(storage_error)?)?,
            output_type: row.try_get("output_type").map_err(storage_error)?,
            output_digest: decode_digest(row.try_get("output_digest").map_err(storage_error)?)?,
            stored_at: decode_timestamp(row.try_get("stored_at").map_err(storage_error)?)?,
        },
        secret_id: parse_id(row.try_get("secret_blob_id").map_err(storage_error)?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        actual_provider_id: parse_provider(
            row.try_get("actual_provider_id").map_err(storage_error)?,
        )?,
        response_digest: decode_optional_digest(
            row.try_get("response_digest").map_err(storage_error)?,
        )?,
        accepted: row.try_get("accepted").map_err(storage_error)?,
        received_at: row
            .try_get::<Option<&str>, _>("received_at")
            .map_err(storage_error)?
            .map(decode_timestamp)
            .transpose()?,
    })
}

fn validate_bind_request(
    request: &ExecutionMutationReceiptWithStageOutputRequest<'_>,
    provider_id: &ProviderId,
) -> Result<(), SecretStoreError> {
    if request.stage_output.provider_id() != provider_id
        || request.stage_output.ordinal() != request.ordinal
        || request.response_digest == [0; 32]
        || !valid_token(request.worker_id)
        || !valid_token(request.correlation_id)
        || request.access.correlation_id != request.correlation_id
    {
        Err(SecretStoreError::InvalidValue)
    } else {
        Ok(())
    }
}

fn validate_resolve_request(
    request: &ExecutionMutationStageOutputResolveRequest<'_>,
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

fn authorize(
    owner_user_id: UserId,
    actual_provider_id: &ProviderId,
    provider_id: &ProviderId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    if actual_provider_id != provider_id || !access.authorizes(owner_user_id) {
        return Err(SecretStoreError::Unauthorized);
    }
    match &access.actor {
        SecretActor::CoreService(_) => Ok(()),
        SecretActor::ProviderRuntime(actual) if actual == provider_id.as_str() => Ok(()),
        _ => Err(SecretStoreError::Unauthorized),
    }
}

async fn insert_stage_output_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    record: &ExecutionMutationStageOutputRecord,
    worker_id: &str,
    correlation_id: &str,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'execution_mutation_receipt_stage_output_recorded', \
                 'execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(record.stored_at))
    .bind(worker_id)
    .bind(record.execution_id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": record.attempt_id,
            "ordinal": record.ordinal,
            "provider_id": record.provider_id,
            "output_type": record.output_type,
            "output_digest": "[HASHED]",
            "response_digest": "[HASHED]",
            "accepted": true,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn parse_id<T: FromStr>(value: &str) -> Result<T, SecretStoreError> {
    T::from_str(value).map_err(|_| SecretStoreError::Storage)
}

fn parse_provider(value: String) -> Result<ProviderId, SecretStoreError> {
    ProviderId::new(value).map_err(|_| SecretStoreError::Storage)
}

fn decode_digest(value: Vec<u8>) -> Result<[u8; 32], SecretStoreError> {
    value.try_into().map_err(|_| SecretStoreError::Storage)
}

fn decode_optional_digest(value: Option<Vec<u8>>) -> Result<Option<[u8; 32]>, SecretStoreError> {
    value.map(decode_digest).transpose()
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
    use std::{collections::BTreeMap, sync::Arc};

    use asterism_domain::{
        ExecutionAttemptId, ExecutionId, ProviderAccountId, ProviderId, ScheduleId, TaskId,
        Timestamp, UserId,
    };
    use asterism_provider_api::ExecutionMutationStageOutput;
    use asterism_scheduler::ScheduledJobKind;
    use asterism_secrets::{SecretAccess, SecretActor, SecretKey, SecretStoreError, SecretValue};
    use chrono::{Duration, TimeZone, Utc};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        ExecutionMutationReceiptWithStageOutputOutcome,
        ExecutionMutationReceiptWithStageOutputRequest, ExecutionMutationStageOutputResolveRequest,
    };

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one claimed-attempt scenario keeps encryption, idempotency, rollback and tamper evidence together"
    )]
    async fn receipt_and_encrypted_stage_output_are_atomic_idempotent_and_authenticated() {
        let fixture = fixture().await;
        let repository = SqliteExecutionMutationStageOutputRepository::new(
            fixture.database.clone(),
            Arc::new(
                SecretKeyring::new(
                    "stage-key",
                    BTreeMap::from([("stage-key".to_owned(), SecretKey::new([7; 32]))]),
                )
                .unwrap(),
            ),
            fixture.provider_id.clone(),
        );
        issue_mutation(&fixture, 1).await;
        let plaintext = br#"{"upload_token":"provider-private-token","object_key":"bound-object"}"#;
        let output_digest: [u8; 32] = Sha256::digest(plaintext).into();
        let response_digest = [9; 32];
        let recorded_at = fixture.now + Duration::seconds(2);
        let record_access = core_access("stage-record");

        let outcome = repository
            .record_execution_mutation_receipt_with_stage_output(record_request(
                &fixture,
                1,
                response_digest,
                recorded_at,
                &record_access,
                stage_output(&fixture.provider_id, 1, plaintext, output_digest),
            ))
            .await
            .unwrap();
        let ExecutionMutationReceiptWithStageOutputOutcome::Recorded(record) = outcome else {
            panic!("first atomic stage-output write repeated");
        };
        assert_eq!(record.output_digest, output_digest);
        let (stored_response, accepted, received_at): (
            Option<Vec<u8>>,
            Option<i64>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT response_digest, accepted, received_at FROM execution_atomic_mutations \
                 WHERE execution_id = ? AND execution_attempt_id = ? AND ordinal = 1",
        )
        .bind(fixture.execution_id.to_string())
        .bind(fixture.attempt_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(stored_response, Some(response_digest.to_vec()));
        assert_eq!(accepted, Some(1));
        assert_eq!(
            received_at.as_deref(),
            Some(encode_timestamp(recorded_at).as_str())
        );
        let encrypted: Vec<u8> = sqlx::query_scalar(
            "SELECT secret.encrypted_data FROM secret_blobs AS secret \
             INNER JOIN execution_mutation_stage_outputs AS output \
                ON output.secret_blob_id = secret.id \
             WHERE output.execution_id = ? AND output.execution_attempt_id = ? \
               AND output.ordinal = 1",
        )
        .bind(fixture.execution_id.to_string())
        .bind(fixture.attempt_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert!(
            !encrypted
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );

        let repeated = repository
            .record_execution_mutation_receipt_with_stage_output(record_request(
                &fixture,
                1,
                response_digest,
                recorded_at + Duration::seconds(1),
                &record_access,
                stage_output(&fixture.provider_id, 1, plaintext, output_digest),
            ))
            .await
            .unwrap();
        assert!(matches!(
            repeated,
            ExecutionMutationReceiptWithStageOutputOutcome::AlreadyRecorded(existing)
                if existing.output_digest == output_digest
        ));

        let resolve_access = core_access("stage-resolve");
        let resolved = repository
            .resolve_execution_mutation_stage_outputs(ExecutionMutationStageOutputResolveRequest {
                execution_id: fixture.execution_id,
                attempt_id: fixture.attempt_id,
                scheduler_job_id: fixture.job_id,
                worker_id: fixture.worker_id,
                correlation_id: "stage-resolve",
                at: recorded_at + Duration::seconds(2),
                access: &resolve_access,
            })
            .await
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].ordinal(), 1);
        assert_eq!(resolved[0].output_type(), "uai.upload.object.v1");
        assert_eq!(resolved[0].output_digest(), output_digest);
        assert_eq!(resolved[0].value().expose_secret(), plaintext);

        issue_mutation(&fixture, 2).await;
        sqlx::query(
            "CREATE TRIGGER reject_second_stage_output \
             BEFORE INSERT ON execution_mutation_stage_outputs \
             WHEN NEW.ordinal = 2 \
             BEGIN SELECT RAISE(ABORT, 'forced stage-output failure'); END",
        )
        .execute(fixture.database.pool())
        .await
        .unwrap();
        let failed = repository
            .record_execution_mutation_receipt_with_stage_output(record_request(
                &fixture,
                2,
                [10; 32],
                recorded_at + Duration::seconds(3),
                &record_access,
                stage_output(
                    &fixture.provider_id,
                    2,
                    b"rollback-secret",
                    Sha256::digest(b"rollback-secret").into(),
                ),
            ))
            .await;
        assert!(matches!(failed, Err(SecretStoreError::Storage)));
        let (response_digest, accepted, received_at): (
            Option<Vec<u8>>,
            Option<i64>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT response_digest, accepted, received_at FROM execution_atomic_mutations \
                 WHERE execution_id = ? AND execution_attempt_id = ? AND ordinal = 2",
        )
        .bind(fixture.execution_id.to_string())
        .bind(fixture.attempt_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!((response_digest, accepted, received_at), (None, None, None));
        let stage_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM execution_mutation_stage_outputs WHERE execution_id = ?",
        )
        .bind(fixture.execution_id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret_blobs")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(stage_count, 1);
        assert_eq!(secret_count, 1);

        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = ? WHERE id = (\
                SELECT secret_blob_id FROM execution_mutation_stage_outputs \
                WHERE execution_id = ? AND execution_attempt_id = ? AND ordinal = 1\
             )",
        )
        .bind(vec![0_u8; 64])
        .bind(fixture.execution_id.to_string())
        .bind(fixture.attempt_id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        let tamper_access = core_access("stage-tamper");
        assert!(matches!(
            repository
                .resolve_execution_mutation_stage_outputs(
                    ExecutionMutationStageOutputResolveRequest {
                        execution_id: fixture.execution_id,
                        attempt_id: fixture.attempt_id,
                        scheduler_job_id: fixture.job_id,
                        worker_id: fixture.worker_id,
                        correlation_id: "stage-tamper",
                        at: recorded_at + Duration::seconds(4),
                        access: &tamper_access,
                    },
                )
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    struct Fixture {
        database: Database,
        provider_id: ProviderId,
        execution_id: ExecutionId,
        attempt_id: ExecutionAttemptId,
        job_id: ScheduleId,
        worker_id: &'static str,
        now: Timestamp,
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture exposes every owner, claim, attempt and mutation binding required by the repository boundary"
    )]
    async fn fixture() -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 20, 1, 2, 3).unwrap();
        let owner_id = UserId::new();
        let account_id = ProviderAccountId::new();
        let task_id = TaskId::new();
        let execution_id = ExecutionId::new();
        let attempt_id = ExecutionAttemptId::new();
        let job_id = ScheduleId::new();
        let provider_id = ProviderId::new("uai").unwrap();
        let worker_id = "stage-worker";
        let encoded_now = encode_timestamp(now);
        let lease_expires_at = encode_timestamp(now + Duration::minutes(10));

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
             VALUES (?, ?, ?, 'UAI', '{\"state\":\"idle\"}', ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner_id.to_string())
        .bind(provider_id.as_str())
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'uai-task', 'uai-task-fingerprint', 'resource', 'routine', \
                     'UAI upload', 'pending', 'running', ?, ?, '[\"resource_execution\"]')",
        )
        .bind(task_id.to_string())
        .bind(account_id.to_string())
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO executions \
             (id, task_id, requested_by, request_source, state, started_at, created_at) \
             VALUES (?, ?, ?, 'web_ui', 'running', ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(owner_id.to_string())
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_attempts (id, execution_id, attempt_no, started_at) \
             VALUES (?, ?, 1, ?)",
        )
        .bind(attempt_id.to_string())
        .bind(execution_id.to_string())
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        let payload = serde_json::to_string(&ScheduledJobKind::Execution { execution_id }).unwrap();
        sqlx::query(
            "INSERT INTO scheduled_jobs \
             (id, job_kind, payload_json, run_at, state, attempts, idempotency_key, worker_id, \
              lease_expires_at, created_at, updated_at) \
             VALUES (?, 'execution', ?, ?, 'claimed', 1, ?, ?, ?, ?, ?)",
        )
        .bind(job_id.to_string())
        .bind(payload)
        .bind(&encoded_now)
        .bind(format!("execution:{execution_id}"))
        .bind(worker_id)
        .bind(&lease_expires_at)
        .bind(&encoded_now)
        .bind(&encoded_now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_leases (task_id, execution_id, worker_id, expires_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(task_id.to_string())
        .bind(execution_id.to_string())
        .bind(worker_id)
        .bind(&lease_expires_at)
        .execute(database.pool())
        .await
        .unwrap();

        Fixture {
            database,
            provider_id,
            execution_id,
            attempt_id,
            job_id,
            worker_id,
            now,
        }
    }

    async fn issue_mutation(fixture: &Fixture, ordinal: u32) {
        sqlx::query(
            "INSERT INTO execution_atomic_mutations \
             (execution_id, execution_attempt_id, ordinal, scheduler_job_id, worker_id, \
              operation_type, request_digest, issued_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(fixture.execution_id.to_string())
        .bind(fixture.attempt_id.to_string())
        .bind(i64::from(ordinal))
        .bind(fixture.job_id.to_string())
        .bind(fixture.worker_id)
        .bind(format!("uai.upload.stage-{ordinal}"))
        .bind(vec![u8::try_from(ordinal).unwrap(); 32])
        .bind(encode_timestamp(fixture.now + Duration::seconds(1)))
        .execute(fixture.database.pool())
        .await
        .unwrap();
    }

    fn record_request<'a>(
        fixture: &'a Fixture,
        ordinal: u32,
        response_digest: [u8; 32],
        at: Timestamp,
        access: &'a SecretAccess,
        stage_output: ExecutionMutationStageOutput,
    ) -> ExecutionMutationReceiptWithStageOutputRequest<'a> {
        ExecutionMutationReceiptWithStageOutputRequest {
            execution_id: fixture.execution_id,
            attempt_id: fixture.attempt_id,
            ordinal,
            scheduler_job_id: fixture.job_id,
            worker_id: fixture.worker_id,
            response_digest,
            correlation_id: "stage-record",
            at,
            access,
            stage_output,
        }
    }

    fn stage_output(
        provider_id: &ProviderId,
        ordinal: u32,
        plaintext: &[u8],
        output_digest: [u8; 32],
    ) -> ExecutionMutationStageOutput {
        ExecutionMutationStageOutput::try_new(
            provider_id.clone(),
            ordinal,
            "uai.upload.object.v1",
            output_digest,
            SecretValue::new(plaintext.to_vec()),
        )
        .unwrap()
    }

    fn core_access(correlation_id: &str) -> SecretAccess {
        SecretAccess {
            actor: SecretActor::CoreService("execution-worker"),
            correlation_id: correlation_id.to_owned(),
            reason: "persist or restore an accepted Provider-private stage output".to_owned(),
        }
    }
}
