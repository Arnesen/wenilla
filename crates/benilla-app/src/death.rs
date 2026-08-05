//! The death arc (decision 0308): the wire-fed stores, the state-machine edges into the Lua UI,
//! the intent drain back to the wire, and the root/water-walk messages the controller acks.
//!
//! The state machine is derived, never simulated (0308 §1): dead = health 0, ghost =
//! `PLAYER_FLAGS` bit 0x10 — read off the self descriptor each frame, edges fired as the
//! reference's own events (`PLAYER_DEAD` / `PLAYER_ALIVE` / `PLAYER_UNGHOST`, the classic
//! semantics: ALIVE fires on release *and* on a pre-release res; UNGHOST on ghost → alive). What
//! lives in [`DeathNet`] is only what the descriptor does not carry: the reclaim-delay expiry
//! (`SMSG_CORPSE_RECLAIM_DELAY`), the corpse location (`MSG_CORPSE_QUERY`), a pending resurrect
//! offer (`SMSG_RESURRECT_REQUEST`), and the spirit-healer confirm (`SMSG_SPIRIT_HEALER_CONFIRM`).

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;
use benilla_ui::script::{DeathAction, DeathUiState, ScriptValue, UiScript};

use crate::net::{ClientCommand, GuidIndex, NetCommands, ObjectStore, SelfGuid, SelfPlayer};
use crate::ui_script::UiInput;

/// Where our corpse is — the `MSG_CORPSE_QUERY` answer (decision 0308 §5). Raw WoW coords.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CorpsePoint {
    /// The map to show/route toward — dungeon-entrance-adjusted server-side (a corpse inside an
    /// instance points at its `ghostEntranceMap` position instead).
    pub(crate) display_map: i32,
    /// Raw WoW position of the corpse (or the dungeon entrance standing in for it).
    pub(crate) position: [f32; 3],
    /// The corpse's REAL map id, never adjusted — the marker shows only while we're on this map
    /// (or on `display_map` for the entrance stand-in).
    #[allow(dead_code)] // read by the slice-2 map markers
    pub(crate) corpse_map: u32,
}

/// A pending resurrection offer (`SMSG_RESURRECT_REQUEST`) — the RESURRECT popup's data.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResurrectOffer {
    /// The offerer. The popup answers `CMSG_RESURRECT_RESPONSE` with this guid.
    pub(crate) caster: u64,
    /// The offerer's display name — EMPTY on the wire for a player caster (the feed resolves it
    /// through the guid name cache, ask-once, like every other player name).
    pub(crate) name: String,
    /// Accepting will apply resurrection sickness (picks the RESURRECT popup variant).
    pub(crate) sickness: bool,
    /// The client should still honor the reclaim-delay gate (RESURRECT vs RESURRECT_NO_TIMER).
    pub(crate) has_timer: bool,
}

/// The wire-fed death state (decision 0308 §1's death feed). Filled by the net drain; read by the
/// death UI feed and the corpse-run systems. Cleared on session teardown with the rest of the
/// net-owned resources.
#[derive(Resource, Default)]
pub(crate) struct DeathNet {
    /// `Time::elapsed_secs_f64` when the corpse becomes reclaimable — `SMSG_CORPSE_RECLAIM_DELAY`'s
    /// delay anchored to arrival time (the packet lands at release and at login-while-dead).
    /// `GetCorpseRecoveryDelay` reads `max(0, reclaim_at − now)`; `None` before any delay landed.
    pub(crate) reclaim_at: Option<f64>,
    /// Bumped on every `SMSG_CORPSE_RECLAIM_DELAY` — the feed's range latch re-announces on it
    /// (the client's 0x269 handler re-fires IN_RANGE/IN_INSTANCE through its latch).
    pub(crate) reclaim_generation: u32,
    /// The corpse location, from the last `MSG_CORPSE_QUERY` answer. `None` = no corpse (also the
    /// server's unprompted not-found at bones-conversion — "drop the marker").
    pub(crate) corpse: Option<CorpsePoint>,
    /// A pending resurrection offer; cleared when answered or when the popup times out.
    pub(crate) resurrect: Option<ResurrectOffer>,
    /// Our streamed corpse OBJECT's guid (a TYPEID_CORPSE create whose `CORPSE_FIELD_OWNER` is
    /// us) — what `CMSG_RECLAIM_CORPSE` carries, like the real client. `None` until the corpse
    /// streams into range (vmangos ignores the guid's content, so a 0 send still reclaims —
    /// the real-client shape is kept where we have it).
    pub(crate) corpse_guid: Option<u64>,
    /// The spirit healer whose confirm (`SMSG_SPIRIT_HEALER_CONFIRM`) is awaiting the XP_LOSS
    /// two-step; its Accept sends `CMSG_SPIRIT_HEALER_ACTIVATE` with this guid.
    pub(crate) spirit_healer: Option<u64>,
    /// Water-walking is granted on our mover (the ghost's walk-on-water). Mirrored for the swim
    /// arc's future mover regime (0308 defers the actual water-surface walk); the ack itself is
    /// the controller's.
    #[allow(dead_code)]
    pub(crate) water_walk: bool,
}

/// The feed's change-tracking memory (the [`crate::ui_unit::UnitFeedState`] pattern — guid-keyed,
/// first-snapshot-counts-as-edge so logging in dead brings the popup up).
#[derive(Resource, Default)]
struct DeathFeedState {
    /// The last `(guid, dead, ghost)` we saw for the self player; `None` before the first
    /// snapshot (and after a despawn — a worldport recreates the entity, same guid).
    last: Option<(u64, bool, bool)>,
    /// `Time::elapsed_secs_f64` at the death edge — the anchor of the client-side release window
    /// (byte-VERIFIED, wow-re death-ui.md §1: the wire carries only the PLAYER_FIELD_BYTES timer
    /// BIT; the client arms `now + 360000 ms` at its own alive→dead mirror-edge — so a
    /// login-while-dead re-arms at login exactly like ours, and expiry reads 0, never negative).
    died_at: Option<f64>,
    /// The last corpse-range verdict announced to the UI (`None` = nothing announced): the
    /// CORPSE_IN_RANGE / CORPSE_OUT_OF_RANGE / CORPSE_IN_INSTANCE edge memory (decision 0308 §5).
    corpse_range: Option<CorpseRange>,
    /// The [`DeathNet::reclaim_generation`] this latch last saw — a fresh `0x269` re-fires the
    /// range events through the latch (the client's own re-announce, death-ui.md §4), so the
    /// RECOVER_CORPSE popup re-shows with the new delay.
    reclaim_generation: u32,
    /// The pending resurrect offer has been announced to the UI (`RESURRECT_REQUEST` fired) —
    /// held back while a player-caster's name is still resolving through the name cache.
    offer_announced: bool,
    /// The spirit-healer confirm has been announced (`CONFIRM_XP_LOSS` fired).
    confirm_announced: bool,
}

/// The three corpse-range verdicts the reference's events distinguish (RECOVER_CORPSE /
/// hide / RECOVER_CORPSE_INSTANCE).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CorpseRange {
    Out,
    In,
    InInstance,
}

/// The server's forced-release window (vmangos `CORPSE_REPOP_TIME` = 6 min) — the client-side
/// mirror both sides know; never on the wire.
const RELEASE_WINDOW_SECS: f64 = 360.0;

/// The corpse-range dialog radius — byte-VERIFIED **40.0 yd, inclusive** (wow-re death-ui.md §3:
/// the client compares d² ≤ `[0xb4e2ac]` = 1600.0 per frame against the corpse-QUERY cache, never
/// the streamed corpse object; edge-latched, no hysteresis). vmangos's `CORPSE_RECLAIM_RADIUS 39`
/// comment "equal client dialog show radius" is off by one against the client's own constant —
/// the server reclaim gate simply sits 1 yd inside the dialog radius. Squared yards.
const CORPSE_RANGE_SQ: f32 = 40.0 * 40.0;

/// The spirit-healer dialog range — byte-VERIFIED (wow-re death-ui.md §7: CheckSpiritHealerDist
/// compares d² ≤ `[0xc4c28c]` = 30.864 = 5.5556², the service interaction range). Squared yards.
const SPIRIT_HEALER_RANGE_SQ: f32 = 5.5556 * 5.5556;

/// Per-frame: derive the death state from the self descriptor, push the countdown/offer snapshot,
/// and fire the reference's death events on the edges (before `UiInput`, so a frame's `OnEvent`
/// sees current values — the [`crate::ui_unit`] feed convention).
#[allow(clippy::too_many_arguments)]
fn feed_death(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<(&ObjectStore, &Transform), With<SelfPlayer>>,
    self_guid: Res<SelfGuid>,
    death_net: Res<DeathNet>,
    // Real, not virtual: this reads a stamp the net apply path wrote on `Time<Real>` (see the
    // aura tuple there) — a server-sent countdown lives in real seconds, and the two ends of the
    // comparison must be the same clock.
    time: Res<Time<Real>>,
    mut feed: ResMut<DeathFeedState>,
    mut names: ResMut<crate::names::NameCache>,
    net: Res<NetCommands>,
    index: Res<GuidIndex>,
    transforms: Query<&Transform>,
    map: Option<Res<crate::world_map::CurrentMap>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(guid) = self_guid.0 else {
        return;
    };
    let Ok((store, self_t)) = self_q.single() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    let dead = store.0.unit_is_dead();
    let ghost = store.0.player_is_ghost();

    // ── The snapshot (set before the events fire, so their handlers read current values) ──────
    let release_remaining = if store.0.player_release_timer_running() {
        feed.died_at
            .map(|t| ((t + RELEASE_WINDOW_SECS - now).max(0.0)) as f32)
    } else {
        None // → GetReleaseTimeRemaining() == −1, the no-timer DEATH text
    };
    let recovery_delay = death_net
        .reclaim_at
        .map_or(0.0, |at| (at - now).max(0.0) as f32);
    let spirit_healer_in_range = death_net.spirit_healer.is_some_and(|npc| {
        index
            .0
            .get(&npc)
            .and_then(|&e| transforms.get(e).ok())
            .is_some_and(|t| {
                t.translation.distance_squared(self_t.translation) <= SPIRIT_HEALER_RANGE_SQ
            })
    });
    script.set_death(DeathUiState {
        release_remaining,
        recovery_delay,
        resurrect_sickness: death_net.resurrect.as_ref().is_some_and(|o| o.sickness),
        resurrect_has_timer: death_net.resurrect.as_ref().is_some_and(|o| o.has_timer),
        spirit_healer_in_range,
        sickness_duration: sickness_duration(store.0.unit_level().unwrap_or(0)),
    });

    // ── The state-machine edges (classic event semantics — module doc) ─────────────────────────
    let prev = feed.last.filter(|&(g, ..)| g == guid);
    feed.last = Some((guid, dead, ghost));
    match (prev, dead, ghost) {
        // Alive → dead (and the login-while-dead first snapshot): the DEATH popup's trigger.
        (Some((_, false, false)) | None, true, false) => {
            feed.died_at = Some(now);
            script.fire_event("PLAYER_DEAD", vec![]);
        }
        // Dead-unreleased → ghost (the release landed: ghost aura + graveyard teleport), or →
        // alive (a pre-release res): both are the classic PLAYER_ALIVE.
        (Some((_, true, false)), d, g) if g || !d => {
            feed.died_at = None;
            script.fire_event("PLAYER_ALIVE", vec![]);
            if g {
                // Now a ghost: ask where the corpse is (feeds the slice-2 markers + range gate).
                let _ = net.0.send(ClientCommand::CorpseQuery);
            }
        }
        // Ghost → alive (reclaim / spirit healer / accepted res).
        (Some((_, _, true)), false, false) => {
            feed.died_at = None;
            script.fire_event("PLAYER_UNGHOST", vec![]);
            // Re-ask where the corpse is — the answer is the authoritative NOT-FOUND that drops
            // the map markers. The server's own "corpse gone" push is LOOTER-gated (vmangos
            // Map.cpp:3617-3629: the unprompted u8(0) goes out only when a PvP looter exists), so
            // a PvE res never gets one; the real client re-runs its corpse query off the same
            // ghost-bit edge (wow-re death-ui.md §5's watcher), and post-res the player→corpse
            // binding is gone (SpawnCorpseBones unbinds), so the answer clears the cache.
            // Director-reported: the map tombstone survived a spirit-healer res without this.
            let _ = net.0.send(ClientCommand::CorpseQuery);
        }
        // Logging in already a ghost: no edge event (nothing was open), just the corpse ask.
        (None, _, true) => {
            let _ = net.0.send(ClientCommand::CorpseQuery);
        }
        _ => {}
    }

    // ── The offer/confirm announcements (edge-fired, name-gated) ───────────────────────────────
    match &death_net.resurrect {
        Some(offer) if !feed.offer_announced => {
            // A player caster's wire name is empty — resolve through the ask-once name cache and
            // hold the popup until it lands (the ref popup formats "%s wants to resurrect you").
            let name = if offer.name.is_empty() {
                names.resolve(offer.caster, &net).map(str::to_owned)
            } else {
                Some(offer.name.clone())
            };
            if let Some(name) = name {
                feed.offer_announced = true;
                script.fire_event("RESURRECT_REQUEST", vec![ScriptValue::Str(name)]);
            }
        }
        None => feed.offer_announced = false,
        _ => {}
    }
    match death_net.spirit_healer {
        Some(_) if !feed.confirm_announced => {
            feed.confirm_announced = true;
            script.fire_event("CONFIRM_XP_LOSS", vec![]);
        }
        None => feed.confirm_announced = false,
        _ => {}
    }

    // ── The corpse-run range gate (0308 §5): fires the reference's range events on the edges. ──
    // Only a ghost runs its corpse; the verdict compares 3-D distance to the query's DISPLAY
    // position (for a dungeon corpse that's the entrance — standing there with the corpse on
    // another map is exactly the CORPSE_IN_INSTANCE case). Distances are isometric across the
    // coord transform, so the Bevy-space compare is the yard compare.
    let range = if ghost {
        death_net.corpse.map(|cp| {
            // The client's own law (death-ui.md §3-4): within 40 yd (inclusive) of the QUERY
            // cache position, on the display map → IN_RANGE when displayMapId == corpseMapId,
            // else IN_INSTANCE (standing at a dungeon's ghost entrance, corpse inside).
            let on_display_map = map
                .as_ref()
                .is_some_and(|m| m.0 == u32::try_from(cp.display_map).unwrap_or(u32::MAX));
            let near = on_display_map
                && wow_to_bevy(cp.position).distance_squared(self_t.translation) <= CORPSE_RANGE_SQ;
            let same_map = u32::try_from(cp.display_map) == Ok(cp.corpse_map);
            match (near, same_map) {
                (false, _) => CorpseRange::Out,
                (true, true) => CorpseRange::In,
                (true, false) => CorpseRange::InInstance,
            }
        })
    } else {
        None
    };
    if feed.reclaim_generation != death_net.reclaim_generation {
        feed.reclaim_generation = death_net.reclaim_generation;
        feed.corpse_range = None; // re-announce whatever holds now (the client's 0x269 re-fire)
    }
    if range != feed.corpse_range {
        match range {
            Some(CorpseRange::In) => script.fire_event("CORPSE_IN_RANGE", vec![]),
            Some(CorpseRange::InInstance) => script.fire_event("CORPSE_IN_INSTANCE", vec![]),
            // Leaving range — and the corpse/ghost state ENDING while a range dialog could be
            // up — both land on the same hide event (the ref's OUT arm hides all three).
            Some(CorpseRange::Out) | None if feed.corpse_range.is_some() => {
                script.fire_event("CORPSE_OUT_OF_RANGE", vec![]);
            }
            _ => {}
        }
        feed.corpse_range = range;
    }
}

/// The sickness-duration string a spirit-healer res would apply at `level` — the verified server
/// table (vmangos `Player::ResurrectPlayer` + `Death.SicknessLevel` 11, 0308 §6): nil below 11,
/// `(level − 10)` minutes through 19, the aura's full 10 minutes from 20.
fn sickness_duration(level: u32) -> Option<String> {
    match level {
        0..=10 => None,
        11..=19 => {
            let m = level - 10;
            Some(if m == 1 {
                "1 minute".into()
            } else {
                format!("{m} minutes")
            })
        }
        _ => Some("10 minutes".into()),
    }
}

/// Drive the ghost-world look (decision 0308 §7, all byte-VERIFIED): the FFXDeath screen pass
/// gates on `PLAYER_FLAGS_GHOST` — instant on release, instant off at resurrect (wow-re
/// death-pass.md: the flag's CMirrorHandler watcher is the single driver of the FFX pass swap,
/// the death light profile, and the ghost ambience — one watcher, several consumers; ours splits
/// the consumers across their owning systems off the same derived flag).
fn drive_death_look(
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    mut ffx: ResMut<crate::ffx_glow::FfxDeathFade>,
) {
    let ghost = self_q.single().is_ok_and(|store| store.0.player_is_ghost());
    let target = if ghost { 1.0 } else { 0.0 };
    if ffx.0 != target {
        ffx.0 = target;
    }
}

/// Per-frame (after `UiInput`, so this frame's clicks drain this frame): map the queued Lua death
/// intents onto the wire.
fn drain_death(
    script: Option<NonSendMut<UiScript>>,
    mut death_net: ResMut<DeathNet>,
    net: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for action in script.take_death_actions() {
        let _ = match action {
            DeathAction::Repop => net.0.send(ClientCommand::RepopRequest),
            // The corpse guid: the streamed corpse object's, when it reached view range (the
            // real client's shape); else 0 — vmangos never reads the content (it resolves the
            // corpse via the player, MiscHandler.cpp:573-603), and inside 39 yd the object has
            // long streamed anyway.
            DeathAction::RetrieveCorpse => net.0.send(ClientCommand::ReclaimCorpse {
                corpse: death_net.corpse_guid.unwrap_or(0),
            }),
            DeathAction::AcceptResurrect | DeathAction::DeclineResurrect => {
                let accept = action == DeathAction::AcceptResurrect;
                match death_net.resurrect.take() {
                    Some(offer) => net.0.send(ClientCommand::ResurrectResponse {
                        caster: offer.caster,
                        accept,
                    }),
                    None => Ok(()), // stale click after the offer timed out — nothing to answer
                }
            }
            DeathAction::AcceptXpLoss => match death_net.spirit_healer.take() {
                Some(npc) => net.0.send(ClientCommand::SpiritHealerActivate { npc }),
                None => Ok(()),
            },
        };
    }
}

/// The death arc's app plugin (decision 0308): the net-fed stores, the state-machine feed + the
/// intent drain, and the controller-facing root/water-walk messages.
pub(crate) struct DeathPlugin;

impl Plugin for DeathPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DeathNet>()
            .init_resource::<DeathFeedState>()
            .add_systems(
                Update,
                (
                    feed_death.before(UiInput),
                    drain_death.after(UiInput),
                    drive_death_look,
                ),
            );
    }
}
