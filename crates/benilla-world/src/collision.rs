//! Physics **collision layers** — the two collision *audiences* the world geometry is queried by.
//!
//! The player body and the third-person camera collide against *different* sets of WMO faces (a
//! binary-VERIFIED 1.12.1 fact; see `benilla_formats`'s MOPY mask doc and wow-5875-re
//! `system/collision/collision.md`): the **walking** gather drops DETAIL faces (`0x04`), the
//! **camera/LOS** gather instead drops NOCAMCOLLIDE faces (`0x02`) — so the camera collides with
//! visible decals/overhangs (forge pipes, low beams) the player walks *under*, and passes through
//! NOCAMCOLLIDE faces the player still stands on. avian can't filter a single trimesh per-face, so
//! each WMO bakes two colliders, separated by these layers; the player and camera queries pick their
//! audience by mask. Everything else — terrain, doodads, GameObjects — sits on the default layer and is
//! collided by **both** (so it needs no explicit layer).

use avian3d::character_controller::move_and_slide::{
    MoveAndSlide, MoveAndSlideConfig, MoveAndSlideHitData, MoveAndSlideHitResponse,
    MoveAndSlideOutput, MoveHitData,
};
use avian3d::prelude::*;
use bevy::ecs::component::Component;
use bevy::ecs::lifecycle::RemovedComponents;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::ResMut;
use bevy::math::{Dir3, Quat, Vec3};

mod one_sided;

/// The collision audiences. avian reserves bit 0 for [`CollisionLayer::Default`], which every collider
/// without an explicit `CollisionLayers` belongs to — so terrain/doodad/GameObject colliders are seen by
/// both the player and camera queries automatically. Only the two per-WMO meshes carry an explicit layer.
#[derive(PhysicsLayer, Default, Clone, Copy)]
pub(crate) enum CollisionLayer {
    /// Geometry both audiences collide with: terrain, doodads, GameObjects (the default layer).
    #[default]
    Default,
    /// WMO faces only the **player body** collides with — the walking gather (skip DETAIL `0x04`).
    Walk,
    /// WMO faces only the **camera** collides with — the camera/LOS gather (skip NOCAMCOLLIDE `0x02`).
    Camera,
}

/// `CollisionLayers` for a WMO's **walking** collider — member of [`CollisionLayer::Walk`] only, so the
/// camera query (which omits `Walk`) never hits it.
pub(crate) fn walk_layers() -> CollisionLayers {
    CollisionLayers::new(CollisionLayer::Walk, LayerMask::ALL)
}

/// `CollisionLayers` for a WMO's **camera/LOS** collider — member of [`CollisionLayer::Camera`] only, so
/// the player movement query (which omits `Camera`) never hits it.
pub(crate) fn camera_layers() -> CollisionLayers {
    CollisionLayers::new(CollisionLayer::Camera, LayerMask::ALL)
}

/// **How many times the world's collider set has changed** — bumped when [`crate::terrain_stream`]
/// attaches a batch of built colliders, and when any collider is removed (a tile or placement
/// streaming out, an entity despawning).
///
/// It exists because a cached collision *answer* is only as good as the world it was computed
/// against. The one consumer today is the creature ground clamp's cast gate (decision 1357), which
/// holds a hit while the unit's own inputs are unchanged: the unit's position and the colliders
/// under it. It could see the first half change and not the second, so a unit that took its answer
/// from a half-arrived world — the terrain under a building, before the building's own floor
/// collider had attached — held that answer forever and stood under the floor for the rest of the
/// session. A stamp on the collider set closes that: an answer computed at one stamp is re-asked at
/// the next.
///
/// A `u64` counter, not a dirty flag: the gate compares its own recorded stamp, so it never has to
/// coordinate with other readers about who clears it.
#[derive(Resource, Default)]
pub struct ColliderEpoch(u64);

impl ColliderEpoch {
    /// The current stamp — recorded alongside a cached collision answer, compared to decide whether
    /// that answer still describes this world.
    pub fn get(&self) -> u64 {
        self.0
    }

    /// **The collider set changed** — call this from any lane that inserts a collider outside the
    /// streamer's own attach queue (the GameObject hull lane is the one such lane today, decision
    /// 0761). A lane that forgets leaves a cached answer dated against a world that no longer
    /// exists; the cost of an over-eager bump is one re-cast.
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// Bump [`ColliderEpoch`] for the *removal* half (the attach half is published by the streamer's
/// own attach loop, which already counts what it did). Despawning an entity emits removal events
/// for its components, so this covers tile/placement stream-out and object destroys alike; reading
/// the queue is O(removals), which is zero on almost every frame.
pub(crate) fn track_collider_removals(
    mut removed: RemovedComponents<Collider>,
    mut epoch: ResMut<ColliderEpoch>,
) {
    if removed.read().count() > 0 {
        epoch.bump();
    }
}

/// Marks a static trimesh collider whose triangles **receive ground decals** (the selection ring).
/// The reference's decal collector is byte-verified (wow-re selection-circle RE, §5-cross-checked):
/// its box query gathers **terrain triangles + WMO group faces** — M2 doodads/GameObjects/units are
/// *never* collected (no collector exists for them; the ring draws under barrels, not onto them) —
/// with the decal draw sites passing flags `0x200122` (terrain on, WMO on, liquid off). The WMO side
/// filters faces only by a MOPY **material-class mask `0x88`** (no walkability/up-facing test — which
/// is why the reference ring drapes down vertical step faces). We mark the WMO **walk** collider as
/// the interim WMO face set — all faces minus DETAIL (`0x04`) — which differs from the reference's
/// minus-`0x88`-classes set only on those flagged faces (exact parity would need a third per-WMO face
/// bake; deferred until the MOPY `0x88` class semantics are RE'd).
#[derive(Component)]
pub struct GroundDecalSurface;

/// Marks a static collider that **occludes the mouse pick** — the reference's scene trace also
/// traces the world and discards the object hit iff the world hit is *strictly nearer* (wow-re
/// selection-circle RE PART 3, §5-cross-checked 2026-07-20: `0x480df0` @ `0x480eb4`,
/// `CWorld::Intersect 0x672170` mask `0x1000114`), so a unit/GameObject behind a wall is not
/// hoverable. The byte-decoded occluder set, and what carries the mark: **terrain** tiles (DDA
/// `0x69c920`), **WMO group faces** with MOPY reject-mask `0x84` — the **walk** bake (reject
/// `0x04`) is the nearest existing face set, differing only on `0x80`-flagged faces (exact parity
/// needs a third per-WMO bake, the same deferral as the decal path's `0x88`) — and **static
/// default-set map doodad hulls** (`0x69cdb0`; map doodads + WMO props here). NOT marked, per the
/// same decode: liquid (mask bits 16-19 are off on the pick path), server-spawned GameObject
/// hulls and units (they are the *object* trace — a chest must not occlude itself), and transport
/// hulls (the 0466 law: an NPC on deck stays hoverable through the railing).
#[derive(Component)]
pub struct PickOccluder;

/// **Colliders this frame's local mover must not see** — the game's half of the reference's
/// per-trace collision mask.
///
/// The engine cannot compute this: "a GameObject whose type is DOOR, while the player is a ghost"
/// names two gameplay concepts, and the engine names nothing of the game (this crate's other API
/// wall). So the game publishes the entity set and [`WorldCollision::cast_mover`] applies it.
///
/// Empty on every ordinary frame, which is what makes the living player's query provably the one it
/// always was.
#[derive(Resource, Default)]
pub struct MoverTraceExclusions(pub bevy::ecs::entity::EntityHashSet);

/// **Trace a body through the world** — the engine's movement-collision face.
///
/// Every one of the four one-sided casts took the same two leading arguments (avian's
/// `MoveAndSlide`, and a `SpatialQueryFilter` the caller built on the line before), and every
/// caller built the *same* filter. Both are the engine's business: which collision layers a body
/// sees is a property of how the world was baked (the two per-WMO face sets, `CollisionLayer`),
/// not a choice a mover gets to make.
///
/// **Deliberately not merged with `WorldPoint`** (1164 item 11): `ground_normal_under` here and
/// `terrain_height_under` there answer "what is the ground" from two different worlds — a physics
/// trimesh and an MCNK heightfield — and merging them would put avian's `MoveAndSlide` in the
/// signature of every zone-text and footstep reader.
///
/// The **one-sidedness** is the whole reason these are not avian's own calls: the reference
/// discards a face approached from its back before computing any distance, and that gate has to
/// run where candidates are enumerated (see [`one_sided`]'s header, decisions 0967/0968).
#[derive(bevy::ecs::system::SystemParam)]
pub struct WorldCollision<'w, 's> {
    ms: MoveAndSlide<'w, 's>,
    exclusions: bevy::ecs::system::Res<'w, MoverTraceExclusions>,
}

impl WorldCollision<'_, '_> {
    /// What a walking **body** collides with: the default layer (terrain, doodads, GameObjects)
    /// plus the walk-only WMO faces — and never the camera-only ones.
    ///
    /// Exposed for the two lanes that hand a filter to something other than these casts (the mouse
    /// pick's occlusion trace, the mount-tilt probe); everything else should use the methods
    /// below, which apply it themselves.
    pub fn body_filter() -> SpatialQueryFilter {
        SpatialQueryFilter::from_mask(LayerMask(
            CollisionLayer::Default.to_bits() | CollisionLayer::Walk.to_bits(),
        ))
    }

    /// …and what the third-person **camera** collides with: the other way round on those two WMO
    /// face layers (it takes NOCAMCOLLIDE faces the body walks through, and skips the DETAIL faces
    /// the body walks on).
    pub(crate) fn camera_filter() -> SpatialQueryFilter {
        SpatialQueryFilter::from_mask(LayerMask(
            CollisionLayer::Default.to_bits() | CollisionLayer::Camera.to_bits(),
        ))
    }

    /// Sweep `shape` along `movement` against the body's world.
    pub fn cast_body(
        &self,
        shape: &Collider,
        from: Vec3,
        movement: Vec3,
        skin_width: f32,
    ) -> Option<MoveHitData> {
        self.cast_body_with(shape, from, movement, skin_width, &Self::body_filter())
    }

    /// [`cast_body`](Self::cast_body) against a **caller-supplied** filter.
    ///
    /// It exists for one caller and one reason: the reference's world trace carries a per-trace
    /// mask, and a GHOST's mask drops DOOR GameObjects from the gather — a ghost walks through
    /// closed doors (wow-re `collision/scratch/ghost-door-tracemask.md`). That is a property of
    /// *this mover's trace*, not of the door, so it cannot be expressed by re-laning the collider:
    /// [`body_filter`](Self::body_filter) is shared with the particle snap, the precipitation
    /// probe, the mouse pick and the creature conform, none of which may lose a door because the
    /// local player happens to be dead.
    ///
    /// The living case passes [`body_filter`](Self::body_filter) itself, so it is the same query it
    /// always was — byte for byte, by construction rather than by inspection.
    fn cast_body_with(
        &self,
        shape: &Collider,
        from: Vec3,
        movement: Vec3,
        skin_width: f32,
        filter: &SpatialQueryFilter,
    ) -> Option<MoveHitData> {
        one_sided::cast_move(&self.ms, shape, from, movement, skin_width, filter)
    }

    /// Sweep the **camera boom** against the camera's world.
    ///
    /// Deliberately avian's own two-sided cast rather than the one-sided law above: the facing
    /// gate exists because a *body* must not stand on a face wound away from it, and a camera has
    /// no such contract — it just must not end up inside geometry, from either side.
    pub fn cast_camera(
        &self,
        shape: &Collider,
        from: Vec3,
        rotation: Quat,
        movement: Vec3,
        skin_width: f32,
    ) -> Option<MoveHitData> {
        self.ms.cast_move(
            shape,
            from,
            rotation,
            movement,
            skin_width,
            &Self::camera_filter(),
        )
    }

    /// A one-sided ray against the body's world.
    pub fn ray_body(&self, origin: Vec3, dir: Dir3, max_distance: f32) -> Option<RayHitData> {
        one_sided::cast_ray(&self.ms, origin, dir, max_distance, &Self::body_filter())
    }

    /// Move `shape` by `velocity` for `delta_time`, sliding along what it hits.
    #[allow(clippy::too_many_arguments)] // the mover's full step, minus the two the facade owns
    pub fn slide_body(
        &self,
        shape: &Collider,
        shape_position: Vec3,
        velocity: Vec3,
        delta_time: std::time::Duration,
        config: &MoveAndSlideConfig,
        on_hit: impl FnMut(MoveAndSlideHitData) -> MoveAndSlideHitResponse,
    ) -> MoveAndSlideOutput {
        self.slide_body_with(
            shape,
            shape_position,
            velocity,
            delta_time,
            config,
            &Self::body_filter(),
            on_hit,
        )
    }

    /// [`slide_body`](Self::slide_body) against a caller-supplied filter — the ghost's door
    /// exclusion, for the reason spelled out on [`cast_body_with`](Self::cast_body_with).
    #[allow(clippy::too_many_arguments)] // `slide_body`'s list, plus the filter it defaults
    fn slide_body_with(
        &self,
        shape: &Collider,
        shape_position: Vec3,
        velocity: Vec3,
        delta_time: std::time::Duration,
        config: &MoveAndSlideConfig,
        filter: &SpatialQueryFilter,
        on_hit: impl FnMut(MoveAndSlideHitData) -> MoveAndSlideHitResponse,
    ) -> MoveAndSlideOutput {
        one_sided::move_and_slide(
            &self.ms,
            shape,
            shape_position,
            velocity,
            delta_time,
            config,
            filter,
            on_hit,
        )
    }

    /// Sweep the **local mover's own body** — [`cast_body`](Self::cast_body) with this frame's
    /// [`MoverTraceExclusions`] applied.
    ///
    /// The reference's world trace carries a per-trace mask, and the local mover's mask gains bit
    /// `0x8000` when the body it is driving is a player **in ghost form** (`0x631658`); the
    /// GameObject collision-candidacy virtual `0x5f85f0` reads that bit and drops every
    /// `GAMEOBJECT_TYPE_ID == 0` (DOOR) object from the gather. So a ghost walks through closed
    /// doors, and nothing else about its collision differs (wow-re
    /// `collision/scratch/ghost-door-tracemask.md`).
    ///
    /// **Why the exclusion rides the trace and not the collider.** It is tempting to drop the
    /// door's collider, or re-lane it — both are wrong, because the fact is about *whose trace this
    /// is*. [`body_filter`](Self::body_filter) is shared with the particle snap, the precipitation
    /// probe, the mouse pick, the taxi probe and the creature ground conform; a door that stopped
    /// existing for all of them because the local player happened to be dead would be a much larger
    /// claim than the binary makes. Only the mover's own sweeps take the exclusion.
    ///
    /// With the set empty — every living frame — this is [`cast_body`](Self::cast_body) exactly.
    pub fn cast_mover(
        &self,
        shape: &Collider,
        from: Vec3,
        movement: Vec3,
        skin_width: f32,
    ) -> Option<MoveHitData> {
        self.cast_body_with(shape, from, movement, skin_width, &self.mover_filter())
    }

    /// [`slide_body`](Self::slide_body) for the local mover — see [`cast_mover`](Self::cast_mover).
    pub fn slide_mover(
        &self,
        shape: &Collider,
        shape_position: Vec3,
        velocity: Vec3,
        delta_time: std::time::Duration,
        config: &MoveAndSlideConfig,
        on_hit: impl FnMut(MoveAndSlideHitData) -> MoveAndSlideHitResponse,
    ) -> MoveAndSlideOutput {
        self.slide_body_with(
            shape,
            shape_position,
            velocity,
            delta_time,
            config,
            &self.mover_filter(),
            on_hit,
        )
    }

    /// [`body_filter`](Self::body_filter), plus whatever this frame's mover must not see.
    ///
    /// Allocation-free while the set is empty (`EntityHashSet::default()` allocates nothing), which
    /// is every frame the player is alive.
    fn mover_filter(&self) -> SpatialQueryFilter {
        Self::body_filter().with_excluded_entities(self.exclusions.0.iter().copied())
    }

    /// Every front-facing triangle in a box around `at` — the step probe's face gather.
    pub fn faces_near_body(&self, at: Vec3, half: Vec3, limit: usize) -> Vec<one_sided::FaceProbe> {
        one_sided::faces_near(&self.ms, at, half, &Self::body_filter(), limit)
    }
}

/// The contact pipeline we deliberately do not run (decision 1232, disabled in `world_plugins`).
///
/// These two tests are a matched pair: the first shows the cost was real — a kinematic trimesh
/// resting in the static world generates contact pairs, and avian then computes a trimesh-vs-trimesh
/// manifold for each, every physics tick — and the second shows that dropping the broad phase
/// removes them *without* costing the shape-casts the character controller actually rides.
#[cfg(test)]
mod contact_pipeline {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::*;

    /// A static 10×10 floor plus a kinematic 4×4 slab overlapping it — the transport-on-terrain
    /// shape that was generating manifolds against 65k-triangle tiles in the real world.
    fn overlapping_world(broad_phase: bool) -> App {
        let mut app = App::new();
        let physics = PhysicsPlugins::new(bevy::app::PostUpdate);
        app.add_plugins((
            MinimalPlugins,
            bevy::transform::TransformPlugin,
            bevy::asset::AssetPlugin::default(),
            bevy::scene::ScenePlugin,
        ));
        if broad_phase {
            app.add_plugins(physics);
        } else {
            app.add_plugins(physics.build().disable::<BvhBroadPhasePlugin>());
        }
        app.init_asset::<Mesh>();
        // `update()` never runs plugin `finish()`, where avian seats its diagnostics resources.
        app.finish();
        app.cleanup();

        let quad = |half: f32, y: f32| {
            (
                vec![
                    Vec3::new(-half, y, -half),
                    Vec3::new(half, y, -half),
                    Vec3::new(half, y, half),
                    Vec3::new(-half, y, half),
                ],
                vec![[0u32, 2, 1], [0, 3, 2]],
            )
        };
        let (fv, ft) = quad(5.0, 0.0);
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::trimesh(fv, ft),
            Transform::default(),
        ));
        // Overlapping, not merely adjacent: same plane, so the AABBs intersect and the narrow
        // phase has real triangle work to do.
        let (sv, st) = quad(2.0, 0.0);
        app.world_mut().spawn((
            RigidBody::Kinematic,
            Collider::trimesh(sv, st),
            Transform::default(),
        ));
        // Two frames, not one: the first seats `Position`/`Rotation` and the collider trees, and
        // the physics schedule first steps on the second. A single-`update()` fixture (which is
        // what `one_sided`'s is) never runs a physics step at all.
        app.update();
        app.update();
        app
    }

    fn active_pairs(app: &App) -> usize {
        app.world().resource::<ContactGraph>().active_pairs().len()
    }

    #[test]
    fn the_stock_plugin_set_generates_contact_pairs_we_never_read() {
        // The bug's premise. If this ever reads 0, avian changed and 1232's reasoning needs a
        // re-read before the disable below can still be justified as a saving.
        assert!(
            active_pairs(&overlapping_world(true)) > 0,
            "expected the stock broad phase to pair the kinematic slab with the static floor"
        );
    }

    #[test]
    fn dropping_the_broad_phase_removes_the_pairs_but_not_the_shape_casts() {
        let mut app = overlapping_world(false);
        assert_eq!(
            active_pairs(&app),
            0,
            "no broad phase means no contact pairs, so nothing to build manifolds for"
        );
        // The half that must NOT regress: the collider BVH is `ColliderTreePlugin`'s, not the
        // broad phase's, so a downward cast still finds the floor the player walks on.
        let hit = app
            .world_mut()
            .run_system_once(|spatial: SpatialQuery| {
                spatial.cast_ray(
                    Vec3::new(0.0, 5.0, 0.0),
                    Dir3::NEG_Y,
                    10.0,
                    true,
                    &SpatialQueryFilter::default(),
                )
            })
            .expect("run_system_once");
        assert!(
            hit.is_some(),
            "the shape-cast lane must survive the broad phase being gone"
        );
    }
}
