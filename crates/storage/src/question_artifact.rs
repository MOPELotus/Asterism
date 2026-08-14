use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, ExecutionAttemptId, ExecutionId, ProviderId, QuestionSessionId,
    QuestionSessionState, SecretId, Timestamp, UserId,
};
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, QuestionSessionArtifactAttachRequest, QuestionSessionArtifactRepository,
    QuestionSessionContinuation, QuestionSessionOperation, QuestionSessionOperationAcceptRequest,
    QuestionSessionOperationFinishOutcome, QuestionSessionOperationIssueOutcome,
    QuestionSessionOperationIssueRequest, QuestionSessionOperationState,
    ResolvedQuestionSessionContinuation, SecretKeyring,
    secret::{
        authorize, decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob,
        validate_secret,
    },
};

const MAX_LABEL_BYTES: usize = 96;

macro_rules! row_value {
    ($row:expr, $column:literal, $type:ty) => {
        $row.try_get::<$type, _>($column).map_err(storage_error)?
    };
    ($row:expr, $column:literal) => {
        $row.try_get($column).map_err(storage_error)?
    };
}

#[derive(Clone, Debug)]
pub struct SqliteQuestionSessionArtifactRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteQuestionSessionArtifactRepository {
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
#[allow(clippy::too_many_lines)]
impl QuestionSessionArtifactRepository for SqliteQuestionSessionArtifactRepository {
    async fn attach_question_session_artifact(
        &self,
        request: QuestionSessionArtifactAttachRequest<'_>,
    ) -> Result<QuestionSessionContinuation, SecretStoreError> {
        validate_label(request.phase)?;
        validate_secret(&request.value)?;
        let plaintext_digest = digest(request.value.expose_secret());
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let session = fetch_artifact_session(&mut transaction, request.session_id)
            .await?
            .ok_or(SecretStoreError::NotFound)?;
        authorize_scoped(
            session.owner_user_id,
            &session.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if session.state != QuestionSessionState::Active
            || request.attached_at < session.created_at
            || request.attached_at >= session.expires_at
            || plaintext_digest != session.artifact_digest
            || !label_belongs_to_provider(&self.provider_id, &session.artifact_type)
        {
            return Err(SecretStoreError::VersionConflict);
        }
        if let Some(existing) = fetch_continuation(&mut transaction, request.session_id).await? {
            if existing.execution_id.is_none()
                && existing.continuation_type == session.artifact_type
                && existing.continuation_digest == plaintext_digest
                && existing.phase == request.phase
                && existing.revision == 1
            {
                transaction.rollback().await.map_err(storage_error)?;
                return Ok(existing);
            }
            return Err(SecretStoreError::VersionConflict);
        }

        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id: session.owner_user_id,
            purpose: SecretPurpose::BrowserJobCredential,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.attached_at,
            updated_at: request.attached_at,
        };
        let (nonce, encrypted_data) = encrypt(key, &secret, request.value.expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted_data).await?;
        sqlx::query(
            "INSERT INTO question_session_continuations \
             (session_id, execution_id, secret_blob_id, continuation_type, \
              continuation_digest, phase, revision, created_at, updated_at) \
             VALUES (?, NULL, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(request.session_id.to_string())
        .bind(secret.id.to_string())
        .bind(&session.artifact_type)
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
            "question_session_artifact_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        let continuation = QuestionSessionContinuation {
            session_id: request.session_id,
            execution_id: None,
            continuation_type: session.artifact_type,
            continuation_digest: plaintext_digest,
            phase: request.phase.to_owned(),
            revision: 1,
            created_at: request.attached_at,
            updated_at: request.attached_at,
        };
        insert_continuation_audit(
            &mut transaction,
            request.access,
            "question_session_artifact_attached",
            &continuation,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(continuation)
    }

    async fn resolve_question_session_continuation(
        &self,
        execution_id: ExecutionId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedQuestionSessionContinuation>, SecretStoreError> {
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(row) = fetch_resolved_row(&mut transaction, execution_id).await? else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        let provider_id = ProviderId::new(row_value!(row, "provider_id", String))
            .map_err(|_| SecretStoreError::Storage)?;
        let owner_user_id = parse_id(row_value!(row, "owner_user_id"))?;
        authorize_scoped(owner_user_id, &provider_id, &self.provider_id, access)?;
        let session_state = decode_session_state(row_value!(row, "session_state"))?;
        let stored_execution_id: Option<&str> = row_value!(row, "session_execution_id");
        let expected_execution_id = execution_id.to_string();
        if !matches!(
            session_state,
            QuestionSessionState::Claimed | QuestionSessionState::Consumed
        ) || stored_execution_id != Some(expected_execution_id.as_str())
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let metadata = decode_continuation(&row)?;
        if metadata.execution_id != Some(execution_id) {
            return Err(SecretStoreError::VersionConflict);
        }
        let secret_id: SecretId = parse_id(row_value!(row, "secret_blob_id"))?;
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
        if secret.owner_user_id != owner_user_id
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
        let latest_operation =
            fetch_latest_operation(&mut transaction, metadata.session_id).await?;
        insert_secret_audit(
            &mut transaction,
            access,
            "question_session_continuation_accessed",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedQuestionSessionContinuation {
            metadata,
            latest_operation,
            value: SecretValue::new(plaintext),
        }))
    }

    async fn issue_question_session_operation(
        &self,
        request: QuestionSessionOperationIssueRequest,
    ) -> Result<QuestionSessionOperationIssueOutcome, SecretStoreError> {
        validate_issue_request(&request)?;
        if !label_belongs_to_provider(&self.provider_id, &request.operation_type) {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(binding) = fetch_operation_binding(&mut transaction, request.execution_id).await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationIssueOutcome::Unavailable);
        };
        if binding.provider_id != self.provider_id
            || binding.session_state != QuestionSessionState::Claimed
            || binding.continuation_revision != request.expected_continuation_revision
            || request.issued_at < binding.continuation_updated_at
            || request.issued_at >= binding.expires_at
            || !binding.execution_live
            || !attempt_is_live(
                &mut transaction,
                request.execution_id,
                request.execution_attempt_id,
            )
            .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationIssueOutcome::Unavailable);
        }
        if let Some(existing) = fetch_operation_for_revision(
            &mut transaction,
            binding.session_id,
            request.expected_continuation_revision,
        )
        .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(if operation_matches_issue(&existing, &request) {
                QuestionSessionOperationIssueOutcome::Duplicate(existing)
            } else {
                QuestionSessionOperationIssueOutcome::Conflict
            });
        }
        let last_sequence: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM question_session_operations WHERE session_id = ?",
        )
        .bind(binding.session_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let sequence = u64::try_from(last_sequence.map_or(1, |value| value.saturating_add(1)))
            .map_err(|_| SecretStoreError::Storage)?;
        let operation = QuestionSessionOperation {
            session_id: binding.session_id,
            sequence,
            execution_id: request.execution_id,
            execution_attempt_id: request.execution_attempt_id,
            continuation_revision: request.expected_continuation_revision,
            operation_type: request.operation_type,
            request_digest: request.request_digest,
            state: QuestionSessionOperationState::Issued,
            result_digest: None,
            issued_at: request.issued_at,
            completed_at: None,
        };
        insert_operation(&mut transaction, &operation).await?;
        insert_operation_audit(
            &mut transaction,
            &request.correlation_id,
            "question_session_operation_issued",
            &operation,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(QuestionSessionOperationIssueOutcome::Issued(operation))
    }

    async fn accept_question_session_operation(
        &self,
        request: QuestionSessionOperationAcceptRequest<'_>,
    ) -> Result<QuestionSessionOperationFinishOutcome, SecretStoreError> {
        validate_accept_request(&request)?;
        if !label_belongs_to_provider(&self.provider_id, request.next_continuation_type) {
            return Err(SecretStoreError::InvalidValue);
        }
        validate_secret(&request.replacement)?;
        let next_digest = digest(request.replacement.expose_secret());
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(existing) = fetch_operation(
            &mut transaction,
            request.operation.session_id,
            request.operation.sequence,
        )
        .await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationFinishOutcome::Unavailable);
        };
        if !matches!(
            existing.state,
            QuestionSessionOperationState::Issued | QuestionSessionOperationState::Ambiguous
        ) {
            let same_rotation = if existing.state == QuestionSessionOperationState::Accepted
                && existing.result_digest == Some(request.result_digest)
            {
                fetch_continuation(&mut transaction, existing.session_id)
                    .await?
                    .is_some_and(|continuation| {
                        continuation.execution_id == Some(existing.execution_id)
                            && continuation.revision
                                == existing.continuation_revision.saturating_add(1)
                            && continuation.continuation_type == request.next_continuation_type
                            && continuation.continuation_digest == next_digest
                            && continuation.phase == request.next_phase
                            && continuation.updated_at == request.accepted_at
                    })
            } else {
                false
            };
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(if same_rotation {
                QuestionSessionOperationFinishOutcome::Duplicate(existing)
            } else {
                QuestionSessionOperationFinishOutcome::Conflict
            });
        }
        if &existing != request.operation {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationFinishOutcome::Conflict);
        }
        let Some(binding) =
            fetch_operation_binding(&mut transaction, existing.execution_id).await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationFinishOutcome::Unavailable);
        };
        authorize_scoped(
            binding.owner_user_id,
            &binding.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if binding.session_id != existing.session_id
            || binding.session_state != QuestionSessionState::Claimed
            || binding.continuation_revision != existing.continuation_revision
        {
            return Ok(QuestionSessionOperationFinishOutcome::Conflict);
        }
        let secret_id = binding.secret_id;
        let stored = fetch_secret(&mut transaction, secret_id).await?;
        if stored.owner_user_id != binding.owner_user_id
            || stored.purpose != SecretPurpose::BrowserJobCredential
            || stored.version != binding.continuation_revision
        {
            return Ok(QuestionSessionOperationFinishOutcome::Conflict);
        }
        let next_revision = stored
            .version
            .checked_add(1)
            .ok_or(SecretStoreError::VersionConflict)?;
        let (key_id, key) = self.keyring.active();
        let rotated = SecretRef {
            id: secret_id,
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
        .bind(i64::from(rotated.version))
        .bind(encode_timestamp(rotated.updated_at))
        .bind(rotated.id.to_string())
        .bind(i64::from(stored.version))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if secret_updated.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        let continuation_updated = sqlx::query(
            "UPDATE question_session_continuations SET continuation_type = ?, \
             continuation_digest = ?, phase = ?, revision = ?, updated_at = ? \
             WHERE session_id = ? AND execution_id = ? AND revision = ?",
        )
        .bind(request.next_continuation_type)
        .bind(next_digest.as_slice())
        .bind(request.next_phase)
        .bind(i64::from(next_revision))
        .bind(encode_timestamp(request.accepted_at))
        .bind(existing.session_id.to_string())
        .bind(existing.execution_id.to_string())
        .bind(i64::from(existing.continuation_revision))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if continuation_updated.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        let expected_state = existing.state;
        let mut accepted = existing;
        accepted.state = QuestionSessionOperationState::Accepted;
        accepted.result_digest = Some(request.result_digest);
        accepted.completed_at = Some(request.accepted_at);
        persist_operation_finish(&mut transaction, &accepted, expected_state).await?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "question_session_continuation_rotated",
            &rotated,
        )
        .await
        .map_err(storage_error)?;
        let continuation = QuestionSessionContinuation {
            session_id: accepted.session_id,
            execution_id: Some(accepted.execution_id),
            continuation_type: request.next_continuation_type.to_owned(),
            continuation_digest: next_digest,
            phase: request.next_phase.to_owned(),
            revision: next_revision,
            created_at: binding.continuation_created_at,
            updated_at: request.accepted_at,
        };
        insert_operation_audit(
            &mut transaction,
            &request.access.correlation_id,
            "question_session_operation_accepted",
            &accepted,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(QuestionSessionOperationFinishOutcome::Accepted {
            operation: accepted,
            continuation,
        })
    }

    async fn finish_question_session_operation(
        &self,
        operation: &QuestionSessionOperation,
        terminal_state: QuestionSessionOperationState,
        result_digest: Option<[u8; 32]>,
        completed_at: Timestamp,
        correlation_id: &str,
    ) -> Result<QuestionSessionOperationFinishOutcome, SecretStoreError> {
        validate_terminal_finish(terminal_state, result_digest, completed_at, operation)?;
        validate_correlation_id(correlation_id)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(existing) =
            fetch_operation(&mut transaction, operation.session_id, operation.sequence).await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationFinishOutcome::Unavailable);
        };
        if existing.state != QuestionSessionOperationState::Issued {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(
                if existing.state == terminal_state && existing.result_digest == result_digest {
                    QuestionSessionOperationFinishOutcome::Duplicate(existing)
                } else {
                    QuestionSessionOperationFinishOutcome::Conflict
                },
            );
        }
        if &existing != operation {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(QuestionSessionOperationFinishOutcome::Conflict);
        }
        let mut finished = existing;
        finished.state = terminal_state;
        finished.result_digest = result_digest;
        finished.completed_at = Some(completed_at);
        persist_operation_finish(
            &mut transaction,
            &finished,
            QuestionSessionOperationState::Issued,
        )
        .await?;
        insert_operation_audit(
            &mut transaction,
            correlation_id,
            match terminal_state {
                QuestionSessionOperationState::Rejected => "question_session_operation_rejected",
                QuestionSessionOperationState::Ambiguous => "question_session_operation_ambiguous",
                QuestionSessionOperationState::Issued | QuestionSessionOperationState::Accepted => {
                    unreachable!()
                }
            },
            &finished,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(QuestionSessionOperationFinishOutcome::Finished(finished))
    }
}

#[derive(Debug)]
struct ArtifactSession {
    owner_user_id: UserId,
    provider_id: ProviderId,
    artifact_type: String,
    artifact_digest: [u8; 32],
    state: QuestionSessionState,
    created_at: Timestamp,
    expires_at: Timestamp,
}

#[derive(Debug)]
struct OperationBinding {
    session_id: QuestionSessionId,
    owner_user_id: UserId,
    provider_id: ProviderId,
    session_state: QuestionSessionState,
    expires_at: Timestamp,
    secret_id: SecretId,
    continuation_revision: u32,
    continuation_created_at: Timestamp,
    continuation_updated_at: Timestamp,
    execution_live: bool,
}

async fn fetch_artifact_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: QuestionSessionId,
) -> Result<Option<ArtifactSession>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT owner_user_id, provider_id, artifact_type, artifact_digest, state, \
                created_at, expires_at FROM question_sessions WHERE id = ?",
    )
    .bind(session_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.as_ref().map(decode_artifact_session).transpose()
}

fn decode_artifact_session(row: &SqliteRow) -> Result<ArtifactSession, SecretStoreError> {
    Ok(ArtifactSession {
        owner_user_id: parse_id(row_value!(row, "owner_user_id"))?,
        provider_id: ProviderId::new(row_value!(row, "provider_id", String))
            .map_err(|_| SecretStoreError::Storage)?,
        artifact_type: row_value!(row, "artifact_type"),
        artifact_digest: decode_digest(row_value!(row, "artifact_digest"))?,
        state: decode_session_state(row_value!(row, "state"))?,
        created_at: decode_timestamp(row_value!(row, "created_at"))?,
        expires_at: decode_timestamp(row_value!(row, "expires_at"))?,
    })
}

async fn fetch_continuation(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: QuestionSessionId,
) -> Result<Option<QuestionSessionContinuation>, SecretStoreError> {
    sqlx::query(
        "SELECT session_id, execution_id, continuation_type, continuation_digest, phase, \
                revision, created_at, updated_at \
         FROM question_session_continuations WHERE session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_continuation)
    .transpose()
}

async fn fetch_resolved_row(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
) -> Result<Option<SqliteRow>, SecretStoreError> {
    sqlx::query(
        "SELECT continuation.session_id, continuation.execution_id, \
                continuation.secret_blob_id, continuation.continuation_type, \
                continuation.continuation_digest, continuation.phase, continuation.revision, \
                continuation.created_at, continuation.updated_at, \
                session.owner_user_id, session.provider_id, session.state AS session_state, \
                session.execution_id AS session_execution_id \
         FROM question_session_continuations AS continuation \
         INNER JOIN question_sessions AS session ON session.id = continuation.session_id \
         WHERE continuation.execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)
}

fn decode_continuation(row: &SqliteRow) -> Result<QuestionSessionContinuation, SecretStoreError> {
    Ok(QuestionSessionContinuation {
        session_id: parse_id(row_value!(row, "session_id"))?,
        execution_id: row_value!(row, "execution_id", Option<&str>)
            .map(parse_id)
            .transpose()?,
        continuation_type: row_value!(row, "continuation_type"),
        continuation_digest: decode_digest(row_value!(row, "continuation_digest"))?,
        phase: row_value!(row, "phase"),
        revision: u32::try_from(row_value!(row, "revision", i64))
            .map_err(|_| SecretStoreError::Storage)?,
        created_at: decode_timestamp(row_value!(row, "created_at"))?,
        updated_at: decode_timestamp(row_value!(row, "updated_at"))?,
    })
}

async fn fetch_operation_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
) -> Result<Option<OperationBinding>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT session.id AS session_id, session.owner_user_id, session.provider_id, \
                session.state AS session_state, session.expires_at, continuation.secret_blob_id, \
                continuation.revision, continuation.created_at, continuation.updated_at, \
                execution.state AS execution_state \
         FROM question_sessions AS session \
         INNER JOIN question_session_continuations AS continuation \
            ON continuation.session_id = session.id AND continuation.execution_id = session.execution_id \
         INNER JOIN executions AS execution ON execution.id = session.execution_id \
         WHERE session.execution_id = ?",
    )
    .bind(execution_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.as_ref()
        .map(|row| {
            Ok(OperationBinding {
                session_id: parse_id(row_value!(row, "session_id"))?,
                owner_user_id: parse_id(row_value!(row, "owner_user_id"))?,
                provider_id: ProviderId::new(row_value!(row, "provider_id", String))
                    .map_err(|_| SecretStoreError::Storage)?,
                session_state: decode_session_state(row_value!(row, "session_state"))?,
                expires_at: decode_timestamp(row_value!(row, "expires_at"))?,
                secret_id: parse_id(row_value!(row, "secret_blob_id"))?,
                continuation_revision: u32::try_from(row_value!(row, "revision", i64))
                    .map_err(|_| SecretStoreError::Storage)?,
                continuation_created_at: decode_timestamp(row_value!(row, "created_at"))?,
                continuation_updated_at: decode_timestamp(row_value!(row, "updated_at"))?,
                execution_live: matches!(
                    row_value!(row, "execution_state"),
                    "running" | "recovering" | "retry_waiting"
                ),
            })
        })
        .transpose()
}

async fn attempt_is_live(
    transaction: &mut Transaction<'_, Sqlite>,
    execution_id: ExecutionId,
    execution_attempt_id: ExecutionAttemptId,
) -> Result<bool, SecretStoreError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM execution_attempts \
         WHERE id = ? AND execution_id = ? AND finished_at IS NULL AND result IS NULL)",
    )
    .bind(execution_attempt_id.to_string())
    .bind(execution_id.to_string())
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)
}

async fn insert_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &QuestionSessionOperation,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO question_session_operations \
         (session_id, sequence, execution_id, execution_attempt_id, continuation_revision, \
          operation_type, request_digest, state, issued_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'issued', ?)",
    )
    .bind(operation.session_id.to_string())
    .bind(i64::try_from(operation.sequence).map_err(|_| SecretStoreError::InvalidValue)?)
    .bind(operation.execution_id.to_string())
    .bind(operation.execution_attempt_id.to_string())
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
    session_id: QuestionSessionId,
    revision: u32,
) -> Result<Option<QuestionSessionOperation>, SecretStoreError> {
    sqlx::query(
        "SELECT session_id, sequence, execution_id, execution_attempt_id, continuation_revision, \
                operation_type, request_digest, state, result_digest, issued_at, completed_at \
         FROM question_session_operations WHERE session_id = ? AND continuation_revision = ?",
    )
    .bind(session_id.to_string())
    .bind(i64::from(revision))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_operation)
    .transpose()
}

async fn fetch_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: QuestionSessionId,
    sequence: u64,
) -> Result<Option<QuestionSessionOperation>, SecretStoreError> {
    sqlx::query(
        "SELECT session_id, sequence, execution_id, execution_attempt_id, continuation_revision, \
                operation_type, request_digest, state, result_digest, issued_at, completed_at \
         FROM question_session_operations WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(i64::try_from(sequence).map_err(|_| SecretStoreError::InvalidValue)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_operation)
    .transpose()
}

async fn fetch_latest_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: QuestionSessionId,
) -> Result<Option<QuestionSessionOperation>, SecretStoreError> {
    sqlx::query(
        "SELECT session_id, sequence, execution_id, execution_attempt_id, continuation_revision, \
                operation_type, request_digest, state, result_digest, issued_at, completed_at \
         FROM question_session_operations WHERE session_id = ? ORDER BY sequence DESC LIMIT 1",
    )
    .bind(session_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(decode_operation)
    .transpose()
}

fn decode_operation(row: &SqliteRow) -> Result<QuestionSessionOperation, SecretStoreError> {
    Ok(QuestionSessionOperation {
        session_id: parse_id(row_value!(row, "session_id"))?,
        sequence: u64::try_from(row_value!(row, "sequence", i64))
            .map_err(|_| SecretStoreError::Storage)?,
        execution_id: parse_id(row_value!(row, "execution_id"))?,
        execution_attempt_id: parse_id(row_value!(row, "execution_attempt_id"))?,
        continuation_revision: u32::try_from(row_value!(row, "continuation_revision", i64))
            .map_err(|_| SecretStoreError::Storage)?,
        operation_type: row_value!(row, "operation_type"),
        request_digest: decode_digest(row_value!(row, "request_digest"))?,
        state: decode_operation_state(row_value!(row, "state"))?,
        result_digest: row_value!(row, "result_digest", Option<Vec<u8>>)
            .map(decode_digest)
            .transpose()?,
        issued_at: decode_timestamp(row_value!(row, "issued_at"))?,
        completed_at: row_value!(row, "completed_at", Option<&str>)
            .map(decode_timestamp)
            .transpose()?,
    })
}

async fn persist_operation_finish(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &QuestionSessionOperation,
    expected_state: QuestionSessionOperationState,
) -> Result<(), SecretStoreError> {
    let result = sqlx::query(
        "UPDATE question_session_operations SET state = ?, result_digest = ?, completed_at = ? \
         WHERE session_id = ? AND sequence = ? AND state = ?",
    )
    .bind(operation_state_name(operation.state))
    .bind(operation.result_digest.map(|digest| digest.to_vec()))
    .bind(operation.completed_at.map(encode_timestamp))
    .bind(operation.session_id.to_string())
    .bind(i64::try_from(operation.sequence).map_err(|_| SecretStoreError::InvalidValue)?)
    .bind(operation_state_name(expected_state))
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
    continuation: &QuestionSessionContinuation,
) -> Result<(), SecretStoreError> {
    let (actor_type, actor_id) = access_actor(&access.actor);
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'question_session', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(continuation.updated_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(continuation.session_id.to_string())
    .bind(&access.correlation_id)
    .bind(
        serde_json::json!({
            "execution_id": continuation.execution_id,
            "continuation_type": continuation.continuation_type,
            "continuation_digest": "[HASHED]",
            "phase": continuation.phase,
            "revision": continuation.revision,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn insert_operation_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    correlation_id: &str,
    action: &str,
    operation: &QuestionSessionOperation,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'execution', ?, ?, 'question_session_operation', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(
        operation.completed_at.unwrap_or(operation.issued_at),
    ))
    .bind(operation.execution_id.to_string())
    .bind(action)
    .bind(format!("{}:{}", operation.session_id, operation.sequence))
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "session_id": operation.session_id,
            "sequence": operation.sequence,
            "execution_attempt_id": operation.execution_attempt_id,
            "continuation_revision": operation.continuation_revision,
            "operation_type": operation.operation_type,
            "request_digest": "[HASHED]",
            "state": operation_state_name(operation.state),
            "result_digest": operation.result_digest.map(|_| "[HASHED]"),
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn validate_issue_request(
    request: &QuestionSessionOperationIssueRequest,
) -> Result<(), SecretStoreError> {
    validate_label(&request.operation_type)?;
    validate_correlation_id(&request.correlation_id)?;
    if request.expected_continuation_revision == 0 || request.request_digest == [0; 32] {
        return Err(SecretStoreError::InvalidValue);
    }
    Ok(())
}

fn validate_accept_request(
    request: &QuestionSessionOperationAcceptRequest<'_>,
) -> Result<(), SecretStoreError> {
    validate_label(request.next_continuation_type)?;
    validate_label(request.next_phase)?;
    let operation_shape_valid = match request.operation.state {
        QuestionSessionOperationState::Issued => request.operation.completed_at.is_none(),
        QuestionSessionOperationState::Ambiguous => request.operation.completed_at.is_some(),
        QuestionSessionOperationState::Accepted | QuestionSessionOperationState::Rejected => false,
    };
    if !operation_shape_valid
        || request.operation.result_digest.is_some()
        || request.result_digest == [0; 32]
        || request.accepted_at < request.operation.issued_at
    {
        return Err(SecretStoreError::InvalidValue);
    }
    Ok(())
}

fn validate_terminal_finish(
    state: QuestionSessionOperationState,
    result_digest: Option<[u8; 32]>,
    completed_at: Timestamp,
    operation: &QuestionSessionOperation,
) -> Result<(), SecretStoreError> {
    let shape_valid = match state {
        QuestionSessionOperationState::Rejected => {
            result_digest.is_some_and(|value| value != [0; 32])
        }
        QuestionSessionOperationState::Ambiguous => result_digest.is_none(),
        QuestionSessionOperationState::Issued | QuestionSessionOperationState::Accepted => false,
    };
    if !shape_valid
        || operation.state != QuestionSessionOperationState::Issued
        || operation.result_digest.is_some()
        || operation.completed_at.is_some()
        || completed_at < operation.issued_at
    {
        return Err(SecretStoreError::InvalidValue);
    }
    Ok(())
}

fn operation_matches_issue(
    operation: &QuestionSessionOperation,
    request: &QuestionSessionOperationIssueRequest,
) -> bool {
    operation.execution_id == request.execution_id
        && operation.execution_attempt_id == request.execution_attempt_id
        && operation.continuation_revision == request.expected_continuation_revision
        && operation.operation_type == request.operation_type
        && operation.request_digest == request.request_digest
        && operation.issued_at == request.issued_at
}

fn authorize_scoped(
    owner_user_id: UserId,
    actual_provider: &ProviderId,
    scoped_provider: &ProviderId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    authorize(owner_user_id, access)?;
    let actor_matches = match &access.actor {
        SecretActor::ProviderRuntime(provider) => provider == scoped_provider.as_str(),
        SecretActor::CoreService(_) => true,
        SecretActor::User(_) | SecretActor::ServiceToken(_) => false,
    };
    if actual_provider == scoped_provider && actor_matches {
        Ok(())
    } else {
        Err(SecretStoreError::Unauthorized)
    }
}

fn validate_label(value: &str) -> Result<(), SecretStoreError> {
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        Err(SecretStoreError::InvalidValue)
    } else {
        Ok(())
    }
}

fn label_belongs_to_provider(provider_id: &ProviderId, value: &str) -> bool {
    value
        .strip_prefix(provider_id.as_str())
        .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
}

fn validate_correlation_id(value: &str) -> Result<(), SecretStoreError> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(SecretStoreError::InvalidValue)
    } else {
        Ok(())
    }
}

fn decode_session_state(value: &str) -> Result<QuestionSessionState, SecretStoreError> {
    match value {
        "active" => Ok(QuestionSessionState::Active),
        "claimed" => Ok(QuestionSessionState::Claimed),
        "consumed" => Ok(QuestionSessionState::Consumed),
        "cancelled" => Ok(QuestionSessionState::Cancelled),
        "expired" => Ok(QuestionSessionState::Expired),
        _ => Err(SecretStoreError::Storage),
    }
}

fn operation_state_name(state: QuestionSessionOperationState) -> &'static str {
    match state {
        QuestionSessionOperationState::Issued => "issued",
        QuestionSessionOperationState::Accepted => "accepted",
        QuestionSessionOperationState::Rejected => "rejected",
        QuestionSessionOperationState::Ambiguous => "ambiguous",
    }
}

fn decode_operation_state(value: &str) -> Result<QuestionSessionOperationState, SecretStoreError> {
    match value {
        "issued" => Ok(QuestionSessionOperationState::Issued),
        "accepted" => Ok(QuestionSessionOperationState::Accepted),
        "rejected" => Ok(QuestionSessionOperationState::Rejected),
        "ambiguous" => Ok(QuestionSessionOperationState::Ambiguous),
        _ => Err(SecretStoreError::Storage),
    }
}

fn access_actor(actor: &SecretActor) -> (&'static str, Option<String>) {
    match actor {
        SecretActor::User(id) => ("user", Some(id.to_string())),
        SecretActor::ServiceToken(id) => ("service_token", Some(id.to_string())),
        SecretActor::CoreService(service) => ("core_service", Some((*service).to_owned())),
        SecretActor::ProviderRuntime(provider) => ("provider_runtime", Some(provider.clone())),
    }
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn decode_digest(value: Vec<u8>) -> Result<[u8; 32], SecretStoreError> {
    value.try_into().map_err(|_| SecretStoreError::Storage)
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

fn storage_error<T>(_error: T) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_domain::{
        AuditActor, ProviderAccountId, QuestionSession, QuestionSnapshotId, TaskId,
    };
    use asterism_secrets::{SecretKey, SecretStoreError};
    use chrono::{Duration, Utc};

    use super::*;
    use crate::{
        QuestionSessionClaimOutcome, QuestionSessionRepository, SqliteQuestionSessionRepository,
    };

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn encrypted_continuation_rotates_only_after_accepted_operation() {
        let fixture = Fixture::new().await;
        let initial = b"exam-enc-v1";
        let (session, execution_id, attempt_id) = fixture.claimed(initial).await;
        let access = fixture.access("continuation-resolve");

        let resolved = fixture
            .artifacts
            .resolve_question_session_continuation(execution_id, &access)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.value.expose_secret(), initial);
        assert_eq!(resolved.metadata.revision, 1);
        assert!(resolved.latest_operation.is_none());

        let issue = QuestionSessionOperationIssueRequest {
            execution_id,
            execution_attempt_id: attempt_id,
            expected_continuation_revision: 1,
            operation_type: "chaoxing.exam.temp-save".to_owned(),
            request_digest: [8; 32],
            issued_at: fixture.now + Duration::seconds(2),
            correlation_id: "exam-save-1".to_owned(),
        };
        let QuestionSessionOperationIssueOutcome::Issued(operation) = fixture
            .artifacts
            .issue_question_session_operation(issue.clone())
            .await
            .unwrap()
        else {
            panic!("expected issued operation");
        };
        assert!(matches!(
            fixture
                .artifacts
                .issue_question_session_operation(issue)
                .await
                .unwrap(),
            QuestionSessionOperationIssueOutcome::Duplicate(existing) if existing == operation
        ));

        let replacement = b"exam-enc-v2";
        let accepted_at = fixture.now + Duration::seconds(3);
        let outcome = fixture
            .artifacts
            .accept_question_session_operation(QuestionSessionOperationAcceptRequest {
                operation: &operation,
                next_continuation_type: "chaoxing.exam-attempt.v1",
                next_phase: "answer_saved",
                replacement: SecretValue::new(replacement.to_vec()),
                result_digest: [9; 32],
                accepted_at,
                access: &fixture.access("continuation-rotate"),
            })
            .await
            .unwrap();
        let QuestionSessionOperationFinishOutcome::Accepted {
            operation: accepted,
            continuation,
        } = outcome
        else {
            panic!("expected atomic continuation rotation");
        };
        assert_eq!(accepted.state, QuestionSessionOperationState::Accepted);
        assert_eq!(continuation.revision, 2);
        assert_eq!(continuation.updated_at, accepted_at);
        assert!(matches!(
            fixture
                .artifacts
                .accept_question_session_operation(QuestionSessionOperationAcceptRequest {
                    operation: &operation,
                    next_continuation_type: "chaoxing.exam-attempt.v1",
                    next_phase: "answer_saved",
                    replacement: SecretValue::new(replacement.to_vec()),
                    result_digest: [9; 32],
                    accepted_at,
                    access: &fixture.access("continuation-rotate-replay"),
                })
                .await
                .unwrap(),
            QuestionSessionOperationFinishOutcome::Duplicate(existing)
                if existing == accepted
        ));
        assert_eq!(
            fixture
                .artifacts
                .accept_question_session_operation(QuestionSessionOperationAcceptRequest {
                    operation: &operation,
                    next_continuation_type: "chaoxing.exam-attempt.v1",
                    next_phase: "answer_saved",
                    replacement: SecretValue::new(b"substituted-state".to_vec()),
                    result_digest: [9; 32],
                    accepted_at,
                    access: &fixture.access("continuation-rotate-conflict"),
                })
                .await
                .unwrap(),
            QuestionSessionOperationFinishOutcome::Conflict
        );

        let resolved = fixture
            .artifacts
            .resolve_question_session_continuation(
                execution_id,
                &fixture.access("continuation-resolve-v2"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.value.expose_secret(), replacement);
        assert_eq!(resolved.metadata.revision, 2);
        assert_eq!(resolved.metadata.phase, "answer_saved");
        assert_eq!(resolved.metadata.session_id, session.id);
        assert_eq!(resolved.latest_operation, Some(accepted));
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression keeps issue, ambiguity, replay rejection, verified rotation and provider scoping in one lifecycle"
    )]
    async fn ambiguity_blocks_replay_but_verified_recovery_can_rotate_state() {
        let fixture = Fixture::new().await;
        let (_, execution_id, attempt_id) = fixture.claimed(b"cidaren-topic-v1").await;
        let issue = QuestionSessionOperationIssueRequest {
            execution_id,
            execution_attempt_id: attempt_id,
            expected_continuation_revision: 1,
            operation_type: "chaoxing.exam.submit".to_owned(),
            request_digest: [4; 32],
            issued_at: fixture.now + Duration::seconds(2),
            correlation_id: "cidaren-verify-1".to_owned(),
        };
        let QuestionSessionOperationIssueOutcome::Issued(operation) = fixture
            .artifacts
            .issue_question_session_operation(issue)
            .await
            .unwrap()
        else {
            panic!("expected issued operation");
        };
        let finished = fixture
            .artifacts
            .finish_question_session_operation(
                &operation,
                QuestionSessionOperationState::Ambiguous,
                None,
                fixture.now + Duration::seconds(3),
                "cidaren-ambiguous",
            )
            .await
            .unwrap();
        let QuestionSessionOperationFinishOutcome::Finished(ambiguous) = finished else {
            panic!("expected ambiguous operation");
        };
        assert_eq!(ambiguous.state, QuestionSessionOperationState::Ambiguous);
        assert_eq!(
            fixture
                .artifacts
                .issue_question_session_operation(QuestionSessionOperationIssueRequest {
                    execution_id,
                    execution_attempt_id: attempt_id,
                    expected_continuation_revision: 1,
                    operation_type: "chaoxing.exam.submit".to_owned(),
                    request_digest: [5; 32],
                    issued_at: fixture.now + Duration::seconds(4),
                    correlation_id: "cidaren-replay".to_owned(),
                })
                .await
                .unwrap(),
            QuestionSessionOperationIssueOutcome::Conflict
        );
        let recovery = fixture
            .artifacts
            .accept_question_session_operation(QuestionSessionOperationAcceptRequest {
                operation: &ambiguous,
                next_continuation_type: "chaoxing.exam-attempt.v1",
                next_phase: "submission-readback-confirmed",
                replacement: SecretValue::new(b"cidaren-topic-v2".to_vec()),
                result_digest: [6; 32],
                accepted_at: fixture.now + Duration::seconds(5),
                access: &fixture.access("cidaren-recovery-accepted"),
            })
            .await
            .unwrap();
        assert!(matches!(
            recovery,
            QuestionSessionOperationFinishOutcome::Accepted {
                operation,
                continuation,
            } if operation.state == QuestionSessionOperationState::Accepted
                && continuation.revision == 2
        ));

        let foreign = SqliteQuestionSessionArtifactRepository::new(
            fixture.database.clone(),
            fixture.keyring.clone(),
            ProviderId::new("uai").unwrap(),
        );
        assert!(matches!(
            foreign
                .resolve_question_session_continuation(
                    execution_id,
                    &SecretAccess {
                        actor: SecretActor::ProviderRuntime("uai".to_owned()),
                        correlation_id: "foreign-resolve".to_owned(),
                        reason: "provider execution".to_owned(),
                    },
                )
                .await,
            Err(SecretStoreError::Unauthorized)
        ));

        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = X'00' WHERE id = ( \
             SELECT secret_blob_id FROM question_session_continuations WHERE execution_id = ?)",
        )
        .bind(execution_id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .artifacts
                .resolve_question_session_continuation(
                    execution_id,
                    &fixture.access("tampered-resolve"),
                )
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    struct Fixture {
        database: Database,
        keyring: Arc<SecretKeyring>,
        sessions: SqliteQuestionSessionRepository,
        artifacts: SqliteQuestionSessionArtifactRepository,
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        snapshot: QuestionSnapshotId,
        provider: ProviderId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let mut keys = BTreeMap::new();
            keys.insert("question-key".to_owned(), SecretKey::new([42; 32]));
            let keyring = Arc::new(SecretKeyring::new("question-key", keys).unwrap());
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
                 VALUES (?, 'artifact-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'Artifact Account', '\"authenticated\"', ?, ?)",
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
                 VALUES (?, ?, 'exam-artifact', 'v1:artifact', 'exam', 'unknown', 'Exam', \
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
            let sessions = SqliteQuestionSessionRepository::new(database.clone());
            let artifacts = SqliteQuestionSessionArtifactRepository::new(
                database.clone(),
                keyring.clone(),
                provider.clone(),
            );
            Self {
                database,
                keyring,
                sessions,
                artifacts,
                owner,
                account,
                task,
                snapshot,
                provider,
                now,
            }
        }

        async fn claimed(
            &self,
            initial: &[u8],
        ) -> (QuestionSession, ExecutionId, ExecutionAttemptId) {
            let session = QuestionSession::active(
                self.owner,
                self.account,
                self.task,
                self.provider.clone(),
                "exam-v1".to_owned(),
                self.snapshot,
                "chaoxing.exam-attempt.v1".to_owned(),
                digest(initial),
                self.now,
                self.now + Duration::minutes(5),
            )
            .unwrap();
            self.sessions
                .create_question_session(
                    &session,
                    AuditActor::User(self.owner),
                    "artifact-session-create",
                )
                .await
                .unwrap();
            self.artifacts
                .attach_question_session_artifact(QuestionSessionArtifactAttachRequest {
                    session_id: session.id,
                    phase: "questions_ready",
                    value: SecretValue::new(initial.to_vec()),
                    attached_at: self.now,
                    access: &self.access("artifact-attach"),
                })
                .await
                .unwrap();
            let (execution_id, attempt_id) = self.execution().await;
            assert!(matches!(
                self.sessions
                    .claim_question_session_for_execution(
                        execution_id,
                        self.now + Duration::seconds(1),
                        "artifact-session-claim",
                    )
                    .await
                    .unwrap(),
                QuestionSessionClaimOutcome::Claimed(_)
            ));
            (session, execution_id, attempt_id)
        }

        async fn execution(&self) -> (ExecutionId, ExecutionAttemptId) {
            let draft_id = asterism_domain::SubmissionDraftId::new();
            let execution_id = ExecutionId::new();
            let attempt_id = ExecutionAttemptId::new();
            let timestamp = encode_timestamp(self.now);
            sqlx::query(
                "INSERT INTO submission_drafts \
                 (id, question_snapshot_id, task_id, provider_id, provider_version, \
                  payload_preview_json, preview_bytes, item_count, created_at) \
                 VALUES (?, ?, ?, ?, 'builder-v1', '{}', 2, 1, ?)",
            )
            .bind(draft_id.to_string())
            .bind(self.snapshot.to_string())
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
            sqlx::query(
                "INSERT INTO execution_attempts \
                 (id, execution_id, attempt_no, started_at) VALUES (?, ?, 1, ?)",
            )
            .bind(attempt_id.to_string())
            .bind(execution_id.to_string())
            .bind(&timestamp)
            .execute(self.database.pool())
            .await
            .unwrap();
            (execution_id, attempt_id)
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
