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
use crate::ui_action::Spells;
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
    /// Bumped on every `SMSG_SPIRIT_HEALER_CONFIRM` — the confirm announces per MESSAGE, not per
    /// Some-edge (decision 1068): a Cancel sends nothing and clears nothing, so only a fresh
    /// message can re-show the dialog, and the healer re-sends one on every gossip ask.
    pub(crate) confirm_generation: u32,
    /// Water-walking is granted on our mover (the ghost's walk-on-water). Mirrored for the swim
    /// arc's future mover regime (0308 defers the actual water-surface walk); the ack itself is
    /// the controller's.
    #[allow(dead_code)]
    pub(crate) water_walk: bool,
}

/// The feed's memory, in **two scopes** (decisions 1290/1291): the body's own state machine is
/// world memory and survives a `/reload`; what this VM has been *told* dies with the VM.
///
/// The reference draws the same line by construction — its death mirror is engine-side (wow-re
/// death-ui.md §1), so a `ReloadUI` neither re-arms the release window nor loses it, while the
/// rebuilt UI repaints from the surviving state. Ours mirrors that: `mirror`/`died_at` stay put
/// across a VM replacement, and the announce latches reset with the VM so the fresh frame tree
/// hears the events the old one consumed.
#[derive(Resource, Default)]
struct DeathFeedState {
    /// **World-scoped.** The last `(guid, dead, ghost)` the self body showed — the edge that
    /// arms and clears the release window. `None` before a world session's first snapshot.
    mirror: Option<(u64, bool, bool)>,
    /// **World-scoped.** `Time::elapsed_secs_f64` at the death edge — the anchor of the
    /// client-side release window (byte-VERIFIED, wow-re death-ui.md §1: the wire carries only
    /// the PLAYER_FIELD_BYTES timer BIT; the client arms `now + 360000 ms` at its own
    /// alive→dead mirror-edge — so a login-while-dead re-arms at login exactly like ours, and
    /// expiry reads 0, never negative). Deliberately NOT per-VM: a `/reload` must not restart
    /// the six minutes — the reference's engine-side window keeps running through one.
    died_at: Option<f64>,
    /// **VM-scoped** — the announce latches, keyed on the VM they announced to.
    vm: crate::ui_script::VmMemo<DeathAnnounced>,
}

/// What the live VM has been told about the death state — [`DeathFeedState::vm`]'s payload
/// (guid-keyed, first-snapshot-counts-as-edge so logging in dead brings the popup up — and so
/// does reloading dead, which is the same situation from the frame tree's point of view).
#[derive(Default)]
struct DeathAnnounced {
    /// The last `(guid, dead, ghost)` THIS VM was given an event for; `None` for a fresh VM.
    last: Option<(u64, bool, bool)>,
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
    /// The [`DeathNet::confirm_generation`] this latch last announced — a fresh
    /// `SMSG_SPIRIT_HEALER_CONFIRM` re-fires `CONFIRM_XP_LOSS` through the latch (decision 1068;
    /// the reclaim-latch pattern above). The old Some-edge latch here was B80's deadlock: it
    /// reset only when `spirit_healer` went `None`, which nothing but Accept ever does, so one
    /// Cancel swallowed every later confirm.
    confirm_generation: u32,
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
    map: Option<Res<benilla_world::world_map::CurrentMap>>,
    status: Res<crate::net::NetStatus>,
    spells: Option<Res<Spells>>,
    mut items: ResMut<crate::items::Items>,
) {
    // **Only a LIVE session's descriptor is a snapshot** (decision 1732). A reconnect-able
    // disconnect keeps the self avatar as the local puppet (0065) — descriptor and all — so
    // without this gate the frames between the socket dying and the reconnect landing would
    // re-arm `mirror` from a frozen relic, and the reconnect would then find no edge to fire.
    // That is the same hole from the other side as [`end_session_death_feed`]: the body's state
    // machine may only be driven by the wire, and while there is no wire there is nothing to
    // mirror. (The `/logout` path needs no gate — the avatar is despawned, so the `self_q` below
    // already ends the feed.)
    if !status.connected {
        return;
    }
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
        // `HasSoulstone()` — see [`resolve_self_res`] for the three gates and their order. Not a
        // per-frame inventory walk in general: the dead gate is first, so while alive this is one
        // health read, and while dead-unreleased the walk only runs on a zero field.
        self_res_label: resolve_self_res(&store.0, &mut items, spells.as_deref(), &net)
            .map(|r| r.label().to_owned()),
    });

    // ── The WORLD edges (the body's own state machine): the release-window anchor and the
    // corpse asks. These run off `mirror`, which survives a VM replacement — a `/reload` while
    // dead must neither restart the six minutes nor re-query the corpse (the cache has it).
    let prev = feed.mirror.filter(|&(g, ..)| g == guid);
    feed.mirror = Some((guid, dead, ghost));
    match (prev, dead, ghost) {
        // Alive → dead (and the login-while-dead first snapshot): arm the release window.
        (Some((_, false, false)) | None, true, false) => {
            feed.died_at = Some(now);
        }
        // Dead-unreleased → ghost (the release landed), or → alive (a pre-release res).
        (Some((_, true, false)), d, g) if g || !d => {
            feed.died_at = None;
            if g {
                // Now a ghost: ask where the corpse is (feeds the slice-2 markers + range gate).
                let _ = net.0.send(ClientCommand::CorpseQuery);
            }
        }
        // Ghost → alive (reclaim / spirit healer / accepted res).
        (Some((_, _, true)), false, false) => {
            feed.died_at = None;
            // Re-ask where the corpse is — the answer is the authoritative NOT-FOUND that drops
            // the map markers. The server's own "corpse gone" push is LOOTER-gated (vmangos
            // Map.cpp:3617-3629: the unprompted u8(0) goes out only when a PvP looter exists), so
            // a PvE res never gets one; the real client re-runs its corpse query off the same
            // ghost-bit edge (wow-re death-ui.md §5's watcher), and post-res the player→corpse
            // binding is gone (SpawnCorpseBones unbinds), so the answer clears the cache.
            // Director-reported: the map tombstone survived a spirit-healer res without this.
            let _ = net.0.send(ClientCommand::CorpseQuery);
        }
        // Logging in already a ghost: no edge, just the corpse ask.
        (None, _, true) => {
            let _ = net.0.send(ClientCommand::CorpseQuery);
        }
        _ => {}
    }

    // ── The VM edges: the classic events, fired off what THIS VM has heard (module doc). A
    // fresh VM's memo is empty, so a reload-while-dead re-fires PLAYER_DEAD to the rebuilt
    // frame tree — the same first-snapshot-counts-as-edge that brings the popup up at a
    // login-while-dead, which is our design's own posture for both (decision 1291).
    let memo = feed.vm.get(&script);
    let prev = memo.last.filter(|&(g, ..)| g == guid);
    memo.last = Some((guid, dead, ghost));
    match (prev, dead, ghost) {
        (Some((_, false, false)) | None, true, false) => {
            script.fire_event("PLAYER_DEAD", vec![]);
        }
        (Some((_, true, false)), d, g) if g || !d => {
            script.fire_event("PLAYER_ALIVE", vec![]);
        }
        (Some((_, _, true)), false, false) => {
            script.fire_event("PLAYER_UNGHOST", vec![]);
        }
        _ => {}
    }

    // ── The offer/confirm announcements (edge-fired, name-gated) ───────────────────────────────
    match &death_net.resurrect {
        Some(offer) if !memo.offer_announced => {
            // A player caster's wire name is empty — resolve through the ask-once name cache and
            // hold the popup until it lands (the ref popup formats "%s wants to resurrect you").
            let name = if offer.name.is_empty() {
                names.resolve(offer.caster, &net).map(str::to_owned)
            } else {
                Some(offer.name.clone())
            };
            if let Some(name) = name {
                memo.offer_announced = true;
                script.fire_event("RESURRECT_REQUEST", vec![ScriptValue::Str(name)]);
            }
        }
        None => memo.offer_announced = false,
        _ => {}
    }
    // The confirm is message-fired, not state-edged (decision 1068): each SMSG bumps the
    // generation, so asking the healer again after a Cancel brings the dialog back.
    if death_net.spirit_healer.is_some() && memo.confirm_generation != death_net.confirm_generation
    {
        memo.confirm_generation = death_net.confirm_generation;
        script.fire_event("CONFIRM_XP_LOSS", vec![]);
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
    if memo.reclaim_generation != death_net.reclaim_generation {
        memo.reclaim_generation = death_net.reclaim_generation;
        memo.corpse_range = None; // re-announce whatever holds now (the client's 0x269 re-fire)
    }
    if range != memo.corpse_range {
        match range {
            Some(CorpseRange::In) => script.fire_event("CORPSE_IN_RANGE", vec![]),
            Some(CorpseRange::InInstance) => script.fire_event("CORPSE_IN_INSTANCE", vec![]),
            // Leaving range — and the corpse/ghost state ENDING while a range dialog could be
            // up — both land on the same hide event (the ref's OUT arm hides all three).
            Some(CorpseRange::Out) | None if memo.corpse_range.is_some() => {
                script.fire_event("CORPSE_OUT_OF_RANGE", vec![]);
            }
            _ => {}
        }
        memo.corpse_range = range;
    }
}

/// `SPELL_EFFECT_SELF_RESURRECT` — `Spell.dbc`'s effect-slot value the client's item leg scans for
/// (`0x5ed650`, `cmp … 0x5e`). Exactly eleven 1.12 spells carry it.
const SPELL_EFFECT_SELF_RESURRECT: u32 = 94;

/// What a `UseSoulstone()` would spend right now — the client's own two-legged fork
/// (`Script::UseSoulstone 0x48ad70`, wow-re death-ui.md §10.4), resolved app-side because neither
/// leg's inputs (the spell catalog, the item cache) are bound in the script VM.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SelfRes {
    /// `PLAYER_SELF_RES_SPELL` is non-zero — send `CMSG_SELF_RES` and let the server cast it.
    Spell {
        #[allow(dead_code)] // the wire needs no id: the server reads its own field
        spell: u32,
        label: String,
    },
    /// The field is zero, but a carried item's ON-USE spell self-resurrects — the client **uses
    /// the item** instead, and labels the button with the ITEM's name.
    Item {
        bag_index: u8,
        slot: u8,
        guid: u64,
        entry: u32,
        label: String,
    },
}

impl SelfRes {
    /// The button text — what `HasSoulstone()` returns.
    fn label(&self) -> &str {
        match self {
            Self::Spell { label, .. } | Self::Item { label, .. } => label,
        }
    }
}

/// **`HasSoulstone()`'s whole answer**, and `UseSoulstone()`'s routing — one resolver, because the
/// binary is one predicate the two bindings share (wow-re death-ui.md §10.1/§10.4).
///
/// Three gates in the client's own order, and the order is the performance story as much as the
/// fidelity one:
///
/// 1. **Dead.** `HasSoulstone` reads `UNIT_FIELD_HEALTH` (signed) and answers nil while it is
///    positive — so it speaks only inside the pre-release dead window, which is exactly where the
///    DEATH dialog lives. A released ghost has health **1** (vmangos `BuildPlayerRepop`), so it
///    stops answering the moment you release, and the walk below never runs while alive.
/// 2. **The field.** Non-zero → the spell leg, named through `Spell.dbc`. An id the catalog cannot
///    resolve is **`"UNKNOWN"`**, not nil — the client pushes that literal rather than hiding the
///    button, and hiding it would silently strip a self-res the server is holding for us.
/// 3. **The carried item.** Only on a zero field: the reference's own inventory walker at its
///    default section mask (gear + bags + backpack + keyring, no bank, no buyback — which is
///    [`InventoryScope::DEFAULT`], the same `0x47` the walker rewrites to) for an item with an
///    ON-USE (`trigger == 0`) spell carrying [`SPELL_EFFECT_SELF_RESURRECT`] in any effect slot.
///    The label is the **item's** name.
///
/// A template still in flight can't be judged and reads as "not a self-res"; the ask-once query
/// the lookup fires answers within a frame or two and the next resolve sees it.
fn resolve_self_res(
    store: &benilla_protocol::ObjectFields,
    items: &mut crate::items::Items,
    spells: Option<&Spells>,
    commands: &NetCommands,
) -> Option<SelfRes> {
    if !store.unit_is_dead() {
        return None;
    }
    if let Some(spell) = store.player_self_res_spell() {
        let label = spells
            .and_then(|s| s.catalog.get(spell))
            .map_or_else(|| "UNKNOWN".to_owned(), |d| d.name.clone());
        return Some(SelfRes::Spell { spell, label });
    }
    // Collected first, then judged: the template lookup needs `items` mutably (it is ask-once,
    // so a miss fires the query) while the walk holds it immutably — `has_key`'s own idiom.
    let slots =
        crate::ui_items::collect_inventory(store, items, crate::ui_items::InventoryScope::DEFAULT);
    slots.into_iter().find_map(|(bag_index, slot, guid)| {
        let entry = items.object(guid)?.object_entry()?;
        let t = items.template(entry, guid, commands)?;
        let self_res = t.spells.iter().any(|sp| {
            sp.trigger == 0
                && spells.is_some_and(|c| {
                    c.catalog
                        .get(sp.spell_id)
                        .is_some_and(|d| d.effects.contains(&SPELL_EFFECT_SELF_RESURRECT))
                })
        });
        self_res.then(|| SelfRes::Item {
            bag_index,
            slot,
            guid,
            entry,
            label: t.name.clone(),
        })
    })
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
    mut ffx: ResMut<benilla_world::ffx_glow::FfxDeathFade>,
) {
    let ghost = ghost_probe()
        .unwrap_or_else(|| self_q.single().is_ok_and(|store| store.0.player_is_ghost()));
    let target = if ghost { 1.0 } else { 0.0 };
    if ffx.0 != target {
        ffx.0 = target;
    }
}

/// `WOW_GHOST_PROBE=1|0` — pin the **ghost world** on or off without dying, so the ghost-world look
/// (and everything that must NOT inherit it — the portrait bakes, decision 1481) can be A/B'd from a
/// capture instead of from a corpse run.
///
/// It drives all three halves of that look together, because they are one state: this screen pass,
/// the death light (`LightParams` slot 4) and the DeathClouds sky, the latter two through
/// [`Viewer::ghost`](benilla_world::view::Viewer::ghost), whose only readers they are. It pinned
/// the screen pass alone until the sky landed, which made an A/B of "the ghost world" quietly a
/// third of one. The death arc's state machine, its events and its UI stay untouched and still
/// follow the wire.
pub(crate) fn ghost_probe() -> Option<bool> {
    static PROBE: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| match std::env::var("WOW_GHOST_PROBE").ok()?.trim() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    })
}

/// Per-frame (after `UiInput`, so this frame's clicks drain this frame): map the queued Lua death
/// intents onto the wire.
fn drain_death(
    script: Option<NonSendMut<UiScript>>,
    mut death_net: ResMut<DeathNet>,
    net: Res<NetCommands>,
    targeting: crate::ui_action::cast_target::CastTargeting,
    mut ladder: crate::ui_action::CastLadder,
    // The soulstone leg is a real arm-290 caller in the reference's own census (wow-re
    // `bind-confirm-law.md`: 290 ← UseSoulstone · UseInventoryItem · UseAction · UseContainerItem),
    // so it goes through the shared send carrying the same bind gate every other item use does.
    mut gate: crate::ui_bind_confirm::BindGate,
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
            // The self-resurrect button — the client's own two-legged fork, **re-resolved
            // here at click time** rather than remembered from the frame the dialog opened. That
            // is the reference's posture too: its `OnCancel` calls `HasSoulstone()` again before
            // branching, because the field belongs to the server and can have been spent since.
            //
            // Nothing is taken or cleared on our side on either leg: the server owns
            // `PLAYER_SELF_RES_SPELL` and zeroes it as it casts, so the button's disappearance is
            // a descriptor delta like every other death-arc confirmation.
            DeathAction::UseSoulstone => {
                let store = targeting.self_store.single().ok().map(|s| s.0.clone());
                match store.as_ref().and_then(|store| {
                    resolve_self_res(
                        store,
                        &mut ladder.items,
                        ladder.spells.as_deref(),
                        &ladder.commands,
                    )
                }) {
                    Some(SelfRes::Spell { .. }) => net.0.send(ClientCommand::SelfRes),
                    // The item leg is an ordinary item use — the same `CGItem_C::Use 0x5d8d00`
                    // every bag click and action button ends at, which is why it goes through the
                    // one send rather than growing a second path (decision 0914).
                    Some(SelfRes::Item {
                        bag_index,
                        slot,
                        guid,
                        entry,
                        ..
                    }) => {
                        let t = ladder
                            .items
                            .template(entry, guid, &ladder.commands)
                            .cloned();
                        let it = crate::ui_items::ItemUse {
                            guid: Some(guid),
                            start_quest: t.as_ref().map_or(0, |t| t.start_quest),
                            bag_index,
                            slot,
                            entry,
                            spell_index: t.as_ref().and_then(|t| t.use_spell_index()).unwrap_or(0),
                            use_spell: t.as_ref().and_then(|t| t.use_spell).map(|u| u.spell_id),
                            on_object: None,
                            is_charter: false,
                        };
                        crate::ui_items::send_item_use(
                            it,
                            &targeting.context(),
                            &mut ladder,
                            &mut script,
                            &mut gate,
                            false,
                        );
                        Ok(())
                    }
                    // Nothing to spend — the reference's silent return (it pushes nothing and
                    // sends nothing on this path either).
                    None => Ok(()),
                }
            }
        };
    }
}

/// **The world scope of [`DeathFeedState`] dies with the world session** (decision 1732) — the
/// teardown [`DeathNet`]'s own has had since 0065, which this half never got.
///
/// `mirror` is the body's state machine and `died_at` its release-window anchor; both are
/// deliberately not per-VM, so a `/reload` cannot restart the six minutes. But "not per-VM" was
/// implemented as "never cleared", and the two scopes then disagreed across a relog: the session
/// teardown resets `DeathNet` (corpse marker included) while `mirror` still read
/// `(same guid, dead, ghost)` from before the socket died. Re-entering the world as a ghost
/// therefore matched **no** arm of the world-edge machine — `(None, _, true)`, the "logging in
/// already a ghost" arm, needs `prev == None` — so `MSG_CORPSE_QUERY` was never re-sent, the
/// corpse cache stayed empty, the range gate could not fire `CORPSE_IN_RANGE`, and the
/// **RECOVER_CORPSE ("Resurrect Now") popup never came up again after a relog**
/// (director-reported, 2026-08-30).
///
/// The guid filter on `mirror` looked like it covered this and does not: it distinguishes a
/// *different* character, not the *same* one re-entering the world. `DisconnectedMessage` is the
/// one total edge — a socket death, a kick, and a `/logout` all reach it (the IO thread emits a
/// `Disconnected` behind `SessionEnd::LoggedOut`) — and it is the very edge `DeathNet` is reset
/// on, which is what keeps the two scopes in lockstep by construction rather than by memory.
fn end_session_death_feed(
    mut msgs: MessageReader<crate::net::DisconnectedMessage>,
    mut feed: ResMut<DeathFeedState>,
) {
    if msgs.read().next().is_some() {
        *feed = DeathFeedState::default();
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
                    // Before the feed, so the frame a session ends is already a frame the feed
                    // sees no memory of the last one.
                    end_session_death_feed.before(feed_death),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// **The world scope dies with the session, and only with the session** (decision 1732).
    ///
    /// Both halves matter and they pull opposite ways: a `/reload` must NOT restart the six
    /// minutes (which is why `mirror`/`died_at` are not per-VM in the first place), and a relog
    /// MUST clear them (or the "logging in already a ghost" arm never fires, no
    /// `MSG_CORPSE_QUERY` goes out, and the RECOVER_CORPSE popup stays down — the reported bug).
    /// A test that only asserted the clear would pass on a resource wiped every frame.
    #[test]
    fn the_session_edge_clears_the_world_scoped_death_memory_and_nothing_else_does() {
        let mut app = App::new();
        app.add_message::<crate::net::DisconnectedMessage>()
            .insert_resource(DeathFeedState {
                mirror: Some((0x1234, true, true)),
                died_at: Some(12.0),
                vm: crate::ui_script::VmMemo::default(),
            });

        // No session edge — the memory stands (the `/reload` half).
        app.world_mut()
            .run_system_once(end_session_death_feed)
            .expect("the teardown runs");
        assert_eq!(
            app.world().resource::<DeathFeedState>().mirror,
            Some((0x1234, true, true)),
            "nothing but the session edge may clear the body's own state machine"
        );

        // A `/logout` is a session edge too — `session_over` is false for it, and the clear may
        // not depend on that (a relog is exactly the `false` case).
        app.world_mut()
            .write_message(crate::net::DisconnectedMessage::new(
                "logged out".into(),
                benilla_protocol::SessionEnd::LoggedOut,
            ));
        app.world_mut()
            .run_system_once(end_session_death_feed)
            .expect("the teardown runs");
        let feed = app.world().resource::<DeathFeedState>();
        assert!(
            feed.mirror.is_none(),
            "a relog must find no mirror, or the login-while-ghost arm cannot fire"
        );
        assert!(
            feed.died_at.is_none(),
            "the release window is the dead session's, and re-arms at the next login"
        );
    }
}

#[cfg(test)]
mod self_res_tests {
    use std::collections::HashMap;

    use benilla_formats::{SpellCatalog, SpellDisplay};
    use benilla_protocol::messages::{ItemInfo, ItemSpellEntry};
    use benilla_protocol::ObjectFields;

    use super::{resolve_self_res, SelfRes, SPELL_EFFECT_SELF_RESURRECT};
    use crate::items::Items;
    use crate::net::{ClientCommand, NetCommands};
    use crate::ui_action::Spells;

    /// Reincarnation's effect spell — what the server writes into the field, named in `Spell.dbc`
    /// as the button text (decision 1746).
    const REINCARNATION: u32 = 21169;
    /// `PLAYER_SELF_RES_SPELL`.
    const F_SELF_RES: u16 = 1224;
    /// `UNIT_FIELD_HEALTH` / `UNIT_FIELD_MAXHEALTH`. Both, because benilla's `unit_is_dead()`
    /// guards on a non-zero max — an all-empty store must not read as a corpse (its own doc). The
    /// binary's gate is the bare signed `health <= 0`; for the self player the two never differ.
    const F_HEALTH: u16 = 22;
    const F_MAXHEALTH: u16 = 28;
    /// `PLAYER_FIELD_PACK_SLOT_1` — the backpack's 16 slots, two dwords per guid.
    const F_PACK_1: u16 = 532;

    fn commands() -> (NetCommands, crossbeam_channel::Receiver<ClientCommand>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (NetCommands(tx), rx)
    }

    fn catalog(pairs: impl IntoIterator<Item = (u32, SpellDisplay)>) -> Spells {
        Spells {
            catalog: SpellCatalog::from_displays(pairs.into_iter().collect::<HashMap<_, _>>()),
            ..Spells::empty_for_tests()
        }
    }

    fn named(name: &str, effects: [u32; 3]) -> SpellDisplay {
        SpellDisplay {
            name: name.to_string(),
            effects,
            ..Default::default()
        }
    }

    /// **The dead gate comes first** (wow-re death-ui.md §10.1): `HasSoulstone` reads
    /// `UNIT_FIELD_HEALTH` and answers nil while it is positive. A living player holding a
    /// standing self-res spell gets nothing — and neither does a released **ghost**, whose health
    /// the server sets to exactly 1 (`BuildPlayerRepop`). That is what confines the answer to the
    /// pre-release window the DEATH dialog lives in.
    #[test]
    fn the_dead_gate_precedes_the_field() {
        let (net, _rx) = commands();
        let mut items = Items::default();
        let spells = catalog([(REINCARNATION, named("Reincarnation", [94, 0, 0]))]);

        let alive = ObjectFields::from_pairs(&[
            (F_MAXHEALTH, 4000),
            (F_HEALTH, 4000),
            (F_SELF_RES, REINCARNATION),
        ]);
        assert_eq!(
            resolve_self_res(&alive, &mut items, Some(&spells), &net),
            None,
            "alive with a self-res owed: nil"
        );

        // A ghost's health is 1, not 0 — the same positive-health path, which is why releasing
        // takes the button away rather than the dialog merely hiding it.
        let ghost = ObjectFields::from_pairs(&[
            (F_MAXHEALTH, 4000),
            (F_HEALTH, 1),
            (F_SELF_RES, REINCARNATION),
        ]);
        assert_eq!(
            resolve_self_res(&ghost, &mut items, Some(&spells), &net),
            None,
            "a released ghost still holds the field, and still answers nil"
        );

        let dead = ObjectFields::from_pairs(&[
            (F_MAXHEALTH, 4000),
            (F_HEALTH, 0),
            (F_SELF_RES, REINCARNATION),
        ]);
        assert_eq!(
            resolve_self_res(&dead, &mut items, Some(&spells), &net),
            Some(SelfRes::Spell {
                spell: REINCARNATION,
                label: "Reincarnation".into(),
            })
        );
    }

    /// An id the catalog cannot resolve is **"UNKNOWN"**, not nil (§10.1's fourth exit). Hiding
    /// the button instead would silently strip a self-res the server is holding for us.
    #[test]
    fn an_unresolvable_spell_id_reads_unknown_not_nil() {
        let (net, _rx) = commands();
        let mut items = Items::default();
        let dead =
            ObjectFields::from_pairs(&[(F_MAXHEALTH, 4000), (F_HEALTH, 0), (F_SELF_RES, 999_999)]);
        for spells in [None, Some(catalog([]))] {
            assert_eq!(
                resolve_self_res(&dead, &mut items, spells.as_ref(), &net),
                Some(SelfRes::Spell {
                    spell: 999_999,
                    label: "UNKNOWN".into(),
                }),
                "no catalog and an empty catalog are the same miss"
            );
        }
    }

    /// **A zero field is not the nil path** (§10.3): the client falls through to the carried
    /// inventory and looks for an item whose ON-USE (`trigger == 0`) spell carries
    /// `SPELL_EFFECT_SELF_RESURRECT`, labelling the button with the ITEM's name. Three items
    /// stand in the backpack — one whose self-res spell is on an EQUIP trigger (must not count),
    /// one whose on-use spell is something else (must not count), and the real one.
    #[test]
    fn a_zero_field_falls_through_to_a_carried_item() {
        let (net, _rx) = commands();
        let mut items = Items::default();

        let item = |name: &str, spells: Vec<ItemSpellEntry>| ItemInfo {
            spells,
            ..crate::items::test_template(name)
        };
        let block = |spell_id: u32, trigger: u32| ItemSpellEntry {
            index: 0,
            spell_id,
            trigger,
            charges: 0,
            cooldown_ms: -1,
            category: 0,
            category_cooldown_ms: -1,
        };
        // 3026 "Use Soulstone" is the real effect-94 spell; 439 "Healing Potion" is not.
        items.insert_template(10, Some(item("Healthstone", vec![block(439, 0)])));
        items.insert_template(11, Some(item("Worn Trinket", vec![block(3026, 1)])));
        items.insert_template(
            12,
            Some(item("Ankh of Reincarnation", vec![block(3026, 0)])),
        );
        for (guid, entry) in [(0xA0_u64, 10_u32), (0xA1, 11), (0xA2, 12)] {
            items.insert_object(guid, ObjectFields::from_pairs(&[(3, entry)]));
        }
        let spells = catalog([
            (
                3026,
                named("Use Soulstone", [SPELL_EFFECT_SELF_RESURRECT, 0, 0]),
            ),
            (439, named("Healing Potion", [6, 0, 0])),
        ]);

        // Backpack slots 23/24/25 hold the three, in that order.
        let mut pairs = vec![(F_MAXHEALTH, 4000), (F_HEALTH, 0)];
        for (i, guid) in [0xA0_u64, 0xA1, 0xA2].iter().enumerate() {
            let f = F_PACK_1 + 2 * i as u16;
            pairs.push((f, (*guid & 0xffff_ffff) as u32));
            pairs.push((f + 1, (*guid >> 32) as u32));
        }
        let dead = ObjectFields::from_pairs(&pairs);

        match resolve_self_res(&dead, &mut items, Some(&spells), &net) {
            Some(SelfRes::Item { entry, label, .. }) => {
                assert_eq!(
                    entry, 12,
                    "the EQUIP-trigger copy and the potion are skipped"
                );
                assert_eq!(label, "Ankh of Reincarnation", "the ITEM names the button");
            }
            other => panic!("expected the item leg, got {other:?}"),
        }

        // Take it away and the answer is nil again — not "UNKNOWN", which is the spell leg's.
        let bare = ObjectFields::from_pairs(&[(F_MAXHEALTH, 4000), (F_HEALTH, 0)]);
        assert_eq!(
            resolve_self_res(&bare, &mut items, Some(&spells), &net),
            None
        );
    }
}
