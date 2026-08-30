//! Every mutation, login, logout and failed login writes one row. Web IPs are pruned after 90
//! days (`prune`), the rows themselves are kept.

use anyhow::Result;
use sqlx::SqlitePool;

use crate::db::now;

pub async fn log(
    db: &SqlitePool,
    actor: Option<i64>,
    ip: Option<&str>,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    if let Err(e) = sqlx::query("INSERT INTO audit (at, actor_user_id, ip, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(now())
        .bind(actor)
        .bind(ip)
        .bind(action)
        .bind(target)
        .bind(detail)
        .execute(db)
        .await
    {
        tracing::error!(error = %e, action, "audit write failed");
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct Entry {
    pub id: i64,
    pub at: i64,
    pub actor: Option<String>,
    pub ip: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

pub async fn recent(db: &SqlitePool, before: Option<i64>, limit: i64) -> Result<Vec<Entry>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.at, u.username AS actor, a.ip, a.action, a.target, a.detail FROM audit a \
         LEFT JOIN users u ON u.id = a.actor_user_id WHERE a.id < ? ORDER BY a.id DESC LIMIT ?",
    )
    .bind(before.unwrap_or(i64::MAX))
    .bind(limit)
    .fetch_all(db)
    .await?)
}

/// Drop IPs older than 90 days from audit and login_attempts.
pub async fn prune(db: &SqlitePool) -> Result<()> {
    let cutoff = now() - 90 * 24 * 3600;
    sqlx::query("UPDATE audit SET ip = NULL WHERE at < ? AND ip IS NOT NULL")
        .bind(cutoff)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM login_attempts WHERE at < ?")
        .bind(cutoff)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
        .bind(now())
        .execute(db)
        .await?;
    Ok(())
}
