//! MCLQ liquid surfaces → per-chunk water meshes (Phase E; Phase 1 = inland water).
//!
//! Vanilla liquid is **MCLQ** — per-MCNK, a 9×9 absolute-height grid + an 8×8 cell-flag grid
//! (no WotLK MH2O). `wow-adt`'s [`MclqChunk`] decodes it; we validated its output **byte-identical**
//! to a hand-decode of the raw 804-B payload against `World\Maps\Azeroth\Azeroth_32_48.adt`
//! (wow-adt 0.6.4, 2026-05-31): per-vertex `height`, raw `tile_flags`, `min/max_height`, and the
//! MCNK-flag-derived `liquid_type` all match (the on-disk `QLCM` magic sits at the offset; the
//! payload — and the crate's parse — start 8 bytes in).
//!
//! Output is in **raw WoW coords** (+X north, +Y west, +Z up, yards), like [`crate::terrain`]; the
//! renderer applies the WoW→Bevy transform and ports the `ocean0_s.bls` shader. We emit a flat
//! surface (normal `(0,0,1)`) of 2 triangles per **wet** cell; dry cells (flag low-nibble `0xf`,
//! whose verts carry the FLT_MAX sentinel height) are skipped. Magma/slime is deferred to Phase 3.

use benilla_adt::{LiquidType, MclqChunk};

use crate::terrain::{snap_to_lattice, UNIT_SIZE};

/// Which animated texture set + render path a liquid surface uses (selects the frame set in
/// `benilla`). The MCLQ (ADT) path emits only Still/Rapids/Ocean; the WMO MLIQ path additionally
/// emits Magma/Slime (canals, fountains, the Ironforge lava, dungeon pools).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiquidKind {
    /// Still water — `XTextures\river\lake_a.*` (30 frames). Lakes, ponds, slow rivers, WMO canals.
    Still,
    /// Rapids — `XTextures\river\fast_a.*` (16 frames). Fast streams (per-cell nibble `8`).
    Rapids,
    /// Ocean — `XTextures\ocean\ocean_h.*` (30 frames). Coastal / sea tiles.
    Ocean,
    /// Magma / lava — `XTextures\lava\lava.*`. Opaque, fullbright (the animated texture IS the body,
    /// no depth LUT; VERIFIED wow-re `rf-water-liquid-type-texture-material.md`, magma vert-fill
    /// `0x68d890` = constant 1.0). WMO liquid only (nibbles `2`/`6`) — the Ironforge Great Forge.
    Magma,
    /// Slime — `XTextures\slime\slime.*`. Its own texture on the magma render category (opaque,
    /// fullbright). WMO liquid only (nibbles `3`/`7`) — Undercity, the Sludge Fields.
    Slime,
}

impl LiquidKind {
    /// The liquid type from a per-tile MLIQ flag low nibble (`flag & 0xf`) — the VERIFIED reference
    /// selection (wow-re `rf-water-liquid-type-texture-material.md`, name table `0x86a000`): the
    /// nibble indexes the animated-texture table directly. `0xf` (and any unmapped value) = hole /
    /// no liquid → `None`. `4` is a same-class `lake_a` variant of `0`; `6`/`7` of `2`/`3`.
    pub fn from_nibble(nibble: u8) -> Option<LiquidKind> {
        match nibble & 0xf {
            0 | 4 => Some(LiquidKind::Still),
            1 => Some(LiquidKind::Ocean),
            2 | 6 => Some(LiquidKind::Magma),
            3 | 7 => Some(LiquidKind::Slime),
            8 => Some(LiquidKind::Rapids),
            _ => None, // 5, 9..=0xf — NULL name-table slots / the 0xf hole sentinel
        }
    }

    /// Whether this kind renders as an **opaque, fullbright** surface (the animated texture is the
    /// body colour, no water swatch / depth ramp / N·L darkening) — magma and slime. Water/ocean use
    /// the depth-swatch `ocean0_s.bls` path instead.
    pub fn is_fullbright(self) -> bool {
        matches!(self, LiquidKind::Magma | LiquidKind::Slime)
    }
}

/// One MCNK's liquid surface as an indexed triangle mesh in **raw WoW coords** (+Z up, flat).
///
/// `positions`/`uvs` hold the full 9×9 grid (81 verts); verts under dry cells stay present but
/// unreferenced. `indices` is 2 triangles per wet cell. Drawn two-sided (cull off), so winding
/// is not load-bearing.
#[derive(Debug, Clone)]
pub struct LiquidMesh {
    /// Vertex positions `[x, y, z]` in WoW yards — 81 entries (9×9 grid, row-major).
    pub positions: Vec<[f32; 3]>,
    /// Cell-index UVs (`¼` per cell, tiling) parallel to `positions`. No scroll for water.
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex liquid swatch coord **V** `0..1` from the MCLQ `SVert` depth byte, parallel to
    /// `positions`. River/lake = `clamp(byte/42)` (VERIFIED `c81768`, saturates ~5 yd); ocean = byte/255
    /// (placeholder — ocean uses a different non-LUT path, see `build_liquid_mesh`). The reference uses
    /// this single V to index the depth swatch for BOTH the body-colour band (shallow→deep water rows)
    /// and the opacity ramp (`colorTex.a` = `127+2·row`, deeper = more opaque). Magma/slime reuse
    /// the union bytes for UVs (N/A here).
    pub depths: Vec<f32>,
    /// Triangle indices into `positions` — 6 per wet cell.
    pub indices: Vec<u32>,
    /// The surface's **sound-class nibble** — the majority wet cell's low nibble (terrain), or
    /// the `0x6ba970`-resolved nibble (WMO): `class = nibble & 3`, `FluidSpeed = nibble & 0xc`,
    /// the key the above-water liquid ambient-loop system resolves through `SoundWaterType.dbc`
    /// ([`crate::WaterSoundCatalog`]; wow-re `liquid-ambience-loop.md`, decision 0506). Carried
    /// beside `kind` because the render kind collapses the river speeds (nibbles 0 and 4 both
    /// draw `lake_a`) that the sound table splits (RiverStill 1111 vs RiverSlow 1112).
    pub sound_nibble: u8,
    /// The texture set / render path this surface uses.
    pub kind: LiquidKind,
}

/// 9×9 vertex grid per MCNK.
const GRID: usize = 9;
/// 8×8 cell grid per MCNK.
const CELLS: usize = 8;
/// Cell-flag low nibble meaning "dry / do not render".
const DRY_NIBBLE: u8 = 0x0f;
/// Cell-flag low nibble meaning "rapids" (`river\fast_a`) rather than still water.
const RAPIDS_NIBBLE: u8 = 0x08;
/// Verts under no liquid carry FLT_MAX (`0x7F7FFFFF`) — `is_finite()` is TRUE for it, so gate on
/// magnitude instead.
const HEIGHT_SENTINEL: f32 = 1.0e9;
/// River/lake depth-byte at which the swatch coord `V` saturates to 1.0: `V = clamp(byte/42)`.
/// VERIFIED from `WoW.exe` — the in-game river/lake water list (`FUN_0068d790`) reads the **steep**
/// `DAT_00c81768` LUT, built by `FUN_0068c4c0` as `clamp((d/9)/4.6667) = clamp(d/42)` (constants
/// `0x81028c=1/9`, `0x810380=1/4.6667` static-verified), and the live VBO confirms `V` is multiples
/// of 1/42 saturating at 1.0. (The gentler `byte/255` = the `DAT_00c7fcd8` LUT on a DIFFERENT list
/// `FUN_0068d690` that the from-above river path does not use — hence the river middle never reached
/// the deep/teal swatch row until this. The river depth ramp is ≈8.5 byte/yd, so V saturates at ≈5 yd.)
const RIVER_DEPTH_V_SATURATION: f32 = 42.0;

/// Build a flat water surface mesh from a parsed MCLQ chunk at MCNK origin `position`
/// (the chunk header's raw WoW `[x, y, z]`). Returns `None` when the chunk has no wet cells
/// or is magma/slime (Phase 3).
pub(crate) fn build_liquid_mesh(mclq: &MclqChunk, position: [f32; 3]) -> Option<LiquidMesh> {
    // Coarse kind from the MCNK-flag liquid type; magma/slime deferred.
    let base_kind = match mclq.liquid_type {
        LiquidType::Water => LiquidKind::Still,
        LiquidType::Ocean => LiquidKind::Ocean,
        LiquidType::Magma | LiquidType::Slime => return None,
    };
    // Depth-byte → swatch coord `V` divisor (VERIFIED). River/lake (the `c81768`/`FUN_0068d790` path)
    // saturates at byte 42 = ~5 yd, so the channel middle reaches the deep/teal swatch row. Ocean uses
    // a DIFFERENT mechanism (`FUN_0068d890` reads explicit per-vertex MCLQ UVs, not a depth LUT) — left
    // at /255 here pending its own RE + an in-game A/B; an ocean A/B has not been done since this fix.
    let depth_v_div = match base_kind {
        LiquidKind::Ocean => 255.0,
        _ => RIVER_DEPTH_V_SATURATION,
    };
    if mclq.vertices.len() < GRID * GRID || mclq.tile_flags.len() < CELLS * CELLS {
        return None;
    }
    let [wx, wy, _wz] = position;

    // 81 grid verts in raw WoW coords. Linear index `n`: row = n/9 (steps south, −X), col = n%9
    // (steps east, −Y) — the same orientation as the MCVT outer grid, so the water lattice lines
    // up with the terrain it sits in. Z is the MCLQ per-vertex absolute world height. Snap X/Y to
    // the global lattice exactly as terrain does.
    let mut positions = Vec::with_capacity(GRID * GRID);
    let mut uvs = Vec::with_capacity(GRID * GRID);
    let mut depths = Vec::with_capacity(GRID * GRID);
    for n in 0..GRID * GRID {
        let row = (n / GRID) as f32;
        let col = (n % GRID) as f32;
        // Dry/no-liquid verts carry the FLT_MAX (0x7F7FFFFF) sentinel (and `is_finite()` is TRUE for
        // it). They're never indexed (only wet cells emit tris), but they stay in `positions`, so a
        // raw sentinel would blow the mesh AABB out to ~1e38 — which makes Bevy's visibility culling
        // drop the whole chunk (the "partial water chunks vanish, fully-wet ocean is fine" bug, since
        // only mixed-wet/dry chunks contain sentinels). Substitute the chunk's `min_height` for any
        // non-finite/sentinel height so the AABB stays tight; the value is invisible (unreferenced).
        let raw = mclq.vertices[n].height;
        let h = if raw.is_finite() && raw.abs() < HEIGHT_SENTINEL {
            raw
        } else {
            mclq.min_height
        };
        positions.push([
            snap_to_lattice(wx - row * UNIT_SIZE),
            snap_to_lattice(wy - col * UNIT_SIZE),
            h,
        ]);
        uvs.push([col * 0.25, row * 0.25]);
        // SVert depth byte (water/ocean: union_data[0]) → swatch coord V (0..1). River/lake saturate at
        // byte 42 (`clamp(byte/42)`, VERIFIED `c81768`) so the channel middle hits the deep/teal swatch
        // row; ocean at /255 (see `depth_v_div`). The SAME V drives both the body-colour depth band and
        // the opacity ramp on the shader side (one swatch row → colour + alpha).
        depths.push((mclq.vertices[n].depth_byte() as f32 / depth_v_div).clamp(0.0, 1.0));
    }

    // 2 tris per wet cell. Skip dry cells (low nibble 0xf) and any cell with a sentinel-height
    // corner (belt-and-suspenders). Track whether any wet cell is rapids → use fast_a for the
    // whole chunk (still/rapids virtually never mix in one MCNK; per-cell sets are a refinement).
    let mut indices = Vec::with_capacity(CELLS * CELLS * 6);
    let mut any_rapids = false;
    // Wet-cell nibble tally → the surface's majority sound-class nibble (`LiquidMesh::sound_nibble`).
    let mut nibble_counts = [0u32; 16];
    for row in 0..CELLS {
        for col in 0..CELLS {
            let flag = mclq.tile_flags[row * CELLS + col] & 0x0f;
            if flag == DRY_NIBBLE {
                continue;
            }
            nibble_counts[flag as usize] += 1;
            let tl = (row * GRID + col) as u32;
            let tr = tl + 1;
            let bl = ((row + 1) * GRID + col) as u32;
            let br = bl + 1;
            // Guard against a wet cell whose corner is the sentinel/NaN (data anomaly) — check the
            // ORIGINAL heights, not the sanitized `positions`. Wet cells normally carry real heights.
            if [tl, tr, bl, br].iter().any(|&i| {
                let raw = mclq.vertices[i as usize].height;
                !raw.is_finite() || raw.abs() >= HEIGHT_SENTINEL
            }) {
                continue;
            }
            if flag == RAPIDS_NIBBLE {
                any_rapids = true;
            }
            indices.extend_from_slice(&[tl, bl, br, tl, br, tr]);
        }
    }

    if indices.is_empty() {
        return None;
    }
    let kind = if any_rapids && base_kind == LiquidKind::Still {
        LiquidKind::Rapids
    } else {
        base_kind
    };
    let sound_nibble = nibble_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, &c)| c)
        .map(|(n, _)| n as u8)
        .unwrap_or(0);
    Some(LiquidMesh {
        positions,
        uvs,
        depths,
        indices,
        sound_nibble,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use benilla_adt::{parse_adt, ParsedAdt};

    use super::*;

    fn data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// Golden test against a real Elwynn tile: Crystal Lake area `Azeroth_32_48.adt` has 42 MCLQ
    /// water chunks (all river/still, surface ≈ 143.99 yd). Skips when the client isn't present.
    #[test]
    fn parses_elwynn_lake_tile() {
        let data = data_dir();
        if !data.is_dir() {
            eprintln!("skipping: no WoW client at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("World\\Maps\\Azeroth\\Azeroth_32_48.adt")
            .expect("read Azeroth_32_48.adt");
        let ParsedAdt::Root(root) =
            parse_adt(&mut std::io::Cursor::new(&bytes[..])).expect("parse adt");

        let mut meshes = Vec::new();
        for mcnk in &root.mcnk_chunks {
            if let Some(mclq) = &mcnk.liquid {
                if let Some(m) = build_liquid_mesh(mclq, mcnk.header.position) {
                    meshes.push(m);
                }
            }
        }

        // 42 water chunks on this tile, every one still river water.
        assert_eq!(
            meshes.len(),
            42,
            "expected 42 liquid chunks on Azeroth_32_48"
        );
        assert!(
            meshes.iter().all(|m| m.kind == LiquidKind::Still),
            "Crystal Lake is all still water"
        );

        for m in &meshes {
            assert_eq!(m.positions.len(), 81, "9×9 grid");
            assert_eq!(m.uvs.len(), 81, "uv per vertex");
            assert_eq!(m.depths.len(), 81, "depth per vertex");
            // EVERY vertex (even unreferenced dry-cell verts) must be finite + sane, or the mesh AABB
            // blows up (dry verts carry the FLT_MAX sentinel) and Bevy frustum-culls the whole chunk —
            // the "partial water chunks vanish, fully-wet ocean is fine" bug.
            for p in &m.positions {
                assert!(
                    p[2].is_finite() && p[2].abs() < 10_000.0,
                    "all liquid verts sane for a clean AABB, got z={}",
                    p[2]
                );
            }
            assert!(!m.indices.is_empty(), "at least one wet cell");
            assert_eq!(m.indices.len() % 6, 0, "2 tris per wet cell");
            // Indices in range, and every referenced vertex has a finite, sane height — no
            // FLT_MAX sentinel leaked in (the dry-cell gate worked). Elwynn water bodies sit at
            // various elevations (Crystal Lake ≈ 144 yd, streams lower), all in a plausible band.
            assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
            for &i in &m.indices {
                let z = m.positions[i as usize][2];
                assert!(
                    z.is_finite() && z.abs() < 10_000.0,
                    "referenced height {z} sane"
                );
                assert!(
                    (50.0..300.0).contains(&z),
                    "Elwynn water elevation {z} plausible"
                );
            }
            // Within one chunk a water surface is near-planar (a lake; a gentle stream slopes a
            // little across the 33-yd chunk but not wildly).
            let zs: Vec<f32> = m
                .indices
                .iter()
                .map(|&i| m.positions[i as usize][2])
                .collect();
            let (zmin, zmax) = zs
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), &z| (lo.min(z), hi.max(z)));
            assert!(
                zmax - zmin < 30.0,
                "per-chunk surface roughly flat ({zmin}..{zmax})"
            );
        }
    }
}
