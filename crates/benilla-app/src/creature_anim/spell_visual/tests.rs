//! Headless integration tests for [`super::route_cast_visuals`] — the cast-edge router run in a
//! minimal app over a synthetic visual chain. First tenant: the instant-cast hold release. An
//! instant cast's START and GO drain from the wire in the same frame, so the GO's spell-id-keyed
//! release must see the hold its own batch's START inserted through deferred `commands` — the
//! stale-query miss left Demon Armor / Ice Armor casters looping the cast pose forever (the
//! director's stuck-cast report, 2026-07-13).

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_formats::{SpellCatalog, SpellDisplay, SpellVisualCatalog, VisualKit, VisualStages};

use super::super::{
    CastEvent, CastEventKind, CastHold, EmoteAnim, RangedHold, SheathRequest, WoundAnim,
};
use super::{route_cast_visuals, KitPush, MissileSpawn, SpellKitFx, SpellKitSound, SpellVisuals};
use crate::creature_anim::SpellGoTargets;

/// Demon Armor's real chain shape (5875 `spellvis 706`): visual 130 → precast kit 217, anim 52 —
/// an instant self-buff whose precast kit carries a sustained cast anim.
const SPELL: u32 = 706;
const VISUAL: u32 = 130;
const PRECAST_KIT: u32 = 217;
const HOLD_ANIM: u16 = 52;

/// A ranged-slot spell with its own chain (an Aimed-Shot shape: `Attributes & 0x2` + a real
/// visual whose cast kit plays the fire clip) — the `0x400` hold tests' subject.
const RANGED_SPELL: u32 = 19434;
const RANGED_VISUAL: u32 = 3180;
const RANGED_CAST_KIT: u32 = 900;
const FIRE_ANIM: u16 = 46;

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_message::<CastEvent>()
        .add_message::<SpellGoTargets>()
        .add_message::<KitPush>()
        .add_message::<EmoteAnim>()
        .add_message::<WoundAnim>()
        .add_message::<SpellKitSound>()
        .add_message::<SpellKitFx>()
        .add_message::<MissileSpawn>()
        .add_message::<crate::entities::dest_fx::GroundBurst>()
        .add_message::<super::ChainProcPlay>()
        .add_message::<SheathRequest>();
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables(
        HashMap::from([
            (
                VISUAL,
                VisualStages {
                    precast: PRECAST_KIT,
                    ..Default::default()
                },
            ),
            (
                RANGED_VISUAL,
                VisualStages {
                    cast: RANGED_CAST_KIT,
                    ..Default::default()
                },
            ),
        ]),
        HashMap::from([
            (
                PRECAST_KIT,
                VisualKit {
                    anim_id: Some(HOLD_ANIM),
                    ..Default::default()
                },
            ),
            (
                RANGED_CAST_KIT,
                VisualKit {
                    anim_id: Some(FIRE_ANIM),
                    ..Default::default()
                },
            ),
        ]),
    )));
    app.insert_resource(crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([
            (
                SPELL,
                SpellDisplay {
                    visual: VISUAL,
                    ..Default::default()
                },
            ),
            (
                RANGED_SPELL,
                SpellDisplay {
                    visual: RANGED_VISUAL,
                    attributes: 0x2, // USES_RANGED_SLOT — the `0x400` hold's gate
                    ..Default::default()
                },
            ),
        ])),
        ..crate::ui_action::Spells::empty_for_tests()
    });
    app.add_systems(Update, route_cast_visuals);
    app
}

fn cast_event(entity: Entity, spell_id: u32, kind: CastEventKind) -> CastEvent {
    CastEvent {
        entity,
        spell_id,
        kind,
        seq: 1,
    }
}

fn hold(app: &App, unit: Entity) -> Option<u32> {
    app.world().entity(unit).get::<CastHold>().map(|h| {
        assert_eq!(h.anim_id, HOLD_ANIM);
        h.spell_id
    })
}

/// The timed-cast lifecycle: START arms the precast hold, the (later-frame) GO releases it.
#[test]
fn timed_cast_hold_arms_and_releases_across_frames() {
    let mut app = app();
    let unit = app.world_mut().spawn_empty().id();

    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Start));
    app.update();
    assert_eq!(hold(&app, unit), Some(SPELL), "START arms the hold");

    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Go));
    app.update();
    assert_eq!(hold(&app, unit), None, "GO releases it");
}

/// The instant-cast regression: START and GO in the SAME frame (one wire drain) — the GO must see
/// the hold its own batch inserted, or it leaks and the cast pose loops forever.
#[test]
fn same_frame_start_and_go_leave_no_hold() {
    let mut app = app();
    let unit = app.world_mut().spawn_empty().id();

    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Start));
    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Go));
    app.update();
    assert_eq!(
        hold(&app, unit),
        None,
        "the instant cast's hold is released"
    );
}

/// The spell-id key survives the overlay: a different spell's GO landing mid-cast (a proc) never
/// drops the held cast — across frames or within one.
#[test]
fn a_foreign_go_never_drops_the_hold() {
    let mut app = app();
    let unit = app.world_mut().spawn_empty().id();

    // Same frame as the START (the proc-during-instant shape) …
    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Start));
    app.world_mut()
        .write_message(cast_event(unit, 999, CastEventKind::Go));
    app.update();
    assert_eq!(
        hold(&app, unit),
        Some(SPELL),
        "same-frame foreign GO ignored"
    );

    // … and a frame later (the classic mid-cast proc).
    app.world_mut()
        .write_message(cast_event(unit, 999, CastEventKind::Go));
    app.update();
    assert_eq!(hold(&app, unit), Some(SPELL), "later foreign GO ignored");
}

/// The precast kit's own sound (kit field 13) rings at START — the gathering shape: Herb
/// Gathering's real chain (5875 `spellvis 2366`: visual 91 → precast kit 64, anim 123
/// "UseStandingLoop", sound 1104 "Gather_Herb"). The hold arms AND the kit-sound edge fires
/// once; the GO releasing the hold emits no second play.
#[test]
fn precast_kit_sound_rings_once_at_start() {
    const HERB: u32 = 2366;
    const HERB_VISUAL: u32 = 91;
    const HERB_KIT: u32 = 64;
    const HERB_SOUND: u32 = 1104;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_message::<CastEvent>()
        .add_message::<SpellGoTargets>()
        .add_message::<KitPush>()
        .add_message::<EmoteAnim>()
        .add_message::<WoundAnim>()
        .add_message::<SpellKitSound>()
        .add_message::<SpellKitFx>()
        .add_message::<MissileSpawn>()
        .add_message::<crate::entities::dest_fx::GroundBurst>()
        .add_message::<super::ChainProcPlay>()
        .add_message::<SheathRequest>();
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables(
        HashMap::from([(
            HERB_VISUAL,
            VisualStages {
                precast: HERB_KIT,
                ..Default::default()
            },
        )]),
        HashMap::from([(
            HERB_KIT,
            VisualKit {
                anim_id: Some(123),
                sound: Some(HERB_SOUND),
                ..Default::default()
            },
        )]),
    )));
    app.insert_resource(crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([(
            HERB,
            SpellDisplay {
                visual: HERB_VISUAL,
                ..Default::default()
            },
        )])),
        ..crate::ui_action::Spells::empty_for_tests()
    });
    app.add_systems(Update, route_cast_visuals);
    let unit = app.world_mut().spawn_empty().id();

    app.world_mut()
        .write_message(cast_event(unit, HERB, CastEventKind::Start));
    app.update();
    let played: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<SpellKitSound>>()
        .drain()
        .collect();
    assert!(
        matches!(
            played.as_slice(),
            [
                SpellKitSound::StopHold { .. },
                SpellKitSound::Play { kit_sound, .. }
            ] if *kit_sound == HERB_SOUND
        ),
        "START rings the precast kit's sound once (got {played:?})"
    );

    app.world_mut()
        .write_message(cast_event(unit, HERB, CastEventKind::Go));
    app.update();
    let after_go: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<SpellKitSound>>()
        .drain()
        .collect();
    assert!(
        !after_go
            .iter()
            .any(|s| matches!(s, SpellKitSound::Play { .. })),
        "the GO plays no second kit sound (got {after_go:?})"
    );
}

/// The `$TRD` resolver ([`super::held_strike_sound`]): the held spell's `SpellVisual` field-14
/// strike sound (decision 0562) — Mining's real shape (visual 93 → 1143 "Mining Impact") rings;
/// a visual without the field (Fireball's 67 shape) and an unknown spell stay `None`.
#[test]
fn held_strike_sound_reads_the_visuals_field_14() {
    const MINING: u32 = 2575;
    const FIREBALL: u32 = 133;
    let visuals = SpellVisualCatalog::from_tables(
        HashMap::from([
            (
                93,
                VisualStages {
                    precast: 166,
                    strike_sound: Some(1143),
                    ..Default::default()
                },
            ),
            (
                67,
                VisualStages {
                    precast: 30,
                    ..Default::default()
                },
            ),
        ]),
        HashMap::new(),
    );
    let spells = crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([
            (
                MINING,
                SpellDisplay {
                    visual: 93,
                    ..Default::default()
                },
            ),
            (
                FIREBALL,
                SpellDisplay {
                    visual: 67,
                    ..Default::default()
                },
            ),
        ])),
        ..crate::ui_action::Spells::empty_for_tests()
    };
    assert_eq!(
        super::held_strike_sound(&spells, &visuals, MINING),
        Some(1143)
    );
    assert_eq!(super::held_strike_sound(&spells, &visuals, FIREBALL), None);
    assert_eq!(super::held_strike_sound(&spells, &visuals, 999), None);
}

/// A same-frame START→FAIL (an instant refusal) releases like the GO path.
#[test]
fn same_frame_start_and_fail_leave_no_hold() {
    let mut app = app();
    let unit = app.world_mut().spawn_empty().id();

    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Start));
    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Fail));
    app.update();
    assert_eq!(hold(&app, unit), None, "the failed cast's hold is released");
}

/// The ranged weapon-visual MERGE (`0x60d450`, decision 0986 correcting 0370's row-level
/// reading), on [`super::resolve_stages`] directly: a RANGED-attribute spell (`Attributes & 0x2`)
/// fills every ZERO slot of its own row from the caster's weapon visual — so a basic shot with no
/// row at all takes the lot, a hunter shot keeps its impact/missile and gains the body kits, and a
/// non-ranged spell never looks.
#[test]
fn ranged_spells_merge_the_weapon_visual_into_their_empty_slots() {
    const THROW: u32 = 2764; // Attributes 0x410012, SpellVisual1 0 — the real Throw shape
    const FIREBALL: u32 = 133; // its own visual; the fallback must stay unused
    const MULTI_SHOT: u32 = 2643; // RANGED, own visual 567: impact + missile, no body kits
    const NO_VIS_MELEE: u32 = 772; // no visual, no RANGED attribute — stays silent
    const WEAPON_VISUAL: u32 = 98; // the real thrown ItemDisplayInfo col-10 substitute
    const MULTI_SHOT_VISUAL: u32 = 567;

    let visuals = SpellVisualCatalog::from_tables(
        HashMap::from([
            (
                WEAPON_VISUAL,
                VisualStages {
                    precast: 171,
                    cast: 172,
                    ..Default::default()
                },
            ),
            (
                VISUAL,
                VisualStages {
                    precast: PRECAST_KIT,
                    ..Default::default()
                },
            ),
            (
                // The real 5875 row: impact kit 658, missile model 528, gate 1, attach 1 — and
                // both body-kit slots empty, which is the whole of B153.
                MULTI_SHOT_VISUAL,
                VisualStages {
                    impact: 658,
                    missile_gate: 1,
                    missile_model: 528,
                    missile_attach: 1,
                    ..Default::default()
                },
            ),
        ]),
        HashMap::new(),
    );
    let spells = crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([
            (
                THROW,
                SpellDisplay {
                    visual: 0,
                    attributes: 0x410012,
                    ..Default::default()
                },
            ),
            (
                FIREBALL,
                SpellDisplay {
                    visual: VISUAL,
                    attributes: 0x2, // ranged bit set AND an own visual: own wins (`60d4b4`)
                    ..Default::default()
                },
            ),
            (
                MULTI_SHOT,
                SpellDisplay {
                    visual: MULTI_SHOT_VISUAL,
                    attributes: 0x10002, // the real word: RANGED set, own visual present
                    ..Default::default()
                },
            ),
            (
                NO_VIS_MELEE,
                SpellDisplay {
                    visual: 0,
                    attributes: 0,
                    ..Default::default()
                },
            ),
        ])),
        ..crate::ui_action::Spells::empty_for_tests()
    };

    let throw = super::resolve_stages(&spells, &visuals, THROW, || Some(WEAPON_VISUAL));
    assert_eq!(
        throw.map(|s| (s.precast, s.cast)),
        Some((171, 172)),
        "Throw borrows the weapon visual's kits"
    );
    assert!(
        super::resolve_stages(&spells, &visuals, THROW, || None).is_none(),
        "no ranged weapon equipped → still silent"
    );
    assert_eq!(
        super::resolve_stages(&spells, &visuals, FIREBALL, || Some(WEAPON_VISUAL))
            .map(|s| s.precast),
        Some(PRECAST_KIT),
        "a populated slot is never displaced by the weapon's"
    );
    let multi = super::resolve_stages(&spells, &visuals, MULTI_SHOT, || Some(WEAPON_VISUAL))
        .expect("Multi-Shot has its own row");
    assert_eq!(
        (multi.precast, multi.cast),
        (171, 172),
        "the empty body-kit slots fill from the weapon — the missing draw/release (B153)"
    );
    assert_eq!(
        (multi.impact, multi.missile_model, multi.missile_attach),
        (658, 528, 1),
        "its own impact + missile block survives the merge untouched"
    );
    assert_eq!(
        super::resolve_stages(&spells, &visuals, MULTI_SHOT, || None).map(|s| (s.cast, s.impact)),
        Some((0, 658)),
        "no ranged weapon equipped → the own row stands alone, unfilled"
    );
    assert!(
        super::resolve_stages(&spells, &visuals, NO_VIS_MELEE, || Some(WEAPON_VISUAL)).is_none(),
        "a non-ranged spell never takes the fallback"
    );
}

/// The aura state watcher (`arm_aura_state_fx`): a spell id appearing in a unit's aura slots
/// arms its state kit's effects persistent under [`super::FxClass::AuraState`]; the id leaving
/// the slots reaps them; a slot-hold in between writes nothing. Food's real chain shape
/// (5875: spell 433 → visual 51 → state kit 409 → effect 393 `Spells\Item_Bread.mdx`).
#[test]
fn aura_state_kit_arms_persistent_and_reaps_on_aura_end() {
    use benilla_protocol::messages::ObjectFields;

    const FOOD: u32 = 433;
    const FOOD_VISUAL: u32 = 51;
    const STATE_KIT: u32 = 409;
    const BREAD_FX: u32 = 393;

    #[derive(Resource, Default)]
    struct FxLog(Vec<SpellKitFx>);

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_message::<SpellKitFx>();
    // The watcher's other fan-outs: the kit's CharProc edges (`crate::aura_visual`) and its
    // sound leg (0852). Food's kit 409 carries neither, so nothing is asserted here — the
    // messages just have to exist for the writers.
    app.add_message::<crate::aura_visual::AuraProc>();
    app.add_message::<SpellKitSound>();
    app.init_resource::<FxLog>();
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables_with_paths(
        HashMap::from([(
            FOOD_VISUAL,
            VisualStages {
                state: STATE_KIT,
                ..Default::default()
            },
        )]),
        HashMap::from([(
            STATE_KIT,
            VisualKit {
                // Slot 4 = the spell-hand tag (KIT_SLOT_TAGS[4] = 0x16) — bread's real slot.
                effect_slots: [
                    None,
                    None,
                    None,
                    None,
                    Some(BREAD_FX),
                    None,
                    None,
                    None,
                    None,
                ],
                ..Default::default()
            },
        )]),
        HashMap::from([(BREAD_FX, "Spells\\Item_Bread.mdx".to_string())]),
    )));
    app.insert_resource(crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([(
            FOOD,
            SpellDisplay {
                visual: FOOD_VISUAL,
                ..Default::default()
            },
        )])),
        ..crate::ui_action::Spells::empty_for_tests()
    });
    app.add_systems(
        Update,
        (
            super::arm_aura_state_fx,
            |mut r: MessageReader<SpellKitFx>, mut log: ResMut<FxLog>| {
                log.0.extend(r.read().cloned());
            },
        )
            .chain(),
    );

    // The aura lands in slot 0: UNIT_FIELD_AURA[0] = 47 carries the spell id; the slot's
    // AURAFLAGS nibble (field 95, low nibble) needs an effect-index bit (occupancy is the
    // flags test, decision 0257).
    let eating = ObjectFields::from_pairs(&[(47, FOOD), (95, 0x0E)]);
    let fasted = ObjectFields::from_pairs(&[(95, 0)]);

    let unit = app.world_mut().spawn(crate::net::ObjectStore(eating)).id();
    app.update();
    {
        let log = &app.world().resource::<FxLog>().0;
        assert_eq!(log.len(), 1, "one Begin on the ADD edge");
        let SpellKitFx::Begin {
            spell_id,
            persistent,
            class,
            effects,
            ..
        } = &log[0]
        else {
            panic!("expected Begin");
        };
        assert_eq!(*spell_id, FOOD);
        assert!(*persistent, "state kit persists for the aura's life");
        assert_eq!(*class, super::FxClass::AuraState);
        assert_eq!(
            effects.as_slice(),
            [(0x16, "Spells\\Item_Bread.mdx".to_string())],
            "bread at the spell hand"
        );
    }

    // Slot held: no further edges.
    app.update();
    assert_eq!(
        app.world().resource::<FxLog>().0.len(),
        1,
        "a held aura re-arms nothing"
    );

    // The aura leaves the slots: one AuraState reap.
    app.world_mut()
        .entity_mut(unit)
        .insert(crate::net::ObjectStore(fasted));
    app.update();
    {
        let log = &app.world().resource::<FxLog>().0;
        assert_eq!(log.len(), 2, "one Reap on the REMOVE edge");
        let SpellKitFx::Reap {
            spell_id, class, ..
        } = &log[1]
        else {
            panic!("expected Reap");
        };
        assert_eq!(*spell_id, FOOD);
        assert_eq!(*class, super::FxClass::AuraState);
    }
}

/// The GO's **release gate** (the client's `0x6e7a70` flush condition): a Speed>0 spell whose
/// cast kit plays a body animation emits its [`MissileSpawn`] deferred (`awaits_release`) —
/// the launch waits for the animation's release keyframe — while a cast kit with no animation
/// (or none at all) launches at GO.
#[test]
fn missile_spawn_defers_iff_the_cast_kit_animates() {
    const ANIMATED: u32 = 133; // Fireball's shape: cast kit with anim 53
    const SILENT: u32 = 134; // same chain, cast kit with no anim
    const ANIMATED_VISUAL: u32 = 67;
    const SILENT_VISUAL: u32 = 68;
    const CAST_KIT: u32 = 38;
    const MUTE_KIT: u32 = 39;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_message::<CastEvent>()
        .add_message::<SpellGoTargets>()
        .add_message::<KitPush>()
        .add_message::<EmoteAnim>()
        .add_message::<WoundAnim>()
        .add_message::<SpellKitSound>()
        .add_message::<SpellKitFx>()
        .add_message::<MissileSpawn>()
        .add_message::<crate::entities::dest_fx::GroundBurst>()
        .add_message::<super::ChainProcPlay>()
        .add_message::<SheathRequest>();
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables(
        HashMap::from([
            (
                ANIMATED_VISUAL,
                VisualStages {
                    cast: CAST_KIT,
                    ..Default::default()
                },
            ),
            (
                SILENT_VISUAL,
                VisualStages {
                    cast: MUTE_KIT,
                    ..Default::default()
                },
            ),
        ]),
        HashMap::from([
            (
                CAST_KIT,
                VisualKit {
                    anim_id: Some(53),
                    ..Default::default()
                },
            ),
            (
                MUTE_KIT,
                VisualKit {
                    anim_id: None,
                    ..Default::default()
                },
            ),
        ]),
    )));
    app.insert_resource(crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([
            (
                ANIMATED,
                SpellDisplay {
                    visual: ANIMATED_VISUAL,
                    speed: 24.0,
                    ..Default::default()
                },
            ),
            (
                SILENT,
                SpellDisplay {
                    visual: SILENT_VISUAL,
                    speed: 24.0,
                    ..Default::default()
                },
            ),
        ])),
        ..crate::ui_action::Spells::empty_for_tests()
    });
    app.add_systems(Update, route_cast_visuals);

    let caster = app.world_mut().spawn_empty().id();
    let target = app.world_mut().spawn_empty().id();
    for spell_id in [ANIMATED, SILENT] {
        app.world_mut().write_message(SpellGoTargets {
            caster,
            spell_id,
            hits: vec![target],
            misses: Vec::new(),
            dest: None,
            ammo_display_id: None,
            seq: 1,
        });
    }
    app.update();
    let spawns: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<MissileSpawn>>()
        .drain()
        .map(|m| (m.spell_id, m.awaits_release))
        .collect();
    assert_eq!(
        spawns,
        vec![(ANIMATED, true), (SILENT, false)],
        "deferred iff the cast kit animates"
    );
}

/// **B130's crash** — the second ever reported: a release build panicked on `insert<CastHold>`
/// while flying at high speed through the Wetlands, applying hold commands to a unit that had
/// despawned. Both windows are exercised here, because they fail for different reasons:
///
/// 1. **Already gone when the edge is read.** Every despawn of an indexed unit runs inside the wire
///    drain (`DESTROY_OBJECT`, the out-of-range stream-out, the worldport purge), and those are
///    applied at the sync point this chain sits behind — so a START and its subject's death arrive
///    in one batch and the edge outlives the unit.
/// 2. **Queued this frame, no sync point between.** `model_fade::apply_despawn_fade` is
///    Update-unordered against this chain; its despawn can be queued before ours and applied first,
///    which a queue-time existence check structurally cannot see.
///
/// The pass condition is that the frame completes — and that neither window resurrects the unit.
#[test]
fn a_despawned_subject_never_panics_the_router() {
    {
        let mut app = app();
        let unit = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(unit).despawn();
        app.world_mut()
            .write_message(cast_event(unit, SPELL, CastEventKind::Start));
        app.update(); // window 1: panicked here before the fix
        assert!(
            app.world().get_entity(unit).is_err(),
            "the hold write must not resurrect a dead subject"
        );
    }
    {
        // `before_ignore_deferred` is exactly the fade lane's shape — an ordering edge with no sync
        // point on it, so both command queues flush together and the despawn applies first.
        let mut app = app();
        let unit = app.world_mut().spawn_empty().id();
        app.add_systems(
            Update,
            (move |mut commands: Commands| {
                commands.entity(unit).try_despawn();
            })
            .before_ignore_deferred(route_cast_visuals),
        );
        app.world_mut()
            .write_message(cast_event(unit, SPELL, CastEventKind::Start));
        app.update();
        assert!(
            app.world().get_entity(unit).is_err(),
            "the same-frame despawn wins; the hold write is dropped"
        );
    }
}

/// The `0x400` weapon-visual hold (wow-re `ranged-sheath-exempt-autorepeat.md` §Q4): a RANGED
/// spell's visual play inserts [`RangedHold`] on ANY caster — what keeps a remote shooter in
/// the drawn Load/Hold idle between shots — and a non-ranged visual play clears it (the
/// client's stale-visual cleanup `0x6ec39e`).
#[test]
fn ranged_visual_play_arms_the_any_caster_hold_and_a_non_ranged_play_clears_it() {
    let mut app = app();
    let unit = app.world_mut().spawn_empty().id();

    // A remote shooter's per-shot GO (cast kit resolves) → the hold arms.
    app.world_mut()
        .write_message(cast_event(unit, RANGED_SPELL, CastEventKind::Go));
    app.update();
    assert!(
        app.world().entity(unit).get::<RangedHold>().is_some(),
        "a ranged GO's visual play sets the hold"
    );

    // A later NON-ranged visual play (the buff's precast kit) → the stale-visual cleanup.
    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Start));
    app.update();
    assert!(
        app.world().entity(unit).get::<RangedHold>().is_none(),
        "a non-ranged visual play clears the hold"
    );

    // A ranged START (the volley activation's precast play) re-arms it too — but this shape's
    // precast stage is empty, so drive it through the GO again after the clear.
    app.world_mut()
        .write_message(cast_event(unit, RANGED_SPELL, CastEventKind::Go));
    app.update();
    assert!(
        app.world().entity(unit).get::<RangedHold>().is_some(),
        "the next ranged play re-arms"
    );
}

/// **The mount poof** ([`super::arm_mount_poof_fx`], decision 0927) — the three properties the
/// reference's `UNIT_FIELD_MOUNTDISPLAYID` watcher gives it, each a real fork in `0x5ffa50`:
/// **the build leg only** (the whole allocation sits behind `5ffa87 je 0x5ffade` on the NEW
/// value, so a dismount spawns nothing), **any changed value** (0→N and N→N′ alike), and
/// **first sight silent** (a unit that streams in already mounted did not just mount — the
/// level-up ding's own treatment, decision 0305).
#[test]
fn the_mount_poof_puffs_on_the_build_leg_only() {
    use benilla_protocol::ObjectFields;

    /// `UNIT_FIELD_MOUNTDISPLAYID` (index 133, decision 0441).
    const FIELD_MOUNTDISPLAYID: u16 = 133;
    /// `SpellVisualEffectName` row 1185's shipped path — the druid-morph cloud.
    const POOF: &str = "Spells\\DruidMorph_Impact_Base.mdx";
    /// The M2 attach the hardcoded-effect spawn stamps (`DAT_0080c968[6]`).
    const BASE_ATTACH: u16 = 0x13;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_message::<SpellKitFx>();
    app.insert_resource(SpellVisuals(
        SpellVisualCatalog::from_tables(HashMap::new(), HashMap::new())
            .with_hardcoded("HARDCODED Mount Poof", POOF),
    ));
    app.add_systems(Update, super::arm_mount_poof_fx);

    let store =
        |v: u32| crate::net::ObjectStore(ObjectFields::from_pairs(&[(FIELD_MOUNTDISPLAYID, v)]));
    // Streams in ALREADY mounted: first sight arms the memory, silently.
    let unit = app.world_mut().spawn(store(2404)).id();
    app.update();
    let puffs = |app: &mut App| -> Vec<(u16, String)> {
        let mut out = Vec::new();
        let world = app.world_mut();
        let mut msgs = world.resource_mut::<Messages<SpellKitFx>>();
        for m in msgs.drain() {
            if let SpellKitFx::Begin {
                entity, effects, ..
            } = m
            {
                assert_eq!(entity, unit);
                out.extend(effects);
            }
        }
        out
    };
    assert!(
        puffs(&mut app).is_empty(),
        "a rider that streams into view did not just mount"
    );

    // Dismount — the NEW value is 0, so the build leg (and the whole allocation) is skipped.
    app.world_mut().entity_mut(unit).insert(store(0));
    app.update();
    assert!(puffs(&mut app).is_empty(), "no poof on the way down");

    // Mount: 0 → N.
    app.world_mut().entity_mut(unit).insert(store(2404));
    app.update();
    assert_eq!(
        puffs(&mut app),
        vec![(BASE_ATTACH, POOF.to_string())],
        "the build leg puffs the druid-morph cloud at the base attach"
    );

    // A steady mounted frame is not an edge — the watcher fires on the field CHANGING.
    app.world_mut().entity_mut(unit).insert(store(2404));
    app.update();
    assert!(puffs(&mut app).is_empty(), "no re-puff while just riding");

    // A swap (N → N′) is a change, and the reference rebuilds and puffs again.
    app.world_mut().entity_mut(unit).insert(store(2405));
    app.update();
    assert_eq!(puffs(&mut app), vec![(BASE_ATTACH, POOF.to_string())]);
}

/// **The link that makes the beam exist at all** (decision 0955 slice 2): a kit carrying a chain
/// `CharProc` emits a [`ChainProcPlay`] when it plays — from BOTH of the reference's dispatcher
/// call sites, `PlaySpellVisualKit`'s tail (the cast release) and the channel poll (`0x612b18`,
/// which is the only way a channelled beam is ever reached). A kit without one emits nothing.
///
/// This is the test that fails if the whole lane silently does nothing: the geometry, the hop
/// array and the wire can all be right while no kit ever asks for a beam.
#[test]
fn a_kit_with_a_chain_char_proc_asks_for_a_beam_from_both_dispatcher_sites() {
    use benilla_formats::{char_proc_type, CharProc};

    const BEAM_SPELL: u32 = 421; // Chain Lightning
    const BEAM_VISUAL: u32 = 36;
    const BEAM_CAST_KIT: u32 = 321;
    const BEAM_CHANNEL_KIT: u32 = 402;

    let mut app = app();
    // Overlay the beam chain onto the fixture catalog: a cast kit with the real type-12 slot and a
    // channel kit with the real type-0 one. Params are the shipped shape — chain id 1, one strand,
    // and the flag that splits cast (0) from channel (1).
    let mut visuals = HashMap::from([(
        BEAM_VISUAL,
        VisualStages {
            cast: BEAM_CAST_KIT,
            channel: BEAM_CHANNEL_KIT,
            ..Default::default()
        },
    )]);
    let mut kits = HashMap::new();
    for (kit, ty, flag) in [
        (BEAM_CAST_KIT, char_proc_type::CHAIN_CAST, 0.0),
        (BEAM_CHANNEL_KIT, char_proc_type::CHAIN_CHANNEL, 1.0),
    ] {
        kits.insert(
            kit,
            VisualKit {
                char_proc_slots: [
                    Some(CharProc {
                        ty,
                        params: [1.0, 1.0, flag, 0.0],
                    }),
                    None,
                    None,
                    None,
                ],
                ..Default::default()
            },
        );
    }
    // …plus a beam-less kit, so "emits nothing" is a real control and not an empty catalog.
    kits.insert(PRECAST_KIT, VisualKit::default());
    visuals.insert(
        VISUAL,
        VisualStages {
            cast: PRECAST_KIT,
            ..Default::default()
        },
    );
    app.insert_resource(SpellVisuals(SpellVisualCatalog::from_tables(visuals, kits)));
    app.insert_resource(crate::ui_action::Spells {
        catalog: SpellCatalog::from_displays(HashMap::from([
            (
                BEAM_SPELL,
                SpellDisplay {
                    visual: BEAM_VISUAL,
                    ..Default::default()
                },
            ),
            (
                SPELL,
                SpellDisplay {
                    visual: VISUAL,
                    ..Default::default()
                },
            ),
        ])),
        ..crate::ui_action::Spells::empty_for_tests()
    });

    let plays = |app: &mut App| -> Vec<(Entity, u32, bool)> {
        app.world_mut()
            .resource_mut::<Messages<super::ChainProcPlay>>()
            .drain()
            .map(|p| (p.entity, p.spell_id, p.proc.flag))
            .collect()
    };

    // Site 1 — the cast release (`0x60f35c`).
    let unit = app.world_mut().spawn_empty().id();
    app.world_mut()
        .write_message(cast_event(unit, BEAM_SPELL, CastEventKind::Go));
    app.update();
    assert_eq!(
        plays(&mut app),
        vec![(unit, BEAM_SPELL, false)],
        "the cast kit's type-12 proc asks for a one-shot beam"
    );

    // The control: a spell whose cast kit carries no chain proc asks for nothing.
    app.world_mut()
        .write_message(cast_event(unit, SPELL, CastEventKind::Go));
    app.update();
    assert!(plays(&mut app).is_empty(), "a beam-less kit stays silent");

    // Site 2 — the channel poll (`0x612b18`). The rising edge of `UNIT_CHANNEL_SPELL` is what
    // reaches Drain Life's kit; nothing else ever plays it.
    let channeller = app
        .world_mut()
        .spawn(crate::net::ObjectStore(
            benilla_protocol::ObjectFields::from_pairs(&[(144, BEAM_SPELL)]),
        ))
        .id();
    app.update();
    assert_eq!(
        plays(&mut app),
        vec![(channeller, BEAM_SPELL, true)],
        "the channel kit's type-0 proc asks for a persistent beam"
    );
}
