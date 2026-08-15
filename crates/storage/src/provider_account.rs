use std::{collections::BTreeSet, str::FromStr};

use asterism_domain::{
    AccountHealth, AuditActor, AuditRecordId, ExecutionId, ProviderAccount, ProviderAccountId,
    ProviderId, SecretId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};

use crate::{
    AccountHealthRepository, Database, ProviderAccountRepository, ProviderAccountRuntimeRepository,
    StorageError,
};

#[derive(Clone, Debug)]
pub struct SqliteProviderAccountRepository {
    database: Database,
}

impl SqliteProviderAccountRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl ProviderAccountRepository for SqliteProviderAccountRepository {
    async fn list_provider_accounts(
        &self,
        owner_id: UserId,
    ) -> Result<Vec<ProviderAccount>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, owner_user_id, provider_id, display_name, tenant, auth_state_json, \
                    network_profile_id, created_at, updated_at \
             FROM provider_accounts WHERE owner_user_id = ? ORDER BY created_at, id",
        )
        .bind(owner_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for row in rows {
            accounts.push(self.decode_account(&row).await?);
        }
        Ok(accounts)
    }

    async fn find_provider_account(
        &self,
        owner_id: UserId,
        account_id: ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StorageError> {
        let row = sqlx::query(
            "SELECT id, owner_user_id, provider_id, display_name, tenant, auth_state_json, \
                    network_profile_id, created_at, updated_at \
             FROM provider_accounts WHERE owner_user_id = ? AND id = ?",
        )
        .bind(owner_id.to_string())
        .bind(account_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(self.decode_account(&row).await?)),
            None => Ok(None),
        }
    }

    async fn create_provider_account(
        &self,
        account: &ProviderAccount,
        actor: AuditActor,
    ) -> Result<(), StorageError> {
        validate_account(account, true)?;
        let mut transaction = self.database.pool().begin().await?;
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, tenant, auth_state_json, \
              network_profile_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account.id.to_string())
        .bind(account.owner_id.to_string())
        .bind(account.provider_id.as_str())
        .bind(&account.display_name)
        .bind(&account.tenant)
        .bind(serde_json::to_string(&account.auth_state)?)
        .bind(&account.network_profile_id)
        .bind(encode_timestamp(account.created_at))
        .bind(encode_timestamp(account.updated_at))
        .execute(&mut *transaction)
        .await?;
        insert_account_audit(
            &mut transaction,
            actor,
            "provider_account_created",
            account.id,
            account.updated_at,
            account.provider_id.as_str(),
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn update_provider_account(
        &self,
        account: &ProviderAccount,
        actor: AuditActor,
    ) -> Result<bool, StorageError> {
        validate_account(account, false)?;
        let mut transaction = self.database.pool().begin().await?;
        let persisted_credential_refs = sqlx::query_scalar::<_, String>(
            "SELECT secret_blob_id FROM provider_account_credentials \
             WHERE provider_account_id = ? ORDER BY created_at, secret_blob_id",
        )
        .bind(account.id.to_string())
        .fetch_all(&mut *transaction)
        .await?;
        let persisted_credential_refs = decode_credential_refs(persisted_credential_refs)?;
        if persisted_credential_refs != account.credential_refs {
            return Err(StorageError::InvalidData(
                "provider account credentials must use the credential repository".to_owned(),
            ));
        }
        let result = sqlx::query(
            "UPDATE provider_accounts \
             SET display_name = ?, tenant = ?, auth_state_json = ?, network_profile_id = ?, \
                 updated_at = ? \
             WHERE id = ? AND owner_user_id = ? AND provider_id = ? AND created_at = ?",
        )
        .bind(&account.display_name)
        .bind(&account.tenant)
        .bind(serde_json::to_string(&account.auth_state)?)
        .bind(&account.network_profile_id)
        .bind(encode_timestamp(account.updated_at))
        .bind(account.id.to_string())
        .bind(account.owner_id.to_string())
        .bind(account.provider_id.as_str())
        .bind(encode_timestamp(account.created_at))
        .execute(&mut *transaction)
        .await?;
        let updated = result.rows_affected() == 1;
        if updated {
            insert_account_audit(
                &mut transaction,
                actor,
                "provider_account_updated",
                account.id,
                account.updated_at,
                account.provider_id.as_str(),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(updated)
    }

    async fn delete_provider_account(
        &self,
        owner_id: UserId,
        account_id: ProviderAccountId,
        at: Timestamp,
        actor: AuditActor,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.database.pool().begin_with("BEGIN IMMEDIATE").await?;
        let provider_id: Option<String> = sqlx::query_scalar(
            "SELECT provider_id FROM provider_accounts WHERE id = ? AND owner_user_id = ?",
        )
        .bind(account_id.to_string())
        .bind(owner_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let deleted = provider_id.is_some();
        if let Some(provider_id) = provider_id {
            sqlx::query(
                "DELETE FROM secret_blobs WHERE id IN \
                 (SELECT secret_blob_id FROM provider_account_credentials \
                  WHERE provider_account_id = ?)",
            )
            .bind(account_id.to_string())
            .execute(&mut *transaction)
            .await?;
            sqlx::query("DELETE FROM provider_accounts WHERE id = ? AND owner_user_id = ?")
                .bind(account_id.to_string())
                .bind(owner_id.to_string())
                .execute(&mut *transaction)
                .await?;
            insert_account_audit(
                &mut transaction,
                actor,
                "provider_account_deleted",
                account_id,
                at,
                &provider_id,
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(deleted)
    }
}

#[async_trait]
impl ProviderAccountRuntimeRepository for SqliteProviderAccountRepository {
    async fn find_runtime_provider_account(
        &self,
        account_id: ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StorageError> {
        let row = sqlx::query(
            "SELECT id, owner_user_id, provider_id, display_name, tenant, auth_state_json, \
                    network_profile_id, created_at, updated_at \
             FROM provider_accounts WHERE id = ?",
        )
        .bind(account_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        match row {
            Some(row) => Ok(Some(self.decode_account(&row).await?)),
            None => Ok(None),
        }
    }
}

#[async_trait]
impl AccountHealthRepository for SqliteProviderAccountRepository {
    async fn find_owned_account_health(
        &self,
        owner_id: UserId,
        account_id: ProviderAccountId,
    ) -> Result<Option<AccountHealth>, StorageError> {
        let Some(account) = self.find_provider_account(owner_id, account_id).await? else {
            return Ok(None);
        };
        let drift = sqlx::query(
            "SELECT execution.id, COALESCE(attempt.finished_at, attempt.started_at) AS drift_at \
             FROM tasks AS task \
             INNER JOIN executions AS execution ON execution.task_id = task.id \
             INNER JOIN execution_attempts AS attempt ON attempt.execution_id = execution.id \
             WHERE task.provider_account_id = ? AND attempt.error_class = 'protocol_drift' \
             ORDER BY drift_at DESC, attempt.id DESC LIMIT 1",
        )
        .bind(account_id.to_string())
        .fetch_optional(self.database.pool())
        .await?
        .map(|row| {
            Ok::<_, StorageError>((
                ExecutionId::from_str(row.try_get("id")?)
                    .map_err(|error| StorageError::InvalidData(error.to_string()))?,
                decode_timestamp(row.try_get("drift_at")?)?,
            ))
        })
        .transpose()?;
        Ok(Some(AccountHealth::from_account(&account, drift)))
    }
}

impl SqliteProviderAccountRepository {
    async fn decode_account(&self, row: &SqliteRow) -> Result<ProviderAccount, StorageError> {
        let account_id = ProviderAccountId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let credential_refs = sqlx::query_scalar::<_, String>(
            "SELECT secret_blob_id FROM provider_account_credentials \
             WHERE provider_account_id = ? ORDER BY created_at, secret_blob_id",
        )
        .bind(account_id.to_string())
        .fetch_all(self.database.pool())
        .await?;
        let credential_refs = decode_credential_refs(credential_refs)?;
        Ok(ProviderAccount {
            id: account_id,
            owner_id: UserId::from_str(row.try_get("owner_user_id")?)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?,
            provider_id: ProviderId::new(row.try_get::<String, _>("provider_id")?)
                .map_err(|error| StorageError::InvalidData(error.to_string()))?,
            display_name: row.try_get("display_name")?,
            tenant: row.try_get("tenant")?,
            auth_state: serde_json::from_str(row.try_get("auth_state_json")?)?,
            network_profile_id: row.try_get("network_profile_id")?,
            credential_refs,
            created_at: decode_timestamp(row.try_get("created_at")?)?,
            updated_at: decode_timestamp(row.try_get("updated_at")?)?,
        })
    }
}

fn decode_credential_refs(values: Vec<String>) -> Result<Vec<SecretId>, StorageError> {
    values
        .into_iter()
        .map(|value| {
            SecretId::from_str(&value).map_err(|error| StorageError::InvalidData(error.to_string()))
        })
        .collect()
}

fn validate_account(account: &ProviderAccount, creating: bool) -> Result<(), StorageError> {
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
    let credential_refs_unique = account
        .credential_refs
        .iter()
        .collect::<BTreeSet<_>>()
        .len()
        == account.credential_refs.len();
    if !display_name_valid
        || !tenant_valid
        || account.updated_at < account.created_at
        || !credential_refs_unique
        || (creating && !account.credential_refs.is_empty())
    {
        return Err(StorageError::InvalidData(
            "provider account fields or lifecycle timestamps are invalid".to_owned(),
        ));
    }
    Ok(())
}

async fn insert_account_audit(
    transaction: &mut Transaction<'_, Sqlite>,
    actor: AuditActor,
    action: &str,
    account_id: ProviderAccountId,
    at: Timestamp,
    provider_id: &str,
) -> Result<(), StorageError> {
    let (actor_type, actor_id) = match actor {
        AuditActor::User(id) => ("user", id.to_string()),
        AuditActor::ServiceToken(id) => ("service_token", id.to_string()),
    };
    let metadata = serde_json::json!({ "provider_id": provider_id });
    sqlx::query(
        "INSERT INTO audit_records \
         (id, occurred_at, actor_type, actor_id, action, resource_type, resource_id, \
          outcome, metadata_sanitized_json) \
         VALUES (?, ?, ?, ?, ?, 'provider_account', ?, 'succeeded', ?)",
    )
    .bind(AuditRecordId::new().to_string())
    .bind(encode_timestamp(at))
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(account_id.to_string())
    .bind(serde_json::to_string(&metadata)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
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
    use asterism_domain::{AccountHealthState, AuthState, Role, TaskId};
    use sqlx::Row;

    use super::*;

    #[tokio::test]
    async fn account_lifecycle_is_owner_scoped_and_audited() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let other_owner = UserId::new();
        insert_user(&database, owner).await;
        insert_user(&database, other_owner).await;
        let repository = SqliteProviderAccountRepository::new(database.clone());
        let now = Utc::now();
        let mut account = ProviderAccount {
            id: ProviderAccountId::new(),
            owner_id: owner,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Idle,
            network_profile_id: None,
            credential_refs: Vec::new(),
            created_at: now,
            updated_at: now,
        };

        repository
            .create_provider_account(&account, AuditActor::User(owner))
            .await
            .unwrap();
        assert_eq!(
            repository.list_provider_accounts(owner).await.unwrap(),
            vec![account.clone()]
        );
        assert!(
            repository
                .find_provider_account(other_owner, account.id)
                .await
                .unwrap()
                .is_none()
        );

        account.display_name = "renamed".to_owned();
        account.tenant = Some("tenant-a".to_owned());
        account.updated_at = now + chrono::Duration::seconds(1);
        assert!(
            repository
                .update_provider_account(&account, AuditActor::User(owner))
                .await
                .unwrap()
        );
        assert_eq!(
            repository
                .find_runtime_provider_account(account.id)
                .await
                .unwrap(),
            Some(account.clone())
        );
        assert!(
            !repository
                .delete_provider_account(
                    other_owner,
                    account.id,
                    now + chrono::Duration::seconds(2),
                    AuditActor::User(other_owner),
                )
                .await
                .unwrap()
        );
        assert!(
            repository
                .delete_provider_account(
                    owner,
                    account.id,
                    now + chrono::Duration::seconds(2),
                    AuditActor::User(owner),
                )
                .await
                .unwrap()
        );

        let actions: Vec<String> = sqlx::query(
            "SELECT action FROM audit_records WHERE resource_id = ? ORDER BY occurred_at",
        )
        .bind(account.id.to_string())
        .fetch_all(database.pool())
        .await
        .unwrap()
        .iter()
        .map(|row| row.get("action"))
        .collect();
        assert_eq!(
            actions,
            [
                "provider_account_created",
                "provider_account_updated",
                "provider_account_deleted"
            ]
        );
    }

    #[tokio::test]
    async fn account_health_uses_only_fresh_attempt_bound_protocol_drift() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let other_owner = UserId::new();
        insert_user(&database, owner).await;
        insert_user(&database, other_owner).await;
        let repository = SqliteProviderAccountRepository::new(database.clone());
        let now = Utc::now();
        let account = ProviderAccount {
            id: ProviderAccountId::new(),
            owner_id: owner,
            provider_id: ProviderId::new("provider-alpha").unwrap(),
            display_name: "primary".to_owned(),
            tenant: None,
            auth_state: AuthState::Authenticated,
            network_profile_id: None,
            credential_refs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        repository
            .create_provider_account(&account, AuditActor::User(owner))
            .await
            .unwrap();
        assert_eq!(
            repository
                .find_owned_account_health(owner, account.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            AccountHealthState::Healthy
        );

        let task_id = TaskId::new();
        let drift_at = now + chrono::Duration::seconds(1);
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, assessment_class, \
              title, remote_state, orchestration_state, discovered_at, updated_at, capabilities_json) \
             VALUES (?, ?, 'task', 'fingerprint', 'work', 'routine', 'Task', 'pending', \
                     'failed', ?, ?, '[]')",
        )
        .bind(task_id.to_string())
        .bind(account.id.to_string())
        .bind(now.to_rfc3339())
        .bind(drift_at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let execution_id = ExecutionId::new();
        sqlx::query(
            "INSERT INTO executions (id, task_id, request_source, state, started_at, finished_at, created_at) \
             VALUES (?, ?, 'system', 'failed', ?, ?, ?)",
        )
        .bind(execution_id.to_string())
        .bind(task_id.to_string())
        .bind(now.to_rfc3339())
        .bind(drift_at.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_attempts \
             (id, execution_id, attempt_no, started_at, finished_at, result, error_class) \
             VALUES (?, ?, 1, ?, ?, 'failed', 'protocol_drift')",
        )
        .bind(asterism_domain::ExecutionAttemptId::new().to_string())
        .bind(execution_id.to_string())
        .bind(now.to_rfc3339())
        .bind(drift_at.to_rfc3339())
        .execute(database.pool())
        .await
        .unwrap();
        let health = repository
            .find_owned_account_health(owner, account.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(health.state, AccountHealthState::ProtocolChanged);
        assert_eq!(health.protocol_drift_execution_id, Some(execution_id));
        assert_eq!(health.protocol_drift_at, Some(drift_at));
        assert!(
            repository
                .find_owned_account_health(other_owner, account.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn insert_user(database: &Database, user_id: UserId) {
        let now = encode_timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', ?, '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(format!("user-{user_id}"))
        .bind(serde_json::to_string(&[Role::User]).unwrap())
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
    }
}
