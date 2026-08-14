use std::str::FromStr;

use asterism_domain::{
    AssessmentClass, CourseId, OrchestrationState, ProviderAccountId, RemoteState, SourceType,
    Task, TaskId, TaskSnapshotId, Timestamp, UserId,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Row, sqlite::SqliteRow};

use crate::{Database, StorageError, TaskPage, TaskQueryRepository, TaskRuntimeRepository};

const MAX_PAGE_SIZE: u32 = 200;

#[derive(Clone, Debug)]
pub struct SqliteTaskQueryRepository {
    database: Database,
}

impl SqliteTaskQueryRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl TaskQueryRepository for SqliteTaskQueryRepository {
    async fn list_owned_tasks(
        &self,
        owner_id: UserId,
        provider_account_id: Option<ProviderAccountId>,
        limit: u32,
        offset: u64,
    ) -> Result<TaskPage, StorageError> {
        if limit == 0 || limit > MAX_PAGE_SIZE || offset > i64::MAX.cast_unsigned() {
            return Err(StorageError::InvalidData(
                "task pagination is outside the supported range".to_owned(),
            ));
        }
        let account_id = provider_account_id.map(|id| id.to_string());
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks AS task \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? \
               AND (? IS NULL OR task.provider_account_id = ?)",
        )
        .bind(owner_id.to_string())
        .bind(&account_id)
        .bind(&account_id)
        .fetch_one(self.database.pool())
        .await?;
        let rows = sqlx::query(
            "SELECT task.id, task.provider_account_id, task.course_id, task.remote_id, \
                    task.source_type, task.assessment_class, task.title, task.remote_state, \
                    task.orchestration_state, task.opens_at, task.due_at, task.closes_at, \
                    task.discovered_at, task.updated_at, task.latest_snapshot_id, \
                    task.capabilities_json \
             FROM tasks AS task \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? \
               AND (? IS NULL OR task.provider_account_id = ?) \
             ORDER BY task.updated_at DESC, task.id DESC LIMIT ? OFFSET ?",
        )
        .bind(owner_id.to_string())
        .bind(&account_id)
        .bind(&account_id)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).expect("validated task offset fits i64"))
        .fetch_all(self.database.pool())
        .await?;
        let items = rows.iter().map(decode_task).collect::<Result<_, _>>()?;
        Ok(TaskPage {
            items,
            total: u64::try_from(total).map_err(|error| {
                StorageError::InvalidData(format!("task count is invalid: {error}"))
            })?,
        })
    }

    async fn find_owned_task(
        &self,
        owner_id: UserId,
        task_id: TaskId,
    ) -> Result<Option<Task>, StorageError> {
        let row = sqlx::query(
            "SELECT task.id, task.provider_account_id, task.course_id, task.remote_id, \
                    task.source_type, task.assessment_class, task.title, task.remote_state, \
                    task.orchestration_state, task.opens_at, task.due_at, task.closes_at, \
                    task.discovered_at, task.updated_at, task.latest_snapshot_id, \
                    task.capabilities_json \
             FROM tasks AS task \
             INNER JOIN provider_accounts AS account ON account.id = task.provider_account_id \
             WHERE account.owner_user_id = ? AND task.id = ?",
        )
        .bind(owner_id.to_string())
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.as_ref().map(decode_task).transpose()
    }
}

#[async_trait]
impl TaskRuntimeRepository for SqliteTaskQueryRepository {
    async fn find_runtime_task(&self, task_id: TaskId) -> Result<Option<Task>, StorageError> {
        let row = sqlx::query(
            "SELECT task.id, task.provider_account_id, task.course_id, task.remote_id, \
                    task.source_type, task.assessment_class, task.title, task.remote_state, \
                    task.orchestration_state, task.opens_at, task.due_at, task.closes_at, \
                    task.discovered_at, task.updated_at, task.latest_snapshot_id, \
                    task.capabilities_json \
             FROM tasks AS task WHERE task.id = ?",
        )
        .bind(task_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        row.map(|row| decode_task(&row)).transpose()
    }

    async fn find_runtime_task_by_remote_identity(
        &self,
        provider_account_id: ProviderAccountId,
        remote_task_id: &str,
    ) -> Result<Option<Task>, StorageError> {
        if remote_task_id.is_empty()
            || remote_task_id.len() > 512
            || remote_task_id.trim() != remote_task_id
            || remote_task_id.chars().any(char::is_control)
        {
            return Err(StorageError::InvalidData(
                "runtime remote Task identity is invalid".to_owned(),
            ));
        }
        let rows = sqlx::query(
            "SELECT task.id, task.provider_account_id, task.course_id, task.remote_id, \
                    task.source_type, task.assessment_class, task.title, task.remote_state, \
                    task.orchestration_state, task.opens_at, task.due_at, task.closes_at, \
                    task.discovered_at, task.updated_at, task.latest_snapshot_id, \
                    task.capabilities_json \
             FROM tasks AS task \
             WHERE task.provider_account_id = ? AND task.remote_id = ? \
             ORDER BY task.id LIMIT 2",
        )
        .bind(provider_account_id.to_string())
        .bind(remote_task_id)
        .fetch_all(self.database.pool())
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_task(row).map(Some),
            [..] => Err(StorageError::InvalidData(
                "runtime remote Task identity is ambiguous across source types".to_owned(),
            )),
        }
    }
}

pub(crate) fn decode_task(row: &SqliteRow) -> Result<Task, StorageError> {
    Ok(Task {
        id: TaskId::from_str(row.try_get("id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        provider_account_id: ProviderAccountId::from_str(row.try_get("provider_account_id")?)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        course_id: row
            .try_get::<Option<&str>, _>("course_id")?
            .map(CourseId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        remote_id: row.try_get("remote_id")?,
        source_type: decode_source_type(row.try_get("source_type")?)?,
        assessment_class: decode_assessment_class(row.try_get("assessment_class")?)?,
        title: row.try_get("title")?,
        remote_state: decode_remote_state(row.try_get("remote_state")?)?,
        orchestration_state: decode_orchestration_state(row.try_get("orchestration_state")?)?,
        opens_at: decode_optional_timestamp(row.try_get("opens_at")?)?,
        due_at: decode_optional_timestamp(row.try_get("due_at")?)?,
        closes_at: decode_optional_timestamp(row.try_get("closes_at")?)?,
        discovered_at: decode_timestamp(row.try_get("discovered_at")?)?,
        updated_at: decode_timestamp(row.try_get("updated_at")?)?,
        latest_snapshot_id: row
            .try_get::<Option<&str>, _>("latest_snapshot_id")?
            .map(TaskSnapshotId::from_str)
            .transpose()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?,
        capabilities: serde_json::from_str(row.try_get("capabilities_json")?)?,
    })
}

fn decode_source_type(value: &str) -> Result<SourceType, StorageError> {
    decode_enum(value)
}

fn decode_assessment_class(value: &str) -> Result<AssessmentClass, StorageError> {
    decode_enum(value)
}

fn decode_remote_state(value: &str) -> Result<RemoteState, StorageError> {
    decode_enum(value)
}

fn decode_orchestration_state(value: &str) -> Result<OrchestrationState, StorageError> {
    decode_enum(value)
}

fn decode_enum<T>(value: &str) -> Result<T, StorageError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(StorageError::Serialization)
}

fn decode_optional_timestamp(value: Option<&str>) -> Result<Option<Timestamp>, StorageError> {
    value.map(decode_timestamp).transpose()
}

fn decode_timestamp(value: &str) -> Result<Timestamp, StorageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StorageError::InvalidData(error.to_string()))
}

#[cfg(test)]
mod tests {
    use chrono::SecondsFormat;

    use super::*;

    #[tokio::test]
    async fn task_queries_are_owner_scoped_filtered_and_paginated() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner = UserId::new();
        let other_owner = UserId::new();
        insert_user(&database, owner, "owner").await;
        insert_user(&database, other_owner, "other").await;
        let account = insert_account(&database, owner, "provider-alpha").await;
        let second_account = insert_account(&database, owner, "provider-beta").await;
        let other_account = insert_account(&database, other_owner, "provider-alpha").await;
        let first = insert_task(&database, account, "first", 1).await;
        let second = insert_task(&database, second_account, "second", 2).await;
        let other = insert_task(&database, other_account, "other", 3).await;
        let repository = SqliteTaskQueryRepository::new(database);

        let page = repository
            .list_owned_tasks(owner, None, 1, 0)
            .await
            .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, second);
        let filtered = repository
            .list_owned_tasks(owner, Some(account), 50, 0)
            .await
            .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].id, first);
        assert!(
            repository
                .find_owned_task(owner, other)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .find_owned_task(other_owner, other)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            repository
                .find_runtime_task_by_remote_identity(account, "first")
                .await
                .unwrap()
                .unwrap()
                .id,
            first
        );
        assert!(
            repository
                .find_runtime_task_by_remote_identity(second_account, "first")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .find_runtime_task_by_remote_identity(account, " first")
                .await
                .is_err()
        );
    }

    async fn insert_user(database: &Database, user_id: UserId, username: &str) {
        let now = timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, updated_at) \
             VALUES (?, ?, '$argon2id$test', 'active', '[\"user\"]', '[]', ?, ?)",
        )
        .bind(user_id.to_string())
        .bind(username)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
    }

    async fn insert_account(
        database: &Database,
        owner_id: UserId,
        provider_id: &str,
    ) -> ProviderAccountId {
        let id = ProviderAccountId::new();
        let now = timestamp(Utc::now());
        sqlx::query(
            "INSERT INTO provider_accounts \
             (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, '{\"state\":\"idle\"}', ?, ?)",
        )
        .bind(id.to_string())
        .bind(owner_id.to_string())
        .bind(provider_id)
        .bind(provider_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        id
    }

    async fn insert_task(
        database: &Database,
        account_id: ProviderAccountId,
        remote_id: &str,
        age_seconds: i64,
    ) -> TaskId {
        let id = TaskId::new();
        let now = timestamp(Utc::now() + chrono::Duration::seconds(age_seconds));
        sqlx::query(
            "INSERT INTO tasks \
             (id, provider_account_id, remote_id, remote_fingerprint, source_type, \
              assessment_class, title, remote_state, orchestration_state, discovered_at, \
              updated_at, capabilities_json) \
             VALUES (?, ?, ?, ?, 'work', 'routine', ?, 'pending', 'ready', ?, ?, \
                     '[\"progress_read\"]')",
        )
        .bind(id.to_string())
        .bind(account_id.to_string())
        .bind(remote_id)
        .bind(format!("fingerprint-{remote_id}"))
        .bind(remote_id)
        .bind(&now)
        .bind(&now)
        .execute(database.pool())
        .await
        .unwrap();
        id
    }

    fn timestamp(value: Timestamp) -> String {
        value.to_rfc3339_opts(SecondsFormat::Nanos, true)
    }
}
