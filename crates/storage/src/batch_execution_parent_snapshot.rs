use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, BatchExecutionAttemptId, BatchExecutionId, ProviderId, SecretId, Timestamp,
    UserId,
};
use asterism_provider_api::ExecutionParentBatchSnapshot;
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    BatchExecutionParentSnapshotBindOutcome, BatchExecutionParentSnapshotBindRequest,
    BatchExecutionParentSnapshotRecord, BatchExecutionParentSnapshotRepository,
    BatchExecutionParentSnapshotResolveRequest, Database, ResolvedBatchExecutionParentSnapshot,
    SecretKeyring,
    batch_execution::assert_batch_worker_claims,
    secret::{decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob},
};

#[derive(Clone, Debug)]
pub struct SqliteBatchExecutionParentSnapshotRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteBatchExecutionParentSnapshotRepository {
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
#[allow(
    clippy::too_many_lines,
    reason = "encrypted batch-parent binding keeps claim, secret, immutable metadata and audit writes atomic"
)]
impl BatchExecutionParentSnapshotRepository for SqliteBatchExecutionParentSnapshotRepository {
    async fn bind_batch_execution_parent_snapshot(
        &self,
        request: BatchExecutionParentSnapshotBindRequest<'_>,
    ) -> Result<BatchExecutionParentSnapshotBindOutcome, SecretStoreError> {
        validate_request_context(request.worker_id, request.correlation_id, request.access)?;
        if request.snapshot.provider_id() != &self.provider_id {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
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
        let binding = fetch_parent_binding(&mut transaction, request.batch_execution_id)
            .await?
            .ok_or(SecretStoreError::VersionConflict)?;
        authorize_parent_access(
            binding.owner_user_id,
            &binding.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if binding.state != "running" {
            return Err(SecretStoreError::VersionConflict);
        }
        let candidate = record_from_snapshot(
            request.batch_execution_id,
            request.attempt_id,
            &request.snapshot,
            request.at,
        );
        if let Some(existing) =
            fetch_stored_snapshot(&mut transaction, request.batch_execution_id).await?
        {
            let identical = existing.metadata == candidate
                && existing.owner_user_id == binding.owner_user_id
                && existing.actual_provider_id == self.provider_id;
            transaction.rollback().await.map_err(storage_error)?;
            return if identical {
                Ok(BatchExecutionParentSnapshotBindOutcome::AlreadyBound(
                    existing.metadata,
                ))
            } else {
                Err(SecretStoreError::VersionConflict)
            };
        }

        let (key_id, key) = self.keyring.active();
        let authority_secret = execution_state_secret(binding.owner_user_id, key_id, request.at);
        let batch_secret = execution_state_secret(binding.owner_user_id, key_id, request.at);
        let (authority_nonce, authority_encrypted) = encrypt(
            key,
            &authority_secret,
            request.snapshot.authority().expose_secret(),
        )?;
        let (batch_nonce, batch_encrypted) =
            encrypt(key, &batch_secret, request.snapshot.batch().expose_secret())?;
        insert_secret_blob(
            &mut transaction,
            &authority_secret,
            &authority_nonce,
            &authority_encrypted,
        )
        .await?;
        insert_secret_blob(
            &mut transaction,
            &batch_secret,
            &batch_nonce,
            &batch_encrypted,
        )
        .await?;
        sqlx::query(
            "INSERT INTO batch_execution_parent_snapshots \
             (batch_execution_id, batch_execution_attempt_id, provider_id, authority_type, \
              authority_digest, authority_secret_blob_id, batch_type, batch_digest, \
              batch_secret_blob_id, bound_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(candidate.batch_execution_id.to_string())
        .bind(candidate.attempt_id.to_string())
        .bind(candidate.provider_id.as_str())
        .bind(&candidate.authority_type)
        .bind(candidate.authority_digest.to_vec())
        .bind(authority_secret.id.to_string())
        .bind(&candidate.batch_type)
        .bind(candidate.batch_digest.to_vec())
        .bind(batch_secret.id.to_string())
        .bind(encode_timestamp(candidate.bound_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "batch_execution_parent_authority_stored",
            &authority_secret,
        )
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "batch_execution_parent_snapshot_stored",
            &batch_secret,
        )
        .await
        .map_err(storage_error)?;
        insert_parent_audit(
            &mut transaction,
            &candidate,
            request.worker_id,
            request.correlation_id,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(BatchExecutionParentSnapshotBindOutcome::Bound(candidate))
    }

    async fn resolve_batch_execution_parent_snapshot(
        &self,
        request: BatchExecutionParentSnapshotResolveRequest<'_>,
    ) -> Result<Option<ResolvedBatchExecutionParentSnapshot>, SecretStoreError> {
        validate_request_context(request.worker_id, request.correlation_id, request.access)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
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
        let binding = fetch_parent_binding(&mut transaction, request.batch_execution_id)
            .await?
            .ok_or(SecretStoreError::VersionConflict)?;
        authorize_parent_access(
            binding.owner_user_id,
            &binding.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if !matches!(binding.state.as_str(), "running" | "recovering") {
            return Err(SecretStoreError::VersionConflict);
        }
        let Some(stored) =
            fetch_stored_snapshot(&mut transaction, request.batch_execution_id).await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        if stored.metadata.attempt_id != request.attempt_id
            || stored.metadata.provider_id != self.provider_id
            || stored.actual_provider_id != self.provider_id
            || stored.owner_user_id != binding.owner_user_id
            || request.at < stored.metadata.bound_at
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let (authority_ref, authority) = resolve_execution_state_secret(
            &mut transaction,
            &self.keyring,
            stored.authority_secret_id,
            stored.owner_user_id,
            stored.metadata.bound_at,
        )
        .await?;
        let (batch_ref, batch) = resolve_execution_state_secret(
            &mut transaction,
            &self.keyring,
            stored.batch_secret_id,
            stored.owner_user_id,
            stored.metadata.bound_at,
        )
        .await?;
        let snapshot = ExecutionParentBatchSnapshot::try_new(
            stored.metadata.provider_id.clone(),
            stored.metadata.authority_type.clone(),
            authority,
            stored.metadata.batch_type.clone(),
            batch,
        )
        .map_err(|_| SecretStoreError::AuthenticationFailed)?;
        if snapshot.authority_digest() != stored.metadata.authority_digest
            || snapshot.batch_digest() != stored.metadata.batch_digest
        {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "batch_execution_parent_authority_accessed",
            &authority_ref,
        )
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "batch_execution_parent_snapshot_accessed",
            &batch_ref,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedBatchExecutionParentSnapshot {
            metadata: stored.metadata,
            snapshot,
        }))
    }
}

struct ParentBinding {
    owner_user_id: UserId,
    provider_id: ProviderId,
    state: String,
}

async fn fetch_parent_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Option<ParentBinding>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT account.owner_user_id, account.provider_id, batch.state \
         FROM batch_executions AS batch \
         INNER JOIN provider_accounts AS account ON account.id = batch.provider_account_id \
         WHERE batch.id = ?",
    )
    .bind(batch_execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.as_ref().map(decode_parent_binding).transpose()
}

fn decode_parent_binding(row: &SqliteRow) -> Result<ParentBinding, SecretStoreError> {
    Ok(ParentBinding {
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        provider_id: ProviderId::new(
            row.try_get::<String, _>("provider_id")
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        state: row.try_get("state").map_err(storage_error)?,
    })
}

struct StoredSnapshot {
    metadata: BatchExecutionParentSnapshotRecord,
    authority_secret_id: SecretId,
    batch_secret_id: SecretId,
    owner_user_id: UserId,
    actual_provider_id: ProviderId,
}

async fn fetch_stored_snapshot(
    transaction: &mut Transaction<'_, Sqlite>,
    batch_execution_id: BatchExecutionId,
) -> Result<Option<StoredSnapshot>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT snapshot.batch_execution_id, snapshot.batch_execution_attempt_id, \
                snapshot.provider_id AS snapshot_provider_id, snapshot.authority_type, \
                snapshot.authority_digest, snapshot.authority_secret_blob_id, \
                snapshot.batch_type, snapshot.batch_digest, snapshot.batch_secret_blob_id, \
                snapshot.bound_at, account.owner_user_id, \
                account.provider_id AS actual_provider_id \
         FROM batch_execution_parent_snapshots AS snapshot \
         INNER JOIN batch_executions AS batch ON batch.id = snapshot.batch_execution_id \
         INNER JOIN provider_accounts AS account ON account.id = batch.provider_account_id \
         WHERE snapshot.batch_execution_id = ?",
    )
    .bind(batch_execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.as_ref().map(decode_stored_snapshot).transpose()
}

fn decode_stored_snapshot(row: &SqliteRow) -> Result<StoredSnapshot, SecretStoreError> {
    let provider_id = ProviderId::new(
        row.try_get::<String, _>("snapshot_provider_id")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    let actual_provider_id = ProviderId::new(
        row.try_get::<String, _>("actual_provider_id")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    if provider_id != actual_provider_id {
        return Err(SecretStoreError::Storage);
    }
    Ok(StoredSnapshot {
        metadata: BatchExecutionParentSnapshotRecord {
            batch_execution_id: parse_id(
                row.try_get("batch_execution_id").map_err(storage_error)?,
            )?,
            attempt_id: parse_id(
                row.try_get("batch_execution_attempt_id")
                    .map_err(storage_error)?,
            )?,
            provider_id,
            authority_type: row.try_get("authority_type").map_err(storage_error)?,
            authority_digest: decode_digest(
                row.try_get("authority_digest").map_err(storage_error)?,
            )?,
            batch_type: row.try_get("batch_type").map_err(storage_error)?,
            batch_digest: decode_digest(row.try_get("batch_digest").map_err(storage_error)?)?,
            bound_at: decode_timestamp(row.try_get("bound_at").map_err(storage_error)?)?,
        },
        authority_secret_id: parse_id(
            row.try_get("authority_secret_blob_id")
                .map_err(storage_error)?,
        )?,
        batch_secret_id: parse_id(row.try_get("batch_secret_blob_id").map_err(storage_error)?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        actual_provider_id,
    })
}

fn record_from_snapshot(
    batch_execution_id: BatchExecutionId,
    attempt_id: BatchExecutionAttemptId,
    snapshot: &ExecutionParentBatchSnapshot,
    bound_at: Timestamp,
) -> BatchExecutionParentSnapshotRecord {
    BatchExecutionParentSnapshotRecord {
        batch_execution_id,
        attempt_id,
        provider_id: snapshot.provider_id().clone(),
        authority_type: snapshot.authority_type().to_owned(),
        authority_digest: snapshot.authority_digest(),
        batch_type: snapshot.batch_type().to_owned(),
        batch_digest: snapshot.batch_digest(),
        bound_at,
    }
}

fn execution_state_secret(owner_user_id: UserId, key_id: &str, at: Timestamp) -> SecretRef {
    SecretRef {
        id: SecretId::new(),
        owner_user_id,
        purpose: SecretPurpose::ProviderExecutionState,
        version: 1,
        key_id: key_id.to_owned(),
        created_at: at,
        updated_at: at,
    }
}

async fn resolve_execution_state_secret(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    secret_id: SecretId,
    owner_user_id: UserId,
    bound_at: Timestamp,
) -> Result<(SecretRef, SecretValue), SecretStoreError> {
    let stored = fetch_secret(transaction, secret_id).await?;
    let secret = SecretRef {
        id: secret_id,
        owner_user_id: stored.owner_user_id,
        purpose: stored.purpose,
        version: stored.version,
        key_id: stored.key_id.clone(),
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    };
    if secret.owner_user_id != owner_user_id
        || secret.purpose != SecretPurpose::ProviderExecutionState
        || secret.version != 1
        || secret.created_at != bound_at
        || secret.updated_at != bound_at
    {
        return Err(SecretStoreError::VersionConflict);
    }
    let key = keyring.get(&secret.key_id)?;
    let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
    Ok((secret, SecretValue::new(plaintext)))
}

async fn insert_parent_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    record: &BatchExecutionParentSnapshotRecord,
    worker_id: &str,
    correlation_id: &str,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'worker', ?, 'batch_execution_parent_snapshot_bound', \
                 'batch_execution', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(record.bound_at))
    .bind(worker_id)
    .bind(record.batch_execution_id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "attempt_id": record.attempt_id,
            "provider_id": record.provider_id,
            "authority_type": record.authority_type,
            "authority_digest": "[HASHED]",
            "batch_type": record.batch_type,
            "batch_digest": "[HASHED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn validate_request_context(
    worker_id: &str,
    correlation_id: &str,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    if valid(worker_id) && valid(correlation_id) && access.correlation_id == correlation_id {
        Ok(())
    } else {
        Err(SecretStoreError::InvalidValue)
    }
}

fn authorize_parent_access(
    owner_user_id: UserId,
    actual_provider_id: &ProviderId,
    scoped_provider_id: &ProviderId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    if actual_provider_id != scoped_provider_id || !access.authorizes(owner_user_id) {
        return Err(SecretStoreError::Unauthorized);
    }
    match &access.actor {
        SecretActor::CoreService(_) => Ok(()),
        SecretActor::ProviderRuntime(provider_id) if provider_id == scoped_provider_id.as_str() => {
            Ok(())
        }
        SecretActor::User(_) | SecretActor::ServiceToken(_) | SecretActor::ProviderRuntime(_) => {
            Err(SecretStoreError::Unauthorized)
        }
    }
}

fn decode_digest(bytes: Vec<u8>) -> Result<[u8; 32], SecretStoreError> {
    bytes.try_into().map_err(|_| SecretStoreError::Storage)
}

fn parse_id<T>(value: &str) -> Result<T, SecretStoreError>
where
    T: FromStr,
{
    T::from_str(value).map_err(|_| SecretStoreError::Storage)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, SecretStoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| SecretStoreError::Storage)
}

fn storage_error(_error: sqlx::Error) -> SecretStoreError {
    SecretStoreError::Storage
}
