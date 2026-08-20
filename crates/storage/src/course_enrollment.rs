use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, CourseEnrollmentAttempt, CourseEnrollmentAttemptId,
    CourseEnrollmentAttemptState, CourseEnrollmentDraft, CourseEnrollmentDraftId,
    CourseEnrollmentMutationReceipt, CourseEnrollmentVerification, ProviderAccountId, ProviderId,
    SecretId, Timestamp, UserId,
};
use asterism_provider_api::ProviderCourseEnrollmentDraft;
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    CourseEnrollmentAttemptCreateOutcome, CourseEnrollmentAttemptCreateRequest,
    CourseEnrollmentAttemptMutationIssueRequest, CourseEnrollmentAttemptReceiptRequest,
    CourseEnrollmentAttemptRepository, CourseEnrollmentAttemptVerificationBeginRequest,
    CourseEnrollmentAttemptVerificationRecordRequest, CourseEnrollmentDraftCreateOutcome,
    CourseEnrollmentDraftCreateRequest, CourseEnrollmentDraftRecord,
    CourseEnrollmentDraftRepository, CourseEnrollmentDraftResolveRequest, Database,
    ResolvedCourseEnrollmentDraft, SecretKeyring, StorageError,
    secret::{decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob},
};

#[derive(Clone, Debug)]
pub struct SqliteCourseEnrollmentDraftRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteCourseEnrollmentDraftRepository {
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
impl CourseEnrollmentDraftRepository for SqliteCourseEnrollmentDraftRepository {
    async fn create_course_enrollment_draft(
        &self,
        request: CourseEnrollmentDraftCreateRequest<'_>,
    ) -> Result<CourseEnrollmentDraftCreateOutcome, SecretStoreError> {
        validate_context(request.correlation_id, request.access)?;
        if request.provider_draft.provider_id() != &self.provider_id {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        authorize_account(
            &mut transaction,
            request.owner_user_id,
            request.provider_account_id,
            &self.provider_id,
            request.access,
        )
        .await?;
        let mut candidate = record_from_request(&request)?;
        if let Some(existing) = fetch_record(&mut transaction, request.draft_id).await? {
            transaction.rollback().await.map_err(storage_error)?;
            return if records_match(&existing, &candidate) {
                Ok(CourseEnrollmentDraftCreateOutcome::AlreadyExists(existing))
            } else {
                Err(SecretStoreError::VersionConflict)
            };
        }
        if let Some(existing) = fetch_record_by_request(
            &mut transaction,
            request.owner_user_id,
            request.provider_account_id,
            request.provider_draft.request_digest(),
        )
        .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(CourseEnrollmentDraftCreateOutcome::AlreadyExists(existing));
        }

        let (key_id, key) = self.keyring.active();
        let secret = enrollment_secret(request.owner_user_id, key_id, request.created_at);
        candidate.draft.artifact_secret_id = secret.id;
        let (nonce, encrypted) = encrypt(
            key,
            &secret,
            request.provider_draft.request().expose_secret(),
        )?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted).await?;
        sqlx::query(
            "INSERT INTO course_enrollment_drafts \
             (id, owner_user_id, provider_account_id, provider_id, artifact_type, \
              remote_course_id, remote_class_id, preview_digest, preview_sanitized_json, \
              request_digest, request_secret_blob_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(candidate.draft.id.to_string())
        .bind(candidate.draft.owner_user_id.to_string())
        .bind(candidate.draft.provider_account_id.to_string())
        .bind(candidate.draft.provider_id.as_str())
        .bind(&candidate.artifact_type)
        .bind(&candidate.draft.remote_course_id)
        .bind(&candidate.draft.remote_class_id)
        .bind(candidate.draft.preview_digest.to_vec())
        .bind(candidate.preview_sanitized.to_string())
        .bind(candidate.draft.request_digest.to_vec())
        .bind(secret.id.to_string())
        .bind(encode_timestamp(candidate.draft.created_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "course_enrollment_request_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_draft_audit(
            &mut transaction,
            &candidate,
            request.correlation_id,
            "course_enrollment_draft_created",
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(CourseEnrollmentDraftCreateOutcome::Created(candidate))
    }

    async fn resolve_course_enrollment_draft(
        &self,
        request: CourseEnrollmentDraftResolveRequest<'_>,
    ) -> Result<Option<ResolvedCourseEnrollmentDraft>, SecretStoreError> {
        validate_context(request.correlation_id, request.access)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        authorize_account(
            &mut transaction,
            request.owner_user_id,
            request.provider_account_id,
            &self.provider_id,
            request.access,
        )
        .await?;
        let Some(stored) = fetch_stored(&mut transaction, request.draft_id).await? else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        if stored.record.draft.owner_user_id != request.owner_user_id
            || stored.record.draft.provider_account_id != request.provider_account_id
            || stored.record.draft.provider_id != self.provider_id
        {
            return Err(SecretStoreError::Unauthorized);
        }
        let secret = fetch_secret(&mut transaction, stored.secret_id).await?;
        if secret.owner_user_id != request.owner_user_id
            || secret.purpose != SecretPurpose::ProviderExecutionState
            || secret.version != 1
            || secret.created_at != stored.record.draft.created_at
        {
            return Err(SecretStoreError::VersionConflict);
        }
        let secret_ref = SecretRef {
            id: stored.secret_id,
            owner_user_id: secret.owner_user_id,
            purpose: secret.purpose,
            version: secret.version,
            key_id: secret.key_id.clone(),
            created_at: secret.created_at,
            updated_at: secret.updated_at,
        };
        let plaintext = decrypt(
            self.keyring.get(&secret.key_id)?,
            &secret_ref,
            &secret.nonce,
            &secret.encrypted_data,
        )?;
        let provider_draft = ProviderCourseEnrollmentDraft::try_new(
            stored.record.draft.provider_id.clone(),
            stored.record.artifact_type.clone(),
            stored.record.draft.remote_course_id.clone(),
            stored.record.draft.remote_class_id.clone(),
            stored.record.preview_sanitized.clone(),
            SecretValue::new(plaintext),
        )
        .map_err(|_| SecretStoreError::AuthenticationFailed)?;
        if provider_draft.preview_digest() != stored.record.draft.preview_digest
            || provider_draft.request_digest() != stored.record.draft.request_digest
        {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "course_enrollment_request_accessed",
            &secret_ref,
        )
        .await
        .map_err(storage_error)?;
        insert_draft_audit(
            &mut transaction,
            &stored.record,
            request.correlation_id,
            "course_enrollment_draft_resolved",
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedCourseEnrollmentDraft {
            record: stored.record,
            provider_draft,
        }))
    }
}

#[async_trait]
impl CourseEnrollmentAttemptRepository for SqliteCourseEnrollmentDraftRepository {
    async fn create_course_enrollment_attempt(
        &self,
        request: CourseEnrollmentAttemptCreateRequest,
    ) -> Result<CourseEnrollmentAttemptCreateOutcome, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let draft = fetch_owned_draft_for_attempt(
            &mut transaction,
            request.draft_id,
            request.owner_user_id,
            request.provider_account_id,
            &self.provider_id,
        )
        .await?
        .ok_or(StorageError::CourseEnrollmentStateConflict)?;
        if request.at < draft.created_at {
            return Err(StorageError::CourseEnrollmentStateConflict);
        }
        if let Some(existing) = fetch_attempt_by_draft(&mut transaction, request.draft_id).await? {
            transaction.rollback().await?;
            return if existing.attempt.id == request.attempt_id {
                Ok(CourseEnrollmentAttemptCreateOutcome::AlreadyExists(
                    existing.attempt,
                ))
            } else {
                Err(StorageError::CourseEnrollmentStateConflict)
            };
        }
        let attempt =
            CourseEnrollmentAttempt::new(request.attempt_id, request.draft_id, request.at);
        sqlx::query(
            "INSERT INTO course_enrollment_attempts \
             (id, draft_id, state, created_at, updated_at, revision) \
             VALUES (?, ?, 'prepared', ?, ?, 1)",
        )
        .bind(attempt.id.to_string())
        .bind(attempt.draft_id.to_string())
        .bind(encode_timestamp(attempt.created_at))
        .bind(encode_timestamp(attempt.updated_at))
        .execute(&mut *transaction)
        .await?;
        insert_attempt_audit(
            &mut transaction,
            &attempt,
            "course_enrollment_attempt_created",
        )
        .await?;
        transaction.commit().await?;
        Ok(CourseEnrollmentAttemptCreateOutcome::Created(attempt))
    }

    async fn issue_course_enrollment_mutation(
        &self,
        request: CourseEnrollmentAttemptMutationIssueRequest<'_>,
    ) -> Result<CourseEnrollmentAttempt, StorageError> {
        self.mutate_attempt(
            request.owner_user_id,
            request.provider_account_id,
            request.attempt_id,
            "course_enrollment_mutation_issued",
            |stored| {
                stored.attempt.issue_mutation(
                    &stored.draft,
                    request.operation_type,
                    request.request_digest,
                    request.at,
                )
            },
        )
        .await
    }

    async fn record_course_enrollment_receipt(
        &self,
        request: CourseEnrollmentAttemptReceiptRequest,
    ) -> Result<CourseEnrollmentAttempt, StorageError> {
        self.mutate_attempt(
            request.owner_user_id,
            request.provider_account_id,
            request.attempt_id,
            "course_enrollment_receipt_recorded",
            |stored| stored.attempt.record_receipt(request.receipt),
        )
        .await
    }

    async fn begin_course_enrollment_verification(
        &self,
        request: CourseEnrollmentAttemptVerificationBeginRequest,
    ) -> Result<CourseEnrollmentAttempt, StorageError> {
        self.mutate_attempt(
            request.owner_user_id,
            request.provider_account_id,
            request.attempt_id,
            "course_enrollment_verification_started",
            |stored| stored.attempt.begin_verification(request.at),
        )
        .await
    }

    async fn record_course_enrollment_verification(
        &self,
        request: CourseEnrollmentAttemptVerificationRecordRequest,
    ) -> Result<CourseEnrollmentAttempt, StorageError> {
        self.mutate_attempt(
            request.owner_user_id,
            request.provider_account_id,
            request.attempt_id,
            "course_enrollment_verification_recorded",
            |stored| stored.attempt.record_verification(request.verification),
        )
        .await
    }

    async fn find_owned_course_enrollment_attempt(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        attempt_id: CourseEnrollmentAttemptId,
    ) -> Result<Option<CourseEnrollmentAttempt>, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let attempt = fetch_owned_attempt(
            &mut transaction,
            attempt_id,
            owner_user_id,
            provider_account_id,
            &self.provider_id,
        )
        .await?
        .map(|stored| stored.attempt);
        transaction.commit().await?;
        Ok(attempt)
    }
}

impl SqliteCourseEnrollmentDraftRepository {
    async fn mutate_attempt(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        attempt_id: CourseEnrollmentAttemptId,
        action: &str,
        mutate: impl FnOnce(
            &mut StoredAttempt,
        ) -> Result<(), asterism_domain::CourseEnrollmentValidationError>,
    ) -> Result<CourseEnrollmentAttempt, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut stored = fetch_owned_attempt(
            &mut transaction,
            attempt_id,
            owner_user_id,
            provider_account_id,
            &self.provider_id,
        )
        .await?
        .ok_or(StorageError::CourseEnrollmentStateConflict)?;
        mutate(&mut stored).map_err(|_| StorageError::CourseEnrollmentStateConflict)?;
        persist_attempt(&mut transaction, &stored).await?;
        insert_attempt_audit(&mut transaction, &stored.attempt, action).await?;
        transaction.commit().await?;
        Ok(stored.attempt)
    }
}

struct StoredAttempt {
    attempt: CourseEnrollmentAttempt,
    draft: CourseEnrollmentDraft,
    revision: i64,
}

async fn fetch_owned_draft_for_attempt(
    transaction: &mut Transaction<'_, Sqlite>,
    draft_id: CourseEnrollmentDraftId,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    provider_id: &ProviderId,
) -> Result<Option<CourseEnrollmentDraft>, StorageError> {
    let row = sqlx::query(
        "SELECT * FROM course_enrollment_drafts \
         WHERE id = ? AND owner_user_id = ? AND provider_account_id = ? AND provider_id = ?",
    )
    .bind(draft_id.to_string())
    .bind(owner_user_id.to_string())
    .bind(provider_account_id.to_string())
    .bind(provider_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(|row| decode_stored(&row).map(|stored| stored.record.draft))
        .transpose()
        .map_err(StorageError::from)
}

async fn fetch_attempt_by_draft(
    transaction: &mut Transaction<'_, Sqlite>,
    draft_id: CourseEnrollmentDraftId,
) -> Result<Option<StoredAttempt>, StorageError> {
    let row = sqlx::query(
        "SELECT a.*, d.owner_user_id, d.provider_account_id, d.provider_id, \
                d.remote_course_id, d.remote_class_id, d.preview_digest, d.request_digest, \
                d.request_secret_blob_id, d.created_at AS draft_created_at \
         FROM course_enrollment_attempts a \
         JOIN course_enrollment_drafts d ON d.id = a.draft_id \
         WHERE a.draft_id = ?",
    )
    .bind(draft_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(|row| decode_attempt(&row)).transpose()
}

async fn fetch_owned_attempt(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt_id: CourseEnrollmentAttemptId,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    provider_id: &ProviderId,
) -> Result<Option<StoredAttempt>, StorageError> {
    let row = sqlx::query(
        "SELECT a.*, d.owner_user_id, d.provider_account_id, d.provider_id, \
                d.remote_course_id, d.remote_class_id, d.preview_digest, d.request_digest, \
                d.request_secret_blob_id, d.created_at AS draft_created_at \
         FROM course_enrollment_attempts a \
         JOIN course_enrollment_drafts d ON d.id = a.draft_id \
         WHERE a.id = ? AND d.owner_user_id = ? AND d.provider_account_id = ? \
               AND d.provider_id = ?",
    )
    .bind(attempt_id.to_string())
    .bind(owner_user_id.to_string())
    .bind(provider_account_id.to_string())
    .bind(provider_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(|row| decode_attempt(&row)).transpose()
}

fn decode_attempt(row: &SqliteRow) -> Result<StoredAttempt, StorageError> {
    let draft = CourseEnrollmentDraft {
        id: parse_storage_id(&row.try_get::<String, _>("draft_id")?)?,
        owner_user_id: parse_storage_id(&row.try_get::<String, _>("owner_user_id")?)?,
        provider_account_id: parse_storage_id(&row.try_get::<String, _>("provider_account_id")?)?,
        provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
            .map_err(|_| invalid_attempt_data())?,
        remote_course_id: row.try_get("remote_course_id")?,
        remote_class_id: row.try_get("remote_class_id")?,
        preview_digest: decode_storage_digest(row.try_get("preview_digest")?)?,
        request_digest: decode_storage_digest(row.try_get("request_digest")?)?,
        artifact_secret_id: parse_storage_id(&row.try_get::<String, _>("request_secret_blob_id")?)?,
        created_at: decode_storage_timestamp(&row.try_get::<String, _>("draft_created_at")?)?,
    };
    draft.validate().map_err(|_| invalid_attempt_data())?;
    let receipt = match (
        row.try_get::<Option<Vec<u8>>, _>("response_digest")?,
        row.try_get::<Option<i64>, _>("response_accepted")?,
        row.try_get::<Option<String>, _>("response_observed_at")?,
    ) {
        (None, None, None) => None,
        (Some(digest), Some(accepted), Some(observed_at)) if matches!(accepted, 0 | 1) => {
            Some(CourseEnrollmentMutationReceipt {
                response_digest: decode_storage_digest(digest)?,
                accepted: accepted == 1,
                observed_at: decode_storage_timestamp(&observed_at)?,
            })
        }
        _ => return Err(invalid_attempt_data()),
    };
    let verification = match (
        row.try_get::<Option<Vec<u8>>, _>("verification_digest")?,
        row.try_get::<Option<i64>, _>("membership_present")?,
        row.try_get::<Option<String>, _>("verification_observed_at")?,
    ) {
        (None, None, None) => None,
        (Some(digest), Some(present), Some(observed_at)) if matches!(present, 0 | 1) => {
            Some(CourseEnrollmentVerification {
                observation_digest: decode_storage_digest(digest)?,
                membership_present: present == 1,
                observed_at: decode_storage_timestamp(&observed_at)?,
            })
        }
        _ => return Err(invalid_attempt_data()),
    };
    let attempt = CourseEnrollmentAttempt {
        id: parse_storage_id(&row.try_get::<String, _>("id")?)?,
        draft_id: draft.id,
        state: decode_attempt_state(&row.try_get::<String, _>("state")?)?,
        issued_operation_type: row.try_get("issued_operation_type")?,
        issued_request_digest: row
            .try_get::<Option<Vec<u8>>, _>("issued_request_digest")?
            .map(decode_storage_digest)
            .transpose()?,
        receipt,
        verification,
        created_at: decode_storage_timestamp(&row.try_get::<String, _>("created_at")?)?,
        updated_at: decode_storage_timestamp(&row.try_get::<String, _>("updated_at")?)?,
    };
    validate_attempt_snapshot(&attempt, &draft)?;
    Ok(StoredAttempt {
        attempt,
        draft,
        revision: row.try_get("revision")?,
    })
}

async fn persist_attempt(
    transaction: &mut Transaction<'_, Sqlite>,
    stored: &StoredAttempt,
) -> Result<(), StorageError> {
    let receipt = stored.attempt.receipt;
    let verification = stored.attempt.verification;
    let changed = sqlx::query(
        "UPDATE course_enrollment_attempts SET \
             state = ?, issued_operation_type = ?, issued_request_digest = ?, response_digest = ?, \
             response_accepted = ?, response_observed_at = ?, verification_digest = ?, \
             membership_present = ?, verification_observed_at = ?, updated_at = ?, \
             revision = revision + 1 \
         WHERE id = ? AND revision = ?",
    )
    .bind(encode_attempt_state(stored.attempt.state))
    .bind(&stored.attempt.issued_operation_type)
    .bind(
        stored
            .attempt
            .issued_request_digest
            .map(|digest| digest.to_vec()),
    )
    .bind(receipt.map(|value| value.response_digest.to_vec()))
    .bind(receipt.map(|value| i64::from(value.accepted)))
    .bind(receipt.map(|value| encode_timestamp(value.observed_at)))
    .bind(verification.map(|value| value.observation_digest.to_vec()))
    .bind(verification.map(|value| i64::from(value.membership_present)))
    .bind(verification.map(|value| encode_timestamp(value.observed_at)))
    .bind(encode_timestamp(stored.attempt.updated_at))
    .bind(stored.attempt.id.to_string())
    .bind(stored.revision)
    .execute(&mut **transaction)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(StorageError::CourseEnrollmentStateConflict);
    }
    Ok(())
}

async fn insert_attempt_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    attempt: &CourseEnrollmentAttempt,
    action: &str,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'service', 'core', ?, 'course_enrollment_attempt', ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(attempt.updated_at))
    .bind(action)
    .bind(attempt.id.to_string())
    .bind(
        serde_json::json!({
            "draft_id": attempt.draft_id,
            "state": encode_attempt_state(attempt.state),
            "issued_operation_type": attempt.issued_operation_type,
            "issued_request_digest": attempt.issued_request_digest.map(|_| "[HASHED]"),
            "receipt_digest": attempt.receipt.map(|_| "[HASHED]"),
            "verification_digest": attempt.verification.map(|_| "[HASHED]"),
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_attempt_snapshot(
    attempt: &CourseEnrollmentAttempt,
    draft: &CourseEnrollmentDraft,
) -> Result<(), StorageError> {
    let lifecycle_valid = match attempt.state {
        CourseEnrollmentAttemptState::Prepared
        | CourseEnrollmentAttemptState::Cancelled
        | CourseEnrollmentAttemptState::FailedBeforeIssue => {
            attempt.issued_operation_type.is_none()
                && attempt.issued_request_digest.is_none()
                && attempt.receipt.is_none()
                && attempt.verification.is_none()
        }
        CourseEnrollmentAttemptState::MutationIssued => {
            attempt.issued_operation_type.is_some()
                && attempt.issued_request_digest.is_some()
                && attempt.receipt.is_none()
                && attempt.verification.is_none()
        }
        CourseEnrollmentAttemptState::ReceiptRecorded => {
            attempt.issued_operation_type.is_some()
                && attempt.issued_request_digest.is_some()
                && attempt.receipt.is_some_and(|receipt| receipt.accepted)
                && attempt.verification.is_none()
        }
        CourseEnrollmentAttemptState::VerificationPending => {
            attempt.issued_operation_type.is_some()
                && attempt.issued_request_digest.is_some()
                && attempt.receipt.is_none_or(|receipt| receipt.accepted)
                && attempt
                    .verification
                    .is_none_or(|verification| !verification.membership_present)
        }
        CourseEnrollmentAttemptState::Succeeded => {
            attempt.issued_operation_type.is_some()
                && attempt.issued_request_digest.is_some()
                && attempt.receipt.is_none_or(|receipt| receipt.accepted)
                && attempt
                    .verification
                    .is_some_and(|verification| verification.membership_present)
        }
        CourseEnrollmentAttemptState::Rejected => {
            attempt.issued_operation_type.is_some()
                && attempt.issued_request_digest.is_some()
                && attempt.receipt.is_some_and(|receipt| !receipt.accepted)
                && attempt.verification.is_none()
        }
    };
    if !lifecycle_valid
        || attempt.draft_id != draft.id
        || attempt.updated_at < attempt.created_at
        || attempt.created_at < draft.created_at
        || attempt
            .issued_request_digest
            .is_some_and(|digest| digest != draft.request_digest)
        || attempt
            .issued_operation_type
            .as_deref()
            .is_some_and(|value| {
                !value
                    .strip_prefix(draft.provider_id.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1)
            })
        || attempt.receipt.is_some_and(|receipt| {
            receipt.response_digest == [0; 32]
                || receipt.observed_at < attempt.created_at
                || receipt.observed_at > attempt.updated_at
        })
        || attempt.verification.is_some_and(|verification| {
            verification.observation_digest == [0; 32]
                || verification.observed_at < attempt.created_at
                || verification.observed_at > attempt.updated_at
        })
    {
        return Err(invalid_attempt_data());
    }
    Ok(())
}

const fn encode_attempt_state(state: CourseEnrollmentAttemptState) -> &'static str {
    match state {
        CourseEnrollmentAttemptState::Prepared => "prepared",
        CourseEnrollmentAttemptState::MutationIssued => "mutation_issued",
        CourseEnrollmentAttemptState::ReceiptRecorded => "receipt_recorded",
        CourseEnrollmentAttemptState::VerificationPending => "verification_pending",
        CourseEnrollmentAttemptState::Succeeded => "succeeded",
        CourseEnrollmentAttemptState::Rejected => "rejected",
        CourseEnrollmentAttemptState::Cancelled => "cancelled",
        CourseEnrollmentAttemptState::FailedBeforeIssue => "failed_before_issue",
    }
}

fn decode_attempt_state(value: &str) -> Result<CourseEnrollmentAttemptState, StorageError> {
    match value {
        "prepared" => Ok(CourseEnrollmentAttemptState::Prepared),
        "mutation_issued" => Ok(CourseEnrollmentAttemptState::MutationIssued),
        "receipt_recorded" => Ok(CourseEnrollmentAttemptState::ReceiptRecorded),
        "verification_pending" => Ok(CourseEnrollmentAttemptState::VerificationPending),
        "succeeded" => Ok(CourseEnrollmentAttemptState::Succeeded),
        "rejected" => Ok(CourseEnrollmentAttemptState::Rejected),
        "cancelled" => Ok(CourseEnrollmentAttemptState::Cancelled),
        "failed_before_issue" => Ok(CourseEnrollmentAttemptState::FailedBeforeIssue),
        _ => Err(invalid_attempt_data()),
    }
}

fn parse_storage_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
{
    T::from_str(value).map_err(|_| invalid_attempt_data())
}

fn decode_storage_digest(bytes: Vec<u8>) -> Result<[u8; 32], StorageError> {
    bytes.try_into().map_err(|_| invalid_attempt_data())
}

fn decode_storage_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| invalid_attempt_data())
}

fn invalid_attempt_data() -> StorageError {
    StorageError::InvalidData("course enrollment Attempt is inconsistent".to_owned())
}

struct StoredDraft {
    record: CourseEnrollmentDraftRecord,
    secret_id: SecretId,
}

fn record_from_request(
    request: &CourseEnrollmentDraftCreateRequest<'_>,
) -> Result<CourseEnrollmentDraftRecord, SecretStoreError> {
    let draft = CourseEnrollmentDraft {
        id: request.draft_id,
        owner_user_id: request.owner_user_id,
        provider_account_id: request.provider_account_id,
        provider_id: request.provider_draft.provider_id().clone(),
        remote_course_id: request.provider_draft.remote_course_id().to_owned(),
        remote_class_id: request.provider_draft.remote_class_id().to_owned(),
        preview_digest: request.provider_draft.preview_digest(),
        request_digest: request.provider_draft.request_digest(),
        artifact_secret_id: SecretId::new(),
        created_at: request.created_at,
    };
    draft
        .validate()
        .map_err(|_| SecretStoreError::InvalidValue)?;
    Ok(CourseEnrollmentDraftRecord {
        draft,
        artifact_type: request.provider_draft.artifact_type().to_owned(),
        preview_sanitized: request.provider_draft.preview_sanitized().clone(),
    })
}

fn records_match(left: &CourseEnrollmentDraftRecord, right: &CourseEnrollmentDraftRecord) -> bool {
    left.draft.id == right.draft.id
        && left.draft.owner_user_id == right.draft.owner_user_id
        && left.draft.provider_account_id == right.draft.provider_account_id
        && left.draft.provider_id == right.draft.provider_id
        && left.draft.remote_course_id == right.draft.remote_course_id
        && left.draft.remote_class_id == right.draft.remote_class_id
        && left.draft.preview_digest == right.draft.preview_digest
        && left.draft.request_digest == right.draft.request_digest
        && left.draft.created_at == right.draft.created_at
        && left.artifact_type == right.artifact_type
        && left.preview_sanitized == right.preview_sanitized
}

async fn authorize_account(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    provider_id: &ProviderId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    let actor_authorized = match &access.actor {
        SecretActor::CoreService(_) => true,
        SecretActor::ProviderRuntime(runtime) => runtime == provider_id.as_str(),
        SecretActor::User(_) | SecretActor::ServiceToken(_) => false,
    };
    if !access.authorizes(owner_user_id) || !actor_authorized {
        return Err(SecretStoreError::Unauthorized);
    }
    let row = sqlx::query("SELECT owner_user_id, provider_id FROM provider_accounts WHERE id = ?")
        .bind(provider_account_id.to_string())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
        .ok_or(SecretStoreError::Unauthorized)?;
    if row
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
    Ok(())
}

async fn fetch_record(
    transaction: &mut Transaction<'_, Sqlite>,
    draft_id: CourseEnrollmentDraftId,
) -> Result<Option<CourseEnrollmentDraftRecord>, SecretStoreError> {
    Ok(fetch_stored(transaction, draft_id)
        .await?
        .map(|stored| stored.record))
}

async fn fetch_record_by_request(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    request_digest: [u8; 32],
) -> Result<Option<CourseEnrollmentDraftRecord>, SecretStoreError> {
    let row = sqlx::query(
        "SELECT * FROM course_enrollment_drafts \
         WHERE owner_user_id = ? AND provider_account_id = ? AND request_digest = ?",
    )
    .bind(owner_user_id.to_string())
    .bind(provider_account_id.to_string())
    .bind(request_digest.to_vec())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.map(|row| decode_stored(&row).map(|stored| stored.record))
        .transpose()
}

async fn fetch_stored(
    transaction: &mut Transaction<'_, Sqlite>,
    draft_id: CourseEnrollmentDraftId,
) -> Result<Option<StoredDraft>, SecretStoreError> {
    let row = sqlx::query("SELECT * FROM course_enrollment_drafts WHERE id = ?")
        .bind(draft_id.to_string())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?;
    row.map(|row| decode_stored(&row)).transpose()
}

fn decode_stored(row: &SqliteRow) -> Result<StoredDraft, SecretStoreError> {
    let secret_id = parse_id(
        &row.try_get::<String, _>("request_secret_blob_id")
            .map_err(storage_error)?,
    )?;
    let draft = CourseEnrollmentDraft {
        id: parse_id(&row.try_get::<String, _>("id").map_err(storage_error)?)?,
        owner_user_id: parse_id(
            &row.try_get::<String, _>("owner_user_id")
                .map_err(storage_error)?,
        )?,
        provider_account_id: parse_id(
            &row.try_get::<String, _>("provider_account_id")
                .map_err(storage_error)?,
        )?,
        provider_id: ProviderId::new(
            row.try_get::<String, _>("provider_id")
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        remote_course_id: row.try_get("remote_course_id").map_err(storage_error)?,
        remote_class_id: row.try_get("remote_class_id").map_err(storage_error)?,
        preview_digest: decode_digest(row.try_get("preview_digest").map_err(storage_error)?)?,
        request_digest: decode_digest(row.try_get("request_digest").map_err(storage_error)?)?,
        artifact_secret_id: secret_id,
        created_at: decode_timestamp(
            &row.try_get::<String, _>("created_at")
                .map_err(storage_error)?,
        )?,
    };
    draft.validate().map_err(|_| SecretStoreError::Storage)?;
    Ok(StoredDraft {
        record: CourseEnrollmentDraftRecord {
            draft,
            artifact_type: row.try_get("artifact_type").map_err(storage_error)?,
            preview_sanitized: serde_json::from_str(
                &row.try_get::<String, _>("preview_sanitized_json")
                    .map_err(storage_error)?,
            )
            .map_err(|_| SecretStoreError::Storage)?,
        },
        secret_id,
    })
}

fn enrollment_secret(owner_user_id: UserId, key_id: &str, at: Timestamp) -> SecretRef {
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

async fn insert_draft_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    record: &CourseEnrollmentDraftRecord,
    correlation_id: &str,
    action: &str,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'service', 'core', ?, 'course_enrollment_draft', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(record.draft.created_at))
    .bind(action)
    .bind(record.draft.id.to_string())
    .bind(correlation_id)
    .bind(
        serde_json::json!({
            "provider_id": record.draft.provider_id,
            "provider_account_id": record.draft.provider_account_id,
            "artifact_type": record.artifact_type,
            "preview_digest": "[HASHED]",
            "request_digest": "[HASHED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn validate_context(correlation_id: &str, access: &SecretAccess) -> Result<(), SecretStoreError> {
    if !correlation_id.is_empty()
        && correlation_id.len() <= 256
        && correlation_id.trim() == correlation_id
        && !correlation_id.chars().any(char::is_control)
        && access.correlation_id == correlation_id
    {
        Ok(())
    } else {
        Err(SecretStoreError::InvalidValue)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_domain::{AuthState, CourseEnrollmentDraftId};
    use asterism_secrets::{SecretActor, SecretKey, SecretValue};
    use serde_json::json;

    use super::*;

    struct Fixture {
        database: Database,
        repository: SqliteCourseEnrollmentDraftRepository,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        access: SecretAccess,
        now: Timestamp,
    }

    async fn fixture() -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let now = Utc::now();
        let owner_user_id = UserId::new();
        let provider_account_id = ProviderAccountId::new();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'owner', 'hash', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner_user_id.to_string())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, 'chaoxing', 'Chaoxing', ?, ?, ?)",
        )
        .bind(provider_account_id.to_string())
        .bind(owner_user_id.to_string())
        .bind(serde_json::to_string(&AuthState::Authenticated).unwrap())
        .bind(encode_timestamp(now))
        .bind(encode_timestamp(now))
        .execute(database.pool())
        .await
        .unwrap();
        let keyring = Arc::new(
            SecretKeyring::new(
                "test-key",
                BTreeMap::from([("test-key".to_owned(), SecretKey::new([31; 32]))]),
            )
            .unwrap(),
        );
        Fixture {
            repository: SqliteCourseEnrollmentDraftRepository::new(
                database.clone(),
                keyring,
                ProviderId::new("chaoxing").unwrap(),
            ),
            database,
            owner_user_id,
            provider_account_id,
            access: SecretAccess {
                actor: SecretActor::CoreService("course-enrollment-test"),
                correlation_id: "enrollment-correlation".to_owned(),
                reason: "persist exact course enrollment draft".to_owned(),
            },
            now,
        }
    }

    fn provider_draft() -> ProviderCourseEnrollmentDraft {
        ProviderCourseEnrollmentDraft::try_new(
            ProviderId::new("chaoxing").unwrap(),
            "chaoxing.course-enrollment.v1",
            "course-7",
            "class-9",
            json!({"course_title": "Writing", "teacher": "Li"}),
            SecretValue::new(b"POST /participateCls?courseId=7&classId=9&check=opaque".to_vec()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn encrypted_draft_round_trips_and_exact_retry_is_idempotent() {
        let fixture = fixture().await;
        let draft_id = CourseEnrollmentDraftId::new();
        let first_artifact = provider_draft();
        let created = fixture
            .repository
            .create_course_enrollment_draft(CourseEnrollmentDraftCreateRequest {
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                provider_draft: &first_artifact,
                created_at: fixture.now,
                correlation_id: &fixture.access.correlation_id,
                access: &fixture.access,
            })
            .await
            .unwrap();
        assert!(matches!(
            created,
            CourseEnrollmentDraftCreateOutcome::Created(_)
        ));
        let retry_artifact = provider_draft();
        let retry = fixture
            .repository
            .create_course_enrollment_draft(CourseEnrollmentDraftCreateRequest {
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                provider_draft: &retry_artifact,
                created_at: fixture.now,
                correlation_id: &fixture.access.correlation_id,
                access: &fixture.access,
            })
            .await
            .unwrap();
        assert!(matches!(
            retry,
            CourseEnrollmentDraftCreateOutcome::AlreadyExists(_)
        ));
        let resolved = fixture
            .repository
            .resolve_course_enrollment_draft(CourseEnrollmentDraftResolveRequest {
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                correlation_id: &fixture.access.correlation_id,
                access: &fixture.access,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.provider_draft.request().expose_secret(),
            first_artifact.request().expose_secret()
        );
        assert_eq!(
            resolved.record.draft.artifact_secret_id.to_string().len(),
            36
        );
        let stored: Vec<u8> = sqlx::query_scalar("SELECT encrypted_data FROM secret_blobs LIMIT 1")
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert!(!stored.windows(14).any(|window| window == b"participateCls"));
    }

    #[tokio::test]
    async fn digest_drift_fails_closed_during_resolution() {
        let fixture = fixture().await;
        let draft_id = CourseEnrollmentDraftId::new();
        let artifact = provider_draft();
        fixture
            .repository
            .create_course_enrollment_draft(CourseEnrollmentDraftCreateRequest {
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                provider_draft: &artifact,
                created_at: fixture.now,
                correlation_id: &fixture.access.correlation_id,
                access: &fixture.access,
            })
            .await
            .unwrap();
        sqlx::query("UPDATE course_enrollment_drafts SET request_digest = ? WHERE id = ?")
            .bind(vec![99_u8; 32])
            .bind(draft_id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .repository
                .resolve_course_enrollment_draft(CourseEnrollmentDraftResolveRequest {
                    draft_id,
                    owner_user_id: fixture.owner_user_id,
                    provider_account_id: fixture.provider_account_id,
                    correlation_id: &fixture.access.correlation_id,
                    access: &fixture.access,
                })
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression keeps issue, ambiguous absence, replay rejection and fresh success in one no-replay lifecycle"
    )]
    async fn issued_attempt_recovers_only_through_fresh_membership_observation() {
        let fixture = fixture().await;
        let draft_id = CourseEnrollmentDraftId::new();
        let artifact = provider_draft();
        fixture
            .repository
            .create_course_enrollment_draft(CourseEnrollmentDraftCreateRequest {
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                provider_draft: &artifact,
                created_at: fixture.now,
                correlation_id: &fixture.access.correlation_id,
                access: &fixture.access,
            })
            .await
            .unwrap();
        let attempt_id = CourseEnrollmentAttemptId::new();
        fixture
            .repository
            .create_course_enrollment_attempt(CourseEnrollmentAttemptCreateRequest {
                attempt_id,
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                at: fixture.now,
            })
            .await
            .unwrap();
        let issued = fixture
            .repository
            .issue_course_enrollment_mutation(CourseEnrollmentAttemptMutationIssueRequest {
                attempt_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                operation_type: "chaoxing.course-enrollment.join",
                request_digest: artifact.request_digest(),
                at: fixture.now,
            })
            .await
            .unwrap();
        assert_eq!(issued.state, CourseEnrollmentAttemptState::MutationIssued);
        let pending = fixture
            .repository
            .begin_course_enrollment_verification(CourseEnrollmentAttemptVerificationBeginRequest {
                attempt_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                at: fixture.now,
            })
            .await
            .unwrap();
        assert_eq!(
            pending.state,
            CourseEnrollmentAttemptState::VerificationPending
        );
        let absent = fixture
            .repository
            .record_course_enrollment_verification(
                CourseEnrollmentAttemptVerificationRecordRequest {
                    attempt_id,
                    owner_user_id: fixture.owner_user_id,
                    provider_account_id: fixture.provider_account_id,
                    verification: CourseEnrollmentVerification {
                        observation_digest: [51; 32],
                        membership_present: false,
                        observed_at: fixture.now,
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            absent.state,
            CourseEnrollmentAttemptState::VerificationPending
        );
        assert!(matches!(
            fixture
                .repository
                .issue_course_enrollment_mutation(CourseEnrollmentAttemptMutationIssueRequest {
                    attempt_id,
                    owner_user_id: fixture.owner_user_id,
                    provider_account_id: fixture.provider_account_id,
                    operation_type: "chaoxing.course-enrollment.join",
                    request_digest: artifact.request_digest(),
                    at: fixture.now,
                })
                .await,
            Err(StorageError::CourseEnrollmentStateConflict)
        ));
        let completed = fixture
            .repository
            .record_course_enrollment_verification(
                CourseEnrollmentAttemptVerificationRecordRequest {
                    attempt_id,
                    owner_user_id: fixture.owner_user_id,
                    provider_account_id: fixture.provider_account_id,
                    verification: CourseEnrollmentVerification {
                        observation_digest: [52; 32],
                        membership_present: true,
                        observed_at: fixture.now,
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(completed.state, CourseEnrollmentAttemptState::Succeeded);
    }

    #[tokio::test]
    async fn definite_rejection_is_terminal_and_cannot_enter_recovery() {
        let fixture = fixture().await;
        let draft_id = CourseEnrollmentDraftId::new();
        let artifact = provider_draft();
        fixture
            .repository
            .create_course_enrollment_draft(CourseEnrollmentDraftCreateRequest {
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                provider_draft: &artifact,
                created_at: fixture.now,
                correlation_id: &fixture.access.correlation_id,
                access: &fixture.access,
            })
            .await
            .unwrap();
        let attempt_id = CourseEnrollmentAttemptId::new();
        fixture
            .repository
            .create_course_enrollment_attempt(CourseEnrollmentAttemptCreateRequest {
                attempt_id,
                draft_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                at: fixture.now,
            })
            .await
            .unwrap();
        fixture
            .repository
            .issue_course_enrollment_mutation(CourseEnrollmentAttemptMutationIssueRequest {
                attempt_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                operation_type: "chaoxing.course-enrollment.join",
                request_digest: artifact.request_digest(),
                at: fixture.now,
            })
            .await
            .unwrap();
        let rejected = fixture
            .repository
            .record_course_enrollment_receipt(CourseEnrollmentAttemptReceiptRequest {
                attempt_id,
                owner_user_id: fixture.owner_user_id,
                provider_account_id: fixture.provider_account_id,
                receipt: CourseEnrollmentMutationReceipt {
                    response_digest: [61; 32],
                    accepted: false,
                    observed_at: fixture.now,
                },
            })
            .await
            .unwrap();
        assert_eq!(rejected.state, CourseEnrollmentAttemptState::Rejected);
        assert!(matches!(
            fixture
                .repository
                .begin_course_enrollment_verification(
                    CourseEnrollmentAttemptVerificationBeginRequest {
                        attempt_id,
                        owner_user_id: fixture.owner_user_id,
                        provider_account_id: fixture.provider_account_id,
                        at: fixture.now,
                    }
                )
                .await,
            Err(StorageError::CourseEnrollmentStateConflict)
        ));
    }
}
