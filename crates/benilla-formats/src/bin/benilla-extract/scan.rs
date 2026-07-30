//! Corpus-scan reports: `ribbonscan` (which models carry ribbon emitters), `groundscan` (which
//! models author flat ground-plane render geometry), and `doodadscan` (how much placed content in
//! a map block actually animates) — the population instruments that sweep many models/placements
//! rather than dumping one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result};
use benilla_formats::{Chain, M2AnimSummary, M2Light};

use crate::{model_key, yn};

/// Dump a WMO root's placed-prop tables: every MODD doodad with its MODS set membership and its
/// OWNING group(s) read from the group files' MODR lists — the relation the reference instantiates
/// from (`0x695aa0` loops a *visible* group's own refs, wow-re `m2-interior-doodad-base-light.md`
/// §453). A prop referenced by NO group is never created by the real client at all — the
/// divergence decision 0689 names and benilla still spawns. This answers "which props exist here,
/// who owns them, and which would the reference even draw" in one read (the B30/B32 question).
pub fn wmodoodads(chain: &mut Chain, raw_path: &str, filter: Option<&str>) -> Result<()> {
    let root_path = raw_path.replace('/', "\\").to_ascii_lowercase();
    let bytes = chain
        .read_file(&root_path)
        .with_context(|| format!("reading WMO root '{root_path}'"))?;
    let root = benilla_formats::parse_wmo_root(&bytes)
        .with_context(|| format!("parsing WMO root '{root_path}'"))?;
    let set_names = mods_set_names(&bytes);

    // MODD index -> the groups whose MODR reference it (the ownership relation).
    let stem = root_path.strip_suffix(".wmo").unwrap_or(&root_path);
    let mut owners: BTreeMap<u16, Vec<u32>> = BTreeMap::new();
    let mut groups_read = 0u32;
    for gi in 0..root.group_count() {
        let group_path = format!("{stem}_{gi:03}.wmo");
        let Ok(gbytes) = chain.read_file(&group_path) else {
            continue;
        };
        groups_read += 1;
        for r in benilla_formats::wmo_group_doodad_refs(&gbytes) {
            owners.entry(r).or_default().push(gi);
        }
    }

    println!(
        "{} doodad(s), {} set(s), {} group(s) ({} group file(s) read)",
        root.doodads().len(),
        root.doodad_sets().len(),
        root.group_count(),
        groups_read,
    );
    for (si, s) in root.doodad_sets().iter().enumerate() {
        let name = set_names.get(si).map(String::as_str).unwrap_or("?");
        println!(
            "set {si:>2}  [{:>5}..{:>5})  count {:>5}  {name}",
            s.start,
            s.start + s.count,
            s.count
        );
    }

    let needle = filter.map(str::to_ascii_lowercase);
    let infos = root.group_infos();
    let mut shown = 0u32;
    let mut orphans_total = 0u32;
    let mut orphans_shown = 0u32;
    for (i, d) in root.doodads().iter().enumerate() {
        let orphan = !owners.contains_key(&(i as u16));
        if orphan {
            orphans_total += 1;
        }
        if let Some(n) = &needle {
            if !model_key(&d.model).contains(n.as_str()) {
                continue;
            }
        }
        shown += 1;
        if orphan {
            orphans_shown += 1;
        }
        let sets: Vec<String> = root
            .doodad_sets()
            .iter()
            .enumerate()
            .filter(|(_, s)| (s.start..s.start + s.count).contains(&(i as u32)))
            .map(|(si, _)| si.to_string())
            .collect();
        let owner_cell = match owners.get(&(i as u16)) {
            Some(gs) => gs
                .iter()
                .map(|&g| {
                    let class = match infos.get(g as usize) {
                        Some(gi) if gi.interior => "INT",
                        Some(_) => "EXT",
                        None => "?",
                    };
                    format!("g{g}({class})")
                })
                .collect::<Vec<_>>()
                .join(" "),
            None => "ORPHAN".into(),
        };
        println!(
            "modd {i:>5}  pos ({:>8.2}, {:>8.2}, {:>8.2})  scale {:.3}  color #{:02x}{:02x}{:02x}{:02x}  sets [{}]  {owner_cell}  {}",
            d.position[0], d.position[1], d.position[2],
            d.scale,
            d.color[0], d.color[1], d.color[2], d.color[3],
            sets.join(","),
            d.model,
        );
    }
    match needle {
        Some(_) => eprintln!(
            "{shown} doodad(s) matched ({orphans_shown} ORPHAN); {orphans_total} orphan(s) among all {}",
            root.doodads().len()
        ),
        None => eprintln!(
            "{orphans_total} of {} doodad(s) are ORPHANS (in no group's MODR — the reference never instantiates these)",
            root.doodads().len()
        ),
    }
    Ok(())
}

/// The MODS set names (`char name[20]` per 32-byte record) — `WmoDoodadSet` keeps only the ranges,
/// so read the names off the raw root bytes here (top-level chunks are `[magic][size][data]`, magic
/// on disk reversed: MODS → `SDOM`).
fn mods_set_names(bytes: &[u8]) -> Vec<String> {
    let mut off = 0usize;
    while off + 8 <= bytes.len() {
        let size = u32::from_le_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]) as usize;
        let Some(data_end) = (off + 8).checked_add(size) else {
            return Vec::new();
        };
        if data_end > bytes.len() {
            return Vec::new();
        }
        if &bytes[off..off + 4] == b"SDOM" {
            return bytes[off + 8..data_end]
                .chunks_exact(32)
                .map(|rec| {
                    let name = &rec[..20];
                    let end = name.iter().position(|&b| b == 0).unwrap_or(20);
                    String::from_utf8_lossy(&name[..end]).into_owned()
                })
                .collect();
        }
        off = data_end;
    }
    Vec::new()
}

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

/// Sweep every `.m2` and census its **attachment addressing**: how many records it authors, how
/// many attach ids its AttachLookup resolves, and — the point of the report — where the two
/// disagree, i.e. where "scan the table for a record with this id" answers differently from the
/// reference's `lookup[id]` (`0x710310`).
///
/// Built for decision 0805 (item glows hang on the item model's ids 0..4) to answer the question
/// that decides whether the lookup can be adopted globally: which models change hands? One line
/// per divergent model — `+id` = an id only the lookup reaches, `-id` = an id only a table scan
/// reaches (a record the reference cannot address at all), `id:a→b` = same id, different record.
pub fn attachscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
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
    let (mut scanned, mut with_points, mut divergent) = (0u32, 0u32, 0u32);
    let mut by_family: BTreeMap<String, u32> = BTreeMap::new();
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        let Ok(format) = benilla_m2::parse_m2(&mut std::io::Cursor::new(&bytes)) else {
            continue;
        };
        let model = format.model();
        scanned += 1;
        if model.attachments.is_empty() && model.attach_lookup.is_empty() {
            continue;
        }
        with_points += 1;
        // What a table scan would answer (first record per id — the pre-0805 rule) against what
        // the lookup answers, compared by the record each id lands on.
        let mut scan: BTreeMap<u16, usize> = BTreeMap::new();
        for (i, a) in model.attachments.iter().enumerate() {
            scan.entry(a.id).or_insert(i);
        }
        let lookup: BTreeMap<u16, usize> = (0..model.attach_lookup.len())
            .filter_map(|id| {
                let idx = *model.attach_lookup.get(id)?;
                (idx != 0xffff && (idx as usize) < model.attachments.len())
                    .then_some((id as u16, idx as usize))
            })
            .collect();
        let mut diffs: Vec<String> = Vec::new();
        for (&id, &idx) in &lookup {
            match scan.get(&id) {
                Some(&s) if s != idx => diffs.push(format!("{id}:{s}→{idx}")),
                Some(_) => {}
                None => diffs.push(format!("+{id}")),
            }
        }
        for &id in scan.keys() {
            if !lookup.contains_key(&id) {
                diffs.push(format!("-{id}"));
            }
        }
        if !diffs.is_empty() {
            divergent += 1;
            *by_family.entry(model_key(&name)).or_default() += 1;
            println!(
                "{:<62} recs {:>2}  lookup {:>2}  {}",
                name,
                model.attachments.len(),
                lookup.len(),
                diffs.join(" ")
            );
        }
    }
    eprintln!(
        "{scanned} models scanned, {with_points} with attachment data, \
         {divergent} where a table scan disagrees with the lookup"
    );
    for (family, n) in by_family {
        eprintln!("  {family}: {n}");
    }
    Ok(())
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
/// descends from one — the joint-palette path, decision 0205), then the same question for the
/// model's **particle emitters and ribbons** (`fx[…]`) — the population behind decision 0813: an
/// emitter on (or under) a billboard bone has a camera-dependent origin, because the reference
/// folds the record position through the *replaced* palette matrix
/// (wow-re `part-anchoring-live-bone.md` §1 row 3 · `m2emitspine::particle_bone_xform`).
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
    // …and the effect riders (particles/ribbons on a billboard chain).
    let mut fx_models = 0u32;
    let mut fx_total: HashMap<String, u32> = HashMap::new();
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
        // The EFFECT riders: a particle emitter / ribbon whose bone chain reaches a billboard
        // bone has a camera-dependent live frame (its record position rides the replaced palette
        // matrix), so a consumer that places it at the rest pose puts it in the wrong place.
        // `d` = the effect's own bone is the billboard bone, `i` = it descends from one.
        let mut fx: HashMap<String, u32> = HashMap::new();
        let mut tally = |tag: &str, bone: u16| {
            if let Some((direct, k)) = ancestor_arm(bone as usize) {
                let key = format!("{tag}{}{}", if direct { "d" } else { "i" }, arm(k));
                *fx.entry(key).or_default() += 1;
            }
        };
        for e in benilla_formats::parse_m2_particle_emitters(&bytes)
            .unwrap_or_default()
            .iter()
        {
            tally("p", e.bone);
        }
        for r in benilla_formats::parse_m2_ribbon_emitters(&bytes)
            .unwrap_or_default()
            .iter()
        {
            tally("r", r.bone);
        }
        let fx_counts = {
            let mut v: Vec<String> = fx.iter().map(|(k, n)| format!("{k}:{n}")).collect();
            v.sort();
            v.join(" ")
        };
        let bones: String = kinds.iter().flatten().map(|&k| arm(k)).collect();
        println!(
            "{bones:>4}  direct[{}]  inherited[{}]  fx[{fx_counts}]  {name}",
            fmt_counts(&direct),
            fmt_counts(&inherited)
        );
        if !fx.is_empty() {
            fx_models += 1;
            for (k, n) in &fx {
                *fx_total.entry(k.clone()).or_default() += n;
            }
        }
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
    let fx_tot = {
        let mut v: Vec<String> = fx_total.iter().map(|(k, n)| format!("{k}:{n}")).collect();
        v.sort();
        v.join(" ")
    };
    eprintln!(
        "{scanned} models scanned, {hits} with billboard bones; models by arm — direct(card) [{}]  inherited(palette) [{}]; {fx_models} with EFFECTS on a billboard chain [{fx_tot}]",
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
            // ANISOTROPIC plane rectangles — the population on which the areaLength↔areaWidth axis
            // pairing is observable at all (a SQUARE area renders identically either way). 0563 and
            // 0566 both pre-named "a 90°-wrong ANISOTROPIC effect" as this lane's suspect, but no
            // instrument could LIST that population, so the swapped pairing outlived both audits
            // until Gressil's 0.1 × 1.1 blade smoke drew its curtain across the blade. Bucketed by
            // aspect so a thin curtain (load-bearing) separates from a near-square (invisible).
            if d.shape == benilla_formats::ParticleShape::Plane {
                let lo = d.area_length.abs().min(d.area_width.abs());
                let hi = d.area_length.abs().max(d.area_width.abs());
                if hi > 1e-4 {
                    let aspect = if lo > 1e-4 { hi / lo } else { f32::INFINITY };
                    if aspect >= 4.0 {
                        hit("plane-area:ANISOTROPIC >=4:1");
                    } else if aspect >= 1.5 {
                        hit("plane-area:anisotropic >=1.5:1");
                    }
                }
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

/// One bone `M2Track` read straight off the file bytes (v256 stride `0x1c`: interp@0, gseq@2,
/// interpolation_ranges `M2Array`@0x04/0x08, timestamps@0x0c/0x10, values@0x14/0x18).
///
/// Deliberately raw rather than via `parse_m2_animations`: this instrument's whole job is to
/// compare what the **file** says against what our parser currently emits, so it must not go
/// through the parser under test.
struct RawBoneTrack {
    interp: u16,
    gseq: u16,
    ranges: Vec<(u32, u32)>,
    ts: Vec<u32>,
    vals: Vec<[f32; 4]>,
    comps: usize,
}

impl RawBoneTrack {
    fn read(b: &[u8], o: usize, comps: usize) -> Option<Self> {
        let u32_at = |p: usize| -> Option<u32> {
            b.get(p..p + 4)
                .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        };
        let u16_at = |p: usize| -> Option<u16> {
            b.get(p..p + 2)
                .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
        };
        let f32_at = |p: usize| -> Option<f32> {
            b.get(p..p + 4)
                .map(|s| f32::from_le_bytes(s.try_into().unwrap()))
        };
        let (interp, gseq) = (u16_at(o)?, u16_at(o + 2)?);
        let (rn, ro) = (u32_at(o + 0x04)? as usize, u32_at(o + 0x08)? as usize);
        let (tn, to) = (u32_at(o + 0x0c)? as usize, u32_at(o + 0x10)? as usize);
        let (vn, vo) = (u32_at(o + 0x14)? as usize, u32_at(o + 0x18)? as usize);
        let ranges = (0..rn)
            .map_while(|i| Some((u32_at(ro + i * 8)?, u32_at(ro + i * 8 + 4)?)))
            .collect();
        let stride = comps * 4;
        let n = tn.min(vn);
        let mut ts = Vec::with_capacity(n);
        let mut vals = Vec::with_capacity(n);
        for i in 0..n {
            let Some(t) = u32_at(to + i * 4) else { break };
            let mut v = [0.0f32; 4];
            let mut ok = true;
            for (c, slot) in v.iter_mut().take(comps).enumerate() {
                match f32_at(vo + i * stride + c * 4) {
                    Some(f) => *slot = f,
                    None => ok = false,
                }
            }
            if !ok {
                break;
            }
            ts.push(t);
            vals.push(v);
        }
        Some(Self {
            interp,
            gseq,
            ranges,
            ts,
            vals,
            comps,
        })
    }

    /// The reference's sample at absolute time `t_ms` for sequence file slot `slot` — FN1
    /// (`0x713d50`) verbatim, then the sampler's own lerp/step leg (wow-re `eval.md` FN1/FN2/FN6).
    fn reference(&self, slot: usize, t_ms: u32) -> Option<[f32; 4]> {
        let last = self.ts.len().checked_sub(1)?;
        // FN1 §1: the window is `ranges[slot]` when the array is present, else the whole key list.
        let (lo, hi) = match self.ranges.get(slot) {
            Some(&(lo, hi)) => (lo as usize, hi as usize),
            None => (0, last),
        };
        let (lo, hi) = (lo.min(last), hi.min(last));
        // FN1 §2: a collapsed window resolves to `keys[lo]` outright.
        if lo >= hi {
            return Some(self.vals[lo]);
        }
        let mut k0 = lo;
        for (k, &ts) in self.ts.iter().enumerate().take(hi + 1).skip(lo) {
            if ts <= t_ms {
                k0 = k;
            } else {
                break;
            }
        }
        // FN1 §5: `k1 = k0+1`, bounded by the TOTAL key count — never by the window's `hi`.
        if self.interp == 0 || k0 + 1 > last {
            return Some(self.vals[k0]);
        }
        let (t0, t1) = (self.ts[k0], self.ts[k0 + 1]);
        if t1 <= t0 {
            return Some(self.vals[k0]);
        }
        let f = (t_ms as f32 - t0 as f32) / (t1 as f32 - t0 as f32);
        let (a, b) = (self.vals[k0], self.vals[k0 + 1]);
        let mut out = [0.0f32; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = a[i] + (b[i] - a[i]) * f;
        }
        Some(out)
    }

    /// What `models::anim::read_bone_track` emits for this band today: the in-band keys, or — when
    /// the band holds none — the single **nearest** out-of-band key (benilla decision 0133).
    /// Returns `(value_at_band_start, value_at_band_end, band_was_empty)`.
    fn benilla_today(&self, start: u32, end: u32) -> Option<([f32; 4], [f32; 4], bool)> {
        let inb: Vec<usize> = (0..self.ts.len())
            .filter(|&k| self.ts[k] >= start && self.ts[k] <= end)
            .collect();
        if let (Some(&f), Some(&l)) = (inb.first(), inb.last()) {
            // Bevy holds the first key before it and the last key after it.
            return Some((self.vals[f], self.vals[l], false));
        }
        let mut best: Option<(u32, usize)> = None;
        for (k, &ts) in self.ts.iter().enumerate() {
            let d = if ts < start { start - ts } else { ts - end };
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, k));
            }
        }
        let (_, k) = best?;
        Some((self.vals[k], self.vals[k], true))
    }

    /// Distance between two sampled values in the channel's own units: **degrees** of rotation for
    /// a quaternion (numerically stable near identity — `acos(dot)` has a ~0.04° floor in f32),
    /// **model units** for a translation/scale vector.
    fn delta(&self, a: [f32; 4], b: [f32; 4]) -> f32 {
        if self.comps == 4 {
            let norm = |q: [f32; 4]| {
                let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
                if n > 0.0 {
                    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
                } else {
                    q
                }
            };
            let (a, mut b) = (norm(a), norm(b));
            if a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() < 0.0 {
                b = [-b[0], -b[1], -b[2], -b[3]];
            }
            let d = a
                .iter()
                .zip(b)
                .map(|(x, y)| (x - y) * (x - y))
                .sum::<f32>()
                .sqrt();
            2.0 * (d / 2.0).min(1.0).asin().to_degrees()
        } else {
            (0..3)
                .map(|i| (a[i] - b[i]) * (a[i] - b[i]))
                .sum::<f32>()
                .sqrt()
        }
    }
}

/// Sweep every `.m2` (optionally under a path prefix) and measure, per bone track and per sequence
/// band, how far **our** skeletal parse sits from the **reference's** sampler — the population
/// instrument behind benilla decision 0133's named residual ("an empty band clamps to the nearest
/// authored key … a named approximation of the mid-gap lerp").
///
/// Three separately-reported disagreements, each a distinct mechanism (wow-re `eval.md`):
///
/// - **EMPTY bands** — a band with no keys of its own. We hold the nearest authored key; the
///   reference holds `keys[ranges[slot].lo]`, or lerps across the bracket when the window spans two
///   keys. The 0133 residual proper.
/// - **HELD edges** — a keyed band whose first key is late / last key is early. Bevy holds the edge
///   key; the reference keeps interpolating toward the neighbouring **out-of-band** key, because
///   FN1's `k1 = k0+1` is bounded by the total key count and not by the window.
/// - **STEP tracks** — `interpolation_type == 0`. The reference's samplers branch on it and copy
///   `keys[k0]` with no interpolation; our bone parse emits keys and lets Bevy interpolate, so a
///   snap becomes a glide.
///
/// Plus the safety check the whole idea rests on: whether any band's own keys fall **outside** the
/// window `ranges[slot]` would search (they never do — if they did, adopting the window would
/// reintroduce the garbage pose 0133 records).
pub fn bonescan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
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
    // Above the f32 noise floor of the stable angle formula (~1e-4°) by two orders, and far below
    // anything an eye could catch — a slot over this is a real authored difference.
    const ROT_EPS: f32 = 0.01; // degrees
    const VEC_EPS: f32 = 1e-4; // model units
    let (mut scanned, mut models_step, mut models_empty_differ, mut models_edge_differ) =
        (0u32, 0u32, 0u32, 0u32);
    let (mut slots_keyed, mut slots_empty) = (0u64, 0u64);
    let (mut empty_differ, mut edge_differ, mut ranges_absent, mut window_violation) =
        (0u64, 0u64, 0u64, 0u64);
    let (mut step_tracks, mut total_tracks, mut step_bands) = (0u64, 0u64, 0u64);
    let (mut gseq_tracks, mut gseq_multi, mut gseq_orphan) = (0u64, 0u64, 0u64);
    let (mut gseq_ranges_absent, mut gseq_ranges_restrict) = (0u64, 0u64);
    let mut gseq_orphan_models: BTreeSet<String> = BTreeSet::new();
    let mut gseq_restrict_models: BTreeSet<String> = BTreeSet::new();
    let mut emitted_extra = 0u64;
    // (empty-band peak, keyed-edge peak) per channel kind — the bound, not a threshold count.
    let (mut peak_rot, mut peak_vec) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let mut emitted_models: BTreeSet<String> = BTreeSet::new();
    // Worst offender per class: (delta, model, bone, channel, slot).
    let mut worst_empty: Vec<(f32, String, usize, &'static str, usize)> = Vec::new();
    let mut worst_edge: Vec<(f32, String, usize, &'static str, usize)> = Vec::new();
    let mut step_models: BTreeMap<String, u64> = BTreeMap::new();
    let mut worst_step: Vec<(f32, String, usize, &'static str, usize, usize)> = Vec::new();
    for name in names {
        let Ok(b) = chain.read_file(&name) else {
            continue;
        };
        if b.len() < 0x40 || &b[0..4] != b"MD20" {
            continue;
        }
        scanned += 1;
        let u32_at = |o: usize| -> usize {
            b.get(o..o + 4)
                .map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize)
                .unwrap_or(0)
        };
        // Sequences in FILE order (count@0x1c/ofs@0x20, stride 0x44) — the order `ranges` indexes.
        let (sn, so) = (u32_at(0x1c), u32_at(0x20));
        let seqs: Vec<(u32, u32)> = (0..sn)
            .map_while(|i| {
                let e = so + i * 0x44;
                (e + 0x44 <= b.len()).then(|| (u32_at(e + 4) as u32, u32_at(e + 8) as u32))
            })
            .collect();
        // globalSequences @0x14/0x18 — a duration per entry; 0 means the loop has no period.
        let (gn, go) = (u32_at(0x14), u32_at(0x18));
        let gseq_period = |g: u16| -> u32 {
            let i = g as usize;
            if i >= gn {
                0
            } else {
                u32_at(go + i * 4) as u32
            }
        };
        let (bn, bo) = (u32_at(0x34), u32_at(0x38));
        let (mut m_step, mut m_empty, mut m_edge) = (0u64, 0u64, 0u64);
        for bi in 0..bn {
            let brec = bo + bi * 0x6c;
            if brec + 0x6c > b.len() {
                break;
            }
            for (off, comps, ch) in [(0x0c, 3, "trans"), (0x28, 4, "rot"), (0x44, 3, "scale")] {
                let Some(tr) = RawBoneTrack::read(&b, brec + off, comps) else {
                    continue;
                };
                if tr.ts.is_empty() {
                    continue;
                }
                total_tracks += 1;
                if tr.interp == 0 {
                    step_tracks += 1;
                    m_step += 1;
                    // The step deviation only becomes visible when a band holds TWO keys: that is
                    // where the reference snaps and we glide. Measure the size of the snap.
                    for (slot, &(start, end)) in seqs.iter().enumerate() {
                        if end <= start {
                            continue;
                        }
                        let inb: Vec<usize> = (0..tr.ts.len())
                            .filter(|&k| tr.ts[k] >= start && tr.ts[k] <= end)
                            .collect();
                        if inb.len() < 2 {
                            continue;
                        }
                        step_bands += 1;
                        let jump = inb
                            .windows(2)
                            .map(|w| tr.delta(tr.vals[w[0]], tr.vals[w[1]]))
                            .fold(0.0f32, f32::max);
                        if jump > if comps == 4 { ROT_EPS } else { VEC_EPS } {
                            worst_step.push((jump, name.clone(), bi, ch, slot, inb.len()));
                        }
                    }
                }
                // A global-sequence track runs on its own clock. `read_bone_track` keeps the
                // SINGLE-key case (a constant channel — the stowed-weapon rest quats) and leaves
                // the multi-key case to `parse_m2_global_sequence_bones` → the `GlobalSeqDrive`
                // lane, which needs a non-zero `globalSequences[gseq]` period. A multi-key channel
                // on a ZERO-period global sequence falls between the two and is sampled by
                // neither — census that gap, and the shape of the `ranges` window the reference
                // would still apply here (FN1 selects the window BEFORE it resolves the gseq
                // clock, so a restrictive window would clip the loop).
                if tr.gseq != 0xffff {
                    gseq_tracks += 1;
                    if tr.ts.len() > 1 {
                        gseq_multi += 1;
                        if gseq_period(tr.gseq) == 0 {
                            gseq_orphan += 1;
                            gseq_orphan_models.insert(name.clone());
                        }
                        match tr.ranges.len() {
                            0 => gseq_ranges_absent += 1,
                            _ => {
                                if tr
                                    .ranges
                                    .iter()
                                    .any(|&(lo, hi)| lo != 0 || hi as usize != tr.ts.len() - 1)
                                {
                                    gseq_ranges_restrict += 1;
                                    gseq_restrict_models.insert(name.clone());
                                }
                            }
                        }
                    }
                    continue;
                }
                let eps = if comps == 4 { ROT_EPS } else { VEC_EPS };
                for (slot, &(start, end)) in seqs.iter().enumerate() {
                    if end <= start {
                        continue;
                    }
                    if tr.ranges.get(slot).is_none() {
                        ranges_absent += 1;
                    }
                    let Some((mine_a, mine_b, was_empty)) = tr.benilla_today(start, end) else {
                        continue;
                    };
                    let (Some(ref_a), Some(ref_b)) =
                        (tr.reference(slot, start), tr.reference(slot, end))
                    else {
                        continue;
                    };
                    let d = tr.delta(mine_a, ref_a).max(tr.delta(mine_b, ref_b));
                    let peak = if comps == 4 {
                        &mut peak_rot
                    } else {
                        &mut peak_vec
                    };
                    if was_empty {
                        peak.0 = peak.0.max(d);
                        slots_empty += 1;
                        if d > eps {
                            empty_differ += 1;
                            m_empty += 1;
                            worst_empty.push((d, name.clone(), bi, ch, slot));
                        }
                    } else {
                        peak.1 = peak.1.max(d);
                        slots_keyed += 1;
                        // Does this band's own key set sit inside the window the reference
                        // searches? If not, adopting the window would drop playable keys.
                        if let Some(&(lo, hi)) = tr.ranges.get(slot) {
                            let inb: Vec<usize> = (0..tr.ts.len())
                                .filter(|&k| tr.ts[k] >= start && tr.ts[k] <= end)
                                .collect();
                            if inb.first().is_some_and(|&f| (f as u32) < lo)
                                || inb.last().is_some_and(|&l| (l as u32) > hi)
                            {
                                window_violation += 1;
                            }
                        }
                        if d > eps {
                            edge_differ += 1;
                            m_edge += 1;
                            worst_edge.push((d, name.clone(), bi, ch, slot));
                        }
                    }
                }
            }
        }
        // The other half of the check: what our parser ACTUALLY emits. A band-slot whose emitted
        // key count exceeds its own in-band key count is one where the head/tail sample differed
        // from the edge key and had to be carried — the only slots this parse changes.
        for a in benilla_formats::parse_m2_animations(&b) {
            for bk in &a.bones {
                for (off, comps, emitted) in [
                    (0x0c, 3, bk.translation.len()),
                    (0x28, 4, bk.rotation.len()),
                    (0x44, 3, bk.scale.len()),
                ] {
                    let brec = bo + bk.bone as usize * 0x6c;
                    let Some(tr) = RawBoneTrack::read(&b, brec + off, comps) else {
                        continue;
                    };
                    if tr.gseq != 0xffff {
                        continue;
                    }
                    let inb = tr
                        .ts
                        .iter()
                        .filter(|&&ts| ts >= a.start_ms && ts <= a.end_ms)
                        .count();
                    if emitted > inb.max(1) {
                        emitted_extra += 1;
                        emitted_models.insert(name.clone());
                    }
                }
            }
        }
        if m_step > 0 {
            models_step += 1;
            step_models.insert(name.clone(), m_step);
        }
        if m_empty > 0 {
            models_empty_differ += 1;
        }
        if m_edge > 0 {
            models_edge_differ += 1;
        }
        // Keep the worst-offender lists bounded without losing the tail.
        for w in [&mut worst_empty, &mut worst_edge] {
            if w.len() > 4096 {
                w.sort_by(|a, b| b.0.total_cmp(&a.0));
                w.truncate(512);
            }
        }
        if worst_step.len() > 4096 {
            worst_step.sort_by(|a, b| b.0.total_cmp(&a.0));
            worst_step.truncate(512);
        }
    }
    worst_empty.sort_by(|a, b| b.0.total_cmp(&a.0));
    worst_edge.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("scanned {scanned} models · {total_tracks} keyed bone tracks");
    println!("\nbone×channel×sequence slots: {slots_keyed} keyed, {slots_empty} EMPTY");
    println!("  ranges array absent for the slot: {ranges_absent}");
    println!("  band keys OUTSIDE their own ranges window: {window_violation}  (must be 0 to adopt the window)");
    for (label, n, models, worst) in [
        (
            "EMPTY band  (nearest-key vs the file's window)",
            empty_differ,
            models_empty_differ,
            &worst_empty,
        ),
        (
            "HELD edge   (hold vs the reference's ongoing lerp)",
            edge_differ,
            models_edge_differ,
            &worst_edge,
        ),
    ] {
        println!("\n{label}: {n} slots differ, across {models} models");
        for (d, m, bi, ch, slot) in worst.iter().take(15) {
            let unit = if *ch == "rot" { "deg" } else { "u" };
            println!("  {d:9.3}{unit:<4} {m:<52} bone {bi:<4} {ch:<6} slot {slot}");
        }
    }
    worst_step.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!(
        "\nSTEP bone tracks (interp == 0, the reference copies keys[k0]): {step_tracks} of \
         {total_tracks}, in {models_step} models"
    );
    println!(
        "  bands where a step track holds 2+ keys (we glide, the reference snaps): {step_bands}; \
         {} of them snap by more than the noise floor. Worst:",
        worst_step.len()
    );
    for (d, m, bi, ch, slot, n) in worst_step.iter().take(20) {
        let unit = if *ch == "rot" { "deg" } else { "u" };
        println!("  {d:9.3}{unit:<4} {m:<52} bone {bi:<4} {ch:<6} slot {slot:<4} ({n} keys)");
    }
    println!(
        "\nGLOBAL-SEQUENCE bone tracks: {gseq_tracks} total, {gseq_multi} multi-key (the \
         `GlobalSeqDrive` lane's input; single-key ones are constants folded into every clip)"
    );
    println!(
        "  multi-key on a ZERO-period global sequence — sampled by NEITHER lane: {gseq_orphan}, \
         in {} models",
        gseq_orphan_models.len()
    );
    for m in gseq_orphan_models.iter().take(10) {
        println!("      {m}");
    }
    println!(
        "  their `ranges` window: absent (whole key list) {gseq_ranges_absent}, RESTRICTIVE \
         (not [0, last] — would clip the loop) {gseq_ranges_restrict} in {} models",
        gseq_restrict_models.len()
    );
    for m in gseq_restrict_models.iter().take(10) {
        println!("      {m}");
    }
    println!(
        "\nworst disagreement anywhere in the corpus (a BOUND, not a threshold count):\n  \
         rotation: empty band {:.5} deg, keyed edge {:.5} deg\n  \
         translation/scale: empty band {:.6} u, keyed edge {:.6} u",
        peak_rot.0, peak_rot.1, peak_vec.0, peak_vec.1
    );
    println!(
        "\nEMITTED clips: {emitted_extra} band-slots carry a head/tail sample beyond their own \
         in-band keys, in {} models",
        emitted_models.len()
    );
    for m in emitted_models.iter().take(10) {
        println!("      {m}");
    }
    println!("  models with any step track:");
    for (m, n) in step_models.iter().take(10) {
        println!("  {n:>6}  {m}");
    }
    if step_models.len() > 10 {
        println!("  … and {} more models", step_models.len() - 10);
    }
    Ok(())
}

/// The GameObject animation arm's LUT (wow-re `gameobject-anim-arm.md` §2c, `.data 0x8607e4`):
/// internal **substate** → the `AnimationData.dbc` id the object layer arms.
const SUBSTATE_ANIM: [u16; 13] = [
    145, // 0  Spawn      — NO client path produces this substate (§2c census)
    147, // 1  Closed     (rest)
    148, // 2  Open       (motion)
    149, // 3  Opened     (rest)
    146, // 4  Close      (motion)
    150, // 5  Destroy    (motion)
    151, // 6  Destroyed  (rest)
    152, // 7  Rebuild    (motion)
    153, 154, 155, 156, // 8..11 Custom0-3 — reachable only via SMSG_GAMEOBJECT_CUSTOM_ANIM
    157, // 12 Despawn
];

/// The six substates a `GAMEOBJECT_STATE` × `GAMEOBJECT_ANIMPROGRESS` pair can actually produce
/// (§2b). Substate 0 (Spawn) has no producer at all, and 8..12 come from other opcodes entirely.
const REACHABLE: [(usize, &str); 6] = [
    (1, "READY  settled"),
    (4, "READY  mid    "),
    (3, "ACTIVE settled"),
    (2, "ACTIVE mid    "),
    (6, "ALT    settled"),
    (5, "ALT    mid    "),
];

/// The §2c four-way remap: what the arm actually requests when the model doesn't author the
/// substate's LUT id. Returns `(id, rate0)` — `rate0` marks the two legs that freeze a *motion*
/// clip at frame 0 to stand in for a missing *rest* pose.
fn go_remap(m: &benilla_m2::M2Model, id: u16) -> (u16, bool) {
    if m.owns_animation(id) {
        return (id, false);
    }
    match id {
        // Close missing: keep it (op4 resolves onward) if Open exists, else fall to Closed.
        146 => (if m.owns_animation(148) { 146 } else { 147 }, false),
        // Closed missing: keep it if Close exists; else freeze Open at frame 0; else Stand.
        147 if m.owns_animation(146) => (147, false),
        147 if m.owns_animation(148) => (148, true),
        147 => (0, false),
        // Open missing: keep it if Close exists; else Destroy if present; else Opened.
        148 => (
            if m.owns_animation(146) {
                148
            } else if m.owns_animation(150) {
                150
            } else {
                149
            },
            false,
        ),
        // Opened missing: keep it if Open exists; else freeze Close at frame 0; else Destroyed.
        149 if m.owns_animation(148) => (149, false),
        149 if m.owns_animation(146) => (146, true),
        149 => (151, false),
        // Outside the door group there is no remap — the id goes to op4 as-is.
        other => (other, false),
    }
}

/// op4's own id → played sequence resolve (`0x7121a0` via `0x711bf0`): the model's
/// `playableAnimationLookup` row (when the id is in range), then `animationLookup` to a file slot.
/// `None` when nothing playable comes out — the reference arms nothing and the pose simply stands.
fn go_resolve_slot(m: &benilla_m2::M2Model, id: u16) -> Option<(u16, u16)> {
    let played = m
        .playable_animation_lookup
        .get(id as usize)
        .map_or(id, |p| p.resolved_id);
    let slot = *m.animation_lookup.get(played as usize)?;
    (slot != 0xffff).then_some((played, slot))
}

/// The generic loader seed (§1, `0x71019b`): resolve id 0, and arm **id 0** when the model owns what
/// that resolves to — only the degenerate leg (owning nothing reachable) falls back to the raw
/// `animations[0]` dword.
fn go_loader_seed(
    m: &benilla_m2::M2Model,
    seqs: &[benilla_formats::ModelAnimation],
) -> Option<(u16, u16)> {
    let resolved = m
        .playable_animation_lookup
        .first()
        .map_or(0, |p| p.resolved_id);
    if m.owns_animation(resolved) {
        go_resolve_slot(m, 0)
    } else {
        // The degenerate leg: `animations[0]`'s low16 — the file-order-first sequence's own id.
        go_resolve_slot(m, seqs.first()?.anim_id)
    }
}

/// Sweep every model named by **GameObjectDisplayInfo.dbc** and resolve, per model, what the
/// reference's GameObject animation arm plays in each reachable `GAMEOBJECT_STATE` ×
/// `GAMEOBJECT_ANIMPROGRESS` substate — see the `Goanimscan` command doc.
pub fn goanimscan(chain: &mut Chain) -> Result<()> {
    let catalog =
        benilla_formats::load_gameobject_catalog(chain).context("GameObjectDisplayInfo.dbc")?;
    // displayId → path, deduped to one entry per model (many displays share a model).
    let mut models: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (id, path) in catalog.iter() {
        let key = model_key(path);
        if key.ends_with(".m2") {
            models.entry(key).or_default().push(id);
        }
    }
    for ids in models.values_mut() {
        ids.sort_unstable();
    }
    let (mut parsed, mut no_seq, mut blind, mut sensitive, mut needs_remap, mut rate0) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    for (path, displays) in &models {
        let Ok(bytes) = chain.read_file(path) else {
            continue;
        };
        let Ok(fmt) = benilla_m2::parse_m2(&mut std::io::Cursor::new(&bytes)) else {
            continue;
        };
        let m = fmt.model();
        parsed += 1;
        let seqs = benilla_formats::parse_m2_animations(&bytes);
        if seqs.is_empty() {
            no_seq += 1;
            continue;
        }
        let seed = go_loader_seed(m, &seqs);
        let mut lines = Vec::new();
        let (mut differs, mut remapped, mut froze) = (false, false, false);
        for (sub, label) in REACHABLE {
            let lut = SUBSTATE_ANIM[sub];
            let (req, r0) = go_remap(m, lut);
            let armed = go_resolve_slot(m, req);
            remapped |= req != lut;
            froze |= r0;
            differs |= armed.map(|(_, s)| s) != seed.map(|(_, s)| s);
            lines.push(format!(
                "   {label} sub{sub}  lut {lut}{}  ->  {}{}",
                if req == lut {
                    String::new()
                } else {
                    format!(" (remap {req})")
                },
                match armed {
                    Some((id, slot)) => format!("id {id} slot {slot}"),
                    None => "NOTHING".to_string(),
                },
                if r0 { "  [rate 0 — frozen]" } else { "" },
            ));
        }
        if differs {
            sensitive += 1;
        } else {
            blind += 1;
        }
        remapped.then(|| needs_remap += 1);
        froze.then(|| rate0 += 1);
        // Only the state-SENSITIVE models are worth printing: on every other one the arm lands on
        // the same sequence the loader seed already holds, so `GAMEOBJECT_STATE` is unobservable.
        if differs {
            println!("{path}  ({} sequences, displays {displays:?})", seqs.len());
            println!(
                "   loader seed              ->  {}",
                match seed {
                    Some((id, slot)) => format!("id {id} slot {slot}"),
                    None => "NOTHING".to_string(),
                }
            );
            for l in lines {
                println!("{l}");
            }
        }
    }
    println!(
        "\n{} GameObjectDisplayInfo M2 models, {parsed} parsed, {no_seq} with no sequences",
        models.len()
    );
    println!(
        "  STATE-BLIND    {blind}  — every reachable substate lands on the loader seed's own \
         sequence, so GAMEOBJECT_STATE cannot be seen on this model at all"
    );
    println!(
        "  STATE-SENSITIVE {sensitive}  — at least one substate plays something else: exactly the \
         models a GO type that skips the arm renders in the wrong pose"
    );
    println!("  needing the §2c remap on some substate: {needs_remap}");
    println!("  hitting a rate-0 freeze leg (a motion clip standing in for a missing rest pose): {rate0}");
    Ok(())
}

/// Sweep every `.m2` and census its particle emitters' over-life **flipbook** fields — the
/// population instrument behind decision 0685 (the reverse-playing cell ramp).
///
/// One line per emitter that is interesting on any axis, then the totals. The axes are exactly the
/// ways a flipbook reader can be wrong, each of which shipped data does exercise:
///
/// - `INVERTED` — a `begin > end` pair. Legal, and it means *play the sheet backwards*; a reader
///   that clamps into `[begin, end]` mangles it (and in Rust panics outright).
/// - `TAIL-RAMP` — the tail streak's own ramp differs from the head's, on an emitter that draws a
///   tail. The two are independently authored (file +0x168.. vs +0x174..); handing the head's cell
///   to the streak animates it through a sheet the author pinned to one cell.
/// - `PAST-ATLAS` — a cell index beyond `rows·cols`. The reference masks the COLUMN and leaves the
///   ROW unbounded, so the index wraps to row 0 rather than holding the last cell.
/// - `REPEAT` — a per-segment repeat count ≠ 1, i.e. the sheet cycles more than once per segment.
/// - `NON-POW2` / `MID` — the two shapes the reference itself degrades on (a 1×1 fallback, and a
///   `mid` of 0/1 that walks its own sampler into a NaN). Both are empty in 1.12.1 and are here so
///   that stays checkable.
pub fn cellscan(chain: &mut Chain) -> Result<()> {
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.to_ascii_lowercase().ends_with(".m2"))
        .collect();

    let (mut models, mut emitters) = (0u32, 0u32);
    let (mut inverted, mut tail_ramp, mut repeat_ne1) = (0u32, 0u32, 0u32);
    let (mut past_atlas, mut past_atlas_real, mut bad_tiles, mut bad_mid) =
        (0u32, 0u32, 0u32, 0u32);

    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        models += 1;
        let Ok(defs) = benilla_formats::parse_m2_particle_emitters(&bytes) else {
            continue;
        };
        for (i, e) in defs.iter().enumerate() {
            emitters += 1;
            let ol = &e.over_life;
            let at = format!("{name} [{i}] {}x{}", e.tile_rows, e.tile_cols);
            let ramps = [
                ol.head_cells[0],
                ol.head_cells[1],
                ol.tail_cells[0],
                ol.tail_cells[1],
            ];
            let pair = |r: &benilla_formats::CellRamp| (r.begin, r.end);

            if ramps.iter().any(|r| r.begin > r.end) {
                inverted += 1;
                println!(
                    "INVERTED   {at} head {:?}/{:?}",
                    pair(&ol.head_cells[0]),
                    pair(&ol.head_cells[1])
                );
            }
            // Only a tail-drawing emitter (particleType 1/2) can show a tail-ramp difference.
            if e.head_tail >= 1 && ol.tail_cells != ol.head_cells {
                tail_ramp += 1;
                println!(
                    "TAIL-RAMP  {at} head {:?}/{:?} tail {:?}/{:?}",
                    pair(&ol.head_cells[0]),
                    pair(&ol.head_cells[1]),
                    pair(&ol.tail_cells[0]),
                    pair(&ol.tail_cells[1])
                );
            }
            if ol.repeat.iter().any(|&r| r != 1.0) {
                repeat_ne1 += 1;
                println!("REPEAT     {at} {:?}", ol.repeat);
            }
            let atlas = e.tile_rows * e.tile_cols;
            if ramps.iter().any(|r| r.begin >= atlas || r.end >= atlas) {
                past_atlas += 1;
                // On a 1×1 sheet every index resolves to the same texture, so only a real atlas
                // can show the wrap.
                if atlas > 1 {
                    past_atlas_real += 1;
                    println!(
                        "PAST-ATLAS {at} head {:?}/{:?}",
                        pair(&ol.head_cells[0]),
                        pair(&ol.head_cells[1])
                    );
                }
            }
            if !e.tile_rows.is_power_of_two() || !e.tile_cols.is_power_of_two() {
                bad_tiles += 1;
                println!("NON-POW2   {at}");
            }
            if !(ol.mid > 0.0 && ol.mid < 1.0) {
                bad_mid += 1;
                println!("MID        {at} mid {}", ol.mid);
            }
        }
    }

    eprintln!(
        "{models} models / {emitters} emitters scanned\n  \
         INVERTED    {inverted}\n  \
         TAIL-RAMP   {tail_ramp}\n  \
         REPEAT      {repeat_ne1}\n  \
         PAST-ATLAS  {past_atlas} ({past_atlas_real} on a real >1x1 atlas)\n  \
         NON-POW2    {bad_tiles}\n  \
         MID 0 or 1  {bad_mid}"
    );
    Ok(())
}

/// Sweep every `.m2` (optionally under a path prefix) and count, per model, the two halves of the
/// **owner-last draw-order** law: the EFFECTS a model authors (particle emitters + ribbon trails)
/// and the TRANSPARENT-pass batches of its own body those effects must draw after (decisions
/// 0719/0721). A model with both is one the rung actually changes; a model with effects and no
/// transparent batch of its own never had the defect and is listed only in the totals.
///
/// This is the population instrument the two decisions were argued from. "Does this fix anything
/// besides the voidwalker's eyes?" is not a question to answer by naming plausible creatures —
/// it is a count, and the count is what says whether the mechanism closes a class or a case.
pub fn fxordercensus(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
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
    // Per top-level content family (Creature / Item / Spells / World / …), so the totals say
    // WHERE the class lives rather than only how big it is.
    let mut family: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let (mut scanned, mut with_fx, mut at_risk) = (0u32, 0u32, 0u32);
    let mut rungs: BTreeMap<u32, u32> = BTreeMap::new();
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let emitters = benilla_formats::parse_m2_particle_emitters(&bytes)
            .map(|e| e.len())
            .unwrap_or(0);
        let trails = benilla_formats::m2_ribbon_emitter_count(&bytes);
        if emitters + trails == 0 {
            continue;
        }
        with_fx += 1;
        // The occluders: batches the renderer puts in the one distance-sorted transparent list.
        // `additive` forces that pass whatever the authored blend says (`model_material`), so the
        // test mirrors the renderer's rather than reading the blend word alone.
        let dir = name.rsplit_once('\\').map_or("", |(d, _)| d);
        let subs = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]).unwrap_or_default();
        let occluders: Vec<&benilla_formats::RenderSubmesh> = subs
            .iter()
            .filter(|s| {
                s.additive
                    || matches!(
                        s.blend,
                        benilla_formats::ModelBlend::Blend
                            | benilla_formats::ModelBlend::Mod
                            | benilla_formats::ModelBlend::Mod2x
                    )
            })
            .collect();
        let transparent = occluders.len();
        // The renderer's own bound and the renderer's own rung — not a re-derivation of them.
        let reach = benilla_formats::m2_owner_reach(&subs);
        let fam = name.split('\\').next().unwrap_or("?").to_string();
        family.entry(fam).or_default().0 += 1;
        if transparent == 0 {
            continue; // effects, but nothing of its own that could paint over them
        }
        at_risk += 1;
        family
            .entry(name.split('\\').next().unwrap_or("?").to_string())
            .or_default()
            .1 += 1;
        // At placement scale 1 — the survey number; a scaled placement multiplies the reach.
        let rung = benilla_formats::owner_last_rung(reach);
        *rungs.entry(rung as u32).or_default() += 1;
        println!(
            "{transparent:>3} transp {emitters:>2} emit {trails:>2} ribb  reach {reach:>8.2} \
             rung {rung:>2.0}  {name}"
        );
    }
    eprintln!(
        "{scanned} models scanned\n  \
         {with_fx} author effects (emitters and/or ribbons)\n  \
         {at_risk} of those ALSO author transparent batches of their own — the population the \
         owner-last rung changes"
    );
    eprintln!("  by family (with-effects / at-risk):");
    for (fam, (n, risk)) in &family {
        eprintln!("    {fam:<24} {n:>5} / {risk:>5}");
    }
    eprintln!("  rung distribution (at-risk models, placement scale 1):");
    for (rung, n) in &rungs {
        eprintln!("    rung {rung:>2}  {n:>5}");
    }
    Ok(())
}

/// The terrain MCSH shadow bit at a world position + an ASCII texel neighborhood (`#` shadowed,
/// `.` lit, `?` off-tile/no-chunk). One MCSH texel is `TILE_SIZE/1024` ≈ 0.52 yd; the grid spans
/// ±8 texels so a doodad base sitting one texel from a shadow edge — the 2.5-vs-0.5 intensity
/// cliff — is visible at a glance.
pub fn shadeat(chain: &mut Chain, map: &str, x: f32, y: f32) -> Result<()> {
    let tiles = benilla_formats::load_tiles_around(chain, map, x, y, 0)
        .with_context(|| format!("loading the tile under ({x}, {y}) on {map}"))?;
    let Some((_, tile)) = tiles.first() else {
        anyhow::bail!("no tile exists under ({x}, {y}) on {map}");
    };
    let texel = benilla_formats::TILE_SIZE / 1024.0;
    let word = |s: Option<bool>| match s {
        Some(true) => "SHADOWED (doodad sun intensity 0.5)",
        Some(false) => "lit (doodad sun intensity 2.5)",
        None => "off-tile / no chunk",
    };
    println!(
        "MCSH at ({x:.2}, {y:.2}): {}",
        word(benilla_formats::mcsh_shadowed_at(&tile.chunks, [x, y, 0.0]))
    );
    println!(
        "neighborhood, texel {texel:.3} yd — rows +X (north) up, cols +Y (west) left; center marked:"
    );
    for dx in (-8i32..=8).rev() {
        let mut row = String::new();
        for dy in (-8i32..=8).rev() {
            let p = [x + dx as f32 * texel, y + dy as f32 * texel, 0.0];
            let mut c = match benilla_formats::mcsh_shadowed_at(&tile.chunks, p) {
                Some(true) => '#',
                Some(false) => '.',
                None => '?',
            };
            if dx == 0 && dy == 0 {
                c = if c == '#' { 'S' } else { 'O' };
            }
            row.push(c);
        }
        println!("{row}");
    }
    Ok(())
}

/// Sweep every `.m2` (optionally under a path prefix) and list the emitters whose **file slot 0 is
/// dead while another slot is alive** — the shape that makes a pinned-slot-0 consumer silently
/// render nothing (decision 0760, found on `BlastedLandsLightningbolt01.m2`, B63).
///
/// The reference samples the **playing** sequence's rate window every frame (wow-re
/// `part-emission-rate-animated.md` §2); a consumer that pins slot 0 instead is only correct while
/// slot 0 carries the emitter's whole story. When an author keys the burst in a *later* variation —
/// a lightning strike that fires on 5 % of arms, an ambient prop with a rare flourish — slot 0 is a
/// flat zero and the pinned consumer emits nothing, for ever, on every placement. That is invisible
/// from the outside: the emitter is built, pooled, and ticking; it just never births a particle.
///
/// `peak0` is slot 0's own peak rate, `peakN` the best any other slot reaches. A listed emitter has
/// `peak0 <= 0 < peakN`. `slots` counts FILE sequence slots (the axis `EmitTiming` bakes on).
pub fn partslotscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
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
    let (mut scanned, mut with_emitters, mut emitters, mut dead0) = (0u32, 0u32, 0u32, 0u32);
    let mut models = 0u32;
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
        let mut lines = Vec::new();
        for (i, d) in defs.iter().enumerate() {
            emitters += 1;
            let views = d.timing.slot_views();
            if views.len() < 2 {
                continue; // one slot ⇒ nothing for a slot pick to get wrong
            }
            // Peak of a slot's baked rate keys; an unkeyed slot emits nothing (`None` ⇒ rate 0).
            let peak = |s: usize| -> f32 {
                views
                    .get(s)
                    .and_then(|v| v.1)
                    .map(|keys| keys.iter().map(|&(_, v)| v).fold(0.0, f32::max))
                    .unwrap_or(0.0)
            };
            let peak0 = peak(0);
            let (mut best, mut best_slot) = (0.0f32, 0usize);
            for s in 1..views.len() {
                if peak(s) > best {
                    best = peak(s);
                    best_slot = s;
                }
            }
            if peak0 <= 0.0 && best > 0.0 {
                dead0 += 1;
                lines.push(format!(
                    "    emitter {i:>2}: slots {:>2}  peak0 {peak0:>7.1}  peakN {best:>7.1} \
                     @ slot {best_slot}  tex {}",
                    views.len(),
                    d.texture.as_deref().unwrap_or("NONE"),
                ));
            }
        }
        if !lines.is_empty() {
            models += 1;
            println!("{name}");
            for l in lines {
                println!("{l}");
            }
        }
    }
    eprintln!(
        "{scanned} models scanned, {with_emitters} with emitters, {emitters} emitter(s): \
         {dead0} DEAD-IN-SLOT-0 across {models} model(s) — these emit nothing at all under a \
         pinned-slot-0 consumer"
    );
    Ok(())
}

/// Sweep every `.m2` (optionally under a path prefix) and list the batches whose texture is
/// authored **CLAMP** (`M2Texture.flags` bit 0/1 clear) while the batch's own UVs run **outside
/// `0..1`** — the exact population a repeat-sampling renderer draws wrong (decision 0763, B52/B96).
///
/// The margin outside `0..1` is deliberate authoring: clamped, it samples the texture's transparent
/// border and the card fades out to nothing. Sampled with repeat it wraps into the opposite edge —
/// on a cutout sheet, the opaque middle — so the margin draws as solid geometry with a hard seam
/// where u or v crosses the wrap. That is why a snow-fir grows pale plates with a crease down each
/// bough, and why the artefact never looked like an extra primitive: it is the *same* card,
/// sampling the wrong texels.
///
/// `over` is how far past the edge the batch reaches, in UV units — the width of the wrongly-drawn
/// margin as a fraction of the sheet.
pub fn uvwrapscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
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
    let (mut scanned, mut batches, mut hits, mut models) = (0u32, 0u32, 0u32, 0u32);
    let mut cutout_hits = 0u32;
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
            if s.uvs.is_empty() {
                continue;
            }
            batches += 1;
            let ext = |axis: usize| {
                s.uvs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), t| {
                    (lo.min(t[axis]), hi.max(t[axis]))
                })
            };
            let (u, v) = (ext(0), ext(1));
            // Only an axis authored CLAMP can be drawn wrong by repeat; a wrapping axis is meant
            // to tile. A hair of float slop past the edge is not a margin — require 1/512 of a
            // sheet, well under the thinnest authored border and well over rounding.
            const SLOP: f32 = 1.0 / 512.0;
            let bad_u = !s.wrap_x && (u.0 < -SLOP || u.1 > 1.0 + SLOP);
            let bad_v = !s.wrap_y && (v.0 < -SLOP || v.1 > 1.0 + SLOP);
            if !bad_u && !bad_v {
                continue;
            }
            hits += 1;
            let cutout = matches!(
                s.blend,
                benilla_formats::ModelBlend::AlphaTest | benilla_formats::ModelBlend::Blend
            );
            if cutout {
                cutout_hits += 1;
            }
            let over = [
                (-u.0).max(0.0),
                (u.1 - 1.0).max(0.0),
                (-v.0).max(0.0),
                (v.1 - 1.0).max(0.0),
            ]
            .into_iter()
            .fold(0.0f32, f32::max);
            lines.push(format!(
                "    batch {i:>3}: {:?} {} verts  u[{:+.3}..{:+.3}] v[{:+.3}..{:+.3}]  \
                 over {over:.3}  {}{}  tex {}",
                s.blend,
                s.positions.len(),
                u.0,
                u.1,
                v.0,
                v.1,
                if bad_u { "U" } else { "-" },
                if bad_v { "V" } else { "-" },
                s.texture.as_deref().unwrap_or("NONE"),
            ));
        }
        if !lines.is_empty() {
            models += 1;
            println!("{name}");
            for l in lines {
                println!("{l}");
            }
        }
    }
    eprintln!(
        "{scanned} models scanned, {batches} textured batch(es): {hits} CLAMP-AUTHORED BATCHES \
         SAMPLING OUTSIDE 0..1 across {models} model(s) — {cutout_hits} of them cutout/blend, \
         where wrapping changes the silhouette rather than just the colour"
    );
    Ok(())
}

/// Sweep every `.m2` and report, per texture path, which sampler ADDRESS MODES the corpus asks of
/// it — and how many paths are asked for **more than one** (decision 0763).
///
/// The design question behind it: the address mode lives on the GPU sampler, which in our asset
/// layer is a property of the loaded `Image`, which is keyed by path. If a `.blp` is only ever
/// asked for one mode, path-keying stays correct and the mode can simply ride the load. Every path
/// asked for two needs two uploads, or one of its users renders wrong.
pub fn texmodescan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
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
    // texture path -> set of (wrap_x, wrap_y) asked for, as a 4-bit mask
    let mut modes: std::collections::BTreeMap<String, u8> = std::collections::BTreeMap::new();
    let mut scanned = 0u32;
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        for s in &subs {
            let Some(tex) = s.texture.as_deref() else {
                continue;
            };
            let bit = 1u8 << ((s.wrap_x as u8) | ((s.wrap_y as u8) << 1));
            *modes.entry(tex.to_ascii_lowercase()).or_default() |= bit;
        }
    }
    let mut conflicted = 0u32;
    for (path, mask) in &modes {
        if mask.count_ones() > 1 {
            conflicted += 1;
            let want = |b: u8, s: &'static str| if mask & (1 << b) != 0 { s } else { "" };
            println!(
                "CONFLICT {path}  asked as: {}{}{}{}",
                want(0, "[clamp,clamp] "),
                want(1, "[repeat,clamp] "),
                want(2, "[clamp,repeat] "),
                want(3, "[repeat,repeat] "),
            );
        }
    }
    eprintln!(
        "{scanned} models scanned, {} distinct texture path(s): {conflicted} asked for MORE THAN \
         ONE address mode (each needs its own upload, or one of its users renders wrong)",
        modes.len()
    );
    Ok(())
}

/// Sweep every WMO **root** in the chain and report the two halves of the skybox mechanism: which
/// roots author a **MOSB** skybox model, and which carry groups flagged `0x40000`
/// ([`benilla_formats::WmoGroupInfo::show_skybox`]).
///
/// This is the instrument that *identifies* the flag — and **exactly how far that identification
/// reaches is the point**. `0x40000` is undocumented, so the cross-tab is what establishes it means
/// anything at all: the bit never appears on a group whose root names no skybox, across all 815
/// roots. That is a one-way implication, `flag ⇒ MOSB`, and the summary prints it so the claim is
/// re-checkable in one command rather than trusted from a decision record.
///
/// **It does not, and cannot, say which group the RENDERER tests** — and reading it as if it did is
/// the mistake decision 0767 made (superseded by 0773). The carved law is that `0x40000` is tested
/// inside the portal flood (`0x6b42e0` in `0x6b41c0`) on the group being *visited*, so the predicate
/// is "any flood-reached group carries the bit". A census over static asset bytes has no way to see
/// that distinction; only the binary did.
///
/// It is also the population instrument for the mechanism: which buildings in 1.12 replace the
/// `Light.dbc` gradient dome with an authored sky, and how much of each one does it. Stratholme's
/// city shell sets the bit on 61 of its 83 groups; the only other roots that set it at all are the
/// four Caverns of Time shells, which ship in the 5875 data with no 1.12 instance to enter.
pub fn skyboxscan(chain: &mut Chain) -> Result<()> {
    // Roots only: a group file is `<stem>_NNN.wmo`, and only the root carries MOSB/MOGI.
    let names: Vec<String> = chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".wmo")
                && !l
                    .strip_suffix(".wmo")
                    .and_then(|s| s.rsplit('_').next())
                    .is_some_and(|tail| tail.len() == 3 && tail.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();

    // The cross-tab that identifies the flag: roots with/without a MOSB × groups with/without 0x40000.
    let (mut both, mut mosb_only, mut flag_only, mut neither) = (0u32, 0u32, 0u32, 0u32);
    let mut scanned = 0u32;
    let mut hits: Vec<(String, String, usize, usize)> = Vec::new();
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        let Ok(root) = benilla_formats::parse_wmo_root(&bytes) else {
            continue; // a group file that slipped the name filter, or a truncated root
        };
        scanned += 1;
        let groups = root.group_infos();
        let flagged = groups.iter().filter(|g| g.show_skybox).count();
        match (root.skybox(), flagged > 0) {
            (Some(sky), true) => {
                both += 1;
                hits.push((name, sky.to_string(), flagged, groups.len()));
            }
            (Some(sky), false) => {
                mosb_only += 1;
                hits.push((
                    name,
                    format!("{sky}  (NO group sets 0x40000)"),
                    0,
                    groups.len(),
                ));
            }
            (None, true) => {
                flag_only += 1;
                hits.push((name, "(no MOSB)".into(), flagged, groups.len()));
            }
            (None, false) => neither += 1,
        }
    }

    hits.sort();
    println!("{:<62} {:>7}  skybox model", "WMO root", "groups");
    for (name, sky, flagged, total) in &hits {
        println!("{name:<62} {flagged:>3}/{total:<3}  {sky}");
    }
    println!();
    println!("{scanned} WMO root(s) scanned");
    println!("  MOSB skybox AND >=1 group with 0x40000 : {both}");
    println!("  MOSB skybox but NO group with 0x40000  : {mosb_only}");
    println!("  group(s) with 0x40000 but NO MOSB      : {flag_only}");
    println!("  neither                                : {neither}");
    if flag_only == 0 {
        println!(
            "\n=> 0x40000 NEVER appears without a MOSB ({flag_only} counter-examples in {scanned} \
             roots), and {mosb_only} root(s) name a skybox no group asks for. So the bit is real and \
             both halves matter — but this census establishes only 'flag implies MOSB'."
        );
        println!(
            "   It does NOT say WHICH group the renderer tests, and reading it as if it did is the \
             mistake decision 0767 made (superseded by 0773). The carved law: 0x40000 is tested \
             inside the portal flood (0x6b42e0 in 0x6b41c0) on the group being VISITED, never on the \
             group the camera stands in — so the predicate is 'any FLOOD-REACHED group carries the \
             bit, and the root names a MOSB'. Stratholme's King's Square is the counter-example that \
             settles it: the camera's own group (39) is EXTERIOR and unflagged, and the reference \
             paints the sky there anyway."
        );
    } else {
        println!(
            "\n=> {flag_only} group(s) set 0x40000 with no MOSB to draw — that would break even the \
             weak 'flag implies MOSB' reading this census rests on; re-derive before building on it."
        );
    }
    Ok(())
}
