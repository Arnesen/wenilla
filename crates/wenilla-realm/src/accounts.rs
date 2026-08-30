//! Game accounts, owned by the service: a player never sees their game password. Creation and
//! every change go through the console (`account create` computes the SRP verifier server-side;
//! there is no offline path worth maintaining), the password is kept encrypted in
//! `game_accounts` so `/api/play` can hand it to the browser client.

use anyhow::{anyhow, Context, Result};
use sqlx::SqlitePool;

use crate::db::now;
use crate::secrets::{random_string, Keyring, ALNUM_UPPER};
use crate::soap::{self, Client, SoapError};

/// cmangos `MAX_ACCOUNT_STR`: both the account name and the password are capped at 16.
pub const MAX_ACCOUNT_STR: usize = 16;

pub fn game_username(user_id: i64) -> String {
    format!("WR{user_id:06}")
}

#[derive(Debug, sqlx::FromRow)]
pub struct GameAccount {
    pub user_id: i64,
    pub game_username: String,
    pub password_enc: Vec<u8>,
    pub nonce: Vec<u8>,
}

pub async fn get(db: &SqlitePool, user_id: i64) -> Result<Option<GameAccount>> {
    Ok(sqlx::query_as(
        "SELECT user_id, game_username, password_enc, nonce FROM game_accounts WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

/// Create the game account for a web user (idempotent: an existing row is returned as-is).
pub async fn provision(
    db: &SqlitePool,
    soap: &Client,
    keys: &Keyring,
    user_id: i64,
) -> Result<GameAccount> {
    if let Some(existing) = get(db, user_id).await? {
        return Ok(existing);
    }
    let name = game_username(user_id);
    let pass = random_string(ALNUM_UPPER, MAX_ACCOUNT_STR);
    match soap.exec(&format!("account create {name} {pass}")).await {
        Ok(_) => {}
        // A row we lost track of (restored sqlite, say): take it over by resetting its password.
        Err(SoapError::Fault(m)) if m.contains("already exist") => {
            soap.exec(&format!("account set password {name} {pass} {pass}"))
                .await?;
        }
        Err(e) => return Err(e.into()),
    }
    let (enc, nonce) = keys.encrypt(pass.as_bytes())?;
    sqlx::query("INSERT INTO game_accounts (user_id, game_username, password_enc, nonce, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(user_id)
        .bind(&name)
        .bind(&enc)
        .bind(&nonce)
        .bind(now())
        .execute(db)
        .await?;
    Ok(GameAccount {
        user_id,
        game_username: name,
        password_enc: enc,
        nonce,
    })
}

pub async fn rotate_password(
    db: &SqlitePool,
    soap: &Client,
    keys: &Keyring,
    user_id: i64,
) -> Result<()> {
    let acct = get(db, user_id)
        .await?
        .ok_or_else(|| anyhow!("no game account"))?;
    let pass = random_string(ALNUM_UPPER, MAX_ACCOUNT_STR);
    soap.exec(&format!(
        "account set password {} {pass} {pass}",
        acct.game_username
    ))
    .await?;
    let (enc, nonce) = keys.encrypt(pass.as_bytes())?;
    sqlx::query(
        "UPDATE game_accounts SET password_enc = ?, nonce = ?, rotated_at = ? WHERE user_id = ?",
    )
    .bind(enc)
    .bind(nonce)
    .bind(now())
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}

pub fn password(keys: &Keyring, acct: &GameAccount) -> Result<String> {
    keys.decrypt_string(&acct.password_enc, &acct.nonce)
        .context("decrypting game password")
}

/// `duration_secs = None` → permanent. The console keeps one token of reason, so it gets a
/// marker and the real reason lives in `bans`.
pub async fn ban(
    db: &SqlitePool,
    soap: &Client,
    user_id: i64,
    by: Option<i64>,
    duration_secs: Option<i64>,
    reason: &str,
) -> Result<()> {
    let acct = get(db, user_id)
        .await?
        .ok_or_else(|| anyhow!("no game account"))?;
    let bantime = match duration_secs {
        None => "-1".to_string(),
        Some(s) if s > 0 => format!("{s}s"),
        Some(_) => return Err(anyhow!("duration must be positive")),
    };
    soap.exec(&format!("ban account {} {bantime} web", acct.game_username))
        .await?;
    let reason: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(255)
        .collect();
    sqlx::query("INSERT INTO bans (game_username, by_user_id, reason, duration_secs, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&acct.game_username)
        .bind(by)
        .bind(if reason.is_empty() { "-".to_string() } else { reason })
        .bind(duration_secs)
        .bind(now())
        .execute(db)
        .await?;
    Ok(())
}

pub async fn unban(db: &SqlitePool, soap: &Client, user_id: i64) -> Result<()> {
    let acct = get(db, user_id)
        .await?
        .ok_or_else(|| anyhow!("no game account"))?;
    soap.exec(&format!("unban account {}", acct.game_username))
        .await?;
    sqlx::query("UPDATE bans SET lifted_at = ? WHERE game_username = ? AND lifted_at IS NULL")
        .bind(now())
        .bind(&acct.game_username)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn kick(soap: &Client, character: &str) -> Result<String> {
    let c = soap::arg(character, 12)?;
    Ok(soap.exec(&format!("kick {c}")).await?)
}

/// Delete the game account and all its characters, then the web user (cascades).
pub async fn delete_user(db: &SqlitePool, soap: &Client, user_id: i64) -> Result<()> {
    if let Some(acct) = get(db, user_id).await? {
        match soap
            .exec(&format!("account delete {}", acct.game_username))
            .await
        {
            Ok(_) => {}
            Err(SoapError::Fault(m)) if m.contains("not exist") => {}
            Err(e) => return Err(e.into()),
        }
    }
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// First-run bootstrap: the classic-db seed ships `ADMINISTRATOR/ADMINISTRATOR` (gmlevel 3).
/// Create the service's own console account, switch to it, and re-password every seeded
/// account so the defaults stop working. Returns the new `(user, pass)`.
pub async fn bootstrap_console(soap: &Client) -> Result<(String, String)> {
    let user = "WRSOAP".to_string();
    let pass = random_string(ALNUM_UPPER, MAX_ACCOUNT_STR);
    match soap.exec(&format!("account create {user} {pass}")).await {
        Ok(_) => {}
        Err(SoapError::Fault(m)) if m.contains("already exist") => {
            soap.exec(&format!("account set password {user} {pass} {pass}"))
                .await?;
        }
        Err(e) => return Err(e.into()),
    }
    soap.exec(&format!("account set gmlevel {user} 3 -1"))
        .await?;
    soap.set_credentials(&user, &pass).await;
    soap.exec("server info")
        .await
        .context("the new console account does not work")?;
    for seeded in ["ADMINISTRATOR", "GAMEMASTER", "MODERATOR", "PLAYER"] {
        let p = random_string(ALNUM_UPPER, MAX_ACCOUNT_STR);
        if let Err(e) = soap
            .exec(&format!("account set password {seeded} {p} {p}"))
            .await
        {
            tracing::warn!(account = seeded, error = %e, "could not re-password seeded account");
        }
    }
    Ok((user, pass))
}
