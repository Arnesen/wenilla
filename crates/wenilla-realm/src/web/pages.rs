//! Unauthenticated pages: `/healthz`, `/about`, `/privacy`.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::{render, templates, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/about", get(about))
        .route("/privacy", get(privacy))
}

async fn healthz(State(state): State<Arc<AppState>>) -> Response {
    let sqlite = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    let mariadb = crate::realmdb::ping(&state.realmdb).await;
    let soap = state.soap.exec("server info").await.is_ok();
    let body = serde_json::json!({ "sqlite": sqlite, "mariadb": mariadb, "soap": soap, "client_data": state.client_data_error.is_none(), "client_data_error": state.client_data_error });
    let code = if sqlite {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body)).into_response()
}

async fn about(State(state): State<Arc<AppState>>) -> Response {
    render(templates::About {
        realm_name: state.realm_name().await,
    })
}

async fn privacy(State(state): State<Arc<AppState>>) -> Response {
    render(templates::Privacy {
        realm_name: state.realm_name().await,
    })
}
