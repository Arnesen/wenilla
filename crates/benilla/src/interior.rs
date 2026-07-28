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

use std::collections::{HashMap, HashSet};

use benilla_assets::{cap96, floor168, AdtTile, WmoModel};
use bevy::asset::AssetId;
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
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// The interior/exterior material variants for one entity submesh part, so [`classify_entity_interior`]
/// can swap by the model's current location without rebuilding. Attached only to M2 entity parts (WMO
/// group geometry carries per-submesh interior in its own material + baked MOCV).
#[derive(Component)]
pub(crate) struct InteriorLit {
    /// The model's classification anchor — the entity whose `GlobalTransform` the down-ray probes:
    /// the NET ENTITY root, for body parts and held/equipped items alike (module docs — the
    /// reference has one light node per unit and items alias it). Every part sharing an anchor
    /// shares its verdict: a body, hands, and blade must never split across light laws.
    anchor: Entity,
    /// The model's indoor law ([`InteriorKind`]) — uniform across an anchor's parts.
    kind: InteriorKind,
    /// The exterior/day-night material (the global-SH lane): since 0354 the Matte law rides it
    /// too — day/night is the intensity byte at the 1.0 point, not a separate material.
    exterior: Handle<WowModelMaterial>,
    /// Last applied law — used to write the material + tag ONLY on change, so a standing prop/NPC
    /// doesn't re-trigger extraction every frame. `None` until the first classification.
    applied: Option<AppliedLaw>,
    /// A transient author (the self-avatar zoom feather) overwrote the material/tag: re-author
    /// them on the next run WITHOUT treating the law as new ([`Self::invalidate`] — clearing
    /// `applied` here replayed the bake lane's ENTRY effects, reseeding the ambient ramp from the
    /// scene light: the director's "still flashes for a second on the zoom out").
    reauthor: bool,
    /// Anchor position at the last test + the residency generation then — the movement/registry gate
    /// that keeps a static entity from re-running the down-ray every frame.
    last_pos: Vec3,
    generation: u32,
}

impl InteriorLit {
    /// Whether this part currently rides the footprint-BAKE lane — the intensity-byte writer's
    /// skip test: a bake part's tag payload is its probe SLOT, so [`crate::entity_shade`] must not
    /// write the shade byte over it (every other law carries the byte — since 0354 the day/night
    /// state is the byte at the intensity-1.0 point, not a material swap).
    pub(crate) fn is_bake(&self) -> bool {
        matches!(self.applied, Some(AppliedLaw::Bake(_)))
    }

    pub(crate) fn new(
        anchor: Entity,
        kind: InteriorKind,
        exterior: Handle<WowModelMaterial>,
    ) -> Self {
        Self {
            anchor,
            kind,
            exterior,
            applied: None,
            reauthor: false,
            last_pos: Vec3::ZERO,
            generation: u32::MAX, // != any real generation ⇒ classify on the first frame
        }
    }

    /// Hand the material/tag channel back to the classifier: a transient author (the self-avatar
    /// zoom fade) wrote over them, so the settled/changed gates would otherwise never restore the
    /// steady state. Forces a re-author (material + tag) on the classifier's next run while
    /// KEEPING the applied law — resetting it made the classifier replay the bake lane's entry
    /// effects (a fresh ambient-ramp seed from the scene light + a new owned slot), which read as
    /// a second-long light flash at the end of every zoom-out (director-caught, 2026-07-13).
    pub(crate) fn invalidate(&mut self) {
        self.reauthor = true;
    }
}

/// Registers the residency registry + the per-frame entity classifier (the streamer fills the registry).
pub(crate) struct InteriorPlugin;

impl Plugin for InteriorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WmoResidency>()
            .add_systems(Update, classify_entity_interior);
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
/// one ray per UNIT per re-test (memoised across its parts), re-run only when the anchor moves or
/// a building streams in/out, so static entities cost a compare per frame and nothing else.
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
    anchors: Query<&GlobalTransform>,
    mut nodes: Query<&mut crate::entity_shade::GroundShade>,
    bake_states: Query<&BakeState>,
    seats: Query<&PropProbeSlot>,
    // Skip an entity whose appear-fade is **pending or live**: it must stay invisible (on its blend twin
    // at α≈0) until armed, and while ramping `apply_render_fade` owns its material + `MeshTag` (the fade
    // alpha). We reclaim the channel (steady material) only once both are gone — i.e. the fade has
    // latched. Without this the classifier would fight the fade for the tag (and force the pending
    // entity opaque).
    mut parts: Query<
        (
            &mut InteriorLit,
            &mut MeshMaterial3d<WowModelMaterial>,
            &mut MeshTag,
        ),
        (Without<RenderFade>, Without<PendingAppearFade>),
    >,
) {
    // One down-ray (and at most one probe fold) per unit per run: parts share their anchor's
    // resolved law, so an 8-part body re-testing after a step costs one ray, not eight.
    let mut verdicts: HashMap<Entity, AppliedLaw> = HashMap::new();
    let _t0 = std::time::Instant::now();
    let (mut n_parts, mut n_resolved) = (0usize, 0usize);
    let mut resolve_us = 0.0f32;
    for (mut lit, mut material, mut tag) in &mut parts {
        n_parts += 1;
        let Ok(anchor_t) = anchors.get(lit.anchor) else {
            continue; // anchor despawning this frame — the parts go with it
        };
        let pos = anchor_t.translation();
        // Skip the down-ray entirely for a settled entity (no movement, no building streamed) —
        // this is what keeps a town full of standing NPCs/props from re-raying every frame. A
        // Bake-law anchor whose node ramps still chase keeps refolding (from the cached ray
        // products — no new ray), so a unit that stops just inside a warm zone finishes its
        // transition instead of freezing mid-ramp.
        let moved = lit.applied.is_none()
            || lit.generation != residency.generation
            || pos.distance_squared(lit.last_pos) >= RESAMPLE_DIST_SQ;
        // The anchor's seated [`PropProbeSlot`] is the single source of truth for the Bake slot —
        // a part's cached `applied` is only its last-written record. Parts join and leave an
        // anchor independently (gear/attachment swaps spawn fresh parts, fades exclude parts from
        // this query for whole transitions), so a part-held slot CAN go stale; trusting the
        // first-iterated part's copy once freed the live slot out from under a standing body's
        // other parts, whose tags then pointed at a zeroed (black) row until the law next changed
        // — the "unit goes black indoors until a charge crosses the doorway" bug.
        let seated = seats.get(lit.anchor).ok().map(|s| s.0);
        if !moved {
            let ramping = lit.is_bake() && nodes.get(lit.anchor).is_ok_and(|n| !n.ramps_settled());
            // A part whose recorded slot disagrees with the anchor's seated slot is stale (it was
            // excluded — fading — across a law transition): repair it even though it didn't move.
            let stale_slot = matches!(lit.applied, Some(AppliedLaw::Bake(s)) if seated != Some(s));
            if !ramping && !lit.reauthor && !stale_slot {
                continue;
            }
        }
        let law = *verdicts.entry(lit.anchor).or_insert_with(|| {
            n_resolved += 1;
            let _r = std::time::Instant::now();
            let out = resolve_anchor_law(
                &mut commands,
                &mut probes,
                &wmos,
                &instances,
                &streamer,
                &adt_tiles,
                &lighting,
                &mut nodes,
                &bake_states,
                lit.anchor,
                anchor_t,
                &lit.kind,
                lit.applied,
                seated,
                moved,
            );
            resolve_us += _r.elapsed().as_secs_f32() * 1e6;
            out
        });
        if moved {
            lit.last_pos = pos;
            lit.generation = residency.generation;
        }
        // Write the material/tag only when the law actually changed — or when a transient author
        // handed the channel back ([`InteriorLit::invalidate`]) — so re-testing a moving NPC
        // mid-room doesn't churn the render extraction.
        if lit.applied == Some(law) && !lit.reauthor {
            continue;
        }
        lit.reauthor = false;
        material.0 = match law {
            // The exterior AND day/night states share the exterior material — the difference is
            // the node's intensity target (the tag byte `entity_shade` ramps; 0354).
            AppliedLaw::Exterior | AppliedLaw::Matte => lit.exterior.clone(),
            AppliedLaw::Bake(_) => match &lit.kind {
                InteriorKind::Bake { material, .. } => material.clone(),
                InteriorKind::Matte => lit.exterior.clone(), // unreachable by construction
            },
        };
        // `WOW_INTERIOR_LOG=1`: print interior classifications — the live-probe instrument for
        // "did this entity actually classify indoors, and under which law?". Scoped to interior
        // verdicts (plus interior→exterior flips) so the world's exterior masses stay silent.
        if (law != AppliedLaw::Exterior || lit.applied.is_some())
            && std::env::var_os("WOW_INTERIOR_LOG").is_some()
        {
            eprintln!(
                "[interior] anchor {:?} at ({:.1}, {:.1}, {:.1}) -> {}",
                lit.anchor,
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
        // The tag: the Bake law's payload carries the probe SLOT in its bits-6..=18 field plus an
        // opaque alpha field (a fade feather composes through `with_alpha` without clobbering the
        // slot); the other laws reset to the opaque exterior payload (shade byte 0 —
        // `entity_shade` runs after this classifier and re-asserts the ramped intensity byte
        // the same frame; it skips only Bake parts). BOTH indoor laws carry the INTERIOR_FOG_BIT
        // (Bake bakes it in): the reference fogs a unit by the unit's OWN interior
        // classification, so an indoor day/night character keeps the room's fog — never the
        // storm's near veil — while the exterior law returns it to the scene fog (wow-re
        // `m2-unit-interior-fog.md`; the director's corridor-vs-porch walk-out). Every arm
        // carries the part's rig field through (decision 0720): a skinned part keeps its palette
        // across the indoor/outdoor transition.
        tag.0 = match law {
            AppliedLaw::Bake(slot) => crate::mesh_tag::with_interior_probe(tag.0, slot),
            AppliedLaw::Matte => {
                crate::mesh_tag::INTERIOR_FOG_BIT | crate::mesh_tag::with_exterior_reset(tag.0)
            }
            AppliedLaw::Exterior => crate::mesh_tag::with_exterior_reset(tag.0),
        };
        lit.applied = Some(law);
    }
    // `WOW_INTERIOR_COST=1`: this lane's per-frame cost, in the terms that diagnose it — how many
    // PARTS the walk touches vs how many ANCHORS actually re-resolve, and what the resolves cost.
    // The split is the whole diagnosis: on 2026-07-27 the lane read 11.3 ms/frame at the LBRS pin
    // and the walk was 0.1 ms of it — 35 moving units paying ~300 us each in `resolve_anchor_law`,
    // which is what sent the hunt into the WMO column rays (decision 0711) rather than into this
    // loop. Cheap enough to leave in: two counters and one `Instant` per frame.
    static COST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *COST.get_or_init(|| std::env::var_os("WOW_INTERIOR_COST").is_some()) {
        eprintln!(
            "[interior-cost] parts={n_parts} anchors_resolved={n_resolved} resolve_ms={:.2} total_ms={:.2}",
            resolve_us / 1000.0,
            _t0.elapsed().as_secs_f32() * 1000.0
        );
    }
}

/// Resolve one anchor's indoor law this run: the down-ray verdict (or, `!moved`, a ramp-only
/// refold from the cached ray products), the node's target/seed updates, and for the Bake law the
/// footprint fold into the anchor's OWNED probe slot. `seated` — the anchor's live
/// [`PropProbeSlot`] — is the ONLY authority on that slot: Bake stays on it, entry/exit is judged
/// by it, and a part-cached `Bake(slot)` is never believed (a fresh part's `previous = None` once
/// re-allocated here and freed the seated slot under the anchor's other parts — the
/// stuck-black-unit bug; the caller's stale-slot repair is the other half). The slot component
/// lives on the ANCHOR — its on-remove hook frees the slot on despawn; law transitions
/// remove/insert it here.
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
    bake_states: &Query<&BakeState>,
    anchor: Entity,
    anchor_t: &GlobalTransform,
    kind: &InteriorKind,
    previous: Option<AppliedLaw>,
    seated: Option<u16>,
    moved: bool,
) -> AppliedLaw {
    let pos = anchor_t.translation();
    // The settled path (`!moved`): a seated anchor stays on its owned slot — refolded from the
    // cached ray products when the node's chases still move, returned as-is otherwise. A part
    // remembering Bake while the anchor holds NO slot is stale (its slot was freed while the part
    // sat outside the query) — fall through and re-resolve with a fresh ray.
    if !moved {
        if let Some(slot) = seated {
            if let (Ok(node), Ok(bake)) = (nodes.get(anchor), bake_states.get(anchor)) {
                let coeffs = fold_interior_probe(
                    node.ambient.to_array(),
                    (bake.word * node.intensity()).to_array(),
                    bake.ref_point,
                    &bake.lobes,
                );
                probes.update_owned(slot, coeffs);
            }
            return AppliedLaw::Bake(slot);
        }
        match previous {
            Some(AppliedLaw::Bake(_)) | None => {}
            Some(settled) => return settled,
        }
    }
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

    /// The stuck-black repair: a part whose cached law names a slot the anchor no longer owns
    /// (freed while the part sat outside the classifier's query — a fade/attach window) converges
    /// to the anchor's SEATED slot even while standing perfectly still. Pre-fix, the resolver
    /// trusted the stale part's slot: `update_owned` on the freed slot silently no-opped, the
    /// applied==law gate skipped every rewrite, and the unit rendered the freed slot's zeroed rows
    /// — a black silhouette that survived any in-room movement until the law itself changed
    /// (director-caught: charge across the doorway un-blacked it).
    #[test]
    fn a_stale_part_converges_to_the_anchors_seated_slot() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = World::new();
        world.init_resource::<WmoResidency>();
        world.init_resource::<Assets<WmoModel>>();
        world.init_resource::<Assets<AdtTile>>();
        world.init_resource::<crate::lighting::WowLighting>();
        world.init_resource::<PropProbes>();
        world.init_resource::<TerrainStreamer>();
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
            .spawn((GlobalTransform::default(), PropProbeSlot(live)))
            .id();
        let mut lit = InteriorLit::new(
            anchor,
            InteriorKind::Bake {
                material: Handle::default(),
                center: Vec3::ZERO,
            },
            Handle::default(),
        );
        lit.applied = Some(AppliedLaw::Bake(stale));
        lit.last_pos = Vec3::ZERO; // matches the anchor: the settled gate sees NO movement
        lit.generation = generation;
        let part = world
            .spawn((
                lit,
                MeshMaterial3d::<WowModelMaterial>(Handle::default()),
                MeshTag(crate::mesh_tag::probe_bits(stale)),
            ))
            .id();

        world.run_system_once(classify_entity_interior).unwrap();

        let lit = world.get::<InteriorLit>(part).unwrap();
        assert!(
            matches!(lit.applied, Some(AppliedLaw::Bake(s)) if s == live),
            "the part's law re-anchors on the seated slot"
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
