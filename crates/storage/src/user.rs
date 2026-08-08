use std::str::FromStr;

use asterism_domain::{AuditRecordId, Role, Timestamp, User, UserId, UserStatus};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{Database, StorageError, UserRepository};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitialMaster {
    pub id: UserId,
    pub username: String,
    pub password_hash: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct SqliteUserRepository {
    database: Database,
}

impl SqliteUserRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    /// Atomically creates the one initial Master and consumes bootstrap
    /// privilege. This method never accepts a plaintext password.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::MasterAlreadyInitialized`] after the first
    /// successful call, or another [`StorageError`] if persistence fails.
    pub async fn bootstrap_master(&self, input: &InitialMaster) -> Result<User, StorageError> {
        if !input.password_hash.starts_with("$argon2id$") {
            return Err(StorageError::InvalidData(
                "initial master password must be an Argon2id PHC hash".to_owned(),
            ));
        }
        let timestamp = encode_timestamp(input.created_at);
        let mut transaction = self.database.pool().begin().await?;
        let consumed = sqlx::query(
            "INSERT INTO system_settings (key, value_json, updated_at) \
             VALUES ('master_initialized', 'true', ?) \
             ON CONFLICT(key) DO NOTHING",
        )
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await?;
        if consumed.rows_affected() != 1 {
            return Err(StorageError::MasterAlreadyInitialized);
        }
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, ?, 'active', '[\"master\"]', '[]', ?, ?)",
        )
        .bind(input.id.to_string())
        .bind(&input.username)
        .bind(&input.password_hash)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES (?, 'master')")
            .bind(input.id.to_string())
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, 0, 0, ?)",
        )
        .bind(input.id.to_string())
        .bind(&timestamp)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO audit_records \
             (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, outcome, metadata_sanitized_json) \
             VALUES (?, ?, 'system', NULL, 'bootstrap_master', 'user', ?, 'succeeded', '{}')",
        )
        .bind(AuditRecordId::new().to_string())
        .bind(&timestamp)
        .bind(input.id.to_string())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(User {
            id: input.id,
            username: input.username.clone(),
            password_hash: input.password_hash.clone(),
            status: UserStatus::Active,
            roles: vec![Role::Master],
            permissions: Vec::new(),
            created_at: input.created_at,
            updated_at: input.created_at,
        })
    }

    /// Reports whether the one-time Master bootstrap has been consumed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when system settings cannot be queried.
    pub async fn master_initialized(&self) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM system_settings WHERE key = 'master_initialized')",
        )
        .fetch_one(self.database.pool())
        .await?)
    }
}

#[async_trait]
impl UserRepository for SqliteUserRepository {
    async fn find_user(&self, id: UserId) -> Result<Option<User>, StorageError> {
        let row = sqlx::query(
            "SELECT id, username, password_hash, status, roles_json, permissions_json, \
                    created_at, updated_at FROM users WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.map(|row| decode_user(&row)).transpose()
    }

    async fn save_user(&self, user: &User) -> Result<(), StorageError> {
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET username = excluded.username, \
                 password_hash = excluded.password_hash, status = excluded.status, \
                 roles_json = excluded.roles_json, permissions_json = excluded.permissions_json, \
                 updated_at = excluded.updated_at",
        )
        .bind(user.id.to_string())
        .bind(&user.username)
        .bind(&user.password_hash)
        .bind(user_status_name(user.status))
        .bind(serde_json::to_string(&user.roles)?)
        .bind(serde_json::to_string(&user.permissions)?)
        .bind(encode_timestamp(user.created_at))
        .bind(encode_timestamp(user.updated_at))
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM user_roles WHERE user_id = ?")
            .bind(user.id.to_string())
            .execute(&mut *transaction)
            .await?;
        for role in &user.roles {
            sqlx::query("INSERT INTO user_roles (user_id, role) VALUES (?, ?)")
                .bind(user.id.to_string())
                .bind(role_name(*role))
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

fn decode_user(row: &sqlx::sqlite::SqliteRow) -> Result<User, StorageError> {
    let status = match row.try_get::<&str, _>("status")? {
        "active" => UserStatus::Active,
        "suspended" => UserStatus::Suspended,
        "disabled" => UserStatus::Disabled,
        value => {
            return Err(StorageError::InvalidData(format!(
                "unknown user status {value}"
            )));
        }
    };
    Ok(User {
        id: UserId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        username: row.try_get("username")?,
        password_hash: row.try_get("password_hash")?,
        status,
        roles: serde_json::from_str(row.try_get("roles_json")?)?,
        permissions: serde_json::from_str(row.try_get("permissions_json")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    })
}

const fn user_status_name(status: UserStatus) -> &'static str {
    match status {
        UserStatus::Active => "active",
        UserStatus::Suspended => "suspended",
        UserStatus::Disabled => "disabled",
    }
}

const fn role_name(role: Role) -> &'static str {
    match role {
        Role::Master => "master",
        Role::Operator => "operator",
        Role::User => "user",
    }
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
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn master_bootstrap_is_atomic_and_one_time() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let repository = SqliteUserRepository::new(database);
        let first = InitialMaster {
            id: UserId::new(),
            username: "master-a".to_owned(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$test$hash".to_owned(),
            created_at: Utc::now(),
        };
        let second = InitialMaster {
            id: UserId::new(),
            username: "master-b".to_owned(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$test$hash".to_owned(),
            created_at: Utc::now(),
        };
        let (first_result, second_result) = tokio::join!(
            repository.bootstrap_master(&first),
            repository.bootstrap_master(&second)
        );
        assert_eq!(
            [first_result, second_result]
                .iter()
                .filter(|result| result.is_ok())
                .count(),
            1
        );
        assert!(repository.master_initialized().await.unwrap());
    }
}
