//! Live probe (decision 0193): the character-select park behaviors benilla's glue layer rests on.
//!
//! 1) the logout round-trip — login, logout, confirm `SMSG_LOGOUT_COMPLETE`, reconnect, confirm
//!    the roster re-serves (the app's drop-and-reconnect relist);
//! 2) idle tolerance — park an authenticated socket at character select for 130 s, then log in
//!    (verified 2026-07-07: vmangos does NOT kick a quiet parked socket, so the glue screen needs
//!    no keep-alive ping).
//!
//! Needs the local vmangos up; account `two`/`ptwo` (the account-X/password-pX convention).

use std::time::Duration;

use benilla_protocol::{logon, WorldSession, WORLD_PORT};

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

    // ── 1 · logout round-trip ──
    let mut s = connect(user, pass)?;
    let chars = s.char_enum()?;
    println!(
        "roster: {:?}",
        chars.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let c = &chars[0];
    s.player_login(c.guid)?;
    s.set_active_mover(c.guid)?;
    println!("logged in as {} — requesting logout in 2s", c.name);
    std::thread::sleep(Duration::from_secs(2));
    s.set_read_timeout(Some(Duration::from_secs(1)))?;
    s.logout(Duration::from_secs(25))?;
    println!("PASS: SMSG_LOGOUT_COMPLETE received");
    drop(s);

    // Fresh cycle re-serves the roster (the app's drop-and-reconnect relist).
    let mut s = connect(user, pass)?;
    let n = s.char_enum()?.len();
    println!("PASS: post-logout reconnect roster has {n} chars");
    drop(s);

    // ── 2 · idle park ──
    let mut s = connect(user, pass)?;
    let chars = s.char_enum()?;
    println!("parking at character select for 130s…");
    std::thread::sleep(Duration::from_secs(130));
    match s.player_login(chars[0].guid).and_then(|()| {
        s.set_active_mover(chars[0].guid)?;
        // Prove the world actually streams: wait for any post-login packet.
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.recv().map(|p| p.name())
    }) {
        Ok(pkt) => println!("PASS: idle-parked login works (first packet: {pkt})"),
        Err(e) => println!("KICKED: idle park failed login: {e:#} (self-heals via relist)"),
    }
    Ok(())
}
