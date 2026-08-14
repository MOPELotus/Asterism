use std::{str::FromStr, sync::Arc};

use asterism_domain::{
    AuditRecordId, BrowserBridgeExchangeState, BrowserBridgeResultArtifactMetadata,
    BrowserBridgeRuntimeStateMetadata, ProviderId, SecretId,
};
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};

use crate::{
    BrowserBridgeCommandArtifactRepository, BrowserBridgeCommandDispatchRecord,
    BrowserBridgeCommandDispatchRequest, BrowserBridgeCommandIssueRequest,
    BrowserBridgeCommandResolveRequest, BrowserBridgeExchangeRecord,
    BrowserBridgeResultArtifactRecord, BrowserBridgeResultReceiveRequest,
    BrowserBridgeResultResolveRequest, Database, DispatchedBrowserBridgeCommand,
    ResolvedBrowserBridgeCommand, ResolvedBrowserBridgeResult, ResolvedBrowserBridgeRuntimeState,
    SecretKeyring,
    browser_bridge::{
        authenticate_session_for_exchange, binding_is_valid, fetch_exchange, fetch_runtime_binding,
        fetch_session, find_claimed_session_for_exchange, insert_exchange_audit,
    },
    secret::{
        authorize, decrypt, encrypt, fetch_secret, insert_secret_audit, insert_secret_blob,
        validate_secret,
    },
};

/// Encrypted `BrowserBridge` command storage permanently scoped to one Provider.
#[derive(Clone, Debug)]
pub struct SqliteBrowserBridgeCommandArtifactRepository {
    database: Database,
    keyring: Arc<SecretKeyring>,
    provider_id: ProviderId,
}

impl SqliteBrowserBridgeCommandArtifactRepository {
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
impl BrowserBridgeCommandArtifactRepository for SqliteBrowserBridgeCommandArtifactRepository {
    async fn issue_browser_bridge_command(
        &self,
        request: BrowserBridgeCommandIssueRequest<'_>,
    ) -> Result<BrowserBridgeExchangeRecord, SecretStoreError> {
        request
            .exchange
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        validate_secret(&request.command_artifact)?;
        if request.exchange.state != BrowserBridgeExchangeState::Issued
            || digest(request.command_artifact.expose_secret()) != request.exchange.command_digest
        {
            return Err(SecretStoreError::InvalidValue);
        }
        if let Some(runtime_state) = &request.runtime_state {
            runtime_state
                .metadata
                .validate()
                .map_err(|_| SecretStoreError::InvalidValue)?;
            validate_secret(&runtime_state.state_artifact)?;
            if runtime_state.metadata.session_id != request.exchange.session_id
                || runtime_state.metadata.sequence != request.exchange.sequence
                || runtime_state.metadata.stored_at != request.exchange.issued_at
                || digest(runtime_state.state_artifact.expose_secret())
                    != runtime_state.metadata.state_digest
            {
                return Err(SecretStoreError::InvalidValue);
            }
        }

        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(session) = find_claimed_session_for_exchange(
            &mut transaction,
            request.exchange.session_id,
            request.exchange.issued_at,
        )
        .await
        .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeExchangeRecord::AccessRejected);
        };
        authorize_scoped(
            session.owner_user_id,
            &session.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if fetch_runtime_binding(&mut transaction, request.exchange.session_id)
            .await
            .map_err(storage_error)?
            .is_none()
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeExchangeRecord::SequenceConflict);
        }

        let sequence =
            i64::try_from(request.exchange.sequence).map_err(|_| SecretStoreError::InvalidValue)?;
        let last_sequence: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM browser_bridge_exchanges WHERE session_id = ?",
        )
        .bind(request.exchange.session_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let expected = last_sequence.map_or(1, |value| value.saturating_add(1));
        if sequence != expected {
            let existing = fetch_exchange(&mut transaction, request.exchange.session_id, sequence)
                .await
                .map_err(storage_error)?;
            let same = existing.as_ref().is_some_and(|existing| {
                existing.command_type == request.exchange.command_type
                    && existing.command_digest == request.exchange.command_digest
                    && existing.issued_at == request.exchange.issued_at
            });
            let artifact_present: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM browser_bridge_exchanges \
                 WHERE session_id = ? AND sequence = ? AND command_secret_blob_id IS NOT NULL)",
            )
            .bind(request.exchange.session_id.to_string())
            .bind(sequence)
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage_error)?;
            let existing_state = fetch_runtime_state_metadata(
                &mut transaction,
                request.exchange.session_id,
                sequence,
            )
            .await?;
            let state_same = match (&request.runtime_state, existing_state) {
                (None, None) => true,
                (Some(requested), Some(existing)) => requested.metadata == existing,
                (None, Some(_)) | (Some(_), None) => false,
            };
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(if same && artifact_present == 1 && state_same {
                BrowserBridgeExchangeRecord::Duplicate(existing.expect("same requires a record"))
            } else {
                BrowserBridgeExchangeRecord::SequenceConflict
            });
        }

        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id: session.owner_user_id,
            purpose: SecretPurpose::BrowserJobCredential,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.exchange.issued_at,
            updated_at: request.exchange.issued_at,
        };
        let (nonce, encrypted_data) =
            encrypt(key, &secret, request.command_artifact.expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted_data).await?;
        sqlx::query(
            "INSERT INTO browser_bridge_exchanges \
             (session_id, sequence, command_type, command_digest, state, issued_at, \
              command_secret_blob_id) VALUES (?, ?, ?, ?, 'issued', ?, ?)",
        )
        .bind(request.exchange.session_id.to_string())
        .bind(sequence)
        .bind(&request.exchange.command_type)
        .bind(request.exchange.command_digest.as_slice())
        .bind(encode_timestamp(request.exchange.issued_at))
        .bind(secret.id.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some(runtime_state) = request.runtime_state {
            let runtime_secret = SecretRef {
                id: SecretId::new(),
                owner_user_id: session.owner_user_id,
                purpose: SecretPurpose::BrowserJobCredential,
                version: 1,
                key_id: key_id.to_owned(),
                created_at: runtime_state.metadata.stored_at,
                updated_at: runtime_state.metadata.stored_at,
            };
            let (runtime_nonce, runtime_encrypted_data) = encrypt(
                key,
                &runtime_secret,
                runtime_state.state_artifact.expose_secret(),
            )?;
            insert_secret_blob(
                &mut transaction,
                &runtime_secret,
                &runtime_nonce,
                &runtime_encrypted_data,
            )
            .await?;
            insert_runtime_state(&mut transaction, &runtime_state.metadata, runtime_secret.id)
                .await?;
            insert_secret_audit(
                &mut transaction,
                request.access,
                "browser_bridge_runtime_state_stored",
                &runtime_secret,
            )
            .await
            .map_err(storage_error)?;
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "browser_bridge_command_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_exchange_audit(
            &mut transaction,
            &request.access.correlation_id,
            &session,
            request.exchange,
            "issued",
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(BrowserBridgeExchangeRecord::Inserted(
            request.exchange.clone(),
        ))
    }

    async fn resolve_browser_bridge_command(
        &self,
        request: BrowserBridgeCommandResolveRequest<'_>,
    ) -> Result<Option<ResolvedBrowserBridgeCommand>, SecretStoreError> {
        let sequence =
            i64::try_from(request.sequence).map_err(|_| SecretStoreError::InvalidValue)?;
        if sequence < 1 {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some((session, _)) = fetch_session(&mut transaction, request.session_id)
            .await
            .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        authorize_scoped(
            session.owner_user_id,
            &session.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if session.owner_user_id != request.owner_user_id
            || session.provider_account_id != request.provider_account_id
            || session.task_id != request.task_id
            || !binding_is_valid(&mut transaction, &session)
                .await
                .map_err(storage_error)?
        {
            return Err(SecretStoreError::Unauthorized);
        }
        let Some(exchange) = fetch_exchange(&mut transaction, request.session_id, sequence)
            .await
            .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        let secret_id: Option<String> = sqlx::query_scalar(
            "SELECT command_secret_blob_id FROM browser_bridge_exchanges \
             WHERE session_id = ? AND sequence = ?",
        )
        .bind(request.session_id.to_string())
        .bind(sequence)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let secret_id = secret_id.ok_or(SecretStoreError::VersionConflict)?;
        let secret_id = SecretId::from_str(&secret_id).map_err(|_| SecretStoreError::Storage)?;
        let stored = fetch_secret(&mut transaction, secret_id).await?;
        if stored.owner_user_id != session.owner_user_id
            || stored.purpose != SecretPurpose::BrowserJobCredential
        {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        let secret = SecretRef {
            id: secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: stored.version,
            key_id: stored.key_id.clone(),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
        };
        let key = self.keyring.get(&stored.key_id)?;
        let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
        if digest(&plaintext) != exchange.command_digest {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        let runtime_state = resolve_runtime_state(
            &mut transaction,
            request.session_id,
            sequence,
            session.owner_user_id,
            &self.keyring,
            request.access,
        )
        .await?;
        if runtime_state
            .as_ref()
            .is_some_and(|runtime_state| runtime_state.metadata.stored_at != exchange.issued_at)
        {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "browser_bridge_command_resolved",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedBrowserBridgeCommand {
            exchange,
            command_artifact: SecretValue::new(plaintext),
            runtime_state,
        }))
    }

    async fn dispatch_browser_bridge_command(
        &self,
        request: BrowserBridgeCommandDispatchRequest<'_>,
    ) -> Result<BrowserBridgeCommandDispatchRecord, SecretStoreError> {
        let sequence =
            i64::try_from(request.sequence).map_err(|_| SecretStoreError::InvalidValue)?;
        if sequence < 1 {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(session) = authenticate_session_for_exchange(
            &mut transaction,
            request.session_id,
            request.access_token_digest,
            request.dispatched_at,
        )
        .await
        .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::AccessRejected);
        };
        authorize_scoped(
            session.owner_user_id,
            &session.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if fetch_runtime_binding(&mut transaction, request.session_id)
            .await
            .map_err(storage_error)?
            .is_none()
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::SequenceConflict);
        }
        let Some(exchange) = fetch_exchange(&mut transaction, request.session_id, sequence)
            .await
            .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::NotFound);
        };
        if exchange.state != BrowserBridgeExchangeState::Issued
            || request.dispatched_at < exchange.issued_at
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::SequenceConflict);
        }
        let result_present: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM browser_bridge_result_artifacts \
             WHERE session_id = ? AND sequence = ?)",
        )
        .bind(request.session_id.to_string())
        .bind(sequence)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if result_present != 0 {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::SequenceConflict);
        }
        let secret_id: Option<String> = sqlx::query_scalar(
            "SELECT command_secret_blob_id FROM browser_bridge_exchanges \
             WHERE session_id = ? AND sequence = ?",
        )
        .bind(request.session_id.to_string())
        .bind(sequence)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let Some(secret_id) = secret_id else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::SequenceConflict);
        };
        let secret_id = SecretId::from_str(&secret_id).map_err(|_| SecretStoreError::Storage)?;
        let stored = fetch_secret(&mut transaction, secret_id).await?;
        if stored.owner_user_id != session.owner_user_id
            || stored.purpose != SecretPurpose::BrowserJobCredential
        {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        let secret = SecretRef {
            id: secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: stored.version,
            key_id: stored.key_id.clone(),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
        };
        let key = self.keyring.get(&stored.key_id)?;
        let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
        if digest(&plaintext) != exchange.command_digest {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        let inserted = sqlx::query(
            "INSERT INTO browser_bridge_command_dispatches \
             (session_id, sequence, dispatched_at) VALUES (?, ?, ?) \
             ON CONFLICT(session_id, sequence) DO NOTHING",
        )
        .bind(request.session_id.to_string())
        .bind(sequence)
        .bind(encode_timestamp(request.dispatched_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if inserted.rows_affected() != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCommandDispatchRecord::AlreadyDispatched);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "browser_bridge_command_dispatched",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_dispatch_audit(
            &mut transaction,
            request.access,
            &exchange,
            request.dispatched_at,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(BrowserBridgeCommandDispatchRecord::Dispatched(
            DispatchedBrowserBridgeCommand {
                exchange,
                command_artifact: SecretValue::new(plaintext),
            },
        ))
    }

    async fn receive_browser_bridge_result(
        &self,
        request: BrowserBridgeResultReceiveRequest<'_>,
    ) -> Result<BrowserBridgeResultArtifactRecord, SecretStoreError> {
        request
            .metadata
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        validate_secret(&request.result_artifact)?;
        if digest(request.result_artifact.expose_secret()) != request.metadata.result_digest {
            return Err(SecretStoreError::InvalidValue);
        }
        let sequence =
            i64::try_from(request.metadata.sequence).map_err(|_| SecretStoreError::InvalidValue)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(session) = authenticate_session_for_exchange(
            &mut transaction,
            request.metadata.session_id,
            request.access_token_digest,
            request.metadata.received_at,
        )
        .await
        .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeResultArtifactRecord::AccessRejected);
        };
        authorize_scoped(
            session.owner_user_id,
            &session.provider_id,
            &self.provider_id,
            request.access,
        )?;
        let Some(exchange) =
            fetch_exchange(&mut transaction, request.metadata.session_id, sequence)
                .await
                .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeResultArtifactRecord::SequenceConflict);
        };
        let dispatched_at: Option<String> = sqlx::query_scalar(
            "SELECT dispatch.dispatched_at FROM browser_bridge_exchanges AS exchange \
             JOIN browser_bridge_command_dispatches AS dispatch \
               ON dispatch.session_id = exchange.session_id \
              AND dispatch.sequence = exchange.sequence \
             WHERE exchange.session_id = ? AND exchange.sequence = ? \
               AND exchange.command_secret_blob_id IS NOT NULL",
        )
        .bind(request.metadata.session_id.to_string())
        .bind(sequence)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let Some(dispatched_at) = dispatched_at else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeResultArtifactRecord::SequenceConflict);
        };
        let dispatched_at = decode_timestamp(&dispatched_at)?;
        if request.metadata.received_at < exchange.issued_at
            || request.metadata.received_at < dispatched_at
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeResultArtifactRecord::SequenceConflict);
        }
        if let Some(existing) =
            fetch_result_metadata(&mut transaction, request.metadata.session_id, sequence).await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(
                if existing.result_type == request.metadata.result_type
                    && existing.result_digest == request.metadata.result_digest
                {
                    BrowserBridgeResultArtifactRecord::Duplicate(existing)
                } else {
                    BrowserBridgeResultArtifactRecord::SequenceConflict
                },
            );
        }
        if exchange.state != BrowserBridgeExchangeState::Issued {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeResultArtifactRecord::SequenceConflict);
        }

        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id: session.owner_user_id,
            purpose: SecretPurpose::BrowserJobCredential,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: request.metadata.received_at,
            updated_at: request.metadata.received_at,
        };
        let (nonce, encrypted_data) =
            encrypt(key, &secret, request.result_artifact.expose_secret())?;
        insert_secret_blob(&mut transaction, &secret, &nonce, &encrypted_data).await?;
        sqlx::query(
            "INSERT INTO browser_bridge_result_artifacts \
             (session_id, sequence, result_type, result_digest, secret_blob_id, received_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(request.metadata.session_id.to_string())
        .bind(sequence)
        .bind(&request.metadata.result_type)
        .bind(request.metadata.result_digest.as_slice())
        .bind(secret.id.to_string())
        .bind(encode_timestamp(request.metadata.received_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(
            &mut transaction,
            request.access,
            "browser_bridge_result_stored",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        insert_result_audit(
            &mut transaction,
            request.access,
            request.metadata,
            "received",
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(BrowserBridgeResultArtifactRecord::Inserted(
            request.metadata.clone(),
        ))
    }

    async fn resolve_browser_bridge_result(
        &self,
        request: BrowserBridgeResultResolveRequest<'_>,
    ) -> Result<Option<ResolvedBrowserBridgeResult>, SecretStoreError> {
        let sequence =
            i64::try_from(request.sequence).map_err(|_| SecretStoreError::InvalidValue)?;
        if sequence < 1 {
            return Err(SecretStoreError::InvalidValue);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some((session, _)) = fetch_session(&mut transaction, request.session_id)
            .await
            .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        authorize_scoped(
            session.owner_user_id,
            &session.provider_id,
            &self.provider_id,
            request.access,
        )?;
        if session.owner_user_id != request.owner_user_id
            || session.provider_account_id != request.provider_account_id
            || session.task_id != request.task_id
            || !binding_is_valid(&mut transaction, &session)
                .await
                .map_err(storage_error)?
        {
            return Err(SecretStoreError::Unauthorized);
        }
        let Some(exchange) = fetch_exchange(&mut transaction, request.session_id, sequence)
            .await
            .map_err(storage_error)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        let Some((metadata, secret_id)) =
            fetch_result_artifact_binding(&mut transaction, request.session_id, sequence).await?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(None);
        };
        let stored = fetch_secret(&mut transaction, secret_id).await?;
        if stored.owner_user_id != session.owner_user_id
            || stored.purpose != SecretPurpose::BrowserJobCredential
        {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        let secret = SecretRef {
            id: secret_id,
            owner_user_id: stored.owner_user_id,
            purpose: stored.purpose,
            version: stored.version,
            key_id: stored.key_id.clone(),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
        };
        let key = self.keyring.get(&stored.key_id)?;
        let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
        if digest(&plaintext) != metadata.result_digest {
            return Err(SecretStoreError::AuthenticationFailed);
        }
        insert_secret_audit(
            &mut transaction,
            request.access,
            "browser_bridge_result_resolved",
            &secret,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(Some(ResolvedBrowserBridgeResult {
            exchange,
            metadata,
            result_artifact: SecretValue::new(plaintext),
        }))
    }
}

async fn fetch_result_metadata(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: asterism_domain::BrowserBridgeSessionId,
    sequence: i64,
) -> Result<Option<BrowserBridgeResultArtifactMetadata>, SecretStoreError> {
    sqlx::query(
        "SELECT result_type, result_digest, received_at \
         FROM browser_bridge_result_artifacts WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .map(|row| decode_result_metadata(&row, session_id, sequence))
    .transpose()
}

async fn fetch_result_artifact_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: asterism_domain::BrowserBridgeSessionId,
    sequence: i64,
) -> Result<Option<(BrowserBridgeResultArtifactMetadata, SecretId)>, SecretStoreError> {
    sqlx::query(
        "SELECT result_type, result_digest, secret_blob_id, received_at \
         FROM browser_bridge_result_artifacts WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .map(|row| {
        let metadata = decode_result_metadata(&row, session_id, sequence)?;
        let secret_id = SecretId::from_str(
            row.try_get::<&str, _>("secret_blob_id")
                .map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?;
        Ok((metadata, secret_id))
    })
    .transpose()
}

fn decode_result_metadata(
    row: &sqlx::sqlite::SqliteRow,
    session_id: asterism_domain::BrowserBridgeSessionId,
    sequence: i64,
) -> Result<BrowserBridgeResultArtifactMetadata, SecretStoreError> {
    let result_digest: [u8; 32] = row
        .try_get::<Vec<u8>, _>("result_digest")
        .map_err(storage_error)?
        .try_into()
        .map_err(|_| SecretStoreError::Storage)?;
    let received_at = decode_timestamp(
        row.try_get::<&str, _>("received_at")
            .map_err(storage_error)?,
    )?;
    let metadata = BrowserBridgeResultArtifactMetadata {
        session_id,
        sequence: u64::try_from(sequence).map_err(|_| SecretStoreError::Storage)?,
        result_type: row.try_get("result_type").map_err(storage_error)?,
        result_digest,
        received_at,
    };
    metadata.validate().map_err(|_| SecretStoreError::Storage)?;
    Ok(metadata)
}

async fn insert_dispatch_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    exchange: &asterism_domain::BrowserBridgeExchange,
    dispatched_at: asterism_domain::Timestamp,
) -> Result<(), SecretStoreError> {
    let (actor_type, actor_id) = match &access.actor {
        SecretActor::User(id) => ("user", id.to_string()),
        SecretActor::ServiceToken(id) => ("service_token", id.to_string()),
        SecretActor::CoreService(service) => ("core_service", (*service).to_owned()),
        SecretActor::ProviderRuntime(provider_id) => ("provider_runtime", provider_id.clone()),
    };
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'browser_bridge_command_dispatched', \
          'browser_bridge_exchange', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(dispatched_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(format!("{}:{}", exchange.session_id, exchange.sequence))
    .bind(&access.correlation_id)
    .bind(
        serde_json::json!({
            "sequence": exchange.sequence,
            "command_type": exchange.command_type,
            "command_digest": "[HASHED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn insert_result_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    metadata: &BrowserBridgeResultArtifactMetadata,
    action: &str,
) -> Result<(), SecretStoreError> {
    let (actor_type, actor_id) = match &access.actor {
        SecretActor::User(id) => ("user", id.to_string()),
        SecretActor::ServiceToken(id) => ("service_token", id.to_string()),
        SecretActor::CoreService(service) => ("core_service", (*service).to_owned()),
        SecretActor::ProviderRuntime(provider_id) => ("provider_runtime", provider_id.clone()),
    };
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'browser_bridge_result', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(metadata.received_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(format!("browser_bridge_result_{action}"))
    .bind(format!("{}:{}", metadata.session_id, metadata.sequence))
    .bind(&access.correlation_id)
    .bind(
        serde_json::json!({
            "sequence": metadata.sequence,
            "result_type": metadata.result_type,
            "result_digest": "[HASHED]",
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn fetch_runtime_state_metadata(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: asterism_domain::BrowserBridgeSessionId,
    sequence: i64,
) -> Result<Option<BrowserBridgeRuntimeStateMetadata>, SecretStoreError> {
    sqlx::query(
        "SELECT state_type, state_digest, stored_at \
         FROM browser_bridge_runtime_state_artifacts \
         WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .as_ref()
    .map(|row| decode_runtime_state_metadata(row, session_id, sequence))
    .transpose()
}

async fn insert_runtime_state(
    transaction: &mut Transaction<'_, Sqlite>,
    metadata: &BrowserBridgeRuntimeStateMetadata,
    secret_id: SecretId,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO browser_bridge_runtime_state_artifacts \
         (session_id, sequence, state_type, state_digest, secret_blob_id, stored_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(metadata.session_id.to_string())
    .bind(i64::try_from(metadata.sequence).map_err(|_| SecretStoreError::InvalidValue)?)
    .bind(&metadata.state_type)
    .bind(metadata.state_digest.as_slice())
    .bind(secret_id.to_string())
    .bind(encode_timestamp(metadata.stored_at))
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn resolve_runtime_state(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: asterism_domain::BrowserBridgeSessionId,
    sequence: i64,
    owner_user_id: asterism_domain::UserId,
    keyring: &SecretKeyring,
    access: &SecretAccess,
) -> Result<Option<ResolvedBrowserBridgeRuntimeState>, SecretStoreError> {
    let Some(row) = sqlx::query(
        "SELECT state_type, state_digest, secret_blob_id, stored_at \
         FROM browser_bridge_runtime_state_artifacts \
         WHERE session_id = ? AND sequence = ?",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    else {
        return Ok(None);
    };
    let metadata = decode_runtime_state_metadata(&row, session_id, sequence)?;
    let secret_id = SecretId::from_str(&row.get::<String, _>("secret_blob_id"))
        .map_err(|_| SecretStoreError::Storage)?;
    let stored = fetch_secret(transaction, secret_id).await?;
    if stored.owner_user_id != owner_user_id
        || stored.purpose != SecretPurpose::BrowserJobCredential
        || stored.created_at != metadata.stored_at
        || stored.updated_at != metadata.stored_at
    {
        return Err(SecretStoreError::AuthenticationFailed);
    }
    let secret = SecretRef {
        id: secret_id,
        owner_user_id: stored.owner_user_id,
        purpose: stored.purpose,
        version: stored.version,
        key_id: stored.key_id.clone(),
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    };
    let key = keyring.get(&stored.key_id)?;
    let plaintext = decrypt(key, &secret, &stored.nonce, &stored.encrypted_data)?;
    if digest(&plaintext) != metadata.state_digest {
        return Err(SecretStoreError::AuthenticationFailed);
    }
    insert_secret_audit(
        transaction,
        access,
        "browser_bridge_runtime_state_resolved",
        &secret,
    )
    .await
    .map_err(storage_error)?;
    Ok(Some(ResolvedBrowserBridgeRuntimeState {
        metadata,
        state_artifact: SecretValue::new(plaintext),
    }))
}

fn decode_runtime_state_metadata(
    row: &sqlx::sqlite::SqliteRow,
    session_id: asterism_domain::BrowserBridgeSessionId,
    sequence: i64,
) -> Result<BrowserBridgeRuntimeStateMetadata, SecretStoreError> {
    let state_digest: [u8; 32] = row
        .get::<Vec<u8>, _>("state_digest")
        .try_into()
        .map_err(|_| SecretStoreError::Storage)?;
    let metadata = BrowserBridgeRuntimeStateMetadata {
        session_id,
        sequence: u64::try_from(sequence).map_err(|_| SecretStoreError::Storage)?,
        state_type: row.get("state_type"),
        state_digest,
        stored_at: decode_timestamp(&row.get::<String, _>("stored_at"))?,
    };
    metadata
        .validate()
        .map_err(|_| SecretStoreError::AuthenticationFailed)?;
    Ok(metadata)
}

fn authorize_scoped(
    owner_user_id: asterism_domain::UserId,
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

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn encode_timestamp(value: asterism_domain::Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<asterism_domain::Timestamp, SecretStoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| SecretStoreError::Storage)
}

fn storage_error<E>(_error: E) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_auth::OpaqueTokenService;
    use asterism_domain::{
        AuditActor, AuthMethod, AuthState, BrowserBridgeExchange, BrowserBridgeRuntimeBinding,
        BrowserBridgeSession, BrowserBridgeSessionCreate, BrowserBridgeSessionState,
        ProviderAccountId, Role, SessionKind, TaskId, UserId,
    };
    use asterism_provider_api::BrowserSessionSpec;
    use asterism_secrets::{
        CredentialAcquisition, CredentialBundle, CredentialField, SecretKey, SecretPurpose,
        SecretStore, SecretStoreError,
    };
    use chrono::{Duration, Utc};

    use super::*;
    use crate::{
        BrowserBridgeCredentialCommitOutcome, BrowserBridgeCredentialCommitRequest,
        BrowserBridgeCredentialRepository, BrowserBridgeResultAttemptFinishRequest,
        BrowserBridgeSessionRepository, PendingBrowserBridgeResult,
        SqliteBrowserBridgeSessionRepository, SqliteSecretStore,
    };

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn command_is_encrypted_recoverable_bound_and_legacy_safe() {
        let fixture = fixture().await;
        let now = Utc::now();
        let (session, access_digest) = fixture.claimed_session(now).await;
        let command = br#"{"kind":"capture_snapshot","nonce":"n-1"}"#;
        let issued = BrowserBridgeExchange::issue(
            session.id,
            1,
            "cidaren.capture.snapshot".to_owned(),
            digest(command),
            now + Duration::seconds(2),
        )
        .unwrap();
        let access = Fixture::access();
        sqlx::query("DELETE FROM browser_bridge_runtime_bindings WHERE session_id = ?")
            .bind(session.id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(
            fixture
                .command_repository
                .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                    exchange: &issued,
                    command_artifact: SecretValue::new(command.to_vec()),
                    runtime_state: None,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeExchangeRecord::SequenceConflict
        );
        assert!(matches!(
            fixture
                .session_repository
                .bind_browser_bridge_runtime(
                    &BrowserBridgeRuntimeBinding {
                        session_id: session.id,
                        observed_origin: "https://www.cidaren.com".to_owned(),
                        frame_id: "top-frame:1".to_owned(),
                        bound_at: now + Duration::seconds(2),
                    },
                    &access_digest,
                    "command-session-rebind",
                )
                .await
                .unwrap(),
            crate::BrowserBridgeRuntimeBindingRecord::Bound(_)
        ));
        let runtime_state = br#"{"cursor":"opaque-state-1"}"#;
        let runtime_metadata = BrowserBridgeRuntimeStateMetadata {
            session_id: session.id,
            sequence: 1,
            state_type: "uai.browser.cursor.v1".to_owned(),
            state_digest: digest(runtime_state),
            stored_at: issued.issued_at,
        };
        let inserted = fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                runtime_state: Some(crate::BrowserBridgeRuntimeStateIssue {
                    metadata: runtime_metadata.clone(),
                    state_artifact: SecretValue::new(runtime_state.to_vec()),
                }),
                access: &access,
            })
            .await
            .unwrap();
        assert!(matches!(inserted, BrowserBridgeExchangeRecord::Inserted(_)));
        assert_eq!(
            fixture
                .session_repository
                .find_latest_browser_bridge_exchange(fixture.owner, session.id)
                .await
                .unwrap(),
            Some(issued.clone())
        );
        assert!(
            fixture
                .session_repository
                .find_latest_browser_bridge_exchange(UserId::new(), session.id)
                .await
                .unwrap()
                .is_none()
        );
        let duplicate = fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                runtime_state: Some(crate::BrowserBridgeRuntimeStateIssue {
                    metadata: runtime_metadata.clone(),
                    state_artifact: SecretValue::new(runtime_state.to_vec()),
                }),
                access: &access,
            })
            .await
            .unwrap();
        assert!(matches!(
            duplicate,
            BrowserBridgeExchangeRecord::Duplicate(_)
        ));
        assert_eq!(
            fixture
                .command_repository
                .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                    exchange: &issued,
                    command_artifact: SecretValue::new(command.to_vec()),
                    runtime_state: None,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeExchangeRecord::SequenceConflict
        );

        let encrypted: Vec<u8> = sqlx::query_scalar(
            "SELECT secret.encrypted_data FROM browser_bridge_exchanges AS exchange \
             JOIN secret_blobs AS secret ON secret.id = exchange.command_secret_blob_id \
             WHERE exchange.session_id = ? AND exchange.sequence = 1",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_ne!(encrypted, command);
        assert!(
            !encrypted
                .windows(command.len())
                .any(|window| window == command)
        );

        let encrypted_runtime_state: Vec<u8> = sqlx::query_scalar(
            "SELECT secret.encrypted_data FROM browser_bridge_runtime_state_artifacts AS state \
             JOIN secret_blobs AS secret ON secret.id = state.secret_blob_id \
             WHERE state.session_id = ? AND state.sequence = 1",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_ne!(encrypted_runtime_state, runtime_state);

        let resolved = fixture
            .command_repository
            .resolve_browser_bridge_command(fixture.resolve_request(&session, 1, &access))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.exchange, issued);
        assert_eq!(resolved.command_artifact.expose_secret(), command);
        let resolved_runtime_state = resolved.runtime_state.unwrap();
        assert_eq!(resolved_runtime_state.metadata, runtime_metadata);
        assert_eq!(
            resolved_runtime_state.state_artifact.expose_secret(),
            runtime_state
        );
        sqlx::query(
            "UPDATE browser_bridge_runtime_state_artifacts SET stored_at = ? \
             WHERE session_id = ? AND sequence = 1",
        )
        .bind(encode_timestamp(issued.issued_at + Duration::seconds(1)))
        .bind(session.id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .command_repository
                .resolve_browser_bridge_command(fixture.resolve_request(&session, 1, &access))
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
        sqlx::query(
            "UPDATE browser_bridge_runtime_state_artifacts SET stored_at = ? \
             WHERE session_id = ? AND sequence = 1",
        )
        .bind(encode_timestamp(issued.issued_at))
        .bind(session.id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();

        let raw_result = br#"{"kind":"menu_list","entries":[]}"#;
        let result_metadata = BrowserBridgeResultArtifactMetadata {
            session_id: session.id,
            sequence: 1,
            result_type: "uai.browser.event".to_owned(),
            result_digest: digest(raw_result),
            received_at: now + Duration::seconds(3),
        };
        assert!(matches!(
            fixture
                .command_repository
                .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                    metadata: &result_metadata,
                    result_artifact: SecretValue::new(raw_result.to_vec()),
                    access_token_digest: &access_digest,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeResultArtifactRecord::SequenceConflict
        ));
        let (_, foreign_access_digest) = OpaqueTokenService::new("ast_bridge").unwrap().generate();
        assert!(matches!(
            fixture
                .dispatch_command(
                    &session,
                    1,
                    &foreign_access_digest,
                    &access,
                    now + Duration::milliseconds(2_400),
                )
                .await,
            BrowserBridgeCommandDispatchRecord::AccessRejected
        ));
        sqlx::query("DELETE FROM browser_bridge_runtime_bindings WHERE session_id = ?")
            .bind(session.id.to_string())
            .execute(fixture.database.pool())
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .dispatch_command(
                    &session,
                    1,
                    &access_digest,
                    &access,
                    now + Duration::milliseconds(2_400),
                )
                .await,
            BrowserBridgeCommandDispatchRecord::SequenceConflict
        ));
        assert!(matches!(
            fixture
                .session_repository
                .bind_browser_bridge_runtime(
                    &BrowserBridgeRuntimeBinding {
                        session_id: session.id,
                        observed_origin: "https://www.cidaren.com".to_owned(),
                        frame_id: "top-frame:1".to_owned(),
                        bound_at: now + Duration::milliseconds(2_400),
                    },
                    &access_digest,
                    "command-session-dispatch-rebind",
                )
                .await
                .unwrap(),
            crate::BrowserBridgeRuntimeBindingRecord::Bound(_)
        ));
        assert!(matches!(
            fixture
                .dispatch_command(
                    &session,
                    2,
                    &access_digest,
                    &access,
                    now + Duration::milliseconds(2_400),
                )
                .await,
            BrowserBridgeCommandDispatchRecord::NotFound
        ));

        let dispatched = fixture
            .dispatch_command(
                &session,
                1,
                &access_digest,
                &access,
                now + Duration::milliseconds(2_500),
            )
            .await;
        let BrowserBridgeCommandDispatchRecord::Dispatched(dispatched) = dispatched else {
            panic!("expected first command dispatch");
        };
        assert_eq!(dispatched.exchange, issued);
        assert_eq!(dispatched.command_artifact.expose_secret(), command);
        let early_result_metadata = BrowserBridgeResultArtifactMetadata {
            received_at: now + Duration::milliseconds(2_400),
            ..result_metadata.clone()
        };
        assert!(matches!(
            fixture
                .command_repository
                .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                    metadata: &early_result_metadata,
                    result_artifact: SecretValue::new(raw_result.to_vec()),
                    access_token_digest: &access_digest,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeResultArtifactRecord::SequenceConflict
        ));
        assert!(matches!(
            fixture
                .dispatch_command(
                    &session,
                    1,
                    &access_digest,
                    &access,
                    now + Duration::milliseconds(2_600),
                )
                .await,
            BrowserBridgeCommandDispatchRecord::AlreadyDispatched
        ));

        assert!(matches!(
            fixture
                .command_repository
                .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                    metadata: &result_metadata,
                    result_artifact: SecretValue::new(raw_result.to_vec()),
                    access_token_digest: &access_digest,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeResultArtifactRecord::Inserted(_)
        ));
        assert!(matches!(
            fixture
                .command_repository
                .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                    metadata: &result_metadata,
                    result_artifact: SecretValue::new(raw_result.to_vec()),
                    access_token_digest: &access_digest,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeResultArtifactRecord::Duplicate(_)
        ));
        let conflicting_result = br#"{"kind":"menu_list","entries":["foreign"]}"#;
        let conflicting_metadata = BrowserBridgeResultArtifactMetadata {
            result_digest: digest(conflicting_result),
            ..result_metadata.clone()
        };
        assert!(matches!(
            fixture
                .command_repository
                .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                    metadata: &conflicting_metadata,
                    result_artifact: SecretValue::new(conflicting_result.to_vec()),
                    access_token_digest: &access_digest,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeResultArtifactRecord::SequenceConflict
        ));
        let encrypted_result: Vec<u8> = sqlx::query_scalar(
            "SELECT secret.encrypted_data FROM browser_bridge_result_artifacts AS result \
             JOIN secret_blobs AS secret ON secret.id = result.secret_blob_id \
             WHERE result.session_id = ? AND result.sequence = 1",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_ne!(encrypted_result, raw_result);
        assert!(
            !encrypted_result
                .windows(raw_result.len())
                .any(|window| window == raw_result)
        );
        let resolved_result = fixture
            .command_repository
            .resolve_browser_bridge_result(BrowserBridgeResultResolveRequest {
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                task_id: fixture.task,
                session_id: session.id,
                sequence: 1,
                access: &access,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved_result.exchange, issued);
        assert_eq!(resolved_result.metadata, result_metadata);
        assert_eq!(resolved_result.result_artifact.expose_secret(), raw_result);
        let mut accepted = issued.clone();
        accepted
            .complete(
                result_metadata.result_type.clone(),
                result_metadata.result_digest,
                result_metadata.received_at,
            )
            .unwrap();
        assert!(matches!(
            fixture
                .session_repository
                .complete_browser_bridge_exchange(&accepted, &access_digest, "result-accepted",)
                .await
                .unwrap(),
            BrowserBridgeExchangeRecord::Inserted(_)
        ));
        assert!(matches!(
            fixture
                .command_repository
                .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                    metadata: &result_metadata,
                    result_artifact: SecretValue::new(raw_result.to_vec()),
                    access_token_digest: &access_digest,
                    access: &access,
                })
                .await
                .unwrap(),
            BrowserBridgeResultArtifactRecord::Duplicate(_)
        ));
        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = X'00' WHERE id = ( \
             SELECT secret_blob_id FROM browser_bridge_result_artifacts \
             WHERE session_id = ? AND sequence = 1)",
        )
        .bind(session.id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .command_repository
                .resolve_browser_bridge_result(BrowserBridgeResultResolveRequest {
                    owner_user_id: fixture.owner,
                    provider_account_id: fixture.account,
                    task_id: fixture.task,
                    session_id: session.id,
                    sequence: 1,
                    access: &access,
                })
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));

        let wrong_task = BrowserBridgeCommandResolveRequest {
            task_id: TaskId::new(),
            ..fixture.resolve_request(&session, 1, &access)
        };
        assert!(matches!(
            fixture
                .command_repository
                .resolve_browser_bridge_command(wrong_task)
                .await,
            Err(SecretStoreError::Unauthorized)
        ));

        let legacy = BrowserBridgeExchange::issue(
            session.id,
            2,
            "cidaren.capture.legacy".to_owned(),
            [9; 32],
            now + Duration::seconds(3),
        )
        .unwrap();
        sqlx::query(
            "INSERT INTO browser_bridge_exchanges \
             (session_id, sequence, command_type, command_digest, state, issued_at) \
             VALUES (?, 2, ?, ?, 'issued', ?)",
        )
        .bind(session.id.to_string())
        .bind(&legacy.command_type)
        .bind(legacy.command_digest.as_slice())
        .bind(encode_timestamp(legacy.issued_at))
        .execute(fixture.database.pool())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .command_repository
                .resolve_browser_bridge_command(fixture.resolve_request(&session, 2, &access))
                .await,
            Err(SecretStoreError::VersionConflict)
        ));
        sqlx::query(
            "UPDATE secret_blobs SET encrypted_data = X'00' WHERE id = ( \
             SELECT secret_blob_id FROM browser_bridge_runtime_state_artifacts \
             WHERE session_id = ? AND sequence = 1)",
        )
        .bind(session.id.to_string())
        .execute(fixture.database.pool())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .command_repository
                .resolve_browser_bridge_command(fixture.resolve_request(&session, 1, &access))
                .await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression proves result dead letter, session failure, token revocation and audit in one transaction"
    )]
    async fn dead_letter_fails_session_and_revokes_helper_access_atomically() {
        let fixture = fixture().await;
        let now = Utc::now() - Duration::seconds(10);
        let (session, access_digest) = fixture.claimed_session(now).await;
        let command = br#"{"kind":"capture_snapshot","nonce":"dead-letter"}"#;
        let issued = BrowserBridgeExchange::issue(
            session.id,
            1,
            "cidaren.capture.snapshot".to_owned(),
            digest(command),
            now + Duration::seconds(2),
        )
        .unwrap();
        let access = Fixture::access();
        fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                runtime_state: None,
                access: &access,
            })
            .await
            .unwrap();
        fixture
            .dispatch_command(
                &session,
                1,
                &access_digest,
                &access,
                now + Duration::milliseconds(2_500),
            )
            .await;
        let received_at = now + Duration::seconds(3);
        let raw_result = br#"{"kind":"capture_snapshot","token":"invalid"}"#;
        fixture
            .command_repository
            .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                metadata: &BrowserBridgeResultArtifactMetadata {
                    session_id: session.id,
                    sequence: 1,
                    result_type: "cidaren.capture.snapshot.result".to_owned(),
                    result_digest: digest(raw_result),
                    received_at,
                },
                result_artifact: SecretValue::new(raw_result.to_vec()),
                access_token_digest: &access_digest,
                access: &access,
            })
            .await
            .unwrap();
        let claimed = fixture
            .session_repository
            .claim_pending_browser_bridge_results(
                received_at,
                &fixture.provider,
                &["cidaren.capture.snapshot.result"],
                1,
                "dead-letter-worker",
                received_at + Duration::seconds(30),
            )
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let failed_at = received_at + Duration::seconds(1);
        assert!(
            fixture
                .session_repository
                .finish_browser_bridge_result_attempt(BrowserBridgeResultAttemptFinishRequest {
                    session_id: session.id,
                    sequence: 1,
                    worker_id: "dead-letter-worker",
                    failed_at,
                    retry_at: None,
                    error_kind: "provider_validation",
                })
                .await
                .unwrap()
        );

        let persisted: (String, Option<Vec<u8>>, i64, String, Option<String>) = sqlx::query_as(
            "SELECT session.state_json, session.access_token_hash, session.revision, \
                    result.processing_state, result.last_error_kind \
             FROM browser_bridge_sessions AS session \
             JOIN browser_bridge_result_artifacts AS result ON result.session_id = session.id \
             WHERE session.id = ? AND result.sequence = 1",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<BrowserBridgeSessionState>(&persisted.0).unwrap(),
            BrowserBridgeSessionState::Failed
        );
        assert_eq!(persisted.1, None);
        assert_eq!(persisted.2, 3);
        assert_eq!(persisted.3, "dead_letter");
        assert_eq!(persisted.4.as_deref(), Some("provider_validation"));
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records \
             WHERE resource_id = ? AND action = 'browser_bridge_session_failed' \
               AND actor_type = 'browser_bridge_worker' AND actor_id = 'dead-letter-worker'",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 1);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn terminal_result_credentials_and_session_commit_atomically() {
        let fixture = fixture().await;
        let now = Utc::now() - Duration::seconds(10);
        let (session, access_digest) = fixture.claimed_session(now).await;
        let command = br#"{"kind":"capture_snapshot","nonce":"credential-1"}"#;
        let issued = BrowserBridgeExchange::issue(
            session.id,
            1,
            "cidaren.capture.snapshot".to_owned(),
            digest(command),
            now + Duration::seconds(2),
        )
        .unwrap();
        let access = Fixture::access();
        fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                runtime_state: None,
                access: &access,
            })
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .dispatch_command(
                    &session,
                    1,
                    &access_digest,
                    &access,
                    now + Duration::milliseconds(2_500),
                )
                .await,
            BrowserBridgeCommandDispatchRecord::Dispatched(_)
        ));
        let completed_at = now + Duration::seconds(3);
        let raw_result = br#"{"kind":"capture_snapshot","token":"captured-token"}"#;
        let result_digest = digest(raw_result);
        fixture
            .command_repository
            .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                metadata: &BrowserBridgeResultArtifactMetadata {
                    session_id: session.id,
                    sequence: 1,
                    result_type: "cidaren.capture.snapshot.result".to_owned(),
                    result_digest,
                    received_at: completed_at,
                },
                result_artifact: SecretValue::new(raw_result.to_vec()),
                access_token_digest: &access_digest,
                access: &access,
            })
            .await
            .unwrap();
        assert!(
            fixture
                .session_repository
                .list_pending_browser_bridge_results(
                    completed_at,
                    &ProviderId::new("other-provider").unwrap(),
                    &["cidaren.capture.snapshot.result"],
                    10,
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .session_repository
                .list_pending_browser_bridge_results(
                    completed_at,
                    &fixture.provider,
                    &["cidaren.capture.unknown"],
                    10,
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .session_repository
                .list_pending_browser_bridge_results(
                    completed_at,
                    &fixture.provider,
                    &["cidaren.capture.snapshot.result"],
                    10,
                )
                .await
                .unwrap(),
            [PendingBrowserBridgeResult {
                owner_user_id: fixture.owner,
                session_id: session.id,
                sequence: 1,
                provider_id: fixture.provider.clone(),
                result_type: "cidaren.capture.snapshot.result".to_owned(),
                attempt_no: 1,
            }]
        );
        let first_claim = fixture
            .session_repository
            .claim_pending_browser_bridge_results(
                completed_at,
                &fixture.provider,
                &["cidaren.capture.snapshot.result"],
                10,
                "worker-a",
                completed_at + Duration::seconds(30),
            )
            .await
            .unwrap();
        assert_eq!(first_claim[0].attempt_no, 1);
        assert!(
            fixture
                .session_repository
                .claim_pending_browser_bridge_results(
                    completed_at,
                    &fixture.provider,
                    &["cidaren.capture.snapshot.result"],
                    10,
                    "worker-b",
                    completed_at + Duration::seconds(30),
                )
                .await
                .unwrap()
                .is_empty()
        );
        let recovered_at = completed_at + Duration::seconds(30);
        let recovered_claim = fixture
            .session_repository
            .claim_pending_browser_bridge_results(
                recovered_at,
                &fixture.provider,
                &["cidaren.capture.snapshot.result"],
                10,
                "worker-b",
                recovered_at + Duration::seconds(30),
            )
            .await
            .unwrap();
        assert_eq!(recovered_claim[0].attempt_no, 2);
        let retry_at = recovered_at + Duration::seconds(5);
        assert!(
            fixture
                .session_repository
                .finish_browser_bridge_result_attempt(BrowserBridgeResultAttemptFinishRequest {
                    session_id: session.id,
                    sequence: 1,
                    worker_id: "worker-b",
                    failed_at: recovered_at,
                    retry_at: Some(retry_at),
                    error_kind: "provider_validation",
                },)
                .await
                .unwrap()
        );
        assert!(
            fixture
                .session_repository
                .list_pending_browser_bridge_results(
                    retry_at - Duration::milliseconds(1),
                    &fixture.provider,
                    &["cidaren.capture.snapshot.result"],
                    10,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let retry_claim = fixture
            .session_repository
            .claim_pending_browser_bridge_results(
                retry_at,
                &fixture.provider,
                &["cidaren.capture.snapshot.result"],
                10,
                "worker-c",
                retry_at + Duration::seconds(30),
            )
            .await
            .unwrap();
        assert_eq!(retry_claim[0].attempt_no, 3);
        let final_retry_at = retry_at + Duration::seconds(5);
        assert!(
            fixture
                .session_repository
                .finish_browser_bridge_result_attempt(BrowserBridgeResultAttemptFinishRequest {
                    session_id: session.id,
                    sequence: 1,
                    worker_id: "worker-c",
                    failed_at: retry_at,
                    retry_at: Some(final_retry_at),
                    error_kind: "commit_storage",
                },)
                .await
                .unwrap()
        );
        assert!(
            fixture
                .session_repository
                .list_pending_browser_bridge_results(
                    final_retry_at - Duration::milliseconds(1),
                    &fixture.provider,
                    &["cidaren.capture.snapshot.result"],
                    10,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let processing: (String, i64, Option<String>, Option<String>, Option<String>) =
            sqlx::query_as(
                "SELECT processing_state, attempt_count, claimed_by, next_attempt_at, \
                        last_error_kind \
                 FROM browser_bridge_result_artifacts WHERE session_id = ? AND sequence = 1",
            )
            .bind(session.id.to_string())
            .fetch_one(fixture.database.pool())
            .await
            .unwrap();
        assert_eq!(processing.0, "retry");
        assert_eq!(processing.1, 3);
        assert_eq!(processing.2, None);
        assert!(processing.3.is_some());
        assert_eq!(processing.4.as_deref(), Some("commit_storage"));
        let mut completed = issued.clone();
        completed
            .complete(
                "cidaren.capture.snapshot.result".to_owned(),
                result_digest,
                completed_at,
            )
            .unwrap();
        let outcome = fixture
            .secret_store
            .commit_browser_bridge_credentials(BrowserBridgeCredentialCommitRequest {
                exchange: &completed,
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                task_id: fixture.task,
                validated_bundle: fixture.bundle(completed_at, b"captured-token"),
                access: &access,
            })
            .await
            .unwrap();
        let BrowserBridgeCredentialCommitOutcome::Committed(committed) = outcome else {
            panic!("expected atomic BrowserBridge credential commit");
        };
        assert_eq!(
            committed.session.state,
            BrowserBridgeSessionState::Completed
        );
        assert_eq!(committed.exchange, completed);
        assert_eq!(committed.credentials.len(), 1);
        assert_eq!(
            fixture
                .secret_store
                .get(&committed.credentials[0].secret, &access)
                .await
                .unwrap()
                .expose_secret(),
            b"captured-token"
        );
        let persisted: (String, Option<Vec<u8>>, String) = sqlx::query_as(
            "SELECT state_json, access_token_hash, auth_state_json \
             FROM browser_bridge_sessions \
             JOIN provider_accounts ON provider_accounts.id = browser_bridge_sessions.provider_account_id \
             WHERE browser_bridge_sessions.id = ?",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<BrowserBridgeSessionState>(&persisted.0).unwrap(),
            BrowserBridgeSessionState::Completed
        );
        assert!(persisted.1.is_none());
        assert_eq!(
            serde_json::from_str::<AuthState>(&persisted.2).unwrap(),
            AuthState::Authenticated
        );
        assert!(
            fixture
                .session_repository
                .list_pending_browser_bridge_results(
                    completed_at,
                    &fixture.provider,
                    &["cidaren.capture.snapshot.result"],
                    10,
                )
                .await
                .unwrap()
                .is_empty()
        );

        let retry = fixture
            .secret_store
            .commit_browser_bridge_credentials(BrowserBridgeCredentialCommitRequest {
                exchange: &completed,
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                task_id: fixture.task,
                validated_bundle: fixture.bundle(completed_at, b"captured-token"),
                access: &access,
            })
            .await
            .unwrap();
        assert_eq!(retry, BrowserBridgeCredentialCommitOutcome::BindingConflict);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn terminal_binding_conflict_leaves_exchange_session_and_credentials_unchanged() {
        let fixture = fixture().await;
        let now = Utc::now() - Duration::seconds(10);
        let (session, access_digest) = fixture.claimed_session(now).await;
        let command = br#"{"kind":"capture_snapshot","nonce":"credential-rollback"}"#;
        let issued = BrowserBridgeExchange::issue(
            session.id,
            1,
            "cidaren.capture.snapshot".to_owned(),
            digest(command),
            now + Duration::seconds(2),
        )
        .unwrap();
        let access = Fixture::access();
        fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                runtime_state: None,
                access: &access,
            })
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .dispatch_command(
                    &session,
                    1,
                    &access_digest,
                    &access,
                    now + Duration::milliseconds(2_500),
                )
                .await,
            BrowserBridgeCommandDispatchRecord::Dispatched(_)
        ));
        let completed_at = now + Duration::seconds(3);
        let raw_result = br#"{"kind":"capture_snapshot","token":"must-not-persist"}"#;
        let result_digest = digest(raw_result);
        fixture
            .command_repository
            .receive_browser_bridge_result(BrowserBridgeResultReceiveRequest {
                metadata: &BrowserBridgeResultArtifactMetadata {
                    session_id: session.id,
                    sequence: 1,
                    result_type: "cidaren.capture.snapshot.result".to_owned(),
                    result_digest,
                    received_at: completed_at,
                },
                result_artifact: SecretValue::new(raw_result.to_vec()),
                access_token_digest: &access_digest,
                access: &access,
            })
            .await
            .unwrap();
        let mut foreign = issued.clone();
        foreign.command_digest = [7; 32];
        foreign
            .complete(
                "cidaren.capture.snapshot.result".to_owned(),
                result_digest,
                completed_at,
            )
            .unwrap();
        let outcome = fixture
            .secret_store
            .commit_browser_bridge_credentials(BrowserBridgeCredentialCommitRequest {
                exchange: &foreign,
                owner_user_id: fixture.owner,
                provider_account_id: fixture.account,
                task_id: fixture.task,
                validated_bundle: fixture.bundle(completed_at, b"must-not-persist"),
                access: &access,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome,
            BrowserBridgeCredentialCommitOutcome::SequenceConflict
        );
        let state: String = sqlx::query_scalar(
            "SELECT state FROM browser_bridge_exchanges WHERE session_id = ? AND sequence = 1",
        )
        .bind(session.id.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        let session_state: String =
            sqlx::query_scalar("SELECT state_json FROM browser_bridge_sessions WHERE id = ?")
                .bind(session.id.to_string())
                .fetch_one(fixture.database.pool())
                .await
                .unwrap();
        let credential_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_account_credentials WHERE provider_account_id = ?",
        )
        .bind(fixture.account.to_string())
        .fetch_one(fixture.database.pool())
        .await
        .unwrap();
        assert_eq!(state, "issued");
        assert_eq!(
            serde_json::from_str::<BrowserBridgeSessionState>(&session_state).unwrap(),
            BrowserBridgeSessionState::Claimed
        );
        assert_eq!(credential_count, 0);
    }

    struct Fixture {
        database: Database,
        session_repository: SqliteBrowserBridgeSessionRepository,
        command_repository: SqliteBrowserBridgeCommandArtifactRepository,
        secret_store: SqliteSecretStore,
        owner: UserId,
        account: ProviderAccountId,
        task: TaskId,
        provider: ProviderId,
    }

    impl Fixture {
        async fn claimed_session(
            &self,
            now: asterism_domain::Timestamp,
        ) -> (BrowserBridgeSession, asterism_auth::TokenDigest) {
            let spec = BrowserSessionSpec {
                version: 1,
                start_url: "https://www.cidaren.com/student/task".to_owned(),
                isolation_key: "cidaren-task".to_owned(),
                allowed_origins: vec!["https://www.cidaren.com".to_owned()],
                read_sources: Vec::new(),
                headless: false,
            };
            let session = BrowserBridgeSession::awaiting_claim(BrowserBridgeSessionCreate {
                owner_user_id: self.owner,
                provider_account_id: self.account,
                task_id: self.task,
                provider_id: self.provider.clone(),
                provider_version: "test".to_owned(),
                spec_version: spec.version,
                spec_digest: spec.digest().unwrap(),
                created_at: now,
                expires_at: now + Duration::hours(1),
            })
            .unwrap();
            let pairing_tokens = OpaqueTokenService::new("ast_bridge_pair").unwrap();
            let (pairing, pairing_digest) = pairing_tokens.generate();
            let access_tokens = OpaqueTokenService::new("ast_bridge").unwrap();
            let (_, access_digest) = access_tokens.generate();
            self.session_repository
                .create_browser_bridge_session(
                    &session,
                    &spec,
                    &pairing_digest,
                    AuditActor::User(self.owner),
                    "command-session-create",
                )
                .await
                .unwrap();
            let claimed = self
                .session_repository
                .claim_browser_bridge_session(
                    session.id,
                    &pairing_tokens.digest(&pairing),
                    &access_digest,
                    now + Duration::seconds(1),
                    "command-session-claim",
                )
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                self.session_repository
                    .bind_browser_bridge_runtime(
                        &BrowserBridgeRuntimeBinding {
                            session_id: session.id,
                            observed_origin: "https://www.cidaren.com".to_owned(),
                            frame_id: "top-frame:1".to_owned(),
                            bound_at: now + Duration::seconds(2),
                        },
                        &access_digest,
                        "command-session-bind",
                    )
                    .await
                    .unwrap(),
                crate::BrowserBridgeRuntimeBindingRecord::Bound(_)
            ));
            (claimed.0, access_digest)
        }

        fn access() -> SecretAccess {
            SecretAccess {
                actor: SecretActor::CoreService("browser-bridge-test"),
                correlation_id: "command-artifact-test".to_owned(),
                reason: "test exact command recovery".to_owned(),
            }
        }

        fn resolve_request<'a>(
            &self,
            session: &BrowserBridgeSession,
            sequence: u64,
            access: &'a SecretAccess,
        ) -> BrowserBridgeCommandResolveRequest<'a> {
            BrowserBridgeCommandResolveRequest {
                owner_user_id: self.owner,
                provider_account_id: self.account,
                task_id: self.task,
                session_id: session.id,
                sequence,
                access,
            }
        }

        async fn dispatch_command(
            &self,
            session: &BrowserBridgeSession,
            sequence: u64,
            access_token_digest: &asterism_auth::TokenDigest,
            access: &SecretAccess,
            dispatched_at: asterism_domain::Timestamp,
        ) -> BrowserBridgeCommandDispatchRecord {
            self.command_repository
                .dispatch_browser_bridge_command(BrowserBridgeCommandDispatchRequest {
                    session_id: session.id,
                    sequence,
                    access_token_digest,
                    dispatched_at,
                    access,
                })
                .await
                .unwrap()
        }

        fn bundle(
            &self,
            captured_at: asterism_domain::Timestamp,
            value: &[u8],
        ) -> CredentialBundle {
            CredentialBundle {
                provider_id: self.provider.clone(),
                tenant: None,
                auth_method: AuthMethod::AssistedSession,
                acquired_via: CredentialAcquisition::CaptureTool,
                captured_at,
                expires_at: None,
                session_kind: SessionKind::BearerToken,
                fields: vec![CredentialField {
                    purpose: SecretPurpose::ProviderAccessToken,
                    value: SecretValue::new(value.to_vec()),
                }],
                user_id_hint: None,
            }
        }
    }

    async fn fixture() -> Fixture {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let account = ProviderAccountId::new();
        let task = TaskId::new();
        let provider = ProviderId::new("cidaren").unwrap();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, 'bridge-command-owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(owner.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'primary', '\"authenticated\"', ?, ?)",
        )
        .bind(account.to_string())
        .bind(owner.to_string())
        .bind(provider.as_str())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, 'remote-task', 'fingerprint', 'work', 'unknown', 'Task', \
                     'pending', 'ready', ?, ?, '[\"browser_bridge\"]')",
        )
        .bind(task.to_string())
        .bind(account.to_string())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        let mut keys = BTreeMap::new();
        keys.insert("test-key".to_owned(), SecretKey::new([7; 32]));
        let keyring = Arc::new(SecretKeyring::new("test-key", keys).unwrap());
        let secret_store = SqliteSecretStore::new(database.clone(), keyring.clone());
        Fixture {
            database: database.clone(),
            session_repository: SqliteBrowserBridgeSessionRepository::new(database.clone()),
            command_repository: SqliteBrowserBridgeCommandArtifactRepository::new(
                database.clone(),
                keyring,
                provider.clone(),
            ),
            secret_store,
            owner,
            account,
            task,
            provider,
        }
    }
}
