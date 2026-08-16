//! The player-UI quad pass (decision 0068 §2): a custom sorted-quad renderer for the WoW-engine UI,
//! **not** `bevy_ui`'s flexbox/hierarchy z-model — probe B (2026-07-02) measured `bevy_ui` fighting the
//! WoW `(stratum, level, layer, sublayer, decl)` total order and chose a dedicated pass instead. This
//! module is the render **substrate** only: [`UiQuad`]/[`UiQuads`] is the data contract the future
//! widget arena (frames, `FontString`s, the anchor resolver) builds against — nothing here knows about
//! frames, strata, or Lua. A caller flattens its own ordering into `z_key: u64` (a WoW frame's
//! `(stratum, level, layer, sublayer, decl)` tuple packs into one `u64` losslessly) and this pass just
//! draws the resulting flat list back-to-front.
//!
//! ## Camera / compositing order (decision 0025)
//! Three cameras share the window, ordered low→high: the 3D world camera (order 0, untouched) → this
//! module's [`PlayerUiCamera`] (order 1) → the egui dev-overlay camera (bumped from 1 to 2 in
//! `debug_panel::spawn_egui_camera` — the one sanctioned edit outside this file). 0025 established
//! "dev overlays composite over a full-screen world"; this pass slots the *player* UI into that
//! ordering exactly where 0025 already reserved it ("where player-UI arbitration will live later") —
//! **dev overlays stay on top of the player UI, which stays on top of the world.** The camera renders
//! nothing from the 3D world (its own [`RenderLayers`] layer, disjoint from the world camera's default
//! layer 0) and composites over whatever the world camera already painted (`CameraOutputMode::Write`
//! with alpha blending, no clear — same pattern as the egui overlay camera).
//!
//! ## Colour space: the UI gamma composite lane (decision 0254)
//! The reference draws its whole UI through the fixed-function device into an 8-bit backbuffer, so
//! every UI multiply and every UI blend is arithmetic on **gamma bytes**, clamped at each write.
//! This pass reproduces that: [`UiQuad::color`] rides through unconverted, `ui_quad.wgsl` puts its
//! sampled texel back into byte space and premultiplies there, and the hardware blend composes in
//! gamma. [`crate::ui_gamma`] then decodes the finished image to linear exactly once, so the sRGB
//! target's write re-encodes it to the client's byte. This is the UI sibling of decision 0161's
//! world lane, whose one decode lives in the FFXGlow combine.
//!
//! Getting this wrong is not a subtlety: composited in linear, `alphaMode="ADD"` lands at roughly a
//! quarter of the reference's lift (sRGB's power law is superadditive, so a linear add is always
//! weaker than a byte add) — the near-invisible gossip/quest/trainer row highlight that forced 0254.
//!
//! ## Rendering approach
//! Quads are stable-sorted by `z_key` ascending (the WoW total order: lower key = further back). CPU
//! clip (an axis-aligned intersection against [`UiQuad::clip`], reprojecting UVs proportionally) is
//! applied per quad; a quad clipped to nothing emits no geometry. The sorted, clipped list is then
//! split into **contiguous runs of matching texture identity** (texture-less quads use a shared 1×1
//! white image, so they participate in the same run-splitting as textured ones) — each run becomes one
//! `Mesh2d`/`ColorMaterial` entity. Painter's order within a run comes from vertex/index submission
//! order in the single draw call (straight-alpha blending composites strictly in submission order, no
//! depth test); painter's order **across** runs comes from each run's entity `Transform.z` — Bevy's 2D
//! transparent phase sorts draws by ascending world-space z (`bevy_sprite_render`'s
//! `mesh2d::material::queue_material2d_meshes`), so runs are placed at increasing z in the same order
//! they appear in the sorted quad list. This is what makes the *contiguous-run* choice load-bearing
//! rather than a pure optimization: batching is scoped to a run, not deduped globally by texture, so
//! the total order holds **even when two quads share a texture but something else is sandwiched
//! between them in z_key** — that in-between quad still forces a run break, so the shared texture ends
//! up in two (or more) separate draw calls rather than one. The batching consequence: UI content that
//! interleaves textures frequently in z-order (e.g. icon, text, icon, text, …) pays one draw call per
//! transition instead of the two-call minimum a global per-texture dedup would give — correctness over
//! draw-call count. A texture atlas is the standard fix (turns "different texture" into "different UV
//! rect on one atlas", collapsing runs) and is a natural extension once the widget arena needs it.
//!
//! ## Explicitly out of scope (v1)
//! - **Real scissor rects.** [`UiQuad::clip`] is a CPU stand-in (rebuild-time geometry clip); a
//!   `ScrollFrame`-driven GPU scissor rect (per 0068 §2, "per-ScrollFrame scissor rects") is a later
//!   render-pass change, not a data-contract change — `clip` can stay as an escape hatch for the rare
//!   non-rectangular-region case even after real scissor lands.
//! - **Text.** `FontString`s render through `cosmic-text` per 0068 §2; nothing here rasterizes glyphs.
//! - **Extraction from a widget arena.** Nothing yet *produces* [`UiQuad`]s from frames/anchors/strata —
//!   that's the widget-arena milestone this pass is built ahead of. The `dev`-gated demo feeder below
//!   fills [`UiQuads`] directly to prove the pass in isolation.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::CameraOutputMode;
use bevy::image::Image;
use bevy::math::Rect;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d,
    TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{Material2d, Material2dKey, Material2dPlugin};
use bevy::window::PrimaryWindow;

use benilla_assets::{AssetSet, WorldAssets};

/// Per-corner UVs for a quad: one explicit `(u,v)` sample per **screen** corner, in the
/// [`Run::push_quad`] winding — `[top-left, top-right, bottom-right, bottom-left]`. Deliberately four
/// independent pairs, **not** a `(min,max)` rect, for two reasons that a 2-corner rect can't serve:
///
/// 1. **Mirror/flip preservation.** A WoW `<TexCoords left="1.0" right="0.09375">` (the PlayerFrame
///    ring's horizontal mirror) has `left > right` on purpose; a normalizing `Rect` would silently
///    un-mirror it. Corners carry `TL.u > TR.u` (and top>bottom for a vertical flip) intact.
/// 2. **Rotation.** A backdrop TOP/BOTTOM edge maps atlas-**u** to screen-**Y** and atlas-v to
///    screen-X (the slice is turned 90°). That's not expressible as `u varies only in x, v only in y`
///    — it needs a distinct `(u,v)` at each corner, which this carries and [`clip_quad`] bilinearly
///    reprojects.
///
/// This subsumes the old `{u0,v0,u1,v1}` two-corner form (`ui: mirrored TexCoords`, commit 87c97d7).
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct UvRect {
    /// `[top-left, top-right, bottom-right, bottom-left]`, each `[u, v]` — the `push_quad` winding.
    pub corners: [[f32; 2]; 4],
}

impl UvRect {
    /// The full texture, no crop.
    pub const FULL: Self = Self {
        corners: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    };

    /// From a WoW `[left, right, top, bottom]` `<TexCoords>` tuple: an axis-aligned crop
    /// (`TL=(left,top)`, `TR=(right,top)`, `BR=(right,bottom)`, `BL=(left,bottom)`). No
    /// normalization — a mirrored (`left>right`) or flipped (`top>bottom`) tuple keeps its
    /// orientation, which is the whole point of this type.
    pub fn from_tex_coords([left, right, top, bottom]: [f32; 4]) -> Self {
        Self {
            corners: [[left, top], [right, top], [right, bottom], [left, bottom]],
        }
    }

    /// From explicit per-corner UVs in the `push_quad` winding (`[TL, TR, BR, BL]`) — the backdrop
    /// edge/corner pieces, whose rotated slices are not an axis-aligned crop.
    pub fn from_corners(corners: [[f32; 2]; 4]) -> Self {
        Self { corners }
    }

    /// From an already-normalized [`Rect`] (e.g. a glyph atlas cell, which is never mirrored):
    /// `min→TL`, `max→BR`.
    pub fn from_rect(r: Rect) -> Self {
        Self::from_tex_coords([r.min.x, r.max.x, r.min.y, r.max.y])
    }
}

/// One resolved, screen-space quad — the render substrate's entire input. A future widget arena
/// (frames/anchors/strata) flattens into this; nothing here is frame-shaped.
#[derive(Clone, PartialEq)]
pub(crate) struct UiQuad {
    /// Screen pixels, **y-down**, already anchor-resolved (no relative/parent math left to do).
    pub rect: Rect,
    /// Total paint order — stable-sorted ascending, so **higher draws later (on top)**. A WoW frame's
    /// `(stratum, level, layer, sublayer, decl)` tuple is expected to pack into this losslessly.
    pub z_key: u64,
    /// `None` draws as flat-shaded (via a shared 1×1 white texture — see the module doc's batching
    /// note for why texture-less quads still participate in texture-identity run-splitting).
    pub texture: Option<Handle<Image>>,
    /// The texture sub-rect (atlas cell / crop) as raw corners — see [`UvRect`] for why this is not a
    /// [`Rect`] (mirror preservation). Default: the full texture ([`UvRect::FULL`]).
    pub uv: UvRect,
    /// Straight-alpha (non-premultiplied) vertex color, multiplied against the sampled texel.
    /// **Client-space sRGB** — the raw FrameXML/Lua value (the client's FFP wrote these straight to
    /// its 8-bit backbuffer). It reaches the shader UNCONVERTED (decision 0254): the quad pass
    /// composites in gamma bytes, so this multiply *is* the client's gamma-space `tint × texel`.
    /// Producers never pre-convert.
    pub color: [f32; 4],
    /// WoW `ADD` blend (EGxBlend 3, `glBlendFunc(GL_SRC_ALPHA, GL_ONE)`: highlights, glows) instead
    /// of straight alpha. Splits the batch run — see [`ui_quad.wgsl`]'s premultiplied trick for how
    /// one pipeline serves both.
    pub additive: bool,
    /// Mask to the quad's inscribed circle (the live unit portrait — the shader-side twin of the
    /// real client's round bake stencil). Splits the run like `additive`; in practice a portrait's
    /// render-target texture is unique to it, so no batching is actually lost.
    pub circular: bool,
    /// Draw the sampled texel as its **luminance** — `Texture:SetDesaturated(1)`, the greyed-out
    /// icon every disabled affordance in the reference wears (decision 1327). Folded in the
    /// client's GAMMA byte space and applied BEFORE the vertex-colour multiply, so the reference's
    /// own `SetItemButtonDesaturated(button, 1, 0.65, 0.65, 0.65)` lands as greyscale *and* dim.
    /// Splits the run like `additive`/`circular`.
    pub desaturated: bool,
    /// CPU-clip stand-in for a real scissor rect (see the module doc). `None` = unclipped.
    pub clip: Option<Rect>,
    /// Rotate the quad's corners by this many radians **clockwise on screen** about the rect's
    /// center, applied at mesh build (the minimap player arrow's facing — decision 0203). The UVs
    /// ride their corners, so the art rotates with the geometry. Composes with `clip` by clipping
    /// FIRST in the unrotated frame (no current producer sets both; the arrow is never clipped).
    pub rotation: f32,
    /// Confine the quad to a screen-anchored alpha mask (the minimap's `MinimapMask.blp` circle —
    /// decision 0203): the fragment's alpha is multiplied by the mask texture's **alpha** channel
    /// (`MinimapMask.blp` is DXT3: white color, the circle ramp in its 8-bit alpha), sampled
    /// where the fragment sits within [`UiQuadMask::rect`]; fragments outside the rect drop.
    /// Screen-anchored — NOT quad UV space — so a world-anchored tile quad panning under a fixed
    /// circular window masks correctly. Splits the run like `additive`/`circular`.
    pub mask: Option<UiQuadMask>,
    /// Explicit screen-space corners (y-down px, **fan order from corner 0**: the index pattern is
    /// `(0,1,2)(0,2,3)`, so any convex 4-gon with corner 0 as the fan apex renders exactly — a
    /// triangle repeats its last vertex). When `Some`, `rect`/`rotation`/`clip` are bypassed
    /// (`rect` should still bound the corners for the probe/debug view). The cooldown pie's
    /// partial-quadrant wedge is the producer (decision 0137 phase 4) — exact geometry computed at
    /// extract, no scissor interplay to get wrong.
    pub corners: Option<[Vec2; 4]>,
}

/// A screen-anchored alpha mask over a [`UiQuad`] — see [`UiQuad::mask`].
#[derive(Clone, PartialEq)]
pub(crate) struct UiQuadMask {
    /// The mask art (its ALPHA channel is the coverage — `MinimapMask.blp` is DXT3 with the
    /// circle authored in alpha over a white color plane).
    pub texture: Handle<Image>,
    /// Where the mask spans on screen, in the same y-down logical px as [`UiQuad::rect`]
    /// (the minimap widget's rect). The mask samples 0..1 across this span.
    pub rect: Rect,
}

impl Default for UiQuad {
    fn default() -> Self {
        Self {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            z_key: 0,
            texture: None,
            uv: UvRect::FULL,
            color: [1.0, 1.0, 1.0, 1.0],
            additive: false,
            circular: false,
            desaturated: false,
            clip: None,
            rotation: 0.0,
            mask: None,
            corners: None,
        }
    }
}

/// The pass's one input resource, two lanes with distinct change protocols:
///
/// - [`Self::quads`] — the BASE lane, owned by a wholesale producer (the widget arena's extract;
///   the dev demo feeder): replaced in full, and the producer sets [`Self::dirty`] only when the
///   new content differs.
/// - [`Self::overlays`] — the APPEND lane ([`UiQuadAppend`]: the minimap fill, V-plates, combat
///   text): cleared at the top of the append window ([`clear_ui_overlays`]) and re-emitted every
///   frame; [`rebuild_ui_mesh`] itself diffs it against last frame's. Appenders never touch
///   `dirty` — when the two lanes lived in one Vec, the extract's change gate compared its fresh
///   output against a Vec still carrying last frame's appends, so it could never hold and the
///   whole UI re-batched every frame (the 0365 live-city churn).
#[derive(Resource, Default)]
pub(crate) struct UiQuads {
    pub quads: Vec<UiQuad>,
    /// The append lane — see the struct doc. Compared by the rebuild, never flagged.
    pub overlays: Vec<UiQuad>,
    /// The append lane as of the last rebuild — [`rebuild_ui_mesh`]'s change gate for it.
    last_overlays: Vec<UiQuad>,
    /// Set by the BASE-lane producer after replacing `quads` with different content; cleared by
    /// [`rebuild_ui_mesh`] once it has rebuilt the mesh batches.
    pub dirty: bool,
}

/// Clear the append lane at the top of the [`UiQuadAppend`] window — every appender re-emits its
/// frame's quads after this; [`rebuild_ui_mesh`] then diffs the lane to decide if anything changed.
fn clear_ui_overlays(mut quads: ResMut<UiQuads>) {
    quads.overlays.clear();
}

/// The dedicated render layer this pass's camera + mesh entities share — disjoint from the world
/// camera's default layer 0 (so neither camera draws the other's content) and distinct from the egui
/// overlay camera's `RenderLayers::none()` (egui paints via its own render node, not `Mesh2d`s, so it
/// doesn't need a mesh-visible layer at all).
const UI_RENDER_LAYER: usize = 1;

fn ui_render_layers() -> RenderLayers {
    RenderLayers::layer(UI_RENDER_LAYER)
}

/// Camera order for the player-UI pass: above the (order-0) world camera, below the egui dev-overlay
/// camera (bumped to order 2 in `debug_panel::spawn_egui_camera`). See the module doc's arbitration note.
const UI_CAMERA_ORDER: isize = 1;

/// Marker on the player-UI camera.
#[derive(Component)]
struct PlayerUiCamera;

/// Marker on each rebuilt batch mesh entity, so [`rebuild_ui_mesh`] can despawn last frame's batches
/// before spawning the new ones.
#[derive(Component)]
struct UiQuadBatch;

/// The shared 1×1 opaque-white texture flat-shaded/texture-less quads sample (see the module doc).
#[derive(Resource)]
struct UiWhiteTexture(Handle<Image>);

/// The quad material: a texture + the blend-mode flag, drawn through one PREMULTIPLIED-alpha
/// pipeline (`ui_quad.wgsl`) so straight-alpha and WoW-ADD quads share the pipeline and differ
/// only per-material. Vertex colors carry the per-quad tint.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct UiQuadMaterial {
    #[uniform(0)]
    additive: u32,
    #[texture(1)]
    #[sampler(2)]
    texture: Option<Handle<Image>>,
    /// Mask to the inscribed circle (live unit portraits) — see [`UiQuad::circular`].
    #[uniform(3)]
    circular: u32,
    /// Fold the texel to luminance — see [`UiQuad::desaturated`].
    #[uniform(7)]
    desaturate: u32,
    /// The screen-anchored mask span in **physical framebuffer px** (`min.xy, max.xy` — the
    /// fragment shader compares `@builtin(position)`, which is physical): [`UiQuadMask::rect`]
    /// scaled by the window's scale factor at mesh build. `z <= x` (degenerate) disables masking.
    #[uniform(4)]
    mask_rect: Vec4,
    /// The mask art — see [`UiQuadMask`]. `None` binds the fallback image (and `mask_rect` is
    /// degenerate, so the shader never reads it).
    #[texture(5)]
    #[sampler(6)]
    mask: Option<Handle<Image>>,
}

impl Material2d for UiQuadMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://benilla_app/shaders/ui_quad.wgsl".into()
    }

    fn alpha_mode(&self) -> bevy::sprite_render::AlphaMode2d {
        // Transparent phase (back-to-front by mesh z — the run order). The actual blend state is
        // forced premultiplied in `specialize`.
        bevy::sprite_render::AlphaMode2d::Blend
    }

    fn specialize(
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        _layout: &bevy::mesh::MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), bevy::render::render_resource::SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(target) = fragment.targets.first_mut().and_then(|t| t.as_mut()) {
                // `(One, OneMinusSrcAlpha)` over GAMMA values (decision 0254): the shader hands us
                // `(rgb·a, a)` for BLEND and `(rgb·a, 0)` for ADD, so this one state reproduces
                // EGxBlend 2 and EGxBlend 3 exactly, clamped at each write like the reference's
                // 8-bit backbuffer.
                let premultiplied = BlendComponent {
                    src_factor: BlendFactor::One,
                    dst_factor: BlendFactor::OneMinusSrcAlpha,
                    operation: BlendOperation::Add,
                };
                target.blend = Some(BlendState {
                    color: premultiplied,
                    alpha: premultiplied,
                });
            }
        }
        Ok(())
    }
}

/// Producers that **append** to [`UiQuads`] after the script extract replaced it — the
/// world-anchored text (combat numbers, nameplates). The set lives in **Update, after
/// [`benilla_world::schedule::WorldStage::Input`]** (itself after the script extract's `UiInput`): the
/// camera controller has written this frame's camera `Transform` by then, and the producers
/// project through THAT fresh Transform (`GlobalTransform::from` — the world camera is a root
/// entity), never the stale propagated `GlobalTransform` — the fix for text dragging one frame
/// behind the render when strafing. The set MUST stay in Update: [`rebuild_ui_mesh`] respawns its
/// `Mesh2d` batches, and bevy_sprite_render cannot specialize meshes spawned in PostUpdate
/// ("missing specialization tick" — the whole UI vanishes; a PostUpdate placement was tried and
/// reverted, caught live by the director). The posed BONE transforms the producers read are last
/// frame's — sub-frame head motion, imperceptible next to camera pan.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UiQuadAppend;

/// Project a world point for a world-anchored overlay, **with the reference projector's own accept
/// verdict** — `None` means "this one does not draw", and the caller must honour it.
///
/// `0x483ee0` returns a boolean (wow-re `object-layer/scratch/nameplate-offscreen-cull.md`,
/// §5-VERIFIED 2026-08-15). It rejects a point behind the near plane (`483f7a`) **and** a point
/// outside the viewport — the four tests at `484075`/`484086`/`484096`/`4840a6`, against the
/// **WorldFrame**'s own region (`[0xb4b2bc]`, mirrored ÷G44/÷G48 at `0x483970`), so the accept
/// region is exactly the viewport, inclusive. Both plate and worldtext callers **destroy** the
/// thing they were about to place when it comes back false.
///
/// **The trap this exists to remove:** on the viewport rejections the projector has *already
/// written* the out-param (`484065`/`484067`), so a caller that ignores the verdict reads a
/// perfectly plausible off-screen coordinate and sails on — which is exactly how benilla ended up
/// clamping the plates of units nobody can see onto the screen border (1341). Returning `Option`
/// makes the verdict unignorable at the call site.
///
/// The chat bubble deliberately does **not** call this: the reference's bubble seat is the one
/// caller of the three that ignores the boolean, so a bubble simply slides off the edge with its
/// speaker. Bevy's own `Err` covers only the behind-camera half, which is why the viewport test
/// has to be here rather than left to it.
pub(crate) fn project_overlay(
    cam: &Camera,
    cam_tf: &GlobalTransform,
    world: Vec3,
    viewport: Vec2,
) -> Option<Vec2> {
    // Bevy's `Err` is the near-plane half (`483f7a`); `accepts` is the viewport half.
    let p = cam.world_to_viewport(cam_tf, world).ok()?;
    accepts(p, viewport).then_some(p)
}

/// The accept REGION: exactly the viewport, **inclusive**. `[0xb4b2bc]` is the WorldFrame, and its
/// region rect is mirrored ÷G44/÷G48 into the compare fields (`0x483970`), so the aspect factors
/// cancel exactly and the test is against the raw screen box.
fn accepts(p: Vec2, viewport: Vec2) -> bool {
    (0.0..=viewport.x).contains(&p.x) && (0.0..=viewport.y).contains(&p.y)
}

/// Owns the player-UI camera + the per-frame quad→mesh rebuild. See the module doc for the
/// camera-order arbitration and the batching/ordering approach.
pub(crate) struct PlayerUiPlugin;

impl Plugin for PlayerUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiQuads>()
            .add_plugins((
                Material2dPlugin::<UiQuadMaterial>::default(),
                // Owned here, not in main.rs: the lane's decode is not optional (see its doc).
                crate::ui_gamma::UiGammaPlugin,
            ))
            .add_systems(Startup, (spawn_ui_camera, init_white_texture))
            .configure_sets(
                Update,
                UiQuadAppend.after(benilla_world::schedule::WorldStage::Input),
            )
            .add_systems(Update, clear_ui_overlays.before(UiQuadAppend))
            .add_systems(Update, rebuild_ui_mesh.after(UiQuadAppend));

        // Dev-only demo feeder (mirrors the repo's env-var dev-instrument gating — e.g. `$WOW_CAPTURE`,
        // `$WOW_TILE_RADIUS` — since the compile-time `dev` cargo feature decision 0026 sets as the
        // eventual seam isn't built yet; see `demo_enabled`'s doc comment). Runs once at Startup, after
        // the asset chain opens so it can load a real BLP through `WorldAssets::sprite_texture`.
        if demo_enabled() {
            app.add_systems(Startup, seed_demo_quads.after(AssetSet::Open));
        }
    }
}

/// `$WOW_UI_DEMO=1` fills [`UiQuads`] once at startup with synthetic content proving the sort, the
/// texture-batching/ordering story, and the CPU clip. Same env-var-gated-instrument idiom as
/// `capture.rs`'s `$WOW_CAPTURE` and `assets::open_world_assets`'s `$WOW_TILE_RADIUS`/`$WOW_TEX_TILES`
/// — decision 0026 phase 1 (a compile-time `dev` cargo feature) is recorded as the target seam but not
/// yet built (0026 §4: "phased, not built now"), so today's instrument-gating convention across the
/// codebase is a runtime env-var check, not `#[cfg(feature = "dev")]`.
fn demo_enabled() -> bool {
    std::env::var("WOW_UI_DEMO").as_deref() == Ok("1")
}

/// A full-window overlay camera that composites the player-UI quad pass above the 3D world and below
/// the egui dev overlays. Mirrors `debug_panel::spawn_egui_camera`'s pattern (own render layer, higher
/// order, alpha-blend `Write` output, no clear) — see the module doc for the order arbitration.
fn spawn_ui_camera(mut commands: Commands) {
    commands.spawn((
        PlayerUiCamera,
        Camera2d,
        // The gamma composite lane's mandatory decode (decision 0254) — without it the UI presents
        // ~2.2× bright, since the quad pass leaves gamma values in the target.
        crate::ui_gamma::UiGammaLane,
        // Every Bevy UI tree renders HERE (decision 0541) — the glue screens and the loading screen.
        // Without the marker, Bevy UI picks the highest-order camera targeting the window, which is
        // the egui dev overlay (order 2): the glue screens rode the dev camera, outside the gamma
        // lane, and composited in linear — the washed-out login boxes the director caught. Bevy UI
        // and egui never contend for it (egui paints through its own render node, not UI nodes).
        bevy::ui::IsDefaultUiCamera,
        ui_render_layers(),
        Camera {
            order: UI_CAMERA_ORDER,
            output_mode: CameraOutputMode::Write {
                // PREMULTIPLIED, not `ALPHA_BLENDING`: `ui_quad.wgsl` writes premultiplied colour
                // (rgb·a), so a `SrcAlpha` factor here would weight it by alpha a SECOND time —
                // translucent UI over the world came out `rgb·a²`, and a pure-additive quad (a = 0)
                // over the world was multiplied clean away. Opaque panels (a = 1) hid it.
                blend_state: Some(
                    bevy::render::render_resource::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                ),
                clear_color: ClearColorConfig::None,
            },
            // An overlay must composite ONLY its own pixels. `ClearColorConfig::None` made this
            // camera depend on `MsaaWriteback::Auto`, which (for any non-first camera on a target)
            // COPIES the world camera's already-final output into this camera's MSAA texture and
            // re-emits it through this camera's SDR blit+blend — a whole-image re-encode round-trip
            // that tinted the world (a fixed blue film on WMO surfaces, the regression the director
            // caught live; it never reproduced in captures). Clear to transparent instead and turn
            // writeback off: this camera never touches the world image at all.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            msaa_writeback: bevy::camera::MsaaWriteback::Off,
            ..default()
        },
    ));
}

/// Build the shared 1×1 white texture texture-less quads sample (see the module doc's batching note on
/// why they still go through a real texture bind rather than a "no texture" material variant).
fn init_white_texture(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = Image::new_fill(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    commands.insert_resource(UiWhiteTexture(images.add(image)));
}

/// Clip `rect`/`uv` to `clip`, reprojecting the UV sub-rect proportionally to the clipped edges (an
/// axis-aligned CPU scissor stand-in — see the module doc). Returns `None` if the clip leaves nothing
/// to draw (empty intersection, or a degenerate zero-size source rect).
fn clip_quad(rect: Rect, uv: UvRect, clip: Rect) -> Option<(Rect, UvRect)> {
    let clipped = rect.intersect(clip);
    if clipped.is_empty() {
        return None;
    }
    let (w, h) = (rect.width(), rect.height());
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // Fraction along each axis the clipped edges sit at within the original rect, then reproject the
    // UV corners by the same fractions — this keeps the texture mapping correct on the visible
    // remainder instead of carrying the original (now-too-large) UV rect through unclipped. The
    // fractions come from the (always-normalized) screen rect. The reprojection is a **bilinear**
    // sample of the four corner UVs at (fx, fy): for an axis-aligned crop it reduces to the old
    // separable `u0→u1`/`v0→v1` lerp (so a mirrored UV stays mirrored), and for a rotated backdrop
    // edge (u tied to screen-Y) it correctly carries the rotation through the clip.
    let t_min_x = (clipped.min.x - rect.min.x) / w;
    let t_max_x = (clipped.max.x - rect.min.x) / w;
    let t_min_y = (clipped.min.y - rect.min.y) / h;
    let t_max_y = (clipped.max.y - rect.min.y) / h;
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let [tl, tr, br, bl] = uv.corners;
    let bilerp = |fx: f32, fy: f32| -> [f32; 2] {
        let top = [lerp(tl[0], tr[0], fx), lerp(tl[1], tr[1], fx)];
        let bot = [lerp(bl[0], br[0], fx), lerp(bl[1], br[1], fx)];
        [lerp(top[0], bot[0], fy), lerp(top[1], bot[1], fy)]
    };
    let new_uv = UvRect::from_corners([
        bilerp(t_min_x, t_min_y), // top-left
        bilerp(t_max_x, t_min_y), // top-right
        bilerp(t_max_x, t_max_y), // bottom-right
        bilerp(t_min_x, t_max_y), // bottom-left
    ]);
    Some((clipped, new_uv))
}

/// One texture-identity run: contiguous quads (in sorted z_key order) sharing the same texture handle.
/// See the module doc for why runs are contiguous rather than globally deduped by texture.
struct Run {
    texture: Handle<Image>,
    additive: bool,
    circular: bool,
    desaturated: bool,
    mask: Option<UiQuadMask>,
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Run {
    fn new(
        texture: Handle<Image>,
        additive: bool,
        circular: bool,
        desaturated: bool,
        mask: Option<UiQuadMask>,
    ) -> Self {
        Self {
            texture,
            additive,
            circular,
            desaturated,
            mask,
            positions: Vec::new(),
            uvs: Vec::new(),
            colors: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// Append one screen-space quad (already clipped) as two triangles, in a fixed
    /// top-left/top-right/bottom-right/bottom-left winding — cross-quad order within the run is
    /// entirely a function of *when* this is called (append order = paint order; see the module doc).
    /// `rotation` spins the corners clockwise about the rect center in screen space (see
    /// [`UiQuad::rotation`]) — the UVs stay with their corners, so the art rotates in place.
    /// `to_world` maps a y-down screen point to the camera's y-up, origin-centred world space.
    /// The client-space sRGB `color` rides through unconverted — the quad pass composites in gamma
    /// bytes (see [`UiQuad::color`] and decision 0254).
    fn push_quad(
        &mut self,
        rect: Rect,
        uv: UvRect,
        color: [f32; 4],
        rotation: f32,
        to_world: impl Fn(Vec2) -> Vec2,
    ) {
        let [tl, tr, br, bl] = uv.corners;
        let mut corners = [
            (Vec2::new(rect.min.x, rect.min.y), Vec2::from(tl)), // top-left
            (Vec2::new(rect.max.x, rect.min.y), Vec2::from(tr)), // top-right
            (Vec2::new(rect.max.x, rect.max.y), Vec2::from(br)), // bottom-right
            (Vec2::new(rect.min.x, rect.max.y), Vec2::from(bl)), // bottom-left
        ];
        if rotation != 0.0 {
            // Clockwise on a y-down screen = the standard CCW rotation matrix in y-down coords.
            let (sin, cos) = rotation.sin_cos();
            let center = (rect.min + rect.max) * 0.5;
            for (p, _) in &mut corners {
                let d = *p - center;
                *p = center + Vec2::new(d.x * cos - d.y * sin, d.x * sin + d.y * cos);
            }
        }
        self.push_corners(corners, color, to_world);
    }

    /// Append four explicit screen-space corners (fan order from corner 0 — [`UiQuad::corners`])
    /// with their UVs, as the standard `(0,1,2)(0,2,3)` fan.
    fn push_corners(
        &mut self,
        corners: [(Vec2, Vec2); 4],
        color: [f32; 4],
        to_world: impl Fn(Vec2) -> Vec2,
    ) {
        let base = self.positions.len() as u32;
        for (p, t) in corners {
            let w = to_world(p);
            self.positions.push([w.x, w.y, 0.0]);
            self.uvs.push([t.x, t.y]);
            self.colors.push(color);
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The rebuild's reused GPU-facing state, kept across frames so a rebuild pays for what CHANGED,
/// never for existence (the 0353 demand-price law, UI lane): batch entities and their mesh assets
/// are pools reused by run index (geometry rewritten in place — no allocator or archetype churn),
/// and materials are cached by their full identity key (same texture/blend/mask ⇒ the same
/// material asset forever, so `prepare_assets` re-prepares nothing on a steady frame). Before
/// this, every rebuild despawned every batch and allocated fresh meshes + materials — ~9 ms/frame
/// of render-side churn in a live city (0365).
#[derive(Default)]
struct BatchPools {
    entities: Vec<Entity>,
    meshes: Vec<Handle<Mesh>>,
    materials: std::collections::HashMap<MatKey, Handle<UiQuadMaterial>>,
}

/// A material's full identity: texture, blend/shape flags, mask texture + mask rect (as bits, so
/// NaN/-0.0 can't split cache entries byte-equal materials would share).
type MatKey = (
    AssetId<Image>,
    bool,
    bool,
    bool,
    Option<AssetId<Image>>,
    [u32; 4],
);

/// Retire every pooled batch entity (blank frame / no drawable content). Mesh handles and the
/// material cache stay — assets referenced only by the pool cost nothing to keep and come back
/// for free.
fn retire_batches(pools: &mut BatchPools, commands: &mut Commands) {
    for entity in pools.entities.drain(..) {
        commands.entity(entity).despawn();
    }
}

/// Per frame: when the BASE lane flagged a change ([`UiQuads::dirty`]) or the APPEND lane's
/// content differs from last frame's, stable-sort both lanes by `z_key`, CPU-clip, split into
/// texture-identity runs, and write the runs into the pooled batches ([`BatchPools`]). See the
/// module doc for the full approach (painter's order within/across runs) and its batching
/// consequence.
fn rebuild_ui_mesh(
    mut quads: ResMut<UiQuads>,
    mut commands: Commands,
    // The two asset stores the rebuild writes, bundled (clippy's argument ceiling).
    mut stores: (ResMut<Assets<Mesh>>, ResMut<Assets<UiQuadMaterial>>),
    mut pools: Local<BatchPools>,
    white: Option<Res<UiWhiteTexture>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    hidden: Res<crate::ui_hide::UiHidden>,
) {
    let (meshes, materials) = (&mut stores.0, &mut stores.1);
    let q = quads.as_mut();
    // TOGGLEUI hides at the *draw*, not at the producers: both lanes keep filling, so the UI comes
    // back exactly as it was (see [`crate::ui_hide::UiHidden`]). Retire the batches on the toggle's
    // down edge; on the way back up, force one full rebuild — while dark we swallow each frame's
    // change flag and keep the append-lane mirror current, so neither lane can hand the rebuild a
    // stale "nothing changed" the moment the UI returns. The edge is the resource's own change tick
    // (`UiHidden` is written only by the binding and the world-exit reset), not a `Local` mirror.
    if hidden.0 {
        if hidden.is_changed() {
            retire_batches(&mut pools, &mut commands);
        }
        q.dirty = false;
        q.last_overlays.clone_from(&q.overlays);
        return;
    }
    if hidden.is_changed() {
        q.dirty = true;
    }
    if !q.dirty && q.overlays == q.last_overlays {
        return;
    }
    q.dirty = false;
    q.last_overlays.clone_from(&q.overlays);

    let (Ok(window), Some(white)) = (windows.single(), white) else {
        // No window to size against, or the white-texture Startup system hasn't run yet — nothing to
        // draw this frame; the resource stays dirty-cleared, matching "we tried and there was nothing
        // to show" rather than spinning forever.
        retire_batches(&mut pools, &mut commands);
        return;
    };
    if q.quads.is_empty() && q.overlays.is_empty() {
        retire_batches(&mut pools, &mut commands);
        return;
    }

    // Screen px (y-down, origin top-left) → the 2D camera's world space (y-up, origin screen-centre) —
    // matches the default `OrthographicProjection` (`ScalingMode::WindowSize`, scale 1.0 ⇒ 1 world
    // unit = 1 logical px) the camera spawns with.
    let (half_w, half_h) = (window.width() * 0.5, window.height() * 0.5);
    let to_world = move |p: Vec2| Vec2::new(p.x - half_w, half_h - p.y);

    // Stable sort by z_key: the WoW-style total order. Stable so equal-z_key quads keep the producer's
    // original relative order (their own decl-order tiebreak, if any). Base lane first, append lane
    // after — the same relative order the one-Vec era produced.
    let mut sorted: Vec<&UiQuad> = q.quads.iter().chain(q.overlays.iter()).collect();
    sorted.sort_by_key(|q| q.z_key);

    // Geometry probe (`WOW_UI_PROBE=1`): dump each textured quad's screen rect once — the
    // capture-harness companion for diagnosing extracted-vs-rendered geometry by data instead of
    // by eyeballing PNGs.
    if std::env::var("WOW_UI_PROBE").as_deref() == Ok("1") {
        // Fire on every rebuild (rebuilds are change-driven and rare); read the LAST block.
        {
            info!(
                "ui probe: window {}x{} logical",
                window.width(),
                window.height()
            );
            for q in &sorted {
                info!(
                    "ui probe: [{:.0},{:.0} {:.0}x{:.0}] tex={} z={:x}",
                    q.rect.min.x,
                    q.rect.min.y,
                    q.rect.width(),
                    q.rect.height(),
                    q.texture.as_ref().map_or_else(
                        || "-".into(),
                        |h| {
                            h.path()
                                .map_or_else(|| format!("{:?}", h.id()), |p| p.to_string())
                        }
                    ),
                    q.z_key
                );
            }
        }
    }

    // Split into contiguous texture-identity runs, clipping each quad on the way in. A quad clipped to
    // nothing is simply never pushed — it does NOT break a run (an invisible quad has no texture to
    // conflict with its neighbours).
    let mut runs: Vec<Run> = Vec::new();
    for q in sorted {
        // Explicit-corner quads (the cooldown pie's wedge) carry exact geometry — no clip, no
        // rotation, no UV reprojection to apply (see [`UiQuad::corners`]).
        let plain = if q.corners.is_none() {
            match q.clip {
                Some(clip) => match clip_quad(q.rect, q.uv, clip) {
                    Some(pair) => Some(pair),
                    None => continue,
                },
                None => Some((q.rect, q.uv)),
            }
        } else {
            None
        };
        let texture = q.texture.clone().unwrap_or_else(|| white.0.clone());
        let same_run = runs.last().is_some_and(|r| {
            r.texture == texture
                && r.additive == q.additive
                && r.circular == q.circular
                && r.desaturated == q.desaturated
                && r.mask == q.mask
        });
        if !same_run {
            runs.push(Run::new(
                texture,
                q.additive,
                q.circular,
                q.desaturated,
                q.mask.clone(),
            ));
        }
        let run = runs.last_mut().unwrap();
        match (q.corners, plain) {
            (Some(c), _) => {
                let [tl, tr, br, bl] = q.uv.corners;
                run.push_corners(
                    [
                        (c[0], Vec2::from(tl)),
                        (c[1], Vec2::from(tr)),
                        (c[2], Vec2::from(br)),
                        (c[3], Vec2::from(bl)),
                    ],
                    q.color,
                    to_world,
                );
            }
            (None, Some((rect, uv))) => run.push_quad(rect, uv, q.color, q.rotation, to_world),
            (None, None) => unreachable!("plain is Some when corners is None"),
        }
    }

    // Cross-run order: ascending world-space z (see the module doc — `bevy_sprite_render` sorts its
    // `Transparent2d` phase by ascending mesh z, so later runs drawing on top is exactly "higher z").
    // Spread runs across a z window comfortably inside the camera's default near/far (±1000) regardless
    // of run count, so this never depends on how many runs a given frame happens to produce.
    let run_count = runs.len().max(1) as f32;
    // An unbounded material key set (a window resize moves every mask rect) resets the cache;
    // materials on live batches survive via the entities' own handle clones and simply re-enter
    // on their next miss.
    if pools.materials.len() > 256 {
        pools.materials.clear();
    }
    let mut used = 0usize;
    for (i, run) in runs.into_iter().enumerate() {
        if run.indices.is_empty() {
            continue;
        }
        let z = -450.0 + (i as f32 / run_count) * 900.0;
        // MAIN_WORLD too (not RENDER_WORLD-only): the pool rewrites this asset in place next
        // rebuild, so the main-world copy must survive extraction.
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, run.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, run.uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, run.colors);
        mesh.insert_indices(Indices::U32(run.indices));
        let mesh_handle = match pools.meshes.get(used) {
            Some(handle) => {
                let _ = meshes.insert(handle.id(), mesh);
                handle.clone()
            }
            None => {
                let handle = meshes.add(mesh);
                pools.meshes.push(handle.clone());
                handle
            }
        };
        // The mask span converts to physical framebuffer px here (the shader compares
        // `@builtin(position)`); a maskless run gets a degenerate rect, which disables the branch.
        let scale = window.scale_factor();
        let (mask_rect, mask) = match &run.mask {
            Some(m) => (
                Vec4::new(
                    m.rect.min.x * scale,
                    m.rect.min.y * scale,
                    m.rect.max.x * scale,
                    m.rect.max.y * scale,
                ),
                Some(m.texture.clone()),
            ),
            None => (Vec4::new(0.0, 0.0, -1.0, -1.0), None),
        };
        if std::env::var("WOW_UI_PROBE").as_deref() == Ok("1") && run.mask.is_some() {
            info!(
                "ui probe: masked run {i}: mask_rect={mask_rect:?} scale={scale} tex={:?}",
                run.mask.as_ref().map(|m| m.texture.id())
            );
        }
        let key: MatKey = (
            run.texture.id(),
            run.additive,
            run.circular,
            run.desaturated,
            mask.as_ref().map(bevy::asset::Handle::id),
            mask_rect.to_array().map(f32::to_bits),
        );
        let material_handle = pools
            .materials
            .entry(key)
            .or_insert_with(|| {
                materials.add(UiQuadMaterial {
                    additive: u32::from(run.additive),
                    texture: Some(run.texture),
                    circular: u32::from(run.circular),
                    desaturate: u32::from(run.desaturated),
                    mask_rect,
                    mask,
                })
            })
            .clone();
        // Reuse the batch entity at this slot — same Mesh2d handle (the pooled asset), a material
        // handle that only changes when the run's identity does, a fresh z.
        match pools.entities.get(used) {
            Some(&entity) => {
                commands.entity(entity).insert((
                    Mesh2d(mesh_handle),
                    MeshMaterial2d(material_handle),
                    Transform::from_xyz(0.0, 0.0, z),
                ));
            }
            None => {
                let entity = commands
                    .spawn((
                        UiQuadBatch,
                        Mesh2d(mesh_handle),
                        MeshMaterial2d(material_handle),
                        Transform::from_xyz(0.0, 0.0, z),
                        ui_render_layers(),
                    ))
                    .id();
                pools.entities.push(entity);
            }
        }
        used += 1;
    }
    // Retire the surplus: batch entities beyond this frame's run count despawn, and their pooled
    // mesh assets drop with the truncation (nothing references them anymore).
    for entity in pools.entities.drain(used..) {
        commands.entity(entity).despawn();
    }
    pools.meshes.truncate(used);
}

/// Deliberately-overlapping synthetic content proving the sort: 5 z strata × 40 quads each, offset both
/// within a stratum (neighbours overlap 35px) and across strata (each stratum offset 20px from the
/// last, so a later, higher-`z_key` stratum's field visibly paints over the previous one's — the actual
/// thing this pass exists to get right). Plus one real-BLP-textured quad and one CPU-clipped quad,
/// both given the highest z_keys — see the module doc: this ordering is what makes the shared "white"
/// texture identity get split into two separate runs (the icon quad sits between them in z_key).
const STRATA: usize = 5;
const PER_STRATUM: usize = 40;
const QUAD_SIZE: f32 = 80.0;
const COLS: usize = 10;

fn strata_color(s: usize) -> [f32; 4] {
    // Red → orange → yellow → green → blue, alpha < 1 so the overlaps are visibly additive-ish
    // (provable by eye: a later stratum's quad should read as a *blend* over the earlier one where
    // clipped by nothing else, and fully opaque-looking where nothing underlies it).
    const COLORS: [[f32; 3]; STRATA] = [
        [0.85, 0.15, 0.15],
        [0.90, 0.55, 0.10],
        [0.85, 0.80, 0.10],
        [0.15, 0.75, 0.25],
        [0.15, 0.45, 0.90],
    ];
    let c = COLORS[s % STRATA];
    [c[0], c[1], c[2], 0.85]
}

fn seed_demo_quads(
    mut quads: ResMut<UiQuads>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
) {
    let mut out = Vec::with_capacity(STRATA * PER_STRATUM + 2);

    for s in 0..STRATA {
        let stratum_offset = s as f32 * 20.0;
        for i in 0..PER_STRATUM {
            let (col, row) = (i % COLS, i / COLS);
            let x = 40.0 + stratum_offset + col as f32 * 45.0;
            let y = 40.0 + stratum_offset + row as f32 * 45.0;
            out.push(UiQuad {
                rect: Rect::new(x, y, x + QUAD_SIZE, y + QUAD_SIZE),
                z_key: (s as u64) * 1000 + i as u64,
                color: strata_color(s),
                ..default()
            });
        }
    }

    // Real BLP through the same UI-art path the loading screen uses (`sprite_texture`: sRGB, clamp,
    // no mip chain — one texture mapped to one quad, not tiling world art).
    if let Some(mut assets) = world_assets {
        if let Some(icon) =
            assets.sprite_texture("Interface\\Icons\\INV_Misc_QuestionMark.blp", &mut images)
        {
            out.push(UiQuad {
                rect: Rect::new(560.0, 40.0, 560.0 + 64.0, 40.0 + 64.0),
                z_key: (STRATA as u64) * 1000 + 500,
                texture: Some(icon),
                ..default()
            });
        }
    }

    // CPU-clip demo: a 160×160 quad clipped to its left half — proves the intersect + UV-reprojection
    // path (not just a quad that already happened to fit).
    let clip_rect = Rect::new(700.0, 40.0, 700.0 + 160.0, 40.0 + 160.0);
    out.push(UiQuad {
        rect: clip_rect,
        z_key: (STRATA as u64) * 1000 + 600,
        color: [0.2, 0.8, 1.0, 1.0],
        clip: Some(Rect::new(
            clip_rect.min.x,
            clip_rect.min.y,
            clip_rect.min.x + 80.0,
            clip_rect.max.y,
        )),
        ..default()
    });

    quads.quads = out;
    quads.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rebuild in isolation: real resources, no renderer (the batches are plain entities until
    /// something draws them).
    fn rebuild_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<UiQuadMaterial>()
            .init_resource::<UiQuads>()
            .init_resource::<crate::ui_hide::UiHidden>()
            .insert_resource(UiWhiteTexture(Handle::default()))
            .add_systems(Update, rebuild_ui_mesh);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        // One flat quad in the BASE lane — enough content for exactly one batch.
        let mut quads = app.world_mut().resource_mut::<UiQuads>();
        quads.quads.push(UiQuad {
            rect: Rect::new(0.0, 0.0, 64.0, 64.0),
            ..default()
        });
        quads.dirty = true;
        app
    }

    /// The projector's accept region is **exactly the viewport, inclusive** — the WorldFrame's own
    /// region mirrored ÷G44/÷G48 (`0x483970`), so the aspect factors cancel. A point on the edge
    /// draws (the seat's clamp, not this, is what keeps the rect fully inside); a point outside it
    /// does not, however plausible the coordinate the projector already wrote looks. The last row
    /// is the measured phantom that bought this: a unit two thirds of a screen below the bottom,
    /// whose plate used to be dragged up onto the border (1341).
    #[test]
    fn the_accept_region_is_the_viewport_inclusive() {
        let vp = Vec2::new(1440.0, 810.0);
        assert!(accepts(Vec2::new(720.0, 405.0), vp));
        assert!(accepts(Vec2::ZERO, vp), "the corner is in view");
        assert!(accepts(vp, vp), "so is the far corner");
        for out in [
            Vec2::new(-0.5, 405.0),
            Vec2::new(1440.5, 405.0),
            Vec2::new(720.0, -0.5),
            Vec2::new(720.0, 810.5),
            Vec2::new(1409.0, 1332.0),
        ] {
            assert!(!accepts(out, vp), "{out:?} is not on screen");
        }
    }

    fn batches(app: &mut App) -> usize {
        app.world_mut()
            .query_filtered::<(), With<UiQuadBatch>>()
            .iter(app.world())
            .count()
    }

    fn set_hidden(app: &mut App, hidden: bool) {
        app.world_mut().resource_mut::<crate::ui_hide::UiHidden>().0 = hidden;
    }

    /// TOGGLEUI at the draw: hiding retires the batches, and un-hiding brings the SAME content back
    /// — the case the two lanes' change protocol would otherwise swallow, since nothing about the
    /// quads themselves changed while the UI was dark (`dirty` false, the append lane identical),
    /// so the rebuild would have taken its early-out and left the screen bare.
    #[test]
    fn toggleui_retires_the_batches_and_restores_them() {
        let mut app = rebuild_app();
        app.update();
        assert_eq!(batches(&mut app), 1, "one batch drawn to begin with");

        set_hidden(&mut app, true);
        app.update();
        assert_eq!(batches(&mut app), 0, "hidden ⇒ nothing drawn");
        // Still nothing after further frames: the producers keep filling both lanes, and the
        // rebuild must keep taking its dark path rather than re-spawning on the next flagged frame.
        app.world_mut().resource_mut::<UiQuads>().dirty = true;
        app.update();
        assert_eq!(batches(&mut app), 0, "hidden stays hidden");

        set_hidden(&mut app, false);
        app.update();
        assert_eq!(
            batches(&mut app),
            1,
            "the UI comes back on the same content"
        );
    }

    /// **The desaturation flag reaches the MATERIAL, and splits the run** (decision 1327).
    ///
    /// Everything upstream of here can be right — the Lua sets it, extract carries it, the quad
    /// holds it — and the screen still not change, because the greyscale lives in a shader uniform
    /// and a uniform only exists per material. Two quads that differ ONLY in `desaturated` must
    /// therefore batch apart; if they merged, whichever of the two came first would decide the look
    /// of both, and the visible symptom would be a talent tree that greys in blocks.
    #[test]
    fn desaturation_splits_the_run_and_lands_on_the_material() {
        let mut app = rebuild_app();
        // Same texture (the shared white), same blend, adjacent in z — everything that batches
        // agrees, so `desaturated` is the only thing that can split them.
        let mut quads = app.world_mut().resource_mut::<UiQuads>();
        quads.quads.push(UiQuad {
            rect: Rect::new(64.0, 0.0, 128.0, 64.0),
            z_key: 1,
            desaturated: true,
            ..default()
        });
        quads.dirty = true;
        app.update();
        assert_eq!(
            batches(&mut app),
            2,
            "a desaturated quad cannot share a material with a full-colour one"
        );

        let mut flags: Vec<u32> = app
            .world_mut()
            .resource::<Assets<UiQuadMaterial>>()
            .iter()
            .map(|(_, m)| m.desaturate)
            .collect();
        flags.sort_unstable();
        assert_eq!(
            flags,
            vec![0, 1],
            "one material greys and one does not — the uniform the shader branches on"
        );
    }

    // The mirror-preservation guarantee, and the reason [`UvRect`] exists instead of
    // `bevy::math::Rect`: PlayerFrameTexture's `<TexCoords left="1.0" right="0.09375">`
    // (ref-PlayerFrame.xml l.55-57) must reach the vertex buffer with `u0 > u1`. `Rect::from_corners`
    // normalizes min/max and would silently un-mirror the ring art.
    #[test]
    fn mirrored_tex_coords_emit_unnormalized_corners() {
        // [left, right, top, bottom] as extraction hands them over — left > right (horizontal mirror).
        let uv = UvRect::from_tex_coords([1.0, 0.09375, 0.0, 0.78125]);
        let [tl, tr, br, bl] = uv.corners;
        assert!(
            tl[0] > tr[0],
            "mirror preserved: TL.u {} > TR.u {}",
            tl[0],
            tr[0]
        );
        // The top-LEFT screen vertex samples u=1.0 and the top-RIGHT samples u=0.09375 — i.e. the
        // texture is flipped horizontally on the quad (push_quad maps corners[0]→TL, corners[1]→TR).
        assert_eq!(tl, [1.0, 0.0]);
        assert_eq!(tr, [0.09375, 0.0]);
        assert_eq!(br, [0.09375, 0.78125]);
        assert_eq!(bl, [1.0, 0.78125]);
    }

    // clip_quad reprojects UVs by the clipped screen-edge fractions. The bilinear reduces to the
    // separable corner lerp for an axis-aligned crop, so a mirrored source UV must survive clipping
    // still mirrored (the pin's "verify, don't assume").
    #[test]
    fn clip_preserves_mirrored_uv() {
        let uv = UvRect::from_tex_coords([1.0, 0.09375, 0.0, 0.78125]);
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        // Clip to the left half of the quad.
        let clip = Rect::new(0.0, 0.0, 50.0, 100.0);
        let (crect, cuv) =
            clip_quad(rect, uv, clip).expect("left half is a non-empty intersection");
        assert_eq!(crect, Rect::new(0.0, 0.0, 50.0, 100.0));
        let [tl, tr, ..] = cuv.corners;
        assert!(
            tl[0] > tr[0],
            "mirror survives clip: TL.u {} > TR.u {}",
            tl[0],
            tr[0]
        );
        // Left half of a mirrored range [1.0 .. 0.09375] samples [1.0 .. midpoint].
        assert_eq!(tl[0], 1.0);
        assert!((tr[0] - 0.546_875).abs() < 1e-6); // lerp(1.0, 0.09375, 0.5)
                                                   // The un-clipped vertical axis is untouched.
        assert_eq!(tl[1], 0.0);
        assert_eq!(cuv.corners[3][1], 0.78125); // BL.v
    }

    // A rotated backdrop TOP-edge UV (atlas-u tied to screen-Y, atlas-v to screen-X reversed —
    // backdrop-mechanism.md §3): clipping to the left half must reproject the *v* axis (screen-X),
    // leaving *u* (screen-Y) untouched. This is the case the old separable-lerp clip could not do
    // and the four-corner bilinear must.
    #[test]
    fn clip_reprojects_rotated_uv() {
        // TOP edge with widthRun = 4: TL=(0.25,4) TR=(0.25,0) BR=(0.375,0) BL=(0.375,4).
        let uv = UvRect::from_corners([[0.25, 4.0], [0.25, 0.0], [0.375, 0.0], [0.375, 4.0]]);
        let rect = Rect::new(0.0, 0.0, 100.0, 16.0);
        // Clip to the left half in screen-X (v axis), full height (u axis untouched).
        let clip = Rect::new(0.0, 0.0, 50.0, 16.0);
        let (crect, cuv) = clip_quad(rect, uv, clip).expect("left half intersects");
        assert_eq!(crect, Rect::new(0.0, 0.0, 50.0, 16.0));
        let [tl, tr, br, bl] = cuv.corners;
        // u (screen-Y) unchanged: top row still 0.25, bottom row still 0.375.
        assert!((tl[0] - 0.25).abs() < 1e-6 && (tr[0] - 0.25).abs() < 1e-6);
        assert!((bl[0] - 0.375).abs() < 1e-6 && (br[0] - 0.375).abs() < 1e-6);
        // v (screen-X, reversed): left edge still 4.0; right edge now the midpoint 2.0.
        assert!((tl[1] - 4.0).abs() < 1e-6, "left v kept: {}", tl[1]);
        assert!((tr[1] - 2.0).abs() < 1e-6, "right v halved: {}", tr[1]);
    }
}
