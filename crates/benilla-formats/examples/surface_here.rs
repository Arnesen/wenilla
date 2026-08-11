//! What surface the footstep chain resolves under a patch of world:
//! `cargo run -p benilla-formats --example surface_here -- <map> <x> <y> [radius_yd] [class]`
//! e.g. `surface_here Azeroth -5595.89 -529.48 24`.
//!
//! A footstep bug is reported as a *sound* ("I'm walking on snow indoors") but it is decided far
//! upstream, by which surface the down-ray claims under the foot. This prints that claim as a
//! top-down map — north up, west left — so the answer is a picture of the ground rather than an
//! impression of the audio: per cell the `TerrainType` the chain lands on, and at the pin the full
//! walk through to the `SoundEntries` kit that would actually play.
//!
//! **This is the ADT leg only** — deliberately. The client (and, since decision 1161, benilla)
//! arbitrates this against a WMO probe and the nearer surface wins, so inside a building the live
//! answer comes from the floor, not from here. What this map shows indoors is the ground *under*
//! that floor: the counterfactual. That is what makes a wrong-surface report legible — B236's
//! sequel was "snow indoors", and this printed 1681 of 1681 cells Snow beneath the Kharanos inn.
//! To see which leg actually answered in a live run, use `RUST_LOG=benilla_app::sound=debug` and
//! read the `footstep: wmo|adt terrain N` line.
//!
//! Output is Blizzard data — pipe it to the scratchpad, never into the repo.

use std::collections::HashMap;

/// `TerrainType.SoundID` → its authored name and map glyph. The domain is closed and verified
/// against build 5875 (`benilla_formats::footsteps` module docs): the eleven rows collapse to ten
/// sound classes, `DustyGrass` sharing `Grass`'s.
fn class_glyph(sound_class: u32) -> (char, &'static str) {
    match sound_class {
        0 => ('.', "None"),
        1 => ('d', "Dirt"),
        2 => ('m', "Metallic"),
        3 => ('S', "Stone"),
        4 => ('*', "Snow"),
        5 => ('w', "Wood"),
        6 => ('g', "Grass"),
        7 => ('l', "Leaves"),
        8 => ('n', "Sand"),
        9 => ('q', "Soggy"),
        _ => ('?', "unknown"),
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let usage =
        || anyhow::anyhow!("usage: surface_here <map> <x> <y> [radius_yd] [footstep_class]");
    let map = args.next().ok_or_else(usage)?;
    let x: f32 = args.next().ok_or_else(usage)?.parse()?;
    let y: f32 = args.next().ok_or_else(usage)?.parse()?;
    let radius: f32 = args.next().map_or(Ok(20.0), |r| r.parse())?;
    // 7 is the humanoid/character class (its ten rows are the `CharacterMediumLarge*` kits); 8 is
    // the small-character class. Both reach it as ordinary data, never a code default.
    let class: u32 = args.next().map_or(Ok(7), |c| c.parse())?;

    let data = benilla_formats::wow_data().expect("no WoW install found (set $WOW_DATA)");
    let mut chain = benilla_formats::open_chain(&data)?;
    let cat = benilla_formats::load_footstep_catalog(&mut chain)?;
    let tiles = benilla_formats::MapTiles::load(&mut chain, &map)?;

    // One sample per cell of a square centred on the pin, north (+x) up and west (+y) left — the
    // orientation the in-game map uses, so a spot on this grid is a spot on their screen.
    let cols = 41usize;
    let step = (radius * 2.0) / (cols - 1) as f32;
    let mut meshes: HashMap<(u32, u32), Option<benilla_formats::TileMesh>> = HashMap::new();
    let mut census: HashMap<Option<u32>, usize> = HashMap::new();

    let sample = |chain: &mut _,
                  meshes: &mut HashMap<(u32, u32), Option<benilla_formats::TileMesh>>,
                  sx: f32,
                  sy: f32|
     -> Option<u32> {
        let (tx, ty) = tiles.tile_at(sx, sy);
        let mesh = meshes
            .entry((tx, ty))
            .or_insert_with(|| benilla_formats::load_tile_mesh(chain, &map, tx, ty).ok());
        let mesh = mesh.as_ref()?;
        benilla_formats::ground_effect_at(&mesh.chunks, [sx, sy, 0.0])
    };

    println!("{map}: footstep surface around ({x}, {y}), r={radius} yd, {step:.2} yd/cell");
    println!("(ADT leg only — inside a building this is the ground UNDER the floor)\n");

    let mut rows = Vec::with_capacity(cols);
    for r in 0..cols {
        let sx = x + radius - r as f32 * step;
        let mut line = String::with_capacity(cols);
        for c in 0..cols {
            let sy = y + radius - c as f32 * step;
            let effect = sample(&mut chain, &mut meshes, sx, sy);
            let terrain = effect.and_then(|e| cat.terrain_of(e));
            *census.entry(terrain).or_default() += 1;
            let glyph = match terrain.and_then(|t| cat.sound_class_of(t)) {
                Some(sc) => class_glyph(sc).0,
                // No effect layer / unknown terrain — the client's −1 sentinel, a SILENT footfall.
                None => ' ',
            };
            line.push(glyph);
        }
        rows.push(line);
    }
    for (r, line) in rows.iter().enumerate() {
        let mark = if r == cols / 2 { " <- pin row" } else { "" };
        println!("  {line}{mark}");
    }
    println!("\n  {:>width$}^ pin column", "", width = cols / 2 + 2);

    let mut census: Vec<_> = census.into_iter().collect();
    census.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    println!("\ncensus over {} cells:", cols * cols);
    for (terrain, n) in census {
        let label = match terrain {
            Some(t) => {
                let sc = cat.sound_class_of(t);
                let name = sc.map_or("?", |sc| class_glyph(sc).1);
                format!("TerrainType {t} ({name}, sound class {})", sc.unwrap_or(0))
            }
            None => "no effect layer (silent footfall)".to_string(),
        };
        println!("  {n:5}  {label}");
    }

    // The pin itself, walked end to end — the line a bug report can be checked against.
    let effect = sample(&mut chain, &mut meshes, x, y);
    println!("\nat the pin exactly:");
    println!("  GroundEffectTexture: {effect:?}");
    match effect.and_then(|e| cat.terrain_of(e)) {
        Some(t) => {
            let sc = cat.sound_class_of(t);
            println!(
                "  TerrainType: {t} ({}), SoundID {:?}",
                sc.map_or("?", |sc| class_glyph(sc).1),
                sc
            );
        }
        None => println!("  TerrainType: none — silent footfall"),
    }
    let kits = benilla_formats::load_sound_kit_catalog(&mut chain)?;
    let name = |id: u32| kits.get(id).map_or("?".into(), |k| k.name.clone());
    match cat.resolve(class, effect) {
        Some((dry, splash)) => println!(
            "  class {class} kits: dry {dry} {}, splash {splash} {}",
            name(dry),
            name(splash)
        ),
        None => println!("  class {class}: no lookup row — silent"),
    }
    println!("  leaves footprints: {}", cat.leaves_footprints(effect));
    Ok(())
}
