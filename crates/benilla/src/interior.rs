//! Interior lighting classification for ENTITIES: which unit/GameObject M2s stand inside a WMO room.
//!
//! ONE law lights every indoor entity M2 (wow-re `unit-m2-shader-light.md`, the Goldshire-inn
//! capture's byte-arbitrated trio — superseding `wmo-lit-selector.md` §3.3's class split):
//!
//! - **Every entity M2** — unit, player, GameObject, held item — is registered with the same
//!   entity-node fill (`Node::SetModel 0x6716f0` ← the model setters, dispatched `0x672a20`): its
//!   env-update attach down-rays the WMO render mesh under the entity and bakes the hit's
//!   barycentric MOCV — **floor-168/cap-96** — as a directional on the fixed interior axis, plus
//!   the hit group's MOLR point lobes. Decoded live at machine zero off the abbey INNBENCH draws
//!   (GameObject) and bit-exact off an inn character draw (unit). Indoors we fold that into an SH
//!   probe ([`InteriorKind::Bake`]) — the same probe table the MODD props ride.
//! - The raw **day/night pair at gain 1.0** is the *null-node fallback* (`0x672a2f`: a model whose
//!   node isn't registered — and our lane when the footprint ray misses or hits a MOPY&1 face).
//!   The abbey capture's flat-lit characters were this state, not a character-path law; the pair
//!   itself is NEVER indoor-modified. [`InteriorKind::Matte`] keeps it for bake-less parts.
//!
//! One SHADING law, **two ATTACHES.** The law above is type-blind; how a node *finds* its group is
//! not. `0x6a86d0` forks on `[node+0x90]` bit 13 (`6a8714 test ah,0x20`), written at node creation
//! from the descriptor TYPEMASK — units/players/doodads take the down-ray attach `0x6a8a20` from
//! the node POSITION, **GameObjects take the containment attach `0x6a8c10` from the node's world
//! bounding-box CENTRE** (`[node+0x5c]`), whose face query retries UPWARD on a miss. There is no
//! subclass test anywhere in the dispatch: the mode bit is the whole fork. We carry it as
//! [`ContainmentAttach`] on the anchor and as [`crate::wmo_portal::LightAttach`] through the
//! verdict. Decision 0776 — a GameObject's origin routinely sits at or below its own floor, and
//! sharing the unit down-ray put those objects outdoors in the middle of a building.
//!
//! Entities move and stream independently of their building, so [`classify_entity_interior`]
//! re-tests them against the placed WMOs. The indoor test is the client's own: a faces-only
//! ray from the attach's anchor onto the placed groups' geometry
//! ([`crate::wmo_portal::indoor_verdict_at`] — the LIGHTING-class fork `[node+0xc]`, outdoor iff
//! the hit group's `MOGI & 0x48` — NOT the zone-text `[node+0x90]` bit-0 predicate, which keys on
//! `0x8` alone and so calls the `0x40`-only city street groups "indoors"; decision 0475 — and an
//! outdoor-class WMO surface forces the LIT target, no MCSH beneath the building: the WMO-linked
//! skip-shadow bit, byte-verified; decisions 0477/0480). One verdict per UNIT, sampled at its
//! [`InteriorLit::anchor`] — a body's parts must never split across light laws (group bounding
//! boxes did exactly that at floor level; director-caught, 2026-07-12), and a held/equipped item
//! M2 anchors at its WEARER's root, never its own carried position: the reference aliases the
//! wearer's light collector into each item by pointer (`[item+0x3b8]=[wearer+0x3b8]`, `0x718960` —
//! `unit-light-combine-storm.md`; a hand-anchored shield split from its body, director-caught,
//! 2026-07-13).

use std::collections::HashSet;

use benilla_assets::{cap96, floor168, AdtTile, WmoModel};
use bevy::asset::AssetId;
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::relationship::RelationshipTarget as _;
use bevy::ecs::world::DeferredWorld;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::lighting::{PropProbeSlot, PropProbes};
use crate::model_fade::{PendingAppearFade, RenderFade};
use crate::terrain::WowModelMaterial;
use crate::terrain_stream::{fold_interior_probe, PropLobeLight, TerrainStreamer};
use crate::wmo_portal::{indoor_verdict_at, IndoorVerdict, WmoPortalInstance};

/// Squared distance (yd²) an entity must move before it's re-tested: an epsilon — the reference
/// runs the classify + footprint chain EVERY frame for units (the node is unlinked, so the
/// WorldFrame ramp tail reaches `0x69e280` per tick; wow-re `unit-light-combine-storm.md` c1), so
/// a moving entity re-samples per frame and the continuous MOCV field never quantizes into steps
/// (the 0.5-yd gate here was the forge's per-step light flash). A standing entity still costs one
/// position compare and nothing else.
const RESAMPLE_DIST_SQ: f32 = 1e-4;

/// Which WMO placements are resident — a generation counter the classifier re-evaluates entities on
/// (a building streaming in under a standing NPC must re-light it even though it didn't move).
/// Rebuilt each frame by the streamer ([`crate::terrain_stream`]) from its live placements; the
/// down-ray itself reads the live [`WmoPortalInstance`]s, so this only carries the change signal.
#[derive(Resource, Default)]
pub(crate) struct WmoResidency {
    resident: HashSet<AssetId<WmoModel>>,
    generation: u32,
}

impl WmoResidency {
    /// The change counter — bumped whenever the resident set actually changes. Read by the per-unit
    /// room claim (`wmo_portal::track_unit_interiors`), whose re-test gate is otherwise movement
    /// alone: a building streaming in under a STANDING unit must still re-claim it.
    pub(crate) fn generation(&self) -> u32 {
        self.generation
    }

    /// Replace the resident set, bumping the generation only when it actually changed
    /// (order-independent, by asset id) — so the per-frame rebuild doesn't defeat the classifier's
    /// movement gate.
    pub(crate) fn update(&mut self, next: impl IntoIterator<Item = AssetId<WmoModel>>) {
        let next_ids: HashSet<AssetId<WmoModel>> = next.into_iter().collect();
        if next_ids != self.resident {
            self.resident = next_ids;
            self.generation = self.generation.wrapping_add(1);
        }
    }
}

/// The indoor light LAW an entity's model takes — decided at build time by whether the part built
/// a bake variant (every M2 does; module docs).
#[derive(Clone)]
pub(crate) enum InteriorKind {
    /// The plain day/night matte at sun ×1.0 indoors — the reference's null-node fallback. Only
    /// for parts with no bake variant; the Bake law also lands here on a footprint miss.
    Matte,
    /// Every entity M2 (unit/player/GameObject/held): the footprint-MOCV bake indoors — `material`
    /// is the interior PROP-lane variant (the shader evaluates the model's SH probe by the
    /// `MeshTag` slot), `center` the M2 vertex-box centre in Bevy model-local (the fold's MOLR
    /// reference point, the byte-cited anchor family).
    Bake {
        material: Handle<WowModelMaterial>,
        center: Vec3,
    },
}

/// The interior-light membership of ONE M2 batch — the single place a spawn site decides whether
/// a batch joins its model's law, and under which [`InteriorKind`]. `None` classifies the batch
/// out entirely (a WMO-display part, which has no interior variant to swap to).
///
/// **Every batch of a model goes through this, cards included.** The reference has one light node
/// per object and every batch shades through the same node fill (wow-re `unit-m2-shader-light.md`);
/// a billboard BONE re-orients geometry, it does not re-route light. Our billboard batches spawn as
/// world ROOTS so the facing system can own their transform (0153) — an implementation detail that
/// must not reach the light, and which did while this policy was written out longhand at each spawn
/// site and one of them omitted it: a Stratholme hanging sign baked from the room it hangs in while
/// its own chain cards stayed on the exterior material and lit from the sky (decision 0778).
///
/// `anchor` is the model's NET ENTITY root for every caller — body mesh, held item, and card alike
/// — so a model can never split across the two light laws ([`BodyBakeCenter`] for why an item
/// aliases its wearer rather than folding from its own position). Each site still writes its own
/// [`MeshTag`](bevy::mesh::MeshTag): the rig slot and the fade alpha are that site's to seed, and
/// the classifier composes into them.
pub(crate) fn part_interior_lit(
    exterior: &Handle<WowModelMaterial>,
    interior: Option<&Handle<WowModelMaterial>>,
    bake: Option<&Handle<WowModelMaterial>>,
    center: Vec3,
    anchor: Entity,
) -> Option<(InteriorLit, ClassifiedBy)> {
    interior?;
    let kind = match bake {
        Some(material) => InteriorKind::Bake {
            material: material.clone(),
            center,
        },
        None => InteriorKind::Matte,
    };
    Some((
        InteriorLit::new(kind, exterior.clone()),
        ClassifiedBy(anchor),
    ))
}

/// The law a part currently renders under (`None` until first classified).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AppliedLaw {
    Exterior,
    /// Indoors on the plain matte (a footprint ray that missed or hit a MOPY&1 face, or a
    /// bake-less part — the day/night null-node lane).
    Matte,
    /// Indoors on the footprint bake, evaluated from this probe-table slot.
    Bake(u16),
}

/// The unit's own body-model bake centre (M2 vertex-box centre, model-local) on the net entity
/// ROOT — the interior fold's MOLR reference point for EVERY part that shares the root's verdict,
/// held items included. The reference has exactly one light node per unit; an equipped item M2
/// aliases the wearer's collector by pointer (`[item+0x3b8]=[wearer+0x3b8]`, `0x718960` — wow-re
/// `unit-light-combine-storm.md`), so an item never folds from its own carried position.
///
/// For a [`ContainmentAttach`] anchor it is also the **attach anchor** — the reference's
/// `[node+0x5c]`, the same world point (decision 0776).
#[derive(Component, Clone, Copy)]
pub(crate) struct BodyBakeCenter(pub(crate) Vec3);

/// This anchor's light node runs the **containment attach** (`0x6a8c10`), not the down-ray one —
/// the reference's `[node+0x90]` bit 13, set at node creation from the descriptor TYPEMASK
/// (`0x613e10`/`0x670db0`) and dispatched at `0x6a86d0`. In 1.12 exactly one object class carries
/// it: **GameObjects**. See [`crate::wmo_portal::LightAttach`] for the two lanes at the bytes, and
/// decision 0776 for what it corrects — a GameObject's origin is frequently at or below its own
/// floor (a Stratholme portcullis spawns 15 cm under the corridor slab), so the down-ray this lane
/// used to share left it outside the building on the outdoor light while its identical neighbour
/// two doors down baked from the same floor.
#[derive(Component)]
pub(crate) struct ContainmentAttach;

/// The part → anchor edge of the classifier's registry (0734): every [`InteriorLit`] part names
/// its NET ENTITY root here — body parts and held/equipped items alike (module docs — the
/// reference has one light node per unit and items alias it). Bevy's relationship hooks maintain
/// the anchor-side [`LitParts`] list through spawn, gear-swap despawn, and teardown, so a law
/// change can write exactly its own parts and a settled anchor touches none.
#[derive(Component)]
#[relationship(relationship_target = LitParts)]
pub(crate) struct ClassifiedBy(pub(crate) Entity);

/// The anchor-side part list [`ClassifiedBy`] maintains — the classifier's write fan-out. Never
/// mutated by hand; bevy removes it when the last part leaves.
#[derive(Component)]
#[relationship_target(relationship = ClassifiedBy)]
pub(crate) struct LitParts(Vec<Entity>);

/// The anchor's classification record (0734) — the law its parts render under, plus the
/// movement/residency gate that used to live per part. Inserted by the classifier on the first
/// resolve; a settled anchor is one distance compare per frame, whatever its part count.
#[derive(Component)]
pub(crate) struct InteriorAnchor {
    law: AppliedLaw,
    /// Anchor position at the last down-ray + the residency generation then — the re-test gate.
    last_pos: Vec3,
    generation: u32,
    /// Whether the law was resolved from a bake-capable part's kind — a bake-capable part
    /// joining a matte-resolved anchor must force a re-resolve (the reauthor drain checks this),
    /// or it would ride the matte fallback until the anchor next moves.
    kind_bake: bool,
}

impl InteriorAnchor {
    /// One line naming the lane this anchor's parts render under — the inspect card's light
    /// readout (`crate::interact`). "Which lane is this object on?" was the exact question decision
    /// 0776 was found by, and answering it took a rebuild with `WOW_INTERIOR_LOG` plus an offline
    /// `WOW_LIGHT_AT` probe; on the card it is a hover. Reads "exterior", "interior day/night", or
    /// "interior bake (probe N)".
    pub(crate) fn law_label(&self) -> String {
        match self.law {
            AppliedLaw::Exterior => "exterior".into(),
            AppliedLaw::Matte => "interior day/night".into(),
            AppliedLaw::Bake(slot) => format!("interior bake (probe {slot})"),
        }
    }
}

/// Parts whose material/tag need re-authoring from their anchor's current law — the classifier's
/// convergence queue (0734), replacing the per-part sweep's repair duty. Fed by the
/// [`InteriorLit`] `on_add` hook (a fresh part joining a settled anchor), the fade-latch observer
/// ([`enqueue_on_fade_latch`] — a part re-entering the write query after a fade owned its
/// channel), and the self-avatar zoom feather's release edge. Drained every classifier run;
/// entries whose part is still excluded (or gone) are dropped — the next edge re-enqueues.
#[derive(Resource, Default)]
pub(crate) struct InteriorReauthor(pub(crate) Vec<Entity>);

/// The interior/exterior material variants for one entity submesh part, so [`classify_entity_interior`]
/// can swap by the model's current location without rebuilding. Attached only to M2 entity parts (WMO
/// group geometry carries per-submesh interior in its own material + baked MOCV); its anchor edge
/// is the sibling [`ClassifiedBy`]. The `on_add` hook enqueues the part for authoring, so a
/// gear-swap part joining an already-settled anchor still gets the standing law.
#[derive(Component)]
#[component(on_add = enqueue_new_part)]
pub(crate) struct InteriorLit {
    /// The model's indoor law ([`InteriorKind`]) — uniform across an anchor's parts.
    kind: InteriorKind,
    /// The exterior/day-night material (the global-SH lane): since 0354 the Matte law rides it
    /// too — day/night is the intensity byte at the 1.0 point, not a separate material.
    exterior: Handle<WowModelMaterial>,
    /// Last applied law — the part's last-written record (the anchor's [`InteriorAnchor`] is the
    /// authority): the write gate, and [`Self::is_bake`]'s source. `None` until first written.
    applied: Option<AppliedLaw>,
}

impl InteriorLit {
    /// Whether this part currently rides the footprint-BAKE lane — the intensity-byte writer's
    /// skip test: a bake part's tag payload is its probe SLOT, so [`crate::entity_shade`] must not
    /// write the shade byte over it (every other law carries the byte — since 0354 the day/night
    /// state is the byte at the intensity-1.0 point, not a material swap).
    pub(crate) fn is_bake(&self) -> bool {
        matches!(self.applied, Some(AppliedLaw::Bake(_)))
    }

    /// The steady (non-feathering) material for the part's CURRENT law — the classifier's own
    /// choice, exposed so the fade writers settle a part onto exactly what the classifier would
    /// have written rather than onto a `cutout` latched before the law was known (decision 0755).
    ///
    /// The exterior and day/night (Matte) states share the exterior material: since 0354 the
    /// difference between them is the node's intensity target (the tag byte `entity_shade` ramps),
    /// not a separate material. Only the footprint bake swaps the variant — and a matte-KIND part
    /// under a bake-law anchor has no bake variant to swap to, so it keeps the exterior one.
    pub(crate) fn steady_material(&self) -> &Handle<WowModelMaterial> {
        match (self.applied, &self.kind) {
            (Some(AppliedLaw::Bake(_)), InteriorKind::Bake { material, .. }) => material,
            _ => &self.exterior,
        }
    }

    /// Re-point a standing part's law variants at a freshly-built material set, and answer what it
    /// should now draw — the character **re-dress** (`entities::attach::redress`): a gear change
    /// re-composites the body atlas and re-resolves the cape texture, so every variant's handle
    /// changes while the part, the room and therefore the *law* stay exactly as they were.
    ///
    /// The applied law is deliberately KEPT, which is what makes this a re-point rather than a
    /// re-classification: nothing moved, so re-running the down-ray would answer the same thing at
    /// the cost of a frame on the wrong material. A part's `kind` cannot change under it either —
    /// bake-capability is a property of the batch's build, and a re-dress never changes which batch
    /// a part is.
    pub(crate) fn repoint(
        &mut self,
        exterior: &Handle<WowModelMaterial>,
        bake: Option<&Handle<WowModelMaterial>>,
    ) -> &Handle<WowModelMaterial> {
        self.exterior = exterior.clone();
        if let (InteriorKind::Bake { material, .. }, Some(b)) = (&mut self.kind, bake) {
            *material = b.clone();
        }
        self.steady_material()
    }

    pub(crate) fn new(kind: InteriorKind, exterior: Handle<WowModelMaterial>) -> Self {
        Self {
            kind,
            exterior,
            applied: None,
        }
    }

    /// Test-only: a part already recorded as riding the Bake law, so
    /// [`crate::model_fade::FadeMaterials::material_for`] can be exercised over every law without
    /// driving a full classifier run from another module ([`AppliedLaw`] is this module's business
    /// and stays private).
    #[cfg(test)]
    pub(crate) fn applied_bake_for_test(
        kind: InteriorKind,
        exterior: Handle<WowModelMaterial>,
    ) -> Self {
        Self {
            kind,
            exterior,
            applied: Some(AppliedLaw::Bake(0)),
        }
    }
}

/// `on_add` hook: a freshly spawned part asks for its anchor's standing law (drained by
/// [`classify_entity_interior`] — a first-resolving anchor covers its parts anyway, but a part
/// joining a SETTLED anchor gets no law-change write without this).
fn enqueue_new_part(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(mut queue) = world.get_resource_mut::<InteriorReauthor>() {
        queue.0.push(ctx.entity);
    }
}

/// Fade latch: re-author the part from its anchor's law the moment `RenderFade` leaves.
///
/// Since 0755 this is a **backstop**, not the convergence path it once was — the classifier now
/// tracks a part's law right through its ramp, and `apply_render_fade` settles the material onto
/// that law itself at the latch, so the forced re-author is normally a no-op. It stays because its
/// failure mode is the "unit stays black indoors" bug (a probe slot freed while its part sat
/// unauthored), which has cost us twice, and one forced write per part at the end of a ramp is
/// nothing. Removal on despawn also lands here; the drain drops dead entries.
fn enqueue_on_fade_latch(
    fade_end: On<Remove, RenderFade>,
    parts: Query<(), With<InteriorLit>>,
    mut queue: ResMut<InteriorReauthor>,
) {
    if parts.contains(fade_end.entity) {
        queue.0.push(fade_end.entity);
    }
}

/// Registers the residency registry, the reauthor queue + its fade-latch observer, and the
/// per-frame entity classifier (the streamer fills the registry).
pub(crate) struct InteriorPlugin;

impl Plugin for InteriorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WmoResidency>()
            .init_resource::<InteriorReauthor>()
            .add_systems(Update, classify_entity_interior)
            .add_observer(enqueue_on_fade_latch);
    }
}

/// The bake fold's cached ray products, on the ANCHOR (net entity root): while the node's ramps
/// move without the entity moving (it stopped just inside the forge's warm zone), the per-frame
/// refold re-uses these instead of re-running the down-ray. Written on every ray that lands a
/// Baked verdict; removed with the law.
#[derive(Component)]
pub(crate) struct BakeState {
    /// floor-168 of the footprint MOCV word (0..1) — the fold's diffuse, × the node intensity.
    word: Vec3,
    /// The hit group's windowed MOLR lobes (world space, pre-gained).
    lobes: Vec<PropLobeLight>,
    /// The fold's MOLR reference point (world space) at the last ray.
    ref_point: Vec3,
}

/// Light each entity part by where its model stands. Outside ⇒ the exterior lane (the global SH ×
/// the ramped intensity byte). Inside a WMO room ⇒ the footprint-MOCV bake folded into the
/// anchor's OWNED SH probe (refolded per frame while the node moves or its ramps chase — the
/// reference's per-tick env update, decision 0354), or the day/night state = the same exterior
/// material at the intensity-1.0 byte point. One law for every entity M2, unit and GameObject
/// alike (module docs). The verdict is the client's faces-only down-ray at the model's anchor —
/// one ray per UNIT per re-test, re-run only when the anchor moves or a building streams in/out.
///
/// The walk is over ANCHORS, not parts (0734): a settled anchor is one distance compare, whatever
/// its part count, and parts are written only when their anchor's law changes (or through the
/// [`InteriorReauthor`] drain — a fresh part, a fade latch, the zoom feather's release).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn classify_entity_interior(
    mut commands: Commands,
    time: Res<Time>,
    residency: Res<WmoResidency>,
    wmos: Res<Assets<WmoModel>>,
    instances: Query<&WmoPortalInstance>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    lighting: Res<crate::lighting::WowLighting>,
    mut probes: ResMut<PropProbes>,
    mut anchors: Query<(
        Entity,
        &GlobalTransform,
        &LitParts,
        Option<&mut InteriorAnchor>,
        Has<ContainmentAttach>,
        Option<&BodyBakeCenter>,
    )>,
    mut nodes: Query<&mut crate::entity_shade::GroundShade>,
    bake_states: Query<&BakeState>,
    seats: Query<&PropProbeSlot>,
    mut queue: ResMut<InteriorReauthor>,
    part_anchors: Query<&ClassifiedBy>,
    // Fading parts are **included** (decision 0755). The light law and the fade alpha are
    // orthogonal, and they are deconflicted by field, not by lockout: the classifier's payload
    // writes carry the tag's alpha field through (`mesh_tag::with_interior_probe` /
    // `with_exterior_reset`), and a fading part takes the BLEND twin of its law from the very
    // rule the ramp itself applies (`FadeMaterials::material_for`) — same handle, either order,
    // no fight. Excluding them is what left a streamed indoor entity with no law at all for the
    // whole 2 s ramp, so it appeared under exterior light and swapped to its room's in one frame
    // at the latch.
    mut parts: Query<(
        &mut InteriorLit,
        &mut MeshMaterial3d<WowModelMaterial>,
        &mut MeshTag,
        Option<&crate::model_fade::FadeMaterials>,
        Has<RenderFade>,
        Has<PendingAppearFade>,
    )>,
) {
    let _t0 = std::time::Instant::now();
    let (mut n_anchors, mut n_resolved, mut n_written) = (0usize, 0usize, 0usize);
    // Anchors that wanted a resolve but had every part fade-excluded — the appear-fade lockout.
    let mut n_fade_blocked = 0usize;
    let mut resolve_us = 0.0f32;
    for (anchor, anchor_t, lit_parts, mut state, containment, bake_center) in &mut anchors {
        n_anchors += 1;
        let pos = anchor_t.translation();
        let had_state = state.is_some();
        // Skip the down-ray entirely for a settled anchor (no movement, no building streamed) —
        // this is what keeps a town full of standing NPCs/props at one compare per frame. A
        // Bake-law anchor whose node ramps still chase keeps refolding (from the cached ray
        // products — no new ray), so a unit that stops just inside a warm zone finishes its
        // transition instead of freezing mid-ramp.
        if let Some(state) = state.as_deref_mut() {
            let settled = state.generation == residency.generation
                && pos.distance_squared(state.last_pos) < RESAMPLE_DIST_SQ;
            if settled {
                if let AppliedLaw::Bake(slot) = state.law {
                    if let (Ok(node), Ok(bake)) = (nodes.get(anchor), bake_states.get(anchor)) {
                        if !node.ramps_settled() {
                            let coeffs = fold_interior_probe(
                                node.ambient.to_array(),
                                (bake.word * node.intensity()).to_array(),
                                bake.ref_point,
                                &bake.lobes,
                            );
                            probes.update_owned(slot, coeffs);
                        }
                    }
                }
                continue;
            }
        }
        // Re-resolving: the kind comes from the anchor's first classifiable part. Since 0755 a
        // fading part is classifiable, so this only bails on an anchor whose parts have all been
        // despawned (a teardown mid-frame) — `fade_blocked` is kept as the tripwire that the
        // lockout has not crept back in.
        let Some(kind) = lit_parts
            .iter()
            .find_map(|part| parts.get(part).ok().map(|(lit, ..)| lit.kind.clone()))
        else {
            n_fade_blocked += 1;
            continue;
        };
        let seated = seats.get(anchor).ok().map(|s| s.0);
        n_resolved += 1;
        let _r = std::time::Instant::now();
        let (attach, attach_at) = attach_anchor(containment, bake_center, anchor_t);
        let law = resolve_anchor_law(
            &mut commands,
            &mut probes,
            &wmos,
            &instances,
            &streamer,
            &adt_tiles,
            &lighting,
            &mut nodes,
            anchor,
            anchor_t,
            attach,
            attach_at,
            &kind,
            seated,
        );
        resolve_us += _r.elapsed().as_secs_f32() * 1e6;
        let kind_bake = matches!(kind, InteriorKind::Bake { .. });
        let changed = match state.as_deref_mut() {
            Some(state) => {
                let changed = state.law != law;
                state.law = law;
                state.last_pos = pos;
                state.generation = residency.generation;
                state.kind_bake = kind_bake;
                changed
            }
            None => {
                // `try_insert`: the anchor may carry a same-frame despawn already queued.
                commands.entity(anchor).try_insert(InteriorAnchor {
                    law,
                    last_pos: pos,
                    generation: residency.generation,
                    kind_bake,
                });
                true
            }
        };
        // Write the parts only when the law actually changed, so re-testing a moving NPC mid-room
        // doesn't churn the render extraction.
        if !changed {
            continue;
        }
        // `WOW_INTERIOR_LOG=1`: print interior classifications — the live-probe instrument for
        // "did this entity actually classify indoors, and under which law?". Scoped to interior
        // verdicts (plus interior→exterior flips) so the world's exterior masses stay silent. The
        // ATTACH and the point it probed are printed too (0776): a line that says only "exterior"
        // can't be read without knowing which lane produced it, and the two lanes now probe
        // different points.
        if (law != AppliedLaw::Exterior || had_state)
            && std::env::var_os("WOW_INTERIOR_LOG").is_some()
        {
            // The PART COUNT is load-bearing, not decoration: a lane readout says which law the
            // model took, never which of its batches actually joined. A billboard card that
            // silently classified out is invisible to every other reading of this line — the
            // Stratholme sign baked correctly *and* its chains lit from the sky, and the anchor
            // log said only "INTERIOR bake" for weeks (decision 0778). Compare it against the
            // model's batch count (`benilla-extract m2batch <model>`).
            eprintln!(
                "[interior] t {:.2} anchor {anchor:?} ({} parts) at ({:.1}, {:.1}, {:.1}) {} probe \
                 ({:.1}, {:.1}, {:.1}) -> {}",
                time.elapsed_secs(),
                lit_parts.len(),
                pos.x,
                pos.y,
                pos.z,
                match attach {
                    crate::wmo_portal::LightAttach::Containment => "containment",
                    crate::wmo_portal::LightAttach::DownRay => "down-ray",
                },
                attach_at.x,
                attach_at.y,
                attach_at.z,
                match law {
                    AppliedLaw::Exterior => "exterior".to_string(),
                    AppliedLaw::Matte => "INTERIOR matte".to_string(),
                    AppliedLaw::Bake(s) => format!("INTERIOR bake slot {s}"),
                }
            );
        }
        for part in lit_parts.iter() {
            if let Ok((mut lit, mut material, mut tag, fm, ramping, pending)) = parts.get_mut(part)
            {
                let fade = fm.filter(|_| ramping || pending);
                n_written += usize::from(write_part_law(
                    law,
                    &mut lit,
                    &mut material,
                    &mut tag,
                    false,
                    fade,
                ));
            }
        }
    }
    // Drain the convergence queue: each entry re-authors from its anchor's standing law. Forced
    // through the part's change gate — the enqueuing edges (fade latch, zoom release) mean a
    // transient author overwrote the material/tag while `applied` stayed current.
    for part in std::mem::take(&mut queue.0) {
        let Ok(edge) = part_anchors.get(part) else {
            continue; // despawned since enqueue
        };
        let Ok((_, _, _, mut state, ..)) = anchors.get_mut(edge.0) else {
            continue;
        };
        let Some(state) = state.as_deref_mut() else {
            continue; // law not resolved yet — the anchor's first resolve writes every part
        };
        let Ok((mut lit, mut material, mut tag, fm, ramping, pending)) = parts.get_mut(part) else {
            continue; // despawned between the enqueue and the drain
        };
        let fade = fm.filter(|_| ramping || pending);
        // The mixed-kind hole: a bake-capable part joining an anchor whose law was resolved from
        // a matte-kind part must force a re-resolve (drop the record; next frame re-rays) — the
        // standing law can't say Bake. Written with the standing law this frame regardless, so
        // the part isn't naked for the gap.
        if matches!(lit.kind, InteriorKind::Bake { .. }) && !state.kind_bake {
            commands.entity(edge.0).try_remove::<InteriorAnchor>();
        }
        n_written += usize::from(write_part_law(
            state.law,
            &mut lit,
            &mut material,
            &mut tag,
            true,
            fade,
        ));
    }
    // `WOW_INTERIOR_COST=1`: this lane's per-frame cost, in the terms that diagnose it — how many
    // anchors the walk visits vs how many actually re-resolve vs how many PARTS got written, and
    // what the resolves cost. The split is the whole diagnosis (it sent the 2026-07-27 hunt into
    // the WMO column rays, decision 0711, and priced 0732's slice C — the 13.7k-part walk this
    // anchor walk replaced). Cheap enough to leave in: three counters and one `Instant` per frame.
    static COST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *COST.get_or_init(|| std::env::var_os("WOW_INTERIOR_COST").is_some()) {
        eprintln!(
            "[interior-cost] anchors={n_anchors} resolved={n_resolved} fade_blocked={n_fade_blocked} parts_written={n_written} resolve_ms={:.2} total_ms={:.2}",
            resolve_us / 1000.0,
            _t0.elapsed().as_secs_f32() * 1000.0
        );
    }
}

/// Write one part's material + tag for `law` — the single place a part's channel is authored.
/// Change-gated on the part's last-written record unless `force` (a transient author — the zoom
/// feather — overwrote the channel while `applied` stayed current). Returns whether it wrote.
///
/// The tag: the Bake law's payload carries the probe SLOT in its bits-6..=18 field; the other laws
/// reset to the plain exterior payload (shade byte 0 — `entity_shade` runs after the classifier and
/// re-asserts the ramped intensity byte the same frame; it skips only Bake parts). Both writes
/// carry the tag's **alpha** field through, so a law change lands cleanly mid-fade. BOTH indoor
/// laws carry the INTERIOR_FOG_BIT (Bake bakes it in): the reference fogs a unit by the unit's OWN
/// interior classification, so an indoor day/night character keeps the room's fog — never the
/// storm's near veil — while the exterior law returns it to the scene fog (wow-re
/// `m2-unit-interior-fog.md`; the director's corridor-vs-porch walk-out). Every arm carries the
/// part's rig field through (decision 0720): a skinned part keeps its palette across the
/// indoor/outdoor transition.
///
/// `fade` is `Some` only while an appear/despawn ramp owns the part (live **or** pending), and it
/// selects the law's BLEND twin instead of its steady material (decision 0755). It is the same
/// [`crate::model_fade::FadeMaterials::material_for`] rule the ramp itself applies every frame, so
/// the two writers produce the identical handle and can never fight, in either order.
///
/// Writing the material here rather than leaving it entirely to the ramp is what keeps the
/// **material mode and the tag payload mode naming the same law at every instant** — the invariant
/// `mesh_tag::describe` exists to catch a violation of (0355 broke exactly this way). Skipping it
/// would leave a part that classifies indoors while still *pending* carrying a probe slot on the
/// exterior material, where the shader decodes those bits as a ground-shade byte.
fn write_part_law(
    law: AppliedLaw,
    lit: &mut InteriorLit,
    material: &mut MeshMaterial3d<WowModelMaterial>,
    tag: &mut MeshTag,
    force: bool,
    fade: Option<&crate::model_fade::FadeMaterials>,
) -> bool {
    if lit.applied == Some(law) && !force {
        return false;
    }
    lit.applied = Some(law);
    let want = match fade {
        Some(fm) => fm.material_for(Some(&*lit), true).clone(),
        None => lit.steady_material().clone(),
    };
    if material.0 != want {
        material.0 = want;
    }
    tag.0 = match law {
        AppliedLaw::Bake(slot) => crate::mesh_tag::with_interior_probe(tag.0, slot),
        AppliedLaw::Matte => {
            crate::mesh_tag::INTERIOR_FOG_BIT | crate::mesh_tag::with_exterior_reset(tag.0)
        }
        AppliedLaw::Exterior => crate::mesh_tag::with_exterior_reset(tag.0),
    };
    true
}

/// The attach and its anchor point — one choice, because `0x6a86d0`'s mode fork picks both the
/// routine and the field it reads: a GameObject attaches by CONTAINMENT from its world
/// bounding-box centre (`[node+0x5c]`), everything else DOWN-RAYS from its position
/// (`[node+0xa8]`). Decision 0776.
fn attach_anchor(
    containment: bool,
    bake_center: Option<&BodyBakeCenter>,
    anchor_t: &GlobalTransform,
) -> (crate::wmo_portal::LightAttach, Vec3) {
    use crate::wmo_portal::LightAttach;
    match (containment, bake_center) {
        (true, Some(BodyBakeCenter(center))) => {
            (LightAttach::Containment, anchor_t.transform_point(*center))
        }
        // A containment anchor whose body model carries no bounds yet degrades to its origin — the
        // same point the down-ray lane uses, never a wrong one.
        (true, None) => (LightAttach::Containment, anchor_t.translation()),
        (false, _) => (LightAttach::DownRay, anchor_t.translation()),
    }
}

/// Resolve one anchor's indoor law: the down-ray verdict, the node's target/seed updates, and for
/// the Bake law the footprint fold into the anchor's OWNED probe slot. (The settled ramp-only
/// refold from the cached ray products lives in the caller's walk — this always rays.) `seated` —
/// the anchor's live [`PropProbeSlot`] — is the ONLY authority on that slot: Bake stays on it,
/// entry/exit is judged by it, and a part-cached `Bake(slot)` is never believed (a fresh part
/// once re-allocated here and freed the seated slot under the anchor's other parts — the
/// stuck-black-unit bug; the fade-latch reauthor is the other half). The slot component lives on
/// the ANCHOR — its on-remove hook frees the slot on despawn; law transitions remove/insert it
/// here.
#[allow(clippy::too_many_arguments)]
fn resolve_anchor_law(
    commands: &mut Commands,
    probes: &mut PropProbes,
    wmos: &Assets<WmoModel>,
    instances: &Query<&WmoPortalInstance>,
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    lighting: &crate::lighting::WowLighting,
    nodes: &mut Query<&mut crate::entity_shade::GroundShade>,
    anchor: Entity,
    anchor_t: &GlobalTransform,
    attach: crate::wmo_portal::LightAttach,
    attach_at: Vec3,
    kind: &InteriorKind,
    seated: Option<u16>,
) -> AppliedLaw {
    let verdict = indoor_verdict_at(
        wmos,
        instances.iter(),
        streamer,
        adt_tiles,
        attach_at,
        attach,
    );
    // Publish the outdoor GROUND kind to the node before the law resolves: standing on an
    // outdoor-class WMO surface (street/deck/porch) forces the lit 2.5 target — the WMO-linked
    // skip-shadow bit, byte-verified (0477/0480; `entity_shade` reads it).
    let on_wmo = matches!(verdict, IndoorVerdict::OutdoorsOnWmo);
    if let Ok(mut node) = nodes.get_mut(anchor) {
        if node.on_wmo != on_wmo {
            node.on_wmo = on_wmo;
        }
    }
    let law = match kind {
        InteriorKind::Matte => match verdict {
            IndoorVerdict::DayNight | IndoorVerdict::Baked { .. } => AppliedLaw::Matte,
            IndoorVerdict::Outdoors | IndoorVerdict::OutdoorsOnWmo => AppliedLaw::Exterior,
        },
        InteriorKind::Bake { center, .. } => {
            match verdict {
                IndoorVerdict::Outdoors | IndoorVerdict::OutdoorsOnWmo => AppliedLaw::Exterior,
                IndoorVerdict::DayNight => AppliedLaw::Matte,
                IndoorVerdict::Baked { mocv, lobes } => {
                    // The committed words: ambient chases cap96(MOCV) through the node's 2.0/s
                    // ramp (seeded from the scene ambient on lane entry, so walking into a warm
                    // room ramps rather than pops — the reference's `[+0x9c]` carries across the
                    // leg flip); diffuse = floor-168(MOCV) × the node's ramped intensity (1.0
                    // settled indoors; >1 transient while descending from an exterior 2.5 — the
                    // trace's "instance E") on the fixed axis + the hit group's windowed MOLR
                    // lobes from the model's bbox-centre reference point. Refolded per frame
                    // while the entity moves or the chases run (the reference re-runs its attach
                    // per env update — for a settled entity every input is time-independent).
                    let ref_point = anchor_t.transform_point(*center);
                    let word = Vec3::from_array(floor168(mocv));
                    let (ambient, intensity) = match nodes.get_mut(anchor) {
                        Ok(mut node) => {
                            let target = Vec3::from_array(cap96(mocv));
                            // Lane ENTRY is "the anchor holds no slot" — a fresh part joining an
                            // already-seated anchor (a gear swap indoors) must neither reseed the
                            // ambient ramp nor re-allocate; the anchor is mid-lane.
                            if seated.is_none() {
                                node.seed_ambient(Vec3::from_array(lighting.ambient), target);
                            } else {
                                node.ambient_target = target;
                            }
                            (node.ambient, node.intensity())
                        }
                        // A bake-capable anchor without a node (no GroundShade yet): the settled
                        // committed words, directly.
                        Err(_) => (Vec3::from_array(cap96(mocv)), 1.0),
                    };
                    let coeffs = fold_interior_probe(
                        ambient.to_array(),
                        (word * intensity).to_array(),
                        ref_point,
                        &lobes,
                    );
                    let slot = match seated {
                        // Staying in Bake: the anchor keeps its owned slot, rewritten in place —
                        // no component churn, no extraction churn.
                        Some(slot) => {
                            probes.update_owned(slot, coeffs);
                            Some(slot)
                        }
                        None => probes.alloc_owned(coeffs),
                    };
                    match slot {
                        Some(slot) => {
                            // `try_insert`: the anchor may have a same-frame despawn already
                            // queued (the net teardown on a stream drop) — despawn discards
                            // this pure cache, and the insert must not panic at apply time.
                            commands.entity(anchor).try_insert(BakeState {
                                word,
                                lobes,
                                ref_point,
                            });
                            AppliedLaw::Bake(slot)
                        }
                        None => {
                            let (live, peak) = probes.occupancy();
                            warn_once!(
                                "interior-prop probe table full (live {live}, peak {peak}); \
                                 indoor entities fall back to the day/night law"
                            );
                            AppliedLaw::Matte
                        }
                    }
                }
            }
        }
    };
    // Publish the indoor verdict to the node — `entity_shade` picks the intensity target from it
    // (2.5/0.5 by MCSH outdoors; the day/night 1.0 indoors, Matte and Bake alike).
    if let Ok(mut node) = nodes.get_mut(anchor) {
        let indoor = law != AppliedLaw::Exterior;
        if node.indoor != indoor {
            node.indoor = indoor;
        }
    }
    // Slot lifecycle on the anchor, judged by the SEATED state (never a part's memory): entering
    // Bake inserts the owned slot's component (its on-remove hook frees the slot); leaving
    // removes it (and the fold cache). Staying rewrites the same slot in place above — no
    // component cycling.
    match (seated, law) {
        (Some(old), AppliedLaw::Bake(new)) if old == new => {}
        (_, AppliedLaw::Bake(new)) => seat_probe_slot(commands, anchor, new),
        (Some(_), _) => {
            // Leaving Bake: on a despawned anchor the despawn itself already ran the slot
            // hook — the removes just must not panic.
            commands
                .entity(anchor)
                .try_remove::<PropProbeSlot>()
                .try_remove::<BakeState>();
        }
        _ => {}
    }
    law
}

/// Queue the Bake slot swap on `anchor`, tolerant at APPLY time: the anchor may carry a
/// same-frame despawn queued earlier by the net teardown (a world-stream drop mid-transition),
/// which applies before this command and used to panic the classifier. Alive → the normal
/// remove/insert swap (the remove's hook frees the old slot); dead → the fresh slot's component
/// never lands, so its on-remove hook (the pool's only freer) never runs — release the orphan
/// directly instead of leaking it in a pool that never resets.
fn seat_probe_slot(commands: &mut Commands, anchor: Entity, new: u16) {
    commands.queue(
        move |world: &mut World| match world.get_entity_mut(anchor) {
            Ok(mut e) => {
                e.remove::<PropProbeSlot>();
                e.insert(PropProbeSlot(new));
            }
            Err(_) => world.resource_mut::<PropProbes>().release(new),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier_world() -> World {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.init_resource::<WmoResidency>();
        world.init_resource::<Assets<WmoModel>>();
        world.init_resource::<Assets<AdtTile>>();
        world.init_resource::<crate::lighting::WowLighting>();
        world.init_resource::<PropProbes>();
        world.init_resource::<TerrainStreamer>();
        world.init_resource::<InteriorReauthor>();
        world
    }

    /// The stuck-black repair, on 0734's queue: a part whose last-written record names a slot the
    /// anchor no longer owns (it sat outside the classifier's write query — a fade window — across
    /// a slot change) converges to the anchor's standing law even while everything stands
    /// perfectly still, because re-entering the world enqueues it (here via the `on_add` hook; at
    /// runtime the fade-latch observer is the same edge). Pre-0734's ancestor bug: the resolver
    /// trusted the stale part's slot, `update_owned` on the freed slot silently no-opped, and the
    /// unit rendered the freed slot's zeroed rows — a black silhouette that survived any in-room
    /// movement until the law itself changed (director-caught: charge across the doorway
    /// un-blacked it).
    #[test]
    fn a_stale_part_converges_to_the_anchors_standing_law() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = classifier_world();
        let coeffs = [Vec4::ZERO; 7];

        // The anchor's live owned slot — and a defunct one its part still remembers.
        let live = world
            .resource_mut::<PropProbes>()
            .alloc_owned(coeffs)
            .unwrap();
        let stale = world
            .resource_mut::<PropProbes>()
            .alloc_owned(coeffs)
            .unwrap();
        world.resource_mut::<PropProbes>().release(stale);

        let generation = world.resource::<WmoResidency>().generation;
        let anchor = world
            .spawn((
                GlobalTransform::default(),
                PropProbeSlot(live),
                InteriorAnchor {
                    law: AppliedLaw::Bake(live),
                    last_pos: Vec3::ZERO, // matches the transform: the settled gate sees NO movement
                    generation,
                    kind_bake: true,
                },
            ))
            .id();
        let mut lit = InteriorLit::new(
            InteriorKind::Bake {
                material: Handle::default(),
                center: Vec3::ZERO,
            },
            Handle::default(),
        );
        lit.applied = Some(AppliedLaw::Bake(stale));
        let part = world
            .spawn((
                lit,
                ClassifiedBy(anchor),
                MeshMaterial3d::<WowModelMaterial>(Handle::default()),
                MeshTag(crate::mesh_tag::probe_bits(stale)),
            ))
            .id();

        world.run_system_once(classify_entity_interior).unwrap();

        let lit = world.get::<InteriorLit>(part).unwrap();
        assert!(
            matches!(lit.applied, Some(AppliedLaw::Bake(s)) if s == live),
            "the part's law re-anchors on the anchor's standing slot"
        );
        assert_eq!(
            world.get::<MeshTag>(part).unwrap().0,
            crate::mesh_tag::probe_bits(live),
            "the tag reads the anchor's live slot, not the freed (black) one"
        );
        let (occupancy, _) = world.resource::<PropProbes>().occupancy();
        assert_eq!(
            occupancy, 1,
            "repair neither re-allocates nor frees the live slot"
        );
    }

    /// A part spawned onto a SETTLED anchor (the gear-swap-indoors case) takes the standing law
    /// through the `on_add` hook + drain — no law change, no anchor movement, and still the fresh
    /// part's material/tag land on the anchor's law the very next classifier run.
    #[test]
    fn a_fresh_part_on_a_settled_anchor_takes_the_standing_law() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = classifier_world();
        let generation = world.resource::<WmoResidency>().generation;
        let anchor = world
            .spawn((
                GlobalTransform::default(),
                InteriorAnchor {
                    law: AppliedLaw::Matte,
                    last_pos: Vec3::ZERO,
                    generation,
                    kind_bake: false,
                },
            ))
            .id();
        let part = world
            .spawn((
                InteriorLit::new(InteriorKind::Matte, Handle::default()),
                ClassifiedBy(anchor),
                MeshMaterial3d::<WowModelMaterial>(Handle::default()),
                MeshTag(0),
            ))
            .id();

        world.run_system_once(classify_entity_interior).unwrap();

        let lit = world.get::<InteriorLit>(part).unwrap();
        assert_eq!(lit.applied, Some(AppliedLaw::Matte));
        assert_ne!(
            world.get::<MeshTag>(part).unwrap().0 & crate::mesh_tag::INTERIOR_FOG_BIT,
            0,
            "the day/night law carries the room's fog bit"
        );
    }

    /// Decision 0778: a model's BILLBOARD batch takes the same law as its mesh batches. The card
    /// spawns as a world ROOT (the facing system owns its transform, 0153) rather than as a child
    /// of the model, and that is the whole difference — it goes through the same
    /// [`part_interior_lit`] and names the same anchor, so both converge on the same law. The bug
    /// this pins: a Stratholme hanging sign baked from its room while its own chain cards, never
    /// classified at all, stayed on the exterior material and lit from the sky.
    ///
    /// Also pins the classify-OUT arm — a WMO-display part builds no interior variant and must not
    /// join at all, or it would be relit off a material it doesn't have.
    #[test]
    fn a_models_billboard_card_takes_the_same_law_as_its_mesh_parts() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = classifier_world();
        let generation = world.resource::<WmoResidency>().generation;
        let slot = world
            .resource_mut::<PropProbes>()
            .alloc_owned([Vec4::ZERO; 7])
            .unwrap();
        let anchor = world
            .spawn((
                GlobalTransform::default(),
                PropProbeSlot(slot),
                InteriorAnchor {
                    law: AppliedLaw::Bake(slot),
                    last_pos: Vec3::ZERO,
                    generation,
                    kind_bake: true,
                },
            ))
            .id();

        let bake = Handle::default();
        let exterior = Handle::default();
        let lit_for = |anchor| {
            part_interior_lit(&exterior, Some(&exterior), Some(&bake), Vec3::ZERO, anchor)
                .expect("an entity M2 batch always builds an interior variant")
        };
        // The mesh batch rides the model's tree; the card is a world root. Nothing else differs.
        let mesh = world
            .spawn((
                lit_for(anchor),
                MeshMaterial3d::<WowModelMaterial>(Handle::default()),
                MeshTag(0),
            ))
            .id();
        world.entity_mut(anchor).add_child(mesh);
        let card = world
            .spawn((
                lit_for(anchor),
                MeshMaterial3d::<WowModelMaterial>(Handle::default()),
                MeshTag(crate::mesh_tag::alpha_bits(1.0)),
            ))
            .id();

        world.run_system_once(classify_entity_interior).unwrap();

        assert_eq!(
            world.get::<InteriorLit>(card).unwrap().applied,
            Some(AppliedLaw::Bake(slot)),
            "the world-root card takes its model's law, not the world's"
        );
        assert_eq!(
            world.get::<InteriorLit>(mesh).unwrap().applied,
            world.get::<InteriorLit>(card).unwrap().applied,
            "a model never splits across the two light laws at the billboard seam"
        );
        assert_eq!(
            world.get::<MeshTag>(card).unwrap().0,
            crate::mesh_tag::with_interior_probe(crate::mesh_tag::alpha_bits(1.0), slot),
            "the law composes into the tag — the probe slot lands, the card's alpha survives"
        );

        assert!(
            part_interior_lit(&exterior, None, None, Vec3::ZERO, anchor).is_none(),
            "a WMO-display part has no interior variant and classifies out entirely"
        );
    }

    /// The mixed-kind hole (0734 §3): a bake-capable part joining an anchor whose law was
    /// resolved from a matte-kind part drops the anchor's record — the next run re-rays with the
    /// bake kind in reach instead of riding the matte fallback until the anchor happens to move.
    #[test]
    fn a_bake_part_joining_a_matte_resolved_anchor_forces_a_re_resolve() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = classifier_world();
        let generation = world.resource::<WmoResidency>().generation;
        let anchor = world
            .spawn((
                GlobalTransform::default(),
                InteriorAnchor {
                    law: AppliedLaw::Exterior,
                    last_pos: Vec3::ZERO,
                    generation,
                    kind_bake: false,
                },
            ))
            .id();
        world.spawn((
            InteriorLit::new(
                InteriorKind::Bake {
                    material: Handle::default(),
                    center: Vec3::ZERO,
                },
                Handle::default(),
            ),
            ClassifiedBy(anchor),
            MeshMaterial3d::<WowModelMaterial>(Handle::default()),
            MeshTag(0),
        ));

        world.run_system_once(classify_entity_interior).unwrap();

        assert!(
            world.get::<InteriorAnchor>(anchor).is_none(),
            "the matte-resolved record is dropped so the next run re-rays"
        );
    }

    /// The 0755 regression, at the anchor walk: an anchor whose parts are ALL mid-appear-fade
    /// still resolves its law. Pre-0755 the classifier's write query excluded fading parts, so
    /// such an anchor was skipped outright — a freshly-streamed indoor entity had no light law for
    /// the whole 2 s ramp, appeared under the exterior lane, and swapped laws in a single frame the
    /// instant the ramp latched (director-reported: "it swaps lighting in an instant once it's
    /// fully faded in", measured at 2.02 s after the fade armed).
    #[test]
    fn an_anchor_whose_parts_are_all_fading_still_resolves() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = classifier_world();
        let anchor = world.spawn(GlobalTransform::default()).id();
        let part = world
            .spawn((
                InteriorLit::new(InteriorKind::Matte, Handle::default()),
                ClassifiedBy(anchor),
                MeshMaterial3d::<WowModelMaterial>(Handle::default()),
                MeshTag(crate::mesh_tag::alpha_bits(0.125)),
                crate::model_fade::RenderFade {
                    started: 0.0,
                    duration: 2.0,
                    from: 0.0,
                    to: 1.0,
                },
            ))
            .id();

        world.run_system_once(classify_entity_interior).unwrap();

        assert!(
            world.get::<InteriorAnchor>(anchor).is_some(),
            "the anchor resolves during the ramp, not two seconds after it"
        );
        assert!(
            world.get::<InteriorLit>(part).unwrap().applied.is_some(),
            "and the part records the law it resolved to"
        );
    }

    /// The other half of 0755: writing a law onto a part whose ramp is live must carry the ramp's
    /// alpha through the payload rewrite (the classifier used to hardcode opaque, which is exactly
    /// why it had to be locked out), and must land the part on that law's BLEND twin — so the
    /// material mode and the tag payload mode name the same law at every instant, mid-ramp
    /// included.
    #[test]
    fn a_law_written_mid_ramp_keeps_the_alpha_and_takes_the_laws_blend_twin() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = classifier_world();
        let slot = world
            .resource_mut::<PropProbes>()
            .alloc_owned([Vec4::ZERO; 7])
            .unwrap();
        let generation = world.resource::<WmoResidency>().generation;
        let anchor = world
            .spawn((
                GlobalTransform::default(),
                PropProbeSlot(slot),
                InteriorAnchor {
                    law: AppliedLaw::Bake(slot),
                    last_pos: Vec3::ZERO, // settled: the walk skips the ray, the drain does the work
                    generation,
                    kind_bake: true,
                },
            ))
            .id();

        // The steady bake variant, and the probe-lit blend twin the ramp must feather on.
        let bake: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("ba000000-0000-4000-8000-00000000ba1e");
        let bake_blend: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("bb000000-0000-4000-8000-00000000b1e4");
        let exterior_blend: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("e0000000-0000-4000-8000-00000000b1e4");
        let mid_ramp = crate::mesh_tag::alpha_bits(0.25);
        let part = world
            .spawn((
                InteriorLit::new(
                    InteriorKind::Bake {
                        material: bake.clone(),
                        center: Vec3::ZERO,
                    },
                    Handle::default(),
                ),
                ClassifiedBy(anchor),
                // Spawned on the EXTERIOR blend twin, as a streamed part is before it classifies.
                MeshMaterial3d::<WowModelMaterial>(exterior_blend.clone()),
                MeshTag(mid_ramp),
                crate::model_fade::FadeMaterials {
                    cutout: Handle::default(),
                    blend: exterior_blend.clone(),
                    bake_blend: Some(bake_blend.clone()),
                    zfill: None,
                },
                crate::model_fade::RenderFade {
                    started: 0.0,
                    duration: 2.0,
                    from: 0.0,
                    to: 1.0,
                },
            ))
            .id();

        world.run_system_once(classify_entity_interior).unwrap();

        let tag = world.get::<MeshTag>(part).unwrap().0;
        assert!(
            world.get::<InteriorLit>(part).unwrap().is_bake(),
            "the ramping part takes the room's bake law"
        );
        assert_eq!(
            tag,
            crate::mesh_tag::with_interior_probe(mid_ramp, slot),
            "the probe slot lands on top of the ramp's alpha, not over it"
        );
        assert_ne!(
            tag,
            crate::mesh_tag::probe_bits(slot),
            "the payload must not force a mid-ramp part opaque (the pre-0755 write)"
        );
        let material = world
            .get::<MeshMaterial3d<WowModelMaterial>>(part)
            .unwrap()
            .0
            .clone();
        assert_eq!(
            material, bake_blend,
            "a ramping part takes the law's PROBE-LIT blend twin — not the exterior one it \
             spawned on (its light would read as full outdoor intensity), and not the steady bake \
             variant (the ramp would stop feathering)"
        );
        assert_ne!(material, exterior_blend);
        assert_ne!(material, bake);
    }

    /// The fade-latch edge: removing a part's `RenderFade` re-enqueues it for authoring — the
    /// event that closes every fade-exclusion window (0734 §3; the old settled-path sweep is
    /// gone, so this observer IS the convergence path).
    #[test]
    fn a_fade_latch_enqueues_the_part_for_reauthoring() {
        let mut world = classifier_world();
        world.add_observer(enqueue_on_fade_latch);
        let part = world
            .spawn(InteriorLit::new(InteriorKind::Matte, Handle::default()))
            .id();
        world.resource_mut::<InteriorReauthor>().0.clear(); // drop the on_add entry
        world
            .entity_mut(part)
            .insert(crate::model_fade::RenderFade {
                started: 0.0,
                duration: 1.0,
                from: 0.0,
                to: 1.0,
            });
        world
            .entity_mut(part)
            .remove::<crate::model_fade::RenderFade>();
        assert_eq!(
            world.resource::<InteriorReauthor>().0,
            vec![part],
            "the latch is the re-entry edge"
        );
    }

    /// **The 0776 fork.** A GameObject anchor probes at its world bounding-box CENTRE, a unit at
    /// its position — and the centre is where the difference bites: the Stratholme portcullis whose
    /// spawn z sits 15 cm *under* the corridor slab rays into open air from its origin and into the
    /// room from its centre (measured: `exterior` at z 125.354, `BAKE g02` at 125.40 and above).
    /// The scale leg matters too — the centre is model-local, so a scaled placement must carry it
    /// through the transform rather than adding a raw offset.
    #[test]
    fn a_gameobject_anchors_at_its_box_centre_and_a_unit_at_its_position() {
        use crate::wmo_portal::LightAttach;

        let at = GlobalTransform::from(
            Transform::from_translation(Vec3::new(10.0, 100.0, -5.0)).with_scale(Vec3::splat(2.0)),
        );
        let centre = BodyBakeCenter(Vec3::new(0.0, 3.5, 0.0));

        let (attach, anchor) = attach_anchor(true, Some(&centre), &at);
        assert_eq!(attach, LightAttach::Containment);
        assert_eq!(
            anchor,
            Vec3::new(10.0, 107.0, -5.0),
            "the containment anchor is the box centre through the placement — scale included"
        );

        let (attach, anchor) = attach_anchor(false, Some(&centre), &at);
        assert_eq!(attach, LightAttach::DownRay);
        assert_eq!(
            anchor,
            at.translation(),
            "a unit still rays from its position, box centre or not"
        );

        // No bounds yet (the model is still streaming): degrade to the origin, never to a wrong
        // point — the containment lane's other legs still apply.
        let (attach, anchor) = attach_anchor(true, None, &at);
        assert_eq!(attach, LightAttach::Containment);
        assert_eq!(anchor, at.translation());
    }

    /// The teardown race, both arms: a slot seated on a live anchor swaps normally; one seated
    /// on an anchor whose despawn applied first neither panics (the old crash) nor leaks the
    /// freshly allocated slot (the pool never resets — an orphan would be held forever).
    #[test]
    fn slot_seat_survives_a_despawned_anchor_and_releases_the_orphan() {
        let mut world = World::new();
        world.init_resource::<PropProbes>();
        let coeffs = [Vec4::ZERO; 7];

        let alive = world.spawn_empty().id();
        let slot_a = world
            .resource_mut::<PropProbes>()
            .alloc_owned(coeffs)
            .unwrap();
        seat_probe_slot(&mut world.commands(), alive, slot_a);
        world.flush();
        assert_eq!(world.get::<PropProbeSlot>(alive).unwrap().0, slot_a);

        let doomed = world.spawn_empty().id();
        let slot_b = world
            .resource_mut::<PropProbes>()
            .alloc_owned(coeffs)
            .unwrap();
        let (live_before, _) = world.resource::<PropProbes>().occupancy();
        world.entity_mut(doomed).despawn();
        seat_probe_slot(&mut world.commands(), doomed, slot_b);
        world.flush();
        let (live_after, _) = world.resource::<PropProbes>().occupancy();
        assert_eq!(
            live_after,
            live_before - 1,
            "the orphan slot is released, not leaked"
        );
    }
}
