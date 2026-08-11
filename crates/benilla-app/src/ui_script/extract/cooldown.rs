//! The Cooldown widget's pixels (decision 0137 phase 4): the radial dark wipe + the finish
//! flash, rebuilt natively from the byte-read `UI-Cooldown-Indicator.m2`. Split from [`super`]
//! purely for size — [`cooldown_quads`] is the only face, called from the extract loop's
//! `QuadContent::Cooldown` arm.

use bevy::prelude::*;

use crate::ui_pass::UiQuad;
use benilla_assets::WorldAssets;

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
pub(super) fn cooldown_quads(
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
