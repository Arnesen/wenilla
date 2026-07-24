//! Live probe: verify the faction-language mechanism end-to-end against the local vmangos — a
//! HORDE character's say must echo back (the server accepted the tongue), a dot-command must
//! answer (it survived the pre-parse `KnowsLanguage` gate), and the split-writer path must carry
//! the tongue too. Run: `cargo run -p benilla-protocol --example horde_chat_probe -- probeN
//! pprobeN [host]` — the slot-keyed probe account (method.md "The local vmangos server"; it
//! once hardcoded the retired shared `three` identity, decision 0530). Creates the orc
//! `Orc<N-spelled>` on this account on first run (names are realm-unique, so the orc keys to
//! the slot too). Exists because a hardcoded-Common send silently ate every Horde character's
//! chat and commands (decision 0392) — this is the one-shot regression check for that whole path.

use anyhow::{bail, Context, Result};
use benilla_protocol::messages::{CharCreateReq, CHAR_CREATE_NAME_IN_USE, CHAR_CREATE_SUCCESS};
use benilla_protocol::{ServerPacket, WorldSession, WORLD_PORT};

/// The slot's Horde character name: `probe4` → `Orcfour` (letters only — digits are not legal
/// in character names, and realm-wide name uniqueness means each slot needs its own orc).
fn orc_name(user: &str) -> Option<String> {
    let n: usize = user.strip_prefix("probe")?.parse().ok()?;
    let spelled = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ]
    .get(n)?;
    Some(format!("Orc{spelled}"))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let user = args
        .next()
        .context("usage: horde_chat_probe -- <probeN> <pprobeN> [host] (slot-keyed account)")?;
    let pass = args
        .next()
        .context("usage: horde_chat_probe -- <probeN> <pprobeN> [host] (slot-keyed account)")?;
    let host = args.next().unwrap_or_else(|| "localhost".into());
    let orc = orc_name(&user)
        .context("account must be a slot-keyed probeN (method.md \"The local vmangos server\")")?;

    let logon = benilla_protocol::logon(&host, &user, &pass)?;
    let addr = logon
        .realms
        .first()
        .map(|r| r.address.clone())
        .unwrap_or_else(|| format!("{host}:{WORLD_PORT}"));
    let mut session = WorldSession::connect(&addr, &user, logon.session_key)?;

    let mut characters = session.char_enum()?;
    if !characters.iter().any(|c| c.name == orc) {
        // Orc (race 2) warrior male — the Horde case under test.
        let req = CharCreateReq {
            name: orc.clone(),
            race: 2,
            class: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        match session.create_character(&req)? {
            CHAR_CREATE_SUCCESS | CHAR_CREATE_NAME_IN_USE => {}
            other => bail!("char create failed: {other:#x}"),
        }
        characters = session.char_enum()?;
    }
    let orc = characters
        .iter()
        .find(|c| c.name == orc)
        .context("slot orc exists after create")?;
    println!("probe: logging in {} (race {})", orc.name, orc.race);
    session.player_login(orc.guid)?;
    session.set_active_mover(orc.guid)?;

    // 1. A plain say must be ACCEPTED: the server echoes our own say back (own sends are never
    //    locally echoed in vanilla). Pre-fix, a Horde say sent Common and the server dropped it
    //    with only an SMSG_NOTIFICATION.
    session.send_chat("orcish probe line")?;
    // 2. A dot-command must survive the language gate: `.gps` answers with system messages.
    session.send_chat(".gps")?;

    let mut say_echoed = false;
    let mut system_reply = false;
    let mut notified: Option<String> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !(say_echoed && system_reply) {
        match session.recv() {
            Ok(ServerPacket::MessageChat(m)) => {
                println!(
                    "probe: chat type {} lang {} text {:?}",
                    m.chat_type, m.language, m.text
                );
                if m.text == "orcish probe line" {
                    say_echoed = true;
                    println!("probe: SAY ECHOED (language {})", m.language);
                }
                // CHAT_MSG_SYSTEM = 0x0a on the inbound u8 field. Only the `.gps` answer counts
                // (the login MOTD is also SYSTEM — it must not satisfy the command check).
                if m.chat_type == 0x0a && m.text.contains("Map:") {
                    system_reply = true;
                    println!("probe: .gps REPLY: {:?}", m.text);
                }
            }
            Ok(ServerPacket::Notification { text }) => {
                println!("probe: NOTIFICATION: {text:?}");
                notified = Some(text);
            }
            Ok(_) => {}
            Err(e) => bail!("recv: {e:#}"),
        }
    }

    // 3. The same through the SPLIT writer — the path the benilla app actually sends on
    //    (`into_split` must carry the tongue across).
    let (mut reader, mut writer) = session.into_split()?;
    writer.send_chat("orcish writer line")?;
    let mut writer_echoed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !writer_echoed {
        match reader.recv() {
            Ok(ServerPacket::MessageChat(m)) if m.text == "orcish writer line" => {
                writer_echoed = true;
                println!("probe: WRITER SAY ECHOED (language {})", m.language);
            }
            Ok(ServerPacket::Notification { text }) => {
                println!("probe: NOTIFICATION: {text:?}");
                notified = Some(text);
            }
            Ok(_) => {}
            Err(e) => bail!("reader recv: {e:#}"),
        }
    }

    println!("---");
    println!("say_echoed    = {say_echoed}");
    println!("system_reply  = {system_reply}");
    println!("writer_echoed = {writer_echoed}");
    println!("notification  = {notified:?}");
    if say_echoed && system_reply && writer_echoed && notified.is_none() {
        println!("VERDICT: PASS — Horde chat + dot-commands accepted on both send paths");
        Ok(())
    } else {
        bail!("VERDICT: FAIL");
    }
}
