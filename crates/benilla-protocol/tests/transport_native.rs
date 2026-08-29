//! The native body of the transport seam, over a real loopback socket.
//!
//! What this pins is the contract the browser body has to match: an awaited `read_exact_async`
//! fills the buffer exactly, a synchronous `write_all` reaches the peer, and a split `Conn` keeps
//! both of those working through the two halves independently. It is deliberately a socket test
//! rather than a mock — the whole point of the seam is that the native side is still a `TcpStream`
//! doing what it always did.

#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use benilla_protocol::transport::{Conn, ReadExactAsync};
use futures_lite::future::block_on;

/// Accept one connection and echo `rounds` four-byte messages back.
fn echo_server(rounds: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        for _ in 0..rounds {
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).unwrap();
            stream.write_all(&buf).unwrap();
        }
    });
    port
}

#[test]
fn a_conn_writes_and_reads_before_the_split() {
    let port = echo_server(1);
    block_on(async {
        let mut conn = Conn::connect("127.0.0.1", port).await.unwrap();
        conn.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        conn.read_exact_async(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
    });
}

#[test]
fn the_split_halves_keep_working() {
    let port = echo_server(2);
    block_on(async {
        let mut conn = Conn::connect("127.0.0.1", port).await.unwrap();
        conn.set_nodelay(true).unwrap();
        conn.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        conn.read_exact_async(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");

        // The streaming phase's shape: one half written from, the other read from.
        let (mut reader, mut writer) = conn.split().unwrap();
        writer.write_all(b"pong").unwrap();
        let mut got = [0u8; 4];
        reader.read_exact_async(&mut got).await.unwrap();
        assert_eq!(&got, b"pong");
    });
}

/// A read that outlives the peer must end as EOF, not hang — the disconnect the sequencer turns
/// into `SessionEnd::Lost`.
#[test]
fn a_closed_peer_ends_the_read() {
    let port = echo_server(0); // accepts, then drops the socket
    block_on(async {
        let mut conn = Conn::connect("127.0.0.1", port).await.unwrap();
        let mut got = [0u8; 4];
        let err = conn.read_exact_async(&mut got).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    });
}
