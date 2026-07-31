//! **The one cast-send path** — every spell benilla casts leaves through [`send_spell_cast`].
//!
//! The action button is only one of its callers; the spellbook (decision 0216 §8), the stance bar,
//! the trade-skill window and the craft window all funnel here too. That is the point: the client's
//! `TryCast 0x6e4b60` → commit `0x6e54f0` is *one* function with a long ladder of local gates and a
//! post-send tail, and duplicating any part of it per caller is how the two paths drift. The ladder
//! below is that function's order, gate for gate — profession intercept, auto-repeat toggle,
//! cooldown, GCD, in-flight guard, mounted, reagents/totems, target binding, range — followed by
//! the commit's own tail (ranged stance, auto-repeat arm, the send, the auto-attack start, the GCD
//! arm).
//!
//! A refusal here is **local and pre-commit**, exactly like the reference's: no packet, no GCD, no
//! pending arm, no autorepeat key — just the red error line's reason code.

use std::time::Instant;

use bevy::prelude::*;

use crate::items::Items;
use crate::net::{ClientCommand, NetCommands, SelfPlayer};

use super::{cast_target, reagent_totem_refusal, state, AutoRepeatActive, CastErrors, Spells};

/// Send one spell cast at the current selection — the client's local cast-send follow-through
/// (the client's `0x6e54f0` tail): a ranged-attribute spell arms the **ranged stance** now
/// (`0x6e5930`'s `SetSheatheState(2,1,1)` — the echo START re-requests it, idempotent), an
/// auto-repeat spell sets the sticky armed state (`0x6e593b`'s `|= 0x200`, the standing Load/Hold
/// idle's gate — decision 0099 phase 5), and the resolved `CMSG_CAST_SPELL` goes out. Shared by
/// [`super::drain::drain_action_uses`] (a SPELL-kind action button) and
/// `ui_spellbook::drain_spell_casts` (a spellbook cast, decision 0216 §8) — ONE cast-send path, so
/// the follow-through can't drift between the two spell sources (the root-cause rule: never
/// duplicate a send path).
///
/// The `pending` guard is the client's optimistic in-flight refusal (wow-re `wave-cast.md`
/// `TryCast` IsCasting gate; see [`crate::ui_cast::PendingCast`]): a normal cast is dropped at the
/// source while one is already in flight, so mashing a key can no longer fire a duplicate
/// `CMSG_CAST_SPELL` the server bounces back as a spurious cast-bar cancel. Ranged/auto-repeat
/// shots keep their own lifecycle — they never arm the guard and are never blocked by it.
#[allow(clippy::too_many_arguments)] // every input the follow-through + the send itself need
pub(crate) fn send_spell_cast(
    spell_id: u32,
    ctx: &cast_target::CastContext,
    commands: &NetCommands,
    self_player: &Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    spells: Option<&Spells>,
    items: &Items,
    sheath: &mut MessageWriter<crate::creature_anim::SheathRequest>,
    ecs: &mut Commands,
    pending: &mut crate::ui_cast::PendingCast,
    queued_melee: &mut crate::ui_cast::QueuedMeleeSpell,
    cooldowns: &mut crate::cooldowns::Cooldowns,
    cast_errors: &mut CastErrors,
    auto_repeat: &mut AutoRepeatActive,
    trade_skill_opens: &mut crate::ui_tradeskill::TradeSkillOpens,
    ground: &mut super::targeting::SpellTargeting,
) {
    let now = Instant::now();
    let def = spells.and_then(|s| s.catalog.get(spell_id));
    // The profession-window intercept (decision 0437): `Spell_C::TryCast 0x6e4b60`'s own first
    // special branch (wow-re `wave-cast.md`, VERIFIED) — an `Effect[0] == SPELL_EFFECT_TRADE_SKILL`
    // cast NEVER reaches the wire; the crafting book opens client-side instead. Before the
    // cooldown ladder, exactly where the client dispatches it (`6e4bce`, ahead of every gate).
    if def.is_some_and(|d| d.effect_1 == benilla_formats::SPELL_EFFECT_TRADE_SKILL) {
        debug!("ui_action: cast {spell_id} is a profession opener — the crafting book opens, no packet");
        trade_skill_opens.0.push(spell_id);
        return;
    }
    // The button re-press toggle (`0x4e60da`, wow-re `nocked-ammo-cancel.md` §Q-B-2):
    // re-invoking the spell that IS the running auto-repeat cancels it instead of re-casting —
    // the classic press-again-to-stop. Checked before the cooldown ladder, like the client's
    // action-button handler (which never reaches TryCast for the toggle-off).
    if def.is_some_and(|d| d.auto_repeat()) && auto_repeat.0 == Some(spell_id) {
        debug!("ui_action: cast {spell_id} re-pressed — auto-repeat toggles off");
        let self_e = self_player.single().ok().map(|(e, _)| e);
        crate::creature_anim::cancel_auto_repeat_local(self_e, auto_repeat, ecs, commands);
        return;
    }
    // TryCast's IsTargeting leg (`6e4d62`, decision 0792): a NEW cast pressed while the
    // targeting cursor is up aborts the targeting first — AbortCast in targeting mode clears
    // the word, no packet — and the press proceeds down the ladder. (The SAME spell's re-press
    // on the action bar never reaches here: UseAction's toggle-cancel returns at the drain.)
    if ground.active() {
        debug!("ui_action: cast {spell_id} supersedes the targeting cursor");
        ground.clear();
    }
    // The local not-ready refusal (the client's `IsSpellOnCooldown 0x6e1690` gate in the cast
    // path): a spell/category cooldown refuses at the source with the client's own reason 0x3c
    // ("Spell is not ready yet.") — never sent, exactly like the real client. The GCD is
    // faithfully NOT part of this test (0x6e1690 skips the GCD fields).
    if cooldowns.is_on_cooldown(spell_id, def, now) {
        debug!("ui_action: cast {spell_id} refused locally — on cooldown");
        cast_errors.0.push((spell_id, 0x3c));
        return;
    }
    // The GCD leg of the local not-ready ladder ([`Cooldowns::gcd_locked`], decision 0379): a
    // GCD-carrying spell pressed while its startRecoveryCategory still runs refuses at the
    // source — same reason 0x3c, NEVER sent. Sending would draw the server's NOT_READY fail
    // (vmangos enforces the GCD), whose faithful revert clears the RUNNING GCD — the spam-press
    // vanished-pie bug. GCD-free presses (Heroic Strike's queue, Attack, Shoot) pass.
    if def.is_some_and(|d| cooldowns.gcd_locked(d, now)) {
        debug!("ui_action: cast {spell_id} refused locally — the GCD is running");
        cast_errors.0.push((spell_id, 0x3c));
        return;
    }
    // The cast classes at this seam. A ranged/auto-repeat shot (Auto Shot, wand Shoot, Throw) is
    // not a cast-bar cast — it runs the ranged-stance / `AutoRepeatArmed` path, outside the
    // in-flight guard. An on-next-swing spell (`Attributes & 0x404` — Heroic Strike, Cleave)
    // queues on the server's melee slot: it arms [`crate::ui_cast::QueuedMeleeSpell`], never the
    // in-flight guard, so a queued strike cannot block the next cast (the ref's `6e4d97`
    // exemption on the inflight rec's 0x404 bits — wow-re `wave-cast.md`).
    let on_next_swing = def.is_some_and(|d| d.on_next_swing());
    let normal_cast = !def.is_some_and(|d| d.ranged_attack()) && !on_next_swing;
    // Re-pressing the queued strike is the ref's silent same-spell bail (`6e4d43`) — no cancel,
    // no error: 1.12 has no re-press-to-unqueue.
    if on_next_swing && queued_melee.current() == Some(spell_id) {
        debug!("ui_action: cast {spell_id} suppressed — already queued on next swing");
        return;
    }
    if (normal_cast || on_next_swing) && pending.in_flight(now) {
        // The ref's already-casting refusal: the same spell bails silently (`6e4d43`); a
        // different one errors reason 0x61 "Another action is in progress" (`6e4d97` →
        // `HandleCastFailed`) — the inflight rec here is always an ordinary cast, so even an
        // on-next-swing press is refused while it holds.
        if pending.current(now) != Some(spell_id) {
            cast_errors.0.push((spell_id, 0x61));
        }
        debug!("ui_action: cast {spell_id} suppressed — a cast is already in flight");
        return;
    }
    // The client-side mounted gate (decision 0481; wow-re `mounted-action-gate.md` §5:
    // TryCast's requirement validator `0x6094f0`, mounted block `0x609c6c` — a live
    // `UNIT_FIELD_MOUNTDISPLAYID` refuses a non-exempt cast with reason 0x39 "You are
    // mounted" BEFORE the cast-arm's target binding, which is why a targetless mounted click
    // never reads "You have no target"). Exemption: Attributes bit 24 (`0x01000000`,
    // castable-while-mounted). The gate must be LOCAL: vmangos silently dismounts a mounted
    // caster instead of erroring, so without this check the message can never appear. Named
    // micro-divergence: the ref range-tests a RESOLVED target (its step 6) before this gate;
    // ours range-tests after binding, so the mounted∧out-of-range double fault shows "You are
    // mounted" where the ref shows "Out of range." — unobservable outside that corner.
    if state::cast_mounted_refusal(
        ctx.rel
            .self_store
            .is_some_and(|s| s.0.unit_mount_display_id() > 0),
        def,
    ) {
        debug!("ui_action: cast {spell_id} refused locally — mounted (0x39)");
        cast_errors.0.push((spell_id, 0x39));
        return;
    }
    // The shapeshift-form leg of the SAME requirement validator (`0x6094f0` at `0x609e49` →
    // the form gate `0x612480`; wow-re `shapeshift-plaincast-toggle.md` §Q3, which corrected
    // `mounted-action-gate.md`'s `0x609ca2` gloss — that address is the POSTURE gate, reason
    // 0x3e NOT_STANDING; vmangos corroborates the reason split,
    // `SpellEntry::GetErrorAtShapeshiftedCast`): a form-blocked press refuses locally with the
    // gate's own red line — 0x3d "Can't do that while shapeshifted" / 0x56 needs-a-form — and
    // never sends, exactly like the mounted leg above. This is the whole Ghost Wolf experience:
    // ordinary spells carry NOT_SHAPESHIFT (verified in the 5875 data), so a shifted shaman's
    // every press lands here.
    if let Some(d) = def {
        let form = ctx
            .rel
            .self_store
            .map(|s| s.0.unit_shapeshift_form())
            .unwrap_or(0);
        let form_is_stance = spells
            .and_then(|s| s.forms.get(&u32::from(form)))
            .is_some_and(|f| f.is_stance());
        if let Some(refusal) = d.form_refusal(form, form_is_stance) {
            let reason = refusal.reason();
            debug!("ui_action: cast {spell_id} refused locally — the form gate ({reason:#x})");
            cast_errors.0.push((spell_id, reason));
            return;
        }
    }
    // The pre-send totem/reagent possession check (`CheckReagentsAndTotems 0x6e4000`, TryCast's
    // `0x6e4ded` — decision 0552): a missing tool (Mining Pick) or a short reagent refuses HERE
    // with the client's own 0x78/0x5c red line and NEVER sends. The gate must be local: vmangos
    // answers a sent pickless cast with the wrong code (`ITEM_GONE` "Item is gone"), so without
    // it the real message can't appear. Placement after the mounted gate mirrors the ref only
    // approximately (the `0x6094f0`-vs-`0x6e4ded` call order isn't pinned — double-fault
    // corners only).
    if reagent_totem_refusal(spell_id, def, ctx.rel.self_store, items, cast_errors) {
        return;
    }
    // ArmCast (`0x6e5250`): resolve the wire target from the spell's targeting constraints —
    // never the raw selection ([`cast_target`] module docs). A refusal is local and pre-commit,
    // like the ref's residual flag_word: no send, no GCD, no pending arm, no autorepeat key.
    let target = match cast_target::resolve_cast_target(
        def,
        ctx.selection_guid,
        ctx.self_guid,
        ctx.auto_self_cast,
        &ctx.rel,
    ) {
        cast_target::CastWireTarget::SelfImplicit => None,
        cast_target::CastWireTarget::Unit(guid) => Some(guid),
        cast_target::CastWireTarget::GroundTargeting => {
            // Enter the targeting-cursor mode's location half (decision 0792) — nothing is
            // sent, nothing armed: the ref's cursor entry (`6e50c8`) runs none of the commit
            // tail; the world click's commit owes the GCD and the pending arm.
            debug!("ui_action: cast {spell_id} awaits its ground click — targeting cursor up");
            ground.enter(spell_id);
            return;
        }
        cast_target::CastWireTarget::Refused(reason) => {
            debug!("ui_action: cast {spell_id} refused locally — unbindable target ({reason:#x})");
            cast_errors.0.push((spell_id, reason));
            return;
        }
    };
    // The local range gate (the client's TryCast runs `CanTargetUnit 0x6e4440` →
    // `IsTargetInRange 0x6e47b0` BEFORE the commit `0x6e54f0`): an out-of-range / too-close
    // press on a bound unit target refuses here — before the ranged-stance arm below, so a
    // too-close Throw/Auto Shot never draws the bow and never stows the melee weapons (the
    // sheath snap `0x6e5930` lives in the commit tail this refusal never reaches). The bound
    // target is only ever the selection or ourselves; a self-bind (autoSelfCast) is distance 0
    // with a min-0 range in practice, so only the selection leg is tested.
    if let Some(d) = def {
        if target.is_some() && target == ctx.selection_guid && target != ctx.self_guid {
            let row = spells.and_then(|s| s.ranges.get(d.range_index));
            let dist_sq = ctx
                .range
                .self_pos
                .zip(ctx.range.target_pos)
                .map(|(a, b)| a.distance_squared(b));
            if let Some(reason) = state::cast_range_refusal(
                d,
                row,
                ctx.range.self_reach,
                ctx.range.target_reach,
                dist_sq,
            ) {
                debug!("ui_action: cast {spell_id} refused locally — range ({reason:#x})");
                cast_errors.0.push((spell_id, reason));
                return;
            }
        }
    }
    // The wand-only auto-repeat handoff (the client's `0x60959e` inside TryCast's `0x6094f0`
    // step, wow-re `nocked-ammo-cancel.md` §Q-B-5): a NEW cast cancels the running auto-repeat
    // iff the CACHED spell carries `AttributesEx3 & 0x400000` — wand Shoot 5019 alone in the
    // 1.12 data. Auto Shot survives by construction: hunter shot-weaving. The client first
    // sends `CMSG_CANCEL_CAST` naming the cached wand spell (`0x6095b8`), then runs the local
    // cancel (whose own `CMSG_CANCEL_AUTO_REPEAT` ack follows). The same-spell re-press never
    // reaches here — the toggle above returned.
    if let Some(cached) = auto_repeat.0 {
        if spells
            .and_then(|s| s.catalog.get(cached))
            .is_some_and(|d| d.casting_cancels_autorepeat())
        {
            debug!("ui_action: cast {spell_id} cancels the running wand repeat {cached}");
            let _ = commands
                .0
                .send(ClientCommand::CancelCast { spell_id: cached });
            let self_e = self_player.single().ok().map(|(e, _)| e);
            crate::creature_anim::cancel_auto_repeat_local(self_e, auto_repeat, ecs, commands);
        }
    }
    if let Some(d) = def {
        if let Ok((e, _)) = self_player.single() {
            if d.ranged_attack() {
                sheath.write(crate::creature_anim::SheathRequest {
                    entity: e,
                    state: 2,
                    ceremony: false,
                });
            }
            if d.auto_repeat() {
                ecs.entity(e).insert(crate::creature_anim::AutoRepeatArmed);
                // The live autorepeat key (`0xceac30 = SpellRec+0x00` at `0x6e5947`) — what the
                // button's flash/checked state reads until CANCEL_AUTO_REPEAT clears it.
                auto_repeat.0 = Some(spell_id);
            }
        }
    }
    let _ = commands
        .0
        .send(ClientCommand::CastSpell { spell_id, target });
    if normal_cast {
        pending.arm(spell_id, now);
    } else if on_next_swing {
        queued_melee.arm(spell_id);
    }
    // TryCast's post-send tail (`6e51b5`) — byte-verified whole by the 2026-07-14 wow-re §5
    // (`combat-feel-law.md` @ c445713b): a committed send whose rec passes
    // [`SpellDisplay::initiates_auto_attack`] (on-next-swing `0x404` or `AttributesEx & 0x200`,
    // and not the GO-deferred Ex2-bit20; Charge carries none) starts the melee auto-attack at
    // the cast's bound unit target, unless one is already running (`0x60ecb0` over
    // `[player+0xc48]`; our mirror is the wire-echoed `Engaged`). The start is the attack path's
    // own pair (`0x6131a0` → `0x5ecb70`): melee-sheath SNAP + `CMSG_ATTACKSWING` — the same two
    // edges as the Attack button's arm in [`super::drain::drain_action_uses`]. Path-independent
    // in the ref (button/spellbook/CastSpellByName share the one tail) — matching our one send
    // seam.
    if let (Some(d), Some(guid)) = (def, target) {
        if d.initiates_auto_attack() {
            if let Ok((e, engaged)) = self_player.single() {
                if !engaged {
                    debug!("ui_action: cast {spell_id} initiates auto-attack at {guid:#x}");
                    sheath.write(crate::creature_anim::SheathRequest {
                        entity: e,
                        state: 1,
                        ceremony: false,
                    });
                    // Melee attack-start cancels a running auto-repeat UNCONDITIONALLY — the
                    // client's `0x5ecd8c` tail right after its melee snap (wow-re
                    // `nocked-ammo-cancel.md` §Q-B-5): you can't melee and auto-shoot at once.
                    crate::creature_anim::cancel_auto_repeat_local(
                        Some(e),
                        auto_repeat,
                        ecs,
                        commands,
                    );
                    let _ = commands.0.send(ClientCommand::AttackSwing { guid });
                }
            }
        }
    }
    // Arm the GCD at send (`StartGlobalCooldown 0x6e2de0` ← the cast-send arm `0x6e58fb`,
    // byte-verified) — a later `SMSG_CAST_RESULT` failure clears it again (`0x6e1630`).
    if let Some(d) = def {
        cooldowns.start_gcd(spell_id, d, now);
    }
}
