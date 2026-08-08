use std::str::FromStr;

use asterism_auth::TokenDigest;
use asterism_domain::{
    AuditActor, AuditRecordId, ServiceScope, ServiceToken, ServiceTokenId, Timestamp, UserStatus,
    WebSession, WebSessionId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction};

use crate::{Database, SessionRepository, SqliteUserRepository, StorageError, UserRepository};

#[derive(Clone, Debug)]
pub struct SqliteSessionRepository {
    database: Database,
}

impl SqliteSessionRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl SessionRepository for SqliteSessionRepository {
    async fn create_web_session(
        &self,
        session: &WebSession,
        token_digest: &TokenDigest,
        actor: AuditActor,
    ) -> Result<(), StorageError> {
        if session.revoked_at.is_some()
            || session.expires_at <= session.created_at
            || session.last_used_at.is_some_and(|last_used_at| {
                last_used_at < session.created_at || last_used_at >= session.expires_at
            })
        {
            return Err(StorageError::InvalidData(
                "new web session must be active with a future expiry".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query(
            "INSERT INTO web_sessions \
             (id, user_id, token_hash, created_at, expires_at, revoked_at, last_used_at) \
             VALUES (?, ?, ?, ?, ?, NULL, ?)",
        )
        .bind(session.id.to_string())
        .bind(session.user_id.to_string())
        .bind(token_digest.as_bytes().as_slice())
        .bind(encode_timestamp(session.created_at))
        .bind(encode_timestamp(session.expires_at))
        .bind(session.last_used_at.map(encode_timestamp))
        .execute(&mut *transaction)
        .await?;
        insert_auth_audit(
            &mut transaction,
            actor,
            "web_session_created",
            "web_session",
            session.id.to_string(),
            session.created_at,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn authenticate_web_session(
        &self,
        token_digest: &TokenDigest,
        now: Timestamp,
    ) -> Result<Option<(WebSession, asterism_domain::User)>, StorageError> {
        let now_text = encode_timestamp(now);
        let row = sqlx::query(
            "UPDATE web_sessions SET last_used_at = ? \
             WHERE token_hash = ? AND revoked_at IS NULL AND expires_at > ? \
               AND EXISTS (SELECT 1 FROM users \
                           WHERE users.id = web_sessions.user_id AND users.status = 'active') \
             RETURNING id, user_id, created_at, expires_at, revoked_at, last_used_at",
        )
        .bind(&now_text)
        .bind(token_digest.as_bytes().as_slice())
        .bind(&now_text)
        .fetch_optional(self.database.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let session = decode_web_session(&row)?;
        let user = SqliteUserRepository::new(self.database.clone())
            .find_user(session.user_id)
            .await?
            .filter(|user| user.status == UserStatus::Active);
        let Some(user) = user else {
            return Ok(None);
        };
        Ok(Some((session, user)))
    }

    async fn revoke_web_session(
        &self,
        session_id: WebSessionId,
        at: Timestamp,
        actor: AuditActor,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let result = sqlx::query(
            "UPDATE web_sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(encode_timestamp(at))
        .bind(session_id.to_string())
        .execute(&mut *transaction)
        .await?;
        let revoked = result.rows_affected() == 1;
        if revoked {
            insert_auth_audit(
                &mut transaction,
                actor,
                "web_session_revoked",
                "web_session",
                session_id.to_string(),
                at,
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(revoked)
    }

    async fn create_service_token(
        &self,
        token: &ServiceToken,
        token_digest: &TokenDigest,
        actor: AuditActor,
    ) -> Result<(), StorageError> {
        if token.name.is_empty()
            || token.name.len() > 128
            || token.name.trim() != token.name
            || token.scopes.is_empty()
            || token.revoked_at.is_some()
            || token
                .expires_at
                .is_some_and(|expires_at| expires_at <= token.created_at)
            || token.last_used_at.is_some_and(|last_used_at| {
                last_used_at < token.created_at
                    || token
                        .expires_at
                        .is_some_and(|expires_at| last_used_at >= expires_at)
            })
        {
            return Err(StorageError::InvalidData(
                "new service token must have a name, scopes, and valid expiry".to_owned(),
            ));
        }
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query(
            "INSERT INTO service_tokens \
             (id, owner_user_id, name, token_hash, scopes_json, created_at, expires_at, revoked_at, last_used_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?)",
        )
        .bind(token.id.to_string())
        .bind(token.owner_user_id.map(|id| id.to_string()))
        .bind(&token.name)
        .bind(token_digest.as_bytes().as_slice())
        .bind(serde_json::to_string(&token.scopes)?)
        .bind(encode_timestamp(token.created_at))
        .bind(token.expires_at.map(encode_timestamp))
        .bind(token.last_used_at.map(encode_timestamp))
        .execute(&mut *transaction)
        .await?;
        insert_auth_audit(
            &mut transaction,
            actor,
            "service_token_created",
            "service_token",
            token.id.to_string(),
            token.created_at,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn authenticate_service_token(
        &self,
        token_digest: &TokenDigest,
        now: Timestamp,
    ) -> Result<Option<ServiceToken>, StorageError> {
        let now_text = encode_timestamp(now);
        let row = sqlx::query(
            "UPDATE service_tokens SET last_used_at = ? \
             WHERE token_hash = ? AND revoked_at IS NULL \
               AND (expires_at IS NULL OR expires_at > ?) \
               AND (owner_user_id IS NULL OR EXISTS (SELECT 1 FROM users \
                    WHERE users.id = service_tokens.owner_user_id AND users.status = 'active')) \
             RETURNING id, owner_user_id, name, scopes_json, created_at, expires_at, \
                       revoked_at, last_used_at",
        )
        .bind(&now_text)
        .bind(token_digest.as_bytes().as_slice())
        .bind(&now_text)
        .fetch_optional(self.database.pool())
        .await?;
        row.map(|row| decode_service_token(&row)).transpose()
    }

    async fn revoke_service_token(
        &self,
        token_id: ServiceTokenId,
        at: Timestamp,
        actor: AuditActor,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        let result = sqlx::query(
            "UPDATE service_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(encode_timestamp(at))
        .bind(token_id.to_string())
        .execute(&mut *transaction)
        .await?;
        let revoked = result.rows_affected() == 1;
        if revoked {
            insert_auth_audit(
                &mut transaction,
                actor,
                "service_token_revoked",
                "service_token",
                token_id.to_string(),
                at,
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(revoked)
    }
}

async fn insert_auth_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: AuditActor,
    action: &str,
    resource_type: &str,
    resource_id: String,
    occurred_at: Timestamp,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'succeeded', '{}')",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(occurred_at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn decode_web_session(row: &sqlx::sqlite::SqliteRow) -> Result<WebSession, StorageError> {
    Ok(WebSession {
        id: parse_id(row.try_get("id")?)?,
        user_id: parse_id(row.try_get("user_id")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        expires_at: decode_timestamp(row.try_get("expires_at")?)?,
        revoked_at: row
            .try_get::<Option<&str>, _>("revoked_at")?
            .map(decode_timestamp)
            .transpose()?,
        last_used_at: row
            .try_get::<Option<&str>, _>("last_used_at")?
            .map(decode_timestamp)
            .transpose()?,
    })
}

fn decode_service_token(row: &sqlx::sqlite::SqliteRow) -> Result<ServiceToken, StorageError> {
    Ok(ServiceToken {
        id: parse_id(row.try_get("id")?)?,
        owner_user_id: row
            .try_get::<Option<&str>, _>("owner_user_id")?
            .map(parse_id)
            .transpose()?,
        name: row.try_get("name")?,
        scopes: serde_json::from_str::<std::collections::BTreeSet<ServiceScope>>(
            row.try_get("scopes_json")?,
        )?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        expires_at: row
            .try_get::<Option<&str>, _>("expires_at")?
            .map(decode_timestamp)
            .transpose()?,
        revoked_at: row
            .try_get::<Option<&str>, _>("revoked_at")?
            .map(decode_timestamp)
            .transpose()?,
        last_used_at: row
            .try_get::<Option<&str>, _>("last_used_at")?
            .map(decode_timestamp)
            .transpose()?,
    })
}

fn parse_id<T>(value: &str) -> Result<T, StorageError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error: T::Err| StorageError::InvalidData(error.to_string()))
}

fn encode_timestamp(value: Timestamp) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use asterism_auth::OpaqueTokenService;
    use asterism_domain::{Permission, Role, User, UserId};
    use chrono::{Duration, Utc};

    use super::*;

    async fn active_user(database: &Database) -> User {
        let now = Utc::now();
        let user = User {
            id: UserId::new(),
            username: "session-user".to_owned(),
            password_hash: "$argon2id$test".to_owned(),
            status: UserStatus::Active,
            roles: vec![Role::User],
            permissions: vec![Permission::ReadProviders],
            created_at: now,
            updated_at: now,
        };
        SqliteUserRepository::new(database.clone())
            .save_user(&user)
            .await
            .unwrap();
        user
    }

    #[tokio::test]
    async fn web_session_checks_expiry_and_revocation() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let user = active_user(&database).await;
        let repository = SqliteSessionRepository::new(database.clone());
        let tokens = OpaqueTokenService::new("ast_ws").unwrap();
        let (_plaintext, digest) = tokens.generate();
        let now = Utc::now();
        let session = WebSession {
            id: WebSessionId::new(),
            user_id: user.id,
            created_at: now,
            expires_at: now + Duration::hours(1),
            revoked_at: None,
            last_used_at: None,
        };
        repository
            .create_web_session(&session, &digest, AuditActor::User(user.id))
            .await
            .unwrap();
        assert!(
            repository
                .authenticate_web_session(&digest, now)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            repository
                .authenticate_web_session(&digest, now + Duration::hours(2))
                .await
                .unwrap()
                .is_none()
        );
        repository
            .revoke_web_session(session.id, now, AuditActor::User(user.id))
            .await
            .unwrap();
        assert!(
            repository
                .authenticate_web_session(&digest, now)
                .await
                .unwrap()
                .is_none()
        );
        let digest_length: i64 =
            sqlx::query_scalar("SELECT length(token_hash) FROM web_sessions WHERE id = ?")
                .bind(session.id.to_string())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(digest_length, 32);
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('web_session_created', 'web_session_revoked')",
        )
        .bind(session.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 2);
    }

    #[tokio::test]
    async fn service_token_round_trips_scopes_and_expires() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let user = active_user(&database).await;
        let repository = SqliteSessionRepository::new(database.clone());
        let tokens = OpaqueTokenService::new("ast_st").unwrap();
        let (_plaintext, digest) = tokens.generate();
        let now = Utc::now();
        let token = ServiceToken {
            id: ServiceTokenId::new(),
            owner_user_id: Some(user.id),
            name: "CLI".to_owned(),
            scopes: BTreeSet::from([ServiceScope::ProviderRead]),
            created_at: now,
            expires_at: Some(now + Duration::hours(1)),
            revoked_at: None,
            last_used_at: None,
        };
        repository
            .create_service_token(&token, &digest, AuditActor::User(user.id))
            .await
            .unwrap();
        let authenticated = repository
            .authenticate_service_token(&digest, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(authenticated.scopes, token.scopes);
        assert!(
            repository
                .authenticate_service_token(&digest, now + Duration::hours(2))
                .await
                .unwrap()
                .is_none()
        );
        repository
            .revoke_service_token(token.id, now, AuditActor::User(user.id))
            .await
            .unwrap();
        assert!(
            repository
                .authenticate_service_token(&digest, now)
                .await
                .unwrap()
                .is_none()
        );
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records WHERE resource_id = ? \
             AND action IN ('service_token_created', 'service_token_revoked')",
        )
        .bind(token.id.to_string())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(audit_count, 2);
    }
}
