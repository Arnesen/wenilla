//! `/setup` — first run: the admin account, the realm's name, rates, bots, MOTD; then the
//! console bootstrap and `setup_complete`. After `reset-admin` the same page runs in a reduced
//! mode that only sets a new admin password.

use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use axum_extra::extract::CookieJar;

use crate::auth::local::{set_password, valid_password, valid_username};
use crate::db::{meta_del, meta_get, meta_set, now, setup_complete};
use crate::session::client_ip;
use crate::{
    accounts, audit, control, mangos_conf, realmdb, render, session, templates, AppError, AppState,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/setup", get(page).post(submit))
}

#[derive(serde::Deserialize)]
pub struct TokenQuery {
    token: Option<String>,
}

async fn page(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Response, AppError> {
    if setup_complete(&state.db).await? {
        return Ok(Redirect::to("/login").into_response());
    }
    let bot_account_count = mangos_conf::load(&state.conf)
        .map(|s| s.bot_account_count)
        .unwrap_or(50);
    Ok(render(templates::Setup {
        error: None,
        token: q.token.unwrap_or_default(),
        public_url: state.cfg.public_url.clone(),
        bot_account_count,
    }))
}

#[derive(serde::Deserialize)]
pub struct SetupForm {
    token: String,
    admin_username: String,
    admin_password: String,
    admin_password2: String,
    #[serde(default)]
    realm_name: String,
    #[serde(default = "one")]
    xp_rate: f64,
    #[serde(default = "one")]
    loot_rate: f64,
    #[serde(default = "one")]
    money_rate: f64,
    #[serde(default)]
    bots: i64,
    #[serde(default)]
    ahbot: Option<String>,
    #[serde(default)]
    motd: String,
}

fn one() -> f64 {
    1.0
}

async fn submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(f): Form<SetupForm>,
) -> Result<Response, AppError> {
    if setup_complete(&state.db).await? {
        return Ok(Redirect::to("/login").into_response());
    }
    let ip = client_ip(&headers);
    let bot_account_count = mangos_conf::load(&state.conf)
        .map(|s| s.bot_account_count)
        .unwrap_or(50);
    let fail = |msg: String| {
        render(templates::Setup {
            error: Some(msg),
            token: f.token.clone(),
            public_url: state.cfg.public_url.clone(),
            bot_account_count,
        })
    };

    if !state.limiter.allow(
        &format!("setup:{}", ip.clone().unwrap_or_default()),
        10,
        900,
    ) {
        return Ok(fail("too many attempts".into()));
    }
    let expected = meta_get(&state.db, "setup_token")
        .await?
        .unwrap_or_default();
    use subtle::ConstantTimeEq;
    if expected.is_empty() || !bool::from(expected.as_bytes().ct_eq(f.token.as_bytes())) {
        audit::log(&state.db, None, ip.as_deref(), "setup.badtoken", None, None).await;
        return Ok(fail(
            "wrong setup token — it is printed in the service log (`realmctl up` shows it)".into(),
        ));
    }
    let admin_username = f.admin_username.trim().to_string();
    if !valid_username(&admin_username) {
        return Ok(fail("username: 3–32 letters, digits, _ or -".into()));
    }
    if f.admin_password != f.admin_password2 {
        return Ok(fail("the two passwords differ".into()));
    }
    if let Err(e) = valid_password(&f.admin_password) {
        return Ok(fail(e.into()));
    }

    let reset_mode = meta_get(&state.db, "setup_mode").await?.as_deref() == Some("reset");
    let admin_id: i64 = if reset_mode {
        // Only the admin password changes; keep every user and setting.
        let existing: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM users WHERE role = 'admin' ORDER BY id LIMIT 1")
                .fetch_optional(&state.db)
                .await?;
        match existing {
            Some((id,)) => {
                sqlx::query("UPDATE users SET username = ? WHERE id = ?")
                    .bind(&admin_username)
                    .bind(id)
                    .execute(&state.db)
                    .await?;
                id
            }
            None => insert_admin(&state, &admin_username).await?,
        }
    } else {
        let realm_name = f.realm_name.trim().to_string();
        if realm_name.is_empty()
            || realm_name.len() > 32
            || !realm_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " '-".contains(c))
        {
            return Ok(fail(
                "realm name: 1–32 letters, digits, spaces, ' or -".into(),
            ));
        }
        if !(0..=2000).contains(&f.bots) {
            return Ok(fail("bot count: 0–2000".into()));
        }

        // 1. The console: our own account, the seeded defaults re-passworded.
        let (su, sp) = match accounts::bootstrap_console(&state.soap).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(fail(format!(
                    "could not reach the world server console: {e:#}. Is mangosd up yet?"
                )))
            }
        };
        state.store_soap_credentials(&su, &sp).await?;

        // 2. Realm name + settings.
        if let Err(e) = realmdb::set_realm_name(&state.realmdb, &realm_name).await {
            tracing::warn!(error = %e, "could not write the realm name to realmlist (the game shows the old one)");
        }
        meta_set(&state.db, "realm_name", &realm_name).await?;
        let old = mangos_conf::load(&state.conf)?;
        let mut new = old.clone();
        new.xp_rate = f.xp_rate;
        new.loot_rate = f.loot_rate;
        new.money_rate = f.money_rate;
        new.bots = f.bots;
        new.ahbot = f.ahbot.is_some();
        let motd = f.motd.trim();
        new.motd = if motd.is_empty() {
            format!("Welcome to {realm_name}")
        } else {
            motd.chars()
                .filter(|c| !c.is_control() && *c != '"')
                .take(250)
                .collect()
        };
        let steps = match mangos_conf::save(&state.conf, &old, &new) {
            Ok(s) => s,
            Err(e) => return Ok(fail(e.to_string())),
        };
        let restart = control::apply(&state.soap, &steps).await.unwrap_or(true);
        let _ = control::set_motd_live(&state.soap, &new.motd).await;
        meta_set(
            &state.db,
            "restart_pending",
            if restart { "1" } else { "0" },
        )
        .await?;

        insert_admin(&state, &admin_username).await?
    };

    set_password(&state.db, admin_id, &f.admin_password, false).await?;
    let _ = accounts::provision(&state.db, &state.soap, &state.secrets, admin_id).await;
    meta_set(&state.db, "setup_complete", "1").await?;
    meta_del(&state.db, "setup_token").await?;
    meta_del(&state.db, "setup_mode").await?;
    let _ = std::fs::remove_file(state.cfg.state_dir.join("setup-token"));
    audit::log(
        &state.db,
        Some(admin_id),
        ip.as_deref(),
        if reset_mode {
            "setup.reset"
        } else {
            "setup.complete"
        },
        Some(&admin_username),
        None,
    )
    .await;

    let token = session::create(&state.db, admin_id, ip.as_deref(), None).await?;
    let jar = jar.add(session::cookie(token, !state.cfg.cookie_insecure));
    Ok((jar, Redirect::to("/admin/users")).into_response())
}

async fn insert_admin(state: &AppState, username: &str) -> Result<i64, AppError> {
    let existing: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&state.db)
        .await?;
    if let Some((id,)) = existing {
        sqlx::query("UPDATE users SET role = 'admin', disabled = 0 WHERE id = ?")
            .bind(id)
            .execute(&state.db)
            .await?;
        return Ok(id);
    }
    let id = sqlx::query(
        "INSERT INTO users (username, display_name, role, created_at) VALUES (?, ?, 'admin', ?)",
    )
    .bind(username)
    .bind(username)
    .bind(now())
    .execute(&state.db)
    .await?
    .last_insert_rowid();
    Ok(id)
}
