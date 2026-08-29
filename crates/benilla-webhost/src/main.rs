//! `benilla-webhost` — the piece of the browser build that can't be static hosting: it serves the
//! wasm bundle, answers game-data reads out of the real MPQ chain, and proxies the two TCP ports
//! the client needs (login 3724, world 8085) over WebSocket, since a browser tab cannot open a
//! raw socket. See `docs/superpowers/plans/2026-08-29-benilla-web.md`'s "Shared interfaces" for
//! the exact URL/encoding rules the client lanes code against.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use benilla_formats::Chain;
use clap::Parser;

/// The only two ports the `/ws/{port}` proxy will ever dial — mangos realmd (login) and worldd
/// (world). Fixed here, not a CLI flag: this host can bind `0.0.0.0` on a Tailscale box, so the
/// allowlist is a security boundary, not a convenience default.
const ALLOWED_PORTS: [u16; 2] = [3724, 8085];

#[derive(Parser)]
#[command(about = "Serve the benilla browser build, its game data, and its net proxy")]
struct Cli {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8090")]
    bind: String,
    /// Directory holding the wasm-bindgen output (`index.html`, `benilla_web.js`, `*.wasm`) —
    /// `scripts/benilla-web.sh build`'s `web/dist/`.
    #[arg(long)]
    www: PathBuf,
    /// The vanilla `Data` directory (or a single `.MPQ`) the chain opens — the same one `benilla`
    /// itself reads on the desktop.
    #[arg(long)]
    data: PathBuf,
    /// Host the `/ws/{port}` proxy dials for the allowed ports — the mangos boxes this host
    /// itself runs against, so it defaults to loopback.
    #[arg(long, default_value = "127.0.0.1")]
    upstream: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    let chain = Arc::new(
        Chain::open(&cli.data).with_context(|| format!("opening {}", cli.data.display()))?,
    );
    tracing::info!(data = %cli.data.display(), "patch chain open");

    let app = benilla_webhost::data::router(chain)
        .merge(benilla_webhost::ws::router(cli.upstream.clone(), ALLOWED_PORTS))
        .merge(benilla_webhost::static_site::router(&cli.www));

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;
    tracing::info!(bind = %cli.bind, www = %cli.www.display(), upstream = %cli.upstream, "benilla-webhost listening");
    axum::serve(listener, app).await.context("serving")
}
