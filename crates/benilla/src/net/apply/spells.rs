//! Spell-book/action-bar + cast-lifecycle arm bodies for [`super::apply_net_updates`]'s dispatch
//! match — one of the largest arm families, split out on its own (the decision 0099/0107 precast →
//! resolve pipeline). Each `pub(super)` fn here is exactly one arm's body; the match at the call
//! site stays the dispatcher, one call per arm.

use std::time::{Duration, Instant};

use benilla_protocol::messages::{ActionButton, SpellCooldown, ACTION_KIND_SPELL};
use bevy::prelude::*;

use crate::cooldowns::Cooldowns;
use crate::creature_anim::{CastEvent, CastEventKind, Casting, SpellGoTargets};
use crate::ui_action::{AutoRepeatActive, CastErrors, PlayerActions, Spells};
use crate::ui_aura::AuraDurations;
use crate::ui_cast::{ActiveChannel, CastBarEdge, CastBarFeed, PendingCast, QueuedMeleeSpell};

use super::super::{GuidIndex, ObjectStore, SelfGuid};

/// The player's spell book (`SMSG_INITIAL_SPELLS`, once at login) → the action store the UI feed
/// reads (`crate::ui_action`), plus the active-cooldown list → the cooldown store (the wire
/// carries *remaining* ms — [`Cooldowns::seed_initial`]'s law).
pub(super) fn spell_book(
    spell_ids: Vec<u32>,
    initial_cooldowns: Vec<SpellCooldown>,
    actions: &mut PlayerActions,
    cooldowns: &mut Cooldowns,
) {
    debug!(
        "net: spell book — {} spells, {} active cooldown(s)",
        spell_ids.len(),
        initial_cooldowns.len()
    );
    actions.spells = spell_ids.into_iter().collect();
    actions.dirty = true;
    let now = Instant::now();
    for cd in &initial_cooldowns {
        cooldowns.seed_initial(cd, now);
    }
}

/// The player's saved action bar (`SMSG_ACTION_BUTTONS`, once at login, and again on server-side
/// edits) → the action store the UI feed reads.
pub(super) fn action_buttons(buttons: Vec<ActionButton>, actions: &mut PlayerActions) {
    debug!("net: action bar — {} occupied slots", buttons.len());
    actions.buttons = buttons.into_iter().map(|b| (b.slot, b)).collect();
    actions.dirty = true;
}

/// A spell added to the book after login (`SMSG_LEARNED_SPELL` — a trainer purchase, a quest reward,
/// a level-up rank gain; decision 0237). The spellbook feed diffs `spells` each frame, so the insert
/// is all it needs to surface (the add-gate that decides which known spells are *book* entries is
/// the feed's, decision 0227). No action-bar change — learning a spell does not bar it.
pub(super) fn learned_spell(spell_id: u32, actions: &mut PlayerActions) {
    debug!("net: learned spell {spell_id}");
    if actions.spells.insert(spell_id) {
        actions.dirty = true;
    }
}

/// A rank-up (`SMSG_SUPERCEDED_SPELL`): the new rank replaces the old in the book **and on the action
/// bar** (decision 0237). The server sends no fresh `SMSG_ACTION_BUTTONS` for this (VERIFIED vmangos
/// `Player::learnSpell` — the `supercededOld` path touches only the spell store), so the client owns
/// the bar swap: every spell button pointing at the old rank re-points at the new one.
pub(super) fn superceded_spell(old_spell_id: u32, new_spell_id: u32, actions: &mut PlayerActions) {
    debug!("net: superceded spell {old_spell_id} -> {new_spell_id}");
    actions.spells.remove(&old_spell_id);
    actions.spells.insert(new_spell_id);
    for button in actions.buttons.values_mut() {
        if button.kind == ACTION_KIND_SPELL && button.action == old_spell_id {
            button.action = new_spell_id;
        }
    }
    actions.dirty = true;
}

/// The server's verdict on our cast (`SMSG_CAST_RESULT`).
#[allow(clippy::too_many_arguments)]
pub(super) fn cast_result(
    spell_id: u32,
    success: bool,
    reason: Option<u8>,
    commands: &mut Commands,
    self_guid: &SelfGuid,
    index: &GuidIndex,
    cast_errors: &mut CastErrors,
    casting: &Query<&Casting>,
    cast_events: &mut MessageWriter<CastEvent>,
    cast_bar: &mut CastBarFeed,
    pending: &mut PendingCast,
    queued_melee: &mut QueuedMeleeSpell,
    cooldowns: &mut Cooldowns,
    auto_repeat: &mut AutoRepeatActive,
    net: &crate::net::NetCommands,
    seq: u64,
) {
    debug!("net: cast result — spell {spell_id} success={success} reason={reason:?}");
    if !success {
        // The cast-fail cooldown edges (the client's `HandleCastFailed 0x6e1a00`, wow-re
        // `wave-cooldown.md` + the 2026-07-10 §5): a plain interactive-cast failure clears ONLY
        // the GCD armed at send (`0x6e1d83 → 0x6e1630`) — the spell's own recovery was never
        // started pre-launch (it lands at SPELL_GO / SMSG_SPELL_COOLDOWN, which a failed cast
        // never reaches), so there is nothing else to revert — and a failing auto-repeat spell
        // runs the FULL local cancel (byte-verified `0x6e1cd9`–`0x6e1cea`, wow-re
        // `nocked-ammo-cancel.md`): iff the failing spellId == the cached auto-repeat spell AND
        // reason ≠ 0x17, the handler jumps into `0x6ea080` — the SAME routine the
        // SMSG_CANCEL_AUTO_REPEAT handler runs — clearing the key, the shooting-idle bits, and
        // the nocked ammo. Against vmangos this path is the ONLY live disarm: the server never
        // sends SMSG_CANCEL_AUTO_REPEAT (dead packet class, zero send sites); a
        // deselect/interrupt surfaces solely as this CAST_RESULT failure
        // (`HandleSetSelectionOpcode` → `Spell::cancel` → `SendCastResult(INTERRUPTED)`).
        let now = Instant::now();
        cooldowns.clear_gcd(spell_id, now);
        let self_e = self_guid.0.and_then(|g| index.0.get(&g)).copied();
        if auto_repeat.0 == Some(spell_id) && reason != Some(0x17) {
            crate::creature_anim::cancel_auto_repeat_local(self_e, auto_repeat, commands, net);
        }
        // Does this failure belong to the cast the bar is showing? Keyed to our in-flight
        // `Casting`, like every reap. A pre-start rejection (no `Casting` — LoS, out of range,
        // silenced, or the send-guard's own duplicate that no longer reaches us) never opened a
        // bar; a proc's failure names a different spell. Neither may red-fade the running bar.
        let fails_our_cast =
            self_e.is_some_and(|e| casting.get(e).is_ok_and(|c| c.spell_id == spell_id));
        if *crate::net::CAST_TRACE {
            info!("cast-trace: RECV CAST_RESULT failure — spell {spell_id} reason={reason:?}; fails_bar={fails_our_cast}");
        }
        // Open the in-flight guard regardless (the send-guard means the spurious "duplicate
        // rejected" failure no longer arrives here at all). A queued on-next-swing strike dies
        // the same way — the server's melee-slot interrupt (target death, cancel, replacement:
        // vmangos `Spell::cancel` on the PREPARING slot) lands here as a failure.
        pending.clear_if(spell_id);
        queued_melee.clear_if(spell_id);
        // The red error line rides `CastErrors` independently of the bar — a pre-start failure
        // still shows "out of range"/"line of sight" even though no bar is (or should be) up.
        if let Some(reason) = reason {
            cast_errors.0.push((spell_id, reason));
        }
        // The cast bar's red "Failed" — only the showing cast's own failure turns it red.
        if fails_our_cast {
            cast_bar.0.push(CastBarEdge::Failed);
        }
        // Our own cast died (`SMSG_SPELL_FAILURE` is never sent — decision 0099): end the self
        // avatar's cast state + precast hold, spell-id-keyed like every reap.
        if let Some(e) = self_e {
            if fails_our_cast {
                commands.entity(e).remove::<Casting>();
            }
            cast_events.write(CastEvent {
                entity: e,
                spell_id,
                kind: CastEventKind::Fail,
                seq,
            });
        }
    }
}

/// A unit began a non-triggered cast (`SMSG_SPELL_START`), instants included (`cast_time_ms == 0`)
/// — the precast trigger the phase-2 casting animation loop builds on (decision 0099 phase 1).
#[allow(clippy::too_many_arguments)]
pub(super) fn spell_start(
    caster: u64,
    spell_id: u32,
    cast_flags: u16,
    cast_time_ms: u32,
    target: Option<u64>,
    ammo_display_id: Option<u32>,
    commands: &mut Commands,
    index: &GuidIndex,
    cast_events: &mut MessageWriter<CastEvent>,
    self_guid: &SelfGuid,
    cast_bar: &mut CastBarFeed,
    pending: &mut PendingCast,
    spells: Option<&crate::ui_action::Spells>,
    seq: u64,
) {
    // The precast trigger (decision 0099 phase 1): a nonzero cast time seeds the
    // `Casting` state seam phase 2's precast loop reads; an instant (timer 0) gets no
    // component — its `SpellGo` follows immediately with nothing to interrupt. No
    // animation consumer yet.
    debug!(
        "net: spell start {spell_id} by {caster:#x} ({cast_time_ms}ms, flags \
         {cast_flags:#x}, target {target:?}, ammo {ammo_display_id:?})"
    );
    // The nocked-ammo refresh (the client's `0x60ba30` @ `0x6e78b6`, gated `SpellRec+0x20&0x20 OR
    // +0x18&0x2` at `0x6e78a1`, on the packet-resolved caster BEFORE the self/other split — any
    // unit; wow-re `nocked-ammo-cancel.md`): a ranged spell's START either affirms the wire's
    // ammo display id or, with the flag clear / id 0, detaches. The model persists through
    // Load/Hold and the fire clip — `SPELL_GO` never touches it.
    if spells
        .and_then(|s| s.catalog.get(spell_id))
        .is_some_and(|d| d.ranged_attack())
    {
        if let Some(&e) = index.0.get(&caster) {
            match ammo_display_id.filter(|id| *id != 0) {
                Some(display_id) => {
                    commands
                        .entity(e)
                        .insert(crate::creature_anim::NockedAmmo { display_id });
                }
                None => {
                    commands
                        .entity(e)
                        .remove::<crate::creature_anim::NockedAmmo>();
                }
            }
        }
    }
    // The cast bar's SECOND gate (the client's `0x6e7700`: fire SPELLCAST_START only on
    // `cast_time > 0 && !(SpellRec+0x18 & 2)` — byte-verified, closes decision 0256's "open —
    // the wire feed" item 1): a ranged-slot spell never opens a bar, whatever cast time the
    // server sent. Concretely: vmangos pads every non-auto-repeat ranged spell's cast by a flat
    // +500 ms (`SpellEntry::GetCastTime`), so Throw arrives as a real 500 ms cast — the ref
    // shows no bar for it, and now neither do we (decision 0376). An unknown spell (no catalog,
    // no row) keeps the bar: only a *known ranged* row suppresses.
    let ranged_slot = spells
        .and_then(|s| s.catalog.get(spell_id))
        .is_some_and(|d| d.ranged_slot());
    if *crate::net::CAST_TRACE && self_guid.0 == Some(caster) {
        info!(
            "cast-trace: RECV SPELL_START — spell {spell_id} cast_time={cast_time_ms}ms \
             (bar {})",
            if ranged_slot {
                "suppressed: ranged"
            } else {
                "opens"
            }
        );
    }
    // Our own timed cast opens the cast bar (an instant shows no bar — decision 0137).
    if self_guid.0 == Some(caster) && cast_time_ms > 0 {
        if !ranged_slot {
            cast_bar.0.push(CastBarEdge::Start {
                spell_id,
                cast_time_ms,
            });
        }
        // The server named the real cast time — tighten the in-flight guard's safety deadline
        // from the send-time provisional to it (the guard is cleared for real by the GO/fail).
        // Ranged too: the in-flight guard is cast-tracking, not bar UI.
        pending.refine(cast_time_ms, Instant::now());
    }
    if let Some(&e) = index.0.get(&caster) {
        if cast_time_ms > 0 {
            commands.entity(e).insert(Casting {
                spell_id,
                until: Some(Instant::now() + Duration::from_millis(u64::from(cast_time_ms))),
            });
        }
        // The anim layer's precast edge (decision 0107) — instants included: their
        // GO follows at once and reaps the (subliminal) hold, like the client's
        // stage-4 persist/reap pair.
        cast_events.write(CastEvent {
            entity: e,
            spell_id,
            kind: CastEventKind::Start,
            seq,
        });
    }
}

/// The cast launched (`SMSG_SPELL_GO`): hit/miss lists + (for a ranged spell) the ammo display id
/// for the projectile visual. The server schedules the *damage* itself off `Spell.dbc` Speed —
/// nothing about missile travel rides this packet; the client (and we) rebuild the flight
/// visually from the same Speed column (decision 0099 phase 4: the target lists go out as
/// [`SpellGoTargets`] for the router's instant-impact/missile branch).
#[allow(clippy::too_many_arguments)] // one dispatch arm's full input set
pub(super) fn spell_go(
    caster: u64,
    spell_id: u32,
    cast_flags: u16,
    hits: Vec<u64>,
    misses: Vec<(u64, u8)>,
    target: Option<u64>,
    go_target: Option<u64>,
    ammo_display_id: Option<u32>,
    item_caster: Option<u64>,
    commands: &mut Commands,
    index: &GuidIndex,
    casting: &Query<&Casting>,
    cast_events: &mut MessageWriter<CastEvent>,
    go_targets: &mut MessageWriter<SpellGoTargets>,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
    cast_bar: &mut CastBarFeed,
    pending: &mut PendingCast,
    queued_melee: &mut QueuedMeleeSpell,
    text: &mut MessageWriter<crate::combat_text::CombatTextSpawn>,
    go_lid: &mut MessageWriter<crate::go_anim::GoLidOpen>,
    // The cooldown store + what its start laws read (grouped: one arm-body concern).
    cooldown_ctx: (
        &mut Cooldowns,
        Option<&Spells>,
        &mut crate::items::Items,
        &crate::net::NetCommands,
    ),
    seq: u64,
) {
    debug!(
        "net: spell go {spell_id} by {caster:#x} ({} hit(s), {} miss(es), flags \
         {cast_flags:#x}, target {target:?}, go_target {go_target:?}, ammo {ammo_display_id:?})",
        hits.len(),
        misses.len()
    );
    // A cast that names a GameObject (an open-lock cast on a chest / locked door) hands off to the GO
    // animation driver, which gates on the open-lock effect and opens the lid on the cast going off
    // (decision 0250). Independent of the caster being streamed to us — an observed open still animates.
    if let Some(go_guid) = go_target {
        go_lid.write(crate::go_anim::GoLidOpen { go_guid, spell_id });
    }
    // Our own launch completes the cast bar (a shown bar fills green and fades; auto-repeat
    // shots — GO with no bar showing — no-op in the reference Lua) and opens the in-flight guard
    // (spell-id-keyed, so a triggered proc's GO mid-cast doesn't unblock the running cast early).
    if self_guid.0 == Some(caster) {
        // Only the cast the bar is actually showing may complete it. A triggered proc that lands
        // mid-cast — Frost Armor's Chilled (6136), a weapon proc — is *our own* cast, so its
        // `SMSG_SPELL_GO` arrives here too; pushing an unconditional STOP fired a spurious
        // SPELLCAST_STOP that finished the running bar early (the observed "the bar vanishes when a
        // mob hits me, the spell fires a moment later"). Key it to our in-flight `Casting`, exactly
        // like the spell-id reap below.
        let completes_our_cast = index
            .0
            .get(&caster)
            .is_some_and(|&e| casting.get(e).is_ok_and(|c| c.spell_id == spell_id));
        if *crate::net::CAST_TRACE {
            info!("cast-trace: RECV SPELL_GO — spell {spell_id} (self); completes_bar={completes_our_cast}");
        }
        if completes_our_cast {
            cast_bar.0.push(CastBarEdge::Stop);
        }
        pending.clear_if(spell_id);
        // The queued on-next-swing strike fired on this swing — the queue (and its checked
        // ring) opens exactly here, like the ref's inflight finish on the matching GO.
        queued_melee.clear_if(spell_id);

        // Our own launch starts the cast's cooldown locally, at the GO — byte-VERIFIED (the
        // 2026-07-10 wow-re follow-up, `wave-handlers.md` ADDENDUM): `HandleSpellGo 0x6e7a70`'s
        // self-insert tail forks on the packet guid pair — itemCaster == caster ⇒ the NO-ITEM
        // spell leg (`0x6e8498`: SpellRec RecoveryTime/Category/CategoryRecoveryTime, onHold from
        // Attributes bit 25, start = the GO receive-time); an item cast takes the item leg
        // (`0x6e8566`, per-slot values with SpellRec fallbacks; a non-resident row pends via
        // `0x6e8660` → `0x6e8830`). `SMSG_SPELL_COOLDOWN` is the server OVERRIDE path, not the
        // normal source — this insert is how a Charge sweep appears on vmangos, which sends no
        // cooldown packet for a plain cast. A pre-launch failure never reaches here, so nothing
        // needs reverting.
        let (cooldowns, spells, items, net_commands) = cooldown_ctx;
        let now = Instant::now();
        let display = spells.and_then(|s| s.catalog.get(spell_id));
        // The ranged-shot pad (the category scaler `0x6e2b60`, byte-verified — wow-re
        // `ranged-cooldown-sweep.md`, decision 0378): a ranged-slot cast folds our live
        // `UNIT_FIELD_RANGEDATTACKTIME` (haste-scaled, server-written) into the category
        // recovery — the Throw/wand-Shoot button sweep, no server packet involved.
        let ranged_ms = display
            .filter(|d| d.ranged_speed_cooldown())
            .and_then(|_| {
                let e = index.0.get(&caster)?;
                stores.get(*e).ok()?.0.unit_ranged_attack_time()
            })
            .unwrap_or(0);
        match item_caster.and_then(|g| items.object(g)?.object_entry()) {
            Some(entry) => {
                let use_spell = items
                    .template(entry, 0, net_commands)
                    .and_then(|t| t.use_spell)
                    .filter(|u| u.spell_id == spell_id);
                match use_spell {
                    Some(u) => cooldowns.start_item(entry, &u, display, now),
                    // The template hasn't streamed (or names a different spell): fall back to
                    // the spell-keyed record so the sweep still runs.
                    None => {
                        if let Some(d) = display {
                            cooldowns.start_spell(spell_id, d, ranged_ms, now);
                        }
                    }
                }
            }
            None => {
                if let Some(d) = display {
                    cooldowns.start_spell(spell_id, d, ranged_ms, now);
                }
            }
        }
        if crate::dbg_trace::enabled() {
            if let Some(d) = display {
                crate::dbg_trace::line(
                    "cd",
                    &format!(
                        "arm spell={spell_id} rec={}ms cat={}:{}ms (GO self-insert)",
                        d.recovery_ms, d.category, d.category_recovery_ms
                    ),
                );
            }
        }
    }
    // The miss list's floating words (0137 phase 2, the `0x6e7a70` handler): one outcome word
    // over each missed target — except REFLECT, which re-anchors to the caster (`0x6e7e51`).
    // Gate A applies to whichever unit the word lands over. Source-classified first (the color
    // law's K, inside every emitter twin): another caster's misses draw nothing. The words keep
    // the row-default white (this site's record push is unpinned — flagged open).
    if !misses.is_empty()
        && super::combat_log::classify_source(caster, index, self_guid, stores).is_some()
    {
        for &(guid, code) in &misses {
            let anchor_guid = if code == 11 { caster } else { guid };
            if self_guid.0 == Some(anchor_guid) {
                continue;
            }
            if let (Some(&anchor), Some((word, category))) = (
                index.0.get(&anchor_guid),
                crate::combat_text::miss_word(code),
            ) {
                text.write(crate::combat_text::CombatTextSpawn {
                    anchor,
                    text: word.to_string(),
                    category,
                    color: None,
                });
            }
        }
    }
    // Keyed by spell id, like the client's reap `0x614150(spellId, 0)` (wow-re
    // `spell-visual-lifecycle.md`): a triggered proc's GO landing mid-cast must not
    // clear a *different* spell's precast state.
    if let Some(&e) = index.0.get(&caster) {
        if casting.get(e).is_ok_and(|c| c.spell_id == spell_id) {
            commands.entity(e).remove::<Casting>();
        }
        cast_events.write(CastEvent {
            entity: e,
            spell_id,
            kind: CastEventKind::Go,
            seq,
        });
        // The impact side: targets not streamed to us (out of range) just drop out of the
        // lists — their impacts are invisible anyway. The miss code rides along: the missile's
        // arrival plays the victim's dodge/block clip off it (the deflect *flight* visual stays
        // a later refinement).
        let hits: Vec<Entity> = hits
            .iter()
            .filter_map(|g| index.0.get(g).copied())
            .collect();
        let misses: Vec<(Entity, u8)> = misses
            .iter()
            .filter_map(|&(g, code)| index.0.get(&g).map(|&e| (e, code)))
            .collect();
        if !hits.is_empty() || !misses.is_empty() {
            go_targets.write(SpellGoTargets {
                caster: e,
                spell_id,
                hits,
                misses,
                ammo_display_id,
                seq,
            });
        }
    }
}

/// An observed cast was interrupted/cancelled (`SMSG_SPELL_FAILED_OTHER`) — ends the caster's
/// `Casting` state seam the same as [`spell_go`].
#[allow(clippy::too_many_arguments)] // one dispatch arm's full input set
pub(super) fn spell_failed_other(
    caster: u64,
    spell_id: u32,
    commands: &mut Commands,
    index: &GuidIndex,
    casting: &Query<&Casting>,
    cast_events: &mut MessageWriter<CastEvent>,
    self_guid: &SelfGuid,
    cast_bar: &mut CastBarFeed,
    pending: &mut PendingCast,
    queued_melee: &mut QueuedMeleeSpell,
    seq: u64,
) {
    debug!("net: spell failed (other) {spell_id} by {caster:#x}");
    // Our own in-flight cast was interrupted — the bar turns red "Interrupted" (decision 0137),
    // but only for the cast the bar is actually showing (keyed to `Casting`, like the reap): a
    // proc's own interrupt must not red-fade a different running bar. The guard opens the same way.
    if self_guid.0 == Some(caster) {
        let interrupts_our_cast = index
            .0
            .get(&caster)
            .is_some_and(|&e| casting.get(e).is_ok_and(|c| c.spell_id == spell_id));
        if *crate::net::CAST_TRACE {
            info!("cast-trace: RECV SPELL_FAILED_OTHER — spell {spell_id} (self); interrupts_bar={interrupts_our_cast}");
        }
        if interrupts_our_cast {
            cast_bar.0.push(CastBarEdge::Interrupted);
        }
        pending.clear_if(spell_id);
        // The melee-slot interrupt's other half (it arrives beside the failing CAST_RESULT).
        queued_melee.clear_if(spell_id);
    }
    // Spell-id-keyed like the GO reap (the 0x2A6 handler's `0x614150(spellId, 0)`).
    if let Some(&e) = index.0.get(&caster) {
        if casting.get(e).is_ok_and(|c| c.spell_id == spell_id) {
            commands.entity(e).remove::<Casting>();
        }
        cast_events.write(CastEvent {
            entity: e,
            spell_id,
            kind: CastEventKind::Fail,
            seq,
        });
    }
}

/// `SMSG_SPELL_DELAYED` — pushback: our own cast took damage and the server extended its timer by
/// `delay_ms` (vmangos `Spell::Delayed`; a normal hit never interrupts a cast, it pushes it back).
/// The cast bar slides its window out by the same (`SPELLCAST_DELAYED`, the reference Lua's
/// `startTime`/`maxValue` shift — the spark jumps back and the bar keeps running), so a hit no
/// longer lets the bar finish early while the real cast runs on (decision 0256). Self-only on the
/// wire, but the caster guid is on the packet — gate on it like every other own-cast edge.
pub(super) fn spell_delayed(
    caster: u64,
    delay_ms: u32,
    self_guid: &SelfGuid,
    cast_bar: &mut CastBarFeed,
    pending: &mut PendingCast,
) {
    debug!("net: spell delayed by {caster:#x} (+{delay_ms}ms pushback)");
    if self_guid.0 == Some(caster) {
        if *crate::net::CAST_TRACE {
            info!("cast-trace: RECV SPELL_DELAYED — +{delay_ms}ms pushback (bar extends, does NOT vanish)");
        }
        cast_bar.0.push(CastBarEdge::Delayed { delay_ms });
        // Keep the in-flight guard (decision 0256 / the spam fix) holding past the stretched end.
        pending.delay(delay_ms, Instant::now());
    }
}

/// `SMSG_CANCEL_AUTO_REPEAT` — the client's handler (`0x6e99d0` → `0x6ea080`, wow-re
/// `wave-handlers.md`) clears the autorepeat key `0xceac30`, which is exactly what the action
/// bar's flash/checked state reads — so the button's auto-repeat highlight goes out
/// (`STOP_AUTOREPEAT_SPELL` fires off this edge in the UI feed). The shots themselves were
/// always wire-paced (decision 0099 phase 5: every shot is its own `SPELL_GO`), and nothing
/// stows (sheath-policy's "nothing sheathes on combat-end").
///
/// **INTERIM (decision 0400 §2's dispatch, Q5):** the cancel also disarms the standing
/// Load/Hold idle ([`crate::creature_anim::AutoRepeatArmed`] off) — the director's report: on
/// the reference the shooting visibly STOPS when the server cancels (target too close), while
/// our sticky arm kept the nock idle looping forever. Whether the real handler clears the
/// `[+0xd58] & 0x200` idle bit (0131 recorded "no clearing writer") or the hold-pose layer
/// merely makes the ref look still is the dispatched question; the verdict corrects this
/// if the mechanism differs.
pub(super) fn cancel_auto_repeat(
    auto_repeat: &mut AutoRepeatActive,
    self_guid: &SelfGuid,
    index: &GuidIndex,
    commands: &mut Commands,
    net: &crate::net::NetCommands,
) {
    debug!("net: cancel auto-repeat");
    // The packet thunk `0x6e99d0` funnels into the same cancel `0x6ea080` as every local
    // trigger — including its CMSG ack (unconditional inside the routine). vmangos never sends
    // this SMSG, so against it the arm is dormant; kept for wire fidelity.
    let self_e = self_guid.0.and_then(|g| index.0.get(&g)).copied();
    crate::creature_anim::cancel_auto_repeat_local(self_e, auto_repeat, commands, net);
}

/// `SMSG_SPELL_COOLDOWN` (`0x6e9460`) — server-pushed cooldowns for the player (school lockouts;
/// the pet list has no benilla consumer yet, so a pet-guid packet for a pet we don't model is
/// dropped like the client drops unknown guids).
pub(super) fn spell_cooldowns(
    caster: u64,
    pairs: Vec<(u32, u32)>,
    self_guid: &SelfGuid,
    spells: Option<&Spells>,
    cooldowns: &mut Cooldowns,
) {
    debug!(
        "net: spell cooldowns for {caster:#x} — {} pair(s)",
        pairs.len()
    );
    if self_guid.0 != Some(caster) {
        return;
    }
    let now = Instant::now();
    for (spell_id, cooldown_ms) in pairs {
        let display = spells.and_then(|s| s.catalog.get(spell_id));
        cooldowns.apply_wire_cooldown(spell_id, cooldown_ms, display, now);
    }
}

/// `SMSG_ITEM_COOLDOWN` (`0x6e95d0`) — the fixed 30 s use cooldown, keyed on the item instance's
/// template entry (the client resolves the guid to its item record the same way).
pub(super) fn item_cooldown(
    item_guid: u64,
    spell_id: u32,
    items: &crate::items::Items,
    cooldowns: &mut Cooldowns,
) {
    debug!("net: item cooldown — item {item_guid:#x} spell {spell_id}");
    if let Some(entry) = items.object(item_guid).and_then(|o| o.object_entry()) {
        cooldowns.apply_wire_item_cooldown(entry, spell_id, Instant::now());
    }
}

/// `SMSG_COOLDOWN_EVENT` (`0x6e9670` → `0x6e3050(force=0)`) — start an on-hold cooldown's parked
/// timers now (Stealth ends, Feign Death drops).
pub(super) fn cooldown_event(
    spell_id: u32,
    caster: u64,
    self_guid: &SelfGuid,
    cooldowns: &mut Cooldowns,
) {
    debug!("net: cooldown event — spell {spell_id} on {caster:#x}");
    if self_guid.0 == Some(caster) {
        cooldowns.cooldown_event(spell_id, Instant::now());
    }
}

/// `SMSG_CLEAR_COOLDOWN` (`0x6e9670` → `0x6e3050(force=1)`) — remove the spell's record outright.
pub(super) fn clear_cooldown(
    spell_id: u32,
    caster: u64,
    self_guid: &SelfGuid,
    cooldowns: &mut Cooldowns,
) {
    debug!("net: clear cooldown — spell {spell_id} on {caster:#x}");
    if self_guid.0 == Some(caster) {
        cooldowns.clear_spell(spell_id);
    }
}

/// `SMSG_COOLDOWN_CHEAT` (`0x6e9730` → `0x6e9700`) — the GM reset wipes the whole list.
pub(super) fn cooldown_cheat(caster: u64, self_guid: &SelfGuid, cooldowns: &mut Cooldowns) {
    debug!("net: cooldown cheat (wipe) for {caster:#x}");
    if self_guid.0 == Some(caster) {
        cooldowns.wipe();
    }
}

/// `MSG_CHANNEL_START` — self-only on the wire (no guid), so it goes straight to the cast bar; the
/// channel *animation* state rides the unit-field pair instead (decision 0137).
pub(super) fn channel_start(
    spell_id: u32,
    duration_ms: u32,
    channel: &mut ActiveChannel,
    feed: &mut CastBarFeed,
) {
    channel.start(spell_id, duration_ms, Instant::now());
    feed.0.push(CastBarEdge::ChannelStart {
        spell_id,
        duration_ms,
    });
}

/// `MSG_CHANNEL_UPDATE` — the running channel's remaining time (`0` is its stop edge).
pub(super) fn channel_update(
    remaining_ms: u32,
    channel: &mut ActiveChannel,
    feed: &mut CastBarFeed,
) {
    channel.update(remaining_ms, Instant::now());
    feed.0.push(CastBarEdge::ChannelUpdate { remaining_ms });
}

/// `SMSG_UPDATE_AURA_DURATION` — one of our own auras' remaining time (decisions 0255/0257), keyed
/// by raw slot and stamped with the receive time. The `ui_aura` feed joins it to the aura in that
/// slot by arrival order; it arrives *before* the descriptor delta that names the slot.
pub(super) fn aura_duration(
    slot: u8,
    remaining_ms: u32,
    durations: &mut AuraDurations,
    now_secs: f64,
) {
    durations.set(slot, remaining_ms, now_secs);
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::ACTION_KIND_MACRO;

    #[test]
    fn learned_spell_adds_to_the_book_once() {
        let mut actions = PlayerActions::default();
        learned_spell(6603, &mut actions);
        assert!(actions.spells.contains(&6603));
        assert!(actions.dirty, "a new spell dirties the feed");

        actions.dirty = false;
        learned_spell(6603, &mut actions);
        assert!(
            !actions.dirty,
            "re-learning a known spell is a no-op (insert returns false)"
        );
    }

    #[test]
    fn superceded_spell_swaps_the_book_and_only_matching_spell_buttons() {
        let mut actions = PlayerActions::default();
        actions.spells.insert(78); // rank 1, known
                                   // Rank 1 sits on the bar in slot 0; slot 1 is a *macro* whose id
                                   // collides with the old rank and must NOT be rewritten.
        actions.buttons.insert(
            0,
            ActionButton {
                slot: 0,
                action: 78,
                kind: ACTION_KIND_SPELL,
            },
        );
        actions.buttons.insert(
            1,
            ActionButton {
                slot: 1,
                action: 78,
                kind: ACTION_KIND_MACRO,
            },
        );

        superceded_spell(78, 284, &mut actions);

        assert!(
            !actions.spells.contains(&78),
            "the old rank leaves the book"
        );
        assert!(
            actions.spells.contains(&284),
            "the new rank enters the book"
        );
        assert_eq!(actions.buttons[&0].action, 284, "the spell button rank-ups");
        assert_eq!(
            actions.buttons[&1].action, 78,
            "the id-colliding macro is untouched"
        );
        assert!(actions.dirty);
    }

    /// The regression the live `WOW_CAST_TRACE` caught: a triggered proc (Frost Armor's Chilled
    /// 6136) that lands mid-cast is *our own* cast, so its `SMSG_SPELL_GO` reaches `spell_go` with
    /// `caster == self`. An unconditional STOP finished the running Fireball bar early (the "bar
    /// vanishes when a mob hits me, the spell fires a moment later" report). The bar-ending edge
    /// must be keyed to our in-flight `Casting`, like the reap beside it.
    #[test]
    fn a_proc_go_mid_cast_does_not_finish_the_running_bar() {
        use crate::combat_text::CombatTextSpawn;
        use crate::creature_anim::Casting;
        use crate::go_anim::GoLidOpen;
        use crate::net::{Guid, SelfPlayer};
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.add_message::<CastEvent>()
            .add_message::<SpellGoTargets>()
            .add_message::<CombatTextSpawn>()
            .add_message::<GoLidOpen>()
            .init_resource::<GuidIndex>()
            .init_resource::<SelfGuid>()
            .init_resource::<CastBarFeed>()
            .init_resource::<PendingCast>()
            .init_resource::<QueuedMeleeSpell>()
            .init_resource::<Cooldowns>()
            .init_resource::<crate::items::Items>();

        // The self player mid-cast on Fireball (133): the bar is up, `Casting{133}` marks it.
        let self_e = app
            .world_mut()
            .spawn((
                Guid(10),
                SelfPlayer,
                Casting {
                    spell_id: 133,
                    until: None,
                },
                ObjectStore::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(10, self_e);
        app.world_mut().resource_mut::<SelfGuid>().0 = Some(10);

        // One `spell_go` call, parameterized by the completing spell id — the whole point is that
        // 6136 (a proc) and 133 (the bar's cast) take different branches.
        let fire_go = |app: &mut App, go_spell: u32| {
            let (tx, _rx) = crossbeam_channel::unbounded();
            app.world_mut()
                .run_system_once(
                    move |mut commands: Commands,
                          index: Res<GuidIndex>,
                          casting: Query<&Casting>,
                          mut cast_events: MessageWriter<CastEvent>,
                          mut go_targets: MessageWriter<SpellGoTargets>,
                          self_guid: Res<SelfGuid>,
                          stores: Query<&mut ObjectStore>,
                          mut cast_bar: ResMut<CastBarFeed>,
                          mut pending: ResMut<PendingCast>,
                          mut queued_melee: ResMut<QueuedMeleeSpell>,
                          mut text: MessageWriter<CombatTextSpawn>,
                          mut go_lid: MessageWriter<GoLidOpen>,
                          mut cooldowns: ResMut<Cooldowns>,
                          mut items: ResMut<crate::items::Items>| {
                        let net_commands = crate::net::NetCommands(tx.clone());
                        spell_go(
                            10,
                            go_spell,
                            0,
                            vec![],
                            vec![],
                            None,
                            None,
                            None,
                            None,
                            &mut commands,
                            &index,
                            &casting,
                            &mut cast_events,
                            &mut go_targets,
                            &self_guid,
                            &stores,
                            &mut cast_bar,
                            &mut pending,
                            &mut queued_melee,
                            &mut text,
                            &mut go_lid,
                            (&mut cooldowns, None, &mut items, &net_commands),
                            1,
                        );
                    },
                )
                .unwrap();
        };

        // The proc's self-GO must NOT touch the bar.
        fire_go(&mut app, 6136);
        assert!(
            app.world().resource::<CastBarFeed>().0.is_empty(),
            "a proc's self-GO (6136) mid-cast must not push a bar edge"
        );

        // The cast the bar is showing (133) finishes it — one STOP.
        fire_go(&mut app, 133);
        let feed = &app.world().resource::<CastBarFeed>().0;
        assert_eq!(
            feed.len(),
            1,
            "the in-flight cast's own GO finishes the bar"
        );
        assert!(matches!(feed[0], CastBarEdge::Stop), "…with a STOP");
    }

    /// The director's stuck-shooting-idle report: click off the target during Auto Shot and the
    /// Load/Hold stance never drops. vmangos never sends `SMSG_CANCEL_AUTO_REPEAT` (dead packet
    /// class) — a deselect surfaces ONLY as a `SMSG_CAST_RESULT` failure for the cached
    /// auto-repeat spell, and the client's `6e1cd9` jump into `0x6ea080` makes that the full
    /// local cancel: the key AND the shooting idle both drop.
    #[test]
    fn a_cast_result_fail_of_the_cached_auto_repeat_spell_disarms_the_shooting_idle() {
        use crate::creature_anim::AutoRepeatArmed;
        use crate::net::{Guid, SelfPlayer};
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.add_message::<CastEvent>()
            .init_resource::<GuidIndex>()
            .init_resource::<SelfGuid>()
            .init_resource::<CastErrors>()
            .init_resource::<CastBarFeed>()
            .init_resource::<PendingCast>()
            .init_resource::<QueuedMeleeSpell>()
            .init_resource::<Cooldowns>()
            .init_resource::<AutoRepeatActive>();

        let self_e = app
            .world_mut()
            .spawn((
                Guid(10),
                SelfPlayer,
                AutoRepeatArmed,
                crate::creature_anim::NockedAmmo { display_id: 5996 },
            ))
            .id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(10, self_e);
        app.world_mut().resource_mut::<SelfGuid>().0 = Some(10);
        app.world_mut().resource_mut::<AutoRepeatActive>().0 = Some(75);

        let (tx, rx) = crossbeam_channel::unbounded();
        let fire_fail = |app: &mut App, reason: u8| {
            let tx = tx.clone();
            app.world_mut()
                .run_system_once(
                    move |mut commands: Commands,
                          self_guid: Res<SelfGuid>,
                          index: Res<GuidIndex>,
                          mut cast_errors: ResMut<CastErrors>,
                          casting: Query<&Casting>,
                          mut cast_events: MessageWriter<CastEvent>,
                          mut cast_bar: ResMut<CastBarFeed>,
                          mut pending: ResMut<PendingCast>,
                          mut queued_melee: ResMut<QueuedMeleeSpell>,
                          mut cooldowns: ResMut<Cooldowns>,
                          mut auto_repeat: ResMut<AutoRepeatActive>| {
                        let net = crate::net::NetCommands(tx.clone());
                        cast_result(
                            75,
                            false,
                            Some(reason),
                            &mut commands,
                            &self_guid,
                            &index,
                            &mut cast_errors,
                            &casting,
                            &mut cast_events,
                            &mut cast_bar,
                            &mut pending,
                            &mut queued_melee,
                            &mut cooldowns,
                            &mut auto_repeat,
                            &net,
                            1,
                        );
                    },
                )
                .unwrap();
        };

        // Reason 0x17 is the client's one skip (`6e1ce1 cmp cl,0x17; je`): armed stays armed.
        fire_fail(&mut app, 0x17);
        assert_eq!(app.world().resource::<AutoRepeatActive>().0, Some(75));
        assert!(app
            .world()
            .entity(self_e)
            .get::<AutoRepeatArmed>()
            .is_some());

        // SPELL_FAILED_INTERRUPTED — the deselect's wire face — runs the full cancel.
        fire_fail(&mut app, 0x1e);
        assert_eq!(
            app.world().resource::<AutoRepeatActive>().0,
            None,
            "the autorepeat key drops"
        );
        assert!(
            app.world()
                .entity(self_e)
                .get::<AutoRepeatArmed>()
                .is_none(),
            "the shooting-idle gate drops with it — the stuck Load/Hold stance"
        );
        assert!(
            app.world()
                .entity(self_e)
                .get::<crate::creature_anim::NockedAmmo>()
                .is_none(),
            "the cancel un-nocks (the client's 0x6ea140 -> 0x60f530)"
        );
        let sent: Vec<_> = rx.try_iter().collect();
        assert!(
            sent.iter()
                .any(|c| matches!(c, crate::net::ClientCommand::CancelAutoRepeat)),
            "the cancel acks the server (CMSG_CANCEL_AUTO_REPEAT_SPELL, the 0x6ea0c6 send)"
        );
    }
}
