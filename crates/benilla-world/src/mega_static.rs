//! **`WOW_MEGA_STATIC=1` — the consolidation bracket** (the drastic-options census, 2026-08-18).
//!
//! An EXPERIMENT lever, not a landing: when armed, the placement assembler diverts every fully
//! static batch (no anim host, no billboard, no animated material) into this buffer instead of
//! spawning an entity, and the flush below merges each material's backlog into ONE mesh with the
//! placement transforms baked into the vertices. Grouping by the deduped material handle is what
//! keeps every material semantic (blend, shade selector, batch order, two-sidedness, fog policy)
//! intact — the group IS the batch key.
//!
//! **What the bracket deliberately breaks, and why that is honest:** merged geometry has no
//! per-placement distance fade, no pick identity, no exterior-window gating, no per-group WMO
//! portal visibility, and never despawns with its tile. Those are exactly the constraints a real
//! consolidation must solve (decision 1413 enumerates them); the bracket exists to measure the
//! CEILING — what the frame costs when the static world is a few hundred rows instead of tens of
//! thousands — so the design work knows what it is buying before anyone pays for it. Parked-pin
//! measurements only; the world it draws is visually wrong in motion (pop-in never fades, culling
//! is coarse) and the flag must never default on.

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::mesh_tag::alpha_bits;
use crate::model_render::{ModelKind, ModelPart};
use benilla_assets::materials::WowModelMaterial;
use benilla_assets::merged_static_mesh;
use benilla_formats::{ModelBlend, RenderSubmesh};
use std::sync::Arc;

/// Is the bracket armed? Read once; the assembler and the flush both key on it.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WOW_MEGA_STATIC").as_deref() == Ok("1"))
}

/// One diverted batch: the shared geometry, this placement's transform, and the deduped material
/// it would have drawn with.
pub struct PendingPart {
    pub geometry: Arc<RenderSubmesh>,
    pub transform: Transform,
    pub blend: ModelBlend,
    pub kind: ModelKind,
}

/// The divert buffer. Keyed by material handle — the merge group and the draw batch are the same
/// thing by construction.
#[derive(Resource, Default)]
pub struct MegaStaticPending {
    pub parts: Vec<(Handle<WowModelMaterial>, PendingPart)>,
}

/// Merge and spawn once the stream has been quiet for [`FLUSH_QUIET_SECS`]. One entity per
/// material group per flush; a later burst (a new tile) makes a new generation of blobs rather
/// than rebuilding the old ones — acceptable for a bracket whose pins are parked.
const FLUSH_QUIET_SECS: f32 = 2.0;

/// (material, spatial cell) → the placements merging into that blob.
type MergeGroups =
    std::collections::HashMap<(Handle<WowModelMaterial>, (i32, i32)), Vec<PendingPart>>;

pub fn flush_mega_static(
    mut commands: Commands,
    mut pending: ResMut<MegaStaticPending>,
    mut meshes: ResMut<Assets<Mesh>>,
    time: Res<Time>,
    // (backlog length at last look, when it last moved) — the quiet timer, kept here so the
    // assembler needs no clock.
    mut prev: Local<(usize, f32)>,
) {
    let (prev_len, moved_at) = &mut *prev;
    if pending.parts.len() != *prev_len {
        *prev_len = pending.parts.len();
        *moved_at = time.elapsed_secs();
        return;
    }
    if pending.parts.is_empty() || time.elapsed_secs() - *moved_at < FLUSH_QUIET_SECS {
        return;
    }
    *prev_len = 0;
    let parts = std::mem::take(&mut pending.parts);
    let total = parts.len();
    // Group key = (material, SPATIAL CELL). Round 1 of this bracket grouped by material alone
    // and LOST (+1.38 cpu_ms at SW, drawn 400 → ~830): one blob per material spans the whole
    // streamed scene, so its Aabb defeats the frustum cull and every blob's full vertex load
    // encodes every frame. The cell restores locality; the distinct-material count (~5.6k at
    // SW) remains the blob-count floor either way — the census finding that per-material
    // merging alone cannot reach the few-hundred-row regime (1413).
    const CELL: f32 = 133.333;
    let cell_of = |t: &Transform| {
        (
            (t.translation.x / CELL).floor() as i32,
            (t.translation.z / CELL).floor() as i32,
        )
    };
    let mut by_mat: MergeGroups = std::collections::HashMap::new();
    for (mat, p) in parts {
        let cell = cell_of(&p.transform);
        by_mat.entry((mat, cell)).or_default().push(p);
    }
    let groups = by_mat.len();
    for ((mat, _cell), group) in by_mat {
        let (blend, kind) = (group[0].blend, group[0].kind);
        let geo: Vec<(Arc<RenderSubmesh>, Transform)> = group
            .into_iter()
            .map(|p| (p.geometry, p.transform))
            .collect();
        let (mesh, mn, mx) = merged_static_mesh(&geo);
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(mat),
            Transform::IDENTITY,
            ModelPart { kind, blend },
            MeshTag(alpha_bits(1.0)),
            Aabb::from_min_max(mn, mx),
            NoAutoAabb,
        ));
    }
    info!("mega-static: merged {total} diverted batches into {groups} blob(s)");
}
