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

/// A hole corner's authored height is not a height, and it must not reach the mesh AABB.
///
/// MLIQ carries a full `xverts × yverts` height array whether or not a tile is wet, and the shipped
/// files leave the hole interiors at a literal `0.0`. Emitted verbatim those are invisible in the
/// draw — no triangle references them — but they stretch the Bevy mesh AABB from the flat sheet all
/// the way to model z 0. Measured on the two files below: Blackfathom's Pool of Ask'ar had 186 of
/// 868 such vertices and an AABB spanning 58.28 yd around a sheet with **zero** vertical extent;
/// Stormwind's canal g099 had 36 of 108 and spanned 6.48 yd. The builder now fills every
/// undrawn vertex with the lowest drawn height, so both collapse to flat.
#[test]
fn hole_corner_heights_never_reach_the_mesh_bounds() {
    let data = benilla_formats::wow_data_or_skip!();
    let reader = Chain::open(&data).expect("open vanilla patch chain");

    for (path, surface_z) in [
        (
            "World\\wmo\\dungeon\\kl_blackfathom\\blackfathom_instance_007.wmo",
            -58.283_04_f32,
        ),
        (
            "World\\wmo\\Azeroth\\Buildings\\Stormwind\\Stormwind_099.wmo",
            -6.482_947_f32,
        ),
    ] {
        let group = reader
            .read(path)
            .unwrap_or_else(|e| panic!("read {path}: {e}"));
        let mesh = wmo_group_liquid_mesh(&group).unwrap_or_else(|| panic!("{path} carries water"));

        // Both pools are dead flat, so EVERY emitted vertex — drawn or not — must sit on the sheet.
        let (lo, hi) = mesh
            .positions
            .iter()
            .map(|p| p[2])
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), z| {
                (lo.min(z), hi.max(z))
            });
        assert!(
            (lo - surface_z).abs() < 1e-3 && (hi - surface_z).abs() < 1e-3,
            "{path}: mesh z spans [{lo}..{hi}], expected the flat sheet at {surface_z}"
        );

        // …and the drawn geometry is unchanged by the substitution: the sheet is still the sheet.
        for &i in &mesh.indices {
            let z = mesh.positions[i as usize][2];
            assert!(
                (z - surface_z).abs() < 1e-3,
                "{path}: drawn vertex {i} at z {z}, expected {surface_z}"
            );
        }
    }
}

/// The two reported sites take **different** water arms, and each carries the inputs its arm reads.
///
/// `0x6b62e0`'s category 0 splits on the owning group's `MOGP.flags & 0x48`, and the two halves are
/// genuinely different renderers — the exterior one binds `MapObjExtWater0.bls` and lights a vertex
/// normal, the interior one is fixed-function, unlit, and takes its whole body colour from
/// `MOMT[materialId].diffColor`. B136 (Blackfathom) and the director's "Stormwind water seems too
/// rough" land on opposite arms, which is the whole reason one shader could not be right for both.
///
/// Pins the two inputs a wrong offset would silently corrupt: the pool's `materialId` (so the body
/// colour is looked up in the right MOMT slot) and the per-vertex opacity byte (which we read as
/// nothing at all until 2026-08-20, rendering every WMO pool at a pinned, fully-opaque 1.0).
#[test]
fn the_two_water_arms_carry_their_own_inputs() {
    let data = benilla_formats::wow_data_or_skip!();
    let reader = Chain::open(&data).expect("open vanilla patch chain");

    // (group file, is the group interior?, the pool's MLIQ materialId)
    for (path, interior, material_id) in [
        (
            "World\\wmo\\dungeon\\kl_blackfathom\\blackfathom_instance_007.wmo",
            true,
            27_u16,
        ),
        (
            "World\\wmo\\Azeroth\\Buildings\\Stormwind\\Stormwind_099.wmo",
            false,
            115_u16,
        ),
    ] {
        let bytes = reader
            .read(path)
            .unwrap_or_else(|e| panic!("read {path}: {e}"));
        let header = benilla_formats::wmo_group_header(&bytes)
            .unwrap_or_else(|| panic!("{path} parses a MOGP header"));
        assert_eq!(
            header.flags & 0x48 == 0,
            interior,
            "{path}: MOGP flags {:#010x} put it on the wrong water arm",
            header.flags
        );

        let mesh = wmo_group_liquid_mesh(&bytes).unwrap_or_else(|| panic!("{path} carries water"));
        assert_eq!(
            mesh.material_id,
            Some(material_id),
            "{path}: the pool must name its own MOMT slot — an interior arm reads its body colour \
             from exactly this index"
        );

        // The opacity channel is per-vertex and really varies: a constant here would mean we were
        // reading a byte nothing authors (or the wrong byte). Blackfathom's pool spans 202 distinct
        // values; Stormwind's canals are 91% a single one, so only the range is asserted in common.
        let (lo, hi) = mesh
            .depths
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(
            (0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi),
            "{path}: opacity V out of range [{lo}..{hi}]"
        );
        assert!(
            hi > 0.0,
            "{path}: every vertex opacity is zero — the byte is not being read"
        );
    }
}

/// One texture repeat per grid cell — the WMO water UV scale, and the fix for "too rough".
///
/// The reference's `0x6b6630` writes `u = (float)i, v = (float)j`, raw tile indices from loop
/// counters that start at a literal 0, so one repeat spans one 4.167 yd cell. We had a quarter of
/// that: a texture 4x too large, which is exactly two mip levels, so the near field never reached
/// the authored chain that flattens the ripple with distance.
#[test]
fn wmo_water_repeats_once_per_grid_cell() {
    let data = benilla_formats::wow_data_or_skip!();
    let reader = Chain::open(&data).expect("open vanilla patch chain");
    let g099 = reader
        .read("World\\wmo\\Azeroth\\Buildings\\Stormwind\\Stormwind_099.wmo")
        .expect("read Stormwind_099.wmo");
    let mesh = wmo_group_liquid_mesh(&g099).expect("group 099 carries water");
    // Adjacent columns of the 12-wide grid are one cell apart, so exactly one repeat apart.
    let du = mesh.uvs[1][0] - mesh.uvs[0][0];
    assert!(
        (du - 1.0).abs() < 1e-4,
        "one repeat per cell: adjacent vertices differ by {du} in u, expected 1.0"
    );
    let dv = mesh.uvs[12][1] - mesh.uvs[0][1];
    assert!(
        (dv - 1.0).abs() < 1e-4,
        "one repeat per cell: adjacent rows differ by {dv} in v, expected 1.0"
    );
}
