//! `$WOW_PARTICLE_DEPTHDUMP` — the CPU half of the particle **depth-contest** measurement (B16).
//!
//! Prints, for every live world-lane quad emitter, the numbers the depth compare is decided by:
//! each particle's quad centre in the world, its rendered half-extent, its view-space z, the NDC
//! depth its quad carries — with the four corners' own NDC depths, so "all corners share the
//! centre's depth" (wow-re `part-flush-emitter-depth.md` §4, the mechanism that lets a flush
//! emitter draw) is *measured* in our pipeline, not assumed — and where the quad lands in
//! **physical pixels**.
//!
//! Pair it with `WOW_DEPTH` at pixels inside the printed rect (same physical-pixel space; MSAA
//! off): that probe answers *what depth the opaque pass left in the buffer*; this one answers
//! *what depth our quad brings to the compare*. Reverse-Z: the fragment survives iff
//! `dquad ≥ d_buffer`. `buffer view-z − quad view-z` at the centre pixel is the emitter's burial
//! as our pipeline actually experiences it — the number to put beside the reference's 0.0058-yd
//! burial / 33–53 % quad survival, without a single screenshot.
//!
//! `WOW_PARTICLE_DEPTHDUMP=<at>[,<frames>]` — dump every frame from `at` seconds elapsed for
//! `<frames>` frames (default 8). One line per particle, capped at 4 per emitter per frame (a
//! flush glow's particles are near-coincident; the cap keeps a 50/s pool readable).

use bevy::camera::Projection;
use bevy::prelude::*;

use super::quads::{draw_gated, particle_center, particle_half, CamBasis, DrawFrame};
use super::Particle;
use benilla_formats::ParticleEmitterDef;

/// Parsed `$WOW_PARTICLE_DEPTHDUMP`: (start seconds, frame count).
static WINDOW: std::sync::LazyLock<Option<(f32, u32)>> = std::sync::LazyLock::new(|| {
    let v = std::env::var("WOW_PARTICLE_DEPTHDUMP").ok()?;
    let (at, n) = match v.split_once(',') {
        Some((a, b)) => (a, b),
        None => (v.as_str(), ""),
    };
    Some((at.trim().parse().ok()?, n.trim().parse().unwrap_or(8)))
});

/// Parsed `$WOW_PARTICLE_DEPTHDUMP_BONES` — dump ONLY emitters mounted on these bones (empty = all).
/// A live world scene has dozens of drawing emitters; unfiltered, the dump floods the log and costs
/// the framerate of the very session it is measuring. Scoping to the subject's bones (the
/// voidwalker's eyes are 60/61) makes the dump free when the subject is off screen, so the window
/// can stay open for a whole director-driven session instead of a timed guess.
static BONES: std::sync::LazyLock<Vec<u32>> = std::sync::LazyLock::new(|| {
    std::env::var("WOW_PARTICLE_DEPTHDUMP_BONES")
        .ok()
        .map(|v| v.split(',').filter_map(|b| b.trim().parse().ok()).collect())
        .unwrap_or_default()
});

/// Is this emitter's bone in the `$WOW_PARTICLE_DEPTHDUMP_BONES` scope (always true when unset)?
pub(super) fn bone_selected(bone: u32) -> bool {
    BONES.is_empty() || BONES.contains(&bone)
}

/// This frame's dump index if the window is open (advances the counter), else `None`. Called once
/// per `simulate_particles` run.
pub(super) fn frame(elapsed: f32, count: &mut u32) -> Option<u32> {
    let (at, frames) = (*WINDOW)?;
    (elapsed >= at && *count < frames).then(|| {
        let f = *count;
        *count += 1;
        f
    })
}

/// Dump one emitter's live pool against the camera it billboards toward.
#[cold]
#[allow(clippy::too_many_arguments)] // the sim loop's full draw context, verbatim
pub(super) fn dump_emitter(
    fidx: u32,
    def: &ParticleEmitterDef,
    particles: &[Particle],
    dframe: &DrawFrame,
    placement: &Transform,
    // The world point births are folded through THIS frame (the sim's own `emitter_world`, passed
    // rather than recomputed so it cannot drift from the formula births actually use). Tracked
    // across a dump window it measures our **eye-bone sway** — the number wow-re's rig puts at
    // 0.128 units (11.7 cm) over the Stand cycle, and the discriminator for "do our births sample
    // the current animated palette or a rest pose" (`part-anchoring-live-bone.md` §5.2).
    birth_world: Vec3,
    basis: &CamBasis,
    cam_tf: &GlobalTransform,
    camera: &Camera,
    projection: &Projection,
    // The sim's own draw gate: a non-resident texture means `expand_quads` was SKIPPED and none
    // of these quads rasterized — numbers that look drawable but weren't. Stated on the line so
    // the reader can't pair a withheld pool with a framebuffer.
    texture_resident: bool,
    // The emitter entity's own visibility, and the mesh asset as the LAST expansion left it (the
    // dump runs just before this frame's expansion) — what the draw actually has in hand.
    vis: Visibility,
    mesh_positions: Option<&Mesh>,
) {
    let Some(vp) = camera.physical_viewport_size() else {
        return;
    };
    // The mesh's OWN first quad, exactly as last expansion wrote it (anchor-relative) — read back
    // from the asset rather than recomputed, so vertex-data corruption is visible as itself.
    // An EMPTY buffer is a real state, not an impossible one: an emitter's pool goes live one
    // frame before that pool is first expanded into the mesh, so a live-world dump catches
    // `pos == []` and indexing `pos[0]` panics the sim (it did, mid-session, on the director's
    // client). Report the empty mesh as itself — it is exactly the "drawable numbers, nothing in
    // the vertex buffer" state this dump exists to make visible.
    if let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) =
        mesh_positions.and_then(|m| m.attribute(Mesh::ATTRIBUTE_POSITION))
    {
        if pos.is_empty() {
            info!("PARTICLE_DEPTHDUMP f={fidx} mesh EMPTY (0 verts — pool live, not yet expanded)");
        } else {
            // Per-quad diagonal |v2−v0| over the WHOLE buffer — the young (big) quads live at the
            // tail, and only reading quad 0 (the oldest, smallest) once mis-called this mesh "sane".
            let diag = |q: &[[f32; 3]]| (Vec3::from(q[2]) - Vec3::from(q[0])).length();
            let quads: Vec<f32> = pos.chunks_exact(4).map(diag).collect();
            let (mut dmin, mut dmax) = (f32::MAX, f32::MIN);
            for &d in &quads {
                dmin = dmin.min(d);
                dmax = dmax.max(d);
            }
            info!(
                "PARTICLE_DEPTHDUMP f={fidx} mesh quads={} diag_min={dmin:.4} diag_max={dmax:.4} \
             first=({:.4},{:.4},{:.4}) last=({:.4},{:.4},{:.4})",
                quads.len(),
                pos[0][0],
                pos[0][1],
                pos[0][2],
                pos[pos.len() - 1][0],
                pos[pos.len() - 1][1],
                pos[pos.len() - 1][2],
            );
        }
    }
    let view_from_world = cam_tf.to_matrix().inverse();
    let clip_from_view = projection.get_clip_from_view();
    // (ndc, view-z) of a world point; `None` behind the near plane. The same matrices the frame
    // renders with, so `ndc.z` IS the depth the rasterizer interpolates for this vertex.
    let project = |world: Vec3| -> Option<(Vec3, f32)> {
        let v = view_from_world * world.extend(1.0);
        let clip = clip_from_view * v;
        (clip.w > 0.0).then(|| (clip.truncate() / clip.w, -v.z))
    };
    let px = |ndc: Vec3| {
        Vec2::new(
            (ndc.x + 1.0) * 0.5 * vp.x as f32,
            (1.0 - ndc.y) * 0.5 * vp.y as f32,
        )
    };
    info!(
        "PARTICLE_DEPTHDUMP f={fidx} emitter bone={} pos=({:.3},{:.3},{:.3}) blend={:?} pool={} \
         tex_resident={texture_resident} mesh_verts={} vis={vis:?} \
         birth=({:.4},{:.4},{:.4}) joint=({:.4},{:.4},{:.4})",
        def.bone,
        def.position[0],
        def.position[1],
        def.position[2],
        def.blend,
        particles.len(),
        mesh_positions.map_or(0, Mesh::count_vertices),
        birth_world.x,
        birth_world.y,
        birth_world.z,
        placement.translation.x,
        placement.translation.y,
        placement.translation.z,
    );
    // Sample the pool EVENLY by index (retain order = age order), not the first four: the pool's
    // head is its oldest, near-contemporary particles, and a cap there hides both the young end
    // (the big, bright quads) and the pool's spatial spread across its age span.
    let stride = (particles.len() / 4).max(1);
    for (i, p) in particles
        .iter()
        .step_by(stride)
        .filter(|p| !draw_gated(def, p))
        .take(6)
        .enumerate()
    {
        let center = particle_center(dframe, placement, p);
        let half = particle_half(def, placement, p);
        // The additive contribution's other factor: the over-life colour this particle's quad
        // carries (raw authored values, as `expand_quads` pushes them).
        let rgba = def
            .over_life
            .sample((p.age / def.lifespan).clamp(0.0, 1.0))
            .color;
        let Some((ndc, viewz)) = project(center) else {
            info!("PARTICLE_DEPTHDUMP f={fidx} p{i} behind the camera");
            continue;
        };
        let c_px = px(ndc);
        // The quad exactly as `expand_quads` builds it (spin ignored — a spun square covers the
        // same disc). Each corner's own NDC depth goes into `corners=[min,max]`: if that span is
        // not ≈ dquad on both ends, our billboard is NOT the constant-depth plane the reference's
        // mechanism depends on — that would itself be the finding.
        let (r, u) = (basis.right * half, basis.up * half);
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        let (mut dmin, mut dmax) = (f32::MAX, f32::MIN);
        for corner in [
            center - r - u,
            center + r - u,
            center + r + u,
            center - r + u,
        ] {
            let Some((n, _)) = project(corner) else {
                continue;
            };
            let p2 = px(n);
            lo = lo.min(p2);
            hi = hi.max(p2);
            dmin = dmin.min(n.z);
            dmax = dmax.max(n.z);
        }
        info!(
            "PARTICLE_DEPTHDUMP f={fidx} p{i} age={:.3} center=({:.4},{:.4},{:.4}) half={:.4} \
             viewz={:.4} dquad={:.9} corners=[{:.9},{:.9}] px=({:.1},{:.1}) \
             rect=({:.0},{:.0})..({:.0},{:.0}) rgba=[{:.2},{:.2},{:.2},{:.2}]",
            p.age,
            center.x,
            center.y,
            center.z,
            half,
            viewz,
            ndc.z,
            dmin,
            dmax,
            c_px.x,
            c_px.y,
            lo.x,
            lo.y,
            hi.x,
            hi.y,
            rgba[0],
            rgba[1],
            rgba[2],
            rgba[3],
        );
    }
}
