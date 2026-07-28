//! The extraction pass: [`drive_script`] turns [`UiScript::extract`]'s per-frame output into
//! [`UiQuads`] for the render pass (screen size → tick → resolve → extract → quads), including the
//! held-cursor icon overlay ([`cursor_icon_quad`]) — CAPTURE-ONLY since decision 0216 §5, where
//! the held payload's icon became the hardware cursor ([`crate::cursor`]) in a normal run; the
//! quad survives only as the visual harness's machine-checkable view (the OS cursor can't appear
//! in capture pixels). Split out of [`super`] purely for size — the plugin wiring and the input
//! pass live there and in [`super::input`] respectively.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_ui::script::{QuadContent, TexCoords, UiScript};

use crate::assets::WorldAssets;
use crate::ui_pass::{UiQuad, UiQuads, UvRect};
use crate::ui_text::UiFontAtlas;

mod text;

/// Per frame: screen size → `tick` (OnUpdate) → `resolve` → `extract` → [`UiQuads`]. Script errors
/// drain to the log (throttled by being drained — each fires once).
#[allow(clippy::too_many_arguments)] // a Bevy system: each param is one resource, the app's convention
pub(super) fn drive_script(
    script: Option<NonSendMut<UiScript>>,
    window: Query<&Window, With<PrimaryWindow>>,
    // REAL time, deliberately: this drives the VM's `GetTime()` session clock, and the reference
    // clock is the OS wall clock. Bevy's default virtual time clamps every frame delta to
    // `max_delta` (250 ms), so hitches, loading stalls, and macOS occlusion throttling (~1 fps
    // when the window is covered) permanently drop UI-clock time — every GetTime-anchored timer
    // (the cooldown sweep, aura expirations, fades) then runs LONG against the wall-clock
    // cooldown store and the server, which is exactly "I can charge before the sweep ends"
    // (verified live: an occluded run's UI clock fell 27 s behind in 43 s of wall time).
    time: Res<Time<Real>>,
    mut quads: ResMut<UiQuads>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut font_atlas: Option<ResMut<UiFontAtlas>>,
    // The token -> off-screen-baked-face bridge a `SetPortraitTexture`-bound region samples.
    portraits: Res<crate::portrait::PortraitImages>,
    // Gates the held-cursor icon quad (decision 0216 §5): CAPTURE-ONLY, the same presence check
    // every other capture-only system uses (`ui_script::capture_ui_active`'s sibling pattern).
    capture: Option<Res<crate::capture::CaptureMode>>,
    // This frame's `<Minimap>` widget slot, parked for `minimap::emit_minimap` (the UiQuadAppend
    // producer that fills the hole with tile/arrow quads — decision 0203 phase 1).
    mut minimap_widget: ResMut<crate::minimap::MinimapWidget>,
    // Whether the digit-advance feed has run (once, on the first frame the atlas exists — the
    // atlas itself bakes once and never re-scales, see `load_fonts_and_build_atlas`).
    mut digits_fed: Local<bool>,
    // The uiScale dial folded into the seam scale (decision 0584).
    ui_scale: Res<super::UiScaleCvar>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Ok(window) = window.single() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    // The 768-virtual UI space (decision 0582 — byte law: the client's FrameXML space is ALWAYS
    // 768 units tall, every aspect, mapped to the window; wow-re ui.md's converter identity
    // `f(screenH) = 768` + the caret `H_px/192` law), times the uiScale dial (decision 0584 —
    // the VM's screen is `768/uiScale` units tall, so a dial below 1 shrinks everything). The VM
    // lives entirely in that space: this seam scales quads ×s on the way out, mouse ÷s on the
    // way in (input.rs), and measures ÷s on the way back. At a 768-tall window with the dial at
    // 1 the whole pipeline is bit-identical to the pre-virtual behavior.
    let s = super::seam_scale(h, ui_scale.0);
    // Phase spans (visible under `bevy/trace_chrome`): this system is the biggest flat CPU cost
    // on an idle frame, and the ledger can only rank what has a name — tick (Lua OnUpdate),
    // resolve (layout), measure (the text round-trips), extract (tree walk + rasterize), diff.
    {
        let _span = bevy::log::info_span!("ui_script: tick").entered();
        script.set_screen_size(w / s, if h > 0.0 { h / s } else { 768.0 });
        script.tick(time.delta_secs());
    }
    {
        let _span = bevy::log::info_span!("ui_script: resolve").entered();
        script.resolve();
    }
    let measure_span = bevy::log::info_span!("ui_script: measure").entered();
    // The digit-advance feed (the synchronous half of the money layout's metrics): measure
    // NumberFontNormal's '0'..'9' once per atlas scale and push them as `BENILLA_DIGIT_W`, so
    // `BenillaMoney_Set` can size its coin slots to their numbers *inside* one update — the real
    // SmallMoneyFrame's GetTextWidth-mid-update shrink (MoneyFrame.lua l.202-269), which the
    // frame-late GetStringWidth round-trip below can't serve. The font pairing comes from the
    // registered font object (Fonts.xml stays the single source of truth).
    if let Some(atlas) = font_atlas.as_deref_mut() {
        if !*digits_fed {
            if let Some(number_font) = script.font_object("NumberFontNormal") {
                let adv = crate::ui_text::digit_advances(
                    atlas,
                    crate::ui_text::FontSpec {
                        path: number_font.font.as_deref(),
                        // The one-to-one drawn px (cap inert at NumberFontNormal's 14) × the
                        // virtual scale; the widths divide back to UI units below.
                        height: crate::ui_text::drawn_px(number_font.height, None, s),
                        // The TRUE outline (NumberFontNormal is NORMAL-outlined): it selects the
                        // baked ring cell; only a THICK outline also biases the step law
                        // (`outline-bake-tint.md`) — either way the fed digit widths match what
                        // the render pass actually lays.
                        outline: number_font.outline,
                        paint_halo: true,
                        alpha_gradient: None,
                    },
                );
                let adv = adv.map(|a| a / s);
                script.set_digit_advances(&adv);
                *digits_fed = true;
            }
        }
    }
    // The measure round-trip (the layout↔font-engine seam): height-less FontStrings ask the host
    // for their wrapped text size; answer from the atlas and resolve again so anchors hanging off
    // a text's bottom (the gossip option rows → the greeting) land this same frame. The cache keys
    // keep the request list empty on quiet frames, so the second resolve is rare.
    if let Some(atlas) = font_atlas.as_deref_mut() {
        let requests = script.fontstrings_needing_measure();
        if !requests.is_empty() {
            let measures: Vec<(u32, f32, f32, u64)> = requests
                .iter()
                .map(|r| {
                    let (w, h) = crate::ui_text::measure_text(
                        atlas,
                        &r.text,
                        r.wrap_width.map(|w| w * s),
                        crate::ui_text::FontSpec {
                            path: r.font.as_deref(),
                            // The render pass's exact drawn px (two regimes × the virtual
                            // scale) — measure == render, and Lua's GetStringWidth/Height echo
                            // the DRAWN size (0x772890); results divide back to UI units below.
                            height: crate::ui_text::drawn_px(r.height, r.text_height, s),
                            // The TRUE outline: THICK biases the client's step law (+1px per
                            // glyph — GlyphStepBase 0x5ca2b0, THICK-only per outline-bake-tint.md)
                            // and any outline adds the +2r line pitch, so measure must see it.
                            outline: r.outline,
                            paint_halo: true, // measure never paints; irrelevant here
                            alpha_gradient: None, // alpha never changes metrics
                        },
                    );
                    (r.id, w / s, h / s, r.key)
                })
                .collect();
            script.set_measured_text(&measures);
            script.resolve();
        }
    }
    // The message-line half of the round-trip: chat ring lines ask for their wrapped ROW COUNT at
    // the frame's resolved width, so the emit pass can stack real content heights (a long line
    // pushes older lines up instead of overlapping them). Same-frame like the FontString half, but
    // no re-resolve — rows shift only the emitted bands, never the anchor graph.
    if let Some(atlas) = font_atlas.as_deref_mut() {
        let requests = script.message_lines_needing_measure();
        if !requests.is_empty() {
            let rows: Vec<(u32, u32, u16, u64)> = requests
                .iter()
                .map(|r| {
                    let n = crate::ui_text::measure_wrapped_rows(
                        atlas,
                        &r.text,
                        r.wrap_width * s,
                        crate::ui_text::FontSpec {
                            path: r.font.as_deref(),
                            // The band's own drawn px (one-to-one regime; inert ≤ 32).
                            height: crate::ui_text::drawn_px(r.height, None, s),
                            outline: r.outline, // step-law width bias, as in the measures above
                            paint_halo: true,   // measure never paints; irrelevant here
                            alpha_gradient: None,
                        },
                    );
                    (r.frame, r.index, n, r.key)
                })
                .collect();
            script.set_message_line_rows(&rows);
        }
    }
    drop(measure_span);
    for err in script.take_errors() {
        warn!("ui_script: {err}");
    }
    for w in script.take_warnings() {
        warn!("ui_script: {w}");
    }

    let extract_span = bevy::log::info_span!("ui_script: extract").entered();
    let mut out = Vec::new();
    let mut assets = world_assets;
    let mut minimap_slot = None;
    // Hyperlink spans collected while rasterizing message-line Text quads (frame-targeted only —
    // chat lines; FontString-region links are a later arc), fed back to the engine's click
    // hit-test after the loop. Rects flip back to the engine's y-up space.
    let mut link_spans: Vec<(
        benilla_ui::widget::FrameHandle,
        benilla_ui::layout::Rect,
        String,
        String,
    )> = Vec::new();
    // The EditBox advance-table answer (the metrics half of the mouse/selection law): the
    // focused box's display string measured per-byte under the same shaping + step law as its
    // draw, so the engine's click→index, drag-select, and scroll window land exactly on the
    // glyphs. (The request reads the region's load-resolved font; the transient state-font
    // overlay below never applies to an editbox text region.)
    if let Some(atlas) = font_atlas.as_deref_mut() {
        if let Some(req) = script.editbox_advances_request() {
            let spec = crate::ui_text::FontSpec {
                path: req.font.as_deref(),
                height: crate::ui_text::drawn_px(req.height, None, s),
                outline: req.outline,
                paint_halo: true,     // measure never paints
                alpha_gradient: None, // alpha never changes metrics
            };
            let cum: Vec<f32> = crate::ui_text::line_advances(atlas, &req.text, spec)
                .iter()
                .map(|a| a / s)
                .collect();
            // A multiline box also gets its wrapped-row starts + row pitch — the same wrap pass
            // the draw uses, so the engine's (row, x) caret/click law lands on the drawn rows.
            // Advances/pitch return in UI units (÷s): the engine's click→index and row math
            // compare them against the ÷s mouse feed.
            let (rows, cell_h) = match req.wrap_width {
                Some(w) => crate::ui_text::line_rows(atlas, &req.text, w * s, spec),
                None => (vec![0], 0.0),
            };
            script.set_editbox_advances(req.id, req.key, cum, rows, cell_h / s);
        }
    }
    // The focused edit box's text-UI geometry (RF-0082 leaves caret/highlight geometry to the
    // host): which Text quad is the box's, the scroll window to draw, and the caret/selection
    // x-spans within it — advance-derived engine-side (the blink phase too, `0x77a790`'s 0.5 s
    // law in the engine tick), matched in the Text arm below.
    let text_ui = script.focused_editbox_text_ui();
    for eq in script.extract() {
        let Some(r) = eq.rect else { continue };
        // WoW UI space is y-up from the bottom-left in 768-virtual units; the quad pass is
        // y-down window px from the top-left — scale ×s, then flip through the window height.
        let rect = Rect::new(r.left * s, h - r.top * s, r.right * s, h - r.bottom * s);
        // The ScrollFrame clip (decision 0112), through the same conversion as `rect` —
        // `UiQuad::clip` is the CPU-clip stand-in `ui_pass` already applies uniformly to
        // every quad (texture, backdrop, and glyph alike), so this is the entire app-side plumb.
        let clip = eq
            .clip
            .map(|c| Rect::new(c.left * s, h - c.top * s, c.right * s, h - c.bottom * s));
        match eq.content {
            // Frames draw nothing themselves in v1 (regions carry the visuals).
            QuadContent::Frame => continue,
            // The `<Minimap>` widget's content hole: parked for the minimap renderer (an
            // UiQuadAppend producer), which fills it at this exact z — the widget slot itself
            // emits nothing here (decision 0203 phase 1).
            QuadContent::Minimap { zoom, inside_zoom } => {
                minimap_slot = Some(crate::minimap::MinimapSlot {
                    rect,
                    z: eq.z,
                    zoom,
                    inside_zoom,
                    alpha: eq.alpha,
                });
                continue;
            }
            // The Cooldown widget's pie wipe + finish flash (decision 0137 phase 4) — the
            // byte-pinned look of `UI-Cooldown-Indicator.m2`, rebuilt natively (see
            // [`cooldown_quads`]).
            QuadContent::Cooldown { fraction, flash } => {
                cooldown_quads(
                    rect,
                    eq.z,
                    eq.alpha,
                    fraction,
                    flash,
                    clip,
                    &mut assets,
                    &mut images,
                    &mut out,
                );
                continue;
            }
            QuadContent::Texture {
                path,
                color,
                additive,
                tex_coords,
                circular,
                portrait_unit,
                rotation,
            } => {
                // A live unit portrait (`SetPortraitTexture(region, unit)`): sample this token's
                // source ([`crate::portrait::PortraitImages`]) — the off-screen model bake, or the
                // ref's 2D TemporaryPortrait stand-in while the model streams in. Absent entry (no
                // booth yet) draws nothing rather than the run-splitter's white default.
                if let Some(token) = &portrait_unit {
                    use crate::portrait::PortraitSource;
                    let handle = match portraits.0.get(token) {
                        Some(PortraitSource::Live(h)) => Some(h.clone()),
                        Some(PortraitSource::File(p)) => assets
                            .as_mut()
                            .and_then(|a| a.sprite_texture(p, &mut images)),
                        None => None,
                    };
                    let Some(handle) = handle else {
                        continue;
                    };
                    out.push(UiQuad {
                        rect,
                        z_key: eq.z,
                        texture: Some(handle),
                        // A portrait binding honours `<TexCoords>`/`SetTexCoord` like any other
                        // texture region — the bake is just the sampled image. The ref crops one
                        // this way: the character micro button samples the same portrait slot as
                        // the unit frame through a narrow (0.2..0.8, 0.0666..0.9) window, and
                        // swaps that window when the button is pushed. This branch used to pin
                        // UvRect::FULL, which silently squashed the whole square bake into
                        // whatever rect the region had.
                        uv: match tex_coords {
                            Some(TexCoords::Rect(edges)) => UvRect::from_tex_coords(edges),
                            Some(TexCoords::Corners(corners)) => UvRect::from_corners(corners),
                            None => UvRect::FULL,
                        },
                        color: [1.0, 1.0, 1.0, eq.alpha],
                        // The binding's mask flag: `SetPortraitTexture` regions cut the inscribed
                        // circle (the ref stamps the same shape into its 64² bake's alpha — and
                        // rounds the square stand-in art the same way); `BenillaSetBoothTexture`
                        // (the paper-doll model pane, decision 0208 §5) samples the bake square.
                        circular,
                        clip,
                        ..default()
                    });
                    continue;
                }
                // An unset texture region (no file, no color — e.g. a cleared icon) draws nothing;
                // defaulting it to white would paint phantom quads.
                if path.is_none() && color.is_none() {
                    continue;
                }
                // A portrait region samples the circular-masked variant so the square icon/model
                // doesn't poke past the frame ring's thin band (SetPortraitToTexture, decision 0084).
                let handle = path.as_deref().and_then(|p| {
                    assets.as_mut().and_then(|a| {
                        if circular {
                            a.portrait_texture(p, &mut images)
                        } else {
                            a.sprite_texture(p, &mut images)
                        }
                    })
                });
                // A pathless Texture region is a solid color; a textured one tints by it.
                let mut color = color.unwrap_or([1.0, 1.0, 1.0, 1.0]);
                color[3] *= eq.alpha;
                // `<TexCoords>`/`SetTexCoord` slices the sampled sub-rect. The 4-edge form
                // `[left,right,top,bottom]` maps to raw UV corners (`left→u0, right→u1, top→v0,
                // bottom→v1`) — carried through [`UvRect`] rather than a normalized `Rect` so a
                // mirrored slice (`left>right`, e.g. PlayerFrameTexture) keeps its flip to the
                // vertex buffer. The 8-arg affine form is already per-corner in the `push_quad`
                // winding (the route-line quads). Absent = the full texture.
                let uv = match tex_coords {
                    Some(TexCoords::Rect(edges)) => UvRect::from_tex_coords(edges),
                    Some(TexCoords::Corners(corners)) => UvRect::from_corners(corners),
                    None => UvRect::FULL,
                };
                out.push(UiQuad {
                    rect,
                    z_key: eq.z,
                    texture: handle,
                    uv,
                    color,
                    additive,
                    clip,
                    // The engine's SetRotation is counterclockwise-positive; the quad pass spins
                    // clockwise-on-screen (`UiQuad::rotation`) — negate to convert.
                    rotation: -rotation,
                    ..default()
                });
            }
            QuadContent::Backdrop {
                path,
                color,
                uvs,
                tile,
            } => {
                // A frame Backdrop piece (bg or one of the 8 border pieces). Repeat-sampled when
                // `tile` (edges tile past UV 1; a tiled bg wraps) — its own GPU image + cache
                // (`sprite_texture_tiled`) since clamp/repeat bake into the `Image`. The four UVs are
                // explicit per-corner (the rotated TOP/BOTTOM edges), so they go straight to `UvRect`.
                let handle = assets.as_mut().and_then(|a| {
                    if tile {
                        a.sprite_texture_tiled(&path, &mut images)
                    } else {
                        a.sprite_texture(&path, &mut images)
                    }
                });
                // No texture (missing BLP) ⇒ draw nothing rather than a phantom solid quad.
                let Some(handle) = handle else { continue };
                let mut color = color;
                color[3] *= eq.alpha;
                out.push(UiQuad {
                    rect,
                    z_key: eq.z,
                    texture: Some(handle),
                    uv: UvRect::from_corners(uvs),
                    color,
                    clip,
                    ..default()
                });
            }
            QuadContent::Text {
                text,
                color,
                justify_h,
                justify_v,
                font,
                font_height,
                text_height,
                shadow,
                outline,
                alpha_gradient,
            } => {
                // No atlas (no client data / Friz Quadrata unreadable — see `ui_text`'s startup
                // system) means text simply doesn't render, same graceful-absence posture as a
                // missing `WorldAssets`. The rasterization itself (editbox window, ellipsis,
                // shadow, links, caret) lives in `text::emit`.
                let (Some(atlas), Some(text)) = (font_atlas.as_deref_mut(), text) else {
                    continue;
                };
                text::emit(
                    atlas,
                    &text,
                    text::TextStyle {
                        color,
                        justify_h,
                        justify_v,
                        font,
                        font_height,
                        text_height,
                        shadow,
                        outline,
                        alpha_gradient,
                    },
                    text::TextHost {
                        z: eq.z,
                        alpha: eq.alpha,
                        target: eq.target,
                        rect,
                        clip,
                        ebox: text_ui.as_ref(),
                        screen_h: h,
                        scale: s,
                        caret_pinned: capture.is_some(),
                    },
                    &mut out,
                    &mut link_spans,
                );
            }
        }
    }
    // The item held on the cursor draws last (a 32×32 icon at the mouse, over the whole UI) — but
    // ONLY in capture mode (decision 0216 §5): a normal run shows it as the hardware cursor
    // instead (`crate::cursor`), which can't appear in a screenshot's pixels, so the quad is the
    // capture harness's stand-in. Purely visual either way — the drag state lives in the engine;
    // the wire settles the actual move.
    if capture.is_some() {
        if let (Some(cursor), Some(pos)) = (script.cursor_item(), window.cursor_position()) {
            if let Some(path) = cursor.texture.as_deref() {
                if let Some(handle) = assets
                    .as_mut()
                    .and_then(|a| a.sprite_texture(path, &mut images))
                {
                    out.push(cursor_icon_quad(pos, handle));
                }
            }
        }
    }

    // The rasterized hyperlink spans replace last frame's set — the engine's release dispatch
    // (`OnHyperlinkClick`) hit-tests against exactly what is on screen.
    script.set_link_spans(link_spans);

    // Park this frame's Minimap widget slot (or clear it — a hidden cluster extracts nothing);
    // `minimap::emit_minimap` runs later in the frame (UiQuadAppend) and fills the hole.
    minimap_widget.0 = minimap_slot;
    drop(extract_span);

    let _span = bevy::log::info_span!("ui_script: diff").entered();
    if quads.quads != out {
        quads.quads = out;
        quads.dirty = true;
    }
}

/// The held-cursor icon quad (CAPTURE-ONLY — see the module doc): a 32×32 icon TOP-LEFT anchored
/// at the mouse (`pos`, y-down logical px — the same space as the extracted quad rects and the
/// hardware cursor's own coordinate origin), `[pos, pos + 32]` — matching the hardware cursor's
/// `(0, 0)` hotspot (decision 0216 §5): the pointer sits at the icon's top-left corner and the
/// icon hangs down-right, the reference look, rather than centering on the mouse. Seated above the
/// whole UI (`z_key = u64::MAX` sorts last → drawn on top). Pure geometry so it's
/// machine-checkable without a live mouse.
fn cursor_icon_quad(pos: Vec2, texture: Handle<Image>) -> UiQuad {
    const SIZE: f32 = 32.0;
    UiQuad {
        rect: Rect::new(pos.x, pos.y, pos.x + SIZE, pos.y + SIZE),
        z_key: u64::MAX,
        texture: Some(texture),
        ..default()
    }
}

/// The cooldown wipe's paint: flat black at α = 0x99/255 — the byte content of
/// `Interface\Cooldown\cooldown.blp` (a uniform 32² black texel field at alpha 0x99, with only a
/// transparent clamp column for the sweep edge), constant through the sweep (the model's
/// quadrant color-alpha tracks hold 1.0 for all of sequence 0 — the m2anim key dump).
const COOLDOWN_WIPE_ALPHA: f32 = 153.0 / 255.0;

/// The finish-flash star's bone-scale pulse — sequence 1's 5 keys, byte-read off
/// `UI-Cooldown-Indicator.m2` with the `m2anim` bone-scale dump (uniform XYZ, so one scalar per
/// key): grow to 1.85× with the alpha rise, then a shrinking double-bounce while it fades.
const STAR_SCALE_KEYS: [(f32, f32); 5] = [
    (0.0, 1.0),
    (0.333, 1.853),
    (0.666, 1.305),
    (0.833, 1.605),
    (1.0, 1.155),
];

/// Linear sample of [`STAR_SCALE_KEYS`] at flash progress `t` (the M2 linear-track read).
fn star_scale(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    for w in STAR_SCALE_KEYS.windows(2) {
        let ((t0, v0), (t1, v1)) = (w[0], w[1]);
        if t <= t1 {
            return v0 + (v1 - v0) * ((t - t0) / (t1 - t0));
        }
    }
    STAR_SCALE_KEYS[4].1
}

/// The Cooldown widget's pixels (decision 0137 phase 4): the radial dark wipe + the finish flash,
/// rebuilt natively from the byte-read `UI-Cooldown-Indicator.m2`.
///
/// **The wipe.** The model draws 4 quadrant quads whose UVs rotate over a clamp-edged
/// half-transparent texture — 2001's way of sweeping a straight edge through each quadrant; with
/// a *flat* texture behind it (`cooldown.blp` is uniform black at α 0x99), the composed result is
/// exactly a uniform dark pie whose bright sector grows **clockwise from 12 o'clock** as the
/// cooldown elapses. Rebuilt as ≤4 quads: one full-dark rect per still-covered quadrant, plus
/// the active quadrant's **wedge** — an exact convex fan (`c → ray exit → [outer corner] →
/// quadrant-end edge midpoint`) through [`UiQuad::corners`]. Pixel-equivalent, no scissor
/// interplay. (The wedge bypasses the scroll clip — no cooldown sits in a ScrollFrame; the
/// full-dark rects still carry it.)
///
/// **The flash.** The model's sequence 1 (authored 1.000 s): the additive `star4` burst whose
/// alpha is the byte-read texture-weight ramp — linear 0→1 over the first third, hold 1 to the
/// half, linear 1→0 over the back half — scaled by the star bone's byte-read 5-key pulse
/// ([`STAR_SCALE_KEYS`], the `m2anim` bone-scale dump): grow to 1.85× with the alpha rise, then
/// a shrinking double-bounce as it fades.
#[allow(clippy::too_many_arguments)] // one draw arm's full input set
fn cooldown_quads(
    rect: Rect,
    z_key: u64,
    frame_alpha: f32,
    fraction: f32,
    flash: Option<f32>,
    clip: Option<Rect>,
    assets: &mut Option<ResMut<WorldAssets>>,
    images: &mut Assets<Image>,
    out: &mut Vec<UiQuad>,
) {
    let c = rect.center();
    let dark = [0.0, 0.0, 0.0, COOLDOWN_WIPE_ALPHA * frame_alpha];

    if let Some(progress) = flash {
        // The finish flash: the byte-read weight ramp (keys 0→1 over 0..⅓, hold to ½, →0 at 1).
        let ramp = if progress < 1.0 / 3.0 {
            progress * 3.0
        } else if progress < 0.5 {
            1.0
        } else {
            (1.0 - progress) * 2.0
        };
        let handle = assets
            .as_mut()
            .and_then(|a| a.sprite_texture("Interface\\Cooldown\\star4", images));
        if let Some(handle) = handle {
            let s = star_scale(progress);
            let half = rect.half_size() * s;
            out.push(UiQuad {
                rect: Rect::from_center_half_size(c, half),
                z_key,
                texture: Some(handle),
                color: [1.0, 1.0, 1.0, ramp * frame_alpha],
                additive: true,
                clip,
                ..default()
            });
        }
        return;
    }

    // The sweep: bright sector [0, θ) clockwise from 12 o'clock; quadrants wholly past the edge
    // are full-dark rects, the active one carries the exact wedge.
    let theta = fraction.clamp(0.0, 1.0) * std::f32::consts::TAU;
    let active = ((theta / std::f32::consts::FRAC_PI_2) as usize).min(3);
    // Clockwise from 12 in y-down screen space: top-right, bottom-right, bottom-left, top-left.
    let quadrant = |k: usize| match k {
        0 => Rect::new(c.x, rect.min.y, rect.max.x, c.y),
        1 => Rect::new(c.x, c.y, rect.max.x, rect.max.y),
        2 => Rect::new(rect.min.x, c.y, c.x, rect.max.y),
        _ => Rect::new(rect.min.x, rect.min.y, c.x, c.y),
    };
    for k in (active + 1)..4 {
        out.push(UiQuad {
            rect: quadrant(k),
            z_key,
            color: dark,
            clip,
            ..default()
        });
    }

    // The active quadrant's wedge. The sweep ray from c at angle θ has direction
    // d(θ) = (sin θ, −cos θ) (y-down); per quadrant it can exit through two edges — the "first"
    // one (adjacent to the sweep's entry axis, whose crossing keeps the quadrant's outer corner
    // dark) or the "second" (past the corner). Vertices fan from c, clockwise:
    // c → E (ray exit) → [K (outer corner) if the ray exits the first edge] → M (the
    // quadrant-end axis point). A three-vertex wedge repeats M (the fan's degenerate second
    // triangle draws nothing).
    let d = Vec2::new(theta.sin(), -theta.cos());
    // Per active quadrant: (first-edge t, second-edge t, K, M). A division by ±0 yields ±inf,
    // which the min-pick handles (the boundary angles resolve to the finite edge).
    let (t_first, t_second, k_corner, m_point) = match active {
        0 => (
            (rect.min.y - c.y) / d.y,
            (rect.max.x - c.x) / d.x,
            Vec2::new(rect.max.x, rect.min.y),
            Vec2::new(rect.max.x, c.y),
        ),
        1 => (
            (rect.max.x - c.x) / d.x,
            (rect.max.y - c.y) / d.y,
            Vec2::new(rect.max.x, rect.max.y),
            Vec2::new(c.x, rect.max.y),
        ),
        2 => (
            (rect.max.y - c.y) / d.y,
            (rect.min.x - c.x) / d.x,
            Vec2::new(rect.min.x, rect.max.y),
            Vec2::new(rect.min.x, c.y),
        ),
        _ => (
            (rect.min.x - c.x) / d.x,
            (rect.min.y - c.y) / d.y,
            Vec2::new(rect.min.x, rect.min.y),
            Vec2::new(c.x, rect.min.y),
        ),
    };
    let e_point = c + d * t_first.min(t_second);
    let corners = if t_first <= t_second {
        [c, e_point, k_corner, m_point]
    } else {
        [c, e_point, m_point, m_point]
    };
    out.push(UiQuad {
        rect: quadrant(active),
        z_key,
        color: dark,
        corners: Some(corners),
        ..default()
    });
}

#[cfg(test)]
mod cooldown_quad_tests {
    use super::{cooldown_quads, star_scale, STAR_SCALE_KEYS};
    use bevy::prelude::*;

    /// Emit the sweep for `fraction` on a unit-friendly 40×40 button at (0,0)..(40,40).
    fn sweep(fraction: f32) -> Vec<crate::ui_pass::UiQuad> {
        let mut out = Vec::new();
        cooldown_quads(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            7,
            1.0,
            fraction,
            None,
            None,
            &mut None,
            &mut Assets::<Image>::default(),
            &mut out,
        );
        out
    }

    /// Total dark coverage of the emitted quads (rect quads + the corner wedge's shoelace area).
    fn dark_area(quads: &[crate::ui_pass::UiQuad]) -> f32 {
        quads
            .iter()
            .map(|q| match q.corners {
                None => q.rect.width() * q.rect.height(),
                Some(c) => {
                    // Shoelace over the fan's two triangles (0,1,2) + (0,2,3).
                    let tri = |a: Vec2, b: Vec2, d: Vec2| ((b - a).perp_dot(d - a) / 2.0).abs();
                    tri(c[0], c[1], c[2]) + tri(c[0], c[2], c[3])
                }
            })
            .sum()
    }

    /// The square-cornered pie's exact per-phase geometry (a square pie's area is not linear in
    /// the swept angle, so the checkpoints below are the analytically known shapes: full, the
    /// 45° corner ray, the 180° half, the end sliver).
    #[test]
    fn cooldown_sweep_geometry_is_the_exact_square_pie() {
        // Fraction 0: everything dark — 3 full quadrant rects + a wedge covering the fourth.
        let q0 = sweep(0.0);
        assert_eq!(q0.len(), 4);
        assert!(
            (dark_area(&q0) - 1600.0).abs() < 1e-2,
            "fully dark at start"
        );

        // Fraction 1/8 (45° — the ray through quadrant 0's outer corner): the bright half of
        // quadrant 0 is gone; the wedge is the triangle {center, corner, right-edge midpoint}.
        let q = sweep(0.125);
        assert_eq!(q.len(), 4);
        assert!(
            (dark_area(&q) - (1600.0 - 200.0)).abs() < 1.0,
            "45°: half of one 400px² quadrant is bright, got {}",
            dark_area(&q)
        );

        // Fraction 0.5 (180°): the right half is bright — two full-dark quadrants + a wedge
        // that spans exactly quadrant 2 (t ties pick the corner-inclusive fan; area 400).
        let q = sweep(0.5);
        assert!((dark_area(&q) - 800.0).abs() < 1.0, "half bright at 180°");

        // Fraction ~1: a sliver — near-zero dark area, no full quadrants left.
        let q = sweep(0.999);
        assert!(dark_area(&q) < 20.0, "almost done — a sliver remains");
        assert_eq!(q.len(), 1, "only the active quadrant's wedge remains");

        // The wedge always fans from the button center.
        for f in [0.05, 0.3, 0.6, 0.9] {
            let q = sweep(f);
            let wedge = q
                .iter()
                .find_map(|u| u.corners)
                .expect("a wedge each phase");
            assert_eq!(wedge[0], Vec2::new(20.0, 20.0), "fan apex at the center");
        }
    }

    /// The star pulse samples its byte-read keys exactly and lerps between them — the ⅓ peak
    /// coincides with the alpha ramp's own peak.
    #[test]
    fn star_scale_follows_the_five_key_curve() {
        for (t, v) in STAR_SCALE_KEYS {
            assert!((star_scale(t) - v).abs() < 1e-4, "key at {t}");
        }
        // Midway up the rise: halfway between 1.0 and 1.853.
        let mid = star_scale(0.333 / 2.0);
        assert!((mid - (1.0 + 1.853) / 2.0).abs() < 1e-2, "got {mid}");
        // Clamped outside the band.
        assert!((star_scale(-1.0) - 1.0).abs() < 1e-4);
        assert!((star_scale(2.0) - 1.155).abs() < 1e-4);
    }

    /// The flash phase draws no dark pie — only the additive star (skipped here: no assets in a
    /// unit test — the pie's absence is the machine-checkable half).
    #[test]
    fn cooldown_flash_phase_emits_no_dark_pie() {
        let mut out = Vec::new();
        cooldown_quads(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            7,
            1.0,
            1.2,
            Some(0.4),
            None,
            &mut None,
            &mut Assets::<Image>::default(),
            &mut out,
        );
        assert!(out.is_empty(), "no assets → no star; and never a dark quad");
    }
}

#[cfg(test)]
mod cursor_quad_tests {
    use super::cursor_icon_quad;
    use bevy::prelude::*;

    #[test]
    fn cursor_icon_quad_is_32px_top_left_anchored_at_the_hotspot() {
        let q = cursor_icon_quad(Vec2::new(100.0, 200.0), Handle::default());
        // 32×32, top-left anchored on the mouse (the hardware cursor's (0,0) hotspot) — hangs
        // down-right: [100..132, 200..232].
        assert_eq!(
            (q.rect.min.x, q.rect.min.y, q.rect.max.x, q.rect.max.y),
            (100.0, 200.0, 132.0, 232.0)
        );
        assert_eq!(q.rect.width(), 32.0);
        assert_eq!(q.rect.height(), 32.0);
        // Above every frame, and textured (straight-alpha, not additive).
        assert_eq!(q.z_key, u64::MAX);
        assert!(q.texture.is_some());
        assert!(!q.additive);
    }
}

/// The app-side half of the ScrollFrame clip plumb (decision 0112): does a [`UiQuad`] built from a
/// clipped [`QuadContent::Texture`] actually carry `clip` through `drive_script`? Drives the real
/// system in a minimal headless `App` (no `DefaultPlugins` — just the resources `drive_script`
/// reads), rather than re-deriving the y-up→y-down flip by hand: that flip is exactly the seam this
/// test exists to catch a regression in.
#[cfg(test)]
mod clip_plumb_tests {
    use bevy::math::Rect;
    use bevy::prelude::*;
    use bevy::window::PrimaryWindow;

    use benilla_ui::script::UiScript;

    use super::{drive_script, UiQuad, UiQuads};
    use crate::portrait::PortraitImages;

    /// A headless app with exactly the resources/entities `drive_script` reads: a `NonSend` VM
    /// carrying a ScrollFrame + scrolled-out child + a colored (pathless) Texture region marker, a
    /// 1024×768 primary window (the 768-virtual design height — s = 1, so the WoW-space rects are
    /// known by hand; decision 0582), and every other resource
    /// left at its default (`WorldAssets`/`UiFontAtlas` absent — no BLP/font pipeline needed to prove
    /// the clip carries through a plain colored quad).
    fn app_with_scrolled_marker() -> App {
        let script = UiScript::new().unwrap();
        script
            .run(
                r#"
            local frame = CreateFrame("ScrollFrame", "SF")
            frame:SetPoint("TOPLEFT", 0, -100)  -- screen top 768 -> frame top 668
            frame:SetSize(300, 200)             -- frame bottom 300, right 300
            local child = CreateFrame("Frame", "Child")
            child:SetSize(300, 600)
            frame:SetScrollChild(child)
            local marker = child:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(1, 0, 0)          -- pathless colored quad: no BLP asset needed
        "#,
            )
            .unwrap();

        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<Time>();
        app.init_resource::<Time<Real>>();
        // The uiScale dial at its identity Default (1.0) — tests pin the byte-identity base;
        // the shipped app inserts the taste default instead (decision 0584).
        app.init_resource::<crate::ui_script::UiScaleCvar>();
        app.world_mut().spawn((
            Window {
                resolution: UVec2::new(1024, 768).into(),
                ..default()
            },
            PrimaryWindow,
        ));
        app.add_systems(Update, drive_script);
        app
    }

    #[test]
    fn a_clipped_texture_quad_carries_its_clip_into_the_uiquad() {
        let mut app = app_with_scrolled_marker();
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let marker: &UiQuad = quads
            .iter()
            .find(|q| q.color[0] == 1.0 && q.color[1] == 0.0 && q.color[2] == 0.0)
            .expect("the colored marker quad extracted");

        // WoW space: frame rect [bottom 468, left 0, top 668, right 300]; the quad pass is y-down
        // from the top-left, so `y_down = window_height - y_up`: bottom(468)->300, top(668)->100.
        assert_eq!(
            marker.clip,
            Some(Rect::new(0.0, 100.0, 300.0, 300.0)),
            "the engine's ScrollFrame clip survives the y-up→y-down flip into UiQuad::clip"
        );
    }

    /// The uiScale dial (decision 0584) through the same plumb, by hand: at dial 0.5 on the same
    /// 1024×768 window, s = 0.5 and the VM sees a 2048×1536 virtual screen — the TOPLEFT-anchored
    /// frame hangs from virtual top 1536, and every window-px rect halves.
    #[test]
    fn the_uiscale_dial_scales_the_extracted_rects() {
        let mut app = app_with_scrolled_marker();
        app.insert_resource(crate::ui_script::UiScaleCvar(0.5));
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let marker: &UiQuad = quads
            .iter()
            .find(|q| q.color[0] == 1.0 && q.color[1] == 0.0 && q.color[2] == 0.0)
            .expect("the colored marker quad extracted");

        // WoW space: frame top = 1536 − 100 = 1436, bottom 1236, right 300. ×0.5 → px y-up
        // [618..718, 0..150]; y-down through the 768 window: top 768−718 = 50, bottom 768−618 = 150.
        assert_eq!(
            marker.clip,
            Some(Rect::new(0.0, 50.0, 150.0, 150.0)),
            "dial 0.5 halves the px rect and re-hangs the frame from the taller virtual top"
        );
    }

    #[test]
    fn an_unclipped_texture_quad_carries_no_clip() {
        let script = UiScript::new().unwrap();
        script
            .run(
                r#"
            local plain = CreateFrame("Frame", "Plain")
            plain:SetPoint("TOPLEFT", 0, 0)
            plain:SetSize(50, 50)
            local m = plain:CreateTexture(nil, "ARTWORK")
            m:SetTexture(0, 1, 0)
        "#,
            )
            .unwrap();

        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<Time>();
        app.init_resource::<Time<Real>>();
        app.init_resource::<crate::ui_script::UiScaleCvar>();
        app.world_mut().spawn((
            Window {
                resolution: UVec2::new(1024, 768).into(),
                ..default()
            },
            PrimaryWindow,
        ));
        app.add_systems(Update, drive_script);
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let marker = quads
            .iter()
            .find(|q| q.color[0] == 0.0 && q.color[1] == 1.0 && q.color[2] == 0.0)
            .expect("the green marker quad extracted");
        assert_eq!(marker.clip, None);
    }
}
