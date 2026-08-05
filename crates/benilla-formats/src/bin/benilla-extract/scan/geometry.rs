//! Corpus scans over **what geometry a model draws** — the shape of its batches, independent of
//! how they are textured or shaded.
//!
//! Billboard cards and which way they face (`bbscan`, `bbfacescan`), geosets and the untextured /
//! single-triangle strays (`geosetscan`), and flat ground-plane quads (`groundscan`). What
//! MATERIAL a batch carries is [`super::material`]'s question.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use benilla_formats::Chain;

/// Sweep every `.m2` (under `prefix`, if given) and classify every BILLBOARD batch by which way its
/// geometry faces — see the `Bbfacescan` command doc for why the sign decides visibility.
///
/// A billboard bone puts the model's **+X** toward the viewer (`billboard-bone-law`, spherical arm),
/// so a batch whose winding normal is +X faces the camera and a −X one faces away. Single-sided
/// (`two_sided` false, i.e. no material `0x04`), the away-facing ones are backface-culled by the
/// reference from every angle — they are authored placeholders the author never saw.
pub fn bbfacescan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let names = super::m2_names(chain, prefix)?;
    let (mut scanned, mut cards) = (0u32, 0u32);
    // The four card populations. `away_single` is the one the renderer's forced-two-sided override
    // changes: those and only those become visible when a card is not allowed to be culled.
    let (mut toward, mut away_single, mut away_two, mut edge_on) = (0u32, 0u32, 0u32, 0u32);
    // …and the batches that are not cards at all. A facing only exists for a batch that IS one
    // plane; a closed solid has faces every way round, backface culling never hides it, and
    // sampling its first triangle answers a question it doesn't have (see `plane_normal`).
    let mut solid = 0u32;
    // …and the other side of the same gate: batches the split REFUSED because their geometry is
    // welded to a billboard bone (`RenderSubmesh::welded_billboard`, decisions 0839/0841). These
    // carry no `billboard` at all, so the card loop below never sees them — they are counted from
    // the flag the render lanes read, which is what makes this a cross-check of `bbscan`'s SEAM
    // column (same models, counted from the two ends of the rule) rather than a re-derivation.
    let (mut welded, mut weld_models) = (0u32, 0u32);
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
        let weld = subs.iter().filter(|s| s.welded_billboard).count() as u32;
        welded += weld;
        weld_models += u32::from(weld > 0);
        for (i, s) in subs.iter().enumerate() {
            let Some(bb) = &s.billboard else { continue };
            cards += 1;
            // Not one plane ⇒ not a card. Counted apart rather than classified by a triangle.
            if s.plane_normal().is_none() {
                solid += 1;
                continue;
            }
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
         {edge_on} edge-on/degenerate, {solid} NOT PLANAR (3-D geometry — no facing to decide); \
         plus {welded} WELDED batch(es) across {weld_models} model(s) the split refused \
         (skinned, not carded)"
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

/// Sweep every `.m2` (under `prefix`, if given) and classify its billboard usage — see the
/// `Bbscan` command doc. Output per model: the authored arms and how many vertices ride each
/// DIRECTLY (primary bone is the billboard bone — the card path) vs INHERITED (primary bone
/// descends from one — the joint-palette path, decision 0205), then the same question for the
/// model's **particle emitters and ribbons** (`fx[…]`) — the population behind decision 0813: an
/// emitter on (or under) a billboard bone has a camera-dependent origin, because the reference
/// folds the record position through the *replaced* palette matrix
/// (wow-re `part-anchoring-live-bone.md` §1 row 3 · `m2emitspine::particle_bone_xform`).
pub fn bbscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let names = super::m2_names(chain, prefix)?;
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
    // …and the seam population (see the classification below).
    let (mut seam_models, mut seam_bones) = (0u32, 0u32);
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
        // SEAM bones: billboard bones whose geometry is welded to the rest of the model — by a
        // partial vertex weight, or by a triangle it shares with a static neighbour. The reference
        // skins per vertex, so such geometry BENDS (a flap's root stays on the body while its tip
        // swings to the camera); a rigid card cannot express that at all, and moving the group
        // rigidly tears the flap in two. These are the bones the split declines (decision 0839),
        // read through the renderer's own predicate so this census cannot drift from it.
        let seam = benilla_formats::non_separable_billboard_bones(&bytes);
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
        let seam_col = if seam.is_empty() {
            String::new()
        } else {
            seam_models += 1;
            seam_bones += seam.len() as u32;
            format!(
                "  SEAM[{}]",
                seam.iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        println!(
            "{bones:>4}  direct[{}]  inherited[{}]  fx[{fx_counts}]{seam_col}  {name}",
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
        "{scanned} models scanned, {hits} with billboard bones; models by arm — direct(card) [{}]  inherited(palette) [{}]; {fx_models} with EFFECTS on a billboard chain [{fx_tot}]; {seam_models} with SEAM billboard bones ({seam_bones} total) — geometry welded to the model, which a rigid card tears",
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
    let names = super::m2_names(chain, prefix)?;
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
    let names = super::m2_names(chain, prefix)?;
    let (mut scanned, mut hits, mut all_flat, mut mixed) = (0u32, 0u32, 0u32, 0u32);
    let (mut quad_total, mut other_total) = (0u32, 0u32);
    // The hover census: every ground-quad-SHAPED batch whose uniform plane sits ABOVE z = 0
    // (any height — the shape test is the renderer's own, the ceiling is lifted), the population
    // that decides where the decal lane's hover ceiling (`GROUND_HOVER_MAX`) can sit.
    let mut hovers: Vec<(f32, String)> = Vec::new();
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
        let mut model_hovers: Vec<f32> = Vec::new();
        for s in &subs {
            if s.positions.is_empty() {
                continue;
            }
            if let Some((_, hover)) = s.ground_quad_hover(f32::INFINITY) {
                if hover > 0.01 {
                    hovers.push((hover, name.clone()));
                    model_hovers.push(hover);
                }
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
        if flat_count == 0 && model_hovers.is_empty() {
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
        let hover_note = if model_hovers.is_empty() {
            String::new()
        } else {
            format!(
                "  hover[{}]",
                model_hovers
                    .iter()
                    .map(|h| format!("{h:.2}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        println!(
            "{total_batches:>3} batches  {flat_count:>3} flat ({quad_count:>2} quad-1bone, {other_count:>2} other-flat){hover_note}  blend[{}]  {name}",
            blend_modes.join(" ")
        );
    }
    eprintln!(
        "{scanned} models scanned, {hits} with flat batches ({all_flat} all-flat, {mixed} mixed); flat batches: {quad_total} QUAD-1BONE, {other_total} OTHER-FLAT"
    );
    if !hovers.is_empty() {
        hovers.sort_by(|a, b| a.0.total_cmp(&b.0));
        eprintln!(
            "{} hovering quad-shaped batches (uniform plane z > 0.01), sorted:",
            hovers.len()
        );
        for (h, name) in &hovers {
            eprintln!("  z {h:>7.3}  {name}");
        }
    }
    Ok(())
}
