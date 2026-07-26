//! WMO **group**-file parsing: the MOGP header + sub-chunks (doodad/light refs, footprint faces)
//! and the render-batch → [`RenderSubmesh`] build against a parsed root.

use std::io::Cursor;

use anyhow::Result;
use benilla_wmo::{parse_wmo, ParsedWmo, WmoLiquid};

use super::{find_wmo_chunk, WmoRoot};
use crate::liquid::{LiquidKind, LiquidMesh};
use crate::models::{remap_submesh, ModelBlend, RenderSubmesh, WmoBatchClass};

/// The MLIQ grid vertex spacing (world yards between adjacent grid vertices) — the reference's
/// `[0x810cc0]` = `4.166666507720947` (`0x40855555`), VERIFIED wow-re. The liquid render dispatch
/// `0x6b62e0` branches on the type nibble: **water** → `0x6b6420`/`0x6b6630`, magma/slime → `0x6b68f0`
/// (wow-re `rf-mliq-liquid-uv-texgen.md`; the earlier `0x6b68f0` "water kernel" label was the magma
/// path). Either kernel places grid vertex `(i, j)` at `base + (i·STEP, j·STEP)`.
const MLIQ_CELL_STEP: f32 = f32::from_bits(0x4085_5555);

/// Yards per water-texture repeat: the per-cell UV step is ¼, so one repeat spans `4·MLIQ_CELL_STEP`
/// ≈ 16.67 yd. We anchor each vertex's UV to its **model-space position** over this period so the
/// texture is one continuous field across all of a WMO's MLIQ surfaces — no per-surface seam.
///
/// VERIFIED mechanism (wow-re `rf-mliq-liquid-uv-texgen.md`): the reference's per-vertex `st` DOES
/// reset per surface (raw `(i, j)`) — seamlessness comes from a shared **world-anchored** `GL_TEXTURE`
/// matrix that translates by the liquid batch's common bbox-min origin. Anchoring off a shared origin
/// is the fix; model space is our equivalent realization (identical within a single WMO placement,
/// which Stormwind is). DEFERRED to the exact form (a world-space texture matrix): the matrix's scale
/// — the true yards-per-repeat — is still INFERRED upstream (`[owner+0x94]`, unlocated), so `4·STEP`
/// stands as the size that preserves the prior ripple; fold the verified scale in when it lands.
const MLIQ_UV_PERIOD: f32 = 4.0 * MLIQ_CELL_STEP;

/// The swatch-coord `V` fed to every WMO **water/ocean** vertex. WMO liquid verts carry flow bytes,
/// not a depth byte (unlike MCLQ), so there is no per-vertex bathymetry to ramp; the reference's WMO
/// water alpha comes from a separate shore LUT (`0xc7fbc0`, wow-re — not yet RE'd for this path). We
/// render the flat surface at the DEEP endpoint: opaque, deep-tinted canal water (the vanilla look),
/// which the zone's `Light.dbc` rows still colour per-zone. Tunable; the exact shore LUT is deferred.
const WMO_WATER_DEPTH_V: f32 = 1.0;

/// The per-group fields the visibility pass reads from a **group file**'s MOGP header: the group
/// `flags` (offset `0x08`) and the group's slice into [`super::WmoPortals::refs`]
/// (`portal_ref_start` @ `0x24`, `portal_ref_count` @ `0x26`). VERIFIED layout (`grp+0x10` flags,
/// `+0x2c`/`+0x30` ref span seeded from MOGP `+0x24`/`+0x26`). `None` if the bytes aren't a
/// parseable group.
#[derive(Debug, Clone, Copy)]
pub struct WmoGroupHeader {
    /// MOGP group flags. `& 0x8` = EXTERIOR (the outdoor bit the flood defers on); `& 0x48 == 0` =
    /// a true interior group (see [`super::WmoGroupInfo`]).
    pub flags: u32,
    /// First index into [`super::WmoPortals::refs`] for this group's portals.
    pub portal_ref_start: u16,
    /// Number of portal refs for this group.
    pub portal_ref_count: u16,
    /// MOGP `uniqueID` @ `0x38` — this group's `WMOAreaTable.WMOGroupID` key (byte-verified
    /// 2026-07-03: Chapel_000 interior → 478, matching its WMOAreaTable CAVE/ambience row).
    /// `0` when the header is too short to carry it.
    pub area_table_id: u32,
    /// MOGP fog indices @ `0x30` — four indices into the root's [`super::WmoFog`] table, walked by
    /// the client's interior-fog selector (`0x69de20`, wow-re `rf-weather-emission-timeline`
    /// ROUND 5). Offset pinned empirically against the Goldshire inn (its groups carry `[1,0,0,0]` /
    /// `[0,0,0,0]` here — valid indices into its 2-record MFOG — while `uniqueID @0x38` and the
    /// no-liquid sentinel `@0x34` self-validate the layout; see `tests/wmo_fogs.rs`).
    /// `[0; 4]` when the header is too short to carry them.
    pub fog_indices: [u8; 4],
}

/// Read one WMO **group** file's MOGP header fields used by the visibility pass (see
/// [`WmoGroupHeader`]). The MOGP super-chunk's payload begins with the 68-byte group header.
pub fn wmo_group_header(group_bytes: &[u8]) -> Option<WmoGroupHeader> {
    let mogp = find_wmo_chunk(group_bytes, b"PGOM")?; // MOGP
    if mogp.len() < 0x28 {
        return None;
    }
    let u32_at = |i: usize| {
        (mogp.len() >= i + 4)
            .then(|| u32::from_le_bytes([mogp[i], mogp[i + 1], mogp[i + 2], mogp[i + 3]]))
    };
    Some(WmoGroupHeader {
        flags: u32::from_le_bytes([mogp[8], mogp[9], mogp[10], mogp[11]]),
        portal_ref_start: u16::from_le_bytes([mogp[0x24], mogp[0x25]]),
        portal_ref_count: u16::from_le_bytes([mogp[0x26], mogp[0x27]]),
        area_table_id: u32_at(0x38).unwrap_or(0),
        fog_indices: if mogp.len() >= 0x34 {
            [mogp[0x30], mogp[0x31], mogp[0x32], mogp[0x33]]
        } else {
            [0; 4]
        },
    })
}

/// One interior WMO group's raw render triangles + per-vertex baked MOCV + per-triangle MOPY flags —
/// the face set the interior FOOTPRINT sample walks. Its live consumer is the **GameObject M2**:
/// the entity light node's env-update attach (`0x6717d0` → `0x69e4c0`, the entity twin of the
/// ADT-MDDF `0x6a8410`) down-rays the group RENDER mesh (`MOVT`/`MOVI`/`MOPY`) under the object and
/// bakes the hit's barycentric MOCV — floor-168/cap-96 — as its committed light (wow-re
/// `wmo-lit-selector.md`; trace-decisive on the abbey INNBENCH draws, diffuse max exactly 168).
/// `None` for a non-group, an exterior group, or a group without baked colours (never a footprint
/// source). (First built for decision 0290's MODD reading, deleted by 0306 when MODD props turned
/// out to use their own baked colour — resurrected now that the chain's true consumer is pinned.)
#[derive(Clone)]
pub struct FootprintTris {
    /// Vertex positions, WMO model space (WoW axes).
    pub positions: Vec<[f32; 3]>,
    /// Triangle list into [`Self::positions`] (MOVI order — MOPY is parallel per triangle).
    pub indices: Vec<u16>,
    /// Per-vertex baked MOCV as RGB bytes, parallel to positions (bytes, not 0–1: the floor/cap
    /// arithmetic is exact u8 fixed-point).
    pub mocv: Vec<[u8; 3]>,
    /// Per-TRIANGLE MOPY flags (`indices.len() / 3` entries). Bit `0x1` on the hit face selects the
    /// day/night exterior colours as the object's base instead of the MOCV sample (§11's binary
    /// selector).
    pub mopy_flags: Vec<u8>,
}

/// The footprint ray's per-face MOPY reject mask (wow-re `unit-light-combine-storm.md`, the BSP
/// face walk `0x6b92b0`/`0x6bc370`): a face with COLLISION `0x08` or VISITED `0x80` set is never a
/// footprint candidate. This is what keeps a floor authored as coplanar layers (a RENDER|DETAIL
/// sheet over a COLLISION sheet, observed on the Goldshire forge + inn) from being a per-position
/// coin toss: only the render layer survives, so the sample is deterministic and regional.
const FOOTPRINT_REJECT: u8 = 0x88;

/// Parse one WMO **group**'s bytes into its footprint-sample face set ([`FootprintTris`]).
/// `None` unless the bytes are a group, the group is INTERIOR (`flags & 0x48 == 0`), and it carries
/// MOCV parallel to its positions. Faces matching [`FOOTPRINT_REJECT`] are dropped here (the
/// client rejects them per-ray inside the BSP walk; the set is static, so filtering at build is
/// equivalent and free).
pub fn wmo_group_footprint_tris(group_bytes: &[u8]) -> Option<FootprintTris> {
    let Ok(ParsedWmo::Group(group)) = parse_wmo(&mut Cursor::new(group_bytes)) else {
        return None;
    };
    if group.flags & 0x48 != 0 || group.vertex_colors.len() != group.vertex_positions.len() {
        return None;
    }
    let mut indices = Vec::new();
    let mut mopy_flags = Vec::new();
    for (ti, tri) in group.vertex_indices.chunks_exact(3).enumerate() {
        let flags = group.material_info.get(ti).map_or(0, |m| m.flags);
        if flags & FOOTPRINT_REJECT != 0 {
            continue;
        }
        indices.extend_from_slice(tri);
        mopy_flags.push(flags);
    }
    Some(FootprintTris {
        positions: group
            .vertex_positions
            .iter()
            .map(|v| [v.x, v.y, v.z])
            .collect(),
        indices,
        mocv: group
            .vertex_colors
            .iter()
            .map(|c| [c.r, c.g, c.b])
            .collect(),
        mopy_flags,
    })
}

/// Find a sub-chunk of a group file's **MOGP** super-chunk by on-disk (reversed) magic. The MOGP
/// payload is the 0x44-byte group header followed by ordinary `[magic][size][data]` sub-chunks
/// (MOPY/MOVT/…/MODR/MOLR); this walks them.
fn find_mogp_subchunk<'a>(group_bytes: &'a [u8], magic: &[u8; 4]) -> Option<&'a [u8]> {
    let mogp = find_wmo_chunk(group_bytes, b"PGOM")?;
    let mut off = 0x44usize; // past the fixed group header
    while off + 8 <= mogp.len() {
        let size = u32::from_le_bytes([mogp[off + 4], mogp[off + 5], mogp[off + 6], mogp[off + 7]])
            as usize;
        let data_start = off + 8;
        let data_end = data_start.checked_add(size)?;
        if data_end > mogp.len() {
            break;
        }
        if &mogp[off..off + 4] == magic {
            return Some(&mogp[data_start..data_end]);
        }
        off = data_end;
    }
    None
}

/// A group's **MODR** doodad references: the MODD indices this group instantiates. This is the
/// faithful doodad→group OWNERSHIP relation — the real client creates a WMO doodad per (MODD index,
/// referencing group) from exactly this list (`0x695aa0` loops `group+0xe8`/count `+0x144`), and the
/// owning group's interior class + MOLR light list decide the doodad's lighting (wow-re
/// `trace-forensics-abbey-interior-d3d` §1.1/§4). Empty when the group has no MODR (no doodads).
pub fn wmo_group_doodad_refs(group_bytes: &[u8]) -> Vec<u16> {
    find_mogp_subchunk(group_bytes, b"RDOM")
        .map(|c| {
            c.chunks_exact(2)
                .map(|r| u16::from_le_bytes([r[0], r[1]]))
                .collect()
        })
        .unwrap_or_default()
}

/// A group's **MOLR** light references: the MOLT indices whose lights this group folds into its own
/// doodads' committed light (`0x695c00` builds the per-group active-light list from exactly this; a
/// group WITHOUT an MOLR chunk provably lights its doodads with nothing — the abbey's group 3 has
/// none, and the director's stand receives zero point light, its own flame 3.7 yd away included).
/// Empty when the group has no MOLR.
pub fn wmo_group_light_refs(group_bytes: &[u8]) -> Vec<u16> {
    find_mogp_subchunk(group_bytes, b"RLOM")
        .map(|c| {
            c.chunks_exact(2)
                .map(|r| u16::from_le_bytes([r[0], r[1]]))
                .collect()
        })
        .unwrap_or_default()
}

/// Build one WMO **group**'s MLIQ liquid surface as a [`LiquidMesh`] in **WMO model space** (WoW
/// axes, yards) — the flat animated water/lava/slime the group embeds (Stormwind's canals, the
/// Ironforge lava, dungeon pools). `None` when the group carries no `MLIQ`, or the grid has no wet
/// tiles / an unmapped type. The placement transform lifts the mesh into the world at spawn.
///
/// Faithful model (VERIFIED wow-re `rf-water-liquid-type-texture-material.md` + `rf-mliq-liquid-uv-texgen.md`;
/// the render dispatch is `0x6b62e0`, water kernels `0x6b6420`/`0x6b6630`): grid vertex `(i, j)` sits
/// at `base + (i·STEP, j·STEP, heights[j·xverts + i])`; a
/// tile renders iff its flag low nibble `!= 0xf` (`0xf` = hole); the type is the group's
/// `groupLiquid` override when set, else the per-tile nibble, mapped to a texture via
/// [`LiquidKind::from_nibble`]. The whole surface takes ONE resolved kind (the reference resolves a
/// single type per surface — `0x6ba970`), with per-tile hole membership.
pub fn wmo_group_liquid_mesh(group_bytes: &[u8]) -> Option<LiquidMesh> {
    let Ok(ParsedWmo::Group(group)) = parse_wmo(&mut Cursor::new(group_bytes)) else {
        return None;
    };
    let lq = group.liquid?;
    build_wmo_liquid_mesh(&lq, group.group_liquid)
}

/// Build the [`LiquidMesh`] for a parsed [`WmoLiquid`] grid + its group's `groupLiquid` override.
/// Split out so a unit test can exercise it without synthesising a full group file.
fn build_wmo_liquid_mesh(lq: &WmoLiquid, group_liquid: u32) -> Option<LiquidMesh> {
    let (xv, yv, xt, yt) = (
        lq.xverts as usize,
        lq.yverts as usize,
        lq.xtiles as usize,
        lq.ytiles as usize,
    );
    // The grid must be tile-consistent (`xtiles = xverts − 1`) and carry its full vertex/tile arrays.
    if xv < 2 || yv < 2 || xt + 1 != xv || yt + 1 != yv {
        return None;
    }
    if lq.heights.len() < xv * yv || lq.tile_flags.len() < xt * yt {
        return None;
    }

    // Resolve ONE kind for the whole surface: the `groupLiquid` override when set (`!= 0xf`), else
    // the first wet tile's nibble (`0x6ba970` returns the first non-hole nibble). Unmapped ⇒ None.
    let type_nibble = if group_liquid != 0xf {
        (group_liquid & 0xf) as u8
    } else {
        let first_wet = lq.tile_flags.iter().map(|&f| f & 0xf).find(|&n| n != 0xf)?;
        first_wet
    };
    let kind = LiquidKind::from_nibble(type_nibble)?;
    let depth_v = if kind.is_fullbright() {
        0.0
    } else {
        WMO_WATER_DEPTH_V
    };

    // Grid vertices (row-major `j·xverts + i`), WMO model space. Every vertex is emitted (dry-tile
    // corners included, unreferenced) so the tile indices line up; the AABB stays tight because WMO
    // liquid heights are real, flat values (no MCLQ FLT_MAX sentinel).
    let mut positions = Vec::with_capacity(xv * yv);
    let mut uvs = Vec::with_capacity(xv * yv);
    let mut depths = Vec::with_capacity(xv * yv);
    for j in 0..yv {
        for i in 0..xv {
            let mx = lq.base[0] + i as f32 * MLIQ_CELL_STEP;
            let my = lq.base[1] + j as f32 * MLIQ_CELL_STEP;
            let h = lq.heights[j * xv + i];
            positions.push([mx, my, if h.is_finite() { h } else { lq.base[2] }]);
            // UV from the vertex's MODEL-SPACE position, NOT its per-surface tile index. A per-surface
            // `(i·¼, j·¼)` resets to 0 at each surface's origin; a surface's tile count is arbitrary
            // (Stormwind g099 is 11 tiles = 2.75 repeats), so that reset lands mid-texture and SEAMS at
            // every surface boundary. Dividing the shared model-space position by the repeat period
            // ties all of a WMO's MLIQ surfaces to ONE continuous UV field, so grid-aligned neighbours
            // tile seamlessly. (Same per-cell ¼ step as before — `i·STEP/period = i·¼` — plus the
            // `base/period` offset that couples the surfaces.)
            uvs.push([mx / MLIQ_UV_PERIOD, my / MLIQ_UV_PERIOD]);
            depths.push(depth_v);
        }
    }

    // 2 tris per wet tile (low nibble `!= 0xf`). Winding is irrelevant (drawn two-sided).
    let mut indices = Vec::with_capacity(xt * yt * 6);
    let mut wet = vec![false; xt * yt];
    for ty in 0..yt {
        for tx in 0..xt {
            if lq.tile_flags[ty * xt + tx] & 0xf == 0xf {
                continue; // hole — no liquid on this tile
            }
            wet[ty * xt + tx] = true;
            let tl = (ty * xv + tx) as u32;
            let tr = tl + 1;
            let bl = ((ty + 1) * xv + tx) as u32;
            let br = bl + 1;
            indices.extend_from_slice(&[tl, bl, br, tl, br, tr]);
        }
    }
    if indices.is_empty() {
        return None; // grid present but every tile a hole
    }
    Some(LiquidMesh {
        grid: [xv as u32, yv as u32],
        wet,
        positions,
        uvs,
        depths,
        indices,
        // The `0x6ba970`-resolved nibble (groupLiquid override / first wet tile) IS the WMO
        // surface's sound class — the ambient-loop key (see `LiquidMesh::sound_nibble`).
        sound_nibble: type_nibble,
        kind,
    })
}

/// Build the render submeshes for one WMO **group** (its bytes), resolving textures/materials against
/// `root`. Empty if the bytes aren't a parseable group. The bytes-in entry point for the Bevy
/// `AssetLoader`; [`super::load_wmo`] is the chain-reading wrapper.
pub fn wmo_group_submeshes(group_bytes: &[u8], root: &WmoRoot) -> Result<Vec<RenderSubmesh>> {
    let ParsedWmo::Root(root) = &root.parsed else {
        anyhow::bail!("WmoRoot is not a root");
    };
    let Ok(ParsedWmo::Group(group)) = parse_wmo(&mut Cursor::new(group_bytes)) else {
        return Ok(Vec::new());
    };
    // The MOGP batch-section counts (`+0x28/+0x2a/+0x2c`, after the byte-verified portal-ref span):
    // MOBA batches are laid out TRANS, then INT, then EXT — the class decides an interior group's
    // per-batch lighting law (see [`WmoBatchClass`]). Absent/short header ⇒ everything reads EXT
    // (the conservative exterior law).
    let (trans_n, int_n) = find_wmo_chunk(group_bytes, b"PGOM")
        .filter(|m| m.len() >= 0x2c)
        .map_or((0usize, 0usize), |m| {
            (
                u16::from_le_bytes([m[0x28], m[0x29]]) as usize,
                u16::from_le_bytes([m[0x2a], m[0x2b]]) as usize,
            )
        });
    let mut out = Vec::new();
    {
        // WMO groups carry MONR normals + MOCV colours (parallel to positions); fall back when absent.
        let has_normals = group.vertex_normals.len() == group.vertex_positions.len();
        let has_colors = group.vertex_colors.len() == group.vertex_positions.len();
        // Interior iff neither the EXTERIOR (0x8) nor exterior-lit (0x40) bit is set — the same
        // `groupFlags & 0x48 == 0` test the real client branches on (RenderSubmesh::interior).
        let interior = (group.flags & 0x48) == 0;
        let vertex = |g: u32| {
            let p = &group.vertex_positions[g as usize];
            let n = group
                .vertex_normals
                .get(g as usize)
                .map_or([0.0, 1.0, 0.0], |n| [n.x, n.y, n.z]);
            let uv = group
                .texture_coords
                .get(g as usize)
                .map_or([0.0, 0.0], |t| [t.u, t.v]);
            // MOCV is BGRA; RGB is the per-vertex shade multiplier, and the REAL alpha rides along —
            // on an interior TRANS batch it is the lit↔bake lerp factor (the FixColorVertexAlpha
            // product); every other batch class forces it opaque below (alpha encodes blend/unused
            // there — the exterior alpha→0xFF fixup). Absent → white.
            let c = group
                .vertex_colors
                .get(g as usize)
                .map_or([1.0, 1.0, 1.0, 1.0], |c| {
                    [
                        c.r as f32 / 255.0,
                        c.g as f32 / 255.0,
                        c.b as f32 / 255.0,
                        c.a as f32 / 255.0,
                    ]
                });
            ([p.x, p.y, p.z], n, uv, c)
        };
        for (bi, batch) in group.render_batches.iter().enumerate() {
            let class = if bi < trans_n {
                WmoBatchClass::Trans
            } else if bi < trans_n + int_n {
                WmoBatchClass::Int
            } else {
                WmoBatchClass::Ext
            };
            let start = batch.start_index as usize;
            let global_indices: Vec<u32> = group
                .vertex_indices
                .get(start..start + batch.count as usize)
                .unwrap_or(&[])
                .iter()
                .map(|&i| u32::from(i))
                .filter(|&g| (g as usize) < group.vertex_positions.len())
                .collect();
            if global_indices.is_empty() {
                continue;
            }
            let material = root.materials.get(batch.material_id as usize);
            let texture = material
                .map(|m| m.get_texture1_index(&root.texture_offset_index_map))
                .and_then(|i| root.textures.get(i as usize))
                .cloned()
                .filter(|s| !s.is_empty());
            let blend = match material.map(|m| m.blend_mode) {
                Some(0) | None => ModelBlend::Opaque,
                Some(1) => ModelBlend::AlphaTest,
                // MOMT.blendMode is a DIRECT EGxBlend index (no M2-style remap — wow-re
                // `wmo-batch-blend-depth-state`): 4 = Mod (DST_COLOR/ZERO), 5 = Mod2x
                // (DST_COLOR/SRC_COLOR). Decision 0528.
                Some(4) => ModelBlend::Mod,
                Some(5) => ModelBlend::Mod2x,
                Some(_) => ModelBlend::Blend,
            };
            // The MOMT window/glass flags, each its own mechanism (wow-re `wmo-lit-selector` §1,
            // `wmo-interior-night-light` §2/§4 — byte-verified):
            //   UNLIT (0x01)  — the EXTERIOR drawer turns lighting off per material → `tex × white`
            //                   fullbright (the inn's always-lit outside panes, MM_ELWYNN_WND_EXT).
            //                   The INTERIOR drawer ignores it — lit/unlit there is dictated by the
            //                   batch section alone, so `emissive` stays false on interior groups.
            //   SIDN (0x10)   — the authored night-glow emissive colour, ramped by the night
            //                   fraction and added inside the lit sum (the abbey's stained glass
            //                   glowing after dusk). Lit lanes only; carried with its colour.
            //   WINDOW (0x20) — interior-drawer batches swap to the brighter midpoint light (the
            //                   warm pane seen from inside — MM_ELWYNN_WND_INT, MM_STAINED_01).
            let flags = material.map_or(0, |m| m.flags);
            let emissive = !interior && flags & 0x01 != 0;
            // UNCULLED (0x04): the real client backface-culls every WMO batch unless this MOMT
            // flag is set — byte-verified at the render-state write (wow-re models.md: bit 0x04
            // inverted → EGxRs 0x14 GL_CULL_FACE; default cull ON, GL_BACK/CCW) — the same law the
            // M2 path already honours. This used to be hardcoded two-sided ("buildings aren't the
            // canopy issue"), and a building IS the canopy issue: 1.12 authors both-sides-visible
            // cloth as the SAME faces twice with reversed winding (one copy per side — B38's tent
            // awning, `wmo_doubled`), each meant to be culled from its wrong side. Drawing them
            // two-sided rasterises both copies into an ulp-level depth tie, and the per-pixel
            // winner is floating-point noise — the B38 flicker/latch (decision 0677).
            // WMO has no skeleton, so the per-vertex skin (`_globals`) is unused.
            let (mut submesh, _globals) = remap_submesh(
                global_indices.into_iter(),
                vertex,
                texture,
                blend,
                flags & 0x04 != 0,
                interior,
                emissive,
            );
            submesh.wmo_batch = Some(class);
            submesh.sidn = (flags & 0x10 != 0)
                .then(|| material.map(|m| m.sidn_rgb))
                .flatten();
            submesh.window = flags & 0x20 != 0;
            if !has_normals {
                submesh.normals.clear(); // no MONR → let the renderer recompute flat normals
            }
            if !has_colors {
                submesh.vertex_colors.clear(); // no MOCV → empty (white, no tint)
            }
            // The MOCV **alpha** is per-vertex LIGHTING data on interior batches: a TRANS batch
            // reads it as the lit↔bake lerp factor, an INT batch as the ×4 self-illumination mask
            // (the reference's interior pixel shader `tex·MOCV·(1 + 4·a)` — the inn's fireplace
            // surround bakes α≈100, the portal-seam whitening 255). Exterior batches encode
            // blend/unused state there — force those opaque so bevy's vertex-colour fold can't
            // perturb their alpha semantics (the shader un-folds coverage for the interior lanes).
            if !(interior && matches!(class, WmoBatchClass::Trans | WmoBatchClass::Int)) {
                for c in &mut submesh.vertex_colors {
                    c[3] = 1.0;
                }
            }
            out.push(submesh);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::test_bytes::chunk;
    use super::*;

    #[test]
    fn builds_wmo_water_mesh_skipping_holes() {
        // A 3×2-vertex grid (2×1 tiles): left tile wet (lake_a, nibble 4), right tile a hole (0xf).
        let lq = WmoLiquid {
            xverts: 3,
            yverts: 2,
            xtiles: 2,
            ytiles: 1,
            base: [100.0, 200.0, -3.0],
            material_id: 0,
            heights: vec![-3.0; 6],
            tile_flags: vec![0x44, 0x0f], // wet (type 4 + fishable), hole
        };
        let m = build_wmo_liquid_mesh(&lq, 0xf).expect("a wet mesh");
        assert_eq!(m.kind, LiquidKind::Still);
        assert_eq!(m.positions.len(), 6); // full 3×2 grid emitted
        assert_eq!(m.indices.len(), 6); // one wet tile → 2 tris → 6 indices
                                        // Grid vertex (i=1, j=0) sits at base + (1·STEP, 0, height).
        assert!((m.positions[1][0] - (100.0 + MLIQ_CELL_STEP)).abs() < 1e-3);
        assert_eq!(m.positions[0], [100.0, 200.0, -3.0]);
        // Water is rendered at the deep (opaque) V.
        assert!(m.depths.iter().all(|&v| v == WMO_WATER_DEPTH_V));
        // The wet tile is the LEFT one (indices reference the left cell's 4 corners: 0,1,3,4).
        assert!(m.indices.iter().all(|&i| [0u32, 1, 3, 4].contains(&i)));
    }

    /// Two grid-aligned WMO liquid surfaces (a canal split across groups) must share ONE continuous
    /// UV field, so the animated water texture doesn't seam at the boundary. Surface B begins exactly
    /// where A's last column sits (base offset by 2·STEP in X), so the vertex they share in model space
    /// must get the SAME UV in both — the property the old per-surface `(i·¼, j·¼)` reset broke (each
    /// surface restarted at UV 0, and an arbitrary tile count put that restart mid-texture → the seam).
    #[test]
    fn adjacent_surfaces_share_a_continuous_uv_field() {
        let base = [100.0, 200.0, -3.0];
        let a = WmoLiquid {
            xverts: 3,
            yverts: 2,
            xtiles: 2,
            ytiles: 1,
            base,
            material_id: 0,
            heights: vec![-3.0; 6],
            tile_flags: vec![0x44, 0x44], // both tiles wet
        };
        // B's origin column coincides with A's rightmost column (one 2-tile span east).
        let b = WmoLiquid {
            xverts: 3,
            yverts: 2,
            xtiles: 2,
            ytiles: 1,
            base: [base[0] + 2.0 * MLIQ_CELL_STEP, base[1], base[2]],
            material_id: 0,
            heights: vec![-3.0; 6],
            tile_flags: vec![0x44, 0x44],
        };
        let ma = build_wmo_liquid_mesh(&a, 0xf).unwrap();
        let mb = build_wmo_liquid_mesh(&b, 0xf).unwrap();
        // A's rightmost-bottom vertex (i=2, j=0 → index 2) and B's leftmost-bottom (i=0, j=0 → index 0)
        // are the same model-space point; their UVs must match (no seam).
        let (ua, ub) = (ma.uvs[2], mb.uvs[0]);
        assert!(
            (ua[0] - ub[0]).abs() < 1e-4 && (ua[1] - ub[1]).abs() < 1e-4,
            "seam: shared-vertex UVs differ A={ua:?} B={ub:?}"
        );
        // The field is position-derived: UV.x still steps exactly ¼ per cell (same ripple size as before).
        assert!((ma.uvs[1][0] - ma.uvs[0][0] - 0.25).abs() < 1e-4);
    }

    #[test]
    fn wmo_liquid_type_override_and_all_holes() {
        // groupLiquid override = 2 (magma) forces the kind regardless of the wet tile's nibble.
        let lq = WmoLiquid {
            xverts: 2,
            yverts: 2,
            xtiles: 1,
            ytiles: 1,
            base: [0.0, 0.0, 0.0],
            material_id: 0,
            heights: vec![0.0; 4],
            tile_flags: vec![0x40], // nibble 0 (would be Still), but the override wins
        };
        let m = build_wmo_liquid_mesh(&lq, 2).expect("magma mesh");
        assert_eq!(m.kind, LiquidKind::Magma);
        assert!(m.depths.iter().all(|&v| v == 0.0)); // fullbright ⇒ depth V unused

        // A grid where every tile is a hole → no mesh.
        let all_holes = WmoLiquid {
            tile_flags: vec![0x0f],
            ..lq
        };
        assert!(build_wmo_liquid_mesh(&all_holes, 0xf).is_none());
    }

    #[test]
    fn reads_group_header_flags_and_portal_ref_span() {
        // A 68-byte MOGP header: flags @0x08, portal_ref_start @0x24, portal_ref_count @0x26.
        let mut hdr = vec![0u8; 68];
        hdr[8..12].copy_from_slice(&0x8u32.to_le_bytes()); // EXTERIOR
        hdr[0x24..0x26].copy_from_slice(&5u16.to_le_bytes());
        hdr[0x26..0x28].copy_from_slice(&3u16.to_le_bytes());
        hdr[0x30..0x34].copy_from_slice(&[2, 0, 0, 0]); // fog indices
                                                        // Group file: MVER then MOGP(super-chunk = header + (no) sub-chunks).
        let mut group = Vec::new();
        group.extend(chunk(b"REVM", &17u32.to_le_bytes()));
        group.extend(chunk(b"PGOM", &hdr));

        let h = wmo_group_header(&group).expect("group header");
        assert_eq!(h.flags, 0x8);
        assert_eq!(h.portal_ref_start, 5);
        assert_eq!(h.portal_ref_count, 3);
        assert_eq!(h.fog_indices, [2, 0, 0, 0]);
    }
}
