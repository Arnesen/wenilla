//! The per-action **dynamic-state feed** (decision 0137 phase 4) — the app-side computation
//! behind the engine's `IsUsableAction`/`IsActionInRange`/`IsCurrentAction`/`GetActionCooldown`
//! family: each occupied action slot's [`ActionState`], recomputed per frame, diff-pushed into
//! the VM, with the reference client's own event edges fired on the transitions the 2026-07-10
//! wow-re §5 byte-mapped (`system/ui/scratch/action-button-state-api.md`):
//!
//! - a cooldown-store change → `ACTIONBAR_UPDATE_COOLDOWN` + `SPELL_UPDATE_COOLDOWN` +
//!   `BAG_UPDATE_COOLDOWN` (the `0x4b31b0`/`0x4f93d0` flush pair the SMSG handlers call);
//! - a usable/oom change on any slot → `ACTIONBAR_UPDATE_USABLE` + `SPELL_UPDATE_USABLE`
//!   (`0x4b31c0`; the client fires only on a cache CHANGE — `0x4e5c00` — hence the diff edge);
//! - a current/auto-repeat change → `ACTIONBAR_UPDATE_STATE` + `CURRENT_SPELL_CAST_CHANGED`
//!   (`0x4b3250`);
//! - our own melee engage/disengage → `PLAYER_ENTER_COMBAT`/`PLAYER_LEAVE_COMBAT`
//!   (`0x6256ff`/`0x625778` — the attack-start/stop handlers);
//! - the live autorepeat key's edges → `START_AUTOREPEAT_SPELL` (`0x6e5952`, at cast-send) /
//!   `STOP_AUTOREPEAT_SPELL` (`0x6ea170`).
//!
//! The per-flag semantics are the §5's confirmed laws (C1–C5): `notEnoughMana` is strictly the
//! power-cost verdict, `IsCurrentAction` keys on the engaged attack GUID / the in-flight cast id,
//! `IsAutoRepeatAction` on the `0xceac30` key, and the range test is squared distance against the
//! byte-verified `GetMinMaxRange 0x6e3480` (its constants transcribed below). The usable pair
//! itself is the full `IsSpellUsableNow 0x6e3d60` gate walk — [`super::usable`], the 2026-07-10
//! §2a fold-back: reagents, forms, stealth, aura states (the Execute-family target dependence),
//! the works.

use std::collections::HashMap;
use std::time::Instant;

use bevy::prelude::*;

use benilla_formats::{SpellDisplay, SpellRange};
use benilla_protocol::messages::{ACTION_KIND_ITEM, ACTION_KIND_SPELL};
use benilla_ui::script::{ActionState, UiScript};

use crate::cooldowns::Cooldowns;
use crate::creature_anim::{Casting, Engaged};
use crate::items::Items;
use crate::net::{GuidIndex, NetCommands, ObjectStore, SelfPlayer};
use crate::target::Selection;

use super::{usable, AutoRepeatActive, PlayerActions, Spells};

/// `GetMinMaxRange 0x6e3480`'s byte constants (wow-re `wave-cooldown.md` + the decomp
/// `FUN_006e3480`, VERIFIED): the **melee-branch-only** reach pad (`0x80b058`; the ranged
/// branch pads by the bare reach sum), the melee floor, and the self-cast short-circuit's
/// flat max.
const MELEE_REACH_PAD: f32 = 1.3333;
const MELEE_RANGE_FLOOR: f32 = 5.0;
const SELF_CAST_MAX: f32 = 100.0;

/// The feed's memory: what was last pushed, and the edge detectors.
#[derive(Default)]
pub(super) struct StateMemory {
    pushed: HashMap<u32, ActionState>,
    last_generation: Option<u64>,
    engaged: bool,
    auto_repeat: Option<u32>,
    /// Last `dbg_trace` "cd tick" stamp — the once-per-second gate (trace runs only).
    last_cd_trace: Option<Instant>,
}

/// The client's cast-fail reasons for the two range refusals ("Out of range." / "Target too
/// close" in [`super::cast_error_text`]'s table) — what `CanTargetUnit 0x6e4440` emits when
/// `IsTargetInRange 0x6e47b0` fails on its max² / min² compare.
pub(super) const ERR_OUT_OF_RANGE: u8 = 0x59;
pub(super) const ERR_TOO_CLOSE: u8 = 0x76;

/// The **pre-send** range refusal — the client's `TryCast` ladder runs `CanTargetUnit 0x6e4440`
/// → `IsTargetInRange 0x6e47b0` BEFORE `ArmCast`/`SendCast` (`wave-cast.md`, byte-verified), so
/// an out-of-range or too-close press fails locally and the commit tail — the ranged sheath
/// snap `0x6e5930` included — never runs. This is why a too-close Throw/Auto Shot must NOT draw
/// the ranged weapon. Squared 3D distance against [`resolve_range`]'s {min, max}: beyond max² →
/// [`ERR_OUT_OF_RANGE`], inside a nonzero min² → [`ERR_TOO_CLOSE`]. Untestable inputs (no range
/// row, unknown distance) pass — the server still judges the cast.
pub(super) fn cast_range_refusal(
    spell: &SpellDisplay,
    row: Option<&SpellRange>,
    self_reach: f32,
    target_reach: Option<f32>,
    dist_sq: Option<f32>,
) -> Option<u8> {
    let (min, max) = resolve_range(spell, row, self_reach, target_reach)?;
    let d2 = dist_sq?;
    if d2 > max * max {
        return Some(ERR_OUT_OF_RANGE);
    }
    if min > 0.0 && d2 < min * min {
        return Some(ERR_TOO_CLOSE);
    }
    None
}

/// The **pre-send** mounted refusal (decision 0481) — the requirement validator `0x6094f0`'s
/// mounted block (`0x609c6c`, wow-re `mounted-action-gate.md` §5): a live
/// `UNIT_FIELD_MOUNTDISPLAYID` refuses the cast with reason `0x39` ("You are mounted") unless
/// the spell carries Attributes bit 24 (`0x01000000`, castable-while-mounted — the exemption
/// test at `0x609c6f`, the exact vmangos `SPELL_ATTR_ALLOW_WHILE_MOUNTED` mirror). A spell
/// with no loaded record has no exemption to claim — the gate holds (the ref always has the
/// record; refusing without data errs toward the ref's visible behavior). The sibling
/// mount-REQUIRED gate (reason 0x53 off `SpellRec+0x5c & 0x40`, `0x609c05`) is recorded but
/// unbuilt — no 1.12 player spell exercises it.
pub(super) fn cast_mounted_refusal(mounted: bool, spell: Option<&SpellDisplay>) -> bool {
    mounted && spell.is_none_or(|d| d.attributes & 0x0100_0000 == 0)
}

/// The resolved {min, max} for one action against one target — the `GetMinMaxRange 0x6e3480`
/// law over our descriptor reaches.
///
/// Two decomp legs are deliberately UNMODELED (0426): the PvP max bonus (`6e3648` — +2.6667 yd
/// when both units carry the `[unit+0x118]+0x40 & 0x200d` flags and the pair is hostile; its
/// gate helpers `0x5fc350` are un-RE'd, so modeling it would be a guess) and the
/// `Attributes & 2` item-scaling leg (`6e36aa` — `max *= item range-mod %` off the resolved
/// item record; verified a data no-op 2026-07-16: vmangos `item_template.range_mod` is 100 on
/// all 513 player-obtainable ranged weapons, 0 only on nine NPC "Monster -" wands). The melee
/// no-target reach fallback also simplifies: the real client re-resolves the current-target
/// global (`0x47bf60(0x498)`) and failing that doubles the caster's own reach — we default the
/// missing side to 1.5.
fn resolve_range(
    spell: &SpellDisplay,
    range: Option<&SpellRange>,
    self_reach: f32,
    target_reach: Option<f32>,
) -> Option<(f32, f32)> {
    // The self-cast short-circuit's attribute test (`SpellRec+0x18 & 0x404` at `0x6e34fb`) —
    // the same on-next-swing mask the queue tracking reads, tested here by the range law.
    if spell.on_next_swing() {
        return Some((0.0, SELF_CAST_MAX));
    }
    let row = range?;
    if row.is_melee() {
        let reach_sum = self_reach + target_reach.unwrap_or(1.5) + MELEE_REACH_PAD;
        return Some((0.0, reach_sum.max(MELEE_RANGE_FLOOR)));
    }
    if row.min == 0.0 && row.max == 0.0 {
        return None; // the self row (id 1): no range to test
    }
    // The ranged branch (0x6e35ee) pads by the BARE reach sum — no 1.3333, that constant is
    // melee-only — added to the max unconditionally but to the min ONLY when the row's min is
    // already nonzero (the fcomp-vs-0.0 guard, decomp `if (*min != 0.0)`): a min-0 spell
    // (Fireball, Shadow Bolt) must never grow a min range, or point-blank casts refuse
    // TOO_CLOSE.
    let Some(target_reach) = target_reach else {
        return Some((row.min, row.max));
    };
    let pad = self_reach + target_reach;
    let min = if row.min == 0.0 { 0.0 } else { row.min + pad };
    Some((min, row.max + pad))
}

/// Compute + diff-push every occupied slot's dynamic state, and fire the reference event edges.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // a Bevy system's full input set
pub(super) fn feed_action_state(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    mut cooldowns: ResMut<Cooldowns>,
    auto_repeat: Res<AutoRepeatActive>,
    // One tuple param (Bevy's 16-SystemParam ceiling): our own cast tracking — the in-flight
    // guard, the queued on-next-swing strike, and the running channel.
    cast_state: (
        Res<crate::ui_cast::PendingCast>,
        Res<crate::ui_cast::QueuedMeleeSpell>,
        Res<crate::ui_cast::ActiveChannel>,
    ),
    self_q: Query<(&ObjectStore, &Transform, Has<Engaged>, Option<&Casting>), With<SelfPlayer>>,
    selection: Res<Selection>,
    index: Res<GuidIndex>,
    units: Query<(&ObjectStore, &Transform), Without<SelfPlayer>>,
    factions: Option<Res<crate::target::Factions>>,
    reputations: Res<crate::net::Reputations>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    mut memory: Local<StateMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    let now = Instant::now();
    // This frame's reading of the VM's GetTime clock — `ui_triple`'s conversion base: every
    // cooldown is pushed as its absolute start on that clock.
    let ui_now = script.now();
    cooldowns.prune(now);
    let gen_changed = memory.last_generation != Some(cooldowns.generation);
    memory.last_generation = Some(cooldowns.generation);
    // The cooldown-clock trace (`WOW_MOVE_TRACE` sink, tag "cd"): once per second.
    let trace_cd = crate::dbg_trace::enabled()
        && memory
            .last_cd_trace
            .is_none_or(|t| now.duration_since(t).as_secs_f32() >= 1.0);
    if trace_cd {
        memory.last_cd_trace = Some(now);
    }

    let (pending, queued_melee, channel) = &cast_state;
    let me = self_q.iter().next();
    let engaged = me.is_some_and(|(_, _, e, _)| e);
    let form_byte = me
        .map(|(s, _, _, _)| s.0.unit_shapeshift_form())
        .unwrap_or(0);
    let casting_spell = me.and_then(|(_, _, _, c)| c.map(|c| c.spell_id));
    let current_cast = pending.current(now).or(casting_spell);
    let self_reach = me.map_or(1.5, |(s, _, _, _)| s.0.unit_combat_reach());
    let self_pos = me.map(|(_, t, _, _)| t.translation);
    // The current target's reach + squared distance (the client tests dx²+dy²+dz² — 0x6e47b0).
    let target = selection
        .guid
        .and_then(|g| index.0.get(&g))
        .and_then(|&e| units.get(e).ok());
    let target_reach = target.map(|(s, _)| s.0.unit_combat_reach());
    let dist_sq = match (self_pos, target) {
        (Some(a), Some((_, t))) => Some(a.distance_squared(t.translation)),
        _ => None,
    };

    let mut fresh: HashMap<u32, ActionState> = HashMap::new();
    for (&slot, button) in &actions.buttons {
        let action = u32::from(slot) + 1;
        let mut st = ActionState::default();
        match button.kind {
            ACTION_KIND_SPELL => {
                let d = spells.as_ref().and_then(|s| s.catalog.get(button.action));
                let Some(d) = d else {
                    fresh.insert(action, st);
                    continue;
                };
                st.is_attack = d.is_melee_auto_attack();
                // C2: the Attack action is "current" while auto-attack is engaged; a castable
                // spell while it is our in-flight cast OR our queued on-next-swing strike OR our
                // running channel (the ref reads one inflight id `0xceca88` — which a queued
                // Heroic Strike *occupies* until the swing fires it — plus the channel id
                // `0xceac58`; our model splits the queue into its own slot, same observable) —
                // OR the shapeshift arm (`IsCurrentAction`'s predicate `0x4e53a0` @ `0x4e5556`,
                // wow-re `action-spell-icon-apis.md` §5): a MOD_SHAPESHIFT spell whose form ==
                // the player's form byte reads checked. Deliberately NOT the icon's aura-scan
                // predicate — the two are different functions in the binary and the asymmetry
                // is load-bearing (a form granted by a different spell lights the check without
                // swapping the icon).
                st.current = if st.is_attack {
                    engaged
                } else {
                    current_cast == Some(button.action)
                        || queued_melee.current() == Some(button.action)
                        || channel.current(now) == Some(button.action)
                        || (form_byte != 0 && d.shapeshift_form == Some(u32::from(form_byte)))
                };
                st.auto_repeat = auto_repeat.0 == Some(button.action);
                // The full usable walk (`0x6e3d60` §2a — [`super::usable`]): reagents, forms,
                // stealth, aura states, the bit-25 cooldown fold, and the power gate (the sole
                // notEnoughMana writer). Target-dependent for the Execute family only. `spells`
                // is necessarily Some here — `d` came out of it.
                if let (Some((store, _, _, _)), Some(sp)) = (me, spells.as_deref()) {
                    let ctx = usable::UsableCtx {
                        store,
                        target_store: target.map(|(s, _)| s),
                        factions: factions.as_deref(),
                        reputations: &reputations,
                        cooldowns: &cooldowns,
                        now,
                    };
                    let (u, oom) =
                        usable::spell_usable(button.action, d, sp, &ctx, &mut items, &commands);
                    st.usable = u;
                    st.not_enough_mana = oom;
                } else {
                    st.usable = true;
                }
                // C4: the range verdict vs the current target; nil without one.
                let row = spells.as_ref().and_then(|s| s.ranges.get(d.range_index));
                let resolved = resolve_range(d, row, self_reach, target_reach);
                st.has_range = resolved
                    .is_some_and(|(min, max)| min.abs() > f32::EPSILON || max.abs() > f32::EPSILON);
                st.in_range = match (resolved, dist_sq) {
                    (Some((min, max)), Some(d2)) if st.has_range => {
                        Some(d2 >= min * min && d2 <= max * max)
                    }
                    _ => None,
                };
                let info = cooldowns.info(button.action, 0, Some(d), now);
                st.cooldown = info.ui_triple(now, ui_now);
                if st.cooldown.is_some() && trace_cd {
                    // The store (Instant clock) vs the widget (GetTime clock) — the sink
                    // stamps the wall time, so drift between the two clocks reads directly.
                    crate::dbg_trace::line(
                        "cd",
                        &format!(
                            "tick action={} rem={}ms dur={}ms engine_now={ui_now:.3}",
                            button.action, info.remaining_ms, info.duration_ms,
                        ),
                    );
                }
            }
            ACTION_KIND_ITEM => {
                let template = items.template(button.action, 0, &commands).cloned();
                st.consumable = template.as_ref().is_some_and(|t| t.class == 0);
                let count = me
                    .map(|(s, _, _, _)| crate::ui_items::count_of(&s.0, &items, button.action))
                    .unwrap_or(0);
                // Worn on any equipment slot (0..18) — the green border's IsEquippedAction.
                st.equipped = me.is_some_and(|(s, _, _, _)| {
                    (0..19).any(|i| {
                        s.0.player_inv_slot(i)
                            .and_then(|g| items.object(g))
                            .and_then(|o| o.object_entry())
                            == Some(button.action)
                    })
                });
                st.usable = count > 0 || st.equipped;
                if let Some(u) = template.as_ref().and_then(|t| t.use_spell) {
                    let d = spells.as_ref().and_then(|s| s.catalog.get(u.spell_id));
                    let info = cooldowns.info(u.spell_id, button.action, d, now);
                    st.cooldown = info.ui_triple(now, ui_now);
                }
            }
            _ => {} // macros: everything cold until a macro window exists
        }
        // No between-generation carry: the triple holds the ABSOLUTE start, so one running
        // cooldown re-derives the same value every frame (no diff churn) and a re-arm derives a
        // new one (the sweep restarts). The old `(remaining, duration)` carry-the-stale-triple
        // scheme aliased a fail-clear+re-arm inside one inter-feed gap into "unchanged" — the
        // vanished-GCD-pie-on-spam bug.
        fresh.insert(action, st);
    }

    // Diff-push + collect which event families changed.
    let mut usable_changed = false;
    let mut state_changed = false;
    let keys: Vec<u32> = fresh
        .keys()
        .chain(memory.pushed.keys())
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    for action in keys {
        let (new, old) = (fresh.get(&action), memory.pushed.get(&action));
        if new == old {
            continue;
        }
        let d = ActionState::default();
        let (n, o) = (new.unwrap_or(&d), old.unwrap_or(&d));
        if (n.usable, n.not_enough_mana) != (o.usable, o.not_enough_mana) {
            usable_changed = true;
        }
        if (n.current, n.auto_repeat) != (o.current, o.auto_repeat) {
            state_changed = true;
        }
        if n.cooldown != o.cooldown && crate::dbg_trace::enabled() {
            crate::dbg_trace::line(
                "cd",
                &format!(
                    "push action={action} cooldown={:?} engine_now={:.3}",
                    n.cooldown,
                    script.now()
                ),
            );
        }
        script.set_action_state(action, new.copied());
    }
    memory.pushed = fresh;

    // The event edges, in the client's own flush order (the ACTIONBAR_* sibling first).
    if gen_changed {
        script.fire_event("ACTIONBAR_UPDATE_COOLDOWN", vec![]);
        script.fire_event("SPELL_UPDATE_COOLDOWN", vec![]);
        script.fire_event("BAG_UPDATE_COOLDOWN", vec![]);
    }
    if usable_changed {
        script.fire_event("ACTIONBAR_UPDATE_USABLE", vec![]);
        script.fire_event("SPELL_UPDATE_USABLE", vec![]);
    }
    if state_changed {
        script.fire_event("ACTIONBAR_UPDATE_STATE", vec![]);
        script.fire_event("CURRENT_SPELL_CAST_CHANGED", vec![]);
    }
    if engaged != memory.engaged {
        memory.engaged = engaged;
        script.fire_event(
            if engaged {
                "PLAYER_ENTER_COMBAT"
            } else {
                "PLAYER_LEAVE_COMBAT"
            },
            vec![],
        );
    }
    if auto_repeat.0 != memory.auto_repeat {
        memory.auto_repeat = auto_repeat.0;
        script.fire_event(
            if auto_repeat.0.is_some() {
                "START_AUTOREPEAT_SPELL"
            } else {
                "STOP_AUTOREPEAT_SPELL"
            },
            vec![],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spell_with_range(range_index: u32, attributes: u32) -> SpellDisplay {
        SpellDisplay {
            range_index,
            attributes,
            ..Default::default()
        }
    }

    /// The `GetMinMaxRange 0x6e3480` transcription: melee reach floor, the ranged reach pad on
    /// both bounds, the self-cast short-circuit, and the rangeless self row.
    #[test]
    fn resolve_range_follows_the_byte_law() {
        let melee = SpellRange {
            min: 0.0,
            max: 5.0,
            flags: 1,
        };
        // Two naked-reach units (1.5 + 1.5 + 1.3333 = 4.333) floor at 5.0…
        let d = spell_with_range(2, 0);
        assert_eq!(
            resolve_range(&d, Some(&melee), 1.5, Some(1.5)),
            Some((0.0, MELEE_RANGE_FLOOR))
        );
        // …a big pair (4 + 4 + 1.3333) exceeds it.
        let (_, max) = resolve_range(&d, Some(&melee), 4.0, Some(4.0)).unwrap();
        assert!((max - 9.3333).abs() < 1e-3);

        // Charge's 8–25 row pads both bounds by the BARE reach sum (no 1.3333 — melee-only)
        // against a unit target.
        let charge_row = SpellRange {
            min: 8.0,
            max: 25.0,
            flags: 0,
        };
        let (min, max) = resolve_range(&d, Some(&charge_row), 1.5, Some(1.5)).unwrap();
        assert!((min - (8.0 + 3.0)).abs() < 1e-3);
        assert!((max - (25.0 + 3.0)).abs() < 1e-3);

        // A min-0 row (Fireball's 0–35) pads the max only — the fcomp-vs-0.0 guard keeps the
        // min at zero, so a point-blank cast never reads a min range.
        let fireball_row = SpellRange {
            min: 0.0,
            max: 35.0,
            flags: 0,
        };
        let (min, max) = resolve_range(&d, Some(&fireball_row), 1.5, Some(1.5)).unwrap();
        assert_eq!(min, 0.0);
        assert!((max - 38.0).abs() < 1e-3);

        // No unit target: the row's raw bounds, unpadded.
        assert_eq!(
            resolve_range(&d, Some(&charge_row), 1.5, None),
            Some((8.0, 25.0))
        );

        // The self-cast attribute short-circuits to a flat 100 without touching the row.
        let selfish = spell_with_range(1, 0x400);
        assert_eq!(
            resolve_range(&selfish, None, 1.5, None),
            Some((0.0, SELF_CAST_MAX))
        );

        // The self row (0, 0, no melee flag) resolves to no range at all.
        let self_row = SpellRange {
            min: 0.0,
            max: 0.0,
            flags: 0,
        };
        assert_eq!(resolve_range(&d, Some(&self_row), 1.5, None), None);
    }

    /// The pre-send refusal (`IsTargetInRange 0x6e47b0`'s two compares over the resolved
    /// bounds): Auto Shot's {8, 35} row + the unit reach pad — a point-blank target refuses
    /// TOO_CLOSE, a distant one OUT_OF_RANGE, the sweet spot passes; untestable inputs pass.
    #[test]
    fn cast_range_refusal_follows_the_two_compares() {
        let d = spell_with_range(114, 0);
        let auto_shot = SpellRange {
            min: 8.0,
            max: 35.0,
            flags: 0,
        };
        let reach = Some(1.5);
        // Both bounds carry the bare reach pad (self 1.5 + target 1.5): min = 11, max = 38.
        let refuse = |d2: f32| cast_range_refusal(&d, Some(&auto_shot), 1.5, reach, Some(d2));
        assert_eq!(refuse(3.0 * 3.0), Some(ERR_TOO_CLOSE));
        assert_eq!(refuse(20.0 * 20.0), None);
        assert_eq!(refuse(60.0 * 60.0), Some(ERR_OUT_OF_RANGE));

        // The regression: a min-0 ranged row (Fireball/Shadow Bolt) must pass point-blank —
        // its min never grows a reach pad — while the max compare still holds.
        let fireball = SpellRange {
            min: 0.0,
            max: 35.0,
            flags: 0,
        };
        let refuse = |d2: f32| cast_range_refusal(&d, Some(&fireball), 1.5, reach, Some(d2));
        assert_eq!(refuse(0.1), None);
        assert_eq!(refuse(60.0 * 60.0), Some(ERR_OUT_OF_RANGE));

        // A melee-family row has min 0 — never TOO_CLOSE, still OUT_OF_RANGE beyond reach.
        let melee = SpellRange {
            min: 0.0,
            max: 5.0,
            flags: 1,
        };
        let melee_spell = spell_with_range(2, 0);
        assert_eq!(
            cast_range_refusal(&melee_spell, Some(&melee), 1.5, reach, Some(0.1)),
            None
        );
        assert_eq!(
            cast_range_refusal(&melee_spell, Some(&melee), 1.5, reach, Some(15.0 * 15.0)),
            Some(ERR_OUT_OF_RANGE)
        );

        // No row / no distance: nothing to test locally — pass (the server judges).
        assert_eq!(cast_range_refusal(&d, None, 1.5, reach, Some(1.0)), None);
        assert_eq!(
            cast_range_refusal(&d, Some(&auto_shot), 1.5, reach, None),
            None
        );
    }

    /// The mounted refusal (`0x609c6c`): a live mount blocks unless Attributes carries the
    /// bit-24 exemption (`0x609c6f`); unmounted always passes; a missing record can claim no
    /// exemption, so the gate holds.
    #[test]
    fn cast_mounted_refusal_honors_the_bit24_exemption() {
        let plain = SpellDisplay::default();
        let exempt = SpellDisplay {
            attributes: 0x0100_0000,
            ..Default::default()
        };
        assert!(cast_mounted_refusal(true, Some(&plain)));
        assert!(!cast_mounted_refusal(true, Some(&exempt)));
        assert!(!cast_mounted_refusal(false, Some(&plain)));
        assert!(!cast_mounted_refusal(false, None));
        assert!(cast_mounted_refusal(true, None), "no record, no exemption");
    }
}
