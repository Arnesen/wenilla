//! Diagnostic probe: measure the server's ACTUAL cooldown window for Charge (spell 100,
//! `recoveryTime 0 / category 44 / categoryRecoveryTime 15000`) against the client-rendered
//! 15 s sweep — the "I can charge before the indicator is up" report.
//!
//! Flow: log in the slot's probe warrior (`Probe<N-spelled>`), `.learn 100`, teleport into the Northshire kobold
//! cluster, Battle Stance, pick a Kobold Vermin (entry 6) from the create stream, Charge it,
//! then spam re-casts every 200 ms. vmangos checks the cooldown at the TOP of `CheckCast`
//! (`Spell.cpp:5369`), so the flip from `SPELL_FAILED_NOT_READY` (60) to any other code (or a
//! second `SMSG_SPELL_GO`) timestamps the server's cooldown end relative to the first GO.
//!
//! Run: `cargo run -p benilla-protocol --example cooldown_probe -- probeN pprobeN [host]` — the
//! slot-keyed probe account (method.md "The local vmangos server"; NEVER `one`, the director's
//! account — a probe login there kicks their live session). Two caveats: this is a COMBAT probe,
//! so it runs director-supervised (method.md: no unattended combat probes), and `.learn` is
//! SEC_DEVELOPER (5) while probe accounts are gmlevel 3 — raise the slot account via the vmangos
//! console for the run and restore it after.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use benilla_protocol::{decode, EntityKind, SessionEvent, WorldSession, WORLD_PORT};

const CHARGE: u32 = 100;
const BATTLE_STANCE: u32 = 2457;
const KOBOLD_VERMIN: u32 = 6;

fn fail_name(reason: u8) -> &'static str {
    match reason {
        0 => "AFFECTING_COMBAT",
        10 => "BAD_TARGETS",
        23 => "DONT_REPORT",
        42 => "LINE_OF_SIGHT",
        50 => "NOPATH",
        56 => "NOT_KNOWN",
        60 => "NOT_READY",
        86 => "ONLY_SHAPESHIFT",
        89 => "OUT_OF_RANGE",
        _ => "?",
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let user = args
        .next()
        .context("usage: cooldown_probe -- <probeN> <pprobeN> [host] (slot-keyed account)")?;
    let pass = args
        .next()
        .context("usage: cooldown_probe -- <probeN> <pprobeN> [host] (slot-keyed account)")?;
    let host = args.next().unwrap_or_else(|| "localhost".into());

    let logon = benilla_protocol::logon(&host, &user, &pass)?;
    let world_addr = logon
        .realms
        .first()
        .map(|r| r.address.clone())
        .unwrap_or_else(|| format!("{host}:{WORLD_PORT}"));
    let mut session = WorldSession::connect(&world_addr, &user, logon.session_key)?;
    let characters = session.char_enum()?;
    let character = characters
        .iter()
        .find(|c| c.name == "Tri")
        .or_else(|| characters.first())
        .context("no characters")?;
    let self_guid = character.guid;
    println!("logging in '{}' (guid {self_guid})", character.name);
    session.player_login(self_guid)?;
    session.set_active_mover(self_guid)?;
    session.set_read_timeout(Some(Duration::from_millis(100)))?;

    let mut kobolds: Vec<u64> = Vec::new();
    let drain = |session: &mut WorldSession,
                 kobolds: &mut Vec<u64>,
                 secs: f32,
                 mut on_event: &mut dyn FnMut(f64, SessionEvent)|
     -> Result<()> {
        let until = Instant::now() + Duration::from_secs_f32(secs);
        let t0 = Instant::now();
        while Instant::now() < until {
            let Ok(msg) = session.recv() else { continue };
            for ev in decode(msg) {
                match &ev {
                    SessionEvent::ObjectCreate {
                        guid, kind, fields, ..
                    } if matches!(kind, EntityKind::Unit)
                        && fields.object_entry() == Some(KOBOLD_VERMIN)
                        && !kobolds.contains(guid) =>
                    {
                        kobolds.push(*guid);
                    }
                    // A `.go` teleport must be acked or the server freezes us and streams nothing.
                    SessionEvent::Teleport { guid, counter, .. } => {
                        println!("  teleport → acking (counter {counter})");
                        session.teleport_ack(*guid, *counter)?;
                    }
                    _ => {}
                }
                on_event(t0.elapsed().as_secs_f64(), ev);
            }
        }
        let _ = &mut on_event;
        Ok(())
    };
    let mut quiet = |_t: f64, ev: SessionEvent| match ev {
        SessionEvent::Chat(m) => println!("  chat: {:?}", m.text),
        SessionEvent::SpellLearned { spell_id } => println!("  learned: {spell_id}"),
        _ => {}
    };

    // World-enter settle, then arm the character: learn Charge, stance, teleport to the kobolds.
    drain(&mut session, &mut kobolds, 3.0, &mut quiet)?;
    session.set_selection(self_guid)?;
    session.send_chat(".learn 100")?;
    session.send_chat(".go xyz -8785 -150 82.5")?;
    drain(&mut session, &mut kobolds, 3.0, &mut quiet)?;
    session.cast_spell(BATTLE_STANCE, None)?;
    let mut chatty = |_t: f64, ev: SessionEvent| match ev {
        SessionEvent::Chat(m) => println!("  chat: {:?}", m.text),
        SessionEvent::CastResult {
            spell_id,
            success,
            reason,
        } => println!(
            "  pre: cast_result spell={spell_id} success={success} reason={reason:?} ({})",
            reason.map(fail_name).unwrap_or("-")
        ),
        _ => {}
    };
    drain(&mut session, &mut kobolds, 2.0, &mut chatty)?;
    println!("kobolds seen: {}", kobolds.len());

    // First Charge: try candidates until one GO lands.
    let mut target: Option<u64> = None;
    let mut charge_go_at: Option<Instant> = None;
    for &guid in kobolds.clone().iter() {
        println!("charging kobold {guid:#x}…");
        session.set_selection(guid)?;
        session.cast_spell(CHARGE, Some(guid))?;
        // The anchor is stamped INSIDE the watch, at GO decode — the drain window runs its full
        // length regardless, and a window-end stamp would smear the anchor by up to 1.5 s.
        let mut verdict: Option<(bool, Instant)> = None;
        let mut watch = |_t: f64, ev: SessionEvent| match ev {
            SessionEvent::SpellGo {
                caster, spell_id, ..
            } if caster == self_guid && spell_id == CHARGE => {
                verdict = Some((true, Instant::now()));
            }
            SessionEvent::CastResult {
                spell_id,
                success: false,
                reason,
            } if spell_id == CHARGE => {
                println!(
                    "  refused: {:?} ({})",
                    reason,
                    reason.map(fail_name).unwrap_or("-")
                );
                verdict = Some((false, Instant::now()));
            }
            _ => {}
        };
        drain(&mut session, &mut kobolds, 1.5, &mut watch)?;
        if let Some((true, at)) = verdict {
            println!("  GO — cooldown armed");
            target = Some(guid);
            charge_go_at = Some(at);
            break;
        }
    }
    let target = target.context("no charge landed — no measurement")?;
    let t0 = charge_go_at.unwrap();

    // The spam loop: recast every 200 ms for 20 s; every result timestamps the server verdict.
    println!("t=0.00s — first GO received; spamming recasts…");
    let mut last_not_ready: Option<f64> = None;
    let mut first_free: Option<(f64, String)> = None;
    while t0.elapsed() < Duration::from_secs(20) {
        session.cast_spell(CHARGE, Some(target))?;
        let mut watch = |_t: f64, ev: SessionEvent| {
            let at = t0.elapsed().as_secs_f64();
            match ev {
                SessionEvent::CastResult {
                    spell_id,
                    success,
                    reason,
                } if spell_id == CHARGE => {
                    let name = reason.map(fail_name).unwrap_or("-");
                    println!(
                        "t={at:5.2}s  cast_result success={success} reason={reason:?} ({name})"
                    );
                    if reason == Some(60) {
                        last_not_ready = Some(at);
                    } else if first_free.is_none() {
                        first_free = Some((at, format!("result {reason:?} ({name})")));
                    }
                }
                SessionEvent::SpellGo {
                    caster, spell_id, ..
                } if caster == self_guid && spell_id == CHARGE => {
                    println!("t={at:5.2}s  SPELL_GO (recast SUCCEEDED)");
                    if first_free.is_none() {
                        first_free = Some((at, "SPELL_GO".into()));
                    }
                }
                SessionEvent::SpellCooldowns { cooldowns, .. } => {
                    println!("t={at:5.2}s  SMSG_SPELL_COOLDOWN {cooldowns:?}");
                }
                SessionEvent::CooldownEvent { spell_id, .. } => {
                    println!("t={at:5.2}s  SMSG_COOLDOWN_EVENT spell={spell_id}");
                }
                SessionEvent::ClearCooldown { spell_id, .. } => {
                    println!("t={at:5.2}s  SMSG_CLEAR_COOLDOWN spell={spell_id}");
                }
                _ => {}
            }
        };
        drain(&mut session, &mut kobolds, 0.2, &mut watch)?;
    }

    println!("---");
    println!("last NOT_READY:  {last_not_ready:?}");
    println!("first non-NOT_READY after GO: {first_free:?}");
    Ok(())
}
