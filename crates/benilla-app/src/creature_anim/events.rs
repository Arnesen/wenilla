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

/// How far (seconds) into a just-armed clip the playhead may have been *at the arm* for
/// [`advance_track`] to open the next frame's window at the clip's head (`t = 0` keyframes
/// included). Comfortably above a frame (even a hitchy one), comfortably below the corpse
/// settle's `seek_to(duration)` (every Death clip is > 1 s).
const FRESH_CLIP_HEAD: f32 = 0.25;

/// One scanned track's memory: the graph node playing, the seek last seen on it, and whether that
/// seek is an **arm stamp** — the frame the clip was armed, which fires nothing (see
/// [`advance_track`]).
#[derive(Clone, Copy)]
pub(crate) struct TrackSeek {
    node: AnimationNodeIndex,
    seek: f32,
    armed: bool,
}

/// A scanner's per-entity track memory — the `Local` every [`advance_track`] caller owns.
pub(crate) type TrackMemory = bevy::ecs::entity::EntityHashMap<TrackSeek>;

/// Fire the event keyframes the current clip crossed since last frame. Runs after
/// [`super::driver::drive_animations`] so the clip/seek state is this frame's. Per unit we
/// remember the playing node and its seek ([`TrackSeek`]); a loop wrap fires the tail
/// `(prev, duration]` then the head `[0, cur]` — a `t = 0` key really does re-fire on every wrap,
/// in the reference too (wow-re `m2-event-track-walker.md` §2), which is why a held turn-in-place
/// lays a footprint pair twice a second there as well.
///
/// Arming is [`advance_track`]'s: an arm frame fires nothing, the frame after it opens the clip's
/// head window (so `t = 0` keyframes are real), and a clip that doesn't survive its arm frame
/// fires nothing at all — the reference's own walker rule, byte-cited there.
pub(super) fn fire_anim_events(
    units: Query<(Entity, &ModelAnimations, &AnimationPlayer, &AnimDriver)>,
    mut last: Local<TrackMemory>,
    // The **masked overlay** track's own memory (decision 0087): a swing/emote routed to the
    // SpineLow overlay plays *beside* the base, so its events (a swing's `$CSS`, an emote's `$CSD`)
    // are scanned on their own node — the base scan above never sees them.
    mut last_overlay: Local<TrackMemory>,
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
        // A freshly-started overlay fires its head window one frame after the arm via
        // [`advance_track`], so an emote's `t = 0` `$CSD` voice still rings; a swing's mid-clip
        // `$CSS` fires as normal.
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

/// Advance a per-track memory and return the `prev` seek to scan events from, or `None` to only
/// arm this frame. Keyed by the playing **graph node** (not the semantic id): two variations of
/// the same id are different timelines with different event tracks (decision 0114), and a node
/// switch is a clip change like any other.
///
/// **The arm frame fires nothing** — the reference's own rule, and the reason this is not simply
/// "fire the head window when you see a new clip" (decision 1273). The client's animation arm
/// `0x7121a0` bakes the block's window start `+0xa8 = now` (`0x712758`), so on that frame the
/// walker computes `prev == cur` and `0x719518 jae` abandons the block before reading a single
/// key. The window only opens on the **next** frame, and it opens at the arm stamp — local
/// `t = 0` — with the fire test `prevL <= t < curL` (`0x7196d5`–`0x7196d9`), so `t = 0` keyframes
/// are real (the emote voices carry `$CSD` at `0.000`) but only for a clip that is **still armed a
/// frame later**. A clip armed and abandoned inside one frame fires nothing at all, which is what
/// keeps a flickering Shuffle↔Stand churn silent in the reference — and is exactly what our
/// fire-on-the-arm-frame rule turned into a footprint carpet under a stuttering mouse-turn.
///
/// So: a clip change (and first sight of a unit — it may have streamed in mid-clip) records the
/// arm and returns `None`; the frame after an arm returns `-1.0`, opening the head window
/// `[0, cur]`. An arm that *starts* deep in its timeline (the corpse settle's `seek_to(duration)`,
/// past [`FRESH_CLIP_HEAD`]) opens at its own stamp instead, so a settled corpse never replays its
/// collapse's keys.
pub(crate) fn advance_track(
    last: &mut TrackMemory,
    entity: Entity,
    node: AnimationNodeIndex,
    cur: f32,
) -> Option<f32> {
    let was = last.get(&entity).copied().filter(|t| t.node == node);
    last.insert(
        entity,
        TrackSeek {
            node,
            seek: cur,
            armed: was.is_none(),
        },
    );
    let was = was?; // the arm frame itself: recorded, scanned never
    Some(if was.armed && was.seek <= FRESH_CLIP_HEAD {
        -1.0
    } else {
        was.seek
    })
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

    fn track() -> (TrackMemory, Entity, AnimationNodeIndex, AnimationNodeIndex) {
        (
            TrackMemory::default(),
            Entity::from_raw_u32(1).expect("valid entity id"),
            AnimationNodeIndex::new(7),  // ShuffleLeft, say
            AnimationNodeIndex::new(11), // Stand
        )
    }

    /// **The arm frame scans nothing, and the frame after it opens the clip's head.** The
    /// reference's arm (`0x7121a0`) stamps the block's window start at `now`, so the walker's
    /// `prev == cur` abandons the block that frame (`0x719518`); the next frame's window opens at
    /// that stamp — local `t = 0`, lower-inclusive — which is what makes an emote's `t = 0.000`
    /// `$CSD` real without making it fire a frame early.
    #[test]
    fn an_arm_frame_is_silent_and_the_next_frame_opens_the_head() {
        let (mut last, unit, shuffle, _) = track();
        assert_eq!(
            advance_track(&mut last, unit, shuffle, 0.0),
            None,
            "the arm frame itself scans nothing"
        );
        assert_eq!(
            advance_track(&mut last, unit, shuffle, 0.016),
            Some(-1.0),
            "the frame after the arm opens the head window [0, cur]"
        );
        assert_eq!(
            advance_track(&mut last, unit, shuffle, 0.032),
            Some(0.016),
            "…and steady frames scan (prev, cur] as before"
        );
    }

    /// **A clip armed and abandoned inside one frame fires nothing** — the report this rule was
    /// written for (decision 1273). A stuttering mouse-turn flickers the gait Shuffle↔Stand at
    /// input cadence, and HumanMale's ShuffleLeft keys `$SL0`+`$SR0` at `t = 0.000`: firing on the
    /// arm frame laid a footprint pair per flicker, where the reference lays none.
    #[test]
    fn a_clip_armed_for_one_frame_never_fires() {
        let (mut last, unit, shuffle, stand) = track();
        for _ in 0..8 {
            assert_eq!(advance_track(&mut last, unit, shuffle, 0.0), None);
            assert_eq!(advance_track(&mut last, unit, stand, 0.0), None);
        }
    }

    /// An arm that *starts* deep in its timeline — the corpse settle's `seek_to(duration)` —
    /// opens at its own stamp, never at the head: a body that streamed in dead must not replay
    /// the collapse's `$DTH` keys.
    #[test]
    fn an_arm_deep_in_the_timeline_never_opens_the_head() {
        let (mut last, unit, death, _) = track();
        assert_eq!(advance_track(&mut last, unit, death, 2.0), None);
        assert_eq!(
            advance_track(&mut last, unit, death, 2.0),
            Some(2.0),
            "the window opens at the settle stamp — (2.0, 2.0] is empty"
        );
    }

    /// The turn-in-place cadence, which this change deliberately leaves alone: HumanMale's
    /// ShuffleLeft (anim 11) is a **0.500 s loop** whose only keys are `$SL0` and `$SR0`, both at
    /// `t = 0.000`, and the reference re-fires a `t = 0` key on **every wrap** (wow-re
    /// `m2-event-track-walker.md` §2, byte-traced) — so a held turn lays a print pair twice a
    /// second in the real client too.
    #[test]
    fn a_loop_wrap_refires_the_head_keys() {
        let clip = shuffle_clip();
        assert_eq!(fired(&clip, -1.0, 0.016), vec![*b"$SL0", *b"$SR0"], "head");
        assert!(
            fired(&clip, 0.016, 0.4).is_empty(),
            "mid-lap: nothing keyed"
        );
        assert_eq!(
            fired(&clip, 0.48, 0.01),
            vec![*b"$SL0", *b"$SR0"],
            "the wrap fires the tail (empty here) then the head — both feet again"
        );
    }

    /// HumanMale ShuffleLeft's real shape (`benilla-extract m2events`): 0.500 s, looping, keys
    /// `$SL0` and `$SR0` both at 0.000.
    fn shuffle_clip() -> AnimClip {
        AnimClip {
            anim_id: 11,
            seq_index: 38,
            node: AnimationNodeIndex::new(7),
            looping: true,
            duration: 0.5,
            move_speed: 0.0,
            blend_time: 0.15,
            bounds_center: bevy::prelude::Vec3::ZERO,
            bounds_radius: 0.0,
            bounds_min: bevy::prelude::Vec3::ZERO,
            bounds_max: bevy::prelude::Vec3::ZERO,
            events: vec![
                benilla_formats::AnimEvent {
                    time: 0.0,
                    ident: *b"$SL0",
                    data: 0,
                },
                benilla_formats::AnimEvent {
                    time: 0.0,
                    ident: *b"$SR0",
                    data: 0,
                },
            ]
            .into(),
            arm_nodes: None,
            upper_node: None,
            frequency: 0,
            replay: (0, 0),
            poses_bones: true,
        }
    }

    /// The idents [`scan_events`] fires over one `(prev, cur]` window, in order.
    fn fired(clip: &AnimClip, prev: f32, cur: f32) -> Vec<[u8; 4]> {
        use bevy::ecs::system::RunSystemOnce;
        #[derive(bevy::prelude::Resource)]
        struct Window(AnimClip, f32, f32);
        let mut world = bevy::prelude::World::new();
        world.init_resource::<bevy::ecs::message::Messages<AnimSoundEvent>>();
        world.insert_resource(Window(clip.clone(), prev, cur));
        world
            .run_system_once(
                |win: bevy::prelude::Res<Window>, mut out: MessageWriter<_>| {
                    let unit = Entity::from_raw_u32(1).expect("valid entity id");
                    scan_events(&win.0, unit, win.1, win.2, &mut out);
                },
            )
            .expect("run_system_once");
        let mut msgs = world.resource_mut::<bevy::ecs::message::Messages<AnimSoundEvent>>();
        msgs.drain().map(|m| m.ident).collect()
    }
}
