//! Dungeon presets: one action makes a group of level-appropriate, geared characters standing at
//! an instance entrance, and a secret link hands them out. Each slot is a normal player user with
//! its own hidden game account; the link mints sessions for those users, so the play page, the
//! relay and every lock work exactly as they do for an invited player. Anyone holding the link
//! can join, summon the group back to the entrance, or delete the whole group.
//!
//! The characters are built by [`headless`] (behind [`Provisioner`], so tests can stand in for
//! the world server), in the background, all slots at once.

pub mod defs;
pub mod headless;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::db::now;
use crate::secrets::{random_string, ALNUM_UPPER};
use crate::{accounts, soap, AppState};
pub use defs::{Preset, Slot};

/// One character to build, owned so it can cross onto a blocking thread.
#[derive(Clone, Debug)]
pub struct Job {
    pub account: String,
    pub password: String,
    pub slot: Slot,
    pub level: u8,
    pub tele: String,
    pub money: u32,
    pub names: Vec<String>,
}

#[async_trait]
pub trait Provisioner: Send + Sync {
    /// Create the job's character on its (empty, GM-enabled) account and leave it saved at the
    /// entrance.
    async fn build(&self, job: Job) -> Result<headless::Built>;
}

/// The real one: [`headless::build`] on a blocking thread, bounded.
pub struct Headless {
    pub servers: headless::Servers,
    /// `classicrealmd`, to see the builder's GM level land before it logs in.
    pub realmdb: sqlx::MySqlPool,
}

/// `account set gmlevel` answers before the login database has the new level (the world server
/// queues that write), and a login that reads the old level cannot say GM commands. Wait for it.
async fn gm_visible(db: &sqlx::MySqlPool, account: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let level: Option<(i64,)> = sqlx::query_as(
            "SELECT CAST(gmlevel AS SIGNED) FROM classicrealmd.account WHERE username = ?",
        )
        .bind(account)
        .fetch_optional(db)
        .await
        .context("reading the builder's GM level")?;
        if level.is_some_and(|(l,)| l >= 3) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("{account} never showed GM level 3 in the login database");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[async_trait]
impl Provisioner for Headless {
    async fn build(&self, job: Job) -> Result<headless::Built> {
        gm_visible(&self.realmdb, &job.account).await?;
        let servers = self.servers.clone();
        let account = job.account.clone();
        let task = tokio::task::spawn_blocking(move || {
            let j = headless::Job {
                account: &job.account,
                password: &job.password,
                slot: &job.slot,
                level: job.level,
                tele: &job.tele,
                money: job.money,
                names: &job.names,
            };
            headless::build(
                &servers,
                &j,
                &mut |m| tracing::info!(account = %job.account, "preset: {m}"),
            )
        });
        match tokio::time::timeout(Duration::from_secs(240), task).await {
            Ok(joined) => joined.context("the builder thread panicked")?,
            // The thread finishes or times out on its own socket reads; nothing waits for it.
            Err(_) => anyhow::bail!("building {account} took more than four minutes"),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Group {
    pub id: i64,
    pub preset: String,
    pub status: String,
    pub created_at: i64,
    pub token_enc: Vec<u8>,
    pub token_nonce: Vec<u8>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Member {
    pub slot: i64,
    pub user_id: i64,
    pub char_name: Option<String>,
    pub status: String,
    pub detail: Option<String>,
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The link's path part for a group (the admin panel shows it again).
pub fn link_token(state: &AppState, g: &Group) -> Result<String> {
    state.secrets.decrypt_string(&g.token_enc, &g.token_nonce)
}

pub async fn by_token(db: &SqlitePool, token: &str) -> Result<Option<Group>> {
    // A token is 43 base64url characters; anything else is not worth a query.
    if token.len() != 43
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Ok(None);
    }
    Ok(sqlx::query_as(
        "SELECT id, preset, status, created_at, token_enc, token_nonce FROM preset_groups WHERE token_hash = ?",
    )
    .bind(token_hash(token))
    .fetch_optional(db)
    .await?)
}

pub async fn by_id(db: &SqlitePool, id: i64) -> Result<Option<Group>> {
    Ok(sqlx::query_as(
        "SELECT id, preset, status, created_at, token_enc, token_nonce FROM preset_groups WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?)
}

pub async fn list(db: &SqlitePool) -> Result<Vec<Group>> {
    Ok(sqlx::query_as(
        "SELECT id, preset, status, created_at, token_enc, token_nonce FROM preset_groups ORDER BY id DESC",
    )
    .fetch_all(db)
    .await?)
}

pub async fn members(db: &SqlitePool, group_id: i64) -> Result<Vec<Member>> {
    Ok(sqlx::query_as(
        "SELECT slot, user_id, char_name, status, detail FROM preset_members WHERE group_id = ? ORDER BY slot",
    )
    .bind(group_id)
    .fetch_all(db)
    .await?)
}

/// The character a preset user plays, once it is built — `/api/play` hands it to the client so
/// the login and character screens are skipped.
pub async fn character_of(db: &SqlitePool, user_id: i64) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT char_name FROM preset_members WHERE user_id = ? AND status = 'ready'",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|r| r.0))
}

/// Make the group's users and rows, then build its characters in the background. Returns the
/// group id and the link token.
pub async fn create(
    state: &Arc<AppState>,
    preset: &'static Preset,
    actor: Option<i64>,
) -> Result<(i64, String)> {
    let token = new_token();
    let (enc, nonce) = state.secrets.encrypt(token.as_bytes())?;
    let mut tx = state.db.begin().await?;
    let group_id = sqlx::query(
        "INSERT INTO preset_groups (preset, token_hash, token_enc, token_nonce, status, created_at, created_by) VALUES (?, ?, ?, ?, 'building', ?, ?)",
    )
    .bind(&preset.id)
    .bind(token_hash(&token))
    .bind(&enc)
    .bind(&nonce)
    .bind(now())
    .bind(actor)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    for (i, slot) in preset.slots.iter().enumerate() {
        let user_id = sqlx::query(
            "INSERT INTO users (username, display_name, role, created_at) VALUES (?, ?, 'player', ?)",
        )
        .bind(format!("preset-{group_id}-{i}"))
        .bind(format!("{} · {}", preset.name, slot.label))
        .bind(now())
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO preset_members (group_id, slot, user_id, status) VALUES (?, ?, ?, 'pending')",
        )
        .bind(group_id)
        .bind(i as i64)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let st = Arc::clone(state);
    tokio::spawn(async move { build_group(st, group_id, preset).await });
    Ok((group_id, token))
}

async fn set_member(
    db: &SqlitePool,
    group_id: i64,
    slot: i64,
    status: &str,
    name: Option<&str>,
    detail: Option<&str>,
) {
    let r = sqlx::query(
        "UPDATE preset_members SET status = ?, char_name = COALESCE(?, char_name), detail = ? WHERE group_id = ? AND slot = ?",
    )
    .bind(status)
    .bind(name)
    .bind(detail)
    .bind(group_id)
    .bind(slot)
    .execute(db)
    .await;
    if let Err(e) = r {
        tracing::error!(error = %e, group_id, slot, "preset member update");
    }
}

async fn build_group(state: Arc<AppState>, group_id: i64, preset: &'static Preset) {
    let members = match members(&state.db, group_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, group_id, "preset: reading members");
            return;
        }
    };
    let mut set = tokio::task::JoinSet::new();
    for m in members {
        let st = Arc::clone(&state);
        set.spawn(async move {
            let slot = &preset.slots[m.slot as usize];
            let ok = match build_member(&st, group_id, preset, slot, &m).await {
                Ok(built) => {
                    let detail = (!built.not_equipped.is_empty()).then(|| {
                        let items: Vec<String> = built
                            .not_equipped
                            .iter()
                            .map(|(item, why)| format!("{item} ({why})"))
                            .collect();
                        format!("not equipped: {}", items.join(", "))
                    });
                    set_member(
                        &st.db,
                        group_id,
                        m.slot,
                        "ready",
                        Some(&built.name),
                        detail.as_deref(),
                    )
                    .await;
                    true
                }
                Err(e) => {
                    tracing::warn!(
                        error = format!("{e:#}"),
                        group_id,
                        slot = m.slot,
                        "preset: build failed"
                    );
                    set_member(
                        &st.db,
                        group_id,
                        m.slot,
                        "failed",
                        None,
                        Some(&format!("{e:#}")),
                    )
                    .await;
                    false
                }
            };
            ok
        });
    }
    let (mut ok, mut all) = (0, 0);
    while let Some(r) = set.join_next().await {
        all += 1;
        ok += usize::from(matches!(r, Ok(true)));
    }
    let status = match ok {
        n if n == all => "ready",
        0 => "failed",
        _ => "partial",
    };
    let _ = sqlx::query("UPDATE preset_groups SET status = ? WHERE id = ?")
        .bind(status)
        .bind(group_id)
        .execute(&state.db)
        .await;
    tracing::info!(group_id, preset = %preset.id, status, "preset group built");
}

async fn build_member(
    state: &AppState,
    group_id: i64,
    preset: &Preset,
    slot: &Slot,
    m: &Member,
) -> Result<headless::Built> {
    set_member(&state.db, group_id, m.slot, "building", None, None).await;
    // Its own random name, not the `WR<user id>` of invited players: a fresh name is a fresh
    // account, where an id-derived one can meet an account an earlier install left behind.
    let name = format!("WP{}", random_string(ALNUM_UPPER, 10));
    let acct =
        accounts::provision_as(&state.db, &state.soap, &state.secrets, m.user_id, &name).await?;
    let password = accounts::password(&state.secrets, &acct)?;
    // The character says its own GM commands while it is built; the level goes back to 0 however
    // the build ends.
    state
        .soap
        .exec(&format!("account set gmlevel {} 3 -1", acct.game_username))
        .await
        .context("granting the builder GM")?;
    let job = Job {
        account: acct.game_username.clone(),
        password,
        slot: slot.clone(),
        level: preset.level,
        tele: preset.tele.clone(),
        money: preset.money,
        names: names(slot.race, 8),
    };
    let built = state.provisioner.build(job).await;
    let revoke = state
        .soap
        .exec(&format!("account set gmlevel {} 0 -1", acct.game_username))
        .await;
    let built = built?;
    revoke.context("dropping the builder's GM")?;
    Ok(built)
}

/// Teleport every built character back to the entrance (works on offline characters too).
pub async fn summon(state: &AppState, g: &Group) -> Result<usize> {
    let preset = defs::get(&g.preset).context("unknown preset")?;
    let mut n = 0;
    for m in members(&state.db, g.id).await? {
        let Some(name) = m.char_name.as_deref() else {
            continue;
        };
        let name = soap::arg(name, 12)?;
        state
            .soap
            .exec(&format!("tele name {name} {}", preset.tele))
            .await
            .with_context(|| format!("teleporting {name}"))?;
        n += 1;
    }
    Ok(n)
}

/// Delete every account and character of the group, then the group. Refused while it is still
/// being built: the builder threads would otherwise race the deletes.
pub async fn delete(state: &AppState, g: &Group) -> Result<(), DeleteError> {
    if g.status == "building" {
        return Err(DeleteError::StillBuilding);
    }
    for m in members(&state.db, g.id).await.map_err(DeleteError::Other)? {
        if let Some(name) = m.char_name.as_deref() {
            // Online characters are kicked first so the delete does not wait on a session.
            let _ = accounts::kick(&state.soap, name).await;
        }
        accounts::delete_user(&state.db, &state.soap, m.user_id)
            .await
            .map_err(DeleteError::Other)?;
    }
    sqlx::query("DELETE FROM preset_groups WHERE id = ?")
        .bind(g.id)
        .execute(&state.db)
        .await
        .map_err(|e| DeleteError::Other(e.into()))?;
    Ok(())
}

#[derive(Debug)]
pub enum DeleteError {
    StillBuilding,
    Other(anyhow::Error),
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteError::StillBuilding => {
                write!(f, "the group is still being built — try again in a minute")
            }
            DeleteError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

/// A restart mid-build leaves groups marked `building` that nothing is building: fail them, so
/// they can be deleted.
pub async fn recover_interrupted(db: &SqlitePool) -> Result<()> {
    sqlx::query("UPDATE preset_members SET status = 'failed', detail = 'interrupted by a restart of the realm service' WHERE status IN ('pending', 'building')")
        .execute(db)
        .await?;
    sqlx::query("UPDATE preset_groups SET status = 'failed' WHERE status = 'building'")
        .execute(db)
        .await?;
    Ok(())
}

/// Name candidates: two syllables, letters only, 4–12 long, as the server's name rules want.
pub fn names(race: u8, n: usize) -> Vec<String> {
    use rand::seq::SliceRandom;
    const HEADS: &[&str] = &[
        "Bram", "Cal", "Dor", "Eld", "Fen", "Gar", "Hal", "Isen", "Jor", "Kel", "Lor", "Mor",
        "Nel", "Ori", "Pell", "Quin", "Ren", "Syl", "Tor", "Ulf", "Val", "Wyn", "Yar", "Zan",
        "Bel", "Cor", "Dun", "Ester", "Frey", "Gwen", "Thal", "Brin",
    ];
    const DWARF: &[&str] = &["Brom", "Durn", "Grim", "Thor", "Mag", "Kaz", "Bald", "Hrok"];
    const GNOME: &[&str] = &["Fizz", "Gim", "Nix", "Pip", "Tink", "Wiz", "Bix", "Sprock"];
    const ELF: &[&str] = &["Ael", "Lyr", "Shan", "Tel", "Ilth", "Mael", "Syl", "Eir"];
    const TAILS: &[&str] = &[
        "dric", "wyn", "dor", "mira", "thas", "ric", "len", "ara", "ius", "wen", "dan", "gar",
        "nor", "ley", "via", "son", "wick", "ra", "bek", "lin", "mund", "ris", "tan", "vell",
    ];
    let heads = match race {
        3 => DWARF,
        7 => GNOME,
        4 => ELF,
        _ => HEADS,
    };
    let mut rng = rand::thread_rng();
    let mut out: Vec<String> = Vec::new();
    while out.len() < n {
        let name = format!(
            "{}{}",
            heads.choose(&mut rng).unwrap(),
            TAILS.choose(&mut rng).unwrap()
        );
        let name = {
            let mut c = name.chars();
            let first = c.next().unwrap().to_ascii_uppercase();
            std::iter::once(first)
                .chain(c.map(|c| c.to_ascii_lowercase()))
                .collect::<String>()
        };
        let three_same = name
            .as_bytes()
            .windows(3)
            .any(|w| w[0].eq_ignore_ascii_case(&w[1]) && w[1].eq_ignore_ascii_case(&w[2]));
        if (4..=12).contains(&name.len()) && !three_same && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_rules() {
        for race in [1, 3, 4, 7] {
            for n in names(race, 50) {
                assert!((4..=12).contains(&n.len()), "{n}");
                assert!(n.chars().all(|c| c.is_ascii_alphabetic()), "{n}");
                assert!(n.chars().next().unwrap().is_ascii_uppercase(), "{n}");
                assert!(n.chars().skip(1).all(|c| c.is_ascii_lowercase()), "{n}");
            }
        }
    }

    #[test]
    fn tokens_are_the_shape_by_token_accepts() {
        let t = new_token();
        assert_eq!(t.len(), 43);
    }
}
