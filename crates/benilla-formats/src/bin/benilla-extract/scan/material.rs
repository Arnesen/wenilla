//! Corpus scans over **how a batch is textured and blended** — the material side of a model's
//! render submeshes.
//!
//! Blend modes (`blendscan`), per-sequence batch visibility through the alpha combine
//! (`alphascan`), sampler address modes and the UVs that need them (`uvwrapscan`, `texmodescan`),
//! and generated-texcoord environment stages (`envmapscan`).

use std::collections::BTreeMap;

use anyhow::Result;
use benilla_formats::Chain;

/// Sweep every `.m2` (under `prefix`, if given) and list the models whose MATERIAL table authors
/// blend mode 5 (Mod) / 6 (Mod2x) — the multiply-blend census (decision 0528). One line per
/// matching model: its per-material `(flags, blend)` pairs and path. The raw header read (materials
/// count/ofs at `0x84`, 4-byte `{u16 flags, u16 blend}` records) matches `benilla-m2`'s parse.
pub fn blendscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let names = super::m2_names(chain, prefix)?;
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
    let names = super::m2_names(chain, prefix)?;
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
                (0..=16u16).all(|k| a.sample(Some(slot), f32::from(k) * 0.25, 0.0) <= 0.0)
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
    let names = super::m2_names(chain, prefix)?;
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
    let names = super::m2_names(chain, prefix)?;
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

/// Sweep every `.m2` (optionally under a path prefix) and census the batches whose texture
/// coordinates are **GENERATED, not authored** — the sphere-map environment stages
/// (`texture_unit_lookup[texCoordSet] > 2`, the reference's gate at `0x70b8bd`).
///
/// The population instrument for a silent class: such a batch carries no usable UVs *by design* —
/// the artist collapses the whole mesh onto one point because the runtime is meant to supply the
/// coordinates — so a renderer that reads the vertex UV draws the entire surface in **one texel**
/// of a reflection sheet. Nothing about that failure is loud: no missing geometry, no error, just a
/// flat wash of whatever colour happens to sit at that corner (`GnomeSubwayGlass.m2` → the Deeprun
/// Tram tube's yellow, `AKGNOMEREFLECT.BLP` texel 0,0 = 225,221,142, doubled by its Mod2x blend).
///
/// **DEGENERATE** marks the batches where the authored UVs collapse to a single point — the ones
/// that render as a flat colour field. The rest carry leftover UVs that merely go unused, so they
/// misdraw as a static smear of the sheet instead: wrong, but not obviously so. Both are fixed by
/// the same mechanism; the split says which reports a renderer's env support explains.
pub fn envmapscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let names = super::m2_names(chain, prefix)?;
    let (mut scanned, mut batches, mut hits, mut models, mut degenerate) =
        (0u32, 0u32, 0u32, 0u32, 0u32);
    // Which blend modes and which sheets the mechanism actually serves — a Mod2x env layer tints
    // what is behind it (glass), an Add one lays a highlight over the surface (the metal sheen).
    let mut by_blend: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_sheet: BTreeMap<String, u32> = BTreeMap::new();
    // **The fallback census.** `stage_is_env_mapped` reads an OUT-OF-RANGE `texture_unit_lookup`
    // index as env — the reference's own unguarded read, and the only safe direction. But that
    // branch is the one way the gate can *invent* env-mapping on a model whose art never asked for
    // it, so it is counted separately: a hit is trustworthy exactly when it came from a real
    // `>= 3` entry. `empty_table` is the degenerate shape of the same thing (no table at all ⇒
    // every batch falls through), broken out because it would tar a whole model rather than a
    // stage.
    let (mut from_oob, mut empty_table) = (0u32, 0u32);
    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        scanned += 1;
        if let Ok(fmt) = benilla_m2::parse_m2(&mut std::io::Cursor::new(&bytes[..])) {
            let model = fmt.model();
            if model.texture_unit_lookup.is_empty() {
                empty_table += 1;
            }
            if let Ok(skin) = model.parse_embedded_skin(&bytes, 0) {
                for b in skin.batches() {
                    let idx = b.texture_coord_combo_index as usize;
                    if model.texture_unit_lookup.get(idx).is_none() {
                        from_oob += 1;
                    }
                }
            }
        }
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        let mut lines = Vec::new();
        for (i, s) in subs.iter().enumerate() {
            batches += 1;
            if !s.env_map {
                continue;
            }
            hits += 1;
            *by_blend.entry(format!("{:?}", s.blend)).or_default() += 1;
            let sheet = s.texture.as_deref().unwrap_or("NONE").to_string();
            *by_sheet.entry(sheet.clone()).or_default() += 1;
            // Does the authored UV set collapse to a point? Then the vertex data cannot even
            // approximate the sheet and the batch renders as one flat colour.
            let span = |axis: usize| {
                s.uvs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), t| {
                    (lo.min(t[axis]), hi.max(t[axis]))
                })
            };
            let (u, v) = (span(0), span(1));
            let flat = s.uvs.is_empty() || (u.1 - u.0 <= 1e-6 && v.1 - v.0 <= 1e-6);
            if flat {
                degenerate += 1;
            }
            lines.push(format!(
                "    batch {i:>3}: {:?}{}{} {} verts  authored uv u[{:+.3}..{:+.3}] \
                 v[{:+.3}..{:+.3}]  {}  tex {sheet}",
                s.blend,
                if s.additive { " additive" } else { "" },
                if s.emissive { " unlit" } else { "" },
                s.positions.len(),
                u.0,
                u.1,
                v.0,
                v.1,
                if flat { "DEGENERATE" } else { "unused" },
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
    eprintln!("blend modes: {by_blend:?}");
    let mut sheets: Vec<_> = by_sheet.into_iter().collect();
    sheets.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    eprintln!("top reflection sheets:");
    for (sheet, n) in sheets.iter().take(10) {
        eprintln!("   {n:>5} batch(es)  {sheet}");
    }
    eprintln!(
        "gate provenance: {from_eal} of {hits} hit(s) came from a REAL `>= 3` table entry, \
         {from_oob} from the out-of-range fallback ({empty_table} model(s) carry no \
         texture_unit_lookup at all)",
        from_eal = hits.saturating_sub(from_oob),
    );
    eprintln!(
        "{scanned} models scanned, {batches} batch(es): {hits} ENV-MAPPED (generated texcoords) \
         across {models} model(s) — {degenerate} of them DEGENERATE (authored UVs collapse to a \
         point, so a renderer reading the vertex UV draws one flat texel), {} carrying leftover \
         UVs that merely go unused. {} distinct reflection sheet(s).",
        hits - degenerate,
        sheets.len(),
    );
    Ok(())
}
