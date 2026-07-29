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
//! dressed look like the `"player"` slot, but frames the whole standing figure from the model's
//! bounds ([`framing::body_frame`], not the authored bust camera), bakes at 512², and spins the
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
//! [`model_pane_light_rows`]), continuous
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
use crate::terrain::WowModelMaterial;

mod framing;
use framing::{body_frame, frame, PORTRAIT_FOV};
pub(crate) use framing::{head_anchor, PortraitAnchors};
mod booth;
use booth::{spawn_booth_model, BoothBillboardSpec, BoothMotion, BoothPart, BoothRider};
mod glue_booth;
pub(crate) use glue_booth::{
    CreateLook, GlueLook, GluePreview, GluePreviewBake, GlueScene, PreviewBillboard, PreviewPart,
    PreviewRider, SelectLook, GLUE_SLOT,
};
mod light;
use light::{material_variant, model_pane_light_rows, studio_light_rows, BoothLight};
mod test_bake;

/// The portrait slots we bake, each with its own render layer/camera: the player + target unit
/// frames, and `"npc"` — the NPC an interaction window (gossip / quest / merchant / trainer /
/// taxi) is bound to ([`crate::ui_session::InteractNpc`]), so those windows show the creature's
/// face instead of the `?` placeholder.
const SLOTS: [&str; 7] = [
    "player", "target", "npc", "party1", "party2", "party3", "party4",
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
/// World is layer 0, the UI quad pass layer 1; portraits sit on their own high layers so nothing in the
/// world leaks into a booth and vice-versa (one layer per slot: base, base+1, …).
const PORTRAIT_LAYER_BASE: usize = 2;
/// The paper-doll booth's render layer — the next layer past the portrait slots.
const PAPERDOLL_LAYER: usize = PORTRAIT_LAYER_BASE + SLOTS.len();
/// The inspect booth's render layer — the next one past the paper doll's.
const INSPECT_LAYER: usize = PAPERDOLL_LAYER + 1;
/// The glue booth's render layer — the next one past inspect's. **Every booth layer is computed
/// HERE**, in one ladder, because they were not: the glue booth (`930b327c`) and the inspect booth
/// (`ead3b0c9`) each defined "the next layer past the paper doll's" in a different file and landed
/// on the same number. Two booths sharing a layer is not a cosmetic clash — `particles::sim`
/// resolves an emitter's booth camera by *finding the first camera whose layers intersect*, so the
/// glue scene's 28 emitters addressed the INSPECT camera, which is off at the glue screens, and the
/// login screen's braziers simulated forever without ever being drawn (decision 0775).
pub(super) const GLUE_LAYER: usize = INSPECT_LAYER + 1;

// The ladder must stay collision-free: a booth camera's layer is its identity for both rendering
// and the emitter→camera match, and the failure above was silent in both.
const _: () = assert!(
    PAPERDOLL_LAYER != INSPECT_LAYER
        && INSPECT_LAYER != GLUE_LAYER
        && PAPERDOLL_LAYER != GLUE_LAYER,
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
#[derive(Component)]
pub(crate) struct PortraitRider {
    pub(crate) static_mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    /// The body-skeleton bone the rider's joint entity belongs to.
    pub(crate) bone: u16,
    /// The attach point's Bevy-space offset under that bone ([`crate::entities::BoneAttach`]).
    pub(crate) offset: Vec3,
}

/// Stamped on a lightweight **anchor child** the attach path plants under a unit for each of its
/// character billboard batches — the undead/night-elf **eye-glow** (geoset 302 / geoset 0, a
/// camera-facing fullbright quad on the eye bone). The visible world card is a *root-spawned*
/// entity ([`crate::billboard::BillboardCard`]), never a unit descendant, so it can't be mirrored;
/// this marker rides the unit's tree purely so the portrait / paper-doll booths (which mirror the
/// unit's dressed descendants) can rebuild the glow as a booth card ([`BoothBillboardSpec`] +
/// [`booth::face_booth_billboards`]) — the same reconstruction the char-create glue path does from
/// its own parts. Carries the centred quad, its fullbright material, and the billboard bone/flag.
#[derive(Component)]
pub(crate) struct PortraitBillboard {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    /// The billboard bone (the eye bone) whose booth joint the card seats on.
    pub(crate) bone: u16,
    pub(crate) kind: benilla_formats::BillboardKind,
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

/// The key identifying what a booth currently has baked: the (mesh, material) identity of every
/// mirrored [`PortraitPart`]. Any change in the unit's dressed look (gear swap, appearance refresh,
/// different unit) changes the key → re-bake.
type PartsKey = Vec<(AssetId<Mesh>, AssetId<WowModelMaterial>)>;

/// Per-slot booth: its render layer, the model-root entity (children = the baked model's meshes),
/// its render-target image (so the bridge can flip back to `Live` after a `File` stand-in), and the
/// parts key currently baked (`None` = empty booth).
struct Booth {
    layer: RenderLayers,
    root: Entity,
    target: Handle<Image>,
    baked: Option<PartsKey>,
    /// Demand-render window (decision 0540): frames [`gate_booth_cameras`] still keeps this
    /// booth's camera active. Armed to [`BOOTH_SETTLE_FRAMES`] by every content edge (bake,
    /// empty, framing/yaw write); 0 with `pending` drained = the camera sleeps and the target
    /// keeps the last render — a still costs nothing per frame.
    wake: u32,
    /// Textures the last bake referenced that were not yet resident: the camera stays awake
    /// until each lands (an `mpq://` image arriving after the bake would otherwise be frozen
    /// OUT of the still forever), then renders one final resident frame.
    pending: Vec<Handle<Image>>,
}

/// How many frames a content edge keeps a booth camera rendering ([`Booth::wake`]): covers the
/// command-applied spawn, the billboard re-face's one-frame lag, and GPU upload of fresh meshes.
const BOOTH_SETTLE_FRAMES: u32 = 4;

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
fn log_bake(token: &str, verb: &str, parts: usize, riders: usize, billboards: usize) {
    if booth_log() {
        eprintln!("[booth] {token} {verb} parts={parts} riders={riders} billboards={billboards}");
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
/// .. })`. That is not a neutral default: [`framing::body_frame`] floors the head signal at `0.1`
/// for "a hypothetical bounds-less display", so zero anchors aim the camera at a 0.1-unit-tall
/// subject — the paper doll "zoomed into the max" and the wrong-size portrait of `#bugs` B106. And
/// it latches: the camera is aimed once per bake, and the parts key cannot change afterwards
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
}

#[derive(Resource, Default)]
struct Booths(HashMap<String, Booth>);

/// Tags a booth camera with its slot token, so the model-sync pass can re-frame it per model.
/// (`BoothCam`, not `PortraitCamera` — that name is the authored M2 rig, `benilla_assets::PortraitCamera`.)
/// (`pub(crate)` for the particle census, which reports draws per booth token — decision 0775.)
#[derive(Component)]
pub(crate) struct BoothCam(pub(crate) String);

/// Owns the portrait bake pipeline: the [`PortraitImages`] bridge + the per-slot off-screen booths.
pub(crate) struct PortraitPlugin;

impl Plugin for PortraitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PortraitImages>()
            .init_resource::<PaperDollBooth>()
            .init_resource::<InspectBooth>()
            .init_resource::<glue_booth::GluePreview>()
            .init_resource::<glue_booth::GluePreviewBake>()
            .init_resource::<Booths>()
            .init_resource::<BoothLight>()
            .init_resource::<crate::ui_session::InteractNpc>()
            .add_systems(Startup, setup_booths)
            // The variant-cache reaper: booth twins die with their world source material.
            .add_systems(Update, light::reap_dead_variants)
            // `feed_interact_npc` resolves the `"npc"` token's entity first; then the test bake owns
            // the booths when its env is set (the live syncs yield to it). The paper-doll sync runs
            // last (it shares the camera/booth/image resources, so the chain keeps the access ordered).
            .add_systems(
                Update,
                (
                    crate::ui_session::feed_interact_npc,
                    test_bake::sync_test_portraits,
                    sync_portraits,
                    sync_paperdoll,
                    sync_inspect_booth,
                    glue_booth::sync_glue_booth,
                    glue_booth::sync_glue_scene,
                    // Last: it reads the wake/pending state every sync above may have armed.
                    gate_booth_cameras,
                )
                    .chain(),
            )
            // Re-face each booth's eye-glow cards to its own camera (reads last-propagate joint
            // globals; unordered w.r.t. the syncs — a fresh card just faces forward one frame).
            .add_systems(Update, booth::face_booth_billboards)
            // The phase-3 preview instrument (`WOW_CREATE_TEST`, decision 0423): inert without the env.
            .add_systems(Update, glue_booth::drive_create_test)
            // `WOW_BOOTH_DUMP=<token>:<path>:<secs>` — photograph a booth's render target to disk
            // (the headless eye on "what is the paperdoll actually showing right now"; the
            // first-login black-pane hunt). Inert without the env.
            .add_systems(Update, test_bake::dump_booth_target);
    }
}

/// A fresh transparent render-target image (RGBA8 sRGB) of `size²`, usable as a camera target and
/// sampled by the UI. Portrait slots pass [`PORTRAIT_SIZE`]; the paper doll passes [`PAPERDOLL_SIZE`].
fn new_target_image(size: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        // NON-sRGB on purpose, measured (see the Hdr note below): with an Srgb label the composited
        // portrait read one net sRGB-decode too dark vs the world render of the same model. Raw bytes
        // + the swapchain's single encode lands the bake at world parity, pixel-verified.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    image
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
    mut mirrors: ResMut<crate::rig_palette::RigPaletteMirrors>,
    device: Res<bevy::render::renderer::RenderDevice>,
    queue: Res<bevy::render::renderer::RenderQueue>,
) {
    // The studio-light buffer: same LAYOUT as the shared world light, written once, never touched
    // again (fixed values — no per-frame upload, no render-world system). Sized to the FULL blob
    // (`light_blob_bytes` — header rows + the 0278 point-light table): the model shader declares the
    // whole struct and wgpu validates the bound size against it at every draw. Only the studio header
    // rows are written; wgpu zero-initializes the rest, so `point_count = 0` and no scene point light
    // ever touches a portrait (the studio look is deliberately static).
    let light_buffer =
        |label: &'static str, rows: [[f32; 4]; crate::lighting::LIGHT_HEADER_ROWS]| {
            let buffer = device.create_buffer(&bevy::render::render_resource::BufferDescriptor {
                label: Some(label),
                size: crate::lighting::light_blob_bytes(),
                usage: bevy::render::render_resource::BufferUsages::STORAGE
                    | bevy::render::render_resource::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&rows));
            buffer
        };
    booth_light.studio.buffer = Some(light_buffer("wow_portrait_light", studio_light_rows()));
    // The body panes' own light — the reference `<PlayerModel>` widget's (see the fn's doc).
    booth_light.pane.buffer = Some(light_buffer(
        "wow_model_pane_light",
        model_pane_light_rows(),
    ));
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
            Camera3d::default(),
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
            // HDR float intermediate + Tonemapping::None — the world camera's exact pipeline shape.
            // Measured without it: the LDR path stored the shader's linear output RAW in the sRGB
            // target (one net extra decode when the UI sampled it — the bake read x^2.4 dark, pixel-
            // verified against the clear color, which round-tripped fine). The HDR path encodes once
            // at the final blit, like the main view.
            bevy::render::view::Hdr,
            Tonemapping::None,
            // The gamma lane (0161): booth materials emit gamma bytes like the world's, so the
            // booth needs the same final node — the FFXGlow combine owns the frame's ONE decode.
            // This also keeps the bake at exact world parity (same glow, same transform chain);
            // without it the portrait reads one encode too bright.
            crate::ffx_glow::FfxGlow::WORLD,
            Msaa::Off,
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
                pending: Vec::new(),
            },
        );
    }

    // The two **body** booths — the character window's paper doll (decision 0208 §5) and the
    // inspect window's pane (decision 0631 §4). Same off-screen pipeline as the portrait slots
    // (transparent target, HDR + the FFXGlow node, negative order so the bake is ready before
    // the world/UI cameras), but their own 512² targets, their own layers, and a body-framing
    // projection (aimed per-bake by `sync_body_booth`). Kept a separate spawn from the portrait
    // loop above so those two cameras stay byte-for-byte what the director approved.
    for (i, (slot, layer_index)) in [
        (PAPERDOLL_SLOT, PAPERDOLL_LAYER),
        (INSPECT_SLOT, INSPECT_LAYER),
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
            Camera3d::default(),
            Camera {
                order: -100 + (SLOTS.len() + i) as isize,
                // A near-black backdrop like the portraits (the pipeline is proven opaque; the pane
                // reads as a recessed dark panel, close to the ref's model-frame background). A
                // transparent float-over-the-frame-art backdrop is a director's-call follow-up.
                clear_color: ClearColorConfig::Custom(Color::srgb(0.055, 0.045, 0.04)),
                ..default()
            },
            RenderTarget::Image(image.clone().into()),
            bevy::render::view::Hdr,
            Tonemapping::None,
            // Decode, but NO glow: these two stand in for 1.12 `<PlayerModel>` widgets, which the
            // reference paints in the UI strata — after the WorldFrame's own FFX apply — so they
            // never carry the scene glow (decision 0638; [`crate::ffx_glow::FfxGlow::UI_PANE`]).
            crate::ffx_glow::FfxGlow::UI_PANE,
            Msaa::Off,
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
                pending: Vec::new(),
            },
        );
    }

    // The glue booth (decisions 0423 + 0465): its own slot/layer/target, framed per-bake.
    glue_booth::spawn_glue_booth(&mut commands, &mut images, &mut portraits, &mut booths);
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
    mounts: Query<'w, 's, (), With<crate::entities::mount::MountBody>>,
}

impl DressedLook<'_, '_> {
    /// Walk `unit`'s descendants once, collecting its part + rider children. Both empty while the
    /// unit's model is still loading / cube-fallback (no attach path has spawned the parts yet).
    ///
    /// A mounted unit's MOUNT child (decision 0441) is a second creature under the unit — a
    /// portrait/paper-doll shows the character alone, never the horse (the ref's `Model:SetUnit`
    /// binds the player model, not the mount). Its **parts** (the mount's body meshes) are
    /// skipped by pruning on the [`mount::MountBody`] marker; **riders** stay collected from the
    /// whole tree, because the rider's own helm/shoulder/held riders hang under the rider's
    /// joints, which re-root under the mount's seat anchor INSIDE the mount subtree while
    /// mounted — and a mount model never carries equipment riders of its own.
    fn collect(
        &self,
        unit: Entity,
    ) -> (
        Vec<&PortraitPart>,
        Vec<&PortraitRider>,
        Vec<&PortraitBillboard>,
    ) {
        let mut parts = Vec::new();
        let mut riders = Vec::new();
        let mut billboards = Vec::new();
        let mut stack: Vec<(Entity, bool)> = vec![(unit, false)];
        while let Some((e, mut in_mount)) = stack.pop() {
            in_mount |= self.mounts.contains(e);
            if !in_mount {
                if let Ok(p) = self.parts.get(e) {
                    parts.push(p);
                }
                // The eye-glow rides the character, not the mount (a portrait shows the rider
                // alone), so it prunes on `in_mount` exactly like a body part.
                if let Ok(b) = self.billboards.get(e) {
                    billboards.push(b);
                }
            }
            if let Ok(r) = self.riders.get(e) {
                riders.push(r);
            }
            if let Ok(c) = self.children.get(e) {
                stack.extend(c.iter().map(|child| (child, in_mount)));
            }
        }
        (parts, riders, billboards)
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
    // The party slots' roster + entity index + the skin-palette table (decision 0720) —
    // one tuple param (the 16-SystemParam ceiling).
    party: (
        Res<crate::ui_party::GroupState>,
        Res<crate::net::GuidIndex>,
        ResMut<crate::rig_palette::RigPalettes>,
    ),
) {
    if test_mode(&mut env_cache) {
        return; // the test bake owns the booths
    }
    let (party_roster, party_index, mut palettes) = party;
    for token in SLOTS {
        let unit: Option<Entity> = match token {
            "player" => self_q.single().ok(),
            "target" => selection.target,
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
            }
            let live = PortraitSource::Live(booth.target.clone());
            if portraits.0.get(token) != Some(&live) {
                portraits.0.insert(token.to_string(), live);
            }
            continue;
        };
        // The unit's dressed look: its attach-spawned part children (geosets filtered, composited
        // materials) + its bone riders (helm/shoulders/held — grandchildren under joint entities)
        // + its eye-glow billboard anchors, one descendants walk. Parts empty while the model is
        // still loading / cube-fallback.
        let (parts, riders, billboards) = look.collect(unit);
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
        let key: PartsKey = parts
            .iter()
            .map(|p| (p.static_mesh.id(), p.material.id()))
            .chain(riders.iter().map(|r| (r.static_mesh.id(), r.material.id())))
            .chain(billboards.iter().map(|b| (b.mesh.id(), b.material.id())))
            .collect();
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
                    parts.len(),
                    riders.len(),
                    billboards.len(),
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
            // The eye-glow, seated on its eye bone's booth joint and camera-faced by the booth
            // (relit onto the studio buffer like everything else — harmless, the glow is fullbright).
            let booth_billboards: Vec<BoothBillboardSpec> = billboards
                .iter()
                .map(|b| BoothBillboardSpec {
                    mesh: b.mesh.clone(),
                    material: booth_light.studio.variant(&b.material, &mut wow_mats),
                    bone: b.bone,
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
            spawn_booth_model(
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
            // Frame through the display's authored portrait camera (heuristic anchors for the
            // camera-less few), resolved above before anything was torn down.
            log_frame(token, &anchors, &frame(&anchors).0);
            aim(&mut cams, token, &frame(&anchors));
            log_bake(token, "bake", parts.len(), riders.len(), billboards.len());
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
/// frame. Like the portrait slots, the booth renders *unconditionally* once the player model exists
/// (no draw-gating on whether the pane is sampled) — one continuous 512² pass; gating it on window
/// visibility is a noted follow-up, not done here to match the existing slots' simplicity.
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
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut env_cache: Local<Option<bool>>,
    mut last_yaw: Local<Option<f32>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
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
        &mut last_yaw,
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
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut env_cache: Local<Option<bool>>,
    mut last_yaw: Local<Option<f32>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
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
        &mut last_yaw,
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
    );
}

/// Bake `unit`'s full-body dressed look into the `slot` booth at `yaw` — the shared body of both
/// body booths (decision 0208 §5 for the paper doll, 0631 §4 for inspect). `unit` is `None` when
/// there is nothing to show, which empties the booth.
#[allow(clippy::too_many_arguments)]
fn sync_body_booth(
    palettes: &mut crate::rig_palette::RigPalettes,
    slot: &str,
    unit: Option<Entity>,
    yaw: f32,
    last_yaw: &mut Option<f32>,
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
) {
    let Some(booth) = booths.0.get_mut(slot) else {
        return;
    };
    // There is no 2D stand-in for a body pane — the bridge always points at the live target (an
    // empty booth just renders the dark backdrop until the unit's model attaches).
    let live = PortraitSource::Live(booth.target.clone());
    if portraits.0.get(slot) != Some(&live) {
        portraits.0.insert(slot.to_string(), live);
    }
    // The unit's dressed look — the same `PortraitPart`/`PortraitRider` descendants the portrait
    // slots mirror. Empty while there's no unit, or its model hasn't attached yet.
    let (parts, riders, billboards) = match unit {
        Some(unit) => look.collect(unit),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    if parts.is_empty() {
        // No unit / model not attached → empty the booth and forget the applied yaw (so it
        // re-applies on the next bake).
        if booth.baked.is_some() {
            commands.entity(booth.root).despawn_related::<Children>();
            booth.baked = None;
            *last_yaw = None;
            // Render the emptied stage before sleeping (decision 0540).
            booth.wake = BOOTH_SETTLE_FRAMES;
            booth.pending.clear();
        }
        return;
    }
    let unit = unit.expect("unit present — parts came from its descendants");
    let key: PartsKey = parts
        .iter()
        .map(|p| (p.static_mesh.id(), p.material.id()))
        .chain(riders.iter().map(|r| (r.static_mesh.id(), r.material.id())))
        .chain(billboards.iter().map(|b| (b.mesh.id(), b.material.id())))
        .collect();
    let parts_changed = booth.baked.as_ref() != Some(&key);
    if parts_changed {
        let display_id = ent_q.get(unit).ok().and_then(|n| n.display_id);
        // Anchors first, before any teardown — a still-loading display must not be framed from
        // fabricated zero bounds (see the portrait site, and `booth_anchors`).
        let Some(anchors) = booth_anchors(creatures, display_id) else {
            booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
            log_bake(
                slot,
                "wait-anchors",
                parts.len(),
                riders.len(),
                billboards.len(),
            );
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
        // The eye-glow, seated on its eye bone's booth joint and camera-faced by the booth (relit
        // onto the studio buffer like everything else — harmless, the glow is fullbright).
        let booth_billboards: Vec<BoothBillboardSpec> = billboards
            .iter()
            .map(|b| BoothBillboardSpec {
                mesh: b.mesh.clone(),
                material: booth_light.pane.variant(&b.material, wow_mats),
                bone: b.bone,
                kind: b.kind,
            })
            .collect();
        // Same law as the portrait bake: never latch a world-lane material into the pane. Leave
        // `booth.baked` alone and retry next frame (see the portrait site for the full note).
        if booth_light.pane.take_unready() {
            booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
            return;
        }
        commands.entity(booth.root).despawn_related::<Children>();
        spawn_booth_model(
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
            BoothMotion::Frozen,
            [false, false], // a still portrait sheaths its weapons — no in-hand grip
            &booth_billboards,
        );
        // Body framing from the display's bounds — the full standing figure, feet-to-crown.
        // Resolved before the teardown above; see the portrait site for why it cannot be faked.
        log_frame(slot, &anchors, &body_frame(&anchors).0);
        aim(cams, slot, &body_frame(&anchors));
        log_bake(slot, "bake", parts.len(), riders.len(), billboards.len());
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
    // Yaw → the model root's rotation (the ref's `Model:SetRotation`; a spin about WoW +Z-up
    // conjugates to a spin about Bevy +Y-up). Applied on a fresh bake (new root children) or
    // whenever the pane's yaw moves — never touched on an otherwise-idle frame.
    if parts_changed || *last_yaw != Some(yaw) {
        commands
            .entity(booth.root)
            .insert(Transform::from_rotation(Quat::from_rotation_y(yaw)));
        *last_yaw = Some(yaw);
        // A spin is a content edge too (decision 0540): render the new pose, then sleep.
        booth.wake = booth.wake.max(BOOTH_SETTLE_FRAMES);
    }
}

/// The demand-render gate (decision 0540): each booth camera is active only while its booth has
/// something new to show — [`Booth::wake`] frames after a content edge, or a bake texture still
/// in flight ([`Booth::pending`]) — except the glue booth, whose scene is live-animated (looping
/// sequences, global-sequence bones, particle emitters) and renders continuously while a glue
/// screen shows. A sleeping camera skips its whole pass (clear + model + FFXGlow chain); its
/// target keeps the last render — exactly right for a still (the 0105 bake, frozen at Stand).
/// With `WOW_PORTRAIT_TEST` set the gate stands down (the eyeball harness wants live cameras).
fn gate_booth_cameras(
    mut booths: ResMut<Booths>,
    preview: Res<GluePreview>,
    images: Res<Assets<Image>>,
    time: Res<Time<bevy::time::Real>>,
    mut cams: Query<(&BoothCam, &mut Camera)>,
    mut env_cache: Local<Option<bool>>,
) {
    let test = test_mode(&mut env_cache);
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
        let live_scene = token.as_str() == GLUE_SLOT && preview.scene.is_some();
        let active = test || live_scene || booth.wake > 0 || !booth.pending.is_empty();
        // `WOW_BOOTH_LOG=1`: the gate's timeline — every activity flip and every armed frame,
        // wall-stamped (the first-login black-pane hunt).
        static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *LOG.get_or_init(|| std::env::var_os("WOW_BOOTH_LOG").is_some())
            && (cam.is_active != active || active)
        {
            eprintln!(
                "[booth] t={:7.2} {} active={} wake={} pending={}",
                time.elapsed_secs(),
                token.as_str(),
                active,
                booth.wake,
                booth.pending.len(),
            );
        }
        if cam.is_active != active {
            cam.is_active = active;
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
