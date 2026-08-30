//! Cookie sessions: a random token in `wr_session`, its SHA-256 in the `sessions` table. The
//! same session gates the HTML pages, the JSON API, and — via `require_session` layered over
//! wenilla-host's routers — `/data/*`, `/ws/*` and the wasm files, so nothing the game needs is
//! reachable without a login.

use std::sync::Arc;

use anyhow::Result;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::db::now;
use crate::AppState;

pub const COOKIE: &str = "wr_session";
const MAX_AGE_SECS: i64 = 30 * 24 * 3600;
const ROTATE_AFTER_SECS: i64 = 24 * 3600;
const TOUCH_EVERY_SECS: i64 = 5 * 60;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub disabled: i64,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

/// The authenticated caller, inserted into request extensions by [`require_session`] and
/// extractable from any handler behind it.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: i64,
    pub user: User,
    pub csrf_token: String,
    /// Set when the token was rotated during this request — the response must re-set the cookie.
    pub fresh_token: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for Session {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())
    }
}

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Create a session for `user_id`; returns the cookie token.
pub async fn create(
    db: &SqlitePool,
    user_id: i64,
    ip: Option<&str>,
    user_agent: Option<&str>,
) -> Result<String> {
    let token = new_token();
    let t = now();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, created_at, rotated_at, last_seen, expires_at, ip, user_agent) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(hash(&token))
    .bind(user_id)
    .bind(new_token())
    .bind(t)
    .bind(t)
    .bind(t)
    .bind(t + MAX_AGE_SECS)
    .bind(ip)
    .bind(user_agent.map(|u| u.chars().take(200).collect::<String>()))
    .execute(db)
    .await?;
    Ok(token)
}

pub async fn delete_by_token(db: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(hash(token))
        .execute(db)
        .await?;
    Ok(())
}

/// Kill every session of a user — on disable, delete, or password change.
pub async fn delete_for_user(db: &SqlitePool, user_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Resolve a cookie token to a live session, touching/rotating it as a side effect.
pub async fn lookup(db: &SqlitePool, token: &str) -> Result<Option<Session>> {
    let t = now();
    let row: Option<(i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT id, user_id, csrf_token, rotated_at, last_seen FROM sessions WHERE token_hash = ? AND expires_at > ?",
    )
    .bind(hash(token))
    .bind(t)
    .fetch_optional(db)
    .await?;
    let Some((id, user_id, csrf_token, rotated_at, last_seen)) = row else {
        return Ok(None);
    };
    let user: Option<User> = sqlx::query_as("SELECT id, username, display_name, role, disabled FROM users WHERE id = ? AND disabled = 0")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    let Some(user) = user else { return Ok(None) };
    let mut fresh_token = None;
    if t - rotated_at > ROTATE_AFTER_SECS {
        let token = new_token();
        sqlx::query("UPDATE sessions SET token_hash = ?, rotated_at = ?, last_seen = ?, expires_at = ? WHERE id = ?")
            .bind(hash(&token))
            .bind(t)
            .bind(t)
            .bind(t + MAX_AGE_SECS)
            .bind(id)
            .execute(db)
            .await?;
        fresh_token = Some(token);
    } else if t - last_seen > TOUCH_EVERY_SECS {
        sqlx::query("UPDATE sessions SET last_seen = ? WHERE id = ?")
            .bind(t)
            .bind(id)
            .execute(db)
            .await?;
    }
    Ok(Some(Session {
        id,
        user,
        csrf_token,
        fresh_token,
    }))
}

pub fn cookie<'a>(token: String, secure: bool) -> Cookie<'a> {
    Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::seconds(MAX_AGE_SECS))
        .build()
}

pub fn removal_cookie<'a>() -> Cookie<'a> {
    Cookie::build((COOKIE, ""))
        .path("/")
        .max_age(time::Duration::ZERO)
        .build()
}

/// Peer address as Caddy reports it. Only Caddy can reach this service, so the header is trusted.
pub fn client_ip(parts: &axum::http::HeaderMap) -> Option<String> {
    parts
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Is this a request for a page (redirect to `/login` on failure) or for an asset/API (401)?
fn wants_html(req: &Request) -> bool {
    let p = req.uri().path();
    if p.starts_with("/data/") || p.starts_with("/ws/") || p.starts_with("/api/") {
        return false;
    }
    req.headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("text/html"))
        .unwrap_or(false)
}

fn unauthorized(req: &Request) -> Response {
    if wants_html(req) {
        Redirect::to("/login").into_response()
    } else {
        let mut r = StatusCode::UNAUTHORIZED.into_response();
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        r
    }
}

/// Middleware: require a live session; inserts [`Session`] into extensions.
pub async fn require_session(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(token) = jar.get(COOKIE).map(|c| c.value().to_string()) else {
        return unauthorized(&req);
    };
    let session = match lookup(&state.db, &token).await {
        Ok(Some(s)) => s,
        Ok(None) => return unauthorized(&req),
        Err(e) => {
            tracing::error!(error = %e, "session lookup");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let fresh = session.fresh_token.clone();
    req.extensions_mut().insert(session);
    let mut resp = next.run(req).await;
    if let Some(token) = fresh {
        let c = cookie(token, !state.cfg.cookie_insecure);
        if let Ok(v) = HeaderValue::from_str(&c.to_string()) {
            resp.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    resp.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Cookie"));
    resp
}

/// Middleware (behind `require_session`): admins only.
pub async fn require_admin(req: Request, next: Next) -> Response {
    match req.extensions().get::<Session>() {
        Some(s) if s.user.is_admin() => next.run(req).await,
        Some(_) => StatusCode::FORBIDDEN.into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}
