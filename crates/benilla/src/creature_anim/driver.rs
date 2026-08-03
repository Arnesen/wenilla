//! The Bevy driver: [`drive_animations`], which executes the state machine [`super::select`]
//! picks, reading and writing [`super::AnimDriver`]'s private state. Kept in its own file as it
//! carries the bulk of the per-frame Bevy system logic, separate from the module face ([`super`])
//! and the pure selector logic ([`super::select`]). Split into a small family of children for its
//! three extractable concerns: the playback primitives ([`play`]), the wound-flinch secondary-blend
//! slot logic ([`wound`]), and the per-hand weapon-grip overlay ([`grip`]) — this file remains the
//! stable face, re-exporting [`grip::drive_hand_grip`] unchanged.

use std::time::Duration;

use benilla_assets::ModelAnimations;
use benilla_formats::AnimDataCatalog;
use bevy::animation::graph::AnimationNodeIndex;
use bevy::animation::transition::AnimationTransitions;
use bevy::prelude::*;

use crate::net::{
    ClientCommand, FacingStep, NetCommands, ObjectStore, RemoteMotion, SelfPlayer, Spline,
    UnitSpeeds,
};
use crate::sound::EmoteSounds;

use super::select::{
    self, current_special, defense_anim, gait_candidates, is_bare_stand, is_swing_id,
    playback_rate, ready_anim, route_oneshot, state_emote_gait, swing_anim_main, swing_anim_off,
    unify, Mode, OneShotRoute, DEATH, DEFAULT_WALK_SPEED, STAND,
};
use super::sheath::{advance_sheath_ceremony, start_sheath_ceremony};
use super::{
    find_resolved, move_flags, AnimData, AnimDriver, AutoRepeatArmed, CastHold, DefenseAnim,
    EmoteAnim, Engaged, MovementState, Overlay, OverlayFade, RangedHold, SheathRequest,
    SheathSwapMessage, SwingImpact, SwingMessage, SwingSlowdown, Wielded, WoundAnim,
};

mod grip;
mod play;
#[cfg(test)]
mod tests;
mod wound;

pub(super) use grip::drive_hand_grip;
use play::{
    enter_special, leave_special, oneshot_finished, play, play_clip, roll_loop, roll_oneshot,
};
use wound::{wound_evict, wound_trigger, wound_upkeep, WoundEdge};

/// The weight of a masked upper-body one-shot overlay ([`AnimDriver::overlay`]) over the base clip on
/// the SpineLow subtree both drive (decision 0087). The base clip is *not* masked out of that subtree
/// (it animates the whole skeleton), so base and overlay blend there — this makes the overlay
/// dominate ≈ 8:1 (the torso visibly swings/emotes, not a wash with the run's torso), a small bleed
/// the cost of Bevy's weighted blend. The legs are excluded by the mask entirely. Same rationale and
/// value as the per-arm sheath ceremony's overlay weight.
const ONESHOT_OVERLAY_WEIGHT: f32 = 8.0;

/// The key-bone **fade-to-rest** window (decision 0878 — wow-re `oneshot-lifecycle.md` §5.4): a
/// finished upper-body one-shot is never stopped. The client's per-frame advance latches the
/// completion (`0x719370`), the deferred event reaches `CGUnit::OnAnimationFinished 0x5fc920` the
/// same frame, and that calls op4 with `param_3 = -1` (`0x5fcacb`) — which snapshots the clip's
/// **held final frame** into the bone's secondary slot, seeds `+0x100 = clock + 150`,
/// `+0x104 = 1/150`, `+0x108 = 1.0`, and disarms the primary so the bone inherits bone 0 again.
/// **Fixed 150 ms** — deliberately NOT the sequence's own blendTime, which is used only on the arm
/// path.
const ONESHOT_RELEASE_FADE: f32 = 0.150;

/// The launch vertical speed (yd/s, +up) above which a new airborne arc is a **jump** (the 37/38
/// bracket) rather than a step-off fall. A jump launches at ≈7.96 up (our own controller's
/// `JUMP_SPEED`; a remote's `-zspeed - g·t` is still ≳4 after relay latency); a step-off starts
/// level or already descending — the two never come near this line.
const JUMP_ARC_MIN_UP: f32 = 0.5;

/// A one-shot play request gathered from this frame's messages, resolved to an anim id inside
/// the unit loop (a swing's id keys the attacker's own wielded weapon).
enum OneShotReq {
    Swing(u32),
    Emote(u16),
}

/// The currently-armed one-shot the combat fast-path tests — the client's key-bone-else-bone-0
/// current-id read (`0x5fe422`, wow-re `combat-anim-fastpath.md` §1): the masked overlay while
/// its node still plays, else the full-body [`Mode::Swing`] clip while unfinished. Gait and
/// Special clips are never returned; their ids aren't combat, so nothing is lost at the
/// classifier gate.
fn live_oneshot(
    drv: &AnimDriver,
    player: &AnimationPlayer,
    tr: &AnimationTransitions,
    anims: &ModelAnimations,
    catalog: Option<&AnimDataCatalog>,
) -> Option<(u16, AnimationNodeIndex)> {
    if let Some(ov) = drv.overlay {
        if player.animation(ov.node).is_some_and(|a| !a.is_finished()) {
            return Some((ov.id, ov.node));
        }
    }
    if let Mode::Swing { id: m, .. } = drv.mode {
        if !oneshot_finished(player, anims, m, catalog) {
            return tr.get_main_animation().map(|n| (m, n));
        }
    }
    None
}

/// Whether **any** one-shot is currently live on the unit — [`live_oneshot`] minus the node. The
/// missile queue's anim-end flush backstop (the client's `0x5fc920` on-anim-finish → `0x60c9b0`
/// is a *callback*; ours polls this edge): a queued missile whose release keyframe never fired
/// launches when the cast one-shot ends.
pub(crate) fn oneshot_is_live(
    drv: &AnimDriver,
    player: &AnimationPlayer,
    tr: &AnimationTransitions,
    anims: &ModelAnimations,
    catalog: Option<&AnimDataCatalog>,
) -> bool {
    live_oneshot(drv, player, tr, anims, catalog).is_some()
}

/// Retire whatever holds the key-bone slot into its cross-fade ([`AnimDriver::overlay_fade`]) —
/// the client's op4 snapshot of the outgoing pose into the bone's SECONDARY (`rep movsd
/// +0x98 → +0xc4`) plus the window seed (decision 0878). `total` is the window: the incoming
/// clip's own blendTime on a blended re-arm, [`ONESHOT_RELEASE_FADE`] on a fade-to-rest. Honors
/// the client's **re-snapshot guard** (`0x7125d4` on the arm path, `0x7123a2` on the release
/// path, both against `0x7ffa24 = 0.5f`): a fade still running above λ = 0.5 is not re-seeded —
/// the older pose keeps decaying and the superseded node is simply dropped. Leaves
/// [`AnimDriver::overlay`] empty; the caller arms the incoming clip (if any) after it.
fn retire_overlay(drv: &mut AnimDriver, player: &mut AnimationPlayer, total: f32) {
    let out = drv.overlay.take().map(|ov| ov.node);
    if drv.overlay_fade.is_some_and(|f| fade_lambda(&f) > 0.5) {
        if let Some(n) = out {
            player.stop(n);
        }
        return;
    }
    if let Some(prev) = drv.overlay_fade.take().and_then(|f| f.out) {
        player.stop(prev);
    }
    drv.overlay_fade = Some(OverlayFade {
        out,
        left: total,
        total,
    });
}

/// The blend weight λ of a key-bone cross-fade this frame — `smoothstep` over the fraction of the
/// window still to run, so it decays 1 → 0 ([`select::blend_lambda`]).
fn fade_lambda(f: &OverlayFade) -> f32 {
    select::blend_lambda(if f.total > 0.0 { f.left / f.total } else { 0.0 })
}

/// The key-bone cross-fade's per-frame advance (decision 0878) — the kernel's λ decay
/// (`0x714880`–`0x714923`) and its self-release (`0x7147b9`: `+0xd0 = -1` and λ = 0 the same
/// frame). Two shapes, decided by whether an incoming clip holds the slot:
///
/// - **A release** (no incoming): the retiring node alone fades against the base, so its weight
///   must land the blended share on `λ · W/(1+W)` — the client's full-override λ, capped by the
///   standing [`ONESHOT_OVERLAY_WEIGHT`] approximation. `w = W·λ / (1 + W·(1−λ))`.
/// - **A re-arm** (incoming present): the two clips split the slot's share by λ exactly as the
///   client's `primary + (secondary − primary)·λ` does, at constant total — `W·λ` out, `W·(1−λ)`
///   in. (An incoming that arrived by *transplant* seeds no fade at all: `blendFlag = 0`.)
fn overlay_fade_upkeep(drv: &mut AnimDriver, player: &mut AnimationPlayer, dt: f32) {
    let Some(mut f) = drv.overlay_fade else {
        return;
    };
    f.left = (f.left - dt).max(0.0);
    let lambda = fade_lambda(&f);
    let live = drv.overlay.map(|ov| ov.node);
    let w = ONESHOT_OVERLAY_WEIGHT;
    if let Some(a) = f.out.and_then(|n| player.animation_mut(n)) {
        a.set_weight(if live.is_some() {
            w * lambda
        } else {
            w * lambda / (1.0 + w * (1.0 - lambda))
        });
    }
    if let Some(a) = live.and_then(|n| player.animation_mut(n)) {
        a.set_weight(w * (1.0 - lambda));
    }
    if f.left <= 0.0 {
        if let Some(n) = f.out {
            player.stop(n);
        }
        if let Some(a) = live.and_then(|n| player.animation_mut(n)) {
            a.set_weight(w);
        }
        drv.overlay_fade = None;
    } else {
        drv.overlay_fade = Some(f);
    }
}

/// The **TRANSPLANT** (`0x5fe919` — wow-re `oneshot-lifecycle.md` §3a, the half
/// `anim-composition-model.md` §2 was missing; decision 0878). A base **locomotion** clip
/// requested while bone 0 still plays a live **CAST** or **COMBAT** one-shot does not replace it:
/// the client copies the bone-0 descriptor — its id, its rate, and `+0x08` the clip's **live
/// elapsed position** — onto the key-bone with `blendFlag = 0`, then hands bone 0 the request. The
/// cast therefore keeps running on the torso *at exactly the frame it was on*, with no cross-fade
/// and no restart, while the legs take the jump or the run. This is the director's "jump right
/// after a cast: the legs jump, the arms finish the cast".
///
/// A no-op when the key-bone is already armed — `0x5fe912` then jumps straight to `0x5fe930`
/// (request → bone 0, key-bone untouched), which is exactly what leaving [`AnimDriver::overlay`]
/// alone already gives us. Named simplification: the moved clip re-arms as a plain `Never` repeat,
/// dropping any unspent replay budget (`R > 1`); every cast and swing this fires for is authored
/// `(0, 0)` ⇒ `R = 1`.
fn transplant_up(
    drv: &mut AnimDriver,
    player: &mut AnimationPlayer,
    tr: &AnimationTransitions,
    anims: &ModelAnimations,
    entity: Entity,
    id: u16,
) -> bool {
    if drv.overlay.is_some() || !(select::is_cast_anim(id) || select::is_combat_anim(id)) {
        return false;
    }
    let Some(node) = tr.get_main_animation() else {
        return false;
    };
    // The live position + rate, read off the armed VARIATION node (the play rolled one of them),
    // which is also how we find the masked twin to resume on.
    let Some((seek, speed)) = player
        .animation(node)
        .filter(|a| !a.is_finished())
        .map(|a| (a.seek_time(), a.speed()))
    else {
        return false;
    };
    let Some(upper) = anims
        .clips
        .iter()
        .find(|c| c.node == node)
        .and_then(|c| c.upper_node)
    else {
        return false;
    };
    let active = player.play(upper);
    active.replay();
    active.set_repeat(bevy::animation::RepeatAnimation::Never);
    active.seek_to(seek);
    active.set_speed(speed);
    active.set_weight(ONESHOT_OVERLAY_WEIGHT); // `blendFlag = 0` — no ramp, no cross-fade
    drv.overlay = Some(Overlay {
        node: upper,
        id,
        looping: false,
    });
    if crate::dbg_trace::enabled() {
        crate::dbg_trace::line(
            "fct",
            &format!("anim transplant unit={entity} id={id} -> key-bone at {seek:.3}s (bone 0 takes the request)"),
        );
    }
    true
}

/// The animation state machine, per unit (decision 0049 + 0073). Death overrides; otherwise:
/// enter/loop/exit the Special states (jump, sit/sleep/kneel) as one-shot-bracketed loops, play the
/// per-packet melee swings as preemptible one-shots, and cross-fade the gaits (the engaged Ready
/// idle among them).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn drive_animations(
    mut commands: Commands,
    mut units: Query<(
        Entity,
        &ModelAnimations,
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &mut AnimDriver,
        Option<&Spline>,
        Option<&RemoteMotion>,
        Option<&MovementState>,
        Option<&UnitSpeeds>,
        Option<&ObjectStore>,
        Option<&Wielded>,
        Option<&CastHold>,
        Option<&FacingStep>,
        (
            Has<Engaged>,
            Has<AutoRepeatArmed>,
            Has<RangedHold>,
            Has<SelfPlayer>,
            Option<&crate::entities::mount::MountBody>,
            Has<crate::net::CreatureSwimming>,
            // The rendered model scale — `OBJECT_FIELD_SCALE_X`, baked onto the entity transform by
            // `entities::attach`. The locomotion playback rate divides by it (decision 0903), so an
            // ogre at 2.2× cycles its legs 2.2× slower than a same-speed human. Read-only: nothing
            // in this system moves a unit.
            &Transform,
        ),
    )>,
    // A mount child's movement view is its HOST's (decision 0441): the same
    // MovementState/RemoteMotion/Spline/speeds the rider's `unify` reads, fetched through
    // `MountBody.host` — so the untouched gait machinery locomotes the mount for self, remote
    // players, and mounted NPCs alike. Read-only over types `units` also only reads.
    mount_hosts: Query<(
        Option<&Spline>,
        Option<&RemoteMotion>,
        Option<&MovementState>,
        Option<&UnitSpeeds>,
        Option<&FacingStep>,
        Has<crate::net::CreatureSwimming>,
        // …and the rider's own `OBJECT_FIELD_SCALE_X`: a mount child's transform carries only its
        // `CreatureDisplayInfo` column, and the mount renders at the PRODUCT of the two (the
        // byte-verified mount composition, wow-re `0x613ef0`). The rate divisor wants that product
        // — the mount model's world scale — not the child's local column (decision 0903).
        &Transform,
    )>,
    mut swings: MessageReader<SwingMessage>,
    mut impacts: MessageReader<SwingImpact>,
    mut defenses: MessageReader<DefenseAnim>,
    mut slows: MessageReader<SwingSlowdown>,
    mut emotes: MessageReader<EmoteAnim>,
    mut spell_wounds: MessageReader<WoundAnim>,
    mut sheath_requests: MessageReader<SheathRequest>,
    mut sheath_swaps: MessageWriter<SheathSwapMessage>,
    anim_data: Option<Res<AnimData>>,
    net: Res<NetCommands>,
    // The shared emote-audio catalog, promoted here for its `Emotes.dbc` → `AnimID` column: the
    // looping state-emote idle (`UNIT_NPC_EMOTESTATE`) below resolves through it, same as the
    // `SMSG_EMOTE` one-shot consumer ([`emote_anim::emote_to_anim`]).
    // One tuple param (the 16-SystemParam ceiling): the shared emote-audio catalog, the
    // client-local loot-target latch, and the frame clock.
    aux: (
        Option<Res<EmoteSounds>>,
        // The loot-target latch (decision 0515): the SELF unit's kneel trigger — armed at the
        // `CMSG_LOOT` send, dropped at release/refusal — read by the loot leg below. `Option`
        // for the headless test worlds that don't build the loot seam.
        Option<Res<crate::ui_loot::LootLatch>>,
        // The frame clock — the key-bone cross-fade's only time source (decision 0878): its
        // retiring node is *frozen* on the clip's final frame, so unlike the wound's decay there
        // is no playback clock to read the λ window off.
        Res<Time>,
    ),
    // The variation roll's LCG state (decision 0114 — the client's single CRT `_rand` stream,
    // shared by every play; [`select::msvc_rand`]).
    mut rng: Local<u32>,
    // The SELF unit's last-written anim state line of the `WOW_MOVE_TRACE` debug trace (the
    // diff-only filter; see the trace block after the mode machine).
    mut anim_trace_last: Local<Option<String>>,
) {
    let (emote_sounds, loot_latch, time) = aux;
    let dt = time.delta_secs();
    // This frame's one-shot PLAY CALLS (swings + anim-emotes), gathered per unit and replayed
    // in the client's call order below ([`PlaySeq`] stamps — the net drain stamps packet order,
    // scene-time emitters stamp after it). Order matters twice over: the later call overwrites
    // the earlier on the default path (decision 0399), and the combat FAST-PATH (decision 0406)
    // keys on what is *currently playing* when each call runs.
    let mut pending: bevy::ecs::entity::EntityHashMap<Vec<(OneShotReq, u64)>> = default();
    // …and by victim: a landed hit with the flinch bit (`HitInfo & 0x2` — the sole trigger gate,
    // decision 0111) plays the victim's wound-flinch **decay overlay** below, as does a spell
    // impact whose kit carries a CombatWound anim ([`WoundAnim`], decision 0099 phase 4 — the
    // kit player's own 8–10 branch). Last hit wins, matching the client, where a re-trigger
    // re-seeds the same secondary slot. Independent of the attacker-side 0x10000 suppressor
    // (that bit gates the *swing* animation only).
    let mut pending_wound: bevy::ecs::entity::EntityHashMap<WoundEdge> = default();
    for s in swings.read() {
        // HitInfo bit 0x10000 suppresses the swing anim (decision 0073's verified suppressor).
        if s.hit_info & 0x10000 == 0 {
            pending
                .entry(s.attacker)
                .or_default()
                .push((OneShotReq::Swing(s.hit_info), s.seq));
        }
    }
    // The victim's wound flinch fires at the swing clip's IMPACT keyframe ([`SwingImpact`],
    // `impact.rs`), not at packet receive — the blow lands, then the victim recoils.
    for SwingImpact {
        swing: s,
        text_only,
        ..
    } in impacts.read()
    {
        if !text_only && s.hit_info & 0x2 != 0 {
            if let Some(victim) = s.victim {
                pending_wound.insert(victim, WoundEdge::Melee(s.hit_info));
            }
        }
    }
    for w in spell_wounds.read() {
        pending_wound.insert(w.entity, WoundEdge::Spell(w.anim_id));
    }
    // This frame's victim DEFENSE reactions (`$CPP`, decision 0279), keyed by victim — last wins
    // (a re-trigger re-arms the same primary). Resolved to an anim id inside the loop: the parry
    // pick keys the victim's OWN mainhand, and the client's alive gate needs the store.
    let mut pending_defense: bevy::ecs::entity::EntityHashMap<u32> = default();
    for d in defenses.read() {
        pending_defense.insert(d.victim, d.victim_state);
    }
    // This frame's whiff slow-downs (`0x712910`, decision 0279): the attacker's in-flight swing
    // drops to HALF speed for its remainder.
    let mut pending_slow: bevy::ecs::entity::EntityHashSet = default();
    for s in slows.read() {
        pending_slow.insert(s.0);
    }
    // This frame's anim-emotes join the same request list (the [`PlaySeq`] sort below puts the
    // two writer streams in emission order). Played as the same over-the-gait one-shot as a
    // swing (the talk emote on interact — decision 0081).
    for e in emotes.read() {
        pending
            .entry(e.entity)
            .or_default()
            .push((OneShotReq::Emote(e.anim_id), e.seq));
    }
    // This frame's sheath requests, keyed by unit (a later request replaces an earlier — the
    // client's last SetSheatheState call wins).
    let mut pending_sheath: bevy::ecs::entity::EntityHashMap<SheathRequest> = default();
    for r in sheath_requests.read() {
        pending_sheath.insert(r.entity, *r);
    }
    // The missing-clip resolver's DBC source (decision 0082): `None` for the brief window before
    // `AnimationData.dbc` loads, in which every lookup below degrades to identity.
    let catalog = anim_data.as_deref().map(|d| &d.0);
    for (
        entity,
        anims,
        mut player,
        mut tr,
        mut drv,
        spline,
        remote,
        movement,
        speeds,
        store,
        wielded,
        cast_hold,
        facing_step,
        (engaged, auto_repeat, ranged_hold, is_self, mount_body, creature_swimming, transform),
    ) in &mut units
    {
        // A mount child drives from its HOST's movement view (decision 0441) — same inputs the
        // rider's own pass reads, so the mount plays exactly the locomotion the rider suppresses.
        // A vanished host (teardown race) reads as stationary until the child despawns with it.
        // The host also supplies the missing half of the mount's world scale (see the query docs).
        let (spline, remote, movement, speeds, facing_step, creature_swimming, host_scale) =
            match mount_body {
                Some(mb) => match mount_hosts.get(mb.host) {
                    Ok((s, r, m, sp, f, sw, t)) => (s, r, m, sp, f, sw, t.scale.x),
                    Err(_) => (None, None, None, None, None, false, 1.0),
                },
                None => (
                    spline,
                    remote,
                    movement,
                    speeds,
                    facing_step,
                    creature_swimming,
                    1.0,
                ),
            };
        // The rendered world scale of the model playing these clips — the `0x5fe2f0` rate divisor's
        // `|modelScale|` (decision 0903). A plain unit's transform already IS its world scale; a
        // mount child's is its display column, which composes under the rider's.
        let model_scale = transform.scale.x * host_scale;
        let walk = speeds.map_or(DEFAULT_WALK_SPEED, |s| s.0.walk);
        // Dead ⇒ health 0 with a real max. Absent health counts as ZERO (`unit_is_dead`): a create
        // block omits zero fields, so an already-dead corpse streams in with no HEALTH at all —
        // requiring an explicit `Some(0)` here is exactly the bug where corpses stood up on relog.
        let dead = store.is_some_and(|s| s.0.unit_is_dead());
        // Mounted (decision 0441): `UNIT_FIELD_MOUNTDISPLAYID` nonzero — the wire's one mounted
        // signal. The rider's base pins to Mount(91) below, Specials/one-shot full-body routes are
        // suppressed, and the sheath reconcile force-stows; the locomotion the selector would have
        // picked plays on the MOUNT child entity instead (its own driver pass, fed the host's
        // movement view by `sync_mount_motion`). A mount child itself has no `ObjectStore`, so it
        // can never read as mounted here.
        let mounted = store.is_some_and(|s| s.0.unit_mount_display_id() != 0);
        // First time we drive this unit (nothing chosen yet) — used to settle a corpse to its end pose.
        let first = drv.gait.is_none() && drv.mode == Mode::Gait;
        let mut mv = unify(movement, remote, spline, creature_swimming);
        // Stand state (decision 0080c): a unit without a controller-fed [`MovementState`] (remote
        // players, creatures) poses from its `UNIT_FIELD_BYTES_1` stand-state byte — a seated
        // remote player renders for free. Our own avatar's rides its `MovementState` (the
        // controller overlays its in-flight request so the pose never waits on the echo).
        if movement.is_none() {
            mv.stand_state = store.map_or(0, |s| s.0.unit_stand_state());
        }
        // Mounted forces stand-state 0 on every recompute (`0x5fdf80`, wow-re sheath-policy §3):
        // no sit/sleep/kneel pose can hold in the saddle.
        if mounted {
            mv.stand_state = 0;
        }
        // Stealth — the prowl pose ([`select::STEALTH_WALK`] / [`select::STEALTH_STAND`]): the CREEP
        // vis flag off the unit's OWN descriptor, read every frame for self and remote alike, which
        // is where the client reads it (`[[unit+0x110]+0x213] & 2` at select time). Never
        // controller-fed, unlike the stand state — there is no client-side prediction of stealth, so
        // the crouch lands with the server's aura. A mount child carries no store and reads `false`:
        // right by construction, since the rider's body holds Mount(91) regardless and stealth and
        // mounts are mutually exclusive anyway.
        mv.stealthed = store.is_some_and(|s| s.0.unit_is_stealthed());
        let moving = mv.flags & move_flags::ANY_MOVE != 0;
        // The airborne arc's bookkeeping (wow-re land-anim-height-gate + rf57b §2): on the arc's
        // FIRST airborne frame, its launch vertical speed splits a **jump** (upward — the client's
        // JumpStart/Jump bracket rides the MSG_MOVE_JUMP event) from a **step-off fall** (level or
        // downward — no bracket; the gait freezes until FALLINGFAR latches Fall 40, and an
        // unlatched landing is a full no-op: the gait keeps rolling — decision 0187).
        let falling = mv.flags & move_flags::FALLING != 0;
        let was_falling = std::mem::replace(&mut drv.was_falling, falling);
        if falling && !was_falling {
            drv.jump_arc = mv.vertical_speed > JUMP_ARC_MIN_UP;
        }
        // The airborne-freeze's exact gate (`0x5fd8e8`, §5-arbitrated — decision 0868):
        // keep-current iff `FALLING && (FALLINGFAR || vz ≠ 0)`. The vz clause pins every real
        // arc (ballistics make vz ≠ 0 from the first integrated substep); the one uncovered
        // case is a fresh walk-off's vz == 0 substep, where the client genuinely may re-pick.
        // Gates the gait slot's mid-air keep-current AND the deferred-cache consumer below
        // (the `+0xd60` read sits downstream of the freeze: a parked clip never plays mid-air).
        let airborne_frozen =
            falling && (mv.flags & move_flags::FALLING_FAR != 0 || mv.vertical_speed != 0.0);
        // The idle re-face turn-shuffle (decision 0123 — the client's facing-delta latch
        // `0x607ed0`, wow-re `loop-replay-fidget.md` §5b): a stationary creature easing its yaw
        // toward its target reads as *turning* to the anim layer, so the gait picks
        // ShuffleLeft/Right and each Shuffle→Stand return re-arms (and, relaxed, re-rolls) Stand
        // — the fidget's recurring trigger. Gated like the client's can-fidget gate (`0x5fce30`:
        // no combat/cast); positive step = counterclockwise WoW yaw = turning left.
        if let Some(step) = facing_step.filter(|_| !moving && !engaged && cast_hold.is_none()) {
            mv.flags |= if step.0 > 0.0 {
                move_flags::TURN_LEFT
            } else {
                move_flags::TURN_RIGHT
            };
        }
        // Whether a looping base arm this frame may ROLL its variation (the client's base-arm
        // `variationIdx = −1`, decision 0123) or is forced to the head: decided from the state
        // *before* this frame's transitions (the client tests the outgoing armed id).
        // (0880 also gated this on a stun. That gate cited `0x5eb4f2`/`0x5ec219` as "the idle/fidget
        // selectors bail on UNIT_FLAG_STUNNED" — re-read at the bytes, those two sites are
        // `ToggleSheath 0x5eb480` and `CanLootNow 0x5ec110`, and NO site in the whole `0x40000`
        // census touches animation selection. The stun does not quiet the fidget; the freeze does,
        // by stopping the clock (decision 0889), so the invented gate is gone.)
        let outgoing = drv.active_anim().unwrap_or(STAND);
        let relaxed = !select::arm_forces_head(engaged, cast_hold.is_some(), outgoing);

        wound_upkeep(&mut drv, &mut player);

        // Death overrides every state (a corpse doesn't transition); play Death and hold.
        if dead {
            if drv.gait != Some(DEATH) {
                if let Some(c) = find_resolved(anims, DEATH, catalog)
                    .or_else(|| find_resolved(anims, STAND, catalog))
                {
                    // A witnessed death rolls its variation like any one-shot (decision 0114); a
                    // corpse that streamed in dead settles on the deterministic head instead.
                    let c = if first {
                        c
                    } else {
                        anims
                            .pick_variation(c.anim_id, select::msvc_rand(&mut rng))
                            .unwrap_or(c)
                    };
                    let active =
                        tr.play(&mut player, c.node, Duration::from_secs_f32(c.blend_time));
                    if c.looping {
                        active.repeat();
                    } else if first {
                        active.seek_to(c.duration); // streamed in dead → settled corpse, not a replay
                    }
                }
                // The death play arms bone 0 — a blended primary re-arm overwrites that bone's
                // secondary slot (decision 0114, the shared-slot eviction): a FULL-BODY wound in
                // flight is evicted; a masked wound rides the key-bone and decays out over the
                // collapse via the upkeep above.
                if let Some(wd) = drv.wound.take_if(|wd| !wd.masked) {
                    player.stop(wd.node);
                }
                drv.mode = Mode::Gait;
                drv.gait = Some(DEATH);
                drv.deferred = None; // the death play is a normal arm — the cache clears
            }
            continue;
        }

        // The loot kneel's trigger (see [`select::LOOT`]; byte-verified, wow-re
        // `loot-anim-leg.md` — the 2026-07-18 §5 fold-back, decision 0515): the predicate
        // (`0x6126b0`) SPLITS on IsActivePlayer — the **self** unit kneels off the client-local
        // loot-target latch ([`crate::ui_loot::LootLatch`], armed at the `CMSG_LOOT` send, so
        // the kneel is client-predicted with no round-trip), a **remote** unit off its mirrored
        // descriptor: `UNIT_FLAG_LOOTING` (0x400) set AND the `0x10000000` bit clear. Never
        // mounted (the leg's `[+0xdc]==0` gate). Movement outranks by CHAIN POSITION, not an
        // in-leg gate — the core ground selector claims first on any direction bit (`[9e8]&0xf`)
        // — transcribed as the `ANY_MOVE` test here; a stationary *swimmer* with the trigger up
        // kneels mid-tread, as the bytes order it (locomotion's swim block needs a direction
        // bit too).
        let looting = !mounted
            && mv.flags & move_flags::ANY_MOVE == 0
            && if is_self {
                loot_latch.as_ref().is_some_and(|l| l.0.is_some())
            } else {
                store.is_some_and(|s| {
                    let f = s.0.unit_flags();
                    f & select::UNIT_FLAG_LOOTING != 0 && f & select::UNIT_FLAG_LOOT_SUPPRESS == 0
                })
            };

        // No Special claims a mounted rider (decision 0441): the jump/fall arc plays on the mount
        // child (whose synced view carries the same flags), and poses were zeroed above.
        let special = if mounted {
            None
        } else {
            current_special(&mv, drv.jump_arc)
        };
        // The loot leg outranks the standState pose resolver (the rf57 chain calls loot before
        // standState): a looting unit kneels at the loot, never into a sit/sleep bracket. The
        // airborne Specials keep their precedence (the chain's airborne freeze runs first).
        let special = special.filter(|s| !(looting && matches!(s, select::Special::Pose(_))));

        // ── Sheath state (decision 0080). The unit's rendered sheath is the **client-side
        // committed cache** (`drv.sheath_cur`, the client's `[+0xd40]`), not the raw descriptor
        // byte: seeded from the byte at first sight (silently), re-adopted whenever the byte
        // *changes* (the `0x604c70` field-apply — a remote unit's own client volunteered the
        // change; our own echo arrives already-committed and adopts as a no-op), and written by
        // the one-setter requests here + the per-animation reconcile below. Every transition
        // **snaps by default** — byte-verified across all 24 SetSheatheState call sites (wow-re
        // `sheath-policy.md`): only the manual toggle's request carries the ceremony, and the
        // draw/stow sound rides the ceremony playback (no clip → no sound).
        let sheath_frame_start = drv.sheath_cur;
        let sheath_byte = store.and_then(|s| s.0.unit_sheath_state()).unwrap_or(0);
        if drv.sheath_cur.is_none() || drv.sheath_byte != Some(sheath_byte) {
            drv.sheath_cur = Some(sheath_byte);
            drv.sheath_byte = Some(sheath_byte);
        }
        // The one setter's queue ([`SheathRequest`] — the Z toggle, the attack-start auto-draw,
        // the stand-state stow rider): idempotency refusal (the client's `newState == CUR`
        // abort), commit, the local player's `CMSG_SETSHEATHED` volunteer, and — for the manual
        // toggle alone — the ceremony: **per-arm masked overlays** (the client's per-slot plays,
        // `0x60b770`): the mainhand's on the right-arm subtree, the offhand's on the left, each
        // by that item's stow family (hip → HipSheath 90, back/shield → Sheath 89), composed
        // over whatever the body is doing and never cancelled; [`VisualSheath`] pins the old
        // weapon placement until the authored `$SHL`/`$SHR` event (~halfway — the hand at the
        // stow point). A ceremony without playable clips degrades to the snap.
        if let Some(req) = pending_sheath.get(&entity) {
            let cur = drv.sheath_cur.unwrap_or(0);
            // Mounted is a persistent DRAW-BLOCK (wow-re `sheath-policy.md` §3: the client's
            // recompute `0x5fdf80` tests the mount model FIRST and forces stowed on every
            // PlayAnimation — nothing can stay drawn on a mount). The client's composite
            // recomputes constantly (the mount's own gait plays); our rider track goes silent
            // under the seat pin, so the equivalent is refusing the draw at the setter — no
            // commit, no volunteered CMSG (named deviation: same observable, earlier site).
            if req.state != cur && !(mounted && req.state != 0) {
                // The setter's own trace, the twin of the reconcile's below: between them every
                // sheath transition names itself and its author, so "why is the weapon out"
                // is one `RUST_LOG=benilla=debug` grep rather than a re-derivation of the
                // 24-site policy from the symptom.
                debug!(
                    "sheath: unit {entity} {cur} -> {} (request, ceremony {})",
                    req.state, req.ceremony
                );
                drv.sheath_cur = Some(req.state);
                if is_self {
                    let _ = net.0.send(ClientCommand::SetSheathed {
                        state: u32::from(req.state),
                    });
                }
                if req.ceremony {
                    start_sheath_ceremony(
                        &mut commands,
                        entity,
                        &mut drv,
                        &mut player,
                        anims,
                        wielded,
                        cur,
                        req.state,
                        catalog,
                    );
                }
            }
        }
        // Ceremony upkeep, per arm: move that arm's weapon when its clip crosses the authored
        // `$SHL`/`$SHR` event (the hand-touches-weapon moment — the draw/stow ring fires there,
        // `sound::sheathe`, since the sound can't ride the tags themselves: hip clips carry none),
        // and when a *stow* clip finishes, run the client's **phase 2** — the on-anim-finish
        // drawer `0x5fc920` @ `0x5fca8c`/`0x5fcaa1`. That deferred second movement is what makes a
        // melee → ranged toggle read as "put the swords away, come back to neutral, *then* reach
        // over the shoulder for the bow" instead of one blended swap.
        let sheath_now = drv.sheath_cur.unwrap_or(0);
        advance_sheath_ceremony(
            &mut commands,
            entity,
            &mut drv,
            &mut player,
            anims,
            wielded,
            sheath_now,
            catalog,
            &mut sheath_swaps,
        );

        // ── This frame's one-shot: a melee swing (decision 0073 — one per SMSG_ATTACKERSTATEUPDATE)
        // or an anim-emote (decision 0081). A swing outranks an emote when both land (combat wins).
        // Its destination is chosen **per play from the unit's live state** (decision 0087,
        // `route_oneshot`): **masked** onto the SpineLow overlay (moving / seated / airborne-in-combat)
        // — playing beside `mode`, the base track's legs untouched — or **full-body** on the base
        // track (standing idle), replacing whatever the base was doing.
        //
        // The shared-slot eviction's inputs (decision 0114): a wound-flinch overlay is the client's
        // per-bone SECONDARY, and a blended primary re-arm on the *same bone* overwrites that slot
        // (op4 `blendFlag≠0` copies the outgoing pose over `+0xc4..`). So this frame's plays are
        // tracked — full-body plays (bone 0: the base track) evict a full-body wound; masked-slot
        // plays (the key-bone) evict a masked wound. A play on the *other* bone leaves the wound
        // decaying (the §5's inherited-swing case). Mode/gait changes proxy the mode machine's own
        // base plays; the flags catch the same-id re-plays the proxy can't see.
        let pre_state = (drv.mode, drv.gait);
        let mut base_played = false;
        let mut masked_played = false;
        let mut played_oneshot: Option<u16> = None;
        // Defense outranks a same-frame own swing/emote: the client's `$CPP` arm is the later
        // PlayAnimation call in that (rare) frame, and the being-hit reaction is the one the
        // player must read. Gated alive (`0x60ec00` checks IsDead / stand-state 7 before the LUT).
        let defense = pending_defense.get(&entity).and_then(|&vs| {
            if dead {
                return None;
            }
            let id = defense_anim(vs, wielded.and_then(|w| w.main));
            if let Some(id) = id {
                debug!("defense: unit {entity} anim {id} (victimState {vs})");
            }
            id
        });
        // The frame's play calls, resolved to anim ids in [`PlaySeq`] call order; the defense
        // reaction runs last (the client plays it from the deferred impact scan, later in the
        // frame than any message handler — decision 0399's surviving arm).
        let mut requests: Vec<u16> = pending
            .remove(&entity)
            .map(|mut v| {
                v.sort_by_key(|&(_, seq)| seq);
                v.into_iter()
                    .map(|(req, _)| match req {
                        OneShotReq::Swing(hit_info) => {
                            let w = wielded.copied().unwrap_or_default();
                            let id = if hit_info & 0x4 != 0 {
                                swing_anim_off(w.off)
                            } else {
                                swing_anim_main(w.main)
                            };
                            debug!("swing: unit {entity} anim {id} (hitInfo {hit_info:#x})");
                            id
                        }
                        OneShotReq::Emote(id) => id,
                    })
                    .collect()
            })
            .unwrap_or_default();
        requests.extend(defense);
        // The deferred-cache consumer (the client's `+0xd60` read at the base recompute,
        // decision 0406): the moment no one-shot is live, the parked combat clip plays — the
        // swing the Eviscerate spin deferred fires once the spin ends. A frame with fresh
        // requests consumes it implicitly instead (a normal arm clears the cache; a fast-path
        // hit re-parks its own request — both what the client's `0x5fe48e`/`0x5fe480` do).
        // NEVER mid-air: the read (`0x5fd392`, inside the `0x5fd360` recompute arm) sits
        // downstream of the airborne-freeze, so a park made mid-arc waits — and dies at the
        // landing play's clear (§5-verified, decision 0868).
        if requests.is_empty()
            && drv.deferred.is_some()
            && !airborne_frozen
            && live_oneshot(&drv, &player, &tr, anims, catalog).is_none()
        {
            requests.extend(drv.deferred.take());
        }
        for id in requests {
            // The COMBAT FAST-PATH (`0x5fe43c`–`0x5fe48b`, wow-re `combat-anim-fastpath.md`,
            // decision 0406): a combat clip requested while another combat clip is playing is
            // NOT armed — the CURRENT clip's rate doubles (op6 2.0f re-times its remainder,
            // pose-continuous) and the request parks in the `+0xd60` cache to play afterwards.
            // This is why the Eviscerate spin survives the auto-swings its cast triggers —
            // sped up, never cut — and why consecutive swings don't hard-cut each other.
            if let Some((cur, node)) = live_oneshot(&drv, &player, &tr, anims, catalog) {
                if select::is_combat_anim(cur) && select::is_combat_anim(id) {
                    if let Some(active) = player.animation_mut(node) {
                        active.set_speed(2.0);
                    }
                    drv.deferred = Some(id);
                    if crate::dbg_trace::enabled() {
                        crate::dbg_trace::line(
                            "fct",
                            &format!(
                                "anim fastpath unit={entity} cur={cur} req={id} (cur 2x, req deferred)"
                            ),
                        );
                    }
                    continue;
                }
            }
            // A normal arm clears the cache (the client's `0x5fe48e` writes −1 on every
            // non-fast-path PlayAnimation).
            drv.deferred = None;
            // The arm-level same-id dedup (`0x5fdba0`, decision 0280): a requested id that
            // already occupies its slot and is still playing is NOT re-armed — the mechanism
            // that lets a looping eat/drink kit clip free-run across the server's ~5 s kit
            // resends. Combat same-id re-plays never reach it (the fast-path above catches
            // them first — the client's head-of-function order).
            let overlay_live = drv.overlay.is_some_and(|ov| {
                ov.id == id && player.animation(ov.node).is_some_and(|a| !a.is_finished())
            });
            let base_live = matches!(drv.mode, Mode::Swing { id: m, .. } if m == id)
                && !oneshot_finished(&player, anims, id, catalog);
            if overlay_live || base_live {
                if crate::dbg_trace::enabled() {
                    crate::dbg_trace::line(
                        "fct",
                        &format!(
                            "anim dedup-eat unit={entity} id={id} overlay_live={overlay_live} base_live={base_live}"
                        ),
                    );
                }
                continue;
            }
            // Mounted forces the masked route (byte-verified, wow-re mount-composition B1:
            // upper-body one-shots route to the key-bone while mounted, and the 91-force would
            // reclaim bone 0 on the next play anyway): a full-body /wave would replace the seat
            // pose with a standing wave floating over the saddle.
            let masked =
                mounted || route_oneshot(id, mv.flags, mv.stand_state) == OneShotRoute::Masked;
            // The play's two rolls (decisions 0114/0117): resolve to what this model has, then
            // pick among that id's variations — the alternating swing arcs — and roll the replay
            // budget (a clamp one-shot authored `(min,max)` plays R times before releasing).
            let picked =
                find_resolved(anims, id, catalog).map(|h| roll_oneshot(anims, h, &mut rng));
            let upper = masked
                .then(|| picked.and_then(|(c, r)| c.upper_node.map(|n| (n, r))))
                .flatten();
            if let Some((node, repeat)) = upper {
                // Masked route: the SpineLow overlay, beside `mode`. The base machine runs untouched.
                // The re-arm is **blended** (op4 `blendFlag = 1`, decision 0878): whatever held the
                // key-bone retires into the fade slot and this clip rises over its own blendTime
                // (`0x7125f2` — the INCOMING sequence's `M2Sequence+0x20`), so a masked swing never
                // swaps the torso in one frame. Note the full-body branch below deliberately does
                // NOT touch the overlay: on that route the client's key-bone slot keeps its current
                // descriptor (a dedup no-op), so a standing emote over a still-running masked swing
                // plays legs-only underneath it (`0x5fe930`; wow-re claim 6 CONFIRMED).
                retire_overlay(
                    &mut drv,
                    &mut player,
                    picked.map_or(0.0, |(c, _)| c.blend_time.max(0.0)),
                );
                let active = player.play(node);
                active.replay();
                active.set_repeat(repeat); // always set — a reused node never keeps a stale count
                active.set_speed(1.0); // nor a stale rate (a prior whiff slow-down, decision 0279)
                active.set_weight(0.0); // the fade upkeep raises it from λ = 1 this same frame
                drv.overlay = Some(Overlay {
                    node,
                    id,
                    looping: false,
                });
                masked_played = true;
                played_oneshot = Some(id);
            } else {
                // Full-body route (standing idle / airborne non-combat), or the split-boneless
                // masked fallback (the client's −1 key-bone sentinel arms bone 0 too): the clip
                // replaces the base on bone 0 — **even over a Special**. The client never drops a
                // play: one slot, last-writer-wins (decisions 0083/0087; the wow-re §3 route puts
                // a jump-in-place cast/emote on bone 0, replacing the hang — decision 0864 is the
                // ref's mid-air cast). Cutting an airborne clip freezes the outgoing node first —
                // the pose-snapshot decay of the client's op4 blend, scoped exactly like
                // [`leave_special`]'s (decision 0503).
                if matches!(special, Some(select::Special::Jump | select::Special::Fall)) {
                    if let Some(active) = tr
                        .get_main_animation()
                        .and_then(|n| player.animation_mut(n))
                    {
                        active.set_speed(0.0);
                    }
                }
                if let Some((c, repeat)) = picked {
                    play_clip(&mut tr, &mut player, c, repeat, 1.0);
                }
                drv.mode = Mode::Swing { id, under: special };
                drv.gait = None;
                base_played = true;
                played_oneshot = Some(id);
            }
            if crate::dbg_trace::enabled() {
                crate::dbg_trace::line(
                    "fct",
                    &format!(
                        "anim play unit={entity} id={id} masked={masked_played} base={base_played} under={special:?}"
                    ),
                );
            }
        }

        // The whiff slow-down (decision 0279, `0x712910`): a miss/dodge/evade drops the attacker's
        // in-flight swing to HALF speed for its remainder — a slowed follow-through (the verified
        // 0.5 rate write), never a cut. The client writes bone 0's rate blindly; benilla scopes it
        // to the swing's own node (masked overlay or full-body main), so a moving attacker's gait
        // keeps its pace — a deliberate deviation, named in 0279. `Mode::Swing` holds ANY
        // full-body one-shot (a spell kit's Special1H rides it too), so the base arm re-checks
        // the id — a whiffing auto-attack must not drag a concurrent special to half speed.
        if pending_slow.contains(&entity) {
            let node = match drv.overlay {
                Some(ov) if is_swing_id(ov.id) => Some(ov.node),
                _ if matches!(drv.mode, Mode::Swing { id: m, .. } if is_swing_id(m)) => {
                    tr.get_main_animation()
                }
                _ => None,
            };
            if let Some(active) = node.and_then(|n| player.animation_mut(n)) {
                active.set_speed(0.5);
            }
        }

        // A Special EDGE is a play (the jump/pose entry, the FALLINGFAR latch's Fall, the land
        // pick — each a normal PlayAnimation), and every normal arm clears the deferred-combat
        // cache (the client's `0x5fe48e`): a swing parked behind the spin dies when a jump cuts
        // the spin. The *level* must not clear (decision 0864 corrects the old per-frame kill):
        // mid-air the airborne-freeze issues no plays, so a fast-path park made mid-arc
        // survives to its clip's end, exactly like the ref.
        let special_edge = special != drv.last_special;
        drv.last_special = special;
        if special_edge {
            drv.deferred = None;
        }
        // The looping-variation ADVANCE (decision 0516 — wow-re `loop-replay-fidget.md` §7/§7d,
        // the per-frame watchdog `0x719370`): every looping arm installed a window `R`
        // clip-lengths wide; when the armed node — still the MAIN animation (the client checks
        // the armed block: any newer arm superseded the window) — completes its `R` passes, the
        // completion callback re-arms the same id with `variationIdx = −1`: a fresh weighted,
        // MEMORYLESS pick with a fresh window. One id's weighted walk is what alternates a
        // gryphon's flap/glide (freq 26214/6553) and strings a five-part /dance together; a
        // single-variation loop just restarts — the wrap it was already playing. Units always
        // opt in (the client's `model+0x70` callback — every CGUnit controller installs it;
        // the callback-less map doodads, which loop one variation forever, are `doodad_anim`'s
        // separate concern). A combat/cast re-arm keeps the deterministic head, like any base
        // arm. The carried rate keeps the gait's scaling; the per-frame sync re-syncs it anyway.
        if let Some((node, budget)) = drv.loop_window {
            if tr.get_main_animation() == Some(node)
                && player
                    .animation(node)
                    .is_some_and(|a| a.completions() >= budget)
            {
                let head = anims
                    .clips
                    .iter()
                    .find(|c| c.node == node)
                    .and_then(|armed| find_resolved(anims, armed.anim_id, catalog));
                if let Some(head) = head {
                    let rate = player.animation(node).map_or(1.0, |a| a.speed());
                    let (c, fresh) = roll_loop(anims, head, relaxed, &mut rng);
                    play_clip(
                        &mut tr,
                        &mut player,
                        c,
                        bevy::animation::RepeatAnimation::Forever,
                        rate,
                    );
                    drv.loop_window = Some((c.node, fresh));
                }
            }
        }
        match drv.mode {
            Mode::Entering(sp) => {
                // The swim re-latch does NOT cut the hop's kick: JumpStart PLAYS OUT over the
                // re-latch and the swim gait resumes only at its end (decision 0517 —
                // director-corrected against the ref; the §5's static cut-at-relatch law could
                // not reproduce the screen and is flagged wow-re-side for a live capture).
                // Only the swim re-latch holds — a ground landing, a water exit, or a new
                // Special still cuts (0503's snapshot-freeze).
                let swim_relatch_hold = sp == select::Special::Jump
                    && special.is_none()
                    && mv.flags & move_flags::SWIMMING != 0
                    && !oneshot_finished(&player, anims, sp.enter(), catalog);
                if swim_relatch_hold {
                    // Hold: the kick keeps playing; the gait recompute waits at its end.
                } else if special != Some(sp) {
                    // What we're entering is no longer wanted before the enter even finished — preempt
                    // to the new Special, to the gait (a pose cut by movement), or to this one's exit.
                    drv.mode = leave_special(
                        sp,
                        special,
                        moving,
                        relaxed,
                        mv.flags,
                        &mut tr,
                        &mut player,
                        anims,
                        catalog,
                        &mut rng,
                        &mut drv.loop_window,
                    );
                } else if oneshot_finished(&player, anims, sp.enter(), catalog) {
                    play(
                        &mut tr,
                        &mut player,
                        anims,
                        sp.loop_id(),
                        true,
                        relaxed,
                        1.0,
                        catalog,
                        &mut rng,
                        &mut drv.loop_window,
                    );
                    drv.mode = Mode::Looping(sp);
                }
            }
            Mode::Looping(sp) => {
                if special != Some(sp) {
                    drv.mode = leave_special(
                        sp,
                        special,
                        moving,
                        relaxed,
                        mv.flags,
                        &mut tr,
                        &mut player,
                        anims,
                        catalog,
                        &mut rng,
                        &mut drv.loop_window,
                    );
                }
            }
            Mode::Land { id, flags } => {
                // The jump landing (39/187) as a freely-overwritten pick (decisions 0083/0087 (d)).
                if let Some(sp) = special {
                    // A new jump or a pose interrupts (a second jump, or sitting right after landing).
                    drv.mode = enter_special(
                        sp,
                        relaxed,
                        &mut tr,
                        &mut player,
                        anims,
                        catalog,
                        &mut rng,
                        &mut drv.loop_window,
                    );
                } else if mv.flags != flags {
                    // Any movement-flag change re-picks from live state *immediately* — land-then-press
                    // runs, land-then-release stands, a direction flip drops the stale-direction land.
                    drv.mode = Mode::Gait;
                    drv.gait = None;
                } else if oneshot_finished(&player, anims, id, catalog) {
                    // Input held steady through the whole landing: fall through to a fresh gait pick.
                    drv.mode = Mode::Gait;
                    drv.gait = None;
                }
            }
            Mode::Exiting(sp, exit) => {
                // Pose stand-ups only now (Jump lands via `Mode::Land`, not this bracket).
                if let Some(next) = special {
                    // A new Special interrupts the exit — re-sitting during the stand-up, say. Enter
                    // it straight away instead of waiting the stand-up out.
                    drv.mode = enter_special(
                        next,
                        relaxed,
                        &mut tr,
                        &mut player,
                        anims,
                        catalog,
                        &mut rng,
                        &mut drv.loop_window,
                    );
                } else if sp.interruptible_by_move() && moving {
                    // Started moving mid stand-up: drop the rest, let the gait cross-fade take over.
                    drv.mode = Mode::Gait;
                    drv.gait = None;
                } else if oneshot_finished(&player, anims, exit, catalog) {
                    drv.mode = Mode::Gait;
                    drv.gait = None; // recompute a fresh gait next frame
                }
            }
            Mode::Swing { id, under } => {
                if special != under {
                    // The state this one-shot replaced CHANGED — the client's next event play
                    // supersedes it: a fresh jump/pose entry (`None → Some`), the FALLINGFAR
                    // latch's Fall (`Jump → Fall`), the `0x602c60` land pick at touchdown
                    // (`Some → None`) — each a plain PlayAnimation over bone 0, never a
                    // "restore". Leaving an airborne/pose `under` routes through
                    // [`leave_special`] exactly like the un-replaced machine: the latch
                    // handoff, the landing pick, and the pose exits all apply unchanged.
                    //
                    // …but FIRST the **transplant** (decision 0878): when what the base is about
                    // to play is a LOCOMOTION clip — a jump entry (37), a land pick (39/187) — and
                    // this one-shot is a live CAST/COMBAT clip, the client moves it up onto the
                    // key-bone rather than letting the request overwrite it. Fall(40) and the pose
                    // enters/exits are NOT locomotion ids, so a FALLINGFAR latch or a sit-down
                    // still replaces the clip on bone 0, exactly as the bytes order it.
                    let incoming_locomotion = match (under, special) {
                        (_, Some(next)) => select::is_locomotion(next.enter()),
                        (Some(select::Special::Jump | select::Special::Fall), None) => {
                            select::jump_land_pick(mv.flags).is_some_and(select::is_locomotion)
                        }
                        _ => false,
                    };
                    if incoming_locomotion {
                        transplant_up(&mut drv, &mut player, &tr, anims, entity, id);
                    }
                    drv.mode = if let Some(sp) = under {
                        leave_special(
                            sp,
                            special,
                            moving,
                            relaxed,
                            mv.flags,
                            &mut tr,
                            &mut player,
                            anims,
                            catalog,
                            &mut rng,
                            &mut drv.loop_window,
                        )
                    } else if let Some(sp) = special {
                        enter_special(
                            sp,
                            relaxed,
                            &mut tr,
                            &mut player,
                            anims,
                            catalog,
                            &mut rng,
                            &mut drv.loop_window,
                        )
                    } else {
                        Mode::Gait // unreachable: special != under with under None ⇒ special Some
                    };
                } else if matches!(under, Some(select::Special::Jump | select::Special::Fall)) {
                    // The airborne-freeze (`0x5fd8e8` keep-current): mid-arc nothing re-picks
                    // bone 0 — a finished clip clamps and holds its last frame for the rest of
                    // the arc (the §6 clamp path), and a mid-air flag change is a keep-current
                    // no-op. The only exits are the edges above: the FALLINGFAR latch's Fall
                    // and the land pick at touchdown. This holds over Fall too: 0864's per-tick
                    // Fall(40) re-assert was §5-REFUTED (decision 0868 — Fall plays ONCE, at the
                    // latch edge `0x61a820@0x61a9eb`; `0x5ff030` is a wire-apply path, not a
                    // tick), so a clip that takes bone 0 after the latch holds until landing.
                } else if let Some(sp) = under {
                    // A pose held under the one-shot: on finish, back to the pose LOOP directly
                    // (decision 0083 (c) — the enter never replays after an interruption).
                    if oneshot_finished(&player, anims, id, catalog) {
                        play(
                            &mut tr,
                            &mut player,
                            anims,
                            sp.loop_id(),
                            true,
                            relaxed,
                            1.0,
                            catalog,
                            &mut rng,
                            &mut drv.loop_window,
                        );
                        drv.mode = Mode::Looping(sp);
                    }
                } else if oneshot_finished(&player, anims, id, catalog)
                    || mv.flags != drv.gait_flags
                {
                    // Finished — or a movement-flag change: the client's base re-arm lands on the
                    // change and blindly overwrites bone 0, one-shot or not (the same re-arm
                    // decision 0280 named for the un-finishable looping kit clip, and
                    // Mode::Land's flag-change re-pick). Holding the clip out instead slides the
                    // post-shot runner over the ground on straight legs (director-observed vs
                    // ref). An EDGE, not a level — the split-boneless masked fallback enters
                    // here already moving, and steady flags must let that clip play out.
                    //
                    // Against `drv.gait_flags` — what the **base** was last armed for — not this
                    // one-shot's own arm-time `flags` (decision 0894). The reference keeps no
                    // per-one-shot latch: a movement-state change requests the base and bone 0
                    // takes it, whenever the one-shot happened to start. Ice Block is the case that
                    // separates them — its root wipes the direction bits in the SAME frame the cast
                    // one-shot arrives, so the arm-time compare sees no edge ever and the cast held
                    // bone 0 for the whole block; against the base's flags the edge is still there,
                    // Stand overwrites the cast, and the character is neutral when the freeze lands.
                    if mv.flags != drv.gait_flags {
                        // The locomotion re-arm is a normal PlayAnimation — the deferred-combat
                        // cache clears with it (decision 0406; a finished clip instead had its
                        // cache consumed by the injection above, before this machine ran).
                        drv.deferred = None;
                        // …and **if** the re-arm resolves to a LOCOMOTION id, a still-playing
                        // CAST/COMBAT clip transplants up to the key-bone instead of being
                        // overwritten (decision 0878; the client's gate is `0x5fee80` on the
                        // *requested* id, `0x5fe912`). A *finished* clip has nothing to move: the
                        // client's descriptor probe reports a completed slot as id −1 (`0x5fe1f0`
                        // reads the completion latch, not the armed record), so the transplant
                        // predicates never see it.
                        //
                        // The gate was missing here, and Ice Block is what it costs (decision
                        // 0894): a stun's root wipes the direction bits, so the flag change re-arms
                        // to **Stand(0)** — not locomotion — and the reference *overwrites* the
                        // cast on bone 0, leaving the character fully neutral for the freeze to
                        // catch. Transplanting unconditionally moved it to the torso instead and
                        // froze an arm out.
                        if select::gait_is_locomotion(&mv, walk) {
                            transplant_up(&mut drv, &mut player, &tr, anims, entity, id);
                        }
                    }
                    drv.mode = Mode::Gait;
                    drv.gait = None; // recompute a fresh gait next frame
                }
            }
            Mode::Gait => {
                if let Some(sp) = special {
                    // Enter a Special state.
                    drv.mode = enter_special(
                        sp,
                        relaxed,
                        &mut tr,
                        &mut player,
                        anims,
                        catalog,
                        &mut rng,
                        &mut drv.loop_window,
                    );
                    drv.gait = None;
                } else if airborne_frozen && drv.gait.is_some_and(|g| g != DEATH) {
                    // The airborne-freeze on the STEP-OFF arc — the exact §5-verified gate
                    // (`0x5fd8e8`: `FALLING && (FALLINGFAR || vz ≠ 0)`, decisions 0864/0868;
                    // the selector chain's leg right after death): mid-air the selector never
                    // re-picks, so the takeoff-frozen gait keeps rolling mid-cycle AND the live
                    // pins further down the chain (the stationary cast hold, the loot kneel, the
                    // Ready/ranged idles, the state-emote idle) cannot swap the clip until
                    // touchdown. Rate stays synced — the client's per-frame rate write is outside
                    // the selector. (DEATH is excluded: the dead-override owns that gait, and a
                    // mid-air revive must re-select, not hold the corpse pose.)
                    if let Some(c) = drv.gait.and_then(|g| find_resolved(anims, g, catalog)) {
                        for v in anims.clips.iter().filter(|v| v.anim_id == c.anim_id) {
                            let rate = playback_rate(v, mv.speed, model_scale);
                            if let Some(active) = player.animation_mut(v.node) {
                                active.set_speed(rate);
                                drv.gait_rate = rate;
                            }
                        }
                    }
                } else {
                    // A bracket-less step-off fall landing needs NO case of its own: the arc never
                    // latched FALLINGFAR, so the `0x602c60` land dispatcher is a verified no-op
                    // (decision 0179) — and a no-op means the gait must keep rolling mid-cycle,
                    // not be re-picked (a re-pick replays the run cycle from its head: the
                    // landing-frame leg pop, decision 0187). Falling through keeps the clip when
                    // the flags still agree and cross-fades normally when they changed mid-air.
                    // Normal gait: select, cross-fade on change, keep the rate synced each frame.
                    // The engaged standing idle: the weapon-class Ready pick (decision 0073).
                    let ready =
                        (engaged && !moving).then(|| ready_anim(wielded.and_then(|w| w.main)));
                    // The ranged standing idle (0099 phase 5): sheath ranged + either armed
                    // bit → the ranged weapon's Load/Hold clip. `auto_repeat` is the local
                    // `0x200` (the resolver `0x5fd460`'s own gate); `ranged_hold` is the
                    // any-caster `0x400` weapon-visual hold — what puts a REMOTE shooter (an
                    // NPC archer, another hunter) in the drawn idle between shots (HoldBow
                    // sustains on `0x200|0x400`, rifle/thrown on `0x400`; the per-weapon
                    // asymmetry is folded into this one gate — no benilla-visible difference).
                    // After one full pass of the Load clip the idle swaps to the drawn HOLD
                    // twin — nock once, then HOLD through every shot: a mid-volley fire clip
                    // returns straight to the hold, never a full re-pull (director-refuted;
                    // decision 0409). The pull replays only when the idle is LEFT and
                    // re-entered (volley start, movement, sheath change — the reset at this
                    // arm's tail). Named deviation: a remote's post-movement re-entry replays
                    // the pull like the local player's, where the byte corollary (the
                    // `0x200`-gated resolver) would leave a remote in plain Stand — unobserved
                    // on the ref, and the derivation rests on the LOAD-ANIM-RECOMPUTE-MODE
                    // open item; revisit if the ref's remote hunters visibly differ.
                    let ranged_load =
                        ((auto_repeat || ranged_hold) && !moving && drv.sheath_cur == Some(2))
                            .then(|| select::ranged_load_anim(wielded.and_then(|w| w.ranged)))
                            .map(|id| {
                                if drv.ranged_held {
                                    select::ranged_hold_anim(id)
                                } else {
                                    id
                                }
                            });
                    let cands = gait_candidates(&mv, walk, ready, ranged_load);
                    // The stationary cast/channel hold pins its pose **full-body in the gait slot**
                    // (decision 0107 — the client's `[CGUnit+0xb4]` stationary-cast gate),
                    // outranking the Ready idle and the state-emote idle below. "Stationary" is
                    // the client's `[9e8] & 0x20000f` test ([`move_flags::CAST_PIN_MOVE`]:
                    // translation + swim, NEVER the turn bits) — a turning caster keeps the pin,
                    // feet sliding; only a translating/swimming one falls through to the masked
                    // hold overlay (the hold block after the mode machine). Testing the turn bits
                    // here was the frostbolt right-drag jitter (decision 0491).
                    let hold_cands;
                    let cands: &[u16] = match cast_hold {
                        Some(h) if mv.flags & move_flags::CAST_PIN_MOVE == 0 => {
                            hold_cands = [h.anim_id, STAND];
                            &hold_cands
                        }
                        _ => cands,
                    };
                    // The looping state-emote idle (`UNIT_NPC_EMOTESTATE`: `/dance`, NPC
                    // cooking/sweeping flavor loops): fills exactly the bare-Stand slot
                    // (`is_bare_stand`) — everything that already outranks Stand (movement, turn,
                    // swim, the Ready idle, a chair-loop stand-state) has already routed `cands`
                    // elsewhere, and Special is handled entirely above this arm. Cleared field
                    // (`unit_emote_state() == 0`, or the catalog has no `AnimID` for it) falls
                    // straight through to `cands` unchanged — back to Stand.
                    let state_emote_cands;
                    let cands: &[u16] = if is_bare_stand(cands) {
                        let emote_anim = store.and_then(|s| {
                            emote_sounds
                                .as_deref()
                                .and_then(|e| e.anim(s.0.unit_emote_state()))
                        });
                        match emote_anim {
                            Some(id) => {
                                state_emote_cands = state_emote_gait(id as u16);
                                &state_emote_cands
                            }
                            None => cands,
                        }
                    } else {
                        cands
                    };
                    // The loot kneel (see [`select::LOOT`]): while the loot trigger is up
                    // (self: the latch; remote: the flag — the `looting` predicate above) on a
                    // stationary, unmounted unit, the gait slot holds Loot 50 — over the cast
                    // pin, the Ready/ranged idles, the chair loops and the state-emote idle
                    // alike (the `0x5fd8b0` chain order, §5-verified: locomotion → LOOT →
                    // standState → combat/channel). The trigger dropping cross-fades back to
                    // whatever the slot picks next.
                    let loot_cands;
                    let cands: &[u16] = if looting {
                        loot_cands = [select::LOOT, STAND];
                        &loot_cands
                    } else {
                        cands
                    };
                    // The mounted pin outranks the whole gait slot (decision 0442 confirms 0441's
                    // B1): the rider holds Mount(91) — moving, turning, engaged — while the mount
                    // child's own driver plays the locomotion this selector would have picked. The
                    // real client has no mount leg in this chain at all — it arms 91 once at
                    // attach and re-forces it on every PlayAnimation instead of selecting it here;
                    // our gait-slot pin renders the identical result without modeling the
                    // re-force. 91 is not rate-scaled, and the resolver's Stand fallback covers a
                    // body that doesn't author it.
                    let mount_cands;
                    let cands: &[u16] = if mounted {
                        mount_cands = [select::MOUNT, STAND];
                        &mount_cands
                    } else {
                        cands
                    };
                    let target = cands[0];
                    // Each RF-0057 candidate, in priority order, resolved through the model's own
                    // baked fallback (decision 0082) before moving to the next candidate — a model
                    // missing the exact id still plays its baked substitute rather than stepping
                    // down the selector's own list early. The state-emote idle's id (above) resolves
                    // through the same call, like every other candidate.
                    let clip = cands
                        .iter()
                        .find_map(|&id| find_resolved(anims, id, catalog));
                    if drv.gait == Some(target) {
                        if let Some(c) = clip {
                            // The armed clip may be a *rolled variation* of the resolved head
                            // (decision 0123) — sync the rate on whichever variation node is
                            // live (a stale fade-out node getting the same rate is harmless).
                            for v in anims.clips.iter().filter(|v| v.anim_id == c.anim_id) {
                                let rate = playback_rate(v, mv.speed, model_scale);
                                if let Some(active) = player.animation_mut(v.node) {
                                    active.set_speed(rate);
                                    drv.gait_rate = rate;
                                }
                            }
                            // One completed pass of the ranged Load clip arms the HOLD twin
                            // (the `ranged_load` map above) — next frame the idle cross-fades
                            // to the drawn hold instead of replaying the nock.
                            if select::is_ranged_load(target)
                                && anims
                                    .clips
                                    .iter()
                                    .filter(|v| v.anim_id == c.anim_id)
                                    .any(|v| {
                                        player
                                            .animation(v.node)
                                            .is_some_and(|a| a.completions() >= 1)
                                    })
                            {
                                drv.ranged_held = true;
                            }
                        }
                    } else if let Some(c) = clip {
                        // A looping base arm rolls its variation when relaxed (decision 0123 —
                        // the client's base-arm `variationIdx = −1`; a combat/cast arm keeps the
                        // deterministic head) AND its replay budget (decision 0516 §7d — the
                        // watchdog window). A re-armed Stand landing on its rare look-around
                        // variations IS the idle fidget.
                        let (c, budget) = roll_loop(anims, c, relaxed, &mut rng);
                        if is_self && crate::dbg_trace::enabled() {
                            // Every fresh gait play, including a same-clip replay (which the
                            // settled-state diff below cannot see) — the exact restart-from-head
                            // event a "frames snap" report is hunting.
                            crate::dbg_trace::line(
                                "anim",
                                &format!(
                                    "PLAY gait {} (was {:?}) rate {:.2}",
                                    c.anim_id,
                                    drv.gait,
                                    playback_rate(c, mv.speed, model_scale)
                                ),
                            );
                        }
                        // The ranged Load plays ONCE and freezes at full draw: as a Forever
                        // gait it WRAPPED to its head at completion — the frames + cross-fade
                        // against the restarted reach-to-quiver were the director's "jumps
                        // back to the start the moment it gets fully pulled", in every build
                        // that replayed a pull (trace-caught, decision 0412). The promotion
                        // then cross-fades the hold from the matching drawn pose.
                        // Loot 50 is likewise authored clamp — one 0.5 s kneel-down that must
                        // FREEZE in the rummage pose; as Forever it would wrap back to standing
                        // and re-kneel every half second.
                        let repeat = if select::is_ranged_load(target) || target == select::LOOT {
                            // A deliberate freeze — no window either: the client's ranged
                            // hand-off is the HOLD promotion, not a watchdog re-pull.
                            drv.loop_window = None;
                            bevy::animation::RepeatAnimation::Never
                        } else {
                            drv.loop_window = Some((c.node, budget));
                            bevy::animation::RepeatAnimation::Forever
                        };
                        drv.gait_rate = playback_rate(c, mv.speed, model_scale);
                        play_clip(&mut tr, &mut player, c, repeat, drv.gait_rate);
                        drv.gait = Some(target);
                        // A fresh pick of the drawn HOLD re-nocks (INTERIM, decision 0409): the
                        // hold resuming after a fire clip (or after the Load promotion) shows
                        // the arrow + drawn string again without a full re-pull — the `$BWP`
                        // re-latch only lives in the Load clip our mid-volley cycle no longer
                        // replays. The ref's exact mid-volley re-attach source is the Q-I
                        // dispatch; the arrow itself still gates on a cached NockedAmmo.
                        if drv.ranged_held && ranged_load == Some(target) {
                            commands.entity(entity).insert(super::NockLatch);
                        }
                    } else {
                        drv.gait = Some(target); // no clip (bind pose) — record the target so we don't churn
                    }
                    // The base is now arm-consistent with this movement state — every branch above
                    // leaves `drv.gait == Some(target)`. A one-shot that displaces it later reads
                    // this, not its own arm-time flags, to know whether the movement state has
                    // moved on since (decision 0894).
                    drv.gait_flags = mv.flags;
                    // Leaving the ranged idle (moving, sheath change, cancel) drops the hold —
                    // the next shooting session opens with a fresh nock.
                    if ranged_load.is_none() {
                        drv.ranged_held = false;
                    }
                }
            }
        }

        // ── Masked one-shot completion — the **fade-to-rest** (decision 0878, correcting 0087 (c)):
        // a finished overlay is NOT stopped. The client latches the completion in its per-frame
        // model advance (`0x719370`) and delivers a deferred event the same frame to
        // `CGUnit::OnAnimationFinished 0x5fc920`, which disarms the key-bone through op4
        // `param_3 = -1` — snapshotting the clip's **held final frame** into the bone's secondary
        // slot and cross-fading it back onto the inherited base over a fixed 150 ms. So the torso's
        // return is a blend out of the follow-through, never the one-frame cut this used to do
        // (the director's "the end of the cast is cut off and it instantly snaps back"). A looping
        // cast-hold overlay never completes — the hold block below owns its release.
        if let Some(ov) = drv.overlay {
            let done = if ov.looping {
                false // a cast-hold loop: the hold block below owns its release
            } else if find_resolved(anims, ov.id, catalog).is_some_and(|c| c.looping) {
                // A hold-less overlay whose CLIP is authored looping (a pushed kit's seated
                // eat/drink gesture) has no natural end — release it when the body is claimed by
                // MOVEMENT. INTERIM (decision 0280): the client's exact release chain for this case
                // is unpinned; supersede-by-a-later-play already evicts above. A **Special** no
                // longer counts (decision 0878): a jump is a bone-0 play, and bone-0 plays never
                // touch the key-bone slot — cutting the torso on takeoff was the same defect the
                // fade above fixes at the other end.
                moving
            } else {
                player.animation(ov.node).is_none_or(|a| a.is_finished())
            };
            if done {
                // Freeze the held frame, then retire it: the client's snapshot copies an already
                // *expired* window, so the secondary clamps to `seq.end` and the fade runs from
                // the clip's final pose (`oneshot-lifecycle.md` §5.4).
                if let Some(a) = player.animation_mut(ov.node) {
                    a.set_speed(0.0);
                }
                retire_overlay(&mut drv, &mut player, ONESHOT_RELEASE_FADE);
            }
        }

        // ── The committed-move cast hold (decision 0107): a casting/channeling unit that
        // translates or swims loops the hold clip on its torso over the gait — the masked route a
        // moving caster falls through to (the stationary case pinned the gait slot above; the
        // split is the same `[9e8] & 0x20000f` gate, [`move_flags::CAST_PIN_MOVE`] — a merely
        // TURNING caster stays pinned there, decision 0491). A
        // masked one-shot in the slot (a swing over the run) wins while it plays; the hold
        // re-takes the subtree the frame after it finishes (the client's last-writer-wins slot).
        // Its start is a play like any other, so the sheath reconcile below sees it (`hold_played`).
        // A **Special no longer cancels it** (decision 0878): jumping mid-cast is a bone-0 play
        // (JumpStart takes the legs) and the client's key-bone slot is untouched by it —
        // `0x5fe912` routes a locomotion request straight to bone 0 when the key-bone is armed.
        // Dropping the hold on takeoff was exactly the director's "jump-running cuts the cast".
        let mut hold_played: Option<u16> = None;
        let masked_hold = cast_hold
            .filter(|_| mv.flags & move_flags::CAST_PIN_MOVE != 0)
            .and_then(|h| {
                find_resolved(anims, h.anim_id, catalog).and_then(|c| {
                    c.upper_node
                        .map(|node| (h.anim_id, node, c.blend_time.max(0.0)))
                })
            });
        match (masked_hold, drv.overlay) {
            (Some((id, node, blend)), prior)
                if prior.is_none_or(|ov| ov.looping && ov.id != id) =>
            {
                // Free slot, or a stale hold loop (the channel switched spells): (re)take it — a
                // blended key-bone re-arm like any other (decision 0878), so the outgoing pose
                // retires under the hold's own blendTime instead of vanishing.
                retire_overlay(&mut drv, &mut player, blend);
                let active = player.play(node);
                active.replay();
                active.repeat();
                active.set_weight(0.0); // the fade upkeep raises it from λ = 1 this same frame
                drv.overlay = Some(Overlay {
                    node,
                    id,
                    looping: true,
                });
                hold_played = Some(id);
            }
            (None, Some(ov)) if ov.looping => {
                // The hold ended, or the unit stopped — release the subtree (the standing pin or
                // the gait takes over). A looping clip never completes, so the client's
                // completion-driven release never fires for it and its true release chain is
                // still unpinned (0107/0280 INTERIM); it borrows the verified fade-to-rest shape
                // rather than snapping, which is right either way.
                retire_overlay(&mut drv, &mut player, ONESHOT_RELEASE_FADE);
            }
            _ => {}
        }
        // The key-bone cross-fade's advance, last — after every arm/release this frame, so the
        // weights land on the slot's settled state (the client's kernel likewise runs λ after the
        // frame's PlayAnimation calls).
        overlay_fade_upkeep(&mut drv, &mut player, dt);

        // The anim half of the `WOW_MOVE_TRACE` debug trace ([`crate::dbg_trace`]): one line per
        // frame the SELF unit's settled anim state *changes* — mode, gait, Special, flags, the
        // rate-driving speed, and **both slots** — interleaved with the mover's `move` lines on the
        // same clock, so a feel report ("the char's frames snap on landing") pins which layer moved
        // and when. It runs HERE, after every arm/release/fade this frame, so `upper` is the
        // settled key-bone state: without it a torso report ("the cast got cut") could not be told
        // from a base one at all (decision 0878). `+fade` marks the 150 ms fade-to-rest running —
        // a boolean, not a countdown, so a fade doesn't spam a line per frame.
        if is_self && crate::dbg_trace::enabled() {
            let state = format!(
                "mode={:?} gait={:?} special={:?} flags={:08x} speed={:.2} scale={:.2} \
                 rate={:.2} upper={:?}{}",
                drv.mode,
                drv.gait,
                special,
                mv.flags,
                mv.speed,
                // The rate's new input and its result (decision 0903): `speed` alone stopped
                // predicting the gait's playback the moment the model scale joined the divisor,
                // so a "the gait looks wrong" trace carrying only speed could no longer answer it.
                model_scale,
                drv.gait_rate,
                drv.overlay.map(|ov| ov.id),
                if drv.overlay_fade.is_some() {
                    "+fade"
                } else {
                    ""
                },
            );
            if anim_trace_last.as_deref() != Some(&state) {
                crate::dbg_trace::line("anim", &state);
                *anim_trace_last = Some(state);
            }
        }

        let masked_played = masked_played || hold_played.is_some();

        let base_played = base_played || (drv.mode, drv.gait) != pre_state;
        wound_evict(&mut drv, &mut player, masked_played, base_played);

        if let Some(&edge) = pending_wound.get(&entity) {
            // A melee edge picks its wound id by severity + the victim's engagement (decision
            // 0111); a spell edge arrives with the kit's own id (the client passes it through).
            let id = match edge {
                WoundEdge::Melee(hit_info) => select::wound_anim(hit_info, engaged),
                WoundEdge::Spell(anim_id) => anim_id,
            };
            wound_trigger(
                &mut drv,
                &mut player,
                anims,
                catalog,
                &mut rng,
                id,
                &mv,
                mounted,
            );
        }

        // A one-shot during the armed ranged idle latches the hold — the byte law (wow-re
        // Q-I, decision 0412): mid-volley the client plays the FULL fire clip (whose tail
        // is the visible quick re-nock) and returns to the hold; it NEVER replays the Load
        // and NEVER rate-scales a bow clip (the `0x5fee80` scaler gate excludes every bow
        // id). 0411's RENOCK_RATE replay is refuted and removed. `ranged_hold` joins the
        // gate so a REMOTE shooter's fire clip returns to its hold the same way.
        if played_oneshot.is_some() && (auto_repeat || ranged_hold) && drv.sheath_cur == Some(2) {
            drv.ranged_held = true;
        }

        // ── The per-animation sheath reconcile (decision 0080 structure 3 — the client's
        // `0x5fdf80`, run after every animation pick). The playing clip's `AnimationData.dbc`
        // WeaponFlags + engagement force the state, data-driven — swimming/looting/casting stow,
        // punching stows (fists need empty hands), armed attacks and the Ready idles draw — and
        // a remote unit with no force pulls back to the server byte. Every force is a SNAP; the
        // local player's force volunteers `CMSG_SETSHEATHED` (`bFireEvent = 1`), which the
        // server echoes into the descriptor for observers. This is why a vendor gossip stows:
        // the talk emote's flags, not a vendor wire (0080's resolved mystery) — future systems
        // (gossip, loot, mounts) inherit the policy with zero new sheath wiring.
        // The reconcile tests the **requested** id, not the model-resolved substitute (decision
        // 0125, director-eyes falsification of 0082's resolved-id reading): the client's arm
        // descriptor carries the id that was ASKED FOR (the cast-override §2.1 writes the kit's
        // own anim id even on a model lacking the sequence), and `0x5fdb50` reads that
        // descriptor. The two differ only when a model lacks the requested clip — the ground
        // truth is GnollCaster.m2: no ReadySpellDirected(51) at all, its hold falls back to a
        // flags-less Stand for *playback*, yet the ref still stows the staff for the whole
        // windup — only 51's own `&4` row can do that. (0082's chicken rationale collapses under
        // the same lens: requested Attack2H's 0x20 vs resolved AttackUnarmed's 0x10 is
        // unobservable on a weaponless chicken.) Byte arbitration of `0x5fdb50`'s miss path is
        // dispatched to wow-re; the director's eyes outrank the old reading meanwhile.
        // The reconcile must see **every play** (decision 0087 (d)): the client's `0x5fdf80` runs
        // inside `PlayAnimation` itself, so a *masked* one-shot — which never touched `mode`, so
        // `active_anim()` still reports the base gait — is reconciled all the same. A masked emote
        // carrying WeaponFlags `0x10` still stows. So this frame's routed one-shot id (either route)
        // takes precedence for the reconcile; absent one, the base track's playing id is tested.
        // And it must see **only plays** (the same byte-fact's other half): `PlayAnimation` is the
        // reconcile's *sole* trigger, so a frame where nothing played keeps the committed state
        // untouched. Re-running it every frame against the base track was the caster staff bug: a
        // masked cast hold stows on its retake play, then the very next frame the base gait
        // (flags-less Stand/Run) + the engaged/server-byte re-assert yanked the weapon back out —
        // the client instead holds the stow across the base loop's silent wraps until something
        // actually plays. The server-byte adopt above needs no play, exactly like the client's
        // field-change watcher (`0x604c70`).
        // …and, beyond plays: a mounted unit whose committed state reads drawn anyway (the
        // descriptor byte-adopt above is the one write the setter gate can't refuse) re-arms
        // the reconcile directly — the client's recompute would run on the composite's next
        // mount-gait play, a beat our silent rider track never gets. Forces once (the mounted
        // branch yields 0, cur becomes 0, the re-arm goes quiet) — no every-frame oscillation
        // (the caster-staff trap below).
        if played_oneshot.is_some()
            || hold_played.is_some()
            || base_played
            || (mounted && drv.sheath_cur.unwrap_or(0) != 0)
        {
            let cur = drv.sheath_cur.unwrap_or(0);
            let anim = played_oneshot
                .or(hold_played)
                .or_else(|| drv.active_anim())
                .unwrap_or(STAND);
            let flags = catalog.map_or(0, |cat| cat.weapon_flags(anim));
            if let Some(forced) =
                select::reconcile_sheath(cur, anim, flags, engaged, is_self, sheath_byte, mounted)
            {
                // The ranged wind-up **bracket** (wow-re `ranged-sheath-exempt-autorepeat.md`,
                // Q1's ordering law): the client surrounds every ranged kit play with ranged
                // snaps — `0x60f34c` before the play, the outer send/START `SetSheatheState
                // (2,1,1)` after — so ReadyThrown 108's genuine force-stow (NOT in the `0x5fe180`
                // exempt set) exists but never renders: the snap always wins the frame. Our
                // frame-spread ECS can't reproduce the call-stack order, so the bracket is
                // stated directly: while a ranged shot's precast hold is live, a reconcile
                // stow resolves to the drawn-ranged state instead. (Named deviation: the real
                // *remote* thrower's wind-up ends CUR=0 until its GO — a derived, unobserved
                // corollary; we keep every caster at 2, the observable the director verified.)
                let forced = if forced != 2 && cast_hold.is_some_and(|h| h.ranged) {
                    2
                } else {
                    forced
                };
                if forced != cur {
                    debug!(
                        "sheath: unit {entity} {cur} -> {forced} (anim {anim} flags {flags:#x}, \
                         engaged {engaged})"
                    );
                    drv.sheath_cur = Some(forced);
                    if is_self {
                        let _ = net.0.send(ClientCommand::SetSheathed {
                            state: u32::from(forced),
                        });
                    }
                }
            }
        }
        // Leaving the ranged sheath stance un-nocks (the client's `0x60fc72` un-nock, gated
        // `[+0xd40] != 2`; wow-re `nocked-ammo-cancel.md`): however the state left 2 this frame
        // — a stow/melee request, the reconcile, the server byte — the ammo model drops.
        // Re-drawing shows no ammo until the next `SMSG_SPELL_START` re-affirms it.
        if sheath_frame_start == Some(2) && drv.sheath_cur != Some(2) {
            commands
                .entity(entity)
                .remove::<(super::NockedAmmo, super::NockLatch)>();
        }
    }
}
