use avian3d::prelude::SpatialQuery;
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use bevy::camera::primitives::{Frustum, Sphere as CullSphere};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::Projection;
use bevy::prelude::*;

use crate::view::WorldCamera;

use super::buffer::{EffectDrawSpec, EffectFog, EffectLightOverride, EffectQuads};
use super::quads::{expand_quads, CamBasis, DrawFrame};
use super::{
    accumulate_emission, emit_local, next_u32, rand01, rand_s11, ChildDraw, ChildEmitter,
    EmitterFade, OwnerLoss, Particle, ParticleEmitter, ParticleTuning, MAX_PARTICLES,
};

/// The emitter-constant inputs of one integrator step ([`integrate_particle`]).
struct StepEnv {
    dt: f32,
    gravity: f32,
    drag: f32,
    anchored: bool,
    kill_origin: Option<Vec3>,
    /// The frame's shared follow-delta vector, stored frame (zero when unauthored).
    follow: Vec3,
}

/// One step of the verified vanilla integrator (`particle_integrate` @ `0x7b2680`): age/kill,
/// the follow-delta add (before the velocity step, skipped once on a fresh particle — the
/// `0x7b2744` branch), `pos += dt·v` with the gravity term on the frame's up axis
/// (`pos.up −= ½·g·dt²`, `v.up −= g·dt`), then drag (`v −= min(dt·drag, 1)·v`) — and, for a
/// sphere KILL-OUTBOUND emitter (`rt 0x800`,
/// [`benilla_formats::ParticleEmitterDef::kill_outbound`]), the reference's tail test: the
/// particle dies the frame `dot(stepVelocity, pos − origin) > 0`, where `stepVelocity` is the
/// pre-gravity, pre-drag velocity (exactly the value the position update consumed — the byte
/// order at `0x7b2680`) and `origin` is the emitter origin in the stored frame (the reference's
/// stored coords put the emitter at zero; ours carry the anchor/bone offset, so the caller
/// re-origins — same geometry). Returns false = dead.
fn integrate_particle(p: &mut Particle, env: &StepEnv) -> bool {
    let (dt, g) = (env.dt, env.gravity);
    p.age += dt;
    // Kill at the particle's OWN birth-sampled lifetime (the emitter's lifespan channel
    // animates — decision 0844; the reference feeds each spawn the current value).
    if p.age >= p.life {
        return false;
    }
    // FOLLOW-DELTA (file 0x4000, wow-re `part-emitter-motion.md` §2): the shared per-frame
    // fraction of the emitter's motion, applied to every particle EXCEPT on its first
    // integrate (the reference's particle+0xd bit — set at spawn, cleared here unconsumed).
    if p.fresh {
        p.fresh = false;
    } else {
        p.pos += env.follow;
    }
    // MODEL-particle tumble (`0x7b28e0`, wow-re `part-model-particles.md`): the Rodrigues
    // half-angle Δquat, body-frame right-multiply, skipped below the reference's 1e-4
    // threshold — then the shared linear integrator below. Quad particles carry zero.
    let theta = p.angvel.length();
    if theta > 1e-4 {
        p.quat = (p.quat * Quat::from_axis_angle(p.angvel / theta, theta * dt)).normalize();
    }
    let step_vel = p.vel;
    p.pos += p.vel * dt;
    if env.anchored {
        p.pos.y -= 0.5 * g * dt * dt;
        p.vel.y -= g * dt;
    } else {
        p.pos.z -= 0.5 * g * dt * dt;
        p.vel.z -= g * dt;
    }
    if env.drag != 0.0 {
        let f = (dt * env.drag).min(1.0);
        p.vel -= f * p.vel;
    }
    if let Some(origin) = env.kill_origin {
        if step_vel.dot(p.pos - origin) > 0.0 {
            return false;
        }
    }
    true
}

/// **How much of the emitter's per-frame world motion a live particle keeps** — the ride-vs-trail
/// law, in one place (decision 0986; `speed` is `|Δ| / dt`, the yd/s the authored follow response
/// is keyed on). Two inputs, and reading the first one off the *flag* is what left every hunter
/// shot a compact blob where the reference draws a 20-yd bead trail (bug B153):
///
/// 1. **The baseline is the host class, not a flag** (wow-re `part-emitter-motion.md` §2b's own
///    discriminator — 0513 read it as follow-vs-no-follow): a cloud whose model the scene graph
///    carries keeps **100%** (the running kobold's candle rides, file flags `0x01`); a FREE world
///    model that composes its own motion into the emitter matrix — §2b's "a translating missile
///    whose own model IS the emitter" — keeps **0%**, its births baking world-absolute so each
///    particle hangs where it was born. Multi-Shot's FLARE emitters are as unflagged as the
///    kobold's candle (`0x0309`) and trail the arrow the length of its flight.
/// 2. **The follow-delta term** (file `0x4000`, §5-resolved decision 0513) then *overrides* the
///    baseline with the authored two-point response
///    ([`benilla_formats::ParticleEmitterDef::follow_line`]). Which is exactly why the corpus
///    authors it on missiles and nothing else: it exists to claw a fast projectile's head glow
///    back onto the tip (ArcaneShot: 0.1 @ 2.5 yd/s → 0.9 @ 16.7) while the unflagged emitters on
///    the same model stay behind. A degenerate response — equal authored speeds, which the
///    reference answers by zeroing both — is no follow term at all, so the baseline stands.
fn world_motion_kept(
    def: &benilla_formats::ParticleEmitterDef,
    world_composed: bool,
    speed: f32,
) -> f32 {
    let baseline = if world_composed { 0.0 } else { 1.0 };
    if !def.follow_emitter() {
        return baseline;
    }
    def.follow_line().map_or(baseline, |(slope, intercept)| {
        (slope * speed + intercept).clamp(0.0, 1.0)
    })
}

/// The velocity-inherit trigger (wow-re `part-emitter-motion.md` §1, `0x7b5230` bytes
/// 0x7b53ce–0x7b54ca): accumulate dt; once past 1/30 s (`_DAT_0081d82c`), hold
/// `oneFrameΔ · ((1/30)/accum) · scale` — zeroed while nothing is live — and reset. Between
/// triggers the held value stands (births keep reading it). The exact factor carries the ×1/30
/// the first transcription sketch missed (a ~30× over-kick).
fn inherit_trigger(accum: &mut f32, held: &mut Vec3, dt: f32, delta: Vec3, live: bool, scale: f32) {
    const INTERVAL: f32 = 1.0 / 30.0;
    *accum += dt;
    if *accum > INTERVAL {
        *held = if live {
            delta * (INTERVAL / *accum) * scale
        } else {
            Vec3::ZERO
        };
        *accum = 0.0;
    }
}

/// The per-frame CHILD drive (wow-re `part-child-recursion.md`, VERIFIED): each child's spawn
/// accumulation runs **once per live parent particle** — the reference's `0x7b5b9f` call with
/// the CHILD receiver and the parent context's translation swapped to the particle's
/// post-integration position — so births land at whichever particle's call tips the child's
/// own `rate·dt` accumulator (volume scales with the parent's live count), composed through
/// the PARENT's rotation fold (the child's record position never composes — the context
/// translation is REPLACED by the particle). Child file flag 0x40 routes the parent
/// PARTICLE's velocity into the child's inherit add (`(1+S11·var)·v` — for children the
/// inherit vector IS the particle velocity, copied per call at `0x7b5b5e`). A burst child
/// latches on its first call of the rising-edge frame. A child never self-emits ambiently.
#[allow(clippy::too_many_arguments)] // the birth fold's full frame, same as the parent path
fn drive_child(
    child: &mut ChildEmitter,
    now: &benilla_formats::ParamsNow,
    parent: &[Particle],
    rate: f32,
    emitting: bool,
    scale: f32,
    dt: f32,
    anchored: bool,
    attach_inv: Quat,
    placement: &Transform,
) {
    let origin = Vec3::from(child.def.position);
    for p in parent {
        accumulate_emission(
            child.def.burst(),
            rate,
            emitting,
            scale,
            dt,
            &mut child.accumulator,
            &mut child.gate_prev,
        );
        while child.accumulator >= 1.0 && child.particles.len() < MAX_PARTICLES {
            child.accumulator -= 1.0;
            let (base, dir) = emit_local(&child.def, now, &mut child.rng);
            let local = base - origin;
            let speed = now.emission_speed * (1.0 + now.speed_variation * rand_s11(&mut child.rng));
            let fold = |v: Vec3| {
                if anchored {
                    attach_inv
                        * (placement.rotation * (placement.scale * wow_to_bevy(v.to_array())))
                } else {
                    v
                }
            };
            let mut vel = fold(dir * speed);
            if child.def.inherits_emitter_motion() {
                vel += (1.0 + now.speed_variation * rand_s11(&mut child.rng)) * p.vel;
            }
            let phase = next_u32(&mut child.rng);
            child.particles.push(Particle {
                pos: p.pos + fold(local),
                vel,
                age: 0.0,
                life: now.lifespan,
                phase,
                fresh: true,
                quat: Quat::IDENTITY,
                angvel: Vec3::ZERO,
            });
        }
    }
}

/// The **draw-set gate's** scene inputs, bundled: the far-clip wall the gate bounds emitters at
/// (0678) and the exterior-window test a WMO interior applies to everything outside it (0786),
/// with the camera's own room as its one exemption. One `SystemParam` because they are read
/// together, at one place, and Bevy caps a system at 16 parameters.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct SceneGates<'w> {
    view: Res<'w, crate::view::ViewDistance>,
    exterior_windows: Res<'w, crate::wmo_portal::ExteriorWindows>,
    camera_claim: Res<'w, crate::wmo_portal::CameraInteriorClaim>,
}

/// The **water-plane interleave** inputs ([`crate::sky_order::FAR_SIDE_BIAS`], where the
/// byte story lives): the loaded liquid surfaces, the eye's submersion verdict, and the room
/// components an anchor's liquid claim resolves from. One `SystemParam` for the same 16-cap
/// reason as [`SceneGates`].
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct WaterInterleave<'w, 's> {
    water: Query<'w, 's, &'static crate::liquid::WaterChunkInfo>,
    /// The spatial pre-filter every classification routes through: these lanes ask per DRAW
    /// (every admitted emitter, every ribbon, every transparent mesh batch), and the full-walk
    /// `surfaces_at` at that grain was the 2026-08-03 12-fps regression (`liquid::spatial`).
    index: Res<'w, crate::liquid::WaterIndex>,
    underwater: Res<'w, crate::liquid::Underwater>,
    rooms: Query<'w, 's, &'static crate::wmo_portal::UnitWmoRoom>,
    placements: crate::liquid::RoomPlacements<'w, 's>,
    parents: Query<'w, 's, &'static ChildOf>,
}

impl WaterInterleave<'_, '_> {
    /// The eye's side of the water — the term that inverts EVERY verdict (`far = above XOR
    /// submerged`). The mesh lane's reactive classifier re-runs its full walk on the frame this
    /// flips; per-draw callers just read it through [`far_side_of_water_at`].
    pub(crate) fn eye_submerged(&self) -> bool {
        self.underwater.0.any()
    }

    /// True when the loaded-surface population changed since the borrowing system last ran (the
    /// [`crate::liquid::WaterIndex`] rebuild edge) — any batch may have gained or lost the water
    /// plane under it, so the mesh lane's reactive classifier promotes the frame to a full walk.
    pub(crate) fn surfaces_changed(&self) -> bool {
        self.index.is_changed()
    }
}

/// Is this draw on the EYE's **far side** of its local water plane — the point-and-slack core.
///
/// The reference splits its transparents into the above-water and below-water lists and the
/// frame interleave draws the eye's far side *before* the water pass (dry eye: below is far;
/// submerged: above is — `0x4836d6`). Membership is `above ⇔ d ≥ −r` against the nearest
/// **admitted** surface over the point's XY (claim from the nearest ancestor with a room
/// verdict; `Unknown` admits both sources, still floor-bounded per pool). **No admitted surface
/// ⇒ the above list** (`+0x19c == 0 → A = 1`, wow-re `water-frame-straddle.md` §2) — the NEAR
/// side for a dry eye and the FAR side for a submerged one: shore content seen from under the
/// water draws before the surface, so the surface tints it (0921's correction of 0911's
/// "no surface → near").
///
/// `r` is the caller's lane law: the model bound-sphere slack for emitters and ribbons
/// ([`model_far_side`]), `0` for the mesh lane's no-clip-planes fallback (`A = d ≥ 0`,
/// `0x7079ed`; decision 0919).
pub(crate) fn far_side_of_water_at(
    w: &WaterInterleave,
    claim_seed: Option<Entity>,
    point_world: Vec3,
    r: f32,
) -> bool {
    let wow = benilla_assets::coords::bevy_to_wow(point_world);
    // The spatial pre-filter first — these lanes ask per DRAW, and the full `surfaces_at` walk
    // at that grain was the 2026-08-03 12-fps regression (`liquid::spatial`). No candidate
    // surface over this XY (the dominant dry-land case) is "no admitted surface" under every
    // claim, so the room walk below is skipped with the scan.
    let candidates = w.index.over(wow[0], wow[1]);
    let above = if candidates.is_empty() {
        true
    } else {
        let mut seed = claim_seed;
        let mut room = None;
        for _ in 0..8 {
            let Some(e) = seed else { break };
            if let Ok(rm) = w.rooms.get(e) {
                room = Some(rm);
                break;
            }
            seed = w.parents.get(e).ok().map(ChildOf::parent);
        }
        let claim = crate::liquid::unit_claim(room, &w.placements);
        let surfaces = candidates.iter().filter_map(|&e| w.water.get(e).ok());
        match crate::liquid::surfaces_at(surfaces, wow, claim)
            .map(|z| wow[2] - z)
            .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        {
            Some(d) => is_above(d, r),
            None => true,
        }
    };
    if w.underwater.0.any() {
        above
    } else {
        !above
    }
}

/// The mesh lane's wrapper: the sign test at the batch's own transform (r = 0) — the
/// reference's no-clip-planes fallback, `model_render::classify_water_side`'s law (0919).
pub(crate) fn far_side_of_water(
    w: &WaterInterleave,
    claim_seed: Option<Entity>,
    anchor_world: Vec3,
) -> bool {
    far_side_of_water_at(w, claim_seed, anchor_world, 0.0)
}

/// The emitter/ribbon lane: **the water side is the MODEL's** (byte-VERIFIED, wow-re
/// `water-frame-straddle.md` §6 — the 0921 correction of 0911's per-emitter gloss). The type-4
/// walk dots the plane ONCE per model in its prologue — `world_matrix × bound-box centre`,
/// slack `r = |matrix row 0| × header sphere radius` — and every emitter of the model reads
/// that one cached boolean (`[ebp-8]` at `0x7085fa`); the ribbon leg reads the model's side-A
/// verbatim (`0x7081f1`). So a shoulder flame cannot blink at the waterline: the verdict rides
/// the model's deep, stable bound centre with a whole radius of slack, never the bobbing bone.
///
/// `model` = the instance-root entity — the cloud ANCHOR field ("the MODEL, never the bone"),
/// whose `GlobalTransform` is the instance matrix; `bound` = the asset's
/// [`benilla_assets::ModelEmitter::water_bound`]. No resolvable matrix → the sign test at
/// `fallback_point` (the emitter's live anchor), r = 0.
pub(crate) fn model_far_side(
    w: &WaterInterleave,
    model: Option<Entity>,
    model_gt: Option<&GlobalTransform>,
    bound: (Vec3, f32),
    fallback_point: Vec3,
) -> bool {
    match model_gt {
        Some(gt) => {
            let point = gt.transform_point(bound.0);
            let r = gt.affine().matrix3.x_axis.length() * bound.1;
            far_side_of_water_at(w, model, point, r)
        }
        None => far_side_of_water_at(w, model, fallback_point, 0.0),
    }
}

/// The membership law alone, pure for the unit test: `above ⇔ d ≥ −r` (the reference's
/// `fcompp; test ah,0x41; jp` at `0x7084cf` — a tie at `d == −r` lands above; NaN lands below,
/// which `>=` gives for free). `d` = point − surface (WoW Z), `r ≥ 0` the lane's slack.
fn is_above(d: f32, r: f32) -> bool {
    d >= -r
}

/// Per-frame: emit, integrate, and expand each emitter's pool into the shared effect-quad
/// stream ([`super::buffer::EffectQuads`]).
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // one Bevy system's full input set
pub(super) fn simulate_particles(
    time: Res<Time>,
    tuning: Res<ParticleTuning>,
    // The draw-set gate's scene inputs — the far-clip wall (0678) and the exterior window test
    // (0786); see [`SceneGates`].
    gates: SceneGates,
    // The water-plane interleave inputs — see [`WaterInterleave`] and [`far_side_of_water`].
    interleave: WaterInterleave,
    mut commands: Commands,
    cam: Query<(Entity, &GlobalTransform, &Frustum, &Camera, &Projection), With<WorldCamera>>,
    // Owner reads (joints/units/roots — never emitter or child-draw entities): disjoint from
    // both `&mut GlobalTransform` queries below.
    transforms: Query<&GlobalTransform, (Without<ParticleEmitter>, Without<ChildDraw>)>,
    // MODEL-particle instance entities ([`super::model`]): the draw-set gate hides them with
    // their frozen emitter.
    mut child_draws: Query<
        (&mut Transform, &mut GlobalTransform, &mut Visibility),
        (
            With<ChildDraw>,
            Without<ParticleEmitter>,
            Without<WorldCamera>,
        ),
    >,
    images: Res<Assets<Image>>,
    mut quads: ResMut<EffectQuads>,
    // The ground-snap probe (file 0x2000 births): terrain + WMO/doodad geometry, the walking
    // collision audience.
    spatial: SpatialQuery,
    // The per-model render alpha an entity-owned cloud is multiplied by (decision 0827), composed
    // along the attached-model chain (0833).
    model_alphas: crate::model_fade::ModelAlphas,
    // `Without<WorldCamera>`: the `&mut GlobalTransform` must be provably disjoint from the
    // camera read above too (an emitter never rides the camera entity).
    mut emitters: Query<
        (
            Entity,
            &mut ParticleEmitter,
            &mut Transform,
            &mut GlobalTransform,
            Option<&EmitterFade>,
            Option<&RenderLayers>,
            Option<&EffectLightOverride>,
        ),
        Without<WorldCamera>,
    >,
    // Booth cameras (decision 0539 §5): a booth-layered emitter (the glue scenes' braziers)
    // billboards against ITS camera, matched by layer intersection — the `face_booth_billboards`
    // rule. `Without<ParticleEmitter>`/`Without<ChildDraw>` keep the read provably disjoint from
    // the `&mut GlobalTransform` writes above.
    booth_cams: Query<
        (Entity, &GlobalTransform, &RenderLayers),
        (
            With<Camera3d>,
            Without<WorldCamera>,
            Without<ParticleEmitter>,
            Without<ChildDraw>,
        ),
    >,
    // Hosted emitters' sequence source (the unit/GameObject lane): the host root's live
    // `AnimationPlayer` names the playing sequence + clip time, same resolution as the
    // material-alpha sampler (`doodad_anim::playing_seq`).
    hosts: Query<(&AnimationPlayer, &benilla_assets::ModelAnimations)>,
    // The lane's two debug instruments' plumbing, sharing one parameter (see [`super::dumps`]):
    // `$WOW_PARTICLE_DEPTHDUMP` and `$WOW_EMIT_DUMP`. Both inert without their env.
    mut dumps: super::dumps::Dumps,
) {
    let Ok((world_cam, cam_tf, frustum, camera, projection)) = cam.single() else {
        return;
    };
    // Clamp dt so a load hitch doesn't fling every particle out of existence in one step.
    let dt = time.delta_secs().min(0.1);
    if dt <= 0.0 {
        return;
    }
    let cam_pos = cam_tf.translation();
    let density = tuning.density.clamp(0.25, 1.0);
    let snap_filter = crate::collision::WorldCollision::body_filter();
    let (_, cam_rot, _) = cam_tf.to_scale_rotation_translation();
    let cam_right = cam_rot * Vec3::X;
    let cam_up = cam_rot * Vec3::Y;
    // The far-clip wall's axis — the SAME forward `debug_panel::visibility` measures the owner
    // doodad's own cull along, so an emitter and its owner cross the wall together.
    let cam_fwd = Vec3::from(cam_tf.forward());
    // The exterior gate, built once for the whole emitter walk — the same value the model
    // visibility authority and `exterior_cull` ask (0784/0786: one spelling of the window test).
    let exterior_gate = crate::exterior_cull::ExteriorGate::build(
        &gates.exterior_windows,
        Some((cam_tf, projection)),
    );
    let camera_instance = gates.camera_claim.0.map(|c| c.room.instance);
    // `$WOW_PARTICLE_DEPTHDUMP` (B16): is this a dump frame? Decided once per run.
    let dump_frame = dumps.depth_frame(time.elapsed_secs());
    // `$WOW_EMIT_DUMP`: is this a dump tick? Decided once per frame, for the whole walk.
    let emit_dump = dumps.emit.due(time.elapsed_secs());

    for (entity, mut emitter, mut entity_tf, mut entity_global, fade, layers, light_override) in
        &mut emitters
    {
        // The camera this emitter's quads face + its emission-LOD distance origin — and, in the
        // shared lane, the view its draw record targets: a booth-layered emitter uses its
        // booth's camera (decision 0539 §5 — the glue scenes' braziers, which the WORLD camera
        // would billboard sideways and LOD from a nonsense distance); everything else the world
        // camera.
        let booth = layers
            .filter(|l| !l.intersects(&RenderLayers::default()))
            .and_then(|l| booth_cams.iter().find(|(_, _, cl)| cl.intersects(l)));
        let is_booth = booth.is_some();
        let (draw_cam, e_cam_pos, e_right, e_up) = match booth {
            Some((cam_entity, tf, _)) => {
                let (_, rot, _) = tf.to_scale_rotation_translation();
                (cam_entity, tf.translation(), rot * Vec3::X, rot * Vec3::Y)
            }
            None => (world_cam, cam_pos, cam_right, cam_up),
        };
        // Draw-set gate (byte-verified, decision 0171): the reference ticks an emitter only when
        // its owner doodad is in the frame's scene worklist — admission = the frustum sphere test
        // + the radius-tiered distance-fade cutoff (`FUN_00683f80`; alpha ≤ 0 is never inserted).
        // A culled owner's emitter neither simulates nor draws: pool + age FROZEN
        // (`[obj+0x88]` accumulates nothing while absent), and on re-entry it resumes from frozen
        // state with one frame's dt — no catch-up, no refill. All three tests use the owner's fade
        // sphere, matching the reference's `[rec+0x68]`.
        //
        // The **far-clip** term is the third test and it is load-bearing (decision 0678, bug B39):
        // the reference's frustum is bounded by its projection far plane at `farclip`, ours is not
        // — our projection far is ~3000 yd so the WDL horizon can draw behind the wall, and
        // `intersects_sphere`'s far-plane term would therefore bound at 3000, not 777. Worse, the
        // fade cutoff alone bounds NOTHING for a big owner: `doodad_fade_alpha` returns a flat 1.0
        // above `NEVER_FADE_RADIUS` (7 yd) — trees, buildings, the large fire props. So every
        // big-doodad emitter used to simulate and draw at ANY distance out to the streaming
        // residency, long past the wall its own owner's mesh had already been culled by. That is
        // "all effects render at unlimited distance", and it is why the terrain under them was
        // already gone. `within_farclip` is the same rule the owner mesh uses in
        // `debug_panel::visibility` — shared, so the two can no longer drift apart.
        if let Some(f) = fade {
            let in_set = f.in_draw_set(
                cam_pos,
                cam_fwd,
                gates.view.farclip,
                // Lateral planes only (`intersect_far = false`) — the depth bound is the farclip
                // term inside `in_draw_set`, deliberately; see `view::within_farclip`.
                frustum.intersects_sphere(
                    &CullSphere {
                        center: f.center.into(),
                        radius: f.radius,
                    },
                    false,
                ),
                // …and the exterior-window term (0786): standing in a WMO interior, a doodad
                // outside is not in the worklist at all, so its emitter neither ticks nor draws —
                // which the mesh gate already knew and this one did not.
                f.exterior_admitted(&exterior_gate, camera_instance),
            );
            if !in_set {
                // Frozen: the pool writes no quads this frame (the shared stream is cleared
                // per frame, so "not pushed" IS "not drawn"); model-instance entities mirror
                // the gate on its edge, as the old entity-visibility flip did.
                if !emitter.gated {
                    emitter.gated = true;
                    for slot in &emitter.model_instances {
                        for (e, _) in &slot.meshes {
                            if let Ok((_, _, mut cv)) = child_draws.get_mut(*e) {
                                *cv = Visibility::Hidden;
                            }
                        }
                    }
                }
                continue;
            }
            emitter.gated = false;
        } else if !is_booth && !emitter.draining {
            // ENTITY-owned emitters (creatures, GameObjects, spell kits, WMO-prop deck lanterns)
            // carry no `EmitterFade`, and the premise that excused them — "the population is
            // bounded by server visibility instead" (`entities::wmo_props`) — is FALSE for
            // transports, which vmangos streams map-wide. Measured (decision 0678): parked in
            // Durotar, **56 emitters** were ticking and drawing at 4853–7080 yd — the deck
            // lanterns of the Teldrassil↔Auberdine boat (`transports` 176244 "Moonspray") and its
            // Menethil↔Auberdine neighbour, five to seven kilometres away. So the wall is applied
            // to EVERY world-lane emitter, not just the faded ones. That is also the faithful
            // shape: the reference ticks particles inside the owning model's animate step, which
            // runs only for models the frame actually draws — and past `farclip` nothing is drawn.
            //
            // Three deliberate exclusions, each of which would be a bug the other way:
            // - **booth-layered** emitters (glue/portrait scenes) — parked thousands of yards away
            //   and drawn by their OWN camera, so the world wall says nothing about them;
            //   `glue_booth` states the contract outright ("a glue scene always ticks").
            // - **draining** emitters — freezing one strands it: it can never empty its pool, so it
            //   never reaches the self-despawn below and leaks for the session.
            // - an emitter whose **owner has vanished** — `subject` is `None`, so it falls through
            //   to the drain path below rather than freezing before that can be noticed.
            //
            // The subject is read LIVE from the owner's transform, never from the stored anchor: a
            // frozen emitter stops refreshing its anchor, so gating on the stored one would latch a
            // moving owner out of existence the first time it crossed the wall.
            let subject = match emitter.owner {
                Some(o) => transforms.get(o).ok().map(|gt| gt.translation()),
                None => Some(emitter.anchor_pos),
            };
            if let Some(at) = subject {
                if !crate::view::within_farclip(gates.view.farclip, cam_pos, cam_fwd, at, 0.0) {
                    if !emitter.gated {
                        emitter.gated = true;
                        for slot in &emitter.model_instances {
                            for (e, _) in &slot.meshes {
                                if let Ok((_, _, mut cv)) = child_draws.get_mut(*e) {
                                    *cv = Visibility::Hidden;
                                }
                            }
                        }
                    }
                    continue;
                }
                emitter.gated = false;
            }
        }
        let ParticleEmitter {
            def,
            placement,
            owner,
            on_owner_loss,
            draining,
            attach,
            attach_rot,
            alpha_src,
            alpha,
            anchor,
            world_composed,
            anchor_pos,
            particles,
            accumulator,
            emitter_prev,
            inherit_accum,
            inherit_vel,
            gate_prev,
            age,
            host,
            seq,
            rng,
            owner_reach,
            water_bound,
            texture,
            recursion: _,
            children,
            geometry: _,
            model_instances,
            gated: _,
        } = &mut *emitter;
        // The water-interleave MODEL frame, captured before the draw-anchor local shadows the
        // `anchor` field below: the cloud anchor is "the MODEL, never the bone" — its transform
        // is the instance matrix the classification dots (`model_far_side`), and the room walk
        // starts there too. The owner (possibly a JOINT) only seconds it as the walk seed when
        // no anchor exists — a joint's transform is not the instance matrix, so that leg keeps
        // the sign-test fallback (`water_gt = None`).
        let water_model = (*anchor).or(*owner);
        let water_gt = (*anchor).and_then(|e| transforms.get(e).ok().copied());
        let water_bound = *water_bound;
        *age += dt;
        // Anchored mode (see [`Particle`]): positions are emitter-relative, so tracking a moving
        // owner needs nothing beyond refreshing `placement` — the cloud rides the anchor for free
        // (the reference's per-frame `translate(−emitterPos)` draw-matrix rebuild).
        let anchored = !def.model_space();

        // 0. If this emitter follows a streamed owner (creature/GameObject/joint), track its world
        //    transform — or DRAIN once the owner is gone (missile impacted, effect reaped, unit
        //    streamed out): the pool finishes its lifespans at the frozen placement, then the
        //    emitter despawns itself. An instant despawn here popped every live particle of a
        //    fireball's trail at the moment of impact.
        if let Some(o) = *owner {
            match transforms.get(o) {
                Ok(gt) => *placement = gt.compute_transform(),
                Err(_) => {
                    *owner = None;
                    match *on_owner_loss {
                        // The owner MODEL is gone, so its effects go with it — the reference frees
                        // a model's emitters at its dtor (decision 0826). Nothing to drain: an
                        // unequipped torch's flame does not stay behind in the air.
                        OwnerLoss::Free => {
                            for slot in model_instances.iter() {
                                for (e, _) in &slot.meshes {
                                    commands.entity(*e).despawn();
                                }
                            }
                            commands.entity(entity).despawn();
                            continue;
                        }
                        OwnerLoss::Drain => {
                            *draining = true;
                            // The ghost-cloud event, named so a run can count it: this pool now
                            // lives out its lifespans FROZEN in world space. Right for an effect
                            // that ended in place (a missile impact), and the reason `Free` exists
                            // for everything that is a model teardown.
                            if !particles.is_empty() {
                                debug!(
                                    "fx orphan: emitter {entity} ({}) lost its owner with {} \
                                     live particles frozen at {:?}",
                                    texture
                                        .path()
                                        .map_or_else(|| "<no path>".into(), |p| p.to_string()),
                                    particles.len(),
                                    *anchor_pos
                                );
                            }
                        }
                    }
                }
            }
        }
        if *draining && particles.is_empty() && children.iter().all(|c| c.particles.is_empty()) {
            for slot in model_instances.iter() {
                for (e, _) in &slot.meshes {
                    commands.entity(*e).despawn();
                }
            }
            commands.entity(entity).despawn();
            continue;
        }
        // The live attach rotation `A(t)` (attached models only — see the field doc). A vanished
        // attach entity keeps the last rotation: the pool drains in its final frame.
        if let Some(a) = *attach {
            if let Ok(gt) = transforms.get(a) {
                let (_, rot, _) = gt.to_scale_rotation_translation();
                *attach_rot = rot;
            }
        }
        // The MODEL's render alpha for this frame (decision 0827 — the reference's per-frame
        // `emitter+0x1a8 = Model+0x19c` copy at `0x718960` @`0x719073`). Two disjoint sources, the
        // same slot the reference writes both through: an entity-owned cloud takes its OWN model
        // instance's composed alpha ([`crate::model_fade::ModelAlphas`] walks the attached-model
        // chain, so a weapon glow's reaches the item's and the item's the wearer's — 0833), and a
        // placed doodad's takes its own distance fade, whose cutoff the draw-set gate above
        // already applies as a hard stop.
        *alpha = alpha_src.map_or(1.0, |e| model_alphas.get(e))
            * fade.map_or(1.0, |f| f.distance_alpha(cam_pos));
        let attach_inv = attach_rot.inverse();
        // The cloud anchor (see the field doc): the model's live translation, or the last-known
        // one while the pool drains. A whole-model owner keeps anchor == owner — identical math.
        match *anchor {
            Some(a) => {
                if let Ok(gt) = transforms.get(a) {
                    *anchor_pos = gt.translation();
                }
            }
            None if owner.is_none() => *anchor_pos = placement.translation,
            None => {} // joint-owned, unanchored (placed doodads): the spawn placement stands
        }
        // `$WOW_EMIT_DUMP`'s subject: the model this cloud belongs to, captured here because
        // `anchor` is shadowed by the draw anchor further down.
        let dump_owner = (*host).or(*anchor);

        // 0b. The EMITTER-MOTION terms (wow-re `part-emitter-motion.md`, byte-verified): both
        //     feed off the emitter origin's one-frame live world Δ. prevPos refreshes EVERY
        //     frame (the reference's rt+0x248 @`0x7b5265`), so even a multi-frame inherit
        //     window measures a single frame's motion. Consumption folds the world vector into
        //     the particles' STORED frame exactly like a birth velocity: attach-local for
        //     anchored mode; for model mode the reference adds the raw world vector to LOCAL
        //     coords (a frame quirk) — we fold it into the local frame instead, since our
        //     world axes are Bevy's, not WoW's (translation-dominant content is equivalent).
        let emitter_world = placement.transform_point(wow_to_bevy(def.position));
        let emitter_delta = emitter_prev.map_or(Vec3::ZERO, |prev| emitter_world - prev);
        *emitter_prev = Some(emitter_world);
        // NOTE on the R(+Z,90°) emitter-frame law (`emit_local`'s tail): we apply R at EMISSION,
        // so stored vectors are already post-R and every stored↔world fold here stays R-free —
        // unlike the reference, which stores pre-R and folds R inside its draw matrix. The two
        // compositions are equivalent; ours keeps a single application point.
        let to_stored = |world: Vec3, attach_inv: Quat, placement: &Transform| {
            if anchored {
                attach_inv * world
            } else {
                Vec3::from(bevy_to_wow(
                    (placement.rotation.inverse() * world) / placement.scale.max(Vec3::splat(1e-6)),
                ))
            }
        };
        // The per-frame world-motion carry ([`world_motion_kept`]), folded into the stored frame:
        // keeping `fraction` of Δ over our anchor-RIDING storage is a `(fraction − 1)·Δ` move on
        // every live particle after its first integrate (the fresh-bit skip).
        let follow = if emitter_delta == Vec3::ZERO {
            Vec3::ZERO
        } else {
            let fraction = world_motion_kept(def, *world_composed, emitter_delta.length() / dt);
            if fraction >= 1.0 {
                Vec3::ZERO // the rigid ride — the overwhelming majority, kept free
            } else {
                to_stored((fraction - 1.0) * emitter_delta, attach_inv, placement)
            }
        };
        // VELOCITY INHERIT (file 0x40): the ~30 Hz trigger holds the inherit velocity births
        // read; the live gate (rt+0x64) zeroes it while nothing is live.
        if def.inherits_emitter_motion() {
            inherit_trigger(
                inherit_accum,
                inherit_vel,
                dt,
                emitter_delta,
                !particles.is_empty(),
                def.inherit_scale,
            );
        }

        // The sequence clock (`EmitTiming`, decision 0641's structure one channel over): a HOSTED
        // emitter reads its host's live playing sequence + clip time each frame — the reference's
        // `m2_animate` emitter phase samples the CURRENT sequence record, which is why a quest
        // GameObject's explosion fires in its one-shot clips and reads OFF in every idle window
        // (bug B27). A host with no live player yet keeps its last slot at that slot's opening
        // pose. Pinned lanes (doodads, effect rigs, booths) run their slot on the spawn-age
        // clock; the baked loops wrap a looping band and end-hold a clamped one.
        let (clock_seq, elapsed_s) = match *host {
            Some(h) => match hosts
                .get(h)
                .ok()
                .and_then(|(p, a)| crate::doodad_anim::playing_seq(p, a))
            {
                Some((s, t)) => {
                    *seq = Some(s);
                    (Some(s), t)
                }
                None => (*seq, 0.0),
            },
            None => (*seq, *age),
        };
        // This frame's emitter PARAMETERS — the nine per-frame-sampled channels, on the same
        // clock as the rate track (the reference's `m2_animate` emitter phase samples all ten;
        // wow-re `part-emission-rate-animated.md` §1). Frost Nova rides its emission radius
        // 0.19 → 13.2 yd out with the ring; Arcane Explosion 0 → 7.2 yd with the dome — births
        // MUST read the frame's values, not `value[0]` (decision 0844).
        // The instance's gseq cursor (0856/0858): the spawn age IS `sceneNow − attach` — the
        // emitter spawns with its instance, and every lane's instance is fresh per play.
        let gseq_now = f64::from(*age);
        let now = def.params.sample(clock_seq, elapsed_s, gseq_now);

        // 1. Age + integrate the live pool. The verified vanilla integrator (`particle_integrate`
        //    @ 0x7b2680): pos += dt·v with the gravity term on the UP axis (up += dt·v_up − ½·g·dt²;
        //    v_up −= g·dt), then **drag**: v −= min(dt·drag, 1)·v (exponential velocity decay,
        //    applied after gravity). Drag is load-bearing — a fast, long-lived, zero-gravity jet
        //    (e.g. the CandelabraTallWall flame: speed 0.56, life 6, g 0, drag 10) relies on it to
        //    stay a ~0.06 yd flicker; without it the particle coasts 3.3 yd to the ceiling. The
        //    frame decides the up axis: WoW +Z in model mode, Bevy +Y in world mode (the math is
        //    frame-independent otherwise — the reference integrates world-space particles with the
        //    same kernel). Kill at age ≥ the particle's birth-sampled life. Gravity is a LIVE
        //    per-frame emitter field (the integrator reads it each frame in the reference).
        let g = now.gravity;
        let drag = def.drag;
        // Sphere KILL-OUTBOUND emitters: the emitter origin in the stored frame — `def.position`
        // composed exactly like a birth (anchored: the live placement/attach fold; model mode:
        // raw local). Every corpus author converges inward (negative speed); this is what stops
        // the stream at the centre instead of spraying it out the far side.
        let kill_origin = def.kill_outbound().then(|| {
            if anchored {
                attach_inv
                    * (placement.translation - *anchor_pos
                        + placement.rotation * (placement.scale * wow_to_bevy(def.position)))
            } else {
                Vec3::from(def.position)
            }
        });
        let env = StepEnv {
            dt,
            gravity: g,
            drag,
            anchored,
            kill_origin,
            follow,
        };
        particles.retain_mut(|p| integrate_particle(p, &env));

        // 2. Emit new particles — the owed-birth count comes from [`accumulate_emission`]
        //    (continuous `rate·dt` pour, or a BURST emitter's one rising-edge puff — the emission
        //    model split of wow-re `part-emission-burst-flag.md`). The rate is the keyed track
        //    STEP-sampled at the emitter's clip clock — how a one-shot effect's emitters (rate
        //    `0 → 200 → 0` over the first 133 ms, the blood spurt's starflash) fire at their
        //    authored moment; constant ambient tracks are unaffected. Floored at 0 (a track tail
        //    may legitimately go negative). Birth position/velocity come from the shape kernel
        //    ([`emit_local`], wow-re `part-shape-kernels.md`).
        //    Within the draw set the reference has NO particle-side distance cull or fade (wow-re
        //    `part-distance-density.md`; population is bounded by the OWNER draw-set gate above —
        //    decision 0171). Its one distance mechanism is this emission LOD (`0x7b5550`,
        //    byte-verified): spawn count × clamp(1 − (camDist − 50)·0.02, 0.25, 1.0) — full rate
        //    inside 50 yd, linear falloff, a 25% floor from 87.5 yd out, never zero — and × the
        //    `particleDensity` CVar.
        //    The enabled M2Track (file +0x1dc) gates NEW emission on the same clock — the
        //    one-shot effects' choreography (a 200 ms hand flash inside a 1.0 s clip; the impact
        //    model's six staggered windows). Live particles are untouched — they finish their
        //    lifespans exactly like the drain above.
        let emitting = !*draining && def.timing.emitting(clock_seq, elapsed_s, gseq_now);
        let rate = def.timing.rate(clock_seq, elapsed_s, gseq_now);
        let dist_lod =
            (1.0 - (placement.translation.distance(e_cam_pos) - 50.0) * 0.02).clamp(0.25, 1.0);
        let burst = accumulate_emission(
            def.burst(),
            rate,
            emitting,
            density * dist_lod,
            dt,
            accumulator,
            gate_prev,
        );
        if burst > 0.0 && benilla_assets::trace::enabled() {
            benilla_assets::trace::line("fx", &format!("burst n={burst} t={elapsed_s:.2}s"));
        }
        while *accumulator >= 1.0 && particles.len() < MAX_PARTICLES {
            *accumulator -= 1.0;
            let (base, dir) = emit_local(def, &now, rng);
            let speed =
                now.emission_speed * (1.0 + now.speed_variation * (rand01(rng) * 2.0 - 1.0));
            // Anchored mode bakes the emitter's ROTATION + scale at birth (the reference's
            // `0x7bca80`/`0x7bcb40` birth transforms) but stores relative to its position — the
            // per-frame anchor supplies the translation at render; on an attached model the
            // birth additionally divides out the live attach rotation (the reference's
            // `worldMx = palette·view⁻¹·A⁻¹`, CLEAR mode), stored attach-local. Model mode
            // stores raw local.
            let (mut pos, vel) = if anchored {
                (
                    attach_inv
                        * (placement.translation - *anchor_pos
                            + placement.rotation
                                * (placement.scale * wow_to_bevy(base.to_array()))),
                    attach_inv
                        * (placement.rotation
                            * (placement.scale * wow_to_bevy((dir * speed).to_array()))),
                )
            } else {
                (base, dir * speed)
            };
            // GROUND SNAP (file 0x2000, [`benilla_formats::ParticleEmitterDef::ground_snap`]):
            // at spawn only, anchored mode only, probe 20 yd straight down against
            // terrain/WMO/doodad geometry (the walking collision audience — the reference's
            // 0x100111 flag set); on a hit the particle stands ON the surface, lifted by its
            // birth over-life SIZE. A miss leaves the spawn position untouched.
            if anchored && def.ground_snap() {
                let world = *anchor_pos + *attach_rot * pos;
                if let Some(hit) = spatial.cast_ray(world, Dir3::NEG_Y, 20.0, true, &snap_filter) {
                    let lifted = world.y - hit.distance + def.over_life.sample(0.0).size;
                    pos = attach_inv * (Vec3::new(world.x, lifted, world.z) - *anchor_pos);
                }
            }
            // VELOCITY INHERIT consumption (the shape kernels' closing block, gated on the
            // flag): `vel += (1 + S11·speedVariation) · inherit`, its own S11 draw, in the
            // stored frame (the block runs after the reference's space fold).
            let vel = if def.inherits_emitter_motion() && *inherit_vel != Vec3::ZERO {
                vel + (1.0 + now.speed_variation * rand_s11(rng))
                    * to_stored(*inherit_vel, attach_inv, placement)
            } else {
                vel
            };
            // MODEL PARTICLES (wow-re `part-model-particles.md` §b): seed the instance
            // orientation from the birth fold (the reference's transposed spawn-basis
            // mat3→quat — the transpose is the row/column-major convention fold; net, the
            // basis rotation) and roll the tumble with the VERIFIED asymmetry: only X honors
            // `min + u·range`, Y/Z multiply a raw [1,2) mantissa by their range alone (their
            // authored min is dead — a faithful original-client quirk). Flag 0x200 sign-flips
            // each axis independently.
            let (quat, angvel) = if def.geometry_model.is_some() {
                let amin = def.angular_velocity_min;
                let amax = def.angular_velocity_max;
                let mut w = [
                    amin[0] + rand01(rng) * (amax[0] - amin[0]),
                    (1.0 + rand01(rng)) * (amax[1] - amin[1]),
                    (1.0 + rand01(rng)) * (amax[2] - amin[2]),
                ];
                if def.tumble_random_sign() {
                    for a in &mut w {
                        if next_u32(rng) & 1 == 0 {
                            *a = -*a;
                        }
                    }
                }
                // The seed basis is the R(+Z,90°)-prepended spawn matrix (`emit_local`'s law
                // note): WoW local +Ẑ is Bevy +Ŷ under the 0002 map, so R rides as a +90°
                // yaw appended to the frame rotation in both modes.
                let r90 = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
                let quat = if anchored {
                    attach_inv * placement.rotation * r90
                } else {
                    r90
                };
                (quat, wow_to_bevy(w))
            } else {
                (Quat::IDENTITY, Vec3::ZERO)
            };
            let phase = next_u32(rng);
            particles.push(Particle {
                pos,
                vel,
                age: 0.0,
                life: now.lifespan,
                phase,
                fresh: true,
                quat,
                angvel,
            });
        }

        // 2b. CHILD emitters (wow-re `part-child-recursion.md`): each drives off the live pool
        //     — post-integration, post-birth, exactly the window the reference's per-particle
        //     child loop sees — with the child's OWN rate/enabled tracks on its own model's
        //     slot-0 clock (a recursion model has no host of its own; the parent's age drives).
        //     A draining parent stops driving; child pools live out their spans (the despawn
        //     above waits for them).
        for child in children.iter_mut() {
            // A child cloud is its own fresh instance: its gseq cursor is its own age (0856).
            let c_emitting = !*draining && child.def.timing.emitting(None, *age, f64::from(*age));
            let c_rate = child.def.timing.rate(None, *age, f64::from(*age));
            let c_now = child.def.params.sample(None, *age, f64::from(*age));
            drive_child(
                child,
                &c_now,
                particles,
                c_rate,
                c_emitting,
                density * dist_lod,
                dt,
                anchored,
                attach_inv,
                placement,
            );
            let c_env = StepEnv {
                dt,
                gravity: c_now.gravity,
                drag: child.def.drag,
                anchored,
                kill_origin: None,
                follow: Vec3::ZERO,
            };
            child
                .particles
                .retain_mut(|p| integrate_particle(p, &c_env));
        }

        // 3. Expand each pool into the shared effect-quad stream — per pool, only once its
        //    texture is resident. While one streams (or failed to decode), the pool pushes
        //    nothing rather than flash the engine fallback through the additive blend. An idle
        //    pool pushes nothing and commits nothing — the old "don't rewrite an already-empty
        //    mesh" guard is now the structure itself.
        // The cloud's anchor: the emitter's live position — the draw record's SORT point (the
        // transparent-phase depth this cloud takes; see `super::buffer`). Still published to
        // the entity transform: the census probe and phase instruments read the anchor there.
        let anchor = if anchored {
            *anchor_pos
        } else {
            placement.translation
        };
        entity_tf.translation = anchor;
        // Post-propagation frame (we're post-palette in PostUpdate) — publish directly so any
        // same-frame reader sees THIS frame's anchor (the `face_billboards` exactness rule;
        // emitter entities live at the world root, so the direct write is exact).
        *entity_global = GlobalTransform::from(*entity_tf);
        let frame = DrawFrame {
            anchored,
            anchor,
            attach_rot: *attach_rot,
            alpha: *alpha,
        };
        let cam = CamBasis {
            right: e_right,
            up: e_up,
        };
        // A GEOMETRY (model-particle) emitter never draws quads — the reference's render
        // dispatch skips the whole quad path when the model mode is on; its instances
        // render via [`super::model::update_model_particles`].
        let want_quads = def.geometry_model.is_none() && images.contains(&*texture);
        // Every effect of one model takes the SAME rung (0719/0721) — computed here per frame,
        // where it used to be baked into the material's `depth_bias` at spawn. Plus the
        // water-plane interleave ([`far_side_of_water`]): a cloud on the eye's far side of its
        // local water plane drops under the water pass, so the surface paints over it — the
        // swimming-enchant erase was these two sorting by raw view-z and flipping with camera
        // angle. Booth emitters belong to their own camera and take no world rung.
        // Classified at the MODEL — its transformed bound centre with the bound-radius slack —
        // never at the emitter's live position: the reference dots the plane once per model in
        // the walk prologue and every emitter reads that verdict (byte-VERIFIED, 0921 correcting
        // 0911's per-emitter reading of `0x7084a0`). This is what makes a shoulder flame stable
        // at the waterline: the bobbing bone doesn't enter, and the whole cloud only flips when
        // the model's centre is a full radius under.
        let far = !is_booth
            && model_far_side(
                &interleave,
                water_model,
                water_gt.as_ref(),
                water_bound,
                placement.translation,
            );
        // The classification's own trace (`WOW_MOVE_TRACE_TAGS=fx`): which clouds dropped under
        // the water pass this frame — the numeric read for "the swimmer's enchant survives the
        // surface", where a pixel can't say which side a draw sorted to.
        if far && !particles.is_empty() && benilla_assets::trace::enabled() {
            benilla_assets::trace::line(
                "fx",
                &format!(
                    "far-side cloud at=[{:.1},{:.1},{:.1}] n={}",
                    placement.translation.x,
                    placement.translation.y,
                    placement.translation.z,
                    particles.len()
                ),
            );
        }
        let bias = super::owner_last_bias(*owner_reach)
            + if far {
                crate::sky_order::FAR_SIDE_BIAS
            } else {
                0.0
            };
        let start = quads.begin();
        if want_quads && !particles.is_empty() {
            expand_quads(def, particles, &frame, placement, &cam, &mut quads.verts);
        }
        // `$WOW_PARTICLE_DEPTHDUMP` (B16): the depth numbers this pool brings to the compare —
        // now over the quads THIS frame just wrote (the exact vertices the draw will consume).
        // Booth emitters are skipped — their pixels belong to a booth camera's target, not the
        // world depth buffer `WOW_DEPTH` reads.
        if let Some(fidx) = dump_frame {
            if !is_booth
                && def.geometry_model.is_none()
                && !particles.is_empty()
                && super::depthdump::bone_selected(u32::from(def.bone))
            {
                super::depthdump::dump_emitter(
                    fidx,
                    def,
                    particles,
                    &frame,
                    placement,
                    emitter_world,
                    &cam,
                    cam_tf,
                    camera,
                    projection,
                    images.contains(&*texture),
                    &quads.verts[start as usize..],
                );
            }
        }
        // `$WOW_EMIT_DUMP`: what this emitter's front end decided this frame — the resolved
        // sequence slot and what the ten tracks sampled at it. Placed here so `live` is the
        // count AFTER this frame's births, i.e. what the draw below actually consumes.
        if emit_dump {
            dumps.emit.dump(
                dump_owner,
                &super::emitdump::Decision {
                    def,
                    seq: clock_seq,
                    elapsed: elapsed_s,
                    rate,
                    emitting,
                    live: particles.len(),
                    now: &now,
                    at: emitter_world,
                },
            );
        }
        quads.commit_quads(
            start,
            EffectDrawSpec {
                cam: draw_cam,
                texture: texture.id(),
                blend: def.blend.into(),
                fog: EffectFog::for_blend(def.flags, def.blend),
                lit: def.lit,
                anchor,
                bias,
                raster_bias: 0,
                cam_relative: false,
                main_entity: entity,
                light: light_override.map(|l| l.0.clone()),
            },
        );
        // CHILD pools: their own texture/blend/fog identity, the PARENT's anchor and rung
        // (a child draws at the parent's anchor, and it is the parent's owner it must clear —
        // `ParticleEmitter::owner_reach`'s doc).
        for child in children.iter() {
            if child.particles.is_empty() || !images.contains(&child.texture) {
                continue;
            }
            let cstart = quads.begin();
            expand_quads(
                &child.def,
                &child.particles,
                &frame,
                placement,
                &cam,
                &mut quads.verts,
            );
            quads.commit_quads(
                cstart,
                EffectDrawSpec {
                    cam: draw_cam,
                    texture: child.texture.id(),
                    blend: child.def.blend.into(),
                    fog: EffectFog::for_blend(child.def.flags, child.def.blend),
                    // A child emitter is a whole emitter record of the recursion model, with its
                    // own flag word and blend field — so it takes its OWN lighting verdict, never
                    // the parent's (the same rule its texture/blend/fog identity follows above).
                    lit: child.def.lit,
                    anchor,
                    bias,
                    raster_bias: 0,
                    cam_relative: false,
                    main_entity: entity,
                    light: light_override.map(|l| l.0.clone()),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        inherit_trigger, integrate_particle, is_above, world_motion_kept, ChildEmitter, Particle,
        StepEnv, Vec3,
    };
    use bevy::prelude::{Quat, Transform};

    /// The membership law (`0x7084cf`): `above ⇔ d ≥ −r` — the tie at `d == −r` lands ABOVE
    /// (the reference's `jp` keeps it), NaN lands BELOW (unordered → the below push), and with
    /// no slack (the mesh lane's r = 0) it degenerates to the sign test with `d = 0` above —
    /// the same tie side as [`crate::player::camera`]'s waterline. The eye then picks which
    /// list is FAR (`0x4836d6`): dry ⇒ below, submerged ⇒ above — exercised at the callers.
    #[test]
    fn membership_is_d_at_least_minus_r() {
        // The slack: a model bobbing within its own radius of the plane stays ABOVE — the
        // shoulder flame's stability at the waterline (0921).
        assert!(is_above(-0.5, 2.0));
        assert!(is_above(-2.0, 2.0), "tie at d == -r lands above");
        assert!(!is_above(-2.1, 2.0));
        // r = 0 (the mesh lane): the sign test, 0 above.
        assert!(is_above(0.0, 0.0));
        assert!(!is_above(-0.1, 0.0));
        // NaN → below, the reference's unordered-compare branch — `>=` gives it for free.
        assert!(!is_above(f32::NAN, 2.0));
    }

    fn particle(pos: Vec3, vel: Vec3) -> Particle {
        Particle {
            pos,
            vel,
            age: 0.0,
            life: 10.0,
            phase: 0,
            fresh: false,
            quat: Quat::IDENTITY,
            angvel: Vec3::ZERO,
        }
    }

    fn env(kill_origin: Option<Vec3>, follow: Vec3) -> StepEnv {
        StepEnv {
            dt: 0.1,
            gravity: 0.0,
            drag: 0.0,
            anchored: true,
            kill_origin,
            follow,
        }
    }

    /// The sphere KILL-OUTBOUND tail test (`0x7b2680`, rt 0x800): an inward particle lives while
    /// converging, dies the frame its motion turns away from the origin (after the crossing) —
    /// and the test uses the PRE-drag step velocity against the UPDATED position, in that byte
    /// order. Without a kill origin the same particle just integrates.
    #[test]
    fn kill_outbound_dies_at_the_centre_crossing() {
        let origin = Vec3::new(1.0, 2.0, 3.0);
        // Inward at 2 yd/s, 0.5 yd out: crosses at t = 0.25 s.
        let mut p = particle(origin + Vec3::X * 0.5, -Vec3::X * 2.0);
        let kill = env(Some(origin), Vec3::ZERO);
        assert!(integrate_particle(&mut p, &kill), "0.3 yd out, inbound");
        assert!(integrate_particle(&mut p, &kill), "0.1 yd out, inbound");
        assert!(
            !integrate_particle(&mut p, &kill),
            "crossed to −0.1 yd: motion now points away — dead"
        );
        // Same trajectory, no kill origin: sails straight through.
        let free_env = env(None, Vec3::ZERO);
        let mut free = particle(origin + Vec3::X * 0.5, -Vec3::X * 2.0);
        for _ in 0..5 {
            assert!(integrate_particle(&mut free, &free_env));
        }
        // An OUTBOUND particle on a kill emitter dies on its first step (the authored content
        // never does this — every kill author emits inward).
        let mut out = particle(origin + Vec3::X * 0.5, Vec3::X);
        assert!(!integrate_particle(&mut out, &kill));
    }

    /// [`world_motion_kept`] — the ride-vs-trail law (0986). The host class sets the baseline;
    /// the follow flag overrides it with the authored response; a degenerate response falls back
    /// to the baseline.
    #[test]
    fn world_motion_kept_reads_the_host_class_then_the_follow_response() {
        let plain = crate::particles::tests::plain_def(); // no flags
        assert_eq!(
            world_motion_kept(&plain, false, 30.0),
            1.0,
            "scene-graph-carried, unflagged: the kobold's candle rides at any speed"
        );
        assert_eq!(
            world_motion_kept(&plain, true, 30.0),
            0.0,
            "a free world model, unflagged: Multi-Shot's flares hang where they were born"
        );
        // ArcaneShot's authored pair: 0.1 @ 2.5 yd/s, 0.9 @ 16.667 — the head glow catching up.
        let following = benilla_formats::ParticleEmitterDef {
            flags: 0x4000,
            follow_speed1: 2.5,
            follow_scale1: 0.1,
            follow_speed2: 16.667,
            follow_scale2: 0.9,
            ..crate::particles::tests::plain_def()
        };
        assert!(
            (world_motion_kept(&following, true, 2.5) - 0.1).abs() < 1e-3,
            "the flag overrides the world-frozen baseline with the authored response"
        );
        assert_eq!(
            world_motion_kept(&following, true, 40.0),
            1.0,
            "clamped at a rigid ride on a fast missile — it never leads"
        );
        // Equal authored speeds: the reference zeroes both, so nothing overrides the baseline.
        let degenerate = benilla_formats::ParticleEmitterDef {
            flags: 0x4000,
            follow_speed1: 4.0,
            follow_speed2: 4.0,
            ..crate::particles::tests::plain_def()
        };
        assert_eq!(world_motion_kept(&degenerate, true, 30.0), 0.0);
        assert_eq!(world_motion_kept(&degenerate, false, 30.0), 1.0);
    }

    /// FOLLOW-DELTA (`0x7b2680` @0x7b2744, rt 0x40000): the shared per-frame vector moves every
    /// live particle — except each particle's FIRST integrate, which consumes its fresh bit
    /// instead (the reference's particle+0xd first-frame skip).
    #[test]
    fn follow_delta_skips_only_the_first_integrate() {
        let following = env(None, Vec3::X * 0.5);
        let mut p = particle(Vec3::ZERO, Vec3::ZERO);
        p.fresh = true;
        assert!(integrate_particle(&mut p, &following));
        assert_eq!(
            p.pos,
            Vec3::ZERO,
            "first integrate: fresh bit eaten, no add"
        );
        assert!(integrate_particle(&mut p, &following));
        assert_eq!(
            p.pos,
            Vec3::X * 0.5,
            "second integrate: the shared delta applies"
        );
    }

    /// The VELOCITY-INHERIT trigger law (`0x7b5230` 0x7b53ce–0x7b54ca): nothing until the
    /// accumulator passes 1/30 s; at the trigger the held vector becomes
    /// `oneFrameΔ·((1/30)/accum)·scale` (with the ×1/30 the first sketch missed), zero when
    /// nothing is live; it HOLDS between triggers.
    #[test]
    fn inherit_trigger_fires_at_thirty_hertz_with_the_exact_factor() {
        let (mut accum, mut held) = (0.0, Vec3::ZERO);
        let delta = Vec3::X * 0.1; // one frame's emitter motion
        inherit_trigger(&mut accum, &mut held, 0.02, delta, true, 6.0);
        assert_eq!(
            held,
            Vec3::ZERO,
            "0.02 s accumulated: below the 1/30 window"
        );
        inherit_trigger(&mut accum, &mut held, 0.02, delta, true, 6.0);
        // accum 0.04 > 1/30: held = 0.1·((1/30)/0.04)·6 = 0.5 on x.
        assert!((held.x - 0.5).abs() < 1e-4, "the (1/30)/accum·scale factor");
        assert_eq!(accum, 0.0, "trigger resets the accumulator");
        // Between triggers the held value stands even as the emitter stops moving.
        inherit_trigger(&mut accum, &mut held, 0.02, Vec3::ZERO, true, 6.0);
        assert!((held.x - 0.5).abs() < 1e-4, "held between triggers");
        // A trigger with nothing live zeroes it (the rt+0x64 gate).
        inherit_trigger(&mut accum, &mut held, 0.04, delta, false, 6.0);
        assert_eq!(held, Vec3::ZERO);
    }

    /// The CHILD drive (`0x7b5b9f`): the child's accumulator receives one `rate·dt` per live
    /// parent particle (volume ∝ live count), births land at parent-particle positions, and a
    /// flag-0x40 child folds its parent particle's velocity into each birth. No parent
    /// particles ⇒ a child never emits (its only source is the per-particle calls).
    #[test]
    fn child_drive_scales_with_the_parent_pool() {
        let mut child = ChildEmitter::bare(benilla_formats::ParticleEmitterDef {
            flags: 0x40, // inherit the parent particle's velocity
            ..crate::particles::tests::plain_def()
        });
        child.def.timing = benilla_formats::EmitTiming::constant(100.0);
        // Point emitter, zero own speed: births land exactly ON the parent particle and the
        // whole birth velocity is the inherited term.
        let c_now = benilla_formats::ParamsNow {
            emission_speed: 0.0,
            area_length: 0.0,
            area_width: 0.0,
            ..crate::particles::emit::tests::now()
        };
        // Two live parents, far apart, distinct velocities.
        let parents = [
            particle(Vec3::X * 10.0, Vec3::Y * 3.0),
            particle(Vec3::X * -10.0, Vec3::Y * -3.0),
        ];
        // 100/s × 0.1 s × 2 calls = 20 births per frame.
        super::drive_child(
            &mut child,
            &c_now,
            &parents,
            100.0,
            true,
            1.0,
            0.1,
            true,
            Quat::IDENTITY,
            &Transform::IDENTITY,
        );
        assert_eq!(child.particles.len(), 20, "one rate·dt per live parent");
        for p in &child.particles {
            let at_a = (p.pos - Vec3::X * 10.0).length() < 1e-3;
            let at_b = (p.pos + Vec3::X * 10.0).length() < 1e-3;
            assert!(at_a || at_b, "born at a parent particle");
            let v = if at_a { Vec3::Y * 3.0 } else { Vec3::Y * -3.0 };
            // speed 0 ⇒ the whole birth velocity is the inherited (1+S11·0)·parentVel.
            assert!(
                (p.vel - v).length() < 1e-3,
                "inherits its parent's velocity"
            );
        }
        // An empty parent pool drives nothing.
        let n = child.particles.len();
        super::drive_child(
            &mut child,
            &c_now,
            &[],
            100.0,
            true,
            1.0,
            0.1,
            true,
            Quat::IDENTITY,
            &Transform::IDENTITY,
        );
        assert_eq!(child.particles.len(), n, "no parents, no births");
    }
}
