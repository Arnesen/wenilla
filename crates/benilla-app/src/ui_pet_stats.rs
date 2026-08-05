//! The hunter pet's **paper-doll stat block** — `GetPetHappiness`, `GetPetLoyalty`,
//! `GetPetTrainingPoints`, `GetPetExperience` and `HasPetUI` (decision 1005; wow-re
//! `ui/scratch/pet-action-bar-api.md` §11b + §6).
//!
//! Its own module rather than more of [`crate::ui_pet`] because it is a different concern on a
//! different clock: the action bar's contents arrive whole in `SMSG_PET_SPELLS` and change when
//! the server says so, while these five read plain descriptor fields plus two DBC tables and move
//! continuously — happiness drifts every few seconds while a pet is out. Tying them together would
//! make the bar's diff-and-fire churn on numbers no button draws.
//!
//! **One gate governs all four stat bindings**: `0x6116e0(pet)`, which resolves the pet, requires
//! its `UNIT_FIELD_PETNUMBER`, requires its owner to be us, and finishes
//! `cmp byte [player.fields + 0x79], 3; sete al` — `UNIT_FIELD_BYTES_0` byte 1, the class, against
//! **Hunter**. (The enum is pinned by the combo-point gate's own class test on the same byte:
//! `== 4 || == 0xB` is Rogue and Druid, wow-re `combo-point-gate.md`.) A warlock's imp therefore
//! resolves perfectly and still answers nothing — happiness, loyalty and training points are
//! hunter machinery — and each binding says "nothing" in its own way ([`benilla_ui::script::PetStats`]).

use bevy::prelude::*;

use benilla_ui::script::{PetStats, UiScript};

use crate::net::ObjectStore;
use crate::ui_pet::{PetBar, PetUnit};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// `UNIT_FIELD_BYTES_0` byte 1 == 3 — **Hunter**, the class the four stat bindings gate on
/// (`0x611752`). Pinned rather than assumed: the same byte's `4`/`0xB` are Rogue and Druid in the
/// already-carved combo-point gate, which fixes the enum this value sits in.
const CLASS_HUNTER: u8 = 3;

/// `UNIT_FIELD_BYTES_0` byte 3 == 4 — the **happiness** power, the field `GetPetHappiness`
/// thresholds. Read by power *index*, not by the unit's active power type: a pet's displayed bar is
/// focus, and happiness is a parallel slot that is always there.
const POWER_HAPPINESS: u8 = 4;

/// The two pet DBC tables, loaded once at startup. Absent client data leaves the stat block empty,
/// which is the same shape as "not a hunter pet" — degraded, never wrong.
#[derive(Resource)]
pub(crate) struct PetStatTables {
    pub(crate) personalities: benilla_formats::PetPersonalities,
    pub(crate) loyalty: benilla_formats::PetLoyaltyNames,
}

pub(crate) struct UiPetStatsPlugin;

impl Plugin for UiPetStatsPlugin {
    fn build(&self, app: &mut App) {
        // Rides the unit feed beside the pet bar's own, and before the VM ticks — the pet frame
        // repaints out of the same pass that pushes its health.
        app.add_systems(Update, feed_pet_stats.in_set(UnitFeed).before(UiInput));
    }
}

/// Resolve the whole stat block for the current pet, or [`PetStats::default`] when there is none.
///
/// Split from the system so the composition is testable: every one of these five values passes
/// through the *same* `hunter_pet` gate, and a bug that let one leak past it would show up as a
/// warlock with a loyalty level rather than as a compile error.
fn stats_for(
    pet: Option<&ObjectStore>,
    self_store: Option<&ObjectStore>,
    tables: Option<&PetStatTables>,
) -> (bool, PetStats) {
    let Some(fields) = pet.map(|s| &s.0) else {
        return (false, PetStats::default());
    };
    // `HasPetUI`'s FIRST return (`0x4be697`): a pet that resolves AND carries a pet number. Not the
    // action bar's gate, which is the cached guid alone — a possessed creature has a bar and no
    // pet number, so it gets buttons and no paper doll.
    let has_ui = fields.unit_is_pet_or_charm();
    // …and the second: the owner's class. We only ever hold a bar for a pet the server named us
    // the controller of, so the owner leg is structurally true here; the class is the live test.
    let hunter = has_ui
        && self_store.map(|s| ((s.0.unit_bytes_0().unwrap_or(0) >> 8) & 0xff) as u8)
            == Some(CLASS_HUNTER);
    if !hunter {
        return (has_ui, PetStats::default());
    }
    let (happiness, damage_percentage, loyalty_rate) = tables
        .and_then(|t| t.personalities.for_pet(None))
        .map(|p| {
            let h = p.happiness(fields.unit_power(POWER_HAPPINESS).unwrap_or(0));
            (Some(h.bucket), h.damage_percentage, h.loyalty_rate)
        })
        // No DBC ⇒ the binding's own gate-failure numbers, and nil for the bucket.
        .unwrap_or((None, 100.0, 0.0));
    (
        has_ui,
        PetStats {
            hunter_pet: true,
            happiness,
            damage_percentage,
            loyalty_rate,
            loyalty: tables.and_then(|t| {
                t.loyalty
                    .name(u32::from(fields.unit_loyalty_level()))
                    .map(str::to_string)
            }),
            training_points: fields.unit_training_points(),
            experience: fields.unit_pet_experience(),
        },
    )
}

fn feed_pet_stats(
    script: Option<NonSendMut<UiScript>>,
    bar: Res<PetBar>,
    pet: PetUnit,
    self_store: Query<&ObjectStore, With<crate::net::SelfPlayer>>,
    tables: Option<Res<PetStatTables>>,
    mut last: Local<Option<(bool, PetStats)>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let fresh = stats_for(
        pet.store(bar.spells.pet_guid),
        self_store.iter().next(),
        tables.as_deref(),
    );
    // Push only on change. Happiness moves on its own clock and the frame repaints off UNIT_*
    // events, so a per-frame push would be pure churn — but the DIFF is what makes this cheap,
    // not a timer, so a real change still lands the same frame it arrives.
    if last.as_ref() == Some(&fresh) {
        return;
    }
    // `UNIT_HAPPINESS` — the pet frame's repaint wire for the icon (ref `PetFrame_OnEvent`'s last
    // arm). The reference fires it off the happiness FIELD changing; we fire it off the three
    // values the icon and its tooltip are built from, which is the same event for every consumer
    // there is — the icon is bucketed and both tooltip lines are bucket-derived, so movement
    // *within* a bucket has nothing to repaint. Named because it is a narrower trigger, not a
    // wider one: nothing reads the raw happiness number.
    let happiness_moved = last.as_ref().map(|(_, s)| {
        (
            s.happiness,
            s.damage_percentage.to_bits(),
            s.loyalty_rate.to_bits(),
        )
    }) != Some((
        fresh.1.happiness,
        fresh.1.damage_percentage.to_bits(),
        fresh.1.loyalty_rate.to_bits(),
    ));
    if happiness_moved {
        debug!(
            "ui_pet_stats: happiness {:?} ({}% damage), loyalty {:?}",
            fresh.1.happiness, fresh.1.damage_percentage, fresh.1.loyalty
        );
    }
    *last = Some(fresh.clone());
    script.set_pet_stats(fresh.0, fresh.1);
    if happiness_moved {
        script.fire_event("UNIT_HAPPINESS", vec![]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::ObjectFields;

    const BYTES_0: u16 = 36;
    const BYTES_1: u16 = 138;
    const POWER5: u16 = 27;
    const PETNUMBER: u16 = 139;
    const PETXP: u16 = 141;
    const PETNEXTXP: u16 = 142;
    const TRAINING: u16 = 149;

    fn hunter() -> ObjectStore {
        // BYTES_0 byte 1 = class. Hunter = 3.
        ObjectStore(ObjectFields::from_pairs(&[(
            BYTES_0,
            u32::from(CLASS_HUNTER) << 8,
        )]))
    }

    fn warlock() -> ObjectStore {
        ObjectStore(ObjectFields::from_pairs(&[(BYTES_0, 9 << 8)]))
    }

    /// A boar: pet number set, loyalty level 6, happy, part-trained, mid-XP.
    fn boar() -> ObjectStore {
        ObjectStore(ObjectFields::from_pairs(&[
            (PETNUMBER, 42),
            (BYTES_1, 6 << 8),             // loyalty level in byte 1
            (POWER5, 1_000_000),           // maximum happiness
            (TRAINING, (170 << 16) | 130), // total in the HIGH word, spent in the low
            (PETXP, 4200),
            (PETNEXTXP, 8000),
        ]))
    }

    fn tables() -> Option<PetStatTables> {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return None;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        Some(PetStatTables {
            personalities: benilla_formats::load_pet_personalities(&mut chain)
                .expect("personality"),
            loyalty: benilla_formats::load_pet_loyalty_names(&mut chain).expect("loyalty"),
        })
    }

    /// The whole block end to end on the real DBC data — the packed/derived reads are the point:
    /// the training-points word order, the loyalty byte, and happiness coming out PRE-BUCKETED
    /// with vanilla's own 125% damage for a maxed pet.
    #[test]
    fn a_hunters_boar_reads_every_field() {
        let Some(t) = tables() else { return };
        let (has_ui, s) = stats_for(Some(&boar()), Some(&hunter()), Some(&t));
        assert!(has_ui && s.hunter_pet);
        assert_eq!(s.happiness, Some(3));
        assert_eq!(s.damage_percentage, 125.0);
        assert_eq!(s.loyalty_rate, 20.0);
        assert_eq!(s.loyalty.as_deref(), Some("(Loyalty Level 6) Best Friend"));
        assert_eq!(
            s.training_points,
            (170, 130),
            "TOTAL first, from the high word"
        );
        assert_eq!(s.experience, (4200, 8000));
    }

    /// The happiness bucket really is derived from the power field, not passed through: three
    /// different raw values give three different buckets and three different damage numbers.
    #[test]
    fn happiness_buckets_the_raw_power() {
        let Some(t) = tables() else { return };
        let at = |raw: u32| {
            let store = ObjectStore(ObjectFields::from_pairs(&[
                (PETNUMBER, 42),
                (BYTES_1, 0),
                (POWER5, raw),
            ]));
            let (_, s) = stats_for(Some(&store), Some(&hunter()), Some(&t));
            (s.happiness, s.damage_percentage)
        };
        assert_eq!(at(0), (Some(1), 75.0), "an unhappy pet deals 75%");
        assert_eq!(at(500_000), (Some(2), 100.0));
        assert_eq!(at(1_000_000), (Some(3), 125.0));
    }

    /// **The gate is the class, and it takes everything with it.** A warlock's imp has a pet
    /// number — so `HasPetUI`'s first return stays true — and answers no stats at all. Letting one
    /// value leak past the gate is the failure this guards: an imp with a loyalty level.
    #[test]
    fn a_warlocks_minion_has_a_ui_and_no_stats() {
        let Some(t) = tables() else { return };
        let (has_ui, s) = stats_for(Some(&boar()), Some(&warlock()), Some(&t));
        assert!(
            has_ui,
            "the pet number is what HasPetUI's first return reads"
        );
        assert!(!s.hunter_pet);
        assert_eq!(s.happiness, None);
        assert_eq!(s.loyalty, None);
        assert_eq!(s.training_points, (0, 0));
        assert_eq!(s.experience, (0, 0));
    }

    /// A possessed creature has an action bar but no pet number, so it gets no paper doll either —
    /// the two gates are genuinely different (`PetHasActionBar` is the cached guid alone).
    #[test]
    fn no_pet_number_means_no_pet_ui() {
        let Some(t) = tables() else { return };
        let possessed = ObjectStore(ObjectFields::from_pairs(&[(POWER5, 1_000_000)]));
        let (has_ui, s) = stats_for(Some(&possessed), Some(&hunter()), Some(&t));
        assert!(!has_ui);
        assert!(!s.hunter_pet);
    }

    /// Loyalty level 0 is nil, not level 1 — the client's own bound, and the difference between a
    /// fresh pet showing nothing and showing "Rebellious".
    #[test]
    fn loyalty_level_zero_is_nil() {
        let Some(t) = tables() else { return };
        let fresh = ObjectStore(ObjectFields::from_pairs(&[
            (PETNUMBER, 42),
            (BYTES_1, 0),
            (POWER5, 1_000_000),
        ]));
        let (_, s) = stats_for(Some(&fresh), Some(&hunter()), Some(&t));
        assert_eq!(s.loyalty, None);
        assert_eq!(s.happiness, Some(3), "…but happiness still answers");
    }

    /// Missing client data degrades to the bindings' own gate-failure numbers rather than to a
    /// wrong bucket: nil happiness with `(100.0, 0.0)` beside it.
    #[test]
    fn absent_dbc_data_degrades_to_the_failure_numbers() {
        let (has_ui, s) = stats_for(Some(&boar()), Some(&hunter()), None);
        assert!(has_ui && s.hunter_pet);
        assert_eq!(s.happiness, None);
        assert_eq!((s.damage_percentage, s.loyalty_rate), (100.0, 0.0));
        // The descriptor-only values still read — they need no table.
        assert_eq!(s.training_points, (170, 130));
    }

    /// The RUNTIME leg on the real data: every GlobalStrings key the happiness tooltip resolves
    /// exists in the shipped 1.12 file, with the `%d` the damage line formats into. A typo'd key
    /// here is silent — `getglobal` answers nil and the tooltip simply loses a line.
    #[test]
    fn every_happiness_string_resolves_in_the_real_global_strings() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| {
            s.lua()
                .globals()
                .get::<String>(key)
                .ok()
                .unwrap_or_default()
        };

        // The three bucket names are resolved by CONCATENATION (`"PET_HAPPINESS"..happiness`), so
        // a missing one shows as an empty tooltip for that bucket alone — exactly the failure a
        // by-eye check misses.
        assert_eq!(g("PET_HAPPINESS1"), "Unhappy");
        assert_eq!(g("PET_HAPPINESS2"), "Content");
        assert_eq!(g("PET_HAPPINESS3"), "Happy");
        assert!(
            g("PET_DAMAGE_PERCENTAGE").contains("%d"),
            "the damage line formats the percentage in: {:?}",
            g("PET_DAMAGE_PERCENTAGE")
        );
        assert!(!g("LOSING_LOYALTY").is_empty());
        assert!(!g("GAINING_LOYALTY").is_empty());
    }

    /// No pet at all: no UI, no stats, and nothing that could be mistaken for a real answer.
    #[test]
    fn no_pet_is_no_ui() {
        let (has_ui, s) = stats_for(None, Some(&hunter()), None);
        assert!(!has_ui);
        assert_eq!(s, PetStats::default());
    }
}
