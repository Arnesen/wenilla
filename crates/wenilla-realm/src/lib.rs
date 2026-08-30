//! `wenilla-realm` — the realm service. One process: login-gated hosting of the browser client
//! (wenilla-host's `/data`, `/ws`, static routers behind a session cookie), the play page that
//! hands the client its hidden game credentials, and an admin panel that runs the game server
//! over SOAP, the config files and read-only DB access. See `docs/DESIGN.md` in wenilla-realm.

pub mod accounts;
pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod control;
pub mod csrf;
pub mod db;
pub mod mangos_conf;
pub mod ratelimit;
pub mod realmdb;
pub mod secrets;
pub mod session;
pub mod soap;
pub mod sysinfo;
pub mod templates;
pub mod web;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use askama::Template;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Router;
use benilla_formats::Chain;

pub use config::Config;

pub struct AppState {
    pub cfg: Config,
    pub db: sqlx::SqlitePool,
    pub realmdb: sqlx::MySqlPool,
    pub soap: soap::Client,
    pub conf: mangos_conf::ConfFiles,
    pub secrets: secrets::Keyring,
    pub providers: Vec<Arc<dyn auth::provider::IdentityProvider>>,
    pub limiter: ratelimit::Limiter,
    /// Why the client data could not be opened at start — `None` when it could. The service
    /// still runs (setup and the panel work) so an operator sees the problem instead of a
    /// crash loop; `/data` answers 503 and `/healthz` reports it until a restart with a valid
    /// `CLIENT_DATA`.
    pub client_data_error: Option<String>,
}

impl AppState {
    pub async fn realm_name(&self) -> String {
        db::meta_get(&self.db, "realm_name")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "Realm".into())
    }

    /// Restore the console credentials the bootstrap stored (or fall back to the seeded ones).
    pub async fn load_soap_credentials(&self) -> Result<()> {
        if let (Some(user), Some(enc), Some(nonce)) = (
            db::meta_get(&self.db, "soap_user").await?,
            db::meta_get(&self.db, "soap_pass_enc").await?,
            db::meta_get(&self.db, "soap_nonce").await?,
        ) {
            let pass = self
                .secrets
                .decrypt_string(&hex::decode(enc)?, &hex::decode(nonce)?)?;
            self.soap.set_credentials(&user, &pass).await;
        }
        Ok(())
    }

    pub async fn store_soap_credentials(&self, user: &str, pass: &str) -> Result<()> {
        let (enc, nonce) = self.secrets.encrypt(pass.as_bytes())?;
        db::meta_set(&self.db, "soap_user", user).await?;
        db::meta_set(&self.db, "soap_pass_enc", &hex::encode(enc)).await?;
        db::meta_set(&self.db, "soap_nonce", &hex::encode(nonce)).await?;
        Ok(())
    }
}

/// Handler errors: the few shapes pages need, plus a catch-all that logs and says 500.
#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Forbidden(&'static str),
    NotFound,
    TooMany,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m).into_response(),
            AppError::NotFound => StatusCode::NOT_FOUND.into_response(),
            AppError::TooMany => (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response(),
            AppError::Internal(e) => {
                tracing::error!(error = format!("{e:#}"), "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("something went wrong: {e:#}"),
                )
                    .into_response()
            }
        }
    }
}

pub fn render<T: Template>(t: T) -> Response {
    match t.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "template");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Until the wizard has run, every page is the wizard.
async fn setup_gate(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let p = req.uri().path();
    if p.starts_with("/setup") || p == "/healthz" {
        return next.run(req).await;
    }
    match db::setup_complete(&state.db).await {
        Ok(true) => next.run(req).await,
        Ok(false) => Redirect::to("/setup").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "setup check");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The whole application.
///
/// `chain` is `None` only in tests (the data router needs real archives); everything else,
/// including the lock on `/data`, is mounted the same way.
pub fn app(state: Arc<AppState>, chain: Option<Arc<Chain>>, www: &Path) -> Router {
    let session_layer =
        middleware::from_fn_with_state(Arc::clone(&state), session::require_session);
    let admin_layer = middleware::from_fn(session::require_admin);

    // Anything the game client needs — the wasm bundle, the data reads, the two relayed ports —
    // sits behind the session. The relay's upstream is a service per port in the container
    // deployment (realmd and mangosd are separate services).
    let locked = api::router()
        .merge(web::play::router())
        .merge(auth::local::account_router())
        .route_layer(session_layer.clone());
    let data = match chain {
        Some(chain) => wenilla_host::data::router(chain),
        None => Router::new().route(
            "/data/{*name}",
            axum::routing::get(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "client data is not available on this server — check CLIENT_DATA",
                )
            }),
        ),
    };
    let game = data
        .merge(wenilla_host::ws::router_map([
            (3724u16, Arc::<str>::from(state.cfg.realmd_host.as_str())),
            (8085u16, Arc::<str>::from(state.cfg.mangosd_host.as_str())),
        ]))
        .merge(wenilla_host::static_site::router(www))
        .layer(session_layer.clone());
    let admin = web::admin::router()
        .route_layer(admin_layer)
        .route_layer(session_layer);

    let service = Router::new()
        .merge(web::setup::router())
        .merge(web::pages::router())
        .merge(auth::router())
        .merge(locked)
        .merge(admin)
        .with_state(Arc::clone(&state));
    service
        .merge(game)
        .layer(middleware::from_fn(csrf::reject_cross_site))
        .layer(middleware::from_fn_with_state(state, setup_gate))
        .layer(tower_http::trace::TraceLayer::new_for_http())
}
