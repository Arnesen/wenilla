//! Playback primitives for [`super::drive_animations`]: resolving and cross-fading into a clip
//! ([`play_clip`], [`play`]), checking whether a one-shot has finished ([`oneshot_finished`]), and
//! leaving a Special state ([`leave_special`]) — split out of [`super`] as its own concern.

use std::time::Duration;

use benilla_assets::{AnimClip, ModelAnimations};
use benilla_formats::AnimDataCatalog;
use bevy::animation::transition::AnimationTransitions;
use bevy::animation::RepeatAnimation;
use bevy::prelude::*;

use super::super::find_resolved;
use super::select::{self, jump_land_pick, Mode, Special};

/// Cross-fade the player into an **already-resolved** clip over its blend-in time. `repeat` sets its
/// repetition (`Forever` for a loop, `Count(R)` for a rolled replay budget, `Never` for a plain
/// one-shot — always set, so a reused graph node never carries a stale count from a prior play);
/// `rate` sets its playback speed. The primitive [`play`] (and the gait picker, which already has
/// the resolved [`AnimClip`] in hand) both funnel through this so resolution never runs twice.
pub(super) fn play_clip(
    tr: &mut AnimationTransitions,
    player: &mut AnimationPlayer,
    c: &AnimClip,
    repeat: RepeatAnimation,
    rate: f32,
) {
    let active = tr.play(
        player,
        c.node,
        Duration::from_secs_f32(c.blend_time.max(0.0)),
    );
    active.set_repeat(repeat);
    active.set_speed(rate);
}

/// The one-shot arm's two rolls, in the client's order (op4: variation at `0x71249a`, replay count
/// at `0x712698` — decision 0117): pick the resolved id's **variation** (the `_rand()`-weighted
/// walk, alternating the 1H swing arcs), then roll the **replay budget** `R` from the picked
/// clip's `(minReplay, maxReplay)` — the client multiplies `R` into the play window; benilla
/// expresses the same window as a `Count(R)` repeat, which `is_finished` honors.
pub(super) fn roll_oneshot<'a>(
    anims: &'a ModelAnimations,
    head: &'a AnimClip,
    rng: &mut u32,
) -> (&'a AnimClip, RepeatAnimation) {
    let c = anims
        .pick_variation(head.anim_id, select::msvc_rand(rng))
        .unwrap_or(head);
    let repeat = match select::replay_count(c.replay, select::msvc_rand(rng)) {
        r if r > 1 => RepeatAnimation::Count(r),
        _ => RepeatAnimation::Never,
    };
    (c, repeat)
}

/// Pick a looping arm's **variation** (decision 0123 — the client's base-arm `variationIdx = −1`,
/// wow-re `loop-replay-fidget.md` §5b): a **relaxed** arm makes the weighted `_rand` walk (the
/// same roll as a one-shot's — this is where a re-armed Stand lands on its rare look-around
/// variations, the fidget); a combat/cast arm is forced to the deterministic head
/// ([`select::arm_forces_head`] decides which, at the call site). The kernel itself never
/// re-rolls — the advance between variations is the WATCHDOG's re-arm ([`roll_loop`]'s window,
/// decision 0516), not a kernel cycle.
pub(super) fn pick_loop_variation<'a>(
    anims: &'a ModelAnimations,
    head: &'a AnimClip,
    relaxed: bool,
    rng: &mut u32,
) -> &'a AnimClip {
    if relaxed {
        anims
            .pick_variation(head.anim_id, select::msvc_rand(rng))
            .unwrap_or(head)
    } else {
        head
    }
}

/// The looping arm's two rolls in op4's order (variation `0x71249a`, then the replay budget
/// `0x712692..` — the same two `_rand` sites as a one-shot's; decision 0516, wow-re
/// `loop-replay-fidget.md` §7d): the budget is live for **loops** too — not as a repeat cap but
/// as the watchdog **window**, `R` clip-lengths wide (`windowHi = arm + span·R`). Returns the
/// armed clip + `R` = total passes before the watchdog re-arms (`R ∈ [min, max−1]` floored to 1
/// — `replayMax` is exclusive, and `(0,0)`/`(0,1)` both play exactly once, visibly).
pub(super) fn roll_loop<'a>(
    anims: &'a ModelAnimations,
    head: &'a AnimClip,
    relaxed: bool,
    rng: &mut u32,
) -> (&'a AnimClip, u32) {
    let c = pick_loop_variation(anims, head, relaxed, rng);
    let r = select::replay_count(c.replay, select::msvc_rand(rng));
    (c, r)
}

/// Cross-fade into clip `id`, resolved through the model's own baked fallback first (decision 0082 —
/// see [`find_resolved`]) so a model lacking `id` plays its baked substitute rather than nothing.
/// `looping` repeats it; `rate` sets its playback speed. No-op if resolution still comes up empty.
/// A **one-shot** (`!looping`) rolls the resolved id's **variation and replay budget** per play
/// ([`roll_oneshot`] — decisions 0114/0117); a looping play rolls its variation (when `relaxed` —
/// decision 0123) **and its budget** ([`roll_loop`] — decision 0516), publishing the armed
/// `(node, R)` into `window` for the watchdog's advance; a one-shot arm clears it (its budget is
/// the `Count` repeat — no window outlives the arm).
#[allow(clippy::too_many_arguments)] // the resolve+roll+play primitive's full input set
pub(super) fn play(
    tr: &mut AnimationTransitions,
    player: &mut AnimationPlayer,
    anims: &ModelAnimations,
    id: u16,
    looping: bool,
    relaxed: bool,
    rate: f32,
    catalog: Option<&AnimDataCatalog>,
    rng: &mut u32,
    window: &mut Option<(bevy::animation::graph::AnimationNodeIndex, u32)>,
) {
    if let Some(c) = find_resolved(anims, id, catalog) {
        let (c, repeat) = if looping {
            let (c, r) = roll_loop(anims, c, relaxed, rng);
            *window = Some((c.node, r));
            (c, RepeatAnimation::Forever)
        } else {
            *window = None;
            roll_oneshot(anims, c, rng)
        };
        play_clip(tr, player, c, repeat, rate);
    }
}

/// Whether the one-shot clip `id` has finished playing (resolved through the model's own baked
/// fallback first, decision 0082 — matching [`play`], which is what started it) — or the model lacks
/// even the substitute, so the machine doesn't wait forever. Checked across the id's **variations**
/// (decision 0114): the play rolled one of them, and whichever it was, "finished" means no variation
/// of the id is still running.
pub(super) fn oneshot_finished(
    player: &AnimationPlayer,
    anims: &ModelAnimations,
    id: u16,
    catalog: Option<&AnimDataCatalog>,
) -> bool {
    match find_resolved(anims, id, catalog) {
        Some(head) => anims
            .clips
            .iter()
            .filter(|c| c.anim_id == head.anim_id)
            .all(|c| player.animation(c.node).is_none_or(|a| a.is_finished())),
        None => true,
    }
}

/// Enter the Special `sp`, returning the mode to adopt. A pose or a jump plays its enter one-shot
/// and settles through [`Mode::Entering`]; **Fall has no enter** — the client plays the Fall(40)
/// loop directly the tick FALLINGFAR latches (`0x602c40`) — so it goes straight to
/// [`Mode::Looping`] with a looping play.
#[allow(clippy::too_many_arguments)]
pub(super) fn enter_special(
    sp: Special,
    relaxed: bool,
    tr: &mut AnimationTransitions,
    player: &mut AnimationPlayer,
    anims: &ModelAnimations,
    catalog: Option<&AnimDataCatalog>,
    rng: &mut u32,
    window: &mut Option<(bevy::animation::graph::AnimationNodeIndex, u32)>,
) -> Mode {
    if sp == Special::Fall {
        play(
            tr,
            player,
            anims,
            sp.loop_id(),
            true,
            relaxed,
            1.0,
            catalog,
            rng,
            window,
        );
        Mode::Looping(sp)
    } else {
        // Enter plays are one-shots — `relaxed` (a looping-arm concern) is moot for them.
        play(
            tr,
            player,
            anims,
            sp.enter(),
            false,
            false,
            1.0,
            catalog,
            rng,
            window,
        );
        Mode::Entering(sp)
    }
}

/// Transition out of the Special flow `sp` this frame, given what the unit now wants (`special`,
/// `moving`). A *different* Special preempts with its own entry (a second jump cutting the first's
/// landing; a jump handing off to Fall when FALLINGFAR latches); a pose abandoned because the unit
/// started moving drops straight to the gait, letting the cross-fade carry the half-pose into the
/// walk; an airborne state landing plays its [`jump_land_pick`]; otherwise `sp` plays its graceful
/// exit one-shot, which [`super::drive_animations`] then waits out. Returns the mode to adopt.
#[allow(clippy::too_many_arguments)]
pub(super) fn leave_special(
    sp: Special,
    special: Option<Special>,
    moving: bool,
    relaxed: bool,
    flags: u32,
    tr: &mut AnimationTransitions,
    player: &mut AnimationPlayer,
    anims: &ModelAnimations,
    catalog: Option<&AnimDataCatalog>,
    rng: &mut u32,
    window: &mut Option<(bevy::animation::graph::AnimationNodeIndex, u32)>,
) -> Mode {
    // The client blends OUT of a cut airborne clip from a **pose snapshot** — op4 `0x7121a0`
    // with blendFlag≠0 copies the outgoing pose to `+0xc4` and *decays the frozen pose* under
    // the incoming's blend-in (wow-re `swim-jump-anim-law.md`; the swim-hop §5, decision 0503).
    // Bevy's transitions instead keep the outgoing clip RUNNING while its weight ramps down —
    // and on an early cut (the swim re-latch ~0.24 s into the 833 ms JumpStart) the clip's
    // remaining frames are the leg RECOVERY, so the fading kick actively retracts: the kick
    // reads far shorter than the ref's lingering mid-kick pose (director-reported). Freezing
    // the outgoing node reproduces the snapshot-decay. Scoped to the airborne cut, where the
    // divergence is visible; the client's snapshot law is universal (every blended arm), and
    // adopting it for all cross-fades is 0503's recorded follow-up.
    if matches!(sp, Special::Jump | Special::Fall) {
        if let Some(active) = tr
            .get_main_animation()
            .and_then(|n| player.animation_mut(n))
        {
            active.set_speed(0.0);
        }
    }
    if let Some(next) = special {
        enter_special(next, relaxed, tr, player, anims, catalog, rng, window)
    } else if matches!(sp, Special::Jump | Special::Fall) {
        // The landing is a plain, freely-overwritten pick (decisions 0083/0087 (d)): the clip
        // is chosen from the input *at touchdown* (`flags`) by the `0x602c60` dispatcher's rule,
        // and re-picked the instant any movement flag changes — not a non-preemptible bracket.
        // A backpedal/walk landing picks NO clip: the gait (WalkBackwards) starts the same frame.
        match jump_land_pick(flags) {
            Some(id) => {
                play(
                    tr, player, anims, id, false, false, 1.0, catalog, rng, window,
                );
                Mode::Land { id, flags }
            }
            None => Mode::Gait,
        }
    } else if sp.interruptible_by_move() && moving {
        Mode::Gait
    } else {
        let exit = sp.exit();
        play(
            tr, player, anims, exit, false, false, 1.0, catalog, rng, window,
        );
        Mode::Exiting(sp, exit)
    }
}
