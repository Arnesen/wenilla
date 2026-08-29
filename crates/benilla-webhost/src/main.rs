//! `benilla-webhost` — the piece of the browser build that can't be static hosting: it serves the
//! wasm bundle, answers game-data reads out of the real MPQ chain, and proxies the two TCP ports
//! the client needs (login 3724, world 8085) over WebSocket, since a browser tab cannot open a
//! raw socket. See `docs/superpowers/plans/2026-08-29-benilla-web.md`'s "Shared interfaces" for
//! the exact URL/encoding rules the client lanes code against.
//!
//! Built up task by task (H1: data route only; H2 adds the WebSocket proxy; H3 adds static
//! hosting and the full CLI) — each stage's `main` is what its own commit's verification ran.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use benilla_formats::Chain;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Serve the benilla browser build, its game data, and its net proxy")]
struct Cli {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8090")]
    bind: String,
    /// The vanilla `Data` directory (or a single `.MPQ`) the chain opens — the same one `benilla`
    /// itself reads on the desktop.
    #[arg(long)]
    data: PathBuf,
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

    let app = benilla_webhost::data::router(chain);

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;
    tracing::info!(bind = %cli.bind, "benilla-webhost listening");
    axum::serve(listener, app).await.context("serving")
}
