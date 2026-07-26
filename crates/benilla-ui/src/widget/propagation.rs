//! [`WidgetArena`]'s propagation mutators — the binary-verified subtree math that keeps
//! `effective_visible`/`strata`/`level`/`effective_scale`/`effective_alpha` (and reparenting, which
//! touches several of those at once) consistent whenever a frame's own state changes. Split out of
//! [`super`] because this is the module's dense, ground-truth-heavy core (see the module doc's
//! "Ground truth" section); the arena's storage/create/destroy/read-access lives with the struct
//! definition in [`super`].

use crate::order::Strata;

use super::{FrameHandle, WidgetArena, SCALE_EPS};

impl WidgetArena {
    // ── Visibility (effective_visible_show/_hide) ────────────────────────────────────────────────

    /// Set a frame's own `shown` bit and propagate effective visibility through its subtree, per
    /// `effective_visible_show 0x76ae10` / `_hide 0x76ad50` (`propagation.md`). Returns, in
    /// pre-order (a node before its descendants), **every frame whose `effective_visible` actually
    /// changed** — the caller fires `OnShow`/`OnHide` for those, in order. A no-op `shown` write, or
    /// a change that does not move any effective visibility (e.g. hiding an already-invisible frame),
    /// returns empty.
    pub fn set_shown(&mut self, h: FrameHandle, shown: bool) -> Vec<FrameHandle> {
        let mut changed = Vec::new();
        match self.frame_mut(h) {
            Some(f) if f.shown != shown => f.shown = shown,
            _ => return changed,
        }
        let parent_visible = self.parent_visible(h);
        self.propagate_visible(h, parent_visible, &mut changed);
        changed
    }

    fn propagate_visible(
        &mut self,
        h: FrameHandle,
        parent_visible: bool,
        changed: &mut Vec<FrameHandle>,
    ) {
        let (shown, old_ev, children) = {
            let f = self.frame(h).expect("live node in propagation");
            (f.shown, f.effective_visible, f.children.clone())
        };
        let new_ev = shown && parent_visible;
        if new_ev == old_ev {
            // Transition-gated: no change here ⇒ no change below (children's parent-visibility and
            // own shown are unchanged). Prune, exactly like the client's recursion.
            return;
        }
        self.frame_mut(h).unwrap().effective_visible = new_ev;
        if new_ev {
            // The client re-ADDS the frame to its (strata, level) bucket on this transition —
            // an intrusive-list append, so a newly shown frame draws over everything already in
            // its bucket ([`WidgetArena::resequence_to_tail`]). Pre-order recursion gives the
            // descendants later seqs than the node, exactly like the client's recursion re-adding
            // each as its own effectiveVisible flips.
            self.resequence_to_tail(h);
        }
        changed.push(h);
        for c in children {
            self.propagate_visible(c, new_ev, changed);
        }
    }

    // ── Strata (set_frame_strata) ────────────────────────────────────────────────────────────────

    /// Force `h` and its **whole subtree** to `strata`, per `set_frame_strata 0x76a470`
    /// (`propagation.md`): set the frame's strata, then recursively set every child frame to the
    /// same value. The client's visible-gated bucket remove/add (its live draw-list maintenance)
    /// is mirrored by re-sequencing a visible frame to its new bucket's tail
    /// ([`WidgetArena::resequence_to_tail`]); [`crate::order::traversal`] recomputes the buckets
    /// from the fields.
    pub fn set_frame_strata(&mut self, h: FrameHandle, strata: Strata) {
        let (visible, children) = match self.frame_mut(h) {
            Some(f) => {
                if f.strata == strata {
                    return;
                }
                f.strata = strata;
                (f.effective_visible, f.children.clone())
            }
            None => return,
        };
        if visible {
            self.resequence_to_tail(h);
        }
        for c in children {
            self.set_frame_strata(c, strata);
        }
    }

    // ── Level (set_frame_level) ──────────────────────────────────────────────────────────────────

    /// Set `h`'s level. When `propagate`, **same-strata** child frames shift by the same delta
    /// (relative offsets preserved); cross-strata children are untouched — per `set_frame_level
    /// 0x76a4f0` (`propagation.md`). Level is unsigned here, so the client's `if (level < 0) level =
    /// 0` clamp is subsumed by the type (a Lua binding saturates on conversion); a propagated shift
    /// that would go negative saturates to 0, and an overflow saturates to `u16::MAX`.
    pub fn set_frame_level(&mut self, h: FrameHandle, level: u16, propagate: bool) {
        let (delta, strata, children) = match self.frame_mut(h) {
            Some(f) => {
                if f.level == level {
                    return;
                }
                let delta = i32::from(level) - i32::from(f.level);
                f.level = level;
                (delta, f.strata, f.children.clone())
            }
            None => return,
        };
        // The client's visible-gated remove/add: a visible frame lands at its NEW bucket's tail
        // (a hidden one is in no bucket — it appends on its next show anyway).
        if self.frame(h).is_some_and(|f| f.effective_visible) {
            self.resequence_to_tail(h);
        }
        if !propagate {
            return;
        }
        for c in children {
            let child_new = match self.frame(c) {
                Some(cf) if cf.strata == strata => {
                    (i32::from(cf.level) + delta).clamp(0, i32::from(u16::MAX)) as u16
                }
                _ => continue, // cross-strata (or stale) child: untouched
            };
            self.set_frame_level(c, child_new, true);
        }
    }

    // ── Scale (effective_scale) ──────────────────────────────────────────────────────────────────

    /// Set `h`'s own scale and recompute effective scale down the subtree, ε-gated, per
    /// `effective_scale 0x76ac90` (`propagation.md`): `effectiveScale = parentScale * ownScale`; a
    /// node whose new effective scale is within [`SCALE_EPS`] of its current one is skipped along
    /// with its subtree (its children's parent-scale did not move).
    pub fn set_scale(&mut self, h: FrameHandle, scale: f32) {
        if let Some(f) = self.frame_mut(h) {
            f.scale = scale;
        } else {
            return;
        }
        let parent_scale = self.parent_effective_scale(h);
        self.propagate_scale(h, parent_scale);
    }

    fn propagate_scale(&mut self, h: FrameHandle, parent_scale: f32) {
        let (own, ignore, old_eff, children) = {
            let f = self.frame(h).expect("live node in propagation");
            (
                f.scale,
                f.ignore_parent_scale,
                f.effective_scale,
                f.children.clone(),
            )
        };
        let new_eff = if ignore { own } else { parent_scale * own };
        if (f64::from(new_eff) - f64::from(old_eff)).abs() < SCALE_EPS {
            return;
        }
        self.frame_mut(h).unwrap().effective_scale = new_eff;
        for c in children {
            self.propagate_scale(c, new_eff);
        }
    }

    // ── Alpha ────────────────────────────────────────────────────────────────────────────────────

    /// Set `h`'s alpha and **overwrite every descendant frame's** with the same raw value — the
    /// byte-verified 1.12 mechanism (`SetAlpha 0x76a690` writes `[this+0xc8]` then recurses the
    /// child-frame list pushing the SAME value; wow-re `propagation.md`, CORRECTED section). An
    /// eager set-time flatten, structurally the strata subtree-force — never a draw-time ancestor
    /// product, so `effective_alpha` (what a renderer's one-hop region composition reads) always
    /// equals `alpha`.
    pub fn set_alpha(&mut self, h: FrameHandle, alpha: f32) {
        let Some(f) = self.frame_mut(h) else { return };
        f.alpha = alpha;
        f.effective_alpha = alpha;
        let children = self.frame(h).expect("just wrote it").children.clone();
        for c in children {
            self.set_alpha(c, alpha);
        }
    }

    // ── Reparenting (set_parent) ─────────────────────────────────────────────────────────────────

    /// Reparent `h` under `new_parent` (or `None` for top-level), then propagate effective
    /// visibility, scale, and alpha down `h`'s subtree — `SetParent` re-inherits the parent chain's
    /// `effectiveVisible` (`frame-model.md`), and effective scale/alpha likewise depend on the
    /// chain. Returns the frames whose `effective_visible` changed (as [`set_shown`](Self::set_shown)
    /// does), so the caller can fire `OnShow`/`OnHide`.
    ///
    /// A reparent that would create a cycle (making `h` its own ancestor) is rejected as a no-op —
    /// the client guards this with `is_descendant 0x767010` (`propagation.md`). Strata/level are NOT
    /// changed by reparenting (a frame keeps them until explicitly set).
    pub fn set_parent(
        &mut self,
        h: FrameHandle,
        new_parent: Option<FrameHandle>,
    ) -> Vec<FrameHandle> {
        let mut changed = Vec::new();
        if self.frame(h).is_none() {
            return changed;
        }
        // Keep only a live new parent; reject a cycle (new_parent == h or below h).
        let new_parent = new_parent.filter(|&p| self.frame(p).is_some());
        if let Some(np) = new_parent {
            if np == h || self.is_ancestor(h, np) {
                return changed;
            }
        }

        let old_parent = self.frame(h).unwrap().parent;
        if old_parent == new_parent {
            return changed;
        }
        if let Some(op) = old_parent {
            if let Some(of) = self.frame_mut(op) {
                of.children.retain(|&c| c != h);
            }
        }
        self.frame_mut(h).unwrap().parent = new_parent;
        if let Some(np) = new_parent {
            self.frame_mut(np).unwrap().children.push(h);
        }

        let pv = self.parent_visible(h);
        self.propagate_visible(h, pv, &mut changed);
        let ps = self.parent_effective_scale(h);
        self.propagate_scale(h, ps);
        // Alpha: untouched by a reparent — the client only pushes alpha at SetAlpha time
        // (module alpha section), so the moved subtree keeps its last-written values.
        changed
    }

    // ── Mouse interaction (the hit-test flag) ────────────────────────────────────────────────────

    /// Set a frame's `mouse_enabled` flag (`EnableMouse`). Only mouse-enabled *and* effective-visible
    /// frames capture the cursor in [`crate::order::hit_test`]; the default is **false** (WoW's
    /// `enableMouse` default). A stale handle is a no-op. (Keyboard focus is a separate concern, not
    /// modeled here yet.)
    pub fn set_mouse_enabled(&mut self, h: FrameHandle, enabled: bool) {
        if let Some(f) = self.frame_mut(h) {
            f.mouse_enabled = enabled;
        }
    }

    /// Whether the frame currently accepts the mouse (see [`WidgetArena::set_mouse_enabled`]). A
    /// stale handle reads as `false`.
    pub fn is_mouse_enabled(&self, h: FrameHandle) -> bool {
        self.frame(h).map(|f| f.mouse_enabled).unwrap_or(false)
    }

    // ── Clamp-to-screen (geometry flags bit4) ────────────────────────────────────────────────────

    /// Set a frame's clamp-to-screen flag (`SetClampedToScreen` `0x776c00` — geometry flags bit4;
    /// see [`Frame::clamped_to_screen`] for the tooltip-kind default). A stale handle is a no-op.
    pub fn set_clamped_to_screen(&mut self, h: FrameHandle, clamp: bool) {
        if let Some(f) = self.frame_mut(h) {
            f.clamped_to_screen = clamp;
        }
    }

    /// Whether the frame clamps to the screen (`IsClampedToScreen` `0x776cb0`). A stale handle
    /// reads as `false`.
    pub fn is_clamped_to_screen(&self, h: FrameHandle) -> bool {
        self.frame(h).map(|f| f.clamped_to_screen).unwrap_or(false)
    }

    // ── Hit-rect insets (the mouse rect, not the draw rect) ──────────────────────────────────────

    /// Set a frame's hit-rect insets, `[left, right, top, bottom]` (`SetHitRectInsets`; see
    /// [`Frame::hit_rect_insets`]). A stale handle is a no-op.
    pub fn set_hit_rect_insets(&mut self, h: FrameHandle, insets: [f32; 4]) {
        if let Some(f) = self.frame_mut(h) {
            f.hit_rect_insets = insets;
        }
    }

    /// A frame's hit-rect insets, `[left, right, top, bottom]` (`GetHitRectInsets`). A stale handle
    /// reads as all-zero — the same "no inset" answer an untouched frame gives.
    pub fn hit_rect_insets(&self, h: FrameHandle) -> [f32; 4] {
        self.frame(h).map(|f| f.hit_rect_insets).unwrap_or([0.0; 4])
    }

    // ── Small helpers ────────────────────────────────────────────────────────────────────────────

    /// Is `maybe_ancestor` on the parent chain of `node` (walking up, loop-guarded)?
    fn is_ancestor(&self, maybe_ancestor: FrameHandle, node: FrameHandle) -> bool {
        let mut cur = self.frame(node).and_then(|f| f.parent);
        let mut guard = 0usize;
        while let Some(c) = cur {
            if c == maybe_ancestor {
                return true;
            }
            cur = self.frame(c).and_then(|f| f.parent);
            guard += 1;
            if guard > self.frames.slots.len() {
                break; // defensive; a well-formed tree never loops
            }
        }
        false
    }

    fn parent_visible(&self, h: FrameHandle) -> bool {
        match self.frame(h).and_then(|f| f.parent) {
            Some(p) => self.frame(p).is_none_or(|pf| pf.effective_visible),
            None => true,
        }
    }

    fn parent_effective_scale(&self, h: FrameHandle) -> f32 {
        match self.frame(h).and_then(|f| f.parent) {
            Some(p) => self.frame(p).map_or(1.0, |pf| pf.effective_scale),
            None => 1.0,
        }
    }
}
