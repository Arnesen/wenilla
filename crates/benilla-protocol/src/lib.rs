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
pub mod wire;
pub mod world;
pub use auth::AuthReject;
pub use events::{decode, CharAction, EntityKind, LoginStage, MoveSpeeds, Poll, SessionEvent};
pub use messages::{
    CharCreateReq, CharEnumItem, Character, CreateSpline, ItemInfo, JumpInfo, MonsterMoveFacing,
    ObjectFields, ServerPacket, SpeedKind, TransportPose, CHARACTER_FLAG_GHOST,
    CHARACTER_FLAG_HIDE_CLOAK, CHARACTER_FLAG_HIDE_HELM, CHARACTER_FLAG_RENAME,
};
pub use world::{WardenRequired, WorldReader, WorldSession, WorldWriter, WORLD_PORT};

use std::net::TcpStream;

use anyhow::{anyhow, Context, Result};
use benilla_srp::{NormalizedString, PublicKey, SrpClientChallenge, SESSION_KEY_LENGTH};

/// The realmd (auth/login) server port — the stock one a vmangos `realmd` listens on, which our
/// deploy maps straight through (`3724:3724` in the compose file at `vmangos-deploy`).
pub const AUTH_PORT: u16 = 3724;
/// The 1.12.1 client build we present to the server.
pub const CLIENT_BUILD: u16 = 5875;

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

/// Perform the full SRP6 logon against a vanilla `realmd` and fetch the realm list.
///
/// Flow: logon challenge → server challenge (B, g, N, salt) → SRP6 → logon proof → verify the
/// server's proof (M2) → request + read the realm list.
pub fn logon(host: &str, username: &str, password: &str) -> Result<Logon> {
    // `host` may carry an explicit `:port` (`WOW_HOST=play.example.com:3724`); bare hosts get
    // [`AUTH_PORT`].
    let (host, port) = host_port(host, AUTH_PORT);
    let mut stream =
        TcpStream::connect((host, port)).with_context(|| format!("connecting to {host}:{port}"))?;

    // 1. Logon challenge — the account name is sent uppercased (matching the SRP6 calculation).
    auth::write_logon_challenge(&mut stream, &username.to_uppercase(), CLIENT_BUILD)
        .context("sending logon challenge")?;

    // 2. Server challenge → SRP6 inputs.
    let reply = auth::read_challenge_reply(&mut stream).context("reading logon challenge reply")?;

    // 3. SRP6 (WoW flavor) — compute A, M1, and the session key.
    let username_n =
        NormalizedString::new(username).map_err(|e| anyhow!("invalid username: {e}"))?;
    let password_n =
        NormalizedString::new(password).map_err(|e| anyhow!("invalid password: {e}"))?;
    let server_public_key = PublicKey::from_le_bytes(reply.server_public_key)
        .map_err(|e| anyhow!("invalid server public key: {e}"))?;
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
    )
    .context("sending logon proof")?;

    // 5. Verify the server's proof (M2); on success we hold the session key.
    let server_proof = auth::read_proof_reply(&mut stream).context("reading logon proof reply")?;
    let client = challenge
        .verify_server_proof(server_proof)
        .map_err(|e| anyhow!("server proof mismatch (wrong password?): {e}"))?;
    let session_key = *client.session_key();

    // 6. Realm list.
    auth::write_realm_list_request(&mut stream).context("requesting realm list")?;
    let realms = auth::read_realm_list(&mut stream).context("reading realm list")?;

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
