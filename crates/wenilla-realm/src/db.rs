//! The service's own sqlite (WAL, migrated at boot) and the `meta` key/value table that holds
//! setup state and the encrypted SOAP credentials.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

pub async fn open_sqlite(path: &Path) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("migrating")?;
    Ok(pool)
}

/// Seconds since the epoch — every timestamp in the schema.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub async fn meta_get(db: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM meta WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.0))
}

pub async fn meta_set(db: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn meta_del(db: &SqlitePool, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM meta WHERE key = ?")
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn setup_complete(db: &SqlitePool) -> Result<bool> {
    Ok(meta_get(db, "setup_complete").await?.as_deref() == Some("1"))
}
