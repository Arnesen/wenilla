//! The widget object model — the frame arena and the show/hide/strata/level/scale/alpha
//! mutations, transcribed from wow-5875-re's binary-verified propagation cluster (decision 0068).
//!
//! This crate owns the *model*, not the runtime: the arena stores frames and their region leaves and
//! implements the propagation math, but it does **not** run Lua or fire `OnShow`/`OnHide` — the
//! mutations that cause visibility transitions *return the set of frames that changed* so the
//! Lua-embedding layer (the app) can fire the handlers in order.
//!
//! ## Ground truth (wow-5875-re `system/ui/scratch/`, binary-verified against `WoW.exe`)
//!
//! - **Effective visibility** (`effective_visible_show 0x76ae10` / `_hide 0x76ad50`,
//!   `propagation.md`): `effectiveVisible = shown AND (parent == null OR parent.effectiveVisible)`.
//!   A transition recurses into child frames — **a hidden mid-tree frame blocks its whole subtree** —
//!   and fires `OnShow`/`OnHide`. The recursion is *transition-gated*: a node whose effective
//!   visibility does not change stops the walk (its subtree cannot have changed).
//! - **Strata** (`set_frame_strata 0x76a470`): the **whole subtree is forced to one stratum** — the
//!   frame's strata is set, then every child frame is recursively set to the same value.
//! - **Level** (`set_frame_level 0x76a4f0`): clamp `>= 0`; `delta = new - old`; **same-strata**
//!   children shift by the same `delta` (relative offsets preserved); cross-strata children are
//!   untouched.
//! - **Effective scale** (`effective_scale 0x76ac90`): `effectiveScale = parentScale * ownScale`,
//!   written to `layoutScale`, **ε-gated** (skip the write + recursion if `|new - cur| < 2.384e-7`,
//!   the `_DAT_008029d4` constant — [`SCALE_EPS`]); recurse to child frames.
//! - **Alpha** (`SetAlpha 0x76a690`, byte-verified — wow-re `propagation.md`, CORRECTED section):
//!   an eager set-time **overwrite-cascade** — SetAlpha writes the raw value to the frame and
//!   recurses over its child frames pushing the SAME value (a flatten, like the strata subtree-
//!   force; never a draw-time ancestor product). At draw a region multiplies its own alpha by its
//!   OWNER frame's alpha only (one hop, `0x772180`/`0x77fac0`) — which is what `effective_alpha`
//!   feeds, so effective always equals own.
//! - **Named auto-publish**: a frame with a name is published to the global name table; a duplicate
//!   name does **not** overwrite the first (see [`WidgetArena::lookup`]).
//!
//! ## Alpha: the settled model (the old 1.12-vs-Era tension is resolved)
//!
//! An earlier INFERRED reading ("1.12 alpha is not tree-propagated") kept a dormant Era-style
//! propagation flag here as a hedge. The §5 cross-check on `SetAlpha 0x76a690` settled it: 1.12
//! DOES reach the subtree, but by set-time overwrite (the same raw byte pushed to every
//! descendant frame's `+0xc8`), not by a live parent×child product — Era's `ignoreParentAlpha`
//! belongs to the later multiplicative model and has no 1.12 counterpart (there is no
//! `GetEffectiveAlpha` in 5875). The hedge machinery is gone; [`WidgetArena::set_alpha`] IS the
//! client's mechanism. Consequences worth knowing: a child's later own-SetAlpha diverges from its
//! parent until the parent is set again (last write wins), and a frame created under a dimmed
//! parent starts at 1.0 (creation does not re-push the parent's alpha). Scale keeps its own
//! (verified multiplicative) propagation and Era `ignore_parent_scale` opt-out, inert in 1.12.

use std::collections::HashMap;

use crate::order::{DrawLayer, Strata};

/// The scale-propagation ε — `_DAT_008029d4` = `0x34800000` ≈ 2.384e-7 (`propagation.md`,
/// `effective_scale 0x76ac90`): a recomputed effective scale within ε of the current one is skipped
/// (no write, no recursion). This is the *same* binary constant the layout resolver uses for its
/// `OnSizeChanged` gate, so we reuse it rather than restate the value.
pub const SCALE_EPS: f64 = crate::layout::SIZE_EPS;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Handles — generational, so a destroyed-then-reused slot never aliases a stale handle
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A generational handle to a [`Frame`] in a [`WidgetArena`]. Copyable and comparable; a handle to a
/// destroyed frame is detected (the slot's generation moved on) and reads return `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameHandle {
    index: u32,
    generation: u32,
}

/// A generational handle to a [`Region`] (a texture or fontstring leaf).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RegionHandle {
    index: u32,
    generation: u32,
}

impl RegionHandle {
    /// The handle's identity as one integer — index and generation packed — for the layout change
    /// gate's fingerprint (`script::layout::InputFingerprint`), which must notice a region being
    /// swapped for a different one at the same map size.
    #[inline]
    pub(crate) fn fingerprint_bits(self) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(self.index)
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// A hand-rolled generational arena (no external deps — a slotmap in miniature)
// ─────────────────────────────────────────────────────────────────────────────────────────────

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// Minimal generational arena: `insert` reuses freed slots, `remove` bumps the slot's generation so
/// old handles stop resolving. Indices/generations are `u32` — plenty for a UI.
struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Arena<T> {
    fn new() -> Arena<T> {
        Arena {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    fn insert(&mut self, value: T) -> (u32, u32) {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            (index, slot.generation)
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 0,
                value: Some(value),
            });
            (index, 0)
        }
    }

    fn remove(&mut self, index: u32, generation: u32) -> Option<T> {
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        let taken = slot.value.take();
        if taken.is_some() {
            slot.generation = slot.generation.wrapping_add(1);
            self.free.push(index);
        }
        taken
    }

    fn get(&self, index: u32, generation: u32) -> Option<&T> {
        let slot = self.slots.get(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.value.as_ref()
    }

    fn get_mut(&mut self, index: u32, generation: u32) -> Option<&mut T> {
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.value.as_mut()
    }

    /// Iterate live entries as `(index, generation, &value)`.
    fn iter(&self) -> impl Iterator<Item = (u32, u32, &T)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.value.as_ref().map(|v| (i as u32, s.generation, v)))
    }

    /// Iterate live entries as `(index, generation, &mut value)`.
    fn iter_mut(&mut self) -> impl Iterator<Item = (u32, u32, &mut T)> + '_ {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| s.value.as_mut().map(|v| (i as u32, s.generation, v)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Widget kinds
// ─────────────────────────────────────────────────────────────────────────────────────────────

mod kinds;
pub use kinds::{
    ButtonState, CooldownState, EditBoxState, FrameKind, KindState, MessageLine, MinimapState,
    RegionKind, ScrollFrameState, ScrollingMessageState, SliderState, StatusBarState, TooltipState,
    COOLDOWN_FLASH_SECS, MINIMAP_DEFAULT_ZOOM, MINIMAP_ZOOM_LEVELS, TOOLTIP_DOUBLE_GAP,
    TOOLTIP_FADE_SECS, TOOLTIP_LINE_GAP, TOOLTIP_PAD, TOOLTIP_WRAP_WIDTH,
};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Frame + Region nodes
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A frame node in the arena — the modeled subset of `CSimpleFrame` (`frame-model.md`).
///
/// The invariant-bearing fields (`effective_visible`, `effective_scale`, `effective_alpha`) and the
/// structural fields (`parent`, `children`, `regions`) are maintained by the [`WidgetArena`]
/// mutators; treat them as **read-only** and change state through
/// [`WidgetArena::set_shown`]/[`set_frame_strata`](WidgetArena::set_frame_strata)/etc. so propagation
/// stays correct. (Not to be confused with [`crate::layout::LayoutInput`], which is the *layout resolver's*
/// per-frame anchor/size input — a separate concern.)
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// The widget subtype.
    pub kind: FrameKind,
    /// The frame's name (its global identifier), if any. See [`WidgetArena::lookup`] for the
    /// non-overwriting publish rule.
    pub name: Option<String>,
    /// The parent frame, or `None` for a top-level frame (attached to the screen root / `UIParent`).
    pub parent: Option<FrameHandle>,
    /// Child frames in **insertion order** (the client's `+0x300` child list order) — this order is
    /// the draw-order tiebreak within a `(strata, level)` bucket for equal-insertion children.
    pub children: Vec<FrameHandle>,
    /// Owned region leaves (textures/fontstrings), in creation order.
    pub regions: Vec<RegionHandle>,
    /// The draw stratum (`frameStrata +0xc0`, default MEDIUM).
    pub strata: Strata,
    /// The level within the stratum (`frameLevel +0xc4`, default 0).
    pub level: u16,
    /// This frame's own alpha, 0.0..=1.0 (`alpha +0xc8`, default 1.0).
    pub alpha: f32,
    /// The alpha a renderer should apply — always == [`Frame::alpha`] (see the module's alpha
    /// section: 1.12 cascades at set time, so there is no separate draw-time product). Kept as its
    /// propagated product (see the module tension note).
    pub effective_alpha: f32,
    /// `shown` bit (`shown +0xd0`, default true) — the frame's own Show/Hide state.
    pub shown: bool,
    /// `effectiveVisible +0xd4` — `shown AND parent-chain visible`. Maintained by propagation.
    pub effective_visible: bool,
    /// Whether the frame accepts the mouse (`EnableMouse`). Default **false** — WoW's `enableMouse`
    /// default; only mouse-enabled *and* effective-visible frames capture the cursor in
    /// [`crate::order::hit_test`]. (Keyboard focus / `EnableKeyboard` is a separate flag, out of
    /// scope in this crate for now.)
    pub mouse_enabled: bool,
    /// Clamp-to-screen (`SetClampedToScreen` / XML `clampedToScreen` — the client's geometry
    /// flags **bit4**, applied inside rect assembly `0x767a20`, wow-re `layout.md`): the layout
    /// resolve shifts this frame's assembled rect back inside the screen, size preserved.
    /// Default **true for GameTooltip frames**, false otherwise: the reference tooltip observably
    /// clamps (the minimap zone-text hover's ANCHOR_LEFT plate hangs down from the screen top
    /// instead of leaving the window) though its XML never sets the attribute — the class supplies
    /// the flag — and the rule here is absolute: no tooltip ever leaves the window (decision 0352).
    pub clamped_to_screen: bool,
    /// `SetHitRectInsets` / XML `<HitRectInsets>` — how far the frame's **mouse** rect is inset
    /// from its resolved rect, as `[left, right, top, bottom]` (default all 0, i.e. the hit rect
    /// IS the frame rect). Only [`crate::order::hit_test`] reads it: drawing, anchoring, and
    /// `GetLeft`/`GetWidth` all stay on the un-inset rect. The reference uses it wherever a
    /// button's art fills less than its frame — the micro buttons are 29×58 frames whose art
    /// occupies only the lower ~40 px, and a `top="18"` inset is what stops that empty header
    /// from swallowing hover and clicks aimed past the bar.
    pub hit_rect_insets: [f32; 4],
    /// This frame's own scale (`ownScale +0xb8`, default 1.0).
    pub scale: f32,
    /// `layoutScale` = `parentEffective * ownScale`, ε-gated (`effective_scale 0x76ac90`).
    pub effective_scale: f32,
    /// Era `ignoreParentScale` opt-out — inert in 1.12 (no frame sets it; extension point).
    pub ignore_parent_scale: bool,
    /// The Lua-visible numeric id (`GetID`/`SetID`, XML `id=` — the client's `+0xb4`): an
    /// app-meaning-free label frames carry for their handlers (a dropdown row's list position, a
    /// tab index). Default 0. Distinct from the layout/wrapper id ([`Model`](crate::script::Model)'s
    /// bijection) — this one is data, not identity.
    pub wow_id: i64,
    /// Monotonic creation sequence — the frame's insertion order for the draw-order key. See the
    /// note on [`WidgetArena`] about how this relates to the client's live per-bucket insertion.
    pub insertion_seq: u32,
    /// Per-kind behavior state (StatusBar value model, …); [`KindState::None`] for plain kinds.
    pub kind_state: KindState,
    /// Next declaration index handed to a new region of this frame (region draw-order tiebreak).
    next_decl: u32,
}

/// A region leaf — a texture or fontstring belonging to one frame (`frame-model.md`).
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// Texture or fontstring.
    pub kind: RegionKind,
    /// The frame this region belongs to.
    pub owner: FrameHandle,
    /// The draw layer within the owner (BACKGROUND..HIGHLIGHT).
    pub draw_layer: DrawLayer,
    /// `textureSubLevel` — ordering within the draw layer (extension point beyond 1.12; see
    /// [`crate::order`]).
    pub sub_level: i8,
    /// Declaration index within the owner frame — the final within-layer draw tiebreak.
    pub decl_seq: u32,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The arena
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The frame arena: the store of all frames and regions, the name registry, and the propagation
/// mutators.
///
/// The **screen root** (`CSimpleTop` / `UIParent`) is *not* a node here — a frame with
/// `parent == None` is a top-level frame anchored to the always-visible, unit-scale screen root.
///
/// **Insertion / draw order.** Each frame gets a monotonic `insertion_seq` at creation, used as the
/// draw-order tiebreak within a `(strata, level)` bucket — and, like the client, a frame is
/// re-sequenced to its bucket's **tail** whenever it *becomes visible* (`effective_visible_show
/// 0x76ae10` re-ADDS to the intrusive level list) or a visible frame changes strata/level (both
/// setters remove-then-add; `propagation.md`). Show order IS draw order within a bucket — how the
/// reference's late-shown `MiniMapTrackingFrame` draws over the earlier-declared `MinimapBackdrop`
/// ring despite the XML declaring it first (decision 0557; the mutators live in [`propagation`]).
pub struct WidgetArena {
    frames: Arena<Frame>,
    regions: Arena<Region>,
    names: HashMap<String, FrameHandle>,
    next_insertion: u32,
}

impl Default for WidgetArena {
    fn default() -> WidgetArena {
        WidgetArena::new()
    }
}

impl WidgetArena {
    /// A new, empty arena in **1.12 mode** (alpha does not propagate).
    pub fn new() -> WidgetArena {
        WidgetArena {
            frames: Arena::new(),
            regions: Arena::new(),
            names: HashMap::new(),
            next_insertion: 0,
        }
    }

    // ── Read access ────────────────────────────────────────────────────────────────────────────

    /// Borrow a frame, or `None` if the handle is stale/unknown.
    pub fn frame(&self, h: FrameHandle) -> Option<&Frame> {
        self.frames.get(h.index, h.generation)
    }

    /// Mutably borrow a frame. Prefer the dedicated mutators for state that propagates
    /// (shown/strata/level/scale/alpha/parent) — direct edits bypass propagation.
    pub fn frame_mut(&mut self, h: FrameHandle) -> Option<&mut Frame> {
        self.frames.get_mut(h.index, h.generation)
    }

    /// Borrow a region, or `None` if the handle is stale/unknown.
    pub fn region(&self, h: RegionHandle) -> Option<&Region> {
        self.regions.get(h.index, h.generation)
    }

    /// Mutably borrow a region.
    pub fn region_mut(&mut self, h: RegionHandle) -> Option<&mut Region> {
        self.regions.get_mut(h.index, h.generation)
    }

    /// Iterate every live frame as `(handle, &Frame)` (arena order — [`crate::order::traversal`]
    /// sorts into draw order).
    pub fn iter_frames(&self) -> impl Iterator<Item = (FrameHandle, &Frame)> + '_ {
        self.frames
            .iter()
            .map(|(index, generation, f)| (FrameHandle { index, generation }, f))
    }

    /// Iterate every live frame as `(handle, &mut Frame)` — for per-kind state the host advances each
    /// tick (the ScrollingMessageFrame fade). Structural fields stay the arena's to mutate through its
    /// propagation setters; this is for [`Frame::kind_state`] only.
    pub fn iter_frames_mut(&mut self) -> impl Iterator<Item = (FrameHandle, &mut Frame)> + '_ {
        self.frames
            .iter_mut()
            .map(|(index, generation, f)| (FrameHandle { index, generation }, f))
    }

    /// Move `h` to the **tail** of its `(strata, level)` draw bucket — the client's own live-list
    /// maintenance (`propagation.md`): `effective_visible_show 0x76ae10` re-ADDS a frame to its
    /// level's intrusive list on the hidden→visible transition, and `set_frame_strata 0x76a470` /
    /// `set_frame_level 0x76a4f0` remove-then-add a visible frame — an intrusive-list add appends.
    /// Called by the [`propagation`] mutators on exactly those transitions (module doc).
    pub(crate) fn resequence_to_tail(&mut self, h: FrameHandle) {
        // The seq lives in a 19-bit field of the packed `ZKey`; unbounded show-bumping could
        // exhaust it over a marathon session, so at the cap renumber every frame in its current
        // order (order-preserving, so no visible change) instead of tripping ZKey's assert.
        if self.next_insertion >= (1 << crate::order::INSERTION_BITS) {
            let mut order: Vec<FrameHandle> =
                self.iter_frames().map(|(handle, _)| handle).collect();
            order.sort_by_key(|&fh| self.frame(fh).map_or(0, |f| f.insertion_seq));
            for (i, fh) in order.iter().enumerate() {
                if let Some(f) = self.frame_mut(*fh) {
                    f.insertion_seq = i as u32;
                }
            }
            self.next_insertion = order.len() as u32;
        }
        let seq = self.next_insertion;
        if let Some(f) = self.frame_mut(h) {
            f.insertion_seq = seq;
            self.next_insertion += 1;
        }
    }

    /// Resolve a name to the frame that published it, or `None`. Publishing is **non-overwriting**:
    /// the first frame created with a given name owns it; a later duplicate keeps its own `name`
    /// field but does not become the lookup target (matching the client's auto-publish rule,
    /// `propagation.md`/decision 0068).
    pub fn lookup(&self, name: &str) -> Option<FrameHandle> {
        self.names.get(name).copied()
    }

    // ── Create / destroy ────────────────────────────────────────────────────────────────────────

    /// Create a frame of `kind`, with an optional `name` and `parent`. A `parent` handle that is not
    /// live is treated as `None` (top-level). The new frame starts `shown`, strata MEDIUM, level 0,
    /// scale 1.0, alpha 1.0; its `effective_visible`/`effective_scale`/`effective_alpha` are computed
    /// from the parent immediately. If `name` is given and not already published, it is published.
    ///
    /// (Strata/level inheritance from the parent at create time is a *loader/CreateFrame* concern,
    /// not the ctor's — the client's ctor writes MEDIUM/0 unconditionally, `frame-model.md`; we match
    /// that and leave inheritance to the layer above.)
    pub fn create(
        &mut self,
        kind: FrameKind,
        name: Option<String>,
        parent: Option<FrameHandle>,
    ) -> FrameHandle {
        // Only keep a parent that is actually live.
        let parent = parent.filter(|&p| self.frame(p).is_some());
        let parent_visible = parent.is_none_or(|p| self.frame(p).unwrap().effective_visible);
        let parent_scale = parent.map_or(1.0, |p| self.frame(p).unwrap().effective_scale);

        let insertion_seq = self.next_insertion;
        self.next_insertion += 1;

        let shown = true;
        let scale = 1.0f32;
        // Alpha starts at 1.0 regardless of the parent's — creation does not re-push a dimmed
        // parent's value (only SetAlpha cascades; module alpha section).
        let alpha = 1.0f32;
        let effective_alpha = alpha;

        let frame = Frame {
            kind,
            name: name.clone(),
            parent,
            children: Vec::new(),
            regions: Vec::new(),
            strata: Strata::default(),
            level: 0,
            alpha,
            effective_alpha,
            shown,
            effective_visible: shown && parent_visible,
            // A button/editbox is mouse-enabled by construction (the client's CSimpleButton and
            // CSimpleEditBox enable input in their ctors — an XML button needs no enableMouse attr
            // to be clickable, and an editbox must take clicks to focus, RF-0082 §1).
            mouse_enabled: matches!(
                kind,
                FrameKind::Button
                    | FrameKind::CheckButton
                    | FrameKind::EditBox
                    // The scroll frame takes the wheel in its ctor (msgframe-runtime.md) — an XML
                    // ScrollingMessageFrame is wheel-interactive with no enableMouse attr, exactly
                    // like a Button is clickable without one.
                    | FrameKind::ScrollingMessageFrame
                    // A plain ScrollFrame likewise takes the wheel/drag by construction (decision
                    // 0112) — an XML ScrollFrame is scroll-interactive with no enableMouse attr,
                    // the same rationale as the ScrollingMessageFrame above.
                    | FrameKind::ScrollFrame
                    // A Slider takes the mouse in its ctor too (the thumb must be draggable): every
                    // scrollbar is a UIPanelScrollBarTemplate Slider that declares no enableMouse yet
                    // is draggable in-game (decision 0250), so the enablement is by construction.
                    | FrameKind::Slider
            ),
            // A tooltip clamps to the screen by construction (its XML never sets the attribute,
            // yet the reference plate observably never leaves the window — the class supplies
            // geometry flags bit4; decision 0352's law: no tooltip off-screen, ever).
            clamped_to_screen: matches!(kind, FrameKind::GameTooltip),
            // No frame is born with an inset hit rect — `<HitRectInsets>`/SetHitRectInsets is the
            // only source (the client's ctor zeroes the four floats too).
            hit_rect_insets: [0.0; 4],
            scale,
            effective_scale: parent_scale * scale,
            ignore_parent_scale: false,
            wow_id: 0,
            insertion_seq,
            kind_state: match kind {
                FrameKind::StatusBar => KindState::StatusBar(StatusBarState::default()),
                FrameKind::Button | FrameKind::CheckButton => {
                    KindState::Button(ButtonState::default())
                }
                FrameKind::EditBox => KindState::EditBox(EditBoxState::default()),
                FrameKind::ScrollingMessageFrame => {
                    KindState::ScrollingMessage(kinds::ScrollingMessageState::default())
                }
                FrameKind::ScrollFrame => KindState::Scroll(kinds::ScrollFrameState::default()),
                FrameKind::Slider => KindState::Slider(kinds::SliderState::default()),
                FrameKind::Minimap => KindState::Minimap(kinds::MinimapState::default()),
                FrameKind::Cooldown => KindState::Cooldown(kinds::CooldownState::default()),
                FrameKind::GameTooltip => KindState::Tooltip(kinds::TooltipState::default()),
                _ => KindState::None,
            },
            next_decl: 0,
        };

        let (index, generation) = self.frames.insert(frame);
        let handle = FrameHandle { index, generation };

        if let Some(p) = parent {
            self.frame_mut(p)
                .expect("live parent")
                .children
                .push(handle);
        }
        if let Some(n) = name {
            // Non-overwriting: first writer wins.
            self.names.entry(n).or_insert(handle);
        }
        handle
    }

    /// Destroy a frame and its whole subtree (child frames and all regions), unpublishing any names
    /// it owns and detaching it from its parent's child list. Stale handles are ignored.
    pub fn destroy(&mut self, h: FrameHandle) {
        let Some(frame) = self.frame(h) else {
            return;
        };
        let children = frame.children.clone();
        let regions = frame.regions.clone();
        let parent = frame.parent;
        let name = frame.name.clone();

        for c in children {
            self.destroy(c);
        }
        for r in regions {
            self.regions.remove(r.index, r.generation);
        }
        if let Some(p) = parent {
            if let Some(pf) = self.frame_mut(p) {
                pf.children.retain(|&c| c != h);
            }
        }
        if let Some(n) = name {
            if self.names.get(&n) == Some(&h) {
                self.names.remove(&n);
            }
        }
        self.frames.remove(h.index, h.generation);
    }

    /// Create a region leaf owned by `owner`. Returns `None` if `owner` is stale.
    pub fn create_region(
        &mut self,
        owner: FrameHandle,
        kind: RegionKind,
        draw_layer: DrawLayer,
        sub_level: i8,
    ) -> Option<RegionHandle> {
        let decl_seq = {
            let f = self.frame_mut(owner)?;
            let d = f.next_decl;
            f.next_decl += 1;
            d
        };
        let (index, generation) = self.regions.insert(Region {
            kind,
            owner,
            draw_layer,
            sub_level,
            decl_seq,
        });
        let handle = RegionHandle { index, generation };
        self.frame_mut(owner)
            .expect("live owner")
            .regions
            .push(handle);
        Some(handle)
    }
}

// The propagation mutators (visibility/strata/level/scale/alpha/reparenting) — split out for size;
// see `propagation`'s module doc.
mod propagation;

#[cfg(test)]
mod tests;
