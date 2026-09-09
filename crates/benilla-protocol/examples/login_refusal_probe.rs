//! Live probe: what a **refused character login** actually looks like on the wire, and what it
//! leaves behind.
//!
//! benilla used to send `CMSG_PLAYER_LOGIN` and declare itself in the world in the same breath, so
//! `SMSG_CHARACTER_LOGIN_FAILED` (0x41) reached nothing — a refusal left the client on a loading
//! screen that could never clear. The fix rests on three facts about the server, and this pins all
//! three against the local vmangos:
//!
//! 1. the refusal **arrives**, with its result byte — vmangos sends a bare `1`
//!    (`WorldSession::HandlePlayerLoginOpcode`'s `loginFailedPacket->result = 1`), which the
//!    reference's table reads as `CHAR_LOGIN_NO_WORLD`, "World server is down";
//! 2. the **session survives** it — still `STATUS_AUTHED`, still able to serve `CMSG_CHAR_ENUM`;
//! 3. a **second, valid pick still works** on the same socket, so a refusal costs the player a
//!    click and not a reconnect.
//!
//! The refusal is provoked by the one guard that needs no server state: `!packet.guid.IsPlayer()`.
//! A creature-typed guid is refused immediately and touches nothing else.
//!
//! Needs the local vmangos up; account `two`/`ptwo` (the account-X/password-pX convention).

use std::time::Duration;

use benilla_protocol::{logon, messages::ServerPacket, WorldSession, WORLD_PORT};

/// A guid the server cannot read as a player: vmangos's `ObjectGuid::IsPlayer()` tests the high
/// type nibble, and `0xF130…` is `HIGHGUID_UNIT`. Nothing on the server is looked up by it — the
/// guard rejects before any lookup — so the probe cannot disturb a real character.
const NOT_A_PLAYER: u64 = 0xF130_0000_0000_0001;

fn connect(user: &str, pass: &str) -> anyhow::Result<WorldSession> {
    let l = logon("localhost", user, pass)?;
    let addr = l
        .realms
        .first()
        .map(|r| r.address.clone())
        .unwrap_or_else(|| format!("localhost:{WORLD_PORT}"));
    WorldSession::connect(&addr, user, l.session_key)
}

fn main() -> anyhow::Result<()> {
    let (user, pass) = ("two", "ptwo");
    let mut s = connect(user, pass)?;
    let chars = s.char_enum()?;
    println!(
        "roster: {:?}",
        chars.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let target = chars.first().expect("the probe account has a character");

    // ── 1 · the refusal, and its byte ──
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.player_login(NOT_A_PLAYER)?;
    let mut refusal = None;
    for _ in 0..64 {
        match s.recv()? {
            ServerPacket::CharacterLoginFailed { result } => {
                refusal = Some(result);
                break;
            }
            other => println!("  (skipping {} while waiting)", other.name()),
        }
    }
    match refusal {
        Some(result) => println!("PASS: SMSG_CHARACTER_LOGIN_FAILED result = {result:#04x}"),
        None => anyhow::bail!("FAIL: no SMSG_CHARACTER_LOGIN_FAILED for a non-player guid"),
    }

    // ── 2 · the session survives it ──
    let again = s.char_enum()?;
    println!(
        "PASS: post-refusal roster still serves {} char(s)",
        again.len()
    );

    // ── 3 · a valid pick still works on the same socket ──
    s.player_login(target.guid)?;
    s.set_active_mover(target.guid)?;
    let mut entered = false;
    for _ in 0..64 {
        if let ServerPacket::LoginVerifyWorld { map, .. } = s.recv()? {
            println!("PASS: retry entered the world (SMSG_LOGIN_VERIFY_WORLD, map {map})");
            entered = true;
            break;
        }
    }
    if !entered {
        anyhow::bail!("FAIL: the retry never reached SMSG_LOGIN_VERIFY_WORLD");
    }
    Ok(())
}
