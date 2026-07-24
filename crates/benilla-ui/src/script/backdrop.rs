//! The frame `Backdrop` mechanism — byte-faithful to `wow-5875-re`'s
//! `system/ui/scratch/backdrop-mechanism.md` (the RE derivation; every constant below cites its
//! section). A backdrop is the tiled bg + 8-piece border a `<Backdrop>` element or Lua `SetBackdrop`
//! installs on a frame (the tooltip/dialog/panel plate). This module owns the **data** ([`Backdrop`])
//! and the **geometry** ([`pieces`]); the Lua verbs live in [`super::object`] and the XML parse in
//! [`crate::loader`]. The renderer is the app's job — [`pieces`] hands it screen-space quads with
//! explicit per-corner UVs.

use crate::layout::Rect;

/// The default `edgeSize`, **logical** px: the ctor writes device `DAT_0081c9b0 = 0.025`, which is
/// logical 32 through the ×1/1024·0.8 logical→device transform (spec §Backdrop-object, `+0x48`, and
/// the usage string `edgeSize = 32`). Our layout is already in logical px, so we carry 32 directly.
pub const DEFAULT_EDGE_SIZE: f32 = 32.0;

/// The `BackgroundInsets` — the bg texture is the frame rect inset by these (the border is *not*
/// inset; it sits flush inside the frame rect). Default all 0 (spec §1). Field order matches the
/// struct offsets `+0x50 top / +0x54 bottom / +0x58 left / +0x5c right`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

/// A frame's installed backdrop (spec §Backdrop-object: the 0x68-byte struct at `frame+0x1ac`). The
/// field set is *exactly* what the compiled reader accepts — `bgFile, edgeFile, tile, tileSize,
/// edgeSize, insets` — plus the two colors (spec §1 "Field-set summary"). Colors default to opaque
/// **white** (the ctor `+0x60/+0x64 = 0xffffffff`); the XML `<Color>`/`<BorderColor>` *parser*
/// separately defaults a *present-but-partial* element to black, which the loader applies.
#[derive(Clone, Debug, PartialEq)]
pub struct Backdrop {
    /// The tiled background texture (`+0x24`); `None`/empty ⇒ no bg piece.
    pub bg_file: Option<String>,
    /// The 8-slice border texture (`+0x30`); `None`/empty ⇒ no border pieces.
    pub edge_file: Option<String>,
    /// `tile` (`+0x40`): tile the bg (else stretch it). Default false.
    pub tile: bool,
    /// `tileSize` (`+0x4c`), logical px; `0.0` ⇒ use `edge_size` as the tile period (spec §3). Default 0.
    pub tile_size: f32,
    /// `edgeSize` (`+0x48`), logical px — the border piece size and the edge tiling period. Default 32.
    pub edge_size: f32,
    /// `BackgroundInsets` (`+0x50..0x5c`). Default 0.
    pub insets: Insets,
    /// bg vertex color (`+0x60`, `SetBackdropColor`). Default opaque white.
    pub bg_color: [f32; 4],
    /// border vertex color (`+0x64`, `SetBackdropBorderColor`; all 8 pieces, never the bg). Default white.
    pub border_color: [f32; 4],
}

impl Default for Backdrop {
    fn default() -> Self {
        // The ctor (spec §Backdrop-object, 0x77e5f0): files empty, tile 0, tileSize 0,
        // edgeSize logical 32, insets 0, both colors 0xffffffff.
        Self {
            bg_file: None,
            edge_file: None,
            tile: false,
            tile_size: 0.0,
            edge_size: DEFAULT_EDGE_SIZE,
            insets: Insets::default(),
            bg_color: [1.0, 1.0, 1.0, 1.0],
            border_color: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

impl Backdrop {
    fn has_bg(&self) -> bool {
        self.bg_file.as_deref().is_some_and(|s| !s.is_empty())
    }
    fn has_border(&self) -> bool {
        self.edge_file.as_deref().is_some_and(|s| !s.is_empty())
    }
}

/// One drawable backdrop piece (the bg, or one of the 8 border pieces). Carries four explicit corner
/// **positions** (not a rect) so the shipped BR-inset bug's slant is representable, plus four explicit
/// per-corner **UVs** (the TOP/BOTTOM edges are rotated 90°, spec §3). Both arrays are in
/// `[top-left, top-right, bottom-right, bottom-left]` screen order (y-up, the layout convention).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackdropPiece {
    /// Corner positions in screen px, y-up: `[TL, TR, BR, BL]`.
    pub corners: [[f32; 2]; 4],
    /// Corner UVs (texture space), same `[TL, TR, BR, BL]` order.
    pub uvs: [[f32; 2]; 4],
    /// The texture needs REPEAT (wrap) addressing — the edge strips run UVs `[0..N]` and a tiled bg
    /// `[0..w/period]`. The corners' UVs stay in `[0,1]` but share the edge texture, so they carry
    /// `true` too (one texture identity ⇒ the 9 pieces batch, spec §2's shared edge texture).
    pub tile: bool,
    /// The bg piece (`true`) draws with [`Backdrop::bg_file`]/`bg_color`; a border piece (`false`)
    /// with `edge_file`/`border_color`. The app resolves the path + tint from the [`Backdrop`].
    pub is_bg: bool,
}

const SLICE: f32 = 0.125;

/// Axis-aligned corners `[TL, TR, BR, BL]` (y-up) for a piece spanning `x∈[x0,x1]`, `y∈[y0,y1]`
/// (`y1` = top). The bg overrides its own corners to reproduce the BR-inset bug.
fn aa_corners(x0: f32, x1: f32, y_bottom: f32, y_top: f32) -> [[f32; 2]; 4] {
    [
        [x0, y_top],    // TL
        [x1, y_top],    // TR
        [x1, y_bottom], // BR
        [x0, y_bottom], // BL
    ]
}

/// Compute the backdrop's drawable pieces for a frame whose resolved rect is `frame` (y-up), in the
/// client's paint order: the bg (layer BACKGROUND) first, then the 8 border pieces (layer BORDER) —
/// spec §2. Empty when neither file is set. All geometry is spec §2; all UVs spec §3.
pub fn pieces(frame: Rect, bd: &Backdrop) -> Vec<BackdropPiece> {
    let mut out = Vec::with_capacity(9);
    let e = bd.edge_size;
    let (l, r, b, t) = (frame.left, frame.right, frame.bottom, frame.top);

    // ── The background piece (spec §2 bg row) ──────────────────────────────────────────────────
    if bd.has_bg() {
        let (il, ir, it, ib) = (
            bd.insets.left,
            bd.insets.right,
            bd.insets.top,
            bd.insets.bottom,
        );
        // The four SetPoint anchors, y-up (spec §2 bg row). **⚠ BR uses the TOP inset**, not bottom
        // — the shipped 1.12.1 bug (spec §2 "The BR-inset quirk", bytes `8b 46 50` @ 0x77e9ae:
        // `SetPoint(8, frame+0x24, 8, −right, +top, 0)` loads `+0x50` = top, not `+0x54` = bottom).
        // Invisible whenever top==bottom (every shipping backdrop); reproduced for fidelity.
        let corners = [
            [l + il, t - it], // TL ← frame TL (+left, −top)
            [r - ir, t - it], // TR ← frame TR (−right, −top)
            [r - ir, b + it], // BR ← frame BR (−right, +top)  ⚠ bug: top inset
            [l + il, b + ib], // BL ← frame BL (+left, +bottom)
        ];
        // Tiled bg UVs (spec §3 bg row): period = tileSize, or edgeSize when tileSize==0. When not
        // tiled, no SetTexCoord runs and the bg keeps [0,1] (stretch).
        let uvs = if bd.tile {
            let period = if bd.tile_size != 0.0 { bd.tile_size } else { e };
            let bg_w = (r - ir) - (l + il);
            let bg_h = (t - it) - (b + ib);
            let (wt, ht) = (bg_w / period, bg_h / period);
            [[0.0, 0.0], [wt, 0.0], [wt, ht], [0.0, ht]]
        } else {
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
        };
        out.push(BackdropPiece {
            corners,
            uvs,
            tile: bd.tile,
            is_bg: true,
        });
    }

    // ── The 8 border pieces (spec §2 edge rows + §3 UV rows) ───────────────────────────────────
    if bd.has_border() {
        // Run lengths (spec §3): the edge strips tile `run` periods between the two corner squares
        // (no upper clamp — they TILE, never stretch); clamped to 0 below 2·edgeSize.
        let w_run = ((r - l) / e - 2.0).max(0.0);
        let h_run = ((t - b) / e - 2.0).max(0.0);

        // Helper: push an axis-aligned edge/corner piece.
        let mut edge = |x0: f32, x1: f32, y0: f32, y1: f32, uvs: [[f32; 2]; 4]| {
            out.push(BackdropPiece {
                corners: aa_corners(x0, x1, y0, y1),
                uvs,
                tile: true,
                is_bg: false,
            });
        };

        // LEFT (slice 0): upright, v ∈ [0, h_run].
        edge(
            l,
            l + e,
            b + e,
            t - e,
            [[0.0, 0.0], [SLICE, 0.0], [SLICE, h_run], [0.0, h_run]],
        );
        // RIGHT (slice 1): upright.
        edge(
            r - e,
            r,
            b + e,
            t - e,
            [
                [SLICE, 0.0],
                [2.0 * SLICE, 0.0],
                [2.0 * SLICE, h_run],
                [SLICE, h_run],
            ],
        );
        // TOP (slice 2): ROTATED — atlas-u→screen-Y (0.25 top, 0.375 bottom), atlas-v→screen-X
        // reversed (w_run at the LEFT end, 0 at the RIGHT).
        edge(
            l + e,
            r - e,
            t - e,
            t,
            [
                [2.0 * SLICE, w_run],
                [2.0 * SLICE, 0.0],
                [3.0 * SLICE, 0.0],
                [3.0 * SLICE, w_run],
            ],
        );
        // BOTTOM (slice 3): same rotation as TOP.
        edge(
            l + e,
            r - e,
            b,
            b + e,
            [
                [3.0 * SLICE, w_run],
                [3.0 * SLICE, 0.0],
                [4.0 * SLICE, 0.0],
                [4.0 * SLICE, w_run],
            ],
        );
        // Corners (slices 4..7): upright, v ∈ [0,1].
        let corner_uv = |i: f32| {
            let u0 = i * SLICE;
            let u1 = (i + 1.0) * SLICE;
            [[u0, 0.0], [u1, 0.0], [u1, 1.0], [u0, 1.0]]
        };
        edge(l, l + e, t - e, t, corner_uv(4.0)); // TOPLEFT
        edge(r - e, r, t - e, t, corner_uv(5.0)); // TOPRIGHT
        edge(l, l + e, b, b + e, corner_uv(6.0)); // BOTTOMLEFT
        edge(r - e, r, b, b + e, corner_uv(7.0)); // BOTTOMRIGHT
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tooltip_backdrop(edge_size: f32, inset: f32) -> Backdrop {
        Backdrop {
            bg_file: Some("Interface\\Tooltips\\UI-Tooltip-Background".into()),
            edge_file: Some("Interface\\Tooltips\\UI-Tooltip-Border".into()),
            tile: true,
            tile_size: edge_size,
            edge_size,
            insets: Insets {
                left: inset,
                right: inset,
                top: inset,
                bottom: inset,
            },
            ..Default::default()
        }
    }

    // The primary piece-geometry check: a 200×100 frame, edgeSize 16, insets 5/5/5/5 (the tooltip's
    // shape). Frame y-up rect = [bottom 0, left 0, top 100, right 200].
    #[test]
    fn piece_geometry_200x100_edge16_inset5() {
        let frame = Rect::new(0.0, 0.0, 100.0, 200.0);
        let bd = tooltip_backdrop(16.0, 5.0);
        let ps = pieces(frame, &bd);
        // bg + 8 border pieces.
        assert_eq!(ps.len(), 9);

        // bg: inset by 5 on every side; with symmetric insets the BR bug is invisible (rect exact).
        let bg = ps[0];
        assert!(bg.is_bg);
        assert_eq!(bg.corners[0], [5.0, 95.0]); // TL (left+5, top-5)
        assert_eq!(bg.corners[1], [195.0, 95.0]); // TR
        assert_eq!(bg.corners[2], [195.0, 5.0]); // BR (bottom+top_inset = 0+5)
        assert_eq!(bg.corners[3], [5.0, 5.0]); // BL (bottom+bottom_inset = 0+5)
                                               // Tiled bg UVs: period = tileSize 16; bg is 190×90 ⇒ 11.875 × 5.625 tiles.
        assert_eq!(bg.uvs[0], [0.0, 0.0]);
        assert!((bg.uvs[1][0] - 190.0 / 16.0).abs() < 1e-4);
        assert!((bg.uvs[2][1] - 90.0 / 16.0).abs() < 1e-4);

        // The border pieces are LEFT, RIGHT, TOP, BOTTOM, TL, TR, BL, BR (indices 1..9). The whole
        // border sits INSIDE the frame rect, flush, insets untouched.
        let left = ps[1];
        assert!(!left.is_bg && left.tile);
        // LEFT edge spans x∈[0,16], y∈[16,84] (b+e .. t-e); corners [TL,TR,BR,BL] y-up.
        assert_eq!(
            left.corners,
            [[0.0, 84.0], [16.0, 84.0], [16.0, 16.0], [0.0, 16.0]]
        );
        // h_run = 100/16 - 2 = 4.25; LEFT UVs upright, v ∈ [0, 4.25].
        assert_eq!(left.uvs[0], [0.0, 0.0]);
        assert_eq!(left.uvs[1], [0.125, 0.0]);
        assert!((left.uvs[3][1] - 4.25).abs() < 1e-4);

        // TOP (index 3): ROTATED. x∈[16,184], y∈[84,100]. w_run = 200/16 - 2 = 10.5.
        let top = ps[3];
        assert_eq!(top.corners[0], [16.0, 100.0]); // TL
        assert_eq!(top.corners[2], [184.0, 84.0]); // BR
                                                   // atlas-u→screen-Y: top row u=0.25, bottom row u=0.375.
        assert_eq!(top.uvs[0][0], 0.25); // TL u
        assert_eq!(top.uvs[3][0], 0.375); // BL u
                                          // atlas-v→screen-X reversed: LEFT end v=w_run, RIGHT end v=0.
        assert!((top.uvs[0][1] - 10.5).abs() < 1e-4); // TL v = w_run
        assert_eq!(top.uvs[1][1], 0.0); // TR v = 0

        // BOTTOMRIGHT corner (index 8): slice 7, upright, v∈[0,1], x∈[184,200], y∈[0,16].
        let br = ps[8];
        assert_eq!(br.corners[0], [184.0, 16.0]); // TL
        assert_eq!(br.uvs[0], [0.875, 0.0]);
        assert_eq!(br.uvs[2], [1.0, 1.0]);
    }

    // The BR-inset quirk (spec §2 ⚠, bytes 8b 46 50 @ 0x77e9ae): with top != bottom the bg's
    // BOTTOMRIGHT corner rides the TOP inset, not the bottom. Asymmetric insets make it assertable.
    #[test]
    fn bg_bottomright_rides_top_inset_bug() {
        let frame = Rect::new(0.0, 0.0, 100.0, 200.0);
        let bd = Backdrop {
            insets: Insets {
                left: 3.0,
                right: 4.0,
                top: 7.0,
                bottom: 9.0,
            },
            ..tooltip_backdrop(16.0, 0.0)
        };
        let bg = pieces(frame, &bd)[0];
        // BL uses the (correct) bottom inset: y = bottom + 9.
        assert_eq!(bg.corners[3], [3.0, 9.0]);
        // BR uses the TOP inset (the bug): y = bottom + 7, NOT bottom + 9.
        assert_eq!(bg.corners[2], [196.0, 7.0]);
        // The other three edges are regular: TL/TR ride the top inset (−7), left/right insets applied.
        assert_eq!(bg.corners[0], [3.0, 93.0]); // TL (left+3, top-7)
        assert_eq!(bg.corners[1], [196.0, 93.0]); // TR (right-4, top-7)
    }

    // Run clamp at tiny sizes: a frame narrower/shorter than 2·edgeSize clamps both runs to 0 (no
    // negative tiling), so the edge strips carry a zero-length run (spec §3, "clamped to 0 if < 0").
    #[test]
    fn run_clamps_to_zero_below_two_edges() {
        // 20×20 frame, edgeSize 16 ⇒ width/16 - 2 = -0.75 → 0.
        let frame = Rect::new(0.0, 0.0, 20.0, 20.0);
        let bd = tooltip_backdrop(16.0, 5.0);
        let ps = pieces(frame, &bd);
        let left = ps[1]; // LEFT edge
        assert_eq!(left.uvs[3][1], 0.0, "h_run clamped to 0"); // BL v
        let top = ps[3]; // TOP edge
        assert_eq!(top.uvs[0][1], 0.0, "w_run clamped to 0"); // TL v
    }

    // No files ⇒ no pieces; bg-only ⇒ 1; border-only ⇒ 8.
    #[test]
    fn presence_gates_pieces() {
        let frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(pieces(frame, &Backdrop::default()).is_empty());
        let bg_only = Backdrop {
            bg_file: Some("bg".into()),
            ..Default::default()
        };
        assert_eq!(pieces(frame, &bg_only).len(), 1);
        let border_only = Backdrop {
            edge_file: Some("edge".into()),
            ..Default::default()
        };
        assert_eq!(pieces(frame, &border_only).len(), 8);
    }
}
