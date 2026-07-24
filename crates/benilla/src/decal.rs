//! The shared world-surface **decal projector** — the reference's own ground-decal mechanism
//! (wow-re selection-circle RE §2 + unit-blob-shadow RE, the `0x6d7330` → `0x6d6fa0` matrices →
//! `0x6d7480` emit chain): gather the triangles of every [`GroundDecalSurface`] collider (terrain
//! tiles + WMO faces — **never** doodads/GameObjects) whose BVH overlaps a projection box, clip
//! each to the box ([`clip_to_frame`]), and emit them with planar top-down UVs. Because the
//! emitted triangles are exact sub-pieces of the drawn surfaces, a decal is pixel-coplanar with
//! what's on screen (the caller's `depth_bias` settles the depth test) and drapes down steps and
//! ledge faces precisely like the reference (a vertical face gets the smeared texel column of its
//! XZ spot: projective texturing, faithfully).
//!
//! Two clients — the same emit loop in the binary: the **selection ring**
//! ([`crate::target`]`::ring`, collector flags `0x200122`) and the **unit blob shadow**
//! ([`crate::blob_shadow`], flags `0x2f0122` — the ring's + the liquid receivers, a gap here:
//! liquid surfaces aren't in the [`GroundDecalSurface`] set yet).

use avian3d::parry::bounding_volume::{Aabb as ParryAabb, BoundingVolume};
use avian3d::prelude::Collider;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::collision::GroundDecalSurface;

/// A decal mesh's initial (placeholder) contents: one degenerate triangle, so the vertex-buffer
/// layout exists before the first projection. [`project_decal`] rewrites every attribute per
/// rebuild.
pub(crate) fn seed_mesh() -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0, 0.0, 0.0]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0, 1.0, 1.0, 1.0]; 3]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
    mesh
}

/// A decal's projection box: a yaw-rotated horizontal rectangle × a vertical slab, all relative
/// to `center` (the owning object's feet — the mesh transform places the emitted positions there).
/// The horizontal bounds live in the **rotated frame** (`x' = dx·cos − dz·sin`,
/// `z' = dz·cos + dx·sin`); UVs map `[min_x, max_x] × [min_z, max_z]` to `[0,1]²`, so the texture
/// square IS this rectangle. An axis-aligned box passes `(sin, cos) = (0, 1)`.
pub(crate) struct DecalFrame {
    pub center: Vec3,
    pub sin: f32,
    pub cos: f32,
    pub min_x: f32,
    pub max_x: f32,
    pub min_z: f32,
    pub max_z: f32,
    /// Vertical bounds relative to `center.y` (`min_y` below, `max_y` above).
    pub min_y: f32,
    pub max_y: f32,
}

impl DecalFrame {
    /// In-frame horizontal coordinates of a world point (the same −θ rotation the UVs use).
    fn in_frame(&self, p: Vec3) -> (f32, f32) {
        let (dx, dz) = (p.x - self.center.x, p.z - self.center.z);
        (dx * self.cos - dz * self.sin, dz * self.cos + dx * self.sin)
    }

    /// The default UV map — the texture square IS the frame rectangle:
    /// `[min_x, max_x] × [min_z, max_z] → [0,1]²` (the ring's and blob shadow's mapping). The
    /// ground-fx lane substitutes a bilinear map over the source quad's authored corner UVs.
    pub fn rect_uv(&self, x: f32, z: f32) -> [f32; 2] {
        [
            (x - self.min_x) / (self.max_x - self.min_x),
            (z - self.min_z) / (self.max_z - self.min_z),
        ]
    }

    /// The world-axis-aligned gather AABB bounding the rotated box (for the BVH broad phase).
    fn gather_aabb(&self) -> ParryAabb {
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for (x, z) in [
            (self.min_x, self.min_z),
            (self.min_x, self.max_z),
            (self.max_x, self.min_z),
            (self.max_x, self.max_z),
        ] {
            // Inverse of `in_frame`: world offset = R(θ)·(x', z').
            let dx = x * self.cos + z * self.sin;
            let dz = z * self.cos - x * self.sin;
            lo = lo.min(Vec2::new(dx, dz));
            hi = hi.max(Vec2::new(dx, dz));
        }
        ParryAabb::new(
            Vec3::new(
                self.center.x + lo.x,
                self.center.y + self.min_y,
                self.center.z + lo.y,
            ),
            Vec3::new(
                self.center.x + hi.x,
                self.center.y + self.max_y,
                self.center.z + hi.y,
            ),
        )
    }
}

/// Rebuild `mesh` as a projected surface decal: gather + clip the [`GroundDecalSurface`]
/// triangles to `frame`'s box and emit them with top-down UVs (positions **relative to
/// `frame.center`** — the caller's transform places them). `alpha` computes each vertex's colour
/// alpha from its in-frame position `(x', y_rel, z')` (vertical fades, edge ramps); `uv` maps
/// in-frame `(x', z')` to the emitted texture coordinate ([`DecalFrame::rect_uv`] for the plain
/// texture-square decals; the ground-fx lane bilerps its quad's authored corner UVs). Returns
/// `false` when nothing was gathered (no receiving surface in the box) — the caller hides the
/// decal, the reference's own no-ground gate (`0x6d74b5`: the whole draw is skipped).
pub(crate) fn project_decal(
    meshes: &mut Assets<Mesh>,
    mesh: &Mesh3d,
    surfaces: &Query<&Collider, With<GroundDecalSurface>>,
    frame: &DecalFrame,
    alpha: impl Fn(Vec3) -> f32,
    uv: impl Fn(f32, f32) -> [f32; 2],
) -> bool {
    let gather = frame.gather_aabb();
    if frame.max_x - frame.min_x <= 0.0 || frame.max_z - frame.min_z <= 0.0 {
        return false;
    }
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for collider in surfaces {
        // The marked colliders are static trimeshes with world-space vertices (identity pose), so
        // their local AABB/triangles are world AABB/triangles.
        let Some(trimesh) = collider.shape().as_trimesh() else {
            continue;
        };
        if !trimesh.local_aabb().intersects(&gather) {
            continue;
        }
        for i in trimesh.bvh().intersect_aabb(&gather) {
            let tri = trimesh.triangle(i);
            let poly = clip_to_frame([tri.a, tri.b, tri.c], frame);
            if poly.len() < 3 {
                continue;
            }
            let base = positions.len() as u32;
            for p in &poly {
                let d = *p - frame.center;
                positions.push([d.x, d.y, d.z]);
                let (u, v) = frame.in_frame(*p);
                uvs.push(uv(u, v));
                let a = alpha(Vec3::new(u, d.y, v));
                colors.push([1.0, 1.0, 1.0, a]);
            }
            // Fan-triangulate the clipped convex polygon.
            for k in 1..poly.len() as u32 - 1 {
                indices.extend([base, base + k, base + k + 1]);
            }
        }
    }
    if positions.is_empty() {
        return false;
    }
    let Some(mesh) = meshes.get_mut(mesh.id()) else {
        return false;
    };
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0]; positions.len()],
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    true
}

/// Sutherland–Hodgman clip of a triangle against the frame's projection box: the yaw-rotated
/// horizontal rectangle (clipping in the rotated frame is exactly the texture frame, so UVs stay
/// in `[0,1]` and the texture can never wrap ghost copies in at the corners) and the vertical
/// slab. Interpolates full 3D positions along clipped edges, so the result stays on the source
/// triangle's plane. Returns fewer than 3 vertices when the triangle lies outside the box.
fn clip_to_frame(tri: [Vec3; 3], frame: &DecalFrame) -> Vec<Vec3> {
    let rx = |p: Vec3| frame.in_frame(p).0;
    let rz = |p: Vec3| frame.in_frame(p).1;
    // Signed inside-distances for the six half-planes of the box.
    let planes: [&dyn Fn(Vec3) -> f32; 6] = [
        &|p: Vec3| frame.max_x - rx(p),
        &|p: Vec3| rx(p) - frame.min_x,
        &|p: Vec3| frame.max_z - rz(p),
        &|p: Vec3| rz(p) - frame.min_z,
        &|p: Vec3| (frame.center.y + frame.max_y) - p.y,
        &|p: Vec3| p.y - (frame.center.y + frame.min_y),
    ];
    let mut poly: Vec<Vec3> = tri.to_vec();
    for dist in planes {
        let mut out = Vec::with_capacity(poly.len() + 1);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (da, db) = (dist(a), dist(b));
            if da >= 0.0 {
                out.push(a);
            }
            // The edge crosses the plane → emit the intersection point.
            if (da >= 0.0) != (db >= 0.0) {
                out.push(a + (b - a) * (da / (da - db)));
            }
        }
        poly = out;
        if poly.len() < 3 {
            return poly;
        }
    }
    poly
}
