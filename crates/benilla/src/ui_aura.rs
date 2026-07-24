//! The app-side **aura feed** (decisions 0255/0257): turns the live `UNIT_FIELD_AURA` blocks of
//! our own avatar and of the current target into the ordered [`AuraState`] lists the `UnitAura`
//! bindings read, maintains the reference client's insertion-ordered display cache for the player,
//! joins in the self-only durations, and drains the right-click cancels back to the wire.
//!
//! Two things make this more than a snapshot projection:
//!
//! - **Order is state.** The player's buff bar draws a densely packed cache in *insertion* order that
//!   repacks on removal (`PlayerAuras_Update`, byte-verified — decision 0257). Ascending slot order,
//!   which a fresh descriptor read would give, is a *different* order and would shuffle icons into
//!   recycled slots. So [`PlayerAuraCache`] is carried across frames: survivors keep their position,
//!   a dropped aura closes its gap, a new aura appends at the end.
//! - **Durations arrive out of band, and before the slot is named.** `SMSG_UPDATE_AURA_DURATION`
//!   ([`AuraDurations`], filled by the net apply path) is keyed by raw slot and lands *before* the
//!   `UNIT_FIELD_AURA` delta that says which spell sits there (verified, decision 0257 B6). The feed
//!   joins a duration to an aura by slot, gated on the aura having appeared no later than the packet
//!   — so a stale timer left in a recycled slot by a since-expired aura is never shown on the
//!   permanent aura that took its place (the reference avoids this via a DBC "until cancelled" flag
//!   we don't parse; the arrival-time gate is our equivalent, decision 0257 §3).
//!
//! Scope: the **local player** (decisions 0255/0257) and the **target** (the target frame's aura
//! rows — 0255's deferred slice). The target's list is the byte-verified *other-unit* law
//! (decision 0257): `UnitBuff`/`UnitDebuff` on another unit read that unit's own `UNIT_FIELD_AURA`
//! straight, **ascending raw slot within the half** — no insertion cache, no durations (the 1.12
//! wire carries none for anyone but yourself). It **does** carry the display filter, though: an aura
//! whose spell is flagged never-display (`NO_AURA_ICON`/`DO_NOT_DISPLAY` — a warrior stance) is
//! hidden on *every* aura display, target rows included, not just the player's own bar (decision
//! 0417, correcting 0268's scope note and wow-re §9 — the director watched the reference hide a
//! target's Battle Stance, which the "other-unit reads straight" reading can't explain: `NO_AURA_ICON`
//! means exactly that, everywhere). A self-target is the one exception in *ordering*: decision 0257 §2
//! resolves the Era API's single `UnitAura` toward the player-bar law under every token, so targeting
//! yourself shows the same insertion-ordered list — filtered identically either way.

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_formats::SpellCatalog;
use benilla_protocol::messages::{UnitAuraSlot, AURA_FLAG_CANCELABLE, UNIT_AURA_POSITIVE_SLOTS};
use benilla_ui::script::{AuraState, ScriptValue, TrackingState, UiScript};

use crate::net::{ClientCommand, Guid, NetCommands, ObjectStore, SelfPlayer};
use crate::target::Selection;
use crate::ui_action::Spells;
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// The self-only aura durations, keyed by raw `UNIT_FIELD_AURA` slot — the decoded
/// `SMSG_UPDATE_AURA_DURATION` payload, stamped with the Bevy time it arrived. Written by the net
/// apply path (which owns the event stream), read by [`feed_auras`]. A slot's entry is overwritten
/// by each fresh packet (apply/refresh) and pruned when the feed sees the slot go empty.
#[derive(Resource, Default)]
pub(crate) struct AuraDurations {
    by_slot: HashMap<u8, DurationStamp>,
}

struct DurationStamp {
    /// Full duration in seconds — the packet's `remaining_ms`, which at apply/refresh is the total.
    total: f64,
    /// The Bevy-clock instant the aura runs out (`received_at + total`).
    expires_at: f64,
    /// The Bevy-clock instant the packet arrived — the freshness gate against a recycled slot.
    received_at: f64,
}

impl AuraDurations {
    /// Record a `SMSG_UPDATE_AURA_DURATION`. Called from the net apply drain (decision 0257).
    pub(crate) fn set(&mut self, slot: u8, remaining_ms: u32, now: f64) {
        let total = f64::from(remaining_ms) / 1000.0;
        self.by_slot.insert(
            slot,
            DurationStamp {
                total,
                expires_at: now + total,
                received_at: now,
            },
        );
    }
}

/// One aura in the player's display cache — a benilla mirror of a `0xbc6040` record (decision 0257).
/// Kept across frames so the insertion order survives; the display fields refresh from the
/// descriptor each frame, the position does not.
struct CachedAura {
    slot: u8,
    spell_id: u32,
    /// The Bevy-clock instant this aura first entered the cache — the duration freshness gate.
    appeared_at: f64,
    flags: u8,
    level: u8,
    stacks: u8,
}

/// The player's insertion-ordered aura cache (decision 0257): buffs and debuffs interleaved in the
/// order the reference client's `PlayerAuras_Update` would hold them. Split into the two filtered
/// lists only at the push, since the bindings filter by sign themselves.
#[derive(Resource, Default)]
pub(crate) struct PlayerAuraCache {
    auras: Vec<CachedAura>,
}

impl PlayerAuraCache {
    /// The live aura spell ids — `ui_tooltip`'s pre-feed reads these at arrival so a buff-bar
    /// hover's `SetPlayerBuff` view hits on the FIRST enter (the ask-once path stays as the
    /// odd-case fallback).
    pub(crate) fn spell_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.auras.iter().map(|a| a.spell_id)
    }
}

/// Adds the aura feed. The `UnitAura` bindings live in `benilla-ui`; this supplies their data and
/// fires `UNIT_AURA`, and drains `CancelUnitBuff` back to the wire.
pub(crate) struct UiAuraPlugin;

impl Plugin for UiAuraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AuraDurations>()
            .init_resource::<PlayerAuraCache>()
            // Feed before the VM dispatch (like the unit feed), so a frame's OnEvent sees the fresh
            // list; drain the cancels after, once the VM has queued them.
            .add_systems(Update, feed_auras.in_set(UnitFeed).before(UiInput))
            .add_systems(Update, drain_aura_cancels.after(UiInput));
    }
}

/// Reconcile the insertion-ordered cache against the live descriptor: drop entries whose slot no
/// longer holds their spell (expired, or the slot was recycled), then append newly occupied slots in
/// ascending order (so a same-tick multi-apply is deterministic). Survivors keep their position and
/// refresh their mutable fields. Mirrors `PlayerAuras_Update`'s shift-down + append (decision 0257).
fn reconcile(cache: &mut Vec<CachedAura>, live: &[UnitAuraSlot], now: f64) {
    cache.retain(|c| {
        live.iter()
            .any(|a| a.slot == c.slot && a.spell_id == c.spell_id)
    });
    for a in live {
        if let Some(c) = cache.iter_mut().find(|c| c.slot == a.slot) {
            // Same slot, same spell (retain guaranteed it) — refresh the volatile fields in place.
            c.flags = a.flags;
            c.level = a.level;
            c.stacks = a.stacks;
        } else {
            cache.push(CachedAura {
                slot: a.slot,
                spell_id: a.spell_id,
                appeared_at: now,
                flags: a.flags,
                level: a.level,
                stacks: a.stacks,
            });
        }
    }
}

/// One aura's discrete projection (spell id, stacks, cancelable, dispel class — the fields a frame
/// repaints on). Excludes the countdown, which drifts every frame and is Lua's `OnUpdate` job, not
/// a `UNIT_AURA` trigger.
type AuraProjection = (u32, u8, bool, Option<String>);

/// The feed's edge-detection memory: per token, the projection of the list last pushed.
#[derive(Default)]
struct AuraFeedMemory {
    present: bool,
    last: Vec<AuraProjection>,
    /// The tracking spell last pushed ([`tracking_state_of`]) — part of the player half's change
    /// key: switching Find Herbs → Find Minerals changes NO visible list (both are excluded from
    /// the bar), only this, and the minimap frame still needs its event.
    tracking_last: Option<u32>,
    /// The target half: the selection guid + projection last pushed under `"target"` (`None` = the
    /// token is cleared). The guid joins the key so a target *switch* re-fires `UNIT_AURA` even
    /// between two units whose lists happen to project identically.
    target_last: Option<(u64, Vec<AuraProjection>)>,
}

fn projection_of(list: &[AuraState]) -> Vec<AuraProjection> {
    list.iter()
        .map(|a| (a.spell_id, a.count, a.cancelable, a.debuff_type.clone()))
        .collect()
}

/// The reference's aura display filter (decisions 0268 + 0417): an aura is shown iff its spell is
/// *not* flagged never-display (`SPELL_ATTR_DO_NOT_DISPLAY` / `SPELL_ATTR_EX_NO_AURA_ICON`, via
/// `SpellDisplay::hidden_from_aura_bar`) **and** is not a tracking spell
/// (`SpellDisplay::tracking_aura` — the `{0x2c,0x2d,0x97}` effect exclusion both byte-verified
/// filters carry: the player-cache rebuild skips a tracking aura *before* the insert, diverting it
/// to the tracking global instead, and `IsAuraDisplayable 0x519860` hides it from other units'
/// rows the same way; wow-re `aura-display-pipeline.md` §3/§9a). This holds on **every** aura
/// display — the player's own bar and any other unit's rows alike (the director watched the
/// reference hide a target's Battle Stance; 0417 corrects 0268's player-only scope note and
/// wow-re §9). A spell the catalog can't resolve stays visible — fail-open, like every other
/// catalog miss in the feed (and like the reference's own no-SpellRec path, which inserts).
fn shown_in_aura_ui(catalog: Option<&SpellCatalog>, spell_id: u32) -> bool {
    catalog
        .and_then(|c| c.get(spell_id))
        .is_none_or(|d| !d.hidden_from_aura_bar() && !d.tracking_aura())
}

/// The player's active tracking aura — the reference's tracking global (`DAT_00bc6378`, wow-re
/// `aura-display-pipeline.md` §3): the cache rebuild walks the raw slots **ascending** and each
/// visible tracking-effect aura overwrites it, so the LAST one wins. The attribute clauses are
/// tested *first* in the reference (the `goto` skips the effect loop), so an attribute-hidden
/// tracking spell never lands here; a catalog miss can't be identified as tracking and lands in
/// the bar instead (the reference's own no-SpellRec path). Read by `GetTrackingTexture` /
/// `CancelTrackingBuff` / `GameTooltip:SetTrackingSpell` via [`UiScript::set_tracking`].
fn tracking_state_of(
    catalog: Option<&SpellCatalog>,
    occupied: &[UnitAuraSlot],
) -> Option<TrackingState> {
    occupied.iter().rev().find_map(|a| {
        let d = catalog.and_then(|c| c.get(a.spell_id))?;
        (!d.hidden_from_aura_bar() && d.tracking_aura()).then(|| TrackingState {
            spell_id: a.spell_id,
            name: Some(d.name.clone()),
            icon: d.icon.clone(),
            cancelable: a.flags & AURA_FLAG_CANCELABLE != 0,
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn feed_auras(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<(&ObjectStore, &Guid), With<SelfPlayer>>,
    selection: Res<Selection>,
    stores: Query<&ObjectStore>,
    spells: Option<Res<Spells>>,
    mut durations: ResMut<AuraDurations>,
    mut cache: ResMut<PlayerAuraCache>,
    time: Res<Time>,
    mut mem: Local<AuraFeedMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Ok((store, self_guid)) = self_q.single() else {
        // No avatar yet: clear once, so a stale list can't linger across a logout.
        if mem.present || mem.target_last.is_some() {
            script.set_auras("player", None);
            script.set_auras("target", None);
            script.set_tracking(None);
            cache.auras.clear();
            *mem = AuraFeedMemory::default();
        }
        return;
    };

    let bevy_now = time.elapsed_secs_f64();
    let catalog = spells.as_ref().map(|s| &s.catalog);
    let occupied: Vec<UnitAuraSlot> = store.0.unit_auras().collect();
    // The reference's DISPLAY FILTER (byte-verified sites `0x4e42b6`–`0x4e42c8`; decisions
    // 0268 + 0385, and 0417 for the target frame): a slot whose spell carries
    // `SPELL_ATTR_DO_NOT_DISPLAY` / `SPELL_ATTR_EX_NO_AURA_ICON` (warrior stances) stays live in
    // `UNIT_FIELD_AURA` but is never shown — `NO_AURA_ICON` means exactly that, on every aura display.
    // For the player's bar we filter the cache's *input*, so a hidden aura never takes a cache
    // position and insertion order/repacking match the reference's; [`shown_in_aura_ui`] is the same
    // predicate the target rows apply below. A catalog miss stays visible — fail-open.
    let live: Vec<UnitAuraSlot> = occupied
        .iter()
        .filter(|a| shown_in_aura_ui(catalog, a.spell_id))
        .copied()
        .collect();
    reconcile(&mut cache.auras, &live, bevy_now);

    // Prune durations for slots that no longer hold any aura — that's the only moment we can be sure
    // a stamp is stale (a still-occupied slot may legitimately keep a permanent aura with no timer).
    // Keyed on every OCCUPIED slot, hidden or not: the reference's expiry array is raw-slot state,
    // independent of the display filter.
    durations
        .by_slot
        .retain(|slot, _| occupied.iter().any(|a| a.slot == *slot));

    let script_now = script.now();

    let list: Vec<AuraState> = cache
        .auras
        .iter()
        .map(|c| {
            let display = catalog.and_then(|cat| cat.get(c.spell_id));
            // A duration counts only if it arrived no earlier than this aura appeared (minus a
            // tick's slack): a stamp older than the aura belongs to a since-expired occupant of the
            // recycled slot (decision 0257 §3). Apply/refresh sends the packet just before the
            // descriptor delta, so a live aura's stamp is always within that slack.
            let (duration, expiration_time) = durations
                .by_slot
                .get(&c.slot)
                .filter(|d| d.received_at >= c.appeared_at - DURATION_SLACK)
                .map(|d| (d.total, script_now + (d.expires_at - bevy_now)))
                .unwrap_or((0.0, 0.0));
            AuraState {
                spell_id: c.spell_id,
                name: display.map(|d| d.name.clone()),
                icon: display.and_then(|d| d.icon.clone()),
                count: c.stacks,
                debuff_type: display.and_then(|d| d.debuff_type()).map(str::to_string),
                duration,
                expiration_time,
                helpful: c.slot < UNIT_AURA_POSITIVE_SLOTS,
                cancelable: c.flags & AURA_FLAG_CANCELABLE != 0,
            }
        })
        .collect();

    // The tracking global's benilla twin: derived from the same walk that excluded tracking spells
    // from `live` above, before the memory update so its change joins the edge key.
    let tracking = tracking_state_of(catalog, &occupied);
    let tracking_spell = tracking.as_ref().map(|t| t.spell_id);

    // Edge-trigger UNIT_AURA on a discrete change (the reference's PLAYER_AURAS_CHANGED), never on
    // the countdown alone — that's a per-frame OnUpdate on the button, not an event. The tracking
    // spell joins the key: a tracking *switch* changes no visible list, only the minimap icon.
    let projection = projection_of(&list);
    let changed = !mem.present || projection != mem.last || tracking_spell != mem.tracking_last;
    mem.last = projection;
    mem.tracking_last = tracking_spell;
    mem.present = true;

    // Debug affordance (`BENILLA_AURA_DUMP=1`): when the visible set changes, log every slot the
    // player's bar will draw — raw slot, spell id, resolved name, class, and the flags nibble. The
    // fastest answer to "what is actually on my bar, and should any of it be there?" — the husk that
    // `unit_aura`'s `& 0x0E` gate now hides never reaches here, so anything listed is a live aura
    // (decisions 0255/0257). Cheap: the env is only consulted on a change.
    if changed && std::env::var_os("BENILLA_AURA_DUMP").is_some() {
        info!("aura dump: {} aura(s) on the player bar", cache.auras.len());
        for c in &cache.auras {
            let name = catalog
                .and_then(|cat| cat.get(c.spell_id))
                .map(|d| d.name.as_str())
                .unwrap_or("<unknown spell>");
            info!(
                "  slot {:>2}  spell {:>5}  {:<26}  {:<6}  flags {:#06b}",
                c.slot,
                c.spell_id,
                name,
                if c.slot < UNIT_AURA_POSITIVE_SLOTS {
                    "buff"
                } else {
                    "debuff"
                },
                c.flags,
            );
        }
    }

    // The target's list (the target frame's aura rows — 0255's deferred slice). A self-target
    // mirrors the player list (decision 0257 §2: the player-bar law under every token); any other
    // unit is its descriptor read straight — ascending raw slot, durationless (the verified
    // other-unit law, 0257/0268) — but through the SAME display filter (`shown_in_aura_ui`): a
    // never-display aura (a warrior stance) is hidden here too, exactly as the reference hides it
    // (decision 0417 — the director's Battle-Stance-on-the-target report; corrects 0268's player-only
    // scope note). Filtering here rather than in the Lua binding keeps `UnitBuff("target", i)`
    // returning the i-th *shown* aura, matching the reference's own indices.
    let target_list: Option<Vec<AuraState>> =
        selection.target.zip(selection.guid).and_then(|(e, guid)| {
            if guid == self_guid.0 {
                return Some(list.clone());
            }
            let target_store = stores.get(e).ok()?;
            Some(
                target_store
                    .0
                    .unit_auras()
                    .filter(|a| shown_in_aura_ui(catalog, a.spell_id))
                    .map(|a| {
                        let display = catalog.and_then(|cat| cat.get(a.spell_id));
                        AuraState {
                            spell_id: a.spell_id,
                            name: display.map(|d| d.name.clone()),
                            icon: display.and_then(|d| d.icon.clone()),
                            count: a.stacks,
                            debuff_type: display.and_then(|d| d.debuff_type()).map(str::to_string),
                            // No duration for any unit but yourself — the 1.12 wire carries none
                            // (byte-verified, 0257 B6); the reference's target frame shows no timers.
                            duration: 0.0,
                            expiration_time: 0.0,
                            helpful: a.slot < UNIT_AURA_POSITIVE_SLOTS,
                            cancelable: a.flags & AURA_FLAG_CANCELABLE != 0,
                        }
                    })
                    .collect(),
            )
        });

    let target_cur = selection
        .guid
        .zip(target_list.as_deref())
        .map(|(guid, l)| (guid, projection_of(l)));
    let target_changed = target_cur.is_some() && target_cur != mem.target_last;
    mem.target_last = target_cur;

    // The BENILLA_AURA_DUMP affordance's target half: what the target rows will draw, on change.
    if target_changed && std::env::var_os("BENILLA_AURA_DUMP").is_some() {
        let l = target_list.as_deref().unwrap_or_default();
        info!("aura dump: {} aura(s) on the target rows", l.len());
        for a in l {
            info!(
                "  spell {:>5}  {:<26}  {:<6}  {}",
                a.spell_id,
                a.name.as_deref().unwrap_or("<unknown spell>"),
                if a.helpful { "buff" } else { "debuff" },
                a.debuff_type.as_deref().unwrap_or("-"),
            );
        }
    }

    script.set_auras("player", Some(list));
    // Clearing the token isn't a UNIT_* event (the frame reacts to PLAYER_TARGET_CHANGED) —
    // same convention as the unit feed's snapshot clear.
    script.set_auras("target", target_list);
    script.set_tracking(tracking);
    if changed {
        script.fire_event("UNIT_AURA", vec![ScriptValue::Str("player".into())]);
        // The reference's own event for the same rebuild — PLAYER_AURAS_CHANGED, no args — which
        // the verbatim-transcribed 1.12 frames register (MiniMapTrackingFrame). Fired beside the
        // Era-shaped UNIT_AURA the adapted BuffFrame listens on: one rebuild, both dialects.
        script.fire_event("PLAYER_AURAS_CHANGED", vec![]);
    }
    if target_changed {
        script.fire_event("UNIT_AURA", vec![ScriptValue::Str("target".into())]);
    }
}

/// The apply/refresh-to-descriptor slack: a duration packet is accepted for an aura if it arrived no
/// more than this long before the aura appeared. Generous versus the sub-frame lead the server
/// actually gives (the packet precedes the same-tick descriptor delta), tight versus the seconds a
/// stale recycled-slot stamp would be off by.
const DURATION_SLACK: f64 = 1.0;

/// Drain the spell ids `CancelUnitBuff` queued this frame and send one `CMSG_CANCEL_AURA` each. The
/// server cancels by spell, not slot (decision 0257 B8); it refuses anything the wire's
/// `AFLAG_CANCELABLE` bit didn't allow, which the binding already gated on.
fn drain_aura_cancels(script: Option<NonSendMut<UiScript>>, net: Res<NetCommands>) {
    let Some(mut script) = script else {
        return;
    };
    for spell_id in script.take_cancel_aura_requests() {
        let _ = net.0.send(ClientCommand::CancelAura { spell_id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(slot: u8, spell_id: u32) -> UnitAuraSlot {
        UnitAuraSlot {
            slot,
            spell_id,
            flags: if slot < UNIT_AURA_POSITIVE_SLOTS {
                AURA_FLAG_CANCELABLE | 0x8
            } else {
                0x8
            },
            level: 60,
            stacks: 1,
        }
    }

    fn order(cache: &[CachedAura]) -> Vec<(u8, u32)> {
        cache.iter().map(|c| (c.slot, c.spell_id)).collect()
    }

    /// The display filter both the player bar and the target rows run through (`shown_in_aura_ui`),
    /// exercised against the REAL 5875 `Spell.dbc`: a warrior's Battle Stance carries `NO_AURA_ICON`
    /// and is hidden on every frame (decision 0417 — the director's "battle stance on the target
    /// frame" report), while Battle Shout is a real buff that stays. Skips without client data.
    #[test]
    fn the_aura_display_filter_hides_a_real_battle_stance_but_keeps_battle_shout() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc");

        // A target carrying Battle Stance (2457) + Battle Shout (6673): the stance is filtered out of
        // what the rows draw, exactly as the reference hides it; the shout survives.
        let slots = [slot(0, 2457), slot(1, 6673)];
        let shown: Vec<u32> = slots
            .iter()
            .filter(|a| shown_in_aura_ui(Some(&catalog), a.spell_id))
            .map(|a| a.spell_id)
            .collect();
        assert_eq!(shown, [6673], "the stance is filtered, the shout stays");

        // Fail-open: no catalog at all, or an id the catalog can't resolve, stays visible.
        assert!(shown_in_aura_ui(None, 2457));
        assert!(shown_in_aura_ui(Some(&catalog), 0xffff_fffe));
    }

    /// The tracking half of the display filter (the Pass-2 law, wow-re `aura-display-pipeline.md`
    /// §3: the `{0x2c,0x2d,0x97}` effect exclusion + the tracking global), against the REAL 5875
    /// `Spell.dbc`: a tracking aura rides a visible `UNIT_FIELD_AURA` slot but never reaches any
    /// bar — it is diverted to the tracking state instead, and the ascending walk's LAST tracking
    /// aura wins the global. Skips without client data.
    #[test]
    fn a_real_tracking_aura_is_diverted_from_the_bar_to_the_tracking_state() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc");

        // Find Herbs 2383 (TRACK_RESOURCES 45), Battle Shout 6673 (an ordinary buff), Track
        // Beasts 1494 (TRACK_CREATURES 44): only the shout is shown on any aura display...
        let slots = [slot(0, 2383), slot(1, 6673), slot(2, 1494)];
        let shown: Vec<u32> = slots
            .iter()
            .filter(|a| shown_in_aura_ui(Some(&catalog), a.spell_id))
            .map(|a| a.spell_id)
            .collect();
        assert_eq!(
            shown,
            [6673],
            "both tracking auras are diverted, the shout stays"
        );

        // ...and the tracking global holds the LAST tracking aura of the ascending walk (each
        // match overwrites), with the display fields the icon + tooltip read.
        let t = tracking_state_of(Some(&catalog), &slots).expect("tracking state set");
        assert_eq!(
            t.spell_id, 1494,
            "slot 2's Track Beasts overwrites slot 0's Find Herbs"
        );
        assert_eq!(t.name.as_deref(), Some("Track Beasts"));
        assert!(t.icon.is_some(), "the icon path GetTrackingTexture returns");
        assert!(
            t.cancelable,
            "the synthesized slot carries AFLAG_CANCELABLE"
        );

        // No tracking aura live → no state (the frame's hide branch); and without a catalog no
        // aura can be identified as tracking (the reference's own no-SpellRec path inserts into
        // the bar instead — fail-open both sides of the diversion).
        assert!(tracking_state_of(Some(&catalog), &[slot(0, 6673)]).is_none());
        assert!(tracking_state_of(None, &slots).is_none());
    }

    /// The distinguishing behaviour of decision 0257: a newly-applied aura in a *lower* slot
    /// appends at the END of the cache, it is not sorted to the front by slot. A plain
    /// `unit_auras()` read (ascending slot) would give the opposite order — this is exactly the
    /// difference between the descriptor's order and the display order.
    #[test]
    fn a_new_low_slot_aura_appends_at_the_end_not_sorted_by_slot() {
        let mut cache = Vec::new();
        // X lands in slot 5 first.
        reconcile(&mut cache, &[slot(5, 100)], 1.0);
        assert_eq!(order(&cache), [(5, 100)]);
        // Y then lands in slot 2 — the descriptor now reads ascending [2, 5], but Y is the newer
        // aura, so it goes to the end.
        reconcile(&mut cache, &[slot(2, 200), slot(5, 100)], 2.0);
        assert_eq!(
            order(&cache),
            [(5, 100), (2, 200)],
            "insertion order, not ascending slot"
        );
    }

    /// A dropped aura closes its gap; the survivors keep their relative order and slide along — the
    /// `PlayerAuras_Update` shift-down. A recycled slot then appends fresh, not into the hole.
    #[test]
    fn a_dropped_aura_repacks_and_a_recycled_slot_appends_fresh() {
        let mut cache = Vec::new();
        reconcile(&mut cache, &[slot(0, 10), slot(1, 20), slot(2, 30)], 1.0);
        assert_eq!(order(&cache), [(0, 10), (1, 20), (2, 30)]);

        // The middle aura (slot 1) drops.
        reconcile(&mut cache, &[slot(0, 10), slot(2, 30)], 2.0);
        assert_eq!(order(&cache), [(0, 10), (2, 30)], "the gap closes");

        // Slot 1 is recycled by a new spell — it appends at the end, not back into the old middle.
        reconcile(&mut cache, &[slot(0, 10), slot(1, 99), slot(2, 30)], 3.0);
        assert_eq!(
            order(&cache),
            [(0, 10), (2, 30), (1, 99)],
            "the recycled slot is the newest, so it is last"
        );
    }

    /// A surviving aura refreshes its volatile fields in place (a stack change) without moving.
    #[test]
    fn a_surviving_aura_refreshes_in_place() {
        let mut cache = Vec::new();
        reconcile(&mut cache, &[slot(0, 10), slot(1, 20)], 1.0);
        let mut restacked = slot(1, 20);
        restacked.stacks = 5;
        reconcile(&mut cache, &[slot(0, 10), restacked], 2.0);
        assert_eq!(order(&cache), [(0, 10), (1, 20)], "position unchanged");
        assert_eq!(cache[1].stacks, 5, "stack count refreshed");
        assert_eq!(cache[0].appeared_at, 1.0, "appeared_at is not disturbed");
    }

    /// The duration freshness gate (decision 0257 §3): a stamp older than the aura is ignored — the
    /// stale-recycled-slot defence — while a stamp from around the apply is accepted.
    #[test]
    fn a_duration_is_joined_only_when_it_is_no_older_than_the_aura() {
        // A stamp received at t=100 is stale for an aura that only appeared at t=200.
        let stale = DurationStamp {
            total: 30.0,
            expires_at: 130.0,
            received_at: 100.0,
        };
        assert!(
            stale.received_at < 200.0 - DURATION_SLACK,
            "a stamp seconds older than the aura is rejected"
        );
        // A stamp received just before the aura appeared (the real apply→descriptor lead) is fresh.
        let fresh = DurationStamp {
            total: 30.0,
            expires_at: 230.0,
            received_at: 200.0 - 0.1,
        };
        assert!(fresh.received_at >= 200.0 - DURATION_SLACK, "accepted");
    }
}
