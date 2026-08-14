use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, ProviderId, QuestionReadAttempt, QuestionReadAttemptId,
    QuestionReadAttemptState, SecretId, Timestamp,
};
use asterism_secrets::{SecretAccess, SecretPurpose, SecretRef, SecretStoreError, SecretValue};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, QuestionReadContinuation, QuestionReadContinuationAttachRequest,
    QuestionReadContinuationRepository, QuestionReadOperation, QuestionReadOperationAcceptRequest,
    QuestionReadOperationFinishOutcome, QuestionReadOperationIssueOutcome,
    QuestionReadOperationIssueRequest, QuestionReadOperationState,
    ResolvedQuestionReadContinuation, SecretKeyring,
    secret::{
        authorize, decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob,
        validate_secret,
    },
};

const MAX_LABEL_BYTES: usize = 96;

#[derive(Clone, Debug)]
pub struct SqliteQuestionReadContinuationRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteQuestionReadContinuationRepository {
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
impl QuestionReadContinuationRepository for SqliteQuestionReadContinuationRepository {
    async fn attach_question_read_continuation(
        &self,
        request: QuestionReadContinuationAttachRequest<'_>,
    ) -> Result<QuestionReadContinuation, SecretStoreError> {
        validate_provider_label(&self.provider_id, request.continuation_type)?;
        validate_provider_label(&self.provider_id, request.phase)?;
        validate_secret(&request.value)?;
        let plaintext_digest = digest(request.value.expose_secret());
        let mut transaction = begin(&self.database).await?;
        let attempt = fetch_attempt(&mut transaction, request.attempt_id)
            .await?
            .ok_or(SecretStoreError::NotFound)?;
        authorize_scoped(&attempt, &self.provider_id, request.access)?;
        if attempt.state != QuestionReadAttemptState::Active
            || attempt.revision != 1
            || request.attached_at < attempt.created_at
            || request.attached_at >= attempt.expires_at
        {
            return Err(SecretStoreError::VersionConflict);
        }
        if let Some(existing) = fetch_continuation(&mut transaction, request.attempt_id).await? {
            if existing.revision == 1
                && existing.continuation_type == request.continuation_type
                && existing.continuation_digest == plaintext_digest
                && existing.phase == request.phase
            {
                transaction.rollback().await.map_err(storage_error)?;
                return Ok(existing);
            }
            return Err(SecretStoreError::VersionConflict);
        }

        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id: attempt.owner_user_id,
            purpose: SecretPurpose::BrowserJobCredential,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.attached_at,
            updated_at: request.attached_at,
        };
        let (nonce, encrypted_data) = encrypt(key, &secret, request.value.expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted_data).await?;
        sqlx::query(
            "INSERT INTO question_read_attempt_continuations \
             (attempt_id, secret_blob_id, continuation_type, continuation_digest, phase, \
              revision, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(request.attempt_id.to_string())
        .bind(secret.id.to_string())
        .bind(request.continuation_type)
        .bind(plaintext_digest.as_slice())
        .bind(request.phase)
        .bind(encode_timestamp(request.attached_at))
        .bind(encode_timestamp(request.attached_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "question_read_continuation_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        let continuation = QuestionReadContinuation {
            attempt_id: request.attempt_id,
            continuation_type: request.continuation_type.to_owned(),
            continuation_digest: plaintext_digest,
            phase: request.phase.to_owned(),
            revision: 1,
            created_at: request.attached_at,
            updated_at: request.attached_at,
        };
        insert_continuation_audit(
            &mut transaction,
            request.access,
            "question_read_continuation_attached",
            &continuation,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(continuation)
    }

    async fn resolve_question_read_continuation(
        &self,
        attempt_id: QuestionReadAttemptId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedQuestionReadContinuation>, SecretStoreError> {
        let mut transaction = begin(&self.database).await?;
        let Some(row) = fetch_resolved_row(&mut transaction, attempt_id).await? else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        let attempt = decode_attempt(&row)?;
        authorize_scoped(&attempt, &self.provider_id, access)?;
        if !matches!(
            attempt.state,
            QuestionReadAttemptState::Active | QuestionReadAttemptState::Ambiguous
        ) {
            return Err(SecretStoreError::VersionConflict);
        }
        let metadata = decode_continuation(&row)?;
        let revision_matches = metadata.revision == attempt.revision
            || (attempt.state == QuestionReadAttemptState::Ambiguous
                && metadata.revision.checked_add(1) == Some(attempt.revision));
        if !revision_matches {
            return Err(SecretStoreError::VersionConflict);
        }
        let secret_id = parse_id(row.try_get("secret_blob_id").map_err(storage_error)?)?;
        let stored = fetch_secret(&mut transaction, secret_id).await?;
        let secret = SecretRef {
            id: secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: stored.version,
            key_id: stored.key_id.clone(),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
        };
        if secret.owner_user_id != attempt.owner_user_id
            || secret.purpose != SecretPurpose::BrowserJobCredential
            || secret.version != metadata.revision
            || secret.updated_at != metadata.updated_at
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let key = self.keyring.get(&secret.key_id)?;
        let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
        if digest(&plaintext) != metadata.continuation_digest {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        let latest_operation = fetch_latest_operation(&mut transaction, attempt_id).await?;
        insert_secret_audit(
            &mut transaction,
            access,
            "question_read_continuation_accessed",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedQuestionReadContinuation {
            metadata,
            latest_operation,
            value: SecretValue::new(plaintext),
        }))
    }

    async fn issue_question_read_operation(
        &self,
        request: QuestionReadOperationIssueRequest<'_>,
    ) -> Result<QuestionReadOperationIssueOutcome, SecretStoreError> {
        validate_provider_label(&self.provider_id, &request.operation_type)?;
        if request.request_digest == [0; 32] || request.expected_continuation_revision == 0 {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        let Some(binding) = fetch_binding(&mut transaction, request.attempt_id).await? else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionReadOperationIssueOutcome::Unavailable);
        };
        authorize_scoped(&binding.attempt, &self.provider_id, request.access)?;
        if binding.attempt.state != QuestionReadAttemptState::Active
            || binding.attempt.revision != request.expected_continuation_revision
            || binding.continuation.revision != request.expected_continuation_revision
            || binding.secret_version != request.expected_continuation_revision
            || request.issued_at < binding.continuation.updated_at
            || request.issued_at >= binding.attempt.expires_at
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionReadOperationIssueOutcome::Unavailable);
        }
        if let Some(existing) = fetch_operation_for_revision(
            &mut transaction,
            request.attempt_id,
            request.expected_continuation_revision,
        )
        .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(
                if existing.state == QuestionReadOperationState::Issued
                    && operation_matches_issue(&existing, &request)
                {
                    QuestionReadOperationIssueOutcome::Duplicate(existing)
                } else {
                    QuestionReadOperationIssueOutcome::Conflict
                },
            );
        }
        let last_sequence: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM question_read_attempt_operations WHERE attempt_id = ?",
        )
        .bind(request.attempt_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let sequence = u64::try_from(last_sequence.map_or(1, |value| value.saturating_add(1)))
            .map_err(|_| SecretStoreError::Storage)?;
        let operation = QuestionReadOperation {
            attempt_id: request.attempt_id,
            sequence,
            continuation_revision: request.expected_continuation_revision,
            operation_type: request.operation_type,
            request_digest: request.request_digest,
            state: QuestionReadOperationState::Issued,
            result_digest: None,
            issued_at: request.issued_at,
            completed_at: None,
        };
        insert_operation(&mut transaction, &operation).await?;
        insert_operation_audit(
            &mut transaction,
            request.access,
            "question_read_operation_issued",
            &operation,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(QuestionReadOperationIssueOutcome::Issued(operation))
    }

    #[allow(clippy::too_many_lines)]
    async fn accept_question_read_operation(
        &self,
        request: QuestionReadOperationAcceptRequest<'_>,
    ) -> Result<QuestionReadOperationFinishOutcome, SecretStoreError> {
        validate_provider_label(&self.provider_id, request.next_continuation_type)?;
        validate_provider_label(&self.provider_id, request.next_phase)?;
        validate_secret(&request.replacement)?;
        if request.result_digest == [0; 32] || request.accepted_at < request.operation.issued_at {
            return Err(SecretStoreError::InvalidValue);
        }
        let next_digest = digest(request.replacement.expose_secret());
        let mut transaction = begin(&self.database).await?;
        let Some(existing) = fetch_operation(
            &mut transaction,
            request.operation.attempt_id,
            request.operation.sequence,
        )
        .await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionReadOperationFinishOutcome::Unavailable);
        };
        if existing.state != QuestionReadOperationState::Issued {
            let duplicate = existing.state == QuestionReadOperationState::Accepted
                && existing.result_digest == Some(request.result_digest)
                && fetch_continuation(&mut transaction, existing.attempt_id)
                    .await?
                    .is_some_and(|continuation| {
                        continuation.revision == existing.continuation_revision.saturating_add(1)
                            && continuation.continuation_type == request.next_continuation_type
                            && continuation.continuation_digest == next_digest
                            && continuation.phase == request.next_phase
                            && continuation.updated_at == request.accepted_at
                    });
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(if duplicate {
                QuestionReadOperationFinishOutcome::Duplicate(existing)
            } else {
                QuestionReadOperationFinishOutcome::Conflict
            });
        }
        if &existing != request.operation {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionReadOperationFinishOutcome::Conflict);
        }
        let Some(binding) = fetch_binding(&mut transaction, existing.attempt_id).await? else {
            return Ok(QuestionReadOperationFinishOutcome::Unavailable);
        };
        authorize_scoped(&binding.attempt, &self.provider_id, request.access)?;
        if binding.attempt.state != QuestionReadAttemptState::Active
            || binding.attempt.revision != existing.continuation_revision
            || binding.continuation.revision != existing.continuation_revision
            || binding.secret_version != existing.continuation_revision
        {
            return Ok(QuestionReadOperationFinishOutcome::Conflict);
        }
        let next_revision = existing
            .continuation_revision
            .checked_add(1)
            .ok_or(SecretStoreError::VersionConflict)?;
        let stored = fetch_secret(&mut transaction, binding.secret_id).await?;
        let (key_id, key) = self.keyring.active();
        let rotated = SecretRef {
            id: binding.secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: next_revision,
            key_id: key_id.to_owned(),
            created_at: stored.created_at,
            updated_at: request.accepted_at,
        };
        let (nonce, encrypted_data) = encrypt(key, &rotated, request.replacement.expose_secret())?;
        let secret_updated = sqlx::query(
            "UPDATE secret_blobs SET key_id = ?, nonce = ?, encrypted_data = ?, version = ?, \
             updated_at = ? WHERE id = ? AND version = ?",
        )
        .bind(&rotated.key_id)
        .bind(nonce)
        .bind(encrypted_data)
        .bind(i64::from(next_revision))
        .bind(encode_timestamp(request.accepted_at))
        .bind(rotated.id.to_string())
        .bind(i64::from(existing.continuation_revision))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if secret_updated.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        let continuation_updated = sqlx::query(
            "UPDATE question_read_attempt_continuations SET continuation_type = ?, \
             continuation_digest = ?, phase = ?, revision = ?, updated_at = ? \
             WHERE attempt_id = ? AND revision = ?",
        )
        .bind(request.next_continuation_type)
        .bind(next_digest.as_slice())
        .bind(request.next_phase)
        .bind(i64::from(next_revision))
        .bind(encode_timestamp(request.accepted_at))
        .bind(existing.attempt_id.to_string())
        .bind(i64::from(existing.continuation_revision))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if continuation_updated.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        let mut attempt = binding.attempt;
        attempt
            .advance_active(request.accepted_at)
            .map_err(|_| SecretStoreError::VersionConflict)?;
        update_attempt(&mut transaction, &attempt, existing.continuation_revision).await?;
        let mut accepted = existing;
        accepted.state = QuestionReadOperationState::Accepted;
        accepted.result_digest = Some(request.result_digest);
        accepted.completed_at = Some(request.accepted_at);
        persist_operation_finish(&mut transaction, &accepted).await?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "question_read_continuation_rotated",
            &rotated,
        )
        .await
        .map_err(storage_error)?;
        let continuation = QuestionReadContinuation {
            attempt_id: accepted.attempt_id,
            continuation_type: request.next_continuation_type.to_owned(),
            continuation_digest: next_digest,
            phase: request.next_phase.to_owned(),
            revision: next_revision,
            created_at: binding.continuation.created_at,
            updated_at: request.accepted_at,
        };
        insert_operation_audit(
            &mut transaction,
            request.access,
            "question_read_operation_accepted",
            &accepted,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(QuestionReadOperationFinishOutcome::Accepted {
            operation: accepted,
            continuation,
            attempt,
        })
    }

    async fn finish_question_read_operation(
        &self,
        operation: &QuestionReadOperation,
        terminal_state: QuestionReadOperationState,
        result_digest: Option<[u8; 32]>,
        completed_at: Timestamp,
        access: &SecretAccess,
    ) -> Result<QuestionReadOperationFinishOutcome, SecretStoreError> {
        if !matches!(
            terminal_state,
            QuestionReadOperationState::Rejected | QuestionReadOperationState::Ambiguous
        ) || completed_at < operation.issued_at
            || (terminal_state == QuestionReadOperationState::Rejected
                && result_digest.is_none_or(|digest| digest == [0; 32]))
            || (terminal_state == QuestionReadOperationState::Ambiguous && result_digest.is_some())
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        let Some(existing) =
            fetch_operation(&mut transaction, operation.attempt_id, operation.sequence).await?
        else {
            return Ok(QuestionReadOperationFinishOutcome::Unavailable);
        };
        if existing.state != QuestionReadOperationState::Issued {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(
                if existing.state == terminal_state && existing.result_digest == result_digest {
                    QuestionReadOperationFinishOutcome::Duplicate(existing)
                } else {
                    QuestionReadOperationFinishOutcome::Conflict
                },
            );
        }
        if &existing != operation {
            return Ok(QuestionReadOperationFinishOutcome::Conflict);
        }
        let Some(binding) = fetch_binding(&mut transaction, operation.attempt_id).await? else {
            return Ok(QuestionReadOperationFinishOutcome::Unavailable);
        };
        authorize_scoped(&binding.attempt, &self.provider_id, access)?;
        if binding.attempt.state != QuestionReadAttemptState::Active
            || binding.attempt.revision != operation.continuation_revision
            || binding.continuation.revision != operation.continuation_revision
        {
            return Ok(QuestionReadOperationFinishOutcome::Conflict);
        }
        let mut attempt = binding.attempt;
        match terminal_state {
            QuestionReadOperationState::Rejected => attempt.reject(
                result_digest.ok_or(SecretStoreError::InvalidValue)?,
                completed_at,
            ),
            QuestionReadOperationState::Ambiguous => attempt.mark_ambiguous(completed_at),
            QuestionReadOperationState::Issued | QuestionReadOperationState::Accepted => {
                unreachable!()
            }
        }
        .map_err(|_| SecretStoreError::VersionConflict)?;
        update_attempt(&mut transaction, &attempt, operation.continuation_revision).await?;
        let mut finished = existing;
        finished.state = terminal_state;
        finished.result_digest = result_digest;
        finished.completed_at = Some(completed_at);
        persist_operation_finish(&mut transaction, &finished).await?;
        insert_operation_audit(
            &mut transaction,
            access,
            match terminal_state {
                QuestionReadOperationState::Rejected => "question_read_operation_rejected",
                QuestionReadOperationState::Ambiguous => "question_read_operation_ambiguous",
                QuestionReadOperationState::Issued | QuestionReadOperationState::Accepted => {
                    unreachable!()
                }
            },
            &finished,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(QuestionReadOperationFinishOutcome::Finished {
            operation: finished,
            attempt,
        })
    }
}

#[derive(Debug)]
struct AttemptBinding {
    attempt: QuestionReadAttempt,
    continuation: QuestionReadContinuation,
    secret_id: SecretId,
    secret_version: u32,
}

async fn begin(database: &Database) -> Result<Transaction<'_, Sqlite>, SecretStoreError> {
    database
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_error)
}

async fn fetch_attempt(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
) -> Result<Option<QuestionReadAttempt>, SecretStoreError> {
    sqlx::query(
        "SELECT id, owner_user_id, provider_account_id, task_id, provider_id, provider_version, \
                state, question_snapshot_id, question_session_id, response_digest, revision, \
                expires_at, completed_at, created_at, updated_at \
         FROM question_read_attempts WHERE id = ?",
    )
    .bind(attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_attempt)
    .transpose()
}

async fn fetch_resolved_row(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
) -> Result<Option<SqliteRow>, SecretStoreError> {
    sqlx::query(
        "SELECT attempt.id, attempt.owner_user_id, attempt.provider_account_id, attempt.task_id, \
                attempt.provider_id, attempt.provider_version, attempt.state, \
                attempt.question_snapshot_id, attempt.question_session_id, attempt.response_digest, \
                attempt.revision, attempt.expires_at, attempt.completed_at, attempt.created_at, \
                attempt.updated_at, continuation.secret_blob_id, continuation.continuation_type, \
                continuation.continuation_digest, continuation.phase, \
                continuation.revision AS continuation_revision, \
                continuation.created_at AS continuation_created_at, \
                continuation.updated_at AS continuation_updated_at \
         FROM question_read_attempts AS attempt \
         INNER JOIN question_read_attempt_continuations AS continuation \
            ON continuation.attempt_id = attempt.id WHERE attempt.id = ?",
    )
    .bind(attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)
}

async fn fetch_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
) -> Result<Option<AttemptBinding>, SecretStoreError> {
    let Some(row) = fetch_resolved_row(transaction, attempt_id).await? else {
        return Ok(None);
    };
    Ok(Some(AttemptBinding {
        attempt: decode_attempt(&row)?,
        continuation: decode_continuation(&row)?,
        secret_id: parse_id(row.try_get("secret_blob_id").map_err(storage_error)?)?,
        secret_version: u32::try_from(
            sqlx::query_scalar::<_, i64>("SELECT version FROM secret_blobs WHERE id = ?")
                .bind(
                    row.try_get::<&str, _>("secret_blob_id")
                        .map_err(storage_error)?,
                )
                .fetch_one(&mut **transaction)
                .await
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
    }))
}

async fn fetch_continuation(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
) -> Result<Option<QuestionReadContinuation>, SecretStoreError> {
    sqlx::query(
        "SELECT attempt_id, continuation_type, continuation_digest, phase, revision, \
                created_at, updated_at FROM question_read_attempt_continuations \
         WHERE attempt_id = ?",
    )
    .bind(attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_continuation)
    .transpose()
}

fn decode_attempt(row: &SqliteRow) -> Result<QuestionReadAttempt, SecretStoreError> {
    let response_digest = row
        .try_get::<Option<Vec<u8>>, _>("response_digest")
        .map_err(storage_error)?
        .map(decode_digest)
        .transpose()?;
    let attempt = QuestionReadAttempt {
        id: parse_id(row.try_get("id").map_err(storage_error)?)?,
        owner_user_id: parse_id(row.try_get("owner_user_id").map_err(storage_error)?)?,
        provider_account_id: parse_id(row.try_get("provider_account_id").map_err(storage_error)?)?,
        task_id: parse_id(row.try_get("task_id").map_err(storage_error)?)?,
        provider_id: ProviderId::new(
            row.try_get::<String, _>("provider_id")
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        provider_version: row.try_get("provider_version").map_err(storage_error)?,
        state: decode_attempt_state(row.try_get("state").map_err(storage_error)?)?,
        question_snapshot_id: row
            .try_get::<Option<&str>, _>("question_snapshot_id")
            .map_err(storage_error)?
            .map(parse_id)
            .transpose()?,
        question_session_id: row
            .try_get::<Option<&str>, _>("question_session_id")
            .map_err(storage_error)?
            .map(parse_id)
            .transpose()?,
        response_digest,
        revision: u32::try_from(row.try_get::<i64, _>("revision").map_err(storage_error)?)
            .map_err(|_| SecretStoreError::Storage)?,
        expires_at: decode_timestamp(row.try_get("expires_at").map_err(storage_error)?)?,
        completed_at: decode_optional_timestamp(
            row.try_get("completed_at").map_err(storage_error)?,
        )?,
        created_at: decode_timestamp(row.try_get("created_at").map_err(storage_error)?)?,
        updated_at: decode_timestamp(row.try_get("updated_at").map_err(storage_error)?)?,
    };
    attempt.validate().map_err(|_| SecretStoreError::Storage)?;
    Ok(attempt)
}

fn decode_continuation(row: &SqliteRow) -> Result<QuestionReadContinuation, SecretStoreError> {
    let revision_column = if row.try_get_raw("continuation_revision").is_ok() {
        "continuation_revision"
    } else {
        "revision"
    };
    let created_column = if row.try_get_raw("continuation_created_at").is_ok() {
        "continuation_created_at"
    } else {
        "created_at"
    };
    let updated_column = if row.try_get_raw("continuation_updated_at").is_ok() {
        "continuation_updated_at"
    } else {
        "updated_at"
    };
    Ok(QuestionReadContinuation {
        attempt_id: parse_id(
            row.try_get("attempt_id")
                .or_else(|_| row.try_get("id"))
                .map_err(storage_error)?,
        )?,
        continuation_type: row.try_get("continuation_type").map_err(storage_error)?,
        continuation_digest: decode_digest(
            row.try_get("continuation_digest").map_err(storage_error)?,
        )?,
        phase: row.try_get("phase").map_err(storage_error)?,
        revision: u32::try_from(
            row.try_get::<i64, _>(revision_column)
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        created_at: decode_timestamp(row.try_get(created_column).map_err(storage_error)?)?,
        updated_at: decode_timestamp(row.try_get(updated_column).map_err(storage_error)?)?,
    })
}

async fn insert_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &QuestionReadOperation,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO question_read_attempt_operations \
         (attempt_id, sequence, continuation_revision, operation_type, request_digest, state, \
          issued_at) VALUES (?, ?, ?, ?, ?, 'issued', ?)",
    )
    .bind(operation.attempt_id.to_string())
    .bind(i64::try_from(operation.sequence).map_err(|_| SecretStoreError::InvalidValue)?)
    .bind(i64::from(operation.continuation_revision))
    .bind(&operation.operation_type)
    .bind(operation.request_digest.as_slice())
    .bind(encode_timestamp(operation.issued_at))
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn fetch_operation_for_revision(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
    revision: u32,
) -> Result<Option<QuestionReadOperation>, SecretStoreError> {
    fetch_operation_query(
        transaction,
        "attempt_id = ? AND continuation_revision = ?",
        attempt_id,
        i64::from(revision),
    )
    .await
}

async fn fetch_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
    sequence: u64,
) -> Result<Option<QuestionReadOperation>, SecretStoreError> {
    fetch_operation_query(
        transaction,
        "attempt_id = ? AND sequence = ?",
        attempt_id,
        i64::try_from(sequence).map_err(|_| SecretStoreError::InvalidValue)?,
    )
    .await
}

async fn fetch_operation_query(
    transaction: &mut Transaction<'_, Sqlite>,
    predicate: &str,
    attempt_id: QuestionReadAttemptId,
    value: i64,
) -> Result<Option<QuestionReadOperation>, SecretStoreError> {
    let query = format!(
        "SELECT attempt_id, sequence, continuation_revision, operation_type, request_digest, \
                state, result_digest, issued_at, completed_at \
         FROM question_read_attempt_operations WHERE {predicate}"
    );
    sqlx::query(&query)
        .bind(attempt_id.to_string())
        .bind(value)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
        .as_ref()
        .map(decode_operation)
        .transpose()
}

async fn fetch_latest_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: QuestionReadAttemptId,
) -> Result<Option<QuestionReadOperation>, SecretStoreError> {
    sqlx::query(
        "SELECT attempt_id, sequence, continuation_revision, operation_type, request_digest, \
                state, result_digest, issued_at, completed_at \
         FROM question_read_attempt_operations WHERE attempt_id = ? \
         ORDER BY sequence DESC LIMIT 1",
    )
    .bind(attempt_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_operation)
    .transpose()
}

fn decode_operation(row: &SqliteRow) -> Result<QuestionReadOperation, SecretStoreError> {
    Ok(QuestionReadOperation {
        attempt_id: parse_id(row.try_get("attempt_id").map_err(storage_error)?)?,
        sequence: u64::try_from(row.try_get::<i64, _>("sequence").map_err(storage_error)?)
            .map_err(|_| SecretStoreError::Storage)?,
        continuation_revision: u32::try_from(
            row.try_get::<i64, _>("continuation_revision")
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        operation_type: row.try_get("operation_type").map_err(storage_error)?,
        request_digest: decode_digest(row.try_get("request_digest").map_err(storage_error)?)?,
        state: decode_operation_state(row.try_get("state").map_err(storage_error)?)?,
        result_digest: row
            .try_get::<Option<Vec<u8>>, _>("result_digest")
            .map_err(storage_error)?
            .map(decode_digest)
            .transpose()?,
        issued_at: decode_timestamp(row.try_get("issued_at").map_err(storage_error)?)?,
        completed_at: decode_optional_timestamp(
            row.try_get("completed_at").map_err(storage_error)?,
        )?,
    })
}

async fn persist_operation_finish(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &QuestionReadOperation,
) -> Result<(), SecretStoreError> {
    let result = sqlx::query(
        "UPDATE question_read_attempt_operations SET state = ?, result_digest = ?, completed_at = ? \
         WHERE attempt_id = ? AND sequence = ? AND state = 'issued'",
    )
    .bind(operation_state_name(operation.state))
    .bind(operation.result_digest.map(|digest| digest.to_vec()))
    .bind(operation.completed_at.map(encode_timestamp))
    .bind(operation.attempt_id.to_string())
    .bind(i64::try_from(operation.sequence).map_err(|_| SecretStoreError::InvalidValue)?)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(SecretStoreError::VersionConflict)
    }
}

async fn update_attempt(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt: &QuestionReadAttempt,
    expected_revision: u32,
) -> Result<(), SecretStoreError> {
    let result = sqlx::query(
        "UPDATE question_read_attempts SET state = ?, response_digest = ?, revision = ?, \
         completed_at = ?, updated_at = ? WHERE id = ? AND revision = ?",
    )
    .bind(attempt_state_name(attempt.state))
    .bind(attempt.response_digest.map(|digest| digest.to_vec()))
    .bind(i64::from(attempt.revision))
    .bind(attempt.completed_at.map(encode_timestamp))
    .bind(encode_timestamp(attempt.updated_at))
    .bind(attempt.id.to_string())
    .bind(i64::from(expected_revision))
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(SecretStoreError::VersionConflict)
    }
}

async fn insert_continuation_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    action: &str,
    continuation: &QuestionReadContinuation,
) -> Result<(), SecretStoreError> {
    insert_audit(
        transaction,
        access,
        action,
        "question_read_attempt",
        &continuation.attempt_id.to_string(),
        continuation.updated_at,
        serde_json::json!({
            "continuation_type": continuation.continuation_type,
            "continuation_digest": "[HASHED]",
            "phase": continuation.phase,
            "revision": continuation.revision,
        }),
    )
    .await
}

async fn insert_operation_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    action: &str,
    operation: &QuestionReadOperation,
) -> Result<(), SecretStoreError> {
    insert_audit(
        transaction,
        access,
        action,
        "question_read_operation",
        &format!("{}:{}", operation.attempt_id, operation.sequence),
        operation.completed_at.unwrap_or(operation.issued_at),
        serde_json::json!({
            "attempt_id": operation.attempt_id,
            "sequence": operation.sequence,
            "continuation_revision": operation.continuation_revision,
            "operation_type": operation.operation_type,
            "request_digest": "[HASHED]",
            "state": operation_state_name(operation.state),
            "result_digest": operation.result_digest.map(|_| "[HASHED]"),
        }),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    occurred_at: Timestamp,
    metadata: serde_json::Value,
) -> Result<(), SecretStoreError> {
    let (actor_type, actor_id) = match &access.actor {
        asterism_secrets::SecretActor::User(id) => ("user", id.to_string()),
        asterism_secrets::SecretActor::ServiceToken(id) => ("service_token", id.to_string()),
        asterism_secrets::SecretActor::CoreService(name) => ("core_service", (*name).to_owned()),
        asterism_secrets::SecretActor::ProviderRuntime(id) => ("provider_runtime", id.to_owned()),
    };
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(occurred_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(&access.correlation_id)
    .bind(metadata.to_string())
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn authorize_scoped(
    attempt: &QuestionReadAttempt,
    provider_id: &ProviderId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    authorize(attempt.owner_user_id, access)?;
    if &attempt.provider_id == provider_id {
        Ok(())
    } else {
        Err(SecretStoreError::Unauthorized)
    }
}

fn validate_provider_label(provider_id: &ProviderId, value: &str) -> Result<(), SecretStoreError> {
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        || !value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
    {
        Err(SecretStoreError::InvalidValue)
    } else {
        Ok(())
    }
}

fn operation_matches_issue(
    operation: &QuestionReadOperation,
    request: &QuestionReadOperationIssueRequest<'_>,
) -> bool {
    operation.attempt_id == request.attempt_id
        && operation.continuation_revision == request.expected_continuation_revision
        && operation.operation_type == request.operation_type
        && operation.request_digest == request.request_digest
        && operation.issued_at == request.issued_at
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn decode_digest(value: Vec<u8>) -> Result<[u8; 32], SecretStoreError> {
    value.try_into().map_err(|_| SecretStoreError::Storage)
}

fn decode_attempt_state(value: &str) -> Result<QuestionReadAttemptState, SecretStoreError> {
    match value {
        "active" => Ok(QuestionReadAttemptState::Active),
        "ambiguous" => Ok(QuestionReadAttemptState::Ambiguous),
        "materialized" => Ok(QuestionReadAttemptState::Materialized),
        "rejected" => Ok(QuestionReadAttemptState::Rejected),
        "cancelled" => Ok(QuestionReadAttemptState::Cancelled),
        "expired" => Ok(QuestionReadAttemptState::Expired),
        _ => Err(SecretStoreError::Storage),
    }
}

fn attempt_state_name(state: QuestionReadAttemptState) -> &'static str {
    match state {
        QuestionReadAttemptState::Active => "active",
        QuestionReadAttemptState::Ambiguous => "ambiguous",
        QuestionReadAttemptState::Materialized => "materialized",
        QuestionReadAttemptState::Rejected => "rejected",
        QuestionReadAttemptState::Cancelled => "cancelled",
        QuestionReadAttemptState::Expired => "expired",
    }
}

fn decode_operation_state(value: &str) -> Result<QuestionReadOperationState, SecretStoreError> {
    match value {
        "issued" => Ok(QuestionReadOperationState::Issued),
        "accepted" => Ok(QuestionReadOperationState::Accepted),
        "rejected" => Ok(QuestionReadOperationState::Rejected),
        "ambiguous" => Ok(QuestionReadOperationState::Ambiguous),
        _ => Err(SecretStoreError::Storage),
    }
}

fn operation_state_name(state: QuestionReadOperationState) -> &'static str {
    match state {
        QuestionReadOperationState::Issued => "issued",
        QuestionReadOperationState::Accepted => "accepted",
        QuestionReadOperationState::Rejected => "rejected",
        QuestionReadOperationState::Ambiguous => "ambiguous",
    }
}

fn parse_id<T>(value: &str) -> Result<T, SecretStoreError>
where
    T: FromStr,
{
    value.parse().map_err(|_| SecretStoreError::Storage)
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, SecretStoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| SecretStoreError::Storage)
}

fn decode_optional_timestamp(value: Option<&str>) -> Result<Option<Timestamp>, SecretStoreError> {
    value.map(decode_timestamp).transpose()
}

fn storage_error(_: impl std::fmt::Display) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_domain::{AuditActor, ProviderAccountId, QuestionReadAttempt, TaskId, UserId};
    use asterism_secrets::{SecretActor, SecretKey};
    use chrono::{Duration, Utc};

    use super::*;
    use crate::{QuestionReadAttemptRepository, SqliteQuestionReadAttemptRepository};

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn accepted_operations_rotate_encrypted_pre_question_state_exactly_once() {
        let fixture = Fixture::new().await;
        let attempt = fixture.attempt(b"cidaren-pre-question-v1").await;

        let resolved = fixture
            .continuations
            .resolve_question_read_continuation(
                attempt.id,
                &fixture.access("pre-question-resolve-v1"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.value.expose_secret(), b"cidaren-pre-question-v1");
        assert_eq!(resolved.metadata.revision, 1);
        assert!(resolved.latest_operation.is_none());

        let issue = QuestionReadOperationIssueRequest {
            attempt_id: attempt.id,
            expected_continuation_revision: 1,
            operation_type: "cidaren.submit-chose-word.v1".to_owned(),
            request_digest: [7; 32],
            issued_at: fixture.now + Duration::seconds(1),
            access: &fixture.access("pre-question-issue"),
        };
        let QuestionReadOperationIssueOutcome::Issued(operation) = fixture
            .continuations
            .issue_question_read_operation(issue.clone())
            .await
            .unwrap()
        else {
            panic!("expected an issued operation");
        };
        assert!(matches!(
            fixture
                .continuations
                .issue_question_read_operation(issue)
                .await
                .unwrap(),
            QuestionReadOperationIssueOutcome::Duplicate(existing) if existing == operation
        ));

        let accepted_at = fixture.now + Duration::seconds(2);
        let replacement = b"cidaren-ready-to-start";
        let outcome = fixture
            .continuations
            .accept_question_read_operation(QuestionReadOperationAcceptRequest {
                operation: &operation,
                next_continuation_type: "cidaren.pre-question.v1",
                next_phase: "cidaren.ready-to-start",
                replacement: SecretValue::new(replacement.to_vec()),
                result_digest: [8; 32],
                accepted_at,
                access: &fixture.access("pre-question-accept"),
            })
            .await
            .unwrap();
        let QuestionReadOperationFinishOutcome::Accepted {
            operation: accepted,
            continuation,
            attempt: advanced,
        } = outcome
        else {
            panic!("expected atomic pre-Question rotation");
        };
        assert_eq!(accepted.state, QuestionReadOperationState::Accepted);
        assert_eq!(continuation.revision, 2);
        assert_eq!(advanced.revision, 2);
        assert_eq!(advanced.state, QuestionReadAttemptState::Active);

        assert!(matches!(
            fixture
                .continuations
                .accept_question_read_operation(QuestionReadOperationAcceptRequest {
                    operation: &operation,
                    next_continuation_type: "cidaren.pre-question.v1",
                    next_phase: "cidaren.ready-to-start",
                    replacement: SecretValue::new(replacement.to_vec()),
                    result_digest: [8; 32],
                    accepted_at,
                    access: &fixture.access("pre-question-accept-replay"),
                })
                .await
                .unwrap(),
            QuestionReadOperationFinishOutcome::Duplicate(existing) if existing == accepted
        ));
        assert_eq!(
            fixture
                .continuations
                .accept_question_read_operation(QuestionReadOperationAcceptRequest {
                    operation: &operation,
                    next_continuation_type: "cidaren.pre-question.v1",
                    next_phase: "cidaren.ready-to-start",
                    replacement: SecretValue::new(b"substituted".to_vec()),
                    result_digest: [8; 32],
                    accepted_at,
                    access: &fixture.access("pre-question-accept-conflict"),
                })
                .await
                .unwrap(),
            QuestionReadOperationFinishOutcome::Conflict
        );

        let resolved = fixture
            .continuations
            .resolve_question_read_continuation(
                attempt.id,
                &fixture.access("pre-question-resolve-v2"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.value.expose_secret(), replacement);
        assert_eq!(resolved.metadata.revision, 2);
        assert_eq!(resolved.latest_operation, Some(accepted));
    }

    #[tokio::test]
    async fn ambiguous_operation_locks_revision_and_secret_scope_fails_closed() {
        let fixture = Fixture::new().await;
        let attempt = fixture.attempt(b"chaoxing-exam-start").await;
        let QuestionReadOperationIssueOutcome::Issued(operation) = fixture
            .continuations
            .issue_question_read_operation(QuestionReadOperationIssueRequest {
                attempt_id: attempt.id,
                expected_continuation_revision: 1,
                operation_type: "cidaren.start-answer.v1".to_owned(),
                request_digest: [4; 32],
                issued_at: fixture.now + Duration::seconds(1),
                access: &fixture.access("start-answer-issue"),
            })
            .await
            .unwrap()
        else {
            panic!("expected an issued operation");
        };
        let finished = fixture
            .continuations
            .finish_question_read_operation(
                &operation,
                QuestionReadOperationState::Ambiguous,
                None,
                fixture.now + Duration::seconds(2),
                &fixture.access("start-answer-ambiguous"),
            )
            .await
            .unwrap();
        assert!(matches!(
            finished,
            QuestionReadOperationFinishOutcome::Finished { operation, attempt }
                if operation.state == QuestionReadOperationState::Ambiguous
                    && attempt.state == QuestionReadAttemptState::Ambiguous
        ));
        assert_eq!(
            fixture
                .continuations
                .issue_question_read_operation(QuestionReadOperationIssueRequest {
                    attempt_id: attempt.id,
                    expected_continuation_revision: 1,
                    operation_type: "cidaren.start-answer.v1".to_owned(),
                    request_digest: [4; 32],
                    issued_at: fixture.now + Duration::seconds(3),
                    access: &fixture.access("start-answer-replay"),
                })
                .await
                .unwrap(),
            QuestionReadOperationIssueOutcome::Unavailable
        );

        let foreign = SqliteQuestionReadContinuationRepository::new(
            fixture.database.clone(),
            fixture.keyring.clone(),
            ProviderId::new("uai").unwrap(),
        );
        assert!(matches!(
            foreign
                .resolve_question_read_continuation(
                    attempt.id,
                    &SecretAccess {
                        actor: SecretActor::ProviderRuntime("uai".to_owned()),
                        correlation_id: "foreign-attempt".to_owned(),
                        reason: "provider execution".to_owned(),
                    },
                )
                .await,
            Err(SecretStoreError::Unauthorized)
        ));

        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = X'00' WHERE id = ( \
             SELECT secret_blob_id FROM question_read_attempt_continuations WHERE attempt_id = ?)",
        )
        .bind(attempt.id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .continuations
                .resolve_question_read_continuation(
                    attempt.id,
                    &fixture.access("tampered-attempt"),
                )
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    struct Fixture {
        database: Database,
        keyring: Arc<SecretKeyring>,
        attempts: SqliteQuestionReadAttemptRepository,
        continuations: SqliteQuestionReadContinuationRepository,
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
            let mut keys = BTreeMap::new();
            keys.insert("question-read-key".to_owned(), SecretKey::new([19; 32]));
            let keyring = Arc::new(SecretKeyring::new("question-read-key", keys).unwrap());
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let task = TaskId::new();
            let provider = ProviderId::new("cidaren").unwrap();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'question-read-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'Question Read', '\"authenticated\"', ?, ?)",
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
                 VALUES (?, ?, 'question-read', 'v1:question-read', 'exercise', 'unknown', \
                         'Question Read', 'pending', 'discovered', ?, ?, '[\"question_inventory\"]')",
            )
            .bind(task.to_string())
            .bind(account.to_string())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            let attempts = SqliteQuestionReadAttemptRepository::new(database.clone());
            let continuations = SqliteQuestionReadContinuationRepository::new(
                database.clone(),
                keyring.clone(),
                provider.clone(),
            );
            Self {
                database,
                keyring,
                attempts,
                continuations,
                owner,
                account,
                task,
                provider,
                now,
            }
        }

        async fn attempt(&self, initial: &[u8]) -> QuestionReadAttempt {
            let attempt = QuestionReadAttempt::active(
                self.owner,
                self.account,
                self.task,
                self.provider.clone(),
                "attempt-v1".to_owned(),
                self.now,
                self.now + Duration::minutes(5),
            )
            .unwrap();
            self.attempts
                .create_question_read_attempt(
                    &attempt,
                    AuditActor::User(self.owner),
                    "question-read-attempt-create",
                )
                .await
                .unwrap();
            self.continuations
                .attach_question_read_continuation(QuestionReadContinuationAttachRequest {
                    attempt_id: attempt.id,
                    continuation_type: "cidaren.pre-question.v1",
                    phase: "cidaren.ready-to-select",
                    value: SecretValue::new(initial.to_vec()),
                    attached_at: self.now,
                    access: &self.access("question-read-attempt-attach"),
                })
                .await
                .unwrap();
            attempt
        }

        fn access(&self, correlation_id: &str) -> SecretAccess {
            SecretAccess {
                actor: SecretActor::ProviderRuntime(self.provider.to_string()),
                correlation_id: correlation_id.to_owned(),
                reason: "provider execution".to_owned(),
            }
        }
    }
}
