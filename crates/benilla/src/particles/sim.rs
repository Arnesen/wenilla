use avian3d::prelude::SpatialQuery;
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use bevy::camera::primitives::{Frustum, Sphere as CullSphere};
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;

use crate::player::WorldCamera;

use super::quads::{expand_quads, CamBasis, DrawFrame};
use super::{
    accumulate_emission, clear_particle_mesh, emit_local, next_u32, rand01, rand_s11, ChildDraw,
    ChildEmitter, EmitterFade, Particle, ParticleEmitter, ParticleTuning, MAX_PARTICLES,
};

/// The emitter-constant inputs of one integrator step ([`integrate_particle`]).
struct StepEnv {
    dt: f32,
    lifespan: f32,
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
    if p.age >= env.lifespan {
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
    parent: &[Particle],
    clip_ms: f32,
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
            &child.def,
            clip_ms,
            emitting,
            scale,
            dt,
            &mut child.accumulator,
            &mut child.gate_prev,
        );
        while child.accumulator >= 1.0 && child.particles.len() < MAX_PARTICLES {
            child.accumulator -= 1.0;
            let (base, dir) = emit_local(&child.def, &mut child.rng);
            let local = base - origin;
            let speed = child.def.emission_speed
                * (1.0 + child.def.speed_variation * rand_s11(&mut child.rng));
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
                vel += (1.0 + child.def.speed_variation * rand_s11(&mut child.rng)) * p.vel;
            }
            let phase = next_u32(&mut child.rng);
            child.particles.push(Particle {
                pos: p.pos + fold(local),
                vel,
                age: 0.0,
                phase,
                fresh: true,
                quat: Quat::IDENTITY,
                angvel: Vec3::ZERO,
            });
        }
    }
}

/// Per-frame: emit, integrate, and rebuild each emitter's billboard mesh.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // one Bevy system's full input set
pub(super) fn simulate_particles(
    time: Res<Time>,
    tuning: Res<ParticleTuning>,
    // The far-clip wall — the emitter draw-set gate's depth bound (see the gate below).
    view: Res<crate::view::ViewDistance>,
    mut commands: Commands,
    cam: Query<(&GlobalTransform, &Frustum), With<WorldCamera>>,
    // Owner reads (joints/units/roots — never emitter or child-draw entities): disjoint from
    // both `&mut GlobalTransform` queries below.
    transforms: Query<&GlobalTransform, (Without<ParticleEmitter>, Without<ChildDraw>)>,
    // CHILD draw entities (world-root meshes like the parent's own): anchored + published
    // exactly like the parent entity each frame.
    mut child_draws: Query<
        (&mut Transform, &mut GlobalTransform, &mut Visibility),
        (
            With<ChildDraw>,
            Without<ParticleEmitter>,
            Without<WorldCamera>,
        ),
    >,
    images: Res<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    // The ground-snap probe (file 0x2000 births): terrain + WMO/doodad geometry, the walking
    // collision audience.
    spatial: SpatialQuery,
    // `Without<WorldCamera>`: the `&mut GlobalTransform` must be provably disjoint from the
    // camera read above too (an emitter never rides the camera entity).
    mut emitters: Query<
        (
            Entity,
            &mut ParticleEmitter,
            &mut Transform,
            &mut GlobalTransform,
            Option<&EmitterFade>,
            &mut Visibility,
            Option<&RenderLayers>,
        ),
        Without<WorldCamera>,
    >,
    // Booth cameras (decision 0539 §5): a booth-layered emitter (the glue scenes' braziers)
    // billboards against ITS camera, matched by layer intersection — the `face_booth_billboards`
    // rule. `Without<ParticleEmitter>`/`Without<ChildDraw>` keep the read provably disjoint from
    // the `&mut GlobalTransform` writes above.
    booth_cams: Query<
        (&GlobalTransform, &RenderLayers),
        (
            With<Camera3d>,
            Without<WorldCamera>,
            Without<ParticleEmitter>,
            Without<ChildDraw>,
        ),
    >,
) {
    let Ok((cam_tf, frustum)) = cam.single() else {
        return;
    };
    // Clamp dt so a load hitch doesn't fling every particle out of existence in one step.
    let dt = time.delta_secs().min(0.1);
    if dt <= 0.0 {
        return;
    }
    let cam_pos = cam_tf.translation();
    let density = tuning.density.clamp(0.25, 1.0);
    let snap_filter = crate::collision::player_query_filter();
    let (_, cam_rot, _) = cam_tf.to_scale_rotation_translation();
    let cam_right = cam_rot * Vec3::X;
    let cam_up = cam_rot * Vec3::Y;
    let face_normal = cam_up.cross(cam_right).normalize_or_zero(); // toward the camera
                                                                   // The far-clip wall's axis — the SAME forward `debug_panel::visibility` measures the owner
                                                                   // doodad's own cull along, so an emitter and its owner cross the wall together.
    let cam_fwd = Vec3::from(cam_tf.forward());

    for (entity, mut emitter, mut entity_tf, mut entity_global, fade, mut vis, layers) in
        &mut emitters
    {
        // The camera this emitter's quads face + its emission-LOD distance origin: a booth-layered
        // emitter uses its booth's camera (decision 0539 §5 — the glue scenes' braziers, which the
        // WORLD camera would billboard sideways and LOD from a nonsense distance); everything else
        // the world camera.
        let booth = layers
            .filter(|l| !l.intersects(&RenderLayers::default()))
            .and_then(|l| booth_cams.iter().find(|(_, cl)| cl.intersects(l)));
        let is_booth = booth.is_some();
        let (e_cam_pos, e_right, e_up, e_normal) = match booth {
            Some((tf, _)) => {
                let (_, rot, _) = tf.to_scale_rotation_translation();
                let right = rot * Vec3::X;
                let up = rot * Vec3::Y;
                (
                    tf.translation(),
                    right,
                    up,
                    up.cross(right).normalize_or_zero(),
                )
            }
            None => (cam_pos, cam_right, cam_up, face_normal),
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
                view.farclip,
                // Lateral planes only (`intersect_far = false`) — the depth bound is the farclip
                // term inside `in_draw_set`, deliberately; see `view::within_farclip`.
                frustum.intersects_sphere(
                    &CullSphere {
                        center: f.center.into(),
                        radius: f.radius,
                    },
                    false,
                ),
            );
            if !in_set {
                if *vis != Visibility::Hidden {
                    *vis = Visibility::Hidden;
                    // Child + model-instance draw entities mirror the gate (frozen with the
                    // parent).
                    for c in &emitter.children {
                        if let Ok((_, _, mut cv)) = child_draws.get_mut(c.entity) {
                            *cv = Visibility::Hidden;
                        }
                    }
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
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
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
                if !crate::view::within_farclip(view.farclip, cam_pos, cam_fwd, at, 0.0) {
                    if *vis != Visibility::Hidden {
                        *vis = Visibility::Hidden;
                        for c in &emitter.children {
                            if let Ok((_, _, mut cv)) = child_draws.get_mut(c.entity) {
                                *cv = Visibility::Hidden;
                            }
                        }
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
                if *vis != Visibility::Inherited {
                    *vis = Visibility::Inherited;
                }
            }
        }
        let ParticleEmitter {
            def,
            placement,
            owner,
            draining,
            attach,
            attach_rot,
            anchor,
            anchor_pos,
            particles,
            accumulator,
            emitter_prev,
            inherit_accum,
            inherit_vel,
            gate_prev,
            age,
            rng,
            mesh,
            texture,
            recursion: _,
            children,
            geometry: _,
            model_instances,
        } = &mut *emitter;
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
                    *draining = true;
                }
            }
        }
        if *draining && particles.is_empty() && children.iter().all(|c| c.particles.is_empty()) {
            for c in children.iter() {
                commands.entity(c.entity).despawn();
            }
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
        // FOLLOW-DELTA (file 0x4000 — wow-re `part-emitter-motion.md` §2 + §2b, §5-resolved):
        // a flagged emitter's particles keep exactly `fraction = clamp(slope·|Δ|/dt +
        // intercept, 0, 1)` of the emitter's per-frame WORLD motion (the authored two-point
        // response, [`benilla_formats::ParticleEmitterDef::follow_line`]) — the reference's
        // baseline for this content class is world-frozen (its emitter motion folds into the
        // emitter matrix, so births bake world-absolute) and the `+fraction·Δ` integrator add
        // recovers the ride. Over OUR anchor-riding storage the same observable is
        // `(fraction − 1)·Δ`: zero at saturation (rigid ride, the fast missile), the full
        // `−Δ` at fraction 0 (a world-frozen trail). Applied to every live particle after its
        // first integrate (the fresh-bit skip).
        let follow = if def.follow_emitter() && emitter_delta != Vec3::ZERO {
            def.follow_line().map_or(Vec3::ZERO, |(slope, intercept)| {
                let fraction = (slope * emitter_delta.length() / dt + intercept).clamp(0.0, 1.0);
                to_stored((fraction - 1.0) * emitter_delta, attach_inv, placement)
            })
        } else {
            Vec3::ZERO
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

        // 1. Age + integrate the live pool. The verified vanilla integrator (`particle_integrate`
        //    @ 0x7b2680): pos += dt·v with the gravity term on the UP axis (up += dt·v_up − ½·g·dt²;
        //    v_up −= g·dt), then **drag**: v −= min(dt·drag, 1)·v (exponential velocity decay,
        //    applied after gravity). Drag is load-bearing — a fast, long-lived, zero-gravity jet
        //    (e.g. the CandelabraTallWall flame: speed 0.56, life 6, g 0, drag 10) relies on it to
        //    stay a ~0.06 yd flicker; without it the particle coasts 3.3 yd to the ceiling. The
        //    frame decides the up axis: WoW +Z in model mode, Bevy +Y in world mode (the math is
        //    frame-independent otherwise — the reference integrates world-space particles with the
        //    same kernel). Kill at age ≥ lifespan.
        let g = def.gravity;
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
            lifespan: def.lifespan,
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
        //    The enabled M2Track (file +0x1dc) gates NEW emission on the same clip clock — the
        //    one-shot effects' choreography (a 200 ms hand flash inside a 1.0 s clip; the impact
        //    model's six staggered windows). Live particles are untouched — they finish their
        //    lifespans exactly like the drain above.
        // The clip clock: emitter age in ms, WRAPPED when the model's first sequence loops
        // (the client's m2_animate sequence time — a hold kit's windowed flash re-fires every
        // pass); a clamped sequence's tracks end-hold, bounded by the instance reap.
        let clip_ms = {
            let ms = *age * 1000.0;
            def.clip_wrap_ms.map_or(ms, |w| ms % w)
        };
        let emitting = !*draining && def.enabled.on_at(clip_ms);
        let dist_lod =
            (1.0 - (placement.translation.distance(e_cam_pos) - 50.0) * 0.02).clamp(0.25, 1.0);
        let burst = accumulate_emission(
            def,
            clip_ms,
            emitting,
            density * dist_lod,
            dt,
            accumulator,
            gate_prev,
        );
        if burst > 0.0 && crate::dbg_trace::enabled() {
            crate::dbg_trace::line("fx", &format!("burst n={burst} clip_ms={clip_ms:.0}"));
        }
        while *accumulator >= 1.0 && particles.len() < MAX_PARTICLES {
            *accumulator -= 1.0;
            let (base, dir) = emit_local(def, rng);
            let speed =
                def.emission_speed * (1.0 + def.speed_variation * (rand01(rng) * 2.0 - 1.0));
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
                    let lifted = world.y - hit.distance + def.over_life.sample(0.0).1;
                    pos = attach_inv * (Vec3::new(world.x, lifted, world.z) - *anchor_pos);
                }
            }
            // VELOCITY INHERIT consumption (the shape kernels' closing block, gated on the
            // flag): `vel += (1 + S11·speedVariation) · inherit`, its own S11 draw, in the
            // stored frame (the block runs after the reference's space fold).
            let vel = if def.inherits_emitter_motion() && *inherit_vel != Vec3::ZERO {
                vel + (1.0 + def.speed_variation * rand_s11(rng))
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
                phase,
                fresh: true,
                quat,
                angvel,
            });
        }

        // 2b. CHILD emitters (wow-re `part-child-recursion.md`): each drives off the live pool
        //     — post-integration, post-birth, exactly the window the reference's per-particle
        //     child loop sees — with the child's OWN rate/enabled tracks on its own clip wrap.
        //     A draining parent stops driving; child pools live out their spans (the despawn
        //     above waits for them).
        for child in children.iter_mut() {
            let raw_ms = *age * 1000.0;
            let c_ms = child.def.clip_wrap_ms.map_or(raw_ms, |w| raw_ms % w);
            let c_emitting = !*draining && child.def.enabled.on_at(c_ms);
            drive_child(
                child,
                particles,
                c_ms,
                c_emitting,
                density * dist_lod,
                dt,
                anchored,
                attach_inv,
                placement,
            );
            let c_env = StepEnv {
                dt,
                lifespan: child.def.lifespan,
                gravity: child.def.gravity,
                drag: child.def.drag,
                anchored,
                kill_origin: None,
                follow: Vec3::ZERO,
            };
            child
                .particles
                .retain_mut(|p| integrate_particle(p, &c_env));
        }

        // 3. Expand each pool into its billboard mesh ([`expand_quads`]) — per pool, only once
        //    its texture is resident. While one streams (or failed to decode), that mesh draws
        //    nothing rather than flash the engine fallback through the additive blend.
        // The cloud's anchor: the emitter's live position. The entity transform carries it (and
        // the mesh verts are relative to it) so the transparent pass sorts this cloud at a real
        // depth — see the note at the vertex push in `expand_quads`.
        let anchor = if anchored {
            *anchor_pos
        } else {
            placement.translation
        };
        entity_tf.translation = anchor;
        // Propagation already ran this frame (we're post-palette in PostUpdate) — publish the
        // anchor to the rendered GlobalTransform directly, or the cloud draws at LAST frame's
        // anchor: one frame × 20 yd/s detached the Shadow Bolt's puff cloud visibly behind the
        // skull (the same exactness rule as `face_billboards`; emitter entities live at the
        // world root, so the direct write is exact).
        *entity_global = GlobalTransform::from(*entity_tf);
        let frame = DrawFrame {
            anchored,
            anchor,
            attach_rot: *attach_rot,
        };
        let cam = CamBasis {
            right: e_right,
            up: e_up,
            face_normal: e_normal,
        };
        if let Some(m) = meshes.get_mut(&*mesh) {
            // A GEOMETRY (model-particle) emitter never draws quads — the reference's render
            // dispatch skips the whole quad path when the model mode is on; its instances
            // render via [`super::model::update_model_particles`].
            if def.geometry_model.is_none() && images.contains(&*texture) {
                expand_quads(def, particles, &frame, placement, &cam, m);
            } else {
                clear_particle_mesh(m);
            }
        }
        for child in children.iter() {
            if let Ok((mut ct, mut cg, mut cv)) = child_draws.get_mut(child.entity) {
                ct.translation = anchor;
                *cg = GlobalTransform::from(*ct);
                if *cv != Visibility::Inherited {
                    *cv = Visibility::Inherited;
                }
            }
            if let Some(m) = meshes.get_mut(&child.mesh) {
                if images.contains(&child.texture) {
                    expand_quads(&child.def, &child.particles, &frame, placement, &cam, m);
                } else {
                    clear_particle_mesh(m);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{inherit_trigger, integrate_particle, ChildEmitter, Particle, StepEnv, Vec3};
    use bevy::prelude::{Quat, Transform};

    fn particle(pos: Vec3, vel: Vec3) -> Particle {
        Particle {
            pos,
            vel,
            age: 0.0,
            phase: 0,
            fresh: false,
            quat: Quat::IDENTITY,
            angvel: Vec3::ZERO,
        }
    }

    fn env(kill_origin: Option<Vec3>, follow: Vec3) -> StepEnv {
        StepEnv {
            dt: 0.1,
            lifespan: 10.0,
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
            emission_speed: 0.0,
            flags: 0x40,      // inherit the parent particle's velocity
            area_length: 0.0, // point emitter: births land exactly ON the parent particle
            area_width: 0.0,
            ..crate::particles::tests::plain_def()
        });
        child.def.emission_rate = benilla_formats::ValueTrack {
            keys: vec![(0, 100.0)],
            interp: 0,
        };
        // Two live parents, far apart, distinct velocities.
        let parents = [
            particle(Vec3::X * 10.0, Vec3::Y * 3.0),
            particle(Vec3::X * -10.0, Vec3::Y * -3.0),
        ];
        // 100/s × 0.1 s × 2 calls = 20 births per frame.
        super::drive_child(
            &mut child,
            &parents,
            0.0,
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
            &[],
            0.0,
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
