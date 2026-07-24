//! Per-model M2 dump printers: `m2coll`, `m2seq`, `m2attach`, `m2anim`, `m2bones`, `m2batch` —
//! the single-model diagnostics that read one `.m2` and print everything a given concern
//! (collision hull, sequences, attachment points, animation channels, bone table, render
//! batches) actually carries.

use anyhow::{Context, Result};
use benilla_formats::{Chain, M2AnimSummary};

use crate::{normalize, yn};

/// Dump an M2's collision hull: counts, model-space AABB (WoW axes, Z up), extents.
pub fn m2coll(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let hull = benilla_formats::load_m2_collision_hull(chain, &name)?;
    if hull.is_empty() {
        println!("no collision hull (nBoundingTriangles == 0) — nothing collides with this model");
        return Ok(());
    }
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &hull.positions {
        for a in 0..3 {
            min[a] = min[a].min(p[a]);
            max[a] = max[a].max(p[a]);
        }
    }
    println!(
        "{} vertices, {} triangles",
        hull.positions.len(),
        hull.triangle_count()
    );
    println!("aabb (model space, yd; WoW axes, Z up — placement scale multiplies):");
    for (axis, a) in ["x", "y", "z"].iter().zip(0..3) {
        println!(
            "  {axis}: {:>8.3} .. {:>8.3}   extent {:>7.3}",
            min[a],
            max[a],
            max[a] - min[a]
        );
    }
    Ok(())
}

/// Dump every sequence's EVENT keyframes: time (s from sequence start), 4CC ident, payload.
/// The event-order instrument (decision 0279): on attack clips, whether `$CPP` (the victim
/// defense dispatch) precedes `$AH0-3`/`$CAH` (the impact dispatch) decides which of the two
/// mutually-exclusive victim reactions the shared swing record feeds.
pub fn m2events(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let data = chain
        .read_file(&name)
        .with_context(|| format!("reading '{name}' from chain"))?;
    let seqs = benilla_formats::parse_m2_animations(&data);
    for (i, s) in seqs.iter().enumerate() {
        if s.events.is_empty() {
            continue;
        }
        let tags: Vec<String> = s
            .events
            .iter()
            .map(|e| {
                let ident = String::from_utf8_lossy(&e.ident);
                if e.data != 0 {
                    format!("{:.3}s {ident}({})", e.time, e.data)
                } else {
                    format!("{:.3}s {ident}", e.time)
                }
            })
            .collect();
        println!(
            "{i:>3}  anim {:>4}  dur {:>6.3}s  {}",
            s.anim_id,
            s.duration,
            tags.join("  ")
        );
    }
    Ok(())
}

/// Dump an M2's animation sequences in file order.
pub fn m2seq(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let data = chain
        .read_file(&name)
        .with_context(|| format!("reading '{name}' from chain"))?;
    let seqs = benilla_formats::parse_m2_animations(&data);
    println!("idx  anim   mode   dur(s)   freq  replay   bones   keys");
    for (i, s) in seqs.iter().enumerate() {
        // How much data the sequence's own time band actually holds: bones with any
        // keyed track, and total keys across T/R/S (clamp constants included — a bone
        // unkeyed in this band pins to its nearest authored key, see `read_bone_track`).
        // Uneven coverage across same-id variations is what exposed the task-#14 tilt
        // (HumanMale Stand idx 136 keys 13 fewer bones than the head).
        let bones = s
            .bones
            .iter()
            .filter(|b| !b.translation.is_empty() || !b.rotation.is_empty() || !b.scale.is_empty())
            .count();
        let keys: usize = s
            .bones
            .iter()
            .map(|b| b.translation.len() + b.rotation.len() + b.scale.len())
            .sum();
        println!(
            "{i:>3}  {:>4}  {}  {:>7.3}  {:>5}  ({}, {})  {bones:>5}  {keys:>5}",
            s.anim_id,
            if s.looping { "loop " } else { "clamp" },
            s.duration,
            s.frequency,
            s.min_replay,
            s.max_replay,
        );
    }
    eprintln!("{} sequences", seqs.len());
    Ok(())
}

/// Dump an M2's attachment points (id + bone).
pub fn m2attach(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let data = chain
        .read_file(&name)
        .with_context(|| format!("reading '{name}' from chain"))?;
    let attachments = benilla_formats::parse_m2_attachments(&data)?;
    println!("id  bone");
    for a in &attachments {
        println!("{:>2}  {}", a.id, a.bone);
    }
    eprintln!("{} attachment points", attachments.len());
    Ok(())
}

/// One texture-transform track line for the `m2anim` dump: key count, interp/gseq tags, and the
/// first/last keys (enough to read a scroll direction + rate off a waterfall).
fn print_txfm_track<V: std::fmt::Debug + Copy + PartialEq>(name: &str, t: &benilla_m2::M2Track<V>) {
    if t.keys.is_empty() {
        println!("    {name}: -");
        return;
    }
    let (t0, v0) = &t.keys[0];
    let (tn, vn) = &t.keys[t.keys.len() - 1];
    println!(
        "    {name}: {} key(s), interp {}, gseq {}, constant {}  [{t0} ms {v0:?} … {tn} ms {vn:?}]",
        t.keys.len(),
        t.interp,
        if t.gseq == 0xffff {
            "-".to_string()
        } else {
            t.gseq.to_string()
        },
        t.constant().is_some(),
    );
}

/// The `m2anim` subcommand's dump — one section per channel family.
fn print_m2anim_summary(s: &M2AnimSummary, bytes: &[u8]) {
    println!("sequences: {}", s.sequence_count);
    println!(
        "  seq0 bone motion: {}  ({} bone(s) with a >1-key track)",
        if s.seq0_has_bone_motion { "yes" } else { "no" },
        s.seq0_animated_bone_count
    );
    println!(
        "global-sequence bone channels: {}",
        s.global_seq_channels.len()
    );
    for (bone, kind, period_ms) in &s.global_seq_channels {
        println!("  bone {bone:>3}  {kind}  period {period_ms} ms");
    }
    println!(
        "transparency tracks: {} total, {} animated (>1 key)",
        s.transparency_tracks.0, s.transparency_tracks.1
    );
    println!(
        "color rgb tracks:    {} total, {} animated (>1 key)",
        s.color_rgb_tracks.0, s.color_rgb_tracks.1
    );
    println!(
        "color alpha tracks:  {} total, {} animated (>1 key)",
        s.color_alpha_tracks.0, s.color_alpha_tracks.1
    );
    println!(
        "texture transforms: {} (header count; tracks unparsed)",
        s.texture_transform_count
    );
    println!("particle emitters:  {}", s.particle_emitter_count);
    // The full defs (pos/blend/texture/shape/rate-keys/ramps) alongside the summary's bone links —
    // which emitter is the flame and which the glow is unreadable from bone+flags alone (the
    // blood-spurt starburst diagnosis, decision 0141; a flame that "doesn't burn" is usually
    // visible right here: an unresolved texture, or a burst rate track whose first key is 0).
    let defs = benilla_formats::parse_m2_particle_emitters(bytes).unwrap_or_default();
    for (i, e) in s.emitter_bones.iter().enumerate() {
        println!(
            "  emitter {i}  bone {:>3}  flags {:#010x}  chain animates: {}",
            e.bone,
            e.flags,
            match (e.chain_seq0, e.chain_gseq) {
                (true, true) => "seq0 + gseq",
                (true, false) => "seq0",
                (false, true) => "gseq",
                (false, false) => "no (rest pose)",
            }
        );
        let Some(d) = defs.get(i) else { continue };
        // The emission MODEL first: a BURST emitter fires one ftol(rate) puff at its rate edge
        // and never pours — reading its keys as a continuous rate is the exact misdiagnosis
        // behind the Eviscerate 0.5s-vs-2s gap (wow-re part-emission-burst-flag.md).
        let burst = if d.burst() { "BURST " } else { "" };
        let rate = if d.emission_rate.keys.len() == 1 {
            format!("{burst}rate {:.1}/s", d.emission_rate.first())
        } else {
            format!(
                "{burst}rate keys {:?}",
                d.emission_rate
                    .keys
                    .iter()
                    .map(|&(t, v)| (t, v as i32))
                    .collect::<Vec<_>>()
            )
        };
        // The enabled gate (clip-relative ms, like the rate keys): a one-shot effect's
        // choreography — "why does this emitter only flash for 200 ms" reads right here.
        let rate = if d.enabled.keys.len() == 1 {
            rate // always-on (the overwhelmingly common shape) — no noise
        } else {
            format!("{rate}  enabled {:?}", d.enabled.keys)
        };
        // A tail's streak length is |velocity|·tail_time — without it "how long is this
        // streak" needs a hand-parse of the raw record (the Eviscerate diagnosis gap).
        let tail = if d.head_tail >= 1 {
            format!(
                "  tail {:.2}s{}",
                d.tail_time,
                if d.tail_clamps_to_age() {
                    " (age-clamped)"
                } else {
                    ""
                }
            )
        } else {
            String::new()
        };
        println!(
            "             {:?} {:?} {}  {rate}  life {:.2}s  speed {:.2}  grav {:.2}  drag {:.1}{tail}  twinkle [{:.2}..{:.2}] spd {:.1} pct {:.2}  spin {:.2}",
            d.shape,
            d.blend,
            match d.head_tail {
                0 => "head",
                1 => "tail",
                _ => "head+tail",
            },
            d.lifespan,
            d.emission_speed,
            d.gravity,
            d.drag,
            d.twinkle_min,
            d.twinkle_max,
            d.twinkle_speed,
            d.twinkle_percent,
            d.spin,
        );
        // The kernel spread (wow-re part-shape-kernels): a sphere's ranges are latitude/longitude
        // about +X (area = min/max shell radius); a plane's are the ±θ/±φ cone about +Z (area =
        // the spawn rectangle). `(lat ±π, lon ±0)` reads directly as the edge-on ring family.
        let spread = match d.shape {
            benilla_formats::ParticleShape::Sphere => format!(
                "radius [{:.2}..{:.2}] lat ±{:.2} lon ±{:.2}",
                d.area_length, d.area_width, d.vertical_range, d.horizontal_range
            ),
            // Spline repurposing (wow-re part-spline-file-layout): area = tMin/tMax,
            // vRange = tangent-spin ψ, hRange = scatter.
            benilla_formats::ParticleShape::Spline => match &d.spline {
                Some(s) => format!(
                    "spline {} pts [{:.2} {:.2} {:.2} ..], t [{:.2}..{:.2}] spin ±{:.2} scatter {:.2}",
                    s.points.len(),
                    s.points[0][0],
                    s.points[0][1],
                    s.points[0][2],
                    d.area_length,
                    d.area_width,
                    d.vertical_range,
                    d.horizontal_range
                ),
                None => "spline UNPARSED (degenerate record)".to_string(),
            },
            _ => format!(
                "area {:.1}x{:.1} cone ±{:.2}/±{:.2}",
                d.area_length, d.area_width, d.vertical_range, d.horizontal_range
            ),
        };
        let zsrc = if d.z_source != 0.0 {
            format!("  zSource {:.2}", d.z_source)
        } else {
            String::new()
        };
        // The per-emitter model references: geometry (3-D model particles) and recursion
        // (child emitters).
        if let Some(g) = &d.geometry_model {
            println!("             MODEL-PARTICLES: {g}");
        }
        if let Some(r) = &d.recursion_model {
            println!("             CHILD-EMITTERS: {r}");
        }
        // The emitter-motion terms (wow-re part-emitter-motion): the follow-delta response
        // line's authored (speed → fraction) samples, and the velocity-inherit scale.
        let motion = match (d.follow_emitter(), d.inherits_emitter_motion()) {
            (false, false) => String::new(),
            (f, i) => {
                let mut s = String::new();
                if f {
                    s += &format!(
                        "  follow ({:.2}->{:.2}, {:.2}->{:.2})",
                        d.follow_speed1, d.follow_scale1, d.follow_speed2, d.follow_scale2
                    );
                }
                if i {
                    s += &format!("  inheritScale {:.2}", d.inherit_scale);
                }
                s
            }
        };
        println!(
            "             pos [{:.2} {:.2} {:.2}]  {spread}{zsrc}{motion}  texture: {}  cells {}x{}",
            d.position[0],
            d.position[1],
            d.position[2],
            d.texture.as_deref().unwrap_or("NONE (unresolved)"),
            d.tile_rows,
            d.tile_cols,
        );
        let c = d.over_life.color;
        println!(
            "             color/alpha keys: [{:.2} {:.2} {:.2} a{:.2}] -> [{:.2} {:.2} {:.2} a{:.2}] -> [{:.2} {:.2} {:.2} a{:.2}]  size {:?}",
            c[0][0], c[0][1], c[0][2], c[0][3], c[1][0], c[1][1], c[1][2], c[1][3], c[2][0],
            c[2][1], c[2][2], c[2][3], d.over_life.scale,
        );
    }
    println!("ribbon emitters:    {}", s.ribbon_emitter_count);
    // A keyed look track prints its full `(ms, value)` ramp — the value[0]-only display once
    // masked HolySmite's slash ribbons (height keyed 0 → 0.167 → 0 printed as `+0.00`, reading
    // as "no ribbon" when the model authors a flare).
    let scalar = |t: &benilla_formats::ValueTrack| -> String {
        match t.keys.len() {
            0 | 1 => format!("{:.2}", t.first()),
            _ => {
                let keys: Vec<String> = t
                    .keys
                    .iter()
                    .map(|&(ms, v)| format!("({ms}, {v:.2})"))
                    .collect();
                format!("keys [{}]", keys.join(", "))
            }
        }
    };
    for (i, r) in benilla_formats::parse_m2_ribbon_emitters(bytes)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let rgb = if r.color.keys.len() <= 1 {
            let c = r.color.first();
            format!("[{:.2} {:.2} {:.2}]", c[0], c[1], c[2])
        } else {
            let keys: Vec<String> = r
                .color
                .keys
                .iter()
                .map(|&(ms, c)| format!("({ms}, [{:.2} {:.2} {:.2}])", c[0], c[1], c[2]))
                .collect();
            format!("keys [{}]", keys.join(", "))
        };
        println!(
            "  ribbon {i}  bone {:>3}  {:?}  {:.1} edges/s  life {:.2}s  g {:.2}  tex {}",
            r.bone,
            r.blend,
            r.edges_per_second,
            r.edge_lifetime,
            r.gravity,
            r.texture.as_deref().unwrap_or("NONE (unresolved)"),
        );
        println!(
            "            h above {}  below {}  rgb {}  a {}",
            scalar(&r.height_above),
            scalar(&r.height_below),
            rgb,
            scalar(&r.alpha),
        );
    }
    println!(
        "fully static: {}",
        if s.is_fully_static() { "yes" } else { "no" }
    );
}

/// Dump an M2's animation-channel summary plus texture-transform detail.
pub fn m2anim(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let data = chain
        .read_file(&name)
        .with_context(|| format!("reading '{name}' from chain"))?;
    let summary = benilla_formats::parse_m2_animation_summary(&data)
        .with_context(|| format!("parsing M2 animation summary '{name}'"))?;
    print_m2anim_summary(&summary, &data);

    // Texture-transform detail (0130 phase 3 grounding): the parsed TRS tracks plus the
    // batch → lookup → transform wiring, straight from the full parser.
    let fmt = benilla_m2::parse_m2(&mut std::io::Cursor::new(&data[..]))
        .with_context(|| format!("parsing M2 '{name}'"))?;
    let m = fmt.model();
    if !m.texture_transforms.is_empty() {
        println!("=== texture transforms ===");
        println!("lookup (header 0xac): {:?}", m.texture_transform_lookup);
        for (i, t) in m.texture_transforms.iter().enumerate() {
            println!("  transform {i}:");
            print_txfm_track("translation", &t.translation);
            print_txfm_track("rotation   ", &t.rotation);
            print_txfm_track("scaling    ", &t.scaling);
        }
        if let Ok(skin) = m.parse_embedded_skin(&data, 0) {
            for (bi, batch) in skin.batches().iter().enumerate() {
                println!(
                    "  batch {bi}: txfm combo {} (texture combo {}, material {})",
                    batch.texture_transform_combo_index,
                    batch.texture_combo_index,
                    batch.material_index
                );
            }
        }
    }
    // Color-alpha + texture-weight keys in full (they're tiny scalar tracks): the "how does this
    // effect fade" instrument — the UI cooldown model's finish-flash ramp was pinned from exactly
    // this dump (decision 0137 phase 4).
    if !m.color_alpha_tracks.is_empty() {
        println!("=== color alpha tracks (per M2Color) ===");
        for (i, t) in m.color_alpha_tracks.iter().enumerate() {
            println!("  color {i}: interp {}, keys {:?}", t.interp, t.keys);
        }
    }
    if !m.transparency_tracks.is_empty() {
        println!("=== transparency (texture-weight) tracks ===");
        for (i, t) in m.transparency_tracks.iter().enumerate() {
            println!("  weight {i}: interp {}, keys {:?}", t.interp, t.keys);
        }
    }
    // Bone SCALE keys per sequence — the "how does this element grow" instrument (the cooldown
    // star's finish-flash pulse is a bone-scale curve, decision 0263's INTERIM). Scale-keyed
    // bones only; effect/UI models keep this tiny.
    let seqs = benilla_formats::parse_m2_animations(&data);
    let any_scaled = seqs
        .iter()
        .any(|s| s.bones.iter().any(|b| !b.scale.is_empty()));
    if any_scaled {
        println!("=== bone scale tracks (per sequence; scale-keyed bones only) ===");
        for (si, s) in seqs.iter().enumerate() {
            for (bi, b) in s.bones.iter().enumerate() {
                if b.scale.is_empty() {
                    continue;
                }
                let keys: Vec<String> = b
                    .scale
                    .iter()
                    .map(|(t, v)| format!("{t:.3}s [{:.3} {:.3} {:.3}]", v[0], v[1], v[2]))
                    .collect();
                println!("  seq {si} bone {bi}: {}", keys.join(" -> "));
            }
        }
    }
    Ok(())
}

/// Dump an M2's bone table: KeyBoneID, flags, parent, pivot, and which sequences key each bone.
pub fn m2bones(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let data = chain
        .read_file(&name)
        .with_context(|| format!("reading '{name}' from chain"))?;
    let fmt = benilla_m2::parse_m2(&mut std::io::Cursor::new(&data[..]))
        .with_context(|| format!("parsing M2 '{name}'"))?;
    let m = fmt.model();
    let seqs = benilla_formats::parse_m2_animations(&data);
    println!("idx  keybone  flags       bb  parent  pivot                       keyed (seq[idx] T/R/S counts)");
    for (i, b) in m.bones.iter().enumerate() {
        let keyed: Vec<String> = seqs
            .iter()
            .enumerate()
            .filter_map(|(si, s)| {
                let bk = s.bones.iter().find(|bk| bk.bone as usize == i)?;
                Some(format!(
                    "seq{si}[T{} R{} S{}]",
                    bk.translation.len(),
                    bk.rotation.len(),
                    bk.scale.len()
                ))
            })
            .collect();
        println!(
            "{i:>3}  {:>7}  {:#010x}  {:>2}  {:>6}  ({:>7.3}, {:>7.3}, {:>7.3})  {}",
            b.key_bone,
            b.flags.bits(),
            yn(b.is_billboard()),
            b.parent,
            b.pivot.x,
            b.pivot.y,
            b.pivot.z,
            keyed.join(" "),
        );
        // Small tracks get their actual key values — two sequences can share a key COUNT while
        // holding different values (the questgiver-marker seq 0 vs 190 lesson: counts alone
        // mislabeled them "the same").
        for (si, s) in seqs.iter().enumerate() {
            let Some(bk) = s.bones.iter().find(|bk| bk.bone as usize == i) else {
                continue;
            };
            if !bk.translation.is_empty() && bk.translation.len() <= 8 {
                let keys: Vec<String> = bk
                    .translation
                    .iter()
                    .map(|(t, v)| format!("{t:.3}s ({:.3}, {:.3}, {:.3})", v[0], v[1], v[2]))
                    .collect();
                println!("       seq{si} (anim {}) T: {}", s.anim_id, keys.join("  "));
            }
            // Rotation keys as axis-angle (model-space WoW axes, Z up) — the "which way does
            // this element actually turn" instrument: an emitter/billboard orientation bug
            // needs the spin axis as ground truth, which a bare key COUNT never shows. Small
            // tracks print every key; a long track (a swirl/rotor loop) prints first/last plus
            // the axis of the first key-to-key increment — the spin axis itself.
            if !bk.rotation.is_empty() {
                let aa = |q: &[f32; 4]| {
                    let w = q[3].clamp(-1.0, 1.0);
                    let angle = 2.0 * w.acos();
                    let s = (1.0 - w * w).sqrt();
                    let (x, y, z) = if s < 1e-5 {
                        (0.0, 0.0, 1.0)
                    } else {
                        (q[0] / s, q[1] / s, q[2] / s)
                    };
                    format!("{:+.2}°@({x:+.2},{y:+.2},{z:+.2})", angle.to_degrees())
                };
                if bk.rotation.len() <= 8 {
                    let keys: Vec<String> = bk
                        .rotation
                        .iter()
                        .map(|(t, q)| format!("{t:.3}s {}", aa(q)))
                        .collect();
                    println!("       seq{si} (anim {}) R: {}", s.anim_id, keys.join("  "));
                } else {
                    let (t0, q0) = &bk.rotation[0];
                    let (_t1, q1) = &bk.rotation[1];
                    let (tn, qn) = bk.rotation.last().unwrap();
                    // increment = q1 · q0⁻¹ — its axis is the track's spin axis.
                    let inv0 = [-q0[0], -q0[1], -q0[2], q0[3]];
                    let inc = [
                        q1[3] * inv0[0] + q1[0] * inv0[3] + q1[1] * inv0[2] - q1[2] * inv0[1],
                        q1[3] * inv0[1] - q1[0] * inv0[2] + q1[1] * inv0[3] + q1[2] * inv0[0],
                        q1[3] * inv0[2] + q1[0] * inv0[1] - q1[1] * inv0[0] + q1[2] * inv0[3],
                        q1[3] * inv0[3] - q1[0] * inv0[0] - q1[1] * inv0[1] - q1[2] * inv0[2],
                    ];
                    println!(
                        "       seq{si} (anim {}) R: {} keys  first {t0:.3}s {}  step {}  last {tn:.3}s {}",
                        s.anim_id,
                        bk.rotation.len(),
                        aa(q0),
                        aa(&inc),
                        aa(qn),
                    );
                }
            }
        }
    }
    eprintln!("{} bones", m.bones.len());
    Ok(())
}

/// Dump an M2's render batches as the renderer sees them.
pub fn m2batch(chain: &mut Chain, internal_path: &str) -> Result<()> {
    let name = normalize(internal_path);
    let data = chain
        .read_file(&name)
        .with_context(|| format!("reading '{name}' from chain"))?;
    let dir = name.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
    let subs = benilla_formats::parse_m2_render_submeshes(&data, dir, &[])
        .with_context(|| format!("parsing M2 render submeshes '{name}'"))?;
    println!("{} render batch(es)", subs.len());
    for (i, s) in subs.iter().enumerate() {
        let mut flags = Vec::new();
        if s.emissive {
            flags.push("emissive");
        }
        if s.additive {
            flags.push("additive");
        }
        if s.two_sided {
            flags.push("two-sided");
        }
        if s.no_depth_write {
            flags.push("no-depth-write");
        }
        if s.no_depth_test {
            flags.push("no-depth-test");
        }
        if s.billboard.is_some() {
            flags.push("BILLBOARD");
        }
        if s.alpha_anim.is_some() {
            flags.push("alpha-anim");
        }
        if s.uv_anim.is_some() {
            flags.push("uv-anim");
        }
        // A character runtime slot (body atlas / hair / object / extra skin) has no embedded path —
        // name the slot rather than a misleading bare NONE.
        let tex = match (&s.texture, s.char_slot) {
            (Some(t), _) => t.clone(),
            (None, Some(slot)) => format!("<char:{slot:?}>"),
            (None, None) => "NONE".into(),
        };
        println!(
            "  batch {i}: geoset {:>4}  {:?}  {} verts  [{}]  tex {}",
            s.geoset_id,
            s.blend,
            s.positions.len(),
            flags.join(" "),
            tex,
        );
    }
    Ok(())
}
