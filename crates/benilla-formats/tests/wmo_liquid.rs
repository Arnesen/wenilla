//! WMO MLIQ liquid-surface build — byte check against Stormwind's canals (the reference building
//! for WMO-embedded water; its city is one 306-group WMO, 22 of whose groups carry `MLIQ`). Pins
//! the SMOLiquidHeader decode, the `xtiles = xverts − 1` grid, the per-tile hole nibble (`0xf`), and
//! the lake_a (nibble 4) type resolution. Skips when the client isn't present.

use benilla_formats::{wmo_group_liquid_mesh, Chain, LiquidKind};

#[test]
fn stormwind_canal_group_builds_still_water() {
    let data = benilla_formats::wow_data_or_skip!();
    let reader = Chain::open(&data).expect("open vanilla patch chain");

    // Group 099 is a canal segment: MLIQ header xverts=12 yverts=9 (12×9 = 108 verts), 11×8 = 88
    // tiles of which 52 are wet (nibble 4 = lake_a) and 36 are holes (nibble 0xf).
    let g099 = reader
        .read("World\\wmo\\Azeroth\\Buildings\\Stormwind\\Stormwind_099.wmo")
        .expect("read Stormwind_099.wmo");
    let mesh = wmo_group_liquid_mesh(&g099).expect("group 099 carries water");
    assert_eq!(mesh.kind, LiquidKind::Still, "canal water is lake_a/still");
    assert_eq!(mesh.positions.len(), 12 * 9, "full 12×9 grid emitted");
    assert_eq!(mesh.uvs.len(), 12 * 9);
    assert_eq!(mesh.depths.len(), 12 * 9);
    assert_eq!(
        mesh.indices.len(),
        52 * 6,
        "52 wet tiles → 2 tris each; the 36 hole tiles are skipped"
    );
    // Every index is in range and every vertex height is a sane, finite value (flat canal water).
    assert!(mesh
        .indices
        .iter()
        .all(|&i| (i as usize) < mesh.positions.len()));
    for p in &mesh.positions {
        assert!(
            p[2].is_finite() && p[2].abs() < 10_000.0,
            "sane height {p:?}"
        );
    }

    // Group 000 is dry masonry — no MLIQ, so no liquid mesh.
    let g000 = reader
        .read("World\\wmo\\Azeroth\\Buildings\\Stormwind\\Stormwind_000.wmo")
        .expect("read Stormwind_000.wmo");
    assert!(
        wmo_group_liquid_mesh(&g000).is_none(),
        "a masonry group carries no liquid"
    );
}
