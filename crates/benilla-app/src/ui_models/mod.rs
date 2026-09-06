//! **UI model tiles** (decision 2008) — the renderer for a `<Model>` widget's M2: the cooldown
//! sweep, the autocast shine, the minimap and world-map pings, the item-push card, the map's
//! arrow, and whatever an addon parks in a `CreateFrame("Model")`.
//!
//! ## The law (wow-re `system/ui/scratch/modelframe-render-law.md`, `e1b1794b`)
//!
//! A `<Model>` with no M2 camera picked draws its scene **orthographically over the frame's
//! rect**: origin at the rect's bottom-left, `+X` right, `+Y` up, `Z` depth only, with the root
//! matrix `T(pos · layoutScale) · R(facing, +Z) · S(G48 · 5/3 · modelScale · layoutScale)` — so one
//! model unit is `1280 · modelScale · layoutScale` FrameXML units, aspect-independent (§2/§3).
//! Every batch draws once, LEQUAL over a depth buffer cleared for the widget's own rect, straight
//! into the back buffer (§6); every in-game UI M2 is UNLIT on every material (§5.7); the
//! animator is the world's, on the widget's private clock (§4). A particle's half-extent is the
//! one quantity outside the unit law — added in eye space, it maps at `768 · √(a²+1)` FrameXML
//! units per model unit and carries neither scale (`clip-and-scale.md` §6).
//!
//! ## The shape here: tiles in one atlas, composited at the callback rank
//!
//! The reference draws into the back buffer between two 2-D batches; this engine's UI is one
//! quad pass, so a scene becomes a **tile**: every visible pane holding a file renders into its
//! own cell of one shared render-target atlas, at the pane's device-pixel size, through ONE
//! orthographic camera whose view is the atlas plane — each tile's model root is placed at its
//! cell, scaled to pixels per model unit, and the camera never moves. [`compose_tiles`] then
//! draws every cell as a premultiplied quad over its pane's rect at `ZKey::callback(Artwork)`
//! (1995's rank — after every texture and font string of the pane's layer), which is the same
//! picture the reference's callback drain produces: the cell is cleared to transparent like the
//! reference clears its depth, the 2-D layers under it stay under it, and the ones over it stay
//! over it. Cells never overlap, so one depth buffer serves every tile.
//!
//! **The composite is this renderer's per-frame output, never the extract's** (decision 2023).
//! The extract's `ModelPane` arm publishes the request — the pane's rect, paint key, alpha and
//! clip beside the unit ladder — and pushes no quad; the quad is appended in the
//! [`UiQuadAppend`] lane (the minimap fill's lane) from THIS frame's cells. The first shape had
//! the arm draw the cell it found in the bridge, which is last frame's at best and, because the
//! conversion is memoized on the engine's list, usually never: a cooldown armed on a quiet
//! interface extracted once (no cell yet), the cell arrived a frame later, and nothing ever
//! re-ran the conversion — the sweep drew only while the interface happened to be churning (the
//! stance bar at UI load), and never on an action press.
//!
//! The pipeline is the booths' (`crate::portrait`): the same HDR view shape, the same
//! `FfxGlow::UI_PANE` decode, the same material twin with only the light storage swapped, the
//! same collapsed rig lane and palette mirror, the same effect lane. What is new is the
//! orthographic preset, the atlas packing, and the clock: a tile samples the file at the play
//! head the ENGINE holds (decision 2007 — `UiScript::visible_model_panes`, read once per frame),
//! so the `AnimationPlayer` is paused and seeked rather than advanced, and every per-sequence
//! material track is sampled off that same cursor.
//!
//! ## What a tile's light is
//!
//! `<Model>`'s embedded light is DISABLED (§5.2), and a LIT batch under no light renders black —
//! which never shows on the shipped UI M2s because all of them are unlit, and which is the
//! faithful answer for an addon's lit one. So the tile light buffer is a black light (no
//! ambient, no diffuse, fog off) and unlit batches bypass it. `SetLight` is stored by the engine
//! and not read here (the reference's `SetLight(0, …)` is a no-op too; the enabled form has no
//! caller in 1.12's FrameXML).
//!
//! ## Texture transforms (decision 2019)
//!
//! A batch whose texture transform animates gets a material of its own per tile — a clone of
//! the twin with two mat-anim rows: the translation delta (the world's lane, `anim_slots.x`)
//! and the **affine** row (`anim_slots.z`: rotation and scale as deltas from the identity), both
//! sampled here off the pane's play head at the sequence's file slot, never off the world clock.
//! The shader composes them as the reference does — `uv' = R((uv + t − p) ⊙ s) + p` — which is
//! how the cooldown indicator's four quadrant quads turn their mask into the clockwise sweep.
//!
//! ## What is deliberately NOT here yet
//!
//! - The **perspective leg** (`SetCamera(n)` naming a real M2 camera; the character panes): a
//!   plain `<Model>` in the shipped interface never picks one, and the character panes keep
//!   their booths. Named, not built.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{OrthographicProjection, Projection, RenderTarget, ScalingMode};
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::render::render_resource::Buffer;
use bevy::render::renderer::{RenderDevice, RenderQueue};

use benilla_assets::materials::WowModelMaterial;
use benilla_assets::{m2_url, quantize, M2Model, WorldAssets};
use benilla_formats::{SeqLoops, UvAnim};
use benilla_ui::script::{ModelPaneFrame, UiScript};
use benilla_ui::widget::{FrameHandle, ModelFileFacts, SequenceFacts};
use benilla_world::doodad_anim::spawn_anim_host;
use benilla_world::lighting::LightBlob;
use benilla_world::mat_anim_table::{affine_row, MatAnimMirrors, MatAnimTable};
use benilla_world::model_forms::ModelForms;
use benilla_world::model_render::M2BatchMaterials;
use benilla_world::particles::buffer::EffectLightOverride;
use benilla_world::particles::{
    spawn_emitter, EmitClock, EmitterFrames, OwnerLoss, ParticleEmitter,
};
use benilla_world::rig_anim::{GlobalSeqDrive, RigPose};
use benilla_world::rig_palette::{RigPaletteMirrors, RigPalettes, RigPart, RigSkin};

use crate::portrait::{
    booth_view_shape, material_variant, new_target_image_sized, StageRig, UI_MODELS_LAYER,
};
use crate::ui_pass::{UiQuad, UiQuadAppend, UiQuads, UvRect};

/// `WOW_TILE_TRACE=1` — the tile probe: one `tile-trace:` line per pane per frame from the
/// renderer (the request, the cell, the play head, the sampled alphas and the rows written) and
/// one from the extract's composite arm (the quad's rect, rank and alpha, or "no cell yet").
/// A pane that is on the engine's paint list but draws nothing names the gate it stopped at.
/// The `test_ui` cooldown tests prove the engine scrubs; this is the instrument for the half
/// they cannot reach — whether the tile exists, where it is, and what it sampled. Read once.
pub(crate) fn trace_on() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var("WOW_TILE_TRACE").as_deref() == Ok("1"));
    *ON
}

/// One pane's request for a tile this frame — what the extract knows about the widget: its
/// size on the device, the unit ladder the render law derives from it, and the Lua-set scene.
/// Published by the extract's `ModelPane` arm (keyed by the pane's frame handle, overwritten on
/// every conversion), read by [`sync_tiles`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileRequest {
    /// The `SetModel` path, as written.
    pub path: String,
    /// The pane's rect on the device, whole pixels — the tile's cell size.
    pub size_px: UVec2,
    /// Device pixels per **model unit** for the geometry: `1280 · modelScale · layoutScale`
    /// FrameXML units per unit, times the seam scale, times the DPI.
    pub px_per_unit: f32,
    /// Device pixels per **layout unit** — `SetPosition`'s space (`T(pos · layoutScale)`):
    /// `768 · √(a²+1) · layoutScale` FrameXML units per unit, times seam and DPI.
    pub pos_px_per_unit: f32,
    /// Device pixels per model unit for a **particle's half-extent** — eye space, no scale:
    /// `768 · √(a²+1)` FrameXML units per unit, times seam and DPI.
    pub star_px_per_unit: f32,
    /// `SetFacing`, radians about the screen normal (CCW positive, the reference's `+Z`).
    pub facing: f32,
    /// `SetPosition`, layout units.
    pub position: Vec3,
    /// `ReplaceIconTexture`'s path — the type-14 batches' texture.
    pub icon: Option<String>,
    /// The pane's rect on the window — y-down logical px, the quad pass's space — where the
    /// cell composites.
    pub rect: Rect,
    /// The pane's paint key: `ZKey::callback(Artwork)` (1995), the composite's rank.
    pub z_key: u64,
    /// The frame's OWN alpha (render law §4.4): the composite draws at it.
    pub alpha: f32,
    /// The enclosing ScrollFrame clip, if any (decision 0112), in the quad pass's space.
    pub clip: Option<Rect>,
}

/// Where a tile sits in the atlas — texel space, `y` down — for the composite quad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    pub origin: UVec2,
    pub size: UVec2,
}

/// The extract ↔ renderer bridge (the `BoothPanes` pattern): requests in, cells and the atlas
/// out. Lives on `crate::portrait::BoothBridge` so the extract reaches it through the seam it
/// already holds.
#[derive(Resource, Default)]
pub(crate) struct UiModelTiles {
    /// The last request per pane. A pane that stops being extracted keeps its stale entry
    /// (the memoized conversion cannot tell the renderer it vanished); which panes DRAW is the
    /// engine's paint list, never this map.
    pub requests: HashMap<FrameHandle, TileRequest>,
    /// This frame's cell per tile that has something to draw.
    pub cells: HashMap<FrameHandle, Cell>,
    /// The atlas image and its size — `None` until the first tile.
    pub atlas: Option<Handle<Image>>,
    pub atlas_size: UVec2,
    /// The window's DPI as the last extract saw it (device px per logical px) — the extract
    /// writes it, the arm reads it to size cells.
    pub dpi: f32,
}

/// The renderer's own fixed state: the camera, its layer, the black light buffer and the
/// per-material twin cache against it.
#[derive(Resource)]
struct TileRig {
    layer: RenderLayers,
    light: Option<Buffer>,
    variants: HashMap<AssetId<WowModelMaterial>, Handle<WowModelMaterial>>,
}

/// Marks the tile camera.
#[derive(Component)]
struct TileCamera;

/// Marks a tile's root entity (the model root, at its cell).
#[derive(Component)]
struct TileRoot;

/// One batch of a built tile whose alpha the file animates: sampled here off the pane's play
/// head (a hosted `MatAnim` would read the paused player, which has no node for a sequence
/// that keys no bone — the cooldown's sweep is exactly that).
struct AlphaPart {
    entity: Entity,
    anim: Arc<benilla_formats::AlphaAnim>,
}

/// One batch whose texture transform animates: the tile's OWN clone of the batch's material
/// (two panes on one file must not share a row — two cooldowns at different fractions), with
/// the table rows it writes per frame off the pane's play head (decision 2019).
struct UvPart {
    /// Held so the clone outlives its parts' handles by exactly the tile's lifetime.
    #[allow(dead_code)]
    material: Handle<WowModelMaterial>,
    /// The translation row: its slot, and the built seed the delta is measured from
    /// (`sun_scale.zw`, the loop's sample at 0).
    trans: Option<(u16, [f32; 2])>,
    /// The affine row's slot — rotation and scale ([`affine_row`]).
    affine: Option<u16>,
    uv_anim: Option<Arc<UvAnim>>,
    uv_seq: Option<Arc<SeqLoops<[f32; 2]>>>,
    uv_rot: Option<Arc<SeqLoops<[f32; 4]>>>,
    uv_scale: Option<Arc<SeqLoops<[f32; 2]>>>,
}

impl UvPart {
    /// Write this frame's rows for the sequence at `(seq_slot, cursor_s)` on the pane's clock
    /// `gseq_s`: the translation delta (quantized like the world's lane), and the affine row
    /// from the raw quaternion and the scale.
    fn write_rows(
        &self,
        table: &mut MatAnimTable,
        seq_slot: Option<usize>,
        cursor_s: f32,
        gseq_s: f64,
    ) {
        if let Some((slot, seed)) = self.trans {
            let uv = match (&self.uv_seq, &self.uv_anim) {
                (Some(seqs), _) => seqs
                    .seq(seq_slot)
                    .map_or([0.0, 0.0], |l| l.sample(l.clock(cursor_s, gseq_s))),
                (None, Some(a)) => a.sample(a.clock(cursor_s, gseq_s)),
                (None, None) => [0.0, 0.0],
            };
            table.set(
                slot,
                [
                    quantize(uv[0], 4096.0) - seed[0],
                    quantize(uv[1], 4096.0) - seed[1],
                    0.0,
                    0.0,
                ],
            );
        }
        if let Some(slot) = self.affine {
            let q = self
                .uv_rot
                .as_ref()
                .and_then(|r| r.seq(seq_slot))
                .map_or([0.0, 0.0, 0.0, 1.0], |l| {
                    l.sample(l.clock(cursor_s, gseq_s))
                });
            let sc = self
                .uv_scale
                .as_ref()
                .and_then(|r| r.seq(seq_slot))
                .map_or([1.0, 1.0], |l| l.sample(l.clock(cursor_s, gseq_s)));
            table.set(slot, affine_row(q, sc));
        }
    }

    fn free(&self, table: &mut MatAnimTable) {
        if let Some((slot, _)) = self.trans {
            table.free(slot);
        }
        if let Some(slot) = self.affine {
            table.free(slot);
        }
    }
}

/// A live tile: its entity tree and what it was built from.
struct Tile {
    root: Entity,
    /// The file key the tree was built for (a `SetModel` to another file rebuilds).
    key: String,
    /// The icon override the tree was built with (a change rebuilds the materials).
    icon: Option<String>,
    m2: Handle<M2Model>,
    /// The tree is spawned (parts, rig, emitters) — until then the root is bare.
    built: bool,
    /// The graph node per `AnimationData` id the file keys a bone for, and its file slot.
    clips: HashMap<u16, (AnimationNodeIndex, usize)>,
    /// Which id the player is currently arming (to re-arm only on change).
    armed: Option<u16>,
    alpha_parts: Vec<AlphaPart>,
    uv_parts: Vec<UvPart>,
    emitters: Vec<Entity>,
    /// The last frame this tile was on the engine's paint list.
    last_seen: u64,
}

impl Tile {
    /// Tear the tile down: its tree, and the table rows its animated materials held.
    fn retire(self, commands: &mut Commands, table: &mut MatAnimTable) {
        for p in &self.uv_parts {
            p.free(table);
        }
        commands.entity(self.root).despawn();
    }
}

/// Frames a tile survives off the paint list before its tree is torn down — long enough that a
/// cooldown that re-arms every few seconds, or a ping, keeps its tree.
const TILE_LINGER_FRAMES: u64 = 600;

/// The atlas edge the first tile allocates, and the cap a grown atlas stops at.
const ATLAS_MIN: u32 = 512;
const ATLAS_MAX: u32 = 4096;

/// Gutter between cells (texels): a tile's bilinear edge never samples a neighbour.
const GUTTER: u32 = 2;

/// The tile camera's order — after every booth (`-100 …`), before the UI camera (`1`).
const TILE_CAMERA_ORDER: isize = -10;

/// The renderer's per-frame state that is not the bridge.
#[derive(Default)]
struct TileState {
    tiles: HashMap<FrameHandle, Tile>,
    /// Files the engine asked facts for, loading.
    pending_facts: HashMap<String, Handle<M2Model>>,
    /// Files whose facts were handed over — the handle kept alive for the tiles.
    loaded: HashMap<String, Handle<M2Model>>,
    frame: u64,
}

pub(crate) struct UiModelsPlugin;

impl Plugin for UiModelsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiModelTiles>()
            .init_non_send_resource::<TileState>()
            .add_systems(Startup, setup_tiles)
            // After the extract published this frame's requests, and before the pose/palette
            // passes read the roots' transforms (they run in PostUpdate).
            .add_systems(Update, sync_tiles.after(crate::ui_script::UiInput))
            // The composite: this frame's cells, appended in the lane the minimap fill uses —
            // after the cells are packed, before the mesh rebuild reads the lane.
            .add_systems(Update, compose_tiles.in_set(UiQuadAppend).after(sync_tiles));
    }
}

/// Startup: the camera, the layer, the black light.
fn setup_tiles(
    mut commands: Commands,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut mirrors: ResMut<RigPaletteMirrors>,
    mut anim_mirrors: ResMut<MatAnimMirrors>,
) {
    // `<Model>`'s embedded light is disabled: no ambient, no diffuse, no fog (render law §5.2).
    // The direction is irrelevant at zero intensity; the builder wants one.
    let blob = LightBlob::model([0.0; 3], [0.0; 3], Vec3::NEG_Y);
    let light = blob.create(&device, "wow_ui_model_light");
    blob.write(&queue, &light);
    // Tile rigs skin from THIS buffer's palette region (decision 0720's mirror law), and the
    // tiles' animated materials read their mat-anim rows from it too (decision 2023): a twin
    // binds this buffer, not the world's, so the table has to be mirrored here or the rows the
    // tiles write every frame reach a buffer nothing in a tile ever samples.
    mirrors.0.insert("ui_models", light.clone());
    anim_mirrors.0.insert("ui_models", light.clone());
    let layer = RenderLayers::layer(UI_MODELS_LAYER);
    commands.spawn((
        Name::new("ui model tiles camera"),
        booth_view_shape(),
        Camera {
            order: TILE_CAMERA_ORDER,
            // The reference clears DEPTH for the widget's rect and leaves colour to the 2-D
            // pass; a tile composites over the 2-D pass instead, so its colour clears to
            // nothing — the premultiplied transparent the booth panes use (decision 1083).
            clear_color: ClearColorConfig::Custom(Color::NONE),
            is_active: false,
            ..default()
        },
        // Decode, no scene glow: a UI model draws in the UI strata after the WorldFrame's
        // FFX apply (decision 0638's law for the body panes, the same widget family).
        benilla_world::ffx_glow::FfxGlow::UI_PANE,
        Projection::Orthographic(OrthographicProjection {
            near: 0.1,
            far: 2000.0,
            scaling_mode: ScalingMode::Fixed {
                width: ATLAS_MIN as f32,
                height: ATLAS_MIN as f32,
            },
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 1000.0),
        layer.clone(),
        TileCamera,
    ));
    commands.insert_resource(TileRig {
        layer,
        light: Some(light),
        variants: HashMap::new(),
    });
}

/// The bevy-space → tile-camera-space rotation: WoW `+X` (bevy `−Z`) to the right, WoW `+Y`
/// (bevy `−X`) up, WoW `+Z` (bevy `+Y`) toward the viewer — the ortho leg's axes (§2). A proper
/// rotation (determinant +1), so winding survives.
fn wow_to_screen() -> Quat {
    Quat::from_mat3(&Mat3::from_cols(
        Vec3::new(0.0, -1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(-1.0, 0.0, 0.0),
    ))
}

/// The assets a tile build reads and writes, in one param (the 16-parameter ceiling).
#[derive(bevy::ecs::system::SystemParam)]
struct TileAssets<'w> {
    asset_server: Res<'w, AssetServer>,
    m2s: Res<'w, Assets<M2Model>>,
    images: ResMut<'w, Assets<Image>>,
    world: Option<ResMut<'w, WorldAssets>>,
    forms: ResMut<'w, ModelForms>,
    meshes: ResMut<'w, Assets<Mesh>>,
}

/// The render-side resources a tile build spends.
#[derive(bevy::ecs::system::SystemParam)]
struct TileRender<'w> {
    /// The batch materials — and, through it, the material store the twins are added to (a
    /// second `ResMut<Assets<WowModelMaterial>>` beside it would conflict at schedule time).
    mats: M2BatchMaterials<'w>,
    palettes: ResMut<'w, RigPalettes>,
    rig: ResMut<'w, TileRig>,
    /// The shared mat-anim table: the tiles' animated materials own rows in it (2019).
    table: ResMut<'w, MatAnimTable>,
}

/// The per-frame pass: feed the engine the facts it asked for, keep one tile per visible pane,
/// pack the atlas, place every tile at its cell and its play head, and aim the camera.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // a Bevy system's full input set
fn sync_tiles(
    mut commands: Commands,
    script: Option<NonSendMut<UiScript>>,
    mut state: NonSendMut<TileState>,
    mut bridge: ResMut<UiModelTiles>,
    mut assets: TileAssets,
    mut render: TileRender,
    mut cams: Query<
        (
            &mut Camera,
            &mut RenderTarget,
            &mut Projection,
            &mut Transform,
        ),
        With<TileCamera>,
    >,
    mut roots: Query<
        (
            &mut Transform,
            &mut Visibility,
            Option<&mut AnimationPlayer>,
        ),
        (With<TileRoot>, Without<TileCamera>),
    >,
    mut parts: Query<(&mut MeshTag, &mut Visibility), (Without<TileRoot>, Without<TileCamera>)>,
    mut emitters: Query<&mut ParticleEmitter>,
) {
    state.frame += 1;
    let frame = state.frame;
    let Some(mut script) = script else {
        // No VM: nothing paints. Tear everything down so a dead UI leaves no live camera.
        for (_, tile) in state.tiles.drain() {
            tile.retire(&mut commands, &mut render.table);
        }
        bridge.cells.clear();
        set_camera_active(&mut cams, false);
        return;
    };

    // ── 1. Facts: what the engine asked for, answered when the file lands ───────────────
    for key in script.model_facts_wanted() {
        if state.loaded.contains_key(&key) || state.pending_facts.contains_key(&key) {
            continue;
        }
        let handle = assets.asset_server.load::<M2Model>(m2_url(&key));
        state.pending_facts.insert(key, handle);
    }
    let landed: Vec<(String, Handle<M2Model>)> = state
        .pending_facts
        .iter()
        .filter(|(_, h)| assets.m2s.contains(*h))
        .map(|(k, h)| (k.clone(), h.clone()))
        .collect();
    for (key, handle) in landed {
        if let Some(model) = assets.m2s.get(&handle) {
            script.set_model_facts(&key, facts_of(model));
        }
        state.pending_facts.remove(&key);
        state.loaded.insert(key, handle);
    }

    // ── 2. The paint list, and one tile per pane on it ──────────────────────────────────
    let panes: Vec<ModelPaneFrame> = script.visible_model_panes();
    let mut live: Vec<(FrameHandle, TileRequest, ModelPaneFrame)> = Vec::new();
    for pane in panes {
        let Some(req) = bridge.requests.get(&pane.handle) else {
            if trace_on() {
                info!(
                    "tile-trace: pane {:?} {} (under {}) on the paint list, not extracted yet",
                    pane.handle,
                    script.frame_name(pane.handle).unwrap_or_default(),
                    script
                        .target_owner_name(benilla_ui::order::ZTarget::Frame(pane.handle))
                        .unwrap_or_default()
                );
            }
            continue; // not extracted yet — next frame
        };
        if req.size_px.x == 0 || req.size_px.y == 0 {
            if trace_on() {
                info!(
                    "tile-trace: {} pane {:?} has a zero rect",
                    req.path, pane.handle
                );
            }
            continue;
        }
        live.push((pane.handle, req.clone(), pane));
    }
    // Stable cell order: the engine's registry order (creation order), so a pane keeps its
    // cell across frames.
    let _ = &live;

    for (handle, req, _) in &live {
        let key = benilla_ui::widget::model_key(&req.path);
        let Some(m2) = state.loaded.get(&key).cloned() else {
            if trace_on() {
                info!("tile-trace: {} facts not landed (key {key})", req.path);
            }
            continue; // facts not landed ⇒ the engine would not have listed it; defensive
        };
        let stale = state
            .tiles
            .get(handle)
            .is_some_and(|t| t.key != key || t.icon != req.icon);
        if stale {
            if let Some(t) = state.tiles.remove(handle) {
                t.retire(&mut commands, &mut render.table);
            }
        }
        let tile = state.tiles.entry(*handle).or_insert_with(|| Tile {
            root: commands
                .spawn((
                    Transform::IDENTITY,
                    Visibility::Hidden,
                    render.rig.layer.clone(),
                    TileRoot,
                ))
                .id(),
            key: key.clone(),
            icon: req.icon.clone(),
            m2: m2.clone(),
            built: false,
            clips: HashMap::new(),
            armed: None,
            alpha_parts: Vec::new(),
            uv_parts: Vec::new(),
            emitters: Vec::new(),
            last_seen: frame,
        });
        tile.last_seen = frame;
        if !tile.built {
            if let Some(model) = assets.m2s.get(&tile.m2) {
                let icon_tex = req.icon.as_deref().and_then(|p| {
                    assets
                        .world
                        .as_mut()
                        .and_then(|w| w.sprite_texture(p, &mut assets.images))
                });
                if let Some(built) = build_tile(
                    &mut commands,
                    tile.root,
                    model,
                    &tile.m2,
                    icon_tex,
                    &mut assets.forms,
                    &mut assets.meshes,
                    &mut render,
                ) {
                    info!(
                        "ui_models: tile built for {} — {} parts, {} emitters, {} animated alphas",
                        req.path,
                        model.submeshes.len(),
                        built.emitters.len(),
                        built.alpha_parts.len()
                    );
                    if trace_on() {
                        for (i, p) in built.uv_parts.iter().enumerate() {
                            info!(
                                "tile-trace: {} uv part {i}: trans slot {:?} affine slot {:?}",
                                req.path,
                                p.trans.map(|(s, _)| s),
                                p.affine
                            );
                        }
                    }
                    tile.clips = built.clips;
                    tile.alpha_parts = built.alpha_parts;
                    tile.uv_parts = built.uv_parts;
                    tile.emitters = built.emitters;
                    tile.built = true;
                } else if trace_on() {
                    info!("tile-trace: {} waiting on materials", req.path);
                }
            } else if trace_on() {
                info!("tile-trace: {} asset not resident", req.path);
            }
        }
    }

    // ── 3. Retire tiles that left the paint list long ago ───────────────────────────────
    let dead: Vec<FrameHandle> = state
        .tiles
        .iter()
        .filter(|(_, t)| frame.saturating_sub(t.last_seen) > TILE_LINGER_FRAMES)
        .map(|(h, _)| *h)
        .collect();
    for h in dead {
        if let Some(t) = state.tiles.remove(&h) {
            t.retire(&mut commands, &mut render.table);
        }
        bridge.requests.remove(&h);
    }

    // ── 4. Pack the atlas ───────────────────────────────────────────────────────────────
    let drawing: Vec<&(FrameHandle, TileRequest, ModelPaneFrame)> = live
        .iter()
        .filter(|(h, _, _)| state.tiles.get(h).is_some_and(|t| t.built))
        .collect();
    let sizes: Vec<UVec2> = drawing.iter().map(|(_, r, _)| r.size_px).collect();
    let (cells, atlas_size) = pack(&sizes);
    if atlas_size != bridge.atlas_size || bridge.atlas.is_none() {
        if atlas_size.x > 0 {
            let image = assets
                .images
                .add(new_target_image_sized(atlas_size.x, atlas_size.y));
            for (_, mut target, mut proj, mut tf) in &mut cams {
                *target = RenderTarget::Image(image.clone().into());
                *proj = Projection::Orthographic(OrthographicProjection {
                    near: 0.1,
                    far: 2000.0,
                    scaling_mode: ScalingMode::Fixed {
                        width: atlas_size.x as f32,
                        height: atlas_size.y as f32,
                    },
                    ..OrthographicProjection::default_3d()
                });
                // The camera looks down `−Z` at the atlas plane, centred; a tile at world
                // `(x, y)` lands at texel `(x, H − y)`.
                *tf = Transform::from_xyz(
                    atlas_size.x as f32 * 0.5,
                    atlas_size.y as f32 * 0.5,
                    1000.0,
                );
            }
            bridge.atlas = Some(image);
        }
        bridge.atlas_size = atlas_size;
    }
    bridge.cells.clear();
    let atlas_h = atlas_size.y as f32;

    // ── 5. Place every drawing tile: cell, unit ladder, facing, play head ───────────────
    let mut hidden: Vec<Entity> = state.tiles.values().map(|t| t.root).collect();
    for (i, (handle, req, pane)) in drawing.iter().enumerate() {
        let Some(cell) = cells.get(i).copied() else {
            continue; // did not fit the capped atlas
        };
        let Some(tile) = state.tiles.get_mut(handle) else {
            continue;
        };
        hidden.retain(|&e| e != tile.root);
        bridge.cells.insert(*handle, cell);
        let Ok((mut tf, mut vis, player)) = roots.get_mut(tile.root) else {
            continue;
        };
        // The root: the cell's bottom-left in camera space, plus `SetPosition` in layout units.
        let cell_bl = Vec2::new(
            cell.origin.x as f32,
            atlas_h - (cell.origin.y + cell.size.y) as f32,
        );
        let pos = Vec2::new(req.position.x, req.position.y) * req.pos_px_per_unit;
        let depth = req.position.z * req.pos_px_per_unit;
        // `T(pos) · R(facing about WoW +Z) · S(px per unit)`, in camera space: the facing turns
        // about bevy `+Y` (WoW's `+Z` after `wow_to_bevy`), then the axis fix, then the scale.
        *tf = Transform {
            translation: Vec3::new(cell_bl.x + pos.x, cell_bl.y + pos.y, depth),
            rotation: wow_to_screen() * Quat::from_rotation_y(req.facing),
            scale: Vec3::splat(req.px_per_unit),
        };
        *vis = Visibility::Visible;

        // The play head: the engine's cursor drives the paused player and every alpha track.
        let (armed, cursor_s, seq_slot) = match pane.play {
            Some(ph) => {
                let slot = tile.clips.get(&ph.anim_id).map(|&(_, s)| s);
                (Some(ph.anim_id), ph.cursor_ms as f32 / 1000.0, slot)
            }
            None => (None, 0.0, None),
        };
        if let Some(mut player) = player {
            if tile.armed != armed {
                player.stop_all();
                if let Some((node, _)) = armed.and_then(|id| tile.clips.get(&id)) {
                    player.play(*node).pause();
                }
                tile.armed = armed;
            }
            if let Some((node, _)) = armed.and_then(|id| tile.clips.get(&id)) {
                if let Some(active) = player.animation_mut(*node) {
                    active.seek_to(cursor_s);
                }
            }
        }
        // The file slot the material tracks read: the armed sequence's, or the file's first
        // when the armed id keys no bone (its slot is still in the facts' order — the cooldown).
        let seq_slot = seq_slot.or_else(|| {
            let id = armed?;
            let facts_slot = assets
                .m2s
                .get(&tile.m2)?
                .sequences
                .iter()
                .find(|s| s.anim_id == id)
                .map(|s| s.seq_index);
            facts_slot
        });
        let gseq_s = pane.clock_ms as f64 / 1000.0;
        let mut trace_alphas: Vec<f32> = Vec::new();
        for part in &tile.alpha_parts {
            let a = part.anim.sample(seq_slot, cursor_s, gseq_s);
            if trace_on() {
                trace_alphas.push(a);
            }
            if let Ok((mut tag, mut pvis)) = parts.get_mut(part.entity) {
                // The `A ≤ 0` cull (wow-re `m2-alpha-combine-cull`): a batch the artist keyed
                // off in this sequence is skipped, not drawn at zero.
                let want = if a > 0.0 {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                if *pvis != want {
                    *pvis = want;
                }
                let bits = benilla_world::mesh_tag::with_alpha(tag.0, a);
                if tag.0 != bits {
                    tag.0 = bits;
                }
            }
        }
        for part in &tile.uv_parts {
            part.write_rows(&mut render.table, seq_slot, cursor_s, gseq_s);
        }
        if trace_on() {
            let rows: Vec<String> = tile
                .uv_parts
                .iter()
                .map(|p| {
                    let t = p.trans.map(|(s, _)| render.table.row(s));
                    let a = p.affine.map(|s| render.table.row(s));
                    format!("t={t:?} a={a:?}")
                })
                .collect();
            info!(
                "tile-trace: {} {} cell=({},{} {}x{}) px/unit={:.2} pos_px/unit={:.2} armed={:?} cursor={:.3}s slot={:?} clock={}ms alphas={:?} rows=[{}]",
                req.path,
                script.frame_name(*handle).unwrap_or_default(),
                cell.origin.x,
                cell.origin.y,
                cell.size.x,
                cell.size.y,
                req.px_per_unit,
                req.pos_px_per_unit,
                armed,
                cursor_s,
                seq_slot,
                pane.clock_ms,
                trace_alphas,
                rows.join(", ")
            );
        }
        for &e in &tile.emitters {
            if let Ok(mut em) = emitters.get_mut(e) {
                em.set_size_scale(req.star_px_per_unit);
            }
        }
    }
    for root in hidden {
        if let Ok((_, mut vis, _)) = roots.get_mut(root) {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        }
    }
    set_camera_active(
        &mut cams,
        !bridge.cells.is_empty() && bridge.atlas.is_some(),
    );
}

#[allow(clippy::type_complexity)] // the system's own query, borrowed
fn set_camera_active(
    cams: &mut Query<
        (
            &mut Camera,
            &mut RenderTarget,
            &mut Projection,
            &mut Transform,
        ),
        With<TileCamera>,
    >,
    active: bool,
) {
    for (mut cam, _, _, _) in cams {
        if cam.is_active != active {
            cam.is_active = active;
        }
    }
}

/// The composite: one premultiplied quad per cell packed THIS frame, over its pane's rect at the
/// pane's paint key and alpha, clipped as the pane is — appended to the UI pass's overlay lane
/// every frame (the lane is cleared at the top of [`UiQuadAppend`] and diffed by the rebuild, so
/// an unchanged set costs no re-batch). A pane with a request and no cell draws nothing; a cell
/// whose request vanished (the linger reaper) draws nothing.
pub(crate) fn compose_tiles(bridge: Res<UiModelTiles>, mut quads: ResMut<UiQuads>) {
    quads.overlays.extend(composite_quads(&bridge));
}

/// [`compose_tiles`]'s pure half: the quads for every `(request, cell)` pair the bridge holds,
/// ordered by paint key so the overlay diff sees the same sequence for the same set.
pub(crate) fn composite_quads(bridge: &UiModelTiles) -> Vec<UiQuad> {
    let Some(atlas) = bridge.atlas.clone() else {
        return Vec::new();
    };
    let a = bridge.atlas_size.as_vec2();
    if a.x <= 0.0 || a.y <= 0.0 {
        return Vec::new();
    }
    let mut out: Vec<UiQuad> = bridge
        .cells
        .iter()
        .filter_map(|(handle, cell)| {
            let req = bridge.requests.get(handle)?;
            let (u0, v0) = (cell.origin.x as f32 / a.x, cell.origin.y as f32 / a.y);
            let (u1, v1) = (
                (cell.origin.x + cell.size.x) as f32 / a.x,
                (cell.origin.y + cell.size.y) as f32 / a.y,
            );
            Some(UiQuad {
                rect: req.rect,
                z_key: req.z_key,
                texture: Some(atlas.clone()),
                uv: UvRect::from_tex_coords([u0, u1, v0, v1]),
                // The instance draws at the widget's OWN alpha (render law §4.4).
                color: [1.0, 1.0, 1.0, req.alpha],
                // A render target: premultiplied by construction (`UiQuad` doc).
                premultiplied: true,
                clip: req.clip,
                ..default()
            })
        })
        .collect();
    out.sort_by_key(|q| q.z_key);
    out
}

/// The engine's facts for a resident file: its sequence table and header bounds.
fn facts_of(model: &M2Model) -> ModelFileFacts {
    ModelFileFacts {
        sequences: model
            .sequences
            .iter()
            .map(|s| SequenceFacts {
                anim_id: s.anim_id,
                duration_ms: s.duration_ms,
                looping: s.looping,
            })
            .collect(),
        bbox: model
            .bounds
            .as_ref()
            .map_or(([0.0; 3], [0.0; 3]), |b| (b.bbox_min, b.bbox_max)),
    }
}

/// Shelf-pack `sizes` (with a gutter) into the smallest power-of-two square atlas from
/// [`ATLAS_MIN`] to [`ATLAS_MAX`] that fits; returns the cells (one per size that fit, in order)
/// and the atlas size chosen (`0×0` for no sizes).
fn pack(sizes: &[UVec2]) -> (Vec<Cell>, UVec2) {
    if sizes.is_empty() {
        return (Vec::new(), UVec2::ZERO);
    }
    let mut edge = ATLAS_MIN;
    loop {
        let cells = shelf_pack(sizes, edge);
        if cells.len() == sizes.len() || edge >= ATLAS_MAX {
            return (cells, UVec2::splat(edge));
        }
        edge *= 2;
    }
}

/// Rows of cells left to right, a new row when one would overflow; stops at the first size
/// that cannot fit the remaining height (so the returned cells are a prefix of `sizes`).
fn shelf_pack(sizes: &[UVec2], edge: u32) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(sizes.len());
    let (mut x, mut y, mut row_h) = (GUTTER, GUTTER, 0u32);
    for &size in sizes {
        let (w, h) = (size.x, size.y);
        if w + 2 * GUTTER > edge || h + 2 * GUTTER > edge {
            break;
        }
        if x + w + GUTTER > edge {
            x = GUTTER;
            y += row_h + GUTTER;
            row_h = 0;
        }
        if y + h + GUTTER > edge {
            break;
        }
        cells.push(Cell {
            origin: UVec2::new(x, y),
            size,
        });
        x += w + GUTTER;
        row_h = row_h.max(h);
    }
    cells
}

/// What [`build_tile`] made.
struct BuiltTile {
    clips: HashMap<u16, (AnimationNodeIndex, usize)>,
    alpha_parts: Vec<AlphaPart>,
    uv_parts: Vec<UvPart>,
    emitters: Vec<Entity>,
}

/// Spawn a file's parts, rig and emitters under `root` on the tile layer — the booth bake's
/// recipe (`portrait::booth::spawn_booth_model`) for a file with no unit. `None` when a material
/// is not resident yet (the caller retries next frame rather than latch a world-lit twin).
#[allow(clippy::too_many_arguments)]
fn build_tile(
    commands: &mut Commands,
    root: Entity,
    model: &M2Model,
    handle: &Handle<M2Model>,
    icon_tex: Option<Handle<Image>>,
    forms: &mut ModelForms,
    meshes: &mut Assets<Mesh>,
    render: &mut TileRender,
) -> Option<BuiltTile> {
    if !render.mats.ready() {
        return None;
    }
    let light = render.rig.light.clone()?;
    let layer = render.rig.layer.clone();
    // The render forms, now (the booth/marker lanes' exception to the paced furnisher: one small
    // model, on demand).
    forms.ensure_now_rigged(handle, &model.submeshes, meshes);
    let built = forms.slices(handle);
    let (stat_forms, skin_forms) = (built.stat, built.skin.unwrap_or(&[]));

    // Materials first — every one must be resident before anything spawns, or a retry would
    // leave half a tree behind.
    let mut part_mats: Vec<Handle<WowModelMaterial>> = Vec::with_capacity(model.submeshes.len());
    let mut uv_parts: Vec<UvPart> = Vec::new();
    for (i, sub) in model.submeshes.iter().enumerate() {
        let texture = if sub.icon_slot {
            icon_tex.clone()
        } else {
            sub.texture.clone()
        };
        let world = render.mats.steady(sub, texture, (i + 1) as u16)?;
        // The twin: same material, the tile's black light, fog OFF (the `rig` leg forces it;
        // the shade selector it also flips is inert on an unlit batch).
        let twin = material_variant(
            &mut render.rig.variants,
            &light,
            &world,
            render.mats.materials(),
            true,
        )?;
        // A batch whose texture transform animates draws through a clone of its own, with its
        // own table rows — the rows are written off THIS pane's play head, so two panes on one
        // file cannot share them (decision 2019).
        let animated = sub.uv_anim.is_some()
            || sub.uv_seq.is_some()
            || sub.uv_rot_seq.is_some()
            || sub.uv_scale_seq.is_some();
        if animated {
            let mut own = render.mats.materials().get(&twin).cloned()?;
            let seed = [own.extension.sun_scale.z, own.extension.sun_scale.w];
            let trans = (sub.uv_anim.is_some() || sub.uv_seq.is_some())
                .then(|| render.table.alloc())
                .flatten()
                .map(|slot| {
                    own.extension.anim_slots.x = f32::from(slot);
                    (slot, seed)
                });
            let affine = (sub.uv_rot_seq.is_some() || sub.uv_scale_seq.is_some())
                .then(|| render.table.alloc())
                .flatten()
                .inspect(|&slot| own.extension.anim_slots.z = f32::from(slot));
            let handle = render.mats.materials().add(own);
            uv_parts.push(UvPart {
                material: handle.clone(),
                trans,
                affine,
                uv_anim: sub.uv_anim.clone(),
                uv_seq: sub.uv_seq.clone(),
                uv_rot: sub.uv_rot_seq.clone(),
                uv_scale: sub.uv_scale_seq.clone(),
            });
            part_mats.push(handle);
        } else {
            part_mats.push(twin);
        }
    }

    // The rig: the collapsed pose buffer + a palette slot, when the file has bones. The tile
    // camera is not the world camera, so bone billboards are left to the rest pose (none of the
    // shipped UI files authors one).
    let mut pose: Option<RigPose> = None;
    let mut slot: u16 = 0;
    let mut clips: HashMap<u16, (AnimationNodeIndex, usize)> = HashMap::new();
    if !model.skeleton.joints.is_empty() {
        let p = RigPose::new(root, &model.skeleton).without_camera_billboards();
        slot = RigSkin::allocate_bones(
            &mut render.palettes,
            model.skeleton.joints.len() as u32,
            model.inverse_bindposes.clone(),
        )
        .map_or(0, |rig| {
            let s = rig.slot;
            commands.entity(root).insert(rig);
            render.palettes.mark_mirrored(s);
            s
        });
        if let Some(anims) = &model.animations {
            for c in &anims.clips {
                clips.entry(c.anim_id).or_insert((c.node, c.seq_index));
            }
            // A paused player: the engine's play head seeks it every frame (2007's clock).
            let mut player = AnimationPlayer::default();
            player.stop_all();
            commands.entity(root).insert((
                player,
                AnimationGraphHandle(anims.graph.clone()),
                anims.clone(),
            ));
            if let Some(drive) = GlobalSeqDrive::new_rig(&anims.global_bones, p.locals.len()) {
                commands.entity(root).insert(drive);
            }
        }
        pose = Some(p);
    }

    // The parts.
    let mut alpha_parts = Vec::new();
    for (i, sub) in model.submeshes.iter().enumerate() {
        let use_rig = slot != 0 && skin_forms.get(i).is_some();
        let mesh = if use_rig {
            skin_forms[i].clone()
        } else {
            stat_forms
                .get(i)
                .map(|(h, _)| h.clone())
                .unwrap_or_default()
        };
        let tag_slot = if use_rig { slot } else { 0 };
        let mut child = commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(part_mats[i].clone()),
            MeshTag(benilla_world::mesh_tag::spawn_tag(tag_slot, 1.0)),
            Transform::IDENTITY,
            layer.clone(),
            ChildOf(root),
            // Skinned or not, a tile's part is framed by construction; there is no cull to lose.
            NoFrustumCulling,
        ));
        if use_rig {
            child.insert(RigPart(root));
        }
        let entity = child.id();
        if let Some(anim) = &sub.alpha_anim {
            alpha_parts.push(AlphaPart {
                entity,
                anim: anim.clone(),
            });
        }
    }

    // The emitters: on their bone's anchor (the collapsed rig's demand-spawned entity), or the
    // root for a boneless file; clocked by the root's player like any hosted cloud; lit by the
    // tile's buffer; sized in the tile's pixels (`set_size_scale`, written per frame).
    let mut emitters = Vec::new();
    for em in &model.emitters {
        let (owner, pivot) = match pose.as_mut() {
            Some(p) => p
                .anchor_for(commands, root, em.def.bone)
                .map_or((root, [0.0; 3]), |joint| (joint, em.bone_pivot)),
            None => (root, [0.0; 3]),
        };
        let Some(e) = spawn_emitter(
            commands,
            em,
            Transform::IDENTITY,
            EmitterFrames {
                owner: Some((owner, pivot)),
                anchor: Some(root),
                alpha: None,
                light_node: None,
                on_owner_loss: OwnerLoss::Free,
            },
            EmitClock::Host(root),
        ) else {
            continue;
        };
        commands.entity(e).insert((
            layer.clone(),
            ChildOf(root),
            EffectLightOverride(light.clone()),
        ));
        emitters.push(e);
    }

    if let Some(p) = pose {
        commands.entity(root).insert((p, StageRig));
    }
    // `spawn_anim_host` is the world's placement recipe (variation re-rolls, the residency
    // window); a widget arms exactly what Lua asked and nothing else, so it is not used here —
    // named so nobody reaches for it.
    let _ = spawn_anim_host;
    Some(BuiltTile {
        clips,
        alpha_parts,
        uv_parts,
        emitters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packer keeps every cell inside the atlas, gutters between, and answers cells in
    /// request order — the composite's UVs and the roots' placements both index by it.
    #[test]
    fn the_packer_keeps_cells_apart_and_in_order() {
        let sizes: Vec<UVec2> = (0..40).map(|_| UVec2::new(72, 72)).collect();
        let (cells, atlas) = pack(&sizes);
        assert_eq!(cells.len(), 40);
        // 6 per row at 512 (74 px each with the gutter) is 36; the 40th needs the next size.
        assert_eq!(atlas, UVec2::splat(1024));
        for (i, a) in cells.iter().enumerate() {
            assert!(a.origin.x + a.size.x <= atlas.x && a.origin.y + a.size.y <= atlas.y);
            for b in &cells[i + 1..] {
                let apart = a.origin.x + a.size.x + GUTTER <= b.origin.x
                    || b.origin.x + b.size.x + GUTTER <= a.origin.x
                    || a.origin.y + a.size.y + GUTTER <= b.origin.y
                    || b.origin.y + b.size.y + GUTTER <= a.origin.y;
                assert!(apart, "cells {i} and another overlap or touch");
            }
        }
        // Growth: a wall of big tiles needs a bigger atlas; an impossible one is capped and
        // the tail simply does not fit.
        let big: Vec<UVec2> = (0..8).map(|_| UVec2::new(400, 400)).collect();
        let (cells, atlas) = pack(&big);
        assert_eq!(cells.len(), 8);
        assert_eq!(atlas, UVec2::splat(2048));
        let huge = vec![UVec2::new(5000, 10)];
        let (cells, atlas) = pack(&huge);
        assert!(cells.is_empty());
        assert_eq!(atlas, UVec2::splat(ATLAS_MAX));
    }

    /// The composite is a function of the bridge alone (decision 2023): a request with no cell
    /// draws nothing, a cell draws its request's rect at the request's key and alpha with the
    /// cell's texel window, and a cell whose request is gone draws nothing — no extract in the
    /// loop.
    #[test]
    fn the_composite_is_the_bridges_cells_over_their_requests() {
        // Two live handles off a real arena — the bridge is keyed by them, nothing more.
        let mut arena = benilla_ui::widget::WidgetArena::new();
        let handle = arena.create(benilla_ui::widget::FrameKind::Frame, None, None);
        let stray = arena.create(benilla_ui::widget::FrameKind::Frame, None, None);
        let req = TileRequest {
            path: r"Interface\Cooldown\UI-Cooldown-Indicator.mdx".into(),
            size_px: UVec2::new(63, 63),
            px_per_unit: 1680.75,
            pos_px_per_unit: 2742.62,
            star_px_per_unit: 2742.62,
            facing: 0.0,
            position: Vec3::ZERO,
            icon: None,
            rect: Rect::new(303.2, 767.1, 334.9, 798.8),
            z_key: 3_458_840_389_530_157_056,
            alpha: 0.5,
            clip: Some(Rect::new(0.0, 700.0, 400.0, 800.0)),
        };
        let mut bridge = UiModelTiles::default();
        bridge.requests.insert(handle, req.clone());
        assert!(
            composite_quads(&bridge).is_empty(),
            "no atlas, no cell: nothing"
        );
        bridge.atlas = Some(Handle::default());
        bridge.atlas_size = UVec2::splat(512);
        assert!(composite_quads(&bridge).is_empty(), "no cell yet: nothing");
        bridge.cells.insert(
            handle,
            Cell {
                origin: UVec2::new(67, 2),
                size: UVec2::new(63, 63),
            },
        );
        // A cell the reaper's request drop orphaned: nothing to place it at.
        bridge.cells.insert(
            stray,
            Cell {
                origin: UVec2::new(2, 2),
                size: UVec2::new(63, 63),
            },
        );
        let quads = composite_quads(&bridge);
        assert_eq!(quads.len(), 1, "one cell with a request draws once");
        let q = &quads[0];
        assert_eq!(q.rect, req.rect);
        assert_eq!(q.z_key, req.z_key);
        assert_eq!(q.color, [1.0, 1.0, 1.0, 0.5], "the frame's own alpha");
        assert!(q.premultiplied);
        assert_eq!(q.clip, req.clip);
        assert!(q.texture.is_some());
        let [tl, _, br, _] = q.uv.corners;
        assert!((tl[0] - 67.0 / 512.0).abs() < 1e-6 && (tl[1] - 2.0 / 512.0).abs() < 1e-6);
        assert!((br[0] - 130.0 / 512.0).abs() < 1e-6 && (br[1] - 65.0 / 512.0).abs() < 1e-6);
    }

    /// The axis fix is a proper rotation that puts WoW `+X` right, `+Y` up, `+Z` toward the
    /// viewer.
    #[test]
    fn the_axis_fix_is_the_ortho_legs_frame() {
        let q = wow_to_screen();
        let wow = |v: [f32; 3]| q * benilla_assets::coords::wow_to_bevy(v);
        assert!((wow([1.0, 0.0, 0.0]) - Vec3::X).length() < 1e-6);
        assert!((wow([0.0, 1.0, 0.0]) - Vec3::Y).length() < 1e-6);
        assert!((wow([0.0, 0.0, 1.0]) - Vec3::Z).length() < 1e-6);
        // A facing of +90° about WoW +Z turns +X into +Y on screen (CCW).
        let turned = q
            * Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
            * benilla_assets::coords::wow_to_bevy([1.0, 0.0, 0.0]);
        assert!((turned - Vec3::Y).length() < 1e-5, "{turned}");
    }
}
