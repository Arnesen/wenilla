//! `kitanim`: census the `SpellVisualKit` **animation** column (field 2) — the half of a kit that
//! plays a clip on the unit's *own body*, as opposed to the attach-point effect models
//! (`spellvis`), the CharProcs (`charprocs`) or the camera shake (`shakecensus`).
//!
//! The scope instrument for "the spell landed on me and my character did nothing". A kit's anim is
//! the only part of a visual that can be *invisible by omission*: an effect model that fails to
//! spawn leaves a trace line, while an anim that is never asked for, or is asked for and instantly
//! overwritten, leaves nothing at all. So the population has to be read off the shipped table:
//!
//! - **by stage**, because the stage decides the lifetime. A `cast`/`impact` anim is a one-shot on
//!   the caster/victim; a **`state`** anim belongs to an aura's whole life, which is a different
//!   consumer entirely (`creature_anim::spell_visual::arm_aura_state_fx`'s slot watcher, not
//!   `play_impact`).
//! - **anim-only kits are called out**, because they are the class an "does this kit do anything?"
//!   test drops. That test is the B114 shape: the aura-state watcher used to arm only on effect
//!   models, so Stealth — whose whole visual is one CharProc — showed nothing. A state kit whose
//!   whole visual is an *anim* fails the same way one level over.
//! - **wound-branch ids are marked**, because 8/9/10 (`StandWound`/`CombatWound`/`CombatCritical`)
//!   do not play as themselves: the client's kit player hands them to the severity-0 flinch
//!   (`0x60f3b8: push 0; call 0x60ea70`), so they are a different mechanism wearing the same
//!   column.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use benilla_formats::{AnimDataCatalog, Chain, VisualStages};

/// A `SpellVisual` stage's column selector — one of the five lifecycle-kit fields.
type StagePick = fn(&VisualStages) -> u32;

/// The five `SpellVisual` lifecycle stages, in field order — the label plus its column selector.
const STAGES: [(&str, StagePick); 5] = [
    ("precast", |s| s.precast),
    ("cast", |s| s.cast),
    ("impact", |s| s.impact),
    ("state", |s| s.state),
    ("channel", |s| s.channel),
];

/// The kit-player's wound branch: ids 8–10 are handed to the severity-0 flinch instead of being
/// played as themselves (`0x60f3b8`), so they are counted apart from the real body clips.
fn is_wound_id(id: u16) -> bool {
    (8..=10).contains(&id)
}

/// One state-stage kit's row: everything that decides whether a consumer keyed on effect models
/// alone would see it.
struct StateKit {
    anim: u16,
    slots: usize,
    world: bool,
    sound: bool,
    procs: usize,
    spells: BTreeSet<u32>,
}

impl StateKit {
    /// Nothing but the anim — the class an effects/sound/proc test drops on the floor.
    fn anim_only(&self) -> bool {
        self.slots == 0 && !self.world && !self.sound && self.procs == 0
    }
}

/// Census the anim column: the raw table, then reachability per stage, then the two stages whose
/// consumers differ — `state` (aura lifetime) in full, `impact` (the victim's one-shot) ranked.
pub fn run(chain: &mut Chain) -> Result<()> {
    let spells = benilla_formats::load_spell_catalog(chain)?;
    let visuals = benilla_formats::load_spell_visual_catalog(chain)?;
    // `AnimationData.dbc` is only the naming column here — a missing table degrades to bare ids
    // rather than failing the census (the same optional shape every DBC consumer takes).
    let anims = benilla_formats::load_anim_data_catalog(chain).ok();
    let name = |id: u16| -> String {
        anims
            .as_ref()
            .and_then(|a: &AnimDataCatalog| a.name(id))
            .unwrap_or("?")
            .to_string()
    };

    // 1. The raw table: which anim ids the shipped kits ask for at all.
    let mut by_anim: BTreeMap<u16, BTreeSet<u32>> = BTreeMap::new();
    for kit_id in visuals.kit_ids() {
        if let Some(anim) = visuals.kit(kit_id).and_then(|k| k.anim_id) {
            by_anim.entry(anim).or_default().insert(kit_id);
        }
    }
    let kits_with_anim: usize = by_anim.values().map(BTreeSet::len).sum();
    println!(
        "SpellVisualKit: {} rows, {kits_with_anim} carry an animation id ({} distinct ids)",
        visuals.kit_len(),
        by_anim.len(),
    );
    println!("\nanim census (all kits):");
    for (anim, kits) in &by_anim {
        println!(
            "  anim {anim:>4} {:<20} {:>4} kit(s){}",
            name(*anim),
            kits.len(),
            if is_wound_id(*anim) {
                "   [wound branch — plays the flinch, not this id]"
            } else {
                ""
            },
        );
    }

    // 2. Reachability: a kit no spell's visual chain names is authored-but-dead, and the STAGE is
    //    what picks the consumer, so both are counted here.
    let mut stage_kits: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    let mut stage_spells: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    let mut state: BTreeMap<u32, StateKit> = BTreeMap::new();
    let mut impact: BTreeMap<u32, (u16, BTreeSet<u32>)> = BTreeMap::new();
    for (spell_id, display) in spells.iter() {
        let Some(stages) = visuals.stages(display.visual) else {
            continue;
        };
        for (label, pick) in STAGES {
            let kit_id = pick(stages);
            let Some(kit) = visuals.kit(kit_id) else {
                continue;
            };
            let Some(anim) = kit.anim_id else { continue };
            stage_kits.entry(label).or_default().insert(kit_id);
            stage_spells.entry(label).or_default().insert(spell_id);
            match label {
                "state" => {
                    state
                        .entry(kit_id)
                        .or_insert_with(|| StateKit {
                            anim,
                            slots: kit.effects().count(),
                            world: kit.world_effect.is_some(),
                            sound: kit.sound.is_some(),
                            procs: kit.char_procs().count(),
                            spells: BTreeSet::new(),
                        })
                        .spells
                        .insert(spell_id);
                }
                "impact" => {
                    impact
                        .entry(kit_id)
                        .or_insert_with(|| (anim, BTreeSet::new()))
                        .1
                        .insert(spell_id);
                }
                _ => {}
            }
        }
    }
    println!("\nreached from a live Spell.dbc visual chain, by stage:");
    for (label, _) in STAGES {
        println!(
            "  {label:8} {:>4} kit(s)  {:>5} spell(s)",
            stage_kits.get(label).map_or(0, BTreeSet::len),
            stage_spells.get(label).map_or(0, BTreeSet::len),
        );
    }

    // 3. The STATE stage in full — the aura-lifetime set, which is the whole reason this census
    //    exists. `ANIM-ONLY` marks the kits an effects/sound/proc arm test never sees.
    let state_spells: usize = state
        .values()
        .flat_map(|k| k.spells.iter())
        .collect::<BTreeSet<_>>()
        .len();
    let anim_only = state.values().filter(|k| k.anim_only()).count();
    println!(
        "\nSTATE-stage anims — the aura-lifetime set ({} kit(s), {state_spells} spell(s), \
         {anim_only} ANIM-ONLY):",
        state.len(),
    );
    let mut rows: Vec<(&u32, &StateKit)> = state.iter().collect();
    rows.sort_by_key(|(id, k)| (std::cmp::Reverse(k.spells.len()), **id));
    for (kit_id, k) in rows {
        println!(
            "  kit {kit_id:<5} anim {:>4} {:<20} slots {} world {} sound {} procs {}  {:>4} \
             spell(s){}",
            k.anim,
            name(k.anim),
            k.slots,
            u8::from(k.world),
            u8::from(k.sound),
            k.procs,
            k.spells.len(),
            if k.anim_only() { "  ANIM-ONLY" } else { "" },
        );
        for spell_id in k.spells.iter().take(4) {
            let n = spells.get(*spell_id).map_or("", |d| d.name.as_str());
            println!("        {spell_id:>6} {n}");
        }
        if k.spells.len() > 4 {
            println!("        … and {} more", k.spells.len() - 4);
        }
    }

    // 4. The IMPACT stage, ranked — the VICTIM's body one-shot, the other consumer. Kept to a
    //    ranked list rather than spelled out: the population is the whole damage table.
    let impact_spells: usize = impact
        .values()
        .flat_map(|(_, s)| s.iter())
        .collect::<BTreeSet<_>>()
        .len();
    println!(
        "\nIMPACT-stage anims — the victim's one-shot ({} kit(s), {impact_spells} spell(s)):",
        impact.len(),
    );
    let mut rows: Vec<(&u32, &(u16, BTreeSet<u32>))> = impact.iter().collect();
    rows.sort_by_key(|(id, (_, s))| (std::cmp::Reverse(s.len()), **id));
    for (kit_id, (anim, spells_hit)) in rows.iter().take(20) {
        println!(
            "  kit {kit_id:<5} anim {anim:>4} {:<20} {:>5} spell(s){}",
            name(*anim),
            spells_hit.len(),
            if is_wound_id(*anim) {
                "   [wound branch]"
            } else {
                ""
            },
        );
    }
    if rows.len() > 20 {
        println!("  … and {} more kit(s)", rows.len() - 20);
    }
    Ok(())
}
