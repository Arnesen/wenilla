//! The animation-LOD gate (decision 0448): park an off-frustum — or, since decision 0739,
//! portal-invisible — rig's **per-bone pose evaluation**, keep **every clock** running. Since
//! 0739 it governs the whole [`RigPose`] population — units, players, AND GameObject rigs (see
//! the query's comment for why the old `With<AnimDriver>` filter left every animated GO
//! sampling off-view).
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
//! gate on the same marker. Contrast the DOODAD gate ([`benilla_world::doodad_anim`]), which faithfully
//! stops the player and re-arms on the shared clock — correct there (no one consumes a doodad's
//! events off-screen), the exact 0075 trap here.
//!
//! **The room leg (decision 0739).** The frustum is the wrong instrument indoors: a dungeon
//! camera's view cone passes through walls, so most of an instance's population stays "in
//! frustum" while only the current room chain is drawable. The portal PVS
//! ([`benilla_world::wmo_portal`], the faithful WMO group cull) already knows which rooms the camera can
//! reach, and [`UnitWmoRoom`] already names each unit's room — so a unit whose room (and every
//! room one portal hop from it) is outside this frame's PVS parks too. Soundness: a unit is
//! room-parked exactly when the geometry *around* it — its room's walls, floor, furniture — is
//! portal-culled by the same bits, and a body can poke only through a portal, never a wall, so
//! the one-hop guard bounds the doorway straddle. Reveals carry no artifact by construction: a
//! mob can only become visible through a portal, and the moment that portal's screen window is
//! non-zero the flood marks its room visible (`compute_wmo_pvs` runs in `Update`, this gate in
//! `PostUpdate` before the evaluator — same-frame wake, absolute-clock pose). Fail-open at every
//! seam: no claim (outdoors, EXTERIOR groups), a despawned placement, a still-loading model, an
//! out-of-range index — all read visible, the [`benilla_world::wmo_portal::WmoGroupVis::drawn_by`]
//! convention.
//! `WOW_NO_ROOM_LOD=1` disables just this leg (the A/B lever); `WOW_RIG_COST=1` prints the
//! per-frame `[rig-gate]` counters.

use bevy::app::AnimationSystems;
use bevy::camera::primitives::{Frustum, Sphere as CullSphere};
use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_assets::{WmoGroupNav, WmoModel, WmoPortalRef};

use crate::entities::mount::MountBody;
use crate::net::SelfPlayer;
use crate::target::SelectionRadius;
use benilla_world::view::WorldCamera;
use benilla_world::wmo_portal::{UnitWmoRoom, WmoPortalInstance, WmoRoom};

use benilla_world::rig_anim::{AnimParked, RigPose};

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

/// Park/wake streamed rigs by the padded sphere-vs-frustum test (decision 0448) ANDed with the
/// portal-PVS room test (decision 0739 — see the module doc's "room leg"). PostUpdate,
/// before [`AnimationSystems`] — a wake drops the marker in time for the same frame's pose
/// evaluation, so the re-appearing unit samples the absolute-clock pose with no stale
/// frame. Exempt: the self-avatar (the camera rides its attachment-17 pivot, and it must keep
/// animating faded-out in first person) and its mount child. `WOW_NO_ANIM_LOD=1` disables
/// parking — the live-probe A/B lever; `WOW_NO_ROOM_LOD=1` disables only the room leg.
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
            Option<&UnitWmoRoom>,
        ),
        // The whole collapsed-rig population (decision 0739), not `With<AnimDriver>`: GameObject
        // rigs deliberately carry no driver (their looping player IS the animation — attach's GO
        // arm), and gating on the driver left every animated GO sampling and refreshing off-view
        // — at the LBRS pin, ~350 of the ~620 per-frame refreshes. Parking preserves their whole
        // observable surface: the event scanner requires `AnimDriver` (GOs never fired anim
        // events), `go_anim`'s state machine arms the player regardless of the marker, and the
        // wake samples the absolute clock. Booth/studio rigs are entity-joint-lane (no
        // `RigPose`), so this query can never park the character pane.
        With<RigPose>,
    >,
    self_hosts: Query<Has<SelfPlayer>>,
    instances: Query<&WmoPortalInstance>,
    wmos: Res<Assets<WmoModel>>,
    mut out_since: Local<EntityHashMap<f32>>,
    mut disabled: Local<Option<bool>>,
    mut park_all: Local<Option<bool>>,
    mut no_room: Local<Option<bool>>,
    mut commands: Commands,
) {
    let disabled = *disabled.get_or_insert_with(|| std::env::var_os("WOW_NO_ANIM_LOD").is_some());
    // The measurement twin of `WOW_NO_ANIM_LOD`: park EVERY streamed rig regardless of view. The
    // two levers bracket the pose-evaluation lane's cost at any pin — "never park" is its ceiling,
    // "always park" its floor — which is the only way to attribute a frame budget without a
    // profiler build (the chrome-trace feature costs a 5-minute rebuild and a GB-scale file).
    // Not a shipping mode: it freezes visible rigs on purpose.
    let park_all = *park_all.get_or_insert_with(|| std::env::var_os("WOW_ANIM_PARK_ALL").is_some());
    let no_room = *no_room.get_or_insert_with(|| std::env::var_os("WOW_NO_ROOM_LOD").is_some());
    // `[rig-gate]` counters (`WOW_RIG_COST`): `room_out` is the room leg's *marginal* park set —
    // rigs the frustum keeps that the PVS rejects — so a `WOW_NO_ROOM_LOD=1` leg reads the
    // premise number without applying the leg (counted either way, applied only when the lever
    // is off).
    let cost_on = benilla_world::rig_palette::rig_cost_enabled();
    let (mut n_rigs, mut n_parked, mut room_out) = (0u32, 0u32, 0u32);
    let Ok(frustum) = cam.single() else {
        return;
    };
    let now = time.elapsed_secs();
    for (entity, tf, radius, parked, is_self, mount, room) in &rigs {
        let exempt =
            disabled || is_self || mount.is_some_and(|m| self_hosts.get(m.host).unwrap_or(false));
        let visible = exempt
            || !park_all && {
                let scale = tf.to_scale_rotation_translation().0.max_element();
                let r = radius.map_or(FALLBACK_RADIUS, |r| r.0 * scale * RADIUS_SCALE + RADIUS_PAD);
                let sphere_in = frustum.intersects_sphere(
                    &CullSphere {
                        center: tf.translation().into(),
                        radius: r,
                    },
                    false,
                );
                // The room leg: only a rig the frustum leg kept can be room-parked.
                let room_in = !sphere_in || (no_room && !cost_on) || {
                    let ok = room_pvs_visible(room, &instances, &wmos);
                    if !ok {
                        room_out += 1;
                    }
                    ok || no_room
                };
                sphere_in && room_in
            };
        if cost_on {
            n_rigs += 1;
            n_parked += u32::from(parked);
        }
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
    if cost_on {
        eprintln!("[rig-gate] rigs={n_rigs} parked={n_parked} room_out={room_out}");
    }
    // Drop entries for rigs that despawned (or lost their model) before parking.
    out_since.retain(|e, _| rigs.contains(*e));
}

/// The room leg's resolve chain: unit claim → placement instance → model → PVS bits. Fail-open
/// at every seam (no claim, despawned placement, still-loading model) — a lookup miss must never
/// park a drawable rig.
fn room_pvs_visible(
    room: Option<&UnitWmoRoom>,
    instances: &Query<&WmoPortalInstance>,
    wmos: &Assets<WmoModel>,
) -> bool {
    let Some(WmoRoom { instance, group }) = room.and_then(|r| r.room()) else {
        return true;
    };
    let Ok(inst) = instances.get(instance) else {
        return true;
    };
    let Some(model) = wmos.get(&inst.handle) else {
        return true;
    };
    room_visible(&inst.visible, &model.group_nav, &model.portal_refs, group)
}

/// Is the claimed group — or any group one portal hop from it — in the PVS? The one-hop union is
/// the doorway-straddle guard: a body can extend past its feet's room only through a portal
/// opening, so the neighbour set bounds everything of the unit that could be on screen. Indices
/// out of range read visible (fail-open, [`benilla_world::wmo_portal::WmoGroupVis::drawn_by`]'s
/// convention); a group with no portal refs at all can only be seen with the camera inside it
/// (the flood cannot reach it), which the direct bit already answered.
fn room_visible(visible: &[bool], nav: &[WmoGroupNav], refs: &[WmoPortalRef], group: u16) -> bool {
    let vis = |g: usize| visible.get(g).copied().unwrap_or(true);
    if vis(group as usize) {
        return true;
    }
    let Some(n) = nav.get(group as usize) else {
        return true;
    };
    let Some(hops) = refs.get(n.ref_start as usize..(n.ref_start as usize + n.ref_count as usize))
    else {
        return true;
    };
    hops.iter().any(|r| vis(r.group as usize))
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

    use super::super::{events::fire_anim_events, AnimDriver, AnimSoundEvent};
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
                poses_bones: true,
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
        // The evaluator's pose buffer (0712 -> 0724); no joint entities at all.
        let skeleton = benilla_assets::ModelSkeleton {
            joints: vec![benilla_assets::ModelJoint {
                parent: -1,
                local_translation: Vec3::ZERO,
                billboard: None,
                parent_arm: None,
            }],
            spine_bone: None,
            head_bone: None,
        };
        app.world_mut()
            .entity_mut(root)
            .insert(benilla_world::rig_anim::RigPose::new(root, &skeleton));
        (root, root)
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
        app.init_asset::<WmoModel>();
        app.add_message::<AnimSoundEvent>();
        // The live schedule's shape: the gate ahead of the frame's pose evaluation; the event
        // scanner reading the advanced seek after it.
        app.add_systems(PostUpdate, gate_rig_animation.before(AnimationSystems));
        benilla_world::rig_anim::plugin(&mut app);
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

    fn bone(app: &App, rig: Entity) -> Vec3 {
        app.world()
            .entity(rig)
            .get::<benilla_world::rig_anim::RigPose>()
            .unwrap()
            .locals[0]
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

    /// A [`WmoGroupNav`] whose only meaningful fields are its portal-ref slice — the room-leg
    /// tests never touch flags/bounds.
    fn nav(ref_start: u16, ref_count: u16) -> WmoGroupNav {
        WmoGroupNav {
            flags: 0,
            bbox_min: [0.0; 3],
            bbox_max: [0.0; 3],
            ref_start,
            ref_count,
            area_table_id: 0,
            fog_indices: [0; 4],
            group_liquid: benilla_formats::NO_GROUP_LIQUID,
        }
    }

    fn pref(group: u16) -> WmoPortalRef {
        WmoPortalRef {
            portal: 0,
            group,
            side: 1,
        }
    }

    /// The room predicate's whole truth table: direct bit, the one-hop straddle guard, the
    /// all-dark park, the sealed room, and both fail-open seams (group past every table, a
    /// ref slice past the refs vec).
    #[test]
    fn room_visible_covers_the_hop_guard_and_fails_open() {
        // Groups 0 ↔ 1 share one portal; group 2 is sealed (no refs).
        let navs = vec![nav(0, 1), nav(1, 1), nav(2, 0)];
        let refs = vec![pref(1), pref(0)];
        assert!(
            room_visible(&[true, false, false], &navs, &refs, 0),
            "direct PVS bit"
        );
        assert!(
            room_visible(&[true, false, false], &navs, &refs, 1),
            "own bit dark but the neighbour lit — the doorway-straddle guard keeps it live"
        );
        assert!(
            !room_visible(&[false, false, true], &navs, &refs, 0),
            "own room and every hop dark ⇒ parked"
        );
        assert!(
            !room_visible(&[true, true, false], &navs, &refs, 2),
            "a sealed room's own bit decides — no hops exist to save it"
        );
        assert!(
            room_visible(&[false], &navs, &refs, 9),
            "a group past the PVS table reads visible (fail-open)"
        );
        assert!(
            room_visible(&[false], &[nav(7, 2)], &refs, 0),
            "a ref slice past the refs vec reads visible (fail-open)"
        );
    }

    /// The room leg end to end: an IN-FRUSTUM rig whose claimed room (and its one hop) is
    /// outside the PVS parks past the hysteresis window; setting the room's bit wakes it and its
    /// bones sample again — the same instant-wake law as the frustum leg.
    #[test]
    fn a_room_dark_rig_parks_and_wakes_when_the_pvs_reaches_it() {
        let mut app = app();
        spawn_camera(&mut app);
        // In front of the camera — only the room leg can park this rig.
        let (rig, joint) = spawn_rig(&mut app, -Vec3::Z * 20.0);
        let model = WmoModel {
            wmo_id: 1,
            portal_refs: vec![pref(1), pref(0)],
            group_nav: vec![nav(0, 1), nav(1, 1)],
            ..Default::default()
        };
        let handle = app
            .world_mut()
            .resource_mut::<Assets<WmoModel>>()
            .add(model);
        let inst = app
            .world_mut()
            .spawn(WmoPortalInstance {
                handle,
                world_from_local: bevy::math::Affine3A::IDENTITY,
                name_set: 0,
                visible: vec![false, false],
                liquid_visited: vec![false, false],
                flooded: vec![None, None],
            })
            .id();
        app.world_mut()
            .entity_mut(rig)
            .insert(UnitWmoRoom::claimed(WmoRoom {
                instance: inst,
                group: 0,
            }));
        advance(&mut app, PARK_AFTER_SECS + 0.2, 4);
        assert!(
            app.world().entity(rig).contains::<AnimParked>(),
            "in-frustum but room-dark past the hysteresis window ⇒ parked"
        );
        let frozen = bone(&app, joint);
        // The flood reaches the room (a doorway's screen window opened): wake and sample.
        app.world_mut()
            .entity_mut(inst)
            .get_mut::<WmoPortalInstance>()
            .unwrap()
            .visible[0] = true;
        advance(&mut app, DUR * 0.3, 2);
        assert!(
            !app.world().entity(rig).contains::<AnimParked>(),
            "PVS reach ⇒ instant wake"
        );
        assert_ne!(
            frozen,
            bone(&app, joint),
            "the woken rig's bones sample again"
        );
    }

    /// The coverage law (decision 0739): a driverless rig — a GameObject's, whose looping
    /// player is its whole animation — parks by the same gate. `With<RigPose>`, not
    /// `With<AnimDriver>`.
    #[test]
    fn a_driverless_go_rig_parks_too() {
        let mut app = app();
        spawn_camera(&mut app);
        let (behind, joint) = spawn_rig(&mut app, Vec3::Z * 50.0);
        app.world_mut().entity_mut(behind).remove::<AnimDriver>();
        advance(&mut app, PARK_AFTER_SECS + 0.2, 4);
        assert!(
            app.world().entity(behind).contains::<AnimParked>(),
            "no driver, still parked — the gate keys on RigPose"
        );
        let frozen = bone(&app, joint);
        advance(&mut app, DUR * 0.35, 2);
        assert_eq!(frozen, bone(&app, joint), "the parked GO rig's bones hold");
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
