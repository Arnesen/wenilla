//! Unit-frame **portraits** — the modern 2D take on the player/target face windows.
//!
//! The real 1.12 client renders a unit's model **once** into a tiny (64²) off-screen texture and
//! freezes it (re-baked only on model change), then stamps a round alpha stencil into it; the low
//! resolution is a 2004 shortcut, not a look (wow-re `system/ui/scratch/portrait-render.md`, §5-verified).
//! benilla keeps the *idea* — a flat 2D face in the ring — but bakes it properly: a **high-resolution**
//! off-screen render of the unit's real model, so the still is crisp. No live 3D widget sits in the UI;
//! what lands in the circle is a plain rendered image (director's call).
//!
//! ## The "photo booth"
//!
//! Each portrait slot (`"player"`/`"target"`) gets its own render layer + camera rendering into a
//! [`PortraitImages`] entry. A third slot — `"paperdoll"` (decision 0208 §5) — reuses the exact same
//! bake pipeline for the character window's **full-body** model pane: it mirrors the *player's*
//! dressed look like the `"player"` slot, but frames it through the model's own `<PlayerModel>`
//! camera ([`framing::body_frame`] — raw `cameras[1]`, not the authored bust camera the round
//! portraits use; decision 1089), bakes at 512², and spins the
//! model to a live yaw ([`PaperDollBooth`], the ref's `Model:SetRotation`). The UI samples it
//! *square*, not through the circular mask.
//!
//! The parts baked are the unit's **live dressed look** — the attach path's
//! spawned children (geosets already appearance-filtered; materials already carrying the composited
//! body skin / hair / NPC atlas), each stamped with a [`PortraitPart`] naming its static bind-pose
//! mesh twin + steady exterior material. Mirroring the children (not the shared display cache) means
//! the portrait can never drift from what's standing in the world, and gear/appearance rebuilds
//! re-bake automatically (the parts key changes). While a live unit's model is still loading, the
//! slot shows the ref's own 2D stand-in (`TemporaryPortrait-{Sex}-{Race}` / `-Monster`, RE C5) via
//! [`PortraitSource::File`].
//!
//! ## Framing: the model's own authored camera
//!
//! The framing is the model's **authored portrait camera** — the MD20 camera `cameraLookup[0]`
//! selects (VERIFIED, wow-re `system/ui/scratch/portrait-render.md` §4 + corrected verdict
//! `aa186e79`): the real bake builds `lookAt(eye, target, up-from-roll)` + the gxumath
//! *diagonal-FOV* perspective at the portrait path's fixed **4/3 aspect** — net vertical
//! half-angle `0.3·fov`, with a 3:4 anamorphic squeeze (`framing::WowPortraitProjection`) —
//! and **no** engine-side yaw or normalization on top. Every artist calibrated camera 0 to their
//! own model — that is the whole mechanism behind the ref's uniformly tight, consistently-angled
//! face crops across humans, wolves, and rabbits. It supersedes the first RE verdict's C4 ("framing
//! is not model data"), corrected on the wow-re record. A camera-less model (a few creatures,
//! props) falls back to [`frame`]'s heuristic head-anchor framing.
//!
//! ## Pose: a fresh instance at Stand
//!
//! The bake is **posed like the ref's** (wow-re §4 D2): a fresh throwaway instance — the booth's
//! own joint hierarchy + the parts' skinned twins — armed to the model's Stand (anim id 0 through
//! its own baked resolution, the ref's loader-idle seed) and frozen, never the unit's live world
//! pose. Bone riders (helm/shoulder armor, held items) ride their bone's joint, so they sit in
//! the Stand pose exactly like the world instance ([`PortraitRider`]; the ref resets the attach
//! *sockets* the same way, RE C3). See [`spawn_booth_model`].
//!
//! ## Deviations from the ref (deliberate) and what's still coarse
//!
//! High-res (256² vs 64²), a fixed neutral **studio light** on the round portraits (vs the ref's
//! ambient state — the *body* panes instead carry the reference's own `<PlayerModel>` light, see
//! [`model_pane_light`]), continuous
//! booth render vs dirty-byte bake, and the frozen Stand *phase* is t=0 (the ref's sampling clock
//! is the verdict's one unsettled INFERRED point — t≈0 vs live phase; both are Stand, and t=0
//! reproduces the ref wolf's open mouth). The creature *loading* stand-in is `-Monster` (our
//! pick — the ref's `-Pet` belongs to its pet-frame delegate).
//! `WOW_PORTRAIT_TEST=<Model\Path.mdx>` (+ `WOW_PORTRAIT_TEST_SKIN=<blp>`) bakes that model into
//! every slot (both portraits + the paper doll's body framing) to eyeball the pipeline without a server.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{PerspectiveProjection, Projection, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::Msaa;

use crate::entities::Creatures;
use crate::net::{NetEntity, SelfPlayer};
use crate::target::Selection;
use benilla_assets::materials::WowModelMaterial;

mod framing;
pub(crate) use framing::{attachment_point, head_anchor, PortraitAnchors};
use framing::{body_frame, frame, PORTRAIT_FOV};
mod booth;
use booth::{
    clear_booth_rig, spawn_booth_effects, spawn_booth_model, BoothBillboardSpec, BoothEffects,
    BoothMotion, BoothPart, BoothRider,
};
mod dressup;
pub(crate) use dressup::{DressUpBake, DressUpLook, DressUpPreview};
mod glue_booth;
pub(crate) use glue_booth::{
    CreateLook, GlueLook, GluePreview, GluePreviewBake, GlueScene, PreviewBillboard,
    PreviewEffects, PreviewPart, PreviewRider, SelectLook, GLUE_SLOT,
};
mod light;
use light::{material_variant, model_pane_light, studio_light, BoothLight};
mod test_bake;

/// The portrait slots we bake, each with its own render layer/camera: the player + target unit
/// frames, `"pet"` (decision 0990's frame), and `"npc"` — the NPC an interaction window (gossip /
/// quest / merchant / trainer / taxi) is bound to ([`crate::ui_session::InteractNpc`]), so those
/// windows show the creature's face instead of the `?` placeholder.
const SLOTS: [&str; 8] = [
    "player", "target", "pet", "npc", "party1", "party2", "party3", "party4",
];
/// The character-window **paper-doll** slot (decision 0208 §5): a full-body bake of the dressed
/// player, sampled *square* (not circular) by the character frame's model pane. Its own booth —
/// separate resolution ([`PAPERDOLL_SIZE`]), body framing ([`framing::body_frame`]), and a live yaw
/// ([`PaperDollBooth`]) — so the two portrait slots stay pixel-identical.
const PAPERDOLL_SLOT: &str = "paperdoll";
/// The **inspect** window's model pane (decision 0631 §4) — the same full-body bake as
/// [`PAPERDOLL_SLOT`], pointed at *another* player. It is the composition of what the existing
/// slots already do separately: body framing like the paper doll, an arbitrary unit like
/// `"target"`. Both run through [`sync_body_booth`]; only the unit and the yaw differ.
const INSPECT_SLOT: &str = "inspect";
/// The **pet paper doll**'s model pane (decision 1057) — the character window's tab 2. Composed
/// exactly like [`INSPECT_SLOT`] (body framing + an arbitrary unit + a live yaw), pointed at the
/// pet instead of at another player; it too runs through [`sync_body_booth`]. It gets its own booth
/// rather than sharing the `"pet"` portrait slot because that one is a 256² *bust* for the pet unit
/// frame — different subject framing, different resolution, and a yaw the portrait must never have.
const PETDOLL_SLOT: &str = "petdoll";
/// World is layer 0, the UI quad pass layer 1; portraits sit on their own high layers so nothing in the
/// world leaks into a booth and vice-versa (one layer per slot: base, base+1, …).
const PORTRAIT_LAYER_BASE: usize = 2;
/// The paper-doll booth's render layer — the next layer past the portrait slots.
const PAPERDOLL_LAYER: usize = PORTRAIT_LAYER_BASE + SLOTS.len();
/// The inspect booth's render layer — the next one past the paper doll's.
const INSPECT_LAYER: usize = PAPERDOLL_LAYER + 1;
/// The pet-doll booth's render layer — the next one past inspect's (the one ladder below).
const PETDOLL_LAYER: usize = INSPECT_LAYER + 1;
/// The glue booth's render layer — the next one past inspect's. **Every booth layer is computed
/// HERE**, in one ladder, because they were not: the glue booth (`930b327c`) and the inspect booth
/// (`ead3b0c9`) each defined "the next layer past the paper doll's" in a different file and landed
/// on the same number. Two booths sharing a layer is not a cosmetic clash — `particles::sim`
/// resolves an emitter's booth camera by *finding the first camera whose layers intersect*, so the
/// glue scene's 28 emitters addressed the INSPECT camera, which is off at the glue screens, and the
/// login screen's braziers simulated forever without ever being drawn (decision 0775).
pub(super) const GLUE_LAYER: usize = PETDOLL_LAYER + 1;
/// The **dressing room**'s render layer (decision 1060) — the next one past the glue booth's, by
/// the same ladder rule.
pub(super) const DRESSUP_LAYER: usize = GLUE_LAYER + 1;
/// The pipe_warm **twin booth**'s render layer — the next one past the dressing room's (same ladder
/// rule as [`GLUE_LAYER`]). This camera exists only while the warm pass runs
/// ([`spawn_warm_booth`]); nothing but menagerie rigs ever rides its layer.
pub(crate) const WARM_BOOTH_LAYER: usize = DRESSUP_LAYER + 1;
/// The **minimap interior composite**'s render layer (decision 1466) — the next one past the warm
/// booth's. Not a portrait booth, but it is an offscreen camera with its own layer, and 0775's rule
/// is that EVERY such layer is computed in this one ladder: the two booths that each worked out
/// "the next layer past the paper doll's" in their own file landed on the same number, and the
/// clash was silent in both rendering and the emitter→camera match.
pub(crate) const MINIMAP_COMPOSITE_LAYER: usize = WARM_BOOTH_LAYER + 1;

// The ladder must stay collision-free: a booth camera's layer is its identity for both rendering
// and the emitter→camera match, and the failure above was silent in both.
const _: () = assert!(
    PAPERDOLL_LAYER != INSPECT_LAYER
        && INSPECT_LAYER != PETDOLL_LAYER
        && PETDOLL_LAYER != GLUE_LAYER
        && PAPERDOLL_LAYER != PETDOLL_LAYER
        && PAPERDOLL_LAYER != GLUE_LAYER
        && INSPECT_LAYER != GLUE_LAYER
        && DRESSUP_LAYER > GLUE_LAYER
        && WARM_BOOTH_LAYER > DRESSUP_LAYER
        && MINIMAP_COMPOSITE_LAYER > WARM_BOOTH_LAYER,
    "booth render layers must be distinct — see GLUE_LAYER"
);
const _: () = assert!(
    PAPERDOLL_LAYER > PORTRAIT_LAYER_BASE + SLOTS.len() - 1,
    "booth layers must not overlap the per-slot portrait layers"
);
/// The baked image is high-res (vs the ref's 64²) — the crisp modern look. Square; the UI quad
/// shader cuts the inscribed circle at draw time (`ui_quad.wgsl`'s `circular`, the ref's stencil).
const PORTRAIT_SIZE: u32 = 256;
/// The paper-doll bake resolution. The pane draws ~233×224 points; at 2× hidpi that is ≈466×448, so
/// 512² covers it crisply with a little to spare (and, being sampled square, wants more than the
/// portraits' 256² for the taller full-body subject).
const PAPERDOLL_SIZE: u32 = 512;
/// Stamped on every spawned unit-model part child by the attach path ([`crate::entities`]): the
/// part's two mesh twins — the booth poses the **skinned** twin at Stand on its own throwaway
/// skeleton (the ref bake, wow-re §4 D2), the **static** bind-pose twin serving the boneless
/// fallback — and its **steady exterior material** (the child may currently wear the appear-fade
/// blend or interior variant; a portrait always wants the steady look). The booth mirrors a unit's
/// `PortraitPart` children — the exact dressed look standing in the world.
#[derive(Component)]
pub(crate) struct PortraitPart {
    pub(crate) static_mesh: Handle<Mesh>,
    /// `None` for a WMO-display part (never skins) — the booth then draws the static twin.
    pub(crate) skinned_mesh: Option<Handle<Mesh>>,
    pub(crate) material: Handle<WowModelMaterial>,
}

/// Stamped on every spawned **bone-rider** mesh child (helm / shoulder / held item —
/// [`crate::entities::equipment`]): the mesh + steady material like [`PortraitPart`], plus where it
/// sits — the body bone it rides and the attach-point offset under that bone. The posed booth
/// seats the rider under its throwaway skeleton's joint (so it rides the Stand pose exactly like
/// the world instance rides its gait); the ref resets the attach *sockets* the same way (RE C3).
///
/// It also rides a **mirror-only carrier** — a marker child that draws nothing — where the world
/// geometry it names is not a unit descendant we can stamp: an item glow's own render batches, which
/// the shared effect lane spawns under its own rig ([`crate::entities::item_glow`], decision 0822).
/// A booth bakes those at the bind pose, like any other rider.
#[derive(Component)]
pub(crate) struct PortraitRider {
    pub(crate) static_mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    /// The body-skeleton bone the rider's joint entity belongs to.
    pub(crate) bone: u16,
    /// The attach point's Bevy-space offset under that bone ([`crate::entities::BoneAttach`]).
    pub(crate) offset: Vec3,
}

/// Stamped on a lightweight **anchor child** the attach path plants under a unit for each
/// camera-facing batch it dresses in. The visible world card is a *root-spawned* entity
/// ([`benilla_world::billboard::BillboardCard`]), never a unit descendant, so it can't be mirrored; this
/// marker rides the unit's tree purely so the portrait / paper-doll booths (which mirror the unit's
/// dressed descendants) can rebuild the batch as a booth card ([`BoothBillboardSpec`] +
/// [`booth::face_booth_billboards`]) — the same reconstruction the char-create glue path does from
/// its own parts ([`PreviewBillboard`]). Carries the centred quad, its material, where it sits, and
/// the billboard flag.
///
/// Three sources plant one, exactly as on the glue path (decision 0822):
///
/// - The character's own **eye-glow** (undead/night-elf, geoset 302 / geoset 0 — a fullbright quad
///   on the eye bone), planted by [`crate::entities::attach`].
/// - An **equipped item's** camera-facing batch — a wand's gem, the held torch's `GLOWWHITE32` halo
///   (270 of the 2681 `Item\` models author one), planted by
///   [`crate::entities::equipment`]'s attach.
/// - An **item glow's** batch (decision 0805) — `Spells\Enchantments\Sparkle_A.m2` is one additive
///   quad and nothing else — planted by [`crate::entities::item_glow`].
#[derive(Component)]
pub(crate) struct PortraitBillboard {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    /// The BODY bone whose booth joint the card seats on — the eye bone for the character's own
    /// glow, the item's attach-point bone for anything an item wears.
    pub(crate) bone: u16,
    pub(crate) seat: PortraitSeat,
    pub(crate) kind: benilla_formats::BillboardKind,
}

/// Whose model a mirrored camera-facing batch belongs to — which decides both where its pivot comes
/// from and whether a *mounted* unit's booth keeps it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PortraitSeat {
    /// A batch of the **rigged host model** itself (the eye-glow). Its bone's joint frame already
    /// bakes the bone pivot (the 0130 rig identity), so it needs no offset — and it belongs to that
    /// model's body, so a mount's own glow card prunes with the mount's meshes (a portrait shows the
    /// rider alone, never the horse — decision 0441).
    Body,
    /// A batch of a **rig-less rider** — an equipped item's, an item glow's — at its seat in the
    /// bone's joint frame (Bevy axes): the attach point **plus the batch's own model-local pivot**,
    /// because with no rig nothing else will bake that pivot. Collected like a [`PortraitRider`]
    /// whatever the mount state: while mounted, the character's own joints re-root INSIDE the mount
    /// subtree, and the rider's gear must survive that.
    Rider(Vec3),
}

impl PortraitSeat {
    /// The offset a booth seats this batch at under its bone's joint.
    pub(crate) fn offset(self) -> Vec3 {
        match self {
            PortraitSeat::Body => Vec3::ZERO,
            PortraitSeat::Rider(at) => at,
        }
    }
}

/// Stamped on an **effect-bearing model** riding a unit — the equipped item whose own emitters are
/// its whole look (the R14 PVP pauldron's `SPARKLE` twinkle, the held torch's flame — decision 0813,
/// `#bugs` B118) and the `ItemVisuals` glow a held weapon hangs on its own attachment points
/// (decision 0805). The world emitters are *free* entities the owner contract walks
/// ([`benilla_world::particles::spawn_emitter`]), never unit descendants, so — like [`PortraitBillboard`] —
/// this marker is how a booth learns they exist at all: the mirror carries the emitter records plus
/// the composed seat, and the booth spawns its own copies against its own camera
/// ([`booth::spawn_booth_effects`]). The glue path's [`PreviewEffects`] is the same carry assembled
/// from data instead of mirrored from entities.
///
/// Which booths act on it is a *fidelity* split, not a cost one — see
/// [`booth::spawn_booth_effects`]: the body panes are live `<PlayerModel>` widgets in the reference,
/// the round portraits a one-shot cached bake.
#[derive(Component)]
pub(crate) struct PortraitEffects {
    /// The BODY bone whose booth joint the effect model's host seats on.
    pub(crate) bone: u16,
    /// The host's offset in that bone's joint frame (Bevy axes) — the item's attach point, plus the
    /// glow slot's offset on the item's own model when this is a glow.
    pub(crate) offset: Vec3,
    pub(crate) emitters: Vec<benilla_assets::ModelEmitter>,
}

/// What a portrait slot currently shows — the booth's live bake, or the ref's 2D stand-in file
/// while a unit's model is still streaming in (RE C5).
#[derive(Clone, PartialEq)]
pub(crate) enum PortraitSource {
    /// The slot's off-screen render target (the model bake).
    Live(Handle<Image>),
    /// A flat portrait BLP (`Interface\CharacterFrame\TemporaryPortrait-…`), resolved by the UI
    /// extract through the standard sprite path.
    File(String),
}

/// The bridge between the booth and the UI: unit token (`"player"`/`"target"`) → what its portrait
/// region shows. The UI extract pass ([`crate::ui_script`]) reads it for a `SetPortraitTexture`-bound
/// region; the booth writes it (Live ↔ File transitions included).
#[derive(Resource, Default)]
pub(crate) struct PortraitImages(pub(crate) HashMap<String, PortraitSource>);

/// The paper-doll model pane's live input (decision 0208 §5): the bake **yaw** in radians — the
/// ref's `Model:SetRotation` convention (rotate-left *decrements*; the pane's default is `0.61`, a
/// three-quarter view). The character window's feed writes `yaw` each frame (from the rotate
/// buttons / drag); the [`PAPERDOLL_SLOT`] booth spins the model root to match and re-bakes only
/// when it (or the dressed look) changes — never every frame. The bake lands in [`PortraitImages`]
/// under the `"paperdoll"` key, sampled square by the model pane's region.
#[derive(Resource)]
pub(crate) struct PaperDollBooth {
    pub(crate) yaw: f32,
}

impl Default for PaperDollBooth {
    fn default() -> Self {
        Self { yaw: 0.61 }
    }
}

/// The inspect window's model pane input (decision 0631 §4) — the [`PaperDollBooth`] twin, plus
/// the one thing the paper doll never needs: **which unit**. `unit` is the entity
/// [`crate::ui_inspect`] resolved the inspected token to this frame, `None` when nothing is being
/// inspected (or the target isn't streamed), which empties the booth.
#[derive(Resource)]
pub(crate) struct InspectBooth {
    pub(crate) yaw: f32,
    pub(crate) unit: Option<Entity>,
}

impl Default for InspectBooth {
    fn default() -> Self {
        Self {
            yaw: 0.61,
            unit: None,
        }
    }
}

/// The pet paper doll's model pane input (decision 1057) — the [`InspectBooth`] shape exactly:
/// a yaw the pane's rotate buttons write, and the unit [`crate::ui_pet_doll`] resolved the pet
/// token to this frame (`None` with no pet out, or while its object hasn't streamed — which empties
/// the booth, the same "no pet" the rest of the page shows).
#[derive(Resource)]
pub(crate) struct PetDollBooth {
    pub(crate) yaw: f32,
    pub(crate) unit: Option<Entity>,
}

impl Default for PetDollBooth {
    fn default() -> Self {
        Self {
            yaw: 0.61,
            unit: None,
        }
    }
}

/// The key identifying what a booth currently has baked. Any change in the unit's dressed look
/// (gear swap, appearance refresh, different unit) changes it → re-bake.
#[derive(PartialEq)]
struct LookKey {
    /// The (mesh, material) identity of every mirrored draw — [`PortraitPart`],
    /// [`PortraitRider`], [`PortraitBillboard`].
    draws: Vec<(AssetId<Mesh>, AssetId<WowModelMaterial>)>,
    /// One entry per mirrored [`PortraitEffects`]: its seat (bone + offset bits) and how many
    /// emitters it carries. Effects carry no asset handle to key on, and they can arrive **later**
    /// than the meshes they ride with — an item glow resolves asynchronously
    /// ([`crate::entities::item_glow`]: a template round trip, then the effect model's own load) —
    /// so a key blind to them would leave the glow out of the bake until some unrelated edge forced
    /// a re-bake.
    effects: Vec<(u16, [u32; 3], usize)>,
}

impl LookKey {
    /// The key for one mirrored dressed look, in the order [`DressedLook::collect`] returns it.
    fn build(
        parts: &[&PortraitPart],
        riders: &[&PortraitRider],
        billboards: &[&PortraitBillboard],
        effects: &[&PortraitEffects],
    ) -> Self {
        LookKey {
            draws: parts
                .iter()
                .map(|p| (p.static_mesh.id(), p.material.id()))
                .chain(riders.iter().map(|r| (r.static_mesh.id(), r.material.id())))
                .chain(billboards.iter().map(|b| (b.mesh.id(), b.material.id())))
                .collect(),
            effects: effects
                .iter()
                .map(|e| {
                    (
                        e.bone,
                        e.offset.to_array().map(f32::to_bits),
                        e.emitters.len(),
                    )
                })
                .collect(),
        }
    }
}

/// Per-slot booth: its render layer, the model-root entity (children = the baked model's meshes),
/// its render-target image (so the bridge can flip back to `Live` after a `File` stand-in), and the
/// parts key currently baked (`None` = empty booth).
struct Booth {
    layer: RenderLayers,
    root: Entity,
    target: Handle<Image>,
    baked: Option<LookKey>,
    /// Demand-render window (decision 0540): frames [`gate_booth_cameras`] still keeps this
    /// booth's camera active. Armed to [`BOOTH_SETTLE_FRAMES`] by every content edge (bake,
    /// empty, framing/yaw write); 0 with `pending` drained = the camera sleeps and the target
    /// keeps the last render — a still costs nothing per frame.
    wake: u32,
    /// The bake standing in this booth is a **live widget**, not a still: its Stand loops and its
    /// item emitters run ([`booth::spawn_booth_effects`]), so it must re-render every frame — but
    /// only while something is drawing it, which [`BoothPanes`] answers (decision 1069). The
    /// generalization of the glue booth's own always-on rule (`live_scene` below, decision 0540);
    /// only the body panes set it (decision 0822 §4 — the round portraits are a one-shot bake).
    live: bool,
    /// Textures the last bake referenced that were not yet resident: the camera stays awake
    /// until each lands (an `mpq://` image arriving after the bake would otherwise be frozen
    /// OUT of the still forever), then renders one final resident frame.
    pending: Vec<Handle<Image>>,
    /// When the current `pending` hold began (wall secs) — `None` while it is empty. The hold
    /// was designed for a texture in flight, not one that will NEVER land (a failed load leaves
    /// no `Assets<Image>` entry): unbounded, that pinned the camera rendering forever behind a
    /// closed window. [`gate_booth_cameras`] releases the hold at [`PENDING_LANDING_SECS`]
    /// with a warn — the still keeps whatever did land.
    pending_since: Option<f64>,
    /// The **destination pane's** aspect this booth's camera is currently framed for
    /// ([`framing::WowPortraitProjection::aspect`], decision 1069) — 1.0 until the UI has drawn the
    /// pane once, then sticky: a hidden window must not re-frame the bake back to square.
    aspect: f32,
    /// A rig stands in this booth ([`booth::BoothRig::rigged`] at the last bake) — the park
    /// gate's "is there anything to park". Cleared with the emptied booth.
    rigged: bool,
    /// The booth scene is parked: its camera is asleep, so [`gate_booth_cameras`] put
    /// [`benilla_world::rig_anim::AnimParked`] on the root — the 0712 evaluator, the pose
    /// composes, the palette writes and the global-sequence writes all skip (the pose HOLDS —
    /// the buffer is state), and the booth-lane emitters freeze under the draw-set law
    /// (`particles::sim` — the reference only ticks an emitter its frame draws). A camera wake
    /// drops the marker before the animation lane runs, so the wake window's first render
    /// already animates. Costs nothing while a window is open; retires the whole idle lane the
    /// rest of the session.
    parked: bool,
}

/// How many frames a content edge keeps a booth camera rendering ([`Booth::wake`]): covers the
/// command-applied spawn, the billboard re-face's one-frame lag, and GPU upload of fresh meshes.
const BOOTH_SETTLE_FRAMES: u32 = 4;

/// This rig stands on a booth stage, not in the world: its `AnimParked` marker is owned by
/// [`gate_booth_cameras`] alone, and the world-view parker (`creature_anim::lod`) filters it out
/// (decision 1447). One marker, one writer — the same split the doodad draw gate holds for
/// placed doodads (decision 1365). The hazard is structural: a booth stage sits outside every
/// world frustum by construction, so an unfiltered view parker freezes every pane moments after
/// its bake — while this gate, tracking only its own park edges, cannot heal the foreign marker.
/// [`booth::BoothRig::finish`] inserts it beside the `RigPose`; [`booth::clear_booth_rig`]
/// strips it with the rest. Policy, not machinery — which is why it lives here and not in
/// `benilla_world::rig_anim` beside the marker it fences (the 1160 line).
#[derive(bevy::prelude::Component)]
pub(crate) struct StageRig;

/// How long a [`Booth::pending`] texture hold may keep the camera awake (wall secs) before it is
/// declared never-landing and released ([`Booth::pending_since`]). Generous against a real load —
/// an MPQ image lands in well under a second — because a premature release only costs a stale
/// still, while the old unbounded hold cost a forever-rendering camera.
const PENDING_LANDING_SECS: f64 = 10.0;

/// `WOW_BOOTH_LOG=1` — is the booth instrument armed? (Read once.)
fn booth_log() -> bool {
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LOG.get_or_init(|| std::env::var_os("WOW_BOOTH_LOG").is_some())
}

/// `WOW_BOOTH_LOG=1` — one line per BAKE DECISION, which is the half the gate timeline cannot
/// show: the gate says a camera *drew*, never *what it drew*. B106's first-login shot is a
/// portrait wearing no armour beside a fully-equipped character, so the question is exactly "how
/// many parts/riders did the bake see, and did it ever re-bake when the rest arrived".
/// `verb`: `bake` committed · `wait` abandoned (a source material was not resident, 0744) ·
/// `stand-in` the 2D fallback while no parts are attached · `empty` the booth was cleared.
fn log_bake(
    token: &str,
    verb: &str,
    parts: &[&PortraitPart],
    riders: &[&PortraitRider],
    billboards: &[&PortraitBillboard],
    effects: &[&PortraitEffects],
) {
    if booth_log() {
        eprintln!(
            "[booth] {token} {verb} parts={} riders={} billboards={} fx={}/{}",
            parts.len(),
            riders.len(),
            billboards.len(),
            effects.len(),
            effects.iter().map(|e| e.emitters.len()).sum::<usize>(),
        );
    }
}

/// The framing anchors for a bake — **`None` means "not ready, come back next frame", never
/// "this display has no anchors".**
///
/// `Creatures::display_anchors` returns `None` in exactly two states, both transient: the display
/// is not in the cache yet, or its `parts` are *"not yet built"* (its own comment). Once built it
/// always answers `Some`; a display that genuinely has no authored portrait camera answers `Some`
/// with `camera: None` inside, which the framing heuristics are built to handle.
///
/// Both bake sites used to paper over that with `unwrap_or(PortraitAnchors { .., pivot_height: 0.0,
/// .. })`. That is not a neutral default: the retired body fit floored the head signal at `0.1` for
/// "a hypothetical bounds-less display", so zero anchors aimed the camera at a 0.1-unit-tall
/// subject — the paper doll "zoomed into the max" and the wrong-size portrait of `#bugs` B106. (1089
/// retired that fit, and zero anchors are no longer a *zoom* — but they are still the wrong camera:
/// an unbuilt display has no `cameras[1]` to read, so the pane would latch the fixed fallback rig
/// aimed at a zero bbox centre.) And it latches: the camera is aimed once per bake, and the parts
/// key cannot change afterwards
/// (handle ids are stable from `load()`, so mesh/model arrival is invisible to it), so the frame
/// stays wrong until some unrelated content edge forces a re-bake.
///
/// A display id of `None` is a different case — there is nothing to wait for — and keeps the
/// bounds-less anchors it always had.
fn booth_anchors(
    creatures: Option<&Creatures>,
    display_id: Option<u32>,
) -> Option<PortraitAnchors> {
    let Some(display_id) = display_id else {
        return Some(PortraitAnchors {
            camera: None,
            pane_camera: None,
            bbox_center: Vec3::ZERO,
            head: None,
            pivot_height: 0.0,
            ground_radius: 0.0,
        });
    };
    creatures?.display_anchors(display_id)
}

/// `WOW_BOOTH_LOG=1` — the resolved framing for a bake: which anchors it had and where the camera
/// ended up. An opaque near-black pane is what a camera parked *inside* the model renders, so
/// "black" and "zoomed to max" may be one symptom; this is the line that tells them apart.
fn log_frame(token: &str, a: &PortraitAnchors, cam: &Transform) {
    if booth_log() {
        eprintln!(
            "[booth] {token} frame eye=({:.3},{:.3},{:.3}) authored_cam={} pivot={:.3} head_y={:.3} gr={:.3}",
            cam.translation.x,
            cam.translation.y,
            cam.translation.z,
            a.camera.is_some(),
            a.pivot_height,
            a.head.map_or(f32::NAN, |h| h.y),
            a.ground_radius,
        );
    }
}

/// Arm `booth` after a content edge: render the settle window, plus every frame until each twin
/// material's texture is resident. `twins` = the material handles the bake just installed.
fn wake_booth<'a>(
    booth: &mut Booth,
    mats: &Assets<WowModelMaterial>,
    twins: impl Iterator<Item = &'a Handle<WowModelMaterial>>,
) {
    booth.wake = BOOTH_SETTLE_FRAMES;
    booth.pending = twins
        .filter_map(|h| mats.get(h))
        .filter_map(|m| m.base.base_color_texture.clone())
        .collect();
    // A fresh hold gets a fresh clock ([`Booth::pending_since`]) — the gate stamps it.
    booth.pending_since = None;
}

#[derive(Resource, Default)]
struct Booths(HashMap<String, Booth>);

/// Where each booth's bake is actually being **sampled on screen this frame**: slot token → the
/// destination region's aspect (width ÷ height). Published by the UI extract for every
/// `BenillaSetBoothTexture` binding it emits — the *square* portrait binding (decision 0208 §5); a
/// round `SetPortraitTexture` unit portrait is not a pane and never appears here.
///
/// Two things need it, and neither can be a constant (decision 1069):
///
/// - **Shape.** A booth renders into a square target that the UI stretches to fill the pane's rect
///   (`extract`'s `UvRect::FULL`), so the projection has to run at the *pane's* aspect for the
///   stretch to cancel. Rendering at 1.0 into the dressing room's 316×351 pane made every
///   character 11% too tall (director report, 2026-08-06).
/// - **Liveness.** A body pane's bake *animates* ([`BoothMotion::Loop`] — the reference's
///   `<PlayerModel>` widgets render live, decision 0822 §4), so its camera renders every frame it
///   is on screen — and, now that this resource can say so, **none** when it is not. That last
///   half is also a strict win for the pre-1069 emitter case, which used to render forever behind
///   a closed window.
///
/// One frame stale by construction (the extract runs after the booth syncs), which is why the
/// aspect is latched into [`Booth::aspect`] rather than read fresh: a window that closes must not
/// re-frame the bake it left standing.
#[derive(Resource, Default)]
pub(crate) struct BoothPanes(pub(crate) HashMap<String, f32>);

/// Both directions of the booth↔UI bridge in one system param: the bake a bound region **samples**
/// ([`PortraitImages`]) and the pane geometry the extract **publishes** back ([`BoothPanes`]).
///
/// They travel together because they are the same seam, and because `drive_script` had already
/// reached Bevy's 16-parameter ceiling — two more `Res`es there is one too many.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct BoothBridge<'w> {
    pub(crate) images: Res<'w, PortraitImages>,
    pub(crate) panes: ResMut<'w, BoothPanes>,
}

/// Tags a booth camera with its slot token, so the model-sync pass can re-frame it per model.
/// (`BoothCam`, not `PortraitCamera` — that name is the authored M2 rig, `benilla_assets::PortraitCamera`.)
/// (`pub(crate)` for the particle census, which reports draws per booth token — decision 0775.)
#[derive(Component)]
pub(crate) struct BoothCam(pub(crate) String);

/// **The display aspect the client's `screencoord` scale is computed from** — its `gxResolution`
/// CVar's `width/height`, gated by the `widescreen` CVar (registered with default `"1"`, so on by
/// default; with it off the client uses `4/3` on any monitor). Its one consumer is
/// [`framing::pane_model_scale`], the model-root renormalize factor a `<PlayerModel>` pane applies
/// to a model with no camera of its own.
///
/// **A stated deviation:** benilla has neither CVar yet, so this reads the primary window's own
/// aspect. That is the same number whenever the client runs at its display's native resolution, and
/// ours always does — we expose no resolution mode list to disagree with. When the graphics CVars
/// land, this resource is the one place to repoint.
///
/// Defaults to `4/3` — the client's own `.data` default pair, and the aspect at which the factor is
/// exactly `1.0`, so a booth that bakes before the window is measured is unscaled rather than wrong.
#[derive(Resource)]
pub(crate) struct GxAspect(pub(crate) f32);

impl Default for GxAspect {
    fn default() -> Self {
        Self(4.0 / 3.0)
    }
}

/// Track the primary window's aspect into [`GxAspect`] (see its deviation note). Cheap enough to run
/// every frame; the booths latch it, so a resize re-scales the standing bake exactly once.
fn feed_gx_aspect(
    mut gx: ResMut<GxAspect>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    let Ok(w) = window.single() else {
        return;
    };
    let (px_w, px_h) = (w.resolution.width(), w.resolution.height());
    if px_h > 0.0 {
        let a = px_w / px_h;
        if (a - gx.0).abs() > 1e-4 {
            gx.0 = a;
        }
    }
}

/// The body panes' render rate (decision 1444): while a `<PlayerModel>`-family pane (paper
/// doll, dressing room, inspect, pet) is on screen, its booth camera renders every OTHER frame
/// instead of every frame. The pane's per-frame bill is the **second render pass itself** —
/// 1441's trace put ~0.9 ms/frame in graph re-run + drawable pressure, and 1443 measured the
/// open-pane delta at +2.7 with the pass running every frame — so a 512²-class doll at 30 fps
/// is the cheapest half of that back. The pose keeps evaluating at full rate (the park is not
/// touched); only the camera skips, so the held frame is always one the pose just produced.
///
/// `boothHalfRate` is **benilla's own CVar** (the reference has no second view to rate-limit —
/// its doll draws in the main pass, 1069's known rent). Default ON; whether a 30 fps doll
/// *reads* right is the director's call (§7) — flip it live with
/// `/script SetCVar("boothHalfRate", 0)` to A/B. Booth-lane item emitters tick only on drawn
/// frames (the draw-set law, `particles::sim`), so at half rate a sparkle cloud advances at
/// half speed — the law's own honest consequence, flagged for the same morning eye.
///
/// The glue screens (`live_scene`) are exempt: a fullscreen create/select scene at 30 fps is
/// not a pane crop, and those screens have no world behind them to pay for.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PaneRate {
    pub(crate) half: bool,
}

impl Default for PaneRate {
    fn default() -> Self {
        Self { half: true }
    }
}

/// The two **framing inputs** a body booth reads, in one param: the pane geometry the UI extract
/// publishes ([`BoothPanes`]) and the display aspect ([`GxAspect`]).
///
/// Bundled rather than passed side by side because these systems sit on Bevy's 16-parameter ceiling
/// — the same reason [`BoothBridge`] exists on the other side of the seam.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct BoothFraming<'w> {
    pub(crate) panes: Res<'w, BoothPanes>,
    pub(crate) gx: Res<'w, GxAspect>,
}

/// Owns the portrait bake pipeline: the [`PortraitImages`] bridge + the per-slot off-screen booths.
pub(crate) struct PortraitPlugin;

impl Plugin for PortraitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PortraitImages>()
            .init_resource::<PaneRate>()
            .init_resource::<PaperDollBooth>()
            .init_resource::<InspectBooth>()
            .init_resource::<PetDollBooth>()
            .init_resource::<glue_booth::GluePreview>()
            .init_resource::<glue_booth::GluePreviewBake>()
            .init_resource::<dressup::DressUpPreview>()
            .init_resource::<dressup::DressUpBake>()
            .init_resource::<Booths>()
            .init_resource::<BoothPanes>()
            .init_resource::<GxAspect>()
            .init_resource::<BoothLight>()
            .add_systems(Startup, setup_booths)
            // The variant-cache reaper: booth twins die with their world source material.
            .add_systems(Update, (light::reap_dead_variants, feed_gx_aspect))
            // The `"npc"` token's entity is resolved by `ui_session`'s own plugin (it is shared with
            // the interaction face-me, decision 1467) — the booths read whatever it last published.
            // Here the test bake owns the booths when its env is set (the live syncs yield to it),
            // and the paper-doll sync runs last (it shares the camera/booth/image resources, so the
            // chain keeps the access ordered).
            .add_systems(
                Update,
                (
                    test_bake::sync_test_portraits,
                    sync_portraits,
                    sync_paperdoll,
                    sync_inspect_booth,
                    sync_petdoll_booth,
                    glue_booth::sync_glue_booth,
                    glue_booth::sync_glue_scene,
                    dressup::sync_dressup_booth,
                    // Last: it reads the wake/pending state every sync above may have armed.
                    gate_booth_cameras,
                )
                    .chain(),
            )
            // Re-face each booth's eye-glow cards to its own camera (reads last-propagate joint
            // globals; unordered w.r.t. the syncs — a fresh card just faces forward one frame).
            .add_systems(Update, booth::face_booth_billboards)
            // The booth twin of the world visibility authority's render-alpha write (decision 0807):
            // push each booth part's sampled `MatAnim` onto its tag. Ordered after the shared
            // sampler, the only producer of `MatAnim::current`, so it moves THIS frame's value.
            .add_systems(
                Update,
                booth::push_booth_mat_alpha.after(benilla_world::doodad_anim::sample_mat_anim),
            )
            // The phase-3 preview instrument (`WOW_CREATE_TEST`, decision 0423): inert without the env.
            .add_systems(Update, glue_booth::drive_create_test)
            // `WOW_BOOTH_DUMP=<token>:<path>:<secs>` — photograph a booth's render target to disk
            // (the headless eye on "what is the paperdoll actually showing right now"; the
            // first-login black-pane hunt). Inert without the env.
            .add_systems(Update, test_bake::dump_booth_target);
    }
}

/// A fresh transparent render-target image of `size²`, usable as a camera target and sampled by the
/// UI. Portrait slots pass [`PORTRAIT_SIZE`]; the paper doll passes [`PAPERDOLL_SIZE`].
fn new_target_image(size: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0; 8],
        // **Un-encoded on purpose, and at float precision — the two are separate choices.**
        //
        // Un-encoded (no `…Srgb` label) is measured: with an Srgb label the composited portrait read
        // one net sRGB-decode too dark vs the world render of the same model, because the UI arc
        // composites in GAMMA bytes and does its one decode at the end (`ui_gamma`, decisions
        // 0254/0541) — a booth target that pre-encodes lands a second encode in that chain. So the
        // target holds the booth's own un-encoded values and the UI shader encodes them.
        //
        // FLOAT, not `Rgba8Unorm`, is B126 (decision 0804). Quantizing *un-encoded* values to 8 bits
        // is a precision collapse exactly where the eye is most sensitive: the only display levels
        // reachable below display byte 100 are `srgb(k/255)` = 0, 13, 22, 28, 34, 38, … — ~25 steps
        // where the 8-bit gamma backbuffer this feeds has 100. That is the reported "colors are 16
        // bit instead of 32" banding on the glue screens' trees and skybox, measured on the
        // histogram of a char-select capture (15 of 16 predicted ladder values hit). The pipeline's
        // *semantics* are unchanged by this — same values, same single encode downstream, just not
        // rounded to a 256-step linear grid on the way through.
        TextureFormat::Rgba16Float,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    image
}

/// The booth **view shape** — the components that set a booth camera's *pipeline key space*:
/// HDR float intermediate + `Tonemapping::None` (the world camera's exact shape — the measured
/// whys sit on the portrait-slot spawn below) at `Msaa::Off`. Defined ONCE and spawned by every
/// booth camera — the portrait slots, the two body panes, the glue booth, and pipe_warm's twin
/// booth — because the warm pass compiles the samples=1 twin of every model pipeline against
/// exactly this shape behind the loading cover (decisions 0938/0958): a booth camera whose shape
/// drifts from the warm booth's is a live pipeline stall on its first bake.
fn booth_view_shape() -> impl Bundle {
    (
        Camera3d::default(),
        bevy::render::view::Hdr,
        Tonemapping::None,
        Msaa::Off,
    )
}

/// The pipe_warm **twin booth** (decision 0958). The real booths spawn with a
/// `PerspectiveProjection` placeholder and render the menagerie's twins under bevy_pbr's
/// PERSPECTIVE view key — but nearly every real bake installs
/// `Projection::custom(WowPortraitProjection)` ([`frame`]'s authored path; [`body_frame`]
/// always), which is the NONSTANDARD view key: a distinct pipeline per variant, so the whole
/// samples=1 twin space compiled AGAIN, live, on the first target/paperdoll bake after entry.
/// This camera is that missing key space: the booth view shape exactly, with the custom
/// projection class real bakes use. pipe_warm spawns it beside the menagerie and despawns it at
/// drain. A REAL booth's projection is deliberately never stamped for warming — a bake completing
/// during the warm window would keep a misframed image. (The placeholder-Perspective class stays
/// warmed through a real booth: [`frame`]'s camera-less fallback still bakes with it.)
pub(crate) fn spawn_warm_booth(
    commands: &mut Commands,
    images: &mut Assets<Image>,
) -> (Entity, RenderLayers) {
    let layer = RenderLayers::layer(WARM_BOOTH_LAYER);
    let image = images.add(new_target_image(PORTRAIT_SIZE));
    let cam = commands
        .spawn((
            booth_view_shape(),
            Camera {
                order: -100 + WARM_BOOTH_LAYER as isize,
                clear_color: ClearColorConfig::Custom(Color::srgb(0.055, 0.045, 0.04)),
                ..default()
            },
            RenderTarget::Image(image.into()),
            benilla_world::ffx_glow::FfxGlow::BOOTH,
            Projection::custom(framing::WowPortraitProjection {
                fov: framing::PANE_FIXED_FOV,
                near: 0.02,
                far: 100.0,
                // The warm pass compiles PIPELINES, which neither aspect keys.
                aspect: 1.0,
                crop_aspect: 1.0,
            }),
            layer.clone(),
        ))
        .id();
    (cam, layer)
}

/// Startup: stand up one booth per slot — its image (registered in [`PortraitImages`]), a model-root
/// entity, and a camera rendering only that slot's layer into the image (transparent, no bloom/MSAA,
/// rendered before the world camera via a negative order).
#[allow(clippy::too_many_arguments)]
fn setup_booths(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut portraits: ResMut<PortraitImages>,
    mut booths: ResMut<Booths>,
    mut booth_light: ResMut<BoothLight>,
    mut mirrors: ResMut<benilla_world::rig_palette::RigPaletteMirrors>,
    device: Res<bevy::render::renderer::RenderDevice>,
    queue: Res<bevy::render::renderer::RenderQueue>,
) {
    // The studio-light buffer: same LAYOUT as the shared world light, written once, never touched
    // again (fixed values — no per-frame upload, no render-world system). Sized to the FULL blob
    // (`light_blob_bytes` — header rows + the 0278 point-light table): the model shader declares the
    // whole struct and wgpu validates the bound size against it at every draw. Only the studio header
    // rows are written; wgpu zero-initializes the rest, so `point_count = 0` and no scene point light
    // ever touches a portrait (the studio look is deliberately static).
    let light_buffer = |label: &'static str, blob: benilla_world::lighting::LightBlob| {
        let buffer = blob.create(&device, label);
        blob.write(&queue, &buffer);
        buffer
    };
    booth_light.studio.buffer = Some(light_buffer("wow_portrait_light", studio_light()));
    // The body panes' own light — the reference `<PlayerModel>` widget's (see the fn's doc).
    booth_light.pane.buffer = Some(light_buffer("wow_model_pane_light", model_pane_light()));
    // Booth rigs skin from the palette regions of THESE buffers (decision 0720): register both
    // as mirrors so the palette upload keeps their regions live.
    for (key, buf) in [
        ("portrait", &booth_light.studio.buffer),
        ("pane", &booth_light.pane.buffer),
    ] {
        if let Some(b) = buf {
            mirrors.0.insert(key, b.clone());
        }
    }

    for (i, token) in SLOTS.iter().enumerate() {
        let image = images.add(new_target_image(PORTRAIT_SIZE));
        portraits
            .0
            .insert((*token).to_string(), PortraitSource::Live(image.clone()));
        let layer = RenderLayers::layer(PORTRAIT_LAYER_BASE + i);
        let root = commands
            .spawn((Transform::IDENTITY, Visibility::Visible, layer.clone()))
            .id();
        commands.spawn((
            // The shared booth view shape — HDR intermediate, no tonemap, no MSAA (its doc has
            // the whys); per-slot here: the order, the backdrop, the glow node, the projection.
            booth_view_shape(),
            Camera {
                // Render the booths first (negative order), so the freshly baked image is ready when
                // the world + UI cameras run. One order per slot to keep them distinct.
                order: -100 + i as isize,
                // The ref bake's opaque near-black backdrop (the world must never show through the
                // circle); the round cut happens at draw time (ui_quad.wgsl's circular mask).
                clear_color: ClearColorConfig::Custom(Color::srgb(0.055, 0.045, 0.04)),
                ..default()
            },
            // The render target is its own component in Bevy 0.18 (Camera `#[require]`s it).
            RenderTarget::Image(image.clone().into()),
            // The gamma lane (0161): booth materials emit gamma bytes like the world's, so the
            // booth needs the same final node — the FFXGlow combine owns the frame's ONE decode.
            // This also keeps the bake at exact world parity (same glow, same transform chain);
            // without it the portrait reads one encode too bright.
            benilla_world::ffx_glow::FfxGlow::BOOTH,
            Projection::from(PerspectiveProjection {
                fov: PORTRAIT_FOV,
                near: 0.02,
                far: 100.0,
                ..default()
            }),
            layer.clone(),
            BoothCam((*token).to_string()),
        ));
        booths.0.insert(
            (*token).to_string(),
            Booth {
                layer,
                root,
                target: image,
                baked: None,
                wake: 0,
                live: false,
                pending: Vec::new(),
                pending_since: None,
                aspect: 1.0,
                rigged: false,
                parked: false,
            },
        );
    }

    // The three **body** booths — the character window's paper doll (decision 0208 §5), the
    // inspect window's pane (decision 0631 §4) and the pet paper doll's (decision 1057). Same
    // off-screen pipeline as the portrait slots (transparent target, HDR + the FFXGlow node,
    // negative order so the bake is ready before the world/UI cameras), but their own 512²
    // targets, their own layers, and a body-framing projection (aimed per-bake by
    // `sync_body_booth`). Kept a separate spawn from the portrait loop above so these cameras
    // stay byte-for-byte what the director approved.
    for (i, (slot, layer_index)) in [
        (PAPERDOLL_SLOT, PAPERDOLL_LAYER),
        (INSPECT_SLOT, INSPECT_LAYER),
        (PETDOLL_SLOT, PETDOLL_LAYER),
    ]
    .into_iter()
    .enumerate()
    {
        let image = images.add(new_target_image(PAPERDOLL_SIZE));
        portraits
            .0
            .insert(slot.to_string(), PortraitSource::Live(image.clone()));
        let layer = RenderLayers::layer(layer_index);
        let root = commands
            .spawn((Transform::IDENTITY, Visibility::Visible, layer.clone()))
            .id();
        commands.spawn((
            booth_view_shape(),
            Camera {
                order: -100 + (SLOTS.len() + i) as isize,
                // **TRANSPARENT** — a `<PlayerModel>` widget has no backdrop of its own (decision
                // 1083). The reference's three model frames declare nothing but the widget; what
                // fills the pane behind the figure is the PAGE's own art, the same
                // `UI-Character-CharacterTab-*` / `UI-Character-General-*` plates our transcription
                // already draws, and the model composites over them. Baking an opaque backdrop into
                // the target painted those plates out — the director's report is exactly that: "our
                // paperdoll is pure black bg instead of having what the ref has, slightly off black
                // and textured" (2026-08-07). The glue booth has composited this way since 0807;
                // the combine passes scene alpha through untouched.
                clear_color: ClearColorConfig::Custom(Color::NONE),
                ..default()
            },
            RenderTarget::Image(image.clone().into()),
            // Decode, but NO glow: these two stand in for 1.12 `<PlayerModel>` widgets, which the
            // reference paints in the UI strata — after the WorldFrame's own FFX apply — so they
            // never carry the scene glow (decision 0638; [`benilla_world::ffx_glow::FfxGlow::UI_PANE`]).
            benilla_world::ffx_glow::FfxGlow::UI_PANE,
            // Placeholder — `sync_body_booth` overwrites transform + projection from the unit's
            // bounds on the first bake. A plain perspective is harmless while the model is loading.
            Projection::from(PerspectiveProjection {
                fov: PORTRAIT_FOV,
                near: 0.02,
                far: 100.0,
                ..default()
            }),
            layer.clone(),
            BoothCam(slot.to_string()),
        ));
        booths.0.insert(
            slot.to_string(),
            Booth {
                layer,
                root,
                target: image,
                baked: None,
                wake: 0,
                live: false,
                pending: Vec::new(),
                pending_since: None,
                aspect: 1.0,
                rigged: false,
                parked: false,
            },
        );
    }

    // The glue booth (decisions 0423 + 0465): its own slot/layer/target, framed per-bake.
    glue_booth::spawn_glue_booth(&mut commands, &mut images, &mut portraits, &mut booths);
    // The dressing room (decision 1060): a third body pane, tuple-driven like the glue booth but
    // lit and framed like the paper doll.
    dressup::spawn_dressup_booth(&mut commands, &mut images, &mut portraits, &mut booths);
}

/// `true` while the `WOW_PORTRAIT_TEST` debug bake owns the booths (checked once — env vars don't
/// change mid-run).
fn test_mode(cached: &mut Option<bool>) -> bool {
    *cached.get_or_insert_with(|| std::env::var("WOW_PORTRAIT_TEST").is_ok_and(|s| !s.is_empty()))
}

/// The three queries that read a unit's **dressed look** — the attach-spawned [`PortraitPart`] /
/// [`PortraitRider`] descendants a booth mirrors. Bundled as one `SystemParam` so `sync_portraits`
/// and `sync_paperdoll` stay under Bevy's 16-parameter system ceiling, and share the one
/// descendants walk ([`Self::collect`]) instead of open-coding it twice.
#[derive(SystemParam)]
struct DressedLook<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    parts: Query<'w, 's, &'static PortraitPart>,
    riders: Query<'w, 's, &'static PortraitRider>,
    billboards: Query<'w, 's, &'static PortraitBillboard>,
    effects: Query<'w, 's, &'static PortraitEffects>,
    mounts: Query<'w, 's, (), With<crate::entities::mount::MountBody>>,
}

impl DressedLook<'_, '_> {
    /// Walk `unit`'s descendants once, collecting its part + rider children. All empty while the
    /// unit's model is still loading / cube-fallback (no attach path has spawned the parts yet).
    ///
    /// A mounted unit's MOUNT child (decision 0441) is a second creature under the unit — a
    /// portrait/paper-doll shows the character alone, never the horse (the ref's `Model:SetUnit`
    /// binds the player model, not the mount). Its **parts** (the mount's body meshes) are
    /// skipped by pruning on the [`mount::MountBody`] marker; **riders** stay collected from the
    /// whole tree, because the rider's own helm/shoulder/held riders hang under the rider's
    /// joints, which re-root under the mount's seat anchor INSIDE the mount subtree while
    /// mounted — and a mount model never carries equipment riders of its own. An item's
    /// camera-facing batches and effects hang off those same joints, so they follow the rider rule,
    /// not the part rule; only the CHARACTER's own billboard (the eye-glow) prunes with the body.
    fn collect(
        &self,
        unit: Entity,
    ) -> (
        Vec<&PortraitPart>,
        Vec<&PortraitRider>,
        Vec<&PortraitBillboard>,
        Vec<&PortraitEffects>,
    ) {
        let mut parts = Vec::new();
        let mut riders = Vec::new();
        let mut billboards = Vec::new();
        let mut effects = Vec::new();
        let mut stack: Vec<(Entity, bool)> = vec![(unit, false)];
        while let Some((e, mut in_mount)) = stack.pop() {
            in_mount |= self.mounts.contains(e);
            if !in_mount {
                if let Ok(p) = self.parts.get(e) {
                    parts.push(p);
                }
            }
            if let Ok(r) = self.riders.get(e) {
                riders.push(r);
            }
            // A camera-facing batch prunes by whose model it is ([`PortraitSeat`]): a mount's own
            // glow card goes with the mount's meshes, an item's card rides an attach joint that
            // re-roots INSIDE the mount subtree while mounted and must survive it like a rider.
            if let Ok(b) = self.billboards.get(e) {
                if !in_mount || b.seat != PortraitSeat::Body {
                    billboards.push(b);
                }
            }
            // Never pruned: every publisher of this marker is an ITEM's model (its own emitters, or
            // its `ItemVisuals` glow's), seated on an attach joint — the rider rule. Nothing
            // publishes a mount's own emitters, so there is no mount case to exclude.
            if let Ok(fx) = self.effects.get(e) {
                effects.push(fx);
            }
            if let Ok(c) = self.children.get(e) {
                stack.extend(c.iter().map(|child| (child, in_mount)));
            }
        }
        (parts, riders, billboards, effects)
    }
}

/// Each frame: for every slot, mirror the unit's **live dressed look** — its attach-spawned
/// [`PortraitPart`] children — into the booth whenever that look changes (new unit, gear swap,
/// appearance refresh), re-framing the camera from the display's anchors. A live unit whose model
/// hasn't attached yet shows the ref's 2D `TemporaryPortrait` stand-in instead (RE C5).
#[allow(clippy::too_many_arguments)]
fn sync_portraits(
    mut commands: Commands,
    mut booths: ResMut<Booths>,
    mut portraits: ResMut<PortraitImages>,
    mut booth_light: ResMut<BoothLight>,
    creatures: Option<Res<Creatures>>,
    selection: Res<Selection>,
    self_q: Query<Entity, With<SelfPlayer>>,
    ent_q: Query<&NetEntity>,
    stores_q: Query<&crate::net::ObjectStore>,
    look: DressedLook,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut env_cache: Local<Option<bool>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    interact_npc: Res<crate::ui_session::InteractNpc>,
    // The party slots' roster + entity index + the skin-palette table (decision 0720), and the pet
    // bar (0990) — one tuple param (the 16-SystemParam ceiling). The index serves both.
    party: (
        Res<crate::ui_party::GroupState>,
        Res<crate::net::GuidIndex>,
        ResMut<benilla_world::rig_palette::RigPalettes>,
        Res<crate::ui_pet::PetBar>,
    ),
) {
    if test_mode(&mut env_cache) {
        return; // the test bake owns the booths
    }
    let (party_roster, party_index, mut palettes, pet_bar) = party;
    for token in SLOTS {
        let unit: Option<Entity> = match token {
            "player" => self_q.single().ok(),
            "target" => selection.target,
            // The pet, off the bar's cached guid — the same word its unit token and `UNIT_PET`
            // read (`crate::ui_pet::feed_pet_unit`). An unstreamed pet empties the booth, exactly
            // as an out-of-range party member does.
            "pet" => (pet_bar.spells.pet_guid != 0)
                .then(|| party_index.0.get(&pet_bar.spells.pet_guid))
                .flatten()
                .copied(),
            // The NPC an interaction window is bound to (gossip / quest / merchant / trainer),
            // resolved to its live entity by `feed_interact_npc` — the same bake path as "target".
            "npc" => interact_npc.0,
            // A party member's slot bakes only while the member is streamed (in range); out of
            // range there's no model to pose and the frame's circle stays empty (0434 phase 2 —
            // whether the reference substitutes anything there is a phase-4 look question).
            tok => tok
                .strip_prefix("party")
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| party_roster.party_slots().nth(n - 1))
                .and_then(|m| party_index.0.get(&m.guid))
                .copied(),
        };
        let Some(booth) = booths.0.get_mut(token) else {
            continue;
        };
        let Some(unit) = unit else {
            // No unit: empty the booth (the frame itself is hidden on UnitExists false; the dark
            // disc behind it never shows).
            if booth.baked.is_some() {
                commands.entity(booth.root).despawn_related::<Children>();
                booth.baked = None;
                // Render the emptied stage (decision 0540): the target must hold the cleared
                // backdrop, not the departed unit's face, before the camera sleeps.
                booth.wake = BOOTH_SETTLE_FRAMES;
                booth.pending.clear();
                // The despawn reaped meshes and anchors; the rig state on the ROOT (pose buffer,
                // player, palette slot) needs its own strip or it evaluates behind the emptied
                // stage for as long as the booth stands empty ([`booth::clear_booth_rig`]).
                clear_booth_rig(&mut commands, booth.root);
                booth.rigged = false;
                booth.parked = false;
            }
            let live = PortraitSource::Live(booth.target.clone());
            if portraits.0.get(token) != Some(&live) {
                portraits.0.insert(token.to_string(), live);
            }
            continue;
        };
        // The unit's dressed look: its attach-spawned part children (geosets filtered, composited
        // materials) + its bone riders (helm/shoulders/held — grandchildren under joint entities)
        // + the camera-facing batches and effect models it wears, one descendants walk. Parts empty
        // while the model is still loading / cube-fallback.
        let (parts, riders, billboards, effects) = look.collect(unit);
        if parts.is_empty() {
            // Model not attached yet → the ref's own 2D stand-in (RE C5): sex/race for a player
            // body, the Monster art for a creature (our pick — the ref's `-Pet` file belongs to
            // its pet delegate). Keeps the booth's last bake around; only the bridge flips.
            let file = temporary_portrait(ent_q.get(unit).ok(), stores_q.get(unit).ok());
            let src = PortraitSource::File(file);
            if portraits.0.get(token) != Some(&src) {
                portraits.0.insert(token.to_string(), src);
            }
            continue;
        }
        let key = LookKey::build(&parts, &riders, &billboards, &effects);
        if booth.baked.as_ref() != Some(&key) {
            let display_id = ent_q.get(unit).ok().and_then(|n| n.display_id);
            // The look changed — re-bake: studio-lit twins of the exact dressed materials, posed
            // at Stand on the booth's own throwaway skeleton (the ref bake — riders ride their
            // bone's joint, exactly like the world instance).
            // Resolve the framing anchors FIRST — before anything is despawned or spawned. A
            // `None` here means the display is still loading (see `booth_anchors`), and a bake
            // committed now would aim the camera at fabricated zero bounds and never re-aim.
            let Some(anchors) = booth_anchors(creatures.as_deref(), display_id) else {
                booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
                log_bake(
                    token,
                    "wait-anchors",
                    &parts,
                    &riders,
                    &billboards,
                    &effects,
                );
                continue;
            };
            let rig = creatures
                .as_deref()
                .zip(display_id)
                .and_then(|(c, d)| c.display_rig(d));
            let booth_parts: Vec<BoothPart> = parts
                .iter()
                .map(|p| BoothPart {
                    skinned: p.skinned_mesh.clone(),
                    static_mesh: p.static_mesh.clone(),
                    material: booth_light.studio.variant(&p.material, &mut wow_mats),
                    // `None` — the same known gap as the glue preview's (decision 0807): a
                    // mirrored `PortraitPart` doesn't carry the batch's alpha loops.
                    alpha_anim: None,
                })
                .collect();
            let booth_riders: Vec<BoothRider> = riders
                .iter()
                .map(|r| BoothRider {
                    mesh: r.static_mesh.clone(),
                    material: booth_light.studio.variant(&r.material, &mut wow_mats),
                    bone: r.bone,
                    offset: r.offset,
                })
                .collect();
            // The camera-facing batches — the eye-glow on its eye bone, an item's gem/halo at its
            // attach point — seated on their bone's booth joint and camera-faced by the booth (relit
            // onto the studio buffer like everything else; harmless where the batch is fullbright).
            let booth_billboards: Vec<BoothBillboardSpec> = billboards
                .iter()
                .map(|b| BoothBillboardSpec {
                    mesh: b.mesh.clone(),
                    material: booth_light.studio.variant(&b.material, &mut wow_mats),
                    bone: b.bone,
                    offset: b.seat.offset(),
                    kind: b.kind,
                })
                .collect();
            // A source material was not resident, so at least one "twin" above is the WORLD
            // material — bound to the world light buffer, which this booth's whole point is to
            // avoid ("would render a night portrait pitch black", `light.rs`). Abandon the bake:
            // `booth.baked` stays untouched, so the parts-key compare re-fires next frame and we
            // retry once the material lands. The previous bake stays on screen meanwhile.
            if booth_light.studio.take_unready() {
                booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
                continue;
            }
            commands.entity(booth.root).despawn_related::<Children>();
            let booth_rig = spawn_booth_model(
                &mut commands,
                &mut palettes,
                booth.root,
                booth.layer.clone(),
                &booth_parts,
                &booth_riders,
                rig.as_ref().and_then(|r| {
                    r.inverse_bindposes
                        .as_ref()
                        .map(|ibp| (r.skeleton, ibp, r.animations))
                }),
                anim_data.as_deref().map(|a| &a.0),
                BoothMotion::Frozen,
                [false, false], // a still portrait sheaths its weapons — no in-hand grip
                &booth_billboards,
            );
            // Even a Frozen still re-evaluates its paused pose every frame — the park matters
            // MOST here, since a portrait's camera sleeps for the whole session outside its
            // bake window.
            booth.rigged = booth_rig.rigged();
            booth_rig.finish(&mut commands);
            booth.parked = false;
            // **No emitters here, and that is the reference's own answer** (decision 0822): the
            // round portrait is a ONE-SHOT bake — a fresh M2 scene + instance, one `0x707680` draw,
            // the texture cached by GUID/displayId and returned with *no re-render* on a hit, nothing
            // persisting between bakes (wow-re `portrait-render.md` §2, byte-verified). A particle
            // emitter contributes nothing to a single frame of a freshly-born pool, so the ref's
            // portrait shows none — and a booth that spawned them would either freeze a cloud
            // mid-birth into the still or have to render forever for a 256² face. The batches above
            // *are* geometry, so they draw in that one frame and belong here. The body panes are a
            // different widget with a different law — see [`sync_body_booth`].
            //
            // Frame through the display's authored portrait camera (heuristic anchors for the
            // camera-less few), resolved above before anything was torn down.
            log_frame(token, &anchors, &frame(&anchors).0);
            aim(&mut cams, token, &frame(&anchors));
            log_bake(token, "bake", &parts, &riders, &billboards, &effects);
            wake_booth(
                booth,
                &wow_mats,
                booth_parts
                    .iter()
                    .map(|p| &p.material)
                    .chain(booth_riders.iter().map(|r| &r.material))
                    .chain(booth_billboards.iter().map(|b| &b.material)),
            );
            booth.baked = Some(key);
        }
        let live = PortraitSource::Live(booth.target.clone());
        if portraits.0.get(token) != Some(&live) {
            portraits.0.insert(token.to_string(), live);
        }
    }
}

/// Each frame: mirror the **player's** dressed look into the paper-doll booth — the SAME
/// [`PortraitPart`]/[`PortraitRider`] children the `"player"` portrait slot mirrors, so a gear /
/// appearance change flips the parts key and re-bakes the full-body pane exactly as it re-bakes the
/// face. Differences from [`sync_portraits`]: only the self player feeds it, the framing is
/// full-body ([`body_frame`] from the model's bounds, never the authored bust camera — decision
/// 0208 §5), and the model root spins to the pane's [`PaperDollBooth::yaw`] (the ref's
/// `Model:SetRotation`).
///
/// **What re-bakes.** A parts-key change respawns the posed instance and re-aims the (yaw-
/// independent) camera; a bare yaw change only re-rotates the root — neither happens on an unchanged
/// frame. The bake stands ready whether or not the window is open, but the 512² *pass* only runs
/// while the pane is being drawn ([`BoothPanes`], decision 1069).
#[allow(clippy::too_many_arguments)]
fn sync_paperdoll(
    mut commands: Commands,
    mut booths: ResMut<Booths>,
    mut portraits: ResMut<PortraitImages>,
    mut booth_light: ResMut<BoothLight>,
    creatures: Option<Res<Creatures>>,
    self_q: Query<Entity, With<SelfPlayer>>,
    ent_q: Query<&NetEntity>,
    look: DressedLook,
    paperdoll: Res<PaperDollBooth>,
    mut palettes: ResMut<benilla_world::rig_palette::RigPalettes>,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut env_cache: Local<Option<bool>>,
    mut last_pose: Local<Option<(f32, f32)>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    framing_in: BoothFraming,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
) {
    if test_mode(&mut env_cache) {
        return; // the test bake owns the booths (it drives the paper doll too, see `bake_test`)
    }
    sync_body_booth(
        &mut palettes,
        PAPERDOLL_SLOT,
        self_q.single().ok(),
        paperdoll.yaw,
        &mut last_pose,
        framing_in.gx.0,
        &mut commands,
        &mut booths,
        &mut portraits,
        &mut booth_light,
        creatures.as_deref(),
        &ent_q,
        &look,
        &mut wow_mats,
        &mut cams,
        anim_data.as_deref(),
        &framing_in.panes,
    );
}

/// The inspect window's model pane (decision 0631 §4) — the paper doll's exact twin, pointed at
/// whichever unit [`crate::ui_inspect`] resolved this frame instead of at the self player.
#[allow(clippy::too_many_arguments)]
fn sync_inspect_booth(
    mut commands: Commands,
    mut booths: ResMut<Booths>,
    mut portraits: ResMut<PortraitImages>,
    mut booth_light: ResMut<BoothLight>,
    creatures: Option<Res<Creatures>>,
    ent_q: Query<&NetEntity>,
    look: DressedLook,
    inspect: Res<InspectBooth>,
    mut palettes: ResMut<benilla_world::rig_palette::RigPalettes>,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut env_cache: Local<Option<bool>>,
    mut last_pose: Local<Option<(f32, f32)>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    framing_in: BoothFraming,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
) {
    if test_mode(&mut env_cache) {
        return;
    }
    sync_body_booth(
        &mut palettes,
        INSPECT_SLOT,
        inspect.unit,
        inspect.yaw,
        &mut last_pose,
        framing_in.gx.0,
        &mut commands,
        &mut booths,
        &mut portraits,
        &mut booth_light,
        creatures.as_deref(),
        &ent_q,
        &look,
        &mut wow_mats,
        &mut cams,
        anim_data.as_deref(),
        &framing_in.panes,
    );
}

/// The pet paper doll's model pane (decision 1057) — the inspect pane's exact twin, pointed at the
/// pet [`crate::ui_pet_doll`] resolved this frame.
#[allow(clippy::too_many_arguments)]
fn sync_petdoll_booth(
    mut commands: Commands,
    mut booths: ResMut<Booths>,
    mut portraits: ResMut<PortraitImages>,
    mut booth_light: ResMut<BoothLight>,
    creatures: Option<Res<Creatures>>,
    ent_q: Query<&NetEntity>,
    look: DressedLook,
    petdoll: Res<PetDollBooth>,
    mut palettes: ResMut<benilla_world::rig_palette::RigPalettes>,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut env_cache: Local<Option<bool>>,
    mut last_pose: Local<Option<(f32, f32)>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    framing_in: BoothFraming,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
) {
    if test_mode(&mut env_cache) {
        return;
    }
    sync_body_booth(
        &mut palettes,
        PETDOLL_SLOT,
        petdoll.unit,
        petdoll.yaw,
        &mut last_pose,
        framing_in.gx.0,
        &mut commands,
        &mut booths,
        &mut portraits,
        &mut booth_light,
        creatures.as_deref(),
        &ent_q,
        &look,
        &mut wow_mats,
        &mut cams,
        anim_data.as_deref(),
        &framing_in.panes,
    );
}

/// Bake `unit`'s full-body dressed look into the `slot` booth at `yaw` — the shared body of all
/// three body booths (decision 0208 §5 for the paper doll, 0631 §4 for inspect, 1057 for the pet
/// doll). `unit` is `None` when there is nothing to show, which empties the booth.
#[allow(clippy::too_many_arguments)]
fn sync_body_booth(
    palettes: &mut benilla_world::rig_palette::RigPalettes,
    slot: &str,
    unit: Option<Entity>,
    yaw: f32,
    last_pose: &mut Option<(f32, f32)>,
    // The primary window's aspect ([`GxAspect`]) — only the model-root scale reads it.
    display_aspect: f32,
    commands: &mut Commands,
    booths: &mut Booths,
    portraits: &mut PortraitImages,
    booth_light: &mut BoothLight,
    creatures: Option<&Creatures>,
    ent_q: &Query<&NetEntity>,
    look: &DressedLook,
    wow_mats: &mut Assets<WowModelMaterial>,
    cams: &mut Query<(&BoothCam, &mut Transform, &mut Projection)>,
    anim_data: Option<&crate::creature_anim::AnimData>,
    panes: &BoothPanes,
) {
    let Some(booth) = booths.0.get_mut(slot) else {
        return;
    };
    // Latch the pane's aspect while it is on screen (decision 1069). Sticky: a closed window
    // publishes nothing, and re-framing the standing bake back to square on the way out would be a
    // visible pop on the way back in.
    let aspect = panes.0.get(slot).copied().unwrap_or(booth.aspect);
    // There is no 2D stand-in for a body pane — the bridge always points at the live target (an
    // empty booth just renders the dark backdrop until the unit's model attaches).
    let live = PortraitSource::Live(booth.target.clone());
    if portraits.0.get(slot) != Some(&live) {
        portraits.0.insert(slot.to_string(), live);
    }
    // The unit's dressed look — the same mirrored descendants the portrait slots collect. Empty
    // while there's no unit, or its model hasn't attached yet.
    let (parts, riders, billboards, effects) = match unit {
        Some(unit) => look.collect(unit),
        None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
    };
    if parts.is_empty() {
        // No unit / model not attached → empty the booth and forget the applied yaw (so it
        // re-applies on the next bake).
        if booth.baked.is_some() {
            commands.entity(booth.root).despawn_related::<Children>();
            booth.baked = None;
            *last_pose = None;
            // Render the emptied stage before sleeping (decision 0540) — and the emptied stage has
            // no emitters left, so the pane stops being live.
            booth.wake = BOOTH_SETTLE_FRAMES;
            booth.live = false;
            booth.pending.clear();
            // The despawn reaped meshes and anchors; the rig state on the ROOT needs its own
            // strip ([`booth::clear_booth_rig`]).
            clear_booth_rig(commands, booth.root);
            booth.rigged = false;
            booth.parked = false;
        }
        return;
    }
    let unit = unit.expect("unit present — parts came from its descendants");
    let key = LookKey::build(&parts, &riders, &billboards, &effects);
    let display_id = ent_q.get(unit).ok().and_then(|n| n.display_id);
    // Anchors first, before any teardown — a still-loading display must not be framed from
    // fabricated zero bounds (see the portrait site, and `booth_anchors`). Resolved out here rather
    // than inside the bake because the model-root scale below needs them on an otherwise-idle frame
    // too (a window resize moves the scale without touching the bake).
    let anchors_now = booth_anchors(creatures, display_id);
    // The model-root scale, taken while the anchors are still in hand (the bake below consumes
    // them). `1.0` until they resolve — an unscaled root is the harmless default, and the 4:3
    // factor is 1.0 anyway.
    let model_scale = anchors_now
        .as_ref()
        .map_or(1.0, |a| framing::pane_root_scale(a, display_aspect));
    // A changed pane aspect re-runs the same path: the camera's projection depends on it, and it
    // only ever moves once — the first frame the window is drawn.
    let parts_changed = booth.baked.as_ref() != Some(&key) || booth.aspect != aspect;
    if parts_changed {
        booth.aspect = aspect;
        let Some(anchors) = anchors_now else {
            booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
            log_bake(slot, "wait-anchors", &parts, &riders, &billboards, &effects);
            return;
        };
        let rig = creatures
            .zip(display_id)
            .and_then(|(c, d)| c.display_rig(d));
        let booth_parts: Vec<BoothPart> = parts
            .iter()
            .map(|p| BoothPart {
                skinned: p.skinned_mesh.clone(),
                static_mesh: p.static_mesh.clone(),
                material: booth_light.pane.variant(&p.material, wow_mats),
                // `None` — the same known gap as the glue preview's (decision 0807).
                alpha_anim: None,
            })
            .collect();
        let booth_riders: Vec<BoothRider> = riders
            .iter()
            .map(|r| BoothRider {
                mesh: r.static_mesh.clone(),
                material: booth_light.pane.variant(&r.material, wow_mats),
                bone: r.bone,
                offset: r.offset,
            })
            .collect();
        // The camera-facing batches — the eye-glow on its eye bone, a wand's gem or the held torch's
        // halo at its attach point — seated on their bone's booth joint and camera-faced by the booth
        // (relit onto the pane buffer like everything else; harmless where the batch is fullbright).
        let booth_billboards: Vec<BoothBillboardSpec> = billboards
            .iter()
            .map(|b| BoothBillboardSpec {
                mesh: b.mesh.clone(),
                material: booth_light.pane.variant(&b.material, wow_mats),
                bone: b.bone,
                offset: b.seat.offset(),
                kind: b.kind,
            })
            .collect();
        // The worn items' effects (decision 0822) — an equipped item's own emitters (0813, `#bugs`
        // B118) and a held weapon's `ItemVisuals` glow (0805). Collected BEFORE the teardown for the
        // same reason as everything else here; spawned after the model, which is what hands us the
        // joints they seat on.
        let booth_effects: Vec<BoothEffects> = effects
            .iter()
            .map(|fx| BoothEffects {
                bone: fx.bone,
                offset: fx.offset,
                emitters: fx.emitters.clone(),
            })
            .collect();
        // Same law as the portrait bake: never latch a world-lane material into the pane. Leave
        // `booth.baked` alone and retry next frame (see the portrait site for the full note).
        if booth_light.pane.take_unready() {
            booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
            return;
        }
        commands.entity(booth.root).despawn_related::<Children>();
        let mut booth_rig = spawn_booth_model(
            commands,
            palettes,
            booth.root,
            booth.layer.clone(),
            &booth_parts,
            &booth_riders,
            rig.as_ref().and_then(|r| {
                r.inverse_bindposes
                    .as_ref()
                    .map(|ibp| (r.skeleton, ibp, r.animations))
            }),
            anim_data.map(|a| &a.0),
            // The pane ANIMATES: Stand loops and the global-sequence bones run, which is what the
            // reference's `<PlayerModel>` widget does (decision 0822 §4 read it as live-rendering
            // and left the pose as a look call; the director made that call — decision 1069).
            BoothMotion::Loop,
            [false, false], // the pane sheaths its weapons — no in-hand grip
            &booth_billboards,
        );
        // The item effects go up on the posed skeleton's anchors — the body pane is the lane that
        // gets them (the reference's `<PlayerModel>` widget renders live; the round portraits are
        // a one-shot cached bake — [`spawn_booth_effects`]).
        spawn_booth_effects(
            commands,
            &mut booth_rig,
            &booth.layer,
            booth_light.pane.buffer.as_ref(),
            &booth_effects,
        );
        // So the whole bake is live, emitters or not — `wake` can't gate a looping animation.
        // `gate_booth_cameras` renders it every frame its pane is on screen, and none when it isn't.
        booth.live = true;
        // A fresh bake is animated by construction; the park state is the new rig's.
        booth.rigged = booth_rig.rigged();
        booth_rig.finish(commands);
        booth.parked = false;
        // Body framing from the display's bounds — the full standing figure, feet-to-crown, at the
        // destination pane's aspect. Resolved before the teardown above; see the portrait site for
        // why it cannot be faked.
        log_frame(slot, &anchors, &body_frame(&anchors, aspect).0);
        aim(cams, slot, &body_frame(&anchors, aspect));
        log_bake(slot, "bake", &parts, &riders, &billboards, &effects);
        wake_booth(
            booth,
            wow_mats,
            booth_parts
                .iter()
                .map(|p| &p.material)
                .chain(booth_riders.iter().map(|r| &r.material))
                .chain(booth_billboards.iter().map(|b| &b.material)),
        );
        booth.baked = Some(key);
    }
    // The model root: **yaw → rotation, plus the pane's model scale** — the widget's own
    // `T(pos)·R(facing)·S(s)` with `pos` at the origin (the ref's `Model:SetRotation` writes the
    // facing, and a spin about WoW +Z-up conjugates to a spin about Bevy +Y-up).
    //
    // `s` is [`framing::pane_root_scale`]: `1.0` for a model with its own camera and the
    // display-aspect renormalize factor for one without. It rides the same latch as the yaw because
    // it moves for the same reason — a window resize — and the reference re-snapshots on exactly
    // that (`DISPLAY_SIZE_CHANGED → RefreshUnit()`, which both panes register). Cheap enough to
    // re-derive here rather than key a whole re-bake on: the CAMERA does not depend on the display
    // aspect, only the root does.
    if parts_changed || *last_pose != Some((yaw, model_scale)) {
        commands.entity(booth.root).insert(
            Transform::from_rotation(Quat::from_rotation_y(yaw))
                .with_scale(Vec3::splat(model_scale)),
        );
        *last_pose = Some((yaw, model_scale));
        // A spin is a content edge too (decision 0540): render the new pose, then sleep.
        booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
    }
}

/// The demand-render gate (decision 0540): each booth camera is active only while its booth has
/// something new to show — [`Booth::wake`] frames after a content edge, or a bake texture still
/// in flight ([`Booth::pending`]) — except the booths whose content is **live**, which render
/// continuously: the glue booth, whose whole scene is animated (looping sequences, global-sequence
/// bones, particle emitters) while a glue screen shows, and a **body pane**, whose bake is a live
/// `<PlayerModel>` widget ([`Booth::live`], decisions 0822 §4 + 1069). A sleeping camera
/// skips its whole pass (clear + model + FFXGlow chain); its target keeps the last render — exactly
/// right for a still (the 0105 bake, frozen at Stand).
///
/// A live pane renders **only while it is on screen** ([`BoothPanes`], decision 1069): a character
/// window nobody opened costs nothing, which is the follow-up [`sync_paperdoll`] named and 0822's
/// unconditional `live` never had.
/// With `WOW_PORTRAIT_TEST` set the gate stands down (the eyeball harness wants live cameras).
/// The pipeline warm pass is demand too (decision 0938): its menagerie duplicates rigs onto a
/// booth layer so the booths' `Msaa::Off` pipeline twins compile behind the entry cover — which
/// only works if the booth cameras render during the warm window.
#[allow(clippy::too_many_arguments)] // a Bevy system: each param is one resource/query
fn gate_booth_cameras(
    mut commands: Commands,
    mut booths: ResMut<Booths>,
    preview: Res<GluePreview>,
    panes: Res<BoothPanes>,
    images: Res<Assets<Image>>,
    warm: Res<crate::pipe_warm::WarmPass>,
    time: Res<Time<bevy::time::Real>>,
    mut cams: Query<(&BoothCam, &mut Camera)>,
    rate: Res<PaneRate>,
    frames: Res<bevy::diagnostic::FrameCount>,
    // `WOW_BOOTH_LOG` only: the marker's REAL state beside this gate's `booth.parked`
    // bookkeeping. The two can desync exactly one way — a foreign writer — and that desync is
    // invisible in every capture (a woken rig snaps to the absolute clock), so it must be
    // loggable (decision 1447: the world parker froze every pane and no dump could show it).
    markers: Query<(), With<benilla_world::rig_anim::AnimParked>>,
    mut env_cache: Local<Option<bool>>,
) {
    let test = test_mode(&mut env_cache);
    // `satisfied()` is false exactly while the covered warm window runs (the loading screen
    // holds on it), so this term costs nothing outside that window.
    let warming = !warm.satisfied();
    for (BoothCam(token), mut cam) in &mut cams {
        let Some(booth) = booths.0.get_mut(token.as_str()) else {
            continue;
        };
        // A pending texture landing this frame still needs one rendered frame to reach the still.
        let had_pending = !booth.pending.is_empty();
        booth.pending.retain(|h| !images.contains(h));
        if had_pending && booth.pending.is_empty() {
            booth.wake = booth.wake.max(1);
        }
        // Bound the hold (the `pending_since` field's doc): a texture that will never land must
        // not keep this camera rendering for the rest of the session.
        if booth.pending.is_empty() {
            booth.pending_since = None;
        } else {
            let now = time.elapsed_secs_f64();
            let since = *booth.pending_since.get_or_insert(now);
            if now - since > PENDING_LANDING_SECS {
                warn!(
                    "booth {}: {} texture(s) never landed after {PENDING_LANDING_SECS:.0}s — \
                     releasing the wake hold with the still as-is",
                    token.as_str(),
                    booth.pending.len(),
                );
                booth.pending.clear();
                booth.pending_since = None;
                booth.wake = booth.wake.max(1);
            }
        }
        let live_scene = token.as_str() == GLUE_SLOT && preview.scene.is_some();
        // A live bake renders every frame — but only while the UI is actually drawing its pane.
        // (The glue screens sample their booth outside the FrameXML extract, so they publish no
        // pane and stay on `live_scene`.)
        let live_pane = booth.live && panes.0.contains_key(token.as_str());
        let active = test
            || warming
            || live_scene
            || live_pane
            || booth.wake > 0
            || !booth.pending.is_empty();
        // Half-rate (decision 1444, [`PaneRate`]): when the live pane is the ONLY thing keeping
        // this camera rendering — no wake window settling a fresh bake, no pending texture hold,
        // no fullscreen glue scene — skip every other frame. `active` stays the LOGICAL state:
        // the park bookkeeping below keys off it (the pose keeps evaluating; only the render
        // skips), and the wake counter keeps draining per real frame.
        let throttled = rate.half
            && live_pane
            && !(test || warming || live_scene)
            && booth.wake == 0
            && booth.pending.is_empty()
            && frames.0 % 2 == 1;
        let render = active && !throttled;
        // `WOW_BOOTH_LOG=1`: the gate's timeline — every activity flip and every armed frame,
        // wall-stamped (the first-login black-pane hunt).
        static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *LOG.get_or_init(|| std::env::var_os("WOW_BOOTH_LOG").is_some())
            && (cam.is_active != render || active)
        {
            eprintln!(
                "[booth] t={:7.2} {} active={} render={} wake={} pending={} marker={}",
                time.elapsed_secs(),
                token.as_str(),
                active,
                render,
                booth.wake,
                booth.pending.len(),
                markers.contains(booth.root),
            );
        }
        if cam.is_active != render {
            cam.is_active = render;
        }
        // Park/unpark the standing scene with its camera (director report 2026-08-19: the doll
        // bake animated at full cost from the LOGIN bake to quit, window opened or not). The
        // park is one `AnimParked` marker on the root (decision 1443): the 0712 evaluator, the
        // pose composes, the palette writes and the global-sequence writes all check it — the
        // pose HOLDS, the buffer is state. Booth-lane emitters freeze on the camera bit itself
        // (`particles::sim`). This system runs before the PostUpdate animation lane, so an
        // unpark's marker drop is seen the same frame — the wake window's first render already
        // animates (0739's wake law, same as every world rig).
        if booth.parked == active && booth.rigged {
            if active {
                commands
                    .entity(booth.root)
                    .remove::<benilla_world::rig_anim::AnimParked>();
            } else {
                commands
                    .entity(booth.root)
                    .insert(benilla_world::rig_anim::AnimParked);
            }
            booth.parked = !active;
        }
        if active {
            booth.wake = booth.wake.saturating_sub(1);
        }
    }
}

/// The ref's 2D portrait stand-in for a not-yet-renderable unit (RE C5):
/// `TemporaryPortrait-{Male|Female}-{Race}` for a player body, `-Monster` otherwise.
fn temporary_portrait(net: Option<&NetEntity>, store: Option<&crate::net::ObjectStore>) -> String {
    use benilla_protocol::EntityKind;
    let base = "Interface\\CharacterFrame\\TemporaryPortrait";
    if net.map(|n| n.kind) == Some(EntityKind::Player) {
        if let Some(s) = store {
            let sex = match s.0.unit_gender() {
                Some(1) => "Female",
                _ => "Male",
            };
            let race = match s.0.unit_race() {
                Some(1) => "Human",
                Some(2) => "Orc",
                Some(3) => "Dwarf",
                Some(4) => "NightElf",
                Some(5) => "Scourge",
                Some(6) => "Tauren",
                Some(7) => "Gnome",
                Some(8) => "Troll",
                _ => return format!("{base}.blp"),
            };
            return format!("{base}-{sex}-{race}.blp");
        }
        return format!("{base}.blp");
    }
    format!("{base}-Monster.blp")
}

/// Set the named slot's camera to the rig `frame` built — transform AND projection (the authored
/// camera brings its own fov/near/far, so the projection is per-bake, not booth-fixed).
fn aim(
    cams: &mut Query<(&BoothCam, &mut Transform, &mut Projection)>,
    token: &str,
    rig: &(Transform, Projection),
) {
    for (cam, mut t, mut p) in cams.iter_mut() {
        if cam.0 == token {
            *t = rig.0;
            *p = rig.1.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_part() -> PortraitPart {
        PortraitPart {
            static_mesh: Handle::default(),
            skinned_mesh: None,
            material: Handle::default(),
        }
    }

    fn card(seat: PortraitSeat) -> PortraitBillboard {
        PortraitBillboard {
            mesh: Handle::default(),
            material: Handle::default(),
            bone: 4,
            seat,
            kind: benilla_formats::BillboardKind::Spherical,
        }
    }

    fn effects(bone: u16, offset: Vec3, count: usize) -> PortraitEffects {
        PortraitEffects {
            bone,
            offset,
            emitters: (0..count)
                .map(|_| benilla_assets::ModelEmitter {
                    def: benilla_world::testing::plain_particle_def(),
                    texture: None,
                    bone_pivot: [0.0; 3],
                    billboard: None,
                    recursion: None,
                    geometry: None,
                    owner_reach: 0.0,
                    water_bound: (Vec3::ZERO, 0.0),
                    idle_seq: 0,
                })
                .collect(),
        }
    }

    /// What [`DressedLook::collect`] found, flattened to what the assertions care about.
    #[derive(Resource, Default)]
    struct Collected {
        parts: usize,
        riders: usize,
        billboards: Vec<PortraitSeat>,
        effects: Vec<(u16, usize)>,
    }

    /// Run the walk over `unit` in `app` and hand back what it collected.
    fn walk(app: &mut App, unit: Entity) -> Collected {
        app.init_resource::<Collected>();
        app.insert_resource(Unit(unit));
        app.add_systems(
            Update,
            |unit: Res<Unit>, look: DressedLook, mut out: ResMut<Collected>| {
                let (parts, riders, billboards, effects) = look.collect(unit.0);
                *out = Collected {
                    parts: parts.len(),
                    riders: riders.len(),
                    billboards: billboards.iter().map(|b| b.seat).collect(),
                    effects: effects.iter().map(|e| (e.bone, e.emitters.len())).collect(),
                };
            },
        );
        app.update();
        std::mem::take(app.world_mut().resource_mut::<Collected>().as_mut())
    }

    #[derive(Resource)]
    struct Unit(Entity);

    /// **A mounted unit's booth keeps the rider's gear and drops the horse's.** The mount prune is
    /// what makes a portrait show the character alone (decision 0441) — but while mounted the
    /// character's own attach joints re-root INSIDE the mount subtree, so pruning everything under it
    /// would take the rider's equipment with the horse. Riders were already exempt; an equipped item's
    /// camera-facing batch and its effects have to be too (decision 0822), and the discriminator is
    /// [`PortraitSeat`] — whose model the batch belongs to — not where in the tree it happens to sit.
    #[test]
    fn a_mounts_own_glow_card_prunes_but_the_riders_gear_does_not() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        let unit = app.world_mut().spawn(body_part()).id();
        // The character's own eye-glow, outside the mount subtree.
        app.world_mut()
            .spawn((card(PortraitSeat::Body), ChildOf(unit)));
        let mount = app
            .world_mut()
            .spawn((
                body_part(),
                crate::entities::mount::MountBody { host: unit },
                ChildOf(unit),
            ))
            .id();
        // The mount's OWN glow card (its lantern) — a batch of the mount's rigged body.
        app.world_mut()
            .spawn((card(PortraitSeat::Body), ChildOf(mount)));
        // …and the seated rider's gear, re-rooted under the mount: a shoulder rider, its
        // camera-facing batch and its emitters.
        let seat = app.world_mut().spawn(ChildOf(mount)).id();
        app.world_mut().spawn((
            PortraitRider {
                static_mesh: Handle::default(),
                material: Handle::default(),
                bone: 4,
                offset: Vec3::new(0.21, 1.42, 0.06),
            },
            ChildOf(seat),
        ));
        app.world_mut().spawn((
            card(PortraitSeat::Rider(Vec3::new(0.15, 1.58, 0.05))),
            ChildOf(seat),
        ));
        app.world_mut()
            .spawn((effects(4, Vec3::new(0.21, 1.42, 0.06), 2), ChildOf(seat)));

        let got = walk(&mut app, unit);
        assert_eq!(got.parts, 1, "the character's body, never the mount's");
        assert_eq!(
            got.riders, 1,
            "the rider's shoulder survives the mount subtree"
        );
        // Two `Body` cards exist in this tree — the character's eye-glow and the mount's lantern —
        // and exactly one may survive; the item's `Rider` card must, wherever it sits.
        let (body, rider): (Vec<PortraitSeat>, Vec<PortraitSeat>) = got
            .billboards
            .iter()
            .partition(|s| **s == PortraitSeat::Body);
        assert_eq!(
            body.len(),
            1,
            "the character's eye-glow, not the mount's lantern"
        );
        assert_eq!(
            rider,
            vec![PortraitSeat::Rider(Vec3::new(0.15, 1.58, 0.05))],
            "the item's card survives the mount subtree, like the rider it belongs to",
        );
        assert_eq!(
            got.effects,
            vec![(4, 2)],
            "an item's emitters are never pruned"
        );
    }

    /// **A key blind to effects would never re-bake for a glow.** An item glow resolves
    /// asynchronously (`entities::item_glow`: the item's template, then the effect model's own load),
    /// so its mirror lands *after* the meshes it rides with — a later frame, with every mesh handle
    /// identical. If the bake key ignored effects, the compare would say "unchanged" and the glow
    /// would stay out of the pane until some unrelated content edge forced a re-bake.
    #[test]
    fn a_glow_arriving_after_its_item_still_changes_the_bake_key() {
        let parts = [body_part()];
        let riders = [PortraitRider {
            static_mesh: Handle::default(),
            material: Handle::default(),
            bone: 4,
            offset: Vec3::new(0.21, 1.42, 0.06),
        }];
        let parts: Vec<&PortraitPart> = parts.iter().collect();
        let riders: Vec<&PortraitRider> = riders.iter().collect();

        let before = LookKey::build(&parts, &riders, &[], &[]);
        let glow = effects(4, Vec3::new(0.21, 1.42, 0.06), 1);
        let after = LookKey::build(&parts, &riders, &[], &[&glow]);
        assert!(before != after, "the glow's arrival is a content edge");

        // …and a *different* seat for the same emitter count is a different bake too: two glow slots
        // on one weapon differ by nothing else.
        let moved = effects(4, Vec3::new(0.21, 1.55, 0.06), 1);
        assert!(
            LookKey::build(&parts, &riders, &[], &[&moved]) != after,
            "the seat is part of the key",
        );
        // The same look twice is the same key — the compare must not re-bake every frame.
        assert!(LookKey::build(&parts, &riders, &[], &[&glow]) == after);
    }
}
