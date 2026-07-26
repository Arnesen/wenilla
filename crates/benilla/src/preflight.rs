//! The session preflight (decision 0649) — one always-on banner naming **what body we just logged
//! into**, plus loud warnings for the avatar states that silently invalidate a session's work.
//!
//! This is instrumentation for the *reader of the log*, not a game system. It exists because three
//! things cost a session ten minutes each and none of them announces itself:
//!
//! 1. **The character is dead or a ghost**, left that way by an earlier session. Nothing can be
//!    interacted with, attacked, looted or talked to; the world renders through the death filter;
//!    movement is rooted (unreleased) or ghost-speed (released). Every symptom reads like a client
//!    bug, and the client is fine.
//! 2. **GM mode is on** — which every probe character carries by default (method.md's guard against
//!    unattended aggro killing it). vmangos's `Player::SetGameMaster` re-templates the player to
//!    **faction template 35** and freezes the mirror timers, so nothing is hostile, nothing aggros,
//!    fall/environmental damage is skipped and breath/fatigue never tick. FactionTemplate.dbc row 35
//!    (VERIFIED from the extracted DBC): faction 31 "Friendly", own group mask 0, enemy mask 0 — it
//!    is hostile to nothing and nothing is hostile to it. Any hostility, reaction-colour, nameplate,
//!    aggro, threat, damage or drowning measurement taken with GM on is simply wrong, quietly.
//! 3. **Movement is server-blocked** — rooted, stunned, confused, fleeing, or mid-taxi-flight. The
//!    controller ignores input and the honest report is "the mover is broken".
//!
//! The banner is **not env-gated**: a warning nobody knows to switch on is not a warning. It costs
//! one line per world entry when everything is fine, and it re-fires on every re-entry (a relog, a
//! `.character race` forced logout, a reconnect) because the state can have changed.
//!
//! The other half is [`account_guard`] — the pre-connect check that a session running inside a
//! worktree pool slot is logging in as **its own** probe account. A vmangos login kicks whoever
//! holds the account, so the default `one` credentials fire the director out of their live session
//! and another slot's `probeN` fires a parallel session's probe out of the world mid-sample
//! (method.md "The local vmangos server"). It gates the **env fast path only** — a human typing at
//! the login screen is never blocked.

use bevy::prelude::*;

use crate::area::AreaTableRes;
use crate::names::NameCache;
use crate::net::{EnteredWorldMessage, ObjectStore, SelfGuid, SelfPlayer};
use crate::terrain_stream::CurrentArea;
use crate::world_map::CurrentMap;

/// `PLAYER_FLAGS_GM` (vmangos `Player.h`) — set by `SetGameMaster(true)` alongside the faction-35
/// re-template. PUBLIC, so it rides our own descriptor like any other player flag.
const PLAYER_FLAGS_GM: u32 = 0x0000_0008;

/// The `UNIT_FIELD_FLAGS` bits that mean "the server is driving, or refusing to let you drive"
/// (vmangos `UnitDefines.h`) — each is a distinct reason the mover looks broken.
const MOVE_BLOCKERS: &[(u32, &str)] = &[
    (0x0002_0000, "PACIFIED (no melee, no pacify-blocked casts)"),
    (0x0004_0000, "STUNNED"),
    (
        0x0010_0000,
        "on a TAXI FLIGHT (input ignored for the whole ride)",
    ),
    (0x0020_0000, "DISARMED (weapon abilities refuse)"),
    (0x0040_0000, "CONFUSED (the server drives the movement)"),
    (0x0080_0000, "FLEEING (the server drives the movement)"),
    (0x0100_0000, "POSSESSED (another unit holds the reins)"),
];

/// `UNIT_FLAG_IN_COMBAT` — worth naming on entry: an unattended probe that logs in already fighting
/// is the exact shape of the accident the unattended-combat ban exists for (method.md).
const UNIT_FLAG_IN_COMBAT: u32 = 0x0008_0000;

/// `UNIT_FLAG_SILENCED` — casts silently refuse.
const UNIT_FLAG_SILENCED: u32 = 0x0000_2000;

/// The GM faction template vmangos swaps in with GM mode (`SetFactionTemplateId(35)`).
const GM_FACTION_TEMPLATE: u32 = 35;

/// How long after world entry we keep waiting for the zone name before reporting without it. The
/// descriptor lands in a few hundred ms but the area id only resolves once the tile under us has
/// streamed (~2 s), and "Eastern Plaguelands" is the line's whole orienting value — a raw map id
/// and a coordinate triple do not tell a reader at a glance that this character is nowhere useful.
const ZONE_WAIT_SECS: f32 = 4.0;

/// The hard backstop: report whatever we have this long after world entry. Only reached when
/// something is badly wrong with the stream, and reporting late beats never reporting.
const DESCRIPTOR_WAIT_SECS: f32 = 8.0;

pub(crate) struct PreflightPlugin;

impl Plugin for PreflightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Preflight>().add_systems(
            Update,
            report_session.after(crate::schedule::WorldStage::Net),
        );
    }
}

/// The banner's once-per-entry latch: armed by [`EnteredWorldMessage`], disarmed when the report
/// goes out (or when the wait expires).
#[derive(Resource, Default)]
struct Preflight {
    /// `Time::elapsed_secs` at the world entry we still owe a report for; `None` = nothing pending.
    armed_at: Option<f32>,
}

/// Wait for the self descriptor after each world entry, then print the banner once.
#[allow(clippy::too_many_arguments)]
fn report_session(
    mut state: ResMut<Preflight>,
    mut entered: MessageReader<EnteredWorldMessage>,
    time: Res<Time>,
    self_q: Query<(&ObjectStore, &Transform), With<SelfPlayer>>,
    self_guid: Res<SelfGuid>,
    names: Res<NameCache>,
    map: Option<Res<CurrentMap>>,
    area: Option<Res<CurrentArea>>,
    area_table: Option<Res<AreaTableRes>>,
) {
    if entered.read().next().is_some() {
        state.armed_at = Some(time.elapsed_secs());
    }
    let Some(armed_at) = state.armed_at else {
        return;
    };
    let expired = time.elapsed_secs() - armed_at >= DESCRIPTOR_WAIT_SECS;
    let Ok((store, transform)) = self_q.single() else {
        if expired {
            state.armed_at = None;
            warn!("preflight: no self descriptor {DESCRIPTOR_WAIT_SECS:.0}s after entering the world — the avatar never streamed in");
        }
        return;
    };
    // MAXHEALTH is always in the login snapshot; an empty store means the create block hasn't been
    // applied yet, and reading it would report a level-0 dead corpse of no race.
    if store.0.unit_max_health().unwrap_or(0) == 0 && !expired {
        return;
    }
    // `CurrentArea` is the FINEST area under us ("Darrowshire"), which on its own can be unplaceable;
    // the parent zone ("Eastern Plaguelands") is what orients a reader, so print zone / subzone.
    let zone = area
        .and_then(|a| a.0)
        .zip(area_table.as_deref())
        .map(|(id, cat)| {
            let sub = cat.0.name(id);
            let top = cat.0.top_zone(id).and_then(|z| cat.0.name(z));
            match (top, sub) {
                (Some(top), Some(sub)) if top != sub => format!(" \"{top} / {sub}\""),
                (Some(n), _) | (_, Some(n)) => format!(" \"{n}\""),
                _ => String::new(),
            }
        })
        .filter(|z| !z.is_empty());
    if zone.is_none() && time.elapsed_secs() - armed_at < ZONE_WAIT_SECS {
        return; // the tile under us hasn't streamed yet — give the zone name its grace
    }
    state.armed_at = None;

    let name = self_guid
        .0
        .and_then(|g| names.peek(g))
        .unwrap_or("<unnamed>");
    let race = store
        .0
        .unit_race()
        .and_then(|r| crate::ui_unit::race_names(r).map(|(d, _)| d))
        .unwrap_or("?");
    let class = store
        .0
        .unit_class()
        .and_then(|c| crate::ui_unit::class_names(c).map(|(d, _)| d))
        .unwrap_or("?");
    let zone = zone.unwrap_or_default();
    // Printed as the raw WoW triple, which is what `.go xyz <x> <y> <z> <map>` takes — the whole
    // point of putting the position in the banner is that it can be pasted back.
    let [wx, wy, wz] = benilla_assets::coords::bevy_to_wow(transform.translation);
    info!(
        "preflight: {name} — level {lvl} {race} {class}, {hp}/{maxhp} hp, map {map}{zone} @ [{wx:.1}, {wy:.1}, {wz:.1}], faction template {faction}",
        lvl = store.0.unit_level().unwrap_or(0),
        hp = store.0.unit_health().unwrap_or(0),
        maxhp = store.0.unit_max_health().unwrap_or(0),
        map = map.map_or(-1, |m| m.0 as i64),
        faction = store.0.unit_faction_template().unwrap_or(0),
    );

    for line in findings(&store.0) {
        warn!("preflight: {line}");
    }
}

/// Everything about this avatar that will quietly invalidate a session's work, worst first. Pure
/// over the descriptor so the whole ladder is unit-testable.
fn findings(fields: &benilla_protocol::messages::ObjectFields) -> Vec<String> {
    let mut out = Vec::new();
    let unit_flags = fields.unit_flags();

    // Dead and ghost are mutually exclusive on the wire (a released ghost's health is 1, decision
    // 0308 §1) — report whichever holds, never both.
    if fields.unit_is_dead() {
        out.push(
            "THE CHARACTER IS DEAD (health 0, corpse not released) — the mover is server-rooted \
             and nothing can be cast, attacked, looted or interacted with. Fix it before anything \
             else: WOW_PROBE_CHAT=\".revive\" (probe accounts are gmlevel 6)."
                .into(),
        );
    } else if fields.player_is_ghost() {
        out.push(
            "THE CHARACTER IS A GHOST (spirit released, corpse elsewhere) — the world renders \
             through the death filter, NPCs and objects refuse every interaction, and movement \
             runs at ghost speed on water. Fix it before anything else: WOW_PROBE_CHAT=\".revive\"."
                .into(),
        );
    }

    if fields.player_flags() & PLAYER_FLAGS_GM != 0 {
        out.push(format!(
            "GM MODE IS ON — vmangos re-templates the player to faction {GM_FACTION_TEMPLATE} \
             (\"Friendly\", enemy mask 0) and freezes the mirror timers, so NOTHING is hostile, \
             nothing aggros, fall/environmental damage is skipped and breath/fatigue never tick. \
             Any hostility, reaction-colour, nameplate, threat, aggro, damage or drowning reading \
             taken now is wrong. Send WOW_PROBE_CHAT=\".gm off\" first — and put it back on when \
             you are done, so an unwatched probe can't be killed."
        ));
    }

    let blocked: Vec<&str> = MOVE_BLOCKERS
        .iter()
        .filter(|(bit, _)| unit_flags & bit != 0)
        .map(|(_, label)| *label)
        .collect();
    if !blocked.is_empty() {
        out.push(format!(
            "MOVEMENT IS SERVER-BLOCKED — {} — the controller will look broken because the server \
             is refusing (or driving) the movement, not because the mover is.",
            blocked.join(", ")
        ));
    }

    if unit_flags & UNIT_FLAG_IN_COMBAT != 0 {
        out.push(
            "IN COMBAT on arrival — something is already fighting this character. Do not leave it \
             unattended (method.md's unattended-combat ban): break the fight or move it out."
                .into(),
        );
    }
    if unit_flags & UNIT_FLAG_SILENCED != 0 {
        out.push("SILENCED — spell casts will refuse for as long as the aura holds.".into());
    }
    out
}

/// The pre-connect account guard, consulted by the login policy's env fast path
/// ([`crate::login`]). Returns `Err(explanation)` when this build lives in a worktree pool slot and
/// the fast path is about to authenticate as an account that belongs to somebody else — the
/// director's `one` (a login KICKS their live session mid-play) or another slot's `probeN` (the
/// kicked client's 0065 teardown despawns every net entity, so a parallel session's probe reads a
/// unit-less world and prints garbage; it happened, method.md records it).
///
/// Slot identity comes from the compiled-in manifest path, because that is what the pool guarantees
/// is unique per session: every slot has its own checkout and its own `target/`. Outside a pool slot
/// (the primary checkout, which is the director's) the guard is inert — it has no business having an
/// opinion about a login it cannot attribute.
///
/// `WOW_ALLOW_ACCOUNT=1` is the escape hatch for the rare deliberate cross-account run; it turns the
/// refusal into a warning rather than silence, because the kick still happens.
pub(crate) fn account_guard(user: &str) -> Result<(), String> {
    guard_for(pool_slot(env!("CARGO_MANIFEST_DIR")), user)
}

/// [`account_guard`]'s decision, with the slot passed in so the ladder is testable from any
/// checkout (the real one reads a compile-time path that differs per worktree).
fn guard_for(slot: Option<u32>, user: &str) -> Result<(), String> {
    let Some(slot) = slot else {
        return Ok(());
    };
    let mine = format!("probe{slot}");
    let user_lc = user.to_ascii_lowercase();
    if user_lc == mine {
        return Ok(());
    }
    let whose = if user_lc == "one" {
        "the DIRECTOR's account — logging in on it kicks them out of their live session mid-play"
    } else if user_lc.starts_with("probe") && user_lc[5..].chars().all(|c| c.is_ascii_digit()) {
        "ANOTHER worktree slot's probe account — logging in on it kicks that session's probe out \
         of the world, and its next sample reads a unit-less world"
    } else {
        return Ok(()); // a bystander account (`two`, a fresh test account): not ours to police
    };
    Err(format!(
        "the env fast path is about to log in as `{user}` from pool-{slot}, and that is {whose}. \
         This slot's identity is WOW_USER=probe{slot} WOW_PASS=pprobe{slot} \
         WOW_CHAR=Probe{spelled} (method.md \"The local vmangos server\").",
        spelled = spell_digit(slot)
    ))
}

/// This slot's index as the word the probe identity spells it with (`pool-4` → `"four"`), or `None`
/// outside a pool slot. The one place anything else should ask "which session am I?" — the rig keys
/// its derived character names off it ([`crate::capture::rig_char_name_from_env`]).
pub(crate) fn slot_word() -> Option<&'static str> {
    pool_slot(env!("CARGO_MANIFEST_DIR")).map(spell_digit)
}

/// The pool slot index this build was compiled in, from a `…/benilla-wt/pool-<N>/…` manifest path.
/// `None` for the primary checkout and any non-pool worktree.
fn pool_slot(manifest_dir: &str) -> Option<u32> {
    let mut parts = manifest_dir.split('/');
    while let Some(part) = parts.next() {
        if part == "benilla-wt" {
            return parts.next()?.strip_prefix("pool-")?.parse().ok();
        }
    }
    None
}

/// The probe character's name suffix for slot `n` — vmangos names carry no digits, so the pool
/// index is spelled (`pool-4` → `Probefour`).
fn spell_digit(n: u32) -> &'static str {
    match n {
        0 => "zero",
        1 => "one",
        2 => "two",
        3 => "three",
        4 => "four",
        5 => "five",
        6 => "six",
        7 => "seven",
        8 => "eight",
        9 => "nine",
        _ => "<n>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::ObjectFields;

    /// Build a player descriptor from `(field index, value)` pairs — the wire indices the
    /// accessors read.
    fn player(fields: &[(u16, u32)]) -> ObjectFields {
        ObjectFields::from_pairs(fields)
    }

    const HEALTH: u16 = 22;
    const MAXHEALTH: u16 = 28;
    const UNIT_FLAGS: u16 = 46;
    const PLAYER_FLAGS: u16 = 190;

    #[test]
    fn a_healthy_avatar_reports_nothing() {
        let f = player(&[(HEALTH, 60), (MAXHEALTH, 60)]);
        assert!(findings(&f).is_empty());
    }

    #[test]
    fn dead_and_ghost_never_report_together() {
        // Dead: health 0 with a real MAXHEALTH.
        let dead = findings(&player(&[(MAXHEALTH, 60)]));
        assert_eq!(dead.len(), 1);
        assert!(dead[0].contains("IS DEAD"));
        // Ghost: health 1 and PLAYER_FLAGS_GHOST — the wire never shows both (decision 0308 §1).
        let ghost = findings(&player(&[
            (HEALTH, 1),
            (MAXHEALTH, 60),
            (PLAYER_FLAGS, 0x10),
        ]));
        assert_eq!(ghost.len(), 1);
        assert!(ghost[0].contains("IS A GHOST"));
    }

    #[test]
    fn gm_mode_is_reported_on_a_perfectly_healthy_avatar() {
        let f = player(&[
            (HEALTH, 60),
            (MAXHEALTH, 60),
            (PLAYER_FLAGS, PLAYER_FLAGS_GM),
        ]);
        let out = findings(&f);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("GM MODE IS ON"));
    }

    #[test]
    fn every_move_blocker_is_named_in_one_line() {
        let f = player(&[
            (HEALTH, 60),
            (MAXHEALTH, 60),
            (UNIT_FLAGS, 0x0004_0000 | 0x0010_0000), // stunned + taxi
        ]);
        let out = findings(&f);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("STUNNED") && out[0].contains("TAXI FLIGHT"));
    }

    #[test]
    fn the_guard_only_polices_accounts_that_belong_to_someone() {
        // Our own slot's probe: the whole point of the identity.
        assert!(guard_for(Some(4), "probe4").is_ok());
        assert!(guard_for(Some(4), "PROBE4").is_ok()); // vmangos accounts are case-insensitive
                                                       // The director's account, and a neighbouring slot's probe: both kick a live session.
        let director = guard_for(Some(4), "one").unwrap_err();
        assert!(director.contains("DIRECTOR") && director.contains("WOW_USER=probe4"));
        // The override hint belongs to the caller that can act on it, not to the reason.
        assert!(!director.contains("WOW_ALLOW_ACCOUNT"));
        assert!(guard_for(Some(4), "probe7")
            .unwrap_err()
            .contains("ANOTHER worktree slot"));
        // A bystander account is nobody's to police, and outside a pool slot we have no standing.
        assert!(guard_for(Some(4), "two").is_ok());
        assert!(guard_for(None, "one").is_ok());
    }

    #[test]
    fn the_probe_character_name_spells_the_slot() {
        // vmangos player names carry no digits, so `Probe4` cannot exist — the pool index is spelled.
        assert!(guard_for(Some(0), "one")
            .unwrap_err()
            .contains("WOW_CHAR=Probezero"));
        assert!(guard_for(Some(9), "one")
            .unwrap_err()
            .contains("WOW_CHAR=Probenine"));
    }

    #[test]
    fn the_slot_is_read_off_the_manifest_path() {
        assert_eq!(
            pool_slot("/Users/sam/dev/benilla-wt/pool-7/crates/benilla"),
            Some(7)
        );
        assert_eq!(pool_slot("/Users/sam/dev/benilla-wow/crates/benilla"), None);
        assert_eq!(
            pool_slot("/Users/sam/dev/benilla-wow/.claude/worktrees/x/crates/benilla"),
            None
        );
    }
}
