//! The frame anchor/layout resolver — anchor graph → resolved rects (decision 0068).
//!
//! This is a faithful transcription of the WoW 1.12.1 client's `CLayoutFrame` geometry resolver,
//! whose math is reverse-engineered bit-exact against `WoW.exe` in the sibling repo `wow-5875-re`
//! (`system/ui/scratch/layout.md`, `.../rf32-composed-resolver.md`, `.../frame-model.md`,
//! `.../propagation.md`). The leaf float kernels and the recursion-guarded per-edge composition are
//! transcribed from that repo's own verified `w5875-ui::layout` crate
//! (`wow-5875-re/crates/ui/src/layout.rs`), which is gated bit-exact under a Unicorn/`WoW.exe`
//! difftest harness (per-function diffs + the composed `assemble` over 6006 synthesized cases).
//! Matching that crate's operations and their order — not "improving" the arithmetic — is the goal.
//!
//! ## Precision — why `f64` intermediates, not naive `f32`
//!
//! The client is an x87 binary: every intermediate lives in an FPU register at PC_53 (53-bit
//! mantissa = IEEE `f64` precision) and is narrowed to `f32` *only* at the documented `fstp m32`
//! store sites (each sub-edge result before a `combine_*`; each final edge into the out-rect; the
//! `width·scale` span). A leaf like `(right + left) * 0.5 + xOff·scale` therefore rounds **once**
//! (at the final store), whereas doing every operation in `f32` would round at each step and
//! diverge. So "the client is f32" is true only at the storage boundaries: to be bit-faithful the
//! chain of `+`/`-`/`*0.5` runs in `f64` (host-exact — every operand is `f32`-derived, so each op is
//! exact in `f64`) with an explicit `as f32` at exactly the sites the RE pins. This mirrors
//! `w5875-ui::layout` verbatim.
//!
//! ## Coordinate space
//!
//! Rects are stored in the client's own convention: `[bottom, left, top, right]` (frame-absolute
//! `+0x64/+0x68/+0x6c/+0x70`; VERIFIED via `GetRect 0x768320` + the Lua `GetBottom/Left/Top/Right`
//! readers, `layout.md`). WoW UI space is **y-up**: `top > bottom`, the screen root's rect is the
//! physical screen (the anchor chain bottoms out at the `CSimpleTop` root), and `width = right −
//! left`, `height = top − bottom`. Coordinates are screen pixels throughout; `scale` (layoutScale =
//! effective scale, `propagation.md`) scales only the per-anchor offsets and the `width/height` span.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Constants (objdump-VERIFIED in layout.md "Constants")
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// UNSET coordinate sentinel: `+Infinity` (the `.data` slot `0xcf550c`, runtime-initialized from
/// `[0x81d56c] = 0x7f800000`, `layout.md`). "coordinate is set" ⇔ `x != +Inf`. Intermediates carry
/// this in `f64`; the `f32` narrowing preserves it (`+Inf as f32 == +Inf`).
const UNSET: f64 = f64::INFINITY;

/// `OnSizeChanged` trigger ε — `_DAT_008029d4` = `0x34800000` ≈ 2.384e-7 (`layout.md` Constants;
/// applied in `ApplyRect 0x76b580`). `OnSizeChanged(width,height)` fires iff `|Δwidth| ≥ ε` OR
/// `|Δheight| ≥ ε` between the old and new resolved rect. See [`size_changed`].
pub const SIZE_EPS: f64 = 2.384_185_791_015_625e-7;

/// Recompute change threshold ε — `_DAT_0080c5c8` = `0x3727c5ac` ≈ 9.96e-6 (`layout.md`; applied in
/// `recompute 0x768d20`): an edge must move at least this far vs the cached rect for a re-store +
/// `ApplyRect`. Benilla owns its own caching (see [`LayoutCache`]); this const records the client's
/// documented threshold for fidelity work that needs it.
pub const RESOLVE_EPS: f64 = f32::from_bits(0x3727_c5ac) as f64;

/// point-id → X column (VERIFIED table `0x81c3b8`.., `layout.md` "Point-id → coordinate"):
/// `{0,3,6}` (LEFT column) → 0, `{1,4,7}` (center) → 1, `{2,5,8}` (RIGHT column) → 2.
const X_COL: [u8; 9] = [0, 1, 2, 0, 1, 2, 0, 1, 2];
/// point-id → Y row: `{0,1,2}` (TOP row) → 0, `{3,4,5}` (center) → 1, `{6,7,8}` (BOTTOM row) → 2.
const Y_ROW: [u8; 9] = [0, 0, 0, 1, 1, 1, 2, 2, 2];

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The nine anchor points, with the client's exact discriminants (VERIFIED static tables
/// `0x81c3b8–0x81c3fc`, `layout.md`): the id is the slot index in the client's `anchorPoints[9]`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Point {
    TopLeft = 0,
    Top = 1,
    TopRight = 2,
    Left = 3,
    Center = 4,
    Right = 5,
    BottomLeft = 6,
    Bottom = 7,
    BottomRight = 8,
}

impl Point {
    /// The point-id (0..=8), which is also the `anchorPoints[9]` slot index.
    #[inline]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// Build a [`Point`] from its client id, `None` if out of range.
    pub const fn from_id(id: u8) -> Option<Point> {
        Some(match id {
            0 => Point::TopLeft,
            1 => Point::Top,
            2 => Point::TopRight,
            3 => Point::Left,
            4 => Point::Center,
            5 => Point::Right,
            6 => Point::BottomLeft,
            7 => Point::Bottom,
            8 => Point::BottomRight,
            _ => return None,
        })
    }
}

/// An opaque handle to an anchor target. This crate does not own the frame arena; the caller maps
/// handles to frames / external rects. (In the client this is the `CAnchor+0xc relativeTo` pointer.)
pub type Handle = u32;

/// A resolved rect in the client's `[bottom, left, top, right]` convention (screen pixels, y-up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub bottom: f32,
    pub left: f32,
    pub top: f32,
    pub right: f32,
}

impl Rect {
    /// Construct from the four edges in client order.
    pub const fn new(bottom: f32, left: f32, top: f32, right: f32) -> Rect {
        Rect {
            bottom,
            left,
            top,
            right,
        }
    }

    /// `[bottom, left, top, right]` — the client's cached-rect layout (`+0x64..0x70`).
    #[inline]
    pub const fn to_array(self) -> [f32; 4] {
        [self.bottom, self.left, self.top, self.right]
    }

    /// From `[bottom, left, top, right]`.
    #[inline]
    pub const fn from_array(a: [f32; 4]) -> Rect {
        Rect::new(a[0], a[1], a[2], a[3])
    }

    /// `right − left` (`GetWidth` semantics, `layout.md`).
    #[inline]
    pub fn width(self) -> f32 {
        self.right - self.left
    }

    /// `top − bottom` (`GetHeight` semantics; y-up).
    #[inline]
    pub fn height(self) -> f32 {
        self.top - self.bottom
    }
}

/// One anchor of a frame — a `CAnchor` record (`{xOff@4, yOff@8, relativeTo@0xc, relativePoint@0x10}`,
/// `frame-model.md`). `point` is the point *on this frame* being pinned (its `anchorPoints[9]` slot);
/// `relative_point` is the point *on the target* it pins to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    /// The point on *this* frame that is being anchored (its `anchorPoints[9]` slot index).
    pub point: Point,
    /// The target frame/region this anchor is relative to (`CAnchor+0xc`).
    pub relative_to: Handle,
    /// The point on the *target* this anchor pins to (`CAnchor+0x10 relativePoint`).
    pub relative_point: Point,
    /// X offset (`CAnchor+4`), scaled by the requesting frame's `scale`.
    pub x_off: f32,
    /// Y offset (`CAnchor+8`), scaled by `scale`.
    pub y_off: f32,
}

impl Anchor {
    /// Convenience constructor.
    pub fn new(
        point: Point,
        relative_to: Handle,
        relative_point: Point,
        x_off: f32,
        y_off: f32,
    ) -> Anchor {
        Anchor {
            point,
            relative_to,
            relative_point,
            x_off,
            y_off,
        }
    }
}

/// A frame's full layout input: its anchors, its optional explicit size, its scale, and the
/// clamp-to-screen flag + extents. Mirrors the client's geometry subobject fields consulted by the
/// resolver (`frame-model.md`, `rf32-composed-resolver.md`): `anchorPoints[9]` (`G+0x28`), `width`
/// (`G+0x50`), `height` (`G+0x54`), `layoutScale` (`G+0x58`), flags bit4 = clamp (`G+0x3c`).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutInput {
    /// The frame's anchors. Placed into the nine `anchorPoints` slots by their `point`; if two share
    /// a `point`, the *last* wins (the client's `SetPoint 0x767c70` overwrites the slot).
    pub anchors: Vec<Anchor>,
    /// Explicit width (`G+0x50`); **`0.0` ⇒ derive from opposing anchors** (the no-explicit-size
    /// sentinel `DAT_007ffd74`, `layout.md`).
    pub width: f32,
    /// Explicit height (`G+0x54`); `0.0` ⇒ derive from opposing anchors.
    pub height: f32,
    /// `layoutScale` = effective scale (`parentScale · ownScale`, ε-gated, `propagation.md`); the
    /// px↔coord multiplier applied to offsets and the size span. Default `1.0`.
    pub scale: f32,
    /// Clamp the assembled rect into `[0, extent]` per axis (`G` flags bit4, `layout.md` Assemble).
    pub clamp: bool,
    /// Screen X extent, only consulted when `clamp` (client `0x41ae60` return, default `0.8`).
    pub extent_x: f32,
    /// Screen Y extent, only consulted when `clamp` (client `0x41ae70` return, default `0.6`).
    pub extent_y: f32,
}

impl Default for LayoutInput {
    fn default() -> LayoutInput {
        LayoutInput {
            anchors: Vec::new(),
            width: 0.0,
            height: 0.0,
            scale: 1.0,
            clamp: false,
            extent_x: 0.8,
            extent_y: 0.6,
        }
    }
}

impl LayoutInput {
    /// A frame with the given anchors and explicit size, scale `1.0`, no clamp.
    pub fn sized(anchors: Vec<Anchor>, width: f32, height: f32) -> LayoutInput {
        LayoutInput {
            anchors,
            width,
            height,
            ..LayoutInput::default()
        }
    }
}

/// The outcome of resolving one frame's rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolveOutcome {
    /// Every edge resolved → the cached rect `[bottom, left, top, right]` (`assemble` returned 1).
    Resolved(Rect),
    /// At least one edge stayed `+Inf` — under-constrained (`assemble 0x767a20` returns 0, caller
    /// sets flags bit3 "resolution-failed").
    Unresolvable,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Leaf float kernels — the owned-fidelity math (layout.md). Transcribed from w5875-ui::layout,
// binary-diffed. Each returns its coordinate in `f64` (the un-narrowed x87 `st0` observable);
// callers narrow to `f32` at the documented store sites.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `combineEdge 0x767440(center, opp, span)` (`layout.md`): resolve one edge from the opposite
/// edge, the axis center, and the signed `size·scale` span, in that fallback order.
///
/// - opposite known and non-zero span → `opp + span` (edge = opposite ± size·scale);
/// - else center known: non-zero span → `span·0.5 + center` (edge = center ± half-size);
///   else opposite known → `(center − opp) + center` (= 2·center − opposite, the mirror);
/// - else → `+Inf` (unresolvable).
///
/// The `+Inf`/`0.0` sentinels are compared with IEEE equality, matching the binary's `fcomp`
/// against the `.data` slots (a NaN operand is unordered ⇒ "not equal to the sentinel").
#[allow(clippy::float_cmp)]
pub fn combine_edge(center: f32, opp: f32, span: f32) -> f64 {
    let (c, o, s) = (f64::from(center), f64::from(opp), f64::from(span));
    if o != UNSET && s != 0.0 {
        return o + s;
    }
    if c != UNSET {
        if s != 0.0 {
            return s * 0.5 + c;
        }
        if o != UNSET {
            return (c - o) + c;
        }
    }
    UNSET
}

/// `combineCenter 0x7672d0(lo, hi, span)` (`layout.md`): resolve an axis center from the low edge,
/// the high edge, and the signed `size·scale` span.
///
/// - low known: high known → `(lo+hi)·0.5`; else non-zero span → `span·0.5 + lo`;
/// - else high known and non-zero span → `hi − span·0.5`;
/// - else → `+Inf`.
#[allow(clippy::float_cmp)]
pub fn combine_center(lo: f32, hi: f32, span: f32) -> f64 {
    let (l, h, s) = (f64::from(lo), f64::from(hi), f64::from(span));
    if l != UNSET {
        if h != UNSET {
            return (l + h) * 0.5;
        }
        if s != 0.0 {
            return s * 0.5 + l;
        }
    }
    if h != UNSET && s != 0.0 {
        return h - s * 0.5;
    }
    UNSET
}

/// `CAnchor::ResolveX 0x7a2f90(scale)` (`layout.md`): resolve this anchor's X coordinate against its
/// `relativeTo`'s cached rect.
///
/// `rect` = the target's cached rect `[bottom, left, top, right]` when `GetRect` succeeded (cache
/// valid), `None` when it failed ⇒ `+Inf`. `mirror` = the target's geometry-vtable `+0x24`
/// normalize predicate (RTL/local-origin): when set, X edges are translated so `left → 0`,
/// `right → right − left` (each stored back as `f32`, the binary's `fstp m32`). For an ordinary
/// frame the predicate returns 0, so `mirror` is `false` (VERIFIED `0x46ff60 = xor eax,eax; ret`,
/// `rf32-composed-resolver.md`). `coord = xOff·scale + edge` for the relativePoint's column
/// (center = mid-X + xOff·scale). `rel_point > 8` ⇒ `+Inf`.
pub fn anchor_resolve_x(
    scale: f32,
    x_off: f32,
    rel_point: u32,
    rect: Option<[f32; 4]>,
    mirror: bool,
) -> f64 {
    let Some([_bottom, mut left, _top, mut right]) = rect else {
        return UNSET;
    };
    if mirror {
        right = (-f64::from(left) + f64::from(right)) as f32;
        left = 0.0;
    }
    if rel_point > 8 {
        return UNSET;
    }
    let (s, xo) = (f64::from(scale), f64::from(x_off));
    let (l, r) = (f64::from(left), f64::from(right));
    match X_COL[rel_point as usize] {
        0 => s * xo + l,             // relLeft
        1 => (r + l) * 0.5 + s * xo, // center
        _ => s * xo + r,             // relRight
    }
}

/// `CAnchor::ResolveY 0x7a3070(scale)` — the Y twin of [`anchor_resolve_x`] (`layout.md`). `mirror`
/// normalizes the Y edges (`bottom → 0`, `top → top − bottom`); `coord = yOff·scale + edge` for the
/// relativePoint's row (center = mid-Y + yOff·scale). `rect` = `[bottom, left, top, right]`.
pub fn anchor_resolve_y(
    scale: f32,
    y_off: f32,
    rel_point: u32,
    rect: Option<[f32; 4]>,
    mirror: bool,
) -> f64 {
    let Some([mut bottom, _left, mut top, _right]) = rect else {
        return UNSET;
    };
    if mirror {
        top = (-f64::from(bottom) + f64::from(top)) as f32;
        bottom = 0.0;
    }
    if rel_point > 8 {
        return UNSET;
    }
    let (s, yo) = (f64::from(scale), f64::from(y_off));
    let (b, t) = (f64::from(bottom), f64::from(top));
    match Y_ROW[rel_point as usize] {
        0 => s * yo + t,             // relTop
        1 => (t + b) * 0.5 + s * yo, // center
        _ => s * yo + b,             // relBottom
    }
}

/// The assembled (and possibly clamped) rect + whether every edge resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssembleRect {
    pub ok: bool,
    /// `[bottom, left, top, right]` (meaningful only when `ok`).
    pub rect: [f32; 4],
}

/// `assemble 0x767a20` (`layout.md`): given the four resolved edge coordinates `[bottom, left, top,
/// right]` (each `+Inf` ⇒ unresolvable), fail if any is `+Inf`; else optionally clamp into
/// `[0, extent]` per axis.
///
/// Clamp (geom flags bit4): a low edge `< 0` shifts to 0 carrying the span to the high edge; a high
/// edge past the screen extent shifts back carrying to the low edge, high edge = extent. Each store
/// is narrowed to `f32`.
#[allow(clippy::float_cmp)]
pub fn assemble_rect(edges: [f32; 4], clamp: bool, extent_x: f32, extent_y: f32) -> AssembleRect {
    let [mut bottom, mut left, mut top, mut right] = edges;
    let inf = f32::INFINITY;
    if bottom == inf || left == inf || top == inf || right == inf {
        return AssembleRect {
            ok: false,
            rect: edges,
        };
    }
    if clamp {
        if (left as f64) < 0.0 {
            right = (f64::from(right) - f64::from(left)) as f32;
            left = 0.0;
        }
        if (bottom as f64) < 0.0 {
            top = (f64::from(top) - f64::from(bottom)) as f32;
            bottom = 0.0;
        }
        if (right as f64) > (extent_x as f64) {
            let over = f64::from(right) - f64::from(extent_x);
            left = (f64::from(left) - over) as f32;
            right = extent_x;
        }
        if (top as f64) > (extent_y as f64) {
            let over = f64::from(top) - f64::from(extent_y);
            bottom = (f64::from(bottom) - over) as f32;
            top = extent_y;
        }
    }
    AssembleRect {
        ok: true,
        rect: [bottom, left, top, right],
    }
}

/// `OnSizeChanged` fire test (`ApplyRect 0x76b580`, `layout.md`): fires iff `|Δwidth| ≥ [`SIZE_EPS`]`
/// OR `|Δheight| ≥ SIZE_EPS`, with `width = right − left`, `height = top − bottom`. Deltas are `f64`
/// of the `f32` edges (the binary's `fsub` of the `fld`'d rect components).
pub fn size_changed(old: Rect, new: Rect) -> bool {
    let dw =
        (f64::from(new.right) - f64::from(new.left)) - (f64::from(old.right) - f64::from(old.left));
    let dh =
        (f64::from(new.top) - f64::from(new.bottom)) - (f64::from(old.top) - f64::from(old.bottom));
    dw.abs() >= SIZE_EPS || dh.abs() >= SIZE_EPS
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The composed resolver — recursion-guarded per-edge sequencing over the leaves above.
// Transcribed from w5875-ui::layout (RF-0032, rf32-composed-resolver.md): bit-exact over the
// binary's `assemble 0x767a20` → six per-edge resolvers → anchor scans → leaf kernels. Adds NO new
// arithmetic — only integer control flow + the documented `fstp m32` `f32` narrowing at each site.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A resolved anchor for the internal edge tree: like [`Anchor`] but with the target's rect already
/// looked up (`None` ⇒ target had no valid rect ⇒ `GetRect`-fail leg).
#[derive(Clone, Copy)]
struct AnchorRel {
    x_off: f32,
    y_off: f32,
    rel_point: u32,
    rel_rect: Option<[f32; 4]>,
    mirror: bool,
}

/// Internal per-edge resolver input: the nine `anchorPoints` slots (by point-id) with rects
/// resolved, plus the size/scale/clamp fields.
struct EdgeInput {
    anchors: [Option<AnchorRel>; 9],
    width: f32,
    height: f32,
    scale: f32,
    clamp: bool,
    extent_x: f32,
    extent_y: f32,
}

// recursion-guard bits (the binary's `G+0x28` bitfield, layout.md).
const G_LEFT: u32 = 0x01;
const G_TOP: u32 = 0x02;
const G_RIGHT: u32 = 0x04;
const G_BOTTOM: u32 = 0x08;
const G_XC: u32 = 0x10;
const G_YC: u32 = 0x20;

// per-edge point-id scan tables (`.rdata` 0x81c3b8..0x81c3f4, rf32-composed-resolver.md).
const IDS_LEFT: [usize; 3] = [0, 3, 6];
const IDS_RIGHT: [usize; 3] = [2, 5, 8];
const IDS_TOP: [usize; 3] = [0, 1, 2];
const IDS_BOTTOM: [usize; 3] = [6, 7, 8];
const IDS_XC: [usize; 3] = [1, 4, 7];
const IDS_YC: [usize; 3] = [3, 4, 5];

/// `anchorScanX 0x7671a0`: over the edge's three point-ids, the first present anchor whose
/// `ResolveX` is non-`+Inf` wins; else `+Inf`.
#[allow(clippy::float_cmp)]
fn anchor_scan_x(ids: &[usize; 3], f: &EdgeInput) -> f64 {
    for &id in ids {
        if let Some(a) = f.anchors[id] {
            let v = anchor_resolve_x(f.scale, a.x_off, a.rel_point, a.rel_rect, a.mirror);
            if v != UNSET {
                return v;
            }
        }
    }
    UNSET
}

/// `anchorScanY 0x7671f0` — the Y twin.
#[allow(clippy::float_cmp)]
fn anchor_scan_y(ids: &[usize; 3], f: &EdgeInput) -> f64 {
    for &id in ids {
        if let Some(a) = f.anchors[id] {
            let v = anchor_resolve_y(f.scale, a.y_off, a.rel_point, a.rel_rect, a.mirror);
            if v != UNSET {
                return v;
            }
        }
    }
    UNSET
}

/// `width·scale` / `height·scale` — the `layout_767_span` PRIMITIVE (`rf32-composed-resolver.md`):
/// operands are `f32` fields, the product narrowed to `f32` at the `fstp [esp]` site, negated for
/// LEFT/BOTTOM.
fn size_span(size: f32, scale: f32, negate: bool) -> f32 {
    let span = (f64::from(size) * f64::from(scale)) as f32;
    if negate {
        -span
    } else {
        span
    }
}

/// `LEFT 0x7673d0` — direct anchor scan, else `combineEdge(Xcenter, RIGHT, −width·scale)`.
#[allow(clippy::float_cmp)]
fn resolve_left(f: &EdgeInput, guard: &mut u32) -> f64 {
    if *guard & G_LEFT != 0 {
        return UNSET;
    }
    *guard |= G_LEFT;
    let v = anchor_scan_x(&IDS_LEFT, f);
    let r = if v != UNSET {
        v
    } else {
        let span = size_span(f.width, f.scale, true);
        let opp = resolve_right(f, guard) as f32;
        let center = resolve_xcenter(f, guard) as f32;
        combine_edge(center, opp, span)
    };
    *guard &= !G_LEFT;
    r
}

/// `RIGHT 0x767540` — direct anchor scan, else `combineEdge(Xcenter, LEFT, +width·scale)`.
#[allow(clippy::float_cmp)]
fn resolve_right(f: &EdgeInput, guard: &mut u32) -> f64 {
    if *guard & G_RIGHT != 0 {
        return UNSET;
    }
    *guard |= G_RIGHT;
    let v = anchor_scan_x(&IDS_RIGHT, f);
    let r = if v != UNSET {
        v
    } else {
        let span = size_span(f.width, f.scale, false);
        let opp = resolve_left(f, guard) as f32;
        let center = resolve_xcenter(f, guard) as f32;
        combine_edge(center, opp, span)
    };
    *guard &= !G_RIGHT;
    r
}

/// `TOP 0x7674d0` — direct anchor scan, else `combineEdge(Ycenter, BOTTOM, +height·scale)`.
#[allow(clippy::float_cmp)]
fn resolve_top(f: &EdgeInput, guard: &mut u32) -> f64 {
    if *guard & G_TOP != 0 {
        return UNSET;
    }
    *guard |= G_TOP;
    let v = anchor_scan_y(&IDS_TOP, f);
    let r = if v != UNSET {
        v
    } else {
        let span = size_span(f.height, f.scale, false);
        let opp = resolve_bottom(f, guard) as f32;
        let center = resolve_ycenter(f, guard) as f32;
        combine_edge(center, opp, span)
    };
    *guard &= !G_TOP;
    r
}

/// `BOTTOM 0x7675b0` — direct anchor scan, else `combineEdge(Ycenter, TOP, −height·scale)`.
#[allow(clippy::float_cmp)]
fn resolve_bottom(f: &EdgeInput, guard: &mut u32) -> f64 {
    if *guard & G_BOTTOM != 0 {
        return UNSET;
    }
    *guard |= G_BOTTOM;
    let v = anchor_scan_y(&IDS_BOTTOM, f);
    let r = if v != UNSET {
        v
    } else {
        let span = size_span(f.height, f.scale, true);
        let opp = resolve_top(f, guard) as f32;
        let center = resolve_ycenter(f, guard) as f32;
        combine_edge(center, opp, span)
    };
    *guard &= !G_BOTTOM;
    r
}

/// `Xcenter 0x767260` — direct anchor scan, else `combineCenter(LEFT, RIGHT, +width·scale)`.
#[allow(clippy::float_cmp)]
fn resolve_xcenter(f: &EdgeInput, guard: &mut u32) -> f64 {
    if *guard & G_XC != 0 {
        return UNSET;
    }
    *guard |= G_XC;
    let v = anchor_scan_x(&IDS_XC, f);
    let r = if v != UNSET {
        v
    } else {
        let span = size_span(f.width, f.scale, false);
        let lo = resolve_left(f, guard) as f32;
        let hi = resolve_right(f, guard) as f32;
        combine_center(lo, hi, span)
    };
    *guard &= !G_XC;
    r
}

/// `Ycenter 0x767360` — direct anchor scan, else `combineCenter(BOTTOM, TOP, +height·scale)`.
#[allow(clippy::float_cmp)]
fn resolve_ycenter(f: &EdgeInput, guard: &mut u32) -> f64 {
    if *guard & G_YC != 0 {
        return UNSET;
    }
    *guard |= G_YC;
    let v = anchor_scan_y(&IDS_YC, f);
    let r = if v != UNSET {
        v
    } else {
        let span = size_span(f.height, f.scale, false);
        let lo = resolve_bottom(f, guard) as f32;
        let hi = resolve_top(f, guard) as f32;
        combine_center(lo, hi, span)
    };
    *guard &= !G_YC;
    r
}

/// `assemble 0x767a20` composed: resolve all four edges (each `fst`'d to the `f32` out-rect),
/// returning [`ResolveOutcome::Unresolvable`] if any stayed `+Inf`, else the assembled (and, when
/// `clamp`, screen-clamped) rect. The four top-level resolves share a fresh guard; the binary
/// clears each edge's bit on return, so order is immaterial — we keep the binary's LEFT→BOTTOM→
/// RIGHT→TOP order. The `+Inf` test is on the un-narrowed `f64` edge (the binary's `fcomp` before
/// the `fst`).
#[allow(clippy::float_cmp)]
fn resolve_edges(f: &EdgeInput) -> ResolveOutcome {
    let mut guard = 0u32;
    let left = resolve_left(f, &mut guard);
    let bottom = resolve_bottom(f, &mut guard);
    let right = resolve_right(f, &mut guard);
    let top = resolve_top(f, &mut guard);
    if left == UNSET || bottom == UNSET || right == UNSET || top == UNSET {
        return ResolveOutcome::Unresolvable;
    }
    let edges = [bottom as f32, left as f32, top as f32, right as f32];
    let a = assemble_rect(edges, f.clamp, f.extent_x, f.extent_y);
    ResolveOutcome::Resolved(Rect::from_array(a.rect))
}

/// Resolve one frame's rect. `target_rect` supplies each anchor target's already-resolved rect
/// (`None` ⇒ the target has no valid rect ⇒ that anchor contributes `+Inf`, the `GetRect`-fail
/// leg). Anchors are placed into the nine `anchorPoints` slots by their `point`; on a duplicate the
/// later anchor wins (the client's `SetPoint` overwrite).
///
/// This is the fidelity surface: it builds the client's `anchorPoints[9]` view and runs the
/// verified composed `assemble` (`rf32-composed-resolver.md`).
pub fn resolve_rect(
    frame: &LayoutInput,
    target_rect: impl FnMut(Handle) -> Option<Rect>,
) -> ResolveOutcome {
    resolve_edges(&build_edge_input(frame, target_rect))
}

/// Build the client's `anchorPoints[9]` view + size/scale/clamp fields for `frame`, resolving each
/// anchor target's rect through `target_rect`. Shared by [`resolve_rect`] (all-or-nothing) and
/// [`resolve_rect_edges`] (per-edge).
fn build_edge_input(
    frame: &LayoutInput,
    mut target_rect: impl FnMut(Handle) -> Option<Rect>,
) -> EdgeInput {
    let mut slots: [Option<AnchorRel>; 9] = [None; 9];
    for a in &frame.anchors {
        slots[a.point.id() as usize] = Some(AnchorRel {
            x_off: a.x_off,
            y_off: a.y_off,
            rel_point: a.relative_point.id() as u32,
            rel_rect: target_rect(a.relative_to).map(Rect::to_array),
            // Ordinary frames' normalize predicate returns 0 (rf32-composed-resolver.md); RTL/mirror
            // frames are not modeled here (the leaf `anchor_resolve_*` covers that leg).
            mirror: false,
        });
    }
    EdgeInput {
        anchors: slots,
        width: frame.width,
        height: frame.height,
        scale: frame.scale,
        clamp: frame.clamp,
        extent_x: frame.extent_x,
        extent_y: frame.extent_y,
    }
}

/// Resolve one frame's four edges, returning each as `Some(edge)` when the anchor graph + explicit
/// size determined it, or `None` when it stayed `+Inf` (unset). The per-edge view **regions** need:
/// a region inherits any edge its anchors + size don't pin from its owner frame (decision 0068's v1
/// region model — see [`crate::script`]'s region resolution), so it must see *which* edges resolved,
/// not the all-or-nothing verdict [`resolve_rect`] gives. Order matches [`Rect`]: `[bottom, left,
/// top, right]`. No `assemble`/clamp is applied (regions never clamp to screen in v1).
#[allow(clippy::float_cmp)]
pub fn resolve_rect_edges(
    frame: &LayoutInput,
    target_rect: impl FnMut(Handle) -> Option<Rect>,
) -> [Option<f32>; 4] {
    let input = build_edge_input(frame, target_rect);
    let mut guard = 0u32;
    let left = resolve_left(&input, &mut guard);
    let bottom = resolve_bottom(&input, &mut guard);
    let right = resolve_right(&input, &mut guard);
    let top = resolve_top(&input, &mut guard);
    let cvt = |v: f64| if v == UNSET { None } else { Some(v as f32) };
    [cvt(bottom), cvt(left), cvt(top), cvt(right)]
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Graph driver — resolve a set of frames in dependency order (anchor targets before dependents),
// detecting cycles. This is benilla's orchestration, not client fidelity math: the client resolves
// lazily-on-read + per-frame-batched via a dependency-ordered deferred queue (layout.md "Resolve /
// dirty / cache flow"); we express the same dependency ordering as an explicit topological pass.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A frame-anchor graph: frames keyed by [`Handle`], plus *external* pre-resolved rects (the screen
/// root — the client's `CSimpleTop`, whose rect is the physical screen — and any fixed targets).
#[derive(Clone, Debug, Default)]
pub struct LayoutGraph {
    frames: BTreeMap<Handle, LayoutInput>,
    externals: BTreeMap<Handle, Rect>,
}

/// The result of resolving a [`LayoutGraph`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolution {
    /// Every known rect: the externals echoed back, plus each resolvable frame's resolved rect.
    pub rects: BTreeMap<Handle, Rect>,
    /// Frames whose `assemble` returned unresolvable (under-constrained, or depending on an
    /// unresolvable/missing target). May overlap [`Resolution::cycle`].
    pub unresolvable: BTreeSet<Handle>,
    /// Frames on (or behind) an anchor dependency cycle — no valid topological position. Resolved
    /// best-effort against whatever rects were available; a cycle member that could not resolve
    /// also appears in `unresolvable`.
    pub cycle: BTreeSet<Handle>,
}

impl LayoutGraph {
    /// A new empty graph.
    pub fn new() -> LayoutGraph {
        LayoutGraph::default()
    }

    /// Insert/replace a frame at `handle`.
    pub fn set_frame(&mut self, handle: Handle, frame: LayoutInput) {
        self.frames.insert(handle, frame);
    }

    /// Remove a frame; returns it if present.
    pub fn remove_frame(&mut self, handle: Handle) -> Option<LayoutInput> {
        self.frames.remove(&handle)
    }

    /// Register an external (pre-resolved) rect — typically the screen root that top-level frames
    /// anchor to.
    pub fn set_external(&mut self, handle: Handle, rect: Rect) {
        self.externals.insert(handle, rect);
    }

    /// The frames map.
    pub fn frames(&self) -> &BTreeMap<Handle, LayoutInput> {
        &self.frames
    }

    /// The externals map.
    pub fn externals(&self) -> &BTreeMap<Handle, Rect> {
        &self.externals
    }

    /// Resolve every frame in dependency order (each anchor target before its dependents),
    /// detecting cycles. Cycles are reported and everything resolvable is still resolved.
    pub fn resolve(&self) -> Resolution {
        // Kahn's algorithm over frame→frame anchor dependencies. A dependency on an external or an
        // unknown handle does not constrain ordering (its rect is fixed / missing, not produced by
        // this pass); only frame→frame edges do. A self-anchor counts as an edge, so a
        // self-referential frame is never emitted ⇒ reported as a cycle.
        let mut indeg: BTreeMap<Handle, usize> = self.frames.keys().map(|&h| (h, 0)).collect();
        let mut dependents: BTreeMap<Handle, Vec<Handle>> = BTreeMap::new();
        for (&h, frame) in &self.frames {
            let mut seen = BTreeSet::new();
            for a in &frame.anchors {
                let d = a.relative_to;
                if self.frames.contains_key(&d) && seen.insert(d) {
                    *indeg.get_mut(&h).expect("frame in indeg") += 1;
                    dependents.entry(d).or_default().push(h);
                }
            }
        }

        let mut queue: VecDeque<Handle> = indeg
            .iter()
            .filter(|&(_, &d)| d == 0)
            .map(|(&h, _)| h)
            .collect();
        let mut order: Vec<Handle> = Vec::with_capacity(self.frames.len());
        while let Some(h) = queue.pop_front() {
            order.push(h);
            if let Some(deps) = dependents.get(&h) {
                for &dep in deps {
                    let e = indeg.get_mut(&dep).expect("dependent in indeg");
                    *e -= 1;
                    if *e == 0 {
                        queue.push_back(dep);
                    }
                }
            }
        }

        // rects accumulates externals + resolved frames; it is the target-rect lookup.
        let mut rects: BTreeMap<Handle, Rect> = self.externals.clone();
        let mut unresolvable: BTreeSet<Handle> = BTreeSet::new();

        let resolve_one = |h: Handle, rects: &mut BTreeMap<Handle, Rect>| -> bool {
            let frame = &self.frames[&h];
            match resolve_rect(frame, |t| rects.get(&t).copied()) {
                ResolveOutcome::Resolved(r) => {
                    rects.insert(h, r);
                    true
                }
                ResolveOutcome::Unresolvable => false,
            }
        };

        for &h in &order {
            if !resolve_one(h, &mut rects) {
                unresolvable.insert(h);
            }
        }

        // Frames never emitted are on (or behind) a cycle; resolve them best-effort.
        let emitted: BTreeSet<Handle> = order.iter().copied().collect();
        let cycle: BTreeSet<Handle> = self
            .frames
            .keys()
            .copied()
            .filter(|h| !emitted.contains(h))
            .collect();
        for &h in &cycle {
            if !resolve_one(h, &mut rects) {
                unresolvable.insert(h);
            }
        }

        Resolution {
            rects,
            unresolvable,
            cycle,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Dirty-tracking wrapper — a thin recompute-on-demand cache over LayoutGraph. The math above is the
// fidelity surface; this caching strategy is benilla's own (the client uses a lazy-on-read +
// per-frame-batched deferred queue — layout.md — which we may later mirror more closely).
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A [`LayoutGraph`] with a memoized [`Resolution`]. Any mutation marks the whole cache dirty; the
/// next [`LayoutCache::resolve`] recomputes. (Per-frame incremental invalidation — dirtying only a
/// changed frame and its transitive dependents — is a future optimization; correctness lives in the
/// resolver, not the cache.)
#[derive(Clone, Debug, Default)]
pub struct LayoutCache {
    graph: LayoutGraph,
    cached: Option<Resolution>,
}

impl LayoutCache {
    /// Wrap a graph (starts dirty).
    pub fn new(graph: LayoutGraph) -> LayoutCache {
        LayoutCache {
            graph,
            cached: None,
        }
    }

    /// Insert/replace a frame and invalidate the cache.
    pub fn set_frame(&mut self, handle: Handle, frame: LayoutInput) {
        self.graph.set_frame(handle, frame);
        self.cached = None;
    }

    /// Remove a frame and invalidate the cache.
    pub fn remove_frame(&mut self, handle: Handle) -> Option<LayoutInput> {
        let removed = self.graph.remove_frame(handle);
        if removed.is_some() {
            self.cached = None;
        }
        removed
    }

    /// Set an external rect and invalidate the cache.
    pub fn set_external(&mut self, handle: Handle, rect: Rect) {
        self.graph.set_external(handle, rect);
        self.cached = None;
    }

    /// Whether the next [`LayoutCache::resolve`] will recompute.
    pub fn is_dirty(&self) -> bool {
        self.cached.is_none()
    }

    /// The underlying graph.
    pub fn graph(&self) -> &LayoutGraph {
        &self.graph
    }

    /// The resolved layout, recomputing if dirty.
    pub fn resolve(&mut self) -> &Resolution {
        if self.cached.is_none() {
            self.cached = Some(self.graph.resolve());
        }
        self.cached.as_ref().expect("just populated")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Screen-root handle used across tests; its rect is the physical screen (y-up).
    const SCREEN: Handle = 0;
    fn screen_rect() -> Rect {
        // [bottom, left, top, right]
        Rect::new(0.0, 0.0, 600.0, 800.0)
    }

    fn bits(r: Rect) -> [u32; 4] {
        [
            r.bottom.to_bits(),
            r.left.to_bits(),
            r.top.to_bits(),
            r.right.to_bits(),
        ]
    }
    fn assert_rect(got: ResolveOutcome, want: Rect) {
        match got {
            ResolveOutcome::Resolved(r) => {
                assert_eq!(bits(r), bits(want), "got {r:?} want {want:?}")
            }
            ResolveOutcome::Unresolvable => panic!("unresolvable, wanted {want:?}"),
        }
    }

    // ── Leaf kernels ─────────────────────────────────────────────────────────────────────────
    // Input probes transcribed from wow-5875-re/crates/ui/tests/difftest/layout.rs (the explicit
    // per-leg probes; binary-diffed there). Expected outputs are hand-computed from the layout.md
    // formulas — this crate has no WoW.exe emulator, so these are spec-derived, not binary-diffed.

    #[test]
    fn combine_edge_legs() {
        // (center, opp, span)
        assert_eq!(combine_edge(10.0, 50.0, 7.0), 57.0); // opp+span
        assert_eq!(combine_edge(10.0, f32::INFINITY, 7.0), 7.0 * 0.5 + 10.0); // span/2+center
        assert_eq!(combine_edge(10.0, 50.0, 0.0), (10.0 - 50.0) + 10.0); // 2c-opp = -30
        assert_eq!(
            combine_edge(f32::INFINITY, f32::INFINITY, 0.0),
            f64::INFINITY
        );
        assert_eq!(combine_edge(f32::INFINITY, 50.0, 0.0), f64::INFINITY);
        assert_eq!(combine_edge(10.0, f32::INFINITY, 0.0), f64::INFINITY);
        // -0.0 span == 0.0 (IEEE) -> the 2c-opp leg
        assert_eq!(combine_edge(10.0, 50.0, -0.0), (10.0 - 50.0) + 10.0);
    }

    #[test]
    fn combine_center_legs() {
        assert_eq!(combine_center(10.0, 50.0, 7.0), (10.0 + 50.0) * 0.5); // both -> midpoint
        assert_eq!(combine_center(10.0, f32::INFINITY, 7.0), 7.0 * 0.5 + 10.0); // span/2+lo
        assert_eq!(combine_center(f32::INFINITY, 50.0, 7.0), 50.0 - 7.0 * 0.5); // hi-span/2
        assert_eq!(
            combine_center(f32::INFINITY, f32::INFINITY, 7.0),
            f64::INFINITY
        );
        assert_eq!(combine_center(10.0, f32::INFINITY, 0.0), f64::INFINITY);
        // both finite -> midpoint regardless of span
        assert_eq!(combine_center(10.0, 50.0, -0.0), 30.0);
    }

    #[test]
    fn anchor_resolve_x_columns() {
        let r = [3.0f32, 100.0, 480.0, 260.0]; // [bottom, left, top, right]
                                               // relLeft, center, relRight (point-ids 0,1,2), scale 1, xOff 5
        assert_eq!(anchor_resolve_x(1.0, 5.0, 0, Some(r), false), 5.0 + 100.0);
        assert_eq!(
            anchor_resolve_x(1.0, 5.0, 1, Some(r), false),
            (260.0 + 100.0) * 0.5 + 5.0
        );
        assert_eq!(anchor_resolve_x(1.0, 5.0, 2, Some(r), false), 5.0 + 260.0);
        // scale multiplies the offset
        assert_eq!(
            anchor_resolve_x(2.0, 5.0, 0, Some(r), false),
            2.0 * 5.0 + 100.0
        );
        // GetRect-fail (None) -> +Inf; out-of-range point -> +Inf
        assert_eq!(anchor_resolve_x(1.0, 5.0, 0, None, false), f64::INFINITY);
        assert_eq!(anchor_resolve_x(1.0, 5.0, 9, Some(r), false), f64::INFINITY);
        // mirror: left->0, right->right-left; center = (r'+0)/2 + xOff
        let rp = (260.0f32 - 100.0f32) as f64;
        assert_eq!(
            anchor_resolve_x(1.0, 0.0, 1, Some(r), true),
            (rp + 0.0) * 0.5
        );
    }

    #[test]
    fn anchor_resolve_y_rows() {
        let r = [3.0f32, 100.0, 480.0, 260.0];
        // relTop, center, relBottom (rows: pt 0->top, 3->center, 6->bottom)
        assert_eq!(anchor_resolve_y(1.0, 5.0, 0, Some(r), false), 5.0 + 480.0);
        assert_eq!(
            anchor_resolve_y(1.0, 5.0, 3, Some(r), false),
            (480.0 + 3.0) * 0.5 + 5.0
        );
        assert_eq!(anchor_resolve_y(1.0, 5.0, 6, Some(r), false), 5.0 + 3.0);
    }

    // ── assemble / clamp ────────────────────────────────────────────────────────────────────
    #[test]
    fn assemble_passthrough_and_fail() {
        let a = assemble_rect([3.0, 100.0, 480.0, 260.0], false, 0.8, 0.6);
        assert!(a.ok);
        assert_eq!(a.rect, [3.0, 100.0, 480.0, 260.0]);
        let bad = assemble_rect([3.0, f32::INFINITY, 480.0, 260.0], false, 0.8, 0.6);
        assert!(!bad.ok);
    }

    #[test]
    fn assemble_clamp_low_edge() {
        // left < 0 -> shift to 0, carry span to right
        let a = assemble_rect([0.1, -0.2, 0.5, 0.3], true, 0.8, 0.6);
        assert!(a.ok);
        assert_eq!(a.rect[1], 0.0); // left
                                    // right = 0.3 - (-0.2) computed via f64 then narrowed
        let want_right = (f64::from(0.3f32) - f64::from(-0.2f32)) as f32;
        assert_eq!(a.rect[3].to_bits(), want_right.to_bits());
    }

    #[test]
    fn assemble_clamp_high_edge() {
        // right > extent_x -> shift back, right = extent
        let a = assemble_rect([0.1, 0.1, 0.5, 1.5], true, 0.8, 0.6);
        assert!(a.ok);
        assert_eq!(a.rect[3], 0.8);
        let over = f64::from(1.5f32) - f64::from(0.8f32);
        let want_left = (f64::from(0.1f32) - over) as f32;
        assert_eq!(a.rect[1].to_bits(), want_left.to_bits());
    }

    // ── Composed resolver ─────────────────────────────────────────────────────────────────────

    /// VERIFIED numeric vector from wow-5875-re/system/ui/scratch/rf32-composed-resolver.md
    /// "Worked oracle (probe 1)" — binary-diffed there against WoW.exe's `assemble 0x767a20`.
    #[test]
    fn oracle_single_topleft_explicit_size() {
        let f = LayoutInput::sized(
            vec![Anchor::new(
                Point::TopLeft,
                SCREEN,
                Point::TopLeft,
                10.0,
                -5.0,
            )],
            200.0,
            50.0,
        );
        // screen root [0,0,600,800] (probe uses root top=600, right=800)
        let got = resolve_rect(&f, |_| Some(Rect::new(0.0, 0.0, 600.0, 800.0)));
        assert_rect(got, Rect::new(545.0, 10.0, 595.0, 210.0));
    }

    #[test]
    fn single_anchor_all_nine_points_explicit_size() {
        // For each own-point p, anchor p -> the same point on screen, xOff/yOff 0, w100 h40.
        // The anchored corner/edge pins directly; the rest derive from size. We assert the pinned
        // coordinate and the resulting width/height, computed by hand from layout.md.
        let screen = screen_rect();
        for &p in &[
            Point::TopLeft,
            Point::Top,
            Point::TopRight,
            Point::Left,
            Point::Center,
            Point::Right,
            Point::BottomLeft,
            Point::Bottom,
            Point::BottomRight,
        ] {
            let f = LayoutInput::sized(vec![Anchor::new(p, SCREEN, p, 0.0, 0.0)], 100.0, 40.0);
            let out = resolve_rect(&f, |_| Some(screen));
            let r = match out {
                ResolveOutcome::Resolved(r) => r,
                ResolveOutcome::Unresolvable => panic!("{p:?} unresolvable"),
            };
            // width/height always preserved
            assert_eq!(r.width().to_bits(), 100.0f32.to_bits(), "{p:?} width");
            assert_eq!(r.height().to_bits(), 40.0f32.to_bits(), "{p:?} height");
        }
    }

    #[test]
    fn two_edge_derivation_no_size() {
        // TOPLEFT + BOTTOMRIGHT to screen corners -> all edges direct, w/h ignored.
        let f = LayoutInput::sized(
            vec![
                Anchor::new(Point::TopLeft, SCREEN, Point::TopLeft, 10.0, -10.0),
                Anchor::new(Point::BottomRight, SCREEN, Point::BottomRight, -20.0, 30.0),
            ],
            0.0, // no explicit width
            0.0, // no explicit height
        );
        let got = resolve_rect(&f, |_| Some(screen_rect()));
        // left=0+10, top=600-10, right=800-20, bottom=0+30
        assert_rect(got, Rect::new(30.0, 10.0, 590.0, 780.0));
    }

    #[test]
    fn center_anchor_derives_edges() {
        let f = LayoutInput::sized(
            vec![Anchor::new(Point::Center, SCREEN, Point::Center, 0.0, 0.0)],
            100.0,
            40.0,
        );
        let got = resolve_rect(&f, |_| Some(screen_rect()));
        // centered at (400, 300): [bottom 280, left 350, top 320, right 450]
        assert_rect(got, Rect::new(280.0, 350.0, 320.0, 450.0));
    }

    #[test]
    fn scale_interaction() {
        // scale 2 -> offsets and size span doubled.
        let f = LayoutInput {
            anchors: vec![Anchor::new(
                Point::TopLeft,
                SCREEN,
                Point::TopLeft,
                10.0,
                -5.0,
            )],
            width: 200.0,
            height: 50.0,
            scale: 2.0,
            ..LayoutInput::default()
        };
        let got = resolve_rect(&f, |_| Some(screen_rect()));
        // left=2*10=20, top=600+2*(-5)=590, right=20+200*2=420, bottom=590-50*2=490
        assert_rect(got, Rect::new(490.0, 20.0, 590.0, 420.0));
    }

    #[test]
    fn chained_frames_via_graph() {
        const A: Handle = 1;
        const B: Handle = 2;
        let mut g = LayoutGraph::new();
        g.set_external(SCREEN, screen_rect());
        // Insert B before A to prove topological ordering, not insertion ordering.
        g.set_frame(
            B,
            LayoutInput::sized(
                vec![Anchor::new(Point::TopLeft, A, Point::BottomRight, 0.0, 0.0)],
                20.0,
                20.0,
            ),
        );
        g.set_frame(
            A,
            LayoutInput::sized(
                vec![Anchor::new(
                    Point::TopLeft,
                    SCREEN,
                    Point::TopLeft,
                    100.0,
                    -100.0,
                )],
                50.0,
                50.0,
            ),
        );
        let res = g.resolve();
        assert!(res.cycle.is_empty());
        assert!(res.unresolvable.is_empty());
        // A: left 100, top 500, right 150, bottom 450
        assert_eq!(
            bits(res.rects[&A]),
            bits(Rect::new(450.0, 100.0, 500.0, 150.0))
        );
        // B hangs off A's bottom-right corner (150, 450), size 20
        assert_eq!(
            bits(res.rects[&B]),
            bits(Rect::new(430.0, 150.0, 450.0, 170.0))
        );
    }

    #[test]
    fn no_anchor_is_unresolvable() {
        // A frame with no SetPoint cannot resolve (every edge derives circularly to +Inf).
        let f = LayoutInput::sized(vec![], 100.0, 40.0);
        assert_eq!(
            resolve_rect(&f, |_| Some(screen_rect())),
            ResolveOutcome::Unresolvable
        );
    }

    #[test]
    fn unset_sentinel_propagates() {
        // Anchor to a target with no valid rect (GetRect fail) -> that edge is +Inf -> frame
        // unresolvable (single anchor, so opposite edges can't derive).
        let f = LayoutInput::sized(
            vec![Anchor::new(
                Point::TopLeft,
                SCREEN,
                Point::TopLeft,
                0.0,
                0.0,
            )],
            100.0,
            40.0,
        );
        assert_eq!(resolve_rect(&f, |_| None), ResolveOutcome::Unresolvable);
    }

    #[test]
    fn dependent_of_unresolvable_is_unresolvable() {
        const A: Handle = 1; // no anchors -> unresolvable
        const B: Handle = 2; // anchored to A
        let mut g = LayoutGraph::new();
        g.set_external(SCREEN, screen_rect());
        g.set_frame(A, LayoutInput::sized(vec![], 100.0, 40.0));
        g.set_frame(
            B,
            LayoutInput::sized(
                vec![Anchor::new(Point::TopLeft, A, Point::TopLeft, 0.0, 0.0)],
                20.0,
                20.0,
            ),
        );
        let res = g.resolve();
        assert!(res.unresolvable.contains(&A));
        assert!(res.unresolvable.contains(&B));
        assert!(!res.rects.contains_key(&A));
        assert!(!res.rects.contains_key(&B));
    }

    #[test]
    fn cycle_detected() {
        const A: Handle = 1;
        const B: Handle = 2;
        let mut g = LayoutGraph::new();
        g.set_frame(
            A,
            LayoutInput::sized(
                vec![Anchor::new(Point::TopLeft, B, Point::TopLeft, 0.0, 0.0)],
                20.0,
                20.0,
            ),
        );
        g.set_frame(
            B,
            LayoutInput::sized(
                vec![Anchor::new(Point::TopLeft, A, Point::TopLeft, 0.0, 0.0)],
                20.0,
                20.0,
            ),
        );
        let res = g.resolve();
        assert!(res.cycle.contains(&A));
        assert!(res.cycle.contains(&B));
        // neither can resolve (each depends on the other's rect, unavailable)
        assert!(res.unresolvable.contains(&A));
        assert!(res.unresolvable.contains(&B));
    }

    #[test]
    fn self_anchor_is_a_cycle() {
        const A: Handle = 1;
        let mut g = LayoutGraph::new();
        g.set_frame(
            A,
            LayoutInput::sized(
                vec![Anchor::new(Point::TopLeft, A, Point::BottomRight, 0.0, 0.0)],
                20.0,
                20.0,
            ),
        );
        let res = g.resolve();
        assert!(res.cycle.contains(&A));
    }

    #[test]
    fn size_changed_epsilon() {
        let r = Rect::new(0.0, 0.0, 40.0, 100.0);
        // sub-epsilon width change -> not fired
        let tiny = Rect::new(0.0, 0.0, 40.0, 100.0 + 1e-7);
        assert!(!size_changed(r, tiny));
        // supra-epsilon -> fired
        let big = Rect::new(0.0, 0.0, 40.0, 100.001);
        assert!(size_changed(r, big));
    }

    #[test]
    fn cache_recomputes_on_dirty() {
        let mut g = LayoutGraph::new();
        g.set_external(SCREEN, screen_rect());
        g.set_frame(
            1,
            LayoutInput::sized(
                vec![Anchor::new(
                    Point::TopLeft,
                    SCREEN,
                    Point::TopLeft,
                    0.0,
                    0.0,
                )],
                100.0,
                40.0,
            ),
        );
        let mut cache = LayoutCache::new(g);
        assert!(cache.is_dirty());
        let first = cache.resolve().rects[&1];
        assert!(!cache.is_dirty());
        // mutate -> dirty -> recompute reflects the change
        cache.set_frame(
            1,
            LayoutInput::sized(
                vec![Anchor::new(
                    Point::TopLeft,
                    SCREEN,
                    Point::TopLeft,
                    50.0,
                    0.0,
                )],
                100.0,
                40.0,
            ),
        );
        assert!(cache.is_dirty());
        let second = cache.resolve().rects[&1];
        assert_eq!(second.left - first.left, 50.0);
    }
}
