//! Faithful M2 **ribbon trails** — weapon enchant trails, wisp streamers, spell-missile trails.
//!
//! The 1.12 client simulates a ribbon as a ring of **edges**: each frame the emitter's bone-local
//! origin is transformed by the live bone matrix into a node point, a new edge (a vertex pair at
//! `±heightAbove/heightBelow` across the node) is committed at `edgesPerSecond`, old edges age out
//! at `edgeLifetime`, gravity sags the stored verts, and the edge list renders as a triangle strip
//! whose `u` texcoord slides with edge age (the texture's transparent tail fades the trail).
//! Byte-exact spec: wow-5875-re `system/models/scratch/ribbon-emitter-spec.md` (their `9c862186`);
//! the sim below transcribes it with the same simplifications as `particles` (distributions and
//! frames mirrored, not the reference's exact float slots).
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

/// Spawn a ribbon-trail entity for one [`ModelRibbon`], riding `owner` (a host-bone joint for a
/// skinned model — pass the joint and the def's baked pivot does the rebase — or the model/item
/// root). `current_anim` is the `AnimationData.dbc` id the owner's model is running (a static held
/// item rests in Stand; `None` = the model's first clip / no sequence), which the per-sequence
/// visibility gate keys off. `owner_scale` is the owner placement's largest scale component — the
/// model-local [`ModelRibbon::owner_reach`] takes it to reach world yards, which is what the
/// draw-order rung is measured in. `None` if the trail has no resolved texture, degenerate
/// emission, or is dark in that sequence.
#[allow(clippy::too_many_arguments)] // the spawn's full wiring, `alpha_src` included
pub fn spawn_ribbon(
    commands: &mut Commands,
    ribbon: &ModelRibbon,
    owner: Entity,
    use_pivot: bool,
    owner_scale: f32,
    current_anim: Option<u16>,
    alpha_src: Option<Entity>,
) -> Option<Entity> {
    // Perf-bisect kill-switch: $WOW_NO_PARTICLES also spawns no ribbons (one switch, whole family).
    if std::env::var_os("WOW_NO_PARTICLES").is_some() {
        return None;
    }
    // Per-sequence visibility (`+0xc0`): a keyed ribbon shows only in the sequences its author lit
    // — the thrown weapon's trail is OFF in Stand (worn in hand) and Impact (landed), ON only in
    // InFlight. A model with no gate (`None` — enchant trails, wisps) always shows. Owner and
    // missile are separate fixed-sequence entities, so the spawn-time decision holds for the
    // instance's life (the shipped keyed ribbons are constant within a sequence).
    if let Some(vis) = &ribbon.def.visible_in_anim {
        let anim = current_anim.unwrap_or(0); // a static held item rests in Stand (0)
        let visible = vis
            .get(&anim)
            .or_else(|| vis.get(&0))
            .copied()
            .unwrap_or(true);
        if !visible {
            return None; // dark this sequence — no trail
        }
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
                    alpha_src,
                    edges: VecDeque::new(),
                    accumulator: 0.0,
                    age: 0.0,
                    texture,
                    // The reference's "a model's emitters draw after that model's batches" —
                    // the same rung the quad clouds take, from the same authored reach,
                    // because a trail is one of the model's emitters.
                    bias: crate::particles::owner_last_bias(ribbon.owner_reach * owner_scale),
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
            alpha_src,
            edges,
            accumulator,
            age,
            texture,
            bias,
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

        // Expire old edges (front = oldest), sag the rest, commit new ones at cadence.
        while edges
            .front()
            .is_some_and(|e| now - e.born >= def.edge_lifetime)
        {
            edges.pop_front();
        }
        if def.gravity != 0.0 {
            let sag = 2.0 * def.gravity * dt;
            for e in edges.iter_mut() {
                e.top.y -= sag;
                e.bottom.y -= sag;
            }
        }
        if let Some((node, axis)) = head {
            *accumulator += def.edges_per_second * dt;
            if *accumulator >= 1.0 {
                *accumulator = accumulator.fract();
                if edges.len() < MAX_EDGES {
                    edges.push_back(Edge {
                        top: node + axis * h_above,
                        bottom: node - axis * h_below,
                        born: now,
                    });
                }
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
                anchor,
                // The owner rung, dropped under the water pass when the head node sits on the
                // eye's far side of its water plane — the same interleave the quad clouds take.
                bias: *bias
                    + if crate::particles::far_side_of_water(&interleave, *owner, anchor) {
                        crate::sky_order::EFFECT_FAR_SIDE_BIAS
                    } else {
                        0.0
                    },
                raster_bias: 0,
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
