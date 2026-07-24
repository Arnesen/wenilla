//! Ground-level spell-effect quads as **projected surface decals** — the third client of the
//! shared decal projector (`decal.rs`), after the selection ring and the unit blob shadow.
//!
//! A base-anchored effect model's flat ground-plane quads (Battle Shout's six crescents, the
//! paladin auras' rings — [`benilla_formats::GroundQuad`], 128 of the 139 flat batches under
//! `Spells\` per the `groundscan` sweep) author every vertex at z = 0 with depth-test ON: drawn as
//! free geometry they are buried per-pixel by the first up-slope and float over the first
//! down-slope. The real 1.12 client draws them exactly that way (standard M2 batch pipeline —
//! no ground conform exists in the spell-visual chain); this lane is a deliberate modern
//! improvement, director-directed: anything that *needs ground level* renders like the ring does.
//!
//! Mechanism: [`crate::entities`]' fx attach spawns one [`GroundFxDecal`] entity per ground quad
//! of a base-anchored instance instead of a mesh child. Each frame ([`update_ground_fx_decals`],
//! in [`crate::billboard::BillboardPlace`] — post-propagation, like the cards), the quad's four
//! authored corners are posed through its live joint × the bone's inverse bindpose (exactly the
//! skinned-vertex path, so the authored slide/spin/scale animation is preserved), a projection
//! frame is fitted to the posed rectangle, and the ground triangles inside it are emitted with
//! the quad's own UVs bilerped across the frame — the crescent drapes the terrain it crosses.
//! The part's material rides along (additive blend, animated tint clone, `MatAnim` alpha loops),
//! plus the decal depth bias that settles coplanarity with the drawn ground. A decal despawns
//! when its joint does (the effect instance's reap/self-termination), the billboard cards' orphan
//! rule; it hides when no receiving surface is in the box (mid-air), the ring's no-ground gate.

use avian3d::prelude::Collider;
use benilla_assets::coords::wow_to_bevy;
use benilla_formats::GroundQuad;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::Affine3A;
use bevy::prelude::*;

use crate::collision::GroundDecalSurface;
use crate::decal::{project_decal, seed_mesh, DecalFrame};
use crate::terrain::WowModelMaterial;

/// Rasterizer depth bias on ground-fx decal materials — the same constant, for the same reason,
/// as the ring's (`RING_DEPTH_BIAS`, see the rationale there): the projected vertices are
/// geometrically coplanar with the drawn ground, and this pushes only their *depth* far above the
/// f32 noise floor. Ties against the ring/shadow don't matter: all three write no depth, so the
/// bias only ever competes with the opaque ground.
pub(crate) const GROUND_FX_DEPTH_BIAS: f32 = 8192.0;

/// One ground-quad decal: the live effect-rig joint it rides, and the authored quad it projects.
#[derive(Component)]
pub(crate) struct GroundFxDecal {
    /// The joint entity whose pose animates the quad (the effect model's own rig; the instance
    /// root for a boneless model). Its despawn — the instance's reap — despawns the decal.
    joint: Entity,
    /// The bone's inverse bindpose (identity for a boneless model): a corner's world position is
    /// `joint_global × ibp × corner`, exactly what the skinned mesh would compute.
    ibp: Mat4,
    /// The quad corners in Bevy model space (y = 0), normalized at spawn so the fitted frame's
    /// `+z'` axis matches the UV `t` axis (see [`spawn_ground_fx_decal`]).
    corners: [Vec3; 4],
    /// The authored UV at each corner, parallel to `corners`.
    uvs: [[f32; 2]; 4],
}

/// Spawn one ground-quad decal entity (a world-root entity — placement is absolute, written by
/// [`update_ground_fx_decals`]). `material` is the part's resolved per-instance material (the
/// caller bakes in [`GROUND_FX_DEPTH_BIAS`] and any animated-tint clone); the caller also inserts
/// the part's `ModelPart`/`MatAnim` riders. Spawns `Hidden` — the first placement pass shows it.
pub(crate) fn spawn_ground_fx_decal(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<WowModelMaterial>,
    quad: &GroundQuad,
    joint: Entity,
    ibp: Mat4,
) -> Entity {
    let mut corners = quad.corners.map(wow_to_bevy);
    let mut uvs = quad.uvs;
    // Normalize corner handedness once, at rest: the WoW→Bevy rotation may map the authored
    // (+x, +y) rect axes to a pair whose second axis lands on the fitted frame's −z'. Runtime
    // joint transforms are proper rotations × positive scales, so this relative handedness is
    // invariant — one row swap here keeps the bilerp `t` axis aligned with `+z'` forever.
    let ex = corners[1] - corners[0] + corners[3] - corners[2];
    let ez = corners[2] - corners[0] + corners[3] - corners[1];
    if ez.z * ex.x - ez.x * ex.z < 0.0 {
        corners.swap(0, 2);
        corners.swap(1, 3);
        uvs.swap(0, 2);
        uvs.swap(1, 3);
    }
    commands
        .spawn((
            GroundFxDecal {
                joint,
                ibp,
                corners,
                uvs,
            },
            Mesh3d(meshes.add(seed_mesh())),
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden,
            // The mesh is rewritten in place every frame and Bevy won't recompute the Aabb on asset
            // change (the ring's rule); live decals are bounded by live casts, so skip culling.
            NoFrustumCulling,
        ))
        .id()
}

/// Fit a projection frame to the posed quad: center at the corner mean, the frame's `x'` axis
/// along the (horizontally projected) `c0→c1` edge, half-extents from the averaged edge lengths,
/// the ring's vertical slab (±2 × the larger half-extent, so a ledge catches a fading smear).
/// `None` for a degenerate pose — the scale-0 first animation frame, or an edge-on tilt.
fn fit_frame(corners: &[Vec3; 4]) -> Option<DecalFrame> {
    let center = (corners[0] + corners[1] + corners[2] + corners[3]) * 0.25;
    let ex = (corners[1] - corners[0] + corners[3] - corners[2]) * 0.5;
    let ez = (corners[2] - corners[0] + corners[3] - corners[1]) * 0.5;
    let exh = Vec2::new(ex.x, ex.z);
    let (half_x, half_z) = (exh.length() * 0.5, Vec2::new(ez.x, ez.z).length() * 0.5);
    if half_x < 1e-3 || half_z < 1e-3 {
        return None;
    }
    let d = exh / (half_x * 2.0);
    let vert = 2.0 * half_x.max(half_z);
    Some(DecalFrame {
        center,
        // `in_frame` computes `x' = dx·cos − dz·sin`: cos = d.x, sin = −d.y sends the posed
        // `c0→c1` edge direction onto the frame's +x' axis.
        sin: -d.y,
        cos: d.x,
        min_x: -half_x,
        max_x: half_x,
        min_z: -half_z,
        max_z: half_z,
        min_y: -vert,
        max_y: vert,
    })
}

/// Bilinear UV over the quad's authored corner UVs at normalized frame coordinates `(s, t)`.
fn bilerp_uv(uvs: &[[f32; 2]; 4], s: f32, t: f32) -> [f32; 2] {
    let lerp2 =
        |a: [f32; 2], b: [f32; 2], k: f32| [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k];
    lerp2(lerp2(uvs[0], uvs[1], s), lerp2(uvs[2], uvs[3], s), t)
}

/// Per-frame placement (in [`crate::billboard::BillboardPlace`] — post-propagation, so the joint
/// pose is THIS frame's): pose each decal's corners through its joint, fit the frame, re-project
/// the mesh, and write the absolute world transform directly (the cards' direct-global rule).
/// Orphaned decals (joint despawned with its effect instance) despawn; a frame with no receiving
/// ground hides (the ring's no-ground gate).
pub(crate) fn update_ground_fx_decals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    surfaces: Query<&Collider, With<GroundDecalSurface>>,
    joints: Query<&GlobalTransform, Without<GroundFxDecal>>,
    mut decals: Query<(
        Entity,
        &GroundFxDecal,
        &Mesh3d,
        &mut Transform,
        &mut GlobalTransform,
        &mut Visibility,
    )>,
) {
    for (entity, decal, mesh, mut tf, mut global, mut vis) in &mut decals {
        let Ok(joint) = joints.get(decal.joint) else {
            commands.entity(entity).despawn();
            continue;
        };
        let pose = joint.affine() * Affine3A::from_mat4(decal.ibp);
        let corners = decal.corners.map(|c| pose.transform_point3(c));
        let projected = fit_frame(&corners).is_some_and(|frame| {
            let vert = frame.max_y;
            let ok = project_decal(
                &mut meshes,
                mesh,
                &surfaces,
                &frame,
                // The ring's vertical trapezoid: full within half the slab, fading to 0 at its
                // edge — a wall/ledge smear dims with height instead of ending in a hard clip.
                |p| ((vert - p.y.abs()) / (0.75 * vert)).clamp(0.0, 1.0),
                |x, z| {
                    let s = (x - frame.min_x) / (frame.max_x - frame.min_x);
                    let t = (z - frame.min_z) / (frame.max_z - frame.min_z);
                    bilerp_uv(&decal.uvs, s, t)
                },
            );
            if ok {
                tf.translation = frame.center;
                tf.rotation = Quat::IDENTITY;
                tf.scale = Vec3::ONE;
                // Propagation already ran this frame — the direct global write is what renders.
                *global = GlobalTransform::from(*tf);
            }
            ok
        });
        *vis = if projected {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A yawed, scaled rect must fit a frame that reproduces its extents and maps corners to the
    /// bilerp's (s, t) corners.
    #[test]
    fn fitted_frame_matches_posed_rect() {
        // A 2×1 rect (half-extents 1.0 / 0.5) yawed 30° about Y, raised to y = 3.
        let yaw = 30_f32.to_radians();
        let rot = Quat::from_rotation_y(yaw);
        let base = [
            Vec3::new(-1.0, 0.0, -0.5),
            Vec3::new(1.0, 0.0, -0.5),
            Vec3::new(-1.0, 0.0, 0.5),
            Vec3::new(1.0, 0.0, 0.5),
        ];
        let corners = base.map(|c| rot * c + Vec3::new(2.0, 3.0, -4.0));
        let frame = fit_frame(&corners).expect("non-degenerate");
        assert!((frame.center - Vec3::new(2.0, 3.0, -4.0)).length() < 1e-5);
        assert!((frame.max_x - 1.0).abs() < 1e-5 && (frame.max_z - 0.5).abs() < 1e-5);
        // Corner c0 must land at the frame's (min_x, min_z); c3 at (max_x, max_z).
        let at = |p: Vec3| {
            let (dx, dz) = (p.x - frame.center.x, p.z - frame.center.z);
            (
                dx * frame.cos - dz * frame.sin,
                dz * frame.cos + dx * frame.sin,
            )
        };
        let (x0, z0) = at(corners[0]);
        let (x3, z3) = at(corners[3]);
        assert!((x0 - frame.min_x).abs() < 1e-5 && (z0 - frame.min_z).abs() < 1e-5);
        assert!((x3 - frame.max_x).abs() < 1e-5 && (z3 - frame.max_z).abs() < 1e-5);
        // The vertical slab is ±2 × the larger half-extent.
        assert!((frame.max_y - 2.0).abs() < 1e-5 && (frame.min_y + 2.0).abs() < 1e-5);
    }

    /// A zero-scale pose (the first frame of an outward-scaling ring) fits no frame.
    #[test]
    fn degenerate_pose_fits_nothing() {
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert!(fit_frame(&[p, p, p, p]).is_none());
    }

    /// Corner UVs bilerp exactly at the corners and average at the middle.
    #[test]
    fn bilerp_hits_corners() {
        let uvs = [[0.0, 0.0], [0.25, 0.0], [0.0, 1.0], [0.25, 1.0]];
        assert_eq!(bilerp_uv(&uvs, 0.0, 0.0), [0.0, 0.0]);
        assert_eq!(bilerp_uv(&uvs, 1.0, 0.0), [0.25, 0.0]);
        assert_eq!(bilerp_uv(&uvs, 0.0, 1.0), [0.0, 1.0]);
        assert_eq!(bilerp_uv(&uvs, 1.0, 1.0), [0.25, 1.0]);
        assert_eq!(bilerp_uv(&uvs, 0.5, 0.5), [0.125, 0.5]);
    }
}
