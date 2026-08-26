use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditActor, AuthSession, AuthSessionId, AuthState, ProviderId, SecretId, Timestamp,
    WaitingUserState,
};
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::Duration;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    Database, InteractiveAuthAbortRequest, InteractiveAuthCandidateFailureRequest,
    InteractiveAuthContinuation, InteractiveAuthContinuationAttachRequest,
    InteractiveAuthContinuationMutationOutcome, InteractiveAuthContinuationRepository,
    InteractiveAuthPollAuthenticateRequest, InteractiveAuthPollClaim,
    InteractiveAuthPollClaimOutcome, InteractiveAuthPollClaimRequest,
    InteractiveAuthPollRotateRequest, InteractiveAuthPollTerminalRequest,
    InteractiveAuthTerminalState, ResolvedInteractiveAuthCandidate, SecretKeyring,
    auth_session::{
        decode_timestamp, encode_timestamp, fetch_auth_session, mirror_account_state,
        update_auth_session_in_transaction,
    },
    secret::{
        authorize, decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob,
        validate_secret,
    },
};

const MAX_LABEL_BYTES: usize = 96;
const MAX_POLL_LEASE_SECONDS: i64 = 120;
const CONTINUATION_SELECT: &str = "SELECT auth_session_id, provider_id, secret_blob_id, \
    continuation_type, continuation_digest, phase, revision, poll_count, maximum_polls, \
    active_poll_sequence, active_poll_digest, active_poll_expires_at, terminal_result_digest, \
    expires_at, created_at, updated_at FROM interactive_auth_continuations";

#[derive(Clone, Debug)]
pub struct SqliteInteractiveAuthContinuationRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteInteractiveAuthContinuationRepository {
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
impl InteractiveAuthContinuationRepository for SqliteInteractiveAuthContinuationRepository {
    #[allow(clippy::too_many_lines)]
    async fn attach_interactive_auth_continuation(
        &self,
        request: InteractiveAuthContinuationAttachRequest<'_>,
    ) -> Result<InteractiveAuthContinuation, SecretStoreError> {
        validate_label(&self.provider_id, request.continuation_type)?;
        validate_label(&self.provider_id, request.phase)?;
        validate_secret(&request.value)?;
        authorize(request.session.owner_user_id, request.access)?;
        if request.provider_id != &self.provider_id
            || request.continuation_digest != digest(request.value.expose_secret())
            || request.maximum_polls == 0
            || request.maximum_polls > 10_000
            || request.session.revision != request.expected_session_revision.saturating_add(1)
            || !matches!(
                request.session.state,
                AuthState::WaitingUser(WaitingUserState::QrScan | WaitingUserState::QrConfirm)
            )
            || request.attached_at < request.session.created_at
            || request.expires_at <= request.attached_at
            || request.expires_at > request.session.expires_at
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        ensure_session_binding(
            &mut transaction,
            request.session,
            &self.provider_id,
            request.expected_session_revision,
        )
        .await?;
        if fetch_continuation(&mut transaction, request.session.id)
            .await?
            .is_some()
        {
            return Err(SecretStoreError::VersionConflict);
        }

        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id: request.session.owner_user_id,
            purpose: SecretPurpose::BrowserJobCredential,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.attached_at,
            updated_at: request.attached_at,
        };
        let (nonce, encrypted_data) = encrypt(key, &secret, request.value.expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted_data).await?;
        sqlx::query(
            "INSERT INTO interactive_auth_continuations \
             (auth_session_id, provider_id, secret_blob_id, continuation_type, \
              continuation_digest, phase, revision, poll_count, maximum_polls, expires_at, \
              created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, 1, 0, ?, ?, ?, ?)",
        )
        .bind(request.session.id.to_string())
        .bind(self.provider_id.as_str())
        .bind(secret.id.to_string())
        .bind(request.continuation_type)
        .bind(request.continuation_digest.as_slice())
        .bind(request.phase)
        .bind(i64::from(request.maximum_polls))
        .bind(encode_timestamp(request.expires_at))
        .bind(encode_timestamp(request.attached_at))
        .bind(encode_timestamp(request.attached_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if !update_auth_session_in_transaction(
            &mut transaction,
            request.session,
            request.expected_session_revision,
            audit_actor(request.access)?,
            &request.access.correlation_id,
        )
        .await
        .map_err(storage_error)?
        {
            return Err(SecretStoreError::VersionConflict);
        }
        mirror_account_state(&mut transaction, request.session)
            .await
            .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "interactive_auth_continuation_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(InteractiveAuthContinuation {
            auth_session_id: request.session.id,
            provider_id: self.provider_id.clone(),
            continuation_type: request.continuation_type.to_owned(),
            continuation_digest: request.continuation_digest,
            phase: request.phase.to_owned(),
            revision: 1,
            poll_count: 0,
            maximum_polls: request.maximum_polls,
            terminal_result_digest: None,
            expires_at: request.expires_at,
            created_at: request.attached_at,
            updated_at: request.attached_at,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn claim_interactive_auth_poll(
        &self,
        request: InteractiveAuthPollClaimRequest<'_>,
    ) -> Result<InteractiveAuthPollClaimOutcome, SecretStoreError> {
        authorize(request.owner_user_id, request.access)?;
        if request.claim_expires_at <= request.claimed_at
            || request.claim_expires_at
                > request.claimed_at + Duration::seconds(MAX_POLL_LEASE_SECONDS)
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        let Some(session) = fetch_auth_session(&mut transaction, request.auth_session_id)
            .await
            .map_err(storage_error)?
        else {
            return Ok(InteractiveAuthPollClaimOutcome::Unavailable);
        };
        let Some(row) = fetch_continuation_row(&mut transaction, request.auth_session_id).await?
        else {
            return Ok(InteractiveAuthPollClaimOutcome::Unavailable);
        };
        let mut continuation = decode_continuation(&row)?;
        if session.owner_user_id != request.owner_user_id
            || session.provider_account_id != request.provider_account_id
            || continuation.provider_id != self.provider_id
            || !matches!(
                session.state,
                AuthState::WaitingUser(WaitingUserState::QrScan | WaitingUserState::QrConfirm)
            )
            || session.revision != continuation.revision.saturating_add(1)
            || request.claimed_at < continuation.updated_at
            || request.claimed_at >= continuation.expires_at
            || request.claim_expires_at > continuation.expires_at
            || !latest_session(&mut transaction, &session).await?
        {
            return Ok(InteractiveAuthPollClaimOutcome::Unavailable);
        }
        let active_expiry = optional_timestamp(&row, "active_poll_expires_at")?;
        if active_expiry.is_some_and(|expires_at| expires_at > request.claimed_at) {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthPollClaimOutcome::Busy);
        }
        if let Some(active_expiry) = active_expiry {
            let stale_sequence = decode_optional_u32(&row, "active_poll_sequence")?
                .ok_or(SecretStoreError::Storage)?;
            let stale_digest = row
                .try_get::<Option<Vec<u8>>, _>("active_poll_digest")
                .map_err(storage_error)?
                .ok_or(SecretStoreError::Storage)?
                .try_into()
                .map_err(|_| SecretStoreError::Storage)?;
            finish_operation(
                &mut transaction,
                &InteractiveAuthPollClaim {
                    continuation: continuation.clone(),
                    poll_sequence: stale_sequence,
                    claim_digest: stale_digest,
                    claim_expires_at: active_expiry,
                },
                "retryable",
                None,
                request.claimed_at,
            )
            .await?;
        }
        if continuation.poll_count >= continuation.maximum_polls {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthPollClaimOutcome::Exhausted);
        }
        let poll_sequence = continuation
            .poll_count
            .checked_add(1)
            .ok_or(SecretStoreError::VersionConflict)?;
        let claim_digest = poll_claim_digest(
            &self.provider_id,
            request.auth_session_id,
            continuation.revision,
            poll_sequence,
            &request.access.correlation_id,
        );
        let result = sqlx::query(
            "UPDATE interactive_auth_continuations SET poll_count = ?, \
             active_poll_sequence = ?, active_poll_digest = ?, active_poll_expires_at = ? \
             WHERE auth_session_id = ? AND revision = ? AND poll_count = ?",
        )
        .bind(i64::from(poll_sequence))
        .bind(i64::from(poll_sequence))
        .bind(claim_digest.as_slice())
        .bind(encode_timestamp(request.claim_expires_at))
        .bind(request.auth_session_id.to_string())
        .bind(i64::from(continuation.revision))
        .bind(i64::from(continuation.poll_count))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthPollClaimOutcome::Busy);
        }
        sqlx::query(
            "INSERT INTO interactive_auth_poll_operations \
             (auth_session_id, poll_sequence, continuation_revision, request_digest, state, \
              issued_at) VALUES (?, ?, ?, ?, 'issued', ?)",
        )
        .bind(request.auth_session_id.to_string())
        .bind(i64::from(poll_sequence))
        .bind(i64::from(continuation.revision))
        .bind(claim_digest.as_slice())
        .bind(encode_timestamp(request.claimed_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        continuation.poll_count = poll_sequence;
        let value = resolve_value(
            &mut transaction,
            &self.keyring,
            &continuation,
            &row,
            request.access,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(InteractiveAuthPollClaimOutcome::Claimed {
            claim: Box::new(InteractiveAuthPollClaim {
                continuation,
                poll_sequence,
                claim_digest,
                claim_expires_at: request.claim_expires_at,
            }),
            value,
        })
    }

    async fn release_interactive_auth_poll(
        &self,
        claim: &InteractiveAuthPollClaim,
        released_at: Timestamp,
        access: &SecretAccess,
    ) -> Result<bool, SecretStoreError> {
        authorize_claim(claim, &self.provider_id, access)?;
        if released_at < claim.continuation.updated_at {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        let Some(session) =
            fetch_auth_session(&mut transaction, claim.continuation.auth_session_id)
                .await
                .map_err(storage_error)?
        else {
            return Ok(false);
        };
        authorize(session.owner_user_id, access)?;
        if !claim_matches(&mut transaction, claim).await? {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(false);
        }
        let updated = clear_claim(&mut transaction, claim, released_at).await?;
        if updated {
            finish_operation(&mut transaction, claim, "retryable", None, released_at).await?;
            transaction.commit().await.map_err(storage_error)?;
        } else {
            transaction.rollback().await.map_err(storage_error)?;
        }
        Ok(updated)
    }

    async fn resolve_interactive_auth_candidate(
        &self,
        owner_user_id: asterism_domain::UserId,
        provider_account_id: asterism_domain::ProviderAccountId,
        auth_session_id: AuthSessionId,
        access: &SecretAccess,
    ) -> Result<Option<ResolvedInteractiveAuthCandidate>, SecretStoreError> {
        authorize(owner_user_id, access)?;
        let mut transaction = begin(&self.database).await?;
        let Some(session) = fetch_auth_session(&mut transaction, auth_session_id)
            .await
            .map_err(storage_error)?
        else {
            return Ok(None);
        };
        let Some(row) = fetch_continuation_row(&mut transaction, auth_session_id).await? else {
            return Ok(None);
        };
        let continuation = decode_continuation(&row)?;
        if session.owner_user_id != owner_user_id
            || session.provider_account_id != provider_account_id
            || session.state != AuthState::ValidatingCredential
            || session.revision != continuation.revision.saturating_add(1)
            || continuation.provider_id != self.provider_id
            || continuation.terminal_result_digest.is_none()
            || !latest_session(&mut transaction, &session).await?
        {
            return Ok(None);
        }
        let value =
            resolve_value(&mut transaction, &self.keyring, &continuation, &row, access).await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedInteractiveAuthCandidate {
            session,
            continuation,
            value,
        }))
    }

    async fn rotate_interactive_auth_continuation(
        &self,
        request: InteractiveAuthPollRotateRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError> {
        self.rotate_claim(
            request.claim,
            request.waiting_session,
            request.expected_session_revision,
            request.continuation_type,
            request.continuation_digest,
            request.phase,
            request.replacement,
            request.result_digest,
            request.completed_at,
            request.access,
            "waiting",
            false,
        )
        .await
    }

    async fn persist_interactive_auth_candidate(
        &self,
        request: InteractiveAuthPollAuthenticateRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError> {
        self.rotate_claim(
            request.claim,
            request.validating_session,
            request.expected_session_revision,
            request.continuation_type,
            request.continuation_digest,
            request.phase,
            request.replacement,
            request.result_digest,
            request.completed_at,
            request.access,
            "authenticated",
            true,
        )
        .await
    }

    async fn finish_interactive_auth_terminal(
        &self,
        request: InteractiveAuthPollTerminalRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError> {
        authorize_claim(request.claim, &self.provider_id, request.access)?;
        authorize(request.terminal_session.owner_user_id, request.access)?;
        if request.result_digest == [0; 32]
            || request.completed_at < request.claim.continuation.updated_at
            || request.terminal_session.revision
                != request.expected_session_revision.saturating_add(1)
            || !matches!(
                (request.terminal_state, &request.terminal_session.state),
                (
                    InteractiveAuthTerminalState::Rejected,
                    AuthState::AuthFailed
                ) | (InteractiveAuthTerminalState::Expired, AuthState::Expired)
                    | (
                        InteractiveAuthTerminalState::Failed,
                        AuthState::AuthFailed
                            | AuthState::HumanRequired(_)
                            | AuthState::ClientUpdateRequired
                    )
            )
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        if !claim_matches(&mut transaction, request.claim).await?
            || !session_matches_claim(
                &mut transaction,
                request.claim,
                request.terminal_session,
                request.expected_session_revision,
            )
            .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        finish_operation(
            &mut transaction,
            request.claim,
            match request.terminal_state {
                InteractiveAuthTerminalState::Rejected => "rejected",
                InteractiveAuthTerminalState::Expired => "expired",
                InteractiveAuthTerminalState::Failed => "failed",
            },
            Some(request.result_digest),
            request.completed_at,
        )
        .await?;
        if !update_auth_session_in_transaction(
            &mut transaction,
            request.terminal_session,
            request.expected_session_revision,
            audit_actor(request.access)?,
            &request.access.correlation_id,
        )
        .await
        .map_err(storage_error)?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        mirror_account_state(&mut transaction, request.terminal_session)
            .await
            .map_err(storage_error)?;
        delete_continuation_secret(&mut transaction, request.claim, request.access).await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(InteractiveAuthContinuationMutationOutcome::Terminal(
            request.terminal_session.clone(),
        ))
    }

    async fn finish_interactive_auth_candidate_failure(
        &self,
        request: InteractiveAuthCandidateFailureRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError> {
        authorize(request.terminal_session.owner_user_id, request.access)?;
        let Some(terminal_result_digest) = request.continuation.terminal_result_digest else {
            return Err(SecretStoreError::InvalidValue);
        };
        if request.continuation.provider_id != self.provider_id
            || request.failed_at < request.continuation.updated_at
            || request.terminal_session.id != request.continuation.auth_session_id
            || request.terminal_session.revision
                != request.expected_session_revision.saturating_add(1)
            || !matches!(
                request.terminal_session.state,
                AuthState::AuthFailed
                    | AuthState::HumanRequired(_)
                    | AuthState::ClientUpdateRequired
                    | AuthState::Expired
            )
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        let Some(current_session) =
            fetch_auth_session(&mut transaction, request.continuation.auth_session_id)
                .await
                .map_err(storage_error)?
        else {
            return Ok(InteractiveAuthContinuationMutationOutcome::Unavailable);
        };
        if current_session.state != AuthState::ValidatingCredential
            || current_session.revision != request.expected_session_revision
            || current_session.owner_user_id != request.terminal_session.owner_user_id
            || current_session.provider_account_id != request.terminal_session.provider_account_id
            || current_session.revision != request.continuation.revision.saturating_add(1)
            || !latest_session(&mut transaction, &current_session).await?
            || !consume_interactive_auth_candidate(
                &mut transaction,
                request.continuation,
                terminal_result_digest,
                current_session.owner_user_id,
                request.access,
            )
            .await?
            || !update_auth_session_in_transaction(
                &mut transaction,
                request.terminal_session,
                request.expected_session_revision,
                audit_actor(request.access)?,
                &request.access.correlation_id,
            )
            .await
            .map_err(storage_error)?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        mirror_account_state(&mut transaction, request.terminal_session)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(InteractiveAuthContinuationMutationOutcome::Terminal(
            request.terminal_session.clone(),
        ))
    }

    async fn abort_interactive_auth_continuation(
        &self,
        request: InteractiveAuthAbortRequest<'_>,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError> {
        authorize(request.terminal_session.owner_user_id, request.access)?;
        if request.aborted_at < request.terminal_session.updated_at
            || request.terminal_session.revision
                != request.expected_session_revision.saturating_add(1)
            || !matches!(
                request.terminal_session.state,
                AuthState::AuthFailed
                    | AuthState::HumanRequired(_)
                    | AuthState::ProviderUnavailable
                    | AuthState::ClientUpdateRequired
                    | AuthState::Expired
                    | AuthState::Cancelled
            )
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = begin(&self.database).await?;
        let Some(current_session) =
            fetch_auth_session(&mut transaction, request.terminal_session.id)
                .await
                .map_err(storage_error)?
        else {
            return Ok(InteractiveAuthContinuationMutationOutcome::Unavailable);
        };
        let Some(row) = fetch_continuation_row(&mut transaction, current_session.id).await? else {
            return Ok(InteractiveAuthContinuationMutationOutcome::Unavailable);
        };
        let continuation = decode_continuation(&row)?;
        if continuation.provider_id != self.provider_id
            || current_session.owner_user_id != request.terminal_session.owner_user_id
            || current_session.provider_account_id != request.terminal_session.provider_account_id
            || current_session.revision != request.expected_session_revision
            || current_session.revision != continuation.revision.saturating_add(1)
            || !matches!(
                current_session.state,
                AuthState::WaitingUser(WaitingUserState::QrScan | WaitingUserState::QrConfirm)
                    | AuthState::ValidatingCredential
            )
            || !latest_session(&mut transaction, &current_session).await?
        {
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        let active_sequence = row
            .try_get::<Option<i64>, _>("active_poll_sequence")
            .map_err(storage_error)?;
        let active_digest = row
            .try_get::<Option<Vec<u8>>, _>("active_poll_digest")
            .map_err(storage_error)?;
        let active_expires_at = optional_timestamp(&row, "active_poll_expires_at")?;
        if active_expires_at.is_some_and(|expires_at| expires_at > request.aborted_at) {
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        if let (Some(sequence), Some(claim_digest), Some(claim_expires_at)) =
            (active_sequence, active_digest, active_expires_at)
        {
            let claim = InteractiveAuthPollClaim {
                continuation: continuation.clone(),
                poll_sequence: u32::try_from(sequence).map_err(|_| SecretStoreError::Storage)?,
                claim_digest: claim_digest
                    .try_into()
                    .map_err(|_| SecretStoreError::Storage)?,
                claim_expires_at,
            };
            finish_operation(
                &mut transaction,
                &claim,
                "retryable",
                None,
                request.aborted_at,
            )
            .await?;
        }
        if !update_auth_session_in_transaction(
            &mut transaction,
            request.terminal_session,
            request.expected_session_revision,
            audit_actor(request.access)?,
            &request.access.correlation_id,
        )
        .await
        .map_err(storage_error)?
        {
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        mirror_account_state(&mut transaction, request.terminal_session)
            .await
            .map_err(storage_error)?;
        delete_continuation_secret_metadata(&mut transaction, &continuation, request.access)
            .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(InteractiveAuthContinuationMutationOutcome::Terminal(
            request.terminal_session.clone(),
        ))
    }
}

impl SqliteInteractiveAuthContinuationRepository {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn rotate_claim(
        &self,
        claim: &InteractiveAuthPollClaim,
        next_session: &AuthSession,
        expected_session_revision: u32,
        continuation_type: &str,
        continuation_digest: [u8; 32],
        phase: &str,
        replacement: SecretValue,
        result_digest: [u8; 32],
        completed_at: Timestamp,
        access: &SecretAccess,
        operation_state: &str,
        authenticated: bool,
    ) -> Result<InteractiveAuthContinuationMutationOutcome, SecretStoreError> {
        authorize_claim(claim, &self.provider_id, access)?;
        authorize(next_session.owner_user_id, access)?;
        validate_label(&self.provider_id, continuation_type)?;
        validate_label(&self.provider_id, phase)?;
        validate_secret(&replacement)?;
        if continuation_digest != digest(replacement.expose_secret())
            || result_digest == [0; 32]
            || completed_at < claim.continuation.updated_at
            || next_session.revision != expected_session_revision.saturating_add(1)
            || authenticated != matches!(next_session.state, AuthState::ValidatingCredential)
            || (!authenticated
                && !matches!(
                    next_session.state,
                    AuthState::WaitingUser(WaitingUserState::QrScan | WaitingUserState::QrConfirm)
                ))
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let next_revision = claim
            .continuation
            .revision
            .checked_add(1)
            .ok_or(SecretStoreError::VersionConflict)?;
        let mut transaction = begin(&self.database).await?;
        if !claim_matches(&mut transaction, claim).await?
            || !session_matches_claim(
                &mut transaction,
                claim,
                next_session,
                expected_session_revision,
            )
            .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        let secret_id =
            fetch_secret_id(&mut transaction, claim.continuation.auth_session_id).await?;
        let stored = fetch_secret(&mut transaction, secret_id).await?;
        validate_stored_secret(&stored, &claim.continuation)?;
        let (key_id, key) = self.keyring.active();
        let rotated = SecretRef {
            id: secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: next_revision,
            key_id: key_id.to_owned(),
            created_at: stored.created_at,
            updated_at: completed_at,
        };
        let (nonce, encrypted_data) = encrypt(key, &rotated, replacement.expose_secret())?;
        let secret_updated = sqlx::query(
            "UPDATE secret_blobs SET key_id = ?, nonce = ?, encrypted_data = ?, version = ?, \
             updated_at = ? WHERE id = ? AND version = ?",
        )
        .bind(&rotated.key_id)
        .bind(nonce)
        .bind(encrypted_data)
        .bind(i64::from(next_revision))
        .bind(encode_timestamp(completed_at))
        .bind(secret_id.to_string())
        .bind(i64::from(claim.continuation.revision))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if secret_updated.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        let continuation_updated = sqlx::query(
            "UPDATE interactive_auth_continuations SET continuation_type = ?, \
             continuation_digest = ?, phase = ?, revision = ?, active_poll_sequence = NULL, \
             active_poll_digest = NULL, active_poll_expires_at = NULL, \
             terminal_result_digest = ?, updated_at = ? WHERE auth_session_id = ? \
             AND revision = ? AND active_poll_sequence = ? AND active_poll_digest = ?",
        )
        .bind(continuation_type)
        .bind(continuation_digest.as_slice())
        .bind(phase)
        .bind(i64::from(next_revision))
        .bind(authenticated.then_some(result_digest.as_slice()))
        .bind(encode_timestamp(completed_at))
        .bind(claim.continuation.auth_session_id.to_string())
        .bind(i64::from(claim.continuation.revision))
        .bind(i64::from(claim.poll_sequence))
        .bind(claim.claim_digest.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if continuation_updated.rows_affected() != 1
            || !update_auth_session_in_transaction(
                &mut transaction,
                next_session,
                expected_session_revision,
                audit_actor(access)?,
                &access.correlation_id,
            )
            .await
            .map_err(storage_error)?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthContinuationMutationOutcome::Conflict);
        }
        mirror_account_state(&mut transaction, next_session)
            .await
            .map_err(storage_error)?;
        finish_operation(
            &mut transaction,
            claim,
            operation_state,
            Some(result_digest),
            completed_at,
        )
        .await?;
        insert_secret_audit(
            &mut transaction,
            access,
            "interactive_auth_continuation_rotated",
            &rotated,
        )
        .await
        .map_err(storage_error)?;
        let continuation = InteractiveAuthContinuation {
            auth_session_id: claim.continuation.auth_session_id,
            provider_id: self.provider_id.clone(),
            continuation_type: continuation_type.to_owned(),
            continuation_digest,
            phase: phase.to_owned(),
            revision: next_revision,
            poll_count: claim.poll_sequence,
            maximum_polls: claim.continuation.maximum_polls,
            terminal_result_digest: authenticated.then_some(result_digest),
            expires_at: claim.continuation.expires_at,
            created_at: claim.continuation.created_at,
            updated_at: completed_at,
        };
        transaction.commit().await.map_err(storage_error)?;
        Ok(if authenticated {
            InteractiveAuthContinuationMutationOutcome::AuthenticatedCandidate(continuation)
        } else {
            InteractiveAuthContinuationMutationOutcome::Rotated(continuation)
        })
    }
}

async fn begin(database: &Database) -> Result<Transaction<'_, Sqlite>, SecretStoreError> {
    database
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_error)
}

async fn ensure_session_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    next_session: &AuthSession,
    provider_id: &ProviderId,
    expected_revision: u32,
) -> Result<(), SecretStoreError> {
    let Some(current) = fetch_auth_session(transaction, next_session.id)
        .await
        .map_err(storage_error)?
    else {
        return Err(SecretStoreError::NotFound);
    };
    let account_provider: Option<String> = sqlx::query_scalar(
        "SELECT provider_id FROM provider_accounts WHERE id = ? AND owner_user_id = ?",
    )
    .bind(current.provider_account_id.to_string())
    .bind(current.owner_user_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if current.revision != expected_revision
        || current.owner_user_id != next_session.owner_user_id
        || current.provider_account_id != next_session.provider_account_id
        || account_provider.as_deref() != Some(provider_id.as_str())
        || !latest_session(transaction, &current).await?
    {
        Err(SecretStoreError::VersionConflict)
    } else {
        Ok(())
    }
}

async fn session_matches_claim(
    transaction: &mut Transaction<'_, Sqlite>,
    claim: &InteractiveAuthPollClaim,
    next_session: &AuthSession,
    expected_revision: u32,
) -> Result<bool, SecretStoreError> {
    let Some(current) = fetch_auth_session(transaction, next_session.id)
        .await
        .map_err(storage_error)?
    else {
        return Ok(false);
    };
    Ok(current.id == claim.continuation.auth_session_id
        && current.revision == expected_revision
        && current.owner_user_id == next_session.owner_user_id
        && current.provider_account_id == next_session.provider_account_id
        && current.revision == claim.continuation.revision.saturating_add(1)
        && matches!(
            current.state,
            AuthState::WaitingUser(WaitingUserState::QrScan | WaitingUserState::QrConfirm)
        )
        && latest_session(transaction, &current).await?)
}

async fn latest_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &AuthSession,
) -> Result<bool, SecretStoreError> {
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT id FROM auth_sessions WHERE owner_user_id = ? AND provider_account_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(session.owner_user_id.to_string())
    .bind(session.provider_account_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(latest.as_deref() == Some(session.id.to_string().as_str()))
}

async fn fetch_continuation(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthSessionId,
) -> Result<Option<InteractiveAuthContinuation>, SecretStoreError> {
    fetch_continuation_row(transaction, session_id)
        .await?
        .as_ref()
        .map(decode_continuation)
        .transpose()
}

async fn fetch_continuation_row(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthSessionId,
) -> Result<Option<SqliteRow>, SecretStoreError> {
    let query = format!("{CONTINUATION_SELECT} WHERE auth_session_id = ?");
    sqlx::query(&query)
        .bind(session_id.to_string())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)
}

fn decode_continuation(row: &SqliteRow) -> Result<InteractiveAuthContinuation, SecretStoreError> {
    let continuation = InteractiveAuthContinuation {
        auth_session_id: AuthSessionId::from_str(
            row.try_get("auth_session_id").map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        provider_id: ProviderId::new(
            row.try_get::<String, _>("provider_id")
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        continuation_type: row.try_get("continuation_type").map_err(storage_error)?,
        continuation_digest: decode_digest(row, "continuation_digest")?,
        phase: row.try_get("phase").map_err(storage_error)?,
        revision: decode_u32(row, "revision")?,
        poll_count: decode_u32(row, "poll_count")?,
        maximum_polls: decode_u32(row, "maximum_polls")?,
        terminal_result_digest: row
            .try_get::<Option<Vec<u8>>, _>("terminal_result_digest")
            .map_err(storage_error)?
            .map(|bytes| bytes.try_into().map_err(|_| SecretStoreError::Storage))
            .transpose()?,
        expires_at: decode_timestamp(row.try_get("expires_at").map_err(storage_error)?)
            .map_err(storage_error)?,
        created_at: decode_timestamp(row.try_get("created_at").map_err(storage_error)?)
            .map_err(storage_error)?,
        updated_at: decode_timestamp(row.try_get("updated_at").map_err(storage_error)?)
            .map_err(storage_error)?,
    };
    if continuation.revision == 0
        || continuation.poll_count > continuation.maximum_polls
        || continuation.expires_at <= continuation.created_at
        || continuation.updated_at < continuation.created_at
    {
        return Err(SecretStoreError::Storage);
    }
    Ok(continuation)
}

async fn resolve_value(
    transaction: &mut Transaction<'_, Sqlite>,
    keyring: &SecretKeyring,
    continuation: &InteractiveAuthContinuation,
    row: &SqliteRow,
    access: &SecretAccess,
) -> Result<SecretValue, SecretStoreError> {
    let secret_id = SecretId::from_str(row.try_get("secret_blob_id").map_err(storage_error)?)
        .map_err(|_| SecretStoreError::Storage)?;
    let stored = fetch_secret(transaction, secret_id).await?;
    validate_stored_secret(&stored, continuation)?;
    let secret = SecretRef {
        id: secret_id,
        owner_user_id: stored.owner_user_id,
        purpose: stored.purpose,
        version: stored.version,
        key_id: stored.key_id.clone(),
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    };
    let plaintext = decrypt(
        keyring.get(&stored.key_id)?,
        &secret,
        &stored.nonce,
        &stored.encrypted_data,
    )?;
    if digest(&plaintext) != continuation.continuation_digest {
        return Err(SecretStoreError::AuthenticationFailed);
    }
    insert_secret_audit(
        transaction,
        access,
        "interactive_auth_continuation_accessed",
        &secret,
    )
    .await
    .map_err(storage_error)?;
    Ok(SecretValue::new(plaintext))
}

fn validate_stored_secret(
    stored: &crate::secret::StoredSecret,
    continuation: &InteractiveAuthContinuation,
) -> Result<(), SecretStoreError> {
    if stored.purpose != SecretPurpose::BrowserJobCredential
        || stored.version != continuation.revision
        || stored.updated_at != continuation.updated_at
    {
        Err(SecretStoreError::VersionConflict)
    } else {
        Ok(())
    }
}

async fn claim_matches(
    transaction: &mut Transaction<'_, Sqlite>,
    claim: &InteractiveAuthPollClaim,
) -> Result<bool, SecretStoreError> {
    let query = format!(
        "{CONTINUATION_SELECT} WHERE auth_session_id = ? AND provider_id = ? AND revision = ? \
         AND poll_count = ? AND active_poll_sequence = ? AND active_poll_digest = ?"
    );
    let Some(row) = sqlx::query(&query)
        .bind(claim.continuation.auth_session_id.to_string())
        .bind(claim.continuation.provider_id.as_str())
        .bind(i64::from(claim.continuation.revision))
        .bind(i64::from(claim.poll_sequence))
        .bind(i64::from(claim.poll_sequence))
        .bind(claim.claim_digest.as_slice())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
    else {
        return Ok(false);
    };
    Ok(decode_continuation(&row)? == claim.continuation)
}

async fn clear_claim(
    transaction: &mut Transaction<'_, Sqlite>,
    claim: &InteractiveAuthPollClaim,
    _at: Timestamp,
) -> Result<bool, SecretStoreError> {
    Ok(sqlx::query(
        "UPDATE interactive_auth_continuations SET active_poll_sequence = NULL, \
         active_poll_digest = NULL, active_poll_expires_at = NULL \
         WHERE auth_session_id = ? AND revision = ? AND active_poll_sequence = ? \
         AND active_poll_digest = ?",
    )
    .bind(claim.continuation.auth_session_id.to_string())
    .bind(i64::from(claim.continuation.revision))
    .bind(i64::from(claim.poll_sequence))
    .bind(claim.claim_digest.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?
    .rows_affected()
        == 1)
}

async fn finish_operation(
    transaction: &mut Transaction<'_, Sqlite>,
    claim: &InteractiveAuthPollClaim,
    state: &str,
    result_digest: Option<[u8; 32]>,
    completed_at: Timestamp,
) -> Result<(), SecretStoreError> {
    let updated = sqlx::query(
        "UPDATE interactive_auth_poll_operations SET state = ?, result_digest = ?, \
         completed_at = ? WHERE auth_session_id = ? AND poll_sequence = ? \
         AND continuation_revision = ? AND request_digest = ? AND state = 'issued'",
    )
    .bind(state)
    .bind(result_digest.map(|digest| digest.to_vec()))
    .bind(encode_timestamp(completed_at))
    .bind(claim.continuation.auth_session_id.to_string())
    .bind(i64::from(claim.poll_sequence))
    .bind(i64::from(claim.continuation.revision))
    .bind(claim.claim_digest.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(SecretStoreError::VersionConflict)
    }
}

async fn fetch_secret_id(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthSessionId,
) -> Result<SecretId, SecretStoreError> {
    let value: String = sqlx::query_scalar(
        "SELECT secret_blob_id FROM interactive_auth_continuations WHERE auth_session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    SecretId::from_str(&value).map_err(|_| SecretStoreError::Storage)
}

async fn delete_continuation_secret(
    transaction: &mut Transaction<'_, Sqlite>,
    claim: &InteractiveAuthPollClaim,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    delete_continuation_secret_metadata(transaction, &claim.continuation, access).await
}

async fn delete_continuation_secret_metadata(
    transaction: &mut Transaction<'_, Sqlite>,
    continuation: &InteractiveAuthContinuation,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    let secret_id = fetch_secret_id(transaction, continuation.auth_session_id).await?;
    let stored = fetch_secret(transaction, secret_id).await?;
    validate_stored_secret(&stored, continuation)?;
    let secret = SecretRef {
        id: secret_id,
        owner_user_id: stored.owner_user_id,
        purpose: stored.purpose,
        version: stored.version,
        key_id: stored.key_id,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    };
    insert_secret_audit(
        transaction,
        access,
        "interactive_auth_continuation_deleted",
        &secret,
    )
    .await
    .map_err(storage_error)?;
    sqlx::query("DELETE FROM secret_blobs WHERE id = ?")
        .bind(secret_id.to_string())
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(())
}

pub(crate) async fn consume_interactive_auth_candidate(
    transaction: &mut Transaction<'_, Sqlite>,
    expected: &InteractiveAuthContinuation,
    terminal_result_digest: [u8; 32],
    owner_user_id: asterism_domain::UserId,
    access: &SecretAccess,
) -> Result<bool, SecretStoreError> {
    authorize(owner_user_id, access)?;
    let Some(row) = fetch_continuation_row(transaction, expected.auth_session_id).await? else {
        return Ok(false);
    };
    let stored_continuation = decode_continuation(&row)?;
    let active_sequence: Option<i64> =
        row.try_get("active_poll_sequence").map_err(storage_error)?;
    if &stored_continuation != expected
        || stored_continuation.terminal_result_digest != Some(terminal_result_digest)
        || active_sequence.is_some()
    {
        return Ok(false);
    }
    let secret_id = SecretId::from_str(row.try_get("secret_blob_id").map_err(storage_error)?)
        .map_err(|_| SecretStoreError::Storage)?;
    let stored = fetch_secret(transaction, secret_id).await?;
    validate_stored_secret(&stored, expected)?;
    if stored.owner_user_id != owner_user_id {
        return Err(SecretStoreError::Unauthorized);
    }
    let secret = SecretRef {
        id: secret_id,
        owner_user_id: stored.owner_user_id,
        purpose: stored.purpose,
        version: stored.version,
        key_id: stored.key_id,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    };
    insert_secret_audit(
        transaction,
        access,
        "interactive_auth_candidate_consumed",
        &secret,
    )
    .await
    .map_err(storage_error)?;
    let deleted = sqlx::query("DELETE FROM secret_blobs WHERE id = ?")
        .bind(secret_id.to_string())
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(deleted.rows_affected() == 1)
}

fn authorize_claim(
    claim: &InteractiveAuthPollClaim,
    provider_id: &ProviderId,
    _access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    if &claim.continuation.provider_id != provider_id {
        return Err(SecretStoreError::Unauthorized);
    }
    Ok(())
}

fn audit_actor(access: &SecretAccess) -> Result<AuditActor, SecretStoreError> {
    match access.actor {
        SecretActor::User(user_id) => Ok(AuditActor::User(user_id)),
        SecretActor::DelegatedUser { actor_user_id, .. } => Ok(AuditActor::User(actor_user_id)),
        SecretActor::ServiceToken(token_id) => Ok(AuditActor::ServiceToken(token_id)),
        SecretActor::CoreService(_) | SecretActor::ProviderRuntime(_) => {
            Err(SecretStoreError::Unauthorized)
        }
    }
}

fn validate_label(provider_id: &ProviderId, value: &str) -> Result<(), SecretStoreError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_LABEL_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && value
            .strip_prefix(provider_id.as_str())
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1);
    if valid {
        Ok(())
    } else {
        Err(SecretStoreError::InvalidValue)
    }
}

fn poll_claim_digest(
    provider_id: &ProviderId,
    session_id: AuthSessionId,
    revision: u32,
    poll_sequence: u32,
    correlation_id: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"asterism.interactive-auth-poll-claim.v1\0");
    hasher.update(provider_id.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(session_id.to_string().as_bytes());
    hasher.update(revision.to_be_bytes());
    hasher.update(poll_sequence.to_be_bytes());
    hasher.update(correlation_id.as_bytes());
    hasher.finalize().into()
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn decode_digest(row: &SqliteRow, column: &str) -> Result<[u8; 32], SecretStoreError> {
    row.try_get::<Vec<u8>, _>(column)
        .map_err(storage_error)?
        .try_into()
        .map_err(|_| SecretStoreError::Storage)
}

fn decode_u32(row: &SqliteRow, column: &str) -> Result<u32, SecretStoreError> {
    u32::try_from(row.try_get::<i64, _>(column).map_err(storage_error)?)
        .map_err(|_| SecretStoreError::Storage)
}

fn decode_optional_u32(row: &SqliteRow, column: &str) -> Result<Option<u32>, SecretStoreError> {
    row.try_get::<Option<i64>, _>(column)
        .map_err(storage_error)?
        .map(|value| u32::try_from(value).map_err(|_| SecretStoreError::Storage))
        .transpose()
}

fn optional_timestamp(
    row: &SqliteRow,
    column: &str,
) -> Result<Option<Timestamp>, SecretStoreError> {
    row.try_get::<Option<&str>, _>(column)
        .map_err(storage_error)?
        .map(decode_timestamp)
        .transpose()
        .map_err(storage_error)
}

fn storage_error<E>(_: E) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_domain::{
        AuthMethod, ProviderAccountId, ProviderId, SessionKind, Timestamp, UserId, WaitingUserState,
    };
    use asterism_secrets::{
        CredentialAcquisition, CredentialBundle, CredentialField, SecretKey, SecretPurpose,
        SecretValue,
    };
    use chrono::Utc;

    use super::*;
    use crate::{
        AuthSessionRepository, InteractiveAuthCredentialCommitOutcome,
        InteractiveAuthCredentialCommitRequest, InteractiveAuthCredentialRepository,
        SqliteAuthSessionRepository, SqliteSecretStore,
    };

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one lifecycle proves poll serialization, retry, rotation and restart-safe terminal candidate recovery"
    )]
    async fn interactive_auth_poll_is_serialized_rotated_and_restart_safe() {
        let fixture = Fixture::new().await;
        let (mut session, _) = fixture.attach(b"qr-initial", 4).await;

        let first = fixture
            .claim(
                session.id,
                fixture.now + Duration::seconds(2),
                "qr-poll-one",
            )
            .await
            .unwrap();
        let InteractiveAuthPollClaimOutcome::Claimed {
            claim: first_claim,
            value,
        } = first
        else {
            panic!("expected first poll claim");
        };
        assert_eq!(value.expose_secret(), b"qr-initial");
        assert!(matches!(
            fixture
                .claim(
                    session.id,
                    fixture.now + Duration::seconds(3),
                    "qr-poll-busy",
                )
                .await
                .unwrap(),
            InteractiveAuthPollClaimOutcome::Busy
        ));
        fixture
            .repository
            .release_interactive_auth_poll(
                &first_claim,
                fixture.now + Duration::seconds(4),
                &fixture.access("qr-poll-release"),
            )
            .await
            .unwrap();

        let second = fixture
            .claim(
                session.id,
                fixture.now + Duration::seconds(5),
                "qr-poll-two",
            )
            .await
            .unwrap();
        let InteractiveAuthPollClaimOutcome::Claimed {
            claim: second_claim,
            value,
        } = second
        else {
            panic!("expected second poll claim");
        };
        assert_eq!(value.expose_secret(), b"qr-initial");
        assert_eq!(second_claim.poll_sequence, 2);
        let expected_session_revision = session.revision;
        session
            .transition(
                AuthState::WaitingUser(WaitingUserState::QrConfirm),
                fixture.now + Duration::seconds(6),
            )
            .unwrap();
        let next_value = b"qr-confirm";
        let rotated = fixture
            .repository
            .rotate_interactive_auth_continuation(InteractiveAuthPollRotateRequest {
                claim: &second_claim,
                waiting_session: &session,
                expected_session_revision,
                continuation_type: "chaoxing.qr.v1",
                continuation_digest: digest(next_value),
                phase: "chaoxing.qr-confirm",
                replacement: SecretValue::new(next_value.to_vec()),
                result_digest: [1; 32],
                completed_at: fixture.now + Duration::seconds(6),
                access: &fixture.access("qr-poll-waiting"),
            })
            .await
            .unwrap();
        assert!(matches!(
            rotated,
            InteractiveAuthContinuationMutationOutcome::Rotated(ref continuation)
                if continuation.revision == 2 && continuation.poll_count == 2
        ));

        let third = fixture
            .claim(
                session.id,
                fixture.now + Duration::seconds(7),
                "qr-poll-three",
            )
            .await
            .unwrap();
        let InteractiveAuthPollClaimOutcome::Claimed {
            claim: third_claim,
            value,
        } = third
        else {
            panic!("expected third poll claim");
        };
        assert_eq!(value.expose_secret(), next_value);
        let expected_session_revision = session.revision;
        session
            .transition(
                AuthState::ValidatingCredential,
                fixture.now + Duration::seconds(8),
            )
            .unwrap();
        let terminal = b"authenticated-cookie-candidate";
        let persisted = fixture
            .repository
            .persist_interactive_auth_candidate(InteractiveAuthPollAuthenticateRequest {
                claim: &third_claim,
                validating_session: &session,
                expected_session_revision,
                continuation_type: "chaoxing.qr-terminal.v1",
                continuation_digest: digest(terminal),
                phase: "chaoxing.authenticated",
                replacement: SecretValue::new(terminal.to_vec()),
                result_digest: [2; 32],
                completed_at: fixture.now + Duration::seconds(8),
                access: &fixture.access("qr-poll-authenticated"),
            })
            .await
            .unwrap();
        assert!(matches!(
            persisted,
            InteractiveAuthContinuationMutationOutcome::AuthenticatedCandidate(
                ref continuation
            ) if continuation.terminal_result_digest == Some([2; 32])
        ));

        let restarted = SqliteInteractiveAuthContinuationRepository::new(
            fixture.database.clone(),
            fixture.keyring.clone(),
            fixture.provider.clone(),
        );
        let resolved = restarted
            .resolve_interactive_auth_candidate(
                fixture.owner,
                fixture.account,
                session.id,
                &fixture.access("qr-terminal-recover"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.session, session);
        assert_eq!(resolved.value.expose_secret(), terminal);
        let mut authenticated_session = resolved.session.clone();
        let expected_session_revision = authenticated_session.revision;
        authenticated_session
            .transition(AuthState::Authenticated, fixture.now + Duration::seconds(9))
            .unwrap();
        let committed = SqliteSecretStore::new(fixture.database.clone(), fixture.keyring.clone())
            .commit_interactive_auth_credentials(InteractiveAuthCredentialCommitRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                authenticated_session: &authenticated_session,
                expected_session_revision,
                continuation: &resolved.continuation,
                terminal_result_digest: [2; 32],
                bundle: CredentialBundle {
                    provider_id: fixture.provider.clone(),
                    tenant: None,
                    auth_method: AuthMethod::QrCode,
                    acquired_via: CredentialAcquisition::NativeProviderLogin,
                    captured_at: fixture.now,
                    expires_at: None,
                    session_kind: SessionKind::Cookie,
                    fields: vec![CredentialField {
                        purpose: SecretPurpose::ProviderCookie,
                        value: SecretValue::new(b"_uid=bound-user".to_vec()),
                    }],
                    user_id_hint: Some("bound-user".to_owned()),
                },
                access: &fixture.access("qr-credential-commit"),
            })
            .await
            .unwrap();
        assert!(matches!(
            committed,
            InteractiveAuthCredentialCommitOutcome::Committed(ref commit)
                if commit.session == authenticated_session && commit.credentials.len() == 1
        ));
        assert!(
            restarted
                .resolve_interactive_auth_candidate(
                    fixture.owner,
                    fixture.account,
                    session.id,
                    &fixture.access("qr-terminal-consumed"),
                )
                .await
                .unwrap()
                .is_none()
        );
        let states: Vec<String> = sqlx::query_scalar(
            "SELECT state FROM interactive_auth_poll_operations WHERE auth_session_id = ? \
             ORDER BY poll_sequence",
        )
        .bind(session.id.to_string())
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(states, ["retryable", "waiting", "authenticated"]);
    }

    #[tokio::test]
    async fn interactive_auth_poll_budget_and_terminal_cleanup_fail_closed() {
        let fixture = Fixture::new().await;
        let (mut session, _) = fixture.attach(b"one-poll", 1).await;
        let InteractiveAuthPollClaimOutcome::Claimed { claim, .. } = fixture
            .claim(
                session.id,
                fixture.now + Duration::seconds(2),
                "one-poll-claim",
            )
            .await
            .unwrap()
        else {
            panic!("expected bounded poll claim");
        };
        let expected_session_revision = session.revision;
        session
            .transition(AuthState::AuthFailed, fixture.now + Duration::seconds(3))
            .unwrap();
        let outcome = fixture
            .repository
            .finish_interactive_auth_terminal(InteractiveAuthPollTerminalRequest {
                claim: &claim,
                terminal_session: &session,
                expected_session_revision,
                terminal_state: InteractiveAuthTerminalState::Rejected,
                result_digest: [3; 32],
                completed_at: fixture.now + Duration::seconds(3),
                access: &fixture.access("one-poll-rejected"),
            })
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            InteractiveAuthContinuationMutationOutcome::Terminal(ref terminal)
                if terminal.state == AuthState::AuthFailed
        ));
        let continuation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM interactive_auth_continuations WHERE auth_session_id = ?",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        let secret_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM secret_blobs WHERE purpose = 'browser_job_credential'",
        )
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!((continuation_count, secret_count), (0, 0));
    }

    #[tokio::test]
    async fn retryable_polls_still_consume_the_fixed_budget() {
        let fixture = Fixture::new().await;
        let (session, _) = fixture.attach(b"one-retry", 1).await;
        let InteractiveAuthPollClaimOutcome::Claimed { claim, .. } = fixture
            .claim(
                session.id,
                fixture.now + Duration::seconds(2),
                "retry-budget-one",
            )
            .await
            .unwrap()
        else {
            panic!("expected first poll claim");
        };
        assert!(
            fixture
                .repository
                .release_interactive_auth_poll(
                    &claim,
                    fixture.now + Duration::seconds(3),
                    &fixture.access("retry-budget-release"),
                )
                .await
                .unwrap()
        );
        assert!(matches!(
            fixture
                .claim(
                    session.id,
                    fixture.now + Duration::seconds(4),
                    "retry-budget-exhausted",
                )
                .await
                .unwrap(),
            InteractiveAuthPollClaimOutcome::Exhausted
        ));
    }

    #[tokio::test]
    async fn expired_poll_lease_is_closed_before_the_next_claim() {
        let fixture = Fixture::new().await;
        let (session, _) = fixture.attach(b"lease-recovery", 2).await;
        let InteractiveAuthPollClaimOutcome::Claimed { claim, .. } = fixture
            .claim(
                session.id,
                fixture.now + Duration::seconds(2),
                "lease-first",
            )
            .await
            .unwrap()
        else {
            panic!("expected first poll claim");
        };
        let recovered = fixture
            .claim(
                session.id,
                claim.claim_expires_at + Duration::seconds(1),
                "lease-recovered",
            )
            .await
            .unwrap();
        let InteractiveAuthPollClaimOutcome::Claimed {
            claim: recovered_claim,
            ..
        } = recovered
        else {
            panic!("expected recovered poll claim");
        };
        assert_eq!(recovered_claim.poll_sequence, 2);
        let states: Vec<String> = sqlx::query_scalar(
            "SELECT state FROM interactive_auth_poll_operations WHERE auth_session_id = ? \
             ORDER BY poll_sequence",
        )
        .bind(session.id.to_string())
        .fetch_all(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(states, ["retryable", "issued"]);
    }

    struct Fixture {
        database: Database,
        keyring: Arc<SecretKeyring>,
        repository: SqliteInteractiveAuthContinuationRepository,
        sessions: SqliteAuthSessionRepository,
        owner: UserId,
        account: ProviderAccountId,
        provider: ProviderId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let owner = UserId::new();
            let account = ProviderAccountId::new();
            let provider = ProviderId::new("chaoxing").unwrap();
            let now = Utc::now();
            let timestamp = encode_timestamp(now);
            sqlx::query(
                "INSERT INTO users \
                 (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
                 VALUES (?, 'interactive-auth-owner', '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
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
                 VALUES (?, ?, ?, 'Interactive Auth', '\"idle\"', ?, ?)",
            )
            .bind(account.to_string())
            .bind(owner.to_string())
            .bind(provider.as_str())
            .bind(&timestamp)
            .bind(&timestamp)
            .execute(database.pool())
            .await
            .unwrap();
            let mut keys = BTreeMap::new();
            keys.insert("interactive-key".to_owned(), SecretKey::new([31; 32]));
            let keyring = Arc::new(SecretKeyring::new("interactive-key", keys).unwrap());
            let repository = SqliteInteractiveAuthContinuationRepository::new(
                database.clone(),
                keyring.clone(),
                provider.clone(),
            );
            let sessions = SqliteAuthSessionRepository::new(database.clone());
            Self {
                database,
                keyring,
                repository,
                sessions,
                owner,
                account,
                provider,
                now,
            }
        }

        async fn attach(&self, value: &[u8], maximum_polls: u32) -> (AuthSession, [u8; 32]) {
            let mut session = AuthSession::starting(
                self.owner,
                self.account,
                AuthMethod::QrCode,
                self.now,
                self.now + Duration::minutes(5),
            )
            .unwrap();
            self.sessions
                .create_auth_session(
                    &session,
                    AuditActor::User(self.owner),
                    "interactive-auth-create",
                )
                .await
                .unwrap();
            session
                .transition(
                    AuthState::WaitingUser(WaitingUserState::QrScan),
                    self.now + Duration::seconds(1),
                )
                .unwrap();
            let value_digest = digest(value);
            self.repository
                .attach_interactive_auth_continuation(InteractiveAuthContinuationAttachRequest {
                    session: &session,
                    expected_session_revision: 1,
                    provider_id: &self.provider,
                    continuation_type: "chaoxing.qr.v1",
                    continuation_digest: value_digest,
                    phase: "chaoxing.qr-scan",
                    value: SecretValue::new(value.to_vec()),
                    maximum_polls,
                    expires_at: self.now + Duration::minutes(4),
                    attached_at: self.now + Duration::seconds(1),
                    access: &self.access("interactive-auth-attach"),
                })
                .await
                .unwrap();
            (session, value_digest)
        }

        async fn claim(
            &self,
            auth_session_id: AuthSessionId,
            claimed_at: Timestamp,
            correlation_id: &str,
        ) -> Result<InteractiveAuthPollClaimOutcome, SecretStoreError> {
            let access = self.access(correlation_id);
            self.repository
                .claim_interactive_auth_poll(InteractiveAuthPollClaimRequest {
                    owner_user_id: self.owner,
                    provider_account_id: self.account,
                    auth_session_id,
                    claimed_at,
                    claim_expires_at: claimed_at + Duration::seconds(30),
                    access: &access,
                })
                .await
        }

        fn access(&self, correlation_id: &str) -> SecretAccess {
            SecretAccess {
                actor: SecretActor::User(self.owner),
                correlation_id: correlation_id.to_owned(),
                reason: "interactive authentication".to_owned(),
            }
        }
    }
}
