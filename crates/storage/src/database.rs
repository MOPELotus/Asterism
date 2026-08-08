use std::{str::FromStr, time::Duration};

use sqlx::{
    SqlitePool,
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
        assert_eq!(migration_count, 7);

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
    }
}
