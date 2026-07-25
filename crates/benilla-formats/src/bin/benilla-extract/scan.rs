//! Corpus-scan reports: `ribbonscan` (which models carry ribbon emitters), `groundscan` (which
//! models author flat ground-plane render geometry), and `doodadscan` (how much placed content in
//! a map block actually animates) — the population instruments that sweep many models/placements
//! rather than dumping one.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use benilla_formats::{Chain, M2AnimSummary, M2Light};

use crate::{model_key, yn};

/// Sweep every `.m2` (under `prefix`, if given) and classify every BILLBOARD batch by which way its
/// geometry faces — see the `Bbfacescan` command doc for why the sign decides visibility.
///
/// A billboard bone puts the model's **+X** toward the viewer (`billboard-bone-law`, spherical arm),
/// so a batch whose winding normal is +X faces the camera and a −X one faces away. Single-sided
/// (`two_sided` false, i.e. no material `0x04`), the away-facing ones are backface-culled by the
/// reference from every angle — they are authored placeholders the author never saw.
pub fn bbfacescan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let (mut scanned, mut cards) = (0u32, 0u32);
    // The four populations. `away_single` is the one the renderer's forced-two-sided override
    // changes: those and only those become visible when a card is not allowed to be culled.
    let (mut toward, mut away_single, mut away_two, mut edge_on) = (0u32, 0u32, 0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        let mut lines = Vec::new();
        for (i, s) in subs.iter().enumerate() {
            let Some(bb) = &s.billboard else { continue };
            cards += 1;
            // The winding normal's X component: +1 faces the viewer, −1 faces away.
            let Some(fx) = facet_x(s) else {
                edge_on += 1;
                continue;
            };
            if fx > 0.5 {
                toward += 1;
            } else if fx < -0.5 {
                if s.two_sided {
                    away_two += 1;
                } else {
                    away_single += 1;
                    lines.push(format!(
                        "    batch {i:>3}: {:?} {:?} {} verts  facetX {fx:+.2}  tex {}",
                        bb.kind,
                        s.blend,
                        s.positions.len(),
                        s.texture.as_deref().unwrap_or("NONE"),
                    ));
                }
            } else {
                edge_on += 1;
            }
        }
        if !lines.is_empty() {
            println!("{name}");
            for l in lines {
                println!("{l}");
            }
        }
    }
    eprintln!(
        "{scanned} models scanned, {cards} billboard batch(es): {toward} toward, \
         {away_single} away+single-sided (reference culls these), {away_two} away+two-sided, \
         {edge_on} edge-on/degenerate"
    );
    Ok(())
}

/// The X component of a batch's first-triangle winding normal, in WoW model space — `None` when the
/// triangle is degenerate or the normal lies in the YZ plane (nothing to decide a facing from).
fn facet_x(s: &benilla_formats::RenderSubmesh) -> Option<f32> {
    let tri = s.indices.get(..3)?;
    let p = |i: u32| s.positions.get(i as usize).copied();
    let (a, b, c) = (p(tri[0])?, p(tri[1])?, p(tri[2])?);
    let (u, v) = (
        [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
        [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
    );
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    (len > 1e-9).then(|| n[0] / len)
}

/// Sweep every `.m2` in the chain and list the models carrying RIBBON emitters.
pub fn ribbonscan(chain: &mut Chain) -> Result<()> {
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.to_ascii_lowercase().ends_with(".m2"))
        .collect();
    let (mut scanned, mut hits) = (0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let n = benilla_formats::m2_ribbon_emitter_count(&bytes);
        if n > 0 {
            hits += 1;
            println!("{n:>2}  {name}");
        }
    }
    eprintln!("{scanned} models scanned, {hits} with ribbons");
    Ok(())
}

/// Sweep every `.m2` (under `prefix`, if given) and list the models whose MATERIAL table authors
/// blend mode 5 (Mod) / 6 (Mod2x) — the multiply-blend census (decision 0528). One line per
/// matching model: its per-material `(flags, blend)` pairs and path. The raw header read (materials
/// count/ofs at `0x84`, 4-byte `{u16 flags, u16 blend}` records) matches `benilla-m2`'s parse.
pub fn blendscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let (mut scanned, mut hits) = (0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let at = |o: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(o..o + 4)?.try_into().ok()?))
        };
        let (Some(n), Some(ofs)) = (at(0x84), at(0x88)) else {
            continue;
        };
        let mats: Vec<(u16, u16)> = (0..n as usize)
            .filter_map(|i| {
                let o = ofs as usize + i * 4;
                let b = bytes.get(o..o + 4)?;
                Some((
                    u16::from_le_bytes([b[0], b[1]]),
                    u16::from_le_bytes([b[2], b[3]]),
                ))
            })
            .collect();
        if mats.iter().any(|&(_, blend)| blend == 5 || blend == 6) {
            hits += 1;
            println!("{mats:?}  {name}");
        }
    }
    eprintln!("{scanned} models scanned, {hits} with Mod/Mod2x materials");
    Ok(())
}

/// Sweep every `.m2` (under `prefix`, if given) and list the models whose particle emitters
/// carry any bit of `mask` in their file flags — see the `Partscan` command doc. One line per
/// matching emitter: its index, full flag word, shape/type, and the model path.
pub fn partscan(chain: &mut Chain, mask: u32, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let (mut scanned, mut hits, mut emitters) = (0u32, 0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let Ok(defs) = benilla_formats::parse_m2_particle_emitters(&bytes) else {
            continue;
        };
        let mut hit = false;
        for (i, d) in defs.iter().enumerate() {
            if d.flags & mask != 0 {
                hit = true;
                emitters += 1;
                println!(
                    "e{i} flags {:#010x}  {:?} {}  {name}",
                    d.flags,
                    d.shape,
                    match d.head_tail {
                        0 => "head",
                        1 => "tail",
                        _ => "head+tail",
                    },
                );
            }
        }
        if hit {
            hits += 1;
        }
    }
    eprintln!(
        "{scanned} models scanned, {hits} with mask {mask:#x} emitters ({emitters} emitters)"
    );
    Ok(())
}

/// Sweep every `.m2` (under `prefix`, if given) and classify its billboard usage — see the
/// `Bbscan` command doc. Output per model: the authored arms and how many vertices ride each
/// DIRECTLY (primary bone is the billboard bone — the card path) vs INHERITED (primary bone
/// descends from one — the joint-palette path, decision 0205).
pub fn bbscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let arm = |k: benilla_formats::BillboardKind| match k {
        benilla_formats::BillboardKind::Spherical => "S",
        benilla_formats::BillboardKind::LockX => "X",
        benilla_formats::BillboardKind::LockY => "Y",
        benilla_formats::BillboardKind::LockZ => "Z",
    };
    let (mut scanned, mut hits) = (0u32, 0u32);
    // Corpus totals: models exercising each arm, split by how the geometry rides it.
    let mut direct_models: HashMap<&'static str, u32> = HashMap::new();
    let mut inherited_models: HashMap<&'static str, u32> = HashMap::new();
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let Ok(fmt) = benilla_m2::parse_m2(&mut std::io::Cursor::new(bytes.as_slice())) else {
            continue;
        };
        let m = fmt.model();
        let kinds: Vec<Option<benilla_formats::BillboardKind>> = m
            .bones
            .iter()
            .map(|b| benilla_formats::BillboardKind::from_bone_flags(b.flags.bits()))
            .collect();
        if kinds.iter().all(|k| k.is_none()) {
            continue;
        }
        hits += 1;
        // Nearest billboard ancestor (self included) per bone — the arm whose palette
        // replacement a vertex on this bone inherits. Bounded walk (M2 parents precede
        // children; the bound is just a malformed-file guard).
        let ancestor_arm = |mut i: usize| -> Option<(bool, benilla_formats::BillboardKind)> {
            let mut hops = 0;
            loop {
                if let Some(k) = kinds.get(i).copied().flatten() {
                    return Some((hops == 0, k));
                }
                let p = usize::try_from(*m.bones.get(i).map(|b| &b.parent)?).ok()?;
                hops += 1;
                if p >= m.bones.len() || hops > m.bones.len() {
                    return None;
                }
                i = p;
            }
        };
        let (mut direct, mut inherited): (HashMap<&str, u32>, HashMap<&str, u32>) =
            (HashMap::new(), HashMap::new());
        for v in &m.vertices {
            match ancestor_arm(v.bone_indices[0] as usize) {
                Some((true, k)) => *direct.entry(arm(k)).or_default() += 1,
                Some((false, k)) => *inherited.entry(arm(k)).or_default() += 1,
                None => {}
            }
        }
        let fmt_counts = |m: &HashMap<&str, u32>| {
            let mut v: Vec<String> = m.iter().map(|(k, n)| format!("{k}:{n}")).collect();
            v.sort();
            v.join(" ")
        };
        let bones: String = kinds.iter().flatten().map(|&k| arm(k)).collect();
        println!(
            "{bones:>4}  direct[{}]  inherited[{}]  {name}",
            fmt_counts(&direct),
            fmt_counts(&inherited)
        );
        for k in direct.keys() {
            *direct_models
                .entry(match *k {
                    "S" => "S",
                    "X" => "X",
                    "Y" => "Y",
                    _ => "Z",
                })
                .or_default() += 1;
        }
        for k in inherited.keys() {
            *inherited_models
                .entry(match *k {
                    "S" => "S",
                    "X" => "X",
                    "Y" => "Y",
                    _ => "Z",
                })
                .or_default() += 1;
        }
    }
    let tot = |m: &HashMap<&'static str, u32>| {
        let mut v: Vec<String> = m.iter().map(|(k, n)| format!("{k}:{n}")).collect();
        v.sort();
        v.join(" ")
    };
    eprintln!(
        "{scanned} models scanned, {hits} with billboard bones; models by arm — direct(card) [{}]  inherited(palette) [{}]",
        tot(&direct_models),
        tot(&inherited_models)
    );
    Ok(())
}

/// Sweep every `.m2` (under `prefix`, if given) and census the **geometry a non-character model
/// draws that the reference may not** — the population instrument behind the bug channel's
/// "stray untextured primitive" family. Three independent signals per model, all read through the
/// renderer's own batch resolution (`m2batch`'s), so the report can't drift from the mechanism:
///
/// - **MULTI-GEOSET** — more than one distinct `skinSectionId`. The character compositor selects
///   among these; every other spawn path draws **all** of them, so this is exactly the population
///   an unfiltered creature/doodad/effect draw over-renders (`Creature\Banshee\Banshee.m2` is the
///   pinned case: `0`×17 + `402`×9).
/// - **UNTEX** — batches with no texture *and* no runtime slot that fills one: neither a character
///   composite slot ([`benilla_formats::RenderSubmesh::char_slot`]) nor a creature skin variation
///   ([`benilla_formats::RenderSubmesh::skin_slot`], filled at spawn from `CreatureDisplayInfo`).
///   Both fills are ordinary, so counting them would drown the signal — 324 of 420 `Creature\`
///   models carry a skin slot. What is left is geometry nothing can texture.
/// - **TINY** — batches of at most 2 faces: the literal single-triangle/quad primitives.
///
/// A model is listed when it trips any of the three. `m2batch` then explains one model in full.
pub fn geosetscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let (mut scanned, mut hits) = (0u32, 0u32);
    let (mut multi_models, mut untex_models, mut tiny_models) = (0u32, 0u32, 0u32);
    // Top-level directory → multi-geoset model count, so the report says *where* the population
    // lives (Creature/, Spells/, World/…) rather than only how big it is.
    let mut by_dir: BTreeMap<String, u32> = BTreeMap::new();
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        let mut geosets: BTreeMap<u16, u32> = BTreeMap::new();
        let (mut untex, mut tiny) = (0u32, 0u32);
        for s in &subs {
            *geosets.entry(s.geoset_id).or_default() += 1;
            if s.texture.is_none() && s.char_slot.is_none() && s.skin_slot.is_none() {
                untex += 1;
            }
            if !s.indices.is_empty() && s.indices.len() <= 6 {
                tiny += 1;
            }
        }
        let multi = geosets.len() > 1;
        if !multi && untex == 0 && tiny == 0 {
            continue;
        }
        hits += 1;
        if multi {
            multi_models += 1;
            let top = name.split_once('\\').map(|(d, _)| d).unwrap_or("<root>");
            *by_dir.entry(top.to_ascii_lowercase()).or_default() += 1;
        }
        if untex > 0 {
            untex_models += 1;
        }
        if tiny > 0 {
            tiny_models += 1;
        }
        let hist = geosets
            .iter()
            .map(|(id, n)| format!("{id}×{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut marks = Vec::new();
        if multi {
            marks.push(format!("MULTI-GEOSET({})", geosets.len()));
        }
        if untex > 0 {
            marks.push(format!("UNTEX({untex})"));
        }
        if tiny > 0 {
            marks.push(format!("TINY({tiny})"));
        }
        println!(
            "{:>3} batches  [{hist}]  {}  {name}",
            subs.len(),
            marks.join(" ")
        );
    }
    let dirs = by_dir
        .iter()
        .map(|(d, n)| format!("{d}={n}"))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "{scanned} models scanned, {hits} listed — {multi_models} MULTI-GEOSET, \
         {untex_models} with UNTEX batches, {tiny_models} with TINY batches; \
         multi-geoset by top dir: [{dirs}]"
    );
    Ok(())
}

/// Sweep every `.m2` (under `prefix`, if given) and report models that author flat ground-plane
/// render geometry — the population instrument for the class of spell effects that lie in the
/// model-space XY plane at z≈0 (WoW axes, Z up) and get buried by sloped terrain (Battle Shout's
/// crescents are the canonical case: 6 batches, each a 4-vert quad, every vertex exactly z=0, each
/// quad skinned 100% to a single bone). Per batch (same batch/geoset resolution `m2batch` uses):
/// FLAT if every vertex has `|z| <= 0.01` in model space; flat batches sub-classify QUAD-1BONE
/// (the crescent shape — [`benilla_formats::RenderSubmesh::ground_quad`], the ground-fx decal
/// lane's own detector) vs OTHER-FLAT (flat but not that shape, staying on the ordinary render
/// path) — which decides how general the renderer mechanism has to be.
pub fn groundscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let (mut scanned, mut hits, mut all_flat, mut mixed) = (0u32, 0u32, 0u32, 0u32);
    let (mut quad_total, mut other_total) = (0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        let total_batches = subs.len();
        let (mut flat_count, mut quad_count, mut other_count) = (0u32, 0u32, 0u32);
        let mut blend_modes: Vec<String> = Vec::new();
        for s in &subs {
            if s.positions.is_empty() {
                continue;
            }
            if !s.positions.iter().all(|v| v[2].abs() <= 0.01) {
                continue;
            }
            flat_count += 1;
            let bm = format!("{:?}", s.blend);
            if !blend_modes.contains(&bm) {
                blend_modes.push(bm);
            }
            // The RENDERER's own detector, so this report is exactly what the ground-fx
            // decal lane will do with each batch — the instrument can't drift from the
            // mechanism it measures.
            if s.ground_quad().is_some() {
                quad_count += 1;
            } else {
                other_count += 1;
            }
        }
        if flat_count == 0 {
            continue;
        }
        hits += 1;
        quad_total += quad_count;
        other_total += other_count;
        if flat_count as usize == total_batches {
            all_flat += 1;
        } else {
            mixed += 1;
        }
        blend_modes.sort();
        println!(
            "{total_batches:>3} batches  {flat_count:>3} flat ({quad_count:>2} quad-1bone, {other_count:>2} other-flat)  blend[{}]  {name}",
            blend_modes.join(" ")
        );
    }
    eprintln!(
        "{scanned} models scanned, {hits} with flat batches ({all_flat} all-flat, {mixed} mixed); flat batches: {quad_total} QUAD-1BONE, {other_total} OTHER-FLAT"
    );
    Ok(())
}

/// List individual doodad (MDDF) and WMO (MODF) placements around a world position whose model
/// path contains `filter` (case-insensitive) — the per-placement position / Euler rotation /
/// scale / uniqueId ground truth an orientation investigation needs (`doodadscan` only
/// aggregates).
pub fn placescan(
    chain: &mut Chain,
    map: &str,
    center_x: f32,
    center_y: f32,
    tile_radius: u32,
    filter: &str,
) -> Result<()> {
    let tiles = benilla_formats::load_tiles_around(chain, map, center_x, center_y, tile_radius)
        .with_context(|| format!("loading tiles around ({center_x}, {center_y}) on {map}"))?;
    eprintln!("{} tile(s) loaded", tiles.len());
    let needle = filter.to_ascii_lowercase();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut hits = 0u32;
    for (_, tile) in &tiles {
        for d in &tile.doodads {
            if seen.insert(d.unique_id) && model_key(&d.model).contains(&needle) {
                hits += 1;
                println!(
                    "MDDF uid {:>8}  pos ({:>9.2}, {:>9.2}, {:>8.2})  rot deg ({:>7.2}, {:>7.2}, {:>7.2})  scale {:.3}  {}",
                    d.unique_id,
                    d.position[0], d.position[1], d.position[2],
                    d.rotation[0], d.rotation[1], d.rotation[2],
                    d.scale,
                    d.model,
                );
            }
        }
        for w in &tile.wmos {
            if seen.insert(w.unique_id) && w.model.to_ascii_lowercase().contains(&needle) {
                hits += 1;
                println!(
                    "MODF uid {:>8}  pos ({:>9.2}, {:>9.2}, {:>8.2})  rot deg ({:>7.2}, {:>7.2}, {:>7.2})  {}",
                    w.unique_id,
                    w.position[0], w.position[1], w.position[2],
                    w.rotation[0], w.rotation[1], w.rotation[2],
                    w.model,
                );
            }
        }
    }
    eprintln!("{hits} placement(s) matched '{filter}'");
    Ok(())
}

/// Bulk-scan placed doodads (MDDF) and WMO doodad-set-0 props (MODF → MODS/MODD) across a
/// `(2·tile_radius+1)²` block of ADT tiles around a world position, and report how much of that
/// content animates.
pub fn doodadscan(
    chain: &mut Chain,
    map: &str,
    center_x: f32,
    center_y: f32,
    tile_radius: u32,
) -> Result<()> {
    let tiles = benilla_formats::load_tiles_around(chain, map, center_x, center_y, tile_radius)
        .with_context(|| format!("loading tiles around ({center_x}, {center_y}) on {map}"))?;
    eprintln!("{} tile(s) loaded", tiles.len());

    // Direct M2 placements (MDDF) — deduped by uniqueId, which a tile-straddling doodad
    // repeats identically across every tile it touches (decision-0021 terrain streamer's own
    // dedup key; see `benilla_formats::Doodad::unique_id`).
    let mut seen_doodad_ids: HashSet<u32> = HashSet::new();
    let mut m2_instances: HashMap<String, u32> = HashMap::new();
    // WMO placements (MODF) — same dedup, by their own uniqueId.
    let mut seen_wmo_ids: HashSet<u32> = HashSet::new();
    let mut wmo_instances: HashMap<String, u32> = HashMap::new();
    for (_, tile) in &tiles {
        for d in &tile.doodads {
            if seen_doodad_ids.insert(d.unique_id) {
                *m2_instances.entry(model_key(&d.model)).or_insert(0) += 1;
            }
        }
        for w in &tile.wmos {
            if seen_wmo_ids.insert(w.unique_id) {
                *wmo_instances.entry(w.model.clone()).or_insert(0) += 1;
            }
        }
    }
    let direct_m2_instances: u32 = m2_instances.values().sum();
    eprintln!(
        "{direct_m2_instances} MDDF doodad placement(s) across {} unique M2 model(s)",
        m2_instances.len()
    );
    eprintln!(
        "{} MODF WMO placement(s) across {} unique WMO model(s)",
        seen_wmo_ids.len(),
        wmo_instances.len()
    );

    // Fold each unique WMO's doodad-set-**0** M2 props into the same instance table (set 0 is
    // the WMO's always-on global set, per `WmoDoodadSet` doc), each multiplied by that WMO's
    // own (deduped) placement count — one building placement = one instance of every set-0 prop
    // it carries.
    let mut wmo_root_failures = 0u32;
    for (wmo_path, &count) in &wmo_instances {
        let root_path = wmo_path.to_ascii_lowercase(); // matches `load_wmo`'s own normalization
        let root = chain
            .read_file(&root_path)
            .ok()
            .and_then(|bytes| benilla_formats::parse_wmo_root(&bytes).ok());
        let Some(root) = root else {
            wmo_root_failures += 1;
            continue;
        };
        let Some(set0) = root.doodad_sets().first() else {
            continue;
        };
        let range = set0.start as usize..(set0.start as usize + set0.count as usize);
        for wd in root.doodads().get(range).unwrap_or(&[]) {
            if !wd.model.is_empty() {
                *m2_instances.entry(model_key(&wd.model)).or_insert(0) += count;
            }
        }
    }
    if wmo_root_failures > 0 {
        eprintln!("  ({wmo_root_failures} WMO root(s) failed to read/parse — skipped)");
    }

    // Per-unique-model animation summary — one parse per model regardless of instance count.
    let mut summaries: HashMap<String, M2AnimSummary> = HashMap::new();
    let mut parse_failures: Vec<(String, String)> = Vec::new();
    for model in m2_instances.keys() {
        match benilla_formats::load_m2_animation_summary(chain, model) {
            Ok(s) => {
                summaries.insert(model.clone(), s);
            }
            Err(e) => parse_failures.push((model.clone(), e.to_string())),
        }
    }

    let total_instances: u32 = m2_instances.values().sum();
    let total_models = m2_instances.len();
    println!();
    println!(
        "=== totals ({total_instances} M2 instance(s), {total_models} unique model(s), {} parse failure(s)) ===",
        parse_failures.len()
    );

    type ChannelCheck = (&'static str, fn(&M2AnimSummary) -> bool);
    let checks: [ChannelCheck; 9] = [
        ("seq-0 bone motion", |s| s.seq0_has_bone_motion),
        ("moving seq0, >1 variation", |s| {
            s.seq0_has_bone_motion && s.seq0_variation_count > 1
        }),
        ("global-seq bones", |s| !s.global_seq_channels.is_empty()),
        ("animated transparency", |s| s.transparency_tracks.1 > 0),
        ("animated color", |s| {
            s.color_rgb_tracks.1 > 0 || s.color_alpha_tracks.1 > 0
        }),
        ("texture transforms", |s| s.texture_transform_count > 0),
        ("particles", |s| s.particle_emitter_count > 0),
        ("emitter on moving bone", |s| {
            s.emitter_bones.iter().any(|e| e.chain_animated())
        }),
        ("ribbons", |s| s.ribbon_emitter_count > 0),
    ];
    let report_row = |label: &str, pred: &dyn Fn(&M2AnimSummary) -> bool| {
        let inst: u32 = m2_instances
            .iter()
            .filter_map(|(m, &c)| summaries.get(m).filter(|s| pred(s)).map(|_| c))
            .sum();
        let models = summaries.values().filter(|s| pred(s)).count();
        println!(
            "  {label:22} {inst:>6} instances ({:5.1}%)   {models:>4} models ({:5.1}%)",
            100.0 * f64::from(inst) / f64::from(total_instances.max(1)),
            100.0 * models as f64 / total_models.max(1) as f64,
        );
    };
    for (label, pred) in checks {
        report_row(label, &pred);
    }
    report_row("NO animated channel", &|s| s.is_fully_static());

    println!();
    println!("=== top 30 models by instance count ===");
    println!(
        "{:>6}  {:>4} {:>4} {:>5} {:>5} {:>4} {:>4} {:>4}  model",
        "count", "seq0", "gseq", "trns", "clr", "txfm", "part", "ribn"
    );
    let mut ranked: Vec<(&String, &u32)> = m2_instances.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (model, &count) in ranked.iter().take(30) {
        match summaries.get(*model) {
            Some(s) => println!(
                "{count:>6}  {:>4} {:>4} {:>5} {:>5} {:>4} {:>4} {:>4}  {model}",
                yn(s.seq0_has_bone_motion),
                yn(!s.global_seq_channels.is_empty()),
                yn(s.transparency_tracks.1 > 0),
                yn(s.color_rgb_tracks.1 > 0 || s.color_alpha_tracks.1 > 0),
                yn(s.texture_transform_count > 0),
                yn(s.particle_emitter_count > 0),
                yn(s.ribbon_emitter_count > 0),
            ),
            None => println!("{count:>6}  <parse failed>  {model}"),
        }
    }

    // The rare material channels by NAME (each is <1% of instances, so the top-30 table
    // almost never surfaces them): the exact models the phase-2/3 material-animation work
    // verifies against.
    println!();
    println!("=== material-channel models (animated transparency / color / UV) ===");
    let mut rare: Vec<(&String, &u32)> = m2_instances
        .iter()
        .filter(|(m, _)| {
            summaries.get(*m).is_some_and(|s| {
                s.transparency_tracks.1 > 0
                    || s.color_rgb_tracks.1 > 0
                    || s.color_alpha_tracks.1 > 0
                    || s.texture_transform_count > 0
            })
        })
        .collect();
    rare.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (model, &count) in &rare {
        let s = &summaries[*model];
        println!(
            "{count:>6}  trns:{} clr:{} txfm:{}  {model}",
            s.transparency_tracks.1,
            s.color_rgb_tracks.1 + s.color_alpha_tracks.1,
            s.texture_transform_count,
        );
    }

    // Moving-seq0 models with a variation chain, by NAME (the wow-re §4a random-variation
    // arm correction): the exact placed models where variationIdx −1 vs 0 is visible at all.
    println!();
    println!("=== moving-seq0 multi-variation models ===");
    let mut varied: Vec<(&String, &u32)> = m2_instances
        .iter()
        .filter(|(m, _)| {
            summaries
                .get(*m)
                .is_some_and(|s| s.seq0_has_bone_motion && s.seq0_variation_count > 1)
        })
        .collect();
    varied.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (model, &count) in &varied {
        let s = &summaries[*model];
        println!(
            "{count:>6}  {} variation(s) of seq0  {model}",
            s.seq0_variation_count
        );
    }

    // Emitters hosted on a moving bone chain, by NAME (0130 phase 4 grounding): the exact
    // placed models where emitter bone-follow is visible at all — an emitter on a static
    // chain sits at its rest pose whether or not we attach it.
    println!();
    println!("=== emitter-on-moving-bone models ===");
    let mut movers: Vec<(&String, &u32)> = m2_instances
        .iter()
        .filter(|(m, _)| {
            summaries
                .get(*m)
                .is_some_and(|s| s.emitter_bones.iter().any(|e| e.chain_animated()))
        })
        .collect();
    movers.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (model, &count) in &movers {
        let s = &summaries[*model];
        let moving = s
            .emitter_bones
            .iter()
            .filter(|e| e.chain_animated())
            .count();
        println!(
            "{count:>6}  {moving}/{} emitter(s) on moving bones  {model}",
            s.emitter_bones.len()
        );
    }

    if !parse_failures.is_empty() {
        println!();
        println!("=== parse failures ({}) ===", parse_failures.len());
        for (model, err) in &parse_failures {
            println!("  {model}: {err}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// partcensus — the particle FEATURE CENSUS (see the `Partcensus` command doc).
// ---------------------------------------------------------------------------

/// The census's raw-extras view of one on-disk emitter record: the fields the shipped
/// [`benilla_formats::ParticleEmitterDef`] deliberately does not carry (yet). Read straight off
/// the record bytes (stride/header per the parser's module doc): the two model-filename M2Arrays
/// at `+0x18` (geometry model — 3-D "model particles") and `+0x20` (recursion model — per-particle
/// child emitters), and the emission-rate track's interpolation word (`+0xdc`, 0 = step).
struct RecordExtras {
    geometry_model: Option<String>,
    recursion_model: Option<String>,
    rate_interp: u16,
    rate_keys: u32,
}

/// Read the raw extras for every emitter record in an M2 (empty if not an MD20 or no emitters).
fn record_extras(bytes: &[u8]) -> Vec<RecordExtras> {
    const STRIDE: usize = 0x1f8;
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let str_at = |count: usize, ofs: usize| -> Option<String> {
        if count == 0 || ofs == 0 || ofs + count > bytes.len() {
            return None;
        }
        let s = String::from_utf8_lossy(&bytes[ofs..ofs + count])
            .trim_end_matches('\0')
            .to_string();
        (!s.is_empty()).then_some(s)
    };
    if bytes.len() < 0x144 || &bytes[..4] != b"MD20" {
        return Vec::new();
    }
    let count = u32_at(0x13c) as usize;
    let base = u32_at(0x140) as usize;
    if count == 0 || count > 256 || base + count * STRIDE > bytes.len() {
        return Vec::new();
    }
    (0..count)
        .map(|i| {
            let e = base + i * STRIDE;
            RecordExtras {
                geometry_model: str_at(u32_at(e + 0x18) as usize, u32_at(e + 0x1c) as usize),
                recursion_model: str_at(u32_at(e + 0x20) as usize, u32_at(e + 0x24) as usize),
                rate_interp: u16_at(e + 0xdc),
                rate_keys: u32_at(e + 0xdc + 0x14),
            }
        })
        .collect()
}

/// model key → the spells whose visual chain plays that model (any kit stage, or the missile).
fn spell_attribution(chain: &mut Chain) -> HashMap<String, Vec<(u32, String)>> {
    let (Ok(spells), Ok(visuals)) = (
        benilla_formats::load_spell_catalog(chain),
        benilla_formats::load_spell_visual_catalog(chain),
    ) else {
        eprintln!("(spell/visual DBCs unavailable — census runs without spell attribution)");
        return HashMap::new();
    };
    let mut map: HashMap<String, Vec<(u32, String)>> = HashMap::new();
    for (id, sp) in spells.iter() {
        let Some(st) = visuals.stages(sp.visual) else {
            continue;
        };
        let mut push = |effect: u32| {
            if effect != 0 {
                if let Some(p) = visuals.effect_path(effect) {
                    map.entry(crate::model_key(p))
                        .or_default()
                        .push((id, sp.name.clone()));
                }
            }
        };
        for kit_id in [st.precast, st.cast, st.impact, st.state, st.channel] {
            if kit_id == 0 {
                continue;
            }
            if let Some(kit) = visuals.kit(kit_id) {
                for (_, effect) in kit.effects() {
                    push(effect);
                }
            }
        }
        push(st.missile_model);
    }
    map
}

/// One census dimension's tally. `model_count` counts every distinct model exactly;
/// `models` keeps the first 64 (sorted) for the example listings.
#[derive(Default)]
struct Tally {
    emitters: u32,
    model_count: u32,
    last_model: String,
    models: std::collections::BTreeSet<String>,
}

impl Tally {
    /// The walk visits each model's emitters consecutively, so "new model" is a change of name.
    fn hit(&mut self, model: &str) {
        self.emitters += 1;
        if self.last_model != model {
            self.model_count += 1;
            self.last_model = model.to_string();
        }
        if self.models.len() < 64 {
            self.models.insert(model.to_string());
        }
    }
}

/// Sweep every `.m2` (under `prefix`, if given) and census which particle-emitter FEATURES the
/// corpus actually authors — see the `Partcensus` command doc.
pub fn partcensus(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let attribution = spell_attribution(chain);
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();

    let mut tallies: std::collections::BTreeMap<&'static str, Tally> =
        std::collections::BTreeMap::new();
    let (mut scanned, mut with_emitters, mut total_emitters) = (0u32, 0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let Ok(defs) = benilla_formats::parse_m2_particle_emitters(&bytes) else {
            continue;
        };
        if defs.is_empty() {
            continue;
        }
        with_emitters += 1;
        let extras = record_extras(&bytes);
        let key = crate::model_key(&name);
        for (i, d) in defs.iter().enumerate() {
            total_emitters += 1;
            let mut hit = |k: &'static str| tallies.entry(k).or_default().hit(&key);
            match d.shape {
                benilla_formats::ParticleShape::Plane => hit("shape:plane"),
                benilla_formats::ParticleShape::Sphere => hit("shape:sphere"),
                benilla_formats::ParticleShape::Spline => hit("shape:SPLINE"),
            }
            match d.head_tail {
                0 => hit("type:head"),
                1 => hit("type:tail"),
                _ => hit("type:head+tail"),
            }
            match d.blend {
                benilla_formats::ParticleBlend::Add => hit("blend:add"),
                benilla_formats::ParticleBlend::Alpha => hit("blend:alpha"),
                benilla_formats::ParticleBlend::Opaque => hit("blend:opaque"),
            }
            // The raw blend byte disambiguates what the parsed enum folds: 5 = Mod, 6 = Mod2x.
            {
                const STRIDE: usize = 0x1f8;
                let base = u32::from_le_bytes(bytes[0x140..0x144].try_into().unwrap()) as usize;
                match bytes[base + i * STRIDE + 0x28] {
                    5 => hit("blend:MOD(5)"),
                    6 => hit("blend:MOD2X(6)"),
                    _ => {}
                }
            }
            if d.flags & 0x1 == 0 {
                hit("flag:LIT(0x1 clear)");
            }
            for (bit, label) in [
                (0x8u32, "flag:0x8 texenv"),
                (0x10, "flag:0x10 model-space"),
                (0x20, "flag:0x20 scale-by-instance"),
                (0x40, "flag:0x40 MOTION-VEL-INHERIT"),
                (0x80, "flag:0x80 KILL-OUTBOUND"),
                (0x100, "flag:0x100 sphere-up"),
                (0x200, "flag:0x200"),
                (0x400, "flag:0x400 tail-age-clamp"),
                (0x800, "flag:0x800 SPAWN-PATH-SPREAD"),
                (0x1000, "flag:0x1000 xy-quad"),
                (0x2000, "flag:0x2000 GROUND-SNAP"),
                (0x4000, "flag:0x4000 FOLLOW-DELTA"),
                (0x8000, "flag:0x8000 burst"),
            ] {
                if d.flags & bit != 0 {
                    hit(label);
                }
            }
            if d.flags >> 16 != 0 {
                hit("flag:HIGH-BITS(>0xffff)");
            }
            if d.spin > 0.0 {
                hit("spin:positive");
            }
            if d.spin < 0.0 {
                hit("spin:negative");
            }
            if d.twinkle_percent < 1.0 {
                hit("twinkle:percent<1");
            }
            if u32::from(d.tile_cols) * u32::from(d.tile_rows) > 1 {
                hit("tiles:atlas");
                if !d.tile_cols.is_power_of_two() {
                    hit("tiles:NON-POW2-COLS");
                }
            }
            if d.z_source != 0.0 {
                hit("kernel:zSource");
            }
            if d.gravity < 0.0 {
                hit("kernel:negative-gravity");
            }
            if d.shape == benilla_formats::ParticleShape::Sphere
                && d.vertical_range > 3.0
                && d.horizontal_range == 0.0
            {
                hit("kernel:edge-on-ring(lat±π,lon0)");
            }
            if let Some(x) = extras.get(i) {
                if x.geometry_model.is_some() {
                    hit("MODEL-PARTICLES(geometry)");
                }
                if x.recursion_model.is_some() {
                    hit("CHILD-EMITTERS(recursion)");
                }
                if x.rate_keys > 1 && x.rate_interp != 0 {
                    hit("rate:LERP-RAMP(interp!=0)");
                    // A BURST emitter with a lerp rate track would arm a near-zero count at its
                    // rising edge — if the corpus authored one, the burst edge law needs a re-look.
                    if d.burst() {
                        hit("rate:BURST+LERP(suspect)");
                    }
                }
            }
        }
    }

    println!(
        "== particle feature census  prefix={}  ({scanned} models scanned, {with_emitters} with emitters, {total_emitters} emitters)",
        prefix.unwrap_or("<all>"),
    );
    for (k, t) in &tallies {
        let ex: Vec<&str> = t.models.iter().take(3).map(|s| s.as_str()).collect();
        println!(
            "{:>6} emitters  {:>4} models  {k}  e.g. {}",
            t.emitters,
            t.model_count,
            ex.join(" · ")
        );
    }

    // The full model list — with spell attribution — for the dimensions that decide mechanism
    // scope (the UPPERCASE keys: unimplemented or folded legs, plus the odd corners).
    let detail: &[&str] = &[
        "MODEL-PARTICLES(geometry)",
        "CHILD-EMITTERS(recursion)",
        "shape:SPLINE",
        "blend:MOD(5)",
        "blend:MOD2X(6)",
        "flag:LIT(0x1 clear)",
        "flag:0x40 MOTION-VEL-INHERIT",
        "flag:0x80 KILL-OUTBOUND",
        "flag:0x800 SPAWN-PATH-SPREAD",
        "flag:0x2000 GROUND-SNAP",
        "flag:0x4000 FOLLOW-DELTA",
        "flag:HIGH-BITS(>0xffff)",
        "tiles:NON-POW2-COLS",
        "rate:LERP-RAMP(interp!=0)",
    ];
    for k in detail {
        let Some(t) = tallies.get(k) else { continue };
        println!();
        println!(
            "=== {k}  ({} emitters, {} models)",
            t.emitters, t.model_count
        );
        for m in t.models.iter().take(16) {
            let spells = attribution.get(m).map_or(String::new(), |v| {
                let mut seen = HashSet::new();
                let names: Vec<String> = v
                    .iter()
                    .filter(|(id, _)| seen.insert(*id))
                    .take(3)
                    .map(|(id, n)| format!("{id} {n}"))
                    .collect();
                if names.is_empty() {
                    String::new()
                } else {
                    format!(
                        "   [{}{}]",
                        names.join(", "),
                        if v.len() > 3 { ", …" } else { "" }
                    )
                }
            });
            println!("  {m}{spells}");
        }
        if t.model_count as usize > 16 {
            println!("  … {} more", t.model_count as usize - 16);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// m2lightscan — the M2 dynamic-LIGHT population instrument (see the `M2lightscan` command doc).
// ---------------------------------------------------------------------------

/// Capitalize an `Item\ObjectComponents\<sub>` path component for consistent family-key display
/// regardless of how a given asset's listfile entry happened to be cased (`WEAPON`/`weapon`/
/// `Weapon` all collapse to `Weapon`).
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

/// The top-level content-family bucket for an internal M2 path — `m2lightscan`'s summary
/// dimension for "how much content is affected, and of what kind". Derived from the path's first
/// one or two components, case-insensitively. `World\Goober\` (GameObject displays) splits from
/// plain `World\` (ADT-placed doodads / WMO props) because only the latter is on benilla's
/// current M2-light spawn path (`crate::terrain_stream::spawn::fx::spawn_lights_for`); everything
/// not otherwise named folds into `other`.
fn family_of(name: &str) -> String {
    let comps: Vec<&str> = name.split('\\').collect();
    let low = |s: &str| s.to_ascii_lowercase();
    match comps.first().map(|s| low(s)).as_deref() {
        Some("creature") => "Creature\\".to_string(),
        Some("character") => "Character\\".to_string(),
        Some("spells") => "Spells\\".to_string(),
        Some("item") if comps.get(1).map(|s| low(s)).as_deref() == Some("objectcomponents") => {
            match comps.get(2) {
                Some(sub) => format!("Item\\ObjectComponents\\{}\\", title_case(sub)),
                None => "Item\\ObjectComponents\\".to_string(),
            }
        }
        Some("world") if comps.get(1).map(|s| low(s)).as_deref() == Some("goober") => {
            "World\\Goober\\".to_string()
        }
        Some("world") => "World\\".to_string(),
        _ => "other".to_string(),
    }
}

/// How many rows of the closing colour tally print (the rest are counted, never silently dropped).
const TALLY_ROWS: usize = 20;

/// Cheap warm/cool/neutral hue classification of a `diffuse_color`, used only to eyeball the
/// colour-tally section of `m2lightscan`'s summary — the warm-torch family vs anything unusual.
fn hue_tag(r: f32, g: f32, b: f32) -> &'static str {
    if r >= g && r > b * 1.15 {
        "warm"
    } else if b > r && b >= g {
        "cool"
    } else {
        "neutral"
    }
}

/// Per-family tally for `m2lightscan`'s summary: how many models in this content family carry
/// lights, how many `type==1` point lights they author in total, how many of those are dark
/// (`visibility_off`), and a handful of example paths.
#[derive(Default)]
struct FamilyStats {
    models: u32,
    point_lights: u32,
    dark: u32,
    examples: Vec<String>,
}

/// Sweep every `.m2` (optionally under a path prefix) and report which models author M2 dynamic
/// LIGHT blocks — the population instrument for the mechanism (decision 0016 / wow-re
/// `system/models/scratch/m2-dynamic-lights.md`). Per model (only models with ≥1 light, printed
/// sorted by path): its `type==1` point-light count vs directional (`type==0`, ambient-feed, not
/// a discrete GL light) count, then per POINT light: bone, model-space position, `diffuse_color ×
/// diffuse_intensity` (raw colour, intensity, and the product), authored attenuation start/end,
/// and an `OFF` tag when [`M2Light::visibility_off`] — the one shape (a static `0` visibility
/// key) that keeps a light dark (§9.4). The closing summary is the real deliverable: totals, a
/// breakdown by top-level content family ([`family_of`]) — benilla only spawns these lights for
/// ADT-placed doodads and WMO props today, so this answers how much of the entity path
/// (creatures, held items, GameObjects) is actually missing them — and a cheap diffuse
/// colour×intensity tally ([`hue_tag`]).
pub fn m2lightscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();

    // Rounded `(r, g, b) × 100` (int-keyed to stay orderable) — a cheap grouping key for the
    // authored diffuse×intensity palette across point lights.
    type ColorKey = (i32, i32, i32);

    let (mut scanned, mut hits, mut total_point, mut total_dark) = (0u32, 0u32, 0u32, 0u32);
    let mut families: BTreeMap<String, FamilyStats> = BTreeMap::new();
    // key -> (hit count, one example model).
    let mut color_tally: BTreeMap<ColorKey, (u32, String)> = BTreeMap::new();
    let mut hit_models: Vec<(String, Vec<M2Light>)> = Vec::new();

    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let lights = benilla_formats::parse_m2_lights(&bytes);
        if lights.is_empty() {
            continue;
        }
        hits += 1;
        let point_count = lights.iter().filter(|l| l.is_point()).count() as u32;
        let dark_count = lights
            .iter()
            .filter(|l| l.is_point() && l.visibility_off)
            .count() as u32;
        total_point += point_count;
        total_dark += dark_count;

        let fam = families.entry(family_of(&name)).or_default();
        fam.models += 1;
        fam.point_lights += point_count;
        fam.dark += dark_count;
        if fam.examples.len() < 8 {
            fam.examples.push(name.clone());
        }

        for l in lights.iter().filter(|l| l.is_point()) {
            let key = (
                (l.diffuse_color[0] * l.diffuse_intensity * 100.0).round() as i32,
                (l.diffuse_color[1] * l.diffuse_intensity * 100.0).round() as i32,
                (l.diffuse_color[2] * l.diffuse_intensity * 100.0).round() as i32,
            );
            color_tally
                .entry(key)
                .or_insert_with(|| (0, name.clone()))
                .0 += 1;
        }

        hit_models.push((name, lights));
    }

    hit_models.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, lights) in &hit_models {
        let point_count = lights.iter().filter(|l| l.is_point()).count();
        let dir_count = lights.len() - point_count;
        println!("{name}  {point_count} point, {dir_count} directional");
        for (i, l) in lights.iter().enumerate() {
            if !l.is_point() {
                continue;
            }
            let prod = [
                l.diffuse_color[0] * l.diffuse_intensity,
                l.diffuse_color[1] * l.diffuse_intensity,
                l.diffuse_color[2] * l.diffuse_intensity,
            ];
            println!(
                "    L{i}  bone {:>4}  pos ({:>9.3}, {:>9.3}, {:>9.3})  diffuse ({:.3}, {:.3}, {:.3}) x {:.3} = ({:.3}, {:.3}, {:.3})  atten [{:.2}, {:.2}]{}",
                l.bone,
                l.position[0], l.position[1], l.position[2],
                l.diffuse_color[0], l.diffuse_color[1], l.diffuse_color[2],
                l.diffuse_intensity,
                prod[0], prod[1], prod[2],
                l.attenuation_start, l.attenuation_end,
                if l.visibility_off { "  OFF" } else { "" },
            );
        }
    }

    println!();
    println!(
        "=== summary ===  {scanned} models scanned, {hits} with light blocks, {total_point} point lights, {total_dark} dark (visibility_off) point lights"
    );

    println!();
    println!("=== by content family ===");
    for (fam, stats) in &families {
        println!(
            "{fam:<32} {:>4} models  {:>4} point lights  {:>3} dark    e.g. {}",
            stats.models,
            stats.point_lights,
            stats.dark,
            stats.examples.join(" · ")
        );
    }

    println!();
    println!("=== diffuse colour x intensity tally (point lights, rounded to 0.01) ===");
    let mut ranked: Vec<(&ColorKey, &(u32, String))> = color_tally.iter().collect();
    ranked.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    for (key, (count, example)) in ranked.iter().take(TALLY_ROWS) {
        let (r, g, b) = (
            key.0 as f32 / 100.0,
            key.1 as f32 / 100.0,
            key.2 as f32 / 100.0,
        );
        let tag = hue_tag(r, g, b);
        println!("{count:>4}x  ({r:.2}, {g:.2}, {b:.2})  {tag:<5}  e.g. {example}");
    }
    // Never let the top-20 read as "that's all of them".
    if let Some(rest) = ranked.len().checked_sub(TALLY_ROWS).filter(|n| *n > 0) {
        println!("      … and {rest} rarer colours (top {TALLY_ROWS} shown)");
    }

    Ok(())
}

/// Sweep every `.m2` (under `prefix`, if given) and census the models whose batch visibility is
/// **per sequence** — geometry the reference draws in one animation and skips in another, via the
/// verified alpha combine (`A = colourAlpha × weight`, `A ≤ 0` culls; wow-re
/// `m2-alpha-combine-cull.md`).
///
/// This is the population instrument for the class of bug where a client bakes the material tracks
/// once and draws the result forever: every model listed here has at least one batch whose authored
/// visibility CHANGES between sequences, so a single-sequence bake is guaranteed to be wrong for it
/// in some animation. Per model it reports how many batches are **hidden in the model's first
/// sequence** (what a doodad-shaped bake would show) versus hidden in *some* sequence, so the two
/// failure directions — drawing geometry that should be hidden, and hiding geometry that should
/// draw — are separated. `m2alpha` then explains one model in full.
pub fn alphascan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect();
    let (mut scanned, mut hits) = (0u32, 0u32);
    let mut by_dir: BTreeMap<String, u32> = BTreeMap::new();
    let mut rows: Vec<(String, usize, usize, usize)> = Vec::new();
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        let seq_count = benilla_formats::parse_m2_animations(&bytes).len();
        if seq_count < 2 {
            continue; // a one-sequence model can't disagree with itself
        }
        // A batch is "hidden in slot s" when its combined factor is 0 across that whole band. The
        // sampling grid is coarse on purpose — a batch that so much as flickers non-zero is drawn.
        let hidden_in = |sub: &benilla_formats::RenderSubmesh, slot: usize| -> bool {
            sub.alpha_anim.as_ref().is_some_and(|a| {
                (0..=16u16).all(|k| a.sample(Some(slot), f32::from(k) * 0.25) <= 0.0)
            })
        };
        let (mut first, mut any, mut varies) = (0usize, 0usize, 0usize);
        for sub in &subs {
            let h0 = hidden_in(sub, 0);
            let mut hid_any = h0;
            let mut differs = false;
            for slot in 1..seq_count {
                let h = hidden_in(sub, slot);
                hid_any |= h;
                differs |= h != h0;
            }
            if h0 {
                first += 1;
            }
            if hid_any {
                any += 1;
            }
            if differs {
                varies += 1;
            }
        }
        if varies == 0 {
            continue;
        }
        hits += 1;
        let top = name.split_once('\\').map(|(d, _)| d).unwrap_or("<root>");
        *by_dir.entry(top.to_ascii_lowercase()).or_default() += 1;
        rows.push((name, varies, first, any));
    }
    // Loudest first: the models where the most geometry changes hands between sequences.
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!(
        "model                                                        varies  hid@seq0  hid@any"
    );
    for (name, varies, first, any) in rows.iter().take(60) {
        println!("{name:<60}  {varies:>6}  {first:>8}  {any:>7}");
    }
    if rows.len() > 60 {
        println!("… and {} more", rows.len() - 60);
    }
    println!("\n{hits} of {scanned} models author per-sequence batch visibility");
    println!("by top-level directory:");
    for (dir, n) in &by_dir {
        println!("  {dir:<16} {n:>5}");
    }
    Ok(())
}
