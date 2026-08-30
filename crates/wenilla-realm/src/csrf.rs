//! Two layers against cross-site requests: every HTML form carries the session's `_csrf` token
//! (checked by the handler via [`verify`]), and a global filter rejects any state-changing
//! request a browser labels `Sec-Fetch-Site: cross-site` — which also covers the JSON API and the
//! WebSocket upgrade. `SameSite=Lax` on the cookie is the third layer.

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::session::Session;
use crate::AppError;

pub fn verify(session: &Session, token: &str) -> Result<(), AppError> {
    use subtle::ConstantTimeEq;
    if session.csrf_token.as_bytes().ct_eq(token.as_bytes()).into() {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "form token mismatch — reload the page and try again",
        ))
    }
}

pub async fn reject_cross_site(req: Request, next: Next) -> Response {
    let unsafe_method = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let is_ws = req.uri().path().starts_with("/ws/");
    if unsafe_method || is_ws {
        if let Some(site) = req
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
        {
            if site == "cross-site" {
                return StatusCode::FORBIDDEN.into_response();
            }
        }
    }
    next.run(req).await
}
