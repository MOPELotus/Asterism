use std::{str::FromStr, sync::Arc};

use asterism_domain::{BrowserBridgeExchangeState, ProviderId, SecretId};
use asterism_secrets::{
    SecretAccess, SecretActor, SecretPurpose, SecretRef, SecretStoreError, SecretValue,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{
    BrowserBridgeCommandArtifactRepository, BrowserBridgeCommandIssueRequest,
    BrowserBridgeCommandResolveRequest, BrowserBridgeExchangeRecord, Database,
    ResolvedBrowserBridgeCommand, SecretKeyring,
    browser_bridge::{
        binding_is_valid, fetch_exchange, fetch_session, find_claimed_session_for_exchange,
        insert_exchange_audit,
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
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(if same && artifact_present == 1 {
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
        }))
    }
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
    value.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn storage_error<E>(_error: E) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asterism_auth::OpaqueTokenService;
    use asterism_domain::{
        AuditActor, AuthMethod, AuthState, BrowserBridgeExchange, BrowserBridgeSession,
        BrowserBridgeSessionCreate, BrowserBridgeSessionState, ProviderAccountId, Role,
        SessionKind, TaskId, UserId,
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
        BrowserBridgeCredentialRepository, BrowserBridgeSessionRepository,
        SqliteBrowserBridgeSessionRepository, SqliteSecretStore,
    };

    #[tokio::test]
    async fn command_is_encrypted_recoverable_bound_and_legacy_safe() {
        let fixture = fixture().await;
        let now = Utc::now();
        let (session, _) = fixture.claimed_session(now).await;
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
        let inserted = fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                access: &access,
            })
            .await
            .unwrap();
        assert!(matches!(inserted, BrowserBridgeExchangeRecord::Inserted(_)));
        let duplicate = fixture
            .command_repository
            .issue_browser_bridge_command(BrowserBridgeCommandIssueRequest {
                exchange: &issued,
                command_artifact: SecretValue::new(command.to_vec()),
                access: &access,
            })
            .await
            .unwrap();
        assert!(matches!(
            duplicate,
            BrowserBridgeExchangeRecord::Duplicate(_)
        ));

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

        let resolved = fixture
            .command_repository
            .resolve_browser_bridge_command(fixture.resolve_request(&session, 1, &access))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.exchange, issued);
        assert_eq!(resolved.command_artifact.expose_secret(), command);

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
    }

    #[tokio::test]
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
                access: &access,
            })
            .await
            .unwrap();
        let completed_at = now + Duration::seconds(3);
        let mut completed = issued.clone();
        completed
            .complete(
                "cidaren.capture.snapshot.result".to_owned(),
                [8; 32],
                completed_at,
            )
            .unwrap();
        let outcome = fixture
            .secret_store
            .commit_browser_bridge_credentials(BrowserBridgeCredentialCommitRequest {
                exchange: &completed,
                access_token_digest: &access_digest,
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

        let retry = fixture
            .secret_store
            .commit_browser_bridge_credentials(BrowserBridgeCredentialCommitRequest {
                exchange: &completed,
                access_token_digest: &access_digest,
                validated_bundle: fixture.bundle(completed_at, b"captured-token"),
                access: &access,
            })
            .await
            .unwrap();
        assert_eq!(retry, BrowserBridgeCredentialCommitOutcome::AccessRejected);
    }

    #[tokio::test]
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
                access: &access,
            })
            .await
            .unwrap();
        let completed_at = now + Duration::seconds(3);
        let mut foreign = issued.clone();
        foreign.command_digest = [7; 32];
        foreign
            .complete(
                "cidaren.capture.snapshot.result".to_owned(),
                [8; 32],
                completed_at,
            )
            .unwrap();
        let outcome = fixture
            .secret_store
            .commit_browser_bridge_credentials(BrowserBridgeCredentialCommitRequest {
                exchange: &foreign,
                access_token_digest: &access_digest,
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
                isolation_key: "cidaren-task".to_owned(),
                allowed_origins: vec!["https://www.cidaren.com".to_owned()],
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
