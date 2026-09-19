//! **A run boundary lands exactly what one run lands** (decision 2306). The drain dispatches in
//! wire order across the migration's seam, so a claimed packet arriving between two of the
//! match's own ends the match's run — a command flush — mid-frame. The match keeps three
//! intra-drain accumulators (`pending`, `StagedModes`, `SpeedStage`) precisely because its spawns
//! are deferred; each is *staged, else live*, so what a boundary flushes the next run reads off
//! the component. This drives the real drain on the built client both ways and compares.

use benilla_protocol::field::{FIELD_UNIT_HEALTH, FIELD_UNIT_LEVEL, FIELD_UNIT_MAXHEALTH};
use benilla_protocol::messages::{ObjectType, SpeedKind, SplineMode};
use benilla_protocol::{EntityKind, MoveSpeeds, ObjectFields, SessionEvent};
use bevy::prelude::*;

use crate::net::{GuidIndex, NetEvents, ObjectStore, UnitMoveModes, UnitSpeeds};

const GUID: u64 = 0xF130_0000_1234_0001;

fn create() -> SessionEvent {
    SessionEvent::ObjectCreate {
        guid: GUID,
        kind: EntityKind::Unit,
        display_id: None,
        position: [1.0, 2.0, 3.0],
        orientation: 0.0,
        scale: 1.0,
        speeds: Some(MoveSpeeds {
            walk: 2.5,
            run: 7.0,
            run_back: 4.5,
            swim: 4.7,
            swim_back: 2.5,
            turn_rate: std::f32::consts::PI,
        }),
        mover: None,
        transport_progress: None,
        transport: None,
        spline: None,
        fields: ObjectFields::from_pairs(&[
            (FIELD_UNIT_HEALTH, 100),
            (FIELD_UNIT_MAXHEALTH, 100),
            (FIELD_UNIT_LEVEL, 9),
        ])
        .into_created(ObjectType::Unit),
    }
}

/// What the object layer does to a unit born this frame: a values delta, a speed change, a
/// granted mode, a second delta — one of each accumulator, and `pending` twice.
fn updates() -> Vec<SessionEvent> {
    vec![
        SessionEvent::ObjectValues {
            guid: GUID,
            fields: ObjectFields::from_pairs(&[(FIELD_UNIT_HEALTH, 60)]),
        },
        SessionEvent::SpeedChanged {
            guid: GUID,
            kind: SpeedKind::Run,
            speed: 14.0,
        },
        SessionEvent::SplineMoveMode {
            guid: GUID,
            mode: SplineMode::Root,
            apply: true,
        },
        SessionEvent::ObjectValues {
            guid: GUID,
            fields: ObjectFields::from_pairs(&[(FIELD_UNIT_LEVEL, 10)]),
        },
    ]
}

/// A kind the mailbox has claimed — any table kind would do; this one needs no open window.
fn claimed() -> SessionEvent {
    SessionEvent::NextMailTime { seconds: -86400.0 }
}

/// One frame of the real drain over `events`; what the unit ended up as.
fn drained(events: Vec<SessionEvent>) -> (Vec<(u16, u32)>, MoveSpeeds, UnitMoveModes) {
    let mut app = crate::game_plugins::schedule_tests::headless_client();
    let (tx, rx) = crossbeam_channel::unbounded();
    app.insert_resource(NetEvents(rx));
    for ev in events {
        tx.send(ev).unwrap();
    }
    let world = app.world_mut();
    super::apply_net_updates(world);
    let e = *world
        .resource::<GuidIndex>()
        .0
        .get(&GUID)
        .expect("the create indexed the unit");
    let fields = world
        .get::<ObjectStore>(e)
        .expect("the store landed")
        .0
        .raw_fields()
        .collect();
    let speeds = world.get::<UnitSpeeds>(e).expect("the speeds landed").0;
    let modes = *world.get::<UnitMoveModes>(e).expect("the modes landed");
    (fields, speeds, modes)
}

#[test]
fn a_claimed_packet_between_a_create_and_its_updates_changes_nothing() {
    // One run: the create and everything after it in one batch of the match, the claimed packet
    // last — what the two-halves dispatch 2305 landed would have made of either frame.
    let mut one_run = vec![create()];
    one_run.extend(updates());
    one_run.push(claimed());
    // The seam at its worst: a claimed packet after the create and between every update, so
    // each accumulator is flushed and re-seeded from the live component.
    let mut split = vec![create()];
    for ev in updates() {
        split.push(claimed());
        split.push(ev);
    }

    let (fields, speeds, modes) = drained(one_run);
    let (split_fields, split_speeds, split_modes) = drained(split);

    // The frame's own meaning, so the comparison below cannot pass on two empty results.
    assert!(fields.contains(&(FIELD_UNIT_HEALTH, 60)), "{fields:?}");
    assert!(fields.contains(&(FIELD_UNIT_LEVEL, 10)), "{fields:?}");
    assert!(fields.contains(&(FIELD_UNIT_MAXHEALTH, 100)), "{fields:?}");
    assert_eq!(speeds.run, 14.0);
    assert_eq!(speeds.walk, 2.5);
    assert!(modes.rooted());

    assert_eq!(split_fields, fields);
    assert_eq!(split_speeds.run, speeds.run);
    assert_eq!(split_speeds.walk, speeds.walk);
    assert_eq!(split_modes, modes);
}
