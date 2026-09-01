//! The `--www` route's response headers. Unlike `tests/data.rs` this needs no game data — the
//! served directory is three bytes written into a temp dir — so it runs everywhere, which is the
//! point: the cross-origin isolation headers are the kind of thing a later refactor drops
//! silently and nobody notices until threads stop being constructible.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// One socket round trip, returning the status line and the raw header block. Async rather than
/// `std::net` for the same reason `tests/data.rs` gives: the server under test is a
/// `tokio::spawn`ed task on this test's own current-thread runtime, and a blocking read here
/// would starve it of the only thread it has.
async fn head_block(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header/body separator");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head
        .lines()
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("status code is numeric");
    (status, head.to_ascii_lowercase())
}

#[tokio::test]
async fn serves_the_client_cross_origin_isolated() {
    let www = std::env::temp_dir().join(format!("wenilla-static-{}", std::process::id()));
    std::fs::create_dir_all(&www).expect("create www dir");
    std::fs::write(www.join("index.html"), "<!doctype html>ok").expect("write index");

    let app = wenilla_host::static_site::router(&www);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (status, head) = head_block(addr, "/index.html").await;
    assert_eq!(status, 200);

    // Both, and both with these exact values: `SharedArrayBuffer` is gated on the *pair*, so
    // either one alone is worth nothing and would still pass a laxer assertion.
    assert!(
        head.contains("cross-origin-opener-policy: same-origin"),
        "missing or wrong COOP:\n{head}"
    );
    assert!(
        head.contains("cross-origin-embedder-policy: require-corp"),
        "missing or wrong COEP:\n{head}"
    );
    // The pre-existing contract, asserted here so the added layers can't quietly displace it
    // (`ServiceBuilder` order is easy to get wrong and the failure is invisible in dev).
    assert!(
        head.contains("cache-control: no-cache"),
        "missing no-cache:\n{head}"
    );

    std::fs::remove_dir_all(&www).ok();
}
