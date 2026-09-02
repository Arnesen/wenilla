//! The **placed-doodad** animation-event scanner — the third and last producer feeding the
//! [`AnimSoundEvent`] stream, beside the creature scanner ([`crate::creature_anim`]) and the
//! GameObject one ([`crate::go_anim`]).
//!
//! Bug B345 ("every world-placed M2 doodad is silent") was this hole: a lamp's hum, a campfire's
//! crackle, a waterfall, a windmill and a gnome machine are all authored as `$DSL`/`$DSO`/`$SND`
//! keys on the model's own idle sequence — and **nothing in benilla ever wrote a doodad's events
//! into the stream**. The routing end ([`crate::sound::anim_events`]) has handled those tags since
//! decision 0070 slice 3; it simply never received one from a doodad, because the only two
//! scanners gate on `AnimDriver` (units) and `GoAnim` (server-sent GameObjects), neither of which a
//! terrain-stream placement ever gets.
//!
//! **Why it is not enough to scan the hosts that already exist.** The corpus says 273 models author
//! a doodad sound marker (`benilla-extract soundeventscan`); only 165 of them are the `FirstSeq`
//! tier that carries an `AnimationPlayer`. The other 108 — including the reported lamp,
//! `KalidarStreetLamp01.m2`, one looping 3.333 s Stand that keys no bone at all and carries exactly
//! one `$DSL` → `NightElfStreetLampLoop` — are bind-posed, and decision 0130's content gate
//! deliberately builds them no rig. That gate is right about pixels and was never a claim about the
//! clock: the reference arms **every** placed doodad and cycles it (wow-re `doodad-anim-host.md`
//! §1/§5). So `doodad_anim::spawn_anim_host` now gives a sound-carrying model a clock-only host —
//! the arm bookkeeping, no player — and this scanner reads that arm.
//!
//! **The clock is the shared one, never the player** ([`DoodadAnimHost::arm_clock`]): the reference
//! gates the cycle on *linkage* (residency), not on the draw, so a campfire behind you keeps
//! crackling. Reading the draw-gated `AnimationPlayer` would silence it the moment you turned away.

use bevy::prelude::*;

use benilla_world::doodad_anim::DoodadAnimHost;
use benilla_world::schedule::WorldStage;

use crate::creature_anim::{advance_track, scan_events, AnimSoundEvent, TrackMemory};
use benilla_assets::ModelAnimations;

/// Fire the event keyframes each placed doodad's armed clip crossed this frame.
///
/// The arming rules are the shared [`advance_track`]/[`scan_events`] ones the other two scanners
/// use — an arm frame fires nothing, the frame after it opens the clip's `t = 0` head (which is
/// where essentially every doodad sound key sits), and a loop wrap fires tail-then-head. A
/// variation re-roll ([`benilla_world::doodad_anim`]'s self-sustaining re-arm) changes the node, so
/// [`advance_track`] sees a fresh arm and never scans across the seam.
fn fire_doodad_anim_events(
    time: Res<Time>,
    hosts: Query<(Entity, &DoodadAnimHost, &ModelAnimations)>,
    mut last: Local<TrackMemory>,
    mut out: MessageWriter<AnimSoundEvent>,
) {
    let now = time.elapsed_secs();
    for (entity, host, anims) in &hosts {
        let Some((node, cur)) = host.arm_clock(now) else {
            continue; // gseq-only host with no sound arm: nothing is armed, so nothing fires
        };
        let Some(clip) = anims.clips.iter().find(|c| c.node == node) else {
            continue;
        };
        if let Some(prev) = advance_track(&mut last, entity, node, cur) {
            scan_events(clip, entity, prev, cur, &mut out);
        }
    }
    // Drop memory for hosts that no longer exist. The other two scanners leave their `Local` to
    // grow — a unit or GameObject population churns with the session — but placements do not:
    // crossing a continent streams tens of thousands of them in and out, and an unreaped entry per
    // placement is an unbounded leak on the one lane where it actually bites. `Query::contains` is
    // a lookup, so this is one pass over a map sized by the live sound-host count. It lives here
    // rather than in its own system because a `Local` belongs to exactly one system.
    last.retain(|e, _| hosts.contains(*e));
}

pub(crate) fn plugin(app: &mut App) {
    app.add_systems(Update, fire_doodad_anim_events.in_set(WorldStage::Present));
}
