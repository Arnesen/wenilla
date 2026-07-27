//! `benilla-extract` — Phase 1 CLI over WoW 1.12.1 (build 5875) MPQ archives.
//!
//! Implemented: `list`, `extract` (raw bytes), operating on the **patch chain**
//! (later archives override earlier; real names come from patch.MPQ's `(listfile)`).
//! BLP->PNG and DBC->CSV come next.
//!
//! This file is the stable face: the clap [`Cli`]/[`Command`] definitions, the dispatch `match`,
//! and the tiny helpers shared across concerns. The heavier subcommands live in sibling modules by
//! concern: per-model M2 dump printers ([`m2dump`]), corpus-scan reports ([`scan`]), and the spell
//! visual chain ([`spellvis`]).

use std::path::PathBuf;

use anyhow::{Context, Result};
use benilla_formats::open_chain;
use clap::{Parser, Subcommand};

mod m2dump;
mod scan;
mod spellvis;

/// Read WoW 1.12.1 asset archives (MPQ).
#[derive(Parser)]
#[command(
    name = "benilla-extract",
    version,
    about,
    allow_negative_numbers = true
)]
struct Cli {
    /// A single `.MPQ` file, or a vanilla `Data` directory (opens the full patch chain).
    path: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List files across the chain, sorted by name.
    List,
    /// Extract one file by its internal path, writing raw bytes to `output`.
    Extract {
        /// Internal path inside the archive (forward or back slashes accepted).
        internal_path: String,
        /// Output file to write the extracted bytes to.
        output: PathBuf,
    },
    /// Decode a BLP texture from the archive and write it as a PNG.
    Blp {
        /// Internal path to the `.blp` (forward or back slashes accepted).
        internal_path: String,
        /// Output `.png` file.
        output: PathBuf,
    },
    /// Dump a DBC table to CSV using our schema for it (headers included).
    Dbc {
        /// Internal path to the `.dbc` (forward or back slashes accepted).
        internal_path: String,
        /// Output `.csv` file.
        output: PathBuf,
    },
    /// Dump a spell's visual chain: spell → SpellVisual stages → each kit's anim/sound (decision
    /// 0099's phase-2 instrument; columns per decision 0107).
    Spellvis {
        /// The `Spell.dbc` id (e.g. 133 = Fireball).
        spell_id: u32,
    },
    /// Dump an M2's collision hull as the mover collides with it: vertex/triangle counts, the
    /// model-space AABB (WoW axes, Z up), and its extents — the "what does walking into this
    /// actually hit" instrument (the step-up climb-vs-slide asset question, decision 0195; a
    /// placement's own scale still multiplies these numbers).
    M2coll {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's animation sequences in file order: `AnimationData.dbc` id, loop/clamp flag,
    /// duration, variation frequency, replay range — the variation/replay instrument
    /// (decisions 0114/0117; sequences sharing an id are its variation chain).
    M2seq {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's animation EVENT keyframes per sequence (`$CSS`/`$CAH`/`$AH0-3`/`$CPP`/`$HIT`…,
    /// time + payload) — the event-order instrument (decision 0279: whether `$CPP` precedes the
    /// impact tag decides defense-anim vs flinch on the shared swing record).
    M2events {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's attachment points (id + bone) — which sheath/held/effect anchors a model
    /// actually has (decisions 0072/0122; a placement whose id is absent hangs nothing).
    M2attach {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's animation-channel summary: sequence-0 bone motion, global-sequence bone
    /// channels, transparency/color track key counts, texture transforms, and — per particle
    /// emitter — its full def: shape, blend mode, head/tail, emission-rate keys (burst emitters
    /// key `0 → peak → 0`), lifespan/speed/scale, position/area, resolved texture, and the
    /// over-life color/alpha/size ramps. The one-command diagnosis for "this effect looks wrong /
    /// doesn't show" (decisions 0130/0141); paired with `doodadscan`.
    M2anim {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's bone table: KeyBoneID, flags (billboard bits tagged), parent, pivot, and which
    /// sequences key each bone (T/R/S key counts) — the bone-attach / billboard-geometry instrument
    /// (decisions 0028/0122: which bone a rider actually rides, and whether it faces the camera).
    M2bones {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's render batches as the renderer sees them: geoset, blend/flags
    /// (emissive/additive/two-sided/billboard/depth), texture, vertex count, and whether an
    /// alpha/UV animation baked — the "why doesn't this part of the model draw (or draw wrong)"
    /// instrument (the Orgrimmar-bonfire missing-flame diagnosis).
    M2batch {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Dump an M2's **per-sequence material alpha**: every colour-alpha/transparency track's raw
    /// keys, then the combined per-batch factor for EVERY sequence band — the "which batches does
    /// the reference hide, and in which animation" instrument. A `HIDE` cell is a batch the real
    /// client skips outright in that sequence (`A <= 0` culls before the blend mode is read,
    /// wow-re `m2-alpha-combine-cull.md`). Pair with `m2batch` (the batch -> track wiring) and
    /// `m2seq` (the bands).
    M2alpha {
        /// Internal path to the `.m2` (forward or back slashes accepted).
        internal_path: String,
    },
    /// Sweep every `.m2` in the chain and list the models carrying RIBBON emitters (header
    /// `0x134`) — the population instrument for the ribbon subsystem (weapon trails, streamers):
    /// which content actually authors one, before we build for it.
    Ribbonscan,
    /// Sweep every `.m2` and census its particle emitters' over-life **flipbook** fields: cell
    /// ramps that play BACKWARDS (`begin > end` — legal, shipped, and fatal to a reader that
    /// clamps into the pair), tail streaks whose ramp differs from the head's, indices past the
    /// atlas (the reference wraps the column and lets the row run off), per-segment repeat counts,
    /// and the two degenerate shapes the reference itself falls back on. Decision 0685.
    Cellscan,
    /// Sweep every `.m2` (optionally under a path prefix) and list the models whose MATERIAL
    /// table authors the MULTIPLY blend modes 5 (Mod) / 6 (Mod2x) — the ARMORREFLECT sheen
    /// family (decision 0528): the population instrument for the multiply-blend mechanism.
    Blendscan {
        /// Internal-path prefix filter (e.g. `item\objectcomponents\weapon`), case-insensitive;
        /// all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and classify its BILLBOARD usage:
    /// which arms it authors (spherical / lock-X / lock-Y / lock-Z), and whether geometry rides
    /// them DIRECTLY (verts skinned to the billboard bone — the per-batch card path) or
    /// INHERITED (verts on a descendant — the joint-palette path, decision 0205). The
    /// population instrument for the billboard mechanism: which arms real content exercises, so
    /// a class of spell visuals is closed by mechanism instead of tested spell-by-spell.
    Bbscan {
        /// Internal-path prefix filter (e.g. `spells`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and classify each BILLBOARD batch by
    /// which way its geometry FACES. A billboard bone puts the model's +X toward the viewer, so a
    /// batch's winding normal decides whether it is ever seen: +X faces the camera, −X faces away
    /// and — single-sided — is backface-culled by the reference from every angle. The population
    /// instrument for "the card renders but shouldn't": the away+single-sided list is exactly the
    /// authored placeholder geometry the reference hides and a forced-two-sided renderer reveals.
    Bbfacescan {
        /// Internal-path prefix filter (e.g. `world\generic`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and report models that author flat
    /// ground-plane render geometry: batches whose vertices all sit at model-space z≈0 (WoW axes,
    /// Z up), which sloped terrain buries (Battle Shout's crescents are the canonical case — 6
    /// batches, each a 4-vert quad, every vertex exactly z=0, each quad skinned 100% to a single
    /// bone). Flat batches sub-classify QUAD-1BONE (that exact crescent shape) vs OTHER-FLAT — the
    /// population instrument for how general the ground-plane renderer mechanism must be.
    Groundscan {
        /// Internal-path prefix filter (e.g. `spells`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and census the geometry a
    /// **non-character** spawn draws that the reference may not: MULTI-GEOSET models (more than
    /// one `skinSectionId` — only the character compositor selects among them, so every other
    /// spawn path draws all of them), UNTEX batches (no embedded texture and no character runtime
    /// slot) and TINY batches (at most 2 faces). The population instrument behind the stray
    /// untextured-primitive reports; `m2batch` then explains a single model in full.
    Geosetscan {
        /// Internal-path prefix filter (e.g. `creature`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every model named by **GameObjectDisplayInfo.dbc** and resolve what the reference's
    /// GameObject animation arm plays in each reachable `GAMEOBJECT_STATE` × `GAMEOBJECT_ANIMPROGRESS`
    /// substate (wow-re `gameobject-anim-arm.md` §2b/§2c: the substate table, the `0x8607e4` LUT, the
    /// four-way missing-sequence remap) — beside the generic loader seed the same model gets at build
    /// (§1, animation id 0 through `playableAnimationLookup`). The population instrument for "which
    /// GameObjects can the wire state actually be SEEN on": a model whose every substate lands on the
    /// seed's own sequence is state-blind, so skipping the arm on its GO type costs nothing, while a
    /// STATE-SENSITIVE model renders in the wrong pose the moment its type is left off the machine.
    /// Also counts the models whose pose depends on the §2c remap (benilla plays nothing there today,
    /// i.e. bind pose) and the ones reaching a rate-0 freeze leg.
    Goanimscan,
    /// Sweep every `.m2` (optionally under a path prefix) and census the models whose batch
    /// visibility is PER SEQUENCE — geometry the reference draws in one animation and skips in
    /// another (the verified `A <= 0` alpha cull). The population instrument for "a single-sequence
    /// material bake draws the wrong set"; `m2alpha` then explains one model in full.
    Alphascan {
        /// Internal-path prefix filter (e.g. `creature`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and measure how far our SKELETAL parse
    /// sits from the reference's sampler, per bone track per sequence band: empty bands (our
    /// nearest-key clamp vs the file's own `interpolation_ranges` window), held band edges (our
    /// hold vs the reference's ongoing lerp toward the next key), and STEP tracks (`interp == 0`,
    /// which the reference copies and we interpolate). The population instrument behind decision
    /// 0133's named residual — and the safety check that a band's keys never fall outside the
    /// window the reference searches.
    Bonescan {
        /// Internal-path prefix filter (e.g. `creature`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and census every particle-emitter
    /// FEATURE the corpus authors — shapes, head/tail, blend modes (incl. the folded Mod/Mod2x),
    /// every file-flag bit, spin signs, twinkle gates, atlas tiling, spline emitters, and the
    /// record fields the renderer doesn't parse yet (geometry-model "model particles",
    /// recursion-model child emitters, the rate track's interp word) — each with counts, example
    /// models, and the spells whose visual chain plays them. The population instrument that turns
    /// "this spell looks wrong" reports into a ranked mechanism worklist, closing feature classes
    /// corpus-wide instead of spell-by-spell.
    Partcensus {
        /// Internal-path prefix filter (e.g. `spells`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and list the models whose PARTICLE
    /// emitters carry any of the given file-flag bits (`M2ParticleEmitter+0x04`) — the
    /// population instrument for particle-flag mechanisms (which content actually authors
    /// e.g. the `0x1000` plane-quad leg), so a class of effects is closed by mechanism
    /// instead of tested spell-by-spell.
    Partscan {
        /// The flag mask to match, hex or decimal (e.g. `0x1000`).
        mask: String,
        /// Internal-path prefix filter (e.g. `spells`), case-insensitive; all models if omitted.
        prefix: Option<String>,
    },
    /// Sweep every `.m2` (optionally under a path prefix) and report which models author M2
    /// dynamic LIGHT blocks — the population instrument for the mechanism (decision 0016 / wow-re
    /// `system/models/scratch/m2-dynamic-lights.md`). Per model: its point (`type==1`, the GL
    /// hot-spot caster) vs directional (ambient-feed) light counts, then per point light its
    /// bone/position/diffuse colour×intensity/attenuation/visibility-gate. The closing summary —
    /// totals, a breakdown by top-level content family (Creature/Item.ObjectComponents/
    /// Character/World/Spells/other), and a diffuse-palette tally — is the real deliverable:
    /// benilla only spawns these lights for ADT-placed doodads and WMO props today, so it answers
    /// how much of the entity path (creatures, held items, GameObjects) is missing them.
    M2lightscan {
        /// Internal-path prefix filter (e.g. `item\objectcomponents`), case-insensitive; all
        /// models if omitted.
        prefix: Option<String>,
    },
    /// Dump all 18 `LightIntBand` rows of the `Light.dbc` entry covering a world position at a
    /// time of day — the band-semantics instrument (which row holds which colour at which hour;
    /// the celestial-diffuse band question, decision 0485).
    Lightbands {
        /// Map id (0 = Eastern Kingdoms, 1 = Kalimdor).
        map: u32,
        /// World-space X (raw WoW yards; may be negative).
        #[arg(allow_hyphen_values = true)]
        x: f32,
        /// World-space Y.
        #[arg(allow_hyphen_values = true)]
        y: f32,
        /// World-space Z.
        #[arg(allow_hyphen_values = true)]
        z: f32,
        /// Game minute-of-day (0..1440; 720 = noon).
        minute: u32,
    },
    /// Dump every `LightIntBand`/`LightFloatBand` row of an explicitly named `LightParams` id — the
    /// instrument for rows **no position can reach**, because nothing in `Light.dbc` references them:
    /// magma submersion reads the fixed global row 7 and slime row 6 (VERIFIED `0x6d2371`), not the
    /// zone's underwater slot.
    Lightparam {
        /// `LightParams.dbc` id (1-based).
        id: u32,
        /// Game minute-of-day (0..1440; 720 = noon).
        minute: u32,
    },
    /// Dump the **per-weather-slot** light resolve at a world position: which `LightParams` id each
    /// of the five `Light.dbc` slots names (clear / clear-underwater / storm / storm-underwater /
    /// death), which of them are UNSET (and so fall back to clear), and the fog/ambient/diffuse the
    /// blend resolves for each. `lightbands` only ever shows the clear slot, so an underwater or
    /// ghost atmosphere that reads wrong could not be told from data that simply has no such record.
    Lightslots {
        /// Map id (0 = Eastern Kingdoms, 1 = Kalimdor).
        map: u32,
        /// World-space X (raw WoW yards; may be negative).
        #[arg(allow_hyphen_values = true)]
        x: f32,
        /// World-space Y.
        #[arg(allow_hyphen_values = true)]
        y: f32,
        /// World-space Z.
        #[arg(allow_hyphen_values = true)]
        z: f32,
        /// Game minute-of-day (0..1440; 720 = noon).
        minute: u32,
    },
    /// Bulk-scan placed doodads (MDDF) and WMO doodad-set-0 props (MODF → MODS/MODD) across a
    /// `(2·tile_radius+1)²` block of ADT tiles around a world position, and report how much of that
    /// content animates (see `m2anim`) — the doodad-animation perf/scope instrument.
    Doodadscan {
        /// Map directory name (e.g. `Azeroth`).
        map: String,
        /// World-space center X (may be negative).
        #[arg(allow_hyphen_values = true)]
        center_x: f32,
        /// World-space center Y (may be negative).
        #[arg(allow_hyphen_values = true)]
        center_y: f32,
        /// ADT tile radius around the center tile to scan (0 = just the containing tile).
        tile_radius: u32,
    },
    /// Dump a WMO root's placed-prop tables: every MODD doodad with its MODS set membership and
    /// its OWNING group(s) read from the group files' MODR lists — the relation the reference
    /// instantiates from (a *visible* group's own refs, `0x695aa0`), so a prop referenced by NO
    /// group is one the real client never creates at all (the divergence decision 0689 names,
    /// which benilla still spawns). The "which props exist in this building, who owns them, and
    /// which would the reference even draw" instrument.
    Wmodoodads {
        /// Internal path to the WMO **root** (forward or back slashes accepted).
        internal_path: String,
        /// Case-insensitive substring of the prop's model path (e.g. `lightray`); all if omitted.
        filter: Option<String>,
    },
    /// List individual doodad (MDDF) and WMO (MODF) placements around a world position whose
    /// model path contains a substring — position, Euler rotation (deg), scale, uniqueId. The
    /// "point at a thing in the world, tell me exactly how it's placed" instrument: an
    /// orientation bug needs the placement rotation as ground truth, which `doodadscan`'s
    /// aggregates never show.
    Placescan {
        /// Map directory name (e.g. `Azeroth`).
        map: String,
        /// World-space center X (may be negative).
        #[arg(allow_hyphen_values = true)]
        center_x: f32,
        /// World-space center Y (may be negative).
        #[arg(allow_hyphen_values = true)]
        center_y: f32,
        /// ADT tile radius around the center tile to scan (0 = just the containing tile).
        tile_radius: u32,
        /// Case-insensitive substring of the model path (e.g. `instanceportal`).
        filter: String,
    },
}

/// MPQ internal paths are backslash-separated; accept `/` for convenience.
fn normalize(path: &str) -> String {
    path.replace('/', "\\")
}

/// The grouping/lookup key for a model path referenced from MDDF/MODD (`doodadscan`'s instance
/// tables): lowercase, `.mdx`/`.mdl` → `.m2`. Mirrors `benilla_formats`' own (crate-private)
/// `model_path` normalization so two placements of the same model authored under different
/// casing/extension collapse to one row instead of double-counting it.
fn model_key(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    match lower
        .strip_suffix(".mdx")
        .or_else(|| lower.strip_suffix(".mdl"))
    {
        Some(stem) => format!("{stem}.m2"),
        None => lower,
    }
}

/// Parse a `0x`-prefixed hex or plain-decimal u32 (`partscan`'s mask argument).
fn parse_u32_maybe_hex(s: &str) -> Result<u32> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).context("invalid hex"),
        None => s.parse().context("invalid decimal"),
    }
}

/// `"yes"`/`"-"` for a compact table cell.
fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "-"
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut chain = open_chain(&cli.path)?;

    match cli.command {
        Command::List => {
            let mut files = chain.list().context("listing chain contents")?;
            files.sort_by(|a, b| a.name.cmp(&b.name));
            for entry in &files {
                println!("{:>12}  {}", entry.size, entry.name);
            }
            eprintln!("{} files", files.len());
        }
        Command::Extract {
            internal_path,
            output,
        } => {
            let name = normalize(&internal_path);
            // Note which archive the file resolved from (handy for debugging overrides).
            let source = chain
                .find_file_archive(&name)
                .map(|p| p.display().to_string());
            let data = chain
                .read_file(&name)
                .with_context(|| format!("reading '{name}' from chain"))?;
            std::fs::write(&output, &data)
                .with_context(|| format!("writing {}", output.display()))?;
            match source {
                Some(src) => eprintln!(
                    "wrote {} bytes to {} (from {})",
                    data.len(),
                    output.display(),
                    src
                ),
                None => eprintln!("wrote {} bytes to {}", data.len(), output.display()),
            }
        }
        Command::Blp {
            internal_path,
            output,
        } => {
            let name = normalize(&internal_path);
            let data = chain
                .read_file(&name)
                .with_context(|| format!("reading '{name}' from chain"))?;
            let (w, h) = benilla_formats::blp_to_png(&data, &output)
                .with_context(|| format!("decoding BLP '{name}'"))?;
            eprintln!("decoded {w}x{h} -> {}", output.display());
        }
        Command::Dbc {
            internal_path,
            output,
        } => {
            let name = normalize(&internal_path);
            let data = chain
                .read_file(&name)
                .with_context(|| format!("reading '{name}' from chain"))?;
            let (records, fields) = benilla_formats::dbc_to_csv(&data, &name, &output)?;
            eprintln!(
                "wrote {records} records ({fields} fields) -> {}",
                output.display()
            );
        }
        Command::M2coll { internal_path } => m2dump::m2coll(&mut chain, &internal_path)?,
        Command::M2seq { internal_path } => m2dump::m2seq(&mut chain, &internal_path)?,
        Command::M2events { internal_path } => m2dump::m2events(&mut chain, &internal_path)?,
        Command::M2attach { internal_path } => m2dump::m2attach(&mut chain, &internal_path)?,
        Command::M2anim { internal_path } => m2dump::m2anim(&mut chain, &internal_path)?,
        Command::M2bones { internal_path } => m2dump::m2bones(&mut chain, &internal_path)?,
        Command::M2batch { internal_path } => m2dump::m2batch(&mut chain, &internal_path)?,
        Command::M2alpha { internal_path } => m2dump::m2alpha(&mut chain, &internal_path)?,
        Command::Ribbonscan => scan::ribbonscan(&mut chain)?,
        Command::Cellscan => scan::cellscan(&mut chain)?,
        Command::Blendscan { prefix } => scan::blendscan(&mut chain, prefix.as_deref())?,
        Command::Bbscan { prefix } => scan::bbscan(&mut chain, prefix.as_deref())?,
        Command::Bbfacescan { prefix } => scan::bbfacescan(&mut chain, prefix.as_deref())?,
        Command::Groundscan { prefix } => scan::groundscan(&mut chain, prefix.as_deref())?,
        Command::Geosetscan { prefix } => scan::geosetscan(&mut chain, prefix.as_deref())?,
        Command::Alphascan { prefix } => scan::alphascan(&mut chain, prefix.as_deref())?,
        Command::Goanimscan => scan::goanimscan(&mut chain)?,
        Command::Bonescan { prefix } => scan::bonescan(&mut chain, prefix.as_deref())?,
        Command::Partcensus { prefix } => scan::partcensus(&mut chain, prefix.as_deref())?,
        Command::Partscan { mask, prefix } => {
            let mask = parse_u32_maybe_hex(&mask)
                .with_context(|| format!("parsing flag mask '{mask}'"))?;
            scan::partscan(&mut chain, mask, prefix.as_deref())?;
        }
        Command::M2lightscan { prefix } => scan::m2lightscan(&mut chain, prefix.as_deref())?,
        Command::Lightbands {
            map,
            x,
            y,
            z,
            minute,
        } => {
            let catalog = benilla_formats::LightCatalog::load(&mut chain)?;
            // `debug_bands` takes the client's half-minute clock (0..2880).
            catalog.debug_bands(map, [x, y, z], minute * 2);
        }
        Command::Lightparam { id, minute } => {
            let catalog = benilla_formats::LightCatalog::load(&mut chain)?;
            catalog.debug_param(id, minute * 2);
        }
        Command::Lightslots {
            map,
            x,
            y,
            z,
            minute,
        } => {
            let catalog = benilla_formats::LightCatalog::load(&mut chain)?;
            catalog.debug_slots(map, [x, y, z], minute * 2);
        }
        Command::Wmodoodads {
            internal_path,
            filter,
        } => scan::wmodoodads(&mut chain, &internal_path, filter.as_deref())?,
        Command::Placescan {
            map,
            center_x,
            center_y,
            tile_radius,
            filter,
        } => scan::placescan(&mut chain, &map, center_x, center_y, tile_radius, &filter)?,
        Command::Doodadscan {
            map,
            center_x,
            center_y,
            tile_radius,
        } => scan::doodadscan(&mut chain, &map, center_x, center_y, tile_radius)?,
        Command::Spellvis { spell_id } => spellvis::run(&mut chain, spell_id)?,
    }

    Ok(())
}
