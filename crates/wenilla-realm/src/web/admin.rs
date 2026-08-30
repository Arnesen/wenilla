//! The admin panel: dashboard (status, who is online, announce/restart), users (create, ban,
//! kick, passwords, delete), config (rates, MOTD, limits, bots), audit.

use std::sync::Arc;

use axum::extract::{Form, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::auth::local::{set_password, valid_password, valid_username};
use crate::db::{meta_get, meta_set, now};
use crate::secrets::{random_string, ALNUM};
use crate::session::{client_ip, Session};
use crate::templates::{self, UserRow};
use crate::{
    accounts, audit, control, csrf, mangos_conf, realmdb, render, session, AppError, AppState,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/server/{action}", post(server_action))
        .route("/admin/users", get(users).post(create_user))
        .route("/admin/users/{id}/{action}", post(user_action))
        .route("/admin/config", get(config_page).post(config_save))
        .route("/admin/audit", get(audit_page))
}

#[derive(serde::Deserialize, Default)]
pub struct Flash {
    notice: Option<String>,
    error: Option<String>,
}

async fn restart_pending(state: &AppState) -> bool {
    meta_get(&state.db, "restart_pending")
        .await
        .ok()
        .flatten()
        .as_deref()
        == Some("1")
}

async fn dashboard(
    session: Session,
    State(state): State<Arc<AppState>>,
    Query(flash): Query<Flash>,
) -> Result<Response, AppError> {
    let status = control::status(&state.soap).await;
    let online = realmdb::online(&state.realmdb).await.unwrap_or_default();
    let (players_online, bots_online) = realmdb::online_count(&state.realmdb)
        .await
        .unwrap_or((0, 0));
    let s = mangos_conf::load(&state.conf).unwrap_or_else(|_| mangos_conf::Settings {
        xp_rate: 1.0,
        loot_rate: 1.0,
        money_rate: 1.0,
        player_limit: 0,
        save_interval_secs: 0,
        max_player_level: 60,
        motd: String::new(),
        bots: 0,
        bot_account_count: 0,
        ahbot: false,
    });
    Ok(render(templates::AdminDashboard {
        realm_name: state.realm_name().await,
        nav: "dashboard",
        s,
        client_data_error: state.client_data_error.clone(),
        csrf: session.csrf_token.clone(),
        me: session.user,
        status,
        online,
        players_online,
        bots_online,
        restart_pending: restart_pending(&state).await,
        notice: flash.notice,
        error: flash.error,
    }))
}

#[derive(serde::Deserialize)]
pub struct ServerForm {
    _csrf: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    delay: Option<u32>,
}

fn back(path: &str, notice: Result<String, String>) -> Response {
    let (k, v) = match notice {
        Ok(m) => ("notice", m),
        Err(m) => ("error", m),
    };
    Redirect::to(&format!("{path}?{k}={}", urlencode(&v))).into_response()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn server_action(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Form(f): Form<ServerForm>,
) -> Result<Response, AppError> {
    csrf::verify(&session, &f._csrf)?;
    let ip = client_ip(&headers);
    let result: Result<String, String> = match action.as_str() {
        "announce" => control::announce(&state.soap, &f.text)
            .await
            .map(|_| "announced".into())
            .map_err(|e| e.to_string()),
        "notify" => control::notify(&state.soap, &f.text)
            .await
            .map(|_| "notified".into())
            .map_err(|e| e.to_string()),
        "reload" => state
            .soap
            .exec("reload config")
            .await
            .map(|_| "config reloaded".into())
            .map_err(|e| e.to_string()),
        "restart" => {
            let delay = f.delay.unwrap_or(60);
            match control::restart(&state.soap, delay).await {
                Ok(()) => {
                    meta_set(&state.db, "restart_pending", "0").await?;
                    Ok(format!(
                        "world server restarts in {delay}s; it is back a minute or two after that"
                    ))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "cancel-restart" => control::cancel_restart(&state.soap)
            .await
            .map(|_| "restart cancelled".into())
            .map_err(|e| e.to_string()),
        _ => return Err(AppError::NotFound),
    };
    audit::log(
        &state.db,
        Some(session.user.id),
        ip.as_deref(),
        &format!("server.{action}"),
        None,
        Some(&format!("{:?}", result.as_ref().map(|_| &f.text))),
    )
    .await;
    Ok(back("/admin", result))
}

async fn user_rows(state: &AppState) -> Result<Vec<UserRow>, AppError> {
    let rows: Vec<(i64, String, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT u.id, u.username, u.display_name, u.role, u.disabled, g.game_username FROM users u \
         LEFT JOIN game_accounts g ON g.user_id = u.id ORDER BY u.id",
    )
    .fetch_all(&state.db)
    .await?;
    let bans: Vec<String> = realmdb::active_bans(&state.realmdb)
        .await
        .map(|b| b.into_iter().map(|b| b.username).collect())
        .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (id, username, display_name, role, disabled, game_username) in rows {
        let game_username = game_username.unwrap_or_default();
        let characters = if game_username.is_empty() {
            Vec::new()
        } else {
            realmdb::characters(&state.realmdb, &game_username)
                .await
                .unwrap_or_default()
        };
        out.push(UserRow {
            id,
            username,
            display_name,
            role,
            disabled: disabled != 0,
            banned: bans.contains(&game_username),
            game_username,
            characters,
        });
    }
    Ok(out)
}

async fn users(
    session: Session,
    State(state): State<Arc<AppState>>,
    Query(flash): Query<Flash>,
) -> Result<Response, AppError> {
    Ok(render(templates::AdminUsers {
        realm_name: state.realm_name().await,
        nav: "users",
        csrf: session.csrf_token.clone(),
        me: session.user,
        users: user_rows(&state).await?,
        notice: flash.notice,
        created: None,
        error: flash.error,
    }))
}

#[derive(serde::Deserialize)]
pub struct CreateForm {
    _csrf: String,
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    role: String,
}

async fn create_user(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<CreateForm>,
) -> Result<Response, AppError> {
    csrf::verify(&session, &f._csrf)?;
    let username = f.username.trim().to_string();
    let realm_name = state.realm_name().await;
    let page = |created: Option<(String, String)>, error: Option<String>, users: Vec<UserRow>| {
        render(templates::AdminUsers {
            realm_name: realm_name.clone(),
            nav: "users",
            csrf: session.csrf_token.clone(),
            me: session.user.clone(),
            users,
            notice: None,
            created,
            error,
        })
    };
    if !valid_username(&username) {
        return Ok(page(
            None,
            Some("username: 3–32 letters, digits, _ or -".into()),
            user_rows(&state).await?,
        ));
    }
    let password = if f.password.trim().is_empty() {
        random_string(ALNUM, 14)
    } else {
        f.password.clone()
    };
    if let Err(e) = valid_password(&password) {
        return Ok(page(None, Some(e.into()), user_rows(&state).await?));
    }
    let role = if f.role == "admin" { "admin" } else { "player" };
    let display = if f.display_name.trim().is_empty() {
        username.clone()
    } else {
        f.display_name.trim().chars().take(40).collect()
    };
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE username = ?")
        .bind(&username)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_some() {
        return Ok(page(
            None,
            Some("that username is taken".into()),
            user_rows(&state).await?,
        ));
    }
    let id = sqlx::query(
        "INSERT INTO users (username, display_name, role, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&username)
    .bind(&display)
    .bind(role)
    .bind(now())
    .execute(&state.db)
    .await?
    .last_insert_rowid();
    set_password(&state.db, id, &password, true).await?;
    if let Err(e) = accounts::provision(&state.db, &state.soap, &state.secrets, id).await {
        // Keep the web user; the game account is provisioned again on their first play.
        tracing::warn!(error = %e, user = %username, "game account provisioning deferred");
    }
    audit::log(
        &state.db,
        Some(session.user.id),
        client_ip(&headers).as_deref(),
        "user.create",
        Some(&username),
        Some(role),
    )
    .await;
    Ok(page(
        Some((username, password)),
        None,
        user_rows(&state).await?,
    ))
}

#[derive(serde::Deserialize)]
pub struct UserActionForm {
    _csrf: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    duration_hours: Option<i64>,
    #[serde(default)]
    character: String,
    #[serde(default)]
    role: String,
}

async fn user_action(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, action)): Path<(i64, String)>,
    Form(f): Form<UserActionForm>,
) -> Result<Response, AppError> {
    csrf::verify(&session, &f._csrf)?;
    let ip = client_ip(&headers);
    let target: Option<(String, String)> =
        sqlx::query_as("SELECT username, role FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let Some((username, role)) = target else {
        return Err(AppError::NotFound);
    };
    let self_target = id == session.user.id;
    let mut detail = None;
    let result: Result<String, String> = match action.as_str() {
        "ban" => {
            let dur = f.duration_hours.filter(|h| *h > 0).map(|h| h * 3600);
            detail = Some(f.reason.clone());
            accounts::ban(
                &state.db,
                &state.soap,
                id,
                Some(session.user.id),
                dur,
                &f.reason,
            )
            .await
            .map(|_| format!("{username} banned"))
            .map_err(|e| e.to_string())
        }
        "unban" => accounts::unban(&state.db, &state.soap, id)
            .await
            .map(|_| format!("{username} unbanned"))
            .map_err(|e| e.to_string()),
        "kick" => {
            detail = Some(f.character.clone());
            accounts::kick(&state.soap, f.character.trim())
                .await
                .map(|r| if r.is_empty() { "kicked".into() } else { r })
                .map_err(|e| e.to_string())
        }
        "reset-web-password" => {
            let pw = random_string(ALNUM, 14);
            set_password(&state.db, id, &pw, true).await?;
            session::delete_for_user(&state.db, id).await?;
            Ok(format!(
                "new web password for {username}: {pw} (they must change it at first login)"
            ))
        }
        "rotate-game-password" => {
            accounts::rotate_password(&state.db, &state.soap, &state.secrets, id)
                .await
                .map(|_| "game password rotated; they need to reload the play page".into())
                .map_err(|e| e.to_string())
        }
        "disable" if !self_target => {
            sqlx::query("UPDATE users SET disabled = 1 WHERE id = ?")
                .bind(id)
                .execute(&state.db)
                .await?;
            session::delete_for_user(&state.db, id).await?;
            if let Err(e) = accounts::ban(
                &state.db,
                &state.soap,
                id,
                Some(session.user.id),
                None,
                "account disabled",
            )
            .await
            {
                tracing::warn!(error = %e, "disable: game ban failed");
            }
            Ok(format!("{username} disabled"))
        }
        "enable" => {
            sqlx::query("UPDATE users SET disabled = 0 WHERE id = ?")
                .bind(id)
                .execute(&state.db)
                .await?;
            let _ = accounts::unban(&state.db, &state.soap, id).await;
            Ok(format!("{username} enabled"))
        }
        "role" if !self_target => {
            let new_role = if f.role == "admin" { "admin" } else { "player" };
            sqlx::query("UPDATE users SET role = ? WHERE id = ?")
                .bind(new_role)
                .bind(id)
                .execute(&state.db)
                .await?;
            session::delete_for_user(&state.db, id).await?;
            detail = Some(new_role.into());
            Ok(format!("{username} is now {new_role}"))
        }
        "delete" if !self_target => accounts::delete_user(&state.db, &state.soap, id)
            .await
            .map(|_| format!("{username} and their characters deleted"))
            .map_err(|e| e.to_string()),
        "disable" | "role" | "delete" => Err("you cannot do that to your own account".into()),
        _ => return Err(AppError::NotFound),
    };
    let _ = role;
    audit::log(
        &state.db,
        Some(session.user.id),
        ip.as_deref(),
        &format!("user.{action}"),
        Some(&username),
        detail
            .as_deref()
            .or(result.as_ref().err().map(String::as_str)),
    )
    .await;
    Ok(back("/admin/users", result))
}

async fn config_page(
    session: Session,
    State(state): State<Arc<AppState>>,
    Query(flash): Query<Flash>,
) -> Result<Response, AppError> {
    let s = mangos_conf::load(&state.conf)?;
    Ok(render(templates::AdminConfig {
        realm_name: state.realm_name().await,
        nav: "settings",
        csrf: session.csrf_token.clone(),
        me: session.user,
        s,
        restart_pending: restart_pending(&state).await,
        notice: flash.notice,
        error: flash.error,
    }))
}

#[derive(serde::Deserialize)]
pub struct ConfigForm {
    _csrf: String,
    xp_rate: f64,
    loot_rate: f64,
    money_rate: f64,
    player_limit: i64,
    save_interval_secs: i64,
    max_player_level: i64,
    #[serde(default)]
    motd: String,
    bots: i64,
    #[serde(default)]
    ahbot: Option<String>,
}

async fn config_save(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<ConfigForm>,
) -> Result<Response, AppError> {
    csrf::verify(&session, &f._csrf)?;
    let old = mangos_conf::load(&state.conf)?;
    let mut new = old.clone();
    new.xp_rate = f.xp_rate;
    new.loot_rate = f.loot_rate;
    new.money_rate = f.money_rate;
    new.player_limit = f.player_limit;
    new.save_interval_secs = f.save_interval_secs;
    new.max_player_level = f.max_player_level;
    new.motd = f
        .motd
        .trim()
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .take(250)
        .collect();
    new.bots = f.bots;
    new.ahbot = f.ahbot.is_some();
    if new == old {
        return Ok(back("/admin/config", Ok("nothing changed".into())));
    }
    let steps = match mangos_conf::save(&state.conf, &old, &new) {
        Ok(s) => s,
        Err(e) => return Ok(back("/admin/config", Err(e.to_string()))),
    };
    let applied = control::apply(&state.soap, &steps).await;
    if new.motd != old.motd {
        let _ = control::set_motd_live(&state.soap, &new.motd).await;
    }
    audit::log(
        &state.db,
        Some(session.user.id),
        client_ip(&headers).as_deref(),
        "config.save",
        None,
        Some(&serde_json::to_string(&new).unwrap_or_default()),
    )
    .await;
    let msg = match applied {
        Ok(true) => {
            meta_set(&state.db, "restart_pending", "1").await?;
            Ok(
                "saved — a world-server restart is needed for some of these (Dashboard → Restart)"
                    .to_string(),
            )
        }
        Ok(false) => Ok("saved and applied live".to_string()),
        Err(e) => Err(format!(
            "saved to the config files, but applying live failed: {e}"
        )),
    };
    Ok(back("/admin/config", msg))
}

#[derive(serde::Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
}

async fn audit_page(
    session: Session,
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuditQuery>,
) -> Result<Response, AppError> {
    let entries = audit::recent(&state.db, q.before, 100).await?;
    let next_before = if entries.len() == 100 {
        entries.last().map(|e| e.id)
    } else {
        None
    };
    Ok(render(templates::AdminAudit {
        realm_name: state.realm_name().await,
        nav: "audit",
        me: session.user,
        entries,
        next_before,
    }))
}
