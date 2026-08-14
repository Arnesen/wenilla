//! [`WidgetArena`]'s propagation mutators — the binary-verified subtree math that keeps
//! `effective_visible`/`strata`/`level`/`effective_scale`/`effective_alpha` (and reparenting, which
//! touches several of those at once) consistent whenever a frame's own state changes. Split out of
//! [`super`] because this is the module's dense, ground-truth-heavy core (see the module doc's
//! "Ground truth" section); the arena's storage/create/destroy/read-access lives with the struct
//! definition in [`super`].

use crate::order::Strata;

use super::{FrameHandle, RegionHandle, WidgetArena, SCALE_EPS};

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

    /// Renumber `strata`'s **occupied** levels contiguously into `[0, count)` and return `count` —
    /// `level_compact 0x764eb0`, the step `CSimpleTop::Raise 0x7650f0` runs immediately before it
    /// sets the raised frame's level to `bucket->count(+0x8)` (wow-re `ui/scratch/toplevel-raise.md`,
    /// consequence 1: the new level is the counter read *after* compaction, "never from a live
    /// max-scan of frames").
    ///
    /// **Only frames in the bucket are renumbered — i.e. effective-visible ones.** A stratum bucket
    /// is an array of intrusive level lists, and a frame is linked into one only while it is
    /// effectively visible (the same visible-gate [`WidgetArena::resequence_to_tail`] mirrors); a
    /// hidden frame is in no bucket, so its `+0xc4` is not touched and it re-enters at whatever level
    /// it kept.
    ///
    /// **The renumber is strictly order-preserving, so by itself it changes no draw order**: distinct
    /// levels map to distinct indices monotonically and equal levels stay equal. Its whole job is to
    /// keep the raise target *bounded* — without it, `level := max + 1` would ratchet upward one step
    /// per raise for as long as the session lasts.
    ///
    /// Two things it deliberately is **not**: it does not propagate (every same-strata descendant is
    /// itself in the bucket and is renumbered by its own level node), and it does not relink — the
    /// client relocates whole level nodes with their intrusive lists intact, so link order (our
    /// `insertion_seq`) must survive. That is why this writes `level` directly instead of going
    /// through [`WidgetArena::set_frame_level`], which would do both.
    pub fn compact_levels(&mut self, strata: Strata) -> u16 {
        let mut occupied: Vec<u16> = self
            .iter_frames()
            .filter(|(_, f)| f.effective_visible && f.strata == strata)
            .map(|(_, f)| f.level)
            .collect();
        occupied.sort_unstable();
        occupied.dedup();
        let renumber: Vec<(FrameHandle, u16)> = self
            .iter_frames()
            .filter(|(_, f)| f.effective_visible && f.strata == strata)
            .filter_map(|(h, f)| {
                let idx = occupied.binary_search(&f.level).ok()? as u16;
                (idx != f.level).then_some((h, idx))
            })
            .collect();
        for (h, level) in renumber {
            if let Some(f) = self.frame_mut(h) {
                f.level = level;
            }
        }
        occupied.len() as u16
    }

    // ── The toplevel flag (flag word bit 0x1) ────────────────────────────────────────────────────

    /// Set a frame's `toplevel` bit — `SetToplevel 0x775440` / XML `toplevel`, both through the pure
    /// bit-setter `0x76a3c0` (see [`Frame::toplevel`]). A pure flag write: **it raises nothing**.
    /// A stale handle is a no-op.
    pub fn set_toplevel(&mut self, h: FrameHandle, toplevel: bool) {
        if let Some(f) = self.frame_mut(h) {
            f.toplevel = toplevel;
        }
    }

    /// Whether the frame carries the `toplevel` bit (`IsToplevel`). A stale handle reads as `false`.
    pub fn is_toplevel(&self, h: FrameHandle) -> bool {
        self.frame(h).is_some_and(|f| f.toplevel)
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

    /// Re-link a **region leaf** (Texture/FontString) to a new owner frame — the arena half of
    /// `Region:SetParent`. `None` detaches ([`super::Region::detached`]). Returns whether anything
    /// moved (a same-owner call and a stale handle both report `false`).
    ///
    /// **This is a different mechanism from the frame reparent above, and the difference is
    /// verified.** `SetParent` is one Region-table binding (`0x7a1550`) dispatching a per-class
    /// virtual: a plain Region gets `0x76c430`, which writes the geometry parent and nothing else,
    /// but a Texture or FontString gets `0x7733d0` → `0x77fd10`, a **full re-link** — remove from
    /// the old parent's draw layer (`0x77fc60`) and region list (`vtbl+0x2c`), store the new
    /// parent, insert into its region list (`0x76a750`) and re-register in its draw layer
    /// (`0x77fcb0`), **preserving layer and sub-level** (wow-re `widget-api-batch-benilla.md` Q7).
    /// So the layer/sub-level fields are deliberately untouched here and only the membership moves;
    /// the fresh `decl_seq` is the "insert into the new parent's region list" half, which lands the
    /// region at the tail of that frame's list exactly as the client's insert does.
    ///
    /// Nothing propagates, unlike the frame reparent: a region has no effective-visible/scale/alpha
    /// of its own here — every reader (`IsVisible`, the measure key, `extract`'s alpha product)
    /// resolves the owner frame's live values through [`super::Region::owner`], so re-pointing that
    /// field *is* the propagation the client does eagerly (`0x77fd10` pushes the new parent's shown
    /// bit into the region).
    ///
    /// **A same-owner call is a no-op, and that is the conservative reading, not a verified one.**
    /// `0x77fd10` is unconditional, so the reference may re-tail a region within its own layer when
    /// an addon re-parents it to the frame it already belongs to. Not reproducing that can only
    /// ever *preserve* declared draw order, never invent one; and the corpus's only region
    /// `SetParent` caller — `FuBar_FuXPFu.lua:210-211`, which re-parents two `XPBar:CreateTexture`
    /// sparks to `XPBar` — moves both in declaration order, so the two readings agree there.
    pub fn set_region_owner(&mut self, rh: RegionHandle, new_owner: Option<FrameHandle>) -> bool {
        let Some(region) = self.region(rh) else {
            return false;
        };
        let (old_owner, was_detached) = (region.owner, region.detached);
        // Keep only a live new owner. No cycle check: the client's (`0x7a177f`) walks the new
        // parent's frame chain looking for the receiver, and a Texture/FontString is never a frame,
        // so for a region it can never fire.
        let new_owner = new_owner.filter(|&f| self.frame(f).is_some());
        match new_owner {
            Some(f) if f == old_owner && !was_detached => return false,
            None if was_detached => return false,
            _ => {}
        }
        let Some(new_owner) = new_owner else {
            // Detached: the entry stays in its last owner's list (so `destroy` still frees it) and
            // the flag alone takes it out of the draw.
            if let Some(r) = self.region_mut(rh) {
                r.detached = true;
            }
            return true;
        };
        if let Some(of) = self.frame_mut(old_owner) {
            of.regions.retain(|&r| r != rh);
        }
        let decl_seq = {
            let f = self.frame_mut(new_owner).expect("live new owner");
            let d = f.next_decl;
            f.next_decl += 1;
            d
        };
        if let Some(r) = self.region_mut(rh) {
            r.owner = new_owner;
            r.decl_seq = decl_seq;
            r.detached = false;
        }
        self.frame_mut(new_owner)
            .expect("live new owner")
            .regions
            .push(rh);
        true
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

    /// `EnableKeyboard` (`0x776f90`) — kind-0/kind-1 bucket membership. Stored and answered; the
    /// key path is not gated on it yet (see [`crate::widget::WidgetState::keyboard_enabled`]).
    pub fn set_keyboard_enabled(&mut self, h: FrameHandle, enabled: bool) {
        if let Some(f) = self.frame_mut(h) {
            f.keyboard_enabled = enabled;
        }
    }

    /// Whether the frame is keyboard-enabled. A stale handle reads as `false`.
    pub fn is_keyboard_enabled(&self, h: FrameHandle) -> bool {
        self.frame(h).is_some_and(|f| f.keyboard_enabled)
    }

    /// `EnableMouseWheel` — the wheel's own gate, separate from the mouse's (decision 1198).
    pub fn set_mouse_wheel_enabled(&mut self, h: FrameHandle, enabled: bool) {
        if let Some(f) = self.frame_mut(h) {
            f.mouse_wheel_enabled = enabled;
        }
    }

    /// Whether the frame currently accepts the wheel. A stale handle reads as `false`.
    pub fn is_mouse_wheel_enabled(&self, h: FrameHandle) -> bool {
        self.frame(h).is_some_and(|f| f.mouse_wheel_enabled)
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

    /// Is `maybe_ancestor` on the parent chain of `node` (walking up, loop-guarded)? — the client's
    /// `is_descendant 0x767010`, read the other way round.
    ///
    /// Two callers, and they are the two the binary has: [`WidgetArena::set_parent`]'s cycle guard,
    /// and the raise's overlap scan, which excludes the raised frame's own subtree from the frames
    /// it may be considered to overlap ([`crate::script::object::toplevel`]).
    pub fn is_ancestor(&self, maybe_ancestor: FrameHandle, node: FrameHandle) -> bool {
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
