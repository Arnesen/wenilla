use mlua::{Lua, Value};

use crate::layout::{Anchor, Rect};
use crate::order::ZTarget;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Public value + extract types (engine-free seams)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A simple, engine-free value the host can inject into Lua — the seam the app's net/state→event
/// bridge (decision 0068 §3) uses to hand `fire_event` its `arg1..argN`. Deliberately not
/// `mlua::Value` so callers never touch mlua handles (the MAXCSTACK discipline reaches the API too).
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptValue {
    Nil,
    Bool(bool),
    /// An integer argument (GUIDs-as-strings aside, most event args are ints/strings).
    Int(i64),
    Number(f64),
    Str(String),
}

impl ScriptValue {
    pub(crate) fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            ScriptValue::Nil => Value::Nil,
            ScriptValue::Bool(b) => Value::Boolean(b),
            ScriptValue::Int(i) => Value::Integer(i),
            ScriptValue::Number(n) => Value::Number(n),
            ScriptValue::Str(s) => Value::String(lua.create_string(&s)?),
        })
    }
}

/// A texture region's UV mapping — the two live `SetTexCoord` forms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TexCoords {
    /// The 4-edge crop `[left, right, top, bottom]` (`SetTexCoord(l,r,t,b)` / XML `<TexCoords>`):
    /// an axis-aligned UV sub-rect in 0..1 texture space, top-left origin. A mirrored
    /// (`left > right`) or flipped tuple keeps its orientation.
    Rect([f32; 4]),
    /// The 8-arg affine form (`SetTexCoord(ULx,ULy, LLx,LLy, URx,URy, LRx,LRy)`) — an arbitrary
    /// UV quad (rotation/shear; the reference's `DrawRouteLine` taxi route lines are the first
    /// consumer). Stored per corner in **screen order `[TL, TR, BR, BL]`** (the renderer's
    /// `push_quad` winding — the Lua arg order is converted at the binding).
    Corners([[f32; 2]; 4]),
}

impl TexCoords {
    /// The tightest axis-aligned `[left, right, top, bottom]` containing the mapping — what the
    /// 4-value `GetTexCoord()` reports (exact for `Rect`, the bounding box for `Corners`).
    pub fn edges(&self) -> [f32; 4] {
        match *self {
            TexCoords::Rect(e) => e,
            TexCoords::Corners(c) => {
                let (us, vs): (Vec<f32>, Vec<f32>) = c.iter().map(|&[u, v]| (u, v)).unzip();
                let min = |xs: &[f32]| xs.iter().copied().fold(f32::INFINITY, f32::min);
                let max = |xs: &[f32]| xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                [min(&us), max(&us), min(&vs), max(&vs)]
            }
        }
    }
}

/// What an [`ExtractedQuad`] carries to a renderer, beyond its rect.
#[derive(Clone, Debug, PartialEq)]
pub enum QuadContent {
    /// The frame's own draw slot (backdrop/scissor seat) — no region visual of its own in v1.
    Frame,
    /// A `Minimap` widget's own draw slot: the circular HUD map's content hole. The engine core
    /// carries the resolved rect + the zoom index; the app renderer draws the world into it —
    /// the streamed tile window, the mask, and the player arrow (decision 0203). Emitted *at the
    /// frame's own z*, so the widget's children (border art, buttons) paint above the map exactly
    /// as the reference layers them.
    Minimap {
        /// The **outdoor** zoom index, `0..MINIMAP_ZOOM_LEVELS` (0 = widest) — see
        /// [`crate::widget::MinimapState`].
        zoom: u8,
        /// The **indoor** zoom index (persisted separately; the client's `0x86f69c`). Both travel
        /// so the app renderer picks by its own WMO-containment test rather than racing the
        /// `inside` flag it pushed down.
        inside_zoom: u8,
    },
    /// A `Cooldown` widget's draw slot (decision 0137 phase 4): the engine-derived phase of the
    /// reference machine (`Cooldown.lua`), for the app renderer's pie-wipe/flash draw. Emitted
    /// only while the widget is shown; `tick` hides it once the flash ends.
    Cooldown {
        /// Sweep progress `0..1` (`(now-start)/duration`, the `Cooldown.lua` scrub); `>= 1` =
        /// the sweep is over (the flash below runs) — no dark pie draws.
        fraction: f32,
        /// The finish flash's progress `0..1` over its authored 1.000 s (the model's sequence 1),
        /// `None` while the sweep still runs. The alpha ramp is the model's own texture-weight
        /// track — byte-read off `UI-Cooldown-Indicator.m2` (linear 0→1 over the first third,
        /// hold to the half, 1→0 over the back half) — applied app-side.
        flash: Option<f32>,
    },
    /// A `Texture` region: a BLP path *or* a solid/vertex color (or both — a tinted texture).
    Texture {
        path: Option<String>,
        color: Option<[f32; 4]>,
        /// WoW `ADD` blend (highlights/glows) instead of straight alpha.
        additive: bool,
        /// The `<TexCoords>`/`SetTexCoord` UV mapping the region samples — the 4-edge crop or the
        /// 8-arg affine quad ([`TexCoords`]). `None` = the full texture (0,1,0,1). The renderer
        /// crops (or reprojects) the quad's UVs to this.
        tex_coords: Option<TexCoords>,
        /// Draw masked to the inscribed circle — a **portrait** (`SetPortraitToTexture`, and
        /// `SetPortraitTexture`'s round unit-portrait binding). The renderer bakes the circular
        /// alpha so the square icon/model doesn't poke past the frame ring. A `portrait_unit`
        /// binding with this **false** is the square booth pane (`BenillaSetBoothTexture` —
        /// the paper doll's model view, decision 0208 §5).
        circular: bool,
        /// A **live unit portrait** (`SetPortraitTexture(region, unit)` round;
        /// `BenillaSetBoothTexture(region, token)` square — `circular` above distinguishes): the
        /// unit token whose model this region renders. When `Some`, the renderer ignores
        /// `path`/`color` and samples the app-side portrait render-target for this token (the
        /// off-screen model bake). `None` = an ordinary texture/color region. The engine-free core
        /// only carries the token; resolving it to a rendered image is the app's job (the modern
        /// 2D portrait is a high-res model bake).
        portrait_unit: Option<String>,
        /// On-screen rotation about the quad center, radians, **counterclockwise-positive** (the
        /// `SetRotation` texture API — a later-era method benilla ships early: the world-map
        /// player arrow spins by it, standing in for the reference's engine arrow model; 0203
        /// flags the stand-in). `0.0` for the overwhelmingly common unrotated case.
        rotation: f32,
    },
    /// One frame `Backdrop` piece (the tiled bg, or one of the 8 border pieces) — emitted at the
    /// owning frame's own draw slot, behind its regions ([`UiScript::extract`](super::UiScript::extract)). A textured quad
    /// whose UVs are four explicit per-corner pairs (`[TL, TR, BR, BL]`, screen order) because the
    /// TOP/BOTTOM edges are rotated 90°, and whose texture needs REPEAT addressing (edges tile).
    /// The [`ExtractedQuad::rect`] is the piece's screen bounding box.
    Backdrop {
        /// The bg or edge texture path.
        path: String,
        /// The bg (`SetBackdropColor`) or border (`SetBackdropBorderColor`) vertex tint, pre-alpha.
        color: [f32; 4],
        /// Four per-corner UVs, `[TL, TR, BR, BL]` (screen order).
        uvs: [[f32; 2]; 4],
        /// Sample the texture with REPEAT (wrap) addressing.
        tile: bool,
    },
    /// A `FontString` region: its text, color, horizontal justification, and the resolved font
    /// (path + height + shadow) from its font object / `SetFont` — rendering/metrics are the Bevy
    /// side's job (it honors [`QuadContent::Text::justify_h`], picks the face by `font`, bakes at
    /// `height`, and draws [`QuadContent::Text::shadow`] as an offset dark pass behind the glyphs).
    Text {
        text: Option<String>,
        color: Option<[f32; 4]>,
        justify_h: JustifyH,
        justify_v: JustifyV,
        /// The resolved font face path (`SetFontObject`/`SetFont`/XML `inherits=`), e.g.
        /// `"Fonts\\FRIZQT__.TTF"`. `None` ⇒ the renderer's default face (Friz Quadrata).
        font: Option<String>,
        /// The resolved font height in logical px. `None` ⇒ the renderer's default size.
        font_height: Option<f32>,
        /// The `SetTextHeight` override (see [`RegionData::text_height`]): `Some` = the
        /// scaled-string regime — draw at THIS height, uncapped (the one-to-one 32-px raster cap
        /// applies only when this is `None`).
        text_height: Option<f32>,
        /// The resolved drop shadow (font object `<Shadow>` — offset + color). The real 1.12
        /// `MasterFont` root carries `(1,-1)` black, so nearly every stock font is shadowed; the
        /// crispness of client text against parchment/world IS this shadow.
        shadow: Option<FontShadow>,
        /// The resolved glyph outline (`outline="NORMAL"/"THICK"`). The Number* fonts (action-bar
        /// hotkeys/counts) carry it; the renderer draws it as a black offset halo under the fill —
        /// `cosmic-text` has no native outline. `None` for the common un-outlined case.
        outline: Outline,
        /// The write-on reveal (`SetAlphaGradient` — see [`RegionData::alpha_gradient`]): the
        /// renderer multiplies each glyph's alpha by the ramp at its character position (before
        /// `start` opaque, the next `length` chars 1→0, beyond invisible). `None` = draw whole.
        alpha_gradient: Option<(f32, f32)>,
    },
}

/// A `FontString`'s horizontal text justification within its resolved rect — the XML `justifyH`
/// attr (`SetJustifyH`). The FrameXML default is **CENTER** (the client's `JustifyH` field default);
/// the app's text renderer positions each line's run against the rect per this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JustifyH {
    Left,
    #[default]
    Center,
    Right,
}

/// A `FontString`'s vertical text justification within its resolved rect — the XML `justifyV`
/// attr (`SetJustifyV`). The FrameXML default is **MIDDLE** (the client's `JustifyV` field
/// default) — a sized FontString centers its line block vertically, which is why the real
/// client's 13px money numbers sit on their coin icons' centerline. The app's text renderer
/// offsets the line block against the rect per this (no-op for a rect sized to its text, i.e.
/// every host-measured height-less FontString).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JustifyV {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// A `FontString`'s glyph outline (the XML `outline`/`<Font outline=>` attr, OUTLINETYPE): `NONE`
/// (the common case), `NORMAL`, or `THICK`. Resolved through the font registry and readable via
/// `GetFont`; the v1 text renderer does **not** yet rasterize outlines (a documented gap — the atlas
/// bakes plain coverage), so this is carried for fidelity/round-tripping, not drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outline {
    #[default]
    None,
    Normal,
    Thick,
}

impl Outline {
    /// Parse an OUTLINETYPE token (`"NONE"`/`"NORMAL"`/`"THICK"`, case-insensitive); unknown ⇒ `None`.
    pub fn parse(s: &str) -> Outline {
        match s.to_ascii_uppercase().as_str() {
            "NORMAL" => Outline::Normal,
            "THICK" => Outline::Thick,
            _ => Outline::None,
        }
    }

    /// The OUTLINETYPE token (for `GetFont`'s flags return).
    pub fn as_str(self) -> &'static str {
        match self {
            Outline::None => "",
            Outline::Normal => "OUTLINE",
            Outline::Thick => "THICKOUTLINE",
        }
    }
}

/// A font object's drop shadow (`<Shadow><Offset/><Color/></Shadow>`): pixel offset in WoW y-up
/// XML convention (`y="-1"` = 1px down) + color. Inherited down `<Font>` chains like every other
/// paint field (the 1.12 `MasterFont` root sets `(1,-1)` black — ref-Fonts.xml l.55-62).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontShadow {
    /// XML `<Offset><AbsDimension x= y=/>` — WoW y-up (negative y = down-screen).
    pub offset: [f32; 2],
    /// `<Color r= g= b= a=/>`; alpha defaults 1.
    pub color: [f32; 4],
}

/// A named virtual **Font object** (`<Font name=… font=… inherits=…>`): the resolved paint a
/// `FontString` picks up by `inherits=`/`SetFontObject`. Values are already flattened through the
/// `<Font>` inherits chain (rooted at a real TTF path); any field the chain never set is `None`
/// (the renderer falls back). This is the registry entry [`Model::font_objects`](super::Model::font_objects) keys by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontObject {
    /// The TTF path (`font="Fonts\\FRIZQT__.TTF"`), inherited down the chain.
    pub font: Option<String>,
    /// The font height in logical px (`<FontHeight><AbsValue val=/>`).
    pub height: Option<f32>,
    /// The drop shadow (`<Shadow>`), inherited down the chain like the rest of the paint.
    pub shadow: Option<FontShadow>,
    /// The text color (`<Color r= g= b= a=/>`).
    pub color: Option<[f32; 4]>,
    /// The glyph outline (`outline=` attr).
    pub outline: Outline,
    /// The default horizontal justification (`justifyH=` attr), if the object sets one.
    pub justify_h: Option<JustifyH>,
    /// The default vertical justification (`justifyV=` attr), if the object sets one.
    pub justify_v: Option<JustifyV>,
}

/// One entry of the render list [`UiScript::extract`](super::UiScript::extract) produces: a draw target in the client's exact
/// painter order (the [`crate::order::traversal`] `ZKey` order), its resolved screen rect (if the
/// layout pass resolved it), the effective alpha to draw at, and its content. This is the seam the
/// Bevy extractor consumes; it names only this crate's own types (handles, [`Rect`]) — no engine.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedQuad {
    /// The draw target (a frame's own slot, or one of its regions).
    pub target: ZTarget,
    /// The packed draw-order key (`ZKey::raw`); entries are already sorted ascending by it.
    pub z: u64,
    /// The resolved rect (`[bottom, left, top, right]`, screen px, y-up). `None` if the owning frame
    /// is under-constrained (no resolvable anchors) — a renderer skips it. A region's rect is its
    /// own owner-relative resolution when it carries anchors, else centered-on / filling its owner
    /// (see [`UiScript::extract`](super::UiScript::extract)).
    pub rect: Option<Rect>,
    /// The effective alpha to draw at: for a frame slot its own `effective_alpha`; for a region the
    /// verified single-hop product `region.alpha × ownerFrame.effective_alpha` (see
    /// [`RegionData::alpha`]).
    pub alpha: f32,
    /// The renderable content.
    pub content: QuadContent,
    /// The ScrollFrame clip this quad draws within (decision 0112): `Some(rect)` when the owning
    /// frame is a ScrollFrame's scroll child, or any descendant of one — nested ScrollFrames
    /// intersect (see [`UiScript::extract`](super::UiScript::extract)'s `effective_clip`). `None` = unclipped, the common case.
    pub clip: Option<Rect>,
}

/// The visual state of a region leaf (`Texture`/`FontString`), stored beside the arena because
/// [`crate::widget::Region`] models only structure (kind/owner/layer), not paint. See [`object`]'s
/// `SetTexture`/`SetVertexColor`/`SetText`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RegionData {
    /// Region-level visibility (`Show`/`Hide` on a Texture/FontString, XML `hidden=` — the real
    /// VisibleRegion bit): a hidden region emits no quad regardless of its owner frame's state.
    /// The ref's own kit relies on it (PanelTemplates_SelectTab hides tab slice textures;
    /// ContainerFrame_Update hides cooldown textures).
    pub(crate) hidden: bool,
    /// The region's own alpha (`SetAlpha`/`GetAlpha`, XML `alpha=`); `None` = never set = 1.0.
    ///
    /// A region draws at `ownAlpha × ownerFrame.alpha` — a **single hop** to its immediate owner,
    /// never a product up the tree (wow-re `propagation.md`, VERIFIED: frame `SetAlpha 0x76a690`
    /// overwrite-cascades onto child *frames* and only *invalidates* child regions, which re-read
    /// the owner's `+0xc8` at draw). [`UiScript::extract`](super::UiScript::extract) folds this into
    /// [`ExtractedQuad::alpha`] alongside that owner alpha.
    ///
    /// **Open (wow-re gap):** the findings verify a two-factor draw multiply
    /// (`ownColorAlpha × frameAlpha`) but never pin whether `SetAlpha` writes a *distinct* field or
    /// simply the alpha channel of [`Self::color`]. We keep it distinct so `SetAlpha` on a
    /// FontString cannot clobber its font object's color. The two readings agree on every reference
    /// call site (none sets both on one region); a site that does is the trigger to settle it.
    pub(crate) alpha: Option<f32>,
    /// A texture path (`SetTexture("Interface\\...")`).
    pub(crate) texture: Option<String>,
    /// A color: a solid fill (`SetTexture(r,g,b,a)`) or a vertex tint (`SetVertexColor`).
    pub(crate) color: Option<[f32; 4]>,
    /// This region is a **portrait**: draw its texture masked to the inscribed circle (set by
    /// `SetPortraitToTexture`, and by `SetPortraitTexture`'s round unit binding). WoW portraits
    /// are circular — the frame ring is a thin band whose transparent corners would otherwise
    /// expose the square icon/model. The app bakes the circular alpha mask; the flag rides
    /// through [`QuadContent::Texture::circular`]. False with a live `portrait_unit` = the square
    /// booth pane (`BenillaSetBoothTexture`, decision 0208 §5).
    pub(crate) circular: bool,
    /// A **live unit portrait** (`SetPortraitTexture(region, unit)` round;
    /// `BenillaSetBoothTexture(region, token)` square): the unit token whose model this region
    /// renders. `Some` ⇒ the region samples the app's off-screen portrait bake for this token
    /// (ignoring `texture`/`color`); rides through [`QuadContent::Texture::portrait_unit`]. Cleared by
    /// `SetTexture`/`SetPortraitToTexture` (the region becomes an ordinary texture again).
    pub(crate) portrait_unit: Option<String>,
    /// FontString text (`SetText`).
    pub(crate) text: Option<String>,
    /// The write-on reveal (`SetAlphaGradient(start, length)` — CSimpleFontString's per-character
    /// alpha gradient, the quest-description "writing" machinery): characters before `start` draw
    /// opaque, the next `length` ramp 1→0, the rest invisible. `None` = no gradient (every
    /// FontString but an armed quest description). Cleared by `SetText` — fresh text draws whole.
    pub(crate) alpha_gradient: Option<(f32, f32)>,
    /// WoW `ADD` blend (`SetBlendMode("ADD")` / XML `alphaMode` — the shared enum `0x811aa8`).
    /// Highlight state textures default to it (the client's `SetHighlightTexture` contract).
    pub(crate) additive: bool,
    /// Explicit region size (`SetWidth`/`SetHeight`/XML `<Size>`); `None` = derive. When the region
    /// carries [`RegionData::anchors`], size fills the axis the anchors don't pin (the client's "0 =
    /// derive"); with **no** anchors, a sized region draws *centered* on its owner (the common
    /// state-texture overhang, e.g. the 64×64 quickslot ring on a 36×36 button).
    pub(crate) size: Option<(f32, f32)>,
    /// Region anchors (`SetPoint`/XML `<Anchors>` on a Texture/FontString). An anchor's
    /// `relative_to` defaults to the owner frame and may name a frame or a **sibling region**
    /// (resolved via [`Model::region_names`]; the fixpoint in [`UiScript::resolve`] orders the
    /// sibling chain). Empty ⇒ the size/centered/fill fallback in [`UiScript::extract`];
    /// non-empty ⇒ resolved in [`UiScript::resolve`], any edge the anchors leave unset inherited
    /// from the owner frame's rect.
    pub(crate) anchors: Vec<Anchor>,
    /// FontString horizontal justification (`SetJustifyH`/XML `justifyH`); default CENTER.
    pub(crate) justify_h: JustifyH,
    /// FontString vertical justification (`SetJustifyV`/XML `justifyV`); default MIDDLE.
    pub(crate) justify_v: JustifyV,
    /// The `<TexCoords>`/`SetTexCoord` UV mapping ([`TexCoords`]: the 4-edge crop, or the 8-arg
    /// affine quad). `None` = the full texture. Slices the quadrant/atlas art (decision 0084).
    pub(crate) tex_coords: Option<TexCoords>,
    /// On-screen rotation about the region center (`SetRotation`, radians, counterclockwise-
    /// positive) — see [`QuadContent::Texture::rotation`].
    pub(crate) rotation: f32,
    /// The named font object this FontString last resolved (`inherits=`/`SetFontObject`), for
    /// `GetFontObject`. The object's paint is copied into the fields below at resolve time.
    pub(crate) font_object: Option<String>,
    /// Resolved font face path (`SetFont`/`SetFontObject`). `None` = the renderer's default face.
    pub(crate) font_path: Option<String>,
    /// Resolved font height in logical px (`SetFont`/`SetFontObject`/`<FontHeight>`). `None` = default.
    pub(crate) font_height: Option<f32>,
    /// A `SetTextHeight` scale override — the client's two text-size regimes (§5-verified,
    /// wow-re `fontstring-overflow.md`): a FontString defaults to **one-to-one** (bit `0x200`,
    /// drawn at the raster px, subject to the 32-px raster cap); `SetTextHeight 0x771600` is the
    /// ONLY clearer — the literal size then flows through UNCAPPED, magnified from the raster.
    /// `Some` = that regime; the renderer draws at this height and skips the one-to-one cap.
    pub(crate) text_height: Option<f32>,
    /// Resolved glyph outline (`SetFont` flags / font object). Carried, not yet drawn (see [`Outline`]).
    pub(crate) outline: Outline,
    /// Resolved drop shadow (font object `<Shadow>`), drawn by the renderer behind the glyphs.
    pub(crate) font_shadow: Option<FontShadow>,
    /// The host-measured wrapped text size for a FontString with no explicit height — the engine's
    /// side of the measure round-trip ([`UiScript::fontstrings_needing_measure`] →
    /// [`UiScript::set_measured_text`]): the real client's layout asks its font engine for string
    /// metrics exactly like this (`fontstring.md`). `key` invalidates on text/font/wrap changes.
    pub(crate) measured: Option<MeasuredText>,
}

/// A cached host measurement of a FontString's laid-out text (see [`RegionData::measured`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MeasuredText {
    pub(crate) w: f32,
    pub(crate) h: f32,
    /// Hash of (text, font path, font height bits, wrap-width bits) — mismatch ⇒ re-measure.
    pub(crate) key: u64,
}

impl RegionData {
    /// The measure-cache key for the region's CURRENT text/font/wrap/outline — the one recipe both
    /// sides of the measure round-trip share. [`super::UiScript::fontstrings_needing_measure`]
    /// stamps it into each request (and skips regions whose stored [`MeasuredText::key`] still
    /// matches); the Lua-visible metric reads (`GetStringWidth`/`GetWidth`, region.rs) treat a
    /// stored measure whose key mismatches as ABSENT — a measure of text this region no longer
    /// holds is not a metric (the whisper-header cursor bug: `SetText("Tell …: ")` then a
    /// same-frame `GetWidth()` served the OLD header's width, and the edit box latched its text
    /// insets on it). The layout solver's own read (layout.rs) deliberately does NOT key-check:
    /// it keeps the last-known box until the fresh measure lands, so lines never collapse for the
    /// round-trip frame.
    pub(crate) fn measure_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.text.clone().unwrap_or_default().hash(&mut hasher);
        self.font_path.hash(&mut hasher);
        self.font_height.map(f32::to_bits).hash(&mut hasher);
        self.text_height.map(f32::to_bits).hash(&mut hasher);
        let wrap_width = self.size.map(|s| s.0).filter(|w| *w > 0.0);
        wrap_width.map(f32::to_bits).hash(&mut hasher);
        // Outline is in the key because it changes measured WIDTH under the client's step law
        // (an outlined font steps +1px more per glyph — GlyphStepBase 0x5ca2b0).
        (self.outline as u8).hash(&mut hasher);
        hasher.finish()
    }
}

/// The focused EditBox's advance-table request — returned by
/// [`UiScript::editbox_advances_request`](super::UiScript::editbox_advances_request), answered
/// via [`UiScript::set_editbox_advances`](super::UiScript::set_editbox_advances) with the
/// per-byte cumulative laid-out widths of `text` (len+1 entries, `[0] = 0`, a continuation byte
/// repeating its lead's value). The metrics seam that makes click→char-index, drag-select, and
/// the caret scroll window engine-local (RF-0082's mouse/caret leaves).
#[derive(Clone, Debug, PartialEq)]
pub struct EditBoxAdvanceRequest {
    /// Opaque frame id of the box — pass back verbatim.
    pub id: u32,
    pub font: Option<String>,
    pub height: Option<f32>,
    /// The resolved glyph outline — the per-glyph step bias (+1px, GlyphStepBase 0x5ca2b0)
    /// changes every advance.
    pub outline: Outline,
    /// The DISPLAY string (mask-aware) the table indexes.
    pub text: String,
    /// `Some(width)` for a multiline box: also answer the wrapped-row starts + row pitch at this
    /// wrap width (the text region's resolved width — the same width the draw wraps at), via
    /// [`UiScript::set_editbox_advances`](super::UiScript::set_editbox_advances)'s `rows`/`cell_h`.
    /// `None` = single-line (answer `rows = [0]`).
    pub wrap_width: Option<f32>,
    /// Cache key — pass back verbatim.
    pub key: u64,
}

/// The text span a cursor motion or deletion operates over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditUnit {
    /// One character (the plain arrow / Backspace / Delete granularity).
    Char,
    /// One word run (`EditBoxState::word_boundary` — the Ctrl/Option-arrow granularity).
    Word,
    /// The line edge: text start going back, text end going forward (Home/End, Cmd+arrow).
    Edge,
}

/// One semantic text-editing operation on the focused EditBox — fed to
/// [`UiScript::editbox_action`](super::UiScript::editbox_action). The host's per-OS keymap
/// (which physical chord means which action: Ctrl+Left on Windows, Option+Left on macOS, …)
/// translates key events into these; the *effect* of each action is the engine's byte-verified
/// box law (RF-0082: selection anchoring, selection-first deletes, word classes). Clipboard
/// operations are deliberately absent: they need the OS pasteboard, so they stay host-side
/// (`editbox_copy`/`editbox_cut`/`paste`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Move the caret one `unit` back/forward; `extend` drags the selection from its fixed
    /// anchor (the Shift family). `Char` moves honor `ignoreArrows` (consumed but inert — the
    /// ref guard `0x77b18e`; arrows are the only chord source of `Char` moves).
    Move {
        unit: EditUnit,
        back: bool,
        extend: bool,
    },
    /// Delete one `unit` back/forward from the caret — the selection first when one exists
    /// (every deletion gesture collapses to "delete the selection"). `Edge` going back is the
    /// macOS Cmd+Backspace "clear to start".
    Delete { unit: EditUnit, back: bool },
    /// Select the whole text, caret to the end (the ref's Ctrl+A, `HighlightText(0, -1)`).
    SelectAll,
    /// Recall the previous (older) submitted line into the box (`historyLines`).
    HistoryPrev,
    /// Step back toward the newest line; past it, restore the stashed draft.
    HistoryNext,
}

/// The focused EditBox's per-frame text-UI geometry — returned by
/// [`UiScript::focused_editbox_text_ui`](super::UiScript::focused_editbox_text_ui): everything
/// the host needs to draw the box's window substring, selection highlight, and caret.
#[derive(Clone, Debug, PartialEq)]
pub struct EditBoxTextUi {
    /// The text region's draw target — match the extracted Text quad by it.
    pub target: crate::order::ZTarget,
    /// Draw `display[display_from..]` (the scroll window's start, a DISPLAY byte offset).
    /// Always 0 for a multiline box (it wraps in place instead of windowing).
    pub display_from: usize,
    /// The box is multiline: the host draws the full wrapped block (no window, no unbounded
    /// rect) and seats the caret/selection by `(row, x)` at the answered row pitch.
    pub multi_line: bool,
    /// Caret x in px from the drawn window's text origin (multiline: from the row's origin).
    pub caret_x: f32,
    /// Caret wrapped-row index (always 0 single-line) — the vertical half of the 2-D seat.
    pub caret_row: usize,
    /// The answered row pitch in px (0 until the advance answer lands); the host may prefer its
    /// own draw-side line cell — the two are the same law (the snapped font em).
    pub cell_h: f32,
    /// The blink phase — draw the caret this frame (hosts may pin it on for captures).
    pub caret_on: bool,
    /// The selection as per-row `(row, x0, x1)` px spans from each row's origin, when non-empty
    /// (single-line: at most one span, row 0 — the old scalar span).
    pub selection: Vec<(usize, f32, f32)>,
    /// The selection highlight tint (`SetHighlightColor`; ctor default opaque `0x606060` gray).
    pub highlight_color: [f32; 4],
}

/// One FontString the layout needs host-measured this frame (no explicit height; text present).
/// Returned by [`UiScript::fontstrings_needing_measure`](super::UiScript::fontstrings_needing_measure); answer via [`UiScript::set_measured_text`](super::UiScript::set_measured_text).
#[derive(Clone, Debug, PartialEq)]
pub struct MeasureRequest {
    /// Opaque region id — pass back verbatim.
    pub id: u32,
    pub font: Option<String>,
    pub height: Option<f32>,
    /// The `SetTextHeight` override (see [`RegionData::text_height`]): measured at this height,
    /// uncapped, when present — the drawn size is the measured size (`0x772890`).
    pub text_height: Option<f32>,
    /// Wrap width when the region's width is pinned (explicit `<Size x>`), else single-line.
    pub wrap_width: Option<f32>,
    /// The resolved glyph outline — it biases the client's per-glyph step (+1px, GlyphStepBase
    /// 0x5ca2b0), so the measured width depends on it.
    pub outline: Outline,
    pub text: String,
    /// Cache key — pass back verbatim.
    pub key: u64,
}

/// One ScrollingMessageFrame ring line whose wrapped **row count** needs a host measurement (its
/// cache key went stale — new line, width change, font change). The message-frame half of the
/// measure round-trip: returned by
/// [`UiScript::message_lines_needing_measure`](super::UiScript::message_lines_needing_measure),
/// answered via [`UiScript::set_message_line_rows`](super::UiScript::set_message_line_rows) with
/// `(frame, index, rows, key)` — same-frame, before extract, no re-resolve needed (rows shift only
/// the emitted bands, never the anchor graph).
#[derive(Clone, Debug, PartialEq)]
pub struct LineMeasureRequest {
    /// Opaque frame id — pass back verbatim.
    pub frame: u32,
    /// The line's ring index at collect time — pass back verbatim (the ring cannot shift between
    /// collect and answer within one host drive; the key still guards a stale store).
    pub index: u32,
    pub font: Option<String>,
    pub height: Option<f32>,
    /// The frame's resolved inner width — the wrap constraint.
    pub wrap_width: f32,
    /// The resolved glyph outline (step-law width bias, as on [`MeasureRequest`]).
    pub outline: Outline,
    pub text: String,
    /// Cache key — pass back verbatim.
    pub key: u64,
}
