//! Vanilla (build 5875 / MD20 v256) M2 **ribbon emitter** parsing — the weapon trails, wisp
//! streamers, and spell-missile trails. Read straight from raw bytes, like `particles`.
//!
//! Byte layout is wow-5875-re's transcription-ready spec (`system/models/scratch/
//! ribbon-emitter-spec.md`, their `9c862186` — a §5 pair over the MD20 relocation fixup + the
//! ctor/render reads; closes the models item-12 remainder for the ribbon record):
//!
//! ```text
//! header array : count @ MD20+0x134, ptr @ MD20+0x138        record stride 0xdc
//! +0x04 boneIndex(u16)  +0x08 position(C3Vector, bone-local)
//! +0x14 textureIndices(M2Array<u16> → M2 textures)  +0x1c materialIndices(M2Array<u16> → the
//!       M2 render-flags table @MD20+0x84, stride 4: flags u16 + blend u16 — the blend source)
//! +0x24 colorTrack(M2Track<C3Vector>)  +0x40 alphaTrack(M2Track<fixed16>)
//! +0x5c heightAboveTrack(M2Track<f32>)  +0x78 heightBelowTrack(M2Track<f32>)
//! +0x94 edgesPerSecond(f32)  +0x98 edgeLifetime(f32, clamped ≥ 0.25)  +0x9c gravity(f32)
//! +0xa0 textureRows(u16)  +0xa2 textureCols(u16)
//! +0xa4 texSlotTrack(M2Track<u16>)  +0xc0 visibilityTrack(M2Track<u8>)
//! ```
//!
//! The **look tracks** (color, alpha, heightAbove/Below) parse as full keyed tracks through the
//! shared [`crate::value_track`] kernel, band-rebased like the particle emission tracks — the
//! one-shot spell effects key them: `HolySmite_Low_Chest.m2` flares its slash ribbons' height
//! `0 → 0.167 → 0` over 267 ms and fades their alpha `1 → 0`, so the old value[0] bake read a
//! permanent zero height and Smite's impact slash never drew (the 0141 lesson, surfaced).
//! `texSlot` stays value[0]-baked (constant in the shipped corpus). The `visibility` gate track
//! (`+0xc0`) IS parsed — **per sequence** ([`RibbonEmitterDef::visible_in_anim`]): the thrown
//! weapon keys its flight trail OFF in Stand (worn in the hand) and Impact (landed), ON only in
//! InFlight (flying), so the spawn site lights it on the missile and never in the hand. This is
//! the multi-sequence gating that was a named seam — the thrown weapon is its driving model. Ramp
//! within a sequence isn't modeled (no shipped ribbon keys it): the band-start value stands.

use std::io::Cursor;

use anyhow::Result;
use benilla_m2::parse_m2;

use crate::value_track::{seq0_band, track_keys_with, ValueTrack};
use crate::ParticleBlend;

const STRIDE: usize = 0xdc;
const HDR_COUNT: usize = 0x134;
const HDR_PTR: usize = 0x138;
/// The M2 render-flags (materials) table: `{flags u16, blend u16}` per entry.
const HDR_RENDER_FLAGS: usize = 0x84;

fn le_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// value[0] of a vanilla 0x1c M2Track whose values have `elem_size` bytes, decoded by `read`.
/// Track tail: values `{count @ +0x14, offset @ +0x18}`. `default` when empty/out-of-range.
fn track_first<T>(
    b: &[u8],
    track: usize,
    elem_size: usize,
    default: T,
    read: impl Fn(&[u8], usize) -> T,
) -> T {
    if track + 0x1c > b.len() {
        return default;
    }
    let n = le_u32(b, track + 0x14);
    let ofs = le_u32(b, track + 0x18) as usize;
    if n == 0 || ofs + elem_size > b.len() {
        return default;
    }
    read(b, ofs)
}

/// One parsed vanilla M2 ribbon emitter, reduced to what the renderer needs. Positions are
/// **bone-local** (WoW axes) — the runtime transforms through the host bone's live matrix each
/// frame, exactly like a particle emitter's origin.
#[derive(Debug, Clone)]
pub struct RibbonEmitterDef {
    pub bone: u16,
    /// Emission origin in the host bone's frame (the reference multiplies it by the bone matrix
    /// per frame — `0x718960`).
    pub position: [f32; 3],
    /// Trail texture (`.blp` path via `textureIndices[0]` → the M2 textures table). `None` if
    /// unresolved (the consumer skips the ribbon).
    pub texture: Option<String>,
    /// Blend mode from `materialIndices[0]` → the M2 render-flags entry's blend u16 (the same
    /// EGxBlend folding as particle/batch blends).
    pub blend: ParticleBlend,
    /// Trail tint (colorTrack, RGB 0..1, keyed on the clip clock; constant white when unkeyed).
    pub color: ValueTrack<[f32; 3]>,
    /// Trail opacity (alphaTrack, fixed16/32767, keyed — the slash's fade-out; constant 1.0 when
    /// unkeyed).
    pub alpha: ValueTrack,
    /// Cross-section half-widths (yards) above/below the bone path, keyed — the slash's
    /// flare-and-collapse. Sampled at edge-commit time (each edge keeps the width it was born
    /// with; the reference stores the vertex pair per edge).
    pub height_above: ValueTrack,
    pub height_below: ValueTrack,
    /// Edge spawn rate (edges/second) — with `edge_lifetime`, sizes the ring.
    pub edges_per_second: f32,
    /// Edge age-out (seconds) — the reference clamps to ≥ 0.25 at load; so do we.
    pub edge_lifetime: f32,
    /// Downward sag applied to live edges (the reference's `2·g·dt` per-frame term).
    pub gravity: f32,
    /// Texture atlas tiling (1×1 = the whole texture) + the (baked) slot index.
    pub tile_rows: u16,
    pub tile_cols: u16,
    pub tex_slot: u16,
    /// Per-sequence **visibility** from the `+0xc0` gate track (`M2Track<u8>`): `anim_id → whether
    /// the trail shows during that sequence`, sampled at the sequence's band start. `None` = no
    /// gate — keyless, global-seq-clocked, or ON in every sequence (the always-on majority: enchant
    /// trails, wisps). `Some(map)` = the trail is dark in some sequence: the thrown weapon keys it
    /// OFF in Stand (worn in hand) and Impact (landed), ON only in InFlight — so the spawn site
    /// gates on the sequence its owner runs ([`crate::RibbonEmitterDef`]). The shipped keyed ribbons
    /// are constant-per-sequence; ramping mid-sequence isn't modeled (no shipped ribbon does it).
    pub visible_in_anim: Option<std::collections::HashMap<u16, bool>>,
}

/// Per-sequence visibility from a ribbon's `+0xc0` gate track (`M2Track<u8>`, sequence-timeline).
/// Returns `anim_id → whether the trail shows during that sequence`, sampled nearest-previous at
/// each sequence's absolute band start (the shipped keyed ribbons are constant-per-sequence, so
/// the band-start value stands for the whole sequence). `None` when there is nothing to gate:
///
/// - a keyless / out-of-range track (the always-on loader default), or
/// - a **global-sequence** clock (`gseq != 0xffff`) — a free-running loop, not per-sequence, or
/// - a track that resolves ON in *every* sequence (a keyed-but-always-on author).
///
/// Sequences: MD20 count @ `0x1c`, offset @ `0x20`, stride `0x44` — `anim_id` @ +0, band `[start,
/// end]` @ +4/+8 (the same walk as [`crate::value_track::seq0_band`], one sequence wider).
fn visibility_by_anim(
    bytes: &[u8],
    vis_track: usize,
) -> Option<std::collections::HashMap<u16, bool>> {
    if vis_track + 0x1c > bytes.len() {
        return None;
    }
    // gseq != 0xffff: a global-sequence free clock, not keyed per animation — leave it always-on.
    if le_u16(bytes, vis_track + 0x02) != 0xffff {
        return None;
    }
    let tn = le_u32(bytes, vis_track + 0x0c) as usize;
    let tofs = le_u32(bytes, vis_track + 0x10) as usize;
    let vn = le_u32(bytes, vis_track + 0x14) as usize;
    let vofs = le_u32(bytes, vis_track + 0x18) as usize;
    let n = tn.min(vn);
    if n == 0 || tofs + n * 4 > bytes.len() || vofs + n > bytes.len() {
        return None; // keyless → the always-on default
    }
    let keys: Vec<(u32, bool)> = (0..n)
        .map(|i| (le_u32(bytes, tofs + i * 4), bytes[vofs + i] != 0))
        .collect();

    let nseq = le_u32(bytes, 0x1c) as usize;
    let oseq = le_u32(bytes, 0x20) as usize;
    let mut map = std::collections::HashMap::new();
    let mut any_off = false;
    for i in 0..nseq {
        let s = oseq + i * 0x44;
        if s + 0x0c > bytes.len() {
            break;
        }
        let anim = le_u16(bytes, s);
        let start = le_u32(bytes, s + 4);
        // Nearest-previous at the band start (M2 step interpolation); before the first key, the
        // first key's value; keyless-safe default ON.
        let mut on = keys.first().map(|&(_, v)| v).unwrap_or(true);
        for &(t, v) in &keys {
            if t <= start {
                on = v;
            } else {
                break;
            }
        }
        any_off |= !on;
        map.entry(anim).or_insert(on); // variations share an id — the head sequence wins
    }
    // Nothing dark anywhere ⇒ effectively always-on: no gate to carry.
    any_off.then_some(map)
}

/// Parse an M2's ribbon emitters (see the module doc). Empty when the model has none or isn't a
/// parseable M2.
pub fn parse_m2_ribbon_emitters(bytes: &[u8]) -> Result<Vec<RibbonEmitterDef>> {
    if bytes.len() < HDR_PTR + 4 || &bytes[0..4] != b"MD20" {
        return Ok(Vec::new());
    }
    // Texture paths from `benilla-m2`'s textures table (same source as particle textures).
    let textures: Vec<Option<String>> = match parse_m2(&mut Cursor::new(bytes)) {
        Ok(fmt) => fmt
            .model()
            .textures
            .iter()
            .map(|t| {
                let f = t
                    .filename
                    .string
                    .to_string_lossy()
                    .trim_end_matches('\0')
                    .to_string();
                (!f.is_empty()).then_some(f)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    let count = le_u32(bytes, HDR_COUNT) as usize;
    let base = le_u32(bytes, HDR_PTR) as usize;
    let rf_count = le_u32(bytes, HDR_RENDER_FLAGS) as usize;
    let rf_base = le_u32(bytes, HDR_RENDER_FLAGS + 4) as usize;
    // The first sequence's absolute time band — the keyed look tracks rebase onto it
    // (`value_track::seq0_band`; the same window the particle tracks use).
    let band = seq0_band(bytes);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = base + i * STRIDE;
        if e + STRIDE > bytes.len() {
            break;
        }
        // textureIndices[0] → the M2 textures table → path.
        let texture = {
            let n = le_u32(bytes, e + 0x14);
            let ofs = le_u32(bytes, e + 0x18) as usize;
            (n > 0 && ofs + 2 <= bytes.len())
                .then(|| le_u16(bytes, ofs) as usize)
                .and_then(|ti| textures.get(ti).cloned().flatten())
        };
        // materialIndices[0] → render-flags entry → blend u16.
        let blend = {
            let n = le_u32(bytes, e + 0x1c);
            let ofs = le_u32(bytes, e + 0x20) as usize;
            let mat = (n > 0 && ofs + 2 <= bytes.len())
                .then(|| le_u16(bytes, ofs) as usize)
                .filter(|&m| m < rf_count && rf_base + m * 4 + 4 <= bytes.len());
            match mat.map(|m| le_u16(bytes, rf_base + m * 4 + 2)) {
                Some(3 | 4) => ParticleBlend::Add,
                Some(2) => ParticleBlend::Alpha,
                Some(_) => ParticleBlend::Opaque,
                None => ParticleBlend::Add, // unresolved material: trails are near-always additive
            }
        };
        out.push(RibbonEmitterDef {
            bone: le_u16(bytes, e + 0x04),
            position: [
                le_f32(bytes, e + 0x08),
                le_f32(bytes, e + 0x0c),
                le_f32(bytes, e + 0x10),
            ],
            texture,
            blend,
            color: track_keys_with(bytes, e + 0x24, [1.0; 3], band, 12, |b, o| {
                [le_f32(b, o), le_f32(b, o + 4), le_f32(b, o + 8)]
            }),
            alpha: track_keys_with(bytes, e + 0x40, 1.0, band, 2, |b, o| {
                f32::from(le_u16(b, o)) / 32767.0
            }),
            height_above: track_keys_with(bytes, e + 0x5c, 0.0, band, 4, le_f32),
            height_below: track_keys_with(bytes, e + 0x78, 0.0, band, 4, le_f32),
            edges_per_second: le_f32(bytes, e + 0x94),
            edge_lifetime: le_f32(bytes, e + 0x98).max(0.25),
            gravity: le_f32(bytes, e + 0x9c),
            tile_rows: le_u16(bytes, e + 0xa0).max(1),
            tile_cols: le_u16(bytes, e + 0xa2).max(1),
            tex_slot: track_first(bytes, e + 0xa4, 2, 0, le_u16),
            visible_in_anim: visibility_by_anim(bytes, e + 0xc0),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data test skips when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The real `HolySmite_Low_Chest.m2` (Smite's impact): its yellow slash IS its two ribbon
    /// emitters, whose look tracks are KEYED inside the seq band [3300, 4433] — height flares
    /// `0 → 0.167 → 0` over the first 267 ms and alpha fades `1 → 0` by 467 ms. The old
    /// value[0] bake read a permanent zero height, so the slash never spawned (the director's
    /// "we have the glitter but not the slash"). Pins the band rebase and the keyed samples.
    #[test]
    fn real_holy_smite_slash_ribbons_are_keyed_flares() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Spells\\HolySmite_Low_Chest.m2")
            .expect("read HolySmite_Low_Chest.m2");
        let defs = parse_m2_ribbon_emitters(&bytes).expect("parse ribbons");
        assert_eq!(defs.len(), 2);
        for r in &defs {
            // Height: born at 0 (the value[0] trap), flaring to 0.167 yd at 200 ms clip time.
            assert_eq!(r.height_above.first(), 0.0, "value[0] bake = no slash");
            assert!((r.height_above.peak() - 0.167).abs() < 1e-3);
            assert!((r.height_above.sample_ms(200.0) - 0.167).abs() < 1e-3);
            assert_eq!(r.height_above.sample_ms(400.0), 0.0, "collapsed by 267 ms");
            assert_eq!(r.height_below.keys, r.height_above.keys, "symmetric slash");
            // Alpha: full through the flare, gone by 467 ms.
            assert_eq!(r.alpha.sample_ms(0.0), 1.0);
            assert!(r.alpha.sample_ms(600.0) < 1e-6);
            // Band rebase happened: every key sits on the 0-based clip clock, inside the
            // 1133 ms clip.
            assert!(r.height_above.keys.iter().all(|&(t, _)| t <= 1133));
            assert_eq!(r.color.first(), [1.0; 3]);
            // Smite's slash is a one-shot flash, not visibility-gated per sequence — the trail is
            // ON wherever it plays (its alpha, above, does the fade). So it must carry NO gate,
            // or the spawn site would wrongly darken it against a Stand default.
            assert_eq!(
                r.visible_in_anim, None,
                "an always-on slash carries no visibility gate"
            );
        }
    }

    /// The thrown dagger's flight trail is **visibility-gated per sequence** (`+0xc0`): its ribbon
    /// is dark in Stand (worn in the hand) and Impact (landed), lit only in InFlight (flying). This
    /// is the seam the multi-sequence gate closes — the worn item resting in Stand shows no trail;
    /// the InFlight missile does. Pins the per-sequence resolve against the shipped asset.
    #[test]
    fn real_thrown_dagger_trail_is_lit_only_in_flight() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Item\\ObjectComponents\\Weapon\\Thrown_1H_Dagger_A_01.m2")
            .expect("read Thrown_1H_Dagger_A_01.m2");
        let defs = parse_m2_ribbon_emitters(&bytes).expect("parse ribbons");
        assert_eq!(defs.len(), 1, "the dagger authors one trail ribbon");
        let vis = defs[0]
            .visible_in_anim
            .as_ref()
            .expect("the thrown dagger's trail IS visibility-gated");
        assert_eq!(vis.get(&0), Some(&false), "Stand (worn in hand): dark");
        assert_eq!(vis.get(&144), Some(&true), "InFlight (flying): lit");
        assert_eq!(vis.get(&191), Some(&false), "Impact (landed): dark");
    }
}
