//! The model event-keyframe scanner: [`fire_anim_events`] and its helpers, plus the
//! [`AnimSoundEvent`] message it emits — the animation timeline's outbound trigger surface
//! (decision 0070 slice 3). Kept in its own file as its own small concern, separate from the
//! driver ([`super::driver`]) that advances the clips it scans.

use benilla_assets::{AnimClip, ModelAnimations};
use bevy::animation::graph::AnimationNodeIndex;
use bevy::prelude::*;

use super::{resolved_id, AnimData, AnimDriver};

/// A model **event keyframe** crossed during playback this frame (the M2 `$xxx` tags — decision
/// 0070 slice 3): the animation timeline's outbound trigger surface. The sound subsystem routes
/// the audio tags (`$SND` kits, footsteps, `$CSS` swings, fidgets); future consumers (camera
/// shake `$SHK`, footprints) read the same stream.
#[derive(Message, Clone, Copy)]
pub(crate) struct AnimSoundEvent {
    pub(crate) entity: Entity,
    /// The forward-stored 4CC tag (`*b"$FL0"`, `*b"$SND"`, …).
    pub(crate) ident: [u8; 4],
    /// The tag payload (a SoundEntries id for `$SND`/`$DSL`/`$DSO`; 0 otherwise).
    pub(crate) data: u32,
}

/// A footfall is **two independent channels**, and a tag belongs to exactly one of them — the
/// client's event dispatcher `0x5ffbd0` routes them to two different handlers (wow-re
/// `footprint-decals.md` §1, §5 4-agent round, byte-arbitrated):
///
/// - **`$FSD` → `0x623390`: the footstep SOUND**, and nothing else — it never reaches the decal
///   path ([`is_footstep_sound`]).
/// - **the per-foot side tags → `0x5fbf70`: the VISUAL footfall** — the footprint decal and the
///   spray/splash particle, and no sound at all ([`footfall_side`]).
///
/// So a gait that authors both fires **one** sound per `$FSD` key, not one per key of either
/// family: HumanMale's Walk keys `$FR0 · $FSD · $FL0 · $FSD` over 1 s and the real client plays
/// **two** steps there, while its turn-in-place ShuffleLeft/Right key only `$SL0 $SR0` at
/// `t = 0.000` and are **silent** (decision 1080).
///
/// The **sound** channel: the dispatch tag `$FSD` alone.
pub(crate) fn is_footstep_sound(ident: &[u8; 4]) -> bool {
    ident == b"$FSD"
}

/// The **visual** channel: the foot side a per-foot plant tag names (`$FL0` → `L`), or `None` for
/// every other tag — the trigger for footfall-driven *visuals* ([`crate::footprints`]). The ten
/// families the dispatcher tables, each with its `0/1/2/3` variants: `$FL/$FR` (front),
/// `$RL/$RR` (rear), `$SL/$SR` (shuffle), `$BL/$BR` (backwards), `$WL/$WR` — call sites
/// `0x5ffe32` (`push 1`, LEFT) and `0x5ffc82` (`push 0`, RIGHT).
pub(crate) fn footfall_side(ident: &[u8; 4]) -> Option<u8> {
    matches!(
        &ident[..3],
        b"$FL" | b"$FR" | b"$RL" | b"$RR" | b"$SL" | b"$SR" | b"$BL" | b"$BR" | b"$WL" | b"$WR"
    )
    .then_some(ident[2])
}

/// How far (seconds) into a just-switched clip the playhead may be for [`fire_anim_events`] to
/// count it as *watched from the start* and fire its head window (`t = 0` keyframes included).
/// Comfortably above a frame (even a hitchy one), comfortably below the corpse settle's
/// `seek_to(duration)` (every Death clip is > 1 s).
const FRESH_CLIP_HEAD: f32 = 0.25;

/// Fire the event keyframes the current clip crossed since last frame. Runs after
/// [`super::driver::drive_animations`] so the clip/seek state is this frame's. Per unit we
/// remember `(anim_id, seek_time)`; a loop wrap fires the tail `(prev, duration]` then the head
/// `[0, cur]`.
///
/// Arming: **first sight of a unit arms silently** (it streamed in mid-clip — never back-fire a
/// window we didn't watch). A **clip change on a tracked unit fires the head window `[0, cur]`**
/// when the playhead is near the start ([`FRESH_CLIP_HEAD`]) — we watched this clip begin, and
/// keyframes at exactly `t = 0` are real (the emote voices: EmoteLaugh/Cry/Chicken carry their
/// `$CSD` at `0.000`, probe-verified — the old arm-only rule silenced /laugh while mid-clip
/// /applaud clapped). A clip change that *starts* deep in the timeline (the corpse settle's
/// `seek_to(duration)`) stays silent — that window wasn't played.
pub(super) fn fire_anim_events(
    units: Query<(Entity, &ModelAnimations, &AnimationPlayer, &AnimDriver)>,
    mut last: Local<bevy::ecs::entity::EntityHashMap<(AnimationNodeIndex, f32)>>,
    // The **masked overlay** track's own `(node, seek)` memory (decision 0087): a swing/emote routed to
    // the SpineLow overlay plays *beside* the base, so its events (a swing's `$CSS`, an emote's `$CSD`)
    // are scanned on their own node — the base scan above never sees them.
    mut last_overlay: Local<bevy::ecs::entity::EntityHashMap<(AnimationNodeIndex, f32)>>,
    mut out: MessageWriter<AnimSoundEvent>,
    anim_data: Option<Res<AnimData>>,
) {
    let catalog = anim_data.as_deref().map(|d| &d.0);
    for (entity, anims, player, drv) in &units {
        // Base track: the **resolved** id (decision 0082) — the clip whose timeline is actually
        // advancing, which can differ from the requested `active_anim()` when this model falls back —
        // then the id's **playing variation** (decision 0114: a one-shot rolled one of the id's
        // variation clips, each its own node with its own event track). During a same-id cross-fade
        // (swing variation A fading under fresh variation B) the newest play — the smallest seek —
        // is the track; the node-keyed memory then treats the switch as a clip change.
        if let Some(id) = drv.resolved_anim(anims, catalog) {
            let playing = anims
                .clips
                .iter()
                .filter(|c| c.anim_id == id)
                .filter_map(|c| player.animation(c.node).map(|a| (c, a.seek_time())))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((clip, cur)) = playing {
                if let Some(prev) = advance_track(&mut last, entity, clip.node, cur) {
                    scan_events(clip, entity, prev, cur, &mut out);
                }
            }
        }
        // Masked overlay track: events fire from whichever track plays the clip. The overlay knows
        // its exact node — match it back to its clip (a variation's `upper_node`, decision 0114).
        // A freshly-started overlay (seek ≈ 0) fires its head window via [`advance_track`], so an
        // emote's `t = 0` `$CSD` voice still rings; a swing's mid-clip `$CSS` fires as normal.
        if let Some(ov) = drv.overlay {
            let id = resolved_id(anims, ov.id, catalog);
            let clip = anims
                .clips
                .iter()
                .filter(|c| c.anim_id == id)
                .find(|c| c.upper_node == Some(ov.node));
            if let Some(clip) = clip {
                if let Some(active) = player.animation(ov.node) {
                    let cur = active.seek_time();
                    if let Some(prev) = advance_track(&mut last_overlay, entity, ov.node, cur) {
                        scan_events(clip, entity, prev, cur, &mut out);
                    }
                }
            }
        }
    }
}

/// Advance a per-track `(node, seek)` memory and return the `prev` seek to scan events from, or `None`
/// to only arm this frame. Keyed by the playing **graph node** (not the semantic id): two variations
/// of the same id are different timelines with different event tracks (decision 0114), and a node
/// switch is a clip change like any other. **First sight of a unit arms silently** (it may have
/// streamed in mid-clip — never back-fire a window we didn't watch). A **clip change caught near the
/// start** ([`FRESH_CLIP_HEAD`]) returns `-1.0` so the head window `[0, cur]` fires — we watched this
/// clip begin, and `t = 0` keyframes are real (the emote voices carry `$CSD` at `0.000`). A clip
/// change that *starts* deep in its timeline (the corpse settle's `seek_to(duration)`) stays silent.
fn advance_track(
    last: &mut bevy::ecs::entity::EntityHashMap<(AnimationNodeIndex, f32)>,
    entity: Entity,
    node: AnimationNodeIndex,
    cur: f32,
) -> Option<f32> {
    match last.insert(entity, (node, cur)) {
        Some((pnode, p)) if pnode == node => Some(p),
        Some(_) if cur <= FRESH_CLIP_HEAD => Some(-1.0),
        _ => None,
    }
}

/// Fire the event keyframes `clip` crossed on `(prev, cur]`. A loop wrap (`cur < prev`) fires the
/// tail `(prev, duration]` of the last cycle then the head `[0, cur]` of the new one.
pub(crate) fn scan_events(
    clip: &AnimClip,
    entity: Entity,
    prev: f32,
    cur: f32,
    out: &mut MessageWriter<AnimSoundEvent>,
) {
    if clip.events.is_empty() || cur == prev {
        return;
    }
    let mut fire = |lo: f32, hi: f32| {
        for e in clip.events.iter() {
            if e.time > lo && e.time <= hi {
                out.write(AnimSoundEvent {
                    entity,
                    ident: e.ident,
                    data: e.data,
                });
            }
        }
    };
    if cur >= prev {
        fire(prev, cur);
    } else {
        fire(prev, clip.duration + 1.0);
        fire(-1.0, cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two channels are disjoint (decision 1080): `$FSD` is the whole sound channel, the
    /// per-foot side tags the whole visual one. HumanMale's Walk keys one of each family per
    /// footfall — routing both to sound is exactly the doubled step rate.
    #[test]
    fn the_sound_channel_is_fsd_alone() {
        assert!(is_footstep_sound(b"$FSD"));
        for t in [
            b"$FL0", b"$FR0", b"$RL2", b"$SL0", b"$SR0", b"$BR0", b"$WL1",
        ] {
            assert!(!is_footstep_sound(t), "{} is the visual channel", lossy(t));
            assert!(footfall_side(t).is_some(), "{} names a side", lossy(t));
        }
        assert_eq!(footfall_side(b"$FSD"), None);
    }

    /// The side letter a per-foot tag names; every other tag is `None`.
    #[test]
    fn footfall_side_reads_the_side_letter() {
        assert_eq!(footfall_side(b"$FL0"), Some(b'L'));
        assert_eq!(footfall_side(b"$FR0"), Some(b'R'));
        assert_eq!(footfall_side(b"$RL2"), Some(b'L'));
        assert_eq!(footfall_side(b"$BR0"), Some(b'R'));
        assert_eq!(footfall_side(b"$WR3"), Some(b'R'));
        assert_eq!(footfall_side(b"$SND"), None);
        assert_eq!(footfall_side(b"$CSL"), None);
    }

    fn lossy(t: &[u8; 4]) -> String {
        String::from_utf8_lossy(t).into_owned()
    }
}
