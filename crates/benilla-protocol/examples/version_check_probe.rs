//! `version_check_probe` — the B241 instrument (decision 1263): does this realmd enforce
//! `StrictVersionCheck`, and does our logon proof satisfy it?
//!
//! Runs the same handshake **twice against the same server**, differing only in the proof packet's
//! `crc_hash`:
//!
//! - **zeros** — what benilla sent for its whole life before 1263. A strict realmd answers this with
//!   `WOW_FAIL_VERSION_INVALID` (0x09) and logs *"tried to login with modified client!"*; 0x09 is the
//!   code our login screen renders as `AUTH_VERSION_MISMATCH` / "Wrong client version", which is
//!   exactly how B241 was reported.
//! - **computed** — `SHA1(A ‖ H)` with the integrity digest for our build and OS
//!   ([`auth::version_proof`]).
//!
//! A non-strict server accepts both arms (it never looks), so the probe reports *what the server
//! enforces* as much as what we send. That is the point: it is the A/B that closes B241 and the one
//! to run against any server a strict-mode rejection is reported from.
//!
//! Usage: `cargo run -p benilla-protocol --example version_check_probe -- <host[:port]> <user> <pass>`
//! (the local strict-mode realmd of decision 1263 is `127.0.0.1:3725`; the ordinary one is 3724.)

use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use benilla_protocol::{auth, host_port, AuthReject, AUTH_PORT, CLIENT_BUILD};
use benilla_srp::{NormalizedString, PublicKey, SrpClientChallenge};

/// Which `crc_hash` the arm puts in the proof packet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    /// Twenty zeros — pre-1263 benilla. Forced by handing the writer a `crc_salt` we hold no digest
    /// for, which is precisely the "no answer" path.
    Zeros,
    /// `SHA1(A ‖ H)` for the salt the server actually sent.
    Computed,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::Zeros => "zeros    (pre-1263)",
            Arm::Computed => "computed (1263)   ",
        }
    }
}

/// One full handshake, sending `arm`'s `crc_hash`. `Ok(None)` = accepted, `Ok(Some(code))` = the
/// server's `WOW_FAIL_*` byte.
fn attempt(host: &str, port: u16, user: &str, pass: &str, arm: Arm) -> Result<Option<u8>> {
    let user_n = NormalizedString::new(user).map_err(|e| anyhow!("invalid username: {e}"))?;
    let pass_n = NormalizedString::new(pass).map_err(|e| anyhow!("invalid password: {e}"))?;

    // Redial past an ambiguously-serialized `B` exactly as `logon` does, so a 0x04 here means a bad
    // password and never an encoding coin-flip (see `benilla-srp`, "Encoding-unambiguous handshakes").
    for _ in 0..8 {
        let mut stream = TcpStream::connect((host, port))
            .with_context(|| format!("connecting to {host}:{port}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        auth::write_logon_challenge(&mut stream, &user.to_uppercase(), CLIENT_BUILD)
            .context("sending logon challenge")?;
        let reply = auth::read_challenge_reply(&mut stream).context("reading challenge reply")?;
        let b = PublicKey::from_le_bytes(reply.server_public_key)
            .map_err(|e| anyhow!("invalid server public key: {e}"))?;
        if !b.is_width_stable() {
            continue;
        }

        let challenge = SrpClientChallenge::new(
            user_n,
            pass_n,
            reply.generator,
            reply.large_safe_prime,
            b,
            reply.salt,
        );
        let salt = match arm {
            Arm::Computed => reply.crc_salt,
            Arm::Zeros => [0u8; 16],
        };
        auth::write_logon_proof(
            &mut stream,
            challenge.client_public_key(),
            challenge.client_proof(),
            &salt,
        )
        .context("sending logon proof")?;

        return match auth::read_proof_reply(&mut stream) {
            Ok(_) => Ok(None),
            Err(e) => match e.downcast_ref::<AuthReject>() {
                Some(r) => Ok(Some(r.code)),
                None => Err(e),
            },
        };
    }
    Err(anyhow!("eight dials, every `B` ambiguous — try again"))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [host_arg, user, pass] = match args.as_slice() {
        [h, u, p] => [h.clone(), u.clone(), p.clone()],
        _ => {
            eprintln!("usage: version_check_probe <host[:port]> <user> <pass>");
            std::process::exit(2);
        }
    };
    let (host, port) = host_port(&host_arg, AUTH_PORT);

    // Report the challenge itself first: our digest is only valid for the one salt every mangos-family
    // realmd sends, so a server issuing anything else explains a `zeros` result on the computed arm.
    let mut stream =
        TcpStream::connect((host, port)).with_context(|| format!("connecting to {host}:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    auth::write_logon_challenge(&mut stream, &user.to_uppercase(), CLIENT_BUILD)?;
    let crc_salt = auth::read_challenge_reply(&mut stream)?.crc_salt;
    drop(stream); // no proof follows, so realmd records nothing for this dial
    let hex: String = crc_salt.iter().map(|b| format!("{b:02x}")).collect();
    let known = auth::version_proof(&crc_salt, &[0u8; 32]) != [0u8; 20];
    println!("{host}:{port} — build {CLIENT_BUILD}, crc_salt {hex}");
    println!(
        "  we {} an integrity digest for that challenge\n",
        if known { "HOLD" } else { "hold NO" }
    );

    for arm in [Arm::Zeros, Arm::Computed] {
        let verdict = match attempt(host, port, &user, &pass, arm)? {
            None => "ACCEPTED".to_string(),
            Some(0x09) => {
                "REJECTED 0x09 WOW_FAIL_VERSION_INVALID (\"Wrong client version\")".into()
            }
            Some(c) => format!("REJECTED {c:#04x}"),
        };
        println!("  crc_hash {} → {verdict}", arm.label());
    }
    Ok(())
}
