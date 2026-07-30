//! Entity ground-shade (decision 0173): units, players, and GameObjects sample the terrain MCSH under
//! them **dynamically** and dim their sun term when standing in baked ground shadow — the fold-back of
//! wow-re's byte-verified §8a/§9 verdict (`models/scratch/m2-interior-doodad-base-light`): a spawned
//! unit/player/GameObject carries the SAME 2.5-lit / 0.5-MCSH-shadowed chain as an ADT doodad, driven by
//! a per-frame MCSH sample at the object's node position and a linear intensity ramp (`0x69e770`, the
//! step constant `[0x810808] = 3.3333`/s), not a static spawn-time bake.
//!
//! **SETTLED (0809), and it split this file's premise in two.** §9's chain is byte-verified up to its
//! last link — "the unit's own light node fills the unit's committed light" — and that link was
//! INFERRED. Observation contradicted it: a player + three NPCs across two of the reference's own
//! apitraces commit their sun at gain **exactly 1.0**, never 2.5 or 0.5, while the same frames' ADT
//! doodads span the full 0.5/0.85/1.0/1.874/2.5 ramp (wow-re `trace-forensics-northshire-d3d` §5).
//! wow-re settled it in `unit-mcsh-shadow-target.md`: the 2.5/0.5 **target** law really is byte-shared
//! (one 2.5 site `69e4ad`, one MCSH sampler `0x69b350`, one `0x69e280` — so §9 was right that far), but
//! **delivery** splits at `0x672a20`. With `[model+0x3c0] == 0` the null fallback commits the raw
//! day/night ambient/diffuse with no intensity multiply anywhere — hardwired ×1.0, position-independent,
//! never sampling MCSH — and a unit's node is born UNREGISTERED (`0x670db0`'s birth-register gate
//! `670fca` is skipped for the model-arg-0 spawn). Their verdict to us: *"benilla must NOT light units
//! on the 2.5/0.5 static-doodad law by default; the outdoor unit base is day/night ×1.0."*
//!
//! So this system keeps the whole chain for **GameObjects** (which consume the node/bake) and, for
//! **units and players**, keeps only its ambient-word duty: their intensity is pinned flat at the
//! day/night point ([`GroundShade::null_fallback`]). What is still ours to answer, and wow-re says so
//! explicitly, is the **registration lifecycle** — which unit states carry `[model+0x3c0]` = node and
//! so earn the 2.5/0.5 chain back (their traces show a standing Stormwind player at ×2.5 against the
//! running Northshire player's ×1.0). Until that is measured, unregistered is the modelled default,
//! which is the state their traces show for a moving unit.
//!
//! Structure mirrors the reference's one-light-node-per-object: [`GroundShade`] lives on the **net
//! entity root** (the `[obj+0xe0]` twin), sampled at the root's feet and ramped there; the resulting
//! shade byte is written to every M2 part in the root's descendant tree — body submeshes, and held
//! items/helm/shoulders hanging off joint entities — via the `MeshTag` shade field (`mesh_tag`), so a
//! weapon dims with its wielder rather than sampling independently at the hand (the real client lights
//! attachments from the owner's node; independent sampling would flicker at shadow edges mid-swing).
//!
//! Interplay: the interior classifier ([`crate::interior`]) owns a part's tag while it stands in a WMO
//! room (packed floor colour — no sun indoors, so no shade either); this system skips those parts and
//! runs after the classifier to re-assert the byte over its exterior reclaim. Fades own the alpha field
//! only (they write through `with_alpha`), so shade rides through appear/despawn/zoom feathering.

use benilla_assets::AdtTile;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::interior::{classify_entity_interior, InteriorLit};
use crate::mesh_tag::{shade_of, with_shade};
use crate::terrain_stream::{doodad_ground_shade, ShadeResolve, TerrainStreamer};

// Decision 0354 generalized this file from "the MCSH ground-shade byte" to the entity light
// node's CPU ramp pair: `t` (the intensity chase — the tag byte the exterior SH lane scales by)
// and the ambient word chase the interior bake fold consumes. The MCSH sample now only picks the
// OUTDOOR target; the classifier's interior verdict overrides it with the day/night point.

/// How fast the shade mix `t` (0 = lit, 1 = shaded) moves toward its target: the binary's linear step
/// `[0x810808] = 3.3333` intensity-units/s over its lit→shaded span (2.5 → 0.5), i.e. the reference's
/// full transition takes 0.6 s. Since 0354 the shader's levels ARE the byte 2.5/0.5, so the
/// normalized-by-span mix is exactly the binary's ramp, not just its timing.
const SHADE_RAMP_PER_SEC: f32 = 3.3333 / 2.0;

/// Squared distance (yd²) the root must move before the MCSH bit is re-sampled — same gate as the
/// interior classifier: a standing NPC costs a position compare and nothing else. (MCSH texels are
/// ~0.5 yd, so half a yard of hysteresis is at the sample's own resolution.)
const RESAMPLE_DIST_SQ: f32 = 0.25;

/// The ambient word's ramp rate — the binary's `[0x810804] = 2.0` colour-units/s (`0x69e770`'s
/// FIRST chase, `[+0x9c]` → `[+0xf4]`; wow-re `unit-light-combine-storm.md` c3 — the intensity
/// chase runs at its own 3.3333).
const AMBIENT_RAMP_PER_SEC: f32 = 2.0;

/// The shade mix `t` encoding the DAY/NIGHT intensity 1.0: `intensity = mix(2.5, 0.5, t)` ⇒
/// `t = 0.75`. **Two distinct mechanisms land on this same value** and the difference matters if
/// either moves: an indoor entity's node target (the reference's interior `[+0xf8] = 1.0`, written at
/// `69e36b` behind the `[+0xc]&2` interior gate), and `0x672a20`'s null-node fallback for an
/// unregistered unit (no intensity multiply at all — see [`GroundShade::null_fallback`]).
const DAYNIGHT_T: f32 = 0.75;

/// Settled-ramp epsilon (on `t` and each ambient channel) — under half a tag/colour byte.
const RAMP_EPS: f32 = 1.0 / 640.0;

/// The per-entity light-node state, on the net entity **root** (unit / player / GameObject) — the
/// CPU twin of the reference's per-object light node (decision 0354): the intensity chase
/// (`[+0xa4]`→`[+0xf8]`, held as the normalized mix `t` over the 2.5→0.5 span) and the ambient
/// word chase (`[+0x9c]`→`[+0xf4]`) — the pair `0x69e770` steps every frame.
#[derive(Component)]
pub(crate) struct GroundShade {
    /// Current shade mix (0 = intensity 2.5, 1 = 0.5; [`DAYNIGHT_T`] = the day/night 1.0) — what the
    /// parts' tags show, and what the interior bake fold scales its diffuse word by.
    t: f32,
    /// Where `t` is ramping to (0/1 from the last MCSH sample outdoors; [`DAYNIGHT_T`] indoors).
    target: f32,
    /// Root position at the last sample (the movement gate).
    last_pos: Vec3,
    /// Whether the first sample landed (it snaps `t = target`; a spawn never plays a ramp-in).
    sampled: bool,
    /// The interior verdict, published by the classifier (`crate::interior`) — indoors the MCSH
    /// sample is overridden by the day/night target ([`DAYNIGHT_T`]).
    pub(crate) indoor: bool,
    /// Standing on an outdoor-class WMO surface (street/deck/porch — `MOGI & 0x48`), published by
    /// the classifier: the MCSH verdict of the terrain BENEATH the building is overridden by the
    /// lit target 0 (intensity 2.5) — byte-verified (0477/0480, wow-re `unit-wmo-mcsh-gate.md`):
    /// the down-ray attach's WMO branch sets the skip-shadow bit `[node+0xd]|=0x2` (`0x6a8bc7`,
    /// every node subclass), the terrain branch clears it (`0x6a8bed`), and the exterior intensity
    /// leg commits the constant 2.5 whenever it's set (`0x69e483`→`0x69e4ad` — the MCSH sample
    /// runs only terrain-linked). `self.target` keeps the raw sample so stepping off onto real
    /// terrain resumes from it.
    pub(crate) on_wmo: bool,
    /// The ramped ambient word (0..1 per channel) — the bake fold's ambient input, chasing
    /// [`Self::ambient_target`] at [`AMBIENT_RAMP_PER_SEC`]. Seeded by the classifier on bake
    /// entry (from the scene ambient, so walking into a warm room ramps rather than pops).
    pub(crate) ambient: Vec3,
    pub(crate) ambient_target: Vec3,
    /// This root's drawn model takes `0x672a20`'s **null-node fallback** — true for units and
    /// players, false for GameObjects (0809; wow-re `unit-mcsh-shadow-target.md` §3/§4). The
    /// fallback commits the raw day/night pair with no per-node multiply, so the intensity is a
    /// hardwired ×1.0 that never samples MCSH and cannot vary with position: `t` pins to
    /// [`DAYNIGHT_T`] outdoors instead of chasing the terrain verdict. The MCSH sample still runs
    /// (it is cheap, gated on movement, and `self.target` stays live) so that the day this models
    /// the registration lifecycle — the one piece wow-re handed back to us — a registered unit
    /// resumes the real chase with no state to rebuild.
    ///
    /// Set once from the entity kind at node attach; a live display-id swap cannot change the kind
    /// (`entities::attach`, decision 0776 makes the same argument for `ContainmentAttach`), which is
    /// why `insert_if_new` keeping the old value is correct rather than merely tolerable.
    null_fallback: bool,
    /// The last effective target the `WOW_INTERIOR_LOG` instrument printed (log-on-change; starts
    /// off-scale so the first resolved target always prints).
    logged_target: f32,
}

impl Default for GroundShade {
    fn default() -> Self {
        Self {
            t: 0.0,
            target: 0.0,
            last_pos: Vec3::ZERO,
            sampled: false,
            indoor: false,
            on_wmo: false,
            ambient: Vec3::ZERO,
            ambient_target: Vec3::ZERO,
            null_fallback: false,
            logged_target: -1.0,
        }
    }
}

impl GroundShade {
    /// The node state for a freshly attached root of this entity kind — the delivery split of
    /// `0x672a20` (0809): a **GameObject** consumes its light node (the 2.5/0.5 terrain-shade
    /// chain); a **unit or player** is born with its node unregistered and takes the null fallback's
    /// flat day/night ×1.0. See [`Self::null_fallback`].
    pub(crate) fn for_kind(kind: benilla_protocol::EntityKind) -> Self {
        Self {
            null_fallback: !matches!(kind, benilla_protocol::EntityKind::GameObject),
            ..Self::default()
        }
    }

    /// The node's current committed intensity (`[node+0xa4]`): 2.5 lit → 0.5 MCSH-shadowed, the
    /// day/night 1.0 at [`DAYNIGHT_T`] — the bake fold multiplies its diffuse word by this.
    pub(crate) fn intensity(&self) -> f32 {
        2.5 - 2.0 * self.t
    }

    /// The intensity chase's EFFECTIVE target: indoors the day/night point overrides the MCSH
    /// verdict; a null-fallback root (unit/player) takes that same point everywhere outdoors,
    /// because its commit never reads a per-node intensity at all; and on an outdoor-class WMO
    /// surface the LIT point overrides (`self.target` keeps the raw sample, so a GameObject
    /// stepping back onto terrain resumes from it).
    fn effective_target(&self) -> f32 {
        if self.indoor || self.null_fallback {
            DAYNIGHT_T
        } else if self.on_wmo {
            0.0
        } else {
            self.target
        }
    }

    /// Whether both chases sit on their targets — the classifier's refold gate (a Bake anchor
    /// keeps refolding its owned probe while either ramp moves). Compares against the EFFECTIVE
    /// intensity target: an indoor unit over MCSH-shadowed terrain (any unit inside a building —
    /// buildings bake their own footprint shadow) settles at the day/night point, and comparing
    /// the raw MCSH sample here kept every settled indoor Bake unit refolding forever.
    pub(crate) fn ramps_settled(&self) -> bool {
        (self.t - self.effective_target()).abs() < RAMP_EPS
            && (self.ambient - self.ambient_target).abs().max_element() < RAMP_EPS
    }

    /// Bake-entry seed (the classifier, on a lane change INTO the footprint bake): the ambient
    /// chase starts from the scene ambient the entity was just lit by, targeting the floor's
    /// cap-96 word — the reference's node carries its ramped `[+0x9c]` across the leg flip.
    pub(crate) fn seed_ambient(&mut self, from: Vec3, target: Vec3) {
        self.ambient = from;
        self.ambient_target = target;
    }
}

pub(crate) struct EntityShadePlugin;

impl Plugin for EntityShadePlugin {
    fn build(&self, app: &mut App) {
        // After the interior classifier: its exterior reclaim writes a fresh tag (shade byte 0) the
        // same frame this re-asserts the ramped byte, so the pair can't fight across frames.
        app.add_systems(Update, update_ground_shade.after(classify_entity_interior));
    }
}

/// Sample + ramp each shaded root, then push the byte to its parts' tags (change-gated per part).
#[allow(clippy::type_complexity)]
// A Bevy system's params are not an argument list to shorten — each is a distinct world access the
// scheduler needs by name, and the card pass below deliberately takes its own disjoint `MeshTag`
// query rather than smuggling one through a shared `ParamSet`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_ground_shade(
    time: Res<Time>,
    streamer: Option<Res<TerrainStreamer>>,
    adt_tiles: Res<Assets<AdtTile>>,
    mut roots: Query<(
        Entity,
        &GlobalTransform,
        &mut GroundShade,
        Option<&crate::net::SelfPlayer>,
    )>,
    children: Query<&Children>,
    // Parts are matched by carrying a `MeshTag`; interior-classified ones are skipped (their payload
    // is the packed floor colour). Fading parts are NOT skipped — shade and fade own disjoint fields.
    mut parts: Query<
        (&mut MeshTag, Option<&InteriorLit>),
        Without<crate::billboard::BillboardCard>,
    >,
    // A card is a world ROOT (the facing system owns its transform), so the descendant walk below
    // cannot reach one — it carries its owner instead. Disjoint from `parts` by the filter above.
    mut cards: Query<(&crate::billboard::BillboardCard, &mut MeshTag)>,
    // Reused across frames: every entity in a shaded root's tree → that root's shade byte, so the
    // card pass is one lookup per card instead of a walk per card.
    mut tree_shade: Local<bevy::platform::collections::HashMap<Entity, u8>>,
    mut self_log: Local<f32>,
) {
    tree_shade.clear();
    let Some(streamer) = streamer else {
        return;
    };
    let step = SHADE_RAMP_PER_SEC * time.delta_secs();
    let ambient_step = AMBIENT_RAMP_PER_SEC * time.delta_secs();
    for (root, gt, shade, is_self) in &mut roots {
        // `WOW_INTERIOR_LOG=1`: a periodic SELF-node dump (every 3 s) — the probe run's definitive
        // "what state is the parked character actually in" line, attributable unlike the
        // change-triggered `[node]` lines below (wandering NPCs fire those constantly).
        if is_self.is_some()
            && time.elapsed_secs() - *self_log > 3.0
            && std::env::var_os("WOW_INTERIOR_LOG").is_some()
        {
            let p = gt.translation();
            eprintln!(
                "[self-node] at wow ({:.1}, {:.1}, {:.1})  t {:.2} -> {:.2} (I {:.2})  indoor {} \
                 on_wmo {}",
                -p.z,
                -p.x,
                p.y,
                shade.t,
                shade.effective_target(),
                2.5 - 2.0 * shade.t,
                shade.indoor,
                shade.on_wmo,
            );
            *self_log = time.elapsed_secs();
        }
        let shade = shade.into_inner(); // one deref; field writes below are unconditional-cheap
        let pos = gt.translation();
        // Re-sample the MCSH bit only on real movement (or the very first pass) — the global
        // world→tile→chunk lookup is cheap, but a town of standing NPCs shouldn't run it per frame.
        if !shade.sampled || pos.distance_squared(shade.last_pos) >= RESAMPLE_DIST_SQ {
            match doodad_ground_shade(&streamer, &adt_tiles, pos) {
                ShadeResolve::Ready(shadowed) => {
                    shade.target = if shadowed { 1.0 } else { 0.0 };
                    shade.last_pos = pos;
                    if !shade.sampled {
                        // First landing: snap — an entity spawns already at its ground's shade
                        // (the appear-fade covers the arrival; a ramp-in from lit would read as a
                        // lighting pop right after materializing).
                        shade.t = shade.effective_target();
                        shade.sampled = true;
                    }
                }
                // The tile under the entity is requested but still decoding — keep the last state
                // and retry next frame (mirrors the doodad spawn's deferral).
                ShadeResolve::Pending => {}
            }
        }
        // Indoors the MCSH sample is moot: the day/night intensity target is 1.0 (the reference's
        // interior `[+0xf8]`, decision 0354). The sample above still ran its movement gate, so
        // stepping back outside resumes from a fresh MCSH verdict.
        let target = shade.effective_target();
        // `WOW_INTERIOR_LOG=1`: one line whenever a node's intensity target moves — the live
        // instrument for "which stage is this character actually in?" (exterior lit 2.5 ⇒ t 0,
        // MCSH-shadowed 0.5 ⇒ t 1, day/night 1.0 ⇒ t 0.75).
        if (target - shade.logged_target).abs() > f32::EPSILON
            && std::env::var_os("WOW_INTERIOR_LOG").is_some()
        {
            eprintln!(
                "[node] root {root:?} at ({:.1}, {:.1}, {:.1}) -> target t {target:.2} \
                 (I {:.2}, indoor {}) from t {:.2}",
                pos.x,
                pos.y,
                pos.z,
                2.5 - 2.0 * target,
                shade.indoor,
                shade.t,
            );
            shade.logged_target = target;
        }
        // The reference ramps: linear toward the target, never past it — intensity (as the mix
        // `t`) at 3.3333/s over its span, the ambient word at 2.0/s per channel (`0x69e770`).
        shade.t = if shade.t < target {
            (shade.t + step).min(target)
        } else {
            (shade.t - step).max(target)
        };
        let a = shade.ambient;
        let at = shade.ambient_target;
        shade.ambient = Vec3::new(
            ramp_toward(a.x, at.x, ambient_step),
            ramp_toward(a.y, at.y, ambient_step),
            ramp_toward(a.z, at.z, ambient_step),
        );
        let byte = (shade.t * 255.0).round().clamp(0.0, 255.0) as u8;
        // Push to every part below the root (body submeshes are direct children; held items/helm ride
        // joint entities deeper down — same full-tree walk as the self-fade). Change-gated per part on
        // the byte, so a settled entity writes nothing and never re-triggers render extraction.
        tree_shade.insert(root, byte);
        for part in children.iter_descendants(root) {
            // Recorded whether or not it is a drawable part: a card can follow a JOINT
            // (`following_joint` — the swinging lamp's glow), and a joint carries no `MeshTag`.
            tree_shade.insert(part, byte);
            let Ok((mut tag, lit)) = parts.get_mut(part) else {
                continue;
            };
            if lit.is_some_and(InteriorLit::is_bake) {
                continue; // the footprint-bake lane: the classifier owns the payload (probe slot)
            }
            if shade_of(tag.0) != byte {
                tag.0 = with_shade(tag.0, byte);
            }
        }
    }
    // The cards (0788's loose end). A card belongs to the same light node as the body it hangs off —
    // the reference shades every batch of an object through one node (0778) — but it is a world root,
    // so the walk above skips it and it kept the lit rung while its owner dimmed. Since 0809 that only
    // ever showed on a **GameObject**: a unit's node is pinned to the day/night point, so its body no
    // longer drops away from a card's default in the first place.
    for (card, mut tag) in &mut cards {
        let Some(byte) = card
            .follows()
            .and_then(|owner| tree_shade.get(&owner).copied())
        else {
            continue; // a fixed terrain doodad's card — its shade rides the material selector
        };
        if shade_of(tag.0) != byte {
            tag.0 = with_shade(tag.0, byte);
        }
    }
}

/// One linear ramp step toward a target, never past it (the binary's clamp-no-overshoot chase).
fn ramp_toward(v: f32, target: f32, step: f32) -> f32 {
    if v < target {
        (v + step).min(target)
    } else {
        (v - step).max(target)
    }
}
