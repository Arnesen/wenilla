//! Faithful M2 particle emitters — the visible fire/glow of campfires, torches, braziers and candles.
//!
//! The 1.12 client simulates particles on the CPU and draws them as camera-facing, additive quads; we
//! do the same (the GPU never touched the legacy particle path, and a CPU sim is the only way to match
//! the integrator exactly — see decision 0014). Each [`ParticleEmitter`] owns a small pool of live
//! particles: every frame we age + integrate the pool, then expand each live particle into a
//! camera-facing quad written into the **shared effect-quad stream** ([`buffer::EffectQuads`],
//! decision 0732 slice P1 — one CPU vertex vec + one GPU upload for the whole family, drawn by
//! the dedicated lane in [`render`]; per-emitter `Mesh`/material assets are gone, and with them
//! the per-frame allocator churn they priced). Every cloud is **anchored to its MODEL's live
//! position** (the reference rebuilds the
//! draw matrix with `translate(−emitterPos)` each frame — a moving model carries its flame, no
//! world-frozen trails) while the emitter's BONE composes only each particle's birth: bone motion
//! is baked per particle (an emitter orbiting on an animated bone — the food sparkle's global-
//! sequence spin — births a moving ring of straight risers, never a swirling cloud; the client's
//! only live-follow plumbing is file flag `0x4000`, unauthored by our content). File flag `0x10`
//! additionally folds the live bone ROTATION at render instead of baking it at birth —
//! byte-verified, wow-re `part-simspace-fields.md` + its `1f40db0b` corrections. The lane's
//! fragment does the vanilla gamma-space combine (`shaders/wow_effect.wgsl`, decisions
//! 0152/0161). The whole effect family — particles, ribbons, the decal family, water foam,
//! precipitation — draws through the lane; `WowParticleMaterial` retired with slice P2 (0733).
//!
//! Over-life colour (incl. the alpha that is the additive weight), size and texture-cell are sampled
//! from the parsed [`benilla_formats::OverLife`] ramp by the particle's normalized age.

use benilla_assets::ModelEmitter;
use benilla_formats::ParticleEmitterDef;
use bevy::prelude::*;

pub mod buffer;
pub(crate) mod census;
mod depthdump;
mod emit;
mod model;
mod quads;
pub mod render;
mod sim;

use emit::{emit_local, next_u32, rand01, rand_s11};
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
    /// This particle's lifetime, captured at birth from the emitter's **current sampled**
    /// lifespan channel (the reference passes it into each spawn as the kernels' `life_param` —
    /// wow-re `part-shape-kernels.md`; the channel ANIMATES, e.g. Frost Nova 0.47 → 0.80 s, so a
    /// shared `def.lifespan` is wrong twice over). Kill at `age >= life`; over-life ramps
    /// normalize by it.
    life: f32,
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

impl ParticleEmitter {
    /// The rig bone this emitter is mounted on — the scope key the depth probes filter by
    /// (`$WOW_DEPTH_QUADS`, `$WOW_PARTICLE_DEPTHDUMP_BONES`).
    pub(crate) fn bone(&self) -> u16 {
        self.def.bone
    }
}

/// A spawned particle emitter: its parsed def + the placement that maps its local space to the world,
/// the live pool, the fractional emission accumulator, and a per-emitter RNG; its quads go into
/// the shared effect stream ([`buffer::EffectQuads`]) each frame. Despawns with its placement
/// (the entity joins the placement's entity list).
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
    /// What losing [`Self::owner`] means for this emitter — the spawn site's call, because only it
    /// knows whether the owner entity going away is a MODEL BEING DESTROYED or an effect ending
    /// (see [`OwnerLoss`]).
    on_owner_loss: OwnerLoss,
    /// The owner died and this emitter is [`OwnerLoss::Drain`]: emission stops, the pool lives out
    /// its lifespans at the frozen placement, and the emitter despawns itself when empty — live
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
    /// The model instance whose [`crate::model_fade::ModelAlpha`] this cloud is multiplied by —
    /// the reference's `emitter+0x1a8`, a per-frame copy of that model's `+0x19c` (`0x718960`
    /// @`0x719073`), folded into each particle's ALPHA by the over-life sampler (`0x7b9b10`
    /// @`0x7b9b42`; wow-re `part-scene-multipliers.md` §4's REFUTED negative + `part-additive-
    /// combine.md` §6.1). For an ATTACHED model — a held item, a helm, a pauldron — it is the
    /// WEARER's, because an attached model inherits its parent's computed alpha (decision 0827).
    /// `None` ⇒ 1.0: a placed doodad instead multiplies its own distance fade ([`EmitterFade`]).
    alpha_src: Option<Entity>,
    /// This frame's value of that alpha (1.0 until read).
    alpha: f32,
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
    /// The emission clock's SEQUENCE source (`EmitTiming`, decision 0641's host resolution one
    /// channel over): the entity whose live `AnimationPlayer` names the playing sequence + clip
    /// time — a unit's / GameObject's emitters read the *playing* sequence's key window, so a
    /// quest object's explosion fires in its one-shot clips and stays OFF at idle. `None` = a
    /// pinned lane (placed doodad, effect rig, booth) on the spawn-age clock.
    host: Option<Entity>,
    /// The sequence FILE slot the timing samples: the pinned slot an effect's rig armed, or the
    /// host's last resolved slot (kept across frames where the host has no live player — the
    /// pre-arm frame reads that slot's opening pose). `None` ⇒ slot 0 (the doodad law).
    seq: Option<usize>,
    rng: u32,
    /// The OWNER model's reach from its origin in **world** yards (the authored
    /// [`ModelEmitter::owner_reach`] × the placement scale) — the draw-order rung this cloud and
    /// every CHILD cloud of it is biased by, so both clear the owner's own transparent batches
    /// (see [`owner_last_bias`]). Children carry the parent's, not their recursion model's: a
    /// child draws at the parent's anchor, so it is the parent's owner it has to clear.
    owner_reach: f32,
    /// The particle texture — the sim withholds pushing quads until it's resident: a
    /// still-loading or failed-to-decode texture would otherwise flash the engine fallback
    /// (white/magenta) through the additive blend. Particles still simulate meanwhile.
    texture: Handle<Image>,
    /// The draw-set gate's memory (was the emitter entity's `Visibility` flip): true while the
    /// owner is out of the frame's draw set — the edge on which model-instance entities are
    /// hidden. Quads need no flag: a gated pool simply pushes nothing into the shared stream.
    gated: bool,
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
/// emitter def + texture and a private pool. Simulated entirely inside the parent's sim block
/// (no ECS ordering with the parent's fresh pool); its quads go into the shared stream as one
/// draw of its own, at the parent's anchor and rung.
struct ChildEmitter {
    def: ParticleEmitterDef,
    texture: Handle<Image>,
    particles: Vec<Particle>,
    accumulator: f32,
    gate_prev: bool,
    rng: u32,
}

/// Marker on a MODEL-particle instance entity ([`model`]) — keeps the sim's transform queries
/// provably disjoint (the owner-read query excludes these; the instance write query requires
/// it).
#[derive(Component)]
pub struct ChildDraw;

#[cfg(test)]
impl ChildEmitter {
    /// A bare child for unit tests.
    fn bare(def: ParticleEmitterDef) -> Self {
        Self {
            def,
            texture: Handle::default(),
            particles: Vec::new(),
            accumulator: 0.0,
            gate_prev: false,
            rng: 7,
        }
    }
}

/// What becomes of a live pool when its owner entity goes away — the distinction the sim itself
/// cannot make, because from inside it a despawned owner looks the same either way (decision 0826).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum OwnerLoss {
    /// The owner MODEL was destroyed: **free the emitter with it**, live particles and all. This
    /// is the reference's own rule — a model's emitters are released synchronously at its dtor
    /// (wow-re `ribbon-basis-emitter-lifecycle`) — and it is what an equipped item that is
    /// replaced/unequipped, or a unit that streams out, must do. Draining instead left the cloud
    /// hanging in world space for a whole lifespan while the character walked away from it.
    Free,
    /// The owner was reaped but the effect should finish: emission stops and the pool lives out
    /// its lifespans at the frozen placement (a missile that impacted, a spell effect that
    /// ended — the reference's model outliving its emitters while they drain). The default,
    /// because it is what every pre-0826 caller got.
    #[default]
    Drain,
}

/// The live frames an emitter rides — bundled so a call site names each one instead of passing a
/// row of positional `Option<Entity>`s (they are easy to swap and impossible to tell apart at the
/// call). See the matching [`ParticleEmitter`] fields for what each frame does.
#[derive(Clone, Copy, Default)]
pub struct EmitterFrames {
    /// The entity whose live transform is this emitter's placement: `(entity, [0; 3])` for a
    /// streamed creature/GameObject/held item (the whole model rides), or `(joint, bone_pivot)`
    /// for an emitter riding its animated bone (0130 phase 4 — the pivot rebases the model-space
    /// `def.position` into the joint's own frame, so an unanimated chain reproduces the static
    /// path exactly). `None` for a static doodad (fixed placement, despawned by the placement's
    /// own entity list).
    pub owner: Option<(Entity, [f32; 3])>,
    /// The attachment matrix `A` frame ([`ParticleEmitter::attach`]).
    pub attach: Option<Entity>,
    /// The cloud anchor — the MODEL, never the bone ([`ParticleEmitter::anchor`]).
    pub anchor: Option<Entity>,
    /// The MODEL INSTANCE whose render alpha multiplies these particles
    /// ([`ParticleEmitter::alpha_src`]).
    pub alpha: Option<Entity>,
    /// What losing `owner` means ([`OwnerLoss`]).
    pub on_owner_loss: OwnerLoss,
}

/// Spawn an emitter entity for one [`ModelEmitter`] at `placement`. `None` if the emitter has no
/// resolved texture (nothing to draw); `frames` ties it to the live entities it rides.
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

    /// The particle texture handle — read by the phase probe ([`crate::capture`]) to report the
    /// GPU-side image state beside the phase membership (a main-world-resident image whose GPU
    /// prep failed leaves every draw silently skipped).
    pub(crate) fn texture(&self) -> &Handle<Image> {
        &self.texture
    }

    /// The owner-last draw-order rung this emitter's effects take (see [`owner_last_bias`]).
    ///
    /// Read by the MODEL-particle draw ([`model::update_model_particles`]), whose instances are
    /// built from the geometry model's own batch materials and would otherwise sit at rung 0 — so
    /// a 3-D particle would be painted over by its own sibling quad cloud. The reference draws all
    /// of a model's emitters in the one bracket; sharing this number is what says so.
    pub(super) fn owner_rung(&self) -> f32 {
        owner_last_bias(self.owner_reach)
    }

    /// Is this emitter in the frame's draw set — i.e. did the draw-set gate let it tick and push
    /// quads this frame? The instrument-side spelling of [`Self::gated`], read by the particle
    /// census probe. (It read the entity's `Visibility` until slice P2 (0733) moved the family onto
    /// the shared stream and emitter entities stopped carrying one — after which the probe's query
    /// matched NOTHING and every column it printed, including B39's `drawn_beyond_wall` guard, was
    /// a vacuous zero.)
    pub(crate) fn drawn(&self) -> bool {
        !self.gated
    }

    /// This frame's MODEL render alpha ([`Self::alpha_src`]) — read by the MODEL-particle lane,
    /// whose instances carry their alpha in a `MeshTag` rather than a vertex colour.
    pub(super) fn render_alpha(&self) -> f32 {
        self.alpha
    }

    /// The cloud's live world anchor — the census probe's fallback distance subject for an emitter
    /// with no [`EmitterFade`] (entity-owned: creatures, GameObjects, spell kits). A faded
    /// emitter's distance is measured to its OWNER's sphere instead, because that sphere is what
    /// the draw-set gate actually tests.
    pub(crate) fn anchor_world(&self) -> Vec3 {
        self.anchor_pos
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
    /// The WMO placement whose doodad set this emitter's owner belongs to; `None` for an ADT map
    /// doodad. Read only by the exterior-window term: a prop of the building the camera is standing
    /// in is not exterior scene to itself, so its candle keeps burning (decision 0784's exemption,
    /// which the owner submesh gets through `WmoGroupVis` — an emitter carries no such component,
    /// so it carries the instance directly).
    pub instance: Option<Entity>,
}

impl EmitterFade {
    /// This owner's distance-fade ALPHA (not the cutoff): the reference writes it into the
    /// doodad's `CM2Model+0x180`, so it multiplies that model's particles exactly as it does its
    /// batches (`FUN_00683f80` → `+0x180` → `+0x19c` → `emitter+0x1a8`, decision 0827). The
    /// [`Self::in_draw_set`] gate is the same curve's zero crossing — this is the feather before
    /// it, which is why a small prop's flame now thins out over its band instead of cutting.
    pub fn distance_alpha(&self, cam_pos: Vec3) -> f32 {
        let (dx, dz) = (self.center.x - cam_pos.x, self.center.z - cam_pos.z);
        crate::model_fade::doodad_fade_alpha(self.radius, (dx * dx + dz * dz).sqrt())
    }

    /// The **draw-set admission rule** in one place: is the owner doodad in the frame's scene
    /// worklist, and therefore does its emitter tick and draw this frame?
    ///
    /// Four terms, all against the owner's fade sphere (the reference's `[rec+0x68]`):
    /// 1. the radius-tiered distance fade hasn't reached zero (`FUN_00683f80`),
    /// 2. the sphere is inside the far-clip wall ([`crate::view::within_farclip`]),
    /// 3. `lateral_in_frustum` — the caller's frustum sphere test on the side/near planes,
    /// 4. `exterior_admitted` — the caller's exterior-window test (decision 0786).
    ///
    /// Term 2 is the one bug B39 was missing (decision 0678), and it cannot be folded into term 1:
    /// [`crate::model_fade::doodad_fade_alpha`] returns a flat `1.0` for any owner bigger than
    /// [`crate::model_fade::NEVER_FADE_RADIUS`], so for exactly the props that carry the big
    /// effects — braziers, bonfires, portal frames — term 1 admits at *every* distance.
    ///
    /// Term 4 is the same omission one layer out. The reference links a doodad into the worklist
    /// through the per-window populate walk `0x683700` and ticks its particles as part of that
    /// model's animate step — so an unlinked doodad emits nothing. We gated the doodad's *mesh* and
    /// left its emitter running, which is a mushroom's spore cloud hanging in a sealed dungeon (the
    /// director's report: `plaguelandmushroom01`, 3 emitters, 59.7 yd through a wall).
    ///
    /// Pure and total so the rule is pinned by tests without an ECS (the `model_fade` pattern).
    pub fn in_draw_set(
        &self,
        cam_pos: Vec3,
        cam_fwd: Vec3,
        farclip: f32,
        lateral_in_frustum: bool,
        exterior_admitted: bool,
    ) -> bool {
        let (dx, dz) = (self.center.x - cam_pos.x, self.center.z - cam_pos.z);
        let horiz = (dx * dx + dz * dz).sqrt();
        crate::model_fade::doodad_fade_alpha(self.radius, horiz) > 0.0
            && crate::view::within_farclip(farclip, cam_pos, cam_fwd, self.center, self.radius)
            && lateral_in_frustum
            && exterior_admitted
    }

    /// Term 4 for this emitter, given the frame's gate and the placement the camera is inside. Kept
    /// beside the rule so both callers — the particle sim and the meshless-host anim gate — ask the
    /// same question instead of each re-deriving the exemption.
    pub fn exterior_admitted(
        &self,
        gate: &crate::exterior_cull::ExteriorGate,
        camera_instance: Option<Entity>,
    ) -> bool {
        if self.instance.is_some() && self.instance == camera_instance {
            return true; // a prop of the building the camera stands in — not exterior to itself
        }
        gate.admits_sphere(self.center, self.radius)
    }
}

pub fn spawn_emitter(
    commands: &mut Commands,
    emitter: &ModelEmitter,
    placement: Transform,
    frames: EmitterFrames,
    clock: EmitClock,
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
    let owner = frames.owner.map(|(entity, pivot)| {
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
    // Gate on the rate track's PEAK over every sequence, not its first key: a one-shot burst
    // emitter (the blood spurt's starflash/glowball, 0140 fold-back) keys `0 → 200 → 0` — value[0]
    // is 0 but it absolutely emits.
    if def.params.peak_lifespan() <= 0.0 || def.timing.peak_rate() <= 0.0 {
        return None; // emits nothing
    }
    let (host, seq) = match clock {
        EmitClock::Pinned => (None, None),
        EmitClock::Effect(s) => (None, s),
        EmitClock::Host(h) => (Some(h), None),
    };
    // The owner's reach is model-local; the rung is a view-space distance, so it takes the
    // placement scale with it (a scaled-up creature's batches spread proportionally).
    let owner_reach = emitter.owner_reach * placement.scale.max_element();
    // Seed the RNG from the placement position so two campfires don't flicker in lockstep.
    let t = placement.translation;
    let rng = (t.x.to_bits() ^ t.y.to_bits().rotate_left(11) ^ t.z.to_bits().rotate_left(22))
        .wrapping_mul(0x9E37_79B9)
        | 1;
    Some(
        commands
            .spawn((
                // The sim writes the anchor here each frame — the census/phase instruments'
                // read point (the draw's own sort key rides the draw record instead).
                Transform::IDENTITY,
                ParticleEmitter {
                    def,
                    placement,
                    owner,
                    on_owner_loss: frames.on_owner_loss,
                    draining: false,
                    attach: frames.attach,
                    attach_rot: Quat::IDENTITY,
                    alpha_src: frames.alpha,
                    alpha: 1.0,
                    anchor: frames.anchor,
                    anchor_pos: placement.translation,
                    particles: Vec::new(),
                    accumulator: 0.0,
                    emitter_prev: None,
                    inherit_accum: 0.0,
                    inherit_vel: Vec3::ZERO,
                    gate_prev: false,
                    age: 0.0,
                    host,
                    seq,
                    rng,
                    owner_reach,
                    texture,
                    gated: false,
                    recursion: emitter.recursion.clone(),
                    children: Vec::new(),
                    geometry: emitter.geometry.clone(),
                    model_instances: Vec::new(),
                },
            ))
            .id(),
    )
}

/// The **owner-last** draw-order rung: how far to bias one of a model's EFFECTS past that model's
/// own transparent batches (`Transparent3d` sort key = view-space z **+ depth_bias**, ascending, so
/// a positive bias draws LATER — the sign law lives in [`crate::sky_order`]).
///
/// The reference draws a model's emitters in their own push/pop bracket (`0x70d8b0`) *after* that
/// model's batches, unconditionally — never interleaved with them. Bevy has one distance-sorted
/// transparent list, and an effect mesh carries `NoFrustumCulling`, which suppresses the `Aabb`
/// Bevy would otherwise sort by, so it sorts at its entity translation while each body batch sorts
/// at its own bind-pose AABB centre a yard or two up the model. Both measured shapes of that:
///
/// - **Quad clouds** sort at the owner's ORIGIN, so the order flips with camera ELEVATION. On the
///   voidwalker at 12 yd: at 0° the eye emitters land at transparent slots 324/325 with 5 of the 14
///   blend batches still behind them; at 35° they drop to slots 7/8 and **all 14** draw over them —
///   B16, the eye glow visible from below the eye horizon and gone from above (decision 0719).
/// - **Ribbon trails** sort at the live head node, which sits *inside* the owner, so they interleave
///   at every angle and the interleave MOVES. On the wisp at 6 yd, elevation 0°: its three streamers
///   land at slots 310/317/324 among the wisp's own 14 blend batches, 6 batches deep each, and which
///   batches are over which streamer changes frame to frame as the streamers whip (decision 0721).
///
/// `reach` is the owner's authored [`benilla_assets::ModelEmitter::owner_reach`] (or
/// [`benilla_assets::ModelRibbon::owner_reach`]) in **world** yards — the model-local bound
/// [`benilla_formats::m2_owner_reach`] measures, times the placement scale. View-space z is
/// 1-Lipschitz in world position and no owner batch sorts farther than `reach` from its origin, so
/// `z(origin) + reach ≥ z(any of its batch centres)`.
///
/// Every effect of one model must take the SAME rung, or the law is broken from the other side: the
/// reference draws a model's emitters in file order within the bracket, so a quad cloud on rung 4
/// beside a model-particle instance on rung 0 would paint over its own sibling.
///
/// The rounding, the ceiling and their reasons live with the shared implementation in
/// [`benilla_formats::owner_last_rung`] — the survey tools (`emdump`, `benilla-extract
/// fxordercensus`) print the same number this applies, and a rung computed twice drifts twice.
pub(crate) fn owner_last_bias(reach: f32) -> f32 {
    benilla_formats::owner_last_rung(reach)
}

/// Wire pending CHILD emitters (wow-re `part-child-recursion.md`, VERIFIED): once a parent's
/// recursion model resolves, its own particle emitters — **capped at 4** (`0x7b5dfe`) — become
/// the parent's children, each with its private pool (drawn from the shared stream at the
/// parent's anchor and rung). The reference wires at the child model's async-load completion
/// (`0x7b5dd0`); this system is that completion hook. A child never self-emits ambiently — its
/// only particle source is the per-parent-particle drive in the sim.
pub(crate) fn wire_child_emitters(
    models: Res<Assets<benilla_assets::M2Model>>,
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
                if em.def.params.peak_lifespan() <= 0.0 || em.def.timing.peak_rate() <= 0.0 {
                    return None;
                }
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
                })
            })
            .collect();
        emitter.recursion = None;
        emitter.children = children;
    }
}

/// Which sequence clock an emitter's rate/enabled tracks sample against
/// ([`benilla_formats::EmitTiming`] — one baked loop per sequence, so the consumer names the
/// sequence). Callers pick per lane at [`spawn_emitter`].
#[derive(Clone, Copy, Default)]
pub enum EmitClock {
    /// Slot 0 on the spawn-age clock — a placed doodad's one-time arm (the pre-per-sequence law,
    /// still correct there), the portrait booths, and any lane with no rig to ask.
    #[default]
    Pinned,
    /// The SPELL-FX lane: the rig's armed slot (a missile's InFlight is not file-order-first;
    /// `None` = slot 0) on the spawn-age clock, like every pinned lane — the effect instance is
    /// byte-verified fresh per play (decision 0858), so its gseq loops open at phase 0 with it.
    Effect(Option<usize>),
    /// A live host's `AnimationPlayer` decides the slot **and the clip time** each frame — units
    /// and GameObjects, whose playing sequence changes; the reference's `m2_animate` emitter
    /// phase samples the CURRENT sequence record (wow-re `part-emission-rate-animated.md` §2).
    Host(Entity),
}

/// One frame of the emission front end: how many births the pool is owed, loaded into
/// `accumulator` (fractional; the caller's birth loop drains whole particles). The reference's
/// per-frame emitter pass + spawn driver (`0x718960` / `0x7b5550`, wow-re
/// `part-emission-burst-flag.md` + `part-emission-rate-animated.md`):
///
/// - `rate`/`emitting` arrive already sampled from the playing sequence's key window
///   ([`benilla_formats::EmitTiming`]) and the gate is `(enabled && rate > 0)`.
/// - A **continuous** emitter (file flag 0x8000 clear) pours `rate · density·LOD · dt`.
/// - A **BURST** emitter (flag set) loads `ftol(rate · density·LOD)` ONCE on the gate's rising
///   edge and re-arms when it falls (a looping clip's next pass re-fires it); the pour never
///   runs for it. This is what bounds the Feint/Eviscerate impact's plume+crescents at one puff
///   ~67 ms in — over by ~0.5 s — while the same-shaped cast-hands flame pours its whole clip.
///
/// Returns the burst count loaded this frame (0.0 otherwise) — the `fx` trace's measurement hook.
fn accumulate_emission(
    is_burst: bool,
    rate: f32,
    emitting: bool,
    scale: f32,
    dt: f32,
    accumulator: &mut f32,
    gate_prev: &mut bool,
) -> f32 {
    if !emitting {
        *accumulator = 0.0;
    }
    let rate = if emitting { rate.max(0.0) } else { 0.0 };
    let gate = rate > 0.0;
    let mut burst = 0.0;
    if is_burst {
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
        // The whole effect family draws through the dedicated lane
        // ([`render::EffectLanePlugin`]); `WowParticleMaterial` and its MaterialPlugin retired
        // with slice P2 (0733 §1) when precipitation and water foam moved onto the stream.
        app.add_plugins(render::EffectLanePlugin)
            .init_resource::<ParticleTuning>()
            .init_resource::<buffer::EffectQuads>()
            // PostUpdate, after the billboard joint palette: an emitter riding a billboarded
            // bone must sample the REPLACED frame, same frame — an Update-time read gets the
            // un-billboarded pose back (avian's fixed-loop sync re-propagates from locals), and
            // the Demon Skin flames followed the character instead of the camera. This also
            // retires the one-frame lag emitters on ANY animated joint carried while reading
            // last frame's globals from Update.
            //
            // `begin_effect_frame` clears the shared stream before BOTH writers (the ribbon sim
            // is the other), so the writer order between them stays free.
            .add_systems(
                PostUpdate,
                (
                    buffer::begin_effect_frame
                        .before(simulate_particles)
                        .before(crate::ribbons::simulate_ribbons),
                    wire_child_emitters,
                    simulate_particles,
                    model::update_model_particles,
                )
                    .chain()
                    .in_set(crate::billboard::BillboardPlace)
                    .after(crate::billboard::billboard_joint_palette)
                    .after(crate::creature_anim::finalize_rig_worlds)
                    // …and after the CARD facing pass, for the same reason one step further out:
                    // an equipped item's emitter rides a mesh-less billboard *frame* card (its
                    // bone chain reaches a billboard bone and an item model has no rig to carry
                    // the palette replacement — decision 0813), so that card's transform has to be
                    // this frame's before births read it.
                    .after(crate::billboard::face_billboards),
            );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use benilla_formats::ParticleShape;

    /// The minimal def, exposed for sibling-module tests (the sim's child-drive test).
    pub(crate) fn plain_def() -> ParticleEmitterDef {
        super::emit::tests::def(ParticleShape::Plane)
    }

    /// Camera at the origin looking down −Z (Bevy's convention), and an owner sphere `depth` yd
    /// straight ahead — the geometry every draw-set test below uses.
    fn gate(radius: f32, depth: f32, farclip: f32) -> bool {
        EmitterFade {
            radius,
            center: Vec3::new(0.0, 0.0, -depth),
            instance: None,
        }
        .in_draw_set(Vec3::ZERO, Vec3::NEG_Z, farclip, true, true)
    }

    /// **The owner-last rung, pinned** (decisions 0719/0721), through the wrapper the renderer
    /// actually calls. The whole point of it is one inequality — the rung must be STRICTLY greater
    /// than the owner's reach, or a batch centred at the edge of the model ties with the effect and
    /// the draw order goes back to being whatever the queue emitted. The voidwalker (reach 3.666 yd
    /// by the old vertex bound, 1.945 by the batch-centre one) is the measured case; the integer
    /// reach is the one that would tie under a bare `ceil`.
    #[test]
    fn the_owner_last_rung_always_clears_the_owner() {
        for reach in [0.0f32, 0.5, 1.0, 3.666, 4.0, 7.999, 12.0, 31.5] {
            let rung = owner_last_bias(reach);
            assert!(rung > reach, "rung {rung} must clear reach {reach}");
            assert_eq!(rung, rung.trunc(), "rung {rung} must be a whole yard");
        }
        // A model too big for the ladder is capped rather than allowed to climb into
        // `sky_order`'s rungs — it loses its own ordering, not the world's.
        assert_eq!(owner_last_bias(500.0), 32.0);
        assert_eq!(owner_last_bias(-1.0), 1.0);
    }

    /// **The B39 defect, pinned** (decision 0678). An owner bigger than `NEVER_FADE_RADIUS` never
    /// distance-fades, so the fade term admits it at every distance — the far-clip term is the only
    /// thing that stops its emitter. Reverting that term makes this test fail, which is the point:
    /// a big fire prop's flames used to draw a kilometre away, over terrain the same wall had
    /// already discarded.
    #[test]
    fn a_never_fading_owner_is_still_bounded_by_the_wall() {
        let big = crate::model_fade::NEVER_FADE_RADIUS + 5.0;
        // The fade term alone says "draw" at any distance — that is what made this invisible.
        assert_eq!(crate::model_fade::doodad_fade_alpha(big, 5000.0), 1.0);
        assert!(gate(big, 500.0, 777.0), "inside the wall: draws");
        assert!(!gate(big, 1000.0, 777.0), "past the wall: must NOT draw");
        // ...and it tracks the live farclip, not a constant: the same emitter at the vanilla
        // minimum view distance is out, at the panel's maximum it is back in.
        assert!(!gate(big, 500.0, 177.0));
        assert!(gate(big, 1000.0, 1200.0));
    }

    /// The wall never *replaces* the fade cutoff — a small prop still pops at its own band end
    /// (40→50 yd) far inside the wall. Both terms are live; neither subsumes the other.
    #[test]
    fn the_wall_does_not_replace_the_size_fade() {
        assert!(gate(0.3, 45.0, 777.0), "mid-band: still drawing");
        assert!(
            !gate(0.3, 60.0, 777.0),
            "past the 50-yd band end, nowhere near the wall"
        );
    }

    /// The lateral frustum term is ANDed, not implied: an owner dead ahead and well inside the
    /// wall is still out when the caller's frustum test rejects it (off-screen).
    #[test]
    fn the_lateral_frustum_term_is_anded() {
        let f = EmitterFade {
            radius: 2.0,
            center: Vec3::new(0.0, 0.0, -60.0),
            instance: None,
        };
        assert!(f.in_draw_set(Vec3::ZERO, Vec3::NEG_Z, 777.0, true, true));
        assert!(!f.in_draw_set(Vec3::ZERO, Vec3::NEG_Z, 777.0, false, true));
        // …and the exterior-window term is ANDed the same way (0786): a doodad no portal window
        // admits is not in the worklist, so its emitter neither ticks nor draws.
        assert!(!f.in_draw_set(Vec3::ZERO, Vec3::NEG_Z, 777.0, true, false));
    }

    /// **The exemption, on the emitter lane.** A sealed room (no windows) admits no exterior scene
    /// — but the props of the building the camera is standing IN are not exterior to it, and their
    /// flames must keep burning. Getting this wrong does not look like a cull bug; it looks like
    /// walking into an inn snuffs its fireplace, which is why it is pinned rather than argued.
    ///
    /// The third case is the reported one: an ADT map doodad carries no building at all, so a
    /// sealed room must stop it (`plaguelandmushroom01`'s spore cloud, seen through a wall).
    #[test]
    fn a_sealed_room_keeps_its_own_props_burning_and_stops_everything_else() {
        use crate::exterior_cull::ExteriorGate;
        let mut w = World::new();
        let (here, elsewhere) = (w.spawn_empty().id(), w.spawn_empty().id());
        let sealed = ExteriorGate::Windows(Vec::new());
        let fade = |instance| EmitterFade {
            radius: 2.0,
            center: Vec3::new(0.0, 0.0, -60.0),
            instance,
        };

        assert!(
            fade(Some(here)).exterior_admitted(&sealed, Some(here)),
            "a prop of the building the camera is in must keep emitting"
        );
        assert!(
            !fade(Some(elsewhere)).exterior_admitted(&sealed, Some(here)),
            "another building's prop is exterior scene, and the room is sealed"
        );
        assert!(
            !fade(None).exterior_admitted(&sealed, Some(here)),
            "an ADT map doodad belongs to no building — the reported mushroom"
        );
        // Outdoors the gate stands down and every one of them emits again.
        for instance in [Some(here), Some(elsewhere), None] {
            assert!(fade(instance).exterior_admitted(&ExteriorGate::Open, Some(here)));
        }
        // …and with no room claimed at all, `None == None` must NOT read as "my own building".
        assert!(!fade(None).exterior_admitted(&sealed, None));
    }

    /// The emitter gate and the owner mesh's own cull must agree on the boundary — they read the
    /// same `within_farclip` now, and this pins that they cannot drift apart again. An emitter
    /// admitted where its owner's mesh is culled IS bug B39.
    ///
    /// Scoped to owners **above** `NEVER_FADE_RADIUS` on purpose: there the size-fade term is a
    /// constant `1.0`, so the gate reduces to the wall alone and the comparison is exact. Below it
    /// the two rules legitimately differ (the mesh keeps drawing while the fade feathers it out),
    /// so an equality there would be asserting something false — and these are the owners that
    /// carry the big effects the report is about anyway.
    #[test]
    fn the_emitter_gate_agrees_with_the_owner_meshs_cull() {
        let (cam, fwd) = (Vec3::ZERO, Vec3::NEG_Z);
        for farclip in [177.0f32, 777.0, 1200.0] {
            for depth in [100.0f32, 700.0, 776.0, 777.0, 778.0, 900.0, 2000.0] {
                for radius in [crate::model_fade::NEVER_FADE_RADIUS + 0.01, 12.0, 40.0] {
                    let center = Vec3::new(0.0, 0.0, -depth);
                    assert_eq!(
                        crate::model_fade::doodad_fade_alpha(radius, depth),
                        1.0,
                        "precondition: this owner never size-fades"
                    );
                    let mesh_drawn = crate::view::within_farclip(farclip, cam, fwd, center, radius);
                    let emitter_drawn = EmitterFade {
                        radius,
                        center,
                        instance: None,
                    }
                    .in_draw_set(cam, fwd, farclip, true, true);
                    assert_eq!(
                        mesh_drawn, emitter_drawn,
                        "wall disagreement at farclip {farclip} depth {depth} radius {radius}"
                    );
                }
            }
        }
    }

    /// The BURST emission model (file flag 0x8000 — Feint/Eviscerate impact's plume+crescents):
    /// one `ftol(rate·scale)` puff on the rising edge of `(enabled && rate > 0)`, latched while
    /// the gate holds, re-armed when it falls. (The rate/enabled *sampling* laws — step vs lerp
    /// vs held tail, per sequence window — are pinned in `benilla-formats`' `emit_timing` tests,
    /// where the bake lives.)
    #[test]
    fn burst_emitter_fires_once_on_the_rising_edge() {
        let (mut acc, mut prev) = (0.0, false);
        assert_eq!(
            accumulate_emission(true, 0.0, true, 1.0, 0.016, &mut acc, &mut prev),
            0.0,
            "rate 0 — no gate, no burst"
        );
        assert_eq!(acc, 0.0);
        assert_eq!(
            accumulate_emission(true, 30.0, true, 1.0, 0.016, &mut acc, &mut prev),
            30.0,
            "the frame the rate rises: one full-count burst"
        );
        assert_eq!(acc, 30.0);
        acc = 0.0; // the birth loop drains it
        accumulate_emission(true, 30.0, true, 1.0, 0.016, &mut acc, &mut prev);
        accumulate_emission(true, 30.0, true, 1.0, 0.016, &mut acc, &mut prev);
        assert_eq!(acc, 0.0, "held-high gate stays latched — never a pour");
        // The gate falls (drain/disable) → re-arms → the next rise bursts again (a looping
        // clip's next pass), the count ftol-truncated through density·LOD.
        accumulate_emission(true, 30.0, false, 1.0, 0.016, &mut acc, &mut prev);
        assert_eq!(
            accumulate_emission(true, 30.0, true, 0.55, 0.016, &mut acc, &mut prev),
            16.0,
            "ftol(30 · 0.55) = 16"
        );
    }

    /// The continuous emission model (flag clear) pours `rate·dt` — and drops its owed
    /// fraction while disabled (the pre-existing drain hygiene).
    #[test]
    fn continuous_emitter_pours_rate_dt() {
        let (mut acc, mut prev) = (0.0, false);
        accumulate_emission(false, 30.0, true, 1.0, 0.1, &mut acc, &mut prev);
        accumulate_emission(false, 30.0, true, 1.0, 0.1, &mut acc, &mut prev);
        assert!((acc - 6.0).abs() < 1e-4, "30/s × 0.2 s, no burst latch");
        accumulate_emission(false, 30.0, false, 1.0, 0.1, &mut acc, &mut prev);
        assert_eq!(acc, 0.0, "disabled zeroes the owed fraction");
    }

    /// The follow-delta response line (`0x7b5d30`): the line through the two authored
    /// (speed, fraction) samples; equal speeds degenerate to no response (the reference zeroes
    /// slope and intercept).
    #[test]
    fn follow_line_matches_the_authored_two_point_response() {
        let mut d = super::emit::tests::def(ParticleShape::Plane);
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
}
