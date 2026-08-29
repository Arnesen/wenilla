//! Static hosting for the browser build's output directory (`index.html`, `benilla_web.js`,
//! `benilla_web_bg.wasm`) — everything `scripts/benilla-web.sh build` writes to `web/dist/`.
//!
//! Everything under `--www` gets `no-cache` rather than the immutable long cache the `/data/*`
//! route uses — that directory holds only `index.html`/`benilla_web.js`/`benilla_web_bg.wasm`,
//! and the wasm bundle changes on every rebuild but keeps the same filename (no content-hash),
//! so a long cache would pin a stale client against a server that has already moved on — dev-
//! cycle friction the `/data/*` files don't have (those are content the game itself never
//! changes).

use std::path::Path;

use axum::http::HeaderValue;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

pub fn router(www: &Path) -> axum::Router {
    let no_cache = HeaderValue::from_static("no-cache");
    // Precompressed, not compressed on the fly: the wasm is ~90 MB, and brotli-ing that per
    // request is seconds of CPU for every page load. `scripts/benilla-web.sh build` writes the
    // `.br`/`.gz` siblings once; a browser that accepts neither gets the plain file.
    let serve = ServeDir::new(www)
        .append_index_html_on_directories(true)
        .precompressed_br()
        .precompressed_gzip();
    axum::Router::new().fallback_service(
        tower::ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                axum::http::header::CACHE_CONTROL,
                no_cache,
            ))
            .service(serve),
    )
}
