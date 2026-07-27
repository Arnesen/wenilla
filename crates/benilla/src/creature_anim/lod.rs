//! The unit animation-LOD gate (decision 0448): park an off-frustum rig's **per-bone pose
//! evaluation**, keep **every clock** running.
//!
//! The reference has NO view cull on unit skeletons — every in-range, non-hidden unit is fully
//! animated every frame (wow-re `unit-anim-visibility-gate.md`: worklist admission is the CGUnit
//! render vtable chain, no frustum/occlusion/distance test — the 0364 §1 dispatch's verdict). So
//! this gate is a *modernization*, not a fidelity copy, and it preserves the verdict's two
//! observables:
//!
//! - **(i) the absolute-clock snap** — a re-appearing unit shows the pose "now" dictates. Nothing
//!   here pauses an [`AnimationPlayer`]: Bevy's `advance_animations` keeps ticking every seek
//!   clock, so waking is just sampling again — there is no frozen state to resume from.
//! - **(ii) off-screen combat stays audible** — the event scanner
//!   ([`super::events::fire_anim_events`]) reads the player's seek, not the bones, and the driver
//!   state machine keeps arming clips for parked rigs; `$CSS`/`$CAH`/`$HIT` (0075), footsteps,
//!   `$BWP`/`$BWR`, and the `$CSL`-keyed missile release (0430) all fire regardless of the camera.
//!
//! The park mechanism is the [`AnimParked`] marker alone (decision 0712 — the evaluator took over
//! from `animate_targets`, and the old per-joint `AnimatedBy` repoint died with the targets): the
//! pose evaluator ([`super::pose`]) skips a parked rig, so its bone `Transform`s stop changing,
//! which quiets transform propagation and `extract_skins`' changed-joint upload by change
//! detection. The pose post-passes (body twist, global-sequence writes, billboard joint palette)
//! gate on the same marker. Contrast the DOODAD gate ([`crate::doodad_anim`]), which faithfully
//! stops the player and re-arms on the shared clock — correct there (no one consumes a doodad's
//! events off-screen), the exact 0075 trap here.

use bevy::app::AnimationSystems;
use bevy::camera::primitives::{Frustum, Sphere as CullSphere};
use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use crate::entities::mount::MountBody;
use crate::net::SelfPlayer;
use crate::player::WorldCamera;
use crate::target::SelectionRadius;

use super::AnimDriver;

/// This rig's per-bone pose evaluation is parked (decision 0448): the pose evaluator and the pose
/// post-passes skip it. The sequence clocks, the driver state machine, and the event scanner all
/// keep running — parking turns *sampling* off, nothing else.
#[derive(Component)]
pub(crate) struct AnimParked;

/// Park only after the rig has been continuously out of frustum this long — a camera swing
/// across the pack doesn't churn bone repoints. Waking is always instant.
const PARK_AFTER_SECS: f32 = 0.5;

/// The frustum sphere: `SelectionRadius × root scale × RADIUS_SCALE + RADIUS_PAD`. The selection
/// radius is the Stand-box *footprint* (tighter than the animated body) — the scale and pad buy
/// the invariant that a rig with a pixel on screen always intersects, with margin for swing/
/// attachment overhang and the one-frame frustum staleness. Model-less (cube) units take
/// [`FALLBACK_RADIUS`]. Over-generous only costs a few more live rigs; under is the only failure.
const RADIUS_SCALE: f32 = 2.0;
const RADIUS_PAD: f32 = 4.0;
const FALLBACK_RADIUS: f32 = 6.0;

/// Park/wake streamed rigs by the padded sphere-vs-frustum test (decision 0448). PostUpdate,
/// before [`AnimationSystems`] — a wake drops the marker in time for the same frame's pose
/// evaluation, so the re-appearing unit samples the absolute-clock pose with no stale
/// frame. Exempt: the self-avatar (the camera rides its attachment-17 pivot, and it must keep
/// animating faded-out in first person) and its mount child. `WOW_NO_ANIM_LOD=1` disables
/// parking — the live-probe A/B lever.
#[allow(clippy::type_complexity, clippy::too_many_arguments)] // one Bevy system's full input set
pub(super) fn gate_rig_animation(
    time: Res<Time>,
    cam: Query<&Frustum, With<WorldCamera>>,
    rigs: Query<
        (
            Entity,
            &GlobalTransform,
            Option<&SelectionRadius>,
            Has<AnimParked>,
            Has<SelfPlayer>,
            Option<&MountBody>,
        ),
        With<AnimDriver>,
    >,
    self_hosts: Query<Has<SelfPlayer>>,
    mut out_since: Local<EntityHashMap<f32>>,
    mut disabled: Local<Option<bool>>,
    mut park_all: Local<Option<bool>>,
    mut commands: Commands,
) {
    let disabled = *disabled.get_or_insert_with(|| std::env::var_os("WOW_NO_ANIM_LOD").is_some());
    // The measurement twin of `WOW_NO_ANIM_LOD`: park EVERY streamed rig regardless of view. The
    // two levers bracket the pose-evaluation lane's cost at any pin — "never park" is its ceiling,
    // "always park" its floor — which is the only way to attribute a frame budget without a
    // profiler build (the chrome-trace feature costs a 5-minute rebuild and a GB-scale file).
    // Not a shipping mode: it freezes visible rigs on purpose.
    let park_all = *park_all.get_or_insert_with(|| std::env::var_os("WOW_ANIM_PARK_ALL").is_some());
    let Ok(frustum) = cam.single() else {
        return;
    };
    let now = time.elapsed_secs();
    for (entity, tf, radius, parked, is_self, mount) in &rigs {
        let exempt =
            disabled || is_self || mount.is_some_and(|m| self_hosts.get(m.host).unwrap_or(false));
        let visible = exempt
            || !park_all && {
                let scale = tf.to_scale_rotation_translation().0.max_element();
                let r = radius.map_or(FALLBACK_RADIUS, |r| r.0 * scale * RADIUS_SCALE + RADIUS_PAD);
                frustum.intersects_sphere(
                    &CullSphere {
                        center: tf.translation().into(),
                        radius: r,
                    },
                    false,
                )
            };
        if visible {
            out_since.remove(&entity);
            if parked {
                commands.entity(entity).remove::<AnimParked>();
            }
        } else if !parked {
            let since = *out_since.entry(entity).or_insert(now);
            if now - since >= PARK_AFTER_SECS {
                out_since.remove(&entity);
                commands.entity(entity).insert(AnimParked);
            }
        }
    }
    // Drop entries for rigs that despawned (or lost their model) before parking.
    out_since.retain(|e, _| rigs.contains(*e));
}

/// Register the gate: [`gate_rig_animation`] before the frame's pose evaluation.
pub(super) fn plugin(app: &mut App) {
    app.add_systems(PostUpdate, gate_rig_animation.before(AnimationSystems));
}

#[cfg(test)]
mod tests {
    use bevy::animation::animation_curves::{AnimatableCurve, AnimatableKeyframeCurve};
    use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle};
    use bevy::animation::{animated_field, AnimationClip};

    use benilla_assets::{
        bone_target_id, AnimClip, ModelAnimations, PoseBone, PoseClip, PoseSource, PoseTrack,
    };

    use super::super::{events::fire_anim_events, AnimSoundEvent};
    use super::*;

    /// The test clip's loop length (seconds) — short so real-dt frames wrap it several times.
    const DUR: f32 = 0.4;

    /// A one-joint rig whose looping clip translates the bone `0 → +X` over [`DUR`] and carries a
    /// `$TST` event keyframe mid-loop: enough to observe all three gate laws — clocks, bones,
    /// events. Returns `(root, joint)`.
    fn spawn_rig(app: &mut App, at: Vec3) -> (Entity, Entity) {
        let mut pose = PoseSource {
            bone_masks: vec![0],
            ..Default::default()
        };
        let mut pose_clip = PoseClip::default();
        pose_clip.push(PoseBone {
            bone: 0,
            translation: PoseTrack::new(&[(0.0, Vec3::ZERO), (DUR, Vec3::X)]),
            rotation: PoseTrack::default(),
            scale: PoseTrack::default(),
        });
        pose.clips.push(pose_clip);
        let mut clip = AnimationClip::default();
        clip.add_curve_to_target(
            bone_target_id(0),
            AnimatableCurve::new(
                animated_field!(Transform::translation),
                AnimatableKeyframeCurve::new([(0.0, Vec3::ZERO), (DUR, Vec3::X)])
                    .expect("two keyframes build"),
            ),
        );
        let clip_handle = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(clip);
        let (graph, node) = AnimationGraph::from_clip(clip_handle);
        pose.set_node(node, 0, 0);
        let graph_handle = app
            .world_mut()
            .resource_mut::<Assets<AnimationGraph>>()
            .add(graph);

        let mut player = AnimationPlayer::default();
        player.play(node).repeat();
        let anims = ModelAnimations {
            graph: graph_handle.clone(),
            clips: vec![AnimClip {
                anim_id: 0,
                seq_index: 0,
                node,
                looping: true,
                duration: DUR,
                move_speed: 0.0,
                blend_time: 0.0,
                bounds_center: Vec3::ZERO,
                bounds_radius: 0.0,
                bounds_min: Vec3::ZERO,
                bounds_max: Vec3::ZERO,
                events: vec![benilla_formats::AnimEvent {
                    time: DUR * 0.5,
                    ident: *b"$TST",
                    data: 0,
                }]
                .into(),
                arm_nodes: None,
                upper_node: None,
                frequency: 0,
                replay: (0, 0),
            }],
            hand_close: [None, None],
            playable_animation_lookup: Vec::new(),
            animation_lookup: Vec::new(),
            global_bones: Vec::new(),
            first_seq: None,
            pose: std::sync::Arc::new(pose),
        };
        // The driver's requested base = the gait slot: point it at the clip so the event
        // scanner's `resolved_anim` finds the advancing timeline (its live writer is
        // `drive_animations`, out of scope here).
        let driver = AnimDriver {
            gait: Some(0),
            ..Default::default()
        };
        let root = app
            .world_mut()
            .spawn((
                Transform::from_translation(at),
                GlobalTransform::from_translation(at),
                player,
                AnimationGraphHandle(graph_handle),
                anims,
                driver,
            ))
            .id();
        let joint = app
            .world_mut()
            .spawn((Transform::default(), ChildOf(root)))
            .id();
        // The evaluator's rig handle (0712); target ids no longer ride the joints.
        app.world_mut()
            .entity_mut(root)
            .insert(super::super::PosedRig {
                joints: vec![joint],
            });
        (root, joint)
    }

    /// A camera at the origin looking down −Z (view = identity): rigs at −Z are in frame, rigs
    /// at +Z are behind it.
    fn spawn_camera(app: &mut App) {
        let clip_from_world = Mat4::perspective_rh(1.2, 1.0, 0.1, 1000.0);
        app.world_mut()
            .spawn((WorldCamera, Frustum::from_clip_from_world(&clip_from_world)));
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::animation::AnimationPlugin,
        ));
        app.add_message::<AnimSoundEvent>();
        // The live schedule's shape: the gate ahead of the frame's pose evaluation; the event
        // scanner reading the advanced seek after it.
        app.add_systems(PostUpdate, gate_rig_animation.before(AnimationSystems));
        super::super::pose::plugin(&mut app);
        app.add_systems(PostUpdate, fire_anim_events.after(AnimationSystems));
        app
    }

    /// Step real time forward by ~`secs` across `steps` updates (Time under MinimalPlugins
    /// advances by wall clock, the same basis the global-seq tests use).
    fn advance(app: &mut App, secs: f32, steps: u32) {
        for _ in 0..steps {
            std::thread::sleep(std::time::Duration::from_secs_f32(secs / steps as f32));
            app.update();
        }
    }

    fn seek(app: &App, rig: Entity) -> f32 {
        let player = app.world().entity(rig).get::<AnimationPlayer>().unwrap();
        let (_, active) = player.playing_animations().next().expect("clip armed");
        active.seek_time()
    }

    fn bone(app: &App, joint: Entity) -> Vec3 {
        app.world()
            .entity(joint)
            .get::<Transform>()
            .unwrap()
            .translation
    }

    /// The three gate laws in one flow (decision 0448): an off-frustum rig parks (joints
    /// repointed at the park entity, bones frozen) while its seek clock keeps advancing and its
    /// event keyframes keep firing (the 0075 trap — off-screen swings must not go silent); a
    /// woken rig and a never-parked twin sample the SAME pose — the absolute-clock snap, not
    /// resume-from-freeze.
    #[test]
    fn parked_rig_keeps_clock_and_events_and_wakes_to_the_absolute_pose() {
        let mut app = app();
        spawn_camera(&mut app);
        // `behind` starts off-frustum (+Z, behind the camera); `twin` stays in frame at −Z.
        let (behind, behind_joint) = spawn_rig(&mut app, Vec3::Z * 50.0);
        let (twin, twin_joint) = spawn_rig(&mut app, -Vec3::Z * 20.0);
        app.update();

        // Past the hysteresis window: parked, joints repointed.
        advance(&mut app, PARK_AFTER_SECS + 0.2, 4);
        assert!(
            app.world().entity(behind).contains::<AnimParked>(),
            "off-frustum past the hysteresis window ⇒ parked"
        );
        assert!(
            !app.world().entity(twin).contains::<AnimParked>(),
            "the in-frame twin stays live"
        );
        // While parked: the bone holds, the clock runs, the mid-loop event keyframe still fires.
        let frozen = bone(&app, behind_joint);
        let seek_at_park = seek(&app, behind);
        let mut cursor = app
            .world()
            .resource::<Messages<AnimSoundEvent>>()
            .get_cursor_current();
        let mut fired = 0usize;
        for _ in 0..6 {
            // > one full loop in total, so the keyframe is crossed whatever the phase.
            std::thread::sleep(std::time::Duration::from_secs_f32(DUR * 0.3));
            app.update();
            let messages = app.world().resource::<Messages<AnimSoundEvent>>();
            fired += cursor
                .read(messages)
                .filter(|ev| ev.entity == behind && &ev.ident == b"$TST")
                .count();
        }
        assert!(fired > 0, "a parked rig's event keyframes keep firing");
        assert_eq!(
            frozen,
            bone(&app, behind_joint),
            "a parked bone's Transform never changes"
        );
        assert_ne!(
            seek_at_park,
            seek(&app, behind),
            "the parked rig's seek clock keeps advancing"
        );

        // Wake: move the rig in front of the camera. The same frame, it samples the
        // absolute-clock pose — seek and bone equal to the never-parked twin's (both players
        // armed the same frame and tick the same delta).
        app.world_mut()
            .entity_mut(behind)
            .insert(GlobalTransform::from_translation(-Vec3::Z * 20.0));
        app.update();
        assert!(
            !app.world().entity(behind).contains::<AnimParked>(),
            "in-frustum ⇒ instant wake"
        );
        assert_eq!(
            seek(&app, behind),
            seek(&app, twin),
            "the seek clocks never diverged"
        );
        assert_eq!(
            bone(&app, behind_joint),
            bone(&app, twin_joint),
            "the woken pose equals the never-parked twin's — the absolute-clock snap"
        );
    }

    /// The self-avatar never parks (the camera rides its attachment pivot; first-person fades it
    /// while its swings still drive combat audio and the camera seat).
    #[test]
    fn the_self_avatar_is_exempt() {
        let mut app = app();
        spawn_camera(&mut app);
        let (behind, behind_joint) = spawn_rig(&mut app, Vec3::Z * 50.0);
        app.world_mut().entity_mut(behind).insert(SelfPlayer);
        advance(&mut app, PARK_AFTER_SECS + 0.2, 4);
        assert!(
            !app.world().entity(behind).contains::<AnimParked>(),
            "SelfPlayer never parks, wherever the camera points"
        );
        // And its bones keep animating — the exemption is live evaluation, not just the marker.
        let before = bone(&app, behind_joint);
        advance(&mut app, DUR * 0.35, 2);
        assert_ne!(before, bone(&app, behind_joint), "its joints stay live");
    }
}
