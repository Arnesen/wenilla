//! Read-only views over the game databases (MariaDB), for the panel. The service's DB user has
//! SELECT on `classicrealmd` and `classiccharacters` and UPDATE on `classicrealmd.realmlist`
//! only. For an *online* character the columns here lag (state lives in mangosd until save);
//! `pinfo` over SOAP is the truth when it matters.

use anyhow::Result;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;

pub async fn connect(url: &str) -> Result<MySqlPool> {
    Ok(MySqlPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_lazy(url)?)
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct Character {
    pub guid: i64,
    pub name: String,
    pub race: i64,
    pub class: i64,
    pub level: i64,
    pub online: i64,
    pub totaltime: i64,
    pub zone: i64,
}

pub async fn characters(db: &MySqlPool, game_username: &str) -> Result<Vec<Character>> {
    Ok(sqlx::query_as(
        // Every integer column cmangos ships is UNSIGNED (`int(11) unsigned`, `tinyint(3)
        // unsigned`), and sqlx refuses to decode one into a signed Rust field ("mismatched
        // types … not compatible with SQL type INT UNSIGNED") — which the dashboard's
        // unwrap_or_default then dressed as "no characters yet" for every player (live bug,
        // 2026-09-01; tests/realmdb_live.rs is the regression). CAST AS SIGNED at the query is
        // the same answer online_count() below already used for its SUMs.
        "SELECT CAST(c.guid AS SIGNED) AS guid, c.name, CAST(c.race AS SIGNED) AS race, \
         CAST(c.class AS SIGNED) AS class, CAST(c.level AS SIGNED) AS level, \
         CAST(c.online AS SIGNED) AS online, CAST(c.totaltime AS SIGNED) AS totaltime, \
         CAST(c.zone AS SIGNED) AS zone \
         FROM classiccharacters.characters c JOIN classicrealmd.account a ON a.id = c.account \
         WHERE a.username = ? ORDER BY c.level DESC, c.name",
    )
    .bind(game_username)
    .fetch_all(db)
    .await?)
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct OnlineRow {
    pub name: String,
    pub level: i64,
    pub race: i64,
    pub class: i64,
    pub zone: i64,
    pub account: String,
}

/// Every online character except the random bots (their accounts are `RNDBOT…`).
pub async fn online(db: &MySqlPool) -> Result<Vec<OnlineRow>> {
    Ok(sqlx::query_as(
        // Same UNSIGNED cast story as characters() above.
        "SELECT c.name, CAST(c.level AS SIGNED) AS level, CAST(c.race AS SIGNED) AS race, \
         CAST(c.class AS SIGNED) AS class, CAST(c.zone AS SIGNED) AS zone, a.username AS account \
         FROM classiccharacters.characters c JOIN classicrealmd.account a ON a.id = c.account \
         WHERE c.online = 1 AND a.username NOT LIKE 'RNDBOT%' ORDER BY c.name",
    )
    .fetch_all(db)
    .await?)
}

pub async fn online_count(db: &MySqlPool) -> Result<(i64, i64)> {
    let (players, bots): (i64, i64) = sqlx::query_as(
        "SELECT CAST(COALESCE(SUM(a.username NOT LIKE 'RNDBOT%'), 0) AS SIGNED), CAST(COALESCE(SUM(a.username LIKE 'RNDBOT%'), 0) AS SIGNED) \
         FROM classiccharacters.characters c JOIN classicrealmd.account a ON a.id = c.account WHERE c.online = 1",
    )
    .fetch_one(db)
    .await?;
    Ok((players, bots))
}

#[derive(Debug, sqlx::FromRow)]
pub struct ActiveBan {
    pub username: String,
    pub banned_at: i64,
    pub expires_at: i64,
    pub reason: String,
}

pub async fn active_bans(db: &MySqlPool) -> Result<Vec<ActiveBan>> {
    Ok(sqlx::query_as(
        // Defensive twin of the casts above: these columns are signed in this cmangos vintage, but
        // the schema has varied and a cast on a signed column costs nothing.
        "SELECT a.username, CAST(b.banned_at AS SIGNED) AS banned_at, \
         CAST(b.expires_at AS SIGNED) AS expires_at, b.reason FROM classicrealmd.account_banned b \
         JOIN classicrealmd.account a ON a.id = b.account_id \
         WHERE b.active = 1 AND (b.expires_at = b.banned_at OR b.expires_at > UNIX_TIMESTAMP())",
    )
    .fetch_all(db)
    .await?)
}

/// Only the name: on the web the realm's address is a label (the client dials the page's own
/// origin), and the packaging's `db-init.sh` already wrote address/port/build for the row.
pub async fn set_realm_name(db: &MySqlPool, name: &str) -> Result<()> {
    sqlx::query("UPDATE classicrealmd.realmlist SET name = ? WHERE id = 1")
        .bind(name)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn ping(db: &MySqlPool) -> bool {
    sqlx::query("SELECT 1").execute(db).await.is_ok()
}

pub const RACES: &[(i64, &str)] = &[
    (1, "Human"),
    (2, "Orc"),
    (3, "Dwarf"),
    (4, "Night Elf"),
    (5, "Undead"),
    (6, "Tauren"),
    (7, "Gnome"),
    (8, "Troll"),
];
pub const CLASSES: &[(i64, &str)] = &[
    (1, "Warrior"),
    (2, "Paladin"),
    (3, "Hunter"),
    (4, "Rogue"),
    (5, "Priest"),
    (7, "Shaman"),
    (8, "Mage"),
    (9, "Warlock"),
    (11, "Druid"),
];

pub fn race_name(id: i64) -> &'static str {
    RACES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, n)| *n)
        .unwrap_or("?")
}
pub fn class_name(id: i64) -> &'static str {
    CLASSES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, n)| *n)
        .unwrap_or("?")
}
