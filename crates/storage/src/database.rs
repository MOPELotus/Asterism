use std::{str::FromStr, time::Duration};

use asterism_secrets::SecretStoreError;
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
    #[error("secret storage error: {0}")]
    Secret(#[from] SecretStoreError),
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
    #[error("batch execution worker no longer owns both the scheduler claim and batch lease")]
    BatchExecutionClaimLost,
    #[error("batch execution state conflicts with the requested worker transition")]
    BatchExecutionStateConflict,
    #[error("batch execution attempt is missing, finished, or belongs to another parent")]
    BatchExecutionAttemptNotActive,
    #[error("course enrollment Attempt state conflicts with the requested transition")]
    CourseEnrollmentStateConflict,
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
    use std::borrow::Cow;

    use sqlx::Row;

    use super::*;

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fresh-database check keeps the complete cross-migration schema inventory visible"
    )]
    async fn all_migrations_apply_to_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        database.health_check().await.unwrap();

        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(migration_count, 99);

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

        let invocation_columns = sqlx::query("PRAGMA table_info(execution_invocation_drafts)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let invocation_names = invocation_columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert!(invocation_names.contains(&"private_input_secret_blob_id".to_owned()));
        assert!(invocation_names.contains(&"claimed_execution_id".to_owned()));
        assert!(!invocation_names.iter().any(|name| name == "private_input"));

        let snapshot_columns = sqlx::query("PRAGMA table_info(question_snapshots)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        assert!(
            snapshot_columns
                .iter()
                .any(|row| row.get::<String, _>("name") == "group_count")
        );

        let group_columns = sqlx::query("PRAGMA table_info(question_snapshot_groups)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let group_names = group_columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert_eq!(
            group_names,
            [
                "snapshot_id",
                "group_id",
                "ordinal",
                "remote_group_id",
                "group_json",
            ]
        );

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

        let protocol_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' \
             AND name IN ('protocol_observations', 'protocol_observation_occurrences')",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(protocol_tables, 2);

        let checkpoint_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ( \
                'course_enrollment_drafts', 'course_enrollment_attempts', \
                'batch_execution_public_inputs', 'batch_execution_materialization_bindings', \
                'execution_mutation_stage_outputs' \
             )",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(checkpoint_tables, 5);

        let question_operation_artifact_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ( \
                'question_read_operation_recovery_artifacts', \
                'question_read_operation_results', \
                'question_session_operation_recovery_artifacts', \
                'question_session_operation_results' \
             )",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(question_operation_artifact_tables, 4);
    }

    #[tokio::test]
    async fn retry_deadline_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let columns = sqlx::query("PRAGMA table_info(execution_atomic_mutations)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        assert!(
            columns
                .iter()
                .any(|row| row.get::<String, _>("name") == "retry_not_before")
        );
    }

    #[tokio::test]
    async fn parent_batch_snapshot_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let columns = sqlx::query("PRAGMA table_info(execution_parent_batch_snapshots)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let names = columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "execution_id",
                "execution_attempt_id",
                "provider_id",
                "authority_type",
                "authority_digest",
                "authority_secret_blob_id",
                "batch_type",
                "batch_digest",
                "batch_secret_blob_id",
                "bound_at",
            ]
        );
    }

    #[tokio::test]
    async fn batch_execution_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ( \
                'batch_executions', 'batch_execution_attempts', 'batch_execution_leases' \
             )",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(tables, 3);
        let course_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' \
             AND name = 'idx_courses_account_binding'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(course_indexes, 1);
    }

    #[tokio::test]
    async fn batch_parent_snapshot_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let columns = sqlx::query("PRAGMA table_info(batch_execution_parent_snapshots)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let names = columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "batch_execution_id",
                "batch_execution_attempt_id",
                "provider_id",
                "authority_type",
                "authority_digest",
                "authority_secret_blob_id",
                "batch_type",
                "batch_digest",
                "batch_secret_blob_id",
                "bound_at",
            ]
        );
    }

    #[tokio::test]
    async fn batch_planning_input_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let columns = sqlx::query("PRAGMA table_info(batch_execution_planning_inputs)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        let names = columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "batch_execution_id",
                "provider_id",
                "input_type",
                "input_digest",
                "secret_blob_id",
                "bound_at",
            ]
        );
    }

    #[tokio::test]
    async fn batch_child_plan_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ( \
                'batch_execution_child_plans', 'batch_execution_child_plan_phases' \
             )",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(tables, 2);
    }

    #[tokio::test]
    async fn batch_runtime_settings_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let columns = sqlx::query("PRAGMA table_info(batch_execution_runtime_settings)")
            .fetch_all(database.pool())
            .await
            .unwrap();
        assert_eq!(columns.len(), 10);
    }

    #[tokio::test]
    async fn batch_child_execution_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'batch_execution_child_executions'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(tables, 1);
    }

    #[tokio::test]
    async fn batch_child_activation_migration_is_present_on_a_fresh_database() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'batch_execution_child_activations'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(tables, 1);
    }

    #[tokio::test]
    async fn acceptance_short_circuit_migration_preserves_sequence_records() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        let prior = sqlx::migrate::Migrator {
            migrations: Cow::Owned(
                MIGRATOR
                    .iter()
                    .filter(|migration| migration.version <= 70)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        let mut connection = database.pool().acquire().await.unwrap();
        prior.run_direct(&mut *connection).await.unwrap();

        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO execution_atomic_mutation_sequence_plans (
                execution_id, execution_attempt_id, scheduler_job_id, worker_id,
                plan_digest, artifact_digest, sequence_type, phase_count, prepared_at
             ) VALUES ('execution', 'attempt', 'job', 'worker', zeroblob(32),
                zeroblob(32), 'test.sequence.v1', 1, '2026-08-15T00:00:00Z')",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_atomic_mutation_sequence_phases (
                execution_id, execution_attempt_id, position, operation_type,
                minimum_occurrences, maximum_occurrences,
                stop_repeating_after_rejection, advance_condition,
                required_observation_type
             ) VALUES ('execution', 'attempt', 1, 'test.operation', 1, 2, 0,
                'accepted_maximum_reached', 'test.observation')",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_atomic_mutation_sequence_observations (
                execution_id, execution_attempt_id, phase_position,
                observation_type, observation_digest, observed_at
             ) VALUES ('execution', 'attempt', 1, 'test.observation',
                zeroblob(32), '2026-08-15T00:00:01Z')",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .unwrap();

        MIGRATOR.run_direct(&mut *connection).await.unwrap();

        let preserved: (String, i64, String) = sqlx::query_as(
            "SELECT phase.advance_condition, phase.maximum_occurrences,
                    observation.observation_type
             FROM execution_atomic_mutation_sequence_phases AS phase
             JOIN execution_atomic_mutation_sequence_observations AS observation
               ON observation.execution_id = phase.execution_id
              AND observation.execution_attempt_id = phase.execution_attempt_id
              AND observation.phase_position = phase.position",
        )
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        assert_eq!(
            preserved,
            (
                "accepted_maximum_reached".to_owned(),
                2,
                "test.observation".to_owned(),
            )
        );
        sqlx::query(
            "UPDATE execution_atomic_mutation_sequence_phases
             SET advance_condition = 'accepted_or_maximum_reached'",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn atomic_mutation_verification_migration_preserves_legacy_receipts() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            r"
            CREATE TABLE scheduled_jobs (id TEXT PRIMARY KEY NOT NULL) STRICT;
            CREATE TABLE execution_attempts (
                execution_id TEXT NOT NULL,
                id TEXT NOT NULL,
                PRIMARY KEY (execution_id, id)
            ) STRICT;
            CREATE TABLE execution_atomic_mutations (
                execution_id TEXT NOT NULL,
                execution_attempt_id TEXT NOT NULL,
                ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 100000),
                scheduler_job_id TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE RESTRICT,
                worker_id TEXT NOT NULL CHECK (length(worker_id) BETWEEN 1 AND 256),
                operation_type TEXT NOT NULL CHECK (length(operation_type) BETWEEN 1 AND 96),
                request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
                response_digest BLOB CHECK (
                    response_digest IS NULL OR length(response_digest) = 32
                ),
                accepted INTEGER CHECK (accepted IS NULL OR accepted IN (0, 1)),
                issued_at TEXT NOT NULL,
                received_at TEXT,
                PRIMARY KEY (execution_id, execution_attempt_id, ordinal),
                FOREIGN KEY (execution_id, execution_attempt_id)
                    REFERENCES execution_attempts(execution_id, id) ON DELETE RESTRICT,
                CHECK (
                    (response_digest IS NULL AND accepted IS NULL AND received_at IS NULL)
                    OR (response_digest IS NOT NULL AND accepted IS NOT NULL
                        AND received_at IS NOT NULL AND received_at >= issued_at)
                )
            ) STRICT;
            CREATE INDEX idx_execution_atomic_mutations_sequence
                ON execution_atomic_mutations (execution_id, execution_attempt_id, ordinal);
            INSERT INTO scheduled_jobs VALUES ('job-a');
            INSERT INTO execution_attempts VALUES ('execution-a', 'attempt-a');
            INSERT INTO execution_atomic_mutations VALUES (
                'execution-a', 'attempt-a', 1, 'job-a', 'worker-a',
                'provider-a.save', randomblob(32), randomblob(32), 1,
                '2026-08-15T10:00:00+00:00', '2026-08-15T10:00:01+00:00'
            );
            ",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/067_execution_atomic_mutation_verification.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/072_execution_mutation_retry_deadline.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();
        let row = sqlx::query(
            "SELECT ordinal, accepted, verification_digest, verified, verified_at, \
                    retry_not_before \
             FROM execution_atomic_mutations",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("ordinal"), 1);
        assert!(row.get::<bool, _>("accepted"));
        assert_eq!(row.get::<Option<Vec<u8>>, _>("verification_digest"), None);
        assert_eq!(row.get::<Option<bool>, _>("verified"), None);
        assert_eq!(row.get::<Option<String>, _>("verified_at"), None);
        assert_eq!(row.get::<Option<String>, _>("retry_not_before"), None);
    }

    #[tokio::test]
    async fn authenticated_account_backfill_creates_one_typed_initial_harvest() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        database.migrate().await.unwrap();
        let owner_id = "018f0000-0000-7000-8000-000000000001";
        let account_id = "018f0000-0000-7000-8000-000000000002";
        let idle_account_id = "018f0000-0000-7000-8000-000000000003";
        let bound_at = "2026-08-15T10:00:00+00:00";
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, status, roles_json, permissions_json, created_at, \
              updated_at) VALUES (?, 'harvest-backfill-owner', '$argon2id$test', 'active', \
              '[\"user\"]', '[]', ?, ?)",
        )
        .bind(owner_id)
        .bind(bound_at)
        .bind(bound_at)
        .execute(database.pool())
        .await
        .unwrap();
        for (id, state) in [
            (account_id, "\"authenticated\""),
            (idle_account_id, "\"idle\""),
        ] {
            sqlx::query(
                "INSERT INTO provider_accounts \
                 (id, owner_user_id, provider_id, display_name, auth_state_json, created_at, \
                  updated_at) VALUES (?, ?, 'provider-alpha', 'Primary', ?, ?, ?)",
            )
            .bind(id)
            .bind(owner_id)
            .bind(state)
            .bind(bound_at)
            .bind(bound_at)
            .execute(database.pool())
            .await
            .unwrap();
        }

        sqlx::raw_sql(include_str!(
            "../../../migrations/065_answer_bootstrap_harvest_backfill.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();
        let row: (String, String, String, String, String) = sqlx::query_as(
            "SELECT harvest.owner_user_id, harvest.provider_id, \
                    harvest.provider_account_id, harvest.created_at, job.payload_json \
             FROM answer_bootstrap_harvests AS harvest \
             INNER JOIN scheduled_jobs AS job ON job.id = harvest.schedule_id",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(row.0, owner_id);
        assert_eq!(row.1, "provider-alpha");
        assert_eq!(row.2, account_id);
        assert_eq!(row.3, bound_at);
        assert!(matches!(
            serde_json::from_str::<asterism_scheduler::ScheduledJobKind>(&row.4).unwrap(),
            asterism_scheduler::ScheduledJobKind::AnswerBootstrapHarvest {
                provider_account_id,
                generation: 1,
                ..
            } if provider_account_id.to_string() == account_id
        ));

        sqlx::raw_sql(include_str!(
            "../../../migrations/065_answer_bootstrap_harvest_backfill.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT \
                (SELECT COUNT(*) FROM answer_bootstrap_harvests), \
                (SELECT COUNT(*) FROM scheduled_jobs \
                 WHERE job_kind = 'answer_bootstrap_harvest')",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(counts, (1, 1));
    }

    #[tokio::test]
    async fn answer_history_task_fact_migration_preserves_legacy_imports_as_unknown() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL) STRICT;\
             CREATE TABLE provider_accounts (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 provider_id TEXT NOT NULL,\
                 UNIQUE (id, provider_id)\
             ) STRICT;\
             CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL) STRICT;\
             CREATE TABLE question_snapshots (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 task_id TEXT NOT NULL,\
                 provider_id TEXT NOT NULL,\
                 UNIQUE (id, task_id, provider_id)\
             ) STRICT;\
             CREATE TABLE answer_history_imports (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,\
                 provider_id TEXT NOT NULL,\
                 provider_account_id TEXT NOT NULL,\
                 task_id TEXT NOT NULL,\
                 provider_attempt_digest BLOB NOT NULL CHECK (length(provider_attempt_digest) = 32),\
                 result_digest BLOB NOT NULL CHECK (length(result_digest) = 32),\
                 content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),\
                 question_snapshot_id TEXT NOT NULL,\
                 candidate_count INTEGER NOT NULL CHECK (candidate_count > 0),\
                 evidence_count INTEGER NOT NULL CHECK (evidence_count >= 0),\
                 imported_at TEXT NOT NULL,\
                 UNIQUE (provider_account_id, task_id, provider_attempt_digest, result_digest),\
                 FOREIGN KEY (provider_account_id, provider_id) \
                     REFERENCES provider_accounts(id, provider_id) ON DELETE CASCADE,\
                 FOREIGN KEY (question_snapshot_id, task_id, provider_id) \
                     REFERENCES question_snapshots(id, task_id, provider_id) ON DELETE CASCADE\
             ) STRICT;\
             CREATE INDEX idx_answer_history_imports_owner_task \
                 ON answer_history_imports (owner_user_id, task_id, imported_at DESC, id);\
             INSERT INTO users VALUES ('owner-a');\
             INSERT INTO provider_accounts VALUES ('account-a', 'provider-a');\
             INSERT INTO tasks VALUES ('task-a');\
             INSERT INTO question_snapshots VALUES ('snapshot-a', 'task-a', 'provider-a');\
             INSERT INTO answer_history_imports VALUES (\
                 'import-a', 'owner-a', 'provider-a', 'account-a', 'task-a',\
                 zeroblob(32), randomblob(32), randomblob(32), 'snapshot-a', 1, 0,\
                 '2026-08-15T10:00:00+00:00'\
             );",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/066_answer_history_task_facts.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();
        let row: (Option<String>, Option<String>, String, String, String) = sqlx::query_as(
            "SELECT score_json, retake_json, provenance_sanitized_json, observed_at, imported_at \
             FROM answer_history_imports WHERE id = 'import-a'",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(row.0, None);
        assert_eq!(row.1, None);
        assert_eq!(row.2, "{}");
        assert_eq!(row.3, row.4);
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
    async fn strict_completion_observation_migration_preserves_attempt_cardinality() {
        let database = Database::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE executions (id TEXT PRIMARY KEY NOT NULL) STRICT;\
             CREATE TABLE execution_attempts (\
                 id TEXT PRIMARY KEY NOT NULL,\
                 execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE\
             ) STRICT;\
             CREATE TABLE strict_completion_workflows (id TEXT PRIMARY KEY NOT NULL) STRICT;\
             CREATE TABLE strict_completion_execution_observations (\
                 execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(id) ON DELETE CASCADE,\
                 execution_attempt_id TEXT NOT NULL UNIQUE REFERENCES execution_attempts(id) ON DELETE CASCADE,\
                 workflow_id TEXT NOT NULL REFERENCES strict_completion_workflows(id) ON DELETE CASCADE,\
                 workflow_attempt_no INTEGER,\
                 completion_outcome TEXT,\
                 diagnosis TEXT,\
                 observed_at TEXT NOT NULL\
             ) STRICT;\
             CREATE INDEX idx_strict_completion_execution_workflow \
                 ON strict_completion_execution_observations (workflow_id, observed_at, execution_id);\
             INSERT INTO executions (id) VALUES ('execution-a');\
             INSERT INTO execution_attempts (id, execution_id) VALUES ('attempt-a', 'execution-a');\
             INSERT INTO strict_completion_workflows (id) VALUES ('workflow-a');\
             INSERT INTO strict_completion_execution_observations (\
                 execution_id, execution_attempt_id, workflow_id, workflow_attempt_no,\
                 completion_outcome, diagnosis, observed_at\
             ) VALUES (\
                 'execution-a', 'attempt-a', 'workflow-a', 1, NULL,\
                 'duration_insufficient', '2026-08-01T12:00:00.000000000Z'\
             );",
        )
        .execute(database.pool())
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/062_strict_completion_attempt_observations.sql"
        ))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::raw_sql(
            "INSERT INTO execution_attempts (id, execution_id) VALUES ('attempt-b', 'execution-a');\
             INSERT INTO strict_completion_execution_observations (\
                 execution_id, execution_attempt_id, workflow_id, workflow_attempt_no,\
                 completion_outcome, diagnosis, observed_at\
             ) VALUES (\
                 'execution-a', 'attempt-b', 'workflow-a', 2, 'completed', NULL,\
                 '2026-08-01T12:01:00.000000000Z'\
             );",
        )
        .execute(database.pool())
        .await
        .unwrap();

        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT execution_attempt_id, completion_outcome, diagnosis \
             FROM strict_completion_execution_observations ORDER BY execution_attempt_id",
        )
        .fetch_all(database.pool())
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "attempt-a".to_owned(),
                    None,
                    Some("duration_insufficient".to_owned())
                ),
                ("attempt-b".to_owned(), Some("completed".to_owned()), None),
            ]
        );
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
