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
//! Give it an **install directory** as a fourth argument and it also derives that install's own
//! integrity digest the way the real client does — HMAC-SHA1 keyed by the challenge over five
//! binaries (decision 1265, mechanism in wow-re `system/net/scratch/logon-integrity-hash.md`) — and
//! says whether it is the stock one. Diagnostic only: benilla always sends the published constant,
//! so a patched `WoW.exe` shows up here without changing a single login.
//!
//! Usage: `cargo run -p benilla-protocol --example version_check_probe -- <host[:port]> <user> <pass> [install-dir]`
//! (the local strict-mode realmd of decision 1263 is `127.0.0.1:3725`; the ordinary one is 3724.)

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
// The login readers are awaited (the transport seam, so the same code runs in a browser); this
// probe stays a straight-line CLI and drives each of them to completion where it stands.
use benilla_protocol::transport::Conn;
use benilla_protocol::{auth, host_port, AuthReject, AUTH_PORT, CLIENT_BUILD};
use futures_lite::future::block_on;
use benilla_srp::{NormalizedString, PublicKey, SrpClientChallenge};
use sha1::{Digest, Sha1};

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
        let mut stream = block_on(Conn::connect(host, port))
            .with_context(|| format!("connecting to {host}:{port}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        auth::write_logon_challenge(&mut stream, &user.to_uppercase(), CLIENT_BUILD)
            .context("sending logon challenge")?;
        let reply = block_on(auth::read_challenge_reply(&mut stream))
            .context("reading challenge reply")?;
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

        return match block_on(auth::read_proof_reply(&mut stream)) {
            Ok(_) => Ok(None),
            Err(e) => match e.downcast_ref::<AuthReject>() {
                Some(r) => Ok(Some(r.code)),
                None => Err(e),
            },
        };
    }
    Err(anyhow!("eight dials, every `B` ambiguous — try again"))
}

/// The five files the real client hashes, **in this order** — `ChecksumExecutables` at `0x5b1170`,
/// walking the `char*` table at `0x85def0` (wow-re `system/net/scratch/logon-integrity-hash.md`).
/// Order is load-bearing: exactly 1 of the 120 permutations reproduces the published digest.
const SCANNED: [&str; 5] = [
    "WoW.exe",
    "fmod.dll",
    "ijl15.dll",
    "dbghelp.dll",
    "unicows.dll",
];

/// HMAC-SHA1 with the 16-byte `crc_salt` as the key — the framing the client uses (`rep stos` of
/// 64×`0x36` / 64×`0x5c`, salt XORed into both, `0x5b117e`–`0x5b11b6`). No long-key shortening,
/// because the key is always the 16-byte salt; the client has no such step either.
fn hmac_sha1(key: &[u8; 16], msg: &[u8]) -> [u8; 20] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let mut inner = Sha1::new();
    inner.update(ipad);
    inner.update(msg);
    let mut outer = Sha1::new();
    outer.update(opad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Derive `H` from a 1.12.1 install the way the real client does, and say what it means. Purely a
/// diagnostic: benilla sends the published constant for its build and OS (decision 1265), never this.
/// A file that cannot be read is **skipped**, contributing no bytes — the client's own behaviour
/// (four early-outs at `0x5b11eb`/`0x5b1212`/`0x5b121f`/`0x5b1246`), and the reason a Data-only
/// install could never answer this challenge for itself.
fn report_install(install_dir: &Path, crc_salt: &[u8; 16]) {
    println!("\n  install {}", install_dir.display());
    let mut msg = Vec::new();
    let mut missing = Vec::new();
    for name in SCANNED {
        match std::fs::read(install_dir.join(name)) {
            Ok(bytes) => {
                println!("    {name:<12} {} bytes", bytes.len());
                msg.extend_from_slice(&bytes);
            }
            Err(e) => {
                println!("    {name:<12} UNREADABLE ({e}) — skipped, as the client skips it");
                missing.push(name);
            }
        }
    }
    let h = hmac_sha1(crc_salt, &msg);
    let hex: String = h.iter().map(|b| format!("{b:02X}")).collect();
    println!("    H = {hex}");
    // What we actually send is `SHA1(A ‖ H)`, so compare the digests through that: equal proofs for
    // one `A` means equal `H`.
    let a = [0u8; 32];
    let matches = {
        let mut sha = Sha1::new();
        sha.update(a);
        sha.update(h);
        let derived: [u8; 20] = sha.finalize().into();
        derived == auth::version_proof(crc_salt, &a)
    };
    println!(
        "    → {}",
        if matches {
            "matches the digest we send: this install is the stock one for the OS we declare"
        } else if !missing.is_empty() {
            "differs, and files are missing — an install like this could never answer the \
             challenge for itself, which is why we send a constant instead"
        } else {
            "differs from the digest we send. On macOS that is expected and harmless: we declare \
             OSX, so we send the Mac client's digest, while your install is the Windows one. \
             Otherwise one of the five is patched. Login is unaffected either way — the digest we \
             send does not come from here"
        }
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (host_arg, user, pass, install) = match args.as_slice() {
        [h, u, p] => (h.clone(), u.clone(), p.clone(), None),
        [h, u, p, i] => (h.clone(), u.clone(), p.clone(), Some(PathBuf::from(i))),
        _ => {
            eprintln!("usage: version_check_probe <host[:port]> <user> <pass> [install-dir]");
            std::process::exit(2);
        }
    };
    let (host, port) = host_port(&host_arg, AUTH_PORT);

    // Report the challenge itself first: our digest is only valid for the one salt every mangos-family
    // realmd sends, so a server issuing anything else explains a `zeros` result on the computed arm.
    let mut stream = block_on(Conn::connect(host, port))
        .with_context(|| format!("connecting to {host}:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    auth::write_logon_challenge(&mut stream, &user.to_uppercase(), CLIENT_BUILD)?;
    let crc_salt = block_on(auth::read_challenge_reply(&mut stream))?.crc_salt;
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
    if let Some(dir) = install {
        report_install(&dir, &crc_salt);
    }
    Ok(())
}
