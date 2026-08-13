use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecordId, AuthSession, AuthSessionId, AuthState, ProviderAccountId, Timestamp,
    UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{AuthSessionRepository, Database, StorageError};

const AUTH_SESSION_SELECT: &str = "SELECT id, owner_user_id, provider_account_id, method_json, state_json, revision, \
            expires_at, created_at, updated_at FROM auth_sessions";

#[derive(Clone, Debug)]
pub struct SqliteAuthSessionRepository {
    database: Database,
}

impl SqliteAuthSessionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl AuthSessionRepository for SqliteAuthSessionRepository {
    async fn create_auth_session(
        &self,
        session: &AuthSession,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError> {
        validate_initial_session(session, correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        ensure_owned_account(
            &mut transaction,
            session.owner_user_id,
            session.provider_account_id,
        )
        .await?;
        sqlx::query(
            "INSERT INTO auth_sessions \
             (id, owner_user_id, provider_account_id, method_json, state_json, revision, \
              expires_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session.id.to_string())
        .bind(session.owner_user_id.to_string())
        .bind(session.provider_account_id.to_string())
        .bind(serde_json::to_string(&session.method)?)
        .bind(serde_json::to_string(&session.state)?)
        .bind(i64::from(session.revision))
        .bind(encode_timestamp(session.expires_at))
        .bind(encode_timestamp(session.created_at))
        .bind(encode_timestamp(session.updated_at))
        .execute(&mut *transaction)
        .await?;
        mirror_account_state(&mut transaction, session).await?;
        insert_auth_session_audit(
            &mut transaction,
            actor,
            "auth_session_created",
            correlation_id,
            session,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn find_auth_session(
        &self,
        owner_user_id: UserId,
        session_id: AuthSessionId,
    ) -> Result<Option<AuthSession>, StorageError> {
        let query = format!("{AUTH_SESSION_SELECT} WHERE id = ? AND owner_user_id = ?");
        sqlx::query(&query)
            .bind(session_id.to_string())
            .bind(owner_user_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_auth_session)
            .transpose()
    }

    async fn find_latest_account_auth_session(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
    ) -> Result<Option<AuthSession>, StorageError> {
        let query = format!(
            "{AUTH_SESSION_SELECT} WHERE owner_user_id = ? AND provider_account_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        );
        sqlx::query(&query)
            .bind(owner_user_id.to_string())
            .bind(provider_account_id.to_string())
            .fetch_optional(self.database.pool())
            .await?
            .as_ref()
            .map(decode_auth_session)
            .transpose()
    }

    async fn update_auth_session(
        &self,
        session: &AuthSession,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError> {
        validate_updated_session(session, expected_revision, correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        if !update_auth_session_in_transaction(
            &mut transaction,
            session,
            expected_revision,
            actor,
            correlation_id,
        )
        .await?
        {
            transaction.rollback().await?;
            return Ok(false);
        }
        mirror_account_state(&mut transaction, session).await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn create_external_oauth_pending(
        &self,
        pending: &asterism_domain::ExternalOauthPending,
        waiting_session: &AuthSession,
        expected_auth_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<(), StorageError> {
        crate::external_oauth::create_pending(
            &self.database,
            pending,
            waiting_session,
            expected_auth_revision,
            actor,
            correlation_id,
        )
        .await
    }

    async fn find_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
    ) -> Result<Option<asterism_domain::ExternalOauthPending>, StorageError> {
        crate::external_oauth::find_pending(
            &self.database,
            owner_user_id,
            provider_account_id,
            auth_session_id,
        )
        .await
    }

    async fn find_external_oauth_pending_by_state(
        &self,
        owner_user_id: UserId,
        provider_id: &asterism_domain::ProviderId,
        state_digest: [u8; 32],
    ) -> Result<Option<asterism_domain::ExternalOauthPending>, StorageError> {
        crate::external_oauth::find_pending_by_state(
            &self.database,
            owner_user_id,
            provider_id,
            state_digest,
        )
        .await
    }

    async fn claim_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
        at: Timestamp,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<Option<crate::ExternalOauthClaim>, StorageError> {
        crate::external_oauth::claim_pending(
            &self.database,
            owner_user_id,
            provider_account_id,
            auth_session_id,
            at,
            actor,
            correlation_id,
        )
        .await
    }

    async fn recover_external_oauth_pending(
        &self,
        owner_user_id: UserId,
        provider_account_id: ProviderAccountId,
        auth_session_id: AuthSessionId,
        at: Timestamp,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<Option<crate::ExternalOauthClaim>, StorageError> {
        crate::external_oauth::recover_pending(
            &self.database,
            owner_user_id,
            provider_account_id,
            auth_session_id,
            at,
            actor,
            correlation_id,
        )
        .await
    }

    async fn update_external_oauth_pending(
        &self,
        pending: &asterism_domain::ExternalOauthPending,
        expected_revision: u32,
        actor: AuditActor,
        correlation_id: &str,
    ) -> Result<bool, StorageError> {
        crate::external_oauth::update_pending(
            &self.database,
            pending,
            expected_revision,
            actor,
            correlation_id,
        )
        .await
    }
}

pub(crate) async fn update_auth_session_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &AuthSession,
    expected_revision: u32,
    actor: AuditActor,
    correlation_id: &str,
) -> Result<bool, StorageError> {
    validate_updated_session(session, expected_revision, correlation_id)?;
    let Some(current) = fetch_auth_session(transaction, session.id).await? else {
        return Ok(false);
    };
    if current.owner_user_id != session.owner_user_id
        || current.revision != expected_revision
        || !is_latest_account_session(transaction, &current).await?
    {
        return Ok(false);
    }
    let mut expected = current;
    expected
        .transition(session.state.clone(), session.updated_at)
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if &expected != session {
        return Err(StorageError::InvalidData(
            "authentication session changed immutable fields".to_owned(),
        ));
    }
    let result = sqlx::query(
        "UPDATE auth_sessions SET state_json = ?, revision = ?, updated_at = ? \
         WHERE id = ? AND owner_user_id = ? AND provider_account_id = ? \
           AND method_json = ? AND expires_at = ? AND created_at = ? AND revision = ?",
    )
    .bind(serde_json::to_string(&session.state)?)
    .bind(i64::from(session.revision))
    .bind(encode_timestamp(session.updated_at))
    .bind(session.id.to_string())
    .bind(session.owner_user_id.to_string())
    .bind(session.provider_account_id.to_string())
    .bind(serde_json::to_string(&session.method)?)
    .bind(encode_timestamp(session.expires_at))
    .bind(encode_timestamp(session.created_at))
    .bind(i64::from(expected_revision))
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    insert_auth_session_audit(
        transaction,
        actor,
        "auth_session_transitioned",
        correlation_id,
        session,
    )
    .await?;
    Ok(true)
}

fn validate_initial_session(
    session: &AuthSession,
    correlation_id: &str,
) -> Result<(), StorageError> {
    validate_session(session, correlation_id)?;
    if session.revision != 1
        || !matches!(session.state, AuthState::Starting)
        || session.created_at != session.updated_at
    {
        return Err(StorageError::InvalidData(
            "new authentication session is not in its initial state".to_owned(),
        ));
    }
    Ok(())
}

fn validate_updated_session(
    session: &AuthSession,
    expected_revision: u32,
    correlation_id: &str,
) -> Result<(), StorageError> {
    validate_session(session, correlation_id)?;
    let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
        StorageError::InvalidData("authentication session revision is exhausted".to_owned())
    })?;
    if expected_revision == 0 || session.revision != next_revision {
        return Err(StorageError::InvalidData(
            "authentication session revision is invalid".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) async fn fetch_auth_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session_id: AuthSessionId,
) -> Result<Option<AuthSession>, StorageError> {
    let query = format!("{AUTH_SESSION_SELECT} WHERE id = ?");
    sqlx::query(&query)
        .bind(session_id.to_string())
        .fetch_optional(&mut **transaction)
        .await?
        .as_ref()
        .map(decode_auth_session)
        .transpose()
}

async fn is_latest_account_session(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &AuthSession,
) -> Result<bool, StorageError> {
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT id FROM auth_sessions WHERE owner_user_id = ? AND provider_account_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(session.owner_user_id.to_string())
    .bind(session.provider_account_id.to_string())
    .fetch_optional(&mut **transaction)
    .await?;
    let session_id = session.id.to_string();
    Ok(latest.as_deref() == Some(session_id.as_str()))
}

fn validate_session(session: &AuthSession, correlation_id: &str) -> Result<(), StorageError> {
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    if correlation_id.is_empty()
        || correlation_id.len() > 128
        || correlation_id.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidData(
            "authentication correlation ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

async fn ensure_owned_account(
    transaction: &mut Transaction<'_, Sqlite>,
    owner_user_id: UserId,
    provider_account_id: ProviderAccountId,
) -> Result<(), StorageError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM provider_accounts WHERE id = ? AND owner_user_id = ?)",
    )
    .bind(provider_account_id.to_string())
    .bind(owner_user_id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    if exists {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "authentication session account is not owned by its user".to_owned(),
        ))
    }
}

pub(crate) async fn mirror_account_state(
    transaction: &mut Transaction<'_, Sqlite>,
    session: &AuthSession,
) -> Result<(), StorageError> {
    let result = sqlx::query(
        "UPDATE provider_accounts SET auth_state_json = ?, updated_at = ? \
         WHERE id = ? AND owner_user_id = ?",
    )
    .bind(serde_json::to_string(&session.state)?)
    .bind(encode_timestamp(session.updated_at))
    .bind(session.provider_account_id.to_string())
    .bind(session.owner_user_id.to_string())
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(StorageError::InvalidData(
            "authentication session account disappeared".to_owned(),
        ))
    }
}

async fn insert_auth_session_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: AuditActor,
    action: &str,
    correlation_id: &str,
    session: &AuthSession,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let metadata = serde_json::json!({
        "provider_account_id": session.provider_account_id,
        "method": session.method,
        "state": session.state,
        "revision": session.revision,
        "expires_at": session.expires_at,
    });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'auth_session', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(Utc::now()))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(session.id.to_string())
    .bind(correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn decode_auth_session(row: &SqliteRow) -> Result<AuthSession, StorageError> {
    let revision = u32::try_from(row.try_get::<i64, _>("revision")?)
        .map_err(|_| StorageError::InvalidData("invalid auth session revision".to_owned()))?;
    let session = AuthSession {
        id: AuthSessionId::from_str(row.try_get("id")?)
            .map_err(|_| StorageError::InvalidData("invalid auth session ID".to_owned()))?,
        owner_user_id: UserId::from_str(row.try_get("owner_user_id")?)
            .map_err(|_| StorageError::InvalidData("invalid auth session owner".to_owned()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|_| StorageError::InvalidData("invalid auth session account".to_owned()))?,
        method: serde_json::from_str(row.try_get("method_json")?)?,
        state: serde_json::from_str(row.try_get("state_json")?)?,
        revision,
        expires_at: decode_timestamp(row.try_get("expires_at")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    };
    session
        .validate()
        .map_err(|error| StorageError::InvalidData(error.to_string()))?;
    Ok(session)
}

pub(crate) fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

pub(crate) fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| StorageError::InvalidData("invalid auth session timestamp".to_owned()))
}

#[cfg(test)]
mod tests {
    use asterism_domain::{
        AuthMethod, ExternalOauthPending, ExternalOauthPendingCreate, ExternalOauthState,
        ProviderId, Role, WaitingUserState,
    };
    use chrono::Duration;

    use super::*;

    #[tokio::test]
    async fn auth_session_updates_are_owner_scoped_revisioned_and_mirrored() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = insert_user(&database).await;
        let account = insert_account(&database, owner).await;
        let repository = SqliteAuthSessionRepository::new(database.clone());
        let now = Utc::now();
        let mut session = AuthSession::starting(
            owner,
            account,
            AuthMethod::ImportedCookie,
            now,
            now + Duration::minutes(10),
        )
        .unwrap();
        repository
            .create_auth_session(&session, AuditActor::User(owner), "auth-session-test")
            .await
            .unwrap();
        session
            .transition(
                AuthState::WaitingUser(WaitingUserState::SessionImport),
                now + Duration::seconds(1),
            )
            .unwrap();
        assert!(
            repository
                .update_auth_session(&session, 1, AuditActor::User(owner), "auth-session-test",)
                .await
                .unwrap()
        );
        assert!(
            !repository
                .update_auth_session(&session, 1, AuditActor::User(owner), "auth-session-stale",)
                .await
                .unwrap()
        );
        let mut invalid = session.clone();
        invalid.state = AuthState::Authenticated;
        invalid.revision = 3;
        invalid.updated_at = now + Duration::seconds(2);
        assert!(matches!(
            repository
                .update_auth_session(&invalid, 2, AuditActor::User(owner), "auth-session-invalid",)
                .await,
            Err(StorageError::InvalidData(_))
        ));
        assert_eq!(
            repository
                .find_auth_session(owner, session.id)
                .await
                .unwrap(),
            Some(session.clone())
        );
        assert!(
            repository
                .find_auth_session(UserId::new(), session.id)
                .await
                .unwrap()
                .is_none()
        );
        let account_state: String =
            sqlx::query_scalar("SELECT auth_state_json FROM provider_accounts WHERE id = ?")
                .bind(account.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<AuthState>(&account_state).unwrap(),
            session.state
        );
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('auth_session_created', 'auth_session_transitioned')",
        )
        .bind(session.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 2);
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the storage integration test exercises hash-only creation, one-shot claim and post-commit crash recovery as one lifecycle"
    )]
    async fn external_oauth_callback_is_hash_only_owner_bound_and_claimed_once() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = insert_user(&database).await;
        let account = insert_account(&database, owner).await;
        let repository = SqliteAuthSessionRepository::new(database.clone());
        let now = Utc::now();
        let mut session = AuthSession::starting(
            owner,
            account,
            AuthMethod::ExternalBrowserOauth,
            now,
            now + Duration::minutes(5),
        )
        .unwrap();
        repository
            .create_auth_session(&session, AuditActor::User(owner), "oauth-create")
            .await
            .unwrap();
        session
            .transition(
                AuthState::WaitingUser(WaitingUserState::BrowserCallback),
                now + Duration::seconds(1),
            )
            .unwrap();
        let pending = ExternalOauthPending::pending(ExternalOauthPendingCreate {
            auth_session_id: session.id,
            owner_user_id: owner,
            provider_account_id: account,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            state_digest: [1; 32],
            provider_context_digest: [2; 32],
            created_at: now,
            expires_at: now + Duration::minutes(5),
        })
        .unwrap();
        repository
            .create_external_oauth_pending(
                &pending,
                &session,
                1,
                AuditActor::User(owner),
                "oauth-create",
            )
            .await
            .unwrap();

        let stored = repository
            .find_external_oauth_pending(owner, account, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, pending);
        assert!(
            repository
                .find_external_oauth_pending(UserId::new(), account, session.id)
                .await
                .unwrap()
                .is_none()
        );
        let claim = repository
            .claim_external_oauth_pending(
                owner,
                account,
                session.id,
                now + Duration::seconds(2),
                AuditActor::User(owner),
                "oauth-claim",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.pending.state, ExternalOauthState::Completing);
        assert_eq!(claim.auth_session.state, AuthState::ExchangingCredential);
        assert!(
            repository
                .claim_external_oauth_pending(
                    owner,
                    account,
                    session.id,
                    now + Duration::seconds(3),
                    AuditActor::User(owner),
                    "oauth-replay",
                )
                .await
                .unwrap()
                .is_none()
        );

        let digest_bytes: Vec<u8> = sqlx::query_scalar(
            "SELECT state_digest FROM external_oauth_pending WHERE auth_session_id = ?",
        )
        .bind(session.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(digest_bytes, vec![1; 32]);

        let mut authenticated = claim.auth_session;
        authenticated
            .transition(AuthState::ValidatingCredential, now + Duration::seconds(3))
            .unwrap();
        assert!(
            repository
                .update_auth_session(
                    &authenticated,
                    3,
                    AuditActor::User(owner),
                    "oauth-validating",
                )
                .await
                .unwrap()
        );
        authenticated
            .transition(AuthState::Authenticated, now + Duration::seconds(4))
            .unwrap();
        assert!(
            repository
                .update_auth_session(
                    &authenticated,
                    4,
                    AuditActor::User(owner),
                    "oauth-authenticated",
                )
                .await
                .unwrap()
        );
        let recovered = repository
            .recover_external_oauth_pending(
                owner,
                account,
                session.id,
                now + Duration::seconds(5),
                AuditActor::User(owner),
                "oauth-recover-success",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.pending.state, ExternalOauthState::Succeeded);
        assert_eq!(recovered.auth_session.state, AuthState::Authenticated);
    }

    #[tokio::test]
    async fn stale_external_oauth_exchange_recovers_ambiguously_without_replay() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = insert_user(&database).await;
        let account = insert_account(&database, owner).await;
        let repository = SqliteAuthSessionRepository::new(database);
        let now = Utc::now();
        let mut session = AuthSession::starting(
            owner,
            account,
            AuthMethod::ExternalBrowserOauth,
            now,
            now + Duration::minutes(5),
        )
        .unwrap();
        repository
            .create_auth_session(&session, AuditActor::User(owner), "oauth-stale-create")
            .await
            .unwrap();
        session
            .transition(
                AuthState::WaitingUser(WaitingUserState::BrowserCallback),
                now + Duration::seconds(1),
            )
            .unwrap();
        let pending = ExternalOauthPending::pending(ExternalOauthPendingCreate {
            auth_session_id: session.id,
            owner_user_id: owner,
            provider_account_id: account,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            state_digest: [3; 32],
            provider_context_digest: [4; 32],
            created_at: now,
            expires_at: now + Duration::minutes(5),
        })
        .unwrap();
        repository
            .create_external_oauth_pending(
                &pending,
                &session,
                1,
                AuditActor::User(owner),
                "oauth-stale-create",
            )
            .await
            .unwrap();
        repository
            .claim_external_oauth_pending(
                owner,
                account,
                session.id,
                now + Duration::seconds(2),
                AuditActor::User(owner),
                "oauth-stale-claim",
            )
            .await
            .unwrap()
            .unwrap();

        let recovered = repository
            .recover_external_oauth_pending(
                owner,
                account,
                session.id,
                now + Duration::seconds(123),
                AuditActor::User(owner),
                "oauth-stale-recover",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.pending.state, ExternalOauthState::Ambiguous);
        assert_eq!(recovered.auth_session.state, AuthState::ProviderUnavailable);
    }

    async fn insert_user(database: &Database) -> UserId {
        let user_id = UserId::new();
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'auth-owner', '$argon2id$test', 'active', ?, '[]', ?, ?)",
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

    async fn insert_account(database: &Database, owner_user_id: UserId) -> ProviderAccountId {
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
