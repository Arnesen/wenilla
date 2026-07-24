//! Faithful M2 particle emitters — the visible fire/glow of campfires, torches, braziers and candles.
//!
//! The 1.12 client simulates particles on the CPU and draws them as camera-facing, additive quads; we
//! do the same (the GPU never touched the legacy particle path, and a CPU sim is the only way to match
//! the integrator exactly — see decision 0014). Each [`ParticleEmitter`] owns a small pool of live
//! particles and **one dynamically-rebuilt mesh**: every frame we age + integrate the pool, then
//! expand each live particle into a billboard facing the camera, writing positions/uvs/colours into the
//! mesh. Every cloud is **anchored to its MODEL's live position** (the reference rebuilds the
//! draw matrix with `translate(−emitterPos)` each frame — a moving model carries its flame, no
//! world-frozen trails) while the emitter's BONE composes only each particle's birth: bone motion
//! is baked per particle (an emitter orbiting on an animated bone — the food sparkle's global-
//! sequence spin — births a moving ring of straight risers, never a swirling cloud; the client's
//! only live-follow plumbing is file flag `0x4000`, unauthored by our content). File flag `0x10`
//! additionally folds the live bone ROTATION at render instead of baking it at birth —
//! byte-verified, wow-re `part-simspace-fields.md` + its `1f40db0b` corrections. The material is [`WowParticleMaterial`] — an unlit `StandardMaterial`
//! shell (its **own** pipeline, so it adds zero buffers to the model bind group — the Metal cap
//! that sank the dynamic-light attempt) whose fragment does the vanilla gamma-space combine
//! (`shaders/wow_particle.wgsl`, decision 0152).
//!
//! Over-life colour (incl. the alpha that is the additive weight), size and texture-cell are sampled
//! from the parsed [`benilla_formats::OverLife`] ramp by the particle's normalized age.

use benilla_assets::ModelEmitter;
use benilla_formats::{ParticleBlend, ParticleEmitterDef};
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;

use crate::lighting::SharedLightBuffer;

mod material;
mod model;
mod quads;
mod sim;

pub use material::{WowParticleExt, WowParticleMaterial};
use sim::simulate_particles;

/// Hard cap on a single emitter's live particle count — a backstop against a pathological model. Real
/// props sit far under this (a campfire's steady state is `rate·lifespan` ≈ 30 + 24 particles).
const MAX_PARTICLES: usize = 1024;

/// Live particle tuning, driven by the debug panel. Holds only the reference's own
/// `particleDensity` CVar — a real game setting, not a dev knob. (The intensity/size/tint A/B
/// sliders — director-confirmed compensation for since-fixed source bugs, 0150/0152 — are gone;
/// the authored values are the only path.)
#[derive(Resource)]
pub struct ParticleTuning {
    /// The vanilla `particleDensity` CVar (byte-verified: handler `0x688fb0` clamps to
    /// [0.25, 1.0], the getter's only two callers are the spawn-count `fmul`s). Scales emission
    /// RATE only — never size, alpha, or draw distance. Default 1.0.
    pub density: f32,
}

impl Default for ParticleTuning {
    fn default() -> Self {
        Self { density: 1.0 }
    }
}

/// One live particle. Its frame depends on the emitter's space mode (wow-re
/// `part-simspace-fields.md` + its `1f40db0b` corrections — the reference re-anchors EVERY cloud
/// to the emitter's current position each frame; there is no world-frozen trail mode):
/// - **Anchored mode** (file flag 0x10 clear — every placed prop): `pos`/`vel` are **world-oriented
///   Bevy axes, relative to the [`ParticleEmitter::anchor`]** (the MODEL, not the bone). Bone and
///   model rotation are baked at birth; the anchor translation follows the model — a running
///   kobold carries its candle flame, while an animated bone never drags the risen cloud.
///   Gravity acts on Bevy `-Y` (world up). On an **attached** model ([`ParticleEmitter::attach`])
///   the same coords are stored with the live attach rotation divided out at birth and re-applied
///   at draw (wow-re `part-kit-effect-attach-orient.md`, byte-verified: birth folds `A(t₀)⁻¹`,
///   draw folds the live `A(t₁)` — a turning host swings its frozen cloud by the heading change
///   since each particle's birth; a stationary host is bit-identical to the plain path).
/// - **Model mode** (0x10 set): `pos`/`vel` are the emitter's **local WoW model space** (Z up);
///   rendering folds the whole live placement transform every frame (rotation and all — the
///   chandelier's flames rigidly ride the swing).
struct Particle {
    pos: Vec3,
    vel: Vec3,
    age: f32,
    /// Spawn-time random phase for the twinkle LUT index (the reference hashes the particle's
    /// pointer; same role — de-sync the flicker across particles).
    phase: u32,
    /// The reference's particle+0xd bit 1 (set at spawn, cleared on the first integrate): a
    /// particle's first frame skips the follow-delta add (wow-re `part-emitter-motion.md` §2).
    fresh: bool,
    /// MODEL particles only (wow-re `part-model-particles.md`): the instance orientation
    /// (stored frame; seeded from the birth fold) and its body-frame angular velocity — the
    /// integrator applies the Rodrigues half-angle spin whenever `angvel` is non-zero. Quad
    /// particles carry identity/zero.
    quat: Quat,
    angvel: Vec3,
}

/// A spawned particle emitter: its parsed def + the placement that maps its local space to the world,
/// the live pool, the fractional emission accumulator, a per-emitter RNG, and the handle of the mesh we
/// rewrite each frame. Despawns with its placement (the entity joins the placement's entity list).
#[derive(Component)]
pub struct ParticleEmitter {
    def: ParticleEmitterDef,
    /// Model→world (Bevy space): `world = placement · wow_to_bevy(local)`. Carries the placement scale.
    /// Static for terrain doodads; refreshed each frame from [`Self::owner`] when set.
    placement: Transform,
    /// The entity this emitter belongs to, if it follows one (a streamed creature/GameObject). Each
    /// frame the emitter copies the owner's world transform into [`Self::placement`] (so it tracks a
    /// moving prop) and **drains** once the owner is gone. `None` for terrain doodads, which carry
    /// a fixed placement and despawn with their placement instead.
    owner: Option<Entity>,
    /// The owner died (missile impacted, effect reaped): emission stops, the pool lives out its
    /// lifespans at the frozen placement, and the emitter despawns itself when empty — live
    /// particles finish instead of popping. Byte ground (wow-re
    /// `ribbon-basis-emitter-lifecycle`, the 0202 dispatch's fold-back): the reference frees
    /// emitters synchronously at the model dtor, its fade coming from the model staying alive
    /// while emitters drain (the `HasLiveParticles` latch keeps a disabled emitter ticking) —
    /// this drain reproduces that defer-until-drained shape from the owner side; see
    /// `ribbons::RibbonTrail::owner` for the one OPEN half.
    draining: bool,
    /// The **attach frame** for an emitter on an ATTACHED model (a spell-kit instance root, a held
    /// item) — the entity whose live world rotation is the reference's attachment matrix `A`
    /// (`[model+0x17c]`, wow-re `part-kit-effect-attach-orient.md`). Anchored-mode particles are
    /// stored attach-local (birth divides `A` out) and drawn through the CURRENT `A`, so a host
    /// that turns mid-effect fans its spray by the heading-since-birth — the Eviscerate/Feint
    /// scatter. `None` for everything unattached (doodads, creatures' own models, missiles):
    /// `A = identity`, the plain world-frozen path.
    attach: Option<Entity>,
    /// The live attach rotation (identity when [`Self::attach`] is `None` or gone) — refreshed
    /// each frame before births/draw.
    attach_rot: Quat,
    /// The **cloud anchor** for anchored mode: the entity whose live translation carries the
    /// live pool (the MODEL — a creature root, an effect-instance root, a held item's root),
    /// NOT the emitter's bone joint. The joint composes each particle's birth (position and
    /// rotation baked, the reference's birth transforms) and then must never move the cloud
    /// again: an emitter riding an ANIMATED bone — the food sparkle orbits on a global-sequence
    /// spin — would otherwise drag every risen star in a circle (the director's swirl). File
    /// flag 0x4000 is the client's only live-particle-follows-emitter plumbing and placed/spell
    /// content doesn't author it. `None` = anchor at the spawn placement (a placed doodad whose
    /// emitter rides a joint anchors at the doodad, not the bone).
    anchor: Option<Entity>,
    /// The anchor's last-known world translation (kept when the entity vanishes so the pool
    /// drains in place; init = the spawn placement's translation).
    anchor_pos: Vec3,
    particles: Vec<Particle>,
    accumulator: f32,
    /// The emitter origin's last-frame live world position (the reference's rt+0x248 prevPos,
    /// refreshed EVERY frame — `0x7b5230` @0x7b5265): the one-frame Δ source for both
    /// emitter-motion terms (follow-delta, velocity inherit). `None` until the first frame.
    emitter_prev: Option<Vec3>,
    /// Velocity-inherit state (file flag 0x40, wow-re `part-emitter-motion.md` §1): the ~30 Hz
    /// trigger accumulator (rt+0x254) and the held inherit velocity (rt+0x258.., world frame) —
    /// recomputed only at a trigger, births read the held value between.
    inherit_accum: f32,
    inherit_vel: Vec3,
    /// Last frame's emission gate `(enabled && rate > 0)` — the rising-edge memory a BURST
    /// emitter (file flag 0x8000) latches on: it births its one `ftol(rate)` puff the frame this
    /// goes false→true and re-arms when it falls (the reference's `block+0x168`, wow-re
    /// `part-emission-burst-flag.md` §1). Unused by continuous emitters.
    gate_prev: bool,
    /// Seconds since this emitter spawned — the clip clock the keyed emission-rate track samples
    /// against (an effect model's emitters spawn at its clip start, so age == clip time). Ambient
    /// props' constant tracks are age-invariant.
    age: f32,
    rng: u32,
    mesh: Handle<Mesh>,
    /// The particle texture — also held here (not just on the material) so the sim can withhold drawing
    /// until it's resident: a still-loading or failed-to-decode texture would otherwise flash the engine
    /// fallback (white/magenta) through the additive blend. Particles still simulate meanwhile.
    texture: Handle<Image>,
    /// The pending recursion model (wow-re `part-child-recursion.md`): once the asset resolves,
    /// [`wire_child_emitters`] turns its own emitters (cap 4, the reference's `0x7b5dfe`) into
    /// [`Self::children`] and clears this.
    recursion: Option<Handle<benilla_assets::M2Model>>,
    /// CHILD emitters — driven once per live parent particle per frame at the particle's
    /// position (never ambiently); each owns its pool and a [`ChildDraw`] mesh entity.
    children: Vec<ChildEmitter>,
    /// The GEOMETRY model (wow-re `part-model-particles.md`): when authored, this emitter's
    /// particles render as 3-D instances of it instead of quads — [`model::update_model_particles`]
    /// grows/positions the instance pool below.
    geometry: Option<Handle<benilla_assets::M2Model>>,
    /// The live instance pool (one slot per drawn particle, grown on demand, hidden past the
    /// live count). Each slot's mesh entities carry per-instance tint-clone materials.
    model_instances: Vec<model::ModelInstance>,
}

/// One wired CHILD emitter (see [`ParticleEmitter::children`]): the recursion model's own
/// emitter def + texture, a private pool, and the mesh entity it draws through. Simulated
/// entirely inside the parent's sim block (no ECS ordering with the parent's fresh pool).
struct ChildEmitter {
    def: ParticleEmitterDef,
    texture: Handle<Image>,
    particles: Vec<Particle>,
    accumulator: f32,
    gate_prev: bool,
    rng: u32,
    mesh: Handle<Mesh>,
    /// The child's draw entity (a world-root mesh like the parent's own); despawned with the
    /// parent.
    entity: Entity,
}

/// Marker on a child emitter's draw entity — keeps the sim's transform queries provably
/// disjoint (the owner-read query excludes these; the child-draw write query requires it).
#[derive(Component)]
pub struct ChildDraw;

#[cfg(test)]
impl ChildEmitter {
    /// A bare child for unit tests — no draw entity/mesh wiring.
    fn bare(def: ParticleEmitterDef) -> Self {
        Self {
            def,
            texture: Handle::default(),
            particles: Vec::new(),
            accumulator: 0.0,
            gate_prev: false,
            rng: 7,
            mesh: Handle::default(),
            entity: Entity::PLACEHOLDER,
        }
    }
}

/// xorshift32 — a dependency-free PRNG for particle jitter (visual only; determinism not required).
fn next_u32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// A uniform random `f32` in `[0, 1)`.
fn rand01(state: &mut u32) -> f32 {
    (next_u32(state) >> 8) as f32 / (1u32 << 24) as f32
}

/// A symmetric random `f32` in `(−1, 1)` — the reference's `S11` draw (its RNG builds ±spans with
/// a ×2.0 constant; every emission distribution below is plain uniform, wow-re
/// `part-shape-kernels.md` §4 — we mirror the distributions, not the bit stream).
fn rand_s11(state: &mut u32) -> f32 {
    rand01(state) * 2.0 - 1.0
}

/// The **emission shape kernel** (wow-re `part-shape-kernels.md`, byte-verified off the three
/// vtable type-spawn generators): one birth's position + unit velocity direction, in the emitter's
/// local WoW frame (Z up, origin at the emitter record's `position`). The caller applies the speed
/// roll and the space-mode transform.
///
/// - **Plane** (`0x7b8890`): position uniform in the ±½·area rectangle; direction a cone around +Z
///   with **symmetric** angles θ = S11·verticalRange, φ = S11·horizontalRange (the reference draws
///   ±range, not [0, range] — a one-sided cone tilted every flame the same way).
/// - **Sphere** (`0x7b8d70`): radius uniform in [areaLength, areaWidth] (= min/max radius for this
///   shape), position on the shell at latitude S11·verticalRange / longitude S11·horizontalRange;
///   direction radial outward (flag `0x4000` ⇒ straight +Z instead).
/// - **Spline** (`0x7b9500`): born ON the authored arc-length-parameterized Bézier chain at a
///   uniform arc fraction in `[tMin, tMax]`; velocity = +Z spun about the local tangent by
///   ψ = S11·spin (zero velocity when no spin; radial-from-pivot when zSource authored), plus
///   an optional along-velocity scatter jitter — see the arm's comment.
/// - Any shape with `zSource ≠ 0`: direction is **radial from the pivot** `(0, 0, zSource)` toward
///   the birth point (fountains that arc outward from a point below the basin).
fn emit_local(def: &ParticleEmitterDef, rng: &mut u32) -> (Vec3, Vec3) {
    let origin = Vec3::from(def.position);
    // The emitter-frame R(+Z, 90°) applied at every branch's return — see the law note at the
    // tail. Per the wow-re bytes the prepend is per-EMITTER and subclass-independent ("gated
    // only by has-emitters"), so the spline kernel takes it exactly like sphere/plane (the
    // 0563 landing missed this branch — caught by the compensation audit).
    let rot90 = |v: Vec3| Vec3::new(-v.y, v.x, v.z);
    // SPLINE (`0x7b9500`, wow-re `part-shape-kernels.md` §3 — VERIFIED, incl. the scatter
    // bytes): born ON the authored Bézier chain at arc fraction `t ∈ [tMin, tMax]` (the
    // repurposed area fields). Velocity: radial from the zSource pivot when authored; else +Z
    // rotated about the local spline tangent by ψ = S11·spin (the reference extracts the
    // transposed row — a −ψ rotation — indistinguishable under the symmetric draw), with an
    // optional `U01·scatter` position jitter along the velocity; else ZERO (the particle sits
    // on the curve and only gravity/drag move it). A degenerate record (no parsed chain)
    // falls through to the plane kernel, as before.
    if let (benilla_formats::ParticleShape::Spline, Some(spline)) = (def.shape, &def.spline) {
        let (t0, t1) = (
            def.area_length.clamp(0.0, 1.0),
            def.area_width.clamp(0.0, 1.0),
        );
        let t = t0 + rand01(rng) * (t1 - t0);
        let mut pos = origin + Vec3::from(spline.eval(t));
        let dir = if def.z_source != 0.0 {
            (pos - origin - Vec3::new(0.0, 0.0, def.z_source)).normalize_or(Vec3::Z)
        } else if def.vertical_range != 0.0 {
            let tangent = Vec3::from(spline.tangent(t)).normalize_or(Vec3::Z);
            let (s, c) = (rand_s11(rng) * def.vertical_range).sin_cos();
            let dir = Vec3::Z * c + tangent.cross(Vec3::Z) * s + tangent * (tangent.z * (1.0 - c));
            if def.horizontal_range != 0.0 {
                pos += rand01(rng) * def.horizontal_range * dir;
            }
            dir
        } else {
            Vec3::ZERO
        };
        return (origin + rot90(pos - origin), rot90(dir));
    }
    // Birth offset in the emitter frame (before the record-position translation). A sphere
    // draws ONE lat/lon unit vector serving both the shell point and (below) the radial
    // velocity — the reference reuses the exact sincos pair (`0x7b8fba`), which is what keeps a
    // ZERO-radius sphere (the fireball impact's plume burst: min = max = 0) spraying uniformly
    // instead of collapsing every direction to the degenerate normalize fallback.
    let (local, shell) = if def.shape == benilla_formats::ParticleShape::Sphere {
        let r = def.area_length + rand01(rng) * (def.area_width - def.area_length).max(0.0);
        let lat = rand_s11(rng) * def.vertical_range;
        let lon = rand_s11(rng) * def.horizontal_range;
        let (slat, clat) = lat.sin_cos();
        let (slon, clon) = lon.sin_cos();
        let shell = Vec3::new(clat * clon, clat * slon, slat); // unit by construction
        (r * shell, Some(shell))
    } else {
        (
            Vec3::new(
                rand_s11(rng) * 0.5 * def.area_width,
                rand_s11(rng) * 0.5 * def.area_length,
                0.0,
            ),
            None,
        )
    };
    let dir = if def.z_source != 0.0 {
        // Radial from the (0, 0, zSource) pivot — degenerate at the pivot itself falls back to +Z.
        (local - Vec3::new(0.0, 0.0, def.z_source)).normalize_or(Vec3::Z)
    } else if let Some(shell) = shell {
        if def.sphere_up() {
            Vec3::Z
        } else {
            shell // radial outward — the same unit draw as the birth point
        }
    } else {
        let theta = rand_s11(rng) * def.vertical_range;
        let phi = rand_s11(rng) * def.horizontal_range;
        let (st, ct) = theta.sin_cos();
        let (sp, cp) = phi.sin_cos();
        Vec3::new(st * cp, st * sp, ct)
    };
    // The emitter-frame R(+Z, 90°) — wow-re `part-modelspace-animbone.md`, §5 byte-verified
    // (`0x719114–0x719142`: axis literal (0,0,1), angle π·0.5, Rodrigues + mat4_mul into the
    // per-frame emitter matrix rt+0x1fc): the reference prepends a fixed +90°-about-local-+Z
    // to EVERY M2 particle emitter's bone matrix, applied to the kernel-relative vectors only
    // (the record-position translation stays outside it). It is what turns the sphere kernel's
    // local-XZ ring into the wheel PERPENDICULAR to a rotor bone's +X spin — the InstancePortal
    // vortex swirling in place instead of tumbling edge-on — and what maps a billboard-bone
    // starburst's spray plane onto the screen plane (the impact flashes). Applied at emission,
    // so every consumer (anchored bake, model-space storage, child emitters) inherits it.
    (origin + rot90(local), rot90(dir))
}

/// Empty a particle mesh's attributes/indices (draws nothing). Used for a fresh emitter, and to stop
/// drawing one whose texture isn't resident.
fn clear_particle_mesh(mesh: &mut Mesh) {
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new());
    mesh.insert_indices(Indices::U32(Vec::new()));
}

/// An empty dynamic mesh (rewritten every frame by [`simulate_particles`]). `RenderAssetUsages::default`
/// keeps it in the main world so we can mutate it; positions are world-space (entity transform is
/// identity).
fn blank_particle_mesh() -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    clear_particle_mesh(&mut mesh);
    mesh
}

/// Spawn an emitter entity for one [`ModelEmitter`] at `placement`. `None` if the emitter has no
/// resolved texture (nothing to draw). `owner` ties the emitter to a live entity's frame — it follows
/// that entity's transform each frame and despawns with it: `(entity, [0; 3])` for a streamed
/// creature/GameObject (the whole model rides), or `(joint, bone_pivot)` for a doodad emitter riding
/// its animated bone (0130 phase 4 — the pivot rebases the model-space `def.position` into the
/// joint's own frame, so an unanimated chain reproduces the static path exactly). Pass `None` for a
/// static doodad (fixed placement, despawned by the placement's own entity list).
impl ParticleEmitter {
    /// Live particle count — read by the perf probe ([`crate::capture`]).
    pub(crate) fn live(&self) -> usize {
        self.particles.len()
    }

    /// The authored def — read by the particle census probe ([`crate::capture`]), which prints
    /// per-emitter facts (blend, texture, rate keys) beside the live count.
    pub(crate) fn def(&self) -> &ParticleEmitterDef {
        &self.def
    }

    /// The cloud-orientation fingerprint — read by the particle census probe: the live cloud's
    /// WORLD centroid, best-fit plane normal (unit), RMS thickness along it, and RMS in-plane
    /// radius, from the same composition the quad expansion draws with. The numeric "which way
    /// is this cloud actually facing" instrument: an orientation bug is settled by this number,
    /// never by eyeballing a capture (method: timing/geometry are measured). `None` under 4
    /// particles (no stable plane).
    pub(crate) fn cloud_fingerprint(&self) -> Option<(Vec3, Vec3, f32, f32)> {
        if self.particles.len() < 4 {
            return None;
        }
        let anchored = !self.def.model_space();
        let world: Vec<Vec3> = self
            .particles
            .iter()
            .map(|p| {
                if anchored {
                    self.anchor_pos + self.attach_rot * p.pos
                } else {
                    self.placement
                        .transform_point(benilla_assets::coords::wow_to_bevy([
                            p.pos.x, p.pos.y, p.pos.z,
                        ]))
                }
            })
            .collect();
        let n = world.len() as f32;
        let centroid = world.iter().sum::<Vec3>() / n;
        // 3×3 covariance; the normal is its smallest-eigenvalue eigenvector — found by power-
        // iterating for the two largest principal axes and crossing them (robust enough for a
        // probe; exactness is not the point, the axis label is).
        let mut cov = Mat3::ZERO;
        for w in &world {
            let d = *w - centroid;
            cov += Mat3::from_cols(d * d.x, d * d.y, d * d.z);
        }
        let power = |m: &Mat3, seed: Vec3| {
            let mut v = seed;
            for _ in 0..32 {
                let next = *m * v;
                if next.length_squared() < 1e-12 {
                    return seed.normalize_or(Vec3::X);
                }
                v = next.normalize();
            }
            v
        };
        let v1 = power(&cov, Vec3::new(0.7, 0.5, 0.5));
        // Deflate v1, then find the second axis in the remaining plane.
        let l1 = (cov * v1).dot(v1);
        let deflated = cov - Mat3::from_cols(v1 * v1.x, v1 * v1.y, v1 * v1.z) * l1;
        let v2 = power(&deflated, v1.any_orthonormal_vector());
        let normal = v1.cross(v2).normalize_or(Vec3::Y);
        let (mut thick2, mut radial2) = (0.0f32, 0.0f32);
        for w in &world {
            let d = *w - centroid;
            let t = d.dot(normal);
            thick2 += t * t;
            radial2 += d.length_squared() - t * t;
        }
        Some((centroid, normal, (thick2 / n).sqrt(), (radial2 / n).sqrt()))
    }
}

/// Ties a terrain-doodad emitter to its owner doodad's byte-verified distance-fade law
/// (`model_fade::doodad_fade_alpha`, `FUN_00683f80`): when the owner falls out of the draw set
/// (fade ≤ 0 — a small prop past 50 yd, a mid prop past 125), the emitter neither simulates nor
/// draws — the reference ticks particles as part of the owning model's animate step, which only
/// runs for drawn models. This is the system that bounds the reference's emitter population; the
/// particle-side rule (emission LOD, no cull — decision 0151) applies only WITHIN the draw set.
/// Attached by the terrain spawn path; entity-owned emitters (creatures/GameObjects/spells) are
/// bounded by server visibility instead and carry no fade.
#[derive(Component)]
pub struct EmitterFade {
    /// World bounding-sphere radius of the OWNER doodad (selects the fade band).
    pub radius: f32,
    /// The owner doodad's world bbox centre — the fade measures horizontal distance to this
    /// (matching the doodad's own gate in `debug_panel::visibility`), not the emitter position.
    pub center: Vec3,
}

#[allow(clippy::too_many_arguments)] // the spawn's full wiring: assets + owner/attach frames
pub fn spawn_emitter(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<WowParticleMaterial>,
    light: &SharedLightBuffer,
    emitter: &ModelEmitter,
    placement: Transform,
    owner: Option<(Entity, [f32; 3])>,
    attach: Option<Entity>,
    anchor: Option<Entity>,
) -> Option<Entity> {
    // Perf-bisect kill-switch: $WOW_NO_PARTICLES spawns no emitters at all.
    if std::env::var_os("WOW_NO_PARTICLES").is_some() {
        return None;
    }
    // A quad emitter with no resolved texture draws nothing; a GEOMETRY (model-particle)
    // emitter never draws quads — the default handle stays non-resident, keeping its quad
    // mesh permanently empty while the instances render.
    let texture = match emitter.texture.clone() {
        Some(t) => t,
        None if emitter.geometry.is_some() => Handle::default(),
        None => return None,
    };
    let mut def = emitter.def.clone();
    let owner = owner.map(|(entity, pivot)| {
        // Rebase the model-space emitter origin into the owner frame (bone-local for a joint owner;
        // pivot = 0 leaves it model-space for a whole-model owner). Same raw WoW axes throughout —
        // the sim composes `def.position` with the owner's live transform at each birth.
        def.position = [
            def.position[0] - pivot[0],
            def.position[1] - pivot[1],
            def.position[2] - pivot[2],
        ];
        entity
    });
    // Gate on the rate track's PEAK, not its first key: a one-shot burst emitter (the blood
    // spurt's starflash/glowball, 0140 fold-back) keys `0 → 200 → 0` — value[0] is 0 but it
    // absolutely emits.
    if def.lifespan <= 0.0 || def.emission_rate.peak() <= 0.0 {
        return None; // emits nothing
    }
    let material = particle_material(&def, &texture, materials, light);
    let mesh = meshes.add(blank_particle_mesh());
    // Seed the RNG from the placement position so two campfires don't flicker in lockstep.
    let t = placement.translation;
    let rng = (t.x.to_bits() ^ t.y.to_bits().rotate_left(11) ^ t.z.to_bits().rotate_left(22))
        .wrapping_mul(0x9E37_79B9)
        | 1;
    Some(
        commands
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material),
                // The sim writes the anchor here each frame; verts are anchor-relative (the
                // transparent-pass sort key — see simulate_particles).
                Transform::IDENTITY,
                NoFrustumCulling, // the mesh AABB is a moving cloud; skip culling
                ParticleEmitter {
                    def,
                    placement,
                    owner,
                    draining: false,
                    attach,
                    attach_rot: Quat::IDENTITY,
                    anchor,
                    anchor_pos: placement.translation,
                    particles: Vec::new(),
                    accumulator: 0.0,
                    emitter_prev: None,
                    inherit_accum: 0.0,
                    inherit_vel: Vec3::ZERO,
                    gate_prev: false,
                    age: 0.0,
                    rng,
                    mesh,
                    texture,
                    recursion: emitter.recursion.clone(),
                    children: Vec::new(),
                    geometry: emitter.geometry.clone(),
                    model_instances: Vec::new(),
                },
            ))
            .id(),
    )
}

/// The shared particle material recipe (the parent's and every child's): unlit, never
/// backface-culled, blend from the def, and the per-blend fog COLOUR policy (wow-re
/// `part-additive-combine.md` §4, byte-verified at `0x70baf0`'s own dispatch — the same
/// blend-identity table M2 batches take): an Add emitter fogs toward BLACK (fades under a veil
/// instead of gaining grey — the storm-veil level-up fix); Alpha/Opaque emitters fog toward the
/// ordinary scene colour; and an emitter with **file flag 0x8 ("unfogged", §4.1) draws with fog
/// disabled outright** — the policy is forced to 0 before the colour table is even consulted.
/// See [`WowParticleExt::params`].
fn particle_material(
    def: &ParticleEmitterDef,
    texture: &Handle<Image>,
    materials: &mut Assets<WowParticleMaterial>,
    light: &SharedLightBuffer,
) -> Handle<WowParticleMaterial> {
    let alpha_mode = match def.blend {
        ParticleBlend::Add => AlphaMode::Add,
        ParticleBlend::Alpha => AlphaMode::Blend,
        ParticleBlend::Opaque => AlphaMode::Opaque,
    };
    let fog_policy = if def.flags & 0x8 != 0 {
        0.0
    } else if matches!(def.blend, ParticleBlend::Add) {
        2.0
    } else {
        1.0
    };
    materials.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color_texture: Some(texture.clone()),
            unlit: true,
            alpha_mode,
            cull_mode: None, // billboards: never backface-cull
            ..default()
        },
        extension: WowParticleExt {
            light_buf: light.0.clone(),
            params: Vec4::new(fog_policy, 0.0, 0.0, 0.0),
            mod2x: false,
        },
    })
}

/// Wire pending CHILD emitters (wow-re `part-child-recursion.md`, VERIFIED): once a parent's
/// recursion model resolves, its own particle emitters — **capped at 4** (`0x7b5dfe`) — become
/// the parent's children, each with its private pool and draw entity. The reference wires at
/// the child model's async-load completion (`0x7b5dd0`); this system is that completion hook.
/// A child never self-emits ambiently — its only particle source is the per-parent-particle
/// drive in the sim.
pub(crate) fn wire_child_emitters(
    mut commands: Commands,
    models: Res<Assets<benilla_assets::M2Model>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WowParticleMaterial>>,
    light: Res<SharedLightBuffer>,
    mut emitters: Query<&mut ParticleEmitter>,
) {
    for mut emitter in &mut emitters {
        let Some(model) = emitter.recursion.as_ref().and_then(|h| models.get(h)) else {
            continue;
        };
        let mut rng_seed = emitter.rng.rotate_left(7) | 1;
        let children: Vec<ChildEmitter> = model
            .emitters
            .iter()
            .take(4)
            .filter_map(|em| {
                let texture = em.texture.clone()?;
                if em.def.lifespan <= 0.0 || em.def.emission_rate.peak() <= 0.0 {
                    return None;
                }
                let material = particle_material(&em.def, &texture, &mut materials, &light);
                let mesh = meshes.add(blank_particle_mesh());
                let entity = commands
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material),
                        Transform::IDENTITY,
                        NoFrustumCulling,
                        ChildDraw,
                    ))
                    .id();
                Some(ChildEmitter {
                    def: em.def.clone(),
                    texture,
                    particles: Vec::new(),
                    accumulator: 0.0,
                    gate_prev: false,
                    rng: {
                        rng_seed = rng_seed.wrapping_mul(0x9E37_79B9) | 1;
                        rng_seed
                    },
                    mesh,
                    entity,
                })
            })
            .collect();
        emitter.recursion = None;
        emitter.children = children;
    }
}

/// One frame of the emission front end: how many births the pool is owed, loaded into
/// `accumulator` (fractional; the caller's birth loop drains whole particles). The reference's
/// per-frame emitter pass + spawn driver (`0x718960` / `0x7b5550`, wow-re
/// `part-emission-burst-flag.md` + `part-emission-rate-animated.md`):
///
/// - The rate is sampled at the clip clock by the reference's own track law
///   ([`ValueTrack::sampled_ms`] — STEP for interp-0 tracks, linear for the rest, held past the
///   last key; the corpus authors bursts as step and pour ramps as linear) and the gate is
///   `(enabled && rate > 0)`.
/// - A **continuous** emitter (file flag 0x8000 clear) pours `rate · density·LOD · dt`.
/// - A **BURST** emitter (flag set) loads `ftol(rate · density·LOD)` ONCE on the gate's rising
///   edge and re-arms when it falls (a looping clip's next pass re-fires it); the pour never
///   runs for it. This is what bounds the Feint/Eviscerate impact's plume+crescents at one puff
///   ~67 ms in — over by ~0.5 s — while the same-shaped cast-hands flame pours its whole clip.
///
/// Returns the burst count loaded this frame (0.0 otherwise) — the `fx` trace's measurement hook.
fn accumulate_emission(
    def: &ParticleEmitterDef,
    clip_ms: f32,
    emitting: bool,
    scale: f32,
    dt: f32,
    accumulator: &mut f32,
    gate_prev: &mut bool,
) -> f32 {
    if !emitting {
        *accumulator = 0.0;
    }
    let rate = if emitting {
        def.emission_rate.sampled_ms(clip_ms).max(0.0)
    } else {
        0.0
    };
    let gate = rate > 0.0;
    let mut burst = 0.0;
    if def.burst() {
        if gate && !*gate_prev {
            burst = (rate * scale).trunc();
            *accumulator = burst;
        }
    } else if gate {
        *accumulator += rate * scale * dt;
    }
    *gate_prev = gate;
    burst
}

/// Registers the per-frame particle simulation/billboard system. Emitters are spawned by the terrain
/// streamer (which owns placement lifecycles) via [`spawn_emitter`].
pub struct ParticlePlugin;

impl Plugin for ParticlePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<WowParticleMaterial>::default())
            .init_resource::<ParticleTuning>()
            // PostUpdate, after the billboard joint palette: an emitter riding a billboarded
            // bone must sample the REPLACED frame, same frame — an Update-time read gets the
            // un-billboarded pose back (avian's fixed-loop sync re-propagates from locals), and
            // the Demon Skin flames followed the character instead of the camera. This also
            // retires the one-frame lag emitters on ANY animated joint carried while reading
            // last frame's globals from Update.
            .add_systems(
                PostUpdate,
                (
                    wire_child_emitters,
                    simulate_particles,
                    model::update_model_particles,
                )
                    .chain()
                    .in_set(crate::billboard::BillboardPlace)
                    .after(crate::billboard::billboard_joint_palette),
            );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use benilla_formats::{OverLife, ParticleShape, ValueTrack};

    /// The minimal def, exposed for sibling-module tests (the sim's child-drive test).
    pub(crate) fn plain_def() -> ParticleEmitterDef {
        def(ParticleShape::Plane)
    }

    /// A minimal def for kernel tests — only the fields [`emit_local`] reads matter.
    fn def(shape: ParticleShape) -> ParticleEmitterDef {
        ParticleEmitterDef {
            flags: 0,
            position: [1.0, 2.0, 3.0],
            bone: 0,
            shape,
            blend: benilla_formats::ParticleBlend::Add,
            texture: None,
            tile_rows: 1,
            tile_cols: 1,
            head_tail: 0,
            clip_wrap_ms: None,
            emission_speed: 1.0,
            speed_variation: 0.0,
            vertical_range: 0.5,
            horizontal_range: std::f32::consts::PI,
            gravity: 0.0,
            lifespan: 1.0,
            emission_rate: ValueTrack {
                keys: vec![(0, 10.0)],
                interp: 0,
            },
            enabled: benilla_formats::OnOffTrack::default(),
            area_length: 2.0,
            area_width: 4.0,
            z_source: 0.0,
            drag: 0.0,
            tail_time: 0.0,
            spline: None,
            geometry_model: None,
            recursion_model: None,
            angular_velocity_min: [0.0; 3],
            angular_velocity_max: [0.0; 3],
            inherit_scale: 0.0,
            follow_speed1: 0.0,
            follow_scale1: 0.0,
            follow_speed2: 0.0,
            follow_scale2: 0.0,
            twinkle_speed: 0.0,
            twinkle_percent: 1.0,
            twinkle_min: 0.0,
            twinkle_max: 0.0,
            spin: 0.0,
            over_life: OverLife {
                mid: 0.5,
                color: [[1.0; 4]; 3],
                scale: [1.0; 3],
                cell_a: [0, 0],
                cell_b: [0, 0],
            },
        }
    }

    /// The BURST emission model (file flag 0x8000 — Feint/Eviscerate impact's plume+crescents):
    /// one `ftol(rate·scale)` puff on the rising edge of `(enabled && step-sampled rate > 0)`,
    /// latched while the gate holds, re-armed when it falls. The STEP rate law means a
    /// `{0:0, 67:30}` track is silent before 67 ms — the puff fires AT the key, full-count.
    #[test]
    fn burst_emitter_fires_once_on_the_rising_edge() {
        let mut d = def(ParticleShape::Plane);
        d.flags = 0x8000;
        d.emission_rate = ValueTrack {
            keys: vec![(0, 0.0), (67, 30.0)],
            interp: 0, // every corpus burst authors a STEP rate track (census-verified)
        };
        let (mut acc, mut prev) = (0.0, false);
        assert_eq!(
            accumulate_emission(&d, 50.0, true, 1.0, 0.016, &mut acc, &mut prev),
            0.0,
            "step-sampled rate is 0 before the 67 ms key — no gate, no burst"
        );
        assert_eq!(acc, 0.0);
        assert_eq!(
            accumulate_emission(&d, 70.0, true, 1.0, 0.016, &mut acc, &mut prev),
            30.0,
            "the frame the sample steps to 30: one full-count burst"
        );
        assert_eq!(acc, 30.0);
        acc = 0.0; // the birth loop drains it
        accumulate_emission(&d, 500.0, true, 1.0, 0.016, &mut acc, &mut prev);
        accumulate_emission(&d, 1500.0, true, 1.0, 0.016, &mut acc, &mut prev);
        assert_eq!(acc, 0.0, "held-high gate stays latched — never a pour");
        // The gate falls (drain/disable) → re-arms → the next rise bursts again (a looping
        // clip's next pass), the count ftol-truncated through density·LOD.
        accumulate_emission(&d, 1600.0, false, 1.0, 0.016, &mut acc, &mut prev);
        assert_eq!(
            accumulate_emission(&d, 70.0, true, 0.55, 0.016, &mut acc, &mut prev),
            16.0,
            "ftol(30 · 0.55) = 16"
        );
    }

    /// The continuous emission model (flag clear) still pours `rate·dt` — and drops its owed
    /// fraction while disabled (the pre-existing drain hygiene).
    #[test]
    fn continuous_emitter_pours_rate_dt() {
        let mut d = def(ParticleShape::Plane);
        d.emission_rate = ValueTrack {
            keys: vec![(0, 0.0), (67, 30.0)],
            interp: 0,
        };
        let (mut acc, mut prev) = (0.0, false);
        accumulate_emission(&d, 100.0, true, 1.0, 0.1, &mut acc, &mut prev);
        accumulate_emission(&d, 200.0, true, 1.0, 0.1, &mut acc, &mut prev);
        assert!((acc - 6.0).abs() < 1e-4, "30/s × 0.2 s, no burst latch");
        accumulate_emission(&d, 300.0, false, 1.0, 0.1, &mut acc, &mut prev);
        assert_eq!(acc, 0.0, "disabled zeroes the owed fraction");
    }

    /// A LINEAR (interp 1) rate ramp pours the lerped envelope — the BloodSpurt shape (`0 → 100
    /// → … → 0` authored interp 1 corpus-wide): mid-ramp the pour runs at the interpolated rate,
    /// not the previous key's step (which over-poured every falling tail and muted every rise).
    #[test]
    fn continuous_lerp_ramp_pours_the_interpolated_rate() {
        let mut d = def(ParticleShape::Plane);
        d.emission_rate = ValueTrack {
            keys: vec![(0, 0.0), (100, 100.0), (200, 0.0)],
            interp: 1,
        };
        let (mut acc, mut prev) = (0.0, false);
        accumulate_emission(&d, 50.0, true, 1.0, 0.1, &mut acc, &mut prev);
        assert!((acc - 5.0).abs() < 1e-4, "rising mid-ramp: 50/s × 0.1 s");
        accumulate_emission(&d, 150.0, true, 1.0, 0.1, &mut acc, &mut prev);
        assert!((acc - 10.0).abs() < 1e-4, "falling mid-ramp: 50/s again");
        accumulate_emission(&d, 500.0, true, 1.0, 0.1, &mut acc, &mut prev);
        assert!(
            (acc - 10.0).abs() < 1e-4,
            "held last key 0: the ramp self-closes"
        );
    }

    /// The follow-delta response line (`0x7b5d30`): the line through the two authored
    /// (speed, fraction) samples; equal speeds degenerate to no response (the reference zeroes
    /// slope and intercept).
    #[test]
    fn follow_line_matches_the_authored_two_point_response() {
        let mut d = def(ParticleShape::Plane);
        d.follow_speed1 = 1.0;
        d.follow_scale1 = 0.2;
        d.follow_speed2 = 3.0;
        d.follow_scale2 = 0.8;
        let (slope, intercept) = d.follow_line().expect("distinct speeds");
        assert!((slope * 2.0 + intercept - 0.5).abs() < 1e-6, "midpoint");
        assert!((slope * 1.0 + intercept - 0.2).abs() < 1e-6, "sample 1");
        d.follow_speed2 = 1.0;
        assert!(
            d.follow_line().is_none(),
            "equal speeds → the reference zeroes the response"
        );
    }

    /// Plane births stay in the ±½·area rectangle around the record position, and the cone is
    /// SYMMETRIC (θ = S11·range — wow-re `part-shape-kernels.md`'s correction of our old
    /// [0, range) draw): with a wide sample, x-velocities must land on both signs. The
    /// rectangle rides the R(+Z,90°) emitter frame (`emit_local`'s tail): the kernel's
    /// width-along-x/length-along-y square lands width-along-y/length-along-x.
    #[test]
    fn plane_kernel_rect_bounds_and_symmetric_cone() {
        let d = def(ParticleShape::Plane);
        let mut rng = 12345u32;
        let (mut neg, mut pos) = (false, false);
        for _ in 0..256 {
            let (p, dir) = emit_local(&d, &mut rng);
            assert!(
                (p.x - 1.0).abs() <= 1.0 + 1e-4,
                "x within ±½·length of position (post-R)"
            );
            assert!(
                (p.y - 2.0).abs() <= 2.0 + 1e-4,
                "y within ±½·width of position (post-R)"
            );
            assert_eq!(p.z, 3.0);
            assert!(dir.z > 0.0, "cone around +Z stays upward for range < π/2");
            neg |= dir.x < -1e-3;
            pos |= dir.x > 1e-3;
        }
        assert!(neg && pos, "symmetric cone covers both x signs");
    }

    /// Sphere births sit on a shell with radius in [areaLength, areaWidth] (min/max radius for
    /// this shape) and fly radially outward through the shell point.
    #[test]
    fn sphere_kernel_radius_bounds_and_radial_velocity() {
        let d = def(ParticleShape::Sphere);
        let mut rng = 99u32;
        for _ in 0..256 {
            let (p, dir) = emit_local(&d, &mut rng);
            let rel = p - Vec3::new(1.0, 2.0, 3.0);
            let r = rel.length();
            assert!(
                (2.0 - 1e-3..=4.0 + 1e-3).contains(&r),
                "shell radius {r} within [min, max]"
            );
            assert!(
                rel.normalize().dot(dir) > 0.999,
                "velocity radial through the shell point"
            );
        }
    }

    /// A ZERO-radius sphere (min = max = 0 — the fireball impact's plume burst) still sprays its
    /// velocities across the authored lat/lon spread: the direction is the shell unit vector
    /// itself (`0x7b8fba` reuses the sincos pair), never a normalize of the (zero) birth offset.
    #[test]
    fn zero_radius_sphere_still_disperses() {
        let mut d = def(ParticleShape::Sphere);
        d.area_length = 0.0;
        d.area_width = 0.0;
        d.vertical_range = std::f32::consts::PI; // the MoltenBlast plume: full ±π latitude fan
        d.horizontal_range = 0.0;
        let mut rng = 4242u32;
        let (mut up, mut down, mut fwd, mut back) = (false, false, false, false);
        for _ in 0..256 {
            let (p, dir) = emit_local(&d, &mut rng);
            assert_eq!(p, Vec3::new(1.0, 2.0, 3.0), "births at the centre");
            assert!((dir.length() - 1.0).abs() < 1e-4, "unit direction");
            assert_eq!(
                dir.x, 0.0,
                "zero longitude range pins the fan to the YZ plane (the R(+Z,90°) emitter frame)"
            );
            up |= dir.z > 0.5;
            down |= dir.z < -0.5;
            fwd |= dir.y > 0.5;
            back |= dir.y < -0.5;
        }
        assert!(
            up && down && fwd && back,
            "the fan covers the full vertical ring, not a single degenerate ray"
        );
    }

    /// The SPLINE kernel (`0x7b9500`): births sit ON the authored chain (origin-composed) at
    /// arc fractions inside [tMin, tMax]; zero spin ⇒ ZERO velocity; a spin range fans +Z
    /// about the local tangent (here +X ⇒ the fan stays in the YZ plane), and scatter jitters
    /// the position along the velocity by at most its own length.
    #[test]
    fn spline_kernel_births_on_the_chain() {
        let x = |v: f32| [v, 0.0, 0.0];
        let mut d = def(ParticleShape::Spline);
        d.spline = benilla_formats::SplineData::new(vec![
            x(0.0),
            x(1.0),
            x(2.0),
            x(3.0), // one straight segment along +X
        ]);
        d.area_length = 0.25; // tMin
        d.area_width = 0.75; // tMax
        d.vertical_range = 0.0;
        d.z_source = 0.0;
        let mut rng = 31u32;
        // The R(+Z,90°) emitter frame (`emit_local`'s tail) turns the authored +X chain to +Y.
        for _ in 0..64 {
            let (p, dir) = emit_local(&d, &mut rng);
            let local = p - Vec3::new(1.0, 2.0, 3.0); // minus the record position
            assert!(
                (0.75 - 1e-3..=2.25 + 1e-3).contains(&local.y),
                "on the chain inside [tMin, tMax] (post-R along +Y): {}",
                local.y
            );
            assert_eq!((local.x, local.z), (0.0, 0.0));
            assert_eq!(dir, Vec3::ZERO, "no spin, no zSource: the flame stands");
        }
        // A spin range fans +Z about the +X tangent — the kernel's YZ fan lands in XZ post-R.
        d.vertical_range = 1.0;
        d.horizontal_range = 0.5; // scatter
        let (mut low, mut high) = (false, false);
        for _ in 0..256 {
            let (p, dir) = emit_local(&d, &mut rng);
            assert!(dir.y.abs() < 1e-4, "fan in the XZ plane (post-R)");
            assert!((dir.length() - 1.0).abs() < 1e-4);
            assert!(dir.z > 0.0, "±1 rad about +Z stays upward");
            low |= dir.x > 0.5;
            high |= dir.x < -0.5;
            // The scatter jitter is along dir, bounded by its own length.
            let local = p - Vec3::new(1.0, 2.0, 3.0);
            assert!(local.x.hypot(local.z) <= 0.5 + 1e-3, "jitter ≤ scatter");
        }
        assert!(low && high, "the fan covers both spin signs");
    }

    /// zSource ≠ 0 redirects any shape's velocity radially away from the (0, 0, zSource) pivot.
    #[test]
    fn z_source_pivots_the_velocity() {
        let mut d = def(ParticleShape::Plane);
        d.z_source = -1.0; // pivot below the emitter → births fly up-and-outward
        let mut rng = 7u32;
        for _ in 0..64 {
            let (p, dir) = emit_local(&d, &mut rng);
            let rel = (p - Vec3::new(1.0, 2.0, 3.0)) - Vec3::new(0.0, 0.0, -1.0);
            assert!(rel.normalize().dot(dir) > 0.999, "radial from the pivot");
        }
    }
}
