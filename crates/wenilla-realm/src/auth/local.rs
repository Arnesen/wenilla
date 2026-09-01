//! Username + password against `local_credentials` (argon2id). The same form serves admins and
//! players; the role on the user row decides what they may open afterwards.

use std::sync::Arc;

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use axum_extra::extract::CookieJar;
use sqlx::SqlitePool;

use crate::db::now;
use crate::session::{self, client_ip, Session};
use crate::{audit, render, templates, AppError, AppState};

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_string())
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

pub fn valid_username(u: &str) -> bool {
    (3..=32).contains(&u.len())
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn valid_password(p: &str) -> Result<(), &'static str> {
    if p.chars().count() < 10 {
        return Err("password must be at least 10 characters");
    }
    if p.len() > 200 {
        return Err("password is too long");
    }
    Ok(())
}

/// A password the user chose for themselves: theirs to keep, no forced change, no expiry.
pub async fn set_password(db: &SqlitePool, user_id: i64, password: &str) -> anyhow::Result<()> {
    store(db, user_id, password, false, None).await
}

/// A password an admin issued to get someone to their first login. It forces a change (which
/// [`session::require_session`] now enforces rather than merely suggesting) and it expires:
/// `ttl_hours` after now, or never when `ttl_hours` is 0.
///
/// The two are separate functions on purpose. They used to be one call with a `must_change: bool`,
/// and a bare bool at a call site is exactly how a credential ends up forced-but-immortal.
pub async fn set_bootstrap_password(
    db: &SqlitePool,
    user_id: i64,
    password: &str,
    ttl_hours: i64,
) -> anyhow::Result<()> {
    let expires_at = (ttl_hours > 0).then(|| now() + ttl_hours * 3600);
    store(db, user_id, password, true, expires_at).await
}

async fn store(
    db: &SqlitePool,
    user_id: i64,
    password: &str,
    must_change: bool,
    expires_at: Option<i64>,
) -> anyhow::Result<()> {
    let hash = hash_password(password)?;
    sqlx::query(
        "INSERT INTO local_credentials (user_id, password_hash, must_change, expires_at, updated_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET password_hash = excluded.password_hash, must_change = excluded.must_change, expires_at = excluded.expires_at, updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(hash)
    .bind(must_change as i64)
    .bind(expires_at)
    .bind(now())
    .execute(db)
    .await?;
    Ok(())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", post(logout))
}

async fn login_page(State(state): State<Arc<AppState>>, jar: CookieJar) -> Response {
    if let Some(t) = jar.get(session::COOKIE) {
        if let Ok(Some(s)) = session::lookup(&state.db, t.value()).await {
            return Redirect::to(if s.user.is_admin() { "/admin" } else { "/" }).into_response();
        }
    }
    render(templates::Login {
        error: None,
        realm_name: state.realm_name().await,
    })
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

async fn login_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers);
    let username = form.username.trim().to_string();
    let realm_name = state.realm_name().await;
    let fail = |msg: &str| {
        render(templates::Login {
            error: Some(msg.to_string()),
            realm_name: realm_name.clone(),
        })
    };

    let ip_key = format!("login:ip:{}", ip.clone().unwrap_or_default());
    let user_key = format!("login:user:{}", username.to_lowercase());
    if !state.limiter.allow(&ip_key, 10, 900) || !state.limiter.allow(&user_key, 10, 900) {
        audit::log(
            &state.db,
            None,
            ip.as_deref(),
            "login.ratelimited",
            Some(&username),
            None,
        )
        .await;
        return Ok(fail("too many attempts — wait 15 minutes"));
    }

    let row: Option<(i64, String, i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT u.id, c.password_hash, c.must_change, u.disabled, c.expires_at FROM users u JOIN local_credentials c ON c.user_id = u.id WHERE u.username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await?;
    let ok = match &row {
        Some((_, hash, _, disabled, _)) => *disabled == 0 && verify_password(hash, &form.password),
        // Burn the same time on unknown users so the response does not say which it was.
        None => {
            let _ = verify_password("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$Wf1yQwVQqk4jTfGCe3Fj5UOX0f8kO0m6mOQWhNfR2nY", &form.password);
            false
        }
    };
    sqlx::query("INSERT INTO login_attempts (ip, username, ok, at) VALUES (?, ?, ?, ?)")
        .bind(&ip)
        .bind(&username)
        .bind(ok as i64)
        .bind(now())
        .execute(&state.db)
        .await?;
    if !ok {
        audit::log(
            &state.db,
            None,
            ip.as_deref(),
            "login.failed",
            Some(&username),
            None,
        )
        .await;
        return Ok(fail("wrong username or password"));
    }
    let (user_id, _, must_change, _, expires_at) = row.expect("checked");
    // Only a bootstrap credential carries an expiry, and only now — after the password itself
    // verified — is it safe to say so: an attacker without the password still cannot tell an
    // expired account from a wrong guess.
    if let Some(exp) = expires_at.filter(|_| must_change != 0) {
        if exp <= now() {
            audit::log(
                &state.db,
                Some(user_id),
                ip.as_deref(),
                "login.expired",
                Some(&username),
                None,
            )
            .await;
            return Ok(fail(
                "that first-login password has expired — ask an admin to issue a new one",
            ));
        }
    }
    let token = session::create(
        &state.db,
        user_id,
        ip.as_deref(),
        headers.get("user-agent").and_then(|v| v.to_str().ok()),
    )
    .await?;
    audit::log(
        &state.db,
        Some(user_id),
        ip.as_deref(),
        "login.ok",
        Some(&username),
        None,
    )
    .await;
    let role: (String,) = sqlx::query_as("SELECT role FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    let dest = if must_change != 0 {
        "/account/password"
    } else if role.0 == "admin" {
        "/admin"
    } else {
        "/"
    };
    let jar = jar.add(session::cookie(token, !state.cfg.cookie_insecure));
    Ok((jar, Redirect::to(dest)).into_response())
}

async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<Response, AppError> {
    if let Some(c) = jar.get(session::COOKIE) {
        if let Ok(Some(s)) = session::lookup(&state.db, c.value()).await {
            audit::log(
                &state.db,
                Some(s.user.id),
                client_ip(&headers).as_deref(),
                "logout",
                None,
                None,
            )
            .await;
        }
        session::delete_by_token(&state.db, c.value()).await?;
    }
    let jar = jar.remove(session::removal_cookie());
    Ok((jar, Redirect::to("/login")).into_response())
}

/// `/account/password` — a player changing their own web password (also the forced first
/// change after an admin created or reset the account).
pub fn account_router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/account/password",
        get(password_page).post(password_submit),
    )
}

async fn password_page(session: Session, State(state): State<Arc<AppState>>) -> Response {
    let must: Option<(i64,)> =
        sqlx::query_as("SELECT must_change FROM local_credentials WHERE user_id = ?")
            .bind(session.user.id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    render(templates::Password {
        csrf: session.csrf_token.clone(),
        error: None,
        forced: must.map(|m| m.0 != 0).unwrap_or(false),
        user: session.user.clone(),
    })
}

#[derive(serde::Deserialize)]
pub struct PasswordForm {
    _csrf: String,
    current: String,
    new: String,
    confirm: String,
}

async fn password_submit(
    session: Session,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<PasswordForm>,
) -> Result<Response, AppError> {
    crate::csrf::verify(&session, &f._csrf)?;
    let fail = |msg: &str| {
        render(templates::Password {
            csrf: session.csrf_token.clone(),
            error: Some(msg.to_string()),
            forced: false,
            user: session.user.clone(),
        })
    };
    let row: Option<(String,)> =
        sqlx::query_as("SELECT password_hash FROM local_credentials WHERE user_id = ?")
            .bind(session.user.id)
            .fetch_optional(&state.db)
            .await?;
    if !row
        .map(|r| verify_password(&r.0, &f.current))
        .unwrap_or(false)
    {
        return Ok(fail("current password is wrong"));
    }
    if f.new != f.confirm {
        return Ok(fail("the two new passwords differ"));
    }
    if let Err(e) = valid_password(&f.new) {
        return Ok(fail(e));
    }
    set_password(&state.db, session.user.id, &f.new).await?;
    audit::log(
        &state.db,
        Some(session.user.id),
        client_ip(&headers).as_deref(),
        "password.changed",
        None,
        None,
    )
    .await;
    Ok(Redirect::to(if session.user.is_admin() {
        "/admin"
    } else {
        "/"
    })
    .into_response())
}
