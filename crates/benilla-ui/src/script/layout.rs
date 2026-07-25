use std::collections::HashMap;

use crate::layout::{self, Anchor, LayoutInput, Point, Rect};
use crate::order::ZTarget;
use crate::widget::{FrameHandle, FrameKind, KindState, RegionHandle};

use super::backdrop;
use super::{
    ExtractedQuad, FontShadow, JustifyH, JustifyV, LineMeasureRequest, MeasureRequest, Model,
    Outline, QuadContent, UiScript, SCREEN,
};

/// A 128-bit rolling fingerprint of everything [`UiScript::resolve_layout`] *reads*, so a resolve
/// whose inputs are byte-identical to the last one can be skipped outright.
///
/// ## Why a fingerprint and not a dirty flag
///
/// A dirty flag has to be *set* at every mutation site, and the resolve's read set is spread over
/// `layout_inputs`, the arena (scale, clamp, ScrollFrame child + offset, frame/region liveness),
/// `region_data` (anchors, size, measured), and `screen`. Missing one site is a silently stale
/// layout — the worst failure mode this module has, and one no test reliably catches because the
/// staleness only shows on the frame the missed setter fires. A fingerprint has no sites to miss:
/// it is computed FROM the read set, so any change to any of it changes the value by construction.
/// It also absorbs *idempotent* writes for free — the GameTooltip pre-pass rewrites tooltip sizes
/// and anchor offsets every single call, which would pin a mutation-tracked flag permanently dirty.
///
/// ## Why 128 bits
///
/// A collision here is not self-healing: the skip keeps the stored value, so a colliding change
/// would stay unresolved until something *else* moved. Two independent lanes put that beyond
/// physical concern (~2^-128 per frame) at the cost of one extra multiply per item.
///
/// ## Order sensitivity is safe
///
/// The accumulator is order-dependent, and it is fed by walking `HashMap`s whose iteration order
/// is arbitrary. That order is nevertheless *stable for an unmutated map*, so an unchanged model
/// reproduces the value; a mutated map may reorder and merely re-resolves. The failure mode is a
/// false DIRTY (a wasted resolve), never a false CLEAN (a stale rect).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputFingerprint(u64, u64);

impl Default for InputFingerprint {
    fn default() -> Self {
        // Distinct non-zero bases (FNV-1a's, and a 64-bit mix constant) so leading zero-valued
        // items still move both lanes.
        InputFingerprint(0xcbf2_9ce4_8422_2325, 0x9e37_79b9_7f4a_7c15)
    }
}

impl InputFingerprint {
    #[inline]
    fn feed(&mut self, v: u64) {
        self.0 = (self.0 ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        self.1 = (self.1 ^ v.rotate_left(29)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    }

    #[inline]
    fn f32(&mut self, v: f32) {
        // Bit pattern, not value: `resolve` is bit-exact float math, so -0.0 and NaN payloads are
        // distinctions that matter here even where `==` would call them equal (or unequal).
        self.feed(u64::from(v.to_bits()));
    }

    #[inline]
    fn anchors(&mut self, anchors: &[Anchor]) {
        self.feed(anchors.len() as u64);
        for a in anchors {
            self.feed(u64::from(a.point.id()));
            self.feed(u64::from(a.relative_to));
            self.feed(u64::from(a.relative_point.id()));
            self.f32(a.x_off);
            self.f32(a.y_off);
        }
    }

    #[inline]
    fn input(&mut self, i: &LayoutInput) {
        self.anchors(&i.anchors);
        self.f32(i.width);
        self.f32(i.height);
        self.f32(i.scale);
        self.feed(u64::from(i.clamp));
        self.f32(i.extent_x);
        self.f32(i.extent_y);
    }

    #[inline]
    fn rect(&mut self, r: Rect) {
        self.f32(r.bottom);
        self.f32(r.left);
        self.f32(r.top);
        self.f32(r.right);
    }
}

/// `WOW_LAYOUT_VERIFY=1` — the change gate's self-check (decision 0022's instruments-first
/// posture). When set, a resolve the gate wants to SKIP runs in full anyway and asserts it
/// reproduced the previous rects exactly; a divergence means the fingerprint's read set is
/// incomplete, and it fails loudly at the resolve that proves it rather than silently rendering a
/// stale frame days later.
///
/// This is what makes the gate's central claim — "the fingerprint covers everything the rounds
/// read" — machine-checked instead of argued: run the workspace suite under it and every UI test
/// (the shipped windows, driven through real FrameXML + Lua) becomes a probe for a missed input.
/// Read once; when off it costs a relaxed atomic load per resolve.
fn layout_verify_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WOW_LAYOUT_VERIFY").as_deref() == Ok("1"))
}

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
            id_to_region,
            region_to_id,
            screen,
            warnings,
            solver,
            layout_fingerprint,
            layout_solves,
            ..
        } = model;

        // One id per live frame, and its layout input with scale synced from the arena. A flat
        // `Vec`, not a map: this list is walked (never probed) by every round below, and it is
        // rebuilt on each call anyway — a `HashMap` bought nothing and cost 1881 hashes per call.
        let mut ids: Vec<(FrameHandle, u32)> = Vec::with_capacity(frame_to_id.len());
        for (&h, &id) in frame_to_id.iter() {
            if arena.frame(h).is_some() {
                ids.push((h, id));
            }
        }
        for &(h, _) in &ids {
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
        for &(h, id) in &ids {
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
        // The per-frame solve plan, resolved ONCE per call rather than per round: each live
        // frame's id, a borrow of its (already scale/clamp-synced) layout input, and its
        // ScrollFrame anchor override if it is a scroll child. The round loop below then walks a
        // flat slice — no map probes, no input clones.
        let plan: Vec<(FrameHandle, u32, &LayoutInput, Option<Anchor>)> = ids
            .iter()
            .filter_map(|&(h, id)| {
                layout_inputs
                    .get(&h)
                    .map(|input| (h, id, input, scroll_child_anchor.get(&h).copied()))
            })
            .collect();

        // ── The change gate ───────────────────────────────────────────────────────────────────
        // Fingerprint exactly what the rounds below read, and skip them outright when nothing has
        // moved since the last converged resolve. Measured on the shipped default UI: the layout
        // inputs are byte-identical on 34 of 35 idle frames and 39 of 41 with three windows open,
        // because almost all per-frame UI traffic (StatusBar values, cooldown sweeps, chat fades,
        // colour changes) is read by EXTRACT, not by the anchor solve.
        //
        // The read set, in the order the rounds consume it:
        //   * `screen`                      — the root external, and the clamp extents;
        //   * `plan`                        — every live frame's id, its (already scale/clamp-
        //                                     synced) `LayoutInput`, and its ScrollFrame override,
        //                                     which together distil `frame_to_id`, frame liveness,
        //                                     `effective_scale`, `clamped_to_screen`, and the
        //                                     ScrollFrame child + vertical offset;
        //   * `id_to_region` × `region_resolved` — the region rects seeded as externals;
        //   * `region_data`                 — each region's anchors, explicit size, and measured
        //                                     text extent;
        //   * `arena.region(rh)`            — liveness and owner (a dead region drops out of the
        //                                     sweep; the owner supplies the fallback edges).
        // `resolved` / `region_resolved` are the previous pass's OUTPUT carried forward as the
        // 0294 seed; at convergence seed and output are equal, so re-running against an unchanged
        // input set is guaranteed to reproduce them — which is precisely what makes the skip safe.
        let mut fp = InputFingerprint::default();
        fp.rect(*screen);
        fp.feed(plan.len() as u64);
        for &(_, id, input, over) in &plan {
            fp.feed(u64::from(id));
            fp.input(input);
            match over {
                Some(a) => fp.anchors(std::slice::from_ref(&a)),
                None => fp.feed(u64::MAX),
            }
        }
        fp.feed(region_data.len() as u64);
        for (&rh, data) in region_data.iter() {
            fp.feed(rh.fingerprint_bits());
            fp.anchors(&data.anchors);
            match data.size {
                Some((w, h)) => {
                    fp.f32(w);
                    fp.f32(h);
                }
                None => fp.feed(u64::MAX),
            }
            match data.measured {
                Some(m) => {
                    fp.f32(m.w);
                    fp.f32(m.h);
                }
                None => fp.feed(u64::MAX),
            }
            // Liveness only. The sweep also reads the region's `owner` and `kind`, but both are
            // fixed at creation and can never change under a live handle; the owner's contribution
            // to the result — its resolved rect and its `effective_scale` — rides the frame half
            // (the scale is synced into `layout_inputs` by the preamble above, and the rect is the
            // frame pass's own output). A DESTROYED region does still matter: `WidgetArena::destroy`
            // drops the arena entry but leaves `region_data` behind, so membership alone would miss
            // it and the sweep's `continue` would go unnoticed.
            fp.feed(u64::from(arena.region(rh).is_some()));
        }
        for (&id, rh) in id_to_region.iter() {
            if let Some(r) = region_resolved.get(rh) {
                fp.feed(u64::from(id));
                fp.rect(*r);
            }
        }
        let gate_skips = *layout_fingerprint == Some(fp);
        if gate_skips && !layout_verify_enabled() {
            return;
        }
        // Under `WOW_LAYOUT_VERIFY=1` a skippable resolve runs anyway, against a copy of the rects
        // it is supposed to reproduce — see [`layout_verify_enabled`].
        let verify_against = gate_skips.then(|| (resolved.clone(), region_resolved.clone()));
        // Cleared, not stored, until the fixpoint actually CONVERGES: a run that exhausts the
        // round cap (a genuine anchor cycle) leaves its rects mid-flight, and the 0294 cross-frame
        // seed is what lets the next frame carry them further. Storing here would freeze a
        // pathological graph at whatever partial state it reached.
        *layout_fingerprint = None;
        // Counted on the gate's DECISION, not on the fixpoint running: under
        // `WOW_LAYOUT_VERIFY=1` a skipped resolve still runs below, and a counter that moved for
        // it would mean something different in the two modes — exactly the ambiguity the tests
        // asserting on it exist to remove.
        if !gate_skips {
            *layout_solves += 1;
        }

        let round_cap = plan.len() + region_data.len() + 2;
        for round in 0..round_cap {
            let mut changed = false;

            // Seed the solver: the screen root, then every region rect settled so far (regions are
            // externals to the frame solve — the fixpoint is what closes the loop between them).
            // Frame ids and region ids come from one monotonic counter, so both live in the
            // solver's single dense rect array and every anchor-target lookup is an array index.
            solver.begin();
            solver.set_external(SCREEN, *screen);
            for (&id, rh) in id_to_region.iter() {
                if let Some(r) = region_resolved.get(rh) {
                    solver.set_external(id, *r);
                }
            }
            for &(_, id, input, over) in &plan {
                match over {
                    Some(a) => solver.set_frame_anchored(id, input, a),
                    None => solver.set_frame(id, input),
                }
            }
            solver.solve();
            for &(h, id, _, _) in &plan {
                match solver.rect(id) {
                    Some(r) => {
                        if resolved.get(&h) != Some(&r) {
                            resolved.insert(h, r);
                            changed = true;
                        }
                    }
                    None => {
                        if resolved.remove(&h).is_some() {
                            changed = true;
                        }
                    }
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
            //
            // `scratch` is hoisted out of the loop so the per-region `LayoutInput` refills its
            // anchors `Vec` in place instead of allocating one per region per round.
            let mut scratch = LayoutInput::default();
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
                scratch.anchors.clear();
                scratch.anchors.extend_from_slice(&data.anchors);
                scratch.width = width;
                scratch.height = height;
                scratch.scale = scale;
                let edges = layout::resolve_rect_edges(&scratch, |id| {
                    // SCREEN is a FRAME-pass external only. The map-pair lookup this replaces
                    // consulted `id_to_frame` and `id_to_region`, and neither can hold the
                    // reserved id 0 (`next_id` starts at 1) — so a region anchor to the screen
                    // root has always fallen through to the owner-edge fallback below. Preserved
                    // deliberately: the solver's dense array DOES carry the screen rect at 0, and
                    // quietly starting to resolve it here would be a behaviour change riding in
                    // on a performance fix.
                    (id != SCREEN).then(|| solver.rect(id)).flatten()
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
                // Publish into the solver as well as the model: a later region in THIS same sweep
                // that anchors to this one must see the fresh rect (the sweep has always worked
                // that way — it read the same `region_resolved` map it was writing).
                if let Some(&id) = region_to_id.get(&rh) {
                    solver.set_external(id, rect);
                }
                if region_resolved.get(&rh) != Some(&rect) {
                    region_resolved.insert(rh, rect);
                    changed = true;
                }
            }

            if !changed {
                if let Some((frames_before, regions_before)) = &verify_against {
                    assert!(
                        frames_before == resolved && regions_before == region_resolved,
                        "WOW_LAYOUT_VERIFY: the change gate skipped a resolve that would have \
                         MOVED something — the fingerprint's read set is incomplete. \
                         frames {} -> {} changed, regions {} -> {} changed",
                        frames_before.len(),
                        resolved.len(),
                        regions_before.len(),
                        region_resolved.len(),
                    );
                }
                *layout_fingerprint = Some(fp);
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
