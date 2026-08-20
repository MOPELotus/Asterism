use std::str::FromStr;

use asterism_domain::{ProviderId, SecretId, SubmissionReceipt, Timestamp, UserId};
use asterism_provider_api::ProviderQuestionOperationArtifact;
use asterism_secrets::{SecretAccess, SecretPurpose, SecretRef, SecretStoreError, SecretValue};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    QuestionOperationAcceptedResult, SecretKeyring,
    secret::{
        decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob, validate_secret,
    },
};

const MAX_RECEIPT_BYTES: usize = 64 * 1_024;

#[derive(Clone, Copy)]
pub(crate) enum QuestionOperationArtifactScope {
    Read,
    Session,
}

impl QuestionOperationArtifactScope {
    const fn key_column(self) -> &'static str {
        match self {
            Self::Read => "attempt_id",
            Self::Session => "session_id",
        }
    }

    const fn recovery_table(self) -> &'static str {
        match self {
            Self::Read => "question_read_operation_recovery_artifacts",
            Self::Session => "question_session_operation_recovery_artifacts",
        }
    }

    const fn result_table(self) -> &'static str {
        match self {
            Self::Read => "question_read_operation_results",
            Self::Session => "question_session_operation_results",
        }
    }

    const fn recovery_stored_event(self) -> &'static str {
        match self {
            Self::Read => "question_read_operation_recovery_artifact_stored",
            Self::Session => "question_session_operation_recovery_artifact_stored",
        }
    }

    const fn recovery_accessed_event(self) -> &'static str {
        match self {
            Self::Read => "question_read_operation_recovery_artifact_accessed",
            Self::Session => "question_session_operation_recovery_artifact_accessed",
        }
    }

    const fn result_stored_event(self) -> &'static str {
        match self {
            Self::Read => "question_read_operation_result_artifact_stored",
            Self::Session => "question_session_operation_result_artifact_stored",
        }
    }

    const fn result_accessed_event(self) -> &'static str {
        match self {
            Self::Read => "question_read_operation_result_artifact_accessed",
            Self::Session => "question_session_operation_result_artifact_accessed",
        }
    }
}

pub(crate) struct QuestionOperationArtifactBinding<'a> {
    pub scope: QuestionOperationArtifactScope,
    pub scope_id: &'a str,
    pub sequence: u64,
    pub owner_user_id: UserId,
    pub provider_id: &'a ProviderId,
}

pub(crate) async fn insert_recovery_artifact(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    binding: &QuestionOperationArtifactBinding<'_>,
    artifact: Option<&ProviderQuestionOperationArtifact>,
    stored_at: Timestamp,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    let Some(artifact) = artifact else {
        return Ok(());
    };
    validate_artifact(binding.provider_id, artifact)?;
    let secret = insert_artifact_secret(
        transaction,
        keyring,
        binding.owner_user_id,
        artifact,
        stored_at,
    )
    .await?;
    let query = format!(
        "INSERT INTO {} ({}, operation_sequence, provider_id, artifact_type, \
         artifact_digest, secret_blob_id, stored_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        binding.scope.recovery_table(),
        binding.scope.key_column(),
    );
    sqlx::query(&query)
        .bind(binding.scope_id)
        .bind(sequence_i64(binding.sequence)?)
        .bind(binding.provider_id.as_str())
        .bind(artifact.artifact_type())
        .bind(artifact.artifact_digest().as_slice())
        .bind(secret.id.to_string())
        .bind(encode_timestamp(stored_at))
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    insert_secret_audit(
        transaction,
        access,
        binding.scope.recovery_stored_event(),
        &secret,
    )
    .await
    .map_err(storage_error)?;
    Ok(())
}

pub(crate) async fn recovery_artifact_matches(
    transaction: &mut Transaction<'_, Sqlite>,
    binding: &QuestionOperationArtifactBinding<'_>,
    artifact: Option<&ProviderQuestionOperationArtifact>,
) -> Result<bool, SecretStoreError> {
    let query = format!(
        "SELECT provider_id, artifact_type, artifact_digest FROM {} \
         WHERE {} = ? AND operation_sequence = ?",
        binding.scope.recovery_table(),
        binding.scope.key_column(),
    );
    let row = sqlx::query(&query)
        .bind(binding.scope_id)
        .bind(sequence_i64(binding.sequence)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(match (row, artifact) {
        (None, None) => true,
        (Some(row), Some(artifact)) => {
            validate_artifact(binding.provider_id, artifact)?;
            row.try_get::<&str, _>("provider_id")
                .map_err(storage_error)?
                == artifact.provider_id().as_str()
                && row
                    .try_get::<&str, _>("artifact_type")
                    .map_err(storage_error)?
                    == artifact.artifact_type()
                && decode_digest(&row, "artifact_digest")? == artifact.artifact_digest()
        }
        (None, Some(_)) | (Some(_), None) => false,
    })
}

pub(crate) async fn insert_accepted_result(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    binding: &QuestionOperationArtifactBinding<'_>,
    receipt: &SubmissionReceipt,
    artifact: Option<&ProviderQuestionOperationArtifact>,
    recorded_at: Timestamp,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    receipt
        .validate()
        .map_err(|_| SecretStoreError::InvalidValue)?;
    if receipt.received_at != recorded_at {
        return Err(SecretStoreError::InvalidValue);
    }
    let receipt_json =
        serde_json::to_string(receipt).map_err(|_| SecretStoreError::InvalidValue)?;
    if receipt_json.is_empty() || receipt_json.len() > MAX_RECEIPT_BYTES {
        return Err(SecretStoreError::InvalidValue);
    }
    let secret = if let Some(artifact) = artifact {
        validate_artifact(binding.provider_id, artifact)?;
        Some(
            insert_artifact_secret(
                transaction,
                keyring,
                binding.owner_user_id,
                artifact,
                recorded_at,
            )
            .await?,
        )
    } else {
        None
    };
    let query = format!(
        "INSERT INTO {} ({}, operation_sequence, provider_id, receipt_json, receipt_bytes, \
         artifact_type, artifact_digest, secret_blob_id, recorded_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        binding.scope.result_table(),
        binding.scope.key_column(),
    );
    sqlx::query(&query)
        .bind(binding.scope_id)
        .bind(sequence_i64(binding.sequence)?)
        .bind(binding.provider_id.as_str())
        .bind(&receipt_json)
        .bind(i64::try_from(receipt_json.len()).map_err(|_| SecretStoreError::InvalidValue)?)
        .bind(artifact.map(ProviderQuestionOperationArtifact::artifact_type))
        .bind(artifact.map(|value| value.artifact_digest().to_vec()))
        .bind(secret.as_ref().map(|value| value.id.to_string()))
        .bind(encode_timestamp(recorded_at))
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    if let Some(secret) = secret {
        insert_secret_audit(
            transaction,
            access,
            binding.scope.result_stored_event(),
            &secret,
        )
        .await
        .map_err(storage_error)?;
    }
    Ok(())
}

pub(crate) async fn accepted_result_matches(
    transaction: &mut Transaction<'_, Sqlite>,
    binding: &QuestionOperationArtifactBinding<'_>,
    receipt: Option<&SubmissionReceipt>,
    artifact: Option<&ProviderQuestionOperationArtifact>,
) -> Result<bool, SecretStoreError> {
    let Some(receipt) = receipt else {
        return Ok(false);
    };
    let query = format!(
        "SELECT provider_id, receipt_json, receipt_bytes, artifact_type, artifact_digest, \
         secret_blob_id FROM {} WHERE {} = ? AND operation_sequence = ?",
        binding.scope.result_table(),
        binding.scope.key_column(),
    );
    let row = sqlx::query(&query)
        .bind(binding.scope_id)
        .bind(sequence_i64(binding.sequence)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let receipt_json: &str = row.try_get("receipt_json").map_err(storage_error)?;
    let receipt_bytes: i64 = row.try_get("receipt_bytes").map_err(storage_error)?;
    let stored_receipt: SubmissionReceipt =
        serde_json::from_str(receipt_json).map_err(|_| SecretStoreError::Storage)?;
    if row
        .try_get::<&str, _>("provider_id")
        .map_err(storage_error)?
        != binding.provider_id.as_str()
        || receipt_bytes
            != i64::try_from(receipt_json.len()).map_err(|_| SecretStoreError::Storage)?
        || stored_receipt != *receipt
    {
        return Ok(false);
    }
    artifact_metadata_matches(&row, binding.provider_id, artifact)
}

pub(crate) async fn resolve_operation_artifacts(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    binding: &QuestionOperationArtifactBinding<'_>,
    access: &SecretAccess,
) -> Result<
    (
        Option<ProviderQuestionOperationArtifact>,
        Option<QuestionOperationAcceptedResult>,
    ),
    SecretStoreError,
> {
    let recovery_query = format!(
        "SELECT provider_id, artifact_type, artifact_digest, secret_blob_id, stored_at \
         FROM {} WHERE {} = ? AND operation_sequence = ?",
        binding.scope.recovery_table(),
        binding.scope.key_column(),
    );
    let recovery = if let Some(row) = sqlx::query(&recovery_query)
        .bind(binding.scope_id)
        .bind(sequence_i64(binding.sequence)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
    {
        Some(
            resolve_artifact(
                transaction,
                keyring,
                binding,
                &row,
                "stored_at",
                access,
                binding.scope.recovery_accessed_event(),
            )
            .await?,
        )
    } else {
        None
    };

    let result_query = format!(
        "SELECT provider_id, receipt_json, receipt_bytes, artifact_type, artifact_digest, \
         secret_blob_id, recorded_at FROM {} WHERE {} = ? AND operation_sequence = ?",
        binding.scope.result_table(),
        binding.scope.key_column(),
    );
    let accepted_result = if let Some(row) = sqlx::query(&result_query)
        .bind(binding.scope_id)
        .bind(sequence_i64(binding.sequence)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
    {
        let receipt_json: &str = row.try_get("receipt_json").map_err(storage_error)?;
        let receipt_bytes: i64 = row.try_get("receipt_bytes").map_err(storage_error)?;
        if receipt_bytes
            != i64::try_from(receipt_json.len()).map_err(|_| SecretStoreError::Storage)?
            || receipt_bytes <= 0
            || receipt_bytes > i64::try_from(MAX_RECEIPT_BYTES).expect("constant fits i64")
        {
            return Err(SecretStoreError::Storage);
        }
        let receipt: SubmissionReceipt =
            serde_json::from_str(receipt_json).map_err(|_| SecretStoreError::Storage)?;
        receipt.validate().map_err(|_| SecretStoreError::Storage)?;
        let recorded_at = decode_timestamp(row.try_get("recorded_at").map_err(storage_error)?)?;
        if receipt.received_at != recorded_at
            || row
                .try_get::<&str, _>("provider_id")
                .map_err(storage_error)?
                != binding.provider_id.as_str()
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let artifact = if row
            .try_get::<Option<&str>, _>("secret_blob_id")
            .map_err(storage_error)?
            .is_some()
        {
            Some(
                resolve_artifact(
                    transaction,
                    keyring,
                    binding,
                    &row,
                    "recorded_at",
                    access,
                    binding.scope.result_accessed_event(),
                )
                .await?,
            )
        } else if row
            .try_get::<Option<&str>, _>("artifact_type")
            .map_err(storage_error)?
            .is_none()
            && row
                .try_get::<Option<Vec<u8>>, _>("artifact_digest")
                .map_err(storage_error)?
                .is_none()
        {
            None
        } else {
            return Err(SecretStoreError::Storage);
        };
        Some(QuestionOperationAcceptedResult { receipt, artifact })
    } else {
        None
    };
    Ok((recovery, accepted_result))
}

async fn insert_artifact_secret(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    owner_user_id: UserId,
    artifact: &ProviderQuestionOperationArtifact,
    stored_at: Timestamp,
) -> Result<SecretRef, SecretStoreError> {
    validate_secret(artifact.value())?;
    let (key_id, key) = keyring.active();
    let secret = SecretRef {
        id: SecretId::new(),
        owner_user_id,
        purpose: SecretPurpose::ProviderExecutionState,
        version: 1,
        key_id: key_id.to_owned(),
        created_at: stored_at,
        updated_at: stored_at,
    };
    let (nonce, encrypted_data) = encrypt(key, &secret, artifact.value().expose_secret())?;
    insert_secret_blob(transaction, &secret, &nonce, &encrypted_data).await?;
    Ok(secret)
}

async fn resolve_artifact(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    binding: &QuestionOperationArtifactBinding<'_>,
    row: &SqliteRow,
    stored_at_column: &str,
    access: &SecretAccess,
    audit_event: &str,
) -> Result<ProviderQuestionOperationArtifact, SecretStoreError> {
    let provider_id: &str = row.try_get("provider_id").map_err(storage_error)?;
    if provider_id != binding.provider_id.as_str() {
        return Err(SecretStoreError::VersionConflict);
    }
    let artifact_type: &str = row.try_get("artifact_type").map_err(storage_error)?;
    let expected_digest = decode_digest(row, "artifact_digest")?;
    let secret_id = SecretId::from_str(
        row.try_get::<&str, _>("secret_blob_id")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    let stored_at = decode_timestamp(row.try_get(stored_at_column).map_err(storage_error)?)?;
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
    if secret.owner_user_id != binding.owner_user_id {
        return Err(SecretStoreError::Unauthorized);
    }
    if secret.purpose != SecretPurpose::ProviderExecutionState {
        return Err(SecretStoreError::InvalidValue);
    }
    if secret.version != 1 {
        return Err(SecretStoreError::VersionConflict);
    }
    if secret.created_at != stored_at || secret.updated_at != stored_at {
        return Err(SecretStoreError::Storage);
    }
    let key = keyring.get(&secret.key_id)?;
    let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
    let artifact = ProviderQuestionOperationArtifact::try_new(
        binding.provider_id.clone(),
        artifact_type,
        expected_digest,
        SecretValue::new(plaintext),
    )
    .map_err(|_| SecretStoreError::AuthenticationFailed)?;
    insert_secret_audit(transaction, access, audit_event, &secret)
        .await
        .map_err(storage_error)?;
    Ok(artifact)
}

fn validate_artifact(
    provider_id: &ProviderId,
    artifact: &ProviderQuestionOperationArtifact,
) -> Result<(), SecretStoreError> {
    validate_secret(artifact.value())?;
    if artifact.provider_id() != provider_id {
        return Err(SecretStoreError::InvalidValue);
    }
    Ok(())
}

fn artifact_metadata_matches(
    row: &SqliteRow,
    provider_id: &ProviderId,
    artifact: Option<&ProviderQuestionOperationArtifact>,
) -> Result<bool, SecretStoreError> {
    let artifact_type: Option<&str> = row.try_get("artifact_type").map_err(storage_error)?;
    let artifact_digest: Option<Vec<u8>> = row.try_get("artifact_digest").map_err(storage_error)?;
    let secret_blob_id: Option<&str> = row.try_get("secret_blob_id").map_err(storage_error)?;
    Ok(
        match (artifact, artifact_type, artifact_digest, secret_blob_id) {
            (None, None, None, None) => true,
            (Some(artifact), Some(stored_type), Some(stored_digest), Some(_)) => {
                validate_artifact(provider_id, artifact)?;
                stored_type == artifact.artifact_type()
                    && stored_digest.as_slice() == artifact.artifact_digest().as_slice()
            }
            _ => false,
        },
    )
}

fn decode_digest(row: &SqliteRow, column: &str) -> Result<[u8; 32], SecretStoreError> {
    row.try_get::<Vec<u8>, _>(column)
        .map_err(storage_error)?
        .try_into()
        .map_err(|_| SecretStoreError::Storage)
}

fn sequence_i64(sequence: u64) -> Result<i64, SecretStoreError> {
    i64::try_from(sequence).map_err(|_| SecretStoreError::InvalidValue)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, SecretStoreError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|_| SecretStoreError::Storage)
}

fn storage_error(_: sqlx::Error) -> SecretStoreError {
    SecretStoreError::Storage
}
