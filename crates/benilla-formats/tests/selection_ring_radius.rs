//! Pins the selection-ring radius against the **pixel-measured reference** on real build-5875 assets.
//!
//! The reference client sizes a **living unit's** ground selection ring (draw `0x608e00`, sizer
//! `0x60aee0`) as:
//!
//! ```text
//! ring_radius = OBJECT_FIELD_SCALE_X × sqrt( 0.5 · sqrt(dx² + dy²) )
//! ```
//!
//! where `dx,dy` are the horizontal (X,Y) extents of the unit's **Stand** animation bounding box (the M2
//! `M2Sequence` CAaBox), Stand being animation **id 0** via `animationLookup[0]`. This was byte-traced
//! *and* Unicorn-emulated in wow-re (`system/object-layer/scratch/selection-ring-scale.md`) and
//! reproduces the reference apitrace's measured ring radii to ~1 mm. It is NOT the render bounding sphere
//! (`0xCC`) — that (`0x5d6fe0`, `[unit+0x2b0]`) is the *corpse* decal; the render sphere over-sized tall
//! humans and under-sized the squat chicken because it folds in height, whereas the nested-sqrt footprint
//! compresses range.
//!
//! [`M2Bounds::ring_footprint`] is the model-local (pre-scale) part; the world radius is it ×
//! `OBJECT_FIELD_SCALE_X`. The four creatures below were the ones captured in the reference trace, drawn
//! life-size (scale ≈ 1.0), so `ring_footprint` ≈ the measured world radius. Skips when the gitignored
//! client data isn't present.

use benilla_formats::{load_m2_bounds, open_chain};

#[test]
fn ring_footprint_matches_reference_pixels() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open chain");

    // (model, ring radius measured from the reference apitrace at scale 1.0). The Stand-box footprint
    // must reproduce these; a header-render-box or render-sphere source does not.
    let cases = [
        ("Creature\\Chicken\\Chicken.mdx", 0.572_f32),
        ("Character\\Human\\Female\\HumanFemale.mdx", 0.731),
        ("Character\\Human\\Male\\HumanMale.mdx", 0.841),
        ("Creature\\Horse\\Horse.mdx", 1.295),
    ];

    for (path, measured) in cases {
        let b = load_m2_bounds(&mut chain, path).unwrap_or_else(|e| panic!("bounds {path}: {e}"));
        assert!(
            (b.ring_footprint - measured).abs() < 0.01,
            "{path}: ring_footprint {:.4} should reproduce the reference-measured {measured} (Stand-box \
             sqrt(0.5·sqrt(dx²+dy²)))",
            b.ring_footprint
        );
        // Guard against a regression to the (wrong) corpse-path render-sphere source: for all four it is
        // far off (0.5×sphere = 0.33 / 1.19 / 1.39 / 1.96), so it can't sneak past the bound above.
        assert!(
            (b.ring_footprint - 0.5 * b.sphere_radius).abs() > 0.05,
            "{path}: ring_footprint must not coincide with the corpse-path 0.5×renderSphere"
        );
    }
}
