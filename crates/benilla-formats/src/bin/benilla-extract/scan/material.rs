//! Corpus scans over **how a batch is textured and blended** — the material side of a model's
//! render submeshes.
//!
//! Blend modes (`blendscan`), per-sequence batch visibility through the alpha combine
//! (`alphascan`), sampler address modes and the UVs that need them (`uvwrapscan`, `texmodescan`),
//! generated-texcoord environment stages (`envmapscan`), and the batches whose animated
//! texture-transform / tint loop is not the same in every sequence slot, which the bake routes to
//! a per-placement material (`uvslotscan`).

use std::collections::BTreeMap;

use anyhow::Result;
use benilla_formats::{Chain, KeyAnim, SeqLoops};

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

/// The two per-batch channels the bake resolves **per file sequence slot** — the pair
/// `models::m2_batches` hands the runtime as `uv_seq` / `rgb_seq`. Both are `C3Vector` tracks, so
/// one classifier serves both; only how many components survive the bake differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// The texture transform's **translation** track (`tex_anim::bake_uv_seqs`), read off
    /// `RenderSubmesh::uv_seq` and its shared-lane twin `uv_anim`.
    Uv,
    /// The M2Color **RGB** tint track (`mat_anim::bake_rgb_seqs`), read off `rgb_seq` / `rgb_anim`.
    Rgb,
}

impl Channel {
    fn label(self) -> &'static str {
        match self {
            Self::Uv => "UV ",
            Self::Rgb => "RGB",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Uv => "UV (texture-transform translation)",
            Self::Rgb => "RGB (M2Color tint)",
        }
    }

    /// The field name the report quotes, so a reader can grep the struct the numbers came from.
    fn field(self) -> &'static str {
        match self {
            Self::Uv => "uv_seq",
            Self::Rgb => "rgb_seq",
        }
    }

    /// Axis names for the value-range cells — one per component the bake keeps. The UV pair stays
    /// `x`/`y`: the bake carries the track's raw components and how the reference maps them onto
    /// U/V is still under RE (`tex_anim`'s module doc).
    fn axes(self) -> &'static [&'static str] {
        match self {
            Self::Uv => &["x", "y"],
            Self::Rgb => &["r", "g", "b"],
        }
    }
}

/// How far apart two baked loops may be and still be the **same authored function**. Tight on
/// purpose: this is the noise floor of a `u32` millisecond key rebased through an `f32` divide, not
/// an authoring tolerance — a disagreement anyone could see is orders of magnitude larger.
const KEY_EPS: f32 = 1e-4;

/// Why one batch's slots disagree — the sub-classification of every batch the bake routed to the
/// per-placement lane, i.e. of every set whose [`SeqLoops::uniform`] came back `None`.
///
/// The question these four answer is whether that verdict is **earned**. `uniform()` compares
/// `KeyAnim` by `PartialEq` — exact float equality on the clock flags, the period and every key —
/// so two slots holding the same authored loop can still be split by a single flag or by
/// sub-epsilon noise in a rebased timestamp. `Dead0` and `RealDiffers` are real divergence, where
/// one material per placement is the only right answer; `WrapOnly` and `KeysEpsilon` are batches a
/// coarser comparison would have left on the shared lane, and their count IS the over-application.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Why {
    /// Slot 0 bakes nothing while a later slot bakes a loop — the B98 shape, the founding case, and
    /// the only class a slot-0-pinned bake could not render at all.
    Dead0,
    /// Every slot bakes the same loop except [`KeyAnim::wrap`]: the sequences carrying it disagree
    /// on their own loop flag, so one band clamps at its tail where another wraps. Visible only at
    /// the very end of a one-shot band — and never at all on a placed doodad, which loops.
    WrapOnly,
    /// Every slot agrees to within [`KEY_EPS`] on every number and on every flag — the slots differ
    /// only by float noise in the rebased key times, and exact `PartialEq` is the whole reason they
    /// are here.
    KeysEpsilon,
    /// Genuinely different loops: a different key count, a different interpolation or clock law,
    /// numbers apart by more than noise — or slot 0 alive against a later slot that holds still,
    /// DEAD-0's mirror.
    RealDiffers,
}

impl Why {
    const ALL: [Self; 4] = [
        Self::Dead0,
        Self::WrapOnly,
        Self::KeysEpsilon,
        Self::RealDiffers,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Dead0 => "DEAD-0",
            Self::WrapOnly => "WRAP-ONLY",
            Self::KeysEpsilon => "KEYS-EPSILON",
            Self::RealDiffers => "REAL-DIFFERS",
        }
    }

    /// The one-line reading of the bucket, printed beside its counts.
    fn blurb(self) -> &'static str {
        match self {
            Self::Dead0 => {
                "slot 0 bakes nothing while a later slot animates — frozen for ever on the \
                 shared lane, and the population the per-placement lane was built for"
            }
            Self::WrapOnly => {
                "the slots bake the same loop and disagree only on the WRAP flag — one band \
                 clamps at its tail where another wraps"
            }
            Self::KeysEpsilon => {
                "the slots agree on every number to within 1e-4 — sub-epsilon noise in the \
                 rebased keys, split by exact float equality"
            }
            Self::RealDiffers => {
                "genuinely different loops (or slot 0 alive against a dead later slot): no single \
                 shared loop can serve every sequence"
            }
        }
    }
}

/// One batch-channel's read of the bake.
struct Verdict {
    why: Why,
    /// The largest numeric disagreement between slot 0 and any other slot. `0.0` means the slots
    /// are bit-identical — which, for a set the bake kept, means only a *flag* differs.
    delta: f32,
    /// The report line's detail cell.
    detail: String,
}

/// The largest absolute disagreement between two baked loops over everything a consumer reads: the
/// period, and every key's time and value. `None` when they are not the same **shape** at all — a
/// different key count or a different interpolation/clock law is a different function, not noise.
fn loop_delta<V: AsRef<[f32]>>(a: &KeyAnim<V>, b: &KeyAnim<V>) -> Option<f32> {
    if a.step != b.step || a.gseq != b.gseq || a.keys.len() != b.keys.len() {
        return None;
    }
    let mut d = (a.period - b.period).abs();
    for ((ta, va), (tb, vb)) in a.keys.iter().zip(&b.keys) {
        d = d.max((ta - tb).abs());
        for (x, y) in va.as_ref().iter().zip(vb.as_ref()) {
            d = d.max((x - y).abs());
        }
    }
    Some(d)
}

/// `[.L]` — one character per **file** sequence slot: `L` where the slot bakes a loop, `.` where
/// its key window moves nothing. Slot 0 is leftmost, so a leading `.` reads as the B98 shape at a
/// glance. Long tables (a creature's fifty-odd sequences) are truncated to a readable head.
fn slot_cell<V>(slots: &[Option<KeyAnim<V>>]) -> String {
    const HEAD: usize = 28;
    let bits: String = slots
        .iter()
        .map(|s| if s.is_some() { 'L' } else { '.' })
        .collect();
    let live = slots.iter().filter(|s| s.is_some()).count();
    if bits.len() <= HEAD {
        format!("[{bits}]")
    } else {
        format!("[{}…] {live}/{} live", &bits[..HEAD], slots.len())
    }
}

/// `[Wc.]` — the per-slot clock law, the other half of what `PartialEq` compares: `W` wrap, `c`
/// clamp, `.` dead. The whole content of a WRAP-ONLY row.
fn wrap_cell<V>(slots: &[Option<KeyAnim<V>>]) -> String {
    slots
        .iter()
        .map(|s| match s {
            Some(l) if l.wrap => 'W',
            Some(_) => 'c',
            None => '.',
        })
        .collect::<String>()
}

/// `1.500s wrap 28k x[+0.000..+0.938] y[…]` — one baked loop as its consumer sees it: the clock,
/// the size, and the range it sweeps on every component the bake keeps.
fn loop_cell<V: AsRef<[f32]>>(l: &KeyAnim<V>, ch: Channel) -> String {
    let range = ch
        .axes()
        .iter()
        .enumerate()
        .map(|(c, axis)| {
            let (lo, hi) = l
                .keys
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), (_, v)| {
                    let x = v.as_ref()[c];
                    (lo.min(x), hi.max(x))
                });
            format!("{axis}[{lo:+.3}..{hi:+.3}]")
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{:>6.3}s {} {:>3}k {range}",
        l.period,
        if l.wrap { "wrap " } else { "clamp" },
        l.keys.len(),
    )
}

/// Read one batch-channel's per-slot set exactly as the runtime does — [`SeqLoops::slots`], the
/// same values [`SeqLoops::uniform`] compared — and say why they disagree.
fn classify<V: AsRef<[f32]> + PartialEq>(set: &SeqLoops<V>, ch: Channel) -> Verdict {
    let slots = set.slots();
    let cell = slot_cell(slots);
    // A set exists only when SOME slot bakes, so a dead slot 0 is by construction a dead slot 0
    // beside a live later one — no second test needed.
    let Some(base) = slots.first().and_then(Option::as_ref) else {
        let (s, live) = slots
            .iter()
            .enumerate()
            .find_map(|(s, l)| Some((s, l.as_ref()?)))
            .expect("a set exists only when some slot bakes a loop");
        return Verdict {
            why: Why::Dead0,
            delta: 0.0,
            detail: format!("{cell} slot 0 dead, slot {s} {}", loop_cell(live, ch)),
        };
    };
    let (mut delta, mut wrap) = (0.0f32, false);
    for (s, slot) in slots.iter().enumerate().skip(1) {
        let Some(l) = slot.as_ref() else {
            return Verdict {
                why: Why::RealDiffers,
                delta,
                detail: format!("{cell} slot 0 {} vs slot {s} dead", loop_cell(base, ch),),
            };
        };
        // Name WHAT diverges, not merely that something did: a value-range cell hides a timing
        // difference (same extremes, different key times) behind identical-looking brackets, and
        // that is exactly the ambiguity this scan exists to remove.
        let reason = match loop_delta(base, l) {
            Some(d) if d <= KEY_EPS => {
                delta = delta.max(d);
                wrap |= l.wrap != base.wrap;
                continue;
            }
            Some(d) => format!("keys apart by {d:.1e}"),
            None => "a different key count or clock law".to_string(),
        };
        return Verdict {
            why: Why::RealDiffers,
            delta,
            detail: format!(
                "{cell} slot 0 {} vs slot {s} {} — {reason}",
                loop_cell(base, ch),
                loop_cell(l, ch),
            ),
        };
    }
    let why = if wrap {
        Why::WrapOnly
    } else {
        Why::KeysEpsilon
    };
    Verdict {
        why,
        delta,
        detail: format!(
            "{cell} {} clock {} delta {delta:.1e}",
            loop_cell(base, ch),
            wrap_cell(slots),
        ),
    }
}

/// Per-channel corpus counters. Model counts are per bucket and count a model once if **any** of
/// its batches lands there, so they overlap rather than partition.
#[derive(Default)]
struct Tally {
    /// The set is `None`: every slot bakes the same loop, so the batch keeps the shared lane.
    shared_b: u32,
    /// …and of those, the ones that actually animate there (`uv_anim`/`rgb_anim` with `period > 0`).
    shared_live_b: u32,
    shared_live_m: u32,
    /// The set is `Some` — batches and models routed to the per-placement lane, by [`Why`].
    per_b: [u32; 4],
    per_m: [u32; 4],
    /// Models with at least one per-placement batch on this channel, whatever the reason.
    per_m_any: u32,
    /// WRAP-ONLY batches whose slots are **bit-identical** apart from the flag; the rest carry
    /// sub-epsilon key noise on top of it.
    wrap_exact: u32,
}

/// One model's per-placement batches in one (channel, bucket) — the row the family census and the
/// worst-hit tail are both built from.
struct Hit {
    name: String,
    ch: Channel,
    why: Why,
    batches: u32,
    /// The model's **file** sequence slot count, straight off the set the bake built.
    slots: usize,
}

/// How many rows of the worst-hit tail print (the rest are counted, never silently dropped).
const TAIL_ROWS: usize = 25;

/// Sweep every `.m2` (optionally under a path prefix) and census the batches the bake routes to a
/// **per-placement material** because their UV / tint loop is not the same in every file sequence
/// slot — see the `Uvslotscan` command doc for the why.
///
/// It reads the bake's own verdict (`RenderSubmesh::uv_seq` / `rgb_seq`, `Some` exactly when
/// [`SeqLoops::uniform`] refused the shared lane) rather than re-deriving it: this scan used to
/// carry a transcribed sampler because the fix it was sizing did not exist yet, and once it did the
/// twin drifted from it — the very failure `idleslotscan`'s rule names ("beside the parse, so the
/// census asks the renderer's own question instead of a hand-copied twin that can drift from it").
/// Every number below is therefore what the runtime sees, and the four PER-PLACEMENT buckets say
/// how much of it `uniform()`'s exact float equality is *earning*.
pub fn uvslotscan(chain: &mut Chain, prefix: Option<&str>) -> Result<()> {
    let names = super::m2_names(chain, prefix)?;
    let (mut scanned, mut batches) = (0u32, 0u64);
    let mut tally: [Tally; 2] = Default::default();
    let mut hits: Vec<Hit> = Vec::new();

    for name in names {
        let Ok(bytes) = chain.read_file(&name) else {
            continue;
        };
        let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
        let Ok(subs) = benilla_formats::parse_m2_render_submeshes(&bytes, dir, &[]) else {
            continue;
        };
        scanned += 1;
        batches += subs.len() as u64;

        let mut lines: Vec<String> = Vec::new();
        // This model's own contribution, folded into the corpus tally once per channel below:
        // (shared, shared-and-live) batch counts, per-bucket batch counts, and the file sequence
        // slot count — which only a batch carrying a set can report, because the set IS the table.
        let mut shared = [(0u32, 0u32); 2];
        let mut per = [[0u32; 4]; 2];
        let mut slots = [0usize; 2];
        for (bi, sub) in subs.iter().enumerate() {
            for ch in [Channel::Uv, Channel::Rgb] {
                let (verdict, live, n) = match ch {
                    Channel::Uv => (
                        sub.uv_seq.as_ref().map(|s| classify(s, ch)),
                        sub.uv_anim.as_ref().is_some_and(|a| a.period > 0.0),
                        sub.uv_seq.as_ref().map_or(0, |s| s.slots().len()),
                    ),
                    Channel::Rgb => (
                        sub.rgb_seq.as_ref().map(|s| classify(s, ch)),
                        sub.rgb_anim.as_ref().is_some_and(|a| a.period > 0.0),
                        sub.rgb_seq.as_ref().map_or(0, |s| s.slots().len()),
                    ),
                };
                let c = ch as usize;
                let Some(v) = verdict else {
                    shared[c].0 += 1;
                    shared[c].1 += u32::from(live);
                    continue;
                };
                per[c][v.why as usize] += 1;
                slots[c] = n;
                if v.why == Why::WrapOnly && v.delta == 0.0 {
                    tally[c].wrap_exact += 1;
                }
                lines.push(format!(
                    "    {} batch {bi:>3}  {:<12}  {}",
                    ch.label(),
                    v.why.label(),
                    v.detail
                ));
            }
        }
        for ch in [Channel::Uv, Channel::Rgb] {
            let c = ch as usize;
            let t = &mut tally[c];
            t.shared_b += shared[c].0;
            t.shared_live_b += shared[c].1;
            t.shared_live_m += u32::from(shared[c].1 > 0);
            t.per_m_any += u32::from(per[c].iter().any(|&n| n > 0));
            for w in Why::ALL {
                let n = per[c][w as usize];
                if n > 0 {
                    t.per_b[w as usize] += n;
                    t.per_m[w as usize] += 1;
                    hits.push(Hit {
                        name: name.clone(),
                        ch,
                        why: w,
                        batches: n,
                        slots: slots[c],
                    });
                }
            }
        }
        if !lines.is_empty() {
            println!(
                "{name}  ({} file sequence slots)",
                slots.iter().copied().max().unwrap_or(0)
            );
            for l in &lines {
                println!("{l}");
            }
        }
    }

    println!();
    println!("=== summary ===  {scanned} model(s) parsed, {batches} render batch(es)");
    for ch in [Channel::Uv, Channel::Rgb] {
        let t = &tally[ch as usize];
        let per_total: u32 = t.per_b.iter().sum();
        println!();
        println!(
            "=== {} ===  {} batch-channel(s)",
            ch.title(),
            u64::from(t.shared_b) + u64::from(per_total)
        );
        println!(
            "  SHARED         {:>7} batch(es)  — `{}` is None: every slot bakes the same loop, so \
             the batch keeps the per-material lane it has always had",
            t.shared_b,
            ch.field()
        );
        println!(
            "    of those     {:>7} batch(es) / {:>4} model(s) carry a LIVE loop there \
             (period > 0); the rest animate nothing at all",
            t.shared_live_b, t.shared_live_m
        );
        println!(
            "  PER-PLACEMENT  {:>7} batch(es) / {:>4} model(s)  — `{}` is Some: `uniform()` \
             refused the shared lane, so the world streamer builds this batch a material per \
             placement",
            per_total,
            t.per_m_any,
            ch.field()
        );
        for w in Why::ALL {
            println!(
                "    {:<13} {:>5} batch(es) / {:>4} model(s)  — {}",
                w.label(),
                t.per_b[w as usize],
                t.per_m[w as usize],
                w.blurb()
            );
        }
        if t.per_b[Why::WrapOnly as usize] > 0 {
            println!(
                "      ({} of the WRAP-ONLY batches are bit-identical apart from the flag; the \
                 rest carry sub-epsilon key noise on top of it)",
                t.wrap_exact
            );
        }
        // Where each bucket's population LIVES, and which models carry most of it. `World\` is a
        // placed doodad or WMO prop — the only lane the per-placement material reaches — so a
        // bucket that is mostly `Creature\`/`Spells\` costs nothing today whatever it says.
        for w in Why::ALL {
            let rows: Vec<&Hit> = hits
                .iter()
                .filter(|h| h.ch == ch && h.why == w)
                .collect::<Vec<_>>();
            if rows.is_empty() {
                continue;
            }
            println!();
            println!("  --- {} ---  {} model(s)", w.label(), rows.len());
            let mut fams: BTreeMap<String, (u32, u32)> = BTreeMap::new();
            for h in &rows {
                let e = fams.entry(super::family_of(&h.name)).or_default();
                e.0 += 1;
                e.1 += h.batches;
            }
            for (fam, (m, b)) in &fams {
                println!("    {fam:<28} {m:>4} model(s)  {b:>5} batch(es)");
            }
            let mut tail = rows;
            tail.sort_by(|a, b| b.batches.cmp(&a.batches).then_with(|| a.name.cmp(&b.name)));
            for h in tail.iter().take(TAIL_ROWS) {
                println!(
                    "      {:>4} batch(es)  {:>3} slots  {}",
                    h.batches, h.slots, h.name
                );
            }
            if let Some(rest) = tail.len().checked_sub(TAIL_ROWS).filter(|n| *n > 0) {
                println!("      … and {rest} more (top {TAIL_ROWS} shown)");
            }
        }
    }
    Ok(())
}
