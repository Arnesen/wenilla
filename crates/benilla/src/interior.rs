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
//! Entities move and stream independently of their building, so [`classify_entity_interior`]
//! re-tests them against the placed WMOs. The indoor test is the client's own: a faces-only
//! down-ray from the position onto the placed groups' geometry
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
#[derive(Component, Clone, Copy)]
pub(crate) struct BodyBakeCenter(pub(crate) Vec3);

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

    pub(crate) fn new(kind: InteriorKind, exterior: Handle<WowModelMaterial>) -> Self {
        Self {
            kind,
            exterior,
            applied: None,
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

/// Fade latch: the appear-fade owned this part's material/tag while it lived (the classifier's
/// write query excludes fading parts entirely), so the moment `RenderFade` leaves, the part
/// re-authors from its anchor's law — the event-driven replacement for the old settled-path
/// stale-slot repair (the "unit stays black indoors" bug: a slot freed while its part sat
/// excluded). Removal on despawn also lands here; the drain drops dead entries.
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
    )>,
    mut nodes: Query<&mut crate::entity_shade::GroundShade>,
    bake_states: Query<&BakeState>,
    seats: Query<&PropProbeSlot>,
    mut queue: ResMut<InteriorReauthor>,
    part_anchors: Query<&ClassifiedBy>,
    // Skip an entity whose appear-fade is **pending or live**: it must stay invisible (on its blend twin
    // at α≈0) until armed, and while ramping `apply_render_fade` owns its material + `MeshTag` (the fade
    // alpha). We reclaim the channel (steady material) only once both are gone — i.e. the fade has
    // latched (the latch observer re-enqueues). Without this the classifier would fight the fade for
    // the tag (and force the pending entity opaque).
    mut parts: Query<
        (
            &mut InteriorLit,
            &mut MeshMaterial3d<WowModelMaterial>,
            &mut MeshTag,
        ),
        (Without<RenderFade>, Without<PendingAppearFade>),
    >,
) {
    let _t0 = std::time::Instant::now();
    let (mut n_anchors, mut n_resolved, mut n_written) = (0usize, 0usize, 0usize);
    let mut resolve_us = 0.0f32;
    for (anchor, anchor_t, lit_parts, mut state) in &mut anchors {
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
        // Re-resolving: the kind comes from the anchor's first classifiable part. Every part
        // excluded (a whole-model appear-fade, pending or live) ⇒ no resolve at all — the fade
        // owns the channel, and the latch observer brings the parts back through the queue.
        let Some(kind) = lit_parts
            .iter()
            .find_map(|part| parts.get(part).ok().map(|(lit, _, _)| lit.kind.clone()))
        else {
            continue;
        };
        let seated = seats.get(anchor).ok().map(|s| s.0);
        n_resolved += 1;
        let _r = std::time::Instant::now();
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
        // verdicts (plus interior→exterior flips) so the world's exterior masses stay silent.
        if (law != AppliedLaw::Exterior || had_state)
            && std::env::var_os("WOW_INTERIOR_LOG").is_some()
        {
            eprintln!(
                "[interior] anchor {anchor:?} at ({:.1}, {:.1}, {:.1}) -> {}",
                pos.x,
                pos.y,
                pos.z,
                match law {
                    AppliedLaw::Exterior => "exterior".to_string(),
                    AppliedLaw::Matte => "INTERIOR matte".to_string(),
                    AppliedLaw::Bake(s) => format!("INTERIOR bake slot {s}"),
                }
            );
        }
        for part in lit_parts.iter() {
            if let Ok((mut lit, mut material, mut tag)) = parts.get_mut(part) {
                n_written += usize::from(write_part_law(
                    law,
                    &mut lit,
                    &mut material,
                    &mut tag,
                    false,
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
        let Ok((_, _, _, mut state)) = anchors.get_mut(edge.0) else {
            continue;
        };
        let Some(state) = state.as_deref_mut() else {
            continue; // law not resolved yet — the anchor's first resolve writes every part
        };
        let Ok((mut lit, mut material, mut tag)) = parts.get_mut(part) else {
            continue; // still fade-excluded — the latch re-enqueues
        };
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
            "[interior-cost] anchors={n_anchors} resolved={n_resolved} parts_written={n_written} resolve_ms={:.2} total_ms={:.2}",
            resolve_us / 1000.0,
            _t0.elapsed().as_secs_f32() * 1000.0
        );
    }
}

/// Write one part's material + tag for `law` — the single place a part's channel is authored.
/// Change-gated on the part's last-written record unless `force` (a transient author — fade,
/// zoom feather — overwrote the channel while `applied` stayed current). Returns whether it wrote.
///
/// The tag: the Bake law's payload carries the probe SLOT in its bits-6..=18 field plus an
/// opaque alpha field (a fade feather composes through `with_alpha` without clobbering the
/// slot); the other laws reset to the opaque exterior payload (shade byte 0 — `entity_shade`
/// runs after the classifier and re-asserts the ramped intensity byte the same frame; it skips
/// only Bake parts). BOTH indoor laws carry the INTERIOR_FOG_BIT (Bake bakes it in): the
/// reference fogs a unit by the unit's OWN interior classification, so an indoor day/night
/// character keeps the room's fog — never the storm's near veil — while the exterior law returns
/// it to the scene fog (wow-re `m2-unit-interior-fog.md`; the director's corridor-vs-porch
/// walk-out). Every arm carries the part's rig field through (decision 0720): a skinned part
/// keeps its palette across the indoor/outdoor transition.
fn write_part_law(
    law: AppliedLaw,
    lit: &mut InteriorLit,
    material: &mut MeshMaterial3d<WowModelMaterial>,
    tag: &mut MeshTag,
    force: bool,
) -> bool {
    if lit.applied == Some(law) && !force {
        return false;
    }
    material.0 = match law {
        // The exterior AND day/night states share the exterior material — the difference is
        // the node's intensity target (the tag byte `entity_shade` ramps; 0354).
        AppliedLaw::Exterior | AppliedLaw::Matte => lit.exterior.clone(),
        AppliedLaw::Bake(_) => match &lit.kind {
            InteriorKind::Bake { material, .. } => material.clone(),
            InteriorKind::Matte => lit.exterior.clone(), // a matte part under a bake-law anchor
        },
    };
    tag.0 = match law {
        AppliedLaw::Bake(slot) => crate::mesh_tag::with_interior_probe(tag.0, slot),
        AppliedLaw::Matte => {
            crate::mesh_tag::INTERIOR_FOG_BIT | crate::mesh_tag::with_exterior_reset(tag.0)
        }
        AppliedLaw::Exterior => crate::mesh_tag::with_exterior_reset(tag.0),
    };
    lit.applied = Some(law);
    true
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
    kind: &InteriorKind,
    seated: Option<u16>,
) -> AppliedLaw {
    let pos = anchor_t.translation();
    let verdict = indoor_verdict_at(wmos, instances.iter(), streamer, adt_tiles, pos);
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
                cutout: Handle::default(),
                blend: Handle::default(),
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
