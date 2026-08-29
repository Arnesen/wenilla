//! `GET /ws/{port}` — the other half of the shared WebSocket scheme (Lane T ↔ Lane H): a browser
//! tab cannot open a raw TCP socket, so `benilla-protocol::transport::web::Conn` opens this
//! instead, and this proxy relays its binary frames to and from the real login (3724) / world
//! (8085) TCP ports. `port` is checked against an explicit allowlist, not just "any u16" — this
//! host runs on a Tailscale-reachable bind address, so an unchecked proxy would be an open relay
//! onto whatever else is listening on the box.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone)]
struct WsState {
    /// Host the proxy dials for an allowed port — plain hostname/IP, not a URL (mangos runs on
    /// this same box in the normal deployment, so this defaults to loopback in `main.rs`).
    upstream: Arc<str>,
    allowed: Arc<HashSet<u16>>,
}

/// Build the `/ws/{port}` router. `allowed` is the exact port set the proxy will dial — production
/// passes `{3724, 8085}` (the plan's shared scheme: "only 3724 and 8085 are allowed"); tests pass
/// their own echo-server port so the allowlist check doesn't have to be bypassed to exercise it.
pub fn router(upstream: impl Into<Arc<str>>, allowed: impl IntoIterator<Item = u16>) -> Router {
    Router::new().route("/ws/{port}", get(upgrade)).with_state(WsState {
        upstream: upstream.into(),
        allowed: Arc::new(allowed.into_iter().collect()),
    })
}

/// `host` arrives as `?host=` — logging only (the shared scheme's own words); the proxy always
/// dials `state.upstream`, never the client-supplied value, so a client can't redirect the socket
/// somewhere else on the network.
async fn upgrade(
    State(state): State<WsState>,
    Path(port): Path<u16>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    if !state.allowed.contains(&port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let upstream = Arc::clone(&state.upstream);
    let host_label = params.get("host").cloned().unwrap_or_default();
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = relay(socket, &upstream, port).await {
            tracing::warn!(error = %e, host = %host_label, port, "ws proxy session ended");
        }
    })
}

/// Relay one session: dial the upstream TCP port, then pump bytes both directions until both
/// sides have closed. "One TCP read chunk = one binary frame, any frame = one `write_all`"
/// (shared scheme) — no re-framing or buffering beyond the OS's own read/write granularity.
///
/// Each direction propagates its own end-of-stream to the *other* transport, rather than the two
/// futures racing in a `select!`: a `select!` here would drop whichever direction lost the race
/// mid-flight, which for the ws->tcp direction means the client's Close frame never gets echoed
/// back (tungstenite then reports a `ResetWithoutClosingHandshake` protocol error instead of a
/// clean close — caught by `tests/ws_proxy.rs`'s close-handshake test hanging, then failing,
/// before this shape). `join!` runs both to their own natural end instead:
/// receiving a WS Close (or a read error) shuts down our TCP write half, which the upstream reads
/// as EOF and — for a well-behaved peer — closes its own side, which our TCP read then sees as
/// EOF and answers with our own Close frame back to the client.
async fn relay(ws: WebSocket, upstream: &str, port: u16) -> anyhow::Result<()> {
    let tcp = TcpStream::connect((upstream, port)).await?;
    tcp.set_nodelay(true)?;
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let tcp_to_ws = async {
        let mut buf = [0u8; 65536];
        loop {
            match tcp_r.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    // If the client already sent its own Close, tungstenite auto-queues a reply
                    // the moment `ws_rx` reads it (the behaviour `Message::Close`'s docs mention:
                    // "axum will automatically respond with a close frame if necessary") — but
                    // only *queues* it; nothing has driven a write-side poll since, so it's still
                    // sitting unflushed. `send` here fails with a "send after closing" protocol
                    // error in that case (there's already a Close in flight); flushing instead is
                    // what actually puts those bytes on the wire. Without this the socket would
                    // just drop with the reply never sent — tungstenite calls that a
                    // `ResetWithoutClosingHandshake`, and it's exactly the shape
                    // `tests/ws_proxy.rs`'s close test hit before this fallback existed.
                    if ws_tx.send(Message::Close(None)).await.is_err() {
                        let _ = ws_tx.flush().await;
                    }
                    break;
                }
                Ok(n) => {
                    if ws_tx.send(Message::binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    };

    let ws_to_tcp = async {
        loop {
            match ws_rx.next().await {
                Some(Ok(Message::Binary(data))) => {
                    if tcp_w.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    if tcp_w.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            }
        }
        // Half-close our write side so the upstream sees EOF, not just a dangling socket.
        let _ = tcp_w.shutdown().await;
    };

    tokio::join!(tcp_to_ws, ws_to_tcp);
    Ok(())
}
