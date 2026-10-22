//! SQLite persistence for CalRelay. Business transactions belong to later Issues.

use std::path::Path;
use std::time::Duration;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("cannot open SQLite database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("cannot migrate SQLite database: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}

/// A migrated, file-backed SQLite pool. Opening never deletes or replaces an existing file.
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    /// Open/create a database in an existing directory and apply embedded migrations.
    ///
    /// Failure closes the new pool and returns the underlying connection or migration error.
    /// Unknown or changed migration history is rejected; no automatic repair is attempted.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, OpenError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        if let Err(error) = MIGRATIONS.run(&pool).await {
            pool.close().await;
            return Err(error.into());
        }
        Ok(Self { pool })
    }

    /// For storage repository implementations; application/domain code must not issue SQL.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Drain checked-out connections and finish SQLite worker threads before filesystem cleanup.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
