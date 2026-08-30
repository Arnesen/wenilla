//! `/` — the play page. It fetches the hidden game credentials over the session and boots the
//! wasm client with them in memory (`window.__wenilla_env`), so no password ever sits in a URL.

use std::sync::Arc;

use axum::response::Response;
use axum::routing::get;
use axum::Router;

use crate::session::Session;
use crate::{render, templates, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(play))
}

async fn play(session: Session) -> Response {
    render(templates::Play {
        user: session.user.clone(),
    })
}
