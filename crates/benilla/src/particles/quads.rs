//! The particle **quad expansion** — one pool of live particles → the camera-facing (or
//! XY-plane) billboard mesh, exactly the reference's quad writer laws (`0x7b2a50` head,
//! `0x7b3041` tail, the twinkle LUT, the `0x7b2dda` spin negate). Shared verbatim by a parent
//! emitter and its CHILD emitters (`part-child-recursion.md`), which differ only in whose pool
//! and def feed it.

use benilla_assets::coords::wow_to_bevy;
use benilla_formats::ParticleEmitterDef;
use bevy::mesh::Indices;
use bevy::prelude::*;

use super::{rand01, Particle};

/// The 128-entry twinkle noise table — the reference seeds `DAT_00cf58f0` with uniform-random f32
/// in [0,1) at startup (wow-re `part-quad-tail-twinkle.md`, byte-verified incl. the fill loop; we
/// mirror the distribution with a fixed seed, not the reference's stream).
static TWINKLE_LUT: std::sync::LazyLock<[f32; 128]> = std::sync::LazyLock::new(|| {
    let mut s = 0xC0FF_EE11u32;
    let mut t = [0.0f32; 128];
    for v in &mut t {
        *v = rand01(&mut s);
    }
    t
});

/// The twinkle noise sample for a particle: the byte-verified index (`0x7b2a86`) is
/// `(floor(clamp(twinkleSpeed · age, 0, 255)) + particlePhase) & 0x7f` — the reference's phase is
/// a per-particle pointer hash; ours is a spawn-time random ([`Particle::phase`]). The phase
/// de-syncs the flicker across particles (without it a whole flame pulses in lockstep).
fn twinkle_noise(twinkle_speed: f32, age: f32, phase: u32) -> f32 {
    let idx = ((twinkle_speed * age).clamp(0.0, 255.0) as u32).wrapping_add(phase) as usize & 0x7f;
    TWINKLE_LUT[idx]
}

/// The quad-spin angle (byte-verified: the negate at `0x7b2dda`, immediately before the fsincos
/// fold, shared by the plain-rotated and Rodrigues legs): `spin·age`, **negated when the raw angle
/// is negative on a particle whose pool-slot pointer carries bit 5** — the 0x20-byte `CParticle2`
/// slots alternate that bit, so this is vanilla's entire rotation randomizer: a NEGATIVE-spin
/// emitter counter-rotates half its cloud (the Fire Blast impact smoke reads as churning volume),
/// a positive-spin one rotates uniformly. Same-age burst particles rotating in lockstep otherwise
/// turn the whole cloud as one rigid picture — the director's "2D smoke". The reference's half-
/// split is a pointer hash; ours is the same spawn-time [`Particle::phase`] the twinkle index uses
/// (stable per particle, which a pool INDEX under `retain_mut` compaction would not be).
pub(super) fn spin_angle(spin: f32, age: f32, phase: u32) -> f32 {
    let angle = spin * age;
    if angle < 0.0 && phase & 0x20 != 0 {
        -angle
    } else {
        angle
    }
}

/// The camera basis one frame of expansion billboards against.
pub(super) struct CamBasis {
    pub right: Vec3,
    pub up: Vec3,
    pub face_normal: Vec3,
}

/// The cloud's draw frame: where its vertices are relative to ([`anchor`]) and how stored
/// coordinates reach the world (the anchored/model split of [`super::Particle`]'s doc).
pub(super) struct DrawFrame {
    pub anchored: bool,
    pub anchor: Vec3,
    pub attach_rot: Quat,
}

/// Expand one pool into its billboard mesh (rewritten in place). Vertices are ANCHOR-relative —
/// the caller carries the anchor on the mesh entity's transform (the transparent-pass sort key).
pub(super) fn expand_quads(
    def: &ParticleEmitterDef,
    particles: &[Particle],
    frame: &DrawFrame,
    placement: &Transform,
    cam: &CamBasis,
    mesh: &mut Mesh,
) {
    let n = particles.len();
    let (anchored, anchor) = (frame.anchored, frame.anchor);
    let (cam_right, cam_up) = (cam.right, cam.up);
    let mut positions = Vec::with_capacity(n * 4);
    let mut normals = Vec::with_capacity(n * 4);
    let mut uvs = Vec::with_capacity(n * 4);
    let mut colors = Vec::with_capacity(n * 4);
    let mut indices = Vec::with_capacity(n * 6);
    // Size scales with the instance transform only when the emitter flags it (0x200) — an
    // instance-scaled prop otherwise scales its particle *positions* only (wow-re B2).
    let scale = if def.scale_size_by_instance() {
        placement.scale.x.max(1e-4)
    } else {
        1.0
    };
    // The XY-quad head basis (file flag 0x1000, wow-re `part-tiled-corner-builder.md`,
    // VERIFIED): the quad lies flat in the emitter's model-space XY plane carried by the
    // LIVE model→world matrix — camera-independent (the impact crescents, state rings,
    // fish-school splashes). Corners inherit the matrix's scale (the reference's `S·M`;
    // separate from — and stacking with — the flag-0x200 size multiply above), and the
    // live placement orients the plane in BOTH sim modes: the reference folds the emitter
    // orientation into the corner matrix even when birth-baking positions (anchored mode).
    // The corner square rides the same R(+Z,90°)-prepended emitter matrix as the particles
    // (wow-re `part-modelspace-animbone.md`; we fold R at emission — `emit_local` — so here the
    // basis vectors carry it explicitly): X̂ → R·X̂ = Ŷ, Ŷ → R·Ŷ = −X̂ — an in-plane quarter
    // turn of every flat quad (invisible on round textures, load-bearing on crescents).
    let plane_basis = def.xy_quad().then(|| {
        let s = placement.scale.x.max(1e-4);
        (
            placement.rotation * (wow_to_bevy([0.0, 1.0, 0.0]) * s),
            placement.rotation * (wow_to_bevy([-1.0, 0.0, 0.0]) * s),
        )
    });
    let cols = def.tile_cols.max(1);
    let rows = def.tile_rows.max(1);
    let (inv_cols, inv_rows) = (1.0 / cols as f32, 1.0 / rows as f32);
    for p in particles {
        let noise = twinkle_noise(def.twinkle_speed, p.age, p.phase);
        // The twinklePercent draw-gate (byte-verified `0x7b2adc`): while percent < 1 (never on
        // placed content, which authors 1.0), a frame whose LUT sample exceeds it emits no
        // quad at all — the reference's hard scintillation.
        if def.twinkle_percent < 1.0 && noise > def.twinkle_percent {
            continue;
        }
        let u_age = (p.age / def.lifespan).clamp(0.0, 1.0);
        let (rgba, size, cell) = def.over_life.sample(u_age);
        // RAW authored RGB — the gamma-space decode happens ONCE, in the material's fragment
        // (`wow_particle.wgsl`, decision 0152), where it also covers the texture term that a
        // CPU-side vertex linearisation (0150) could not reach. Alpha is a blend weight — raw
        // everywhere. The remaining bonfire-CORE brightness gap vs the reference is the
        // additive-composite-space question (0148 open item: the reference sums gamma bytes in
        // an LDR framebuffer; we sum linear and encode once), NOT a colour-space bug — do not
        // "fix" it here again.
        // Anchored positions ride the emitter's live translation — through the CURRENT attach
        // rotation on an attached model (`DrawMx = A·Translate·V`, the heading-since-birth
        // fan); model mode folds the whole live placement transform (the reference's rt-0x100
        // render fold, `0x7b3d20`).
        let center = if anchored {
            anchor + frame.attach_rot * p.pos
        } else {
            placement.transform_point(wow_to_bevy([p.pos.x, p.pos.y, p.pos.z]))
        };
        // `size` is the half-extent: the reference quad corners are ±1.0 (verified in wow-5875-re,
        // `quad_expand`), so a vertex sits at `center ± size` and the world quad edge spans 2·size.
        // Rendered half-size (wow-re B2, byte-verified `0x7b2a50`): the over-life ramp is the
        // base, × the GATED twinkle flicker (skipped when min == max — `{0,0}` and `{1,1}`
        // alike burn steady; the old base+rand reading collapsed the kobold candle to zero),
        // × the instance scale iff flagged.
        let half = size * def.twinkle(noise) * scale;
        // Texture-atlas cell → sub-rect UV (v increases downward in image space).
        let cell = cell.min(rows * cols - 1);
        let cx = (cell % cols) as f32;
        let cy = (cell / cols) as f32;
        let (u0, u1) = (cx * inv_cols, (cx + 1.0) * inv_cols);
        let (v0, v1) = (cy * inv_rows, (cy + 1.0) * inv_rows);
        let mut push_quad = |corners: [Vec3; 4], quv: [[f32; 2]; 4]| {
            let b = positions.len() as u32;
            for (c, t) in corners.iter().zip(quv) {
                // Vertices are ANCHOR-relative (the entity transform carries the anchor):
                // Bevy's transparent pass sorts meshes by their entity position, so every
                // cloud needs a real depth — all-world-baked-at-origin made two overlapping
                // smoke plumes sort-tie and swap draw order per frame (the distant-bonfire
                // flashing). Within one cloud, quad order stays pool order — the reference's
                // global per-particle painter sort is a named deliberate simplification.
                positions.push((*c - anchor).to_array());
                normals.push(cam.face_normal.to_array());
                colors.push(rgba);
                uvs.push(t);
            }
            indices.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
        };
        // HEAD quad (particleType 0/2), drawn first (`0x7b2bc9`): a camera-facing billboard,
        // or — XY-quad emitters — the flat plane basis computed above.
        if def.head_tail != 1 {
            let (base_r, base_u) = plane_basis.unwrap_or((cam_right, cam_up));
            // Quad spin (file +0x198): rotate in-plane by `angle = spin·age` (the fcos/fsin
            // fold at `0x7b2ddc`; on an XY quad the same form IS the reference's Rodrigues
            // about the quad-plane normal, since `normal × base_r = base_u`). 0 for nearly
            // every prop. A NEGATIVE spin counter-rotates half the cloud ([`spin_angle`]).
            let (r, u) = if def.spin != 0.0 {
                let (sa, ca) = spin_angle(def.spin, p.age, p.phase).sin_cos();
                (
                    (base_r * ca + base_u * sa) * half,
                    (base_u * ca - base_r * sa) * half,
                )
            } else {
                (base_r * half, base_u * half)
            };
            push_quad(
                [
                    center - r - u,
                    center + r - u,
                    center + r + u,
                    center - r + u,
                ],
                [[u0, v1], [u1, v1], [u1, v0], [u0, v0]],
            );
        }
        // TAIL quad (particleType 1/2, byte-verified `0x7b3041`): a velocity-projected streak
        // trailing the motion — world length |velocity|·tailTime (flag 0x400 grows it from
        // zero with age), width 2·half perpendicular IN SCREEN SPACE, U running along the
        // tail (0 at the particle → 1 at the tip), same flipbook cell. A view-parallel
        // velocity (degenerate screen length) falls back to a plain billboard.
        if def.head_tail >= 1 {
            let vel_world = if anchored {
                frame.attach_rot * p.vel
            } else {
                placement.rotation * (placement.scale * wow_to_bevy(p.vel.to_array()))
            };
            let t_eff = if def.tail_clamps_to_age() {
                def.tail_time.min(p.age)
            } else {
                def.tail_time
            };
            let tail = -vel_world * t_eff;
            let (tr, tu) = (tail.dot(cam_right), tail.dot(cam_up));
            let l2 = tr * tr + tu * tu;
            if l2 < 7.7e-4 {
                // Degenerate: the reference's plain-billboard fallback (`0x7b33fa`).
                let (r, u) = (cam_right * half, cam_up * half);
                push_quad(
                    [
                        center - r - u,
                        center + r - u,
                        center + r + u,
                        center - r + u,
                    ],
                    [[u0, v1], [u1, v1], [u1, v0], [u0, v0]],
                );
            } else {
                let inv_l = half / l2.sqrt();
                let perp = (cam_up * tr - cam_right * tu) * inv_l;
                push_quad(
                    [
                        center - perp,
                        center + perp,
                        center + tail + perp,
                        center + tail - perp,
                    ],
                    [[u0, v1], [u0, v0], [u1, v0], [u1, v1]],
                );
            }
        }
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

#[cfg(test)]
mod tests {
    use super::spin_angle;

    /// The `0x7b2dda` negate: only a NEGATIVE angle on a bit-5 particle flips — a negative-spin
    /// emitter counter-rotates exactly its bit-5 half, a positive-spin one never splits.
    #[test]
    fn negative_spin_counter_rotates_the_bit5_half() {
        assert_eq!(spin_angle(-3.0, 0.5, 0x20), 1.5, "bit 5 set: negated");
        assert_eq!(spin_angle(-3.0, 0.5, 0x1f), -1.5, "bit 5 clear: kept");
        assert_eq!(
            spin_angle(3.0, 0.5, 0x20),
            1.5,
            "positive spin: never negated"
        );
        assert_eq!(spin_angle(3.0, 0.5, 0x1f), 1.5);
        assert_eq!(spin_angle(0.0, 0.5, 0xff), 0.0);
    }
}
