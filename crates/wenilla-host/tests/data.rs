//! Exercises the `/data/*` route against the *real* MPQ chain — set `WOW_DATA` to the client's
//! `Data` directory to run it for real; skips (not fails) when no install is found, so `cargo
//! test` stays green on a box without the 5.1 GB of game data checked out.

use std::sync::Arc;

use benilla_formats::Chain;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A GET/HEAD with no request body needs no HTTP client crate — one socket round trip. Async, not
/// `std::net` blocking I/O: the server under test runs as a `tokio::spawn`ed task on this same
/// `#[tokio::test]` runtime, and a blocking call here would starve that task of the single
/// current-thread executor's only thread — a deadlock, not a slow test (caught the hard way: the
/// first version of this test hung until an outer `timeout` killed it).
async fn raw_request(addr: std::net::SocketAddr, method: &str, path: &str) -> (u16, Vec<u8>) {
    let (status, _head, body) = raw_request_with(addr, method, path, "").await;
    (status, body)
}

/// As above, plus arbitrary extra request headers and the response's header block (lowercased)
/// — what the content-negotiation assertions need and the original two did not.
async fn raw_request_with(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra: &str,
) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{extra}\r\n"
            )
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
    (status, head.to_ascii_lowercase(), raw[split + 4..].to_vec())
}

#[tokio::test]
async fn serves_a_known_file_and_404s_a_missing_one() {
    let Some(data_dir) = benilla_formats::wow_data() else {
        eprintln!("skip: no WoW Data directory found (set WOW_DATA)");
        return;
    };
    let chain = Arc::new(Chain::open(&data_dir).expect("open chain"));
    let app = wenilla_host::data::router(chain);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // `Interface\Glues\Common\Glue-Panel-Button-Up.blp` -> one percent-encoded path segment,
    // `\` -> `%5C`, per the shared Data URL scheme.
    let (status, body) = raw_request(
        addr,
        "GET",
        "/data/Interface%5CGlues%5CCommon%5CGlue-Panel-Button-Up.blp",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(&body[..4], b"BLP2");

    let (status, _) = raw_request(addr, "GET", "/data/nope.x").await;
    assert_eq!(status, 404);
}

/// The compression contract end to end, against the real chain: the name index — the single
/// largest uncompressed body this route ever served, and the one `web/boot-manifest.json` now
/// pulls during boot — comes back encoded, while a BLP does not. The unit tests in
/// `src/data.rs` pin the predicate and the classifier; this pins that they are actually wired
/// to the bytes on the socket.
#[tokio::test]
async fn compresses_the_index_and_leaves_blp_texels_alone() {
    let Some(data_dir) = benilla_formats::wow_data() else {
        eprintln!("skip: no WoW Data directory found (set WOW_DATA)");
        return;
    };
    let chain = Arc::new(Chain::open(&data_dir).expect("open chain"));
    let app = wenilla_host::data::router(chain);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (status, head, _) =
        raw_request_with(addr, "GET", "/data/__index", "accept-encoding: br\r\n").await;
    assert_eq!(status, 200);
    assert!(
        head.contains("content-encoding: br"),
        "the index should compress:\n{head}"
    );

    let (status, head, _) = raw_request_with(
        addr,
        "GET",
        "/data/Interface%5CGlues%5CCommon%5CGlue-Panel-Button-Up.blp",
        "accept-encoding: br\r\n",
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        !head.contains("content-encoding:"),
        "DXT blocks should not be re-compressed:\n{head}"
    );
}
