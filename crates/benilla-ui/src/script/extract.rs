//! [`UiScript::extract`] — the render-list builder (decision 0068): the visible-tree
//! [`crate::order::traversal`] zipped with the resolved rects and per-kind region visuals, in the
//! client's painter order. An `impl UiScript` block beside its concern (the `layout.rs` pattern);
//! the shared ScrollFrame clip it walks lives in [`super::clip`].

use crate::layout::Rect;
use crate::order::{self, ZTarget};
use crate::widget::FrameHandle;

use super::clip::{effective_clip, scroll_clip_sources};
use super::{slider, ExtractedQuad, FontObject, QuadContent, TexCoords, UiScript};

impl UiScript {
    /// **Every live draw target in the VM**, whether or not it is currently visible — each frame's
    /// own slot followed by its region leaves, in arena order.
    ///
    /// [`Self::extract`] answers *what is being drawn*; this answers *what exists*, and the two
    /// together are what let an instrument say **who drew a quad**. Diff this across a load and the
    /// new targets are exactly the ones that load created: no name prefixes, no heuristics, and no
    /// dependence on an addon naming anything at all. That is the oracle behind the addon
    /// harness's render column, and it is written this way because the obvious alternatives are
    /// both wrong — a with/without **quad-count diff** reads zero for an addon that *replaces* a
    /// window rather than adding one (Bagnon takes the bags over), and a **name-prefix** match
    /// cannot see an anonymous frame or a region an addon hangs off one of ours.
    ///
    /// Handles are generational, so a destroyed-and-reused slot can never be mistaken for a
    /// survivor of the baseline.
    pub fn live_targets(&self) -> Vec<ZTarget> {
        let model = self.model_ref();
        let mut out = Vec::new();
        for (fh, frame) in model.arena.iter_frames() {
            out.push(ZTarget::Frame(fh));
            out.extend(frame.regions.iter().copied().map(ZTarget::Region));
        }
        out
    }

    /// The frame a draw target belongs to — itself for a frame slot, the owner for a region.
    ///
    /// The half of the attribution that separates *created a window of its own* from *painted onto
    /// one of ours*: a new `Region` whose owner frame is **not** new is an addon hooking an
    /// existing frame, which is precisely what `!OmniCC` does to a cooldown and what a check built
    /// only on new frames would score as "drew nothing".
    pub fn target_frame(&self, target: ZTarget) -> Option<FrameHandle> {
        match target {
            ZTarget::Frame(fh) => self.model_ref().arena.frame(fh).map(|_| fh),
            ZTarget::Region(rh) => self.model_ref().arena.region(rh).map(|r| r.owner),
        }
    }

    /// A frame's parent, or `None` for a top-level frame (or a stale handle).
    ///
    /// The other half of the attribution walk: a frame being **new** does not make it the addon's
    /// own window — `!OmniCC` creates a brand-new anonymous frame *parented to one of our action
    /// buttons*, and only the chain tells an overlay from a window.
    pub fn frame_parent(&self, frame: FrameHandle) -> Option<FrameHandle> {
        self.model_ref().arena.frame(frame)?.parent
    }

    /// A frame's name, or `None` — it is anonymous, or the handle is stale.
    ///
    /// For reporting only. Nothing above depends on a frame being named; this is what turns an
    /// attributed handle into a row a human can act on.
    pub fn frame_name(&self, frame: FrameHandle) -> Option<String> {
        self.model_ref().arena.frame(frame)?.name.clone()
    }

    /// The nearest **named** frame at or above `target` — its own frame's name, else the closest
    /// named ancestor.
    ///
    /// An addon's slot buttons are usually named and its inner art usually is not, so a bare
    /// [`Self::frame_name`] would report a hole exactly where the interesting quads are. Walking up
    /// answers "which window is this part of", which is the question a report is asking.
    pub fn target_owner_name(&self, target: ZTarget) -> Option<String> {
        let model = self.model_ref();
        let mut frame = match target {
            ZTarget::Frame(fh) => Some(fh),
            ZTarget::Region(rh) => Some(model.arena.region(rh)?.owner),
        };
        while let Some(fh) = frame {
            let f = model.arena.frame(fh)?;
            if let Some(name) = &f.name {
                return Some(name.clone());
            }
            frame = f.parent;
        }
        None
    }

    /// The render list in the client's painter order (decision 0068): the visible-tree
    /// [`crate::order::traversal`] zipped with the resolved rects and region visuals. Already sorted
    /// ascending by `ZKey`. Call [`UiScript::resolve`] first for populated rects.
    pub fn extract(&self) -> Vec<ExtractedQuad> {
        let now = self.now();
        let model = self.model_ref();
        let list = order::traversal(&model.arena);
        let mut out = Vec::with_capacity(list.len());
        // The ScrollFrame clip sources (decision 0112): every live ScrollFrame with a resolved rect
        // and a live child, `child handle → the scrollframe's resolved rect`. Built once per extract;
        // [`effective_clip`] walks a quad's owner up through this to find every ancestor ScrollFrame
        // it is clipped by (nested ScrollFrames intersect).
        let scroll_sources = scroll_clip_sources(&model);
        for (target, zkey) in list {
            let (rect, alpha, content, clip, scale) = match target {
                ZTarget::Frame(fh) => {
                    let frame = model.arena.frame(fh);
                    let alpha = frame.map(|f| f.effective_alpha).unwrap_or(1.0);
                    let scale = frame.map(|f| f.effective_scale).unwrap_or(1.0);
                    let clip = effective_clip(&model, &scroll_sources, fh);
                    // A Minimap widget's own slot carries its zoom out to the app renderer (the
                    // tile/mask/arrow draw — decision 0203); every other frame slot is bare.
                    let content = match frame.map(|f| &f.kind_state) {
                        Some(crate::widget::KindState::Minimap(m)) => QuadContent::Minimap {
                            zoom: m.zoom,
                            inside_zoom: m.inside_zoom,
                        },
                        // A shown Cooldown's phase (the reference machine's derived state): the
                        // sweep scrub while `now < start+duration`, else the 1 s flash's own
                        // progress. `tick` hides the widget at flash end, so a stale slot never
                        // draws (decision 0137 phase 4).
                        Some(crate::widget::KindState::Cooldown(cd)) if cd.duration > 0.0 => {
                            let fraction = ((now - cd.start) / cd.duration) as f32;
                            let flash = (fraction >= 1.0).then(|| {
                                ((now - cd.sweep_end()) / crate::widget::COOLDOWN_FLASH_SECS)
                                    .clamp(0.0, 1.0) as f32
                            });
                            QuadContent::Cooldown { fraction, flash }
                        }
                        _ => QuadContent::Frame,
                    };
                    (
                        model.resolved.get(&fh).copied(),
                        alpha,
                        content,
                        clip,
                        scale,
                    )
                }
                ZTarget::Region(rh) => {
                    let region = model.arena.region(rh);
                    let owner = region.map(|r| r.owner);
                    let owner_frame = owner.and_then(|o| model.arena.frame(o));
                    // Regions clip with their owner frame (decision 0112 §4).
                    let clip = owner.and_then(|o| effective_clip(&model, &scroll_sources, o));
                    let mut rect = owner.and_then(|o| model.resolved.get(&o).copied());
                    // A StatusBar's bar-fill region draws at the value fraction of the frame's rect,
                    // along the orientation axis (horizontal grows rightward, vertical bottom-up). The
                    // bar owns its geometry — it never carries anchors, so it skips the region-rect
                    // precedence below. It also CROPS: see `bar_fill_uv`.
                    let mut bar_fill: Option<&crate::widget::StatusBarState> = None;
                    if let Some(crate::widget::KindState::StatusBar(sb)) =
                        owner_frame.map(|f| &f.kind_state)
                    {
                        if sb.bar == Some(rh) {
                            rect = rect.map(|r| bar_fill_rect(r, sb));
                            bar_fill = Some(sb);
                        }
                    }
                    // A Slider's thumb draws at the value fraction along the track (decision 0250 §4),
                    // centered on the cross-axis — like the bar-fill, it owns its geometry and skips
                    // the region-rect precedence below.
                    let mut thumb_fill = false;
                    if let Some(crate::widget::KindState::Slider(sl)) =
                        owner_frame.map(|f| &f.kind_state)
                    {
                        if sl.thumb == Some(rh) {
                            let tsize = model.region_data.get(&rh).and_then(|d| d.size);
                            rect = rect
                                .map(|r| slider::thumb_rect(r, tsize, sl.vertical, sl.fraction()));
                            thumb_fill = true;
                        }
                    }
                    // A Button's state textures show by interaction state (the texture-array
                    // "current" pointer): a non-current state texture emits no quad this frame.
                    // The ButtonText additionally re-points to the current STATE's font object
                    // (disabled > hovered > normal; hover falls back to normal) — the client's
                    // per-state label font swap (UIPanelButtonTemplate's gold/white/gray trio).
                    let mut state_font: Option<&FontObject> = None;
                    let mut state_color: Option<[f32; 4]> = None;
                    // `Button:SetFont` — the face/size/flags written on the button's own embedded
                    // fonts rather than on any object they inherit.
                    let mut button_font: Option<&crate::widget::ButtonFont> = None;
                    if let Some(crate::widget::KindState::Button(bs)) =
                        owner_frame.map(|f| &f.kind_state)
                    {
                        let hovered = owner.is_some() && model.mouseover == owner;
                        // ANY registered mouse button holds a button down, not only the left one
                        // (`0x77924b`, see `button::wants_press_visual`) — which is what makes a
                        // right-click on a bar or spellbook slot flash its pushed art.
                        let held = owner.is_some_and(|o| super::button::press_held(&model, o));
                        if !bs.region_visible(rh, hovered, held) {
                            continue;
                        }
                        if bs.text == Some(rh) {
                            let name = if !bs.enabled {
                                bs.disabled_font.as_ref()
                            } else if hovered {
                                bs.highlight_font.as_ref().or(bs.normal_font.as_ref())
                            } else {
                                bs.normal_font.as_ref()
                            };
                            state_font = name.and_then(|n| model.font_objects.get(n));
                            button_font = bs.font.as_ref();
                            // The per-state color override (Button:Set*TextColor) — hover falls
                            // back to the normal color, mirroring the font fallback above.
                            state_color = if !bs.enabled {
                                bs.disabled_color
                            } else if hovered {
                                bs.highlight_color.or(bs.normal_color)
                            } else {
                                bs.normal_color
                            };
                        }
                    }
                    let mut data = model.region_data.get(&rh).cloned().unwrap_or_default();
                    // Region-level Hide (the VisibleRegion bit): no quad at all.
                    if data.hidden {
                        continue;
                    }
                    // The single-hop draw multiply (`propagation.md`): the region's own alpha times
                    // its immediate owner's — never a product up the tree, because the owner's own
                    // `effective_alpha` was already overwritten by any ancestor's SetAlpha.
                    let alpha = owner_frame.map(|f| f.effective_alpha).unwrap_or(1.0)
                        * data.alpha.unwrap_or(1.0);
                    if let Some(fo) = state_font {
                        // The font object's paint wholesale, except a color the Lua explicitly
                        // SetTextColor'd (the client's explicitly-set mask keeps it — ui ledger
                        // FONTINSTANCE+0x38, color bit 0x404).
                        data.font_path = fo.font.clone().or(data.font_path);
                        data.font_height = fo.height.or(data.font_height);
                        // Same severance as the colour below: a region that called
                        // `SetShadowColor`/`SetShadowOffset` keeps its own, or the font object it
                        // inherits would silently overwrite the value the addon just set.
                        if !data.font_explicit.shadow {
                            data.font_shadow = fo.shadow.or(data.font_shadow);
                        }
                        data.outline = fo.outline;
                        // Test the severance MASK, which is what the sentence above claims and what
                        // wow-re pinned, not `vertex_color.is_none()`. The nil-check was an
                        // equivalent proxy for exactly as long as an explicit `SetTextColor` was the
                        // only way a button label's colour could be populated at all; the moment
                        // `SetTextFontObject` began linking the label to its font object (so
                        // `GetFont`/`GetFontObject` stop answering nil), a label carried the NORMAL
                        // object's colour and the disabled state's gray silently stopped applying.
                        // Caught by `button_label_repaints_by_state_font_object`, which is the test
                        // that exists for this.
                        if !data.font_explicit.color {
                            data.vertex_color = fo.color.or(data.vertex_color);
                        }
                        // The object's own justify (`<NormalFont inherits=… justifyH="LEFT"/>` —
                        // how the ref left-aligns a ButtonText).
                        if let Some(j) = fo.justify_h {
                            data.justify.set_h(j);
                        }
                        if let Some(j) = fo.justify_v {
                            data.justify.set_v(j);
                        }
                    }
                    // `Button:SetFont` sits BETWEEN the two: it is a local set on the button's own
                    // embedded font, so it outranks the font object that font inherits (a locally
                    // set axis severs inheritance and is never restored — wow-re
                    // `font-object-lua-surface.md`), but it loses to a face the label FontString
                    // set for *itself*, which severs one level further down. That is what
                    // `font_explicit` is, so it is the gate here too.
                    if let Some(bf) = button_font {
                        if !data.font_explicit.face {
                            data.font_path = Some(bf.path.clone());
                        }
                        if !data.font_explicit.height {
                            data.font_height = Some(bf.height);
                        }
                        if !data.font_explicit.outline {
                            data.outline = super::Outline::flags(&bf.flags);
                        }
                    }
                    // The button-level state color wins over the font object AND the region's own
                    // explicit color — the client's Button color slots repaint the label outright.
                    if let Some(c) = state_color {
                        data.vertex_color = Some(c);
                    }
                    // Region-rect precedence (decision 0068 v1): anchored regions resolved in
                    // [`resolve`] → their owner-relative rect; else an explicitly-sized region draws
                    // *centered* on its owner (the state-texture overhang); else it fills the owner
                    // (`rect` already = owner). The bar-fill region keeps its fraction geometry.
                    // The fill CROPS its texture (wow-re `nameplate-vkey.md`, VERIFIED): every
                    // `SetValue` rewrites the region's 4-corner UV block with `u1 = fraction` and
                    // recomputes the quad as `right = left + frac·width`. So the art is sliced, never
                    // squeezed — a bar texture with a horizontal ramp (`UI-StatusBar` brightens 124→166
                    // left-to-right) keeps its true gradient at every fill level.
                    if let Some(sb) = bar_fill {
                        data.tex_coords = Some(bar_fill_uv(data.tex_coords, sb));
                    }
                    if bar_fill.is_none() && !thumb_fill {
                        if let Some(rr) = model.region_resolved.get(&rh) {
                            rect = Some(*rr);
                        } else if let (Some((w, h)), Some(r)) = (data.size, rect) {
                            let (cx, cy) = ((r.left + r.right) * 0.5, (r.bottom + r.top) * 0.5);
                            rect = Some(Rect::new(
                                cy - h * 0.5,
                                cx - w * 0.5,
                                cy + h * 0.5,
                                cx + w * 0.5,
                            ));
                        }
                    }
                    let is_text = matches!(
                        region.map(|r| r.kind),
                        Some(crate::widget::RegionKind::FontString)
                    );
                    let content = if is_text {
                        QuadContent::Text {
                            text: data.text,
                            color: data.vertex_color,
                            // The gx translator's answer, not the getter's: a cleared axis draws
                            // CENTER/MIDDLE (`0x44d420`), where `GetJustifyH` says "UNKNOWN".
                            justify_h: data.justify.paint_h(),
                            justify_v: data.justify.paint_v(),
                            font: data.font_path,
                            font_height: data.font_height,
                            text_height: data.text_height,
                            shadow: data.font_shadow,
                            outline: data.outline,
                            alpha_gradient: data.alpha_gradient,
                        }
                    } else {
                        // The draw gate is the TEXTURE slot, never the colour (`0x7706e0`: `+0xcc`
                        // empty -> emit NOTHING — `texture-color-composition.md` §4, VERIFIED). A
                        // vertex colour is a tint on whatever texture exists; alone it is not
                        // drawable content — it survives `SetTexture(nil)` by design ("a tint
                        // outlives the art it was tinting") and used to leak out of here as a
                        // solid plate the moment the art was cleared (the white action buttons on
                        // a character switch; the 2026-07-10 grey wells were the same class).
                        // A GRADIENT is drawable content in its own right — the client generates it
                        // into the same texture slot the colour form of `SetTexture` fills, so a
                        // region carrying only a gradient still paints. Folded to its midpoint here
                        // because a quad carries ONE tint; the whole gradient stays on the region
                        // (`RegionData::gradient`) so a renderer that grows a second stop needs no
                        // API change. The approximation is visible, and it is stated there and here
                        // rather than discovered later.
                        let fill = data.fill.or_else(|| data.gradient.map(|g| g.midpoint()));
                        let has_texture = data.texture.is_some() || fill.is_some();
                        QuadContent::Texture {
                            path: data.texture,
                            color: has_texture
                                .then(|| texture_color(fill, data.vertex_color))
                                .flatten(),
                            additive: data.additive,
                            tex_coords: data.tex_coords,
                            circular: data.circular,
                            portrait_unit: data.portrait_unit,
                            rotation: data.rotation,
                        }
                    };
                    // A region draws at its owner's scale — same single hop as alpha (a region
                    // has no scale of its own; `propagation.md`'s product lives on frames).
                    let scale = owner_frame.map(|f| f.effective_scale).unwrap_or(1.0);
                    (rect, alpha, content, clip, scale)
                }
            };
            out.push(ExtractedQuad {
                target,
                z: zkey.raw(),
                rect,
                alpha,
                content,
                clip,
                scale,
            });
            // A ScrollingMessageFrame emits, on top of its own (empty) frame slot, one Text quad per
            // visible ring line — stacked bottom-up (newest at the frame's bottom edge), each carrying
            // its live fade alpha in the text color. The generic region path can't do this (the lines
            // aren't declared FontStrings); the ring lives in the frame's kind state.
            if let (ZTarget::Frame(fh), Some(fr)) = (target, rect) {
                // The frame's own draw slot carries its Backdrop, behind its regions (which sort
                // after the frame slot). A ScrollingMessageFrame's ring lines do NOT: they take the
                // slot's ARTWORK *content* key ([`crate::order::ZKey::content`]) — the layer the
                // client's own message font strings live in — so the frame's BACKGROUND textures
                // stay behind them. At the bare slot the chat window's hover box painted over its
                // own messages. Both clip like the frame's own slot (the frame IS the clipped owner
                // when it's a scroll child).
                let paint = super::layout::FramePaint { alpha, scale };
                Self::emit_backdrop(&model, fh, fr, zkey.raw(), paint, clip, &mut out);
                Self::emit_message_lines(
                    &model,
                    fh,
                    fr,
                    zkey.content(order::DrawLayer::Artwork).raw(),
                    paint,
                    clip,
                    &mut out,
                );
            }
        }
        out
    }
}

/// The single colour a Texture region draws with: **`texel × vertexColour`**, per channel and
/// **alpha included** (wow-re `system/ui/scratch/texture-color-composition.md`, VERIFIED — the
/// stage-0 combine's `MODULATE(TEXTURE, DIFFUSE)` for both colour and alpha).
///
/// `fill` is the region's own solid-colour texture ([`RegionData::fill`] — the client generates a
/// real 8×8 texel block from it), so where it is set it IS the texel and the product is the drawn
/// colour. Where it isn't, the texel comes from `path`'s art and this returns the tint alone for the
/// renderer to modulate the sample by; `None` means untinted.
///
/// The correction this encodes: a `<Color 1,1,1,0.2>` trough later `SetVertexColor(0,0,0.75,0.5)`'d
/// draws at alpha **0.1**. benilla used to store one colour slot and let the second call *replace*
/// the first, which drew it at 0.5.
fn texture_color(fill: Option<[f32; 4]>, vertex: Option<[f32; 4]>) -> Option<[f32; 4]> {
    match (fill, vertex) {
        (Some(f), Some(v)) => Some([f[0] * v[0], f[1] * v[1], f[2] * v[2], f[3] * v[3]]),
        (Some(c), None) | (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

/// A StatusBar's bar-fill rect: the frame rect scaled by the value fraction — rightward from the
/// left edge (horizontal) or upward from the bottom (vertical), the documented fill directions
/// (reverse-fill is an Era extension, not modeled yet).
fn bar_fill_rect(r: Rect, sb: &crate::widget::StatusBarState) -> Rect {
    let f = sb.fraction();
    if sb.vertical {
        Rect::new(r.bottom, r.left, r.bottom + r.height() * f, r.right)
    } else {
        Rect::new(r.bottom, r.left, r.top, r.left + r.width() * f)
    }
}

/// A StatusBar's bar-fill UV sub-rect: `base` (its `<TexCoords>`/`SetTexCoord`, or the full texture)
/// sliced to the value fraction along the fill axis — `[left, right, top, bottom]`, 0..1, top-left
/// origin.
///
/// The client CROPS rather than scales (wow-re `nameplate-vkey.md`, VERIFIED): `SetValue`
/// (`0x7cc450`→`0x7833c0`) drives `0x770410`, which writes the 4-corner UV block (`+0x104..+0x120`)
/// with `u1 = GetValue()` and recomputes `right = left + frac·width`. Horizontal is the verified
/// case; VERTICAL mirrors it up the `v` axis (bottom-up fill ⇒ the *bottom* edge of the art is
/// pinned) — inferred from the same block, not separately pinned.
fn bar_fill_uv(base: Option<TexCoords>, sb: &crate::widget::StatusBarState) -> TexCoords {
    // The fill crop is inherently the 4-edge form; an affine base (no live StatusBar uses one)
    // contributes its bounding edges.
    let [l, r, t, b] = base.map(|tc| tc.edges()).unwrap_or([0.0, 1.0, 0.0, 1.0]);
    let f = sb.fraction();
    TexCoords::Rect(if sb.vertical {
        [l, r, b - (b - t) * f, b]
    } else {
        [l, l + (r - l) * f, t, b]
    })
}
