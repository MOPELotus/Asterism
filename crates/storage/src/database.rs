use std::{str::FromStr, time::Duration};

use sqlx::{
    Row, SqlitePool,
    migrate::MigrateError,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Clone, Debug)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Opens a database pool with Asterism's `SQLite` safety pragmas.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the URL is invalid or the database cannot
    /// be opened.
    pub async fn connect(database_url: &str) -> Result<Self, StorageError> {
        let in_memory = database_url.contains(":memory:");
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .journal_mode(if in_memory {
                SqliteJournalMode::Memory
            } else {
                SqliteJournalMode::Wal
            })
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePoolOptions::new()
            .max_connections(if in_memory { 1 } else { 5 })
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    /// Applies all embedded migrations in order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if migration metadata or a migration statement
    /// cannot be applied.
    pub async fn migrate(&self) -> Result<(), StorageError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Verifies that the pool can execute a trivial query.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the database is unavailable.
    pub async fn health_check(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Returns the highest successfully applied migration version.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if migration metadata cannot be queried.
    pub async fn schema_version(&self) -> Result<i64, StorageError> {
        let row = sqlx::query("SELECT COALESCE(MAX(version), 0) AS version FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("version")?)
    }

    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("database migration error: {0}")]
    Migration(#[from] MigrateError),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("persisted data is invalid: {0}")]
    InvalidData(String),
    #[error("execution lease is no longer owned by this worker")]
    LeaseLost,
    #[error("execution lease expiry must be in the future")]
    InvalidLeaseExpiry,
    #[error("execution worker no longer owns both the scheduler claim and execution lease")]
    ExecutionClaimLost,
    #[error("execution or task state conflicts with the requested worker transition")]
    ExecutionStateConflict,
    #[error("execution attempt is missing, finished, or belongs to another execution")]
    ExecutionAttemptNotActive,
    #[error("outbox claim is no longer owned by this worker")]
    OutboxClaimLost,
    #[error("outbox claim expiry must be in the future and its batch must be non-zero")]
    InvalidOutboxClaim,
    #[error("available credit is insufficient")]
    InsufficientCredits,
    #[error("credit reservation is missing or no longer active")]
    ReservationNotActive,
    #[error("credit amount cannot be represented by SQLite")]
    CreditAmountOutOfRange,
    #[error("persisted credit balances violate ledger invariants")]
    CreditInvariant,
    #[error("scheduler claim is no longer owned by this worker")]
    SchedulerClaimLost,
    #[error("scheduler claim expiry must be in the future and its batch must be non-zero")]
    InvalidSchedulerClaim,
    #[error("the initial master has already been created")]
    MasterAlreadyInitialized,
}

#[cfg(test)]
mod tests {
    use sqlx::Row;

    use super::*;

    #[tokio::test]
    async fn all_migrations_apply_to_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        database.health_check().await.unwrap();

        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(migration_count, 60);

        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(foreign_keys, 1);

        let columns = sqlx::query("PRAGMA table_info(secret_blobs)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let names: Vec<String> = columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect();
        assert!(names.contains(&"encrypted_data".to_owned()));
        assert!(!names.iter().any(|name| name == "plaintext"));

        let exchange_columns = sqlx::query("PRAGMA table_info(browser_bridge_exchanges)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        assert!(
            exchange_columns
                .iter()
                .any(|row| { row.get::<String, _>("name") == "command_secret_blob_id" })
        );

        let dispatch_columns = sqlx::query("PRAGMA table_info(browser_bridge_command_dispatches)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let dispatch_names = dispatch_columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert_eq!(dispatch_names, ["session_id", "sequence", "dispatched_at"]);

        let result_columns = sqlx::query("PRAGMA table_info(browser_bridge_result_artifacts)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        assert!(
            result_columns
                .iter()
                .any(|row| row.get::<String, _>("name") == "processed_at")
        );

        let workflow_contexts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'browser_bridge_workflow_contexts'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(workflow_contexts, 1);

        let question_sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'question_sessions'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(question_sessions, 1);

        let question_continuations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'question_session_continuations'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(question_continuations, 1);

        let question_read_attempts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'question_read_attempts'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(question_read_attempts, 1);

        let question_read_attempt_continuations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'question_read_attempt_continuations'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(question_read_attempt_continuations, 1);
    }

    #[tokio::test]
    async fn execution_completion_policy_migration_backfills_frozen_defaults() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE executions (id TEXT PRIMARY KEY NOT NULL) STRICT;\
             INSERT INTO executions (id) VALUES ('execution-a');\
             CREATE TABLE execution_runtime_settings (\
                 execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,\
                 provider_id TEXT NOT NULL,\
                 schema_version INTEGER NOT NULL CHECK (schema_version >= 1),\
                 resolved_settings_json TEXT NOT NULL,\
                 sources_json TEXT NOT NULL,\
                 provider_revision INTEGER,\
                 provider_account_revision INTEGER,\
                 task_revision INTEGER,\
                 captured_at TEXT NOT NULL\
             ) STRICT;\
             INSERT INTO execution_runtime_settings (\
                 execution_id, provider_id, schema_version, resolved_settings_json, sources_json, captured_at\
             ) VALUES (\
                 'execution-a', 'provider-a', 1, '{}', '{}',\
                 '2026-08-01T12:34:56.123456789Z'\
             );",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/060_execution_completion_policy.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();

        let policy_json: String = sqlx::query_scalar(
            "SELECT completion_policy_json FROM execution_runtime_settings \
             WHERE execution_id = 'execution-a'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        let policy: asterism_domain::CompletionPolicySnapshot =
            serde_json::from_str(&policy_json).unwrap();
        assert!(policy.strict_completion_enabled);
        assert!(!policy.score_improvement_enabled);
        assert_eq!(policy.strict_attempt_limit, 3);
        assert_eq!(policy.score_improvement_attempt_limit, 1);
        assert_eq!(policy.score_target_millis, 1_000);
        assert_eq!(
            policy.strict_expires_at.unwrap(),
            policy.captured_at + chrono::Duration::days(7)
        );
        assert_eq!(
            policy.score_improvement_expires_at.unwrap(),
            policy.captured_at + chrono::Duration::days(1)
        );
        assert!(policy.formal_retry_requires_confirmation);
        policy.validate().unwrap();

        let completion_column = sqlx::query("PRAGMA table_info(execution_runtime_settings)")
            .fetch_all(database.pool())
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.get::<String, _>("name") == "completion_policy_json")
            .unwrap();
        assert_eq!(completion_column.get::<i64, _>("notnull"), 1);
    }

    #[tokio::test]
    async fn desired_scan_interval_migration_backfills_existing_schedules() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE scan_schedules (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 interval_seconds INTEGER NOT NULL CHECK (interval_seconds > 0)\
             ) STRICT;\
             INSERT INTO scan_schedules (id, interval_seconds) VALUES ('schedule-a', 90);",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/013_scan_schedule_desired_interval.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();

        let desired: i64 = sqlx::query_scalar(
            "SELECT desired_interval_seconds FROM scan_schedules WHERE id = 'schedule-a'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(desired, 90);
    }

    #[tokio::test]
    async fn execution_capability_selection_migration_backfills_existing_rows() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL, capabilities_json TEXT NOT NULL) STRICT;\
             CREATE TABLE executions (id TEXT PRIMARY KEY NOT NULL, task_id TEXT NOT NULL) STRICT;\
             INSERT INTO tasks (id, capabilities_json) VALUES \
                 ('task-a', '[\"progress_read\",\"resource_execution\",\"duration_report\"]');\
             INSERT INTO executions (id, task_id) VALUES ('execution-a', 'task-a');",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/032_execution_requested_capabilities.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();

        let capabilities: String = sqlx::query_scalar(
            "SELECT requested_capabilities_json FROM executions WHERE id = 'execution-a'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&capabilities).unwrap(),
            serde_json::json!(["resource_execution", "duration_report"])
        );
    }

    #[tokio::test]
    async fn secret_version_migration_backfills_existing_blobs() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE secret_blobs (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 encrypted_data BLOB NOT NULL\
             ) STRICT;\
             INSERT INTO secret_blobs (id, encrypted_data) VALUES ('secret-a', X'01');",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!("../../../migrations/014_secret_versions.sql"))
            .execute(database.pool())
            .await
            .unwrap();

        let version: i64 =
            sqlx::query_scalar("SELECT version FROM secret_blobs WHERE id = 'secret-a'")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(version, 1);
    }

    #[tokio::test]
    async fn provider_credential_metadata_migration_backfills_existing_links() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE provider_account_credentials (\
                 provider_account_id TEXT NOT NULL,\
                 secret_blob_id TEXT NOT NULL,\
                 credential_kind TEXT NOT NULL,\
                 created_at TEXT NOT NULL,\
                 PRIMARY KEY (provider_account_id, secret_blob_id)\
             ) STRICT;\
             INSERT INTO provider_account_credentials \
                 (provider_account_id, secret_blob_id, credential_kind, created_at) \
             VALUES ('account-a', 'secret-a', 'provider_cookie', \
                     '2026-08-09T00:00:00.000000000Z');",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/015_provider_credential_metadata.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();

        let row = sqlx::query(
            "SELECT session_kind, acquired_via, expires_at, updated_at \
             FROM provider_account_credentials WHERE provider_account_id = 'account-a'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("session_kind"), "provider_specific");
        assert_eq!(row.get::<String, _>("acquired_via"), "manual_import");
        assert!(row.get::<Option<String>, _>("expires_at").is_none());
        assert_eq!(
            row.get::<String, _>("updated_at"),
            "2026-08-09T00:00:00.000000000Z"
        );
    }

    #[tokio::test]
    async fn execution_idempotency_migration_preserves_existing_rows() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE executions (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 task_id TEXT NOT NULL,\
                 request_source TEXT NOT NULL,\
                 state TEXT NOT NULL,\
                 created_at TEXT NOT NULL\
             ) STRICT;\
             INSERT INTO executions (id, task_id, request_source, state, created_at)\
             VALUES ('execution-a', 'task-a', 'system', 'requested',\
                     '2026-08-09T00:00:00.000000000Z');",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/019_execution_idempotency.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();

        let row = sqlx::query(
            "SELECT idempotency_scope, idempotency_key FROM executions WHERE id = 'execution-a'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(row.get::<Option<String>, _>("idempotency_scope").is_none());
        assert!(row.get::<Option<String>, _>("idempotency_key").is_none());
    }
}
