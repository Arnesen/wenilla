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

use avian3d::prelude::*;
use bevy::ecs::component::Component;

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

/// Spatial-query filter for the **player body**: the default layer (terrain/doodads/GameObjects) plus
/// the walk-only WMO faces — and *not* the camera-only WMO faces.
pub(crate) fn player_query_filter() -> SpatialQueryFilter {
    SpatialQueryFilter::from_mask(LayerMask(
        CollisionLayer::Default.to_bits() | CollisionLayer::Walk.to_bits(),
    ))
}

/// Spatial-query filter for the **third-person camera**: the default layer plus the camera-only WMO
/// faces — and *not* the walk-only (NOCAMCOLLIDE) WMO faces.
pub(crate) fn camera_query_filter() -> SpatialQueryFilter {
    SpatialQueryFilter::from_mask(LayerMask(
        CollisionLayer::Default.to_bits() | CollisionLayer::Camera.to_bits(),
    ))
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
pub(crate) struct GroundDecalSurface;

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
pub(crate) struct PickOccluder;
