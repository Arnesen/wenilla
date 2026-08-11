//! Faithful M2 **ribbon trails** — weapon enchant trails, wisp streamers, spell-missile trails.
//!
//! The 1.12 client simulates a ribbon as a ring of **edges**: each frame the emitter's bone-local
//! origin is transformed by the live bone matrix into a node point, `floor(dt·edgesPerSecond +
//! phase)` new edges (a vertex pair at `±heightAbove/heightBelow` across the node) are committed
//! and backdated inside the frame, old edges age out at `edgeLifetime`, **gravity carries the
//! stored verts along world +Z by `g·t²`** — up for the positive majority of the corpus — and the
//! edge list renders as a triangle strip whose `u` texcoord slides with edge age (the texture's
//! transparent tail fades the trail). Byte-exact spec: wow-5875-re
//! `system/models/scratch/ribbon-emitter-spec.md`; the sim below transcribes it with the same
//! simplifications as `particles` (distributions and frames mirrored, not the reference's exact
//! float slots).
//!
//! Like particles, each trail writes its strip into the **shared effect-quad stream**
//! ([`crate::particles::buffer::EffectQuads`], decision 0732 slice P1) — per segment, one quad
//! duplicating the shared edge vertices (identical triangles to the old strip mesh; a few dozen
//! extra vertices per trail buys the whole family one vertex layout and one index pattern). A
//! trail rides its **owner** entity (a skinned model's host-bone joint, an item root, a
//! missile); when the owner goes it drains — committed edges finish fading, then the trail
//! despawns itself (the reference's enable-gate law).

use std::collections::VecDeque;

use benilla_assets::coords::wow_to_bevy;
use benilla_assets::ModelRibbon;
use benilla_formats::ParticleBlend;
use bevy::prelude::*;

use crate::particles::buffer::{EffectDrawSpec, EffectFog, EffectQuads, EffectVertex};
use crate::player::WorldCamera;

/// Hard cap on stored edges — a backstop against a pathological rate·lifetime (the reference's
/// ring capacity is `ceil(rate·lifetime)+2`; shipped trails sit far below this).
const MAX_EDGES: usize = 512;

/// One committed trail edge: the vertex pair across the node, world (Bevy) space, and its birth
/// time on the shared clock.
struct Edge {
    top: Vec3,
    bottom: Vec3,
    born: f32,
    /// Seconds this edge has been alive, carried explicitly because the reference does
    /// ([`simulate_ribbons`]'s gravity note): its per-edge age array at `emitter+0x0c` is the term
    /// the gravity increment reads, and an edge born part-way through a frame starts at its
    /// **backdated** age, not zero.
    age: f32,
}

/// This frame's gravity displacement for one live edge, in world **+Z (up)** yards — the
/// reference's exact per-frame term (`ribbonage 0x7b7e60`, `0x7b8007`..`0x7b800f`):
/// `gravity · ((age + age) + dt) · dt`, applied identically to both of the edge's vertices, with
/// `age` advanced by `dt` straight after.
///
/// `(2a + dt)·dt ≡ (a + dt)² − a²`, so stepping it across an edge's life telescopes exactly to
/// `Δz = gravity · t²` — no ½, no factor of 2, and **upward** for a positive `gravity` (the block
/// is two `fadd`s with no `fchs`, on world-space positions in a +Z-up frame). See
/// [`simulate_ribbons`] for why both halves of that mattered on screen.
fn gravity_step(gravity: f32, age: f32, dt: f32) -> f32 {
    gravity * ((age + age) + dt) * dt
}

/// A live ribbon trail riding `owner`. Positions are world-space; the mesh entity's transform is
/// identity (like a particle emitter's).
#[derive(Component)]
pub struct RibbonTrail {
    def: benilla_formats::RibbonEmitterDef,
    /// The emission origin in the owner's frame: `wow_to_bevy(position − bone_pivot)` for a joint
    /// owner (the same rig identity as particle emitters), `wow_to_bevy(position)` for a root.
    local_offset: Vec3,
    /// The node source — `None` once the owner is gone (missile impacted, effect reaped, item
    /// unequipped): the trail then **drains** — commits nothing, ages its edges out, and
    /// despawns itself with the last edge. The reference frees a model's emitters SYNCHRONOUSLY
    /// at the model dtor (`0x70e313` — no orphan list; wow-re `ribbon-basis-emitter-lifecycle`);
    /// its visible fade comes from keeping the MODEL alive while emitters drain (the
    /// `HasLiveParticles 0x7b5f60` latch + the model's is-any-emitter-active flag). Our owners
    /// despawn at their own moment (impact, reap), so this drain reproduces the
    /// defer-until-drained shape; whether the client's effect controller actually polls the
    /// active flag before destroy, or hard-cuts at animation end, is the one OPEN half
    /// (CEffect-side, flagged in decision 0206).
    owner: Option<Entity>,
    /// Which clock the `+0xc0` enable gate is sampled against, every frame ([`RibbonSeq`]).
    seq: RibbonSeq,
    /// The MODEL INSTANCE whose [`crate::model_fade::ModelAlpha`] decides whether this trail is
    /// drawn at all (decision 0827). The reference's ribbon render leg reads the owning model's
    /// render alpha (`block+0x3c × Model+0x19c`) and **drops the draw** below a threshold (wow-re
    /// `ribbon-emitter-spec.md` §5) — so an invisible model has no streamer, which is what a
    /// first-person avatar's enchant trail needs (ledger F05). Only the drop is implemented: the
    /// note does not say the model alpha scales the strip's vertex colour the way it does a
    /// particle's, and inventing a ramp on top of a gate would be building past the evidence.
    /// `None` ⇒ always drawn (a placed prop, an effect instance).
    alpha_src: Option<Entity>,
    /// Committed edges, newest at the back. The live head (the current node) is appended at
    /// render time only, so the trail always connects to the emitter between commits.
    edges: VecDeque<Edge>,
    accumulator: f32,
    /// Seconds since spawn — the clip clock the keyed look tracks (colour/alpha/heights) sample
    /// against (an effect model's ribbons spawn at its clip start, so age == clip time — the
    /// particle emitters' law; a persistent trail's constant tracks are age-invariant).
    age: f32,
    texture: Handle<Image>,
    /// The owner-last draw-order rung ([`crate::particles::owner_last_bias`] over the owner's
    /// world reach, computed at spawn) — a trail is one of its model's emitters and takes the
    /// SAME rung as the quad clouds beside it (0721). Was the material's `depth_bias`; now the
    /// draw record's sort-key add.
    bias: f32,
    /// The owner model's bound sphere ([`ModelRibbon::water_bound`]) — the water-plane side is
    /// the MODEL's (0921): the ribbon leg reads the model's side-A boolean verbatim, slack
    /// included, so the trail flips with its model's bound centre, never with its whipping head.
    water_bound: (Vec3, f32),
}

impl RibbonTrail {
    /// The emitter bone this trail rides — the identity `WOW_PHASE=particles:<bone>` arms on, and
    /// the one `emdump`/`m2anim` print, so an instrument line and an asset line name the same trail.
    pub(crate) fn bone(&self) -> u16 {
        self.def.bone
    }

    /// The authored blend, and how many edges are committed right now (0 = nothing drawn yet).
    pub(crate) fn shape(&self) -> (ParticleBlend, usize) {
        (self.def.blend, self.edges.len())
    }
}

/// What decides a trail's `+0xc0` **enable** gate — the reference's per-ribbon `block+0xbc` byte,
/// which it re-reads every frame (wow-re `ribbon-emitter-spec.md` §6).
#[derive(Clone, Copy)]
pub enum RibbonSeq {
    /// Re-read each frame from this entity's [`AnimationPlayer`] + [`ModelAnimations`] — the unit,
    /// GameObject, doodad and effect-instance lanes, where the sequence **changes under the
    /// trail**: a trap springs, a door opens, an effect model steps Stand → Hold → Decay.
    Host(Entity),
    /// A fixed `AnimationData.dbc` id for the instance's life — a worn item rests in `Stand`(0),
    /// and nothing on it will ever play anything else.
    Fixed(u16),
}

/// Spawn a ribbon-trail entity for one [`ModelRibbon`], riding `owner` (a host-bone joint for a
/// skinned model — pass the joint and the def's baked pivot does the rebase — or the model/item
/// root). `seq` names the clock the per-sequence enable gate is sampled against (see
/// [`RibbonSeq`]). `owner_scale` is the owner placement's largest scale component — the
/// model-local [`ModelRibbon::owner_reach`] takes it to reach world yards, which is what the
/// draw-order rung is measured in. `None` if the trail has no resolved texture or degenerate
/// emission.
///
/// A gated trail still spawns: the gate is **live**, not a spawn-time decision, because the
/// sequence that answers it changes while the instance lives. Refusing at spawn was correct only
/// for the fixed-sequence lanes it was written for (a held item, a missile) and silently wrong for
/// everything with a state machine — see [`simulate_ribbons`].
#[allow(clippy::too_many_arguments)] // the spawn's full wiring, `alpha_src` included
pub fn spawn_ribbon(
    commands: &mut Commands,
    ribbon: &ModelRibbon,
    owner: Entity,
    use_pivot: bool,
    owner_scale: f32,
    seq: RibbonSeq,
    alpha_src: Option<Entity>,
) -> Option<Entity> {
    // Perf-bisect kill-switch: $WOW_NO_PARTICLES also spawns no ribbons (one switch, whole family).
    if std::env::var_os("WOW_NO_PARTICLES").is_some() {
        return None;
    }
    let texture = ribbon.texture.clone()?;
    let def = ribbon.def.clone();
    // The degenerate gate reads the tracks' PEAKS: a keyed slash (HolySmite) is born at height 0
    // and flares mid-clip — its value[0] is exactly the zero this gate must not trip on.
    if def.edges_per_second <= 0.0
        || (def.height_above.peak().max(0.0) + def.height_below.peak().max(0.0)) <= 0.0
    {
        return None; // nothing to trail
    }
    let p = def.position;
    let local = if use_pivot {
        [
            p[0] - ribbon.bone_pivot[0],
            p[1] - ribbon.bone_pivot[1],
            p[2] - ribbon.bone_pivot[2],
        ]
    } else {
        p
    };
    Some(
        commands
            .spawn((
                // The sim writes the trail's sort anchor (the live head node) here each frame
                // — the phase probe's read point.
                Transform::IDENTITY,
                RibbonTrail {
                    local_offset: wow_to_bevy(local),
                    def,
                    owner: Some(owner),
                    seq,
                    alpha_src,
                    edges: VecDeque::new(),
                    accumulator: 0.0,
                    age: 0.0,
                    texture,
                    // The reference's "a model's emitters draw after that model's batches" —
                    // the same rung the quad clouds take, from the same authored reach,
                    // because a trail is one of the model's emitters.
                    bias: crate::particles::owner_last_bias(ribbon.owner_reach * owner_scale),
                    water_bound: ribbon.water_bound,
                },
            ))
            .id(),
    )
}

/// Per-frame: place the node from the owner's live transform, commit/expire edges, sag by
/// gravity, and write the strip into the shared effect-quad stream.
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(crate) fn simulate_ribbons(
    time: Res<Time>,
    mut commands: Commands,
    // Owner reads (joints/roots — never trail entities): disjoint from the trail query's
    // `&mut GlobalTransform` below.
    transforms: Query<&GlobalTransform, Without<RibbonTrail>>,
    // The `+0xc0` enable gate's clock: the sequence a [`RibbonSeq::Host`] instance is playing.
    hosts: Query<(&AnimationPlayer, &benilla_assets::ModelAnimations)>,
    images: Res<Assets<Image>>,
    mut quads: ResMut<EffectQuads>,
    // The owning model's render alpha — the trail's draw gate (decision 0827), composed along the
    // attached-model chain (0833).
    model_alphas: crate::model_fade::ModelAlphas,
    // Trails belong to the world lane (no booth ribbons; a booth-parked owner's strip is eaten
    // by the shader's farclip wall, exactly as on the material path).
    world_cam: Query<Entity, With<WorldCamera>>,
    // The water-plane interleave inputs — a trail is one of the model's emitters and classifies
    // above/below water like the quad clouds ([`crate::particles::far_side_of_water`]).
    interleave: crate::particles::WaterInterleave,
    mut trails: Query<(
        Entity,
        &mut RibbonTrail,
        &mut Transform,
        &mut GlobalTransform,
    )>,
) {
    let Ok(cam) = world_cam.single() else {
        return;
    };
    let dt = time.delta_secs().min(0.1);
    let now = time.elapsed_secs();
    for (entity, mut trail, mut entity_tf, mut entity_global) in &mut trails {
        let RibbonTrail {
            def,
            local_offset,
            owner,
            seq,
            alpha_src,
            edges,
            accumulator,
            age,
            texture,
            bias,
            water_bound,
        } = &mut *trail;
        // The keyed look tracks sample on the trail's clip clock (see [`RibbonTrail::age`]):
        // heights at edge-commit time (each edge keeps the width it was born with — the
        // reference stores the vertex pair per edge), colour/alpha per frame for the whole strip.
        *age += dt;
        let ms = *age * 1000.0;
        let h_above = def.height_above.sample_ms(ms).max(0.0);
        let h_below = def.height_below.sample_ms(ms).max(0.0);

        // Owner gone (despawned missile/creature, unequipped item root) → DRAIN: no new
        // commits, the committed edges age out, and the trail despawns with its last edge
        // (see [`RibbonTrail::owner`]).
        if owner.is_some_and(|o| !transforms.contains(o)) {
            *owner = None;
        }
        let head = owner.and_then(|o| transforms.get(o).ok()).map(|owner_gt| {
            let node = owner_gt.transform_point(*local_offset);
            // Cross-section axis: the bone frame's local +Y — byte-VERIFIED (wow-re
            // `ribbon-basis-emitter-lifecycle.md`, the 0202 dispatch's fold-back): `node_place
            // 0x7b76c0` captures the basis fresh each frame from the live bone matrix, row 1
            // (= bone-local +Y) being the ±heightAbove/Below span (`ribbon_frame_build
            // 0x7b6990` fmuls only that pair). Sampling the live owner rotation here IS that
            // per-frame capture. (First pinned by elimination on the fireball missile's
            // authored bone pair; the bytes then confirmed it.)
            let axis = (owner_gt.rotation() * wow_to_bevy([0.0, 1.0, 0.0])).normalize_or(Vec3::Y);
            (node, axis)
        });
        if head.is_none() && edges.is_empty() {
            commands.entity(entity).despawn();
            continue;
        }

        // The `+0xc0` **enable** gate, sampled LIVE against the sequence the host is playing.
        // SETTLED at the bytes (wow-re `ribbon-emitter-spec.md` §7, closed — the dispatch this
        // session): the per-ribbon runtime byte `block+0xbc` IS the sampled `visibilityTrack`
        // value. Complete writer census — ctor `0x71b34c` = 0, loader default `0x70f80e` = 1,
        // then per frame `values[k0]` at `0x7176ee` (step arm) and `0x717714` (non-step arm; a u8
        // track is never blended, so both copy the same raw byte) inside `0x714260`. No
        // equipment/attach/sheathe writer exists anywhere. `0x718960` only READS it.
        //
        // Decision 1011 wired this and was right about the mechanism; decision 1013 unwound it on
        // the strength of §7's then-open INFERENCE and was wrong. What actually made the trap look
        // wrong was gravity (see below) — the low rig sank instead of rising, so the tuft the
        // reference shows above the crown vanished and only the gated-off high rig had ever been
        // producing anything visible, as a downward column. Two faults, one screenshot.
        //
        // Clearing the byte KILLS THE WHOLE RIBBON'S DRAW — `0x7080c2` jumps to the collect loop's
        // continue at `0x708263`, emitting no record at all. The earlier "committed edges keep
        // fading" gloss is withdrawn by the same pass, so this gates the DRAW, not just the
        // commit: a trail whose gate clears vanishes on the frame it clears.
        let lit = def.visible.as_ref().is_none_or(|vis| match *seq {
            RibbonSeq::Fixed(a) => vis.at(a, 0.0),
            RibbonSeq::Host(h) => match hosts.get(h) {
                // Sequences exist: the playing one answers, and `playing_seq` already degrades to
                // the loader-idle clip. A slot with no clip row falls back to `Stand`(0).
                Ok((player, anims)) => {
                    let (anim, t) = crate::doodad_anim::playing_seq(player, anims)
                        .and_then(|(slot, t)| {
                            Some((anims.clips.iter().find(|c| c.seq_index == slot)?.anim_id, t))
                        })
                        .unwrap_or((0, 0.0));
                    vis.at(anim, t)
                }
                // No clock on the host at all — the instance is still being built this frame. The
                // reference never has this window (its loader seed arms a sequence the moment the
                // M2 goes LIVE, `0x70ebd0`), so answering "enabled" here would invent a state and
                // pop one frame of every gated trail. Hold dark until the host can answer.
                Err(_) => false,
            },
        });
        let head = lit.then_some(head).flatten();

        // Expire old edges (front = oldest), move the rest under gravity, commit new ones at rate.
        while edges
            .front()
            .is_some_and(|e| now - e.born >= def.edge_lifetime)
        {
            edges.pop_front();
        }
        // GRAVITY — byte-verified (`ribbonage 0x7b7e60`, the loop at `0x7b7fe7..0x7b807a`; wow-re
        // `ribbon-emitter-spec.md` §4). Per frame, per live edge, into BOTH vertices' world z:
        //
        //     term = gravity · ((age + age) + dt) · dt   ;   age += dt
        //
        // `(2a + dt)·dt` is identically `(a + dt)² − a²`, and the age advance follows immediately,
        // so the whole loop telescopes to a closed form — an edge sits at
        //
        //     z(t) = z_at_emit + gravity · t²        (t = seconds since commit, yards)
        //
        // with no ½ and no factor of 2. It is a **position** increment; the vertex is the only
        // state besides the per-edge age, there is no velocity, and it is scaled by nothing (the
        // interp factor the loop computes just above is never an operand here).
        //
        // And the sign is `fadd`, twice, with no `fchs` anywhere in the block: positions are world
        // space, WoW's +Z is UP, so a **positive gravity makes the trail RISE**. The corpus agrees
        // — of 590 ribbon records 86 are positive (totems 5.0, the cleanse family 0.5/1.0) and 16
        // negative, including ±0.5 up/down PAIRS inside one model, which a single-signed reading
        // cannot explain at all. Our old `pos.y -= 2·g·dt` was wrong twice over: constant velocity
        // instead of `g·t²`, and falling instead of rising. On the Frost Trap that turned a
        // 0.6–1.3 yd tuft rising off the crown into a 2–3 yd column smeared to the ground.
        for e in edges.iter_mut() {
            let term = gravity_step(def.gravity, e.age, dt);
            e.top.y += term;
            e.bottom.y += term;
            e.age += dt;
        }
        // EMISSION — `edgesPerSecond` is a true rate, not a per-frame cadence: the reference
        // commits `n = floor(dt·eps + phase)` edges this frame, carries the fraction as the phase,
        // and **backdates** each one inside the frame (wow-re §4.2). A one-edge-per-frame cap
        // silently thins every trail below `eps` frames per second — the whole trail, at 30 fps
        // with the Frost Trap's `eps` 30, is half the edges it should hold.
        if let Some((node, axis)) = head {
            *accumulator += def.edges_per_second * dt;
            let n = accumulator.floor().max(0.0);
            *accumulator -= n;
            // Sub-frame backdating: the n-th edge of this frame was emitted `k/n · dt` ago (the
            // node itself only has this frame's position, so the sample point is shared — the
            // backdate is what the age, and therefore the gravity rise, is measured from).
            let n = (n as usize).min(MAX_EDGES.saturating_sub(edges.len()));
            for k in 0..n {
                let back = dt * (n - 1 - k) as f32 / n as f32;
                edges.push_back(Edge {
                    top: node + axis * h_above,
                    bottom: node - axis * h_below,
                    born: now - back,
                    age: back,
                });
            }
        }

        // Write the strip into the shared stream: live head first (while the owner lives), then
        // committed edges newest→oldest. u slides with age across the tex-slot cell (the
        // texture's transparent tail is the fade); v spans the cell band. An idle trail — no
        // strip yet, or a non-resident texture — pushes nothing and commits nothing: the old
        // "don't rewrite an already-empty mesh" guard is now the structure itself.
        if !images.contains(&*texture) {
            continue;
        }
        // The gate again, on the DRAW: `0x7080c2` skips the whole record when the byte is 0, so a
        // gated-off ribbon shows nothing — not a fading remainder. The sim above still ran (edges
        // age and expire exactly as `ribbonage` ages them), which is what makes the trail resume
        // mid-strip rather than from empty when the byte comes back.
        if !lit {
            continue;
        }
        // An invisible MODEL has no streamer: the reference's ribbon render leg reads the owning
        // model's render alpha and drops the draw below a threshold (decision 0827). This is what
        // takes your own weapon's enchant trail out of your face in first person, and keeps a
        // not-yet-shown unit's trail off the screen while its body is still at alpha 0.
        if alpha_src.is_some_and(|e| model_alphas.get(e) <= 1e-3) {
            continue;
        }
        let n = edges.len() + usize::from(head.is_some());
        if n < 2 {
            continue;
        }
        let (rows, cols) = (def.tile_rows.max(1), def.tile_cols.max(1));
        let cell = def.tex_slot.min(rows * cols - 1);
        let (u0, u1) = (
            f32::from(cell % cols) / f32::from(cols),
            f32::from(cell % cols + 1) / f32::from(cols),
        );
        let (v0, v1) = (
            f32::from(cell / cols) / f32::from(rows),
            f32::from(cell / cols + 1) / f32::from(rows),
        );
        // RAW authored RGB — the gamma decode happens once in the effect shader (decision 0152),
        // covering the texture term too. Alpha is a blend weight, raw.
        let rgb = def.color.sample_ms(ms);
        let rgba = [rgb[0], rgb[1], rgb[2], def.alpha.sample_ms(ms).max(0.0)];
        // The trail's SORT anchor — the live head node (the point the material path's entity
        // translation used to carry; same sort-tie flashing fix as the particle clouds).
        // Draining trails anchor on their newest surviving edge.
        let anchor = head.map(|(node, _)| node).unwrap_or_else(|| {
            let e = edges.back().expect("n >= 2 ⇒ edges exist while draining");
            (e.top + e.bottom) * 0.5
        });
        entity_tf.translation = anchor;
        // Post-propagation frame: publish directly (see the particle sim's matching note; trail
        // entities live at the world root, the direct write is exact).
        *entity_global = GlobalTransform::from(*entity_tf);
        // The edge sequence, head first then newest→oldest — each consecutive pair becomes one
        // quad whose corner order reproduces the old strip's exact triangles: strip triangles
        // (t₀,b₀,t₁),(b₀,b₁,t₁) = quad [b₀,b₁,t₁,t₀] under the lane's [0,1,2, 0,2,3] pattern.
        let mut pairs: Vec<(Vec3, Vec3, f32)> = Vec::with_capacity(n);
        if let Some((node, axis)) = head {
            pairs.push((node + axis * h_above, node - axis * h_below, 0.0));
        }
        for e in edges.iter().rev() {
            pairs.push((
                e.top,
                e.bottom,
                ((now - e.born) / def.edge_lifetime).clamp(0.0, 1.0),
            ));
        }
        let start = quads.begin();
        for w in pairs.windows(2) {
            let ((t0, b0, a0), (t1, b1, a1)) = (w[0], w[1]);
            let (ua0, ua1) = (u0 + (u1 - u0) * a0, u0 + (u1 - u0) * a1);
            for (pos, uv) in [
                (b0, [ua0, v1]),
                (b1, [ua1, v1]),
                (t1, [ua1, v0]),
                (t0, [ua0, v0]),
            ] {
                quads.verts.push(EffectVertex {
                    pos: pos.to_array(),
                    uv,
                    color: rgba,
                });
            }
        }
        quads.commit_quads(
            start,
            EffectDrawSpec {
                cam,
                texture: texture.id(),
                blend: def.blend.into(),
                // params.x = the per-blend fog-colour policy (the M2 batch state setter's
                // table, `0x70baf0` / wow-re ROUND 4 — ribbons ride the same trio): additive
                // trails fog toward BLACK, fading under the storm veil instead of adding grey;
                // alpha/opaque trails fog toward the scene colour. (No ribbon authors the
                // particle "unfogged" file flag — pass 0.)
                fog: EffectFog::for_blend(0, def.blend),
                // Ribbons keep the lane's unlit default: the M2 ribbon record has no flag word
                // to read the particle path's unlit bit off, and the trail corpus is additive
                // weapon/spell art authored to burn at its own colour. Revisit only with a
                // byte law for the ribbon batch state, not by analogy with particles.
                lit: false,
                anchor,
                // The owner rung, dropped under the water pass when the MODEL sits on the eye's
                // far side of its water plane — the model's bound centre with the bound-radius
                // slack, never the whipping head node (0921: the ribbon leg reads the model's
                // side-A boolean verbatim, `0x7081f1`). The model frame is `alpha_src` — "the
                // MODEL INSTANCE" — with the owner (possibly a joint) seconding as the walk
                // seed; an unresolvable matrix falls back to the sign test at the head.
                bias: *bias
                    + if crate::particles::model_far_side(
                        &interleave,
                        alpha_src.or(*owner),
                        alpha_src.and_then(|e| transforms.get(e).ok()),
                        *water_bound,
                        anchor,
                    ) {
                        crate::sky_order::FAR_SIDE_BIAS
                    } else {
                        0.0
                    },
                raster_bias: 0,
                cam_relative: false,
                main_entity: entity,
                light: None, // trails never carry a light override (world lane only)
            },
        );
    }
}

/// Registers the per-frame ribbon simulation. Trails are spawned by the model spawn sites
/// (creatures, held items, missiles, spell effects, doodads) via [`spawn_ribbon`].
pub struct RibbonPlugin;

impl Plugin for RibbonPlugin {
    fn build(&self, app: &mut App) {
        // PostUpdate, after the billboard joint palette — same law and reason as the particle
        // sim: a trail node on a billboarded/animated bone must sample the frame the palette
        // just wrote (see `billboard_joint_palette`'s consumer note).
        app.add_systems(
            PostUpdate,
            simulate_ribbons
                .in_set(crate::billboard::BillboardPlace)
                .after(crate::billboard::billboard_joint_palette)
                .after(crate::creature_anim::finalize_rig_worlds),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::gravity_step;

    /// The per-frame gravity term telescopes to the closed form the bytes imply: stepping
    /// `gravity · ((2·age) + dt) · dt` and advancing `age` by `dt` leaves an edge exactly
    /// `gravity · t²` from where it was emitted, **whatever the frame rate** — which is the
    /// property that makes a trail look the same at 30 and 144 fps. Sign included: positive
    /// gravity rises.
    #[test]
    fn gravity_telescopes_to_g_t_squared_at_any_frame_rate() {
        for &g in &[0.5_f32, 1.0, 1.5, 2.0, 5.0, -1.0] {
            for &dt in &[1.0 / 144.0_f32, 1.0 / 60.0, 1.0 / 30.0, 1.0 / 15.0] {
                let steps = (1.0 / dt).round() as usize; // exactly one second of frames
                let (mut z, mut age) = (0.0_f32, 0.0_f32);
                for _ in 0..steps {
                    z += gravity_step(g, age, dt);
                    age += dt;
                }
                let t = steps as f32 * dt;
                assert!(
                    (z - g * t * t).abs() < 1e-4,
                    "g {g} at dt {dt}: got {z}, want {}",
                    g * t * t
                );
            }
        }
        // The sign is the half that was inverted: a positive gravity RISES.
        assert!(gravity_step(2.0, 0.0, 0.016) > 0.0);
        assert!(gravity_step(-1.0, 0.0, 0.016) < 0.0);
    }

    /// The Frost Trap's four low streamers, with their authored `gravity` and `edgeLifetime`:
    /// each rises `g·L²` = 0.605 / 1.000 / 1.215 / 1.280 yd over an edge's life, from a node at
    /// model z 0.129. That is the compact tuft standing off a crown whose own geometry stops at
    /// z 0.637 — and it is what the old constant-velocity fall (1.1–3.2 yd DOWNWARD from the
    /// twelve upper streamers at z 1.55) turned into a column reaching the ground.
    #[test]
    fn frost_trap_low_rig_rises_into_a_tuft_over_the_crown() {
        for (g, life, want) in [
            (0.5_f32, 1.1_f32, 0.605_f32),
            (1.0, 1.0, 1.000),
            (1.5, 0.9, 1.215),
            (2.0, 0.8, 1.280),
        ] {
            let dt = 1.0 / 60.0;
            let steps = (life / dt).round() as usize;
            let (mut z, mut age) = (0.0_f32, 0.0_f32);
            for _ in 0..steps {
                z += gravity_step(g, age, dt);
                age += dt;
            }
            assert!((z - want).abs() < 0.01, "g {g} life {life}: {z} vs {want}");
            assert!(0.129 + z > 0.637, "clears the crown's own geometry");
        }
    }
}
