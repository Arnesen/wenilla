//! The placed-model assembler: [`spawn_model_entities`] builds a model's submesh entities with the
//! full doodad/WMO component set (production materials, fade, mesh tags, the doodad anim host and
//! its skinned twins). Two consumers — the terrain streamer's world-static placements
//! ([`super::spawn_loaded_placements`]) and the WMO-display gameobject's doodad props
//! (`crate::entities`' `wmo_props`).

use benilla_assets::{M2Model, ModelSubmesh};
use benilla_formats::ModelBlend;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::render::render_resource::Buffer;

use crate::billboard::BillboardCard;
use crate::doodad_anim::DoodadAnimHost;
use crate::mesh_tag::alpha_bits;
use crate::model_fade::DoodadFade;
use crate::model_render::{model_material, MaterialCache, ShadeSel};
use crate::model_render::{ModelKind, ModelPart};
use benilla_assets::materials::WowModelMaterial;

/// What one placement's animation host armed, for the consumers that spawn alongside its submeshes.
/// All fields are *per placement*, not per model: the anchors are this instance's, and `arm` is
/// this instance's own anim root, whose live player names the sequence it is currently playing.
pub struct PlacementHost {
    /// `(bone, anchor)` — the consumer anchors this placement minted (decision 1365): one entity
    /// per bone something actually rides, registered in the host's `RigPose` (its compose passes
    /// re-seat them) and pre-minted here for every emitter and ribbon bone the model authors, so
    /// the fx spawns that run after the pose buffer is attached only look entities up.
    anchors: Vec<(u16, Entity)>,
    /// This placement's anim-root entity. It rides the returned entity list so it despawns with the
    /// placement, but it is **not geometry** — a caller tagging "everything this placement draws"
    /// has to be able to leave it out, or it lands in a cull that can only fail open on a boundless
    /// entity (decision 0784). Distinct from `arm`, which is `None` unless a sequence was armed.
    pub(crate) root: Entity,
    /// The anim-root entity IF this placement armed a sequence; `None` on the gseq-only tier, where
    /// nothing was armed and slot 0 is the honest answer.
    ///
    /// This used to be the armed slot as a plain `usize`, captured once at spawn. That was right
    /// only while a placed doodad kept one variation for life — it re-rolls every play-window
    /// (decision 0768), so a captured slot goes stale within about a second and the consumer has to
    /// resolve against the live player instead.
    pub arm: Option<Entity>,
}

impl PlacementHost {
    /// The anchor standing in for `bone` — what `joints.get(bone)` used to answer. `None` = the
    /// bone hosts nothing this placement consumes (or is outside the skeleton): the consumer
    /// falls back exactly as it did on a missing joint.
    pub fn anchor(&self, bone: u16) -> Option<Entity> {
        self.anchors
            .iter()
            .find(|&&(b, _)| b == bone)
            .map(|&(_, e)| e)
    }
}

/// Spawn a model's submeshes at `transform` with the full doodad/WMO component set. Returns the spawned
/// entities (for refcounted despawn).
///
/// `pub(crate)`: two consumers — the terrain streamer's world-static placements here, and the
/// WMO-display GameObject's doodad props ([`crate::entities`]'s `wmo_props`, the ship's sails), which
/// pass a doodad-LOCAL `transform` + `card_owner` and parent the returned entities under the moving
/// gameobject (every downstream system reads propagated `GlobalTransform`s, so the composition holds).
#[allow(clippy::too_many_arguments)]
pub fn spawn_model_entities(
    commands: &mut Commands,
    mat_cache: &mut MaterialCache,
    materials: &mut Assets<WowModelMaterial>,
    light: &Buffer,
    submeshes: &[ModelSubmesh],
    // The model's app-built render forms (decision 0834), index-parallel with `submeshes`: the
    // static handle + its build-time `Aabb` per batch, and the skinned twins when this model's
    // lane rigs (`None` otherwise). Callers gate on `ModelForms::require`, so by the time a
    // placement spawns these are complete.
    forms: crate::model_forms::FormSlices<'_>,
    transform: Transform,
    is_wmo: bool,
    // The static terrain-shade selector for every batch (the MCSH sample at this placement's base →
    // `ShadeSel::Lit`/`Matte`/`Shaded`; see `model_render::ShadeSel`). Ignored by WMO group geometry
    // and interior props (their lighting lanes don't read it).
    shade: ShadeSel,
    // `Some(slot)` for an INTERIOR M2 prop: its folded SH probe's table slot, carried per-instance
    // in `MeshTag` on EVERY batch of the model, billboard cards included (read by
    // `wow_model.wgsl`'s interior-prop lane). Its ordinary batches are steady indoors; a card still
    // distance-fades with its doodad, and the fade composes with the slot rather than clobbering it
    // (decision 0778). `None` everywhere else (exterior props fade; WMO groups use their own
    // per-submesh interior flag + batch class).
    interior_slot: Option<u16>,
    radius: f32,
    local_center: Vec3,
    // The model's authored **all-animation** bound in Bevy model-local space
    // ([`super::m2_anim_bound`]) — what an ANIMATED placement's submeshes are culled with instead of
    // their bind-pose mesh bound, which the joint palette has left behind (decision 1259). `None` for
    // WMO group geometry (no M2 header) and for a model with no authored box; a static placement
    // ignores it and keeps the tighter per-batch bound.
    anim_bound: Option<Aabb>,
    // `Some((model, now))` for an M2 placement: the doodad-animation gate (decision 0130) — an
    // animated model spawns joints + the skinned twins; `now` is the clock origin. `None` for WMO
    // group geometry.
    m2: Option<(&M2Model, f32)>,
    // UV-animated material registry (decision 0130 phase 3): a batch with a `uv_anim` loop registers
    // its material here so `tick_anim_materials` scrolls the shared offset while it is drawn.
    uv_reg: &mut crate::doodad_anim::UvAnimMaterials,
    // Animated-tint material registry (the M2Color RGB twin of `uv_reg`): a batch with an
    // `rgb_anim` loop registers its material here so `tick_anim_materials` re-samples the
    // shared tint each frame.
    tint_reg: &mut crate::doodad_anim::TintAnimMaterials,
    // `Some(anchor)` for a prop spawned ON a streamed entity (the WMO-gameobject path): a boneless
    // model's billboard cards FOLLOW this anchor (`BillboardCard::following` — the entity-path law,
    // decision 0153) instead of baking a world pivot, so they track the moving owner and
    // self-despawn with it; such cards are also excluded from the returned entity list (the caller
    // parents that list, and a card is a world root the billboard pass writes absolutely).
    // `None` for world-static placements (terrain), whose pivots never move.
    card_owner: Option<Entity>,
    // Returns the spawned entities + what the anim host armed for this placement, when the model
    // animates: the joint set (bone-indexed) the emitter spawn rides its host bone off (0130 phase
    // 4), and the FILE sequence slot its variation roll landed on, which the emitters' rate/gate
    // tracks must be sampled against (decision 0760).
) -> (Vec<Entity>, Option<PlacementHost>) {
    let kind = if is_wmo {
        ModelKind::Wmo
    } else {
        ModelKind::Doodad
    };
    let mut out = Vec::with_capacity(submeshes.len());
    // The doodad-animation host (decision 0130 phase 1): an animated model — most placed instances
    // aren't, and stay on the static path below untouched — gets an anim-root + joint hierarchy +
    // player/drive from [`crate::doodad_anim::spawn_anim_host`]; its ordinary submeshes then draw
    // the skinned twin bound to those joints. Billboard batches keep today's camera-facing path
    // (their transform is the billboard system's, and their glow-pulse already rides
    // `BoneScaleAnim`). The host exists for every JOINT CONSUMER, not just skinned submeshes:
    // emitters and ribbons ride their bone's joint too (0130 phase 4), so a particles-only model
    // (0 render batches — the InstancePortal swirl, whose two emitters sit on the spinning rotor
    // bones) still needs its joints driven. Only a model with nothing but billboard cards and no
    // emitters/ribbons spawns no host.
    let mut host = m2
        .filter(|(m, _)| {
            submeshes.iter().any(|s| s.billboard.is_none())
                || !m.emitters.is_empty()
                || !m.ribbons.is_empty()
        })
        .and_then(|(m, _)| crate::doodad_anim::spawn_anim_host(commands, m, transform));
    // Captures freeze material-alpha clocks at 0 (deterministic frames) — read the env once.
    let mat_frozen = crate::dev_state::deterministic_run();
    let animated = host.is_some();
    let rig_root = host.as_ref().map(|h| h.root);
    if let Some(h) = &host {
        out.push(h.root);
    }
    let mut skinned_meshes: Vec<Entity> = Vec::new();
    // The parts the lazy-rig wake will promote static → skinned (decision 0863) — the ones that
    // got a [`crate::doodad_anim::SkinnedTwin`] below.
    let mut lazy_parts: Vec<Entity> = Vec::new();
    for (batch_idx, sub) in submeshes.iter().enumerate() {
        // The batch's app-built render form (decision 0834). Callers gate spawning on the forms
        // being complete, so a miss here is a broken contract — skip the batch rather than panic.
        let Some((stat_mesh, stat_aabb)) = forms.stat.get(batch_idx) else {
            continue;
        };
        // Every batch (WMO and M2) carries its authored order (index + 1) into the material's
        // transparent sort bias, so one model's coplanar layers draw in file order instead of
        // re-flipping a sort tie every frame (`model_render::BATCH_ORDER_SORT_EPS`). WMO batches
        // additionally ride it into the clip-z nudge that resolves coplanar layers in MOBA file
        // order — the byte-verified client behaviour (wow-5875-re
        // models/scratch/wmo-batch-blend-depth-state.md); `model_material` gates that half on
        // `is_wmo`.
        let batch_order = u16::try_from(batch_idx + 1).unwrap_or(u16::MAX);
        let interior = sub.interior || interior_slot.is_some();
        // A billboard card is culled by the SAME rule as any other batch — its material's `0x04`
        // flag, nothing else. This site used to force `|| sub.billboard.is_some()`, on the reasoning
        // that a card whose plane normal points away from the viewer would backface-cull to nothing;
        // that is precisely the mechanism the reference USES, and forcing it off is what drew the
        // stray solid triangles beside working particle effects (decision 0629, bugs B05/B34).
        // A billboard bone puts the model's +X toward the viewer, the cull is GL_BACK/CCW (wow-re
        // `models.md` MOMT/M2 flag map, `m2-depth-blend-state.md` §0x04), so a −X-facing card is
        // never seen — and an author who wanted one seen set `0x04` themselves: Elwynn's LampPost
        // carries a two-sided −X card AND a single-sided +X glow card, one model, both rules.
        let two_sided = sub.two_sided;
        // A lit interior M2 prop submesh (not a WMO group): it carries its SH-probe slot in
        // `MeshTag`, so the shader evaluates the room's probe instead of the sky base. **Billboard
        // batches are included** — a chain or glow card is a batch of the same model, and the
        // reference shades every batch of an object through one light node (decision 0778). They
        // were excluded here on a since-stale worry that the distance fade would overwrite the
        // slot: true before the 0355 re-lane, when the slot lived in the alpha bits, and false
        // after it — the fade writes through `mesh_tag::with_alpha`, which composes with the slot
        // (`debug_panel::visibility`). Excluding them left an interior card on an interior-mode
        // material with NO slot in its tag, which the shader decodes as slot **0** — whichever
        // probe won the streaming race, the same wrong-probe read the 0355 note below describes.
        let interior_probe = !is_wmo && interior_slot.is_some();
        // …and the narrower half: only a NON-card interior prop is steady indoors. A card still
        // distance-fades with its doodad (below — the halo must cull when the lamp does), so it
        // keeps its fade twin and its `DoodadFade`.
        let steady_interior_prop = interior_probe && sub.billboard.is_none();
        let cutout = model_material(
            mat_cache,
            materials,
            sub.texture.clone(),
            sub.blend,
            two_sided,
            is_wmo,
            interior,
            sub.emissive,
            sub.additive,
            false,
            sub.no_depth_write,
            sub.no_depth_test,
            sub.fog_policy,
            sub.env_map, // texture_unit_lookup > 2 ⇒ the runtime generates this batch's UVs
            shade,
            batch_order,
            sub.uv_anim.as_ref(),
            sub.rgb_anim.as_ref(),
            sub.wmo_batch,
            sub.sidn,
            sub.window,
            false, // the world streamer never spawns a skybox
            light,
        );
        // The blend twin for the distance-fade feather pass (reuse the cutout when already blend, or when
        // this is a non-fading interior prop). A MULTIPLY batch (Mod/Mod2x — the weapon-rack
        // ARMORREFLECT sheen) also reuses its steady self: its blend equation reads no alpha, so no
        // material swap can feather it — the reference's instanceAlpha fade leaves it at full
        // strength (0528). Since decision 0865 the tag alpha it already carries is no longer inert:
        // `wow_model.wgsl` lerps a multiply batch's colour toward the blend identity by it, so the
        // rack sheen now rides the distance fade too (the deliberate deviation).
        let blend = if steady_interior_prop
            || matches!(
                sub.blend,
                ModelBlend::Blend | ModelBlend::Mod | ModelBlend::Mod2x
            ) {
            cutout.clone()
        } else {
            // The SOURCE blend (Opaque or AlphaKey here) rides into the twin: fade_variant builds
            // AlphaMode::Blend either way, and the source decides the twin's 224/255 cutout marker
            // (only an AlphaKey source alpha-tests while fading — decision 0842).
            model_material(
                mat_cache,
                materials,
                sub.texture.clone(),
                sub.blend,
                two_sided,
                is_wmo,
                interior,
                sub.emissive,
                sub.additive,
                true,
                sub.no_depth_write,
                sub.no_depth_test,
                sub.fog_policy,
                sub.env_map, // texture_unit_lookup > 2 ⇒ the runtime generates this batch's UVs
                shade,
                batch_order,
                sub.uv_anim.as_ref(),
                sub.rgb_anim.as_ref(),
                sub.wmo_batch,
                sub.sidn,
                sub.window,
                false, // the world streamer never spawns a skybox
                light,
            )
        };
        // Register both variants' materials for the per-frame UV scroll (idempotent per material —
        // every instance of the batch lands on the same deduped handle).
        if let Some(anim) = &sub.uv_anim {
            uv_reg.0.entry(cutout.id()).or_insert_with(|| anim.clone());
            uv_reg.0.entry(blend.id()).or_insert_with(|| anim.clone());
        }
        // …and for the per-frame tint re-sample (the animated M2Color RGB, same shared clock —
        // the same invisible seq-band phase divergence as the UV scroll, recorded there).
        if let Some(anim) = &sub.rgb_anim {
            tint_reg
                .0
                .entry(cutout.id())
                .or_insert_with(|| anim.clone());
            tint_reg.0.entry(blend.id()).or_insert_with(|| anim.clone());
        }
        // `MeshTag` is the per-instance lighting/fade scalar: an interior prop carries its SH-probe
        // slot index here (the shader evaluates the folded probe instead of the sky base);
        // everything else carries the distance-fade alpha (`1.0` = opaque, the no-fade default the
        // fade system overwrites). The slot goes through the ONE typed constructor
        // (`mesh_tag::probe_bits` — bits 16-29 since the 0355 re-lane): this site kept the old
        // bits-0..=15 write through that re-lane, so every static interior prop read probe slot 0,
        // taking whichever probe won the streaming race — the director's inn-doodad regression.
        let mesh_tag = match interior_slot {
            Some(slot) if interior_probe => MeshTag(crate::mesh_tag::probe_bits(slot)),
            _ => MeshTag(alpha_bits(1.0)),
        };
        // A billboard batch (glow card / chain) faces the camera each frame, so its transform is owned
        // by the billboard system. It still distance-fades with its doodad (same `radius` band) — the
        // fade only touches the material/`MeshTag`, not the transform — so the bright halo culls when the
        // lamp does instead of hanging in the air far past it. The mesh is centred at the pivot, so the
        // fade centre is `ZERO` (→ the entity's world translation = the pivot).
        let (entity, fade_center) = if let Some(info) = &sub.billboard {
            // An animated doodad's card rides its billboard bone's live anchor (the swinging
            // lamp's glow follows the swing — 0153 follow-up; the anchor frame bakes the pivot,
            // the 0130 rig identity carried into the collapsed lane by 1365). A static doodad
            // keeps the fixed placement-baked pivot.
            let card = match host.as_mut().and_then(|h| h.anchor(commands, info.bone)) {
                Some(anchor) => BillboardCard::following_joint(info, anchor),
                None => match card_owner {
                    Some(anchor) => BillboardCard::following(info, anchor),
                    None => BillboardCard::new(info, transform),
                },
            };
            let mut card_entity = commands.spawn((
                Mesh3d(stat_mesh.clone()),
                MeshMaterial3d(cutout.clone()),
                Transform::from_translation(transform.transform_point(info.pivot)),
                ModelPart {
                    kind,
                    blend: sub.blend,
                },
                // The picker's triangles (decision 0857): the render forms are `RENDER_WORLD`-only,
                // so the inspector/probe rays read the model's resident geometry. The caster
                // centres a card at its pivot, the same bake the render form draws with.
                crate::interact::PickMesh(sub.geometry.clone()),
                mesh_tag,
                card,
            ));
            // The build-time Aabb, inserted explicitly: the static form is `RENDER_WORLD`-only,
            // so Bevy's `calculate_bounds` can race extraction — and the exterior cull fails
            // OPEN on a missing bound (0832's rule, extended to the model lane). `NoAutoAabb`
            // says the bound is OURS: see the ordinary-part insert below for what derives it
            // otherwise, and what that cost.
            if let Some(aabb) = stat_aabb {
                card_entity.insert((*aabb, NoAutoAabb));
            }
            (card_entity.id(), Vec3::ZERO)
        } else {
            // An animated doodad's ordinary submesh spawns on the STATIC form with its skinned
            // twin waiting beside it (`SkinnedTwin`): the palette slot no longer exists at spawn
            // — the draw gate allocates it at the placement's first wake and swaps the mesh in
            // (decision 0863, `doodad_anim::lazy`). The twin comes from the app-built forms
            // (0834); a lane that didn't request it (or a contract break) simply never promotes,
            // rather than arm the picker with a joint-less "skinned" mesh.
            let skinned_mesh = animated
                .then(|| forms.skin.and_then(|s| s.get(batch_idx)).cloned())
                .flatten();
            let mut part_entity = commands.spawn((
                Mesh3d(stat_mesh.clone()),
                MeshMaterial3d(cutout.clone()),
                transform,
                ModelPart {
                    kind,
                    blend: sub.blend,
                },
                // The picker's triangles (decision 0857) — same rule as the card above.
                crate::interact::PickMesh(sub.geometry.clone()),
                mesh_tag,
            ));
            // Every part now spawns static, so every part carries the build-time Aabb (the
            // RENDER_WORLD rule above).
            //
            // For an ANIMATED placement that bind-pose bound is a lie, because the joints move the
            // vertices while this entity's transform stays at the placement origin. Widen to the
            // model's authored all-animation box (decision 1259) — the union, never the replacement,
            // so the 152 corpus models whose authored box does not fully contain their own bind pose
            // still bound their geometry. A billboard card is exempt: its entity transform FOLLOWS
            // its joint every frame, so its own small bound travels with what it draws.
            //
            // **`NoAutoAabb` is what makes that bound survive** (decision 1261). Bevy's
            // `calculate_bounds` runs TWO queries: one that inserts a bound where there is none,
            // and one that **overwrites an existing bound** on `Changed<Mesh3d>`. The lazy rig
            // swaps `Mesh3d` static → skinned at this placement's first draw-gate wake
            // (`doodad_anim::lazy`), and the skinned twin shares the BIND-POSE geometry — so the
            // second query recomputed the very box 1259 had just widened, and the birds went back
            // to blinking a second after they streamed in. The bound here is authored, not derived;
            // this component is how that is said.
            if let Some(aabb) =
                cull_bound(stat_aabb.as_ref(), animated.then_some(anim_bound).flatten())
            {
                part_entity.insert((aabb, NoAutoAabb));
            }
            if let Some(sm) = skinned_mesh {
                part_entity.insert(crate::doodad_anim::SkinnedTwin {
                    skinned: sm,
                    stat: stat_mesh.clone(),
                });
                lazy_parts.push(part_entity.id());
            }
            let entity = part_entity.id();
            if let Some(root) = rig_root {
                commands
                    .entity(entity)
                    .insert(crate::rig_palette::RigPart(root));
                // The draw-gate list is about visibility, not skinning — the bind-pose fallback
                // part still represents the placement.
                skinned_meshes.push(entity);
            }
            (entity, local_center)
        };
        // Animated material alpha (decision 0130 phase 2): the rare batch whose colour-alpha/weight
        // tracks animate (fire flicker) or constantly dim gets its per-instance sampler; the
        // visibility authority composes the value into the render-alpha tag + the A ≤ 0 cull.
        // Captures freeze the clock at 0 (deterministic frames). A lit interior prop composes the
        // same way since the 0355 re-lane gave the probe-slot payload its own alpha field (bits
        // 0..=15) — the 0130-era "partial alpha can't show on the colour payload" deferral is
        // collected (bug B30: the Undercity lightshaft's authored 0.10/0.05 dimming).
        if let Some(anim) = &sub.alpha_anim {
            commands
                .entity(entity)
                .insert(crate::doodad_anim::MatAnim::new(
                    anim.clone(),
                    m2.map(|(_, now)| now).unwrap_or_default(),
                    mat_frozen,
                ));
        }
        // Distance fade for everything except a lit interior prop (whose `MeshTag` carries colour, and
        // which is steady indoors). Adding `DoodadFade` is what makes the fade system drive the tag.
        if !steady_interior_prop {
            commands.entity(entity).insert(DoodadFade {
                radius,
                local_center: fade_center,
                cutout,
                blend,
            });
        }
        // An entity-following card manages its own lifecycle (`face_billboards` despawns it with
        // its owner) and must stay a world root — keep it out of the caller's parent/despawn list.
        if sub.billboard.is_none() || card_owner.is_none() {
            out.push(entity);
        }
    }
    // Arm the draw gate: animation runs iff any of these submeshes is drawn (`doodad_anim`) —
    // or, for a meshless (particles-only) host, iff the placement's fade sphere is in the draw
    // set (the same 0171 law its emitters gate on).
    let arm = host.as_ref().and_then(|h| h.seq.map(|_| h.root));
    let placement_host = host.map(|mut h| {
        let now = m2.map(|(_, now)| now).unwrap_or_default();
        // Pre-mint the fx consumers' anchors (decision 1365) while the pose buffer is still in
        // hand: exactly the bones the model authors emitters/ribbons on, nothing speculative —
        // the fx spawns that run after this only look the entities up ([`PlacementHost::anchor`]).
        if let Some((m, _)) = m2 {
            for bone in m
                .emitters
                .iter()
                .map(|e| e.def.bone)
                .chain(m.ribbons.iter().map(|r| r.def.bone))
            {
                h.anchor(commands, bone);
            }
        }
        commands.entity(h.root).insert(DoodadAnimHost {
            meshes: skinned_meshes,
            fade: (radius, transform.transform_point(local_center)),
            clip: h.clip,
            armed_at: now,
            // Born already expired, so the first frame runs the holder setup's `variationIdx = -1`
            // arm over the loader's var-0 seed — the reference's own two-stage load (decision 0768).
            window_hi: f32::NEG_INFINITY,
            anim_id: h.anim_id,
            // Born PARKED (decision 0863): the spawn frame's `Visibility` is the default-visible
            // lie (the fade/cull authorities haven't classified the fresh parts yet), and the
            // gate's promote requires a drawn frame ON TOP of `active` — so the first real
            // verdict, not the spawn race, decides the slot. The gate flips this true the same
            // first pass either way, so pose/resume semantics are unchanged.
            active: false,
            parked_at: now,
        });
        // The lazy palette rig (decision 0863): what the draw gate's first wake allocates and
        // promotes. Only when the placement has skinnable parts at all — an emitter-only host
        // (chimney smoke) never takes a slot, which under the eager design it wasted one on.
        if !lazy_parts.is_empty() {
            commands.entity(h.root).insert(crate::doodad_anim::LazyRig {
                bones: h.bones(),
                ibp: h.inverse_bindposes.clone(),
                parts: lazy_parts,
            });
        }
        let root = h.root;
        let anchors = h.finish(commands);
        PlacementHost { anchors, root, arm }
    });
    (out, placement_host)
}

/// The `Aabb` a placed submesh is culled with: its build-time bind-pose bound, widened by the
/// model's authored all-animation box when this placement animates (decision 1259).
///
/// It is a **union**, not a swap, for one measured reason: 152 of the 9315 M2s that carry geometry
/// author a header box that does not fully contain their own bind-pose vertices (worst case
/// `Spells\Teleport.m2`, 19.9 yd short). The reference never notices — it tests the box's
/// *circumsphere*, which swallows the shortfall — so a bare swap would be the one way this change
/// could cull geometry the reference draws.
fn cull_bound(stat: Option<&Aabb>, anim: Option<Aabb>) -> Option<Aabb> {
    match (stat, anim) {
        (Some(s), Some(a)) => Some(Aabb::from_min_max(
            Vec3::from(s.min().min(a.min())),
            Vec3::from(s.max().max(a.max())),
        )),
        (Some(s), None) => Some(*s),
        (None, a) => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug's own numbers (`benilla-extract animboundscan`, `World\critter\birds\Bird01.m2`):
    /// a 1.2 × 1.8 × 0.23 yd bind-pose box, and an authored all-animation box 67 × 18 × 7 yd that
    /// reaches ~37 yd past it. Culled by the bind pose, the bird is dropped while its drawn
    /// geometry is still on screen.
    #[test]
    fn an_animated_placements_bound_covers_the_whole_flight_path() {
        // Bevy-space conversions of Bird01's two boxes (WoW (x,y,z) -> Bevy (-y, z, -x)).
        let bind = Aabb::from_min_max(
            Vec3::new(-0.863, 9.359, -3.536),
            Vec3::new(0.915, 9.586, -2.366),
        );
        let authored = Aabb::from_min_max(
            Vec3::new(-4.219, 8.816, -30.547),
            Vec3::new(13.571, 16.019, 36.605),
        );
        let widened = cull_bound(Some(&bind), Some(authored)).expect("a bound");
        // Every corner of the authored box is inside the widened bound. `Aabb` stores
        // centre/half-extents, so the reconstructed corners carry a float epsilon — hence the
        // tolerance, which is ~7 orders of magnitude below anything a cull can see.
        const EPS: f32 = 1e-4;
        assert!((widened.min() - EPS).cmple(authored.min()).all());
        assert!((widened.max() + EPS).cmpge(authored.max()).all());
        // …and the bind-pose bound alone was ~37 yd short of it along the flight axis.
        assert!(authored.max().z - bind.max().z > 36.0);
    }

    /// The union arm, not a swap: an authored box that fails to contain the bind pose (152 corpus
    /// models do) must not shrink the bound.
    #[test]
    fn a_short_authored_box_never_shrinks_the_bound() {
        let bind = Aabb::from_min_max(Vec3::splat(-5.0), Vec3::splat(5.0));
        let authored = Aabb::from_min_max(Vec3::new(-40.0, -1.0, -1.0), Vec3::new(40.0, 1.0, 1.0));
        let b = cull_bound(Some(&bind), Some(authored)).expect("a bound");
        assert!(Vec3::from(b.min()).abs_diff_eq(Vec3::new(-40.0, -5.0, -5.0), 1e-4));
        assert!(Vec3::from(b.max()).abs_diff_eq(Vec3::new(40.0, 5.0, 5.0), 1e-4));
    }

    /// A static placement is untouched — it keeps the tighter per-batch bind-pose bound.
    #[test]
    fn a_static_placement_keeps_its_per_batch_bound() {
        let bind = Aabb::from_min_max(Vec3::splat(-1.0), Vec3::splat(1.0));
        let b = cull_bound(Some(&bind), None).expect("a bound");
        assert!(Vec3::from(b.min()).abs_diff_eq(Vec3::splat(-1.0), 1e-4));
        assert!(Vec3::from(b.max()).abs_diff_eq(Vec3::splat(1.0), 1e-4));
    }
}
