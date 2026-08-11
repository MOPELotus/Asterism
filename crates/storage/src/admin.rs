use std::str::FromStr;

use asterism_domain::{
    AuditActor, AuditRecord, AuditRecordId, Role, Timestamp, User, UserId, UserProfile, UserStatus,
};
use asterism_events::{DomainEvent, EventEnvelope};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row;

use crate::{
    AuditFilter, AuditPage, AuditQueryRepository, Database, StorageError, UserAdminCreate,
    UserAdminCreateOutcome, UserAdminRepository, UserAdminUpdate, UserAdminUpdateOutcome,
    UserProfilePage, outbox::enqueue_in_transaction,
};

const MAX_ADMIN_PAGE_SIZE: u32 = 200;
const MAX_ADMIN_OFFSET: u64 = 1_000_000;
const MAX_AUDIT_FILTER_BYTES: usize = 128;

#[derive(Clone, Debug)]
pub struct SqliteAdminRepository {
    database: Database,
}

impl SqliteAdminRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl UserAdminRepository for SqliteAdminRepository {
    async fn list_user_profiles(
        &self,
        limit: u32,
        offset: u64,
    ) -> Result<UserProfilePage, StorageError> {
        validate_pagination(limit, offset)?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(self.database.pool())
            .await?;
        let rows = sqlx::query(
            "SELECT id, username, status, roles_json, permissions_json, created_at, updated_at \
             FROM users ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated admin offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        Ok(UserProfilePage {
            items: rows
                .iter()
                .map(decode_user_profile)
                .collect::<Result<_, _>>()?,
            total: decode_count(total, "user")?,
        })
    }

    async fn find_user_profile(
        &self,
        user_id: UserId,
    ) -> Result<Option<UserProfile>, StorageError> {
        sqlx::query(
            "SELECT id, username, status, roles_json, permissions_json, created_at, updated_at \
             FROM users WHERE id = ?",
        )
        .bind(user_id.to_string())
        .fetch_optional(self.database.pool())
        .await?
        .as_ref()
        .map(decode_user_profile)
        .transpose()
    }

    async fn create_user(
        &self,
        request: UserAdminCreate<'_>,
    ) -> Result<UserAdminCreateOutcome, StorageError> {
        validate_user(request.user)?;
        validate_correlation_id(request.correlation_id)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let username_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE username = ? COLLATE NOCASE)",
        )
        .bind(&request.user.username)
        .fetch_one(&mut *transaction)
        .await?;
        if username_exists {
            transaction.rollback().await?;
            return Ok(UserAdminCreateOutcome::UsernameConflict);
        }
        insert_user(&mut transaction, request.user).await?;
        replace_roles(&mut transaction, request.user.id, &request.user.roles).await?;
        sqlx::query(
            "INSERT INTO credit_accounts (user_id, available, reserved, updated_at) \
             VALUES (?, 0, 0, ?)",
        )
        .bind(request.user.id.to_string())
        .bind(encode_timestamp(request.user.created_at))
        .execute(&mut *transaction)
        .await?;
        insert_user_audit(
            &mut transaction,
            request.actor,
            "user_created",
            request.user.id,
            request.correlation_id,
            request.user.created_at,
            serde_json::json!({
                "status": request.user.status,
                "roles": request.user.roles,
                "permissions": request.user.permissions,
            }),
        )
        .await?;
        enqueue_user_changed(
            &mut transaction,
            request.user.id,
            request.user.status,
            request.user.created_at,
            request.correlation_id,
        )
        .await?;
        transaction.commit().await?;
        Ok(UserAdminCreateOutcome::Created(UserProfile::from(
            request.user,
        )))
    }

    async fn update_user(
        &self,
        request: UserAdminUpdate<'_>,
    ) -> Result<UserAdminUpdateOutcome, StorageError> {
        validate_user_admin_update(&request)?;
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let current = sqlx::query(
            "SELECT id, username, status, roles_json, permissions_json, created_at, updated_at \
             FROM users WHERE id = ?",
        )
        .bind(request.user_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(current) = current.as_ref().map(decode_user_profile).transpose()? else {
            transaction.rollback().await?;
            return Ok(UserAdminUpdateOutcome::UserNotFound);
        };
        if current.updated_at != request.expected_updated_at {
            transaction.rollback().await?;
            return Ok(UserAdminUpdateOutcome::RevisionConflict);
        }
        if removes_last_active_master(&mut transaction, &current, &request).await? {
            transaction.rollback().await?;
            return Ok(UserAdminUpdateOutcome::LastActiveMaster);
        }
        let changed = sqlx::query(
            "UPDATE users SET status = ?, roles_json = ?, permissions_json = ?, updated_at = ? \
             WHERE id = ? AND updated_at = ?",
        )
        .bind(user_status_name(request.status))
        .bind(serde_json::to_string(request.roles)?)
        .bind(serde_json::to_string(request.permissions)?)
        .bind(encode_timestamp(request.updated_at))
        .bind(request.user_id.to_string())
        .bind(encode_timestamp(request.expected_updated_at))
        .execute(&mut *transaction)
        .await?;
        if changed.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(UserAdminUpdateOutcome::RevisionConflict);
        }
        replace_roles(&mut transaction, request.user_id, request.roles).await?;
        insert_user_audit(
            &mut transaction,
            request.actor,
            "user_updated",
            request.user_id,
            request.correlation_id,
            request.updated_at,
            serde_json::json!({
                "from_status": current.status,
                "to_status": request.status,
                "from_roles": current.roles,
                "to_roles": request.roles,
                "from_permissions": current.permissions,
                "to_permissions": request.permissions,
            }),
        )
        .await?;
        enqueue_user_changed(
            &mut transaction,
            request.user_id,
            request.status,
            request.updated_at,
            request.correlation_id,
        )
        .await?;
        let updated = UserProfile {
            id: current.id,
            username: current.username,
            status: request.status,
            roles: request.roles.to_vec(),
            permissions: request.permissions.to_vec(),
            created_at: current.created_at,
            updated_at: request.updated_at,
        };
        transaction.commit().await?;
        Ok(UserAdminUpdateOutcome::Updated(updated))
    }
}

#[async_trait]
impl AuditQueryRepository for SqliteAdminRepository {
    async fn list_audit_records(
        &self,
        owner_scope: Option<UserId>,
        filter: &AuditFilter,
        limit: u32,
        offset: u64,
    ) -> Result<AuditPage, StorageError> {
        validate_pagination(limit, offset)?;
        validate_audit_filter(filter)?;
        let owner = owner_scope.map(|id| id.to_string());
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_records AS audit \
             WHERE (? IS NULL OR \
                    (audit.actor_type = 'user' AND audit.actor_id = ?) OR \
                    (audit.actor_type = 'service_token' AND audit.actor_id IN \
                        (SELECT id FROM service_tokens WHERE owner_user_id = ?)) OR \
                    (audit.resource_type = 'user' AND audit.resource_id = ?)) \
               AND (? IS NULL OR audit.action = ?) \
               AND (? IS NULL OR audit.resource_type = ?) \
               AND (? IS NULL OR audit.resource_id = ?) \
               AND (? IS NULL OR audit.outcome = ?)",
        )
        .bind(&owner)
        .bind(&owner)
        .bind(&owner)
        .bind(&owner)
        .bind(&filter.action)
        .bind(&filter.action)
        .bind(&filter.resource_type)
        .bind(&filter.resource_type)
        .bind(&filter.resource_id)
        .bind(&filter.resource_id)
        .bind(&filter.outcome)
        .bind(&filter.outcome)
        .fetch_one(self.database.pool())
        .await?;
        let rows = sqlx::query(
            "SELECT audit.id, audit.occurred_at, audit.actor_type, audit.actor_id, audit.action, \
                    audit.resource_type, audit.resource_id, audit.request_id, audit.correlation_id, \
                    audit.outcome, audit.metadata_sanitized_json \
             FROM audit_records AS audit \
             WHERE (? IS NULL OR \
                    (audit.actor_type = 'user' AND audit.actor_id = ?) OR \
                    (audit.actor_type = 'service_token' AND audit.actor_id IN \
                        (SELECT id FROM service_tokens WHERE owner_user_id = ?)) OR \
                    (audit.resource_type = 'user' AND audit.resource_id = ?)) \
               AND (? IS NULL OR audit.action = ?) \
               AND (? IS NULL OR audit.resource_type = ?) \
               AND (? IS NULL OR audit.resource_id = ?) \
               AND (? IS NULL OR audit.outcome = ?) \
             ORDER BY audit.occurred_at DESC, audit.id DESC LIMIT ? OFFSET ?",
        )
        .bind(&owner)
        .bind(&owner)
        .bind(&owner)
        .bind(&owner)
        .bind(&filter.action)
        .bind(&filter.action)
        .bind(&filter.resource_type)
        .bind(&filter.resource_type)
        .bind(&filter.resource_id)
        .bind(&filter.resource_id)
        .bind(&filter.outcome)
        .bind(&filter.outcome)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated admin offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        Ok(AuditPage {
            items: rows
                .iter()
                .map(decode_audit_record)
                .collect::<Result<_, _>>()?,
            total: decode_count(total, "audit")?,
        })
    }
}

async fn insert_user(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user: &User,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO users \
         (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user.id.to_string())
    .bind(&user.username)
    .bind(&user.password_hash)
    .bind(user_status_name(user.status))
    .bind(serde_json::to_string(&user.roles)?)
    .bind(serde_json::to_string(&user.permissions)?)
    .bind(encode_timestamp(user.created_at))
    .bind(encode_timestamp(user.updated_at))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn replace_roles(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: UserId,
    roles: &[Role],
) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM user_roles WHERE user_id = ?")
        .bind(user_id.to_string())
        .execute(&mut **transaction)
        .await?;
    for role in roles {
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES (?, ?)")
            .bind(user_id.to_string())
            .bind(role_name(*role))
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

async fn removes_last_active_master(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    current: &UserProfile,
    request: &UserAdminUpdate<'_>,
) -> Result<bool, StorageError> {
    let currently_active_master =
        current.status == UserStatus::Active && current.roles.contains(&Role::Master);
    let remains_active_master =
        request.status == UserStatus::Active && request.roles.contains(&Role::Master);
    if !currently_active_master || remains_active_master {
        return Ok(false);
    }
    let other_active_masters: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users AS user \
         INNER JOIN user_roles AS role ON role.user_id = user.id AND role.role = 'master' \
         WHERE user.status = 'active' AND user.id <> ?",
    )
    .bind(current.id.to_string())
    .fetch_one(&mut **transaction)
    .await?;
    Ok(other_active_masters == 0)
}

async fn insert_user_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor: AuditActor,
    action: &str,
    user_id: UserId,
    correlation_id: &str,
    at: Timestamp,
    metadata: serde_json::Value,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          correlation_id, outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'user', ?, ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(user_id.to_string())
    .bind(correlation_id)
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn enqueue_user_changed(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: UserId,
    status: UserStatus,
    at: Timestamp,
    correlation_id: &str,
) -> Result<(), StorageError> {
    enqueue_in_transaction(
        transaction,
        &EventEnvelope::at(
            correlation_id,
            DomainEvent::UserChanged { user_id, status },
            at,
        ),
    )
    .await
}

fn validate_user(user: &User) -> Result<(), StorageError> {
    if user.username.is_empty()
        || user.username.len() > 64
        || user.username.trim() != user.username
        || user.username.chars().any(char::is_control)
        || !user.password_hash.starts_with("$argon2id$")
        || user.roles.is_empty()
        || has_duplicates(&user.roles)
        || has_duplicates(&user.permissions)
        || user.updated_at < user.created_at
    {
        return Err(StorageError::InvalidData(
            "admin user payload violates persistence invariants".to_owned(),
        ));
    }
    Ok(())
}

fn validate_user_admin_update(request: &UserAdminUpdate<'_>) -> Result<(), StorageError> {
    validate_correlation_id(request.correlation_id)?;
    if request.roles.is_empty()
        || has_duplicates(request.roles)
        || has_duplicates(request.permissions)
        || request.updated_at <= request.expected_updated_at
    {
        return Err(StorageError::InvalidData(
            "admin user update violates persistence invariants".to_owned(),
        ));
    }
    Ok(())
}

fn has_duplicates<T: Ord + Copy>(values: &[T]) -> bool {
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    ordered.windows(2).any(|pair| pair[0] == pair[1])
}

fn validate_correlation_id(value: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value.len() > MAX_AUDIT_FILTER_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(StorageError::InvalidData(
            "correlation ID is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_pagination(limit: u32, offset: u64) -> Result<(), StorageError> {
    if limit == 0 || limit > MAX_ADMIN_PAGE_SIZE || offset > MAX_ADMIN_OFFSET {
        Err(StorageError::InvalidData(
            "admin pagination is outside the supported range".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_audit_filter(filter: &AuditFilter) -> Result<(), StorageError> {
    for value in [
        filter.action.as_deref(),
        filter.resource_type.as_deref(),
        filter.resource_id.as_deref(),
        filter.outcome.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.is_empty()
            || value.len() > MAX_AUDIT_FILTER_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(StorageError::InvalidData(
                "audit filter is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn decode_user_profile(row: &sqlx::sqlite::SqliteRow) -> Result<UserProfile, StorageError> {
    Ok(UserProfile {
        id: parse_id(row.try_get("id")?)?,
        username: row.try_get("username")?,
        status: decode_enum(row.try_get("status")?)?,
        roles: serde_json::from_str(row.try_get("roles_json")?)?,
        permissions: serde_json::from_str(row.try_get("permissions_json")?)?,
        created_at: decode_timestamp(row.try_get("created_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
    })
}

fn decode_audit_record(row: &sqlx::sqlite::SqliteRow) -> Result<AuditRecord, StorageError> {
    Ok(AuditRecord {
        id: parse_id(row.try_get("id")?)?,
        occurred_at: decode_timestamp(row.try_get("occurred_at")?)?,
        actor_type: row.try_get("actor_type")?,
        actor_id: row.try_get("actor_id")?,
        action: row.try_get("action")?,
        resource_type: row.try_get("resource_type")?,
        resource_id: row.try_get("resource_id")?,
        request_id: row.try_get("request_id")?,
        correlation_id: row.try_get("correlation_id")?,
        outcome: row.try_get("outcome")?,
        metadata_sanitized: serde_json::from_str(row.try_get("metadata_sanitized_json")?)?,
    })
}

fn parse_id<T: FromStr>(value: &str) -> Result<T, StorageError>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error: T::Err| StorageError::InvalidData(error.to_string()))
}

fn decode_enum<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(StorageError::Serialization)
}

fn decode_count(value: i64, resource: &str) -> Result<u64, StorageError> {
    u64::try_from(value)
        .map_err(|_| StorageError::InvalidData(format!("{resource} count is invalid")))
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
    use chrono::Duration;

    use super::*;
    use crate::{InitialMaster, SqliteUserRepository};

    #[tokio::test]
    async fn user_administration_is_password_free_revisioned_and_audited() {
        let fixture = Fixture::new().await;
        let user = fixture.user("member", Role::User);
        let created = fixture
            .repository
            .create_user(UserAdminCreate {
                user: &user,
                actor: AuditActor::User(fixture.master_id),
                correlation_id: "create-member",
            })
            .await
            .unwrap();
        assert!(matches!(
            created,
            UserAdminCreateOutcome::Created(UserProfile { id, .. }) if id == user.id
        ));
        let duplicate = fixture
            .repository
            .create_user(UserAdminCreate {
                user: &User {
                    id: UserId::new(),
                    ..user.clone()
                },
                actor: AuditActor::User(fixture.master_id),
                correlation_id: "duplicate-member",
            })
            .await
            .unwrap();
        assert_eq!(duplicate, UserAdminCreateOutcome::UsernameConflict);
        let page = fixture.repository.list_user_profiles(10, 0).await.unwrap();
        assert_eq!(page.total, 2);
        assert!(page.items.iter().any(|profile| profile.id == user.id));

        let updated_at = user.updated_at + Duration::seconds(1);
        let updated = fixture
            .repository
            .update_user(UserAdminUpdate {
                user_id: user.id,
                expected_updated_at: user.updated_at,
                status: UserStatus::Suspended,
                roles: &[Role::Operator],
                permissions: &[],
                actor: AuditActor::User(fixture.master_id),
                correlation_id: "suspend-member",
                updated_at,
            })
            .await
            .unwrap();
        assert!(matches!(
            updated,
            UserAdminUpdateOutcome::Updated(UserProfile {
                status: UserStatus::Suspended,
                ..
            })
        ));
        let stale = fixture
            .repository
            .update_user(UserAdminUpdate {
                user_id: user.id,
                expected_updated_at: user.updated_at,
                status: UserStatus::Active,
                roles: &[Role::User],
                permissions: &[],
                actor: AuditActor::User(fixture.master_id),
                correlation_id: "stale-member",
                updated_at: updated_at + Duration::seconds(1),
            })
            .await
            .unwrap();
        assert_eq!(stale, UserAdminUpdateOutcome::RevisionConflict);
        assert_eq!(fixture.count("user_roles").await, 2);
        assert_eq!(fixture.count("event_outbox").await, 2);
    }

    #[tokio::test]
    async fn the_last_active_master_cannot_be_suspended_or_demoted() {
        let fixture = Fixture::new().await;
        let profile = fixture
            .repository
            .find_user_profile(fixture.master_id)
            .await
            .unwrap()
            .unwrap();
        let outcome = fixture
            .repository
            .update_user(UserAdminUpdate {
                user_id: fixture.master_id,
                expected_updated_at: profile.updated_at,
                status: UserStatus::Suspended,
                roles: &[Role::Master],
                permissions: &[],
                actor: AuditActor::User(fixture.master_id),
                correlation_id: "suspend-last-master",
                updated_at: profile.updated_at + Duration::seconds(1),
            })
            .await
            .unwrap();
        assert_eq!(outcome, UserAdminUpdateOutcome::LastActiveMaster);
        assert_eq!(
            fixture
                .repository
                .find_user_profile(fixture.master_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            UserStatus::Active
        );
    }

    #[tokio::test]
    async fn audit_queries_separate_owner_scope_from_global_scope() {
        let fixture = Fixture::new().await;
        let member = fixture.user("member-audit", Role::User);
        fixture
            .repository
            .create_user(UserAdminCreate {
                user: &member,
                actor: AuditActor::User(fixture.master_id),
                correlation_id: "create-audit-member",
            })
            .await
            .unwrap();
        let all = fixture
            .repository
            .list_audit_records(None, &AuditFilter::default(), 50, 0)
            .await
            .unwrap();
        assert_eq!(all.total, 2);
        let member_scope = fixture
            .repository
            .list_audit_records(Some(member.id), &AuditFilter::default(), 50, 0)
            .await
            .unwrap();
        assert_eq!(member_scope.total, 1);
        assert_eq!(
            member_scope.items[0].resource_id.as_deref(),
            Some(&*member.id.to_string())
        );
        let filtered = fixture
            .repository
            .list_audit_records(
                None,
                &AuditFilter {
                    action: Some("user_created".to_owned()),
                    ..AuditFilter::default()
                },
                50,
                0,
            )
            .await
            .unwrap();
        assert_eq!(filtered.total, 1);
    }

    struct Fixture {
        database: Database,
        repository: SqliteAdminRepository,
        master_id: UserId,
        now: Timestamp,
    }

    impl Fixture {
        async fn new() -> Self {
            let database = Database::connect("sqlite::memory:").await.unwrap();
            database.migrate().await.unwrap();
            let now = Utc::now();
            let master_id = UserId::new();
            SqliteUserRepository::new(database.clone())
                .bootstrap_master(&InitialMaster {
                    id: master_id,
                    username: "master".to_owned(),
                    password_hash: "$argon2id$v=19$m=19456,t=2,p=1$test$hash".to_owned(),
                    created_at: now,
                })
                .await
                .unwrap();
            Self {
                repository: SqliteAdminRepository::new(database.clone()),
                database,
                master_id,
                now,
            }
        }

        fn user(&self, username: &str, role: Role) -> User {
            User {
                id: UserId::new(),
                username: username.to_owned(),
                password_hash: "$argon2id$v=19$m=19456,t=2,p=1$test$hash".to_owned(),
                status: UserStatus::Active,
                roles: vec![role],
                permissions: Vec::new(),
                created_at: self.now,
                updated_at: self.now,
            }
        }

        async fn count(&self, table: &str) -> i64 {
            let query = format!("SELECT COUNT(*) FROM {table}");
            sqlx::query_scalar(&query)
                .fetch_one(self.database.pool())
                .await
                .unwrap()
        }
    }
}
