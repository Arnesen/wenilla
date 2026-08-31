//! `benilla-protocol` — WoW 1.12.1 (build 5875) auth + world wire protocol.
//!
//! Auth/SRP6 and the world header crypto come from `benilla-srp`; message (de)serialization is
//! in-repo ([`auth`] for the realmd login protocol, [`world`]/[`events`] for the world protocol) —
//! decision 0021. 1.12.1 uses **login protocol version 3**. This crate owns the session logic; the
//! async↔ECS bridge lives in `benilla`. Phase 3: SRP6 logon + realm
//! list.

pub mod auth;
pub mod events;
pub mod guid;
pub mod messages;
pub mod transport;
pub mod wire;
pub mod world;
pub use auth::AuthReject;
pub use events::{
    decode, CharAction, EntityKind, LoginRefusal, LoginStage, MoveSpeeds, Poll, SessionEnd,
    SessionEvent,
};
pub use messages::{
    CharCreateReq, CharEnumItem, Character, CorpseLook, CreateSpline, ItemInfo, JumpInfo,
    MonsterMoveFacing, MoveMode, ObjectFields, OwnerFallback, ServerPacket, SpeedKind,
    TransportPose, CHARACTER_FLAG_GHOST, CHARACTER_FLAG_HIDE_CLOAK, CHARACTER_FLAG_HIDE_HELM,
    CHARACTER_FLAG_RENAME,
};
pub use world::{
    WardenRequired, WorldAuthReject, WorldReader, WorldSession, WorldWriter, WORLD_PORT,
};

use anyhow::{anyhow, Context, Result};
use benilla_srp::{NormalizedString, PublicKey, SrpClientChallenge, SESSION_KEY_LENGTH};

use crate::transport::Conn;

/// The realmd (auth/login) server port — the stock one a vmangos `realmd` listens on, which our
/// deploy maps straight through (`3724:3724` in the compose file at `vmangos-deploy`).
pub const AUTH_PORT: u16 = 3724;
/// The 1.12.1 client build we present to the server.
pub const CLIENT_BUILD: u16 = 5875;
/// How many logon challenges [`logon`] will ask for while looking for a `B` both serialization
/// conventions read the same way (see the redial comment there). One dial in ~137 comes back
/// ambiguous, so eight is already a probability of about 10⁻¹⁷ of running out.
const MAX_CHALLENGE_DIALS: u32 = 8;

/// Split an optional `:port` off a host string (`play.example.com:3724`). A bare host — or one
/// whose suffix isn't a port, e.g. a raw IPv6 address like `::1` — comes back intact with
/// `default`.
pub fn host_port(host: &str, default: u16) -> (&str, u16) {
    match host.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse::<u16>() {
            Ok(p) => (h, p),
            Err(_) => (host, default),
        },
        _ => (host, default),
    }
}

/// The dial never got a socket — **and which of the two reasons it was**.
///
/// This distinction only started mattering when the realmlist became something a player types
/// (decision 1667). Before that the address was a constant and "Unable to connect" could only mean
/// one thing; afterwards it means either *you typed a name that does not exist* or *the address is
/// fine and nothing is answering on it*, and a player with no way to tell them apart edits a
/// correct address over and over. That is exactly what the first live use of the editor produced.
///
/// The split is drawn where it is honest — resolution and connection are two separate operations,
/// so we do them separately rather than reading tea leaves out of one `io::Error` (a DNS failure
/// surfaces as `ErrorKind::Uncategorized` on macOS, so the kind cannot carry this).
#[derive(Debug, Clone)]
pub struct DialFailure {
    /// What we tried to reach, as the player would recognise it (`host:port`).
    pub address: String,
    /// `true` = the host name resolved to nothing; `false` = it resolved and the connection failed.
    pub unresolved: bool,
}

impl std::fmt::Display for DialFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.unresolved {
            write!(f, "cannot find a server called {}", self.address)
        } else {
            write!(f, "nothing answered at {}", self.address)
        }
    }
}

impl std::error::Error for DialFailure {}

/// Resolve `host:port` and open a socket to it, reporting the two failures apart ([`DialFailure`]).
///
/// `to_socket_addrs` is the resolution step on its own; an empty result is a name that resolved to
/// no addresses, which is as much a "cannot find it" as an outright lookup error. Connecting to the
/// resolved list (rather than to `(host, port)` again) also means the OS does not repeat the
/// lookup, and that every address the name carries is tried — the `localhost` → `::1` then
/// `127.0.0.1` case on a machine whose server binds only IPv4.
async fn dial(host: &str, port: u16) -> Result<Conn> {
    let address = format!("{host}:{port}");
    // Resolution is a separate step natively so the two failures stay apart. On the web the
    // page never resolves anything — the host proxies the dial — so a failure there can only be
    // "nothing answered".
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::net::ToSocketAddrs;
        match (host, port).to_socket_addrs().map(|mut a| a.next().is_some()) {
            Ok(true) => {}
            Ok(false) => {
                return Err(anyhow::Error::new(DialFailure {
                    address,
                    unresolved: true,
                }))
            }
            Err(e) => {
                return Err(anyhow::Error::new(DialFailure {
                    address,
                    unresolved: true,
                })
                .context(format!("resolving {host}: {e}")))
            }
        }
    }
    Conn::connect(host, port).await.map_err(|e| {
        anyhow::Error::new(DialFailure {
            address,
            unresolved: false,
        })
        .context(format!("connecting: {e}"))
    })
}

/// A realm as advertised by the auth server's realm list.
#[derive(Debug, Clone)]
pub struct RealmInfo {
    pub name: String,
    /// `host:port` of the world server, as the client would connect to it.
    pub address: String,
    pub population: String,
    pub characters: u8,
    /// The wire's `realm_type` (the list "icon"): 0 Normal, 1 PvP, 6 RP, 8 RPPvP — the select
    /// screen's `(PVP)`/`(RP)` realm-name suffix keys on it.
    pub realm_type: u32,
}

/// Result of a successful logon: the SRP6 session key (carried into the world server) + realms.
pub struct Logon {
    pub session_key: [u8; SESSION_KEY_LENGTH],
    pub realms: Vec<RealmInfo>,
}

/// Perform the full SRP6 logon against a vanilla `realmd` and fetch the realm list — the blocking
/// twin of [`logon_async`], for the CLIs/probes and the native net thread.
#[cfg(not(target_arch = "wasm32"))]
pub fn logon(host: &str, username: &str, password: &str) -> Result<Logon> {
    futures_lite::future::block_on(logon_async(host, username, password))
}

/// Perform the full SRP6 logon against a vanilla `realmd` and fetch the realm list.
///
/// Flow: logon challenge → server challenge (B, g, N, salt) → SRP6 → logon proof → verify the
/// server's proof (M2) → request + read the realm list.
pub async fn logon_async(host: &str, username: &str, password: &str) -> Result<Logon> {
    // `host` may carry an explicit `:port` (`WOW_HOST=play.example.com:3724`); bare hosts get
    // [`AUTH_PORT`].
    let (host, port) = host_port(host, AUTH_PORT);

    let username_n =
        NormalizedString::new(username).map_err(|e| anyhow!("invalid username: {e}"))?;
    let password_n =
        NormalizedString::new(password).map_err(|e| anyhow!("invalid password: {e}"))?;

    // 1-2. Logon challenge → the server's SRP6 inputs. The account name is sent uppercased (matching
    // the SRP6 calculation).
    //
    // `B` is the one hashed value benilla does not draw, and ~1 in 137 of them is one the client and
    // the mangos family serialize differently — a correct password answered with 0x04 (see
    // `benilla-srp`, "Encoding-unambiguous handshakes"). Redial for a fresh `B` when that lands: the
    // challenge is abandoned before any proof goes out, and realmd counts nothing at this stage for a
    // known account (`AuthSocket::_HandleLogonChallenge`), so a redial is free and invisible.
    let (mut stream, reply, server_public_key) = {
        let mut dialed = None;
        for _ in 0..MAX_CHALLENGE_DIALS {
            let mut stream = dial(host, port).await?;
            auth::write_logon_challenge(&mut stream, &username.to_uppercase(), CLIENT_BUILD)
                .context("sending logon challenge")?;
            let reply = auth::read_challenge_reply(&mut stream)
                .await
                .context("reading logon challenge reply")?;
            let server_public_key = PublicKey::from_le_bytes(reply.server_public_key)
                .map_err(|e| anyhow!("invalid server public key: {e}"))?;
            let stable = server_public_key.is_width_stable();
            dialed = Some((stream, reply, server_public_key));
            if stable {
                break;
            }
        }
        // Every dial ran out ambiguous (p ≈ 10⁻¹⁷): send the last anyway rather than inventing a
        // failure the server never gave us.
        dialed.expect("MAX_CHALLENGE_DIALS is non-zero")
    };

    // 3. SRP6 (WoW flavor) — compute A, M1, and the session key.
    let challenge = SrpClientChallenge::new(
        username_n,
        password_n,
        reply.generator,
        reply.large_safe_prime,
        server_public_key,
        reply.salt,
    );

    // 4. Logon proof.
    auth::write_logon_proof(
        &mut stream,
        challenge.client_public_key(),
        challenge.client_proof(),
        &reply.crc_salt,
    )
    .context("sending logon proof")?;

    // 5. Verify the server's proof (M2); on success we hold the session key.
    let server_proof = auth::read_proof_reply(&mut stream)
        .await
        .context("reading logon proof reply")?;
    let client = challenge
        .verify_server_proof(server_proof)
        .map_err(|e| anyhow!("server proof mismatch (wrong password?): {e}"))?;
    let session_key = *client.session_key();

    // 6. Realm list.
    auth::write_realm_list_request(&mut stream).context("requesting realm list")?;
    let realms = auth::read_realm_list(&mut stream)
        .await
        .context("reading realm list")?;

    Ok(Logon {
        session_key,
        realms,
    })
}

#[cfg(test)]
mod host_port_tests {
    use super::{host_port, AUTH_PORT};

    #[test]
    fn explicit_port_splits() {
        // Explicit port beats the default — they differ so the assert can tell them apart.
        assert_eq!(
            host_port("play.example.com:5000", AUTH_PORT),
            ("play.example.com", 5000)
        );
    }

    #[test]
    fn bare_host_gets_default() {
        assert_eq!(host_port("localhost", AUTH_PORT), ("localhost", AUTH_PORT));
    }

    #[test]
    fn raw_ipv6_is_not_split() {
        assert_eq!(host_port("::1", 3724), ("::1", 3724));
    }

    #[test]
    fn non_numeric_suffix_stays_host() {
        assert_eq!(
            host_port("play.example.com:realmd", 3724),
            ("play.example.com:realmd", 3724)
        );
    }
}
