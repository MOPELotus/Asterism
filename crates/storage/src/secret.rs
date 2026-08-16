use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    str::FromStr,
    sync::Arc,
};

use asterism_domain::{
    AuditActor, AuditRecordId, AuthBootstrapPurpose, AuthBootstrapSessionId, AuthMethod,
    AuthSession, AuthState, BrowserBridgeExchangeState, BrowserBridgeSession,
    BrowserBridgeSessionState, ProviderAccount, ProviderAccountId, ProviderId, SecretId,
    SessionKind, Timestamp, UserId,
};
use asterism_secrets::{
    CredentialAcquisition, CredentialBundle, NewProviderCredential, ProviderCredential,
    ProviderCredentialRenewal, ProviderCredentialRenewer, ProviderCredentialResolution,
    ProviderCredentialResolver, ProviderCredentialStore, ResolvedProviderCredential, SecretAccess,
    SecretActor, SecretKey, SecretPurpose, SecretRef, SecretStore, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    AuthBootstrapCredentialCommit, AuthBootstrapCredentialCommitOutcome,
    AuthBootstrapCredentialCommitRequest, AuthBootstrapCredentialRepository,
    AuthenticatedCredentialRepository, BrowserBridgeCredentialCommit,
    BrowserBridgeCredentialCommitOutcome, BrowserBridgeCredentialCommitRequest,
    BrowserBridgeCredentialRepository, Database, InteractiveAuthCredentialCommit,
    InteractiveAuthCredentialCommitOutcome, InteractiveAuthCredentialCommitRequest,
    InteractiveAuthCredentialRepository,
    answer_harvest::ensure_initial_answer_bootstrap_harvest,
    auth_bootstrap::{authenticate_access_in_transaction, complete_auth_bootstrap_in_transaction},
    auth_session::{fetch_auth_session, update_auth_session_in_transaction},
    browser_bridge::{fetch_exchange, find_claimed_session_for_exchange, insert_exchange_audit},
};
use crate::{QuestionReadContinuationRepositoryFactory, QuestionSessionArtifactRepositoryFactory};

const MAX_SECRET_BYTES: usize = 1024 * 1024;

pub struct SecretKeyring {
    active_key_id: String,
    keys: BTreeMap<String, SecretKey>,
}

impl SecretKeyring {
    /// Builds a keyring whose active key encrypts new versions while retained
    /// keys remain available for decryption.
    ///
    /// # Errors
    ///
    /// Returns [`SecretKeyringError`] when a key ID is unsafe for persistence
    /// or the selected active key is absent.
    pub fn new(
        active_key_id: impl Into<String>,
        keys: BTreeMap<String, SecretKey>,
    ) -> Result<Self, SecretKeyringError> {
        let active_key_id = active_key_id.into();
        if !valid_key_id(&active_key_id) || keys.keys().any(|key_id| !valid_key_id(key_id)) {
            return Err(SecretKeyringError::InvalidKeyId);
        }
        if !keys.contains_key(&active_key_id) {
            return Err(SecretKeyringError::ActiveKeyMissing);
        }
        Ok(Self {
            active_key_id,
            keys,
        })
    }

    pub(crate) fn active(&self) -> (&str, &SecretKey) {
        (
            &self.active_key_id,
            self.keys
                .get(&self.active_key_id)
                .expect("active key was validated at construction"),
        )
    }

    pub(crate) fn get(&self, key_id: &str) -> Result<&SecretKey, SecretStoreError> {
        self.keys
            .get(key_id)
            .ok_or(SecretStoreError::KeyUnavailable)
    }
}

impl fmt::Debug for SecretKeyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretKeyring")
            .field("active_key_id", &self.active_key_id)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SecretKeyringError {
    #[error("secret key ID must contain 1-64 safe ASCII characters")]
    InvalidKeyId,
    #[error("active secret key is missing from the keyring")]
    ActiveKeyMissing,
}

#[derive(Clone, Debug)]
pub struct SqliteSecretStore {
    database: Database,
    keyring: Arc<SecretKeyring>,
}

/// SQLite-backed credential resolver permanently scoped to one Provider ID.
/// The scope is selected by Core at composition time rather than by Provider
/// request data.
#[derive(Clone, Debug)]
pub struct SqliteProviderCredentialResolver {
    store: SqliteSecretStore,
    provider_id: ProviderId,
}

impl SqliteProviderCredentialResolver {
    pub const fn new(store: SqliteSecretStore, provider_id: ProviderId) -> Self {
        Self { store, provider_id }
    }
}

impl SqliteSecretStore {
    pub fn new(database: Database, keyring: Arc<SecretKeyring>) -> Self {
        Self { database, keyring }
    }

    /// Builds a permanently Provider-scoped encrypted continuation repository
    /// for operations that occur before the first real Question snapshot.
    pub fn question_read_continuations(
        &self,
        provider_id: ProviderId,
    ) -> crate::SqliteQuestionReadContinuationRepository {
        crate::SqliteQuestionReadContinuationRepository::new(
            self.database.clone(),
            self.keyring.clone(),
            provider_id,
        )
    }

    /// Builds a permanently Provider-scoped encrypted continuation repository
    /// for operations on a materialized `QuestionSession`.
    pub fn question_session_artifacts(
        &self,
        provider_id: ProviderId,
    ) -> crate::SqliteQuestionSessionArtifactRepository {
        crate::SqliteQuestionSessionArtifactRepository::new(
            self.database.clone(),
            self.keyring.clone(),
            provider_id,
        )
    }

    /// Builds a permanently Provider-scoped encrypted continuation repository
    /// for restart-safe interactive authentication.
    pub fn interactive_auth_continuations(
        &self,
        provider_id: ProviderId,
    ) -> crate::SqliteInteractiveAuthContinuationRepository {
        crate::SqliteInteractiveAuthContinuationRepository::new(
            self.database.clone(),
            self.keyring.clone(),
            provider_id,
        )
    }

    /// Builds a permanently Provider-scoped encrypted `BrowserBridge` command
    /// repository. Core chooses the scope; Provider payloads cannot change it.
    pub fn browser_bridge_commands(
        &self,
        provider_id: ProviderId,
    ) -> crate::SqliteBrowserBridgeCommandArtifactRepository {
        crate::SqliteBrowserBridgeCommandArtifactRepository::new(
            self.database.clone(),
            self.keyring.clone(),
            provider_id,
        )
    }

    /// Builds a permanently Provider-scoped encrypted repository for the
    /// parent authority and complete batch snapshot frozen before child work.
    pub fn execution_parent_batch_snapshots(
        &self,
        provider_id: ProviderId,
    ) -> crate::SqliteExecutionParentBatchSnapshotRepository {
        crate::SqliteExecutionParentBatchSnapshotRepository::new(
            self.database.clone(),
            self.keyring.clone(),
            provider_id,
        )
    }

    /// Builds the encrypted parent authority repository for a Course-scoped
    /// `BatchExecution` Attempt.
    pub fn batch_execution_parent_snapshots(
        &self,
        provider_id: ProviderId,
    ) -> crate::SqliteBatchExecutionParentSnapshotRepository {
        crate::SqliteBatchExecutionParentSnapshotRepository::new(
            self.database.clone(),
            self.keyring.clone(),
            provider_id,
        )
    }

    async fn replace_provider_credentials_internal(
        &self,
        request: CredentialSetCommit<'_>,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
        let CredentialSetCommit {
            owner_user_id,
            provider_account_id,
            bundle,
            authenticated_session,
            access,
        } = request;
        authorize(owner_user_id, access)?;
        let (key_id, key) = self.keyring.active();
        let prepared =
            prepare_credential_bundle(owner_user_id, provider_account_id, bundle, key_id, key)?;
        let authenticated_at = authenticated_session.map(|(session, _)| session.updated_at);
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        ensure_account_binding(
            &mut transaction,
            owner_user_id,
            provider_account_id,
            &prepared.provider_id,
            prepared.tenant.as_deref(),
        )
        .await?;
        if let Some((session, expected_revision)) = authenticated_session {
            let actor = secret_audit_actor(&access.actor).ok_or(SecretStoreError::Unauthorized)?;
            let updated = update_auth_session_in_transaction(
                &mut transaction,
                session,
                expected_revision,
                actor,
                &access.correlation_id,
            )
            .await
            .map_err(|_| SecretStoreError::Storage)?;
            if !updated {
                return Err(SecretStoreError::VersionConflict);
            }
        }
        let replaced_count =
            replace_previous_credentials(&mut transaction, provider_account_id, access).await?;
        persist_prepared_credentials(&mut transaction, &prepared.credentials, access).await?;
        authenticate_provider_account(
            &mut transaction,
            owner_user_id,
            provider_account_id,
            &prepared.provider_id,
            authenticated_at.unwrap_or(prepared.prepared_at),
        )
        .await?;
        insert_bundle_audit(
            &mut transaction,
            access,
            provider_account_id,
            prepared.auth_method,
            prepared.session_kind,
            replaced_count,
            prepared.credentials.len(),
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(prepared
            .credentials
            .into_iter()
            .map(|prepared| prepared.credential)
            .collect())
    }
}

impl QuestionReadContinuationRepositoryFactory for SqliteSecretStore {
    fn for_provider(
        &self,
        provider_id: ProviderId,
    ) -> Arc<dyn crate::QuestionReadContinuationRepository> {
        Arc::new(self.question_read_continuations(provider_id))
    }
}

impl QuestionSessionArtifactRepositoryFactory for SqliteSecretStore {
    fn for_provider(
        &self,
        provider_id: ProviderId,
    ) -> Arc<dyn crate::QuestionSessionArtifactRepository> {
        Arc::new(self.question_session_artifacts(provider_id))
    }
}

impl crate::InteractiveAuthContinuationRepositoryFactory for SqliteSecretStore {
    fn for_provider(
        &self,
        provider_id: ProviderId,
    ) -> Arc<dyn crate::InteractiveAuthContinuationRepository> {
        Arc::new(self.interactive_auth_continuations(provider_id))
    }
}

#[async_trait]
impl SecretStore for SqliteSecretStore {
    async fn put(
        &self,
        owner_user_id: UserId,
        purpose: SecretPurpose,
        value: SecretValue,
        access: &SecretAccess,
    ) -> Result<SecretRef, SecretStoreError> {
        authorize(owner_user_id, access)?;
        if purpose.is_provider_credential() {
            return Err(SecretStoreError::CredentialManaged);
        }
        validate_secret(&value)?;
        let now = Utc::now();
        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id,
            purpose,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: now,
            updated_at: now,
        };
        let (nonce, encrypted_data) = encrypt(key, &secret, value.expose_secret())?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO secret_blobs \
             (id, owner_user_id, purpose, key_id, nonce, encrypted_data, version, created_at, \
              updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(secret.id.to_string())
        .bind(secret.owner_user_id.to_string())
        .bind(encode_purpose(secret.purpose))
        .bind(&secret.key_id)
        .bind(nonce)
        .bind(encrypted_data)
        .bind(i64::from(secret.version))
        .bind(encode_timestamp(secret.created_at))
        .bind(encode_timestamp(secret.updated_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        insert_secret_audit(&mut transaction, access, "secret_stored", &secret)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(secret)
    }

    async fn get(
        &self,
        secret: &SecretRef,
        access: &SecretAccess,
    ) -> Result<SecretValue, SecretStoreError> {
        authorize(secret.owner_user_id, access)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let row = fetch_secret(&mut transaction, secret.id).await?;
        verify_reference(secret, &row)?;
        let key = self.keyring.get(&row.key_id)?;
        let plaintext = decrypt(key, secret, &row.nonce, &row.encrypted_data)?;
        insert_secret_audit(&mut transaction, access, "secret_accessed", secret)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(SecretValue::new(plaintext))
    }

    async fn rotate(
        &self,
        secret: &SecretRef,
        replacement: SecretValue,
        access: &SecretAccess,
    ) -> Result<SecretRef, SecretStoreError> {
        authorize(secret.owner_user_id, access)?;
        if secret.purpose.is_provider_credential() {
            return Err(SecretStoreError::CredentialManaged);
        }
        validate_secret(&replacement)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let row = fetch_secret(&mut transaction, secret.id).await?;
        verify_reference(secret, &row)?;
        let version = secret
            .version
            .checked_add(1)
            .ok_or(SecretStoreError::VersionConflict)?;
        let (key_id, key) = self.keyring.active();
        let rotated = SecretRef {
            version,
            key_id: key_id.to_owned(),
            updated_at: Utc::now(),
            ..secret.clone()
        };
        let (nonce, encrypted_data) = encrypt(key, &rotated, replacement.expose_secret())?;
        let result = sqlx::query(
            "UPDATE secret_blobs SET key_id = ?, nonce = ?, encrypted_data = ?, version = ?, \
             updated_at = ? WHERE id = ? AND version = ?",
        )
        .bind(&rotated.key_id)
        .bind(nonce)
        .bind(encrypted_data)
        .bind(i64::from(rotated.version))
        .bind(encode_timestamp(rotated.updated_at))
        .bind(rotated.id.to_string())
        .bind(i64::from(secret.version))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        insert_secret_audit(&mut transaction, access, "secret_rotated", &rotated)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(rotated)
    }

    async fn delete(
        &self,
        secret: &SecretRef,
        access: &SecretAccess,
    ) -> Result<(), SecretStoreError> {
        authorize(secret.owner_user_id, access)?;
        if secret.purpose.is_provider_credential() {
            return Err(SecretStoreError::CredentialManaged);
        }
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let row = fetch_secret(&mut transaction, secret.id).await?;
        verify_reference(secret, &row)?;
        let result = sqlx::query("DELETE FROM secret_blobs WHERE id = ? AND version = ?")
            .bind(secret.id.to_string())
            .bind(i64::from(secret.version))
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        insert_secret_audit(&mut transaction, access, "secret_deleted", secret)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }
}

#[async_trait]
impl ProviderCredentialStore for SqliteSecretStore {
    async fn replace_provider_credentials(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        bundle: CredentialBundle,
        access: &SecretAccess,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
        self.replace_provider_credentials_internal(CredentialSetCommit {
            owner_user_id,
            provider_account_id,
            bundle,
            authenticated_session: None,
            access,
        })
        .await
    }

    async fn create_provider_credential(
        &self,
        owner_user_id: UserId,
        credential: NewProviderCredential,
        access: &SecretAccess,
    ) -> Result<ProviderCredential, SecretStoreError> {
        authorize(owner_user_id, access)?;
        validate_secret(&credential.value)?;
        let now = Utc::now();
        let (key_id, key) = self.keyring.active();
        let secret = SecretRef {
            id: SecretId::new(),
            owner_user_id,
            purpose: credential.purpose,
            version: 1,
            key_id: key_id.to_owned(),
            created_at: now,
            updated_at: now,
        };
        let stored = ProviderCredential {
            provider_account_id: credential.provider_account_id,
            secret,
            session_kind: credential.session_kind,
            acquired_via: credential.acquired_via,
            captured_at: now,
            expires_at: credential.expires_at,
            updated_at: now,
        };
        stored
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        let (nonce, encrypted_data) =
            encrypt(key, &stored.secret, credential.value.expose_secret())?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        ensure_account_owner(
            &mut transaction,
            owner_user_id,
            credential.provider_account_id,
        )
        .await?;
        insert_secret_blob(&mut transaction, &stored.secret, &nonce, &encrypted_data).await?;
        insert_provider_credential(&mut transaction, &stored).await?;
        insert_secret_audit(&mut transaction, access, "secret_stored", &stored.secret)
            .await
            .map_err(storage_error)?;
        insert_credential_audit(
            &mut transaction,
            access,
            "provider_credential_created",
            &stored,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(stored)
    }

    async fn list_provider_credentials(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        access: &SecretAccess,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
        authorize(owner_user_id, access)?;
        let mut transaction = self.database.pool().begin().await.map_err(storage_error)?;
        ensure_account_owner(&mut transaction, owner_user_id, provider_account_id).await?;
        let rows = sqlx::query(CREDENTIAL_SELECT)
            .bind(provider_account_id.to_string())
            .fetch_all(&mut *transaction)
            .await
            .map_err(storage_error)?;
        let credentials = rows
            .iter()
            .map(decode_provider_credential)
            .collect::<Result<Vec<_>, _>>()?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(credentials)
    }

    async fn rotate_provider_credential(
        &self,
        owner_user_id: UserId,
        credential: &ProviderCredential,
        replacement: SecretValue,
        expires_at: Option<Timestamp>,
        access: &SecretAccess,
    ) -> Result<ProviderCredential, SecretStoreError> {
        authorize(owner_user_id, access)?;
        validate_secret(&replacement)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        ensure_account_owner(
            &mut transaction,
            owner_user_id,
            credential.provider_account_id,
        )
        .await?;
        let persisted = fetch_provider_credential(
            &mut transaction,
            credential.provider_account_id,
            credential.secret.id,
        )
        .await?;
        if &persisted != credential {
            return Err(SecretStoreError::VersionConflict);
        }
        let row = fetch_secret(&mut transaction, credential.secret.id).await?;
        verify_reference(&credential.secret, &row)?;
        let version = credential
            .secret
            .version
            .checked_add(1)
            .ok_or(SecretStoreError::VersionConflict)?;
        let now = Utc::now();
        let (key_id, key) = self.keyring.active();
        let rotated = ProviderCredential {
            secret: SecretRef {
                version,
                key_id: key_id.to_owned(),
                updated_at: now,
                ..credential.secret.clone()
            },
            expires_at,
            updated_at: now,
            ..credential.clone()
        };
        rotated
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        let (nonce, encrypted_data) = encrypt(key, &rotated.secret, replacement.expose_secret())?;
        let secret_result = sqlx::query(
            "UPDATE secret_blobs SET key_id = ?, nonce = ?, encrypted_data = ?, version = ?, \
             updated_at = ? WHERE id = ? AND version = ?",
        )
        .bind(&rotated.secret.key_id)
        .bind(nonce)
        .bind(encrypted_data)
        .bind(i64::from(rotated.secret.version))
        .bind(encode_timestamp(rotated.secret.updated_at))
        .bind(rotated.secret.id.to_string())
        .bind(i64::from(credential.secret.version))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let credential_result = sqlx::query(
            "UPDATE provider_account_credentials SET expires_at = ?, updated_at = ? \
             WHERE provider_account_id = ? AND secret_blob_id = ? AND updated_at = ?",
        )
        .bind(rotated.expires_at.map(encode_timestamp))
        .bind(encode_timestamp(rotated.updated_at))
        .bind(rotated.provider_account_id.to_string())
        .bind(rotated.secret.id.to_string())
        .bind(encode_timestamp(credential.updated_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if secret_result.rows_affected() != 1 || credential_result.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        insert_secret_audit(&mut transaction, access, "secret_rotated", &rotated.secret)
            .await
            .map_err(storage_error)?;
        insert_credential_audit(
            &mut transaction,
            access,
            "provider_credential_rotated",
            &rotated,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(rotated)
    }

    async fn delete_provider_credential(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        secret_id: SecretId,
        access: &SecretAccess,
    ) -> Result<(), SecretStoreError> {
        authorize(owner_user_id, access)?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        ensure_account_owner(&mut transaction, owner_user_id, provider_account_id).await?;
        let credential =
            fetch_provider_credential(&mut transaction, provider_account_id, secret_id).await?;
        let result = sqlx::query("DELETE FROM secret_blobs WHERE id = ? AND version = ?")
            .bind(secret_id.to_string())
            .bind(i64::from(credential.secret.version))
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(SecretStoreError::VersionConflict);
        }
        insert_secret_audit(
            &mut transaction,
            access,
            "secret_deleted",
            &credential.secret,
        )
        .await
        .map_err(storage_error)?;
        insert_credential_audit(
            &mut transaction,
            access,
            "provider_credential_deleted",
            &credential,
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }
}

#[async_trait]
impl ProviderCredentialResolver for SqliteProviderCredentialResolver {
    async fn resolve_provider_credentials(
        &self,
        request: ProviderCredentialResolution,
    ) -> Result<Vec<ResolvedProviderCredential>, SecretStoreError> {
        validate_resolution_request(&request.credential_refs, &request.purposes)?;
        let access = SecretAccess {
            actor: SecretActor::ProviderRuntime(self.provider_id.to_string()),
            correlation_id: request.correlation_id,
            reason: "resolve account-bound Provider credentials".to_owned(),
        };
        let mut transaction = self
            .store
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let account = resolve_runtime_account_binding(
            &mut transaction,
            request.provider_account_id,
            &self.provider_id,
            &access,
        )
        .await?;

        let rows = sqlx::query(CREDENTIAL_SELECT)
            .bind(request.provider_account_id.to_string())
            .fetch_all(&mut *transaction)
            .await
            .map_err(storage_error)?;
        let credentials = rows
            .iter()
            .map(decode_provider_credential)
            .collect::<Result<Vec<_>, _>>()?;
        let requested_purposes = validate_runtime_credential_binding(
            &credentials,
            request.provider_account_id,
            account.owner_user_id,
            &request.credential_refs,
            request.purposes,
        )?;

        let mut resolved = Vec::with_capacity(requested_purposes.len());
        for credential in credentials
            .into_iter()
            .filter(|credential| requested_purposes.contains(&credential.secret.purpose))
        {
            let row = fetch_secret(&mut transaction, credential.secret.id).await?;
            verify_reference(&credential.secret, &row)?;
            let key = self.store.keyring.get(&row.key_id)?;
            let value = SecretValue::new(decrypt(
                key,
                &credential.secret,
                &row.nonce,
                &row.encrypted_data,
            )?);
            insert_secret_audit(
                &mut transaction,
                &access,
                "provider_credential_resolved",
                &credential.secret,
            )
            .await
            .map_err(storage_error)?;
            resolved.push(ResolvedProviderCredential { credential, value });
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(resolved)
    }
}

#[async_trait]
impl ProviderCredentialRenewer for SqliteProviderCredentialResolver {
    async fn renew_provider_credentials(
        &self,
        request: ProviderCredentialRenewal,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
        if request.expected_credentials.is_empty()
            || request.expected_credentials.len() > 16
            || request
                .expected_credentials
                .iter()
                .map(|credential| credential.secret.id)
                .collect::<BTreeSet<_>>()
                .len()
                != request.expected_credentials.len()
        {
            return Err(SecretStoreError::AccountMismatch);
        }
        request
            .bundle
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        let access = SecretAccess {
            actor: SecretActor::ProviderRuntime(self.provider_id.to_string()),
            correlation_id: request.correlation_id,
            reason: "renew account-bound Provider credentials".to_owned(),
        };
        let mut transaction = self
            .store
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let account = resolve_runtime_account_binding(
            &mut transaction,
            request.provider_account_id,
            &self.provider_id,
            &access,
        )
        .await?;
        if request.bundle.provider_id != self.provider_id || request.bundle.tenant != account.tenant
        {
            return Err(SecretStoreError::AccountMismatch);
        }
        let rows = sqlx::query(CREDENTIAL_SELECT)
            .bind(request.provider_account_id.to_string())
            .fetch_all(&mut *transaction)
            .await
            .map_err(storage_error)?;
        let credentials = rows
            .iter()
            .map(decode_provider_credential)
            .collect::<Result<Vec<_>, _>>()?;
        if !runtime_credentials_match(
            &credentials,
            &request.expected_credentials,
            request.provider_account_id,
            account.owner_user_id,
        ) {
            return Err(SecretStoreError::VersionConflict);
        }

        let (key_id, key) = self.store.keyring.active();
        let prepared = prepare_credential_bundle(
            account.owner_user_id,
            request.provider_account_id,
            request.bundle,
            key_id,
            key,
        )?;
        let replaced_count =
            replace_previous_credentials(&mut transaction, request.provider_account_id, &access)
                .await?;
        persist_prepared_credentials(&mut transaction, &prepared.credentials, &access).await?;
        authenticate_provider_account(
            &mut transaction,
            account.owner_user_id,
            request.provider_account_id,
            &self.provider_id,
            prepared.prepared_at,
        )
        .await?;
        insert_bundle_audit(
            &mut transaction,
            &access,
            request.provider_account_id,
            prepared.auth_method,
            prepared.session_kind,
            replaced_count,
            prepared.credentials.len(),
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(prepared
            .credentials
            .into_iter()
            .map(|prepared| prepared.credential)
            .collect())
    }
}

fn validate_resolution_request(
    credential_refs: &[SecretId],
    purposes: &[SecretPurpose],
) -> Result<(), SecretStoreError> {
    let references_valid = credential_refs_are_valid(credential_refs);
    let purposes_valid = !purposes.is_empty()
        && purposes.len() <= 16
        && purposes.iter().copied().collect::<HashSet<_>>().len() == purposes.len()
        && purposes
            .iter()
            .all(|purpose| purpose.is_provider_credential());
    if references_valid && purposes_valid {
        Ok(())
    } else {
        Err(SecretStoreError::AccountMismatch)
    }
}

fn credential_refs_are_valid(credential_refs: &[SecretId]) -> bool {
    !credential_refs.is_empty()
        && credential_refs.len() <= 16
        && credential_refs.iter().collect::<BTreeSet<_>>().len() == credential_refs.len()
}

struct RuntimeProviderAccount {
    owner_user_id: UserId,
    tenant: Option<String>,
}

async fn resolve_runtime_account_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    provider_account_id: ProviderAccountId,
    provider_id: &ProviderId,
    access: &SecretAccess,
) -> Result<RuntimeProviderAccount, SecretStoreError> {
    let account = sqlx::query(
        "SELECT owner_user_id, provider_id, tenant, auth_state_json \
         FROM provider_accounts WHERE id = ?",
    )
    .bind(provider_account_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(SecretStoreError::NotFound)?;
    let owner_user_id = UserId::from_str(
        account
            .try_get::<&str, _>("owner_user_id")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    authorize(owner_user_id, access)?;
    let stored_provider_id: &str = account.try_get("provider_id").map_err(storage_error)?;
    let auth_state: AuthState = serde_json::from_str(
        account
            .try_get::<&str, _>("auth_state_json")
            .map_err(storage_error)?,
    )
    .map_err(|_| SecretStoreError::Storage)?;
    if stored_provider_id == provider_id.as_str() && matches!(auth_state, AuthState::Authenticated)
    {
        Ok(RuntimeProviderAccount {
            owner_user_id,
            tenant: account.try_get("tenant").map_err(storage_error)?,
        })
    } else {
        Err(SecretStoreError::AccountMismatch)
    }
}

fn validate_runtime_credential_binding(
    credentials: &[ProviderCredential],
    provider_account_id: ProviderAccountId,
    owner_user_id: UserId,
    credential_refs: &[SecretId],
    purposes: Vec<SecretPurpose>,
) -> Result<HashSet<SecretPurpose>, SecretStoreError> {
    if !runtime_credential_refs_match(
        credentials,
        provider_account_id,
        owner_user_id,
        credential_refs,
    ) {
        return Err(SecretStoreError::AccountMismatch);
    }
    let requested_purposes = purposes.into_iter().collect::<HashSet<_>>();
    let stored_purposes = credentials
        .iter()
        .map(|credential| credential.secret.purpose)
        .collect::<HashSet<_>>();
    if requested_purposes.is_subset(&stored_purposes) {
        Ok(requested_purposes)
    } else {
        Err(SecretStoreError::AccountMismatch)
    }
}

fn runtime_credential_refs_match(
    credentials: &[ProviderCredential],
    provider_account_id: ProviderAccountId,
    owner_user_id: UserId,
    credential_refs: &[SecretId],
) -> bool {
    let requested_refs = credential_refs.iter().copied().collect::<BTreeSet<_>>();
    let stored_refs = credentials
        .iter()
        .map(|credential| credential.secret.id)
        .collect::<BTreeSet<_>>();
    requested_refs == stored_refs
        && credentials.len() == stored_refs.len()
        && credentials.iter().all(|credential| {
            credential.provider_account_id == provider_account_id
                && credential.secret.owner_user_id == owner_user_id
        })
}

fn runtime_credentials_match(
    stored: &[ProviderCredential],
    expected: &[ProviderCredential],
    provider_account_id: ProviderAccountId,
    owner_user_id: UserId,
) -> bool {
    stored.len() == expected.len()
        && stored.iter().all(|credential| {
            credential.provider_account_id == provider_account_id
                && credential.secret.owner_user_id == owner_user_id
                && expected.contains(credential)
        })
}

#[async_trait]
impl AuthBootstrapCredentialRepository for SqliteSecretStore {
    async fn commit_auth_bootstrap_credentials(
        &self,
        request: AuthBootstrapCredentialCommitRequest<'_>,
    ) -> Result<AuthBootstrapCredentialCommitOutcome, SecretStoreError> {
        let AuthBootstrapCredentialCommitRequest {
            session_id,
            access_token_digest,
            mut validated_account,
            bundle,
            completed_at,
            access,
        } = request;
        authorize(validated_account.owner_id, access)?;
        let (key_id, key) = self.keyring.active();
        let prepared = prepare_credential_bundle(
            validated_account.owner_id,
            validated_account.id,
            bundle,
            key_id,
            key,
        )?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(session) = authenticate_access_in_transaction(
            &mut transaction,
            session_id,
            access_token_digest,
            completed_at,
        )
        .await
        .map_err(|_| SecretStoreError::Storage)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(AuthBootstrapCredentialCommitOutcome::AccessRejected);
        };
        if !prepare_bootstrap_account_binding(
            &mut transaction,
            &session,
            &validated_account,
            &prepared,
            completed_at,
            access,
        )
        .await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(AuthBootstrapCredentialCommitOutcome::BindingConflict);
        }
        persist_bootstrap_credentials(
            &mut transaction,
            &validated_account,
            &prepared,
            completed_at,
            access,
        )
        .await?;
        let Some(completed_session) = complete_auth_bootstrap_in_transaction(
            &mut transaction,
            &session,
            access_token_digest,
            validated_account.id,
            completed_at,
            &access.correlation_id,
        )
        .await
        .map_err(|_| SecretStoreError::Storage)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(AuthBootstrapCredentialCommitOutcome::BindingConflict);
        };
        transaction.commit().await.map_err(storage_error)?;
        let credentials = prepared
            .credentials
            .into_iter()
            .map(|prepared| prepared.credential)
            .collect::<Vec<_>>();
        validated_account.auth_state = AuthState::Authenticated;
        validated_account.credential_refs = credentials
            .iter()
            .map(|credential| credential.secret.id)
            .collect();
        validated_account.credential_refs.sort_unstable();
        validated_account.updated_at = completed_at;
        Ok(AuthBootstrapCredentialCommitOutcome::Committed(Box::new(
            AuthBootstrapCredentialCommit {
                session: completed_session,
                account: validated_account,
                credentials,
            },
        )))
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl BrowserBridgeCredentialRepository for SqliteSecretStore {
    async fn commit_browser_bridge_credentials(
        &self,
        request: BrowserBridgeCredentialCommitRequest<'_>,
    ) -> Result<BrowserBridgeCredentialCommitOutcome, SecretStoreError> {
        request
            .exchange
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        let Some(completed_at) = request.exchange.completed_at else {
            return Err(SecretStoreError::InvalidValue);
        };
        if request.exchange.state != BrowserBridgeExchangeState::Completed
            || request.validated_bundle.captured_at < request.exchange.issued_at
            || request.validated_bundle.captured_at > completed_at
            || !matches!(
                request.validated_bundle.acquired_via,
                CredentialAcquisition::CaptureTool
                    | CredentialAcquisition::BrowserExtension
                    | CredentialAcquisition::AndroidHelper
            )
        {
            return Err(SecretStoreError::InvalidValue);
        }
        request
            .validated_bundle
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;

        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        let Some(session) = find_claimed_session_for_exchange(
            &mut transaction,
            request.exchange.session_id,
            completed_at,
        )
        .await
        .map_err(|_| SecretStoreError::Storage)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::BindingConflict);
        };
        authorize_browser_bridge_secret_access(&session, request.access)?;
        if request.owner_user_id != session.owner_user_id
            || request.provider_account_id != session.provider_account_id
            || request.task_id != session.task_id
            || request.validated_bundle.provider_id != session.provider_id
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::BindingConflict);
        }
        match ensure_account_binding(
            &mut transaction,
            session.owner_user_id,
            session.provider_account_id,
            &session.provider_id,
            request.validated_bundle.tenant.as_deref(),
        )
        .await
        {
            Ok(()) => {}
            Err(SecretStoreError::NotFound | SecretStoreError::AccountMismatch) => {
                transaction.rollback().await.map_err(storage_error)?;
                return Ok(BrowserBridgeCredentialCommitOutcome::BindingConflict);
            }
            Err(error) => return Err(error),
        }

        let sequence =
            i64::try_from(request.exchange.sequence).map_err(|_| SecretStoreError::InvalidValue)?;
        let Some(existing) =
            fetch_exchange(&mut transaction, request.exchange.session_id, sequence)
                .await
                .map_err(|_| SecretStoreError::Storage)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::SequenceConflict);
        };
        let artifact_present: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM browser_bridge_exchanges \
             JOIN browser_bridge_result_artifacts AS result \
               ON result.session_id = browser_bridge_exchanges.session_id \
              AND result.sequence = browser_bridge_exchanges.sequence \
             WHERE browser_bridge_exchanges.session_id = ? \
               AND browser_bridge_exchanges.sequence = ? \
               AND browser_bridge_exchanges.command_secret_blob_id IS NOT NULL \
               AND result.result_type = ? AND result.result_digest = ?)",
        )
        .bind(request.exchange.session_id.to_string())
        .bind(sequence)
        .bind(request.exchange.result_type.as_deref())
        .bind(request.exchange.result_digest.map(|digest| digest.to_vec()))
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if artifact_present != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::BindingConflict);
        }
        if existing.state != BrowserBridgeExchangeState::Issued
            || existing.command_type != request.exchange.command_type
            || existing.command_digest != request.exchange.command_digest
            || existing.issued_at != request.exchange.issued_at
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::SequenceConflict);
        }

        let (key_id, key) = self.keyring.active();
        let prepared = prepare_credential_bundle(
            session.owner_user_id,
            session.provider_account_id,
            request.validated_bundle,
            key_id,
            key,
        )?;
        let updated_exchange = sqlx::query(
            "UPDATE browser_bridge_exchanges SET result_type = ?, result_digest = ?, \
             state = 'completed', completed_at = ? \
             WHERE session_id = ? AND sequence = ? AND state = 'issued' \
               AND command_type = ? AND command_digest = ? AND issued_at = ?",
        )
        .bind(request.exchange.result_type.as_deref())
        .bind(request.exchange.result_digest.map(|digest| digest.to_vec()))
        .bind(encode_timestamp(completed_at))
        .bind(request.exchange.session_id.to_string())
        .bind(sequence)
        .bind(&request.exchange.command_type)
        .bind(request.exchange.command_digest.as_slice())
        .bind(encode_timestamp(request.exchange.issued_at))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if updated_exchange.rows_affected() != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::SequenceConflict);
        }

        let replaced_count = replace_previous_credentials(
            &mut transaction,
            session.provider_account_id,
            request.access,
        )
        .await?;
        persist_prepared_credentials(&mut transaction, &prepared.credentials, request.access)
            .await?;
        authenticate_provider_account(
            &mut transaction,
            session.owner_user_id,
            session.provider_account_id,
            &session.provider_id,
            prepared.prepared_at,
        )
        .await?;
        insert_bundle_audit(
            &mut transaction,
            request.access,
            session.provider_account_id,
            prepared.auth_method,
            prepared.session_kind,
            replaced_count,
            prepared.credentials.len(),
        )
        .await
        .map_err(storage_error)?;

        let mut completed_session = session;
        completed_session
            .complete(completed_at)
            .map_err(|_| SecretStoreError::VersionConflict)?;
        let updated_session = sqlx::query(
            "UPDATE browser_bridge_sessions SET state_json = ?, pairing_token_hash = NULL, \
             access_token_hash = NULL, revision = ?, updated_at = ? \
             WHERE id = ? AND access_token_hash IS NOT NULL AND pairing_token_hash IS NULL \
               AND state_json = ? AND revision = 2",
        )
        .bind(
            serde_json::to_string(&completed_session.state)
                .map_err(|_| SecretStoreError::Storage)?,
        )
        .bind(i64::from(completed_session.revision))
        .bind(encode_timestamp(completed_session.updated_at))
        .bind(completed_session.id.to_string())
        .bind(
            serde_json::to_string(&BrowserBridgeSessionState::Claimed)
                .map_err(|_| SecretStoreError::Storage)?,
        )
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if updated_session.rows_affected() != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(BrowserBridgeCredentialCommitOutcome::BindingConflict);
        }
        insert_exchange_audit(
            &mut transaction,
            &request.access.correlation_id,
            &completed_session,
            request.exchange,
            "credential_committed",
        )
        .await
        .map_err(|_| SecretStoreError::Storage)?;
        insert_browser_bridge_session_audit(&mut transaction, request.access, &completed_session)
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        let credentials = prepared
            .credentials
            .into_iter()
            .map(|prepared| prepared.credential)
            .collect();
        Ok(BrowserBridgeCredentialCommitOutcome::Committed(Box::new(
            BrowserBridgeCredentialCommit {
                session: completed_session,
                exchange: request.exchange.clone(),
                credentials,
            },
        )))
    }
}

#[async_trait]
impl AuthenticatedCredentialRepository for SqliteSecretStore {
    async fn commit_authenticated_credentials(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        bundle: CredentialBundle,
        authenticated_session: &AuthSession,
        expected_session_revision: u32,
        access: &SecretAccess,
    ) -> Result<Vec<ProviderCredential>, SecretStoreError> {
        self.replace_provider_credentials_internal(CredentialSetCommit {
            owner_user_id,
            provider_account_id,
            bundle,
            authenticated_session: Some((authenticated_session, expected_session_revision)),
            access,
        })
        .await
    }
}

#[async_trait]
impl InteractiveAuthCredentialRepository for SqliteSecretStore {
    #[allow(clippy::too_many_lines)]
    async fn commit_interactive_auth_credentials(
        &self,
        request: InteractiveAuthCredentialCommitRequest<'_>,
    ) -> Result<InteractiveAuthCredentialCommitOutcome, SecretStoreError> {
        let InteractiveAuthCredentialCommitRequest {
            owner_user_id,
            provider_account_id,
            authenticated_session,
            expected_session_revision,
            continuation,
            terminal_result_digest,
            bundle,
            access,
        } = request;
        authorize(owner_user_id, access)?;
        if terminal_result_digest == [0; 32]
            || authenticated_session.owner_user_id != owner_user_id
            || authenticated_session.provider_account_id != provider_account_id
            || authenticated_session.state != AuthState::Authenticated
            || authenticated_session.revision != expected_session_revision.saturating_add(1)
            || continuation.auth_session_id != authenticated_session.id
            || continuation.provider_id != bundle.provider_id
            || continuation.revision.saturating_add(1) != expected_session_revision
            || continuation.terminal_result_digest != Some(terminal_result_digest)
        {
            return Err(SecretStoreError::InvalidValue);
        }
        let prepared = prepare_credential_bundle(
            owner_user_id,
            provider_account_id,
            bundle,
            self.keyring.active().0,
            self.keyring.active().1,
        )?;
        let mut transaction = self
            .database
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage_error)?;
        ensure_account_binding(
            &mut transaction,
            owner_user_id,
            provider_account_id,
            &prepared.provider_id,
            prepared.tenant.as_deref(),
        )
        .await?;
        let Some(current_session) = fetch_auth_session(&mut transaction, authenticated_session.id)
            .await
            .map_err(|_| SecretStoreError::Storage)?
        else {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthCredentialCommitOutcome::BindingConflict);
        };
        if current_session.owner_user_id != owner_user_id
            || current_session.provider_account_id != provider_account_id
            || current_session.state != AuthState::ValidatingCredential
            || current_session.revision != expected_session_revision
            || !crate::interactive_auth::consume_interactive_auth_candidate(
                &mut transaction,
                continuation,
                terminal_result_digest,
                owner_user_id,
                access,
            )
            .await?
            || !update_auth_session_in_transaction(
                &mut transaction,
                authenticated_session,
                expected_session_revision,
                secret_audit_actor(&access.actor).ok_or(SecretStoreError::Unauthorized)?,
                &access.correlation_id,
            )
            .await
            .map_err(|_| SecretStoreError::Storage)?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(InteractiveAuthCredentialCommitOutcome::BindingConflict);
        }
        let replaced_count =
            replace_previous_credentials(&mut transaction, provider_account_id, access).await?;
        persist_prepared_credentials(&mut transaction, &prepared.credentials, access).await?;
        authenticate_provider_account(
            &mut transaction,
            owner_user_id,
            provider_account_id,
            &prepared.provider_id,
            authenticated_session.updated_at,
        )
        .await?;
        insert_bundle_audit(
            &mut transaction,
            access,
            provider_account_id,
            prepared.auth_method,
            prepared.session_kind,
            replaced_count,
            prepared.credentials.len(),
        )
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(InteractiveAuthCredentialCommitOutcome::Committed(
            InteractiveAuthCredentialCommit {
                session: authenticated_session.clone(),
                credentials: prepared
                    .credentials
                    .into_iter()
                    .map(|prepared| prepared.credential)
                    .collect(),
            },
        ))
    }
}

struct CredentialSetCommit<'a> {
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    bundle: CredentialBundle,
    authenticated_session: Option<(&'a AuthSession, u32)>,
    access: &'a SecretAccess,
}

struct PreparedCredentialBundle {
    provider_id: ProviderId,
    tenant: Option<String>,
    auth_method: AuthMethod,
    session_kind: SessionKind,
    prepared_at: Timestamp,
    credentials: Vec<PreparedCredential>,
}

struct PreparedCredential {
    credential: ProviderCredential,
    nonce: Vec<u8>,
    encrypted_data: Vec<u8>,
}

async fn prepare_bootstrap_account_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &asterism_domain::AuthBootstrapSession,
    account: &ProviderAccount,
    prepared: &PreparedCredentialBundle,
    completed_at: Timestamp,
    access: &SecretAccess,
) -> Result<bool, SecretStoreError> {
    let base_binding_matches = session.owner_user_id == account.owner_id
        && session.provider_id == account.provider_id
        && prepared.provider_id == account.provider_id
        && prepared.tenant == account.tenant;
    if !base_binding_matches {
        return Ok(false);
    }
    let binding_matches = match session.purpose {
        AuthBootstrapPurpose::AddAccount => {
            session.provider_account_id.is_none()
                && validate_new_bootstrap_account(account, completed_at)
        }
        AuthBootstrapPurpose::Reauthenticate | AuthBootstrapPurpose::RepairSession => {
            session.provider_account_id == Some(account.id)
                && account_snapshot_matches(transaction, account).await?
        }
    };
    if !binding_matches {
        return Ok(false);
    }
    if session.purpose == AuthBootstrapPurpose::AddAccount {
        insert_bootstrap_provider_account(transaction, account, session.id, access).await?;
    }
    Ok(true)
}

async fn persist_bootstrap_credentials(
    transaction: &mut Transaction<'_, Sqlite>,
    account: &ProviderAccount,
    prepared: &PreparedCredentialBundle,
    completed_at: Timestamp,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    let replaced_count = replace_previous_credentials(transaction, account.id, access).await?;
    persist_prepared_credentials(transaction, &prepared.credentials, access).await?;
    authenticate_provider_account(
        transaction,
        account.owner_id,
        account.id,
        &prepared.provider_id,
        completed_at,
    )
    .await?;
    insert_bundle_audit(
        transaction,
        access,
        account.id,
        prepared.auth_method,
        prepared.session_kind,
        replaced_count,
        prepared.credentials.len(),
    )
    .await
    .map_err(storage_error)
}

fn validate_new_bootstrap_account(account: &ProviderAccount, completed_at: Timestamp) -> bool {
    let display_name_valid = !account.display_name.is_empty()
        && account.display_name.len() <= 128
        && account.display_name.trim() == account.display_name
        && !account.display_name.chars().any(char::is_control);
    let tenant_valid = account.tenant.as_ref().is_none_or(|tenant| {
        !tenant.is_empty()
            && tenant.len() <= 256
            && tenant.trim() == tenant
            && !tenant.chars().any(char::is_control)
    });
    display_name_valid
        && tenant_valid
        && account.auth_state == AuthState::Idle
        && account.network_profile_id.is_none()
        && account.credential_refs.is_empty()
        && account.created_at == completed_at
        && account.updated_at == completed_at
}

async fn account_snapshot_matches(
    transaction: &mut Transaction<'_, Sqlite>,
    account: &ProviderAccount,
) -> Result<bool, SecretStoreError> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM provider_accounts \
         WHERE id = ? AND owner_user_id = ? AND provider_id = ? AND display_name = ? \
           AND tenant IS ? AND auth_state_json = ? AND network_profile_id IS ? \
           AND created_at = ? AND updated_at = ?)",
    )
    .bind(account.id.to_string())
    .bind(account.owner_id.to_string())
    .bind(account.provider_id.as_str())
    .bind(&account.display_name)
    .bind(&account.tenant)
    .bind(serde_json::to_string(&account.auth_state).map_err(|_| SecretStoreError::Storage)?)
    .bind(&account.network_profile_id)
    .bind(encode_timestamp(account.created_at))
    .bind(encode_timestamp(account.updated_at))
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(exists == 1)
}

async fn insert_bootstrap_provider_account(
    transaction: &mut Transaction<'_, Sqlite>,
    account: &ProviderAccount,
    bootstrap_session_id: AuthBootstrapSessionId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO provider_accounts \
         (id, owner_user_id, provider_id, display_name, tenant, auth_state_json, \
          network_profile_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
    )
    .bind(account.id.to_string())
    .bind(account.owner_id.to_string())
    .bind(account.provider_id.as_str())
    .bind(&account.display_name)
    .bind(&account.tenant)
    .bind(serde_json::to_string(&account.auth_state).map_err(|_| SecretStoreError::Storage)?)
    .bind(encode_timestamp(account.created_at))
    .bind(encode_timestamp(account.updated_at))
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, 'auth_bootstrap_session', ?, 'provider_account_created', \
                 'provider_account', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(account.created_at))
    .bind(bootstrap_session_id.to_string())
    .bind(account.id.to_string())
    .bind(&access.correlation_id)
    .bind(serde_json::json!({ "provider_id": account.provider_id }).to_string())
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn prepare_credential_bundle(
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    bundle: CredentialBundle,
    key_id: &str,
    key: &SecretKey,
) -> Result<PreparedCredentialBundle, SecretStoreError> {
    bundle
        .validate()
        .map_err(|_| SecretStoreError::InvalidValue)?;
    let CredentialBundle {
        provider_id,
        tenant,
        auth_method,
        acquired_via,
        captured_at,
        expires_at,
        session_kind,
        fields,
        user_id_hint: _,
    } = bundle;
    let prepared_at = Utc::now();
    let mut credentials = Vec::with_capacity(fields.len());
    for field in fields {
        validate_secret(&field.value)?;
        let credential = ProviderCredential {
            provider_account_id,
            secret: SecretRef {
                id: SecretId::new(),
                owner_user_id,
                purpose: field.purpose,
                version: 1,
                key_id: key_id.to_owned(),
                created_at: prepared_at,
                updated_at: prepared_at,
            },
            session_kind,
            acquired_via,
            captured_at,
            expires_at,
            updated_at: prepared_at,
        };
        credential
            .validate()
            .map_err(|_| SecretStoreError::InvalidValue)?;
        let (nonce, encrypted_data) =
            encrypt(key, &credential.secret, field.value.expose_secret())?;
        credentials.push(PreparedCredential {
            credential,
            nonce,
            encrypted_data,
        });
    }
    Ok(PreparedCredentialBundle {
        provider_id,
        tenant,
        auth_method,
        session_kind,
        prepared_at,
        credentials,
    })
}

async fn replace_previous_credentials(
    transaction: &mut Transaction<'_, Sqlite>,
    provider_account_id: ProviderAccountId,
    access: &SecretAccess,
) -> Result<usize, SecretStoreError> {
    let rows = sqlx::query(CREDENTIAL_SELECT)
        .bind(provider_account_id.to_string())
        .fetch_all(&mut **transaction)
        .await
        .map_err(storage_error)?;
    let previous = rows
        .iter()
        .map(decode_provider_credential)
        .collect::<Result<Vec<_>, _>>()?;
    for credential in &previous {
        insert_secret_audit(transaction, access, "secret_replaced", &credential.secret)
            .await
            .map_err(storage_error)?;
        insert_credential_audit(
            transaction,
            access,
            "provider_credential_replaced",
            credential,
        )
        .await
        .map_err(storage_error)?;
    }
    sqlx::query(
        "DELETE FROM secret_blobs WHERE id IN \
         (SELECT secret_blob_id FROM provider_account_credentials \
          WHERE provider_account_id = ?)",
    )
    .bind(provider_account_id.to_string())
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(previous.len())
}

async fn persist_prepared_credentials(
    transaction: &mut Transaction<'_, Sqlite>,
    prepared: &[PreparedCredential],
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    for prepared in prepared {
        let credential = &prepared.credential;
        insert_secret_blob(
            transaction,
            &credential.secret,
            &prepared.nonce,
            &prepared.encrypted_data,
        )
        .await?;
        insert_provider_credential(transaction, credential).await?;
        insert_secret_audit(transaction, access, "secret_stored", &credential.secret)
            .await
            .map_err(storage_error)?;
        insert_credential_audit(
            transaction,
            access,
            "provider_credential_created",
            credential,
        )
        .await
        .map_err(storage_error)?;
    }
    Ok(())
}

async fn authenticate_provider_account(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    provider_id: &ProviderId,
    updated_at: Timestamp,
) -> Result<(), SecretStoreError> {
    let previous_auth_state_json: Option<String> = sqlx::query_scalar(
        "SELECT auth_state_json FROM provider_accounts \
         WHERE id = ? AND owner_user_id = ? AND provider_id = ?",
    )
    .bind(provider_account_id.to_string())
    .bind(owner_user_id.to_string())
    .bind(provider_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    let Some(previous_auth_state_json) = previous_auth_state_json else {
        return Err(SecretStoreError::VersionConflict);
    };
    let previous_was_authenticated =
        account_auth_state_was_authenticated(&previous_auth_state_json)?;
    let result = sqlx::query(
        "UPDATE provider_accounts SET auth_state_json = ?, updated_at = ? \
         WHERE id = ? AND owner_user_id = ? AND provider_id = ?",
    )
    .bind(serde_json::to_string(&AuthState::Authenticated).map_err(|_| SecretStoreError::Storage)?)
    .bind(encode_timestamp(updated_at))
    .bind(provider_account_id.to_string())
    .bind(owner_user_id.to_string())
    .bind(provider_id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if result.rows_affected() != 1 {
        return Err(SecretStoreError::VersionConflict);
    }
    if !previous_was_authenticated {
        ensure_initial_answer_bootstrap_harvest(
            transaction,
            owner_user_id,
            provider_id,
            provider_account_id,
            updated_at,
        )
        .await
        .map_err(|_| SecretStoreError::Storage)?;
    }
    Ok(())
}

fn account_auth_state_was_authenticated(value: &str) -> Result<bool, SecretStoreError> {
    if let Ok(state) = serde_json::from_str::<AuthState>(value) {
        return Ok(state == AuthState::Authenticated);
    }
    let legacy = serde_json::from_str::<String>(value).map_err(|_| SecretStoreError::Storage)?;
    match legacy.as_str() {
        "authenticated" => Ok(true),
        "idle"
        | "starting"
        | "exchanging_credential"
        | "validating_credential"
        | "refreshing"
        | "expired"
        | "auth_failed"
        | "provider_unavailable"
        | "client_update_required"
        | "cancelled" => Ok(false),
        _ => Err(SecretStoreError::Storage),
    }
}

const CREDENTIAL_SELECT: &str = "SELECT c.provider_account_id, c.session_kind, c.acquired_via, c.expires_at, \
            c.created_at AS captured_at, c.updated_at AS credential_updated_at, \
            s.id AS secret_id, s.owner_user_id, s.purpose, s.key_id, s.version, \
            s.created_at AS secret_created_at, s.updated_at AS secret_updated_at \
     FROM provider_account_credentials c \
     JOIN secret_blobs s ON s.id = c.secret_blob_id \
     WHERE c.provider_account_id = ? ORDER BY c.credential_kind, c.secret_blob_id";

async fn ensure_account_owner(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
) -> Result<(), SecretStoreError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM provider_accounts WHERE id = ? AND owner_user_id = ?)",
    )
    .bind(provider_account_id.to_string())
    .bind(owner_user_id.to_string())
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if exists {
        Ok(())
    } else {
        Err(SecretStoreError::NotFound)
    }
}

async fn ensure_account_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
    provider_id: &ProviderId,
    tenant: Option<&str>,
) -> Result<(), SecretStoreError> {
    let row = sqlx::query(
        "SELECT provider_id, tenant FROM provider_accounts WHERE id = ? AND owner_user_id = ?",
    )
    .bind(provider_account_id.to_string())
    .bind(owner_user_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(SecretStoreError::NotFound)?;
    let stored_provider_id: &str = row.try_get("provider_id").map_err(storage_error)?;
    let stored_tenant: Option<&str> = row.try_get("tenant").map_err(storage_error)?;
    if stored_provider_id != provider_id.as_str() || stored_tenant != tenant {
        return Err(SecretStoreError::AccountMismatch);
    }
    Ok(())
}

pub(crate) async fn insert_secret_blob(
    transaction: &mut Transaction<'_, Sqlite>,
    secret: &SecretRef,
    nonce: &[u8],
    encrypted_data: &[u8],
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO secret_blobs \
         (id, owner_user_id, purpose, key_id, nonce, encrypted_data, version, created_at, \
          updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(secret.id.to_string())
    .bind(secret.owner_user_id.to_string())
    .bind(encode_purpose(secret.purpose))
    .bind(&secret.key_id)
    .bind(nonce)
    .bind(encrypted_data)
    .bind(i64::from(secret.version))
    .bind(encode_timestamp(secret.created_at))
    .bind(encode_timestamp(secret.updated_at))
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn insert_provider_credential(
    transaction: &mut Transaction<'_, Sqlite>,
    credential: &ProviderCredential,
) -> Result<(), SecretStoreError> {
    sqlx::query(
        "INSERT INTO provider_account_credentials \
         (provider_account_id, secret_blob_id, credential_kind, session_kind, acquired_via, \
          expires_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(credential.provider_account_id.to_string())
    .bind(credential.secret.id.to_string())
    .bind(encode_purpose(credential.secret.purpose))
    .bind(encode_session_kind(credential.session_kind))
    .bind(encode_acquisition(credential.acquired_via))
    .bind(credential.expires_at.map(encode_timestamp))
    .bind(encode_timestamp(credential.captured_at))
    .bind(encode_timestamp(credential.updated_at))
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_credential_write_error(&error))?;
    Ok(())
}

async fn fetch_provider_credential(
    transaction: &mut Transaction<'_, Sqlite>,
    provider_account_id: ProviderAccountId,
    secret_id: SecretId,
) -> Result<ProviderCredential, SecretStoreError> {
    let rows = sqlx::query(CREDENTIAL_SELECT)
        .bind(provider_account_id.to_string())
        .fetch_all(&mut **transaction)
        .await
        .map_err(storage_error)?;
    rows.iter()
        .map(decode_provider_credential)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|credential| credential.secret.id == secret_id)
        .ok_or(SecretStoreError::NotFound)
}

fn decode_provider_credential(row: &SqliteRow) -> Result<ProviderCredential, SecretStoreError> {
    let version = u32::try_from(row.try_get::<i64, _>("version").map_err(storage_error)?)
        .map_err(|_| SecretStoreError::Storage)?;
    let credential = ProviderCredential {
        provider_account_id: ProviderAccountId::from_str(
            row.try_get("provider_account_id").map_err(storage_error)?,
        )
        .map_err(|_| SecretStoreError::Storage)?,
        secret: SecretRef {
            id: SecretId::from_str(row.try_get("secret_id").map_err(storage_error)?)
                .map_err(|_| SecretStoreError::Storage)?,
            owner_user_id: UserId::from_str(row.try_get("owner_user_id").map_err(storage_error)?)
                .map_err(|_| SecretStoreError::Storage)?,
            purpose: decode_purpose(row.try_get("purpose").map_err(storage_error)?)?,
            version,
            key_id: row.try_get("key_id").map_err(storage_error)?,
            created_at: decode_timestamp(row.try_get("secret_created_at").map_err(storage_error)?)?,
            updated_at: decode_timestamp(row.try_get("secret_updated_at").map_err(storage_error)?)?,
        },
        session_kind: decode_session_kind(row.try_get("session_kind").map_err(storage_error)?)?,
        acquired_via: decode_acquisition(row.try_get("acquired_via").map_err(storage_error)?)?,
        captured_at: decode_timestamp(row.try_get("captured_at").map_err(storage_error)?)?,
        expires_at: row
            .try_get::<Option<&str>, _>("expires_at")
            .map_err(storage_error)?
            .map(decode_timestamp)
            .transpose()?,
        updated_at: decode_timestamp(
            row.try_get("credential_updated_at")
                .map_err(storage_error)?,
        )?,
    };
    credential
        .validate()
        .map_err(|_| SecretStoreError::Storage)?;
    Ok(credential)
}

async fn insert_credential_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    action: &str,
    credential: &ProviderCredential,
) -> Result<(), sqlx::Error> {
    let (actor_type, actor_id) = encode_secret_actor(&access.actor);
    let metadata = serde_json::json!({
        "secret_id": credential.secret.id,
        "purpose": encode_purpose(credential.secret.purpose),
        "session_kind": encode_session_kind(credential.session_kind),
        "acquired_via": encode_acquisition(credential.acquired_via),
        "expires_at": credential.expires_at,
        "reason": access.reason,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'provider_account', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(Utc::now()))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(credential.provider_account_id.to_string())
    .bind(&access.correlation_id)
    .bind(serde_json::to_string(&metadata).map_err(|error| sqlx::Error::Encode(Box::new(error)))?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_bundle_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    provider_account_id: ProviderAccountId,
    auth_method: AuthMethod,
    session_kind: SessionKind,
    replaced_count: usize,
    credential_count: usize,
) -> Result<(), sqlx::Error> {
    let (actor_type, actor_id) = encode_secret_actor(&access.actor);
    let metadata = serde_json::json!({
        "auth_method": auth_method,
        "session_kind": session_kind,
        "replaced_count": replaced_count,
        "credential_count": credential_count,
        "reason": access.reason,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'provider_credentials_committed', 'provider_account', ?, ?, \
                 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(Utc::now()))
    .bind(actor_type)
    .bind(actor_id)
    .bind(provider_account_id.to_string())
    .bind(&access.correlation_id)
    .bind(serde_json::to_string(&metadata).map_err(|error| sqlx::Error::Encode(Box::new(error)))?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn authorize_browser_bridge_secret_access(
    session: &BrowserBridgeSession,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    authorize(session.owner_user_id, access)?;
    let actor_matches = match &access.actor {
        SecretActor::CoreService(_) => true,
        SecretActor::ProviderRuntime(provider_id) => provider_id == session.provider_id.as_str(),
        SecretActor::User(_) | SecretActor::ServiceToken(_) => false,
    };
    actor_matches
        .then_some(())
        .ok_or(SecretStoreError::Unauthorized)
}

async fn insert_browser_bridge_session_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    session: &BrowserBridgeSession,
) -> Result<(), sqlx::Error> {
    let (actor_type, actor_id) = encode_secret_actor(&access.actor);
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, 'browser_bridge_session_completed', \
                 'browser_bridge_session', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(session.updated_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(session.id.to_string())
    .bind(&access.correlation_id)
    .bind(
        serde_json::json!({
            "state": session.state,
            "revision": session.revision,
            "provider_id": session.provider_id,
            "task_id": session.task_id,
        })
        .to_string(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn map_credential_write_error(error: &sqlx::Error) -> SecretStoreError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        SecretStoreError::VersionConflict
    } else {
        SecretStoreError::Storage
    }
}

pub(crate) struct StoredSecret {
    pub(crate) owner_user_id: UserId,
    pub(crate) purpose: SecretPurpose,
    pub(crate) key_id: String,
    pub(crate) nonce: Vec<u8>,
    pub(crate) encrypted_data: Vec<u8>,
    pub(crate) version: u32,
    pub(crate) created_at: Timestamp,
    pub(crate) updated_at: Timestamp,
}

pub(crate) async fn fetch_secret(
    transaction: &mut Transaction<'_, Sqlite>,
    secret_id: SecretId,
) -> Result<StoredSecret, SecretStoreError> {
    let row = sqlx::query(
        "SELECT owner_user_id, purpose, key_id, nonce, encrypted_data, version, created_at, \
                updated_at FROM secret_blobs WHERE id = ?",
    )
    .bind(secret_id.to_string())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(SecretStoreError::NotFound)?;
    decode_stored_secret(&row)
}

fn decode_stored_secret(row: &SqliteRow) -> Result<StoredSecret, SecretStoreError> {
    let version = u32::try_from(row.try_get::<i64, _>("version").map_err(storage_error)?)
        .map_err(|_| SecretStoreError::Storage)?;
    Ok(StoredSecret {
        owner_user_id: UserId::from_str(row.try_get("owner_user_id").map_err(storage_error)?)
            .map_err(|_| SecretStoreError::Storage)?,
        purpose: decode_purpose(row.try_get("purpose").map_err(storage_error)?)?,
        key_id: row.try_get("key_id").map_err(storage_error)?,
        nonce: row.try_get("nonce").map_err(storage_error)?,
        encrypted_data: row.try_get("encrypted_data").map_err(storage_error)?,
        version,
        created_at: decode_timestamp(row.try_get("created_at").map_err(storage_error)?)?,
        updated_at: decode_timestamp(row.try_get("updated_at").map_err(storage_error)?)?,
    })
}

fn verify_reference(secret: &SecretRef, row: &StoredSecret) -> Result<(), SecretStoreError> {
    if secret.owner_user_id != row.owner_user_id {
        return Err(SecretStoreError::Unauthorized);
    }
    if secret.purpose != row.purpose
        || secret.version != row.version
        || secret.key_id != row.key_id
        || secret.created_at != row.created_at
        || secret.updated_at != row.updated_at
    {
        return Err(SecretStoreError::VersionConflict);
    }
    Ok(())
}

pub(crate) fn encrypt(
    key: &SecretKey,
    secret: &SecretRef,
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), SecretStoreError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.expose_secret())
        .map_err(|_| SecretStoreError::KeyUnavailable)?;
    let nonce = XNonce::generate();
    let aad = associated_data(secret);
    let encrypted = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| SecretStoreError::Storage)?;
    Ok((nonce.to_vec(), encrypted))
}

pub(crate) fn decrypt(
    key: &SecretKey,
    secret: &SecretRef,
    nonce: &[u8],
    encrypted: &[u8],
) -> Result<Vec<u8>, SecretStoreError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.expose_secret())
        .map_err(|_| SecretStoreError::KeyUnavailable)?;
    let nonce = XNonce::try_from(nonce).map_err(|_| SecretStoreError::AuthenticationFailed)?;
    let aad = associated_data(secret);
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: encrypted,
                aad: &aad,
            },
        )
        .map_err(|_| SecretStoreError::AuthenticationFailed)
}

fn associated_data(secret: &SecretRef) -> Vec<u8> {
    format!(
        "asterism-secret-v1\0{}\0{}\0{}\0{}\0{}",
        secret.id,
        secret.owner_user_id,
        encode_purpose(secret.purpose),
        secret.version,
        secret.key_id,
    )
    .into_bytes()
}

pub(crate) async fn insert_secret_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    action: &str,
    secret: &SecretRef,
) -> Result<(), sqlx::Error> {
    let (actor_type, actor_id) = encode_secret_actor(&access.actor);
    let metadata = serde_json::json!({
        "purpose": encode_purpose(secret.purpose),
        "version": secret.version,
        "key_id": secret.key_id,
        "reason": access.reason,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'secret', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(Utc::now()))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(secret.id.to_string())
    .bind(&access.correlation_id)
    .bind(serde_json::to_string(&metadata).map_err(|error| sqlx::Error::Encode(Box::new(error)))?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn encode_secret_actor(actor: &SecretActor) -> (&'static str, String) {
    match actor {
        SecretActor::User(id) => ("user", id.to_string()),
        SecretActor::ServiceToken(id) => ("service_token", id.to_string()),
        SecretActor::CoreService(service) => ("core_service", (*service).to_owned()),
        SecretActor::ProviderRuntime(provider_id) => ("provider_runtime", provider_id.to_owned()),
    }
}

fn secret_audit_actor(actor: &SecretActor) -> Option<AuditActor> {
    match actor {
        SecretActor::User(id) => Some(AuditActor::User(*id)),
        SecretActor::ServiceToken(id) => Some(AuditActor::ServiceToken(*id)),
        SecretActor::CoreService(_) | SecretActor::ProviderRuntime(_) => None,
    }
}

pub(crate) fn authorize(
    owner_user_id: UserId,
    access: &SecretAccess,
) -> Result<(), SecretStoreError> {
    if access.authorizes(owner_user_id) {
        Ok(())
    } else {
        Err(SecretStoreError::Unauthorized)
    }
}

pub(crate) fn validate_secret(value: &SecretValue) -> Result<(), SecretStoreError> {
    let length = value.expose_secret().len();
    if length == 0 || length > MAX_SECRET_BYTES {
        Err(SecretStoreError::InvalidValue)
    } else {
        Ok(())
    }
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn encode_purpose(purpose: SecretPurpose) -> &'static str {
    match purpose {
        SecretPurpose::ProviderUsername => "provider_username",
        SecretPurpose::ProviderPassword => "provider_password",
        SecretPurpose::ProviderCookie => "provider_cookie",
        SecretPurpose::ProviderAccessToken => "provider_access_token",
        SecretPurpose::ProviderRefreshToken => "provider_refresh_token",
        SecretPurpose::ProviderCompositeSession => "provider_composite_session",
        SecretPurpose::WebSessionToken => "web_session_token",
        SecretPurpose::ServiceToken => "service_token",
        SecretPurpose::IntegrationCredential => "integration_credential",
        SecretPurpose::BrowserJobCredential => "browser_job_credential",
        SecretPurpose::ProviderExecutionState => "provider_execution_state",
    }
}

fn decode_purpose(value: &str) -> Result<SecretPurpose, SecretStoreError> {
    match value {
        "provider_username" => Ok(SecretPurpose::ProviderUsername),
        "provider_password" => Ok(SecretPurpose::ProviderPassword),
        "provider_cookie" => Ok(SecretPurpose::ProviderCookie),
        "provider_access_token" => Ok(SecretPurpose::ProviderAccessToken),
        "provider_refresh_token" => Ok(SecretPurpose::ProviderRefreshToken),
        "provider_composite_session" => Ok(SecretPurpose::ProviderCompositeSession),
        "web_session_token" => Ok(SecretPurpose::WebSessionToken),
        "service_token" => Ok(SecretPurpose::ServiceToken),
        "integration_credential" => Ok(SecretPurpose::IntegrationCredential),
        "browser_job_credential" => Ok(SecretPurpose::BrowserJobCredential),
        "provider_execution_state" => Ok(SecretPurpose::ProviderExecutionState),
        _ => Err(SecretStoreError::Storage),
    }
}

fn encode_session_kind(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Cookie => "cookie",
        SessionKind::BearerToken => "bearer_token",
        SessionKind::Jwt => "jwt",
        SessionKind::Composite => "composite",
        SessionKind::ProviderSpecific => "provider_specific",
    }
}

fn decode_session_kind(value: &str) -> Result<SessionKind, SecretStoreError> {
    match value {
        "cookie" => Ok(SessionKind::Cookie),
        "bearer_token" => Ok(SessionKind::BearerToken),
        "jwt" => Ok(SessionKind::Jwt),
        "composite" => Ok(SessionKind::Composite),
        "provider_specific" => Ok(SessionKind::ProviderSpecific),
        _ => Err(SecretStoreError::Storage),
    }
}

fn encode_acquisition(acquisition: CredentialAcquisition) -> &'static str {
    match acquisition {
        CredentialAcquisition::NativeProviderLogin => "native_provider_login",
        CredentialAcquisition::CaptureTool => "capture_tool",
        CredentialAcquisition::BrowserExtension => "browser_extension",
        CredentialAcquisition::AndroidHelper => "android_helper",
        CredentialAcquisition::ManualImport => "manual_import",
    }
}

fn decode_acquisition(value: &str) -> Result<CredentialAcquisition, SecretStoreError> {
    match value {
        "native_provider_login" => Ok(CredentialAcquisition::NativeProviderLogin),
        "capture_tool" => Ok(CredentialAcquisition::CaptureTool),
        "browser_extension" => Ok(CredentialAcquisition::BrowserExtension),
        "android_helper" => Ok(CredentialAcquisition::AndroidHelper),
        "manual_import" => Ok(CredentialAcquisition::ManualImport),
        _ => Err(SecretStoreError::Storage),
    }
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
    use asterism_auth::OpaqueTokenService;
    use asterism_domain::{
        AuditActor, AuthBootstrapPurpose, AuthBootstrapSession, AuthBootstrapState, AuthState,
        ProviderAccount, ProviderId, Role,
    };
    use asterism_secrets::CredentialField;

    use super::*;
    use crate::{
        AnswerBootstrapHarvestRepository, AuthBootstrapSessionRepository,
        ProviderAccountRepository, SqliteAnswerBootstrapHarvestRepository,
        SqliteAuthBootstrapSessionRepository, SqliteProviderAccountRepository,
    };

    #[tokio::test]
    async fn encrypted_secret_lifecycle_is_versioned_authorized_and_audited() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let access = user_access(owner_id, "secret-lifecycle");
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 7)])),
        );

        let secret = store
            .put(
                owner_id,
                SecretPurpose::IntegrationCredential,
                SecretValue::new(b"initial-secret".to_vec()),
                &access,
            )
            .await
            .unwrap();
        assert_eq!(secret.version, 1);
        let (nonce, encrypted): (Vec<u8>, Vec<u8>) =
            sqlx::query_as("SELECT nonce, encrypted_data FROM secret_blobs WHERE id = ?")
                .bind(secret.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(nonce.len(), 24);
        assert_ne!(encrypted, b"initial-secret");
        assert_eq!(
            store.get(&secret, &access).await.unwrap().expose_secret(),
            b"initial-secret"
        );
        assert!(matches!(
            store
                .get(&secret, &user_access(UserId::new(), "denied"))
                .await,
            Err(SecretStoreError::Unauthorized)
        ));

        let rotating_store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-b", &[("key-a", 7), ("key-b", 9)])),
        );
        let rotated = rotating_store
            .rotate(
                &secret,
                SecretValue::new(b"rotated-secret".to_vec()),
                &access,
            )
            .await
            .unwrap();
        assert_eq!(rotated.version, 2);
        assert_eq!(rotated.key_id, "key-b");
        assert!(matches!(
            rotating_store.get(&secret, &access).await,
            Err(SecretStoreError::VersionConflict)
        ));
        assert_eq!(
            rotating_store
                .get(&rotated, &access)
                .await
                .unwrap()
                .expose_secret(),
            b"rotated-secret"
        );
        rotating_store.delete(&rotated, &access).await.unwrap();
        assert!(matches!(
            rotating_store.get(&rotated, &access).await,
            Err(SecretStoreError::NotFound)
        ));

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('secret_stored', 'secret_accessed', 'secret_rotated', \
                            'secret_deleted')",
        )
        .bind(secret.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 5);
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND (metadata_sanitized_json LIKE '%initial-secret%' \
                  OR metadata_sanitized_json LIKE '%rotated-secret%')",
        )
        .bind(secret.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(leaked, 0);
    }

    #[tokio::test]
    async fn ciphertext_tampering_fails_authentication() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let access = user_access(owner_id, "tamper-test");
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 11)])),
        );
        let secret = store
            .put(
                owner_id,
                SecretPurpose::IntegrationCredential,
                SecretValue::new(b"cookie-value".to_vec()),
                &access,
            )
            .await
            .unwrap();
        let mut encrypted: Vec<u8> =
            sqlx::query_scalar("SELECT encrypted_data FROM secret_blobs WHERE id = ?")
                .bind(secret.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        encrypted[0] ^= 1;
        sqlx::query("UPDATE secret_blobs SET encrypted_data = ? WHERE id = ?")
            .bind(encrypted)
            .bind(secret.id.to_string())
            .execute(database.pool())
            .await
            .unwrap();

        assert!(matches!(
            store.get(&secret, &access).await,
            Err(SecretStoreError::AuthenticationFailed)
        ));
    }

    #[tokio::test]
    async fn provider_credential_lifecycle_is_transactional_and_versioned() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let account_id = insert_provider_account(&database, owner_id).await;
        let access = user_access(owner_id, "credential-lifecycle");
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 17)])),
        );
        let expires_at = Utc::now() + chrono::Duration::hours(1);
        let credential = store
            .create_provider_credential(
                owner_id,
                new_cookie_credential(account_id, b"cookie-a", expires_at),
                &access,
            )
            .await
            .unwrap();
        assert_eq!(credential.secret.version, 1);
        assert_eq!(
            store
                .get(&credential.secret, &access)
                .await
                .unwrap()
                .expose_secret(),
            b"cookie-a"
        );
        assert_eq!(
            SqliteProviderAccountRepository::new(database.clone())
                .find_provider_account(owner_id, account_id)
                .await
                .unwrap()
                .unwrap()
                .credential_refs,
            [credential.secret.id]
        );

        assert!(matches!(
            store
                .create_provider_credential(
                    owner_id,
                    new_cookie_credential(account_id, b"duplicate", expires_at),
                    &access,
                )
                .await,
            Err(SecretStoreError::VersionConflict)
        ));
        let blob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret_blobs")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(blob_count, 1);

        let listed = store
            .list_provider_credentials(owner_id, account_id, &access)
            .await
            .unwrap();
        assert_eq!(listed.as_slice(), std::slice::from_ref(&credential));
        let rotated = store
            .rotate_provider_credential(
                owner_id,
                &credential,
                SecretValue::new(b"cookie-b".to_vec()),
                Some(expires_at + chrono::Duration::hours(1)),
                &access,
            )
            .await
            .unwrap();
        assert_eq!(rotated.secret.version, 2);
        assert!(matches!(
            store
                .rotate_provider_credential(
                    owner_id,
                    &credential,
                    SecretValue::new(b"stale".to_vec()),
                    None,
                    &access,
                )
                .await,
            Err(SecretStoreError::VersionConflict)
        ));
        store
            .delete_provider_credential(owner_id, account_id, rotated.secret.id, &access)
            .await
            .unwrap();
        assert!(
            store
                .list_provider_credentials(owner_id, account_id, &access)
                .await
                .unwrap()
                .is_empty()
        );

        let credential_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('provider_credential_created', 'provider_credential_rotated', \
                            'provider_credential_deleted')",
        )
        .bind(account_id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(credential_audits, 3);
    }

    #[tokio::test]
    async fn runtime_resolution_requires_exact_authenticated_provider_binding() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let account_id = insert_provider_account(&database, owner_id).await;
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 23)])),
        );
        let captured_at = Utc::now();
        let credentials = store
            .replace_provider_credentials(
                owner_id,
                account_id,
                runtime_password_bundle(captured_at, b"_uid=123; token=abc"),
                &user_access(owner_id, "runtime-resolution-setup"),
            )
            .await
            .unwrap();
        let credential_refs = credentials
            .iter()
            .map(|credential| credential.secret.id)
            .collect::<Vec<_>>();
        let resolver = SqliteProviderCredentialResolver::new(
            store.clone(),
            ProviderId::new("provider-alpha").unwrap(),
        );
        let resolved_credentials = resolver
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: account_id,
                credential_refs: credential_refs.clone(),
                purposes: vec![SecretPurpose::ProviderCookie],
                correlation_id: "scheduled-scan:runtime-resolution".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(resolved_credentials.len(), 1);
        assert!(resolved_credentials.iter().any(|item| {
            item.credential.secret.purpose == SecretPurpose::ProviderCookie
                && item.value.expose_secret() == b"_uid=123; token=abc"
        }));
        assert_runtime_credentials_are_redacted(&resolved_credentials);

        let wrong_provider = SqliteProviderCredentialResolver::new(
            store.clone(),
            ProviderId::new("provider-beta").unwrap(),
        );
        assert!(matches!(
            wrong_provider
                .resolve_provider_credentials(ProviderCredentialResolution {
                    provider_account_id: account_id,
                    credential_refs: credential_refs.clone(),
                    purposes: vec![SecretPurpose::ProviderCookie],
                    correlation_id: "scheduled-scan:wrong-provider".to_owned(),
                })
                .await,
            Err(SecretStoreError::AccountMismatch)
        ));
        assert!(matches!(
            resolver
                .resolve_provider_credentials(ProviderCredentialResolution {
                    provider_account_id: account_id,
                    credential_refs: credential_refs[..2].to_vec(),
                    purposes: vec![SecretPurpose::ProviderCookie],
                    correlation_id: "scheduled-scan:incomplete-binding".to_owned(),
                })
                .await,
            Err(SecretStoreError::AccountMismatch)
        ));

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE action = 'provider_credential_resolved' \
             AND correlation_id = 'scheduled-scan:runtime-resolution'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 1);
    }

    #[tokio::test]
    async fn runtime_renewal_is_atomic_and_rejects_stale_metadata() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let account_id = insert_provider_account(&database, owner_id).await;
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 29)])),
        );
        let old = store
            .replace_provider_credentials(
                owner_id,
                account_id,
                runtime_password_bundle(Utc::now(), b"_uid=OLD; token=old"),
                &user_access(owner_id, "runtime-renewal-setup"),
            )
            .await
            .unwrap();
        let old_refs = old
            .iter()
            .map(|credential| credential.secret.id)
            .collect::<Vec<_>>();
        let runtime = SqliteProviderCredentialResolver::new(
            store.clone(),
            ProviderId::new("provider-alpha").unwrap(),
        );
        let renewed = runtime
            .renew_provider_credentials(ProviderCredentialRenewal {
                provider_account_id: account_id,
                expected_credentials: old.clone(),
                bundle: runtime_password_bundle(Utc::now(), b"_uid=NEW; token=new"),
                correlation_id: "scheduled-scan:runtime-renewal".to_owned(),
            })
            .await
            .unwrap();
        let renewed_refs = renewed
            .iter()
            .map(|credential| credential.secret.id)
            .collect::<Vec<_>>();
        assert_ne!(renewed_refs, old_refs);
        assert!(matches!(
            store
                .get(
                    &old.iter()
                        .find(|credential| {
                            credential.secret.purpose == SecretPurpose::ProviderCookie
                        })
                        .unwrap()
                        .secret,
                    &user_access(owner_id, "old-cookie-read"),
                )
                .await,
            Err(SecretStoreError::NotFound)
        ));
        let resolved = runtime
            .resolve_provider_credentials(ProviderCredentialResolution {
                provider_account_id: account_id,
                credential_refs: renewed_refs,
                purposes: vec![SecretPurpose::ProviderCookie],
                correlation_id: "scheduled-scan:renewed-cookie".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(resolved[0].value.expose_secret(), b"_uid=NEW; token=new");

        let renewed_password = renewed
            .iter()
            .find(|credential| credential.secret.purpose == SecretPurpose::ProviderPassword)
            .unwrap();
        store
            .rotate_provider_credential(
                owner_id,
                renewed_password,
                SecretValue::new(b"password-rotated".to_vec()),
                None,
                &user_access(owner_id, "rotate-before-stale-renewal"),
            )
            .await
            .unwrap();
        assert!(matches!(
            runtime
                .renew_provider_credentials(ProviderCredentialRenewal {
                    provider_account_id: account_id,
                    expected_credentials: renewed,
                    bundle: runtime_password_bundle(Utc::now(), b"_uid=STALE; token=stale"),
                    correlation_id: "scheduled-scan:stale-renewal".to_owned(),
                })
                .await,
            Err(SecretStoreError::VersionConflict)
        ));
    }

    fn assert_runtime_credentials_are_redacted(credentials: &[ResolvedProviderCredential]) {
        let debug = format!("{credentials:?}");
        for secret in ["student-a", "password-a", "token=abc"] {
            assert!(!debug.contains(secret));
        }
    }

    fn runtime_password_bundle(captured_at: Timestamp, cookie: &[u8]) -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            tenant: None,
            auth_method: AuthMethod::Password,
            acquired_via: CredentialAcquisition::NativeProviderLogin,
            captured_at,
            expires_at: None,
            session_kind: SessionKind::Composite,
            fields: vec![
                CredentialField {
                    purpose: SecretPurpose::ProviderUsername,
                    value: SecretValue::new(b"student-a".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderPassword,
                    value: SecretValue::new(b"password-a".to_vec()),
                },
                CredentialField {
                    purpose: SecretPurpose::ProviderCookie,
                    value: SecretValue::new(cookie.to_vec()),
                },
            ],
            user_id_hint: None,
        }
    }

    #[tokio::test]
    async fn credential_bundle_replaces_the_full_set_and_authenticates_atomically() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let account_id = insert_provider_account(&database, owner_id).await;
        let access = user_access(owner_id, "credential-bundle-replace");
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 19)])),
        );
        let old = store
            .create_provider_credential(
                owner_id,
                new_cookie_credential(
                    account_id,
                    b"old-cookie",
                    Utc::now() + chrono::Duration::hours(1),
                ),
                &access,
            )
            .await
            .unwrap();
        let captured_at = Utc::now();
        let committed = store
            .replace_provider_credentials(
                owner_id,
                account_id,
                CredentialBundle {
                    provider_id: ProviderId::new("provider-alpha").unwrap(),
                    tenant: None,
                    auth_method: AuthMethod::ImportedToken,
                    acquired_via: CredentialAcquisition::ManualImport,
                    captured_at,
                    expires_at: Some(captured_at + chrono::Duration::hours(2)),
                    session_kind: SessionKind::Composite,
                    fields: vec![
                        CredentialField {
                            purpose: SecretPurpose::ProviderAccessToken,
                            value: SecretValue::new(b"new-access".to_vec()),
                        },
                        CredentialField {
                            purpose: SecretPurpose::ProviderRefreshToken,
                            value: SecretValue::new(b"new-refresh".to_vec()),
                        },
                    ],
                    user_id_hint: None,
                },
                &access,
            )
            .await
            .unwrap();

        assert_eq!(committed.len(), 2);
        assert!(matches!(
            store.get(&old.secret, &access).await,
            Err(SecretStoreError::NotFound)
        ));
        let account = SqliteProviderAccountRepository::new(database.clone())
            .find_provider_account(owner_id, account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account.auth_state, AuthState::Authenticated);
        assert_eq!(account.credential_refs.len(), 2);
        assert_initial_harvest(&database, owner_id, &account, account.updated_at).await;
        let refreshed = store
            .replace_provider_credentials(
                owner_id,
                account_id,
                runtime_password_bundle(Utc::now(), b"refreshed-cookie"),
                &access,
            )
            .await
            .unwrap();
        assert_eq!(refreshed.len(), 3);
        assert_eq!(table_count(&database, "answer_bootstrap_harvests").await, 1);
        assert_eq!(table_count(&database, "scheduled_jobs").await, 1);
        let committed_audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action = 'provider_credentials_committed'",
        )
        .bind(account_id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(committed_audits, 2);
    }

    #[tokio::test]
    async fn bootstrap_commit_creates_account_and_credentials_only_with_live_access() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let now = Utc::now();
        let mut session = AuthBootstrapSession::awaiting_claim(
            owner_id,
            ProviderId::new("provider-alpha").unwrap(),
            None,
            AuthBootstrapPurpose::AddAccount,
            3,
            now,
            now + chrono::Duration::minutes(10),
        )
        .unwrap();
        let pairing_tokens = OpaqueTokenService::new("ast_pair").unwrap();
        let access_tokens = OpaqueTokenService::new("ast_boot").unwrap();
        let (_, pairing_digest) = pairing_tokens.generate();
        let (_, access_digest) = access_tokens.generate();
        let (_, wrong_digest) = access_tokens.generate();
        let repository = SqliteAuthBootstrapSessionRepository::new(database.clone());
        repository
            .create_auth_bootstrap_session(
                &session,
                &pairing_digest,
                AuditActor::User(owner_id),
                "bootstrap-secret-create",
            )
            .await
            .unwrap();
        session = repository
            .claim_auth_bootstrap_session(
                session.id,
                &pairing_digest,
                &access_digest,
                now + chrono::Duration::seconds(1),
                "bootstrap-secret-claim",
            )
            .await
            .unwrap()
            .unwrap();
        let completed_at = now + chrono::Duration::seconds(2);
        let account = new_bootstrap_account(owner_id, completed_at);
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 31)])),
        );
        let denied_access = core_access("bootstrap-secret-denied");
        let denied = store
            .commit_auth_bootstrap_credentials(AuthBootstrapCredentialCommitRequest {
                session_id: session.id,
                access_token_digest: &wrong_digest,
                validated_account: account.clone(),
                bundle: bootstrap_bundle(now, b"denied-cookie"),
                completed_at,
                access: &denied_access,
            })
            .await
            .unwrap();
        assert_eq!(denied, AuthBootstrapCredentialCommitOutcome::AccessRejected);
        assert_eq!(table_count(&database, "provider_accounts").await, 0);
        assert_eq!(table_count(&database, "secret_blobs").await, 0);
        assert_eq!(table_count(&database, "answer_bootstrap_harvests").await, 0);
        assert_eq!(table_count(&database, "scheduled_jobs").await, 0);

        let access = core_access("bootstrap-secret-commit");
        let committed = store
            .commit_auth_bootstrap_credentials(AuthBootstrapCredentialCommitRequest {
                session_id: session.id,
                access_token_digest: &access_digest,
                validated_account: account.clone(),
                bundle: bootstrap_bundle(now, b"captured-cookie"),
                completed_at,
                access: &access,
            })
            .await
            .unwrap();
        let AuthBootstrapCredentialCommitOutcome::Committed(committed) = committed else {
            panic!("live Bootstrap access must commit")
        };
        assert_eq!(committed.session.state, AuthBootstrapState::Completed);
        assert_eq!(committed.session.provider_account_id, Some(account.id));
        assert_eq!(committed.account.auth_state, AuthState::Authenticated);
        assert_eq!(committed.credentials.len(), 1);
        assert_eq!(
            store
                .get(&committed.credentials[0].secret, &access)
                .await
                .unwrap()
                .expose_secret(),
            b"captured-cookie"
        );
        assert!(
            repository
                .authenticate_auth_bootstrap_access(session.id, &access_digest, completed_at)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(table_count(&database, "provider_accounts").await, 1);
        assert_eq!(table_count(&database, "secret_blobs").await, 1);
        assert_initial_harvest(&database, owner_id, &account, completed_at).await;
    }

    #[tokio::test]
    async fn bootstrap_commit_replaces_bound_account_credentials_and_seals_access() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let account_id = insert_provider_account(&database, owner_id).await;
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 37)])),
        );
        let old_access = user_access(owner_id, "bootstrap-old-credential");
        let old = store
            .create_provider_credential(
                owner_id,
                new_cookie_credential(
                    account_id,
                    b"old-bootstrap-cookie",
                    Utc::now() + chrono::Duration::hours(1),
                ),
                &old_access,
            )
            .await
            .unwrap();
        assert_eq!(table_count(&database, "answer_bootstrap_harvests").await, 0);
        assert_eq!(table_count(&database, "scheduled_jobs").await, 0);
        let account = SqliteProviderAccountRepository::new(database.clone())
            .find_provider_account(owner_id, account_id)
            .await
            .unwrap()
            .unwrap();
        let now = Utc::now();
        let (session, access_digest) = claimed_bootstrap_session(
            &database,
            owner_id,
            AuthBootstrapPurpose::Reauthenticate,
            Some(account_id),
            now,
            "bootstrap-replace",
        )
        .await;
        let access = core_access("bootstrap-replace-commit");
        let mut stale_account = account.clone();
        stale_account.display_name = "stale-snapshot".to_owned();
        let conflicted = store
            .commit_auth_bootstrap_credentials(AuthBootstrapCredentialCommitRequest {
                session_id: session.id,
                access_token_digest: &access_digest,
                validated_account: stale_account,
                bundle: bootstrap_bundle(now, b"conflicted-bootstrap-cookie"),
                completed_at: now + chrono::Duration::seconds(2),
                access: &access,
            })
            .await
            .unwrap();
        assert_eq!(
            conflicted,
            AuthBootstrapCredentialCommitOutcome::BindingConflict
        );
        assert_eq!(
            store
                .get(&old.secret, &old_access)
                .await
                .unwrap()
                .expose_secret(),
            b"old-bootstrap-cookie"
        );
        let outcome = store
            .commit_auth_bootstrap_credentials(AuthBootstrapCredentialCommitRequest {
                session_id: session.id,
                access_token_digest: &access_digest,
                validated_account: account,
                bundle: bootstrap_bundle(now, b"replacement-bootstrap-cookie"),
                completed_at: now + chrono::Duration::seconds(2),
                access: &access,
            })
            .await
            .unwrap();
        let AuthBootstrapCredentialCommitOutcome::Committed(committed) = outcome else {
            panic!("bound Bootstrap access must replace credentials")
        };
        assert_eq!(committed.account.id, account_id);
        assert_eq!(committed.session.state, AuthBootstrapState::Completed);
        assert!(matches!(
            store.get(&old.secret, &access).await,
            Err(SecretStoreError::NotFound)
        ));
        assert_eq!(table_count(&database, "provider_accounts").await, 1);
        assert_eq!(table_count(&database, "secret_blobs").await, 1);
        assert_eq!(table_count(&database, "answer_bootstrap_harvests").await, 1);
        assert_eq!(table_count(&database, "scheduled_jobs").await, 1);
        assert!(
            SqliteAuthBootstrapSessionRepository::new(database)
                .authenticate_auth_bootstrap_access(session.id, &access_digest, Utc::now())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn deleting_provider_account_removes_attached_secret_blob() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = insert_user(&database).await;
        let account_id = insert_provider_account(&database, owner_id).await;
        let access = user_access(owner_id, "account-delete");
        let store = SqliteSecretStore::new(
            database.clone(),
            Arc::new(keyring("key-a", &[("key-a", 23)])),
        );
        let credential = store
            .create_provider_credential(
                owner_id,
                new_cookie_credential(
                    account_id,
                    b"delete-with-account",
                    Utc::now() + chrono::Duration::hours(1),
                ),
                &access,
            )
            .await
            .unwrap();

        assert!(
            SqliteProviderAccountRepository::new(database.clone())
                .delete_provider_account(
                    owner_id,
                    account_id,
                    Utc::now(),
                    AuditActor::User(owner_id),
                )
                .await
                .unwrap()
        );
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM secret_blobs WHERE id = ?)")
                .bind(credential.secret.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(!exists);
    }

    #[test]
    fn keyring_rejects_missing_active_or_unsafe_key_ids() {
        assert!(matches!(
            SecretKeyring::new("missing", BTreeMap::new()),
            Err(SecretKeyringError::ActiveKeyMissing)
        ));
        assert!(matches!(
            SecretKeyring::new(
                "bad key",
                BTreeMap::from([("bad key".to_owned(), SecretKey::new([1; 32]),)])
            ),
            Err(SecretKeyringError::InvalidKeyId)
        ));
    }

    fn keyring(active: &str, keys: &[(&str, u8)]) -> SecretKeyring {
        SecretKeyring::new(
            active,
            keys.iter()
                .map(|(key_id, byte)| (key_id.to_string(), SecretKey::new([*byte; 32])))
                .collect(),
        )
        .unwrap()
    }

    fn user_access(user_id: UserId, correlation_id: &str) -> SecretAccess {
        SecretAccess {
            actor: SecretActor::User(user_id),
            correlation_id: correlation_id.to_owned(),
            reason: "provider credential lifecycle".to_owned(),
        }
    }

    fn core_access(correlation_id: &str) -> SecretAccess {
        SecretAccess {
            actor: SecretActor::CoreService("auth-bootstrap"),
            correlation_id: correlation_id.to_owned(),
            reason: "complete Capture credential submission".to_owned(),
        }
    }

    fn new_bootstrap_account(owner_id: UserId, at: Timestamp) -> ProviderAccount {
        ProviderAccount {
            id: ProviderAccountId::new(),
            owner_id,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Idle,
            network_profile_id: None,
            credential_refs: Vec::new(),
            created_at: at,
            updated_at: at,
        }
    }

    fn bootstrap_bundle(captured_at: Timestamp, value: &[u8]) -> CredentialBundle {
        CredentialBundle {
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            tenant: None,
            auth_method: AuthMethod::ImportedCookie,
            acquired_via: CredentialAcquisition::CaptureTool,
            captured_at,
            expires_at: Some(captured_at + chrono::Duration::hours(1)),
            session_kind: SessionKind::Cookie,
            fields: vec![CredentialField {
                purpose: SecretPurpose::ProviderCookie,
                value: SecretValue::new(value.to_vec()),
            }],
            user_id_hint: None,
        }
    }

    async fn assert_initial_harvest(
        database: &Database,
        owner_id: UserId,
        account: &ProviderAccount,
        created_at: Timestamp,
    ) {
        assert_eq!(table_count(database, "answer_bootstrap_harvests").await, 1);
        assert_eq!(table_count(database, "scheduled_jobs").await, 1);
        let repository = SqliteAnswerBootstrapHarvestRepository::new(database.clone());
        let harvest = repository
            .find_owned_answer_bootstrap_harvest(owner_id, account.id, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            harvest.state,
            asterism_domain::AnswerBootstrapHarvestState::Pending
        );
        assert_eq!(harvest.provider_id, account.provider_id);
        assert_eq!(harvest.created_at, created_at);
        assert!(
            repository
                .find_owned_answer_bootstrap_harvest(UserId::new(), account.id, 1)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn table_count(database: &Database, table: &str) -> i64 {
        let query = format!("SELECT COUNT(*) FROM {table}");
        sqlx::query_scalar(&query)
            .fetch_one(database.pool())
            .await
            .unwrap()
    }

    async fn claimed_bootstrap_session(
        database: &Database,
        owner_id: UserId,
        purpose: AuthBootstrapPurpose,
        provider_account_id: Option<ProviderAccountId>,
        now: Timestamp,
        correlation_prefix: &str,
    ) -> (AuthBootstrapSession, asterism_auth::TokenDigest) {
        let session = AuthBootstrapSession::awaiting_claim(
            owner_id,
            ProviderId::new("provider-alpha").unwrap(),
            provider_account_id,
            purpose,
            3,
            now,
            now + chrono::Duration::minutes(10),
        )
        .unwrap();
        let (_, pairing_digest) = OpaqueTokenService::new("ast_pair").unwrap().generate();
        let (_, access_digest) = OpaqueTokenService::new("ast_boot").unwrap().generate();
        let repository = SqliteAuthBootstrapSessionRepository::new(database.clone());
        repository
            .create_auth_bootstrap_session(
                &session,
                &pairing_digest,
                AuditActor::User(owner_id),
                &format!("{correlation_prefix}-create"),
            )
            .await
            .unwrap();
        let session = repository
            .claim_auth_bootstrap_session(
                session.id,
                &pairing_digest,
                &access_digest,
                now + chrono::Duration::seconds(1),
                &format!("{correlation_prefix}-claim"),
            )
            .await
            .unwrap()
            .unwrap();
        (session, access_digest)
    }

    fn new_cookie_credential(
        provider_account_id: ProviderAccountId,
        value: &[u8],
        expires_at: Timestamp,
    ) -> NewProviderCredential {
        NewProviderCredential {
            provider_account_id,
            purpose: SecretPurpose::ProviderCookie,
            session_kind: SessionKind::Cookie,
            acquired_via: CredentialAcquisition::ManualImport,
            expires_at: Some(expires_at),
            value: SecretValue::new(value.to_vec()),
        }
    }

    async fn insert_user(database: &Database) -> UserId {
        let user_id = UserId::new();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'secret-owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        user_id
    }

    async fn insert_provider_account(
        database: &Database,
        owner_user_id: UserId,
    ) -> ProviderAccountId {
        let account_id = ProviderAccountId::new();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, \
              updated_at) VALUES (?, ?, ?, 'primary', ?, ?, ?)",
        )
        .bind(account_id.to_string())
        .bind(owner_user_id.to_string())
        .bind(ProviderId::new("provider-alpha").unwrap().as_str())
        .bind(serde_json::to_string(&AuthState::Idle).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        account_id
    }
}
