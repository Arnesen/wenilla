//! JSON for the play page. Same-origin only: mutations (none yet) would require the
//! `X-Requested-With: wenilla` header; reads are gated by the session like everything else.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::session::Session;
use crate::{accounts, AppError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/play", get(play))
        .route("/api/me", get(me))
}

async fn play(session: Session, State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    if !state.limiter.allow(&format!("play:{}", session.id), 30, 60) {
        return Err(AppError::TooMany);
    }
    let acct = match accounts::get(&state.db, session.user.id).await? {
        Some(a) => a,
        None => {
            accounts::provision(&state.db, &state.soap, &state.secrets, session.user.id).await?
        }
    };
    let pass = accounts::password(&state.secrets, &acct)?;
    let mut body = serde_json::json!({
        "user": acct.game_username,
        "pass": pass,
        "host": state.cfg.public_host(),
        "realm": state.realm_name().await,
    });
    if state.cfg.dev_query_creds {
        body["dev_query_creds"] = serde_json::Value::String("1".into());
    }
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

async fn me(session: Session) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({ "username": session.user.username, "display_name": session.user.display_name, "role": session.user.role }),
    )
}
