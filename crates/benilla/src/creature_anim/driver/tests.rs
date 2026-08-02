//! Headless integration tests for [`super::drive_animations`] — the full driver system run in a
//! minimal app on synthetic units, exercising the cross-frame composition the pure-fn tests in
//! `select::tests` can't reach. First tenant: the caster staff-stow chain (decisions 0080/0107 —
//! the stationary cast-hold gait pin feeding the per-animation sheath reconcile), pinned because
//! vmangos hard-sets every creature's sheath byte to melee at spawn ("creatures always have melee
//! weapon ready", `Creature.cpp`), so the cast-hold clip's WeaponFlags `&4` is the ONLY signal
//! that ever stows a caster NPC's weapon.

use bevy::animation::graph::AnimationNodeIndex;
use bevy::animation::transition::AnimationTransitions;
use bevy::prelude::*;

use benilla_assets::{AnimClip, ModelAnimations};
use benilla_formats::{AnimDataCatalog, AnimEntry};

use super::super::{
    move_flags, AnimData, AnimDriver, CastHold, EmoteAnim, Engaged, MovementState, SheathRequest,
    SheathSwapMessage, SwingMessage, Wielded, WoundAnim,
};
use super::drive_animations;
use crate::net::NetCommands;

fn clip(anim_id: u16, node: u32, looping: bool) -> AnimClip {
    AnimClip {
        anim_id,
        seq_index: 0,
        node: AnimationNodeIndex::new(node as usize),
        looping,
        duration: 1.0,
        move_speed: 0.0,
        blend_time: 0.15,
        bounds_center: Vec3::ZERO,
        bounds_radius: 0.0,
        bounds_min: Vec3::ZERO,
        bounds_max: Vec3::ZERO,
        events: Vec::new().into(),
        arm_nodes: None,
        upper_node: None,
        frequency: 0,
        replay: (0, 0),
    }
}

/// A staff-caster's model: Stand, Run, the staff Ready idle, and the precast hold clip (with a
/// masked upper-body variant, so the committed-move route has its overlay destination).
fn caster_model() -> ModelAnimations {
    let mut hold = clip(51, 3, true); // ReadySpellDirected — the precast hold
    hold.upper_node = Some(AnimationNodeIndex::new(5));
    ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),  // Stand
            clip(28, 2, true), // Ready2HL — the staff-class Ready idle
            hold,
            clip(5, 4, true), // Run
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    }
}

/// The real 5875 rows this chain rests on (decode-verified in `anim_data::tests`):
/// ReadySpellDirected carries the force-stow WeaponFlags `&4`, Ready2HL and Attack1H the
/// force-draw `&0x20`.
fn catalog() -> AnimData {
    AnimData(AnimDataCatalog::from_rows([
        (
            0,
            AnimEntry {
                weapon_flags: 0,
                fallback: 0,
            },
        ),
        (
            17,
            AnimEntry {
                weapon_flags: 0x20,
                fallback: 0,
            },
        ),
        (
            28,
            AnimEntry {
                weapon_flags: 0x20,
                fallback: 0,
            },
        ),
        (
            51,
            AnimEntry {
                weapon_flags: 4,
                fallback: 52,
            },
        ),
    ]))
}

fn app() -> App {
    let mut app = App::new();
    // Asset + animation plugins so tests with REAL clip assets (the watchdog test) get Bevy's
    // `advance_animations` ticking completions; units without a graph handle are skipped by it,
    // so the asset-less tenants are unaffected.
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin::default(),
        bevy::animation::AnimationPlugin,
    ));
    app.add_message::<SwingMessage>()
        .add_message::<crate::creature_anim::SwingImpact>()
        .add_message::<crate::creature_anim::DefenseAnim>()
        .add_message::<crate::creature_anim::SwingSlowdown>()
        .add_message::<EmoteAnim>()
        .add_message::<WoundAnim>()
        .add_message::<SheathRequest>()
        .add_message::<SheathSwapMessage>();
    // A dead-letter net channel: the driver's `let _ = send(...)` tolerates the dropped receiver,
    // and no test unit is the self player anyway.
    let (tx, _rx) = crossbeam_channel::unbounded();
    app.insert_resource(NetCommands(tx));
    app.insert_resource(catalog());
    app.add_systems(Update, drive_animations);
    app
}

/// The caster-NPC staff chain, end to end through the real system: engaged Ready draws (reconcile
/// rule 4), the stationary cast hold's gait pin stows (rule 1 — `&4` outranks engaged), the hold's
/// removal re-draws. The director's report ("caster NPC still holds their staff while casting")
/// is exactly this middle assertion.
#[test]
fn stationary_cast_hold_stows_an_engaged_casters_weapon() {
    let mut app = app();
    let unit = app
        .world_mut()
        .spawn((
            caster_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            Engaged,
            Wielded {
                main: Some((2, 0xa)), // class 2 subclass 10: a staff
                off: None,
                ranged: None,
                main_sheath: 2,
                off_sheath: 0,
                ..Default::default()
            },
        ))
        .id();
    let sheath = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .sheath_state()
    };

    // Engaged, stationary, no cast: the Ready idle forces melee-drawn.
    app.update();
    assert_eq!(sheath(&app), Some(1), "engaged Ready idle draws");

    // SMSG_SPELL_START landed (the router inserted the precast hold): the stationary pin plays
    // ReadySpellDirected full-body in the gait slot, and its WeaponFlags `&4` force-stows.
    app.world_mut().entity_mut(unit).insert(CastHold {
        ranged: false,
        anim_id: 51,
        spell_id: 20793,
    });
    app.update();
    assert_eq!(sheath(&app), Some(0), "the cast hold stows the staff");

    // GO (the router removed the hold): the engaged Ready re-takes the slot and re-draws.
    app.world_mut().entity_mut(unit).remove::<CastHold>();
    app.update();
    assert_eq!(sheath(&app), Some(1), "drawn again once the cast resolves");
}

/// The committed-move route: the hold loops masked on the torso over the gait, and its stow must
/// HOLD between plays — the reconcile is edge-triggered like the client's (`0x5fdf80` runs only
/// inside `PlayAnimation`, wow-re `sheath-policy.md`), so the base track's flags-less Run never
/// re-draws mid-hold on the frames where nothing plays. This was the caster staff bug's shape:
/// the per-frame base-track re-assert yanked the weapon back out one frame after the retake.
#[test]
fn moving_cast_hold_keeps_its_stow_between_plays() {
    let mut app = app();
    let unit = app
        .world_mut()
        .spawn((
            caster_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            Engaged,
            Wielded {
                main: Some((2, 0xa)),
                off: None,
                ranged: None,
                main_sheath: 2,
                off_sheath: 0,
                ..Default::default()
            },
            MovementState {
                speed: 7.0,
                flags: move_flags::FORWARD,
                ..Default::default()
            },
        ))
        .id();
    let sheath = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .sheath_state()
    };

    // Engaged and running: the flags-less Run gait plays, the engaged re-assert draws.
    app.update();
    assert_eq!(sheath(&app), Some(1), "engaged runner draws");

    // The precast lands mid-move: the hold takes the masked overlay route — its retake is a play,
    // so the reconcile stows.
    app.world_mut().entity_mut(unit).insert(CastHold {
        ranged: false,
        anim_id: 51,
        spell_id: 20793,
    });
    app.update();
    assert_eq!(sheath(&app), Some(0), "the masked hold's retake stows");

    // Frames where nothing plays (the gait loop wraps, the hold loops): the committed state holds.
    for _ in 0..3 {
        app.update();
        assert_eq!(sheath(&app), Some(0), "no play — the stow persists");
    }

    // GO while still running: the hold drops, but the base Run keeps looping — still no play, so
    // the staff stays stowed (the client re-draws only at the next play).
    app.world_mut().entity_mut(unit).remove::<CastHold>();
    app.update();
    assert_eq!(sheath(&app), Some(0), "released mid-run — no play yet");

    // The creature stops: the engaged Ready idle plays, and that play's reconcile re-draws.
    app.world_mut().entity_mut(unit).remove::<MovementState>();
    app.update();
    assert_eq!(sheath(&app), Some(1), "the stop's Ready play re-draws");
}

/// A fidgeter's model: Stand as a two-variation chain — a zero-frequency head plus a
/// max-frequency "look around" variation, so the first `_rand` roll (38, from the LCG's zero
/// seed) deterministically lands on the variation — and a ShuffleLeft clip for the turn latch.
fn fidget_model() -> ModelAnimations {
    let mut head = clip(0, 1, true); // Stand — the head variation
    head.frequency = 0;
    let mut look = clip(0, 6, true); // Stand — the rare look-around variation
    look.frequency = 32767;
    ModelAnimations {
        graph: Handle::default(),
        clips: vec![head, look, clip(11, 7, true) /* ShuffleLeft */],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    }
}

/// The emergent idle fidget (decision 0123 — wow-re `loop-replay-fidget.md` §5b): a RELAXED base
/// arm rolls its variation (the client's `variationIdx = −1`), an engaged one is forced to the
/// deterministic head, and the idle re-face turn-shuffle ([`crate::net::FacingStep`]) drives the
/// Shuffle↔Stand churn whose every return to Stand re-rolls.
#[test]
fn relaxed_base_arms_roll_variations_and_the_shuffle_drives_them() {
    let mut app = app();
    let unit = app
        .world_mut()
        .spawn((
            fidget_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
        ))
        .id();
    let active = |app: &App, node: u32| {
        app.world()
            .entity(unit)
            .get::<AnimationPlayer>()
            .unwrap()
            .animation(AnimationNodeIndex::new(node as usize))
            .is_some()
    };
    let gait = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().gait;

    // The first (relaxed) Stand arm rolls: the weighted walk lands on the look-around variation.
    app.update();
    assert_eq!(gait(&app), Some(0));
    assert!(active(&app, 6), "the rolled variation is what armed");
    assert!(!active(&app, 1), "not the head");

    // The idle re-face steps its yaw: the turn latch routes the gait to the foot-shuffle.
    app.world_mut()
        .entity_mut(unit)
        .insert(crate::net::FacingStep(0.3));
    app.update();
    assert_eq!(gait(&app), Some(11), "stepping yaw → ShuffleLeft");

    // The ease settles: Shuffle → Stand is a fresh relaxed re-arm — a fresh roll (the fidget).
    app.world_mut()
        .entity_mut(unit)
        .remove::<crate::net::FacingStep>();
    app.update();
    assert_eq!(gait(&app), Some(0), "settled → back to Stand");
    assert!(
        active(&app, 1) || active(&app, 6),
        "some Stand variation re-armed"
    );
}

/// The combat carve-out: an engaged unit's base arms keep the deterministic head — fighters
/// never fidget (the client's `0x5fdba0` re-zero gate).
#[test]
fn engaged_base_arms_keep_the_head_variation() {
    let mut app = app();
    let unit = app
        .world_mut()
        .spawn((
            fidget_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            Engaged,
        ))
        .id();
    app.update();
    let player = app.world().entity(unit).get::<AnimationPlayer>().unwrap();
    // Engaged with no weapon: the Ready pick resolves down to Stand — armed as the HEAD.
    assert!(
        player.animation(AnimationNodeIndex::new(1)).is_some(),
        "the head variation"
    );
    assert!(
        player.animation(AnimationNodeIndex::new(6)).is_none(),
        "no roll while engaged"
    );
}

/// The GnollCaster case (decision 0125 — the director's ref falsification of the resolved-id
/// reading): a model with NO spell animations at all falls back to a flags-less Stand for
/// *playback*, but the sheath reconcile tests the **requested** id — ReadySpellDirected's own
/// force-stow row — so the staff still leaves the hand for the whole windup, exactly like the
/// ref's Redridge Mystic.
#[test]
fn cast_hold_stows_even_when_the_model_lacks_the_spell_anims() {
    let mut app = app();
    // A gnoll-shaped model: Stand and a Ready idle only — no 51/53 anywhere — with the real
    // gnoll's baked lookup shape (row 51 → Stand), so playback of the hold genuinely lands on
    // the flags-less Stand clip and only the requested id's own row can stow.
    let mut lookup = vec![
        benilla_formats::PlayableAnim {
            resolved_id: 0,
            dir_flags: 0,
        };
        64
    ];
    lookup[26].resolved_id = 26;
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![clip(0, 1, true), clip(26, 2, true)],
        hand_close: [None, None],
        playable_animation_lookup: lookup,
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            Engaged,
            Wielded {
                main: Some((2, 0xa)),
                off: None,
                ranged: None,
                main_sheath: 2,
                off_sheath: 0,
                ..Default::default()
            },
        ))
        .id();
    let sheath = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .sheath_state()
    };

    app.update();
    assert_eq!(sheath(&app), Some(1), "engaged Ready draws");

    // The precast hold requests 51; playback resolves to Stand (the model has nothing better),
    // but 51's own WeaponFlags `&4` still force the stow.
    app.world_mut().entity_mut(unit).insert(CastHold {
        ranged: false,
        anim_id: 51,
        spell_id: 20792,
    });
    app.update();
    assert_eq!(
        sheath(&app),
        Some(0),
        "the requested hold id stows regardless of the playback fallback"
    );

    app.world_mut().entity_mut(unit).remove::<CastHold>();
    app.update();
    assert_eq!(sheath(&app), Some(1), "re-drawn once the cast resolves");
}

/// A spell impact whose kit carries a CombatWound anim rides the wound **secondary slot**, never
/// the one-shot route — the client's own 8–10 branch inside the kit player (`0x60edf0` @
/// `0x60f3ad`, decision 0099 phase 4): the [`WoundAnim`] edge arms the decaying overlay and the
/// base track keeps playing untouched underneath (routing it as a one-shot would replace the
/// base — the exact mistake decision 0111 falsified for melee).
#[test]
fn spell_impact_wound_rides_the_secondary_slot() {
    let mut app = app();
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),  // Stand
            clip(9, 2, false), // CombatWound — Fireball's impact-kit anim
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
        ))
        .id();

    app.update(); // settle: Stand holds the gait slot
    fn drv(app: &App, unit: Entity) -> &AnimDriver {
        app.world().entity(unit).get::<AnimDriver>().unwrap()
    }
    assert!(drv(&app, unit).wound.is_none());
    let gait_before = drv(&app, unit).gait;

    app.world_mut().write_message(WoundAnim {
        entity: unit,
        anim_id: 9,
    });
    app.update();
    assert!(
        drv(&app, unit).wound.is_some(),
        "the impact kit's wound anim armed the secondary slot"
    );
    assert_eq!(
        drv(&app, unit).gait,
        gait_before,
        "the base track is untouched — a decaying overlay, not a replace"
    );
}

/// The whiff slow-down touches SWING anims only (decision 0279's scoping): a spell kit's
/// full-body special (Special1H 57) rides the same `Mode::Swing` slot, and a concurrent
/// auto-attack miss must not drag it to half speed — the director's "the Eviscerate spin
/// drags". A real swing keeps the verified 0.5 write.
#[test]
fn whiff_slowdown_spares_a_non_swing_oneshot() {
    let mut app = app();
    let model = || ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(57, 2, false), // Special1H — Eviscerate's kit anim
            clip(16, 3, false), // AttackUnarmed — the bare-hands swing
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let spinner = app
        .world_mut()
        .spawn((
            model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
        ))
        .id();
    let swinger = app
        .world_mut()
        .spawn((
            model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
        ))
        .id();
    app.update(); // settle: Stand holds both gait slots

    // The kit anim plays as a full-body one-shot; the swing as its own.
    app.world_mut().write_message(EmoteAnim {
        entity: spinner,
        anim_id: 57,
        seq: 1,
    });
    app.world_mut().write_message(SwingMessage {
        attacker: swinger,
        victim: None,
        hit_info: 0,
        victim_state: 2, // dodge — the whiff class
        damage: 0,
        seq: 2,
    });
    app.update();
    // Both whiff the same frame the one-shots are in flight.
    app.world_mut()
        .write_message(crate::creature_anim::SwingSlowdown(spinner));
    app.world_mut()
        .write_message(crate::creature_anim::SwingSlowdown(swinger));
    app.update();

    let speed = |app: &App, unit: Entity, node: u32| {
        app.world()
            .entity(unit)
            .get::<AnimationPlayer>()
            .unwrap()
            .animation(AnimationNodeIndex::new(node as usize))
            .expect("one-shot in flight")
            .speed()
    };
    assert_eq!(
        speed(&app, spinner, 2),
        1.0,
        "the special is not a swing — the whiff must not drag it"
    );
    assert_eq!(
        speed(&app, swinger, 3),
        0.5,
        "the real swing keeps the verified half-speed follow-through"
    );
}

/// A same-frame swing/kit-anim collision runs the client's COMBAT FAST-PATH (decision 0406,
/// wow-re `combat-anim-fastpath.md`): the requests replay in [`PlaySeq`] wire order, the FIRST
/// arms, and the second — combat over combat — does NOT overwrite it: the armed clip doubles
/// to 2× and the second parks in the deferred cache. Both wire orders keep the first arrival on
/// the body. The director's ref ground truth this pins: the Eviscerate spin survives the
/// auto-swings its cast triggers — sped up, never cut.
#[test]
fn same_frame_collision_fast_paths_the_second_combat_clip() {
    let mut app = app();
    let model = || ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(57, 2, false), // Special1H — Eviscerate's kit anim
            clip(16, 3, false), // the bare-hands swing
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let mut unit = || {
        app.world_mut()
            .spawn((
                model(),
                AnimationPlayer::default(),
                AnimationTransitions::new(),
                AnimDriver::default(),
            ))
            .id()
    };
    let spin_last = unit();
    let swing_last = unit();
    app.update(); // settle: Stand holds both gait slots

    // spin_last: the kit anim arrived after the swing on the wire — the spin must win.
    app.world_mut().write_message(SwingMessage {
        attacker: spin_last,
        victim: None,
        hit_info: 0x2,
        victim_state: 1,
        damage: 21,
        seq: 1,
    });
    app.world_mut().write_message(EmoteAnim {
        entity: spin_last,
        anim_id: 57,
        seq: 2,
    });
    // swing_last: the wire order reversed — the swing must win.
    app.world_mut().write_message(EmoteAnim {
        entity: swing_last,
        anim_id: 57,
        seq: 3,
    });
    app.world_mut().write_message(SwingMessage {
        attacker: swing_last,
        victim: None,
        hit_info: 0x2,
        victim_state: 1,
        damage: 21,
        seq: 4,
    });
    app.update();

    fn drv(app: &App, unit: Entity) -> &AnimDriver {
        app.world().entity(unit).get::<AnimDriver>().unwrap()
    }
    let speed = |app: &App, unit: Entity, node: u32| {
        app.world()
            .entity(unit)
            .get::<AnimationPlayer>()
            .unwrap()
            .animation(AnimationNodeIndex::new(node as usize))
            .expect("armed clip in flight")
            .speed()
    };
    // Swing first on the wire: the swing arms, the spin fast-paths — the swing doubles and the
    // spin parks (it plays when the swing ends; the ref's swing-first batch shows exactly this).
    assert_eq!(
        drv(&app, spin_last).mode,
        super::super::select::Mode::Swing {
            id: 16,
            under: None,
        },
        "the first arrival holds the body"
    );
    assert_eq!(drv(&app, spin_last).deferred, Some(57), "the spin parks");
    assert_eq!(speed(&app, spin_last, 3), 2.0, "the armed swing doubles");
    // Spin first on the wire (the trace's t=106 batch): the spin arms and SURVIVES the swing —
    // doubled, with the swing parked behind it. The old last-call-wins model ate the spin here.
    assert_eq!(
        drv(&app, swing_last).mode,
        super::super::select::Mode::Swing {
            id: 57,
            under: None,
        },
        "the spin holds the body through the later swing"
    );
    assert_eq!(drv(&app, swing_last).deferred, Some(16), "the swing parks");
    assert_eq!(speed(&app, swing_last, 2), 2.0, "the spin doubles");
}

/// The deferred-cache consumer (the client's `+0xd60` read at the base recompute): the moment no
/// one-shot is live, the parked combat clip plays — the swing the spin deferred fires once the
/// spin ends, at normal rate. Hand-sets the cache with the body idle (the state the instant the
/// spin finished) because the headless harness never advances clips to completion.
#[test]
fn deferred_combat_clip_plays_once_the_body_frees() {
    let mut app = app();
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(16, 3, false), // the bare-hands swing
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
        ))
        .id();
    app.update(); // settle: Stand holds the gait slot
    app.world_mut()
        .entity_mut(unit)
        .get_mut::<AnimDriver>()
        .unwrap()
        .deferred = Some(16);
    app.update();
    let drv = app.world().entity(unit).get::<AnimDriver>().unwrap();
    assert_eq!(
        drv.mode,
        super::super::select::Mode::Swing {
            id: 16,
            under: None,
        },
        "the parked swing armed"
    );
    assert_eq!(drv.deferred, None, "the cache is consumed");
    let speed = app
        .world()
        .entity(unit)
        .get::<AnimationPlayer>()
        .unwrap()
        .animation(AnimationNodeIndex::new(3))
        .expect("swing in flight")
        .speed();
    assert_eq!(speed, 1.0, "a consumed clip plays at normal rate");
}

/// The post-shot leg slide (director-observed vs ref): a one-shot that routed FULL-BODY while
/// standing must yield to the gait the instant the movement flags change — the client's
/// locomotion re-arm lands on the change and blindly overwrites bone 0 (the decision 0280
/// re-arm; `Mode::Land` re-picks on the same edge). Holding the clip out slides the runner
/// over the ground on straight legs. The edge is the trigger, not the level: with the flags
/// steady the clip plays out (third assertion, via the boneless masked fallback's moving entry).
#[test]
fn a_movement_flag_change_cuts_a_full_body_oneshot_immediately() {
    let mut app = app();
    let model = || ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(5, 2, true),   // Run
            clip(16, 3, false), // the bare-hands swing (1.0 s — far from finished)
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            MovementState::default(),
        ))
        .id();
    app.update(); // settle: Stand
    let mode = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().mode;

    // A standing swing routes full-body onto the base track.
    app.world_mut().write_message(SwingMessage {
        attacker: unit,
        victim: None,
        hit_info: 0x2,
        victim_state: 1,
        damage: 21,
        seq: 1,
    });
    app.update();
    assert_eq!(
        mode(&app),
        super::super::select::Mode::Swing {
            id: 16,
            under: None,
        },
        "standing swing holds the base track"
    );

    // The player starts running one frame later: the flag change must re-pick the gait NOW,
    // not when the 1.0 s clip finishes.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FORWARD,
        ..Default::default()
    });
    app.update();
    assert_eq!(
        mode(&app),
        super::super::select::Mode::Gait,
        "the movement-flag change cuts the swing to the gait immediately"
    );

    // Steady flags: a fresh standing swing plays out (still Swing on the very next frame).
    app.world_mut()
        .entity_mut(unit)
        .insert(MovementState::default());
    app.update(); // the return to standing re-picks the idle
    app.world_mut().write_message(SwingMessage {
        attacker: unit,
        victim: None,
        hit_info: 0x2,
        victim_state: 1,
        damage: 21,
        seq: 2,
    });
    app.update();
    app.update();
    assert!(
        matches!(mode(&app), super::super::select::Mode::Swing { id: 16, .. }),
        "steady flags let the clip play out"
    );
}

/// A stationary caster mouselook-turning: the chase-step TURN flag flickers at mouse-event
/// cadence (set on delta frames, clear on quiet ones — `drive_body_heading`'s fold), but the
/// client's cast pin tests `[9e8] & 0x20000f` — translation + swim, NEVER the turn bits (wow-re
/// `spell-visual-apply.md` §2.1, `move_flags::CAST_PIN_MOVE`) — so the full-body hold stays
/// pinned through the flap. Routing this through the one-shot mask (`0x20003f`) instead churned
/// the gait hold↔Shuffle on every mouse-delta frame — the frostbolt right-drag jitter
/// (decision 0491).
#[test]
fn turning_in_place_never_unpins_the_stationary_cast_hold() {
    let mut app = app();
    let mut model = caster_model();
    model.clips.push(clip(11, 7, true)); // ShuffleLeft — the churn destination the bug routed to
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            MovementState::default(),
            CastHold {
                ranged: false,
                anim_id: 51,
                spell_id: 116,
            },
        ))
        .id();
    let playing = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .playing()
    };

    app.update();
    assert_eq!(playing(&app), (Some(51), None), "stationary: the hold pins");

    // Flap the chase-step TURN flag across frames (mouse delta / quiet / delta …).
    for frame in 0..6u32 {
        let flags = if frame % 2 == 0 {
            move_flags::TURN_LEFT
        } else {
            0
        };
        app.world_mut().entity_mut(unit).insert(MovementState {
            flags,
            ..Default::default()
        });
        app.update();
        assert_eq!(
            playing(&app),
            (Some(51), None),
            "turn flap frame {frame}: pinned full-body, no overlay"
        );
    }

    // Real translation still demotes: the gait leaves the pin and the masked hold takes over.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FORWARD,
        speed: 7.0,
        ..Default::default()
    });
    app.update();
    let (base, overlay) = playing(&app);
    assert_ne!(base, Some(51), "a translating caster leaves the pin");
    assert_eq!(overlay, Some(51), "…and loops the hold masked on the torso");
}

/// The swim re-latch does NOT cut the hop's kick (decision 0517, director-corrected — amends
/// 0503's swim arm): JumpStart PLAYS OUT over the re-latch, the swim gait waiting at its end.
/// A GROUND cut (landing on a bank) still cuts immediately with 0503's pose-snapshot freeze.
#[test]
fn the_swim_relatch_holds_the_kick_but_a_ground_cut_freezes_it() {
    let mut app = app();
    let model = || ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(41, 2, true),  // SwimIdle
            clip(42, 3, true),  // Swim
            clip(37, 4, false), // JumpStart — the kick (833 ms real; the test never advances it)
            clip(38, 5, true),  // Jump hang
            clip(39, 6, false), // JumpEnd
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            MovementState::default(),
        ))
        .id();
    app.update(); // settle: Stand
    let drv = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().mode;

    // The dolphin hop launches: FALLING with an upward seed → the JumpStart bracket enters.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING | move_flags::FORWARD,
        vertical_speed: 9.0,
        speed: 4.7,
        ..Default::default()
    });
    app.update();
    assert_eq!(
        drv(&app),
        super::super::select::Mode::Entering(super::super::select::Special::Jump),
        "the upward launch enters the JumpStart bracket"
    );

    // Swim re-latches ~0.24 s later, mid-kick: the kick is HELD — no cut, no gait yet — and
    // keeps PLAYING (speed 1, not 0503's frozen ground-cut).
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::SWIMMING | move_flags::FORWARD,
        speed: 4.7,
        ..Default::default()
    });
    app.update();
    app.update();
    assert_eq!(
        drv(&app),
        super::super::select::Mode::Entering(super::super::select::Special::Jump),
        "the re-latch holds the kick (0517) — the swim gait waits for its end"
    );
    let player = app.world().entity(unit).get::<AnimationPlayer>().unwrap();
    let kick = player
        .animation(AnimationNodeIndex::new(4))
        .expect("the held JumpStart is still the armed clip");
    assert_eq!(
        kick.speed(),
        1.0,
        "held, not frozen — the kick keeps playing"
    );

    // A GROUND cut is unchanged: a fresh hop that lands on a bank (flags drop to grounded,
    // no SWIMMING) cuts immediately — Land pick + the 0503 snapshot-freeze on the kick.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: 0,
        ..Default::default()
    });
    app.update();
    assert_eq!(
        drv(&app),
        super::super::select::Mode::Land { id: 39, flags: 0 },
        "a stopped ground landing picks JumpEnd"
    );
    let player = app.world().entity(unit).get::<AnimationPlayer>().unwrap();
    let kick = player
        .animation(AnimationNodeIndex::new(4))
        .expect("the cut JumpStart still fades under the transition");
    assert_eq!(
        kick.speed(),
        0.0,
        "the ground cut is FROZEN mid-pose (0503)"
    );
}

/// The loot kneel, REMOTE half (the `0x5fd8b0` chain's loot leg → Loot 50, decision 0515):
/// `UNIT_FLAG_LOOTING` (`UNIT_FIELD_FLAGS` = field 46, bit 0x400 — up exactly while the unit's
/// corpse-loot window is open) holds the authored-clamp kneel in a stationary unit's gait slot;
/// movement suppresses it (the chain's locomotion-first order); the flag dropping (the loot
/// release's round-trip) hands the slot back to Stand.
#[test]
fn unit_flag_looting_kneels_stationary_units_only() {
    use benilla_protocol::messages::ObjectFields;

    let mut app = app();
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![clip(0, 1, true), clip(50, 2, false), clip(5, 3, true)],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            crate::net::ObjectStore(ObjectFields::from_pairs(&[(46, 0x400)])),
        ))
        .id();
    let gait = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().gait;

    // Stationary with the flag up: the kneel takes the gait slot.
    app.update();
    assert_eq!(gait(&app), Some(50), "looting kneels");

    // Movement outranks the kneel.
    app.world_mut().entity_mut(unit).insert(MovementState {
        speed: 7.0,
        flags: move_flags::FORWARD,
        ..Default::default()
    });
    app.update();
    assert_eq!(gait(&app), Some(5), "a moving looter runs");

    // Stopped again with the flag down (the release landed): back to Stand.
    app.world_mut().entity_mut(unit).remove::<MovementState>();
    app.world_mut()
        .entity_mut(unit)
        .insert(crate::net::ObjectStore(ObjectFields::from_pairs(&[(
            46, 0,
        )])));
    app.update();
    assert_eq!(gait(&app), Some(0), "released — back to Stand");
}

/// The loot kneel, SELF half (decision 0515 — the byte predicate `0x6126b0` splits on
/// IsActivePlayer): the local player's kneel rides the client-local loot-target latch
/// ([`crate::ui_loot::LootLatch`], the `[player+0x1d28]` mirror) — NOT its descriptor flag — so
/// it starts the frame the `CMSG_LOOT` send arms the latch (client-predicted, before any server
/// response reaches the descriptor) and ends the frame the latch drops.
#[test]
fn the_self_kneel_rides_the_loot_latch_not_the_flag() {
    use benilla_protocol::messages::ObjectFields;

    let mut app = app();
    app.init_resource::<crate::ui_loot::LootLatch>();
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![clip(0, 1, true), clip(50, 2, false), clip(5, 3, true)],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    // A SELF unit whose descriptor carries UNIT_FLAG_LOOTING but whose latch is empty: no kneel —
    // the flag is the REMOTE trigger only.
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            crate::net::SelfPlayer,
            crate::net::ObjectStore(ObjectFields::from_pairs(&[(46, 0x400)])),
            MovementState::default(),
        ))
        .id();
    let gait = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().gait;

    app.update();
    assert_eq!(
        gait(&app),
        Some(0),
        "the self unit ignores its own descriptor flag"
    );

    // The CMSG_LOOT send arms the latch: the kneel is client-predicted the same frame cycle.
    app.world_mut()
        .resource_mut::<crate::ui_loot::LootLatch>()
        .0 = Some(0x42);
    app.update();
    assert_eq!(gait(&app), Some(50), "the armed latch kneels the self unit");

    // The release/refusal drops the latch: straight back to Stand, no wire round-trip needed.
    app.world_mut()
        .resource_mut::<crate::ui_loot::LootLatch>()
        .0 = None;
    app.update();
    assert_eq!(
        gait(&app),
        Some(0),
        "the dropped latch stands the self unit"
    );
}

/// **B114's second half, end to end**: the prowl pose off the descriptor, through the real driver.
/// The CREEP vis flag (`UNIT_FIELD_BYTES_1` byte 3 bit 1 — field 138, `0x0200_0000`) is the whole
/// gate, and it is read from the unit's own descriptor for the SELF unit too (unlike the loot kneel
/// above, which splits self/remote): there is no client-side prediction of stealth, so the crouch
/// arrives with the server's aura. Stand ⇄ StealthStand and Run ⇄ StealthWalk both flip on the bit
/// alone, with no other state changing.
#[test]
fn the_creep_vis_flag_prowls_the_body() {
    use benilla_protocol::messages::ObjectFields;

    const CREEP: u32 = 0x0200_0000;
    let mut app = app();
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(5, 2, true),   // Run
            clip(119, 3, true), // StealthWalk
            clip(120, 4, true), // StealthStand
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            crate::net::SelfPlayer,
            crate::net::ObjectStore(ObjectFields::from_pairs(&[(138, 0)])),
            MovementState::default(),
        ))
        .id();
    let gait = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().gait;
    let set_flag = |app: &mut App, v: u32| {
        app.world_mut()
            .entity_mut(unit)
            .insert(crate::net::ObjectStore(ObjectFields::from_pairs(&[(
                138, v,
            )])));
    };

    app.update();
    assert_eq!(gait(&app), Some(0), "unstealthed idle stands");

    // The stealth aura landed: the same standing unit drops into the crouch.
    set_flag(&mut app, CREEP);
    app.update();
    assert_eq!(gait(&app), Some(120), "the CREEP bit crouches the idle");

    // Moving while stealthed creeps — at a speed that would otherwise be a flat-out Run.
    app.world_mut().entity_mut(unit).insert(MovementState {
        speed: 7.0,
        flags: move_flags::FORWARD,
        ..Default::default()
    });
    app.update();
    assert_eq!(gait(&app), Some(119), "the prowl outranks the speed tail");

    // Stealth broke mid-run: straight back to the ordinary gait, nothing else touched.
    set_flag(&mut app, 0);
    app.update();
    assert_eq!(gait(&app), Some(5), "broken stealth runs again");
}

/// The looping-variation ADVANCE (decision 0516 — wow-re `loop-replay-fidget.md` §7/§7d, the
/// watchdog `0x719370`): a relaxed looping base arm installs a replay window (here `(1,1)` → one
/// pass exactly); each completed window re-arms the id through the weighted, MEMORYLESS variation
/// walk. Over a dozen windows both authored Stand variations must take the main slot — the
/// gryphon's flap/glide alternation and the multi-part /dance in miniature. (The pre-0516 driver
/// armed once and wrapped forever: one variation on screen, the other never.)
#[test]
fn a_looping_arm_advances_through_its_variations_at_window_end() {
    use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle};
    use bevy::animation::AnimationClip;

    let mut app = app();
    const DUR: f32 = 0.1;
    let clip_handles: Vec<_> = (0..2)
        .map(|_| {
            let mut c = AnimationClip::default();
            c.set_duration(DUR);
            app.world_mut()
                .resource_mut::<Assets<AnimationClip>>()
                .add(c)
        })
        .collect();
    let (graph, nodes) = AnimationGraph::from_clips(clip_handles);
    let graph_handle = app
        .world_mut()
        .resource_mut::<Assets<AnimationGraph>>()
        .add(graph);
    // Two Stand variations, equal weight, replay (1,1): every window is exactly one pass.
    let variation = |node| {
        let mut c = clip(0, 0, true);
        c.node = node;
        c.duration = DUR;
        c.blend_time = 0.0;
        c.frequency = 0x4000;
        c.replay = (1, 1);
        c
    };
    let anims = ModelAnimations {
        graph: graph_handle.clone(),
        clips: vec![variation(nodes[0]), variation(nodes[1])],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    let unit = app
        .world_mut()
        .spawn((
            anims,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimationGraphHandle(graph_handle),
            AnimDriver::default(),
        ))
        .id();

    let mut seen = std::collections::HashSet::new();
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        app.update();
        let tr = app
            .world()
            .entity(unit)
            .get::<AnimationTransitions>()
            .unwrap();
        if let Some(n) = tr.get_main_animation() {
            seen.insert(n);
        }
    }
    assert!(
        seen.contains(&nodes[0]) && seen.contains(&nodes[1]),
        "over ~15 one-pass windows the memoryless weighted walk must visit BOTH variations \
         (saw {seen:?}) — an arm-once-wrap-forever driver never leaves the first"
    );
}

/// A jumper's model for the decision 0864 suite: the airborne bracket (JumpStart/Jump hang/
/// JumpEnd/Fall), the gaits, a spell-kit cast anim (SpellCastOmni 54, with a masked variant),
/// and the combat pair (Special1H 57 / AttackUnarmed 16) for the mid-air fast-path tenant.
fn jumper_model() -> ModelAnimations {
    let mut cast = clip(54, 7, false); // SpellCastOmni — the kit release anim
    cast.upper_node = Some(AnimationNodeIndex::new(8));
    ModelAnimations {
        graph: Handle::default(),
        clips: vec![
            clip(0, 1, true),   // Stand
            clip(5, 2, true),   // Run
            clip(37, 3, false), // JumpStart
            clip(38, 4, true),  // Jump hang
            clip(39, 5, false), // JumpEnd
            clip(40, 6, true),  // Fall
            cast,
            clip(57, 9, false),  // Special1H — a spell kit's combat one-shot
            clip(16, 10, false), // AttackUnarmed — the bare-hands swing
        ],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    }
}

fn jumper(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            jumper_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            MovementState::default(),
        ))
        .id()
}

/// The ref's jump-in-place cast (decision 0864 — the report this pins): a cast id is CLASS_A
/// but NOT COMBAT, so the airborne route test doesn't mask it — with no move bits it routes
/// FULL-BODY and REPLACES the jump hang on bone 0 (the client's one-slot last-writer-wins; the
/// old machine dropped it, which is why the cast only showed on *walking* jumps). The clip then
/// rides the airborne-freeze: the continuing Jump level must NOT re-preempt it — the client
/// issues no plays mid-arc, so the clip plays out (and a finished one clamps and holds) until
/// an edge.
#[test]
fn a_jump_in_place_cast_replaces_the_hang_and_survives_the_arc() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update(); // settle: Stand
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: 7.9, // an upward launch: a jump arc, in place
        ..Default::default()
    });
    app.update();
    let mode = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().mode;
    assert_eq!(
        mode(&app),
        super::super::select::Mode::Entering(super::super::select::Special::Jump),
        "airborne: the jump bracket enters"
    );
    // The instant AoE releases mid-air: the kit anim arrives.
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    fn drv(app: &App, unit: Entity) -> &AnimDriver {
        app.world().entity(unit).get::<AnimDriver>().unwrap()
    }
    assert_eq!(
        drv(&app, unit).mode,
        super::super::select::Mode::Swing {
            id: 54,
            under: Some(super::super::select::Special::Jump),
        },
        "the cast replaces the hang full-body — never dropped, never masked"
    );
    assert!(
        drv(&app, unit).overlay.is_none(),
        "no move bits, non-combat id: not the overlay route"
    );
    // The arc continues: the Jump *level* must not preempt the clip back to the bracket.
    app.update();
    app.update();
    assert!(
        matches!(
            drv(&app, unit).mode,
            super::super::select::Mode::Swing { id: 54, .. }
        ),
        "the airborne-freeze holds the one-shot through the arc"
    );
}

/// Touchdown while a mid-air one-shot holds bone 0: the Special edge (`Some → None`) routes
/// through `leave_special` — the `0x602c60` land dispatcher's pick replaces the clip like any
/// plain play (a stationary landing picks JumpEnd 39).
#[test]
fn landing_mid_cast_plays_the_land_pick() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update();
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: 7.9,
        ..Default::default()
    });
    app.update();
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    app.world_mut()
        .entity_mut(unit)
        .insert(MovementState::default()); // touchdown, stationary
    app.update();
    let mode = app.world().entity(unit).get::<AnimDriver>().unwrap().mode;
    assert_eq!(
        mode,
        super::super::select::Mode::Land { id: 39, flags: 0 },
        "the land pick cuts the held cast at touchdown"
    );
}

/// The FALLINGFAR latch mid-one-shot is an edge (`Jump → Fall`): the client plays Fall(40)
/// ONCE, on the substep it latches (`0x61a820` — the 0864-era per-tick re-assert was
/// §5-refuted, decision 0868), replacing the clip. A fresh cast armed AFTER the latch then
/// holds bone 0 like any other airborne one-shot, until the landing pick cuts it.
#[test]
fn a_cast_over_the_fall_loop_holds_until_landing() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update();
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: 7.9,
        ..Default::default()
    });
    app.update();
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update(); // Swing { 54, under: Jump }
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING | move_flags::FALLING_FAR,
        vertical_speed: -5.0,
        ..Default::default()
    });
    app.update();
    let mode = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().mode;
    assert_eq!(
        mode(&app),
        super::super::select::Mode::Looping(super::super::select::Special::Fall),
        "the latch's Fall play replaces the held cast"
    );
    // A second cast while falling far arms (last-writer-wins) …
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 2,
    });
    app.update();
    assert!(
        matches!(
            mode(&app),
            super::super::select::Mode::Swing {
                id: 54,
                under: Some(super::super::select::Special::Fall),
                ..
            }
        ),
        "the cast arms over the Fall loop"
    );
    // … and HOLDS through the rest of the fall: no per-tick re-assert exists (0868), and the
    // Fall level is not an edge.
    app.update();
    app.update();
    assert!(
        matches!(
            mode(&app),
            super::super::select::Mode::Swing {
                id: 54,
                under: Some(super::super::select::Special::Fall),
                ..
            }
        ),
        "the cast holds bone 0 through the fall — Fall plays only at its latch edge"
    );
    // Touchdown cuts it with the land pick, as every airborne one-shot.
    app.world_mut()
        .entity_mut(unit)
        .insert(MovementState::default());
    app.update();
    assert_eq!(
        mode(&app),
        super::super::select::Mode::Land { id: 39, flags: 0 },
        "the landing pick replaces the held cast"
    );
}

/// The walking jump keeps the masked route (the already-working half, now pinned): the
/// takeoff-frozen FORWARD bit routes the cast to the SpineLow overlay — the torso casts, the
/// legs keep the arc, and the base machine is untouched.
#[test]
fn a_moving_jump_cast_masks_onto_the_overlay() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update();
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FORWARD | move_flags::FALLING,
        vertical_speed: 7.9,
        speed: 7.0,
        ..Default::default()
    });
    app.update();
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    let drv = app.world().entity(unit).get::<AnimDriver>().unwrap();
    assert!(
        drv.overlay.is_some_and(|ov| ov.id == 54),
        "frozen-in move bits: the cast masks onto the overlay"
    );
    assert_eq!(
        drv.mode,
        super::super::select::Mode::Entering(super::super::select::Special::Jump),
        "the base machine keeps the bracket untouched"
    );
}

/// **The transplant** (decision 0878 — the director's "jump right after a cast should only play
/// the lower body animation and finish the upper body one"). A standing cast routes full-body to
/// bone 0; the jump that follows is a LOCOMOTION request, so the client does **not** overwrite it
/// — `0x5fe919` copies the bone-0 descriptor (id, rate, and its **live play position**) onto the
/// key-bone and hands bone 0 the jump. The legs jump; the arms finish the cast.
#[test]
fn a_jump_over_a_live_cast_transplants_it_to_the_torso() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update(); // settle: Stand
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    {
        let drv = app.world().entity(unit).get::<AnimDriver>().unwrap();
        assert!(
            matches!(drv.mode, super::super::select::Mode::Swing { id: 54, .. }),
            "standing: the cast takes the FULL BODY on bone 0"
        );
        assert!(drv.overlay.is_none(), "nothing on the key-bone yet");
    }
    // Jump in place, mid-cast.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: 7.9,
        ..Default::default()
    });
    app.update();
    let drv = app.world().entity(unit).get::<AnimDriver>().unwrap();
    assert_eq!(
        drv.overlay.map(|ov| (ov.id, ov.node)),
        Some((54, AnimationNodeIndex::new(8))),
        "the cast transplants onto the SpineLow overlay instead of being replaced"
    );
    assert_eq!(
        drv.mode,
        super::super::select::Mode::Entering(super::super::select::Special::Jump),
        "…and the legs get the jump"
    );
    assert!(
        drv.overlay_fade.is_none(),
        "a transplant carries blendFlag = 0: it resumes mid-clip, it does not cross-fade"
    );
}

/// A **Special is a bone-0 play, so it cannot cut the torso** (decision 0878 — the jump-running
/// half of the director's report). A moving caster's hold rides the key-bone; taking off routes
/// JumpStart to bone 0 (`0x5fe912`: with the key-bone armed the locomotion request goes straight
/// there) and leaves the hold running. The old `special.is_none()` filter dropped it on takeoff.
#[test]
fn a_jump_does_not_cut_the_moving_cast_hold() {
    let mut app = app();
    let unit = app
        .world_mut()
        .spawn((
            caster_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            MovementState {
                flags: move_flags::FORWARD,
                speed: 7.0,
                ..Default::default()
            },
            CastHold {
                anim_id: 51,
                spell_id: 1,
                ranged: false,
            },
        ))
        .id();
    app.update();
    assert!(
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .overlay
            .is_some_and(|ov| ov.id == 51 && ov.looping),
        "a moving caster holds on the torso"
    );
    // Take off, still running, still casting.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FORWARD | move_flags::FALLING,
        vertical_speed: 7.9,
        speed: 7.0,
        ..Default::default()
    });
    app.update();
    assert!(
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .overlay
            .is_some_and(|ov| ov.id == 51 && ov.looping),
        "the jump takes bone 0 — the hold keeps the torso"
    );
}

/// **The fade-to-rest** (decision 0878 — "the end of the cast animation is cut off and it
/// instantly snaps back"). A finished key-bone one-shot is never stopped: the client's completion
/// event disarms the bone through op4 `param_3 = -1`, which holds the clip's final frame in the
/// secondary slot and cross-fades it back onto the base over a fixed 150 ms. Real clip assets, so
/// Bevy actually completes the overlay.
#[test]
fn a_finished_masked_cast_fades_out_instead_of_snapping() {
    use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle};
    use bevy::animation::AnimationClip;

    let mut app = app();
    const CAST: f32 = 0.3;
    let run_handle = {
        let mut c = AnimationClip::default();
        c.set_duration(1.0);
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(c)
    };
    let cast_handle = {
        let mut c = AnimationClip::default();
        c.set_duration(CAST);
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(c)
    };
    // Three nodes: the run base, the cast's full-body node, and the cast's masked twin.
    let (graph, nodes) = AnimationGraph::from_clips([run_handle, cast_handle.clone(), cast_handle]);
    let graph_handle = app
        .world_mut()
        .resource_mut::<Assets<AnimationGraph>>()
        .add(graph);
    let mut run = clip(5, 0, true);
    run.node = nodes[0];
    let mut cast = clip(54, 0, false);
    cast.node = nodes[1];
    cast.duration = CAST;
    cast.blend_time = 0.05;
    cast.upper_node = Some(nodes[2]);
    let unit = app
        .world_mut()
        .spawn((
            ModelAnimations {
                graph: graph_handle.clone(),
                clips: vec![run, cast],
                hand_close: [None, None],
                playable_animation_lookup: Vec::new(),
                animation_lookup: Vec::new(),
                global_bones: Vec::new(),
                first_seq: None,
                pose: Default::default(),
            },
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimationGraphHandle(graph_handle),
            AnimDriver::default(),
            MovementState {
                flags: move_flags::FORWARD,
                speed: 7.0,
                ..Default::default()
            },
        ))
        .id();
    app.update(); // settle: Run
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    {
        let e = app.world().entity(unit);
        let drv = e.get::<AnimDriver>().unwrap();
        assert!(
            drv.overlay.is_some_and(|ov| ov.node == nodes[2]),
            "moving: the cast masks onto the torso"
        );
        assert!(
            drv.overlay_fade.is_some_and(|f| f.out.is_none()),
            "the arm is blended: it rises over its own blendTime, from the base pose"
        );
        let w = e.get::<AnimationPlayer>().unwrap().animation(nodes[2]);
        assert!(
            w.is_some_and(|a| a.weight() < super::ONESHOT_OVERLAY_WEIGHT),
            "…so it starts below full weight, not snapped on"
        );
    }
    // Step in small frames until the clip completes (a big frame would run the whole 150 ms fade
    // out in one go — right behaviour, useless assertion).
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.update();
        if app
            .world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .overlay
            .is_none()
        {
            break;
        }
    }
    {
        let e = app.world().entity(unit);
        let drv = e.get::<AnimDriver>().unwrap();
        assert!(drv.overlay.is_none(), "the cast finished");
        assert_eq!(
            drv.overlay_fade.and_then(|f| f.out),
            Some(nodes[2]),
            "…and retired into the fade slot rather than being dropped"
        );
        let active = e
            .get::<AnimationPlayer>()
            .unwrap()
            .animation(nodes[2])
            .expect("the finished clip is still driving the torso, holding its last frame");
        assert_eq!(
            active.speed(),
            0.0,
            "held on the final frame, not replaying"
        );
        assert!(active.weight() > 0.0, "still blended in as λ decays");
    }
    // Past the 150 ms window: the slot self-releases, exactly like the kernel's `+0xd0 = -1`.
    std::thread::sleep(std::time::Duration::from_millis(200));
    app.update();
    let e = app.world().entity(unit);
    assert!(
        e.get::<AnimDriver>().unwrap().overlay_fade.is_none(),
        "the fade window expired"
    );
    assert!(
        e.get::<AnimationPlayer>()
            .unwrap()
            .animation(nodes[2])
            .is_none(),
        "…and the node is released"
    );
}

/// The airborne-freeze in the GAIT slot (the step-off arc, decision 0864): live pins — here
/// the stationary cast hold — cannot swap the clip mid-air; the takeoff gait keeps rolling and
/// the pin applies at touchdown.
#[test]
fn the_step_off_arc_freezes_the_gait_against_live_pins() {
    let mut app = app();
    let unit = app
        .world_mut()
        .spawn((
            caster_model(),
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            MovementState::default(),
        ))
        .id();
    app.update(); // settle: Stand
    let gait = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().gait;
    assert_eq!(gait(&app), Some(0));
    // Step off a ledge (downward launch: no jump arc, no Special; vz ≠ 0 — the §5-verified
    // freeze gate `FALLING && (FALLINGFAR || vz ≠ 0)`, decision 0868) …
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: -3.0,
        ..Default::default()
    });
    // … and the precast lands mid-fall: the stationary pin must NOT re-pick mid-air.
    app.world_mut().entity_mut(unit).insert(CastHold {
        ranged: false,
        anim_id: 51,
        spell_id: 20793,
    });
    app.update();
    app.update();
    assert_eq!(
        gait(&app),
        Some(0),
        "the selector never re-picks mid-air — the takeoff gait holds"
    );
    // Touchdown: the freeze lifts and the pin applies immediately.
    app.world_mut()
        .entity_mut(unit)
        .insert(MovementState::default());
    app.update();
    assert_eq!(gait(&app), Some(51), "the pin lands with the unit");
}

/// A mid-air fast-path park survives the arc's LEVEL (decision 0864's edge-clear): the old
/// per-frame kill cleared the deferred cache on every airborne frame; the client clears only
/// at plays (state EDGES) — and the landing's pick, a play, still kills it.
#[test]
fn a_midair_deferred_park_survives_the_level_and_dies_at_the_landing_play() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update();
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: 7.9,
        ..Default::default()
    });
    app.update(); // Entering(Jump)
                  // A kit combat one-shot replaces the bracket (57 is forced-full-body), then a swing lands
                  // the same wire batch: combat-over-combat fast-paths — the swing parks.
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 57,
        seq: 1,
    });
    app.world_mut().write_message(SwingMessage {
        attacker: unit,
        victim: None,
        hit_info: 0x2,
        victim_state: 1,
        damage: 21,
        seq: 2,
    });
    app.update();
    fn drv(app: &App, unit: Entity) -> &AnimDriver {
        app.world().entity(unit).get::<AnimDriver>().unwrap()
    }
    assert_eq!(
        drv(&app, unit).deferred,
        Some(16),
        "the swing parks behind the kit clip"
    );
    // The arc's level must not kill the park (the old per-frame clear did).
    app.update();
    app.update();
    assert_eq!(
        drv(&app, unit).deferred,
        Some(16),
        "no play mid-arc — the park survives"
    );
    // Touchdown: the land pick is a play — the park dies with it (`0x5fe48e`).
    app.world_mut()
        .entity_mut(unit)
        .insert(MovementState::default());
    app.update();
    assert_eq!(
        drv(&app, unit).deferred,
        None,
        "the landing play clears the cache"
    );
}

/// The deferred cache's consuming read sits DOWNSTREAM of the airborne-freeze (`0x5fd392`
/// inside the `0x5fd360` recompute arm; §5-verified, decision 0868): a park made mid-arc is
/// never consumed mid-air, even with the body free — it waits, and the landing edge's play
/// clears it.
#[test]
fn a_midair_park_is_not_consumed_before_landing() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update();
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FALLING,
        vertical_speed: 7.9,
        ..Default::default()
    });
    app.update(); // Entering(Jump), body otherwise free
    app.world_mut()
        .entity_mut(unit)
        .get_mut::<AnimDriver>()
        .unwrap()
        .deferred = Some(16);
    app.update();
    app.update();
    fn drv(app: &App, unit: Entity) -> &AnimDriver {
        app.world().entity(unit).get::<AnimDriver>().unwrap()
    }
    assert_eq!(
        drv(&app, unit).deferred,
        Some(16),
        "the freeze blocks the consuming read — the park waits mid-air"
    );
    assert!(
        drv(&app, unit).overlay.is_none(),
        "the parked swing never played mid-air"
    );
    // Touchdown: the landing play clears the cache (`0x5fe48e`) — the park dies unplayed.
    app.world_mut()
        .entity_mut(unit)
        .insert(MovementState::default());
    app.update();
    assert_eq!(
        drv(&app, unit).deferred,
        None,
        "the landing play clears the cache"
    );
}

/// **The ranged→melee handoff at every landed swing** (`0x625829`, wow-re `sheath-policy.md` §1's
/// `0x6255b0` row) — the director's report: a bow drawn by a shot that never fired, then a melee
/// attack, and the swings keep coming out of the bow. The reconcile provably cannot fix it, and
/// this pins both halves: the CONTROL (a sword swing while ranged-drawn leaves the stance at 2 —
/// the client's melee force is gated `CUR != 2`, `0x5fe0f9`/`0x5fe13b`, so the ranged stance is
/// stable under any number of swings), and the FIX (the packet arm's own snap moves it, and the
/// reconcile then holds it there).
#[test]
fn a_landed_swing_snaps_its_attacker_out_of_the_ranged_stance() {
    let mut app = app();
    let model = ModelAnimations {
        graph: Handle::default(),
        clips: vec![clip(0, 1, true), clip(17, 2, false)], // Stand + Attack1H
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    // A warrior wearing a 1H sword and a bow: the exact loadout of the report.
    let unit = app
        .world_mut()
        .spawn((
            model,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimDriver::default(),
            crate::net::SelfPlayer,
            Wielded {
                main: Some((2, 0x7)), // 1H sword -> Attack1H (17)
                off: None,
                ranged: Some((2, 0x2)), // bow
                main_sheath: 3,
                off_sheath: 0,
                ..Default::default()
            },
        ))
        .id();
    let sheath = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .sheath_state()
    };
    let swing = |app: &mut App| {
        app.world_mut().write_message(SwingMessage {
            attacker: unit,
            victim: None,
            hit_info: 0,
            victim_state: 1,
            damage: 7,
            seq: 0,
        });
        app.update();
    };

    // The shot's draw (`SetSheatheState(2, SNAP)` — the cast-send arm).
    app.world_mut().write_message(SheathRequest {
        entity: unit,
        state: 2,
        ceremony: false,
    });
    app.update();
    assert_eq!(sheath(&app), Some(2), "the shot draws the bow");

    // CONTROL: swings alone. Attack1H's WeaponFlags `&0x20` is a force-DRAW-MELEE the client only
    // consults on the `CUR != 2` path, so nothing in the reconcile can leave the ranged stance.
    swing(&mut app);
    swing(&mut app);
    assert_eq!(
        sheath(&app),
        Some(2),
        "the reconcile alone never leaves the ranged stance — this is the bug's shape"
    );

    // The packet arm's snap (what `net::apply::combat::attacker_state` now writes beside the
    // swing): the sword comes out on the first landed blow.
    app.world_mut().write_message(SheathRequest {
        entity: unit,
        state: 1,
        ceremony: false,
    });
    swing(&mut app);
    assert_eq!(sheath(&app), Some(1), "the landed swing draws melee");

    // …and holds: every later swing re-requests 1, which the setter refuses as idempotent, and
    // Attack1H's `&0x20` re-asserts melee on the `CUR != 2` path.
    swing(&mut app);
    assert_eq!(sheath(&app), Some(1), "melee holds across the volley");
}

/// **The ceremony's two movements, end to end** (decision 0872) — the director's report: pressing
/// Z with both hands full puts the weapons away, lets the arms come back to neutral, and only
/// *then* reaches over the shoulder for the bow. Phase 1 is the setter's own play (`0x611b60`);
/// phase 2 is the on-anim-finish drawer (`0x5fc920` @ `0x5fca8c`/`0x5fcaa1`), which is the half
/// benilla never had — before it, the ceremony ended when the stow clips did and the bow simply
/// appeared. Real clip assets, so Bevy's `advance_animations` actually completes them.
#[test]
fn a_melee_to_ranged_toggle_stows_both_hands_before_it_reaches_for_the_bow() {
    use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle};
    use bevy::animation::AnimationClip;

    let mut app = app();
    const DUR: f32 = 0.1;
    let handles: Vec<_> = (0..5)
        .map(|_| {
            let mut c = AnimationClip::default();
            c.set_duration(DUR);
            app.world_mut()
                .resource_mut::<Assets<AnimationClip>>()
                .add(c)
        })
        .collect();
    let (graph, nodes) = AnimationGraph::from_clips(handles);
    let graph_handle = app
        .world_mut()
        .resource_mut::<Assets<AnimationGraph>>()
        .add(graph);
    // The two stow/draw families, each with its per-arm masked pair. No `$SHL`/`$SHR` events, so
    // each arm's weapon moves at the authored-event fallback: halfway.
    let family = |id: u16, right: usize, left: usize| {
        let mut c = clip(id, 0, false);
        c.node = nodes[right];
        c.duration = DUR;
        c.blend_time = 0.0;
        c.arm_nodes = Some((nodes[right], nodes[left]));
        c
    };
    let mut stand = clip(0, 0, true);
    stand.node = nodes[0];
    stand.duration = DUR;
    let anims = ModelAnimations {
        graph: graph_handle.clone(),
        clips: vec![stand, family(90, 1, 2), family(89, 3, 4)],
        hand_close: [None, None],
        playable_animation_lookup: Vec::new(),
        animation_lookup: Vec::new(),
        global_bones: Vec::new(),
        first_seq: None,
        pose: Default::default(),
    };
    // Sword-and-board plus a bow — the director's warrior. Hip sword (3 ⇒ HipSheath 90), back
    // shield (4 ⇒ Sheath 89), back bow (1 ⇒ 89, and INVTYPE_RANGED ⇒ the LEFT arm).
    let unit = app
        .world_mut()
        .spawn((
            anims,
            AnimationPlayer::default(),
            AnimationTransitions::new(),
            AnimationGraphHandle(graph_handle),
            AnimDriver::default(),
            // The Z toggle is the local player's alone; a remote unit's committed state is pulled
            // back to the server byte by the reconcile's rule 5 before any ceremony could run.
            crate::net::SelfPlayer,
            Wielded {
                main: Some((2, 0x7)),
                off: Some((4, 6)),
                ranged: Some((2, 0x2)),
                main_sheath: 3,
                off_sheath: 4,
                ranged_sheath: 1,
                ranged_inv: 0x0f,
                materials: [1, 6, 2],
            },
        ))
        .id();
    let visual = |app: &App| {
        app.world()
            .entity(unit)
            .get::<crate::creature_anim::VisualSheath>()
            .map(|v| v.0)
    };

    // Get to melee-drawn without a ceremony (a snap, as every reactive trigger does), then press Z.
    for (state, ceremony) in [(1u8, false), (2, true)] {
        app.world_mut().write_message(SheathRequest {
            entity: unit,
            state,
            ceremony,
        });
        app.update();
    }
    assert_eq!(
        visual(&app),
        Some([1, 1]),
        "phase 1: both hands still hold their weapons while the stow clips play"
    );

    let mut seen = vec![[1u8, 1]];
    let mut settled = false;
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.update();
        match visual(&app) {
            Some(v) if seen.last() != Some(&v) => seen.push(v),
            None => {
                settled = true;
                break;
            }
            _ => {}
        }
    }

    // The smoking gun: a state where the RIGHT arm has settled into the ranged stance (sword on
    // the back, nothing left to do) while the LEFT is still empty-handed — the bow on its way but
    // not yet arrived. That can only exist if a second clip started after the stows finished.
    assert!(
        seen.contains(&[2, 0]),
        "phase 2 never ran: the bow must be drawn by a SECOND clip, after both stows finished \
         (saw {seen:?})"
    );
    let stow = seen.iter().position(|v| *v == [1, 1]).unwrap();
    let reach = seen.iter().position(|v| *v == [2, 0]).unwrap();
    assert!(
        reach > stow,
        "the reach must follow the stow, not blend with it (saw {seen:?})"
    );
    assert!(
        settled,
        "the ceremony must end with the pin dropped, leaving the committed state (saw {seen:?})"
    );
}

/// **Ice Block, at the mechanism (decision 0894).** The stun's root wipes the direction bits in the
/// SAME frame the cast one-shot arrives, so the one-shot's own arm-time flags already read ROOT and
/// an arm-time comparison sees no movement edge — ever. The base's flags still hold the run it was
/// armed for, so the edge is there, the re-arm resolves to **Stand(0)** (not locomotion → no
/// transplant), and Stand overwrites the cast on bone 0: the character is fully neutral, with
/// nothing on the torso, by the time the freeze catches it. Before this the cast held bone 0 for the
/// whole block, or rode up to the torso and froze an arm out.
#[test]
fn a_root_landing_with_the_cast_returns_the_body_to_neutral() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FORWARD,
        speed: 7.0,
        ..Default::default()
    });
    app.update();
    let gait = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().gait;
    let mode = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().mode;
    let overlay = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .overlay
            .map(|o| o.id)
    };
    assert_eq!(gait(&app), Some(5), "running");

    // One frame: the root lands (direction bits wiped) AND the cast one-shot arrives.
    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::ROOT,
        ..Default::default()
    });
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    // The cast takes bone 0 and the base re-arm displaces it inside the same frame — the reference's
    // one-slot last-writer-wins, which is exactly what the base's flags (still FORWARD) unlock.
    assert_eq!(
        mode(&app),
        super::super::select::Mode::Gait,
        "the base request wins bone 0 back"
    );
    assert!(
        overlay(&app).is_none(),
        "no torso transplant: Stand(0) is not a locomotion request"
    );
    app.update();
    assert_eq!(gait(&app), Some(0), "fully neutral standing");
    assert!(overlay(&app).is_none(), "and nothing left on the torso");
}

/// The control that keeps 0878 honest: the transplant still fires when the re-arm really *is*
/// locomotion. A cast fired standing, then the player runs — the legs take Run and the cast keeps
/// going on the torso, which is the whole of R-B.
#[test]
fn a_run_starting_under_a_cast_still_transplants_it_up() {
    let mut app = app();
    let unit = jumper(&mut app);
    app.update(); // settle: Stand
    app.world_mut().write_message(EmoteAnim {
        entity: unit,
        anim_id: 54,
        seq: 1,
    });
    app.update();
    let mode = |app: &App| app.world().entity(unit).get::<AnimDriver>().unwrap().mode;
    let overlay = |app: &App| {
        app.world()
            .entity(unit)
            .get::<AnimDriver>()
            .unwrap()
            .overlay
            .map(|o| o.id)
    };
    assert!(matches!(
        mode(&app),
        super::super::select::Mode::Swing { id: 54, .. }
    ));

    app.world_mut().entity_mut(unit).insert(MovementState {
        flags: move_flags::FORWARD,
        speed: 7.0,
        ..Default::default()
    });
    app.update();
    assert_eq!(
        overlay(&app),
        Some(54),
        "a locomotion re-arm moves the cast to the torso instead of cutting it"
    );
}
