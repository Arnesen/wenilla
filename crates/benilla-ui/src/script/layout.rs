use std::collections::HashMap;

use crate::layout::{self, Anchor, LayoutGraph, LayoutInput, Point, Rect};
use crate::order::ZTarget;
use crate::widget::{FrameHandle, FrameKind, KindState, RegionHandle};

use super::backdrop;
use super::{
    ExtractedQuad, FontShadow, JustifyH, JustifyV, LineMeasureRequest, MeasureRequest, Model,
    Outline, QuadContent, UiScript, SCREEN,
};

impl UiScript {
    /// [`UiScript::resolve`]'s body, taking `&mut Model` directly rather than `&mut self` — so a
    /// Lua binding holding only a `Model` borrow (via `lua.app_data_mut`, no `&Lua`-wrapping
    /// `UiScript` in scope) can force a fresh resolve too. [`scrollframe`]'s `UpdateScrollChildRect`
    /// is the motivating caller: it fires `OnScrollRangeChanged` with the CURRENT range, but
    /// "current" was a lie for its own primary use (the Era idiom of resize-then-notify in one
    /// script tick) as long as it only read the LAST full [`UiScript::resolve`]'s cached rects —
    /// those don't yet reflect a `SetHeight` earlier in the SAME tick (Lua runs before the app's own
    /// per-frame `resolve()`, `ui_script/mod.rs`'s drive loop), so the fired range was one tick
    /// stale on every "just resized, tell the scrollbar" call — precisely when a caller most needs
    /// it fresh. Cheap to call on demand: the fixpoint early-exits in one round on a quiet graph
    /// (this function's own doc).
    pub(super) fn resolve_layout(model: &mut Model) {
        // The GameTooltip auto-size + right-flush pre-pass (decision 0274): writes tooltip frame
        // sizes + right-column anchor offsets from the measure round-trip's cached extents, so
        // the graph below solves them like any other frame.
        super::tooltip::layout_tooltips(model);
        let Model {
            arena,
            layout_inputs,
            resolved,
            region_data,
            region_resolved,
            frame_to_id,
            id_to_frame,
            id_to_region,
            screen,
            warnings,
            ..
        } = model;

        // One id per live frame, and its layout input with scale synced from the arena.
        let mut ids: HashMap<FrameHandle, u32> = HashMap::new();
        for (&h, &id) in frame_to_id.iter() {
            if arena.frame(h).is_some() {
                ids.insert(h, id);
            }
        }
        for &h in ids.keys() {
            let (scale, clamp) = arena
                .frame(h)
                .map(|f| (f.effective_scale, f.clamped_to_screen))
                .unwrap_or((1.0, false));
            let input = layout_inputs.entry(h).or_default();
            input.scale = scale;
            // Clamp-to-screen is a FRAME property (geometry flags bit4 — SetClampedToScreen, the
            // XML attribute, the GameTooltip-kind default), synced like scale so EVERY placement
            // path — SetOwner's anchor law, a caller's SetPoint, the cursor-seated world plates —
            // resolves through the same clamp, with extents tracking the live window size.
            input.clamp = clamp;
            if clamp {
                input.extent_x = screen.right;
                input.extent_y = screen.top;
            }
        }

        // The ScrollFrame mechanism (decision 0112): a live ScrollFrame with a live scroll child
        // overrides the child's own anchors for this solve — `SetScrollChild` pins the child TOPLEFT
        // to the scrollframe's TOPLEFT, offset by the live vertical scroll (`(0, vertical)`; XML
        // y-positive-up, so a positive offset lifts the child, bringing content below the fold into
        // view — `frame top 500, vertical 40 ⇒ child top 540`). This is a LOCAL map, consulted only
        // while building each round's graph below — the child's authored `LayoutInput.anchors` are
        // never touched, so `SetScrollChild(nil)` needs no restore: the override just stops being
        // computed. The child's own width/height (from its own `LayoutInput`) stay whatever they are;
        // only its anchors are replaced.
        let mut scroll_child_anchor: HashMap<FrameHandle, Anchor> = HashMap::new();
        for (&h, &id) in ids.iter() {
            let Some(frame) = arena.frame(h) else {
                continue;
            };
            if frame.kind != FrameKind::ScrollFrame {
                continue;
            }
            let KindState::Scroll(state) = &frame.kind_state else {
                continue;
            };
            let Some(child) = state.child else { continue };
            if arena.frame(child).is_none() {
                continue; // a stale/destroyed child contributes no override
            }
            scroll_child_anchor.insert(
                child,
                Anchor::new(Point::TopLeft, id, Point::TopLeft, 0.0, state.vertical),
            );
        }

        // Alternating frame/region rounds, run to a CHANGE-DRIVEN FIXPOINT: the real client
        // resolves ONE layout graph in which frames and regions anchor each other freely (the
        // gossip option rows anchor to the greeting FontString's laid-out bottom; the quest log's
        // detail pane chains ~15 regions deep with item BUTTONS anchored to the tail). Our frame
        // pass is a batch graph solve, so each round re-solves it with the latest REGION rects as
        // externals, re-resolves the regions against the new frame rects, and stops when a full
        // round changed nothing. The old fixed budget (2 frame rounds × 3 region rounds) silently
        // left chain links past ~6 on owner-edge fallbacks and dropped tail-anchored frames to the
        // screen origin — decision 0088 §2's finding, which forced windows into invented fixed
        // offsets instead of ref-verbatim chains. The round cap is the node count (an honest round
        // binds at least one new link), so only a genuine anchor CYCLE can exhaust it — warned,
        // mirroring the client's resolving-flag cycle bail. Convergence compares exact rects: each
        // pass recomputes from the same inputs through the same arithmetic, so stabilized values
        // repeat bit-for-bit (no epsilon drift).
        //
        // The fixpoint CARRIES ACROSS FRAMES (decision 0294): every rect is recomputed purely from
        // (inputs, externals), never from its own prior value, so last frame's converged rects are a
        // legal seed — a quiet frame re-verifies in ONE round instead of re-propagating every anchor
        // chain link-by-link (measured: the full default UI held 5–10 rounds × ~7 ms/round at
        // opt-level 0, every frame, ~72 ms of a quiet frame — the director's 60→50 fps). Only
        // regions that left the model (or lost their anchors) drop; a from-scratch resolve is just
        // the empty-seed special case, so first-frame behavior is unchanged.
        region_resolved.retain(|rh, _| {
            arena.region(*rh).is_some()
                && region_data.get(rh).is_some_and(|d| !d.anchors.is_empty())
        });
        let round_cap = ids.len() + region_data.len() + 2;
        for round in 0..round_cap {
            let mut changed = false;

            let mut graph = LayoutGraph::new();
            graph.set_external(SCREEN, *screen);
            for (&id, rh) in id_to_region.iter() {
                if let Some(r) = region_resolved.get(rh) {
                    graph.set_external(id, *r);
                }
            }
            for (&h, &id) in ids.iter() {
                if let Some(input) = layout_inputs.get(&h) {
                    match scroll_child_anchor.get(&h) {
                        Some(&over) => {
                            let mut overridden = input.clone();
                            overridden.anchors = vec![over];
                            graph.set_frame(id, overridden);
                        }
                        None => graph.set_frame(id, input.clone()),
                    }
                }
            }
            let res = graph.resolve();
            for (&h, &id) in ids.iter() {
                if let Some(&r) = res.rects.get(&id) {
                    if resolved.get(&h) != Some(&r) {
                        resolved.insert(h, r);
                        changed = true;
                    }
                } else if resolved.remove(&h).is_some() {
                    changed = true;
                }
            }

            // Second pass — anchored regions. A region that carries `SetPoint` anchors resolves through
            // the same [`crate::layout`] leaf math as frames; any edge the anchors + explicit size don't
            // pin inherits the owner frame's edge (so a single-point FontString gets a real rect, not
            // `+Inf`). Anchor targets may be frames OR **sibling regions by name** (the real XML does
            // this everywhere — the merchant label plate anchors to its `$parentSlot` texture; the
            // owner-fallback that stood in for this was the jutting-plates bug). One region sweep per
            // outer round: a not-yet-resolved chain target leaves this region on its owner-edge
            // fallback for now, the fixpoint re-sweeps until every link has settled.
            for (&rh, data) in region_data.iter() {
                if data.anchors.is_empty() {
                    continue;
                }
                let Some((owner, is_fontstring)) = arena.region(rh).map(|r| {
                    (
                        r.owner,
                        matches!(r.kind, crate::widget::RegionKind::FontString),
                    )
                }) else {
                    continue;
                };
                let Some(owner_rect) = resolved.get(&owner).copied() else {
                    continue;
                };
                let scale = arena.frame(owner).map(|f| f.effective_scale).unwrap_or(1.0);
                // A FontString with no explicit height takes its host-measured wrapped size
                // (the measure round-trip — the client's layout↔font-engine seam).
                let mut height = data.size.map_or(0.0, |s| s.1);
                let mut width = data.size.map_or(0.0, |s| s.0);
                if let Some(m) = &data.measured {
                    if height == 0.0 {
                        height = m.h;
                    }
                    if width == 0.0 {
                        width = m.w;
                    }
                }
                let input = LayoutInput {
                    anchors: data.anchors.clone(),
                    width,
                    height,
                    scale,
                    ..LayoutInput::default()
                };
                let edges = layout::resolve_rect_edges(&input, |id| {
                    id_to_frame
                        .get(&id)
                        .and_then(|h| resolved.get(h))
                        .or_else(|| id_to_region.get(&id).and_then(|h| region_resolved.get(h)))
                        .copied()
                });
                // Unpinned edges: textures inherit the owner's edge (the v1 region model,
                // decision 0068). A FONTSTRING's implicit extent is its measured TEXT, and
                // empty/unmeasured text measures zero — so an unpinned edge COLLAPSES onto its
                // pinned opposite (a zero span), never onto the owner's edge. The owner
                // fallback there stretched an empty tooltip line to the plate's bottom, and
                // the line chain (each line anchors to the previous line's resolved bottom)
                // marched every later line out of the plate. An axis with NO pinned edge keeps
                // the owner fallback (nothing to collapse onto).
                let axis = |lo: Option<f32>, hi: Option<f32>, olo: f32, ohi: f32| -> (f32, f32) {
                    match (is_fontstring, lo, hi) {
                        (true, Some(l), None) => (l, l),
                        (true, None, Some(h)) => (h, h),
                        _ => (lo.unwrap_or(olo), hi.unwrap_or(ohi)),
                    }
                };
                let (bottom, top) = axis(edges[0], edges[2], owner_rect.bottom, owner_rect.top);
                let (left, right) = axis(edges[1], edges[3], owner_rect.left, owner_rect.right);
                let rect = Rect::new(bottom, left, top, right);
                if region_resolved.get(&rh) != Some(&rect) {
                    region_resolved.insert(rh, rect);
                    changed = true;
                }
            }

            if !changed {
                return;
            }
            if round + 1 == round_cap {
                warnings.push(format!(
                    "layout: anchor graph did not converge in {round_cap} rounds — \
                     an anchor cycle? (rects left at their last pass)"
                ));
            }
        }
    }

    /// FontStrings whose layout needs a host text measurement (an auto-sized axis — explicit
    /// width and/or height of 0, the client's size-to-text idiom — text present, cache stale) —
    /// the engine side of the measure round-trip. Call after [`UiScript::resolve`]; answer with
    /// [`UiScript::set_measured_text`] and resolve again (the second solve is cheap and the cache
    /// keys keep this empty on quiet frames). A zero WIDTH auto-sizes exactly like a zero height
    /// (the real client sizes the rect to the unwrapped line — `<Size x="0" y="16"/>` labels like
    /// the mail window's "From:" anchor their RIGHT edge and grow leftward); gating on height
    /// alone left those rects zero-width and their anchored neighbours overlapping.
    pub fn fontstrings_needing_measure(&mut self) -> Vec<MeasureRequest> {
        let mut model = self.model_mut();
        let mut out = Vec::new();
        let handles: Vec<RegionHandle> = model
            .region_data
            .iter()
            .filter(|(_, d)| {
                d.size.is_none_or(|s| s.0 == 0.0 || s.1 == 0.0)
                    && d.text.as_deref().is_some_and(|t| !t.is_empty())
            })
            .map(|(&rh, _)| rh)
            .collect();
        for rh in handles {
            let is_fs = model
                .arena
                .region(rh)
                .is_some_and(|r| matches!(r.kind, crate::widget::RegionKind::FontString));
            if !is_fs {
                continue;
            }
            let d = model.region_data.get(&rh).expect("live region data");
            let text = d.text.clone().unwrap_or_default();
            let wrap_width = d.size.map(|s| s.0).filter(|w| *w > 0.0);
            let font = d.font_path.clone();
            let height = d.font_height;
            let text_height = d.text_height;
            let outline = d.outline;
            // The shared key recipe ([`RegionData::measure_key`]) — the metric reads
            // (region.rs) check the stored measure against the same hash.
            let key = d.measure_key();
            if d.measured.map(|m| m.key) == Some(key) {
                continue;
            }
            let id = model.region_id(rh);
            out.push(MeasureRequest {
                id,
                font,
                height,
                text_height,
                wrap_width,
                outline,
                text,
                key,
            });
        }
        out
    }

    /// Ring lines whose wrapped **row count** needs a host measurement (new line, or the frame's
    /// width/font changed under it) — the message-frame half of the measure round-trip. Call after
    /// [`UiScript::resolve`] (widths must be resolved); answer with
    /// [`UiScript::set_message_line_rows`] before extract. Cache keys keep this empty on quiet
    /// frames.
    pub fn message_lines_needing_measure(&mut self) -> Vec<LineMeasureRequest> {
        use std::hash::{Hash, Hasher};
        let mut model = self.model_mut();
        let mut out = Vec::new();
        let frames: Vec<(FrameHandle, Rect)> = model
            .resolved
            .iter()
            .filter(|(&fh, _)| {
                model.arena.frame(fh).is_some_and(|f| {
                    matches!(f.kind_state, KindState::ScrollingMessage(_)) && f.effective_visible
                })
            })
            .map(|(&fh, &fr)| (fh, fr))
            .collect();
        for (fh, fr) in frames {
            let wrap_width = fr.right - fr.left;
            if wrap_width <= 1.0 {
                continue; // unresolved/degenerate width — nothing meaningful to wrap against
            }
            let (font, height, _, outline) = Self::message_frame_font(&model, fh);
            let frame_id = model.frame_id(fh);
            let Some(frame) = model.arena.frame(fh) else {
                continue;
            };
            let KindState::ScrollingMessage(smf) = &frame.kind_state else {
                continue;
            };
            for (index, line) in smf.lines.iter().enumerate() {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                line.text.hash(&mut hasher);
                font.hash(&mut hasher);
                height.map(f32::to_bits).hash(&mut hasher);
                wrap_width.to_bits().hash(&mut hasher);
                (outline as u8).hash(&mut hasher);
                let key = hasher.finish();
                if line.rows_key == key {
                    continue;
                }
                out.push(LineMeasureRequest {
                    frame: frame_id,
                    index: index as u32,
                    font: font.clone(),
                    height,
                    wrap_width,
                    outline,
                    text: line.text.clone(),
                    key,
                });
            }
        }
        out
    }

    /// Store host row-count answers for [`LineMeasureRequest`]s (`(frame, index, rows, key)` —
    /// frame/index/key verbatim from the request). The key is stored beside the rows, so a line
    /// whose width/font changed again since the request simply re-requests next frame.
    pub fn set_message_line_rows(&mut self, rows: &[(u32, u32, u16, u64)]) {
        let mut model = self.model_mut();
        for &(frame_id, index, n, key) in rows {
            let Some(&fh) = model.id_to_frame.get(&frame_id) else {
                continue;
            };
            let Some(frame) = model.arena.frame_mut(fh) else {
                continue;
            };
            let KindState::ScrollingMessage(smf) = &mut frame.kind_state else {
                continue;
            };
            if let Some(line) = smf.lines.get_mut(index as usize) {
                line.rows = n.max(1);
                line.rows_key = key;
            }
        }
    }

    /// Push one [`QuadContent::Backdrop`] per piece of frame `fh`'s installed backdrop (no-op if the
    /// frame has none). Pieces carry the frame slot's `z` — behind the frame's regions (which sort
    /// after it) and, among themselves, bg-then-border in paint order (the stable z-sort keeps the
    /// emission order). Each piece's rect is its screen bounding box; the app resolves the texture
    /// from the piece path and multiplies the tint by `frame_alpha`.
    pub(super) fn emit_backdrop(
        model: &Model,
        fh: FrameHandle,
        fr: Rect,
        z: u64,
        frame_alpha: f32,
        clip: Option<Rect>,
        out: &mut Vec<ExtractedQuad>,
    ) {
        let Some(bd) = model.backdrops.get(&fh) else {
            return;
        };
        for piece in backdrop::pieces(fr, bd) {
            // The piece's screen bounding box (the render is axis-aligned; the BR-inset bug's slant,
            // invisible for the symmetric insets every shipping backdrop uses, collapses to the box).
            let xs = piece.corners.map(|c| c[0]);
            let ys = piece.corners.map(|c| c[1]);
            let (left, right) = (
                xs.iter().copied().fold(f32::MAX, f32::min),
                xs.iter().copied().fold(f32::MIN, f32::max),
            );
            let (bottom, top) = (
                ys.iter().copied().fold(f32::MAX, f32::min),
                ys.iter().copied().fold(f32::MIN, f32::max),
            );
            let path = if piece.is_bg {
                bd.bg_file.clone()
            } else {
                bd.edge_file.clone()
            };
            let Some(path) = path else { continue };
            let color = if piece.is_bg {
                bd.bg_color
            } else {
                bd.border_color
            };
            out.push(ExtractedQuad {
                target: ZTarget::Frame(fh),
                z,
                rect: Some(Rect::new(bottom, left, top, right)),
                alpha: frame_alpha,
                content: QuadContent::Backdrop {
                    path,
                    color,
                    uvs: piece.uvs,
                    tile: piece.tile,
                },
                clip,
            });
        }
    }

    /// Push one [`QuadContent::Text`] per visible message of a ScrollingMessageFrame `fh` (no-op for
    /// any other kind). Messages stack bottom-up from `fr`'s bottom edge; each occupies
    /// `rows × pitch` (its host-measured wrapped row count — the message-line measure round-trip),
    /// so a long line pushes everything above it up by its real height. The pitch is the **font
    /// height itself** — the client's own line-step law (`LayoutLines` 0x5cdc20: step =
    /// px(size) + spacing, spacing 0), the same law the app's text renderer lays wrapped rows at,
    /// so band grid and glyph rows coincide by construction. (The msgframe's own relayout
    /// `0x788750/0x788c00` is only partially read in wow-re — if the look pass ever shows the ref
    /// spacing chat lines wider than the font height, that residual is the place to pin.)
    ///
    /// `scroll_offset` picks which message sits at the bottom. A message that only partially fits
    /// at the frame's top still draws — clipped to the frame rect, so its lower wrapped rows show
    /// (a reader mid-scrollback), never ink outside the frame. A fully-faded line draws nothing but
    /// its rows still hold their place: the reference's chat never re-packs as old lines fade.
    pub(super) fn emit_message_lines(
        model: &Model,
        fh: FrameHandle,
        fr: Rect,
        z: u64,
        frame_alpha: f32,
        clip: Option<Rect>,
        out: &mut Vec<ExtractedQuad>,
    ) {
        let Some(frame) = model.arena.frame(fh) else {
            return;
        };
        let crate::widget::KindState::ScrollingMessage(smf) = &frame.kind_state else {
            return;
        };
        if smf.lines.is_empty() {
            return;
        }
        let (font, font_height, font_shadow, outline) = Self::message_frame_font(model, fh);
        let pitch = font_height.unwrap_or(14.0);
        if pitch <= 0.0 || fr.top <= fr.bottom {
            return;
        }
        // Message text never inks outside its frame: a partially-fitting top message is scissored
        // at the frame edge (intersected with any ScrollFrame ancestor clip).
        let line_clip = Some(match clip {
            Some(c) => super::clip::intersect_rect(c, fr),
            None => fr,
        });
        // The newest message the view shows sits `scroll_offset` up from the ring's back.
        let top_index = smf.lines.len().saturating_sub(1 + smf.scroll_offset);
        let mut used = 0.0f32; // rows already consumed, in px up from fr.bottom
        for idx in (0..=top_index).rev() {
            if used >= fr.top - fr.bottom {
                break; // the next band would start above the frame — nothing more can show
            }
            let line = &smf.lines[idx];
            let band_h = f32::from(line.rows.max(1)) * pitch;
            let bottom = fr.bottom + used;
            used += band_h;
            if line.alpha <= 0.0 {
                continue; // fully faded — holds its place, draws nothing
            }
            let band = Rect::new(bottom, fr.left, bottom + band_h, fr.right);
            out.push(ExtractedQuad {
                target: ZTarget::Frame(fh),
                z,
                rect: Some(band),
                alpha: frame_alpha,
                clip: line_clip,
                content: QuadContent::Text {
                    text: Some(line.text.clone()),
                    color: Some([
                        f32::from(line.color[0]) / 255.0,
                        f32::from(line.color[1]) / 255.0,
                        f32::from(line.color[2]) / 255.0,
                        line.alpha,
                    ]),
                    justify_h: JustifyH::Left,
                    // The band is our own per-message construct (rows × pitch tall, not a
                    // FontString rect) — keep the block at its top so the band math, not MIDDLE
                    // centering, owns the vertical rhythm; the band height equals the wrapped
                    // block height exactly, so Top means "fills the band".
                    justify_v: JustifyV::Top,
                    font: font.clone(),
                    // The chat font object's own drop shadow (ChatFontNormal's `(1,-1)` black) — what
                    // makes the lines read against the world behind the transparent frame.
                    shadow: font_shadow,
                    font_height,
                    text_height: None, // message lines are never SetTextHeight'd
                    outline,
                    alpha_gradient: None,
                },
            });
        }
    }

    /// The font a ScrollingMessageFrame's ring lines draw with: its declared `<FontString>` child
    /// (ChatFontNormal for the chat window). Shared by [`Self::emit_message_lines`] and the
    /// message-line measure round-trip so bands, measures, and glyphs agree. Missing ⇒ the
    /// renderer's default face at the ~14px chat default.
    pub(super) fn message_frame_font(
        model: &Model,
        fh: FrameHandle,
    ) -> (Option<String>, Option<f32>, Option<FontShadow>, Outline) {
        model
            .arena
            .frame(fh)
            .and_then(|frame| {
                frame
                    .regions
                    .iter()
                    .find(|&&rh| {
                        matches!(
                            model.arena.region(rh).map(|r| r.kind),
                            Some(crate::widget::RegionKind::FontString)
                        )
                    })
                    .and_then(|rh| model.region_data.get(rh))
                    .map(|d| (d.font_path.clone(), d.font_height, d.font_shadow, d.outline))
            })
            .unwrap_or((None, None, None, Outline::default()))
    }
}
