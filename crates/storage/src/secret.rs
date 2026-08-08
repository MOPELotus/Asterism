use std::{collections::BTreeMap, fmt, str::FromStr, sync::Arc};

use asterism_domain::{AuditRecordId, SecretId, Timestamp, UserId};
use asterism_secrets::{
    SecretAccess, SecretActor, SecretKey, SecretPurpose, SecretRef, SecretStore, SecretStoreError,
    SecretValue,
};
use async_trait::async_trait;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::Database;

const MAX_SECRET_BYTES: usize = 1024 * 1024;
const MAX_ACCESS_REASON_BYTES: usize = 256;
const MAX_CORRELATION_ID_BYTES: usize = 128;

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

    fn active(&self) -> (&str, &SecretKey) {
        (
            &self.active_key_id,
            self.keys
                .get(&self.active_key_id)
                .expect("active key was validated at construction"),
        )
    }

    fn get(&self, key_id: &str) -> Result<&SecretKey, SecretStoreError> {
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

impl SqliteSecretStore {
    pub fn new(database: Database, keyring: Arc<SecretKeyring>) -> Self {
        Self { database, keyring }
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

struct StoredSecret {
    owner_user_id: UserId,
    purpose: SecretPurpose,
    key_id: String,
    nonce: Vec<u8>,
    encrypted_data: Vec<u8>,
    version: u32,
    created_at: Timestamp,
    updated_at: Timestamp,
}

async fn fetch_secret(
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

fn encrypt(
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

fn decrypt(
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

async fn insert_secret_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    access: &SecretAccess,
    action: &str,
    secret: &SecretRef,
) -> Result<(), sqlx::Error> {
    let (actor_type, actor_id) = match &access.actor {
        SecretActor::User(id) => ("user", id.to_string()),
        SecretActor::CoreService(service) => ("core_service", (*service).to_owned()),
        SecretActor::ProviderRuntime(provider_id) => ("provider_runtime", provider_id.to_owned()),
    };
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

fn authorize(owner_user_id: UserId, access: &SecretAccess) -> Result<(), SecretStoreError> {
    let actor_valid = match &access.actor {
        SecretActor::User(user_id) => *user_id == owner_user_id,
        SecretActor::CoreService(service) => valid_actor_label(service),
        SecretActor::ProviderRuntime(provider_id) => valid_actor_label(provider_id),
    };
    let context_valid = !access.correlation_id.is_empty()
        && access.correlation_id.len() <= MAX_CORRELATION_ID_BYTES
        && !access.correlation_id.chars().any(char::is_control)
        && !access.reason.is_empty()
        && access.reason.len() <= MAX_ACCESS_REASON_BYTES
        && !access.reason.chars().any(char::is_control);
    if actor_valid && context_valid {
        Ok(())
    } else {
        Err(SecretStoreError::Unauthorized)
    }
}

fn validate_secret(value: &SecretValue) -> Result<(), SecretStoreError> {
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

fn valid_actor_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn encode_purpose(purpose: SecretPurpose) -> &'static str {
    match purpose {
        SecretPurpose::ProviderPassword => "provider_password",
        SecretPurpose::ProviderCookie => "provider_cookie",
        SecretPurpose::ProviderAccessToken => "provider_access_token",
        SecretPurpose::ProviderRefreshToken => "provider_refresh_token",
        SecretPurpose::ProviderCompositeSession => "provider_composite_session",
        SecretPurpose::WebSessionToken => "web_session_token",
        SecretPurpose::ServiceToken => "service_token",
        SecretPurpose::IntegrationCredential => "integration_credential",
        SecretPurpose::BrowserJobCredential => "browser_job_credential",
    }
}

fn decode_purpose(value: &str) -> Result<SecretPurpose, SecretStoreError> {
    match value {
        "provider_password" => Ok(SecretPurpose::ProviderPassword),
        "provider_cookie" => Ok(SecretPurpose::ProviderCookie),
        "provider_access_token" => Ok(SecretPurpose::ProviderAccessToken),
        "provider_refresh_token" => Ok(SecretPurpose::ProviderRefreshToken),
        "provider_composite_session" => Ok(SecretPurpose::ProviderCompositeSession),
        "web_session_token" => Ok(SecretPurpose::WebSessionToken),
        "service_token" => Ok(SecretPurpose::ServiceToken),
        "integration_credential" => Ok(SecretPurpose::IntegrationCredential),
        "browser_job_credential" => Ok(SecretPurpose::BrowserJobCredential),
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
    use asterism_domain::Role;

    use super::*;

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
                SecretPurpose::ProviderAccessToken,
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
                SecretPurpose::ProviderCookie,
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
}
