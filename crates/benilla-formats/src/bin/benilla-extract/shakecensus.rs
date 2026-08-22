//! `shakecensus`: the whole-table view of the **camera-shake** system (decision 1540) — the 24
//! shipped `CameraShakes.dbc` presets, and every creature model that names one.
//!
//! The scope instrument for B298 ("walking past an Ancient Protector shakes no screen"). It
//! answers, from the shipped data rather than from expectation, the two questions that turn that
//! one report into a system: *which* creatures are affected (not just the Ancients — every kodo,
//! giant, titan and dragon in the game), and whether `CreatureModelData` fields 11/12 really are
//! `CameraShakes` keys — because if they are, every live value must land on a real row of a
//! 24-row table, and the amplitudes must rank by mass. A column map that were merely plausible
//! would scatter across the id space and hit holes.
//!
//! Read the output beside the module doc of [`benilla_formats::CameraShakeCatalog`]: the creature
//! rows and the spell rows are visibly different shapes (phase, duration, the direction triples),
//! and that split is itself the check.

use std::collections::BTreeMap;

use anyhow::Result;
use benilla_formats::Chain;

/// Which creature models name a preset, and through which column.
#[derive(Default)]
struct Users {
    footstep: Vec<String>,
    death_thud: Vec<String>,
}

/// The model's own file name — the census reads better without the `Creature\\Foo\\` prefix.
fn short(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or(path).to_string()
}

pub fn shakecensus(chain: &mut Chain) -> Result<()> {
    let shakes = benilla_formats::load_camera_shakes(chain)?;
    let creatures = benilla_formats::load_creature_catalog(chain)?;

    let mut models: Vec<(u32, String, u32, u32)> = creatures
        .shaking_models()
        .map(|(id, path, f, t)| (id, path.to_string(), f, t))
        .collect();
    models.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

    let mut users: BTreeMap<u32, Users> = BTreeMap::new();
    for (model_id, path, footstep, thud) in &models {
        let label = format!("{} ({model_id})", short(path));
        if *footstep != 0 {
            users
                .entry(*footstep)
                .or_default()
                .footstep
                .push(label.clone());
        }
        if *thud != 0 {
            users.entry(*thud).or_default().death_thud.push(label);
        }
    }

    println!("CameraShakes.dbc — {} presets\n", shakes.len());
    println!(
        "{:>4}  {:>4} {:>4}  {:>9} {:>9} {:>8} {:>7} {:>7}  named by",
        "id", "type", "dir", "amplitude", "frequency", "duration", "phase", "coeff"
    );
    let mut rows: Vec<_> = shakes.iter().collect();
    rows.sort_by_key(|r| r.id);
    for r in rows {
        let named = match users.get(&r.id) {
            None => "—  (spell-side or unused)".to_string(),
            Some(u) => {
                let mut parts = Vec::new();
                if !u.footstep.is_empty() {
                    parts.push(format!("footstep: {}", u.footstep.join(", ")));
                }
                if !u.death_thud.is_empty() {
                    parts.push(format!("death thud: {}", u.death_thud.join(", ")));
                }
                parts.join("  ·  ")
            }
        };
        println!(
            "{:>4}  {:>4} {:>4}  {:>9.3} {:>9.3} {:>8.3} {:>7.3} {:>7.3}  {}",
            r.id,
            r.shake_type,
            r.direction,
            r.amplitude,
            r.frequency,
            r.duration,
            r.phase,
            r.coefficient,
            named
        );
    }

    // Every value the creature columns actually carry must resolve — a miss would say the column
    // map is wrong, which is the whole point of running this.
    let mut dangling = Vec::new();
    for (model_id, path, footstep, thud) in &models {
        for (col, id) in [
            ("FootstepShakeSize", footstep),
            ("DeathThudShakeSize", thud),
        ] {
            if *id != 0 && shakes.get(*id).is_none() {
                dangling.push(format!("{path} ({model_id}): {col} = {id}"));
            }
        }
    }
    println!(
        "\n{} of the shipped creature models name a shake; {} dangling id(s)",
        models.len(),
        dangling.len()
    );
    for d in &dangling {
        println!("  DANGLING {d}");
    }
    Ok(())
}
