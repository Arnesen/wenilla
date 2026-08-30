//! Login and logout. `local` is the username/password form (M1); `provider` is the seam the
//! OAuth providers plug into (M2).

pub mod local;
pub mod provider;

use std::sync::Arc;

use axum::Router;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    local::router()
}
