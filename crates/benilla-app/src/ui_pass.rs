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
    /// The sampled texture already carries **premultiplied** colour, so the shader must not weight
    /// it by its own alpha a second time — the booth render targets ([`crate::portrait`]), and
    /// nothing else.
    ///
    /// A booth that clears **transparent** (the `<PlayerModel>` body panes and the dressing room,
    /// which composite over the page's own art — decision 1083) builds its target the way any
    /// render-to-texture does: opaque geometry writes `a = 1`, alpha batches blend over it, and an
    /// **additive particle adds light while contributing no coverage** (`wow_effect.wgsl`'s
    /// `(rgb·a, 0)` under a premultiplied state). That buffer is premultiplied by construction —
    /// colour is emitted light, alpha is coverage. Sampling it as *straight* alpha and applying the
    /// usual `rgb·a` multiplies every effect hanging in EMPTY pane space by zero: the R14
    /// pauldrons' fire vanished outright, and a weapon glow survived only where it happened to
    /// overlap the model's own opaque pixels — chopped to a hard edge at the silhouette (the
    /// director's paper-doll/dressing-room report). The opaque-cleared round portraits hid it,
    /// because `a = 1` everywhere makes the extra multiply a no-op — exactly the way opaque panels
    /// hid the identical defect one level up, in this camera's own output blend (see
    /// [`spawn_player_ui_camera`]'s note on `rgb·a²`).
    ///
    /// Splits the run like `additive`/`circular`; a booth target is its own texture anyway, so no
    /// batching is lost.
    pub premultiplied: bool,
    /// The sampled texture holds the client's **gamma bytes, undecoded** — a SKIP_DECODE upload
    /// (`BlpVariant::MapTile`, the minimap tiles: the reference's tile sampler carries
    /// `GL_TEXTURE_SRGB_DECODE_EXT = GL_SKIP_DECODE_EXT`, so the hardware filters authored bytes).
    /// The ordinary arm's `linear_to_srgb` exists to undo the sampler's sRGB decode; on a texture
    /// whose sampler did no decode it ENCODES A SECOND TIME, which is how the outdoor minimap
    /// washed bright when MapTile moved off `Rgba8UnormSrgb` and only the alpha-test arm learned
    /// the new contract. With this set the texel passes through as the authored byte and the
    /// vertex-colour multiply lands on it directly — the fixed-function MODULATE, byte for byte.
    /// The alpha-test arm ignores the flag: it decodes explicitly for the un-encoded composite
    /// target. Splits the run like `additive`/`circular`.
    pub gamma_texel: bool,
    /// **Alpha TEST** instead of alpha blend, at this reference value on the client's
    /// `texel.a × vertexColour.a` — `Some(224.0 / 255.0)` is the WMO-interior minimap tile draw and
    /// nothing else so far. A passing fragment writes FULLY OPAQUE (the screen mask still cuts it);
    /// a failing one is discarded, so partial coverage never exists on this path.
    ///
    /// The reference's minimap tile draw sets EGxBlend **1**, whose applicator `glDisable`s
    /// blending outright, and the `SetRenderState` id-7→id-8 cascade then arms
    /// `glAlphaFunc(GL_GEQUAL, 0.87843144)` (`.data 0x85ad20[1] = 224`; wow-re
    /// `wmo-interior-minimap-composite.md`). Blending those tiles instead leaves `(1−a)(1−b)` of
    /// the black clear at EVERY boundary where two group tiles meet — up to 25% black where the
    /// two filtered edges are complementary — which is B141's "odd black lines" (they recoloured
    /// with the backing quad, which is how they were caught). Splits the batch run.
    pub alpha_test: Option<f32>,
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
            premultiplied: false,
            gamma_texel: false,
            alpha_test: None,
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
    /// The **world backdrop** ([`crate::world_backdrop`]): the frame the world camera rendered,
    /// drawn before every other quad so the UI blends over it in the same gamma bytes it blends
    /// over itself in. Not part of either lane and not sorted with them — it is not content
    /// competing for a `z_key`, it is the ground. `None` whenever there is no world to paint (the
    /// glue screens, the loading screen, a gated camera).
    ///
    /// It changes no batching decision beyond being first, and it flags [`Self::dirty`] only when
    /// the QUAD changes — arrival, departure, a resize. Its image's contents change every frame and
    /// deliberately do not flag anything: the batch holds the handle and the material samples
    /// whatever the world camera just rendered into it.
    pub backdrop: Option<UiQuad>,
    pub quads: Vec<UiQuad>,
    /// The append lane — see the struct doc. Compared by the rebuild, never flagged.
    pub overlays: Vec<UiQuad>,
    /// The append lane as of the last rebuild — [`rebuild_ui_mesh`]'s change gate for it.
    last_overlays: Vec<UiQuad>,
    /// Set by the BASE-lane producer after replacing `quads` with different content; cleared by
    /// [`rebuild_ui_mesh`] once it has rebuilt the mesh batches.
    pub dirty: bool,
}

/// The **append lane's own z bands** — the world-anchored overlays' slice of the same `z_key`
/// total order the scripted UI packs its `(stratum, level, layer, …)` tuple into
/// ([`benilla_ui::order::ZKey`]).
///
/// The lane's three producers are three *different* client systems that all draw over the world,
/// and their relative order is a fidelity fact each one owns (the floating combat numbers are a
/// world-scene draw under all UI; the plates and the bubbles are frames). What is NOT a fidelity
/// fact is the arithmetic that keeps them apart — and while each producer picked its own small
/// integer, that arithmetic was three private conventions with nothing naming the shared law:
/// combat text at 0, the bubble's four pieces at 0..=3 *on top of it*, the plates at 4..=8. The
/// bands below make the split explicit and, more to the point, give the middle band **room** —
/// a chat bubble is one frame per speaker with its own frame level (decision 1504), which a
/// four-key allocation cannot express. Everything here stays far below the scripted UI's keys
/// (a `ZKey` region carries its is-region bit at `1 << 20`), so the whole lane still paints
/// under the player UI.
pub(crate) mod overlay_z {
    /// Floating combat/XP/honor numbers — the client draws them in the world scene, beneath
    /// every UI frame ([`crate::combat_text`]).
    pub(crate) const WORLD_TEXT: u64 = 0;
    /// The chat bubbles' band ([`crate::chat_bubble`]): one **frame level** per live bubble,
    /// [`BUBBLE_STRIDE`] keys wide, ordered farthest-camera-distance first.
    pub(crate) const BUBBLE: u64 = 1 << 8;
    /// Keys per bubble level — the frame's own four pieces (bg, edge, tail, text).
    pub(crate) const BUBBLE_STRIDE: u64 = 4;
    /// The V-plates ([`crate::vplates`]), above every bubble. Same-unit overlap cannot happen
    /// (the plate/bubble mutual exclusion), so this only orders cross-unit stacking.
    pub(crate) const VPLATE: u64 = 1 << 16;
    /// The highest bubble level the band holds before it would reach [`VPLATE`]. Far past any
    /// real population (a bubble needs a speaker within 20 yd), so the clamp is a guard rail,
    /// not a policy.
    pub(crate) const BUBBLE_MAX_LEVEL: u64 = (VPLATE - BUBBLE) / BUBBLE_STRIDE - 1;
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
    /// The texel is already premultiplied (a booth bake) — see [`UiQuad::premultiplied`].
    #[uniform(8)]
    premultiplied: u32,
    /// Alpha-TEST reference on `texel.a × colour.a`; `<= 0` disables — see [`UiQuad::alpha_test`].
    #[uniform(9)]
    alpha_ref: f32,
    /// The texel is already the client's gamma byte (a SKIP_DECODE upload — the minimap tiles), so
    /// the ordinary arm skips its `linear_to_srgb` re-encode — see [`UiQuad::gamma_texel`].
    #[uniform(10)]
    gamma_texel: u32,
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

impl UiQuadMaterial {
    /// The **WMO-interior minimap tile** material (decision 1466): the texture, an alpha TEST at
    /// `alpha_ref`, and nothing else — no tint, no mask, no circle, no desaturate. It is built here
    /// rather than through the quad stream because these tiles do not draw at the screen at all:
    /// they draw into the minimap's own 256² composite target on its own camera
    /// ([`crate::minimap`]'s `composite`), one shared quad mesh per tile under a Transform.
    ///
    /// The forced `(One, OneMinusSrcAlpha)` blend in [`Self::specialize`] is what makes this
    /// reproduce the client's *disabled* blending exactly: a fragment that passes the test emits
    /// `a = 1`, so the state degenerates to a plain replace — and one that fails emits nothing.
    pub(crate) fn interior_tile(texture: Handle<Image>, alpha_ref: f32) -> Self {
        Self {
            additive: 0,
            texture: Some(texture),
            circular: 0,
            desaturate: 0,
            premultiplied: 0,
            alpha_ref,
            // The alpha-test arm never reads this: it decodes explicitly (its target is the
            // un-encoded composite), so the tile's SKIP_DECODE contract is already honoured there.
            gamma_texel: 0,
            mask_rect: Vec4::new(0.0, 0.0, -1.0, -1.0),
            mask: None,
        }
    }
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
                // **No blend at all** — this camera now carries the world too
                // ([`crate::world_backdrop`]), so its target is a whole opaque frame and the blit
                // is a copy. That is the point: the blend that used to happen HERE, against the
                // sRGB swapchain view, was the frame's one linear composite, and it was the only
                // one that mixed UI with world. Moving the world into the UI's own byte buffer
                // moves that blend into `ui_quad.wgsl`'s gamma target, where every other UI blend
                // already lives.
                //
                // It also retires the hazard 0254 patched around here: `ui_quad.wgsl` writes
                // PREMULTIPLIED colour, so the original `ALPHA_BLENDING`'s `SrcAlpha` factor
                // weighted it by alpha twice (`rgb·a²`), and a pure-additive quad (a = 0) over the
                // world was multiplied clean away. `PREMULTIPLIED_ALPHA_BLENDING` fixed the
                // arithmetic but kept the blend — and kept it in the wrong space. With nothing to
                // blend against, neither factor can be wrong.
                blend_state: None,
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
    premultiplied: bool,
    gamma_texel: bool,
    alpha_test: Option<f32>,
    mask: Option<UiQuadMask>,
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Run {
    /// A fresh run carrying `q`'s material identity (every flag that splits a run) under the
    /// already-resolved `texture` (the quad's own, or the shared white fallback).
    fn new(texture: Handle<Image>, q: &UiQuad) -> Self {
        Self {
            texture,
            additive: q.additive,
            circular: q.circular,
            desaturated: q.desaturated,
            premultiplied: q.premultiplied,
            gamma_texel: q.gamma_texel,
            alpha_test: q.alpha_test,
            mask: q.mask.clone(),
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
    /// Each slot's last-written MESH content (base positions, before [`Self::offsets`]) — the
    /// per-run skip gate (decision 1361): a slot whose run is bit-identical to what its pooled
    /// mesh already holds is not rewritten, so one animating quad no longer drags every batch
    /// through the GPU mesh allocator.
    stored: Vec<StoredRun>,
    /// Each slot's current XY translation from its stored base, carried by the batch entity's
    /// `Transform` instead of the mesh (decision 1463): a run that only *panned* — the minimap
    /// tile was ~74% of all moving-regime rebuild triggers — moves without an `Assets<Mesh>`
    /// write, because ONE Modified event per frame arms `AssetChanged` probes over every
    /// `Mesh3d` row in the scene (1370's all-or-nothing fast path, ~0.8 ms/frame at 1461's
    /// Goldshire pin).
    offsets: Vec<Vec2>,
    materials: std::collections::HashMap<MatKey, Handle<UiQuadMaterial>>,
}

/// One pooled batch slot's full identity: the mesh bytes plus everything the entity was last
/// written with (material key, z). Equality is exact — see the skip gate's comment for why a
/// hash was rejected.
#[derive(PartialEq)]
struct StoredRun {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    z_bits: u32,
    key: MatKey,
}

impl StoredRun {
    /// The candidate's uniform XY offset from this base: `Some(d)` when every vertex is this
    /// run's vertex plus one constant delta and everything else is bit-equal — `Some(ZERO)` is
    /// exactly the old bit-identical skip case, so this subsumes 1361's gate. `None` means the
    /// run genuinely changed shape and must be rebaked. The comparison is exact float
    /// arithmetic on purpose: a miss only falls back to the rewrite path, so a widget whose
    /// pan doesn't survive `b + d == n` bit-exactly loses the optimization, never correctness.
    /// Why [`Self::translation_from`] missed — the pan-gate diagnostic (`WOW_UI_DIFF=1`).
    fn translation_miss_reason(&self, new: &StoredRun) -> &'static str {
        if self.key != new.key {
            "key"
        } else if self.z_bits != new.z_bits {
            "z"
        } else if self.positions.len() != new.positions.len() {
            "len"
        } else if self.indices != new.indices {
            "indices"
        } else if self.uvs != new.uvs {
            "uvs"
        } else if self.colors != new.colors {
            "colors"
        } else {
            "delta-nonuniform"
        }
    }

    fn translation_from(&self, new: &StoredRun) -> Option<Vec2> {
        if self.key != new.key
            || self.z_bits != new.z_bits
            || self.positions.len() != new.positions.len()
            || self.positions.is_empty()
            || self.indices != new.indices
            || self.uvs != new.uvs
            || self.colors != new.colors
        {
            return None;
        }
        let d = Vec2::new(
            new.positions[0][0] - self.positions[0][0],
            new.positions[0][1] - self.positions[0][1],
        );
        self.positions
            .iter()
            .zip(&new.positions)
            .all(|(b, n)| n[0] == b[0] + d.x && n[1] == b[1] + d.y && n[2] == b[2])
            .then_some(d)
    }
}

/// A material's full identity: texture, blend/shape flags, mask texture + mask rect (as bits, so
/// NaN/-0.0 can't split cache entries byte-equal materials would share).
type MatKey = (
    AssetId<Image>,
    bool,
    bool,
    bool,
    bool,
    bool,
    u32,
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
    // The skip gate must forget with them (decision 1361): a retired slot's entity is gone, so
    // "content unchanged" must not skip the respawn when the UI returns. The pan gate's
    // offsets ride the same lifecycle (they index the same slots).
    pools.stored.clear();
    pools.offsets.clear();
}

/// Per frame: when the BASE lane flagged a change ([`UiQuads::dirty`]) or the APPEND lane's
/// content differs from last frame's, stable-sort both lanes by `z_key`, CPU-clip, split into
/// texture-identity runs, and write the runs into the pooled batches ([`BatchPools`]). See the
/// module doc for the full approach (painter's order within/across runs) and its batching
/// consequence.
fn ui_mesh_frozen() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_FREEZE_UI_MESH").is_some())
}

/// `WOW_UI_DIFF` presence — launch-time knob, read once (the rebuild path re-ran getenv per frame).
static UI_DIFF: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("WOW_UI_DIFF").is_some());

/// `WOW_UI_PROBE=1` — launch-time knob, read once (the per-run check re-ran getenv ~90×/frame).
static UI_PROBE: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var("WOW_UI_PROBE").as_deref() == Ok("1"));

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
    // back exactly as it was (see [`crate::ui_hide::UiHidden`]).
    //
    // **It hides the two LANES, never the backdrop.** The world reaches the screen as this pass's
    // own first quad now ([`crate::world_backdrop`]), so the older "retire every batch while
    // hidden" would black the screen — the exact inverse of a binding whose stated point is to
    // leave "the world and nothing else". While dark we still swallow each frame's change flag and
    // keep the append-lane mirror current, so neither lane can hand the rebuild a stale "nothing
    // changed" the moment the UI returns. The edge is the resource's own change tick (`UiHidden` is
    // written only by the binding and the world-exit reset), not a `Local` mirror.
    if hidden.is_changed() {
        q.dirty = true;
    }
    let lanes_hidden = hidden.0;
    if lanes_hidden {
        q.last_overlays.clone_from(&q.overlays);
        // Nothing either lane produces can move a pixel while dark, so only the toggle edge and
        // the backdrop's own arrival/departure (which sets `dirty`) reach the rebuild below.
        if !q.dirty {
            return;
        }
    } else if !q.dirty && q.overlays == q.last_overlays {
        return;
    }
    // `WOW_UI_DIFF=1` — WHO re-triggers the rebuild? Names the first differing overlay quad (or
    // says the BASE lane's dirty flag did it), once a second. The instrument behind the
    // Stormwind mesh-churn hunt: a widget re-emitting a moving/jittering quad every frame drags
    // every batch through a full rebuild and the GPU allocator through ~90 frees+reallocs.
    if *UI_DIFF {
        if q.dirty {
            eprintln!("[ui-diff] BASE lane dirty");
        } else {
            let i = q
                .overlays
                .iter()
                .zip(&q.last_overlays)
                .position(|(a, b)| a != b);
            match i {
                Some(i) => {
                    let (a, b) = (&q.overlays[i], &q.last_overlays[i]);
                    eprintln!(
                        "[ui-diff] overlay {i}/{} differs: tex={:?} rect {:?} -> {:?} uv_changed={} color_changed={}",
                        q.overlays.len(),
                        a.texture.as_ref().and_then(|t| t.path()),
                        b.rect,
                        a.rect,
                        a.uv != b.uv,
                        a.color != b.color,
                    );
                }
                None => eprintln!(
                    "[ui-diff] overlay COUNT changed: {} -> {}",
                    q.last_overlays.len(),
                    q.overlays.len()
                ),
            }
        }
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
    let lanes_empty = lanes_hidden || (q.quads.is_empty() && q.overlays.is_empty());
    if q.backdrop.is_none() && lanes_empty {
        retire_batches(&mut pools, &mut commands);
        q.dirty = false;
        q.last_overlays.clone_from(&q.overlays);
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
    let mut sorted: Vec<&UiQuad> = if lanes_hidden {
        Vec::new()
    } else {
        q.quads.iter().chain(q.overlays.iter()).collect()
    };
    sorted.sort_by_key(|q| q.z_key);
    // The backdrop is PREPENDED, not sorted in. Giving it a `z_key` would mean picking a number
    // below every other producer's and trusting all of them to stay above it — and the append
    // lane's lowest band is already 0 (`overlay_z::WORLD_TEXT`), so there is no room under it
    // without renumbering a total order that encodes fidelity facts. Position, not arithmetic.
    if let Some(backdrop) = q.backdrop.as_ref() {
        sorted.insert(0, backdrop);
    }

    // Geometry probe (`WOW_UI_PROBE=1`): dump each textured quad's screen rect once — the
    // capture-harness companion for diagnosing extracted-vs-rendered geometry by data instead of
    // by eyeballing PNGs.
    if *UI_PROBE {
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
                && r.premultiplied == q.premultiplied
                && r.gamma_texel == q.gamma_texel
                && r.alpha_test == q.alpha_test
                && r.mask == q.mask
        });
        if !same_run {
            runs.push(Run::new(texture, q));
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
        if *UI_PROBE && run.mask.is_some() {
            info!(
                "ui probe: masked run {i}: mask_rect={mask_rect:?} scale={scale} tex={:?}",
                run.mask.as_ref().map(|m| m.texture.id())
            );
        }
        let alpha_ref = run.alpha_test.unwrap_or(0.0);
        let key: MatKey = (
            run.texture.id(),
            run.additive,
            run.circular,
            run.desaturated,
            run.premultiplied,
            run.gamma_texel,
            alpha_ref.to_bits(),
            mask.as_ref().map(bevy::asset::Handle::id),
            mask_rect.to_array().map(f32::to_bits),
        );
        // The per-slot skip gate (decision 1361). A rebuild fires for the WHOLE quad stream the
        // moment anything differs — and one continuously-animating quad (the resting blink on
        // the player frame, in every city) fires it every frame. The run that quad lives in is
        // the only one whose bytes moved; the other ~90 slots are bit-identical to what their
        // pooled mesh already holds, and rewriting them anyway put every UI batch through the
        // GPU mesh allocator's free+realloc each frame (0.86 ms/frame at the Stormwind pin).
        // Full content compare, not a hash — a stale batch drawn over a collision would be a
        // rendering bug no one could reproduce.
        let stored = StoredRun {
            positions: run.positions,
            uvs: run.uvs,
            colors: run.colors,
            indices: run.indices,
            z_bits: z.to_bits(),
            key,
        };
        // The pan gate (1463): a run that matches its slot's base up to one constant XY delta
        // moves on the batch entity's `Transform` — no mesh write, no `AssetChanged` arming.
        // `Some(ZERO)` is the bit-identical case (1361's old skip gate), where even the
        // `Transform` write is skipped unless a previous pan is being undone.
        let pan = pools
            .stored
            .get(used)
            .and_then(|prev| prev.translation_from(&stored).map(|d| (d, prev.z_bits)));
        if let Some((d, z_bits)) = pan {
            if pools.offsets[used] != d {
                pools.offsets[used] = d;
                if let Some(&entity) = pools.entities.get(used) {
                    commands.entity(entity).insert(Transform::from_xyz(
                        d.x,
                        d.y,
                        f32::from_bits(z_bits),
                    ));
                }
            }
            used += 1;
            continue;
        }
        // Pan-gate miss diagnostic (`WOW_UI_DIFF=1`, ≤3 lines/s so a startup burst can't
        // exhaust it): names the check that sent this slot to the rewrite path.
        if *UI_DIFF {
            use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
            static SHOWN: AtomicU32 = AtomicU32::new(0);
            static LAST_SEC: AtomicU64 = AtomicU64::new(0);
            static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            let sec = START
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_secs();
            if LAST_SEC.swap(sec, Ordering::Relaxed) != sec {
                SHOWN.store(0, Ordering::Relaxed);
            }
            if SHOWN.fetch_add(1, Ordering::Relaxed) < 3 {
                let why = pools
                    .stored
                    .get(used)
                    .map_or("no-prev", |p| p.translation_miss_reason(&stored));
                eprintln!(
                    "[ui-pan] slot {used} rewrite: {why} (quads={}, key.tex={:?})",
                    stored.positions.len() / 4,
                    stored.key.0
                );
            }
        }
        // MAIN_WORLD too (not RENDER_WORLD-only): the pool rewrites this asset in place next
        // rebuild, so the main-world copy must survive extraction.
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, stored.positions.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, stored.uvs.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, stored.colors.clone());
        mesh.insert_indices(Indices::U32(stored.indices.clone()));
        if pools.stored.len() > used {
            pools.stored[used] = stored;
        } else {
            pools.stored.push(stored);
        }
        // A rebaked slot's mesh holds absolute positions again — its pan resets with it (the
        // rewrite arm below writes `Transform::from_xyz(0, 0, z)`).
        if pools.offsets.len() > used {
            pools.offsets[used] = Vec2::ZERO;
        } else {
            pools.offsets.push(Vec2::ZERO);
        }
        let mesh_handle = match pools.meshes.get(used) {
            Some(handle) => {
                // `WOW_FREEZE_UI_MESH=1` — never rewrite a pooled batch mesh in place (the UI
                // freezes at its first-built frame). An EXPERIMENT knob (the 1370 bracket): the
                // resting blink's ~2 rewrites/frame are one-asset `Assets<Mesh>` mutations, and
                // bevy 0.18's `AssetChanged<Mesh3d>` fast path is all-or-nothing — one modified
                // mesh arms a hash probe over every `Mesh3d` row in the scene (~44k at the SW
                // pin) in three PostUpdate walks. This lever prices that arming; `WOW_MESH_EVENTS`
                // confirms it (events/s → 0). First build (the `None` arm) is untouched.
                if !ui_mesh_frozen() {
                    let _ = meshes.insert(handle.id(), mesh);
                }
                handle.clone()
            }
            None => {
                let handle = meshes.add(mesh);
                pools.meshes.push(handle.clone());
                handle
            }
        };
        let material_handle = pools
            .materials
            .entry(key)
            .or_insert_with(|| {
                materials.add(UiQuadMaterial {
                    additive: u32::from(run.additive),
                    texture: Some(run.texture),
                    circular: u32::from(run.circular),
                    desaturate: u32::from(run.desaturated),
                    premultiplied: u32::from(run.premultiplied),
                    alpha_ref,
                    gamma_texel: u32::from(run.gamma_texel),
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
    pools.stored.truncate(used);
    pools.offsets.truncate(used);
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

    /// **TOGGLEUI hides the UI, not the world.** Since the world arrives as this pass's own
    /// backdrop quad ([`crate::world_backdrop`]), the dark path can no longer mean "retire every
    /// batch" — that would black the screen, which is the exact inverse of what the binding is
    /// for. One batch survives while dark (the backdrop's), and the lanes come back on top of it.
    ///
    /// This is the regression the change itself created: every earlier version of the hidden path
    /// asserted zero batches, and zero batches is now a black screen.
    #[test]
    fn toggleui_keeps_the_world_backdrop() {
        let mut app = rebuild_app();
        let backdrop = UiQuad {
            rect: Rect::from_corners(Vec2::ZERO, Vec2::new(800.0, 600.0)),
            texture: None,
            ..UiQuad::default()
        };
        {
            let mut q = app.world_mut().resource_mut::<UiQuads>();
            q.backdrop = Some(backdrop);
            q.dirty = true;
        }
        app.update();
        let lit = batches(&mut app);
        assert!(lit >= 1, "content drawn to begin with");

        set_hidden(&mut app, true);
        app.update();
        assert_eq!(
            batches(&mut app),
            1,
            "dark ⇒ the backdrop alone; anything less is a black screen"
        );

        set_hidden(&mut app, false);
        app.update();
        assert_eq!(
            batches(&mut app),
            lit,
            "the UI comes back over the same world"
        );
    }

    /// With no world to paint (the glue screens, the loading screen, a gated camera) the dark path
    /// is the old one: nothing at all. The backdrop earns an exemption because it IS the world,
    /// not because it is first in the list.
    #[test]
    fn toggleui_with_no_backdrop_still_retires_everything() {
        let mut app = rebuild_app();
        app.update();
        assert_eq!(batches(&mut app), 1, "one batch drawn to begin with");
        set_hidden(&mut app, true);
        app.update();
        assert_eq!(batches(&mut app), 0, "no world, no backdrop, nothing drawn");
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
