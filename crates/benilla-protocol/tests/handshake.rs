//! The world handshake against a fake server, exercising [`WorldSession::connect`] over a real
//! socket with real header obfuscation.
//!
//! What these pin down is the packet *ordering* tolerance the handshake needs. A server does not
//! promise `SMSG_AUTH_RESPONSE` is the first encrypted packet — it interleaves its own traffic —
//! and one of those interleaved packets, `SMSG_WARDEN_DATA`, means the server runs an anticheat we
//! cannot answer and must be refused at login rather than entered and kicked ~30 s later.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use benilla_protocol::messages::opcode;
use benilla_protocol::{messages, WardenRequired, WorldSession};
use benilla_srp::vanilla_header::HeaderCrypto;
use benilla_srp::SESSION_KEY_LENGTH;

const SESSION_KEY: [u8; SESSION_KEY_LENGTH] = [7u8; SESSION_KEY_LENGTH];
const SERVER_SEED: u32 = 0xDEAD_BEEF;

/// Write one server packet: 2-byte BE size (counts the opcode, not itself) + 2-byte LE opcode,
/// encrypted once `crypto` is in play, then the plaintext body.
fn send(stream: &mut TcpStream, crypto: Option<&mut HeaderCrypto>, opcode: u16, body: &[u8]) {
    let size = (body.len() + 2) as u16;
    let s = size.to_be_bytes();
    let o = opcode.to_le_bytes();
    let mut header = [s[0], s[1], o[0], o[1]];
    if let Some(c) = crypto {
        c.encrypter().encrypt(&mut header);
    }
    stream.write_all(&header).unwrap();
    stream.write_all(body).unwrap();
}

/// Read the client's unencrypted `CMSG_AUTH_SESSION` (6-byte header: BE size + LE u32 opcode).
fn read_auth_session(stream: &mut TcpStream) {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header).unwrap();
    let size = u16::from_be_bytes([header[0], header[1]]) as usize;
    let mut body = vec![0u8; size - 4];
    stream.read_exact(&mut body).unwrap();
}

/// Stand up a fake world server that sends `pre` (opcode, body) pairs — encrypted, in order —
/// before a successful `SMSG_AUTH_RESPONSE`. Returns the address to point `connect` at.
fn fake_server(pre: Vec<(u16, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        send(
            &mut stream,
            None,
            opcode::SMSG_AUTH_CHALLENGE,
            &SERVER_SEED.to_le_bytes(),
        );
        read_auth_session(&mut stream);
        let mut crypto = HeaderCrypto::from_session_key(SESSION_KEY);
        for (op, body) in pre {
            send(&mut stream, Some(&mut crypto), op, &body);
        }
        send(
            &mut stream,
            Some(&mut crypto),
            opcode::SMSG_AUTH_RESPONSE,
            &[messages::AUTH_OK],
        );
        // Hold the socket open so the client's reads never see a premature EOF.
        thread::sleep(std::time::Duration::from_secs(2));
    });
    addr
}

/// The plain case: `SMSG_AUTH_RESPONSE` leads, the handshake completes.
#[test]
fn auth_response_alone_completes_the_handshake() {
    let addr = fake_server(vec![]);
    assert!(WorldSession::connect(&addr, "one", SESSION_KEY).is_ok());
}

/// A server interleaving its own traffic ahead of the auth response must not fail the handshake —
/// the real one does this, and demanding AUTH_RESPONSE lead once broke login outright.
#[test]
fn packets_ahead_of_the_auth_response_are_skipped() {
    let addr = fake_server(vec![
        (opcode::SMSG_LOGIN_VERIFY_WORLD, vec![0u8; 20]),
        (opcode::SMSG_SET_FACTION_STANDING, vec![0u8; 12]),
    ]);
    assert!(WorldSession::connect(&addr, "one", SESSION_KEY).is_ok());
}

/// Warden among them is refused, with the error the login screen shows — never a session that
/// would be kicked once the server's response clock expires.
#[test]
fn a_warden_server_is_refused_at_the_handshake() {
    let addr = fake_server(vec![(opcode::SMSG_WARDEN_DATA, vec![0u8; 16])]);
    let Err(err) = WorldSession::connect(&addr, "one", SESSION_KEY) else {
        panic!("a Warden server must not yield a session");
    };
    assert!(
        err.downcast_ref::<WardenRequired>().is_some(),
        "expected WardenRequired, got: {err:#}"
    );
}
