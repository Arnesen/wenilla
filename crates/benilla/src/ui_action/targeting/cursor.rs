//! What the player *sees* while a cast waits for its click — the classifier pre-empt `0x4820f0`
//! (wow-re `cursor-system.md` §5) and the two numbers it computes: the ground point's range verdict
//! (`CheckGroundPointInRange 0x6e6810`) and the reticle's radius (`GetCurrentCastRadius 0x6e6350`).
//!
//! Both are *location* quantities, and that is the whole reason this is one small module rather
//! than a branch inside each seam: only the terrain seam has a point to judge, so the item and
//! GameObject seams take plain `Cast` and nothing here runs for them.

use bevy::prelude::*;

use benilla_formats::SpellRange;

use crate::net::SelfPlayer;
use crate::target::{CursorKind, PickOcclusion, WorldCursor};
use crate::ui_action::Spells;

use super::{SpellTargeting, TargetingWants};

/// `CheckGroundPointInRange 0x6e6810` — min²/max² from the spell's `SpellRange` row against the
/// squared caster↔point distance. Its ONE caller binary-wide is the hover-cursor classifier
/// (`0x4820f0` — wow-re `world-click-targeting.md` Q1's caller census): the verdict colours
/// Cast/UnableCast and nothing else. The click never consults it, so neither does ours. No row
/// (a failed DBC, an unknown spell) is permissive — the server validates every send anyway.
fn ground_point_in_range(row: Option<&SpellRange>, self_pos: Vec3, point: Vec3) -> bool {
    let Some(row) = row else {
        return true;
    };
    let dist_sq = self_pos.distance_squared(point);
    if row.min > 0.0 && dist_sq < row.min * row.min {
        return false;
    }
    dist_sq <= row.max * row.max
}

/// The targeting spell's `SpellRange` row, through the catalogs.
fn range_row(spells: Option<&Spells>, spell_id: u32) -> Option<&SpellRange> {
    let spells = spells?;
    spells.ranges.get(spells.catalog.get(spell_id)?.range_index)
}

/// `GetCurrentCastRadius 0x6e6350` (wow-re `ground-target-reticle.md` B2) — the reticle's
/// radius: per-effect `radius + casterLevel × perLevel` over **EffectRadiusIndex[0] and [1]
/// only** (slot 2 is never read by the client), the max with candidate 1 winning ties/NaN,
/// clamped to 20.0 (`0x4820f0`'s `[0x804478]` literal — `min`, NaN → 20). `0.0` = no radius
/// rows; the reticle then draws at its literal default size. Class-6 spell modifiers are
/// unmodelled (the 0792 residual, same as the range gate).
pub(crate) fn ground_cast_radius(spells: Option<&Spells>, spell_id: u32, level: u32) -> f32 {
    let Some(spells) = spells else { return 0.0 };
    let Some(d) = spells.catalog.get(spell_id) else {
        return 0.0;
    };
    let candidate = |slot: usize| -> f32 {
        let idx = d.effect_radius_index[slot];
        if idx == 0 {
            return 0.0;
        }
        spells
            .radii
            .get(idx)
            .map_or(0.0, |r| r.radius + level as f32 * r.per_level)
    };
    let (c0, c1) = (candidate(0), candidate(1));
    // Strict > for candidate 0; a tie or a NaN c0 falls to candidate 1 — the byte order.
    let r = if c0 > c1 { c0 } else { c1 };
    r.min(20.0)
}

/// While targeting, the world cursor is the classifier's pre-empt (`0x4820f0`, cursor-system
/// §5): **Cast** over an in-range ground point, **UnableCast** out of range / too close / no
/// ground hit at all (sky, mouselook). Runs right after [`crate::target`]'s classifier in the
/// target chain and overwrites its verdict — the ref runs this branch before the object
/// classifier ever executes, and the visible result is identical. Because it writes the *base*
/// [`WorldCursor`], it also pre-empts every UI overlay downstream ([`crate::cursor`]'s
/// repair/sell latches only arm while the base is Point) — which is the same total pre-emption
/// the reference's step 2 has.
///
/// The **item** and **GameObject** seams take plain `Cast`, never the grayed twin: the range
/// verdict below comes from `CheckGroundPointInRange 0x6e6810`, which is a *location* predicate
/// (its one caller binary-wide is this classifier, over the ground point), and neither an
/// item-targeting nor a lock word has a ground point to judge. Whether the reference grays either
/// per hovered slot / object is unpinned — the honest read is that their validity gate runs at BIND
/// time (`0x495d60`'s `0x0a` for an item; nothing at all for a GameObject, whose refusal is the
/// server's), not at hover time. Named INTERIM, decisions 0923 / 0939.
pub(crate) fn drive_targeting_cursor(
    targeting: Res<SpellTargeting>,
    occlusion: Res<PickOcclusion>,
    spells: Option<Res<Spells>>,
    self_tf: Query<&Transform, With<SelfPlayer>>,
    mut cursor: ResMut<WorldCursor>,
) {
    let Some(spell_id) = targeting.spell() else {
        return;
    };
    let unable = targeting.wants(TargetingWants::Location)
        && !match (occlusion.point, self_tf.single()) {
            (Some(point), Ok(tf)) => ground_point_in_range(
                range_row(spells.as_deref(), spell_id),
                tf.translation,
                point,
            ),
            _ => false,
        };
    *cursor = WorldCursor {
        kind: CursorKind::Cast,
        unable,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `0x6e6810` mirror: min²/max² against the squared caster↔point distance — the
    /// CURSOR's verdict and nothing else (its one caller binary-wide is the hover classifier;
    /// the click never asks). Permissive with no row (Blizzard's row 4 is 0–30 yd; a synthetic
    /// min exercises the too-close arm the real row can't).
    #[test]
    fn ground_point_in_range_mirrors_check_ground_point_in_range() {
        let row = |min: f32, max: f32| SpellRange { min, max, flags: 0 };
        let origin = Vec3::ZERO;
        let at = |d: f32| Vec3::new(d, 0.0, 0.0);
        let blizzard = row(0.0, 30.0);
        assert!(ground_point_in_range(Some(&blizzard), origin, at(29.9)));
        assert!(!ground_point_in_range(Some(&blizzard), origin, at(30.1)));
        let banded = row(8.0, 35.0);
        assert!(!ground_point_in_range(Some(&banded), origin, at(5.0)));
        assert!(ground_point_in_range(Some(&banded), origin, at(20.0)));
        // No row → permissive (the server still validates).
        assert!(ground_point_in_range(None, origin, at(500.0)));
    }

    /// `GetCurrentCastRadius 0x6e6350` + the `0x4820f0` clamp: slots 0/1 only (slot 2 is never
    /// read), max with candidate-1 winning ties, per-level scaling, min(r, 20). Fixture rows
    /// mirror the real table (row 14 = 8.0 Blizzard, row 8 = 5.0 Flamestrike).
    #[test]
    fn ground_cast_radius_mirrors_get_current_cast_radius() {
        use benilla_formats::{SpellDisplay, SpellRadius};
        use std::collections::HashMap;
        let mut spells = crate::ui_action::Spells::empty_for_tests();
        let display = |idx: [u32; 3]| SpellDisplay {
            effect_radius_index: idx,
            ..SpellDisplay::default()
        };
        spells.catalog = benilla_formats::SpellCatalog::from_displays(HashMap::from([
            (10, display([14, 0, 0])),
            (2120, display([8, 8, 0])),
            (777, display([0, 0, 13])), // slot 2 only — the client never reads it
            (778, display([90, 8, 0])), // per-level row in slot 0
            (779, display([10, 0, 0])), // row 10 = 30.0 — the 20.0 clamp
        ]));
        spells.radii = benilla_formats::SpellRadiusCatalog::from_rows(HashMap::from([
            (
                14,
                SpellRadius {
                    radius: 8.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                8,
                SpellRadius {
                    radius: 5.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                13,
                SpellRadius {
                    radius: 10.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                10,
                SpellRadius {
                    radius: 30.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                90,
                SpellRadius {
                    radius: 2.0,
                    per_level: 0.1,
                    max: 0.0,
                },
            ),
        ]));
        let s = Some(&spells);
        assert_eq!(ground_cast_radius(s, 10, 60), 8.0);
        assert_eq!(ground_cast_radius(s, 2120, 60), 5.0);
        // Slot 2 is invisible to the reticle — no rows in 0/1 reads 0 (→ the default size).
        assert_eq!(ground_cast_radius(s, 777, 60), 0.0);
        // Per-level: 2.0 + 60 × 0.1 = 8.0 beats slot 1's 5.0.
        assert_eq!(ground_cast_radius(s, 778, 60), 8.0);
        // The 20.0 clamp (`[0x804478]`).
        assert_eq!(ground_cast_radius(s, 779, 60), 20.0);
        // Unknown spell / no data at all → 0 (default size).
        assert_eq!(ground_cast_radius(s, 9999, 60), 0.0);
        assert_eq!(ground_cast_radius(None, 10, 60), 0.0);
    }
}
