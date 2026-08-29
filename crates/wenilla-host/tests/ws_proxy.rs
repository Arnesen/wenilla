//! Exercises the `/ws/{port}` proxy against a plain TCP echo server — no real mangosd needed, so
//! this runs anywhere (the box's actual 3724/8085 are used for the manual smoke test only, per
//! the plan).

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

async fn spawn_echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn relays_binary_frames_both_ways_and_closes_with_the_upstream() {
    let echo_port = spawn_echo_server().await;
    // The allowlist is by port, so the test builds its own router with the echo port allowed —
    // production's fixed {3724, 8085} set lives in main.rs, not in ws::router itself.
    let app = wenilla_host::ws::router("127.0.0.1", [echo_port]);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind proxy");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let url = format!("ws://{addr}/ws/{echo_port}?host=test");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");
    let (mut tx, mut rx) = ws.split();

    for i in 0u8..3 {
        let payload = vec![i; 16];
        tx.send(Message::Binary(payload.clone().into()))
            .await
            .expect("send frame");
        let echoed = rx.next().await.expect("frame").expect("ws frame ok");
        assert_eq!(echoed.into_data().as_ref(), payload.as_slice());
    }

    // A forbidden port is rejected before the upgrade even starts.
    let forbidden = format!("ws://{addr}/ws/{}", echo_port.wrapping_add(1));
    let err = tokio_tungstenite::connect_async(&forbidden)
        .await
        .expect_err("non-allowlisted port must be refused");
    assert!(
        err.to_string().contains("403") || err.to_string().to_lowercase().contains("forbidden"),
        "unexpected error: {err}"
    );

    // `SplitSink`/`SplitStream` share the one underlying connection through a lock (that's how
    // `.split()` works) — dropping just the sink half releases the write lock but sends nothing
    // and closes nothing, so the proxy's read loop would never see EOF and this would hang
    // forever (caught the hard way: the first version of this test did exactly that). Sending an
    // actual Close frame is what tears the connection down: the proxy's `ws_rx.next()` sees it,
    // ends the ws->tcp task, which drops the TCP write half, which the echo server reads as EOF —
    // closing its side too, which axum then answers with its own Close frame back to us.
    tx.close().await.expect("close ws");
    let end = rx.next().await;
    assert!(
        end.is_none() || matches!(end, Some(Ok(Message::Close(_)))),
        "expected the proxy to close, got {end:?}"
    );
}
